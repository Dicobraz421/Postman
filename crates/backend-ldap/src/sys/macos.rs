//! macOS (MAC-05): Kerberos — публичный GSS-API GSS.framework (RFC 2744), контроллер домена — записи SRV через
//! mDNSResponder (`dns_sd`, libSystem: учитывает DNS VPN по доменам, в отличие от `/etc/resolv.conf`). Каждый блок
//! `unsafe` — с обоснованием.

use std::cell::{Cell, RefCell};
use std::ffi::{CString, c_char, c_int, c_void};
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use crate::kerberos::{Gss, GssStep};

type OmUint32 = u32;
type Name = *mut c_void;
type Ctx = *mut c_void;
type Cred = *mut c_void;

// <GSS/gssapi.h>: на Apple структуры GSS-API упакованы по 2 байта (`#pragma pack(push, 2)`).
#[repr(C, packed(2))]
struct Buffer {
    length: usize,
    value: *mut c_void,
}

impl Buffer {
    const EMPTY: Buffer = Buffer { length: 0, value: null_mut() };

    /// Вход GSS-API: функции его только читают.
    fn of(data: &[u8]) -> Buffer {
        Buffer { length: data.len(), value: data.as_ptr().cast_mut().cast() }
    }
}

#[repr(C, packed(2))]
struct Oid {
    length: OmUint32,
    elements: *const c_void,
}

#[repr(C, packed(2))]
struct OidSet {
    count: usize,
    elements: *const Oid,
}

#[repr(C, packed(2))]
struct ChannelBindings {
    initiator_addrtype: OmUint32,
    initiator_address: Buffer,
    acceptor_addrtype: OmUint32,
    acceptor_address: Buffer,
    application_data: Buffer,
}

/// OID в `static`: GSS-API может хранить указатель на него, пока жив контекст.
struct StaticOid(Oid);
// SAFETY: указатель — на неизменяемые байты в `static`; GSS-API OID только читает.
unsafe impl Sync for StaticOid {}

/// Механизм Kerberos 5 (1.2.840.113554.1.2.2).
static MECH_KRB5: StaticOid = StaticOid(Oid { length: 9, elements: b"\x2a\x86\x48\x86\xf7\x12\x01\x02\x02".as_ptr().cast() });
/// Имя службы `служба@узел` (1.2.840.113554.1.2.1.4, `GSS_C_NT_HOSTBASED_SERVICE`).
static NT_HOSTBASED_SERVICE: StaticOid = StaticOid(Oid { length: 10, elements: b"\x2a\x86\x48\x86\xf7\x12\x01\x02\x01\x04".as_ptr().cast() });

const GSS_C_MUTUAL_FLAG: OmUint32 = 2;
const GSS_C_CONF_FLAG: OmUint32 = 16;
const GSS_C_INTEG_FLAG: OmUint32 = 32;
const GSS_S_CONTINUE_NEEDED: OmUint32 = 1;
const GSS_C_GSS_CODE: c_int = 1;
const GSS_C_MECH_CODE: c_int = 2;
const GSS_C_INITIATE: c_int = 1;
const GSS_C_QOP_DEFAULT: OmUint32 = 0;

/// Ошибка GSS-API — в старших 16 битах кода (ошибка вызова и подпрограммы).
fn gss_error(major: OmUint32) -> bool {
    major & 0xFFFF_0000 != 0
}

