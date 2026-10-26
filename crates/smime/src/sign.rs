//! Подпись исходящих (S5-12 Ф3, `docs/SMIME.md` §6.4, 12.3а): SignedData без содержимого и `multipart/signed`.
//!
//! Вход — своё письмо (RFC 5322, собрал `postman_compose::build`) и свой сертификат: чужих данных нет, поэтому сборка —
//! в ядре. Закрытый ключ сюда не попадает: подписывает [`Signer`] — CNG (RSA, ECDSA), СКЗИ (ГОСТ, решение владельца
//! 2026-10-22: операции с ключом ГОСТ — только в сертифицированном СКЗИ) или ключ в памяти (демо, тесты).
//!
//! Подписанная часть — заголовки `Content-*` письма и тело, только 7 бит (RFC 8551 §3.1: иначе транспорт
//! перекодирует 8bit и подпись сломается); концы строк — CRLF. Атрибуты: `contentType`, `messageDigest`,
//! `signingTime`, `SMIMECapabilities` (AES-256-GCM, AES-256-CBC, AES-128-CBC, RSA-OAEP; у ГОСТ — «Кузнечик»),
//! `SMIMEEncryptionKeyPreference` (сертификат подписанта). В `certificates` — подписант и промежуточные, без корня.

use cms::cert::{CertificateChoices, IssuerAndSerialNumber};
use cms::content_info::{CmsVersion, ContentInfo};
use cms::signed_data::{CertificateSet, EncapsulatedContentInfo, SignedData, SignerIdentifier, SignerInfo, SignerInfos};
use const_oid::ObjectIdentifier as Oid;
use der::asn1::{Any, Null, OctetString, SetOfVec};
use der::{Decode, Encode, Sequence};
use spki::AlgorithmIdentifierOwned;
use x509_cert::Certificate;
use x509_cert::attr::Attribute;
use x509_cert::serial_number::SerialNumber;

use crate::{Hash, Scheme, oid};

/// Почему письмо не подписано.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SignError {
    #[error("smart card PIN required")]
    NeedPin,
    #[error("smart card is not inserted")]
    NoCard,
    #[error("PIN entry cancelled")]
    Cancelled,
    /// Ключ ГОСТ, а сертифицированного СКЗИ нет.
    #[error("GOST needs a certified crypto provider")]
    NeedSkzi,
    /// Нет своего сертификата подписи для адреса «От».
    #[error("no signing certificate for {0}")]
    NoCertificate(String),
    /// В подписываемой части есть 8-битные байты (письмо собрано не в 7 бит).
    #[error("8-bit content cannot be signed")]
    EightBit,
    #[error("{0}")]
    Failed(String),
}

/// Подписант: свой сертификат и закрытый ключ (в CNG, СКЗИ или памяти).
pub trait Signer {
    /// Сертификат подписанта (DER).
    fn certificate(&self) -> &[u8];
    /// Промежуточные сертификаты до корня (без корня).
    fn chain(&self) -> &[Vec<u8>];
    /// Схема подписи (хэш — `scheme().hash()`).
    fn scheme(&self) -> Scheme;
    /// Подпись над хэшем `digest`: RSA — PKCS#1 v1.5, ECDSA — DER `ECDSA-Sig-Value`, ГОСТ — 2×N байтов как в CMS
    /// (RFC 4491 §2.2.2).
    fn sign(&self, digest: &[u8]) -> Result<Vec<u8>, SignError>;
}

/// `micalg` по хэшу (RFC 8551 §3.5.3.2; ГОСТ — как КриптоПро [проверить]).
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

#[derive(Sequence)]
struct Capability {
    id: Oid,
}

fn attribute<T: der::EncodeValue + der::Tagged>(oid: Oid, v: &T) -> Result<Attribute, SignError> {
    let any = Any::encode_from(v).map_err(|e| SignError::Failed(e.to_string()))?;
    Ok(Attribute { oid, values: SetOfVec::try_from(vec![any]).map_err(|e| SignError::Failed(e.to_string()))? })
}

fn failed(e: impl std::fmt::Display) -> SignError {
    SignError::Failed(e.to_string())
}

/// `signatureAlgorithm` SignerInfo: RSA — `rsaEncryption` (как Outlook), ECDSA — «с хэшем», ГОСТ — OID ключа (как
/// КриптоПро).
fn signature_algorithm(scheme: Scheme, key_oid: Oid) -> AlgorithmIdentifierOwned {
    let null = || Any::encode_from(&Null).ok();
    match scheme {
        Scheme::RsaPkcs1(_) | Scheme::RsaPss { .. } => AlgorithmIdentifierOwned { oid: oid::RSA_ENCRYPTION, parameters: null() },
        Scheme::Ecdsa(Hash::Sha384) => AlgorithmIdentifierOwned { oid: oid::ECDSA_SHA384, parameters: None },
        Scheme::Ecdsa(Hash::Sha512) => AlgorithmIdentifierOwned { oid: oid::ECDSA_SHA512, parameters: None },
        Scheme::Ecdsa(_) => AlgorithmIdentifierOwned { oid: oid::ECDSA_SHA256, parameters: None },
        Scheme::Gost256 | Scheme::Gost512 => AlgorithmIdentifierOwned { oid: key_oid, parameters: None },
    }
}

