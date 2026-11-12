//! Каталоги пользователя по ОС (MAC-01) — одна точка вместо разбросанных `LOCALAPPDATA`/`USERPROFILE`. Только
//! переменные окружения, без вызовов ОС.
//!
//! | | Данные | Журнал | Модули просмотра администратора |
//! |---|---|---|---|
//! | Windows | `%LOCALAPPDATA%\Postman` | `<данные>\logs` | `%ProgramFiles%\Postman\viewers` |
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
        var("XDG_DATA_HOME").or_else(|| home().map(|h| h.join(".local").join("share")))
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
        var("XDG_STATE_HOME").or_else(|| home().map(|h| h.join(".local").join("state"))).map(|s| s.join(APP))
    } else {
        None
    }
}

/// Каталог пользователя: Linux — из `xdg-user-dirs` (`key` — `DOWNLOAD`, `DOCUMENTS`, `DESKTOP`), иначе подкаталог
/// домашнего `name`; нет домашнего — временный каталог.
fn user_dir(key: &str, name: &str) -> PathBuf {
    let Some(home) = home() else { return std::env::temp_dir() };
    if cfg!(target_os = "linux")
        && let Some(config) = var("XDG_CONFIG_HOME").or_else(|| Some(home.join(".config")))
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

/// Каталог модулей просмотра, который ставит администратор (ADR 0007); на прочих ОС — нет.
pub fn system_viewers() -> Option<PathBuf> {
    if cfg!(windows) {
        var("ProgramFiles").map(|p| p.join(APP).join("viewers"))
    } else if cfg!(target_os = "macos") {
        Some(PathBuf::from("/Library/Application Support").join(APP).join("viewers"))
    } else if cfg!(target_os = "linux") {
        // Пакет deb/rpm (LIN-06) и администратор — в `/usr/share`, имя каталога — как у пакета.
        Some(PathBuf::from("/usr/share/postman/viewers"))
    } else {
        None
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
