//! macOS (MAC-05): свои ключи S/MIME — удостоверения (`SecIdentity`: сертификат + закрытый ключ) Связки ключей
//! пользователя и смарт-карт (CryptoTokenKit). Аналог `cng.rs`, `sign_win.rs` и `seal_win.rs`.
//!
//! Ключ не покидает Связку ключей: RSA (PKCS#1 v1.5, OAEP), ECDH и подпись RSA/ECDSA — `SecKey`. Сертификаты свои
//! (Связка ключей — данные машины, не письма): издатель, серийный номер и SKI для сравнения с получателем берутся из
//! DER своего сертификата ([`postman_smime::sign::own_rid`]), чужой DER здесь не разбирается.
//!
//! «Молча» (открытие письма, `Prompt::Silent`): на время операции окна Связки ключей запрещены
//! (`SecKeychainSetUserInteractionAllowed`) — нужен пароль или PIN — `NeedPin` («Ввести» повторит с окном ОС).

use std::sync::{Mutex, PoisonError};

use core_foundation::base::TCFType;
use core_foundation::error::CFError;
use postman_smime::envelope::{Cek, Envelope, Rid, Wrap};
use postman_smime::seal::{Ops, SealError};
use postman_smime::sign::{SignError, Signer, own_certificate, own_rid};
use postman_smime::{Hash, Scheme};
use security_framework::certificate::SecCertificate;
use security_framework::identity::SecIdentity;
use security_framework::item::{ItemClass, ItemSearchOptions, Limit, Reference, SearchResult};
use security_framework::key::{Algorithm, GenerateKeyOptions, KeyType, SecKey};
use security_framework::os::macos::keychain::SecKeychain;
use security_framework::policy::SecPolicy;
use security_framework::trust::SecTrust;
use zeroize::Zeroizing;

use super::keys::{KeyError, Prompt, labels, supported};

const ERR_SEC_USER_CANCELED: isize = -128;
const ERR_SEC_AUTH_FAILED: isize = -25293;
const ERR_SEC_INTERACTION_NOT_ALLOWED: isize = -25308;
const ERR_SEC_INTERACTION_REQUIRED: isize = -25315;
/// CryptoTokenKit (`TKErrorDomain`): отмена, токена нет, нужна или не прошла проверка PIN.
const TK_CANCELED: isize = -4;
const TK_AUTH_FAILED: isize = -5;
const TK_TOKEN_NOT_FOUND: isize = -7;
const TK_AUTH_NEEDED: isize = -9;
/// Сертификатов «других пользователей» из Связки ключей — не больше.
const OTHERS_MAX: usize = 1000;

/// Связки ключей для поиска: `None` — список поиска пользователя и смарт-карты; тесты — временная Связка.
pub(crate) type Keychains<'a> = Option<&'a [SecKeychain]>;

/// Что сказала Связка ключей.
enum OsError {
    NeedPin,
    NoCard,
    Cancelled,
    Failed(String),
}

fn os_error(e: &CFError, what: &str) -> OsError {
    let code = e.code();
    let token = e.domain().to_string().contains("CryptoTokenKit");
    match code {
        ERR_SEC_INTERACTION_NOT_ALLOWED | ERR_SEC_INTERACTION_REQUIRED | ERR_SEC_AUTH_FAILED => OsError::NeedPin,
        ERR_SEC_USER_CANCELED => OsError::Cancelled,
        TK_CANCELED if token => OsError::Cancelled,
        TK_TOKEN_NOT_FOUND if token => OsError::NoCard,
        TK_AUTH_NEEDED | TK_AUTH_FAILED if token => OsError::NeedPin,
        _ => OsError::Failed(format!("{what}: {} {code}", e.domain())),
    }
}

fn key_error(e: &CFError, what: &str) -> KeyError {
    match os_error(e, what) {
        OsError::NeedPin => KeyError::NeedPin,
        OsError::NoCard => KeyError::NoCard,
        OsError::Cancelled => KeyError::Cancelled,
        OsError::Failed(s) => KeyError::Failed(s),
    }
}

fn sign_error(e: &CFError, what: &str) -> SignError {
    match os_error(e, what) {
        OsError::NeedPin => SignError::NeedPin,
        OsError::NoCard => SignError::NoCard,
        OsError::Cancelled => SignError::Cancelled,
        OsError::Failed(s) => SignError::Failed(s),
    }
}

