//! S/MIME на Linux (LIN-05, решение владельца 2026-11-12): якоря — системное хранилище корней (`/etc/ssl/certs` и
//! аналоги дистрибутивов), свои ключи — файлы PKCS#12 с паролем (токенов PKCS#11 и хранилищ нет; ГОСТ — LIN-07).
//!
//! Файлы и пароли передаёт приложение ([`load_personal_files`]): пути — из настроек, пароли — из хранилища паролей
//! системы (`pets_shell::credentials`, Secret Service); на диск ядро их не пишет. Открытые ключи живут в памяти
//! процесса до следующей загрузки. Отзыв цепочки ОС не подтверждает (нет службы, как CryptoAPI или `SecTrust`):
//! `Revocation::NotChecked`, как раньше.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use postman_smime::p12::{Identity, P12Error};
use postman_smime::sign::{SignError, own_certificate};
use postman_smime::{Anchor, Scheme, Trust};

pub(super) use super::other::{confirm, fetch_crl, gost_decrypt, gost_provider};

/// Общие хранилища корней дистрибутивов (первое найденное): почтовые (Fedora/RHEL — отдельный набор для S/MIME),
/// затем общие TLS. `SSL_CERT_FILE` — вместо них.
const BUNDLES: &[&str] = &[
    "/etc/pki/ca-trust/extracted/pem/email-ca-bundle.pem",
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/pki/tls/certs/ca-bundle.crt",
    "/etc/ssl/ca-bundle.pem",
    "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
];

/// Корни, добавленные администратором (Debian/Ubuntu, Fedora/RHEL, Arch, openSUSE) — «корпоративные».
const ADMIN_DIRS: &[&str] =
    &["/usr/local/share/ca-certificates", "/etc/pki/ca-trust/source/anchors", "/etc/ca-certificates/trust-source/anchors", "/etc/pki/trust/anchors"];

/// Запрещённые администратором (Fedora/RHEL, Arch, openSUSE).
const DENY_DIRS: &[&str] = &["/etc/pki/ca-trust/source/blocklist", "/etc/ca-certificates/trust-source/blocklist", "/etc/pki/trust/blacklisted"];

/// Файл больше — не набор корней.
const MAX_BUNDLE: u64 = 8 << 20;

/// Сертификаты из PEM (`BEGIN CERTIFICATE`) или одного DER.
fn certificates(data: &[u8]) -> Vec<Vec<u8>> {
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";
    let Ok(text) = std::str::from_utf8(data) else {
        // Не текст — один сертификат DER.
        return if data.first() == Some(&0x30) { vec![data.to_vec()] } else { Vec::new() };
    };
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(BEGIN) {
        let body = &rest[start + BEGIN.len()..];
        let Some(end) = body.find(END) else { break };
        if let Some(der) = base64(&body[..end]) {
            out.push(der);
        }
        rest = &body[end + END.len()..];
    }
    out
}

/// Base64 (RFC 4648) с пробелами и переводами строк; ошибка — `None`.
fn base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.bytes().filter(|c| !c.is_ascii_whitespace()) {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn read_limited(path: &Path) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_BUNDLE {
        return None;
    }
    std::fs::read(path).ok()
}

/// Сертификаты всех файлов каталога (без подкаталогов).
fn dir_certificates(dir: &Path) -> Vec<Vec<u8>> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    files.sort();
    files.iter().filter_map(|p| read_limited(p)).flat_map(|d| certificates(&d)).collect()
}

fn sha256_hex(der: &[u8]) -> String {
    postman_smime::Hash::Sha256.digest(der).iter().map(|b| format!("{b:02x}")).collect()
}

