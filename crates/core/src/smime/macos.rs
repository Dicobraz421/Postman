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
/// `errSecInvalidSignature` и `errSecVerifyFailed` [проверить на macOS: какой из них отдаёт `SecTrust`].
const ERR_SEC_INVALID_SIGNATURE: i64 = -67688;
const ERR_SEC_VERIFY_FAILED: i64 = -67808;
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

/// Настройка доверия сертификата `cert` в домене `domain` для S/MIME (PAR-AUDIT №27): записи без политики (на все
/// политики) и с политикой «SMIME»; TLS и прочие — мимо. Пустой список настроек — «доверять как корню» (правило
/// `SecTrustSettingsCopyTrustSettings`). `None` — для S/MIME настройки нет.
fn smime_setting(cert: &SecCertificate, domain: Domain) -> Option<TrustSettingsForCertificate> {
    use core_foundation::array::{CFArray, CFArrayRef};
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;
    let mut array: CFArrayRef = std::ptr::null();
    // SAFETY: сертификат — живой объект; массив по правилу Copy — владеем (обёртка ниже).
    let st =
        unsafe { security_framework_sys::trust_settings::SecTrustSettingsCopyTrustSettings(cert.as_concrete_TypeRef(), domain.into(), &mut array) };
    if st != 0 || array.is_null() {
        return None;
    }
    // SAFETY: массив словарей получен выше по правилу Copy.
    let settings = unsafe { CFArray::<CFDictionary>::wrap_under_create_rule(array) };
    if settings.is_empty() {
        return Some(TrustSettingsForCertificate::TrustRoot);
    }
    let name_key = CFString::from_static_string("kSecTrustSettingsPolicyName");
    let result_key = CFString::from_static_string("kSecTrustSettingsResult");
    for item in settings.iter() {
        let name = item.find(name_key.as_CFTypeRef().cast()).map(|n| {
            // SAFETY: значение ключа имени политики — CFString (документация Trust Settings).
            unsafe { CFString::wrap_under_get_rule((*n).cast()) }.to_string()
        });
        // [проверить на macOS] имя политики S/MIME в настройках доверия — «SMIME».
        if name.as_deref().is_some_and(|n| n != "SMIME") {
            continue;
        }
        let result = item
            .find(result_key.as_CFTypeRef().cast())
            .and_then(|v| {
                // SAFETY: значение ключа итога — CFNumber.
                unsafe { CFNumber::wrap_under_get_rule((*v).cast()) }.to_i64()
            })
            .unwrap_or(i64::from(security_framework_sys::trust_settings::kSecTrustSettingsResultTrustRoot));
        // `kSecTrustSettingsResult`: 1 — корень, 2 — как корень, 3 — запрет; 0 и 4 (нет/не задан) — дальше.
        match result {
            1 => return Some(TrustSettingsForCertificate::TrustRoot),
            2 => return Some(TrustSettingsForCertificate::TrustAsRoot),
            3 => return Some(TrustSettingsForCertificate::Deny),
            _ => continue,
        }
    }
    None
}

