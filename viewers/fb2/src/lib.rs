//! Модуль просмотра книги FictionBook `.fb2` (образец SDK, ADR 0007): XML → разделы по главам.
//!
//! Первый раздел — обложка: название и авторы из `description/title-info`, аннотация цитатой (у тела книги свой
//! `title` — он вместо названия из описания); дальше — раздел на каждую `section` верхнего уровня главного `body`
//! (вложенные — заголовками ниже), примечания (второй и следующие `body`) — одним разделом. Из текста берутся
//! абзацы, заголовки, подзаголовки, курсив (`emphasis`), жирный (`strong`), зачёркнутый, код, стихи (`poem`,
//! `stanza`, `v` — цитатой, строки — переносами), цитаты и эпиграфы, таблицы, внешние ссылки (проверяет хост;
//! сноски `#…` — текстом); картинки — пометкой. `binary` (картинки base64) не разбирается: на нём разбор кончается.
//!
//! Кодировка — из объявления XML: UTF-8 (по умолчанию) или Windows-1251. XML — свой потоковый разбор без DTD:
//! объявление типа пропускается, сущности — только пять встроенных и символьные ссылки, вложенность — не глубже
//! [`MAX_DEPTH`]. Пределы — свои, не больше общих ([`postman_viewer_sdk`]): показано начало — `truncated`.
//!
//! Топливо: интерпретатор хоста списывает его за весь линейный код участка (тело функции, итерация цикла) при входе,
//! а не за исполненные ветви, поэтому обработчики событий не встраиваются в цикл разбора (`#[inline(never)]`), а
//! текст не проверяется заменой негодного UTF-8 (`from_utf8_lossy` в интерпретаторе на порядок дороже строгой
//! проверки). Роман на [`MAX_CHARS`] символов — порядка 0,4·10⁹ топлива из 10⁹.

use std::borrow::Cow;

use postman_viewer_sdk::{Align, Block, Inline, Marks, Section, SectionTitle, ViewerOutput};

postman_viewer_sdk::viewer!(view);

/// Блоков в ответе (у хоста — [`postman_viewer_sdk::MAX_BLOCKS`]).
pub const MAX_BLOCKS: usize = 9_000;
/// Символов текста (у хоста — [`postman_viewer_sdk::MAX_CHARS`]).
pub const MAX_CHARS: usize = 900_000;
/// Ячеек таблиц (у хоста — [`postman_viewer_sdk::MAX_CELLS`]).
pub const MAX_CELLS: usize = 100_000;
/// Разделов: обложка, главы, примечания (у хоста — [`postman_viewer_sdk::MAX_SECTIONS`]).
pub const MAX_SECTIONS: usize = 400;
/// Вложенность цитат (стихи в эпиграфе в цитате…); глубже — содержимое в объемлющей (у хоста —
/// [`postman_viewer_sdk::MAX_NESTING`]).
pub const MAX_QUOTES: usize = 8;
/// Вложенность элементов XML: глубже — ошибка.
pub const MAX_DEPTH: usize = 256;
/// Разбирается начало файла не длиннее: на большее топлива хоста ([`postman_viewer_sdk::FUEL`]) может не хватить.
pub const MAX_XML: usize = 2 * 1024 * 1024;
/// Название и авторы на обложке — не длиннее (символов).
const MAX_LINE: usize = 1000;

/// Пределы разбора; в тестах — меньше.
#[derive(Debug, Clone, Copy)]
struct Limits {
    blocks: usize,
    chars: usize,
    sections: usize,
}

const LIMITS: Limits = Limits { blocks: MAX_BLOCKS, chars: MAX_CHARS, sections: MAX_SECTIONS };

/// Точка входа модуля (`pm_view`).
pub fn view(input: &[u8]) -> ViewerOutput {
    match book(input, LIMITS) {
        Ok((sections, truncated)) => ViewerOutput::Sections { sections, truncated },
        Err(e) => ViewerOutput::Error(e),
    }
}

fn book(input: &[u8], limits: Limits) -> Result<(Vec<Section>, bool), String> {
    let (xml, cp1251) = encoding(input)?;
    let cut = xml.len() > MAX_XML;
    let xml = &xml[..xml.len().min(MAX_XML)];
    let mut reader = Reader { s: xml, pos: 0, pending_end: false, cp1251: cp1251.then(|| CP1251.chars().collect()) };
    let mut b = Builder::new(limits);
    while let Some(event) = reader.next() {
        let go = match event {
            Event::Start(name, attrs) => b.start(name, attrs)?,
            Event::End => b.end(),
            Event::Text(t) => b.text(&t),
        };
        if !go {
            break;
        }
    }
    if !b.fiction {
        return Err("not a FictionBook file".into());
    }
    // Окно разбора кончилось посреди книги.
    b.truncated |= cut && reader.pos >= xml.len();
    Ok(b.finish())
}

