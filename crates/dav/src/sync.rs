//! Синхронизация коллекции: `sync-collection` (RFC 6578) — что изменилось после токена; без него — список ETag
//! (PROPFIND Depth 1) и сверка с прошлым (`diff`). Сами ресурсы — `calendar-multiget` (RFC 4791 §7.9) и
//! `addressbook-multiget` (RFC 6352 §8.7) пачками; события окна — `calendar-query` с `time-range`.

use postman_calendar::Span;

use crate::multistatus::{Multistatus, same_href};
use crate::xml::{self, CALDAV, CARDDAV, DAV, escape};
use crate::{Client, Error, Result};

/// Ресурсов в одном multiget: тело ответа — до единиц МиБ.
pub const MULTIGET_BATCH: usize = 100;

/// Ресурс и его ETag (без тела).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub href: String,
    pub etag: String,
}

/// Ресурс с телом.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resource {
    pub href: String,
    pub etag: Option<String>,
    /// Schedule-Tag (RFC 6638 §3.2) — у ресурсов планирования.
    pub schedule_tag: Option<String>,
    /// `.ics` или `.vcf` как есть: разбор — в воркере.
    pub data: String,
}

/// Изменения после токена.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Changes {
    /// Новые и изменённые.
    pub changed: Vec<Item>,
    /// Удалённые (404 в ответе).
    pub removed: Vec<String>,
    /// Новый токен — сохранить после применения изменений.
    pub token: String,
    /// Ответ — полный список коллекции, а не изменения: токена не было или сервер его не узнал. Чего нет в
    /// `changed`, того на сервере нет (сверка `diff`). Stalwart на неизвестный токен отвечает полным списком
    /// без `DAV:valid-sync-token` — его узнаём по самой коллекции в списке (`docs/STALWART.md`).
    pub full: bool,
    /// Сервер отдал не всё (507 у коллекции, RFC 6578 §3.6): повторить с новым токеном.
    pub truncated: bool,
}

/// Итог multiget.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Fetched {
    pub resources: Vec<Resource>,
    /// Пути, которых на сервере уже нет (404).
    pub missing: Vec<String>,
}

#[derive(Clone, Copy)]
enum Data {
    Calendar,
    Address,
}

impl Data {
    fn report(self) -> &'static str {
        match self {
            Data::Calendar => "c:calendar-multiget",
            Data::Address => "r:addressbook-multiget",
        }
    }

    fn prop(self) -> &'static str {
        match self {
            Data::Calendar => "<d:getetag/><c:calendar-data/><c:schedule-tag/>",
            Data::Address => "<d:getetag/><r:address-data/>",
        }
    }

    fn element(self) -> (&'static str, &'static str) {
        match self {
            Data::Calendar => (CALDAV, "calendar-data"),
            Data::Address => (CARDDAV, "address-data"),
        }
    }
}

impl Client {
    /// `sync-collection`: без токена — полный список (первая синхронизация), с токеном — изменения. Токен,
    /// которого сервер не знает, — `Error::SyncToken` (или полный список с `full`, как у Stalwart).
    pub async fn sync(&self, collection: &str, token: Option<&str>) -> Result<Changes> {
        let ms = self.multistatus("REPORT", collection, None, sync_body(token)).await?;
        changes(&ms, collection, token.is_none())
    }

    /// Список ETag коллекции (PROPFIND Depth 1) — синхронизация без `sync-collection`: сверить `diff` с прошлым.
    pub async fn etags(&self, collection: &str) -> Result<Vec<Item>> {
        let ms = self.multistatus("PROPFIND", collection, Some("1"), xml::propfind(&["d:getetag", "d:resourcetype"])).await?;
        Ok(items(&ms, collection))
    }

    /// Календарные ресурсы по путям (пачками по `MULTIGET_BATCH`).
    pub async fn calendar_multiget(&self, collection: &str, hrefs: &[String]) -> Result<Fetched> {
        self.multiget(collection, hrefs, Data::Calendar).await
    }

    /// Карточки по путям (пачками по `MULTIGET_BATCH`).
    pub async fn addressbook_multiget(&self, collection: &str, hrefs: &[String]) -> Result<Fetched> {
        self.multiget(collection, hrefs, Data::Address).await
    }

