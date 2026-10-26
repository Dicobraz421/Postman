//! Цепочка подписанта к якорям (`rustls-webpki`), назначение сертификата, «Недоверенные», адрес ↔ «От»,
//! сводка сертификатов.

use der::Decode;
use digest::Digest;
use pki_types::{CertificateDer, TrustAnchor, UnixTime};
use webpki::{EndEntityCert, KeyUsage};
use x509_cert::Certificate;
use x509_cert::ext::pkix::name::GeneralName;
use x509_cert::ext::pkix::{KeyUsage as Ku, SubjectAltName};
use x509_cert::name::Name;

use crate::signed::Checked;
use crate::{CertInfo, Report, Status, Trust, algs};

/// EKU «Защита электронной почты» (1.3.6.1.5.5.7.3.4) — содержимое OID.
const EKU_EMAIL: &[u8] = &[0x2B, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x04];

fn sha256_hex(der: &[u8]) -> String {
    sha2::Sha256::digest(der).iter().map(|b| format!("{b:02X}")).collect()
}

/// CN имени или всё имя (RFC 4514), если CN нет.
pub(crate) fn name_text(n: &Name) -> String {
    match n.common_name() {
        Ok(Some(cn)) => cn.value().into_owned(),
        _ => n.to_string(),
    }
}

fn organization(n: &Name) -> Option<String> {
    n.organization().ok().flatten().map(|o| o.value().into_owned())
}

/// Адреса сертификата: SAN `rfc822Name` и устаревший `emailAddress` в имени, в нижнем регистре.
fn emails(c: &Certificate) -> Vec<String> {
    let tbs = c.tbs_certificate();
    let mut out: Vec<String> = Vec::new();
    if let Ok(Some((_, san))) = tbs.get_extension::<SubjectAltName>() {
        for n in san.0 {
            if let GeneralName::Rfc822Name(m) = n {
                out.push(m.as_str().trim().to_lowercase());
            }
        }
    }
    if let Ok(Some(m)) = tbs.subject().email_address() {
        out.push(m.as_str().trim().to_lowercase());
    }
    out.dedup();
    out
}

fn secs(t: &x509_cert::time::Time) -> i64 {
    i64::try_from(t.to_unix_duration().as_secs()).unwrap_or(i64::MAX)
}

/// Сводка сертификата для карточки.
pub(crate) fn info(der: &[u8]) -> Option<CertInfo> {
    let c = Certificate::from_der(der).ok()?;
    let tbs = c.tbs_certificate();
    Some(CertInfo {
        subject: name_text(tbs.subject()),
        organization: organization(tbs.subject()),
        emails: emails(&c),
        issuer: name_text(tbs.issuer()),
        issuer_organization: organization(tbs.issuer()),
        serial: tbs.serial_number().as_bytes().iter().map(|b| format!("{b:02X}")).collect(),
        not_before: secs(&tbs.validity().not_before),
        not_after: secs(&tbs.validity().not_after),
        fingerprint: sha256_hex(der),
        key: algs::key_kind(tbs.subject_public_key_info()),
    })
}

/// Свой сертификат годится для подписи почты: KeyUsage и EKU («Защита почты» или без EKU).
pub(crate) fn can_sign_mail(der: &[u8]) -> bool {
    use x509_cert::ext::pkix::ExtendedKeyUsage;
    let Ok(c) = Certificate::from_der(der) else { return false };
    let eku_ok = match c.tbs_certificate().get_extension::<ExtendedKeyUsage>() {
        Ok(Some((_, eku))) => eku.0.contains(&crate::oid::EKU_EMAIL_PROTECTION),
        Ok(None) => true,
        Err(_) => false,
    };
    eku_ok && usage_allows_signing(&c)
}

/// KeyUsage разрешает подпись (RFC 8550 §4.4.2): нет расширения — разрешает.
fn usage_allows_signing(c: &Certificate) -> bool {
    match c.tbs_certificate().get_extension::<Ku>() {
        Ok(Some((_, ku))) => ku.digital_signature() || ku.non_repudiation(),
        Ok(None) => true,
        Err(_) => false,
    }
}

fn webpki_status(e: &webpki::Error) -> Status {
    use webpki::Error as E;
    match e {
        E::UnknownIssuer => Status::Untrusted("unknown issuer".into()),
        E::CertExpired { .. } | E::CertNotValidYet { .. } | E::InvalidCertValidity => Status::Expired,
        E::RequiredEkuNotFoundContext(_) | E::EmptyEkuExtension => Status::WrongUsage,
        E::InvalidSignatureForPublicKey => Status::Untrusted("issuer signature is invalid".into()),
        E::UnsupportedSignatureAlgorithmContext(_) | E::UnsupportedSignatureAlgorithmForPublicKeyContext(_) => {
            Status::Untrusted("unsupported issuer algorithm".into())
        }
        E::UnsupportedCriticalExtension => Status::Untrusted("unsupported critical extension".into()),
        E::CaUsedAsEndEntity => Status::WrongUsage,
        other => Status::Untrusted(format!("{other:?}")),
    }
}

