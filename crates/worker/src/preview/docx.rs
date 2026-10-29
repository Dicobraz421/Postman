//! Просмотр документа Word `.docx` (T1-13а): `word/document.xml` → безопасный IR. Только здесь, в песочнице.
//!
//! ZIP — с теми же защитами, что оглавление и проверка Office: часть документа читается не больше [`MAX_XML`] байт
//! после распаковки (ложные размеры каталога и zip-бомба упираются в этот предел), служебные части (стили, нумерация,
//! связи) — не больше [`MAX_PART`]. XML — потоковый (`quick-xml`): `<!DOCTYPE` — ошибка, сущность кроме пяти
//! встроенных и символьных ссылок — ошибка, вложенность — не глубже [`MAX_DEPTH`]. Наружу — не больше
//! [`MAX_BLOCKS`] блоков и [`MAX_CHARS`] символов (`truncated`). Время ограничивает сторож в `serve`.
//!
//! Из документа берутся абзацы, заголовки (стиль «heading N»/«Title» или уровень структуры), жирный, курсив,
//! подчёркнутый и зачёркнутый, списки (`numPr`, вид — из нумерации), таблицы (текстом по ячейкам; вложенная —
//! строкой в ячейке), внешние ссылки (через проверку ссылок IR) и разрывы строк. Картинки, объекты, сноски и
//! примечания — пометкой («[рисунок]» и т. п.); у полей — показанное значение, без кода поля; скрытый и удалённый
//! правкой текст и колонтитулы не выводятся.

use std::cell::Cell;
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read};
use std::ops::ControlFlow;
use std::rc::Rc;

use postman_ir::{Align, Block, Inline, Marks, check_link, plain_text};
use quick_xml::events::Event;
use quick_xml::name::ResolveResult;
use quick_xml::reader::NsReader;

/// `document.xml` после распаковки: дальше не читаем, показанное — `truncated`.
pub const MAX_XML: u64 = 16 * 1024 * 1024;
/// Стили, нумерация, связи: больше — часть не учитывается.
pub const MAX_PART: u64 = 4 * 1024 * 1024;
/// Блоков в ответе (пункт списка и строка таблицы — тоже блок).
pub const MAX_BLOCKS: usize = 10_000;
/// Символов текста в ответе.
pub const MAX_CHARS: usize = 1_000_000;
/// Глубже документы Word не бывают (таблица в таблице — с запасом).
pub const MAX_DEPTH: usize = 256;

const W: &[u8] = b"http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const W_STRICT: &[u8] = b"http://purl.oclc.org/ooxml/wordprocessingml/main";
const MC: &[u8] = b"http://schemas.openxmlformats.org/markup-compatibility/2006";

/// Пределы разбора; в тестах — меньше.
#[derive(Debug, Clone, Copy)]
struct Limits {
    xml: u64,
    blocks: usize,
    chars: usize,
}

const LIMITS: Limits = Limits { xml: MAX_XML, blocks: MAX_BLOCKS, chars: MAX_CHARS };

/// Документ `.docx` → блоки IR и признак «показано начало».
pub fn docx(bytes: &[u8]) -> Result<(Vec<Block>, bool), String> {
    parse(bytes, LIMITS)
}

fn parse(bytes: &[u8], limits: Limits) -> Result<(Vec<Block>, bool), String> {
    if !bytes.starts_with(b"PK\x03\x04") {
        return Err("не документ .docx (старый .doc или защищён паролем)".into());
    }
    let mut zip = zip::ZipArchive::new(io::Cursor::new(bytes)).map_err(|e| format!("документ не разобран: {e}"))?;
    // Основная часть — по связи пакета, обычно `word/document.xml`.
    let main = part(&mut zip, "_rels/.rels")
        .and_then(|d| rels(&d).ok())
        .and_then(|r| r.into_iter().find(|r| r.kind.ends_with("/officeDocument")).map(|r| resolve("", &r.target)))
        .unwrap_or_else(|| "word/document.xml".into());
    let (dir, file) = match main.rsplit_once('/') {
        Some((d, f)) => (format!("{d}/"), f.to_string()),
        None => (String::new(), main.clone()),
    };
    let links = part(&mut zip, &format!("{dir}_rels/{file}.rels")).and_then(|d| rels(&d).ok()).unwrap_or_default();
    let target = |kind: &str, default: &str| {
        links.iter().find(|r| r.kind.ends_with(kind)).map_or_else(|| format!("{dir}{default}"), |r| resolve(&dir, &r.target))
    };
    let (styles_at, numbering_at) = (target("/styles", "styles.xml"), target("/numbering", "numbering.xml"));
    let styles = part(&mut zip, &styles_at).and_then(|d| styles(&d).ok()).unwrap_or_default();
    let numbering = part(&mut zip, &numbering_at).and_then(|d| numbering(&d).ok()).unwrap_or_default();
    let links: HashMap<String, String> =
        links.into_iter().filter(|r| r.external && r.kind.ends_with("/hyperlink")).map(|r| (r.id, r.target)).collect();

    let f = zip.by_name(&main).map_err(|_| "в документе нет текста Word".to_string())?;
    let hit = Rc::new(Cell::new(false));
    let src = BufReader::new(Limit { inner: f, left: limits.xml, hit: hit.clone() });
    let mut b = Builder::new(&styles, &numbering, &links, limits);
    match walk(src, ns, |ev| b.event(ev)) {
        // Обрезали по пределу посреди элемента — это не порча документа.
        Err(_) if hit.get() => {}
        Err(e) => return Err(format!("документ не разобран: {e}")),
        Ok(()) => {}
    }
    let truncated = b.truncated || hit.get();
    Ok((b.finish(), truncated))
}

