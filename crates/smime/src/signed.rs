//! CMS SignedData (RFC 5652 §5): подписант, `messageDigest`, подпись над `signedAttrs`, атрибуты S/MIME.

use cms::cert::IssuerAndSerialNumber;
use cms::signed_data::SignerIdentifier;
use const_oid::ObjectIdentifier as Oid;
use der::asn1::OctetString;
use der::{Decode, Encode};
use spki::AlgorithmIdentifierOwned;
use x509_cert::Certificate;
use x509_cert::attr::{Attribute, Attributes};
use x509_cert::ext::pkix::SubjectKeyIdentifier;

use crate::algs::{self, Hash, PublicKey, Scheme, Unsupported};
use crate::ber::{self, Tlv};
use crate::{Status, oid};

/// Итог проверки SignedData.
#[derive(Debug, Clone)]
pub(crate) struct Checked {
    pub scheme: Scheme,
    /// Подпись математически верна (хэш содержимого совпал и подпись — этим ключом).
    pub valid: bool,
    /// DER сертификата подписанта — как пришёл.
    pub signer: Vec<u8>,
    /// Остальные сертификаты из SignedData — как пришли.
    pub others: Vec<Vec<u8>>,
    pub signing_time: Option<i64>,
    pub capabilities: Vec<String>,
    /// RSA короче 2048 бит.
    pub weak_key: bool,
    /// Подписанное содержимое (`eContent`) у `signed-data`.
    pub content: Option<Vec<u8>>,
}

/// Тип содержимого `ContentInfo` (SignedData, EnvelopedData…) — чтобы отличить шифрованное.
pub(crate) fn content_type(cms: &[u8]) -> Option<Oid> {
    let (ci, _) = ber::read(cms)?;
    let kids = ci.children()?;
    Oid::from_der(kids.first()?.raw).ok()
}

fn malformed(what: &str) -> Status {
    Status::Malformed(what.into())
}

fn unsupported(u: Unsupported) -> Status {
    Status::Unsupported(u.0)
}

struct SignerRaw<'a> {
    sid: SignerIdentifier,
    digest: AlgorithmIdentifierOwned,
    /// `[0] IMPLICIT SignedAttributes` как пришёл.
    attrs: Option<Tlv<'a>>,
    signature_alg: AlgorithmIdentifierOwned,
    signature: Vec<u8>,
}

fn signer_raw<'a>(t: &Tlv<'a>) -> Option<SignerRaw<'a>> {
    let kids = t.children()?;
    let mut it = kids.into_iter();
    let _version = it.next()?;
    let sid = SignerIdentifier::from_der(it.next()?.raw).ok()?;
    let digest = AlgorithmIdentifierOwned::from_der(it.next()?.raw).ok()?;
    let mut next = it.next()?;
    let attrs = if next.tag == 0xA0 {
        let a = next;
        next = it.next()?;
        Some(a)
    } else {
        None
    };
    let signature_alg = AlgorithmIdentifierOwned::from_der(next.raw).ok()?;
    let signature = it.next().filter(|s| s.tag & 0x1f == 0x04)?.octets()?;
    Some(SignerRaw { sid, digest, attrs, signature_alg, signature })
}

fn hash_of(alg: &AlgorithmIdentifierOwned) -> Result<Hash, Status> {
    Hash::from_oid(&alg.oid).ok_or_else(|| {
        Status::Unsupported(match alg.oid {
            oid::MD5 => "MD5".into(),
            oid::GOST94 => "GOST R 34.11-94".into(),
            other => other.to_string(),
        })
    })
}

/// Сертификат подписанта по `sid`: издатель и серийный номер или SKI.
fn find_signer<'c>(sid: &SignerIdentifier, certs: &'c [(Vec<u8>, Option<Certificate>)]) -> Option<&'c (Vec<u8>, Option<Certificate>)> {
    certs.iter().find(|(_, c)| {
        let Some(c) = c else { return false };
        let tbs = c.tbs_certificate();
        match sid {
            SignerIdentifier::IssuerAndSerialNumber(IssuerAndSerialNumber { issuer, serial_number }) => {
                tbs.issuer() == issuer && tbs.serial_number() == serial_number
            }
            SignerIdentifier::SubjectKeyIdentifier(ski) => {
                matches!(tbs.get_extension::<SubjectKeyIdentifier>(), Ok(Some((_, own))) if own.0 == ski.0)
            }
        }
    })
}