/// «Молча»: окна Связки ключей запрещены до конца области (флаг процесса — поэтому под замком).
struct Quiet<'a>(Option<std::sync::MutexGuard<'a, ()>>);

fn quiet(silent: bool) -> Quiet<'static> {
    static LOCK: Mutex<()> = Mutex::new(());
    if !silent {
        return Quiet(None);
    }
    let guard = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    // SAFETY: флаг процесса; возвращается в `Drop`.
    #[allow(deprecated)]
    unsafe {
        security_framework_sys::keychain::SecKeychainSetUserInteractionAllowed(0)
    };
    Quiet(Some(guard))
}

impl Drop for Quiet<'_> {
    fn drop(&mut self) {
        if self.0.is_some() {
            // SAFETY: см. `quiet`.
            #[allow(deprecated)]
            unsafe {
                security_framework_sys::keychain::SecKeychainSetUserInteractionAllowed(1)
            };
        }
    }
}

/// Свои удостоверения (сертификат DER и удостоверение), без повторов.
pub(crate) fn identities(keychains: Keychains<'_>) -> Vec<(Vec<u8>, SecIdentity)> {
    let mut found: Vec<SearchResult> = Vec::new();
    let mut search = |token: bool| {
        let mut o = ItemSearchOptions::new();
        o.class(ItemClass::identity()).load_refs(true).limit(Limit::All);
        if let Some(k) = keychains {
            o.keychains(k);
        }
        if token {
            o.access_group_token();
        }
        // Нет ни одного — `errSecItemNotFound`: пусто.
        if let Ok(r) = o.search() {
            found.extend(r);
        }
    };
    search(false);
    if keychains.is_none() {
        // Смарт-карты (CryptoTokenKit) [проверить с картой на M1].
        search(true);
    }
    let mut out: Vec<(Vec<u8>, SecIdentity)> = Vec::new();
    for r in found {
        let SearchResult::Ref(Reference::Identity(id)) = r else { continue };
        let Ok(cert) = id.certificate() else { continue };
        let der = cert.to_der();
        if !out.iter().any(|(d, _)| *d == der) {
            out.push((der, id));
        }
    }
    out
}

/// Сертификаты Связки ключей (без закрытых ключей тоже) — DER.
fn certificates(keychains: Keychains<'_>) -> Vec<Vec<u8>> {
    let mut o = ItemSearchOptions::new();
    o.class(ItemClass::certificate()).load_refs(true).limit(Limit::All);
    if let Some(k) = keychains {
        o.keychains(k);
    }
    let Ok(found) = o.search() else { return Vec::new() };
    let mut out: Vec<Vec<u8>> = Vec::new();
    for r in found {
        if let SearchResult::Ref(Reference::Certificate(c)) = r {
            let der = c.to_der();
            if !out.contains(&der) {
                out.push(der);
            }
        }
    }
    out
}

/// Случайные байты: `SecRandomCopyBytes`, запасной путь — генератор ядра; оба отказали — ошибка (шифровать нельзя).
fn random(out: &mut [u8]) -> Result<(), SealError> {
    if security_framework::random::SecRandom::default().copy_bytes(out).is_ok() {
        return Ok(());
    }
    use std::io::Read;
    if std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(out)).is_err() {
        out.fill(0);
        tracing::error!("smime: no system random source");
        return Err(SealError::Failed("no system random source".into()));
    }
    Ok(())
}

/// Открытый ключ EC из точки `04‖X‖Y` (свой временный ключ Security.framework).
fn ec_public(point: &[u8]) -> Result<SecKey, String> {
    use core_foundation::data::CFData;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;
    use security_framework_sys::item::{kSecAttrKeyClass, kSecAttrKeyClassPublic, kSecAttrKeyType, kSecAttrKeyTypeECSECPrimeRandom};
    let data = CFData::from_buffer(point);
    // SAFETY: константы Security.framework (правило Get).
    let attrs = unsafe {
        CFDictionary::from_CFType_pairs(&[
            (CFString::wrap_under_get_rule(kSecAttrKeyType), CFString::wrap_under_get_rule(kSecAttrKeyTypeECSECPrimeRandom)),
            (CFString::wrap_under_get_rule(kSecAttrKeyClass), CFString::wrap_under_get_rule(kSecAttrKeyClassPublic)),
        ])
    };
    let mut err: core_foundation::error::CFErrorRef = std::ptr::null_mut();
    // SAFETY: данные и словарь живут до конца вызова; ошибку освобождает `CFError`.
    let key = unsafe { security_framework_sys::key::SecKeyCreateWithData(data.as_concrete_TypeRef(), attrs.as_concrete_TypeRef(), &mut err) };
    if key.is_null() {
        let e = if err.is_null() {
            "SecKeyCreateWithData".to_owned()
        } else {
            // SAFETY: ошибка создана вызовом (правило Create).
            let e = unsafe { CFError::wrap_under_create_rule(err) };
            format!("SecKeyCreateWithData: {}", e.code())
        };
        return Err(e);
    }
    // SAFETY: ключ создан выше (правило Create).
    Ok(unsafe { SecKey::wrap_under_create_rule(key) })
}

