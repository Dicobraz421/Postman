//! Windows: процесс создаётся приостановленным в AppContainer (без сетевых
//! capability и доступа к профилю пользователя), с mitigation policies на
//! старте (win32k off, только Microsoft-DLL, без динамического кода, без
//! дочерних процессов), в Job Object (kill-on-close, 1 процесс, лимит памяти,
//! UI-ограничения), и только потом запускается. Хост модулей просмотра
//! (`postman-viewer`, S8-24) — так же, с пределом памяти 512 МиБ.
//!
//! Exe воркера должен быть читаем для AppContainer (ALL APPLICATION PACKAGES):
//! так по умолчанию в Program Files. Если запуск в AppContainer не удался или
//! воркер в нём не ответил (например, сборка в профиле пользователя), он
//! запускается без AppContainer с остальными ограничениями, уровень — `Partial`.

use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;
use std::path::Path;
use std::ptr::{null, null_mut};
use std::sync::Arc;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_OBJECT_0};
use windows_sys::Win32::Security::Isolation::{CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName};
use windows_sys::Win32::Security::{FreeSid, PSID, SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOBOBJECT_BASIC_UI_RESTRICTIONS, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicUIRestrictions, JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DETACHED_PROCESS, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
    GetExitCodeProcess, InitializeProcThreadAttributeList, PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, ProcessSystemCallDisablePolicy,
    ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, SetProcessMitigationPolicy, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};

use windows_sys::Win32::System::SystemServices::{JOB_OBJECT_UILIMIT_ALL, PROCESS_MITIGATION_SYSTEM_CALL_DISABLE_POLICY};
use windows_sys::Win32::System::WindowsProgramming::PROCESS_CREATION_CHILD_PROCESS_RESTRICTED;

use super::Level;
use crate::Process;

const PROFILE: &str = "Postman.MailWorker";
/// Предел памяти воркера; у хоста модулей — свой (`wasm::JOB_MEMORY`).
pub(crate) const MEMORY_LIMIT: usize = 1 << 30;

// PROCESS_CREATION_MITIGATION_POLICY_* (winbase.h), значения *_ALWAYS_ON.
const DEP_ENABLE: u64 = 0x01;
const HEAP_TERMINATE: u64 = 1 << 12;
const BOTTOM_UP_ASLR: u64 = 1 << 16;
const HIGH_ENTROPY_ASLR: u64 = 1 << 20;
const STRICT_HANDLE_CHECKS: u64 = 1 << 24;
const WIN32K_SYSTEM_CALL_DISABLE: u64 = 1 << 28;
const EXTENSION_POINT_DISABLE: u64 = 1 << 32;
const PROHIBIT_DYNAMIC_CODE: u64 = 1 << 36;
const BLOCK_NON_MICROSOFT_BINARIES: u64 = 1 << 44;
const FONT_DISABLE: u64 = 1 << 48;
const IMAGE_LOAD_NO_REMOTE: u64 = 1 << 52;
const IMAGE_LOAD_NO_LOW_LABEL: u64 = 1 << 56;
const IMAGE_LOAD_PREFER_SYSTEM32: u64 = 1 << 60;

const MITIGATIONS: u64 = DEP_ENABLE
    | HEAP_TERMINATE
    | BOTTOM_UP_ASLR
    | HIGH_ENTROPY_ASLR
    | STRICT_HANDLE_CHECKS
    | WIN32K_SYSTEM_CALL_DISABLE
    | EXTENSION_POINT_DISABLE
    | PROHIBIT_DYNAMIC_CODE
    | BLOCK_NON_MICROSOFT_BINARIES
    | FONT_DISABLE
    | IMAGE_LOAD_NO_REMOTE
    | IMAGE_LOAD_NO_LOW_LABEL
    | IMAGE_LOAD_PREFER_SYSTEM32;