    async fn multiget(&self, collection: &str, hrefs: &[String], kind: Data) -> Result<Fetched> {
        let mut out = Fetched::default();
        for chunk in hrefs.chunks(MULTIGET_BATCH) {
            let ms = self.multistatus("REPORT", collection, Some("1"), multiget_body(kind, chunk)).await?;
            let part = fetched(&ms, kind);
            out.resources.extend(part.resources);
            out.missing.extend(part.missing);
        }
        Ok(out)
    }

    /// `calendar-query`: ресурсы с компонентом (`VEVENT`, `VTODO`), пересекающие окно (`None` — все). Повторы
    /// сервер проверяет сам (RFC 4791 §9.9): событие с повтором в окне — в ответе целиком.
    pub async fn calendar_query(&self, collection: &str, component: &str, range: Option<Span>) -> Result<Vec<Resource>> {
        let ms = self.multistatus("REPORT", collection, Some("1"), query_body(component, range, false)?).await?;
        Ok(fetched(&ms, Data::Calendar).resources)
    }

    /// `calendar-query` с развёрнутыми повторами (`expand`, RFC 4791 §9.6.5): каждый экземпляр в окне — свой VEVENT с
    /// `RECURRENCE-ID`, время — UTC (как у JMAP, где повторы разворачивает сервер).
    pub async fn calendar_expand(&self, collection: &str, component: &str, range: Span) -> Result<Vec<Resource>> {
        let ms = self.multistatus("REPORT", collection, Some("1"), query_body(component, Some(range), true)?).await?;
        Ok(fetched(&ms, Data::Calendar).resources)
    }
}

fn sync_body(token: Option<&str>) -> String {
    let mut s = xml::open("d:sync-collection");
    match token {
        Some(t) => s.push_str(&format!("<d:sync-token>{}</d:sync-token>", escape(t))),
        None => s.push_str("<d:sync-token/>"),
    }
    s.push_str("<d:sync-level>1</d:sync-level><d:prop><d:getetag/></d:prop></d:sync-collection>");
    s
}

fn multiget_body(kind: Data, hrefs: &[String]) -> String {
    let root = kind.report();
    let mut s = xml::open(root);
    s.push_str("<d:prop>");
    s.push_str(kind.prop());
    s.push_str("</d:prop>");
    for h in hrefs {
        s.push_str(&format!("<d:href>{}</d:href>", escape(h)));
    }
    s.push_str(&format!("</{root}>"));
    s
}

/// Время iCalendar в UTC: `20261023T080000Z`.
pub(crate) fn ical_utc(secs: i64) -> Result<String> {
    let t = jiff::Timestamp::from_second(secs).map_err(|e| Error::Protocol(format!("time: {e}")))?;
    Ok(t.strftime("%Y%m%dT%H%M%SZ").to_string())
}

fn query_body(component: &str, range: Option<Span>, expand: bool) -> Result<String> {
    if !component.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err(Error::Protocol("bad component name".into()));
    }
    let mut s = xml::open("c:calendar-query");
    match range.filter(|_| expand) {
        Some(r) => s.push_str(&format!(
            "<d:prop><d:getetag/><c:calendar-data><c:expand start=\"{}\" end=\"{}\"/></c:calendar-data><c:schedule-tag/></d:prop>",
            ical_utc(r.start)?,
            ical_utc(r.end)?
        )),
        None => s.push_str("<d:prop><d:getetag/><c:calendar-data/><c:schedule-tag/></d:prop>"),
    }
    s.push_str(&format!("<c:filter><c:comp-filter name=\"VCALENDAR\"><c:comp-filter name=\"{}\">", component.to_ascii_uppercase()));
    if let Some(r) = range {
        s.push_str(&format!("<c:time-range start=\"{}\" end=\"{}\"/>", ical_utc(r.start)?, ical_utc(r.end)?));
    }
    s.push_str("</c:comp-filter></c:comp-filter></c:filter></c:calendar-query>");
    Ok(s)
}

