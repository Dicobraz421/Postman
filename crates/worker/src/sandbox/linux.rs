//! Linux (LIN-04, ADR 0010): user/net/ipc namespaces, rlimits, Landlock (ФС, сеть, scopes), `no_new_privs`,
//! seccomp-bpf со списком разрешённых вызовов. Без любого из механизмов воркер не работает: `isolate`/`lockdown` вернут
//! причину, клиент покажет её текстом («песочница: …»), процесс выйдет с кодом 3.
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

use super::Level;
use crate::Process;

/// Предел адресного пространства воркера (`RLIMIT_AS`), если клиент не передал свой.
pub(crate) const MEMORY_LIMIT: usize = 1 << 30;

/// Старший ABI Landlock, правила которого ставим: ФС (V1–V5), сеть TCP (V4), scopes — абстрактные сокеты и сигналы
/// (V6). На ядре старше — эти же правила, на младшем — что ядро умеет (ФС закрывает уже V1, ядро 5.13).
const LANDLOCK_ABI: landlock::ABI = landlock::ABI::V6;

/// Диагностика (только отладочные сборки, по образцу `POSTMAN_SANDBOX_DROP` на Windows): `userns,rlimits,landlock,nnp,
/// seccomp` — снять перечисленные механизмы, `all` — все. Так видно, какой запрет мешает воркеру стартовать
/// (`tests/linux_probe.rs`). В release переменная не читается и не передаётся — ослабить песочницу снаружи нельзя.
const DROP: &str = "POSTMAN_SANDBOX_DROP";

fn dropped(what: &str) -> bool {
    cfg!(debug_assertions) && std::env::var(DROP).is_ok_and(|v| v.split(',').map(str::trim).any(|d| d == what || d == "all"))
}

/// `_relaxed` не используется: ослабленного режима нет. `memory` — `RLIMIT_AS` процесса: уходит аргументом `--memory=…`,
/// окружение очищено.
pub(crate) fn spawn(exe: &Path, _relaxed: bool, memory: usize) -> io::Result<Process> {
    let mut cmd = Command::new(exe);
    cmd.env_clear().current_dir("/").arg(format!("--memory={memory}")).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
    if cfg!(debug_assertions)
        && let Some(drop) = std::env::var_os(DROP)
    {
        cmd.env(DROP, drop);
    }
    // SAFETY: в pre_exec — только async-signal-safe prctl.
    unsafe {
        cmd.pre_exec(|| {
            // Клиент упал — воркер не переживает его.
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn()?;
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(io::Error::other("worker pipes are missing"));
    };
    let child = Arc::new(Mutex::new(child));
    let watched = Arc::clone(&child);
    Ok(Process {
        stdin: Box::new(stdin),
        stdout: Box::new(stdout),
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

/// Отдельные user/net/ipc namespaces: без сети и без System V IPC даже при ошибке в остальных механизмах.
/// Вызывается, пока процесс однопоточный.
pub(crate) fn isolate() -> Result<(), String> {
    if dropped("userns") {
        return Ok(());
    }
    // SAFETY: простой системный вызов без указателей.
    if unsafe { libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNET | libc::CLONE_NEWIPC) } == 0 {
        return Ok(());
    }
    let os = io::Error::last_os_error();
    Err(format!("user namespaces недоступны ({os}){}", userns_reason()))
}

/// Какой запрет дистрибутива мешает user namespaces (читается до запирания).
fn userns_reason() -> String {
    let read = |p: &str| std::fs::read_to_string(p).ok().map(|s| s.trim().to_owned());
    if read("/proc/sys/kernel/apparmor_restrict_unprivileged_userns").as_deref() == Some("1") {
        return ": запрещены AppArmor (kernel.apparmor_restrict_unprivileged_userns = 1)".into();
    }
    if read("/proc/sys/kernel/unprivileged_userns_clone").as_deref() == Some("0") {
        return ": запрещены ядром (kernel.unprivileged_userns_clone = 0)".into();
    }
    if read("/proc/sys/user/max_user_namespaces").as_deref() == Some("0") {
        return ": запрещены ядром (user.max_user_namespaces = 0)".into();
    }
    String::new()
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
