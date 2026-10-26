//! Алгоритмы подписи: хэши, разбор открытого ключа, проверка подписи CMS и таблица алгоритмов для цепочки
//! (`rustls-webpki`, трейт `SignatureVerificationAlgorithm`).
//!
//! RSA — крейт `rsa` (PKCS#1 v1.5, PSS); ECDSA P-256/P-384 и ГОСТ Р 34.10-2012 — своя проверка (`ec.rs`), одна
//! арифметика на все кривые. SHA-1 принимается и помечается «слабая» (решение владельца №10); MD5 — нет.

use std::sync::OnceLock;

use const_oid::ObjectIdentifier as Oid;
use der::asn1::{Any, Null, UintRef};
use der::{Decode, Encode, Sequence};
use digest::Digest;
use pki_types::{AlgorithmIdentifier, InvalidSignature, SignatureVerificationAlgorithm};
use rsa::pkcs1::DecodeRsaPublicKey;
use rsa::traits::PublicKeyParts;
use rsa::{Pkcs1v15Sign, RsaPublicKey};
use serde::{Deserialize, Serialize};
use spki::{AlgorithmIdentifierOwned, SubjectPublicKeyInfoOwned};

use crate::ec::{self, Curve};
use crate::gost::{self, Size};
use crate::oid;

/// Хэш-функция подписи.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Hash {
    Sha1,
    Sha256,
    Sha384,
    Sha512,
    Streebog256,
    Streebog512,
}

impl Hash {
    pub(crate) fn from_oid(oid: &Oid) -> Option<Hash> {
        Some(match *oid {
            oid::SHA1 => Hash::Sha1,
            oid::SHA256 => Hash::Sha256,
            oid::SHA384 => Hash::Sha384,
            oid::SHA512 => Hash::Sha512,
            oid::STREEBOG256 => Hash::Streebog256,
            oid::STREEBOG512 => Hash::Streebog512,
            _ => return None,
        })
    }

    pub fn oid(self) -> Oid {
        match self {
            Hash::Sha1 => oid::SHA1,
            Hash::Sha256 => oid::SHA256,
            Hash::Sha384 => oid::SHA384,
            Hash::Sha512 => oid::SHA512,
            Hash::Streebog256 => oid::STREEBOG256,
            Hash::Streebog512 => oid::STREEBOG512,
        }
    }

    pub fn digest(self, data: &[u8]) -> Vec<u8> {
        match self {
            Hash::Sha1 => sha1::Sha1::digest(data).to_vec(),
            Hash::Sha256 => sha2::Sha256::digest(data).to_vec(),
            Hash::Sha384 => sha2::Sha384::digest(data).to_vec(),
            Hash::Sha512 => sha2::Sha512::digest(data).to_vec(),
            Hash::Streebog256 => streebog::Streebog256::digest(data).to_vec(),
            Hash::Streebog512 => streebog::Streebog512::digest(data).to_vec(),
        }
    }

    pub fn output_len(self) -> usize {
        match self {
            Hash::Sha1 => 20,
            Hash::Sha256 | Hash::Streebog256 => 32,
            Hash::Sha384 => 48,
            Hash::Sha512 | Hash::Streebog512 => 64,
        }
    }

    /// SHA-1: коллизии найдены (SHAttered, 2017) — подпись показывается «слабой», не «недействительной».
    pub fn weak(self) -> bool {
        self == Hash::Sha1
    }

    /// Имя для карточки (как в Outlook и Windows).
    pub fn name(self) -> &'static str {
        match self {
            Hash::Sha1 => "SHA-1",
            Hash::Sha256 => "SHA-256",
            Hash::Sha384 => "SHA-384",
            Hash::Sha512 => "SHA-512",
            Hash::Streebog256 => "Streebog-256",
            Hash::Streebog512 => "Streebog-512",
        }
    }
}

/// Схема подписи.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scheme {
    RsaPkcs1(Hash),
    RsaPss {
        hash: Hash,
        salt: u32,
    },
    Ecdsa(Hash),
    /// ГОСТ Р 34.10-2012 с «Стрибогом» того же размера.
    Gost256,
    Gost512,
}