/// Диагностика (только отладочные сборки): `POSTMAN_SANDBOX_DROP=win32k,dyncode,…` снимает
/// перечисленные ограничения, чтобы найти то, из-за которого воркер не стартует.
/// В release переменная не читается — ослабить песочницу снаружи нельзя.
fn dropped(name: &str) -> bool {
    cfg!(debug_assertions) && std::env::var("POSTMAN_SANDBOX_DROP").is_ok_and(|v| v.split(',').any(|x| x.trim() == name || x.trim() == "all"))
}

fn mitigations() -> u64 {
    let off = [
        ("dep", DEP_ENABLE),
        ("heap", HEAP_TERMINATE),
        ("aslr", BOTTOM_UP_ASLR | HIGH_ENTROPY_ASLR),
        ("strict", STRICT_HANDLE_CHECKS),
        ("win32k", WIN32K_SYSTEM_CALL_DISABLE),
        ("ext", EXTENSION_POINT_DISABLE),
        ("dyncode", PROHIBIT_DYNAMIC_CODE),
        ("msonly", BLOCK_NON_MICROSOFT_BINARIES),
        ("font", FONT_DISABLE),
        ("imageload", IMAGE_LOAD_NO_REMOTE | IMAGE_LOAD_NO_LOW_LABEL | IMAGE_LOAD_PREFER_SYSTEM32),
    ];
    off.iter().filter(|(n, _)| dropped(n)).fold(MITIGATIONS, |m, (_, bits)| m & !bits)
}

/// Закрывает HANDLE при выходе из области видимости.
struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: хэндл принадлежит нам и закрывается один раз.
            unsafe { CloseHandle(self.0) };
        }
    }
}

impl Owned {
    fn terminate(&self) {
        // SAFETY: хэндл процесса валиден, пока жив `self`.
        unsafe { TerminateProcess(self.0, 1) };
    }
}

// SAFETY: HANDLE процесса/джоба можно использовать из любого потока.
unsafe impl Send for Owned {}
// SAFETY: через общий хэндл процесса вызываются только потокобезопасные функции ядра
// (TerminateProcess, WaitForSingleObject, GetExitCodeProcess).
unsafe impl Sync for Owned {}