fn oaep(h: Hash) -> Option<Algorithm> {
    Some(match h {
        Hash::Sha1 => Algorithm::RSAEncryptionOAEPSHA1,
        Hash::Sha256 => Algorithm::RSAEncryptionOAEPSHA256,
        Hash::Sha384 => Algorithm::RSAEncryptionOAEPSHA384,
        Hash::Sha512 => Algorithm::RSAEncryptionOAEPSHA512,
        _ => return None,
    })
}

/// Снять ключ содержимого ключом `key` (тесты зовут с ключом временной Связки).
pub(crate) fn decrypt_with(key: &SecKey, wrap: &Wrap, data: &[u8], silent: bool, key_len: usize) -> Result<Cek, KeyError> {
    let _quiet = quiet(silent);
    match wrap {
        Wrap::RsaPkcs1 | Wrap::RsaOaep(_) => {
            let alg = match wrap {
                Wrap::RsaOaep(h) => oaep(*h).ok_or_else(|| KeyError::Unsupported(format!("RSA-OAEP {h:?}")))?,
                _ => Algorithm::RSAEncryptionPKCS1,
            };
            match key.decrypt_data(alg, data) {
                Ok(v) => Ok(Zeroizing::new(v)),
                Err(e) => match key_error(&e, "SecKeyCreateDecryptedData") {
                    // Заполнение PKCS#1 не снялось: случайный ключ — одна ошибка с неверным ключом содержимого (RFC 3218).
                    KeyError::Failed(_) if matches!(wrap, Wrap::RsaPkcs1) => {
                        let mut cek = Zeroizing::new(vec![0u8; key_len]);
                        random(&mut cek).map_err(|e| KeyError::Failed(e.to_string()))?;
                        Ok(cek)
                    }
                    e => Err(e),
                },
            }
        }
        Wrap::Ecdh { ephemeral, .. } => {
            let size = match ephemeral.len() {
                65 => 32,
                97 => 48,
                133 => 66,
                _ => return Err(KeyError::Unsupported("ECDH curve".into())),
            };
            if ephemeral.first() != Some(&4) {
                return Err(KeyError::Unsupported("ECDH point".into()));
            }
            let peer = ec_public(ephemeral).map_err(KeyError::Failed)?;
            // `Standard` — сырой общий секрет (X, big-endian), KDF X9.63 — свой (`ecdh_cek`).
            let z = Zeroizing::new(
                key.key_exchange(Algorithm::ECDHKeyExchangeStandard, &peer, size, None).map_err(|e| key_error(&e, "SecKeyCopyKeyExchangeResult"))?,
            );
            postman_smime::envelope::ecdh_cek(&z, wrap, data).ok_or_else(|| KeyError::Failed("ECDH: key unwrap".into()))
        }
        Wrap::Gost(_) => Err(KeyError::Unsupported("ГОСТ — через СКЗИ".into())),
        Wrap::Unsupported(n) => Err(KeyError::Unsupported(n.clone())),
    }
}

/// Свой сертификат `der` подходит получателю.
fn matches(der: &[u8], rid: &Rid) -> bool {
    let Some(own) = own_rid(der) else { return false };
    match rid {
        Rid::IssuerSerial { issuer, serial } => {
            let le: Vec<u8> = own.serial.iter().rev().copied().collect();
            own.issuer == *issuer && postman_smime::envelope::serial_matches(serial, &le)
        }
        Rid::Ski(ski) => !ski.is_empty() && own.ski.as_deref() == Some(ski.as_slice()),
    }
}

/// Снять ключ содержимого своими удостоверениями Связки ключей.
pub(crate) fn unwrap_cek(env: &Envelope, prompt: Prompt) -> Result<Cek, KeyError> {
    unwrap_in(None, env, prompt)
}

