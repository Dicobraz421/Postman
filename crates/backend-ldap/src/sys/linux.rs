//! Linux (LIN-05): контроллер домена — записи SRV через резолвер glibc (`res_query`, учитывает `/etc/resolv.conf` и
//! systemd-resolved), домен — область билета Kerberos из кэша по умолчанию (`kinit`, SSSD) через GSS-API MIT Kerberos
//! (`libgssapi_krb5`). Сам вход Kerberos — `ldap3` (`sasl_gssapi_bind`, с привязкой к каналу TLS). Каждый блок
//! `unsafe` — с обоснованием.

use std::ffi::{CString, c_char, c_int, c_void};
use std::ptr::null_mut;

type OmUint32 = u32;
type Name = *mut c_void;
type Cred = *mut c_void;

// <gssapi/gssapi.h> MIT Kerberos: обычное выравнивание C.
#[repr(C)]
struct Buffer {
    length: usize,
    value: *mut c_void,
}

#[repr(C)]
struct Oid {
    length: OmUint32,
    elements: *const c_void,
}

#[repr(C)]
struct OidSet {
    count: usize,
    elements: *const Oid,
}

/// Механизм Kerberos 5 (1.2.840.113554.1.2.2), DER без тега и длины.
static MECH_KRB5: [u8; 9] = *b"\x2a\x86\x48\x86\xf7\x12\x01\x02\x02";
const GSS_C_INITIATE: c_int = 1;

/// Ошибка GSS-API — в старших 16 битах кода.
fn gss_error(major: OmUint32) -> bool {
    major & 0xFFFF_0000 != 0
}

#[link(name = "gssapi_krb5")]
unsafe extern "C" {
    fn gss_acquire_cred(
        minor: *mut OmUint32,
        name: Name,
        time_req: OmUint32,
        mechs: *const OidSet,
        usage: c_int,
        cred: *mut Cred,
        actual_mechs: *mut *mut OidSet,
        time_rec: *mut OmUint32,
    ) -> OmUint32;
    fn gss_inquire_cred(
        minor: *mut OmUint32,
        cred: Cred,
        name: *mut Name,
        lifetime: *mut OmUint32,
        usage: *mut c_int,
        mechs: *mut *mut OidSet,
    ) -> OmUint32;
    fn gss_release_cred(minor: *mut OmUint32, cred: *mut Cred) -> OmUint32;
    fn gss_display_name(minor: *mut OmUint32, name: Name, out: *mut Buffer, name_type: *mut *const Oid) -> OmUint32;
    fn gss_release_name(minor: *mut OmUint32, name: *mut Name) -> OmUint32;
    fn gss_release_buffer(minor: *mut OmUint32, buffer: *mut Buffer) -> OmUint32;
}

// <resolv.h>: в glibc ≥ 2.34 — в libc.
unsafe extern "C" {
    fn res_query(name: *const c_char, class: c_int, kind: c_int, answer: *mut u8, len: c_int) -> c_int;
}

