//! Шифрованное письмо S/MIME (S5-12 Ф2, `docs/SMIME.md` §6.2): EnvelopedData (RFC 5652 §6) и AuthEnvelopedData
//! (RFC 5083) — разбор получателей, расшифровка содержимого по готовому ключу (CEK). Внутреннее письмо отдаётся
//! байтами: его MIME, как и MIME внешнего, разбирает воркер (`postman_worker::smime`, S8-36).
//!
//! Вариант B (решение владельца №1): **CMS разбирает воркер**. Ядру уходят только типизированные поля получателей
//! ([`Envelope`]: издатель и серийный номер или SKI — байтами как есть, алгоритм, зашифрованный ключ); ядро
//! снимает ключ средствами ОС (CNG) и отдаёт CEK обратно воркеру. Чистые функции без разбора чужого DER —
//! [`ecdh_kek`], [`unwrap_key`], [`serial_matches`] — общие для ядра и тестов.
//!
//! Что читается: AES-128/192/256-CBC, AES-128/192/256-GCM (тег проверяется до разбора MIME), 3DES (старые
//! письма, решение №10); ключ — RSA PKCS#1 v1.5, RSA-OAEP (SHA-1/256/384/512, MGF1 тем же хэшем, без метки),
//! ECDH P-256/P-384/P-521 (`dhSinglePass-stdDH-*kdf-scheme` и cofactor, X9.63 KDF, AES Key Wrap). RC2, DES — нет;
//! ГОСТ (VKO + «Кузнечик»/«Магма») — разбирается и называется, но не расшифровывается (`Unsupported`).
//!
//! Одна ошибка на всё, что может быть оракулом (RFC 3218): неверное заполнение CBC, тег GCM, длина ключа, сбой
//! снятия PKCS#1 в ядре (там — случайный ключ той же длины) — [`OpenError::Damaged`].

use aes::cipher::{BlockDecrypt, BlockDecryptMut, KeyInit, KeyIvInit};
use const_oid::ObjectIdentifier as Oid;
use der::asn1::OctetString;
use der::{Decode, Encode, Sequence};
use serde::{Deserialize, Serialize};
use spki::AlgorithmIdentifierOwned;
use zeroize::Zeroizing;

use crate::ber::{self, Tlv};
use crate::{Hash, Letter, Report, mime, oid};

/// Ключ содержимого: только в памяти, обнуляется при освобождении.
pub type Cek = Zeroizing<Vec<u8>>;

/// Глубже вложенности (шифрование → подпись → …) не разбираем: §6.2, рекурсия не глубже 3 слоёв.
pub const MAX_LAYERS: usize = 3;

/// Кому зашифровано: издатель (DER `Name` как пришёл) и серийный номер (содержимое `INTEGER`, big-endian, как
/// пришло) или идентификатор ключа (SKI).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rid {
    IssuerSerial { issuer: Vec<u8>, serial: Vec<u8> },
    Ski(Vec<u8>),
}

/// Как зашифрован ключ содержимого для получателя.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Wrap {
    /// RSA PKCS#1 v1.5 (`rsaEncryption`).
    RsaPkcs1,
    /// RSA-OAEP: хэш и MGF1 одним алгоритмом, без метки.
    RsaOaep(Hash),
    /// ECDH (RFC 5753): эфемерный ключ отправителя (точка без сжатия, `04‖X‖Y`), KDF X9.63 этим хэшем, AES Key
    /// Wrap ключом `wrap_bits`, `ukm` — если был.
    Ecdh { kdf: Hash, wrap_bits: u16, ephemeral: Vec<u8>, ukm: Option<Vec<u8>> },
    /// ГОСТ Р 34.10 (RFC 4490 §5.2, `GostR3410-KeyTransport`): ключ снимает только СКЗИ (решение владельца
    /// 2026-10-22) — ядро передаёт эти поля провайдеру ГОСТ, сам ключ ГОСТ Postman не держит.
    Gost(GostTransport),
    /// Не поддерживается (имя или OID): kekri, pwri…
    Unsupported(String),
}

/// Ключ ГОСТ получателя.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GostKey {
    G2001,
    G2012_256,
    G2012_512,
}

/// Поля `GostR3410-KeyTransport` (RFC 4490 §5.2, ГОСТ 28147-89 с экспортом ключа КриптоПро): всё разобрано
/// воркером, ядру — значения.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GostTransport {
    pub key: GostKey,
    /// Набор параметров шифрования ключа (`encryptionParamSet`), OID строкой.
    pub param_set: String,
    /// Эфемерный ключ отправителя: точка (X‖Y, little-endian, как в `SubjectPublicKeyInfo` ГОСТ), набор
    /// параметров ключа и хэша (OID строкой).
    pub ephemeral: Vec<u8>,
    pub public_param_set: String,
    pub digest_param_set: Option<String>,
    /// Синхропосылка (8 байтов), зашифрованный ключ (32) и имитовставка (4).
    pub ukm: Vec<u8>,
    pub encrypted_key: Vec<u8>,
    pub mac: Vec<u8>,
}

/// Получатель письма.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recipient {
    pub rid: Rid,
    pub wrap: Wrap,
    pub encrypted_key: Vec<u8>,
    /// Для строки «зашифровано не для вас»: имя издателя (CN) и серийный номер (hex) или SKI — разобраны в
    /// воркере, ядро DER не читает.
    pub label: String,
}

