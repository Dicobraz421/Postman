//! Платформенный код интерфейса — прослойка над нейтральным API pets (решение владельца 2026-10-28): окно —
//! `gpui_windows::pets` (pets-gpui-windows) и `pets_gpui_macos` (MAC-02), без UI — `pets_shell`. Вызовов ОС здесь нет,
//! на других ОС — заглушки. `unsafe` — только счётчик кучи (`alloc`) и `set_var` в начале `main`
//! (`shell::default_env`).
//!
//! Тексты интерфейса, где нарисованы клавиши ОС, на macOS — свой вариант ключа ([`os_key`], MAC-04).

use gpui_kit::Window;
#[cfg(windows)]
use gpui_windows::pets;

/// Считающий распределитель для замера кучи (фича `alloc-count`).
#[cfg(feature = "alloc-count")]
pub mod alloc;

/// Поля бенча в конце простоя (S4-08, `docs/MEMORY.md`): частный рабочий набор (столбец «Память» диспетчера
/// задач; macOS — `phys_footprint`, «Память» Мониторинга системы), куча Rust при фиче `alloc-count`. `POSTMAN_BENCH_MEMMAP=1` — ещё разбивка по модулям, файлам и блокам
/// частной памяти в stderr.
pub fn bench_idle_fields() -> String {
    [os_memory_field(), heap_field()].into_iter().flatten().collect::<Vec<String>>().join(" ")
}

#[cfg(windows)]
fn os_memory_field() -> Option<String> {
    let (text, private_ws) = pets_shell::memory::report();
    if std::env::var_os("POSTMAN_BENCH_MEMMAP").is_some() {
        eprint!("{text}");
    }
    Some(format!("idle_private_ws_mb={:.0}", private_ws as f64 / 1_048_576.0))
}