pub(crate) fn unwrap_in(keychains: Keychains<'_>, env: &Envelope, prompt: Prompt) -> Result<Cek, KeyError> {
    let key_len = env.cipher.as_ref().map(|c| c.key_len()).unwrap_or(32);
    let mut result: Option<Result<Cek, KeyError>> = None;
    for (der, id) in identities(keychains) {
        for r in env.recipients.iter().filter(|r| matches(&der, &r.rid)) {
            if !supported(&r.wrap) {
                result.get_or_insert(Err(KeyError::Unsupported(match &r.wrap {
                    Wrap::Unsupported(n) => n.clone(),
                    _ => "?".into(),
                })));
                continue;
            }
            let attempt = id
                .private_key()
                .map_err(|e| KeyError::Failed(format!("SecIdentityCopyPrivateKey: {}", e.code())))
                .and_then(|k| decrypt_with(&k, &r.wrap, &r.encrypted_key, prompt == Prompt::Silent, key_len));
            match attempt {
                Ok(cek) => return Ok(cek),
                // Нужен PIN или карта — это и показать (важнее «не для вас»).
                Err(e @ (KeyError::NeedPin | KeyError::NoCard | KeyError::Cancelled)) => result = Some(Err(e)),
                Err(e) => {
                    tracing::warn!("S/MIME key: {e:?}");
                    result.get_or_insert(Err(e));
                }
            }
        }
    }
    result.unwrap_or_else(|| Err(KeyError::NotForYou(labels(env))))
}

/// Подписант Связки ключей: сертификат, промежуточные, закрытый ключ.
pub(crate) struct KeychainSigner {
    pub(crate) cert: Vec<u8>,
    pub(crate) chain: Vec<Vec<u8>>,
    pub(crate) scheme: Scheme,
    pub(crate) key: SecKey,
    pub(crate) silent: bool,
}

impl Signer for KeychainSigner {
    fn certificate(&self) -> &[u8] {
        &self.cert
    }

    fn chain(&self) -> &[Vec<u8>] {
        &self.chain
    }

    fn scheme(&self) -> Scheme {
        self.scheme
    }

    fn sign(&self, digest: &[u8]) -> Result<Vec<u8>, SignError> {
        let alg = match self.scheme {
            Scheme::RsaPkcs1(Hash::Sha1) => Algorithm::RSASignatureDigestPKCS1v15SHA1,
            Scheme::RsaPkcs1(Hash::Sha384) => Algorithm::RSASignatureDigestPKCS1v15SHA384,
            Scheme::RsaPkcs1(Hash::Sha512) => Algorithm::RSASignatureDigestPKCS1v15SHA512,
            Scheme::RsaPkcs1(_) => Algorithm::RSASignatureDigestPKCS1v15SHA256,
            // ECDSA X9.62 — подпись уже в DER `ECDSA-Sig-Value`.
            Scheme::Ecdsa(Hash::Sha384) => Algorithm::ECDSASignatureDigestX962SHA384,
            Scheme::Ecdsa(Hash::Sha512) => Algorithm::ECDSASignatureDigestX962SHA512,
            Scheme::Ecdsa(_) => Algorithm::ECDSASignatureDigestX962SHA256,
            _ => return Err(SignError::Failed("Keychain: scheme is not supported".into())),
        };
        let _quiet = quiet(self.silent);
        self.key.create_signature(alg, digest).map_err(|e| sign_error(&e, "SecKeyCreateSignature"))
    }
}

/// Промежуточные сертификаты своего сертификата (цепочка `SecTrust` без сети), без корня.
fn intermediates(der: &[u8]) -> Vec<Vec<u8>> {
    let Ok(cert) = SecCertificate::from_der(der) else { return Vec::new() };
    let Ok(mut trust) = SecTrust::create_with_certificates(&[cert], &[SecPolicy::create_x509()]) else { return Vec::new() };
    let _ = trust.set_network_fetch_allowed(false);
    // Цепочка строится и при недоверенном корне; итог оценки здесь не важен.
    let _ = trust.evaluate_with_error();
    let mut out = Vec::new();
    for c in trust.chain().into_iter().skip(1) {
        if c.issuer() == c.subject() {
            continue; // корень
        }
        out.push(c.to_der());
    }
    out
}