/// Подпись получателя для интерфейса.
fn label(rid: &Rid) -> String {
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02X}")).collect::<String>();
    match rid {
        Rid::IssuerSerial { issuer, serial } => {
            let name = x509_cert::name::Name::from_der(issuer).map(|n| crate::chain::name_text(&n)).unwrap_or_default();
            let serial: Vec<u8> = serial.iter().copied().skip_while(|&b| b == 0).collect();
            format!("{name}, № {}", hex(&serial))
        }
        Rid::Ski(s) => format!("SKI {}", hex(s)),
    }
}

/// Шифр содержимого.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Cipher {
    AesCbc(u16),
    AesGcm(u16),
    Des3,
    /// ГОСТ 28147-89 (CFB, RFC 4490): содержимое расшифровывает СКЗИ в ядре — ключ из СКЗИ не выходит.
    Gost28147,
}

impl Cipher {
    /// Длина ключа содержимого, байты.
    pub fn key_len(self) -> usize {
        match self {
            Cipher::AesCbc(bits) | Cipher::AesGcm(bits) => usize::from(bits / 8),
            Cipher::Des3 => 24,
            Cipher::Gost28147 => 32,
        }
    }

    /// «AES-256-GCM», «3DES» — для карточки.
    pub fn name(self) -> String {
        match self {
            Cipher::AesCbc(b) => format!("AES-{b}-CBC"),
            Cipher::AesGcm(b) => format!("AES-{b}-GCM"),
            Cipher::Des3 => "3DES".into(),
            Cipher::Gost28147 => "ГОСТ 28147-89".into(),
        }
    }
}

/// Что видит ядро: получатели и шифр содержимого (без самого содержимого — кроме ГОСТ: его расшифровывает СКЗИ).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub recipients: Vec<Recipient>,
    /// `Err` — шифр не поддерживается (имя): ключ снимать незачем.
    pub cipher: Result<Cipher, String>,
    /// AuthEnvelopedData (RFC 5083).
    pub authenticated: bool,
    /// ГОСТ 28147-89: IV, набор параметров и шифрованное содержимое — байтами, для СКЗИ в ядре.
    #[serde(default)]
    pub gost: Option<GostContent>,
}

/// Шифрованное содержимое ГОСТ 28147-89 (CFB) для СКЗИ.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GostContent {
    pub iv: Vec<u8>,
    pub param_set: String,
    pub data: Vec<u8>,
}

/// Почему письмо не открылось.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum OpenError {
    #[error("not encrypted")]
    NotEncrypted,
    /// Ключ не подошёл, заполнение, тег GCM — одна ошибка (RFC 3218).
    #[error("integrity check failed")]
    Damaged,
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("malformed: {0}")]
    Malformed(String),
    /// Шифрование внутри шифрования глубже [`MAX_LAYERS`].
    #[error("too many layers")]
    Nested,
    #[error("message too large")]
    TooLarge,
}

fn malformed(what: &str) -> OpenError {
    OpenError::Malformed(what.into())
}

/// Содержимое для расшифровки (только в воркере).
struct Content {
    cipher: Result<Cipher, String>,
    /// IV (CBC) или nonce (GCM).
    iv: Vec<u8>,
    data: Vec<u8>,
    /// GCM: тег и длина тега, AAD (`authAttrs` как SET).
    tag: Vec<u8>,
    aad: Vec<u8>,
}

struct Parsed {
    recipients: Vec<Recipient>,
    content: Content,
    authenticated: bool,
}

fn oid_of(t: &Tlv<'_>) -> Option<Oid> {
    Oid::from_der(t.raw).ok()
}

fn alg_of(t: &Tlv<'_>) -> Option<AlgorithmIdentifierOwned> {
    AlgorithmIdentifierOwned::from_der(t.raw).or_else(|_| AlgorithmIdentifierOwned::from_ber(t.raw)).ok()
}

fn hash_by_oid(o: &Oid) -> Option<Hash> {
    Some(match *o {
        oid::SHA1 => Hash::Sha1,
        oid::SHA256 => Hash::Sha256,
        oid::SHA384 => Hash::Sha384,
        oid::SHA512 => Hash::Sha512,
        _ => return None,
    })
}

/// Имя неподдерживаемого алгоритма для строки ошибки.
fn alg_name(o: &Oid) -> String {
    match *o {
        oid::RC2_CBC => "RC2".into(),
        oid::DES_CBC => "DES".into(),
        oid::GOST28147 => "ГОСТ 28147-89".into(),
        oid::KUZNYECHIK_CTR_ACPKM | oid::KUZNYECHIK_CTR_ACPKM_OMAC => "ГОСТ Р 34.12-2015 «Кузнечик»".into(),
        oid::MAGMA_CTR_ACPKM | oid::MAGMA_CTR_ACPKM_OMAC => "ГОСТ Р 34.12-2015 «Магма»".into(),
        oid::GOST_KEY_256 | oid::GOST_KEY_512 | oid::GOST2001_KEY => "ГОСТ Р 34.10-2012 (VKO)".into(),
        other => other.to_string(),
    }
}

