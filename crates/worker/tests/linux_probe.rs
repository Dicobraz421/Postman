//! Песочница Linux по слоям (LIN-04, ADR 0010) и диагностика: какой механизм не даёт воркеру стартовать.
//! Диагностика: `cargo test -p postman-worker --test linux_probe -- --ignored --nocapture` (отладочная сборка).
//! `POSTMAN_SANDBOX_DROP` снимает механизмы по одному; работает только без механизма — значит, мешает он.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)
#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use postman_worker::{Client, Probe, Request};

/// Тесты файла меняют окружение процесса: по одному.
static ENV: Mutex<()> = Mutex::new(());

fn client(drop: &str) -> Client {
    // SAFETY: окружение меняется только под `ENV`; клиент читает его при запуске воркера в этом же потоке.
    unsafe { std::env::set_var("POSTMAN_SANDBOX_DROP", drop) };
    Client::new(PathBuf::from(env!("CARGO_BIN_EXE_postman-worker")), Duration::from_secs(5))
}

/// Слои держат и поодиночке: без seccomp файл, сеть и запуск процесса закрывают Landlock, namespaces и rlimits — отказ
/// ОС, воркер жив. Без seccomp и Landlock сеть закрыта отдельным net namespace.
#[test]
fn layers_hold_without_seccomp() {
    let _env = ENV.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = tempfile::tempdir().unwrap();
    let secret = dir.path().join("secret.txt");
    std::fs::write(&secret, "секрет").unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let file = Probe::ReadFile(secret.to_string_lossy().into_owned());
    let net = Probe::Connect(addr);
    let mut c = client("seccomp");
    for p in [file, net.clone(), Probe::Spawn("/bin/true".into())] {
        let e = c.render(&Request::Probe(p.clone())).unwrap_err();
        assert!(e.starts_with("запрещено: "), "{p:?}: {e}");
    }
    let mut c = client("seccomp,landlock");
    let e = c.render(&Request::Probe(net)).unwrap_err();
    assert!(e.starts_with("запрещено: "), "{e}");
}

#[test]
#[ignore = "диагностика, запускается явно"]
fn which_restriction_breaks_startup() {
    let _env = ENV.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    // SAFETY: `landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION)` только возвращает версию ABI.
    let abi = unsafe { libc::syscall(libc::SYS_landlock_create_ruleset, std::ptr::null::<u8>(), 0usize, 1u32) };
    println!("ядро {}, Landlock ABI {}", std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default().trim(), abi.max(0));
    for f in ["kernel/apparmor_restrict_unprivileged_userns", "kernel/unprivileged_userns_clone", "user/max_user_namespaces"] {
        let v = std::fs::read_to_string(format!("/proc/sys/{f}")).map(|s| s.trim().to_owned()).unwrap_or_else(|_| "нет".into());
        println!("{f} = {v}");
    }
    for v in ["", "userns", "rlimits", "landlock", "nnp", "seccomp", "all"] {
        let mut c = client(v);
        let r = c.render(&Request::Text("проверка".into()));
        println!("снято [{v:>8}]: {}", r.map(|_| "ОК".to_string()).unwrap_or_else(|e| e));
    }
}
