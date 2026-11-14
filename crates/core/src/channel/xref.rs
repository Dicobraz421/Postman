//! Кросс-ссылки и обратные ссылки (ADR 0012 п. 11): ядро вынимает из каждого сохранённого тела — сообщения канала,
//! письма, задачи, встречи — ссылки `postman://`, `mid:` (RFC 2392) и ссылки каналов ([`ChannelPlugin::parse_link`])
//! и пишет `xref`; у сообщения видно «Где на него сослались» ([`backlinks`]).
//!
//! - **Цель** — ключ [`target_key`]: у письма с `Message-ID` — `mid:<id>` (одно письмо — одна цель, в каком бы ящике и
//!   какой бы беседе оно ни лежало, и `postman://mail/…`, и `mid:` в чужом письме ведут туда же); у остального —
//!   адрес `postman://…`. Ссылка канала (`https://t.me/c/…`) разбирается плагином в адрес и становится тем же ключом.
//! - **Источник** — [`Source`]: сообщение канала — его адрес; письмо, задача, встреча — ключ этого профиля
//!   (`letter:<n>`, `task:<uid>`, `event:<uid>`): таблица — индекс базы профиля, на другой компьютер она не уходит
//!   и собирается заново из тел.
//! - **Наружу** ([`Links::outside`]) — ссылка канала, если она есть (`mid:` у почты, `https://t.me/c/…` у Telegram),
//!   иначе `postman://`.
//!
//! Ссылки источника заменяются целиком при каждой новой версии тела. Источника больше нет (сообщение удалено, письмо
//! ушло, задача снята) — его строки уходят при чтении обратных ссылок, беседа ушла и «Выйти» — сразу. Чтение и запись
//! базы — I/O: из фона, не из потока интерфейса.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use postman_cache::{Cache, ConvKey};

use super::{Address, Applied, ChanAccount, ChannelError, ChannelPlugin, ChannelStore, Entry, Message, MessageId, Participant};
use crate::model::LetterId;

type Result<T> = std::result::Result<T, ChannelError>;

/// Схема ссылки на письмо (RFC 2392).
pub const MID: &str = "mid:";
/// Целей у одного источника — не больше (письмо-рассылка со ссылками на всё подряд).
pub const MAX_TARGETS: usize = 200;

/// Плагины каналов для ссылок: разобрать ссылку канала, дать ссылку наружу, ссылку на человека для карточки контакта.
/// Дешёвый клон.
#[derive(Clone, Default)]
pub struct Links(Arc<[Arc<dyn ChannelPlugin>]>);

static INSTALLED: OnceLock<Links> = OnceLock::new();

impl Links {
    pub fn new(plugins: Vec<Arc<dyn ChannelPlugin>>) -> Self {
        Self(plugins.into())
    }

    /// Каналы реестра плагинов.
    pub fn of(registry: &crate::backend::Registry) -> Self {
        Self::new(registry.channels().to_vec())
    }

    /// Плагины процесса — один раз, в композиционном корне: ими пользуются фоновые индексы (письма, задачи, потоки
    /// учёток). Уже поставлено — `false`.
    pub fn install(self) -> bool {
        INSTALLED.set(self).is_ok()
    }

    /// Плагины процесса ([`Links::install`]); не поставлены — без каналов (`postman://` и `mid:` разбираются всё равно).
    pub fn installed() -> Self {
        INSTALLED.get().cloned().unwrap_or_default()
    }

    fn plugin(&self, id: &str) -> Option<&Arc<dyn ChannelPlugin>> {
        self.0.iter().find(|p| p.id() == id)
    }

    /// Ссылка → ключ цели: `postman://…`, `mid:…`, ссылка канала. Чужое — `None`.
    pub fn parse(&self, url: &str) -> Option<String> {
        let url = url.trim();
        if let Some(a) = Address::parse(url) {
            return Some(target_key(&a));
        }
        if let Some(mid) = parse_mid(url) {
            return Some(format!("{MID}{mid}"));
        }
        self.0.iter().find_map(|p| p.parse_link(url)).map(|a| target_key(&a))
    }

