//! Хранилище формата 6 в ядре (ADR 0006, S8-09): открытие с переносом формата 5 (ход — `Event::StoreUpgrade`),
//! письмо целиком — частями через воркер (`MimeSplit`, с ним и `Message-ID`) в содержимое, тело из содержимого без сети, хранилище заданий
//! переноса для [`migrate::Engine`] и мост между `LetterId` и ключами кэша, которые пока держат плагины путей.
//!
//! Ссылка на письмо в строковых полях (id документа поиска, ключ тела из содержимого, ссылка задачи на письмо) —
//! [`letter_ref`], `#<LetterId>`: с `#` ключи кэша не начинаются, ссылка с ключом не путается.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use postman_cache::{BodyRow, Cache, Encoding, Migrated, Migration, MoveTicket, Owner, RemoteRef, SplitPart};
use postman_worker::{Client, mime};

use crate::migrate::{self, Engine, JobRow, LetterInfo, Outbound, Step};
use crate::model::{FolderId, ImportError, JobId, LetterId, MigrationMode, MigrationState, Remote, Result};
use crate::sync::{Event, Home, Homes};

const REF: char = '#';

/// Ссылка на письмо в строковом поле.
pub fn letter_ref(id: LetterId) -> String {
    format!("{REF}{}", id.0)
}

/// Письмо ссылки [`letter_ref`]; ключ кэша — `None`.
pub fn parse_ref(s: &str) -> Option<LetterId> {
    s.strip_prefix(REF)?.parse().ok().map(LetterId)
}

pub(crate) fn cache_id(id: LetterId) -> postman_cache::LetterId {
    postman_cache::LetterId(id.0)
}

fn cache_folder(id: FolderId) -> postman_cache::FolderId {
    postman_cache::FolderId(id.0)
}

fn cache_job(id: JobId) -> postman_cache::JobId {
    postman_cache::JobId(id.0)
}

/// Ключ кэша письма учётки (основная — пустая строка).
fn key(account: &str, id: &str) -> String {
    postman_cache::scoped(Some(account).filter(|a| !a.is_empty()), id)
}

/// Ключ кэша письма — по действующей связи с сервером (команды путей, тела от плагинов); без неё — `None`.
pub fn key_of(cache: &Cache, id: LetterId) -> postman_cache::Result<Option<String>> {
    Ok(cache.remotes(cache_id(id))?.into_iter().find(|r| !r.detached).map(|r| key(&r.account, &r.id)))
}

/// Письмо по ключу кэша (действующая связь) или по ссылке [`letter_ref`]; нет такого — `None`.
pub fn letter_of_key(cache: &Cache, key: &str) -> postman_cache::Result<Option<LetterId>> {
    if let Some(id) = parse_ref(key) {
        return Ok(cache.letter(cache_id(id))?.map(|_| id));
    }
    let (account, id) = postman_cache::unscoped(key);
    Ok(cache.letter_of(account.unwrap_or(""), id)?.map(|l| LetterId(l.0)))
}

// --- содержимое ---

/// Ответ воркера `MimeSplit` (диапазоны частей и их раскодированные байты подряд) → части для
/// [`Cache::put_content`]. Несогласованный ответ — ошибка.
pub fn parts(split: mime::Split, decoded: &[u8]) -> std::result::Result<Vec<SplitPart>, String> {
    let bad = || "split does not match the decoded bytes".to_owned();
    let mut at = 0usize;
    let mut out = Vec::with_capacity(split.parts.len());
    for p in split.parts {
        let len = usize::try_from(p.decoded_len).map_err(|_| bad())?;
        let end = at.checked_add(len).ok_or_else(bad)?;
        let bytes = decoded.get(at..end).ok_or_else(bad)?;
        let range = usize::try_from(p.range.start).map_err(|_| bad())?..usize::try_from(p.range.end).map_err(|_| bad())?;
        let encoding = match p.encoding {
            mime::Encoding::Base64 => Encoding::Base64,
            mime::Encoding::QuotedPrintable => Encoding::QuotedPrintable,
        };
        out.push(SplitPart { range, encoding, decoded: bytes.to_vec() });
        at = end;
    }
    if at != decoded.len() {
        return Err(bad());
    }
    Ok(out)
}

