//! macOS (MAC-05): якоря из хранилища доверия системы, подтверждение цепочки `SecTrust` с отзывом; ключи — Связка
//! ключей ([`super::keychain`]). Аналог `windows.rs`.
//!
//! Security.framework разбирает здесь только сертификаты цепочки, которую воркер уже довёл до доверенного корня
//! (решение владельца №3); адреса CRL/OCSP — из сертификатов CA, не отправителя. Якоря: корни системы
//! (`SecTrustCopyAnchorCertificates`) и корни с настройками доверия пользователя и администратора; «корпоративные» —
//! настройки доверия домена администратора (их ставит профиль MDM или администратор Mac — аналог групповой политики
//! Windows) и отпечатки `POSTMAN_CORPORATE_CA`. Запрещённые («Никогда не доверять») — в `disallowed`. ГОСТ и СКЗИ на
//! macOS не делаются (`docs/decisions/2026-10-27-smime-gost-skzi-deferred.md`).

use core_foundation::base::TCFType;
use postman_smime::{Anchor, Trust};
use security_framework::certificate::SecCertificate;
use security_framework::policy::{RevocationPolicy, SecPolicy};
use security_framework::trust::SecTrust;
use security_framework::trust_settings::{Domain, TrustSettings, TrustSettingsForCertificate};

use super::{Confirm, Revocation};

pub(super) use super::keychain::unwrap_cek;

const ERR_SEC_CERTIFICATE_EXPIRED: i64 = -67818;
const ERR_SEC_CERTIFICATE_NOT_VALID_YET: i64 = -67819;
const ERR_SEC_CERTIFICATE_REVOKED: i64 = -67820;
const ERR_SEC_INVALID_EXTENDED_KEY_USAGE: i64 = -67609;
const ERR_SEC_TRUST_SETTING_DENY: i64 = -67654;
const ERR_SEC_NOT_TRUSTED: i64 = -67843;
const ERR_SEC_CREATE_CHAIN_FAILED: i64 = -25318;
/// `kSecTrustResultDeny`: сертификат цепочки — «Никогда не доверять».
const TRUST_RESULT_DENY: u32 = 3;

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecTrustGetTrustResult(trust: security_framework_sys::trust::SecTrustRef, result: *mut u32) -> i32;
    fn SecPolicyCreateWithProperties(
        policy_identifier: core_foundation::base::CFTypeRef,
        properties: core_foundation::dictionary::CFDictionaryRef,
    ) -> security_framework_sys::base::SecPolicyRef;
}

unsafe extern "C" {
    /// CommonCrypto (libSystem).
    fn CC_SHA256(data: *const core::ffi::c_void, len: u32, md: *mut u8) -> *mut u8;
}

/// SHA-256 средствами ОС (CommonCrypto): отпечаток своих байтов хранилища, без разбора.
fn sha256_hex(der: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = [0u8; 32];
    let Ok(len) = u32::try_from(der.len()) else { return String::new() };
    // SAFETY: вход — срез с длиной, выход — буфер на 32 байта (CC_SHA256_DIGEST_LENGTH).
    unsafe { CC_SHA256(der.as_ptr().cast(), len, out.as_mut_ptr()) };
    out.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02X}");
        s
    })
}

/// Сертификаты с настройками доверия в домене `domain` и итог настройки.
fn trust_settings(domain: Domain) -> Vec<(Vec<u8>, Option<TrustSettingsForCertificate>)> {
    let ts = TrustSettings::new(domain);
    let Ok(iter) = ts.iter() else { return Vec::new() };
    iter.map(|c| {
        let r = ts.tls_trust_settings_for_certificate(&c).ok().flatten();
        (c.to_der(), r)
    })
    .collect()
}

