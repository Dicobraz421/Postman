//! «Подготовленный получатель» (S5-12 Ф4, `docs/SMIME.md` §5, §6.4): сертификат адресата — чужие данные, поэтому
//! его разбирает и проверяет **воркер** (цепочка к якорям, EKU, KeyUsage для шифрования, срок), а ядру уходят только
//! значения: ключ (RSA `n`/`e`, точка EC и кривая, ключ ГОСТ), издатель и серийный номер, SKI — байтами как есть.
//!
//! Кандидат может быть и PKCS#7 SignedData с сертификатами — так каталоги хранят `userSMIMECertificate` (RFC 2798,
//! S5-12.5в): получатель — первый сертификат из набора с адресом, который прошёл проверку; остальные — промежуточные.

use der::{Decode, Encode};
use serde::{Deserialize, Serialize};
use x509_cert::Certificate;
use x509_cert::ext::pkix::{KeyUsage, SubjectKeyIdentifier};

use crate::envelope::GostKey;
use crate::{Trust, oid};

/// Открытый ключ получателя для шифрования.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecipientKey {
    /// RSA: модуль и экспонента (big-endian).
    Rsa { n: Vec<u8>, e: Vec<u8> },
    /// EC: кривая (256 или 384 бит) и точка без сжатия (`04‖X‖Y`).
    Ec { bits: u16, point: Vec<u8> },
    /// ГОСТ Р 34.10: ключ (X‖Y little-endian), наборы параметров — шифрует только СКЗИ.
    Gost { key: GostKey, point: Vec<u8>, public_param_set: String, digest_param_set: Option<String> },
}

impl RecipientKey {
    pub fn is_gost(&self) -> bool {
        matches!(self, RecipientKey::Gost { .. })
    }
}

/// Получатель, готовый к шифрованию.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prepared {
    /// DER `Name` издателя как в сертификате и содержимое `INTEGER` серийного номера.
    pub issuer: Vec<u8>,
    pub serial: Vec<u8>,
    pub ski: Option<Vec<u8>>,
    pub key: RecipientKey,
    /// Адреса сертификата (нижний регистр).
    pub emails: Vec<String>,
    pub not_after: i64,
    /// SHA-256 DER, hex — чтобы отличать сертификаты одного адресата.
    pub fingerprint: String,
}

/// Подготовить сертификат `der` (или PKCS#7 с сертификатами; промежуточные — `others`) к шифрованию на момент `now`.
pub fn prepare(der: &[u8], others: &[Vec<u8>], trust: &Trust, now: u64) -> Result<Prepared, String> {
    std::panic::catch_unwind(|| match bundle(der) {
        Some(certs) => prepare_bundle(&certs, others, trust, now),
        None => prepare_inner(der, others, trust, now),
    })
    .unwrap_or_else(|_| Err("parser failure".into()))
}

/// Сертификатов в одном PKCS#7 — не больше: набор из каталога — чужие данные.
const BUNDLE_MAX: usize = 16;

/// Сертификаты из PKCS#7 SignedData (`ContentInfo`, `[0] certificates`); не PKCS#7 — `None`.
fn bundle(der: &[u8]) -> Option<Vec<Vec<u8>>> {
    let (ci, _) = crate::ber::read(der)?;
    let ci = ci.children()?;
    if ci.first().and_then(|t| const_oid::ObjectIdentifier::from_der(t.raw).ok()) != Some(oid::SIGNED_DATA) {
        return None;
    }
    let sd = ci.get(1).filter(|t| t.tag == 0xA0)?.children()?.into_iter().next()?;
    let parts = sd.children()?;
    // version, digestAlgorithms, encapContentInfo, [0] certificates?, [1] crls?, signerInfos
    let certs = parts.iter().skip(3).find(|p| p.tag == 0xA0).and_then(|p| p.children()).unwrap_or_default();
    Some(certs.into_iter().filter(|c| c.tag == 0x30).take(BUNDLE_MAX).map(|c| c.raw.to_vec()).collect())
}

/// Получатель из набора: первый сертификат с адресом, прошедший проверку (CA — не получатель).
fn prepare_bundle(certs: &[Vec<u8>], others: &[Vec<u8>], trust: &Trust, now: u64) -> Result<Prepared, String> {
    let mut last = Err("PKCS#7: no certificate".to_owned());
    for (i, c) in certs.iter().enumerate() {
        let mut rest: Vec<Vec<u8>> = certs.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, d)| d.clone()).collect();
        rest.extend_from_slice(others);
        match prepare_inner(c, &rest, trust, now) {
            Ok(p) if !p.emails.is_empty() => return Ok(p),
            Ok(_) => last = Err("PKCS#7: certificate without address".into()),
            Err(e) => last = Err(e),
        }
    }
    last
}

