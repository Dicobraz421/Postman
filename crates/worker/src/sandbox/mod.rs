//! Платформенная изоляция: `spawn` — на стороне UI, `lockdown` — в самом воркере.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(crate) use linux::{MEMORY_LIMIT, isolate, lockdown, spawn};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::{MEMORY_LIMIT, lockdown, spawn};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::Limited;
#[cfg(all(target_os = "macos", debug_assertions))]
pub(crate) use macos::probe;
#[cfg(target_os = "macos")]
pub(crate) use macos::{MEMORY_LIMIT, lockdown, spawn};

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
pub(crate) const MEMORY_LIMIT: usize = 1 << 30;

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub(crate) fn spawn(_: &std::path::Path, _relaxed: bool, _memory: usize) -> std::io::Result<crate::Process> {
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
