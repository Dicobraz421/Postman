//! Просмотр презентации (S8-18): `.pptx`/`.ppsx`/`.potx` и `.odp` → раздел на слайд. Только здесь, в песочнице.
//!
//! Защиты — как у [`docx`] и [`sheet`]: часть читается потоково не больше [`docx::MAX_XML`] байт после распаковки, все
//! слайды и заметки вместе — не больше [`MAX_UNPACKED`], служебные части — не больше [`docx::MAX_PART`]; XML без DTD
//! и чужих сущностей, с пределом вложенности. Наружу — до [`MAX_SLIDES`] слайдов, блоков и символов — как у `.docx`
//! ([`docx::MAX_BLOCKS`], [`docx::MAX_CHARS`]); остальное — `truncated`. Время ограничивает сторож в `serve`.
//!
//! `.pptx`: слайды по порядку `p:sldIdLst` из `ppt/presentation.xml` и его связей; на слайд — раздел с номером:
//! заголовок (место `title`/`ctrTitle`) — `Heading`, текст фигур — абзацы (`a:p`, `a:r`; жирный, курсив, подчёркнутый,
//! зачёркнутый), пункты (маркер `a:buChar`/`a:buAutoNum` или текст в месте для содержимого без `a:buNone`) — списки
//! по уровню `lvl`, таблицы (`a:tbl`) — `Table`, заметки докладчика (`notesSlide`) — `Quote` в конце раздела;
//! картинки, диаграммы, SmartArt и объекты — блоком `Figure` (подпись — у интерфейса); дата, колонтитул и номер
//! слайда не выводятся.
//! `.odp`: `draw:page` из `content.xml` так же: рамка `presentation:class="title"` — заголовок, `text:list` — пункты,
//! `table:table` — таблица, `presentation:notes` — заметки; жирный и курсив — из автоматических стилей.

use std::collections::HashMap;
use std::io;
use std::ops::ControlFlow;

use postman_ir::{Align, Block, Inline, Marks};

use super::docx::{self, Ev, attr, part, rels, resolve, walk};
use super::sheet::{Od, od, read};
use super::{MAX_UNPACKED, Section, SectionTitle};

/// Слайдов в ответе.
pub const MAX_SLIDES: usize = 500;

/// Пределы разбора; в тестах — меньше.
#[derive(Debug, Clone, Copy)]
struct Limits {
    /// Одна часть и все части после распаковки.
    xml: u64,
    unpacked: u64,
    slides: usize,
    blocks: usize,
    chars: usize,
}

const LIMITS: Limits = Limits { xml: docx::MAX_XML, unpacked: MAX_UNPACKED, slides: MAX_SLIDES, blocks: docx::MAX_BLOCKS, chars: docx::MAX_CHARS };

type Zip<'a> = zip::ZipArchive<io::Cursor<&'a [u8]>>;

/// Презентация `.pptx`/`.odp` → раздел на слайд и признак «показано начало».
pub fn slides(bytes: &[u8]) -> Result<(Vec<Section>, bool), String> {
    parse(bytes, LIMITS)
}

fn parse(bytes: &[u8], limits: Limits) -> Result<(Vec<Section>, bool), String> {
    if !bytes.starts_with(b"PK\x03\x04") {
        return Err("не презентация .pptx или .odp (старый .ppt или защищена паролем)".into());
    }
    let mut zip = zip::ZipArchive::new(io::Cursor::new(bytes)).map_err(|e| format!("презентация не разобрана: {e}"))?;
    let mut b = Builder::new(limits);
    if zip.by_name("content.xml").is_ok() && zip.by_name("[Content_Types].xml").is_err() {
        let mut p = OdpPart::default();
        let read =
            read(&mut zip, "content.xml", b.part_left(), od, |ev| p.event(ev, &mut b)).map_err(|e| format!("презентация не разобрана: {e}"))?;
        b.spent(read);
    } else {
        pptx(&mut zip, &mut b)?;
    }
    if b.sections.is_empty() {
        return Err("в презентации нет слайдов".into());
    }
    Ok((b.sections, b.truncated))
}

// --- сборка слайда ---

/// Чей текст: заголовок, место для содержимого (абзацы — пункты), прочая фигура или невидимое (дата, номер слайда,
/// картинка слайда в заметках).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Kind {
    #[default]
    Plain,
    Title,
    Body,
    Hidden,
}

/// Абзац в сборке: уровень, маркер (`Some(нумерованный)`), «маркера нет» явно и текст.
#[derive(Default)]
struct Para {
    level: u8,
    bullet: Option<bool>,
    no_bullet: bool,
    inlines: Vec<Inline>,
}

/// Строки → ячейки → инлайны.
type Rows = Vec<Vec<Vec<Inline>>>;

