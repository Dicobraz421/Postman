//! Экспорт писем в файлы для других клиентов (S8-31, `docs/specs/S8.md` «Экспорт»): задание в потоке ядра.
//!
//! `Command::Export` → [`Exporter::start`]: охват ([`Scope`]) раскладывается на папки, папка — каталог или файл mbox
//! по раскладке формата (`postman_export::Tree`), письма идут по папкам. Байты письма — из содержимого базы
//! (`Cache::letter_content_reader`); нет — `Command::FetchRaw` у учётки письма (не больше [`FETCHES`] сразу), ответ
//! (`Event::Raw`) — в [`Exporter::event`]. Ошибка сервера или нет ответа за [`FETCH_WAIT`] (нет связи: без связи путь
//! команду не ждёт) — письмо в итог «не загрузилось» по учётке, экспорт идёт дальше; учётка без ответа больше не
//! спрашивается. MSG и PST — после свойств MAPI воркера и их писателей (S8-29, S8-30, S8-33): пока [`Format::available`]
//! — нет, задание кончается отказом.
//!
//! Отмена (`Command::StopExport` → [`Exporter::stop`]): готовые файлы остаются, недописанный mbox удаляется (EML и
//! Maildir пишутся письмом целиком за раз, Maildir — через `tmp`). Ход — `Event::Export`, не чаще
//! [`PROGRESS_EVERY`]; смена состояния — сразу. Файлы пишет только поток ядра: [`Exporter::poll`] — не больше
//! [`LETTERS_PER_POLL`] писем за вызов, чтобы между вызовами доходили «Остановить» и ответы путей.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jiff::tz::TimeZone;
use postman_cache::{Cache, FolderRow, LetterRow, Owner};
use postman_export::{Flags, Layout, Tree, eml, maildir, mbox};

use crate::migrate::Output;
pub use crate::migrate::PROGRESS_EVERY;
use crate::model::{FolderId, LetterId};
use crate::store;
use crate::sync::{Command, Event, Home, Homes};

/// Писем, которые одно задание одновременно загружает с серверов.
pub const FETCHES: usize = 4;
/// Ответа сервера на `FetchRaw` ждём не дольше: нет — связи нет, письма этой учётки — в итог «не загрузилось».
pub const FETCH_WAIT: Duration = Duration::from_secs(120);
/// Писем за один [`Exporter::poll`] на задание.
pub const LETTERS_PER_POLL: usize = 100;
/// Байт за один [`Exporter::poll`] на задание (крупные письма).
const BYTES_PER_POLL: u64 = 32 * 1024 * 1024;
/// Писем из папки базы за одно чтение.
const PAGE: usize = 1000;

/// Что экспортировать.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Выделенные письма: каждое — в каталог (файл mbox) своей папки, без её родителей.
    Letters(Vec<LetterId>),
    /// Папка; `subfolders` — с подпапками (деревом файлов под ней).
    Folder { id: FolderId, subfolders: bool },
    /// Все папки учётки или «На этом компьютере» — деревом от корня.
    Account(Home),
    /// Все учётки и «На этом компьютере»: каталог на каждую с именем из списка (имена учёток и «На этом компьютере»
    /// (`local-group`) знает интерфейс); нет в списке — ключ учётки, `local`.
    All(Vec<(Home, String)>),
}

/// Формат файлов (S8-D, `export.ftl`: `export-eml`, `export-mbox`, `export-maildir`, `export-msg`, `export-pst`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Файл на письмо, каталог на папку.
    Eml,
    /// mbox раскладкой Thunderbird: файл `Папка`, подпапки — в `Папка.sbd/`.
    Mbox,
    /// mbox для Apple Mail: пакет `Папка.mbox/mbox`.
    AppleMbox,
    /// Каталог на папку с `cur`, `new`, `tmp`.
    Maildir,
    /// Файл Outlook на письмо (S8-29, S8-30).
    Msg,
    /// Файл данных Outlook (S8-33).
    Pst,
}

impl Format {
    /// Писатель формата есть: MSG и PST — ещё нет, интерфейс их не предлагает.
    pub fn available(self) -> bool {
        !matches!(self, Format::Msg | Format::Pst)
    }

    fn layout(self) -> Option<Layout> {
        match self {
            Format::Eml => Some(Layout::Dirs),
            Format::Mbox => Some(Layout::Thunderbird),
            Format::AppleMbox => Some(Layout::AppleMail),
            Format::Maildir => Some(Layout::Maildir),
            Format::Msg | Format::Pst => None,
        }
    }
}

/// Номер задания экспорта (не хранится: задание живёт до выхода).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExportId(pub u64);

/// Состояние задания в `Event::Export`. Тексты — у интерфейса (S8-D, `export.ftl`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Идёт: `export-running`, `export-running-detail` (`fetching` — писем ещё загрузить с серверов), в строке
    /// состояния — `export-status` и `export-stop`.
    Running { fetching: u64 },
    /// Готово: всё записано — `export-done`; не всё — `export-done-partial` и `export-failed-fetch` на каждую учётку
    /// `unfetched` (писем не загрузилось с её сервера: ошибка или нет связи); `export-open`.
    Done { unfetched: Vec<(Home, u64)> },
    /// Остановлено пользователем: `export-stopped`, `export-stopped-detail`.
    Stopped,
    /// Запись не удалась (нет места, нет доступа, формат недоступен): `export-stopped` с подробностью — текст ошибки
    /// как есть (по-английски); недописанный mbox удалён; `export-retry`.
    Failed(String),
}

