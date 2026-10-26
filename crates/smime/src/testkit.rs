//! Тестовый набор (фича `testkit`, только тесты и генератор демо-образцов): ключи, сертификаты X.509, SignedData
//! и письма S/MIME. **Ключи — только для тестов**: RSA — из файлов `testkit/*.der` (OpenSSL, 2048 бит), EC и ГОСТ —
//! из строки-семени (детерминированно).
//!
//! Сертификат собирается своей структурой TBS (поля `x509-cert` закрыты), SignedData — типами `cms`. Подпись ГОСТ —
//! тестовый подписант `gost::sign` (детерминированный `k`): не для настоящих писем.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // тестовый набор: сбой — ошибка теста

use std::str::FromStr;
use std::time::{Duration, SystemTime};

use cms::cert::{CertificateChoices, IssuerAndSerialNumber};
use cms::content_info::{CmsVersion, ContentInfo};
use cms::signed_data::{CertificateSet, EncapsulatedContentInfo, SignedData, SignerIdentifier, SignerInfo, SignerInfos};
use const_oid::ObjectIdentifier as Oid;
use crypto_bigint::U512;
use der::asn1::{Any, BitString, Ia5String, Null, OctetString, SetOfVec, Uint};
use der::{Decode, Encode, Sequence};
use digest::Digest;
use rsa::RsaPrivateKey;
use rsa::pkcs1::DecodeRsaPrivateKey;
use spki::{AlgorithmIdentifierOwned, SubjectPublicKeyInfoOwned};
use x509_cert::Certificate;
use x509_cert::attr::Attribute;
use x509_cert::ext::Extension;
use x509_cert::ext::pkix::name::GeneralName;
use x509_cert::ext::pkix::{BasicConstraints, ExtendedKeyUsage, KeyUsage, KeyUsages, SubjectAltName, SubjectKeyIdentifier};
use x509_cert::name::Name;
use x509_cert::serial_number::SerialNumber;
use x509_cert::time::{Time, Validity};

use crate::algs::{Hash, Scheme};
use crate::ec;
use crate::gost;
use crate::oid;

pub use crate::oid as oids;

/// Начало 2026 года и 2046-й: срок тестовых сертификатов по умолчанию.
pub const Y2026: i64 = 1_767_225_600;
pub const Y2046: i64 = 2_398_377_600;
/// «Сейчас» тестов: 2026-10-21.
pub const NOW: u64 = 1_792_540_800;

/// Тестовый ключ.
pub enum Key {
    Rsa(Box<RsaPrivateKey>),
    P256(p256::ecdsa::SigningKey),
    P384(p384::ecdsa::SigningKey),
    Gost { set: Oid, size: gost::Size, d: U512 },
}

fn seed(label: &str, n: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0u32;
    while out.len() < n {
        out.extend(sha2::Sha512::digest(format!("{label}/{i}").as_bytes()));
        i += 1;
    }
    out.truncate(n);
    out
}

impl Key {
    /// RSA-2048 «A», «B», «C» (разные ключи).
    pub fn rsa(which: char) -> Key {
        // PKCS#1 `RSAPrivateKey` (DER, `openssl genpkey -outform DER`).
        let der: &[u8] = match which {
            'a' => include_bytes!("testkit/rsa-a.der"),
            'b' => include_bytes!("testkit/rsa-b.der"),
            _ => include_bytes!("testkit/rsa-c.der"),
        };
        Key::Rsa(Box::new(RsaPrivateKey::from_pkcs1_der(der).unwrap()))
    }

    pub fn p256(label: &str) -> Key {
        let mut s = seed(label, 32);
        s[0] &= 0x7f;
        Key::P256(p256::ecdsa::SigningKey::from_slice(&s).unwrap())
    }

    pub fn p384(label: &str) -> Key {
        let mut s = seed(label, 48);
        s[0] &= 0x7f;
        Key::P384(p384::ecdsa::SigningKey::from_slice(&s).unwrap())
    }

    /// ГОСТ Р 34.10-2012 на наборе `set` (256 или 512 бит — по набору).
    pub fn gost(label: &str, set: Oid) -> Key {
        let curve = ec::curve(&set).expect("набор ГОСТ");
        let size = if curve.bytes == 32 { gost::Size::S256 } else { gost::Size::S512 };
        let d = curve.reduce(&ec::uint_be(&seed(label, curve.bytes)).unwrap());
        Key::Gost { set, size, d }
    }

    /// `SubjectPublicKeyInfo`.
    pub fn spki(&self) -> SubjectPublicKeyInfoOwned {
        match self {
            Key::Rsa(k) => {
                use rsa::pkcs8::EncodePublicKey;
                let der = k.to_public_key().to_public_key_der().unwrap();
                SubjectPublicKeyInfoOwned::from_der(der.as_bytes()).unwrap()
            }
            Key::P256(k) => ec_spki(oid::P256, k.verifying_key().to_sec1_point(false).as_bytes()),
            Key::P384(k) => ec_spki(oid::P384, k.verifying_key().to_sec1_point(false).as_bytes()),
            Key::Gost { set, size, d } => {
                let curve = ec::curve(set).unwrap();
                let (x, y) = curve.mul_g(d).unwrap();
                let mut point = ec::to_be(&x, curve.bytes);
                point.reverse();
                let mut yl = ec::to_be(&y, curve.bytes);
                yl.reverse();
                point.extend(yl);
                let key_oid = if *size == gost::Size::S256 { oid::GOST_KEY_256 } else { oid::GOST_KEY_512 };
                // 256-битные наборы КриптоПро — с digestParamSet (RFC 9215 §4.2), остальные — без.
                let digest = matches!(*set, oid::CRYPTOPRO_A | oid::CRYPTOPRO_B | oid::CRYPTOPRO_C | oid::GOST_TEST_256).then_some(oid::STREEBOG256);
                let params = Any::encode_from(&GostParams { set: *set, digest }).unwrap();
                SubjectPublicKeyInfoOwned {
                    algorithm: AlgorithmIdentifierOwned { oid: key_oid, parameters: Some(params) },
                    subject_public_key: BitString::from_bytes(&OctetString::new(point).unwrap().to_der().unwrap()).unwrap(),
                }
            }
        }
    }

    /// Схема подписи по умолчанию для ключа.
    pub fn default_scheme(&self) -> Scheme {
        match self {
            Key::Rsa(_) => Scheme::RsaPkcs1(Hash::Sha256),
            Key::P256(_) => Scheme::Ecdsa(Hash::Sha256),
            Key::P384(_) => Scheme::Ecdsa(Hash::Sha384),
            Key::Gost { size: gost::Size::S256, .. } => Scheme::Gost256,
            Key::Gost { .. } => Scheme::Gost512,
        }
    }

