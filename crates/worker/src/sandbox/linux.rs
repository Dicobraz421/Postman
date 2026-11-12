//! Linux (LIN-04, LIN-04b, ADR 0010): user/net/ipc namespaces, rlimits, Landlock (ФС, сеть, scopes), `no_new_privs`,
//! seccomp-bpf со списком разрешённых вызовов. Без rlimits, Landlock, `no_new_privs` или seccomp воркер не работает:
//! `isolate`/`lockdown` вернут причину, клиент покажет её текстом («песочница: …»), процесс выйдет с кодом 3.
//! Namespaces — по режиму [`Namespaces`] (аргумент `--namespaces=…`): `optional` (по умолчанию) — запрет дистрибутива
//! пишется в журнал (WARN, какой sysctl) и воркер работает без них; `required` — отказ, как без остальных механизмов.
//!
//! Порядок: `isolate` — первым делом, пока процесс однопоточный (`unshare(CLONE_NEWUSER)` в многопоточном процессе
//! запрещён ядром); потом сторож времени (новый поток), потом `lockdown`.

use std::collections::BTreeMap;
use std::io;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use seccompiler::{BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter, SeccompRule};

use super::{Level, Namespaces};
use crate::Process;

/// Предел адресного пространства воркера (`RLIMIT_AS`), если клиент не передал свой.
pub(crate) const MEMORY_LIMIT: usize = 1 << 30;

/// Старший ABI Landlock, правила которого ставим: ФС (V1–V5), сеть TCP (V4), scopes — абстрактные сокеты и сигналы
/// (V6). На ядре старше — эти же правила, на младшем — что ядро умеет (ФС закрывает уже V1, ядро 5.13).
const LANDLOCK_ABI: landlock::ABI = landlock::ABI::V6;

/// Диагностика (только отладочные сборки, по образцу `POSTMAN_SANDBOX_DROP` на Windows): `userns,rlimits,landlock,nnp,
/// seccomp` — снять перечисленные механизмы, `all` — все; `userns-deny` — вести себя так, будто namespaces запрещены. Так видно, какой запрет мешает воркеру стартовать
/// (`tests/linux_probe.rs`). В release переменная не читается и не передаётся — ослабить песочницу снаружи нельзя.
const DROP: &str = "POSTMAN_SANDBOX_DROP";

fn dropped(what: &str) -> bool {
    cfg!(debug_assertions) && std::env::var(DROP).is_ok_and(|v| v.split(',').map(str::trim).any(|d| d == what || d == "all"))
}

