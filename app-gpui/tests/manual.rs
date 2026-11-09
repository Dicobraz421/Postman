//! Автопроверка ручных разделов `docs/MANUAL-TESTS.md` (2–6, 12, 13, 15, 16, 18–33, 35–43) без нажатий в окне: запуск `postman.exe` с ключами,
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
    themed_scenes(LIGHT, DARK);
}

/// Снимки сцен в обеих темах: `target/tmp/manual-screens/{light,dark}`; тёмные файлы — с окончанием `-dark`.
fn themed_scenes(light: &[&str], dark: &[&str]) {
    themed_scenes_in("", light, dark);
}

/// То же в каталогах `<приставка><тема>`: отдельный запуск со своим порядком сцен.
fn themed_scenes_in(prefix: &str, light: &[&str], dark: &[&str]) {
    for (theme, names) in [("light", light), ("dark", dark)] {
        let data = tempfile::tempdir().unwrap();
        let dir = screens_dir(&format!("{prefix}{theme}"));
        let mut c = postman(data.path(), &["--screens", dir.to_str().unwrap(), "--lang", "ru", "--theme", theme]);
        c.env("POSTMAN_SCREENS_ONLY", names.join(","));
        let out = run(c, Duration::from_secs(420));
        assert!(out.status.success(), "{theme}: код {:?}: {}", out.status.code(), String::from_utf8_lossy(&out.stderr));
        for name in names {
            let file = if theme == "dark" { format!("{name}-dark.png") } else { format!("{name}.png") };
            assert!(dir.join(file).exists(), "{theme}: нет снимка {name}");
        }
    }
}

/// Разделы 27–31 (S5-07, S5-04, S5-08, S5-10, S5-05б): вложения и просмотр, сроки, выделение и меню, офлайн, палитра,
/// поиск, виды беседы. Смотрит агент.
#[test]
#[ignore = "окно и рабочий стол: cargo nextest run -p postman-gpui --test manual --run-ignored only"]
fn s27_to_s31_scenes() {
    need_worker();
    const LIGHT: &[&str] = &[
        "attachments",
        "preview-image",
        "preview-pdf",
        "preview-pdf-end",
        "preview-zip",
        "attach-macro",
        "lifecycle",
        "overdue",
        "bulk",
        "context-menu",
        "phishing",
        "offline",
        "palette",
        "palette-query",
        "search",
        "search-letter",
        "search-next",
        "folder-menu",
        "thread",
        "thread-view-menu",
        "thread-stack",
        "thread-list",
        "settings-reading",
    ];
    const DARK: &[&str] = &[
        "attachments",
        "preview-pdf",
        "attach-macro",
        "lifecycle",
        "overdue",
        "bulk",
        "context-menu",
        "phishing",
        "palette",
        "search",
        "search-letter",
        "thread",
        "thread-stack",
        "thread-list",
    ];
    themed_scenes(LIGHT, DARK);
}

/// Разделы 32, 33, 35, 36 (S5-14, S5-12 Ф1, S5-16, S5-20): переход по совпадениям, значки палитры, подписи S/MIME, вкладка
/// почты, настройки календаря и цвет свободного времени. Смотрит агент.
#[test]
#[ignore = "окно и рабочий стол: cargo nextest run -p postman-gpui --test manual --run-ignored only"]
fn s32_to_s36_scenes() {
    need_worker();
    const LIGHT: &[&str] = &[
        "search",
        "search-letter",
        "search-next",
        "palette",
        "smime",
        "smime-gost",
        "smime-bad",
        "smime-untrusted",
        "quick",
        "tab-mail",
        "settings-calendar",
        "settings-calendar-free",
        "meeting",
        "meeting-blue",
    ];
    const DARK: &[&str] = &["palette", "smime", "smime-bad", "smime-untrusted", "settings-calendar", "meeting", "meeting-blue"];
    themed_scenes(LIGHT, DARK);
}

