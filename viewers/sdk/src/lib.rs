//! SDK модулей просмотра Postman (ADR 0007, решение A): ответ модуля [`ViewerOutput`] и макрос [`viewer!`] с
//! экспортами ABI v1. Модуль — `cdylib` под `wasm32-unknown-unknown` без импортов: ни WASI, ни своих функций хоста
//! (модуль с любым импортом хост не загружает).
//!
//! ABI v1 (хост — `postman-viewer.exe`, `postman_worker::wasm`):
//! - `pm_abi() -> i32` — версия ABI, [`ABI`];
//! - `pm_alloc(len: i32) -> i32` — место под вход длины `len` в памяти модуля (экспорт `memory`);
//! - `pm_view(ptr: i32, len: i32) -> i64` — просмотр входа; итог — указатель ответа в старших 32 битах и его длина
//!   в младших; ответ — [`ViewerOutput`] в postcard, не длиннее [`MAX_OUTPUT`].
//!
//! Типы — копия схемы хоста (`postman_ir::Block`, `Inline`, `Marks`, `Align`; `postman_worker::Section`,
//! `SectionTitle`, `wasm::ViewerOutput`) без зависимости от него: postcard не описывает себя, поэтому порядок
//! вариантов и полей менять нельзя — только новой версией ABI. Хост проверяет ответ как чужие данные: пределы ниже
//! — его; ответ сверх них отвергается целиком, поэтому модуль обрезает показ сам (`truncated`).

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

/// Версия ABI.
pub const ABI: u32 = 1;
/// Вход больше хост модулю не передаёт.
pub const MAX_INPUT: usize = 48 * 1024 * 1024;
/// Память модуля: рост сверх — ловушка.
pub const MAX_MEMORY: usize = 256 * 1024 * 1024;
/// Топливо на запрос (порядка 10⁹ инструкций WASM).
pub const FUEL: u64 = 1_000_000_000;
/// Ответ в postcard длиннее — отказ.
pub const MAX_OUTPUT: usize = 8 * 1024 * 1024;
/// Разделов в ответе.
pub const MAX_SECTIONS: usize = 500;
/// Блоков во всех разделах, с вложенными (пункт списка, блок цитаты — тоже блок).
pub const MAX_BLOCKS: usize = 10_000;
/// Символов текста во всех разделах (текст, код, подписи картинок, имена листов).
pub const MAX_CHARS: usize = 1_000_000;
/// Ячеек во всех таблицах.
pub const MAX_CELLS: usize = 200_000;
/// Вложенность блоков (цитата в цитате, список в списке): глубже — отказ.
pub const MAX_NESTING: usize = 16;
/// Сторона картинки, пикселей.
pub const MAX_SIDE: u32 = 2048;
/// Ошибка модуля длиннее хост обрезает.
pub const MAX_ERROR: usize = 1000;

/// Ответ модуля.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ViewerOutput {
    /// Документ разделами (вкладки или листание); `truncated` — показано начало.
    Sections { sections: Vec<Section>, truncated: bool },
    /// Картинка RGBA8: `width × height × 4` байт, стороны — от 1 до [`MAX_SIDE`].
    Image { width: u32, height: u32, rgba: Vec<u8> },
    /// Файл не показан; текст — для журнала и подсказки, без управляющих символов.
    Error(String),
}

/// Раздел документа: глава, лист, слайд.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Section {
    pub title: SectionTitle,
    pub blocks: Vec<Block>,
}

