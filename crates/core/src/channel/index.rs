//! Сообщения каналов в общем индексе поиска (ADR 0012 п. 10): документ `c<MsgKey>` рядом с письмами — тело, автор,
//! название беседы, имена вложений; поля фильтров — канал, адрес беседы, адреса автора (участник канала, его почта и
//! телефон, строчными). Пишет поток учётки после каждой пачки ([`applied`]); правка — документ заменяется, удаление —
//! удаляется, беседа ушла — уходят все её документы. Схема сменилась — индекс собирается заново из базы
//! (`crate::indexer::rebuild`, сообщения каналов — [`rebuild`]).
//!
//! Поиск ([`search`]): слова запроса — как у почты (словоформы, опечатки, раскладка, ё = е), условия `канал:`,
//! `беседа:`, `от:`; `от:` — все адреса человека ([`authors`]): участники, у которых совпало имя, ник, почта или
//! телефон, и адреса карточки контакта (почта, телефоны, адреса в мессенджерах).

use std::collections::HashMap;

use postman_cache::{Cache, ConvKey, MsgKey};
use postman_search::{Attachment, Doc, Scope, SearchIndex};

pub use postman_search::AttachmentHit;

use super::{Address, ChanAccount, ChannelError, ChannelStore, Entry, Message, MessageId, Participant, ParticipantId, StoredConversation};

type Result<T> = std::result::Result<T, ChannelError>;

/// Документы каналов в индексе: `c<MsgKey>` (письма — `crate::store::letter_ref`).
const PREFIX: &str = "c";
/// Сообщений за одно чтение и один коммит при пересборке.
const PAGE: usize = 500;

/// id документа сообщения.
pub fn doc_id(key: MsgKey) -> String {
    format!("{PREFIX}{}", key.0)
}

/// Сообщение документа [`doc_id`]; письмо и чужое — `None`.
pub fn parse_doc_id(id: &str) -> Option<MsgKey> {
    id.strip_prefix(PREFIX)?.parse().ok().map(MsgKey)
}

/// Адрес участника канала для поиска: `<канал>:<учётка>:<id>`, строчными.
pub fn author_key(chan: &ChanAccount, id: &ParticipantId) -> String {
    format!("{}:{}:{}", chan.channel, chan.account, id).to_lowercase()
}

/// Телефон для сравнения: только цифры (и `+` в начале).
pub fn phone_key(phone: &str) -> String {
    let digits: String = phone.chars().filter(char::is_ascii_digit).collect();
    if phone.trim_start().starts_with('+') { format!("+{digits}") } else { digits }
}

/// Все адреса участника: его ключ, почта, телефон.
pub fn participant_addresses(chan: &ChanAccount, p: &Participant) -> Vec<String> {
    let mut out = vec![author_key(chan, &p.id)];
    out.extend(p.emails.iter().map(|e| e.trim().to_lowercase()));
    out.extend(p.phone.as_deref().map(phone_key).filter(|p| !p.is_empty()));
    out
}

/// Адреса карточки контакта для [`authors`] и «Человек во всех каналах»: почта, телефоны, адреса в мессенджерах
/// (`ContactCard::services`). Сверка ссылки мессенджера с участником — у канала (CH-03: `t.me/<имя>` ↔ ник).
pub fn contact_addresses(card: &crate::model::ContactCard) -> Vec<String> {
    card.emails.iter().chain(&card.phones).chain(&card.mobile).chain(&card.services).map(|a| a.trim().to_owned()).filter(|a| !a.is_empty()).collect()
}

/// Беседы и люди, прочитанные за одну пачку документов.
struct Memo<'a> {
    store: &'a ChannelStore,
    convs: HashMap<ConvKey, Option<StoredConversation>>,
    people: HashMap<ParticipantId, Option<Participant>>,
}

impl<'a> Memo<'a> {
    fn new(store: &'a ChannelStore) -> Self {
        Self { store, convs: HashMap::new(), people: HashMap::new() }
    }

    fn conv(&mut self, key: ConvKey) -> Result<Option<&StoredConversation>> {
        if !self.convs.contains_key(&key) {
            let c = self.store.conversation_by_key(key)?;
            self.convs.insert(key, c);
        }
        Ok(self.convs.get(&key).and_then(Option::as_ref))
    }

    fn person(&mut self, id: &ParticipantId) -> Result<Option<&Participant>> {
        if !self.people.contains_key(id) {
            let p = self.store.participant(id)?;
            self.people.insert(id.clone(), p);
        }
        Ok(self.people.get(id).and_then(Option::as_ref))
    }
}

