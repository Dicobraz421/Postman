//! Перенос писем между учётками (S8-16, ADR 0006 п. 3–5, макет S8-D «Перенос»): поток ядра с движком переноса,
//! вопрос после переноса, ход, пауза, «нет места», остановка и итог.
//!
//! Поток ядра ([`Core`]) держит `migrate::Engine` на кэше экрана: `MoveLetters` — перенос в базе одной транзакцией,
//! `Migration` — ответ, пауза, продолжение, отмена; ответы путей (`Raw`, `Imported`, `Destroyed`…, умения, квота)
//! — копией из синхронизации. Команды движка путям и его события возвращаются экрану: команды — в синхронизацию
//! учётки по маршруту, события — сюда.
//!
//! Без модальных окон (DESIGN «Действия и состояния»): вопрос — нейтральной полосой над списком папки цели, пока
//! она открыта; ход — в строке состояния, как квота, щелчок — полоса задания; «нет места» показывается сама;
//! «Остановить» — строкой подтверждения (очередь на паузе до ответа); конец — тост.

use std::collections::BTreeMap;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::{Metric, Role};
use postman_cache::Cache;
use postman_core::migrate::Engine;
use postman_core::model::{JobId, MigrationMode, MigrationRequest, MigrationState};
use postman_core::store::{CacheStore, move_letters, worker_splitter};
use postman_core::sync::{Command, Event, Route, Target};

use crate::Mailer;
use crate::chrome::{glyph, icon};
use crate::roles::Look;

/// Значки полосы задания (Segoe Fluent Icons): пауза.
const PAUSE: char = '\u{E769}';

/// Ответа воркера на разбор письма ждать не дольше.
const WORKER_TIMEOUT: Duration = Duration::from_secs(20);
/// Без заданий и событий поток ядра спит до следующей вести.
const IDLE: Duration = Duration::from_secs(3600);

// --- поток ядра ---

enum In {
    Command(Command),
    /// Событие синхронизации учётной записи профиля (`None` — основной).
    Event(Option<String>, Event),
}

/// Что поток ядра отдаёт экрану.
pub(crate) enum Out {
    /// Команды движка путям: маршрут — синхронизация учётки.
    Commands(Vec<(Route, Command)>),
    /// Ход заданий (`Migration`, `SourceKept`); `moved` — перенос в базе прошёл: списки и дерево заново.
    Events { events: Vec<Event>, moved: bool },
}

/// Поток ядра (ADR 0006 п. 3–4): перенос в базе и задания выгрузки. Закрыт канал — поток кончается.
pub(crate) struct Core {
    tx: mpsc::Sender<In>,
}

impl Core {
    /// Поток на кэше `cache`; `out` — весть экрану (из потока ядра).
    pub(crate) fn spawn(cache: Cache, out: impl Fn(Out) + Send + 'static) -> Option<Core> {
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new().name("core".into()).spawn(move || run(cache, rx, out));
        match spawned {
            Ok(_) => Some(Core { tx }),
            Err(e) => {
                tracing::warn!("core thread: {e}");
                None
            }
        }
    }

    /// Команда ядру (`MoveLetters`, `Migration`); `false` — поток кончился.
    pub(crate) fn send(&self, c: Command) -> bool {
        self.tx.send(In::Command(c)).is_ok()
    }

    /// Копия события синхронизации, которое ждёт движок.
    pub(crate) fn event(&self, profile: Option<&str>, e: &Event) {
        if let Some(e) = wanted(e) {
            let _ = self.tx.send(In::Event(profile.map(str::to_owned), e));
        }
    }
}

/// Копия события пути, нужного движку переноса (`Event` не копируется целиком: в нём бывают тела писем); прочие — `None`.
fn wanted(e: &Event) -> Option<Event> {
    use postman_core::settings::Reply;
    Some(match e {
        Event::Raw { letter, data } => Event::Raw { letter: *letter, data: data.clone() },
        Event::Imported { job, letter, remote } => Event::Imported { job: *job, letter: *letter, remote: remote.clone() },
        Event::ImportFailed { job, letter, error } => Event::ImportFailed { job: *job, letter: *letter, error: error.clone() },
        Event::Destroyed { job, letter } => Event::Destroyed { job: *job, letter: *letter },
        Event::DestroyFailed { job, letter, error } => Event::DestroyFailed { job: *job, letter: *letter, error: error.clone() },
        Event::Features(f) => Event::Features(*f),
        Event::Settings(Reply::Quota(u)) => Event::Settings(Reply::Quota(*u)),
        _ => return None,
    })
}

fn run(cache: Cache, rx: mpsc::Receiver<In>, out: impl Fn(Out)) {
    let client = postman_worker::Client::new(postman_worker::Client::default_exe(), WORKER_TIMEOUT);
    let mut engine = match Engine::open(CacheStore::new(cache, worker_splitter(client))) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("migration engine: {e}");
            return;
        }
    };
    loop {
        let wait = engine.wake_at().map_or(IDLE, |t| t.saturating_duration_since(Instant::now()));
        let mut moved = false;
        match rx.recv_timeout(wait) {
            Ok(msg) => moved |= handle(&mut engine, msg),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        while let Ok(msg) = rx.try_recv() {
            moved |= handle(&mut engine, msg);
        }
        pump(&mut engine, moved, &out);
    }
}