/// Разделы 37–41 (S5-17, S5-23, S5-18, S5-27, S5-24): схема клавиш, KeyTips, окна встречи, разметка письма, состояния
/// карточки приглашения и боковая панель. Смотрит агент.
#[test]
#[ignore = "окно и рабочий стол: cargo nextest run -p postman-gpui --test manual --run-ignored only"]
fn s37_to_s41_scenes() {
    need_worker();
    // `settings-keys` — в `s22_to_s26_scenes`. KeyTips — отдельным запуском: сцена ждёт удержания Ctrl, и прошлые сцены
    // его сбрасывают.
    themed_scenes_in("keytips-", &["keytips", "keytips-single"], &["keytips"]);
    const LIGHT: &[&str] = &[
        "compose",
        "meeting",
        "meeting-slot",
        "meeting-blue",
        "invite",
        "invite-answered",
        "invite-updated",
        "invite-cancelled",
        "invite-outdated",
        "invite-side",
    ];
    const DARK: &[&str] = &["compose", "meeting", "meeting-slot", "invite", "invite-answered", "invite-side"];
    themed_scenes(LIGHT, DARK);
}

/// Разделы 42, 43 (S5-31, S5-25): цвета главного окна, письма и календаря в обеих темах, контакты и «Часто пишу». Смотрит агент.
#[test]
#[ignore = "окно и рабочий стол: cargo nextest run -p postman-gpui --test manual --run-ignored only"]
fn s42_s43_scenes() {
    need_worker();
    const LIGHT: &[&str] = &["list", "letter", "contacts", "contacts-menu", "contact-edit", "contacts-frequent"];
    const DARK: &[&str] = &["list", "letter", "contacts", "contacts-menu", "contact-edit", "contacts-frequent", "settings-categories"];
    themed_scenes(LIGHT, DARK);
}

/// Сценарии с вводом внутри процесса (`app-gpui/src/drill.rs`): клавиши и мышь идут только в окно Postman.
#[test]
#[ignore = "окно и рабочий стол: cargo nextest run -p postman-gpui --test manual --run-ignored only"]
fn input_drills() {
    need_worker();
    const DRILLS: &[&str] = &["drill-tabs", "drill-select", "drill-split", "drill-keytips", "drill-palette", "drill-nav-mail-only", "drill-nav"];
    themed_scenes(DRILLS, DRILLS);
}

/// Сценарии области «почта и письмо» (`app-gpui/src/drill_mail.rs`): клавиши и мышь идут только в окно Postman.
#[test]
#[ignore = "окно и рабочий стол: cargo nextest run -p postman-gpui --test manual --run-ignored only"]
fn mail_drills() {
    need_worker();
    const DRILLS: &[&str] = &[
        "mail-t207-link",
        "mail-06-toast",
        "mail-07-send",
        "mail-07-remove",
        "mail-10-bar",
        "mail-19-feed",
        "mail-19-nav",
        "mail-20-keys",
        "mail-21-later",
        "mail-22-tabs",
        "mail-23-tab",
        "mail-23-chips",
        "mail-23-format",
        "mail-23-reply",
        "mail-23-switch",
        "mail-25-sort",
        "mail-27-steps",
        "mail-27-broken",
        "mail-27-chart",
        "mail-28-due",
        "mail-28-date",
        "mail-28-remind",
        "mail-28-wait",
        "mail-28-expire",
        "mail-29-range",
        "mail-29-read",
        "mail-29-archive",
        "mail-29-due",
        "mail-29-move",
        "mail-30-recent",
        "mail-30-letter",
        "mail-30-bold",
        "mail-30-syntax",
        "mail-32-hits",
        "mail-s809-local",
        "mail-35-keys",
        "mail-35-tabs",
        "mail-37-single",
        "mail-37-search",
        "mail-37-custom",
        "mail-39-hint",
        "mail-39-off",
        "mail-40-md",
        "mail-40-reply",
        "mail-t114-view",
        "mail-t116-order",
        "mail-31-outlook",
        "mail-t117-many",
        "mail-t117-one",
        "mail-s519-summary",
        "mail-10-scroll",
        "mail-19-solo",
        "mail-23-draft",
        "mail-25-two",
        "mail-25-hide",
        "mail-25-pane",
        "mail-27-zip",
        "mail-27-save",
        "mail-28-arrive",
        "mail-29-menu",
        "mail-29-menu-keys",
        "mail-29-menu-many",
        "mail-30-refine",
        "mail-31-stack",
        "mail-31-settings",
        "mail-32-keys",
        "mail-37-afe",
        "mail-t104-tree",
        "mail-t104-go",
        "mail-d9-expand",
        "mail-d9-reply",
        "mail-d9-attachments",
        "mail-d9-compose",
        "mail-29-thread-remind",
        "mail-29-thread-forward",
        "mail-t105-split",
        "mail-t105-narrow",
        "mail-t1d-focus",
        "mail-t1d-blur",
        "mail-t1d-accents",
        "mail-t1d-gray",
        "mail-s519-invite",
    ];
    // Сцены меняют ящик и оставляют вкладки: каждая — в своём запуске, со свежей копией демо.
    for name in DRILLS {
        themed_scenes(&[name], &[name]);
        tones_of(name);
    }
    restart_keeps_choices();
}