impl Scheme {
    pub fn hash(self) -> Hash {
        match self {
            Scheme::RsaPkcs1(h) | Scheme::Ecdsa(h) | Scheme::RsaPss { hash: h, .. } => h,
            Scheme::Gost256 => Hash::Streebog256,
            Scheme::Gost512 => Hash::Streebog512,
        }
    }

    pub fn is_gost(self) -> bool {
        matches!(self, Scheme::Gost256 | Scheme::Gost512)
    }
}

/// Открытый ключ для сводки (карточка сертификата).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyKind {
    Rsa {
        bits: u32,
    },
    Ec {
        curve: String,
    },
    /// ГОСТ Р 34.10-2012: 256 или 512 бит, набор параметров.
    Gost {
        bits: u32,
        params: String,
    },
    Other(String),
}

/// Разобранный открытый ключ.
pub(crate) enum PublicKey {
    Rsa(RsaPublicKey),
    Ec { curve: &'static Curve, x: crypto_bigint::U512, y: crypto_bigint::U512 },
    Gost { key: gost::Key, size: Size },
}

/// Несовместимость или неизвестный алгоритм: текст для «не поддерживается: …».
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unsupported(pub String);

fn params_der(alg: &AlgorithmIdentifierOwned) -> Option<Vec<u8>> {
    alg.parameters.as_ref().and_then(|p| p.to_der().ok())
}

/// Точка SEC 1 без сжатия (`04‖x‖y`) на кривой `curve`.
fn sec1_point(curve: &'static Curve, bytes: &[u8]) -> Option<(crypto_bigint::U512, crypto_bigint::U512)> {
    let n = curve.bytes;
    if bytes.len() != 1 + 2 * n || bytes[0] != 4 {
        return None;
    }
    let x = ec::uint_be(&bytes[1..=n])?;
    let y = ec::uint_be(&bytes[n + 1..])?;
    curve.on_curve(&x, &y).then_some((x, y))
}

fn ec_curve(alg: &AlgorithmIdentifierOwned) -> Option<&'static Curve> {
    let named: Oid = alg.parameters.as_ref()?.decode_as().ok()?;
    matches!(named, oid::P256 | oid::P384).then(|| ec::curve(&named)).flatten()
}

/// Ключ из `SubjectPublicKeyInfo`.
pub(crate) fn public_key(spki: &SubjectPublicKeyInfoOwned) -> Result<PublicKey, Unsupported> {
    let alg = &spki.algorithm;
    let bits = spki.subject_public_key.as_bytes().ok_or_else(|| Unsupported("public key bits".into()))?;
    match alg.oid {
        oid::RSA_ENCRYPTION => {
            let key = RsaPublicKey::from_pkcs1_der(bits).map_err(|e| Unsupported(format!("RSA key: {e}")))?;
            Ok(PublicKey::Rsa(key))
        }
        oid::EC_PUBLIC_KEY => {
            let curve = ec_curve(alg).ok_or_else(|| Unsupported("EC curve".into()))?;
            let (x, y) = sec1_point(curve, bits).ok_or_else(|| Unsupported("EC point".into()))?;
            Ok(PublicKey::Ec { curve, x, y })
        }
        oid::GOST_KEY_256 | oid::GOST_KEY_512 => {
            let size = Size::of_key(&alg.oid).ok_or_else(|| Unsupported("GOST key".into()))?;
            let set = params_der(alg).and_then(|p| gost::param_set(&p)).ok_or_else(|| Unsupported("GOST parameters".into()))?;
            let key = gost::key(size, &set, bits).ok_or_else(|| Unsupported(format!("GOST parameter set {set}")))?;
            Ok(PublicKey::Gost { key, size })
        }
        oid::GOST2001_KEY => Err(Unsupported("GOST R 34.10-2001".into())),
        other => Err(Unsupported(other.to_string())),
    }
}