/// Весть потоку ядра; `true` — перенос в базе прошёл.
fn handle(engine: &mut Engine<CacheStore>, msg: In) -> bool {
    let now = Instant::now();
    let r = match msg {
        In::Command(Command::MoveLetters { ids, to }) => match move_letters(engine, &ids, to) {
            Ok(t) => {
                tracing::info!(moved = t.moved, outbound = t.outbound, "letters moved");
                return true;
            }
            Err(e) => Err(e),
        },
        In::Command(Command::Migration(r)) => engine.request(r),
        In::Command(c) => {
            tracing::warn!("core thread: unexpected command ({} routes)", c.routes().len());
            Ok(())
        }
        In::Event(profile, Event::Features(f)) => {
            engine.features(profile.as_deref(), f);
            Ok(())
        }
        In::Event(profile, Event::Settings(postman_core::settings::Reply::Quota(u))) => engine.quota(profile.as_deref(), u),
        In::Event(_, e) => engine.event(&e, now),
    };
    if let Err(e) = r {
        tracing::warn!("migration: {e}");
    }
    false
}

/// Шаги движка: письмо «На этом компьютере» целиком — здесь же (без сети), прочие команды и события — экрану.
fn pump(engine: &mut Engine<CacheStore>, moved: bool, out: &impl Fn(Out)) {
    let mut events = Vec::new();
    let mut commands = Vec::new();
    // Ответы, которые даёт сама база, — сразу движку; кругов не больше, чем писем в пачке.
    for _ in 0..64 {
        let o = match engine.poll(Instant::now()) {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!("migration poll: {e}");
                break;
            }
        };
        events.extend(o.events);
        let mut local = false;
        for (route, c) in o.commands {
            match (route, c) {
                (Route::Core, Command::FetchRaw { letter }) => {
                    let e = postman_core::store::local_raw(engine.store().cache(), letter);
                    if let Err(e) = engine.event(&e, Instant::now()) {
                        tracing::warn!("migration: {e}");
                    }
                    local = true;
                }
                (Route::Core, _) => tracing::warn!("core thread: unexpected engine command"),
                (route, c) => commands.push((route, c)),
            }
        }
        if !local {
            break;
        }
    }
    if !commands.is_empty() {
        out(Out::Commands(commands));
    }
    if moved || !events.is_empty() {
        out(Out::Events { events, moved });
    }
}

// --- задания на экране ---

/// Задание переноса на экране: из базы при запуске и из `Event::Migration`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct JobView {
    pub(crate) id: JobId,
    pub(crate) state: MigrationState,
    pub(crate) mode: MigrationMode,
    pub(crate) done: u64,
    pub(crate) total: u64,
    pub(crate) bytes: u64,
    /// Всего байт задания (выгружено и осталось).
    pub(crate) total_bytes: u64,
    /// Папка цели и папки источника — ключи дерева.
    pub(crate) to: String,
    pub(crate) from: Vec<String>,
    /// Когда задание начало выгрузку на экране (оценка «осталось около»): число готовых тогда.
    pub(crate) since: Option<(Instant, u64)>,
}

impl JobView {
    /// Учётка цели (ключ; основная — пустая строка).
    pub(crate) fn target(&self) -> &str {
        crate::local::home(&self.to)
    }

    /// Учётка источника — первой папки.
    pub(crate) fn source(&self) -> &str {
        self.from.first().map_or("", |f| crate::local::home(f))
    }

    pub(crate) fn left(&self) -> u64 {
        self.total.saturating_sub(self.done)
    }

    /// Осталось около: по скорости с начала выгрузки на экране; мало данных — `None`.
    pub(crate) fn eta(&self, now: Instant) -> Option<Duration> {
        let (t0, d0) = self.since?;
        let done = self.done.checked_sub(d0).filter(|d| *d > 0)?;
        let spent = now.saturating_duration_since(t0);
        (spent >= Duration::from_secs(5)).then(|| spent.mul_f64(self.left() as f64 / done as f64))
    }
}

/// Задание из базы: строка задания и строки его писем (`outbound`) — папки, байты; `None` — задания нет.
pub(crate) fn job_view(cache: &Cache, id: JobId) -> Option<JobView> {
    let row = cache.job(postman_cache::JobId(id.0)).ok().flatten()?;
    let letters = cache.outbound(postman_cache::JobId(id.0)).ok()?;
    let key = |f: postman_cache::FolderId| cache.folder(f).ok().flatten().and_then(|r| crate::local::folder_key(f, &r));
    let to = letters.first().and_then(|(_, o)| key(o.to))?;
    let mut from: Vec<String> = Vec::new();
    let mut left_bytes = 0u64;
    for (id, o) in &letters {
        for f in &o.from {
            if let Some(k) = key(*f).filter(|k| !from.contains(k)) {
                from.push(k);
            }
        }
        left_bytes += cache.letter(*id).ok().flatten().map_or(0, |l| l.size);
    }
    Some(JobView {
        id,
        state: state_of(row.state),
        mode: mode_of(row.mode),
        done: row.total.saturating_sub(letters.len() as u64),
        total: row.total,
        bytes: row.bytes,
        total_bytes: row.bytes + left_bytes,
        to,
        from,
        since: None,
    })
}