fn attr(attrs: &Attributes, oid: Oid) -> Option<&Attribute> {
    attrs.iter().find(|a| a.oid == oid)
}

/// Unix-время атрибута `signingTime` (UTCTime или GeneralizedTime).
fn signing_time(a: &Attribute) -> Option<i64> {
    let v = a.values.iter().next()?;
    let t: x509_cert::time::Time = v.decode_as().ok()?;
    i64::try_from(t.to_unix_duration().as_secs()).ok()
}

/// OID из `SMIMECapabilities` (`SEQUENCE OF SEQUENCE { capabilityID, parameters }`).
fn capabilities(a: &Attribute) -> Vec<String> {
    let Some(v) = a.values.iter().next() else { return Vec::new() };
    let Ok(list) = v.decode_as::<Vec<AlgorithmIdentifierOwned>>() else { return Vec::new() };
    list.into_iter().map(|c| c.oid.to_string()).collect()
}

/// Проверить SignedData `cms`; `detached` — подписанное содержимое отдельной подписи (`multipart/signed`).
/// Ошибка — итог без проверки (повреждено, алгоритм не поддерживается, нет сертификата).
pub(crate) fn check(cms: &[u8], detached: Option<&[u8]>) -> Result<Checked, Status> {
    let (ci, _) = ber::read(cms).ok_or_else(|| malformed("ContentInfo"))?;
    let ci = ci.children().ok_or_else(|| malformed("ContentInfo"))?;
    let ct = ci.first().and_then(|t| Oid::from_der(t.raw).ok()).ok_or_else(|| malformed("content type"))?;
    if ct != oid::SIGNED_DATA {
        return Err(Status::NotSigned);
    }
    let explicit = ci.get(1).filter(|t| t.tag == 0xA0).ok_or_else(|| malformed("SignedData"))?;
    let sd = explicit.children().and_then(|k| k.into_iter().next()).ok_or_else(|| malformed("SignedData"))?;
    let parts = sd.children().ok_or_else(|| malformed("SignedData"))?;
    // version, digestAlgorithms, encapContentInfo, [0] certificates?, [1] crls?, signerInfos
    let encap = parts.get(2).ok_or_else(|| malformed("encapContentInfo"))?;
    let encap = encap.children().ok_or_else(|| malformed("encapContentInfo"))?;
    let econtent_type = encap.first().and_then(|t| Oid::from_der(t.raw).ok()).ok_or_else(|| malformed("eContentType"))?;
    let econtent = match encap.get(1).filter(|t| t.tag == 0xA0) {
        Some(e) => {
            let inner = e.children().and_then(|k| k.into_iter().next()).ok_or_else(|| malformed("eContent"))?;
            Some(inner.octets().ok_or_else(|| malformed("eContent"))?)
        }
        None => None,
    };
    let mut certs: Vec<(Vec<u8>, Option<Certificate>)> = Vec::new();
    let mut signer_infos = None;
    for p in &parts[3..] {
        match p.tag {
            0xA0 => {
                for c in p.children().ok_or_else(|| malformed("certificates"))? {
                    if c.tag == 0x30 {
                        certs.push((c.raw.to_vec(), Certificate::from_der(c.raw).ok()));
                    }
                }
            }
            0xA1 => {}
            0x31 => signer_infos = Some(*p),
            _ => return Err(malformed("SignedData")),
        }
    }
    let signer_infos = signer_infos.and_then(|s| s.children()).ok_or_else(|| malformed("signerInfos"))?;
    if signer_infos.is_empty() {
        return Err(malformed("no signer"));
    }
    let content: Vec<u8> = match (detached, &econtent) {
        (_, Some(e)) => e.clone(),
        (Some(d), None) => d.to_vec(),
        (None, None) => return Err(malformed("no content")),
    };
    // Несколько подписантов — берём первого, чья подпись верна; иначе отчёт по первому.
    let mut first: Option<Result<Checked, Status>> = None;
    for si in &signer_infos {
        let r = check_signer(si, &certs, &content, &econtent_type);
        if matches!(&r, Ok(c) if c.valid) {
            return r.map(|mut c| {
                c.content = econtent.clone();
                c
            });
        }
        first.get_or_insert(r);
    }
    first.unwrap_or_else(|| Err(malformed("no signer"))).map(|mut c| {
        c.content = econtent;
        c
    })
}