/// Что пишется в документ сообщения.
struct Fields {
    id: String,
    title: String,
    people: String,
    body: String,
    attachments: Vec<String>,
    received_at: i64,
    channel: String,
    conv: String,
    authors: Vec<String>,
}

fn fields(memo: &mut Memo, key: MsgKey, conv: ConvKey, m: &Message) -> Result<Option<Fields>> {
    let chan = memo.store.chan();
    let Some(c) = memo.conv(conv)? else { return Ok(None) };
    let (title, address) = (c.conversation.title.clone(), c.address.to_string());
    let mut authors = vec![author_key(&chan, &m.author)];
    let mut people = m.author.0.clone();
    if let Some(p) = memo.person(&m.author)? {
        people = format!("{} {}", p.name, p.handle.as_deref().unwrap_or_default());
        authors = participant_addresses(&chan, p);
    }
    let mut body = m.body.plain();
    for q in &m.quotes {
        body.push('\n');
        body.push_str(&q.text);
    }
    Ok(Some(Fields {
        id: doc_id(key),
        title,
        people,
        body,
        attachments: m.attachments.iter().map(|a| a.name.clone()).collect(),
        received_at: m.sent.div_euclid(1000),
        channel: chan.channel.0,
        conv: address,
        authors,
    }))
}

fn upsert(index: &SearchIndex, docs: &[Fields]) {
    let names: Vec<Vec<Attachment>> = docs.iter().map(|f| f.attachments.iter().map(|n| Attachment { name: n, text: "" }).collect()).collect();
    let docs: Vec<Doc> = docs
        .iter()
        .zip(&names)
        .map(|(f, attachments)| Doc {
            id: &f.id,
            subject: &f.title,
            people: &f.people,
            body: &f.body,
            attachments,
            received_at: f.received_at,
            channel: &f.channel,
            conv: &f.conv,
            authors: &f.authors,
        })
        .collect();
    if let Err(e) = index.upsert(&docs) {
        tracing::warn!("channel search: messages not indexed: {e}");
    }
}

/// Пачка записана ([`super::Applied`]): новые и изменённые сообщения — в индекс, удалённые и беседы, которых нет, —
/// из индекса. Сбой индекса почте и каналу не мешает: в журнал, поиск догонит пересборкой.
pub fn applied(index: &SearchIndex, store: &ChannelStore, a: &super::Applied) {
    if a.indexed.is_empty() && a.unindexed.is_empty() && a.gone.is_empty() {
        return;
    }
    let chan = store.chan();
    if !a.unindexed.is_empty() {
        let ids: Vec<String> = a.unindexed.iter().map(|k| doc_id(*k)).collect();
        let _ = index.delete(&ids);
    }
    for c in &a.gone {
        let _ = index.delete_conversation(&Address::conversation(&chan.channel, &chan.account, c).to_string());
    }
    let mut memo = Memo::new(store);
    let mut docs = Vec::with_capacity(a.indexed.len());
    for k in &a.indexed {
        match store.entry_by_key(*k) {
            Ok(Some((conv, Entry::Message { message, .. }))) => match fields(&mut memo, *k, conv, &message) {
                Ok(Some(f)) => docs.push(f),
                Ok(None) => {}
                Err(e) => tracing::warn!("channel search: message not indexed: {e}"),
            },
            Ok(_) => {}
            Err(e) => tracing::warn!("channel search: message not read: {e}"),
        }
    }
    upsert(index, &docs);
    if let Err(e) = index.commit() {
        tracing::warn!("channel search: index not committed: {e}");
    }
}

/// «Выйти»: документы всех бесед учётки — из индекса.
pub fn forget(index: &SearchIndex, store: &ChannelStore) {
    let chan = store.chan();
    match ChannelStore::conversations(store.cache(), 0, usize::MAX) {
        Ok(all) => {
            for c in all.iter().filter(|c| c.address.chan() == chan) {
                let _ = index.delete_conversation(&c.address.to_string());
            }
            let _ = index.commit();
        }
        Err(e) => tracing::warn!("channel search: sign-out left documents: {e}"),
    }
}

/// Сообщения каналов из базы — в индекс (после очистки индекса, `crate::indexer::rebuild`). `false` — прервано.
pub fn rebuild(index: &SearchIndex, cache: &Cache, stop: &dyn Fn() -> bool) -> Result<bool> {
    let stores = ChannelStore::all(cache)?;
    if stores.is_empty() {
        return Ok(true);
    }
    let mut memos: Vec<Memo> = stores.iter().map(Memo::new).collect();
    let mut from = MsgKey(0);
    loop {
        if stop() {
            return Ok(false);
        }
        let page = cache.chan_msgs_from(from, PAGE)?;
        let mut docs = Vec::with_capacity(page.len());
        for doc in &page {
            from = MsgKey(doc.key.0 + 1);
            let Some(conv) = cache.chan_conv(doc.conv)? else { continue };
            let Some(memo) = memos.iter_mut().find(|m| m.store.key() == conv.account) else { continue };
            let (key, conv) = (doc.key, doc.conv);
            if let Ok(Entry::Message { message, .. }) = super::store::entry(doc.clone())
                && let Some(f) = fields(memo, key, conv, &message)?
            {
                docs.push(f);
            }
        }
        upsert(index, &docs);
        let _ = index.commit();
        if page.len() < PAGE {
            return Ok(true);
        }
    }
}

