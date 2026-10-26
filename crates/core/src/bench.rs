//! `--bench`: один сценарий замера для всех машин и версий, чтобы сравнение было честным.
//! Прогрев → листание с открытием письма каждый кадр → листание списка без открытия → ресайз окна → простой.
//! Итог — одна строка key=value в stdout, затем процесс завершается.
//!
//! Живой ящик (`--bench --live`, S4-03): сценарий начинается после первой синхронизации; в итоге ещё
//! `first_sync_ms` (запуск → `Status::Synced`) и `list_ms` (запуск → первый кадр со строками в списке).

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use cpu_time::{ProcessTime, ThreadTime};

const WARMUP: usize = 30;
/// Листание с открытием: прыжок на STEP строк и открытие письма каждый кадр (худший случай).
/// `POSTMAN_BENCH_SCROLL_FRAMES` — другое число кадров, например для профилировщика.
const SCROLL_FRAMES: usize = 400;
const STEP: usize = 25;
/// Листание списка без открытия писем: LIST_STEP строк за кадр, как прокрутка колесом.
/// `POSTMAN_BENCH_LIST_FRAMES` — другое число кадров.
const LIST_FRAMES: usize = 400;
const LIST_STEP: usize = 3;
/// Ресайз окна туда-обратно. `POSTMAN_BENCH_RESIZE_FRAMES` — другое число кадров (0 — без ресайза).
const RESIZE_FRAMES: usize = 120;
/// Простой: окно открыто, ввода нет. Сначала даём улечься фоновым задачам.
const IDLE_SETTLE: Duration = Duration::from_secs(3);
const IDLE: Duration = Duration::from_secs(10);

pub enum Step {
    Wait,
    /// Открыть письмо `i` (список прокручивается к нему).
    Select(usize),
    /// Прокрутить список так, чтобы строка `i` была первой; письмо не открывать.
    Scroll(usize),
    Resize(f32, f32),
    /// Сценарий окончен: остановить перерисовку и вызвать [`finish`].
    Done(String),
}

/// Замеры одной фазы: интервалы кадров, загрузка процесса и потока интерфейса.
#[derive(Default)]
struct Phase {
    frames: Vec<f32>,
    load: Option<Load>,
    ui_load: Option<(Instant, ThreadTime)>,
    cpu: f32,
    ui_cpu: f32,
}

impl Phase {
    /// Кадр `f` фазы (с 1); `dt` — интервал от прошлого кадра.
    fn frame(&mut self, f: usize, dt: Option<f32>) {
        if f == 1 {
            self.load = Some(Load::start());
            self.ui_load = ThreadTime::try_now().ok().map(|t| (Instant::now(), t));
        } else if let Some(dt) = dt {
            self.frames.push(dt);
        }
    }

    fn stop(&mut self) {
        if let Some(l) = self.load.take() {
            self.cpu = l.percent();
        }
        if let Some((wall, cpu)) = self.ui_load.take() {
            self.ui_cpu = percent(cpu.elapsed(), wall.elapsed());
        }
    }

    /// `name_p50 name_p95 name_p99 name_max name_cpu_pct name_ui_thread_cpu_pct`.
    fn report(&mut self, name: &str) -> String {
        let (p50, p99, max) = stats(&mut self.frames);
        let p95 = quantile(&self.frames, 0.95);
        format!(
            "{name}_p50={p50:.2} {name}_p95={p95:.2} {name}_p99={p99:.2} {name}_max={max:.2} {name}_cpu_pct={:.0} {name}_ui_thread_cpu_pct={:.0}",
            self.cpu, self.ui_cpu
        )
    }
}

