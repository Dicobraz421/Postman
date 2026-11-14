//! Данные клиента в базе профиля (решение владельца 2026-11-14 «Все что можно в бд»,
//! `docs/decisions/2026-11-14-all-in-db.md`; ADR 0006 «Что не в базе»): то, что раньше лежало файлами рядом с базой
//! и в папке данных, — в таблице `local` хранилища (`postman_cache`, ключ — строка, значение — байты, крупные байты —
//! частями содержимого по хэшу).
//!
//! | Ключ | Что | Значение | Части |
//! |---|---|---|---|
//! | [`LABELS`] | копия таблиц категорий ([`labels::Book`]) | JSON | — |
//! | [`ACCOUNT`] | подписи, шаблоны, профиль учётки ([`AccountPrefs`]) | JSON | картинки подписей по порядку [`AccountPrefs::sign_images`] |
//! | [`ICS`]`<ссылка>` | скачанная подписка на календарь | JSON `{etag}` | файл `.ics` |
//! | [`NOTICE`]`<время>/<номер>` | запись истории уведомлений ([`Notice`]) | JSON | — |
//! | [`SMIME_SEEN`] | промежуточные сертификаты из писем (`smime::seen`) | длина + DER подряд | — |
//! | [`DRAFT`]`<ключ письма>` | черновик на сервере: реплики ответа на фрагменты и правило вложений беседы ([`DraftLayout`]) | JSON | — |
//!
//! Перенос прежних файлов — [`adopt`] при открытии базы (поток открытия, не интерфейса): одной транзакцией, файлы — в
//! `*.bak`, при следующем удачном открытии — удаляются.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use postman_cache::{Cache, LocalPut};
use serde::{Deserialize, Serialize};

use crate::labels::{self, Book};
use crate::prefs::{AccountPrefs, MAX_SIGN_IMAGE, Prefs, image_mime};

