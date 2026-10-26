//! Живое отображение Markdown в поле письма (S5-27, `docs/DESIGN.md` «Новое письмо»): своё письмо разбирается здесь
//! (без I/O, без чужих данных), интерфейс красит текст по отрезкам. Отправка — без изменений (`markdown_to_html`).
//!
//! Итог — отрезки без пересечений, у каждого набор признаков ([`Look`]): жирный, курсив, подчёркнутый, ссылка… и
//! «разметка» — сами знаки `**`, `#`, `>`, `[…](…)`, `<u>`: их интерфейс приглушает. Незакрытый маркер (`**текст`) —
//! обычный текст, как и при отправке.

use std::ops::Range;

use pulldown_cmark::{Event, LinkType, Options, Parser, Tag, TagEnd};

/// Признаки отрезка текста.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Look {
    pub strong: bool,
    pub emphasis: bool,
    pub underline: bool,
    pub strike: bool,
    pub code: bool,
    pub link: bool,
    pub quote: bool,
    /// Уровень заголовка (1…6), 0 — не заголовок.
    pub heading: u8,
    /// Маркер пункта списка (`-`, `*`, `1.`).
    pub list: bool,
    /// Знаки разметки — приглушить.
    pub markup: bool,
}

impl Look {
    fn is_plain(&self) -> bool {
        *self == Look::default()
    }
}

/// Отрезок текста (байты UTF-8) с признаками; отрезки идут по порядку, не пересекаются, обычный текст — пропущен.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub range: Range<usize>,
    pub look: Look,
}

/// Разметка письма для отображения в поле ввода.
pub fn display(md: &str) -> Vec<Span> {
    let mut looks = vec![Look::default(); md.len()];
    let mut set = |r: Range<usize>, f: &dyn Fn(&mut Look)| {
        for l in &mut looks[r.start.min(md.len())..r.end.min(md.len())] {
            f(l);
        }
    };
    let opts = Options::ENABLE_STRIKETHROUGH;
    let mut underline: Option<Range<usize>> = None;
    let mut code: Vec<Range<usize>> = Vec::new();
    for (ev, r) in Parser::new_ext(md, opts).into_offset_iter() {
        let src = &md[r.clone()];
        match ev {
            Event::Start(Tag::Heading { level, .. }) => {
                let level = level as u8;
                set(r.clone(), &|l| l.heading = level);
                let marks = src.len() - src.trim_start_matches('#').len();
                if marks > 0 {
                    let spaces = src[marks..].len() - src[marks..].trim_start_matches([' ', '\t']).len();
                    set(r.start..r.start + marks + spaces, &|l| l.markup = true);
                }
            }
            Event::Start(Tag::Strong) => {
                set(r.clone(), &|l| l.strong = true);
                wrap(&mut set, &r, 2);
            }
            Event::Start(Tag::Emphasis) => {
                set(r.clone(), &|l| l.emphasis = true);
                wrap(&mut set, &r, 1);
            }
            Event::Start(Tag::Strikethrough) => {
                set(r.clone(), &|l| l.strike = true);
                let n = src.len() - src.trim_start_matches('~').len();
                wrap(&mut set, &r, n);
            }
            Event::Code(_) => {
                set(r.clone(), &|l| l.code = true);
                let n = src.len() - src.trim_start_matches('`').len();
                wrap(&mut set, &r, n);
                code.push(r.clone());
            }
            Event::Start(Tag::Link { link_type, .. }) => match link_type {
                LinkType::Autolink | LinkType::Email => {
                    set(r.clone(), &|l| l.link = true);
                    if src.starts_with('<') && src.ends_with('>') {
                        wrap(&mut set, &r, 1);
                    }
                }
                LinkType::Inline => {
                    // `[текст](адрес)`: текст — ссылкой, скобки и адрес — разметкой.
                    match src.rfind("](") {
                        Some(i) if src.starts_with('[') => {
                            set(r.start + 1..r.start + i, &|l| l.link = true);
                            set(r.start..r.start + 1, &|l| l.markup = true);
                            set(r.start + i..r.end, &|l| l.markup = true);
                        }
                        _ => set(r.clone(), &|l| l.link = true),
                    }
                }
                _ => set(r.clone(), &|l| l.link = true),
            },
            Event::Start(Tag::BlockQuote(_)) => {
                set(r.clone(), &|l| l.quote = true);
                // `>` в начале каждой строки цитаты (и пробел после него).
                let mut at = r.start;
                for line in src.split_inclusive('\n') {
                    let lead = line.len() - line.trim_start_matches([' ', '\t']).len();
                    let rest = &line[lead..];
                    let n = rest.len() - rest.trim_start_matches(['>', ' ']).len();
                    if rest.starts_with('>') {
                        set(at + lead..at + lead + n, &|l| l.markup = true);
                    }
                    at += line.len();
                }
            }
            Event::Start(Tag::Item) => {
                let lead = src.len() - src.trim_start_matches([' ', '\t']).len();
                let rest = &src[lead..];
                let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
                let marker = match rest[digits..].chars().next() {
                    Some('.' | ')') if digits > 0 => digits + 1,
                    Some('-' | '*' | '+') if digits == 0 => 1,
                    _ => 0,
                };
                if marker > 0 {
                    set(r.start + lead..r.start + lead + marker, &|l| l.list = true);
                }
            }
            Event::InlineHtml(h) => {
                if h.eq_ignore_ascii_case("<u>") {
                    underline = Some(r.clone());
                    set(r.clone(), &|l| l.markup = true);
                } else if h.eq_ignore_ascii_case("</u>") {
                    if let Some(open) = underline.take() {
                        set(open.end..r.start, &|l| l.underline = true);
                    }
                    set(r.clone(), &|l| l.markup = true);
                }
            }
            Event::End(TagEnd::Paragraph) => underline = None,
            _ => {}
        }
    }
    // Экранирование `\*`: обратная косая черта — разметка (в коде — текст); у `\\` первая — разметка, вторая — текст.
    let bytes = md.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'\\' && bytes[i + 1].is_ascii_punctuation() && !code.iter().any(|c| c.contains(&i)) {
            looks[i].markup = true;
            i += 2;
        } else {
            i += 1;
        }
    }
    spans(md, &looks)
}

