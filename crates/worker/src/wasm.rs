//! Модули просмотра WASM (S8-24, ADR 0007, решение A): ответ модуля и его проверка — здесь, хост (`wasmi`) — только
//! в `postman-viewer.exe` (признак `viewer`); в `postman-worker.exe` интерпретатора нет.
//!
//! ABI v1 — ядро модели WASM без компонентов, импортов у модуля нет вовсе (ни WASI, ни своих функций):
//! - `pm_abi() -> i32` — версия ABI, [`ABI`];
//! - `pm_alloc(len: i32) -> i32` — место под вход длины `len` в памяти модуля (экспорт `memory`);
//! - `pm_view(ptr: i32, len: i32) -> i64` — просмотр входа; итог — указатель ответа в старших 32 битах и его длина в
//!   младших; ответ — [`ViewerOutput`] в postcard, не длиннее [`MAX_OUTPUT`].
//!
//! Пределы — на запрос: модуль — [`MAX_MODULE`], вход — [`MAX_INPUT`], память модуля — [`MAX_MEMORY`], топливо —
//! [`FUEL`], время процесса — сторож [`VIEW_LIMIT`]; Job Object процесса — [`JOB_MEMORY`]. Ответ — чужие данные:
//! разделы и блоки — в пределах `.docx`, ссылки — через проверку IR, картинка — не больше [`MAX_SIDE`] по стороне.

use std::time::Duration;

use postman_ir::{Block, Inline};
use serde::{Deserialize, Serialize};

use crate::preview::{self, Section, SectionTitle, docx, sheet, slides};

/// Версия ABI, которую понимает хост.
pub const ABI: u32 = 1;
/// Модуль больше — не загружаем.
pub const MAX_MODULE: usize = 16 * 1024 * 1024;
/// Вход модуля — как у встроенного просмотра.
pub const MAX_INPUT: usize = preview::MAX_INPUT;
/// Память модуля (`StoreLimits`): рост сверх — отказ.
pub const MAX_MEMORY: usize = 256 * 1024 * 1024;
/// Топливо на запрос (порядка 10⁹ инструкций).
pub const FUEL: u64 = 1_000_000_000;
/// Ответ модуля длиннее — отказ.
pub const MAX_OUTPUT: usize = 8 * 1024 * 1024;
/// Картинка больше по стороне — отказ.
pub const MAX_SIDE: u32 = 2048;
/// Разделов в ответе — не больше, чем слайдов у презентации.
pub const MAX_SECTIONS: usize = slides::MAX_SLIDES;
/// Вложенность блоков (цитаты, списки) — как у `.md`.
pub const MAX_NESTING: usize = preview::markdown::MAX_NESTING;
/// Ошибка модуля длиннее — обрезается.
pub const MAX_ERROR: usize = 1000;
/// Сторож времени процесса-хоста: запрос дольше — выход с [`crate::EXIT_TIME`].
pub const VIEW_LIMIT: Duration = Duration::from_secs(10);
/// Сколько клиент ждёт ответа хоста: сторож срабатывает раньше.
pub const VIEW_TIMEOUT: Duration = Duration::from_secs(12);
/// Предел памяти процесса-хоста (Job Object).
pub const JOB_MEMORY: usize = 512 * 1024 * 1024;

/// Ответ модуля (ABI v1, postcard). Типы — общие с SDK модулей (`viewers/sdk`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ViewerOutput {
    /// Документ разделами, как `Preview::Document`; `truncated` — показано начало.
    Sections { sections: Vec<Section>, truncated: bool },
    /// Картинка RGBA8 (`width × height × 4` байт); от хоста к клиенту — пиксели в бинарной части.
    Image { width: u32, height: u32, rgba: Vec<u8> },
    /// Модуль файл не показал; текст — от модуля, без управляющих символов, не длиннее [`MAX_ERROR`].
    Error(String),
}

/// Почему хост не показал файл модулем — данными: текст составляет интерфейс.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refusal {
    /// Модуль больше [`MAX_MODULE`] или вход больше [`MAX_INPUT`].
    TooLarge,
    /// Не модуль WASM или не прошёл проверку.
    Invalid,
    /// У модуля есть импорты.
    Imports,
    /// Нет экспортов ABI v1, другая версия ABI или указатель вне памяти модуля.
    Abi,
    /// Кончилось топливо.
    Fuel,
    /// Память модуля сверх [`MAX_MEMORY`].
    Memory,
    /// Ловушка в модуле.
    Trap,
    /// Ответ длиннее [`MAX_OUTPUT`], не разобран или вне пределов.
    Output,
}