/// Изменения из ответа `sync-collection`.
pub fn changes(ms: &Multistatus, collection: &str, initial: bool) -> Result<Changes> {
    let token = ms.sync_token.clone().ok_or_else(|| Error::Protocol("sync-collection without sync-token".into()))?;
    let mut out = Changes { token, full: initial, ..Default::default() };
    for r in &ms.responses {
        if same_href(&r.href, collection) {
            // Сама коллекция: 507 — список обрезан; иначе (Stalwart) — признак полного списка.
            if r.status == Some(507) {
                out.truncated = true;
            } else {
                out.full = true;
            }
            continue;
        }
        match r.status {
            Some(404) => out.removed.push(r.href.clone()),
            Some(s) if !(200..300).contains(&s) => {}
            _ => out.changed.push(Item { href: r.href.clone(), etag: r.text(DAV, "getetag").unwrap_or_default() }),
        }
    }
    Ok(out)
}

/// Ресурсы (не коллекции) с ETag из PROPFIND Depth 1.
pub fn items(ms: &Multistatus, collection: &str) -> Vec<Item> {
    ms.responses
        .iter()
        .filter(|r| r.ok() && !same_href(&r.href, collection))
        .filter(|r| !r.prop(DAV, "resourcetype").is_some_and(|t| t.child(DAV, "collection").is_some()))
        .filter_map(|r| Some(Item { href: r.href.clone(), etag: r.text(DAV, "getetag")? }))
        .collect()
}

fn fetched(ms: &Multistatus, kind: Data) -> Fetched {
    let (ns, name) = kind.element();
    let mut out = Fetched::default();
    for r in &ms.responses {
        if !r.ok() {
            out.missing.push(r.href.clone());
            continue;
        }
        let Some(data) = r.prop(ns, name).map(|d| d.text.clone()).filter(|d| !d.trim().is_empty()) else {
            out.missing.push(r.href.clone());
            continue;
        };
        out.resources.push(Resource {
            href: r.href.clone(),
            etag: r.text(DAV, "getetag"),
            schedule_tag: r.text(CALDAV, "schedule-tag"),
            data: data.trim_start().to_owned(),
        });
    }
    out
}

