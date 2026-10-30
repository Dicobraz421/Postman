//! Вложения (S5-07): что с файлом можно делать и чем его показать — по имени и по итогу проверки в воркере.
//!
//! Имя и MIME — строки из письма, не содержимое файла: их классификация не разбирает чужие данные. Содержимое
//! (макросы в OLE и OOXML, оглавление архива, страницы PDF) смотрит только воркер (`postman_worker::preview`);
//! здесь — итоговое решение по его ответу ([`risk_with`]).

use std::collections::BTreeSet;

use postman_worker::PreviewKind;

use crate::files;
use crate::viewers::{ModuleId, Registry};

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

/// Встроенный просмотр (разбор — в воркере) или модуль просмотра ([`Viewer::Module`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Viewer {
    /// PNG, JPEG, GIF, WebP, BMP, ICO, TIFF (страницами, как PDF) — форматы декодера воркера.
    Image,
    Pdf,
    /// Оглавление ZIP без распаковки.
    Archive,
    /// Текст, JSON, XML — как текст, с ограничением размера.
    Text,
    /// Документ Office: карточка проверки (макросы), содержимое не показывается.
    Office,
    /// Документ — блоками IR из воркера: Word `.docx` (T1-13б), `.odt`, `.rtf`, `.md` (S8-19); `.docx` на макросы
    /// проверяется, как Office ([`needs_scan`]). С макросами по имени (`.docm`, `.dotm`) и старый `.doc` —
    /// [`Viewer::Office`].
    Document,
    /// Книга `.xlsx`, `.xltx`, `.ods` — раздел на лист (S8-17); с макросами по имени и старый `.xls` — `Office`.
    Sheet,
    /// Таблица `.csv`, `.tsv` — один раздел (S8-17).
    Table,
    /// Презентация `.pptx`, `.ppsx`, `.potx`, `.odp` — раздел на слайд (S8-18); с макросами и `.ppt` — `Office`.
    Slides,
    /// Письмо во вложении `.eml`, `message/rfc822`, `.msg` Outlook (S8-20): шапка, тело IR, список вложений.
    Letter,
    /// Модуль просмотра WASM (S8-26): показывает хост `postman-viewer` ([`crate::viewer::Viewer`]); описание и
    /// «сторонний» — [`Registry::get`].
    Module {
        id: ModuleId,
    },
    /// Встроенного просмотра нет: карточка с размером и действиями.
    None,
}

impl Viewer {
    /// Запрос к воркеру (`Client::preview`); `None` — воркер не нужен (`Office` — проверка отдельно, по
    /// [`needs_scan`]; модуль — у хоста модулей). Ответ разделами или письмом — в модель ядра [`crate::render::viewed`].
    pub fn kind(self) -> Option<PreviewKind> {
        match self {
            Viewer::Image => Some(PreviewKind::Image),
            Viewer::Pdf => Some(PreviewKind::Pdf),
            Viewer::Archive => Some(PreviewKind::Archive),
            Viewer::Text => Some(PreviewKind::Text),
            Viewer::Document => Some(PreviewKind::Document),
            Viewer::Sheet => Some(PreviewKind::Sheet),
            Viewer::Table => Some(PreviewKind::Table),
            Viewer::Slides => Some(PreviewKind::Slides),
            Viewer::Letter => Some(PreviewKind::Letter),
            Viewer::Office | Viewer::Module { .. } | Viewer::None => None,
        }
    }
}

/// Расширения Office, которые объявляют макросы сами (Word, Excel, PowerPoint с поддержкой макросов, надстройки).
const MACRO_EXT: &[&str] = &["docm", "dotm", "xlsm", "xltm", "xlam", "xla", "pptm", "potm", "ppsm", "ppam", "sldm"];

/// Документы Office, макросы в которых видно только по содержимому: OLE (`VBA` в составном файле) и OOXML
/// (`vbaProject.bin` в архиве, в том числе в файле, переименованном из `.xlsm` в `.xlsx`).
const OFFICE_EXT: &[&str] = &["doc", "dot", "xls", "xlt", "ppt", "pot", "pps", "docx", "dotx", "xlsx", "xltx", "xlsb", "pptx", "potx", "ppsx"];

