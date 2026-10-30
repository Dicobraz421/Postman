//! Просмотр `.rtf` (S8-19) и тело письма Outlook в RTF (S8-20, после распаковки LZFu). Только здесь, в песочнице.
//!
//! Разбор — свой, потоковый: группы, управляющие слова и символы. Из текста берутся абзацы (`\par`; уровень структуры
//! `\outlinelevel` — заголовок; выравнивание `\qc`, `\qr`, `\qj`), жирный, курсив, подчёркнутый и зачёркнутый,
//! разрывы строк и таблицы — текстом по ячейкам (`\cell`, `\row`; вложенная — через « | » в ячейке). Кодировка —
//! `\ansicpg` документа и `\fcharset` шрифта (1250–1254, 1257), `\'hh`, `\uN` с пропуском `\ucN` замен.
//! Служебные группы (`\*`, таблицы шрифтов и цветов, стили, картинки, колонтитулы, сноски, код поля) не выводятся,
//! `\bin` пропускается по длине. Вложенность групп — не глубже [`MAX_DEPTH`] (глубже — ошибка), блоков и символов —
//! как у `.docx`; документ без конца — `truncated`.
//!
//! HTML внутри RTF (`\fromhtml1`, MS-OXRTFEX, письма Outlook): текст групп `\*\htmltag` и текст вне `\htmlrtf` — HTML,
//! он идёт тем же путём HTML → IR, что тело письма.

use std::collections::HashMap;
use std::ops::ControlFlow;

use postman_ir::{Align, Block, Inline, Marks};

use super::docx::{self, MAX_BLOCKS, MAX_CHARS};

/// Вложенность групп `{…}`: глубже — ошибка.
pub const MAX_DEPTH: usize = 256;
/// HTML внутри RTF: дальше не собираем, показанное — `truncated`.
pub const MAX_HTML: usize = 16 * 1024 * 1024;

/// Пределы разбора; в тестах — меньше.
#[derive(Debug, Clone, Copy)]
struct Limits {
    blocks: usize,
    chars: usize,
}

const LIMITS: Limits = Limits { blocks: MAX_BLOCKS, chars: MAX_CHARS };

/// Документ RTF → блоки IR и признак «показано начало».
pub fn rtf(bytes: &[u8]) -> Result<(Vec<Block>, bool), String> {
    Ok(match body(bytes)? {
        Body::Blocks(blocks, truncated) => (blocks, truncated),
        Body::Html(html, truncated) => {
            let (blocks, cut) = html_blocks(&html);
            (blocks, truncated || cut)
        }
    })
}

/// Что внутри RTF: свой текст или HTML (`\fromhtml1`).
#[derive(Debug, PartialEq)]
pub(super) enum Body {
    Blocks(Vec<Block>, bool),
    Html(String, bool),
}

/// HTML → IR тем же путём, что тело письма; блоков — не больше, чем у документа.
pub(super) fn html_blocks(html: &str) -> (Vec<Block>, bool) {
    let mut blocks = postman_ir::from_html(html).blocks;
    let cut = blocks.len() > MAX_BLOCKS;
    blocks.truncate(MAX_BLOCKS);
    (blocks, cut)
}

/// Похоже на RTF: `{\rtf` в начале (после BOM и пробелов).
pub fn is_rtf(bytes: &[u8]) -> bool {
    let b = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    b.trim_ascii_start().starts_with(b"{\\rtf")
}

pub(super) fn body(bytes: &[u8]) -> Result<Body, String> {
    parse(bytes, LIMITS)
}

fn parse(bytes: &[u8], limits: Limits) -> Result<Body, String> {
    if !is_rtf(bytes) {
        return Err("не документ RTF".into());
    }
    let mut p = Parser::new(limits);
    let complete = p.run(bytes)?;
    p.flush();
    let truncated = !complete || p.out.truncated;
    Ok(match p.html {
        Some(html) => Body::Html(html, truncated || p.html_cut),
        None => Body::Blocks(p.out.finish(), truncated),
    })
}

// --- кодировки ---

/// Однобайтовая кодовая страница Windows: верхняя половина (0x80–0xFF).
pub(super) struct Codepage([char; 128]);

