//! Postman (`postman.exe`): интерфейс на gpui (gpui-kit). Ядро (хранилище, IR, воркер-песочница, демо-ящик) —
//! в крейтах ниже; здесь только интерфейс.
//!
//!   postman [--demo N] [--bench] [--screens <каталог> [--live]] [--sign-out] [--lang ru|en|auto] [--help] [--version]
//!
//! Без `--demo` — живой ящик: `POSTMAN_URL` + `POSTMAN_USER`/`POSTMAN_PASSWORD` (или `POSTMAN_TOKEN`), иначе
//! сохранённый вход, иначе строка входа в окне. `--sign-out` — забыть сохранённый вход.
//! Release-сборка — без консоли; вывод `--bench`, `--screens`, `--help` — в консоль родителя или в перенаправление.

// Release: без окна консоли (S4-01). Отладочная сборка — с консолью, журнал виден сразу.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod account;
mod actions;
mod attachments;
mod backend;
mod bar;
mod bulk;
mod buttons;
mod cal_model;
mod calendar;
mod chrome;
mod commands;
mod compose;
mod composer;
mod contacts;
mod diagnostics;
mod features;
mod filters;
mod folder_menu;
mod folders;
mod invite_card;
mod ir_view;
mod keymap;
#[cfg(test)]
mod keys_doc;
mod keytips;
mod labels;
mod lifecycle;
mod list;
mod live;
mod mailer;
mod meeting;
mod menu;
mod notices;
mod outline;
mod palette;
mod part;
mod parts;
mod plugins;
mod prefs;
mod preview;
mod reading;
mod roles;
mod rules;
mod screens;
mod search;
mod security;
mod select;
mod send_later;
mod settings;
mod settings_accounts;
mod settings_cal;
mod settings_cats;
mod settings_general;
mod settings_keys;
mod settings_look;
mod settings_security;
mod settings_sign;
mod settings_tab;
mod shared;
mod signin;
mod smime;
mod smime_enc;
mod smime_seal;
mod states;
mod sys;
mod tabs;
mod tasks;
mod thread;
mod vacation;

pub(crate) use mailer::{Mailer, Section};

use std::time::Instant;

use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::Level as Toast;
use postman_cache::Cache;
use postman_core::bench::{self, Bench};

/// Замер кучи Rust (S4-08): `cargo build --release -p postman-gpui --features alloc-count`, поля `heap_…` в бенче.
#[cfg(feature = "alloc-count")]
#[global_allocator]
static ALLOC: sys::alloc::Counting = sys::alloc::Counting;

pub(crate) fn format_date(ts: i64) -> String {
    let tz = jiff::tz::TimeZone::system();
    let (Ok(t), now) = (jiff::Timestamp::from_second(ts), jiff::Zoned::now()) else { return String::new() };
    let t = t.to_zoned(tz);
    if t.date() == now.date() { t.strftime("%H:%M").to_string() } else { t.strftime("%d.%m %H:%M").to_string() }
}

pub(crate) fn data_dir() -> std::path::PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_DATA_HOME"))
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| ".".into())
        .join("Postman")
}

/// Запуск невозможен (нет каталога данных, хранилище или окно не открылись): сообщение в журнал
/// (и журнал Windows «Приложение») и выход с кодом 1, без паники.
fn fatal(what: &str, e: impl std::fmt::Display) -> ! {
    pets_shell::fatal(&tr!(what), e)
}

/// Второй запуск: показать окно первого и сказать об этом тостом.
fn listen_second_instance(listener: pets_shell::Listener, (window, mailer): (AnyWindowHandle, Entity<Mailer>), cx: &mut App) {
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<Vec<String>>();
    listener.listen(move |args| {
        let _ = tx.unbounded_send(args);
    });
    cx.spawn(async move |cx| {
        use futures::StreamExt as _;
        while let Some(args) = rx.next().await {
            let _ = window.update(cx, |_, window, cx| {
                window.activate_window();
                // Ссылка `mailto:` из другой программы — новое письмо; иначе — «Postman уже открыт».
                match args.iter().find(|a| a.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("mailto:"))) {
                    Some(uri) => mailer.update(cx, |m, cx| m.open_mailto(uri, cx)),
                    None => mailer.update(cx, |m, cx| m.toasts.update(cx, |t, cx| t.push(Toast::Info, tr!("second-instance"), cx))),
                }
            });
        }
    })
    .detach();
}

