//! IMAP и SMTP submission для второго пути к серверу (ADR 0003, S7-04): тонкая обёртка над `async-imap` и
//! `mail-send` без знания модели Postman — папки с ролями, состояние синхронизации папки, заголовки писем, флаги и
//! ключевые слова, письмо целиком, перенос, удаление, `APPEND`, поиск, IDLE; отправка — SMTP submission. Перевод в
//! кэш Postman — в `postman_core` (адаптер `Mail::Imap`).
//!
//! TLS — rustls с проверкой сертификата и отзыва Windows (`rustls-platform-verifier`, как у JMAP): неявный TLS
//! (993, 465) или STARTTLS (143, 587); без TLS не подключаемся. Письма — чужие данные: здесь они только байты,
//! разбор MIME — в воркере-песочнице.

pub mod flags;
pub mod names;
pub mod smtp;
pub mod state;

use std::sync::Arc;
use std::time::Duration;

use async_imap::extensions::idle::IdleResponse;
use async_imap::imap_proto::NameAttribute;
use futures::TryStreamExt as _;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

pub use names::Role;
pub use state::FolderState;

/// Сколько ждать соединения и ответа на команду.
pub const TIMEOUT: Duration = Duration::from_secs(30);
/// IDLE — не дольше: серверы рвут его через 30 мин (RFC 2177 советует 29).
pub const IDLE_MAX: Duration = Duration::from_secs(29 * 60);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network: {0}")]
    Io(#[from] std::io::Error),
    #[error("tls: {0}")]
    Tls(String),
    /// Сервер отклонил вход (`NO`/`BAD` на LOGIN, 535 у SMTP).
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("imap: {0}")]
    Imap(#[from] async_imap::error::Error),
    #[error("smtp: {0}")]
    Smtp(String),
    /// Сервер ответил не так, как ждали (нет UIDVALIDITY, нет STARTTLS…).
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("timeout")]
    Timeout,
}

pub type Result<T> = std::result::Result<T, Error>;

/// Как защищено соединение.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    /// TLS с первого байта (IMAP 993, SMTP 465).
    Tls,
    /// Открытое соединение, затем STARTTLS (IMAP 143, SMTP 587).
    StartTls,
}

/// Сервер и вход.
#[derive(Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub user: String,
    pub password: String,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Пароль — никогда в журнал.
        f.debug_struct("Config").field("host", &self.host).field("port", &self.port).field("security", &self.security).finish()
    }
}

/// Папка сервера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    /// Имя на сервере (modified UTF-7) — им папка называется в командах.
    pub raw: String,
    /// Имя для людей: последний уровень, UTF-8.
    pub name: String,
    /// Родитель — его `raw`; `None` — верхний уровень.
    pub parent: Option<String>,
    pub role: Option<Role>,
    /// `\Noselect` и т. п.: писем в ней нет.
    pub selectable: bool,
}

/// Выбранная папка.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selected {
    pub exists: u32,
    pub state: FolderState,
    /// Сервер хранит свои ключевые слова (`PERMANENTFLAGS` с `\*`).
    pub keywords: bool,
}

/// Заголовок письма для списка.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Header {
    pub uid: u32,
    /// Флаги и ключевые слова как у сервера (`\Seen`, `$Forwarded`, `pm.due`).
    pub flags: Vec<String>,
    /// Unix-секунды (INTERNALDATE).
    pub received_at: i64,
    pub size: u32,
    pub subject: String,
    pub from_name: String,
    pub from_addr: String,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    /// `References` (RFC 5322 §3.6.4): первый — начало цепочки.
    pub references: Vec<String>,
    /// В структуре письма есть вложение (часть `attachment` или не текст верхнего уровня).
    pub has_attachment: bool,
    pub modseq: Option<u64>,
}

/// Изменение флагов письма (CONDSTORE).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlagChange {
    pub uid: u32,
    pub flags: Vec<String>,
}

type Stream = TlsStream<TcpStream>;