/// Лучшее удостоверение для подписи адреса `from`: годное, действующее, самый поздний срок.
fn signing_identity(keychains: Keychains<'_>, from: &str, now: i64) -> Option<(Vec<u8>, SecIdentity, Scheme)> {
    let mut best: Option<(Vec<u8>, SecIdentity, Scheme, i64)> = None;
    for (der, id) in identities(keychains) {
        let Some((info, can, Some(scheme))) = own_certificate(&der) else { continue };
        let mine = info.emails.iter().any(|e| e.eq_ignore_ascii_case(from.trim()));
        let valid = info.not_before <= now && now <= info.not_after;
        if can && mine && valid && best.as_ref().is_none_or(|b| info.not_after > b.3) {
            best = Some((der, id, scheme, info.not_after));
        }
    }
    best.map(|(d, i, s, _)| (d, i, s))
}

/// Есть ли свой сертификат подписи для адреса `from` (годный, действующий, с ключом) — без доступа к ключу.
pub(crate) fn can_sign(from: &str, now: i64) -> bool {
    signing_identity(None, from, now).is_some()
}

/// Подписать письмо `raw` сертификатом адреса `from` из Связки ключей; `silent` — без окон ОС.
pub(crate) fn sign_message(raw: &[u8], from: &str, silent: bool, now: i64) -> Result<Vec<u8>, SignError> {
    sign_in(None, raw, from, silent, now)
}

pub(crate) fn sign_in(keychains: Keychains<'_>, raw: &[u8], from: &str, silent: bool, now: i64) -> Result<Vec<u8>, SignError> {
    let Some((cert, id, scheme)) = signing_identity(keychains, from, now) else { return Err(SignError::NoCertificate(from.into())) };
    if matches!(scheme, Scheme::Gost256 | Scheme::Gost512) {
        // СКЗИ на macOS нет (ГОСТ отложен).
        return Err(SignError::NeedSkzi);
    }
    let key = id.private_key().map_err(|e| SignError::Failed(format!("SecIdentityCopyPrivateKey: {}", e.code())))?;
    let chain = intermediates(&cert);
    postman_smime::sign::sign_message(raw, &KeychainSigner { cert, chain, scheme, key, silent }, now)
}

/// Случайные числа и эфемерный ECDH — Security.framework.
pub(crate) struct KeychainOps;

impl Ops for KeychainOps {
    fn random(&self, out: &mut [u8]) -> Result<(), SealError> {
        random(out)
    }

    fn ecdh(&self, bits: u16, peer: &[u8]) -> Result<(Vec<u8>, Zeroizing<Vec<u8>>), SealError> {
        let size = if bits == 384 { 48usize } else { 32 };
        if peer.len() != 1 + 2 * size || peer[0] != 4 {
            return Err(SealError::Failed("EC point".into()));
        }
        let peer = ec_public(peer).map_err(SealError::Failed)?;
        let mut o = GenerateKeyOptions::default();
        // Без `location` ключ временный: в Связку ключей не попадает.
        o.set_key_type(KeyType::ec_sec_prime_random()).set_size_in_bits(u32::from(if bits == 384 { 384u16 } else { 256 }));
        let eph = SecKey::new(&o).map_err(|e| SealError::Failed(format!("SecKeyCreateRandomKey: {}", e.code())))?;
        let own = eph
            .public_key()
            .and_then(|k| k.external_representation())
            .map(|d| d.to_vec())
            .ok_or_else(|| SealError::Failed("EC public key".into()))?;
        let z = eph
            .key_exchange(Algorithm::ECDHKeyExchangeStandard, &peer, size, None)
            .map_err(|e| SealError::Failed(format!("SecKeyCopyKeyExchangeResult: {}", e.code())))?;
        Ok((own, Zeroizing::new(z)))
    }
}

/// Свои сертификаты для адреса `from` (удостоверения Связки ключей) — кандидаты «себе»; проверяет воркер.
pub(crate) fn own_certs(from: &str) -> Vec<Vec<u8>> {
    identities(None)
        .into_iter()
        .map(|(der, _)| der)
        .filter(|der| own_certificate(der).is_some_and(|(info, _, _)| info.emails.iter().any(|e| e.eq_ignore_ascii_case(from.trim()))))
        .collect()
}

