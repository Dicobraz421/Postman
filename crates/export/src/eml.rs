//! EML: письмо — файл, байты как есть.

use std::io::{self, Write};

use crate::{Civil, Letter};

/// Имя файла `ГГГГ-ММ-ДД ЧЧММ — Тема.eml` по местному времени письма; без темы — `ГГГГ-ММ-ДД ЧЧММ.eml`.
/// Через `safe_name`; повторы в каталоге — `Tree::unique`.
pub fn file_name(letter: &Letter) -> String {
    let c = Civil::from_unix(letter.date.saturating_add(i64::from(letter.offset)));
    let date = format!("{:04}-{:02}-{:02} {:02}{:02}", c.year, c.month, c.day, c.hour, c.minute);
    let subject = letter.subject.split_whitespace().collect::<Vec<_>>().join(" ");
    let name = if subject.is_empty() { format!("{date}.eml") } else { format!("{date} — {subject}.eml") };
    crate::safe_name(&name)
}

/// Байты письма как есть.
pub fn write(out: &mut impl Write, letter: &Letter) -> io::Result<()> {
    out.write_all(letter.raw)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::{Flags, Layout, Tree};

    fn letter<'a>(subject: &'a str, raw: &'a [u8]) -> Letter<'a> {
        // 2026-10-29 11:05:09 UTC; в Москве — 14:05.
        Letter { raw, date: 1_793_271_909, offset: 3 * 3600, from: "a@example.org", subject, flags: Flags::default(), folder: &[] }
    }

    #[test]
    fn names() {
        assert_eq!(file_name(&letter("Отчёт за октябрь", b"")), "2026-10-29 1405 — Отчёт за октябрь.eml");
        assert_eq!(file_name(&letter("Re: план?\r\n\tвторой", b"")), "2026-10-29 1405 — Re_ план_ второй.eml");
        assert_eq!(file_name(&letter("  ", b"")), "2026-10-29 1405.eml");
        let utc = Letter { offset: 0, ..letter("x", b"") };
        assert_eq!(file_name(&utc), "2026-10-29 1105 — x.eml");
        let mut tree = Tree::new(Layout::Dirs);
        let a = tree.unique(Path::new(""), &file_name(&letter("Тема", b"")));
        let b = tree.unique(Path::new(""), &file_name(&letter("тема", b"")));
        assert_eq!((a.as_str(), b.as_str()), ("2026-10-29 1405 — Тема.eml", "2026-10-29 1405 — тема (2).eml"));
    }

    #[test]
    fn bytes_as_is() {
        let raw = b"From: a@example.org\r\nSubject: x\r\n\r\nFrom here\r\n";
        let mut out = Vec::new();
        write(&mut out, &letter("x", raw)).unwrap();
        assert_eq!(out, raw);
    }
}
