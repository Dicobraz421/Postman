//! Связанные беседы (ADR 0012 п. 11): почтовая беседа ↔ группа Telegram ↔ комната Matrix — «Связать с беседой…»
//! (`conv_link`, в базе профиля, в обе стороны). Связанные видны рядом в чтении, «Медиа беседы» и поиск идут по всем
//! ([`group`], [`scope`]). Ядро предлагает связь ([`suggest`]: совпали тема или участники), но само её не ставит —
//! ставит пользователь.
//!
//! Связи — между адресами бесед (`postman://…` без сообщения): переносимы, как ссылки. Чтение и запись базы — I/O: из
//! фона, не из потока интерфейса.

use std::collections::{HashSet, VecDeque};

use postman_cache::Cache;

use super::mail::MailSource;
use super::{Address, ChannelError, Chats, ConversationSource, Participant, Summary};

type Result<T> = std::result::Result<T, ChannelError>;

/// Бесед в одной группе связанных — не больше (обход связей).
pub const GROUP_LIMIT: usize = 64;
/// Предложений связи — не больше.
pub const SUGGEST_LIMIT: usize = 10;

/// Связать беседы `a` и `b` (адреса бесед; сообщение в адресе не учитывается). `false` — уже связаны или это одна беседа.
pub fn link(cache: &Cache, a: &Address, b: &Address) -> Result<bool> {
    Ok(cache.conv_link(&a.of_conversation().to_string(), &b.of_conversation().to_string())?)
}

/// Развязать. `false` — связи не было.
pub fn unlink(cache: &Cache, a: &Address, b: &Address) -> Result<bool> {
    Ok(cache.conv_unlink(&a.of_conversation().to_string(), &b.of_conversation().to_string())?)
}

/// Беседы, прямо связанные с `at`.
pub fn direct(cache: &Cache, at: &Address) -> Result<Vec<Address>> {
    Ok(cache.conv_links(&at.of_conversation().to_string())?.iter().filter_map(|s| Address::parse(s)).collect())
}

/// Беседа `at` и все связанные с ней (и через другие связанные), первой — `at`; не больше [`GROUP_LIMIT`].
pub fn group(cache: &Cache, at: &Address) -> Result<Vec<Address>> {
    let first = at.of_conversation();
    let mut seen: HashSet<Address> = HashSet::from([first.clone()]);
    let mut out = vec![first.clone()];
    let mut queue = VecDeque::from([first]);
    while let Some(a) = queue.pop_front() {
        for b in direct(cache, &a)? {
            if out.len() >= GROUP_LIMIT {
                return Ok(out);
            }
            if seen.insert(b.clone()) {
                out.push(b.clone());
                queue.push_back(b);
            }
        }
    }
    Ok(out)
}

/// Фильтр `беседа:` поиска со связанными: адреса бесед запроса и всех связанных с ними, без повторов.
pub fn scope(cache: &Cache, convs: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for c in convs {
        let group = match Address::parse(c) {
            Some(a) => group(cache, &a)?.iter().map(Address::to_string).collect(),
            None => vec![c.clone()],
        };
        for g in group {
            if !out.contains(&g) {
                out.push(g);
            }
        }
    }
    Ok(out)
}

