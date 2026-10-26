//! Поиск (RFC 6764, RFC 4791 §6.2, RFC 6352 §7.1, RFC 6638 §2): контекст DAV — `.well-known/caldav` и
//! `/carddav` (или адрес, который дал пользователь), `current-user-principal`, домашние наборы календарей и
//! адресных книг, ящики scheduling, адреса участника.

use crate::multistatus::{Multistatus, same_href};
use crate::xml::{self, CALDAV, CARDDAV, DAV};
use crate::{Client, Error, Result};

/// Что нашлось у учётной записи.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Discovery {
    /// Путь принципала (`/dav/pal/user1%40itt.test/`).
    pub principal: String,
    pub name: Option<String>,
    /// Домашние наборы календарей (обычно один); пусто — CalDAV нет.
    pub calendar_home: Vec<String>,
    /// Домашние наборы адресных книг; пусто — CardDAV нет.
    pub addressbook_home: Vec<String>,
    /// Входящие планирования (RFC 6638 §2.2): приглашения и ответы, положенные сервером.
    pub schedule_inbox: Option<String>,
    /// Исходящие планирования (RFC 6638 §2.1): POST VFREEBUSY — занятость.
    pub schedule_outbox: Option<String>,
    /// Календарь, куда сервер кладёт приглашения (RFC 6638 §9.2); не объявлен — первый календарь с VEVENT.
    pub schedule_default_calendar: Option<String>,
    /// Адреса участника (`mailto:user1@itt.test`), первым — основной (RFC 6638 §2.4.1).
    pub addresses: Vec<String>,
}

impl Discovery {
    /// Основной адрес участника без `mailto:`.
    pub fn email(&self) -> Option<&str> {
        self.addresses.iter().find_map(|a| strip_mailto(a))
    }

    /// Есть ли scheduling (RFC 6638): без outbox — ни занятости, ни рассылки сервером.
    pub fn has_scheduling(&self) -> bool {
        self.schedule_outbox.is_some()
    }
}

/// `mailto:a@b` → `a@b` (регистр схемы — любой).
pub fn strip_mailto(addr: &str) -> Option<&str> {
    let (scheme, rest) = addr.split_once(':')?;
    scheme.eq_ignore_ascii_case("mailto").then_some(rest).filter(|r| !r.is_empty())
}

const PRINCIPAL_PROPS: &[&str] = &[
    "d:displayname",
    "c:calendar-home-set",
    "r:addressbook-home-set",
    "c:schedule-inbox-URL",
    "c:schedule-outbox-URL",
    "c:schedule-default-calendar-URL",
    "c:calendar-user-address-set",
];

impl Client {
    /// Найти принципала и его наборы. Порядок: путь из адреса учётной записи (если он не корень), затем
    /// `.well-known/caldav`, `.well-known/carddav`, корень сервера.
    pub async fn discover(&self) -> Result<Discovery> {
        let principal = self.current_user_principal().await?;
        let mut d = self.principal(&principal).await?;
        if d.addressbook_home.is_empty() {
            // CardDAV может жить у другого принципала (другой контекст): спросить его по `.well-known/carddav`.
            if let Ok(other) = self.principal_at("/.well-known/carddav").await
                && !same_href(&other, &principal)
                && let Ok(ms) = self.multistatus("PROPFIND", &other, Some("0"), xml::propfind(&["r:addressbook-home-set"])).await
            {
                d.addressbook_home =
                    ms.responses.first().and_then(|r| r.prop(CARDDAV, "addressbook-home-set")).map(|p| p.hrefs()).unwrap_or_default();
            }
        }
        tracing::debug!(
            calendars = !d.calendar_home.is_empty(),
            contacts = !d.addressbook_home.is_empty(),
            scheduling = d.has_scheduling(),
            "dav discovery"
        );
        Ok(d)
    }

    /// Свойства принципала по пути: домашние наборы, ящики планирования, адреса участника — свои или коллеги
    /// (адрес коллеги в `addresses` — проверка, что путь тот).
    pub async fn principal(&self, href: &str) -> Result<Discovery> {
        let ms = self.multistatus("PROPFIND", href, Some("0"), xml::propfind(PRINCIPAL_PROPS)).await?;
        from_principal(&ms, href)
    }