struct Builder {
    limits: Limits,
    sections: Vec<Section>,
    /// Текущий слайд и его заметки; `notes` — текст идёт в заметки.
    blocks: Vec<Block>,
    notes: Vec<Block>,
    in_notes: bool,
    kind: Kind,
    /// Пункты текущей фигуры: уровень, нумерованный ли, текст.
    list: Vec<(u8, bool, Vec<Inline>)>,
    para: Option<Para>,
    table: Option<Rows>,
    count: usize,
    chars: usize,
    unpacked: u64,
    truncated: bool,
}

impl Builder {
    fn new(limits: Limits) -> Self {
        Builder {
            limits,
            sections: Vec::new(),
            blocks: Vec::new(),
            notes: Vec::new(),
            in_notes: false,
            kind: Kind::Plain,
            list: Vec::new(),
            para: None,
            table: None,
            count: 0,
            chars: 0,
            unpacked: 0,
            truncated: false,
        }
    }

    fn part_left(&self) -> u64 {
        self.limits.xml.min(self.limits.unpacked.saturating_sub(self.unpacked))
    }

    fn spent(&mut self, (used, hit): (u64, bool)) {
        self.unpacked += used;
        self.truncated |= hit;
    }

    /// Новый слайд; `false` — слайдов уже предел.
    fn start_slide(&mut self) -> bool {
        if self.sections.len() >= self.limits.slides {
            self.truncated = true;
            return false;
        }
        self.in_notes = false;
        self.kind = Kind::Plain;
        true
    }

    /// Конец слайда (в том числе по пределу): недособранное — как есть, заметки — цитатой в конце.
    fn end_slide(&mut self) {
        let _ = self.end_para();
        let _ = self.end_table();
        self.flush_list();
        let mut blocks = std::mem::take(&mut self.blocks);
        let notes = std::mem::take(&mut self.notes);
        if !notes.is_empty() {
            blocks.push(Block::Quote(notes));
        }
        let n = self.sections.len() as u32 + 1;
        self.sections.push(Section { title: SectionTitle::Slide(n), blocks });
        (self.in_notes, self.kind) = (false, Kind::Plain);
    }

    fn target(&mut self) -> &mut Vec<Block> {
        if self.in_notes { &mut self.notes } else { &mut self.blocks }
    }

    /// Новая фигура (или её конец): пункты прежней — списком.
    fn shape(&mut self, kind: Kind) {
        self.flush_list();
        self.kind = kind;
    }

    fn flush_list(&mut self) {
        if !self.list.is_empty() {
            let items = std::mem::take(&mut self.list);
            let list = docx::list(items);
            self.target().push(list);
        }
    }