// --- кодировка ---

/// Windows-1251, байты 0x80–0xFF.
const CP1251: &str = "ЂЃ‚ѓ„…†‡€‰Љ‹ЊЌЋЏђ‘’“”•–—\u{FFFD}™љ›њќћџ\u{A0}ЎўЈ¤Ґ¦§Ё©Є«¬\u{AD}®Ї°±Ііґµ¶·ё№є»јЅѕїАБВГДЕЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯабвгдежзийклмнопрстуфхцчшщъыьэюя";

/// XML без BOM и признак Windows-1251 (иначе UTF-8). Весь файл заранее не перекодируется: разметка — ASCII, текст
/// перекодирует [`Reader`] по кускам — до конца разбора (`binary`, предел) дело не доходит.
fn encoding(input: &[u8]) -> Result<(&[u8], bool), String> {
    if let Some(rest) = input.strip_prefix(b"\xEF\xBB\xBF") {
        return Ok((rest, false));
    }
    if input.starts_with(b"\xFF\xFE") || input.starts_with(b"\xFE\xFF") {
        return Err("UTF-16 is not supported".into());
    }
    match declared(input).as_deref() {
        None | Some("utf-8" | "utf8") => Ok((input, false)),
        Some("windows-1251" | "cp1251" | "cp-1251") => Ok((input, true)),
        Some(other) => Err(format!("encoding {other} is not supported")),
    }
}

/// Кодировка из объявления `<?xml … encoding="…"?>`, строчными.
fn declared(input: &[u8]) -> Option<String> {
    let head = input.strip_prefix(b"<?xml")?;
    let head = &head[..head.len().min(1024)];
    let decl = &head[..head.windows(2).position(|w| w == b"?>")?];
    let at = decl.windows(8).position(|w| w == b"encoding")?;
    let rest = decl[at + 8..].trim_ascii_start().strip_prefix(b"=")?.trim_ascii_start();
    let (&quote, rest) = rest.split_first()?;
    let end = rest.iter().position(|&c| c == quote)?;
    Some(String::from_utf8_lossy(&rest[..end]).trim().to_ascii_lowercase())
}

// --- XML ---

enum Event<'a> {
    /// Локальное имя (без приставки пространства имён) и атрибуты как есть.
    Start(&'a str, &'a [u8]),
    End,
    Text(Cow<'a, str>),
}

/// Потоковый разбор XML по байтам: комментарии, инструкции и объявление типа пропускаются; файл оборвался — конец.
struct Reader<'a> {
    s: &'a [u8],
    pos: usize,
    /// У пустого элемента `<x/>` конец — следующим событием.
    pending_end: bool,
    /// Таблица Windows-1251 для байтов 0x80–0xFF; `None` — UTF-8.
    cp1251: Option<Vec<char>>,
}

