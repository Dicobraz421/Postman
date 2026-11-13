//! Платформенная изоляция: `spawn` — на стороне UI, `lockdown` — в самом воркере.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(all(target_os = "linux", debug_assertions))]
pub(crate) use linux::probe_fd;
#[cfg(target_os = "linux")]
pub(crate) use linux::{MEMORY_LIMIT, isolate, lockdown, parent, spawn};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::{MEMORY_LIMIT, lockdown, parent, remove_profile, spawn};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::Limited;
#[cfg(all(target_os = "macos", debug_assertions))]
pub(crate) use macos::probe;
#[cfg(all(target_os = "macos", debug_assertions))]
pub(crate) use macos::probe_fd;
#[cfg(target_os = "macos")]
pub(crate) use macos::{MEMORY_LIMIT, lockdown, parent, spawn};

/// Профиль песочницы на диске есть только на Windows (AppContainer); на остальных платформах удалять нечего.
#[cfg(not(windows))]
pub(crate) fn remove_profile() -> Result<(), String> {
    Ok(())
}

/// Шаг изоляции, которому нужен однопоточный процесс (Linux: namespaces); на остальных платформах его нет.
#[cfg(not(target_os = "linux"))]
pub(crate) fn isolate() -> Result<(), String> {
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub(crate) fn lockdown() -> Result<(), String> {
    Err("платформа не поддерживается".into())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub(crate) fn parent() -> u32 {
    0
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub(crate) const MEMORY_LIMIT: usize = 1 << 30;

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub(crate) fn spawn(_: &std::path::Path, _memory: usize) -> std::io::Result<crate::Process> {
    Err(std::io::Error::other("платформа не поддерживается"))
}

/// Что удалось включить при запуске воркера (для статус-строки и логов).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    None,
    /// Все механизмы платформы.
    Full,
    /// Часть механизмов недоступна (например, нет AppContainer-профиля или user namespaces).
    Partial,
}

/// Нужны ли процессу песочницы отдельные user/net/ipc namespaces (Linux, ADR 0010). Политику администратора
/// (`SandboxNamespaces`) читает приложение (LIN-03) и передаёт клиенту ([`crate::Client::namespaces`]); на остальных
/// платформах режим не используется.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Namespaces {
    /// Дистрибутив запретил — работаем без них (WARN в журнале); Landlock, seccomp, rlimits остаются обязательными.
    #[default]
    Optional,
    /// Без namespaces процесс не работает.
    Required,
}

impl Namespaces {
    /// Значение аргумента `--namespaces=…`.
    pub fn as_arg(self) -> &'static str {
        match self {
            Self::Optional => "optional",
            Self::Required => "required",
        }
    }

    pub fn from_arg(v: &str) -> Option<Self> {
        match v {
            "optional" => Some(Self::Optional),
            "required" => Some(Self::Required),
            _ => None,
        }
    }
}

/// Дескрипторы по номерам — только Unix; на Windows наследование задаёт `HANDLE_LIST` (stdin, stdout, stderr).
#[cfg(all(debug_assertions, not(unix)))]
pub(crate) fn probe_fd(_fd: i32) -> Result<(), String> {
    Err("только Unix".into())
}