    /// `AlgorithmIdentifier` подписи сертификата (X.509) по схеме.
    pub fn cert_alg(scheme: Scheme) -> AlgorithmIdentifierOwned {
        let null = || Some(Any::encode_from(&Null).unwrap());
        let (oid, parameters) = match scheme {
            Scheme::RsaPkcs1(Hash::Sha1) => (oid::SHA1_RSA, null()),
            Scheme::RsaPkcs1(Hash::Sha384) => (oid::SHA384_RSA, null()),
            Scheme::RsaPkcs1(Hash::Sha512) => (oid::SHA512_RSA, null()),
            Scheme::RsaPkcs1(_) => (oid::SHA256_RSA, null()),
            Scheme::Ecdsa(Hash::Sha1) => (oid::ECDSA_SHA1, None),
            Scheme::Ecdsa(Hash::Sha384) => (oid::ECDSA_SHA384, None),
            Scheme::Ecdsa(Hash::Sha512) => (oid::ECDSA_SHA512, None),
            Scheme::Ecdsa(_) => (oid::ECDSA_SHA256, None),
            Scheme::Gost256 => (oid::GOST_SIGN_256, None),
            Scheme::Gost512 => (oid::GOST_SIGN_512, None),
            Scheme::RsaPss { .. } => panic!("PSS в тестовом наборе — образцы OpenSSL"),
        };
        AlgorithmIdentifierOwned { oid, parameters }
    }

    /// RSA — PKCS#8 DER (тесты CNG: импорт временного ключа).
    pub fn rsa_pkcs8(&self) -> Option<Vec<u8>> {
        use rsa::pkcs8::EncodePrivateKey;
        let Key::Rsa(k) = self else { return None };
        k.to_pkcs8_der().ok().map(|d| d.as_bytes().to_vec())
    }

    /// EC — точка X‖Y и закрытое число (тесты CNG: `BCRYPT_ECCPRIVATE_BLOB`).
    pub fn ec_raw(&self) -> Option<(Vec<u8>, Vec<u8>)> {
        match self {
            Key::P256(k) => Some((k.verifying_key().to_sec1_point(false).as_bytes()[1..].to_vec(), k.to_bytes().to_vec())),
            Key::P384(k) => Some((k.verifying_key().to_sec1_point(false).as_bytes()[1..].to_vec(), k.to_bytes().to_vec())),
            _ => None,
        }
    }

    /// Подпись `message` по схеме (хэш считается здесь).
    pub fn sign(&self, scheme: Scheme, message: &[u8]) -> Vec<u8> {
        self.sign_digest(scheme, &scheme.hash().digest(message))
    }

    /// Подпись готового хэша (как CNG `NCryptSignHash`).
    pub fn sign_digest(&self, scheme: Scheme, digest: &[u8]) -> Vec<u8> {
        use p256::ecdsa::signature::hazmat::PrehashSigner;
        let digest = digest.to_vec();
        match (self, scheme) {
            (Key::Rsa(k), Scheme::RsaPkcs1(h)) => {
                let pad = match h {
                    Hash::Sha1 => rsa::Pkcs1v15Sign::new::<sha1::Sha1>(),
                    Hash::Sha384 => rsa::Pkcs1v15Sign::new::<sha2::Sha384>(),
                    Hash::Sha512 => rsa::Pkcs1v15Sign::new::<sha2::Sha512>(),
                    _ => rsa::Pkcs1v15Sign::new::<sha2::Sha256>(),
                };
                k.sign(pad, &digest).unwrap()
            }
            (Key::P256(k), Scheme::Ecdsa(_)) => {
                let s: p256::ecdsa::Signature = k.sign_prehash(&digest).unwrap();
                s.to_der().as_bytes().to_vec()
            }
            (Key::P384(k), Scheme::Ecdsa(_)) => {
                let s: p384::ecdsa::Signature = k.sign_prehash(&digest).unwrap();
                s.to_der().as_bytes().to_vec()
            }
            (Key::Gost { set, d, .. }, Scheme::Gost256 | Scheme::Gost512) => {
                let curve = ec::curve(set).unwrap();
                let mut kd = d.to_be_bytes().as_ref().to_vec();
                kd.extend(&digest);
                let k = curve.reduce(&ec::uint_be(&seed(&format!("{kd:?}"), curve.bytes)).unwrap());
                gost::sign(curve, d, &digest, &k).unwrap()
            }
            _ => panic!("ключ и схема не совместимы"),
        }
    }
}

#[derive(Sequence)]
struct GostParams {
    set: Oid,
    digest: Option<Oid>,
}

fn ec_spki(curve: Oid, point: &[u8]) -> SubjectPublicKeyInfoOwned {
    SubjectPublicKeyInfoOwned {
        algorithm: AlgorithmIdentifierOwned { oid: oid::EC_PUBLIC_KEY, parameters: Some(Any::encode_from(&curve).unwrap()) },
        subject_public_key: BitString::from_bytes(point).unwrap(),
    }
}

/// Назначение (EKU) тестового сертификата.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Eku {
    /// «Защита электронной почты».
    Email,
    /// Только «Проверка подлинности сервера» — не для почты.
    Server,
    /// Без расширения EKU (RFC 5280: любое назначение).
    None,
}

/// Что выпустить.
#[derive(Clone)]
pub struct Spec {
    pub cn: String,
    pub org: Option<String>,
    pub email: Option<String>,
    pub ca: bool,
    pub eku: Eku,
    /// KeyUsage без `digitalSignature` (только шифрование).
    pub encrypt_only: bool,
    pub not_before: i64,
    pub not_after: i64,
    pub serial: u64,
    /// Схема подписи сертификата издателем (по умолчанию — по ключу издателя).
    pub scheme: Option<Scheme>,
    /// Адрес CRL (расширение CRL Distribution Points).
    pub crl_url: Option<String>,
}

impl Spec {
    pub fn ca(cn: &str) -> Spec {
        Spec {
            cn: cn.into(),
            org: Some("Postman Test".into()),
            email: None,
            ca: true,
            eku: Eku::None,
            encrypt_only: false,
            not_before: Y2026,
            not_after: Y2046,
            serial: 1,
            scheme: None,
            crl_url: None,
        }
    }

