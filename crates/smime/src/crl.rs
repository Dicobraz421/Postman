//! Отзыв без СКЗИ (S5-12.1д, `docs/SMIME.md` §8): CRL (RFC 5280 §5) издателя из уже доверенной цепочки — разбор с
//! пределами, подпись CRL ключом издателя (таблица алгоритмов цепочки `algs`, в том числе ГОСТ Р 34.10-2012 —
//! **не сертифицировано**), поиск серийного номера, срок `nextUpdate`.
//!
//! CRL — чужие байты: проверка только в воркере. Скачивает его ядро — по адресам [`distribution_points`] из
//! сертификатов, которые выпустил доверенный УЦ (адреса — от УЦ, не от отправителя; решение владельца №3).
//!
//! Не поддерживаются (итог — «не проверен»): дельта-CRL, косвенные CRL (`indirectCRL`, `certificateIssuer`), CRL на
//! часть причин (`onlySomeReasons`) или чужой части сертификатов, неизвестные критические расширения.

use const_oid::db::rfc5280 as ids;
use der::Decode;
use serde::{Deserialize, Serialize};
use spki::AlgorithmIdentifierOwned;
use x509_cert::Certificate;
use x509_cert::crl::RevokedCert;
use x509_cert::ext::Extensions;
use x509_cert::ext::pkix::crl::dp::IssuingDistributionPoint;
use x509_cert::ext::pkix::name::{DistributionPointName, GeneralName};
use x509_cert::ext::pkix::{BasicConstraints, CrlDistributionPoints, KeyUsage, KeyUsages};
use x509_cert::name::Name;
use x509_cert::time::Time;

use crate::{algs, ber};

/// Больше CRL не проверяем (у крупных УЦ списки — десятки мегабайт); кадр воркера — 64 МБ.
pub const MAX_CRL: usize = 32 * 1024 * 1024;
/// Адресов CRL из одного сертификата — не больше.
const MAX_POINTS: usize = 4;
/// Длина адреса CRL.
const MAX_URL: usize = 2048;

/// Что сказал CRL о сертификате.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    NotRevoked,
    /// Отозван; `at` — дата отзыва из CRL, Unix-время.
    Revoked {
        at: i64,
    },
}

/// Итог проверки по CRL, которому можно верить до `next_update` (ядро держит CRL в кэше до этого срока).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checked {
    pub verdict: Verdict,
    /// `thisUpdate`, `nextUpdate` — Unix-время.
    pub this_update: i64,
    pub next_update: i64,
}

/// Почему по CRL не проверить: итог — «отзыв не проверен».
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum CrlError {
    #[error("CRL too large")]
    TooLarge,
    #[error("malformed: {0}")]
    Malformed(String),
    /// CRL или сертификат — не этого издателя.
    #[error("issuer mismatch")]
    WrongIssuer,
    /// В `KeyUsage` издателя нет `cRLSign`.
    #[error("issuer is not a CRL signer")]
    NotCrlSigner,
    /// Подпись CRL — не ключом издателя.
    #[error("bad CRL signature")]
    BadSignature,
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// `thisUpdate` позже «сейчас».
    #[error("CRL not yet valid")]
    NotYetValid,
    /// `nextUpdate` прошёл.
    #[error("CRL expired")]
    Expired,
}

fn malformed(what: &str) -> CrlError {
    CrlError::Malformed(what.into())
}

fn unsupported(what: impl Into<String>) -> CrlError {
    CrlError::Unsupported(what.into())
}

fn secs(t: &Time) -> i64 {
    i64::try_from(t.to_unix_duration().as_secs()).unwrap_or(i64::MAX)
}

/// Серийный номер без ведущих нулей (у INTEGER DER знаковый `00` впереди — не часть номера).
fn trim(serial: &[u8]) -> &[u8] {
    let start = serial.iter().position(|b| *b != 0).unwrap_or(serial.len());
    &serial[start..]
}

/// Проверить по CRL `crl` (DER) сертификат `cert`, выпущенный `issuer` (оба — DER из цепочки, которую воркер уже
/// довёл до доверенного якоря), на момент `now` (Unix, секунды). Не паникует: любой сбой разбора — `Err`.
pub fn check(cert: &[u8], issuer: &[u8], crl: &[u8], now: u64) -> Result<Checked, CrlError> {
    if crl.len() > MAX_CRL {
        return Err(CrlError::TooLarge);
    }
    std::panic::catch_unwind(|| check_inner(cert, issuer, crl, now)).unwrap_or_else(|_| Err(malformed("parser failure")))
}

