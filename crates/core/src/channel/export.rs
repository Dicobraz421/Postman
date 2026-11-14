//! Экспорт беседы любого канала (ADR 0012 п. 8 «Экспорт, печать»): лента из общей модели (`ConversationSource` —
//! чаты и почта одинаково) → `postman_export::feed` (HTML для чтения и печати, JSON). Файл пишет вызывающий — туда,
//! куда выбрал пользователь (решение «всё в базе»: экспорт — исключение, файл берут с собой).
//!
//! Чтение базы — I/O: из фона, не из потока интерфейса.

use std::collections::HashMap;

use postman_export::feed::{Entry, Feed, File, Labels};

use super::{Address, ChannelError, ConversationSource, Line, ParticipantId};

type Result<T> = std::result::Result<T, ChannelError>;

/// Строк ленты за одно чтение.
const PAGE: usize = 500;

/// Формат файла.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Html,
    Json,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Format::Html => "html",
            Format::Json => "json",
        }
    }
}

/// Лента беседы `at` целиком, старые первыми: имена авторов — по людям беседы; `offset` — сдвиг местного времени от
/// UTC, секунды; `labels` — подписи HTML на языке интерфейса. Беседы нет — `None`.
pub fn feed(source: &dyn ConversationSource, at: &Address, offset: i32, labels: Labels) -> Result<Option<Feed>> {
    let at = at.of_conversation();
    let Some(summary) = source.conversation(&at)? else { return Ok(None) };
    let names: HashMap<ParticipantId, String> = source.people(&at)?.into_iter().map(|p| (p.id, p.name)).collect();
    let name = |id: &ParticipantId| names.get(id).cloned().filter(|n| !n.is_empty()).unwrap_or_else(|| id.0.clone());
    let mut lines: Vec<Line> = Vec::new();
    let mut before = None;
    loop {
        let page = source.timeline(&at, before.as_ref(), PAGE)?;
        let Some(first) = page.first() else { break };
        let next = first.id().clone();
        let full = page.len() >= PAGE;
        lines.splice(0..0, page);
        if !full || Some(&next) == before.as_ref() {
            break;
        }
        before = Some(next);
    }
    let entries = lines
        .iter()
        .map(|l| match l {
            Line::Deleted { id, author, sent } => Entry {
                id: id.0.clone(),
                author: name(author),
                author_id: author.0.clone(),
                at: sent.div_euclid(1000),
                deleted: true,
                ..Entry::default()
            },
            Line::Message { message: m, .. } => Entry {
                id: m.id.0.clone(),
                author: name(&m.author),
                author_id: m.author.0.clone(),
                at: m.sent.div_euclid(1000),
                edited: m.edited.map(|t| t.div_euclid(1000)),
                deleted: false,
                text: m.body.plain(),
                reply_to: m.reply_to.as_ref().map(|r| r.0.clone()),
                forwarded: m.forwarded.as_ref().map(|o| o.name.clone()),
                quotes: m.quotes.iter().map(|q| q.text.clone()).collect(),
                attachments: m.attachments.iter().map(|a| File { name: a.name.clone(), mime: a.mime.clone(), size: a.size }).collect(),
                reactions: m.reactions.iter().map(|r| (r.key.clone(), r.count)).collect(),
                mine: m.mine,
            },
        })
        .collect();
    Ok(Some(Feed { title: summary.conversation.title, channel: at.channel.0.clone(), address: at.to_string(), offset, entries, labels }))
}

/// Байты файла ленты.
pub fn render(feed: &Feed, format: Format) -> Vec<u8> {
    match format {
        Format::Html => postman_export::feed::to_html(feed),
        Format::Json => postman_export::feed::to_json(feed),
    }
    .into_bytes()
}

/// Имя файла: название беседы без недопустимых знаков и расширение формата.
pub fn file_name(feed: &Feed, format: Format) -> String {
    let base = postman_export::safe_name(&feed.title);
    let base = if base.trim().is_empty() { feed.channel.clone() } else { base };
    format!("{base}.{}", format.extension())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{
        AccountId, Body, ChannelEvent, ChannelId, ChannelStore, Chats, Conversation, ConversationId, Message, MessageId, Participant,
    };
    use postman_cache::Cache;

    /// Беседа чата целиком (больше страницы), имена авторов, удалённое — строкой; HTML и JSON; имя файла.
    #[test]
    fn chat_feed_exports() {
        let cache = Cache::open_in_memory().unwrap();
        let store = ChannelStore::open(cache.clone(), &ChannelId::new("demo"), &AccountId::new("me@example.test"), "Я").unwrap();
        let g = ConversationId::new("g");
        let msgs: Vec<Message> = (0..PAGE as i64 + 3)
            .map(|i| Message {
                id: MessageId::new(format!("{i}")),
                author: crate::channel::ParticipantId::new("u-anna"),
                sent: 1_000 * (i + 1),
                body: Body::Plain(format!("сообщение {i}")),
                ..Message::default()
            })
            .collect();
        store
            .apply(vec![
                ChannelEvent::People(vec![Participant {
                    id: crate::channel::ParticipantId::new("u-anna"),
                    name: "Анна".into(),
                    ..Participant::default()
                }]),
                ChannelEvent::Conversations(vec![Conversation {
                    id: g.clone(), title: "Команда: план/итоги".into(), ..Conversation::default()
                }]),
                ChannelEvent::Members {
                    conversation: g.clone(),
                    changed: vec![crate::channel::Member { id: crate::channel::ParticipantId::new("u-anna"), role: crate::channel::Role::Member }],
                    left: vec![],
                },
                ChannelEvent::Messages { conversation: g.clone(), messages: msgs, live: false },
                ChannelEvent::Deleted { conversation: Some(g.clone()), ids: vec![MessageId::new("1")], for_all: true },
            ])
            .unwrap();
        let at = Address::conversation(store.channel(), store.account(), &g);
        let f = feed(&Chats::new(cache), &at, 0, Labels::default()).unwrap().unwrap();
        assert_eq!(f.entries.len(), PAGE + 3);
        assert_eq!(f.entries[0].text, "сообщение 0");
        assert_eq!(f.entries[0].author, "Анна");
        assert!(f.entries[1].deleted);
        let html = String::from_utf8(render(&f, Format::Html)).unwrap();
        assert!(html.contains("сообщение 502"));
        let json = String::from_utf8(render(&f, Format::Json)).unwrap();
        assert!(json.contains("\"channel\":\"demo\""));
        assert!(!file_name(&f, Format::Json).contains('/'));
        assert!(file_name(&f, Format::Json).ends_with(".json"));
    }
}