/// Папка выгрузки: путь от корня выгрузки (имена папок) и письма с признаком «содержимое в базе».
struct Part {
    path: Vec<String>,
    letters: Vec<(LetterId, bool)>,
}

/// Папка, которую пишем сейчас.
struct Current {
    path: Vec<String>,
    /// Каталог папки или файл mbox — от корня выгрузки.
    rel: PathBuf,
    /// Содержимое в базе — записать.
    stored: VecDeque<LetterId>,
    /// Содержимого нет — загрузить.
    missing: VecDeque<LetterId>,
    /// `FetchRaw` ушёл: когда и к какой учётке.
    asked: HashMap<LetterId, (Instant, Home)>,
    /// Пришло с сервера — записать.
    arrived: VecDeque<(LetterId, Arc<Vec<u8>>)>,
    mbox: Option<BufWriter<File>>,
}

impl Current {
    fn idle(&self) -> bool {
        self.stored.is_empty() && self.missing.is_empty() && self.asked.is_empty() && self.arrived.is_empty()
    }
}

struct Job {
    format: Format,
    root: PathBuf,
    scope: Option<Scope>,
    tree: Tree,
    parts: VecDeque<Part>,
    current: Option<Current>,
    total: u64,
    done: u64,
    bytes: u64,
    /// Писем без содержимого, ещё не записанных и не списанных в итог.
    fetching: u64,
    unfetched: Vec<(Home, u64)>,
    /// Учётки, которые не ответили за [`FETCH_WAIT`].
    unreachable: Vec<Home>,
    /// Конец: событие — и задание уходит.
    end: Option<State>,
    dirty: bool,
    forced: bool,
    shown: Option<Instant>,
    /// Есть что записать без ответов путей (предел за вызов кончился).
    more: bool,
}

/// Задания экспорта потока ядра.
pub struct Exporter {
    tz: TimeZone,
    maildir: maildir::Maildir,
    jobs: BTreeMap<ExportId, Job>,
    next: u64,
    out: Output,
    polled: Option<Instant>,
}

impl Exporter {
    /// `tz` — часовой пояс имён файлов EML (местное время письма; у интерфейса — системный).
    pub fn new(tz: TimeZone) -> Self {
        // Имя письма Maildir: узел — имя компьютера; `;` вместо `:` — NTFS не пускает двоеточие в имя (как mbsync).
        let host = std::env::var("COMPUTERNAME").unwrap_or_default();
        let maildir = maildir::Maildir::new(std::process::id(), &host, maildir::SEMICOLON);
        Self { tz, maildir, jobs: BTreeMap::new(), next: 0, out: Output::default(), polled: None }
    }

    /// Новое задание (`Command::Export`): разбор охвата и запись — в [`Exporter::poll`].
    pub fn start(&mut self, scope: Scope, format: Format, dir: PathBuf) -> ExportId {
        self.next += 1;
        let id = ExportId(self.next);
        let job = Job {
            format,
            root: dir,
            scope: Some(scope),
            tree: Tree::new(format.layout().unwrap_or(Layout::Dirs)),
            parts: VecDeque::new(),
            current: None,
            total: 0,
            done: 0,
            bytes: 0,
            fetching: 0,
            unfetched: Vec::new(),
            unreachable: Vec::new(),
            end: if format.available() { None } else { Some(State::Failed("format is not available yet".into())) },
            dirty: true,
            forced: true,
            shown: None,
            more: true,
        };
        self.jobs.insert(id, job);
        id
    }

    /// «Остановить» (`Command::StopExport`): готовые файлы остаются, недописанный mbox удаляется.
    pub fn stop(&mut self, id: ExportId) {
        let Some(j) = self.jobs.get_mut(&id).filter(|j| j.end.is_none()) else { return };
        j.discard();
        j.finish(State::Stopped);
    }

    /// Ответ пути: письмо целиком для задания. Прочие события и письма не заданий — мимо.
    pub fn event(&mut self, e: &Event) {
        let Event::Raw { letter, data } = e else { return };
        for j in self.jobs.values_mut() {
            let Some(c) = j.current.as_mut() else { continue };
            let Some((_, home)) = c.asked.remove(letter) else { continue };
            match data {
                Ok(raw) => c.arrived.push_back((*letter, raw.clone())),
                Err(e) => {
                    tracing::warn!(letter = letter.0, "export: letter not fetched: {e}");
                    j.not_fetched(home);
                }
            }
            j.more = true;
        }
    }

