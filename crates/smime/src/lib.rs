//! S/MIME (RFC 8551): проверка подписи входящих писем — S5-12 Ф1 (`docs/SMIME.md`).
//!
//! Чужие данные: разбор и проверка идут **только в воркере-песочнице** (решение владельца №1, вариант B). Крейт без
//! I/O и без Windows: вход — байты письма и якоря доверия, выход — [`Report`] (итог, сводка сертификатов, DER
//! цепочки). Подтверждение цепочки и отзыв — в ядре (`postman_core::smime`, решение №3).
//!
//! Что умеет: `multipart/signed` (отдельная подпись, канонизация CRLF) и `application/pkcs7-mime;
//! smime-type=signed-data` (подпись внутри); SignedData с подписью RSA PKCS#1 v1.5 и PSS, ECDSA P-256/P-384,
//! ГОСТ Р 34.10-2012 (256 и 512 бит, [`gost`] — **не сертифицировано**); хэши SHA-256/384/512, «Стрибог» 256/512,
//! SHA-1 — «слабая» (решение №10); цепочка — `rustls-webpki` к переданным якорям, EKU «Защита почты», адрес
//! сертификата ↔ «От»; `SMIMECapabilities`.
//!
//! Ф2 — расшифровка ([`envelope`]): получатели и шифр — ядру, содержимое по ключу ядра — здесь же, в воркере.
//! Отзыв ГОСТ без СКЗИ (12.1д) — [`crl`]: подпись CRL и поиск серийного номера, CRL скачивает ядро.

mod algs;
mod ber;
mod chain;
pub mod crl;
mod ec;
pub mod envelope;
pub mod gost;
mod mime;
pub mod oid;
pub mod prepare;
pub mod seal;
pub mod sign;
mod signed;
#[cfg(feature = "soft")]
pub mod soft;
pub mod wire;

#[cfg(feature = "testkit")]
pub mod testkit;

use serde::{Deserialize, Serialize};

pub use algs::{Hash, KeyKind, Scheme};

/// Больше письма не проверяем: кадр воркера — 64 МБ, вместе с якорями.
pub const MAX_MESSAGE: usize = 62 * 1024 * 1024;
/// Предел якорей и промежуточных за один запрос.
pub const MAX_TRUST: usize = 1536 * 1024;

/// Якорь доверия (корневой сертификат).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor {
    pub der: Vec<u8>,
    /// Корпоративный CA (решение владельца №2): метка «корпоративный» в карточке.
    pub corporate: bool,
}

/// Чему верить: корни (Windows `Root`), промежуточные (`CA`), запрещённые (`Disallowed`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Trust {
    pub roots: Vec<Anchor>,
    pub intermediates: Vec<Vec<u8>>,
    pub disallowed: Vec<Vec<u8>>,
}

/// Вид подписи в письме.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    /// `multipart/signed` + `smime.p7s`.
    Detached,
    /// `application/pkcs7-mime; smime-type=signed-data` (`smime.p7m`).
    Opaque,
    /// `application/pkcs7-mime; smime-type=enveloped-data` — зашифровано (расшифровка — [`envelope`]).
    Encrypted,
    /// Не S/MIME.
    None,
}

/// Итог проверки (главное — для значка). Порядок важности: чем выше в списке «плохих», тем раньше показывается.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    /// Подпись верна, цепочка доведена до доверенного якоря.
    Valid,
    /// Подпись верна, но издатель не доверен (цепочка не дошла до якоря); причина — для карточки.
    Untrusted(String),
    /// Подпись верна, сертификат подписанта (или CA) вне срока действия.
    Expired,
    /// Сертификат не для подписи почты (EKU без «Защиты почты», KeyUsage без `digitalSignature`).
    WrongUsage,
    /// Сертификат цепочки — в «Недоверенных» Windows (`Disallowed`).
    Distrusted,
    /// Содержимое изменено или подпись не этим ключом.
    Invalid,
    /// В письме нет сертификата подписанта.
    NoCertificate,
    /// Алгоритм не поддерживается (имя или OID).
    Unsupported(String),
    /// Подпись повреждена (DER, MIME).
    Malformed(String),
    /// Подписи нет.
    NotSigned,
}

impl Status {
    /// Подпись математически верна (значит, цепочку можно показать и подтвердить в Windows).
    pub fn signature_ok(&self) -> bool {
        matches!(self, Status::Valid | Status::Untrusted(_) | Status::Expired | Status::WrongUsage | Status::Distrusted)
    }
}

/// Сводка сертификата для карточки.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertInfo {
    /// CN (или полное имя, если CN нет).
    pub subject: String,
    pub organization: Option<String>,
    /// Адреса: SAN `rfc822Name` и `emailAddress` в имени.
    pub emails: Vec<String>,
    /// CN издателя (или его полное имя).
    pub issuer: String,
    pub issuer_organization: Option<String>,
    /// Серийный номер, hex.
    pub serial: String,
    /// Unix-время, секунды.
    pub not_before: i64,
    pub not_after: i64,
    /// SHA-256 DER, hex заглавными.
    pub fingerprint: String,
    pub key: KeyKind,
}

