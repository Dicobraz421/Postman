//! Действие над всей выборкой на сервере («выбрано всё»): письма определяет сервер, а не то, что уже загружено в кэш.
//!
//! Выбрано всё в папке — это условие ([`Selector`]), а не список id. Действие идёт в потоке ядра двумя шагами:
//! сначала снимок: ключи писем постранично с сервера (`Command::ListLetters`) и в базу, затем — страница за страницей:
//! строки недостающих писем в кэш (`Command::StoreLetters`), действие над страницей. Снимок нужен потому, что само
//! действие меняет выдачу сервера: письма уходят из папки, и страницы по позиции поехали бы. Письмо, пришедшее после
//! снимка, в выборку не попадает: пользователь его не выбирал.
//!
//! Перенос между учётками идёт в обычное задание `migrate::Engine`: страницы приносят письма в одно задание, отмена,
//! пауза и «нет места» — его. Письма за пределами пришедших страниц папку не покидают, поэтому отмена их не трогает.
//!
//! Запись — в `sync_state` кэша (без новых таблиц и формата): строки выборок одной записью, страницы снимка — отдельно.
//! Каждый шаг записан до следующего: после перезапуска выборка идёт с той же страницы, повтор страницы ничего не портит
//! (письмо уже в цели пропускается). Снимок, не дописанный до выхода, снимается заново. Экспорт живёт до выхода, как и
//! задания экспорта.
//!
//! Команды уходят только учётке, которая в этом запуске вошла на сервер ([`Sweeper::online`]). Отказ сервера или сети —
//! повтор страницы после паузы.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use postman_cache::Cache;
use serde::{Deserialize, Serialize};

use crate::bulk::{self, Act};
use crate::export::{self, Format};
use crate::lifecycle;
use crate::marks::{Kind, When};
use crate::migrate::{Engine, Output, PROGRESS_EVERY};
use crate::model::{FolderId, JobId, LetterId, MigrationRequest, MigrationState, Result, Selector, SweepId};
use crate::store::{CacheStore, move_into};
use crate::sync::{Command, Event, FOLDER_UNSUPPORTED, Route};

/// Страница снимка и действия, писем.
pub const PAGE: u32 = 500;
/// Пауза перед повтором страницы: сервер отказал или связи нет.
pub const RETRY: Duration = Duration::from_secs(30);

const INDEX: &str = "sweeps";

fn page_key(id: SweepId, n: u32) -> String {
    format!("sweep.{}.{n}", id.0)
}

/// Что сделать с выборкой.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    /// В другую учётку или «На этом компьютере»: задание переноса.
    Transfer { to: FolderId },
    /// Правка на сервере учётки, как у массовых действий; `archive_name` — имя «Архива», если его ещё нет.
    Patch { act: Act, archive_name: String },
    /// Срок, напоминание, «ждём» (`lifecycle::mark`): `when` — [`when_text`].
    Mark { kind: Kind, when: Option<String>, archive_name: String },
    /// Удалить насовсем (подтверждено строкой «Удалить навсегда?»).
    Purge,
    /// Экспорт выборки в каталог: страницы только приносят строки писем, файлы пишет `export::Exporter`.
    Export { format: Format, dir: PathBuf },
}

/// Срок строкой для записи выборки: дата и время через `/`.
pub fn when_text(w: When) -> String {
    w.time.map_or_else(|| w.date.to_string(), |t| format!("{}/{t}", w.date))
}

fn parse_when(s: &str) -> Option<When> {
    let (date, time) = s.split_once('/').map_or((s, None), |(d, t)| (d, Some(t)));
    Some(When { date: date.parse().ok()?, time: time.map(str::parse).transpose().ok()? })
}

/// Шаг выборки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Phase {
    Listing,
    Applying,
}

/// Выборка в базе.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SweepRow {
    id: SweepId,
    selector: Selector,
    action: Action,
    job: Option<JobId>,
    /// Писем в выборке: пока снимок идёт — по ответу сервера, затем — в снимке.
    total: u64,
    /// Позиция следующей страницы снимка у сервера.
    position: u64,
    pages: u32,
    /// Страниц применено и писем в них.
    applied: u32,
    done: u64,
    phase: Phase,
}