fn prepare_inner(der: &[u8], others: &[Vec<u8>], trust: &Trust, now: u64) -> Result<Prepared, String> {
    let c = Certificate::from_der(der).map_err(|e| format!("certificate: {e}"))?;
    let tbs = c.tbs_certificate();
    let info = crate::chain::info(der).ok_or("certificate")?;
    let now_i = i64::try_from(now).unwrap_or(i64::MAX);
    if now_i < info.not_before || now_i > info.not_after {
        return Err("expired".into());
    }
    crate::chain::path_ok(der, others, trust, now)?;
    let spki = tbs.subject_public_key_info();
    let bits = spki.subject_public_key.as_bytes().ok_or("public key")?;
    let ku = tbs.get_extension::<KeyUsage>().ok().flatten().map(|(_, k)| k);
    let key = match spki.algorithm.oid {
        oid::RSA_ENCRYPTION => {
            if ku.as_ref().is_some_and(|k| !k.key_encipherment()) {
                return Err("key usage: no keyEncipherment".into());
            }
            use rsa::pkcs1::DecodeRsaPublicKey;
            use rsa::traits::PublicKeyParts;
            let k = rsa::RsaPublicKey::from_pkcs1_der(bits).map_err(|e| format!("RSA: {e}"))?;
            RecipientKey::Rsa { n: k.n().to_be_bytes().to_vec(), e: k.e().to_be_bytes().to_vec() }
        }
        oid::EC_PUBLIC_KEY => {
            if ku.as_ref().is_some_and(|k| !k.key_agreement()) {
                return Err("key usage: no keyAgreement".into());
            }
            let curve = spki.algorithm.parameters.as_ref().and_then(|p| p.decode_as::<const_oid::ObjectIdentifier>().ok());
            let bits_n = match curve {
                Some(oid::P256) => 256,
                Some(oid::P384) => 384,
                _ => return Err("EC curve".into()),
            };
            if bits.first() != Some(&4) {
                return Err("EC point".into());
            }
            RecipientKey::Ec { bits: bits_n, point: bits.to_vec() }
        }
        oid::GOST_KEY_256 | oid::GOST_KEY_512 | oid::GOST2001_KEY => {
            let key = match spki.algorithm.oid {
                oid::GOST2001_KEY => GostKey::G2001,
                oid::GOST_KEY_512 => GostKey::G2012_512,
                _ => GostKey::G2012_256,
            };
            let params = spki.algorithm.parameters.as_ref().and_then(|p| p.to_der().ok()).unwrap_or_default();
            let kids = crate::ber::read(&params).and_then(|(t, _)| t.children()).unwrap_or_default();
            let oid_at = |i: usize| kids.get(i).and_then(|t| <const_oid::ObjectIdentifier>::from_der(t.raw).ok()).map(|o| o.to_string());
            let public_param_set = oid_at(0).ok_or("GOST parameters")?;
            let (inner, _) = crate::ber::read(bits).ok_or("GOST point")?;
            RecipientKey::Gost { key, point: inner.value.to_vec(), public_param_set, digest_param_set: oid_at(1) }
        }
        other => return Err(format!("key algorithm {other}")),
    };
    Ok(Prepared {
        issuer: tbs.issuer().to_der().map_err(|e| e.to_string())?,
        serial: tbs.serial_number().as_bytes().to_vec(),
        ski: tbs.get_extension::<SubjectKeyIdentifier>().ok().flatten().map(|(_, s)| s.0.as_bytes().to_vec()),
        key,
        emails: crate::chain::emails_of(der),
        not_after: info.not_after,
        fingerprint: info.fingerprint,
    })
}

/// Выбор сертификата адресата из подготовленных: `preferred` (отпечаток из `SMIMEEncryptionKeyPreference`) важнее,
/// затем — самый поздний срок.
pub fn choose<'a>(list: &'a [Prepared], preferred: Option<&str>) -> Option<&'a Prepared> {
    if let Some(p) = preferred.and_then(|f| list.iter().find(|c| c.fingerprint.eq_ignore_ascii_case(f))) {
        return Some(p);
    }
    list.iter().max_by_key(|c| c.not_after)
}
