//! Имена и роли папок, заголовки и структура письма: SPECIAL-USE (RFC 6154), иначе — по имени; имена папок —
//! modified UTF-7 (RFC 3501 §5.1.3); тема и имя отправителя — encoded words (RFC 2047).

use async_imap::imap_proto::BodyStructure;

use crate::Folder;

/// Роль папки — как `Mailbox.role` JMAP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Inbox,
    Sent,
    Drafts,
    Trash,
    Junk,
    Archive,
}

impl Role {
    /// Имя роли JMAP (`inbox`, `sent`…): так её знает кэш Postman.
    pub fn jmap(self) -> &'static str {
        match self {
            Role::Inbox => "inbox",
            Role::Sent => "sent",
            Role::Drafts => "drafts",
            Role::Trash => "trash",
            Role::Junk => "junk",
            Role::Archive => "archive",
        }
    }
}

/// Признак папки из `LIST`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attr {
    Role(Role),
    NoSelect,
    Other,
}

/// Папка из строки `LIST`: имя, родитель по разделителю, роль (SPECIAL-USE, иначе по имени).
pub fn folder(raw: &str, delimiter: Option<&str>, attrs: &[Attr]) -> Folder {
    let decoded = decode_utf7(raw);
    let (parent, name) = match delimiter.filter(|d| !d.is_empty()) {
        Some(d) => match (raw.rsplit_once(d), decoded.rsplit_once(d)) {
            (Some((p, _)), Some((_, leaf))) => (Some(p.to_owned()), leaf.to_owned()),
            _ => (None, decoded.clone()),
        },
        None => (None, decoded.clone()),
    };
    let special = attrs.iter().find_map(|a| match a {
        Attr::Role(r) => Some(*r),
        _ => None,
    });
    Folder { raw: raw.to_owned(), name, parent, role: special.or_else(|| role_by_name(raw, &decoded)), selectable: !attrs.contains(&Attr::NoSelect) }
}

/// Роль по имени папки (без SPECIAL-USE): «Входящие» — всегда `INBOX`; остальное — частые имена en/ru верхнего
/// уровня или под `INBOX`.
pub fn role_by_name(raw: &str, decoded: &str) -> Option<Role> {
    if raw.eq_ignore_ascii_case("INBOX") {
        return Some(Role::Inbox);
    }
    let leaf = decoded.rsplit(['/', '.']).next().unwrap_or(decoded).to_lowercase();
    let role = match leaf.as_str() {
        "sent" | "sent items" | "sent messages" | "sent mail" | "отправленные" => Role::Sent,
        "drafts" | "draft" | "черновики" => Role::Drafts,
        "trash" | "deleted" | "deleted items" | "deleted messages" | "bin" | "удалённые" | "удаленные" | "корзина" => {
            Role::Trash
        }
        "junk" | "spam" | "junk e-mail" | "junk email" | "спам" | "нежелательная почта" => Role::Junk,
        "archive" | "archives" | "архив" => Role::Archive,
        _ => return None,
    };
    Some(role)
}