impl Codepage {
    /// Известные — 1250–1254 и 1257; остальные — как 1252.
    pub(super) fn new(cp: u32) -> Self {
        const CP1250: &str = "€\u{FFFD}‚\u{FFFD}„…†‡\u{FFFD}‰Š‹ŚŤŽŹ\u{FFFD}‘’“”•–—\u{FFFD}™š›śťžź\u{A0}ˇ˘Ł¤Ą¦§¨©Ş«¬\u{AD}®Ż°±˛ł´µ¶·¸ąş»Ľ˝ľżŔÁÂĂÄĹĆÇČÉĘËĚÍÎĎĐŃŇÓÔŐÖ×ŘŮÚŰÜÝŢßŕáâăäĺćçčéęëěíîďđńňóôőö÷řůúűüýţ˙";
        const CP1251: &str = "ЂЃ‚ѓ„…†‡€‰Љ‹ЊЌЋЏђ‘’“”•–—\u{FFFD}™љ›њќћџ\u{A0}ЎўЈ¤Ґ¦§Ё©Є«¬\u{AD}®Ї°±Ііґµ¶·ё№є»јЅѕїАБВГДЕЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯабвгдежзийклмнопрстуфхцчшщъыьэюя";
        const CP1252: &str = "€\u{FFFD}‚ƒ„…†‡ˆ‰Š‹Œ\u{FFFD}Ž\u{FFFD}\u{FFFD}‘’“”•–—˜™š›œ\u{FFFD}žŸ\u{A0}¡¢£¤¥¦§¨©ª«¬\u{AD}®¯°±²³´µ¶·¸¹º»¼½¾¿ÀÁÂÃÄÅÆÇÈÉÊËÌÍÎÏÐÑÒÓÔÕÖ×ØÙÚÛÜÝÞßàáâãäåæçèéêëìíîïðñòóôõö÷øùúûüýþÿ";
        const CP1253: &str = "€\u{FFFD}‚ƒ„…†‡\u{FFFD}‰\u{FFFD}‹\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}‘’“”•–—\u{FFFD}™\u{FFFD}›\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{A0}΅Ά£¤¥¦§¨©\u{FFFD}«¬\u{AD}®―°±²³΄µ¶·ΈΉΊ»Ό½ΎΏΐΑΒΓΔΕΖΗΘΙΚΛΜΝΞΟΠΡ\u{FFFD}ΣΤΥΦΧΨΩΪΫάέήίΰαβγδεζηθικλμνξοπρςστυφχψωϊϋόύώ\u{FFFD}";
        const CP1254: &str = "€\u{FFFD}‚ƒ„…†‡ˆ‰Š‹Œ\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}‘’“”•–—˜™š›œ\u{FFFD}\u{FFFD}Ÿ\u{A0}¡¢£¤¥¦§¨©ª«¬\u{AD}®¯°±²³´µ¶·¸¹º»¼½¾¿ÀÁÂÃÄÅÆÇÈÉÊËÌÍÎÏĞÑÒÓÔÕÖ×ØÙÚÛÜİŞßàáâãäåæçèéêëìíîïğñòóôõö÷øùúûüışÿ";
        const CP1257: &str = "€\u{FFFD}‚\u{FFFD}„…†‡\u{FFFD}‰\u{FFFD}‹\u{FFFD}¨ˇ¸\u{FFFD}‘’“”•–—\u{FFFD}™\u{FFFD}›\u{FFFD}¯˛\u{FFFD}\u{A0}\u{FFFD}¢£¤\u{FFFD}¦§Ø©Ŗ«¬\u{AD}®Æ°±²³´µ¶·ø¹ŗ»¼½¾æĄĮĀĆÄÅĘĒČÉŹĖĢĶĪĻŠŃŅÓŌÕÖ×ŲŁŚŪÜŻŽßąįāćäåęēčéźėģķīļšńņóōõö÷ųłśūüżž˙";
        let high = match cp {
            1250 => CP1250,
            1251 => CP1251,
            1253 => CP1253,
            1254 => CP1254,
            1257 => CP1257,
            _ => CP1252,
        };
        let mut table = ['\u{FFFD}'; 128];
        for (slot, c) in table.iter_mut().zip(high.chars()) {
            *slot = c;
        }
        Codepage(table)
    }