/// Сессия IMAP после входа.
pub struct Session {
    inner: async_imap::Session<Stream>,
    caps: Vec<String>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").field("caps", &self.caps).finish()
    }
}

/// `POSTMAN_INSECURE_TLS=1` — не проверять сертификат сервера (как у JMAP, `postman_jmap::insecure_tls`). Только
/// для стенда с самоподписанным сертификатом; у рабочего сервера сертификат проверяется всегда.
pub fn insecure_tls() -> bool {
    std::env::var("POSTMAN_INSECURE_TLS").is_ok_and(|v| v == "1")
}

/// TLS с проверкой сертификата Windows (как у JMAP); провайдер — aws-lc-rs, как у reqwest.
pub(crate) fn tls_connector() -> Result<TlsConnector> {
    use rustls_platform_verifier::BuilderVerifierExt as _;
    let provider = Arc::new(tokio_rustls::rustls::crypto::aws_lc_rs::default_provider());
    let builder = tokio_rustls::rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::Tls(e.to_string()))?;
    let config = if insecure_tls() {
        builder.dangerous().with_custom_certificate_verifier(Arc::new(stand::AnyCert(provider))).with_no_client_auth()
    } else {
        builder.with_platform_verifier().map_err(|e| Error::Tls(e.to_string()))?.with_no_client_auth()
    };
    Ok(TlsConnector::from(Arc::new(config)))
}

/// Проверка сертификата стенда (`POSTMAN_INSECURE_TLS=1`): любой сертификат, подписи рукопожатия — проверяются.
mod stand {
    use std::sync::Arc;

    use tokio_rustls::rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use tokio_rustls::rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
    use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use tokio_rustls::rustls::{DigitallySignedStruct, Error, SignatureScheme};

    #[derive(Debug)]
    pub(crate) struct AnyCert(pub(crate) Arc<CryptoProvider>);

    impl ServerCertVerifier for AnyCert {
        fn verify_server_cert(
            &self,
            _: &CertificateDer<'_>,
            _: &[CertificateDer<'_>],
            _: &ServerName<'_>,
            _: &[u8],
            _: UnixTime,
        ) -> Result<ServerCertVerified, Error> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
            verify_tls12_signature(m, c, d, &self.0.signature_verification_algorithms)
        }

        fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
            verify_tls13_signature(m, c, d, &self.0.signature_verification_algorithms)
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.0.signature_verification_algorithms.supported_schemes()
        }
    }
}

async fn tls(host: &str, tcp: TcpStream) -> Result<Stream> {
    let name = tokio_rustls::rustls::pki_types::ServerName::try_from(host.to_owned()).map_err(|e| Error::Tls(e.to_string()))?;
    tls_connector()?.connect(name, tcp).await.map_err(|e| Error::Tls(e.to_string()))
}

async fn timed<T>(f: impl Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(TIMEOUT, f).await.map_err(|_| Error::Timeout)?
}

/// Строка ответа сервера (до CRLF) на открытом соединении — для STARTTLS до TLS.
async fn line<S: AsyncRead + Unpin>(s: &mut S) -> Result<String> {
    let mut out = Vec::new();
    let mut b = [0u8; 1];
    while out.len() < 8192 {
        if s.read(&mut b).await? == 0 {
            return Err(Error::Protocol("connection closed".into()));
        }
        out.push(b[0]);
        if out.ends_with(b"\r\n") {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&out).trim_end().to_owned())
}

/// STARTTLS до входа (RFC 3501 §6.2.1): приветствие, `a STARTTLS`, ответ `a OK` — дальше TLS.
async fn starttls<S: AsyncRead + AsyncWrite + Unpin>(s: &mut S) -> Result<()> {
    let greeting = line(s).await?;
    if !greeting.starts_with("* OK") {
        return Err(Error::Protocol(format!("greeting: {greeting}")));
    }
    s.write_all(b"a STARTTLS\r\n").await?;
    loop {
        let l = line(s).await?;
        if l.starts_with("a OK") {
            return Ok(());
        }
        if l.starts_with("a ") {
            return Err(Error::Protocol(format!("STARTTLS: {l}")));
        }
    }
}

