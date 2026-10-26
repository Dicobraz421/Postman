//! 207 Multi-Status (RFC 4918 §13): ответы по ресурсам, свойства с 200, статус ресурса целиком (404 в
//! `sync-collection` и `multiget`), `sync-token` (RFC 6578).

use crate::xml::{DAV, Element, XmlError};

/// Ответ по одному ресурсу.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// Путь ресурса, как его дал сервер (с процентным кодированием); полный URL сводится к пути.
    pub href: String,
    /// Статус ресурса целиком (`<d:status>` без propstat): 404 — ресурса нет, 507 — список обрезан.
    pub status: Option<u16>,
    /// Свойства из propstat с 2xx; не найденные (404) сюда не попадают.
    pub props: Vec<Element>,
}

impl Response {
    pub fn prop(&self, ns: &str, name: &str) -> Option<&Element> {
        self.props.iter().find(|p| p.is(ns, name))
    }

    /// Текст свойства без пробелов по краям; пустое — `None`.
    pub fn text(&self, ns: &str, name: &str) -> Option<String> {
        self.prop(ns, name).map(|p| p.value().to_owned()).filter(|v| !v.is_empty())
    }

    /// Ресурс есть и свойства пришли (а не 404 у ресурса целиком).
    pub fn ok(&self) -> bool {
        self.status.is_none_or(|s| (200..300).contains(&s))
    }
}

/// Ответ 207 целиком.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Multistatus {
    pub responses: Vec<Response>,
    /// Новый `sync-token` (`sync-collection`).
    pub sync_token: Option<String>,
}

/// Разобрать тело 207.
pub fn parse(body: &[u8]) -> Result<Multistatus, XmlError> {
    let root = crate::xml::parse(body)?;
    if !root.is(DAV, "multistatus") {
        return Err(XmlError(format!("expected DAV:multistatus, got {}", root.name)));
    }
    let mut out =
        Multistatus { sync_token: root.child(DAV, "sync-token").map(|t| t.value().to_owned()).filter(|t| !t.is_empty()), ..Default::default() };
    for r in root.all(DAV, "response") {
        let Some(href) = r.href() else { continue };
        let status = r.child(DAV, "status").and_then(|s| status_code(s.value()));
        let mut props = Vec::new();
        for ps in r.all(DAV, "propstat") {
            let code = ps.child(DAV, "status").and_then(|s| status_code(s.value())).unwrap_or(200);
            if !(200..300).contains(&code) {
                continue;
            }
            if let Some(prop) = ps.child(DAV, "prop") {
                props.extend(prop.children.iter().cloned());
            }
        }
        out.responses.push(Response { href: path_of(href), status, props });
    }
    Ok(out)
}

/// Код из строки статуса `HTTP/1.1 404 Not Found`.
pub fn status_code(line: &str) -> Option<u16> {
    line.split_whitespace().nth(1)?.parse().ok()
}

/// Полный URL в `href` → путь (с запросом): сервер вправе отдать и так, и так (RFC 4918 §8.3).
pub fn path_of(href: &str) -> String {
    match url::Url::parse(href) {
        Ok(u) if u.has_host() => match u.query() {
            Some(q) => format!("{}?{q}", u.path()),
            None => u.path().to_owned(),
        },
        _ => href.to_owned(),
    }
}

/// Один ли это ресурс: без процентного кодирования и завершающей `/` (`user1%40itt.test` = `user1@itt.test`).
pub fn same_href(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        let p = path_of(s);
        let d = percent_encoding::percent_decode_str(&p).decode_utf8_lossy().into_owned();
        d.trim_end_matches('/').to_owned()
    };
    norm(a) == norm(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_hrefs_and_comparison() {
        assert_eq!(status_code("HTTP/1.1 404 Not Found"), Some(404));
        assert_eq!(status_code("garbage"), None);
        assert_eq!(path_of("https://h.example/dav/cal/u%40d/"), "/dav/cal/u%40d/");
        assert_eq!(path_of("/dav/cal/"), "/dav/cal/");
        assert!(same_href("/dav/cal/user1%40itt.test/default/", "https://x/dav/cal/user1@itt.test/default"));
        assert!(!same_href("/dav/cal/a/", "/dav/cal/b/"));
    }

    #[test]
    fn not_found_props_are_dropped_and_resource_status_kept() {
        let ms = parse(include_bytes!("../tests/fixtures/calendar-multiget.xml")).unwrap();
        assert_eq!(ms.responses.len(), 2);
        let missing = ms.responses.iter().find(|r| r.href.ends_with("missing.ics")).unwrap();
        assert_eq!((missing.status, missing.ok()), (Some(404), false));
        let found = ms.responses.iter().find(|r| r.ok()).unwrap();
        assert!(found.prop(crate::xml::CALDAV, "schedule-tag").is_none(), "404 propstat dropped");
        assert_eq!(found.text(DAV, "getetag").as_deref(), Some("\"2206879412\""));
    }

    #[test]
    fn other_root_is_an_error() {
        assert!(parse(b"<a xmlns=\"DAV:\"/>").is_err());
    }
}