fn state_of(s: postman_cache::MigrationState) -> MigrationState {
    use postman_cache::MigrationState as S;
    match s {
        S::Asking => MigrationState::Asking,
        S::Running => MigrationState::Running,
        S::Paused => MigrationState::Paused,
        S::QuotaFull => MigrationState::QuotaFull,
        S::Done => MigrationState::Done,
        S::Cancelled => MigrationState::Cancelled,
    }
}

fn mode_of(m: postman_cache::MigrationMode) -> MigrationMode {
    use postman_cache::MigrationMode as M;
    match m {
        M::Ask => MigrationMode::Ask,
        M::Upload => MigrationMode::Upload,
        M::LocalArchive => MigrationMode::LocalArchive,
    }
}

/// Перенос на экране: задания, открытая полоса задания, подтверждение остановки, обновление хранилища.
#[derive(Default)]
pub(crate) struct Migrations {
    pub(crate) core: Option<Core>,
    pub(crate) jobs: BTreeMap<JobId, JobView>,
    /// Полоса задания открыта щелчком по строке состояния.
    pub(crate) open: bool,
    /// «Остановить» нажато: строка подтверждения этого задания (очередь на паузе).
    pub(crate) stopping: Option<JobId>,
    /// Обновление хранилища (формат 5 → 6): проценты — строкой вместо списка.
    pub(crate) upgrade: Option<u64>,
}

/// Что показать полосой над списком.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Bar {
    /// Вопрос после переноса в открытую папку: задания с вопросом (первое — главное).
    Ask(Vec<JobId>),
    Running(JobId),
    Paused(JobId),
    NoSpace(JobId),
    Stop(JobId),
}

impl Migrations {
    /// Полоса над списком при открытой папке `open`: остановка, «нет места» (сама), полоса задания (по щелчку),
    /// вопрос — пока открыта папка цели.
    pub(crate) fn bar(&self, open: Option<&str>) -> Option<Bar> {
        if let Some(id) = self.stopping.filter(|id| self.jobs.contains_key(id)) {
            return Some(Bar::Stop(id));
        }
        if let Some(j) = self.jobs.values().find(|j| j.state == MigrationState::QuotaFull) {
            return Some(Bar::NoSpace(j.id));
        }
        if self.open
            && let Some(j) = self.active()
        {
            return Some(if j.state == MigrationState::Paused { Bar::Paused(j.id) } else { Bar::Running(j.id) });
        }
        let asking: Vec<JobId> =
            self.jobs.values().filter(|j| j.state == MigrationState::Asking && Some(j.to.as_str()) == open).map(|j| j.id).collect();
        (!asking.is_empty()).then_some(Bar::Ask(asking))
    }

    /// Задание в строке состояния: идёт, на паузе или без места.
    pub(crate) fn active(&self) -> Option<&JobView> {
        let shown = |s: MigrationState| matches!(s, MigrationState::Running | MigrationState::Paused | MigrationState::QuotaFull);
        self.jobs.values().find(|j| j.state == MigrationState::Running).or_else(|| self.jobs.values().find(|j| shown(j.state)))
    }

    /// Ход задания: новое состояние и числа; задание кончилось — убрать и вернуть, чем кончилось (для тоста).
    pub(crate) fn progress(&mut self, id: JobId, done: u64, total: u64, bytes: u64, state: MigrationState, now: Instant) -> Option<JobView> {
        let j = self.jobs.get_mut(&id)?;
        if state == MigrationState::Running && (j.state != MigrationState::Running || j.since.is_none()) {
            j.since = Some((now, done));
        }
        j.state = state;
        j.done = done;
        j.total = total;
        j.bytes = bytes;
        if matches!(state, MigrationState::Done | MigrationState::Cancelled) {
            if self.stopping == Some(id) {
                self.stopping = None;
            }
            let j = self.jobs.remove(&id);
            if self.active().is_none() {
                self.open = false;
            }
            return j;
        }
        None
    }
}

/// Размер: «84 КБ», «1,2 МБ», «4,8 ГБ».
pub(crate) fn size_text(bytes: u64) -> String {
    let gb = bytes as f64 / (1024. * 1024. * 1024.);
    if gb >= 1. { tr!("migration-size-gb", n = format!("{gb:.1}").replace('.', ",")) } else { crate::bulk::size_text(bytes) }
}

/// Число с пробелами между тысячами: «12 480».
pub(crate) fn count_text(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push('\u{a0}');
        }
        out.push(c);
    }
    out
}

/// «около 20 мин», «около 1 ч 5 мин».
fn eta_text(d: Duration) -> String {
    let min = d.as_secs().div_ceil(60).max(1);
    if min < 60 { tr!("migration-eta-min", n = min as i64) } else { tr!("migration-eta-hours", h = (min / 60) as i64, m = (min % 60) as i64) }
}

/// Цель переноса — путём сервера Exchange: метки Postman там не сохранятся (вопрос №9 S8).
fn exchange(m: &Mailer, account: &str) -> bool {
    if account.is_empty() {
        return m.live_account().is_some_and(|s| s.url.to_ascii_lowercase().contains("/ews"));
    }
    let profile = postman_cache::profile_of(Some(account)).unwrap_or(account);
    let by_settings = m.live.profile.names.iter().find(|(p, _)| p == profile).is_some_and(|(_, s)| s.url.to_ascii_lowercase().contains("/ews"));
    by_settings || (!m.backend.is_live() && Some(profile) == postman_demo::EXCHANGE.profile)
}

