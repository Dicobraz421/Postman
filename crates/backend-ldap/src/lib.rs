//! Каталог Active Directory — плагин ядра (ADR 0004; S5-12 Ф5, решение владельца №12,
//! `docs/decisions/2026-11-02-ldap-active-directory.md`): сертификаты адресатов для «Конфиденциально».
//!
//! Где каталог: контроллер домена компьютера — службой Windows (`DsGetDcNameW`, `sys.rs`), база поиска —
//! `defaultNamingContext` из rootDSE (AD, 389 DS — ALD Pro, FreeIPA), иначе первый раздел данных из `namingContexts`
//! (OpenLDAP); компьютер вне домена — сервер и база из настроек. Вход — учётной записью Windows (SASL GSSAPI,
//! Kerberos через SSPI): по `ldap://` — с шифрованием Kerberos, по TLS — с привязкой к каналу. Простой вход (имя и
//! пароль, в том числе служебная учётная запись LDAP-шлюза authentik) — только по TLS: `ldaps://` или StartTLS.
//! Без входа — каталог, открытый на чтение (частая настройка OpenLDAP). TLS — с проверкой сертификата Windows.
//!
//! Поиск — по `mail` и `proxyAddresses` (`smtp:`), атрибут `userCertificate` (у каталогов по RFC 4523 — с
//! `;binary`). Ответ — DER как есть: это чужие данные, их проверяет и разбирает воркер, а ждёт ответа и ограничивает
//! его ядро (`postman_core::smime::directory`). `userSMIMECertificate` (PKCS#7) — тоже как есть: сертификаты из него
//! достаёт воркер.

mod sys;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use ldap3::asn1::StructureTag;
use ldap3::{Ldap, LdapConnAsync, LdapConnSettings, LdapError, Scope, SearchOptions, ldap_escape};
use postman_core::smime::directory::{Directory, DirectoryError, TIMEOUT};
pub use zeroize::Zeroizing;

/// Атрибуты сертификата: у AD — `userCertificate`, у каталогов по RFC 4523 — с `;binary`; `userSMIMECertificate`
/// (RFC 2798) — PKCS#7 с сертификатами, их достаёт воркер (S5-12.5в).
const CERT_ATTRS: [&str; 4] = ["userCertificate", "userCertificate;binary", "userSMIMECertificate", "userSMIMECertificate;binary"];
/// Записей в ответе — не больше: сертификаты нужны адресатам одного письма.
const SIZE_LIMIT: i32 = 100;
/// Код LDAP «превышен предел записей» (RFC 4511): берём то, что пришло.
const SIZE_LIMIT_EXCEEDED: u32 = 4;

/// Настройки каталога («Настройки → Безопасность», S5-12.5б).
#[derive(Clone, Default)]
pub struct Settings {
    /// Сервер: `host`, `host:port`, `ldap://…` или `ldaps://…`; `None` — контроллер домена компьютера.
    pub server: Option<String>,
    /// База поиска; `None` — из rootDSE: `defaultNamingContext`, иначе первый раздел данных `namingContexts`.
    pub base_dn: Option<String>,
    /// StartTLS по `ldap://` (порт 389): TLS после подключения — как `ldaps://`.
    pub starttls: bool,
    pub login: Login,
}

/// Как входить в каталог.
#[derive(Clone, Default)]
pub enum Login {
    /// Учётная запись Windows (Kerberos), без пароля.
    #[default]
    Windows,
    /// Имя (DN или `user@domain`) и пароль — только по TLS: по открытому каналу пароль ушёл бы как есть.
    Simple { user: String, password: Zeroizing<String> },
    /// Без входа: каталог открыт на чтение. Подмену ответа в пути ловит воркер — сертификат без доверенной цепочки
    /// не примется.
    Anonymous,
}

/// Каталог LDAP для ядра (`postman_core::smime::directory::Directory`).
pub struct LdapDirectory {
    settings: Arc<Settings>,
}

impl LdapDirectory {
    pub fn new(settings: Settings) -> Self {
        Self { settings: Arc::new(settings) }
    }
}