/// Якоря из файлов системы: `bundle` — общий набор, `admin` — добавленные администратором, `deny` — запрещённые.
fn trust_from(bundle: Vec<Vec<u8>>, admin: Vec<Vec<u8>>, deny: Vec<Vec<u8>>, corporate_extra: &[String]) -> Trust {
    let mut roots: Vec<Anchor> = Vec::new();
    let mut size = 0usize;
    for der in admin.iter().cloned().chain(bundle) {
        if deny.contains(&der) || roots.iter().any(|a| a.der == der) || size + der.len() > postman_smime::MAX_TRUST / 2 {
            continue;
        }
        size += der.len();
        let corporate = admin.contains(&der) || (!corporate_extra.is_empty() && corporate_extra.contains(&sha256_hex(&der)));
        roots.push(Anchor { der, corporate });
    }
    let mut disallowed: Vec<Vec<u8>> = Vec::new();
    for der in deny {
        if !disallowed.contains(&der) && size + der.len() <= postman_smime::MAX_TRUST {
            size += der.len();
            disallowed.push(der);
        }
    }
    // Промежуточные — из письма (воркер): отдельного хранилища CA нет.
    Trust { roots, intermediates: Vec::new(), disallowed }
}

pub(super) fn load_trust(corporate_extra: &[String]) -> Trust {
    let from_env = std::env::var_os("SSL_CERT_FILE").map(PathBuf::from);
    let bundle = from_env
        .into_iter()
        .chain(BUNDLES.iter().map(PathBuf::from))
        .find_map(|p| read_limited(&p).map(|d| certificates(&d)).filter(|c| !c.is_empty()))
        .unwrap_or_default();
    if bundle.is_empty() {
        tracing::warn!("smime: no system root certificates found");
    }
    let admin = ADMIN_DIRS.iter().flat_map(|d| dir_certificates(Path::new(d))).collect();
    let deny = DENY_DIRS.iter().flat_map(|d| dir_certificates(Path::new(d))).collect();
    trust_from(bundle, admin, deny, corporate_extra)
}

/// Открытые файлы PKCS#12: сертификаты с ключами в памяти процесса.
static PERSONAL: Mutex<Vec<Identity>> = Mutex::new(Vec::new());

/// Почему файл своего сертификата не открыт — для строки в настройках.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PersonalFileError {
    #[error("cannot read the file: {0}")]
    Read(String),
    #[error(transparent)]
    Pkcs12(#[from] P12Error),
}

fn open_file(path: &Path, password: &str) -> Result<Vec<Identity>, PersonalFileError> {
    let meta = std::fs::metadata(path).map_err(|e| PersonalFileError::Read(e.kind().to_string()))?;
    if meta.len() > postman_smime::p12::MAX_FILE as u64 {
        return Err(P12Error::Malformed("too large").into());
    }
    let data = zeroize::Zeroizing::new(std::fs::read(path).map_err(|e| PersonalFileError::Read(e.kind().to_string()))?);
    Ok(postman_smime::p12::parse(&data, password)?)
}

/// Проверить файл PKCS#12 паролем (настройки: «Добавить сертификат из файла»), ничего не запоминая: свои сертификаты
/// файла для списка.
pub fn check_personal_file(path: &Path, password: &str) -> Result<Vec<super::Personal>, PersonalFileError> {
    Ok(super::personal_from(open_file(path, password)?.into_iter().map(|i| i.cert).collect()))
}

/// Открыть свои файлы PKCS#12 (путь и пароль; пароль — из хранилища паролей, не с диска) и держать ключи в памяти
/// вместо прежних. Итог — по файлу: число сертификатов с ключом или ошибка (файл пропускается, остальные работают).
pub fn load_personal_files(files: &[(PathBuf, zeroize::Zeroizing<String>)]) -> Vec<Result<usize, PersonalFileError>> {
    let mut all = Vec::new();
    let mut out = Vec::new();
    for (path, password) in files {
        match open_file(path, password) {
            Ok(ids) => {
                out.push(Ok(ids.len()));
                all.extend(ids);
            }
            Err(e) => {
                tracing::warn!("smime: personal certificate file not opened: {e}");
                out.push(Err(e));
            }
        }
    }
    *PERSONAL.lock().unwrap_or_else(PoisonError::into_inner) = all;
    out
}

