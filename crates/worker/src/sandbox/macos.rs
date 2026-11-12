//! macOS (MAC-01, `docs/decisions/2026-11-11-macos.md`): Seatbelt — `sandbox_init` с профилем «запрещено всё»
//! (`deny default`) в самом воркере, до первого запроса; плюс пределы `setrlimit` (без дампа памяти, ≤ 16 дескрипторов).
//! `RLIMIT_FSIZE = 0`, как на Linux, не ставится: stderr воркера унаследован от приложения и может быть файлом
//! журнала — запись в него убила бы воркер сигналом `SIGXFSZ`; новые файлы и так не открыть (Seatbelt).
//!
//! После запирания у воркера остаются только уже открытые stdin и stdout (каналы к клиенту): ни файлов, ни сети, ни
//! дочерних процессов, ни служб Mach, ни sysctl. Профиль ничего не открывает; понадобится открыть — только явным
//! правилом здесь, с причиной в комментарии, диагностикой и ADR (MAC-03).
//!
//! Предел памяти: `RLIMIT_AS` macOS не соблюдает. До MAC-03 (ограничивающий распределитель) от разрастания защищает
//! только сторож времени воркера.

use std::ffi::{CStr, c_char, c_int};
use std::io;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::ptr::null_mut;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::Level;
use crate::Process;

/// Предел памяти воркера (для `Client`; на macOS не применяется, см. выше).
pub(crate) const MEMORY_LIMIT: usize = 1 << 30;

/// Профиль Seatbelt (SBPL): запрещено всё, исключений нет.
const PROFILE: &CStr = c"(version 1)\n(deny default)\n";

// <sandbox.h>, <sys/resource.h> — libSystem, линкуется всегда.
unsafe extern "C" {
    fn sandbox_init(profile: *const c_char, flags: u64, errorbuf: *mut *mut c_char) -> c_int;
    fn sandbox_free_error(errorbuf: *mut c_char);
    fn setrlimit(resource: c_int, rlp: *const RLimit) -> c_int;
}

#[repr(C)]
struct RLimit {
    cur: u64,
    max: u64,
}

const RLIMIT_CORE: c_int = 4;
const RLIMIT_NOFILE: c_int = 8;

/// `_relaxed` и `_memory` не используются: ослабленного режима нет, предел памяти — MAC-03.
pub(crate) fn spawn(exe: &Path, _relaxed: bool, _memory: usize) -> io::Result<Process> {
    let mut cmd = Command::new(exe);
    cmd.env_clear().current_dir("/").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
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
        // Код выхода (сторож времени — `EXIT_TIME`) для текста ошибки; сигнал — как в оболочке, 128 + номер. Ждём
        // завершения не дольше 500 мс, как на Windows.
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
        // Без песочницы воркер не работает (`lockdown` → выход 3), поэтому ответивший воркер — в полной изоляции.
        level: Level::Full,
        fallback: None,
    })
}

pub(crate) fn lockdown() -> Result<(), String> {
    set_rlimits()?;
    let mut err: *mut c_char = null_mut();
    // SAFETY: профиль — строка C со статическим временем жизни; `err` — указатель на локальную переменную, строку
    // ошибки освобождает `sandbox_free_error`.
    let rc = unsafe { sandbox_init(PROFILE.as_ptr(), 0, &mut err) };
    if rc == 0 {
        return Ok(());
    }
    let text = if err.is_null() {
        format!("sandbox_init: {rc}")
    } else {
        // SAFETY: при ошибке `sandbox_init` кладёт в `err` строку C; копируем её и освобождаем один раз.
        let s = unsafe { CStr::from_ptr(err) }.to_string_lossy().into_owned();
        unsafe { sandbox_free_error(err) };
        format!("sandbox_init: {s}")
    };
    Err(text)
}

fn set_rlimits() -> Result<(), String> {
    let set = |res, v: u64| {
        let lim = RLimit { cur: v, max: v };
        // SAFETY: передаём указатель на локальную структуру.
        if unsafe { setrlimit(res, &lim) } != 0 {
            return Err(format!("setrlimit({res}): {}", io::Error::last_os_error()));
        }
        Ok(())
    };
    set(RLIMIT_CORE, 0)?;
    set(RLIMIT_NOFILE, 16)
}