fn last_error() -> io::Error {
    io::Error::last_os_error()
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// `relaxed` — сразу без AppContainer (строгий запуск уже не дал воркеру работать); `memory` — предел памяти процесса
/// (Job Object).
pub(crate) fn spawn(exe: &Path, relaxed: bool, memory: usize) -> io::Result<Process> {
    if relaxed {
        // Причину (воркер не ответил в AppContainer) знает клиент.
        return create(exe, None, memory);
    }
    if dropped("appcontainer") {
        return partial(exe, "POSTMAN_SANDBOX_DROP=appcontainer".into(), memory);
    }
    let sid = match app_container_sid() {
        Ok(sid) => sid,
        Err(hr) => return partial(exe, format!("профиль AppContainer: HRESULT 0x{hr:08X}"), memory),
    };
    match create_in_container(exe, &sid, memory) {
        Ok(mut p) => {
            p.level = Level::Full;
            Ok(p)
        }
        // Любой отказ AppContainer — запуск с остальными ограничениями; причина — в `fallback` и в ошибке, если и он
        // не удался.
        Err(strict) => partial(exe, format!("запуск в AppContainer: {strict}"), memory)
            .map_err(|e| io::Error::new(e.kind(), format!("{e}; в AppContainer: {strict}"))),
    }
}

/// Сколько ждём запуска в AppContainer при «файл не найден» (и ERROR_GEN_FAILURE) и шаг повтора (см. [`create_in_container`]).
const CONTAINER_WAIT: std::time::Duration = std::time::Duration::from_secs(1);
const CONTAINER_STEP: std::time::Duration = std::time::Duration::from_millis(20);

/// Запуск в AppContainer. `CreateProcessW` с атрибутом AppContainer изредка отвечает ERROR_FILE_NOT_FOUND (2), хотя exe
/// на месте и следующая попытка через 20–80 мс удаётся: так ОС ведёт себя, когда процессы AppContainer одного профиля
/// создаются и завершаются параллельно (в том числе из разных процессов) — каталог объектов контейнера
/// (`\Sessions\N\AppContainerNamedObjects\<SID>`) в этот момент пересоздаётся. Воспроизведение: тест `smime.rs`
/// (три клиента параллельно) — 3 из 40 прогонов, вместе с `sandbox.rs` — 3 из 30; мьютекс на свои запуски и маркер
/// контейнера, удерживаемый родителем, окно не закрывают. Ошибка временная и не наша — повторяем до [`CONTAINER_WAIT`],
/// прежде чем уйти в частичную изоляцию (с повтором — 0 из 40 параллельных прогонов). Так же повторяем
/// ERROR_GEN_FAILURE (31): та же гонка на runner CI (`verdicts_through_the_sandbox`, #88).
fn create_in_container(exe: &Path, sid: &Sid, memory: usize) -> io::Result<Process> {
    let start = std::time::Instant::now();
    let mut attempt = 1u32;
    loop {
        match create(exe, Some(sid.0), memory) {
            Err(e) if start.elapsed() < CONTAINER_WAIT && matches!(e.raw_os_error(), Some(2 | 3 | 31)) => {
                std::thread::sleep(CONTAINER_STEP);
                attempt += 1;
            }
            Err(e) if attempt > 1 => return Err(io::Error::new(e.kind(), format!("{e} (попыток: {attempt})"))),
            other => return other,
        }
    }
}

/// Запуск без AppContainer с остальными ограничениями; `reason` — почему.
fn partial(exe: &Path, reason: String, memory: usize) -> io::Result<Process> {
    let mut p = create(exe, None, memory)?;
    p.fallback = Some(reason);
    Ok(p)
}

/// Блок окружения воркера: только системные пути, никаких секретов родителя.
/// Совсем пустой блок Windows не принимает (ERROR_ENVVAR_NOT_FOUND, 203), а для
/// AppContainer ей нужны пути профиля: TEMP, TMP и LOCALAPPDATA она подменяет на каталог контейнера.
fn environment() -> Vec<u16> {
    const KEEP: &[&str] = &["SystemRoot", "windir", "SystemDrive", "USERPROFILE", "LOCALAPPDATA", "APPDATA", "TEMP", "TMP"];
    let mut env = Vec::new();
    for k in KEEP {
        let Some(v) = std::env::var_os(k) else { continue };
        env.extend(k.encode_utf16());
        env.push(u16::from(b'='));
        env.extend(v.encode_wide());
        env.push(0);
    }
    if env.is_empty() {
        env.extend("SystemRoot=C:\\Windows".encode_utf16());
        env.push(0);
    }
    env.push(0);
    env
}

struct Sid(PSID);

impl Drop for Sid {
    fn drop(&mut self) {
        // SAFETY: SID получен из userenv и освобождается один раз.
        unsafe { FreeSid(self.0) };
    }
}

/// SID профиля AppContainer воркера (создаётся при первом запуске); ошибка — HRESULT.
fn app_container_sid() -> Result<Sid, u32> {
    let name = wide(PROFILE);
    let display = wide("Postman: разбор писем");
    let mut sid: PSID = null_mut();
    // SAFETY: строки нуль-терминированы; sid — выходной параметр.
    let hr = unsafe { CreateAppContainerProfile(name.as_ptr(), display.as_ptr(), display.as_ptr(), null(), 0, &mut sid) };
    if hr >= 0 {
        return Ok(Sid(sid));
    }
    // Профиль уже есть (HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS)) — берём его SID.
    // SAFETY: см. выше.
    let hr = unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid) };
    if hr >= 0 { Ok(Sid(sid)) } else { Err(hr as u32) }
}

/// Пара анонимных каналов; конец для дочернего процесса наследуемый.
fn pipe(child_reads: bool) -> io::Result<(Owned, Owned)> {
    let sa = SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: null_mut(), bInheritHandle: 1 };
    let (mut r, mut w): (HANDLE, HANDLE) = (null_mut(), null_mut());
    // SAFETY: выходные параметры — локальные переменные.
    if unsafe { CreatePipe(&mut r, &mut w, &sa, 0) } == 0 {
        return Err(last_error());
    }
    let (r, w) = (Owned(r), Owned(w));
    let (child, parent) = if child_reads { (r, w) } else { (w, r) };
    // SAFETY: хэндл валиден; снимаем наследование с родительского конца.
    if unsafe { SetHandleInformation(parent.0, HANDLE_FLAG_INHERIT, 0) } == 0 {
        return Err(last_error());
    }
    Ok((child, parent))
}

