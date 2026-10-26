//! Пароль учётной записи — в диспетчере учётных данных Windows (CredWriteW / CredReadW / CredDeleteW),
//! тип «общие учётные данные», хранение — для этого пользователя на этой машине. На диск приложения пароль
//! не попадает; в журнал — тоже (ошибки — только код Win32).

use windows_sys::Win32::Foundation::{ERROR_NOT_FOUND, GetLastError};
use windows_sys::Win32::Security::Credentials::{
    CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW, CredWriteW,
};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn last_error(what: &str) -> String {
    // SAFETY: без аргументов, читает код ошибки потока.
    let code = unsafe { GetLastError() };
    format!("{what}: Win32 error {code}")
}

pub fn write(target: &str, user: &str, secret: &str) -> Result<(), String> {
    let mut target = wide(target);
    let mut user = wide(user);
    let mut blob = secret.as_bytes().to_vec();
    let size = u32::try_from(blob.len()).map_err(|_| "credential is too long".to_owned())?;
    let cred = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: target.as_mut_ptr(),
        CredentialBlobSize: size,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: user.as_mut_ptr(),
        ..Default::default()
    };
    // SAFETY: строки и блоб живут до конца вызова; CredWriteW копирует их.
    let ok = unsafe { CredWriteW(&cred, 0) };
    // Копия секрета в памяти процесса — затереть.
    blob.fill(0);
    if ok == 0 { Err(last_error("CredWriteW")) } else { Ok(()) }
}

/// `Ok(None)` — такой записи нет.
pub fn read(target: &str) -> Result<Option<String>, String> {
    let target = wide(target);
    let mut cred: *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: имя — строка с нулём на конце; результат освобождается CredFree ниже.
    let ok = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut cred) };
    if ok == 0 {
        // SAFETY: без аргументов.
        return if unsafe { GetLastError() } == ERROR_NOT_FOUND { Ok(None) } else { Err(last_error("CredReadW")) };
    }
    // SAFETY: при успехе `cred` указывает на запись, выделенную системой; блоб — `CredentialBlobSize` байт.
    let secret = unsafe {
        let c = &*cred;
        let bytes = if c.CredentialBlob.is_null() { &[][..] } else { std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize) };
        let s = String::from_utf8(bytes.to_vec());
        CredFree(cred.cast());
        s
    };
    secret.map(Some).map_err(|_| "credential is not UTF-8".to_owned())
}

/// Нет записи — не ошибка.
pub fn delete(target: &str) -> Result<(), String> {
    let target = wide(target);
    // SAFETY: имя — строка с нулём на конце.
    let ok = unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) };
    // SAFETY: без аргументов.
    if ok == 0 && unsafe { GetLastError() } != ERROR_NOT_FOUND { Err(last_error("CredDeleteW")) } else { Ok(()) }
}

#[cfg(test)]
mod tests {
    /// Пишет, читает и удаляет пробную запись в диспетчере учётных данных этого пользователя.
    #[test]
    #[ignore = "writes to the Windows Credential Manager of the current user"]
    fn credential_round_trip() {
        let target = format!("Postman:test:{}:{}", std::process::id(), jiff::Timestamp::now().as_nanosecond());
        assert_eq!(super::read(&target).unwrap(), None);
        super::write(&target, "user", "dummy секрет").unwrap();
        assert_eq!(super::read(&target).unwrap().as_deref(), Some("dummy секрет"));
        super::write(&target, "user", "second").unwrap();
        assert_eq!(super::read(&target).unwrap().as_deref(), Some("second"));
        super::delete(&target).unwrap();
        assert_eq!(super::read(&target).unwrap(), None);
        super::delete(&target).unwrap();
    }
}
