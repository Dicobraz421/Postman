//! Автопроверка ручных разделов `docs/MANUAL-TESTS.md` (2–6) без нажатий в окне: запуск `postman.exe` с ключами,
//! файлы снимков, состояние окна и журнал «Приложение» — средствами Windows. Данные — во временном `LOCALAPPDATA`,
//! настоящий ящик и настройки не трогаются.
//!
//! Нужны рабочий стол и `postman-worker.exe` рядом со сборкой; в CI не идут (`#[ignore]`):
//! `cargo build -p postman-worker && cargo nextest run -p postman-gpui --test manual --run-ignored only`.
//! Снимки остаются в `target/tmp/manual-screens` — их смотрит агент или владелец.

#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // тест: «не падать» — для кода приложения (S2-03)

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

fn exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_postman"))
}

/// Воркер рядом со сборкой — без него письма не разбираются, и сцены писем не дождутся готовности.
fn need_worker() {
    let worker = exe().with_file_name("postman-worker.exe");
    assert!(worker.exists(), "нет {} — сначала `cargo build -p postman-worker`", worker.display());
}

/// Запуск с отдельным каталогом данных (`LOCALAPPDATA`): настройки, демо-ящик и вход владельца не трогаются.
fn postman(data: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(exe());
    c.args(args).env("LOCALAPPDATA", data).env_remove("POSTMAN_URL").env_remove("POSTMAN_TOKEN").env_remove("POSTMAN_SCREENS_ONLY");
    c
}

