//! Windows: якоря из системных хранилищ, подтверждение цепочки `CertGetCertificateChain` с отзывом, поиск СКЗИ ГОСТ.
//!
//! Crypt32 разбирает здесь только сертификаты цепочки, которую воркер уже довёл до доверенного корня (решение
//! владельца №3); адреса CRL/OCSP — из сертификатов CA, не отправителя.

use std::ptr;

use postman_smime::{Anchor, Trust};
use windows_sys::Win32::Security::Cryptography::*;

use super::{Confirm, Revocation};

pub(super) use super::cng::{gost_decrypt, unwrap_cek};

const ENCODING: CERT_QUERY_ENCODING_TYPE = X509_ASN_ENCODING | PKCS_7_ASN_ENCODING;
/// Сколько ждать CRL/OCSP по сети (все запросы цепочки вместе).
const URL_TIMEOUT_MS: u32 = 10_000;
/// Типы провайдеров ГОСТ CryptoAPI: ГОСТ Р 34.10-2001 DH, 2012-256, 2012-512 (КриптоПро CSP, ViPNet CSP) [проверить].
const GOST_PROVIDER_TYPES: [u32; 3] = [80, 81, 75];
/// Хранилища групповой политики и предприятия — «корпоративные» корни (без AD они пусты, это не мешает).
const CERT_SYSTEM_STORE_LOCATION_SHIFT: u32 = 16;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Сертификаты системного хранилища `name` в расположении `location` (DER).
fn store_certs(location: u32, name: &str) -> Vec<Vec<u8>> {
    let name = wide(name);
    let flags = location | CERT_STORE_READONLY_FLAG | CERT_STORE_OPEN_EXISTING_FLAG;
    // SAFETY: имя хранилища — строка UTF-16 с нулём, живёт до конца вызова; хранилище закрывается ниже.
    let store = unsafe { CertOpenStore(CERT_STORE_PROV_SYSTEM_W, 0, 0, flags, name.as_ptr().cast()) };
    if store.is_null() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut ctx: *const CERT_CONTEXT = ptr::null();
    loop {
        // SAFETY: перебор контекстов хранилища; предыдущий контекст освобождает сама функция.
        ctx = unsafe { CertEnumCertificatesInStore(store, ctx) };
        if ctx.is_null() {
            break;
        }
        // SAFETY: контекст действителен до следующего вызова перебора; байты копируются сразу.
        let der = unsafe { std::slice::from_raw_parts((*ctx).pbCertEncoded, (*ctx).cbCertEncoded as usize) };
        out.push(der.to_vec());
    }
    // SAFETY: хранилище открыто выше.
    unsafe { CertCloseStore(store, 0) };
    out
}

fn sha256_hex(der: &[u8]) -> String {
    // Отпечаток своих байтов хранилища (не письма) — без разбора.
    use std::fmt::Write;
    let digest = sha256(der);
    digest.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02X}");
        s
    })
}

/// SHA-256 средствами Windows (BCrypt) не тянем: здесь достаточно CryptoAPI `CryptHashCertificate2`.
fn sha256(data: &[u8]) -> Vec<u8> {
    let mut out = [0u8; 32];
    let mut len = out.len() as u32;
    let alg = wide("SHA256");
    // SAFETY: буферы с длинами; алгоритм — строка UTF-16 с нулём.
    let ok = unsafe { CryptHashCertificate2(alg.as_ptr(), 0, ptr::null(), data.as_ptr(), data.len() as u32, out.as_mut_ptr(), &mut len) };
    if ok == 0 { Vec::new() } else { out[..len as usize].to_vec() }
}