/// Копия таблиц категорий.
pub const LABELS: &str = "labels";
/// Настройки учётной записи.
pub const ACCOUNT: &str = "account";
/// Подписка на календарь: префикс, за ним — ссылка.
pub const ICS: &str = "ics/";
/// Запись истории уведомлений: префикс, за ним — время (секунды, 20 знаков) и номер.
pub const NOTICE: &str = "notice/";
/// Промежуточные сертификаты из писем.
pub const SMIME_SEEN: &str = "smime/intermediates";
/// Черновик на сервере (ключ письма в кэше): префикс, за ним — ключ.
pub const DRAFT: &str = "draft/";
/// История уведомлений — за столько секунд.
pub const NOTICE_KEEP_SECS: i64 = 7 * 24 * 3600;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("local data: {0}")]
    Store(#[from] postman_cache::Error),
    #[error("local data: {0}")]
    Json(#[from] serde_json::Error),
    #[error("local data version {0} is newer than this Postman")]
    Version(u32),
    #[error("old data file: {0}")]
    File(#[from] crate::prefs::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

fn put(key: impl Into<String>, value: Vec<u8>, parts: Vec<(Vec<u8>, String)>) -> LocalPut {
    LocalPut { key: key.into(), value, parts }
}

// --- категории ---

/// Таблицы категорий в базе профиля ([`labels::Store`]).
pub struct Labels(pub Cache);

impl labels::Store for Labels {
    fn load(&self) -> std::result::Result<Option<Book>, labels::StoreError> {
        match self.0.local(LABELS)? {
            Some(doc) => labels::parse_book(&String::from_utf8_lossy(&doc.value)),
            None => Ok(None),
        }
    }

    fn save(&self, book: &Book) -> std::result::Result<(), labels::StoreError> {
        Ok(self.0.put_local(&put(LABELS, serde_json::to_vec(book)?, Vec::new()))?)
    }
}

// --- настройки учётной записи ---

#[derive(Deserialize)]
struct Version {
    #[serde(default)]
    version: u32,
}

/// Запись настроек учётки: JSON и картинки подписей частями по порядку [`AccountPrefs::sign_images`].
fn account_put(prefs: &AccountPrefs) -> Result<LocalPut> {
    let parts = prefs.sign_images().map(|i| (i.data.to_vec(), i.mime.clone())).collect();
    Ok(put(ACCOUNT, serde_json::to_vec(prefs)?, parts))
}

/// Подписи, шаблоны и профиль учётки; записи нет — по умолчанию. Картинки, которой нет в базе или которая не своего
/// типа, нет и в подписи (как у прежнего файла).
pub fn account_prefs(cache: &Cache) -> Result<AccountPrefs> {
    let Some(doc) = cache.local(ACCOUNT)? else { return Ok(AccountPrefs::default()) };
    let Version { version } = serde_json::from_slice(&doc.value)?;
    if version > crate::prefs::VERSION {
        return Err(Error::Version(version));
    }
    let mut prefs = serde_json::from_slice::<AccountPrefs>(&doc.value)?.normalized();
    let mut hashes = doc.parts.into_iter();
    for s in &mut prefs.signatures {
        let mut images = Vec::with_capacity(s.images.len());
        for mut img in std::mem::take(&mut s.images) {
            let Some(hash) = hashes.next() else { break };
            match cache.local_part(hash)? {
                Some(data) if data.len() <= MAX_SIGN_IMAGE && image_mime(&data) == Some(img.mime.as_str()) => {
                    img.data = std::sync::Arc::new(data);
                    images.push(img);
                }
                _ => tracing::warn!(cid = %img.cid, "local: signature image missing in the store, dropped"),
            }
        }
        s.images = images;
    }
    Ok(prefs)
}

/// Записать настройки учётки с картинками: прежние картинки, на которые больше никто не ссылается, уходят из базы той
/// же транзакцией.
pub fn save_account_prefs(cache: &Cache, prefs: &AccountPrefs) -> Result<()> {
    Ok(cache.put_local(&account_put(prefs)?)?)
}

// --- подписки на календари ---

/// Скачанная подписка: файл `.ics` и ETag сервера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    pub body: Vec<u8>,
    pub etag: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct SubMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    etag: Option<String>,
}

fn ics_key(url: &str) -> String {
    format!("{ICS}{}", url.trim())
}

fn ics_put(url: &str, body: Vec<u8>, etag: Option<String>) -> Result<LocalPut> {
    Ok(put(ics_key(url), serde_json::to_vec(&SubMeta { etag })?, vec![(body, "text/calendar".into())]))
}

/// Скачанная подписка по ссылке; ещё не скачана — `None`.
pub fn subscription(cache: &Cache, url: &str) -> Result<Option<Subscription>> {
    let Some(doc) = cache.local(&ics_key(url))? else { return Ok(None) };
    let meta: SubMeta = serde_json::from_slice(&doc.value).unwrap_or_default();
    let Some(body) = doc.parts.first().map(|h| cache.local_part(*h)).transpose()?.flatten() else { return Ok(None) };
    Ok(Some(Subscription { body, etag: meta.etag }))
}

/// ETag последней скачанной версии (для `If-None-Match`), без чтения самого файла.
pub fn subscription_etag(cache: &Cache, url: &str) -> Result<Option<String>> {
    let Some(doc) = cache.local(&ics_key(url))? else { return Ok(None) };
    Ok(serde_json::from_slice::<SubMeta>(&doc.value).ok().and_then(|m| m.etag))
}

/// Новая версия подписки: прежний файл заменяется одной транзакцией.
pub fn set_subscription(cache: &Cache, url: &str, body: Vec<u8>, etag: Option<String>) -> Result<()> {
    Ok(cache.put_local(&ics_put(url, body, etag)?)?)
}

/// Подписка удалена из настроек: её файл — из базы.
pub fn remove_subscription(cache: &Cache, url: &str) -> Result<()> {
    Ok(cache.remove_local(&[ics_key(url).as_str()])?)
}

/// Прежнее имя файла подписки (`subscriptions\<id>.ics`): хэш ссылки. Ключ подписки в модели календаря (`ics:<id>`).
pub fn subscription_id(url: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.trim().hash(&mut h);
    format!("{:016x}", h.finish())
}

// --- история уведомлений ---

/// Запись истории уведомлений: время (секунды), уровень (`e`, `w`, `s`, `i`), текст. Поля — как в прежнем
/// `notices.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    pub t: i64,
    pub l: char,
    pub x: String,
}

/// Номер записи внутри секунды: две записи одной секунды не затирают друг друга.
static NOTICE_SEQ: AtomicU32 = AtomicU32::new(0);

fn notice_key(t: i64, n: u32) -> String {
    format!("{NOTICE}{:020}/{n:010}", t.max(0))
}

fn notice_put(n: &Notice, seq: u32) -> Result<LocalPut> {
    Ok(put(notice_key(n.t, seq), serde_json::to_vec(n)?, Vec::new()))
}

/// Дописать запись истории.
pub fn push_notice(cache: &Cache, n: &Notice) -> Result<()> {
    Ok(cache.put_local(&notice_put(n, NOTICE_SEQ.fetch_add(1, Ordering::Relaxed))?)?)
}

/// История за [`NOTICE_KEEP_SECS`] до `now`, старые первыми; старше — удаляются из базы, битые записи — мимо.
pub fn recent_notices(cache: &Cache, now: i64) -> Result<Vec<Notice>> {
    let mut old = Vec::new();
    let mut out = Vec::new();
    for (key, doc) in cache.local_prefix(NOTICE)? {
        match serde_json::from_slice::<Notice>(&doc.value) {
            Ok(n) if now - n.t <= NOTICE_KEEP_SECS => out.push(n),
            _ => old.push(key),
        }
    }
    if !old.is_empty() {
        cache.remove_local(&old.iter().map(String::as_str).collect::<Vec<_>>())?;
    }
    Ok(out)
}

// --- черновики: реплики и правило вложений беседы ---

/// Черновик на сервере — обычное письмо с уже собранным текстом. Как он устроен в окне письма — здесь, по ключу письма
/// в кэше: свой текст, реплики «цитата → ответ» (MSGR-01), хвост (подпись и полная цитата) и «Не отправлять» вложения
/// беседы новым участникам (T3-14). Открыли черновик снова (из «Черновиков», после «Отменить») — окно собирает его
/// из этой записи, если текст на сервере тот же ([`DraftLayout::markdown`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftLayout {
    /// Свой текст над репликами.
    #[serde(default)]
    pub head: String,
    #[serde(default)]
    pub replies: Vec<Reply>,
    /// Подпись и полная цитата ответа — под репликами.
    #[serde(default)]
    pub tail: String,
    /// Подпись, стоящая в тексте (смена подписи заменяет её).
    #[serde(default)]
    pub signature: String,
    /// «Не отправлять» вложения беседы новым участникам.
    #[serde(default)]
    pub share_off: bool,
    /// Текст письма, собранный из частей, — как ушёл на сервер.
    #[serde(default)]
    pub markdown: String,
}