/// `--help`: консольный вывод — по-английски, как журнал.
const USAGE: &str = "Postman - mail, calendar and contacts for Stalwart (JMAP)

Usage: postman [options]
  --demo [N]            demo mailbox with N messages (default 50000), no server
  --lang ru|en|auto     interface language (default: from Windows)
  --theme light|dark|system
                        theme (default: system - light/dark and accent from Windows, live)
  --density compact|normal|comfortable
                        list row density (default: normal)
  --sign-out            forget the saved sign-in
  --minimized           start with the window minimized (autostart)
  mailto:...            open a new message (default mail client)
  --bench               measure and exit: one key=value line on stdout
  --screens <dir> [--live] [--keep]
                        save screenshots (PNG) and exit: one 'screen <name> <path> <w>x<h>' line per file;
                        with --theme dark the files are <name>-dark.png
  --help, --version     this text, the version

Live mailbox: POSTMAN_URL and POSTMAN_USER/POSTMAN_PASSWORD (or POSTMAN_TOKEN); otherwise the saved sign-in.
Log: POSTMAN_LOG (e.g. debug); errors also go to the Windows Application log.
cmd waits for postman.exe only with 'start /wait'; PowerShell - with '| Out-Default'; Git Bash and .cmd files wait.
";

fn main() {
    let mut start = Instant::now();
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Release без консоли: вывод этих режимов — в консоль родителя (если поток не перенаправлен).
    if args.iter().any(|a| ["--bench", "--screens", "--help", "-h", "--version"].contains(&a.as_str())) {
        sys::attach_parent_console();
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return;
    }
    if args.iter().any(|a| a == "--version") {
        println!("postman {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let n = args.iter().position(|a| a == "--demo").and_then(|i| args.get(i + 1)).and_then(|s| s.parse().ok()).unwrap_or(50_000);
    let bench = args.iter().any(|a| a == "--bench");
    let demo = args.iter().any(|a| a == "--demo");
    // `--screens --live`: снимки живого ящика (учётная запись — из окружения).
    let live = args.iter().any(|a| a == "--live");
    // `--keep`: оставить рабочую копию данных бенча (для разбора).
    let keep = args.iter().any(|a| a == "--keep");
    // `--bench --live --warm`: замер живого ящика на кэше прошлого прогона (`--keep`), без — с первой синхронизации.
    let warm = args.iter().any(|a| a == "--warm");
    if bench {
        bench::on_idle(sys::bench_idle_fields);
    }
    let value = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).map(String::as_str);
    // Настройки пользователя (S5-09): замер и снимки — без файла, повторяемо; аргументы сильнее файла.
    let dir = data_dir();
    let screening = args.iter().any(|a| a == "--screens");
    let persist = !(bench || screening);
    let user_prefs = if persist { prefs::load(&dir) } else { postman_core::prefs::Prefs::default() };
    // Журнал (S5-30): уровень — из «Настройки → Общие», если `POSTMAN_LOG` не задан; без консоли (release) — в файл
    // папки журнала, по файлу на день, старше недели — удаляются.
    if persist {
        sys::shell::default_env("POSTMAN_LOG", user_prefs.general.log.filter());
        diagnostics::start_log_file(&dir);
    }
    pets_shell::init_logging("Postman", "POSTMAN_LOG");
    let lang = prefs::language_at_start(value("--lang"), &user_prefs);
    let theme = prefs::theme_at_start(value("--theme"), &user_prefs);
    let density = prefs::density_at_start(value("--density"), &user_prefs);
    // Тексты интерфейса (S2-06) — описи частей (`parts.rs`), файл на часть экрана; и `pets_ui::FTL`.
    let mut texts = parts::texts();
    texts.push(pets_ui::FTL);
    for p in pets_i18n::init(&texts, &lang) {
        tracing::warn!("i18n: {p}");
    }
    let mut screens = args.iter().position(|a| a == "--screens").and_then(|i| args.get(i + 1)).map(|d| {
        let suffix = match theme.as_str() {
            pets_ui::theme::FLUENT_DARK => "-dark",
            roles::GRAY => "-gray",
            _ => "",
        };
        screens::Driver::new(d.into(), live, suffix).unwrap_or_else(|e| fatal("fatal-screens-dir", e))
    });
    // Второй запуск отдаёт аргументы первому (тот показывает окно) и выходит; замеры и снимки — всегда отдельно.
    let listener = if bench || screens.is_some() {
        None
    } else {
        match pets_shell::single_instance("Postman", &args) {
            Ok(pets_shell::Instance::First(l)) => Some(l),
            Ok(pets_shell::Instance::Second) => return,
            Err(e) => {
                tracing::warn!("single instance: {e}");
                None
            }
        }
    };
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| fatal("fatal-data-dir", e));
    // Копии перетащенных вложений (T1-18) прошлого запуска — в фоне; замеры и снимки не трогают копии работающего окна.
    #[cfg(windows)]
    if persist {
        std::thread::spawn(|| sys::drag::clean(&sys::drag::copies()));
    }
    let prefs_path = persist.then(|| prefs::path(&dir));
    if args.iter().any(|a| a == "--sign-out") {
        account::forget(&dir);
    }
    let flags = account::Flags { demo, bench, screens: screens.is_some(), live };
    let start_with = account::choose(flags, postman_core::sync::Account::from_env(), || account::load_stored(&dir));
    if bench && live && !matches!(start_with, account::Start::Live(_)) {
        fatal("fatal-bench-account", "POSTMAN_URL");
    }
    if let (Some(d), account::Start::SignIn(_)) = (&mut screens, &start_with) {
        d.sign_in();
    }
    // Живой ящик: кэш учётной записи; нет учётной записи — пустой кэш в памяти до входа.
    let (cache, index) = match &start_with {
        account::Start::Demo => (demo_cache(&dir, bench, screens.is_some(), keep, n, &mut start), None),
        account::Start::Live(_) if bench => live::bench_store(warm, keep, &mut start).unwrap_or_else(|e| fatal("fatal-store", e)),
        account::Start::Live(a) => live::open_store(&account::cache_path(&dir, a)).unwrap_or_else(|e| fatal("fatal-store", e)),
        account::Start::SignIn(_) => (Cache::open_in_memory().unwrap_or_else(|e| fatal("fatal-store", e)), None),
    };

    gpui_kit::application().run(move |cx: &mut App| {
        gpui_kit::init(cx);
        // Тема (S5-01): «как в ОС» или `--theme`, на лету; до первого окна — чтение реестра, микросекунды.
        roles::init(&theme, density, user_prefs.look.accent, cx);
        security::apply(&user_prefs);
        // Клавиши — из реестра команд (S2-02) и свои из настроек (S5-09); в поле ввода клавиши без модификаторов — текст.
        commands::bind(cx, &user_prefs);
        prefs::init(user_prefs, prefs_path, cx);
        let minimized = args.iter().any(|a| a == "--minimized");
        let mailto = args.iter().find(|a| a.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("mailto:"))).cloned();
        let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
        // Свой заголовок окна (S5-01): вкладки, перетаскивание, «─ □ ✕» — `chrome.rs`.
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { title: Some("Postman".into()), appears_transparent: true, ..Default::default() }),
            ..Default::default()
        };
        let opened = gpui_kit::open_window(options, cx, |window, cx| {
            roles::observe_window(window);
            let bench = bench.then(|| if live { Bench::live(start) } else { Bench::new(start) });
            let cache = cache.clone();
            cx.new(|cx| {
                let mut m = Mailer { screens, ..Mailer::new(cache, bench, window, cx) };
                match start_with {
                    account::Start::Demo => {}
                    account::Start::Live(a) => m.start_sync(a, index, false, cx),
                    account::Start::SignIn(prefill) => m.show_signin(prefill, window, cx),
                }
                m
            })
        });
        match opened {
            Ok(handle) => {
                // `postman.exe mailto:…` (клиент по умолчанию, S5-30) — новое письмо; автозапуск — свёрнутым.
                let (window, mailer) = handle.clone();
                let _ = window.update(cx, |_, window, cx| {
                    if let Err(e) = sys::set_icons(window) {
                        tracing::warn!("window icons: {e}");
                    }
                    if minimized {
                        window.minimize_window();
                    }
                    if let Some(uri) = &mailto {
                        mailer.update(cx, |m, cx| m.open_mailto(uri, cx));
                    }
                });
                if let Some(l) = listener {
                    listen_second_instance(l, handle, cx);
                }
            }
            Err(e) => fatal("fatal-window", e),
        }
        cx.activate(true);
    });
    // Выход: копии перетащенных вложений (T1-18) больше не нужны.
    #[cfg(windows)]
    if persist {
        sys::drag::clean(&sys::drag::copies());
    }
}

