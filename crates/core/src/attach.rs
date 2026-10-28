//! Вложения (S5-07): что с файлом можно делать и чем его показать — по имени и по итогу проверки в воркере.
//!
//! Имя и MIME — строки из письма, не содержимое файла: их классификация не разбирает чужие данные. Содержимое
//! (макросы в OLE и OOXML, оглавление архива, страницы PDF) смотрит только воркер (`postman_worker::preview`);
//! здесь — итоговое решение по его ответу ([`risk_with`]).

use crate::files;

/// Насколько опасно открыть файл системным приложением.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Risk {
    /// Обычный файл: «Открыть» и «Сохранить как».
    Safe,
    /// Документ Office с макросами: открытие заблокировано, только «Сохранить» с предупреждением.
    Macros,
    /// Исполняемое и «активное» содержимое (`files::can_open` — нет): только сохранить.
    Blocked,
}

/// Встроенный просмотр (разбор — в воркере).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Viewer {
    /// PNG, JPEG, GIF, WebP — форматы декодера воркера.
    Image,
    Pdf,
    /// Оглавление ZIP без распаковки.
    Archive,
    /// Текст, CSV, письмо `.eml` — как текст, с ограничением размера.
    Text,
    /// Документ Office: карточка проверки (макросы), содержимое не показывается.
    Office,
    /// Документ Word `.docx` (T1-13б): текст документа — блоками IR из воркера; на макросы проверяется, как Office
    /// ([`needs_scan`]). С макросами по имени (`.docm`, `.dotm`) и старый `.doc` — [`Viewer::Office`].
    Document,
    /// Встроенного просмотра нет: карточка с размером и действиями.
    None,
}

/// Расширения Office, которые объявляют макросы сами (Word, Excel, PowerPoint с поддержкой макросов, надстройки).
const MACRO_EXT: &[&str] = &["docm", "dotm", "xlsm", "xltm", "xlam", "xla", "pptm", "potm", "ppsm", "ppam", "sldm"];

/// Документы Office, макросы в которых видно только по содержимому: OLE (`VBA` в составном файле) и OOXML
/// (`vbaProject.bin` в архиве, в том числе в файле, переименованном из `.xlsm` в `.xlsx`).
const OFFICE_EXT: &[&str] = &["doc", "dot", "xls", "xlt", "ppt", "pot", "pps", "docx", "dotx", "xlsx", "xltx", "xlsb", "pptx", "potx", "ppsx"];

const IMAGE_EXT: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];
const TEXT_EXT: &[&str] = &["txt", "log", "md", "csv", "tsv", "eml", "json", "xml", "ini", "cfg", "conf", "yaml", "yml"];
/// MIME документа Word `.docx`.
const DOCX_MIME: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

fn extension(name: &str) -> String {
    let safe = files::safe_name(name);
    safe.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

/// Опасность по имени: исполняемые — [`Risk::Blocked`], Office с макросами по расширению — [`Risk::Macros`].
/// Так же классифицируются файлы внутри архива (имена из оглавления, которое прислал воркер).
pub fn risk(name: &str) -> Risk {
    let ext = extension(name);
    if !ext.is_empty() && !files::can_open(name) {
        Risk::Blocked
    } else if MACRO_EXT.contains(&ext.as_str()) {
        Risk::Macros
    } else {
        Risk::Safe
    }
}

/// Опасность с итогом проверки содержимого в воркере: `macros` — найден проект VBA. Блокировка по имени
/// не снимается проверкой (переименованный `.docm` остаётся с макросами, даже если проекта не нашлось).
pub fn risk_with(name: &str, macros: Option<bool>) -> Risk {
    match (risk(name), macros) {
        (Risk::Safe, Some(true)) => Risk::Macros,
        (r, _) => r,
    }
}

/// Нужна ли проверка содержимого на макросы (документ Office).
pub fn needs_scan(name: &str) -> bool {
    let ext = extension(name);
    OFFICE_EXT.contains(&ext.as_str()) || MACRO_EXT.contains(&ext.as_str())
}

/// Чем показать файл во встроенном просмотре.
pub fn viewer(name: &str, mime: &str) -> Viewer {
    let ext = extension(name);
    let mime = mime.to_ascii_lowercase();
    let e = ext.as_str();
    if risk(name) == Risk::Blocked {
        return Viewer::None;
    }
    if IMAGE_EXT.contains(&e) {
        Viewer::Image
    } else if e == "pdf" {
        Viewer::Pdf
    } else if e == "zip" {
        Viewer::Archive
    } else if TEXT_EXT.contains(&e) {
        Viewer::Text
    } else if e == "docx" {
        Viewer::Document
    } else if needs_scan(name) {
        Viewer::Office
    } else if !e.is_empty() {
        Viewer::None
    } else if mime == DOCX_MIME {
        Viewer::Document
    } else if matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/gif" | "image/webp") {
        Viewer::Image
    } else if mime == "application/pdf" {
        Viewer::Pdf
    } else if mime == "application/zip" || mime == "application/x-zip-compressed" {
        Viewer::Archive
    } else if mime.starts_with("text/plain") || mime == "message/rfc822" || mime == "text/csv" {
        Viewer::Text
    } else {
        Viewer::None
    }
}