/// Служебная часть пакета целиком, если она есть и не больше [`MAX_PART`].
pub(super) fn part(zip: &mut zip::ZipArchive<io::Cursor<&[u8]>>, name: &str) -> Option<Vec<u8>> {
    let f = zip.by_name(name).ok()?;
    let mut data = Vec::new();
    f.take(MAX_PART + 1).read_to_end(&mut data).ok()?;
    (data.len() as u64 <= MAX_PART).then_some(data)
}

/// Путь части по ссылке из связей: от каталога источника (`word/`) или от корня пакета (`/…`).
pub(super) fn resolve(dir: &str, target: &str) -> String {
    let joined = match target.strip_prefix('/') {
        Some(abs) => abs.to_string(),
        None => format!("{dir}{target}"),
    };
    let mut out: Vec<&str> = Vec::new();
    for seg in joined.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

/// Чтение не дальше предела; `hit` — за пределом есть ещё данные.
pub(super) struct Limit<R> {
    pub inner: R,
    pub left: u64,
    pub hit: Rc<Cell<bool>>,
}

impl<R: Read> Read for Limit<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.left == 0 {
            let mut probe = [0u8; 1];
            if self.inner.read(&mut probe)? > 0 {
                self.hit.set(true);
            }
            return Ok(0);
        }
        let n = buf.len().min(usize::try_from(self.left).unwrap_or(usize::MAX));
        let got = self.inner.read(&mut buf[..n])?;
        self.left -= got as u64;
        Ok(got)
    }
}

// --- XML ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ns {
    /// WordprocessingML (переходный и строгий).
    W,
    /// Совместимость разметки (`mc:AlternateContent`).
    Mc,
    Other,
}

fn ns(uri: &[u8]) -> Ns {
    match uri {
        W | W_STRICT => Ns::W,
        MC => Ns::Mc,
        _ => Ns::Other,
    }
}

/// Событие XML: пространство имён (как его различает разборщик части), локальное имя, атрибуты (локальное имя →
/// значение; у атрибутов связей OOXML — `r:` и локальное имя, иначе `r:id` и `id` у `p:sldId` не различить).
pub(super) enum Ev<'a, N> {
    Start(N, &'a [u8], &'a [(Vec<u8>, String)]),
    End(N, &'a [u8]),
    Text(&'a str),
}

pub(super) fn attr<'a>(attrs: &'a [(Vec<u8>, String)], key: &[u8]) -> Option<&'a str> {
    attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

const REL: &[u8] = b"http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const REL_STRICT: &[u8] = b"http://purl.oclc.org/ooxml/officeDocument/relationships";