    /// `current-user-principal` по очереди мест поиска.
    async fn current_user_principal(&self) -> Result<String> {
        let mut places: Vec<String> = Vec::new();
        let path = self.base().path();
        if path != "/" && !path.is_empty() {
            places.push(path.to_owned());
        }
        places.extend(["/.well-known/caldav", "/.well-known/carddav", "/"].map(str::to_owned));
        let mut last = Error::Protocol("no current-user-principal".into());
        for place in places {
            match self.principal_at(&place).await {
                Ok(p) => return Ok(p),
                // Вход не принят — дальше искать незачем.
                Err(Error::Auth) => return Err(Error::Auth),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    async fn principal_at(&self, href: &str) -> Result<String> {
        let ms = self.multistatus("PROPFIND", href, Some("0"), xml::propfind(&["d:current-user-principal"])).await?;
        ms.responses
            .iter()
            .find_map(|r| r.prop(DAV, "current-user-principal").and_then(|p| p.href()).map(str::to_owned))
            .ok_or_else(|| Error::Protocol("no current-user-principal".into()))
    }
}

/// Свойства принципала из ответа PROPFIND (Depth 0).
pub fn from_principal(ms: &Multistatus, principal: &str) -> Result<Discovery> {
    let r = ms
        .responses
        .iter()
        .find(|r| same_href(&r.href, principal))
        .or(ms.responses.first())
        .ok_or_else(|| Error::Protocol("empty principal response".into()))?;
    let one = |ns: &str, name: &str| r.prop(ns, name).and_then(|p| p.href()).map(str::to_owned);
    let many = |ns: &str, name: &str| r.prop(ns, name).map(|p| p.hrefs()).unwrap_or_default();
    Ok(Discovery {
        principal: r.href.clone(),
        name: r.text(DAV, "displayname"),
        calendar_home: many(CALDAV, "calendar-home-set"),
        addressbook_home: many(CARDDAV, "addressbook-home-set"),
        schedule_inbox: one(CALDAV, "schedule-inbox-URL"),
        schedule_outbox: one(CALDAV, "schedule-outbox-URL"),
        schedule_default_calendar: one(CALDAV, "schedule-default-calendar-URL"),
        addresses: many(CALDAV, "calendar-user-address-set"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn principal_and_sets_of_the_stand() {
        let cup = crate::multistatus::parse(include_bytes!("../tests/fixtures/current-user-principal.xml")).unwrap();
        let p = cup.responses[0].prop(DAV, "current-user-principal").and_then(|p| p.href()).unwrap();
        assert_eq!(p, "/dav/pal/user1%40itt.test/");

        let ms = crate::multistatus::parse(include_bytes!("../tests/fixtures/principal.xml")).unwrap();
        let d = from_principal(&ms, p).unwrap();
        assert_eq!(d.principal, "/dav/pal/user1%40itt.test/");
        assert_eq!(d.name.as_deref(), Some("Test user1"));
        assert_eq!(d.calendar_home, ["/dav/cal/user1%40itt.test/"]);
        assert_eq!(d.addressbook_home, ["/dav/card/user1%40itt.test/"]);
        assert_eq!(d.schedule_inbox.as_deref(), Some("/dav/itip/user1%40itt.test/inbox/"));
        assert_eq!(d.schedule_outbox.as_deref(), Some("/dav/itip/user1%40itt.test/outbox/"));
        // У Stalwart `schedule-default-calendar-URL` — 404.
        assert_eq!(d.schedule_default_calendar, None);
        assert_eq!(d.email(), Some("user1@itt.test"));
        assert!(d.has_scheduling());
    }

    #[test]
    fn mailto_is_stripped_case_insensitively() {
        assert_eq!(strip_mailto("MAILTO:a@b"), Some("a@b"));
        assert_eq!(strip_mailto("urn:uuid:1"), None);
        assert_eq!(strip_mailto("mailto:"), None);
    }
}