pub(super) fn load_trust(corporate_extra: &[String]) -> Trust {
    let system: Vec<Vec<u8>> = SecTrust::copy_anchor_certificates().map(|v| v.iter().map(SecCertificate::to_der).collect()).unwrap_or_default();
    let admin = trust_settings(Domain::Admin);
    let user = trust_settings(Domain::User);
    let trusted = |r: &Option<TrustSettingsForCertificate>| {
        // Без настройки для политики (`None`) — сертификат в списке домена: доверие «по умолчанию» (корень).
        matches!(r, None | Some(TrustSettingsForCertificate::TrustRoot | TrustSettingsForCertificate::TrustAsRoot))
    };
    let corporate: Vec<Vec<u8>> = admin.iter().filter(|(_, r)| trusted(r)).map(|(d, _)| d.clone()).collect();
    let mut roots: Vec<Anchor> = Vec::new();
    let mut size = 0usize;
    let all = corporate.iter().cloned().chain(user.iter().filter(|(_, r)| trusted(r)).map(|(d, _)| d.clone())).chain(system);
    let denied: Vec<Vec<u8>> =
        admin.iter().chain(&user).filter(|(_, r)| matches!(r, Some(TrustSettingsForCertificate::Deny))).map(|(d, _)| d.clone()).collect();
    for der in all {
        if denied.contains(&der) || roots.iter().any(|a| a.der == der) || size + der.len() > postman_smime::MAX_TRUST / 2 {
            continue;
        }
        size += der.len();
        let is_corporate = corporate.contains(&der) || (!corporate_extra.is_empty() && corporate_extra.contains(&sha256_hex(&der)));
        roots.push(Anchor { der, corporate: is_corporate });
    }
    let mut disallowed: Vec<Vec<u8>> = Vec::new();
    for der in denied {
        if !disallowed.contains(&der) && size + der.len() <= postman_smime::MAX_TRUST {
            size += der.len();
            disallowed.push(der);
        }
    }
    // Промежуточные: отдельного хранилища CA, как в Windows, у macOS нет — берутся из письма (воркер).
    Trust { roots, intermediates: Vec::new(), disallowed }
}

/// Политика S/MIME (`kSecPolicyAppleSMIME`: EKU «Защита почты», KeyUsage); без адреса — адрес проверил воркер.
fn smime_policy() -> SecPolicy {
    // SAFETY: идентификатор политики — константа Security.framework; свойств нет (NULL).
    let p = unsafe { SecPolicyCreateWithProperties(security_framework_sys::certificate::kSecPolicyAppleSMIME.cast(), std::ptr::null()) };
    if p.is_null() {
        return SecPolicy::create_x509();
    }
    // SAFETY: политика создана выше (правило Create — владеем).
    unsafe { SecPolicy::wrap_under_create_rule(p) }
}

/// Одна оценка цепочки: `Ok` — доверена; `Err` — код ошибки `SecTrust` и признак «запрещён».
fn evaluate(chain: &[SecCertificate], exclusive_root: Option<&SecCertificate>, revocation: RevocationPolicy) -> Result<(), (i64, bool)> {
    let Ok(rev) = SecPolicy::create_revocation(revocation) else { return Err((0, false)) };
    let Ok(mut trust) = SecTrust::create_with_certificates(chain, &[smime_policy(), rev]) else { return Err((0, false)) };
    if let Some(root) = exclusive_root
        && (trust.set_anchor_certificates(std::slice::from_ref(root)).is_err() || trust.set_trust_anchor_certificates_only(true).is_err())
    {
        return Err((0, false));
    }
    let _ = trust.set_network_fetch_allowed(true);
    match trust.evaluate_with_error() {
        Ok(()) => Ok(()),
        Err(e) => {
            let mut result = 0u32;
            // SAFETY: оценка выполнена выше; результат — число.
            let ok = unsafe { SecTrustGetTrustResult(trust.as_concrete_TypeRef(), &mut result) } == 0;
            Err((e.code() as i64, ok && result == TRUST_RESULT_DENY))
        }
    }
}

/// Итог первой оценки (ошибка `code`) — общим правилом [`super::chain_verdict`].
fn verdict(code: i64, denied: bool) -> Revocation {
    let mut c = super::ChainCheck::default();
    if denied || code == ERR_SEC_TRUST_SETTING_DENY {
        c.distrusted = true;
    } else {
        match code {
            ERR_SEC_CERTIFICATE_REVOKED => c.revoked = true,
            ERR_SEC_CERTIFICATE_EXPIRED | ERR_SEC_CERTIFICATE_NOT_VALID_YET => c.expired = true,
            ERR_SEC_INVALID_EXTENDED_KEY_USAGE => c.wrong_usage = true,
            ERR_SEC_NOT_TRUSTED | ERR_SEC_CREATE_CHAIN_FAILED => c.untrusted = true,
            _ => c.other = true,
        }
    }
    super::chain_verdict(c)
}