/// Сверка списков ETag (синхронизация без `sync-collection`): `(изменённые и новые, удалённые)`.
pub fn diff(old: &[Item], new: &[Item]) -> (Vec<Item>, Vec<String>) {
    let changed = new.iter().filter(|n| !old.iter().any(|o| same_href(&o.href, &n.href) && o.etag == n.etag)).cloned().collect();
    let removed = old.iter().filter(|o| !new.iter().any(|n| same_href(&n.href, &o.href))).map(|o| o.href.clone()).collect();
    (changed, removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAL: &str = "/dav/cal/user1%40itt.test/default/";

    fn ms(doc: &[u8]) -> Multistatus {
        crate::multistatus::parse(doc).unwrap()
    }

    #[test]
    fn initial_sync_is_full_and_skips_the_collection() {
        let c = changes(&ms(include_bytes!("../tests/fixtures/sync-initial.xml")), CAL, true).unwrap();
        assert!(c.full && !c.truncated);
        assert_eq!(c.changed.len(), 6, "семь ответов, один — сама коллекция");
        assert!(c.changed.iter().all(|i| !i.etag.is_empty() && !same_href(&i.href, CAL)));
        assert_eq!(c.token, "urn:stalwart:davsync:3e0");
    }

    #[test]
    fn incremental_sync_reports_changes_and_removals() {
        let c = changes(&ms(include_bytes!("../tests/fixtures/sync-changed.xml")), CAL, false).unwrap();
        assert!(!c.full);
        assert_eq!(c.changed, [Item { href: format!("{CAL}postman-dav-rec-20261023022343.ics"), etag: "\"2206879412\"".into() }]);
        let c = changes(&ms(include_bytes!("../tests/fixtures/sync-removed.xml")), CAL, false).unwrap();
        assert!(c.changed.is_empty() && !c.full);
        assert_eq!(c.removed, [format!("{CAL}postman-dav-rec-20261023022343.ics")]);
        assert_eq!(c.token, "urn:stalwart:davsync:429");
    }

    #[test]
    fn unknown_token_answer_is_detected_as_full_listing() {
        // Stalwart на неизвестный токен — полный список с самой коллекцией (как sync-initial).
        let c = changes(&ms(include_bytes!("../tests/fixtures/sync-initial.xml")), CAL, false).unwrap();
        assert!(c.full);
    }

    #[test]
    fn truncated_answer_and_missing_token() {
        let doc = br#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/c/</d:href><d:status>HTTP/1.1 507 Insufficient Storage</d:status></d:response>
<d:response><d:href>/c/a.ics</d:href><d:propstat><d:prop><d:getetag>"1"</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:sync-token>t2</d:sync-token></d:multistatus>"#;
        let c = changes(&ms(doc), "/c/", false).unwrap();
        assert!(c.truncated && !c.full);
        assert_eq!(c.changed.len(), 1);
        let none = br#"<d:multistatus xmlns:d="DAV:"/>"#;
        assert!(changes(&ms(none), "/c/", false).is_err());
    }

    #[test]
    fn multiget_returns_data_and_missing() {
        let f = fetched(&ms(include_bytes!("../tests/fixtures/calendar-multiget.xml")), Data::Calendar);
        assert_eq!(f.missing, [format!("{CAL}missing.ics")]);
        assert_eq!(f.resources.len(), 1);
        let r = &f.resources[0];
        assert!(r.data.starts_with("BEGIN:VCALENDAR") && r.data.contains("SUMMARY:Запись DAV"));
        assert_eq!((r.etag.as_deref(), r.schedule_tag.as_deref()), (Some("\"2206879412\""), None));

        let f = fetched(&ms(include_bytes!("../tests/fixtures/addressbook-multiget.xml")), Data::Address);
        assert_eq!(f.missing.len(), 1);
        assert!(f.resources[0].data.contains("FN:Иван Петров"));
    }

    #[test]
    fn query_answer_has_events() {
        let f = fetched(&ms(include_bytes!("../tests/fixtures/calendar-query.xml")), Data::Calendar);
        assert_eq!(f.resources.len(), 1);
        assert!(f.resources[0].data.contains("UID:postman-dav-rec-"));
    }

    #[test]
    fn request_bodies() {
        let b = sync_body(Some("urn:x<1>"));
        assert!(b.contains("<d:sync-token>urn:x&lt;1&gt;</d:sync-token>") && b.contains("<d:sync-level>1</d:sync-level>"));
        assert!(sync_body(None).contains("<d:sync-token/>"));
        let m = multiget_body(Data::Address, &["/a&b.vcf".into()]);
        assert!(m.starts_with("<?xml") && m.contains("<r:addressbook-multiget ") && m.contains("<d:href>/a&amp;b.vcf</d:href>"));
        crate::xml::parse(m.as_bytes()).unwrap();
        let q = query_body("vevent", Some(Span::new(1_793_000_000, 1_793_086_400)), false).unwrap();
        assert!(q.contains("name=\"VEVENT\"") && q.contains("start=\"20261026T") && q.contains("Z\" end=\""), "{q}");
        crate::xml::parse(q.as_bytes()).unwrap();
        assert!(query_body("VEVENT\"/><x", None, false).is_err());
        let e = query_body("VEVENT", Some(Span::new(0, 3600)), true).unwrap();
        assert!(e.contains("<c:calendar-data><c:expand start=\"19700101T000000Z\" end=\"19700101T010000Z\"/></c:calendar-data>"), "{e}");
        crate::xml::parse(e.as_bytes()).unwrap();
    }

    #[test]
    fn etag_listing_and_diff() {
        let doc = br#"<d:multistatus xmlns:d="DAV:">
<d:response><d:href>/c/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:response><d:href>/c/a.ics</d:href><d:propstat><d:prop><d:getetag>"1"</d:getetag><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:response><d:href>/c/b.ics</d:href><d:propstat><d:prop><d:getetag>"2"</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:response><d:href>/c/sub/</d:href><d:propstat><d:prop><d:getetag>"9"</d:getetag><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
</d:multistatus>"#;
        let new = items(&ms(doc), "/c/");
        assert_eq!(new.iter().map(|i| i.href.as_str()).collect::<Vec<_>>(), ["/c/a.ics", "/c/b.ics"]);
        let old = vec![
            Item { href: "/c/a.ics".into(), etag: "\"1\"".into() },
            Item { href: "/c/gone.ics".into(), etag: "\"5\"".into() },
            Item { href: "/c/b.ics".into(), etag: "\"0\"".into() },
        ];
        let (changed, removed) = diff(&old, &new);
        assert_eq!(changed, [Item { href: "/c/b.ics".into(), etag: "\"2\"".into() }]);
        assert_eq!(removed, ["/c/gone.ics"]);
    }

    #[test]
    fn utc_format() {
        assert_eq!(ical_utc(0).unwrap(), "19700101T000000Z");
    }
}