fn job(memory: usize) -> io::Result<Owned> {
    // SAFETY: безымянный джоб с атрибутами по умолчанию.
    let job = Owned(unsafe { CreateJobObjectW(null(), null()) });
    if job.0.is_null() {
        return Err(last_error());
    }
    // SAFETY: структура полностью инициализирована нулями, дальше задаём поля.
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
        | JOB_OBJECT_LIMIT_PROCESS_MEMORY
        | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
    limits.BasicLimitInformation.ActiveProcessLimit = 1;
    limits.ProcessMemoryLimit = memory;
    let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS { UIRestrictionsClass: if dropped("job_ui") { 0 } else { JOB_OBJECT_UILIMIT_ALL } };
    // SAFETY: указатели на локальные структуры правильного размера.
    let ok = unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) != 0
            && SetInformationJobObject(
                job.0,
                JobObjectBasicUIRestrictions,
                (&raw const ui).cast(),
                size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>() as u32,
            ) != 0
    };
    if !ok {
        return Err(last_error());
    }
    Ok(job)
}

/// Список атрибутов процесса; память и значения живут, пока жив список.
struct Attributes {
    buf: Vec<usize>,
}

impl Attributes {
    fn new(count: u32) -> io::Result<Self> {
        let mut size = 0usize;
        // SAFETY: первый вызов только узнаёт размер (ожидаемо возвращает ошибку).
        unsafe { InitializeProcThreadAttributeList(null_mut(), count, 0, &mut size) };
        let mut buf = vec![0usize; size.div_ceil(size_of::<usize>())];
        // SAFETY: буфер нужного размера и выравнивания.
        if unsafe { InitializeProcThreadAttributeList(buf.as_mut_ptr().cast(), count, 0, &mut size) } == 0 {
            return Err(last_error());
        }
        Ok(Self { buf })
    }

    fn ptr(&mut self) -> *mut c_void {
        self.buf.as_mut_ptr().cast()
    }

    /// # Safety
    /// `value` должен жить до `CreateProcessW`.
    unsafe fn set(&mut self, attr: u32, value: *const c_void, size: usize) -> io::Result<()> {
        // SAFETY: обязанность вызывающего — время жизни `value`.
        if unsafe { UpdateProcThreadAttribute(self.ptr(), 0, attr as usize, value, size, null_mut(), null()) } == 0 {
            return Err(last_error());
        }
        Ok(())
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: список был инициализирован в `new`.
        unsafe { DeleteProcThreadAttributeList(self.ptr()) };
    }
}