/// Знаки по краям `r` шириной `n` — разметка.
fn wrap(set: &mut impl FnMut(Range<usize>, &dyn Fn(&mut Look)), r: &Range<usize>, n: usize) {
    if n == 0 || r.len() < 2 * n {
        return;
    }
    set(r.start..r.start + n, &|l| l.markup = true);
    set(r.end - n..r.end, &|l| l.markup = true);
}

/// Подряд идущие байты с одинаковыми признаками — один отрезок; границы — по символам.
fn spans(md: &str, looks: &[Look]) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for (i, _) in md.char_indices() {
        let look = looks[i];
        let end = md[i..].chars().next().map_or(i + 1, |c| i + c.len_utf8());
        match out.last_mut() {
            Some(s) if s.range.end == i && s.look == look => s.range.end = end,
            _ if look.is_plain() => {}
            _ => out.push(Span { range: i..end, look }),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Отрезки с признаком `f` — их текст.
    fn of(md: &str, f: impl Fn(&Look) -> bool) -> Vec<&str> {
        display(md).into_iter().filter(|s| f(&s.look)).map(|s| &md[s.range]).collect()
    }

    fn markup(md: &str) -> Vec<&str> {
        of(md, |l| l.markup)
    }

    #[test]
    fn inline_styles_and_their_markup() {
        let md = "Текст **жирный**, *курсив*, ~~зачёркнут~~ и `код`.";
        assert_eq!(of(md, |l| l.strong && !l.markup), ["жирный"]);
        assert_eq!(of(md, |l| l.emphasis && !l.markup), ["курсив"]);
        assert_eq!(of(md, |l| l.strike && !l.markup), ["зачёркнут"]);
        assert_eq!(of(md, |l| l.code && !l.markup), ["код"]);
        assert_eq!(markup(md), ["**", "**", "*", "*", "~~", "~~", "`", "`"]);
        // Вложенное: жирный курсив.
        let md = "***оба***";
        assert_eq!(of(md, |l| l.strong && l.emphasis && !l.markup), ["оба"]);
    }

    #[test]
    fn headings_quotes_and_links() {
        let md = "## Итоги\n\n> Цитата\n> вторая строка\n\nСм. [отчёт](https://corp.example/r) и <https://a.example>.";
        assert_eq!(of(md, |l| l.heading == 2 && !l.markup).first().map(|s| s.trim_end()), Some("Итоги"));
        assert!(markup(md).contains(&"## "));
        assert!(of(md, |l| l.quote && !l.markup).iter().any(|s| s.contains("Цитата")));
        assert_eq!(markup(md).iter().filter(|m| **m == "> ").count(), 2, "«> » у каждой строки");
        assert_eq!(of(md, |l| l.link && !l.markup), ["отчёт", "https://a.example"]);
        assert!(markup(md).contains(&"](https://corp.example/r)"));
    }

    /// Вложенные списки: маркер каждого пункта, нумерованные и с отступом.
    #[test]
    fn nested_lists() {
        let md = "- один\n  - вложенный\n    1. первый\n    2. второй\n- два\n";
        assert_eq!(of(md, |l| l.list), ["-", "-", "1.", "2.", "-"]);
    }

    /// `<u>…</u>` — подчёркнутый; теги — разметка.
    #[test]
    fn underline_tag() {
        let md = "Это <u>важно</u>, а это <u>нет";
        assert_eq!(of(md, |l| l.underline), ["важно"]);
        assert_eq!(markup(md), ["<u>", "</u>", "<u>"]);
    }

    /// Экранированный знак — текст, обратная косая черта приглушена; в коде — как есть.
    #[test]
    fn escapes() {
        let md = r"Не \*курсив\* и `a\*b`";
        assert!(of(md, |l| l.emphasis).is_empty());
        assert_eq!(markup(md).iter().filter(|m| **m == "\\").count(), 2);
        let md = r"путь C:\\temp";
        assert_eq!(markup(md), ["\\"], "первая косая экранирует вторую");
    }

    /// Незакрытые маркеры — обычный текст (как при отправке).
    #[test]
    fn unclosed_markers_stay_plain() {
        for md in ["**жирный без конца", "*курсив", "`код", "[ссылка](без конца", "~~"] {
            assert!(display(md).iter().all(|s| !s.look.strong && !s.look.emphasis && !s.look.code && !s.look.link), "{md}");
        }
        assert!(display("Просто текст").is_empty());
    }

    /// Отрезки по порядку, не пересекаются, по границам символов.
    #[test]
    fn spans_are_ordered_and_char_aligned() {
        let md = "## Заголовок\n\n**Ж** *к* [с](https://x.example) `к` <u>п</u>\n\n> ц\n\n- п";
        let s = display(md);
        for w in s.windows(2) {
            assert!(w[0].range.end <= w[1].range.start);
        }
        for x in &s {
            assert!(md.is_char_boundary(x.range.start) && md.is_char_boundary(x.range.end));
        }
    }
}
