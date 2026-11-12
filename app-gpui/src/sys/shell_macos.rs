//! Связь с macOS для «Настройки → Общие» и окна (MAC-02): клиент `mailto:` по умолчанию (LaunchServices), журнал
//! в файл (`~/Library/Logs/Postman` — путь даёт `postman_core::paths::log_dir`), Finder («Открыть», «Показать в
//! Finder»), уведомления центра уведомлений. MAC-04: запуск при входе (`SMAppService`), политики (профиль
//! конфигурации), FileVault, щелчок по уведомлению. Вызовы ОС — `pets_shell` и `pets_gpui_macos`; здесь — что делает
//! Postman. Вход без пароля в домене (Kerberos) — MAC-05.

use std::path::Path;

use pets_gpui_macos::toast;

/// Схема ссылок почты (объявлена в `Info.plist`, `scripts/macos/package.sh`).
const MAILTO: &str = "mailto";

/// Шифрование диска с данными (S8-35): FileVault — `fdesetup status` (без прав администратора; шифруется весь
/// загрузочный том с домашней папкой, где данные Postman). Имени диска в текстах macOS нет — пустая строка.
/// `Some(true)` — включён, `Some(false)` — выключен, `None` — не ответил. Звать в фоне.
pub fn bitlocker(_path: &Path) -> (String, Option<bool>) {
    let out = std::process::Command::new("/usr/bin/fdesetup").arg("status").output();
    let state = match out {
        Ok(o) if o.status.success() => filevault(&String::from_utf8_lossy(&o.stdout)),
        Ok(o) => {
            tracing::warn!("fdesetup status: {}", o.status);
            None
        }
        Err(e) => {
            tracing::warn!("fdesetup: {e}");
            None
        }
    };
    (String::new(), state)
}

/// Ответ `fdesetup status`: «FileVault is On.» (и «… Encryption in progress» — уже защищает новые данные),
/// «FileVault is Off.» (и расшифровка).
fn filevault(status: &str) -> Option<bool> {
    let first = status.lines().next()?.trim();
    if first.starts_with("FileVault is On") {
        Some(true)
    } else if first.starts_with("FileVault is Off") {
        Some(false)
    } else {
        None
    }
}

/// «Системные настройки → Конфиденциальность и безопасность → FileVault».
pub fn open_bitlocker() -> Result<(), String> {
    open("x-apple.systempreferences:com.apple.preference.security?FileVault")
}

/// Вход Windows (UPN) на Mac не бывает; Kerberos — MAC-05.
pub fn upn() -> Option<String> {
    None
}

/// Вход без пароля в Exchange на macOS (Kerberos, `sspi`/GSS.framework) — MAC-05; до того вариант не предлагается.
pub fn in_domain() -> bool {
    false
}

/// Открывать Postman при входе в систему: `SMAppService.mainApp` (объект входа «Postman»). Выключенный пользователем
/// в «Объектах входа» — открыть их: включить можно только там.
pub fn set_autostart(on: bool) -> Result<(), String> {
    use pets_shell::login_item::{self, State};
    login_item::set(on).map_err(|e| e.to_string())?;
    if on && login_item::state() == State::RequiresApproval {
        login_item::open_settings();
    }
    Ok(())
}

/// Postman открывается при входе.
pub fn autostart() -> bool {
    pets_shell::login_item::state() == pets_shell::login_item::State::On
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

/// Политика организации (`AllowUpdate` и др., как `Software\Policies\Pets\Postman` на Windows): принудительное
/// значение профиля конфигурации для пакета Postman (`CFPreferences`); свои `defaults write` не считаются.
pub fn policy(name: &str) -> Option<String> {
    pets_shell::managed::get(name)
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

/// Уведомление о новом письме: со звуком или молча («Настройки → Общие → Звук»).
pub fn notify_with(title: &str, body: &str, sound: bool) -> Result<(), String> {
    let sound = if sound { toast::Sound::Default } else { toast::Sound::Silent };
    toast::show_with(title, body, sound).map_err(|e| e.to_string())
}

/// Уведомление со щелчком (MAC-04, как T3-13 на Windows): `on_click` — из потока центра уведомлений, пока Postman
/// запущен. `sound`: `None` и `Some(true)` — звук уведомлений, `Some(false)` — молча.
pub fn notify_click(title: &str, body: &str, sound: Option<bool>, on_click: impl Fn() + Send + 'static) -> Result<(), String> {
    let sound = if sound == Some(false) { toast::Sound::Silent } else { toast::Sound::Default };
    toast::show_clickable(title, body, sound, on_click).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    /// Ответы `fdesetup status` (macOS 13 — 26).
    #[test]
    fn filevault_status() {
        assert_eq!(
            super::filevault(
                "FileVault is On.
"
            ),
            Some(true)
        );
        assert_eq!(
            super::filevault(
                "FileVault is On.
Encryption in progress: Percent completed = 12
"
            ),
            Some(true)
        );
        assert_eq!(
            super::filevault(
                "FileVault is Off.
"
            ),
            Some(false)
        );
        assert_eq!(
            super::filevault(
                "FileVault is Off.
Decryption in progress: Percent completed = 40
"
            ),
            Some(false)
        );
        assert_eq!(super::filevault(""), None);
    }
}
