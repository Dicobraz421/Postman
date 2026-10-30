//! S/MIME в ядре (S5-12 Ф1, `docs/SMIME.md` §6.3): якоря доверия, подтверждение цепочки Windows, кэш итогов и
//! сертификатов адресатов.
//!
//! Порядок: подпись проверяет воркер (чужие байты — только там, решение владельца №1); ядро передаёт ему якоря
//! (`Root`, `AuthRoot`, `CA`, `Disallowed` Windows — данные машины, не письма) и для цепочки, которую воркер **уже
//! довёл до доверенного корня**, подтверждает её в Windows: `CertGetCertificateChain` с проверкой отзыва (CRL/OCSP по
//! адресам CA, не отправителя) и политика `CERT_CHAIN_POLICY_BASE` — решение №3. Требование к любому CA — адреса
//! HTTP CDP/AIA в сертификатах; AD CS не предполагается.
//!
//! ГОСТ — движок выбирается сам (решение владельца 2026-10-21): есть сертифицированное СКЗИ (КриптоПро CSP, ViPNet
//! CSP — провайдеры ГОСТ в CryptoAPI) — цепочку и отзыв ГОСТ подтверждает Windows тем же путём, что RSA; нет — только
//! своя проверка Postman, отзыв «не проверен (ГОСТ без СКЗИ)». Подпись CMS в обоих случаях проверяет воркер (своя
//! реализация); проверка подписи средствами СКЗИ — задача 12.1е (`docs/SMIME.md` §8). Без СКЗИ отзыв ГОСТ — своя
//! проверка CRL ([`crl`], 12.1д): CRL по CDP доверенной цепочки, проверка — в воркере; «не проверен (ГОСТ без СКЗИ)» —
//! только когда CRL недоступен или не прошёл проверку. Обнаружение СКЗИ — за трейтом [`GostProvider`], тесты
//! подменяют оба режима.

#[cfg(windows)]
mod cng;
pub mod crl;
pub mod keys;
#[cfg(windows)]
mod seal_win;
#[cfg(windows)]
mod sign_win;
pub mod skzi;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as sys;

#[cfg(not(windows))]
mod other;
#[cfg(not(windows))]
use other as sys;

use std::sync::{Arc, Mutex, PoisonError};

use postman_cache::Cache;
use postman_smime::{Report, Trust};
use serde::{Deserialize, Serialize};

/// Отзыв перепроверяется, если прошлой проверке больше суток (`docs/SMIME.md` §6.3, п. 4).
pub const RECHECK_AFTER: i64 = 24 * 3600;

/// Итог проверки отзыва и подтверждения цепочки в Windows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Revocation {
    /// Не отозван (ответ CRL или OCSP получен).
    Good,
    /// Сертификат цепочки отозван.
    Revoked,
    /// Не удалось проверить: CA недоступен или в сертификате нет адресов CRL/OCSP.
    Unknown,
    /// Windows не подтвердила цепочку, которую принял воркер (корень убран, политика, запрет): текст — для журнала.
    Rejected(Confirm),
    /// ГОСТ без СКЗИ: своя проверка CRL (12.1д) не удалась — CRL недоступен, не прошёл проверку или в сертификатах
    /// нет адресов CRL.
    NotCheckedGost,
    /// Не проверялся: цепочка не доверена, подпись недействительна или демо-ящик.
    NotChecked,
}

/// Что сказала Windows о цепочке (кроме отзыва).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Confirm {
    Untrusted,
    Distrusted,
    Expired,
    WrongUsage,
    BadSignature,
    Failed,
}

/// Чем проверен ГОСТ.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GostEngine {
    /// Своя реализация Postman, без СКЗИ (не сертифицирована).
    Postman,
    /// Цепочку и отзыв подтвердило СКЗИ (имя провайдера CryptoAPI); подпись — Postman (12.1е).
    Skzi(String),
}

/// Есть ли в системе СКЗИ с ГОСТ Р 34.10-2012 (провайдер CryptoAPI). Трейт — чтобы тесты подменяли оба режима.
pub trait GostProvider: Send + Sync {
    /// Имя провайдера ГОСТ («Crypto-Pro GOST R 34.10-2012 Cryptographic Service Provider»…) или `None`.
    fn provider(&self) -> Option<String>;
}

