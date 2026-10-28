//! IR письма: Markdown-подобное дерево, в которое сводится любое входящее тело.
//!
//! Входящий HTML проходит через белый список ([`from_html`]), text/plain — через
//! [`from_plain`]. Из IR строятся отображение, цитата для ответа ([`quote`]) и
//! исходящий HTML ([`markdown_to_html`]). Крейт не делает ввода-вывода и
//! предназначен для запуска внутри изолированного воркера.

mod history;
mod html;
mod link;
mod markdown;
mod plain;
mod style;

pub use html::from_html;
pub use link::{LinkCheck, check_link};
pub use markdown::{markdown_to_html, quote, to_markdown};
pub use plain::from_plain;
pub use style::MAX_IMAGE_SIDE;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Document {
    pub blocks: Vec<Block>,
    /// Сколько внешних картинок выброшено (для баннера «картинки заблокированы»).
    pub remote_images_blocked: usize,
    /// Первый блок истории переписки (цитаты прошлых писем, шапка Outlook); `None` — истории нет или она не
    /// отделяется надёжно. Ставит разбор в воркере (S5-05); новый текст — [`Document::new_text`].
    #[serde(default)]
    pub history: Option<usize>,
}

impl Document {
    /// Только новый текст письма, без истории переписки (лента беседы).
    pub fn new_text(&self) -> &[Block] {
        match self.history {
            Some(h) if h <= self.blocks.len() => &self.blocks[..h],
            _ => &self.blocks,
        }
    }

    /// Блок в начало (предупреждение над письмом); граница истории сдвигается.
    pub fn prepend(&mut self, block: Block) {
        self.blocks.insert(0, block);
        if let Some(h) = &mut self.history {
            *h += 1;
        }
    }
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
    /// Строки -> ячейки -> инлайны. Только простая сетка без вложенности.
    Table(Vec<Vec<Vec<Inline>>>),
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
    Link {
        /// Нормализованный URL (IDN уже в punycode).
        href: String,
        /// Хост для показа при наведении; `None` для mailto.
        host: Option<String>,
        /// Видимый текст похож на адрес, но ведёт на другой хост.
        suspicious: bool,
        content: Vec<Inline>,
    },
    /// Картинка из вложения письма (`cid:`); показывается только по явному разрешению.
    Image {
        cid: String,
        alt: String,
        /// Задуманный размер в CSS px — из атрибутов `width`/`height` и `style` (T1-10б); `None` — не задан или
        /// вне 1..=[`MAX_IMAGE_SIDE`]. Одна сторона без другой — вторую считают по пропорции кадра.
        #[serde(default)]
        width: Option<u32>,
        #[serde(default)]
        height: Option<u32>,
    },
    LineBreak,
}

impl Inline {
    pub fn text(s: impl Into<String>) -> Self {
        Inline::Text { text: s.into(), marks: Marks::default() }
    }
}

/// Плоский текст инлайнов (для сравнения текста ссылки, поиска и т.п.).
pub fn plain_text(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for i in inlines {
        match i {
            Inline::Text { text, .. } => out.push_str(text),
            Inline::Link { content, .. } => out.push_str(&plain_text(content)),
            Inline::Image { alt, .. } => out.push_str(alt),
            Inline::LineBreak => out.push('\n'),
        }
    }
    out
}
