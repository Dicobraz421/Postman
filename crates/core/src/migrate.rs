//! Перенос писем в другую учётку и в «На этом компьютере» (ADR 0006 п. 4–5): очередь заданий в потоке ядра.
//!
//! Задание создаёт перенос в базе (`Command::MoveLetters` → строки `outbound`); движок ведёт его письма по шагам:
//! содержимое есть в базе (нет — `FetchRaw` у источника) → выгрузка в папку цели (`Import`, между аккаунтами одной
//! сессии — `ServerCopy` пачкой) → сервер подтвердил (связь с целью, `Synced`) → удаление у источника
//! (`DestroyLetters`), снять `detached`-связь. Письмо у источника удалено только по ответу его сервера
//! (`Event::Destroyed`, решение владельца 2026-10-29): отказ — повтор после паузы, не больше [`DESTROY_ATTEMPTS`]
//! раз; затем письмо выходит из задания с пометкой «не удалено у источника» (решение владельца 2026-10-30), итог —
//! `Event::SourceKept`. Режим «локальный архив» — содержимое (хэш сверен), затем удаление у источника, `Local`.
//! Каждый шаг записан в хранилище до команды и после ответа: после перезапуска задание идёт с того же шага.
//!
//! Движок без I/O и без потоков: поток ядра отдаёт ему ответы путей ([`Engine::event`], [`Engine::quota`]) и
//! просьбы пользователя ([`Engine::request`]), забирает команды путям и события интерфейсу ([`Engine::poll`]) и будит
//! его к сроку ([`Engine::wake_at`]). Хранилище — [`Store`] (формат 6, `postman_cache`; в тестах — в памяти).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::model::{Feature, Features, FolderId, ImportError, JobId, LetterId, MigrationMode, MigrationRequest, MigrationState, Remote, Result};
use crate::settings::{self, Usage};
use crate::sync::{Command, Event, Home, Homes, Route};

/// Квота цели — перед первой выгрузкой и после каждых стольких выгруженных писем.
pub const QUOTA_EVERY: u64 = 500;
/// Ответа о квоте ждём не дольше: нет ответа — выгрузка идёт, место кончится — скажет сервер ([`ImportError::Quota`]).
pub const QUOTA_WAIT: Duration = Duration::from_secs(30);
/// Ход задания (`Event::Migration`) — не чаще 4 раз в секунду; смена состояния — сразу.
pub const PROGRESS_EVERY: Duration = Duration::from_millis(250);
/// Пауза перед повтором письма: содержимое не загрузилось, сервер цели отказал.
pub const RETRY: Duration = Duration::from_secs(30);
/// Столько отказов сервера цели ([`ImportError::Rejected`]) — и письмо остаётся локальным с пометкой.
pub const ATTEMPTS: u32 = 3;
/// Попыток удалить письмо у источника (между ними — [`RETRY`]); все — отказ: письмо выходит из задания с пометкой
/// «не удалено у источника» ([`Store::kept`]). Счёт — с запуска движка.
pub const DESTROY_ATTEMPTS: u32 = 10;
/// Писем, которые одно задание одновременно загружает у источника.
const FETCHES: usize = 4;

/// Шаг письма задания; задание начинается с первого.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Письмо целиком есть в базе (нет — загрузить у источника).
    Content,
    /// Выгрузить в папку цели.
    Upload,
    /// Выгрузка отправлена, ждём подтверждения: после перезапуска и после отказа — повтор с проверкой по `Message-ID`
    /// (ответ мог потеряться, а письмо — дойти).
    Confirm,
    /// Удалить у источника и снять `detached`-связь.
    DeleteSource,
}

/// Строка задания по письму: куда и на каком шаге.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outbound {
    pub to: FolderId,
    pub step: Step,
}

/// Задание переноса в хранилище: режим, состояние, всего писем, байт выгружено.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobRow {
    pub id: JobId,
    pub mode: MigrationMode,
    pub state: MigrationState,
    pub total: u64,
    pub bytes: u64,
}

/// Что движку нужно о письме.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LetterInfo {
    /// Письмо целиком в базе, хэш сверен.
    pub content: bool,
    pub size: u64,
    /// Ключевые слова для цели, с `$seen` и `$flagged`.
    pub keywords: Vec<String>,
    /// Unix-секунды.
    pub received_at: i64,
    /// Где письмо у источника: его `detached`-связи (учётка — аккаунт ключей кэша).
    pub sources: Vec<Remote>,
}

/// Пределы пути цели: писем в пачке (`ServerCopy`, удаления; JMAP — `maxObjectsInSet`, EWS — до 20) и одновременных
/// выгрузок.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub batch: usize,
    pub imports: usize,
}

impl Limits {
    pub const DEFAULT: Limits = Limits { batch: 20, imports: 2 };
}

/// Хранилище заданий и писем (формат 6). Каждый метод записи — одна транзакция; владелец папки — [`Homes`].
pub trait Store: Homes {
    /// Все задания с письмами в `outbound`.
    fn jobs(&self) -> Result<Vec<JobRow>>;
    fn save_job(&mut self, job: &JobRow) -> Result<()>;
    /// Задание кончилось: его строки больше не нужны.
    fn drop_job(&mut self, job: JobId) -> Result<()>;
    fn outbound(&self, job: JobId) -> Result<Vec<(LetterId, Outbound)>>;
    fn set_step(&mut self, job: JobId, letter: LetterId, step: Step) -> Result<()>;
    /// `None` — письма в базе нет.
    fn info(&self, id: LetterId) -> Result<Option<LetterInfo>>;
    /// Письмо целиком с сервера источника → содержимое (части — воркером, хэш сверен).
    fn put_content(&mut self, letter: LetterId, raw: &[u8]) -> Result<()>;
    /// Сервер цели принял письмо: связь с ним, `place = Synced`, шаг — [`Step::DeleteSource`].
    fn confirm(&mut self, job: JobId, letter: LetterId, remote: &Remote) -> Result<()>;
    /// Локальный архив: письмо только в базе (`place = Local`), шаг — [`Step::DeleteSource`].
    fn archive(&mut self, job: JobId, letter: LetterId) -> Result<()>;
    /// Сервер цели письмо не берёт: оно только в базе с пометкой `why`, строка задания убрана; у источника не
    /// удаляется.
    fn refused(&mut self, job: JobId, letter: LetterId, why: &ImportError) -> Result<()>;
    /// Источник подтвердил удаление: строка задания и `detached`-связи письма убраны.
    fn finish(&mut self, job: JobId, letter: LetterId) -> Result<()>;
    /// Источник письмо так и не удалил ([`DESTROY_ATTEMPTS`] отказов): строка задания убрана, `detached`-связи
    /// остаются — пометка «не удалено у источника».
    fn kept(&mut self, job: JobId, letter: LetterId) -> Result<()>;
    /// Отмена: письма — одной транзакцией обратно в папку источника с прежними связями, строки задания убраны.
    fn restore(&mut self, job: JobId, letters: &[LetterId]) -> Result<()>;
}

/// Что отдать после [`Engine::poll`]: команды — путям по маршруту, события — интерфейсу.
#[derive(Default)]
pub struct Output {
    pub commands: Vec<(Route, Command)>,
    pub events: Vec<Event>,
}

/// Учётная запись профиля учётки (аккаунт ключей кэша; основная — пустая строка): ключ пределов, квоты и умений.
type Profile = Option<String>;

fn profile_of(account: &str) -> Profile {
    postman_cache::profile_of(Some(account).filter(|a| !a.is_empty())).map(str::to_owned)
}

struct Letter {
    to: FolderId,
    step: Step,
    /// Команда ушла, ждём ответа.
    busy: bool,
    attempts: u32,
    size: u64,
    /// Выгрузка (`Import` или пачка `ServerCopy`) — номер в [`Net::slots`].
    slot: Option<u64>,
    /// Перенесено `ServerCopy`: источник удалил сервер.
    copied: bool,
    /// Удаление у источника: ответов ещё ждём (по учётке источника), был ли отказ и сколько попыток кончились им.
    pending: usize,
    failed: bool,
    destroy_failures: u32,
}

impl Letter {
    fn new(to: FolderId, step: Step, size: u64) -> Self {
        Self { to, step, busy: false, attempts: 0, size, slot: None, copied: false, pending: 0, failed: false, destroy_failures: 0 }
    }
}

