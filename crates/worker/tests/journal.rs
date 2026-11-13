//! Приёмник журнала по умолчанию (SBX-04): строки stderr воркера доходят до приёмника приложения с уровнем.
//! Отдельный файл: приёмник глобальный и ставится один раз на процесс.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use postman_worker::{Client, JournalLevel, Probe, Request};

#[test]
fn default_journal_gets_worker_lines_with_level() {
    assert_eq!(JournalLevel::of("thread 'main' panicked at src/x.rs:1:1"), JournalLevel::Error);
    assert_eq!(JournalLevel::of("memory allocation of 1024 bytes failed"), JournalLevel::Error);
    assert_eq!(JournalLevel::of("WARN postman-worker: user namespaces unavailable"), JournalLevel::Warn);
    let got = Arc::new(Mutex::new(Vec::<(JournalLevel, String)>::new()));
    let sink = Arc::clone(&got);
    assert!(postman_worker::set_default_journal(move |level, line| sink.lock().unwrap().push((level, line.to_owned()))));
    assert!(!postman_worker::set_default_journal(|_, _| {}), "второй вызов приёмник не меняет");
    let mut c = Client::new(PathBuf::from(env!("CARGO_BIN_EXE_postman-worker")), Duration::from_secs(10));
    let line = "postman-worker: stderr probe\n".len() as u32;
    assert_eq!(c.render(&Request::Probe(Probe::Stderr(line))).unwrap_err(), "stderr: записано");
    let until = Instant::now() + Duration::from_secs(5);
    while got.lock().unwrap().is_empty() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(got.lock().unwrap().first(), Some(&(JournalLevel::Warn, "postman-worker: stderr probe".to_owned())));
}