/// Сводка ключа для карточки.
pub(crate) fn key_kind(spki: &SubjectPublicKeyInfoOwned) -> KeyKind {
    match public_key(spki) {
        Ok(PublicKey::Rsa(k)) => KeyKind::Rsa { bits: k.n().bits_vartime() },
        Ok(PublicKey::Ec { curve, .. }) => KeyKind::Ec { curve: curve.name.into() },
        Ok(PublicKey::Gost { key, size }) => KeyKind::Gost { bits: if size == Size::S256 { 256 } else { 512 }, params: key.curve.name.into() },
        Err(Unsupported(e)) => KeyKind::Other(e),
    }
}

/// RSASSA-PSS-params (RFC 4055 §3.1); по умолчанию SHA-1, MGF1-SHA-1, соль 20.
#[derive(Sequence)]
struct PssParams {
    #[asn1(context_specific = "0", tag_mode = "EXPLICIT", optional = "true")]
    hash: Option<AlgorithmIdentifierOwned>,
    #[asn1(context_specific = "1", tag_mode = "EXPLICIT", optional = "true")]
    mgf: Option<AlgorithmIdentifierOwned>,
    #[asn1(context_specific = "2", tag_mode = "EXPLICIT", optional = "true")]
    salt: Option<u32>,
    #[asn1(context_specific = "3", tag_mode = "EXPLICIT", optional = "true")]
    trailer: Option<u32>,
}

fn pss(alg: &AlgorithmIdentifierOwned) -> Result<Scheme, Unsupported> {
    let bad = || Unsupported("RSASSA-PSS parameters".into());
    let p = params_der(alg).and_then(|d| PssParams::from_der(&d).ok()).ok_or_else(bad)?;
    let hash = match &p.hash {
        Some(h) => Hash::from_oid(&h.oid).ok_or_else(bad)?,
        None => Hash::Sha1,
    };
    let mgf_hash = match &p.mgf {
        Some(m) if m.oid == oid::MGF1 => {
            let inner: AlgorithmIdentifierOwned = m.parameters.as_ref().and_then(|a| a.decode_as().ok()).ok_or_else(bad)?;
            Hash::from_oid(&inner.oid).ok_or_else(bad)?
        }
        Some(_) => return Err(bad()),
        None => Hash::Sha1,
    };
    if mgf_hash != hash || p.trailer.unwrap_or(1) != 1 || hash.output_len() > 64 || matches!(hash, Hash::Streebog256 | Hash::Streebog512) {
        return Err(bad());
    }
    Ok(Scheme::RsaPss { hash, salt: p.salt.unwrap_or(20) })
}

/// Схема подписи `SignerInfo`: алгоритм подписи и алгоритм хэша (`digestAlgorithm`). Outlook пишет в
/// `signatureAlgorithm` `rsaEncryption` (хэш — из `digestAlgorithm`), КриптоПро — OID ключа ГОСТ.
pub(crate) fn cms_scheme(sig: &AlgorithmIdentifierOwned, digest: Hash) -> Result<Scheme, Unsupported> {
    let with = |h: Hash| if h == digest { Ok(h) } else { Err(Unsupported(format!("{} with {}", sig.oid, digest.name()))) };
    match sig.oid {
        oid::RSA_ENCRYPTION => Ok(Scheme::RsaPkcs1(digest)),
        oid::SHA1_RSA => with(Hash::Sha1).map(Scheme::RsaPkcs1),
        oid::SHA256_RSA => with(Hash::Sha256).map(Scheme::RsaPkcs1),
        oid::SHA384_RSA => with(Hash::Sha384).map(Scheme::RsaPkcs1),
        oid::SHA512_RSA => with(Hash::Sha512).map(Scheme::RsaPkcs1),
        oid::RSA_PSS => pss(sig).and_then(|s| with(s.hash()).map(|_| s)),
        oid::EC_PUBLIC_KEY => Ok(Scheme::Ecdsa(digest)),
        oid::ECDSA_SHA1 => with(Hash::Sha1).map(Scheme::Ecdsa),
        oid::ECDSA_SHA256 => with(Hash::Sha256).map(Scheme::Ecdsa),
        oid::ECDSA_SHA384 => with(Hash::Sha384).map(Scheme::Ecdsa),
        oid::ECDSA_SHA512 => with(Hash::Sha512).map(Scheme::Ecdsa),
        oid::GOST_SIGN_256 | oid::GOST_KEY_256 => with(Hash::Streebog256).map(|_| Scheme::Gost256),
        oid::GOST_SIGN_512 | oid::GOST_KEY_512 => with(Hash::Streebog512).map(|_| Scheme::Gost512),
        oid::MD5_RSA => Err(Unsupported("MD5".into())),
        oid::GOST2001_SIGN | oid::GOST2001_KEY => Err(Unsupported("GOST R 34.10-2001".into())),
        other => Err(Unsupported(other.to_string())),
    }
}

