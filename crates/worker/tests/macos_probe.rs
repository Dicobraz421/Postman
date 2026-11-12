//! Диагностика песочницы macOS (по образцу `windows_probe.rs`): какой запрет Seatbelt не даёт воркеру стартовать.
//! Запуск: `cargo test -p postman-worker --test macos_probe -- --ignored --nocapture` (отладочная сборка). Каждая строка —
//! профиль `deny default` плюс `(allow …)` одной операции; `all` — `(allow default)`, `off` — без `sandbox_init`.
//! Работает только с исключением — значит, мешает этот запрет; отказы видны в журнале Seatbelt (строка в конце).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)
#![cfg(target_os = "macos")]

use std::path::PathBuf;
use std::time::Duration;

use postman_worker::{Client, Request};

#[test]
#[ignore = "диагностика, запускается явно"]
fn which_restriction_breaks_startup() {
    let variants = [
        "",
        "mach-lookup",
        "sysctl-read",
        "file-read*",
        "file-write*",
        "file-ioctl",
        "iokit-open",
        "ipc-posix*",
        "process-fork",
        "process-exec*",
        "signal",
        "network*",
        "system*",
        "all",
        "off",
    ];
    for v in variants {
        // Тест один и идёт последовательно: переменная окружения меняется только здесь.
        // SAFETY: других потоков, читающих окружение, в этом тестовом процессе нет.
        unsafe { std::env::set_var("POSTMAN_SANDBOX_ALLOW", v) };
        let mut c = Client::new(PathBuf::from(env!("CARGO_BIN_EXE_postman-worker")), Duration::from_secs(5));
        let r = c.render(&Request::Text("проверка".into()));
        println!(
            "разрешено [{v:>13}]: {}",
            match r {
                Ok(_) => "ОК".to_string(),
                Err(e) => e,
            }
        );
    }
    println!("Отказы Seatbelt: log show --last 5m --style compact --predicate 'sender == \"Sandbox\"'");
}