#[link(name = "GSS", kind = "framework")]
unsafe extern "C" {
    fn gss_import_name(minor: *mut OmUint32, name: *const Buffer, name_type: *const Oid, out: *mut Name) -> OmUint32;
    fn gss_release_name(minor: *mut OmUint32, name: *mut Name) -> OmUint32;
    fn gss_display_name(minor: *mut OmUint32, name: Name, out: *mut Buffer, name_type: *mut *const Oid) -> OmUint32;
    fn gss_init_sec_context(
        minor: *mut OmUint32,
        cred: Cred,
        ctx: *mut Ctx,
        target: Name,
        mech: *const Oid,
        flags: OmUint32,
        time_req: OmUint32,
        bindings: *const ChannelBindings,
        input: *const Buffer,
        actual_mech: *mut *const Oid,
        output: *mut Buffer,
        ret_flags: *mut OmUint32,
        time_rec: *mut OmUint32,
    ) -> OmUint32;
    fn gss_delete_sec_context(minor: *mut OmUint32, ctx: *mut Ctx, output: *mut Buffer) -> OmUint32;
    fn gss_wrap(
        minor: *mut OmUint32,
        ctx: Ctx,
        conf_req: c_int,
        qop: OmUint32,
        input: *const Buffer,
        conf_state: *mut c_int,
        output: *mut Buffer,
    ) -> OmUint32;
    fn gss_unwrap(minor: *mut OmUint32, ctx: Ctx, input: *const Buffer, output: *mut Buffer, conf_state: *mut c_int, qop: *mut OmUint32) -> OmUint32;
    fn gss_wrap_size_limit(
        minor: *mut OmUint32,
        ctx: Ctx,
        conf_req: c_int,
        qop: OmUint32,
        max_output: OmUint32,
        max_input: *mut OmUint32,
    ) -> OmUint32;
    fn gss_release_buffer(minor: *mut OmUint32, buffer: *mut Buffer) -> OmUint32;
    fn gss_display_status(
        minor: *mut OmUint32,
        status: OmUint32,
        status_type: c_int,
        mech: *const Oid,
        message_context: *mut OmUint32,
        out: *mut Buffer,
    ) -> OmUint32;
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
}

/// Байты буфера, выданного GSS-API, — и освободить его.
fn take(buf: &mut Buffer) -> Vec<u8> {
    let (len, ptr) = (buf.length, buf.value);
    let out = if ptr.is_null() || len == 0 {
        Vec::new()
    } else {
        // SAFETY: буфер заполнил GSS-API: `value` указывает на `length` байт, живущих до `gss_release_buffer`.
        unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) }.to_vec()
    };
    let mut minor = 0;
    // SAFETY: буфер выдал GSS-API (или он пуст), после этого он не используется.
    unsafe { gss_release_buffer(&mut minor, buf) };
    out
}

/// Текст ошибки GSS-API (английский, для журнала и `DirectoryError`).
fn status(major: OmUint32, minor: OmUint32) -> String {
    let mut parts = Vec::new();
    for (code, kind, mech) in [(major, GSS_C_GSS_CODE, null()), (minor, GSS_C_MECH_CODE, &raw const MECH_KRB5.0)] {
        if code == 0 {
            continue;
        }
        let mut context = 0;
        for _ in 0..8 {
            let (mut m, mut out) = (0, Buffer::EMPTY);
            // SAFETY: все указатели — на живые локальные значения; `out` освобождает `take`.
            let rc = unsafe { gss_display_status(&mut m, code, kind, mech, &mut context, &mut out) };
            let text = take(&mut out);
            if gss_error(rc) {
                break;
            }
            parts.push(String::from_utf8_lossy(&text).into_owned());
            if context == 0 {
                break;
            }
        }
    }
    format!("GSS-API error {major:#x}/{minor:#x}: {}", parts.join("; "))
}

/// Контекст Kerberos для `ldap@<узел>`: билет пользователя (вход в домен, `kinit` или единый вход Kerberos).
pub(crate) struct Kerberos {
    ctx: Ctx,
    target: Name,
    /// Привязка к каналу TLS (`tls-server-end-point:…`).
    binding: Option<Vec<u8>>,
}

// SAFETY: контекст и имя — объекты GSS.framework без привязки к потоку; одновременных вызовов нет: методы берут
// `&mut self`, мост держит контекст под `Mutex`.
unsafe impl Send for Kerberos {}

