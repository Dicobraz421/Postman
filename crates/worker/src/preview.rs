//! Встроенный просмотр вложений (S5-07): картинка, страница PDF, оглавление ZIP, текст, документ Word `.docx`
//! (T1-13а, [`docx`]), книга `.xlsx`/`.ods` ([`sheet`]) и таблица `.csv`/`.tsv` ([`csv`]) — S8-17, презентация
//! `.pptx`/`.odp` ([`slides`], S8-18), проверка Office на макросы. Только здесь, в песочнице: UI получает пиксели,
//! строки, числа и безопасный IR (документ — разделами: лист книги, слайд, ADR 0007).
//!
//! Картинки — PNG, JPEG, GIF, WebP, BMP, ICO и TIFF (страницы — как у PDF, S8-21).
//!
//! Пределы — на каждый запрос: вход не больше [`MAX_INPUT`], картинка и страница — не больше `max_side` по
//! стороне (страница PDF — шириной `width`, если задана), текст — [`MAX_TEXT`] байт, оглавление — [`MAX_LISTED`] строк; архив не распаковывается (размеры —
//! из центрального каталога), из OOXML читается только `vbaProject.bin` и не больше [`MAX_VBA`], у `.docx` — пределы
//! [`docx`] (распаковка, блоки, символы), у книги и таблицы — пределы [`sheet`] (листы, строки, столбцы, ячейки), у
//! презентации — [`slides`]; все части книги или презентации вместе — не больше [`MAX_UNPACKED`] после распаковки. Время
//! ограничивает сторож в `serve` (процесс завершается с [`crate::EXIT_TIME`]), память — Job Object песочницы.

use std::io::{self, Read};

pub mod csv;
pub mod docx;
pub mod sheet;
pub mod slides;

use serde::{Deserialize, Serialize};

use crate::Rgba;

/// Больше не смотрим: файл слишком большой для просмотра (сохранить можно).
pub const MAX_INPUT: usize = 48 * 1024 * 1024;
/// Текст: столько байт от начала файла.
pub const MAX_TEXT: usize = 64 * 1024;
/// Строк оглавления архива в ответе; остальные — только в счётчиках.
pub const MAX_LISTED: usize = 500;
/// `vbaProject.bin` больше — макросы есть, модули не перечисляем.
pub const MAX_VBA: u64 = 16 * 1024 * 1024;
/// Сторона страницы PDF при просмотре, если UI не задал меньше.
pub const PDF_SIDE: u32 = 1600;
/// Все части книги или презентации вместе после распаковки: дальше не читаем, показанное — `truncated`.
pub const MAX_UNPACKED: u64 = 64 * 1024 * 1024;

/// Чем показать вложение (выбирает UI по имени и MIME, `postman_core::attach::viewer`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PreviewKind {
    Image,
    Pdf,
    Archive,
    Text,
    /// Документ Office: только проверка на макросы.
    Office,
    /// Документ Word `.docx` → IR (T1-13а); документ с макросами — проверка Office, как у `Office`.
    Document,
    /// Книга `.xlsx`/`.xltx`/`.ods` → раздел на лист с таблицей (S8-17); с макросами — проверка Office.
    Sheet,
    /// Таблица `.csv`/`.tsv` → один раздел-таблица (S8-17).
    Table,
    /// Презентация `.pptx`/`.ppsx`/`.potx`/`.odp` → раздел на слайд (S8-18); с макросами — проверка Office.
    Slides,
}

/// Итог просмотра. У `Image` и `Pdf` пиксели RGBA — в бинарной части ответа.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Preview {
    Image {
        width: u32,
        height: u32,
        /// Размер исходной картинки и её формат («PNG»).
        source: (u32, u32),
        format: String,
        /// Показанная страница (с нуля) и всего страниц: у TIFF — как у PDF, у остальных — 0 и 1.
        page: u32,
        pages: u32,
    },
    Pdf {
        width: u32,
        height: u32,
        /// Показанная страница (с нуля) и всего страниц.
        page: u32,
        pages: u32,
        title: Option<String>,
    },
    Archive(Listing),
    Text {
        text: String,
        /// Показано начало файла.
        truncated: bool,
    },
    Office(OfficeScan),
    /// Документ разделами: безопасный IR (как тело письма); `truncated` — показано начало.
    Document {
        sections: Vec<Section>,
        truncated: bool,
    },
}

