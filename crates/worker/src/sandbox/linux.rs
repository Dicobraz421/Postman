//! Linux: namespaces (если разрешены) + rlimits + Landlock + seccomp-bpf.

use std::collections::BTreeMap;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use seccompiler::{BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter, SeccompRule};

use super::Level;
use crate::Process;

/// Предел адресного пространства воркера.
const MEMORY_LIMIT: u64 = 1 << 30;

/// `_relaxed` на Linux не используется: ослабление решает сам воркер (namespaces — по возможности).
pub(crate) fn spawn(exe: &Path, _relaxed: bool) -> io::Result<Process> {
    let mut cmd = Command::new(exe);
    cmd.env_clear().current_dir("/").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
    // SAFETY: в pre_exec вызываются только async-signal-safe системные вызовы.
    unsafe {
        cmd.pre_exec(|| {
            // Отдельные user/net/ipc namespaces: без сети даже при ошибке в фильтре.
            // В контейнерах может быть запрещено — тогда остаются Landlock и seccomp.
            libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNET | libc::CLONE_NEWIPC);
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
    let mut child = cmd.spawn()?;
    let stdin = child.stdin.take().expect("piped");
    let stdout = child.stdout.take().expect("piped");
    Ok(Process {
        stdin: Box::new(stdin),
        stdout: Box::new(stdout),
        kill: Box::new(move || {
            let _ = child.kill();
            let _ = child.wait();
        }),
        exit_code: Box::new(|| None),
        // Фактический уровень сообщает сам воркер через отказ работать без seccomp;
        // namespaces — бонус, поэтому здесь считаем полную изоляцию.
        level: Level::Full,
        fallback: None,
    })
}

pub(crate) fn lockdown() -> Result<(), String> {
    set_rlimits()?;
    landlock()?;
    // SAFETY: простой prctl без указателей.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(format!("no_new_privs: {}", io::Error::last_os_error()));
    }
    seccompiler::apply_filter(&filter()?).map_err(|e| format!("seccomp: {e}"))
}

fn set_rlimits() -> Result<(), String> {
    let set = |res, v: u64| {
        let lim = libc::rlimit { rlim_cur: v, rlim_max: v };
        // SAFETY: передаём указатель на локальную структуру.
        if unsafe { libc::setrlimit(res, &lim) } != 0 {
            return Err(format!("setrlimit({res}): {}", io::Error::last_os_error()));
        }
        Ok(())
    };
    set(libc::RLIMIT_AS, MEMORY_LIMIT)?;
    set(libc::RLIMIT_CORE, 0)?;
    set(libc::RLIMIT_FSIZE, 0)?;
    set(libc::RLIMIT_NOFILE, 16)
}

/// Запрет любого доступа к файловой системе (best effort на старых ядрах).
fn landlock() -> Result<(), String> {
    use landlock::{ABI, Access, AccessFs, Ruleset, RulesetAttr};
    Ruleset::default()
        .handle_access(AccessFs::from_all(ABI::V5))
        .and_then(|r| r.create())
        .and_then(|r| r.restrict_self())
        .map(drop)
        .map_err(|e| format!("landlock: {e}"))
}

fn filter() -> Result<BpfProgram, String> {
    let no_exec = || -> Result<Vec<SeccompRule>, String> {
        let c = SeccompCondition::new(2, SeccompCmpArgLen::Dword, SeccompCmpOp::MaskedEq(libc::PROT_EXEC as u64), 0).map_err(|e| e.to_string())?;
        Ok(vec![SeccompRule::new(vec![c]).map_err(|e| e.to_string())?])
    };
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = [
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_writev,
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
        // abort() при панике: raise(SIGABRT) самому себе.
        libc::SYS_getpid,
        libc::SYS_gettid,
        libc::SYS_tgkill,
    ]
    .into_iter()
    .map(|s| (s, vec![]))
    .collect();
    rules.insert(libc::SYS_mmap, no_exec()?);
    rules.insert(libc::SYS_mprotect, no_exec()?);
    let arch = std::env::consts::ARCH.try_into().map_err(|e| format!("seccomp arch: {e:?}"))?;
    SeccompFilter::new(rules, SeccompAction::KillProcess, SeccompAction::Allow, arch)
        .and_then(BpfProgram::try_from)
        .map_err(|e| format!("seccomp: {e}"))
}
