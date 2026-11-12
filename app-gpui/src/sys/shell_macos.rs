//! Связь с macOS для «Настройки → Общие» и окна (MAC-02): клиент `mailto:` по умолчанию (LaunchServices), журнал
//! в файл (`~/Library/Logs/Postman` — путь даёт `postman_core::paths::log_dir`), Finder («Открыть», «Показать в
//! Finder»), уведомления центра уведомлений. Вызовы ОС — `pets_shell` и `pets_gpui_macos`; здесь — что делает Postman.
//! Автозапуск, политики, сведения о домене и FileVault — MAC-04.

use std::path::Path;

use pets_gpui_macos::toast;

/// Схема ссылок почты (объявлена в `Info.plist`, `scripts/macos/package.sh`).
const MAILTO: &str = "mailto";

/// Шифрование диска: FileVault — MAC-04; пока «не известно».
pub fn bitlocker(_path: &Path) -> (String, Option<bool>) {
    (String::new(), None)
}

pub fn open_bitlocker() -> Result<(), String> {
    Err("disk encryption settings on macOS: MAC-04".into())
}

/// Вход Windows (UPN) на Mac не бывает; Kerberos — MAC-05.
pub fn upn() -> Option<String> {
    None
}

pub fn in_domain() -> bool {
    false
}

/// Автозапуск (`SMAppService`) — MAC-04.
pub fn set_autostart(_on: bool) -> Result<(), String> {
    Err("autostart on macOS: MAC-04".into())
}

pub fn autostart() -> bool {
    false
}

/// Сделать Postman приложением по умолчанию для `mailto:`; macOS сама спросит подтверждение.
pub fn register_mailto() -> Result<(), String> {
    pets_shell::url_scheme::set_default(MAILTO).map_err(|e| e.to_string())
}

/// На macOS выбор делает `register_mailto` (с подтверждением системы): «Приложения по умолчанию» открывать не нужно.
pub fn show_default_apps() -> Result<(), String> {
    Ok(())
}

/// Ссылки `mailto:` открывает Postman.
pub fn mailto_is_default() -> bool {
    pets_shell::url_scheme::is_default(MAILTO)
}

/// Политики организации (профили конфигурации, `CFPreferences`) — MAC-04.
pub fn policy(_name: &str) -> Option<String> {
    None
}

/// Запуск из Finder или Dock (stderr — `/dev/null`): поток ошибок — в файл журнала, туда пишет `tracing`. Из терминала
/// — ничего. `true` — журнал пишется в файл.
pub fn log_to_file(path: &Path) -> bool {
    pets_shell::console::stderr_to_file(path).unwrap_or(false)
}

/// Уровень журнала по умолчанию (`POSTMAN_LOG`), если его не задали снаружи. Вызывать до потоков.
pub fn default_env(name: &str, value: &str) {
    if std::env::var_os(name).is_none() {
        // SAFETY: в начале `main`, до запуска потоков (Rust 2024: `set_var` — unsafe).
        unsafe { std::env::set_var(name, value) };
    }
}

/// Открыть путь или адрес приложением по умолчанию (`open`) — по действию пользователя.
pub fn open(target: &str) -> Result<(), String> {
    std::process::Command::new("/usr/bin/open").arg(target).spawn().map(|_| ()).map_err(|e| format!("open: {e}"))
}

/// Показать файл в Finder (`open -R`).
pub fn reveal(path: &Path) -> Result<(), String> {
    std::process::Command::new("/usr/bin/open").arg("-R").arg(path).spawn().map(|_| ()).map_err(|e| format!("open -R: {e}"))
}

/// Уведомление центра уведомлений (напоминание задачи и т. п.): звук по умолчанию.
pub fn notify(title: &str, body: &str) -> Result<(), String> {
    toast::show(title, body).map_err(|e| e.to_string())
}

/// Уведомление о новом письме: со звуком или молча («Настройки → Общие → Звук»).
pub fn notify_with(title: &str, body: &str, sound: bool) -> Result<(), String> {
    let sound = if sound { toast::Sound::Default } else { toast::Sound::Silent };
    toast::show_with(title, body, sound).map_err(|e| e.to_string())
}
