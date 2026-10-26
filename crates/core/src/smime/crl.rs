//! Отзыв ГОСТ без СКЗИ (S5-12.1д, `docs/SMIME.md` §8): CRL по адресам CDP уже доверенной цепочки.
//!
//! Без СКЗИ Windows цепочку ГОСТ не проверяет, поэтому отзыв — своим кодом. Ядро для цепочки, которую воркер уже
//! довёл до доверенного корня, берёт адреса CRL из сертификатов (их записал доверенный УЦ, не отправитель — решение
//! владельца №3; только `http`/`https`), скачивает CRL с тайм-аутом и отдаёт **воркеру**: подпись CRL ключом УЦ,
//! серийный номер и срок проверяет он (`postman_smime::crl`, CRL — чужие байты). Проверенный CRL держится в памяти
//! до `nextUpdate`; недоступный адрес не спрашивается [`RETRY_AFTER`] — окно не ждёт его снова.
//!
//! Загрузчик и проверка — за трейтами [`CrlFetch`] и [`CrlVerify`]: тесты подменяют сеть и воркер.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use postman_smime::crl::{Checked, Verdict};

use super::Revocation;

/// Тайм-аут одного CRL.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
/// Вся проверка отзыва одной цепочки — не дольше.
pub const BUDGET: Duration = Duration::from_secs(8);
/// Недоступный адрес CRL не спрашивается снова столько секунд.
pub const RETRY_AFTER: i64 = 10 * 60;
/// Сколько байтов CRL держать в памяти (больше — проверяется, но не кэшируется).
const CACHE_BYTES: usize = 32 * 1024 * 1024;

/// Скачать CRL по адресу (`http`/`https` из сертификата доверенной цепочки) не дольше `timeout`. Байты не разбирает.
pub trait CrlFetch: Send + Sync {
    fn fetch(&self, url: &str, timeout: Duration) -> Result<Vec<u8>, String>;
}

/// Проверить CRL `crl` для сертификата `cert`, выпущенного `issuer` (в песочнице).
pub trait CrlVerify: Send + Sync {
    fn verify(&self, cert: &[u8], issuer: &[u8], crl: &[u8], now: i64) -> Result<Checked, String>;
}

/// Проверка в воркере: свой процесс воркера, запускается при первом CRL.
pub struct WorkerVerify(Mutex<Option<postman_worker::Client>>);

impl CrlVerify for WorkerVerify {
    fn verify(&self, cert: &[u8], issuer: &[u8], crl: &[u8], now: i64) -> Result<Checked, String> {
        let mut slot = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let worker = slot.get_or_insert_with(|| postman_worker::Client::new(postman_worker::Client::default_exe(), postman_worker::SMIME_TIMEOUT));
        match worker.smime_crl(cert, issuer, crl, u64::try_from(now).unwrap_or(0)) {
            Ok(Ok(c)) => Ok(c),
            Ok(Err(e)) => Err(e.to_string()),
            Err(e) => Err(format!("{e:?}")),
        }
    }
}

/// Загрузка средствами Windows (`CryptRetrieveObjectByUrlW`, без разбора).
pub struct SystemFetch;

impl CrlFetch for SystemFetch {
    fn fetch(&self, url: &str, timeout: Duration) -> Result<Vec<u8>, String> {
        super::sys::fetch_crl(url, timeout)
    }
}

struct Entry {
    crl: Arc<Vec<u8>>,
    next_update: i64,
}

#[derive(Default)]
struct State {
    lists: HashMap<String, Entry>,
    /// Адрес → когда не удалось скачать или проверить.
    failed: HashMap<String, i64>,
}

/// Проверка отзыва ГОСТ по CRL с кэшем.
pub struct GostCrl {
    fetch: Box<dyn CrlFetch>,
    verify: Box<dyn CrlVerify>,
    budget: Duration,
    state: Mutex<State>,
}