/// Письмо целиком по ответу воркера `MimeSplit`: части для [`Cache::put_content`] и `Message-ID` (с угловыми
/// скобками) — по нему путь цели ищет письмо при повторе выгрузки (`Command::Import`). Заголовки чужого письма в
/// процессе приложения не разбираются (решение владельца 2026-10-30).
#[derive(Debug, Default)]
pub struct Parts {
    pub parts: Vec<SplitPart>,
    pub message_id: Option<String>,
}

/// Ответ воркера `MimeSplit` целиком → [`Parts`]. Несогласованный ответ — ошибка.
pub fn split_of(split: mime::Split, decoded: &[u8]) -> std::result::Result<Parts, String> {
    let message_id = split.message_id.clone();
    Ok(Parts { parts: parts(split, decoded)?, message_id })
}

/// Письмо целиком → [`Parts`]; ошибка — текст для журнала.
pub type Splitter = Box<dyn FnMut(&[u8]) -> std::result::Result<Parts, String> + Send>;

/// Части письма и `Message-ID` — воркером в песочнице (`MimeSplit`): разбор чужого письма — только там.
pub fn worker_splitter(mut client: Client) -> Splitter {
    Box::new(move |raw| {
        let (split, decoded) = client.mime_split(raw)?;
        split_of(split, &decoded)
    })
}

/// Письмо целиком `raw` — содержимым письма `id`; ответ — его `Message-ID` от воркера. Не разобралось (не RFC 5322,
/// воркер не ответил) — хранится одним скелетом: байты те же, только без дедупликации частей; `Message-ID` — `None`.
pub fn put_raw(cache: &Cache, split: &mut Splitter, id: LetterId, raw: &[u8]) -> postman_cache::Result<Option<String>> {
    let p = split(raw).unwrap_or_else(|_| {
        tracing::warn!(letter = id.0, "content split failed, letter stored whole");
        Parts::default()
    });
    cache.put_content(cache_id(id), raw, &p.parts)?;
    Ok(p.message_id)
}

/// Открыть хранилище; файл формата 5 — перенести в формат 6 (ADR 0006 п. 6): ход — `Event::StoreUpgrade` в `notify`,
/// письма целиком — на части `split` (`None` — целиком). Перенос долгий — звать не из потока интерфейса: первый кадр
/// его не ждёт. `Migrated::Fresh` — база пустая: синхронизация с нуля, как прежде при смене формата. После переноса
/// и с пустой базой индекс поиска по `LetterId` не готов: поиск — по кэшу до [`crate::indexer::rebuild`].
pub fn open(path: &Path, split: Option<Splitter>, notify: &dyn Fn(Event)) -> postman_cache::Result<(Cache, Migrated)> {
    let mut split = split;
    let mut how = Migration {
        progress: Box::new(|done, total| notify(Event::StoreUpgrade { done, total })),
        // Письмо, которое воркер не разобрал, хранится целиком: перенос из-за него не прерывается.
        split: split.as_mut().map(|s| -> postman_cache::Split<'_> { Box::new(move |raw: &[u8]| Ok(s(raw).map(|p| p.parts).unwrap_or_default())) }),
    };
    let (cache, migrated) = Cache::open_with(path, &mut how)?;
    match &migrated {
        Migrated::Current => {}
        Migrated::Done => tracing::info!("store migrated from format 5"),
        Migrated::Fresh(reason) => tracing::warn!("store starts empty: {reason}"),
    }
    Ok((cache, migrated))
}

// --- тело ---

/// Откуда показать письмо.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    /// Разобранное тело уже есть.
    Body(Box<BodyRow>),
    /// Тела нет, письмо целиком есть: тело собирает воркер, без сети.
    Raw(Arc<Vec<u8>>),
    /// Ни того, ни другого: тело — у сервера учётки письма.
    Missing,
}

/// Тело письма: от плагина (по ключу действующей связи) или собранное из содержимого (по [`letter_ref`]).
pub fn body(cache: &Cache, id: LetterId) -> postman_cache::Result<Option<BodyRow>> {
    if let Some(k) = key_of(cache, id)?
        && let Some(b) = cache.body(&k)?
    {
        return Ok(Some(b));
    }
    cache.body(&letter_ref(id))
}

/// Как показать письмо: тело, письмо целиком для воркера или ничего — тогда нужен сервер. Тело из содержимого,
/// собранное воркером (`render::Rendered::Parsed`), кладут под ключом [`letter_ref`].
pub fn source(cache: &Cache, id: LetterId) -> postman_cache::Result<Source> {
    if let Some(b) = body(cache, id)? {
        return Ok(Source::Body(Box::new(b)));
    }
    Ok(match cache.letter_content(cache_id(id))? {
        Some(raw) => Source::Raw(Arc::new(raw)),
        None => Source::Missing,
    })
}