fn check_signer(si: &Tlv<'_>, certs: &[(Vec<u8>, Option<Certificate>)], content: &[u8], econtent_type: &Oid) -> Result<Checked, Status> {
    let s = signer_raw(si).ok_or_else(|| malformed("SignerInfo"))?;
    let hash = hash_of(&s.digest)?;
    let scheme = algs::cms_scheme(&s.signature_alg, hash).map_err(unsupported)?;
    let (signer_der, signer) = find_signer(&s.sid, certs).ok_or(Status::NoCertificate)?;
    let signer = signer.as_ref().ok_or_else(|| malformed("signer certificate"))?;
    let key = algs::public_key(signer.tbs_certificate().subject_public_key_info()).map_err(unsupported)?;
    let weak_key = matches!(&key, PublicKey::Rsa(k) if rsa::traits::PublicKeyParts::n(k).bits_vartime() < 2048);
    let digest = hash.digest(content);
    let mut out = Checked {
        scheme,
        valid: false,
        signer: signer_der.clone(),
        others: certs.iter().filter(|(d, _)| d != signer_der).map(|(d, _)| d.clone()).collect(),
        signing_time: None,
        capabilities: Vec::new(),
        weak_key,
        content: None,
    };
    let Some(raw) = s.attrs else {
        // Без подписанных атрибутов подпись — прямо над содержимым (RFC 5652 §5.4), тип — только id-data.
        out.valid = *econtent_type == oid::DATA && algs::verify(&key, scheme, content, &s.signature);
        return Ok(out);
    };
    // Подпись — над DER `SET OF Attribute` с тегом SET вместо [0]: исходные байты; если отправитель
    // закодировал не по DER (порядок), — ещё и пересобранные по DER.
    let mut as_set = ber::header(0x31, raw.value.len());
    as_set.extend_from_slice(raw.value);
    let attrs = Attributes::from_der(&as_set).or_else(|_| Attributes::from_ber(&as_set)).map_err(|_| malformed("signedAttrs"))?;
    let types_ok = attr(&attrs, oid::ATTR_CONTENT_TYPE)
        .and_then(|a| a.values.iter().next())
        .and_then(|v| v.decode_as::<Oid>().ok())
        .is_some_and(|t| t == *econtent_type);
    let digest_ok = attr(&attrs, oid::ATTR_MESSAGE_DIGEST)
        .and_then(|a| a.values.iter().next())
        .and_then(|v| v.decode_as::<OctetString>().ok())
        .is_some_and(|d| d.as_bytes() == digest.as_slice());
    out.signing_time = attr(&attrs, oid::ATTR_SIGNING_TIME).and_then(signing_time);
    out.capabilities = attr(&attrs, oid::ATTR_SMIME_CAPABILITIES).map(capabilities).unwrap_or_default();
    if raw.indefinite {
        as_set = attrs.to_der().map_err(|_| malformed("signedAttrs"))?;
    }
    let mut signed_ok = algs::verify(&key, scheme, &as_set, &s.signature);
    if !signed_ok
        && let Ok(resorted) = attrs.to_der()
        && resorted != as_set
    {
        signed_ok = algs::verify(&key, scheme, &resorted, &s.signature);
    }
    out.valid = types_ok && digest_ok && signed_ok;
    Ok(out)
}
