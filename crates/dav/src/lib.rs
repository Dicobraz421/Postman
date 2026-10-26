//! CalDAV и CardDAV для второго пути к серверу (ADR 0003, S7-05): тонкий клиент WebDAV на `reqwest` и quick-xml
//! без знания модели Postman — поиск (`.well-known`, `current-user-principal`, домашние наборы, ящики
//! scheduling), календари и адресные книги, синхронизация (`sync-collection` RFC 6578, иначе ctag и ETag),
//! `calendar-multiget`/`addressbook-multiget`, `calendar-query` по времени, PUT/DELETE с `If-Match`, занятость
//! (POST VFREEBUSY в outbox, RFC 6638), права коллекции (ACL RFC 3744). Перевод в кэш Postman — в `postman_core`
//! (адаптеры `Calendar::CalDav`, `Contacts::CardDav`, S7-05б).
//!
//! TLS — rustls с проверкой сертификата и отзыва Windows (`rustls-platform-verifier` внутри reqwest, как у JMAP);
//! вход — Basic. Перенаправления — вручную и только на тот же хост: пароль не уходит на чужой адрес.
//! iCalendar и vCard здесь — только байты: разбор чужих .ics и .vcf — в воркере-песочнице; сам клиент разбирает
//! лишь XML своего сервера и строки `FREEBUSY` ответа outbox (`schedule`).

pub mod acl;
pub mod collection;
pub mod discover;
pub mod multistatus;
pub mod schedule;
pub mod sync;
pub mod xml;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use reqwest::header::{HeaderMap, HeaderValue};
use url::Url;

pub use acl::{Ace, Privilege};
pub use collection::{Collection, Tokens};
pub use discover::Discovery;
pub use multistatus::{Multistatus, Response, same_href};
pub use schedule::FreeBusyReply;
pub use sync::{Changes, Fetched, Item, Resource, diff};

/// Сколько ждать соединения.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Сколько ждать ответа целиком (большая коллекция — мегабайты XML).
pub const TIMEOUT: Duration = Duration::from_secs(120);
/// Больше тело ответа не читаем: коллекция в десятки тысяч ресурсов — единицы МиБ.
pub const MAX_BODY: usize = 64 * 1024 * 1024;
/// Перенаправлений подряд — не больше (`.well-known` → контекст DAV — одно).
const MAX_REDIRECTS: usize = 5;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Сеть и TLS; адрес из текста ошибки убран (`without_url`): в журнал он не попадает.
    #[error("http: {0}")]
    Http(reqwest::Error),
    #[error("url: {0}")]
    Url(#[from] url::ParseError),
    /// 401: сервер не принял вход.
    #[error("authentication failed")]
    Auth,
    /// 403: нет прав на ресурс.
    #[error("forbidden")]
    Forbidden,
    /// 404: ресурса или коллекции нет.
    #[error("not found")]
    NotFound,
    /// 412: ресурс изменился (`If-Match`, `If-Schedule-Tag-Match`) или уже есть (`If-None-Match: *`).
    #[error("precondition failed")]
    Precondition,
    /// `sync-token` больше не годится (RFC 6578 §3.2, `DAV:valid-sync-token`): нужна полная сверка.
    #[error("sync token is no longer valid")]
    SyncToken,
    /// Другой код ответа.
    #[error("server answered {0}")]
    Status(u16),
    #[error("xml: {0}")]
    Xml(#[from] xml::XmlError),
    /// Сервер ответил не так, как ждали (нет `current-user-principal`, чужой хост в перенаправлении…).
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("response is larger than {MAX_BODY} bytes")]
    TooLarge,
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Http(e.without_url())
    }
}

/// Сервер и вход.
#[derive(Clone)]
pub struct Config {
    /// Адрес сервера (`https://mail.example`) или контекст DAV (`https://mail.example/dav/cal/`).
    pub url: String,
    pub user: String,
    pub password: String,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Ни адрес, ни пароль — в журнал не идут.
        f.debug_struct("Config").finish_non_exhaustive()
    }
}

/// Что кладём: календарь (`.ics`) или карточку (`.vcf`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Content {
    Calendar,
    VCard,
}