/// Раздел документа: у `.docx` и таблицы — один без заголовка, у книги — лист (заголовок — имя листа), у презентации
/// — слайд («Слайд N»).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Section {
    pub title: Option<String>,
    pub blocks: Vec<postman_ir::Block>,
}

/// Оглавление архива.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Listing {
    /// Первые [`MAX_LISTED`] файлов (без каталогов).
    pub entries: Vec<Entry>,
    /// Всего файлов, их размер после распаковки и в архиве.
    pub files: u64,
    pub size: u64,
    pub packed: u64,
    /// Есть зашифрованные файлы.
    pub encrypted: bool,
    /// Похоже на zip-бомбу: после распаковки в сотни раз больше архива и больше гигабайта.
    pub bomb: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    pub size: u64,
    pub packed: u64,
    /// «2026-10-21 14:01» (время из архива, без пояса).
    pub modified: Option<String>,
    pub encrypted: bool,
}

/// Проверка документа Office.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OfficeScan {
    pub format: OfficeFormat,
    /// Найден проект VBA (`VBA`/`Macros` в OLE, `vbaProject.bin` или тип `macroEnabled` в OOXML).
    pub macros: bool,
    /// Модули проекта VBA (имена потоков), если их удалось перечислить.
    pub modules: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum OfficeFormat {
    /// Составной файл OLE (`.doc`, `.xls`, `.ppt`).
    Ole,
    /// Office Open XML (ZIP).
    Ooxml,
    #[default]
    Unknown,
}

/// Просмотр в песочнице: итог и пиксели (пусто, если их нет). `width` — ширина страницы PDF в пикселях (по ширине
/// панели просмотра); 0 — страница целиком в `max_side`.
pub fn preview(kind: PreviewKind, bytes: &[u8], page: u32, max_side: u32, width: u32) -> Result<(Preview, Vec<u8>), String> {
    if bytes.len() > MAX_INPUT {
        return Err("файл слишком большой для просмотра".into());
    }
    match kind {
        PreviewKind::Image => {
            let d = crate::decode(bytes, max_side, page)?;
            let p =
                Preview::Image { width: d.img.width, height: d.img.height, source: d.source, format: d.format.into(), page: d.page, pages: d.pages };
            Ok((p, d.img.pixels))
        }
        PreviewKind::Pdf => pdf(bytes, page, max_side, width),
        PreviewKind::Archive => Ok((Preview::Archive(list_zip(bytes)?), Vec::new())),
        PreviewKind::Text => {
            let (text, truncated) = text(bytes);
            Ok((Preview::Text { text, truncated }, Vec::new()))
        }
        PreviewKind::Office => Ok((Preview::Office(scan_office(bytes)?), Vec::new())),
        PreviewKind::Document => sections(bytes, |b| docx::docx(b).map(|(blocks, t)| (vec![Section { title: None, blocks }], t))),
        PreviewKind::Sheet => sections(bytes, sheet::sheet),
        PreviewKind::Slides => sections(bytes, slides::slides),
        PreviewKind::Table => {
            let (sections, truncated) = csv::csv(bytes);
            Ok((Preview::Document { sections, truncated }, Vec::new()))
        }
    }
}

/// Документ Office разделами; с макросами (`.docm`, `.xlsm`, `.pptm`, `vbaProject.bin`) — не просмотр, а проверка: открытие
/// заблокировано.
fn sections(bytes: &[u8], parse: impl Fn(&[u8]) -> Result<(Vec<Section>, bool), String>) -> Result<(Preview, Vec<u8>), String> {
    let scan = scan_office(bytes)?;
    if scan.macros {
        return Ok((Preview::Office(scan), Vec::new()));
    }
    let (sections, truncated) = parse(bytes)?;
    Ok((Preview::Document { sections, truncated }, Vec::new()))
}

// --- PDF ---

