//! Контроллер домена компьютера — службой Windows (`DsGetDcNameW`, решение владельца №12); на macOS — DNS SRV и
//! Kerberos GSS.framework (`sys/macos.rs`, MAC-05); на Linux — DNS SRV и билет MIT Kerberos (`sys/linux.rs`,
//! LIN-05). Единственное место `unsafe` в крейте; у каждого блока — почему он безопасен.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(crate) use linux::domain_controller;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(crate) use macos::{Kerberos, domain_controller};

/// Поиск контроллера домена — не дольше на всех ОС (PAR-AUDIT №35, ADR 0011): служба Windows, DNS SRV macOS и Linux.
#[cfg_attr(not(any(windows, target_os = "macos", target_os = "linux")), allow(dead_code))]
pub(crate) const DC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Блокирующий поиск `find` в отдельном потоке, не дольше [`DC_TIMEOUT`]: вышло время — `None` (как «не в домене»),
/// поток доживает сам. События — DEBUG: детали ОС, без имён.
#[cfg_attr(not(any(windows, target_os = "macos", target_os = "linux")), allow(dead_code))]
pub(crate) fn bounded<T: Send + 'static>(find: impl FnOnce() -> Option<T> + Send + 'static) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().name("ldap-dc".into()).spawn(move || {
        let _ = tx.send(find());
    });
    if spawned.is_err() {
        tracing::debug!("ldap: domain controller lookup thread not started");
        return None;
    }
    match rx.recv_timeout(DC_TIMEOUT) {
        Ok(found) => found,
        Err(_) => {
            tracing::debug!("ldap: domain controller lookup timed out");
            None
        }
    }
}

/// Имя DNS контроллера домена с каталогом (без `\\`); `None` — компьютер не в домене или контроллер не найден.
#[cfg(windows)]
pub(crate) fn domain_controller() -> Option<String> {
    bounded(dc_service)
}

#[cfg(windows)]
fn dc_service() -> Option<String> {
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
        tracing::debug!(rc, "ldap: no domain controller (DsGetDcNameW)");
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

/// Другие ОС: домена нет — только сервер из настроек.
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
pub(crate) fn domain_controller() -> Option<String> {
    None
}