/// Итог по одному сертификату цепочки.
enum Link {
    Good,
    Revoked,
    Unknown,
}

fn web(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

impl GostCrl {
    pub fn new(fetch: Box<dyn CrlFetch>, verify: Box<dyn CrlVerify>) -> Self {
        GostCrl { fetch, verify, budget: BUDGET, state: Mutex::new(State::default()) }
    }

    /// Предел времени всей проверки (тесты).
    pub fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }

    /// Сеть Windows и воркер; один на процесс — кэш CRL общий.
    pub fn system() -> Arc<GostCrl> {
        static ONE: OnceLock<Arc<GostCrl>> = OnceLock::new();
        ONE.get_or_init(|| Arc::new(GostCrl::new(Box::new(SystemFetch), Box::new(WorkerVerify(Mutex::new(None)))))).clone()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Отзыв цепочки `chain` (подписант, промежуточные, корень — уже доверенная): отозван любой — «отозван»; все
    /// проверены — «не отозван»; иначе — «не проверен (ГОСТ без СКЗИ)».
    pub fn check(&self, chain: &[Vec<u8>], now: i64) -> Revocation {
        if chain.len() < 2 {
            return Revocation::NotCheckedGost;
        }
        let deadline = Instant::now() + self.budget;
        let mut all_good = true;
        // Корень (последний) сам себя не отзывает.
        for pair in chain.windows(2) {
            match self.link(&pair[0], &pair[1], now, deadline) {
                Link::Revoked => return Revocation::Revoked,
                Link::Good => {}
                Link::Unknown => all_good = false,
            }
        }
        if all_good { Revocation::Good } else { Revocation::NotCheckedGost }
    }

    fn link(&self, cert: &[u8], issuer: &[u8], now: i64, deadline: Instant) -> Link {
        for url in postman_smime::crl::distribution_points(cert).into_iter().filter(|u| web(u)) {
            let Some(crl) = self.list(&url, now, deadline) else { continue };
            match self.verify.verify(cert, issuer, &crl, now) {
                Ok(checked) => {
                    self.keep(&url, crl, checked.next_update);
                    return match checked.verdict {
                        Verdict::NotRevoked => Link::Good,
                        Verdict::Revoked { .. } => Link::Revoked,
                    };
                }
                Err(e) => {
                    tracing::info!(%url, error = %e, "GOST CRL is not usable");
                    let mut s = self.state();
                    s.lists.remove(&url);
                    s.failed.insert(url, now);
                }
            }
        }
        Link::Unknown
    }

    /// CRL из кэша (до `nextUpdate`) или скачанный; `None` — недоступен, недавно не скачался или время вышло.
    fn list(&self, url: &str, now: i64, deadline: Instant) -> Option<Arc<Vec<u8>>> {
        {
            let s = self.state();
            if let Some(e) = s.lists.get(url).filter(|e| now < e.next_update) {
                return Some(e.crl.clone());
            }
            if s.failed.get(url).is_some_and(|at| now - at < RETRY_AFTER) {
                return None;
            }
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        match self.fetch.fetch(url, left.min(FETCH_TIMEOUT)) {
            Ok(bytes) if bytes.len() <= postman_smime::crl::MAX_CRL => Some(Arc::new(bytes)),
            Ok(_) => {
                self.state().failed.insert(url.to_owned(), now);
                None
            }
            Err(e) => {
                tracing::info!(%url, error = %e, "GOST CRL is unreachable");
                self.state().failed.insert(url.to_owned(), now);
                None
            }
        }
    }

    /// Проверенный CRL — в кэш до `nextUpdate`; сверх предела вытесняются раньше истекающие.
    fn keep(&self, url: &str, crl: Arc<Vec<u8>>, next_update: i64) {
        if crl.len() > CACHE_BYTES {
            return;
        }
        let mut s = self.state();
        s.failed.remove(url);
        s.lists.insert(url.to_owned(), Entry { crl, next_update });
        while s.lists.values().map(|e| e.crl.len()).sum::<usize>() > CACHE_BYTES {
            let Some(oldest) = s.lists.iter().min_by_key(|(_, e)| e.next_update).map(|(u, _)| u.clone()) else { break };
            s.lists.remove(&oldest);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use postman_smime::oid;
    use postman_smime::testkit::{self, Issued, Key, NOW, Spec};

    use super::*;

    /// Проверка тем же кодом, что в воркере, — без процесса воркера (его exe тестам ядра недоступен).
    struct Direct;

    impl CrlVerify for Direct {
        fn verify(&self, cert: &[u8], issuer: &[u8], crl: &[u8], now: i64) -> Result<Checked, String> {
            postman_smime::crl::check(cert, issuer, crl, now as u64).map_err(|e| e.to_string())
        }
    }

    /// Подменный загрузчик: адрес → байты; счётчик загрузок; `None` — «нет связи» после ожидания до тайм-аута.
    struct Fake {
        lists: HashMap<String, Option<Vec<u8>>>,
        calls: Arc<AtomicUsize>,
        delay: Duration,
    }

    impl CrlFetch for Fake {
        fn fetch(&self, url: &str, timeout: Duration) -> Result<Vec<u8>, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.lists.get(url).cloned().flatten() {
                Some(b) => Ok(b),
                None => {
                    std::thread::sleep(self.delay.min(timeout));
                    Err("timeout".into())
                }
            }
        }
    }

    const URL: &str = "http://ca.itt.test/gost.crl";

    struct Pki {
        root: Issued,
        key: Key,
        user_key: Key,
        good: Issued,
        revoked: Issued,
    }

    fn pki() -> Pki {
        let key = Key::gost("root", oid::TC26_256_A);
        let root = testkit::issue(&Spec::ca("ГОСТ УЦ"), &key, None);
        let user_key = Key::gost("user", oid::TC26_512_C);
        let user = |serial: u64| {
            testkit::issue(&Spec { serial, crl_url: Some(URL.into()), ..Spec::user("Сидоров", "user2@itt.test") }, &user_key, Some((&root, &key)))
        };
        Pki { good: user(0x52), revoked: user(0x51), root, key, user_key }
    }

    fn engine(lists: &[(&str, Option<Vec<u8>>)], delay: Duration) -> (GostCrl, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let fake = Fake { lists: lists.iter().map(|(u, b)| ((*u).to_owned(), b.clone())).collect(), calls: calls.clone(), delay };
        (GostCrl::new(Box::new(fake), Box::new(Direct)), calls)
    }

    #[test]
    fn revoked_good_and_cached() {
        let p = pki();
        let now = NOW as i64;
        let crl = testkit::crl(&p.root, &p.key, &[0x51], now - 3600, now + 86_400);
        let (e, calls) = engine(&[(URL, Some(crl))], Duration::ZERO);
        let chain = |c: &Issued| vec![c.der.clone(), p.root.der.clone()];
        assert_eq!(e.check(&chain(&p.revoked), now), Revocation::Revoked);
        assert_eq!(e.check(&chain(&p.good), now + 60), Revocation::Good);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "второй раз — CRL из кэша, без загрузки");
        // После nextUpdate — загрузка заново (тот же CRL уже просрочен: «не проверен»).
        assert_eq!(e.check(&chain(&p.good), now + 86_400), Revocation::NotCheckedGost);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        // Без корня в цепочке и без адреса CRL — не проверен.
        assert_eq!(e.check(&chain(&p.good)[..1], now), Revocation::NotCheckedGost);
        assert_eq!(e.check(&[p.root.der.clone(), p.root.der.clone()], now), Revocation::NotCheckedGost);
    }

    #[test]
    fn foreign_signature_is_not_checked() {
        let p = pki();
        let now = NOW as i64;
        let forged = testkit::crl(&p.root, &Key::gost("forger", oid::TC26_256_A), &[], now - 3600, now + 86_400);
        let (e, calls) = engine(&[(URL, Some(forged))], Duration::ZERO);
        let chain = vec![p.revoked.der.clone(), p.root.der.clone()];
        assert_eq!(e.check(&chain, now), Revocation::NotCheckedGost);
        // Негодный CRL не кэшируется и не скачивается снова до RETRY_AFTER.
        assert_eq!(e.check(&chain, now + 60), Revocation::NotCheckedGost);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        e.check(&chain, now + RETRY_AFTER + 1);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// CDP недоступен: «не проверен», ожидание ограничено бюджетом, повторное открытие сеть не ждёт.
    #[test]
    fn unreachable_point_does_not_hold_the_letter() {
        let p = pki();
        let now = NOW as i64;
        let (e, calls) = engine(&[], Duration::from_secs(30));
        let e = e.with_budget(Duration::from_millis(200));
        let chain = vec![p.good.der.clone(), p.root.der.clone()];
        let t = Instant::now();
        assert_eq!(e.check(&chain, now), Revocation::NotCheckedGost);
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
        let t = Instant::now();
        assert_eq!(e.check(&chain, now + 1), Revocation::NotCheckedGost);
        assert!(t.elapsed() < Duration::from_millis(100), "второй раз — без загрузки: {:?}", t.elapsed());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// Итог письма: подпись ГОСТ без СКЗИ — «отозван» / «не отозван» по CRL; повторное открытие — из кэша итогов,
    /// без загрузки.
    #[test]
    fn letter_verdict_follows_the_crl() {
        use postman_cache::Cache;
        use postman_smime::testkit::{Head, Sign};
        use postman_smime::{Anchor, Trust};

        use crate::smime::{Context, GostEngine, cached, finish};

        let p = pki();
        let now = NOW as i64;
        let crl = testkit::crl(&p.root, &p.key, &[0x51], now - 3600, now + 86_400);
        let (e, calls) = engine(&[(URL, Some(crl))], Duration::ZERO);
        let trust = Trust { roots: vec![Anchor { der: p.root.der.clone(), corporate: false }], ..Default::default() };
        let ctx = Context { crl: Some(Arc::new(e)), ..Context::fixed(trust.clone()) };
        let head = Head { from: "user2@itt.test", to: "me@itt.test", subject: "s", date: "Wed, 21 Oct 2026 10:00:00 +0300" };
        let report =
            |c: &Issued| postman_smime::verify(&testkit::signed_letter(&head, "<p>x</p>", c, &p.user_key, &[&p.root], &Sign::default()), &trust, NOW);
        let cache = Cache::open_in_memory().unwrap();
        let v = finish(&cache, "revoked", report(&p.revoked), &ctx, now);
        assert_eq!((v.gost, v.revocation), (Some(GostEngine::Postman), Revocation::Revoked));
        let v = finish(&cache, "good", report(&p.good), &ctx, now);
        assert_eq!(v.revocation, Revocation::Good);
        assert_eq!(cached(&cache, "good", &ctx, now + 60).unwrap().revocation, Revocation::Good);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // Без проверки CRL (демо) — как раньше.
        assert_eq!(finish(&cache, "demo", report(&p.good), &Context::fixed(trust), now).revocation, Revocation::NotCheckedGost);
    }

    #[test]
    fn cache_is_bounded() {
        let (e, _) = engine(&[], Duration::ZERO);
        e.keep("http://a", Arc::new(vec![0; CACHE_BYTES / 2 + 1]), 10);
        e.keep("http://b", Arc::new(vec![0; CACHE_BYTES / 2 + 1]), 20);
        let s = e.state();
        assert!(s.lists.contains_key("http://b") && !s.lists.contains_key("http://a"), "раньше истекающий вытеснен");
    }
}