/// Цвета снимка сцены: тона таблицы токенов (T1-D, S5-19) видны на экране.
fn tones_of(name: &str) {
    let (light, dark): (&[u32], &[u32]) = match name {
        "mail-t1d-focus" => (&[0xD1E4F4, 0x0067C0, 0xF0F6FB], &[0x2B3E50, 0x191919, 0x2B2B2B, 0x2B333A]),
        "mail-t1d-blur" => (&[0xEBEBEB, 0x8A8A8A], &[0x3A3A3A]),
        "mail-t1d-gray" => (&[0xDADADA, 0xEDEDED, 0xC2D5E5], &[0xDADADA, 0xEDEDED, 0xC2D5E5]),
        "mail-s519-invite" => (&[0x0067C0, 0xDFF0FF, 0xD0E6FA], &[0x23364F, 0x2C4466]),
        _ => return,
    };
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("manual-screens");
    has_tones(&root.join("light").join(format!("{name}.png")), light);
    has_tones(&root.join("dark").join(format!("{name}-dark.png")), dark);
}

/// На снимке есть пиксели этих цветов (`0xRRGGBB`, допуск на округление — 2).
fn has_tones(png_file: &Path, tones: &[u32]) {
    let file = std::fs::File::open(png_file).unwrap();
    let mut reader = png::Decoder::new(std::io::BufReader::new(file)).read_info().unwrap();
    let mut rgba = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut rgba).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgba, "{}", png_file.display());
    let (px, _) = rgba[..info.buffer_size()].as_chunks::<4>();
    for &tone in tones {
        let want = [(tone >> 16) as u8, (tone >> 8) as u8, tone as u8];
        let close = |p: &&[u8; 4]| (0..3).all(|i| p[i].abs_diff(want[i]) <= 2);
        let n = px.iter().filter(close).count();
        assert!(n > 0, "{}: нет цвета #{tone:06X}", png_file.display());
    }
}

/// Точек подложки нерабочих часов на снимке дня: полоса шире 100 × 100 точек.
const OFF_HOURS_AREA: usize = 10_000;

/// Подложка T1-09, шаг 2: на снимке занимает заметную площадь, а не случайные пиксели.
fn has_area(png_file: &Path, tone: u32, least: usize) {
    let file = std::fs::File::open(png_file).unwrap();
    let mut reader = png::Decoder::new(std::io::BufReader::new(file)).read_info().unwrap();
    let mut rgba = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut rgba).unwrap();
    let (px, _) = rgba[..info.buffer_size()].as_chunks::<4>();
    let want = [(tone >> 16) as u8, (tone >> 8) as u8, tone as u8];
    let n = px.iter().filter(|p| (0..3).all(|i| p[i].abs_diff(want[i]) <= 2)).count();
    assert!(n >= least, "{}: цвета #{tone:06X} только {n} точек", png_file.display());
}

