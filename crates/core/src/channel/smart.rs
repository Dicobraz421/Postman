//! Общий список бесед всех каналов и умные папки ядра (ADR 0012 «Вид»: гибрид): «Непрочитанное везде», «Упоминания
//! меня», «Закреплённые», «Человек во всех каналах» — беседы, а не общий поток сообщений; закреплённые — сверху.
//!
//! - **Счётчики** ([`chat_counts`]): у канала с `server_unread` — счётчики сервера, иначе — сообщения после моего
//!   «прочитано», кроме своих; упоминания — сообщения с моим id в `mentions`. У почты — непрочитанные письма беседы и
//!   упоминания по тексту (`MailSource`). Заглушённое не считается ([`totals`]) — как CMF-01.
//! - **Заглушить, закрепить** ([`set_mute`], [`set_pin`]): одни команды для всех каналов — на сервер при
//!   `server_mute`/`server_pin` (ответ — `Action` для канала), иначе — в базе профиля (`local["conv/…"]`); почта —
//!   ключевыми словами (`MailSource::plan`), как CMF-01 и CMF-02.
//!
//! Чтение базы — I/O: из фона, не из потока интерфейса.

use std::collections::HashSet;

use postman_cache::{Cache, EmailRow, LocalPut, Recipients, Scan};

use super::mail::MailSource;
use super::{Action, Address, ChannelError, ChannelFeatures, ChannelStore, Chats, ConversationSource, Counts, MessageKind, ParticipantId, Summary};

type Result<T> = std::result::Result<T, ChannelError>;

/// Беседа в списке: сводка, счётчики, заглушена ли и закреплена ли (сервером или в базе).
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub summary: Summary,
    pub counts: Counts,
    pub muted: bool,
    pub pinned: bool,
}

/// Умная папка ядра.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Folder {
    /// Беседы с непрочитанным, кроме заглушённых.
    Unread,
    /// Беседы, где меня упомянули в непрочитанном.
    Mentions,
    /// Закреплённые.
    Pinned,
    /// Беседы, где есть этот человек: его адреса (`index::authors` — участники каналов, почта, телефоны).
    Person(Vec<String>),
}

/// Непрочитанных в беседе считается не больше: дальше — «999+» у интерфейса.
pub const COUNT_CAP: u32 = 999;
/// Бесед почты в умной папке — не больше, новые первыми.
pub const MAIL_THREADS: usize = 500;

// --- заглушить, закрепить в базе ---

fn mark_key(at: &Address, what: &str) -> String {
    format!("conv/{}/{what}", at.of_conversation())
}

/// Префикс отметок бесед учётки канала (`ChannelStore::forget` их убирает).
pub fn marks_prefix(chan: &super::ChanAccount) -> String {
    format!("conv/{}", super::account_prefix(chan))
}

fn local_i64(cache: &Cache, key: &str) -> Result<Option<i64>> {
    Ok(cache.local(key)?.and_then(|d| <[u8; 8]>::try_from(d.value.as_slice()).ok()).map(i64::from_le_bytes))
}

/// Заглушено в базе до (Unix, мс).
pub fn local_mute(cache: &Cache, at: &Address) -> Result<Option<i64>> {
    local_i64(cache, &mark_key(at, "mute"))
}

/// Закреплено в базе.
pub fn local_pin(cache: &Cache, at: &Address) -> Result<bool> {
    Ok(cache.local(&mark_key(at, "pin"))?.is_some())
}

/// Заглушить до `until` (Unix, мс; `i64::MAX` — навсегда) или вернуть звук (`None`). Канал умеет на сервере —
/// ответ — действие каналу; иначе — отметка в базе профиля.
pub fn set_mute(cache: &Cache, at: &Address, features: &ChannelFeatures, until: Option<i64>) -> Result<Option<Action>> {
    if features.server_mute {
        return Ok(Some(Action::Mute { conversation: at.conversation.clone(), until }));
    }
    let key = mark_key(at, "mute");
    match until {
        Some(t) => cache.put_local(&LocalPut { key, value: t.to_le_bytes().to_vec(), parts: Vec::new() })?,
        None => cache.remove_local(&[key.as_str()])?,
    }
    Ok(None)
}

/// В базе уже [`crate::pin::LIMIT`] закреплённых бесед каналов: новую не закрепить, пока не открепят другую.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Full;

