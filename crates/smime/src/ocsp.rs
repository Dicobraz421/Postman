//! OCSP (RFC 6960) — отзыв на Linux (LIN-05d), как CryptoAPI на Windows: сначала OCSP, затем CRL.
//!
//! Ядро берёт адреса ответчика из AIA сертификата уже доверенной цепочки ([`responders`]), собирает запрос с nonce
//! ([`request`]) и отправляет его; ответ — чужие байты: разбор и проверка — только [`check`] в воркере. Проверяется:
//! статус ответа, подпись ответчика (сам издатель или его делегат с EKU `OCSPSigning`, выпущенный издателем и
//! действующий), `CertID` (хэши имени и ключа издателя, серийный номер), nonce (если ответчик его вернул — тот же),
//! срок (`thisUpdate`, `nextUpdate`). Итог — тот же [`Checked`], что у CRL.

use der::asn1::GeneralizedTime;
use der::{Decode, Encode};
use serde::{Deserialize, Serialize};
use x509_cert::Certificate;
use x509_cert::ext::pkix::name::GeneralName;
use x509_cert::ext::pkix::{AuthorityInfoAccessSyntax, ExtendedKeyUsage};

use crate::ber::{self, Tlv};
use crate::crl::{Checked, Verdict};
use crate::{Hash, algs};

/// Больше ответ OCSP не бывает (у ответчиков — единицы КБ).
pub const MAX_RESPONSE: usize = 256 * 1024;
/// Адресов ответчика из одного сертификата — не больше.
const MAX_RESPONDERS: usize = 4;
const MAX_URL: usize = 2048;
/// Расхождение часов с ответчиком.
const SKEW: i64 = 5 * 60;
/// Ответ без `nextUpdate` годен столько от `thisUpdate`.
const NO_NEXT_UPDATE: i64 = 60 * 60;

/// id-ad-ocsp, 1.3.6.1.5.5.7.48.1.
const AD_OCSP: der::asn1::ObjectIdentifier = der::asn1::ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.48.1");
/// id-kp-OCSPSigning, 1.3.6.1.5.5.7.3.9.
const KP_OCSP_SIGNING: der::asn1::ObjectIdentifier = der::asn1::ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.3.9");
// DER OID без тега и длины.
const OID_BASIC: &[u8] = &[0x2B, 0x06, 0x01, 0x05, 0x05, 0x07, 0x30, 0x01, 0x01];
const OID_NONCE: &[u8] = &[0x2B, 0x06, 0x01, 0x05, 0x05, 0x07, 0x30, 0x01, 0x02];
const OID_SHA1: &[u8] = &[0x2B, 0x0E, 0x03, 0x02, 0x1A];
const OID_SHA256: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01];

/// Почему по OCSP не проверить: ядро переходит к CRL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum OcspError {
    #[error("OCSP response too large")]
    TooLarge,
    #[error("malformed: {0}")]
    Malformed(String),
    /// `responseStatus` не `successful` (код).
    #[error("OCSP responder status {0}")]
    Status(u8),
    /// Ответ не про этот сертификат.
    #[error("OCSP response is for another certificate")]
    WrongCert,
    /// nonce ответа — не наш.
    #[error("OCSP nonce mismatch")]
    Nonce,
    /// Подписал не издатель и не его делегат.
    #[error("OCSP responder is not authorized")]
    Unauthorized,
    #[error("bad OCSP signature")]
    BadSignature,
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("OCSP response not yet valid")]
    NotYetValid,
    #[error("OCSP response expired")]
    Expired,
    /// Ответчик не знает сертификат (`unknown`).
    #[error("certificate unknown to the OCSP responder")]
    Unknown,
}

fn malformed(what: &str) -> OcspError {
    OcspError::Malformed(what.into())
}

/// Адреса ответчика OCSP (`http`/`https`) из AIA сертификата.
pub fn responders(cert: &[u8]) -> Vec<String> {
    let Ok(c) = Certificate::from_der(cert) else { return Vec::new() };
    let Ok(Some((_, aia))) = c.tbs_certificate().get_extension::<AuthorityInfoAccessSyntax>() else { return Vec::new() };
    let mut out: Vec<String> = Vec::new();
    for ad in aia.0.iter().filter(|a| a.access_method == AD_OCSP) {
        let GeneralName::UniformResourceIdentifier(uri) = &ad.access_location else { continue };
        let url = uri.as_str();
        let lower = url.to_ascii_lowercase();
        let web = lower.starts_with("http://") || lower.starts_with("https://");
        if web && url.len() <= MAX_URL && url.bytes().all(|b| b.is_ascii_graphic()) && !out.iter().any(|u| u == url) {
            out.push(url.to_owned());
        }
    }
    out.truncate(MAX_RESPONDERS);
    out
}

