//! Платформенный код интерфейса; `unsafe` — только здесь.

use gpui_kit::Window;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

#[cfg(windows)]
mod windows;

#[cfg(windows)]
mod console;

#[cfg(windows)]
mod memmap;

/// Считающий распределитель для замера кучи (фича `alloc-count`).
#[cfg(feature = "alloc-count")]
pub mod alloc;

/// Поля бенча в конце простоя (S4-08, `docs/MEMORY.md`): частный рабочий набор (столбец «Память» диспетчера
/// задач), куча Rust при фиче `alloc-count`. `POSTMAN_BENCH_MEMMAP=1` — ещё разбивка по модулям, файлам и блокам
/// частной памяти в stderr.
pub fn bench_idle_fields() -> String {
    let mut out = Vec::new();
    #[cfg(windows)]
    {
        let (text, private_ws) = memmap::report();
        out.push(format!("idle_private_ws_mb={:.0}", private_ws as f64 / 1_048_576.0));
        if std::env::var_os("POSTMAN_BENCH_MEMMAP").is_some() {
            eprint!("{text}");
        }
    }
    #[cfg(feature = "alloc-count")]
    out.push(alloc::fields());
    out.join(" ")
}

/// Вывод в консоль родителя, если stdout или stderr не перенаправлены (release без консоли, S4-01):
/// для `--bench`, `--screens`, `--help`, `--version`.
#[cfg(windows)]
pub fn attach_parent_console() {
    console::attach_parent();
}

#[cfg(not(windows))]
pub fn attach_parent_console() {}

/// Кодовая страница ANSI процесса (65001 — манифест exe действует).
#[cfg(all(test, windows))]
pub fn active_code_page() -> u32 {
    console::active_code_page()
}

/// Пароль учётной записи — в диспетчере учётных данных Windows (S3-04).
#[cfg(windows)]
pub mod credentials;

/// Другие ОС: хранилища секретов нет — вход только из окружения или каждый раз строкой входа.
#[cfg(not(windows))]
pub mod credentials {
    pub fn write(_target: &str, _user: &str, _secret: &str) -> Result<(), String> {
        Err("credential store is Windows only".into())
    }

    pub fn read(_target: &str) -> Result<Option<String>, String> {
        Ok(None)
    }

    pub fn delete(_target: &str) -> Result<(), String> {
        Ok(())
    }
}

/// Автозапуск, клиент `mailto:`, журнал в файл, открытие в Проводнике (S5-30).
#[cfg(windows)]
pub mod shell;

/// Другие ОС: ничего из этого нет.
#[cfg(not(windows))]
pub mod shell {
    use std::path::Path;

    pub fn set_autostart(_on: bool) -> Result<(), String> {
        Err("autostart is Windows only".into())
    }

    pub fn autostart() -> bool {
        false
    }

    pub fn register_mailto() -> Result<(), String> {
        Err("mailto registration is Windows only".into())
    }

    pub fn mailto_is_default() -> bool {
        false
    }

    pub fn policy(_name: &str) -> Option<String> {
        None
    }

    pub fn log_to_file(_path: &Path) -> bool {
        false
    }

    pub fn default_env(_name: &str, _value: &str) {}

    pub fn open(_target: &str) -> Result<(), String> {
        Err("open is Windows only".into())
    }

    pub fn reveal(_path: &Path) -> Result<(), String> {
        Err("reveal is Windows only".into())
    }

    pub fn notify(_title: &str, _body: &str) -> Result<(), String> {
        Err("notifications are Windows only".into())
    }
}

/// Снимок окна: RGBA, 8 бит на канал, строки сверху вниз.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Дескриптор окна числом — его можно передать в другой поток (сам `Window` туда не передаётся).
pub fn window_id(window: &Window) -> Result<isize, String> {
    match HasWindowHandle::window_handle(window).map_err(|e| format!("window handle: {e}"))?.as_raw() {
        RawWindowHandle::Win32(h) => Ok(h.hwnd.get()),
        other => Err(format!("window capture is not supported for {other:?}")),
    }
}

/// Снимок клиентской области окна. Вызывать не из потока окна.
#[cfg(windows)]
pub fn capture(window: isize) -> Result<Image, String> {
    windows::capture(window)
}

#[cfg(not(windows))]
pub fn capture(_window: isize) -> Result<Image, String> {
    Err("window capture is Windows only".into())
}

/// Свои значки окна, большой и малый (T1-01): заголовок, панель задач, Alt+Tab.
#[cfg(windows)]
pub fn set_icons(window: &Window) -> Result<(), String> {
    windows::set_icons(window_id(window)?)
}

#[cfg(not(windows))]
pub fn set_icons(_window: &Window) -> Result<(), String> {
    Ok(())
}

#[cfg(all(test, windows))]
mod tests {
    /// Манифест из build.rs действует, хотя gpui вшивает свой: кодовая страница процесса — UTF-8.
    #[test]
    fn manifest_is_applied() {
        assert_eq!(super::active_code_page(), 65001);
    }

    /// Разбивка памяти (S4-08): частный рабочий набор посчитан, категории — в отчёте.
    #[test]
    fn memory_map_is_reported() {
        let (text, private_ws) = super::memmap::report();
        assert!(private_ws > 0, "{text}");
        for line in ["mem image:", "mem mapped:", "mem private:", "mem total:"] {
            assert!(text.contains(line), "no {line}: {text}");
        }
        assert!(super::bench_idle_fields().starts_with("idle_private_ws_mb="));
    }
}