/// Забыть ключи (выход, смена настроек).
pub fn clear_personal_files() {
    PERSONAL.lock().unwrap_or_else(PoisonError::into_inner).clear();
}

fn with_personal<T>(f: impl FnOnce(&[Identity]) -> T) -> T {
    f(&PERSONAL.lock().unwrap_or_else(PoisonError::into_inner))
}

/// Свои сертификаты с ключом (для списка в настройках).
pub(super) fn personal() -> Vec<Vec<u8>> {
    with_personal(|ids| ids.iter().map(|i| i.cert.clone()).collect())
}

/// Свои сертификаты для адреса `from` — кандидаты «себе»; проверяет воркер.
pub(super) fn own_certs(from: &str) -> Vec<Vec<u8>> {
    personal()
        .into_iter()
        .filter(|der| own_certificate(der).is_some_and(|(info, _, _)| info.emails.iter().any(|e| e.eq_ignore_ascii_case(from.trim()))))
        .collect()
}

/// Лучший свой сертификат для подписи `from`: годный, действующий, RSA или ECDSA, самый поздний срок.
fn signing(ids: &[Identity], from: &str, now: i64) -> Option<(usize, Scheme)> {
    let mut best: Option<(usize, Scheme, i64)> = None;
    for (n, id) in ids.iter().enumerate() {
        let Some((info, can, Some(scheme))) = own_certificate(&id.cert) else { continue };
        let mine = info.emails.iter().any(|e| e.eq_ignore_ascii_case(from.trim()));
        let valid = info.not_before <= now && now <= info.not_after;
        if can && mine && valid && matches!(scheme, Scheme::RsaPkcs1(_) | Scheme::Ecdsa(_)) && best.is_none_or(|b| info.not_after > b.2) {
            best = Some((n, scheme, info.not_after));
        }
    }
    best.map(|(n, s, _)| (n, s))
}

pub(super) fn can_sign(from: &str, now: i64) -> bool {
    with_personal(|ids| signing(ids, from, now).is_some())
}

pub(super) fn sign_message(raw: &[u8], from: &str, now: i64) -> Result<Vec<u8>, SignError> {
    with_personal(|ids| {
        let (n, scheme) = signing(ids, from, now).ok_or_else(|| SignError::NoCertificate(from.into()))?;
        let signer = ids.get(n).map(|id| id.signer(scheme)).ok_or_else(|| SignError::NoCertificate(from.into()))?;
        postman_smime::sign::sign_message(raw, &signer, now)
    })
}

/// Снять ключ содержимого своими ключами из файлов.
pub(super) fn unwrap_cek(
    env: &postman_smime::envelope::Envelope,
    _prompt: super::keys::Prompt,
) -> Result<postman_smime::envelope::Cek, super::keys::KeyError> {
    use super::keys::KeyError;
    use postman_smime::soft::SoftError;
    let keys: Vec<_> = with_personal(|ids| ids.iter().filter_map(Identity::soft_key).collect());
    match postman_smime::soft::unwrap(&keys, env) {
        Ok(cek) => Ok(cek),
        Err(SoftError::NotForYou) => Err(KeyError::NotForYou(super::keys::labels(env))),
        Err(SoftError::Unsupported(name)) => Err(KeyError::Unsupported(name)),
    }
}

/// Случайные числа — `/dev/urandom`; эфемерный ECDH (получатели с ключом EC P-256/P-384) — `p256`/`p384` с ключом
/// из тех же случайных чисел.
pub(super) struct LinuxOps {
    random: std::fs::File,
    failed: std::sync::atomic::AtomicBool,
}

impl LinuxOps {
    pub(super) fn new() -> Result<LinuxOps, postman_smime::seal::SealError> {
        let random =
            std::fs::File::open("/dev/urandom").map_err(|e| postman_smime::seal::SealError::Failed(format!("/dev/urandom: {}", e.kind())))?;
        Ok(LinuxOps { random, failed: std::sync::atomic::AtomicBool::new(false) })
    }