/// Закрепить или открепить. Канал умеет на сервере — действие каналу (предел — у сервера); иначе — в базе, не больше
/// [`crate::pin::LIMIT`].
pub fn set_pin(cache: &Cache, at: &Address, features: &ChannelFeatures, on: bool) -> Result<std::result::Result<Option<Action>, Full>> {
    if features.server_pin {
        return Ok(Ok(Some(Action::Pin { conversation: at.conversation.clone(), on })));
    }
    let key = mark_key(at, "pin");
    if !on {
        cache.remove_local(&[key.as_str()])?;
        return Ok(Ok(None));
    }
    let pinned = cache.local_prefix("conv/")?.into_iter().filter(|(k, _)| k.ends_with("/pin")).count();
    if pinned >= crate::pin::LIMIT && cache.local(&key)?.is_none() {
        return Ok(Err(Full));
    }
    cache.put_local(&LocalPut { key, value: vec![1], parts: Vec::new() })?;
    Ok(Ok(None))
}

/// Заглушена ли беседа сейчас (`now`, Unix, мс): сервером или в базе.
pub fn is_muted(cache: &Cache, s: &Summary, now: i64) -> Result<bool> {
    Ok(s.conversation.muted_until.is_some_and(|t| t > now) || local_mute(cache, &s.address)?.is_some_and(|t| t > now))
}

/// Закреплена ли беседа: сервером или в базе.
pub fn is_pinned(cache: &Cache, s: &Summary) -> Result<bool> {
    Ok(s.conversation.pinned || local_pin(cache, &s.address)?)
}

// --- счётчики ---

/// Непрочитанное и упоминания меня в беседе чата: счётчики сервера (`server_unread`) или сообщения после моего
/// «прочитано», кроме своих и служебных; упоминание — мой id в `mentions`.
pub fn chat_counts(store: &ChannelStore, s: &Summary, features: &ChannelFeatures, me: Option<&ParticipantId>) -> Result<Counts> {
    if features.server_unread
        && let Some(c) = s.conversation.unread
    {
        return Ok(c);
    }
    let mut counts = Counts::default();
    let mut before = None;
    'pages: loop {
        let page = store.timeline(&s.conversation.id, before.as_ref(), 100)?;
        let Some(first) = page.first() else { break };
        let next = first.id().clone();
        for e in page.iter().rev() {
            if s.conversation.last_read.as_ref() == Some(e.id()) || counts.unread >= COUNT_CAP {
                break 'pages;
            }
            let Some(m) = e.message() else { continue };
            if m.mine || m.kind != MessageKind::Normal {
                continue;
            }
            counts.unread += 1;
            if me.is_some_and(|me| m.mentions.contains(me)) {
                counts.mentions += 1;
            }
        }
        if page.len() < 100 {
            break;
        }
        before = Some(next);
    }
    Ok(counts)
}

/// Беседы чатов всех каналов: счётчики, заглушены, закреплены. `now` — Unix, мс.
pub fn chat_items(cache: &Cache, now: i64) -> Result<Vec<Item>> {
    let stores = ChannelStore::all(cache)?;
    let mut out = Vec::new();
    for s in Chats::new(cache.clone()).list(0, usize::MAX)? {
        let Some(store) = stores.iter().find(|st| st.chan() == s.address.chan()) else { continue };
        let features = store.features()?;
        let me = store.me()?.map(|p| p.id);
        let counts = chat_counts(store, &s, &features, me.as_ref())?;
        let (muted, pinned) = (is_muted(cache, &s, now)?, is_pinned(cache, &s)?);
        out.push(Item { summary: s, counts, muted, pinned });
    }
    Ok(out)
}

/// Условие на письмо и его получателей.
pub type Want<'a> = dyn Fn(&EmailRow, &Recipients) -> bool + 'a;

/// Беседы почты по условию на письма (все ящики профиля): новые первыми, не больше [`MAIL_THREADS`].
pub fn mail_items(mail: &MailSource, cache: &Cache, want: &Want) -> Result<Vec<Item>> {
    let mut found: Vec<(i64, Address)> = Vec::new();
    let mut seen = HashSet::new();
    for scope in mail.scopes() {
        cache.scan(&Scan::Account(scope.map(str::to_owned)), |row, to| {
            if want(&row, &to)
                && let Some(at) = mail.address_of(&row)
                && seen.insert(at.clone())
            {
                found.push((row.received_at, at));
            }
            true
        })?;
    }
    found.sort_by_key(|f| std::cmp::Reverse(f.0));
    let mut out = Vec::new();
    for (_, at) in found.into_iter().take(MAIL_THREADS) {
        let Some(s) = mail.conversation(&at)? else { continue };
        let counts = s.conversation.unread.unwrap_or_default();
        let (muted, pinned) = (s.conversation.muted_until.is_some(), s.conversation.pinned);
        out.push(Item { summary: s, counts, muted, pinned });
    }
    Ok(out)
}