/// `RSAES-OAEP-params`: хэш и MGF1 (по умолчанию SHA-1), метка — только пустая.
fn oaep(params: Option<&der::Any>) -> Wrap {
    let Some(p) = params else { return Wrap::RsaOaep(Hash::Sha1) };
    let bytes = p.to_der().unwrap_or_default();
    let Some((seq, _)) = ber::read(&bytes) else { return Wrap::Unsupported("RSA-OAEP".into()) };
    let mut hash = Hash::Sha1;
    let mut mgf = Hash::Sha1;
    for f in seq.children().unwrap_or_default() {
        let inner = f.children().and_then(|k| k.into_iter().next());
        match (f.tag, inner) {
            (0xA0, Some(a)) => match alg_of(&a).and_then(|a| hash_by_oid(&a.oid)) {
                Some(h) => hash = h,
                None => return Wrap::Unsupported("RSA-OAEP hash".into()),
            },
            (0xA1, Some(a)) => {
                let Some(alg) = alg_of(&a) else { return Wrap::Unsupported("RSA-OAEP MGF".into()) };
                let h = alg
                    .parameters
                    .as_ref()
                    .and_then(|p| AlgorithmIdentifierOwned::from_der(&p.to_der().ok()?).ok())
                    .and_then(|a| hash_by_oid(&a.oid));
                match (alg.oid == oid::MGF1, h) {
                    (true, Some(h)) => mgf = h,
                    _ => return Wrap::Unsupported("RSA-OAEP MGF".into()),
                }
            }
            // Метка (pSourceFunc) — только пустая: иначе CNG без метки не снимет.
            (0xA2, _) => return Wrap::Unsupported("RSA-OAEP label".into()),
            _ => return Wrap::Unsupported("RSA-OAEP".into()),
        }
    }
    if hash != mgf {
        return Wrap::Unsupported("RSA-OAEP MGF".into());
    }
    Wrap::RsaOaep(hash)
}

/// Идентификатор: `IssuerAndSerialNumber` (SEQUENCE) или SKI (`[0]` IMPLICIT OCTET STRING или
/// `RecipientKeyIdentifier`).
fn rid(t: &Tlv<'_>) -> Option<Rid> {
    match t.tag {
        0x30 => {
            let kids = t.children()?;
            let (issuer, serial) = (kids.first()?, kids.get(1)?);
            (issuer.tag == 0x30 && serial.tag == 0x02).then(|| Rid::IssuerSerial { issuer: issuer.raw.to_vec(), serial: serial.value.to_vec() })
        }
        // ktri: `[0] IMPLICIT SubjectKeyIdentifier`.
        0x80 => Some(Rid::Ski(t.value.to_vec())),
        // kari: `[0] IMPLICIT RecipientKeyIdentifier { subjectKeyIdentifier, date?, other? }`.
        0xA0 => {
            let first = t.children()?.into_iter().next()?;
            (first.tag == 0x04).then(|| Rid::Ski(first.value.to_vec()))
        }
        _ => None,
    }
}

fn ktri(t: &Tlv<'_>) -> Option<Recipient> {
    let kids = t.children()?;
    let rid = rid(kids.get(1)?)?;
    let alg = alg_of(kids.get(2)?)?;
    let encrypted_key = kids.get(3).filter(|k| k.tag & 0x1f == 0x04)?.octets()?;
    let wrap = match alg.oid {
        oid::RSA_ENCRYPTION => Wrap::RsaPkcs1,
        oid::RSA_OAEP => oaep(alg.parameters.as_ref()),
        oid::GOST2001_KEY | oid::GOST_KEY_256 | oid::GOST_KEY_512 => gost_transport(&alg.oid, &encrypted_key),
        other => Wrap::Unsupported(alg_name(&other)),
    };
    let label = label(&rid);
    Some(Recipient { rid, wrap, encrypted_key, label })
}

fn ecdh_kdf(o: &Oid) -> Option<Hash> {
    Some(match *o {
        oid::ECDH_STD_SHA1 | oid::ECDH_COF_SHA1 => Hash::Sha1,
        oid::ECDH_STD_SHA256 | oid::ECDH_COF_SHA256 => Hash::Sha256,
        oid::ECDH_STD_SHA384 | oid::ECDH_COF_SHA384 => Hash::Sha384,
        oid::ECDH_STD_SHA512 | oid::ECDH_COF_SHA512 => Hash::Sha512,
        _ => return None,
    })
}

fn wrap_bits(o: &Oid) -> Option<u16> {
    Some(match *o {
        oid::AES128_WRAP => 128,
        oid::AES192_WRAP => 192,
        oid::AES256_WRAP => 256,
        _ => return None,
    })
}