/// Вложение тела из содержимого (`#<письмо>#<номер>`, [`crate::render::body_of`]) — из базы, байты части — воркером;
/// не такое вложение — `None` (его читает путь учётки).
pub fn attachment(cache: &Cache, worker: &mut Client, blob_id: &str) -> Option<std::result::Result<Vec<u8>, String>> {
    let (letter, index) = blob_id.rsplit_once('#')?;
    let (letter, index) = (parse_ref(letter)?, index.parse::<u32>().ok()?);
    Some(match cache.letter_content(cache_id(letter)) {
        Ok(Some(raw)) => worker.mime_part(&raw, index),
        Ok(None) => Err("letter content is not stored".into()),
        Err(e) => Err(e.to_string()),
    })
}

// --- маршрут и перенос ---

/// Чьи письма и папки — по хранилищу (маршрут `Command::routes_by`): письмо — по действующей связи, без неё — «На
/// этом компьютере»; папка — по владельцу. Не прочиталось — «На этом компьютере» (без сети), причина — в журнал.
pub struct CacheHomes<'a>(pub &'a Cache);

impl Homes for CacheHomes<'_> {
    fn letter(&self, id: LetterId) -> Home {
        letter_home(self.0, id)
    }

    fn folder(&self, id: FolderId) -> Home {
        folder_home(self.0, id)
    }
}

fn letter_home(cache: &Cache, id: LetterId) -> Home {
    match cache.remotes(cache_id(id)) {
        Ok(refs) => refs.into_iter().find(|r| !r.detached).map_or(Home::Local, |r| Home::Account(r.account)),
        Err(e) => {
            tracing::warn!(letter = id.0, "store: letter owner not read: {e}");
            Home::Local
        }
    }
}

fn folder_home(cache: &Cache, id: FolderId) -> Home {
    match cache.folder(cache_folder(id)) {
        Ok(Some(f)) => match f.owner {
            Owner::Account(a) => Home::Account(a),
            Owner::Local => Home::Local,
        },
        Ok(None) => Home::Local,
        Err(e) => {
            tracing::warn!(folder = id.0, "store: folder owner not read: {e}");
            Home::Local
        }
    }
}

/// Хранилище заданий переноса для [`migrate::Engine`] поверх кэша формата 6: каждая запись — одна транзакция кэша;
/// письмо целиком от источника — частями через `split`, его `Message-ID` — оттуда же.
pub struct CacheStore {
    cache: Cache,
    split: Splitter,
    /// `Message-ID` писем заданий от `split`: в базе его нет, у содержимого из базы — спросить воркер снова.
    message_ids: HashMap<LetterId, Option<String>>,
}

impl CacheStore {
    pub fn new(cache: Cache, split: Splitter) -> Self {
        Self { cache, split, message_ids: HashMap::new() }
    }

    pub fn cache(&self) -> &Cache {
        &self.cache
    }
}

/// `Command::MoveLetters` в потоке ядра: перенос в базе одной транзакцией на пачку, задание выгрузки — движку.
pub fn move_letters(engine: &mut Engine<CacheStore>, ids: &[LetterId], to: FolderId) -> Result<MoveTicket> {
    let ids: Vec<postman_cache::LetterId> = ids.iter().map(|id| cache_id(*id)).collect();
    let ticket = engine.store().cache.move_letters(&ids, cache_folder(to))?;
    engine.moved(ticket.job.map(|j| JobId(j.0)))?;
    Ok(ticket)
}

/// `Command::FetchRaw` письма «На этом компьютере» (маршрут — ядро): письмо целиком из базы, без сети.
pub fn local_raw(cache: &Cache, letter: LetterId) -> Event {
    let data = match cache.letter_content(cache_id(letter)) {
        Ok(Some(raw)) => Ok(Arc::new(raw)),
        Ok(None) => Err("letter content is not stored".into()),
        Err(e) => Err(e.to_string()),
    };
    Event::Raw { letter, data }
}

impl Homes for CacheStore {
    fn letter(&self, id: LetterId) -> Home {
        letter_home(&self.cache, id)
    }

    fn folder(&self, id: FolderId) -> Home {
        folder_home(&self.cache, id)
    }
}

