//! Просмотр книги (S8-17): `.xlsx`/`.xltx` и `.ods` → раздел на лист с таблицей значений. Только здесь, в песочнице.
//!
//! Защиты — как у [`docx`]: часть книги читается потоково не больше [`docx::MAX_XML`] байт после распаковки, все части
//! вместе — не больше [`MAX_UNPACKED`] (ложные размеры каталога и zip-бомба упираются в предел, итог — `truncated`),
//! служебные части — не больше [`docx::MAX_PART`]; XML без DTD и чужих сущностей, с пределом вложенности. Наружу — до
//! [`MAX_SHEETS`] листов, на лист — до [`MAX_ROWS`] строк × [`MAX_COLS`] столбцов, на книгу — до [`MAX_CELLS`] ячеек
//! сетки и [`docx::MAX_CHARS`] символов; остальное — `truncated`. Время ограничивает сторож в `serve`.
//!
//! `.xlsx`: листы по порядку `xl/workbook.xml` (скрытые не показываются), общие строки, `inlineStr`, числа, булевы,
//! ошибки, даты (встроенные `numFmtId` и свои форматы с `d`/`m`/`y`, системы 1900 и 1904); у формулы — её итог из
//! `<v>`, без вычисления. `.ods`: `table:table` из `content.xml`; ячейка — показанный текст (`text:p`), без него —
//! значение; повторы строк и столбцов (`number-*-repeated`) разворачиваются только в пределах листа.

use std::cell::Cell;
use std::collections::HashMap;
use std::io::{self, BufReader};
use std::ops::ControlFlow;
use std::rc::Rc;

use postman_ir::{Block, Inline};

use super::docx::{self, Ev, Limit, attr, part, rels, resolve, walk};
use super::{MAX_UNPACKED, Section, SectionTitle};

/// Листов в ответе.
pub const MAX_SHEETS: usize = 50;
/// Строк и столбцов листа.
pub const MAX_ROWS: u32 = 1_000;
pub const MAX_COLS: u32 = 100;
/// Ячеек сетки во всей книге (с пустыми): ответ — не больше десятков МиБ.
pub const MAX_CELLS: usize = 200_000;

/// Пределы разбора; в тестах — меньше.
#[derive(Debug, Clone, Copy)]
pub(super) struct Limits {
    /// Одна часть и все части после распаковки.
    pub xml: u64,
    pub unpacked: u64,
    pub sheets: usize,
    pub rows: u32,
    pub cols: u32,
    pub cells: usize,
    pub chars: usize,
}

pub(super) const LIMITS: Limits = Limits {
    xml: docx::MAX_XML,
    unpacked: MAX_UNPACKED,
    sheets: MAX_SHEETS,
    rows: MAX_ROWS,
    cols: MAX_COLS,
    cells: MAX_CELLS,
    chars: docx::MAX_CHARS,
};

type Zip<'a> = zip::ZipArchive<io::Cursor<&'a [u8]>>;

/// Ячейки листа: строка, столбец (с нуля) и текст.
pub(super) type Cells = Vec<(u32, u32, String)>;

/// Книга `.xlsx`/`.ods` → раздел на лист и признак «показано начало».
pub fn sheet(bytes: &[u8]) -> Result<(Vec<Section>, bool), String> {
    parse(bytes, LIMITS)
}

fn parse(bytes: &[u8], limits: Limits) -> Result<(Vec<Section>, bool), String> {
    if !bytes.starts_with(b"PK\x03\x04") {
        return Err("не книга .xlsx или .ods (старый .xls или защищена паролем)".into());
    }
    let mut zip = zip::ZipArchive::new(io::Cursor::new(bytes)).map_err(|e| format!("книга не разобрана: {e}"))?;
    let mut budget = Budget::new(limits);
    let ods = zip.by_name("content.xml").is_ok() && zip.by_name("[Content_Types].xml").is_err();
    let sections = if ods { self::ods(&mut zip, &mut budget)? } else { xlsx(&mut zip, &mut budget)? };
    Ok((sections, budget.truncated))
}

// --- пределы и сетка (и для CSV) ---

/// Расход пределов по всей книге.
pub(super) struct Budget {
    pub limits: Limits,
    unpacked: u64,
    cells: usize,
    chars: usize,
    pub truncated: bool,
}

impl Budget {
    pub fn new(limits: Limits) -> Self {
        Budget { limits, unpacked: 0, cells: 0, chars: 0, truncated: false }
    }