    pub(super) fn char(&self, b: u8) -> char {
        if b < 0x80 { char::from(b) } else { self.0[usize::from(b - 0x80)] }
    }
}

/// `\fcharsetN` шрифта → кодовая страница; 0 (ANSI) и 1 (по умолчанию) — страница документа.
fn charset_codepage(charset: i32) -> Option<u32> {
    Some(match charset {
        161 => 1253,
        162 => 1254,
        186 => 1257,
        204 => 1251,
        238 => 1250,
        2 | 77 => 1252,
        _ => return None,
    })
}

// --- разбор ---

/// Куда идёт текст группы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dest {
    Text,
    /// Служебная группа: текст не выводится.
    Skip,
    /// Таблица шрифтов: только номера и кодировки.
    Fonts,
    /// `\*\htmltag`: HTML как есть.
    Html,
}

/// Состояние группы: восстанавливается на `}`.
#[derive(Debug, Clone, Copy)]
struct Group {
    dest: Dest,
    marks: Marks,
    hidden: bool,
    /// Сколько замен пропускать после `\uN`.
    uc: usize,
    /// Номер шрифта (кодировка — по таблице шрифтов).
    font: i32,
    /// `\htmlrtf`: только для RTF, в HTML не идёт.
    htmlrtf: bool,
}

struct Parser {
    groups: Vec<Group>,
    /// `\*` перед управляющим словом: неизвестная группа пропускается.
    star: bool,
    /// Страница документа (`\ansicpg`) и шрифтов (`\fcharset`).
    cp: u32,
    fonts: HashMap<i32, u32>,
    pages: HashMap<u32, Codepage>,
    /// Шрифт в таблице шрифтов, которому пишем кодировку.
    font_def: i32,
    /// Ещё столько замен `\uN` пропустить.
    skip_chars: usize,
    /// Старшая половина суррогатной пары из `\uN`.
    surrogate: Option<u16>,
    /// Накопленный текст (выводится на следующем управляющем слове или скобке).
    text: String,
    /// HTML внутри RTF (`\fromhtml1`).
    html: Option<String>,
    html_cut: bool,
    out: Builder,
}

impl Parser {
    fn new(limits: Limits) -> Self {
        let root = Group { dest: Dest::Text, marks: Marks::default(), hidden: false, uc: 1, font: -1, htmlrtf: false };
        Parser {
            groups: vec![root],
            star: false,
            cp: 1252,
            fonts: HashMap::new(),
            pages: HashMap::new(),
            font_def: -1,
            skip_chars: 0,
            surrogate: None,
            text: String::new(),
            html: None,
            html_cut: false,
            out: Builder::new(limits),
        }
    }

    fn group(&self) -> Group {
        self.groups.last().copied().unwrap_or(Group { dest: Dest::Skip, marks: Marks::default(), hidden: true, uc: 1, font: -1, htmlrtf: false })
    }

    fn group_mut(&mut self) -> Option<&mut Group> {
        // Корневая группа — до первой `{`; её не меняем.
        if self.groups.len() > 1 { self.groups.last_mut() } else { None }
    }

    /// Весь документ; `false` — кончился раньше закрывающей скобки (или стоп по пределу).
    fn run(&mut self, b: &[u8]) -> Result<bool, String> {
        let mut i = b.iter().position(|&c| c == b'{').unwrap_or(b.len());
        while i < b.len() {
            match b[i] {
                b'{' => {
                    self.flush();
                    if self.groups.len() > MAX_DEPTH {
                        return Err("rtf: слишком глубокая вложенность".into());
                    }
                    let g = self.group();
                    self.groups.push(g);
                    (self.star, self.skip_chars) = (false, 0);
                    i += 1;
                }
                b'}' => {
                    self.flush();
                    self.groups.pop();
                    (self.star, self.skip_chars) = (false, 0);
                    i += 1;
                    if self.groups.len() <= 1 {
                        return Ok(true);
                    }
                }
                b'\\' => {
                    let (word, param, next) = control(b, i + 1);
                    i = next;
                    if word == b"bin" {
                        // Двоичные данные — не текст.
                        let n = usize::try_from(param.unwrap_or(0).max(0)).unwrap_or(0);
                        i = i.saturating_add(n).min(b.len());
                        continue;
                    }
                    if word == b"'" {
                        let hex = b.get(i..i + 2).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
                        if let Some(byte) = hex {
                            i += 2;
                            self.byte(byte);
                        }
                        continue;
                    }
                    if self.word(word, param).is_break() {
                        self.flush();
                        return Ok(false);
                    }
                }
                b'\r' | b'\n' | 0 => i += 1,
                c => {
                    self.byte(c);
                    i += 1;
                }
            }
            if self.out.stop {
                self.flush();
                return Ok(false);
            }
        }
        Ok(false)
    }