/// Имя пользователя из билета Kerberos по умолчанию (`user@CORP.EXAMPLE`); нет билета — `None`. Без запросов пароля:
/// только чтение кэша билетов.
fn kerberos_principal() -> Option<String> {
    let krb5 = Oid { length: MECH_KRB5.len() as OmUint32, elements: MECH_KRB5.as_ptr().cast() };
    let mechs = OidSet { count: 1, elements: &krb5 };
    let (mut minor, mut cred) = (0, null_mut());
    // SAFETY: имя — по умолчанию (null); OID и набор механизмов живы на время вызова; `cred` освобождается ниже.
    let major = unsafe { gss_acquire_cred(&mut minor, null_mut(), 0, &mechs, GSS_C_INITIATE, &mut cred, null_mut(), null_mut()) };
    if gss_error(major) || cred.is_null() {
        tracing::debug!(major, minor, "ldap: no Kerberos credentials");
        return None;
    }
    let mut name = null_mut();
    // SAFETY: `cred` выдан выше; `name` освобождается ниже; остальные выходы не нужны (null).
    let major = unsafe { gss_inquire_cred(&mut minor, cred, &mut name, null_mut(), null_mut(), null_mut()) };
    // SAFETY: `cred` выдан выше и дальше не используется.
    unsafe { gss_release_cred(&mut 0, &mut cred) };
    if gss_error(major) || name.is_null() {
        tracing::debug!(major, minor, "ldap: no Kerberos principal");
        return None;
    }
    let mut out = Buffer { length: 0, value: null_mut() };
    // SAFETY: `name` выдан выше; буфер и имя освобождаются ниже ровно один раз.
    let major = unsafe { gss_display_name(&mut minor, name, &mut out, null_mut()) };
    let text = if gss_error(major) || out.value.is_null() {
        None
    } else {
        // SAFETY: буфер заполнил GSS-API: `value` указывает на `length` байт, живущих до `gss_release_buffer`.
        Some(unsafe { std::slice::from_raw_parts(out.value.cast::<u8>(), out.length) }.to_vec())
    };
    // SAFETY: буфер и имя выданы GSS-API выше и дальше не используются.
    unsafe {
        gss_release_buffer(&mut 0, &mut out);
        gss_release_name(&mut 0, &mut name);
    }
    String::from_utf8(text?).ok()
}

const DNS_CLASS_IN: c_int = 1;
const DNS_TYPE_SRV: c_int = 33;
/// Предел ответа DNS (TCP): больше не бывает.
const DNS_MAX: usize = 65535;

/// Данные записей SRV имени `name` (RDATA); блокирует поток на время запроса (сроки — у резолвера).
fn srv_records(name: &str) -> Vec<Vec<u8>> {
    let Ok(cname) = CString::new(name) else { return Vec::new() };
    let mut answer = vec![0u8; DNS_MAX];
    // SAFETY: имя — строка C с нулём; буфер ответа — `DNS_MAX` байт, резолвер пишет не больше `len`.
    let n = unsafe { res_query(cname.as_ptr(), DNS_CLASS_IN, DNS_TYPE_SRV, answer.as_mut_ptr(), DNS_MAX as c_int) };
    let Ok(n) = usize::try_from(n) else {
        tracing::debug!("ldap: DNS SRV query failed");
        return Vec::new();
    };
    answer.truncate(n.min(DNS_MAX));
    crate::srv::answers(&answer, DNS_TYPE_SRV as u16)
}

/// Контроллер домена: домен — область билета Kerberos пользователя, сервер — по записям SRV
/// `_ldap._tcp.dc._msdcs.<домен>`. Нет билета или записей — `None` (компьютер не в домене).
pub(crate) fn domain_controller() -> Option<String> {
    super::bounded(dc_lookup)
}

fn dc_lookup() -> Option<String> {
    let domain = crate::srv::realm_domain(&kerberos_principal()?)?;
    let records = srv_records(&crate::srv::query_name(&domain));
    let dc = crate::srv::choose(records.iter().filter_map(|r| crate::srv::parse(r)));
    if dc.is_none() {
        tracing::debug!("ldap: no domain controller in DNS for the Kerberos realm");
    }
    dc
}

#[cfg(test)]
mod tests {
    /// Без билета (пустой кэш) — `None`, без паники и без запросов пароля.
    #[test]
    fn no_ticket_no_controller() {
        let dir = std::env::temp_dir().join(format!("postman-ldap-krb5-{}", std::process::id()));
        // SAFETY (окружение): переменные читает только libkrb5 в этом тесте; nextest запускает тест отдельным процессом.
        unsafe {
            std::env::set_var("KRB5CCNAME", format!("FILE:{}", dir.join("none").display()));
            std::env::set_var("KRB5_CONFIG", dir.join("none.conf"));
        }
        assert_eq!(super::kerberos_principal(), None);
        assert_eq!(super::domain_controller(), None);
    }
}