pub(super) fn load_trust(corporate_extra: &[String]) -> Trust {
    let cu = CERT_SYSTEM_STORE_CURRENT_USER;
    let lm = CERT_SYSTEM_STORE_LOCAL_MACHINE;
    let gp_lm = CERT_SYSTEM_STORE_LOCAL_MACHINE_GROUP_POLICY_ID << CERT_SYSTEM_STORE_LOCATION_SHIFT;
    let ent_lm = CERT_SYSTEM_STORE_LOCAL_MACHINE_ENTERPRISE_ID << CERT_SYSTEM_STORE_LOCATION_SHIFT;
    let gp_cu = CERT_SYSTEM_STORE_CURRENT_USER_GROUP_POLICY_ID << CERT_SYSTEM_STORE_LOCATION_SHIFT;
    let corporate: Vec<Vec<u8>> = [gp_lm, ent_lm, gp_cu].iter().flat_map(|&l| store_certs(l, "Root")).collect();
    let mut roots: Vec<Anchor> = Vec::new();
    let mut size = 0usize;
    // Корпоративные — первыми: при пределе размера они не выпадают.
    let all = corporate.iter().cloned().chain(store_certs(cu, "Root")).chain(store_certs(lm, "Root")).chain(store_certs(lm, "AuthRoot"));
    for der in all {
        if roots.iter().any(|a| a.der == der) || size + der.len() > postman_smime::MAX_TRUST / 2 {
            continue;
        }
        size += der.len();
        let is_corporate = corporate.contains(&der) || (!corporate_extra.is_empty() && corporate_extra.contains(&sha256_hex(&der)));
        roots.push(Anchor { der, corporate: is_corporate });
    }
    let mut intermediates: Vec<Vec<u8>> = Vec::new();
    for der in store_certs(cu, "CA").into_iter().chain(store_certs(lm, "CA")) {
        if !intermediates.contains(&der) && size + der.len() <= postman_smime::MAX_TRUST * 7 / 8 {
            size += der.len();
            intermediates.push(der);
        }
    }
    let mut disallowed: Vec<Vec<u8>> = Vec::new();
    for der in store_certs(cu, "Disallowed").into_iter().chain(store_certs(lm, "Disallowed")) {
        if !disallowed.contains(&der) && size + der.len() <= postman_smime::MAX_TRUST {
            size += der.len();
            disallowed.push(der);
        }
    }
    Trust { roots, intermediates, disallowed }
}

/// Ресурсы одного подтверждения: освобождаются в `Drop`.
struct Guard {
    stores: Vec<HCERTSTORE>,
    leaf: *const CERT_CONTEXT,
    chain: *const CERT_CHAIN_CONTEXT,
    engine: HCERTCHAINENGINE,
}

impl Drop for Guard {
    fn drop(&mut self) {
        // SAFETY: каждое значение получено от crypt32 в этом модуле и освобождается один раз.
        unsafe {
            if !self.chain.is_null() {
                CertFreeCertificateChain(self.chain);
            }
            if !self.leaf.is_null() {
                CertFreeCertificateContext(self.leaf);
            }
            if !self.engine.is_null() {
                CertFreeCertificateChainEngine(self.engine);
            }
            for s in &self.stores {
                CertCloseStore(*s, 0);
            }
        }
    }
}

fn memory_store(g: &mut Guard) -> Option<HCERTSTORE> {
    // SAFETY: хранилище в памяти без параметров.
    let s = unsafe { CertOpenStore(CERT_STORE_PROV_MEMORY, 0, 0, 0, ptr::null()) };
    if s.is_null() {
        return None;
    }
    g.stores.push(s);
    Some(s)
}

fn add_cert(store: HCERTSTORE, der: &[u8]) -> bool {
    // SAFETY: байты DER с длиной; копию делает crypt32.
    unsafe { CertAddEncodedCertificateToStore(store, ENCODING, der.as_ptr(), der.len() as u32, CERT_STORE_ADD_ALWAYS, ptr::null_mut()) != 0 }
}

