//! Песочница процессов на Linux сверх общих проверок `sandbox.rs` (LIN-04, ADR 0010): seccomp убивает процесс с
//! понятным кодом, предел памяти воркера и хоста модулей, код завершения доходит до текста ошибки.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)
#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::time::Duration;

use postman_worker::{Client, Level, Probe, Request};

fn client() -> Client {
    Client::new(PathBuf::from(env!("CARGO_BIN_EXE_postman-worker")), Duration::from_secs(5))
}

/// Воркер ответил и он в полной изоляции: дальше клиент не пробует ослабленную.
fn started(c: &mut Client) {
    assert!(c.render(&Request::Text("ok".into())).is_ok());
    assert_eq!(c.level(), Level::Full, "{:?}", c.fallback());
}

/// Открыть файл, сокет или запустить процесс воркер не может даже попытаться: seccomp убивает его (`SIGSYS`), клиент
/// пишет причину и поднимает новый процесс. Без песочницы те же проверки удаются.
#[test]
fn forbidden_calls_kill_the_worker_with_a_clear_code() {
    let dir = tempfile::tempdir().unwrap();
    let secret = dir.path().join("secret.txt");
    std::fs::write(&secret, "секрет").unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let probes = [Probe::ReadFile(secret.to_string_lossy().into_owned()), Probe::Connect(addr), Probe::Spawn("/bin/true".into())];
    let mut c = client();
    started(&mut c);
    for p in probes {
        postman_worker::probe_here(p.clone()).unwrap_or_else(|e| panic!("{p:?} без песочницы: {e}"));
        let e = c.render(&Request::Probe(p.clone())).unwrap_err();
        assert!(e.contains("код 159 (запрещённый системный вызов, seccomp)"), "{p:?}: {e}");
        assert!(c.render(&Request::Text("ещё жив".into())).is_ok());
    }
}

/// Предел памяти — `RLIMIT_DATA` (частная выделенная память, SBX-03): под пределом выделение удаётся, сверх — отказ ядра, процесс аварийно завершается,
/// клиент поднимает новый. Воркер — `MEMORY_LIMIT` (1 ГиБ).
#[test]
fn worker_memory_over_the_limit_exits_with_a_clear_code() {
    let mut c = client();
    started(&mut c);
    assert_eq!(c.render(&Request::Probe(Probe::Alloc(256 << 20))).unwrap_err(), "ДОСТУП РАЗРЕШЁН", "под пределом");
    let e = c.render(&Request::Probe(Probe::Alloc(3 << 29))).unwrap_err();
    assert!(e.contains("превышен предел памяти"), "{e}");
    assert!(c.render(&Request::Text("ещё жив".into())).is_ok());
}

/// Хост модулей просмотра — свой предел, `wasm::JOB_MEMORY` (512 МиБ): клиент передаёт его процессу.
#[test]
fn viewer_memory_over_the_limit_exits_with_a_clear_code() {
    let mut c = Client::viewer(PathBuf::from(env!("CARGO_BIN_EXE_postman-viewer")));
    assert_eq!(c.render(&Request::Probe(Probe::Alloc(256 << 20))).unwrap_err(), "ДОСТУП РАЗРЕШЁН", "под пределом");
    assert_eq!(c.level(), Level::Full, "{:?}", c.fallback());
    let e = c.render(&Request::Probe(Probe::Alloc(768 << 20))).unwrap_err();
    assert!(e.contains("превышен предел памяти"), "{e}");
}
