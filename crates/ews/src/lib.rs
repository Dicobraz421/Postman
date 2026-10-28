//! Минимальный EWS для Exchange on-prem (best effort; ADR 0003, S7-06): календарь и ответы на встречи, занятость
//! (`GetUserAvailability`), справочник (`FindPeople`, `ResolveNames`) и почта (`mail`). Почта Exchange идёт по EWS — решение
//! владельца 2026-10-23 (у Exchange компании IMAP4 и приём SMTP выключены, `docs/EXCHANGE.md`). Без надстроек
//! Microsoft: ни правил, ни главного списка категорий, ни OOF, ни задач, ни общих ящиков. Изменения — опросом
//! `SyncFolderItems`/`SyncFolderHierarchy`, а что и когда опрашивать — по уведомлениям сервера (`notify`: streaming,
//! запасной — pull, T1-21а).
//!
//! Транспорт — HTTP/1.1 одним соединением TLS на сессию (hyper, тот же, что под reqwest): вход Negotiate (Kerberos
//! от учётной записи Windows) или NTLM с паролем через SSPI Windows (`sys`), с привязкой к каналу TLS. Basic и
//! OAuth нет. TLS — SChannel Windows (`native-tls`): шифры и проверка сертификата — как у Outlook (`http.rs`).
//!
//! Ответы сервера разбираются здесь (`xml`): без DTD и сущностей, с пределами размера и глубины. Модель Postman —
//! типы `postman-calendar` (`Event`, `Participant`, `Busy`); перевод в кэш и интерфейс — в `postman_core` (S7-06б).
//!
//! Запросы — последовательно (`&mut self`), не чаще [`MIN_INTERVAL`]; `ErrorServerBusy` — пауза
//! `BackOffMilliseconds`, не больше [`BUSY_RETRIES`] повторов (S7 «Проверка на боевом Exchange»).

mod auth;
pub mod autodiscover;
pub mod availability;
pub mod calendar;
pub mod directory;
mod http;
pub mod mail;
pub mod notify;
mod soap;
mod sys;
pub mod tz;
mod xml;

use std::time::Duration;

pub use autodiscover::{Endpoint, Source, discover};
pub use availability::Availability;
pub use calendar::{
    Attendee, CalendarItem, Changes, DayOfWeek, DeleteMode, Folder, FolderId, FolderRef, ItemId, ItemKind, ItemRef, NewItem, Notify, Occurrence,
    Pattern, Range, Recurrence, Reply, SyncChanges, SyncState, WeekIndex, WellKnown,
};
pub use directory::Person;
pub use mail::{Attachment, Disposition, HierarchyChanges, MailFolder, MailHeader, MailItem, MailRole, MailSync, apply_roles};
pub use notify::{Change, Event, EventKind, EventStream, Mode, Notifier, Subscription, changes};