impl Mailer {
    /// Поток ядра на кэше экрана; задания из базы — на экран (вопрос переживает перезапуск).
    pub(crate) fn start_core(&mut self, cx: &mut Context<Self>) {
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<Out>();
        self.migration.core = Core::spawn(self.cache.clone(), move |o| {
            let _ = tx.unbounded_send(o);
        });
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            while let Some(o) = rx.next().await {
                if this.update(cx, |m, cx| m.on_core(o, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        self.load_jobs(cx);
        self.sync_servers(cx);
    }

    /// Задания из базы — заново (запуск, снимки).
    pub(crate) fn load_jobs(&mut self, cx: &mut Context<Self>) {
        let cache = self.cache.clone();
        let task = cx.background_spawn(async move {
            let ids: Vec<JobId> = cache.jobs().unwrap_or_default().into_iter().map(|r| JobId(r.id.0)).collect();
            ids.into_iter().filter_map(|id| job_view(&cache, id)).collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let jobs = task.await;
            let _ = this.update(cx, |m, cx| {
                m.migration.jobs = jobs.into_iter().map(|j| (j.id, j)).collect();
                cx.notify();
            });
        })
        .detach();
    }

    /// Команда ядру: перенос, ответ, пауза, отмена.
    pub(crate) fn core_send(&mut self, c: Command, cx: &mut Context<Self>) {
        let sent = self.migration.core.as_ref().is_some_and(|core| core.send(c));
        if !sent {
            self.toast(pets_ui::Level::Error, tr!("migration-core-stopped"), cx);
        }
    }

    /// Копия событий синхронизации — движку.
    pub(crate) fn core_events(&self, profile: Option<&str>, batch: &[Event]) {
        if let Some(core) = &self.migration.core {
            for e in batch {
                core.event(profile, e);
            }
        }
    }

    fn on_core(&mut self, o: Out, cx: &mut Context<Self>) {
        match o {
            Out::Commands(commands) => {
                for (route, c) in commands {
                    let Route::Sync(target) = route else { continue };
                    let profile = match target {
                        Target::Profile(p) => p,
                        _ => None,
                    };
                    // Демо и остановленная учётка: команда не ушла — задание стоит на этом шаге.
                    let _ = self.backend.send_to(profile.as_deref(), c);
                }
            }
            Out::Events { events, moved } => {
                let now = Instant::now();
                let mut refresh = moved;
                for e in events {
                    match e {
                        Event::Migration { job, done, total, bytes, state } => {
                            refresh = true;
                            if !self.migration.jobs.contains_key(&job) {
                                self.fetch_job(job, cx);
                                continue;
                            }
                            if let Some(end) = self.migration.progress(job, done, total, bytes, state, now) {
                                self.job_ended(&end, cx);
                            }
                        }
                        Event::SourceKept { letters, .. } => {
                            self.toast(pets_ui::Level::Warning, tr!("migration-source-kept", n = letters as i64), cx);
                        }
                        _ => {}
                    }
                }
                if moved {
                    self.load_jobs(cx);
                    self.reload_folders(cx);
                }
                if refresh {
                    self.list.update(cx, |l, cx| l.refresh(cx));
                }
                cx.notify();
            }
        }
    }

    /// Новое задание (событием ядра): из базы — на экран.
    fn fetch_job(&mut self, id: JobId, cx: &mut Context<Self>) {
        let cache = self.cache.clone();
        let task = cx.background_spawn(async move { job_view(&cache, id) });
        cx.spawn(async move |this, cx| {
            if let Some(j) = task.await {
                let _ = this.update(cx, |m, cx| {
                    m.migration.jobs.insert(j.id, j);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Название папки для текстов: «Учётка · Папка».
    pub(crate) fn place_name(&self, key: &str, cx: &App) -> String {
        let f = self.folders.read(cx);
        let name = f.mailboxes.iter().find(|m| m.id == key).map(|m| m.name.clone()).unwrap_or_default();
        format!("{} · {name}", self.account_name(crate::local::home(key), cx))
    }

    /// Тост в конце задания (макет: нейтральный).
    fn job_ended(&mut self, j: &JobView, cx: &mut Context<Self>) {
        let server = self.account_name(j.target(), cx);
        let source = self.account_name(j.source(), cx);
        let (n, count) = (j.done as i64, count_text(j.done));
        let text = match (j.state, j.mode) {
            (MigrationState::Cancelled, MigrationMode::LocalArchive) => {
                format!(
                    "{}\n{}",
                    tr!("migration-done-local", n = n, count = count.clone()),
                    tr!("migration-done-local-detail", to = self.place_name(&j.to, cx), source = source)
                )
            }
            (MigrationState::Cancelled, _) => {
                let from = j.from.first().map(|f| self.place_name(f, cx)).unwrap_or_default();
                tr!("migration-returned", n = j.left() as i64, count = count_text(j.left()), from = from)
            }
            (_, MigrationMode::LocalArchive) => {
                format!(
                    "{}\n{}",
                    tr!("migration-done-local", n = n, count = count.clone()),
                    tr!("migration-done-local-detail", to = self.place_name(&j.to, cx), source = source)
                )
            }
            _ if j.done < j.total => tr!("migration-done-partial", server = server, done = count, total = count_text(j.total)),
            _ => format!(
                "{}\n{}",
                tr!("migration-done", server = server.clone(), n = n, count = count),
                tr!("migration-done-source", source = source, server = server)
            ),
        };
        self.toast(pets_ui::Level::Info, text, cx);
    }

    /// Ответ, пауза, продолжение, отмена задания.
    fn job_request(&mut self, r: MigrationRequest, cx: &mut Context<Self>) {
        self.core_send(Command::Migration(r), cx);
    }

    fn answer(&mut self, jobs: &[JobId], mode: MigrationMode, cx: &mut Context<Self>) {
        for &job in jobs {
            self.job_request(MigrationRequest::Answer { job, mode }, cx);
        }
    }

    /// «Остановить»: очередь на паузе, пока нет ответа строки подтверждения.
    fn ask_stop(&mut self, job: JobId, cx: &mut Context<Self>) {
        self.job_request(MigrationRequest::Pause { job }, cx);
        self.migration.stopping = Some(job);
        cx.notify();
    }

    /// Ответ строки остановки: `None` — «Не останавливать» (продолжить), иначе — отмена с этим вариантом.
    fn stop_answer(&mut self, job: JobId, keep_local: Option<bool>, cx: &mut Context<Self>) {
        self.migration.stopping = None;
        let r = match keep_local {
            Some(keep_local) => MigrationRequest::Cancel { job, keep_local },
            None => MigrationRequest::Resume { job },
        };
        self.job_request(r, cx);
        cx.notify();
    }

    /// «Оставить локально» при «нет места»: остальные — локальным архивом.
    fn keep_local(&mut self, job: JobId, cx: &mut Context<Self>) {
        self.job_request(MigrationRequest::Answer { job, mode: MigrationMode::LocalArchive }, cx);
        self.job_request(MigrationRequest::Resume { job }, cx);
    }

    /// Esc: строка остановки — «Не останавливать», затем полоса задания. `false` — закрывать было нечего.
    pub(crate) fn migration_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if let Some(job) = self.migration.stopping {
            self.stop_answer(job, None, cx);
            return true;
        }
        if self.migration.open {
            self.migration.open = false;
            cx.notify();
            return true;
        }
        false
    }

    /// Полоса над списком (на месте полосы автоответа).
    pub(crate) fn migration_bar(&self, l: &Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.section != crate::Section::Mail {
            return None;
        }
        let open = self.list.read(cx).mailbox.clone();
        let bar = self.migration.bar(open.as_deref())?;
        let job = |id: &JobId| self.migration.jobs.get(id);
        Some(match bar {
            Bar::Ask(ids) => {
                let j = job(ids.first()?)?;
                let server = self.account_name(j.target(), cx);
                let from: Vec<String> = ids.iter().filter_map(job).flat_map(|j| j.from.iter().cloned()).collect();
                let mut seen = Vec::new();
                for f in from {
                    if !seen.contains(&f) {
                        seen.push(f);
                    }
                }
                let n: u64 = ids.iter().filter_map(job).map(|j| j.total).sum();
                let first = seen.first().map(|f| self.place_name(f, cx)).unwrap_or_default();
                let text = if seen.len() > 1 {
                    tr!("migration-ask-more", n = n as i64, count = count_text(n), from = first, more = (seen.len() - 1) as i64)
                } else {
                    tr!("migration-ask", n = n as i64, count = count_text(n), from = first)
                };
                let note = exchange(self, j.target()).then(|| tr!("migration-ask-exchange"));
                let (up, keep) = (ids.clone(), ids.clone());
                strip("migration-ask-bar", crate::local::UPLOAD, l)
                    .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).child(text))
                    .children(note.map(|n| div().flex_none().text_color(l.text2()).child(n)))
                    .child(div().flex_1())
                    .child(link("migration-to-server", tr!("migration-to-server", server = server), l, cx, move |m, cx| {
                        m.answer(&up, MigrationMode::Upload, cx)
                    }))
                    .child(link("migration-keep-local", tr!("migration-keep-local"), l, cx, move |m, cx| {
                        m.answer(&keep, MigrationMode::LocalArchive, cx)
                    }))
                    .into_any_element()
            }
            Bar::Running(id) => {
                let j = job(&id)?;
                let mut detail = tr!(
                    "migration-running-detail",
                    done = count_text(j.done),
                    total = count_text(j.total),
                    bytes = size_text(j.bytes),
                    total_bytes = size_text(j.total_bytes)
                );
                // «осталось около …» — когда есть скорость.
                if let Some(d) = j.eta(Instant::now()) {
                    detail = format!("{detail} {}", tr!("migration-running-eta", eta = eta_text(d)));
                }
                strip("migration-job-bar", crate::local::UPLOAD, l)
                    .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).child(tr!("migration-running", to = self.place_name(&j.to, cx))))
                    .child(div().flex_none().text_color(l.text2()).child(detail))
                    .child(meter(j.done, j.total, 120., false, l))
                    .child(div().flex_1())
                    .child(link("migration-pause", tr!("migration-pause"), l, cx, move |m, cx| {
                        m.job_request(MigrationRequest::Pause { job: id }, cx)
                    }))
                    .child(link("migration-stop", tr!("migration-stop"), l, cx, move |m, cx| m.ask_stop(id, cx)))
                    .into_any_element()
            }
            Bar::Paused(id) => {
                let j = job(&id)?;
                let detail =
                    format!("· {} {} · «{}»", count_text(j.done), tr!("migration-of", total = count_text(j.total)), self.place_name(&j.to, cx));
                strip("migration-job-bar", PAUSE, l)
                    .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).child(tr!("migration-paused")))
                    .child(div().flex_none().text_color(l.text2()).child(detail))
                    .child(meter(j.done, j.total, 60., false, l))
                    .child(div().flex_1())
                    .child(link("migration-resume", tr!("migration-resume"), l, cx, move |m, cx| {
                        m.job_request(MigrationRequest::Resume { job: id }, cx)
                    }))
                    .child(link("migration-stop", tr!("migration-stop"), l, cx, move |m, cx| m.ask_stop(id, cx)))
                    .into_any_element()
            }
            Bar::NoSpace(id) => {
                let j = job(&id)?;
                let left_bytes = j.total_bytes.saturating_sub(j.bytes);
                let text = tr!(
                    "migration-no-space",
                    server = self.account_name(j.target(), cx),
                    n = j.left() as i64,
                    count = count_text(j.left()),
                    bytes = size_text(left_bytes)
                );
                strip("migration-quota-bar", glyph::WARNING, l)
                    .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).child(text))
                    .child(div().flex_1())
                    .child(link("migration-resume", tr!("migration-resume"), l, cx, move |m, cx| {
                        m.job_request(MigrationRequest::Resume { job: id }, cx)
                    }))
                    .child(link("migration-keep-local-short", tr!("migration-keep-local-short"), l, cx, move |m, cx| m.keep_local(id, cx)))
                    .into_any_element()
            }
            Bar::Stop(id) => {
                let j = job(&id)?;
                strip("migration-stop-bar", glyph::INFO, l)
                    .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).child(tr!(
                        "migration-stop-ask",
                        done = count_text(j.done),
                        total = count_text(j.total)
                    )))
                    .child(div().flex_1())
                    .child(link("migration-stop-keep", tr!("migration-stop-keep"), l, cx, move |m, cx| m.stop_answer(id, Some(true), cx)))
                    .child(link("migration-stop-return", tr!("migration-stop-return"), l, cx, move |m, cx| m.stop_answer(id, Some(false), cx)))
                    .child(link("migration-stop-no", tr!("migration-stop-no"), l, cx, move |m, cx| m.stop_answer(id, None, cx)))
                    .into_any_element()
            }
        })
    }

    /// Ход в строке состояния (как квота): «Перенос: 3 120 из 12 480 · 1,2 ГБ»; щелчок — полоса задания.
    pub(crate) fn migration_status(&self, l: &Look, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let j = self.migration.active()?;
        let (glyph_, text, full) = match j.state {
            MigrationState::Paused => (PAUSE, tr!("migration-status-paused", done = count_text(j.done), total = count_text(j.total)), false),
            MigrationState::QuotaFull => (glyph::WARNING, tr!("migration-status-no-space"), true),
            _ => (
                crate::local::UPLOAD,
                tr!("migration-status", done = count_text(j.done), total = count_text(j.total), bytes = size_text(j.bytes)),
                false,
            ),
        };
        let detail = full.then(|| format!("· {} {}", count_text(j.done), tr!("migration-of", total = count_text(j.total))));
        Some(
            div()
                .id("migration-status")
                .h(px(22.))
                .px(px(6.))
                .flex()
                .items_center()
                .gap(px(8.))
                .rounded(l.px(Metric::RadiusControl))
                .cursor_pointer()
                .hover(|d| d.bg(l.hover()))
                .when(self.migration.open, |d| d.bg(l.pressed()))
                .child(icon(glyph_, 12.))
                .child(div().when(full, |d| d.text_color(l.text()).font_weight(FontWeight::SEMIBOLD)).child(text))
                .children(detail)
                .child(meter(j.done, j.total, 64., full, l))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.migration.open = !this.migration.open;
                    cx.notify();
                })),
        )
    }

    /// Обновление хранилища (формат 5 → 6): строка вместо списка.
    pub(crate) fn store_upgrade(&self, l: &Look) -> Option<AnyElement> {
        let percent = self.migration.upgrade?;
        Some(
            div()
                .id("store-upgrade")
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(10.))
                .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child(tr!("sync-store-upgrade", percent = percent as i64)))
                .child(meter(percent, 100, 240., false, l))
                .child(div().text_size(l.px(Metric::TypeCaption)).text_color(l.text2()).child(tr!("sync-store-upgrade-hint")))
                .into_any_element(),
        )
    }
}