/// SignedData без содержимого (`smime.p7s`, DER) над каноническими байтами `content`.
pub fn detached(content: &[u8], signer: &dyn Signer, signing_time: i64) -> Result<Vec<u8>, SignError> {
    let scheme = signer.scheme();
    let hash = scheme.hash();
    let cert = Certificate::from_der(signer.certificate()).map_err(failed)?;
    let tbs = cert.tbs_certificate();
    let isn =
        IssuerAndSerialNumber { issuer: tbs.issuer().clone(), serial_number: SerialNumber::new(tbs.serial_number().as_bytes()).map_err(failed)? };
    let time = x509_cert::time::Time::try_from(std::time::UNIX_EPOCH + std::time::Duration::from_secs(u64::try_from(signing_time).unwrap_or(0)))
        .map_err(failed)?;
    let gost = matches!(scheme, Scheme::Gost256 | Scheme::Gost512);
    let caps: Vec<Capability> = if gost {
        vec![Capability { id: oid::KUZNYECHIK_CTR_ACPKM }]
    } else {
        [oid::AES256_GCM, oid::AES256_CBC, oid::AES128_CBC, oid::RSA_OAEP].into_iter().map(|id| Capability { id }).collect()
    };
    let mut attrs = vec![
        attribute(oid::ATTR_CONTENT_TYPE, &oid::DATA)?,
        attribute(oid::ATTR_MESSAGE_DIGEST, &OctetString::new(hash.digest(content)).map_err(failed)?)?,
        attribute(oid::ATTR_SIGNING_TIME, &time)?,
        attribute(oid::ATTR_SMIME_CAPABILITIES, &caps)?,
    ];
    // SMIMEEncryptionKeyPreference ::= CHOICE { issuerAndSerialNumber [0] IMPLICIT … } — свой сертификат.
    let isn_der = isn.to_der().map_err(failed)?;
    let (seq, _) = crate::ber::read(&isn_der).ok_or_else(|| failed("IssuerAndSerialNumber"))?;
    let mut pref = crate::ber::header(0xA0, seq.value.len());
    pref.extend_from_slice(seq.value);
    attrs.push(Attribute {
        oid: oid::ATTR_ENCRYPTION_KEY_PREFERENCE,
        values: SetOfVec::try_from(vec![Any::from_der(&pref).map_err(failed)?]).map_err(failed)?,
    });
    let set = SetOfVec::try_from(attrs).map_err(failed)?;
    let signed_attrs_der = set.to_der().map_err(failed)?;
    let signature = signer.sign(&hash.digest(&signed_attrs_der))?;
    let digest_alg = AlgorithmIdentifierOwned { oid: hash.oid(), parameters: None };
    let si = SignerInfo {
        version: CmsVersion::V1,
        sid: SignerIdentifier::IssuerAndSerialNumber(isn),
        digest_alg: digest_alg.clone(),
        signed_attrs: Some(set),
        signature_algorithm: signature_algorithm(scheme, tbs.subject_public_key_info().algorithm.oid),
        signature: OctetString::new(signature).map_err(failed)?,
        unsigned_attrs: None,
    };
    let mut certs = vec![CertificateChoices::Certificate(cert.clone())];
    for c in signer.chain() {
        certs.push(CertificateChoices::Certificate(Certificate::from_der(c).map_err(failed)?));
    }
    let sd = SignedData {
        version: CmsVersion::V1,
        digest_algorithms: SetOfVec::try_from(vec![digest_alg]).map_err(failed)?,
        encap_content_info: EncapsulatedContentInfo { econtent_type: oid::DATA, econtent: None },
        certificates: Some(CertificateSet::try_from(certs).map_err(failed)?),
        crls: None,
        signer_infos: SignerInfos::try_from(vec![si]).map_err(failed)?,
    };
    ContentInfo { content_type: oid::SIGNED_DATA, content: Any::encode_from(&sd).map_err(failed)? }.to_der().map_err(failed)
}

/// Base64 со строками по 76 знаков и CRLF.
pub(crate) fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len() * 4 / 3 + data.len() / 38 + 4);
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