impl Kerberos {
    pub(crate) fn new(host: &str, binding: Option<&[u8]>) -> Result<Kerberos, String> {
        let service = format!("ldap@{host}");
        let mut minor = 0;
        let mut target = null_mut();
        // SAFETY: буфер имени указывает на живую строку; тип — OID в `static`; `target` — выходной указатель.
        let major = unsafe { gss_import_name(&mut minor, &Buffer::of(service.as_bytes()), &raw const NT_HOSTBASED_SERVICE.0, &mut target) };
        if gss_error(major) {
            return Err(status(major, minor));
        }
        Ok(Kerberos { ctx: null_mut(), target, binding: binding.map(<[u8]>::to_vec) })
    }
}

impl Drop for Kerberos {
    fn drop(&mut self) {
        let mut minor = 0;
        // SAFETY: контекст и имя выдал GSS-API (или они null), после `drop` не используются.
        unsafe {
            if !self.ctx.is_null() {
                gss_delete_sec_context(&mut minor, &mut self.ctx, null_mut());
            }
            if !self.target.is_null() {
                gss_release_name(&mut minor, &mut self.target);
            }
        }
    }
}

impl Gss for Kerberos {
    fn step(&mut self, input: Option<&[u8]>) -> Result<GssStep, String> {
        let input = input.map(Buffer::of);
        let bindings = self.binding.as_deref().map(|b| ChannelBindings {
            initiator_addrtype: 0,
            initiator_address: Buffer::EMPTY,
            acceptor_addrtype: 0,
            acceptor_address: Buffer::EMPTY,
            application_data: Buffer::of(b),
        });
        let (mut minor, mut out, mut ret_flags) = (0, Buffer::EMPTY, 0);
        // SAFETY: учётные данные — по умолчанию (null); контекст — свой, GSS-API создаёт его на первом шаге; имя,
        // OID, привязка и входной токен живы на время вызова; `out` освобождает `take`.
        let major = unsafe {
            gss_init_sec_context(
                &mut minor,
                null_mut(),
                &mut self.ctx,
                self.target,
                &raw const MECH_KRB5.0,
                GSS_C_MUTUAL_FLAG | GSS_C_INTEG_FLAG | GSS_C_CONF_FLAG,
                0,
                bindings.as_ref().map_or(null(), std::ptr::from_ref),
                input.as_ref().map_or(null(), std::ptr::from_ref),
                null_mut(),
                &mut out,
                &mut ret_flags,
                null_mut(),
            )
        };
        let token = take(&mut out);
        if gss_error(major) {
            return Err(status(major, minor));
        }
        Ok(GssStep { token, done: major & GSS_S_CONTINUE_NEEDED == 0 })
    }

    fn wrap(&mut self, encrypt: bool, data: &[u8]) -> Result<Vec<u8>, String> {
        let (mut minor, mut conf, mut out) = (0, 0, Buffer::EMPTY);
        // SAFETY: контекст установлен (`bind` зовёт после готовности); вход жив на время вызова; `out` — `take`.
        let major = unsafe { gss_wrap(&mut minor, self.ctx, c_int::from(encrypt), GSS_C_QOP_DEFAULT, &Buffer::of(data), &mut conf, &mut out) };
        let token = take(&mut out);
        if gss_error(major) {
            return Err(status(major, minor));
        }
        if encrypt && conf == 0 {
            return Err("GSS-API wrap without confidentiality".into());
        }
        Ok(token)
    }

    fn unwrap(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let (mut minor, mut conf, mut qop, mut out) = (0, 0, 0, Buffer::EMPTY);
        // SAFETY: как у `wrap`.
        let major = unsafe { gss_unwrap(&mut minor, self.ctx, &Buffer::of(data), &mut out, &mut conf, &mut qop) };
        let plain = take(&mut out);
        if gss_error(major) {
            return Err(status(major, minor));
        }
        Ok(plain)
    }

    fn wrap_limit(&mut self, max_token: u32) -> Result<u32, String> {
        let (mut minor, mut max) = (0, 0);
        // SAFETY: контекст установлен; выход — локальная переменная.
        let major = unsafe { gss_wrap_size_limit(&mut minor, self.ctx, 1, GSS_C_QOP_DEFAULT, max_token, &mut max) };
        if gss_error(major) {
            return Err(status(major, minor));
        }
        Ok(max)
    }
}