/// Поля `TBSCertList` (RFC 5280 §5.1): исходные байты для подписи, остальное — разобрано.
struct Tbs<'a> {
    signature: &'a [u8],
    issuer: Name,
    this_update: Time,
    next_update: Option<Time>,
    revoked: Option<&'a [u8]>,
    extensions: Extensions,
}

/// Без строгого разбора всего списка: отозванные — потоком по одному (список в десятки мегабайт не копируется в
/// память целиком).
fn tbs_fields(tbs: ber::Tlv<'_>) -> Result<Tbs<'_>, CrlError> {
    let bad = || malformed("tbsCertList");
    let fields = tbs.children().ok_or_else(bad)?;
    let mut it = fields.iter().peekable();
    if let Some(v) = it.next_if(|t| t.tag == 0x02) {
        // Есть версия — только v2 (`1`).
        if v.value != [1] {
            return Err(unsupported("CRL version"));
        }
    }
    let signature = it.next_if(|t| t.tag == 0x30).ok_or_else(bad)?.raw;
    let issuer = Name::from_der(it.next().ok_or_else(bad)?.raw).map_err(|_| bad())?;
    let this_update = Time::from_der(it.next().ok_or_else(bad)?.raw).map_err(|_| bad())?;
    let next_update = match it.next_if(|t| matches!(t.tag, 0x17 | 0x18)) {
        Some(t) => Some(Time::from_der(t.raw).map_err(|_| bad())?),
        None => None,
    };
    let revoked = it.next_if(|t| t.tag == 0x30).map(|t| t.value);
    let extensions = match it.next_if(|t| t.tag == 0xA0) {
        Some(t) => Extensions::from_der(t.value).map_err(|_| malformed("CRL extensions"))?,
        None => Vec::new(),
    };
    if it.next().is_some() || fields.iter().any(|f| f.indefinite) {
        return Err(bad());
    }
    Ok(Tbs { signature, issuer, this_update, next_update, revoked, extensions })
}

fn is_ca(cert: &Certificate) -> Result<bool, CrlError> {
    let bc = cert.tbs_certificate().get_extension::<BasicConstraints>().map_err(|_| malformed("basic constraints"))?;
    Ok(bc.is_some_and(|(_, b)| b.ca))
}

/// Область CRL (`issuingDistributionPoint`): CRL должен покрывать этот сертификат целиком.
fn scope(idp: &IssuingDistributionPoint, cert: &Certificate) -> Result<(), CrlError> {
    if idp.indirect_crl || idp.only_some_reasons.is_some() || idp.only_contains_attribute_certs {
        return Err(unsupported("partial CRL"));
    }
    let ca = is_ca(cert)?;
    if (idp.only_contains_user_certs && ca) || (idp.only_contains_ca_certs && !ca) {
        return Err(unsupported("CRL scope"));
    }
    if let Some(name) = &idp.distribution_point {
        let points = cert.tbs_certificate().get_extension::<CrlDistributionPoints>().map_err(|_| malformed("CRL distribution points"))?;
        let covered = points.is_some_and(|(_, p)| p.0.iter().any(|dp| dp.distribution_point.as_ref() == Some(name)));
        if !covered {
            return Err(unsupported("CRL scope"));
        }
    }
    Ok(())
}

fn check_inner(cert_der: &[u8], issuer_der: &[u8], crl: &[u8], now: u64) -> Result<Checked, CrlError> {
    let cert = Certificate::from_der(cert_der).map_err(|_| malformed("certificate"))?;
    let issuer = Certificate::from_der(issuer_der).map_err(|_| malformed("issuer certificate"))?;
    let itbs = issuer.tbs_certificate();
    if cert.tbs_certificate().issuer() != itbs.subject() {
        return Err(CrlError::WrongIssuer);
    }
    if let Some((_, ku)) = itbs.get_extension::<KeyUsage>().map_err(|_| malformed("issuer key usage"))?
        && !ku.0.contains(KeyUsages::CRLSign)
    {
        return Err(CrlError::NotCrlSigner);
    }

    // CertificateList ::= SEQUENCE { tbsCertList, signatureAlgorithm, signatureValue } — подпись над исходными байтами.
    let (top, rest) = ber::read(crl).ok_or_else(|| malformed("CRL"))?;
    if !rest.is_empty() || top.tag != 0x30 || top.indefinite {
        return Err(malformed("CRL"));
    }
    let parts = top.children().ok_or_else(|| malformed("CRL"))?;
    let [tbs, alg, sig] = parts.as_slice() else { return Err(malformed("CRL")) };
    if tbs.tag != 0x30 || alg.tag != 0x30 || sig.tag != 0x03 || parts.iter().any(|p| p.indefinite) {
        return Err(malformed("CRL"));
    }
    let Some((0, signature)) = sig.value.split_first() else { return Err(malformed("CRL signature")) };
    match algs::verify_signed(alg.value, itbs.subject_public_key_info(), tbs.raw, signature) {
        Some(true) => {}
        Some(false) => return Err(CrlError::BadSignature),
        None => {
            let name = AlgorithmIdentifierOwned::from_der(alg.raw).map_or_else(|_| "signature algorithm".into(), |a| a.oid.to_string());
            return Err(unsupported(name));
        }
    }

    let fields = tbs_fields(*tbs)?;
    if fields.signature != alg.raw {
        return Err(malformed("signature algorithm mismatch"));
    }
    if &fields.issuer != itbs.subject() {
        return Err(CrlError::WrongIssuer);
    }
    for e in &fields.extensions {
        match e.extn_id {
            ids::ID_CE_DELTA_CRL_INDICATOR => return Err(unsupported("delta CRL")),
            ids::ID_CE_ISSUING_DISTRIBUTION_POINT => {
                let idp = IssuingDistributionPoint::from_der(e.extn_value.as_bytes()).map_err(|_| malformed("issuing distribution point"))?;
                scope(&idp, &cert)?;
            }
            ids::ID_CE_AUTHORITY_KEY_IDENTIFIER | ids::ID_CE_CRL_NUMBER | ids::ID_CE_FRESHEST_CRL | ids::ID_PE_AUTHORITY_INFO_ACCESS => {}
            other if e.critical => return Err(unsupported(format!("critical extension {other}"))),
            _ => {}
        }
    }

    let now = i64::try_from(now).unwrap_or(i64::MAX);
    let this_update = secs(&fields.this_update);
    if this_update > now {
        return Err(CrlError::NotYetValid);
    }
    // Без `nextUpdate` (RFC 5280: УЦ обязан его указать) не знать, до каких пор верить.
    let next_update = secs(fields.next_update.as_ref().ok_or_else(|| unsupported("no nextUpdate"))?);
    if now >= next_update {
        return Err(CrlError::Expired);
    }

    let serial = trim(cert.tbs_certificate().serial_number().as_bytes());
    let mut verdict = Verdict::NotRevoked;
    let mut list = fields.revoked.unwrap_or_default();
    while !list.is_empty() {
        let (entry, rest) = ber::read(list).ok_or_else(|| malformed("revoked certificates"))?;
        list = rest;
        if entry.tag != 0x30 || entry.indefinite {
            return Err(malformed("revoked certificates"));
        }
        let e: RevokedCert = RevokedCert::from_der(entry.raw).map_err(|_| malformed("revoked certificate"))?;
        for x in e.crl_entry_extensions.iter().flatten() {
            match x.extn_id {
                // Косвенный CRL: следующие записи — другого издателя.
                ids::ID_CE_CERTIFICATE_ISSUER => return Err(unsupported("indirect CRL")),
                ids::ID_CE_CRL_REASONS | ids::ID_CE_INVALIDITY_DATE => {}
                other if x.critical => return Err(unsupported(format!("critical entry extension {other}"))),
                _ => {}
            }
        }
        if verdict == Verdict::NotRevoked && trim(e.serial_number.as_bytes()) == serial {
            verdict = Verdict::Revoked { at: secs(&e.revocation_date) };
        }
    }
    Ok(Checked { verdict, this_update, next_update })
}

/// Адреса CRL (`http`/`https`) из расширения CRL Distribution Points сертификата `cert` (DER). Вызывает ядро — для
/// сертификатов цепочки, которую воркер уже довёл до доверенного якоря: расширение записал доверенный УЦ. Только
/// `fullName` с URI, без `cRLIssuer` и `reasons` (косвенные и частичные CRL не поддерживаются); не больше 4 адресов.
pub fn distribution_points(cert: &[u8]) -> Vec<String> {
    let Ok(c) = Certificate::from_der(cert) else { return Vec::new() };
    let Ok(Some((_, points))) = c.tbs_certificate().get_extension::<CrlDistributionPoints>() else { return Vec::new() };
    let mut out = Vec::new();
    for dp in points.0.iter().filter(|dp| dp.crl_issuer.is_none() && dp.reasons.is_none()) {
        let Some(DistributionPointName::FullName(names)) = &dp.distribution_point else { continue };
        for n in names {
            let GeneralName::UniformResourceIdentifier(uri) = n else { continue };
            let url = uri.as_str();
            let lower = url.to_ascii_lowercase();
            let web = lower.starts_with("http://") || lower.starts_with("https://");
            if web && url.len() <= MAX_URL && url.bytes().all(|b| b.is_ascii_graphic()) && !out.iter().any(|u| u == url) {
                out.push(url.to_owned());
            }
        }
    }
    out.truncate(MAX_POINTS);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oid;
    use crate::testkit::{self, Issued, Key, NOW, Spec};

    struct Ca {
        root: Issued,
        key: Key,
        good: Issued,
        revoked: Issued,
    }

    const URL: &str = "http://ca.itt.test/gost.crl";

    fn ca(set: const_oid::ObjectIdentifier, user_set: const_oid::ObjectIdentifier) -> Ca {
        let key = Key::gost("crl-root", set);
        let root = testkit::issue(&Spec::ca("ГОСТ УЦ"), &key, None);
        let user_key = Key::gost("crl-user", user_set);
        let user = |serial: u64| {
            let spec = Spec { serial, crl_url: Some(URL.into()), ..Spec::user("Сидоров", "user2@itt.test") };
            testkit::issue(&spec, &user_key, Some((&root, &key)))
        };
        Ca { good: user(0x52), revoked: user(0x51), root, key }
    }

    fn list(ca: &Ca, revoked: &[u64]) -> Vec<u8> {
        testkit::crl(&ca.root, &ca.key, revoked, NOW as i64 - 3600, NOW as i64 + 7 * 86_400)
    }

    #[test]
    fn gost_ca_revoked_and_good() {
        for (set, user_set) in [(oid::TC26_256_A, oid::TC26_512_C), (oid::CRYPTOPRO_A, oid::TC26_256_B), (oid::TC26_512_A, oid::CRYPTOPRO_XCH_A)] {
            let ca = ca(set, user_set);
            let crl = list(&ca, &[0x10, 0x51, 0x99]);
            let r = check(&ca.revoked.der, &ca.root.der, &crl, NOW).unwrap();
            assert_eq!(r.verdict, Verdict::Revoked { at: NOW as i64 - 3600 }, "{set}");
            assert_eq!((r.this_update, r.next_update), (NOW as i64 - 3600, NOW as i64 + 7 * 86_400));
            assert_eq!(check(&ca.good.der, &ca.root.der, &crl, NOW).unwrap().verdict, Verdict::NotRevoked, "{set}");
            // Пустой список (без `revokedCertificates`).
            assert_eq!(check(&ca.revoked.der, &ca.root.der, &list(&ca, &[]), NOW).unwrap().verdict, Verdict::NotRevoked);
        }
    }

    /// Самоподписанные сертификаты и пустые CRL RFC 9215 (приложение D) — независимое кодирование ГОСТ.
    #[test]
    fn rfc9215_examples() {
        let certs = testkit::pem_blocks(include_str!("../tests/fixtures/rfc9215-certs.pem"));
        let crls = testkit::pem_blocks(include_str!("../tests/fixtures/rfc9215-crls.pem"));
        // 2014-01-01 12:00 UTC: между thisUpdate и nextUpdate примеров.
        let then = 1_388_577_600;
        for (cert, crl) in certs.iter().zip(&crls) {
            let r = check(cert, cert, crl, then).unwrap();
            assert_eq!(r.verdict, Verdict::NotRevoked);
            assert_eq!(check(cert, cert, crl, NOW), Err(CrlError::Expired));
            assert_eq!(check(cert, cert, crl, then - 86_400), Err(CrlError::NotYetValid));
        }
        // CRL одного примера с сертификатом другого: тот же CN=Example, другой ключ.
        assert_eq!(check(&certs[0], &certs[0], &crls[1], then), Err(CrlError::BadSignature));
    }

    #[test]
    fn foreign_signature_expired_wrong_issuer() {
        let ca = ca(oid::TC26_256_A, oid::TC26_256_A);
        // Тот же издатель по имени, подпись — чужим ключом.
        let forged = testkit::crl(&ca.root, &Key::gost("forger", oid::TC26_256_A), &[], NOW as i64 - 3600, NOW as i64 + 86_400);
        assert_eq!(check(&ca.revoked.der, &ca.root.der, &forged, NOW), Err(CrlError::BadSignature));
        let old = testkit::crl(&ca.root, &ca.key, &[0x51], NOW as i64 - 10 * 86_400, NOW as i64 - 86_400);
        assert_eq!(check(&ca.revoked.der, &ca.root.der, &old, NOW), Err(CrlError::Expired));
        let early = testkit::crl(&ca.root, &ca.key, &[0x51], NOW as i64 + 3600, NOW as i64 + 86_400);
        assert_eq!(check(&ca.revoked.der, &ca.root.der, &early, NOW), Err(CrlError::NotYetValid));
        // Сертификат другого УЦ; CRL другого УЦ.
        let other = self::ca(oid::TC26_256_B, oid::TC26_256_B);
        let other_root = testkit::issue(&Spec::ca("Другой УЦ"), &other.key, None);
        assert_eq!(check(&ca.revoked.der, &other_root.der, &list(&ca, &[0x51]), NOW), Err(CrlError::WrongIssuer));
        let foreign = testkit::crl(&other_root, &other.key, &[0x51], NOW as i64 - 3600, NOW as i64 + 86_400);
        // Подпись проверяется раньше разбора полей: чужой CRL — «чужая подпись».
        assert_eq!(check(&ca.revoked.der, &ca.root.der, &foreign, NOW), Err(CrlError::BadSignature));
        // Сертификат, выпущенный пользователем (у издателя KeyUsage без cRLSign).
        let user_key = Key::gost("crl-user", oid::TC26_256_A);
        let sub = testkit::issue(&Spec::user("u", "u@itt.test"), &Key::gost("sub", oid::TC26_256_A), Some((&ca.good, &user_key)));
        let by_user = testkit::crl(&ca.good, &user_key, &[], NOW as i64 - 3600, NOW as i64 + 86_400);
        assert_eq!(check(&sub.der, &ca.good.der, &by_user, NOW), Err(CrlError::NotCrlSigner));
        // Сертификат издателя вместо CRL; CRL вместо сертификата.
        assert!(check(&ca.revoked.der, &ca.root.der, &ca.root.der, NOW).is_err());
        assert!(check(&list(&ca, &[]), &ca.root.der, &list(&ca, &[]), NOW).is_err());
    }

    /// RSA и ECDSA — та же таблица: проверка не только ГОСТ.
    #[test]
    fn rsa_and_ec_lists() {
        for pki in [testkit::Pki::rsa("RSA УЦ", "u", "u@itt.test"), testkit::Pki::build(Key::p256("r"), Key::p256("u"), "EC УЦ", "u", "u@itt.test")]
        {
            let crl = testkit::crl(&pki.root, &pki.root_key, &[0x1001], NOW as i64 - 60, NOW as i64 + 60);
            assert!(matches!(check(&pki.user.der, &pki.root.der, &crl, NOW).unwrap().verdict, Verdict::Revoked { .. }));
        }
    }

    #[test]
    fn truncated_flipped_random_never_panic() {
        let ca = ca(oid::TC26_256_A, oid::TC26_256_A);
        let crl = list(&ca, &[0x51, 0x77]);
        for cut in 0..crl.len() {
            assert!(check(&ca.revoked.der, &ca.root.der, &crl[..cut], NOW).is_err(), "обрезан на {cut}");
        }
        for i in 0..crl.len() {
            let mut bad = crl.clone();
            bad[i] ^= 0x01;
            // Испорченный байт — ошибка или (в незначащем бите подписи) тот же итог; «не отозван» — никогда.
            if let Ok(r) = check(&ca.revoked.der, &ca.root.der, &bad, NOW) {
                assert_eq!(r.verdict, Verdict::Revoked { at: NOW as i64 - 3600 }, "байт {i}");
            }
        }
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        for len in [0usize, 1, 2, 7, 64, 300, 4096] {
            let junk: Vec<u8> = (0..len)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    x as u8
                })
                .collect();
            assert!(check(&ca.revoked.der, &ca.root.der, &junk, NOW).is_err());
            assert!(check(&junk, &ca.root.der, &crl, NOW).is_err());
            let mut tail = vec![0x30, 0x82, (len >> 8) as u8, len as u8];
            tail.extend(&junk);
            assert!(check(&ca.revoked.der, &ca.root.der, &tail, NOW).is_err());
        }
        assert_eq!(check(&ca.revoked.der, &ca.root.der, &vec![0x30; MAX_CRL + 1], NOW), Err(CrlError::TooLarge));
    }

    #[test]
    fn points_from_the_certificate() {
        let ca = ca(oid::TC26_256_A, oid::TC26_256_A);
        assert_eq!(distribution_points(&ca.good.der), vec![URL.to_owned()]);
        assert!(distribution_points(&ca.root.der).is_empty());
        let ldap =
            testkit::issue(&Spec { crl_url: Some("ldap://ca/cn=crl".into()), ..Spec::user("u", "u@itt.test") }, &ca.key, Some((&ca.root, &ca.key)));
        assert!(distribution_points(&ldap.der).is_empty());
        assert!(distribution_points(b"junk").is_empty());
    }
}
