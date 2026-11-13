//! Каталоги пользователя по ОС (MAC-01) — одна точка вместо разбросанных `LOCALAPPDATA`/`USERPROFILE`. Каталог
//! данных — из переменных окружения (их подменяют тесты); «Загрузки», «Документы», Program Files и системный каталог
//! Windows — известные папки ОС (`SHGetKnownFolderPath`, PAR-04 №36, №47, №48): переменную окружения подменить можно,
//! известную папку — нет, а «Загрузки» бывают перенесены (OneDrive, другой диск).
//!
//! | | Данные | Журнал | Модули просмотра администратора |
//! |---|---|---|---|
//! | Windows | `%LOCALAPPDATA%\Postman` | `<данные>\logs` | `<Program Files>\Postman\viewers` (`FOLDERID_ProgramFiles`) |
//! | macOS | `~/Library/Application Support/Postman` | `~/Library/Logs/Postman` | `/Library/Application Support/Postman/viewers` |
//! | Linux (LIN-02) | `$XDG_DATA_HOME/Postman` или `~/.local/share/Postman` | `$XDG_STATE_HOME/Postman` или `~/.local/state/Postman` | `/usr/share/postman/viewers` |
//! | прочие | как Linux | `<данные>/logs` | — |
//!
//! «Загрузки», «Документы», «Рабочий стол» на Linux — по `xdg-user-dirs` (`$XDG_CONFIG_HOME/user-dirs.dirs`, файл
//! читается, программа не запускается), иначе `~/Downloads`….
//!
//! Тесты и сценарии подменяют каталог данных переменной окружения корня (`LOCALAPPDATA` на Windows, `HOME` на macOS,
//! `XDG_DATA_HOME` на Linux).

use std::path::{Path, PathBuf};

/// Имя каталога приложения.
const APP: &str = "Postman";

fn var(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Каталог XDG из переменной: только абсолютный путь (спецификация XDG Base Directory: относительный — не задан,
/// PAR-04 №47).
fn xdg_var(name: &str) -> Option<PathBuf> {
    var(name).filter(|p| p.is_absolute())
}

/// Домашний каталог пользователя.
pub fn home() -> Option<PathBuf> {
    if cfg!(windows) { var("USERPROFILE") } else { var("HOME") }
}

/// Корень данных приложений пользователя (без имени приложения).
pub fn data_root() -> Option<PathBuf> {
    if cfg!(windows) {
        var("LOCALAPPDATA")
    } else if cfg!(target_os = "macos") {
        home().map(|h| h.join("Library").join("Application Support"))
    } else {
        xdg_var("XDG_DATA_HOME").or_else(|| home().map(|h| h.join(".local").join("share")))
    }
}

/// Каталог данных Postman: настройки, хранилище, вход, журнал (кроме macOS).
pub fn data_dir() -> PathBuf {
    data_root().unwrap_or_else(|| PathBuf::from(".")).join(APP)
}

/// Каталог журнала для каталога данных `data`. macOS: для обычного каталога данных — `~/Library/Logs/Postman` (его
/// показывает «Консоль»); Linux — `$XDG_STATE_HOME/Postman` (журнал — состояние, не данные, спецификация XDG Base
/// Directory). Другой каталог данных (тесты, сценарии) — его `logs`, как на Windows.
pub fn log_dir(data: &Path) -> PathBuf {
    if data == data_dir()
        && let Some(dir) = os_log_dir()
    {
        return dir;
    }
    data.join("logs")
}

/// Журнал по правилам ОС для обычного каталога данных; `None` — `<данные>/logs`.
fn os_log_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        home().map(|h| h.join("Library").join("Logs").join(APP))
    } else if cfg!(target_os = "linux") {
        xdg_var("XDG_STATE_HOME").or_else(|| home().map(|h| h.join(".local").join("state"))).map(|s| s.join(APP))
    } else {
        None
    }
}

/// `$XDG_CONFIG_HOME` (только абсолютный) или `~/.config` — Linux и прочие Unix.
pub fn config_home() -> Option<PathBuf> {
    xdg_var("XDG_CONFIG_HOME").or_else(|| home().map(|h| h.join(".config")))
}