/// Найденное сообщение канала.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    /// Адрес сообщения.
    pub address: Address,
    pub key: MsgKey,
    /// Совпало во вложении (имя).
    pub matched_in: Option<AttachmentHit>,
}

/// Адреса людей условия `от:` в каналах: участники всех учёток, у которых имя, ник, почта или телефон содержат
/// значение, — все их адреса; и `contact` — адреса карточки контакта этого значения (почта, телефоны, адреса в
/// мессенджерах), сверенные с участниками по почте и телефону. Один человек — по всем адресам.
pub fn authors(cache: &Cache, value: &str, contact: &[String]) -> Result<Vec<String>> {
    let needle = value.trim().to_lowercase();
    let mut want: Vec<String> = contact.iter().map(|a| a.trim().to_lowercase()).collect();
    want.extend(contact.iter().map(|a| phone_key(a)).filter(|p| p.len() > 4));
    if needle.contains('@') {
        want.push(needle.clone());
    }
    let mut out = want.clone();
    for store in ChannelStore::all(cache)? {
        let chan = store.chan();
        for p in store.participants()? {
            let addrs = participant_addresses(&chan, &p);
            let named = !needle.is_empty()
                && (p.name.to_lowercase().contains(&needle)
                    || p.handle.as_deref().is_some_and(|h| h.to_lowercase().contains(&needle))
                    || addrs.iter().any(|a| a.contains(&needle)));
            if named || addrs.iter().any(|a| want.contains(a)) {
                out.extend(addrs);
            }
        }
    }
    // Один человек во всех каналах: участники, связанные с тем же контактом (`person_link`), — тоже.
    out.extend(super::person::same_people(cache, &out)?);
    out.sort();
    out.dedup();
    Ok(out)
}

/// Поиск сообщений каналов по индексу: слова `words`, фильтр `scope` (каналы пусты — все каналы бесед профиля, без
/// почты). Документы сообщений, которых в базе больше нет, убираются из индекса.
pub fn search(index: &SearchIndex, cache: &Cache, words: &str, scope: &Scope, limit: usize) -> Result<Vec<Found>> {
    let stores = ChannelStore::all(cache)?;
    let mut scope = scope.clone();
    if scope.channels.is_empty() {
        scope.channels = stores.iter().map(|s| s.channel().0.clone()).collect();
        scope.channels.dedup();
    }
    scope.channels.retain(|c| c != super::MAIL);
    if scope.channels.is_empty() {
        return Ok(Vec::new());
    }
    let hits = index.hits_in(words, &scope, limit).map_err(|e| ChannelError::Storage(e.to_string()))?;
    let (mut out, mut gone) = (Vec::with_capacity(hits.len()), Vec::new());
    for h in hits {
        let Some(key) = parse_doc_id(&h.id) else { continue };
        let found = (|| -> Result<Option<Address>> {
            let Some(doc) = cache.chan_msg(key)? else { return Ok(None) };
            let Some(conv) = cache.chan_conv(doc.conv)? else { return Ok(None) };
            let Some(store) = stores.iter().find(|s| s.key() == conv.account) else { return Ok(None) };
            let c = super::ConversationId(conv.remote);
            Ok(Some(Address::message(store.channel(), store.account(), &c, &MessageId(doc.remote))))
        })()?;
        match found {
            Some(address) => out.push(Found { address, key, matched_in: h.attachment }),
            None => gone.push(h.id),
        }
    }
    if !gone.is_empty() && index.delete(&gone).is_ok() {
        let _ = index.commit();
    }
    Ok(out)
}