/// `Ecdsa-Sig-Value ::= SEQUENCE { r INTEGER, s INTEGER }`.
#[derive(Sequence)]
struct EcdsaSig<'a> {
    r: UintRef<'a>,
    s: UintRef<'a>,
}

fn ecdsa(curve: &Curve, x: &crypto_bigint::U512, y: &crypto_bigint::U512, hash: &[u8], signature: &[u8]) -> bool {
    let Ok(sig) = EcdsaSig::from_der(signature) else { return false };
    let (Some(r), Some(s)) = (ec::uint_be(sig.r.as_bytes()), ec::uint_be(sig.s.as_bytes())) else { return false };
    ec::ecdsa_verify(curve, x, y, hash, &r, &s)
}

fn rsa_pkcs1(key: &RsaPublicKey, hash: Hash, digest: &[u8], signature: &[u8]) -> bool {
    let scheme = match hash {
        Hash::Sha1 => Pkcs1v15Sign::new::<sha1::Sha1>(),
        Hash::Sha256 => Pkcs1v15Sign::new::<sha2::Sha256>(),
        Hash::Sha384 => Pkcs1v15Sign::new::<sha2::Sha384>(),
        Hash::Sha512 => Pkcs1v15Sign::new::<sha2::Sha512>(),
        Hash::Streebog256 | Hash::Streebog512 => return false,
    };
    key.verify(scheme, digest, signature).is_ok()
}

fn rsa_pss(key: &RsaPublicKey, hash: Hash, salt: u32, digest: &[u8], signature: &[u8]) -> bool {
    let salt = salt as usize;
    let r = match hash {
        Hash::Sha1 => key.verify(rsa::pss::Pss::<sha1::Sha1>::new_with_salt(salt), digest, signature),
        Hash::Sha256 => key.verify(rsa::pss::Pss::<sha2::Sha256>::new_with_salt(salt), digest, signature),
        Hash::Sha384 => key.verify(rsa::pss::Pss::<sha2::Sha384>::new_with_salt(salt), digest, signature),
        Hash::Sha512 => key.verify(rsa::pss::Pss::<sha2::Sha512>::new_with_salt(salt), digest, signature),
        Hash::Streebog256 | Hash::Streebog512 => return false,
    };
    r.is_ok()
}

/// Проверить подпись `signature` над `message` (хэш считается здесь) ключом `key` по схеме `scheme`.
pub(crate) fn verify(key: &PublicKey, scheme: Scheme, message: &[u8], signature: &[u8]) -> bool {
    let digest = scheme.hash().digest(message);
    match (key, scheme) {
        (PublicKey::Rsa(k), Scheme::RsaPkcs1(h)) => rsa_pkcs1(k, h, &digest, signature),
        (PublicKey::Rsa(k), Scheme::RsaPss { hash, salt }) => rsa_pss(k, hash, salt, &digest, signature),
        (PublicKey::Ec { curve, x, y }, Scheme::Ecdsa(_)) => ecdsa(curve, x, y, &digest, signature),
        (PublicKey::Gost { key, size: Size::S256 }, Scheme::Gost256) | (PublicKey::Gost { key, size: Size::S512 }, Scheme::Gost512) => {
            gost::verify(key, &digest, signature)
        }
        _ => false,
    }
}

// --- таблица для цепочки (`rustls-webpki`) ---