fn fetch_flags(f: &async_imap::types::Fetch) -> Vec<String> {
    f.flags().map(|x| flags::flag_text(&x)).collect()
}

impl Session {
    /// Подключиться и войти (`LOGIN` по TLS).
    pub async fn connect(cfg: &Config) -> Result<Session> {
        timed(async {
            let mut tcp = TcpStream::connect((cfg.host.as_str(), cfg.port)).await?;
            if cfg.security == Security::StartTls {
                starttls(&mut tcp).await?;
            }
            let stream = tls(&cfg.host, tcp).await?;
            let mut client = async_imap::Client::new(stream);
            if cfg.security == Security::Tls {
                // Приветствие сервера (после STARTTLS его нет).
                client.read_response().await?;
            }
            let mut inner = client.login(&cfg.user, &cfg.password).await.map_err(|(e, _)| match e {
                async_imap::error::Error::No(m) | async_imap::error::Error::Bad(m) => Error::Auth(m),
                e => Error::Imap(e),
            })?;
            let caps: Vec<String> = inner.capabilities().await?.iter().map(|c| format!("{c:?}")).map(|c| caps::name(&c)).collect();
            let mut s = Session { inner, caps };
            // CONDSTORE/QRESYNC объявлен — включить (RFC 7162 §3.2.3): MODSEQ в ответах FETCH.
            if s.has("QRESYNC") {
                s.inner.run_command_and_check_ok("ENABLE QRESYNC").await?;
            } else if s.has("CONDSTORE") {
                s.inner.run_command_and_check_ok("ENABLE CONDSTORE").await?;
            }
            Ok(s)
        })
        .await
    }

    /// Возможность сервера (`IDLE`, `MOVE`, `CONDSTORE`, `UIDPLUS`, `SPECIAL-USE`…), без учёта регистра.
    pub fn has(&self, cap: &str) -> bool {
        self.caps.iter().any(|c| c.eq_ignore_ascii_case(cap))
    }

    pub fn capabilities(&self) -> &[String] {
        &self.caps
    }

    fn condstore(&self) -> bool {
        self.has("CONDSTORE") || self.has("QRESYNC")
    }

    /// Все папки с ролями: `SPECIAL-USE`, иначе — по имени (ru/en).
    pub async fn folders(&mut self) -> Result<Vec<Folder>> {
        let names: Vec<async_imap::types::Name> = timed(async { Ok(self.inner.list(Some(""), Some("*")).await?.try_collect().await?) }).await?;
        Ok(names
            .iter()
            .map(|n| {
                let attrs: Vec<names::Attr> = n
                    .attributes()
                    .iter()
                    .map(|a| match a {
                        NameAttribute::Sent => names::Attr::Role(Role::Sent),
                        NameAttribute::Drafts => names::Attr::Role(Role::Drafts),
                        NameAttribute::Trash => names::Attr::Role(Role::Trash),
                        NameAttribute::Junk => names::Attr::Role(Role::Junk),
                        NameAttribute::Archive => names::Attr::Role(Role::Archive),
                        NameAttribute::NoSelect => names::Attr::NoSelect,
                        _ => names::Attr::Other,
                    })
                    .collect();
                names::folder(n.name(), n.delimiter(), &attrs)
            })
            .collect())
    }

    /// Выбрать папку (с CONDSTORE, если он есть).
    pub async fn select(&mut self, raw: &str) -> Result<Selected> {
        let condstore = self.condstore();
        let mb = timed(async { Ok(if condstore { self.inner.select_condstore(raw).await? } else { self.inner.select(raw).await? }) }).await?;
        let uid_validity = mb.uid_validity.ok_or_else(|| Error::Protocol(format!("no UIDVALIDITY for {raw}")))?;
        let keywords = mb.permanent_flags.iter().any(|f| matches!(f, async_imap::types::Flag::MayCreate));
        Ok(Selected {
            exists: mb.exists,
            state: FolderState { uid_validity, uid_next: mb.uid_next.unwrap_or(0), modseq: mb.highest_modseq },
            keywords,
        })
    }

