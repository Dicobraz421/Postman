//! Вложения на диске: безопасные имена, сохранение и открытие по явному
//! действию пользователя. Исполняемые и «активные» типы не открываются —
//! только сохраняются. На Windows файлы получают Mark-of-the-Web (зона
//! Интернет), чтобы SmartScreen и защищённый просмотр Office срабатывали.

use std::io;
use std::path::{Path, PathBuf};

/// Типы, которые открывать нельзя (по мотивам списка Level 1 в Outlook), плюс
/// HTML/SVG (скрипты в браузере с file://) и образы дисков (обход MOTW).
const BLOCKED: &[&str] = &[
    "ade",
    "adp",
    "app",
    "application",
    "appref-ms",
    "asp",
    "aspx",
    "bas",
    "bat",
    "cer",
    "chm",
    "cmd",
    "cnt",
    "com",
    "cpl",
    "crt",
    "csh",
    "der",
    "diagcab",
    "dll",
    "exe",
    "fxp",
    "gadget",
    "grp",
    "hlp",
    "hpj",
    "hta",
    "htc",
    "htm",
    "html",
    "inf",
    "ins",
    "iso",
    "img",
    "isp",
    "its",
    "jar",
    "jnlp",
    "js",
    "jse",
    "ksh",
    "lnk",
    "mad",
    "maf",
    "mag",
    "mam",
    "maq",
    "mar",
    "mas",
    "mat",
    "mau",
    "mav",
    "maw",
    "mcf",
    "mda",
    "mdb",
    "mde",
    "mdt",
    "mdw",
    "mdz",
    "mht",
    "mhtml",
    "msc",
    "msh",
    "msh1",
    "msh2",
    "mshxml",
    "msh1xml",
    "msh2xml",
    "msi",
    "msix",
    "msixbundle",
    "msp",
    "mst",
    "msu",
    "ops",
    "osd",
    "pcd",
    "pif",
    "pl",
    "plg",
    "prf",
    "prg",
    "printerexport",
    "ps1",
    "ps1xml",
    "ps2",
    "ps2xml",
    "psc1",
    "psc2",
    "psd1",
    "psdm1",
    "pst",
    "py",
    "pyc",
    "pyo",
    "pyw",
    "pyz",
    "pyzw",
    "reg",
    "scf",
    "scr",
    "sct",
    "settingcontent-ms",
    "sh",
    "shb",
    "shs",
    "svg",
    "svgz",
    "sys",
    "theme",
    "tmp",
    "url",
    "vb",
    "vbe",
    "vbp",
    "vbs",
    "vhd",
    "vhdx",
    "vsmacros",
    "vsw",
    "webpnp",
    "website",
    "ws",
    "wsb",
    "wsc",
    "wsf",
    "wsh",
    "xbap",
    "xht",
    "xhtml",
    "xll",
    "xnk",
];

const RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5",
    "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Имя файла, безопасное для любой ФС: без путей, управляющих и bidi-символов
/// (подмена `invoice<U+202E>fdp.exe`, видимая как «invoiceexe.pdf»), без зарезервированных имён Windows.
pub fn safe_name(name: &str) -> String {
    let mut s: String = name
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
        .map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c })
        .collect();
    s = s.trim().trim_end_matches(['.', ' ']).to_string();
    let stem = s.split('.').next().unwrap_or("").to_ascii_lowercase();
    if s.is_empty() || RESERVED.contains(&stem.as_str()) {
        s = format!("_{s}");
    }
    if s.chars().count() > 150 {
        let ext = extension(&s);
        let keep: String = s.chars().take(140).collect();
        s = if ext.is_empty() { keep } else { format!("{keep}.{ext}") };
    }
    s
}

fn extension(name: &str) -> String {
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

/// Можно ли открыть файл системным приложением.
pub fn can_open(name: &str) -> bool {
    let ext = extension(&safe_name(name));
    !ext.is_empty() && !BLOCKED.contains(&ext.as_str())
}

pub fn human_size(n: u64) -> String {
    match n {
        n if n < 1024 => format!("{n} Б"),
        n if n < 1024 * 1024 => format!("{:.0} КБ", n as f64 / 1024.0),
        n => format!("{:.1} МБ", n as f64 / 1024.0 / 1024.0),
    }
}

/// Записать файл по выбранному пользователем пути (диалог — в интерфейсе) с Mark-of-the-Web.
pub fn write(path: &Path, data: &[u8]) -> io::Result<()> {
    std::fs::write(path, data)?;
    mark_of_the_web(path);
    Ok(())
}

#[cfg(windows)]
fn mark_of_the_web(path: &Path) {
    let mut ads = path.as_os_str().to_owned();
    ads.push(":Zone.Identifier");
    let _ = std::fs::write(ads, "[ZoneTransfer]\r\nZoneId=3\r\n");
}

#[cfg(not(windows))]
fn mark_of_the_web(_: &Path) {}

/// Сохранить во временный каталог и открыть системным приложением.
pub fn open(dir: &Path, name: &str, data: &[u8]) -> io::Result<PathBuf> {
    if !can_open(name) {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "этот тип файла можно только сохранить"));
    }
    let nonce = jiff::Timestamp::now().as_nanosecond();
    let dir = dir.join(format!("{nonce:x}"));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(safe_name(name));
    write(&path, data)?;
    open::that_detached(&path)?;
    Ok(path)
}

/// MIME по расширению — для заголовка Content-Type вложения.
pub fn mime_for(name: &str) -> &'static str {
    match extension(name).as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "txt" | "log" | "csv" => "text/plain",
        "zip" => "application/zip",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "doc" => "application/msword",
        "xls" => "application/vnd.ms-excel",
        "ics" => "text/calendar",
        "eml" => "message/rfc822",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(safe_name("../../evil.txt"), ".._.._evil.txt");
        assert_eq!(safe_name("invoice\u{202e}fdp.exe"), "invoicefdp.exe");
        assert_eq!(safe_name("CON.txt"), "_CON.txt");
        assert_eq!(safe_name("report. "), "report");
        assert!(!can_open("invoice\u{202e}fdp.exe"));
        assert!(!can_open("счёт.pdf.js"));
        assert!(!can_open("page.HTML"));
        assert!(!can_open("noext"));
        assert!(can_open("Отчёт.xlsx"));
    }
}
