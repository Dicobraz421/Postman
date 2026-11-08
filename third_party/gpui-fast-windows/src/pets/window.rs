//! Окно приложения: номер окна для другого потока, свои значки из ресурса exe, снимок клиентской области через
//! PrintWindow (`PW_RENDERFULLCONTENT` — с содержимым DirectX/DComp).
//!
//! Снимок вызывать не из потока окна: PrintWindow шлёт сообщения окну и ждёт ответа, а в потоке окна это повторный
//! вход в оконную процедуру gpui посреди отрисовки.

use std::ffi::c_void;
use std::io;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::Foundation::{HMODULE, HWND, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject,
    GdiFlush, GetDC, HBITMAP, HDC, HGDIOBJ, ReleaseDC, SelectObject,
};
use windows_sys::Win32::Storage::Xps::{PW_CLIENTONLY, PrintWindow};
use windows_sys::Win32::System::LibraryLoader::{
    FindResourceW, GetModuleHandleW, LoadResource, LockResource, SizeofResource,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetSystemMetrics, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_DEFAULTCOLOR, LoadImageW,
    PW_RENDERFULLCONTENT, RT_GROUP_ICON, SM_CXICON, SM_CXSMICON, SM_CYICON, SM_CYSMICON, SendMessageW, WM_SETICON,
};

use super::last_error;

/// Номер окна — его можно передать в другой поток (сам `Window` туда не передаётся): снимок, родитель системного
/// окна (запрос ПИН-кода и т.п.).
pub fn id(window: &gpui::Window) -> io::Result<isize> {
    match HasWindowHandle::window_handle(window)
        .map_err(|e| io::Error::other(format!("window handle: {e}")))?
        .as_raw()
    {
        RawWindowHandle::Win32(h) => Ok(h.hwnd.get()),
        other => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("not a Win32 window: {other:?}"),
        )),
    }
}

/// Значки окна: большой (`SM_CXICON`: Alt+Tab) и малый (`SM_CXSMICON`: заголовок, панель задач) — каждый своего
/// размера из группы значков `resource` exe (`build.rs` приложения: `1 ICON res/app.ico`), а не масштабом одного
/// кадра. gpui ставит классу окна только значок размера по умолчанию; малый Windows тогда уменьшает сама. Значки
/// живут до конца процесса.
pub fn set_icons(window: &gpui::Window, resource: u16) -> io::Result<()> {
    set_icons_from(exe(), resource, id(window)?)
}

/// Стороны кадров группы значков `resource` в exe, по возрастанию (256 — кадр 0 в записи). Нет группы — `NotFound`.
pub fn icon_sizes(resource: u16) -> io::Result<Vec<u32>> {
    group_sizes(exe(), resource)
}

/// Модуль exe процесса (живёт до конца процесса).
fn exe() -> HMODULE {
    // SAFETY: null — модуль exe процесса; указателей нет.
    unsafe { GetModuleHandleW(std::ptr::null()) }
}

fn set_icons_from(module: HMODULE, resource: u16, hwnd: isize) -> io::Result<()> {
    let hwnd = hwnd as HWND;
    if module.is_null() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "GetModuleHandleW: no module"));
    }
    for (kind, cx, cy) in [(ICON_BIG, SM_CXICON, SM_CYICON), (ICON_SMALL, SM_CXSMICON, SM_CYSMICON)] {
        // SAFETY: без указателей.
        let (w, h) = unsafe { (GetSystemMetrics(cx), GetSystemMetrics(cy)) };
        // SAFETY: номер ресурса — целым в PCWSTR (MAKEINTRESOURCE); модуль загружен и жив, пока жив процесс (exe) или
        // вызывающий (тест).
        let icon = unsafe {
            LoadImageW(
                module,
                resource as usize as *const u16,
                IMAGE_ICON,
                w,
                h,
                LR_DEFAULTCOLOR,
            )
        };
        if icon.is_null() {
            return Err(last_error(&format!("LoadImageW icon {resource} {w}x{h}")));
        }
        // SAFETY: окно процесса; значок не освобождается — окно держит его до конца процесса.
        unsafe { SendMessageW(hwnd, WM_SETICON, kind as usize, icon as isize) };
    }
    Ok(())
}

/// Заголовок GRPICONDIR — 6 байт, записи — по 14, первый байт записи — ширина (0 — 256).
fn group_sizes(module: HMODULE, resource: u16) -> io::Result<Vec<u32>> {
    // SAFETY: модуль загружен; номер ресурса — целым в PCWSTR; данные ресурса читаются только в пределах
    // `SizeofResource` и живут, пока загружен модуль.
    let bytes = unsafe {
        let res = FindResourceW(module, resource as usize as *const u16, RT_GROUP_ICON);
        if res.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no RT_GROUP_ICON {resource}"),
            ));
        }
        let len = SizeofResource(module, res) as usize;
        let data = LockResource(LoadResource(module, res)) as *const u8;
        if data.is_null() || len < 6 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("RT_GROUP_ICON {resource}: {len} bytes"),
            ));
        }
        std::slice::from_raw_parts(data, len)
    };
    let n = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
    let mut sizes: Vec<u32> = (0..n)
        .filter_map(|i| bytes.get(6 + 14 * i))
        .map(|&w| if w == 0 { 256 } else { u32::from(w) })
        .collect();
    sizes.sort_unstable();
    Ok(sizes)
}