    /// Шаги, которые можно сделать сейчас: запись файлов, команды путям (`FetchRaw`), ход интерфейсу.
    pub fn poll(&mut self, cache: &Cache, now: Instant) -> Output {
        self.polled = Some(now);
        let ids: Vec<ExportId> = self.jobs.keys().copied().collect();
        for id in ids {
            let Some(j) = self.jobs.get_mut(&id) else { continue };
            if j.end.is_none()
                && let Err(e) = j.pump(cache, &self.tz, &mut self.maildir, &mut self.out, now)
            {
                tracing::warn!(job = id.0, "export failed: {e}");
                j.discard();
                j.finish(State::Failed(e.to_string()));
            }
            if j.forced || (j.dirty && j.shown.is_none_or(|t| now >= t + PROGRESS_EVERY)) {
                let state = j.end.clone().unwrap_or(State::Running { fetching: j.fetching });
                self.out.events.push(Event::Export { job: id, done: j.done, total: j.total, bytes: j.bytes, state });
                j.shown = Some(now);
                j.dirty = false;
                j.forced = false;
                if j.end.is_some() {
                    self.jobs.remove(&id);
                }
            }
        }
        std::mem::take(&mut self.out)
    }

    /// Когда снова звать [`Exporter::poll`] без новых событий: есть что записать (сразу), срок ответа сервера,
    /// отложенный ход. `None` — до следующего события или команды.
    pub fn wake_at(&self) -> Option<Instant> {
        let mut at: Option<Instant> = None;
        let mut soon = |t: Instant| at = Some(at.map_or(t, |a| a.min(t)));
        for j in self.jobs.values() {
            if j.more || j.forced || j.end.is_some() {
                soon(self.polled.unwrap_or_else(Instant::now));
            }
            if j.dirty
                && let Some(t) = j.shown
            {
                soon(t + PROGRESS_EVERY);
            }
            if let Some(c) = &j.current {
                c.asked.values().for_each(|(t, _)| soon(*t + FETCH_WAIT));
            }
        }
        at
    }

    /// Заданий нет.
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }
}