impl<'a> Reader<'a> {
    #[inline(never)]
    fn next(&mut self) -> Option<Event<'a>> {
        if std::mem::take(&mut self.pending_end) {
            return Some(Event::End);
        }
        loop {
            let rest = self.s.get(self.pos..).filter(|r| !r.is_empty())?;
            if rest[0] != b'<' {
                let end = find(rest, b"<").unwrap_or(rest.len());
                self.pos += end;
                return Some(Event::Text(self.text(&rest[..end], true)));
            }
            if let Some(body) = rest.strip_prefix(b"<!--") {
                self.pos += 4 + find(body, b"-->")? + 3;
            } else if let Some(body) = rest.strip_prefix(b"<![CDATA[") {
                let end = find(body, b"]]>")?;
                self.pos += 9 + end + 3;
                return Some(Event::Text(self.text(&body[..end], false)));
            } else if let Some(body) = rest.strip_prefix(b"<?") {
                self.pos += 2 + find(body, b"?>")? + 2;
            } else if rest.starts_with(b"<!") {
                // Объявление типа с внутренним подмножеством `[…]`: сущности не раскрываются, всё — мимо.
                let mut depth = 0usize;
                let end = rest.iter().position(|&c| {
                    match c {
                        b'[' => depth += 1,
                        b']' => depth = depth.saturating_sub(1),
                        b'>' if depth == 0 => return true,
                        _ => {}
                    }
                    false
                })?;
                self.pos += end + 1;
            } else if rest.starts_with(b"</") {
                self.pos += find(rest, b">")? + 1;
                return Some(Event::End);
            } else {
                // Конец тега — первая `>` вне кавычек значения атрибута.
                let mut quote = 0u8;
                let end = rest.iter().position(|&c| {
                    match (quote, c) {
                        (0, b'"' | b'\'') => quote = c,
                        (q, _) if q == c => quote = 0,
                        (0, b'>') => return true,
                        _ => {}
                    }
                    false
                })?;
                self.pos += end + 1;
                let tag = &rest[1..end];
                let (tag, empty) = match tag.strip_suffix(b"/") {
                    Some(t) => (t, true),
                    None => (tag, false),
                };
                let split = tag.iter().position(u8::is_ascii_whitespace).unwrap_or(tag.len());
                let (name, attrs) = tag.split_at(split);
                self.pending_end = empty;
                return Some(Event::Start(std::str::from_utf8(local(name)).unwrap_or_default(), attrs));
            }
        }
    }

    /// Кусок текста в строку: Windows-1251 по таблице, UTF-8 — с заменой негодного; `escaped` — с сущностями.
    #[inline(never)]
    fn text(&self, bytes: &'a [u8], escaped: bool) -> Cow<'a, str> {
        let s = match &self.cp1251 {
            Some(high) if !bytes.is_ascii() => Cow::Owned(
                bytes
                    .iter()
                    .map(|&b| if b < 0x80 { char::from(b) } else { high.get(usize::from(b - 0x80)).copied().unwrap_or('\u{FFFD}') })
                    .collect(),
            ),
            // Сначала строгая проверка: замена негодного (`from_utf8_lossy`) в интерпретаторе на порядок дороже.
            _ => std::str::from_utf8(bytes).map_or_else(|_| String::from_utf8_lossy(bytes), Cow::Borrowed),
        };
        match s {
            Cow::Borrowed(b) if escaped => unescape(b),
            Cow::Owned(o) if escaped && o.contains('&') => Cow::Owned(unescape(&o).into_owned()),
            s => s,
        }
    }
}

/// Первое вхождение `needle`.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    let (&first, tail) = needle.split_first()?;
    let mut from = 0;
    while let Some(at) = hay.get(from..)?.iter().position(|&b| b == first) {
        let at = from + at;
        if hay.get(at + 1..at + needle.len()) == Some(tail) {
            return Some(at);
        }
        from = at + 1;
    }
    None
}

fn local(name: &[u8]) -> &[u8] {
    name.iter().rposition(|&b| b == b':').map_or(name, |at| &name[at + 1..])
}

/// Значение атрибута с локальным именем `want` (`l:href`, `xlink:href` → `href`).
fn attr(attrs: &[u8], want: &str) -> Option<String> {
    let mut rest = attrs;
    loop {
        let eq = rest.iter().position(|&b| b == b'=')?;
        let name = local(rest[..eq].trim_ascii());
        let value = rest[eq + 1..].trim_ascii_start();
        let quote = *value.first().filter(|c| matches!(c, b'"' | b'\''))?;
        let end = value[1..].iter().position(|&b| b == quote)?;
        if name == want.as_bytes() {
            return Some(unescape(&String::from_utf8_lossy(&value[1..1 + end])).into_owned());
        }
        rest = &value[1 + end + 1..];
    }
}