/// Реплика черновика: фрагмент письма и ответ на него (`postman_compose::fragments::Fragment` без номера).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    #[serde(default)]
    pub letter: String,
    #[serde(default)]
    pub who: String,
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub answer: String,
}

impl From<&postman_compose::fragments::Fragment> for Reply {
    fn from(f: &postman_compose::fragments::Fragment) -> Self {
        Reply { letter: f.letter.clone(), who: f.who.clone(), date: f.date.clone(), text: f.text.clone(), answer: f.answer.clone() }
    }
}

impl From<&Reply> for postman_compose::fragments::Fragment {
    fn from(r: &Reply) -> Self {
        let mut f = postman_compose::fragments::Fragment::new(&r.letter, &r.who, &r.date, &r.text);
        f.answer.clone_from(&r.answer);
        f
    }
}

impl DraftLayout {
    /// Хранить нечего: реплик нет и вложения беседы не отключены.
    pub fn is_plain(&self) -> bool {
        self.replies.is_empty() && !self.share_off
    }

    /// Текст черновика на сервере — тот же, что записан здесь (пробелы и переводы строк не в счёт): черновик не
    /// правили в другой программе.
    pub fn fits(&self, markdown: &str) -> bool {
        let squeeze = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
        squeeze(&self.markdown) == squeeze(markdown)
    }
}

fn draft_key(id: &str) -> String {
    format!("{DRAFT}{id}")
}

