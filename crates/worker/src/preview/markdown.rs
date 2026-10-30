//! Просмотр `.md` (S8-19): Markdown → безопасный IR разборщиком `pulldown-cmark` (тот же, что у `compose`). Только
//! здесь, в песочнице.
//!
//! Заголовки, абзацы, жирный, курсив, зачёркнутый, код, цитаты, списки, таблицы и черта — блоками IR; ссылки — через
//! проверку ссылок IR (`javascript:` и прочие схемы — текстом), картинки — их подписью (ничего не загружается), сырой
//! HTML — текстом как есть. Читается начало файла не больше [`MAX_MARKDOWN`] байт (UTF-8, UTF-16 с BOM или
//! Windows-1251); блоков и символов — как у `.docx`, вложенность цитат и списков — не глубже [`MAX_NESTING`] (глубже
//! — в том же уровне).

use std::ops::ControlFlow;

use postman_ir::{Align, Block, Inline, Marks, check_link, plain_text};
use pulldown_cmark::{Event, LinkType, Options, Parser, Tag, TagEnd};

use super::docx::{self, MAX_BLOCKS, MAX_CHARS};

/// Столько байт от начала файла разбираем; дальше — `truncated`.
pub const MAX_MARKDOWN: usize = 4 * 1024 * 1024;
/// Цитат и списков друг в друге (у IR в JSON ответа каждый уровень — до четырёх уровней вложенности).
pub const MAX_NESTING: usize = 16;

/// Пределы разбора; в тестах — меньше.
#[derive(Debug, Clone, Copy)]
struct Limits {
    blocks: usize,
    chars: usize,
}

const LIMITS: Limits = Limits { blocks: MAX_BLOCKS, chars: MAX_CHARS };

/// Текст Markdown → блоки IR и признак «показано начало»; двоичный файл — ошибка.
pub fn markdown(bytes: &[u8]) -> Result<(Vec<Block>, bool), String> {
    parse(bytes, LIMITS)
}

fn parse(bytes: &[u8], limits: Limits) -> Result<(Vec<Block>, bool), String> {
    let bom = [&[0xEF, 0xBB, 0xBF][..], &[0xFF, 0xFE], &[0xFE, 0xFF]].iter().any(|b| bytes.starts_with(b));
    if !bom && bytes[..bytes.len().min(8192)].contains(&0) {
        return Err("не текстовый документ".into());
    }
    let cut = bytes.len() > MAX_MARKDOWN;
    let text = super::decode(&bytes[..bytes.len().min(MAX_MARKDOWN)], cut);
    let mut b = Builder::new(limits);
    for ev in Parser::new_ext(&text, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH) {
        if b.event(ev).is_break() {
            break;
        }
    }
    let truncated = cut || b.truncated;
    Ok((b.finish(), truncated))
}

/// Контейнер в сборке.
enum Frame {
    Quote(Vec<Block>),
    List { ordered: bool, items: Vec<Vec<Block>> },
    Item(Vec<Block>),
}

/// Блок с текстом в сборке.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Leaf {
    Para,
    Heading(u8),
    Cell,
}

/// Строки → ячейки → инлайны.
type Rows = Vec<Vec<Vec<Inline>>>;

struct Builder {
    limits: Limits,
    root: Vec<Block>,
    frames: Vec<Frame>,
    /// Открытые контейнеры: `false` — глубже предела, не заведён.
    opened: Vec<bool>,
    leaf: Option<Leaf>,
    inlines: Vec<Inline>,
    /// Вложенность выделений: жирный, курсив, зачёркнутый.
    strong: usize,
    emphasis: usize,
    strike: usize,
    link: Option<(String, Vec<Inline>)>,
    links_open: usize,
    table: Option<Rows>,
    code: Option<String>,
    blocks: usize,
    chars: usize,
    truncated: bool,
}

impl Builder {
    fn new(limits: Limits) -> Self {
        Builder {
            limits,
            root: Vec::new(),
            frames: Vec::new(),
            opened: Vec::new(),
            leaf: None,
            inlines: Vec::new(),
            strong: 0,
            emphasis: 0,
            strike: 0,
            link: None,
            links_open: 0,
            table: None,
            code: None,
            blocks: 0,
            chars: 0,
            truncated: false,
        }
    }

    fn marks(&self) -> Marks {
        Marks { bold: self.strong > 0, italic: self.emphasis > 0, strike: self.strike > 0, ..Marks::default() }
    }