    pub fn user(cn: &str, email: &str) -> Spec {
        Spec { email: Some(email.into()), ca: false, eku: Eku::Email, serial: 0x1001, ..Spec::ca(cn) }
    }
}

/// Выпущенный сертификат.
#[derive(Clone)]
pub struct Issued {
    pub der: Vec<u8>,
    pub cert: Certificate,
}

#[derive(Sequence)]
struct Tbs {
    #[asn1(context_specific = "0", tag_mode = "EXPLICIT")]
    version: u8,
    serial: Uint,
    signature: AlgorithmIdentifierOwned,
    issuer: Name,
    validity: Validity,
    subject: Name,
    spki: SubjectPublicKeyInfoOwned,
    #[asn1(context_specific = "3", tag_mode = "EXPLICIT", optional = "true")]
    extensions: Option<Vec<Extension>>,
}

#[derive(Sequence)]
struct Cert {
    tbs: Tbs,
    alg: AlgorithmIdentifierOwned,
    sig: BitString,
}

fn time(t: i64) -> Time {
    Time::try_from(SystemTime::UNIX_EPOCH + Duration::from_secs(t as u64)).unwrap()
}

fn ext<T: Encode>(oid: Oid, critical: bool, v: &T) -> Extension {
    Extension { extn_id: oid, critical, extn_value: OctetString::new(v.to_der().unwrap()).unwrap() }
}

fn name(spec: &Spec) -> Name {
    let mut s = format!("CN={}", spec.cn);
    if let Some(o) = &spec.org {
        s.push_str(&format!(",O={o}"));
    }
    Name::from_str(&s).unwrap()
}

/// Выпустить сертификат: самоподписанный (`issuer` — `None`) или издателем.
pub fn issue(spec: &Spec, key: &Key, issuer: Option<(&Issued, &Key)>) -> Issued {
    let spki = key.spki();
    let signer = issuer.map_or(key, |(_, k)| k);
    let scheme = spec.scheme.unwrap_or_else(|| signer.default_scheme());
    let alg = Key::cert_alg(scheme);
    let subject = name(spec);
    let issuer_name = issuer.map_or_else(|| subject.clone(), |(i, _)| i.cert.tbs_certificate().subject().clone());
    let ski = sha1::Sha1::digest(spki.subject_public_key.raw_bytes()).to_vec();
    let mut exts = vec![
        ext(const_oid::db::rfc5280::ID_CE_BASIC_CONSTRAINTS, true, &BasicConstraints { ca: spec.ca, path_len_constraint: None }),
        ext(const_oid::db::rfc5280::ID_CE_SUBJECT_KEY_IDENTIFIER, false, &SubjectKeyIdentifier(OctetString::new(ski).unwrap())),
    ];
    let ku = if spec.ca {
        KeyUsages::KeyCertSign | KeyUsages::CRLSign | KeyUsages::DigitalSignature
    } else if spec.encrypt_only {
        KeyUsages::KeyEncipherment | KeyUsages::DataEncipherment
    } else {
        KeyUsages::DigitalSignature | KeyUsages::NonRepudiation | KeyUsages::KeyEncipherment | KeyUsages::KeyAgreement
    };
    exts.push(ext(const_oid::db::rfc5280::ID_CE_KEY_USAGE, true, &KeyUsage(ku)));
    match spec.eku {
        Eku::Email => exts.push(ext(const_oid::db::rfc5280::ID_CE_EXT_KEY_USAGE, false, &ExtendedKeyUsage(vec![oid::EKU_EMAIL_PROTECTION]))),
        Eku::Server => {
            exts.push(ext(const_oid::db::rfc5280::ID_CE_EXT_KEY_USAGE, false, &ExtendedKeyUsage(vec![const_oid::db::rfc5280::ID_KP_SERVER_AUTH])))
        }
        Eku::None => {}
    }
    if let Some(e) = &spec.email {
        exts.push(ext(
            const_oid::db::rfc5280::ID_CE_SUBJECT_ALT_NAME,
            false,
            &SubjectAltName(vec![GeneralName::Rfc822Name(Ia5String::new(e).unwrap())]),
        ));
    }
    if let Some(url) = &spec.crl_url {
        // CRLDistributionPoints ::= SEQUENCE OF SEQUENCE { [0] { [0] fullName { [6] URI } } }
        let mut uri = crate::ber::header(0x86, url.len());
        uri.extend_from_slice(url.as_bytes());
        let wrap = |tag: u8, inner: Vec<u8>| {
            let mut out = crate::ber::header(tag, inner.len());
            out.extend(inner);
            out
        };
        let value = wrap(0x30, wrap(0x30, wrap(0xA0, wrap(0xA0, uri))));
        exts.push(Extension {
            extn_id: const_oid::db::rfc5280::ID_CE_CRL_DISTRIBUTION_POINTS,
            critical: false,
            extn_value: OctetString::new(value).unwrap(),
        });
    }
    let tbs = Tbs {
        version: 2,
        serial: Uint::new(&spec.serial.to_be_bytes()).unwrap(),
        signature: alg.clone(),
        issuer: issuer_name,
        validity: Validity::new(time(spec.not_before), time(spec.not_after)),
        subject,
        spki,
        extensions: Some(exts),
    };
    let sig = signer.sign(scheme, &tbs.to_der().unwrap());
    let der = Cert { tbs, alg, sig: BitString::from_bytes(&sig).unwrap() }.to_der().unwrap();
    let cert = Certificate::from_der(&der).unwrap();
    Issued { der, cert }
}

#[derive(Sequence)]
struct Revoked {
    serial: Uint,
    date: Time,
}

#[derive(Sequence)]
struct TbsCrl {
    version: u8,
    signature: AlgorithmIdentifierOwned,
    issuer: Name,
    this_update: Time,
    next_update: Time,
    revoked: Option<Vec<Revoked>>,
}

#[derive(Sequence)]
struct Crl {
    tbs: TbsCrl,
    alg: AlgorithmIdentifierOwned,
    sig: BitString,
}

/// CRL издателя `issuer` (v2) с отозванными серийными номерами `revoked`, действует до `next_update`.
pub fn crl(issuer: &Issued, key: &Key, revoked: &[u64], this_update: i64, next_update: i64) -> Vec<u8> {
    let scheme = key.default_scheme();
    let alg = Key::cert_alg(scheme);
    let list: Vec<Revoked> = revoked.iter().map(|s| Revoked { serial: Uint::new(&s.to_be_bytes()).unwrap(), date: time(this_update) }).collect();
    let tbs = TbsCrl {
        version: 1,
        signature: alg.clone(),
        issuer: issuer.cert.tbs_certificate().subject().clone(),
        this_update: time(this_update),
        next_update: time(next_update),
        revoked: (!list.is_empty()).then_some(list),
    };
    let sig = key.sign(scheme, &tbs.to_der().unwrap());
    Crl { tbs, alg, sig: BitString::from_bytes(&sig).unwrap() }.to_der().unwrap()
}