    /// Байт текста в кодировке текущего шрифта (замены после `\uN` пропускаются).
    fn byte(&mut self, b: u8) {
        if self.skip_chars > 0 {
            self.skip_chars -= 1;
            return;
        }
        let g = self.group();
        if g.dest == Dest::Skip || g.dest == Dest::Fonts {
            return;
        }
        let cp = self.fonts.get(&g.font).copied().unwrap_or(self.cp);
        let c = self.pages.entry(cp).or_insert_with(|| Codepage::new(cp)).char(b);
        self.push_char(c);
    }

    fn push_char(&mut self, c: char) {
        self.surrogate = None;
        self.text.push(c);
    }

    /// Накопленный текст — в вывод группы.
    fn flush(&mut self) {
        if self.text.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.text);
        let g = self.group();
        match (&mut self.html, g.dest) {
            (Some(_), Dest::Html) => self.html_push(&text),
            (Some(_), Dest::Text) if !g.htmlrtf => self.html_push(&text),
            (Some(_), _) => {}
            (None, Dest::Text) if !g.hidden => {
                let _ = self.out.text(&text, g.marks);
            }
            (None, _) => {}
        }
    }

    fn html_push(&mut self, s: &str) {
        let Some(html) = &mut self.html else { return };
        if html.len() + s.len() > MAX_HTML {
            self.html_cut = true;
            self.out.stop = true;
            return;
        }
        html.push_str(s);
    }

    /// Разрыв: в HTML — перевод строки, в документ — абзац, строка, ячейка.
    fn structure(&mut self, f: impl FnOnce(&mut Builder) -> ControlFlow<()>) -> ControlFlow<()> {
        let g = self.group();
        if self.html.is_some() {
            if g.dest == Dest::Html || (g.dest == Dest::Text && !g.htmlrtf) {
                self.html_push("\r\n");
            }
            return ControlFlow::Continue(());
        }
        if g.dest != Dest::Text {
            return ControlFlow::Continue(());
        }
        f(&mut self.out)
    }

    /// Управляющее слово или символ.
    fn word(&mut self, word: &[u8], param: Option<i32>) -> ControlFlow<()> {
        let star = std::mem::take(&mut self.star);
        // Знаки текста — без сброса накопленного.
        let symbol = match word {
            b"\\" => Some('\\'),
            b"{" => Some('{'),
            b"}" => Some('}'),
            b"~" => Some('\u{A0}'),
            b"_" => Some('\u{2011}'),
            b"emdash" => Some('—'),
            b"endash" => Some('–'),
            b"bullet" => Some('•'),
            b"lquote" => Some('‘'),
            b"rquote" => Some('’'),
            b"ldblquote" => Some('“'),
            b"rdblquote" => Some('”'),
            b"emspace" | b"enspace" | b"qmspace" => Some(' '),
            _ => None,
        };
        if let Some(c) = symbol {
            if self.skip_chars > 0 {
                self.skip_chars -= 1;
            } else {
                self.push_char(c);
            }
            return ControlFlow::Continue(());
        }
        if word == b"u" {
            return self.unicode(param.unwrap_or(0));
        }
        self.flush();
        self.skip_chars = 0;
        let on = param.is_none_or(|p| p != 0);
        // Свойства абзаца — только из текста (не из стилей и служебных групп).
        let text = self.group().dest == Dest::Text;
        match word {
            b"*" => self.star = true,
            b"ansicpg" => self.cp = param.and_then(|p| u32::try_from(p).ok()).unwrap_or(1252),
            b"fromhtml" if on && self.groups.len() == 2 => self.html = Some(String::new()),
            b"htmltag" if star && self.html.is_some() => self.set(|g| g.dest = Dest::Html),
            b"htmlrtf" => self.set(|g| g.htmlrtf = on),
            b"fonttbl" => self.set(|g| g.dest = Dest::Fonts),
            b"f" if self.group().dest == Dest::Fonts => self.font_def = param.unwrap_or(-1),
            b"fcharset" if self.group().dest == Dest::Fonts => {
                if let Some(cp) = charset_codepage(param.unwrap_or(0)) {
                    self.fonts.insert(self.font_def, cp);
                }
            }
            b"f" => {
                let font = param.unwrap_or(-1);
                self.set(|g| g.font = font);
            }
            b"colortbl"
            | b"stylesheet"
            | b"info"
            | b"pict"
            | b"header"
            | b"headerl"
            | b"headerr"
            | b"headerf"
            | b"footer"
            | b"footerl"
            | b"footerr"
            | b"footerf"
            | b"footnote"
            | b"fldinst"
            | b"listtable"
            | b"listoverridetable"
            | b"revtbl"
            | b"rsidtbl"
            | b"xmlnstbl"
            | b"filetbl"
            | b"objdata"
            | b"nonshppict"
            | b"themedata"
            | b"colorschememapping"
            | b"latentstyles"
            | b"datastore"
            | b"annotation"
            | b"atnid"
            | b"atnauthor"
            | b"xe"
            | b"tc"
            | b"txe"
            | b"mmathPr"
            | b"pgdsctbl" => self.set(|g| g.dest = Dest::Skip),
            _ if star => self.set(|g| g.dest = Dest::Skip),
            b"uc" => {
                let n = usize::try_from(param.unwrap_or(1).max(0)).unwrap_or(1).min(16);
                self.set(|g| g.uc = n);
            }
            b"plain" => self.set(|g| (g.marks, g.hidden) = (Marks::default(), false)),
            b"b" => self.set(|g| g.marks.bold = on),
            b"i" => self.set(|g| g.marks.italic = on),
            b"ul" | b"uld" | b"uldash" | b"uldashd" | b"uldashdd" | b"uldb" | b"ulth" | b"ulw" | b"ulwave" | b"ulhwave" | b"ulldash" => {
                self.set(|g| g.marks.underline = on)
            }
            b"ulnone" => self.set(|g| g.marks.underline = false),
            b"strike" | b"striked" => self.set(|g| g.marks.strike = on),
            b"v" => self.set(|g| g.hidden = on),
            b"pard" if text => self.out.pard(),
            b"intbl" if text => self.out.intbl = true,
            b"outlinelevel" if text => self.out.outline = param.and_then(|p| u8::try_from(p).ok()).filter(|&l| l < 9).map(|l| (l + 1).min(6)),
            b"ql" if text => self.out.align = Align::Start,
            b"qc" if text => self.out.align = Align::Center,
            b"qr" if text => self.out.align = Align::End,
            b"qj" | b"qd" if text => self.out.align = Align::Justify,
            b"par" | b"\n" | b"\r" | b"page" | b"sect" => return self.structure(Builder::par),
            b"line" => return self.structure(Builder::line),
            b"tab" => {
                self.push_char('\t');
            }
            b"cell" => return self.structure(Builder::cell),
            b"nestcell" => return self.structure(|o| o.text(" | ", Marks::default())),
            b"row" => return self.structure(Builder::row),
            _ => {}
        }
        ControlFlow::Continue(())
    }

    /// Свойство текущей группы.
    fn set(&mut self, f: impl FnOnce(&mut Group)) {
        if let Some(g) = self.group_mut() {
            f(g);
        }
    }

    /// `\uN`: знак UTF-16 (отрицательные — с 65536), пары суррогатов — в один знак; дальше — `\ucN` замен.
    fn unicode(&mut self, n: i32) -> ControlFlow<()> {
        let unit = n.rem_euclid(65536) as u16;
        let high = self.surrogate.take();
        match unit {
            0xD800..=0xDBFF => self.surrogate = Some(unit),
            0xDC00..=0xDFFF => {
                if let Some(h) = high
                    && let Some(c) = char::decode_utf16([h, unit]).next().and_then(Result::ok)
                {
                    self.text.push(c);
                }
            }
            _ => self.text.push(char::from_u32(u32::from(unit)).unwrap_or('\u{FFFD}')),
        }
        self.skip_chars = self.group().uc;
        ControlFlow::Continue(())
    }
}