fn pdf(bytes: &[u8], page: u32, max_side: u32, width: u32) -> Result<(Preview, Vec<u8>), String> {
    use hayro::hayro_interpret::InterpreterSettings;
    use hayro::hayro_syntax::Pdf;
    use hayro::vello_cpu::color::palette::css::WHITE;
    use hayro::{PixmapSettings, RenderCache, RenderSettings};

    if !bytes.starts_with(b"%PDF") && !bytes.get(..1024).is_some_and(|h| h.windows(5).any(|w| w == b"%PDF-")) {
        return Err("не PDF".into());
    }
    let pdf = Pdf::new(bytes.to_vec()).map_err(|e| match e {
        hayro::hayro_syntax::LoadPdfError::Decryption(_) => "PDF защищён паролем".to_string(),
        _ => "PDF не разобран".to_string(),
    })?;
    let pages = pdf.pages();
    let count = pages.len();
    let Some(last) = count.checked_sub(1) else { return Err("в PDF нет страниц".into()) };
    let index = (page as usize).min(last);
    let p = &pages[index];
    let (w, h) = p.render_dimensions();
    if !(w.is_finite() && h.is_finite() && w >= 1.0 && h >= 1.0) {
        return Err("у страницы нет размера".into());
    }
    let side = max_side.clamp(64, 4096) as f32;
    // По ширине `width`, но не больше `side` по стороне; меньше страницы в пунктах — не увеличиваем больше чем
    // вчетверо.
    let fit = side / w.max(h);
    let scale = if width > 0 { (width as f32 / w).min(fit) } else { fit }.min(4.0);
    let settings = InterpreterSettings { render_annotations: true, ..InterpreterSettings::default() };
    let pixmap = hayro::render(
        p,
        &RenderCache::new(),
        &settings,
        &RenderSettings::default(),
        &PixmapSettings { x_scale: scale, y_scale: scale, bg_color: WHITE },
    );
    let (width, height) = (u32::from(pixmap.width()), u32::from(pixmap.height()));
    // Фон белый, непрозрачный: умноженные на альфу каналы совпадают с обычными.
    let pixels: Vec<u8> = pixmap.data_as_u8_slice().to_vec();
    if pixels.len() != width as usize * height as usize * 4 {
        return Err("страница не отрисована".into());
    }
    let title = pdf_title(&pdf);
    let preview = Preview::Pdf { width, height, page: index as u32, pages: count as u32, title };
    Ok((preview, pixels))
}

fn pdf_title(pdf: &hayro::hayro_syntax::Pdf) -> Option<String> {
    let raw = pdf.metadata().title.as_deref()?;
    let text = pdf_string(raw);
    let text: String = text.chars().filter(|c| !c.is_control()).take(200).collect();
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Строка PDF: UTF-16BE с BOM или PDFDocEncoding (для простоты — Latin-1).
fn pdf_string(b: &[u8]) -> String {
    if let Some(rest) = b.strip_prefix(&[0xFE, 0xFF]) {
        let units = rest.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c));
        return char::decode_utf16(units).map(|c| c.unwrap_or('\u{FFFD}')).collect();
    }
    if let Some(rest) = b.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    b.iter().map(|&c| char::from(c)).collect()
}

// --- ZIP ---

/// Оглавление ZIP по центральному каталогу: ничего не распаковывается, даже если размеры — ложь.
pub fn list_zip(bytes: &[u8]) -> Result<Listing, String> {
    let zip = zip::ZipArchive::new(io::Cursor::new(bytes)).map_err(|e| format!("архив не разобран: {e}"))?;
    let mut out = Listing::default();
    for i in 0..zip.len() {
        let Ok(e) = zip.by_index_data(i) else { continue };
        if e.is_dir() {
            continue;
        }
        let (size, packed) = (e.size(), e.compressed_size());
        out.files += 1;
        out.size = out.size.saturating_add(size);
        out.packed = out.packed.saturating_add(packed);
        out.encrypted |= e.encrypted();
        if out.entries.len() < MAX_LISTED {
            let modified = e
                .last_modified()
                .filter(|d| d.year() >= 1980)
                .map(|d| format!("{:04}-{:02}-{:02} {:02}:{:02}", d.year(), d.month(), d.day(), d.hour(), d.minute()));
            out.entries.push(Entry { name: entry_name(e.name_raw()), size, packed, modified, encrypted: e.encrypted() });
        }
    }
    out.bomb = out.size > 1 << 30 && out.size / out.packed.max(1) > 100;
    Ok(out)
}

