//! Календари и адресные книги в домашнем наборе: имя, цвет, компоненты, признаки изменений (ctag, sync-token),
//! права текущего пользователя.

use crate::multistatus::{Multistatus, Response, same_href};
use crate::xml::{self, CALDAV, CARDDAV, CS, DAV, ICAL};
use crate::{Client, Error, Result};

/// Календарь или адресная книга.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Collection {
    pub href: String,
    /// `displayname`; нет — последний сегмент пути.
    pub name: String,
    pub description: Option<String>,
    /// `#RRGGBB` или `#RRGGBBAA`, как у сервера (Apple `calendar-color`).
    pub color: Option<String>,
    /// Компоненты календаря (`VEVENT`, `VTODO`); пусто — не объявлены (RFC 4791 §5.2.3: любые). У адресной
    /// книги — пусто.
    pub components: Vec<String>,
    pub ctag: Option<String>,
    pub sync_token: Option<String>,
    /// Можно писать (`write`, `write-content`, `bind` или `all`); права не объявлены — считаем, что можно.
    pub writable: bool,
    /// Календарь не делает занятым (`schedule-calendar-transp` = `transparent`, RFC 6638 §9.1).
    pub transparent: bool,
}

impl Collection {
    /// Есть ли в календаре компонент (`VEVENT`, `VTODO`); не объявлены — есть.
    pub fn supports(&self, component: &str) -> bool {
        self.components.is_empty() || self.components.iter().any(|c| c.eq_ignore_ascii_case(component))
    }
}

/// Признаки изменений коллекции: сменились — пора синхронизировать.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Tokens {
    pub ctag: Option<String>,
    pub sync_token: Option<String>,
}

const CALENDAR_PROPS: &[&str] = &[
    "d:resourcetype",
    "d:displayname",
    "a:calendar-color",
    "c:calendar-description",
    "c:supported-calendar-component-set",
    "c:schedule-calendar-transp",
    "cs:getctag",
    "d:sync-token",
    "d:current-user-privilege-set",
];

const ADDRESSBOOK_PROPS: &[&str] =
    &["d:resourcetype", "d:displayname", "r:addressbook-description", "cs:getctag", "d:sync-token", "d:current-user-privilege-set"];

impl Client {
    /// Календари домашнего набора (PROPFIND Depth 1); ящики scheduling и прочие коллекции — не календари.
    pub async fn calendars(&self, home: &str) -> Result<Vec<Collection>> {
        let ms = self.multistatus("PROPFIND", home, Some("1"), xml::propfind(CALENDAR_PROPS)).await?;
        Ok(calendars(&ms, home))
    }

    /// Адресные книги домашнего набора.
    pub async fn address_books(&self, home: &str) -> Result<Vec<Collection>> {
        let ms = self.multistatus("PROPFIND", home, Some("1"), xml::propfind(ADDRESSBOOK_PROPS)).await?;
        Ok(address_books(&ms, home))
    }

    /// ctag и sync-token одной коллекции (Depth 0): дешёвая проверка «изменилось ли что-то».
    pub async fn tokens(&self, collection: &str) -> Result<Tokens> {
        let ms = self.multistatus("PROPFIND", collection, Some("0"), xml::propfind(&["cs:getctag", "d:sync-token"])).await?;
        let r = ms.responses.first().ok_or_else(|| Error::Protocol("empty PROPFIND response".into()))?;
        Ok(Tokens { ctag: r.text(CS, "getctag"), sync_token: r.text(DAV, "sync-token") })
    }
}

fn has_type(r: &Response, ns: &str, name: &str) -> bool {
    r.prop(DAV, "resourcetype").is_some_and(|t| t.child(ns, name).is_some())
}

/// Календари из ответа PROPFIND домашнего набора.
pub fn calendars(ms: &Multistatus, home: &str) -> Vec<Collection> {
    ms.responses
        .iter()
        .filter(|r| r.ok() && !same_href(&r.href, home) && has_type(r, CALDAV, "calendar"))
        .map(|r| {
            let mut c = common(r);
            c.description = r.text(CALDAV, "calendar-description");
            c.color = r.text(ICAL, "calendar-color");
            c.components = r
                .prop(CALDAV, "supported-calendar-component-set")
                .map(|s| s.all(CALDAV, "comp").filter_map(|c| c.attr("name")).map(str::to_ascii_uppercase).collect())
                .unwrap_or_default();
            c.transparent = r.prop(CALDAV, "schedule-calendar-transp").is_some_and(|t| t.child(CALDAV, "transparent").is_some());
            c
        })
        .collect()
}