/// kari (`[1] IMPLICIT KeyAgreeRecipientInfo`): один эфемерный ключ — несколько получателей.
fn kari(t: &Tlv<'_>) -> Option<Vec<Recipient>> {
    let kids = t.children()?;
    let mut it = kids.into_iter().skip(1);
    // originator [0] EXPLICIT OriginatorIdentifierOrKey → [1] IMPLICIT OriginatorPublicKey { alg, BIT STRING }.
    let originator = it.next().filter(|o| o.tag == 0xA0)?.children()?.into_iter().next()?;
    let ephemeral = if originator.tag == 0xA1 {
        let pk = originator.children()?;
        let bits = pk.get(1).filter(|b| b.tag == 0x03)?.value;
        // BIT STRING: первый байт — неиспользуемые биты (0).
        let (&unused, point) = bits.split_first()?;
        (unused == 0).then(|| point.to_vec())
    } else {
        // Статический ключ отправителя (сертификат) — не поддерживаем.
        None
    };
    let mut next = it.next()?;
    let ukm = if next.tag == 0xA1 {
        let u = next.children()?.into_iter().next().filter(|u| u.tag & 0x1f == 0x04)?.octets()?;
        next = it.next()?;
        Some(u)
    } else {
        None
    };
    let alg = alg_of(&next)?;
    let keys = it.next().filter(|k| k.tag == 0x30)?.children()?;
    let wrap_alg = alg.parameters.as_ref().and_then(|p| AlgorithmIdentifierOwned::from_der(&p.to_der().ok()?).ok());
    let wrap = match (ecdh_kdf(&alg.oid), wrap_alg.as_ref().and_then(|w| wrap_bits(&w.oid)), ephemeral) {
        (Some(kdf), Some(bits), Some(ephemeral)) => Wrap::Ecdh { kdf, wrap_bits: bits, ephemeral, ukm },
        (None, ..) => Wrap::Unsupported(alg_name(&alg.oid)),
        (_, None, _) => Wrap::Unsupported(wrap_alg.map_or_else(|| "key wrap".into(), |w| alg_name(&w.oid))),
        (_, _, None) => Wrap::Unsupported("ECDH: static originator key".into()),
    };
    keys.iter()
        .map(|k| {
            let kk = k.children()?;
            let rid = rid(kk.first()?)?;
            let encrypted_key = kk.get(1).filter(|e| e.tag & 0x1f == 0x04)?.octets()?;
            let label = label(&rid);
            Some(Recipient { rid, wrap: wrap.clone(), encrypted_key, label })
        })
        .collect()
}

/// Шифр содержимого и его параметры (IV, nonce, длина тега).
fn content_alg(alg: &AlgorithmIdentifierOwned) -> (Result<Cipher, String>, Vec<u8>, usize) {
    let params = alg.parameters.as_ref().and_then(|p| p.to_der().ok()).unwrap_or_default();
    let octets = || ber::read(&params).and_then(|(t, _)| (t.tag & 0x1f == 0x04).then(|| t.octets()).flatten());
    let cbc = |bits: u16, iv_len: usize, c: Cipher| match octets() {
        Some(iv) if iv.len() == iv_len => (Ok(c), iv, 0),
        _ => (Err(format!("{} IV", Cipher::AesCbc(bits).name())), Vec::new(), 0),
    };
    match alg.oid {
        oid::AES128_CBC => cbc(128, 16, Cipher::AesCbc(128)),
        oid::AES192_CBC => cbc(192, 16, Cipher::AesCbc(192)),
        oid::AES256_CBC => cbc(256, 16, Cipher::AesCbc(256)),
        oid::DES_EDE3_CBC => cbc(0, 8, Cipher::Des3),
        oid::AES128_GCM | oid::AES192_GCM | oid::AES256_GCM => {
            let bits = match alg.oid {
                oid::AES128_GCM => 128,
                oid::AES192_GCM => 192,
                _ => 256,
            };
            // GCMParameters { aes-nonce OCTET STRING, aes-ICVlen INTEGER DEFAULT 12 }.
            let fields = ber::read(&params).and_then(|(t, _)| t.children()).unwrap_or_default();
            let nonce = fields.first().filter(|n| n.tag == 0x04).map(|n| n.value.to_vec());
            let icv = match fields.get(1) {
                Some(i) if i.tag == 0x02 && i.value.len() == 1 => usize::from(i.value[0]),
                Some(_) => 0,
                None => 12,
            };
            match nonce {
                Some(n) if n.len() == 12 && (icv == 12 || icv == 16) => (Ok(Cipher::AesGcm(bits)), n, icv),
                _ => (Err(format!("AES-{bits}-GCM parameters")), Vec::new(), 0),
            }
        }
        // `Gost28147-89-Parameters { iv OCTET STRING (8), encryptionParamSet OID }` — IV и набор отдаются ядру
        // (`Envelope::gost`), расшифровка — в СКЗИ.
        oid::GOST28147 => (Ok(Cipher::Gost28147), Vec::new(), 0),
        other => (Err(alg_name(&other)), Vec::new(), 0),
    }
}

/// `GostR3410-KeyTransport` → поля для СКЗИ. Форма с «Кузнечиком»/«Магмой» (KExp15) — не поддерживается [проверить
/// с КриптоПро CSP 5].
fn gost_transport(key_oid: &Oid, der: &[u8]) -> Wrap {
    let key = match *key_oid {
        oid::GOST2001_KEY => GostKey::G2001,
        oid::GOST_KEY_512 => GostKey::G2012_512,
        _ => GostKey::G2012_256,
    };
    let parse = || -> Option<GostTransport> {
        let (t, _) = ber::read(der)?;
        let kids = t.children()?;
        let sek = kids.first().filter(|s| s.tag == 0x30)?.children()?;
        let encrypted_key = sek.first().filter(|k| k.tag == 0x04)?.value.to_vec();
        let mac = sek.last().filter(|m| m.tag == 0x04)?.value.to_vec();
        let tp = kids.get(1).filter(|p| p.tag == 0xA0)?.children()?;
        let param_set = oid_of(tp.first()?)?.to_string();
        let spki = tp.get(1).filter(|k| k.tag == 0xA0)?.children()?;
        let alg = spki.first()?.children()?;
        let params = alg.get(1)?.children()?;
        let public_param_set = oid_of(params.first()?)?.to_string();
        let digest_param_set = params.get(1).and_then(oid_of).map(|o| o.to_string());
        let bits = spki.get(1).filter(|b| b.tag == 0x03)?.value;
        // BIT STRING (0 неиспользуемых) с OCTET STRING точки внутри.
        let (inner, _) = ber::read(bits.get(1..)?)?;
        let ephemeral = (inner.tag == 0x04).then(|| inner.value.to_vec())?;
        let ukm = tp.last().filter(|u| u.tag == 0x04 && u.value.len() == 8)?.value.to_vec();
        (encrypted_key.len() == 32 && mac.len() == 4).then_some(GostTransport {
            key,
            param_set,
            ephemeral,
            public_param_set,
            digest_param_set,
            ukm,
            encrypted_key,
            mac,
        })
    };
    parse().map_or_else(|| Wrap::Unsupported("ГОСТ: KExp15 / GostR3410-KeyTransport".into()), Wrap::Gost)
}