/// Имя файла в архиве: UTF-8, иначе — OEM-кодировка русской Windows (CP866), как пишет «Проводник» без флага UTF-8.
fn entry_name(raw: &[u8]) -> String {
    let name = match std::str::from_utf8(raw) {
        Ok(s) => s.to_string(),
        Err(_) => raw.iter().map(|&b| cp866(b)).collect(),
    };
    name.chars().filter(|c| !c.is_control()).take(400).collect()
}

fn cp866(b: u8) -> char {
    const BOX: &str = "░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀";
    const TAIL: &str = "ЁёЄєЇїЎў°∙·√№¤■\u{A0}";
    match b {
        0..=0x7F => char::from(b),
        0x80..=0xAF => char::from_u32(0x0410 + u32::from(b - 0x80)).unwrap_or('?'),
        0xB0..=0xDF => BOX.chars().nth(usize::from(b - 0xB0)).unwrap_or('?'),
        0xE0..=0xEF => char::from_u32(0x0440 + u32::from(b - 0xE0)).unwrap_or('?'),
        _ => TAIL.chars().nth(usize::from(b - 0xF0)).unwrap_or('?'),
    }
}

// --- Office ---

const OLE_MAGIC: &[u8] = &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

/// Макросы в документе Office: OLE — хранилища `VBA`, `_VBA_PROJECT_CUR`, `Macros`; OOXML — `vbaProject.bin`
/// (его модули — тем же разбором OLE) или тип содержимого `macroEnabled`.
pub fn scan_office(bytes: &[u8]) -> Result<OfficeScan, String> {
    if bytes.starts_with(OLE_MAGIC) {
        let (macros, modules) = scan_ole(bytes)?;
        return Ok(OfficeScan { format: OfficeFormat::Ole, macros, modules });
    }
    if !bytes.starts_with(b"PK\x03\x04") {
        return Ok(OfficeScan::default());
    }
    let mut zip = zip::ZipArchive::new(io::Cursor::new(bytes)).map_err(|e| format!("документ не разобран: {e}"))?;
    let mut scan = OfficeScan { format: OfficeFormat::Ooxml, ..OfficeScan::default() };
    let vba: Vec<usize> = (0..zip.len())
        .filter(|&i| zip.by_index_data(i).is_ok_and(|e| String::from_utf8_lossy(e.name_raw()).to_ascii_lowercase().ends_with("vbaproject.bin")))
        .collect();
    scan.macros = !vba.is_empty();
    if let Some(&i) = vba.first()
        && let Ok(f) = zip.by_index(i)
    {
        let mut data = Vec::new();
        if f.size() <= MAX_VBA && f.take(MAX_VBA).read_to_end(&mut data).is_ok() {
            scan.modules = scan_ole(&data).map(|(_, m)| m).unwrap_or_default();
        }
    }
    if !scan.macros
        && let Ok(f) = zip.by_name("[Content_Types].xml")
    {
        let mut types = Vec::new();
        if f.take(1024 * 1024).read_to_end(&mut types).is_ok() {
            scan.macros = String::from_utf8_lossy(&types).to_ascii_lowercase().contains("macroenabled");
        }
    }
    Ok(scan)
}

/// Составной файл: есть ли проект VBA и имена его модулей (потоки хранилища `VBA`, кроме служебных).
fn scan_ole(bytes: &[u8]) -> Result<(bool, Vec<String>), String> {
    let cf = cfb::CompoundFile::open(io::Cursor::new(bytes)).map_err(|e| format!("документ не разобран: {e}"))?;
    let mut macros = false;
    let mut modules = Vec::new();
    for e in cf.walk() {
        let name = e.name();
        if e.is_storage() && matches!(name, "VBA" | "_VBA_PROJECT_CUR" | "Macros") {
            macros = true;
        }
        let parent = e.path().parent().and_then(|p| p.file_name()).and_then(|n| n.to_str());
        let service = matches!(name, "_VBA_PROJECT" | "dir" | "PROJECT" | "PROJECTwm") || name.starts_with("__SRP_");
        if e.is_stream() && parent == Some("VBA") && !service && modules.len() < 64 {
            modules.push(name.chars().filter(|c| !c.is_control()).take(64).collect());
        }
    }
    Ok((macros, modules))
}