/// Фильтр индекса по запросу (`канал:`, `беседа:`, `от:`): `от:` — через [`authors`]; `contact` — адреса карточки
/// контакта значения `от:` (пусто — не контакт); `беседа:` — со связанными беседами (`super::linked::scope`).
pub fn scope_of(cache: &Cache, q: &crate::query::Query, contact: &dyn Fn(&str) -> Vec<String>) -> Result<Scope> {
    let mut authors_all = Vec::new();
    for v in &q.from {
        authors_all.extend(authors(cache, v, &contact(v))?);
    }
    Ok(Scope { channels: q.channels.clone(), convs: super::linked::scope(cache, &q.conversations)?, authors: authors_all })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{AccountId, Body, ChannelEvent, ChannelId, Conversation, ConversationId};

    fn msg(id: &str, author: &str, sent: i64, text: &str) -> Message {
        Message { id: MessageId::new(id), author: ParticipantId::new(author), sent, body: Body::Plain(text.into()), ..Message::default() }
    }

    fn setup() -> (Cache, ChannelStore, SearchIndex) {
        let cache = Cache::open_in_memory().unwrap();
        let store = ChannelStore::open(cache.clone(), &ChannelId::new("demo"), &AccountId::new("me@example.test"), "Я").unwrap();
        (cache, store, SearchIndex::in_memory().unwrap())
    }

    /// Записанное — в индексе с полями канала; правка — заменяет; удалённое и ушедшая беседа — уходят; `от:` по
    /// почте участника и по имени; пересборка — те же документы.
    #[test]
    fn messages_follow_the_store() {
        let (cache, store, index) = setup();
        let anna = Participant {
            id: ParticipantId::new("u-anna"),
            name: "Анна Смирнова".into(),
            emails: vec!["anna@example.test".into()],
            ..Participant::default()
        };
        let conv = |id: &str, title: &str| Conversation { id: ConversationId::new(id), title: title.into(), ..Conversation::default() };
        let ev = |c: &str, m: Vec<Message>| ChannelEvent::Messages { conversation: ConversationId::new(c), messages: m, live: false };
        let a = store
            .apply(vec![
                ChannelEvent::People(vec![anna]),
                ChannelEvent::Conversations(vec![conv("g", "Команда проекта"), conv("d", "Анна")]),
                ev("g", vec![msg("1", "u-anna", 1_000, "Отчёт по сборке готов"), msg("2", "u-boris", 2_000, "Принято")]),
                ev("d", vec![msg("3", "u-anna", 3_000, "Посмотри отчёт")]),
            ])
            .unwrap();
        applied(&index, &store, &a);
        let find = |words: &str, scope: &Scope| {
            search(&index, &cache, words, scope, 10).unwrap().into_iter().map(|f| f.address.message.unwrap().0).collect::<Vec<_>>()
        };
        let mut hits = find("отчет", &Scope::default());
        hits.sort();
        assert_eq!(hits, ["1", "3"]);
        let mut team = find("команда", &Scope::default());
        team.sort();
        assert_eq!(team, ["1", "2"], "название беседы ищется");
        let in_direct = Scope { convs: vec!["postman://demo/me@example.test/d".into()], ..Scope::default() };
        assert_eq!(find("отчет", &in_direct), ["3"]);
        let by_mail = authors(&cache, "anna@example.test", &[]).unwrap();
        assert!(by_mail.contains(&"demo:me@example.test:u-anna".to_owned()), "{by_mail:?}");
        let by_name = authors(&cache, "смирнова", &[]).unwrap();
        assert_eq!(by_name, by_mail);
        let from_anna = Scope { authors: by_mail, ..Scope::default() };
        assert_eq!(find("", &from_anna), ["3", "1"]);
        assert!(find("отчет", &Scope::channel(crate::channel::MAIL)).is_empty(), "почта — не в поиске каналов");

        let a = store.apply(vec![ev("g", vec![Message { edited: Some(5_000), ..msg("1", "u-anna", 1_000, "Черновик плана") }])]).unwrap();
        applied(&index, &store, &a);
        assert_eq!(find("план", &Scope::default()), ["1"]);
        assert!(find("сборке", &Scope::default()).is_empty(), "правка заменила документ");
        let a = store
            .apply(vec![
                ChannelEvent::Deleted { conversation: None, ids: vec![MessageId::new("1")], for_all: true },
                ChannelEvent::ConversationGone(ConversationId::new("d")),
            ])
            .unwrap();
        applied(&index, &store, &a);
        assert!(find("план", &Scope::default()).is_empty());
        assert!(find("посмотри", &Scope::default()).is_empty());

        index.clear().unwrap();
        index.commit().unwrap();
        assert!(rebuild(&index, &cache, &|| false).unwrap());
        assert_eq!(find("принято", &Scope::default()), ["2"]);
        forget(&index, &store);
        assert!(find("принято", &Scope::default()).is_empty());
    }

    #[test]
    fn doc_ids_and_phones() {
        assert_eq!(parse_doc_id(&doc_id(MsgKey(42))), Some(MsgKey(42)));
        assert_eq!(parse_doc_id("l42"), None);
        assert_eq!(phone_key("+7 (900) 000-00-00"), "+79000000000");
    }
}