/// Как устроен черновик `id` (ключ письма в кэше); записи нет — `None`.
pub fn draft_layout(cache: &Cache, id: &str) -> Result<Option<DraftLayout>> {
    let Some(doc) = cache.local(&draft_key(id))? else { return Ok(None) };
    Ok(Some(serde_json::from_slice(&doc.value)?))
}

/// Черновик сохранён на сервере как `id`: записать, как он устроен, и убрать записи прежних версий `old` (их заменила
/// эта). Хранить нечего ([`DraftLayout::is_plain`]) — записи `id` тоже нет.
pub fn keep_draft_layout(cache: &Cache, id: Option<&str>, layout: Option<&DraftLayout>, old: &[&str]) -> Result<()> {
    let mut gone: Vec<String> = old.iter().filter(|o| Some(**o) != id).map(|o| draft_key(o)).collect();
    match (id, layout.filter(|l| !l.is_plain())) {
        (Some(id), Some(l)) => cache.put_local(&put(draft_key(id), serde_json::to_vec(l)?, Vec::new()))?,
        (Some(id), None) => gone.push(draft_key(id)),
        (None, _) => {}
    }
    if !gone.is_empty() {
        cache.remove_local(&gone.iter().map(String::as_str).collect::<Vec<_>>())?;
    }
    Ok(())
}

/// Записи черновиков, писем которых больше нет в кэше (отправлены, удалены): убрать. Сколько убрано.
pub fn sweep_draft_layouts(cache: &Cache) -> Result<usize> {
    let keys: Vec<String> = cache.local_prefix(DRAFT)?.into_iter().map(|(k, _)| k).collect();
    if keys.is_empty() {
        return Ok(0);
    }
    let ids: Vec<String> = keys.iter().map(|k| k[DRAFT.len()..].to_owned()).collect();
    let alive: std::collections::HashSet<String> = cache.rows_by_ids(&ids)?.into_iter().map(|r| r.id).collect();
    let gone: Vec<&str> = keys.iter().zip(&ids).filter(|(_, id)| !alive.contains(*id)).map(|(k, _)| k.as_str()).collect();
    if !gone.is_empty() {
        cache.remove_local(&gone)?;
    }
    Ok(gone.len())
}

// --- перенос прежних файлов ---

/// Прежние файлы данных клиента: рядом с базой `cache` и в папке данных `data` — папке самой базы (файлы общей
/// папки попадают в базу, открытую первой; база замера или теста в своей папке чужих файлов не видит).
#[derive(Debug, Clone)]
pub struct OldFiles {
    pub cache: PathBuf,
    pub data: PathBuf,
}

/// Что сделал [`adopt`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Adopted {
    /// Перенос уже был (или переносить нечего): остатки `*.bak` убраны.
    Already,
    /// Перенесено записей; файлы — в `*.bak`.
    Moved(usize),
}

impl OldFiles {
    /// Прежние файлы базы `cache`: папка данных — её папка.
    pub fn of(cache: &Path) -> Self {
        Self { cache: cache.to_owned(), data: cache.parent().map(Path::to_owned).unwrap_or_default() }
    }

    fn labels(&self) -> PathBuf {
        labels::LocalFile::beside(&self.cache).0
    }

    fn account(&self) -> PathBuf {
        AccountPrefs::beside(&self.cache)
    }

    fn images(&self) -> PathBuf {
        AccountPrefs::images_dir(&self.account())
    }

    fn subscriptions(&self) -> PathBuf {
        self.data.join("subscriptions")
    }

    fn notices(&self) -> PathBuf {
        self.data.join("notices.jsonl")
    }

    fn seen(&self) -> PathBuf {
        self.data.join(crate::smime::SEEN_FILE)
    }

    /// Что переносится: файлы и папки.
    fn moved(&self) -> [PathBuf; 6] {
        [self.labels(), self.account(), self.images(), self.subscriptions(), self.notices(), self.seen()]
    }
}

fn bak(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".bak");
    s.into()
}

fn remove_any(path: &Path) -> std::io::Result<()> {
    let r = if path.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
    match r {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        r => r,
    }
}