/// Ошибка задания: базы или диска.
#[derive(Debug, thiserror::Error)]
enum Failure {
    #[error("{0}")]
    Store(#[from] postman_cache::Error),
    #[error("{0}")]
    Io(#[from] io::Error),
}

impl Job {
    fn finish(&mut self, state: State) {
        self.end = Some(state);
        self.forced = true;
        self.current = None;
        self.parts.clear();
    }

    /// Недописанный mbox текущей папки — удалить (EML и Maildir недописанных файлов между вызовами не оставляют).
    fn discard(&mut self) {
        let Some(c) = self.current.take() else { return };
        if let Some(w) = c.mbox {
            drop(w);
            let path = self.root.join(&c.rel);
            if let Err(e) = fs::remove_file(&path) {
                tracing::warn!("export: unfinished mbox not removed: {e}");
            }
        }
    }

    fn not_fetched(&mut self, home: Home) {
        match self.unfetched.iter_mut().find(|(h, _)| *h == home) {
            Some((_, n)) => *n += 1,
            None => self.unfetched.push((home, 1)),
        }
        self.fetching = self.fetching.saturating_sub(1);
        self.dirty = true;
    }

    fn pump(&mut self, cache: &Cache, tz: &TimeZone, md: &mut maildir::Maildir, out: &mut Output, now: Instant) -> Result<(), Failure> {
        if let Some(scope) = self.scope.take() {
            fs::create_dir_all(&self.root)?;
            self.parts = plan(cache, &scope)?.into();
            self.total = self.parts.iter().map(|p| p.letters.len() as u64).sum();
            self.fetching = self.parts.iter().flat_map(|p| &p.letters).filter(|(_, stored)| !stored).count() as u64;
            self.dirty = true;
        }
        self.more = false;
        let (mut letters, mut bytes) = (0usize, 0u64);
        loop {
            if self.current.is_none() {
                let Some(part) = self.parts.pop_front() else {
                    let unfetched = std::mem::take(&mut self.unfetched);
                    self.finish(State::Done { unfetched });
                    return Ok(());
                };
                self.open(part)?;
            }
            self.expire(now);
            self.ask(cache, out, now);
            if letters >= LETTERS_PER_POLL || bytes >= BYTES_PER_POLL {
                // Ещё есть что записать без ответов путей; ждём ответов — не будить зря.
                self.more = self.current.as_ref().is_none_or(|c| !c.stored.is_empty() || !c.arrived.is_empty() || c.idle());
                return Ok(());
            }
            let Some(c) = self.current.as_mut() else { continue };
            if let Some(id) = c.stored.pop_front() {
                match cache.letter_content_reader(store::cache_id(id))? {
                    Some(mut reader) => {
                        if let Some(n) = self.write(cache, tz, md, id, &mut reader)? {
                            bytes += n;
                        }
                        letters += 1;
                    }
                    // Содержимое ушло из базы между чтениями — с сервера.
                    None => {
                        c.missing.push_back(id);
                        self.fetching += 1;
                    }
                }
            } else if let Some((id, raw)) = c.arrived.pop_front() {
                self.fetching = self.fetching.saturating_sub(1);
                if let Some(n) = self.write(cache, tz, md, id, &mut raw.as_slice())? {
                    bytes += n;
                }
                letters += 1;
            } else if c.idle() {
                self.close()?;
            } else {
                // Ждём ответов путей.
                return Ok(());
            }
        }
    }

    /// Начать папку: каталог (с `cur`, `new`, `tmp` у Maildir) или файл mbox.
    fn open(&mut self, part: Part) -> Result<(), Failure> {
        let rel = self.tree.folder(&part.path);
        let full = self.root.join(&rel);
        let mut mbox_file = None;
        match self.format {
            Format::Mbox | Format::AppleMbox => {
                if let Some(parent) = full.parent() {
                    fs::create_dir_all(parent)?;
                }
                // Файл папки прежней выгрузки в тот же каталог — заменяется.
                mbox_file = Some(BufWriter::new(File::create(&full)?));
            }
            Format::Maildir => {
                for d in maildir::DIRS {
                    fs::create_dir_all(full.join(d))?;
                }
            }
            _ => fs::create_dir_all(&full)?,
        }
        let (stored, missing): (Vec<_>, Vec<_>) = part.letters.into_iter().partition(|(_, s)| *s);
        self.current = Some(Current {
            path: part.path,
            rel,
            stored: stored.into_iter().map(|(id, _)| id).collect(),
            missing: missing.into_iter().map(|(id, _)| id).collect(),
            asked: HashMap::new(),
            arrived: VecDeque::new(),
            mbox: mbox_file,
        });
        Ok(())
    }

    /// Папка готова: mbox — дописать на диск.
    fn close(&mut self) -> Result<(), Failure> {
        if let Some(mut c) = self.current.take()
            && let Some(mut w) = c.mbox.take()
            && let Err(e) = w.flush()
        {
            // Вернуть, чтобы `discard` удалил недописанный файл.
            c.mbox = Some(w);
            self.current = Some(c);
            return Err(e.into());
        }
        Ok(())
    }

    /// Нет ответа за [`FETCH_WAIT`] — нет связи с учёткой: её письма — в итог, больше не спрашивать.
    fn expire(&mut self, now: Instant) {
        let Some(c) = self.current.as_mut() else { return };
        let late: Vec<(LetterId, Home)> = c.asked.iter().filter(|(_, (t, _))| now >= *t + FETCH_WAIT).map(|(id, (_, h))| (*id, h.clone())).collect();
        c.asked.retain(|id, _| !late.iter().any(|(l, _)| l == id));
        for (id, home) in late {
            tracing::warn!(letter = id.0, "export: no answer from the server, letter skipped");
            if !self.unreachable.contains(&home) {
                self.unreachable.push(home.clone());
            }
            self.not_fetched(home);
        }
    }

    /// Письма без содержимого — `FetchRaw` у учётки письма, не больше [`FETCHES`] сразу; учётка без связи и «На этом
    /// компьютере» без содержимого — сразу в итог.
    fn ask(&mut self, cache: &Cache, out: &mut Output, now: Instant) {
        let homes = store::CacheHomes(cache);
        loop {
            let Some(c) = self.current.as_mut() else { return };
            if c.asked.len() >= FETCHES {
                return;
            }
            let Some(id) = c.missing.pop_front() else { return };
            let home = homes.letter(id);
            if home == Home::Local || self.unreachable.contains(&home) {
                self.not_fetched(home);
                continue;
            }
            out.commands.push((home.route(), Command::FetchRaw { letter: id }));
            c.asked.insert(id, (now, home));
        }
    }

    /// Письмо в файл формата; письма уже нет в базе — `None` (из итога убирается).
    fn write(&mut self, cache: &Cache, tz: &TimeZone, md: &mut maildir::Maildir, id: LetterId, src: &mut dyn Read) -> Result<Option<u64>, Failure> {
        let Some(row) = cache.letter(store::cache_id(id))? else {
            self.total = self.total.saturating_sub(1);
            self.dirty = true;
            return Ok(None);
        };
        let Some(c) = self.current.as_mut() else { return Ok(None) };
        let offset = jiff::Timestamp::from_second(row.received_at).map_or(0, |t| tz.to_offset(t).seconds());
        let mut letter = postman_export::Letter {
            raw: &[],
            date: row.received_at,
            offset,
            from: &row.from_addr,
            subject: &row.subject,
            flags: flags(&row),
            folder: &c.path,
        };
        let n = match self.format {
            Format::Mbox | Format::AppleMbox => {
                let mut raw = Vec::new();
                src.read_to_end(&mut raw)?;
                letter.raw = &raw;
                let Some(w) = c.mbox.as_mut() else { return Ok(None) };
                mbox::write(w, &letter)?;
                raw.len() as u64
            }
            Format::Maildir => {
                // Имя — по времени и флагам письма; байты — потоком в `tmp`, затем в `cur`.
                let (tmp, cur, mut file) = loop {
                    let place = md.place(&c.rel, &letter);
                    let (tmp, cur) = (self.root.join(place.tmp), self.root.join(place.cur));
                    match OpenOptions::new().write(true).create_new(true).open(&tmp) {
                        Ok(f) => break (tmp, cur, f),
                        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                        Err(e) => return Err(e.into()),
                    }
                };
                let copied = copy(src, &mut file).and_then(|n| fs::rename(&tmp, &cur).map(|()| n));
                copied.inspect_err(|_| {
                    let _ = fs::remove_file(&tmp);
                })?
            }
            _ => {
                // Имя «ГГГГ-ММ-ДД ЧЧММ — Тема.eml»; файл с тем же именем на диске (прежняя выгрузка) — «(2)», не поверх.
                let name = eml::file_name(&letter);
                let (path, mut file) = loop {
                    let path = self.root.join(&c.rel).join(self.tree.unique(&c.rel, &name));
                    match OpenOptions::new().write(true).create_new(true).open(&path) {
                        Ok(f) => break (path, f),
                        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                        Err(e) => return Err(e.into()),
                    }
                };
                copy(src, &mut file).inspect_err(|_| {
                    let _ = fs::remove_file(&path);
                })?
            }
        };
        self.done += 1;
        self.bytes += n;
        self.dirty = true;
        Ok(Some(n))
    }
}

/// Байты письма в файл целиком (сброс на диск — в конце).
fn copy(src: &mut dyn Read, file: &mut File) -> io::Result<u64> {
    let mut w = BufWriter::new(file);
    let n = io::copy(src, &mut w)?;
    w.flush()?;
    Ok(n)
}

fn flags(row: &LetterRow) -> Flags {
    let has = |k: &str| row.keywords.iter().any(|x| x == k);
    Flags { seen: row.seen, answered: has("$answered"), flagged: row.flagged, draft: has("$draft") }
}

fn home_of(owner: &Owner) -> Home {
    match owner {
        Owner::Account(a) => Home::Account(a.clone()),
        Owner::Local => Home::Local,
    }
}

/// Охват → папки выгрузки с путями от корня выгрузки и их письма.
fn plan(cache: &Cache, scope: &Scope) -> Result<Vec<Part>, Failure> {
    let folders: HashMap<postman_cache::FolderId, FolderRow> = cache.folders()?.into_iter().collect();
    let mut order: Vec<postman_cache::FolderId> = folders.keys().copied().collect();
    order.sort();
    let path_of = |id: postman_cache::FolderId| folder_path(&folders, id);
    let mut parts = Vec::new();
    match scope {
        Scope::Letters(ids) => {
            // Папки — по имени, без родителей: письма одноимённых папок сойдутся в одну ([`merge`]).
            for id in ids {
                let Some(row) = cache.letter(store::cache_id(*id))? else { continue };
                let path = row.folders.first().and_then(|f| folders.get(f)).map(|r| vec![r.name.clone()]).unwrap_or_default();
                parts.push(Part { path, letters: vec![(*id, row.content.is_some())] });
            }
        }
        Scope::Folder { id, subfolders } => {
            let top = postman_cache::FolderId(id.0);
            let skip = path_of(top).len().saturating_sub(1);
            let mut ids = vec![top];
            if *subfolders {
                ids.extend(descendants(&folders, &order, top));
            }
            for f in ids {
                if folders.contains_key(&f) {
                    parts.push(Part { path: path_of(f).split_off(skip), letters: letters(cache, f)? });
                }
            }
        }
        Scope::Account(home) => {
            for f in order.iter().copied().filter(|f| folders.get(f).is_some_and(|r| home_of(&r.owner) == *home)) {
                parts.push(Part { path: path_of(f), letters: letters(cache, f)? });
            }
        }
        Scope::All(names) => {
            for f in order.iter().copied() {
                let Some(row) = folders.get(&f) else { continue };
                let home = home_of(&row.owner);
                let owner = names.iter().find(|(h, _)| *h == home).map(|(_, n)| n.clone()).unwrap_or_else(|| match &home {
                    Home::Account(a) if !a.is_empty() => a.clone(),
                    Home::Account(_) => "account".into(),
                    Home::Local => "local".into(),
                });
                let mut path = vec![owner];
                path.extend(path_of(f));
                parts.push(Part { path, letters: letters(cache, f)? });
            }
        }
    }
    Ok(merge(parts))
}

/// Папки с одним путём (одноимённые папки выделения, учётки с одним именем) — одной папкой выгрузки: иначе второй
/// файл mbox заменил бы первый.
fn merge(parts: Vec<Part>) -> Vec<Part> {
    let mut out: Vec<Part> = Vec::with_capacity(parts.len());
    let mut at: HashMap<Vec<String>, usize> = HashMap::new();
    for p in parts {
        match at.get(&p.path).and_then(|&i| out.get_mut(i)) {
            Some(o) => o.letters.extend(p.letters),
            None => {
                at.insert(p.path.clone(), out.len());
                out.push(p);
            }
        }
    }
    out
}

/// Путь папки от корня владельца — имена; цикл в дереве не зацикливает.
fn folder_path(folders: &HashMap<postman_cache::FolderId, FolderRow>, id: postman_cache::FolderId) -> Vec<String> {
    let mut path = Vec::new();
    let mut at = Some(id);
    let mut seen = HashSet::new();
    while let Some(f) = at.filter(|f| seen.insert(*f)) {
        let Some(row) = folders.get(&f) else { break };
        path.push(row.name.clone());
        at = row.parent;
    }
    path.reverse();
    path
}

/// Подпапки `top` на всю глубину, родитель раньше детей.
fn descendants(
    folders: &HashMap<postman_cache::FolderId, FolderRow>,
    order: &[postman_cache::FolderId],
    top: postman_cache::FolderId,
) -> Vec<postman_cache::FolderId> {
    let mut out = Vec::new();
    let mut seen = HashSet::from([top]);
    let mut queue = VecDeque::from([top]);
    while let Some(p) = queue.pop_front() {
        for f in order.iter().copied().filter(|f| folders.get(f).is_some_and(|r| r.parent == Some(p))) {
            if seen.insert(f) {
                out.push(f);
                queue.push_back(f);
            }
        }
    }
    out
}

/// Письма папки с признаком «содержимое в базе».
fn letters(cache: &Cache, folder: postman_cache::FolderId) -> Result<Vec<(LetterId, bool)>, Failure> {
    let mut out = Vec::new();
    loop {
        let page = cache.folder_page(folder, out.len(), PAGE)?;
        let last = page.len() < PAGE;
        out.extend(page.into_iter().map(|(id, row)| (LetterId(id.0), row.content.is_some())));
        if last {
            return Ok(out);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::sync::{Route, Target};

    fn demo() -> (tempfile::TempDir, Cache) {
        let dir = tempfile::Builder::new().prefix("postman-").tempdir().unwrap();
        let path = dir.path().join("demo.redb");
        postman_demo::ensure(&path, 60).unwrap();
        let (cache, _) = store::open(&path, None, &|_| {}).unwrap();
        (dir, cache)
    }

    fn raw(id: LetterId) -> Vec<u8> {
        format!("From: a@x.ru\r\nSubject: s{0}\r\n\r\nFrom here {0}\r\n", id.0).into_bytes()
    }

    fn folder(cache: &Cache, owner: &str, name: &str) -> postman_cache::FolderId {
        let (id, _) = cache.folders().unwrap().into_iter().find(|(_, f)| f.owner == Owner::Account(owner.into()) && f.name == name).unwrap();
        id
    }

    fn ids(cache: &Cache, f: postman_cache::FolderId) -> Vec<LetterId> {
        letters(cache, f).unwrap().into_iter().map(|(id, _)| id).collect()
    }

    /// «Входящие» основной учётки с подпапками: письма 0 и 1 — в «Клиенты», 2 и 3 — в «Клиенты/Северный
    /// филиал/Договоры»; содержимое в базе — у всех, кроме писем с номерами `without`. Ответ — папка и все её письма
    /// до переноса.
    fn inbox(cache: &Cache, without: &[usize]) -> (FolderId, Vec<LetterId>) {
        let inbox = folder(cache, "", "Входящие");
        let all = ids(cache, inbox);
        cache.move_letters(&[store::cache_id(all[0]), store::cache_id(all[1])], folder(cache, "", "Клиенты")).unwrap();
        cache.move_letters(&[store::cache_id(all[2]), store::cache_id(all[3])], folder(cache, "", "Договоры")).unwrap();
        for (i, id) in all.iter().enumerate() {
            if !without.contains(&i) {
                cache.put_content(store::cache_id(*id), &raw(*id), &[]).unwrap();
            }
        }
        (FolderId(inbox.0), all)
    }

    fn seen(cache: &Cache, folders: &[postman_cache::FolderId]) -> usize {
        folders.iter().flat_map(|f| cache.folder_page(*f, 0, PAGE).unwrap()).filter(|(_, r)| r.seen).count()
    }

    /// Задание до конца: `answer` отвечает на `FetchRaw` (`None` — молчит), время идёт по `step`.
    fn run(e: &mut Exporter, cache: &Cache, answer: &dyn Fn(LetterId) -> Option<Event>, step: Duration) -> (Vec<Event>, Vec<(Route, LetterId)>) {
        let mut now = Instant::now();
        let (mut events, mut asked) = (Vec::new(), Vec::new());
        for _ in 0..10_000 {
            let out = e.poll(cache, now);
            events.extend(out.events);
            for (route, cmd) in out.commands {
                let Command::FetchRaw { letter } = cmd else { panic!("только FetchRaw") };
                asked.push((route, letter));
                if let Some(ev) = answer(letter) {
                    e.event(&ev);
                }
            }
            if e.is_empty() {
                return (events, asked);
            }
            now = e.wake_at().map_or(now + step, |t| t.max(now)).max(now + Duration::from_millis(1));
        }
        panic!("задание не кончилось");
    }

    fn last(events: &[Event]) -> (u64, u64, u64, State) {
        match events.last() {
            Some(Event::Export { done, total, bytes, state, .. }) => (*done, *total, *bytes, state.clone()),
            _ => panic!("нет события экспорта"),
        }
    }

    fn files(root: &Path) -> Vec<(String, Vec<u8>)> {
        fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
            for e in fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    walk(root, &p, out);
                } else {
                    let rel = p.strip_prefix(root).unwrap().components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>();
                    out.push((rel.join("/"), fs::read(&p).unwrap()));
                }
            }
        }
        let mut out = Vec::new();
        walk(root, root, &mut out);
        out.sort();
        out
    }

    /// Папка с подпапками — в каждый формат: дерево файлов по раскладке, байты писем как есть (mbox — с `>From`),
    /// флаги Maildir — в имени.
    #[test]
    fn folder_with_subfolders_in_every_format() {
        let (dir, cache) = demo();
        let (inbox, all) = inbox(&cache, &[]);
        let n = all.len() - 4;
        let scope = Scope::Folder { id: inbox, subfolders: true };
        let none = |_: LetterId| -> Option<Event> { panic!("всё в базе") };
        for format in [Format::Eml, Format::Mbox, Format::AppleMbox, Format::Maildir] {
            let root = dir.path().join(format!("{format:?}"));
            let mut e = Exporter::new(TimeZone::UTC);
            let job = e.start(scope.clone(), format, root.clone());
            let (events, asked) = run(&mut e, &cache, &none, Duration::from_millis(10));
            assert!(asked.is_empty());
            assert!(events.iter().all(|e| matches!(e, Event::Export { job: j, .. } if *j == job)));
            let (done, total, bytes, state) = last(&events);
            assert_eq!((done, total, state), (n as u64 + 4, n as u64 + 4, State::Done { unfetched: vec![] }), "{format:?}");
            let got = files(&root);
            let names: Vec<&str> = got.iter().map(|(p, _)| p.as_str()).collect();
            match format {
                Format::Eml => {
                    assert_eq!(got.len(), n + 4);
                    assert_eq!(names.iter().filter(|p| p.starts_with("Входящие/Клиенты/Северный филиал/Договоры/")).count(), 2);
                    assert_eq!(names.iter().filter(|p| p.starts_with("Входящие/Клиенты/") && p.matches('/').count() == 2).count(), 2);
                    assert!(names.iter().all(|p| p.ends_with(".eml")));
                    let sum: u64 = got.iter().map(|(_, b)| b.len() as u64).sum();
                    assert_eq!(sum, bytes);
                    assert!(got.iter().all(|(_, b)| b.starts_with(b"From: a@x.ru\r\n")), "байты как есть");
                }
                Format::Mbox => {
                    // Пустые подпапки — тоже файлы: дерево Thunderbird целиком.
                    assert_eq!(
                        names,
                        [
                            "Входящие",
                            "Входящие.sbd/Клиенты",
                            "Входящие.sbd/Клиенты.sbd/Северный филиал",
                            "Входящие.sbd/Клиенты.sbd/Северный филиал.sbd/Договоры",
                            "Входящие.sbd/Рассылки",
                        ]
                    );
                    assert_eq!(got[3].1.split(|&c| c == b'\n').filter(|l| l.starts_with(b"From ")).count(), 2);
                    assert_eq!(got[3].1.split(|&c| c == b'\n').filter(|l| l.starts_with(b">From here")).count(), 2, "экранирование mboxrd");
                    assert!(got[2].1.is_empty(), "пустая папка — пустой файл");
                }
                Format::AppleMbox => {
                    assert!(names.contains(&"Входящие.mbox/mbox"));
                    assert!(names.contains(&"Входящие.mbox/Клиенты.mbox/Северный филиал.mbox/Договоры.mbox/mbox"));
                    assert_eq!(names.len(), 5);
                }
                _ => {
                    let cur: Vec<&&str> = names.iter().filter(|p| p.contains("/cur/")).collect();
                    assert_eq!((cur.len(), names.len()), (n + 4, n + 4), "в tmp ничего не осталось");
                    assert_eq!(cur.iter().filter(|p| p.starts_with("Входящие/Клиенты/Северный филиал/Договоры/cur/")).count(), 2);
                    for d in ["Входящие/Рассылки/cur", "Входящие/Рассылки/new", "Входящие/Рассылки/tmp"]
                    {
                        assert!(root.join(d).is_dir(), "{d}");
                    }
                    let folders = [postman_cache::FolderId(inbox.0), folder(&cache, "", "Клиенты"), folder(&cache, "", "Договоры")];
                    assert!(cur.iter().all(|p| p.contains(";2,")));
                    assert_eq!(cur.iter().filter(|p| p.ends_with('S')).count(), seen(&cache, &folders), "прочитанные — флаг S");
                }
            }
        }
    }

    /// Письмо без содержимого — с сервера своей учётки; сервер ответил ошибкой — в итог; учётка без ответа (нет связи) —
    /// в итог, больше не спрашивается, экспорт идёт дальше.
    #[test]
    fn letters_without_content_and_without_connection() {
        let (dir, cache) = demo();
        let without: Vec<usize> = (4..14).collect();
        let (inbox, all) = inbox(&cache, &without);
        let missing: Vec<LetterId> = without.iter().map(|i| all[*i]).collect();
        let main = Route::Sync(Target::Profile(None));
        // Первое — с сервера, второе — ошибка сервера, остальные — без ответа: нет связи.
        let answer = |l: LetterId| -> Option<Event> {
            if l == missing[0] {
                Some(Event::Raw { letter: l, data: Ok(Arc::new(raw(l))) })
            } else if l == missing[1] {
                Some(Event::Raw { letter: l, data: Err("server error".into()) })
            } else {
                None
            }
        };
        let root = dir.path().join("out");
        let mut e = Exporter::new(TimeZone::UTC);
        e.start(Scope::Folder { id: inbox, subfolders: false }, Format::Eml, root.clone());
        let (events, asked) = run(&mut e, &cache, &answer, Duration::from_secs(1));
        let n = all.len() as u64 - 4;
        // Не больше FETCHES сразу: на место ответивших — следующие; без ответа за FETCH_WAIT — учётка без связи,
        // оставшиеся не спрашиваются.
        assert_eq!(asked.iter().map(|(_, l)| *l).collect::<Vec<_>>(), missing[..6]);
        assert!(asked.iter().all(|(r, _)| *r == main));
        let (done, total, _, state) = last(&events);
        assert_eq!((done, total), (n - 9, n));
        assert_eq!(state, State::Done { unfetched: vec![(Home::Account(String::new()), 9)] });
        assert_eq!(files(&root).len() as u64, n - 9);
        let fetching: Vec<u64> = events
            .iter()
            .filter_map(|e| match e {
                Event::Export { state: State::Running { fetching }, .. } => Some(*fetching),
                _ => None,
            })
            .collect();
        assert_eq!(fetching.first(), Some(&10));
        assert!(fetching.windows(2).all(|w| w[0] >= w[1]));
    }

    /// Отмена посередине: готовые файлы mbox остаются, недописанный удаляется; поздний ответ сервера — мимо.
    #[test]
    fn cancel_midway_keeps_finished_files() {
        let (dir, cache) = demo();
        // В «Договорах» письмо без содержимого: папка ждёт сервера.
        let (inbox, all) = inbox(&cache, &[2]);
        let root = dir.path().join("out");
        let mut e = Exporter::new(TimeZone::UTC);
        let job = e.start(Scope::Folder { id: inbox, subfolders: true }, Format::Mbox, root.clone());
        let now = Instant::now();
        let mut asked = Vec::new();
        for i in 0..20 {
            let out = e.poll(&cache, now + Duration::from_millis(300 * i));
            asked.extend(out.commands);
        }
        assert_eq!(asked.len(), 1, "ждём письмо «Договоров»");
        let busy = root.join("Входящие.sbd/Клиенты.sbd/Северный филиал.sbd/Договоры");
        assert!(busy.exists() && root.join("Входящие").exists() && root.join("Входящие.sbd/Клиенты").exists());
        e.stop(job);
        e.event(&Event::Raw { letter: all[2], data: Ok(Arc::new(raw(all[2]))) });
        let out = e.poll(&cache, now + Duration::from_secs(10));
        assert!(matches!(&out.events[..], [Event::Export { state: State::Stopped, .. }]));
        assert!(e.is_empty() && e.wake_at().is_none());
        assert!(!busy.exists(), "недописанный mbox удалён");
        let got = files(&root);
        assert!(got.iter().any(|(p, b)| p == "Входящие" && !b.is_empty()), "готовые файлы целы");
        assert_eq!(got.len(), 4, "Входящие, Клиенты, Рассылки, Северный филиал");
    }

    /// Выделение — в каталог своей папки; всё — каталог на учётку с именем от интерфейса; MSG и PST — пока отказ.
    #[test]
    fn letters_all_and_unavailable_formats() {
        let (dir, cache) = demo();
        let inbox = folder(&cache, "", "Входящие");
        let sales = folder(&cache, "sales", "Входящие");
        let picked = [ids(&cache, inbox)[0], ids(&cache, sales)[0]];
        for id in picked {
            cache.put_content(store::cache_id(id), &raw(id), &[]).unwrap();
        }
        let root = dir.path().join("sel");
        let mut e = Exporter::new(TimeZone::UTC);
        e.start(Scope::Letters(picked.to_vec()), Format::Eml, root.clone());
        let (events, _) = run(&mut e, &cache, &|_| None, Duration::from_millis(10));
        assert_eq!(last(&events).3, State::Done { unfetched: vec![] });
        let got = files(&root);
        // Папки с одним именем у разных учёток — один каталог.
        let dirs: HashSet<&str> = got.iter().map(|(p, _)| p.split('/').next().unwrap()).collect();
        assert_eq!((got.len(), dirs), (2, HashSet::from(["Входящие"])), "письмо — в каталог своей папки");

        let root = dir.path().join("all");
        let names = vec![(Home::Account(String::new()), "me@corp.example".to_string()), (Home::Local, "На этом компьютере".to_string())];
        cache.create_folder(None, "Архив 2025").unwrap();
        e.start(Scope::All(names), Format::Maildir, root.clone());
        let (events, _) = run(&mut e, &cache, &|_| None, Duration::from_secs(60));
        let (done, total, _, state) = last(&events);
        let State::Done { unfetched } = state else { panic!("готово") };
        // Без связи со всеми учётками: записаны два письма с содержимым, прочие — в итог по учёткам.
        assert_eq!((done, unfetched.iter().map(|(_, n)| n).sum::<u64>()), (2, total - 2));
        assert!(unfetched.iter().all(|(h, _)| matches!(h, Home::Account(_))));
        for d in [
            "me@corp.example/Входящие/cur",
            "me@corp.example/Проекты/Офис на Лесной/Смета/2026/cur",
            "На этом компьютере/Архив 2025/cur",
            "sales/Входящие/cur",
            "@3/Входящие/Бухгалтерия/cur",
        ] {
            assert!(root.join(d).is_dir(), "{d}");
        }

        for format in [Format::Msg, Format::Pst] {
            assert!(!format.available());
            let job = e.start(Scope::Account(Home::Local), format, dir.path().join("msg"));
            let out = e.poll(&cache, Instant::now());
            assert!(matches!(&out.events[..], [Event::Export { job: j, state: State::Failed(_), .. }] if *j == job));
            assert!(!dir.path().join("msg").exists());
        }
    }
}