/// Где выборка.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Снимок: `done` — писем в снимке.
    Listing,
    Working,
    Done,
    Stopped,
    /// Путь не умеет выборку: ответ как есть.
    Failed(String),
}

/// Ход выборки — интерфейсу.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub sweep: SweepId,
    pub action: Action,
    /// Задание переноса: идёт оно — ход показывает оно.
    pub job: Option<JobId>,
    pub done: u64,
    pub total: u64,
    pub state: State,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wait {
    Idle,
    /// Команда ушла, ждём ответа.
    Asked,
    Until(Instant),
}

struct Run {
    row: SweepRow,
    wait: Wait,
    /// Ключи снимка: новое письмо сверху сдвигает страницы, письмо может прийти дважды.
    seen: HashSet<String>,
    /// Строки страницы запрошены, ключи писем, которых на сервере уже нет.
    stored: bool,
    gone: HashSet<String>,
    /// Задание на паузе или учётка не вошла: будить незачем.
    blocked: bool,
    dirty: bool,
    forced: bool,
    shown: Option<Instant>,
}

impl Run {
    fn new(row: SweepRow) -> Self {
        Self {
            row,
            wait: Wait::Idle,
            seen: HashSet::new(),
            stored: false,
            gone: HashSet::new(),
            blocked: false,
            dirty: true,
            forced: true,
            shown: None,
        }
    }

    fn progress(&self, state: State) -> Progress {
        let r = &self.row;
        let done = if r.phase == Phase::Listing { r.position } else { r.done };
        Progress { sweep: r.id, action: r.action.clone(), job: r.job, done, total: r.total, state }
    }

    fn state(&self) -> State {
        if self.row.phase == Phase::Listing { State::Listing } else { State::Working }
    }

    fn source(&self) -> Option<String> {
        postman_cache::profile_of(postman_cache::account_of(&self.row.selector.mailbox)).map(str::to_owned)
    }
}

/// Команда по ключам кэша — маршрутом учётки.
fn sync_routes(c: Command) -> impl Iterator<Item = (Route, Command)> {
    c.routes().into_iter().map(|(t, c)| (Route::Sync(t), c))
}

/// Действия над выборками; живёт в потоке ядра рядом с `migrate::Engine`.
pub struct Sweeper {
    cache: Cache,
    runs: BTreeMap<SweepId, Run>,
    online: HashSet<Option<String>>,
    out: Output,
    moved: bool,
}

impl Sweeper {
    /// Выборки из базы: с той страницы, где их застал выход. Недописанный снимок снимается заново, экспорт не хранится.
    pub fn open(cache: Cache) -> Result<Self> {
        let rows: Vec<SweepRow> = match cache.state(INDEX)? {
            Some(text) => serde_json::from_str(&text).map_err(crate::model::Error::json)?,
            None => Vec::new(),
        };
        let mut s = Self { cache, runs: BTreeMap::new(), online: HashSet::new(), out: Output::default(), moved: false };
        for mut row in rows {
            if matches!(row.action, Action::Export { .. }) {
                s.forget_pages(&row)?;
                continue;
            }
            if row.phase == Phase::Listing {
                s.forget_pages(&row)?;
                (row.position, row.pages) = (0, 0);
            }
            s.runs.insert(row.id, Run::new(row));
        }
        s.save()?;
        Ok(s)
    }

    /// Задания незаконченных выборок — для `Engine::open_held`.
    pub fn held(&self) -> Vec<(JobId, FolderId, u64)> {
        self.runs
            .values()
            .filter_map(|r| match (&r.row.action, r.row.job) {
                (Action::Transfer { to }, Some(job)) => Some((job, *to, r.row.total.saturating_sub(r.row.done))),
                _ => None,
            })
            .collect()
    }

    /// Сколько писем ещё придёт в задание `job` (показ задания при запуске).
    pub fn expected(cache: &Cache, job: JobId) -> u64 {
        let rows: Vec<SweepRow> = cache.state(INDEX).ok().flatten().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        rows.iter().find(|r| r.job == Some(job)).map_or(0, |r| r.total.saturating_sub(r.done))
    }

