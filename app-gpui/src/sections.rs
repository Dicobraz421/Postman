//! Разделы просмотра вложения (S8-23, макет S8-D «Просмотр»): книга — листами во вкладках, таблица — с закреплённой
//! первой строкой и прокруткой вбок; презентация — по слайду, листание ‹ › и ← / →, как страницы PDF; письмо во
//! вложении — шапкой (от, кому, копия, дата), своими вложениями и текстом.
//!
//! Воркер отдаёт данные (`SectionTitle`, `Block::Figure`), подписи — здесь, через Fluent: «Лист «…»», «Слайд N»,
//! «[рисунок]». Заметки докладчика воркер кладёт последней цитатой слайда.

use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::Metric;
use postman_ir::{Block, Inline};
use postman_worker::{Section, SectionTitle};

use crate::reading::ReadingPane;
use crate::roles::Look;

/// Как показать разделы.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Layout {
    /// Книга (xlsx, ods) и таблица (csv, tsv): лист — вкладкой, таблица — сеткой.
    Book,
    /// Презентация: по слайду.
    Slides,
    /// Документ (docx, odt, rtf, md, модуль): разделы подряд.
    Doc,
}

impl Layout {
    pub(crate) fn of(v: postman_core::attach::Viewer) -> Layout {
        use postman_core::attach::Viewer;
        match v {
            Viewer::Sheet | Viewer::Table => Layout::Book,
            Viewer::Slides => Layout::Slides,
            _ => Layout::Doc,
        }
    }
}

/// Письмо во вложении (S8-20): шапка — данными.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Letter {
    pub(crate) from: Vec<postman_worker::mime::Addr>,
    pub(crate) to: Vec<postman_worker::mime::Addr>,
    pub(crate) cc: Vec<postman_worker::mime::Addr>,
    pub(crate) date: Option<i64>,
    pub(crate) subject: String,
    pub(crate) blocks: Vec<Block>,
    pub(crate) attachments: Vec<(String, String, u64)>,
    pub(crate) truncated: bool,
}

/// Подпись раздела: «Лист «Смета»», «Слайд 3»; без заголовка — пусто.
pub(crate) fn title(t: &SectionTitle) -> String {
    match t {
        SectionTitle::None => String::new(),
        SectionTitle::Sheet(name) => tr!("att-pv-sheet", name = name.as_str()),
        SectionTitle::Slide(n) => tr!("att-pv-slide", n = i64::from(*n)),
    }
}

/// Имя вкладки листа: имя листа; у раздела без имени — «Лист N».
pub(crate) fn tab_name(s: &Section, i: usize) -> String {
    match &s.title {
        SectionTitle::Sheet(name) => name.clone(),
        _ => tr!("att-pv-slide", n = (i + 1) as i64),
    }
}

/// Текст ячейки или абзаца — строкой.
pub(crate) fn plain(content: &[Inline]) -> String {
    let mut out = String::new();
    fn walk(i: &Inline, out: &mut String) {
        match i {
            Inline::Text { text, .. } => out.push_str(text),
            Inline::Link { content, .. } => content.iter().for_each(|c| walk(c, out)),
            Inline::LineBreak => out.push(' '),
            _ => {}
        }
    }
    content.iter().for_each(|c| walk(c, &mut out));
    out
}

/// Заголовок слайда: первый заголовок (или первый абзац) — «Слайд 3 · Этапы».
pub(crate) fn slide_caption(s: &Section) -> String {
    let head = s.blocks.iter().find_map(|b| match b {
        Block::Heading { content, .. } | Block::Paragraph { content, .. } => Some(plain(content)),
        _ => None,
    });
    match head.filter(|h| !h.trim().is_empty()) {
        Some(h) => format!("{} · {}", title(&s.title), h.trim()),
        None => title(&s.title),
    }
}

/// Слайд: блоки и заметки докладчика (последняя цитата).
pub(crate) fn split_notes(blocks: &[Block]) -> (&[Block], Option<&[Block]>) {
    match blocks.split_last() {
        Some((Block::Quote(notes), rest)) => (rest, Some(notes.as_slice())),
        _ => (blocks, None),
    }
}

