//! Просмотр таблицы `.csv`/`.tsv` (S8-17): один раздел-таблица. Только здесь, в песочнице.
//!
//! Читается не больше [`MAX_CSV`] байт от начала файла: UTF-8 (с BOM или без), UTF-16 с BOM, иначе — Windows-1251.
//! Разделитель — `,`, `;` или табуляция: тот, что делит первые [`SNIFF`] строк на одинаковое число полей (больше
//! одного), из таких — с большим числом полей. Кавычки — RFC 4180: `""` — кавычка, перевод строки в кавычках — внутри
//! ячейки; кавычка посреди поля без кавычек — просто знак. Пределы — как у листа книги ([`super::sheet`]): строки,
//! столбцы, ячейки и символы; остальное — `truncated`.

use std::ops::ControlFlow;

use super::Section;
use super::sheet::{self, Budget, Limits};

/// Байт от начала файла.
pub const MAX_CSV: usize = 16 * 1024 * 1024;
/// Строк для выбора разделителя.
pub const SNIFF: usize = 20;

/// Таблица → один раздел и признак «показано начало». Не ошибается: любой текст — таблица хотя бы в столбец.
pub fn csv(bytes: &[u8]) -> (Vec<Section>, bool) {
    parse(bytes, sheet::LIMITS)
}

fn parse(bytes: &[u8], limits: Limits) -> (Vec<Section>, bool) {
    let cut = bytes.len() > MAX_CSV;
    let text = super::decode(&bytes[..bytes.len().min(MAX_CSV)], cut);
    let delim = delimiter(&text, limits);
    let mut budget = Budget::new(limits);
    budget.truncated = cut;
    let mut cells = Vec::new();
    let mut row = 0u32;
    records(&text, delim, limits.cols as usize + 1, |fields| {
        if row >= limits.rows {
            budget.truncated = true;
            return ControlFlow::Break(());
        }
        for (col, f) in fields.iter().enumerate() {
            if f.is_empty() {
                continue;
            }
            if col >= limits.cols as usize {
                budget.truncated = true;
                break;
            }
            let Some(text) = budget.text(f) else { return ControlFlow::Break(()) };
            cells.push((row, col as u32, text));
        }
        row += 1;
        ControlFlow::Continue(())
    });
    let blocks = budget.table(cells);
    (vec![Section { title: None, blocks }], budget.truncated)
}

/// Разделитель по первым [`SNIFF`] непустым строкам; ничего не подошло — запятая.
fn delimiter(text: &str, limits: Limits) -> char {
    let mut best = (',', (false, 0usize));
    for d in [',', ';', '\t'] {
        let mut counts = Vec::new();
        records(text, d, limits.cols as usize + 1, |f| {
            if !(f.len() == 1 && f[0].is_empty()) {
                counts.push(f.len());
            }
            if counts.len() >= SNIFF { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
        });
        let first = counts.first().copied().unwrap_or(0);
        let same = first > 1 && counts.iter().all(|&n| n == first);
        let score = (same, counts.iter().copied().min().unwrap_or(0));
        if score > best.1 {
            best = (d, score);
        }
    }
    best.0
}

/// Записи по RFC 4180; в записи — не больше `max` полей (остальные отбрасываются). Конец строки — `\r\n`, `\n` или `\r`.
fn records(text: &str, delim: char, max: usize, mut on: impl FnMut(&[String]) -> ControlFlow<()>) {
    let mut fields: Vec<String> = Vec::new();
    let mut field = String::new();
    let (mut quoted, mut start, mut any) = (false, true, false);
    let mut chars = text.chars().peekable();
    let push = |fields: &mut Vec<String>, field: &mut String| {
        let f = std::mem::take(field);
        if fields.len() < max {
            fields.push(f);
        }
    };
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                '\r' => {
                    chars.next_if_eq(&'\n');
                    field.push('\n');
                }
                c => field.push(c),
            }
            continue;
        }
        match c {
            '"' if start => (quoted, start, any) = (true, false, true),
            '\r' | '\n' => {
                if c == '\r' {
                    chars.next_if_eq(&'\n');
                }
                push(&mut fields, &mut field);
                if on(&fields).is_break() {
                    return;
                }
                fields.clear();
                (start, any) = (true, false);
            }
            c if c == delim => {
                push(&mut fields, &mut field);
                (start, any) = (true, true);
            }
            c => {
                field.push(c);
                (start, any) = (false, true);
            }
        }
    }
    if any {
        push(&mut fields, &mut field);
        let _ = on(&fields);
    }
}