fn create(exe: &Path, sid: Option<PSID>, memory: usize) -> io::Result<Process> {
    let (child_in, parent_in) = pipe(true)?;
    let (child_out, parent_out) = pipe(false)?;
    let job = job(memory)?;

    let mitigations: u64 = mitigations();
    let child_policy: u32 = PROCESS_CREATION_CHILD_PROCESS_RESTRICTED;
    let handles: [HANDLE; 2] = [child_in.0, child_out.0];
    let caps = sid.map(|sid| SECURITY_CAPABILITIES { AppContainerSid: sid, Capabilities: null_mut(), CapabilityCount: 0, Reserved: 0 });
    let mut attrs = Attributes::new(if caps.is_some() { 4 } else { 3 })?;
    // SAFETY: все значения — локальные переменные, живущие до CreateProcessW.
    unsafe {
        attrs.set(PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY, (&raw const mitigations).cast(), size_of::<u64>())?;
        if !dropped("childproc") {
            attrs.set(PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY, (&raw const child_policy).cast(), size_of::<u32>())?;
        }
        attrs.set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, handles.as_ptr().cast(), size_of_val(&handles))?;
        if let Some(c) = &caps {
            attrs.set(PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, (c as *const SECURITY_CAPABILITIES).cast(), size_of::<SECURITY_CAPABILITIES>())?;
        }
    }

    // SAFETY: обнулённая структура — допустимое начальное состояние.
    let mut si: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    si.StartupInfo.hStdInput = child_in.0;
    si.StartupInfo.hStdOutput = child_out.0;
    si.StartupInfo.hStdError = null_mut();
    si.lpAttributeList = attrs.ptr();

    let app: Vec<u16> = exe.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut cmdline: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain(Some(0)).collect();
    let env = environment();
    let cwd = wide("C:\\");
    // SAFETY: обнулённая выходная структура.
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: все строки нуль-терминированы; список атрибутов инициализирован;
    // блок окружения в UTF-16 с двойным нулём в конце.
    let ok = unsafe {
        CreateProcessW(
            app.as_ptr(),
            cmdline.as_mut_ptr(),
            null(),
            null(),
            1,
            // Без консоли вообще: для консольного процесса Windows запускает conhost.exe,
            // а запрет дочерних процессов это блокирует — воркер падал с 0xC0000142.
            // Ввод-вывод идёт по каналам, консоль не нужна.
            CREATE_SUSPENDED | DETACHED_PROCESS | EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            env.as_ptr().cast(),
            cwd.as_ptr(),
            &si.StartupInfo,
            &mut pi,
        )
    };
    if ok == 0 {
        return Err(last_error());
    }
    let process = Arc::new(Owned(pi.hProcess));
    let thread = Owned(pi.hThread);
    // SAFETY: хэндлы только что получены; процесс ещё не исполнял код.
    unsafe {
        if !dropped("job") && AssignProcessToJobObject(job.0, process.0) == 0 {
            let e = last_error();
            TerminateProcess(process.0, 1);
            return Err(e);
        }
        if ResumeThread(thread.0) == u32::MAX {
            let e = last_error();
            TerminateProcess(process.0, 1);
            return Err(e);
        }
    }
    drop((child_in, child_out, thread));

    let into_file = |h: Owned| {
        let raw = h.0;
        std::mem::forget(h);
        // SAFETY: владение хэндлом передаётся File.
        unsafe { File::from_raw_handle(raw) }
    };
    let stdin = into_file(parent_in);
    let stdout = into_file(parent_out);
    let mut job = Some(job);
    let watched = process.clone();
    Ok(Process {
        stdin: Box::new(stdin),
        stdout: Box::new(stdout),
        kill: Box::new(move || {
            process.terminate();
            // Закрытие джоба (KILL_ON_JOB_CLOSE) добивает всё, что осталось.
            job.take();
        }),
        exit_code: Box::new(move || {
            let mut code = 0u32;
            // SAFETY: хэндл процесса жив, пока жив `watched`; ждём завершения не дольше 500 мс.
            let done = unsafe { WaitForSingleObject(watched.0, 500) } == WAIT_OBJECT_0 && unsafe { GetExitCodeProcess(watched.0, &mut code) } != 0;
            done.then_some(code)
        }),
        level: Level::Partial,
        fallback: None,
    })
}

/// Внутри воркера: повторно включить запрет win32k (если родитель не смог
/// задать его при создании). Остальное уже наложено родителем.
pub(crate) fn lockdown() -> Result<(), String> {
    // SAFETY: обнулённая политика с одним выставленным битом.
    let mut p: PROCESS_MITIGATION_SYSTEM_CALL_DISABLE_POLICY = unsafe { std::mem::zeroed() };
    // DisallowWin32kSystemCalls — младший бит.
    p.Anonymous.Flags = 1;
    // SAFETY: указатель на локальную структуру правильного размера.
    let ok = unsafe {
        SetProcessMitigationPolicy(ProcessSystemCallDisablePolicy, (&raw const p).cast(), size_of::<PROCESS_MITIGATION_SYSTEM_CALL_DISABLE_POLICY>())
            != 0
    };
    // Уже включено при создании процесса — тогда повторный вызов может вернуть ошибку; это не отказ.
    let _ = ok;
    Ok(())
}