    /// Ссылка на беседу или сообщение для письма или другого канала: ссылка канала, если она есть (`mid:` у письма,
    /// [`ChannelPlugin::link`] у чатов), иначе `postman://…`.
    pub fn outside(&self, at: &Address) -> String {
        if at.channel.0 == super::MAIL {
            if let Some(m) = at.message.as_ref().filter(|m| !super::mail::is_local(m)) {
                return mid_url(&m.0);
            }
            return at.to_string();
        }
        self.plugin(&at.channel.0).and_then(|p| p.link(at)).unwrap_or_else(|| at.to_string())
    }

    /// Ссылка на человека канала для карточки контакта ([`ChannelPlugin::person_url`]); у канала нет — `None`.
    pub fn person_url(&self, chan: &ChanAccount, p: &Participant) -> Option<String> {
        self.plugin(&chan.channel.0).and_then(|pl| pl.person_url(p))
    }
}

/// `mid:` ссылка на письмо: `Message-ID` без `<>` в процент-кодировке (RFC 2392).
pub fn mid_url(message_id: &str) -> String {
    format!("{MID}{}", super::encode_segment(postman_cache::bare_message_id(message_id).unwrap_or(message_id)))
}

/// `Message-ID` ссылки `mid:` (без части `/<Content-ID>`); не `mid:` — `None`.
pub fn parse_mid(url: &str) -> Option<String> {
    let head = url.get(..MID.len())?;
    if !head.eq_ignore_ascii_case(MID) {
        return None;
    }
    let rest = url[MID.len()..].split('/').next()?;
    let id = super::decode_segment(rest)?;
    postman_cache::bare_message_id(&id).map(str::to_owned)
}

/// Ключ цели адреса: письмо с `Message-ID` — `mid:<id>`, остальное — адрес.
pub fn target_key(at: &Address) -> String {
    match &at.message {
        Some(m) if at.channel.0 == super::MAIL && !super::mail::is_local(m) => format!("{MID}{}", m.0),
        _ => at.to_string(),
    }
}

/// Откуда ссылка.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Source {
    /// Сообщение канала.
    Message(Address),
    /// Письмо этого профиля.
    Letter(LetterId),
    /// Задача (UID).
    Task(String),
    /// Встреча (UID).
    Event(String),
}

const LETTER: &str = "letter:";
const TASK: &str = "task:";
const EVENT: &str = "event:";

impl Source {
    /// Ключ строки `xref`.
    pub fn key(&self) -> String {
        match self {
            Source::Message(a) => a.to_string(),
            Source::Letter(l) => format!("{LETTER}{}", l.0),
            Source::Task(uid) => format!("{TASK}{uid}"),
            Source::Event(uid) => format!("{EVENT}{uid}"),
        }
    }

    /// Обратно к [`Source::key`].
    pub fn parse(key: &str) -> Option<Self> {
        if let Some(a) = Address::parse(key) {
            return Some(Source::Message(a));
        }
        if let Some(n) = key.strip_prefix(LETTER) {
            return n.parse().ok().map(|n| Source::Letter(LetterId(n)));
        }
        if let Some(uid) = key.strip_prefix(TASK) {
            return Some(Source::Task(uid.to_owned()));
        }
        key.strip_prefix(EVENT).map(|uid| Source::Event(uid.to_owned()))
    }
}

/// Схемы, с которых начинается ссылка в тексте.
const SCHEMES: &[&str] = &["postman://", "mid:", "https://", "http://"];

/// Знаки, на которых кончается ссылка в тексте (разметка Markdown, кавычки, скобки).
fn stops(c: char) -> bool {
    c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '`' | '|')
}