/// Сертификаты других людей из Связки ключей (аналог «Других пользователей» Windows): с адресом, без своих
/// удостоверений; адреса и годность узнает воркер.
pub(crate) fn address_book() -> Vec<Vec<u8>> {
    let own: Vec<Vec<u8>> = identities(None).into_iter().map(|(d, _)| d).collect();
    certificates(None)
        .into_iter()
        .filter(|d| !own.contains(d) && own_certificate(d).is_some_and(|(info, _, _)| !info.emails.is_empty()))
        .take(OTHERS_MAX)
        .collect()
}

/// Свои сертификаты с закрытым ключом (для списка в настройках).
pub(crate) fn personal(keychains: Keychains<'_>) -> Vec<Vec<u8>> {
    identities(keychains).into_iter().map(|(d, _)| d).collect()
}

#[cfg(test)]
mod tests {
    //! Полный путь во временной Связке ключей (`security create-keychain`, удаляется в `Drop`): тестовые сертификат
    //! и ключ на `example.test` → подпись и расшифровка туда и обратно. Список поиска пользователя не меняется.
    use super::*;
    use postman_smime::envelope::Cipher;
    use postman_smime::testkit::{self, Head, Key, KeyAlg, NOW, Pki, To};
    use postman_smime::{Kind, Status};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    struct TempKeychain {
        path: PathBuf,
        _dir: tempfile::TempDir,
        keychain: SecKeychain,
    }

    impl Drop for TempKeychain {
        fn drop(&mut self) {
            let _ = Command::new("security").arg("delete-keychain").arg(&self.path).status();
        }
    }

