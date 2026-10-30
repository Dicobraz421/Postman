//! Просмотр документа OpenDocument `.odt` (S8-19): `content.xml` → безопасный IR, как `.docx`. Только здесь, в песочнице.
//!
//! Защиты — как у [`docx`]: `content.xml` читается потоково не больше [`docx::MAX_XML`] байт после распаковки (дальше —
//! `truncated`), XML без DTD и чужих сущностей, вложенность — не глубже [`docx::MAX_DEPTH`]; блоков и символов — как у
//! `.docx`. Время ограничивает сторож в `serve`.
//!
//! Из документа берутся заголовки (`text:h`, уровень — `text:outline-level`), абзацы (`text:p`), жирный, курсив,
//! подчёркнутый и зачёркнутый из автоматических стилей абзаца и фрагмента, списки (`text:list`, нумерованный ли —
//! из `text:list-style` по уровню), таблицы (текстом по ячейкам; вложенная — строкой в ячейке), ссылки (`text:a`,
//! через проверку ссылок IR) и разрывы строк. Рамки (картинки, надписи, объекты) — блоком `Figure` после абзаца
//! (подпись — у интерфейса); примечания, сноски и записи правок не выводятся.

use std::collections::HashMap;
use std::io;
use std::ops::ControlFlow;

use postman_ir::{Align, Block, Inline, Marks, check_link, plain_text};

use super::docx::{self, Ev, attr};
use super::sheet::{Od, od, read};
use super::slides::{odf_marks, or};

/// Пределы разбора; в тестах — меньше.
#[derive(Debug, Clone, Copy)]
struct Limits {
    xml: u64,
    blocks: usize,
    chars: usize,
}

const LIMITS: Limits = Limits { xml: docx::MAX_XML, blocks: docx::MAX_BLOCKS, chars: docx::MAX_CHARS };

/// Пакет OpenDocument, а не OOXML: есть `content.xml`, нет `[Content_Types].xml`.
pub fn is_odf(bytes: &[u8]) -> bool {
    zip::ZipArchive::new(io::Cursor::new(bytes))
        .is_ok_and(|z| z.index_for_name("content.xml").is_some() && z.index_for_name("[Content_Types].xml").is_none())
}

/// Документ `.odt` → блоки IR и признак «показано начало».
pub fn odt(bytes: &[u8]) -> Result<(Vec<Block>, bool), String> {
    parse(bytes, LIMITS)
}

fn parse(bytes: &[u8], limits: Limits) -> Result<(Vec<Block>, bool), String> {
    let mut zip = zip::ZipArchive::new(io::Cursor::new(bytes)).map_err(|e| format!("документ не разобран: {e}"))?;
    let mut b = Builder::new(limits);
    let (_, hit) = read(&mut zip, "content.xml", limits.xml, od, |ev| b.event(ev)).map_err(|e| format!("документ не разобран: {e}"))?;
    let truncated = b.truncated || hit;
    Ok((b.finish(), truncated))
}

/// Абзац в сборке: заголовок ли (уровень), текст и рамки в нём.
#[derive(Default)]
struct Para {
    heading: Option<u8>,
    inlines: Vec<Inline>,
    figures: usize,
}

/// Строки → ячейки → инлайны.
type Rows = Vec<Vec<Vec<Inline>>>;

struct Builder {
    limits: Limits,
    out: Vec<Block>,
    /// Автоматические стили: текст (имя → марки) и списки (имя → нумерованный ли по уровню).
    styles: HashMap<String, Marks>,
    style: Option<(String, Marks)>,
    list_styles: HashMap<String, [bool; 10]>,
    list_style: Option<String>,
    /// Открытые списки (стиль каждого; вложенный без стиля — стиль внешнего) и пункты текущего списка.
    lists: Vec<Option<String>>,
    items: Vec<(u8, bool, Vec<Inline>)>,
    /// Пункт только открыт: его первый абзац — новый пункт, следующие — строками в нём.
    fresh_item: bool,
    /// Открытые таблицы (внешняя — первая).
    tables: Vec<Rows>,
    para: Option<Para>,
    para_marks: Marks,
    marks: Marks,
    /// Ссылка: адрес и её содержимое.
    link: Option<(String, Vec<Inline>)>,
    /// Пропускаемое поддерево (глубина внутри него).
    skip: usize,
    blocks: usize,
    chars: usize,
    truncated: bool,
}