/// Как подписывать (по умолчанию — как Outlook: подписанные атрибуты, сертификаты подписанта и CA в письме).
#[derive(Clone)]
pub struct Sign {
    pub scheme: Option<Scheme>,
    pub signing_time: Option<i64>,
    pub capabilities: Vec<Oid>,
    /// `sid` — SubjectKeyIdentifier (иначе издатель и серийный номер).
    pub by_ski: bool,
    pub attrs: bool,
    /// Подпись внутри (`signed-data`): содержимое в `eContent`.
    pub opaque: bool,
    /// Не класть сертификат подписанта в письмо.
    pub omit_signer: bool,
    /// `signatureAlgorithm` как у Outlook (`rsaEncryption`) / КриптоПро (OID ключа ГОСТ); иначе — «с хэшем».
    pub key_oid: bool,
}

impl Default for Sign {
    fn default() -> Self {
        Sign {
            scheme: None,
            signing_time: Some(NOW as i64 - 3600),
            capabilities: vec![oid::AES256_GCM, oid::AES256_CBC, oid::AES128_CBC],
            by_ski: false,
            attrs: true,
            opaque: false,
            omit_signer: false,
            key_oid: true,
        }
    }
}

fn attribute<T: der::EncodeValue + der::Tagged>(oid: Oid, v: &T) -> Attribute {
    Attribute { oid, values: SetOfVec::try_from(vec![Any::encode_from(v).unwrap()]).unwrap() }
}

#[derive(Sequence)]
struct Capability {
    id: Oid,
}

/// SignedData (`ContentInfo`, DER) над `content`: подписант `signer` с ключом `key`, `chain` — ещё сертификаты.
pub fn signed_data(content: &[u8], signer: &Issued, key: &Key, chain: &[&Issued], opts: &Sign) -> Vec<u8> {
    let scheme = opts.scheme.unwrap_or_else(|| key.default_scheme());
    let hash = scheme.hash();
    let digest_alg = AlgorithmIdentifierOwned { oid: hash.oid(), parameters: None };
    let tbs = signer.cert.tbs_certificate();
    let (version, sid) = if opts.by_ski {
        let (_, ski) = tbs.get_extension::<SubjectKeyIdentifier>().unwrap().unwrap();
        (CmsVersion::V3, SignerIdentifier::SubjectKeyIdentifier(ski))
    } else {
        let isn = IssuerAndSerialNumber { issuer: tbs.issuer().clone(), serial_number: SerialNumber::new(tbs.serial_number().as_bytes()).unwrap() };
        (CmsVersion::V1, SignerIdentifier::IssuerAndSerialNumber(isn))
    };
    let (signed_attrs, to_sign) = if opts.attrs {
        let mut attrs = vec![
            attribute(oid::ATTR_CONTENT_TYPE, &oid::DATA),
            attribute(oid::ATTR_MESSAGE_DIGEST, &OctetString::new(hash.digest(content)).unwrap()),
        ];
        if let Some(t) = opts.signing_time {
            attrs.push(attribute(oid::ATTR_SIGNING_TIME, &time(t)));
        }
        if !opts.capabilities.is_empty() {
            let caps: Vec<Capability> = opts.capabilities.iter().map(|&id| Capability { id }).collect();
            attrs.push(attribute(oid::ATTR_SMIME_CAPABILITIES, &caps));
        }
        let set = SetOfVec::try_from(attrs).unwrap();
        let der = set.to_der().unwrap();
        (Some(set), der)
    } else {
        (None, content.to_vec())
    };
    let signature = key.sign(scheme, &to_sign);
    let null = || Some(Any::encode_from(&Null).unwrap());
    let signature_algorithm = match (scheme, opts.key_oid) {
        (Scheme::RsaPkcs1(_), true) => AlgorithmIdentifierOwned { oid: oid::RSA_ENCRYPTION, parameters: null() },
        (Scheme::Gost256, true) => AlgorithmIdentifierOwned { oid: oid::GOST_KEY_256, parameters: None },
        (Scheme::Gost512, true) => AlgorithmIdentifierOwned { oid: oid::GOST_KEY_512, parameters: None },
        _ => Key::cert_alg(scheme),
    };
    let si = SignerInfo {
        version,
        sid,
        digest_alg: digest_alg.clone(),
        signed_attrs,
        signature_algorithm,
        signature: OctetString::new(signature).unwrap(),
        unsigned_attrs: None,
    };
    let mut certs: Vec<CertificateChoices> = Vec::new();
    if !opts.omit_signer {
        certs.push(CertificateChoices::Certificate(signer.cert.clone()));
    }
    certs.extend(chain.iter().map(|c| CertificateChoices::Certificate(c.cert.clone())));
    let econtent = opts.opaque.then(|| Any::encode_from(&OctetString::new(content.to_vec()).unwrap()).unwrap());
    let sd = SignedData {
        version,
        digest_algorithms: SetOfVec::try_from(vec![digest_alg]).unwrap(),
        encap_content_info: EncapsulatedContentInfo { econtent_type: oid::DATA, econtent },
        certificates: (!certs.is_empty()).then(|| CertificateSet::try_from(certs).unwrap()),
        crls: None,
        signer_infos: SignerInfos::try_from(vec![si]).unwrap(),
    };
    ContentInfo { content_type: oid::SIGNED_DATA, content: Any::encode_from(&sd).unwrap() }.to_der().unwrap()
}

/// Base64 со строками по 76 знаков и CRLF.
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for (i, c) in data.chunks(3).enumerate() {
        if i > 0 && i % 19 == 0 {
            out.push_str("\r\n");
        }
        let n = (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Base64 → байты (пробелы и переводы строк пропускаются).
pub fn unbase64(text: &str) -> Vec<u8> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let digits: Vec<u8> = text.bytes().filter_map(val).collect();
    let mut out = Vec::new();
    for c in digits.chunks(4) {
        let n = c.iter().enumerate().fold(0u32, |acc, (i, &d)| acc | (u32::from(d) << (18 - 6 * i)));
        out.push((n >> 16) as u8);
        if c.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if c.len() > 3 {
            out.push(n as u8);
        }
    }
    out
}

/// Блоки PEM (`-----BEGIN …-----` … `-----END …-----`) → DER.
pub fn pem_blocks(text: &str) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("-----BEGIN") {
            cur = Some(String::new());
        } else if line.starts_with("-----END") {
            out.extend(cur.take().map(|b| unbase64(&b)));
        } else if let Some(b) = &mut cur {
            b.push_str(line);
        }
    }
    out
}