/// Пять встроенных сущностей и символьные ссылки; прочее — как есть.
fn unescape(s: &str) -> Cow<'_, str> {
    if !s.contains('&') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let entity = rest[1..].find(';').filter(|&n| n <= 10).map(|n| &rest[1..1 + n]);
        let c = entity.and_then(|e| match e {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => {
                let n = match e.strip_prefix("#x").or_else(|| e.strip_prefix("#X")) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => e.strip_prefix('#').and_then(|d| d.parse().ok()),
                };
                n.and_then(char::from_u32)
            }
        });
        match (c, entity) {
            (Some(c), Some(e)) => {
                out.push(c);
                rest = &rest[e.len() + 2..];
            }
            _ => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

// --- книга ---

/// Куда идёт абзац, когда закроется.
#[derive(Clone, Copy, PartialEq)]
enum Target {
    Block,
    Title,
    Stanza,
    Cell,
}

/// Открытый абзац: строки текста, ссылка в нём, базовое начертание (подзаголовок, ячейка заголовка — жирные).
struct Para {
    target: Target,
    align: Align,
    base: Marks,
    content: Vec<Inline>,
    /// Внешняя ссылка: адрес, содержимое и уровень открывшего её элемента.
    link: Option<(String, Vec<Inline>, usize)>,
    /// Уровень открывшего абзац элемента.
    level: usize,
    /// Последним записан пробел (или ничего): пробелы схлопываются.
    space: bool,
}

/// Начертание по открытым элементам.
#[derive(Default)]
struct Styles {
    italic: usize,
    bold: usize,
    strike: usize,
    code: usize,
}

struct Builder<'a> {
    limits: Limits,
    /// Корень — `FictionBook`.
    fiction: bool,
    /// Открытые элементы.
    path: Vec<&'a str>,
    sections: Vec<Section>,
    /// Блоки раздела и открытые цитаты над ними: блоки и уровень открывшего элемента.
    stack: Vec<(Vec<Block>, usize)>,
    para: Option<Para>,
    styles: Styles,
    /// Строки заголовка и уровень `title`.
    title: Option<(Vec<Vec<Inline>>, usize)>,
    stanza: Option<Vec<Vec<Inline>>>,
    /// Таблица и уровень `table`.
    table: Option<(Vec<Vec<Vec<Inline>>>, usize)>,
    /// Вложенность `section` в теле.
    depth: usize,
    poems: usize,
    bodies: usize,
    in_body: bool,
    /// Название и авторы из описания.
    book_title: String,
    authors: Vec<String>,
    author: Vec<String>,
    /// Блоков обложки из описания в начале раздела: заголовок тела их заменяет.
    cover: usize,
    blocks: usize,
    chars: usize,
    cells: usize,
    truncated: bool,
}

impl<'a> Builder<'a> {
    fn new(limits: Limits) -> Self {
        Self {
            limits,
            fiction: false,
            path: Vec::new(),
            sections: Vec::new(),
            stack: vec![(Vec::new(), 0)],
            para: None,
            styles: Styles::default(),
            title: None,
            stanza: None,
            table: None,
            depth: 0,
            poems: 0,
            bodies: 0,
            in_body: false,
            book_title: String::new(),
            authors: Vec::new(),
            author: Vec::new(),
            cover: 0,
            blocks: 0,
            chars: 0,
            cells: 0,
            truncated: false,
        }
    }

    fn inside(&self, name: &str) -> bool {
        self.path.contains(&name)
    }

    /// В описании книги — только `title-info` (не `src-title-info`).
    fn in_title_info(&self) -> bool {
        self.inside("title-info") && !self.inside("src-title-info")
    }

    /// Текст книги: тело или аннотация в описании.
    fn content(&self) -> bool {
        self.in_body || (self.in_title_info() && self.inside("annotation"))
    }

    /// Элемент открыт; `false` — разбор окончен (`binary` или предел).
    #[inline(never)]
    fn start(&mut self, name: &'a str, attrs: &[u8]) -> Result<bool, String> {
        if self.path.is_empty() && !self.fiction {
            if name != "FictionBook" {
                return Err("not a FictionBook file".into());
            }
            self.fiction = true;
        }
        self.path.push(name);
        if self.path.len() > MAX_DEPTH {
            return Err("XML is nested too deep".into());
        }
        let level = self.path.len();
        match name {
            "binary" => return Ok(false),
            "body" if self.path.len() == 2 => {
                if self.bodies > 0 {
                    self.flush();
                }
                self.bodies += 1;
                self.in_body = true;
                self.depth = 0;
            }
            "author" if self.in_title_info() && !self.content() => self.author.clear(),
            _ if !self.content() => {}
            "section" => {
                if self.depth == 0 && self.bodies == 1 {
                    self.flush();
                }
                self.depth += 1;
            }
            "title" if self.para.is_none() && self.title.is_none() => self.title = Some((Vec::new(), level)),
            "poem" | "cite" | "epigraph" | "annotation" if self.para.is_none() => {
                if name == "poem" {
                    self.poems += 1;
                }
                if self.stack.len() <= MAX_QUOTES {
                    self.stack.push((Vec::new(), level));
                }
            }
            "stanza" if self.para.is_none() => self.stanza = Some(Vec::new()),
            "table" if self.para.is_none() && self.table.is_none() => self.table = Some((Vec::new(), level)),
            "tr" if self.para.is_none() => {
                if let Some((rows, _)) = &mut self.table {
                    rows.push(Vec::new());
                }
            }
            "p" | "v" | "subtitle" | "text-author" | "td" | "th" if self.para.is_none() => {
                let target = if self.title.is_some() {
                    Target::Title
                } else if name == "v" && self.stanza.is_some() {
                    Target::Stanza
                } else if matches!(name, "td" | "th") && self.table.as_ref().is_some_and(|(rows, _)| !rows.is_empty()) {
                    Target::Cell
                } else {
                    Target::Block
                };
                let align = match name {
                    "subtitle" => Align::Center,
                    "text-author" => Align::End,
                    _ => Align::Start,
                };
                let base = Marks { bold: matches!(name, "subtitle" | "th"), ..Marks::default() };
                self.para = Some(Para { target, align, base, content: Vec::new(), link: None, level, space: true });
            }
            "a" => {
                if let Some(p) = &mut self.para
                    && p.link.is_none()
                    && let Some(href) = attr(attrs, "href").filter(|h| !h.starts_with('#'))
                {
                    p.link = Some((href, Vec::new(), level));
                }
            }
            "emphasis" => self.styles.italic += 1,
            "strong" => self.styles.bold += 1,
            "strikethrough" => self.styles.strike += 1,
            "code" => self.styles.code += 1,
            "image" if self.para.is_none() && self.title.is_none() && self.table.is_none() => self.push(Block::Figure),
            _ => {}
        }
        Ok(!self.truncated)
    }