    /// Заголовки писем выбранной папки по набору UID (`1:*`, `5,7,9`, `100:*`).
    pub async fn headers(&mut self, uids: &str) -> Result<Vec<Header>> {
        let query = if self.condstore() {
            "(UID FLAGS INTERNALDATE RFC822.SIZE ENVELOPE BODYSTRUCTURE MODSEQ BODY.PEEK[HEADER.FIELDS (REFERENCES)])"
        } else {
            "(UID FLAGS INTERNALDATE RFC822.SIZE ENVELOPE BODYSTRUCTURE BODY.PEEK[HEADER.FIELDS (REFERENCES)])"
        };
        let fetched: Vec<async_imap::types::Fetch> = timed(async { Ok(self.inner.uid_fetch(uids, query).await?.try_collect().await?) }).await?;
        Ok(fetched.iter().filter_map(header).collect())
    }

    /// Флаги писем, изменённых после `modseq` (CONDSTORE, RFC 7162 §3.1.4).
    pub async fn changed_since(&mut self, modseq: u64) -> Result<Vec<FlagChange>> {
        let query = format!("(UID FLAGS) (CHANGEDSINCE {modseq})");
        let fetched: Vec<async_imap::types::Fetch> = timed(async { Ok(self.inner.uid_fetch("1:*", &query).await?.try_collect().await?) }).await?;
        Ok(fetched.iter().filter_map(|f| Some(FlagChange { uid: f.uid?, flags: fetch_flags(f) })).collect())
    }

    /// Флаги всех писем выбранной папки (без CONDSTORE — сверка целиком).
    pub async fn all_flags(&mut self) -> Result<Vec<FlagChange>> {
        let fetched: Vec<async_imap::types::Fetch> =
            timed(async { Ok(self.inner.uid_fetch("1:*", "(UID FLAGS)").await?.try_collect().await?) }).await?;
        Ok(fetched.iter().filter_map(|f| Some(FlagChange { uid: f.uid?, flags: fetch_flags(f) })).collect())
    }

    /// UID всех писем выбранной папки: удалённые на сервере — те, которых здесь нет.
    pub async fn uids(&mut self) -> Result<Vec<u32>> {
        let mut v: Vec<u32> = timed(async { Ok(self.inner.uid_search("ALL").await?) }).await?.into_iter().collect();
        v.sort_unstable();
        Ok(v)
    }

    /// Письмо целиком (RFC 5322), не отмечая прочитанным (`BODY.PEEK[]`).
    pub async fn message(&mut self, uid: u32) -> Result<Option<Vec<u8>>> {
        let fetched: Vec<async_imap::types::Fetch> =
            timed(async { Ok(self.inner.uid_fetch(uid.to_string(), "(UID BODY.PEEK[])").await?.try_collect().await?) }).await?;
        Ok(fetched.iter().find(|f| f.uid == Some(uid)).and_then(|f| f.body().map(<[u8]>::to_vec)))
    }

    /// Поставить (`on`) или снять флаги и ключевые слова у писем.
    pub async fn store(&mut self, uids: &str, flags: &[String], on: bool) -> Result<()> {
        if flags.is_empty() || uids.is_empty() {
            return Ok(());
        }
        let query = format!("{}FLAGS.SILENT ({})", if on { '+' } else { '-' }, flags.join(" "));
        timed(async {
            let _: Vec<async_imap::types::Fetch> = self.inner.uid_store(uids, &query).await?.try_collect().await?;
            Ok(())
        })
        .await
    }

