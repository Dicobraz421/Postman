//! IR -> Markdown (цитирование, text/plain) и Markdown -> HTML из фиксированного набора тегов.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd, html};

use crate::{Block, Document, Inline, Marks};

pub fn to_markdown(doc: &Document) -> String {
    let mut out = String::new();
    write_blocks(&doc.blocks, &mut out);
    out
}

/// Цитата для ответа: строка-заголовок («… написал(а):») и тело с `> `.
pub fn quote(doc: &Document, header: &str) -> String {
    let md = to_markdown(doc);
    let mut out = String::from(header);
    out.push('\n');
    for line in md.lines() {
        out.push_str(if line.is_empty() { ">" } else { "> " });
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn write_blocks(blocks: &[Block], out: &mut String) {
    for (n, b) in blocks.iter().enumerate() {
        if n > 0 {
            out.push_str("\n\n");
        }
        write_block(b, out);
    }
}

fn write_block(b: &Block, out: &mut String) {
    match b {
        Block::Paragraph { content, .. } => write_inlines(content, out),
        Block::Heading { level, content } => {
            out.push_str(&"#".repeat(*level as usize));
            out.push(' ');
            let mut s = String::new();
            write_inlines(content, &mut s);
            out.push_str(&s.replace('\n', " ").replace("  ", " "));
        }
        Block::Quote(inner) => {
            let mut s = String::new();
            write_blocks(inner, &mut s);
            prefix_lines(&s, "> ", "> ", out);
        }
        Block::List { ordered, items } => {
            for (n, item) in items.iter().enumerate() {
                if n > 0 {
                    out.push('\n');
                }
                let marker = if *ordered { format!("{}. ", n + 1) } else { "- ".to_string() };
                let mut s = String::new();
                write_blocks(item, &mut s);
                let indent = " ".repeat(marker.len());
                prefix_lines(&s, &marker, &indent, out);
            }
        }
        Block::Code(code) => {
            let fence = "`".repeat(longest_run(code, '`').max(2) + 1);
            out.push_str(&fence);
            out.push('\n');
            out.push_str(code);
            out.push('\n');
            out.push_str(&fence);
        }
        Block::Rule => out.push_str("---"),
        Block::Figure => {}
        Block::Table(rows) => {
            let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
            for (n, row) in rows.iter().enumerate() {
                out.push('|');
                for c in 0..cols {
                    let mut s = String::new();
                    if let Some(cell) = row.get(c) {
                        write_inlines(cell, &mut s);
                    }
                    out.push(' ');
                    out.push_str(&s.replace('\n', " ").replace("  ", " ").replace('|', "\\|"));
                    out.push_str(" |");
                }
                out.push('\n');
                if n == 0 {
                    out.push('|');
                    out.push_str(&" --- |".repeat(cols));
                    out.push('\n');
                }
            }
            out.pop();
        }
    }
    trim_trailing_newlines(out);
}

fn prefix_lines(s: &str, first: &str, rest: &str, out: &mut String) {
    for (n, line) in s.lines().enumerate() {
        if n > 0 {
            out.push('\n');
        }
        let p = if n == 0 { first } else { rest };
        if line.is_empty() {
            out.push_str(p.trim_end());
        } else {
            out.push_str(p);
            out.push_str(line);
        }
    }
}

fn write_inlines(inlines: &[Inline], out: &mut String) {
    for i in inlines {
        match i {
            Inline::Text { text, marks } => write_text(text, *marks, out),
            Inline::LineBreak => out.push_str("  \n"),
            Inline::Image { cid, alt, .. } => {
                out.push_str("![");
                out.push_str(&escape(alt));
                out.push_str("](cid:");
                out.push_str(cid);
                out.push(')');
            }
            Inline::Link { href, content, .. } => {
                let visible = crate::plain_text(content);
                let bare = href.strip_prefix("mailto:").unwrap_or(href);
                if visible == *href || visible == bare || format!("{visible}/") == *href {
                    out.push('<');
                    out.push_str(href);
                    out.push('>');
                } else {
                    out.push('[');
                    write_inlines(content, out);
                    out.push_str("](");
                    out.push_str(&href.replace(' ', "%20").replace(')', "%29"));
                    out.push(')');
                }
            }
        }
    }
}

fn write_text(text: &str, m: Marks, out: &mut String) {
    if m.code {
        let fence = "`".repeat(longest_run(text, '`') + 1);
        out.push_str(&fence);
        out.push_str(text);
        out.push_str(&fence);
        return;
    }
    // Разметка не может начинаться/заканчиваться пробелом — выносим его наружу.
    let core = text.trim();
    if core.is_empty() {
        out.push_str(text);
        return;
    }
    let lead = &text[..text.len() - text.trim_start().len()];
    let trail = &text[text.trim_end().len()..];
    let mut open = String::new();
    if m.bold {
        open.push_str("**");
    }
    if m.italic {
        open.push('*');
    }
    if m.strike {
        open.push_str("~~");
    }
    let close: String = open.chars().rev().collect();
    out.push_str(lead);
    out.push_str(&open);
    out.push_str(&escape(core));
    out.push_str(&close);
    out.push_str(trail);
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '*' | '_' | '`' | '[' | ']' | '<' | '~') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn longest_run(s: &str, ch: char) -> usize {
    let (mut best, mut cur) = (0, 0);
    for c in s.chars() {
        cur = if c == ch { cur + 1 } else { 0 };
        best = best.max(cur);
    }
    best
}

fn trim_trailing_newlines(s: &mut String) {
    while s.ends_with('\n') {
        s.pop();
    }
}

/// Markdown композера -> HTML исходящего письма. Сырой HTML экранируется (кроме `<u>`/`</u>`: подчёркивания в
/// Markdown нет, кнопка «Ч» композера вставляет их), ссылки — только http(s)/mailto, картинки — только cid:.
pub fn markdown_to_html(md: &str) -> String {
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let mut link_stack: Vec<bool> = Vec::new();
    let mut image_depth = 0usize;
    let events = Parser::new_ext(md, opts).filter_map(|ev| match ev {
        Event::InlineHtml(s) if s.eq_ignore_ascii_case("<u>") || s.eq_ignore_ascii_case("</u>") => Some(Event::InlineHtml(s)),
        Event::Html(s) | Event::InlineHtml(s) => Some(Event::Text(s)),
        Event::Start(Tag::Link { link_type, dest_url, title, id }) => {
            let ok = safe_link(&dest_url);
            link_stack.push(ok);
            ok.then_some(Event::Start(Tag::Link { link_type, dest_url, title, id }))
        }
        Event::End(TagEnd::Link) => link_stack.pop().unwrap_or(false).then_some(Event::End(TagEnd::Link)),
        // Внешняя картинка: остаётся только alt-текст.
        Event::Start(Tag::Image { dest_url, .. }) if !dest_url.starts_with("cid:") => {
            image_depth += 1;
            None
        }
        Event::End(TagEnd::Image) if image_depth > 0 => {
            image_depth -= 1;
            None
        }
        ev => Some(ev),
    });
    let mut out = String::new();
    html::push_html(&mut out, events);
    out
}

fn safe_link(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{from_html, from_plain};

    #[test]
    fn roundtrip_markup() {
        let d = from_html(
            r#"<p><b>Важно</b>: см. <a href="https://ex.com/a">отчёт</a> и <a href="https://ex.com/">https://ex.com</a></p><ul><li>раз</li><li>два<br>строка</li></ul><blockquote><p>q1</p><p>q2</p></blockquote>"#,
        );
        let md = to_markdown(&d);
        assert_eq!(md, "**Важно**: см. [отчёт](https://ex.com/a) и <https://ex.com/>\n\n- раз\n- два  \n  строка\n\n> q1\n>\n> q2");
    }

    #[test]
    fn quote_prefixes_every_line() {
        let q = quote(&from_plain("a\n\nb"), "Иван написал(а):");
        assert_eq!(q, "Иван написал(а):\n> a\n>\n> b\n");
    }

    #[test]
    fn outgoing_html_is_sanitized() {
        let h = markdown_to_html("<script>x</script>\n\n[a](javascript:x) [b](https://ok.ru) ![t](https://track/p.gif) ![l](cid:logo)");
        assert!(!h.contains("<script"));
        assert!(!h.contains("href=\"javascript"));
        assert!(h.contains(r#"href="https://ok.ru""#));
        assert!(!h.contains("track/p.gif"));
        assert!(h.contains(r#"src="cid:logo""#));
    }

    /// Подчёркивание композера (S5-06б): `<u>` проходит, остальной HTML и атрибуты — текстом.
    #[test]
    fn underline_passes_and_other_html_does_not() {
        let h = markdown_to_html("a <u>b</u> <u onclick=x>c</u> <b>d</b>");
        assert!(h.contains("<u>b</u>"), "{h}");
        assert!(!h.contains("<u onclick"), "{h}");
        assert!(!h.contains("<b>"), "{h}");
        assert!(format!("{:?}", from_html(&h)).contains("underline: true"));
    }
}