/// Ссылки в тексте: слова, где есть одна из схем (с границей слова перед ней); хвостовые знаки препинания —
/// не часть ссылки.
pub fn urls_in(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for token in text.split(stops).filter(|t| !t.is_empty()) {
        let lower = token.to_ascii_lowercase();
        let start = SCHEMES
            .iter()
            .filter_map(|s| {
                lower.match_indices(s).map(|(i, _)| i).find(|&i| i == 0 || !lower[..i].chars().next_back().is_some_and(char::is_alphanumeric))
            })
            .min();
        let Some(start) = start else { continue };
        let url = token[start..].trim_end_matches(['.', ',', ';', ':', '!', '?']);
        if SCHEMES.iter().all(|s| !url.eq_ignore_ascii_case(s)) {
            out.push(url);
        }
    }
    out
}

/// Адреса ссылок документа (`Inline::Link`), по порядку чтения.
pub fn hrefs(doc: &postman_ir::Document) -> Vec<&str> {
    use postman_ir::{Block, Inline};
    fn inl<'a>(items: &'a [Inline], out: &mut Vec<&'a str>) {
        for i in items {
            if let Inline::Link { href, content, .. } = i {
                out.push(href);
                inl(content, out);
            }
        }
    }
    fn blocks<'a>(bs: &'a [Block], out: &mut Vec<&'a str>) {
        for b in bs {
            match b {
                Block::Paragraph { content, .. } | Block::Heading { content, .. } => inl(content, out),
                Block::Quote(inner) => blocks(inner, out),
                Block::List { items, .. } => items.iter().for_each(|i| blocks(i, out)),
                Block::Table(rows) => rows.iter().flatten().for_each(|c| inl(c, out)),
                Block::Code(_) | Block::Rule | Block::Figure => {}
            }
        }
    }
    let mut out = Vec::new();
    blocks(&doc.blocks, &mut out);
    out
}

/// Ключи целей в тексте и адресах ссылок, без повторов, не больше [`MAX_TARGETS`].
pub fn targets<'a>(links: &Links, text: &'a str, hrefs: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for url in hrefs.into_iter().chain(urls_in(text)) {
        if out.len() >= MAX_TARGETS {
            break;
        }
        if let Some(t) = links.parse(url)
            && !out.contains(&t)
        {
            out.push(t);
        }
    }
    out
}

/// Цели сообщения канала: тело (текст и адреса ссылок) и цитаты.
pub fn message_targets(links: &Links, m: &Message) -> Vec<String> {
    let mut text = m.body.plain();
    for q in &m.quotes {
        text.push('\n');
        text.push_str(&q.text);
    }
    match &m.body {
        super::Body::Ir(doc) => targets(links, &text, hrefs(doc)),
        _ => targets(links, &text, []),
    }
}

/// Ссылки источника — ровно эти цели (прежние уходят).
pub fn index(cache: &Cache, source: &Source, targets: &[String]) -> Result<()> {
    Ok(cache.xref_set(&source.key(), targets)?)
}

/// Письмо `letter` с текстом `text` (текст тела или Markdown разобранного HTML — с адресами ссылок).
pub fn index_letter(cache: &Cache, links: &Links, letter: LetterId, text: &str) -> Result<()> {
    index(cache, &Source::Letter(letter), &targets(links, text, []))
}

/// Задача `uid` с описанием `text`.
pub fn index_task(cache: &Cache, links: &Links, uid: &str, text: &str) -> Result<()> {
    index(cache, &Source::Task(uid.to_owned()), &targets(links, text, []))
}

/// Встреча `uid` с описанием и местом `text`.
pub fn index_event(cache: &Cache, links: &Links, uid: &str, text: &str) -> Result<()> {
    index(cache, &Source::Event(uid.to_owned()), &targets(links, text, []))
}

/// Источников больше нет: письма удалены, задачи сняты.
pub fn forget(cache: &Cache, sources: &[Source]) -> Result<()> {
    Ok(cache.xref_drop(&sources.iter().map(Source::key).collect::<Vec<_>>())?)
}

