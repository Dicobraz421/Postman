//! Песочница процессов на macOS сверх общих проверок `sandbox.rs` (MAC-03, ADR 0009): службы Mach, sysctl, предел
//! памяти воркера и хоста модулей, запись в унаследованный stderr-файл журнала.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)
#![cfg(target_os = "macos")]

use std::path::PathBuf;
use std::process::Command;
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

/// Без песочницы проверка удаётся, в воркере — отказ ОС, и воркер жив. Не удалась и без песочницы (службы нет в этой
/// версии macOS) — `false`: такая проверка ничего не доказывает.
fn denied_only_in_the_sandbox(c: &mut Client, p: Probe) -> bool {
    if let Err(e) = postman_worker::probe_here(p.clone()) {
        eprintln!("{p:?}: без песочницы тоже не удалось ({e}) — пропуск");
        return false;
    }
    let e = c.render(&Request::Probe(p.clone())).expect_err("должно быть отказано");
    assert!(e.starts_with("запрещено"), "{p:?}: {e}");
    assert!(c.render(&Request::Text("ещё жив".into())).is_ok());
    true
}

/// Службы Mach (`mach-lookup`): ни буфера обмена, ни окон, ни уведомлений, ни настроек — у launchd ничего не найти.
/// Первые три есть в любом сеансе, буфер обмена и окна — в сеансе пользователя (на раннере CI тоже).
#[test]
fn mach_services_are_out_of_reach() {
    let mut c = client();
    started(&mut c);
    let mut checked = 0;
    for name in [
        "com.apple.system.notification_center",
        "com.apple.cfprefsd.daemon",
        "com.apple.system.logger",
        "com.apple.pasteboard.1",
        "com.apple.windowserver.active",
    ] {
        checked += usize::from(denied_only_in_the_sandbox(&mut c, Probe::MachLookup(name.into())));
    }
    assert!(checked >= 2, "проверено служб: {checked}");
}

/// sysctl (`sysctl-read`): ни имени машины, ни списка процессов.
#[test]
fn sysctl_is_out_of_reach() {
    let mut c = client();
    started(&mut c);
    for name in ["kern.hostname", "kern.proc.all"] {
        assert!(denied_only_in_the_sandbox(&mut c, Probe::Sysctl(name.into())), "{name}");
    }
}

/// Предел памяти (`RLIMIT_AS` на macOS не действует): под пределом занять можно, сверх — выход с понятным кодом, клиент
/// поднимает новый процесс. Воркер — `MEMORY_LIMIT` (1 ГиБ).
#[test]
fn worker_memory_over_the_limit_exits_with_a_clear_code() {
    let mut c = client();
    started(&mut c);
    assert_eq!(c.render(&Request::Probe(Probe::Alloc(256 << 20))).unwrap_err(), "ДОСТУП РАЗРЕШЁН", "под пределом");
    let e = c.render(&Request::Probe(Probe::Alloc(3 << 29))).unwrap_err();
    assert!(e.contains("код 6 (превышен предел памяти)"), "{e}");
    assert!(c.render(&Request::Text("ещё жив".into())).is_ok());
}

/// Хост модулей просмотра — свой предел, `wasm::JOB_MEMORY` (512 МиБ): клиент передаёт его процессу.
#[test]
fn viewer_memory_over_the_limit_exits_with_a_clear_code() {
    let mut c = Client::viewer(PathBuf::from(env!("CARGO_BIN_EXE_postman-viewer")));
    assert_eq!(c.render(&Request::Probe(Probe::Alloc(256 << 20))).unwrap_err(), "ДОСТУП РАЗРЕШЁН", "под пределом");
    assert_eq!(c.level(), Level::Full, "{:?}", c.fallback());
    let e = c.render(&Request::Probe(Probe::Alloc(768 << 20))).unwrap_err();
    assert!(e.contains("код 6 (превышен предел памяти)"), "{e}");
}

/// Признак процесса-ребёнка в [`stderr_log_file_does_not_kill_the_worker`].
const CHILD: &str = "POSTMAN_TEST_STDERR_CHILD";

/// stderr воркера — канал к клиенту (SBX-01), клиент пишет строки в свой stderr — файл журнала: `RLIMIT_FSIZE = 0` воркера
/// не мешает, строки доходят до файла с префиксом `worker: `, сверх предела частоты — строка WARN. Тест запускает себя же
/// со stderr в файл.
#[test]
fn stderr_log_file_does_not_kill_the_worker() {
    if std::env::var_os(CHILD).is_some() {
        let mut c = client();
        started(&mut c);
        assert_eq!(c.render(&Request::Probe(Probe::Stderr(1 << 20))).unwrap_err(), "stderr: записано");
        assert!(c.render(&Request::Text("ещё жив".into())).is_ok());
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("postman.log");
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "stderr_log_file_does_not_kill_the_worker", "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .stderr(std::fs::File::create(&log).unwrap())
        .output()
        .unwrap();
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(out.status.success(), "{}\n{}", String::from_utf8_lossy(&out.stdout), text.chars().take(4000).collect::<String>());
    assert!(text.contains("worker: postman-worker: stderr probe"), "{}", text.chars().take(4000).collect::<String>());
    assert!(text.contains("worker: WARN stderr rate limit"), "{}", text.chars().take(4000).collect::<String>());
}