fn job_from(r: postman_cache::JobRow) -> JobRow {
    let mode = match r.mode {
        postman_cache::MigrationMode::Ask => MigrationMode::Ask,
        postman_cache::MigrationMode::Upload => MigrationMode::Upload,
        postman_cache::MigrationMode::LocalArchive => MigrationMode::LocalArchive,
    };
    let state = match r.state {
        postman_cache::MigrationState::Asking => MigrationState::Asking,
        postman_cache::MigrationState::Running => MigrationState::Running,
        postman_cache::MigrationState::Paused => MigrationState::Paused,
        postman_cache::MigrationState::QuotaFull => MigrationState::QuotaFull,
        postman_cache::MigrationState::Done => MigrationState::Done,
        postman_cache::MigrationState::Cancelled => MigrationState::Cancelled,
    };
    JobRow { id: JobId(r.id.0), mode, state, total: r.total, bytes: r.bytes }
}

fn job_to(r: &JobRow) -> postman_cache::JobRow {
    let mode = match r.mode {
        MigrationMode::Ask => postman_cache::MigrationMode::Ask,
        MigrationMode::Upload => postman_cache::MigrationMode::Upload,
        MigrationMode::LocalArchive => postman_cache::MigrationMode::LocalArchive,
    };
    let state = match r.state {
        MigrationState::Asking => postman_cache::MigrationState::Asking,
        MigrationState::Running => postman_cache::MigrationState::Running,
        MigrationState::Paused => postman_cache::MigrationState::Paused,
        MigrationState::QuotaFull => postman_cache::MigrationState::QuotaFull,
        MigrationState::Done => postman_cache::MigrationState::Done,
        MigrationState::Cancelled => postman_cache::MigrationState::Cancelled,
    };
    postman_cache::JobRow { id: cache_job(r.id), mode, state, total: r.total, bytes: r.bytes }
}

fn step_from(s: postman_cache::Step) -> Step {
    match s {
        postman_cache::Step::Content => Step::Content,
        postman_cache::Step::Upload => Step::Upload,
        postman_cache::Step::Confirm => Step::Confirm,
        postman_cache::Step::DeleteSource => Step::DeleteSource,
    }
}

fn step_to(s: Step) -> postman_cache::Step {
    match s {
        Step::Content => postman_cache::Step::Content,
        Step::Upload => postman_cache::Step::Upload,
        Step::Confirm => postman_cache::Step::Confirm,
        Step::DeleteSource => postman_cache::Step::DeleteSource,
    }
}

impl migrate::Store for CacheStore {
    fn jobs(&self) -> Result<Vec<JobRow>> {
        Ok(self.cache.jobs()?.into_iter().map(job_from).collect())
    }

    fn save_job(&mut self, job: &JobRow) -> Result<()> {
        Ok(self.cache.save_job(&job_to(job))?)
    }

    fn drop_job(&mut self, job: JobId) -> Result<()> {
        Ok(self.cache.drop_job(cache_job(job))?)
    }

    fn outbound(&self, job: JobId) -> Result<Vec<(LetterId, Outbound)>> {
        let rows = self.cache.outbound(cache_job(job))?;
        Ok(rows.into_iter().map(|(id, ob)| (LetterId(id.0), Outbound { to: FolderId(ob.to.0), step: step_from(ob.step) })).collect())
    }

    fn set_step(&mut self, job: JobId, letter: LetterId, step: Step) -> Result<()> {
        Ok(self.cache.set_step(cache_job(job), cache_id(letter), step_to(step))?)
    }

    fn info(&self, id: LetterId) -> Result<Option<LetterInfo>> {
        let Some(row) = self.cache.letter(cache_id(id))? else { return Ok(None) };
        // Локальные метки (замок S/MIME) на сервер цели не уходят.
        let mut keywords: Vec<String> = row.keywords.into_iter().filter(|k| k != postman_cache::ENCRYPTED).collect();
        keywords.extend(row.seen.then(|| postman_cache::SEEN.to_owned()));
        keywords.extend(row.flagged.then(|| postman_cache::FLAGGED.to_owned()));
        let sources = self
            .cache
            .remotes(cache_id(id))?
            .into_iter()
            .filter(|r| r.detached)
            .map(|r| Remote { account: r.account, id: r.id, data: r.data })
            .collect();
        Ok(Some(LetterInfo { content: row.content.is_some(), size: row.size, keywords, received_at: row.received_at, sources }))
    }

