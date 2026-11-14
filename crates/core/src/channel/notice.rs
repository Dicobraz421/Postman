//! Уведомления каналов (ADR 0012 п. 11): по `Update::Arrived` (пришло сейчас, не своё) — если беседа не заглушена
//! (сервером или в базе); упоминание меня — «важное». Одна беседа — один тост за [`GROUP_MS`]: следующие сообщения
//! того же окна — счётом в нём. Тексты и показ — у интерфейса (тосты, трей, CH-04); журнал — без текста сообщений.

use std::collections::HashMap;

use postman_cache::Cache;

use super::{Address, ChanAccount, ChannelError, ChannelStore, Chats, ConversationId, ConversationSource, MessageId, MessageKind, source};

type Result<T> = std::result::Result<T, ChannelError>;

/// Уведомления одной беседы за столько миллисекунд — одним тостом.
pub const GROUP_MS: i64 = 5_000;

/// Что показать о новых сообщениях беседы.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// Адрес последнего сообщения (открыть по щелчку).
    pub address: Address,
    /// Название беседы.
    pub title: String,
    /// Автор последнего сообщения (имя, без него — id).
    pub author: String,
    /// Начало последнего сообщения.
    pub preview: String,
    /// Сколько новых сообщений.
    pub count: usize,
    /// Меня упомянули — «важное».
    pub mention: bool,
}

/// Новые чужие сообщения беседы → уведомление; заглушена, только свои или служебные — `None`. `now` — Unix, мс.
pub fn notice(cache: &Cache, chan: &ChanAccount, conversation: &ConversationId, messages: &[MessageId], now: i64) -> Result<Option<Notice>> {
    let Some(store) = ChannelStore::find(cache, &chan.channel, &chan.account)? else { return Ok(None) };
    let at = Address::conversation(&chan.channel, &chan.account, conversation);
    let Some(summary) = Chats::new(cache.clone()).conversation(&at)? else { return Ok(None) };
    if super::smart::is_muted(cache, &summary, now)? {
        return Ok(None);
    }
    let me = store.me()?.map(|p| p.id);
    let mut fresh = Vec::new();
    for id in messages {
        if let Some(m) = store.message(conversation, id)?.and_then(|e| e.message().cloned())
            && !m.mine
            && m.kind == MessageKind::Normal
        {
            fresh.push(m);
        }
    }
    let Some(last) = fresh.last() else { return Ok(None) };
    let mention = me.as_ref().is_some_and(|me| fresh.iter().any(|m| m.mentions.contains(me)));
    let author = store.participant(&last.author)?.map(|p| p.name).filter(|n| !n.is_empty()).unwrap_or_else(|| last.author.0.clone());
    Ok(Some(Notice {
        address: Address { message: Some(last.id.clone()), ..at },
        title: summary.conversation.title,
        author,
        preview: source::preview(&last.body.plain()),
        count: fresh.len(),
        mention,
    }))
}

/// Тосты бесед: одна беседа — один тост за [`GROUP_MS`].
#[derive(Debug, Default)]
pub struct Grouper {
    shown: HashMap<Address, i64>,
}

impl Grouper {
    /// Показать уведомление беседы сейчас (`now`, Unix, мс)? Нет — его сообщения уже в тосте этого окна. Упоминание
    /// меня показывается всегда.
    pub fn admit(&mut self, n: &Notice, now: i64) -> bool {
        let conv = n.address.of_conversation();
        self.shown.retain(|_, t| now - *t < GROUP_MS);
        if !n.mention && self.shown.contains_key(&conv) {
            return false;
        }
        self.shown.insert(conv, now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{Body, ChannelEvent, ChannelFeatures, Message, Participant, ParticipantId};

    /// Чужое — уведомление с автором и счётом, упоминание — «важное», своё — нет, заглушённое — молчит; одна беседа
    /// за 5 с — один тост.
    #[test]
    fn notices_respect_mute_and_group() {
        let cache = Cache::open_in_memory().unwrap();
        let chan = ChanAccount::new("demo", "me@example.test");
        let store = ChannelStore::open(cache.clone(), &chan.channel, &chan.account, "Я").unwrap();
        store.set_me(&Participant { id: ParticipantId::new("u-me"), ..Participant::default() }).unwrap();
        let m = |id: &str, author: &str, mention: bool| Message {
            id: MessageId::new(id),
            author: ParticipantId::new(author),
            sent: 1,
            body: Body::Plain(format!("текст {id}")),
            mentions: if mention { vec![ParticipantId::new("u-me")] } else { vec![] },
            mine: author == "u-me",
            ..Message::default()
        };
        let c = ConversationId::new("g");
        store
            .apply(vec![
                ChannelEvent::People(vec![Participant { id: ParticipantId::new("u-anna"), name: "Анна".into(), ..Participant::default() }]),
                ChannelEvent::Messages {
                    conversation: c.clone(),
                    messages: vec![m("1", "u-anna", false), m("2", "u-anna", true), m("3", "u-me", false)],
                    live: true,
                },
            ])
            .unwrap();
        let ids = |v: &[&str]| v.iter().map(|i| MessageId::new(*i)).collect::<Vec<_>>();
        let n = notice(&cache, &chan, &c, &ids(&["1", "2", "3"]), 0).unwrap().unwrap();
        assert_eq!((n.author.as_str(), n.count, n.mention, n.preview.as_str()), ("Анна", 2, true, "текст 2"));
        assert_eq!(notice(&cache, &chan, &c, &ids(&["3"]), 0).unwrap(), None, "своё — без уведомления");
        let plain = notice(&cache, &chan, &c, &ids(&["1"]), 0).unwrap().unwrap();
        let mut g = Grouper::default();
        assert!(g.admit(&plain, 0) && !g.admit(&plain, 4_000) && g.admit(&n, 4_500) && g.admit(&plain, 10_000));
        let at = Address::conversation(&chan.channel, &chan.account, &c);
        crate::channel::smart::set_mute(&cache, &at, &ChannelFeatures::default(), Some(i64::MAX)).unwrap();
        assert_eq!(notice(&cache, &chan, &c, &ids(&["1"]), 0).unwrap(), None, "заглушённое молчит");
    }
}