    /// Текст ячейки в пределах символов книги: управляющие (кроме перевода строки и табуляции) и bidi-символы
    /// убраны; `None` — символы кончились.
    pub fn text(&mut self, s: &str) -> Option<String> {
        let left = self.limits.chars.saturating_sub(self.chars);
        if left == 0 {
            self.truncated = true;
            return None;
        }
        let mut clean = String::new();
        let mut n = 0usize;
        for c in s
            .chars()
            .filter(|&c| c == '\n' || c == '\t' || !c.is_control())
            .filter(|c| !matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
        {
            if n == left {
                self.truncated = true;
                break;
            }
            clean.push(c);
            n += 1;
        }
        // Ячейка — не меньше символа из предела (их тысячи в маленьком XML).
        self.chars += n.max(1);
        Some(clean)
    }

    /// Ячейки листа → таблица IR: сетка до последней занятой строки и столбца, в пределах ячеек книги.
    pub fn table(&mut self, cells: Cells) -> Vec<Block> {
        let (Some(rows), Some(cols)) = (cells.iter().map(|c| c.0).max(), cells.iter().map(|c| c.1).max()) else {
            return Vec::new();
        };
        let (rows, cols) = (rows as usize + 1, cols as usize + 1);
        let fit = (self.limits.cells.saturating_sub(self.cells) / cols).min(rows);
        if fit < rows {
            self.truncated = true;
        }
        if fit == 0 {
            return Vec::new();
        }
        self.cells += fit * cols;
        let mut grid: Vec<Vec<Vec<Inline>>> = vec![vec![Vec::new(); cols]; fit];
        for (r, c, text) in cells {
            if let Some(cell) = grid.get_mut(r as usize).and_then(|row| row.get_mut(c as usize)) {
                *cell = inlines(&text);
            }
        }
        vec![Block::Table(grid)]
    }

    /// Сколько ещё можно распаковать в следующую часть.
    pub fn part_left(&self) -> u64 {
        self.limits.xml.min(self.limits.unpacked.saturating_sub(self.unpacked))
    }

    /// Учесть прочитанную часть ([`read`]): распаковано байт и упёрлась ли она в предел.
    pub fn spent(&mut self, (used, hit): (u64, bool)) {
        self.unpacked += used;
        self.truncated |= hit;
    }
}

/// Часть пакета потоково, не больше `left` байт после распаковки: сколько распаковано и упёрлась ли часть в предел
/// (это не порча, а `truncated`).
pub(super) fn read<N: Copy>(
    zip: &mut Zip,
    name: &str,
    left: u64,
    ns: impl Fn(&[u8]) -> N,
    on: impl FnMut(Ev<'_, N>) -> ControlFlow<()>,
) -> Result<(u64, bool), String> {
    let f = zip.by_name(name).map_err(|_| format!("нет части {name}"))?;
    let hit = Rc::new(Cell::new(false));
    let mut src = BufReader::new(Limit { inner: f, left, hit: hit.clone() });
    let r = walk(&mut src, ns, on);
    let used = left - src.get_ref().left;
    match r {
        // Обрезали по пределу посреди элемента — это не порча.
        Err(_) if hit.get() => Ok((used, true)),
        Err(e) => Err(e),
        Ok(()) => Ok((used, hit.get())),
    }
}

/// Текст ячейки → инлайны: переводы строк — разрывами.
fn inlines(text: &str) -> Vec<Inline> {
    let mut out = Vec::new();
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push(Inline::LineBreak);
        }
        if !line.is_empty() {
            out.push(Inline::text(line));
        }
    }
    out
}

/// Имя листа: без управляющих символов, не длиннее 100.
pub(super) fn title(name: &str) -> String {
    name.chars().filter(|c| !c.is_control()).take(100).collect()
}

/// Число как в «Общем» формате Excel: целое — без дроби, иначе не больше 15 значащих цифр (0.1 + 0.2 → 0.3).
pub(super) fn general(x: f64) -> String {
    if x == x.trunc() && x.abs() < 1e15 {
        return format!("{}", x as i64);
    }
    let r: f64 = format!("{x:.14e}").parse().unwrap_or(x);
    if r.abs() >= 1e15 || r.abs() < 1e-9 { format!("{r:E}") } else { format!("{r}") }
}

// --- xlsx ---

const S: &[u8] = b"http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const S_STRICT: &[u8] = b"http://purl.oclc.org/ooxml/spreadsheetml/main";

/// Элемент SpreadsheetML (переходный и строгий).
fn sml(uri: &[u8]) -> bool {
    uri == S || uri == S_STRICT
}

/// Как показывать число ячейки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fmt {
    Number,
    Date,
    DateTime,
    Time,
}