/// `encryptedContent [0] IMPLICIT OCTET STRING` — примитивный (DER) или составной (BER потоком у Outlook).
fn encrypted_content(t: &Tlv<'_>) -> Option<Vec<u8>> {
    match t.tag {
        0x80 => Some(t.value.to_vec()),
        0xA0 => {
            let mut out = Vec::new();
            for c in t.children()? {
                out.extend(c.octets()?);
            }
            Some(out)
        }
        _ => None,
    }
}

/// Разобрать `ContentInfo` с EnvelopedData или AuthEnvelopedData.
fn parse(cms: &[u8]) -> Result<Parsed, OpenError> {
    let (ci, _) = ber::read(cms).ok_or_else(|| malformed("ContentInfo"))?;
    let ci = ci.children().ok_or_else(|| malformed("ContentInfo"))?;
    let ct = ci.first().and_then(oid_of).ok_or_else(|| malformed("content type"))?;
    let authenticated = match ct {
        oid::ENVELOPED_DATA => false,
        oid::AUTH_ENVELOPED_DATA => true,
        _ => return Err(OpenError::NotEncrypted),
    };
    let ed = ci
        .get(1)
        .filter(|t| t.tag == 0xA0)
        .and_then(|t| t.children())
        .and_then(|k| k.into_iter().next())
        .ok_or_else(|| malformed("EnvelopedData"))?;
    let parts = ed.children().ok_or_else(|| malformed("EnvelopedData"))?;
    // version, originatorInfo [0]?, recipientInfos SET, (auth)encryptedContentInfo, …
    let mut i = 1;
    if parts.get(i).is_some_and(|t| t.tag == 0xA0) {
        i += 1;
    }
    let infos = parts.get(i).filter(|t| t.tag == 0x31).and_then(|t| t.children()).ok_or_else(|| malformed("recipientInfos"))?;
    let mut recipients = Vec::new();
    for ri in &infos {
        match ri.tag {
            0x30 => recipients.push(ktri(ri).ok_or_else(|| malformed("KeyTransRecipientInfo"))?),
            0xA1 => recipients.extend(kari(ri).ok_or_else(|| malformed("KeyAgreeRecipientInfo"))?),
            0xA2 => recipients.push(Recipient {
                rid: Rid::Ski(Vec::new()),
                wrap: Wrap::Unsupported("KEKRecipientInfo".into()),
                encrypted_key: Vec::new(),
                label: String::new(),
            }),
            0xA3 => recipients.push(Recipient {
                rid: Rid::Ski(Vec::new()),
                wrap: Wrap::Unsupported("PasswordRecipientInfo".into()),
                encrypted_key: Vec::new(),
                label: String::new(),
            }),
            _ => {}
        }
    }
    let eci = parts.get(i + 1).and_then(|t| t.children()).ok_or_else(|| malformed("encryptedContentInfo"))?;
    let alg = eci.get(1).and_then(alg_of).ok_or_else(|| malformed("contentEncryptionAlgorithm"))?;
    let data = eci.get(2).and_then(encrypted_content).ok_or_else(|| malformed("encryptedContent"))?;
    let (cipher, iv, icv) = content_alg(&alg);
    let mut content = Content { cipher, iv, data, tag: Vec::new(), aad: Vec::new() };
    if authenticated {
        // authAttrs [1]?, mac OCTET STRING, unauthAttrs [2]?
        let mut j = i + 2;
        if let Some(a) = parts.get(j).filter(|t| t.tag == 0xA1) {
            // AAD — DER `authAttrs` с тегом SET (RFC 5083 §2.2).
            let mut aad = ber::header(0x31, a.value.len());
            aad.extend_from_slice(a.value);
            content.aad = aad;
            j += 1;
        }
        let mac = parts.get(j).filter(|t| t.tag & 0x1f == 0x04).and_then(Tlv::octets).ok_or_else(|| malformed("mac"))?;
        if content.cipher.is_ok() && mac.len() != icv {
            return Err(OpenError::Damaged);
        }
        content.tag = mac;
        if matches!(content.cipher, Ok(Cipher::AesCbc(_) | Cipher::Des3)) {
            return Err(OpenError::Unsupported("AuthEnvelopedData + CBC".into()));
        }
    } else if matches!(content.cipher, Ok(Cipher::AesGcm(_))) {
        return Err(OpenError::Unsupported("EnvelopedData + GCM".into()));
    }
    Ok(Parsed { recipients, content, authenticated })
}