/// Встроенные форматы по расширению (ADR 0007, «Встроенный набор S8»). Книги и презентации OOXML — ещё и в
/// [`OFFICE_EXT`]: на макросы их проверяет воркер.
const BY_EXT: &[(&[&str], Viewer)] = &[
    (&["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "tif", "tiff"], Viewer::Image),
    (&["pdf"], Viewer::Pdf),
    (&["zip"], Viewer::Archive),
    (&["txt", "log", "json", "xml", "ini", "cfg", "conf", "yaml", "yml"], Viewer::Text),
    (&["docx", "odt", "rtf", "md"], Viewer::Document),
    (&["xlsx", "xltx", "ods"], Viewer::Sheet),
    (&["csv", "tsv"], Viewer::Table),
    (&["pptx", "ppsx", "potx", "odp"], Viewer::Slides),
    (&["eml", "msg"], Viewer::Letter),
];

/// Файл без расширения — по MIME (без параметров, `; charset=…`).
const BY_MIME: &[(&str, Viewer)] = &[
    ("image/png", Viewer::Image),
    ("image/jpeg", Viewer::Image),
    ("image/gif", Viewer::Image),
    ("image/webp", Viewer::Image),
    ("image/bmp", Viewer::Image),
    ("image/x-icon", Viewer::Image),
    ("image/vnd.microsoft.icon", Viewer::Image),
    ("image/tiff", Viewer::Image),
    ("application/pdf", Viewer::Pdf),
    ("application/zip", Viewer::Archive),
    ("application/x-zip-compressed", Viewer::Archive),
    ("text/plain", Viewer::Text),
    (DOCX_MIME, Viewer::Document),
    ("application/vnd.oasis.opendocument.text", Viewer::Document),
    ("application/rtf", Viewer::Document),
    ("text/rtf", Viewer::Document),
    ("text/markdown", Viewer::Document),
    ("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", Viewer::Sheet),
    ("application/vnd.oasis.opendocument.spreadsheet", Viewer::Sheet),
    ("text/csv", Viewer::Table),
    ("text/tab-separated-values", Viewer::Table),
    ("application/vnd.openxmlformats-officedocument.presentationml.presentation", Viewer::Slides),
    ("application/vnd.oasis.opendocument.presentation", Viewer::Slides),
    ("message/rfc822", Viewer::Letter),
    ("application/vnd.ms-outlook", Viewer::Letter),
];

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

/// Чем показать файл во встроенном просмотре: по расширению, без расширения — по MIME. С макросами по имени
/// (`.xlsm`, `.pptm`, `.docm`…) и старые `.doc`, `.xls`, `.ppt` — [`Viewer::Office`].
pub fn viewer(name: &str, mime: &str) -> Viewer {
    let ext = extension(name);
    if risk(name) == Risk::Blocked {
        return Viewer::None;
    }
    if let Some(&(_, v)) = BY_EXT.iter().find(|(exts, _)| exts.contains(&ext.as_str())) {
        return v;
    }
    if needs_scan(name) {
        Viewer::Office
    } else if !ext.is_empty() {
        Viewer::None
    } else {
        let mime = mime.split(';').next().unwrap_or_default().trim().to_ascii_lowercase();
        BY_MIME.iter().find(|(m, _)| *m == mime).map_or(Viewer::None, |&(_, v)| v)
    }
}

/// Чем показать файл: встроенный просмотр ([`viewer`]) главнее; нет его — модуль просмотра по расширению, MIME или
/// первым байтам файла `head` (пусто — ещё не загружен; сверяется с началом, не разбирается), выключенные в
/// настройках (`off`, [`crate::prefs::Viewers`]) — как нет их. Исполняемые — без просмотра и модулем.
pub fn viewer_with(name: &str, mime: &str, head: &[u8], modules: &Registry, off: &BTreeSet<String>) -> Viewer {
    match viewer(name, mime) {
        Viewer::None if risk(name) != Risk::Blocked => {
            let mime = mime.split(';').next().unwrap_or_default().trim().to_ascii_lowercase();
            modules.find(&extension(name), &mime, head, off).map_or(Viewer::None, |id| Viewer::Module { id })
        }
        v => v,
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
        assert_eq!(viewer("notes.txt", "text/plain"), Viewer::Text);
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

    /// Встроенный набор S8 (ADR 0007): каждое расширение таблицы и MIME без имени.
    #[test]
    fn viewers_s8() {
        let by_ext: &[(&[&str], Viewer)] = &[
            (&["Отчёт.odt", "Письмо.RTF", "README.md", "Отчёт.docx"], Viewer::Document),
            (&["Итоги.xlsx", "Шаблон.xltx", "Бюджет.ods"], Viewer::Sheet),
            (&["data.csv", "data.TSV"], Viewer::Table),
            (&["deck.pptx", "show.ppsx", "Шаблон.potx", "deck.odp"], Viewer::Slides),
            (&["letter.eml", "Outlook.msg"], Viewer::Letter),
            (&["scan.bmp", "favicon.ico", "fax.tif", "fax.TIFF", "logo.png", "photo.jpeg", "anim.gif", "pic.webp"], Viewer::Image),
        ];
        for (names, v) in by_ext {
            for name in *names {
                assert_eq!(viewer(name, "application/octet-stream"), *v, "{name}");
            }
        }
        // С макросами по имени и старые двоичные — карточка проверки, как раньше.
        for name in ["Расчёт.xlsm", "Шаблон.xltm", "deck.pptm", "show.ppsm", "Отчёт.docm", "old.xls", "old.ppt", "old.doc", "book.xlsb"]
        {
            assert_eq!(viewer(name, ""), Viewer::Office, "{name}");
        }
        assert!(needs_scan("Итоги.xlsx") && needs_scan("deck.pptx") && !needs_scan("Бюджет.ods") && !needs_scan("data.csv"));
        // Имя главнее MIME: у `.xlsm` с MIME книги — всё равно проверка.
        assert_eq!(viewer("Расчёт.xlsm", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"), Viewer::Office);
        assert_eq!(viewer("letter.eml", "text/plain"), Viewer::Letter);

        let by_mime = [
            ("application/vnd.oasis.opendocument.text", Viewer::Document),
            ("application/vnd.oasis.opendocument.spreadsheet", Viewer::Sheet),
            ("application/vnd.oasis.opendocument.presentation", Viewer::Slides),
            ("application/rtf", Viewer::Document),
            ("text/rtf", Viewer::Document),
            ("text/markdown; charset=utf-8", Viewer::Document),
            ("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", Viewer::Sheet),
            ("application/vnd.openxmlformats-officedocument.presentationml.presentation", Viewer::Slides),
            ("text/csv", Viewer::Table),
            ("text/csv; charset=windows-1251", Viewer::Table),
            ("text/tab-separated-values", Viewer::Table),
            ("application/vnd.ms-outlook", Viewer::Letter),
            ("message/rfc822", Viewer::Letter),
            ("Message/RFC822", Viewer::Letter),
            ("image/bmp", Viewer::Image),
            ("image/x-icon", Viewer::Image),
            ("image/vnd.microsoft.icon", Viewer::Image),
            ("image/tiff", Viewer::Image),
            ("text/plain; charset=utf-8", Viewer::Text),
            ("application/vnd.ms-excel", Viewer::None),
            ("text/html", Viewer::None),
        ];
        for (mime, v) in by_mime {
            assert_eq!(viewer("attachment", mime), v, "{mime}");
        }
    }

    /// Модуль — только когда встроенного просмотра нет: `.docx` с модулем `.docx` — встроенный.
    #[test]
    fn builtin_viewer_wins_over_module() {
        let root = tempfile::tempdir().unwrap();
        let json = r#"{"name": "Всё", "version": "1", "abi": 1, "extensions": ["docx", "fb2", "exe"], "mime": ["application/x-fictionbook+xml"],
            "signatures": [{"prefix": "<?xml", "contains": "<FictionBook"}]}"#;
        std::fs::write(root.path().join("all.json"), json).unwrap();
        std::fs::write(root.path().join("all.wasm"), b"\0asm\x01\0\0\0").unwrap();
        let modules = Registry::load(&[crate::viewers::Dir { path: root.path().into(), third_party: true }]);
        let (id, _) = modules.modules().next().unwrap();
        let none = BTreeSet::new();
        assert_eq!(viewer_with("Отчёт.docx", "", b"", &modules, &none), Viewer::Document);
        assert_eq!(viewer_with("Отчёт.docm", "", b"", &modules, &none), Viewer::Office);
        assert_eq!(viewer_with("книга.FB2", "", b"", &modules, &none), Viewer::Module { id });
        assert_eq!(viewer_with("книга", "application/x-fictionbook+xml; charset=utf-8", b"", &modules, &none), Viewer::Module { id });
        assert_eq!(viewer_with("книга.bin", "", b"<?xml version=\"1.0\"?><FictionBook>", &modules, &none), Viewer::Module { id });
        assert_eq!(viewer_with("книга.xml", "", b"<?xml version=\"1.0\"?><FictionBook>", &modules, &none), Viewer::Text, "XML — встроенный");
        assert_eq!(viewer_with("setup.exe", "", b"", &modules, &none), Viewer::None, "исполняемые — без модуля");
        assert_eq!(viewer_with("книга.fb2", "", b"", &modules, &BTreeSet::from(["all".to_string()])), Viewer::None, "выключен");
        assert_eq!(Viewer::Module { id }.kind(), None);
    }

    #[test]
    fn worker_kinds() {
        assert_eq!(Viewer::Sheet.kind(), Some(PreviewKind::Sheet));
        assert_eq!(Viewer::Table.kind(), Some(PreviewKind::Table));
        assert_eq!(Viewer::Slides.kind(), Some(PreviewKind::Slides));
        assert_eq!(Viewer::Letter.kind(), Some(PreviewKind::Letter));
        assert_eq!(Viewer::Document.kind(), Some(PreviewKind::Document));
        assert_eq!((Viewer::Office.kind(), Viewer::None.kind()), (None, None));
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
