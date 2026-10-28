//! Снимок клиентской области окна через PrintWindow (`PW_RENDERFULLCONTENT` — с содержимым DirectX/DComp) и значки
//! окна (T1-01).
//!
//! Снимок вызывать не из потока окна: PrintWindow шлёт сообщения окну и ждёт ответа, а в потоке окна это
//! повторный вход в оконную процедуру gpui посреди отрисовки.

use std::ffi::c_void;

use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GdiFlush, GetDC, HBITMAP,
    HDC, HGDIOBJ, ReleaseDC, SelectObject,
};
use windows_sys::Win32::Storage::Xps::{PW_CLIENTONLY, PrintWindow};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetSystemMetrics, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_DEFAULTCOLOR, LoadImageW, PW_RENDERFULLCONTENT, SM_CXICON, SM_CXSMICON,
    SM_CYICON, SM_CYSMICON, SendMessageW, WM_SETICON,
};

use super::Image;

/// Значок приложения в ресурсах exe (`build.rs`: `1 ICON res/postman.ico`).
const ICON_ID: u16 = 1;

/// Значки окна (T1-01): большой (`SM_CXICON`: Alt+Tab) и малый (`SM_CXSMICON`: заголовок, панель задач) — каждый
/// своего размера из группы значков 1 exe, а не масштабом одного кадра. gpui ставит классу окна только значок
/// размера по умолчанию; малый Windows тогда уменьшает сама. Значки живут до конца процесса.
pub fn set_icons(hwnd: isize) -> Result<(), String> {
    let hwnd = hwnd as HWND;
    // SAFETY: без указателей; модуль exe процесса.
    let module = unsafe { GetModuleHandleW(std::ptr::null()) };
    if module.is_null() {
        return Err("GetModuleHandleW: no module".into());
    }
    for (kind, cx, cy) in [(ICON_BIG, SM_CXICON, SM_CYICON), (ICON_SMALL, SM_CXSMICON, SM_CYSMICON)] {
        // SAFETY: без указателей.
        let (w, h) = unsafe { (GetSystemMetrics(cx), GetSystemMetrics(cy)) };
        // SAFETY: номер ресурса — целым в PCWSTR (MAKEINTRESOURCE); модуль — exe процесса, жив до конца процесса.
        let icon = unsafe { LoadImageW(module, ICON_ID as usize as *const u16, IMAGE_ICON, w, h, LR_DEFAULTCOLOR) };
        if icon.is_null() {
            return Err(format!("LoadImageW: no icon {ICON_ID} of {w}x{h}"));
        }
        // SAFETY: окно процесса; значок не освобождается — окно держит его до конца процесса.
        unsafe { SendMessageW(hwnd, WM_SETICON, kind as usize, icon as isize) };
    }
    Ok(())
}

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

#[cfg(test)]
mod tests {
    use windows_sys::Win32::System::LibraryLoader::{FindResourceW, GetModuleHandleW, LoadResource, LockResource, SizeofResource};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, ICON_BIG, ICON_SMALL, IDI_APPLICATION, LoadIconW, RT_GROUP_ICON, SendMessageW, WM_GETICON, WS_OVERLAPPED,
    };

    /// Стороны кадров группы значков 1 текущего exe (тесты собраны с теми же ресурсами, `build.rs`): заголовок
    /// GRPICONDIR — 6 байт, записи — по 14, первый байт записи — ширина (0 — 256).
    fn group_sizes() -> Vec<u32> {
        // SAFETY: модуль процесса; ресурс читается только в пределах `SizeofResource`, данные ресурса живут до конца
        // процесса.
        unsafe {
            let module = GetModuleHandleW(std::ptr::null());
            let res = FindResourceW(module, super::ICON_ID as usize as *const u16, RT_GROUP_ICON);
            assert!(!res.is_null(), "no RT_GROUP_ICON 1");
            let len = SizeofResource(module, res) as usize;
            let data = LockResource(LoadResource(module, res)) as *const u8;
            assert!(!data.is_null() && len >= 6);
            let bytes = std::slice::from_raw_parts(data, len);
            let n = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
            let mut sizes: Vec<u32> = (0..n).filter_map(|i| bytes.get(6 + 14 * i)).map(|&w| if w == 0 { 256 } else { u32::from(w) }).collect();
            sizes.sort_unstable();
            sizes
        }
    }

    /// T1-01: в exe — значок «Ленты» всех размеров `res/postman.ico`.
    #[test]
    fn exe_icon_has_all_sizes() {
        assert_eq!(group_sizes(), [16, 20, 24, 32, 40, 48, 64, 256]);
    }

    /// T1-01: у окна после `set_icons` оба значка свои — не пусты и не общий `IDI_APPLICATION`.
    #[test]
    fn window_gets_both_icons() {
        let class: Vec<u16> = "STATIC".encode_utf16().chain([0]).collect();
        // SAFETY: системный класс STATIC, окно без родителя; уничтожается в конце теста.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                std::ptr::null(),
                WS_OVERLAPPED,
                0,
                0,
                100,
                100,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert!(!hwnd.is_null(), "CreateWindowExW");
        super::set_icons(hwnd as isize).unwrap();
        // SAFETY: общий системный значок, без освобождения.
        let generic = unsafe { LoadIconW(std::ptr::null_mut(), IDI_APPLICATION) } as isize;
        for kind in [ICON_SMALL, ICON_BIG] {
            // SAFETY: окно создано выше.
            let icon = unsafe { SendMessageW(hwnd, WM_GETICON, kind as usize, 0) };
            assert!(icon != 0 && icon != generic, "icon {kind}: {icon:#x}");
        }
        // SAFETY: окно создано выше и больше не нужно.
        unsafe { DestroyWindow(hwnd) };
    }
}
