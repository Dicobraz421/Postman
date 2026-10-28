//! Связь с Windows для «Настройки → Общие» (S5-30): автозапуск (HKCU Run), клиент `mailto:` по умолчанию
//! (HKCU Classes, RegisteredApplications), журнал в файл (поток ошибок процесса без консоли), переменная окружения
//! уровня журнала. Всё — в разделе пользователя, без прав администратора. Вызовы ОС — `pets_shell` и `gpui_windows::pets`; здесь —
//! что и куда пишет Postman.

use std::path::Path;

use gpui_windows::pets::toast;
use pets_shell::console;
use pets_shell::registry::{self, Hive};

/// Имя значения в Run и приложения в RegisteredApplications.
const APP: &str = "Postman";
const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// ProgId ссылок `mailto:`.
pub const MAILTO_PROGID: &str = "Postman.Url.mailto";
const CAPABILITIES: &str = r"Software\Clients\Mail\Postman\Capabilities";
const USER_CHOICE: &str = r"Software\Microsoft\Windows\Shell\Associations\UrlAssociations\mailto\UserChoice";

/// Записать строковое значение в разделе пользователя (`name` — `None`: значение по умолчанию ключа), создав ключ.
fn set_string(path: &str, name: Option<&str>, value: &str) -> Result<(), String> {
    registry::set_string(path, name, value).map_err(|e| e.to_string())
}

/// Строковое значение раздела пользователя; нет — `None`.
fn get_string(path: &str, name: Option<&str>) -> Option<String> {
    registry::get_string(Hive::User, path, name)
}

fn exe() -> Result<String, String> {
    std::env::current_exe().map(|p| p.display().to_string()).map_err(|e| format!("current exe: {e}"))
}

/// Запуск вместе с Windows: свёрнутым (`--minimized`).
pub fn set_autostart(on: bool) -> Result<(), String> {
    if on {
        set_string(RUN, Some(APP), &format!("\"{}\" --minimized", exe()?))
    } else {
        // Значения не было — тоже «выключено».
        registry::delete_value(RUN, APP).map_err(|e| e.to_string())
    }
}

/// Значок приложения (T1-01) — файлом для тостов: `IconUri` у AUMID — путь к картинке на диске.
const ICON: &[u8] = include_bytes!("../../res/postman.ico");

/// Уведомление Windows (S6-07: напоминание задачи, когда окно свёрнуто): тост центра уведомлений от имени Postman.
/// AUMID без ярлыка в «Пуске» (`Pets.Postman`): имя и значок «Ленты» (T1-01; файл — в каталоге данных,
/// переписывается, если отличается от вшитого).
pub fn notify(title: &str, body: &str) -> Result<(), String> {
    let icon = crate::data_dir().join("postman.ico");
    if std::fs::read(&icon).ok().as_deref() != Some(ICON) {
        // Без файла тост выйдет со значком по умолчанию — не повод его не показывать.
        if let Err(e) = std::fs::write(&icon, ICON) {
            tracing::warn!("toast icon {}: {e}", icon.display());
        }
    }
    let sender = toast::Sender { aumid: "Pets.Postman", name: APP, icon: Some(&icon) };
    toast::show(&sender, title, body).map_err(|e| e.to_string())
}

/// Политика организации (`Software\Policies\Pets\Postman`, сначала HKLM, затем HKCU): строковое значение.
pub fn policy(name: &str) -> Option<String> {
    const PATH: &str = r"Software\Policies\Pets\Postman";
    registry::get_string(Hive::Machine, PATH, Some(name)).or_else(|| get_string(PATH, Some(name)))
}

/// Значение автозапуска есть.
pub fn autostart() -> bool {
    get_string(RUN, Some(APP)).is_some()
}

/// Зарегистрировать Postman как почтовый клиент для `mailto:` (выбор — за пользователем в «Приложениях по
/// умолчанию»).
pub fn register_mailto() -> Result<(), String> {
    let exe = exe()?;
    let classes = format!(r"Software\Classes\{MAILTO_PROGID}");
    set_string(&classes, None, "URL:MailTo Protocol")?;
    set_string(&format!(r"{classes}\DefaultIcon"), None, &format!("\"{exe}\",0"))?;
    set_string(&format!(r"{classes}\shell\open\command"), None, &format!("\"{exe}\" \"%1\""))?;
    set_string(CAPABILITIES, Some("ApplicationName"), APP)?;
    set_string(CAPABILITIES, Some("ApplicationDescription"), "Postman")?;
    set_string(&format!(r"{CAPABILITIES}\URLAssociations"), Some("mailto"), MAILTO_PROGID)?;
    set_string(r"Software\RegisteredApplications", Some(APP), CAPABILITIES)
}

/// Ссылки `mailto:` открывает Postman (выбор пользователя в Windows).
pub fn mailto_is_default() -> bool {
    get_string(USER_CHOICE, Some("ProgId")).is_some_and(|p| p == MAILTO_PROGID)
}

/// Процесс без консоли (release): поток ошибок — в файл журнала, туда пишет `tracing`. Есть консоль — ничего.
/// `true` — журнал пишется в файл.
pub fn log_to_file(path: &Path) -> bool {
    console::stderr_to_file(path).unwrap_or(false)
}

/// Уровень журнала по умолчанию (`POSTMAN_LOG`), если его не задали снаружи. Вызывать до потоков.
pub fn default_env(name: &str, value: &str) {
    if std::env::var_os(name).is_none() {
        // SAFETY: в начале `main`, до запуска потоков (Rust 2024: `set_var` — unsafe).
        unsafe { std::env::set_var(name, value) };
    }
}

/// Открыть путь, адрес или страницу параметров Windows (`ms-settings:defaultapps`) — по действию пользователя.
pub fn open(target: &str) -> Result<(), String> {
    std::process::Command::new("explorer.exe").arg(target).spawn().map(|_| ()).map_err(|e| format!("explorer: {e}"))
}

/// Показать файл в Проводнике.
pub fn reveal(path: &Path) -> Result<(), String> {
    std::process::Command::new("explorer.exe").arg(format!("/select,{}", path.display())).spawn().map(|_| ()).map_err(|e| format!("explorer: {e}"))
}

#[cfg(test)]
mod tests {
    /// Чтение несуществующего значения — `None`, без паники.
    #[test]
    fn missing_values_read_as_none() {
        assert_eq!(super::get_string(r"Software\Postman-test-nothing-here", Some("x")), None);
    }
}