fn run(mut c: Command, limit: Duration) -> Output {
    let child = c.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let start = Instant::now();
    let id = child.id();
    let waiter = std::thread::spawn(move || child.wait_with_output().unwrap());
    while !waiter.is_finished() {
        if start.elapsed() > limit {
            let _ = Command::new("taskkill").args(["/F", "/PID", &id.to_string()]).output();
            panic!("postman не завершился за {limit:?}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    waiter.join().unwrap()
}

/// PowerShell: вывод одной строкой.
fn ps(script: &str) -> String {
    let script = format!("[Console]::OutputEncoding = [Text.Encoding]::UTF8; {script}");
    let out = Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", &script]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// Окно процесса: дескриптор, свёрнуто ли, на переднем ли плане (через user32).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Window {
    handle: u64,
    iconic: bool,
    foreground: bool,
}

fn window_of(pid: u32) -> Option<Window> {
    let script = format!(
        "Add-Type -Namespace W -Name U -MemberDefinition '[DllImport(\"user32.dll\")] public static extern bool IsIconic(IntPtr h); [DllImport(\"user32.dll\")] public static extern IntPtr GetForegroundWindow();'; \
         $h = (Get-Process -Id {pid} -ErrorAction SilentlyContinue).MainWindowHandle; \
         if ($h -and $h -ne 0) {{ \"$([int64]$h) $([W.U]::IsIconic($h)) $([W.U]::GetForegroundWindow() -eq $h)\" }}"
    );
    let line = ps(&script);
    let mut parts = line.split_whitespace();
    let handle = parts.next()?.parse().ok()?;
    let iconic = parts.next()? == "True";
    let foreground = parts.next()? == "True";
    Some(Window { handle, iconic, foreground })
}

fn wait_for<T>(limit: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let start = Instant::now();
    while start.elapsed() < limit {
        if let Some(v) = f() {
            return Some(v);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    None
}

/// Своя копия Postman (не чужой процесс): закрыть по завершении теста.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Postman, запущенный не этим тестом, мешает проверке «одного экземпляра».
fn others_running() -> bool {
    ps("@(Get-Process -Name postman -ErrorAction SilentlyContinue).Count") != "0"
}

fn screens_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("manual-screens").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Раздел 2 (S2-05): `--screens` — пять экранов без участия человека, код 0; по-английски — те же экраны. Снимок
/// одного цвета (чёрный, пустой) приложение само считает ошибкой и выходит с кодом 1.
#[test]
#[ignore = "окно и рабочий стол: cargo nextest run -p postman-gpui --test manual --run-ignored only"]
fn s02_screens() {
    need_worker();
    const NAMES: [&str; 5] = ["list", "letter", "search", "calendar", "assistant"];
    // `assistant` продолжает сцену `meeting` (вкладка встречи): без неё сцене не на чем строиться.
    const RUN: &str = "list,letter,search,calendar,meeting,assistant";
    for lang in ["ru", "en"] {
        let data = tempfile::tempdir().unwrap();
        let dir = screens_dir(lang);
        let mut c = postman(data.path(), &["--screens", dir.to_str().unwrap(), "--lang", lang]);
        c.env("POSTMAN_SCREENS_ONLY", RUN);
        let out = run(c, Duration::from_secs(300));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{lang}: код {:?}\n{stdout}\n{}", out.status.code(), String::from_utf8_lossy(&out.stderr));
        for name in NAMES {
            let file = dir.join(format!("{name}.png"));
            let size = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
            assert!(size > 20_000, "{lang}: {} — {size} байт", file.display());
            assert!(stdout.lines().any(|l| l.starts_with(&format!("screen {name} "))), "{lang}: нет строки о {name}:\n{stdout}");
        }
    }
}

/// Раздел 4 (S2-06): второй запуск не открывает окна — показывает свёрнутое первое и выходит; после закрытия —
/// обычный старт; `--bench` при открытом Postman идёт в своём окне.
#[test]
#[ignore = "окно и рабочий стол: cargo nextest run -p postman-gpui --test manual --run-ignored only"]
fn s04_single_instance() {
    need_worker();
    assert!(!others_running(), "закройте Postman: второй запуск ушёл бы в него");
    let data = tempfile::tempdir().unwrap();
    let first = Running(postman(data.path(), &["--demo", "300", "--minimized", "--lang", "ru"]).spawn().unwrap());
    let pid = first.0.id();
    // Окно сворачивается после первого кадра (до него скрыто): ждём свёрнутого.
    wait_for(Duration::from_secs(30), || window_of(pid)).expect("окно первого запуска не появилось");
    let w = wait_for(Duration::from_secs(15), || window_of(pid).filter(|w| w.iconic));
    assert!(w.is_some(), "первый запуск с --minimized не свернулся: {:?}", window_of(pid));
    // Второй запуск: отдаёт аргументы первому и выходит сам, своего окна нет.
    let second = run(postman(data.path(), &[]), Duration::from_secs(15));
    assert!(second.status.success(), "второй запуск: код {:?}", second.status.code());
    let shown = wait_for(Duration::from_secs(10), || window_of(pid).filter(|w| !w.iconic));
    let shown = shown.expect("первое окно не развернулось после второго запуска");
    // Передний план Windows может не отдать процессу, запущенному не из проводника: только сообщаем.
    println!("первое окно развернулось; на переднем плане: {}", shown.foreground);
    // Замер — в своём окне, даже когда Postman открыт.
    let bench = run(postman(data.path(), &["--bench", "--demo", "2000"]), Duration::from_secs(180));
    let line = String::from_utf8_lossy(&bench.stdout);
    assert!(bench.status.success() && line.contains('='), "--bench при открытом Postman: код {:?}, вывод {line}", bench.status.code());
    assert!(window_of(pid).is_some(), "первое окно закрылось во время замера");
    drop(first);
    // После закрытия — обычный старт: процесс живёт и открывает окно.
    let again = Running(postman(data.path(), &["--demo", "300", "--minimized"]).spawn().unwrap());
    assert!(wait_for(Duration::from_secs(30), || window_of(again.0.id())).is_some(), "после закрытия Postman не стартует");
}

/// Раздел 5 (S2-06): сбой запуска — код 1 и ошибка в журнале «Приложение» от источника `Postman`.
#[test]
#[ignore = "журнал Windows: cargo nextest run -p postman-gpui --test manual --run-ignored only"]
fn s05_event_log() {
    let data = tempfile::tempdir().unwrap();
    let since = ps("(Get-Date).AddSeconds(-2).ToString('o')");
    let out = run(postman(data.path(), &["--demo", "abc", "--screens", r"Z:\нет\такого"]), Duration::from_secs(60));
    assert_eq!(out.status.code(), Some(1), "недоступный каталог снимков — код 1");
    let events = wait_for(Duration::from_secs(10), || {
        let text = ps(&format!(
            "Get-WinEvent -LogName Application -FilterXPath \"*[System[Provider[@Name='Postman'] and (Level=2)]]\" -MaxEvents 10 -ErrorAction SilentlyContinue | \
             Where-Object {{ $_.TimeCreated -ge [datetime]'{since}' -and $_.Id -eq 1000 }} | ForEach-Object {{ $_.Properties.Value }}"
        ));
        (!text.is_empty()).then_some(text)
    });
    let events = events.expect("в журнале «Приложение» нет ошибки Postman с кодом 1000");
    assert!(events.contains("снимков") || events.to_lowercase().contains("screens"), "запись не про сбой снимков: {events}");
    println!("журнал: {}", events.lines().next().unwrap_or_default());
}

/// Раздел 6 (S2-06, шаг 6): в простое Postman не грузит процессор — меньше 2 % одного ядра за 10 с.
#[test]
#[ignore = "окно и рабочий стол: cargo nextest run -p postman-gpui --test manual --run-ignored only"]
fn s06_idle_cpu() {
    need_worker();
    assert!(!others_running(), "закройте Postman: замер — своего процесса");
    let data = tempfile::tempdir().unwrap();
    let app = Running(postman(data.path(), &["--demo", "2000", "--lang", "ru"]).spawn().unwrap());
    let pid = app.0.id();
    assert!(wait_for(Duration::from_secs(30), || window_of(pid)).is_some(), "окно не появилось");
    // Первые секунды — разбор демо-ящика и первый кадр.
    std::thread::sleep(Duration::from_secs(15));
    let cpu = || ps(&format!("(Get-Process -Id {pid}).TotalProcessorTime.TotalMilliseconds")).replace(',', ".").parse::<f64>().unwrap();
    let before = cpu();
    std::thread::sleep(Duration::from_secs(10));
    let used = cpu() - before;
    println!("простой: {used:.0} мс процессора за 10 с");
    assert!(used < 200., "в простое {used:.0} мс за 10 с (> 2 % ядра)");
}

/// Раздел 6 (шаги 3 и 5): стопка тостов (три и «+N ещё») и история уведомлений — снимком сцены `toasts`.
#[test]
#[ignore = "окно и рабочий стол: cargo nextest run -p postman-gpui --test manual --run-ignored only"]
fn s06_toasts_scene() {
    need_worker();
    let data = tempfile::tempdir().unwrap();
    let dir = screens_dir("toasts");
    let mut c = postman(data.path(), &["--screens", dir.to_str().unwrap(), "--lang", "ru"]);
    c.env("POSTMAN_SCREENS_ONLY", "toasts");
    let out = run(c, Duration::from_secs(180));
    assert!(out.status.success(), "код {:?}: {}", out.status.code(), String::from_utf8_lossy(&out.stderr));
    assert!(dir.join("toasts.png").exists());
}