/// Закреплённые сверху, дальше — новые первыми.
pub fn arrange(items: &mut [Item]) {
    items.sort_by(|a, b| b.pinned.cmp(&a.pinned).then(b.summary.last.cmp(&a.summary.last)));
}

/// Общий список бесед всех каналов (без почты: у почты — свои папки, ADR 0012 «Вид»), закреплённые сверху.
pub fn all_chats(cache: &Cache, now: i64) -> Result<Vec<Item>> {
    let mut items = chat_items(cache, now)?;
    arrange(&mut items);
    Ok(items)
}

/// Беседы чата с этим человеком: адреса участников беседы пересекаются с `people`.
fn has_person(chats: &Chats, s: &Summary, people: &HashSet<String>) -> Result<bool> {
    let chan = s.address.chan();
    Ok(chats.people(&s.address)?.iter().any(|p| super::index::participant_addresses(&chan, p).iter().any(|a| people.contains(a))))
}

/// Умная папка: беседы чатов и почты (`mail` — `None`, без почты), закреплённые сверху. `now` — Unix, мс.
pub fn folder(cache: &Cache, mail: Option<&MailSource>, folder: &Folder, now: i64) -> Result<Vec<Item>> {
    let chats = Chats::new(cache.clone());
    let people: HashSet<String> = match folder {
        Folder::Person(a) => a.iter().map(|x| x.trim().to_lowercase()).collect(),
        _ => HashSet::new(),
    };
    let mut items = Vec::new();
    for i in chat_items(cache, now)? {
        let keep = match folder {
            Folder::Unread => i.counts.unread > 0 && !i.muted,
            Folder::Mentions => i.counts.mentions > 0,
            Folder::Pinned => i.pinned,
            Folder::Person(_) => has_person(&chats, &i.summary, &people)?,
        };
        if keep {
            items.push(i);
        }
    }
    if let Some(mail) = mail {
        let me = mail.me();
        let addr = |a: &str| people.contains(&a.trim().to_lowercase());
        let want: Box<Want> = match folder {
            Folder::Unread => Box::new(|r, _| !r.seen && !crate::mute::is_muted(r) && !mail.mine(r)),
            Folder::Mentions => Box::new(|r, _| {
                !r.seen && !mail.mine(r) && !me.is_empty() && cache.body(&r.id).ok().flatten().is_some_and(|b| me.mentioned_in_body(&b))
            }),
            Folder::Pinned => Box::new(|r, _| crate::pin::is_pinned(r)),
            Folder::Person(_) => Box::new(|r, to| addr(&r.from_addr) || to.to.iter().chain(&to.cc).any(|a| addr(a))),
        };
        let mut mail_items = mail_items(mail, cache, &*want)?;
        if *folder == Folder::Unread {
            mail_items.retain(|i| !i.muted);
        }
        items.extend(mail_items);
    }
    arrange(&mut items);
    Ok(items)
}