/// Ослабленного режима нет. `memory` — `RLIMIT_AS` процесса, `namespaces` — режим
/// namespaces: уходят аргументами `--memory=…` и `--namespaces=…`, окружение очищено.
pub(crate) fn spawn(exe: &Path, memory: usize, namespaces: Namespaces) -> io::Result<Process> {
    let mut cmd = Command::new(exe);
    cmd.env_clear()
        .current_dir("/")
        .arg(format!("--memory={memory}"))
        .arg(format!("--namespaces={}", namespaces.as_arg()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if cfg!(debug_assertions)
        && let Some(drop) = std::env::var_os(DROP)
    {
        cmd.env(DROP, drop);
    }
    // SAFETY: в pre_exec — только async-signal-safe prctl и close_range.
    unsafe {
        cmd.pre_exec(|| {
            // Дескрипторы родителя сверх stdin/stdout/stderr воркеру не достаются (SBX-02): закрываются при exec.
            // `CLOSE_RANGE_CLOEXEC`, а не закрытие: канал, по которому `Command` узнаёт об ошибке exec, должен жить.
            if libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, libc::CLOSE_RANGE_CLOEXEC) != 0 {
                return Err(io::Error::last_os_error());
            }
            // Клиент упал — воркер не переживает его.
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn()?;
    let (Some(stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(io::Error::other("worker pipes are missing"));
    };
    let child = Arc::new(Mutex::new(child));
    let watched = Arc::clone(&child);
    Ok(Process {
        stdin: Box::new(stdin),
        stdout: Box::new(stdout),
        stderr: Box::new(stderr),
        kill: Box::new(move || {
            let mut c = child.lock().unwrap_or_else(PoisonError::into_inner);
            let _ = c.kill();
            let _ = c.wait();
        }),
        // Код выхода (сторож времени — `EXIT_TIME`, отказ песочницы — 3) для текста ошибки; сигнал — как в оболочке,
        // 128 + номер (seccomp — `SIGSYS`, 159). Ждём завершения не дольше 500 мс, как на Windows и macOS.
        exit_code: Box::new(move || {
            let mut c = watched.lock().unwrap_or_else(PoisonError::into_inner);
            let until = Instant::now() + Duration::from_millis(500);
            loop {
                match c.try_wait() {
                    Ok(Some(st)) => return st.code().map(|v| v as u32).or_else(|| st.signal().map(|s| 128 + s as u32)),
                    Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(10)),
                    _ => return None,
                }
            }
        }),
        // Без полной песочницы воркер не работает (`isolate`/`lockdown` → выход 3), поэтому ответивший воркер — в полной
        // изоляции.
        level: Level::Full,
        fallback: None,
    })
}

/// Режим namespaces из аргумента `--namespaces=…`; без аргумента — [`Namespaces::Optional`], неизвестное значение — отказ.
fn namespaces_mode() -> Result<Namespaces, String> {
    let arg = std::env::args_os().find_map(|a| a.to_str()?.strip_prefix("--namespaces=").map(str::to_owned));
    match arg.as_deref() {
        None => Ok(Namespaces::default()),
        Some(v) => Namespaces::from_arg(v).ok_or_else(|| format!("неизвестный режим namespaces: {v}")),
    }
}

/// Отдельные user/net/ipc namespaces: без сети и без System V IPC даже при ошибке в остальных механизмах.
/// Вызывается, пока процесс однопоточный. Недоступны: `required` — отказ с причиной, `optional` — WARN в журнал.
pub(crate) fn isolate() -> Result<(), String> {
    let mode = namespaces_mode()?;
    if dropped("userns") {
        return Ok(());
    }
    // Диагностика (отладочная сборка): как будто дистрибутив запретил namespaces — проверка обоих режимов на машине,
    // где они разрешены.
    // SAFETY: простой системный вызов без указателей.
    let err = if dropped("userns-deny") {
        io::Error::from_raw_os_error(libc::EPERM)
    } else if unsafe { libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNET | libc::CLONE_NEWIPC) } == 0 {
        return Ok(());
    } else {
        io::Error::last_os_error()
    };
    let sysctl = userns_sysctl();
    if mode == Namespaces::Required {
        let why = sysctl.map(|s| format!(": запрещены дистрибутивом ({s})")).unwrap_or_default();
        return Err(format!("user namespaces недоступны ({err}){why}, а режим namespaces — required"));
    }
    // Журнал — по-английски, без содержимого; stderr воркера унаследован от приложения.
    eprintln!(
        "WARN postman-worker: user namespaces unavailable ({err}; {}), running with Landlock + seccomp + rlimits only",
        sysctl.unwrap_or("no sysctl forbids them")
    );
    Ok(())
}

/// Какой sysctl дистрибутива запрещает user namespaces (читается до запирания).
fn userns_sysctl() -> Option<&'static str> {
    let read = |p: &str| std::fs::read_to_string(p).ok().map(|s| s.trim().to_owned());
    [
        ("/proc/sys/kernel/apparmor_restrict_unprivileged_userns", "1", "kernel.apparmor_restrict_unprivileged_userns = 1"),
        ("/proc/sys/kernel/unprivileged_userns_clone", "0", "kernel.unprivileged_userns_clone = 0"),
        ("/proc/sys/user/max_user_namespaces", "0", "user.max_user_namespaces = 0"),
    ]
    .into_iter()
    .find(|(path, bad, _)| read(path).as_deref() == Some(*bad))
    .map(|(_, _, name)| name)
}

pub(crate) fn lockdown() -> Result<(), String> {
    // `args_os`: `args` упал бы на аргументе не в UTF-8.
    let memory = std::env::args_os().find_map(|a| a.to_str()?.strip_prefix("--memory=")?.parse::<u64>().ok()).unwrap_or(MEMORY_LIMIT as u64);
    if !dropped("rlimits") {
        set_rlimits(memory)?;
    }
    if !dropped("landlock") {
        landlock()?;
    }
    // SAFETY: простой prctl без указателей.
    if !dropped("nnp") && unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(format!("no_new_privs: {}", io::Error::last_os_error()));
    }
    if dropped("seccomp") {
        return Ok(());
    }
    seccompiler::apply_filter(&filter()?).map_err(|e| format!("seccomp: {e}"))
}

fn set_rlimits(memory: u64) -> Result<(), String> {
    let set = |res, name: &str, v: u64| {
        let lim = libc::rlimit { rlim_cur: v, rlim_max: v };
        // SAFETY: передаём указатель на локальную структуру.
        if unsafe { libc::setrlimit(res, &lim) } != 0 {
            return Err(format!("setrlimit({name}): {}", io::Error::last_os_error()));
        }
        Ok(())
    };
    set(libc::RLIMIT_AS, "RLIMIT_AS", memory)?;
    set(libc::RLIMIT_CORE, "RLIMIT_CORE", 0)?;
    set(libc::RLIMIT_FSIZE, "RLIMIT_FSIZE", 0)?;
    set(libc::RLIMIT_NOFILE, "RLIMIT_NOFILE", 16)?;
    set(libc::RLIMIT_NPROC, "RLIMIT_NPROC", 0)?;
    set(libc::RLIMIT_MEMLOCK, "RLIMIT_MEMLOCK", 0)?;
    set(libc::RLIMIT_MSGQUEUE, "RLIMIT_MSGQUEUE", 0)
}

/// Запрет любого доступа к ФС, TCP (bind/connect), абстрактных unix-сокетов и сигналов вне процесса — всё, что знает ядро
/// до [`LANDLOCK_ABI`]; правил-исключений нет. Ядро без Landlock — отказ с причиной.
fn landlock() -> Result<(), String> {
    use landlock::{Access, AccessFs, AccessNet, LandlockStatus, Ruleset, RulesetAttr, RulesetStatus, Scope};
    let status = Ruleset::default()
        .handle_access(AccessFs::from_all(LANDLOCK_ABI))
        .and_then(|r| r.handle_access(AccessNet::from_all(LANDLOCK_ABI)))
        .and_then(|r| r.scope(Scope::from_all(LANDLOCK_ABI)))
        .and_then(|r| r.create())
        .and_then(|r| r.restrict_self())
        .map_err(|e| format!("landlock: {e}"))?;
    if status.ruleset != RulesetStatus::NotEnforced {
        return Ok(());
    }
    Err(match status.landlock {
        LandlockStatus::NotEnabled => "landlock: выключен в ядре (нет в параметре загрузки lsm=)".into(),
        LandlockStatus::NotImplemented => "landlock: ядро собрано без него (нужно 5.13+ с CONFIG_SECURITY_LANDLOCK)".into(),
        LandlockStatus::Available { .. } => "landlock: правила не применены".into(),
    })
}

fn filter() -> Result<BpfProgram, String> {
    let cond = |arg, op| SeccompCondition::new(arg, SeccompCmpArgLen::Qword, op, 0).map_err(|e| e.to_string());
    let rule = |c: Vec<SeccompCondition>| SeccompRule::new(c).map_err(|e| e.to_string());
    let eq = |arg, v: u64| -> Result<SeccompCondition, String> {
        SeccompCondition::new(arg, SeccompCmpArgLen::Qword, SeccompCmpOp::Eq, v).map_err(|e| e.to_string())
    };
    // Ни одной исполняемой страницы: JIT и подгрузка кода невозможны.
    let no_exec = || -> Result<Vec<SeccompRule>, String> { Ok(vec![rule(vec![cond(2, SeccompCmpOp::MaskedEq(libc::PROT_EXEC as u64))?])?]) };
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = [
        libc::SYS_close,
        libc::SYS_brk,
        libc::SYS_munmap,
        libc::SYS_mremap,
        libc::SYS_madvise,
        libc::SYS_futex,
        libc::SYS_rt_sigreturn,
        libc::SYS_rt_sigprocmask,
        libc::SYS_sigaltstack,
        libc::SYS_clock_gettime,
        libc::SYS_getrandom,
        libc::SYS_sched_yield,
        libc::SYS_exit,
        libc::SYS_exit_group,
        // abort() при панике: raise(SIGABRT) самому себе (`tgkill` — только своему процессу, ниже).
        libc::SYS_getpid,
        libc::SYS_gettid,
    ]
    .into_iter()
    .map(|s| (s, vec![]))
    .collect();
    rules.insert(libc::SYS_mmap, no_exec()?);
    rules.insert(libc::SYS_mprotect, no_exec()?);
    // Ввод — только кадры клиента (stdin), вывод — ответы (stdout) и журнал (stderr): других дескрипторов у воркера нет.
    rules.insert(libc::SYS_read, vec![rule(vec![eq(0, 0)?])?]);
    for sys in [libc::SYS_write, libc::SYS_writev] {
        rules.insert(sys, vec![rule(vec![eq(0, 1)?])?, rule(vec![eq(0, 2)?])?]);
    }
    // SAFETY: getpid без аргументов. После CLONE_NEWUSER pid тот же (pid namespace не меняется).
    let pid = unsafe { libc::getpid() };
    rules.insert(libc::SYS_tgkill, vec![rule(vec![eq(0, pid as u64)?])?]);
    let arch = std::env::consts::ARCH.try_into().map_err(|e| format!("seccomp arch: {e:?}"))?;
    SeccompFilter::new(rules, SeccompAction::KillProcess, SeccompAction::Allow, arch)
        .and_then(BpfProgram::try_from)
        .map_err(|e| format!("seccomp: {e}"))
}