/// Ответ модуля как чужие данные: пределы разделов, блоков, символов и вложенности, ссылки — через
/// [`postman_ir::check_link`] (негодная — текстом без ссылки), картинки `cid:` — их подписью (вложений у модуля нет),
/// размер картинки.
pub fn checked(out: ViewerOutput) -> Result<ViewerOutput, Refusal> {
    match out {
        ViewerOutput::Sections { sections, truncated } => {
            if sections.len() > MAX_SECTIONS {
                return Err(Refusal::Output);
            }
            let mut budget = Budget::default();
            let sections = sections
                .into_iter()
                .map(|s| {
                    if let SectionTitle::Sheet(name) = &s.title {
                        budget.chars(name)?;
                    }
                    Ok(Section { title: s.title, blocks: budget.blocks(s.blocks, 0)? })
                })
                .collect::<Result<_, Refusal>>()?;
            Ok(ViewerOutput::Sections { sections, truncated })
        }
        ViewerOutput::Image { width, height, rgba } => {
            let sides = (1..=MAX_SIDE).contains(&width) && (1..=MAX_SIDE).contains(&height);
            if !sides || rgba.len() != width as usize * height as usize * 4 {
                return Err(Refusal::Output);
            }
            Ok(ViewerOutput::Image { width, height, rgba })
        }
        ViewerOutput::Error(e) => Ok(ViewerOutput::Error(e.chars().filter(|c| !c.is_control()).take(MAX_ERROR).collect())),
    }
}

/// Сколько ответа уже принято: блоки (с вложенными), ячейки таблиц, символы.
#[derive(Default)]
struct Budget {
    blocks: usize,
    cells: usize,
    chars: usize,
}

impl Budget {
    fn chars(&mut self, s: &str) -> Result<(), Refusal> {
        self.chars += s.chars().count();
        if self.chars > docx::MAX_CHARS { Err(Refusal::Output) } else { Ok(()) }
    }

    fn blocks(&mut self, blocks: Vec<Block>, depth: usize) -> Result<Vec<Block>, Refusal> {
        if depth > MAX_NESTING {
            return Err(Refusal::Output);
        }
        blocks.into_iter().map(|b| self.block(b, depth)).collect()
    }

    fn block(&mut self, b: Block, depth: usize) -> Result<Block, Refusal> {
        self.blocks += 1;
        if self.blocks > docx::MAX_BLOCKS {
            return Err(Refusal::Output);
        }
        Ok(match b {
            Block::Paragraph { align, content } => Block::Paragraph { align, content: self.inlines(content, false)? },
            Block::Heading { level, content } => Block::Heading { level: level.clamp(1, 6), content: self.inlines(content, false)? },
            Block::Quote(inner) => Block::Quote(self.blocks(inner, depth + 1)?),
            Block::List { ordered, items } => {
                Block::List { ordered, items: items.into_iter().map(|i| self.blocks(i, depth + 1)).collect::<Result<_, _>>()? }
            }
            Block::Code(code) => {
                self.chars(&code)?;
                Block::Code(code)
            }
            Block::Table(rows) => {
                let mut out = Vec::with_capacity(rows.len());
                for row in rows {
                    self.cells += row.len();
                    if self.cells > sheet::MAX_CELLS {
                        return Err(Refusal::Output);
                    }
                    out.push(row.into_iter().map(|c| self.inlines(c, false)).collect::<Result<_, _>>()?);
                }
                Block::Table(out)
            }
            b @ (Block::Rule | Block::Figure) => b,
        })
    }

    /// Инлайны; `in_link` — внутри ссылки: вложенная ссылка — её текстом.
    fn inlines(&mut self, inlines: Vec<Inline>, in_link: bool) -> Result<Vec<Inline>, Refusal> {
        let mut out = Vec::with_capacity(inlines.len());
        for i in inlines {
            match i {
                Inline::Text { text, marks } => {
                    self.chars(&text)?;
                    out.push(Inline::Text { text, marks });
                }
                Inline::Link { href, content, .. } => {
                    let content = self.inlines(content, true)?;
                    let check = if in_link { None } else { postman_ir::check_link(&href, &postman_ir::plain_text(&content)) };
                    match check {
                        Some(c) => out.push(Inline::Link { href: c.href, host: c.host, suspicious: c.suspicious, content }),
                        None => out.extend(content),
                    }
                }
                Inline::Image { alt, .. } => {
                    self.chars(&alt)?;
                    if !alt.is_empty() {
                        out.push(Inline::text(alt));
                    }
                }
                Inline::LineBreak => out.push(Inline::LineBreak),
            }
        }
        Ok(out)
    }
}

#[cfg(feature = "viewer")]
pub use host::{run, serve};

#[cfg(feature = "viewer")]
mod host {
    use wasmi::errors::{ErrorKind, InstantiationError};
    use wasmi::{Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder, TrapCode};