#[derive(Default)]
struct Quota {
    /// Спросили и ждём ответа.
    asked: Option<Instant>,
    checked: bool,
    /// Выгружено после последней проверки.
    since: u64,
    /// «Продолжить» после «нет места»: проверки больше не останавливают, остановит отказ сервера.
    waived: bool,
}

struct Job {
    row: JobRow,
    target: Home,
    letters: BTreeMap<LetterId, Letter>,
    content: VecDeque<LetterId>,
    upload: VecDeque<LetterId>,
    delete: VecDeque<LetterId>,
    waiting: Vec<(Instant, LetterId)>,
    fetching: usize,
    quota: Quota,
    /// Отменено: `Some(keep_local)`.
    cancel: Option<bool>,
    /// Вернулись в папку источника при отмене — не «готово».
    returned: u64,
    /// Вышли из задания с пометкой «не удалено у источника» — итог в конце.
    kept: u64,
    dirty: bool,
    /// Смена состояния — показать сразу.
    forced: bool,
    shown: Option<Instant>,
}

impl Job {
    fn new(row: JobRow, target: Home) -> Self {
        Self {
            row,
            target,
            letters: BTreeMap::new(),
            content: VecDeque::new(),
            upload: VecDeque::new(),
            delete: VecDeque::new(),
            waiting: Vec::new(),
            fetching: 0,
            quota: Quota::default(),
            cancel: None,
            returned: 0,
            kept: 0,
            dirty: true,
            forced: true,
            shown: None,
        }
    }

    fn done(&self) -> u64 {
        self.row.total.saturating_sub(self.letters.len() as u64).saturating_sub(self.returned)
    }

    fn profile(&self) -> Option<Profile> {
        match &self.target {
            Home::Account(a) => Some(profile_of(a)),
            Home::Local => None,
        }
    }

    fn queue(&mut self, id: LetterId, step: Step) {
        match step {
            Step::Content => self.content.push_back(id),
            Step::Upload | Step::Confirm => self.upload.push_back(id),
            Step::DeleteSource => self.delete.push_back(id),
        }
    }

    fn set_state(&mut self, state: MigrationState, store: &mut dyn Store) -> Result<()> {
        if self.row.state != state {
            self.row.state = state;
            self.forced = true;
            store.save_job(&self.row)?;
        }
        Ok(())
    }

    /// Письмо ушло из задания: удалено, вернулось или осталось локальным.
    fn remove(&mut self, id: LetterId) {
        self.letters.remove(&id);
        self.dirty = true;
    }

    /// Письмо ждёт повтора.
    fn later(&mut self, id: LetterId, now: Instant) {
        self.waiting.push((now + RETRY, id));
    }

    /// Содержимое в базе: дальше — выгрузка или (локальный архив) удаление у источника.
    fn after_content(&mut self, id: LetterId, store: &mut dyn Store) -> Result<()> {
        if self.cancel.is_some() {
            return self.settle_cancelled(&[id], store);
        }
        let step = if self.row.mode == MigrationMode::LocalArchive {
            store.archive(self.row.id, id)?;
            Step::DeleteSource
        } else {
            store.set_step(self.row.id, id, Step::Upload)?;
            Step::Upload
        };
        if let Some(l) = self.letters.get_mut(&id) {
            l.step = step;
        }
        self.queue(id, step);
        Ok(())
    }

    /// Письма отменённого задания без команды в пути: подтверждённые целью — удалить у источника; с содержимым и
    /// «оставить локально» — архив; прочие — одной транзакцией обратно к источнику.
    fn settle_cancelled(&mut self, ids: &[LetterId], store: &mut dyn Store) -> Result<()> {
        let mut back = Vec::new();
        for &id in ids {
            let Some(step) = self.letters.get(&id).filter(|l| !l.busy).map(|l| l.step) else { continue };
            if step == Step::DeleteSource {
                self.delete.push_back(id);
            } else if self.cancel == Some(true) && store.info(id)?.is_some_and(|l| l.content) {
                store.archive(self.row.id, id)?;
                if let Some(l) = self.letters.get_mut(&id) {
                    l.step = Step::DeleteSource;
                }
                self.delete.push_back(id);
            } else {
                back.push(id);
            }
        }
        if !back.is_empty() {
            store.restore(self.row.id, &back)?;
            for id in &back {
                self.remove(*id);
            }
            self.returned += back.len() as u64;
        }
        Ok(())
    }
}

/// Общее для заданий: умения и пределы путей, паузы по просьбе сервера, выгрузки в пути, команды и события.
#[derive(Default)]
struct Net {
    features: HashMap<Profile, Features>,
    limits: HashMap<Profile, Limits>,
    throttled: HashMap<Profile, Instant>,
    /// Выгрузок в пути по учётной записи цели.
    imports: HashMap<Profile, usize>,
    /// Выгрузка → (учётная запись цели, писем без ответа).
    slots: HashMap<u64, (Profile, usize)>,
    next_slot: u64,
    out: Output,
}

impl Net {
    fn limits(&self, p: &Profile) -> Limits {
        self.limits.get(p).copied().unwrap_or(Limits::DEFAULT)
    }

    fn open_slot(&mut self, p: &Profile, letters: usize) -> u64 {
        self.next_slot += 1;
        self.slots.insert(self.next_slot, (p.clone(), letters));
        *self.imports.entry(p.clone()).or_default() += 1;
        self.next_slot
    }

    /// Ответ по письму выгрузки: последнее письмо — выгрузка кончилась.
    fn release(&mut self, slot: Option<u64>) {
        let Some(slot) = slot else { return };
        let Some((p, left)) = self.slots.get_mut(&slot) else { return };
        *left = left.saturating_sub(1);
        if *left == 0 {
            let p = p.clone();
            self.slots.remove(&slot);
            if let Some(n) = self.imports.get_mut(&p) {
                *n = n.saturating_sub(1);
            }
        }
    }

    fn send(&mut self, route: Route, cmd: Command) {
        self.out.commands.push((route, cmd));
    }
}

/// Движок заданий переноса.
pub struct Engine<S: Store> {
    store: S,
    jobs: BTreeMap<JobId, Job>,
    net: Net,
}

impl<S: Store> Engine<S> {
    /// Задания из хранилища — с того шага, где их застал выход (или сбой).
    pub fn open(store: S) -> Result<Self> {
        let mut e = Self { store, jobs: BTreeMap::new(), net: Net::default() };
        for row in e.store.jobs()? {
            e.load(row)?;
        }
        Ok(e)
    }

    /// Хранилище: поток ядра даёт его маршруту команд (`Command::routes_by`) и читает списки.
    pub fn store(&self) -> &S {
        &self.store
    }

    pub fn into_store(self) -> S {
        self.store
    }

    /// Умения пути учётной записи профиля `profile` (`Event::Features`). Пока путь не сказал — выгрузка без проверки
    /// квоты и без копии на сервере.
    pub fn features(&mut self, profile: Option<&str>, f: Features) {
        self.net.features.insert(profile.map(str::to_owned), f);
    }

    /// Пределы пути учётной записи профиля `profile`; не сказаны — [`Limits::DEFAULT`].
    pub fn limits(&mut self, profile: Option<&str>, l: Limits) {
        self.net.limits.insert(profile.map(str::to_owned), l);
    }

    fn load(&mut self, row: JobRow) -> Result<()> {
        let rows = self.store.outbound(row.id)?;
        let Some((_, first)) = rows.first() else {
            return self.store.drop_job(row.id);
        };
        let mut job = Job::new(row, self.store.folder(first.to));
        if row.state == MigrationState::Cancelled {
            job.cancel = Some(row.mode == MigrationMode::LocalArchive);
        }
        for (id, ob) in rows {
            let Some(info) = self.store.info(id)? else { continue };
            job.letters.insert(id, Letter::new(ob.to, ob.step, info.size));
            job.queue(id, ob.step);
        }
        self.jobs.insert(row.id, job);
        Ok(())
    }