/// Каталог пользователя: Linux — из `xdg-user-dirs` (`key` — `DOWNLOAD`, `DOCUMENTS`, `DESKTOP`), иначе подкаталог
/// домашнего `name`; нет домашнего — временный каталог.
fn user_dir(key: &str, name: &str) -> PathBuf {
    #[cfg(windows)]
    if let Some(dir) = known::folder(match key {
        "DOWNLOAD" => known::Folder::Downloads,
        "DOCUMENTS" => known::Folder::Documents,
        _ => known::Folder::Desktop,
    }) {
        return dir;
    }
    let Some(home) = home() else { return std::env::temp_dir() };
    if cfg!(target_os = "linux")
        && let Some(config) = config_home()
        && let Ok(text) = std::fs::read_to_string(config.join("user-dirs.dirs"))
        && let Some(dir) = xdg_user_dir(&text, key, &home)
    {
        return dir;
    }
    home.join(name)
}

/// Значение `XDG_<key>_DIR="…"` файла `user-dirs.dirs`: `$HOME/…` или абсолютный путь (так пишет `xdg-user-dirs-update`).
/// Каталог, равный домашнему, — «не задан» (так `xdg-user-dirs` отключает каталог).
fn xdg_user_dir(text: &str, key: &str, home: &Path) -> Option<PathBuf> {
    let name = format!("XDG_{key}_DIR=");
    let value = text.lines().map(str::trim).find_map(|l| l.strip_prefix(name.as_str()))?;
    let value = value.trim().strip_prefix('"')?.strip_suffix('"')?;
    let dir = match value.strip_prefix("$HOME") {
        Some(rest) => home.join(rest.trim_start_matches('/')),
        None if value.starts_with('/') => PathBuf::from(value),
        None => return None,
    };
    (dir != home).then_some(dir)
}

/// «Загрузки».
pub fn downloads() -> PathBuf {
    user_dir("DOWNLOAD", "Downloads")
}

/// «Документы».
pub fn documents() -> PathBuf {
    user_dir("DOCUMENTS", "Documents")
}

/// «Рабочий стол».
pub fn desktop() -> PathBuf {
    user_dir("DESKTOP", "Desktop")
}

/// Каталог модулей просмотра, который ставит администратор (ADR 0007); на прочих ОС — нет. Не из окружения
/// (PAR-04 №48): каталог пользователя не выдать за админский.
pub fn system_viewers() -> Option<PathBuf> {
    if cfg!(windows) {
        program_files().map(|p| p.join(APP).join("viewers"))
    } else if cfg!(target_os = "macos") {
        Some(PathBuf::from("/Library/Application Support").join(APP).join("viewers"))
    } else if cfg!(target_os = "linux") {
        // Пакет deb/rpm (LIN-06) и администратор — в `/usr/share`, имя каталога — как у пакета.
        Some(PathBuf::from("/usr/share/postman/viewers"))
    } else {
        None
    }
}

/// Program Files (Windows, `FOLDERID_ProgramFiles`); на других ОС — `None`.
pub fn program_files() -> Option<PathBuf> {
    #[cfg(windows)]
    return known::folder(known::Folder::ProgramFiles);
    #[cfg(not(windows))]
    None
}

/// Системная утилита ОС полным путём (PAR-04 №36): Windows — `System32\<name>` (`FOLDERID_System`) или
/// `Windows\<name>` (`explorer.exe`, `FOLDERID_Windows`); Unix — `/usr/bin/<name>`. Подмена утилиты из `PATH` или
/// каталога программы не срабатывает. Папку не узнать — `None` (вызов не делать).
pub fn system_tool(name: &str) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let in_windows = name.eq_ignore_ascii_case("explorer.exe");
        let dir = known::folder(if in_windows { known::Folder::Windows } else { known::Folder::System })?;
        Some(dir.join(name))
    }
    #[cfg(not(windows))]
    Some(Path::new("/usr/bin").join(name))
}

/// Известные папки Windows (`SHGetKnownFolderPath`).
#[cfg(windows)]
mod known {
    use std::os::windows::ffi::OsStringExt;
    use std::path::PathBuf;

    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{
        FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_ProgramFiles, FOLDERID_System, FOLDERID_Windows, KF_FLAG_DEFAULT,
        SHGetKnownFolderPath,
    };

    #[derive(Clone, Copy)]
    pub(super) enum Folder {
        Downloads,
        Documents,
        Desktop,
        ProgramFiles,
        System,
        Windows,
    }