/// Параметры ГОСТ 28147-89 из `contentEncryptionAlgorithm` (IV и набор) и шифрованное содержимое.
fn gost_content(cms: &[u8], data: Vec<u8>) -> Option<GostContent> {
    let (ci, _) = ber::read(cms)?;
    let ed = ci.children()?.get(1)?.children()?.into_iter().next()?;
    let parts = ed.children()?;
    let eci = parts.iter().find(|p| p.tag == 0x30 && p.children().is_some_and(|k| k.first().and_then(oid_of) == Some(oid::DATA)))?;
    let alg = eci.children()?.get(1)?.children()?;
    let params = alg.get(1)?.children()?;
    let iv = params.first().filter(|i| i.tag == 0x04 && i.value.len() == 8)?.value.to_vec();
    let param_set = oid_of(params.get(1)?)?.to_string();
    Some(GostContent { iv, param_set, data })
}

/// DER CMS шифрованного письма (корень — `application/pkcs7-mime`, части — от воркера).
fn cms_of(letter: &Letter) -> Result<Vec<u8>, OpenError> {
    match letter.parts.as_ref().map_err(String::clone).and_then(mime::find) {
        Ok(mime::Found::Encrypted { cms: Some(cms) }) => Ok(cms),
        Ok(mime::Found::Encrypted { cms: None }) => Err(malformed("pkcs7-mime body")),
        Ok(_) => Err(OpenError::NotEncrypted),
        Err(e) => Err(OpenError::Malformed(e)),
    }
}

fn guarded<T>(f: impl FnOnce() -> Result<T, OpenError> + std::panic::UnwindSafe) -> Result<T, OpenError> {
    std::panic::catch_unwind(f).unwrap_or_else(|_| Err(malformed("parser failure")))
}

/// Получатели и шифр письма (корень разобран воркером) — для ядра. Ничего не расшифровывает.
pub fn envelope(letter: &Letter) -> Result<Envelope, OpenError> {
    guarded(|| {
        let cms = cms_of(letter)?;
        let p = parse(&cms)?;
        let gost = (p.content.cipher == Ok(Cipher::Gost28147)).then(|| gost_content(&cms, p.content.data.clone())).flatten();
        Ok(Envelope { recipients: p.recipients, cipher: p.content.cipher, authenticated: p.authenticated, gost })
    })
}

fn decrypt_content(c: &Content, cek: &[u8]) -> Result<Zeroizing<Vec<u8>>, OpenError> {
    use aes::cipher::block_padding::Pkcs7;
    let cipher = c.cipher.clone().map_err(OpenError::Unsupported)?;
    if cek.len() != cipher.key_len() {
        return Err(OpenError::Damaged);
    }
    let damaged = |_| OpenError::Damaged;
    let out = match cipher {
        Cipher::AesCbc(128) => cbc::Decryptor::<aes::Aes128>::new_from_slices(cek, &c.iv)
            .map_err(|_| OpenError::Damaged)?
            .decrypt_padded_vec_mut::<Pkcs7>(&c.data)
            .map_err(damaged)?,
        Cipher::AesCbc(192) => cbc::Decryptor::<aes::Aes192>::new_from_slices(cek, &c.iv)
            .map_err(|_| OpenError::Damaged)?
            .decrypt_padded_vec_mut::<Pkcs7>(&c.data)
            .map_err(damaged)?,
        Cipher::AesCbc(_) => cbc::Decryptor::<aes::Aes256>::new_from_slices(cek, &c.iv)
            .map_err(|_| OpenError::Damaged)?
            .decrypt_padded_vec_mut::<Pkcs7>(&c.data)
            .map_err(damaged)?,
        Cipher::Des3 => cbc::Decryptor::<des::TdesEde3>::new_from_slices(cek, &c.iv)
            .map_err(|_| OpenError::Damaged)?
            .decrypt_padded_vec_mut::<Pkcs7>(&c.data)
            .map_err(damaged)?,
        Cipher::AesGcm(bits) => gcm(bits, cek, c)?,
        // Ключ ГОСТ из СКЗИ не выходит: содержимое расшифровывает ядро (`open_inner`).
        Cipher::Gost28147 => return Err(OpenError::Unsupported("ГОСТ 28147-89: нужно СКЗИ".into())),
    };
    Ok(Zeroizing::new(out))
}

/// AES-GCM: тег проверяется до того, как кто-либо увидит открытый текст.
fn gcm(bits: u16, cek: &[u8], c: &Content) -> Result<Vec<u8>, OpenError> {
    use aes_gcm::aead::AeadInPlace;
    use aes_gcm::aead::consts::{U12, U16};
    use aes_gcm::{AesGcm, Nonce, Tag};
    fn run<C, T>(key: &[u8], c: &Content) -> Result<Vec<u8>, OpenError>
    where
        C: aes::cipher::BlockCipher + aes::cipher::BlockSizeUser<BlockSize = aes::cipher::consts::U16> + aes::cipher::BlockEncrypt + KeyInit,
        T: aes_gcm::TagSize,
    {
        let aead = AesGcm::<C, U12, T>::new_from_slice(key).map_err(|_| OpenError::Damaged)?;
        let mut buf = c.data.clone();
        let tag = Tag::<T>::from_exact_iter(c.tag.iter().copied()).ok_or(OpenError::Damaged)?;
        aead.decrypt_in_place_detached(Nonce::from_slice(&c.iv), &c.aad, &mut buf, &tag).map_err(|_| OpenError::Damaged)?;
        Ok(buf)
    }
    match (bits, c.tag.len()) {
        (128, 16) => run::<aes::Aes128, U16>(cek, c),
        (192, 16) => run::<aes::Aes192, U16>(cek, c),
        (256, 16) => run::<aes::Aes256, U16>(cek, c),
        (128, 12) => run::<aes::Aes128, U12>(cek, c),
        (192, 12) => run::<aes::Aes192, U12>(cek, c),
        (256, 12) => run::<aes::Aes256, U12>(cek, c),
        _ => Err(OpenError::Damaged),
    }
}