/// Счётчики для трея и папок: непрочитанное и упоминания всех бесед, кроме заглушённых.
pub fn totals(items: &[Item]) -> Counts {
    items.iter().filter(|i| !i.muted).fold(Counts::default(), |mut t, i| {
        t.unread = t.unread.saturating_add(i.counts.unread);
        t.mentions = t.mentions.saturating_add(i.counts.mentions);
        t
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::mail::MailAccount;
    use crate::channel::{
        AccountId, Body, ChannelEvent, ChannelId, Conversation, ConversationId, Member, Message, MessageId, Participant, Reader, Role,
    };
    use crate::filters::Me;

    fn msg(id: &str, author: &str, sent: i64, mentions: &[&str]) -> Message {
        Message {
            id: MessageId::new(id),
            author: ParticipantId::new(author),
            sent,
            body: Body::Plain(format!("сообщение {id}")),
            mentions: mentions.iter().map(|m| ParticipantId::new(*m)).collect(),
            mine: author == "u-me",
            ..Message::default()
        }
    }

    fn setup() -> (Cache, ChannelStore) {
        let cache = Cache::open_in_memory().unwrap();
        let store = ChannelStore::open(cache.clone(), &ChannelId::new("demo"), &AccountId::new("me@example.test"), "Я").unwrap();
        store.set_me(&Participant { id: ParticipantId::new("u-me"), name: "Я".into(), ..Participant::default() }).unwrap();
        let conv = |id: &str| Conversation { id: ConversationId::new(id), title: id.into(), ..Conversation::default() };
        let ev = |c: &str, m: Vec<Message>| ChannelEvent::Messages { conversation: ConversationId::new(c), messages: m, live: true };
        store
            .apply(vec![
                ChannelEvent::People(vec![Participant {
                    id: ParticipantId::new("u-anna"),
                    name: "Анна".into(),
                    emails: vec!["anna@example.test".into()],
                    ..Participant::default()
                }]),
                ChannelEvent::Conversations(vec![conv("g"), conv("d"), conv("quiet")]),
                ev(
                    "g",
                    vec![msg("1", "u-boris", 10, &[]), msg("2", "u-me", 20, &[]), msg("3", "u-boris", 30, &["u-me"]), msg("4", "u-boris", 40, &[])],
                ),
                ChannelEvent::Read { conversation: ConversationId::new("g"), who: Reader::Me, up_to: MessageId::new("2") },
                ev("d", vec![msg("5", "u-anna", 50, &[])]),
                ChannelEvent::Members {
                    conversation: ConversationId::new("d"),
                    changed: vec![Member { id: ParticipantId::new("u-anna"), role: Role::Member }],
                    left: vec![],
                },
                ChannelEvent::Read { conversation: ConversationId::new("d"), who: Reader::Me, up_to: MessageId::new("5") },
                ev("quiet", vec![msg("6", "u-boris", 60, &[])]),
            ])
            .unwrap();
        (cache, store)
    }

    fn at(c: &str) -> Address {
        Address::conversation(&ChannelId::new("demo"), &AccountId::new("me@example.test"), &ConversationId::new(c))
    }

    fn ids(items: &[Item]) -> Vec<String> {
        items.iter().map(|i| i.summary.address.conversation.0.clone()).collect()
    }

    /// Счётчики после «прочитано» без своих, упоминание меня; заглушить и закрепить в базе (канал не умеет на
    /// сервере); умные папки и трей без заглушённого; «Человек во всех каналах» — по почте участника и письмам.
    #[test]
    fn smart_folders_over_channels() {
        let (cache, _store) = setup();
        let none = ChannelFeatures::default();
        assert_eq!(set_mute(&cache, &at("quiet"), &none, Some(i64::MAX)).unwrap(), None);
        assert_eq!(set_pin(&cache, &at("d"), &none, true).unwrap(), Ok(None));
        let server = ChannelFeatures { server_mute: true, server_pin: true, ..ChannelFeatures::default() };
        assert!(matches!(set_mute(&cache, &at("g"), &server, None).unwrap(), Some(Action::Mute { until: None, .. })), "на сервер — действием");

        let all = all_chats(&cache, 0).unwrap();
        assert_eq!(ids(&all), ["d", "quiet", "g"], "закреплённое сверху, дальше — новые");
        let g = all.iter().find(|i| i.summary.address == at("g")).unwrap();
        assert_eq!(g.counts, Counts { unread: 2, mentions: 1 });
        assert_eq!(totals(&all), Counts { unread: 2, mentions: 1 }, "заглушённое не считается");
        assert_eq!(ids(&folder(&cache, None, &Folder::Unread, 0).unwrap()), ["g"]);
        assert_eq!(ids(&folder(&cache, None, &Folder::Mentions, 0).unwrap()), ["g"]);
        assert_eq!(ids(&folder(&cache, None, &Folder::Pinned, 0).unwrap()), ["d"]);

        cache
            .upsert_emails(&[EmailRow {
                id: "e1".into(),
                thread_id: "t1".into(),
                mailbox_ids: vec!["in".into()],
                received_at: 70,
                subject: "Отчёт".into(),
                from_name: String::new(),
                from_addr: "Anna@Example.test".into(),
                preview: String::new(),
                seen: false,
                flagged: false,
                has_attachment: false,
                size: 0,
                keywords: Vec::new(),
            }])
            .unwrap();
        let mail = MailSource::new(cache.clone(), vec![MailAccount::new(None, "me@example.test")], Me::new(["me@example.test"], ["Я"]));
        let anna = crate::channel::index::authors(&cache, "anna@example.test", &[]).unwrap();
        let person = folder(&cache, Some(&mail), &Folder::Person(anna), 0).unwrap();
        assert_eq!(ids(&person), ["d", "t1"], "личная беседа (закреплена) и письмо Анны");
        let unread = folder(&cache, Some(&mail), &Folder::Unread, 0).unwrap();
        assert_eq!(ids(&unread), ["t1", "g"]);

        set_mute(&cache, &at("quiet"), &none, None).unwrap();
        assert_eq!(set_pin(&cache, &at("d"), &none, false).unwrap(), Ok(None));
        assert!(folder(&cache, None, &Folder::Pinned, 0).unwrap().is_empty());
        assert_eq!(totals(&all_chats(&cache, 0).unwrap()).unread, 3);
    }
}