    fn put_content(&mut self, letter: LetterId, raw: &[u8]) -> Result<()> {
        let message_id = put_raw(&self.cache, &mut self.split, letter, raw)?;
        self.message_ids.insert(letter, message_id);
        Ok(())
    }

    fn message_id(&mut self, letter: LetterId) -> Result<Option<String>> {
        if let Some(m) = self.message_ids.get(&letter) {
            return Ok(m.clone());
        }
        let Some(raw) = self.cache.letter_content(cache_id(letter))? else { return Ok(None) };
        match (self.split)(&raw) {
            Ok(p) => {
                self.message_ids.insert(letter, p.message_id.clone());
                Ok(p.message_id)
            }
            // Воркер не ответил: без `Message-ID` сейчас, в следующий раз — спросить снова.
            Err(e) => {
                tracing::warn!(letter = letter.0, "message id not read: {e}");
                Ok(None)
            }
        }
    }

    fn confirm(&mut self, job: JobId, letter: LetterId, remote: &Remote) -> Result<()> {
        let link = RemoteRef { account: remote.account.clone(), id: remote.id.clone(), data: remote.data.clone(), detached: false };
        Ok(self.cache.confirm(cache_job(job), cache_id(letter), link)?)
    }

    fn archive(&mut self, job: JobId, letter: LetterId) -> Result<()> {
        Ok(self.cache.archive(cache_job(job), cache_id(letter))?)
    }

    fn refused(&mut self, job: JobId, letter: LetterId, why: &ImportError) -> Result<()> {
        let why = match why {
            ImportError::TooLarge => postman_cache::Refused::TooLarge,
            _ => postman_cache::Refused::Rejected,
        };
        self.message_ids.remove(&letter);
        Ok(self.cache.refused(cache_job(job), cache_id(letter), why)?)
    }

    fn finish(&mut self, job: JobId, letter: LetterId) -> Result<()> {
        self.message_ids.remove(&letter);
        Ok(self.cache.finish(cache_job(job), cache_id(letter))?)
    }

    fn kept(&mut self, job: JobId, letter: LetterId) -> Result<()> {
        self.message_ids.remove(&letter);
        Ok(self.cache.kept(cache_job(job), cache_id(letter))?)
    }

    fn restore(&mut self, job: JobId, letters: &[LetterId]) -> Result<()> {
        for l in letters {
            self.message_ids.remove(l);
        }
        let ids: Vec<postman_cache::LetterId> = letters.iter().map(|id| cache_id(*id)).collect();
        Ok(self.cache.restore(cache_job(job), &ids)?)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::time::Instant;

    use postman_cache::Place;

    use super::*;
    use crate::sync::{Command, Route, Target};

    /// Части — тем же кодом, что в воркере (`MimeSplit`), без процесса.
    pub(crate) fn splitter() -> Splitter {
        Box::new(|raw| {
            let (split, decoded) = mime::split(raw)?;
            split_of(split, &decoded)
        })
    }

    fn b64(data: &[u8]) -> String {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::new();
        for (i, c) in data.chunks(3).enumerate() {
            if i > 0 && i % 19 == 0 {
                s.push_str("\r\n");
            }
            let n = c.iter().enumerate().fold(0u32, |n, (j, b)| n | (*b as u32) << (16 - 8 * j));
            for j in 0..4 {
                s.push(if j <= c.len() { T[(n >> (18 - 6 * j) & 63) as usize] as char } else { '=' });
            }
        }
        s
    }

    /// Письмо с текстом и вложением 100 КБ (крупная часть — отдельно в хранилище).
    pub(crate) fn raw(n: u32) -> Vec<u8> {
        let file: Vec<u8> = (0..100_000u32).map(|i| (i.wrapping_mul(2_654_435_761).wrapping_add(n) >> 13) as u8).collect();
        format!(
            "From: a@x.ru\r\nTo: i@x.ru\r\nMessage-ID: <m{n}@x.ru>\r\nSubject: s{n}\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\n\
Content-Type: text/plain; charset=utf-8\r\n\r\nотчёт {n}\r\n--b\r\nContent-Type: application/pdf\r\nContent-Transfer-Encoding: base64\r\n\
Content-Disposition: attachment; filename=r.pdf\r\n\r\n{}\r\n--b--\r\n",
            b64(&file)
        )
        .into_bytes()
    }

    pub(crate) fn demo() -> (tempfile::TempDir, Cache) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo.redb");
        postman_demo::ensure(&path, 60).unwrap();
        let (cache, migrated) = open(&path, Some(splitter()), &|_| {}).unwrap();
        assert_eq!(migrated, Migrated::Current);
        (dir, cache)
    }