// --- текст ---

/// Начало файла как текст: UTF-8 (с BOM или без), UTF-16 с BOM, иначе — Windows-1251.
pub fn text(bytes: &[u8]) -> (String, bool) {
    let truncated = bytes.len() > MAX_TEXT;
    let decoded = decode(&bytes[..bytes.len().min(MAX_TEXT)], truncated);
    let text = decoded
        .replace("\r\n", "\n")
        .chars()
        .map(|c| if c == '\r' { '\n' } else { c })
        .filter(|&c| c == '\n' || c == '\t' || !c.is_control())
        .filter(|c| !matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
        .collect();
    (text, truncated)
}

/// Начало файла `head` в строку: UTF-8 (с BOM или без), UTF-16 с BOM, иначе — Windows-1251; `truncated` — файл
/// длиннее `head` (последний символ UTF-8 мог разрезаться).
fn decode(head: &[u8], truncated: bool) -> String {
    if let Some(rest) = head.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(rest).into_owned()
    } else if let Some(rest) = head.strip_prefix(&[0xFF, 0xFE]) {
        char::decode_utf16(rest.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c))).map(|c| c.unwrap_or('\u{FFFD}')).collect()
    } else if let Some(rest) = head.strip_prefix(&[0xFE, 0xFF]) {
        char::decode_utf16(rest.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c))).map(|c| c.unwrap_or('\u{FFFD}')).collect()
    } else {
        match std::str::from_utf8(head) {
            Ok(s) => s.to_string(),
            // Обрезали посреди символа UTF-8 — это всё ещё UTF-8.
            Err(e) if truncated && e.error_len().is_none() => String::from_utf8_lossy(&head[..e.valid_up_to()]).into_owned(),
            Err(_) => head.iter().map(|&b| cp1251(b)).collect(),
        }
    }
}

fn cp1251(b: u8) -> char {
    const HIGH: &str = "ЂЃ‚ѓ„…†‡€‰Љ‹ЊЌЋЏђ‘’“”•–—\u{FFFD}™љ›њќћџ\u{A0}ЎўЈ¤Ґ¦§Ё©Є«¬\u{AD}®Ї°±Ііґµ¶·ё№є»јЅѕї";
    match b {
        0..=0x7F => char::from(b),
        0x80..=0xBF => HIGH.chars().nth(usize::from(b - 0x80)).unwrap_or('?'),
        _ => char::from_u32(0x0410 + u32::from(b - 0xC0)).unwrap_or('?'),
    }
}

