//! Консоль родителя для release-сборки без консоли (`windows_subsystem = "windows"`, S4-01).
//!
//! У GUI-процесса stdout и stderr пусты, если их не перенаправили: строки `screen …` и `bench …` пропали бы.
//! Перенаправленный поток (`> файл`, канал Git Bash) оставляем как есть; пустой — пишем в консоль родителя
//! (`AttachConsole(ATTACH_PARENT_PROCESS)`, затем `CONOUT$`). Нет консоли у родителя (Проводник) — ничего.

use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
#[cfg(test)]
use windows_sys::Win32::Globalization::GetACP;
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_UNKNOWN, GetFileType, OPEN_EXISTING};
use windows_sys::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
};

/// Описатель, в который можно писать: файл, канал или консоль.
fn usable(h: HANDLE) -> bool {
    // SAFETY: GetFileType с чужим или закрытым описателем возвращает FILE_TYPE_UNKNOWN, а не UB.
    !h.is_null() && h != INVALID_HANDLE_VALUE && unsafe { GetFileType(h) } != FILE_TYPE_UNKNOWN
}

pub fn attach_parent() {
    const STREAMS: [STD_HANDLE; 2] = [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE];
    // SAFETY: GetStdHandle только читает таблицу стандартных описателей процесса.
    let before = STREAMS.map(|s| unsafe { GetStdHandle(s) });
    if before.iter().all(|&h| usable(h)) {
        return;
    }
    // SAFETY: без аргументов-указателей; неудача (нет консоли у родителя, уже есть своя) — просто 0.
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
        return;
    }
    let conout: Vec<u16> = "CONOUT$\0".encode_utf16().collect();
    for (stream, old) in STREAMS.into_iter().zip(before) {
        if usable(old) {
            // Перенаправленный поток: AttachConsole мог заменить его консолью — вернуть.
            // SAFETY: возвращаем описатель, который был стандартным до вызова.
            unsafe { SetStdHandle(stream, old) };
            continue;
        }
        // SAFETY: см. выше.
        if usable(unsafe { GetStdHandle(stream) }) {
            continue;
        }
        // SAFETY: `conout` — строка UTF-16 с нулём в конце; атрибуты безопасности и шаблон не нужны.
        let h = unsafe {
            CreateFileW(
                conout.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if h != INVALID_HANDLE_VALUE {
            // SAFETY: описатель консоли открыт выше и живёт до конца процесса.
            unsafe { SetStdHandle(stream, h) };
        }
    }
}

/// Кодовая страница ANSI процесса: 65001, если действует манифест exe (`activeCodePage` UTF-8).
#[cfg(test)]
pub fn active_code_page() -> u32 {
    // SAFETY: без аргументов.
    unsafe { GetACP() }
}
