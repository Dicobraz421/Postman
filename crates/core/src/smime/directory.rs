//! Каталог сертификатов адресатов (S5-12 Ф5, решение владельца №12 — Active Directory,
//! `docs/decisions/2026-11-02-ldap-active-directory.md`): договор с плагином каталога (`postman-backend-ldap`) и кэш
//! найденного. Ядро не знает LDAP: плагин отдаёт DER как есть — это чужие данные, их проверяет и разбирает воркер
//! (`SmimePrepare`); в сертификаты адресата попадают только проверенные, с источником [`SOURCE`], и живут там
//! [`FRESH_FOR`] — дальше каталог спрашивают снова.
//!
//! Каталог спрашивают только по явному запросу («Найти в каталоге») и при отправке, не на каждую букву. Не ответил
//! за [`TIMEOUT`] — [`DirectoryError::Timeout`]: вход Kerberos блокирует поток, поэтому плагин ждут в отдельном
//! потоке, а не тайм-аутом внутри его рантайма.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use postman_cache::Cache;

use super::KnownCert;

/// Источник сертификата в кэше адресата.
pub const SOURCE: &str = "ldap";
/// Сколько найденное в каталоге считается свежим (сутки, `docs/SMIME.md` Ф5).
pub const FRESH_FOR: i64 = 24 * 3600;
/// Дольше каталог не ждут: отправка не висит (`docs/SMIME.md` Ф5).
pub const TIMEOUT: Duration = Duration::from_secs(5);
/// Не больше сертификатов за один запрос и не больше байт в одном: ответ каталога — чужие данные.
pub const MAX_CERTS: usize = 64;
pub const MAX_CERT_LEN: usize = 64 * 1024;

/// Почему каталог не дал ответа — для строки «каталог недоступен» и журнала.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DirectoryError {
    /// Компьютер не в домене, а сервер в настройках не задан.
    #[error("directory is not configured")]
    NotConfigured,
    /// Нет связи, отказ входа или поиска; текст — для журнала.
    #[error("directory is unavailable: {0}")]
    Unavailable(String),
    /// Не ответил за [`TIMEOUT`].
    #[error("directory did not answer in time")]
    Timeout,
}

/// Каталог: сертификаты (DER, не проверенные) по адресам. Реализация — плагин; вызов может блокировать поток.
pub trait Directory: Send + Sync {
    fn certificates(&self, addresses: &[String]) -> Result<Vec<Vec<u8>>, DirectoryError>;
}

/// Спросить каталог о сертификатах адресов: адреса — в нижнем регистре, без пустых и повторов; ждать не дольше
/// `timeout` ([`TIMEOUT`]); ответ — без повторов, не больше [`MAX_CERTS`], каждый не длиннее [`MAX_CERT_LEN`].
pub fn lookup(dir: Arc<dyn Directory>, addresses: &[String], timeout: Duration) -> Result<Vec<Vec<u8>>, DirectoryError> {
    let mut asked: Vec<String> = Vec::new();
    for a in addresses.iter().map(|a| a.trim().to_lowercase()).filter(|a| !a.is_empty()) {
        if !asked.contains(&a) {
            asked.push(a);
        }
    }
    if asked.is_empty() {
        return Ok(Vec::new());
    }
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("postman-directory".into())
        .spawn(move || {
            // Получателя может уже не быть (тайм-аут): ответ просто пропадает.
            let _ = tx.send(dir.certificates(&asked));
        })
        .map_err(|e| DirectoryError::Unavailable(e.to_string()))?;
    let found = match rx.recv_timeout(timeout) {
        Ok(r) => r?,
        Err(mpsc::RecvTimeoutError::Timeout) => return Err(DirectoryError::Timeout),
        Err(mpsc::RecvTimeoutError::Disconnected) => return Err(DirectoryError::Unavailable("directory thread stopped".into())),
    };
    let mut out: Vec<Vec<u8>> = Vec::new();
    for der in found.into_iter().filter(|d| !d.is_empty() && d.len() <= MAX_CERT_LEN) {
        if out.len() == MAX_CERTS {
            tracing::warn!("smime: ответ справочника урезан до {MAX_CERTS} сертификатов");
            break;
        }
        if !out.contains(&der) {
            out.push(der);
        }
    }
    Ok(out)
}

/// Сертификат из каталога, который воркер проверил (`SmimePrepare`): адреса из сертификата, отпечаток, срок.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub emails: Vec<String>,
    pub fingerprint: String,
    pub not_after: i64,
    pub der: Vec<u8>,
}

/// Итог запроса — в кэш: у каждого спрошенного адреса прежние сертификаты из каталога заменяются найденными
/// сейчас (убранный из каталога сертификат уходит и из кэша); сертификаты из подписей не трогаются.
pub fn remember(cache: &Cache, asked: &[String], found: &[Found], now: i64) {
    for address in asked.iter().map(|a| a.trim().to_lowercase()).filter(|a| !a.is_empty()) {
        let mut list: Vec<KnownCert> = cache.certs(&address).ok().flatten().unwrap_or_default();
        list.retain(|c| c.source != SOURCE);
        for f in found.iter().filter(|f| f.emails.iter().any(|e| e.trim().eq_ignore_ascii_case(&address))) {
            if !list.iter().any(|c| c.fingerprint == f.fingerprint) {
                list.push(KnownCert {
                    fingerprint: f.fingerprint.clone(),
                    der: f.der.clone(),
                    not_after: f.not_after,
                    capabilities: Vec::new(),
                    seen_at: now,
                    source: SOURCE.into(),
                });
            }
        }
        if let Err(e) = cache.set_certs(&address, &list) {
            tracing::warn!("smime: сертификаты справочника не записаны в кэш: {e}");
        }
    }
}