/// Внутренняя часть письма: `text/html` (UTF-8, base64) — то, что подписывается.
pub fn html_part(html: &str) -> Vec<u8> {
    format!("Content-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n", base64(html.as_bytes())).into_bytes()
}

/// То же — `text/plain`.
pub fn text_part(text: &str) -> Vec<u8> {
    format!("Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n", base64(text.as_bytes())).into_bytes()
}

/// Заголовки письма.
pub struct Head<'a> {
    pub from: &'a str,
    pub to: &'a str,
    pub subject: &'a str,
    pub date: &'a str,
}

fn head(h: &Head<'_>) -> String {
    format!(
        "From: {}\r\nTo: {}\r\nSubject: =?utf-8?B?{}?=\r\nDate: {}\r\nMessage-ID: <{}@postman.test>\r\nMIME-Version: 1.0\r\n",
        h.from,
        h.to,
        base64(h.subject.as_bytes()).replace("\r\n", ""),
        h.date,
        sha2::Sha256::digest(h.subject.as_bytes()).iter().take(8).map(|b| format!("{b:02x}")).collect::<String>()
    )
}

/// Письмо `multipart/signed` (отдельная подпись) над частью `inner` и подписью `p7s` (DER).
pub fn multipart_signed(h: &Head<'_>, inner: &[u8], p7s: &[u8], micalg: &str) -> Vec<u8> {
    let boundary = "----=_Part_Postman_Test_0001";
    let mut out = head(h).into_bytes();
    out.extend(
        format!(
            "Content-Type: multipart/signed; protocol=\"application/pkcs7-signature\"; micalg={micalg}; boundary=\"{boundary}\"\r\n\r\nThis is an S/MIME signed message\r\n\r\n--{boundary}\r\n"
        )
        .as_bytes(),
    );
    out.extend_from_slice(inner);
    out.extend(
        format!(
            "\r\n--{boundary}\r\nContent-Type: application/pkcs7-signature; name=\"smime.p7s\"\r\nContent-Transfer-Encoding: base64\r\nContent-Disposition: attachment; filename=\"smime.p7s\"\r\n\r\n{}\r\n\r\n--{boundary}--\r\n",
            base64(p7s)
        )
        .as_bytes(),
    );
    out
}

/// Письмо `application/pkcs7-mime; smime-type=signed-data` (подпись внутри).
pub fn opaque_signed(h: &Head<'_>, cms: &[u8]) -> Vec<u8> {
    let mut out = head(h).into_bytes();
    out.extend(
        format!(
            "Content-Type: application/pkcs7-mime; smime-type=signed-data; name=\"smime.p7m\"\r\nContent-Transfer-Encoding: base64\r\nContent-Disposition: attachment; filename=\"smime.p7m\"\r\n\r\n{}\r\n",
            base64(cms)
        )
        .as_bytes(),
    );
    out
}

/// micalg по хэшу (RFC 8551 §3.5.3.2; ГОСТ — как КриптоПро [проверить]).
pub fn micalg(hash: Hash) -> &'static str {
    match hash {
        Hash::Sha1 => "sha-1",
        Hash::Sha256 => "sha-256",
        Hash::Sha384 => "sha-384",
        Hash::Sha512 => "sha-512",
        Hash::Streebog256 => "gostr3411-2012-256",
        Hash::Streebog512 => "gostr3411-2012-512",
    }
}

/// Всё сразу: подписать HTML-часть и собрать письмо.
pub fn signed_letter(h: &Head<'_>, html: &str, signer: &Issued, key: &Key, chain: &[&Issued], opts: &Sign) -> Vec<u8> {
    let inner = html_part(html);
    let scheme = opts.scheme.unwrap_or_else(|| key.default_scheme());
    if opts.opaque {
        opaque_signed(h, &signed_data(&inner, signer, key, chain, opts))
    } else {
        let p7s = signed_data(&inner, signer, key, chain, opts);
        multipart_signed(h, &inner, &p7s, micalg(scheme.hash()))
    }
}

/// Тестовый PKI: корень и пользователь, подписанный им.
pub struct Pki {
    pub root: Issued,
    pub root_key: Key,
    pub user: Issued,
    pub user_key: Key,
}

impl Pki {
    /// RSA: корень «A», пользователь «B».
    pub fn rsa(root_cn: &str, user_cn: &str, email: &str) -> Pki {
        Pki::build(Key::rsa('a'), Key::rsa('b'), root_cn, user_cn, email)
    }

    pub fn build(root_key: Key, user_key: Key, root_cn: &str, user_cn: &str, email: &str) -> Pki {
        let root = issue(&Spec::ca(root_cn), &root_key, None);
        let user = issue(&Spec::user(user_cn, email), &user_key, Some((&root, &root_key)));
        Pki { root, root_key, user, user_key }
    }

    pub fn trust(&self) -> crate::Trust {
        crate::Trust { roots: vec![crate::Anchor { der: self.root.der.clone(), corporate: true }], ..Default::default() }
    }
}

/// Подписант исходящих для тестов (Ф3): сертификат, ключ и промежуточные (`crate::sign::Signer`).
pub struct TestSigner<'a> {
    pub cert: &'a Issued,
    pub key: &'a Key,
    pub chain: Vec<Vec<u8>>,
}

impl crate::sign::Signer for TestSigner<'_> {
    fn certificate(&self) -> &[u8] {
        &self.cert.der
    }

    fn chain(&self) -> &[Vec<u8>] {
        &self.chain
    }

    fn scheme(&self) -> Scheme {
        self.key.default_scheme()
    }

    fn sign(&self, digest: &[u8]) -> Result<Vec<u8>, crate::sign::SignError> {
        Ok(self.key.sign_digest(self.key.default_scheme(), digest))
    }
}

// --- шифрованные письма (Ф2) ---

use p256::elliptic_curve::sec1::ToSec1Point as _;

/// Детерминированный «случайный» источник для тестового шифрования RSA (rand_core 0.10).
pub struct TestRng(u64);

impl TestRng {
    pub fn new(seed: u64) -> TestRng {
        TestRng(seed.max(1))
    }
}

