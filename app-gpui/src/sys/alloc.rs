//! Считающий распределитель (фича `alloc-count`, S4-08): сколько байт кучи держит наш код (Rust) сейчас и в пике.
//! Только для замера: каждая операция — лишние атомарные сложения. Куча C/C++ (DirectWrite, драйвер GPU) сюда не
//! попадает — её даёт разбивка адресного пространства (`memmap`).

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

pub struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static COUNT: AtomicUsize = AtomicUsize::new(0);

fn grow(n: usize) {
    let live = LIVE.fetch_add(n, Relaxed) + n;
    PEAK.fetch_max(live, Relaxed);
}

// SAFETY: все вызовы — в системный распределитель с теми же аргументами; сверху только счётчики.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: контракт `GlobalAlloc::alloc` передаётся как есть.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            grow(layout.size());
            COUNT.fetch_add(1, Relaxed);
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: контракт `GlobalAlloc::alloc_zeroed` передаётся как есть.
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            grow(layout.size());
            COUNT.fetch_add(1, Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: контракт `GlobalAlloc::dealloc` передаётся как есть.
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Relaxed);
        COUNT.fetch_sub(1, Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: контракт `GlobalAlloc::realloc` передаётся как есть.
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            if new_size >= layout.size() {
                grow(new_size - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - new_size, Relaxed);
            }
        }
        p
    }
}

/// Поля строки бенча: живые байты кучи Rust, пик, число живых блоков.
pub fn fields() -> String {
    let mb = |b: usize| b as f64 / 1_048_576.0;
    format!("heap_live_mb={:.1} heap_peak_mb={:.1} heap_blocks={}", mb(LIVE.load(Relaxed)), mb(PEAK.load(Relaxed)), COUNT.load(Relaxed))
}