/// Потоковый обход: пустой элемент — начало и конец; DTD и чужие сущности — ошибка; `Break` — стоп без ошибки.
/// `ns` — пространство имён элемента по его URI (без пространства — пустой URI).
pub(super) fn walk<R: BufRead, N: Copy>(src: R, ns: impl Fn(&[u8]) -> N, mut on: impl FnMut(Ev<'_, N>) -> ControlFlow<()>) -> Result<(), String> {
    let mut reader = NsReader::from_reader(src);
    reader.config_mut().expand_empty_elements = true;
    let mut buf = Vec::new();
    let mut attrs: Vec<(Vec<u8>, String)> = Vec::new();
    let mut depth = 0usize;
    loop {
        buf.clear();
        let (uri, event) = reader.read_resolved_event_into(&mut buf).map_err(|e| format!("xml: {e}"))?;
        let ns = match uri {
            ResolveResult::Bound(n) => ns(n.as_ref()),
            _ => ns(b""),
        };
        let flow = match event {
            Event::Start(e) => {
                depth += 1;
                if depth > MAX_DEPTH {
                    return Err("xml: слишком глубокая вложенность".into());
                }
                attrs.clear();
                for a in e.attributes() {
                    let a = a.map_err(|e| format!("xml: {e}"))?;
                    if a.key.as_ref().starts_with(b"xmlns") {
                        continue;
                    }
                    let v = a.normalized_value(quick_xml::XmlVersion::default()).map_err(|e| format!("xml: {e}"))?;
                    let mut key = match reader.resolver().resolve_attribute(a.key).0 {
                        ResolveResult::Bound(n) if n.as_ref() == REL || n.as_ref() == REL_STRICT => b"r:".to_vec(),
                        _ => Vec::new(),
                    };
                    key.extend_from_slice(a.key.local_name().as_ref());
                    attrs.push((key, v.into_owned()));
                }
                on(Ev::Start(ns, e.local_name().as_ref(), &attrs))
            }
            Event::End(e) => {
                depth = depth.saturating_sub(1);
                on(Ev::End(ns, e.local_name().as_ref()))
            }
            Event::Text(t) => on(Ev::Text(&t.decode().map_err(|e| format!("xml: {e}"))?)),
            Event::CData(t) => on(Ev::Text(&t.decode().map_err(|e| format!("xml: {e}"))?)),
            Event::GeneralRef(r) => {
                let s = match r.resolve_char_ref().map_err(|e| format!("xml: {e}"))? {
                    Some(c) => c.to_string(),
                    None => {
                        let name = r.decode().map_err(|e| format!("xml: {e}"))?;
                        quick_xml::escape::resolve_predefined_entity(&name).ok_or("xml: неизвестная сущность")?.to_string()
                    }
                };
                on(Ev::Text(&s))
            }
            Event::DocType(_) => return Err("xml: DTD запрещён".into()),
            Event::Empty(_) | Event::Decl(_) | Event::PI(_) | Event::Comment(_) => ControlFlow::Continue(()),
            Event::Eof => return Ok(()),
        };
        if flow.is_break() {
            return Ok(());
        }
    }
}

// --- служебные части ---

/// Связь части: `Id`, тип (URI), цель и внешняя ли она.
#[derive(Debug, Default)]
pub(super) struct Rel {
    pub id: String,
    pub kind: String,
    pub target: String,
    pub external: bool,
}

pub(super) fn rels(data: &[u8]) -> Result<Vec<Rel>, String> {
    let mut out = Vec::new();
    walk(data, ns, |ev| {
        if let Ev::Start(_, b"Relationship", a) = ev {
            out.push(Rel {
                id: attr(a, b"Id").unwrap_or_default().into(),
                kind: attr(a, b"Type").unwrap_or_default().into(),
                target: attr(a, b"Target").unwrap_or_default().into(),
                external: attr(a, b"TargetMode") == Some("External"),
            });
        }
        ControlFlow::Continue(())
    })?;
    Ok(out)
}

/// Что берём из стиля абзаца: уровень заголовка и нумерацию.
#[derive(Debug, Default, Clone, PartialEq)]
struct Style {
    heading: Option<u8>,
    num: Option<(String, u8)>,
}

/// Стиль абзаца по `w:styleId`; уровень заголовка — по встроенному имени («heading 1», «Title» — одинаковы в любой
/// локализации Word, а `styleId` в русском Word — «1», «2»…) или уровню структуры, с наследованием `basedOn`.
fn styles(data: &[u8]) -> Result<HashMap<String, Style>, String> {
    #[derive(Default)]
    struct Raw {
        name: String,
        outline: Option<u8>,
        based_on: Option<String>,
        num_id: Option<String>,
        ilvl: u8,
    }
    let mut raw: HashMap<String, Raw> = HashMap::new();
    let mut cur: Option<(String, Raw)> = None;
    walk(data, ns, |ev| {
        match ev {
            Ev::Start(Ns::W, b"style", a) => {
                cur = (attr(a, b"type").is_none_or(|t| t == "paragraph"))
                    .then(|| attr(a, b"styleId"))
                    .flatten()
                    .map(|id| (id.to_string(), Raw::default()));
            }
            Ev::Start(Ns::W, name, a) => {
                if let Some((_, s)) = &mut cur {
                    let val = attr(a, b"val");
                    match name {
                        b"name" => s.name = val.unwrap_or_default().to_ascii_lowercase(),
                        b"basedOn" => s.based_on = val.map(str::to_string),
                        b"outlineLvl" => s.outline = val.and_then(|v| v.parse().ok()),
                        b"numId" => s.num_id = val.map(str::to_string),
                        b"ilvl" => s.ilvl = val.and_then(|v| v.parse().ok()).unwrap_or(0),
                        _ => {}
                    }
                }
            }
            Ev::End(Ns::W, b"style") => {
                if let Some((id, s)) = cur.take() {
                    raw.insert(id, s);
                }
            }
            _ => {}
        }
        ControlFlow::Continue(())
    })?;
    let own = |s: &Raw| heading_of(&s.name).or_else(|| s.outline.and_then(outline_level));
    let mut out = HashMap::new();
    for (id, s) in &raw {
        let mut style = Style { heading: own(s), num: s.num_id.clone().map(|n| (n, s.ilvl.min(8))) };
        let mut parent = s.based_on.as_deref();
        // Наследование — не глубже восьми стилей (и без циклов).
        for _ in 0..8 {
            let Some(p) = parent.and_then(|p| raw.get(p)) else { break };
            style.heading = style.heading.or_else(|| own(p));
            style.num = style.num.or_else(|| p.num_id.clone().map(|n| (n, p.ilvl.min(8))));
            parent = p.based_on.as_deref();
        }
        out.insert(id.clone(), style);
    }
    Ok(out)
}

/// Уровень заголовка по встроенному имени стиля.
fn heading_of(name: &str) -> Option<u8> {
    if name == "title" {
        return Some(1);
    }
    let n: u8 = name.strip_prefix("heading")?.trim().parse().ok()?;
    (1..=9).contains(&n).then_some(n.min(6))
}

/// `w:outlineLvl` (0 — первый уровень, 9 — основной текст) → уровень заголовка.
fn outline_level(v: u8) -> Option<u8> {
    (v < 9).then_some((v + 1).min(6))
}

/// Нумерация: `numId` → абстрактная нумерация → по уровням нумерованный ли список (иначе маркированный).
#[derive(Debug, Default)]
struct Numbering {
    abstracts: HashMap<String, [bool; 9]>,
    nums: HashMap<String, String>,
}

impl Numbering {
    fn ordered(&self, num: &str, level: u8) -> bool {
        self.nums.get(num).and_then(|a| self.abstracts.get(a)).is_some_and(|l| l[usize::from(level.min(8))])
    }
}

fn numbering(data: &[u8]) -> Result<Numbering, String> {
    let mut out = Numbering::default();
    let (mut abs, mut lvl, mut num): (Option<String>, Option<u8>, Option<String>) = (None, None, None);
    walk(data, ns, |ev| {
        match ev {
            Ev::Start(Ns::W, b"abstractNum", a) => abs = attr(a, b"abstractNumId").map(str::to_string),
            Ev::Start(Ns::W, b"lvl", a) => lvl = attr(a, b"ilvl").and_then(|v| v.parse().ok()).filter(|&l: &u8| l <= 8),
            Ev::Start(Ns::W, b"numFmt", a) => {
                if let (Some(abs), Some(lvl)) = (&abs, lvl) {
                    let ordered = attr(a, b"val").is_some_and(|v| v != "bullet" && v != "none");
                    out.abstracts.entry(abs.clone()).or_default()[usize::from(lvl)] = ordered;
                }
            }
            Ev::Start(Ns::W, b"num", a) => num = attr(a, b"numId").map(str::to_string),
            Ev::Start(Ns::W, b"abstractNumId", a) => {
                if let (Some(num), Some(v)) = (&num, attr(a, b"val")) {
                    out.nums.insert(num.clone(), v.to_string());
                }
            }
            Ev::End(Ns::W, b"abstractNum") => abs = None,
            Ev::End(Ns::W, b"lvl") => lvl = None,
            Ev::End(Ns::W, b"num") => num = None,
            _ => {}
        }
        ControlFlow::Continue(())
    })?;
    Ok(out)
}

// --- документ ---

/// Абзац в сборке.
#[derive(Default)]
struct Para {
    style: Option<String>,
    num_id: Option<String>,
    ilvl: Option<u8>,
    outline: Option<u8>,
    align: Align,
    inlines: Vec<Inline>,
}

/// Строки → ячейки → инлайны.
type Rows = Vec<Vec<Vec<Inline>>>;

struct Builder<'a> {
    styles: &'a HashMap<String, Style>,
    numbering: &'a Numbering,
    links: &'a HashMap<String, String>,
    limits: Limits,
    out: Vec<Block>,
    /// Пункты текущего списка: уровень, нумерованный ли, текст; `list_id` — его `numId`.
    list: Vec<(u8, bool, Vec<Inline>)>,
    list_id: String,
    /// Открытые таблицы (внешняя — первая).
    tables: Vec<Rows>,
    para: Option<Para>,
    /// Ссылка: адрес (если внешний) и её содержимое; `links_open` — вложенность `w:hyperlink`.
    link: Option<(Option<String>, Vec<Inline>)>,
    links_open: usize,
    marks: Marks,
    /// Пропускаемое поддерево (глубина внутри него).
    skip: usize,
    in_ppr: bool,
    in_rpr: bool,
    in_t: bool,
    /// Скрытый текст (`w:vanish`) в текущем фрагменте.
    hidden: bool,
    blocks: usize,
    chars: usize,
    truncated: bool,
}