    /// Элемент закрыт.
    #[inline(never)]
    fn end(&mut self) -> bool {
        let level = self.path.len();
        let Some(name) = self.path.pop() else { return true };
        if self.para.as_ref().is_some_and(|p| p.link.as_ref().is_some_and(|l| l.2 == level))
            && let Some(p) = &mut self.para
            && let Some((href, content, _)) = p.link.take()
        {
            p.content.push(Inline::link(href, content));
        }
        if self.para.as_ref().is_some_and(|p| p.level == level) {
            self.close_para();
        }
        match name {
            "body" if level == 2 => {
                self.flush();
                self.in_body = false;
            }
            "author" if self.in_title_info() && !self.content() => {
                let name = self.author.join(" ");
                if !name.is_empty() {
                    self.authors.push(name);
                }
            }
            "title-info" => self.cover(),
            "section" if self.in_body => {
                self.depth = self.depth.saturating_sub(1);
                if self.depth == 0 && self.bodies == 1 {
                    self.flush();
                }
            }
            "title" if self.title.as_ref().is_some_and(|t| t.1 == level) => self.close_title(),
            "poem" => self.poems = self.poems.saturating_sub(1),
            "stanza" => {
                if let Some(lines) = self.stanza.take() {
                    let content = join(lines, Inline::LineBreak);
                    if !content.is_empty() {
                        self.push(Block::Paragraph { align: Align::Start, content });
                    }
                }
            }
            "table" if self.table.as_ref().is_some_and(|t| t.1 == level) => self.close_table(),
            "emphasis" => self.styles.italic = self.styles.italic.saturating_sub(1),
            "strong" => self.styles.bold = self.styles.bold.saturating_sub(1),
            "strikethrough" => self.styles.strike = self.styles.strike.saturating_sub(1),
            "code" => self.styles.code = self.styles.code.saturating_sub(1),
            _ => {}
        }
        if self.stack.len() > 1 && self.stack.last().is_some_and(|s| s.1 == level) {
            self.close_quote();
        }
        !self.truncated
    }

    #[inline(never)]
    fn text(&mut self, t: &str) -> bool {
        if let Some(p) = &mut self.para {
            let marks = Marks {
                bold: p.base.bold || self.styles.bold > 0,
                italic: self.styles.italic > 0,
                underline: false,
                strike: self.styles.strike > 0,
                code: self.styles.code > 0,
            };
            // Пробелы XML (ASCII) схлопываются, управляющие символы — прочь; остальное — кусками, не по символу.
            let mut s = String::with_capacity(t.len());
            let bytes = t.as_bytes();
            let mut run = 0;
            while run < bytes.len() {
                let i = bytes[run..].iter().position(|&b| b <= b' ' || b == 0x7F).map_or(bytes.len(), |n| run + n);
                if run < i {
                    s.push_str(&t[run..i]);
                    p.space = false;
                }
                if bytes.get(i).is_some_and(u8::is_ascii_whitespace) && !p.space {
                    s.push(' ');
                    p.space = true;
                }
                run = i + 1;
            }
            // Предел — посреди текста: ответ сверх общего предела хост отверг бы целиком.
            let left = self.limits.chars.saturating_sub(self.chars);
            let n = s.chars().count();
            if n > left {
                s = s.chars().take(left).collect();
                self.truncated = true;
            }
            if s.is_empty() {
                return !self.truncated;
            }
            self.chars += n.min(left);
            let into = match &mut p.link {
                Some((_, content, _)) => content,
                None => &mut p.content,
            };
            match into.last_mut() {
                Some(Inline::Text { text, marks: m }) if *m == marks => text.push_str(&s),
                _ => into.push(Inline::Text { text: s, marks }),
            }
        } else if self.in_title_info() && !self.content() {
            let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
            match self.path.last().copied() {
                Some("book-title") => self.book_title.push_str(&t),
                Some("first-name" | "middle-name" | "last-name" | "nickname") if self.inside("author") && !t.is_empty() => self.author.push(t),
                _ => {}
            }
        }
        !self.truncated
    }