    fn event(&mut self, ev: Event<'_>) -> ControlFlow<()> {
        match ev {
            Event::Start(tag) => return self.start(tag),
            Event::End(tag) => return self.end(tag),
            Event::Text(t) if self.code.is_some() => return self.code_text(&t),
            Event::Text(t) => return self.text(&t, self.marks()),
            Event::Code(t) => return self.text(&t, Marks { code: true, ..self.marks() }),
            // Сырой HTML — текстом, строки — разрывами.
            Event::Html(t) | Event::InlineHtml(t) => {
                for (n, line) in t.trim_end_matches('\n').split('\n').enumerate() {
                    if n > 0 {
                        self.inline(Inline::LineBreak);
                        self.spend()?;
                    }
                    self.text(line, self.marks())?;
                }
            }
            Event::SoftBreak => return self.text(" ", self.marks()),
            Event::HardBreak => {
                self.inline(Inline::LineBreak);
                return self.spend();
            }
            Event::Rule => {
                self.end_leaf()?;
                return self.put(Block::Rule);
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }

    fn start(&mut self, tag: Tag<'_>) -> ControlFlow<()> {
        match tag {
            Tag::Paragraph | Tag::HtmlBlock => {
                self.end_leaf()?;
                self.leaf = Some(Leaf::Para);
            }
            Tag::Heading { level, .. } => {
                self.end_leaf()?;
                self.leaf = Some(Leaf::Heading((level as u8).clamp(1, 6)));
            }
            Tag::BlockQuote(_) => self.open(Frame::Quote(Vec::new()))?,
            Tag::List(start) => self.open(Frame::List { ordered: start.is_some(), items: Vec::new() })?,
            Tag::Item => self.open(Frame::Item(Vec::new()))?,
            Tag::CodeBlock(_) => {
                self.end_leaf()?;
                self.code = Some(String::new());
            }
            Tag::Table(_) => {
                self.end_leaf()?;
                self.table = Some(Vec::new());
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(rows) = &mut self.table {
                    rows.push(Vec::new());
                    return self.count();
                }
            }
            Tag::TableCell => {
                self.inlines.clear();
                self.leaf = Some(Leaf::Cell);
            }
            Tag::Emphasis => self.emphasis += 1,
            Tag::Strong => self.strong += 1,
            Tag::Strikethrough => self.strike += 1,
            Tag::Link { link_type, dest_url, .. } => {
                self.links_open += 1;
                if self.links_open == 1 {
                    let href = if link_type == LinkType::Email { format!("mailto:{dest_url}") } else { dest_url.to_string() };
                    self.link = Some((href, Vec::new()));
                }
            }
            // Картинка — её подписью (текст внутри), адрес не нужен.
            _ => {}
        }
        ControlFlow::Continue(())
    }

    fn end(&mut self, tag: TagEnd) -> ControlFlow<()> {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::HtmlBlock => return self.end_leaf(),
            TagEnd::BlockQuote(_) | TagEnd::List(_) | TagEnd::Item => return self.close(),
            TagEnd::CodeBlock => {
                if let Some(code) = self.code.take() {
                    return self.put(Block::Code(code.trim_end_matches('\n').to_string()));
                }
            }
            TagEnd::TableCell => {
                self.close_link();
                self.leaf = None;
                let cell = std::mem::take(&mut self.inlines);
                if let Some(row) = self.table.as_mut().and_then(|t| t.last_mut()) {
                    row.push(cell);
                    return self.spend();
                }
            }
            TagEnd::Table => {
                if let Some(rows) = self.table.take() {
                    self.place(Block::Table(rows));
                }
            }
            TagEnd::Emphasis => self.emphasis = self.emphasis.saturating_sub(1),
            TagEnd::Strong => self.strong = self.strong.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            TagEnd::Link => {
                self.links_open = self.links_open.saturating_sub(1);
                if self.links_open == 0 {
                    self.close_link();
                }
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }

    /// Контейнер: глубже предела — его содержимое в текущем.
    fn open(&mut self, f: Frame) -> ControlFlow<()> {
        self.end_leaf()?;
        let open = self.frames.len() < MAX_NESTING;
        if open {
            self.frames.push(f);
        }
        self.opened.push(open);
        ControlFlow::Continue(())
    }

    fn close(&mut self) -> ControlFlow<()> {
        self.end_leaf()?;
        if !self.opened.pop().unwrap_or(false) {
            return ControlFlow::Continue(());
        }
        match self.frames.pop() {
            Some(Frame::Quote(blocks)) => self.place(Block::Quote(blocks)),
            Some(Frame::List { ordered, items }) => self.place(Block::List { ordered, items }),
            Some(Frame::Item(blocks)) => match self.frames.last_mut() {
                Some(Frame::List { items, .. }) => items.push(blocks),
                _ => {
                    for b in blocks {
                        self.place(b);
                    }
                }
            },
            None => {}
        }
        ControlFlow::Continue(())
    }

    /// Готовый блок — в текущий контейнер без счёта (контейнер, таблица: их части уже посчитаны).
    fn place(&mut self, b: Block) {
        match self.frames.last_mut() {
            Some(Frame::Quote(v) | Frame::Item(v)) => v.push(b),
            Some(Frame::List { items, .. }) => items.push(vec![b]),
            None => self.root.push(b),
        }
    }

    fn put(&mut self, b: Block) -> ControlFlow<()> {
        self.place(b);
        self.count()
    }

    /// Конец абзаца или заголовка (и неявного абзаца пункта списка).
    fn end_leaf(&mut self) -> ControlFlow<()> {
        let Some(leaf) = self.leaf.take() else { return ControlFlow::Continue(()) };
        if leaf == Leaf::Cell {
            self.leaf = Some(leaf);
            return ControlFlow::Continue(());
        }
        self.close_link();
        let mut content = std::mem::take(&mut self.inlines);
        while matches!(content.last(), Some(Inline::LineBreak)) {
            content.pop();
        }
        if docx::is_blank(&content) {
            return ControlFlow::Continue(());
        }
        match leaf {
            Leaf::Heading(level) => self.put(Block::Heading { level, content }),
            _ => self.put(Block::Paragraph { align: Align::Start, content }),
        }
    }

    /// Инлайн — в ссылку, если она открыта, иначе в текущий блок (у пункта без абзаца — неявный абзац).
    fn inline(&mut self, i: Inline) {
        if let Some((_, content)) = &mut self.link {
            docx::push(content, i);
            return;
        }
        if self.leaf.is_none() {
            self.leaf = Some(Leaf::Para);
        }
        docx::push(&mut self.inlines, i);
    }

    fn close_link(&mut self) {
        let Some((href, content)) = self.link.take() else { return };
        self.links_open = 0;
        match check_link(&href, &plain_text(&content)) {
            Some(l) if !content.is_empty() => self.inline(Inline::Link { href: l.href, host: l.host, suspicious: l.suspicious, content }),
            // Запрещённая схема, относительный адрес или пустая ссылка: остаётся только текст.
            _ => {
                for i in content {
                    self.inline(i);
                }
            }
        }
    }

    /// Текст с пределом символов; управляющие и bidi-символы убираются.
    fn text(&mut self, s: &str, marks: Marks) -> ControlFlow<()> {
        let (clean, cut) = self.take(s);
        if !clean.is_empty() {
            self.inline(Inline::Text { text: clean, marks });
        }
        if cut {
            self.truncated = true;
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    }

    fn code_text(&mut self, s: &str) -> ControlFlow<()> {
        let (clean, cut) = self.take(s);
        if let Some(code) = &mut self.code {
            code.push_str(&clean);
        }
        if cut {
            self.truncated = true;
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    }

    /// Чистый текст в пределах символов и признак «упёрлись».
    fn take(&mut self, s: &str) -> (String, bool) {
        let left = self.limits.chars.saturating_sub(self.chars);
        let mut taken = 0usize;
        let clean: String = s
            .chars()
            .filter(|&c| c == '\t' || (c == '\n' && self.code.is_some()) || !c.is_control())
            .filter(|c| !matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
            .take_while(|_| {
                taken += 1;
                taken <= left
            })
            .collect();
        self.chars += clean.chars().count();
        (clean, taken > left)
    }

    /// Разрыв строки и ячейка — по символу из предела.
    fn spend(&mut self) -> ControlFlow<()> {
        self.chars += 1;
        if self.chars > self.limits.chars {
            self.truncated = true;
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    }

    fn count(&mut self) -> ControlFlow<()> {
        self.blocks += 1;
        if self.blocks >= self.limits.blocks {
            self.truncated = true;
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    }

    /// Конец разбора (в том числе по пределу): недособранное — как есть.
    fn finish(mut self) -> Vec<Block> {
        if self.leaf == Some(Leaf::Cell) {
            let _ = self.end(TagEnd::TableCell);
        }
        if let Some(code) = self.code.take() {
            self.place(Block::Code(code));
        }
        if let Some(rows) = self.table.take() {
            self.place(Block::Table(rows));
        }
        let _ = self.end_leaf();
        while !self.opened.is_empty() {
            let _ = self.close();
        }
        self.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preview::{Preview, PreviewKind, Section, SectionTitle, preview};

    fn t(s: &str) -> Inline {
        Inline::text(s)
    }

    fn p(content: Vec<Inline>) -> Block {
        Block::Paragraph { align: Align::Start, content }
    }

    #[test]
    fn markdown_blocks_marks_and_safe_links() {
        let md = "# Отчёт\n\nТекст **жирный** и *курсив*, `код`,\nдальше ~~нет~~.\n\n\
                  [сайт](https://example.com/a) [скрипт](javascript:alert(1)) <https://example.org> ![схема](http://tracker.example/p.png)\n\n\
                  > цитата\n\n- один\n- два\n  1. вложенный\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\n```\nlet x;\n```\n\n<div onclick=\"x\">html</div>\n\n---\n";
        let (b, cut) = markdown(md.as_bytes()).unwrap();
        assert!(!cut);
        let bold = Marks { bold: true, ..Marks::default() };
        let it = Marks { italic: true, ..Marks::default() };
        let code = Marks { code: true, ..Marks::default() };
        let strike = Marks { strike: true, ..Marks::default() };
        let link = |href: &str, host: &str, text: &str| Inline::Link {
            href: href.into(),
            host: Some(host.into()),
            suspicious: false,
            content: vec![t(text)],
        };
        assert_eq!(
            b,
            [
                Block::Heading { level: 1, content: vec![t("Отчёт")] },
                p(vec![
                    t("Текст "),
                    Inline::Text { text: "жирный".into(), marks: bold },
                    t(" и "),
                    Inline::Text { text: "курсив".into(), marks: it },
                    t(", "),
                    Inline::Text { text: "код".into(), marks: code },
                    t(", дальше "),
                    Inline::Text { text: "нет".into(), marks: strike },
                    t("."),
                ]),
                p(vec![
                    link("https://example.com/a", "example.com", "сайт"),
                    t(" скрипт "),
                    link("https://example.org/", "example.org", "https://example.org"),
                    t(" схема"),
                ]),
                Block::Quote(vec![p(vec![t("цитата")])]),
                Block::List {
                    ordered: false,
                    items: vec![
                        vec![p(vec![t("один")])],
                        vec![p(vec![t("два")]), Block::List { ordered: true, items: vec![vec![p(vec![t("вложенный")])]] }],
                    ]
                },
                Block::Table(vec![vec![vec![t("A")], vec![t("B")]], vec![vec![t("1")], vec![t("2")]]]),
                Block::Code("let x;".into()),
                p(vec![t("<div onclick=\"x\">html</div>")]),
                Block::Rule,
            ]
        );
    }

    #[test]
    fn deep_nesting_and_limits() {
        let deep = format!("{}x\n", "> ".repeat(100));
        let (b, _) = markdown(deep.as_bytes()).unwrap();
        let mut depth = 0;
        let mut cur = &b;
        while let [Block::Quote(inner)] = cur.as_slice() {
            depth += 1;
            cur = inner;
        }
        assert_eq!(depth, MAX_NESTING);
        assert_eq!(cur, &[p(vec![t("x")])]);
        let many = "абзац\n\n".repeat(50);
        let (b, cut) = parse(many.as_bytes(), Limits { blocks: 10, ..LIMITS }).unwrap();
        assert!(cut && b.len() == 10);
        let (b, cut) = parse(many.as_bytes(), Limits { chars: 7, ..LIMITS }).unwrap();
        assert!(cut);
        assert_eq!(b, [p(vec![t("абзац")]), p(vec![t("аб")])]);
        assert!(markdown(b"\x00\x01binary").is_err());
    }

    #[test]
    fn document_kind_falls_back_to_markdown() {
        // Windows-1251 без BOM.
        let (pv, _) = preview(PreviewKind::Document, &[b'#', b' ', 0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2], 0, 0, 0).unwrap();
        let heading = Block::Heading { level: 1, content: vec![t("Привет")] };
        assert_eq!(pv, Preview::Document { sections: vec![Section { title: SectionTitle::None, blocks: vec![heading] }], truncated: false });
    }
}
