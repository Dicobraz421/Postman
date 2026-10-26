//! Снимок клиентской области окна через PrintWindow (`PW_RENDERFULLCONTENT` — с содержимым DirectX/DComp).
//!
//! Вызывать не из потока окна: PrintWindow шлёт сообщения окну и ждёт ответа, а в потоке окна это
//! повторный вход в оконную процедуру gpui посреди отрисовки.

use std::ffi::c_void;

use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GdiFlush, GetDC, HBITMAP,
    HDC, HGDIOBJ, ReleaseDC, SelectObject,
};
use windows_sys::Win32::Storage::Xps::{PW_CLIENTONLY, PrintWindow};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetClientRect, PW_RENDERFULLCONTENT};

use super::Image;

/// Контекст экрана окна: освобождается при выходе из области.
struct WindowDc(HWND, HDC);

impl Drop for WindowDc {
    fn drop(&mut self) {
        // SAFETY: контекст получен `GetDC` для этого окна и освобождается один раз.
        unsafe { ReleaseDC(self.0, self.1) };
    }
}

/// Контекст в памяти с выбранной в него картинкой; при выходе возвращает прежний объект и удаляет оба.
struct MemDc {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
}

impl Drop for MemDc {
    fn drop(&mut self) {
        // SAFETY: объекты созданы в `capture` и удаляются один раз; картинку сначала выбираем из контекста.
        unsafe {
            if !self.old.is_null() {
                SelectObject(self.dc, self.old);
            }
            if !self.bitmap.is_null() {
                DeleteObject(self.bitmap);
            }
            DeleteDC(self.dc);
        }
    }
}

pub fn capture(hwnd: isize) -> Result<Image, String> {
    let hwnd = hwnd as HWND;
    let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    // SAFETY: `rect` — валидный указатель на RECT; неверное окно даёт ошибку, а не UB.
    if unsafe { GetClientRect(hwnd, &mut rect) } == 0 {
        return Err("GetClientRect: window unavailable".into());
    }
    let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
    if w <= 0 || h <= 0 {
        return Err(format!("empty client area {w}x{h} (window minimized?)"));
    }
    // SAFETY: GetDC с окном процесса; результат проверяется и освобождается в `WindowDc`.
    let screen = unsafe { GetDC(hwnd) };
    if screen.is_null() {
        return Err("GetDC: no window DC".into());
    }
    let screen = WindowDc(hwnd, screen);
    // SAFETY: совместимый контекст для действующего контекста окна.
    let dc = unsafe { CreateCompatibleDC(screen.1) };
    if dc.is_null() {
        return Err("CreateCompatibleDC".into());
    }
    let mut mem = MemDc { dc, bitmap: std::ptr::null_mut(), old: std::ptr::null_mut() };
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            // Отрицательная высота — строки сверху вниз, как в PNG.
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut c_void = std::ptr::null_mut();
    // SAFETY: `info` описывает 32-битную DIB w×h; `bits` получит указатель на её пиксели.
    mem.bitmap = unsafe { CreateDIBSection(mem.dc, &info, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0) };
    if mem.bitmap.is_null() || bits.is_null() {
        return Err("CreateDIBSection".into());
    }
    // SAFETY: картинка и контекст созданы выше; прежний объект вернёт `MemDc::drop`.
    mem.old = unsafe { SelectObject(mem.dc, mem.bitmap) };
    // SAFETY: окно и контекст действующие; вызов не из потока окна (см. описание модуля).
    if unsafe { PrintWindow(hwnd, mem.dc, PW_CLIENTONLY | PW_RENDERFULLCONTENT) } == 0 {
        return Err("PrintWindow failed".into());
    }
    // SAFETY: дождаться записи GDI в пиксели DIB перед чтением.
    unsafe { GdiFlush() };
    let len = w as usize * h as usize * 4;
    // SAFETY: DIB 32 бит без выравнивания строк (ширина × 4 кратна 4) — ровно `len` байт; живёт, пока жив `mem`.
    let bgra = unsafe { std::slice::from_raw_parts(bits as *const u8, len) };
    let mut rgba = Vec::with_capacity(len);
    for px in bgra.as_chunks::<4>().0 {
        rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
    }
    drop(mem);
    Ok(Image { width: w as u32, height: h as u32, rgba })
}
