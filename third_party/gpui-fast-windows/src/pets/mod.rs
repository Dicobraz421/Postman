//! Pets patch: платформенный код окна приложений Pets за нейтральным API — приложение не держит `windows`/`windows-sys`
//! и `unsafe` для окна. Решение владельца 2026-10-28: «всё платформозависимое — в выделенные модули»; без UI (реестр,
//! учётные данные, потоки процесса, память) — в `pets-shell`.
//!
//! `unsafe` — только в этих модулях, у каждого блока — `SAFETY`. Ошибки — `std::io::Error` (код и текст ОС), без
//! типов `windows` в сигнатурах; окно — `gpui::Window` или его номер ([`window::id`]) для другого потока.
//!
//! - [`drag`] — перетаскивание файлов из окна в другие приложения (OLE drag source): имя и байты или ленивый
//!   источник, запасная копия на диске во временном каталоге приложения;
//! - [`window`] — значки окна из ресурса exe, снимок клиентской области;
//! - [`toast`] — уведомление центра уведомлений от имени приложения без ярлыка в «Пуске».

pub mod drag;
pub mod toast;
pub mod window;

/// Строка UTF-16 с нулём в конце — для `PCWSTR`.
#[cfg(test)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Ошибка ОС с именем вызова: `GetLastError` потока.
fn last_error(what: &str) -> std::io::Error {
    let e = std::io::Error::last_os_error();
    std::io::Error::new(e.kind(), format!("{what}: {e}"))
}

/// Ошибка `windows` (HRESULT) — в `io::Error` с именем вызова.
fn com_error(what: &str, e: windows::core::Error) -> std::io::Error {
    std::io::Error::other(format!("{what}: {}", e.message()))
}