fn xlsx(zip: &mut Zip, budget: &mut Budget) -> Result<Vec<Section>, String> {
    // Основная часть — по связи пакета, обычно `xl/workbook.xml`.
    let main = part(zip, "_rels/.rels")
        .and_then(|d| rels(&d).ok())
        .and_then(|r| r.into_iter().find(|r| r.kind.ends_with("/officeDocument")).map(|r| resolve("", &r.target)))
        .unwrap_or_else(|| "xl/workbook.xml".into());
    let (dir, file) = match main.rsplit_once('/') {
        Some((d, f)) => (format!("{d}/"), f.to_string()),
        None => (String::new(), main.clone()),
    };
    let links = part(zip, &format!("{dir}_rels/{file}.rels")).and_then(|d| rels(&d).ok()).unwrap_or_default();
    let target = |kind: &str, default: &str| {
        links.iter().find(|r| r.kind.ends_with(kind)).map_or_else(|| format!("{dir}{default}"), |r| resolve(&dir, &r.target))
    };
    let (strings_at, styles_at) = (target("/sharedStrings", "sharedStrings.xml"), target("/styles", "styles.xml"));
    let workbook = part(zip, &main).ok_or("в книге нет листов")?;

    // Листы по порядку книги: имя и путь части.
    let mut sheets: Vec<(String, String)> = Vec::new();
    let mut date1904 = false;
    walk(workbook.as_slice(), sml, |ev| {
        match ev {
            Ev::Start(true, b"sheet", a) if !matches!(attr(a, b"state"), Some("hidden" | "veryHidden")) => {
                let path = attr(a, b"r:id").and_then(|id| links.iter().find(|r| r.id == id && r.kind.ends_with("/worksheet")));
                if let (Some(name), Some(rel)) = (attr(a, b"name"), path) {
                    sheets.push((title(name), resolve(&dir, &rel.target)));
                }
            }
            Ev::Start(true, b"workbookPr", a) => date1904 = matches!(attr(a, b"date1904"), Some("1" | "true")),
            _ => {}
        }
        ControlFlow::Continue(())
    })
    .map_err(|e| format!("книга не разобрана: {e}"))?;
    if sheets.is_empty() {
        return Err("в книге нет листов".into());
    }
    let fmts = part(zip, &styles_at).and_then(|d| styles(&d).ok()).unwrap_or_default();
    let strings = if zip.by_name(&strings_at).is_ok() { shared_strings(zip, &strings_at, budget)? } else { Vec::new() };

    let mut out = Vec::new();
    for (n, (name, path)) in sheets.into_iter().enumerate() {
        if n >= budget.limits.sheets {
            budget.truncated = true;
            break;
        }
        let mut s = XlSheet { strings: &strings, fmts: &fmts, date1904, ..XlSheet::default() };
        let read = read(zip, &path, budget.part_left(), sml, |ev| s.event(ev, budget)).map_err(|e| format!("книга не разобрана: {e}"))?;
        budget.spent(read);
        out.push(Section { title: SectionTitle::Sheet(name), blocks: budget.table(s.cells) });
    }
    Ok(out)
}

/// Общие строки книги по порядку; строка из фрагментов (`r`) — целиком, без фонетики (`rPh`).
fn shared_strings(zip: &mut Zip, name: &str, budget: &mut Budget) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let (mut cur, mut in_t, mut rph) = (String::new(), false, 0usize);
    let read = read(zip, name, budget.part_left(), sml, |ev| {
        match ev {
            Ev::Start(true, b"si", _) => cur.clear(),
            Ev::Start(true, b"rPh", _) => rph += 1,
            Ev::End(true, b"rPh") => rph = rph.saturating_sub(1),
            Ev::Start(true, b"t", _) => in_t = true,
            Ev::End(true, b"t") => in_t = false,
            Ev::Text(t) if in_t && rph == 0 => cur.push_str(t),
            Ev::End(true, b"si") => out.push(std::mem::take(&mut cur)),
            _ => {}
        }
        ControlFlow::Continue(())
    })
    .map_err(|e| format!("книга не разобрана: {e}"))?;
    budget.spent(read);
    Ok(out)
}

/// Формат числа по индексу стиля ячейки (`cellXfs`).
fn styles(data: &[u8]) -> Result<Vec<Fmt>, String> {
    let mut custom: HashMap<u32, Fmt> = HashMap::new();
    let mut xfs: Vec<u32> = Vec::new();
    let mut in_xfs = false;
    walk(data, sml, |ev| {
        match ev {
            Ev::Start(true, b"numFmt", a) => {
                if let (Some(id), Some(code)) = (attr(a, b"numFmtId").and_then(|v| v.parse().ok()), attr(a, b"formatCode")) {
                    custom.insert(id, custom_format(code));
                }
            }
            Ev::Start(true, b"cellXfs", _) => in_xfs = true,
            Ev::End(true, b"cellXfs") => in_xfs = false,
            Ev::Start(true, b"xf", a) if in_xfs => xfs.push(attr(a, b"numFmtId").and_then(|v| v.parse().ok()).unwrap_or(0)),
            _ => {}
        }
        ControlFlow::Continue(())
    })?;
    Ok(xfs.into_iter().map(|id| custom.get(&id).copied().unwrap_or_else(|| builtin_format(id))).collect())
}

/// Встроенные форматы дат и времени (ECMA-376, 18.8.30; 27–36 и 50–58 — восточноазиатские даты).
fn builtin_format(id: u32) -> Fmt {
    match id {
        14..=17 | 27..=31 | 34..=36 | 50..=58 => Fmt::Date,
        22 => Fmt::DateTime,
        18..=21 | 32 | 33 | 45..=47 => Fmt::Time,
        _ => Fmt::Number,
    }
}

/// Свой формат: дата — если в первой секции есть `d`, `y` или `m` без `h`/`s` (иначе это минуты), время — `h`, `s`;
/// текст в кавычках, экранированные знаки, заполнители (`_x`, `*x`) и `[…]` (цвет, язык) не считаются.
fn custom_format(code: &str) -> Fmt {
    let mut plain = String::new();
    let (mut quoted, mut bracket) = (false, false);
    let mut chars = code.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            _ if quoted => {}
            '\\' | '_' | '*' => {
                chars.next();
            }
            '[' => bracket = true,
            ']' => bracket = false,
            _ if bracket => {}
            ';' => break,
            c => plain.push(c.to_ascii_lowercase()),
        }
    }
    let time = plain.contains(['h', 's']);
    let date = plain.contains(['d', 'y']) || (plain.contains('m') && !time);
    match (date, time) {
        (true, true) => Fmt::DateTime,
        (true, false) => Fmt::Date,
        (false, true) => Fmt::Time,
        (false, false) => Fmt::Number,
    }
}