    /// Блок в открытую цитату или раздел.
    #[inline(never)]
    fn push(&mut self, block: Block) {
        self.blocks += 1;
        if self.blocks > self.limits.blocks {
            self.truncated = true;
            return;
        }
        if let Some((blocks, _)) = self.stack.last_mut() {
            blocks.push(block);
        }
    }

    #[inline(never)]
    fn close_para(&mut self) {
        let Some(mut p) = self.para.take() else { return };
        if let Some((href, content, _)) = p.link.take() {
            p.content.push(Inline::link(href, content));
        }
        trim_end(&mut p.content);
        match p.target {
            Target::Title => {
                if let Some((lines, _)) = &mut self.title
                    && !p.content.is_empty()
                {
                    lines.push(p.content);
                }
            }
            Target::Stanza => {
                if let Some(lines) = &mut self.stanza
                    && !p.content.is_empty()
                {
                    lines.push(p.content);
                }
            }
            Target::Cell => {
                self.cells += 1;
                if self.cells > MAX_CELLS {
                    self.truncated = true;
                } else if let Some(row) = self.table.as_mut().and_then(|(rows, _)| rows.last_mut()) {
                    row.push(p.content);
                }
            }
            Target::Block if !p.content.is_empty() => self.push(Block::Paragraph { align: p.align, content: p.content }),
            Target::Block => {}
        }
    }

    #[inline(never)]
    fn close_title(&mut self) {
        let Some((lines, _)) = self.title.take() else { return };
        let content = join(lines, Inline::LineBreak);
        if content.is_empty() {
            return;
        }
        let notes = usize::from(self.bodies > 1);
        let level = (self.depth + notes + usize::from(self.poems > 0)).clamp(1, 6) as u8;
        let heading = Block::Heading { level, content };
        // Заголовок тела книги — вместо названия и авторов из описания, в начале обложки.
        if self.in_body && self.depth == 0 && self.sections.is_empty() && self.cover > 0 && self.stack.len() == 1 {
            if let Some((blocks, _)) = self.stack.first_mut() {
                blocks.splice(..self.cover.min(blocks.len()), [heading]);
            }
            self.blocks = self.blocks + 1 - self.cover;
            self.cover = 0;
            return;
        }
        self.push(heading);
    }

    #[inline(never)]
    fn close_table(&mut self) {
        let Some((rows, _)) = self.table.take() else { return };
        let rows: Vec<_> = rows.into_iter().filter(|r| !r.is_empty()).collect();
        if !rows.is_empty() {
            self.push(Block::Table(rows));
        }
    }

    #[inline(never)]
    fn close_quote(&mut self) {
        let Some((blocks, _)) = self.stack.pop() else { return };
        if !blocks.is_empty() {
            self.push(Block::Quote(blocks));
        }
    }

    /// Описание прочитано: название и авторы — в начало обложки (аннотация уже там).
    #[inline(never)]
    fn cover(&mut self) {
        let mut head = Vec::new();
        let title: String = std::mem::take(&mut self.book_title).chars().take(MAX_LINE).collect();
        let authors: String = self.authors.join(", ").chars().take(MAX_LINE).collect();
        self.chars += title.chars().count() + authors.chars().count();
        if !title.is_empty() {
            head.push(Block::Heading { level: 1, content: vec![Inline::text(title)] });
        }
        if !authors.is_empty() {
            head.push(Block::Paragraph { align: Align::Start, content: vec![Inline::text(authors)] });
        }
        self.blocks += head.len();
        self.cover = head.len();
        if let Some((blocks, _)) = self.stack.first_mut() {
            blocks.splice(0..0, head);
        }
    }