/// Перезапуск: первый процесс меняет вид и ширины, второй — на том же каталоге данных — видит их (`mail-restart-check`).
fn restart_keeps_choices() {
    let data = tempfile::tempdir().unwrap();
    let dir = screens_dir("restart");
    for name in ["mail-restart-set", "mail-restart-check"] {
        let mut c = postman(data.path(), &["--screens", dir.to_str().unwrap(), "--lang", "ru"]);
        c.env("POSTMAN_SCREENS_ONLY", name).env("POSTMAN_SCREENS_PREFS", "1");
        let out = run(c, Duration::from_secs(420));
        assert!(out.status.success(), "{name}: код {:?}: {}", out.status.code(), String::from_utf8_lossy(&out.stderr));
    }
}

/// Сценарии с вводом для настроек, календаря, встреч и контактов (`app-gpui/src/drill_cal.rs`); `-kept` — второй запуск с тем
/// же каталогом данных: выбор пережил перезапуск.
#[test]
#[ignore = "окно и рабочий стол: cargo nextest run -p postman-gpui --test manual --run-ignored only"]
fn cal_drills() {
    need_worker();
    const DRILLS: &[&str] = &[
        "cal-24-open",
        "cal-24-search",
        "cal-24-cats",
        "cal-24-purge",
        "cal-24-keys",
        "cal-24-sign",
        "cal-24-accounts",
        "cal-42-old-colors",
        "cal-38-arrows",
        "cal-38-long",
        "cal-38-drag",
        "cal-38-days",
        "cal-38-discard",
        "cal-26-meeting",
        "cal-26-contact",
        "cal-13-write",
        "cal-13-suggest",
        "cal-43-search",
        "cal-s521-views",
        "cal-s521-search",
        "cal-s521-weekend",
        "cal-d9-event",
        "cal-d9-keys",
    ];
    themed_scenes_in("cal-", DRILLS, DRILLS);
    // Настройки на диске: сцены первого запуска меняют их одна за другой, второй читает всё разом.
    let data = tempfile::tempdir().unwrap();
    let dir = screens_dir("cal-kept");
    let pass = |names: &str, args: &[&str]| {
        let mut c = postman(data.path(), &[&["--screens", dir.to_str().unwrap()], args].concat());
        c.env("POSTMAN_SCREENS_ONLY", names).env("POSTMAN_SCREENS_PREFS", "1");
        let out = run(c, Duration::from_secs(420));
        assert!(out.status.success(), "{names}: код {:?}: {}", out.status.code(), String::from_utf8_lossy(&out.stderr));
    };
    pass("cal-36-hours,cal-36-zone,cal-24-reading,cal-24-look", &["--lang", "ru", "--theme", "light"]);
    pass("cal-36-kept", &[]);
}

