//! Письмо во вложении (S8-20): `.eml` и вложенное `message/rfc822` — разбором `mail-parser`, `.msg` Outlook — [`msg`].
//! Только здесь, в песочнице.
//!
//! Шапка — данными (от, кому, копия, дата, тема; подписи — у интерфейса), тело — тем же путём HTML → IR, что тело
//! письма (внешние картинки не грузятся, ссылки — через проверку IR; без HTML — текст), вложения — списком: имя,
//! MIME, размер (открыть вложенное вложение — не в S8). Писем друг в друге — не глубже [`MAX_NESTING`] (глубже —
//! ошибка), блоков тела — как у документа, вложений и адресов — не больше [`MAX_LISTED`].

use mail_parser::{Encoding, MimeHeaders, PartType};
use postman_ir::Block;

use super::docx::MAX_BLOCKS;
use super::{MAX_LISTED, OLE_MAGIC, Preview};
use crate::mime::{Addr, addrs};

/// Уровней писем друг в друге, считая показываемое.
pub const MAX_NESTING: usize = 3;

/// Письмо `.eml` или `.msg` (по содержимому) → шапка, тело IR и список вложений.
pub fn letter(bytes: &[u8]) -> Result<Preview, String> {
    if bytes.starts_with(OLE_MAGIC) {
        return super::msg::msg(bytes);
    }
    eml(bytes)
}

pub(super) fn too_deep() -> String {
    format!("письма вложены глубже {MAX_NESTING} уровней")
}

fn eml(bytes: &[u8]) -> Result<Preview, String> {
    let msg = mail_parser::MessageParser::default().parse(bytes).ok_or("не письмо RFC 5322")?;
    if msg.parts.first().is_none_or(|p| p.headers.is_empty()) {
        return Err("не письмо RFC 5322".into());
    }
    if depth(&msg) > MAX_NESTING {
        return Err(too_deep());
    }
    let doc = match msg.html_body.first().and_then(|&i| msg.parts.get(i as usize)).map(|p| &p.body) {
        Some(PartType::Html(html)) => postman_ir::from_html(html),
        _ => msg.body_text(0).map(|t| postman_ir::from_plain(&t)).unwrap_or_default(),
    };
    let (blocks, mut truncated) = cut(doc.blocks);
    let mut attachments = Vec::new();
    for a in msg.attachments() {
        if attachments.len() >= MAX_LISTED {
            truncated = true;
            break;
        }
        let nested = match &a.body {
            PartType::Message(m) if !m.parts.is_empty() => Some(m),
            _ => None,
        };
        let name = a.attachment_name().or_else(|| nested.and_then(|m| m.subject())).unwrap_or_default();
        let mime = match a.content_type() {
            Some(c) => format!("{}/{}", c.ctype(), c.subtype().unwrap_or("octet-stream")).to_ascii_lowercase(),
            None if nested.is_some() => "message/rfc822".into(),
            None => "application/octet-stream".into(),
        };
        // Вложенное письмо без кодировки: разборщик хранит всё внешнее письмо — размер по границам части.
        let size = match (&a.body, a.encoding) {
            (PartType::Message(_), Encoding::None) => a.offset_end.saturating_sub(a.offset_body) as usize,
            _ => a.contents().len(),
        };
        attachments.push((clean(name), clean(&mime), size as u64));
    }
    Ok(Preview::Letter {
        from: people(addrs(msg.from())),
        to: people(addrs(msg.to())),
        cc: people(addrs(msg.cc())),
        date: msg.date().map(|d| d.to_timestamp()),
        subject: clean(msg.subject().unwrap_or_default()),
        blocks,
        attachments,
        truncated,
    })
}

/// Уровней писем друг в друге (само письмо — первый); обход без рекурсии.
fn depth(msg: &mail_parser::Message<'_>) -> usize {
    let mut max = 1;
    let mut stack = vec![(msg, 1usize)];
    while let Some((m, d)) = stack.pop() {
        max = max.max(d);
        if d > MAX_NESTING {
            break;
        }
        for p in &m.parts {
            if let PartType::Message(inner) = &p.body {
                stack.push((inner, d + 1));
            }
        }
    }
    max
}

/// Блоков тела — не больше, чем у документа.
pub(super) fn cut(mut blocks: Vec<Block>) -> (Vec<Block>, bool) {
    let cut = blocks.len() > MAX_BLOCKS;
    blocks.truncate(MAX_BLOCKS);
    (blocks, cut)
}