/// Полоса над списком: нейтральная, значок слева, ссылки-кнопки справа (макет S8-D).
fn strip(id: &'static str, glyph_: char, l: &Look) -> Stateful<Div> {
    div()
        .id(id)
        .mx(px(8.))
        .mb(px(6.))
        .h(px(36.))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(10.))
        .px(px(14.))
        .rounded(px(6.))
        .bg(l.fill())
        .text_size(px(13.))
        .text_color(l.text())
        .overflow_hidden()
        .child(icon(glyph_, 14.).text_color(l.text2()))
}

/// Кнопка-ссылка полосы.
fn link(
    id: &'static str,
    text: String,
    l: &Look,
    cx: &mut Context<Mailer>,
    f: impl Fn(&mut Mailer, &mut Context<Mailer>) + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .px(px(8.))
        .py(px(4.))
        .rounded(l.px(Metric::RadiusControl))
        .cursor_pointer()
        .text_color(l.accent_text())
        .hover(|d| d.bg(l.hover()))
        .child(text)
        .on_click(cx.listener(move |this, _, _, cx| f(this, cx)))
}

/// Полоса хода: доля готового.
fn meter(done: u64, total: u64, w: f32, strong: bool, l: &Look) -> Div {
    let fill = if total == 0 { 0. } else { (done.min(total) as f32) / total as f32 };
    div()
        .flex_none()
        .w(px(w))
        .h(px(4.))
        .rounded(px(2.))
        .overflow_hidden()
        .bg(l.c(Role::StrokeDivider))
        .child(div().h_full().w(relative(fill)).bg(if strong { l.text() } else { l.accent() }))
}