/// Вложение расшифрованного письма (без содержимого).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Part {
    /// Номер вложения по порядку (для `postman_worker::smime::part`).
    pub index: u32,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub cid: Option<String>,
    pub inline: bool,
}

/// Расшифрованное письмо (в воркере): шифр, подпись внутри, тело и вложения.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Opened {
    pub cipher: Cipher,
    pub authenticated: bool,
    /// Подпись внутреннего письма (вид `None` — не подписано).
    pub inner: Report,
    pub html: Option<String>,
    pub text: Option<String>,
    pub parts: Vec<Part>,
}

/// Расшифрованное содержимое: шифр и внутреннее письмо (MIME) — его разбирает воркер.
pub struct Decrypted {
    pub cipher: Cipher,
    pub authenticated: bool,
    pub inner: Zeroizing<Vec<u8>>,
}

/// Расшифровать письмо (корень разобран воркером) ключом `cek`. Внутреннее письмо — байтами: тело, вложения и подпись
/// внутри разбирает воркер (`postman_worker::smime::open`).
pub fn decrypt(letter: &Letter, cek: &[u8]) -> Result<Decrypted, OpenError> {
    guarded(std::panic::AssertUnwindSafe(|| {
        let cms = cms_of(letter)?;
        let p = parse(&cms)?;
        let inner = decrypt_content(&p.content, cek)?;
        let cipher = p.content.cipher.clone().map_err(OpenError::Unsupported)?;
        Ok(Decrypted { cipher, authenticated: p.authenticated, inner })
    }))
}

// --- общее для ядра и тестов: без разбора чужого DER ---

/// OID строкой из DER (OID или `SEQUENCE { OID … }`) — набор параметров в своём блобе СКЗИ.
pub fn oid_text_of(der: &[u8]) -> Option<String> {
    let (t, _) = ber::read(der)?;
    let t = if t.tag == 0x30 { t.children()?.into_iter().next()? } else { t };
    oid_of(&t).map(|o| o.to_string())
}

/// DER OID из строки (блобы СКЗИ собирает ядро из значений, которые разобрал воркер).
pub fn oid_der(text: &str) -> Option<Vec<u8>> {
    Oid::new(text).ok()?.to_der().ok()
}

/// Серийный номер получателя (`INTEGER` big-endian как пришёл) совпадает с серийным номером сертификата в
/// little-endian (`CERT_INFO.SerialNumber` Windows). Ведущие нули знака не важны.
pub fn serial_matches(rid_be: &[u8], cert_le: &[u8]) -> bool {
    let be: Vec<u8> = rid_be.iter().copied().skip_while(|&b| b == 0).collect();
    let le: Vec<u8> = cert_le.iter().rev().copied().skip_while(|&b| b == 0).collect();
    !be.is_empty() && be == le
}

#[derive(Sequence)]
struct SharedInfo {
    key_info: AlgorithmIdentifierOwned,
    #[asn1(context_specific = "0", tag_mode = "EXPLICIT", optional = "true")]
    entity_u_info: Option<OctetString>,
    #[asn1(context_specific = "2", tag_mode = "EXPLICIT")]
    supp_pub_info: OctetString,
}

fn wrap_oid(bits: u16) -> Oid {
    match bits {
        128 => oid::AES128_WRAP,
        192 => oid::AES192_WRAP,
        _ => oid::AES256_WRAP,
    }
}

/// Ключ обёртки ECDH (RFC 5753 §7.2, X9.63 KDF): из общего секрета `z` и `ECC-CMS-SharedInfo`.
pub fn ecdh_kek(z: &[u8], kdf: Hash, wrap_bits: u16, ukm: Option<&[u8]>) -> Option<Zeroizing<Vec<u8>>> {
    let info = SharedInfo {
        key_info: AlgorithmIdentifierOwned { oid: wrap_oid(wrap_bits), parameters: None },
        entity_u_info: ukm.map(|u| OctetString::new(u.to_vec())).transpose().ok()?,
        supp_pub_info: OctetString::new(u32::from(wrap_bits).to_be_bytes().to_vec()).ok()?,
    }
    .to_der()
    .ok()?;
    let len = usize::from(wrap_bits / 8);
    let mut out = Zeroizing::new(Vec::with_capacity(len + 64));
    let mut counter = 1u32;
    while out.len() < len {
        let mut input = Zeroizing::new(z.to_vec());
        input.extend_from_slice(&counter.to_be_bytes());
        input.extend_from_slice(&info);
        out.extend(kdf.digest(&input));
        counter += 1;
    }
    out.truncate(len);
    Some(out)
}

const KW_IV: [u8; 8] = [0xA6; 8];