fn tlv(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = ber::header(tag, body.len());
    out.extend_from_slice(body);
    out
}

/// Хэши `CertID` (имя и ключ издателя) для алгоритма `h`, серийный номер (содержимое INTEGER).
struct Id {
    name: Vec<u8>,
    key: Vec<u8>,
    serial: Vec<u8>,
}

fn cert_id(cert: &Certificate, issuer: &Certificate, h: Hash) -> Option<Id> {
    let name = cert.tbs_certificate().issuer().to_der().ok()?;
    let key = issuer.tbs_certificate().subject_public_key_info().subject_public_key.raw_bytes();
    Some(Id { name: h.digest(&name), key: h.digest(key), serial: cert.tbs_certificate().serial_number().as_bytes().to_vec() })
}

/// Запрос OCSP (DER) о сертификате `cert`, выпущенном `issuer` (оба — из доверенной цепочки), с nonce `nonce`
/// (1–32 байта, RFC 8954). `CertID` — SHA-1, как у всех ответчиков.
pub fn request(cert: &[u8], issuer: &[u8], nonce: &[u8]) -> Option<Vec<u8>> {
    if nonce.is_empty() || nonce.len() > 32 {
        return None;
    }
    let (c, i) = (Certificate::from_der(cert).ok()?, Certificate::from_der(issuer).ok()?);
    let id = cert_id(&c, &i, Hash::Sha1)?;
    let alg = tlv(0x30, &[tlv(0x06, OID_SHA1), vec![0x05, 0x00]].concat());
    let cert_id = tlv(0x30, &[alg, tlv(0x04, &id.name), tlv(0x04, &id.key), tlv(0x02, &id.serial)].concat());
    let request_list = tlv(0x30, &tlv(0x30, &cert_id));
    let ext = tlv(0x30, &[tlv(0x06, OID_NONCE), tlv(0x04, &tlv(0x04, nonce))].concat());
    let exts = tlv(0xA2, &tlv(0x30, &ext));
    Some(tlv(0x30, &tlv(0x30, &[request_list, exts].concat())))
}

fn kids<'a>(t: &Tlv<'a>, tag: u8, what: &str) -> Result<Vec<Tlv<'a>>, OcspError> {
    if t.tag != tag || t.indefinite {
        return Err(malformed(what));
    }
    t.children().ok_or_else(|| malformed(what))
}

fn time(t: &Tlv<'_>) -> Result<i64, OcspError> {
    let g = GeneralizedTime::from_der(t.raw).map_err(|_| malformed("time"))?;
    Ok(i64::try_from(g.to_unix_duration().as_secs()).unwrap_or(i64::MAX))
}

/// Серийный номер без ведущих нулей.
fn trim(serial: &[u8]) -> &[u8] {
    let start = serial.iter().position(|b| *b != 0).unwrap_or(serial.len());
    &serial[start..]
}

/// `CertID` ответа — про наш сертификат (SHA-1 или SHA-256).
fn same_cert(id: &Tlv<'_>, cert: &Certificate, issuer: &Certificate) -> Result<bool, OcspError> {
    let f = kids(id, 0x30, "CertID")?;
    let [alg, name, key, serial] = f.as_slice() else { return Err(malformed("CertID")) };
    let oid = kids(alg, 0x30, "CertID")?.first().filter(|t| t.tag == 0x06).map(|t| t.value).ok_or_else(|| malformed("CertID"))?;
    let h = match oid {
        OID_SHA1 => Hash::Sha1,
        OID_SHA256 => Hash::Sha256,
        _ => return Err(OcspError::Unsupported("CertID hash".into())),
    };
    let ours = cert_id(cert, issuer, h).ok_or_else(|| malformed("certificate"))?;
    if name.tag != 0x04 || key.tag != 0x04 || serial.tag != 0x02 {
        return Err(malformed("CertID"));
    }
    Ok(name.value == ours.name && key.value == ours.key && trim(serial.value) == trim(&ours.serial))
}

