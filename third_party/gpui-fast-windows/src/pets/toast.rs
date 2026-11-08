//! Уведомление центра уведомлений Windows от имени приложения без ярлыка в «Пуске»: AUMID регистрируется в
//! `HKCU\Software\Classes\AppUserModelId\<aumid>` (имя и значок), процессу AUMID не ставится — группировка окон на
//! панели задач не меняется (в отличие от `App::set_app_identity` gpui).

use std::io;
use std::path::Path;

use windows::Data::Xml::Dom::XmlDocument;
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
use windows::core::HSTRING;

use windows_registry::CURRENT_USER;

use super::com_error;

/// От чьего имени уведомление.
pub struct Sender<'a> {
    /// AUMID: `Pets.Postman`.
    pub aumid: &'a str,
    /// Имя в заголовке уведомления.
    pub name: &'a str,
    /// Значок — файл картинки на диске (`IconUri` у AUMID — путь к файлу, не ресурс exe); нет — значок по умолчанию.
    pub icon: Option<&'a Path>,
}

/// Показать уведомление: заголовок и текст.
pub fn show(sender: &Sender<'_>, title: &str, body: &str) -> io::Result<()> {
    let key = format!(r"Software\Classes\AppUserModelId\{}", sender.aumid);
    let register = || -> windows::core::Result<()> {
        let key = CURRENT_USER.create(&key)?;
        key.set_string("DisplayName", sender.name)?;
        if let Some(icon) = sender.icon.filter(|p| p.exists()) {
            key.set_string("IconUri", icon.display().to_string())?;
        }
        Ok(())
    };
    register().map_err(|e| com_error("AppUserModelId", e))?;
    let xml = document(title, body);
    let run = || -> windows::core::Result<()> {
        let doc = XmlDocument::new()?;
        doc.LoadXml(&HSTRING::from(xml.as_str()))?;
        let toast = ToastNotification::CreateToastNotification(&doc)?;
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(sender.aumid))?.Show(&toast)
    };
    run().map_err(|e| com_error("toast", e))
}

/// XML уведомления ToastGeneric: две строки, текст экранирован.
fn document(title: &str, body: &str) -> String {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
        esc(title),
        esc(body)
    )
}

#[cfg(test)]
mod tests {
    /// Разметка из текста пользователя не ломает XML уведомления.
    #[test]
    fn pets_toast_text_is_escaped() {
        let xml = super::document("A & <b>", "\"x\" > y");
        assert!(
            xml.contains("<text>A &amp; &lt;b&gt;</text><text>&quot;x&quot; &gt; y</text>"),
            "{xml}"
        );
        let doc = windows::Data::Xml::Dom::XmlDocument::new().unwrap();
        doc.LoadXml(&windows::core::HSTRING::from(xml.as_str())).unwrap();
    }
}