impl Directory for LdapDirectory {
    fn certificates(&self, addresses: &[String]) -> Result<Vec<Vec<u8>>, DirectoryError> {
        // Контроллер домена — блокирующий вызов Windows: ядро ждёт весь вызов в своём потоке не дольше TIMEOUT.
        let target = match self.settings.server.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => Target::parse(s)?,
            None => Target::parse(&sys::domain_controller().ok_or(DirectoryError::NotConfigured)?)?,
        };
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| DirectoryError::Unavailable(e.to_string()))?;
        rt.block_on(async {
            match tokio::time::timeout(TIMEOUT, search(&self.settings, &target, addresses)).await {
                Ok(r) => r,
                Err(_) => Err(DirectoryError::Timeout),
            }
        })
    }
}

/// Контроллер домена компьютера (`DsGetDcNameW`) — для подсказки в настройках; блокирует поток.
pub fn domain_controller() -> Option<String> {
    sys::domain_controller()
}

/// Куда подключаться: адрес для `ldap3` и имя узла (SPN Kerberos — `ldap/<узел>`).
#[derive(Debug, PartialEq, Eq)]
struct Target {
    url: String,
    host: String,
    tls: bool,
}

impl Target {
    fn parse(server: &str) -> Result<Target, DirectoryError> {
        let bad = || DirectoryError::Unavailable(format!("bad directory server: {server}"));
        let server = server.trim();
        let (scheme, rest) = match server.split_once("://") {
            Some((s, r)) => (s.to_ascii_lowercase(), r),
            None => ("ldap".to_owned(), server),
        };
        let tls = match scheme.as_str() {
            "ldap" => false,
            "ldaps" => true,
            _ => return Err(bad()),
        };
        let authority = rest.split('/').next().unwrap_or_default().trim();
        let host = match authority.strip_prefix('[') {
            Some(v6) => v6.split(']').next().unwrap_or_default(),
            None => authority.rsplit_once(':').map_or(authority, |(h, _)| h),
        };
        if host.is_empty() || authority.contains(char::is_whitespace) || authority.contains('@') {
            return Err(bad());
        }
        Ok(Target { url: format!("{scheme}://{authority}"), host: host.to_owned(), tls })
    }
}

fn unavailable(e: LdapError) -> DirectoryError {
    DirectoryError::Unavailable(e.to_string())
}

/// Подключиться, войти, найти сертификаты адресов `addresses` и выйти.
async fn search(settings: &Settings, target: &Target, addresses: &[String]) -> Result<Vec<Vec<u8>>, DirectoryError> {
    let tls = target.tls || settings.starttls;
    if matches!(settings.login, Login::Simple { .. }) && !tls {
        return Err(DirectoryError::Unavailable("simple bind needs ldaps:// or StartTLS".into()));
    }
    let mut conn = LdapConnSettings::new().set_conn_timeout(TIMEOUT);
    if tls {
        conn = conn.set_config(tls_config()?).set_starttls(settings.starttls && !target.tls);
    }
    let (conn, mut ldap) = LdapConnAsync::with_settings(conn, &target.url).await.map_err(unavailable)?;
    ldap3::drive!(conn);
    let found = bound_search(&mut ldap, settings, target, addresses).await;
    // Выход — вежливость: соединение закроется и так.
    let _ = ldap.unbind().await;
    found
}

async fn bound_search(ldap: &mut Ldap, settings: &Settings, target: &Target, addresses: &[String]) -> Result<Vec<Vec<u8>>, DirectoryError> {
    match &settings.login {
        Login::Windows => {
            ldap.sasl_gssapi_bind(&target.host).await.and_then(|r| r.success()).map_err(unavailable)?;
        }
        Login::Simple { user, password } => {
            ldap.simple_bind(user, password).await.and_then(|r| r.success()).map_err(unavailable)?;
        }
        Login::Anonymous => {}
    }
    let base = match settings.base_dn.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        Some(b) => b.to_owned(),
        None => naming_context(ldap).await?,
    };
    let found = ldap
        .with_search_options(SearchOptions::new().sizelimit(SIZE_LIMIT))
        .search(&base, Scope::Subtree, &filter(addresses), CERT_ATTRS.to_vec())
        .await
        .map_err(unavailable)?;
    if found.1.rc != 0 && found.1.rc != SIZE_LIMIT_EXCEEDED {
        return Err(DirectoryError::Unavailable(format!("search failed: rc={} {}", found.1.rc, found.1.text)));
    }
    Ok(found.0.into_iter().flat_map(|e| values(e.0, &CERT_ATTRS)).collect())
}