/// Имя папки из modified UTF-7 (RFC 3501 §5.1.3): `&BB4EQgQ,BEAEMAQyBDsENQQ9BD0ESwQ1-` → «Отправленные»,
/// `&-` → `&`. Испорченная последовательность остаётся как есть.
pub fn decode_utf7(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let Some(end) = after.find('-') else {
            out.push_str(&rest[i..]);
            return out;
        };
        let chunk = &after[..end];
        if chunk.is_empty() {
            out.push('&');
        } else {
            match decode_b64_utf16(chunk) {
                Some(text) => out.push_str(&text),
                None => out.push_str(&rest[i..i + 2 + end]),
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Имя папки в modified UTF-7 (RFC 3501 §5.1.3) для команд: печатный ASCII — как есть, `&` → `&-`, остальное —
/// `&` + base64 (`,` вместо `/`, без `=`) от UTF-16BE + `-`.
pub fn encode_utf7(s: &str) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,";
    fn flush(out: &mut String, units: &mut Vec<u16>) {
        if units.is_empty() {
            return;
        }
        let bytes: Vec<u8> = units.drain(..).flat_map(u16::to_be_bytes).collect();
        out.push('&');
        let (mut bits, mut n) = (0u32, 0);
        for b in bytes {
            bits = (bits << 8) | u32::from(b);
            n += 8;
            while n >= 6 {
                n -= 6;
                out.push(char::from(ALPHABET[((bits >> n) & 0x3f) as usize]));
            }
        }
        if n > 0 {
            out.push(char::from(ALPHABET[((bits << (6 - n)) & 0x3f) as usize]));
        }
        out.push('-');
    }
    let mut out = String::with_capacity(s.len());
    let mut units = Vec::new();
    for c in s.chars() {
        if (' '..='~').contains(&c) {
            flush(&mut out, &mut units);
            if c == '&' { out.push_str("&-") } else { out.push(c) }
        } else {
            let mut buf = [0u16; 2];
            units.extend_from_slice(c.encode_utf16(&mut buf));
        }
    }
    flush(&mut out, &mut units);
    out
}

/// Имя папки на сервере: родитель (`raw`, уже в UTF-7) + разделитель + имя в UTF-7; без родителя — верхний уровень.
/// Имя с разделителем внутри или пустое — `None`: сервер понял бы его как путь.
pub fn folder_path(parent: Option<&str>, name: &str, delimiter: Option<&str>) -> Option<String> {
    let name = name.trim();
    let d = delimiter.filter(|d| !d.is_empty());
    if name.is_empty() || d.is_some_and(|d| name.contains(d)) {
        return None;
    }
    let leaf = encode_utf7(name);
    match (parent, d) {
        (Some(p), Some(d)) => Some(format!("{p}{d}{leaf}")),
        // Без разделителя иерархии нет: только верхний уровень.
        (Some(_), None) => None,
        (None, _) => Some(leaf),
    }
}

/// `CREATE` папки (RFC 3501 §6.3.3) по имени на сервере.
pub fn create_command(path: &str) -> String {
    format!("CREATE {}", crate::quote(path))
}

/// `RENAME` папки (RFC 3501 §6.3.5): переименование и перенос — новый путь.
pub fn rename_command(from: &str, to: &str) -> String {
    format!("RENAME {} {}", crate::quote(from), crate::quote(to))
}

fn decode_b64_utf16(chunk: &str) -> Option<String> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,";
    let mut bits: u32 = 0;
    let mut n = 0;
    let mut bytes = Vec::new();
    for c in chunk.bytes() {
        let v = ALPHABET.iter().position(|&a| a == c)? as u32;
        bits = (bits << 6) | v;
        n += 6;
        if n >= 8 {
            n -= 8;
            bytes.push((bits >> n) as u8);
            bits &= (1 << n) - 1;
        }
    }
    if bytes.len() % 2 != 0 {
        return None;
    }
    let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|p| u16::from_be_bytes(*p)).collect();
    String::from_utf16(&units).ok()
}

/// Значение заголовка с encoded words (RFC 2047) → текст: разбор — `mail-parser` (тот же, что у компоновщика и
/// S/MIME), только одной строки заголовка, не письма.
pub fn decode_header(value: &str) -> String {
    if !value.contains("=?") {
        return value.to_owned();
    }
    let raw = format!("Subject: {value}\r\n\r\n");
    mail_parser::MessageParser::new().parse_headers(raw.as_bytes()).and_then(|m| m.subject().map(str::to_owned)).unwrap_or_else(|| value.to_owned())
}

