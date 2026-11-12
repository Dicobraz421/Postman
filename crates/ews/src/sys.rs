//! Вход HTTP без пароля средствами ОС: Negotiate (Kerberos, иначе NTLM) или NTLM учётной записи текущего входа.
//! Единственное место `unsafe` в крейте; у каждого блока — почему он безопасен. Вход с паролем — NTLM на чистом Rust
//! (`ntlm`), одинаково на всех ОС.
//!
//! - Windows — SSPI: `AcquireCredentialsHandleW` и `InitializeSecurityContextW`, учётная запись Windows текущего
//!   входа (Kerberos на машине в домене).
//! - macOS — GSS.framework (MAC-05): `gss_init_sec_context` механизма SPNEGO или NTLM, учётные данные системы по
//!   умолчанию (билет Kerberos домена AD, Kerberos SSO, `kinit`).
//! - Linux — тот же GSS-API (RFC 2744) из MIT Kerberos `libgssapi_krb5` (LIN-05): SPNEGO с билетом из кэша по
//!   умолчанию (`kinit`, SSSD); NTLM — только если в системе есть механизм NTLM для GSS (gss-ntlmssp), иначе отказ.

use crate::auth::Scheme;
use crate::ntlm::Ntlm;
use crate::{Auth, Result};

/// Контекст безопасности одного рукопожатия.
pub(crate) struct Context(Inner);

enum Inner {
    /// Без пароля — средствами ОС.
    Os(imp::Context),
    /// С паролем — NTLM на чистом Rust.
    Ntlm(Ntlm),
}

impl Context {
    /// Учётные данные и контекст для `target` (SPN `HTTP/<host>`); `binding` — данные привязки к каналу TLS.
    pub(crate) fn new(scheme: Scheme, auth: &Auth, target: &str, binding: Option<&[u8]>) -> Result<Context> {
        match auth {
            Auth::Windows => imp::Context::new(scheme, target, binding).map(|c| Context(Inner::Os(c))),
            Auth::Password { user, password } => Ntlm::new(user, password, target, binding).map(|c| Context(Inner::Ntlm(c))),
        }
    }

    /// Следующий шаг: токен сервера (на первом шаге — `None`) → токен для сервера (может быть пустым на
    /// последнем шаге).
    pub(crate) fn step(&mut self, input: Option<&[u8]>) -> Result<Vec<u8>> {
        match &mut self.0 {
            Inner::Os(c) => c.step(input),
            Inner::Ntlm(c) => c.step(input),
        }
    }

    /// Логин на вторую попытку после отказа сервера (только вход с паролем, [`Ntlm::alternative`]).
    pub(crate) fn alternative(&self) -> Option<String> {
        match &self.0 {
            Inner::Os(_) => None,
            Inner::Ntlm(c) => c.alternative(),
        }
    }

