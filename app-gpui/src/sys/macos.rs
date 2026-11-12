//! macOS: снимок окна для `--screens` (MAC-01) — CoreGraphics напрямую, без objc. Свои окна процесс снимает без
//! разрешения «Запись экрана». `CGWindowListCreateImage` с macOS 14 устарела (замена — ScreenCaptureKit), но работает;
//! переход — с аналогом `gpui_windows::pets::window` в pets (MAC-04).

use std::ffi::c_void;

use super::Image;

type CFTypeRef = *const c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFArrayGetCount(array: CFTypeRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CFTypeRef, index: isize) -> CFTypeRef;
    fn CFDictionaryGetValue(dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    fn CFNumberGetValue(number: CFTypeRef, kind: isize, value: *mut c_void) -> u8;
    fn CFRelease(cf: CFTypeRef);
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    static kCGWindowNumber: CFTypeRef;
    static kCGWindowOwnerPID: CFTypeRef;
    static kCGWindowLayer: CFTypeRef;
    static CGRectNull: CGRect;
    fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> CFTypeRef;
    fn CGWindowListCreateImage(bounds: CGRect, option: u32, window: u32, image_option: u32) -> CFTypeRef;
    fn CGImageGetWidth(image: CFTypeRef) -> usize;
    fn CGImageGetHeight(image: CFTypeRef) -> usize;
    fn CGColorSpaceCreateDeviceRGB() -> CFTypeRef;
    fn CGBitmapContextCreate(data: *mut c_void, width: usize, height: usize, bits: usize, row: usize, space: CFTypeRef, info: u32) -> CFTypeRef;
    fn CGContextDrawImage(context: CFTypeRef, rect: CGRect, image: CFTypeRef);
}

const ON_SCREEN_ONLY: u32 = 1 << 0;
const EXCLUDE_DESKTOP: u32 = 1 << 4;
const INCLUDING_WINDOW: u32 = 1 << 3;
/// Без тени окна.
const BOUNDS_IGNORE_FRAMING: u32 = 1 << 0;
/// `kCFNumberSInt64Type`.
const SINT64: isize = 4;
/// `kCGImageAlphaPremultipliedLast | kCGBitmapByteOrder32Big`: байты R, G, B, A.
const RGBA: u32 = 1 | (4 << 12);

/// Объект CoreFoundation, освобождается при выходе из области.
struct Owned(CFTypeRef);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: объект получен от функции Create/Copy и освобождается один раз.
            unsafe { CFRelease(self.0) }
        }
    }
}

/// Число из словаря окна.
fn number(dict: CFTypeRef, key: CFTypeRef) -> Option<i64> {
    // SAFETY: `dict` — словарь из живого массива окон, ключ — константа CoreGraphics.
    let value = unsafe { CFDictionaryGetValue(dict, key) };
    if value.is_null() {
        return None;
    }
    let mut out = 0i64;
    // SAFETY: значение по этим ключам — CFNumber; пишем в локальную переменную подходящего размера.
    (unsafe { CFNumberGetValue(value, SINT64, (&raw mut out).cast()) } != 0).then_some(out)
}

/// Номер окна этого процесса на экране, впереди всех (слой 0 — обычные окна).
pub(super) fn own_window() -> Result<u32, String> {
    let pid = i64::from(std::process::id());
    // SAFETY: функция без указателей; массив освобождает `Owned`.
    let list = Owned(unsafe { CGWindowListCopyWindowInfo(ON_SCREEN_ONLY | EXCLUDE_DESKTOP, 0) });
    if list.0.is_null() {
        return Err("CGWindowListCopyWindowInfo failed".into());
    }
    // SAFETY: `list.0` — живой CFArray словарей окон (порядок — спереди назад); ключи — константы CoreGraphics.
    let (count, keys) = unsafe { (CFArrayGetCount(list.0), (kCGWindowOwnerPID, kCGWindowLayer, kCGWindowNumber)) };
    for i in 0..count {
        // SAFETY: индекс в пределах массива.
        let dict = unsafe { CFArrayGetValueAtIndex(list.0, i) };
        if number(dict, keys.0) == Some(pid)
            && number(dict, keys.1) == Some(0)
            && let Some(id) = number(dict, keys.2).and_then(|n| u32::try_from(n).ok())
        {
            return Ok(id);
        }
    }
    Err("no on-screen window of this process".into())
}

/// Снимок окна `window` без рамки и тени: RGBA, строки сверху вниз.
pub(super) fn capture(window: u32) -> Result<Image, String> {
    // SAFETY: `CGRectNull` — константа CoreGraphics; снимок освобождает `Owned`.
    let image = Owned(unsafe { CGWindowListCreateImage(CGRectNull, INCLUDING_WINDOW, window, BOUNDS_IGNORE_FRAMING) });
    if image.0.is_null() {
        return Err(format!("CGWindowListCreateImage({window}) failed"));
    }
    // SAFETY: `image.0` — живой CGImage.
    let (width, height) = unsafe { (CGImageGetWidth(image.0), CGImageGetHeight(image.0)) };
    let (Ok(w), Ok(h)) = (u32::try_from(width), u32::try_from(height)) else { return Err(format!("capture {width}x{height}")) };
    if w == 0 || h == 0 {
        return Err("capture is empty".into());
    }
    let row = width.checked_mul(4).ok_or("capture row overflow")?;
    let mut rgba = vec![0u8; row.checked_mul(height).ok_or("capture size overflow")?];
    // SAFETY: буфер `rgba` — ровно `row × height` байт и живёт дольше контекста; контекст и пространство цветов
    // освобождает `Owned` (контекст — первым: объявлен позже).
    unsafe {
        let space = Owned(CGColorSpaceCreateDeviceRGB());
        let ctx = Owned(CGBitmapContextCreate(rgba.as_mut_ptr().cast(), width, height, 8, row, space.0, RGBA));
        if ctx.0.is_null() {
            return Err("CGBitmapContextCreate failed".into());
        }
        let rect = CGRect { origin: CGPoint { x: 0.0, y: 0.0 }, size: CGSize { width: width as f64, height: height as f64 } };
        CGContextDrawImage(ctx.0, rect, image.0);
    }
    // Окно непрозрачно: предумноженная альфа совпадает с обычной.
    Ok(Image { width: w, height: h, rgba })
}