/// Есть ли у письма вложение: часть с `Content-Disposition: attachment` или не текст и не multipart/alternative
/// среди частей `multipart/mixed`.
pub fn has_attachment(b: &BodyStructure<'_>) -> bool {
    match b {
        BodyStructure::Basic { common, .. } | BodyStructure::Text { common, .. } | BodyStructure::Message { common, .. } => {
            common.disposition.as_ref().is_some_and(|d| d.ty.eq_ignore_ascii_case("attachment"))
        }
        BodyStructure::Multipart { common, bodies, .. } => {
            let mixed = common.ty.subtype.eq_ignore_ascii_case("mixed");
            bodies.iter().any(|p| has_attachment(p) || (mixed && matches!(p, BodyStructure::Basic { .. } | BodyStructure::Message { .. })))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Attr, Role, create_command, decode_header, decode_utf7, encode_utf7, folder, folder_path, rename_command, role_by_name};

    #[test]
    fn utf7_names() {
        assert_eq!(decode_utf7("INBOX"), "INBOX");
        assert_eq!(decode_utf7("&BB4EQgQ,BEAEMAQyBDsENQQ9BD0ESwQ1-"), "Отправленные");
        assert_eq!(decode_utf7("A &- B"), "A & B");
        assert_eq!(decode_utf7("broken &AB"), "broken &AB");
    }

    #[test]
    fn utf7_encode() {
        assert_eq!(encode_utf7("INBOX"), "INBOX");
        assert_eq!(encode_utf7("Отправленные"), "&BB4EQgQ,BEAEMAQyBDsENQQ9BD0ESwQ1-");
        assert_eq!(encode_utf7("A & B"), "A &- B");
        for s in ["Проект 2026", "日本語", "смесь & mix 🙂", "~peter/mail/台北/日本語"] {
            assert_eq!(decode_utf7(&encode_utf7(s)), s);
            assert!(encode_utf7(s).is_ascii());
        }
        // RFC 3501 §5.1.3, пример.
        assert_eq!(encode_utf7("~peter/mail/台北/日本語"), "~peter/mail/&U,BTFw-/&ZeVnLIqe-");
    }

    /// Создать и перенести папку (T1-04а): путь с разделителем ящика, имя — в UTF-7, команды в кавычках.
    #[test]
    fn create_and_rename_commands() {
        let path = folder_path(Some("INBOX"), "Проект", Some(".")).unwrap();
        assert_eq!(path, format!("INBOX.{}", encode_utf7("Проект")));
        assert_eq!(create_command(&path), format!("CREATE \"{path}\""));
        assert_eq!(folder_path(None, "Отчёты", Some("/")).as_deref(), Some(encode_utf7("Отчёты").as_str()));
        let moved = folder_path(Some("Archive"), "Проект", Some("/")).unwrap();
        assert_eq!(rename_command(&path, &moved), format!("RENAME \"{path}\" \"{moved}\""));
        assert_eq!(rename_command("A \"q\"", "B"), r#"RENAME "A \"q\"" "B""#, "кавычки экранированы");
        assert_eq!(folder_path(Some("INBOX"), "a.b", Some(".")), None, "разделитель в имени");
        assert_eq!(folder_path(None, "  ", Some(".")), None, "пустое имя");
        assert_eq!(folder_path(Some("INBOX"), "x", None), None, "без разделителя — только верхний уровень");
    }

    #[test]
    fn roles_from_special_use_or_name() {
        assert_eq!(role_by_name("INBOX", "INBOX"), Some(Role::Inbox));
        assert_eq!(role_by_name("x", "Отправленные"), Some(Role::Sent));
        assert_eq!(role_by_name("x", "INBOX.Trash"), Some(Role::Trash));
        assert_eq!(role_by_name("x", "Projects"), None);
        let f = folder("Mail/Old", Some("/"), &[Attr::Role(Role::Archive), Attr::Other]);
        assert_eq!((f.parent.as_deref(), f.name.as_str(), f.role, f.selectable), (Some("Mail"), "Old", Some(Role::Archive), true));
        let root = folder("[Gmail]", Some("/"), &[Attr::NoSelect]);
        assert!(!root.selectable && root.parent.is_none());
        assert_eq!(Role::Junk.jmap(), "junk");
    }

    #[test]
    fn encoded_words() {
        assert_eq!(decode_header("Plain subject"), "Plain subject");
        assert_eq!(decode_header("=?utf-8?B?0J7RgtGH0ZHRgg==?="), "Отчёт");
        assert_eq!(decode_header("=?koi8-r?B?8NLJ18XU?="), "Привет");
    }
}