pub struct Bench {
    start: Instant,
    first_frame: Option<Duration>,
    frame: usize,
    last: Option<Instant>,
    scroll: Phase,
    list: Phase,
    resize: Vec<f32>,
    ui_cpu: Vec<f32>,
    rss_peak: usize,
    private_peak: usize,
    /// Память на границах фаз (S4-08): после прогрева, листания с открытием, листания списка — кто сколько добавил.
    marks: Vec<(&'static str, Memory)>,
    /// Текущая фаза сценария ([`Bench::enter`]).
    phase: u8,
    scroll_frames: usize,
    list_frames: usize,
    resize_frames: usize,
    /// Живой ящик: ждать первой синхронизации ([`Bench::synced`]).
    live: bool,
    synced_at: Option<Duration>,
    list_at: Option<Duration>,
}

/// Число кадров фазы из переменной окружения или по умолчанию.
fn frames(var: &str, default: usize) -> usize {
    std::env::var(var).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

impl Bench {
    /// `start` — момент запуска процесса (до создания окна).
    pub fn new(start: Instant) -> Self {
        Self {
            start,
            first_frame: None,
            frame: 0,
            last: None,
            scroll: Phase::default(),
            list: Phase::default(),
            resize: Vec::new(),
            ui_cpu: Vec::new(),
            rss_peak: 0,
            private_peak: 0,
            marks: Vec::new(),
            phase: 0,
            scroll_frames: frames("POSTMAN_BENCH_SCROLL_FRAMES", SCROLL_FRAMES),
            list_frames: frames("POSTMAN_BENCH_LIST_FRAMES", LIST_FRAMES),
            resize_frames: frames("POSTMAN_BENCH_RESIZE_FRAMES", RESIZE_FRAMES),
            live: false,
            synced_at: None,
            list_at: None,
        }
    }

    /// Замер живого ящика: сценарий — после [`Bench::synced`].
    pub fn live(start: Instant) -> Self {
        Self { live: true, ..Self::new(start) }
    }

    /// Первая синхронизация закончилась.
    pub fn synced(&mut self) {
        self.synced_at.get_or_insert(self.start.elapsed());
    }

    /// Вызывается в начале каждого кадра; `rows` — сколько писем в списке.
    pub fn frame(&mut self, rows: usize) -> Step {
        let now = Instant::now();
        self.first_frame.get_or_insert(now - self.start);
        let dt = self.last.map(|l| (now - l).as_secs_f32() * 1000.0);
        self.last = Some(now);
        if self.live {
            if rows > 0 {
                self.list_at.get_or_insert(now - self.start);
            }
            if self.synced_at.is_none() {
                return Step::Wait;
            }
        }
        self.frame += 1;
        if self.frame.is_multiple_of(20) {
            self.sample_memory();
        }
        let rows = rows.max(1);
        let f = self.frame;
        if f <= WARMUP {
            return Step::Wait;
        }
        let f = f - WARMUP;
        if f <= self.scroll_frames {
            self.enter(1);
            self.scroll.frame(f, dt);
            return Step::Select((f * STEP) % rows);
        }
        let f = f - self.scroll_frames;
        if f <= self.list_frames {
            self.enter(2);
            self.list.frame(f, dt);
            return Step::Scroll((f * LIST_STEP) % rows);
        }
        let f = f - self.list_frames;
        if f <= self.resize_frames {
            if !self.enter(3)
                && let Some(dt) = dt
            {
                self.resize.push(dt);
            }
            let w = if (f / 4).is_multiple_of(2) { 1000.0 } else { 1280.0 };
            return Step::Resize(w, 800.0);
        }
        self.enter(4);
        Step::Done(self.report(rows))
    }

    /// Время построения кадра интерфейсом (без ожидания vsync).
    pub fn ui_time(&mut self, d: Duration) {
        if self.frame > WARMUP {
            self.ui_cpu.push(d.as_secs_f32() * 1000.0);
        }
    }

    /// Переход к фазе `phase` (1 — листание с открытием, 2 — листание списка, 3 — ресайз, 4 — итог): закрыть
    /// прошедшие фазы (и пропущенные — с нулём кадров) и отметить память на их границе. `true` — первый кадр фазы.
    fn enter(&mut self, phase: u8) -> bool {
        if self.phase >= phase {
            return false;
        }
        while self.phase < phase {
            match self.phase {
                0 => self.mark("start"),
                1 => {
                    self.scroll.stop();
                    self.mark("scroll");
                }
                2 => {
                    self.list.stop();
                    self.mark("list");
                }
                3 => self.mark("resize"),
                _ => {}
            }
            self.phase += 1;
        }
        true
    }

    fn mark(&mut self, phase: &'static str) {
        self.sample_memory();
        self.marks.push((phase, memory()));
    }

    fn sample_memory(&mut self) {
        let m = memory();
        self.rss_peak = self.rss_peak.max(m.rss);
        self.private_peak = self.private_peak.max(m.private);
    }

    fn report(&mut self, rows: usize) -> String {
        self.sample_memory();
        let r = stats(&mut self.resize);
        let c = stats(&mut self.ui_cpu);
        let ms = |d: Option<Duration>| d.unwrap_or_default().as_secs_f32() * 1000.0;
        let marks: String = self.marks.iter().map(|(p, m)| format!(" rss_{p}_mb={:.0} private_{p}_mb={:.0}", mb(m.rss), mb(m.private))).collect();
        let live = if self.live { format!("live=1 first_sync_ms={:.0} list_ms={:.0} ", ms(self.synced_at), ms(self.list_at)) } else { String::new() };
        format!(
            "{live}rows={rows} exe_mb={:.1} first_frame_ms={:.0} {} {} resize_p50={:.2} resize_p99={:.2} resize_max={:.2} \
             ui_cpu_p50={:.2} ui_cpu_p99={:.2} rss_peak_mb={:.0} private_peak_mb={:.0}{marks}",
            exe_mb(),
            self.first_frame.unwrap_or_default().as_secs_f32() * 1000.0,
            self.scroll.report("scroll"),
            self.list.report("list"),
            r.0,
            r.1,
            r.2,
            c.0,
            c.1,
            mb(self.rss_peak),
            mb(self.private_peak),
        )
    }
}

/// Рабочая копия данных бенча — удалить при завершении (H-01).
static WORK_COPY: OnceLock<PathBuf> = OnceLock::new();

/// Удалить данные бенча (файл или каталог), когда замер закончится (флаг `--keep` у приложения — не вызывать).
pub fn remove_on_finish(path: PathBuf) {
    let _ = WORK_COPY.set(path);
}

/// Удалить рабочую копию, отмеченную `remove_on_finish` (если отмечена), — перед выходом из процесса.
/// Каталог — как получится: открытые отображённые в память файлы (индекс поиска) Windows удалить не даст,
/// их уберёт следующий прогон.
pub fn remove_work_copy() {
    if let Some(p) = WORK_COPY.get() {
        let _ = if p.is_dir() { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) };
    }
}

/// Дополнительные поля строки итога в конце простоя (S4-08): например, куча из считающего распределителя
/// приложения. Вызывается в потоке замера после снятия `idle_…`.
static IDLE_FIELDS: OnceLock<fn() -> String> = OnceLock::new();

/// Задать [`IDLE_FIELDS`] (один раз за процесс).
pub fn on_idle(fields: fn() -> String) {
    let _ = IDLE_FIELDS.set(fields);
}

/// Замер простоя в отдельном потоке: печатает `prefix report idle_…` и завершает процесс.
/// Интерфейс к этому моменту должен перестать запрашивать кадры.
pub fn finish(line: String) {
    std::thread::spawn(move || {
        std::thread::sleep(IDLE_SETTLE);
        let load = Load::start();
        std::thread::sleep(IDLE);
        let m = memory();
        let extra = IDLE_FIELDS.get().map(|f| format!(" {}", f())).unwrap_or_default();
        println!("{line} idle_cpu_pct={:.1} idle_rss_mb={:.0} idle_private_mb={:.0}{extra}", load.percent(), mb(m.rss), mb(m.private));
        // Хранилище ещё открыто, но std открывает файлы с FILE_SHARE_DELETE: файл исчезнет, когда процесс
        // закроет его при выходе.
        remove_work_copy();
        let limit = std::env::var(MAX_IDLE_RSS_VAR).ok();
        if let Some(e) = over_limit(mb(m.rss), limit.as_deref()) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        std::process::exit(0);
    });
}

/// Простой с планировщиком жизненного цикла (S5-14): проход по ящику в начале простоя и таймер к ближайшему сроку.
pub const IDLE_LIFECYCLE_VAR: &str = "POSTMAN_BENCH_IDLE_LIFECYCLE";
/// Простой без связи (S5-14): строка «нет связи» с обратным отсчётом повтора (таймер раз в секунду).
pub const IDLE_OFFLINE_VAR: &str = "POSTMAN_BENCH_IDLE_OFFLINE";
/// Простой в другом разделе (S5-32): `calendar`, `contacts` или `tasks` открывается в начале простоя — таймеры и
/// перерисовки календаря (линия «сейчас», виды), контактов и задач (напоминания) попадают в `idle_cpu_pct`.
pub const IDLE_SECTION_VAR: &str = "POSTMAN_BENCH_IDLE_SECTION";

/// Порог рабочего набора в простое, МБ (S4-08): задан и превышен — бенч выходит с кодом 1 (для CI и стенда).
pub const MAX_IDLE_RSS_VAR: &str = "POSTMAN_BENCH_MAX_IDLE_RSS_MB";

/// Сообщение о превышении порога `limit` (значение [`MAX_IDLE_RSS_VAR`]) рабочим набором `rss_mb`.
fn over_limit(rss_mb: f32, limit: Option<&str>) -> Option<String> {
    let limit = limit?;
    match limit.trim().parse::<f32>() {
        Ok(max) if rss_mb > max => Some(format!("bench: idle working set {rss_mb:.0} MB is over {MAX_IDLE_RSS_VAR}={max}")),
        Ok(_) => None,
        Err(_) => Some(format!("bench: {MAX_IDLE_RSS_VAR}={limit:?} is not a number")),
    }
}

/// Загрузка CPU процессом: процессорное время / прошедшее, в % от одного ядра.
struct Load {
    wall: Instant,
    cpu: Option<ProcessTime>,
}

impl Load {
    fn start() -> Self {
        Self { wall: Instant::now(), cpu: ProcessTime::try_now().ok() }
    }