    /// Рукопожатие завершено с нашей стороны.
    pub(crate) fn done(&self) -> bool {
        match &self.0 {
            Inner::Os(c) => c.done,
            Inner::Ntlm(c) => c.done,
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::ptr;

    use windows::Win32::Foundation::{SEC_E_OK, SEC_I_COMPLETE_AND_CONTINUE, SEC_I_COMPLETE_NEEDED, SEC_I_CONTINUE_NEEDED};
    use windows::Win32::Security::Authentication::Identity::{
        AcquireCredentialsHandleW, CompleteAuthToken, DeleteSecurityContext, FreeContextBuffer, FreeCredentialsHandle, ISC_REQ_ALLOCATE_MEMORY,
        InitializeSecurityContextW, SECBUFFER_CHANNEL_BINDINGS, SECBUFFER_TOKEN, SECBUFFER_VERSION, SECPKG_CRED_OUTBOUND, SECURITY_NATIVE_DREP,
        SecBuffer, SecBufferDesc,
    };
    use windows::Win32::Security::Credentials::SecHandle;
    use windows::core::PCWSTR;

    use crate::auth::{Scheme, sec_channel_bindings};
    use crate::{Error, Result};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub(super) struct Context {
        cred: SecHandle,
        ctx: SecHandle,
        has_ctx: bool,
        target: Vec<u16>,
        /// `SEC_CHANNEL_BINDINGS` + данные; хранится в u64 — выравнивание структуры для SSPI.
        binding: Option<(Vec<u64>, u32)>,
        pub(super) done: bool,
    }

    impl Context {
        pub(super) fn new(scheme: Scheme, target: &str, binding: Option<&[u8]>) -> Result<Context> {
            let package = wide(match scheme {
                Scheme::Negotiate => "Negotiate",
                Scheme::Ntlm => "NTLM",
            });
            let mut cred = SecHandle::default();
            // SAFETY: `package` — строка UTF-16 с нулём, живёт до конца вызова; `cred` — место под дескриптор;
            // остальные указатели — нулевые (текущий вход Windows, без ключей и срока).
            let status = unsafe {
                AcquireCredentialsHandleW(PCWSTR::null(), PCWSTR(package.as_ptr()), SECPKG_CRED_OUTBOUND, None, None, None, None, &mut cred, None)
            };
            status.map_err(|e| Error::Sspi { call: "AcquireCredentialsHandleW", code: e.code().0 as u32 })?;
            let binding = binding.map(|app| {
                let bytes = sec_channel_bindings(app);
                let mut words = vec![0u64; bytes.len().div_ceil(8)];
                for (i, b) in bytes.iter().enumerate() {
                    words[i / 8] |= (*b as u64) << (8 * (i % 8));
                }
                (words, bytes.len() as u32)
            });
            Ok(Context { cred, ctx: SecHandle::default(), has_ctx: false, target: wide(target), binding, done: false })
        }

        pub(super) fn step(&mut self, input: Option<&[u8]>) -> Result<Vec<u8>> {
            let mut token_in = input.map(<[u8]>::to_vec);
            let mut in_bufs: Vec<SecBuffer> = Vec::with_capacity(2);
            if let Some(t) = token_in.as_mut() {
                in_bufs.push(SecBuffer { cbBuffer: t.len() as u32, BufferType: SECBUFFER_TOKEN, pvBuffer: t.as_mut_ptr().cast() });
            }
            if let Some((words, len)) = self.binding.as_mut() {
                in_bufs.push(SecBuffer { cbBuffer: *len, BufferType: SECBUFFER_CHANNEL_BINDINGS, pvBuffer: words.as_mut_ptr().cast() });
            }
            let in_desc = SecBufferDesc { ulVersion: SECBUFFER_VERSION, cBuffers: in_bufs.len() as u32, pBuffers: in_bufs.as_mut_ptr() };
            let mut out_buf = SecBuffer { cbBuffer: 0, BufferType: SECBUFFER_TOKEN, pvBuffer: ptr::null_mut() };
            let mut out_desc = SecBufferDesc { ulVersion: SECBUFFER_VERSION, cBuffers: 1, pBuffers: &mut out_buf };
            let mut attrs = 0u32;
            let ctx: *mut SecHandle = &mut self.ctx;
            // SAFETY: `cred` получен `AcquireCredentialsHandleW` и не освобождён; `ctx` — наш дескриптор (на первом
            // шаге не передаётся, SSPI его заполняет; дальше — тот же, это разрешено: phNewContext = phContext).
            // `target` — UTF-16 с нулём. Входные буферы (`token_in`, `binding`) и описатели живут до конца вызова.
            // Выход — память SSPI (`ISC_REQ_ALLOCATE_MEMORY`), её освобождаем ниже.
            let status = unsafe {
                InitializeSecurityContextW(
                    Some(&self.cred),
                    self.has_ctx.then_some(ctx as *const SecHandle),
                    Some(self.target.as_ptr()),
                    ISC_REQ_ALLOCATE_MEMORY,
                    0,
                    SECURITY_NATIVE_DREP,
                    (!in_bufs.is_empty()).then_some(&in_desc as *const SecBufferDesc),
                    0,
                    Some(ctx),
                    Some(&mut out_desc),
                    &mut attrs,
                    None,
                )
            };
            if status.is_ok() {
                self.has_ctx = true;
            }
            if status == SEC_I_COMPLETE_NEEDED || status == SEC_I_COMPLETE_AND_CONTINUE {
                // SAFETY: контекст только что создан этим вызовом, `out_desc` указывает на его выходной токен.
                unsafe { CompleteAuthToken(ctx, &out_desc) }.map_err(|e| Error::Sspi { call: "CompleteAuthToken", code: e.code().0 as u32 })?;
            }
            let token = if out_buf.pvBuffer.is_null() || out_buf.cbBuffer == 0 {
                Vec::new()
            } else {
                // SAFETY: SSPI выделил `cbBuffer` байт по `pvBuffer`; читаем их до освобождения.
                unsafe { std::slice::from_raw_parts(out_buf.pvBuffer as *const u8, out_buf.cbBuffer as usize) }.to_vec()
            };
            if !out_buf.pvBuffer.is_null() {
                // SAFETY: буфер выделил SSPI (`ISC_REQ_ALLOCATE_MEMORY`), освобождаем один раз.
                let _ = unsafe { FreeContextBuffer(out_buf.pvBuffer) };
            }
            if status == SEC_E_OK || status == SEC_I_COMPLETE_NEEDED {
                self.done = true;
            } else if status != SEC_I_CONTINUE_NEEDED && status != SEC_I_COMPLETE_AND_CONTINUE {
                return Err(Error::Sspi { call: "InitializeSecurityContextW", code: status.0 as u32 });
            }
            Ok(token)
        }
    }

    impl Drop for Context {
        fn drop(&mut self) {
            if self.has_ctx {
                // SAFETY: контекст создан `InitializeSecurityContextW` и удаляется один раз.
                let _ = unsafe { DeleteSecurityContext(&self.ctx) };
            }
            // SAFETY: дескриптор получен `AcquireCredentialsHandleW` (иначе `Context` не создан) и освобождается
            // один раз.
            let _ = unsafe { FreeCredentialsHandle(&self.cred) };
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// NTLM без сети от входа Windows: первый токен — NEGOTIATE_MESSAGE (`NTLMSSP\0`, тип 1); мусор вместо
        /// вызова сервера — ошибка SSPI, не паника.
        #[test]
        fn ntlm_first_token_offline() {
            let mut ctx = Context::new(Scheme::Ntlm, "HTTP/mail.example.test", Some(b"tls-server-end-point:x")).unwrap();
            let token = ctx.step(None).unwrap();
            assert!(token.starts_with(b"NTLMSSP\0\x01\0\0\0"), "{:?}", &token[..token.len().min(12)]);
            assert!(!ctx.done);
            assert!(ctx.step(Some(b"garbage")).is_err());
        }

        /// Linux без билета Kerberos (пустой кэш): отказ GSS ошибкой входа, не паника и не зависание.
        #[cfg(target_os = "linux")]
        #[test]
        fn negotiate_without_ticket_fails() {
            let dir = std::env::temp_dir().join(format!("postman-ews-krb5-{}", std::process::id()));
            // SAFETY (окружение): переменные читает только libkrb5 в этом тесте; nextest запускает тест отдельным
            // процессом.
            unsafe {
                std::env::set_var("KRB5CCNAME", format!("FILE:{}", dir.join("none").display()));
                std::env::set_var("KRB5_CONFIG", dir.join("none.conf"));
            }
            let mut ctx = Context::new(Scheme::Negotiate, "HTTP/mail.example.test", Some(b"tls-server-end-point:x")).unwrap();
            assert!(matches!(ctx.step(None), Err(crate::Error::Sspi { call: "gss_init_sec_context", .. })));
            assert!(!ctx.done);
        }
    }
}

/// macOS (MAC-05): GSS.framework — системный GSS-API (Heimdal) с механизмами SPNEGO, Kerberos и NTLM. Учётные данные
/// — системы по умолчанию (`GSS_C_NO_CREDENTIAL`): билет Kerberos входа в домен AD, расширения Kerberos SSO или
/// `kinit` (хранилище GSSCred), иначе учётные данные NTLM оттуда же.
///
/// Привязка к каналу TLS — `gss_channel_bindings_struct` с теми же данными приложения, что у SSPI
/// (`tls-server-end-point:` + хэш сертификата, `auth::channel_binding`): адреса пустые, тип адреса 0
/// (`GSS_C_AF_UNSPEC`), как у `SEC_CHANNEL_BINDINGS` Windows, — хэш привязки у сервера тот же.
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod imp {
    use std::ffi::c_void;
    use std::ptr;

    use crate::auth::Scheme;
    use crate::{Error, Result};

    /// `gss_OID_desc`. Заголовки GSS.framework упаковывают структуры по 2 байта только на ppc, i386 и x86_64; на
    /// arm64 и в MIT Kerberos (Linux) — обычное выравнивание C.
    #[cfg_attr(all(target_os = "macos", target_arch = "x86_64"), repr(C, packed(2)))]
    #[cfg_attr(not(all(target_os = "macos", target_arch = "x86_64")), repr(C))]
    struct OidDesc {
        length: u32,
        elements: *const c_void,
    }

    #[cfg_attr(all(target_os = "macos", target_arch = "x86_64"), repr(C, packed(2)))]
    #[cfg_attr(not(all(target_os = "macos", target_arch = "x86_64")), repr(C))]
    struct BufferDesc {
        length: usize,
        value: *mut c_void,
    }

    impl BufferDesc {
        const EMPTY: BufferDesc = BufferDesc { length: 0, value: ptr::null_mut() };

        /// Вход для GSS: GSS его не меняет (`const gss_buffer_t`).
        fn of(b: &[u8]) -> BufferDesc {
            BufferDesc { length: b.len(), value: b.as_ptr().cast_mut().cast() }
        }
    }

    /// `gss_channel_bindings_struct`.
    #[cfg_attr(all(target_os = "macos", target_arch = "x86_64"), repr(C, packed(2)))]
    #[cfg_attr(not(all(target_os = "macos", target_arch = "x86_64")), repr(C))]
    struct ChannelBindings {
        initiator_addrtype: u32,
        initiator_address: BufferDesc,
        acceptor_addrtype: u32,
        acceptor_address: BufferDesc,
        application_data: BufferDesc,
    }

    type Name = *mut c_void;
    type Ctx = *mut c_void;
    type Cred = *mut c_void;

    const GSS_S_CONTINUE_NEEDED: u32 = 1;
    /// Ошибка вызова или механизма (`GSS_ERROR`); младшие 16 бит — дополнительные сведения, не ошибка.
    const GSS_ERROR_MASK: u32 = 0xFFFF_0000;
    const GSS_C_GSS_CODE: i32 = 1;
    const GSS_C_MECH_CODE: i32 = 2;

    // OID в DER (без тега и длины).
    /// SPNEGO, 1.3.6.1.5.5.2.
    const SPNEGO: &[u8] = &[0x2B, 0x06, 0x01, 0x05, 0x05, 0x02];
    /// NTLM (`GSS_NTLM_MECHANISM`), 1.3.6.1.4.1.311.2.2.10.
    const NTLM: &[u8] = &[0x2B, 0x06, 0x01, 0x04, 0x01, 0x82, 0x37, 0x02, 0x02, 0x0A];
    /// `GSS_C_NT_HOSTBASED_SERVICE`, 1.2.840.113554.1.2.1.4: `service@host`.
    const NT_HOSTBASED_SERVICE: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x12, 0x01, 0x02, 0x01, 0x04];
    /// `GSS_C_NT_USER_NAME`, 1.2.840.113554.1.2.1.1: `user@DOMAIN`.
    #[cfg(all(target_os = "macos", any(test, feature = "testkit")))]
    const NT_USER_NAME: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x12, 0x01, 0x02, 0x01, 0x01];

    fn oid(der: &'static [u8]) -> OidDesc {
        OidDesc { length: der.len() as u32, elements: der.as_ptr().cast() }
    }

    #[cfg_attr(target_os = "macos", link(name = "GSS", kind = "framework"))]
    #[cfg_attr(target_os = "linux", link(name = "gssapi_krb5"))]
    unsafe extern "C" {
        fn gss_import_name(minor: *mut u32, input: *const BufferDesc, name_type: *const OidDesc, output: *mut Name) -> u32;
        fn gss_release_name(minor: *mut u32, name: *mut Name) -> u32;
        fn gss_init_sec_context(
            minor: *mut u32,
            cred: Cred,
            ctx: *mut Ctx,
            target: Name,
            mech: *const OidDesc,
            req_flags: u32,
            time_req: u32,
            bindings: *const ChannelBindings,
            input: *const BufferDesc,
            actual_mech: *mut *mut OidDesc,
            output: *mut BufferDesc,
            ret_flags: *mut u32,
            time_rec: *mut u32,
        ) -> u32;
        fn gss_delete_sec_context(minor: *mut u32, ctx: *mut Ctx, output: *mut BufferDesc) -> u32;
        fn gss_release_buffer(minor: *mut u32, buffer: *mut BufferDesc) -> u32;
        fn gss_display_status(
            minor: *mut u32,
            status: u32,
            status_type: i32,
            mech: *const OidDesc,
            message_context: *mut u32,
            text: *mut BufferDesc,
        ) -> u32;
    }

    /// Текст статуса GSS для журнала.
    fn status_text(status: u32, kind: i32, mech: &'static [u8]) -> String {
        let mech = oid(mech);
        let mut out = String::new();
        let mut more = 0u32;
        for _ in 0..8 {
            let (mut minor, mut text) = (0u32, BufferDesc::EMPTY);
            // SAFETY: `mech` и `text` — наши, живут до конца вызова; буфер текста выделяет GSS, освобождаем ниже.
            let major = unsafe { gss_display_status(&mut minor, status, kind, &mech, &mut more, &mut text) };
            if major & GSS_ERROR_MASK != 0 {
                break;
            }
            if !text.value.is_null() {
                // SAFETY: GSS выделил `length` байт по `value`; читаем до освобождения.
                let part = unsafe { std::slice::from_raw_parts(text.value as *const u8, text.length) };
                if !out.is_empty() {
                    out.push_str("; ");
                }
                out.push_str(&String::from_utf8_lossy(part));
                // SAFETY: буфер выделил `gss_display_status`, освобождаем один раз.
                unsafe { gss_release_buffer(&mut minor, &mut text) };
            }
            if more == 0 {
                break;
            }
        }
        out
    }

    /// Ошибка вызова GSS: код — major status (`GSS_S_*`). Причина механизма (minor) — только в журнал `debug`: в
    /// сообщениях Kerberos бывают имена.
    fn fail(call: &'static str, major: u32, minor: u32, mech: &'static [u8]) -> Error {
        tracing::debug!(call, major, minor, gss = %status_text(major, GSS_C_GSS_CODE, mech), mech = %status_text(minor, GSS_C_MECH_CODE, mech), "ews: gss");
        Error::Sspi { call, code: major }
    }

    /// SPN `HTTP/<host>` → имя службы GSS `HTTP@<host>`.
    fn service_name(target: &str) -> String {
        target.replacen('/', "@", 1)
    }

    fn import(name: &str, kind: &'static [u8], mech: &'static [u8]) -> Result<Name> {
        let (mut minor, mut out) = (0u32, ptr::null_mut());
        let kind = oid(kind);
        // SAFETY: буфер имени и OID живут до конца вызова; GSS копирует имя в свой объект `out`.
        let major = unsafe { gss_import_name(&mut minor, &BufferDesc::of(name.as_bytes()), &kind, &mut out) };
        if major & GSS_ERROR_MASK != 0 {
            return Err(fail("gss_import_name", major, minor, mech));
        }
        Ok(out)
    }

    pub(super) struct Context {
        ctx: Ctx,
        target: Name,
        mech: &'static [u8],
        /// Данные приложения привязки к каналу (`tls-server-end-point:` + хэш).
        binding: Option<Vec<u8>>,
        pub(super) done: bool,
    }

    // SAFETY: объекты GSS (имя, контекст) принадлежат одному `Context` и используются только через `&mut self` —
    // одним потоком за раз; привязки объекта к потоку у GSS нет. Рукопожатие идёт в async-задаче, которую
    // планировщик может перенести на другой поток.
    unsafe impl Send for Context {}

    impl Context {
        pub(super) fn new(scheme: Scheme, target: &str, binding: Option<&[u8]>) -> Result<Context> {
            let mech = match scheme {
                Scheme::Negotiate => SPNEGO,
                Scheme::Ntlm => NTLM,
            };
            let mut this = Context { ctx: ptr::null_mut(), target: ptr::null_mut(), mech, binding: binding.map(<[u8]>::to_vec), done: false };
            this.target = import(&service_name(target), NT_HOSTBASED_SERVICE, mech)?;
            Ok(this)
        }

        pub(super) fn step(&mut self, input: Option<&[u8]>) -> Result<Vec<u8>> {
            let input = input.map(BufferDesc::of);
            let bindings = self.binding.as_deref().map(|app| ChannelBindings {
                initiator_addrtype: 0,
                initiator_address: BufferDesc::EMPTY,
                acceptor_addrtype: 0,
                acceptor_address: BufferDesc::EMPTY,
                application_data: BufferDesc::of(app),
            });
            let mech = oid(self.mech);
            let (mut minor, mut out) = (0u32, BufferDesc::EMPTY);
            // SAFETY: учётные данные — нуль (по умолчанию системы); `ctx` — нуль на первом шаге (GSS создаёт контекст)
            // и тот же объект дальше; `target` — из `gss_import_name`. Вход, привязка и OID живут до конца вызова;
            // выходной токен выделяет GSS, освобождаем ниже. Необязательные выходы — нули.
            let major = unsafe {
                gss_init_sec_context(
                    &mut minor,
                    ptr::null_mut(),
                    &mut self.ctx,
                    self.target,
                    &mech,
                    0,
                    0,
                    bindings.as_ref().map_or(ptr::null(), |b| b as *const ChannelBindings),
                    input.as_ref().map_or(ptr::null(), |b| b as *const BufferDesc),
                    ptr::null_mut(),
                    &mut out,
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            };
            let token = if out.value.is_null() || out.length == 0 {
                Vec::new()
            } else {
                // SAFETY: GSS выделил `length` байт по `value`; читаем их до освобождения.
                unsafe { std::slice::from_raw_parts(out.value as *const u8, out.length) }.to_vec()
            };
            if !out.value.is_null() {
                // SAFETY: буфер выделил `gss_init_sec_context`, освобождаем один раз.
                unsafe { gss_release_buffer(&mut 0, &mut out) };
            }
            if major & GSS_ERROR_MASK != 0 {
                return Err(fail("gss_init_sec_context", major, minor, self.mech));
            }
            if major & GSS_S_CONTINUE_NEEDED == 0 {
                self.done = true;
            }
            Ok(token)
        }
    }

    impl Drop for Context {
        fn drop(&mut self) {
            if !self.ctx.is_null() {
                // SAFETY: контекст создан `gss_init_sec_context` и удаляется один раз; токен удаления не нужен.
                unsafe { gss_delete_sec_context(&mut 0, &mut self.ctx, ptr::null_mut()) };
            }
            if !self.target.is_null() {
                // SAFETY: имя получено `gss_import_name`, освобождается один раз.
                unsafe { gss_release_name(&mut 0, &mut self.target) };
            }
        }
    }

    #[cfg(all(target_os = "macos", any(test, feature = "testkit")))]
    pub use testkit::SystemLogin;

    #[cfg(all(target_os = "macos", any(test, feature = "testkit")))]
    mod testkit {
        use std::ffi::c_void;
        use std::ptr;

        use super::{Cred, GSS_ERROR_MASK, NT_USER_NAME, NTLM, Name, OidDesc, fail, gss_release_name, import, oid};
        use crate::Result;

        #[repr(C)]
        struct Opaque {
            _p: [u8; 0],
        }

        /// `kCFStringEncodingUTF8`.
        const UTF8: u32 = 0x0800_0100;

        #[link(name = "CoreFoundation", kind = "framework")]
        unsafe extern "C" {
            static kCFTypeDictionaryKeyCallBacks: Opaque;
            static kCFTypeDictionaryValueCallBacks: Opaque;
            fn CFStringCreateWithBytes(alloc: *const c_void, bytes: *const u8, len: isize, encoding: u32, external: u8) -> *const c_void;
            fn CFDictionaryCreate(
                alloc: *const c_void,
                keys: *const *const c_void,
                values: *const *const c_void,
                count: isize,
                key_callbacks: *const Opaque,
                value_callbacks: *const Opaque,
            ) -> *const c_void;
            fn CFRelease(cf: *const c_void);
        }

        #[link(name = "GSS", kind = "framework")]
        unsafe extern "C" {
            fn gss_aapl_initial_cred(
                name: Name,
                mech: *const OidDesc,
                attributes: *const c_void,
                output: *mut Cred,
                error: *mut *const c_void,
            ) -> u32;
            fn gss_destroy_cred(minor: *mut u32, cred: *mut Cred) -> u32;
        }

        /// Учётные данные NTLM в хранилище системы (GSSCred) — как после входа в домен — для тестов входа без пароля
        /// (`testkit`): `gss_aapl_initial_cred`. Из хранилища убираются при `drop`.
        pub struct SystemLogin(Cred);

        impl SystemLogin {
            /// `user@DOMAIN` и пароль → учётные данные NTLM системы по умолчанию.
            pub fn ntlm(user: &str, password: &str) -> Result<SystemLogin> {
                let mut name = import(user, NT_USER_NAME, NTLM)?;
                let mech = oid(NTLM);
                let mut cred: Cred = ptr::null_mut();
                let mut error: *const c_void = ptr::null();
                // SAFETY: строки (ключ и пароль) и словарь создаются здесь и освобождаются после вызова (ошибка CF —
                // тоже); ключ `kGSSICPassword` в заголовке GSS — `CFSTR("kGSSICPassword")`, символа у него нет. Имя —
                // из `gss_import_name`, освобождается один раз.
                let major = unsafe {
                    let cf = |s: &str| CFStringCreateWithBytes(ptr::null(), s.as_ptr(), s.len() as isize, UTF8, 0);
                    let (key, pw) = (cf("kGSSICPassword"), cf(password));
                    let (keys, values) = ([key], [pw]);
                    let attrs = CFDictionaryCreate(
                        ptr::null(),
                        keys.as_ptr(),
                        values.as_ptr(),
                        1,
                        &raw const kCFTypeDictionaryKeyCallBacks,
                        &raw const kCFTypeDictionaryValueCallBacks,
                    );
                    let major = gss_aapl_initial_cred(name, &mech, attrs, &mut cred, &mut error);
                    CFRelease(attrs);
                    CFRelease(pw);
                    CFRelease(key);
                    if !error.is_null() {
                        CFRelease(error);
                    }
                    gss_release_name(&mut 0, &mut name);
                    major
                };
                if major & GSS_ERROR_MASK != 0 || cred.is_null() {
                    return Err(fail("gss_aapl_initial_cred", major, 0, NTLM));
                }
                Ok(SystemLogin(cred))
            }
        }

        impl Drop for SystemLogin {
            fn drop(&mut self) {
                // SAFETY: учётные данные получены `gss_aapl_initial_cred`; `gss_destroy_cred` убирает их из хранилища и
                // освобождает дескриптор, один раз.
                unsafe { gss_destroy_cred(&mut 0, &mut self.0) };
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn service_names() {
            assert_eq!(service_name("HTTP/mail.corp.example"), "HTTP@mail.corp.example");
        }

        /// Без пароля — учётные данные NTLM системы (как после входа в домен): первый токен — NEGOTIATE (`NTLMSSP\0`,
        /// тип 1), с привязкой к каналу; мусор вместо CHALLENGE — ошибка GSS, не паника.
        #[cfg(target_os = "macos")]
        #[test]
        fn ntlm_from_system_login() {
            let _login = SystemLogin::ntlm("nobody@EXAMPLE", "not-a-password").unwrap();
            let mut ctx = Context::new(Scheme::Ntlm, "HTTP/mail.example.test", Some(b"tls-server-end-point:x")).unwrap();
            let token = ctx.step(None).unwrap();
            assert!(token.starts_with(b"NTLMSSP\0\x01\0\0\0"), "{:?}", &token[..token.len().min(12)]);
            assert!(!ctx.done);
            assert!(ctx.step(Some(b"garbage")).is_err());
        }

        /// Linux без билета Kerberos (пустой кэш): отказ GSS ошибкой входа, не паника и не зависание.
        #[cfg(target_os = "linux")]
        #[test]
        fn negotiate_without_ticket_fails() {
            let dir = std::env::temp_dir().join(format!("postman-ews-krb5-{}", std::process::id()));
            // SAFETY (окружение): переменные читает только libkrb5 в этом тесте; nextest запускает тест отдельным
            // процессом.
            unsafe {
                std::env::set_var("KRB5CCNAME", format!("FILE:{}", dir.join("none").display()));
                std::env::set_var("KRB5_CONFIG", dir.join("none.conf"));
            }
            let mut ctx = Context::new(Scheme::Negotiate, "HTTP/mail.example.test", Some(b"tls-server-end-point:x")).unwrap();
            assert!(matches!(ctx.step(None), Err(crate::Error::Sspi { call: "gss_init_sec_context", .. })));
            assert!(!ctx.done);
        }
    }
}

#[cfg(all(target_os = "macos", feature = "testkit"))]
pub use imp::SystemLogin;

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
mod imp {
    use crate::auth::Scheme;
    use crate::{Error, Result};

    /// Вход без пароля — Windows (SSPI), macOS и Linux (GSS-API); на других ОС его нет.
    pub(super) struct Context {
        pub(super) done: bool,
    }

    const NO_OS_SIGN_IN: &str = "sign-in without a password is not available on this system; enter the password";

    impl Context {
        pub(super) fn new(_: Scheme, _: &str, _: Option<&[u8]>) -> Result<Context> {
            Err(Error::Auth(NO_OS_SIGN_IN.into()))
        }

        pub(super) fn step(&mut self, _: Option<&[u8]>) -> Result<Vec<u8>> {
            Err(Error::Auth(NO_OS_SIGN_IN.into()))
        }
    }
}
