//! Контроллер домена компьютера — службой Windows (`DsGetDcNameW`, решение владельца №12). Единственное место
//! `unsafe` в крейте; у каждого блока — почему он безопасен.

/// Имя DNS контроллера домена с каталогом (без `\\`); `None` — компьютер не в домене или контроллер не найден.
#[cfg(windows)]
pub(crate) fn domain_controller() -> Option<String> {
    use windows_sys::Win32::NetworkManagement::NetManagement::NetApiBufferFree;
    use windows_sys::Win32::Networking::ActiveDirectory::{DOMAIN_CONTROLLER_INFOW, DS_DIRECTORY_SERVICE_REQUIRED, DS_RETURN_DNS_NAME, DsGetDcNameW};

    let mut info: *mut DOMAIN_CONTROLLER_INFOW = std::ptr::null_mut();
    // SAFETY: компьютер, домен, GUID и сайт — null (текущие); `info` — выходной указатель на буфер NetApi, который
    // при успехе освобождается ниже ровно один раз.
    let rc = unsafe {
        DsGetDcNameW(
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            DS_DIRECTORY_SERVICE_REQUIRED | DS_RETURN_DNS_NAME,
            &mut info,
        )
    };
    if rc != 0 || info.is_null() {
        tracing::debug!("ldap: контроллер домена не найден, DsGetDcNameW = {rc}");
        return None;
    }
    // SAFETY: успех — `info` указывает на заполненную `DOMAIN_CONTROLLER_INFOW`; имя — строка UTF-16 с нулём на конце
    // (или null), живёт до `NetApiBufferFree`.
    let name = unsafe { wide((*info).DomainControllerName) };
    // SAFETY: буфер выдал `DsGetDcNameW`, после этой строки он не используется.
    unsafe { NetApiBufferFree(info.cast()) };
    name.map(|n| n.trim_start_matches('\\').to_owned()).filter(|n| !n.is_empty())
}

/// Строка UTF-16 с нулём на конце.
///
/// # Safety
/// `p` — null или указатель на строку UTF-16, оканчивающуюся нулём.
#[cfg(windows)]
unsafe fn wide(p: *const u16) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let mut len = 0usize;
    // SAFETY: по договору функции строка оканчивается нулём — чтение не выходит за неё.
    while unsafe { *p.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `len` символов до нуля прочитаны выше.
    Some(String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) }))
}

/// Не Windows: домена нет — только сервер из настроек.
#[cfg(not(windows))]
pub(crate) fn domain_controller() -> Option<String> {
    None
}