/// Дни от 1970-01-01 → год, месяц, день (H. Hinnant, `civil_from_days`).
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Серийное число Excel → «2026-10-29», «2026-10-29 14:30» или «14:30»; вне 1900–9999 — `None` (число как есть).
/// В системе 1900 есть несуществующее 29 февраля 1900 (число 60), как в Excel.
fn excel_date(x: f64, date1904: bool, fmt: Fmt) -> Option<String> {
    if !(x.is_finite() && (0.0..2_958_466.0).contains(&x)) {
        return None;
    }
    let mut day = x.floor() as i64;
    let mut secs = ((x - x.floor()) * 86_400.0).round() as i64;
    if secs >= 86_400 {
        day += 1;
        secs -= 86_400;
    }
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    let time = if s == 0 { format!("{h:02}:{m:02}") } else { format!("{h:02}:{m:02}:{s:02}") };
    if fmt == Fmt::Time {
        return Some(time);
    }
    let date = if !date1904 && day == 60 {
        "1900-02-29".to_string()
    } else {
        // От 1970-01-01: 1904-01-01 — −24 107, 1899-12-30 — −25 569 (с числа 61, после ложного 29 февраля).
        let unix = if date1904 {
            day - 24_107
        } else if day < 60 {
            day - 25_568
        } else {
            day - 25_569
        };
        let (y, m, d) = civil(unix);
        format!("{y:04}-{m:02}-{d:02}")
    };
    Some(if fmt == Fmt::DateTime { format!("{date} {time}") } else { date })
}

/// «B12» → строка 11, столбец 1 (с нуля).
fn cell_ref(r: &str) -> Option<(u32, u32)> {
    let split = r.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = r.split_at(split);
    if letters.is_empty() || letters.len() > 3 || !letters.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    let col = letters.bytes().fold(0u32, |n, b| n * 26 + u32::from(b.to_ascii_uppercase() - b'A') + 1) - 1;
    let row = digits.parse::<u32>().ok()?.checked_sub(1)?;
    Some((row, col))
}

/// Ячейка листа в сборке.
#[derive(Default)]
struct XlCell {
    row: u32,
    col: u32,
    kind: String,
    style: usize,
    value: String,
}

/// Лист `.xlsx` в сборке.
#[derive(Default)]
struct XlSheet<'a> {
    strings: &'a [String],
    fmts: &'a [Fmt],
    date1904: bool,
    cells: Cells,
    next_row: u32,
    col: u32,
    cell: Option<XlCell>,
    in_v: bool,
    in_is: bool,
    in_t: bool,
    rph: usize,
}

impl XlSheet<'_> {
    fn event(&mut self, ev: Ev<'_, bool>, budget: &mut Budget) -> ControlFlow<()> {
        match ev {
            Ev::Start(true, b"row", a) => {
                let row = attr(a, b"r").and_then(|r| r.parse::<u32>().ok()).and_then(|r| r.checked_sub(1)).unwrap_or(self.next_row);
                self.next_row = row.saturating_add(1);
                self.col = 0;
            }
            Ev::Start(true, b"c", a) => {
                let row = self.next_row.saturating_sub(1);
                let (row, col) = attr(a, b"r").and_then(cell_ref).unwrap_or((row, self.col));
                self.col = col.saturating_add(1);
                let kind = attr(a, b"t").unwrap_or_default().to_string();
                let style = attr(a, b"s").and_then(|s| s.parse().ok()).unwrap_or(0);
                self.cell = Some(XlCell { row, col, kind, style, value: String::new() });
            }
            Ev::Start(true, b"v", _) => self.in_v = true,
            Ev::End(true, b"v") => self.in_v = false,
            Ev::Start(true, b"is", _) => self.in_is = true,
            Ev::End(true, b"is") => self.in_is = false,
            Ev::Start(true, b"t", _) => self.in_t = self.in_is,
            Ev::End(true, b"t") => self.in_t = false,
            Ev::Start(true, b"rPh", _) => self.rph += 1,
            Ev::End(true, b"rPh") => self.rph = self.rph.saturating_sub(1),
            Ev::Text(t) if self.in_v || (self.in_t && self.rph == 0) => {
                if let Some(c) = &mut self.cell {
                    c.value.push_str(t);
                }
            }
            Ev::End(true, b"c") => {
                let Some(c) = self.cell.take() else { return ControlFlow::Continue(()) };
                let text = self.display(&c);
                if text.is_empty() {
                    return ControlFlow::Continue(());
                }
                let limits = budget.limits;
                if c.row >= limits.rows || c.col >= limits.cols {
                    budget.truncated = true;
                    // Строки идут по порядку: дальше листа в пределах уже не будет.
                    return if c.row >= limits.rows { ControlFlow::Break(()) } else { ControlFlow::Continue(()) };
                }
                if self.cells.len() >= limits.rows as usize * limits.cols as usize {
                    budget.truncated = true;
                    return ControlFlow::Break(());
                }
                let Some(text) = budget.text(&text) else { return ControlFlow::Break(()) };
                self.cells.push((c.row, c.col, text));
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }

    /// Показанное значение: строка, булево, ошибка — как есть; число — датой по формату стиля или «Общим».
    fn display(&self, c: &XlCell) -> String {
        match c.kind.as_str() {
            "s" => c.value.trim().parse::<usize>().ok().and_then(|i| self.strings.get(i)).cloned().unwrap_or_default(),
            "inlineStr" | "str" | "e" | "d" => c.value.clone(),
            "b" => match c.value.trim() {
                "1" | "true" => "TRUE".into(),
                "0" | "false" => "FALSE".into(),
                _ => c.value.clone(),
            },
            _ => {
                let Ok(x) = c.value.trim().parse::<f64>() else { return c.value.clone() };
                match self.fmts.get(c.style).copied().unwrap_or(Fmt::Number) {
                    Fmt::Number => general(x),
                    fmt => excel_date(x, self.date1904, fmt).unwrap_or_else(|| general(x)),
                }
            }
        }
    }
}

// --- ods ---

/// Пространства имён OpenDocument, которые различает разбор (и у презентаций `.odp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Od {
    Office,
    Table,
    Text,
    Draw,
    Presentation,
    Style,
    Other,
}