/// Сертификат получателя (Ф4) доведён до доверенного якоря (EKU «Защита почты», если есть), не запрещён; ошибка —
/// текстом для журнала и строки «нет сертификата».
pub(crate) fn path_ok(leaf: &[u8], others: &[Vec<u8>], trust: &Trust, now: u64) -> Result<(), String> {
    let disallowed: Vec<String> = trust.disallowed.iter().map(|d| sha256_hex(d)).collect();
    let anchors: Vec<TrustAnchor<'_>> = trust
        .roots
        .iter()
        .filter_map(|a| webpki::anchor_from_trusted_cert(&CertificateDer::from(a.der.as_slice())).ok().map(|t| t.to_owned()))
        .collect();
    let intermediates: Vec<CertificateDer<'_>> = others.iter().chain(&trust.intermediates).map(|d| CertificateDer::from(d.as_slice())).collect();
    let ee_der = CertificateDer::from(leaf);
    let ee = EndEntityCert::try_from(&ee_der).map_err(|e| format!("{e:?}"))?;
    let time = UnixTime::since_unix_epoch(std::time::Duration::from_secs(now));
    let path = ee
        .verify_for_usage(algs::chain_algs(), &anchors, &intermediates, time, KeyUsage::required_if_present(EKU_EMAIL), None, None)
        .map_err(|e| format!("{:?}", webpki_status(&e)))?;
    let mut chain = vec![leaf.to_vec()];
    chain.extend(path.intermediate_certificates().map(|c| c.der().to_vec()));
    if chain.iter().any(|d| disallowed.contains(&sha256_hex(d))) {
        return Err("distrusted".into());
    }
    Ok(())
}

/// Адреса сертификата (для получателей Ф4).
pub(crate) fn emails_of(der: &[u8]) -> Vec<String> {
    Certificate::from_der(der).map(|c| emails(&c)).unwrap_or_default()
}

/// Итог по цепочке: статус, сводки, DER цепочки, корпоративный ли корень, адрес.
pub(crate) fn evaluate(report: &mut Report, checked: &Checked, trust: &Trust, now: u64) {
    let Ok(leaf) = Certificate::from_der(&checked.signer) else {
        report.status = Status::Malformed("signer certificate".into());
        return;
    };
    let leaf_emails = emails(&leaf);
    report.address_ok = !report.from.is_empty() && leaf_emails.contains(&report.from);
    let leaf_info = info(&checked.signer);
    if !checked.valid {
        report.status = Status::Invalid;
        report.certs = leaf_info.into_iter().collect();
        return;
    }
    let disallowed: Vec<String> = trust.disallowed.iter().map(|d| sha256_hex(d)).collect();
    let anchors: Vec<(TrustAnchor<'_>, usize)> = trust
        .roots
        .iter()
        .enumerate()
        .filter_map(|(i, a)| {
            let der = CertificateDer::from(a.der.as_slice());
            webpki::anchor_from_trusted_cert(&der).ok().map(|t| (t.to_owned(), i))
        })
        .collect();
    let trust_anchors: Vec<TrustAnchor<'_>> = anchors.iter().map(|(t, _)| t.clone()).collect();
    let intermediates: Vec<CertificateDer<'_>> =
        checked.others.iter().chain(&trust.intermediates).map(|d| CertificateDer::from(d.as_slice())).collect();
    let ee_der = CertificateDer::from(checked.signer.as_slice());
    let ee = match EndEntityCert::try_from(&ee_der) {
        Ok(ee) => ee,
        Err(e) => {
            report.status = Status::Malformed(format!("signer certificate: {e:?}"));
            report.certs = leaf_info.into_iter().collect();
            return;
        }
    };
    let time = UnixTime::since_unix_epoch(std::time::Duration::from_secs(now));
    let path = ee.verify_for_usage(algs::chain_algs(), &trust_anchors, &intermediates, time, KeyUsage::required_if_present(EKU_EMAIL), None, None);
    let mut chain: Vec<Vec<u8>> = vec![checked.signer.clone()];
    let status = match path {
        Ok(path) => {
            chain.extend(path.intermediate_certificates().map(|c| c.der().to_vec()));
            let anchor = path.anchor();
            let root = anchors.iter().find(|(t, _)| {
                t.subject.as_ref() == anchor.subject.as_ref() && t.subject_public_key_info.as_ref() == anchor.subject_public_key_info.as_ref()
            });
            if let Some((_, i)) = root
                && let Some(a) = trust.roots.get(*i)
            {
                report.corporate = a.corporate;
                chain.push(a.der.clone());
            }
            if usage_allows_signing(&leaf) { Status::Valid } else { Status::WrongUsage }
        }
        Err(e) => {
            // Для карточки — сертификаты из письма (издатель), без доверия.
            chain.extend(checked.others.iter().cloned());
            webpki_status(&e)
        }
    };
    report.status = if chain.iter().any(|d| disallowed.contains(&sha256_hex(d))) { Status::Distrusted } else { status };
    report.certs = chain.iter().filter_map(|d| info(d)).collect();
    report.chain = chain;
}