/// Подписант ответа (`responderID`) — издатель или его делегат из `certs`.
fn signer(responder: &Tlv<'_>, issuer: &Certificate, certs: &[Certificate], now: i64) -> Result<Certificate, OcspError> {
    let inner = responder.children().and_then(|c| c.into_iter().next()).ok_or_else(|| malformed("responderID"))?;
    let matches = |c: &Certificate| -> bool {
        match responder.tag {
            0xA1 => c.tbs_certificate().subject().to_der().is_ok_and(|n| n == inner.raw),
            0xA2 => {
                inner.tag == 0x04 && Hash::Sha1.digest(c.tbs_certificate().subject_public_key_info().subject_public_key.raw_bytes()) == inner.value
            }
            _ => false,
        }
    };
    if matches(issuer) {
        return Ok(issuer.clone());
    }
    let delegate = certs.iter().find(|c| matches(c)).ok_or(OcspError::Unauthorized)?;
    let tbs = delegate.tbs_certificate();
    // Делегат выпущен издателем (подпись его ключом), действует и имеет EKU `OCSPSigning`.
    if tbs.issuer() != issuer.tbs_certificate().subject() {
        return Err(OcspError::Unauthorized);
    }
    let tbs_der = tbs.to_der().map_err(|_| malformed("delegate"))?;
    let sig_alg = delegate.signature_algorithm().to_der().map_err(|_| malformed("delegate"))?;
    let alg_content = ber::read(&sig_alg).map(|(t, _)| t.value).ok_or_else(|| malformed("delegate"))?;
    let signature = delegate.signature().raw_bytes();
    if algs::verify_signed(alg_content, issuer.tbs_certificate().subject_public_key_info(), &tbs_der, signature) != Some(true) {
        return Err(OcspError::Unauthorized);
    }
    let v = tbs.validity();
    let secs = |t: &x509_cert::time::Time| i64::try_from(t.to_unix_duration().as_secs()).unwrap_or(i64::MAX);
    if now < secs(&v.not_before) || now > secs(&v.not_after) {
        return Err(OcspError::Unauthorized);
    }
    let eku = tbs.get_extension::<ExtendedKeyUsage>().map_err(|_| malformed("delegate EKU"))?;
    if !eku.is_some_and(|(_, e)| e.0.contains(&KP_OCSP_SIGNING)) {
        return Err(OcspError::Unauthorized);
    }
    Ok(delegate.clone())
}

/// nonce из `responseExtensions` (`[1]`), если есть.
fn response_nonce(ext: Option<&Tlv<'_>>) -> Result<Option<Vec<u8>>, OcspError> {
    let Some(ext) = ext else { return Ok(None) };
    let list = ext.children().and_then(|c| c.into_iter().next()).ok_or_else(|| malformed("extensions"))?;
    for e in kids(&list, 0x30, "extensions")? {
        let f = kids(&e, 0x30, "extension")?;
        if f.first().is_some_and(|o| o.tag == 0x06 && o.value == OID_NONCE) {
            let value = f.last().filter(|v| v.tag == 0x04).ok_or_else(|| malformed("nonce"))?;
            // Значение — OCTET STRING с nonce (RFC 8954); старые ответчики кладут байты без обёртки.
            let inner = ber::read(value.value).filter(|(t, rest)| t.tag == 0x04 && rest.is_empty()).map(|(t, _)| t.value.to_vec());
            return Ok(Some(inner.unwrap_or_else(|| value.value.to_vec())));
        }
    }
    Ok(None)
}