/// Ширина столбца сетки: по самой длинной ячейке (первые 200 строк), 64–320 px.
fn column_widths(rows: &[Vec<Vec<Inline>>]) -> Vec<f32> {
    let n = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut w = vec![0usize; n];
    for row in rows.iter().take(200) {
        for (c, cell) in row.iter().enumerate() {
            w[c] = w[c].max(plain(cell).chars().count());
        }
    }
    w.into_iter().map(|chars| (chars as f32 * 7.5 + 20.).clamp(64., 320.)).collect()
}

/// Сетка листа: первая строка закреплена (над прокруткой вниз), шире панели — прокрутка вбок вместе с ней.
pub(crate) fn grid(rows: &[Vec<Vec<Inline>>], key: &str, l: &Look) -> AnyElement {
    let widths = column_widths(rows);
    let total: f32 = widths.iter().sum();
    let row_el = |r: usize, row: &[Vec<Inline>], head: bool| {
        div()
            .flex()
            .flex_none()
            .w(px(total))
            .h(px(24.))
            .border_b_1()
            .border_color(l.border())
            .when(head, |d| d.bg(l.fill()).font_weight(FontWeight::SEMIBOLD))
            .children(widths.iter().enumerate().map(|(c, w)| {
                let text = row.get(c).map(|cell| plain(cell)).unwrap_or_default();
                // Числа — вправо, как в книге.
                let number = !head && !text.is_empty() && text.chars().all(|ch| ch.is_ascii_digit() || " .,-−+%".contains(ch));
                div()
                    .id((SharedString::from(format!("{key}-{r}")), c))
                    .w(px(*w))
                    .flex_none()
                    .h_full()
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .when(number, |d| d.justify_end())
                    .border_r_1()
                    .border_color(l.border())
                    .overflow_hidden()
                    .child(div().truncate().child(text))
            }))
    };
    let head = rows.first().map(|r| row_el(0, r, true));
    let body = div()
        .id(SharedString::from(format!("{key}-rows")))
        .flex_1()
        .min_h_0()
        .w(px(total))
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .children(rows.iter().enumerate().skip(1).map(|(r, row)| row_el(r, row, false)));
    div()
        .id(SharedString::from(format!("{key}-grid")))
        .flex_1()
        .min_h_0()
        .overflow_x_scroll()
        .text_size(l.px(Metric::TypeCaption))
        .child(div().h_full().flex().flex_col().children(head).child(body))
        .into_any_element()
}

/// Строк в сетке листа.
pub(crate) fn rows_of(s: &Section) -> usize {
    s.blocks.iter().map(|b| if let Block::Table(rows) = b { rows.len() } else { 0 }).sum()
}