    /// Перенести письма в папку: `MOVE`, иначе `COPY` + `\Deleted` + `UID EXPUNGE`.
    pub async fn move_to(&mut self, uids: &str, to: &str) -> Result<()> {
        if self.has("MOVE") {
            return timed(async { Ok(self.inner.uid_mv(uids, to).await?) }).await;
        }
        timed(async { Ok(self.inner.uid_copy(uids, to).await?) }).await?;
        self.delete(uids).await
    }

    /// Удалить письма насовсем: `\Deleted` и `UID EXPUNGE` (UIDPLUS), иначе `EXPUNGE`.
    pub async fn delete(&mut self, uids: &str) -> Result<()> {
        self.store(uids, &["\\Deleted".to_owned()], true).await?;
        let uidplus = self.has("UIDPLUS");
        timed(async {
            if uidplus {
                let _: Vec<u32> = self.inner.uid_expunge(uids).await?.try_collect().await?;
            } else {
                let _: Vec<u32> = self.inner.expunge().await?.try_collect().await?;
            }
            Ok(())
        })
        .await
    }

    /// Положить письмо в папку (копия отправленного, черновик).
    pub async fn append(&mut self, folder: &str, flags: &[&str], raw: &[u8]) -> Result<()> {
        let flags = (!flags.is_empty()).then(|| format!("({})", flags.join(" ")));
        timed(async { Ok(self.inner.append(folder, flags.as_deref(), None, raw).await?) }).await
    }

    /// Поиск в выбранной папке по готовому условию IMAP (`HEADER Message-ID "<…>"`, `UID 1:100`).
    pub async fn search(&mut self, query: &str) -> Result<Vec<u32>> {
        let mut v: Vec<u32> = timed(async { Ok(self.inner.uid_search(query).await?) }).await?.into_iter().collect();
        v.sort_unstable();
        Ok(v)
    }

    /// Поиск в выбранной папке: текст в заголовках и теле (`TEXT`, UTF-8).
    pub async fn search_text(&mut self, text: &str) -> Result<Vec<u32>> {
        let q = format!("CHARSET UTF-8 TEXT {}", quote(text));
        let mut v: Vec<u32> = timed(async { Ok(self.inner.uid_search(&q).await?) }).await?.into_iter().collect();
        v.sort_unstable();
        Ok(v)
    }

    /// IDLE на выбранной папке до изменения или `max` (не дольше [`IDLE_MAX`]). `true` — сервер сообщил об
    /// изменении (новое письмо, удаление, флаги). Сессия возвращается после `DONE`.
    pub async fn idle(self, max: Duration) -> Result<(Session, bool)> {
        let Session { inner, caps } = self;
        let mut handle = inner.idle();
        handle.init().await?;
        let changed = {
            let (wait, _stop) = handle.wait_with_timeout(max.min(IDLE_MAX));
            matches!(wait.await?, IdleResponse::NewData(_))
        };
        let inner = handle.done().await?;
        Ok((Session { inner, caps }, changed))
    }

    pub async fn logout(mut self) -> Result<()> {
        timed(async { Ok(self.inner.logout().await?) }).await
    }
}

mod caps {
    /// Имя возможности из её `Debug` (`Atom("IDLE")` → `IDLE`, `Auth("PLAIN")` → `AUTH=PLAIN`).
    pub(crate) fn name(debug: &str) -> String {
        let inner = |s: &str| s.trim_end_matches(')').trim_matches('"').to_owned();
        if let Some(rest) = debug.strip_prefix("Atom(") {
            inner(rest)
        } else if let Some(rest) = debug.strip_prefix("Auth(") {
            format!("AUTH={}", inner(rest))
        } else if debug == "Imap4rev1" {
            "IMAP4rev1".to_owned()
        } else {
            debug.to_owned()
        }
    }
}