/// Что проверяет запись таблицы.
#[derive(Debug, Clone, Copy)]
enum Kind {
    RsaPkcs1(Hash),
    RsaPss(Hash),
    Ecdsa(&'static Oid, Hash),
    Gost(Size, Oid),
}

/// Алгоритм для `rustls-webpki`: точные `AlgorithmIdentifier` ключа и подписи (содержимое `SEQUENCE`, как их
/// сравнивает webpki) и проверка.
#[derive(Debug)]
struct ChainAlg {
    public_key: &'static [u8],
    signature: &'static [u8],
    kind: Kind,
}

/// Наименьший RSA-ключ в цепочке.
const CHAIN_MIN_RSA_BITS: u32 = 2048;

impl SignatureVerificationAlgorithm for ChainAlg {
    fn verify_signature(&self, public_key: &[u8], message: &[u8], signature: &[u8]) -> Result<(), InvalidSignature> {
        let ok = match self.kind {
            Kind::RsaPkcs1(h) | Kind::RsaPss(h) => {
                let Ok(key) = RsaPublicKey::from_pkcs1_der(public_key) else { return Err(InvalidSignature) };
                if key.n().bits_vartime() < CHAIN_MIN_RSA_BITS {
                    return Err(InvalidSignature);
                }
                let digest = h.digest(message);
                match self.kind {
                    Kind::RsaPss(_) => rsa_pss(&key, h, h.output_len() as u32, &digest, signature),
                    _ => rsa_pkcs1(&key, h, &digest, signature),
                }
            }
            Kind::Ecdsa(curve, h) => {
                let Some(curve) = ec::curve(curve) else { return Err(InvalidSignature) };
                let Some((x, y)) = sec1_point(curve, public_key) else { return Err(InvalidSignature) };
                ecdsa(curve, &x, &y, &h.digest(message), signature)
            }
            Kind::Gost(size, set) => {
                let Some(key) = gost::key(size, &set, public_key) else { return Err(InvalidSignature) };
                let h = if size == Size::S256 { Hash::Streebog256 } else { Hash::Streebog512 };
                gost::verify(&key, &h.digest(message), signature)
            }
        };
        if ok { Ok(()) } else { Err(InvalidSignature) }
    }

    fn public_key_alg_id(&self) -> AlgorithmIdentifier {
        AlgorithmIdentifier::from_slice(self.public_key)
    }

    fn signature_alg_id(&self) -> AlgorithmIdentifier {
        AlgorithmIdentifier::from_slice(self.signature)
    }
}

/// Содержимое `AlgorithmIdentifier` (без заголовка `SEQUENCE`) — так его сравнивает webpki.
fn alg_id(oid: Oid, params: Option<Any>) -> Option<&'static [u8]> {
    let der = AlgorithmIdentifierOwned { oid, parameters: params }.to_der().ok()?;
    let start = match *der.get(1)? {
        l if l < 0x80 => 2,
        l => 2 + usize::from(l & 0x7f),
    };
    Some(Box::leak(der.get(start..)?.to_vec().into_boxed_slice()))
}

fn null() -> Option<Any> {
    Any::encode_from(&Null).ok()
}

fn any<T: der::EncodeValue + der::Tagged>(v: &T) -> Option<Any> {
    Any::encode_from(v).ok()
}

/// `GostR3410-2012-PublicKeyParameters` для записи таблицы.
#[derive(Sequence)]
struct GostKeyParams {
    set: Oid,
    digest: Option<Oid>,
}

