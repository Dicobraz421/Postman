//! Платформенная изоляция: `spawn` — на стороне UI, `lockdown` — в самом воркере.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(crate) use linux::{MEMORY_LIMIT, lockdown, spawn};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::{MEMORY_LIMIT, lockdown, spawn};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(crate) use macos::{MEMORY_LIMIT, lockdown, spawn};

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