/// Записи из прежних файлов. Файл не читается (испорчен, новее этой версии) — не переносится и остаётся на месте:
/// причина — в журнал, остальные переносятся; ответ — записи и файлы, которые можно убрать в `*.bak`.
fn collect(files: &OldFiles, now: i64) -> (Vec<LocalPut>, Vec<PathBuf>) {
    let mut puts = Vec::new();
    let mut done = Vec::new();
    let exists = |p: &Path| p.exists();

    let path = files.labels();
    if exists(&path) {
        use labels::Store as _;
        match labels::LocalFile(path.clone()).load() {
            Ok(Some(book)) => match serde_json::to_vec(&book) {
                Ok(v) => {
                    puts.push(put(LABELS, v, Vec::new()));
                    done.push(path);
                }
                Err(e) => tracing::error!("local: category table not moved: {e}"),
            },
            // Прежний формат — только копия: таблицу пришлёт сервер.
            Ok(None) => done.push(path),
            Err(e) => tracing::error!("local: category table not moved, file kept: {e}"),
        }
    }

    let (path, images) = (files.account(), files.images());
    if exists(&path) {
        match AccountPrefs::load(&path).map_err(Error::from).and_then(|p| account_put(&p)) {
            Ok(p) => {
                puts.push(p);
                done.extend([path, images]);
            }
            Err(e) => tracing::error!("local: account settings not moved, file kept: {e}"),
        }
    } else if exists(&images) {
        done.push(images);
    }

    let dir = files.subscriptions();
    if exists(&dir) {
        // Подписки — в общих настройках; файл — по хэшу ссылки. Файл без подписки в настройках — остаток, не нужен.
        let urls = Prefs::load(&files.data.join(crate::prefs::FILE)).map(|p| p.calendar.subscriptions).unwrap_or_default();
        for s in urls {
            let id = subscription_id(&s.url);
            let Ok(body) = std::fs::read(dir.join(format!("{id}.ics"))) else { continue };
            let etag = std::fs::read_to_string(dir.join(format!("{id}.etag"))).ok().map(|t| t.trim().to_owned()).filter(|t| !t.is_empty());
            match ics_put(&s.url, body, etag) {
                Ok(p) => puts.push(p),
                Err(e) => tracing::error!("local: calendar subscription not moved: {e}"),
            }
        }
        done.push(dir);
    }

    let path = files.notices();
    if exists(&path) {
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let lines = text.lines().filter_map(|l| serde_json::from_str::<Notice>(l).ok()).filter(|n| now - n.t <= NOTICE_KEEP_SECS);
                for (i, n) in lines.enumerate() {
                    if let Ok(p) = notice_put(&n, i as u32) {
                        puts.push(p);
                    }
                }
                done.push(path);
            }
            Err(e) => tracing::error!("local: notification history not moved, file kept: {e}"),
        }
    }

    let path = files.seen();
    if exists(&path) {
        let list = crate::smime::seen_file(&path);
        if !list.is_empty() {
            puts.push(put(SMIME_SEEN, crate::smime::seen_bytes(&list), Vec::new()));
        }
        done.push(path);
    }
    (puts, done)
}