    /// Перенос в базе прошёл (`Cache::move_letters`): новое задание `job` (перенос в «На этом компьютере» — локальный
    /// архив без вопроса, в учётку — вопрос), у прежних — их письма заново (часть могли перенести обратно).
    pub fn moved(&mut self, job: Option<JobId>) -> Result<()> {
        for j in self.jobs.values_mut() {
            let rows: BTreeMap<LetterId, Outbound> = self.store.outbound(j.row.id)?.into_iter().collect();
            let gone: Vec<LetterId> = j.letters.iter().filter(|(id, l)| !l.busy && !rows.contains_key(id)).map(|(id, _)| *id).collect();
            for id in gone {
                j.remove(id);
                j.row.total = j.row.total.saturating_sub(1);
            }
            for (id, ob) in rows {
                match j.letters.get_mut(&id) {
                    Some(l) => l.to = ob.to,
                    None => {
                        let Some(info) = self.store.info(id)? else { continue };
                        j.letters.insert(id, Letter::new(ob.to, ob.step, info.size));
                        j.queue(id, ob.step);
                        j.row.total += 1;
                    }
                }
            }
            self.store.save_job(&j.row)?;
        }
        let Some(id) = job.filter(|id| !self.jobs.contains_key(id)) else { return Ok(()) };
        let rows = self.store.outbound(id)?;
        let Some((_, first)) = rows.first() else { return Ok(()) };
        let (mode, state) = match self.store.folder(first.to) {
            Home::Local => (MigrationMode::LocalArchive, MigrationState::Running),
            Home::Account(_) => (MigrationMode::Ask, MigrationState::Asking),
        };
        let row = JobRow { id, mode, state, total: rows.len() as u64, bytes: 0 };
        self.store.save_job(&row)?;
        self.load(row)
    }

    /// Просьба пользователя: ответ на вопрос, пауза, продолжение, отмена.
    pub fn request(&mut self, r: MigrationRequest) -> Result<()> {
        let id = match r {
            MigrationRequest::Answer { job, .. } | MigrationRequest::Pause { job } | MigrationRequest::Resume { job } => job,
            MigrationRequest::Cancel { job, .. } => job,
        };
        let Some(j) = self.jobs.get_mut(&id) else { return Ok(()) };
        if j.cancel.is_some() {
            return Ok(());
        }
        let store: &mut dyn Store = &mut self.store;
        match r {
            MigrationRequest::Answer { mode, .. } => {
                if mode != MigrationMode::Ask && j.row.mode != mode {
                    j.row.mode = mode;
                    j.forced = true;
                    store.save_job(&j.row)?;
                }
                if j.row.state == MigrationState::Asking && j.row.mode != MigrationMode::Ask {
                    j.set_state(MigrationState::Running, store)?;
                }
            }
            MigrationRequest::Pause { .. } => {
                if matches!(j.row.state, MigrationState::Asking | MigrationState::Running | MigrationState::QuotaFull) {
                    j.set_state(MigrationState::Paused, store)?;
                }
            }
            MigrationRequest::Resume { .. } => {
                if matches!(j.row.state, MigrationState::Paused | MigrationState::QuotaFull) {
                    j.quota.waived |= j.row.state == MigrationState::QuotaFull;
                    let state = if j.row.mode == MigrationMode::Ask { MigrationState::Asking } else { MigrationState::Running };
                    j.set_state(state, store)?;
                }
            }
            MigrationRequest::Cancel { keep_local, .. } => {
                j.cancel = Some(keep_local);
                // «Оставить локально» — режим локального архива: так отмена переживает перезапуск ([`Engine::load`]).
                if keep_local {
                    j.row.mode = MigrationMode::LocalArchive;
                }
                // Ждущие ответа пути — по ответу.
                let idle: Vec<LetterId> = j.letters.iter().filter(|(_, l)| !l.busy && l.step != Step::DeleteSource).map(|(id, _)| *id).collect();
                j.settle_cancelled(&idle, store)?;
                j.set_state(MigrationState::Cancelled, store)?;
            }
        }
        Ok(())
    }