impl<'a> Builder<'a> {
    fn new(styles: &'a HashMap<String, Style>, numbering: &'a Numbering, links: &'a HashMap<String, String>, limits: Limits) -> Self {
        Builder {
            styles,
            numbering,
            links,
            limits,
            out: Vec::new(),
            list: Vec::new(),
            list_id: String::new(),
            tables: Vec::new(),
            para: None,
            link: None,
            links_open: 0,
            marks: Marks::default(),
            skip: 0,
            in_ppr: false,
            in_rpr: false,
            in_t: false,
            hidden: false,
            blocks: 0,
            chars: 0,
            truncated: false,
        }
    }

    fn event(&mut self, ev: Ev<'_, Ns>) -> ControlFlow<()> {
        match ev {
            Ev::Start(..) if self.skip > 0 => {
                self.skip += 1;
                ControlFlow::Continue(())
            }
            Ev::End(..) if self.skip > 0 => {
                self.skip -= 1;
                ControlFlow::Continue(())
            }
            Ev::Start(ns, name, attrs) => self.start(ns, name, attrs),
            Ev::End(ns, name) => self.end(ns, name),
            Ev::Text(t) if self.skip == 0 && self.in_t => self.text(t, self.marks),
            Ev::Text(_) => ControlFlow::Continue(()),
        }
    }

