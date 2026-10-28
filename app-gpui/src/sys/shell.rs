//! Связь с Windows для «Настройки → Общие» (S5-30): автозапуск (HKCU Run), клиент `mailto:` по умолчанию
//! (HKCU Classes, RegisteredApplications), журнал в файл (поток ошибок процесса без консоли), переменная окружения
//! уровня журнала. Всё — в разделе пользователя, без прав администратора.

use std::os::windows::io::IntoRawHandle as _;
use std::path::Path;

use windows_sys::Win32::Foundation::{ERROR_SUCCESS, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, SetStdHandle};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW,
    RegDeleteKeyValueW, RegGetValueW, RegSetValueExW,
};

/// Имя значения в Run и приложения в RegisteredApplications.
const APP: &str = "Postman";
const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// ProgId ссылок `mailto:`.
pub const MAILTO_PROGID: &str = "Postman.Url.mailto";
const CAPABILITIES: &str = r"Software\Clients\Mail\Postman\Capabilities";
const USER_CHOICE: &str = r"Software\Microsoft\Windows\Shell\Associations\UrlAssociations\mailto\UserChoice";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Записать строковое значение (`name` — `None`: значение по умолчанию ключа), создав ключ.
fn set_string(path: &str, name: Option<&str>, value: &str) -> Result<(), String> {
    let wpath = wide(path);
    let mut key: HKEY = std::ptr::null_mut();
    // SAFETY: строка пути живёт до конца вызова; `key` — выходной параметр.
    let rc = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            wpath.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(format!("RegCreateKeyExW {path}: {rc}"));
    }
    let wname = name.map(wide);
    let data = wide(value);
    let bytes = u32::try_from(data.len() * 2).map_err(|_| "value is too long".to_owned())?;
    // SAFETY: ключ открыт выше; данные — UTF-16 с нулём, размер в байтах.
    let rc = unsafe { RegSetValueExW(key, wname.as_ref().map_or(std::ptr::null(), |n| n.as_ptr()), 0, REG_SZ, data.as_ptr().cast(), bytes) };
    // SAFETY: ключ открыт выше и больше не нужен.
    unsafe { RegCloseKey(key) };
    if rc == ERROR_SUCCESS { Ok(()) } else { Err(format!("RegSetValueExW {path}: {rc}")) }
}

/// Строковое значение; нет — `None`.
fn get_string(path: &str, name: Option<&str>) -> Option<String> {
    get_string_in(HKEY_CURRENT_USER, path, name)
}

fn get_string_in(root: HKEY, path: &str, name: Option<&str>) -> Option<String> {
    let wpath = wide(path);
    let wname = name.map(wide);
    let mut buf = vec![0u16; 1024];
    let mut size = u32::try_from(buf.len() * 2).ok()?;
    // SAFETY: буфер и его размер в байтах; строки живут до конца вызова.
    let rc = unsafe {
        RegGetValueW(
            root,
            wpath.as_ptr(),
            wname.as_ref().map_or(std::ptr::null(), |n| n.as_ptr()),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if rc != ERROR_SUCCESS {
        return None;
    }
    let len = (size as usize / 2).saturating_sub(1).min(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

fn exe() -> Result<String, String> {
    std::env::current_exe().map(|p| p.display().to_string()).map_err(|e| format!("current exe: {e}"))
}

/// Запуск вместе с Windows: свёрнутым (`--minimized`).
pub fn set_autostart(on: bool) -> Result<(), String> {
    if on {
        set_string(RUN, Some(APP), &format!("\"{}\" --minimized", exe()?))
    } else {
        let (path, name) = (wide(RUN), wide(APP));
        // SAFETY: строки живут до конца вызова.
        let rc = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, path.as_ptr(), name.as_ptr()) };
        // Значения не было — тоже «выключено».
        if rc == ERROR_SUCCESS || rc == windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(format!("RegDeleteKeyValueW: {rc}"))
        }
    }
}

/// Значок приложения (T1-01) — файлом для тостов: `IconUri` у AUMID — путь к картинке на диске.
const ICON: &[u8] = include_bytes!("../../res/postman.ico");

/// Уведомление Windows (S6-07: напоминание задачи, когда окно свёрнуто): тост центра уведомлений от имени Postman.
/// AUMID без ярлыка в «Пуске» — регистрируется в `HKCU\Software\Classes\AppUserModelId`: имя и значок «Ленты»
/// (T1-01; файл — в каталоге данных, переписывается, если отличается от вшитого).
pub fn notify(title: &str, body: &str) -> Result<(), String> {
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
    use windows::core::HSTRING;
    const AUMID: &str = "Pets.Postman";
    let key = format!(r"Software\Classes\AppUserModelId\{AUMID}");
    set_string(&key, Some("DisplayName"), APP)?;
    let icon = crate::data_dir().join("postman.ico");
    if std::fs::read(&icon).ok().as_deref() != Some(ICON) {
        // Без файла тост выйдет со значком по умолчанию — не повод его не показывать.
        if let Err(e) = std::fs::write(&icon, ICON) {
            tracing::warn!("toast icon {}: {e}", icon.display());
        }
    }
    if icon.exists() {
        set_string(&key, Some("IconUri"), &icon.display().to_string())?;
    }
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;");
    let xml =
        format!("<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>", esc(title), esc(body));
    let run = || -> windows::core::Result<()> {
        let doc = XmlDocument::new()?;
        doc.LoadXml(&HSTRING::from(xml.as_str()))?;
        let toast = ToastNotification::CreateToastNotification(&doc)?;
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))?.Show(&toast)
    };
    run().map_err(|e| format!("toast: {e}"))
}

/// Политика организации (`Software\Policies\Pets\Postman`, сначала HKLM, затем HKCU): строковое значение.
pub fn policy(name: &str) -> Option<String> {
    const PATH: &str = r"Software\Policies\Pets\Postman";
    get_string_in(HKEY_LOCAL_MACHINE, PATH, Some(name)).or_else(|| get_string(PATH, Some(name)))
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
    // SAFETY: без аргументов-указателей; дескриптор не закрываем.
    let current = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
    if !current.is_null() && current != INVALID_HANDLE_VALUE {
        return false;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(file) = std::fs::OpenOptions::new().create(true).append(true).open(path) else { return false };
    let handle = file.into_raw_handle();
    // SAFETY: дескриптор файла живёт до конца процесса (не закрывается); Rust читает стандартный поток при каждой
    // записи в stderr.
    unsafe { SetStdHandle(STD_ERROR_HANDLE, handle) != 0 }
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