/// macOS (MAC-04): `phys_footprint` — столбец «Память» Мониторинга системы (аналог частного рабочего набора).
#[cfg(target_os = "macos")]
fn os_memory_field() -> Option<String> {
    let bytes = pets_shell::memory::phys_footprint()?;
    Some(format!("idle_phys_footprint_mb={:.0}", bytes as f64 / 1_048_576.0))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn os_memory_field() -> Option<String> {
    None
}

#[cfg(feature = "alloc-count")]
fn heap_field() -> Option<String> {
    Some(alloc::fields())
}

#[cfg(not(feature = "alloc-count"))]
fn heap_field() -> Option<String> {
    None
}

/// Вывод в консоль родителя, если stdout или stderr не перенаправлены (release без консоли, S4-01):
/// для `--bench`, `--screens`, `--help`, `--version`.
pub fn attach_parent_console() {
    #[cfg(windows)]
    pets_shell::console::attach_parent();
}

/// Тексты со своим вариантом на macOS (MAC-04): только там, где на Mac другие клавиши (⌘ вместо Ctrl, Option вместо
/// Alt). Названий ОС в текстах нет (решение 2026-11-12 «no-os-names»): «системное хранилище паролей», «Как в
/// системе». Вариант — тот же ключ с `-mac`.
pub const MAC_TEXTS: &[(&str, &str)] = &[
    ("bulk-pick", "bulk-pick-mac"),
    ("signin-demo", "signin-demo-mac"),
    ("settings-reading-aside-1", "settings-reading-aside-1-mac"),
    ("settings-keys-layout-1", "settings-keys-layout-1-mac"),
    ("settings-keys-layout-2", "settings-keys-layout-2-mac"),
    ("settings-keys-note-outlook", "settings-keys-note-outlook-mac"),
    ("settings-keytips", "settings-keytips-mac"),
];

/// Ключ текста для этой ОС: на macOS — вариант из [`MAC_TEXTS`], иначе — `key` как есть.
pub fn os_key(key: &str) -> &str {
    os_key_on(key, cfg!(target_os = "macos"))
}

/// [`os_key`] для заданной ОС (тесты).
pub fn os_key_on(key: &str, mac: bool) -> &str {
    if !mac {
        return key;
    }
    MAC_TEXTS.iter().find(|(k, _)| *k == key).map_or(key, |(_, m)| m)
}

/// Пароль учётной записи — в диспетчере учётных данных Windows (S3-04), тип «общие учётные данные», для этого
/// пользователя на этой машине; на macOS — в Связке ключей входа (MAC-02). На диск приложения пароль не попадает;
/// в журнал — тоже (ошибки — только код ОС).
#[cfg(any(windows, target_os = "macos"))]
pub mod credentials {
    use pets_shell::credentials as os;

    pub fn write(target: &str, user: &str, secret: &str) -> Result<(), String> {
        os::write(target, user, secret).map_err(|e| e.to_string())
    }

    /// `Ok(None)` — такой записи нет.
    pub fn read(target: &str) -> Result<Option<String>, String> {
        os::read(target).map_err(|e| e.to_string())
    }

    /// Нет записи — не ошибка.
    pub fn delete(target: &str) -> Result<(), String> {
        os::delete(target).map_err(|e| e.to_string())
    }
}

/// Другие ОС: хранилища секретов нет — вход только из окружения или каждый раз строкой входа.
#[cfg(not(any(windows, target_os = "macos")))]
pub mod credentials {
    pub fn write(_target: &str, _user: &str, _secret: &str) -> Result<(), String> {
        Err("credential store is Windows and macOS only".into())
    }

    pub fn read(_target: &str) -> Result<Option<String>, String> {
        Ok(None)
    }

    pub fn delete(_target: &str) -> Result<(), String> {
        Ok(())
    }
}

/// HTML из буфера обмена (Word, браузер, T3-06): фрагмент `CF_HTML` — `pets_shell::clipboard`. Нет HTML, он больше
/// `max` байт или буфер занят — `None` (вставится текст).
pub fn clipboard_html(max: usize) -> Option<String> {
    match pets_shell::clipboard::read_html(max) {
        Ok(html) => html.map(|h| h.fragment),
        Err(e) => {
            tracing::warn!("clipboard: HTML не прочитан, вставка текстом: {e}");
            None
        }
    }
}

/// Автозапуск, клиент `mailto:`, журнал в файл, открытие в Проводнике (S5-30).
#[cfg(windows)]
pub mod shell;

/// macOS (MAC-02): клиент `mailto:` (LaunchServices), журнал в файл, Finder, уведомления.
#[cfg(target_os = "macos")]
#[path = "shell_macos.rs"]
pub mod shell;

/// Другие ОС: ничего из этого нет.
#[cfg(not(any(windows, target_os = "macos")))]
pub mod shell {
    use std::path::Path;

    pub fn bitlocker(_path: &Path) -> (String, Option<bool>) {
        (String::new(), None)
    }

    pub fn open_bitlocker() -> Result<(), String> {
        Err("BitLocker is Windows only".into())
    }

    pub fn upn() -> Option<String> {
        None
    }

    pub fn in_domain() -> bool {
        false
    }

    pub fn set_autostart(_on: bool) -> Result<(), String> {
        Err("autostart is Windows only".into())
    }

    pub fn autostart() -> bool {
        false
    }

    pub fn register_mailto() -> Result<(), String> {
        Err("mailto registration is Windows only".into())
    }

    pub fn show_default_apps() -> Result<(), String> {
        Err("default apps are Windows and macOS only".into())
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

    pub fn notify_with(_title: &str, _body: &str, _sound: bool) -> Result<(), String> {
        Err("notifications are Windows only".into())
    }
}

/// Перетаскивание вложения в другое приложение (T1-18): OLE drag source `gpui_windows::pets::drag`. Что тащить
/// (без макросов и исполняемых) — решает `attachments::draggable`; копия для `CF_HDROP` — в `%TEMP%\Postman\drag`,
/// с безопасным именем и меткой «из Интернета», как у «Сохранить как».
#[cfg(windows)]
pub mod drag {
    use std::path::PathBuf;
    use std::sync::Arc;

    use gpui_windows::pets::drag::{self as os, DragFile, Outcome, TempCopies};

    /// Каталог копий: `%TEMP%\Postman\drag`.
    pub fn copies() -> TempCopies {
        TempCopies::new("Postman")
    }

    /// Порог начала перетаскивания в точках окна (системный).
    pub fn threshold(scale: f32) -> (f32, f32) {
        os::threshold(scale)
    }

    /// Копия файла для `CF_HDROP` (в фоне, до начала перетаскивания): свой подкаталог на каждое перетаскивание.
    pub fn prepare(copies: &TempCopies, name: &str, data: &[u8]) -> std::io::Result<PathBuf> {
        let path = copies.slot()?.join(postman_core::files::safe_name(name));
        postman_core::files::write(&path, data)?;
        Ok(path)
    }

    /// Удалить копии (старт и выход).
    pub fn clean(copies: &TempCopies) {
        if let Err(e) = copies.clean() {
            tracing::warn!(path = %copies.root().display(), "drag: копии перетаскивания не удалены: {e}");
        }
    }

    /// Перетащить файл (поток окна, модальный цикл OLE — вне `update`). `Ok(true)` — бросили, `Ok(false)` — отменили.
    pub fn drag(name: &str, data: Arc<Vec<u8>>, copy: Option<PathBuf>) -> Result<bool, String> {
        let file = DragFile::bytes(name, data);
        let file = match copy {
            Some(path) => file.with_copy(path),
            None => file,
        };
        os::drag_out(vec![file]).map(|o| o == Outcome::Dropped).map_err(|e| e.to_string())
    }
}

/// Перетаскивание вложения в Finder и другие приложения (MAC-02): `pets_gpui_macos::drag` (`NSDraggingSession`). Копия
/// файла — в `$TMPDIR/Postman/drag`, с безопасным именем и меткой карантина, как у «Сохранить как». Сеанс не
/// модальный: начинается в событии мыши и идёт дальше сам.
#[cfg(target_os = "macos")]
pub mod drag {
    use std::path::{Path, PathBuf};

    use pets_gpui_macos::drag as os;
    pub use pets_gpui_macos::drag::TempCopies;

    /// Каталог копий: `$TMPDIR/Postman/drag`.
    pub fn copies() -> TempCopies {
        TempCopies::new("Postman")
    }

    /// Порог начала перетаскивания в точках окна.
    pub fn threshold(scale: f32) -> (f32, f32) {
        os::threshold(scale)
    }

    /// Копия файла для перетаскивания: свой подкаталог на каждое перетаскивание.
    pub fn prepare(copies: &TempCopies, name: &str, data: &[u8]) -> std::io::Result<PathBuf> {
        let path = copies.slot()?.join(postman_core::files::safe_name(name));
        postman_core::files::write(&path, data)?;
        Ok(path)
    }

    /// Удалить копии (старт и выход).
    pub fn clean(copies: &TempCopies) {
        if let Err(e) = copies.clean() {
            tracing::warn!(path = %copies.root().display(), "drag: копии перетаскивания не удалены: {e}");
        }
    }

    /// Начать перетаскивание копии (поток окна, во время движения мыши с нажатой кнопкой).
    pub fn drag(copy: &Path) -> Result<(), String> {
        os::drag_out(&[copy.to_path_buf()]).map_err(|e| e.to_string())
    }
}

/// Спрятать окно в трей (T3-13): с экрана и с панели задач.
#[cfg(windows)]
pub fn hide_window(window: &Window) -> Result<(), String> {
    pets::window::hide(window).map_err(|e| e.to_string())
}

/// Показать спрятанное или свёрнутое окно.
#[cfg(windows)]
pub fn show_window(window: &Window) -> Result<(), String> {
    pets::window::show(window).map_err(|e| e.to_string())
}

/// Снимок окна: RGBA, 8 бит на канал, строки сверху вниз.
#[cfg(windows)]
pub use gpui_windows::pets::window::Image;

/// Снимок окна: RGBA, 8 бит на канал, строки сверху вниз.
#[cfg(target_os = "macos")]
pub use pets_gpui_macos::window::Image;

/// Снимок окна: RGBA, 8 бит на канал, строки сверху вниз.
#[cfg(not(any(windows, target_os = "macos")))]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Дескриптор окна числом — его можно передать в другой поток (сам `Window` туда не передаётся).
#[cfg(windows)]
pub fn window_id(window: &Window) -> Result<isize, String> {
    pets::window::id(window).map_err(|e| e.to_string())
}

/// macOS: номер окна (`CGWindowID`, `-[NSWindow windowNumber]`).
#[cfg(target_os = "macos")]
pub fn window_id(window: &Window) -> Result<isize, String> {
    pets_gpui_macos::window::id(window).map_err(|e| e.to_string())
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn window_id(_window: &Window) -> Result<isize, String> {
    Err("window id is Windows only".into())
}

/// Снимок клиентской области окна. Вызывать не из потока окна.
#[cfg(windows)]
pub fn capture(window: isize) -> Result<Image, String> {
    pets::window::capture(window).map_err(|e| e.to_string())
}

#[cfg(target_os = "macos")]
pub fn capture(window: isize) -> Result<Image, String> {
    pets_gpui_macos::window::capture(window).map_err(|e| e.to_string())
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn capture(_window: isize) -> Result<Image, String> {
    Err("window capture is Windows only".into())
}

/// Значок приложения в ресурсах exe (`build.rs`: `1 ICON res/postman.ico`).
#[cfg(windows)]
const ICON_ID: u16 = 1;

/// Свои значки окна, большой и малый (T1-01): заголовок, панель задач, Alt+Tab — каждый своего размера из группы
/// значков exe.
#[cfg(windows)]
pub fn set_icons(window: &Window) -> Result<(), String> {
    pets::window::set_icons(window, ICON_ID).map_err(|e| e.to_string())
}

#[cfg(not(windows))]
pub fn set_icons(_window: &Window) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod os_texts {
    /// MAC-04: у каждого текста macOS есть перевод на обоих языках, у исходного ключа — тоже; на Windows ключ не
    /// меняется.
    #[test]
    fn mac_texts_are_translated() {
        let mut texts = crate::parts::texts();
        texts.push(pets_ui::FTL);
        for lang in ["ru", "en"] {
            let (i18n, _) = pets_i18n::I18n::new(&texts, lang);
            for (key, mac) in super::MAC_TEXTS {
                assert_ne!(i18n.get(key), *key, "{lang}: {key}");
                assert_ne!(i18n.get(mac), *mac, "{lang}: {mac}");
            }
        }
        assert_eq!(super::os_key_on("bulk-pick", false), "bulk-pick");
        assert_eq!(super::os_key_on("bulk-pick", true), "bulk-pick-mac");
        assert_eq!(super::os_key_on("nav-mail", true), "nav-mail");
    }

    /// Решение 2026-11-12 «no-os-names»: в текстах интерфейса нет названий ОС и их хранилищ — «система», «системное
    /// хранилище паролей». Комментарии `.ftl` не в счёт.
    #[test]
    fn texts_have_no_os_names() {
        const NAMES: [&str; 9] =
            ["Windows", "macOS", "BitLocker", "FileVault", "Keychain", "Связк", "Credential Manager", "Диспетчер учётных", "Проводник"];
        let mut texts = crate::parts::texts();
        texts.push(pets_ui::FTL);
        let mut found = Vec::new();
        for set in &texts {
            for (lang, source) in set.iter() {
                for line in source.lines().filter(|l| !l.trim_start().starts_with('#')) {
                    if let Some(name) = NAMES.iter().find(|n| line.contains(*n)) {
                        found.push(format!("{lang}: {name}: {line}"));
                    }
                }
            }
        }
        assert!(
            found.is_empty(),
            "названия ОС в текстах:
{}",
            found.join(
                "
"
            )
        );
    }
}

#[cfg(all(test, windows))]
mod tests {
    use gpui_windows::pets;

    /// Манифест из build.rs действует, хотя gpui вшивает свой: кодовая страница процесса — UTF-8.
    #[test]
    fn manifest_is_applied() {
        assert_eq!(pets_shell::console::active_code_page(), 65001);
    }

    /// T1-01: в exe — значок «Ленты» всех размеров `res/postman.ico` (тесты собраны с теми же ресурсами, `build.rs`).
    #[test]
    fn exe_icon_has_all_sizes() {
        assert_eq!(pets::window::icon_sizes(super::ICON_ID).unwrap(), [16, 20, 24, 32, 40, 48, 64, 256]);
    }

    /// Разбивка памяти (S4-08): частный рабочий набор посчитан.
    #[test]
    fn memory_is_reported() {
        assert!(super::bench_idle_fields().starts_with("idle_private_ws_mb="));
    }

    /// T1-18: копия для `CF_HDROP` — с безопасным именем в своём подкаталоге; чистка убирает всё.
    #[test]
    fn drag_copy_is_removed() {
        let root = tempfile::Builder::new().prefix("postman-").tempdir().unwrap();
        let copies = pets::drag::TempCopies::at(root.path().join("drag"));
        let p = super::drag::prepare(&copies, "a/b:c.pdf", b"x").unwrap();
        assert!(p.starts_with(copies.root()) && p.is_file());
        assert_eq!(std::fs::read(&p).unwrap(), b"x");
        super::drag::clean(&copies);
        assert!(!copies.root().exists());
        super::drag::clean(&copies);
    }
}
