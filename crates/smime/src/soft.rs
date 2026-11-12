//! Ключи в памяти (фича `soft`): демо-ящик и тесты ядра снимают ключ содержимого без CNG. Ключ — тестовый RSA
//! (`RsaPrivateKey`): закрытые операции `rsa` подвержены RUSTSEC-2023-0071 (утечка по времени), поэтому настоящих
//! ключей здесь не бывает — у живого ящика ключи только в хранилище Windows (`postman_core::smime::keys`).
//!
//! Сертификат свой (демо, тест), не из письма: разбирать его здесь можно.
//!
//! Исключение — Linux (LIN-05, решение владельца 2026-11-12): свои ключи — файл PKCS#12 ([`crate::p12`]), закрытые
//! операции — здесь же. `rsa` 0.10 считает на `crypto-bigint` в постоянном времени (RUSTSEC-2023-0071 закрыт в 0.10).

use der::{Decode, Encode};
use p256::elliptic_curve::sec1::ToSec1Point;
use rsa::PrivateKey as RsaPrivateKey;
use rsa::pkcs1::DecodeRsaPrivateKey;
use x509_cert::Certificate;
use x509_cert::ext::pkix::SubjectKeyIdentifier;
use zeroize::Zeroizing;

use crate::Hash;
use crate::envelope::{Cek, Envelope, Rid, Wrap};

/// Закрытый ключ в памяти: RSA или EC P-256/P-384 (файл PKCS#12, Linux).
#[derive(Clone)]
pub enum Private {
    Rsa(Box<RsaPrivateKey>),
    P256(p256::SecretKey),
    P384(p384::SecretKey),
}

/// Свой сертификат и закрытый ключ.
pub struct SoftKey {
    issuer: Vec<u8>,
    serial: Vec<u8>,
    ski: Option<Vec<u8>>,
    key: Private,
}

/// Общий секрет ECDH своего ключа `key` и точки `peer` (SEC1); не та кривая или не точка — `None`.
pub(crate) fn ecdh(key: &Private, peer: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    match key {
        Private::P256(k) => {
            let p = p256::PublicKey::from_sec1_bytes(peer).ok()?;
            Some(Zeroizing::new(p256::ecdh::diffie_hellman(k.to_nonzero_scalar(), p.as_affine()).raw_secret_bytes().to_vec()))
        }
        Private::P384(k) => {
            let p = p384::PublicKey::from_sec1_bytes(peer).ok()?;
            Some(Zeroizing::new(p384::ecdh::diffie_hellman(k.to_nonzero_scalar(), p.as_affine()).raw_secret_bytes().to_vec()))
        }
        Private::Rsa(_) => None,
    }
}