pub(super) fn od(uri: &[u8]) -> Od {
    match uri {
        b"urn:oasis:names:tc:opendocument:xmlns:office:1.0" => Od::Office,
        b"urn:oasis:names:tc:opendocument:xmlns:table:1.0" => Od::Table,
        b"urn:oasis:names:tc:opendocument:xmlns:text:1.0" => Od::Text,
        b"urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" => Od::Draw,
        b"urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" => Od::Presentation,
        b"urn:oasis:names:tc:opendocument:xmlns:style:1.0" => Od::Style,
        _ => Od::Other,
    }
}

/// Повтор строки или столбца (`number-*-repeated`): не меньше одного.
pub(super) fn repeated(a: &[(Vec<u8>, String)], key: &[u8]) -> u32 {
    attr(a, key).and_then(|v| v.parse().ok()).unwrap_or(1).max(1)
}

fn ods(zip: &mut Zip, budget: &mut Budget) -> Result<Vec<Section>, String> {
    let mut s = OdsBook::default();
    let read = read(zip, "content.xml", budget.part_left(), od, |ev| s.event(ev, budget)).map_err(|e| format!("книга не разобрана: {e}"))?;
    budget.spent(read);
    let mut out = Vec::new();
    for (name, cells) in s.sheets {
        out.push(Section { title: SectionTitle::Sheet(name), blocks: budget.table(cells) });
    }
    if out.is_empty() {
        return Err("в книге нет листов".into());
    }
    Ok(out)
}

/// Ячейка `.ods` в сборке: повтор, показанный текст, значение (если текста нет) и открытые абзацы.
struct OdsCell {
    repeat: u32,
    text: String,
    value: String,
    paras: usize,
    open: usize,
}

/// Книга `.ods` в сборке.
#[derive(Default)]
struct OdsBook {
    sheets: Vec<(String, Cells)>,
    /// Лист открыт (вложенные таблицы — пропускаются).
    open: bool,
    row: u32,
    row_repeat: u32,
    col: u32,
    row_cells: Vec<(u32, String)>,
    cell: Option<OdsCell>,
    skip: usize,
}

