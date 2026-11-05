//! Автопроверка ручных разделов `docs/MANUAL-TESTS.md` (2–6, 12, 13, 15, 16, 18–26) без нажатий в окне: запуск `postman.exe` с ключами,
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

/// Подсистема из заголовка PE: 2 — окно, 3 — консоль.
fn pe_subsystem(exe: &Path) -> u16 {
    let bytes = std::fs::read(exe).unwrap();
    let pe = u32::from_le_bytes(bytes[0x3c..0x40].try_into().unwrap()) as usize;
    u16::from_le_bytes(bytes[pe + 24 + 68..pe + 24 + 70].try_into().unwrap())
}

/// Раздел 12 (S4-01): архив — установка без прав, запуск, отказ при открытом Postman, удаление с сохранением данных.
/// Ярлык и «Пуск» — владельцу: тест не трогает настоящее меню (`-NoShortcut`), установка идёт во временный `LOCALAPPDATA`.
/// `POSTMAN_ARCHIVE` — распакованный архив релиза; без него файлы собираются из текущей сборки (она с консолью).
#[test]
#[ignore = "окно и рабочий стол: cargo nextest run -p postman-gpui --test manual --run-ignored only"]
fn s12_install() {
    need_worker();
    assert!(!others_running(), "закройте Postman: установка отказывает при открытом");
    // Временный каталог — без коротких имён 8.3: сравнение путей в `install.ps1` их не разворачивает.
    let root = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let data = root.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let archive = std::env::var_os("POSTMAN_ARCHIVE").map(PathBuf::from);
    let src = archive.clone().unwrap_or_else(|| {
        let src = root.path().join("zip");
        std::fs::create_dir_all(&src).unwrap();
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("scripts/windows");
        for f in ["install.ps1", "README.txt"] {
            std::fs::copy(repo.join(f), src.join(f)).unwrap();
        }
        for f in ["postman.exe", "postman-worker.exe"] {
            std::fs::copy(exe().with_file_name(f), src.join(f)).unwrap();
        }
        src
    });
    // Шаг 1: README в UTF-8 с меткой — читается в «Блокноте» и консоли.
    let readme = std::fs::read(src.join("README.txt")).unwrap();
    assert!(readme.starts_with(&[0xef, 0xbb, 0xbf]), "README.txt без метки UTF-8");
    assert!(String::from_utf8_lossy(&readme).contains("УСТАНОВКА"));

    let script = |args: &[&str]| {
        let mut c = Command::new("powershell");
        c.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]).arg(src.join("install.ps1")).args(args).env("LOCALAPPDATA", &data);
        run(c, Duration::from_secs(60))
    };
    // Шаг 2: каталог по умолчанию, без прав администратора; песочнице открыт доступ к файлам.
    let out = script(&["-NoShortcut"]);
    assert!(out.status.success(), "установка: {}", String::from_utf8_lossy(&out.stderr));
    let dest = data.join(r"Programs\Postman");
    let installed = dest.join("postman.exe");
    assert!(dest.join("postman-worker.exe").exists() && dest.join("README.txt").exists());
    let acl = ps(&format!("(Get-Acl '{}').Sddl", dest.join("postman-worker.exe").display()));
    assert!(acl.contains(";;;AC)"), "нет доступа песочнице: {acl}");
    // Шаг 3: свойства файла и значок.
    let file = installed.display();
    let info = ps(&format!("$v = (Get-Item '{file}').VersionInfo; $v.ProductName + '|' + $v.ProductVersion"));
    assert!(info.starts_with("Postman|") && info.contains(env!("CARGO_PKG_VERSION")), "свойства файла: {info}");
    let icons = ps(&format!(
        "Add-Type -Name I -Namespace P -MemberDefinition '[DllImport(\"shell32.dll\", CharSet = CharSet.Unicode)] public static extern uint ExtractIconEx(string f, int i, IntPtr[] l, IntPtr[] s, uint n);'; \
         [P.I]::ExtractIconEx('{file}', -1, $null, $null, 0)"
    ));
    assert!(icons.trim().parse::<u32>().unwrap_or(0) >= 1, "в exe нет значка: {icons}");
    if archive.is_some() {
        assert_eq!(pe_subsystem(&installed), 2, "у сборки релиза не должно быть консоли");
    }
    // Шаг 6: справка в той же консоли.
    let mut c = Command::new(&installed);
    c.arg("--help").env("LOCALAPPDATA", &data);
    let help = run(c, Duration::from_secs(20));
    let text = String::from_utf8_lossy(&help.stdout);
    assert!(help.status.success() && text.contains("--screens"), "--help: код {:?}, вывод {text}", help.status.code());
    // Шаг 4: установленная копия работает, снимок строки состояния смотрит агент.
    let dir = screens_dir("install");
    let mut c = Command::new(&installed);
    c.args(["--screens", dir.to_str().unwrap(), "--lang", "ru"]).env("LOCALAPPDATA", &data).env("POSTMAN_SCREENS_ONLY", "letter");
    let out = run(c, Duration::from_secs(180));
    assert!(out.status.success(), "снимок установленной копии: код {:?}, {}", out.status.code(), String::from_utf8_lossy(&out.stderr));
    assert!(dir.join("letter.png").exists());
    // Шаг 7: открытый Postman не даёт заменить файлы.
    {
        let mut c = Command::new(&installed);
        c.args(["--demo", "300", "--minimized"]).env("LOCALAPPDATA", &data);
        let open = Running(c.spawn().unwrap());
        assert!(wait_for(Duration::from_secs(30), || window_of(open.0.id())).is_some(), "установленная копия не открыла окно");
        let out = script(&["-NoShortcut"]);
        let text = String::from_utf8_lossy(&out.stderr).to_lowercase().replace(['\r', '\n'], "");
        assert!(!out.status.success() && text.contains("close it"), "повторная установка при открытом: {text}");
    }
    // Шаг 8: файлы удалены, данные и вход — на месте.
    std::fs::create_dir_all(data.join("Postman")).unwrap();
    std::fs::write(data.join(r"Postman\keep.txt"), "x").unwrap();
    let mut gone = script(&["-Uninstall", "-NoShortcut"]);
    // Процесс только что закрыт: Windows отпускает файл не сразу.
    for _ in 0..5 {
        if gone.status.success() {
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
        gone = script(&["-Uninstall", "-NoShortcut"]);
    }
    assert!(gone.status.success(), "удаление: {}", String::from_utf8_lossy(&gone.stderr));
    assert!(!dest.exists(), "папка установки осталась");
    assert!(data.join(r"Postman\keep.txt").exists(), "данные пользователя удалены");
}