    /// Раздел готов: блоки — в ответ.
    #[inline(never)]
    fn flush(&mut self) {
        let Some((blocks, _)) = self.stack.first_mut() else { return };
        if blocks.is_empty() {
            return;
        }
        if self.sections.len() >= self.limits.sections {
            self.truncated = true;
            return;
        }
        let blocks = std::mem::take(blocks);
        self.sections.push(Section { title: SectionTitle::None, blocks });
        self.cover = 0;
    }

    /// Разбор окончен (в том числе посреди книги): открытое — закрыть, раздел — в ответ.
    fn finish(mut self) -> (Vec<Section>, bool) {
        let truncated = self.truncated;
        // Закрытие после предела не добавляет блоков: `push` их отбрасывает.
        self.close_para();
        self.close_title();
        self.close_table();
        while self.stack.len() > 1 {
            self.close_quote();
        }
        self.truncated = false;
        self.flush();
        (self.sections, truncated || self.truncated)
    }
}

/// Строки через разделитель.
fn join(lines: Vec<Vec<Inline>>, sep: Inline) -> Vec<Inline> {
    let mut out = Vec::new();
    for line in lines {
        if !out.is_empty() {
            out.push(sep.clone());
        }
        out.extend(line);
    }
    out
}

/// Пробел в конце абзаца — прочь.
fn trim_end(content: &mut Vec<Inline>) {
    let last = match content.last_mut() {
        Some(Inline::Text { text, .. }) => text,
        Some(Inline::Link { content, .. }) => return trim_end(content),
        _ => return,
    };
    let len = last.trim_end().len();
    last.truncate(len);
    if last.is_empty() {
        content.pop();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    const SAMPLE: &[u8] = include_bytes!("../sample.fb2");

    fn text(s: &str) -> Inline {
        Inline::text(s)
    }

    fn marked(s: &str, marks: Marks) -> Inline {
        Inline::Text { text: s.into(), marks }
    }

    fn para(content: Vec<Inline>) -> Block {
        Block::Paragraph { align: Align::Start, content }
    }

    fn sections(input: &[u8]) -> Vec<Section> {
        match view(input) {
            ViewerOutput::Sections { sections, truncated: false } => sections,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn sample_book_is_split_into_chapters() {
        let s = sections(SAMPLE);
        assert_eq!(s.len(), 4, "обложка, две главы, примечания");
        assert!(s.iter().all(|s| s.title == SectionTitle::None));
        // Обложка: заголовок тела вместо названия из описания, аннотация — цитатой.
        assert_eq!(
            s[0].blocks,
            [
                Block::Heading { level: 1, content: vec![text("Иван Петров"), Inline::LineBreak, text("Тестовая книга")] },
                Block::Quote(vec![para(vec![text("Книга для проверки модуля.")])]),
            ]
        );
        let italic = Marks { italic: true, ..Marks::default() };
        let bold = Marks { bold: true, ..Marks::default() };
        assert_eq!(
            s[1].blocks,
            [
                Block::Heading { level: 1, content: vec![text("Глава 1. Начало")] },
                Block::Quote(vec![para(vec![text("Эпиграф.")]), Block::Paragraph { align: Align::End, content: vec![text("Автор эпиграфа")] }]),
                para(vec![text("Первый абзац с "), marked("курсивом", italic), text(" и "), marked("жирным", bold), text(".")]),
                para(vec![
                    text("Ссылка на "),
                    Inline::link("https://example.org/book", vec![text("сайт")]),
                    text(" и сноска [1] — «кавычки» & знак."),
                ]),
                Block::Heading { level: 2, content: vec![text("Часть 1.1")] },
                para(vec![text("Вложенный раздел.")]),
                Block::Figure,
            ]
        );
        assert_eq!(
            s[2].blocks,
            [
                Block::Heading { level: 1, content: vec![text("Глава 2")] },
                Block::Quote(vec![
                    Block::Heading { level: 2, content: vec![text("Стихи")] },
                    para(vec![text("Первая строка,"), Inline::LineBreak, text("вторая строка.")]),
                    para(vec![text("Вторая строфа.")]),
                ]),
                Block::Paragraph { align: Align::Center, content: vec![marked("* * *", bold)] },
                Block::Table(vec![vec![vec![marked("А", bold)], vec![marked("Б", bold)]], vec![vec![text("1")], vec![text("2")]]]),
            ]
        );
        assert_eq!(
            s[3].blocks,
            [
                Block::Heading { level: 1, content: vec![text("Примечания")] },
                Block::Heading { level: 2, content: vec![text("1")] },
                para(vec![text("Текст сноски.")]),
            ]
        );
    }

    #[test]
    fn windows_1251_and_cover_from_description() {
        let mut book = b"<?xml version='1.0' encoding='windows-1251'?><FictionBook><description><title-info>".to_vec();
        book.extend(b"<author><first-name>\xc8\xe2\xe0\xed</first-name><last-name>\xcf\xe5\xf2\xf0\xee\xe2</last-name></author>");
        book.extend(b"<book-title>\xca\xed\xe8\xe3\xe0</book-title></title-info></description>");
        book.extend(b"<body><section><p>\xd2\xe5\xea\xf1\xf2 &#x2014; &#1105;</p></section></body></FictionBook>");
        let s = sections(&book);
        assert_eq!(s[0].blocks, [Block::Heading { level: 1, content: vec![text("Книга")] }, para(vec![text("Иван Петров")])]);
        assert_eq!(s[1].blocks, [para(vec![text("Текст — ё")])]);
    }

    #[test]
    fn book_without_sections_is_one_chapter() {
        let s = sections("<FictionBook><body><p>Один</p><p>  Два \n  три  </p></body></FictionBook>".as_bytes());
        assert_eq!(s, [Section { title: SectionTitle::None, blocks: vec![para(vec![text("Один")]), para(vec![text("Два три")])] }]);
    }

    #[test]
    fn limits_truncate() {
        let mut book = String::from("<FictionBook><body>");
        for i in 0..50 {
            book.push_str(&format!("<section><title><p>Глава {i}</p></title><p>Абзац {i}</p></section>"));
        }
        book.push_str("</body></FictionBook>");
        let limits = |blocks, chars, sections| Limits { blocks, chars, sections };
        let (s, truncated) = book_of(&book, limits(10_000, 1_000_000, 10));
        assert!(truncated);
        assert_eq!(s.len(), 10);
        let (s, truncated) = book_of(&book, limits(5, 1_000_000, 100));
        assert!(truncated);
        assert_eq!(s.iter().map(|s| s.blocks.len()).sum::<usize>(), 5);
        // Предел символов — посреди текста.
        let (s, truncated) = book_of(&book, limits(10_000, 30, 100));
        assert!(truncated);
        assert_eq!(s.len(), 3);
        assert_eq!(s[2].blocks, [Block::Heading { level: 1, content: vec![text("Гл")] }]);
        let (s, truncated) = book_of(&book, LIMITS);
        assert!(!truncated);
        assert_eq!(s.len(), 50);
    }

    fn book_of(book: &str, limits: Limits) -> (Vec<Section>, bool) {
        super::book(book.as_bytes(), limits).unwrap()
    }

    #[test]
    fn hostile_input_is_an_error_not_a_panic() {
        assert_eq!(view(b"PK\x03\x04"), ViewerOutput::Error("not a FictionBook file".into()));
        assert_eq!(view(b"<html><body><p>x</p></body></html>"), ViewerOutput::Error("not a FictionBook file".into()));
        assert_eq!(view(b"\xFF\xFE<\0F\0"), ViewerOutput::Error("UTF-16 is not supported".into()));
        assert_eq!(view(b"<?xml encoding=\"koi8-r\"?><FictionBook/>"), ViewerOutput::Error("encoding koi8-r is not supported".into()));
        let deep = format!("<FictionBook>{}", "<cite>".repeat(MAX_DEPTH + 1));
        assert_eq!(view(deep.as_bytes()), ViewerOutput::Error("XML is nested too deep".into()));
        // Цитаты глубже предела — в объемлющей; оборванный файл — что успели.
        let quotes = format!("<FictionBook><body>{}<p>x", "<cite>".repeat(40));
        let s = sections(quotes.as_bytes());
        let mut depth = 0;
        let mut b = &s[0].blocks[0];
        while let Block::Quote(inner) = b {
            depth += 1;
            b = &inner[0];
        }
        assert_eq!((depth, b), (MAX_QUOTES, &para(vec![text("x")])));
        // Сущности DTD не раскрываются, объявление типа — мимо.
        let dtd = "<?xml version=\"1.0\"?><!DOCTYPE b [<!ENTITY e \"boom\">]><FictionBook><body><p>&e; &amp;</p></body></FictionBook>";
        assert_eq!(sections(dtd.as_bytes())[0].blocks, [para(vec![text("&e; &")])]);
        for cut in 0..SAMPLE.len() {
            let _ = view(&SAMPLE[..cut]);
        }
    }
}