/// Строка в кавычках IMAP (RFC 3501 `quoted`): `\` и `"` экранируются, переводы строк — пробелом.
pub fn quote(s: &str) -> String {
    let body: String = s.chars().map(|c| if c == '\r' || c == '\n' { ' ' } else { c }).collect();
    format!("\"{}\"", body.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Набор UID для команды: подряд идущие — диапазоном.
pub fn uid_set(uids: &[u32]) -> String {
    let mut v = uids.to_vec();
    v.sort_unstable();
    v.dedup();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < v.len() {
        let start = v[i];
        let mut end = start;
        while i + 1 < v.len() && v[i + 1] == end + 1 {
            i += 1;
            end = v[i];
        }
        out.push(if start == end { start.to_string() } else { format!("{start}:{end}") });
        i += 1;
    }
    out.join(",")
}

fn text(b: &Option<std::borrow::Cow<'_, [u8]>>) -> String {
    b.as_deref().map(|b| names::decode_header(&String::from_utf8_lossy(b))).unwrap_or_default()
}

/// Идентификаторы из заголовка `References` (с переносами строк): `<a@b> <c@d>` → `["<a@b>", "<c@d>"]`.
pub fn references(header: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(header);
    let value = text.split_once(':').map_or("", |(_, v)| v);
    let mut out = Vec::new();
    let mut rest = value;
    while let Some(start) = rest.find('<') {
        let Some(len) = rest[start..].find('>') else { break };
        let id: String = rest[start..=start + len].chars().filter(|c| !c.is_whitespace()).collect();
        out.push(id);
        rest = &rest[start + len + 1..];
    }
    out
}

fn header(f: &async_imap::types::Fetch) -> Option<Header> {
    let uid = f.uid?;
    let env = f.envelope();
    let from = env.and_then(|e| e.from.as_ref()).and_then(|v| v.first());
    let addr = from
        .map(|a| {
            let mailbox = a.mailbox.as_deref().map(String::from_utf8_lossy).unwrap_or_default();
            let host = a.host.as_deref().map(String::from_utf8_lossy).unwrap_or_default();
            if host.is_empty() { mailbox.into_owned() } else { format!("{mailbox}@{host}") }
        })
        .unwrap_or_default();
    Some(Header {
        uid,
        flags: fetch_flags(f),
        received_at: f.internal_date().map_or(0, |d| d.timestamp()),
        size: f.size.unwrap_or(0),
        subject: env.map(|e| text(&e.subject)).unwrap_or_default(),
        from_name: from.map(|a| text(&a.name)).unwrap_or_default(),
        from_addr: addr,
        message_id: env.and_then(|e| e.message_id.as_deref()).map(|b| String::from_utf8_lossy(b).into_owned()),
        in_reply_to: env.and_then(|e| e.in_reply_to.as_deref()).map(|b| String::from_utf8_lossy(b).into_owned()),
        references: f.header().map(references).unwrap_or_default(),
        has_attachment: f.bodystructure().is_some_and(names::has_attachment),
        modseq: f.modseq,
    })
}

#[cfg(test)]
mod tests {
    use super::{caps, quote, uid_set};

    #[test]
    fn uid_sets_are_ranges() {
        assert_eq!(uid_set(&[5, 1, 2, 3, 9, 10, 3]), "1:3,5,9:10");
        assert_eq!(uid_set(&[7]), "7");
        assert_eq!(uid_set(&[]), "");
    }

    #[test]
    fn quoted_strings_escape() {
        assert_eq!(quote(r#"отчёт "Q3" \ итог"#), r#""отчёт \"Q3\" \\ итог""#);
        assert_eq!(quote("a\r\nb"), "\"a  b\"");
    }

    #[test]
    fn references_header() {
        let h = b"References: <a@x.test>\r\n <b@x.test>  <c@\r\n x.test>\r\n\r\n";
        assert_eq!(super::references(h), ["<a@x.test>", "<b@x.test>", "<c@x.test>"]);
        assert!(super::references(b"\r\n").is_empty());
    }

    #[test]
    fn capability_names() {
        assert_eq!(caps::name("Atom(\"IDLE\")"), "IDLE");
        assert_eq!(caps::name("Auth(\"PLAIN\")"), "AUTH=PLAIN");
        assert_eq!(caps::name("Imap4rev1"), "IMAP4rev1");
    }
}