    /// Учётная запись профиля `profile` вошла на сервер (`Status::SignedIn`).
    pub fn online(&mut self, profile: Option<&str>) {
        self.online.insert(profile.map(str::to_owned));
    }

    /// Новая выборка.
    pub fn start(&mut self, selector: Selector, action: Action) -> Result<SweepId> {
        let id = SweepId(self.runs.keys().last().map_or(1, |k| k.0 + 1));
        let row = SweepRow { id, selector, action, job: None, total: 0, position: 0, pages: 0, applied: 0, done: 0, phase: Phase::Listing };
        self.runs.insert(id, Run::new(row));
        self.save()?;
        Ok(id)
    }

    /// «Остановить»: сделанное остаётся. Письма, уже пришедшие в задание переноса, задание вернёт на место.
    pub fn stop(&mut self, id: SweepId, engine: &mut Engine<CacheStore>) -> Result<()> {
        let Some(run) = self.runs.get(&id) else { return Ok(()) };
        if let Some(job) = run.row.job {
            engine.request(MigrationRequest::Cancel { job, keep_local: false })?;
        }
        self.end(id, State::Stopped)
    }

    /// Ответ пути.
    pub fn event(&mut self, e: &Event, now: Instant) -> Result<()> {
        match e {
            Event::LettersListed { sweep, ids, total, end } => {
                let Some(run) = self.runs.get_mut(sweep).filter(|r| r.row.phase == Phase::Listing && r.wait == Wait::Asked) else { return Ok(()) };
                run.wait = Wait::Idle;
                let except: HashSet<&String> = run.row.selector.except.iter().collect();
                let fresh: Vec<&String> = ids.iter().filter(|k| !except.contains(k) && !run.seen.contains(*k)).collect();
                let line = serde_json::to_string(&fresh).map_err(crate::model::Error::json)?;
                let n = run.row.pages;
                run.seen.extend(fresh.iter().map(|k| (*k).clone()));
                self.cache.set_state(&page_key(*sweep, n), &line)?;
                run.row.pages += 1;
                run.row.position += ids.len() as u64;
                run.row.total = total.saturating_sub(run.row.selector.except.len() as u64);
                if *end || ids.is_empty() {
                    run.row.total = run.seen.len() as u64;
                    run.row.phase = Phase::Applying;
                    run.seen = HashSet::new();
                    run.forced = true;
                }
                run.dirty = true;
                self.save()?;
            }
            Event::LettersStored { sweep, gone } => {
                let Some(run) = self.runs.get_mut(sweep).filter(|r| r.wait == Wait::Asked) else { return Ok(()) };
                run.wait = Wait::Idle;
                run.stored = true;
                run.gone = gone.iter().cloned().collect();
            }
            Event::ListFailed { sweep, error } => {
                let Some(run) = self.runs.get_mut(sweep).filter(|r| r.wait == Wait::Asked) else { return Ok(()) };
                if error.contains(FOLDER_UNSUPPORTED) {
                    tracing::debug!(sweep = sweep.0, "sweep: папка не поддерживает очистку: {error}");
                    return self.end(*sweep, State::Failed(error.clone()));
                }
                tracing::warn!(sweep = sweep.0, "sweep: страница не получена, повтор: {error}");
                run.wait = Wait::Until(now + RETRY);
            }
            _ => {}
        }
        Ok(())
    }

    /// Шаги, которые можно сделать сейчас: команды путям, ход интерфейсу. Письма ушли в другую папку — `true` у
    /// [`Sweeper::take_moved`].
    pub fn poll(&mut self, engine: &mut Engine<CacheStore>, now: Instant) -> Result<Output> {
        let ids: Vec<SweepId> = self.runs.keys().copied().collect();
        for id in ids {
            self.step(id, engine, now)?;
            if let Some(run) = self.runs.get_mut(&id)
                && (run.forced || (run.dirty && run.shown.is_none_or(|t| now >= t + PROGRESS_EVERY)))
            {
                let p = run.progress(run.state());
                self.out.events.push(Event::Sweep(p));
                (run.dirty, run.forced, run.shown) = (false, false, Some(now));
            }
        }
        Ok(std::mem::take(&mut self.out))
    }