/// Тема для сравнения: без «Re:», «Fwd:», «Ответ:» в начале, строчными, пробелы схлопнуты.
pub fn topic(title: &str) -> String {
    const PREFIXES: &[&str] = &["re:", "fw:", "fwd:", "aw:", "wg:", "ответ:", "отв:", "пересл:", "переслано:"];
    let mut t = title.trim().to_lowercase();
    while let Some(p) = PREFIXES.iter().find(|p| t.starts_with(**p)) {
        t = t[p.len()..].trim_start().to_owned();
    }
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Адреса людей беседы для сравнения: почта, телефоны и ключи участников каналов.
fn people_keys(at: &Address, people: &[Participant]) -> HashSet<String> {
    let chan = at.chan();
    people.iter().flat_map(|p| super::index::participant_addresses(&chan, p)).filter(|a| a.contains('@') || a.starts_with('+')).collect()
}

/// Беседа и люди по адресу: чат или почта.
fn read(chats: &Chats, mail: Option<&MailSource>, at: &Address) -> Result<Option<(Summary, Vec<Participant>)>> {
    if at.channel.0 == super::MAIL {
        let Some(mail) = mail else { return Ok(None) };
        return Ok(mail.conversation(at)?.map(|s| (s, mail.people(at).unwrap_or_default())));
    }
    Ok(chats.conversation(at)?.map(|s| (s, chats.people(at).unwrap_or_default())))
}

/// Предложить связи для беседы `at`: беседы других каналов с той же темой или двумя и больше общими людьми (почта,
/// телефон), кроме уже связанных; лучшие и новые первыми, не больше [`SUGGEST_LIMIT`]. Почтовые беседы — при
/// `mail` (по теме, среди писем профиля).
pub fn suggest(cache: &Cache, mail: Option<&MailSource>, at: &Address) -> Result<Vec<Address>> {
    let chats = Chats::new(cache.clone());
    let Some((me, people)) = read(&chats, mail, at)? else { return Ok(Vec::new()) };
    let (title, mine) = (topic(&me.conversation.title), people_keys(at, &people));
    let linked: HashSet<Address> = group(cache, at)?.into_iter().collect();
    let score = |s: &Summary, people: &[Participant]| -> usize {
        let same = usize::from(!title.is_empty() && topic(&s.conversation.title) == title) * 2;
        let common = people_keys(&s.address, people).intersection(&mine).count();
        same + if common >= 2 { common } else { 0 }
    };
    let mut found: Vec<(usize, i64, Address)> = Vec::new();
    for s in chats.list(0, usize::MAX)? {
        if s.address.channel == at.channel || linked.contains(&s.address) {
            continue;
        }
        let people = chats.people(&s.address)?;
        let n = score(&s, &people);
        if n >= 2 {
            found.push((n, s.last, s.address));
        }
    }
    if let Some(mail) = mail.filter(|_| at.channel.0 != super::MAIL && !title.is_empty()) {
        let want = |r: &postman_cache::EmailRow, _: &postman_cache::Recipients| topic(&r.subject) == title;
        for i in super::smart::mail_items(mail, cache, &want)? {
            if linked.contains(&i.summary.address) {
                continue;
            }
            let people = mail.people(&i.summary.address)?;
            found.push((score(&i.summary, &people), i.summary.last, i.summary.address));
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    Ok(found.into_iter().take(SUGGEST_LIMIT).map(|(.., a)| a).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{AccountId, ChannelEvent, ChannelId, ChannelStore, Conversation, ConversationId, Member, ParticipantId, Role};

    fn at(channel: &str, conv: &str) -> Address {
        Address::conversation(&ChannelId::new(channel), &AccountId::new("me@example.test"), &ConversationId::new(conv))
    }

    /// Связь — в обе стороны и через одну; фильтр поиска — со всеми связанными; развязка — снимает.
    #[test]
    fn groups_are_transitive() {
        let cache = Cache::open_in_memory().unwrap();
        let (mail, tg, mx) = (at("mail", "t1"), at("telegram", "g"), at("matrix", "!r"));
        assert!(link(&cache, &mail, &tg).unwrap());
        assert!(link(&cache, &Address { message: Some(crate::channel::MessageId::new("5")), ..tg.clone() }, &mx).unwrap());
        assert_eq!(group(&cache, &mx).unwrap(), [mx.clone(), tg.clone(), mail.clone()]);
        assert_eq!(scope(&cache, &[mail.to_string()]).unwrap().len(), 3);
        assert!(unlink(&cache, &tg, &mail).unwrap());
        assert_eq!(group(&cache, &mail).unwrap(), [mail]);
        assert_eq!(topic("Re: FWD:  Отчёт  за неделю"), "отчёт за неделю");
    }

    /// Предложение: беседа другого канала с той же темой и общими людьми; связанная и того же канала — нет.
    #[test]
    fn suggestions_by_topic_and_people() {
        let cache = Cache::open_in_memory().unwrap();
        let open = |ch: &str| ChannelStore::open(cache.clone(), &ChannelId::new(ch), &AccountId::new("me@example.test"), "Я").unwrap();
        let person =
            |id: &str, mail: &str| Participant { id: ParticipantId::new(id), name: id.into(), emails: vec![mail.into()], ..Participant::default() };
        let conv = |ch: &str, id: &str, title: &str| {
            let s = open(ch);
            s.apply(vec![
                ChannelEvent::People(vec![person("a", "anna@example.test"), person("b", "boris@example.test")]),
                ChannelEvent::Conversations(vec![Conversation { id: ConversationId::new(id), title: title.into(), ..Conversation::default() }]),
                ChannelEvent::Members {
                    conversation: ConversationId::new(id),
                    changed: ["a", "b"].map(|p| Member { id: ParticipantId::new(p), role: Role::Member }).to_vec(),
                    left: vec![],
                },
            ])
            .unwrap();
        };
        conv("telegram", "g", "Проект «Север»");
        conv("matrix", "!r", "Re: проект «север»");
        conv("matrix", "!other", "Другое");
        conv("telegram", "g2", "Проект «Север»");
        let got = suggest(&cache, None, &at("telegram", "g")).unwrap();
        assert_eq!(got[0], at("matrix", "!r"));
        assert!(got.contains(&at("matrix", "!other")), "двое общих людей — тоже предложение");
        assert!(!got.contains(&at("telegram", "g2")), "тот же канал — не предлагается");
        link(&cache, &at("telegram", "g"), &at("matrix", "!r")).unwrap();
        assert!(!suggest(&cache, None, &at("telegram", "g")).unwrap().contains(&at("matrix", "!r")));
    }
}