/// Сценарии задач, поручений, общих ящиков, локальных папок и экспорта (`app-gpui/src/drill_tasks.rs`). Диалоги Windows обойдены:
/// каталог экспорта задаёт сценарий. Отдельные запуски: тёмная и «Серая» темы, английский язык, перезапуск, модуль просмотра в папке.
#[test]
#[ignore = "окно и рабочий стол: cargo nextest run -p postman-gpui --test manual --run-ignored only"]
fn task_drills() {
    need_worker();
    const DRILLS: &[&str] = &[
        "task-t204-promote",
        "task-s604-chips",
        "task-s604-unchip",
        "task-s604-layout",
        "task-s604-phrases",
        "task-s605-lists",
        "task-s605-row",
        "task-s605-done",
        "task-s605-new",
        "task-s604-other-day",
        "task-s511-tab",
        "task-s511-delete",
        "task-s605-keys",
        "task-s605-mail",
        "task-s606-fields",
        "task-s606-subs",
        "task-s606-notes",
        "task-s607-repeat",
        "task-s607-next",
        "task-s607-31",
        "task-s607-remind",
        "task-s607-default",
        "task-s608-block",
        "task-s609-statuses",
        "task-s609-return",
        "task-s609-accept",
        "task-s609-proposed",
        "task-s609-remind",
        "task-s609-incoming",
        "task-s609-assign",
        "task-s609-letter",
        "task-s611-by-me",
        "task-s611-to-me",
        "task-s611-remind",
        "task-s611-delete",
        "task-s611-wait",
        "task-s526-rule",
        "task-s528-hints",
        "task-s528-button",
        "task-s528-marks",
        "task-s528-take",
        "task-s528-strip",
        "task-s528-steal",
        "task-s528-reply",
        "task-s528-readonly",
        "task-s528-revocation",
        "task-s529-look",
        "task-s530-order",
        "task-s530-look",
        "task-s815-group",
        "task-s815-edit",
        "task-s815-move",
        "task-s832-bar",
        "task-s833-formats",
        "task-s832-mbox",
        "task-s833-pst",
        "task-s832-stop",
        "task-t1-focus-lost",
        "task-t110-images",
        "task-t106-groups",
        "task-t109-day",
        "task-t122-sent",
        "task-t123-menu",
        "task-t123-keys",
        "task-s521-event",
        "task-s522-owners",
        "task-24-firstrun",
        "task-38-tip",
        "task-41-tip",
        "task-s823-sections",
        "task-t113-word",
        "task-t113-xlsm",
        "task-s827-viewers",
        "task-t118-click",
    ];
    /// Вид в тёмной теме проверяется по снимку.
    const DARK: &[&str] =
        &["task-s604-chips", "task-s605-row", "task-s609-statuses", "task-s528-strip", "task-s529-look", "task-t109-day", "task-s521-event"];
    let pass = |names: &[&str], args: &[&str], folder: &str, data: &Path| {
        let dir = screens_dir(folder);
        let mut c = postman(data, &[&["--screens", dir.to_str().unwrap()], args].concat());
        c.env("POSTMAN_SCREENS_ONLY", names.join(",")).env("POSTMAN_SCREENS_PREFS", "1");
        let out = run(c, Duration::from_secs(1500));
        assert!(out.status.success(), "{folder}: код {:?}: {}", out.status.code(), String::from_utf8_lossy(&out.stderr));
        for name in names {
            let found = std::fs::read_dir(&dir).unwrap().flatten().any(|f| f.file_name().to_string_lossy().starts_with(name));
            assert!(found, "{folder}: нет снимка {name}");
        }
    };
    let fresh = || tempfile::tempdir().unwrap();
    pass(DRILLS, &["--lang", "ru", "--theme", "light"], "task-light", fresh().path());
    pass(DARK, &["--lang", "ru", "--theme", "dark"], "task-dark", fresh().path());
    pass(&["task-t109-day"], &["--lang", "ru", "--theme", "gray"], "task-gray", fresh().path());
    pass(&["task-s604-en"], &["--lang", "en", "--theme", "light"], "task-en", fresh().path());
    // T1-09, шаг 2: нерабочие часы и выходные — своя подложка (`app.cal.off`) в трёх темах.
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("manual-screens");
    for (file, tone) in
        [("task-light/task-t109-day.png", 0xF6F6F6), ("task-dark/task-t109-day-dark.png", 0x232323), ("task-gray/task-t109-day-gray.png", 0xE2E2E2)]
    {
        has_area(&root.join(file), tone, OFF_HOURS_AREA);
    }
    // S5-22, шаг 2: цвет календаря после перезапуска.
    let data = fresh();
    pass(&["task-s522-colors"], &["--lang", "ru", "--theme", "light"], "task-colors", data.path());
    pass(&["task-s522-kept"], &[], "task-kept", data.path());
    // S8-27, шаг 2: модуль в папке до запуска; содержимое не исполняется, нужен только заголовок WASM.
    let data = fresh();
    let viewers = data.path().join("Postman").join("viewers");
    std::fs::create_dir_all(&viewers).unwrap();
    std::fs::write(viewers.join("fb2.json"), r#"{"name": "FB2", "version": "1.0", "abi": 1, "extensions": ["fb2"]}"#).unwrap();
    std::fs::write(viewers.join("fb2.wasm"), b"\0asm\x01\0\0\0").unwrap();
    pass(&["task-s827-module"], &["--lang", "ru", "--theme", "light"], "task-module", data.path());
}