/// Пиксели ответа для клиента: длина сходится с размером.
pub(crate) fn rgba(width: u32, height: u32, pixels: Vec<u8>) -> Result<Rgba, String> {
    if pixels.len() == width as usize * height as usize * 4 {
        Ok(Rgba { width, height, pixels })
    } else {
        Err("неожиданный ответ".into())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn zip_of(files: &[(&str, &[u8])], method: zip::CompressionMethod) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default().compression_method(method);
        for (name, data) in files {
            if name.ends_with('/') {
                w.add_directory(*name, opts).unwrap();
            } else {
                w.start_file(*name, opts).unwrap();
                w.write_all(data).unwrap();
            }
        }
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn lists_zip_entries_without_extracting() {
        let z = zip_of(&[("01.png", b"png"), ("docs/", b""), ("tools/run.bat", b"@echo off")], zip::CompressionMethod::Stored);
        let l = list_zip(&z).unwrap();
        assert_eq!(l.files, 2, "каталоги не считаются");
        let names: Vec<&str> = l.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"01.png") && names.contains(&"tools/run.bat"));
        assert_eq!(l.entries[0].size, 3);
        assert!(!l.encrypted && !l.bomb);
        assert!(l.entries[0].modified.is_some());
    }

    #[test]
    fn many_entries_are_counted_but_listed_up_to_the_limit() {
        let names: Vec<String> = (0..MAX_LISTED + 20).map(|i| format!("f{i}.txt")).collect();
        let files: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), b"x" as &[u8])).collect();
        let l = list_zip(&zip_of(&files, zip::CompressionMethod::Stored)).unwrap();
        assert_eq!(l.entries.len(), MAX_LISTED);
        assert_eq!(l.files, (MAX_LISTED + 20) as u64);
    }

    /// Zip-бомба: каталог обещает 4 ГБ в каждом из 1000 файлов при 10 байтах данных. Оглавление строится по
    /// каталогу, мгновенно и без выделения памяти под данные; распаковки нет.
    #[test]
    fn zip_bomb_is_listed_without_decompressing() {
        let z = lying_zip(1000, 0xFFFF_FFF0);
        let started = std::time::Instant::now();
        let l = list_zip(&z).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        assert_eq!(l.files, 1000);
        assert_eq!(l.size, 1000 * 0xFFFF_FFF0);
        assert!(l.bomb);
    }

    /// ZIP из «сохранённых» записей с ложным размером после распаковки (в локальном и центральном заголовках).
    fn lying_zip(n: usize, claimed: u32) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        let data = b"0123456789";
        for i in 0..n {
            let name = format!("bomb{i}.bin");
            let offset = out.len() as u32;
            let header = |sig: u32, central: bool| {
                let mut h = Vec::new();
                h.extend(sig.to_le_bytes());
                if central {
                    h.extend(20u16.to_le_bytes()); // made by
                }
                h.extend(20u16.to_le_bytes()); // needed
                h.extend(0u16.to_le_bytes()); // flags
                h.extend(8u16.to_le_bytes()); // deflate (не распакуется — и не нужно)
                h.extend(0u16.to_le_bytes()); // time
                h.extend(0x5821u16.to_le_bytes()); // date 2024-01-01
                h.extend(0u32.to_le_bytes()); // crc
                h.extend((data.len() as u32).to_le_bytes());
                h.extend(claimed.to_le_bytes());
                h.extend((name.len() as u16).to_le_bytes());
                h.extend(0u16.to_le_bytes()); // extra
                if central {
                    h.extend(0u16.to_le_bytes()); // comment
                    h.extend(0u16.to_le_bytes()); // disk
                    h.extend(0u16.to_le_bytes()); // internal attrs
                    h.extend(0u32.to_le_bytes()); // external attrs
                    h.extend(offset.to_le_bytes());
                }
                h.extend(name.as_bytes());
                h
            };
            out.extend(header(0x0403_4b50, false));
            out.extend(data);
            central.extend(header(0x0201_4b50, true));
        }
        let cd_offset = out.len() as u32;
        out.extend(&central);
        out.extend(0x0605_4b50u32.to_le_bytes());
        out.extend([0u8; 4]);
        out.extend((n as u16).to_le_bytes());
        out.extend((n as u16).to_le_bytes());
        out.extend((central.len() as u32).to_le_bytes());
        out.extend(cd_offset.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out
    }

    #[test]
    fn non_utf8_names_are_cp866() {
        // «Отчёт» в CP866: 8E E2 E7 F1 E2.
        assert_eq!(entry_name(&[0x8E, 0xE2, 0xE7, 0xF1, 0xE2, b'.', b't', b'x', b't']), "Отчёт.txt");
        assert_eq!(entry_name("Отчёт.txt".as_bytes()), "Отчёт.txt");
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(list_zip(b"PK\x03\x04 garbage").is_err());
        assert!(list_zip(b"").is_err());
        assert!(preview(PreviewKind::Pdf, b"not a pdf", 0, 800, 0).is_err());
        assert!(preview(PreviewKind::Pdf, b"%PDF-1.7\n garbage", 0, 800, 0).is_err());
        assert!(scan_office(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0, 0]).is_err());
        assert_eq!(scan_office(b"plain").unwrap(), OfficeScan::default());
        assert!(preview(PreviewKind::Archive, &vec![0u8; MAX_INPUT + 1], 0, 800, 0).is_err());
    }

    /// Составной файл с проектом VBA: модули — потоки хранилища `VBA`, служебные потоки не считаются.
    fn ole_with_vba(root: &str) -> Vec<u8> {
        let mut cf = cfb::CompoundFile::create(io::Cursor::new(Vec::new())).unwrap();
        let base = if root.is_empty() { String::new() } else { format!("/{root}") };
        if !root.is_empty() {
            cf.create_storage(&base).unwrap();
        }
        cf.create_storage(format!("{base}/VBA")).unwrap();
        for s in ["_VBA_PROJECT", "dir", "Module1", "ThisWorkbook", "__SRP_0"] {
            cf.create_stream(format!("{base}/VBA/{s}")).unwrap().write_all(b"x").unwrap();
        }
        cf.create_stream(format!("{base}/PROJECT")).unwrap().write_all(b"x").unwrap();
        cf.flush().unwrap();
        cf.into_inner().into_inner()
    }

    #[test]
    fn ole_macros_are_found() {
        // Excel 97: хранилище `_VBA_PROJECT_CUR` с `VBA` внутри.
        let s = scan_office(&ole_with_vba("_VBA_PROJECT_CUR")).unwrap();
        assert_eq!(s.format, OfficeFormat::Ole);
        assert!(s.macros);
        assert_eq!(s.modules, ["Module1", "ThisWorkbook"]);
        // Документ без макросов.
        let mut cf = cfb::CompoundFile::create(io::Cursor::new(Vec::new())).unwrap();
        cf.create_stream("/WordDocument").unwrap().write_all(b"x").unwrap();
        cf.flush().unwrap();
        let s = scan_office(&cf.into_inner().into_inner()).unwrap();
        assert!(!s.macros && s.modules.is_empty());
    }

    #[test]
    fn ooxml_macros_are_found_even_when_renamed() {
        let vba = ole_with_vba("");
        let xlsm = zip_of(
            &[("[Content_Types].xml", b"<Types/>"), ("xl/workbook.xml", b"<w/>"), ("xl/vbaProject.bin", &vba)],
            zip::CompressionMethod::Deflated,
        );
        let s = scan_office(&xlsm).unwrap();
        assert_eq!(s.format, OfficeFormat::Ooxml);
        assert!(s.macros);
        assert_eq!(s.modules, ["Module1", "ThisWorkbook"]);
        let typed = zip_of(
            &[("[Content_Types].xml", b"<Override ContentType=\"application/vnd.ms-excel.sheet.macroEnabled.main+xml\"/>")],
            zip::CompressionMethod::Deflated,
        );
        assert!(scan_office(&typed).unwrap().macros);
        let clean = zip_of(&[("[Content_Types].xml", b"<Types/>"), ("word/document.xml", b"<d/>")], zip::CompressionMethod::Deflated);
        assert!(!scan_office(&clean).unwrap().macros);
    }

    #[test]
    fn text_is_decoded_and_limited() {
        assert_eq!(text("Привет\r\nмир".as_bytes()), ("Привет\nмир".to_string(), false));
        // Windows-1251.
        assert_eq!(text(&[0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2]).0, "Привет");
        // UTF-16LE с BOM.
        assert_eq!(text(&[0xFF, 0xFE, b'O', 0, b'K', 0]).0, "OK");
        // Управляющие и bidi-символы убраны.
        assert_eq!(text("a\u{0}b\u{202e}c\td".as_bytes()).0, "abc\td");
        let big = "я".repeat(MAX_TEXT);
        let (t, cut) = text(big.as_bytes());
        assert!(cut && t.len() <= MAX_TEXT && t.chars().all(|c| c == 'я'));
    }

    #[test]
    fn pdf_page_fits_width_within_side() {
        let pdf = include_bytes!("../tests/fixtures/report.pdf");
        // A4 (595 × 842 пт) шириной 900: высота по пропорции, сторона не больше 2048.
        let (p, px) = preview(PreviewKind::Pdf, pdf, 0, 2048, 900).unwrap();
        let Preview::Pdf { width, height, .. } = p else { panic!("{p:?}") };
        assert_eq!(width, 900);
        assert!((1270..=1276).contains(&height), "{height}");
        assert_eq!(px.len(), (width * height * 4) as usize);
        // Шире предела — предел по длинной стороне.
        let (p, _) = preview(PreviewKind::Pdf, pdf, 0, 2048, 3000).unwrap();
        let Preview::Pdf { width, height, .. } = p else { panic!("{p:?}") };
        assert_eq!(height, 2048);
        assert!(width < 1500, "{width}");
    }

    #[test]
    fn pdf_strings() {
        assert_eq!(pdf_string(&[0xFE, 0xFF, 0x04, 0x1E, 0x04, 0x42]), "От");
        assert_eq!(pdf_string(b"Report"), "Report");
    }
}
