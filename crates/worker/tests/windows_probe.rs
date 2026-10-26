//! Диагностика песочницы Windows: какое ограничение не даёт воркеру стартовать.
//! Запуск: `cargo test -p postman-worker --test windows_probe -- --ignored --nocapture` (отладочная сборка).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)
#![cfg(windows)]

use std::path::PathBuf;
use std::time::Duration;

use postman_worker::{Client, Request};

#[test]
#[ignore = "диагностика, запускается явно"]
fn which_restriction_breaks_startup() {
    let variants = [
        "",
        "appcontainer",
        "job_ui",
        "job",
        "childproc",
        "win32k",
        "dyncode",
        "msonly",
        "strict",
        "ext",
        "font",
        "heap",
        "aslr",
        "dep",
        "imageload",
        "all",
    ];
    for v in variants {
        // Тест один и идёт последовательно: переменная окружения меняется только здесь.
        // SAFETY: других потоков, читающих окружение, в этом тестовом процессе нет.
        unsafe { std::env::set_var("POSTMAN_SANDBOX_DROP", v) };
        let mut c = Client::new(PathBuf::from(env!("CARGO_BIN_EXE_postman-worker")), Duration::from_secs(5));
        let r = c.render(&Request::Text("проверка".into()));
        println!(
            "снято [{v:>12}]: {:?} {}",
            c.level(),
            match r {
                Ok(_) => "ОК".to_string(),
                Err(e) => e,
            }
        );
    }
}