/// Управляющее слово после `\` с позиции `i`: слово (или символ), параметр и позиция за ним (пробел-разделитель
/// съеден). Перевод строки после `\` — как `\par`.
fn control(b: &[u8], i: usize) -> (&[u8], Option<i32>, usize) {
    let Some(&first) = b.get(i) else { return (b"", None, i) };
    if !first.is_ascii_alphabetic() {
        return (&b[i..i + 1], None, i + 1);
    }
    let mut j = i;
    while j < b.len() && b[j].is_ascii_alphabetic() && j - i < 32 {
        j += 1;
    }
    let word = &b[i..j];
    let start = j;
    if b.get(j) == Some(&b'-') && b.get(j + 1).is_some_and(u8::is_ascii_digit) {
        j += 1;
    }
    while j < b.len() && b[j].is_ascii_digit() && j - start < 11 {
        j += 1;
    }
    let param =
        std::str::from_utf8(&b[start..j]).ok().and_then(|s| s.parse::<i64>().ok()).map(|p| p.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32);
    if b.get(j) == Some(&b' ') {
        j += 1;
    }
    (word, param, j)
}

// --- документ ---

/// Строки → ячейки → инлайны.
type Rows = Vec<Vec<Vec<Inline>>>;

struct Builder {
    limits: Limits,
    out: Vec<Block>,
    /// Текущий абзац: текст и его свойства (сбрасывает `\pard`).
    inlines: Vec<Inline>,
    align: Align,
    outline: Option<u8>,
    intbl: bool,
    /// Открытая таблица и её текущая строка.
    table: Rows,
    row: Vec<Vec<Inline>>,
    blocks: usize,
    chars: usize,
    truncated: bool,
    /// Предел достигнут: разбор останавливается.
    stop: bool,
}

