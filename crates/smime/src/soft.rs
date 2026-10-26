//! Ключи в памяти (фича `soft`): демо-ящик и тесты ядра снимают ключ содержимого без CNG. Ключ — тестовый RSA
//! (`RsaPrivateKey`): закрытые операции `rsa` подвержены RUSTSEC-2023-0071 (утечка по времени), поэтому настоящих
//! ключей здесь не бывает — у живого ящика ключи только в хранилище Windows (`postman_core::smime::keys`).
//!
//! Сертификат свой (демо, тест), не из письма: разбирать его здесь можно.

use der::{Decode, Encode};
use rsa::RsaPrivateKey;
use rsa::pkcs1::DecodeRsaPrivateKey;
use x509_cert::Certificate;
use x509_cert::ext::pkix::SubjectKeyIdentifier;
use zeroize::Zeroizing;

use crate::Hash;
use crate::envelope::{Cek, Envelope, Rid, Wrap};

/// Свой сертификат и закрытый ключ RSA.
pub struct SoftKey {
    issuer: Vec<u8>,
    serial: Vec<u8>,
    ski: Option<Vec<u8>>,
    key: RsaPrivateKey,
}

/// Почему ключ не снят.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoftError {
    /// Ни один получатель письма — не наш сертификат.
    NotForYou,
    /// Получатель наш, но алгоритм не поддерживается.
    Unsupported(String),
}

impl SoftKey {
    /// Сертификат (DER) и ключ PKCS#1 (DER).
    pub fn new(cert: &[u8], pkcs1: &[u8]) -> Option<SoftKey> {
        let c = Certificate::from_der(cert).ok()?;
        let tbs = c.tbs_certificate();
        let ski = tbs.get_extension::<SubjectKeyIdentifier>().ok().flatten().map(|(_, s)| s.0.as_bytes().to_vec());
        Some(SoftKey {
            issuer: tbs.issuer().to_der().ok()?,
            serial: tbs.serial_number().as_bytes().to_vec(),
            ski,
            key: RsaPrivateKey::from_pkcs1_der(pkcs1).ok()?,
        })
    }

    fn matches(&self, rid: &Rid) -> bool {
        match rid {
            Rid::IssuerSerial { issuer, serial } => {
                let le: Vec<u8> = self.serial.iter().rev().copied().collect();
                *issuer == self.issuer && crate::envelope::serial_matches(serial, &le)
            }
            Rid::Ski(s) => self.ski.as_deref() == Some(s.as_slice()),
        }
    }

    fn decrypt(&self, wrap: &Wrap, data: &[u8]) -> Result<Option<Cek>, SoftError> {
        let r = match wrap {
            Wrap::RsaPkcs1 => self.key.decrypt(rsa::Pkcs1v15Encrypt, data),
            Wrap::RsaOaep(Hash::Sha1) => self.key.decrypt(rsa::Oaep::<sha1::Sha1>::new(), data),
            Wrap::RsaOaep(Hash::Sha256) => self.key.decrypt(rsa::Oaep::<sha2::Sha256>::new(), data),
            Wrap::RsaOaep(Hash::Sha384) => self.key.decrypt(rsa::Oaep::<sha2::Sha384>::new(), data),
            Wrap::RsaOaep(Hash::Sha512) => self.key.decrypt(rsa::Oaep::<sha2::Sha512>::new(), data),
            Wrap::RsaOaep(h) => return Err(SoftError::Unsupported(format!("RSA-OAEP {h:?}"))),
            Wrap::Ecdh { .. } => return Err(SoftError::Unsupported("ECDH".into())),
            // Ключи ГОСТ — только в СКЗИ (решение владельца 2026-10-22), в памяти их не бывает.
            Wrap::Gost(_) => return Err(SoftError::Unsupported("ГОСТ: нужно СКЗИ".into())),
            Wrap::Unsupported(n) => return Err(SoftError::Unsupported(n.clone())),
        };
        Ok(r.ok().map(Zeroizing::new))
    }
}

/// Снять ключ содержимого своими ключами. Сбой снятия (неверное заполнение) — ключ из нулей нужной длины: расшифровка
/// содержимого даст ту же ошибку «повреждено», что и неверный CEK (RFC 3218).
pub fn unwrap(keys: &[SoftKey], env: &Envelope) -> Result<Cek, SoftError> {
    let len = env.cipher.as_ref().map(|c| c.key_len()).unwrap_or(32);
    let mut unsupported = None;
    for r in &env.recipients {
        for k in keys.iter().filter(|k| k.matches(&r.rid)) {
            match k.decrypt(&r.wrap, &r.encrypted_key) {
                Ok(Some(cek)) => return Ok(cek),
                Ok(None) => return Ok(Zeroizing::new(vec![0; len])),
                Err(e) => unsupported = Some(e),
            }
        }
    }
    Err(unsupported.unwrap_or(SoftError::NotForYou))
}

/// Ключ PKCS#1 в DER (для демо-образцов).
pub fn pkcs1_der(key: &RsaPrivateKey) -> Option<Vec<u8>> {
    use rsa::pkcs1::EncodeRsaPrivateKey;
    key.to_pkcs1_der().ok().map(|d| d.as_bytes().to_vec())
}

/// Ключ PKCS#8 в DER (тесты CNG: импорт временного ключа).
pub fn pkcs8_der(key: &RsaPrivateKey) -> Option<Vec<u8>> {
    use rsa::pkcs8::EncodePrivateKey;
    key.to_pkcs8_der().ok().map(|d| d.as_bytes().to_vec())
}