/// Перенести прежние файлы в базу (решение 2026-11-14): при первом открытии базы новой версией — все записи одной
/// транзакцией с отметкой «перенесено» (записанное уже в базу не перезаписывается), затем файлы — в `*.bak`; при
/// следующем открытии (отметка есть) `*.bak` удаляются. Транзакция не прошла — файлы не тронуты, перенос — при
/// следующем запуске. Звать из потока открытия базы: файлы читаются здесь. `now` — секунды (срок истории).
pub fn adopt(cache: &Cache, files: &OldFiles, now: i64) -> Result<Adopted> {
    let adopted = if cache.local_imported()? {
        for p in files.moved() {
            if let Err(e) = remove_any(&bak(&p)) {
                tracing::warn!("local: old file backup not removed: {e}");
            }
        }
        Adopted::Already
    } else {
        let (puts, done) = collect(files, now);
        let n = cache.import_local(&puts)?.unwrap_or(0);
        for p in done.into_iter().filter(|p| p.exists()) {
            if let Err(e) = std::fs::rename(&p, bak(&p)) {
                tracing::warn!("local: moved file not renamed to .bak: {e}");
            }
        }
        if n > 0 {
            tracing::info!(records = n, "local: old data files moved into the store");
        }
        Adopted::Moved(n)
    };
    crate::smime::open_seen(cache);
    Ok(adopted)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::labels::{Categories, Store as _};
    use crate::prefs::{CalendarSubscription, SignImage, Signature};

    const NOW: i64 = 1_800_000_000;
    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];

    fn store(dir: &Path) -> (Cache, OldFiles) {
        let path = dir.join("account-u-1234.redb");
        (Cache::open(&path).unwrap(), OldFiles::of(&path))
    }

    fn book() -> Book {
        Book::own(Categories::defaults(|id| format!("[{id}]")))
    }

    fn prefs_with_logo() -> AccountPrefs {
        let img = SignImage::new("logo.png", PNG.to_vec(), 42).unwrap();
        let mut a = AccountPrefs::default();
        let text = format!("Иван\n{}", img.markdown());
        a.signatures.push(Signature { id: "s1".into(), name: "Logo".into(), identity: None, text, images: vec![img] });
        a
    }

    fn tmp() -> tempfile::TempDir {
        tempfile::Builder::new().prefix("postman-local-").tempdir().unwrap()
    }

    /// Таблица категорий — в базе, туда и обратно; запись новее этой версии — ошибка.
    #[test]
    fn labels_in_store() {
        let c = Cache::open_in_memory().unwrap();
        let s = Labels(c.clone());
        assert!(s.load().unwrap().is_none());
        s.save(&book()).unwrap();
        assert_eq!(s.load().unwrap(), Some(book()));
        c.put_local(&put(LABELS, br#"{"version": 99, "accounts": {}}"#.to_vec(), Vec::new())).unwrap();
        assert!(matches!(s.load(), Err(labels::StoreError::Version(99))));
    }

    /// Подпись с картинкой — в базе: картинка частью по хэшу, после правки без картинки часть уходит.
    #[test]
    fn account_prefs_in_store() {
        let c = Cache::open_in_memory().unwrap();
        assert_eq!(account_prefs(&c).unwrap(), AccountPrefs::default());
        let a = prefs_with_logo();
        save_account_prefs(&c, &a).unwrap();
        let back = account_prefs(&c).unwrap();
        assert_eq!(back, a);
        assert_eq!(back.sign_images().next().map(|i| i.data.as_slice()), Some(PNG));
        assert!(c.check_refs().unwrap().is_empty(), "вхождения части сходятся");
        let mut b = back;
        b.remove_signature("s1");
        save_account_prefs(&c, &b).unwrap();
        assert!(account_prefs(&c).unwrap().signatures.is_empty());
        assert!(c.check_refs().unwrap().is_empty());
        assert_eq!(c.store_stats().unwrap().parts, 0, "картинка без ссылок удалена");
    }

    /// Подписка: файл и ETag заменяются новой версией, удаляются вместе с подпиской.
    #[test]
    fn subscription_in_store() {
        let c = Cache::open_in_memory().unwrap();
        let url = "https://cal.example.test/a.ics";
        assert_eq!(subscription(&c, url).unwrap(), None);
        set_subscription(&c, url, b"BEGIN:VCALENDAR".to_vec(), Some("\"1\"".into())).unwrap();
        set_subscription(&c, url, b"BEGIN:VCALENDAR 2".to_vec(), Some("\"2\"".into())).unwrap();
        let got = subscription(&c, &format!(" {url} ")).unwrap().unwrap();
        assert_eq!((got.body.as_slice(), got.etag.as_deref()), (&b"BEGIN:VCALENDAR 2"[..], Some("\"2\"")));
        assert_eq!(subscription_etag(&c, url).unwrap().as_deref(), Some("\"2\""));
        remove_subscription(&c, url).unwrap();
        assert_eq!(subscription(&c, url).unwrap(), None);
        assert!(c.check_refs().unwrap().is_empty());
        assert_eq!(c.store_stats().unwrap().parts, 0);
    }

    /// История: две записи одной секунды, старше недели — удаляется при чтении.
    #[test]
    fn notices_in_store() {
        let c = Cache::open_in_memory().unwrap();
        let n = |t: i64, x: &str| Notice { t, l: 'e', x: x.into() };
        push_notice(&c, &n(NOW - NOTICE_KEEP_SECS - 1, "old")).unwrap();
        push_notice(&c, &n(NOW, "a")).unwrap();
        push_notice(&c, &n(NOW, "b")).unwrap();
        assert_eq!(recent_notices(&c, NOW).unwrap(), [n(NOW, "a"), n(NOW, "b")]);
        assert_eq!(c.local_prefix(NOTICE).unwrap().len(), 2, "старая удалена");
    }

    /// Черновик в базе: реплики и «Не отправлять» — по ключу версии; новая версия убирает прежнюю, простой черновик
    /// записи не держит; записи писем, которых нет в кэше, убираются.
    #[test]
    fn draft_layout_in_store() {
        let c = Cache::open_in_memory().unwrap();
        let reply = Reply {
            letter: "m1".into(), who: "Анна".into(), date: "17.10.2026 12:00".into(), text: "Сроки?".into(), answer: "Да.".into()
        };
        let l = DraftLayout {
            head: "Коллеги,".into(),
            replies: vec![reply.clone()],
            tail: "С уважением".into(),
            signature: "С уважением".into(),
            share_off: true,
            markdown: "Коллеги,\n\n> Анна, 17.10.2026 12:00:\n> Сроки?\n\nДа.\n\nС уважением".into(),
        };
        keep_draft_layout(&c, Some("d1"), Some(&l), &[]).unwrap();
        assert_eq!(draft_layout(&c, "d1").unwrap(), Some(l.clone()));
        assert!(l.fits("Коллеги,\r\n\r\n> Анна, 17.10.2026 12:00:\r\n> Сроки?\r\n\r\nДа.\r\n\r\nС уважением\r\n"));
        assert!(!l.fits("Коллеги, правка в другой программе"));
        let f: postman_compose::fragments::Fragment = (&reply).into();
        assert_eq!((f.letter.as_str(), f.answer.as_str()), ("m1", "Да."));
        // Новая версия — под своим ключом, прежняя уходит.
        keep_draft_layout(&c, Some("d2"), Some(&l), &["d1"]).unwrap();
        assert_eq!(draft_layout(&c, "d1").unwrap(), None);
        // Реплик нет и вложения не отключены — хранить нечего.
        keep_draft_layout(&c, Some("d2"), Some(&DraftLayout::default()), &[]).unwrap();
        assert_eq!(draft_layout(&c, "d2").unwrap(), None);
        // Письма «d3» в кэше нет — запись убирается; «d4» есть — остаётся.
        keep_draft_layout(&c, Some("d3"), Some(&l), &[]).unwrap();
        keep_draft_layout(&c, Some("d4"), Some(&l), &[]).unwrap();
        let row = postman_cache::EmailRow {
            id: "d4".into(),
            thread_id: "t".into(),
            mailbox_ids: vec!["drafts".into()],
            received_at: NOW,
            subject: String::new(),
            from_name: String::new(),
            from_addr: "me@corp.example".into(),
            preview: String::new(),
            seen: true,
            flagged: false,
            has_attachment: false,
            size: 0,
            keywords: Vec::new(),
        };
        c.upsert_emails(&[row]).unwrap();
        assert_eq!(sweep_draft_layouts(&c).unwrap(), 1);
        assert!(draft_layout(&c, "d3").unwrap().is_none() && draft_layout(&c, "d4").unwrap().is_some());
    }

    /// Обновление со старой версии: все файлы — в базу одной транзакцией, файлы — в `*.bak`; следующее открытие
    /// убирает `*.bak`, повторного переноса нет.
    #[test]
    fn adopt_moves_every_file_once() {
        let d = tmp();
        let (c, files) = store(d.path());
        // Прежние файлы: метки, настройки с картинкой, подписка, история, промежуточные.
        labels::LocalFile::beside(&files.cache).save(&book()).unwrap();
        prefs_with_logo().save(&files.account()).unwrap();
        let url = "https://cal.example.test/a.ics";
        let mut p = Prefs::default();
        p.calendar.subscriptions.push(CalendarSubscription { url: url.into(), name: "A".into() });
        p.save(&d.path().join(crate::prefs::FILE)).unwrap();
        let subs = files.subscriptions();
        std::fs::create_dir_all(&subs).unwrap();
        let id = subscription_id(url);
        std::fs::write(subs.join(format!("{id}.ics")), b"BEGIN:VCALENDAR").unwrap();
        std::fs::write(subs.join(format!("{id}.etag")), "\"7\"").unwrap();
        std::fs::write(subs.join("0000000000000000.ics"), b"orphan").unwrap();
        let line = |t: i64, x: &str| serde_json::to_string(&Notice { t, l: 'w', x: x.into() }).unwrap() + "\n";
        std::fs::write(files.notices(), line(NOW - 60, "Не отправлено") + &line(NOW - NOTICE_KEEP_SECS - 1, "old") + "bad\n").unwrap();
        let der = vec![7u8; 30];
        std::fs::write(files.seen(), crate::smime::seen_bytes(std::slice::from_ref(&der))).unwrap();

        assert_eq!(adopt(&c, &files, NOW).unwrap(), Adopted::Moved(5));
        assert_eq!(Labels(c.clone()).load().unwrap(), Some(book()));
        let a = account_prefs(&c).unwrap();
        assert_eq!(a, prefs_with_logo());
        assert_eq!(a.sign_images().next().map(|i| Arc::clone(&i.data)).as_deref().map(Vec::as_slice), Some(PNG));
        let s = subscription(&c, url).unwrap().unwrap();
        assert_eq!((s.body.as_slice(), s.etag.as_deref()), (&b"BEGIN:VCALENDAR"[..], Some("\"7\"")));
        assert_eq!(recent_notices(&c, NOW).unwrap().iter().map(|n| n.x.as_str()).collect::<Vec<_>>(), ["Не отправлено"]);
        assert_eq!(crate::smime::seen_of(&c), [der]);
        assert!(c.check_refs().unwrap().is_empty());
        for p in files.moved() {
            assert!(!p.exists(), "{} убран", p.display());
            assert!(bak(&p).exists(), "{} в .bak", p.display());
        }
        assert!(d.path().join(crate::prefs::FILE).exists(), "общие настройки — файлом");

        // Следующий запуск: файл прежней версии снова появился (откат и обратно) — не переносится, `*.bak` убраны.
        labels::LocalFile::beside(&files.cache).save(&Book::own(Categories::defaults(|id| id.to_owned()))).unwrap();
        assert_eq!(adopt(&c, &files, NOW).unwrap(), Adopted::Already);
        assert_eq!(Labels(c).load().unwrap(), Some(book()), "база — главная");
        for p in files.moved() {
            assert!(!bak(&p).exists(), "{} .bak удалён", p.display());
        }
    }

    /// Испорченный файл не мешает остальным и остаётся на месте; записанное в базу не затирается файлом.
    #[test]
    fn adopt_keeps_broken_file_and_store_wins() {
        let d = tmp();
        let (c, files) = store(d.path());
        std::fs::write(files.labels(), "not json").unwrap();
        let mine = prefs_with_logo();
        save_account_prefs(&c, &mine).unwrap();
        AccountPrefs::default().save(&files.account()).unwrap();
        assert_eq!(adopt(&c, &files, NOW).unwrap(), Adopted::Moved(0));
        assert!(files.labels().exists(), "испорченный файл — на месте");
        assert!(!files.account().exists() && bak(&files.account()).exists());
        assert_eq!(account_prefs(&c).unwrap(), mine, "база не затёрта файлом");
    }

    /// Нет прежних файлов — отметка ставится, второй раз ничего не читается.
    #[test]
    fn adopt_with_no_files() {
        let d = tmp();
        let (c, files) = store(d.path());
        assert_eq!(adopt(&c, &files, NOW).unwrap(), Adopted::Moved(0));
        assert!(c.local_imported().unwrap());
        assert_eq!(adopt(&c, &files, NOW).unwrap(), Adopted::Already);
    }
}