    fn security(args: &[&str], path: &Path) {
        let out = Command::new("security").args(args).arg(path).output().unwrap();
        assert!(out.status.success(), "security {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// Временная Связка с паролем; импорт DER-файлов (сертификаты и ключи), доступ к ключам — без окон.
    fn keychain(files: &[(&str, &str, &[u8])]) -> TempKeychain {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("postman-test.keychain-db");
        let pw = "postman-test";
        security(&["create-keychain", "-p", pw], &path);
        security(&["set-keychain-settings"], &path);
        security(&["unlock-keychain", "-p", pw], &path);
        for (name, kind, data) in files {
            let f = dir.path().join(name);
            std::fs::write(&f, data).unwrap();
            let f = f.to_string_lossy().into_owned();
            let mut args = vec!["import", f.as_str(), "-A", "-t", kind];
            if *kind == "priv" {
                args.extend(["-f", "openssl"]);
            }
            args.push("-k");
            security(&args, &path);
        }
        security(&["set-key-partition-list", "-S", "apple-tool:,apple:,unsigned:", "-s", "-k", pw], &path);
        let keychain = SecKeychain::open(&path).unwrap();
        TempKeychain { path, _dir: dir, keychain }
    }

    fn pem(label: &str, der: &[u8]) -> Vec<u8> {
        format!("-----BEGIN {label}-----\n{}\n-----END {label}-----\n", testkit::base64(der)).into_bytes()
    }

    /// SEC1 `ECPrivateKey` P-256 (для `security import -f openssl`).
    fn sec1_p256(d: &[u8], xy: &[u8]) -> Vec<u8> {
        let mut v = vec![0x30, 0x77, 0x02, 0x01, 0x01, 0x04, 0x20];
        v.extend_from_slice(d);
        v.extend_from_slice(&[0xA0, 0x0A, 0x06, 0x08, 0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x03, 0x01, 0x07]);
        v.extend_from_slice(&[0xA1, 0x44, 0x03, 0x42, 0x00, 0x04]);
        v.extend_from_slice(xy);
        v
    }

    const LETTER: &[u8] = b"From: rsa@example.test\r\nTo: ec@example.test\r\nSubject: Keychain\r\nDate: Wed, 21 Oct 2026 10:00:00 +0300\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=us-ascii\r\n\r\nSigned by the Keychain.\r\n";

    fn sealed_to(pki: &Pki, alg: KeyAlg, cek: &[u8]) -> Vec<u8> {
        let cms = testkit::enveloped(
            &testkit::text_part("Keychain round trip"),
            &[To { cert: &pki.user, key: &pki.user_key, alg, ski: false }],
            Cipher::AesCbc(256),
            cek,
            false,
        );
        testkit::encrypted_letter(
            &Head { from: "a@example.test", to: "me@example.test", subject: "s", date: "Wed, 21 Oct 2026 10:00:00 +0300" },
            &cms,
            false,
        )
    }

    #[test]
    fn keychain_round_trip() {
        let rsa = Pki::rsa("Postman Test Root", "Postman RSA", "rsa@example.test");
        let ec = Pki::build(Key::rsa('a'), Key::p256("keychain"), "Postman Test EC Root", "Postman EC", "ec@example.test");
        let Key::Rsa(k) = &rsa.user_key else { unreachable!() };
        let rsa_key = pem("RSA PRIVATE KEY", &postman_smime::soft::pkcs1_der(k).unwrap());
        let (xy, d) = ec.user_key.ec_raw().unwrap();
        let ec_key = pem("EC PRIVATE KEY", &sec1_p256(&d, &xy));
        let kc = keychain(&[
            ("rsa.cer", "cert", &rsa.user.der),
            ("rsa.pem", "priv", &rsa_key),
            ("ec.cer", "cert", &ec.user.der),
            ("ec.pem", "priv", &ec_key),
        ]);
        let list = [kc.keychain.clone()];
        let kcs = Some(&list[..]);
        let mine = personal(kcs);
        assert!(mine.contains(&rsa.user.der) && mine.contains(&ec.user.der), "удостоверения: {}", mine.len());
        let now = NOW as i64;

        // Подпись: RSA и ECDSA ключами Связки — своя проверка «верна».
        for (pki, from) in [(&rsa, "rsa@example.test"), (&ec, "ec@example.test")] {
            let signed = sign_in(kcs, LETTER, from, true, now).unwrap();
            let r = postman_worker::smime::verify(&signed, &pki.trust(), NOW);
            assert_eq!((r.kind, &r.status), (Kind::Detached, &Status::Valid), "{from}: {r:?}");
        }
        assert_eq!(sign_in(kcs, LETTER, "nobody@example.test", true, now), Err(SignError::NoCertificate("nobody@example.test".into())));

        // Расшифровка: RSA PKCS#1, RSA-OAEP и ECDH — ключ содержимого тот же.
        for (pki, alg) in [(&rsa, KeyAlg::RsaPkcs1), (&rsa, KeyAlg::RsaOaep(Hash::Sha256)), (&ec, KeyAlg::Ecdh(Hash::Sha256, 128))] {
            let env = postman_worker::smime::envelope(&sealed_to(pki, alg, &[7; 32])).unwrap();
            let cek = unwrap_in(kcs, &env, Prompt::Silent).unwrap();
            assert_eq!(cek.as_slice(), &[7; 32], "{alg:?}");
        }
        let other = Pki::build(Key::rsa('a'), Key::rsa('c'), "Чужой УЦ", "Петров", "p@example.test");
        let env = postman_worker::smime::envelope(&sealed_to(&other, KeyAlg::RsaPkcs1, &[7; 32])).unwrap();
        assert!(matches!(unwrap_in(kcs, &env, Prompt::Silent), Err(KeyError::NotForYou(_))));

        // Шифрование своим (KeychainOps: случайные числа, эфемерный ECDH) → снятие ключом Связки → тот же текст.
        let trust = ec.trust();
        let raw = b"From: rsa@example.test\r\nTo: ec@example.test\r\nSubject: s\r\nContent-Type: text/plain\r\n\r\nKeychain ECDH\r\n";
        let to = [
            postman_smime::prepare::prepare(&ec.user.der, &[], &trust, NOW).unwrap(),
            postman_smime::prepare::prepare(&rsa.user.der, &[], &rsa.trust(), NOW).unwrap(),
        ]
        .map(|prepared| postman_smime::seal::To { prepared, gcm: false });
        let m = postman_smime::seal::seal_message(raw, &to, &KeychainOps).unwrap();
        let env = postman_worker::smime::envelope(&m).unwrap();
        let cek = unwrap_in(kcs, &env, Prompt::Silent).unwrap();
        let o = postman_worker::smime::open(&m, &cek, &trust, NOW).unwrap();
        assert_eq!(o.text.as_deref().map(str::trim), Some("Keychain ECDH"));
    }

    #[test]
    fn ecdh_ops_agree() {
        let key = Key::p256("keychain-ops");
        let (xy, _) = key.ec_raw().unwrap();
        let peer = [&[4u8][..], &xy].concat();
        let (own, z) = KeychainOps.ecdh(256, &peer).unwrap();
        assert_eq!(testkit::ecdh_with(&key, &own).unwrap(), z.to_vec());
        let mut a = [0u8; 32];
        KeychainOps.random(&mut a).unwrap();
        assert_ne!(a, [0u8; 32]);
    }
}