/// Опасные файлы в оглавлении архива: исполняемые и документы с макросами по имени.
pub fn dangerous<'a>(names: impl IntoIterator<Item = &'a str>) -> usize {
    names.into_iter().filter(|n| risk(n) != Risk::Safe).count()
}

/// Свободное имя для «Сохранить все» в каталоге, где уже есть `taken`: «отчёт.pdf», «отчёт (2).pdf»…
pub fn unique_name(name: &str, taken: &mut std::collections::HashSet<String>) -> String {
    let safe = files::safe_name(name);
    let (stem, ext) = match safe.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() && e.len() <= 10 && e.chars().all(|c| c.is_alphanumeric()) => (s.to_string(), format!(".{e}")),
        _ => (safe.clone(), String::new()),
    };
    let mut candidate = safe;
    let mut n = 2;
    while !taken.insert(candidate.to_lowercase()) {
        candidate = format!("{stem} ({n}){ext}");
        n += 1;
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn risk_by_name() {
        assert_eq!(risk("Расчёт.xlsm"), Risk::Macros);
        assert_eq!(risk("Шаблон.DOTM"), Risk::Macros);
        assert_eq!(risk("deck.pptm"), Risk::Macros);
        assert_eq!(risk("Итоги.xlsx"), Risk::Safe);
        assert_eq!(risk("invoice.pdf.exe"), Risk::Blocked);
        assert_eq!(risk("run.ps1"), Risk::Blocked);
        assert_eq!(risk("счёт\u{202e}fdp.exe"), Risk::Blocked);
        assert_eq!(risk("README"), Risk::Safe);
    }

    #[test]
    fn content_scan_upgrades_but_never_clears() {
        // Переименованный .xlsm: имя безобидное, в архиве — vbaProject.bin.
        assert_eq!(risk_with("Итоги.xlsx", Some(true)), Risk::Macros);
        assert_eq!(risk_with("Итоги.xlsx", Some(false)), Risk::Safe);
        assert_eq!(risk_with("Итоги.xlsx", None), Risk::Safe);
        assert_eq!(risk_with("old.doc", Some(true)), Risk::Macros);
        // Объявленные макросы и исполняемые — без поблажек.
        assert_eq!(risk_with("Расчёт.xlsm", Some(false)), Risk::Macros);
        assert_eq!(risk_with("setup.exe", Some(false)), Risk::Blocked);
        assert!(needs_scan("old.DOC") && needs_scan("Итоги.xlsx") && needs_scan("Расчёт.xlsm"));
        assert!(!needs_scan("scan.pdf") && !needs_scan("photo.png"));
    }

    #[test]
    fn viewers() {
        assert_eq!(viewer("logo.PNG", ""), Viewer::Image);
        assert_eq!(viewer("Отчёт.pdf", "application/octet-stream"), Viewer::Pdf);
        assert_eq!(viewer("Скриншоты.zip", ""), Viewer::Archive);
        assert_eq!(viewer("data.csv", "text/csv"), Viewer::Text);
        assert_eq!(viewer("letter.eml", "message/rfc822"), Viewer::Text);
        assert_eq!(viewer("Расчёт.xlsm", ""), Viewer::Office);
        assert_eq!(viewer("old.doc", ""), Viewer::Office);
        // Word (T1-13б): .docx — просмотр документа, с макросами по имени и старый .doc — карточка проверки.
        assert_eq!(viewer("Отчёт.DOCX", "application/octet-stream"), Viewer::Document);
        assert_eq!(viewer("Отчёт.docm", DOCX_MIME), Viewer::Office);
        assert_eq!(viewer("Шаблон.dotm", ""), Viewer::Office);
        assert_eq!(viewer("old.doc", "application/msword"), Viewer::Office);
        assert_eq!(viewer("Отчёт", DOCX_MIME), Viewer::Document, "MIME без имени");
        assert_eq!(viewer("Отчёт", &DOCX_MIME.to_uppercase()), Viewer::Document);
        assert!(needs_scan("Отчёт.docx"), "макросы .docx проверяются, как раньше");
        assert_eq!(viewer("setup.exe", ""), Viewer::None);
        assert_eq!(viewer("page.html", "text/html"), Viewer::None);
        assert_eq!(viewer("archive.7z", ""), Viewer::None);
        assert_eq!(viewer("photo.heic", "image/heic"), Viewer::None);
        // Без расширения — по MIME.
        assert_eq!(viewer("photo", "image/jpeg"), Viewer::Image);
        assert_eq!(viewer("scan", "application/pdf"), Viewer::Pdf);
        assert_eq!(viewer("blob", "application/octet-stream"), Viewer::None);
    }

    #[test]
    fn dangerous_inner_files() {
        let names = ["01.png", "tools/run.bat", "Отчёт.docm", "readme.txt", "x.js"];
        assert_eq!(dangerous(names), 3);
    }

    #[test]
    fn unique_names_for_save_all() {
        let mut taken = std::collections::HashSet::new();
        assert_eq!(unique_name("отчёт.pdf", &mut taken), "отчёт.pdf");
        assert_eq!(unique_name("Отчёт.pdf", &mut taken), "Отчёт (2).pdf");
        assert_eq!(unique_name("отчёт.pdf", &mut taken), "отчёт (3).pdf");
        assert_eq!(unique_name("../README", &mut taken), ".._README");
        assert_eq!(unique_name(".._README", &mut taken), ".._README (2)");
    }
}