/// Заголовок раздела — данными: подпись («Лист …», «Слайд N») составляет интерфейс. У главы — `None`, её название —
/// заголовком в блоках.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SectionTitle {
    None,
    /// Имя листа книги.
    Sheet(String),
    /// Номер слайда с единицы.
    Slide(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
    Justify,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Block {
    Paragraph {
        align: Align,
        content: Vec<Inline>,
    },
    /// Уровень 1–6.
    Heading {
        level: u8,
        content: Vec<Inline>,
    },
    Quote(Vec<Block>),
    List {
        ordered: bool,
        items: Vec<Vec<Block>>,
    },
    Code(String),
    Rule,
    /// Строки → ячейки → инлайны; простая сетка без вложенности.
    Table(Vec<Vec<Vec<Inline>>>),
    /// Картинка или объект документа: без содержимого, подпись — у интерфейса.
    Figure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Marks {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub code: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Inline {
    Text {
        text: String,
        marks: Marks,
    },
    /// Ссылка: `host` и `suspicious` хост ставит сам по проверке ссылки (негодная — текстом); модуль даёт `None` и
    /// `false`.
    Link {
        href: String,
        host: Option<String>,
        suspicious: bool,
        content: Vec<Inline>,
    },
    /// Картинка письма (`cid:`): у модуля вложений нет — хост показывает подпись `alt`.
    Image {
        cid: String,
        alt: String,
        width: Option<u32>,
        height: Option<u32>,
    },
    LineBreak,
}

impl Inline {
    pub fn text(s: impl Into<String>) -> Self {
        Inline::Text { text: s.into(), marks: Marks::default() }
    }

    /// Внешняя ссылка; проверяет её хост.
    pub fn link(href: impl Into<String>, content: Vec<Inline>) -> Self {
        Inline::Link { href: href.into(), host: None, suspicious: false, content }
    }
}

/// Экспорты ABI v1 для функции `fn(&[u8]) -> ViewerOutput`: `postman_viewer_sdk::viewer!(view);`. Только под
/// `wasm32`: на других целях (тесты модуля) макрос ничего не порождает.
#[macro_export]
macro_rules! viewer {
    ($view:path) => {
        #[cfg(target_arch = "wasm32")]
        #[unsafe(no_mangle)]
        pub extern "C" fn pm_abi() -> u32 {
            $crate::ABI
        }

        #[cfg(target_arch = "wasm32")]
        #[unsafe(no_mangle)]
        pub extern "C" fn pm_alloc(len: u32) -> u32 {
            $crate::__alloc(len)
        }

        #[cfg(target_arch = "wasm32")]
        #[unsafe(no_mangle)]
        pub extern "C" fn pm_view(ptr: u32, len: u32) -> u64 {
            $crate::__view(ptr, len, $view)
        }
    };
}

/// Ответ в postcard; не уложился в [`MAX_OUTPUT`] — ошибкой.
pub fn encode(out: &ViewerOutput) -> Vec<u8> {
    match postcard::to_allocvec(out) {
        Ok(bytes) if bytes.len() <= MAX_OUTPUT => bytes,
        _ => postcard::to_allocvec(&ViewerOutput::Error(String::from("answer is too large"))).unwrap_or_default(),
    }
}

#[cfg(target_arch = "wasm32")]
#[doc(hidden)]
pub fn __alloc(len: u32) -> u32 {
    // Вход живёт до конца запроса: хост создаёт модуль заново на каждый просмотр, освобождать незачем.
    let buf = alloc::vec![0u8; len as usize].into_boxed_slice();
    alloc::boxed::Box::leak(buf).as_mut_ptr() as u32
}

#[cfg(target_arch = "wasm32")]
#[doc(hidden)]
pub fn __view(ptr: u32, len: u32, view: fn(&[u8]) -> ViewerOutput) -> u64 {
    // SAFETY: `ptr` и `len` — от `pm_alloc` этого же модуля: хост пишет вход в выданный им буфер той же длины.
    let input = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
    let out = alloc::boxed::Box::leak(encode(&view(input)).into_boxed_slice());
    (u64::from(out.as_ptr() as u32) << 32) | out.len() as u64
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    /// Байты postcard — договор с хостом: те же значения хост разбирает в `postman_worker::wasm::ViewerOutput`.
    #[test]
    fn wire_format_matches_the_host_schema() {
        let bold = Marks { bold: true, ..Marks::default() };
        let out = ViewerOutput::Sections {
            sections: vec![Section {
                title: SectionTitle::Slide(3),
                blocks: vec![Block::Heading { level: 2, content: vec![Inline::Text { text: "a".into(), marks: bold }] }, Block::Figure],
            }],
            truncated: true,
        };
        assert_eq!(encode(&out), [0, 1, 2, 3, 2, 1, 2, 1, 0, 1, b'a', 1, 0, 0, 0, 0, 7, 1]);
        let image = ViewerOutput::Image { width: 2, height: 1, rgba: vec![1, 2, 3, 4, 5, 6, 7, 8] };
        assert_eq!(encode(&image), [1, 2, 1, 8, 1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(encode(&ViewerOutput::Error("x".into())), [2, 1, b'x']);

        // Номера вариантов и порядок полей всех типов.
        let blocks = vec![
            Block::Paragraph { align: Align::Justify, content: vec![Inline::LineBreak] },
            Block::Quote(vec![Block::Rule]),
            Block::List { ordered: true, items: vec![vec![Block::Code("c".into())]] },
            Block::Table(vec![vec![vec![Inline::link("h", vec![])]]]),
            Block::Paragraph {
                align: Align::Center,
                content: vec![Inline::Image { cid: "i".into(), alt: "t".into(), width: Some(5), height: None }],
            },
        ];
        let section = Section { title: SectionTitle::Sheet("s".into()), blocks };
        let out = ViewerOutput::Sections { sections: vec![section], truncated: false };
        #[rustfmt::skip]
        let expected = [
            0, 1, 1, 1, b's', 5,
            0, 3, 1, 3,
            2, 1, 5,
            3, 1, 1, 1, 4, 1, b'c',
            6, 1, 1, 1, 1, 1, b'h', 0, 0, 0,
            0, 1, 1, 2, 1, b'i', 1, b't', 1, 5, 0,
            0,
        ];
        assert_eq!(encode(&out), expected);
        let back: ViewerOutput = postcard::from_bytes(&expected).unwrap();
        assert_eq!(back, out);
    }

    #[test]
    fn oversized_answer_becomes_an_error() {
        let out = ViewerOutput::Image { width: 1, height: 1, rgba: vec![0; MAX_OUTPUT] };
        let back: ViewerOutput = postcard::from_bytes(&encode(&out)).unwrap();
        assert_eq!(back, ViewerOutput::Error("answer is too large".into()));
    }
}