/// Имя пользователя из билета Kerberos по умолчанию (`user@CORP.EXAMPLE`); нет билета — `None`. Без запросов пароля:
/// только чтение кэша билетов.
fn kerberos_principal() -> Option<String> {
    let mechs = OidSet { count: 1, elements: &raw const MECH_KRB5.0 };
    let (mut minor, mut cred) = (0, null_mut());
    // SAFETY: имя — по умолчанию (null); набор механизмов жив на время вызова; `cred` освобождается ниже.
    let major = unsafe { gss_acquire_cred(&mut minor, null_mut(), 0, &mechs, GSS_C_INITIATE, &mut cred, null_mut(), null_mut()) };
    if gss_error(major) || cred.is_null() {
        tracing::info!("no Kerberos credentials: {}", status(major, minor));
        return None;
    }
    let mut name = null_mut();
    // SAFETY: `cred` выдан выше; `name` освобождается ниже; остальные выходы не нужны (null).
    let major = unsafe { gss_inquire_cred(&mut minor, cred, &mut name, null_mut(), null_mut(), null_mut()) };
    // SAFETY: `cred` выдан выше и дальше не используется.
    unsafe { gss_release_cred(&mut 0, &mut cred) };
    if gss_error(major) || name.is_null() {
        tracing::info!("no Kerberos principal: {}", status(major, minor));
        return None;
    }
    let mut out = Buffer::EMPTY;
    // SAFETY: `name` выдан выше; `out` освобождает `take`, `name` — ниже.
    let major = unsafe { gss_display_name(&mut minor, name, &mut out, null_mut()) };
    let text = take(&mut out);
    // SAFETY: `name` выдан выше и дальше не используется.
    unsafe { gss_release_name(&mut 0, &mut name) };
    if gss_error(major) {
        tracing::info!("no Kerberos principal name: {}", status(major, minor));
        return None;
    }
    String::from_utf8(text).ok()
}

// <dns_sd.h> — libSystem (libsystem_dnssd), линкуется всегда; `poll` — <poll.h>.
type DnsRef = *mut c_void;
type DnsReply = unsafe extern "C" fn(DnsRef, u32, u32, i32, *const c_char, u16, u16, u16, *const c_void, u32, *mut c_void);

unsafe extern "C" {
    fn DNSServiceQueryRecord(
        sd: *mut DnsRef,
        flags: u32,
        iface: u32,
        name: *const c_char,
        rrtype: u16,
        rrclass: u16,
        reply: DnsReply,
        context: *mut c_void,
    ) -> i32;
    fn DNSServiceRefSockFD(sd: DnsRef) -> c_int;
    fn DNSServiceProcessResult(sd: DnsRef) -> i32;
    fn DNSServiceRefDeallocate(sd: DnsRef);
    fn poll(fds: *mut PollFd, nfds: u32, timeout: c_int) -> c_int;
}

#[repr(C)]
struct PollFd {
    fd: c_int,
    events: i16,
    revents: i16,
}

const POLLIN: i16 = 1;
const DNS_FLAGS_MORE_COMING: u32 = 0x1;
const DNS_FLAGS_ADD: u32 = 0x2;
/// Отрицательные ответы (нет имени) — сразу, а не по истечении ожидания.
const DNS_FLAGS_RETURN_INTERMEDIATES: u32 = 0x1000;
const DNS_TYPE_SRV: u16 = 33;
const DNS_CLASS_IN: u16 = 1;
/// Ждать ответа DNS — не дольше.
const DNS_TIMEOUT: Duration = Duration::from_secs(5);

/// Ответы на запрос SRV: обработчик и цикл видят их только по общей ссылке.
struct Answers {
    records: RefCell<Vec<Vec<u8>>>,
    done: Cell<bool>,
}