    /// Текст в абзац с пределом символов; управляющие и bidi-символы убираются.
    fn text(&mut self, s: &str, marks: Marks) -> ControlFlow<()> {
        if self.kind == Kind::Hidden || self.para.is_none() {
            return ControlFlow::Continue(());
        }
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
        if let Some(p) = &mut self.para
            && !clean.is_empty()
        {
            docx::push(&mut p.inlines, Inline::Text { text: clean, marks });
        }
        if taken > left {
            self.truncated = true;
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    }

    fn line_break(&mut self) -> ControlFlow<()> {
        if self.kind == Kind::Hidden {
            return ControlFlow::Continue(());
        }
        let Some(p) = &mut self.para else { return ControlFlow::Continue(()) };
        p.inlines.push(Inline::LineBreak);
        self.spend()
    }

    /// Разрыв строки и ячейка — по символу из предела (их тысячи в маленьком XML).
    fn spend(&mut self) -> ControlFlow<()> {
        self.chars += 1;
        if self.chars > self.limits.chars {
            self.truncated = true;
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    }

    fn end_para(&mut self) -> ControlFlow<()> {
        let Some(p) = self.para.take() else { return ControlFlow::Continue(()) };
        if self.kind == Kind::Hidden {
            return ControlFlow::Continue(());
        }
        if let Some(rows) = &mut self.table {
            // Абзац ячейки — строкой ячейки.
            if let Some(cell) = rows.last_mut().and_then(|r| r.last_mut()) {
                if !cell.is_empty() && !p.inlines.is_empty() {
                    cell.push(Inline::LineBreak);
                }
                for i in p.inlines {
                    docx::push(cell, i);
                }
            }
            return ControlFlow::Continue(());
        }
        if docx::is_blank(&p.inlines) {
            return ControlFlow::Continue(());
        }
        if self.kind == Kind::Title {
            self.flush_list();
            return self.emit(Block::Heading { level: 1, content: p.inlines });
        }
        let bullet = if p.no_bullet { None } else { p.bullet.or((self.kind == Kind::Body).then_some(false)) };
        if let Some(ordered) = bullet {
            self.list.push((p.level, ordered, p.inlines));
            return self.counted();
        }
        self.flush_list();
        self.emit(Block::Paragraph { align: Align::Start, content: p.inlines })
    }

    fn emit(&mut self, b: Block) -> ControlFlow<()> {
        self.target().push(b);
        self.counted()
    }

    fn counted(&mut self) -> ControlFlow<()> {
        self.count += 1;
        if self.count >= self.limits.blocks {
            self.truncated = true;
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    }

    /// Картинка, диаграмма, объект — блоком `Figure` (в ячейке таблицы и в невидимом — нет).
    fn picture(&mut self) -> ControlFlow<()> {
        if self.kind == Kind::Hidden || self.table.is_some() {
            return ControlFlow::Continue(());
        }
        self.flush_list();
        self.emit(Block::Figure)
    }

    fn start_table(&mut self) {
        let _ = self.end_table();
        self.flush_list();
        self.table = Some(Vec::new());
    }

    /// Строка таблицы — тоже блок для предела.
    fn row(&mut self) -> ControlFlow<()> {
        let Some(rows) = &mut self.table else { return ControlFlow::Continue(()) };
        rows.push(Vec::new());
        self.counted()
    }

    fn cell(&mut self) -> ControlFlow<()> {
        let Some(row) = self.table.as_mut().and_then(|t| t.last_mut()) else { return ControlFlow::Continue(()) };
        row.push(Vec::new());
        self.spend()
    }

    fn end_table(&mut self) -> ControlFlow<()> {
        let Some(rows) = self.table.take() else { return ControlFlow::Continue(()) };
        if self.kind != Kind::Hidden && !rows.iter().flatten().all(|c| c.is_empty()) {
            self.target().push(Block::Table(rows));
        }
        ControlFlow::Continue(())
    }
}

// --- pptx ---

/// Пространства имён PresentationML и DrawingML (переходные и строгие), совместимость разметки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pn {
    A,
    P,
    Mc,
    Other,
}

fn pn(uri: &[u8]) -> Pn {
    match uri {
        b"http://schemas.openxmlformats.org/drawingml/2006/main" | b"http://purl.oclc.org/ooxml/drawingml/main" => Pn::A,
        b"http://schemas.openxmlformats.org/presentationml/2006/main" | b"http://purl.oclc.org/ooxml/presentationml/main" => Pn::P,
        b"http://schemas.openxmlformats.org/markup-compatibility/2006" => Pn::Mc,
        _ => Pn::Other,
    }
}

/// Каталог и имя части: «ppt/slides/slide1.xml» → «ppt/slides/», «slide1.xml».
fn split(path: &str) -> (String, String) {
    match path.rsplit_once('/') {
        Some((d, f)) => (format!("{d}/"), f.to_string()),
        None => (String::new(), path.to_string()),
    }
}

fn pptx(zip: &mut Zip, b: &mut Builder) -> Result<(), String> {
    // Основная часть — по связи пакета, обычно `ppt/presentation.xml`.
    let main = part(zip, "_rels/.rels")
        .and_then(|d| rels(&d).ok())
        .and_then(|r| r.into_iter().find(|r| r.kind.ends_with("/officeDocument")).map(|r| resolve("", &r.target)))
        .unwrap_or_else(|| "ppt/presentation.xml".into());
    let (dir, file) = split(&main);
    let links = part(zip, &format!("{dir}_rels/{file}.rels")).and_then(|d| rels(&d).ok()).unwrap_or_default();
    let presentation = part(zip, &main).ok_or("в презентации нет слайдов")?;
    let mut order: Vec<String> = Vec::new();
    walk(presentation.as_slice(), pn, |ev| {
        if let Ev::Start(Pn::P, b"sldId", a) = ev
            && let Some(id) = attr(a, b"r:id")
        {
            order.push(id.to_string());
        }
        ControlFlow::Continue(())
    })
    .map_err(|e| format!("презентация не разобрана: {e}"))?;
    let slides: Vec<String> =
        order.iter().filter_map(|id| links.iter().find(|r| r.id == *id && r.kind.ends_with("/slide"))).map(|r| resolve(&dir, &r.target)).collect();
    for path in slides {
        if !b.start_slide() {
            break;
        }
        let (sdir, sfile) = split(&path);
        let notes = part(zip, &format!("{sdir}_rels/{sfile}.rels"))
            .and_then(|d| rels(&d).ok())
            .and_then(|r| r.into_iter().find(|r| r.kind.ends_with("/notesSlide")))
            .map(|r| resolve(&sdir, &r.target));
        let mut parts = vec![(path, false)];
        parts.extend(notes.map(|n| (n, true)));
        for (name, notes) in parts {
            if b.truncated {
                break;
            }
            b.in_notes = notes;
            b.kind = if notes { Kind::Hidden } else { Kind::Plain };
            let mut p = PptxPart { notes, ..PptxPart::default() };
            let read = read(zip, &name, b.part_left(), pn, |ev| p.event(ev, b)).map_err(|e| format!("презентация не разобрана: {e}"))?;
            b.spent(read);
        }
        b.end_slide();
        if b.truncated {
            break;
        }
    }
    Ok(())
}

/// Слайд или его заметки в разборе.
#[derive(Default)]
struct PptxPart {
    notes: bool,
    skip: usize,
    in_t: bool,
    marks: Marks,
}

impl PptxPart {
    /// Фигура без места (надпись, автофигура): на слайде — текст, в заметках — не видна.
    fn plain(&self) -> Kind {
        if self.notes { Kind::Hidden } else { Kind::Plain }
    }

    /// Место (`p:ph`) по типу; без типа — место для содержимого.
    fn placeholder(&self, ty: Option<&str>) -> Kind {
        match (self.notes, ty) {
            (true, Some("body")) => Kind::Plain,
            (true, _) => Kind::Hidden,
            (false, Some("title" | "ctrTitle")) => Kind::Title,
            (false, None | Some("body" | "obj")) => Kind::Body,
            (false, Some("dt" | "ftr" | "hdr" | "sldNum")) => Kind::Hidden,
            (false, _) => Kind::Plain,
        }
    }

    fn event(&mut self, ev: Ev<'_, Pn>, b: &mut Builder) -> ControlFlow<()> {
        match ev {
            Ev::Start(..) if self.skip > 0 => self.skip += 1,
            Ev::End(..) if self.skip > 0 => self.skip -= 1,
            // Из `mc:AlternateContent` — только `Choice` (запасной вариант — то же ещё раз).
            Ev::Start(Pn::Mc, b"Fallback", _) => self.skip = 1,
            Ev::Start(Pn::P, b"sp" | b"graphicFrame" | b"cxnSp", _) | Ev::End(Pn::P, b"sp" | b"graphicFrame" | b"cxnSp") => b.shape(self.plain()),
            Ev::Start(Pn::P, b"ph", a) => b.kind = self.placeholder(attr(a, b"type")),
            Ev::Start(Pn::P, b"pic", _) => {
                self.skip = 1;
                return b.picture();
            }
            // Рамка не с таблицей — диаграмма, SmartArt, объект.
            Ev::Start(Pn::A, b"graphicData", a) if !attr(a, b"uri").is_some_and(|u| u.ends_with("/table")) => {
                self.skip = 1;
                return b.picture();
            }
            Ev::Start(Pn::A, b"tbl", _) => b.start_table(),
            Ev::End(Pn::A, b"tbl") => return b.end_table(),
            Ev::Start(Pn::A, b"tr", _) => return b.row(),
            Ev::Start(Pn::A, b"tc", _) => return b.cell(),
            Ev::Start(Pn::A, b"p", _) => b.para = Some(Para::default()),
            Ev::End(Pn::A, b"p") => return b.end_para(),
            Ev::Start(Pn::A, b"pPr", a) => {
                if let Some(p) = &mut b.para {
                    p.level = attr(a, b"lvl").and_then(|v| v.parse().ok()).unwrap_or(0u8).min(8);
                }
            }
            Ev::Start(Pn::A, name @ (b"buChar" | b"buBlip" | b"buAutoNum" | b"buNone"), _) => {
                if let Some(p) = &mut b.para {
                    match name {
                        b"buNone" => p.no_bullet = true,
                        b"buAutoNum" => p.bullet = Some(true),
                        _ => p.bullet = Some(false),
                    }
                }
            }
            Ev::Start(Pn::A, b"r" | b"fld", _) => self.marks = Marks::default(),
            Ev::Start(Pn::A, b"rPr", a) => {
                let on = |key: &[u8], off: &str| attr(a, key).is_some_and(|v| v != off && v != "0" && v != "false");
                self.marks =
                    Marks { bold: on(b"b", "0"), italic: on(b"i", "0"), underline: on(b"u", "none"), strike: on(b"strike", "noStrike"), code: false };
            }
            Ev::Start(Pn::A, b"t", _) => self.in_t = true,
            Ev::End(Pn::A, b"t") => self.in_t = false,
            Ev::Text(t) if self.in_t => return b.text(t, self.marks),
            Ev::Start(Pn::A, b"br", _) => return b.line_break(),
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

// --- odp ---

/// Марки текста из свойств стиля OpenDocument.
pub(super) fn odf_marks(a: &[(Vec<u8>, String)]) -> Marks {
    let on = |key: &[u8]| attr(a, key).is_some_and(|v| v != "none");
    Marks {
        bold: attr(a, b"font-weight").is_some_and(|v| v == "bold" || v.parse::<u16>().is_ok_and(|w| w >= 600)),
        italic: matches!(attr(a, b"font-style"), Some("italic" | "oblique")),
        underline: on(b"text-underline-style"),
        strike: on(b"text-line-through-style"),
        code: false,
    }
}

pub(super) fn or(a: Marks, b: Marks) -> Marks {
    Marks { bold: a.bold || b.bold, italic: a.italic || b.italic, underline: a.underline || b.underline, strike: a.strike || b.strike, code: false }
}

/// `content.xml` презентации в разборе: автоматические стили (имя → марки), глубина списка, марки абзаца и фрагмента.
#[derive(Default)]
struct OdpPart {
    skip: usize,
    styles: HashMap<String, Marks>,
    style: Option<(String, Marks)>,
    lists: usize,
    para_marks: Marks,
    marks: Marks,
    /// У рамки уже есть `Figure` (картинка-замена объекта — та же).
    pictured: bool,
}

impl OdpPart {
    fn style(&self, a: &[(Vec<u8>, String)]) -> Marks {
        attr(a, b"style-name").and_then(|n| self.styles.get(n)).copied().unwrap_or_default()
    }

    fn event(&mut self, ev: Ev<'_, Od>, b: &mut Builder) -> ControlFlow<()> {
        let plain = |b: &Builder| if b.in_notes { Kind::Hidden } else { Kind::Plain };
        match ev {
            Ev::Start(..) if self.skip > 0 => self.skip += 1,
            Ev::End(..) if self.skip > 0 => self.skip -= 1,
            // Примечания и сноски — не текст слайда.
            Ev::Start(Od::Office, b"annotation", _) | Ev::Start(Od::Text, b"note", _) => self.skip = 1,
            Ev::Start(Od::Style, b"style", a) => self.style = attr(a, b"name").map(|n| (n.to_string(), Marks::default())),
            Ev::Start(Od::Style, b"text-properties", a) => {
                if let Some((_, m)) = &mut self.style {
                    *m = odf_marks(a);
                }
            }
            Ev::End(Od::Style, b"style") => {
                if let Some((name, m)) = self.style.take() {
                    self.styles.insert(name, m);
                }
            }
            Ev::Start(Od::Draw, b"page", _) => {
                if !b.start_slide() {
                    return ControlFlow::Break(());
                }
            }
            Ev::End(Od::Draw, b"page") => b.end_slide(),
            Ev::Start(Od::Presentation, b"notes", _) => {
                b.shape(Kind::Hidden);
                b.in_notes = true;
            }
            Ev::End(Od::Presentation, b"notes") => {
                b.shape(Kind::Hidden);
                (b.in_notes, b.kind) = (false, Kind::Plain);
            }
            Ev::Start(Od::Draw, b"frame", a) => {
                let kind = match (b.in_notes, attr(a, b"class")) {
                    (true, Some("notes")) => Kind::Plain,
                    (true, _) => Kind::Hidden,
                    (false, Some("title")) => Kind::Title,
                    (false, Some("header" | "footer" | "date-time" | "page-number")) => Kind::Hidden,
                    (false, _) => Kind::Plain,
                };
                b.shape(kind);
                self.pictured = false;
            }
            Ev::End(Od::Draw, b"frame") => b.shape(plain(b)),
            Ev::Start(Od::Draw, b"image" | b"object" | b"object-ole" | b"plugin", _) => {
                self.skip = 1;
                if !self.pictured {
                    self.pictured = true;
                    return b.picture();
                }
            }
            Ev::Start(Od::Table, b"table", _) => b.start_table(),
            Ev::End(Od::Table, b"table") => return b.end_table(),
            Ev::Start(Od::Table, b"table-row", _) => return b.row(),
            Ev::Start(Od::Table, b"table-cell" | b"covered-table-cell", _) => return b.cell(),
            Ev::Start(Od::Text, b"list", _) => self.lists += 1,
            Ev::End(Od::Text, b"list") => self.lists = self.lists.saturating_sub(1),
            Ev::Start(Od::Text, b"p" | b"h", a) => {
                self.para_marks = self.style(a);
                self.marks = self.para_marks;
                let level = u8::try_from(self.lists.saturating_sub(1)).unwrap_or(8).min(8);
                b.para = Some(Para { level, bullet: (self.lists > 0).then_some(false), no_bullet: self.lists == 0, inlines: Vec::new() });
            }
            Ev::End(Od::Text, b"p" | b"h") => return b.end_para(),
            Ev::Start(Od::Text, b"span", a) => self.marks = or(self.para_marks, self.style(a)),
            Ev::End(Od::Text, b"span") => self.marks = self.para_marks,
            Ev::Start(Od::Text, b"s", a) => {
                let n = attr(a, b"c").and_then(|v| v.parse().ok()).unwrap_or(1usize).min(1000);
                return b.text(&" ".repeat(n), self.marks);
            }
            Ev::Start(Od::Text, b"tab", _) => return b.text("\t", self.marks),
            Ev::Start(Od::Text, b"line-break", _) => return b.line_break(),
            Ev::Text(t) => return b.text(t, self.marks),
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preview::sheet::tests::zip_of;
    use crate::preview::{OfficeScan, Preview, PreviewKind, preview};

    const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    const NS: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#;

    fn rels_of(links: &str) -> String {
        format!(r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{links}</Relationships>"#)
    }

    /// `.pptx` из слайдов (`p:spTree` и заметки), порядок — `order` (номера файлов); `extra` — ещё части.
    fn pptx_of(slides: &[(&str, Option<&str>)], order: &[usize], extra: &[(&str, &[u8])]) -> Vec<u8> {
        let mut files: Vec<(String, Vec<u8>)> = vec![
            ("[Content_Types].xml".into(), b"<Types/>".to_vec()),
            (
                "_rels/.rels".into(),
                rels_of(&format!(r#"<Relationship Id="rId1" Type="{REL}/officeDocument" Target="ppt/presentation.xml"/>"#)).into_bytes(),
            ),
        ];
        let mut links = String::new();
        for (i, (tree, notes)) in slides.iter().enumerate() {
            let n = i + 1;
            links.push_str(&format!(r#"<Relationship Id="rId{n}" Type="{REL}/slide" Target="slides/slide{n}.xml"/>"#));
            let xml = format!(r#"<?xml version="1.0" encoding="UTF-8"?><p:sld {NS}><p:cSld><p:spTree>{tree}</p:spTree></p:cSld></p:sld>"#);
            files.push((format!("ppt/slides/slide{n}.xml"), xml.into_bytes()));
            if let Some(notes) = notes {
                let link = format!(r#"<Relationship Id="rId9" Type="{REL}/notesSlide" Target="../notesSlides/notesSlide{n}.xml"/>"#);
                files.push((format!("ppt/slides/_rels/slide{n}.xml.rels"), rels_of(&link).into_bytes()));
                let xml = format!(r#"<p:notes {NS}><p:cSld><p:spTree>{notes}</p:spTree></p:cSld></p:notes>"#);
                files.push((format!("ppt/notesSlides/notesSlide{n}.xml"), xml.into_bytes()));
            }
        }
        links.push_str(&format!(r#"<Relationship Id="rIdM" Type="{REL}/slideMaster" Target="slideMasters/slideMaster1.xml"/>"#));
        let ids: String = order.iter().enumerate().map(|(i, n)| format!(r#"<p:sldId id="{}" r:id="rId{n}"/>"#, 256 + i)).collect();
        let presentation = format!(
            r#"<p:presentation {NS}><p:sldMasterIdLst><p:sldMasterId id="1" r:id="rIdM"/></p:sldMasterIdLst><p:sldIdLst>{ids}</p:sldIdLst></p:presentation>"#
        );
        files.push(("ppt/presentation.xml".into(), presentation.into_bytes()));
        files.push(("ppt/_rels/presentation.xml.rels".into(), rels_of(&links).into_bytes()));
        let mut all: Vec<(&str, &[u8])> = files.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect();
        all.extend_from_slice(extra);
        zip_of(&all)
    }

    /// Фигура: место `ph` (тип или пусто — без места; `Some("")` — место без типа) и абзацы `a:p`.
    fn sp(ph: Option<&str>, paras: &str) -> String {
        let ph = match ph {
            None => String::new(),
            Some("") => "<p:ph idx=\"1\"/>".into(),
            Some(t) => format!(r#"<p:ph type="{t}"/>"#),
        };
        format!(
            r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="x"/><p:cNvSpPr/><p:nvPr>{ph}</p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:lstStyle><a:lvl1pPr><a:buChar char="•"/></a:lvl1pPr></a:lstStyle>{paras}</p:txBody></p:sp>"#
        )
    }

    fn ap(text: &str) -> String {
        format!("<a:p><a:r><a:rPr lang=\"ru-RU\"/><a:t>{text}</a:t></a:r></a:p>")
    }

    fn t(s: &str) -> Inline {
        Inline::text(s)
    }

    fn p(content: Vec<Inline>) -> Block {
        Block::Paragraph { align: Align::Start, content }
    }

    fn section(n: u32, blocks: Vec<Block>) -> Section {
        Section { title: SectionTitle::Slide(n), blocks }
    }

    #[test]
    fn pptx_slides_in_sldidlst_order_with_titles_lists_tables_and_notes() {
        let title = sp(Some("title"), &ap("План"));
        let body = sp(
            Some(""),
            &format!(
                r#"{}<a:p><a:pPr lvl="1"/><a:r><a:t>вложенный</a:t></a:r></a:p><a:p><a:pPr><a:buNone/></a:pPr><a:r><a:t>без маркера</a:t></a:r></a:p>"#,
                ap("Первый")
            ),
        );
        let text = sp(
            None,
            r#"<a:p><a:r><a:rPr b="1"/><a:t>Жир</a:t></a:r><a:r><a:rPr i="1" u="sng"/><a:t> курсив</a:t></a:r><a:br/><a:r><a:t>строка</a:t></a:r></a:p>
               <a:p><a:pPr><a:buAutoNum type="arabicPeriod"/></a:pPr><a:r><a:t>раз</a:t></a:r></a:p><a:p><a:pPr><a:buAutoNum type="arabicPeriod"/></a:pPr><a:r><a:t>два</a:t></a:r></a:p>"#,
        );
        let footer = sp(Some("sldNum"), &ap("7"));
        let first = format!("{title}{body}{text}{footer}");
        let cell = |s: &str| format!("<a:tc><a:txBody><a:bodyPr/>{}</a:txBody></a:tc>", ap(s));
        let table = format!(
            r#"<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="4" name="t"/><p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/table"><a:tbl><a:tblGrid/><a:tr h="1">{}{}</a:tr><a:tr h="1">{}{}</a:tr></a:tbl></a:graphicData></a:graphic></p:graphicFrame>"#,
            cell("Имя"),
            cell("Срок"),
            cell("Смета"),
            cell("")
        );
        let chart = r#"<p:graphicFrame><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart xmlns:c="urn:c"><a:t>внутри</a:t></c:chart></a:graphicData></a:graphic></p:graphicFrame>"#;
        let pic = r#"<p:pic><p:nvPicPr><p:cNvPr id="5" name="рис"/></p:nvPicPr><p:blipFill/></p:pic>"#;
        let second = format!("{}{table}{chart}{pic}", sp(Some("ctrTitle"), &ap("Сроки")));
        let notes = format!("{}{}", sp(Some("sldImg"), ""), sp(Some("body"), &format!("{}{}", ap("Сказать про"), ap("бюджет"))));
        let third = sp(Some("subTitle"), &ap("Вопросы"));
        // Файлы — slide1..3, порядок показа — 2, 3, 1.
        let x = pptx_of(&[(&first, None), (&second, None), (&third, Some(&notes))], &[2, 3, 1], &[]);
        let (pv, _) = preview(PreviewKind::Slides, &x, 0, 0, 0).unwrap();
        let Preview::Document { sections, truncated } = pv else { panic!("{pv:?}") };
        assert!(!truncated);
        let bold = Marks { bold: true, ..Marks::default() };
        let iu = Marks { italic: true, underline: true, ..Marks::default() };
        assert_eq!(
            sections,
            [
                section(
                    1,
                    vec![
                        Block::Heading { level: 1, content: vec![t("Сроки")] },
                        Block::Table(vec![vec![vec![t("Имя")], vec![t("Срок")]], vec![vec![t("Смета")], vec![]]]),
                        Block::Figure,
                        Block::Figure,
                    ]
                ),
                section(2, vec![p(vec![t("Вопросы")]), Block::Quote(vec![p(vec![t("Сказать про")]), p(vec![t("бюджет")])])]),
                section(
                    3,
                    vec![
                        Block::Heading { level: 1, content: vec![t("План")] },
                        Block::List {
                            ordered: false,
                            items: vec![vec![p(vec![t("Первый")]), Block::List { ordered: false, items: vec![vec![p(vec![t("вложенный")])]] }]]
                        },
                        p(vec![t("без маркера")]),
                        p(vec![
                            Inline::Text { text: "Жир".into(), marks: bold },
                            Inline::Text { text: " курсив".into(), marks: iu },
                            Inline::LineBreak,
                            t("строка")
                        ]),
                        Block::List { ordered: true, items: vec![vec![p(vec![t("раз")])], vec![p(vec![t("два")])]] },
                    ]
                ),
            ]
        );
    }

    #[test]
    fn macro_presentations_are_office_scans() {
        let x = pptx_of(&[(&sp(None, &ap("x")), None)], &[1], &[("ppt/vbaProject.bin", b"x")]);
        let (pv, _) = preview(PreviewKind::Slides, &x, 0, 0, 0).unwrap();
        assert!(matches!(pv, Preview::Office(OfficeScan { macros: true, .. })), "{pv:?}");
    }

    #[test]
    fn odp_pages_like_pptx() {
        let content = r#"<?xml version="1.0" encoding="UTF-8"?><office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0">
            <office:automatic-styles><style:style style:name="T1" style:family="text"><style:text-properties fo:font-weight="bold"/></style:style></office:automatic-styles>
            <office:body><office:presentation>
              <draw:page draw:name="page1">
                <draw:frame presentation:class="title"><draw:text-box><text:p>Итоги</text:p></draw:text-box></draw:frame>
                <draw:frame presentation:class="outline"><draw:text-box><text:list><text:list-item><text:p>Рост <text:span text:style-name="T1">выручки</text:span></text:p>
                  <text:list><text:list-item><text:p>на<text:s text:c="2"/>10%</text:p></text:list-item></text:list></text:list-item></text:list></draw:text-box></draw:frame>
                <draw:frame presentation:class="graphic"><draw:object/><draw:image/></draw:frame>
                <draw:frame presentation:class="page-number"><draw:text-box><text:p>1</text:p></draw:text-box></draw:frame>
                <presentation:notes><draw:page-thumbnail/><draw:frame presentation:class="notes"><draw:text-box><text:p>Заметка</text:p></draw:text-box></draw:frame></presentation:notes>
              </draw:page>
              <draw:page draw:name="page2"><draw:frame><table:table><table:table-row><table:table-cell><text:p>a</text:p></table:table-cell><table:table-cell><text:p>b</text:p></table:table-cell></table:table-row></table:table></draw:frame></draw:page>
            </office:presentation></office:body></office:document-content>"#;
        let odp = zip_of(&[("mimetype", b"application/vnd.oasis.opendocument.presentation"), ("content.xml", content.as_bytes())]);
        let (s, cut) = slides(&odp).unwrap();
        assert!(!cut);
        let bold = Marks { bold: true, ..Marks::default() };
        assert_eq!(
            s,
            [
                section(
                    1,
                    vec![
                        Block::Heading { level: 1, content: vec![t("Итоги")] },
                        Block::List {
                            ordered: false,
                            items: vec![vec![
                                p(vec![t("Рост "), Inline::Text { text: "выручки".into(), marks: bold }]),
                                Block::List { ordered: false, items: vec![vec![p(vec![t("на  10%")])]] }
                            ]]
                        },
                        Block::Figure,
                        Block::Quote(vec![p(vec![t("Заметка")])]),
                    ]
                ),
                section(2, vec![Block::Table(vec![vec![vec![t("a")], vec![t("b")]]])]),
            ]
        );
    }

    #[test]
    fn broken_presentations_are_errors_and_big_ones_are_cut() {
        assert!(slides(b"plain").is_err());
        assert!(slides(b"PK\x03\x04 garbage").is_err());
        assert!(slides(&zip_of(&[("a.txt", b"x")])).is_err());
        let broken = pptx_of(&[(r#"<p:sp><p:txBody><a:p></p:txBody>"#, None)], &[1], &[]);
        assert!(slides(&broken).is_err());
        let dtd = br#"<?xml version="1.0"?><!DOCTYPE d [<!ENTITY x "y">]><office:document-content/>"#;
        assert!(slides(&zip_of(&[("content.xml", dtd)])).is_err());
        // Слайдов больше предела.
        let one = sp(None, &ap("x"));
        let many: Vec<(&str, Option<&str>)> = (0..5).map(|_| (one.as_str(), None)).collect();
        let x = pptx_of(&many, &[1, 2, 3, 4, 5], &[]);
        let (s, cut) = parse(&x, Limits { slides: 3, ..LIMITS }).unwrap();
        assert!(cut && s.len() == 3);
        // Блоки и символы.
        let (s, cut) = parse(&x, Limits { blocks: 2, ..LIMITS }).unwrap();
        assert!(cut && s.len() == 2);
        let (_, cut) = parse(&x, Limits { chars: 3, ..LIMITS }).unwrap();
        assert!(cut);
        // Zip-бомба: слайд в сотни раз больше архива — до предела части, итог `truncated`.
        let mut tree = sp(None, &ap("начало"));
        tree.push_str(&" ".repeat(4 << 20));
        tree.push_str(&sp(None, &ap("конец")));
        let x = pptx_of(&[(&tree, None), (&one, None)], &[1, 2], &[]);
        assert!(x.len() < 64 * 1024, "{}", x.len());
        let (s, cut) = parse(&x, Limits { xml: 1 << 20, ..LIMITS }).unwrap();
        assert!(cut);
        assert_eq!(s, [section(1, vec![p(vec![t("начало")])])]);
        let (s, cut) = parse(&x, Limits { unpacked: 1 << 20, ..LIMITS }).unwrap();
        assert!(cut && s.len() == 1);
    }
}