impl Builder {
    fn new(limits: Limits) -> Self {
        Builder {
            limits,
            out: Vec::new(),
            inlines: Vec::new(),
            align: Align::Start,
            outline: None,
            intbl: false,
            table: Vec::new(),
            row: Vec::new(),
            blocks: 0,
            chars: 0,
            truncated: false,
            stop: false,
        }
    }

    fn pard(&mut self) {
        (self.align, self.outline, self.intbl) = (Align::Start, None, false);
    }

    /// Текст с пределом символов; управляющие и bidi-символы убираются.
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
            docx::push(&mut self.inlines, Inline::Text { text: clean, marks });
        }
        if taken > left {
            return self.halt();
        }
        ControlFlow::Continue(())
    }

    fn halt(&mut self) -> ControlFlow<()> {
        (self.truncated, self.stop) = (true, true);
        ControlFlow::Break(())
    }

    /// Разрыв строки, ячейка — по символу из предела.
    fn spend(&mut self) -> ControlFlow<()> {
        self.chars += 1;
        if self.chars > self.limits.chars {
            return self.halt();
        }
        ControlFlow::Continue(())
    }

    fn line(&mut self) -> ControlFlow<()> {
        self.inlines.push(Inline::LineBreak);
        self.spend()
    }

    /// Конец абзаца: в таблице — строка ячейки, иначе — абзац или заголовок (таблица перед ним закрывается).
    fn par(&mut self) -> ControlFlow<()> {
        if self.intbl {
            return self.line();
        }
        self.end_table()?;
        let content = std::mem::take(&mut self.inlines);
        if docx::is_blank(&content) {
            return ControlFlow::Continue(());
        }
        let block = match self.outline {
            Some(level) => Block::Heading { level, content },
            None => Block::Paragraph { align: self.align, content },
        };
        self.emit(block)
    }

    fn cell(&mut self) -> ControlFlow<()> {
        let mut content = std::mem::take(&mut self.inlines);
        while matches!(content.last(), Some(Inline::LineBreak)) {
            content.pop();
        }
        self.row.push(content);
        self.spend()
    }

    fn row(&mut self) -> ControlFlow<()> {
        let row = std::mem::take(&mut self.row);
        self.inlines.clear();
        if row.is_empty() {
            return ControlFlow::Continue(());
        }
        self.table.push(row);
        self.count()
    }

    fn end_table(&mut self) -> ControlFlow<()> {
        if !self.row.is_empty() {
            let row = std::mem::take(&mut self.row);
            self.table.push(row);
        }
        let rows = std::mem::take(&mut self.table);
        if !rows.iter().flatten().all(|c| c.is_empty()) {
            self.out.push(Block::Table(rows));
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
            return self.halt();
        }
        ControlFlow::Continue(())
    }

    /// Конец разбора (в том числе по пределу): недособранное — как есть.
    fn finish(mut self) -> Vec<Block> {
        if self.intbl && !self.inlines.is_empty() {
            let _ = self.cell();
        }
        self.intbl = false;
        let _ = self.par();
        let _ = self.end_table();
        self.out
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

    fn blocks(src: &str) -> Vec<Block> {
        let (b, cut) = rtf(src.as_bytes()).unwrap();
        assert!(!cut, "{src}");
        b
    }

    /// WordPad (русская Windows): `\ansicpg1251`, шрифт `\fcharset204`, кириллица — `\'hh`, жирный и курсив.
    #[test]
    fn wordpad_cyrillic_by_hex_escapes() {
        let src = r"{\rtf1\ansi\ansicpg1251\deff0\nouicompat\deflang1049{\fonttbl{\f0\fnil\fcharset204 Calibri;}{\f1\fnil\fcharset0 Arial;}}
{\colortbl ;\red255\green0\blue0;}
{\*\generator Riched20 10.0.19041}\viewkind4\uc1 
\pard\sa200\sl276\slmult1\f0\fs22\lang1049\'cf\'f0\'e8\'e2\'e5\'f2, \b\'ec\'e8\'f0\b0  \i\'ea\'f3\'f0\'f1\'e8\'e2\i0\par
\pard\qc\'d6\'e5\'ed\'f2\'f0\par
}
";
        let bold = Marks { bold: true, ..Marks::default() };
        let it = Marks { italic: true, ..Marks::default() };
        assert_eq!(
            blocks(src),
            [
                p(vec![t("Привет, "), Inline::Text { text: "мир".into(), marks: bold }, t(" "), Inline::Text { text: "курсив".into(), marks: it }]),
                Block::Paragraph { align: Align::Center, content: vec![t("Центр")] },
            ]
        );
    }

    /// Word: `\uN` с заменой `\'hh` (пропуск по `\uc1`), шрифт латиницы `\fcharset0` в документе 1252, суррогатная
    /// пара, заголовок по `\outlinelevel`, подчёркнутый, скрытый текст, поле — только результат, картинка пропущена.
    #[test]
    fn word_unicode_with_fallbacks() {
        let src = r#"{\rtf1\adeflang1025\ansi\ansicpg1252\uc1\adeff31507\deff0{\fonttbl{\f0\fbidi \froman\fcharset0\fprq2{\*\panose 02020603050405020304}Times New Roman;}{\f43\fbidi \froman\fcharset204\fprq2 Times New Roman Cyr;}}
{\stylesheet{\ql \li0\ri0 Normal;}{\s1\ql \outlinelevel0 heading 1;}}{\*\rsidtbl \rsid1}{\info{\title Отчёт}{\author A}}
\pard\plain \s1\outlinelevel0\f0 {\b \u1054\'3f\u1090\u1095\'3f\u1105\'3f\u1090\'3f}\par
\pard\plain \f0 \u1058\'d2\u1077\'e5\u1082\'ea\u1089\'f1\u1090\'f2 {\ul \u-10179\'3f\u-8704\'3f}{\v hidden}{\field{\*\fldinst {HYPERLINK "https://example.com"}}{\fldrslt {link}}}{\pict\pngblip 89504e47}{\f43 \'c0}\par
}"#;
        let b = blocks(src);
        let bold = Marks { bold: true, ..Marks::default() };
        let ul = Marks { underline: true, ..Marks::default() };
        assert_eq!(
            b,
            [
                Block::Heading { level: 1, content: vec![Inline::Text { text: "Отчёт".into(), marks: bold }] },
                p(vec![t("Текст "), Inline::Text { text: "😀".into(), marks: ul }, t("linkА")]),
            ]
        );
    }

    #[test]
    fn tables_are_text_by_cells() {
        let src = r"{\rtf1\ansi\ansicpg1251
\trowd\cellx1000\cellx2000\pard\intbl \'c8\'ec\'ff\cell \'d1\'f0\'ee\'ea\cell\row
\trowd\cellx1000\cellx2000\pard\intbl \'d1\'ec\'e5\'f2\'e0\par v2\cell x\nestcell y\cell\row
\pard \'cf\'ee\'f1\'eb\'e5\par}";
        assert_eq!(
            blocks(src),
            [
                Block::Table(vec![vec![vec![t("Имя")], vec![t("Срок")]], vec![vec![t("Смета"), Inline::LineBreak, t("v2")], vec![t("x | y")]]]),
                p(vec![t("После")]),
            ]
        );
    }

    #[test]
    fn deep_nesting_is_an_error_and_unfinished_is_truncated() {
        let deep = format!(r"{{\rtf1 {}x{}}}", "{".repeat(MAX_DEPTH), "}".repeat(MAX_DEPTH));
        let e = rtf(deep.as_bytes()).unwrap_err();
        assert!(e.contains("вложенность"), "{e}");
        let fits = format!(r"{{\rtf1 {}x{}}}", "{".repeat(MAX_DEPTH - 1), "}".repeat(MAX_DEPTH - 1));
        assert_eq!(rtf(fits.as_bytes()).unwrap().0, [p(vec![t("x")])]);
        // `{\rtf1` без конца — показанное и `truncated`.
        let (b, cut) = rtf(br"{\rtf1\ansi abc\par de").unwrap();
        assert!(cut);
        assert_eq!(b, [p(vec![t("abc")]), p(vec![t("de")])]);
        // Не RTF, мусор и `\bin` с ложной длиной — без паники.
        assert!(rtf(b"plain").is_err());
        assert!(rtf(br"{\rtf1 \bin99999999 xyz}").is_ok());
        assert!(rtf(br"{\rtf1 \'zz\u\uc-5\'").is_ok());
    }

    #[test]
    fn limits_cut_the_document() {
        let src = format!(r"{{\rtf1 {}}}", r"abc\par ".repeat(20));
        let Body::Blocks(b, cut) = parse(src.as_bytes(), Limits { blocks: 5, ..LIMITS }).unwrap() else { panic!() };
        assert!(cut && b.len() == 5);
        let Body::Blocks(b, cut) = parse(src.as_bytes(), Limits { chars: 4, ..LIMITS }).unwrap() else { panic!() };
        assert!(cut);
        assert_eq!(b, [p(vec![t("abc")]), p(vec![t("a")])]);
    }

    /// Outlook: HTML внутри RTF — теги из `\*\htmltag`, текст вне `\htmlrtf`; дальше — HTML → IR.
    #[test]
    fn html_inside_rtf() {
        let src = r#"{\rtf1\ansi\ansicpg1251\fromhtml1 \deff0{\fonttbl{\f0\fswiss\fcharset204 Arial;}}
{\*\htmltag19 <html>}{\*\htmltag50 <body>}{\*\htmltag64 <p>}\htmlrtf {\htmlrtf0 \'cf\'f0\'e8\'e2\'e5\'f2 
{\*\htmltag84 <b>}\htmlrtf {\b \htmlrtf0 \'ec\'e8\'f0{\*\htmltag92 </b>}\htmlrtf }\htmlrtf0 
{\*\htmltag72 </p>}\htmlrtf \par}\htmlrtf0 {\*\htmltag72 <a href="javascript:x">}x{\*\htmltag72 </a>}}"#;
        let Body::Html(html, false) = body(src.as_bytes()).unwrap() else { panic!() };
        assert!(html.contains("<p>Привет") && html.contains("<b>мир</b>"), "{html}");
        let b = rtf(src.as_bytes()).unwrap().0;
        let bold = Marks { bold: true, ..Marks::default() };
        assert_eq!(b[0], p(vec![t("Привет "), Inline::Text { text: "мир".into(), marks: bold }]));
        assert!(!format!("{b:?}").contains("javascript"), "{b:?}");
    }

    #[test]
    fn document_kind_sniffs_rtf() {
        let (pv, _) = preview(PreviewKind::Document, br"{\rtf1 hi\par}", 0, 0, 0).unwrap();
        assert_eq!(pv, Preview::Document { sections: vec![Section { title: SectionTitle::None, blocks: vec![p(vec![t("hi")])] }], truncated: false });
    }
}