/// Адресные книги из ответа PROPFIND домашнего набора.
pub fn address_books(ms: &Multistatus, home: &str) -> Vec<Collection> {
    ms.responses
        .iter()
        .filter(|r| r.ok() && !same_href(&r.href, home) && has_type(r, CARDDAV, "addressbook"))
        .map(|r| {
            let mut c = common(r);
            c.description = r.text(CARDDAV, "addressbook-description");
            c
        })
        .collect()
}

fn common(r: &Response) -> Collection {
    let fallback = || {
        let path = r.href.trim_end_matches('/');
        let last = path.rsplit('/').next().unwrap_or(path);
        percent_encoding::percent_decode_str(last).decode_utf8_lossy().into_owned()
    };
    let writable = match r.prop(DAV, "current-user-privilege-set") {
        Some(set) => set
            .all(DAV, "privilege")
            .any(|p| p.children.iter().any(|g| g.ns == DAV && matches!(g.name.as_str(), "all" | "write" | "write-content" | "bind"))),
        None => true,
    };
    Collection {
        href: r.href.clone(),
        name: r.text(DAV, "displayname").unwrap_or_else(fallback),
        ctag: r.text(CS, "getctag"),
        sync_token: r.text(DAV, "sync-token"),
        writable,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stand_calendar_home_has_one_calendar() {
        let ms = crate::multistatus::parse(include_bytes!("../tests/fixtures/calendars.xml")).unwrap();
        let list = calendars(&ms, "/dav/cal/user1%40itt.test/");
        assert_eq!(list.len(), 1, "сам набор — не календарь");
        let c = &list[0];
        assert_eq!(c.href, "/dav/cal/user1%40itt.test/default/");
        assert_eq!(c.name, "Stalwart Calendar (user1@itt.test)");
        assert!(c.supports("VEVENT") && c.supports("vtodo") && !c.supports("VX"));
        assert_eq!(c.ctag.as_deref(), Some("\"992\""));
        assert_eq!(c.sync_token.as_deref(), Some("urn:stalwart:davsync:3e0"));
        assert_eq!((c.color.as_deref(), c.description.as_deref()), (None, None), "404 — нет");
        assert!(c.writable && !c.transparent);
    }

    #[test]
    fn stand_addressbook_home_has_one_book() {
        let ms = crate::multistatus::parse(include_bytes!("../tests/fixtures/addressbooks.xml")).unwrap();
        let list = address_books(&ms, "/dav/card/user1%40itt.test/");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].href, "/dav/card/user1%40itt.test/default/");
        assert_eq!(list[0].name, "Stalwart Address Book (user1@itt.test)");
        assert!(list[0].components.is_empty() && list[0].writable);
        // Календари в наборе адресных книг не ищутся.
        assert!(calendars(&ms, "/dav/card/user1%40itt.test/").is_empty());
    }

    #[test]
    fn read_only_calendar_with_color_and_no_name() {
        let doc = br##"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:a="http://apple.com/ns/ical/">
<d:response><d:href>/cal/u/shared%20one/</d:href><d:propstat><d:prop>
<d:resourcetype><d:collection/><c:calendar/></d:resourcetype><a:calendar-color>#3366FFFF</a:calendar-color>
<c:supported-calendar-component-set><c:comp name="vtodo"/></c:supported-calendar-component-set>
<c:schedule-calendar-transp><c:transparent/></c:schedule-calendar-transp>
<d:current-user-privilege-set><d:privilege><d:read/></d:privilege></d:current-user-privilege-set>
</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:response><d:href>/cal/u/inbox/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/><c:schedule-inbox/></d:resourcetype></d:prop>
<d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"##;
        let ms = crate::multistatus::parse(doc).unwrap();
        let list = calendars(&ms, "/cal/u/");
        assert_eq!(list.len(), 1, "inbox — не календарь");
        let c = &list[0];
        assert_eq!((c.name.as_str(), c.color.as_deref()), ("shared one", Some("#3366FFFF")));
        assert_eq!(c.components, ["VTODO"]);
        assert!(!c.supports("VEVENT") && !c.writable && c.transparent);
    }
}