/// Демо-ящик. Бенч и снимки — на свежей копии; подготовка данных не входит в first_frame_ms.
fn demo_cache(dir: &std::path::Path, bench: bool, screens: bool, keep: bool, n: usize, start: &mut Instant) -> Cache {
    let prepared = Instant::now();
    let path = if bench || screens {
        // Снимки — на небольшой свежей копии: письма в первой строке те же от прогона к прогону.
        let n = if screens { 2000 } else { n };
        let path = postman_demo::bench_copy(&postman_demo::testdata_dir(), n).unwrap_or_else(|e| fatal("fatal-bench-mailbox", e));
        if !keep {
            bench::remove_on_finish(path.clone());
        }
        path
    } else {
        let path = dir.join("demo.redb");
        postman_demo::ensure(&path, n).unwrap_or_else(|e| fatal("fatal-demo-mailbox", e));
        path
    };
    if bench {
        *start += prepared.elapsed();
    }
    Cache::open(&path).unwrap_or_else(|e| fatal("fatal-store", e))
}

#[cfg(test)]
mod tests {
    /// Наборы текстов согласованы: оба языка, одни ключи и переменные, без дублей.
    #[test]
    fn texts_are_consistent() {
        let mut texts = crate::parts::texts();
        texts.push(pets_ui::FTL);
        assert_eq!(pets_i18n::check(&texts), vec![]);
    }