/// Разделы 13, 15, 16 (S4-02, S4-05, S4-07): экраны без сервера — контакты, карточка, меню строки, автоответ, правила,
/// квота. Смотрит агент: снимки в `target/tmp/manual-screens/settings`.
#[test]
#[ignore = "окно и рабочий стол: cargo nextest run -p postman-gpui --test manual --run-ignored only"]
fn s13_s15_s16_scenes() {
    need_worker();
    let data = tempfile::tempdir().unwrap();
    let dir = screens_dir("settings");
    let mut c = postman(data.path(), &["--screens", dir.to_str().unwrap(), "--lang", "ru"]);
    c.env("POSTMAN_SCREENS_ONLY", "contacts,contacts-menu,contact-edit,vacation,rules,quota");
    let out = run(c, Duration::from_secs(240));
    assert!(out.status.success(), "код {:?}: {}", out.status.code(), String::from_utf8_lossy(&out.stderr));
    for name in ["contacts", "contacts-menu", "contact-edit", "vacation", "rules", "quota"] {
        assert!(dir.join(format!("{name}.png")).exists(), "нет снимка {name}");
    }
}

/// Разделы 18–21 (S4-06, S5-05а, S5-02а, S5-06а): экраны без сервера — ветки общих ящиков, лента беседы и её виды,
/// метки, «Отправить позже», запланированное письмо. Смотрит агент: снимки в `target/tmp/manual-screens/mail`.
#[test]
#[ignore = "окно и рабочий стол: cargo nextest run -p postman-gpui --test manual --run-ignored only"]
fn s18_to_s21_scenes() {
    need_worker();
    // `labels` — отдельным запуском: сцена берёт строки открытых «Входящих», а не папки прошлой сцены.
    const RUNS: [&[&str]; 2] =
        [&["thread", "thread-view-menu", "thread-stack", "thread-list", "compose", "send-later", "sent", "sent-letter", "shared"], &["labels"]];
    let dir = screens_dir("mail");
    for names in RUNS {
        let data = tempfile::tempdir().unwrap();
        let mut c = postman(data.path(), &["--screens", dir.to_str().unwrap(), "--lang", "ru"]);
        c.env("POSTMAN_SCREENS_ONLY", names.join(","));
        let out = run(c, Duration::from_secs(300));
        assert!(out.status.success(), "{names:?}: код {:?}: {}", out.status.code(), String::from_utf8_lossy(&out.stderr));
        for name in names {
            assert!(dir.join(format!("{name}.png")).exists(), "нет снимка {name}");
        }
    }
}

/// Разделы 22–26 (S5-01, S5-06б, S5-09, S5-03, S5-11): письмо вкладкой и замечания, настройки, первый запуск, фильтры,
/// календарь, встреча, приглашение; тёмная тема — отдельным запуском. Смотрит агент: снимки в
/// `target/tmp/manual-screens/{light,dark}`.
#[test]
#[ignore = "окно и рабочий стол: cargo nextest run -p postman-gpui --test manual --run-ignored only"]
fn s22_to_s26_scenes() {
    need_worker();
    const LIGHT: &[&str] = &[
        "compose",
        "checks",
        "reply-inline",
        "filters",
        "quick",
        "tab-mail",
        "settings-general",
        "settings-categories",
        "settings-keys",
        "settings-accounts",
        "settings-quick",
        "settings-reading",
        "settings-sign",
        "signin",
        "calendar",
        "event",
        "event-organizer",
        "calendar-day",
        "calendar-week",
        "calendar-month",
        "meeting",
        "meeting-slot",
    ];
    const DARK: &[&str] = &["compose", "checks", "filters", "quick", "calendar", "event", "meeting", "contacts"];
    for (theme, names) in [("light", LIGHT), ("dark", DARK)] {
        let data = tempfile::tempdir().unwrap();
        let dir = screens_dir(theme);
        let mut c = postman(data.path(), &["--screens", dir.to_str().unwrap(), "--lang", "ru", "--theme", theme]);
        c.env("POSTMAN_SCREENS_ONLY", names.join(","));
        let out = run(c, Duration::from_secs(420));
        assert!(out.status.success(), "{theme}: код {:?}: {}", out.status.code(), String::from_utf8_lossy(&out.stderr));
        for name in names {
            // Снимки тёмной темы — с окончанием `-dark`.
            let file = if theme == "dark" { format!("{name}-dark.png") } else { format!("{name}.png") };
            assert!(dir.join(file).exists(), "{theme}: нет снимка {name}");
        }
    }
}