/// Опись части (ADR 0005, `parts.rs`): тексты и сцены переноса.
pub(crate) const PART: crate::part::Part =
    crate::part::Part { name: "migration", texts: TEXTS, scenes: scenes::SCENES, reset: Some(scenes::reset), ..crate::part::Part::NONE };

const TEXTS: &[pets_i18n::Resource] = &[("ru", include_str!("../i18n/ru/migration.ftl")), ("en", include_str!("../i18n/en/migration.ftl"))];

/// Сцены `--screens` (демо S8-14): задание «Exchange · Входящие → Архив» в каждом состоянии.
pub(crate) mod scenes {
    use gpui_kit::*;
    use postman_demo::store::JobShot;

    use crate::screens::Scene;
    use crate::{Mailer, Section};

    pub(crate) const SCENES: &[Scene] = &[
        // Вопрос над списком «Архива»: 12 480 писем перенесены из «Exchange · Входящие», значки «ждёт переноса».
        Scene::new("migration-ask", ask, ask_ready).wait(to_job_letters),
        // Идёт: полоса задания (щелчок по строке состояния) и ход в строке состояния.
        Scene::new("migration-running", running, running_ready).wait(to_job_letters),
        // Нет места на сервере: полоса показывается сама, в строке состояния — значок и жирный текст.
        Scene::new("migration-quota", quota, quota_ready).wait(to_job_letters),
        // «Остановить»: строка подтверждения.
        Scene::new("migration-cancel", cancel, cancel_ready).wait(cancel_wait),
        // Обновление хранилища: строка вместо списка.
        Scene::new("store-upgrade", upgrade, upgrade_ready),
    ];