    use super::{ABI, FUEL, MAX_INPUT, MAX_MEMORY, MAX_MODULE, MAX_OUTPUT, Refusal, VIEW_LIMIT, ViewerOutput, checked};
    use crate::{Request, Response};

    /// Точка входа `postman-viewer.exe`: та же песочница и тот же обмен, что у воркера; запрос — только
    /// [`Request::ViewModule`].
    pub fn serve() -> ! {
        crate::run(VIEW_LIMIT, |req, bin| match req {
            Ok(Request::ViewModule { wasm }) => {
                let Some((module, input)) = bin.split_at_checked(wasm as usize) else {
                    return (Response::View(Err(Refusal::Invalid)), Vec::new());
                };
                // Сбой интерпретатора на чужом модуле — отказ этого просмотра, а не смерть процесса.
                let r = std::panic::catch_unwind(|| run(module, input, FUEL)).unwrap_or(Err(Refusal::Trap));
                match r {
                    Ok(ViewerOutput::Image { width, height, rgba }) => {
                        (Response::View(Ok(ViewerOutput::Image { width, height, rgba: Vec::new() })), rgba)
                    }
                    r => (Response::View(r), Vec::new()),
                }
            }
            Ok(_) => (Response::Err("только запрос модуля просмотра".into()), Vec::new()),
            Err(e) => (Response::Err(e.to_string()), Vec::new()),
        })
    }

    /// Модуль `module` смотрит вход `input` с топливом `fuel` (в процессе-хосте — [`FUEL`]); ответ проверен
    /// ([`checked`]). Глубину вложенности postcard не ограничивает: переполнение стека на разборе — падение процесса
    /// хоста, клиент поднимает новый.
    pub fn run(module: &[u8], input: &[u8], fuel: u64) -> Result<ViewerOutput, Refusal> {
        if module.len() > MAX_MODULE || input.len() > MAX_INPUT {
            return Err(Refusal::TooLarge);
        }
        let mut config = Config::default();
        config.consume_fuel(true);
        let engine = Engine::new(&config);
        let module = Module::new(&engine, module).map_err(|_| Refusal::Invalid)?;
        if module.imports().len() != 0 {
            return Err(Refusal::Imports);
        }
        let limits =
            StoreLimitsBuilder::new().memory_size(MAX_MEMORY).memories(1).tables(1).table_elements(1 << 20).trap_on_grow_failure(true).build();
        let mut store = Store::new(&engine, limits);
        store.limiter(|l: &mut StoreLimits| l);
        store.set_fuel(fuel).map_err(|_| Refusal::Fuel)?;
        let instance = Linker::new(&engine).instantiate_and_start(&mut store, &module).map_err(refusal)?;
        let abi = instance.get_typed_func::<(), u32>(&store, "pm_abi").map_err(|_| Refusal::Abi)?;
        let alloc = instance.get_typed_func::<u32, u32>(&store, "pm_alloc").map_err(|_| Refusal::Abi)?;
        let view = instance.get_typed_func::<(u32, u32), u64>(&store, "pm_view").map_err(|_| Refusal::Abi)?;
        let memory = instance.get_memory(&store, "memory").ok_or(Refusal::Abi)?;
        if abi.call(&mut store, ()).map_err(refusal)? != ABI {
            return Err(Refusal::Abi);
        }
        let len = u32::try_from(input.len()).map_err(|_| Refusal::TooLarge)?;
        let ptr = alloc.call(&mut store, len).map_err(refusal)?;
        memory.write(&mut store, ptr as usize, input).map_err(|_| Refusal::Abi)?;
        let packed = view.call(&mut store, (ptr, len)).map_err(refusal)?;
        let (at, n) = ((packed >> 32) as usize, (packed & 0xFFFF_FFFF) as usize);
        if n > MAX_OUTPUT {
            return Err(Refusal::Output);
        }
        let mut out = vec![0; n];
        memory.read(&store, at, &mut out).map_err(|_| Refusal::Output)?;
        // Память модуля больше не нужна: в процессе — ответ и его разбор.
        drop(store);
        checked(postcard::from_bytes(&out).map_err(|_| Refusal::Output)?)
    }

    fn refusal(e: wasmi::Error) -> Refusal {
        match (e.as_trap_code(), e.kind()) {
            (Some(TrapCode::OutOfFuel), _) => Refusal::Fuel,
            (Some(TrapCode::GrowthOperationLimited), _)
            | (_, ErrorKind::Memory(_) | ErrorKind::Instantiation(InstantiationError::FailedToInstantiateMemory(_))) => Refusal::Memory,
            _ => Refusal::Trap,
        }
    }
}