/// Сертификат из каталога устарел: спрашивали больше [`FRESH_FOR`] назад.
pub fn stale(c: &KnownCert, now: i64) -> bool {
    c.source == SOURCE && now.saturating_sub(c.seen_at) >= FRESH_FOR
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    struct Fixed(Result<Vec<Vec<u8>>, DirectoryError>);

    impl Directory for Fixed {
        fn certificates(&self, _: &[String]) -> Result<Vec<Vec<u8>>, DirectoryError> {
            self.0.clone()
        }
    }

    /// Запоминает, о чём спросили.
    struct Asked(std::sync::Mutex<Vec<String>>);

    impl Directory for Asked {
        fn certificates(&self, addresses: &[String]) -> Result<Vec<Vec<u8>>, DirectoryError> {
            self.0.lock().unwrap().extend(addresses.iter().cloned());
            Ok(Vec::new())
        }
    }

    struct Hangs;

    impl Directory for Hangs {
        fn certificates(&self, _: &[String]) -> Result<Vec<Vec<u8>>, DirectoryError> {
            std::thread::sleep(Duration::from_secs(30));
            Ok(Vec::new())
        }
    }

    /// Адреса — в нижнем регистре, без пустых и повторов; без адресов каталог не спрашивают.
    #[test]
    fn asks_normalized_addresses() {
        let dir = Arc::new(Asked(std::sync::Mutex::new(Vec::new())));
        lookup(dir.clone(), &[" P@X.test".into(), "p@x.test".into(), "".into(), "q@x.test".into()], TIMEOUT).unwrap();
        assert_eq!(*dir.0.lock().unwrap(), ["p@x.test", "q@x.test"]);
        lookup(dir.clone(), &["  ".into()], TIMEOUT).unwrap();
        assert_eq!(dir.0.lock().unwrap().len(), 2);
    }

    /// Ответ — без повторов, пустых и слишком длинных, не больше [`MAX_CERTS`].
    #[test]
    fn answer_is_bounded() {
        let mut list = vec![vec![1], vec![1], vec![], vec![0; MAX_CERT_LEN + 1]];
        list.extend((0..100u8).map(|i| vec![2, i]));
        let got = lookup(Arc::new(Fixed(Ok(list))), &["p@x.test".into()], TIMEOUT).unwrap();
        assert_eq!(got.len(), MAX_CERTS);
        assert_eq!(got[0], vec![1]);
        assert_eq!(got[1], vec![2, 0]);
    }

    /// Каталог молчит — тайм-аут в срок, отправка не висит; ошибка каталога — как есть.
    #[test]
    fn times_out_and_passes_errors() {
        let t = std::time::Instant::now();
        let got = lookup(Arc::new(Hangs), &["p@x.test".into()], Duration::from_millis(200));
        assert_eq!(got, Err(DirectoryError::Timeout));
        assert!(t.elapsed() < Duration::from_secs(5));
        let down = Fixed(Err(DirectoryError::Unavailable("refused".into())));
        assert_eq!(lookup(Arc::new(down), &["p@x.test".into()], TIMEOUT), Err(DirectoryError::Unavailable("refused".into())));
    }

    fn known(fp: &str, source: &str, seen_at: i64) -> KnownCert {
        KnownCert { fingerprint: fp.into(), der: vec![1], not_after: 10, capabilities: vec![], seen_at, source: source.into() }
    }

    /// Новый ответ каталога заменяет прежний, сертификаты из подписей остаются; адрес — по сертификату.
    #[test]
    fn remember_replaces_directory_entries() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("c.redb")).unwrap();
        cache.set_certs("p@x.test", &vec![known("S", "signed", 1), known("OLD", SOURCE, 1)]).unwrap();
        let found = [
            Found { emails: vec!["P@x.test".into()], fingerprint: "NEW".into(), not_after: 20, der: vec![2] },
            Found { emails: vec!["other@x.test".into()], fingerprint: "O".into(), not_after: 20, der: vec![3] },
        ];
        remember(&cache, &["p@x.test".into(), "none@x.test".into()], &found, 100);
        let list: Vec<KnownCert> = cache.certs("p@x.test").unwrap().unwrap();
        let fps: Vec<(&str, &str)> = list.iter().map(|c| (c.fingerprint.as_str(), c.source.as_str())).collect();
        assert_eq!(fps, [("S", "signed"), ("NEW", SOURCE)]);
        assert_eq!(list[1].seen_at, 100);
        let none: Vec<KnownCert> = cache.certs("none@x.test").unwrap().unwrap();
        assert!(none.is_empty());
    }

    /// Свежесть — только у сертификатов из каталога.
    #[test]
    fn staleness() {
        assert!(!stale(&known("A", SOURCE, 100), 100 + FRESH_FOR - 1));
        assert!(stale(&known("A", SOURCE, 100), 100 + FRESH_FOR));
        assert!(!stale(&known("A", "signed", 0), 10 * FRESH_FOR));
    }
}