/// Адреса строкой: «Имя <адрес>; …».
fn addrs(list: &[postman_worker::mime::Addr]) -> String {
    list.iter()
        .map(|a| {
            if a.name.is_empty() {
                a.email.clone()
            } else if a.email.is_empty() {
                a.name.clone()
            } else {
                format!("{} <{}>", a.name, a.email)
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

impl ReadingPane {
    /// Письмо во вложении: тема, поля шапки, вложения карточками, текст.
    pub(crate) fn letter_view(&self, m: &Letter, key: &str, l: &Look, cx: &mut Context<Self>) -> AnyElement {
        let field = |label: String, value: String| {
            div()
                .flex()
                .gap(px(12.))
                .child(div().w(px(56.)).flex_none().text_color(l.text2()).child(label))
                .child(div().flex_1().min_w_0().child(value))
        };
        let date = m.date.and_then(|t| jiff::Timestamp::from_second(t).ok()).map(|t| crate::format_date(t.as_second()));
        let head = div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .text_size(l.px(Metric::TypeCaption))
            .when(!m.from.is_empty(), |d| d.child(field(tr!("att-pv-letter-from"), addrs(&m.from))))
            .when(!m.to.is_empty(), |d| d.child(field(tr!("att-pv-letter-to"), addrs(&m.to))))
            .when(!m.cc.is_empty(), |d| d.child(field(tr!("att-pv-letter-cc"), addrs(&m.cc))))
            .children(date.map(|d| field(tr!("att-pv-letter-date"), d)));
        let files = div().flex().flex_wrap().gap(px(8.)).children(m.attachments.iter().map(|(name, mime, size)| {
            let g = if mime.starts_with("message/") || name.to_ascii_lowercase().ends_with(".eml") {
                crate::chrome::glyph::MAIL
            } else {
                crate::chrome::glyph::ATTACH
            };
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .h(px(36.))
                .px(px(12.))
                .rounded(px(6.))
                .border_1()
                .border_color(l.stroke())
                .child(crate::chrome::icon(g, 14.).text_color(l.text2()))
                .child(div().max_w(px(220.)).truncate().child(name.clone()))
                .child(div().text_size(l.px(Metric::TypeCaption)).text_color(l.text3()).child(crate::attachments::size_text(*size)))
        }));
        let body = crate::ir_view::blocks(&m.blocks, key, &crate::ir_view::Images::new(), self.column.get(), cx);
        div()
            .id("pv-letter")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p(px(16.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(div().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).child(m.subject.clone()))
            .child(head)
            .when(!m.attachments.is_empty(), |d| d.child(files))
            .child(div().h(px(1.)).bg(l.border()))
            .when(m.truncated, |d| d.child(div().text_size(l.px(Metric::TypeCaption)).text_color(l.text2()).child(tr!("att-pv-doc-cut"))))
            .children(body)
            .into_any_element()
    }
}

/// Образцы файлов для снимков (книга, презентация, таблица, письмо, RTF) — собираются сценой и идут через воркер, как
/// вложение. Данные образцов — файлами в `src/samples/` (текст образцов — не строки интерфейса); здесь — упаковка.
pub(crate) mod samples {

    use std::io::Write as _;

    fn zip_of(files: &[(&str, &[u8])]) -> zip::result::ZipResult<Vec<u8>> {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in files {
            w.start_file(*name, opts)?;
            w.write_all(data)?;
        }
        Ok(w.finish()?.into_inner())
    }

    const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    const PKG: &str = "http://schemas.openxmlformats.org/package/2006/relationships";

    fn rels(links: &str) -> String {
        format!(r#"<Relationships xmlns="{PKG}">{links}</Relationships>"#)
    }

    /// Разделы текста `# Заголовок` и строки под ними.
    fn parts(text: &str) -> Vec<(&str, Vec<&str>)> {
        let mut out: Vec<(&str, Vec<&str>)> = Vec::new();
        for line in text.lines() {
            match (line.strip_prefix("# "), out.last_mut()) {
                (Some(title), _) => out.push((title.trim(), Vec::new())),
                (None, Some((_, lines))) if !line.trim().is_empty() => lines.push(line),
                _ => {}
            }
        }
        out
    }

    /// Смета (макет S8-D, `samples/book.txt`): листы — разделами, ячейки — через табуляцию.
    pub(crate) fn xlsx() -> zip::result::ZipResult<Vec<u8>> {
        const NS: &str = r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;
        let sheets: Vec<(String, String)> = parts(include_str!("samples/book.txt"))
            .into_iter()
            .map(|(name, lines)| {
                let rows: String = lines
                    .iter()
                    .enumerate()
                    .map(|(r, line)| {
                        let cells: String = line
                            .split('\t')
                            .enumerate()
                            .filter(|(_, s)| !s.is_empty())
                            .map(|(c, s)| format!(r#"<c r="{}{}" t="inlineStr"><is><t>{s}</t></is></c>"#, (b'A' + c as u8) as char, r + 1))
                            .collect();
                        format!(r#"<row r="{}">{cells}</row>"#, r + 1)
                    })
                    .collect();
                (name.to_owned(), format!(r#"<?xml version="1.0" encoding="UTF-8"?><worksheet {NS}><sheetData>{rows}</sheetData></worksheet>"#))
            })
            .collect();
        let list: String =
            sheets.iter().enumerate().map(|(i, (name, _))| format!(r#"<sheet name="{name}" sheetId="{}" r:id="rId{i}"/>"#, i + 1)).collect();
        let links: String = (0..sheets.len())
            .map(|i| format!(r#"<Relationship Id="rId{i}" Type="{REL}/worksheet" Target="worksheets/sheet{}.xml"/>"#, i + 1))
            .collect();
        let workbook = format!(r#"<workbook {NS}><sheets>{list}</sheets></workbook>"#);
        let root = rels(&format!(r#"<Relationship Id="rId1" Type="{REL}/officeDocument" Target="xl/workbook.xml"/>"#));
        let links = rels(&links);
        let names: Vec<String> = (0..sheets.len()).map(|i| format!("xl/worksheets/sheet{}.xml", i + 1)).collect();
        let mut files: Vec<(&str, &[u8])> = vec![
            ("[Content_Types].xml", b"<Types/>"),
            ("_rels/.rels", root.as_bytes()),
            ("xl/workbook.xml", workbook.as_bytes()),
            ("xl/_rels/workbook.xml.rels", links.as_bytes()),
        ];
        for (name, (_, xml)) in names.iter().zip(&sheets) {
            files.push((name, xml.as_bytes()));
        }
        zip_of(&files)
    }

    /// «Запуск проекта» (макет S8-D, `samples/deck.txt`): слайд — заголовком `#`, пункты `-`, рисунок `[pic]`,
    /// заметки `>`.
    pub(crate) fn pptx() -> zip::result::ZipResult<Vec<u8>> {
        const NS: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#;
        let ap = |t: &str| format!(r#"<a:p><a:r><a:rPr lang="ru-RU"/><a:t>{t}</a:t></a:r></a:p>"#);
        let sp = |ph: &str, paras: String| {
            format!(
                r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="x"/><p:cNvSpPr/><p:nvPr>{ph}</p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:lstStyle><a:lvl1pPr><a:buChar char="•"/></a:lvl1pPr></a:lstStyle>{paras}</p:txBody></p:sp>"#
            )
        };
        let mut files: Vec<(String, Vec<u8>)> = vec![
            ("[Content_Types].xml".into(), b"<Types/>".to_vec()),
            (
                "_rels/.rels".into(),
                rels(&format!(r#"<Relationship Id="rId1" Type="{REL}/officeDocument" Target="ppt/presentation.xml"/>"#)).into_bytes(),
            ),
        ];
        let mut links = String::new();
        let mut ids = String::new();
        for (i, (title, lines)) in parts(include_str!("samples/deck.txt")).into_iter().enumerate() {
            let n = i + 1;
            let mut tree = sp(r#"<p:ph type="title"/>"#, ap(title));
            let items: String = lines.iter().filter_map(|l| l.strip_prefix("- ")).map(ap).collect();
            if !items.is_empty() {
                tree.push_str(&sp(r#"<p:ph idx="1"/>"#, items));
            }
            if lines.contains(&"[pic]") {
                tree.push_str(r#"<p:pic><p:nvPicPr><p:cNvPr id="5" name="pic"/></p:nvPicPr><p:blipFill/></p:pic>"#);
            }
            let notes: String = lines.iter().filter_map(|l| l.strip_prefix("> ")).map(ap).collect();
            if !notes.is_empty() {
                let notes = sp(r#"<p:ph type="body"/>"#, notes);
                let link = format!(r#"<Relationship Id="rId9" Type="{REL}/notesSlide" Target="../notesSlides/notesSlide{n}.xml"/>"#);
                files.push((format!("ppt/slides/_rels/slide{n}.xml.rels"), rels(&link).into_bytes()));
                files.push((
                    format!("ppt/notesSlides/notesSlide{n}.xml"),
                    format!(r#"<p:notes {NS}><p:cSld><p:spTree>{notes}</p:spTree></p:cSld></p:notes>"#).into_bytes(),
                ));
            }
            let xml = format!(r#"<?xml version="1.0" encoding="UTF-8"?><p:sld {NS}><p:cSld><p:spTree>{tree}</p:spTree></p:cSld></p:sld>"#);
            files.push((format!("ppt/slides/slide{n}.xml"), xml.into_bytes()));
            links.push_str(&format!(r#"<Relationship Id="rId{n}" Type="{REL}/slide" Target="slides/slide{n}.xml"/>"#));
            ids.push_str(&format!(r#"<p:sldId id="{}" r:id="rId{n}"/>"#, 256 + i));
        }
        let presentation = format!(r#"<p:presentation {NS}><p:sldIdLst>{ids}</p:sldIdLst></p:presentation>"#);
        files.push(("ppt/presentation.xml".into(), presentation.into_bytes()));
        files.push(("ppt/_rels/presentation.xml.rels".into(), rels(&links).into_bytes()));
        let all: Vec<(&str, &[u8])> = files.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect();
        zip_of(&all)
    }

    /// Таблица CSV (разделитель «;», как у Excel с русской локалью).
    pub(crate) fn csv() -> Vec<u8> {
        include_bytes!("samples/invoices.csv").to_vec()
    }

    /// Письмо во вложении: шапка (UTF-8, RFC 6532), текст и два своих вложения; строки — CRLF.
    pub(crate) fn eml() -> Vec<u8> {
        include_str!("samples/letter.eml").replace("\r\n", "\n").replace('\n', "\r\n").into_bytes()
    }

    /// RTF: заголовок и абзацы `samples/protocol.txt` (кириллица — `\uN?`).
    pub(crate) fn rtf() -> Vec<u8> {
        let u = |s: &str| s.chars().map(|c| if c.is_ascii() { c.to_string() } else { format!("\\u{}?", c as u32 as i32 as i16) }).collect::<String>();
        let mut lines = include_str!("samples/protocol.txt").lines().filter(|l| !l.trim().is_empty());
        let head = lines.next().map(u).unwrap_or_default();
        let rest: String = lines.map(|l| format!("\\pard {}\\par", u(l))).collect();
        format!("{{\\rtf1\\ansi\\deff0{{\\fonttbl{{\\f0 Calibri;}}}}\\pard\\b\\fs32 {head}\\b0\\fs22\\par{rest}}}").into_bytes()
    }

    /// Глава книги для снимка модуля (`samples/book.fb2.txt`): `#` и `##` — заголовки, прочее — абзацы.
    pub(crate) fn fb2_chapter() -> Vec<postman_ir::Block> {
        use postman_ir::{Align, Block, Inline};
        include_str!("samples/book.fb2.txt")
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| match (l.strip_prefix("## "), l.strip_prefix("# ")) {
                (Some(h), _) => Block::Heading { level: 2, content: vec![Inline::text(h)] },
                (None, Some(h)) => Block::Heading { level: 1, content: vec![Inline::text(h)] },
                _ => Block::Paragraph { align: Align::Start, content: vec![Inline::text(l)] },
            })
            .collect()
    }

    #[cfg(test)]
    mod tests {
        /// Образцы собираются: три листа сметы, 12 слайдов, глава книги.
        #[test]
        fn samples_build() {
            assert_eq!(super::parts(include_str!("samples/book.txt")).len(), 3);
            assert_eq!(super::parts(include_str!("samples/deck.txt")).len(), 12);
            assert!(super::xlsx().is_ok() && super::pptx().is_ok());
            assert!(super::rtf().starts_with(b"{\\rtf1"));
            assert_eq!(super::fb2_chapter().len(), 4);
        }
    }
}

/// Опись части (ADR 0005, `parts.rs`): сцены разделов просмотра (тексты — в `attachments.ftl`).
pub(crate) const PART: crate::part::Part = crate::part::Part { name: "sections", scenes: scenes::SCENES, ..crate::part::Part::NONE };

/// Сцены `--screens`: файлы собраны сценой, разбирает их воркер, как вложение письма.
pub(crate) mod scenes {
    use gpui_kit::*;
    use postman_core::attach::Viewer;

    use crate::Mailer;
    use crate::screens::{self, Scene};

    pub(crate) const SCENES: &[Scene] = &[
        // Книга (S8-23): листы вкладками, первая строка закреплена.
        Scene::new("preview-xlsx", letter, ready).after("preview-docx").wait(place),
        // Презентация: слайд 3 из 12 — список, «[рисунок]», заметки докладчика.
        Scene::new("preview-pptx", letter, slide_ready).continues().wait(place),
        // Таблица CSV — сеткой без вкладок.
        Scene::new("preview-csv", letter, ready).continues().wait(place),
        // Письмо во вложении: шапка, свои вложения, текст.
        Scene::new("preview-eml", letter, ready).continues().wait(place),
        // RTF — документом.
        Scene::new("preview-rtf", letter, ready).continues().wait(place),
    ];

    /// Файл сцены: имя, просмотр, байты.
    fn file(s: &Scene) -> (&'static str, Viewer) {
        match s.name {
            "preview-xlsx" => ("Смета 2026.xlsx", Viewer::Sheet),          // i18n-ok: имя файла образца
            "preview-pptx" => ("Запуск проекта.pptx", Viewer::Slides),     // i18n-ok: имя файла образца
            "preview-csv" => ("Счета октябрь.csv", Viewer::Table),         // i18n-ok: имя файла образца
            "preview-eml" => ("RE: Договор поставки.eml", Viewer::Letter), // i18n-ok: имя файла образца
            _ => ("Протокол.rtf", Viewer::Document),                       // i18n-ok: имя файла образца
        }
    }

    fn bytes(s: &Scene) -> Vec<u8> {
        let built = match s.name {
            "preview-xlsx" => super::samples::xlsx(),
            "preview-pptx" => super::samples::pptx(),
            "preview-csv" => Ok(super::samples::csv()),
            "preview-eml" => Ok(super::samples::eml()),
            _ => Ok(super::samples::rtf()),
        };
        built.unwrap_or_else(|e| {
            tracing::warn!("screens: sample {}: {e}", s.name);
            Vec::new()
        })
    }

    /// Письмо с вложениями открыто (просмотр — на месте его текста).
    fn letter(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        if screens::reading(m, cx).is_none() {
            screens::select_demo(m, screens::outlook_letter, cx)?;
        }
        Ok(())
    }

    /// Письмо открылось — файл сцены в просмотр (как щелчок по карточке); у презентации — слайд 3 (два нажатия →).
    fn place(s: &Scene, m: &mut Mailer, cx: &mut Context<Mailer>) {
        let (name, viewer) = file(s);
        let blob = format!("screen-{name}");
        let Some(r) = screens::reading(m, cx) else { return };
        let current = r.preview.as_ref().map(|p| (p.row.blob_id.clone(), p.ready(), p.section()));
        match current {
            Some((b, ready, at)) if b == blob => {
                if ready && s.name == "preview-pptx" && at.is_some_and(|i| i < 2) {
                    m.reading.update(cx, |r, cx| r.screen_section(2, cx));
                }
            }
            _ => {
                let data = bytes(s);
                m.reading.update(cx, |r, cx| r.screen_preview(name, viewer, data, cx));
            }
        }
    }

    fn ready(s: &Scene, m: &Mailer, cx: &App) -> bool {
        let blob = format!("screen-{}", file(s).0);
        screens::reading(m, cx).and_then(|r| r.preview.as_ref()).is_some_and(|p| p.row.blob_id == blob && p.ready())
    }

    fn slide_ready(s: &Scene, m: &Mailer, cx: &App) -> bool {
        ready(s, m, cx) && screens::reading(m, cx).and_then(|r| r.preview.as_ref()).and_then(|p| p.section()) == Some(2)
    }
}

#[cfg(test)]
mod tests {
    use postman_ir::{Align, Block, Inline};
    use postman_worker::{Section, SectionTitle};

    use super::{slide_caption, split_notes, title};

    fn p(s: &str) -> Block {
        Block::Paragraph { align: Align::Start, content: vec![Inline::text(s)] }
    }

    /// Подписи разделов — из интерфейса (Fluent), оба языка; заметки — последняя цитата слайда.
    #[test]
    fn captions_and_notes() {
        let _ = pets_i18n::init(&crate::parts::texts(), "ru");
        assert_eq!(title(&SectionTitle::Sheet("Смета".into())), "Лист «Смета»");
        assert_eq!(title(&SectionTitle::Slide(3)), "Слайд 3");
        assert_eq!(title(&SectionTitle::None), "");
        let slide =
            Section { title: SectionTitle::Slide(3), blocks: vec![Block::Heading { level: 1, content: vec![Inline::text("Этапы")] }, p("x")] };
        assert_eq!(slide_caption(&slide), "Слайд 3 · Этапы");
        let blocks = [p("a"), Block::Quote(vec![p("заметка")])];
        let (body, notes) = split_notes(&blocks);
        assert_eq!((body.len(), notes.map(<[Block]>::len)), (1, Some(1)));
        let _ = pets_i18n::init(&crate::parts::texts(), "en");
        assert_eq!(title(&SectionTitle::Sheet("Q3".into())), "Sheet “Q3”");
        assert_eq!(title(&SectionTitle::Slide(2)), "Slide 2");
    }
}