    /// Путь известной папки этого пользователя; ошибка ОС — `None`.
    pub(super) fn folder(f: Folder) -> Option<PathBuf> {
        let id = match f {
            Folder::Downloads => &FOLDERID_Downloads,
            Folder::Documents => &FOLDERID_Documents,
            Folder::Desktop => &FOLDERID_Desktop,
            Folder::ProgramFiles => &FOLDERID_ProgramFiles,
            Folder::System => &FOLDERID_System,
            Folder::Windows => &FOLDERID_Windows,
        };
        let mut raw: windows_sys::core::PWSTR = std::ptr::null_mut();
        // SAFETY: `id` — живой GUID, `raw` — место для указателя; токен 0 — текущий пользователь. Строку выделяет
        // система, освобождается ниже `CoTaskMemFree` и при ошибке тоже (документация функции).
        let hr = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT as u32, std::ptr::null_mut(), &mut raw) };
        let path = (hr >= 0 && !raw.is_null()).then(|| {
            // SAFETY: при успехе `raw` — строка UTF-16 с нулём в конце.
            let len = (0..).take_while(|&i| unsafe { *raw.add(i) } != 0).count();
            // SAFETY: `len` символов до нуля прочитаны выше.
            let wide = unsafe { std::slice::from_raw_parts(raw, len) };
            PathBuf::from(std::ffi::OsString::from_wide(wide))
        });
        // SAFETY: `raw` выделен `SHGetKnownFolderPath` (или нулевой — тогда вызов ничего не делает).
        unsafe { CoTaskMemFree(raw.cast()) };
        path.filter(|p| p.is_absolute())
    }
}

/// Каталог модулей просмотра пользователя («сторонние»); нет корня данных — нет и каталога.
pub fn user_viewers() -> Option<PathBuf> {
    data_root().map(|r| r.join(APP).join("viewers"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_and_logs_are_per_os() {
        let data = data_dir();
        assert!(data.ends_with(APP), "{}", data.display());
        if cfg!(target_os = "macos") {
            assert!(data.ends_with("Library/Application Support/Postman"), "{}", data.display());
            assert!(log_dir(&data).ends_with("Library/Logs/Postman"));
        } else if cfg!(target_os = "linux") {
            assert_ne!(log_dir(&data), data.join("logs"));
            assert!(log_dir(&data).ends_with(APP));
            assert_eq!(system_viewers().as_deref(), Some(Path::new("/usr/share/postman/viewers")));
        } else {
            assert_eq!(log_dir(&data), data.join("logs"));
        }
        // Другой каталог данных — свой журнал на любой ОС.
        let other = Path::new("elsewhere");
        assert_eq!(log_dir(other), other.join("logs"));
        if home().is_some() {
            assert!(downloads().ends_with("Downloads"));
        }
    }

    /// PAR-04 №36, №48: системная утилита и Program Files — полным путём, не из окружения.
    #[test]
    fn system_paths_are_absolute() {
        if let Some(tool) = system_tool(if cfg!(windows) { "whoami.exe" } else { "klist" }) {
            assert!(tool.is_absolute(), "{}", tool.display());
        }
        if cfg!(windows) {
            assert!(program_files().is_some_and(|p| p.is_absolute()));
            assert!(system_tool("explorer.exe").is_some_and(|p| !p.parent().is_some_and(|d| d.ends_with("System32"))));
        } else {
            assert_eq!(program_files(), None);
        }
    }

    /// LIN-02: `user-dirs.dirs` — `$HOME/…`, абсолютный путь, комментарии; каталог, равный домашнему, и кривая
    /// строка — «не задан».
    #[test]
    fn xdg_user_dirs() {
        let home = Path::new("/home/anna");
        let text = "# комментарий\nXDG_DESKTOP_DIR=\"$HOME/Рабочий стол\"\nXDG_DOWNLOAD_DIR=\"/data/dl\"\n\
                    XDG_DOCUMENTS_DIR=\"$HOME/\"\nXDG_MUSIC_DIR=$HOME/Music\n";
        assert_eq!(xdg_user_dir(text, "DESKTOP", home), Some(home.join("Рабочий стол")));
        assert_eq!(xdg_user_dir(text, "DOWNLOAD", home), Some(PathBuf::from("/data/dl")));
        assert_eq!(xdg_user_dir(text, "DOCUMENTS", home), None);
        assert_eq!(xdg_user_dir(text, "MUSIC", home), None);
        assert_eq!(xdg_user_dir(text, "VIDEOS", home), None);
    }
}