    /// Были ли письма перенесены с прошлого вопроса: списки и дерево заново.
    pub fn take_moved(&mut self) -> bool {
        std::mem::take(&mut self.moved)
    }

    /// Когда снова звать [`Sweeper::poll`] без новых событий.
    pub fn wake_at(&self, now: Instant) -> Option<Instant> {
        let mut at: Option<Instant> = None;
        let mut soon = |t: Instant| at = Some(at.map_or(t, |a| a.min(t)));
        for r in self.runs.values() {
            match r.wait {
                Wait::Idle if !r.blocked => soon(now),
                Wait::Until(t) => soon(t),
                _ => {}
            }
            if r.dirty
                && let Some(t) = r.shown
            {
                soon(t + PROGRESS_EVERY);
            }
        }
        at
    }

    fn step(&mut self, id: SweepId, engine: &mut Engine<CacheStore>, now: Instant) -> Result<()> {
        let online = self.online.clone();
        let Some(run) = self.runs.get_mut(&id) else { return Ok(()) };
        if let Wait::Until(t) = run.wait {
            if now < t {
                return Ok(());
            }
            run.wait = Wait::Idle;
        }
        if run.wait != Wait::Idle {
            return Ok(());
        }
        run.blocked = false;
        if run.row.phase == Phase::Applying && run.row.applied >= run.row.pages {
            return self.finish(id, engine);
        }
        if let Some(job) = run.row.job {
            match engine.job_state(job) {
                None | Some(MigrationState::Cancelled) => return self.end(id, State::Stopped),
                Some(MigrationState::Paused | MigrationState::QuotaFull) => {
                    run.blocked = true;
                    return Ok(());
                }
                _ => {}
            }
        }
        if !online.contains(&run.source()) {
            run.blocked = true;
            return Ok(());
        }
        match run.row.phase {
            Phase::Listing => {
                let c = Command::ListLetters { sweep: id, selector: run.row.selector.clone(), position: run.row.position, limit: PAGE };
                run.wait = Wait::Asked;
                self.out.commands.extend(sync_routes(c));
                Ok(())
            }
            Phase::Applying => self.apply(id, engine),
        }
    }

    /// Страница снимка: строки недостающих писем — у сервера, затем действие.
    fn apply(&mut self, id: SweepId, engine: &mut Engine<CacheStore>) -> Result<()> {
        let Some(run) = self.runs.get_mut(&id) else { return Ok(()) };
        let page = page_key(id, run.row.applied);
        let ids: Vec<String> = match self.cache.state(&page)? {
            Some(text) => serde_json::from_str(&text).map_err(crate::model::Error::json)?,
            None => Vec::new(),
        };
        if !run.stored {
            let mut missing = Vec::new();
            for key in &ids {
                if find(&self.cache, key)?.is_none() {
                    missing.push(key.clone());
                }
            }
            if !missing.is_empty() {
                run.wait = Wait::Asked;
                let c = Command::StoreLetters { sweep: id, mailbox: run.row.selector.mailbox.clone(), ids: missing };
                self.out.commands.extend(sync_routes(c));
                return Ok(());
            }
        }
        let gone = std::mem::take(&mut run.gone);
        run.stored = false;
        let live: Vec<String> = ids.iter().filter(|k| !gone.contains(*k)).cloned().collect();
        match run.row.action.clone() {
            Action::Transfer { to } => {
                let left = run.row.total.saturating_sub(run.row.done);
                let job = match run.row.job {
                    Some(job) => job,
                    None => {
                        let job = engine.begin(to, left)?;
                        run.row.job = Some(job);
                        self.save()?;
                        job
                    }
                };
                let mut letters: Vec<LetterId> = Vec::with_capacity(live.len());
                for key in &live {
                    // Уже перенесённое (страница повторяется после перезапуска) — связь `detached`.
                    if let Some((letter, false)) = find(&self.cache, key)? {
                        letters.push(letter);
                    }
                }
                if !letters.is_empty() {
                    move_into(engine, &letters, to, job)?;
                    self.moved = true;
                }
            }
            Action::Patch { act, archive_name } => {
                let rows = self.cache.rows_by_ids(&live)?;
                let plan = bulk::plan(&rows, &act, &self.cache.mailboxes()?, |_, _| true);
                self.patch(plan.patches, archive_name)?;
            }
            Action::Mark { kind, when, archive_name } => {
                let rows = self.cache.rows_by_ids(&live)?;
                let account = postman_cache::account_of(&run.row.selector.mailbox).map(str::to_owned);
                let roles = lifecycle::Roles::of(&self.cache.mailboxes()?, account.as_deref());
                let when = when.as_deref().and_then(parse_when);
                self.patch(rows.iter().map(|r| lifecycle::mark(r, kind, when, &roles)).collect(), archive_name)?;
            }
            Action::Purge => {
                self.cache.delete_emails(&live)?;
                self.moved = true;
                for key in live {
                    self.out.commands.extend(sync_routes(Command::Destroy(key)));
                }
            }
            Action::Export { .. } => {}
        }
        let Some(run) = self.runs.get_mut(&id) else { return Ok(()) };
        run.row.applied += 1;
        run.row.done += ids.len() as u64;
        run.dirty = true;
        if let Some(job) = run.row.job {
            engine.expect(job, run.row.total.saturating_sub(run.row.done));
        }
        if !matches!(run.row.action, Action::Export { .. }) {
            self.cache.remove_state(&page)?;
        }
        self.save()
    }

