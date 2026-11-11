//! Каталоги пользователя по ОС (MAC-01) — одна точка вместо разбросанных `LOCALAPPDATA`/`USERPROFILE`. Только
//! переменные окружения, без вызовов ОС.
//!
//! | | Данные | Журнал | Модули просмотра администратора |
//! |---|---|---|---|
//! | Windows | `%LOCALAPPDATA%\Postman` | `<данные>\logs` | `%ProgramFiles%\Postman\viewers` |
//! | macOS | `~/Library/Application Support/Postman` | `~/Library/Logs/Postman` | `/Library/Application Support/Postman/viewers` |
//! | прочие | `$XDG_DATA_HOME/Postman` или `~/.local/share/Postman` | `<данные>/logs` | — |
//!
//! Тесты и сценарии подменяют каталог данных переменной окружения корня (`LOCALAPPDATA` на Windows, `HOME` на macOS).

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
/// показывает «Консоль»); другой каталог данных (тесты, сценарии) — его `logs`, как на остальных ОС.
pub fn log_dir(data: &Path) -> PathBuf {
    if cfg!(target_os = "macos")
        && data == data_dir()
        && let Some(h) = home()
    {
        return h.join("Library").join("Logs").join(APP);
    }
    data.join("logs")
}

/// Подкаталог домашнего; нет домашнего — временный каталог.
fn in_home(name: &str) -> PathBuf {
    home().map(|h| h.join(name)).unwrap_or_else(std::env::temp_dir)
}

/// «Загрузки».
pub fn downloads() -> PathBuf {
    in_home("Downloads")
}

/// «Документы».
pub fn documents() -> PathBuf {
    in_home("Documents")
}

/// «Рабочий стол».
pub fn desktop() -> PathBuf {
    in_home("Desktop")
}

/// Каталог модулей просмотра, который ставит администратор (ADR 0007); на прочих ОС — нет.
pub fn system_viewers() -> Option<PathBuf> {
    if cfg!(windows) {
        var("ProgramFiles").map(|p| p.join(APP).join("viewers"))
    } else if cfg!(target_os = "macos") {
        Some(PathBuf::from("/Library/Application Support").join(APP).join("viewers"))
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
}