/// Сколько ждать соединения и ответа на один запрос.
pub const TIMEOUT: Duration = Duration::from_secs(60);
/// Не чаще одного запроса за это время (5 в секунду — предел S7 для боевого Exchange).
pub const MIN_INTERVAL: Duration = Duration::from_millis(200);
/// Повторов после `ErrorServerBusy`, затем — [`Error::ServerBusy`].
pub const BUSY_RETRIES: u32 = 2;
/// Пачка объектов в одном запросе (`GetItem`, `DeleteItem`).
pub const BATCH: usize = 20;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network: {0}")]
    Io(#[from] std::io::Error),
    #[error("tls: {0}")]
    Tls(String),
    #[error("http: {0}")]
    Http(String),
    /// Сервер закрыл соединение раньше ответа.
    #[error("connection closed")]
    Closed,
    /// Ответ HTTP, которого не ждали (не 200 и не SOAP Fault с 500).
    #[error("http status {0}")]
    Status(u16),
    /// Сервер отклонил вход.
    #[error("authentication failed: {0}")]
    Auth(String),
    /// Сервер не предлагает ни Negotiate, ни NTLM (только Basic или ничего) — не Exchange или вход выключен.
    #[error("server offers neither Negotiate nor NTLM")]
    AuthScheme,
    /// Вызов SSPI вернул ошибку (`SEC_E_*`).
    #[error("sspi {call}: 0x{code:08x}")]
    Sspi { call: &'static str, code: u32 },
    #[error("bad url: {0}")]
    Url(#[from] url::ParseError),
    /// Ответ не разобрался: не XML, DTD, слишком глубоко, нет нужного элемента.
    #[error("xml: {0}")]
    Xml(String),
    /// Ответ больше предела.
    #[error("response too large")]
    TooLarge,
    /// Ошибка EWS: `ResponseCode` (`ErrorItemNotFound`, `ErrorInvalidSyncStateData`…) и текст сервера. В тексте
    /// бывают адреса — в журнал и на экран идёт только код.
    #[error("ews {code}")]
    Ews { code: String, message: String },
    /// `ErrorServerBusy` и после [`BUSY_RETRIES`] повторов.
    #[error("server busy, back off {back_off_ms} ms")]
    ServerBusy { back_off_ms: u64 },
    #[error("autodiscover: {0}")]
    Autodiscover(String),
    #[error("timeout")]
    Timeout,
}

impl Error {
    /// Код EWS, если это ошибка EWS.
    pub fn code(&self) -> Option<&str> {
        match self {
            Error::Ews { code, .. } => Some(code),
            _ => None,
        }
    }

    /// Состояние синхронизации больше не годится (`ErrorInvalidSyncStateData`): синхронизировать заново.
    pub fn is_invalid_sync_state(&self) -> bool {
        matches!(self.code(), Some("ErrorInvalidSyncStateData" | "ErrorInvalidSyncStateDataFormat"))
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Только TLS 1.3 для новых соединений («Настройки → Безопасность»), как у JMAP и DAV.
pub fn set_tls13_only(on: bool) {
    http::set_tls13_only(on);
}

/// Похоже ли `url` на EWS Exchange — без входа: POST пустого конверта, ответ 401 с Negotiate или NTLM. Так вид
/// сервера определяется по адресу до входа (S7-06б); адрес EWS по почте — [`discover`] (со входом).
pub async fn probe(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else { return false };
    if parsed.scheme() != "https" {
        return false;
    }
    let mut http = http::Http::new(Auth::Windows);
    http.timeout = Duration::from_secs(5);
    match http.post_anonymous(&parsed, soap::envelope("").into_bytes()).await {
        Ok(reply) => http::Http::offers_sspi(&reply),
        Err(e) => {
            tracing::debug!(error = %e, "ews probe");
            false
        }
    }
}

/// Вход. Только SSPI: Basic и OAuth не поддерживаются (решение владельца, ADR 0003).
#[derive(Clone)]
pub enum Auth {
    /// Учётная запись Windows, без пароля: Kerberos на машине в домене (Negotiate), иначе NTLM текущего входа.
    Windows,
    /// Явная учётная запись: `DOMAIN\user` или `user@domain` (UPN) и пароль — NTLM или Kerberos через Negotiate.
    Password { user: String, password: zeroize::Zeroizing<String> },
}

impl Auth {
    pub fn password(user: impl Into<String>, password: impl Into<String>) -> Auth {
        Auth::Password { user: user.into(), password: zeroize::Zeroizing::new(password.into()) }
    }
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Ни имя, ни пароль — в журнал не попадают.
        match self {
            Auth::Windows => f.write_str("Auth::Windows"),
            Auth::Password { .. } => f.write_str("Auth::Password"),
        }
    }
}

/// Версия сервера из `ServerVersionInfo` ответа (15.1 — Exchange 2016, 15.2 — 2019 и SE).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct ServerVersion {
    pub major: u32,
    pub minor: u32,
    pub build: u32,
    pub revision: u32,
}

/// Клиент EWS одной учётной записи: одно соединение, запросы по очереди.
pub struct Client {
    http: http::Http,
    url: url::Url,
    version: Option<ServerVersion>,
    /// Id корня почтовых папок (`msgfolderroot`) — после первого запроса дерева.
    root: Option<String>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Адрес сервера — не в журнал (S7: ни адреса, ни домена).
        f.debug_struct("Client").field("version", &self.version).finish()
    }
}

impl Client {
    /// Клиент по адресу EWS (`https://…/EWS/Exchange.asmx`): из [`discover`] или введённый вручную. Соединяется
    /// при первом запросе.
    pub fn new(url: &str, auth: Auth) -> Result<Client> {
        let url = url::Url::parse(url)?;
        // http:// — только подменный сервер тестов (`testkit`); у рабочей сборки — только https.
        let plain_ok = cfg!(feature = "testkit") && url.scheme() == "http";
        if (url.scheme() != "https" && !plain_ok) || url.host_str().is_none() {
            return Err(Error::Http("EWS needs an https:// URL".into()));
        }
        Ok(Client { http: http::Http::new(auth), url, version: None, root: None })
    }

    /// Версия сервера — после первого ответа.
    pub fn server_version(&self) -> Option<ServerVersion> {
        self.version
    }

    /// Схема входа на этом соединении (`Negotiate` или `NTLM`) — после первого ответа, если сервер просил вход.
    pub fn signed_in_with(&self) -> Option<&'static str> {
        self.http.signed_in
    }

    /// Интервал между запросами (по умолчанию [`MIN_INTERVAL`]).
    pub fn set_min_interval(&mut self, d: Duration) {
        self.http.min_interval = d;
    }

    /// Один вызов SOAP: тело операции → первый элемент `Body` ответа (`<m:{Op}Response>`). Сбой SOAP — ошибка;
    /// `ErrorServerBusy` — пауза и повтор.
    pub(crate) async fn call(&mut self, op: &'static str, body: &str) -> Result<xml::El> {
        let envelope = soap::envelope(body);
        let mut retries = 0;
        loop {
            let started = std::time::Instant::now();
            let reply = self.http.post(&self.url, envelope.clone().into_bytes()).await?;
            tracing::debug!(op, status = reply.status, ms = started.elapsed().as_millis() as u64, "ews call");
            if reply.status != 200 && reply.status != 500 {
                return Err(Error::Status(reply.status));
            }
            let doc = xml::parse(&reply.body)?;
            if let Some(v) = soap::server_version(&doc) {
                self.version = Some(v);
            }
            match soap::response(doc) {
                Err(Error::ServerBusy { back_off_ms }) if retries < BUSY_RETRIES => {
                    retries += 1;
                    tracing::warn!(op, back_off_ms, retries, "ews server busy");
                    tokio::time::sleep(Duration::from_millis(back_off_ms.min(soap::MAX_BACK_OFF_MS))).await;
                }
                other => return other,
            }
        }
    }
}