fn build_chain_algs() -> Vec<ChainAlg> {
    let mut out = Vec::new();
    let mut push = |pk: Option<&'static [u8]>, sig: Option<&'static [u8]>, kind: Kind| {
        if let (Some(public_key), Some(signature)) = (pk, sig) {
            out.push(ChainAlg { public_key, signature, kind });
        }
    };
    let rsa_key = alg_id(oid::RSA_ENCRYPTION, null());
    for (sig, h) in [(oid::SHA1_RSA, Hash::Sha1), (oid::SHA256_RSA, Hash::Sha256), (oid::SHA384_RSA, Hash::Sha384), (oid::SHA512_RSA, Hash::Sha512)] {
        // С `NULL` в параметрах (RFC 4055) и без них (встречается у некоторых CA).
        push(rsa_key, alg_id(sig, null()), Kind::RsaPkcs1(h));
        push(rsa_key, alg_id(sig, None), Kind::RsaPkcs1(h));
    }
    for (constant, h) in [
        (pki_types::alg_id::RSA_PSS_SHA256, Hash::Sha256),
        (pki_types::alg_id::RSA_PSS_SHA384, Hash::Sha384),
        (pki_types::alg_id::RSA_PSS_SHA512, Hash::Sha512),
    ] {
        let sig: &'static [u8] = Box::leak(constant.as_ref().to_vec().into_boxed_slice());
        push(rsa_key, Some(sig), Kind::RsaPss(h));
    }
    static P256: Oid = oid::P256;
    static P384: Oid = oid::P384;
    for curve in [&P256, &P384] {
        let pk = alg_id(oid::EC_PUBLIC_KEY, any(curve));
        for (sig, h) in
            [(oid::ECDSA_SHA1, Hash::Sha1), (oid::ECDSA_SHA256, Hash::Sha256), (oid::ECDSA_SHA384, Hash::Sha384), (oid::ECDSA_SHA512, Hash::Sha512)]
        {
            push(pk, alg_id(sig, None), Kind::Ecdsa(curve, h));
        }
    }
    for (size, key_oid, sig_oid) in [(Size::S256, oid::GOST_KEY_256, oid::GOST_SIGN_256), (Size::S512, oid::GOST_KEY_512, oid::GOST_SIGN_512)] {
        let sigs = [alg_id(sig_oid, None), alg_id(sig_oid, null())];
        for set in gost::param_sets(size) {
            for digest in [None, Some(oid::STREEBOG256), Some(oid::STREEBOG512)] {
                let pk = alg_id(key_oid, any(&GostKeyParams { set, digest }));
                for sig in sigs {
                    push(pk, sig, Kind::Gost(size, set));
                }
            }
        }
    }
    out
}

