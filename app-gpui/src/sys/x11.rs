//! Linux (LIN-02): снимок окна для `--screens` — X11 `GetImage` клиентской области своим соединением с `$DISPLAY`
//! (в CI — Xvfb). Wayland: окно — через XWayland (`WAYLAND_DISPLAY` не задан, gpui выберет X11), иначе читаемый отказ.

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt, ImageFormat, ImageOrder};

use super::Image;

/// Номер окна X11 (`xcb_window_t`).
pub fn window_id(window: &gpui_kit::Window) -> Result<isize, String> {
    let handle = HasWindowHandle::window_handle(window).map_err(|e| format!("window handle: {e}"))?;
    match handle.as_raw() {
        RawWindowHandle::Xcb(h) => Ok(h.window.get() as isize),
        RawWindowHandle::Xlib(h) => isize::try_from(h.window).map_err(|e| e.to_string()),
        RawWindowHandle::Wayland(_) => Err("window capture needs X11: on Wayland run with WAYLAND_DISPLAY unset (XWayland)".into()),
        other => Err(format!("window capture: unsupported window handle {other:?}")),
    }
}

/// Снимок клиентской области окна `window`: RGBA сверху вниз.
pub fn capture(window: isize) -> Result<Image, String> {
    let window = u32::try_from(window).map_err(|e| format!("window id: {e}"))?;
    let (conn, _) = x11rb::connect(None).map_err(|e| format!("X11 connect: {e}"))?;
    let geo = conn.get_geometry(window).map_err(|e| e.to_string())?.reply().map_err(|e| format!("GetGeometry: {e}"))?;
    let (width, height) = (u32::from(geo.width), u32::from(geo.height));
    let image = conn
        .get_image(ImageFormat::Z_PIXMAP, window, 0, 0, geo.width, geo.height, !0)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| format!("GetImage: {e}"))?;
    let setup = conn.setup();
    let bpp = setup.pixmap_formats.iter().find(|f| f.depth == image.depth).map(|f| f.bits_per_pixel);
    if bpp != Some(32) || !matches!(image.depth, 24 | 32) {
        return Err(format!("GetImage: unsupported depth {} ({bpp:?} bits per pixel)", image.depth));
    }
    let rgba = to_rgba(&image.data, width, height, setup.image_byte_order == ImageOrder::MSB_FIRST)?;
    Ok(Image { width, height, rgba })
}

/// Показать (`MapWindow`) или спрятать (`UnmapWindow`) окно `window` своим соединением (трей, PAR-06).
pub fn set_mapped(window: isize, mapped: bool) -> Result<(), String> {
    let window = u32::try_from(window).map_err(|e| format!("window id: {e}"))?;
    let (conn, _) = x11rb::connect(None).map_err(|e| format!("X11 connect: {e}"))?;
    let cookie = if mapped { conn.map_window(window) } else { conn.unmap_window(window) };
    cookie.map_err(|e| e.to_string())?.check().map_err(|e| format!("map window: {e}"))
}

/// Пиксели `ZPixmap` 32 бита (`0x00RRGGBB` в порядке байтов сервера) → RGBA с непрозрачной альфой.
fn to_rgba(data: &[u8], width: u32, height: u32, msb: bool) -> Result<Vec<u8>, String> {
    let n = width as usize * height as usize;
    let Some(data) = data.get(..n * 4) else {
        return Err(format!("GetImage: {} bytes for {width}x{height}", data.len()));
    };
    let (px, _) = data.as_chunks::<4>();
    Ok(px
        .iter()
        .flat_map(|p| {
            let v = if msb { u32::from_be_bytes(*p) } else { u32::from_le_bytes(*p) };
            let [_, r, g, b] = v.to_be_bytes();
            [r, g, b, 0xFF]
        })
        .collect())
}

#[cfg(test)]
mod tests {
    /// Порядок байтов сервера: младший первый (Xvfb на x86) и старший первый — один и тот же цвет.
    #[test]
    fn pixels_to_rgba() {
        let le = [0x30, 0x20, 0x10, 0x00, 0xFF, 0xFF, 0xFF, 0x00];
        assert_eq!(super::to_rgba(&le, 2, 1, false).unwrap(), [0x10, 0x20, 0x30, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
        let be = [0x00, 0x10, 0x20, 0x30];
        assert_eq!(super::to_rgba(&be, 1, 1, true).unwrap(), [0x10, 0x20, 0x30, 0xFF]);
        assert!(super::to_rgba(&le, 2, 2, false).is_err());
    }
}