    /// Чистое начало: заданий и строк переноса нет.
    pub(crate) fn reset(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) {
        m.migration.jobs.clear();
        m.migration.open = false;
        m.migration.stopping = None;
        m.migration.upgrade = None;
        cx.notify();
    }

    /// Задание демо в состоянии `shot` (одно на копию ящика: письма задания — постоянными ключами) и папка цели
    /// открыта.
    fn job(m: &mut Mailer, shot: JobShot, cx: &mut Context<Mailer>) -> Result<(), String> {
        use postman_demo::store::{JOB_DONE, JOB_LETTER_SIZE, JOB_TOTAL};
        m.set_section(Section::Mail, cx);
        let id = match m.cache.jobs().map_err(|e| e.to_string())?.first() {
            Some(r) => r.id,
            None => postman_demo::store::migration(&m.cache, JobShot::Running).map_err(|e| e.to_string())?,
        };
        let mode = if shot == JobShot::Asking { postman_cache::MigrationMode::Ask } else { postman_cache::MigrationMode::Upload };
        let bytes = JOB_DONE as u64 * JOB_LETTER_SIZE;
        let row = postman_cache::JobRow { id, mode, state: shot.state(), total: JOB_TOTAL as u64, bytes };
        m.cache.save_job(&row).map_err(|e| e.to_string())?;
        m.load_jobs(cx);
        let (profile, remote) = postman_demo::store::JOB_TARGET;
        m.open_mailbox(postman_cache::scoped(profile, remote), cx);
        Ok(())
    }

    fn ask(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        job(m, JobShot::Asking, cx)
    }