/// Снимок окна: RGBA, 8 бит на канал, строки сверху вниз.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
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

/// Снимок клиентской области окна [`id`]. Вызывать не из потока окна (см. описание модуля).
pub fn capture(window: isize) -> io::Result<Image> {
    let hwnd = window as HWND;
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: `rect` — валидный указатель на RECT; неверное окно даёт ошибку, а не UB.
    if unsafe { GetClientRect(hwnd, &mut rect) } == 0 {
        return Err(last_error("GetClientRect"));
    }
    let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
    if w <= 0 || h <= 0 {
        return Err(io::Error::other(format!(
            "empty client area {w}x{h} (window minimized?)"
        )));
    }
    // SAFETY: GetDC с окном процесса; результат проверяется и освобождается в `WindowDc`.
    let screen = unsafe { GetDC(hwnd) };
    if screen.is_null() {
        return Err(io::Error::other("GetDC: no window DC"));
    }
    let screen = WindowDc(hwnd, screen);
    // SAFETY: совместимый контекст для действующего контекста окна.
    let dc = unsafe { CreateCompatibleDC(screen.1) };
    if dc.is_null() {
        return Err(io::Error::other("CreateCompatibleDC"));
    }
    let mut mem = MemDc {
        dc,
        bitmap: std::ptr::null_mut(),
        old: std::ptr::null_mut(),
    };
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
        return Err(io::Error::other("CreateDIBSection"));
    }
    // SAFETY: картинка и контекст созданы выше; прежний объект вернёт `MemDc::drop`.
    mem.old = unsafe { SelectObject(mem.dc, mem.bitmap) };
    // SAFETY: окно и контекст действующие; вызов не из потока окна (см. описание модуля).
    if unsafe { PrintWindow(hwnd, mem.dc, PW_CLIENTONLY | PW_RENDERFULLCONTENT) } == 0 {
        return Err(io::Error::other("PrintWindow failed"));
    }
    // SAFETY: дождаться записи GDI в пиксели DIB перед чтением.
    unsafe { GdiFlush() };
    let len = w as usize * h as usize * 4;
    // SAFETY: DIB 32 бит без выравнивания строк (ширина × 4 кратна 4) — ровно `len` байт; живёт, пока жив `mem`.
    let bgra = unsafe { std::slice::from_raw_parts(bits as *const u8, len) };
    let mut rgba = Vec::with_capacity(len);
    for px in bgra.chunks_exact(4) {
        rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
    }
    drop(mem);
    Ok(Image {
        width: w as u32,
        height: h as u32,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use windows_sys::Win32::Foundation::FreeLibrary;
    use windows_sys::Win32::System::LibraryLoader::{LOAD_LIBRARY_AS_DATAFILE, LoadLibraryExW};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, ICON_BIG, ICON_SMALL, IDI_APPLICATION, LoadIconW, SendMessageW, WM_GETICON,
        WS_OVERLAPPED,
    };

    use super::*;

    /// Окно класса STATIC без родителя; уничтожается при выходе из области.
    struct TestWindow(HWND);

    impl TestWindow {
        fn new() -> Self {
            let class = super::super::wide("STATIC");
            // SAFETY: системный класс STATIC, окно без родителя; уничтожается в `drop`.
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
            Self(hwnd)
        }
    }

    impl Drop for TestWindow {
        fn drop(&mut self) {
            // SAFETY: окно создано в `new` и уничтожается один раз.
            unsafe { DestroyWindow(self.0) };
        }
    }

    /// Значки из группы 1 `shell32.dll` (у тестового exe своих значков нет): у окна оба свои — не пусты и не общий
    /// `IDI_APPLICATION`.
    #[test]
    fn pets_window_gets_both_icons() {
        let name = super::super::wide("shell32.dll");
        // SAFETY: системная библиотека как данные (без кода); освобождается в конце теста.
        let shell32 = unsafe { LoadLibraryExW(name.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_AS_DATAFILE) };
        assert!(!shell32.is_null(), "shell32.dll");
        let sizes = group_sizes(shell32, 1).unwrap();
        assert!(sizes.contains(&16) && sizes.contains(&32), "{sizes:?}");
        let window = TestWindow::new();
        set_icons_from(shell32, 1, window.0 as isize).unwrap();
        // SAFETY: общий системный значок, без освобождения.
        let generic = unsafe { LoadIconW(std::ptr::null_mut(), IDI_APPLICATION) } as isize;
        for kind in [ICON_SMALL, ICON_BIG] {
            // SAFETY: окно создано выше.
            let icon = unsafe { SendMessageW(window.0, WM_GETICON, kind as usize, 0) };
            assert!(icon != 0 && icon != generic, "icon {kind}: {icon:#x}");
        }
        drop(window);
        // SAFETY: значки окна уничтожены вместе с окном; библиотека загружена выше.
        unsafe { FreeLibrary(shell32) };
    }

    /// Нет ресурса или окна — ошибка, а не паника.
    #[test]
    fn pets_window_missing_icon_or_window_is_an_error() {
        assert_eq!(
            icon_sizes(1).unwrap_err().kind(),
            io::ErrorKind::NotFound,
            "у тестового exe значков нет"
        );
        let window = TestWindow::new();
        assert!(set_icons_from(exe(), 1, window.0 as isize).is_err());
        assert!(capture(0).is_err());
    }
}