/// Провайдеры CryptoAPI этой машины (перечисление — один раз на процесс).
pub struct SystemGost;

impl GostProvider for SystemGost {
    fn provider(&self) -> Option<String> {
        static FOUND: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
        FOUND
            .get_or_init(|| {
                let p = sys::gost_provider();
                tracing::info!(provider = ?p, "GOST CSP detection");
                p
            })
            .clone()
    }
}

/// Заданный ответ (тесты, демо).
pub struct FixedGost(pub Option<String>);

impl GostProvider for FixedGost {
    fn provider(&self) -> Option<String> {
        self.0.clone()
    }
}

/// Итог по письму: отчёт воркера, отзыв, когда проверено.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub report: Report,
    /// DER цепочки (из отчёта воркера): для перепроверки отзыва без воркера.
    pub chain: Vec<Vec<u8>>,
    pub revocation: Revocation,
    /// Unix-время последней проверки (отзыва — у доверенной цепочки).
    pub checked_at: i64,
    /// У подписи ГОСТ — чем проверено.
    pub gost: Option<GostEngine>,
}

impl Verdict {
    /// Отзыв пора перепроверить (доверенная цепочка, отзыв проверялся в Windows или по CRL ГОСТ).
    pub fn stale(&self, now: i64) -> bool {
        let checked = !matches!(self.revocation, Revocation::NotChecked);
        self.report.trusted() && checked && now - self.checked_at > RECHECK_AFTER
    }
}

/// Сертификат адресата из проверенной подписи (для шифрования — Ф4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownCert {
    pub fingerprint: String,
    pub der: Vec<u8>,
    pub not_after: i64,
    /// OID шифров `SMIMECapabilities` последнего подписанного письма.
    pub capabilities: Vec<String>,
    pub seen_at: i64,
    /// Откуда: «signed» — подпись входящего письма (Ф1); AddressBook, контакты, LDAP — Ф4–Ф5.
    pub source: String,
}

/// Откуда якоря: Windows (живой ящик) или переданные (демо, тесты) — тогда без подтверждения в Windows.
#[derive(Clone)]
pub enum Anchors {
    Windows,
    Fixed(Arc<Trust>),
}

/// Окружение проверки: якоря, СКЗИ ГОСТ и отзыв ГОСТ без СКЗИ.
#[derive(Clone)]
pub struct Context {
    pub anchors: Anchors,
    pub gost: Arc<dyn GostProvider>,
    /// CRL ГОСТ по CDP (12.1д); `None` — отзыв ГОСТ без СКЗИ не проверяется (демо).
    pub crl: Option<Arc<crl::GostCrl>>,
}

impl Context {
    /// Живой ящик: хранилища и провайдеры Windows, CRL ГОСТ — по сети.
    pub fn system() -> Self {
        Context { anchors: Anchors::Windows, gost: Arc::new(SystemGost), crl: Some(crl::GostCrl::system()) }
    }

    /// Демо и тесты: свои якоря, без Windows, без СКЗИ и без сети.
    pub fn fixed(trust: Trust) -> Self {
        Context { anchors: Anchors::Fixed(Arc::new(trust)), gost: Arc::new(FixedGost(None)), crl: None }
    }
}

static WINDOWS_TRUST: Mutex<Option<Arc<Trust>>> = Mutex::new(None);

/// Отпечатки корпоративных CA (SHA-256, hex) сверх хранилищ групповой политики: `POSTMAN_CORPORATE_CA` через `;` —
/// MDM ставит переменную машины [проверить на стенде MDM].
fn corporate_extra() -> Vec<String> {
    std::env::var("POSTMAN_CORPORATE_CA")
        .unwrap_or_default()
        .split([';', ','])
        .map(|s| s.trim().replace(':', "").to_ascii_uppercase())
        .filter(|s| !s.is_empty())
        .collect()
}