impl rand_core::TryRng for TestRng {
    type Error = std::convert::Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        self.try_next_u64().map(|v| v as u32)
    }

    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        // xorshift64*: тестам нужна воспроизводимость, не стойкость.
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        Ok(self.0.wrapping_mul(0x2545_F491_4F6C_DD1D))
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
        for chunk in dst.chunks_mut(8) {
            let v = self.try_next_u64()?.to_le_bytes();
            chunk.copy_from_slice(&v[..chunk.len()]);
        }
        Ok(())
    }
}

impl rand_core::TryCryptoRng for TestRng {}

/// Как зашифровать ключ содержимого для получателя.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAlg {
    RsaPkcs1,
    RsaOaep(Hash),
    /// ECDH P-256/P-384, KDF этим хэшем, AES Key Wrap ключом `bits`.
    Ecdh(Hash, u16),
    /// ГОСТ Р 34.10 (`GostR3410-KeyTransport`, RFC 4490): поля — правдоподобные, ключ не снимается (только разбор:
    /// снимает СКЗИ).
    GostTransport,
}

/// Получатель: сертификат, его ключ (для ECDH — открытый из него), алгоритм; `ski` — идентификатор SKI вместо
/// издателя и серийного номера.
pub struct To<'a> {
    pub cert: &'a Issued,
    pub key: &'a Key,
    pub alg: KeyAlg,
    pub ski: bool,
}

fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = crate::ber::header(tag, content.len());
    out.extend_from_slice(content);
    out
}

fn seq(parts: &[Vec<u8>]) -> Vec<u8> {
    tlv(0x30, &parts.concat())
}

fn oid_der(o: Oid) -> Vec<u8> {
    o.to_der().unwrap()
}

fn alg_der(o: Oid, params: Option<Vec<u8>>) -> Vec<u8> {
    let mut parts = vec![oid_der(o)];
    parts.extend(params);
    seq(&parts)
}

fn hash_alg_der(h: Hash) -> Vec<u8> {
    alg_der(h.oid(), None)
}

/// AES Key Wrap (RFC 3394) — тестам, чтобы собрать ECDH-получателя.
pub fn wrap_key(kek: &[u8], key: &[u8]) -> Vec<u8> {
    use aes::cipher::{BlockEncrypt, KeyInit};
    fn run<C: BlockEncrypt + KeyInit + aes::cipher::BlockSizeUser<BlockSize = aes::cipher::consts::U16>>(kek: &[u8], key: &[u8]) -> Vec<u8> {
        let cipher = C::new_from_slice(kek).unwrap();
        let n = key.len() / 8;
        let mut a = [0xA6u8; 8];
        let mut r = key.to_vec();
        for j in 0..6 {
            for i in 1..=n {
                let mut block = aes::cipher::Block::<C>::default();
                block[..8].copy_from_slice(&a);
                block[8..].copy_from_slice(&r[(i - 1) * 8..i * 8]);
                cipher.encrypt_block(&mut block);
                let t = ((n * j) + i) as u64;
                a.copy_from_slice(&block[..8]);
                for (k, b) in t.to_be_bytes().iter().enumerate() {
                    a[k] ^= b;
                }
                r[(i - 1) * 8..i * 8].copy_from_slice(&block[8..]);
            }
        }
        [a.to_vec(), r].concat()
    }
    match kek.len() {
        16 => run::<aes::Aes128>(kek, key),
        24 => run::<aes::Aes192>(kek, key),
        _ => run::<aes::Aes256>(kek, key),
    }
}

fn rid_der(cert: &Issued, ski: bool, kari: bool) -> Vec<u8> {
    let tbs = cert.cert.tbs_certificate();
    if ski {
        let id = match tbs.get_extension::<SubjectKeyIdentifier>() {
            Ok(Some((_, s))) => s.0.as_bytes().to_vec(),
            _ => panic!("нет SKI"),
        };
        // ktri: [0] IMPLICIT OCTET STRING; kari: [0] IMPLICIT RecipientKeyIdentifier { OCTET STRING }.
        if kari { tlv(0xA0, &tlv(0x04, &id)) } else { tlv(0x80, &id) }
    } else {
        seq(&[tbs.issuer().to_der().unwrap(), tbs.serial_number().to_der().unwrap()])
    }
}

fn kdf_oid(h: Hash) -> Oid {
    match h {
        Hash::Sha1 => oid::ECDH_STD_SHA1,
        Hash::Sha384 => oid::ECDH_STD_SHA384,
        Hash::Sha512 => oid::ECDH_STD_SHA512,
        _ => oid::ECDH_STD_SHA256,
    }
}

fn wrap_alg_oid(bits: u16) -> Oid {
    match bits {
        128 => oid::AES128_WRAP,
        192 => oid::AES192_WRAP,
        _ => oid::AES256_WRAP,
    }
}