#[cfg(test)]
mod tests {
    use postman_ir::{Block, Inline};

    use super::*;
    use crate::preview::{Preview, PreviewKind, preview};

    fn grid(s: &[Section]) -> Vec<Vec<String>> {
        let [Section { title: None, blocks }] = s else { panic!("{s:?}") };
        let [Block::Table(rows)] = blocks.as_slice() else { panic!("{blocks:?}") };
        rows.iter()
            .map(|r| {
                r.iter()
                    .map(|c| {
                        c.iter()
                            .map(|i| match i {
                                Inline::Text { text, .. } => text.as_str(),
                                Inline::LineBreak => "\n",
                                _ => "?",
                            })
                            .collect()
                    })
                    .collect()
            })
            .collect()
    }

    fn rows(r: &[&[&str]]) -> Vec<Vec<String>> {
        r.iter().map(|r| r.iter().map(|s| s.to_string()).collect()).collect()
    }

    #[test]
    fn semicolons_in_cp1251() {
        // «Дата;Сумма\r\n21.10;100,5\r\n» в Windows-1251, как сохраняет русский Excel.
        let mut b: Vec<u8> = vec![0xC4, 0xE0, 0xF2, 0xE0, b';', 0xD1, 0xF3, 0xEC, 0xEC, 0xE0];
        b.extend(b"\r\n21.10;100,5\r\n");
        let (pv, _) = preview(PreviewKind::Table, &b, 0, 0, 0).unwrap();
        let Preview::Document { sections, truncated } = pv else { panic!("{pv:?}") };
        assert!(!truncated);
        assert_eq!(grid(&sections), rows(&[&["Дата", "Сумма"], &["21.10", "100,5"]]));
    }

    #[test]
    fn quotes_rfc4180_tabs_and_bom() {
        let text = "\u{feff}a,\"b, \"\"c\"\"\",\"line\r\nbreak\"\n1,,3\n\n x\"y\" ,z\n";
        let (s, cut) = csv(text.as_bytes());
        assert!(!cut);
        assert_eq!(grid(&s), rows(&[&["a", "b, \"c\"", "line\nbreak"], &["1", "", "3"], &["", "", ""], &[" x\"y\" ", "z", ""]]));
        let (s, _) = csv("имя\tсрок\nсмета\t2026-10-29".as_bytes());
        assert_eq!(grid(&s), rows(&[&["имя", "срок"], &["смета", "2026-10-29"]]));
        // Запятые внутри полей не сбивают выбор: поровну делит только «;».
        let (s, _) = csv(b"a;b,c\nd;e\nf;g,h,i\n");
        assert_eq!(grid(&s), rows(&[&["a", "b,c"], &["d", "e"], &["f", "g,h,i"]]));
        // Один столбец, пусто, незакрытая кавычка.
        assert_eq!(grid(&csv(b"x\ny").0), rows(&[&["x"], &["y"]]));
        assert_eq!(csv(b"").0, [Section { title: None, blocks: Vec::new() }]);
        assert_eq!(grid(&csv(b"\"open,1\n2").0), rows(&[&["open,1\n2"]]));
    }

    #[test]
    fn big_tables_are_cut() {
        let line: String = (0..150).map(|i| i.to_string()).collect::<Vec<_>>().join(",");
        let text = format!("{line}\n").repeat(1500);
        let (s, cut) = csv(text.as_bytes());
        assert!(cut);
        let g = grid(&s);
        assert_eq!((g.len(), g[0].len()), (sheet::MAX_ROWS as usize, sheet::MAX_COLS as usize));
        assert_eq!(g[999][99], "99");
        let (s, cut) = parse(text.as_bytes(), Limits { chars: 30, ..sheet::LIMITS });
        assert!(cut);
        assert_eq!(grid(&s).len(), 1);
        let huge = vec![b'x'; MAX_CSV + 10];
        assert!(csv(&huge).1);
    }
}