impl Builder {
    fn new(limits: Limits) -> Self {
        Builder {
            limits,
            out: Vec::new(),
            styles: HashMap::new(),
            style: None,
            list_styles: HashMap::new(),
            list_style: None,
            lists: Vec::new(),
            items: Vec::new(),
            fresh_item: false,
            tables: Vec::new(),
            para: None,
            para_marks: Marks::default(),
            marks: Marks::default(),
            link: None,
            skip: 0,
            blocks: 0,
            chars: 0,
            truncated: false,
        }
    }

    fn style(&self, a: &[(Vec<u8>, String)]) -> Marks {
        attr(a, b"style-name").and_then(|n| self.styles.get(n)).copied().unwrap_or_default()
    }

    fn event(&mut self, ev: Ev<'_, Od>) -> ControlFlow<()> {
        match ev {
            Ev::Start(..) if self.skip > 0 => self.skip += 1,
            Ev::End(..) if self.skip > 0 => self.skip -= 1,
            Ev::Text(_) if self.skip > 0 => {}
            Ev::Start(ns, name, a) => return self.start(ns, name, a),
            Ev::End(ns, name) => return self.end(ns, name),
            Ev::Text(t) if self.para.is_some() => {
                // Пробелы в тексте ODF сворачиваются (несколько подряд — `text:s`).
                let mut s = String::with_capacity(t.len());
                for c in t.chars() {
                    let c = if c.is_whitespace() { ' ' } else { c };
                    if !(c == ' ' && s.ends_with(' ')) {
                        s.push(c);
                    }
                }
                return self.text(&s, self.marks);
            }
            Ev::Text(_) => {}
        }
        ControlFlow::Continue(())
    }