impl OdsBook {
    fn event(&mut self, ev: Ev<'_, Od>, budget: &mut Budget) -> ControlFlow<()> {
        let limits = budget.limits;
        match ev {
            Ev::Start(..) if self.skip > 0 => self.skip += 1,
            Ev::End(..) if self.skip > 0 => self.skip -= 1,
            // Примечания и сноски — не текст ячейки.
            Ev::Start(Od::Office, b"annotation", _) | Ev::Start(Od::Text, b"note", _) => self.skip = 1,
            Ev::Start(Od::Table, b"table", a) => {
                if self.open {
                    self.skip = 1;
                } else if self.sheets.len() >= limits.sheets {
                    budget.truncated = true;
                    return ControlFlow::Break(());
                } else {
                    self.open = true;
                    self.row = 0;
                    self.sheets.push((title(attr(a, b"name").unwrap_or_default()), Vec::new()));
                }
            }
            Ev::End(Od::Table, b"table") => self.open = false,
            Ev::Start(Od::Table, b"table-row", a) if self.open => {
                self.row_repeat = repeated(a, b"number-rows-repeated");
                self.row_cells.clear();
                self.col = 0;
            }
            Ev::Start(Od::Table, b"table-cell" | b"covered-table-cell", a) if self.open => {
                let value = match attr(a, b"value-type") {
                    Some("float" | "percentage" | "currency") => attr(a, b"value").and_then(|v| v.parse().ok()).map(general),
                    Some("boolean") => attr(a, b"boolean-value").map(|v| if v == "true" { "TRUE".into() } else { "FALSE".into() }),
                    Some("date") => attr(a, b"date-value").map(str::to_string),
                    Some("time") => attr(a, b"time-value").map(str::to_string),
                    _ => None,
                };
                let repeat = repeated(a, b"number-columns-repeated");
                self.cell = Some(OdsCell { repeat, text: String::new(), value: value.unwrap_or_default(), paras: 0, open: 0 });
            }
            Ev::Start(Od::Text, name, a) => {
                if let Some(c) = &mut self.cell {
                    match name {
                        b"p" | b"h" => {
                            if c.paras > 0 {
                                c.text.push('\n');
                            }
                            c.paras += 1;
                            c.open += 1;
                        }
                        b"s" => {
                            let n = attr(a, b"c").and_then(|v| v.parse().ok()).unwrap_or(1usize).min(1000);
                            c.text.extend(std::iter::repeat_n(' ', n));
                        }
                        b"tab" => c.text.push('\t'),
                        b"line-break" => c.text.push('\n'),
                        _ => {}
                    }
                }
            }
            Ev::End(Od::Text, b"p" | b"h") => {
                if let Some(c) = &mut self.cell {
                    c.open = c.open.saturating_sub(1);
                }
            }
            Ev::Text(t) => {
                if let Some(c) = self.cell.as_mut().filter(|c| c.open > 0) {
                    c.text.push_str(t);
                }
            }
            Ev::End(Od::Table, b"table-cell" | b"covered-table-cell") => {
                let Some(c) = self.cell.take() else { return ControlFlow::Continue(()) };
                let text = if c.text.is_empty() { c.value } else { c.text };
                if !text.is_empty() {
                    for k in 0..c.repeat {
                        let col = self.col.saturating_add(k);
                        if col >= limits.cols {
                            budget.truncated = true;
                            break;
                        }
                        self.row_cells.push((col, text.clone()));
                    }
                }
                self.col = self.col.saturating_add(c.repeat);
            }
            Ev::End(Od::Table, b"table-row") if self.open => {
                if !self.row_cells.is_empty() {
                    let Some((_, cells)) = self.sheets.last_mut() else { return ControlFlow::Continue(()) };
                    for k in 0..self.row_repeat {
                        let row = self.row.saturating_add(k);
                        if row >= limits.rows {
                            budget.truncated = true;
                            break;
                        }
                        for (col, text) in &self.row_cells {
                            let Some(text) = budget.text(text) else { return ControlFlow::Break(()) };
                            cells.push((row, *col, text));
                        }
                    }
                }
                self.row = self.row.saturating_add(self.row_repeat);
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

#[cfg(test)]
pub(super) mod tests {
    use std::io::Write;

    use postman_ir::Inline;

    use super::*;
    use crate::preview::{OfficeScan, Preview, PreviewKind, preview};

    const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    const NS: &str = r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

    pub(crate) fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in files {
            w.start_file(*name, opts).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    /// `.xlsx` из листов (имя, `sheetData`), общих строк и стилей; `extra` — ещё части пакета.
    fn xlsx_of(sheets: &[(&str, &str)], strings: &[&str], styles: &str, extra: &[(&str, &[u8])]) -> Vec<u8> {
        let root = format!(
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL}/officeDocument" Target="xl/workbook.xml"/></Relationships>"#
        );
        let mut list = String::new();
        let mut links = format!(
            r#"<Relationship Id="rIdS" Type="{REL}/sharedStrings" Target="sharedStrings.xml"/><Relationship Id="rIdT" Type="{REL}/styles" Target="/xl/styles.xml"/>"#
        );
        let mut parts: Vec<(String, Vec<u8>)> = Vec::new();
        for (i, (name, data)) in sheets.iter().enumerate() {
            let state = if name.starts_with('~') { r#" state="hidden""# } else { "" };
            list.push_str(&format!(r#"<sheet name="{name}" sheetId="{}" r:id="rId{i}"{state}/>"#, i + 1));
            links.push_str(&format!(r#"<Relationship Id="rId{i}" Type="{REL}/worksheet" Target="worksheets/sheet{}.xml"/>"#, 9 - i));
            let xml = format!(r#"<?xml version="1.0" encoding="UTF-8"?><worksheet {NS}><sheetData>{data}</sheetData></worksheet>"#);
            parts.push((format!("xl/worksheets/sheet{}.xml", 9 - i), xml.into_bytes()));
        }
        let workbook = format!(r#"<workbook {NS}><workbookPr/><sheets>{list}</sheets></workbook>"#);
        let links = format!(r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{links}</Relationships>"#);
        let sst: String = strings.iter().map(|s| format!("<si>{s}</si>")).collect();
        let sst = format!(r#"<sst {NS} count="{0}" uniqueCount="{0}">{sst}</sst>"#, strings.len());
        let styles = format!(r#"<styleSheet {NS}>{styles}</styleSheet>"#);
        let mut files: Vec<(&str, &[u8])> = vec![
            ("[Content_Types].xml", b"<Types/>"),
            ("_rels/.rels", root.as_bytes()),
            ("xl/workbook.xml", workbook.as_bytes()),
            ("xl/_rels/workbook.xml.rels", links.as_bytes()),
            ("xl/sharedStrings.xml", sst.as_bytes()),
            ("xl/styles.xml", styles.as_bytes()),
        ];
        files.extend(parts.iter().map(|(n, d)| (n.as_str(), d.as_slice())));
        files.extend_from_slice(extra);
        zip_of(&files)
    }

    fn c(s: &str) -> Vec<Inline> {
        if s.is_empty() { Vec::new() } else { vec![Inline::text(s)] }
    }

    fn table(rows: &[&[&str]]) -> Vec<Block> {
        vec![Block::Table(rows.iter().map(|r| r.iter().map(|s| c(s)).collect()).collect())]
    }

    #[test]
    fn xlsx_strings_numbers_dates_and_two_sheets() {
        let styles = r##"<numFmts count="1"><numFmt numFmtId="164" formatCode="[$-419]dd/mm/yyyy\ &quot;г.&quot;"/>
            <numFmt numFmtId="165" formatCode="#,##0.00&quot;m&quot;"/></numFmts>
            <cellStyleXfs count="1"><xf numFmtId="14"/></cellStyleXfs>
            <cellXfs count="5"><xf numFmtId="0"/><xf numFmtId="14"/><xf numFmtId="164"/><xf numFmtId="22"/><xf numFmtId="165"/></cellXfs>"##;
        let first = r#"<row r="1"><c r="A1" t="s"><v>0</v></c><c r="C1" t="s"><v>1</v></c></row>
            <row r="2"><c r="A2"><v>42</v></c><c r="B2"><v>0.30000000000000004</v></c><c r="C2" s="4"><v>1234.5</v></c></row>
            <row r="4"><c r="A4" s="1"><v>46324</v></c><c r="B4" s="2"><v>46324</v></c><c r="C4" s="3"><v>46324.5</v></c></row>
            <row r="5"><c r="A5" t="b"><v>1</v></c><c r="B5" t="inlineStr"><is><r><t>в</t></r><r><t>строке</t></r><rPh><t>фонетика</t></rPh></is></c>
              <c r="C5"><f>A2*2</f><v>84</v></c><c r="D5" t="e"><v>#DIV/0!</v></c><c r="E5" s="1"/></row>"#;
        let second = r#"<row><c t="str"><f>"x"&amp;"y"</f><v>xy</v></c><c><v>-1.5E-3</v></c></row>"#;
        let x = xlsx_of(
            &[("Итоги", first), ("~скрытый", "<row><c><v>1</v></c></row>"), ("Лист2", second)],
            &["<t>Имя</t>", "<r><t xml:space=\"preserve\">Сум</t></r><r><t>ма</t></r>"],
            styles,
            &[],
        );
        let (pv, _) = preview(PreviewKind::Sheet, &x, 0, 0, 0).unwrap();
        let Preview::Document { sections, truncated } = pv else { panic!("{pv:?}") };
        assert!(!truncated);
        assert_eq!(
            sections,
            [
                Section {
                    title: SectionTitle::Sheet("Итоги".into()),
                    blocks: table(&[
                        &["Имя", "", "Сумма", ""],
                        &["42", "0.3", "1234.5", ""],
                        &["", "", "", ""],
                        &["2026-10-29", "2026-10-29", "2026-10-29 12:00", ""],
                        &["TRUE", "встроке", "84", "#DIV/0!"],
                    ])
                },
                Section { title: SectionTitle::Sheet("Лист2".into()), blocks: table(&[&["xy", "-0.0015"]]) },
            ]
        );
    }

    #[test]
    fn dates_and_numbers() {
        assert_eq!(excel_date(46324.0, false, Fmt::Date).unwrap(), "2026-10-29");
        assert_eq!(excel_date(44862.0, true, Fmt::Date).unwrap(), "2026-10-29");
        assert_eq!(excel_date(1.0, false, Fmt::Date).unwrap(), "1900-01-01");
        assert_eq!(excel_date(60.0, false, Fmt::Date).unwrap(), "1900-02-29");
        assert_eq!(excel_date(61.0, false, Fmt::Date).unwrap(), "1900-03-01");
        assert_eq!(excel_date(0.75, false, Fmt::Time).unwrap(), "18:00");
        assert_eq!(excel_date(0.999_999_9, false, Fmt::DateTime).unwrap(), "1900-01-01 00:00");
        assert!(excel_date(-1.0, false, Fmt::Date).is_none() && excel_date(1e12, false, Fmt::Date).is_none());
        assert_eq!(custom_format("mm:ss"), Fmt::Time);
        assert_eq!(custom_format("d/m/yyyy h:mm"), Fmt::DateTime);
        assert_eq!(custom_format("mmm"), Fmt::Date);
        assert_eq!(custom_format(r#"0.00" дней""#), Fmt::Number);
        assert_eq!(custom_format("[Red]#,##0;[Blue]d"), Fmt::Number);
        assert_eq!(custom_format("General"), Fmt::Number);
        assert_eq!(general(3.0), "3");
        assert_eq!(general(1e20), "1E20");
        assert_eq!(general(f64::NAN), "NaN");
        assert_eq!(cell_ref("AB12"), Some((11, 27)));
        assert_eq!(cell_ref("A0"), None);
        assert_eq!(cell_ref("ЖЖ1"), None);
    }

    #[test]
    fn xlsx_cells_beyond_the_sheet_limit_are_cut() {
        let rows: String = (1..=30).map(|r| format!(r#"<row r="{r}"><c r="A{r}"><v>{r}</v></c><c r="Z{r}"><v>1</v></c></row>"#)).collect();
        let x = xlsx_of(&[("a", &rows), ("b", "<row><c><v>1</v></c></row>"), ("c", "<row><c><v>1</v></c></row>")], &[], "", &[]);
        let limits = Limits { rows: 10, cols: 5, sheets: 2, ..LIMITS };
        let (s, cut) = parse(&x, limits).unwrap();
        assert!(cut);
        assert_eq!(s.len(), 2);
        let Block::Table(grid) = &s[0].blocks[0] else { panic!() };
        assert_eq!((grid.len(), grid[0].len()), (10, 1));
        // Ячеек в книге — не больше предела: строки листа обрезаются.
        let (s, cut) = parse(&x, Limits { cells: 53, ..LIMITS }).unwrap();
        assert!(cut);
        let Block::Table(grid) = &s[0].blocks[0] else { panic!() };
        assert_eq!((grid.len(), grid[0].len()), (2, 26));
        assert_eq!(s[1].blocks, table(&[&["1"]]));
        assert!(s[2].blocks.is_empty());
        // Символы.
        let (_, cut) = parse(&x, Limits { chars: 5, ..LIMITS }).unwrap();
        assert!(cut);
    }

    /// Книга с `vbaProject.bin` или макросами по типу содержимого — проверка Office, не просмотр.
    #[test]
    fn macro_books_are_office_scans() {
        let x = xlsx_of(&[("a", "")], &[], "", &[("xl/vbaProject.bin", b"x")]);
        let (pv, _) = preview(PreviewKind::Sheet, &x, 0, 0, 0).unwrap();
        assert!(matches!(pv, Preview::Office(OfficeScan { macros: true, .. })), "{pv:?}");
    }

    fn ods_of(body: &str) -> Vec<u8> {
        let content = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"><office:body><office:spreadsheet>{body}</office:spreadsheet></office:body></office:document-content>"#
        );
        zip_of(&[("mimetype", b"application/vnd.oasis.opendocument.spreadsheet"), ("content.xml", content.as_bytes())])
    }

    #[test]
    fn ods_values_and_repeats_up_to_the_limit() {
        let body = r#"<table:table table:name="Данные">
              <table:table-column table:number-columns-repeated="16384"/>
              <table:table-row><table:table-cell office:value-type="string"><text:p>Имя</text:p><office:annotation><text:p>заметка</text:p></office:annotation></table:table-cell>
                <table:table-cell table:number-columns-repeated="2"/><table:table-cell office:value-type="float" office:value="0.1"><text:p>0,10 ₽</text:p></table:table-cell></table:table-row>
              <table:table-row table:number-rows-repeated="2"><table:table-cell office:value-type="boolean" office:boolean-value="true"/>
                <table:table-cell><text:p>a<text:s text:c="2"/>b</text:p><text:p>c</text:p></table:table-cell></table:table-row>
              <table:table-row table:number-rows-repeated="1048570"><table:table-cell table:number-columns-repeated="16384"/></table:table-row>
            </table:table>
            <table:table table:name="Повтор"><table:table-row><table:table-cell table:number-columns-repeated="16384" office:value-type="float" office:value="7"/></table:table-row></table:table>"#;
        let (s, cut) = sheet(&ods_of(body)).unwrap();
        assert_eq!(s[0].title, SectionTitle::Sheet("Данные".into()));
        let multiline = vec![Inline::text("a  b"), Inline::LineBreak, Inline::text("c")];
        let row = vec![c("TRUE"), multiline, c(""), c("")];
        assert_eq!(s[0].blocks, [Block::Table(vec![vec![c("Имя"), c(""), c(""), c("0,10 ₽")], row.clone(), row])]);
        // Повтор столбцов 16 384 — до предела листа.
        let Block::Table(grid) = &s[1].blocks[0] else { panic!() };
        assert_eq!(grid.len(), 1);
        assert_eq!(grid[0].len(), MAX_COLS as usize);
        assert!(grid[0].iter().all(|c| *c == vec![Inline::text("7")]));
        assert!(cut);
    }

    #[test]
    fn broken_books_are_errors_and_bombs_are_cut() {
        assert!(sheet(b"plain").is_err());
        assert!(sheet(b"PK\x03\x04 garbage").is_err());
        assert!(sheet(&zip_of(&[("a.txt", b"x")])).is_err());
        let broken = xlsx_of(&[("a", "<row><c><v>1</v></row>")], &[], "", &[]);
        assert!(sheet(&broken).is_err());
        assert!(sheet(&ods_of("<table:table><table:table-row></table:table>")).is_err());
        let dtd = br#"<?xml version="1.0"?><!DOCTYPE d [<!ENTITY x "y">]><office:document-content/>"#;
        assert!(sheet(&zip_of(&[("content.xml", dtd)])).is_err());
        // Zip-бомба: лист в сотни раз больше архива — читается до предела, итог `truncated`.
        let mut rows = String::from("<row><c><v>1</v></c></row>");
        rows.push_str(&" ".repeat(4 << 20));
        rows.push_str("<row><c><v>2</v></c></row>");
        let x = xlsx_of(&[("a", &rows)], &[], "", &[]);
        assert!(x.len() < 64 * 1024, "{}", x.len());
        let started = std::time::Instant::now();
        let (s, cut) = parse(&x, Limits { xml: 1 << 20, ..LIMITS }).unwrap();
        assert!(started.elapsed() < crate::TIME_LIMIT / 4);
        assert!(cut);
        assert_eq!(s[0].blocks, table(&[&["1"]]));
        // Все части вместе — тоже в пределе.
        let (s, cut) = parse(&xlsx_of(&[("a", &rows), ("b", &rows)], &[], "", &[]), Limits { unpacked: 5 << 20, ..LIMITS }).unwrap();
        assert!(cut);
        assert_eq!(s[0].blocks, table(&[&["1"], &["2"]]));
        assert_eq!(s[1].blocks, table(&[&["1"]]));
    }
}