fn recipient_info(to: &To<'_>, cek: &[u8], n: u64) -> Vec<u8> {
    let mut rng = TestRng::new(n.wrapping_mul(7919) + 3);
    match (to.alg, to.key) {
        (KeyAlg::RsaPkcs1 | KeyAlg::RsaOaep(_), Key::Rsa(k)) => {
            let public = k.to_public_key();
            let (alg, ek) = match to.alg {
                KeyAlg::RsaOaep(h) => {
                    let ek = match h {
                        Hash::Sha1 => public.encrypt(&mut rng, rsa::Oaep::<sha1::Sha1>::new(), cek),
                        Hash::Sha384 => public.encrypt(&mut rng, rsa::Oaep::<sha2::Sha384>::new(), cek),
                        Hash::Sha512 => public.encrypt(&mut rng, rsa::Oaep::<sha2::Sha512>::new(), cek),
                        _ => public.encrypt(&mut rng, rsa::Oaep::<sha2::Sha256>::new(), cek),
                    }
                    .unwrap();
                    // RSAES-OAEP-params: [0] hash, [1] MGF1(hash); SHA-1 — по умолчанию (пусто).
                    let params = if h == Hash::Sha1 {
                        seq(&[])
                    } else {
                        seq(&[tlv(0xA0, &hash_alg_der(h)), tlv(0xA1, &alg_der(oid::MGF1, Some(hash_alg_der(h))))])
                    };
                    (alg_der(oid::RSA_OAEP, Some(params)), ek)
                }
                _ => (alg_der(oid::RSA_ENCRYPTION, Some(vec![0x05, 0x00])), public.encrypt(&mut rng, rsa::Pkcs1v15Encrypt, cek).unwrap()),
            };
            let version = if to.ski { 2 } else { 0 };
            seq(&[tlv(0x02, &[version]), rid_der(to.cert, to.ski, false), alg, tlv(0x04, &ek)])
        }
        (KeyAlg::Ecdh(kdf, bits), Key::P256(k)) => {
            let eph = p256::SecretKey::from_slice(&seed(&format!("eph-{n}"), 32)).unwrap();
            let peer = p256::PublicKey::from(k.verifying_key());
            let z = p256::ecdh::diffie_hellman(eph.to_nonzero_scalar(), peer.as_affine());
            let point = eph.public_key().to_sec1_point(false);
            ecdh_ri(to, z.raw_secret_bytes(), point.as_bytes(), kdf, bits, cek)
        }
        (KeyAlg::Ecdh(kdf, bits), Key::P384(k)) => {
            let eph = p384::SecretKey::from_slice(&seed(&format!("eph-{n}"), 48)).unwrap();
            let peer = p384::PublicKey::from(k.verifying_key());
            let z = p384::ecdh::diffie_hellman(eph.to_nonzero_scalar(), peer.as_affine());
            let point = eph.public_key().to_sec1_point(false);
            ecdh_ri(to, z.raw_secret_bytes(), point.as_bytes(), kdf, bits, cek)
        }
        (KeyAlg::GostTransport, Key::Gost { .. }) => {
            let spki = to.key.spki();
            let key_oid = spki.algorithm.oid;
            let params = spki.algorithm.parameters.as_ref().unwrap().to_der().unwrap();
            // Эфемерный ключ — открытый ключ получателя (разбор не проверяет математику).
            let bits = [vec![0u8], spki.subject_public_key.raw_bytes().to_vec()].concat();
            let eph = tlv(0xA0, &[seq(&[oid_der(key_oid), params]), tlv(0x03, &bits)].concat());
            let sek = seq(&[tlv(0x04, &seed("gost-ek", 32)), tlv(0x04, &seed("gost-mac", 4))]);
            let tparams = [oid_der(oid::GOST28147_PARAM_A), eph, tlv(0x04, &seed("gost-ukm", 8))].concat();
            let transport = seq(&[sek, tlv(0xA0, &tparams)]);
            seq(&[tlv(0x02, &[0]), rid_der(to.cert, to.ski, false), alg_der(key_oid, None), tlv(0x04, &transport)])
        }
        _ => panic!("ключ не подходит к алгоритму"),
    }
}

fn ecdh_ri(to: &To<'_>, z: &[u8], point: &[u8], kdf: Hash, bits: u16, cek: &[u8]) -> Vec<u8> {
    let ukm = b"postman-ukm".to_vec();
    let kek = crate::envelope::ecdh_kek(z, kdf, bits, Some(&ukm)).unwrap();
    let ek = wrap_key(&kek, cek);
    let mut bit = vec![0u8];
    bit.extend_from_slice(point);
    let originator = tlv(0xA0, &tlv(0xA1, &[alg_der(oid::EC_PUBLIC_KEY, None), tlv(0x03, &bit)].concat()));
    let ukm = tlv(0xA1, &tlv(0x04, &ukm));
    let alg = alg_der(kdf_oid(kdf), Some(alg_der(wrap_alg_oid(bits), None)));
    let keys = seq(&[seq(&[rid_der(to.cert, to.ski, true), tlv(0x04, &ek)])]);
    tlv(0xA1, &[tlv(0x02, &[3]), originator, ukm, alg, keys].concat())
}

