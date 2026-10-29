//! Maildir: каталог папки с `cur/`, `new/`, `tmp/`; письмо — файл `<время>.<pid>_<n>.<узел>:2,<флаги>`, байты как
//! есть. Запись — в `tmp`, затем переименование в `cur` (`Place`); каталоги папок — `Tree::folder` с раскладкой
//! `Maildir` или `MaildirPlus`.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::{Flags, Letter};

/// Подкаталоги каталога папки.
pub const DIRS: [&str; 3] = ["cur", "new", "tmp"];

/// Разделитель имени и флагов по Maildir.
pub const COLON: char = ':';
/// Разделитель для ФС без двоеточия в именах (NTFS: `имя:2,S` — поток ADS), как у mbsync на Windows.
pub const SEMICOLON: char = ';';

/// Пути письма: сначала пишем в `tmp`, затем переименовываем в `cur`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub tmp: PathBuf,
    pub cur: PathBuf,
}

/// Имена писем одной выгрузки: `n` растёт с каждым письмом, имена не повторяются.
#[derive(Debug)]
pub struct Maildir {
    pid: u32,
    host: String,
    info: char,
    n: u64,
}

impl Maildir {
    /// `pid` и `host` — процесса и узла, которые пишут; `info` — разделитель флагов (`COLON` или `SEMICOLON`).
    pub fn new(pid: u32, host: &str, info: char) -> Self {
        // Узел — одним полем имени: без точек, разделителей путей и флагов.
        let host: String = host.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
        let host = if host.is_empty() { "localhost".to_string() } else { host };
        Self { pid, host, info, n: 0 }
    }

    /// Пути письма в каталоге папки `dir`; время в имени — время письма.
    pub fn place(&mut self, dir: &Path, letter: &Letter) -> Place {
        self.n += 1;
        let base = format!("{}.{}_{}.{}", letter.date.max(0), self.pid, self.n, self.host);
        Place { tmp: dir.join("tmp").join(&base), cur: dir.join("cur").join(format!("{base}{}2,{}", self.info, flags(letter.flags))) }
    }
}

/// Флаги Maildir по алфавиту: `D` — черновик, `F` — флаг, `R` — отвечено, `S` — прочитано.
pub fn flags(f: Flags) -> String {
    [(f.draft, 'D'), (f.flagged, 'F'), (f.answered, 'R'), (f.seen, 'S')].iter().filter(|(on, _)| *on).map(|(_, c)| c).collect()
}

/// Байты письма как есть.
pub fn write(out: &mut impl Write, letter: &Letter) -> io::Result<()> {
    out.write_all(letter.raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn letter(flags: Flags) -> Letter<'static> {
        Letter { raw: b"Subject: x\n\nbody\n", date: 1_793_271_909, offset: 0, from: "a@b", subject: "x", flags, folder: &[] }
    }

    #[test]
    fn flags_in_order() {
        assert_eq!(flags(Flags::default()), "");
        assert_eq!(flags(Flags { seen: true, ..Flags::default() }), "S");
        assert_eq!(flags(Flags { seen: true, answered: true, ..Flags::default() }), "RS");
        assert_eq!(flags(Flags { seen: true, flagged: true, ..Flags::default() }), "FS");
        assert_eq!(flags(Flags { draft: true, flagged: true, ..Flags::default() }), "DF");
        assert_eq!(flags(Flags { seen: true, answered: true, flagged: true, draft: true }), "DFRS");
    }

    #[test]
    fn places() {
        let mut md = Maildir::new(4242, "WORK-PC", COLON);
        let dir = Path::new("Входящие");
        let p = md.place(dir, &letter(Flags { seen: true, answered: true, ..Flags::default() }));
        assert_eq!(p.tmp, dir.join("tmp").join("1793271909.4242_1.WORK-PC"));
        assert_eq!(p.cur, dir.join("cur").join("1793271909.4242_1.WORK-PC:2,RS"));
        let p = md.place(dir, &letter(Flags::default()));
        assert_eq!(p.cur, dir.join("cur").join("1793271909.4242_2.WORK-PC:2,"));
        let mut md = Maildir::new(7, "host.example/x:y", SEMICOLON);
        let p = md.place(Path::new(""), &letter(Flags { flagged: true, ..Flags::default() }));
        assert_eq!(p.cur, Path::new("cur").join("1793271909.7_1.host_example_x_y;2,F"));
        assert_eq!(Maildir::new(1, "", COLON).host, "localhost");
    }

    #[test]
    fn bytes_as_is() {
        let mut out = Vec::new();
        write(&mut out, &letter(Flags::default())).unwrap();
        assert_eq!(out, b"Subject: x\n\nbody\n");
    }
}