/// AES Key Wrap (RFC 3394): обёртка ключа `key` ключом `kek` (Ф4, ECDH-получатели).
pub fn wrap_key(kek: &[u8], key: &[u8]) -> Option<Vec<u8>> {
    use aes::cipher::BlockEncrypt;
    fn run<C: BlockEncrypt + KeyInit + aes::cipher::BlockSizeUser<BlockSize = aes::cipher::consts::U16>>(kek: &[u8], key: &[u8]) -> Option<Vec<u8>> {
        let cipher = C::new_from_slice(kek).ok()?;
        let n = key.len() / 8;
        let mut a = KW_IV;
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
        Some([a.to_vec(), r].concat())
    }
    if key.len() < 16 || !key.len().is_multiple_of(8) {
        return None;
    }
    match kek.len() {
        16 => run::<aes::Aes128>(kek, key),
        24 => run::<aes::Aes192>(kek, key),
        32 => run::<aes::Aes256>(kek, key),
        _ => None,
    }
}

/// AES Key Unwrap (RFC 3394); неверная проверка целостности — `None`.
pub fn unwrap_key(kek: &[u8], wrapped: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    fn run<C: BlockDecrypt + KeyInit + aes::cipher::BlockSizeUser<BlockSize = aes::cipher::consts::U16>>(
        kek: &[u8],
        wrapped: &[u8],
    ) -> Option<Zeroizing<Vec<u8>>> {
        let cipher = C::new_from_slice(kek).ok()?;
        let n = wrapped.len() / 8 - 1;
        let mut a: [u8; 8] = wrapped[..8].try_into().ok()?;
        let mut r = Zeroizing::new(wrapped[8..].to_vec());
        for j in (0..6).rev() {
            for i in (1..=n).rev() {
                let t = (n * j + i) as u64;
                let mut block = aes::cipher::Block::<C>::default();
                let mut x = a;
                for (k, b) in t.to_be_bytes().iter().enumerate() {
                    x[k] ^= b;
                }
                block[..8].copy_from_slice(&x);
                block[8..].copy_from_slice(&r[(i - 1) * 8..i * 8]);
                cipher.decrypt_block(&mut block);
                a.copy_from_slice(&block[..8]);
                r[(i - 1) * 8..i * 8].copy_from_slice(&block[8..]);
            }
        }
        // Проверка целостности без ветвления по байтам.
        let diff = a.iter().zip(KW_IV).fold(0u8, |d, (x, y)| d | (x ^ y));
        (diff == 0).then_some(r)
    }
    if wrapped.len() < 24 || !wrapped.len().is_multiple_of(8) {
        return None;
    }
    match kek.len() {
        16 => run::<aes::Aes128>(kek, wrapped),
        24 => run::<aes::Aes192>(kek, wrapped),
        32 => run::<aes::Aes256>(kek, wrapped),
        _ => None,
    }
}

/// ECDH-получатель: из общего секрета `z` — CEK (KDF и снятие обёртки).
pub fn ecdh_cek(z: &[u8], wrap: &Wrap, encrypted_key: &[u8]) -> Option<Cek> {
    let Wrap::Ecdh { kdf, wrap_bits, ukm, .. } = wrap else { return None };
    let kek = ecdh_kek(z, *kdf, *wrap_bits, ukm.as_deref())?;
    unwrap_key(&kek, encrypted_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 3394 §4.1 и §4.6: обёртка 128 бит ключом 128 и 256 бит ключом 256.
    #[test]
    fn rfc3394_vectors() {
        let hex = |s: &str| (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect::<Vec<u8>>();
        let kek = hex("000102030405060708090A0B0C0D0E0F");
        let wrapped = hex("1FA68B0A8112B447AEF34BD8FB5A7B829D3E862371D2CFE5");
        assert_eq!(unwrap_key(&kek, &wrapped).unwrap().as_slice(), hex("00112233445566778899AABBCCDDEEFF"));
        let kek = hex("000102030405060708090A0B0C0D0E0F101112131415161718191A1B1C1D1E1F");
        let wrapped = hex("28C9F404C4B810F4CBCCB35CFB87F8263F5786E2D80ED326CBC7F0E71A99F43BFB988B9B7A02DD21");
        assert_eq!(unwrap_key(&kek, &wrapped).unwrap().as_slice(), hex("00112233445566778899AABBCCDDEEFF000102030405060708090A0B0C0D0E0F"));
        let mut bad = wrapped.clone();
        bad[5] ^= 1;
        assert!(unwrap_key(&kek, &bad).is_none());
        assert!(unwrap_key(&kek, &wrapped[..20]).is_none());
    }

    /// Серийный номер: DER big-endian (с нулём знака) против little-endian Windows.
    #[test]
    fn serial_endianness() {
        assert!(serial_matches(&[0x00, 0x81, 0x02], &[0x02, 0x81]));
        assert!(serial_matches(&[0x01, 0x02], &[0x02, 0x01, 0x00]));
        assert!(!serial_matches(&[0x01, 0x02], &[0x01, 0x02]));
        assert!(!serial_matches(&[], &[]));
    }

    /// Враждебный ввод: обрезанный и случайный CMS — ошибка, без паники.
    #[test]
    fn hostile_cms() {
        for n in 0..64 {
            let junk: Vec<u8> = (0..n).map(|i| (i * 71 + 5) as u8).collect();
            assert!(parse(&junk).is_err());
        }
    }
}
