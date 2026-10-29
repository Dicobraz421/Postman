//! mbox (mboxrd): строка `From <адрес> <asctime UTC>`, письмо с экранированием `^>*From ` → `>` спереди,
//! пустая строка после письма. Переводы строк — как в письме. Флаги не пишем (`X-Mozilla-Status` — нет).
//! Файл папки — `Tree::folder` с раскладкой `Thunderbird` или `AppleMail`; письма дописываются по одному.

use std::io::{self, Write};

use crate::{Civil, Letter};

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// Дописать письмо в mbox. Писать в буфер (`BufWriter`): вывод идёт кусками между экранированиями.
pub fn write(out: &mut impl Write, letter: &Letter) -> io::Result<()> {
    let raw = letter.raw;
    let eol: &[u8] = match raw.iter().position(|&b| b == b'\n') {
        Some(i) if i > 0 && raw[i - 1] == b'\r' => b"\r\n",
        _ => b"\n",
    };
    out.write_all(from_line(letter).as_bytes())?;
    out.write_all(eol)?;
    let mut done = 0;
    let mut start = 0;
    while start < raw.len() {
        let line = &raw[start..];
        if line.iter().position(|&b| b != b'>').is_some_and(|i| line[i..].starts_with(b"From ")) {
            out.write_all(&raw[done..start])?;
            out.write_all(b">")?;
            done = start;
        }
        start = match line.iter().position(|&b| b == b'\n') {
            Some(i) => start + i + 1,
            None => raw.len(),
        };
    }
    out.write_all(&raw[done..])?;
    if !raw.is_empty() && !raw.ends_with(b"\n") {
        out.write_all(eol)?;
    }
    out.write_all(eol)
}

/// `From user@host Thu Oct 29 11:05:09 2026` (без перевода строки). Адрес без пробелов; нет — `MAILER-DAEMON`.
fn from_line(letter: &Letter) -> String {
    let addr: String = letter.from.chars().filter(|c| !c.is_whitespace() && !c.is_control()).collect();
    let addr = if addr.is_empty() { "MAILER-DAEMON" } else { &addr };
    let c = Civil::from_unix(letter.date);
    format!(
        "From {addr} {} {} {:>2} {:02}:{:02}:{:02} {}",
        WEEKDAYS[c.weekday as usize % 7],
        MONTHS[(c.month as usize + 11) % 12],
        c.day,
        c.hour,
        c.minute,
        c.second,
        c.year
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Flags;

    fn letter<'a>(raw: &'a [u8], date: i64, from: &'a str) -> Letter<'a> {
        Letter { raw, date, offset: 0, from, subject: "", flags: Flags::default(), folder: &[] }
    }

    fn mbox(letters: &[Letter]) -> Vec<u8> {
        let mut out = Vec::new();
        for l in letters {
            write(&mut out, l).unwrap();
        }
        out
    }

    /// Обратное чтение mboxrd: письмо начинается строкой `From ` в начале файла или после пустой строки,
    /// кончается пустой строкой перед следующим; `>` перед `>*From ` снимается.
    fn read(file: &[u8]) -> Vec<Vec<u8>> {
        let mut lines = Vec::new();
        let mut start = 0;
        while start < file.len() {
            let end = file[start..].iter().position(|&b| b == b'\n').map_or(file.len(), |i| start + i + 1);
            lines.push(&file[start..end]);
            start = end;
        }
        let blank = |l: &[u8]| l == b"\n" || l == b"\r\n";
        let mut letters: Vec<Vec<u8>> = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            if line.starts_with(b"From ") && (i == 0 || blank(lines[i - 1])) {
                if let Some(last) = letters.last_mut() {
                    let cut = if last.ends_with(b"\r\n") { 2 } else { 1 };
                    last.truncate(last.len() - cut);
                }
                letters.push(Vec::new());
                continue;
            }
            let Some(last) = letters.last_mut() else { panic!("в начале файла нет строки From") };
            let unescaped = line.iter().position(|&b| b != b'>').is_some_and(|j| j > 0 && line[j..].starts_with(b"From "));
            last.extend_from_slice(if unescaped { &line[1..] } else { line });
        }
        if let Some(last) = letters.last_mut() {
            let cut = if last.ends_with(b"\r\n") { 2 } else { 1 };
            last.truncate(last.len() - cut);
        }
        letters
    }

    #[test]
    fn escapes_from_lines() {
        let raw = b"From: a@example.org\nSubject: x\n\nFrom here we go\n>From quoted\n>>From deeper\n From not at start\nFromage\n";
        let out = mbox(&[letter(raw, 1_793_271_909, "a@example.org")]);
        let expected = "From a@example.org Thu Oct 29 11:05:09 2026\n\
            From: a@example.org\nSubject: x\n\n>From here we go\n>>From quoted\n>>>From deeper\n From not at start\nFromage\n\n";
        assert_eq!(String::from_utf8(out).unwrap(), expected);
    }

    #[test]
    fn from_line_format() {
        // asctime: день — с пробелом до двух знаков.
        assert_eq!(from_line(&letter(b"", 1_790_845_384, "user@host")), "From user@host Thu Oct  1 09:03:04 2026");
        assert_eq!(from_line(&letter(b"", 0, " ")), "From MAILER-DAEMON Thu Jan  1 00:00:00 1970");
        assert_eq!(from_line(&letter(b"", 0, "bad addr@host")), "From badaddr@host Thu Jan  1 00:00:00 1970");
    }

    #[test]
    fn keeps_line_endings() {
        let raw = b"Subject: x\r\n\r\nFrom me\r\nbye\r\n";
        let out = mbox(&[letter(raw, 0, "a@b")]);
        assert_eq!(out, b"From a@b Thu Jan  1 00:00:00 1970\r\nSubject: x\r\n\r\n>From me\r\nbye\r\n\r\n");
        // Без перевода строки в конце — дописываем его, иначе следующая строка From прилипнет к письму.
        let out = mbox(&[letter(b"Subject: y\n\nend", 0, "a@b")]);
        assert_eq!(out, b"From a@b Thu Jan  1 00:00:00 1970\nSubject: y\n\nend\n\n");
    }

    #[test]
    fn read_back_gives_original_bytes() {
        let letters: [&[u8]; 5] = [
            b"From: a@example.org\nSubject: one\n\nFrom the start\n\nFrom after blank\n>From already quoted\n",
            b"Subject: two\r\n\r\n>>From crlf\r\n\r\nFrom crlf after blank\r\n\r\n",
            b"From nobody header-like first line\nSubject: three\n\nbody\n",
            b"Subject: empty body\n\n",
            b"Subject: four\n\n\n\nFrom \n",
        ];
        let input: Vec<Letter> = letters.iter().enumerate().map(|(i, raw)| letter(raw, 1_790_845_384 + i as i64, "a@example.org")).collect();
        let file = mbox(&input);
        let back = read(&file);
        assert_eq!(back.len(), letters.len());
        for (got, want) in back.iter().zip(letters) {
            assert_eq!(String::from_utf8_lossy(got), String::from_utf8_lossy(want));
        }
    }
}