    fn ask_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        let open = m.list.read(cx).mailbox.clone();
        matches!(m.migration.bar(open.as_deref()), Some(super::Bar::Ask(_))) && m.list.read(cx).has_marks()
    }

    /// Письма задания — в демо 2024 года, под письмами «Архива»: список — к первому из них (значки «ждёт переноса»).
    fn to_job_letters(_: &Scene, m: &mut Mailer, cx: &mut Context<Mailer>) {
        m.list.update(cx, |l, cx| {
            let first = l.count.saturating_sub(postman_demo::store::JOB_TOTAL);
            if l.count > 0 && !l.has_marks() && l.visible.start < first {
                // Строка — вверху окна; несколько строк запаса: прокрутка списка с заголовками групп неточна.
                l.scroll_to_top(first + 8, cx);
            }
        });
    }

    fn running(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        job(m, JobShot::Running, cx)?;
        m.migration.open = true;
        Ok(())
    }

    fn quota(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        job(m, JobShot::QuotaFull, cx)
    }

    fn job_ready(m: &Mailer, cx: &App, state: postman_core::model::MigrationState) -> bool {
        m.migration.active().is_some_and(|j| j.state == state) && m.list.read(cx).has_marks()
    }

    fn running_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        job_ready(m, cx, postman_core::model::MigrationState::Running)
    }

    fn quota_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        job_ready(m, cx, postman_core::model::MigrationState::QuotaFull)
    }

    fn cancel(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        job(m, JobShot::Running, cx)
    }

    fn cancel_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        m.migration.stopping.is_some() && m.list.read(cx).has_marks()
    }

    /// Как «Остановить» на полосе, без команды ядру: в демо движок этого задания не ведёт.
    fn cancel_wait(s: &Scene, m: &mut Mailer, cx: &mut Context<Mailer>) {
        to_job_letters(s, m, cx);
        let running = m.migration.active().filter(|j| j.state == postman_core::model::MigrationState::Running).map(|j| j.id);
        if m.migration.stopping.is_none() && running.is_some() {
            m.migration.stopping = running;
            cx.notify();
        }
    }

    fn upgrade(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.set_section(Section::Mail, cx);
        m.migration.upgrade = Some(42);
        cx.notify();
        Ok(())
    }

    fn upgrade_ready(_: &Scene, m: &Mailer, _: &App) -> bool {
        m.migration.upgrade.is_some()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use postman_core::model::{JobId, MigrationMode, MigrationState};

    use super::{Bar, JobView, Migrations, count_text};

    fn job(id: u64, state: MigrationState, to: &str) -> JobView {
        JobView {
            id: JobId(id),
            state,
            mode: MigrationMode::Ask,
            done: 0,
            total: 10,
            bytes: 0,
            total_bytes: 100,
            to: to.into(),
            from: vec!["@2/inbox".into()],
            since: None,
        }
    }

    fn with(jobs: Vec<JobView>) -> Migrations {
        Migrations { jobs: jobs.into_iter().map(|j| (j.id, j)).collect(), ..Migrations::default() }
    }

    /// Полоса над списком (макет S8-D): вопрос — только у папки цели, несколько заданий в одну папку — одна полоса;
    /// «нет места» — сама; полоса задания — по щелчку; остановка — главнее всего.
    #[test]
    fn which_bar() {
        let mut m =
            with(vec![job(1, MigrationState::Asking, "archive"), job(2, MigrationState::Asking, "archive"), job(3, MigrationState::Running, "x")]);
        assert_eq!(m.bar(Some("inbox")), None, "вопрос — пока открыта папка цели");
        assert_eq!(m.bar(Some("archive")), Some(Bar::Ask(vec![JobId(1), JobId(2)])));
        m.open = true;
        assert_eq!(m.bar(Some("archive")), Some(Bar::Running(JobId(3))));
        m.jobs.get_mut(&JobId(3)).unwrap().state = MigrationState::Paused;
        assert_eq!(m.bar(None), Some(Bar::Paused(JobId(3))));
        m.jobs.get_mut(&JobId(2)).unwrap().state = MigrationState::QuotaFull;
        assert_eq!(m.bar(None), Some(Bar::NoSpace(JobId(2))), "нет места — без щелчка");
        m.stopping = Some(JobId(3));
        assert_eq!(m.bar(None), Some(Bar::Stop(JobId(3))));
    }

    /// Переходы: ход обновляет задание; конец убирает его (и строку остановки) и отдаёт для тоста; строка состояния —
    /// идущее главнее остальных.
    #[test]
    fn progress_and_end() {
        let mut m = with(vec![job(1, MigrationState::Paused, "a"), job(2, MigrationState::Running, "b")]);
        assert_eq!(m.active().map(|j| j.id), Some(JobId(2)));
        let t0 = Instant::now();
        assert!(m.progress(JobId(2), 3, 10, 30, MigrationState::Running, t0).is_none());
        assert_eq!((m.jobs[&JobId(2)].done, m.jobs[&JobId(2)].bytes), (3, 30));
        m.stopping = Some(JobId(2));
        m.open = true;
        let end = m.progress(JobId(2), 3, 10, 30, MigrationState::Cancelled, t0).expect("конец задания");
        assert_eq!((end.state, end.left()), (MigrationState::Cancelled, 7));
        assert_eq!((m.stopping, m.jobs.len()), (None, 1));
        assert!(m.open, "задание на паузе ещё в строке состояния");
        assert!(m.progress(JobId(9), 1, 1, 1, MigrationState::Done, t0).is_none(), "чужое задание");
    }

    /// «Осталось около»: по скорости с начала выгрузки на экране; первые секунды — без оценки.
    #[test]
    fn eta() {
        let t0 = Instant::now();
        let mut j = job(1, MigrationState::Running, "a");
        j.total = 100;
        j.since = Some((t0, 10));
        j.done = 30;
        assert_eq!(j.eta(t0 + Duration::from_secs(2)), None);
        assert_eq!(j.eta(t0 + Duration::from_secs(20)), Some(Duration::from_secs(70)));
    }

    #[test]
    fn counts_read_like_the_mockup() {
        assert_eq!(count_text(12_480), "12\u{a0}480");
        assert_eq!(count_text(999), "999");
        assert_eq!(count_text(1_200_000), "1\u{a0}200\u{a0}000");
    }

    #[test]
    fn sizes() {
        let _ = pets_i18n::init(&crate::parts::texts(), "ru");
        assert_eq!(super::size_text(1_288_490_189), "1,2 ГБ");
        assert_eq!(super::size_text(84 * 1024), "84 КБ");
    }
}
