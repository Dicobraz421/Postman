//! Участник канала = контакт (ADR 0012 п. 4, решение владельца №2): один человек во всех каналах — для умной папки
//! «Человек во всех каналах» и `от:` в поиске.
//!
//! - **Автоматически** ([`auto`]): телефон участника ↔ телефоны и мобильные контакта, почта участника ↔ почта
//!   контакта, ссылка канала на человека ([`super::Links::person_url`]) ↔ адреса в мессенджерах карточки
//!   (`ContactCard::services`). Только в базе профиля (`person_link`): автоматические совпадения на сервер не пишутся.
//! - **Вручную** ([`link`]): «Связать с контактом» — строка в базе и, у личного контакта, ссылка канала в его карточку
//!   на сервере (JSContact `onlineServices`, vCard `IMPP`, адреса чата Exchange — команда `SaveContact`); так связь
//!   есть на всех компьютерах. Контакт справочника организации (GAL) сервер менять не даёт — связь только в базе.
//!   Ссылки на человека у канала нет — тоже только в базе.
//!
//! Ручная связь сильнее автоматической: [`auto`] её не трогает. Чтение и запись базы — I/O: из фона.

use std::collections::HashSet;

use postman_cache::{Cache, PersonKey};
use serde::{Deserialize, Serialize};

use super::index::phone_key;
use super::{ChanAccount, ChannelError, ChannelStore, Links, Participant, ParticipantId};
use crate::model::{ContactCard, Principal};
use crate::sync::Command;

type Result<T> = std::result::Result<T, ChannelError>;

/// Как связаны участник и контакт.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum How {
    Phone,
    Email,
    /// Ссылка канала на человека — в адресах в мессенджерах карточки.
    Service,
    /// «Связать с контактом».
    Manual,
}

/// Связь участника с контактом.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonLink {
    /// id карточки личного контакта или записи справочника.
    pub contact: String,
    pub how: How,
    /// Контакт справочника организации (GAL): связь только в базе.
    pub directory: bool,
}

#[derive(Serialize, Deserialize)]
enum Envelope {
    V1(PersonLink),
}