/// Своё письмо → (заголовки письма без `MIME-Version`, MIME-сущность: заголовки `Content-*` и тело), концы строк —
/// CRLF. Общее для подписи и шифрования (Ф3, Ф4).
pub(crate) fn split_entity(raw: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let canonical = crate::mime::canonical(raw).into_owned();
    let split = canonical.windows(4).position(|w| w == b"\r\n\r\n")?;
    let (head, body) = (&canonical[..split + 2], &canonical[split + 4..]);
    let mut outer = Vec::new();
    let mut entity = Vec::new();
    for h in header_blocks(head) {
        let name = h.iter().position(|&b| b == b':').map(|p| &h[..p]).unwrap_or_default();
        if name.len() >= 8 && name[..8].eq_ignore_ascii_case(b"content-") {
            entity.extend_from_slice(h);
        } else if !name.eq_ignore_ascii_case(b"mime-version") {
            outer.extend_from_slice(h);
        }
    }
    if entity.is_empty() {
        entity.extend_from_slice(b"Content-Type: text/plain; charset=us-ascii\r\n");
    }
    entity.extend_from_slice(b"\r\n");
    entity.extend_from_slice(body);
    Some((outer, entity))
}

/// Заголовок письма (со строками продолжения) — `Content-*` (уходит в подписанную часть) или остальное.
fn header_blocks(head: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < head.len() {
        let end = head[i..].iter().position(|&b| b == b'\n').map_or(head.len(), |p| i + p + 1);
        // Начало нового заголовка — строка не с пробела или табуляции.
        if i > start && !matches!(head.get(i), Some(b' ' | b'\t')) {
            out.push(&head[start..i]);
            start = i;
        }
        i = end;
    }
    if start < head.len() {
        out.push(&head[start..]);
    }
    out
}

/// Подписать письмо `raw` (своё, RFC 5322): заголовки письма — снаружи, `Content-*` и тело — подписанная часть
/// `multipart/signed`. Подписанная часть — только 7 бит.
pub fn sign_message(raw: &[u8], signer: &dyn Signer, now: i64) -> Result<Vec<u8>, SignError> {
    let (outer, entity) = split_entity(raw).ok_or_else(|| failed("no body"))?;
    if entity.iter().any(|&b| b >= 0x80) {
        return Err(SignError::EightBit);
    }
    let p7s = detached(&entity, signer, now)?;
    let boundary =
        format!("----=_Postman_Signed_{:016x}", u64::from_le_bytes(signer.scheme().hash().digest(&entity)[..8].try_into().unwrap_or_default()));
    let micalg = micalg(signer.scheme().hash());
    let mut out = outer;
    out.extend_from_slice(b"MIME-Version: 1.0\r\n");
    out.extend(
        format!("Content-Type: multipart/signed; protocol=\"application/pkcs7-signature\"; micalg={micalg};\r\n boundary=\"{boundary}\"\r\n\r\nThis is a cryptographically signed message in MIME format.\r\n\r\n--{boundary}\r\n")
            .into_bytes(),
    );
    out.extend_from_slice(&entity);
    out.extend(
        format!(
            "\r\n--{boundary}\r\nContent-Type: application/pkcs7-signature; name=\"smime.p7s\"\r\nContent-Transfer-Encoding: base64\r\nContent-Disposition: attachment; filename=\"smime.p7s\"\r\nContent-Description: S/MIME Cryptographic Signature\r\n\r\n{}\r\n\r\n--{boundary}--\r\n",
            base64(&p7s)
        )
        .into_bytes(),
    );
    Ok(out)
}

/// DER `ECDSA-Sig-Value` из `r‖s` (так подписывает CNG).
pub fn ecdsa_der(raw: &[u8]) -> Option<Vec<u8>> {
    if raw.is_empty() || !raw.len().is_multiple_of(2) {
        return None;
    }
    let (r, s) = raw.split_at(raw.len() / 2);
    let int = |v: &[u8]| {
        let v: Vec<u8> = v.iter().copied().skip_while(|&b| b == 0).collect();
        let mut body = if v.first().is_some_and(|&b| b >= 0x80) || v.is_empty() { vec![0] } else { Vec::new() };
        body.extend(v);
        let mut out = crate::ber::header(0x02, body.len());
        out.extend(body);
        out
    };
    let seq = [int(r), int(s)].concat();
    let mut out = crate::ber::header(0x30, seq.len());
    out.extend(seq);
    Some(out)
}

/// Свой сертификат (из хранилища пользователя, не из письма): сводка, годится ли для подписи (KeyUsage, EKU «Защита
/// почты» или без EKU) и схема подписи по ключу.
pub fn own_certificate(der: &[u8]) -> Option<(crate::CertInfo, bool, Option<Scheme>)> {
    let info = crate::chain::info(der)?;
    let can = crate::chain::can_sign_mail(der);
    let scheme = match &info.key {
        crate::KeyKind::Rsa { .. } => Some(Scheme::RsaPkcs1(Hash::Sha256)),
        crate::KeyKind::Ec { curve } if curve.contains("384") => Some(Scheme::Ecdsa(Hash::Sha384)),
        crate::KeyKind::Ec { curve } if curve.contains("256") => Some(Scheme::Ecdsa(Hash::Sha256)),
        crate::KeyKind::Gost { bits: 512, .. } => Some(Scheme::Gost512),
        crate::KeyKind::Gost { .. } => Some(Scheme::Gost256),
        _ => None,
    };
    Some((info, can, scheme))
}