/// Алгоритмы подписи сертификатов для `rustls-webpki`.
pub(crate) fn chain_algs() -> &'static [&'static dyn SignatureVerificationAlgorithm] {
    static ALGS: OnceLock<Vec<&'static dyn SignatureVerificationAlgorithm>> = OnceLock::new();
    ALGS.get_or_init(|| build_chain_algs().into_iter().map(|a| &*Box::leak(Box::new(a)) as &'static dyn SignatureVerificationAlgorithm).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_table_is_built() {
        let algs = chain_algs();
        // RSA 8 + PSS 3 + ECDSA 8 + ГОСТ (256: 11 наборов, 512: 4) × 3 × 2.
        assert!(algs.len() > 19 + 60, "{}", algs.len());
        // Константы pki-types совпадают с нашими (RSA PKCS#1 SHA-256 с NULL, ECDSA P-256).
        assert!(algs.iter().any(|a| a.signature_alg_id().as_ref() == pki_types::alg_id::RSA_PKCS1_SHA256.as_ref()));
        assert!(algs.iter().any(|a| a.public_key_alg_id().as_ref() == pki_types::alg_id::ECDSA_P256.as_ref()));
        assert!(algs.iter().any(|a| a.signature_alg_id().as_ref() == pki_types::alg_id::ECDSA_SHA384.as_ref()));
    }

    /// Содержимое `AlgorithmIdentifier` (без заголовка), как его видит webpki.
    fn content(der: &[u8]) -> Vec<u8> {
        crate::ber::read(der).unwrap().0.value.to_vec()
    }

    /// Подпись `tbs` (сертификат или CRL) проверяется алгоритмом из таблицы цепочки, найденным по точным байтам.
    fn verify_signed(tbs_alg: &[u8], spki: &SubjectPublicKeyInfoOwned, tbs: &[u8], sig: &[u8]) -> bool {
        let pk = content(&spki.algorithm.to_der().unwrap());
        let alg = chain_algs()
            .iter()
            .find(|a| a.signature_alg_id().as_ref() == tbs_alg && a.public_key_alg_id().as_ref() == pk.as_slice())
            .unwrap_or_else(|| panic!("нет алгоритма для {tbs_alg:02X?} / {pk:02X?}"));
        alg.verify_signature(spki.subject_public_key.raw_bytes(), tbs, sig).is_ok()
    }

    /// Совместимость кодирования ГОСТ с независимой реализацией: примеры RFC 9215 (приложение D) — самоподписанные
    /// сертификаты и CRL на тестовом наборе 256, наборе ТК 26 256-A (скрученная кривая Эдвардса) и тестовом 512.
    #[test]
    fn rfc9215_examples_verify() {
        use x509_cert::Certificate;
        let certs = crate::testkit::pem_blocks(include_str!("../tests/fixtures/rfc9215-certs.pem"));
        let crls = crate::testkit::pem_blocks(include_str!("../tests/fixtures/rfc9215-crls.pem"));
        assert_eq!((certs.len(), crls.len()), (3, 3));
        for (der, crl) in certs.iter().zip(&crls) {
            let cert = Certificate::from_der(der).unwrap();
            let spki = cert.tbs_certificate().subject_public_key_info();
            let parts = crate::ber::read(der).unwrap().0.children().unwrap();
            let sig_alg = content(parts[1].raw);
            let sig = &parts[2].value[1..];
            assert!(verify_signed(&sig_alg, spki, parts[0].raw, sig), "сертификат {:?}", cert.tbs_certificate().subject());
            let mut bad = sig.to_vec();
            bad[5] ^= 1;
            assert!(!verify_signed(&sig_alg, spki, parts[0].raw, &bad));
            let cparts = crate::ber::read(crl).unwrap().0.children().unwrap();
            assert!(verify_signed(&content(cparts[1].raw), spki, cparts[0].raw, &cparts[2].value[1..]), "CRL");
            assert!(matches!(key_kind(spki), KeyKind::Gost { .. }), "{:?}", key_kind(spki));
        }
    }

    #[test]
    fn cms_schemes() {
        let alg = |oid: Oid| AlgorithmIdentifierOwned { oid, parameters: None };
        assert_eq!(cms_scheme(&alg(oid::RSA_ENCRYPTION), Hash::Sha256), Ok(Scheme::RsaPkcs1(Hash::Sha256)));
        assert_eq!(cms_scheme(&alg(oid::SHA1_RSA), Hash::Sha1), Ok(Scheme::RsaPkcs1(Hash::Sha1)));
        assert!(cms_scheme(&alg(oid::SHA1_RSA), Hash::Sha256).is_err(), "хэш подписи и digestAlgorithm расходятся");
        assert_eq!(cms_scheme(&alg(oid::GOST_KEY_256), Hash::Streebog256), Ok(Scheme::Gost256));
        assert_eq!(cms_scheme(&alg(oid::GOST_SIGN_512), Hash::Streebog512), Ok(Scheme::Gost512));
        assert!(cms_scheme(&alg(oid::GOST_SIGN_512), Hash::Streebog256).is_err());
        assert!(cms_scheme(&alg(oid::MD5_RSA), Hash::Sha256).is_err());
        assert!(cms_scheme(&alg(oid::RSA_PSS), Hash::Sha256).is_err(), "PSS без параметров — SHA-1, не SHA-256");
        assert_eq!(cms_scheme(&alg(oid::RSA_PSS), Hash::Sha1), Err(Unsupported("RSASSA-PSS parameters".into())));
    }

    #[test]
    fn hashes() {
        // ГОСТ Р 34.11-2012, пример 1 (M1): 63 байта «0123…012».
        let m1 = b"012345678901234567890123456789012345678901234567890123456789012";
        // В стандарте сообщение и хэш записаны числами; байты — в обратном порядке (little-endian).
        let h = Hash::Streebog256.digest(m1);
        assert_eq!(h.iter().map(|b| format!("{b:02x}")).collect::<String>(), "9d151eefd8590b89daa6ba6cb74af9275dd051026bb149a452fd84e5e57b5500");
        assert_eq!(Hash::Sha256.digest(b"abc").len(), 32);
        assert!(Hash::Sha1.weak() && !Hash::Sha256.weak());
    }
}