/// С кем связать вручную.
#[derive(Debug, Clone, Copy)]
pub enum Contact<'a> {
    /// Личный контакт: связь — и в карточку на сервере.
    Personal(&'a ContactCard),
    /// Запись справочника организации (`Principal::id`): только в базе.
    Directory(&'a str),
}

fn key<'a>(chan: &'a ChanAccount, id: &'a ParticipantId) -> PersonKey<'a> {
    (chan.channel.as_str(), chan.account.as_str(), id.as_str())
}

fn enc(l: &PersonLink) -> Result<Vec<u8>> {
    Ok(postman_cache::encode(&Envelope::V1(l.clone()))?)
}

fn dec(b: &[u8]) -> Result<PersonLink> {
    let Envelope::V1(l) = postman_cache::decode(b)?;
    Ok(l)
}

/// Связь участника; нет — `None`.
pub fn get(cache: &Cache, chan: &ChanAccount, id: &ParticipantId) -> Result<Option<PersonLink>> {
    cache.person_link(&chan.channel.0, &chan.account.0, &id.0)?.map(|v| dec(&v)).transpose()
}

/// Все связи профиля.
pub fn all(cache: &Cache) -> Result<Vec<(ChanAccount, ParticipantId, PersonLink)>> {
    let mut out = Vec::new();
    for ((c, a, p), v) in cache.person_links()? {
        match dec(&v) {
            Ok(l) => out.push((ChanAccount::new(c, a), ParticipantId(p), l)),
            Err(e) => tracing::warn!("person link: {e}"),
        }
    }
    Ok(out)
}

/// Участники всех каналов, связанные с контактом `contact`.
pub fn of_contact(cache: &Cache, contact: &str) -> Result<Vec<(ChanAccount, ParticipantId)>> {
    Ok(all(cache)?.into_iter().filter(|(.., l)| l.contact == contact).map(|(c, p, _)| (c, p)).collect())
}

/// Адрес для сравнения ссылок: без регистра и `/` в конце.
fn url_key(s: &str) -> String {
    s.trim().trim_end_matches('/').to_lowercase()
}

/// Как участник совпадает с карточкой: ссылка канала, почта, телефон (по силе); нет — `None`.
pub fn matches(links: &Links, chan: &ChanAccount, p: &Participant, card: &ContactCard) -> Option<How> {
    if let Some(url) = links.person_url(chan, p).map(|u| url_key(&u))
        && card.services.iter().any(|s| url_key(s) == url)
    {
        return Some(How::Service);
    }
    let emails: HashSet<String> = p.emails.iter().map(|e| e.trim().to_lowercase()).filter(|e| !e.is_empty()).collect();
    if card.emails.iter().any(|e| emails.contains(&e.trim().to_lowercase())) {
        return Some(How::Email);
    }
    let phone = p.phone.as_deref().map(phone_key).filter(|k| k.len() > 4)?;
    card.phones.iter().chain(&card.mobile).any(|t| same_phone(&phone_key(t), &phone)).then_some(How::Phone)
}

/// Телефоны совпадают: равны или один — тот же номер без кода страны (последние 10 цифр).
fn same_phone(a: &str, b: &str) -> bool {
    let tail = |s: &str| s.trim_start_matches('+').chars().rev().take(10).collect::<String>();
    !a.is_empty() && (a == b || (a.len() >= 10 && b.len() >= 10 && tail(a) == tail(b)))
}

/// Совпадение с записью справочника: почта, телефон, мобильный.
fn matches_directory(p: &Participant, d: &Principal) -> Option<How> {
    if let Some(mail) = d.email.as_deref().map(|e| e.trim().to_lowercase()).filter(|e| !e.is_empty())
        && p.emails.iter().any(|e| e.trim().to_lowercase() == mail)
    {
        return Some(How::Email);
    }
    let phone = p.phone.as_deref().map(phone_key).filter(|k| k.len() > 4)?;
    [&d.phone, &d.mobile].into_iter().any(|t| same_phone(&phone_key(t), &phone)).then_some(How::Phone)
}

/// Связать участников всех каналов с контактами автоматически: личные карточки `cards` (ссылка канала, почта,
/// телефон) и справочник `directory` (почта, телефон). Прежние автоматические связи пересчитываются, ручные остаются.
/// Ответ — сколько связей стало.
pub fn auto(cache: &Cache, links: &Links, cards: &[ContactCard], directory: &[Principal]) -> Result<usize> {
    let manual: HashSet<(ChanAccount, ParticipantId)> =
        all(cache)?.into_iter().filter(|(.., l)| l.how == How::Manual).map(|(c, p, _)| (c, p)).collect();
    let mut rows: Vec<(ChanAccount, ParticipantId, Option<PersonLink>)> = Vec::new();
    for store in ChannelStore::all(cache)? {
        let chan = store.chan();
        for p in store.participants()? {
            if manual.contains(&(chan.clone(), p.id.clone())) {
                continue;
            }
            let personal = cards.iter().filter(|c| !c.group).find_map(|c| matches(links, &chan, &p, c).map(|how| (c.id.clone(), how, false)));
            let found = personal.or_else(|| directory.iter().find_map(|d| matches_directory(&p, d).map(|how| (d.id.clone(), how, true))));
            let link = found.map(|(contact, how, directory)| PersonLink { contact, how, directory });
            rows.push((chan.clone(), p.id.clone(), link));
        }
    }
    let values: Vec<Option<Vec<u8>>> = rows.iter().map(|(.., l)| l.as_ref().map(enc).transpose()).collect::<Result<_>>()?;
    let set: Vec<(PersonKey, Option<&[u8]>)> = rows.iter().zip(&values).map(|((c, p, _), v)| (key(c, p), v.as_deref())).collect();
    cache.person_link_set(&set)?;
    Ok(values.iter().filter(|v| v.is_some()).count())
}

/// «Связать с контактом»: строка в базе; у личного контакта, если у канала есть ссылка на человека, — команда записать
/// её в карточку на сервере (её ещё нет в адресах в мессенджерах; `ticket` — номер ответа `ContactSaved`). Справочник —
/// только база.
pub fn link(cache: &Cache, links: &Links, chan: &ChanAccount, p: &Participant, contact: Contact, ticket: u64) -> Result<Option<Command>> {
    let (id, directory) = match contact {
        Contact::Personal(card) => (card.id.clone(), false),
        Contact::Directory(id) => (id.to_owned(), true),
    };
    let row = enc(&PersonLink { contact: id, how: How::Manual, directory })?;
    cache.person_link_set(&[(key(chan, &p.id), Some(&row))])?;
    let Contact::Personal(card) = contact else { return Ok(None) };
    let Some(url) = links.person_url(chan, p) else { return Ok(None) };
    if card.services.iter().any(|s| url_key(s) == url_key(&url)) {
        return Ok(None);
    }
    let mut new = card.clone();
    new.services.push(url);
    Ok(Some(Command::SaveContact { card: Box::new(new), old: Some(Box::new(card.clone())), ticket }))
}

/// «Это другой человек»: ручная связь снимается; у личного контакта — команда убрать ссылку канала из карточки на
/// сервере, если она там есть.
pub fn unlink(cache: &Cache, links: &Links, chan: &ChanAccount, p: &Participant, card: Option<&ContactCard>, ticket: u64) -> Result<Option<Command>> {
    cache.person_link_set(&[(key(chan, &p.id), None)])?;
    let (Some(card), Some(url)) = (card, links.person_url(chan, p)) else { return Ok(None) };
    if !card.services.iter().any(|s| url_key(s) == url_key(&url)) {
        return Ok(None);
    }
    let mut new = card.clone();
    new.services.retain(|s| url_key(s) != url_key(&url));
    Ok(Some(Command::SaveContact { card: Box::new(new), old: Some(Box::new(card.clone())), ticket }))
}

/// Ключи поиска (`index::author_key`) всех участников, связанных с тем же контактом, что и участники `keys`.
pub fn same_people(cache: &Cache, keys: &[String]) -> Result<Vec<String>> {
    let links = all(cache)?;
    let key_of = |c: &ChanAccount, p: &ParticipantId| super::index::author_key(c, p);
    let contacts: HashSet<&str> = links.iter().filter(|(c, p, _)| keys.contains(&key_of(c, p))).map(|(.., l)| l.contact.as_str()).collect();
    Ok(links.iter().filter(|(.., l)| contacts.contains(l.contact.as_str())).map(|(c, p, _)| key_of(c, p)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{AccountId, ChannelEvent, ChannelId};

    fn card(id: &str) -> ContactCard {
        ContactCard { id: id.into(), name: id.into(), ..ContactCard::default() }
    }

    /// Автоматически — по почте, телефону без кода страны и справочнику; ручная связь остаётся и пишется в карточку
    /// только при ссылке канала; справочник — только база; люди одного контакта — вместе.
    #[test]
    fn people_link_to_contacts() {
        let cache = Cache::open_in_memory().unwrap();
        let store = ChannelStore::open(cache.clone(), &ChannelId::new("demo"), &AccountId::new("me@example.test"), "Я").unwrap();
        let p = |id: &str, phone: Option<&str>, mail: Option<&str>| Participant {
            id: ParticipantId::new(id),
            name: id.into(),
            phone: phone.map(Into::into),
            emails: mail.into_iter().map(Into::into).collect(),
            ..Participant::default()
        };
        let people = vec![p("a", None, Some("Anna@Example.test")), p("b", Some("+7 900 000-00-01"), None), p("c", Some("+7 900 000-00-02"), None)];
        store.apply(vec![ChannelEvent::People(people.clone())]).unwrap();
        let anna = ContactCard { emails: vec!["anna@example.test".into()], ..card("card-anna") };
        let boris = ContactCard { mobile: vec!["8 (900) 000-00-01".into()], ..card("card-boris") };
        let gal = Principal { id: "gal-c".into(), phone: "+79000000002".into(), ..Principal::default() };
        let links = Links::default();
        assert_eq!(auto(&cache, &links, &[anna.clone(), boris], &[gal]).unwrap(), 3);
        let chan = store.chan();
        assert_eq!(get(&cache, &chan, &ParticipantId::new("a")).unwrap().unwrap().how, How::Email);
        assert_eq!(get(&cache, &chan, &ParticipantId::new("b")).unwrap().unwrap().how, How::Phone);
        assert!(get(&cache, &chan, &ParticipantId::new("c")).unwrap().unwrap().directory);

        // Вручную: «c» — это Анна. Ссылки на человека у канала нет — только база; справочник — тоже.
        assert!(link(&cache, &links, &chan, &people[2], Contact::Personal(&anna), 1).unwrap().is_none());
        assert!(link(&cache, &links, &chan, &people[1], Contact::Directory("gal-b"), 2).unwrap().is_none());
        assert_eq!(auto(&cache, &links, std::slice::from_ref(&anna), &[]).unwrap(), 1, "ручные не пересчитываются");
        assert_eq!(get(&cache, &chan, &ParticipantId::new("c")).unwrap().unwrap().how, How::Manual);
        let mut same = same_people(&cache, &[super::super::index::author_key(&chan, &ParticipantId::new("a"))]).unwrap();
        same.sort();
        assert_eq!(same, ["demo:me@example.test:a", "demo:me@example.test:c"]);
        assert_eq!(of_contact(&cache, "card-anna").unwrap().len(), 2);
        unlink(&cache, &links, &chan, &people[2], Some(&anna), 3).unwrap();
        assert!(get(&cache, &chan, &ParticipantId::new("c")).unwrap().is_none());
        store.forget().unwrap();
        assert!(all(&cache).unwrap().is_empty(), "«Выйти» снимает связи учётки");
    }

    #[test]
    fn phones_compare_without_country_code() {
        assert!(same_phone(&phone_key("8 (900) 123-45-67"), &phone_key("+7 900 123 45 67")));
        assert!(!same_phone(&phone_key("123"), &phone_key("+7 900 123 45 67")));
    }
}