/// Эфемерный ECDH для шифрования (`seal::Ops::ecdh`): ключ из случайных байтов `seed` (32 — P-256, 48 — P-384) →
/// своя точка (SEC1 без сжатия) и общий секрет с `peer`. Неподходящие байты (≥ порядка, ноль) или точка — `None`.
pub fn ecdh_ephemeral(bits: u16, seed: &[u8], peer: &[u8]) -> Option<(Vec<u8>, Zeroizing<Vec<u8>>)> {
    let key =
        if bits == 384 { Private::P384(p384::SecretKey::from_slice(seed).ok()?) } else { Private::P256(p256::SecretKey::from_slice(seed).ok()?) };
    let own = match &key {
        Private::P256(k) => k.public_key().to_sec1_point(false).as_bytes().to_vec(),
        Private::P384(k) => k.public_key().to_sec1_point(false).as_bytes().to_vec(),
        Private::Rsa(_) => return None,
    };
    Some((own, ecdh(&key, peer)?))
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
            key: Private::Rsa(Box::new(RsaPrivateKey::from_pkcs1_der(pkcs1).ok()?)),
        })
    }

    /// Сертификат (DER) и готовый ключ (из файла PKCS#12).
    pub fn from_key(cert: &[u8], key: Private) -> Option<SoftKey> {
        let c = Certificate::from_der(cert).ok()?;
        let tbs = c.tbs_certificate();
        let ski = tbs.get_extension::<SubjectKeyIdentifier>().ok().flatten().map(|(_, s)| s.0.as_bytes().to_vec());
        Some(SoftKey { issuer: tbs.issuer().to_der().ok()?, serial: tbs.serial_number().as_bytes().to_vec(), ski, key })
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
        if let Wrap::Ecdh { ephemeral, .. } = wrap {
            let z = ecdh(&self.key, ephemeral).ok_or_else(|| SoftError::Unsupported("ECDH: curve".into()))?;
            return Ok(crate::envelope::ecdh_cek(&z, wrap, data));
        }
        let Private::Rsa(key) = &self.key else { return Err(SoftError::Unsupported("EC key: RSA recipient".into())) };
        let r = match wrap {
            Wrap::RsaPkcs1 => key.decrypt(rsa::Pkcs1v15Encrypt, data),
            Wrap::RsaOaep(Hash::Sha1) => key.decrypt(rsa::Oaep::<sha1::Sha1>::new(), data),
            Wrap::RsaOaep(Hash::Sha256) => key.decrypt(rsa::Oaep::<sha2::Sha256>::new(), data),
            Wrap::RsaOaep(Hash::Sha384) => key.decrypt(rsa::Oaep::<sha2::Sha384>::new(), data),
            Wrap::RsaOaep(Hash::Sha512) => key.decrypt(rsa::Oaep::<sha2::Sha512>::new(), data),
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

/// Подписант ключом в памяти (RSA PKCS#1 v1.5, ECDSA P-256/P-384; файл PKCS#12 на Linux).
pub struct SoftSigner {
    pub(crate) cert: Vec<u8>,
    pub(crate) chain: Vec<Vec<u8>>,
    pub(crate) scheme: crate::Scheme,
    pub(crate) key: Private,
}

impl crate::sign::Signer for SoftSigner {
    fn certificate(&self) -> &[u8] {
        &self.cert
    }

    fn chain(&self) -> &[Vec<u8>] {
        &self.chain
    }

    fn scheme(&self) -> crate::Scheme {
        self.scheme
    }

    fn sign(&self, digest: &[u8]) -> Result<Vec<u8>, crate::sign::SignError> {
        use p256::ecdsa::signature::hazmat::PrehashSigner;
        use rsa::Pkcs1v15Sign;
        let failed = |e: String| crate::sign::SignError::Failed(e);
        match (&self.key, self.scheme) {
            (Private::P256(k), crate::Scheme::Ecdsa(_)) => {
                let s: p256::ecdsa::Signature = p256::ecdsa::SigningKey::from(k).sign_prehash(digest).map_err(|e| failed(e.to_string()))?;
                return Ok(s.to_der().as_bytes().to_vec());
            }
            (Private::P384(k), crate::Scheme::Ecdsa(_)) => {
                let s: p384::ecdsa::Signature = p384::ecdsa::SigningKey::from(k).sign_prehash(digest).map_err(|e| failed(e.to_string()))?;
                return Ok(s.to_der().as_bytes().to_vec());
            }
            (Private::Rsa(_), _) => {}
            (_, s) => return Err(failed(format!("key in memory: {s:?} is not supported"))),
        }
        let Private::Rsa(key) = &self.key else { return Err(failed("key in memory".into())) };
        let padding = match self.scheme {
            crate::Scheme::RsaPkcs1(Hash::Sha1) => Pkcs1v15Sign::new::<sha1::Sha1>(),
            crate::Scheme::RsaPkcs1(Hash::Sha256) => Pkcs1v15Sign::new::<sha2::Sha256>(),
            crate::Scheme::RsaPkcs1(Hash::Sha384) => Pkcs1v15Sign::new::<sha2::Sha384>(),
            crate::Scheme::RsaPkcs1(Hash::Sha512) => Pkcs1v15Sign::new::<sha2::Sha512>(),
            s => return Err(crate::sign::SignError::Failed(format!("key in memory: {s:?} is not supported"))),
        };
        key.sign(padding, digest).map_err(|e| failed(e.to_string()))
    }
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
