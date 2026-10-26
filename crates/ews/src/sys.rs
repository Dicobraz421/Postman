//! SSPI Windows для входа HTTP: `AcquireCredentialsHandleW` + `InitializeSecurityContextW` пакета Negotiate
//! (Kerberos, иначе NTLM) или NTLM. Единственное место `unsafe` в крейте; у каждого блока — почему он безопасен.
//!
//! Без пароля — учётная запись Windows текущего входа (Kerberos на машине в домене). С паролем —
//! `SEC_WINNT_AUTH_IDENTITY_W`: SSPI копирует её при вызове, буферы с паролем обнуляются сразу после.

use crate::auth::Scheme;
use crate::{Auth, Result};

/// Контекст безопасности одного рукопожатия.
pub(crate) struct Context(imp::Context);

impl Context {
    /// Учётные данные и контекст для `target` (SPN `HTTP/<host>`); `binding` — данные привязки к каналу TLS.
    pub(crate) fn new(scheme: Scheme, auth: &Auth, target: &str, binding: Option<&[u8]>) -> Result<Context> {
        imp::Context::new(scheme, auth, target, binding).map(Context)
    }

    /// Следующий шаг: токен сервера (на первом шаге — `None`) → токен для сервера (может быть пустым на
    /// последнем шаге).
    pub(crate) fn step(&mut self, input: Option<&[u8]>) -> Result<Vec<u8>> {
        self.0.step(input)
    }

    /// Рукопожатие завершено с нашей стороны.
    pub(crate) fn done(&self) -> bool {
        self.0.done
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::ptr;

    use windows::Win32::Foundation::{SEC_E_OK, SEC_I_COMPLETE_AND_CONTINUE, SEC_I_COMPLETE_NEEDED, SEC_I_CONTINUE_NEEDED};
    use windows::Win32::Security::Authentication::Identity::{
        AcquireCredentialsHandleW, CompleteAuthToken, DeleteSecurityContext, FreeContextBuffer, FreeCredentialsHandle, ISC_REQ_ALLOCATE_MEMORY,
        InitializeSecurityContextW, SECBUFFER_CHANNEL_BINDINGS, SECBUFFER_TOKEN, SECBUFFER_VERSION, SECPKG_CRED_OUTBOUND, SECURITY_NATIVE_DREP,
        SecBuffer, SecBufferDesc,
    };
    use windows::Win32::Security::Credentials::SecHandle;
    use windows::Win32::System::Rpc::{SEC_WINNT_AUTH_IDENTITY_UNICODE, SEC_WINNT_AUTH_IDENTITY_W};
    use windows::core::PCWSTR;
    use zeroize::Zeroizing;

    use crate::auth::{Scheme, sec_channel_bindings};
    use crate::{Auth, Error, Result};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// `DOMAIN\user` → (user, domain); UPN `user@domain` — имя целиком, домен пустой (так его принимает SSPI).
    fn split_user(user: &str) -> (&str, &str) {
        match user.split_once('\\') {
            Some((domain, name)) => (name, domain),
            None => (user, ""),
        }
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
        pub(super) fn new(scheme: Scheme, auth: &Auth, target: &str, binding: Option<&[u8]>) -> Result<Context> {
            let package = wide(match scheme {
                Scheme::Negotiate => "Negotiate",
                Scheme::Ntlm => "NTLM",
            });
            let mut cred = SecHandle::default();
            let status = match auth {
                Auth::Windows => {
                    // SAFETY: `package` — строка UTF-16 с нулём, живёт до конца вызова; `cred` — место под
                    // дескриптор; остальные указатели — нулевые (текущий вход Windows, без ключей и срока).
                    unsafe {
                        AcquireCredentialsHandleW(
                            PCWSTR::null(),
                            PCWSTR(package.as_ptr()),
                            SECPKG_CRED_OUTBOUND,
                            None,
                            None,
                            None,
                            None,
                            &mut cred,
                            None,
                        )
                    }
                }
                Auth::Password { user, password } => {
                    let (name, domain) = split_user(user);
                    let mut name: Vec<u16> = name.encode_utf16().collect();
                    let mut domain: Vec<u16> = domain.encode_utf16().collect();
                    let mut secret: Zeroizing<Vec<u16>> = Zeroizing::new(password.encode_utf16().collect());
                    let ptr_or_null = |v: &mut Vec<u16>| if v.is_empty() { ptr::null_mut() } else { v.as_mut_ptr() };
                    let identity = SEC_WINNT_AUTH_IDENTITY_W {
                        User: ptr_or_null(&mut name),
                        UserLength: name.len() as u32,
                        Domain: ptr_or_null(&mut domain),
                        DomainLength: domain.len() as u32,
                        Password: ptr_or_null(&mut secret),
                        PasswordLength: secret.len() as u32,
                        Flags: SEC_WINNT_AUTH_IDENTITY_UNICODE,
                    };
                    // SAFETY: `identity` и её буферы (`name`, `domain`, `secret`) живут до конца вызова, длины — в
                    // символах UTF-16 без нуля, как требует `SEC_WINNT_AUTH_IDENTITY_UNICODE`; SSPI копирует их в
                    // дескриптор. `package` — UTF-16 с нулём; `cred` — место под дескриптор.
                    unsafe {
                        AcquireCredentialsHandleW(
                            PCWSTR::null(),
                            PCWSTR(package.as_ptr()),
                            SECPKG_CRED_OUTBOUND,
                            None,
                            Some(&identity as *const SEC_WINNT_AUTH_IDENTITY_W as *const c_void),
                            None,
                            None,
                            &mut cred,
                            None,
                        )
                    }
                }
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

        #[test]
        fn users() {
            assert_eq!(split_user(r"CORP\ivanov"), ("ivanov", "CORP"));
            assert_eq!(split_user("ivanov@example.test"), ("ivanov@example.test", ""));
        }

        /// NTLM без сети: первый токен — NEGOTIATE_MESSAGE (`NTLMSSP\0`, тип 1), и с паролем, и от входа Windows.
        #[test]
        fn ntlm_first_token_offline() {
            for auth in [Auth::Windows, Auth::password(r"EXAMPLE\nobody", "not-a-password")] {
                let mut ctx = Context::new(Scheme::Ntlm, &auth, "HTTP/mail.example.test", Some(b"tls-server-end-point:x")).unwrap();
                let token = ctx.step(None).unwrap();
                assert!(token.starts_with(b"NTLMSSP\0\x01\0\0\0"), "{:?}", &token[..token.len().min(12)]);
                assert!(!ctx.done);
                // Мусор вместо вызова сервера — ошибка SSPI, не паника.
                assert!(ctx.step(Some(b"garbage")).is_err());
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use crate::auth::Scheme;
    use crate::{Auth, Error, Result};

    /// Postman — только для Windows (S0); вне Windows входа SSPI нет.
    pub(super) struct Context {
        pub(super) done: bool,
    }

    impl Context {
        pub(super) fn new(_: Scheme, _: &Auth, _: &str, _: Option<&[u8]>) -> Result<Context> {
            Err(Error::Auth("SSPI is available on Windows only".into()))
        }

        pub(super) fn step(&mut self, _: Option<&[u8]>) -> Result<Vec<u8>> {
            Err(Error::Auth("SSPI is available on Windows only".into()))
        }
    }
}