impl Content {
    fn mime(self) -> &'static str {
        match self {
            Content::Calendar => "text/calendar; charset=utf-8",
            Content::VCard => "text/vcard; charset=utf-8",
        }
    }

    /// Расширение имени нового ресурса.
    pub fn ext(self) -> &'static str {
        match self {
            Content::Calendar => "ics",
            Content::VCard => "vcf",
        }
    }
}

/// Условие записи и удаления.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Precondition<'a> {
    /// Без условия (последний пишущий побеждает) — только для уборки.
    None,
    /// Новый ресурс: `If-None-Match: *`, существующий — 412.
    Create,
    /// Тот же ETag (`If-Match`): изменился — 412.
    Etag(&'a str),
    /// Тот же Schedule-Tag (`If-Schedule-Tag-Match`, RFC 6638 §3.2): у участника — правка своей копии,
    /// которую не сбивают изменения только `PARTSTAT` других участников.
    ScheduleTag(&'a str),
}

/// Итог PUT.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Written {
    /// Создан (201), а не заменён.
    pub created: bool,
    /// Новый ETag, если сервер его отдал (иначе — перечитать).
    pub etag: Option<String>,
    /// Schedule-Tag (RFC 6638 §3.2) у ресурса планирования.
    pub schedule_tag: Option<String>,
}

/// Только TLS 1.3 («Настройки → Безопасность», S5-30; как `postman_jmap::set_tls13_only`); иначе — 1.2 и выше.
static TLS13_ONLY: AtomicBool = AtomicBool::new(false);

/// Минимальная версия TLS для новых клиентов: `true` — только 1.3.
pub fn set_tls13_only(on: bool) {
    TLS13_ONLY.store(on, Ordering::Relaxed);
}

/// `POSTMAN_INSECURE_TLS=1` — не проверять сертификат сервера (как у JMAP и IMAP). Только для стенда с
/// самоподписанным сертификатом; у рабочего сервера сертификат проверяется всегда.
pub fn insecure_tls() -> bool {
    std::env::var("POSTMAN_INSECURE_TLS").is_ok_and(|v| v == "1")
}

/// Ответ сервера: код, заголовки, тело (не больше `MAX_BODY`).
pub(crate) struct Reply {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn header(&self, name: &str) -> Option<String> {
        self.headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_owned)
    }
}

/// Клиент CalDAV/CardDAV одной учётной записи.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: Url,
    user: String,
    password: String,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").finish_non_exhaustive()
    }
}