    fn percent(&self) -> f32 {
        self.cpu.as_ref().map_or(0.0, |cpu| percent(cpu.elapsed(), self.wall.elapsed()))
    }
}

/// Процессорное время / прошедшее, в % от одного ядра.
fn percent(cpu: Duration, wall: Duration) -> f32 {
    let wall = wall.as_secs_f32();
    if wall > 0.0 { cpu.as_secs_f32() / wall * 100.0 } else { 0.0 }
}

/// (p50, p99, max) в мс.
fn stats(v: &mut [f32]) -> (f32, f32, f32) {
    if v.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    v.sort_by(f32::total_cmp);
    (quantile(v, 0.5), quantile(v, 0.99), v[v.len() - 1])
}

/// Квантиль `q` отсортированных значений (пусто — 0).
fn quantile(sorted: &[f32], q: f32) -> f32 {
    sorted.get(((sorted.len().saturating_sub(1)) as f32 * q) as usize).copied().unwrap_or(0.0)
}

/// Рабочий набор процесса (RSS / Working Set), байты.
pub fn rss() -> usize {
    memory().rss
}

#[derive(Clone, Copy)]
struct Memory {
    /// Рабочий набор (Working Set): столбец «Память» в диспетчере задач.
    rss: usize,
    /// Windows — private bytes (PagefileUsage): столбец «Выделенная память» в диспетчере задач.
    /// На других ОС — виртуальный размер, с Windows не сравнивать.
    private: usize,
}

fn memory() -> Memory {
    memory_stats::memory_stats().map(|m| Memory { rss: m.physical_mem, private: m.virtual_mem }).unwrap_or(Memory { rss: 0, private: 0 })
}

fn mb(bytes: usize) -> f32 {
    bytes as f32 / 1_048_576.0
}

/// Размер исполняемого файла, МиБ.
fn exe_mb() -> f32 {
    std::env::current_exe().and_then(std::fs::metadata).map_or(0.0, |m| m.len() as f32 / 1_048_576.0)
}

#[cfg(test)]
mod tests {
    use super::over_limit;

    #[test]
    fn idle_rss_limit() {
        assert_eq!(over_limit(140.0, None), None);
        assert_eq!(over_limit(140.0, Some("150")), None);
        assert!(over_limit(151.0, Some("150")).is_some_and(|e| e.contains("151 MB")));
        assert!(over_limit(1.0, Some("много")).is_some());
    }
}