/// Пачка записана ([`Applied`]): ссылки новых и изменённых сообщений — заново, «удалено у всех» — без ссылок, беседа
/// ушла — ссылки всех её сообщений. Сбой — в журнал: ссылки догонит следующая версия тела.
pub fn applied(store: &ChannelStore, links: &Links, a: &Applied) {
    let chan = store.chan();
    for c in &a.gone {
        let prefix = format!("{}/", Address::conversation(&chan.channel, &chan.account, c));
        if let Err(e) = store.cache().xref_drop_prefix(&prefix) {
            tracing::warn!("channel links: conversation links left: {e}");
        }
    }
    let mut convs: HashMap<ConvKey, Option<Address>> = HashMap::new();
    for k in a.indexed.iter().chain(&a.unindexed) {
        let result = (|| -> Result<()> {
            let Some((conv, entry)) = store.entry_by_key(*k)? else { return Ok(()) };
            let at = match convs.get(&conv) {
                Some(at) => at.clone(),
                None => {
                    let at = store.conversation_by_key(conv)?.map(|c| c.address);
                    convs.insert(conv, at.clone());
                    at
                }
            };
            let Some(at) = at else { return Ok(()) };
            let at = Address { message: Some(entry.id().clone()), ..at };
            let found = match &entry {
                Entry::Message { message, .. } => message_targets(links, message),
                Entry::Deleted { .. } => Vec::new(),
            };
            index(store.cache(), &Source::Message(at), &found)
        })();
        if let Err(e) = result {
            tracing::warn!("channel links: message links not stored: {e}");
        }
    }
}

/// Источник ещё есть в базе (письмо, задача, сообщение канала); встречи не хранятся — есть всегда.
fn exists(cache: &Cache, s: &Source) -> Result<bool> {
    Ok(match s {
        Source::Letter(l) => cache.letter(crate::store::cache_id(*l))?.is_some(),
        Source::Task(uid) => cache.task(uid)?.is_some_and(|t| !t.deleted),
        Source::Event(_) => true,
        Source::Message(at) if at.channel.0 == super::MAIL => true,
        Source::Message(at) => {
            let (Some(store), Some(id)) = (ChannelStore::find(cache, &at.channel, &at.account)?, at.message.as_ref()) else { return Ok(false) };
            matches!(store.message(&at.conversation, id)?, Some(Entry::Message { .. }))
        }
    })
}

/// «Где на это сослались»: источники ссылок на беседу или сообщение `at`, без повторов. Источники, которых больше
/// нет, уходят из таблицы.
pub fn backlinks(cache: &Cache, at: &Address) -> Result<Vec<Source>> {
    let mut out = Vec::new();
    let mut gone = Vec::new();
    for key in cache.xref_sources(&target_key(at))? {
        let Some(s) = Source::parse(&key) else { continue };
        if exists(cache, &s)? {
            out.push(s);
        } else {
            gone.push(key);
        }
    }
    if !gone.is_empty() {
        cache.xref_drop(&gone)?;
    }
    Ok(out)
}

/// На что ссылается источник (ключи целей).
pub fn targets_of(cache: &Cache, source: &Source) -> Result<Vec<String>> {
    Ok(cache.xref_targets(&source.key())?)
}

