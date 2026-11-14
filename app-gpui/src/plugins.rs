//! Пути к серверам (ADR 0004, S7-02б): композиционный корень. Здесь — какие плагины протоколов знает Postman и в
//! каком порядке: почтовые пути — JMAP (путь по умолчанию: адрес без своей схемы), затем EWS Exchange (S7-06г:
//! адрес — URL EWS; узел без схемы — EWS узла или `autodiscover.<узел>`), затем IMAP + SMTP; пути областей — CalDAV и
//! CardDAV, EWS (у IMAP-учётки на Exchange). Новый протокол — плагин-крейт и строка здесь; `postman-core` и экраны не меняются.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use postman_core::backend::Registry;

/// Реестр плагинов процесса.
pub(crate) fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        Registry::default()
            .with_mail(postman_backend_jmap::JmapPlugin)
            .with_mail(postman_backend_ews::EwsPlugin)
            .with_mail(postman_backend_imap::ImapPlugin)
            .with_area(postman_backend_dav::DavPlugin)
            .with_area(postman_backend_ews::EwsAreaPlugin)
    })
}

/// Автоответ записан, но сервер хранит один текст всем (отказ в своём скрипте `vacation`, плагин JMAP): это не ошибка
/// записи, а ограничение, о котором надо сказать пользователю.
pub(crate) fn autoreply_limited(error: &str) -> bool {
    error == postman_backend_jmap::settings::ONE_TEXT_ONLY
}

/// Выход: письма в окне «Отменить» держат потоки IMAP и Exchange, сообщения каналов — очереди отправки потоков
/// каналов (ADR 0012 п. 11): они уходят сейчас, процесс их дожидается. Окна уже нет: не дольше [`LEAVE_LIMIT`], сервер
/// без ответа не держит процесс вечно.
pub(crate) fn finish_sends() {
    use postman_core::channel::outbox;
    let left = || postman_backend_imap::holding() + postman_backend_ews::sync::holding() + outbox::holding();
    if left() == 0 {
        return;
    }
    postman_backend_imap::leave();
    postman_backend_ews::sync::leave();
    outbox::leave();
    let start = Instant::now();
    while left() > 0 && start.elapsed() < LEAVE_LIMIT {
        std::thread::sleep(Duration::from_millis(50));
    }
}

const LEAVE_LIMIT: Duration = Duration::from_secs(30);

#[cfg(test)]
mod tests {
    /// Адрес со своей схемой — путь IMAP; адрес без схемы и `https://` — ничей (проверит сеть, иначе JMAP).
    #[test]
    fn imap_scheme_is_claimed() {
        let r = super::registry();
        assert!(r.claims("imaps://mail.example.test") && r.claims("IMAP://x"));
        assert!(!r.claims("https://mail.example.test") && !r.claims("mail.example.test"));
        // Адрес EWS — путь Exchange (S7-06г).
        assert!(r.claims("https://mail.example.test/EWS/Exchange.asmx"));
    }
}