    /// Ответ пути: письмо целиком, выгрузка принята или нет, удаление у источника. Прочие события — не движку.
    pub fn event(&mut self, e: &Event, now: Instant) -> Result<()> {
        match e {
            Event::Raw { letter, data } => {
                let Some(j) = self.jobs.values_mut().find(|j| j.letters.get(letter).is_some_and(|l| l.busy && l.step == Step::Content)) else {
                    return Ok(());
                };
                j.fetching = j.fetching.saturating_sub(1);
                if let Some(l) = j.letters.get_mut(letter) {
                    l.busy = false;
                }
                let stored = match data {
                    Ok(raw) => self.store.put_content(*letter, raw).map_err(|e| e.to_string()),
                    Err(e) => Err(e.clone()),
                };
                match stored {
                    Ok(()) => j.after_content(*letter, &mut self.store)?,
                    Err(e) => {
                        tracing::warn!(job = j.row.id.0, letter = letter.0, "migration: no content yet: {e}");
                        if j.cancel.is_some() {
                            j.settle_cancelled(&[*letter], &mut self.store)?;
                        } else {
                            j.later(*letter, now);
                        }
                    }
                }
            }
            Event::Imported { job, letter, remote } => {
                let Some(j) = self.jobs.get_mut(job) else { return Ok(()) };
                let Some(l) = j.letters.get_mut(letter).filter(|l| l.busy) else { return Ok(()) };
                l.busy = false;
                l.step = Step::DeleteSource;
                self.net.release(l.slot.take());
                let size = l.size;
                self.store.confirm(*job, *letter, remote)?;
                j.row.bytes += size;
                j.quota.since += 1;
                j.dirty = true;
                j.delete.push_back(*letter);
            }
            Event::ImportFailed { job, letter, error } => {
                let Some(j) = self.jobs.get_mut(job) else { return Ok(()) };
                let Some(l) = j.letters.get_mut(letter).filter(|l| l.busy) else { return Ok(()) };
                l.busy = false;
                l.copied = false;
                self.net.release(l.slot.take());
                l.attempts += matches!(error, ImportError::Rejected(_)) as u32;
                let refused = match error {
                    ImportError::TooLarge => true,
                    ImportError::Rejected(_) => l.attempts >= ATTEMPTS,
                    ImportError::Quota | ImportError::Throttled { .. } => false,
                };
                tracing::warn!(job = job.0, letter = letter.0, "migration: import failed: {error}");
                if refused {
                    self.store.refused(*job, *letter, error)?;
                    j.remove(*letter);
                    return Ok(());
                }
                // Повтор — с проверкой у цели: при обрыве связи выгрузка могла дойти без ответа.
                l.step = Step::Confirm;
                self.store.set_step(*job, *letter, Step::Confirm)?;
                match error {
                    ImportError::Throttled { retry_after } => {
                        if let Some(p) = j.profile() {
                            self.net.throttled.insert(p, now + Duration::from_secs(*retry_after));
                        }
                        j.upload.push_front(*letter);
                    }
                    ImportError::Quota => {
                        j.upload.push_front(*letter);
                        j.quota.waived = false;
                        if matches!(j.row.state, MigrationState::Running | MigrationState::Asking) {
                            j.set_state(MigrationState::QuotaFull, &mut self.store)?;
                        }
                    }
                    _ => j.later(*letter, now),
                }
                if j.cancel.is_some() {
                    j.settle_cancelled(&[*letter], &mut self.store)?;
                }
            }
            Event::Destroyed { job, letter } | Event::DestroyFailed { job, letter, .. } => {
                let Some(j) = self.jobs.get_mut(job) else { return Ok(()) };
                let Some(l) = j.letters.get_mut(letter).filter(|l| l.busy && l.step == Step::DeleteSource) else { return Ok(()) };
                if let Event::DestroyFailed { error, .. } = e {
                    tracing::warn!(job = job.0, letter = letter.0, "migration: source delete failed: {error}");
                    l.failed = true;
                }
                l.pending = l.pending.saturating_sub(1);
                if l.pending > 0 {
                    return Ok(());
                }
                l.busy = false;
                if std::mem::take(&mut l.failed) {
                    l.destroy_failures += 1;
                    if l.destroy_failures < DESTROY_ATTEMPTS {
                        j.later(*letter, now);
                    } else {
                        tracing::warn!(job = job.0, letter = letter.0, "migration: source delete gave up, letter kept at source");
                        self.store.kept(*job, *letter)?;
                        j.remove(*letter);
                        j.kept += 1;
                    }
                } else {
                    self.store.finish(*job, *letter)?;
                    j.remove(*letter);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Квота учётной записи профиля `profile` (`settings::Reply::Quota`; запрос не удался — `None`): не хватит места на
    /// оставшиеся письма — задание «нет места».
    pub fn quota(&mut self, profile: Option<&str>, usage: Option<Usage>) -> Result<()> {
        let p: Profile = profile.map(str::to_owned);
        for j in self.jobs.values_mut().filter(|j| j.quota.asked.is_some() && j.profile().as_ref() == Some(&p)) {
            j.quota = Quota { waived: j.quota.waived, checked: true, ..Quota::default() };
            let Some(u) = usage.filter(|_| !j.quota.waived) else { continue };
            let need: u64 = j.letters.values().filter(|l| l.step != Step::DeleteSource).map(|l| l.size).sum();
            if u.limit.saturating_sub(u.used) < need && j.row.state == MigrationState::Running {
                j.set_state(MigrationState::QuotaFull, &mut self.store)?;
            }
        }
        Ok(())
    }

    /// Шаги, которые можно сделать сейчас: команды путям и ход заданий интерфейсу.
    pub fn poll(&mut self, now: Instant) -> Result<Output> {
        let ids: Vec<JobId> = self.jobs.keys().copied().collect();
        for id in ids {
            let Some(j) = self.jobs.get_mut(&id) else { continue };
            pump(j, &mut self.store, &mut self.net, now)?;
            let end = j.letters.is_empty();
            if end {
                let state = if j.cancel.is_some() { MigrationState::Cancelled } else { MigrationState::Done };
                j.set_state(state, &mut self.store)?;
                j.dirty = true;
            }
            if j.forced || (j.dirty && j.shown.is_none_or(|t| now >= t + PROGRESS_EVERY)) {
                show(j, &mut self.net.out, now);
                if end {
                    if j.kept > 0 {
                        self.net.out.events.push(Event::SourceKept { job: id, letters: j.kept });
                    }
                    self.store.drop_job(id)?;
                    self.jobs.remove(&id);
                } else {
                    self.store.save_job(&j.row)?;
                }
            }
        }
        Ok(std::mem::take(&mut self.net.out))
    }

    /// Когда снова звать [`Engine::poll`] без новых событий: повтор письма, конец паузы сервера, срок ответа о квоте,
    /// отложенный ход. `None` — до следующего события.
    pub fn wake_at(&self) -> Option<Instant> {
        let mut at: Option<Instant> = None;
        let mut soon = |t: Instant| at = Some(at.map_or(t, |a| a.min(t)));
        for j in self.jobs.values() {
            if j.dirty
                && let Some(t) = j.shown
            {
                soon(t + PROGRESS_EVERY);
            }
            // Задание стоит — ни повторов, ни квоты.
            if matches!(j.row.state, MigrationState::Paused | MigrationState::QuotaFull) {
                continue;
            }
            j.waiting.iter().for_each(|(t, _)| soon(*t));
            let uploading =
                j.cancel.is_none() && j.row.mode == MigrationMode::Upload && j.row.state == MigrationState::Running && !j.upload.is_empty();
            if !uploading {
                continue;
            }
            if let Some(t) = j.quota.asked {
                soon(t + QUOTA_WAIT);
            }
            if let Some(t) = j.profile().and_then(|p| self.net.throttled.get(&p)) {
                soon(*t);
            }
        }
        at
    }
}

fn show(j: &mut Job, out: &mut Output, now: Instant) {
    out.events.push(Event::Migration { job: j.row.id, done: j.done(), total: j.row.total, bytes: j.row.bytes, state: j.row.state });
    j.shown = Some(now);
    j.dirty = false;
    j.forced = false;
}

/// Шаги задания, которые можно сделать сейчас.
fn pump(j: &mut Job, store: &mut dyn Store, net: &mut Net, now: Instant) -> Result<()> {
    if matches!(j.row.state, MigrationState::Paused | MigrationState::QuotaFull) {
        return Ok(());
    }
    let (due, wait): (Vec<_>, Vec<_>) = j.waiting.drain(..).partition(|(t, _)| *t <= now);
    j.waiting = wait;
    for (_, id) in due {
        if let Some(step) = j.letters.get(&id).filter(|l| !l.busy).map(|l| l.step) {
            j.queue(id, step);
        }
    }
    if j.cancel.is_some() {
        // Отменено (и до перезапуска): письма очередей — как при отмене.
        let idle: Vec<LetterId> = j.content.drain(..).chain(j.upload.drain(..)).collect();
        j.settle_cancelled(&idle, store)?;
    } else {
        fetch(j, store, net)?;
        match j.row.mode {
            MigrationMode::LocalArchive => {
                while let Some(id) = j.upload.pop_front() {
                    if j.letters.get(&id).is_some_and(|l| !l.busy && matches!(l.step, Step::Upload | Step::Confirm)) {
                        store.archive(j.row.id, id)?;
                        if let Some(l) = j.letters.get_mut(&id) {
                            l.step = Step::DeleteSource;
                        }
                        j.delete.push_back(id);
                    }
                }
            }
            MigrationMode::Upload if j.row.state == MigrationState::Running => upload(j, store, net, now)?,
            _ => {}
        }
    }
    delete(j, store, net)
}

/// Шаг «содержимое»: есть в базе — дальше, нет — загрузить у источника (не больше [`FETCHES`] писем сразу).
fn fetch(j: &mut Job, store: &mut dyn Store, net: &mut Net) -> Result<()> {
    while let Some(id) = j.content.pop_front() {
        if !j.letters.get(&id).is_some_and(|l| !l.busy && l.step == Step::Content) {
            continue;
        }
        let Some(info) = store.info(id)? else {
            j.remove(id);
            continue;
        };
        if info.content {
            j.after_content(id, store)?;
            continue;
        }
        let Some(source) = info.sources.first() else {
            // Ни содержимого, ни источника: выгружать нечего — письмо обратно.
            store.restore(j.row.id, &[id])?;
            j.remove(id);
            j.returned += 1;
            continue;
        };
        if j.fetching >= FETCHES {
            j.content.push_front(id);
            break;
        }
        net.send(Home::Account(source.account.clone()).route(), Command::FetchRaw { letter: id });
        j.fetching += 1;
        if let Some(l) = j.letters.get_mut(&id) {
            l.busy = true;
        }
    }
    Ok(())
}

/// Квота цели известна (или ждать её больше нечего); иначе — спросить и ждать.
fn quota_ready(j: &mut Job, p: &Profile, net: &mut Net, now: Instant) -> bool {
    if !net.features.get(p).is_some_and(|f| f.has(Feature::Quota)) || j.quota.waived {
        return true;
    }
    if let Some(t) = j.quota.asked {
        if now < t + QUOTA_WAIT {
            return false;
        }
        j.quota = Quota { checked: true, ..Quota::default() };
    }
    if j.quota.checked && j.quota.since < QUOTA_EVERY {
        return true;
    }
    net.send(j.target.route(), Command::Settings(settings::Request::Quota));
    j.quota.asked = Some(now);
    false
}

/// Шаг «выгрузка»: не больше [`Limits::imports`] выгрузок на учётку цели; между аккаунтами одной сессии — пачкой
/// `ServerCopy`.
fn upload(j: &mut Job, store: &mut dyn Store, net: &mut Net, now: Instant) -> Result<()> {
    let Some(p) = j.profile() else { return Ok(()) };
    if net.throttled.get(&p).is_some_and(|t| now >= *t) {
        net.throttled.remove(&p);
    }
    if j.upload.is_empty() || net.throttled.contains_key(&p) || !quota_ready(j, &p, net, now) {
        return Ok(());
    }
    let limits = net.limits(&p);
    let copy = net.features.get(&p).is_some_and(|f| f.server_copy());
    let ready = |j: &Job, id: &LetterId| j.letters.get(id).is_some_and(|l| !l.busy && matches!(l.step, Step::Upload | Step::Confirm));
    // Копия на сервере — только из аккаунтов той же учётной записи профиля и не повтором (повтор — с проверкой).
    let copyable = |j: &Job, id: &LetterId, info: &LetterInfo| {
        copy && j.letters.get(id).is_some_and(|l| l.step == Step::Upload)
            && !info.sources.is_empty()
            && info.sources.iter().all(|s| profile_of(&s.account) == p)
    };
    while net.imports.get(&p).copied().unwrap_or(0) < limits.imports {
        let Some(id) = j.upload.pop_front() else { break };
        if !ready(j, &id) {
            continue;
        }
        let Some(info) = store.info(id)? else {
            j.remove(id);
            continue;
        };
        let Some((to, retry)) = j.letters.get(&id).map(|l| (l.to, l.step == Step::Confirm)) else { continue };
        if !info.content {
            store.set_step(j.row.id, id, Step::Content)?;
            if let Some(l) = j.letters.get_mut(&id) {
                l.step = Step::Content;
            }
            j.content.push_back(id);
            continue;
        }
        if copyable(j, &id, &info) {
            let mut batch = vec![id];
            while batch.len() < limits.batch {
                let Some(next) = j.upload.front().copied() else { break };
                if !ready(j, &next) || j.letters.get(&next).is_none_or(|l| l.to != to) {
                    break;
                }
                match store.info(next)? {
                    Some(n) if n.content && copyable(j, &next, &n) => {
                        j.upload.pop_front();
                        batch.push(next);
                    }
                    _ => break,
                }
            }
            let slot = net.open_slot(&p, batch.len());
            for b in &batch {
                store.set_step(j.row.id, *b, Step::Confirm)?;
                if let Some(l) = j.letters.get_mut(b) {
                    (l.step, l.busy, l.slot, l.copied) = (Step::Confirm, true, Some(slot), true);
                }
            }
            net.send(j.target.route(), Command::ServerCopy { job: j.row.id, letters: batch, to });
        } else {
            // До команды: после сбоя письмо повторится с проверкой у цели, а не выгрузится второй раз.
            store.set_step(j.row.id, id, Step::Confirm)?;
            let slot = net.open_slot(&p, 1);
            if let Some(l) = j.letters.get_mut(&id) {
                (l.step, l.busy, l.slot) = (Step::Confirm, true, Some(slot));
            }
            let cmd = Command::Import { job: j.row.id, letter: id, folder: to, keywords: info.keywords, received_at: info.received_at, retry };
            net.send(j.target.route(), cmd);
        }
    }
    Ok(())
}

/// Шаг «удалить у источника»: цель подтвердила или письмо — локальный архив. Источнику — `DestroyLetters` пачками по
/// его учётке; письмо кончается по ответу ([`Engine::event`]). Источник удалил сервер цели (`ServerCopy`) или
/// источника нет — сразу.
fn delete(j: &mut Job, store: &mut dyn Store, net: &mut Net) -> Result<()> {
    let mut by_source: BTreeMap<String, Vec<LetterId>> = BTreeMap::new();
    while let Some(id) = j.delete.pop_front() {
        let Some(copied) = j.letters.get(&id).filter(|l| !l.busy && l.step == Step::DeleteSource).map(|l| l.copied) else { continue };
        let mut accounts: Vec<String> =
            if copied { Vec::new() } else { store.info(id)?.map(|l| l.sources).unwrap_or_default() }.into_iter().map(|s| s.account).collect();
        accounts.sort();
        accounts.dedup();
        if accounts.is_empty() {
            store.finish(j.row.id, id)?;
            j.remove(id);
            continue;
        }
        if let Some(l) = j.letters.get_mut(&id) {
            (l.busy, l.pending, l.failed) = (true, accounts.len(), false);
        }
        for a in accounts {
            by_source.entry(a).or_default().push(id);
        }
    }
    for (account, letters) in by_source {
        let batch = net.limits(&profile_of(&account)).batch.max(1);
        for chunk in letters.chunks(batch) {
            let cmd = Command::DestroyLetters { job: j.row.id, account: account.clone(), letters: chunk.to_vec() };
            net.send(Home::Account(account.clone()).route(), cmd);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::Target;
    use std::collections::{BTreeSet, HashSet};

    /// Где письмо в подменном хранилище.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Place {
        Outbound,
        Synced,
        Local,
        Kept(String),
    }

    #[derive(Debug, Clone)]
    struct Row {
        folder: FolderId,
        source_folder: FolderId,
        content: Option<Vec<u8>>,
        sources: Vec<Remote>,
        links: Vec<Remote>,
        place: Place,
    }

    /// Хранилище в памяти.
    #[derive(Default)]
    struct Mem {
        folders: HashMap<FolderId, Home>,
        jobs: BTreeMap<JobId, JobRow>,
        outbound: BTreeMap<(JobId, LetterId), Outbound>,
        letters: BTreeMap<LetterId, Row>,
        restores: Vec<Vec<LetterId>>,
    }

    const SOURCE: FolderId = FolderId(1);
    const JOB: JobId = JobId(1);

    fn raw(n: u64) -> Vec<u8> {
        format!("Message-ID: <m{n}@x.test>\r\nSubject: {n}\r\n\r\nbody {n}\r\n").into_bytes()
    }

    fn message_id(raw: &[u8]) -> String {
        String::from_utf8_lossy(raw).lines().next().unwrap_or_default().to_owned()
    }

    impl Mem {
        /// Письма `1..=n` из папки учётки `source` перенесены в папку `to` (её владелец — `target`): задание [`JOB`]. С
        /// содержимым — письма из `with_content`.
        fn moved(source: &str, target: Home, n: u64, with_content: impl Fn(u64) -> bool) -> Self {
            let to = FolderId(2);
            let mut m = Mem::default();
            m.folders.insert(SOURCE, Home::Account(source.into()));
            m.folders.insert(to, target);
            for i in 1..=n {
                let src = Remote { account: source.into(), id: format!("m{i}"), data: Vec::new() };
                let row = Row {
                    folder: to,
                    source_folder: SOURCE,
                    content: with_content(i).then(|| raw(i)),
                    sources: vec![src],
                    links: Vec::new(),
                    place: Place::Outbound,
                };
                m.letters.insert(LetterId(i), row);
                m.outbound.insert((JOB, LetterId(i)), Outbound { to, step: Step::Content });
            }
            m
        }

        fn row(&mut self, id: LetterId) -> Result<&mut Row> {
            self.letters.get_mut(&id).ok_or_else(|| crate::model::Error::storage(format!("no letter {}", id.0)))
        }

        fn step(&mut self, job: JobId, id: LetterId, step: Step) -> Result<()> {
            let ob = self.outbound.get_mut(&(job, id)).ok_or_else(|| crate::model::Error::storage("no outbound"))?;
            ob.step = step;
            Ok(())
        }
    }

    impl Homes for Mem {
        fn letter(&self, _: LetterId) -> Home {
            Home::Local
        }

        fn folder(&self, id: FolderId) -> Home {
            self.folders.get(&id).cloned().unwrap_or(Home::Local)
        }
    }

    impl Store for Mem {
        fn jobs(&self) -> Result<Vec<JobRow>> {
            Ok(self.jobs.values().copied().collect())
        }
        fn save_job(&mut self, job: &JobRow) -> Result<()> {
            self.jobs.insert(job.id, *job);
            Ok(())
        }
        fn drop_job(&mut self, job: JobId) -> Result<()> {
            self.jobs.remove(&job);
            Ok(())
        }
        fn outbound(&self, job: JobId) -> Result<Vec<(LetterId, Outbound)>> {
            Ok(self.outbound.range((job, LetterId(0))..=(job, LetterId(u64::MAX))).map(|((_, l), o)| (*l, *o)).collect())
        }
        fn set_step(&mut self, job: JobId, letter: LetterId, step: Step) -> Result<()> {
            self.step(job, letter, step)
        }
        fn info(&self, id: LetterId) -> Result<Option<LetterInfo>> {
            Ok(self.letters.get(&id).map(|r| LetterInfo {
                content: r.content.is_some(),
                size: 100,
                keywords: vec!["$seen".into()],
                received_at: id.0 as i64,
                sources: r.sources.clone(),
            }))
        }
        fn put_content(&mut self, letter: LetterId, raw: &[u8]) -> Result<()> {
            self.row(letter)?.content = Some(raw.to_vec());
            Ok(())
        }
        fn confirm(&mut self, job: JobId, letter: LetterId, remote: &Remote) -> Result<()> {
            let r = self.row(letter)?;
            r.links.push(remote.clone());
            r.place = Place::Synced;
            self.step(job, letter, Step::DeleteSource)
        }
        fn archive(&mut self, job: JobId, letter: LetterId) -> Result<()> {
            let r = self.row(letter)?;
            assert!(r.content.is_some(), "архив — только с содержимым");
            r.place = Place::Local;
            self.step(job, letter, Step::DeleteSource)
        }
        fn refused(&mut self, job: JobId, letter: LetterId, why: &ImportError) -> Result<()> {
            self.row(letter)?.place = Place::Kept(why.to_string());
            self.outbound.remove(&(job, letter));
            Ok(())
        }
        fn finish(&mut self, job: JobId, letter: LetterId) -> Result<()> {
            self.row(letter)?.sources.clear();
            self.outbound.remove(&(job, letter));
            Ok(())
        }
        fn kept(&mut self, job: JobId, letter: LetterId) -> Result<()> {
            let r = self.row(letter)?;
            assert!(!r.sources.is_empty(), "пометка — связью источника");
            self.outbound.remove(&(job, letter));
            Ok(())
        }
        fn restore(&mut self, job: JobId, letters: &[LetterId]) -> Result<()> {
            for l in letters {
                let r = self.row(*l)?;
                (r.folder, r.place) = (r.source_folder, Place::Synced);
                self.outbound.remove(&(job, *l));
            }
            self.restores.push(letters.to_vec());
            Ok(())
        }
    }

    /// Подменные серверы: источник (письма `m<n>`) и цель; сбои — по номеру выгрузки.
    #[derive(Default)]
    struct Path {
        source: BTreeSet<String>,
        /// id на сервере цели → Message-ID.
        target: BTreeMap<String, String>,
        imports: u64,
        retries: u64,
        copies: Vec<usize>,
        quota: Option<Usage>,
        quota_asks: u64,
        throttle_at: Option<u64>,
        quota_at: Option<u64>,
        too_large: HashSet<LetterId>,
        /// Выгрузка с этим номером дошла до цели, ответ — нет: сбой клиента.
        crash_at: Option<u64>,
        /// Выгрузка с этим номером дошла до цели, ответ — обрыв связи.
        lost_at: Option<u64>,
        crashed: bool,
        /// Ответы на выгрузку не приходят (пауза посередине).
        hold: bool,
        /// Ответы источника на удаление не приходят.
        hold_destroy: bool,
        held: Vec<Event>,
        /// Столько удалений у источника кончатся отказом.
        destroy_fails: u32,
        /// Писем в командах удаления у источника.
        destroys: u32,
    }

    impl Path {
        fn with_source(n: u64) -> Self {
            Path { source: (1..=n).map(|i| format!("m{i}")).collect(), quota: Some(Usage { used: 0, limit: u64::MAX }), ..Path::default() }
        }

        fn import(&mut self, job: JobId, letter: LetterId, account: &str, raw: &[u8], retry: bool) -> Option<Event> {
            let mid = message_id(raw);
            let remote = |id: &String| Remote { account: account.into(), id: id.clone(), data: Vec::new() };
            if retry {
                self.retries += 1;
                if let Some((id, _)) = self.target.iter().find(|(_, m)| **m == mid) {
                    return Some(Event::Imported { job, letter, remote: remote(id) });
                }
            }
            self.imports += 1;
            let n = self.imports;
            let error = if self.too_large.contains(&letter) {
                Some(ImportError::TooLarge)
            } else if self.throttle_at.take_if(|t| *t == n).is_some() {
                Some(ImportError::Throttled { retry_after: 5 })
            } else if self.quota_at.take_if(|t| *t == n).is_some() {
                Some(ImportError::Quota)
            } else {
                None
            };
            if let Some(error) = error {
                return Some(Event::ImportFailed { job, letter, error });
            }
            let id = format!("t{n}");
            self.target.insert(id.clone(), mid);
            if self.crash_at == Some(n) {
                self.crashed = true;
                return None;
            }
            if self.lost_at.take_if(|t| *t == n).is_some() {
                return Some(Event::ImportFailed { job, letter, error: ImportError::Throttled { retry_after: RETRY.as_secs() } });
            }
            Some(Event::Imported { job, letter, remote: remote(&id) })
        }

        /// Команда пути → его ответы. `target` — учётка цели; проверки договора — здесь же.
        fn handle(&mut self, route: Route, cmd: Command, store: &Mem, target: &str) -> Vec<Event> {
            match cmd {
                Command::FetchRaw { letter } => {
                    let src = &store.letters[&letter].sources[0];
                    assert_eq!(route, Home::Account(src.account.clone()).route(), "письмо — у источника");
                    let data = if self.source.contains(&src.id) { Ok(Arc::new(raw(letter.0))) } else { Err("gone".into()) };
                    vec![Event::Raw { letter, data }]
                }
                Command::Import { job, letter, folder, keywords, received_at, retry } => {
                    assert_eq!(route, store.folder(folder).route(), "выгрузка — в поток цели");
                    assert_eq!((keywords, received_at), (vec!["$seen".to_string()], letter.0 as i64));
                    let raw = store.letters[&letter].content.clone().expect("выгрузка — только с содержимым");
                    let e = self.import(job, letter, target, &raw, retry);
                    if self.hold {
                        self.held.extend(e);
                        return Vec::new();
                    }
                    e.into_iter().collect()
                }
                Command::ServerCopy { job, letters, .. } => {
                    self.copies.push(letters.len());
                    letters
                        .into_iter()
                        .map(|letter| {
                            let src = store.letters[&letter].sources[0].id.clone();
                            assert!(self.source.remove(&src), "копия с удалением у источника");
                            self.imports += 1;
                            let id = format!("t{}", self.imports);
                            self.target.insert(id.clone(), message_id(&raw(letter.0)));
                            Event::Imported { job, letter, remote: Remote { account: target.into(), id, data: Vec::new() } }
                        })
                        .collect()
                }
                Command::DestroyLetters { job, account, letters } => {
                    let mut out = Vec::new();
                    for letter in letters {
                        let row = &store.letters[&letter];
                        let src = row.sources.iter().find(|s| s.account == account).expect("письмо источника этой учётки");
                        assert_eq!(route, Home::Account(account.clone()).route(), "удаление — в поток источника");
                        match &row.place {
                            Place::Synced => {
                                assert!(row.links.iter().any(|l| l.account == target), "удаление у источника — после подтверждения цели")
                            }
                            Place::Local => assert!(row.content.is_some(), "локальный архив — с содержимым"),
                            p => panic!("удаление у источника письма {p:?}"),
                        }
                        self.destroys += 1;
                        if self.destroy_fails > 0 {
                            self.destroy_fails -= 1;
                            out.push(Event::DestroyFailed { job, letter, error: "busy".into() });
                            continue;
                        }
                        // Повтор после сбоя — письма у источника уже нет: путь отвечает «удалено», не ошибкой.
                        self.source.remove(&src.id);
                        out.push(Event::Destroyed { job, letter });
                    }
                    if self.hold_destroy {
                        self.held.extend(out);
                        return Vec::new();
                    }
                    out
                }
                Command::Settings(settings::Request::Quota) => {
                    self.quota_asks += 1;
                    Vec::new()
                }
                _ => panic!("неожиданная команда"),
            }
        }
    }

    /// Мир теста: движок, серверы, время, ход заданий.
    struct World {
        engine: Engine<Mem>,
        path: Path,
        now: Instant,
        target: String,
        progress: Vec<(Instant, JobId, u64, u64, MigrationState)>,
        /// Итоги `Event::SourceKept`.
        kept: Vec<(JobId, u64)>,
    }

    impl World {
        fn new(store: Mem, path: Path, target: &str) -> Self {
            let mut engine = Engine::open(store).unwrap();
            engine.moved(Some(JOB)).unwrap();
            let mut w = World { engine, path, now: Instant::now(), target: target.into(), progress: Vec::new(), kept: Vec::new() };
            w.features(Features::ALL.with_import(true));
            w
        }

        fn features(&mut self, f: Features) {
            let p = profile_of(&self.target);
            self.engine.features(p.as_deref(), f);
        }

        /// Работать, пока есть что делать (со сдвигом времени к сроку движка, не дальше минуты) или до сбоя.
        fn run(&mut self) {
            self.run_for(Duration::from_secs(60));
        }

        /// [`World::run`] со сдвигом времени не дальше `limit`.
        fn run_for(&mut self, limit: Duration) {
            let until = self.now + limit;
            let mut idle = 0;
            for _ in 0..100_000 {
                let out = self.engine.poll(self.now).unwrap();
                for e in out.events {
                    if let Event::SourceKept { job, letters } = e {
                        self.kept.push((job, letters));
                        continue;
                    }
                    let Event::Migration { job, done, total, state, .. } = e else { panic!("неожиданное событие") };
                    if let Some(prev) = self.progress.iter().rev().find(|p| p.1 == job)
                        && prev.4 == state
                    {
                        assert!(self.now >= prev.0 + PROGRESS_EVERY, "ход — не чаще 4 раз в секунду");
                    }
                    self.progress.push((self.now, job, done, total, state));
                }
                if out.commands.is_empty() {
                    match self.engine.wake_at() {
                        Some(t) if t <= self.now => {
                            idle += 1;
                            assert!(idle < 3, "движок будит без дела");
                        }
                        Some(t) if t <= until => (self.now, idle) = (t, 0),
                        _ => return,
                    }
                    continue;
                }
                idle = 0;
                for (route, cmd) in out.commands {
                    let quota = matches!(cmd, Command::Settings(_));
                    let profile = match &route {
                        Route::Sync(Target::Profile(p)) => p.clone(),
                        _ => None,
                    };
                    for e in self.path.handle(route, cmd, self.engine.store(), &self.target) {
                        self.engine.event(&e, self.now).unwrap();
                    }
                    if quota {
                        self.engine.quota(profile.as_deref(), self.path.quota).unwrap();
                    }
                    if self.path.crashed {
                        return;
                    }
                }
            }
            panic!("движок не остановился");
        }

        fn request(&mut self, r: MigrationRequest) {
            self.engine.request(r).unwrap();
        }

        fn last(&self) -> (u64, u64, MigrationState) {
            self.progress.last().map(|p| (p.2, p.3, p.4)).unwrap()
        }

        fn store(&self) -> &Mem {
            self.engine.store()
        }
    }

    use std::sync::Arc;

    /// 1 000 писем в другую учётку: вопрос — содержимое уже загружается; ответ — выгрузка; сбой на 400-м (цель
    /// приняла, ответа нет) и перезапуск — без дублей и потерь; квота — до старта и каждые 500; удаление у источника
    /// — только после подтверждения.
    #[test]
    fn upload_1000_survives_a_crash() {
        let mut w = World::new(Mem::moved("", Home::Account("@2".into()), 1000, |_| false), Path::with_source(1000), "@2");
        w.run();
        assert_eq!(w.last(), (0, 1000, MigrationState::Asking));
        assert!(w.store().letters.values().all(|r| r.content.is_some()), "содержимое — до ответа");
        assert!(w.path.target.is_empty() && w.path.source.len() == 1000);

        w.path.crash_at = Some(400);
        w.request(MigrationRequest::Answer { job: JOB, mode: MigrationMode::Upload });
        w.run();
        assert!(w.path.crashed);
        assert_eq!(w.path.target.len(), 400);
        let store = std::mem::replace(&mut w.engine, Engine::open(Mem::default()).unwrap()).into_store();
        assert!(store.outbound.values().any(|o| o.step == Step::Confirm), "выгрузка без ответа помнится");

        // Перезапуск: задание — из хранилища, с того же шага.
        w.engine = Engine::open(store).unwrap();
        w.progress.clear();
        w.features(Features::ALL.with_import(true));
        w.path.crashed = false;
        w.run();
        assert_eq!(w.last(), (1000, 1000, MigrationState::Done));
        let mids: HashSet<&String> = w.path.target.values().collect();
        assert_eq!((w.path.target.len(), mids.len()), (1000, 1000), "без дублей");
        assert!(w.path.retries >= 1, "повтор — с проверкой по Message-ID");
        assert!(w.path.source.is_empty());
        let s = w.store();
        assert!(s.letters.values().all(|r| r.place == Place::Synced && r.links.len() == 1 && r.sources.is_empty()));
        assert!(s.outbound.is_empty() && s.jobs.is_empty());
        // До старта, после перезапуска и через 500 писем.
        assert_eq!(w.path.quota_asks, 3);
    }

    /// Посередине: сервер просит подождать — пауза очереди; письмо больше предела — локальным с пометкой, у источника
    /// остаётся, очередь идёт; нет места — «нет места», «Продолжить» — до конца.
    #[test]
    fn throttled_too_large_and_quota_midway() {
        let mut path = Path::with_source(20);
        path.throttle_at = Some(3);
        path.too_large.insert(LetterId(5));
        path.quota_at = Some(10);
        let mut w = World::new(Mem::moved("", Home::Account("@2".into()), 20, |_| true), path, "@2");
        w.request(MigrationRequest::Answer { job: JOB, mode: MigrationMode::Upload });
        let start = w.now;
        w.run();
        assert!(w.now >= start + Duration::from_secs(5), "пауза по просьбе сервера");
        assert_eq!(w.last().2, MigrationState::QuotaFull);
        assert!(w.path.target.len() < 19);

        w.request(MigrationRequest::Resume { job: JOB });
        w.run();
        assert_eq!(w.last(), (20, 20, MigrationState::Done));
        assert_eq!(w.path.target.len(), 19);
        assert_eq!(w.path.source.iter().collect::<Vec<_>>(), ["m5"], "слишком большое — у источника");
        let big = &w.store().letters[&LetterId(5)];
        assert_eq!(big.place, Place::Kept(ImportError::TooLarge.to_string()));
        assert!(big.content.is_some() && big.links.is_empty());
    }

    /// Обрыв связи после выгрузки (письмо у цели, ответа нет): повтор — с проверкой по `Message-ID`, без дубля.
    #[test]
    fn lost_answer_is_not_a_duplicate() {
        let mut path = Path::with_source(5);
        path.lost_at = Some(2);
        let mut w = World::new(Mem::moved("", Home::Account("@2".into()), 5, |_| true), path, "@2");
        w.features(Features::ALL.with_import(true).with(Feature::Quota, false));
        w.request(MigrationRequest::Answer { job: JOB, mode: MigrationMode::Upload });
        w.run();
        assert_eq!(w.last(), (5, 5, MigrationState::Done));
        let mids: HashSet<&String> = w.path.target.values().collect();
        assert_eq!((w.path.target.len(), mids.len(), w.path.retries), (5, 5, 1));
        assert!(w.path.source.is_empty());
    }

    /// Пауза посередине: выгрузки в пути дойдут, новых нет; «Продолжить» — до конца.
    #[test]
    fn pause_and_resume() {
        let mut w = World::new(Mem::moved("", Home::Account("@2".into()), 10, |_| true), Path::with_source(10), "@2");
        w.features(Features::ALL.with_import(true).with(Feature::Quota, false));
        w.request(MigrationRequest::Answer { job: JOB, mode: MigrationMode::Upload });
        w.path.hold = true;
        w.run();
        let held = std::mem::take(&mut w.path.held);
        assert_eq!(held.len(), Limits::DEFAULT.imports, "не больше двух выгрузок сразу");
        w.request(MigrationRequest::Pause { job: JOB });
        w.path.hold = false;
        for e in &held {
            w.engine.event(e, w.now).unwrap();
        }
        let out = w.engine.poll(w.now).unwrap();
        assert!(out.commands.iter().all(|(_, c)| matches!(c, Command::DestroyLetters { .. })), "на паузе — только удаление подтверждённых");
        assert!(matches!(out.events[..], [Event::Migration { state: MigrationState::Paused, .. }]));
        w.progress.push((w.now, JOB, 2, 10, MigrationState::Paused));
        for (route, cmd) in out.commands {
            for e in w.path.handle(route, cmd, w.engine.store(), "@2") {
                w.engine.event(&e, w.now).unwrap();
            }
        }
        assert_eq!(w.engine.poll(w.now + Duration::from_secs(1)).unwrap().commands.len(), 0);

        w.request(MigrationRequest::Resume { job: JOB });
        w.run();
        assert_eq!(w.last(), (10, 10, MigrationState::Done));
        assert_eq!(w.path.target.len(), 10);
        assert!(w.path.source.is_empty());
    }

    /// Отмена: «Вернуть на место» — все письма одной транзакцией к источнику; «Оставить локально» — с содержимым —
    /// архив и удаление у источника, без содержимого — к источнику.
    #[test]
    fn cancel_both_ways() {
        for keep_local in [false, true] {
            let mut w = World::new(Mem::moved("", Home::Account("@2".into()), 6, |i| i <= 3), Path::with_source(6), "@2");
            w.request(MigrationRequest::Cancel { job: JOB, keep_local });
            w.run();
            let s = w.store();
            assert!(s.outbound.is_empty() && s.jobs.is_empty());
            assert!(w.path.target.is_empty());
            if keep_local {
                assert_eq!(s.restores, [vec![LetterId(4), LetterId(5), LetterId(6)]]);
                assert!((1..=3).all(|i| s.letters[&LetterId(i)].place == Place::Local));
                assert_eq!(w.path.source.len(), 3);
                assert_eq!(w.last(), (3, 6, MigrationState::Cancelled));
            } else {
                assert_eq!(s.restores, [(1..=6).map(LetterId).collect::<Vec<_>>()], "одной транзакцией");
                assert!(s.letters.values().all(|r| r.place == Place::Synced && r.folder == SOURCE));
                assert_eq!(w.path.source.len(), 6);
                assert_eq!(w.last(), (0, 6, MigrationState::Cancelled));
            }
        }
    }

    /// Отмена, когда выгрузка в пути: принятое целью доводится (удаление у источника), прочее — к источнику.
    #[test]
    fn cancel_with_upload_in_flight() {
        let mut w = World::new(Mem::moved("", Home::Account("@2".into()), 5, |_| true), Path::with_source(5), "@2");
        w.features(Features::ALL.with_import(true).with(Feature::Quota, false));
        w.request(MigrationRequest::Answer { job: JOB, mode: MigrationMode::Upload });
        w.path.hold = true;
        w.run();
        w.request(MigrationRequest::Cancel { job: JOB, keep_local: false });
        w.path.hold = false;
        for e in std::mem::take(&mut w.path.held) {
            w.engine.event(&e, w.now).unwrap();
        }
        w.run();
        let s = w.store();
        assert_eq!(s.restores, [vec![LetterId(3), LetterId(4), LetterId(5)]]);
        assert_eq!(w.path.target.len(), 2);
        assert_eq!(w.path.source.len(), 3);
        assert!(s.outbound.is_empty() && s.jobs.is_empty());
        assert_eq!(w.last(), (2, 5, MigrationState::Cancelled));
    }

    /// Отмена, затем сбой до ответа на выгрузку: после перезапуска задание остаётся отменённым, письма без
    /// подтверждения — к источнику (с «оставить локально» — архив).
    #[test]
    fn cancel_survives_a_restart() {
        for keep_local in [false, true] {
            let mut w = World::new(Mem::moved("", Home::Account("@2".into()), 4, |_| true), Path::with_source(4), "@2");
            w.features(Features::ALL.with_import(true).with(Feature::Quota, false));
            w.request(MigrationRequest::Answer { job: JOB, mode: MigrationMode::Upload });
            w.path.hold = true;
            w.run();
            w.request(MigrationRequest::Cancel { job: JOB, keep_local });
            let store = std::mem::replace(&mut w.engine, Engine::open(Mem::default()).unwrap()).into_store();
            w.engine = Engine::open(store).unwrap();
            w.path.hold = false;
            w.progress.clear();
            w.run();
            let s = w.store();
            assert!(s.outbound.is_empty() && s.jobs.is_empty());
            assert_eq!(w.last().2, MigrationState::Cancelled);
            let local = s.letters.values().filter(|r| r.place == Place::Local).count();
            assert_eq!(local, if keep_local { 4 } else { 0 });
            assert_eq!(w.path.source.len(), if keep_local { 0 } else { 4 });
        }
    }

    /// В «На этом компьютере» — локальный архив без вопроса: содержимое (нет — у источника), затем удаление у
    /// источника; источник письмо потерял — повтор с паузой.
    #[test]
    fn to_this_computer_is_a_local_archive() {
        let mut path = Path::with_source(4);
        path.source.remove("m4");
        let mut w = World::new(Mem::moved("@2", Home::Local, 4, |i| i == 1), path, "");
        w.run();
        assert!(w.progress.iter().all(|p| p.4 != MigrationState::Asking), "без вопроса");
        assert!(w.path.source.is_empty() && w.path.target.is_empty());
        let s = w.store();
        assert!((1..=3).all(|i| s.letters[&LetterId(i)].place == Place::Local));
        assert_eq!(w.last(), (3, 4, MigrationState::Running), "письмо без содержимого ждёт повтора");
        // Источник отдал письмо — задание кончилось.
        w.path.source.insert("m4".into());
        w.run();
        assert_eq!(w.last(), (4, 4, MigrationState::Done));
        assert!(w.path.source.is_empty());
    }

    /// Между аккаунтами одной учётной записи (общий ящик) — `ServerCopy` пачками по пределу пути, источник удаляет
    /// сервер.
    #[test]
    fn server_copy_in_batches() {
        let mut w = World::new(Mem::moved("", Home::Account("shared".into()), 7, |_| true), Path::with_source(7), "shared");
        w.features(Features::ALL.with_import(true).with_server_copy(true).with(Feature::Quota, false));
        w.engine.limits(None, Limits { batch: 3, imports: 2 });
        w.request(MigrationRequest::Answer { job: JOB, mode: MigrationMode::Upload });
        w.run();
        assert_eq!(w.path.copies, [3, 3, 1]);
        assert_eq!(w.path.target.len(), 7);
        assert!(w.path.source.is_empty());
        assert_eq!(w.last(), (7, 7, MigrationState::Done));
    }

    /// Удаление у источника — только по ответу его сервера (решение владельца 2026-10-29): без ответа письмо в задании
    /// и после перезапуска (удаление повторится); отказ — повтор после паузы; задание кончается по последнему ответу.
    #[test]
    fn source_delete_waits_for_the_server() {
        let mut w = World::new(Mem::moved("", Home::Account("@2".into()), 3, |_| true), Path::with_source(3), "@2");
        w.features(Features::ALL.with_import(true).with(Feature::Quota, false));
        w.request(MigrationRequest::Answer { job: JOB, mode: MigrationMode::Upload });
        w.path.hold_destroy = true;
        w.run();
        assert_eq!(w.path.target.len(), 3);
        assert!(w.path.source.is_empty(), "сервер источника удалил");
        assert_eq!(w.last().0, 0, "ответа нет — не готово");
        assert!(w.store().letters.values().all(|r| r.sources.len() == 1), "связи источника — до ответа");
        assert!(w.store().outbound.values().all(|o| o.step == Step::DeleteSource));

        // Перезапуск без ответов: удаление — снова, ответ «удалено» (письма у источника уже нет).
        w.path.held.clear();
        let store = std::mem::replace(&mut w.engine, Engine::open(Mem::default()).unwrap()).into_store();
        w.engine = Engine::open(store).unwrap();
        w.progress.clear();
        w.path.hold_destroy = false;
        w.path.destroy_fails = 1;
        let start = w.now;
        w.run();
        assert!(w.now >= start + RETRY, "отказ — повтор после паузы");
        assert_eq!(w.last(), (3, 3, MigrationState::Done));
        let s = w.store();
        assert!(s.letters.values().all(|r| r.sources.is_empty() && r.place == Place::Synced));
        assert!(s.outbound.is_empty() && s.jobs.is_empty());
    }

    /// Удаление у источника — не больше [`DESTROY_ATTEMPTS`] попыток с паузой между ними (решение владельца
    /// 2026-10-30): 9 отказов и успех — письмо удалено, пометки нет; 10 отказов — письмо у цели с пометкой «не удалено
    /// у источника» (его связь), задание закрыто, итог — `Event::SourceKept` с числом писем.
    #[test]
    fn source_delete_gives_up_after_ten_attempts() {
        for fails in [DESTROY_ATTEMPTS - 1, DESTROY_ATTEMPTS] {
            let mut path = Path::with_source(1);
            path.destroy_fails = fails;
            let mut w = World::new(Mem::moved("", Home::Account("@2".into()), 1, |_| true), path, "@2");
            w.features(Features::ALL.with_import(true).with(Feature::Quota, false));
            w.request(MigrationRequest::Answer { job: JOB, mode: MigrationMode::Upload });
            let start = w.now;
            w.run_for(RETRY * (DESTROY_ATTEMPTS + 1));
            assert_eq!(w.path.destroys, DESTROY_ATTEMPTS.min(fails + 1), "попыток — не больше {DESTROY_ATTEMPTS}");
            assert!(w.now >= start + RETRY * (DESTROY_ATTEMPTS - 1), "между попытками — пауза");
            assert_eq!(w.last(), (1, 1, MigrationState::Done), "задание закрыто, не висит");
            assert!(w.engine.wake_at().is_none());
            let s = w.store();
            assert!(s.outbound.is_empty() && s.jobs.is_empty());
            let row = &s.letters[&LetterId(1)];
            assert_eq!((row.place.clone(), row.links.len()), (Place::Synced, 1), "письмо у цели");
            if fails < DESTROY_ATTEMPTS {
                assert!(row.sources.is_empty() && !w.path.source.contains("m1"));
                assert!(w.kept.is_empty());
            } else {
                assert_eq!(row.sources.len(), 1, "пометка «не удалено у источника» — его связь");
                assert!(w.path.source.contains("m1"));
                assert_eq!(w.kept, [(JOB, 1)]);
            }
        }
    }
}