    pub(crate) fn id(cache: &Cache, key: &str) -> LetterId {
        letter_of_key(cache, key).unwrap().unwrap()
    }

    /// `Message-ID` для выгрузки — из ответа воркера: при записи содержимого, у содержимого из базы — воркер снова
    /// (один раз); воркер не ответил — `None` и спросить снова.
    #[test]
    fn message_id_comes_from_the_worker() {
        use crate::migrate::Store as _;
        let (_dir, cache) = demo();
        let (a, b, c) = (id(&cache, "demo3"), id(&cache, "demo4"), id(&cache, "demo5"));
        assert_eq!(put_raw(&cache, &mut splitter(), a, &raw(3)).unwrap().as_deref(), Some("<m3@x.ru>"));
        put_raw(&cache, &mut splitter(), b, &raw(4)).unwrap();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let n = calls.clone();
        let mut inner = splitter();
        let counted: Splitter = Box::new(move |raw| {
            n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            inner(raw)
        });
        let mut store = CacheStore::new(cache.clone(), counted);
        store.put_content(c, &raw(5)).unwrap();
        assert_eq!(store.message_id(c).unwrap().as_deref(), Some("<m5@x.ru>"));
        assert_eq!(store.message_id(b).unwrap().as_deref(), Some("<m4@x.ru>"));
        assert_eq!(store.message_id(b).unwrap().as_deref(), Some("<m4@x.ru>"));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2, "воркер — при записи и раз у содержимого из базы");
        assert_eq!(store.message_id(id(&cache, "demo6")).unwrap(), None, "содержимого нет");
        let mut down = CacheStore::new(cache.clone(), Box::new(|_| Err("worker is down".into())));
        assert_eq!(down.message_id(a).unwrap(), None);
        assert!(!down.message_ids.contains_key(&a), "не ответил — спросить снова");
    }

    #[test]
    fn refs_and_split() {
        assert_eq!(parse_ref(&letter_ref(LetterId(42))), Some(LetterId(42)));
        assert_eq!((parse_ref("42"), parse_ref("#x"), parse_ref("@2/e")), (None, None, None));
        let raw = raw(1);
        let (split, decoded) = mime::split(&raw).unwrap();
        let p = parts(split.clone(), &decoded).unwrap();
        assert_eq!((p.len(), p[0].decoded.len(), p[0].encoding), (1, 100_000, Encoding::Base64));
        assert!(parts(split, &decoded[1..]).is_err(), "ответ воркера не сходится — ошибка, не паника");
    }

    /// Демо-ящик без сети (S8-09): письмо целиком — частями в содержимое; тела нет — показать из содержимого (воркер
    /// собирает тело), собранное — под ссылкой; вложение тела — из базы.
    #[test]
    fn letter_opens_from_content_without_network() {
        let (_dir, cache) = demo();
        let letter = id(&cache, "demo3");
        assert_eq!(key_of(&cache, letter).unwrap().as_deref(), Some("demo3"));
        assert_eq!(source(&cache, letter).unwrap(), Source::Missing, "без тела и содержимого — к серверу");
        let raw = raw(3);
        put_raw(&cache, &mut splitter(), letter, &raw).unwrap();
        let Source::Raw(got) = source(&cache, letter).unwrap() else { panic!("из содержимого") };
        assert_eq!(*got, raw, "ровно исходные байты");
        let r = letter_ref(letter);
        let body = crate::render::body_of(mime::parse(&got).unwrap(), &r);
        assert_eq!(body.text.as_deref().map(str::trim), Some("отчёт 3"));
        assert_eq!((body.message_id.as_deref(), body.attachments[0].blob_id.clone()), (Some("m3@x.ru"), format!("{r}#0")));
        cache.put_body(&r, &body).unwrap();
        assert_eq!(source(&cache, letter).unwrap(), Source::Body(Box::new(body)));
        // Вложение по ключу кэша — не из содержимого: его читает путь учётки.
        assert!(attachment(&cache, &mut Client::new("none".into(), std::time::Duration::from_secs(1)), "demo3#0").is_none());
        // Неразборчивое — целиком, байты те же.
        let other = id(&cache, "demo4");
        put_raw(&cache, &mut splitter(), other, b"not a letter").unwrap();
        assert_eq!(cache.letter_content(cache_id(other)).unwrap().as_deref(), Some(&b"not a letter"[..]));
        assert!(cache.check_refs().unwrap().is_empty());
    }

    /// Перенос в «На этом компьютере» движком поверх хранилища: содержимое есть — архив, нет — у источника; удаление
    /// у источника — по `detached`-связи, письмо кончается по ответу; потом письмо только в базе, команды над ним — в ядро, письмо целиком — из базы.
    #[test]
    fn local_archive_through_the_engine() {
        let (_dir, cache) = demo();
        let local = cache.create_folder(None, "Архив 2025").unwrap();
        let (a, b) = (id(&cache, "demo5"), id(&cache, "demo6"));
        put_raw(&cache, &mut splitter(), a, &raw(5)).unwrap();
        let homes = CacheHomes(&cache);
        let route = |c: Command| c.routes_by(&CacheHomes(&cache)).into_iter().map(|(r, _)| r).collect::<Vec<_>>();
        assert_eq!(route(Command::FetchRaw { letter: b }), [Route::Sync(Target::Profile(None))]);
        assert_eq!(homes.folder(FolderId(local.0)), Home::Local);
        let mut engine = Engine::open(CacheStore::new(cache.clone(), splitter())).unwrap();
        let ticket = move_letters(&mut engine, &[a, b], FolderId(local.0)).unwrap();
        assert_eq!((ticket.moved, ticket.outbound), (2, 2));
        let now = Instant::now();
        let (mut fetch, mut destroy) = (Vec::new(), Vec::new());
        for _ in 0..5 {
            for (r, c) in engine.poll(now).unwrap().commands {
                match c {
                    Command::FetchRaw { letter } => fetch.push((r, letter)),
                    Command::DestroyLetters { job, account, letters } => {
                        // Путь источника удаляет по `detached`-связи: она ещё есть, пока сервер не ответил.
                        for l in &letters {
                            let refs = cache.remotes(cache_id(*l)).unwrap();
                            assert!(refs.iter().any(|x| x.detached && x.account == account));
                            destroy.push((r.clone(), refs[0].id.clone()));
                            engine.event(&Event::Destroyed { job, letter: *l }, now).unwrap();
                        }
                    }
                    _ => panic!("неожиданная команда"),
                }
            }
            if let Some((_, letter)) = fetch.pop() {
                assert_eq!(letter, b);
                engine.event(&Event::Raw { letter, data: Ok(Arc::new(raw(6))) }, now).unwrap();
            }
        }
        destroy.sort_by(|x, y| x.1.cmp(&y.1));
        let main = Route::Sync(Target::Profile(None));
        assert_eq!(destroy, [(main.clone(), "demo5".to_string()), (main, "demo6".to_string())]);
        for (l, n) in [(a, 5), (b, 6)] {
            let row = cache.letter(cache_id(l)).unwrap().unwrap();
            assert_eq!((row.place, row.folders), (Place::Local, vec![local]));
            assert!(cache.remotes(cache_id(l)).unwrap().is_empty());
            assert_eq!(route(Command::FetchRaw { letter: l }), [Route::Core], "письмо только в базе — без сети");
            let Event::Raw { data: Ok(got), .. } = local_raw(&cache, l) else { panic!("из базы") };
            assert_eq!(*got, raw(n));
        }
        assert_eq!(letter_of_key(&cache, "demo5").unwrap(), None, "прежний ключ — у источника, письма там нет");
        assert!(cache.jobs().unwrap().is_empty() && cache.check_index().unwrap().is_empty() && cache.check_refs().unwrap().is_empty());
    }

    /// Открытие: файл не хранилище — пустая база (синхронизация с нуля), хода переноса нет.
    #[test]
    fn open_foreign_file_starts_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.redb");
        std::fs::write(&path, b"SQLite format 3\0").unwrap();
        let events = std::cell::RefCell::new(Vec::new());
        let (cache, migrated) = open(&path, None, &|e| events.borrow_mut().push(e)).unwrap();
        assert!(matches!(migrated, Migrated::Fresh(_)));
        assert!(events.borrow().is_empty() && cache.folders().unwrap().is_empty());
        drop(cache);
        assert_eq!(open(&path, None, &|_| {}).unwrap().1, Migrated::Current);
    }
}