/// Собрать `ContentInfo` с EnvelopedData (CBC, 3DES) или AuthEnvelopedData (GCM) над `content` ключом `cek`.
/// `tamper` — испортить последний байт шифра (тег GCM или блок CBC).
pub fn enveloped(content: &[u8], to: &[To<'_>], cipher: crate::envelope::Cipher, cek: &[u8], tamper: bool) -> Vec<u8> {
    use crate::envelope::Cipher;
    use aes::cipher::block_padding::Pkcs7;
    use aes::cipher::{BlockEncryptMut, KeyIvInit};
    let infos: Vec<u8> = to.iter().enumerate().map(|(i, t)| recipient_info(t, cek, i as u64)).collect::<Vec<_>>().concat();
    let iv: Vec<u8> = seed("iv", 16);
    let data_ct = oid_der(oid::DATA);
    let (alg, mut ct, mut mac) = match cipher {
        Cipher::AesCbc(bits) => {
            let o = match bits {
                128 => oid::AES128_CBC,
                192 => oid::AES192_CBC,
                _ => oid::AES256_CBC,
            };
            let ct = match bits {
                128 => cbc::Encryptor::<aes::Aes128>::new_from_slices(cek, &iv).unwrap().encrypt_padded_vec_mut::<Pkcs7>(content),
                192 => cbc::Encryptor::<aes::Aes192>::new_from_slices(cek, &iv).unwrap().encrypt_padded_vec_mut::<Pkcs7>(content),
                _ => cbc::Encryptor::<aes::Aes256>::new_from_slices(cek, &iv).unwrap().encrypt_padded_vec_mut::<Pkcs7>(content),
            };
            (alg_der(o, Some(tlv(0x04, &iv))), ct, None)
        }
        Cipher::Des3 => {
            let ct = cbc::Encryptor::<des::TdesEde3>::new_from_slices(cek, &iv[..8]).unwrap().encrypt_padded_vec_mut::<Pkcs7>(content);
            (alg_der(oid::DES_EDE3_CBC, Some(tlv(0x04, &iv[..8]))), ct, None)
        }
        // ГОСТ 28147-89: шифровать нечем (только СКЗИ) — содержимое как есть, параметры настоящие.
        Cipher::Gost28147 => (alg_der(oid::GOST28147, Some(seq(&[tlv(0x04, &iv[..8]), oid_der(oid::GOST28147_PARAM_A)]))), content.to_vec(), None),
        Cipher::AesGcm(bits) => {
            use aes_gcm::aead::AeadInPlace;
            use aes_gcm::{Aes128Gcm, Aes256Gcm, KeyInit, Nonce};
            let nonce = &iv[..12];
            let mut buf = content.to_vec();
            let tag = match bits {
                128 => Aes128Gcm::new_from_slice(cek).unwrap().encrypt_in_place_detached(Nonce::from_slice(nonce), &[], &mut buf).unwrap().to_vec(),
                _ => Aes256Gcm::new_from_slice(cek).unwrap().encrypt_in_place_detached(Nonce::from_slice(nonce), &[], &mut buf).unwrap().to_vec(),
            };
            let o = if bits == 128 { oid::AES128_GCM } else { oid::AES256_GCM };
            // GCMParameters: nonce и длина тега 16.
            (alg_der(o, Some(seq(&[tlv(0x04, nonce), tlv(0x02, &[16])]))), buf, Some(tag))
        }
    };
    if tamper {
        match mac.as_mut() {
            Some(t) => t[0] ^= 1,
            None => {
                if let Some(b) = ct.last_mut() {
                    *b ^= 1;
                }
            }
        }
    }
    let eci = seq(&[data_ct, alg, tlv(0x80, &ct)]);
    let any_v2 = to.iter().any(|t| t.ski || matches!(t.alg, KeyAlg::Ecdh(..)));
    let (ct_oid, body) = match mac {
        Some(tag) => (oid::AUTH_ENVELOPED_DATA, seq(&[tlv(0x02, &[0]), tlv(0x31, &infos), eci, tlv(0x04, &tag)])),
        None => (oid::ENVELOPED_DATA, seq(&[tlv(0x02, &[if any_v2 { 2 } else { 0 }]), tlv(0x31, &infos), eci])),
    };
    seq(&[oid_der(ct_oid), tlv(0xA0, &body)])
}

/// Письмо `application/pkcs7-mime; smime-type=enveloped-data` (или `authEnveloped-data`) с CMS `cms`.
pub fn encrypted_letter(h: &Head<'_>, cms: &[u8], authenticated: bool) -> Vec<u8> {
    let kind = if authenticated { "authEnveloped-data" } else { "enveloped-data" };
    let mut out = head(h).into_bytes();
    out.extend(
        format!(
            "Content-Type: application/pkcs7-mime; smime-type={kind}; name=\"smime.p7m\"\r\nContent-Transfer-Encoding: base64\r\nContent-Disposition: attachment; filename=\"smime.p7m\"\r\n\r\n{}\r\n",
            base64(cms)
        )
        .as_bytes(),
    );
    out
}

/// Внутреннее письмо с HTML и вложением (для шифрования): `multipart/mixed`.
pub fn mixed_part(html: &str, attachment: (&str, &str, &[u8])) -> Vec<u8> {
    let (name, mime, data) = attachment;
    let b = "----=_Mixed_Postman_Test";
    let mut out = format!("Content-Type: multipart/mixed; boundary=\"{b}\"\r\n\r\n--{b}\r\n").into_bytes();
    out.extend(html_part(html));
    out.extend(
        format!(
            "\r\n--{b}\r\nContent-Type: {mime}; name=\"{name}\"\r\nContent-Transfer-Encoding: base64\r\nContent-Disposition: attachment; filename=\"{name}\"\r\n\r\n{}\r\n--{b}--\r\n",
            base64(data)
        )
        .as_bytes(),
    );
    out
}

/// Внутреннее письмо, подписанное `signer` (`multipart/signed` без заголовков письма) — подписать, затем
/// зашифровать (RFC 8551 §3.7).
pub fn signed_part(inner: &[u8], signer: &Issued, key: &Key, chain: &[&Issued], opts: &Sign) -> Vec<u8> {
    let scheme = opts.scheme.unwrap_or_else(|| key.default_scheme());
    let p7s = signed_data(inner, signer, key, chain, opts);
    let full =
        multipart_signed(&Head { from: "x@x", to: "x@x", subject: "x", date: "Wed, 21 Oct 2026 10:00:00 +0300" }, inner, &p7s, micalg(scheme.hash()));
    // Без заголовков письма: от `Content-Type` до конца.
    let at = full.windows(13).position(|w| w == b"Content-Type:").unwrap();
    full[at..].to_vec()
}

/// Случайные числа и ECDH для тестового шифрования (Ф4): детерминированно, ECDH — `p256`/`p384`.
pub struct TestOps(pub std::cell::Cell<u64>);

impl Default for TestOps {
    fn default() -> Self {
        TestOps(std::cell::Cell::new(7))
    }
}

impl crate::seal::Ops for TestOps {
    fn random(&self, out: &mut [u8]) {
        let mut rng = TestRng::new(self.0.get());
        rand_core::TryRng::try_fill_bytes(&mut rng, out).unwrap();
        self.0.set(self.0.get().wrapping_add(1));
    }

    fn ecdh(&self, bits: u16, peer: &[u8]) -> Result<(Vec<u8>, zeroize::Zeroizing<Vec<u8>>), crate::seal::SealError> {
        let label = format!("seal-eph-{}", self.0.get());
        self.0.set(self.0.get().wrapping_add(1));
        let fail = |e: String| crate::seal::SealError::Failed(e);
        if bits == 384 {
            let eph = p384::SecretKey::from_slice(&seed(&label, 48)).map_err(|e| fail(e.to_string()))?;
            let pk = p384::PublicKey::from_sec1_bytes(peer).map_err(|e| fail(e.to_string()))?;
            let z = p384::ecdh::diffie_hellman(eph.to_nonzero_scalar(), pk.as_affine());
            Ok((eph.public_key().to_sec1_point(false).as_bytes().to_vec(), zeroize::Zeroizing::new(z.raw_secret_bytes().to_vec())))
        } else {
            let eph = p256::SecretKey::from_slice(&seed(&label, 32)).map_err(|e| fail(e.to_string()))?;
            let pk = p256::PublicKey::from_sec1_bytes(peer).map_err(|e| fail(e.to_string()))?;
            let z = p256::ecdh::diffie_hellman(eph.to_nonzero_scalar(), pk.as_affine());
            Ok((eph.public_key().to_sec1_point(false).as_bytes().to_vec(), zeroize::Zeroizing::new(z.raw_secret_bytes().to_vec())))
        }
    }
}

/// Общий секрет ECDH своим ключом `key` с точкой `peer` (тесты: получатель снимает обёртку).
pub fn ecdh_with(key: &Key, peer: &[u8]) -> Option<Vec<u8>> {
    match key {
        Key::P256(sk) => Some(
            p256::ecdh::diffie_hellman(*sk.as_nonzero_scalar(), p256::PublicKey::from_sec1_bytes(peer).ok()?.as_affine()).raw_secret_bytes().to_vec(),
        ),
        Key::P384(sk) => Some(
            p384::ecdh::diffie_hellman(*sk.as_nonzero_scalar(), p384::PublicKey::from_sec1_bytes(peer).ok()?.as_affine()).raw_secret_bytes().to_vec(),
        ),
        _ => None,
    }
}
