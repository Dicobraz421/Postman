//! Вход HTTP без пароля средствами ОС: Negotiate (Kerberos, иначе NTLM) или NTLM учётной записи текущего входа.
//! Единственное место `unsafe` в крейте; у каждого блока — почему он безопасен. Вход с паролем — NTLM на чистом Rust
//! (`ntlm`), одинаково на всех ОС.
//!
//! Windows — SSPI: `AcquireCredentialsHandleW` (учётная запись Windows текущего входа, Kerberos на машине в домене)
//! + `InitializeSecurityContextW`. macOS — MAC-05.

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
    }
}

#[cfg(not(windows))]
mod imp {
    use crate::auth::Scheme;
    use crate::{Error, Result};

    /// Вход без пароля вне Windows: на macOS — MAC-05 (Kerberos через GSS.framework), на других ОС его нет.
    pub(super) struct Context {
        pub(super) done: bool,
    }

    const NO_OS_SIGN_IN: &str = "sign-in without a password is available on Windows only; enter the password";

    impl Context {
        pub(super) fn new(_: Scheme, _: &str, _: Option<&[u8]>) -> Result<Context> {
            Err(Error::Auth(NO_OS_SIGN_IN.into()))
        }

        pub(super) fn step(&mut self, _: Option<&[u8]>) -> Result<Vec<u8>> {
            Err(Error::Auth(NO_OS_SIGN_IN.into()))
        }
    }
}
