//! HTML из буфера обмена Windows (`CF_HTML`, «HTML Format»: Word, браузеры) — для вставки в письмо с таблицами и
//! разметкой (T3-06). Буфер обмена gpui отдаёт только текст, картинки и файлы, а нейтрального API у pets для HTML нет:
//! здесь — четыре вызова user32/kernel32 без зависимостей (`unsafe` — только в этом модуле). Разбирает HTML воркер.

use std::ffi::c_void;

type Handle = *mut c_void;

#[link(name = "user32")]
unsafe extern "system" {
    fn RegisterClipboardFormatW(name: *const u16) -> u32;
    fn IsClipboardFormatAvailable(format: u32) -> i32;
    fn OpenClipboard(owner: Handle) -> i32;
    fn CloseClipboard() -> i32;
    fn GetClipboardData(format: u32) -> Handle;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GlobalLock(mem: Handle) -> *mut c_void;
    fn GlobalUnlock(mem: Handle) -> i32;
    fn GlobalSize(mem: Handle) -> usize;
}

/// Буфер открыт; закрывается при выходе из области.
struct Open;

impl Drop for Open {
    fn drop(&mut self) {
        // SAFETY: буфер открыт этим потоком (`OpenClipboard` вернул успех).
        unsafe { CloseClipboard() };
    }
}

/// Байты `CF_HTML` (UTF-8, с заголовком `Version:…StartFragment:…`) не больше `max`; нет HTML — `None`.
pub fn html(max: usize) -> Option<Vec<u8>> {
    let name: Vec<u16> = "HTML Format".encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: строка с нулём в конце живёт до конца вызова.
    let format = unsafe { RegisterClipboardFormatW(name.as_ptr()) };
    // SAFETY: вызовы без указателей; буфер открыт без окна-владельца и закрывается `Open`.
    if format == 0 || unsafe { IsClipboardFormatAvailable(format) } == 0 || unsafe { OpenClipboard(std::ptr::null_mut()) } == 0 {
        return None;
    }
    let _open = Open;
    // SAFETY: буфер открыт; дескриптор принадлежит буферу, его не освобождаем.
    let mem = unsafe { GetClipboardData(format) };
    if mem.is_null() {
        return None;
    }
    // SAFETY: `mem` — глобальная память буфера; размер — её, указатель живёт до `GlobalUnlock`.
    let size = unsafe { GlobalSize(mem) };
    if size == 0 || size > max {
        return None;
    }
    // SAFETY: см. выше.
    let ptr = unsafe { GlobalLock(mem) }.cast::<u8>();
    if ptr.is_null() {
        return None;
    }
    // SAFETY: `ptr` указывает на `size` байт, пока память заблокирована; копируем и сразу отпускаем.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, size) }.to_vec();
    // SAFETY: блокировка этого вызова.
    unsafe { GlobalUnlock(mem) };
    Some(bytes)
}