/// Итог из флагов ошибок цепочки.
fn verdict(error: u32) -> Revocation {
    if error & CERT_TRUST_IS_EXPLICIT_DISTRUST != 0 {
        Revocation::Rejected(Confirm::Distrusted)
    } else if error & CERT_TRUST_IS_REVOKED != 0 {
        Revocation::Revoked
    } else if error & CERT_TRUST_IS_NOT_SIGNATURE_VALID != 0 {
        Revocation::Rejected(Confirm::BadSignature)
    } else if error & (CERT_TRUST_IS_UNTRUSTED_ROOT | CERT_TRUST_IS_PARTIAL_CHAIN) != 0 {
        Revocation::Rejected(Confirm::Untrusted)
    } else if error & CERT_TRUST_IS_NOT_TIME_VALID != 0 {
        Revocation::Rejected(Confirm::Expired)
    } else if error & CERT_TRUST_IS_NOT_VALID_FOR_USAGE != 0 {
        Revocation::Rejected(Confirm::WrongUsage)
    } else if error & (CERT_TRUST_REVOCATION_STATUS_UNKNOWN | CERT_TRUST_IS_OFFLINE_REVOCATION) != 0 {
        Revocation::Unknown
    } else if error != 0 {
        Revocation::Rejected(Confirm::Failed)
    } else {
        Revocation::Good
    }
}

/// Подтвердить цепочку `chain` (подписант первым) в Windows с проверкой отзыва. `exclusive_root` — тесты: свой
/// движок цепочек, которому доверен только этот корень; `crls` — CRL в дополнительном хранилище.
pub(super) fn confirm_inner(chain: &[Vec<u8>], exclusive_root: Option<&[u8]>, crls: &[Vec<u8>]) -> Revocation {
    let mut g = Guard { stores: Vec::new(), leaf: ptr::null(), chain: ptr::null(), engine: ptr::null_mut() };
    let Some((leaf, rest)) = chain.split_first() else { return Revocation::NotChecked };
    let Some(extra) = memory_store(&mut g) else { return Revocation::Rejected(Confirm::Failed) };
    for der in rest {
        add_cert(extra, der);
    }
    for crl in crls {
        // SAFETY: байты CRL с длиной; копию делает crypt32.
        unsafe { CertAddEncodedCRLToStore(extra, ENCODING, crl.as_ptr(), crl.len() as u32, CERT_STORE_ADD_ALWAYS, ptr::null_mut()) };
    }
    if let Some(root) = exclusive_root {
        let Some(roots) = memory_store(&mut g) else { return Revocation::Rejected(Confirm::Failed) };
        add_cert(roots, root);
        let config = CERT_CHAIN_ENGINE_CONFIG {
            cbSize: size_of::<CERT_CHAIN_ENGINE_CONFIG>() as u32,
            hExclusiveRoot: roots,
            dwUrlRetrievalTimeout: URL_TIMEOUT_MS,
            ..Default::default()
        };
        // SAFETY: конфигурация заполнена, хранилище живо до освобождения движка (порядок в `Guard`).
        if unsafe { CertCreateCertificateChainEngine(&config, &mut g.engine) } == 0 {
            return Revocation::Rejected(Confirm::Failed);
        }
    }
    // SAFETY: байты DER с длиной; контекст освобождается в `Guard`.
    g.leaf = unsafe { CertCreateCertificateContext(ENCODING, leaf.as_ptr(), leaf.len() as u32) };
    if g.leaf.is_null() {
        return Revocation::Rejected(Confirm::Failed);
    }
    let mut email = *b"1.3.6.1.5.5.7.3.4\0";
    let mut usage = [email.as_mut_ptr()];
    let para = CERT_CHAIN_PARA {
        cbSize: size_of::<CERT_CHAIN_PARA>() as u32,
        RequestedUsage: CERT_USAGE_MATCH {
            dwType: USAGE_MATCH_TYPE_AND,
            Usage: CTL_USAGE { cUsageIdentifier: 1, rgpszUsageIdentifier: usage.as_mut_ptr() },
        },
        dwUrlRetrievalTimeout: URL_TIMEOUT_MS,
        ..Default::default()
    };
    let flags = CERT_CHAIN_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT | CERT_CHAIN_REVOCATION_ACCUMULATIVE_TIMEOUT;
    let mut ctx: *mut CERT_CHAIN_CONTEXT = ptr::null_mut();
    // SAFETY: все указатели живы до конца вызова; контекст цепочки освобождается в `Guard`.
    let ok = unsafe { CertGetCertificateChain(g.engine, g.leaf, ptr::null(), extra, &para, flags, ptr::null(), &mut ctx) };
    g.chain = ctx;
    if ok == 0 || ctx.is_null() {
        return Revocation::Rejected(Confirm::Failed);
    }
    // SAFETY: контекст цепочки получен выше.
    let error = unsafe { (*ctx).TrustStatus.dwErrorStatus };
    tracing::debug!(error = format_args!("{error:#x}"), "S/MIME chain confirmed by Windows");
    verdict(error)
}