/// База поиска по умолчанию — из rootDSE.
async fn naming_context(ldap: &mut Ldap) -> Result<String, DirectoryError> {
    let root =
        ldap.search("", Scope::Base, "(objectClass=*)", vec![DEFAULT_CONTEXT, CONTEXTS]).await.and_then(|r| r.success()).map_err(unavailable)?;
    let entry = root.0.into_iter().next().map(|e| e.0);
    entry.and_then(choose_base).ok_or_else(|| DirectoryError::Unavailable("no naming context in rootDSE".into()))
}

const DEFAULT_CONTEXT: &str = "defaultNamingContext";
const CONTEXTS: &str = "namingContexts";

/// База из записи rootDSE: `defaultNamingContext` (AD, 389 DS), иначе первый раздел данных из `namingContexts` —
/// служебные (`cn=config`, `cn=changelog`, `o=NetscapeRoot` у 389 DS) пропускаются.
fn choose_base(root: StructureTag) -> Option<String> {
    let text = |v: Vec<u8>| String::from_utf8(v).ok().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    let default = values(root.clone(), &[DEFAULT_CONTEXT]).into_iter().find_map(text);
    default.or_else(|| {
        values(root, &[CONTEXTS]).into_iter().filter_map(text).find(|c| {
            let c = c.to_ascii_lowercase();
            !c.starts_with("cn=") && c != "o=netscaperoot"
        })
    })
}

/// Фильтр поиска: адрес — в `mail` или в `proxyAddresses` как `smtp:<адрес>` (сравнение в AD — без регистра).
fn filter(addresses: &[String]) -> String {
    let mut f = String::from("(|");
    for a in addresses {
        let a = ldap_escape(a.as_str());
        f.push_str(&format!("(mail={a})(proxyAddresses=smtp:{a})"));
    }
    f.push(')');
    f
}

/// Значения атрибутов `names` (без регистра) из записи `SearchResultEntry` (RFC 4511, `[APPLICATION 4]`). Ответ
/// сервера — чужие данные: разбор без паник, лишнее пропускается.
fn values(entry: StructureTag, names: &[&str]) -> Vec<Vec<u8>> {
    let Some(mut parts) = entry.match_id(4).and_then(StructureTag::expect_constructed).map(Vec::into_iter) else {
        return Vec::new();
    };
    let Some(attrs) = parts.nth(1).and_then(StructureTag::expect_constructed) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for attr in attrs {
        let Some(mut attr) = attr.expect_constructed().map(Vec::into_iter) else { continue };
        let Some(name) = attr.next().and_then(StructureTag::expect_primitive) else { continue };
        if !names.iter().any(|n| n.as_bytes().eq_ignore_ascii_case(&name)) {
            continue;
        }
        let Some(vals) = attr.next().and_then(StructureTag::expect_constructed) else { continue };
        out.extend(vals.into_iter().filter_map(StructureTag::expect_primitive));
    }
    out
}

/// TLS с проверкой сертификата Windows (как у IMAP); провайдер — aws-lc-rs, как у reqwest.
fn tls_config() -> Result<Arc<rustls::ClientConfig>, DirectoryError> {
    use rustls_platform_verifier::BuilderVerifierExt as _;
    let tls = |e: rustls::Error| DirectoryError::Unavailable(e.to_string());
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone()).with_safe_default_protocol_versions().map_err(tls)?;
    #[cfg(debug_assertions)]
    if let Some(root) = stand_root() {
        let verifier = rustls_platform_verifier::Verifier::new_with_extra_roots([root], provider).map_err(tls)?;
        return Ok(Arc::new(builder.dangerous().with_custom_certificate_verifier(Arc::new(verifier)).with_no_client_auth()));
    }
    Ok(Arc::new(builder.with_platform_verifier().map_err(tls)?.with_no_client_auth()))
}

/// Только отладочная сборка, сцены `drill_net`: корень стенда (DER, файл `POSTMAN_TEST_CA`) рядом с корнями Windows. Цепочку и
/// имя сервера по-прежнему проверяет Windows; в выпуске этого кода нет, хранилище ОС не трогается (`docs/DEV.md`, «Стенд сети»).
#[cfg(debug_assertions)]
fn stand_root() -> Option<rustls::pki_types::CertificateDer<'static>> {
    std::fs::read(std::env::var_os("POSTMAN_TEST_CA")?).ok().map(Into::into)
}