fn confirm_inner(chain: &[Vec<u8>], exclusive_root: Option<&[u8]>) -> Revocation {
    if chain.is_empty() {
        return Revocation::NotChecked;
    }
    let Ok(certs) = chain.iter().map(|d| SecCertificate::from_der(d)).collect::<Result<Vec<_>, _>>() else {
        return Revocation::Rejected(Confirm::Failed);
    };
    let root = match exclusive_root.map(SecCertificate::from_der) {
        None => None,
        Some(Ok(r)) => Some(r),
        Some(Err(_)) => return Revocation::Rejected(Confirm::Failed),
    };
    // Сначала «по возможности» (недоступный OCSP/CRL не ошибка): отозван, не доверен, срок, назначение. Затем — с
    // обязательным ответом: не получен — отзыв «не проверен» (как `REVOCATION_STATUS_UNKNOWN` Windows).
    if let Err((code, denied)) = evaluate(&certs, root.as_ref(), RevocationPolicy::USE_ANY_METHOD_AVAILABLE) {
        tracing::debug!(code, denied, "S/MIME chain rejected by macOS");
        return verdict(code, denied);
    }
    let strict = RevocationPolicy::USE_ANY_METHOD_AVAILABLE | RevocationPolicy::REQUIRE_POSITIVE_RESPONSE;
    let c = match evaluate(&certs, root.as_ref(), strict) {
        Ok(()) => super::ChainCheck::default(),
        Err((ERR_SEC_CERTIFICATE_REVOKED, _)) => super::ChainCheck { revoked: true, ..Default::default() },
        Err((code, _)) => {
            tracing::debug!(code, "S/MIME revocation status unknown (macOS)");
            super::ChainCheck { revocation_unknown: true, ..Default::default() }
        }
    };
    super::chain_verdict(c)
}

pub(super) fn confirm(chain: &[Vec<u8>], exclusive_root: Option<&[u8]>) -> Revocation {
    confirm_inner(chain, exclusive_root)
}

/// Отзыв ГОСТ своими силами (12.1д) на macOS не нужен: ГОСТ отложен.
pub(super) fn fetch_crl(_url: &str, _timeout: std::time::Duration) -> Result<Vec<u8>, String> {
    Err("CRL download is not used on macOS (GOST deferred)".into())
}

/// СКЗИ на macOS нет (ГОСТ отложен).
pub(super) fn gost_provider() -> Option<String> {
    None
}

pub(super) fn gost_decrypt(
    _env: &postman_smime::envelope::Envelope,
    _prompt: super::keys::Prompt,
) -> Result<super::skzi::Plain, super::keys::KeyError> {
    Err(super::keys::KeyError::NeedSkzi)
}

#[cfg(test)]
mod tests {
    use super::*;
    use postman_smime::testkit::{Key, Pki, Spec, issue};

    #[test]
    fn system_roots_are_loaded() {
        let t = load_trust(&[]);
        assert!(t.roots.len() > 5, "{}", t.roots.len());
        let size: usize =
            t.roots.iter().map(|a| a.der.len()).sum::<usize>() + t.intermediates.iter().chain(&t.disallowed).map(Vec::len).sum::<usize>();
        assert!(size <= postman_smime::MAX_TRUST);
        assert_eq!(sha256_hex(b"abc"), "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD");
    }

    /// Свой корень (исключительный якорь): цепочка подтверждена (в тестовом сертификате нет CDP/OCSP — отзыв «не
    /// проверен» или «не отозван», по правилам macOS); чужой корень — «не доверен».
    #[test]
    fn exclusive_root() {
        let pki = Pki::rsa("Postman Test Root", "Иванов", "user1@example.test");
        let chain = vec![pki.user.der.clone(), pki.root.der.clone()];
        let r = confirm(&chain, Some(&pki.root.der));
        assert!(matches!(r, Revocation::Unknown | Revocation::Good), "{r:?}");
        let other = issue(&Spec::ca("Other Root"), &Key::rsa('c'), None);
        assert_eq!(confirm(&chain, Some(&other.der)), Revocation::Rejected(Confirm::Untrusted));
        assert_eq!(confirm(&[], None), Revocation::NotChecked);
        assert_eq!(verdict(ERR_SEC_CERTIFICATE_REVOKED, false), Revocation::Revoked);
        assert_eq!(verdict(ERR_SEC_NOT_TRUSTED, true), Revocation::Rejected(Confirm::Distrusted));
    }
}