/// Адрес письма (`mid:` цели): `Message-ID` ключа цели; не письмо — `None`.
pub fn mail_target(key: &str) -> Option<MessageId> {
    key.strip_prefix(MID).map(MessageId::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{AccountId, Body, ChannelEvent, ChannelId, ConversationId};

    #[test]
    fn urls_in_text() {
        let text = "См. postman://demo/me/g/1, и mid:m1%40example.test. А ещё [тут](https://t.me/c/1/2) humid:x (https://example.test/a).";
        assert_eq!(urls_in(text), ["postman://demo/me/g/1", "mid:m1%40example.test", "https://t.me/c/1/2", "https://example.test/a"]);
        assert_eq!(parse_mid("MID:m1%40example.test/part"), Some("m1@example.test".into()));
        assert_eq!(parse_mid("mid:%3Cm1@example.test%3E"), Some("m1@example.test".into()));
        assert_eq!(mid_url("<a b@example.test>"), "mid:a%20b@example.test");
    }

    /// Письмо с `Message-ID` — цель `mid:` и по `postman://mail/…`, и по `mid:`; без него — адрес.
    #[test]
    fn mail_targets_are_message_ids() {
        let links = Links::default();
        let mail = Address::message(
            &ChannelId::new("mail"),
            &AccountId::new("me@example.test"),
            &ConversationId::new("t1"),
            &MessageId::new("m1@example.test"),
        );
        assert_eq!(target_key(&mail), "mid:m1@example.test");
        assert_eq!(links.parse(&mail.to_string()).as_deref(), Some("mid:m1@example.test"));
        assert_eq!(links.parse("mid:m1@example.test").as_deref(), Some("mid:m1@example.test"));
        assert_eq!(links.outside(&mail), "mid:m1@example.test");
        let local = Address { message: Some(MessageId::new("~7")), ..mail.clone() };
        assert_eq!(target_key(&local), local.to_string());
        assert_eq!(links.parse("https://example.test/x"), None, "без каналов ссылка сайта — не цель");
        for s in [Source::Letter(LetterId(7)), Source::Task("u1".into()), Source::Event("e1".into()), Source::Message(mail)] {
            assert_eq!(Source::parse(&s.key()), Some(s));
        }
    }

    /// Сообщение со ссылками на письмо и другое сообщение — обратные ссылки у обоих; правка — ссылки заново; удалено —
    /// источник уходит; беседа ушла — её источники тоже.
    #[test]
    fn backlinks_follow_messages() {
        let cache = Cache::open_in_memory().unwrap();
        let store = ChannelStore::open(cache.clone(), &ChannelId::new("demo"), &AccountId::new("me@example.test"), "Я").unwrap();
        let links = Links::default();
        let g = ConversationId::new("g");
        let target = Address::message(store.channel(), store.account(), &g, &MessageId::new("1"));
        let m =
            |id: &str, text: &str, edited| Message { id: MessageId::new(id), sent: 10, edited, body: Body::Plain(text.into()), ..Message::default() };
        let ev = |ms| ChannelEvent::Messages { conversation: g.clone(), messages: ms, live: false };
        let a = store.apply(vec![ev(vec![m("1", "исходное", None), m("2", &format!("см. {target} и mid:m1@example.test"), None)])]).unwrap();
        applied(&store, &links, &a);
        let source = Source::Message(Address { message: Some(MessageId::new("2")), ..target.of_conversation() });
        assert_eq!(backlinks(&cache, &target).unwrap(), std::slice::from_ref(&source));
        let letter = Address::message(
            &ChannelId::new("mail"),
            &AccountId::new("me@example.test"),
            &ConversationId::new("t"),
            &MessageId::new("m1@example.test"),
        );
        assert_eq!(backlinks(&cache, &letter).unwrap(), std::slice::from_ref(&source));
        index_task(&cache, &links, "task-1", &format!("Ответить: {target}")).unwrap();
        assert_eq!(backlinks(&cache, &target).unwrap().len(), 1, "задачи в базе нет — источник уходит");

        let a = store.apply(vec![ev(vec![m("2", "без ссылок", Some(20))])]).unwrap();
        applied(&store, &links, &a);
        assert!(backlinks(&cache, &target).unwrap().is_empty(), "правка — ссылки заново");
        let a = store.apply(vec![ev(vec![m("2", &format!("снова {target}"), Some(30))])]).unwrap();
        applied(&store, &links, &a);
        assert_eq!(backlinks(&cache, &target).unwrap().len(), 1);
        let a = store.apply(vec![ChannelEvent::Deleted { conversation: None, ids: vec![MessageId::new("2")], for_all: true }]).unwrap();
        applied(&store, &links, &a);
        assert!(backlinks(&cache, &target).unwrap().is_empty(), "удалено у всех — ссылок нет");
        let a = store.apply(vec![ev(vec![m("3", &format!("и ещё {target}"), None)])]).unwrap();
        applied(&store, &links, &a);
        assert_eq!(backlinks(&cache, &target).unwrap().len(), 1);
        let a = store.apply(vec![ChannelEvent::ConversationGone(g.clone())]).unwrap();
        applied(&store, &links, &a);
        assert!(cache.xref_sources(&target_key(&target)).unwrap().is_empty());
    }
}