impl Anchors {
    /// Якоря (из хранилищ Windows — один раз на процесс; [`Anchors::reload`] — заново).
    pub fn trust(&self) -> Arc<Trust> {
        match self {
            Anchors::Fixed(t) => t.clone(),
            Anchors::Windows => {
                let mut cached = WINDOWS_TRUST.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(t) = cached.as_ref() {
                    return t.clone();
                }
                let t = Arc::new(sys::load_trust(&corporate_extra()));
                tracing::info!(
                    roots = t.roots.len(),
                    intermediates = t.intermediates.len(),
                    disallowed = t.disallowed.len(),
                    "S/MIME trust anchors loaded"
                );
                *cached = Some(t.clone());
                t
            }
        }
    }

    /// Хранилища изменились (импорт сертификата): перечитать при следующей проверке.
    pub fn reload() {
        *WINDOWS_TRUST.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    fn windows(&self) -> bool {
        matches!(self, Anchors::Windows)
    }
}

/// Подтвердить цепочку в Windows и проверить отзыв (доверенная цепочка; ГОСТ — при СКЗИ, без него — по CRL).
fn revocation(report: &Report, chain: &[Vec<u8>], ctx: &Context, skzi: Option<&str>, now: i64) -> Revocation {
    if !report.trusted() || chain.is_empty() {
        return Revocation::NotChecked;
    }
    if report.is_gost() && skzi.is_none() {
        return ctx.crl.as_ref().map_or(Revocation::NotCheckedGost, |c| c.check(chain, now));
    }
    if !ctx.anchors.windows() {
        return Revocation::NotChecked;
    }
    sys::confirm(chain, None)
}

/// Движок ГОСТ и отзыв для отчёта.
fn confirm(report: &Report, chain: &[Vec<u8>], ctx: &Context, now: i64) -> (Revocation, Option<GostEngine>) {
    if !report.is_gost() {
        return (revocation(report, chain, ctx, None, now), None);
    }
    let skzi = ctx.gost.provider();
    let rev = revocation(report, chain, ctx, skzi.as_deref(), now);
    let engine = match skzi {
        Some(name) if report.trusted() => GostEngine::Skzi(name),
        _ => GostEngine::Postman,
    };
    (rev, Some(engine))
}

/// Итог по письму из кэша: свежий — как есть; с устаревшим отзывом — перепроверка отзыва в Windows (без воркера).
/// `None` — проверки не было (нужны байты письма и воркер).
pub fn cached(cache: &Cache, id: &str, ctx: &Context, now: i64) -> Option<Verdict> {
    let mut v: Verdict = cache.smime(id).ok().flatten()?;
    if v.stale(now) {
        (v.revocation, v.gost) = confirm(&v.report, &v.chain, ctx, now);
        v.checked_at = now;
        store(cache, id, &v);
    }
    v.report.chain = v.chain.clone();
    Some(v)
}

fn store(cache: &Cache, id: &str, v: &Verdict) {
    if let Err(e) = cache.set_smime(id, v) {
        tracing::warn!("S/MIME verdict is not cached: {e}");
    }
}

/// Отчёт воркера → итог: подтверждение в Windows, запись в кэш, сертификат подписанта — в сертификаты адресата.
pub fn finish(cache: &Cache, id: &str, report: Report, ctx: &Context, now: i64) -> Verdict {
    let chain = report.chain.clone();
    let (revocation, gost) = confirm(&report, &chain, ctx, now);
    let v = Verdict { report, chain, revocation, checked_at: now, gost };
    if v.report.status.signature_ok() || matches!(v.report.status, postman_smime::Status::Invalid) {
        store(cache, id, &v);
    }
    if v.report.trusted() && v.report.address_ok && !matches!(v.revocation, Revocation::Revoked | Revocation::Rejected(_)) {
        remember(cache, &v, now);
    }
    v
}

/// Подпись внутри расшифрованного письма (Ф2): подтверждение в Windows, сертификат подписанта — в сертификаты
/// адресата; итог в кэш **не** пишется — письмо открывается каждый раз заново, а на диске остаётся только шифрованный
/// блоб (решение владельца №4).
pub fn assess(cache: &Cache, report: Report, ctx: &Context, now: i64) -> Verdict {
    let chain = report.chain.clone();
    let (revocation, gost) = confirm(&report, &chain, ctx, now);
    let v = Verdict { report, chain, revocation, checked_at: now, gost };
    if v.report.trusted() && v.report.address_ok && !matches!(v.revocation, Revocation::Revoked | Revocation::Rejected(_)) {
        remember(cache, &v, now);
    }
    v
}

/// Сертификат подписанта — в кэш сертификатов адресата (по адресу «От», совпавшему с сертификатом).
fn remember(cache: &Cache, v: &Verdict, now: i64) {
    let (Some(info), Some(der)) = (v.report.certs.first(), v.chain.first()) else { return };
    let address = v.report.from.as_str();
    let mut list: Vec<KnownCert> = cache.certs(address).ok().flatten().unwrap_or_default();
    list.retain(|c| c.fingerprint != info.fingerprint);
    list.push(KnownCert {
        fingerprint: info.fingerprint.clone(),
        der: der.clone(),
        not_after: info.not_after,
        capabilities: v.report.capabilities.clone(),
        seen_at: now,
        source: "signed".into(),
    });
    if let Err(e) = cache.set_certs(address, &list) {
        tracing::warn!("recipient certificate is not cached: {e}");
    }
}

/// Сертификаты адресата, собранные из подписей (Ф4 выберет из них сертификат шифрования).
pub fn known_certs(cache: &Cache, address: &str) -> Vec<KnownCert> {
    cache.certs(address).ok().flatten().unwrap_or_default()
}

#[cfg(windows)]
pub use windows::confirm_with_root;

/// Есть свой сертификат подписи для адреса `from` (решение №7: «Подписать» включено, только если есть чем).
pub fn can_sign(from: &str, now: i64) -> bool {
    #[cfg(windows)]
    {
        sign_win::can_sign(from, now)
    }
    #[cfg(not(windows))]
    {
        let _ = (from, now);
        false
    }
}

/// Зашифровать своё письмо `raw` (S5-12 Ф4) для подготовленных получателей `to` (себя добавляет вызывающий):
/// RSA/EC — здесь (случайные числа и ECDH — CNG), все получатели ГОСТ — СКЗИ. Смесь ГОСТ и RSA/EC не шифруется:
/// такое письмо делит на два интерфейс (решение владельца 2026-10-22).
pub fn seal_outgoing(raw: &[u8], to: &[postman_smime::seal::To]) -> Result<Vec<u8>, postman_smime::seal::SealError> {
    use postman_smime::seal::SealError;
    let gost = to.iter().filter(|t| t.prepared.key.is_gost()).count();
    if gost > 0 && gost < to.len() {
        return Err(SealError::Failed("GOST and RSA/EC recipients in one message".into()));
    }
    #[cfg(windows)]
    {
        if gost > 0 {
            if SystemGost.provider().is_none() {
                return Err(SealError::Gost);
            }
            return seal_win::seal_gost(raw, to);
        }
        postman_smime::seal::seal_message(raw, to, &seal_win::WindowsOps)
    }
    #[cfg(not(windows))]
    {
        let _ = raw;
        Err(SealError::Failed("encryption needs Windows".into()))
    }
}

/// Письмо «Конфиденциально» по семействам ключей (решение владельца 2026-10-22): есть и ГОСТ, и RSA/EC — два письма
/// (сначала получателям ГОСТ, затем остальным), в каждом — только свои получатели (Кому, Копия, Скрытая) и свой
/// сертификат «себе» того же семейства. Получатель с обоими сертификатами — в письмо RSA/EC. Ошибка — код
/// `smime-seal:<код>[:адрес]`: нет сертификата адресата или своего нужного семейства.
pub fn split_for_encryption(draft: &postman_compose::Draft) -> Result<Vec<postman_compose::Draft>, String> {
    use postman_smime::seal::To;
    if draft.encrypt_to.is_empty() {
        return Ok(vec![draft.clone()]);
    }
    let of = |addr: &str, gost: Option<bool>| -> Option<&To> {
        let addr = addr.trim().to_lowercase();
        draft.encrypt_to.iter().filter(|t| t.prepared.emails.contains(&addr)).find(|t| gost.is_none_or(|g| t.prepared.key.is_gost() == g))
    };
    // Семейство адресата: RSA/EC, если есть; иначе ГОСТ; иначе — нет сертификата.
    let family = |addr: &str| -> Result<bool, String> {
        if of(addr, Some(false)).is_some() {
            Ok(false)
        } else if of(addr, Some(true)).is_some() {
            Ok(true)
        } else {
            Err(format!("smime-seal:no-cert:{addr}"))
        }
    };
    let mut groups: Vec<bool> = Vec::new();
    for a in draft.recipients() {
        let g = family(&a.email)?;
        if !groups.contains(&g) {
            groups.push(g);
        }
    }
    if groups.is_empty() {
        // Черновик без адресатов — себе.
        groups.push(of(&draft.from.email, Some(false)).is_none() && of(&draft.from.email, Some(true)).is_some());
    }
    groups.sort_by_key(|g| !g); // ГОСТ — первым
    let mut out = Vec::new();
    for gost in groups {
        let keep = |list: &[postman_compose::Address]| -> Vec<postman_compose::Address> {
            list.iter().filter(|a| family(&a.email).is_ok_and(|g| g == gost)).cloned().collect()
        };
        let mut d = draft.clone();
        d.to = keep(&draft.to);
        d.cc = keep(&draft.cc);
        d.bcc = keep(&draft.bcc);
        let me = of(&draft.from.email, Some(gost)).ok_or_else(|| format!("smime-seal:no-own-cert:{}", if gost { "gost" } else { "rsa" }))?;
        let mut tos: Vec<To> = Vec::new();
        for t in d.recipients().filter_map(|a| of(&a.email, Some(gost))).chain(std::iter::once(me)) {
            if !tos.iter().any(|x| x.prepared.fingerprint == t.prepared.fingerprint) {
                tos.push(t.clone());
            }
        }
        d.encrypt_to = tos;
        out.push(d);
    }
    Ok(out)
}

/// Кандидаты в сертификаты адресатов (DER): из подписанных писем (кэш) и «Других пользователей» Windows; проверяет и
/// разбирает их воркер (`SmimePrepare`). `own` — свой адрес: ещё свои сертификаты из `My` (себе — всегда).
pub fn recipient_candidates(cache: &Cache, addresses: &[String], own: &str) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    for a in addresses {
        for c in known_certs(cache, a) {
            if !out.contains(&c.der) {
                out.push(c.der);
            }
        }
    }
    #[cfg(windows)]
    {
        for der in seal_win::address_book().into_iter().chain(seal_win::own_certs(own)) {
            if !out.contains(&der) {
                out.push(der);
            }
        }
    }
    #[cfg(not(windows))]
    let _ = own;
    out
}

