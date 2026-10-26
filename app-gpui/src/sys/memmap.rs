//! Разбивка памяти процесса (S4-08, `docs/MEMORY.md`): обход адресного пространства (`VirtualQuery`) и рабочего
//! набора (`QueryWorkingSet`) — как VMMap, без прав администратора и сторонних программ.
//!
//! Категории: образы (exe и DLL — по модулям), отображённые файлы (по файлам), частная память (куча, стеки,
//! выделения драйвера и D3D) — по блокам выделения. На каждую — выделено (commit), в рабочем наборе и частное
//! в рабочем наборе (столбец «Память» диспетчера задач считает только частное).

use std::collections::HashMap;
use std::ffi::c_void;
use std::fmt::Write as _;

use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_IMAGE, MEM_MAPPED, MEM_PRIVATE, MEMORY_BASIC_INFORMATION, PAGE_GUARD, VirtualQuery};
use windows_sys::Win32::System::ProcessStatus::{GetMappedFileNameW, QueryWorkingSet};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

const PAGE: usize = 4096;

/// Страницы рабочего набора: номер страницы → частная ли (не разделяется с другими процессами).
fn working_set() -> HashMap<usize, bool> {
    // Буфер: NumberOfEntries, затем записи; при нехватке места QueryWorkingSet пишет нужное число записей.
    let mut buf: Vec<usize> = vec![0; 1 << 16];
    loop {
        let bytes = (buf.len() * size_of::<usize>()) as u32;
        // SAFETY: буфер наш, размер в байтах передан; псевдодескриптор текущего процесса закрывать не нужно.
        let ok = unsafe { QueryWorkingSet(GetCurrentProcess(), buf.as_mut_ptr().cast::<c_void>(), bytes) } != 0;
        let n = buf[0];
        if ok {
            // Запись: номер страницы в старших битах, флаги — в младших 12; бит 8 — страница разделяемая.
            return buf[1..=n.min(buf.len() - 1)].iter().map(|e| (e >> 12, e & (1 << 8) == 0)).collect();
        }
        if n + 1024 <= buf.len() {
            return HashMap::new();
        }
        buf = vec![0; n + 4096];
    }
}

fn mapped_name(base: *const c_void) -> String {
    let mut name = [0u16; 520];
    // SAFETY: буфер наш, длина в символах передана.
    let len = unsafe { GetMappedFileNameW(GetCurrentProcess(), base, name.as_mut_ptr(), name.len() as u32) } as usize;
    if len == 0 {
        return "(section)".into();
    }
    let path = String::from_utf16_lossy(&name[..len]);
    path.rsplit('\\').next().unwrap_or(&path).to_string()
}

/// Итог по одной группе (модуль, файл, блок выделения).
#[derive(Default, Clone)]
struct Sum {
    commit: usize,
    ws: usize,
    private_ws: usize,
}

impl Sum {
    fn add(&mut self, o: &Sum) {
        self.commit += o.commit;
        self.ws += o.ws;
        self.private_ws += o.private_ws;
    }
}

fn mb(b: usize) -> f64 {
    b as f64 / 1_048_576.0
}

/// Отчёт (по строке на категорию и крупнейшие группы в ней) и частный рабочий набор, байты. Обход всего
/// адресного пространства — только для замера.
pub fn report() -> (String, usize) {
    let ws = working_set();
    let mut groups: [HashMap<String, Sum>; 3] = Default::default();
    let mut addr = 0usize;
    loop {
        // SAFETY: структура из целых чисел и указателей — нули допустимы.
        let mut mbi: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: структура наша, размер передан; адрес — любой, VirtualQuery сам проверяет.
        let got = unsafe { VirtualQuery(addr as *const c_void, &mut mbi, size_of::<MEMORY_BASIC_INFORMATION>()) };
        if got == 0 {
            break;
        }
        let (base, size) = (mbi.BaseAddress as usize, mbi.RegionSize);
        if mbi.State == MEM_COMMIT && mbi.Protect & PAGE_GUARD == 0 {
            let (kind, name) = match mbi.Type {
                MEM_IMAGE => (0, mapped_name(mbi.AllocationBase)),
                MEM_MAPPED => (1, mapped_name(mbi.AllocationBase)),
                MEM_PRIVATE => (2, format!("{:#x}", mbi.AllocationBase as usize)),
                _ => (2, "?".into()),
            };
            let mut s = Sum { commit: size, ..Default::default() };
            for page in (base / PAGE)..((base + size) / PAGE) {
                if let Some(&private) = ws.get(&page) {
                    s.ws += PAGE;
                    if private {
                        s.private_ws += PAGE;
                    }
                }
            }
            groups[kind].entry(name).or_default().add(&s);
        }
        match base.checked_add(size) {
            Some(next) if next > addr => addr = next,
            _ => break,
        }
    }
    let mut out = String::new();
    let mut total = Sum::default();
    for (kind, title) in ["image", "mapped", "private"].iter().enumerate() {
        let mut v: Vec<(&String, &Sum)> = groups[kind].iter().collect();
        let mut t = Sum::default();
        for (_, s) in &v {
            t.add(s);
        }
        total.add(&t);
        let _ = writeln!(out, "mem {title}: commit={:.1} ws={:.1} private_ws={:.1} groups={}", mb(t.commit), mb(t.ws), mb(t.private_ws), v.len());
        v.sort_by_key(|(_, s)| std::cmp::Reverse(s.ws));
        for (name, s) in v.into_iter().take(12) {
            let _ = writeln!(out, "mem   {title} {name}: commit={:.1} ws={:.1} private_ws={:.1}", mb(s.commit), mb(s.ws), mb(s.private_ws));
        }
    }
    let _ = writeln!(
        out,
        "mem total: commit={:.1} ws={:.1} private_ws={:.1} ws_pages={}",
        mb(total.commit),
        mb(total.ws),
        mb(total.private_ws),
        ws.len()
    );
    (out, total.private_ws)
}