/// Итог проверки письма.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub kind: Kind,
    pub status: Status,
    /// Схема подписи (если SignerInfo разобран).
    pub scheme: Option<Scheme>,
    /// SHA-1 или RSA короче 2048 бит: подпись «слабая» (решение №10).
    pub weak: bool,
    /// Адрес сертификата совпадает с «От» (`from`); без сертификата — `false`.
    pub address_ok: bool,
    /// Адрес «От» письма (как его видит воркер).
    pub from: String,
    /// `signingTime`, Unix-время.
    pub signing_time: Option<i64>,
    /// Цепочка: подписант, промежуточные, корень (корень — если цепочка дошла до якоря).
    pub certs: Vec<CertInfo>,
    /// Корень цепочки — корпоративный CA (решение №2).
    pub corporate: bool,
    /// Шифры из `SMIMECapabilities` (OID) — для выбора шифра исходящих (Ф4).
    pub capabilities: Vec<String>,
    /// DER цепочки (как в `certs`); идёт в ядро для `CertGetCertificateChain`. В JSON не входит — бинарная часть.
    #[serde(skip)]
    pub chain: Vec<Vec<u8>>,
    /// Подписанное содержимое `signed-data` (внутренний MIME) — для показа тела в воркере. В JSON не входит.
    #[serde(skip)]
    pub content: Option<Vec<u8>>,
}

impl Report {
    fn new(kind: Kind, status: Status) -> Self {
        Report {
            kind,
            status,
            scheme: None,
            weak: false,
            address_ok: false,
            from: String::new(),
            signing_time: None,
            certs: Vec::new(),
            corporate: false,
            capabilities: Vec::new(),
            chain: Vec::new(),
            content: None,
        }
    }

    /// Подпись ГОСТ (отзыв в Windows без СКЗИ не проверить).
    pub fn is_gost(&self) -> bool {
        self.scheme.is_some_and(Scheme::is_gost)
    }

    /// Цепочка дошла до доверенного якоря.
    pub fn trusted(&self) -> bool {
        matches!(self.status, Status::Valid)
    }
}

/// Признак S/MIME по метаданным части (тип и имя вложения из JMAP): подпись отдельно (`smime.p7s`) или
/// `smime.p7m` (подпись внутри или шифрование — видно только по содержимому).
pub fn is_smime_part(mime: &str, name: &str) -> bool {
    let mime = mime.to_ascii_lowercase();
    let name = name.to_ascii_lowercase();
    matches!(mime.as_str(), "application/pkcs7-signature" | "application/x-pkcs7-signature" | "application/pkcs7-mime" | "application/x-pkcs7-mime")
        || (mime == "application/octet-stream" && (name.ends_with(".p7s") || name.ends_with(".p7m")))
}

/// Тело подписанного содержимого `signed-data` (внутренний MIME): HTML и текст — для показа письма вместо пустого
/// тела (сервер видит только `smime.p7m`). Только в воркере.
pub fn inner_body(content: &[u8]) -> (Option<String>, Option<String>) {
    let Some(msg) = mail_parser::MessageParser::default().parse(content) else { return (None, None) };
    let html = msg.body_html(0).map(|h| h.into_owned());
    let text = msg.body_text(0).map(|t| t.into_owned());
    (html, text)
}

/// Проверить подпись письма `message` (RFC 5322 как пришло) к якорям `trust` на момент `now` (Unix, секунды).
/// Не паникует: любой сбой разбора — [`Status::Malformed`].
pub fn verify(message: &[u8], trust: &Trust, now: u64) -> Report {
    if message.len() > MAX_MESSAGE {
        return Report::new(Kind::None, Status::Malformed("message too large".into()));
    }
    // Сбой разборщика на чужих данных — итог «повреждено», а не падение воркера.
    match std::panic::catch_unwind(|| verify_inner(message, trust, now)) {
        Ok(r) => r,
        Err(_) => Report::new(Kind::None, Status::Malformed("parser failure".into())),
    }
}

fn verify_inner(message: &[u8], trust: &Trust, now: u64) -> Report {
    let found = match mime::find(message) {
        Ok(f) => f,
        Err(e) => return Report::new(Kind::None, Status::Malformed(e)),
    };
    let (kind, cms, content, from) = match found {
        mime::Found::None { from } => {
            let mut r = Report::new(Kind::None, Status::NotSigned);
            r.from = from;
            return r;
        }
        mime::Found::Encrypted { from, .. } => {
            let mut r = Report::new(Kind::Encrypted, Status::NotSigned);
            r.from = from;
            return r;
        }
        mime::Found::Detached { signed, signature, from } => (Kind::Detached, signature, Some(signed), from),
        mime::Found::Opaque { cms, from } => (Kind::Opaque, cms, None, from),
    };
    let mut report = Report::new(kind, Status::NotSigned);
    report.from = from;
    let checked = match signed::check(&cms, content.as_deref()) {
        Ok(c) => c,
        Err(status) => {
            report.status = status;
            return report;
        }
    };
    report.scheme = Some(checked.scheme);
    report.signing_time = checked.signing_time;
    report.capabilities = checked.capabilities.clone();
    report.weak = checked.scheme.hash().weak() || checked.weak_key;
    if kind == Kind::Opaque {
        report.content = checked.content.clone();
    }
    chain::evaluate(&mut report, &checked, trust, now);
    report
}