    /// Правки страницы: кэш сразу, сервер — одной командой.
    fn patch(&mut self, patches: Vec<lifecycle::Patch>, archive_name: String) -> Result<()> {
        if patches.is_empty() {
            return Ok(());
        }
        let local: Vec<_> = patches.iter().map(lifecycle::for_cache).collect();
        self.cache.patch_emails(&local)?;
        self.moved = true;
        self.out.commands.extend(sync_routes(Command::Patch { patches, archive_name }));
        Ok(())
    }

    /// Все страницы применены.
    fn finish(&mut self, id: SweepId, engine: &mut Engine<CacheStore>) -> Result<()> {
        let Some(run) = self.runs.get(&id) else { return Ok(()) };
        if let Some(job) = run.row.job {
            engine.expect(job, 0);
        }
        if let Action::Export { format, dir } = run.row.action.clone() {
            let mut letters = Vec::new();
            for n in 0..run.row.pages {
                let ids: Vec<String> = match self.cache.state(&page_key(id, n))? {
                    Some(text) => serde_json::from_str(&text).map_err(crate::model::Error::json)?,
                    None => Vec::new(),
                };
                for key in &ids {
                    if let Some((letter, false)) = find(&self.cache, key)? {
                        letters.push(letter);
                    }
                }
            }
            let scope = export::Scope::Letters(letters);
            self.out.commands.push((Route::Core, Command::Export { scope, format, dir }));
        }
        self.end(id, State::Done)
    }

    fn end(&mut self, id: SweepId, state: State) -> Result<()> {
        let Some(run) = self.runs.remove(&id) else { return Ok(()) };
        self.forget_pages(&run.row)?;
        self.out.events.push(Event::Sweep(run.progress(state)));
        self.save()
    }

    fn forget_pages(&self, row: &SweepRow) -> Result<()> {
        for n in 0..row.pages {
            self.cache.remove_state(&page_key(row.id, n))?;
        }
        Ok(())
    }

    fn save(&self) -> Result<()> {
        let rows: Vec<&SweepRow> = self.runs.values().map(|r| &r.row).collect();
        let line = serde_json::to_string(&rows).map_err(crate::model::Error::json)?;
        Ok(self.cache.set_state(INDEX, &line)?)
    }
}

/// Письмо по ключу кэша и перенесено ли оно уже; строки нет — `None`.
fn find(cache: &Cache, key: &str) -> postman_cache::Result<Option<(LetterId, bool)>> {
    let (account, id) = postman_cache::unscoped(key);
    Ok(cache.find_letter(account.unwrap_or(""), id)?.map(|(l, detached)| (LetterId(l.0), detached)))
}

#[cfg(test)]
mod tests;