impl Client {
    pub fn new(config: &Config) -> Result<Self> {
        let mut builder = reqwest::Client::builder()
            .user_agent(concat!("Postman/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(TIMEOUT)
            // Перенаправления — сами (`send`): метод и тело WebDAV сохраняются, хост — тот же.
            .redirect(reqwest::redirect::Policy::none())
            .tls_version_min(if TLS13_ONLY.load(Ordering::Relaxed) { reqwest::tls::Version::TLS_1_3 } else { reqwest::tls::Version::TLS_1_2 });
        if insecure_tls() {
            tracing::warn!("POSTMAN_INSECURE_TLS: server certificate is NOT verified (test stand only)");
            builder = builder.danger_accept_invalid_certs(true);
        }
        let base = Url::parse(&config.url)?;
        if !matches!(base.scheme(), "https" | "http") {
            return Err(Error::Protocol(format!("unsupported scheme {}", base.scheme())));
        }
        Ok(Self { http: builder.build()?, base, user: config.user.clone(), password: config.password.clone() })
    }

    /// Адрес, с которого начат поиск.
    pub fn base(&self) -> &Url {
        &self.base
    }

    /// Путь или URL ресурса → полный URL на сервере.
    pub fn url(&self, href: &str) -> Result<Url> {
        Ok(self.base.join(href)?)
    }

    /// Запрос с Basic и ручными перенаправлениями (тот же хост; http → https можно).
    pub(crate) async fn send(&self, method: &str, href: &str, headers: &[(&str, String)], body: Option<(&str, String)>) -> Result<Reply> {
        let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(|_| Error::Protocol(format!("bad method {method}")))?;
        let mut url = self.url(href)?;
        let started = Instant::now();
        for _ in 0..=MAX_REDIRECTS {
            let mut rb = self.http.request(method.clone(), url.clone()).basic_auth(&self.user, Some(&self.password));
            for (k, v) in headers {
                rb = rb.header(*k, HeaderValue::from_str(v).map_err(|_| Error::Protocol(format!("bad header {k}")))?);
            }
            if let Some((ct, b)) = &body {
                rb = rb.header(reqwest::header::CONTENT_TYPE, *ct).body(b.clone());
            }
            let mut resp = rb.send().await?;
            let status = resp.status().as_u16();
            if matches!(status, 301 | 302 | 303 | 307 | 308) {
                let to = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| Error::Protocol(format!("redirect {status} without Location")))?;
                let next = url.join(to)?;
                let same_host = next.host_str() == url.host_str() && next.port_or_known_default() == url.port_or_known_default();
                let upgrade = next.host_str() == url.host_str() && next.scheme() == "https";
                if !(same_host || upgrade) {
                    return Err(Error::Protocol("redirect to another host".into()));
                }
                tracing::debug!(method = %method, status, "dav redirect");
                url = next;
                continue;
            }
            let headers = resp.headers().clone();
            if resp.content_length().is_some_and(|n| n > MAX_BODY as u64) {
                return Err(Error::TooLarge);
            }
            let mut body = Vec::new();
            while let Some(chunk) = resp.chunk().await? {
                if body.len() + chunk.len() > MAX_BODY {
                    return Err(Error::TooLarge);
                }
                body.extend_from_slice(&chunk);
            }
            // Адреса и пути не пишем (в них адрес почты): метод, код, размер, время.
            tracing::debug!(method = %method, status, bytes = body.len(), ms = started.elapsed().as_millis() as u64, "dav");
            return Ok(Reply { status, headers, body });
        }
        Err(Error::Protocol("too many redirects".into()))
    }

    /// Запрос, ждущий 207: PROPFIND, REPORT.
    pub(crate) async fn multistatus(&self, method: &str, href: &str, depth: Option<&str>, body: String) -> Result<Multistatus> {
        let mut headers = Vec::new();
        if let Some(d) = depth {
            headers.push(("Depth", d.to_owned()));
        }
        let reply = self.send(method, href, &headers, Some(("application/xml; charset=utf-8", body))).await?;
        if reply.status != 207 {
            return Err(status_error(&reply));
        }
        Ok(multistatus::parse(&reply.body)?)
    }

    /// GET ресурса: тело, ETag, Schedule-Tag.
    pub async fn get(&self, href: &str) -> Result<Resource> {
        let reply = self.send("GET", href, &[], None).await?;
        if reply.status != 200 {
            return Err(status_error(&reply));
        }
        let data = String::from_utf8(reply.body.clone()).map_err(|_| Error::Protocol("resource is not UTF-8".into()))?;
        Ok(Resource { href: href.to_owned(), etag: reply.header("etag"), schedule_tag: reply.header("schedule-tag"), data })
    }

    /// PUT календаря или карточки с условием.
    pub async fn put(&self, href: &str, content: Content, body: &str, pre: Precondition<'_>) -> Result<Written> {
        let reply = self.send("PUT", href, &precondition(pre), Some((content.mime(), body.to_owned()))).await?;
        if !(200..300).contains(&reply.status) {
            return Err(status_error(&reply));
        }
        Ok(Written { created: reply.status == 201, etag: reply.header("etag"), schedule_tag: reply.header("schedule-tag") })
    }

    /// DELETE с условием. `false` — ресурса уже не было (404).
    pub async fn delete(&self, href: &str, pre: Precondition<'_>) -> Result<bool> {
        let reply = self.send("DELETE", href, &precondition(pre), None).await?;
        match reply.status {
            200..=299 => Ok(true),
            404 => Ok(false),
            _ => Err(status_error(&reply)),
        }
    }
}

fn precondition(pre: Precondition<'_>) -> Vec<(&'static str, String)> {
    match pre {
        Precondition::None => Vec::new(),
        Precondition::Create => vec![("If-None-Match", "*".into())],
        Precondition::Etag(e) => vec![("If-Match", e.to_owned())],
        Precondition::ScheduleTag(t) => vec![("If-Schedule-Tag-Match", t.to_owned())],
    }
}

/// Код ответа → ошибка; `DAV:valid-sync-token` в теле — `SyncToken`.
pub(crate) fn status_error(reply: &Reply) -> Error {
    match reply.status {
        401 => Error::Auth,
        403 | 409 | 400 if reply.body.windows(16).any(|w| w == b"valid-sync-token") => Error::SyncToken,
        403 => Error::Forbidden,
        404 => Error::NotFound,
        412 => Error::Precondition,
        s => Error::Status(s),
    }
}

/// Есть ли CalDAV на адресе (`https://узел`) — без входа: `.well-known/caldav` отвечает 207, перенаправлением или
/// 401 (RFC 6764 §5). Для вида сервера по адресу (`postman_core::backend::Registry::detect`, плагин `postman-backend-dav`); ни сети, ни ответа — `false`.
pub async fn has_dav(url: &str) -> bool {
    let Ok(base) = Url::parse(url) else { return false };
    let Ok(target) = base.join("/.well-known/caldav") else { return false };
    let mut builder = reqwest::Client::builder()
        .user_agent(concat!("Postman/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none());
    if insecure_tls() {
        builder = builder.danger_accept_invalid_certs(true);
    }
    let Ok(http) = builder.build() else { return false };
    let Ok(method) = reqwest::Method::from_bytes(b"PROPFIND") else { return false };
    match http.request(method, target).header("Depth", "0").send().await {
        Ok(r) => matches!(r.status().as_u16(), 207 | 301 | 302 | 303 | 307 | 308 | 401),
        Err(_) => false,
    }
}

/// Путь нового ресурса в коллекции: имя — из UID (процентное кодирование всего, кроме букв, цифр и `-._~`).
pub fn child_href(collection: &str, name: &str, content: Content) -> String {
    const KEEP: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');
    let base = if collection.ends_with('/') { collection.to_owned() } else { format!("{collection}/") };
    format!("{base}{}.{}", percent_encoding::utf8_percent_encode(name, KEEP), content.ext())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(status: u16, body: &[u8]) -> Reply {
        Reply { status, headers: HeaderMap::new(), body: body.to_vec() }
    }

    #[test]
    fn status_codes_map_to_errors() {
        assert!(matches!(status_error(&reply(401, b"")), Error::Auth));
        assert!(matches!(status_error(&reply(403, b"")), Error::Forbidden));
        let expired = b"<d:error xmlns:d=\"DAV:\"><d:valid-sync-token/></d:error>";
        assert!(matches!(status_error(&reply(403, expired)), Error::SyncToken));
        assert!(matches!(status_error(&reply(409, expired)), Error::SyncToken));
        assert!(matches!(status_error(&reply(412, b"")), Error::Precondition));
        assert!(matches!(status_error(&reply(404, b"")), Error::NotFound));
        assert!(matches!(status_error(&reply(500, b"")), Error::Status(500)));
    }

    #[test]
    fn preconditions_become_headers() {
        assert_eq!(precondition(Precondition::Create), vec![("If-None-Match", "*".to_owned())]);
        assert_eq!(precondition(Precondition::Etag("\"1\"")), vec![("If-Match", "\"1\"".to_owned())]);
        assert_eq!(precondition(Precondition::ScheduleTag("\"s\"")), vec![("If-Schedule-Tag-Match", "\"s\"".to_owned())]);
        assert!(precondition(Precondition::None).is_empty());
    }

    #[test]
    fn child_href_encodes_uid() {
        assert_eq!(child_href("/dav/cal/u/default/", "a b@c/d", Content::Calendar), "/dav/cal/u/default/a%20b%40c%2Fd.ics");
        assert_eq!(child_href("/dav/card/u/default", "x-1_2.3~", Content::VCard), "/dav/card/u/default/x-1_2.3~.vcf");
    }

    #[test]
    fn config_debug_hides_secrets() {
        let c = Config { url: "https://stand.example".into(), user: "user1@itt.test".into(), password: "secret".into() };
        let s = format!("{c:?}");
        assert!(!s.contains("secret") && !s.contains("stand.example") && !s.contains("user1"), "{s}");
        let client = Client::new(&c).unwrap();
        assert!(!format!("{client:?}").contains("secret"));
        assert!(Client::new(&Config { url: "ftp://x".into(), ..c }).is_err());
    }
}