    fn start(&mut self, ns: Od, name: &[u8], a: &[(Vec<u8>, String)]) -> ControlFlow<()> {
        match (ns, name) {
            // Примечания, сноски, записи правок, оглавление полей формы — не текст документа.
            (Od::Office, b"annotation" | b"forms") | (Od::Text, b"note" | b"tracked-changes" | b"sequence-decls" | b"variable-decls") => {
                self.skip = 1
            }
            (Od::Style, b"style") => self.style = attr(a, b"name").map(|n| (n.to_string(), Marks::default())),
            (Od::Style, b"text-properties") => {
                if let Some((_, m)) = &mut self.style {
                    *m = odf_marks(a);
                }
            }
            (Od::Text, b"list-style") => self.list_style = attr(a, b"name").map(str::to_string),
            (Od::Text, kind @ (b"list-level-style-number" | b"list-level-style-bullet" | b"list-level-style-image")) => {
                if let (Some(style), Some(level)) = (&self.list_style, attr(a, b"level").and_then(|l| l.parse::<usize>().ok()))
                    && (1..=10).contains(&level)
                {
                    self.list_styles.entry(style.clone()).or_default()[level - 1] = kind == b"list-level-style-number";
                }
            }
            // Рамка, фигура, группа — блоком `Figure` (в таблице — нет); надписи в них не выводятся.
            (Od::Draw, _) => {
                self.skip = 1;
                if !self.tables.is_empty() {
                    return ControlFlow::Continue(());
                }
                match &mut self.para {
                    Some(p) => p.figures += 1,
                    None => {
                        self.flush_list();
                        return self.emit(Block::Figure);
                    }
                }
            }
            (Od::Table, b"table") => {
                if self.tables.is_empty() {
                    self.flush_list();
                }
                self.tables.push(Vec::new());
            }
            (Od::Table, b"table-row") => {
                if let Some(t) = self.tables.last_mut() {
                    t.push(Vec::new());
                    // Строка таблицы — тоже блок для предела.
                    if self.tables.len() == 1 {
                        return self.count();
                    }
                }
            }
            (Od::Table, b"table-cell" | b"covered-table-cell") => {
                if let Some(row) = self.tables.last_mut().and_then(|t| t.last_mut()) {
                    row.push(Vec::new());
                    return self.spend();
                }
            }
            (Od::Text, b"list") => {
                let style = attr(a, b"style-name").map(str::to_string).or_else(|| self.lists.last().cloned().flatten());
                self.lists.push(style);
            }
            (Od::Text, b"list-item" | b"list-header") => self.fresh_item = true,
            (Od::Text, b"p" | b"h") => {
                self.close_link();
                if let Some(p) = self.para.take() {
                    self.paragraph(p)?;
                }
                let heading = (name == b"h").then(|| attr(a, b"outline-level").and_then(|l| l.parse::<u8>().ok()).unwrap_or(1).clamp(1, 6));
                self.para = Some(Para { heading, ..Para::default() });
                self.para_marks = self.style(a);
                self.marks = self.para_marks;
            }
            (Od::Text, b"span") => self.marks = or(self.para_marks, self.style(a)),
            (Od::Text, b"a") => {
                if self.link.is_none() {
                    self.link = Some((attr(a, b"href").unwrap_or_default().to_string(), Vec::new()));
                }
            }
            (Od::Text, b"s") => {
                let n = attr(a, b"c").and_then(|v| v.parse().ok()).unwrap_or(1usize).min(1000);
                return self.text(&" ".repeat(n), self.marks);
            }
            (Od::Text, b"tab") => return self.text("\t", self.marks),
            (Od::Text, b"line-break") => {
                self.inline(Inline::LineBreak);
                return self.spend();
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }

    fn end(&mut self, ns: Od, name: &[u8]) -> ControlFlow<()> {
        match (ns, name) {
            (Od::Style, b"style") => {
                if let Some((name, m)) = self.style.take() {
                    self.styles.insert(name, m);
                }
            }
            (Od::Text, b"list-style") => self.list_style = None,
            (Od::Text, b"span") => self.marks = self.para_marks,
            (Od::Text, b"a") => self.close_link(),
            (Od::Text, b"p" | b"h") => {
                // Ссылка не закрылась в абзаце — её текст остаётся в нём.
                self.close_link();
                if let Some(p) = self.para.take() {
                    return self.paragraph(p);
                }
            }
            (Od::Text, b"list") => {
                self.lists.pop();
                if self.lists.is_empty() {
                    self.flush_list();
                }
            }
            (Od::Table, b"table") => {
                if let Some(rows) = self.tables.pop() {
                    return self.table(rows);
                }
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }

    /// Текст в абзац (или ссылку) с пределом символов; управляющие и bidi-символы убираются.
    fn text(&mut self, s: &str, marks: Marks) -> ControlFlow<()> {
        let left = self.limits.chars.saturating_sub(self.chars);
        let mut taken = 0usize;
        let clean: String = s
            .chars()
            .filter(|&c| c == '\t' || !c.is_control())
            .filter(|c| !matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
            .take_while(|_| {
                taken += 1;
                taken <= left
            })
            .collect();
        self.chars += clean.chars().count();
        if !clean.is_empty() {
            self.inline(Inline::Text { text: clean, marks });
        }
        if taken > left {
            self.truncated = true;
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
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

    fn inline(&mut self, i: Inline) {
        let target = match (&mut self.link, &mut self.para) {
            (Some((_, content)), _) => content,
            (None, Some(p)) => &mut p.inlines,
            (None, None) => return,
        };
        docx::push(target, i);
    }

    fn close_link(&mut self) {
        let Some((href, content)) = self.link.take() else { return };
        match check_link(&href, &plain_text(&content)) {
            Some(l) if !content.is_empty() => self.inline(Inline::Link { href: l.href, host: l.host, suspicious: l.suspicious, content }),
            // Запрещённая схема, ссылка внутри документа или пустая: остаётся только текст.
            _ => {
                for i in content {
                    self.inline(i);
                }
            }
        }
    }

    fn paragraph(&mut self, p: Para) -> ControlFlow<()> {
        let content = p.inlines;
        if let Some(rows) = self.tables.last_mut() {
            // Абзац ячейки — строкой ячейки.
            if rows.is_empty() {
                rows.push(Vec::new());
            }
            let Some(row) = rows.last_mut() else { return ControlFlow::Continue(()) };
            if row.is_empty() {
                row.push(Vec::new());
            }
            if let Some(cell) = row.last_mut() {
                if !cell.is_empty() && !content.is_empty() {
                    cell.push(Inline::LineBreak);
                }
                for i in content {
                    docx::push(cell, i);
                }
            }
            return ControlFlow::Continue(());
        }
        if !docx::is_blank(&content) {
            if let Some(level) = p.heading {
                self.flush_list();
                self.emit(Block::Heading { level, content })?;
            } else if let Some(style) = self.lists.last() {
                let level = u8::try_from(self.lists.len() - 1).unwrap_or(9).min(9);
                let ordered = style.as_ref().and_then(|s| self.list_styles.get(s)).is_some_and(|l| l[usize::from(level)]);
                match self.items.last_mut() {
                    Some(last) if !self.fresh_item && last.0 == level => {
                        last.2.push(Inline::LineBreak);
                        for i in content {
                            docx::push(&mut last.2, i);
                        }
                    }
                    _ => {
                        self.items.push((level, ordered, content));
                        self.fresh_item = false;
                        self.count()?;
                    }
                }
            } else {
                self.emit(Block::Paragraph { align: Align::Start, content })?;
            }
        }
        for _ in 0..p.figures {
            self.flush_list();
            self.emit(Block::Figure)?;
        }
        ControlFlow::Continue(())
    }

    fn table(&mut self, rows: Rows) -> ControlFlow<()> {
        if self.tables.is_empty() {
            if !rows.iter().flatten().all(|c| c.is_empty()) {
                self.out.push(Block::Table(rows));
            }
            return ControlFlow::Continue(());
        }
        // Вложенная таблица — строками в ячейке внешней: ячейки через « | ».
        let mut flat: Vec<Inline> = Vec::new();
        for row in rows {
            let cells: Vec<Vec<Inline>> = row.into_iter().filter(|c| !c.is_empty()).collect();
            if cells.is_empty() {
                continue;
            }
            if !flat.is_empty() {
                flat.push(Inline::LineBreak);
            }
            for (n, cell) in cells.into_iter().enumerate() {
                if n > 0 {
                    docx::push(&mut flat, Inline::text(" | "));
                }
                for i in cell {
                    docx::push(&mut flat, i);
                }
            }
        }
        if !flat.is_empty() {
            self.paragraph(Para { inlines: flat, ..Para::default() })?;
        }
        ControlFlow::Continue(())
    }

    fn emit(&mut self, b: Block) -> ControlFlow<()> {
        self.out.push(b);
        self.count()
    }

    fn count(&mut self) -> ControlFlow<()> {
        self.blocks += 1;
        if self.blocks >= self.limits.blocks {
            self.truncated = true;
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    }

    fn flush_list(&mut self) {
        if !self.items.is_empty() {
            let items = std::mem::take(&mut self.items);
            self.out.push(docx::list(items));
        }
    }

    /// Конец разбора (в том числе по пределу): недособранное — как есть.
    fn finish(mut self) -> Vec<Block> {
        self.close_link();
        if let Some(p) = self.para.take() {
            let _ = self.paragraph(p);
        }
        while let Some(rows) = self.tables.pop() {
            let _ = self.table(rows);
        }
        self.flush_list();
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preview::sheet::tests::zip_of;
    use crate::preview::{Preview, PreviewKind, Section, SectionTitle, preview};

    const NS: &str = r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" xmlns:xlink="http://www.w3.org/1999/xlink""#;

    fn odt_of(styles: &str, body: &str) -> Vec<u8> {
        let content = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><office:document-content {NS}><office:automatic-styles>{styles}</office:automatic-styles><office:body><office:text><text:sequence-decls><text:sequence-decl text:name="Figure"/></text:sequence-decls>{body}</office:text></office:body></office:document-content>"#
        );
        zip_of(&[("mimetype", b"application/vnd.oasis.opendocument.text"), ("content.xml", content.as_bytes())])
    }

    fn t(s: &str) -> Inline {
        Inline::text(s)
    }

    fn p(content: Vec<Inline>) -> Block {
        Block::Paragraph { align: Align::Start, content }
    }

    #[test]
    fn heading_marks_list_table_and_links() {
        let styles = r#"<style:style style:name="P1" style:family="paragraph"><style:text-properties fo:font-style="italic"/></style:style>
            <style:style style:name="T1" style:family="text"><style:text-properties fo:font-weight="bold"/></style:style>
            <text:list-style style:name="L1"><text:list-level-style-number text:level="1"/><text:list-level-style-bullet text:level="2"/></text:list-style>"#;
        let body = r#"<text:h text:style-name="Heading_20_1" text:outline-level="2">Отчёт</text:h>
            <text:p>Обычный, <text:span text:style-name="T1">жирный</text:span><text:s text:c="2"/>текст
              с переносом<text:line-break/>и <text:a xlink:href="https://example.com/a">сайт</text:a> <text:a xlink:href="javascript:alert(1)">скрипт</text:a><text:note><text:note-body><text:p>сноска</text:p></text:note-body></text:note></text:p>
            <text:p text:style-name="P1">Курсив<draw:frame><draw:image/><draw:text-box><text:p>надпись</text:p></draw:text-box></draw:frame></text:p>
            <text:list text:style-name="L1"><text:list-item><text:p>Один</text:p><text:p>ещё строка</text:p>
              <text:list><text:list-item><text:p>вложенный</text:p></text:list-item></text:list></text:list-item>
              <text:list-item><text:p>Два</text:p></text:list-item></text:list>
            <table:table><table:table-column/><table:table-row><table:table-cell><text:p>Имя</text:p></table:table-cell><table:table-cell><text:p>Срок</text:p></table:table-cell></table:table-row>
              <table:table-row><table:table-cell><text:p>Смета</text:p><text:p>v2</text:p></table:table-cell><table:covered-table-cell/></table:table-row></table:table>
            <text:p/><office:annotation><text:p>примечание</text:p></office:annotation>"#;
        let (b, cut) = odt(&odt_of(styles, body)).unwrap();
        assert!(!cut);
        let bold = Marks { bold: true, ..Marks::default() };
        let it = Marks { italic: true, ..Marks::default() };
        let link =
            Inline::Link { href: "https://example.com/a".into(), host: Some("example.com".into()), suspicious: false, content: vec![t("сайт")] };
        assert_eq!(
            b,
            [
                Block::Heading { level: 2, content: vec![t("Отчёт")] },
                p(vec![
                    t("Обычный, "),
                    Inline::Text { text: "жирный".into(), marks: bold },
                    t("  текст с переносом"),
                    Inline::LineBreak,
                    t("и "),
                    link,
                    t(" скрипт"),
                ]),
                p(vec![Inline::Text { text: "Курсив".into(), marks: it }]),
                Block::Figure,
                Block::List {
                    ordered: true,
                    items: vec![
                        vec![
                            p(vec![t("Один"), Inline::LineBreak, t("ещё строка")]),
                            Block::List { ordered: false, items: vec![vec![p(vec![t("вложенный")])]] }
                        ],
                        vec![p(vec![t("Два")])],
                    ]
                },
                Block::Table(vec![vec![vec![t("Имя")], vec![t("Срок")]], vec![vec![t("Смета"), Inline::LineBreak, t("v2")], vec![]]]),
            ]
        );
    }

    #[test]
    fn document_kind_sniffs_odt_and_limits_apply() {
        let doc = odt_of("", "<text:p>x</text:p>");
        assert!(is_odf(&doc));
        let (pv, _) = preview(PreviewKind::Document, &doc, 0, 0, 0).unwrap();
        assert_eq!(pv, Preview::Document { sections: vec![Section { title: SectionTitle::None, blocks: vec![p(vec![t("x")])] }], truncated: false });
        let many = odt_of("", &"<text:p>абзац</text:p>".repeat(50));
        let (b, cut) = parse(&many, Limits { blocks: 10, ..LIMITS }).unwrap();
        assert!(cut && b.len() == 10);
        let (b, cut) = parse(&many, Limits { xml: 1500, ..LIMITS }).unwrap();
        assert!(cut && !b.is_empty() && b.len() < 50);
        let dtd = br#"<?xml version="1.0"?><!DOCTYPE d [<!ENTITY x "y">]><office:document-content/>"#;
        assert!(odt(&zip_of(&[("content.xml", dtd)])).is_err());
        assert!(odt(&zip_of(&[("a.txt", b"x")])).is_err());
    }
}