/// Проверить ответ OCSP `response` о сертификате `cert`, выпущенном `issuer` (оба — из цепочки, которую воркер уже
/// довёл до доверенного якоря), на запрос с nonce `nonce`, на момент `now`. Не паникует: сбой разбора — `Err`.
pub fn check(cert: &[u8], issuer: &[u8], nonce: &[u8], response: &[u8], now: u64) -> Result<Checked, OcspError> {
    if response.len() > MAX_RESPONSE {
        return Err(OcspError::TooLarge);
    }
    let now = i64::try_from(now).unwrap_or(i64::MAX);
    let cert = Certificate::from_der(cert).map_err(|_| malformed("certificate"))?;
    let issuer = Certificate::from_der(issuer).map_err(|_| malformed("issuer certificate"))?;
    let (top, rest) = ber::read(response).ok_or_else(|| malformed("response"))?;
    if !rest.is_empty() {
        return Err(malformed("response"));
    }
    let top = kids(&top, 0x30, "response")?;
    let status = top.first().filter(|t| t.tag == 0x0A && t.value.len() == 1).map(|t| t.value[0]).ok_or_else(|| malformed("status"))?;
    if status != 0 {
        return Err(OcspError::Status(status));
    }
    let bytes = top
        .get(1)
        .filter(|t| t.tag == 0xA0)
        .and_then(|t| t.children())
        .and_then(|c| c.into_iter().next())
        .ok_or_else(|| malformed("responseBytes"))?;
    let rb = kids(&bytes, 0x30, "responseBytes")?;
    let [kind, body] = rb.as_slice() else { return Err(malformed("responseBytes")) };
    if kind.tag != 0x06 || kind.value != OID_BASIC {
        return Err(OcspError::Unsupported("response type".into()));
    }
    if body.tag != 0x04 {
        return Err(malformed("responseBytes"));
    }
    let (basic, rest) = ber::read(body.value).ok_or_else(|| malformed("BasicOCSPResponse"))?;
    if !rest.is_empty() {
        return Err(malformed("BasicOCSPResponse"));
    }
    let b = kids(&basic, 0x30, "BasicOCSPResponse")?;
    let (tbs, alg, sig) = match b.as_slice() {
        [tbs, alg, sig] | [tbs, alg, sig, _] => (tbs, alg, sig),
        _ => return Err(malformed("BasicOCSPResponse")),
    };
    if tbs.tag != 0x30 || tbs.indefinite || alg.tag != 0x30 || sig.tag != 0x03 {
        return Err(malformed("BasicOCSPResponse"));
    }
    let certs: Vec<Certificate> = match b.get(3) {
        Some(c) if c.tag == 0xA0 => {
            let seq = c.children().and_then(|v| v.into_iter().next()).ok_or_else(|| malformed("certs"))?;
            kids(&seq, 0x30, "certs")?.iter().take(8).filter_map(|t| Certificate::from_der(t.raw).ok()).collect()
        }
        Some(_) => return Err(malformed("certs")),
        None => Vec::new(),
    };

    // ResponseData: [0] version?, responderID, producedAt, responses, [1] extensions?
    let mut d = kids(tbs, 0x30, "ResponseData")?;
    if d.first().is_some_and(|t| t.tag == 0xA0) {
        d.remove(0);
    }
    let [responder, _produced, responses, ext @ ..] = d.as_slice() else { return Err(malformed("ResponseData")) };
    let signed_by = signer(responder, &issuer, &certs, now)?;
    let Some((0, signature)) = sig.value.split_first() else { return Err(malformed("signature")) };
    match algs::verify_signed(alg.value, signed_by.tbs_certificate().subject_public_key_info(), tbs.raw, signature) {
        Some(true) => {}
        Some(false) => return Err(OcspError::BadSignature),
        None => return Err(OcspError::Unsupported("signature algorithm".into())),
    }
    if let Some(theirs) = response_nonce(ext.first().filter(|t| t.tag == 0xA1))?
        && theirs != nonce
    {
        return Err(OcspError::Nonce);
    }

    for single in kids(responses, 0x30, "responses")?.iter().take(16) {
        let s = kids(single, 0x30, "SingleResponse")?;
        let [id, status, this_update, more @ ..] = s.as_slice() else { return Err(malformed("SingleResponse")) };
        if !same_cert(id, &cert, &issuer)? {
            continue;
        }
        let this_update = time(this_update)?;
        let next_update = match more.first().filter(|t| t.tag == 0xA0) {
            Some(n) => Some(time(&n.children().and_then(|c| c.into_iter().next()).ok_or_else(|| malformed("nextUpdate"))?)?),
            None => None,
        };
        if this_update > now + SKEW {
            return Err(OcspError::NotYetValid);
        }
        let next_update = next_update.unwrap_or(this_update.saturating_add(NO_NEXT_UPDATE));
        if now > next_update {
            return Err(OcspError::Expired);
        }
        let verdict = match status.tag {
            0x80 => Verdict::NotRevoked,
            0xA1 => {
                let at = status.children().and_then(|c| c.into_iter().next()).ok_or_else(|| malformed("revokedInfo"))?;
                Verdict::Revoked { at: time(&at)? }
            }
            0x82 => return Err(OcspError::Unknown),
            _ => return Err(malformed("certStatus")),
        };
        return Ok(Checked { verdict, this_update, next_update });
    }
    Err(OcspError::WrongCert)
}

#[cfg(all(test, feature = "testkit"))]
#[path = "ocsp/tests.rs"]
mod tests;