    /// Строки интерфейса — только через Fluent: в коде экранов нет литералов с кириллицей.
    #[test]
    fn no_raw_ui_strings() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut found = Vec::new();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "rs") {
                let source = std::fs::read_to_string(&path).unwrap();
                for r in pets_i18n::raw_strings(&source) {
                    found.push(format!("{}:{}: {}", path.display(), r.line, r.text));
                }
            }
        }
        assert!(found.is_empty(), "сырые строки, перенести в i18n/<язык>/*.ftl:\n{}", found.join("\n"));
    }

    /// Цвета — только ролями темы (S5-01, базис Pets): литералы и именованные цвета gpui — лишь в `roles.rs`.
    #[test]
    fn no_color_literals() {
        const CALLS: [&str; 9] = ["rgb(", "rgba(", "hsla(", "white()", "black()", "red()", "green()", "blue()", "yellow()"];
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut found = Vec::new();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "rs") || path.file_name().is_some_and(|n| n == "roles.rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            for (n, line) in source.lines().enumerate() {
                for call in CALLS {
                    let hit = line.match_indices(call).any(|(i, _)| !line[..i].ends_with(|c: char| c.is_alphanumeric() || "_:.\"".contains(c)));
                    if hit {
                        found.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
                    }
                }
            }
        }
        assert!(found.is_empty(), "цвет литералом, взять роль темы (`roles.rs`):\n{}", found.join("\n"));
    }
}
