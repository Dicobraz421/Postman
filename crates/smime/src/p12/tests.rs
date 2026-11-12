//! Файлы — `openssl pkcs12 -export` (OpenSSL 3.0) с одноразовым тестовым ключом `user@example.test`:
//! `aes.p12` — по умолчанию (PBES2 AES-256-CBC, PBKDF2 HMAC-SHA-256, MAC SHA-256, пароль не ASCII, плюс лишний
//! сертификат); `legacy.p12` — `-legacy` (сертификаты RC2-40, ключ 3DES, MAC SHA-1); `des3.p12` — как экспорт Windows
//! «TripleDES-SHA1» (всё 3DES).

use super::*;

const AES: &[u8] = include_bytes!("aes.p12");
const LEGACY: &[u8] = include_bytes!("legacy.p12");
const DES3: &[u8] = include_bytes!("des3.p12");
/// SHA-256 сертификата ключа (DER).
const CERT_SHA256: &str = "a3a80ba48fbf78cecc827a5f0a9bdfb3a3f9d683b5428cb1033639f7b439b554";

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn open(file: &[u8], password: &str, extra: usize) -> Identity {
    let ids = parse(file, password).unwrap();
    assert_eq!(ids.len(), 1);
    let id = ids.into_iter().next().unwrap();
    assert_eq!(hex(&Hash::Sha256.digest(&id.cert)), CERT_SHA256);
    assert_eq!(id.chain.len(), extra);
    id
}

#[test]
fn opens_modern_legacy_and_windows_files() {
    open(AES, "тест-пароль", 1);
    open(LEGACY, "secret", 0);
    open(DES3, "secret", 0);
}

#[test]
fn wrong_password_is_reported() {
    for (file, pw) in [(AES, "secret"), (LEGACY, ""), (DES3, "Secret")] {
        assert_eq!(parse(file, pw).unwrap_err(), P12Error::BadPassword);
    }
}

/// Обрезанный или испорченный файл — ошибка, не паника.
#[test]
fn damaged_files_do_not_panic() {
    for cut in (0..LEGACY.len()).step_by(7) {
        assert!(parse(&LEGACY[..cut], "secret").is_err());
    }
    let mut bad = AES.to_vec();
    for i in (0..bad.len()).step_by(97) {
        bad[i] ^= 0x5A;
        let _ = parse(&bad, "тест-пароль");
    }
    assert!(parse(b"not a pfx", "x").is_err());
}

/// Ключ из файла подписывает (проверка — открытым ключом сертификата) и подходит письмам на этот сертификат.
#[test]
fn key_signs_and_matches_certificate() {
    use rsa::pkcs8::DecodePublicKey;
    let id = open(DES3, "secret", 0);
    let signer = id.signer(crate::Scheme::RsaPkcs1(Hash::Sha256));
    let digest = Hash::Sha256.digest(b"hello");
    let sig = crate::sign::Signer::sign(&signer, &digest).unwrap();
    let cert = Certificate::from_der(&id.cert).unwrap();
    let spki = cert.tbs_certificate().subject_public_key_info().to_der().unwrap();
    let public = rsa::PublicKey::from_public_key_der(&spki).unwrap();
    assert!(public.verify(rsa::Pkcs1v15Sign::new::<sha2::Sha256>(), &digest, &sig).is_ok());
    assert!(id.soft_key().is_some());
}
