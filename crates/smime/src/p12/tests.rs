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

/// `ec384.p12` — ключ EC P-384 (OpenSSL 3 по умолчанию): подпись ECDSA проверяется ключом сертификата, ECDH с
/// эфемерным ключом шифрования даёт тот же секрет, что у получателя.
#[test]
fn ec_key_signs_and_agrees() {
    use p384::ecdsa::signature::hazmat::PrehashVerifier;
    let ids = parse(include_bytes!("ec384.p12"), "secret").unwrap();
    let id = &ids[0];
    assert_eq!(hex(&Hash::Sha256.digest(&id.cert)), "bc09bca62442913d32613afeca1a8cbebaa1098435cc4ffe8022c166a4132524");
    let digest = Hash::Sha384.digest(b"hello");
    let sig = crate::sign::Signer::sign(&id.signer(crate::Scheme::Ecdsa(Hash::Sha384)), &digest).unwrap();
    let cert = Certificate::from_der(&id.cert).unwrap();
    let point = cert.tbs_certificate().subject_public_key_info().subject_public_key.as_bytes().unwrap().to_vec();
    let vk = p384::ecdsa::VerifyingKey::from_sec1_bytes(&point).unwrap();
    assert!(vk.verify_prehash(&digest, &p384::ecdsa::Signature::from_der(&sig).unwrap()).is_ok());
    let (eph, z) = crate::soft::ecdh_ephemeral(384, &[7u8; 48], &point).unwrap();
    assert_eq!(crate::soft::ecdh(&id.key, &eph).unwrap(), z);
    assert!(id.soft_key().is_some());
}

/// `ec521.p12` — ключ P-521: открывается, ECDH даёт тот же секрет, что у отправителя (расшифровка, как CNG);
/// подписи P-521 нет — подписант отказывает, не падает.
#[test]
fn p521_key_agrees() {
    use p256::elliptic_curve::sec1::ToSec1Point;
    let ids = parse(include_bytes!("ec521.p12"), "secret").unwrap();
    let id = &ids[0];
    let cert = Certificate::from_der(&id.cert).unwrap();
    let point = cert.tbs_certificate().subject_public_key_info().subject_public_key.as_bytes().unwrap().to_vec();
    let eph = p521::SecretKey::from_slice(&[1u8; 66]).unwrap();
    let z = p521::ecdh::diffie_hellman(eph.to_nonzero_scalar(), p521::PublicKey::from_sec1_bytes(&point).unwrap().as_affine());
    let eph_point = eph.public_key().to_sec1_point(false).as_bytes().to_vec();
    assert_eq!(crate::soft::ecdh(&id.key, &eph_point).unwrap().as_slice(), z.raw_secret_bytes().as_slice());
    assert!(id.soft_key().is_some());
    assert!(crate::sign::Signer::sign(&id.signer(crate::Scheme::Ecdsa(Hash::Sha512)), &[0; 64]).is_err());
}