/// Объявил ли адресат AES-GCM (по `SMIMECapabilities` его подписанных писем).
pub fn declares_gcm(cache: &Cache, address: &str) -> bool {
    known_certs(cache, address).iter().any(|c| postman_smime::seal::declares_gcm(&c.capabilities))
}

/// Подписать своё письмо `raw` сертификатом адреса `from` (S5-12 Ф3): RSA и ECDSA — CNG, ГОСТ — только СКЗИ
/// (решение владельца 2026-10-22). `hwnd` — окно для PIN ОС (`None` — молча: нужен PIN — ошибка `NeedPin`).
pub fn sign_outgoing(raw: &[u8], from: &str, hwnd: Option<isize>, now: i64) -> Result<Vec<u8>, postman_smime::sign::SignError> {
    #[cfg(windows)]
    {
        let gost = SystemGost.provider();
        sign_win::sign_message(raw, from, hwnd, now, gost)
    }
    #[cfg(not(windows))]
    {
        let _ = (raw, hwnd, now);
        Err(postman_smime::sign::SignError::NoCertificate(from.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use postman_smime::testkit::{self, Head, NOW, Pki, Sign};

    fn letter_report(pki: &Pki) -> Report {
        let m = testkit::signed_letter(
            &Head { from: "user1@itt.test", to: "me@itt.test", subject: "s", date: "Wed, 21 Oct 2026 10:00:00 +0300" },
            "<p>x</p>",
            &pki.user,
            &pki.user_key,
            &[&pki.root],
            &Sign::default(),
        );
        postman_worker::smime::verify(&m, &pki.trust(), NOW)
    }

    /// Повторное открытие — из кэша без воркера; сертификат подписанта запомнен по адресу.
    #[test]
    fn verdict_is_cached_and_certificate_remembered() {
        let cache = Cache::open_in_memory().unwrap();
        let pki = Pki::rsa("Root", "User", "user1@itt.test");
        let anchors = Context::fixed(pki.trust());
        assert!(cached(&cache, "m1", &anchors, NOW as i64).is_none());
        let v = finish(&cache, "m1", letter_report(&pki), &anchors, NOW as i64);
        assert_eq!(v.report.status, postman_smime::Status::Valid);
        assert_eq!(v.revocation, Revocation::NotChecked, "переданные якоря — без Windows");
        let again = cached(&cache, "m1", &anchors, NOW as i64 + 10).unwrap();
        assert_eq!(again.report.status, v.report.status);
        assert_eq!(again.report.chain, v.chain, "цепочка — из кэша");
        let certs = known_certs(&cache, "USER1@itt.test");
        assert_eq!(certs.len(), 1);
        assert_eq!(certs[0].der, pki.user.der);
        // Повтор не дублирует сертификат.
        finish(&cache, "m2", letter_report(&pki), &anchors, NOW as i64);
        assert_eq!(known_certs(&cache, "user1@itt.test").len(), 1);
    }

    #[test]
    fn stale_only_for_trusted_non_gost() {
        let pki = Pki::rsa("Root", "User", "user1@itt.test");
        let report = letter_report(&pki);
        let v = Verdict { report, chain: vec![], revocation: Revocation::Unknown, checked_at: 0, gost: None };
        assert!(v.stale(RECHECK_AFTER + 1));
        assert!(!v.stale(RECHECK_AFTER - 1));
        let mut v2 = v.clone();
        v2.revocation = Revocation::NotChecked;
        assert!(!v2.stale(RECHECK_AFTER * 10));
    }

    /// Запрещённый сертификат (Disallowed) — «не доверен» уже в воркере: Windows не спрашиваем.
    #[test]
    fn disallowed_is_not_confirmed() {
        let cache = Cache::open_in_memory().unwrap();
        let pki = Pki::rsa("Root", "User", "user1@itt.test");
        let mut trust = pki.trust();
        trust.disallowed.push(pki.root.der.clone());
        let m = testkit::signed_letter(
            &Head { from: "user1@itt.test", to: "me@itt.test", subject: "s", date: "Wed, 21 Oct 2026 10:00:00 +0300" },
            "<p>x</p>",
            &pki.user,
            &pki.user_key,
            &[&pki.root],
            &Sign::default(),
        );
        let report = postman_worker::smime::verify(&m, &trust, NOW);
        let v = finish(&cache, "m", report, &Context::system(), NOW as i64);
        assert_eq!(v.report.status, postman_smime::Status::Distrusted);
        assert_eq!(v.revocation, Revocation::NotChecked);
        assert!(known_certs(&cache, "user1@itt.test").is_empty());
    }

    /// ГОСТ: движок по СКЗИ (провайдер подменён) — без СКЗИ «Postman», отзыв не проверен; с СКЗИ — его имя. Обычная
    /// подпись RSA — без движка ГОСТ.
    #[test]
    fn gost_engine_follows_the_provider() {
        let cache = Cache::open_in_memory().unwrap();
        let set = postman_smime::oid::TC26_256_A;
        let pki = Pki::build(testkit::Key::gost("r", set), testkit::Key::gost("u", set), "ГОСТ Корень", "Сидоров", "user1@itt.test");
        let without = Context::fixed(pki.trust());
        let v = finish(&cache, "g1", letter_report(&pki), &without, NOW as i64);
        assert_eq!((v.gost, v.revocation), (Some(GostEngine::Postman), Revocation::NotCheckedGost));
        let with = Context { gost: Arc::new(FixedGost(Some("Crypto-Pro GOST R 34.10-2012 CSP".into()))), ..Context::fixed(pki.trust()) };
        let v = finish(&cache, "g2", letter_report(&pki), &with, NOW as i64);
        assert_eq!(v.gost, Some(GostEngine::Skzi("Crypto-Pro GOST R 34.10-2012 CSP".into())));
        assert_eq!(v.revocation, Revocation::NotChecked, "переданные якоря — Windows не спрашиваем");
        let rsa = Pki::rsa("Root", "User", "user1@itt.test");
        assert_eq!(finish(&cache, "r", letter_report(&rsa), &with, NOW as i64).gost, None);
    }

    /// Деление письма «Конфиденциально»: ГОСТ и RSA — два письма, у каждого свои адресаты и «себе» своего семейства;
    /// без сертификата — код с адресом.
    #[test]
    fn split_by_key_family() {
        use postman_compose::{Address, Draft};
        use postman_smime::seal::To;
        let rsa = Pki::rsa("Root", "Петров", "p@itt.test");
        let me_rsa = Pki::rsa("Root", "Я", "me@itt.test");
        let set = postman_smime::oid::TC26_256_A;
        let gost = Pki::build(testkit::Key::gost("r", set), testkit::Key::gost("u", set), "ГОСТ УЦ", "Сидоров", "s@itt.test");
        let me_gost = Pki::build(testkit::Key::gost("r", set), testkit::Key::gost("me", set), "ГОСТ УЦ", "Я", "me@itt.test");
        let to = |p: &Pki| To { prepared: postman_smime::prepare::prepare(&p.user.der, &[], &p.trust(), NOW).unwrap(), gcm: false };
        let draft = Draft {
            from: Address::new("Я", "me@itt.test"),
            to: vec![Address::new("Петров", "p@itt.test"), Address::new("Сидоров", "S@itt.test")],
            bcc: vec![Address::new("", "p@itt.test")],
            encrypt_to: vec![to(&rsa), to(&gost), to(&me_rsa), to(&me_gost)],
            ..Default::default()
        };
        let parts = split_for_encryption(&draft).unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].to.iter().map(|a| a.email.as_str()).collect::<Vec<_>>(), ["S@itt.test"], "ГОСТ — первым");
        assert!(parts[0].bcc.is_empty() && parts[0].encrypt_to.iter().all(|t| t.prepared.key.is_gost()) && parts[0].encrypt_to.len() == 2);
        assert_eq!(parts[1].to.iter().map(|a| a.email.as_str()).collect::<Vec<_>>(), ["p@itt.test"]);
        assert_eq!(parts[1].bcc.len(), 1);
        assert!(parts[1].encrypt_to.iter().all(|t| !t.prepared.key.is_gost()) && parts[1].encrypt_to.len() == 2, "себе — тоже");
        // Только RSA — одно письмо; себе ГОСТ не нужен.
        let only = Draft { to: vec![Address::new("", "p@itt.test")], ..draft.clone() };
        assert_eq!(split_for_encryption(&only).unwrap().len(), 1);
        // Нет сертификата адресата; нет своего ГОСТ.
        let missing = Draft { to: vec![Address::new("", "x@itt.test")], ..draft.clone() };
        assert_eq!(split_for_encryption(&missing).unwrap_err(), "smime-seal:no-cert:x@itt.test");
        let no_own = Draft { encrypt_to: vec![to(&rsa), to(&gost), to(&me_rsa)], ..draft.clone() };
        assert_eq!(split_for_encryption(&no_own).unwrap_err(), "smime-seal:no-own-cert:gost");
        // Открытое письмо — как есть.
        let plain = Draft { encrypt_to: vec![], ..draft };
        assert_eq!(split_for_encryption(&plain).unwrap().len(), 1);
    }
}