    fn start(&mut self, ns: Ns, name: &[u8], attrs: &[(Vec<u8>, String)]) -> ControlFlow<()> {
        // Из `mc:AlternateContent` — только `Choice` (запасной вариант — та же картинка ещё раз).
        if ns == Ns::Mc && name == b"Fallback" {
            self.skip = 1;
        }
        if ns != Ns::W {
            return ControlFlow::Continue(());
        }
        let val = attr(attrs, b"val");
        let on = || val.is_none_or(|v| !matches!(v, "0" | "false" | "off" | "none"));
        match name {
            b"p" => {
                if let Some(p) = self.para.take() {
                    self.paragraph(p)?;
                }
                self.para = Some(Para::default());
            }
            b"pPr" => self.in_ppr = true,
            // `rPr` внутри `pPr` — знак абзаца, не текст.
            b"rPr" => self.in_rpr = !self.in_ppr,
            b"pStyle" | b"ilvl" | b"numId" | b"jc" | b"outlineLvl" if self.in_ppr => {
                if let (Some(p), Some(v)) = (&mut self.para, val) {
                    match name {
                        b"pStyle" => p.style = Some(v.to_string()),
                        b"ilvl" => p.ilvl = v.parse().ok().map(|l: u8| l.min(8)),
                        b"numId" => p.num_id = Some(v.to_string()),
                        b"jc" => {
                            p.align = match v {
                                "center" => Align::Center,
                                "right" | "end" => Align::End,
                                "both" | "distribute" => Align::Justify,
                                _ => Align::Start,
                            }
                        }
                        _ => p.outline = v.parse().ok().and_then(outline_level),
                    }
                }
            }
            b"r" => (self.marks, self.hidden) = (Marks::default(), false),
            b"b" if self.in_rpr => self.marks.bold = on(),
            b"i" if self.in_rpr => self.marks.italic = on(),
            b"u" if self.in_rpr => self.marks.underline = on(),
            b"strike" | b"dstrike" if self.in_rpr => self.marks.strike = on(),
            b"t" => self.in_t = true,
            // `w:tab` в `pPr` — позиция табуляции, не знак.
            // Скрытый текст Word не показывает — и мы.
            b"vanish" if self.in_rpr => self.hidden = on(),
            b"tab" if !self.in_ppr => return self.text("\t", self.marks),
            b"br" | b"cr" if !self.in_ppr && !self.hidden => {
                self.inline(Inline::LineBreak);
                return self.spend();
            }
            b"noBreakHyphen" => return self.text("-", self.marks),
            b"drawing" | b"pict" => {
                self.skip = 1;
                return self.placeholder("[рисунок]");
            }
            b"object" => {
                self.skip = 1;
                return self.placeholder("[объект]");
            }
            b"footnoteReference" | b"endnoteReference" => return self.placeholder("[сноска]"),
            b"commentReference" => return self.placeholder("[примечание]"),
            // Удалённое правкой, старые свойства правки, свойства раздела и элемента управления, код поля.
            b"del" | b"moveFrom" | b"delText" | b"instrText" | b"rPrChange" | b"pPrChange" | b"sectPr" | b"sdtPr" => self.skip = 1,
            b"hyperlink" => {
                self.links_open += 1;
                if self.links_open == 1 {
                    let href = attr(attrs, b"r:id").and_then(|id| self.links.get(id)).cloned();
                    self.link = Some((href, Vec::new()));
                }
            }
            b"tbl" => {
                if self.tables.is_empty() {
                    self.flush_list();
                }
                self.tables.push(Vec::new());
            }
            b"tr" => {
                if let Some(t) = self.tables.last_mut() {
                    t.push(Vec::new());
                    // Строка таблицы — тоже блок для предела.
                    if self.tables.len() == 1 {
                        return self.count();
                    }
                }
            }
            b"tc" => {
                if let Some(row) = self.tables.last_mut().and_then(|t| t.last_mut()) {
                    row.push(Vec::new());
                    return self.spend();
                }
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }

    fn end(&mut self, ns: Ns, name: &[u8]) -> ControlFlow<()> {
        if ns != Ns::W {
            return ControlFlow::Continue(());
        }
        match name {
            b"pPr" => self.in_ppr = false,
            b"rPr" => self.in_rpr = false,
            b"t" => self.in_t = false,
            b"p" => {
                if let Some(p) = self.para.take() {
                    return self.paragraph(p);
                }
            }
            b"hyperlink" => {
                self.links_open = self.links_open.saturating_sub(1);
                if self.links_open == 0 {
                    self.close_link();
                }
            }
            b"tbl" => {
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
        if self.hidden {
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
        let cut = taken > left;
        self.chars += clean.chars().count();
        if !clean.is_empty() {
            self.inline(Inline::Text { text: clean, marks });
        }
        if cut {
            self.truncated = true;
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    }

    fn placeholder(&mut self, s: &str) -> ControlFlow<()> {
        self.text(s, Marks { italic: true, ..Marks::default() })
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

    /// Инлайн — в ссылку, если она открыта, иначе в абзац; соседний текст с теми же марками сливается.
    fn inline(&mut self, i: Inline) {
        let target = match (&mut self.link, &mut self.para) {
            (Some((_, content)), _) => content,
            (None, Some(p)) => &mut p.inlines,
            (None, None) => return,
        };
        push(target, i);
    }

    fn close_link(&mut self) {
        let Some((href, content)) = self.link.take() else { return };
        let checked = href.and_then(|h| check_link(&h, &plain_text(&content)));
        match checked {
            Some(l) if !content.is_empty() => self.inline(Inline::Link { href: l.href, host: l.host, suspicious: l.suspicious, content }),
            // Запрещённая схема, закладка в документе или пустая ссылка: остаётся только текст.
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
                    push(cell, i);
                }
            }
            return ControlFlow::Continue(());
        }
        if is_blank(&content) {
            return ControlFlow::Continue(());
        }
        let style = p
            .style
            .as_deref()
            .map(|s| self.styles.get(s).cloned().unwrap_or_else(|| Style { heading: heading_of(&s.to_ascii_lowercase()), num: None }));
        if let Some(level) = p.outline.or_else(|| style.as_ref().and_then(|s| s.heading)) {
            self.flush_list();
            return self.emit(Block::Heading { level, content });
        }
        let num = match (p.num_id, style.and_then(|s| s.num)) {
            (Some(id), _) => Some((id, p.ilvl.unwrap_or(0))),
            (None, Some((id, l))) => Some((id, p.ilvl.unwrap_or(l))),
            (None, None) => None,
        };
        if let Some((id, level)) = num.filter(|(id, _)| id != "0") {
            if self.list_id != id {
                self.flush_list();
                self.list_id = id;
            }
            let ordered = self.numbering.ordered(&self.list_id, level);
            self.list.push((level, ordered, content));
            return self.count();
        }
        self.flush_list();
        self.emit(Block::Paragraph { align: p.align, content })
    }

    fn table(&mut self, rows: Rows) -> ControlFlow<()> {
        if self.tables.is_empty() {
            if rows.iter().flatten().all(|c| c.is_empty()) {
                return ControlFlow::Continue(());
            }
            self.out.push(Block::Table(rows));
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
                    push(&mut flat, Inline::text(" | "));
                }
                for i in cell {
                    push(&mut flat, i);
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
        if !self.list.is_empty() {
            let items = std::mem::take(&mut self.list);
            self.out.push(list(items));
        }
        self.list_id.clear();
    }

    /// Конец разбора (в том числе по пределу): недособранное — как есть.
    fn finish(mut self) -> Vec<Block> {
        self.links_open = 0;
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

pub(super) fn push(target: &mut Vec<Inline>, i: Inline) {
    if let (Some(Inline::Text { text, marks }), Inline::Text { text: t, marks: m }) = (target.last_mut(), &i)
        && marks == m
    {
        text.push_str(t);
        return;
    }
    target.push(i);
}

pub(super) fn is_blank(content: &[Inline]) -> bool {
    content.iter().all(|i| match i {
        Inline::Text { text, .. } => text.trim().is_empty(),
        Inline::LineBreak => true,
        _ => false,
    })
}

/// Пункты подряд → список; пункты глубже первого уровня — вложенным списком в предыдущем пункте.
pub(super) fn list(items: Vec<(u8, bool, Vec<Inline>)>) -> Block {
    let base = items.iter().map(|i| i.0).min().unwrap_or(0);
    let ordered = items.first().is_some_and(|i| i.1);
    let mut out: Vec<Vec<Block>> = Vec::new();
    let mut deeper: Vec<(u8, bool, Vec<Inline>)> = Vec::new();
    for item in items {
        if item.0 > base && !out.is_empty() {
            deeper.push(item);
            continue;
        }
        if !deeper.is_empty()
            && let Some(last) = out.last_mut()
        {
            last.push(list(std::mem::take(&mut deeper)));
        }
        out.push(vec![Block::Paragraph { align: Align::Start, content: item.2 }]);
    }
    if !deeper.is_empty()
        && let Some(last) = out.last_mut()
    {
        last.push(list(deeper));
    }
    Block::List { ordered, items: out }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use crate::preview::{OfficeScan, Preview, PreviewKind, Section, preview};

    const NS: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006""#;
    const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

    fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in files {
            w.start_file(*name, opts).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    /// `.docx` из тела документа и служебных частей (стили, нумерация, связи документа).
    fn docx_of(body: &str, styles: &str, numbering: &str, rels: &str) -> Vec<u8> {
        let doc = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document {NS}><w:body>{body}<w:sectPr><w:pgSz w:w="11906"/></w:sectPr></w:body></w:document>"#
        );
        let styles = format!(r#"<w:styles {NS}>{styles}</w:styles>"#);
        let numbering = format!(r#"<w:numbering {NS}>{numbering}</w:numbering>"#);
        let rels = format!(r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{rels}</Relationships>"#);
        let root = format!(
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL}/officeDocument" Target="word/document.xml"/></Relationships>"#
        );
        zip_of(&[
            ("[Content_Types].xml", b"<Types/>"),
            ("_rels/.rels", root.as_bytes()),
            ("word/document.xml", doc.as_bytes()),
            ("word/styles.xml", styles.as_bytes()),
            ("word/numbering.xml", numbering.as_bytes()),
            ("word/_rels/document.xml.rels", rels.as_bytes()),
        ])
    }

    fn blocks(body: &str) -> Vec<Block> {
        docx(&docx_of(body, "", "", "")).unwrap().0
    }

    fn t(s: &str) -> Inline {
        Inline::text(s)
    }

    fn p(content: Vec<Inline>) -> Block {
        Block::Paragraph { align: Align::Start, content }
    }

    fn run(text: &str) -> String {
        format!(r#"<w:r><w:t xml:space="preserve">{text}</w:t></w:r>"#)
    }

    #[test]
    fn headings_by_builtin_name_and_paragraph_with_marks() {
        // Русский Word: `styleId` «1», имя — встроенное «heading 1»; «Заголовок» без styles.xml — по `styleId`.
        let styles = r#"<w:style w:type="paragraph" w:styleId="1"><w:name w:val="heading 1"/></w:style>
            <w:style w:type="paragraph" w:styleId="my"><w:name w:val="Мой"/><w:basedOn w:val="1"/></w:style>"#;
        let body = format!(
            r#"<w:p><w:pPr><w:pStyle w:val="Title"/></w:pPr>{}</w:p>
            <w:p><w:pPr><w:pStyle w:val="1"/><w:rPr><w:b/></w:rPr></w:pPr>{}</w:p>
            <w:p><w:pPr><w:pStyle w:val="my"/></w:pPr>{}</w:p>
            <w:p><w:pPr><w:outlineLvl w:val="2"/></w:pPr>{}</w:p>
            <w:p><w:pPr><w:jc w:val="center"/></w:pPr>
              <w:r><w:t>Обычный, </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>жир</w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>ный</w:t></w:r>
              <w:r><w:rPr><w:i/><w:u w:val="single"/></w:rPr><w:t xml:space="preserve"> курсив</w:t></w:r>
              <w:r><w:rPr><w:b w:val="0"/><w:u w:val="none"/></w:rPr><w:t>.</w:t></w:r></w:p>"#,
            run("Отчёт"),
            run("Введение"),
            run("Свой"),
            run("Третий")
        );
        let b = docx(&docx_of(&body, styles, "", "")).unwrap().0;
        let bold = Marks { bold: true, ..Marks::default() };
        let iu = Marks { italic: true, underline: true, ..Marks::default() };
        assert_eq!(
            b,
            [
                Block::Heading { level: 1, content: vec![t("Отчёт")] },
                Block::Heading { level: 1, content: vec![t("Введение")] },
                Block::Heading { level: 1, content: vec![t("Свой")] },
                Block::Heading { level: 3, content: vec![t("Третий")] },
                Block::Paragraph {
                    align: Align::Center,
                    content: vec![
                        t("Обычный, "),
                        Inline::Text { text: "жирный".into(), marks: bold },
                        Inline::Text { text: " курсив".into(), marks: iu },
                        t("."),
                    ]
                },
            ]
        );
    }

    #[test]
    fn lists_are_numbered_or_bulleted_and_nested() {
        let numbering = r#"<w:abstractNum w:abstractNumId="7"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/></w:lvl>
            <w:lvl w:ilvl="1"><w:numFmt w:val="bullet"/></w:lvl></w:abstractNum>
            <w:abstractNum w:abstractNumId="8"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/></w:lvl></w:abstractNum>
            <w:num w:numId="1"><w:abstractNumId w:val="7"/></w:num><w:num w:numId="2"><w:abstractNumId w:val="8"/></w:num>"#;
        let item = |num: u8, lvl: u8, text: &str| {
            format!(r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="{lvl}"/><w:numId w:val="{num}"/></w:numPr></w:pPr>{}</w:p>"#, run(text))
        };
        let body = [item(1, 0, "Один"), item(1, 1, "вложенный"), item(1, 0, "Два"), item(2, 0, "Маркер"), run_p("После")].concat();
        let b = docx(&docx_of(&body, "", numbering, "")).unwrap().0;
        assert_eq!(
            b,
            [
                Block::List {
                    ordered: true,
                    items: vec![
                        vec![p(vec![t("Один")]), Block::List { ordered: false, items: vec![vec![p(vec![t("вложенный")])]] }],
                        vec![p(vec![t("Два")])],
                    ]
                },
                Block::List { ordered: false, items: vec![vec![p(vec![t("Маркер")])]] },
                p(vec![t("После")]),
            ]
        );
    }

    fn run_p(text: &str) -> String {
        format!("<w:p>{}</w:p>", run(text))
    }

    #[test]
    fn tables_are_text_by_cells_and_nested_tables_are_flattened() {
        let cell = |inner: &str| format!("<w:tc><w:tcPr/>{inner}</w:tc>");
        let inner = format!("<w:tbl><w:tr>{}{}</w:tr></w:tbl>", cell(&run_p("x")), cell(&run_p("y")));
        let body = format!(
            "<w:tbl><w:tblPr/><w:tr>{}{}</w:tr><w:tr>{}{}</w:tr></w:tbl>",
            cell(&run_p("Имя")),
            cell(&run_p("Срок")),
            cell(&format!("{}{}", run_p("Смета"), run_p("v2"))),
            cell(&inner)
        );
        assert_eq!(
            blocks(&body),
            [Block::Table(vec![vec![vec![t("Имя")], vec![t("Срок")]], vec![vec![t("Смета"), Inline::LineBreak, t("v2")], vec![t("x | y")]],])]
        );
    }

    #[test]
    fn links_go_through_the_ir_check() {
        let rels = format!(
            r#"<Relationship Id="rId5" Type="{REL}/hyperlink" Target="https://example.com/a?b=1" TargetMode="External"/>
            <Relationship Id="rId6" Type="{REL}/hyperlink" Target="javascript:alert(1)" TargetMode="External"/>"#
        );
        let body = format!(
            r#"<w:p><w:hyperlink r:id="rId5">{}</w:hyperlink><w:hyperlink r:id="rId6">{}</w:hyperlink><w:hyperlink w:anchor="_Toc1">{}</w:hyperlink></w:p>"#,
            run("сайт"),
            run(" скрипт"),
            run(" закладка")
        );
        let b = docx(&docx_of(&body, "", "", &rels)).unwrap().0;
        let link =
            Inline::Link {
                href: "https://example.com/a?b=1".into(), host: Some("example.com".into()), suspicious: false, content: vec![t("сайт")]
            };
        assert_eq!(b, [p(vec![link, t(" скрипт закладка")])]);
    }

    #[test]
    fn breaks_placeholders_and_hidden_parts() {
        let body = format!(
            r#"<w:p><w:r><w:t>а</w:t><w:br/><w:t>б</w:t><w:tab/><w:t>в</w:t></w:r>
            <w:r><mc:AlternateContent><mc:Choice Requires="wps"><w:drawing><w:t>внутри</w:t></w:drawing></mc:Choice>
              <mc:Fallback><w:pict/></mc:Fallback></mc:AlternateContent></w:r>
            <w:r><w:footnoteReference w:id="1"/></w:r><w:r><w:rPr><w:vanish/></w:rPr><w:t>скрыто</w:t><w:br/></w:r><w:del><w:r><w:delText>удалено</w:delText></w:r></w:del>
            <w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText>PAGE</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r>{}<w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>
            <w:p/><w:p><w:r><w:t xml:space="preserve">   </w:t></w:r></w:p>"#,
            run("7")
        );
        let it = Marks { italic: true, ..Marks::default() };
        assert_eq!(
            blocks(&body),
            [p(vec![t("а"), Inline::LineBreak, t("б\tв"), Inline::Text { text: "[рисунок][сноска]".into(), marks: it }, t("7"),])]
        );
    }

    /// `.docm` и документ с `vbaProject.bin` — проверка Office с макросами, не документ.
    #[test]
    fn macro_documents_are_office_scans() {
        let doc = docx_of(&run_p("x"), "", "", "");
        let (pv, _) = preview(PreviewKind::Document, &doc, 0, 0, 0).unwrap();
        assert_eq!(pv, Preview::Document { sections: vec![Section { title: None, blocks: vec![p(vec![t("x")])] }], truncated: false });
        let docm = zip_of(&[("[Content_Types].xml", b"<Types/>"), ("word/document.xml", b"<w:document/>"), ("word/vbaProject.bin", b"x")]);
        let (pv, _) = preview(PreviewKind::Document, &docm, 0, 0, 0).unwrap();
        assert!(matches!(pv, Preview::Office(OfficeScan { macros: true, .. })), "{pv:?}");
    }

    #[test]
    fn dtd_entities_and_broken_xml_are_errors() {
        let dtd = br#"<?xml version="1.0"?><!DOCTYPE d [<!ENTITY x SYSTEM "file:///c:/windows/win.ini">]><w:document>&x;</w:document>"#;
        assert!(docx(&zip_of(&[("word/document.xml", dtd)])).is_err());
        let entity = format!("<w:document {NS}><w:body><w:p><w:r><w:t>&x;</w:t></w:r></w:p></w:body></w:document>");
        assert!(docx(&zip_of(&[("word/document.xml", entity.as_bytes())])).is_err());
        let broken = format!("<w:document {NS}><w:body><w:p><w:r><w:t>a</w:p></w:body>");
        assert!(docx(&zip_of(&[("word/document.xml", broken.as_bytes())])).is_err());
        let deep = format!("<w:document {NS}>{}", "<w:sdt>".repeat(MAX_DEPTH + 1));
        assert!(docx(&zip_of(&[("word/document.xml", deep.as_bytes())])).is_err());
        // DTD в служебной части — часть не учитывается, документ показывается.
        let doc = docx_of(&run_p("x"), "", "", "");
        let mut z = zip::ZipArchive::new(io::Cursor::new(doc.as_slice())).unwrap();
        assert!(part(&mut z, "word/styles.xml").is_some());
        assert!(styles(dtd).is_err());
        // Не ZIP, пустой ZIP, ZIP без документа.
        assert!(docx(b"plain").is_err());
        assert!(docx(b"PK\x03\x04 garbage").is_err());
        assert!(docx(&zip_of(&[("a.txt", b"x")])).is_err());
        // Символьные ссылки и встроенные сущности — можно.
        let refs = format!("<w:document {NS}><w:body><w:p><w:r><w:t>&lt;&#1071;&#x44F;&amp;</w:t></w:r></w:p></w:body></w:document>");
        assert_eq!(docx(&zip_of(&[("word/document.xml", refs.as_bytes())])).unwrap().0, [p(vec![t("<Яя&")])]);
    }

    /// Zip-бомба: `document.xml` в сотни раз больше архива — читается только до предела, итог `truncated`.
    #[test]
    fn zip_bomb_stops_at_the_xml_limit() {
        let mut body = run_p("начало");
        body.push_str(&" ".repeat(4 << 20));
        body.push_str(&run_p("конец"));
        let doc = docx_of(&body, "", "", "");
        assert!(doc.len() < 64 * 1024, "{}", doc.len());
        let started = std::time::Instant::now();
        let (b, cut) = parse(&doc, Limits { xml: 1 << 20, ..LIMITS }).unwrap();
        assert!(started.elapsed() < crate::TIME_LIMIT / 4);
        assert!(cut);
        assert_eq!(b, [p(vec![t("начало")])]);
    }

    #[test]
    fn huge_documents_are_cut_by_blocks_and_chars() {
        let body = run_p("абзац").repeat(50);
        let (b, cut) = parse(&docx_of(&body, "", "", ""), Limits { blocks: 10, ..LIMITS }).unwrap();
        assert!(cut);
        assert_eq!(b.len(), 10);
        let (b, cut) = parse(&docx_of(&body, "", "", ""), Limits { chars: 12, ..LIMITS }).unwrap();
        assert!(cut);
        assert_eq!(b, [p(vec![t("абзац")]), p(vec![t("абзац")]), p(vec![t("аб")])]);
        // Целиком документ в пределах — не обрезан.
        let (b, cut) = docx(&docx_of(&body, "", "", "")).unwrap();
        assert!(!cut && b.len() == 50);
    }
}