/// Сертификаты с настройками доверия в домене `domain` и итог настройки для S/MIME.
fn trust_settings(domain: Domain) -> Vec<(Vec<u8>, Option<TrustSettingsForCertificate>)> {
    let ts = TrustSettings::new(domain);
    let Ok(iter) = ts.iter() else { return Vec::new() };
    iter.map(|c| {
        let r = smime_setting(&c, domain);
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
    // Промежуточные (PAR-AUDIT №26): хранилища CA, как в Windows, у macOS нет — сертификаты Связки ключей без адреса
    // (CA), не корни, не запрещённые и не свои удостоверения; годность для цепочки решает воркер. Предел размера —
    // как у Windows.
    let own: Vec<Vec<u8>> = super::keychain::personal(None);
    let mut intermediates: Vec<Vec<u8>> = Vec::new();
    for der in super::keychain::all_certificates() {
        if roots.iter().any(|a| a.der == der) || disallowed.contains(&der) || own.contains(&der) || intermediates.contains(&der) {
            continue;
        }
        if postman_smime::sign::own_certificate(&der).is_some_and(|(info, _, _)| !info.emails.is_empty()) {
            continue;
        }
        if size + der.len() <= postman_smime::MAX_TRUST * 7 / 8 {
            size += der.len();
            intermediates.push(der);
        }
    }
    Trust { roots, intermediates, disallowed }
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

/// Одна оценка цепочки: `Ok` — доверена; `Err` — код ошибки `SecTrust` и признак «запрещён». `network` — можно ли
/// спрашивать OCSP/CRL (без сети — только кэш ОС).
fn evaluate(
    chain: &[SecCertificate],
    exclusive_root: Option<&SecCertificate>,
    revocation: RevocationPolicy,
    network: bool,
) -> Result<(), (i64, bool)> {
    let Ok(rev) = SecPolicy::create_revocation(revocation) else { return Err((0, false)) };
    let Ok(mut trust) = SecTrust::create_with_certificates(chain, &[smime_policy(), rev]) else { return Err((0, false)) };
    if let Some(root) = exclusive_root
        && (trust.set_anchor_certificates(std::slice::from_ref(root)).is_err() || trust.set_trust_anchor_certificates_only(true).is_err())
    {
        return Err((0, false));
    }
    let _ = trust.set_network_fetch_allowed(network);
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

/// Итог первой оценки (ошибка `code`) — общим правилом [`super::chain_verdict`]: «ещё не действует» — своя причина
/// `NotYetValid` (PAR-02b), неверная подпись — `BadSignature`, как у Windows (PAR-AUDIT №30).
fn verdict(code: i64, denied: bool) -> Revocation {
    let mut c = super::ChainCheck::default();
    if denied || code == ERR_SEC_TRUST_SETTING_DENY {
        c.distrusted = true;
    } else {
        match code {
            ERR_SEC_CERTIFICATE_REVOKED => c.revoked = true,
            ERR_SEC_CERTIFICATE_EXPIRED => c.expired = true,
            ERR_SEC_CERTIFICATE_NOT_VALID_YET => c.not_yet_valid = true,
            ERR_SEC_INVALID_EXTENDED_KEY_USAGE => c.wrong_usage = true,
            ERR_SEC_NOT_TRUSTED | ERR_SEC_CREATE_CHAIN_FAILED => c.untrusted = true,
            ERR_SEC_INVALID_SIGNATURE | ERR_SEC_VERIFY_FAILED => c.bad_signature = true,
            _ => c.other = true,
        }
    }
    super::chain_verdict(c)
}

/// Отзыв с обязательным ответом по сети — в отдельном потоке, не дольше общего предела [`super::crl::BUDGET`]
/// (тот же, что у Windows `dwUrlRetrievalTimeout` и Linux; PAR-AUDIT №29). Вышло время — «не удалось проверить».
fn strict_revocation(chain: &[Vec<u8>], exclusive_root: Option<&[u8]>) -> super::ChainCheck {
    let chain = chain.to_vec();
    let root = exclusive_root.map(<[u8]>::to_vec);
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().name("smime-revocation".into()).spawn(move || {
        let certs: Vec<SecCertificate> = chain.iter().filter_map(|d| SecCertificate::from_der(d).ok()).collect();
        let root = root.and_then(|r| SecCertificate::from_der(&r).ok());
        let strict = RevocationPolicy::USE_ANY_METHOD_AVAILABLE | RevocationPolicy::REQUIRE_POSITIVE_RESPONSE;
        let _ = tx.send(evaluate(&certs, root.as_ref(), strict, true));
    });
    if spawned.is_err() {
        return super::ChainCheck { revocation_unknown: true, ..Default::default() };
    }
    match rx.recv_timeout(super::crl::BUDGET) {
        Ok(Ok(())) => super::ChainCheck::default(),
        Ok(Err((ERR_SEC_CERTIFICATE_REVOKED, _))) => super::ChainCheck { revoked: true, ..Default::default() },
        Ok(Err((code, _))) => {
            tracing::debug!(code, "smime: revocation status unknown");
            super::ChainCheck { revocation_unknown: true, ..Default::default() }
        }
        Err(_) => {
            tracing::debug!("smime: revocation check timed out");
            super::ChainCheck { revocation_unknown: true, ..Default::default() }
        }
    }
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
    // Сначала цепочка без сети (отзыв — только из кэша ОС): отозван, не доверен, срок, назначение — быстро и без
    // ожидания. Затем — отзыв с обязательным ответом по сети в пределах общего времени: не получен — «не проверен»
    // (как `REVOCATION_STATUS_UNKNOWN` Windows).
    if let Err((code, denied)) = evaluate(&certs, root.as_ref(), RevocationPolicy::USE_ANY_METHOD_AVAILABLE, false) {
        tracing::debug!(code, denied, "smime: chain rejected by the OS");
        return verdict(code, denied);
    }
    super::chain_verdict(strict_revocation(chain, exclusive_root))
}

pub(super) fn confirm(chain: &[Vec<u8>], exclusive_root: Option<&[u8]>) -> Revocation {
    confirm_inner(chain, exclusive_root)
}

/// Отзыв ГОСТ своими силами (12.1д) на macOS не нужен: ГОСТ отложен.
pub(super) fn fetch_crl(_url: &str, _timeout: std::time::Duration) -> Result<Vec<u8>, String> {
    Err("CRL download is not available (GOST deferred)".into())
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
        // Таблица кодов — те же причины, что у Windows (PAR-AUDIT №30).
        for (code, want) in [
            (ERR_SEC_CERTIFICATE_EXPIRED, Confirm::Expired),
            (ERR_SEC_CERTIFICATE_NOT_VALID_YET, Confirm::NotYetValid),
            (ERR_SEC_INVALID_EXTENDED_KEY_USAGE, Confirm::WrongUsage),
            (ERR_SEC_NOT_TRUSTED, Confirm::Untrusted),
            (ERR_SEC_INVALID_SIGNATURE, Confirm::BadSignature),
            (ERR_SEC_VERIFY_FAILED, Confirm::BadSignature),
            (-1, Confirm::Failed),
        ] {
            assert_eq!(verdict(code, false), Revocation::Rejected(want), "{code}");
        }
    }
}