pub(super) fn confirm(chain: &[Vec<u8>], exclusive_root: Option<&[u8]>) -> Revocation {
    confirm_inner(chain, exclusive_root, &[])
}

/// Для тестов ядра: своё хранилище корней и CRL.
pub fn confirm_with_root(chain: &[Vec<u8>], root: &[u8], crls: &[Vec<u8>]) -> Revocation {
    confirm_inner(chain, Some(root), crls)
}

/// Скачать CRL ГОСТ (12.1д) по адресу `url` из сертификата доверенной цепочки: `CryptRetrieveObjectByUrlW` без OID
/// объекта — сырые байты, Crypt32 их **не** разбирает (CRL проверяет воркер); только сеть (`CRYPT_WIRE_ONLY_RETRIEVAL`,
/// свой кэш — в ядре), без учётных данных Windows (`CRYPT_NO_AUTH_RETRIEVAL`).
pub(super) fn fetch_crl(url: &str, timeout: std::time::Duration) -> Result<Vec<u8>, String> {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err("only http and https".into());
    }
    let wurl = wide(url);
    let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX).max(1);
    let mut obj: *mut core::ffi::c_void = ptr::null_mut();
    // SAFETY: строка — с нулём на конце; `obj` получает CRYPT_BLOB_ARRAY, освобождается CryptMemFree ниже.
    let ok = unsafe {
        CryptRetrieveObjectByUrlW(
            wurl.as_ptr(),
            ptr::null(),
            CRYPT_WIRE_ONLY_RETRIEVAL | CRYPT_NO_AUTH_RETRIEVAL | CRYPT_DONT_CACHE_RESULT,
            ms,
            &mut obj,
            0,
            ptr::null(),
            ptr::null(),
            ptr::null_mut(),
        )
    };
    if ok == 0 || obj.is_null() {
        // SAFETY: код ошибки потока сразу после вызова.
        let e = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        return Err(format!("CryptRetrieveObjectByUrlW: {e:#x}"));
    }
    let blobs = obj as *const CRYPT_BLOB_ARRAY;
    // SAFETY: успешный вызов без OID объекта отдаёт CRYPT_BLOB_ARRAY; берём первый блоб и копируем байты.
    let out = unsafe {
        let a = &*blobs;
        if a.cBlob == 0 || a.rgBlob.is_null() {
            None
        } else {
            let b = &*a.rgBlob;
            let n = b.cbData as usize;
            (n <= postman_smime::crl::MAX_CRL && !b.pbData.is_null()).then(|| std::slice::from_raw_parts(b.pbData, n).to_vec())
        }
    };
    // SAFETY: память выделила CryptRetrieveObjectByUrlW.
    unsafe { CryptMemFree(obj) };
    out.ok_or_else(|| "empty or too large CRL".into())
}

