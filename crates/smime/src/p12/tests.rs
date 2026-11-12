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

/// RC2: векторы RFC 2268 §5 (расшифровка шифртекста даёт открытый текст).
#[test]
fn rc2_vectors() {
    let cases: [(&str, usize, &str, &str); 4] = [
        ("0000000000000000", 63, "0000000000000000", "ebb773f993278eff"),
        ("ffffffffffffffff", 64, "ffffffffffffffff", "278b27e42e2f0d49"),
        ("3000000000000000", 64, "1000000000000001", "30649edf9be7d2c2"),
        ("88bca90e90875a7f0f79c384627bafb2", 128, "0000000000000000", "2269552ab0f85ca6"),
    ];
    let unhex = |s: &str| (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect::<Vec<u8>>();
    for (key, bits, plain, cipher) in cases {
        let mut block: [u8; 8] = unhex(cipher).try_into().unwrap();
        assert!(rc2::ecb_block(&unhex(key), bits, &mut block));
        assert_eq!(hex(&block), plain, "key {key}");
    }
}
