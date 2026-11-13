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

/// Linux (LIN-03): своя память из `/proc/self/smaps_rollup` (частные страницы и выгруженное) — ближайшее к частному
/// рабочему набору Windows, то же имя поля (ADR 0011 п. 3).
#[cfg(target_os = "linux")]
fn os_memory_field() -> Option<String> {
    let bytes = pets_shell::memory::phys_footprint()?;
    Some(format!("idle_private_ws_mb={:.0}", bytes as f64 / 1_048_576.0))
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
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
/// пользователя на этой машине; на macOS — в Связке ключей входа (MAC-02); на Linux — Secret Service (GNOME Keyring,
/// KWallet, LIN-03): службы нет — ошибка, вход каждый раз строкой. На диск приложения пароль не попадает; в журнал —
/// тоже (ошибки — только код ОС).
#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
pub mod credentials {
    use pets_shell::credentials as os;

    /// Имя записи без учёта регистра на всех ОС (PAR-04 №43): диспетчер Windows регистр не различает, Связка ключей и
    /// Secret Service — различают; адрес сервера в другом регистре — та же запись.
    fn key(target: &str) -> String {
        target.to_lowercase()
    }

    pub fn write(target: &str, user: &str, secret: &str) -> Result<(), String> {
        os::write(&key(target), user, secret).map_err(|e| e.to_string())
    }

    /// `Ok(None)` — такой записи нет. Запись прежнего имени (до PAR-04, в исходном регистре) — тоже читается.
    pub fn read(target: &str) -> Result<Option<String>, String> {
        let k = key(target);
        match os::read(&k).map_err(|e| e.to_string())? {
            None if k != target => os::read(target).map_err(|e| e.to_string()),
            found => Ok(found),
        }
    }

    /// Нет записи — не ошибка. Удаляет и запись прежнего имени.
    pub fn delete(target: &str) -> Result<(), String> {
        let k = key(target);
        if k != target {
            os::delete(target).map_err(|e| e.to_string())?;
        }
        os::delete(&k).map_err(|e| e.to_string())
    }
}

/// Другие ОС: хранилища секретов нет — вход только из окружения или каждый раз строкой входа.
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
pub mod credentials {
    pub fn write(_target: &str, _user: &str, _secret: &str) -> Result<(), String> {
        Err("no credential store on this OS".into())
    }

    pub fn read(_target: &str) -> Result<Option<String>, String> {
        Ok(None)
    }

    pub fn delete(_target: &str) -> Result<(), String> {
        Ok(())
    }
}

/// Linux (LIN-02): шрифт интерфейса по fontconfig — семейство `sans-serif` среды (`fc-match`; GNOME — Cantarell или
/// Adwaita Sans, KDE — Noto Sans, Ubuntu — Ubuntu), а не первое из запасного списка gpui. Семейства нет среди шрифтов
/// gpui или `fc-match` не ответил — `None` (gpui выберет сам). Один раз при старте, единицы миллисекунд.
#[cfg(target_os = "linux")]
pub fn ui_font(installed: &[String]) -> Option<String> {
    let out = match system_command("fc-match").and_then(|mut c| c.args(["-f", "%{family[0]}", "sans-serif"]).output().map_err(|e| e.to_string())) {
        Ok(o) if o.status.success() => o.stdout,
        Ok(o) => {
            tracing::warn!("fonts: fc-match failed ({}), UI font is chosen by gpui", o.status);
            return None;
        }
        Err(e) => {
            tracing::warn!("fonts: fc-match not run, UI font is chosen by gpui: {e}");
            return None;
        }
    };
    let family = String::from_utf8_lossy(&out).trim().to_owned();
    if !installed.contains(&family) {
        tracing::warn!(family, "fonts: fontconfig sans-serif is not among gpui fonts, UI font is chosen by gpui");
        return None;
    }
    tracing::debug!(family, "fonts: UI font from fontconfig");
    Some(family)
}

/// Системная утилита полным путём (PAR-04 №36, `postman_core::paths::system_tool`): Windows — `System32`/`Windows`,
/// Unix — `/usr/bin`; подмена утилиты из `PATH` или каталога программы не срабатывает.
#[cfg_attr(target_os = "macos", allow(dead_code, reason = "macOS shell spells absolute paths itself"))]
pub fn system_command(name: &str) -> Result<std::process::Command, String> {
    postman_core::paths::system_tool(name).map(std::process::Command::new).ok_or_else(|| format!("{name}: system folder not found"))
}

/// HTML из буфера обмена (Word, браузер, T3-06): фрагмент `CF_HTML` — `pets_shell::clipboard`. Нет HTML, он больше
/// `max` байт или буфер занят — `None` (вставится текст).
pub fn clipboard_html(max: usize) -> Option<String> {
    match pets_shell::clipboard::read_html(max) {
        Ok(html) => html.map(|h| h.fragment),
        Err(e) => {
            tracing::warn!("clipboard: HTML not read, pasting as text: {e}");
            None
        }
    }
}

/// Значение политики организации с типом, как его задал администратор (POL-01): реестр (`REG_SZ`, `REG_DWORD`,
/// `REG_QWORD`) или профиль конфигурации macOS (строка, логическое, целое, дробное). Разбор — `security::policy_*`.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(windows, allow(dead_code, reason = "дробное и логическое бывают только в профиле macOS и файле Linux"))]
pub enum PolicyValue {
    Text(String),
    Int(i64),
    Real(f64),
    Bool(bool),
    /// Тип, которого политики не бывает (`REG_BINARY`, массив…): имя типа — для журнала.
    Other(&'static str),
}

/// Автозапуск, клиент `mailto:`, журнал в файл, открытие в Проводнике (S5-30).
#[cfg(windows)]
pub mod shell;

/// macOS (MAC-02): клиент `mailto:` (LaunchServices), журнал в файл, Finder, уведомления.
#[cfg(target_os = "macos")]
#[path = "shell_macos.rs"]
pub mod shell;

/// Linux (LIN-03): стандарты freedesktop — `.desktop`, `xdg-open`, D-Bus, политики в `/etc`.
#[cfg(target_os = "linux")]
#[path = "shell_linux.rs"]
pub mod shell;

/// Linux (LIN-03): тёмная тема и акцент среды (portal).
#[cfg(target_os = "linux")]
#[path = "appearance_linux.rs"]
pub mod appearance;

/// Другие ОС: ничего из этого нет.
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
pub mod shell {
    use std::path::Path;

    pub const MAILTO_HINT: &str = "settings-mailto-hint-confirm";

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

    pub fn domain() -> Option<String> {
        None
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

    pub fn policy(_name: &str) -> Option<super::PolicyValue> {
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

    /// Копия файла для `CF_HDROP` (в фоне, до начала перетаскивания): свой подкаталог на каждое перетаскивание. Метка
    /// «из Интернета» — `mark_from_internet` (pets PAR-07): не записалась — ошибка, перетаскивания нет (PAR-04 №11).
    pub fn prepare(copies: &TempCopies, name: &str, data: &[u8]) -> std::io::Result<PathBuf> {
        let path = copies.slot()?.join(postman_core::files::safe_name(name));
        postman_core::files::write(&path, data)?;
        os::mark_from_internet(&path)?;
        Ok(path)
    }

    /// Удалить копии (старт и выход).
    pub fn clean(copies: &TempCopies) {
        if let Err(e) = copies.clean() {
            tracing::warn!(path = %copies.root().display(), "drag: drag copies not removed: {e}");
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
            tracing::warn!(path = %copies.root().display(), "drag: drag copies not removed: {e}");
        }
    }

    /// Начать перетаскивание копии (поток окна, во время движения мыши с нажатой кнопкой).
    pub fn drag(copy: &Path) -> Result<(), String> {
        os::drag_out(&[copy.to_path_buf()]).map_err(|e| e.to_string())
    }
}

/// Перетаскивание вложения в файловый менеджер на Linux (INT-01): `gpui_linux::pets::drag` — gpui сам ведёт сеанс
/// (`on_drag` + `external_drag_payload`: X11 XDND, Wayland `wl_data_device`), отдаёт получателю пути копий. Копия — в
/// `$TMPDIR/Postman/drag`, с безопасным именем, готовится заранее (как на macOS); без копии — не тащим (PAR-04 №11).
#[cfg(target_os = "linux")]
pub mod drag {
    use std::path::{Path, PathBuf};

    use gpui_linux::pets::drag as os;
    pub use gpui_linux::pets::drag::TempCopies;

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
            tracing::warn!(path = %copies.root().display(), "drag: drag copies not removed: {e}");
        }
    }

    /// Содержимое для получателя за окном: путь копии.
    pub fn payload(copy: &Path) -> gpui_kit::ExternalDragPayload {
        os::payload(&[copy.to_path_buf()])
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

/// Linux (PAR-06): спрятать окно в трей — X11 `UnmapWindow` (окна нет ни на экране, ни на панели задач); Wayland —
/// спрятать нельзя, окно сворачивается.
#[cfg(target_os = "linux")]
pub fn hide_window(window: &Window) -> Result<(), String> {
    match x11::window_id(window) {
        Ok(id) => x11::set_mapped(id, false),
        Err(_) => {
            window.minimize_window();
            Ok(())
        }
    }
}

/// Linux: вернуть спрятанное окно (`MapWindow`); Wayland — его выведет `activate_window`.
#[cfg(target_os = "linux")]
pub fn show_window(window: &Window) -> Result<(), String> {
    match x11::window_id(window) {
        Ok(id) => x11::set_mapped(id, true),
        Err(_) => Ok(()),
    }
}

/// Linux (PAR-06): значок в трее — StatusNotifierItem.
#[cfg(target_os = "linux")]
#[path = "tray_linux.rs"]
pub mod tray;

/// Снимок окна: RGBA, 8 бит на канал, строки сверху вниз.
#[cfg(windows)]
pub use gpui_windows::pets::window::Image;

/// Снимок окна: RGBA, 8 бит на канал, строки сверху вниз.
#[cfg(target_os = "macos")]
pub use pets_gpui_macos::window::Image;

/// Linux (LIN-02): снимок окна X11.
#[cfg(target_os = "linux")]
mod x11;

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

/// Linux: номер окна X11 (LIN-02); окно Wayland — отказ.
#[cfg(target_os = "linux")]
pub use x11::window_id;

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
pub fn window_id(_window: &Window) -> Result<isize, String> {
    Err("window id is not supported on this OS".into())
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

/// Linux: X11 `GetImage` (LIN-02).
#[cfg(target_os = "linux")]
pub use x11::capture;

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
pub fn capture(_window: isize) -> Result<Image, String> {
    Err("window capture is not supported on this OS".into())
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

/// macOS (MAC-07): бейдж на значке в Dock (`NSDockTile`); `None` — убрать. Только из потока интерфейса.
#[cfg(target_os = "macos")]
pub fn set_dock_badge(text: Option<&str>) -> Result<(), String> {
    pets_gpui_macos::dock::set_badge_label(text).map_err(|e| e.to_string())
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