/// Ответ mDNSResponder; зовётся из `DNSServiceProcessResult` в том же потоке. Без паник (вызов из C).
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn on_record(
    _sd: DnsRef,
    flags: u32,
    _iface: u32,
    error: i32,
    _name: *const c_char,
    rrtype: u16,
    _class: u16,
    rdlen: u16,
    rdata: *const c_void,
    _ttl: u32,
    context: *mut c_void,
) {
    // SAFETY: `context` — `&Answers` из `srv_records`, жив весь цикл обработки.
    let answers = unsafe { &*context.cast_const().cast::<Answers>() };
    if error != 0 {
        answers.done.set(true);
        return;
    }
    if rrtype == DNS_TYPE_SRV && flags & DNS_FLAGS_ADD != 0 && !rdata.is_null() {
        // SAFETY: mDNSResponder передаёт `rdlen` байт данных записи, живущих до конца вызова.
        let record = unsafe { std::slice::from_raw_parts(rdata.cast::<u8>(), usize::from(rdlen)) }.to_vec();
        if let Ok(mut records) = answers.records.try_borrow_mut() {
            records.push(record);
        }
    }
    let any = answers.records.try_borrow().is_ok_and(|r| !r.is_empty());
    if any && flags & DNS_FLAGS_MORE_COMING == 0 {
        answers.done.set(true);
    }
}

/// Данные записей SRV имени `name` (RDATA); блокирует поток не дольше `DNS_TIMEOUT`.
fn srv_records(name: &str) -> Vec<Vec<u8>> {
    let Ok(cname) = CString::new(name) else { return Vec::new() };
    let answers = Answers { records: RefCell::new(Vec::new()), done: Cell::new(false) };
    let mut sd: DnsRef = null_mut();
    // SAFETY: имя — строка C с нулём; `answers` переживает `sd` (освобождается ниже, до выхода из функции).
    let rc = unsafe {
        DNSServiceQueryRecord(
            &mut sd,
            DNS_FLAGS_RETURN_INTERMEDIATES,
            0,
            cname.as_ptr(),
            DNS_TYPE_SRV,
            DNS_CLASS_IN,
            on_record,
            std::ptr::from_ref(&answers).cast_mut().cast(),
        )
    };
    if rc != 0 || sd.is_null() {
        tracing::info!("DNS SRV query failed: {rc}");
        return Vec::new();
    }
    let deadline = Instant::now() + DNS_TIMEOUT;
    while !answers.done.get() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            tracing::info!("DNS SRV query timed out");
            break;
        }
        // SAFETY: `sd` выдан выше и ещё не освобождён.
        let mut fd = PollFd { fd: unsafe { DNSServiceRefSockFD(sd) }, events: POLLIN, revents: 0 };
        // SAFETY: один `PollFd` на стеке.
        let n = unsafe { poll(&mut fd, 1, c_int::try_from(left.as_millis()).unwrap_or(c_int::MAX)) };
        if n <= 0 {
            break;
        }
        // SAFETY: `sd` жив; ответ приходит в `on_record` с `answers` в этом же потоке.
        if unsafe { DNSServiceProcessResult(sd) } != 0 {
            break;
        }
    }
    // SAFETY: `sd` выдан выше, после этого не используется; `on_record` больше не зовётся.
    unsafe { DNSServiceRefDeallocate(sd) };
    answers.records.into_inner()
}

/// Контроллер домена: домен — область билета Kerberos пользователя, сервер — по записям SRV
/// `_ldap._tcp.dc._msdcs.<домен>`. Нет билета или записей — `None` (компьютер не в домене).
pub(crate) fn domain_controller() -> Option<String> {
    let domain = crate::srv::realm_domain(&kerberos_principal()?)?;
    let records = srv_records(&crate::srv::query_name(&domain));
    let dc = crate::srv::choose(records.iter().filter_map(|r| crate::srv::parse(r)));
    if dc.is_none() {
        tracing::info!("no domain controller in DNS for the Kerberos realm");
    }
    dc
}
