//! Каталог сертификатов адресатов (S5-12 Ф5, S5-12.5б): плагин `postman-backend-ldap` по настройкам «Безопасность»,
//! ответ — в воркер (`SmimePrepare`: чужие сертификаты проверяет и разбирает только он), проверенные — в кэш адресата
//! (`postman_core::smime::directory`). Каталог спрашивают только по «Найти в каталоге», при отправке и по
//! «Проверить» в настройках. Пароль простого входа — в Диспетчере учётных данных (`Postman:ldap:<сервер>`).

use std::sync::{Arc, Mutex, PoisonError};

use pets_i18n::tr;
use postman_backend_ldap::{LdapDirectory, Login, Settings, Zeroizing};
use postman_cache::Cache;
use postman_core::prefs::{Directory as Prefs, DirectoryLogin};
use postman_core::smime::directory::{self, Directory, DirectoryError, Found};
use postman_smime::prepare::Prepared;
use postman_worker::Client as Worker;

/// Запись пароля каталога в Диспетчере учётных данных.
pub(crate) fn credential_target(server: &str) -> String {
    format!("Postman:ldap:{}", server.trim().to_lowercase())
}

/// Каталог из настроек; пароль — из Диспетчера учётных данных (блокирует поток: только в фоне).
pub(crate) fn from_prefs(p: &Prefs) -> Arc<dyn Directory> {
    let login = match p.login {
        DirectoryLogin::Windows => Login::Windows,
        DirectoryLogin::Anonymous => Login::Anonymous,
        DirectoryLogin::Password => {
            let password = crate::sys::credentials::read(&credential_target(&p.server)).ok().flatten().unwrap_or_default();
            Login::Simple { user: p.user.trim().to_owned(), password: Zeroizing::new(password) }
        }
    };
    let text = |s: &str| Some(s.trim().to_owned()).filter(|s| !s.is_empty());
    Arc::new(LdapDirectory::new(Settings { server: text(&p.server), base_dn: text(&p.base_dn), starttls: p.starttls, login }))
}

/// Демо-ящик: каталога нет, сертификатов в нём тоже (снимки строки «Перед отправкой»).
pub(crate) struct Empty;

impl Directory for Empty {
    fn certificates(&self, _: &[String]) -> Result<Vec<Vec<u8>>, DirectoryError> {
        Ok(Vec::new())
    }
}

/// Спросить каталог об адресах `addrs`, проверить ответ в воркере и запомнить проверенные: подготовленные
/// получатели (только с адресом из `addrs`) или ошибка каталога.
pub(crate) fn search(
    dir: Arc<dyn Directory>,
    addrs: &[String],
    cache: &Cache,
    worker: &Mutex<Worker>,
    trust: &postman_smime::Trust,
    now: i64,
) -> Result<Vec<Prepared>, DirectoryError> {
    let ders = directory::lookup(dir, addrs, directory::TIMEOUT)?;
    let checked = if ders.is_empty() {
        Vec::new()
    } else {
        let mut w = worker.lock().unwrap_or_else(PoisonError::into_inner);
        match w.smime_prepare(&ders, trust, u64::try_from(now).unwrap_or(0)) {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!("directory certificates are not checked: {e:?}");
                return Err(DirectoryError::Unavailable("worker".into()));
            }
        }
    };
    let wanted = |e: &str| addrs.iter().any(|a| a.trim().eq_ignore_ascii_case(e.trim()));
    let mut found = Vec::new();
    let mut prepared = Vec::new();
    // Ответ воркера — по индексу кандидата: сертификат и его DER.
    for (der, p) in ders.into_iter().zip(checked) {
        let Ok(p) = p else { continue };
        if !p.emails.iter().any(|e| wanted(e)) {
            continue;
        }
        found.push(Found { emails: p.emails.clone(), fingerprint: p.fingerprint.clone(), not_after: p.not_after, der });
        prepared.push(p);
    }
    directory::remember(cache, addrs, &found, now);
    Ok(prepared)
}

/// Текст ошибки каталога для строки «Перед отправкой» и настроек.
pub(crate) fn error_text(e: &DirectoryError) -> String {
    match e {
        DirectoryError::NotConfigured => tr!("directory-not-configured"),
        DirectoryError::Timeout => tr!("directory-timeout"),
        DirectoryError::Unavailable(_) => tr!("directory-unavailable"),
    }
}