/// Строка шапки: без управляющих и bidi-символов, не длиннее 1000 знаков.
pub(super) fn clean(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() && !matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
        .take(1000)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Адреса шапки: очищенные, не больше [`MAX_LISTED`].
pub(super) fn people(list: Vec<Addr>) -> Vec<Addr> {
    list.into_iter().take(MAX_LISTED).map(|a| Addr { name: clean(&a.name), email: clean(&a.email) }).collect()
}

#[cfg(test)]
mod tests {
    use postman_ir::{Align, Inline, Marks};

    use super::*;
    use crate::preview::{PreviewKind, preview};

    const EML: &str = "From: =?utf-8?B?0JDQvdC90LA=?= <anna@example.test>\r\nTo: =?utf-8?B?0J/RkdGC0YA=?= <petr@example.test>, ivan@example.test\r\nCc: team@example.test\r\nDate: Tue, 21 Oct 2025 14:01:00 +0300\r\nSubject: =?utf-8?B?0J7RgtGH0ZHRgg==?=\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b1\"\r\n\r\n--b1\r\nContent-Type: multipart/alternative; boundary=\"b2\"\r\n\r\n--b2\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nПривет\r\n--b2\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>Привет, <b>мир</b> <a href=\"javascript:alert(1)\">x</a><img src=\"https://tracker.example/p.gif\"></p>\r\n--b2--\r\n--b1\r\nContent-Type: application/pdf; name=\"report.pdf\"\r\nContent-Disposition: attachment; filename=\"report.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\nSGVsbG8=\r\n--b1--\r\n";

    #[test]
    fn eml_with_html_and_attachment() {
        let (pv, px) = preview(PreviewKind::Letter, EML.as_bytes(), 0, 0, 0).unwrap();
        assert!(px.is_empty());
        let Preview::Letter { from, to, cc, date, subject, blocks, attachments, truncated } = pv else { panic!("{pv:?}") };
        assert_eq!(from, [Addr { name: "Анна".into(), email: "anna@example.test".into() }]);
        assert_eq!(to.iter().map(|a| a.email.as_str()).collect::<Vec<_>>(), ["petr@example.test", "ivan@example.test"]);
        assert_eq!(to[0].name, "Пётр");
        assert_eq!(cc[0].email, "team@example.test");
        assert_eq!(date, Some(1_761_044_460));
        assert_eq!(subject, "Отчёт");
        let bold = Marks { bold: true, ..Marks::default() };
        assert_eq!(
            blocks,
            [Block::Paragraph {
                align: Align::Start,
                content: vec![Inline::text("Привет, "), Inline::Text { text: "мир".into(), marks: bold }, Inline::text(" "), Inline::text("x")]
            }]
        );
        assert_eq!(attachments, [("report.pdf".to_string(), "application/pdf".to_string(), 5)]);
        assert!(!truncated);
    }

    /// Письмо в письме: вложение — списком с темой вместо имени; глубже [`MAX_NESTING`] — ошибка.
    #[test]
    fn nested_letters_up_to_the_limit() {
        let wrap = |inner: &str, subject: &str| {
            format!(
                "From: a@example.test\r\nSubject: {subject}\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"{subject}\"\r\n\r\n--{subject}\r\nContent-Type: text/plain\r\n\r\nтело {subject}\r\n--{subject}\r\nContent-Type: message/rfc822\r\n\r\n{inner}\r\n--{subject}--\r\n"
            )
        };
        let leaf = "From: c@example.test\r\nSubject: leaf\r\n\r\nлист\r\n";
        let two = wrap(leaf, "two");
        let three = wrap(&two, "three");
        let Preview::Letter { subject, blocks, attachments, .. } = letter(three.as_bytes()).unwrap() else { panic!() };
        assert_eq!(subject, "three");
        assert_eq!(blocks, [Block::Paragraph { align: Align::Start, content: vec![Inline::text("тело three")] }]);
        assert_eq!(attachments.len(), 1);
        assert_eq!((attachments[0].0.as_str(), attachments[0].1.as_str()), ("two", "message/rfc822"));
        assert!(attachments[0].2 > 0 && (attachments[0].2 as usize) < three.len() - 100, "{}", attachments[0].2);
        // Письмо в письме в письме в письме.
        let four = wrap(&three, "four");
        assert_eq!(letter(four.as_bytes()).unwrap_err(), too_deep());
    }

    #[test]
    fn broken_eml_is_an_error() {
        assert!(letter(b"").is_err());
        assert!(letter(b"\r\n\r\njunk").is_err());
        // Мусор разборщик принимает за письмо без заголовков — без паники.
        let _ = letter(&[0xFF; 100]);
    }
}