    /// Случайные числа не получены — письмо не годится.
    pub(super) fn failed(&self) -> bool {
        self.failed.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl postman_smime::seal::Ops for LinuxOps {
    fn random(&self, out: &mut [u8]) {
        use std::io::Read;
        if (&self.random).read_exact(out).is_err() {
            out.fill(0);
            self.failed.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    fn ecdh(&self, bits: u16, peer: &[u8]) -> Result<(Vec<u8>, zeroize::Zeroizing<Vec<u8>>), postman_smime::seal::SealError> {
        use postman_smime::seal::Ops;
        let mut seed = zeroize::Zeroizing::new(vec![0u8; if bits == 384 { 48 } else { 32 }]);
        // Байты вне порядка кривой — вероятность ~2^-32 и меньше; несколько попыток, затем отказ.
        for _ in 0..4 {
            self.random(&mut seed);
            if self.failed() {
                break;
            }
            if let Some(r) = postman_smime::soft::ecdh_ephemeral(bits, &seed, peer) {
                return Ok(r);
            }
        }
        Err(postman_smime::seal::SealError::Failed("ECDH".into()))
    }
}

pub(super) fn seal(raw: &[u8], to: &[postman_smime::seal::To]) -> Result<Vec<u8>, postman_smime::seal::SealError> {
    let ops = LinuxOps::new()?;
    let sealed = postman_smime::seal::seal_message(raw, to, &ops)?;
    if ops.failed() {
        return Err(postman_smime::seal::SealError::Failed("no random numbers".into()));
    }
    Ok(sealed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AES: &[u8] = include_bytes!("../../../smime/src/p12/aes.p12");

    #[test]
    fn reads_pem_bundles_and_der() {
        let pem = b"junk\n-----BEGIN CERTIFICATE-----\nMIIB\nAAA=\n-----END CERTIFICATE-----\n-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----\n";
        assert_eq!(certificates(pem), vec![vec![0x30, 0x82, 0x01, 0x00, 0x00], vec![1, 2, 3]]);
        assert_eq!(certificates(&[0x30, 0x82, 0xFF]), vec![vec![0x30, 0x82, 0xFF]]);
        assert!(certificates(b"-----BEGIN CERTIFICATE-----\n#$%\n-----END CERTIFICATE-----").is_empty());
    }

    #[test]
    fn admin_roots_are_corporate_and_denied_are_out() {
        let (a, b, c) = (vec![0x30, 1], vec![0x30, 2], vec![0x30, 3]);
        let t = trust_from(vec![a.clone(), b.clone(), c.clone()], vec![b.clone()], vec![c.clone()], &[sha256_hex(&a)]);
        assert_eq!(t.roots, vec![Anchor { der: b, corporate: true }, Anchor { der: a, corporate: true }]);
        assert_eq!(t.disallowed, vec![c]);
    }

    /// Файл PKCS#12 → свой сертификат в памяти: можно подписать письмо адреса из сертификата, кандидаты «себе» — он.
    #[test]
    fn personal_file_round_trip() {
        let path = std::env::temp_dir().join(format!("postman-p12-{}.p12", std::process::id()));
        std::fs::write(&path, AES).unwrap();
        assert!(matches!(check_personal_file(&path, "x"), Err(PersonalFileError::Pkcs12(P12Error::BadPassword))));
        assert_eq!(check_personal_file(&path, "тест-пароль").unwrap().len(), 1);
        let loaded = load_personal_files(&[(path.clone(), zeroize::Zeroizing::new("тест-пароль".into()))]);
        let _ = std::fs::remove_file(&path);
        assert_eq!(loaded, vec![Ok(1)]);
        let now = 1_900_000_000;
        assert!(can_sign("User@Example.test", now));
        assert!(!can_sign("other@example.test", now));
        assert_eq!(own_certs("user@example.test").len(), 1);
        let raw = b"Content-Type: text/plain\r\n\r\nhello\r\n";
        let signed = sign_message(raw, "user@example.test", now).unwrap();
        assert!(signed.len() > raw.len());
        clear_personal_files();
        assert!(!can_sign("user@example.test", now));
    }
}