/// Провайдер CryptoAPI с ГОСТ Р 34.10-2012 (КриптоПро CSP, ViPNet CSP) — имя первого найденного.
pub(super) fn gost_provider() -> Option<String> {
    for index in 0..256u32 {
        let mut kind = 0u32;
        let mut bytes = 0u32;
        // SAFETY: первый вызов узнаёт длину имени (байты), второй — заполняет буфер этой длины.
        if unsafe { CryptEnumProvidersW(index, ptr::null(), 0, &mut kind, ptr::null_mut(), &mut bytes) } == 0 {
            break;
        }
        let mut name = vec![0u16; (bytes as usize).div_ceil(2).max(1)];
        // SAFETY: буфер на `bytes` байтов.
        if unsafe { CryptEnumProvidersW(index, ptr::null(), 0, &mut kind, name.as_mut_ptr(), &mut bytes) } == 0 {
            continue;
        }
        if GOST_PROVIDER_TYPES.contains(&kind) {
            let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
            return Some(String::from_utf16_lossy(&name[..end]));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use postman_smime::testkit::{Key, Pki, Spec, issue};

    /// Хранилища читаются: корни Windows есть всегда.
    #[test]
    fn system_roots_are_loaded() {
        let t = load_trust(&[]);
        assert!(t.roots.len() > 5, "{}", t.roots.len());
        let size: usize =
            t.roots.iter().map(|a| a.der.len()).sum::<usize>() + t.intermediates.iter().chain(&t.disallowed).map(Vec::len).sum::<usize>();
        assert!(size <= postman_smime::MAX_TRUST);
        assert_eq!(sha256(b"abc").len(), 32);
    }

    /// Движок с `hExclusiveRoot`: доверенный тестовый корень — цепочка принята, отзыв не проверить (нет CDP);
    /// чужой корень — «не доверен».
    #[test]
    fn exclusive_root_engine() {
        let pki = Pki::rsa("Postman Test Root", "Иванов", "user1@itt.test");
        let chain = vec![pki.user.der.clone(), pki.root.der.clone()];
        let r = confirm_with_root(&chain, &pki.root.der, &[]);
        assert_eq!(r, Revocation::Unknown, "в тестовом сертификате нет CDP — отзыв не проверен");
        let other = issue(&Spec::ca("Other Root"), &Key::rsa('c'), None);
        assert_eq!(confirm_with_root(&chain, &other.der, &[]), Revocation::Rejected(Confirm::Untrusted));
        assert_eq!(verdict(0), Revocation::Good);
        assert_eq!(verdict(CERT_TRUST_IS_REVOKED | CERT_TRUST_REVOCATION_STATUS_UNKNOWN), Revocation::Revoked);
    }

    /// Отзыв по CRL: CRL корня в дополнительном хранилище — отозванный сертификат «отозван», другой — «не отозван».
    #[test]
    fn revoked_by_crl() {
        let root_key = Key::rsa('a');
        let user_key = Key::rsa('b');
        let root = issue(&Spec::ca("Postman Test CRL Root"), &root_key, None);
        let url = Some("http://127.0.0.1:9/postman-test.crl".to_string());
        let revoked = issue(&Spec { serial: 0x51, crl_url: url.clone(), ..Spec::user("Отозван", "r@itt.test") }, &user_key, Some((&root, &root_key)));
        let good = issue(&Spec { serial: 0x52, crl_url: url, ..Spec::user("Действует", "g@itt.test") }, &user_key, Some((&root, &root_key)));
        let now = jiff::Timestamp::now().as_second();
        let crl = postman_smime::testkit::crl(&root, &root_key, &[0x51], now - 3600, now + 7 * 86_400);
        let chain = |c: &postman_smime::testkit::Issued| vec![c.der.clone(), root.der.clone()];
        assert_eq!(confirm_with_root(&chain(&revoked), &root.der, std::slice::from_ref(&crl)), Revocation::Revoked);
        assert_eq!(confirm_with_root(&chain(&good), &root.der, &[crl]), Revocation::Good);
    }

    /// Загрузка CRL: только http/https; закрытый порт на этой машине — ошибка с тайм-аутом, не зависание.
    #[test]
    fn crl_download_fails_fast() {
        assert!(fetch_crl("file:///C:/Windows/win.ini", std::time::Duration::from_secs(1)).is_err());
        assert!(fetch_crl("ldap://ca/cn=crl", std::time::Duration::from_secs(1)).is_err());
        let t = std::time::Instant::now();
        assert!(fetch_crl("http://127.0.0.1:9/postman-test.crl", std::time::Duration::from_millis(500)).is_err());
        assert!(t.elapsed() < std::time::Duration::from_secs(5), "{:?}", t.elapsed());
    }

    /// Поиск СКЗИ не падает; на стенде без КриптоПро — `None` [проверить с КриптоПро CSP].
    #[test]
    fn gost_provider_detection_runs() {
        let p = gost_provider();
        eprintln!("GOST provider: {p:?}");
    }
}
