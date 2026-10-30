//! Письмо целиком (RFC 5322) → свойства MAPI для MSG и PST (S8-29): заголовки, тело, получатели и вложения чужого
//! письма разбираются только здесь, в песочнице; типы — `postman_export::mapi`, файл собирает `export` (S8-30, S8-33).
//!
//! Класс — `IPM.Note`; подписанное `multipart/signed` — `IPM.Note.SMIME.MultipartSigned` с одним вложением `SMIME.p7m`
//! (всё письмо, тело — из подписанного), `application/pkcs7-mime` (зашифрованное или подписанное целиком) —
//! `IPM.Note.SMIME` с одним вложением `smime.p7m` (CMS без кодировки передачи, тела нет; MS-OXOSMIME 2.1.3).
//! Пределы — как у [`crate::mime::split`]: письмо — [`MAX_SPLIT`], частей во всём дереве — [`MAX_SPLIT_PARTS`];
//! вложенные письма — не глубже [`MAX_NESTING`], глубже — вложением `message/rfc822` байтами.

use mail_parser::{HeaderValue, Message, MessageParser, MimeHeaders, PartType};
use postman_export::mapi::{
    ADDRTYPE_SMTP, ATTACH_BY_VALUE, ATTACH_EMBEDDED_MSG, Attachment, CP_UTF8, FILETIME_UNIX_EPOCH, IPM_NOTE, IPM_NOTE_SMIME,
    IPM_NOTE_SMIME_MULTIPART_SIGNED, MAPI_BCC, MAPI_CC, MAPI_TO, MSGFLAG_READ, MapiMessage, PR_ADDRTYPE_W, PR_ATTACH_CONTENT_ID_W,
    PR_ATTACH_DATA_BIN, PR_ATTACH_LONG_FILENAME_W, PR_ATTACH_METHOD, PR_ATTACH_MIME_TAG_W, PR_BODY_W, PR_CLIENT_SUBMIT_TIME, PR_DISPLAY_BCC_W,
    PR_DISPLAY_CC_W, PR_DISPLAY_NAME_W, PR_DISPLAY_TO_W, PR_EMAIL_ADDRESS_W, PR_HTML, PR_IN_REPLY_TO_ID_W, PR_INTERNET_CPID,
    PR_INTERNET_MESSAGE_ID_W, PR_MESSAGE_DELIVERY_TIME, PR_MESSAGE_FLAGS, PR_NORMALIZED_SUBJECT_W, PR_RECIPIENT_TYPE, PR_SENDER_ADDRTYPE_W,
    PR_SENDER_EMAIL_ADDRESS_W, PR_SENDER_NAME_W, PR_SENDER_SMTP_ADDRESS_W, PR_SENT_REPRESENTING_ADDRTYPE_W, PR_SENT_REPRESENTING_EMAIL_ADDRESS_W,
    PR_SENT_REPRESENTING_NAME_W, PR_SENT_REPRESENTING_SMTP_ADDRESS_W, PR_SMTP_ADDRESS_W, PR_SUBJECT_PREFIX_W, PR_SUBJECT_W,
    PR_TRANSPORT_MESSAGE_HEADERS_W, PT_BINARY, PT_BOOLEAN, PT_LONG, PT_SYSTIME, PT_UNICODE, Recipient, Tag, Value,
};

use crate::mime::{Addr, MAX_SPLIT, MAX_SPLIT_PARTS, addrs, is_message_id};

/// Вложенных писем — не глубже (письмо в письме в письме).
pub const MAX_NESTING: usize = 3;

/// Письмо целиком → [`MapiMessage`]; `seen` — прочитано (`PR_MESSAGE_FLAGS`), вложенные письма — прочитаны.
pub fn mapi(raw: &[u8], seen: bool) -> Result<MapiMessage, String> {
    if raw.len() > MAX_SPLIT {
        return Err("письмо слишком большое".into());
    }
    let msg = MessageParser::default().parse(raw).ok_or("не письмо RFC 5322")?;
    if msg.parts.first().is_none_or(|p| p.headers.is_empty()) {
        return Err("не письмо RFC 5322".into());
    }
    let mut count = 0;
    message(&msg, seen, 0, &mut count)
}

/// Как письмо защищено S/MIME (по типу корня).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Smime {
    None,
    /// `multipart/signed`.
    Signed,
    /// `application/pkcs7-mime` (зашифрованное или подписанное целиком) — тип корня для вложения.
    Opaque(&'static str),
}

fn smime(msg: &Message<'_>) -> Smime {
    let Some(ct) = msg.content_type() else { return Smime::None };
    let sub = ct.subtype().unwrap_or_default();
    match (ct.ctype().to_ascii_lowercase().as_str(), sub.to_ascii_lowercase().as_str()) {
        ("multipart", "signed") => Smime::Signed,
        ("application", "pkcs7-mime") => Smime::Opaque("application/pkcs7-mime"),
        ("application", "x-pkcs7-mime") => Smime::Opaque("application/x-pkcs7-mime"),
        _ => Smime::None,
    }
}

fn message(msg: &Message<'_>, seen: bool, depth: usize, count: &mut usize) -> Result<MapiMessage, String> {
    *count += msg.parts.len();
    if *count > MAX_SPLIT_PARTS {
        return Err(format!("больше {MAX_SPLIT_PARTS} частей"));
    }
    let kind = smime(msg);
    let class = match kind {
        Smime::None => IPM_NOTE,
        Smime::Signed => IPM_NOTE_SMIME_MULTIPART_SIGNED,
        Smime::Opaque(_) => IPM_NOTE_SMIME,
    };
    let mut props: Vec<(Tag, Value)> = Vec::new();
    let text = |tag: Tag, s: &str| (tag, Value::Unicode(s.to_owned()));

    let subject = msg.subject().unwrap_or_default();
    let (prefix, normalized) = subject_prefix(subject);
    props.push(text(PR_SUBJECT_W, subject));
    props.push(text(PR_SUBJECT_PREFIX_W, prefix));
    props.push(text(PR_NORMALIZED_SUBJECT_W, normalized));

    let from = addrs(msg.from()).into_iter().next();
    let sender = addrs(msg.sender()).into_iter().next().or_else(|| from.clone());
    if let Some(a) = &sender {
        props.extend([text(PR_SENDER_NAME_W, display(a)), text(PR_SENDER_ADDRTYPE_W, ADDRTYPE_SMTP)]);
        props.extend([text(PR_SENDER_EMAIL_ADDRESS_W, &a.email), text(PR_SENDER_SMTP_ADDRESS_W, &a.email)]);
    }
    if let Some(a) = &from {
        props.extend([text(PR_SENT_REPRESENTING_NAME_W, display(a)), text(PR_SENT_REPRESENTING_ADDRTYPE_W, ADDRTYPE_SMTP)]);
        props.extend([text(PR_SENT_REPRESENTING_EMAIL_ADDRESS_W, &a.email), text(PR_SENT_REPRESENTING_SMTP_ADDRESS_W, &a.email)]);
    }

    let to = addrs(msg.to());
    let cc = addrs(msg.cc());
    let bcc = addrs(msg.bcc());
    let names = |list: &[Addr]| list.iter().map(display).collect::<Vec<_>>().join("; ");
    props.push(text(PR_DISPLAY_TO_W, &names(&to)));
    props.push(text(PR_DISPLAY_CC_W, &names(&cc)));
    if !bcc.is_empty() {
        props.push(text(PR_DISPLAY_BCC_W, &names(&bcc)));
    }

    let date = msg.date().and_then(|d| filetime(d.to_timestamp()));
    let delivered = msg.received_all().next().and_then(|r| r.date()).and_then(|d| filetime(d.to_timestamp())).or(date);
    if let Some(t) = date {
        props.push((PR_CLIENT_SUBMIT_TIME, Value::SysTime(t)));
    }
    if let Some(t) = delivered {
        props.push((PR_MESSAGE_DELIVERY_TIME, Value::SysTime(t)));
    }
    if let Some(id) = msg.message_id().map(|m| format!("<{}>", m.trim())).filter(|m| is_message_id(m)) {
        props.push((PR_INTERNET_MESSAGE_ID_W, Value::Unicode(id)));
    }
    let reply = match msg.in_reply_to() {
        HeaderValue::Text(t) => Some(t.as_ref()),
        HeaderValue::TextList(l) => l.first().map(|t| t.as_ref()),
        _ => None,
    };
    if let Some(id) = reply.map(|m| format!("<{}>", m.trim())).filter(|m| is_message_id(m)) {
        props.push((PR_IN_REPLY_TO_ID_W, Value::Unicode(id)));
    }
    let root = &msg.parts[0];
    if let Some(headers) = msg.raw_message.get(root.offset_header as usize..root.offset_body as usize) {
        props.push((PR_TRANSPORT_MESSAGE_HEADERS_W, Value::Unicode(String::from_utf8_lossy(headers).into_owned())));
    }

    // Тело зашифрованного или подписанного целиком не выносится (MS-OXOSMIME 2.1.3.2): только вложение.
    if !matches!(kind, Smime::Opaque(_)) {
        if let Some(body) = msg.body_text(0) {
            props.push((PR_BODY_W, Value::Unicode(body.into_owned())));
        }
        // Только настоящий HTML: из текстового письма разборщик тоже собирает HTML.
        if msg.html_bodies().any(|p| p.is_text_html())
            && let Some(html) = msg.body_html(0)
        {
            props.push((PR_HTML, Value::Binary(html.into_owned().into_bytes())));
        }
    }
    props.push((PR_MESSAGE_FLAGS, Value::Long(if seen { MSGFLAG_READ } else { 0 })));
    props.push((PR_INTERNET_CPID, Value::Long(CP_UTF8)));

    let recipients = [(MAPI_TO, &to), (MAPI_CC, &cc), (MAPI_BCC, &bcc)]
        .into_iter()
        .flat_map(|(kind, list)| list.iter().map(move |a| (kind, a)))
        .map(|(kind, a)| Recipient {
            props: vec![
                (PR_RECIPIENT_TYPE, Value::Long(kind)),
                text(PR_DISPLAY_NAME_W, display(a)),
                text(PR_ADDRTYPE_W, ADDRTYPE_SMTP),
                text(PR_EMAIL_ADDRESS_W, &a.email),
                text(PR_SMTP_ADDRESS_W, &a.email),
            ],
        })
        .collect();

    let attachments = match kind {
        Smime::Signed => vec![by_value("SMIME.p7m", "multipart/signed", None, msg.raw_message())],
        Smime::Opaque(mime) => vec![by_value("smime.p7m", mime, None, root.contents())],
        Smime::None => {
            let mut out = Vec::new();
            for part in msg.attachments() {
                let name = part.attachment_name();
                let cid = part.content_id().map(|c| c.trim_matches(['<', '>']));
                match &part.body {
                    PartType::Message(inner) if depth < MAX_NESTING => {
                        let mut props = vec![(PR_ATTACH_METHOD, Value::Long(ATTACH_EMBEDDED_MSG)), text(PR_ATTACH_MIME_TAG_W, "message/rfc822")];
                        if let Some(n) = name {
                            props.push(text(PR_ATTACH_LONG_FILENAME_W, n));
                        }
                        let embedded = message(inner, true, depth + 1, count)?;
                        out.push(Attachment { props, embedded: Some(Box::new(embedded)) });
                    }
                    _ => out.push(by_value(name.unwrap_or_default(), &mime_type(part), cid, part.contents())),
                }
            }
            out
        }
    };
    Ok(MapiMessage { class: class.into(), props, recipients, attachments })
}

/// Вложение байтами; пустое имя — без `PR_ATTACH_LONG_FILENAME_W`.
fn by_value(name: &str, mime: &str, cid: Option<&str>, data: &[u8]) -> Attachment {
    let mut props = vec![(PR_ATTACH_METHOD, Value::Long(ATTACH_BY_VALUE))];
    if !name.is_empty() {
        props.push((PR_ATTACH_LONG_FILENAME_W, Value::Unicode(name.into())));
    }
    props.push((PR_ATTACH_MIME_TAG_W, Value::Unicode(mime.into())));
    if let Some(cid) = cid.filter(|c| !c.is_empty()) {
        props.push((PR_ATTACH_CONTENT_ID_W, Value::Unicode(cid.into())));
    }
    props.push((PR_ATTACH_DATA_BIN, Value::Binary(data.to_vec())));
    Attachment { props, embedded: None }
}

fn mime_type<'x>(part: &impl MimeHeaders<'x>) -> String {
    match part.content_type() {
        Some(c) => format!("{}/{}", c.ctype(), c.subtype().unwrap_or("octet-stream")).to_ascii_lowercase(),
        None => "application/octet-stream".into(),
    }
}

/// Имя для показа: имя, нет — адрес.
fn display(a: &Addr) -> &str {
    if a.name.is_empty() { &a.email } else { &a.name }
}

/// Unix-секунды → FILETIME; до 1601 года — нет.
fn filetime(unix: i64) -> Option<u64> {
    let secs = unix.checked_add(i64::try_from(FILETIME_UNIX_EPOCH / 10_000_000).ok()?)?;
    u64::try_from(secs).ok()?.checked_mul(10_000_000)
}

/// Приставка темы (MS-OXCMSG 2.2.1.9): до трёх букв перед «: » («Re: », «Fwd: »); тема без неё. Нет — пусто и тема.
fn subject_prefix(subject: &str) -> (&str, &str) {
    match subject.find(": ") {
        Some(i) if (1..=3).contains(&subject[..i].chars().count()) && subject[..i].chars().all(char::is_alphabetic) => {
            (&subject[..i + 2], &subject[i + 2..])
        }
        _ => ("", subject),
    }
}

// --- Ответ воркера: письмо MAPI байтами (у типов `export` нет serde) ---
//
// письмо := класс:строка свойства n:u32 n×свойства n:u32 n×(свойства есть:u8 [письмо]); свойства := n:u32
// n×(тег:u32 тип:u16 значение); значение — по типу тега: LONG — i32, BOOLEAN — u8, SYSTIME — u64, UNICODE — строка,
// BINARY — байты; строка и байты — длина u32 и байты. Числа — LE.

/// Письмо MAPI → байты ответа.
pub(crate) fn encode(m: &MapiMessage) -> Vec<u8> {
    let mut out = Vec::new();
    encode_message(m, &mut out);
    out
}

fn encode_message(m: &MapiMessage, out: &mut Vec<u8>) {
    let bytes = |b: &[u8], out: &mut Vec<u8>| {
        out.extend((b.len() as u32).to_le_bytes());
        out.extend(b);
    };
    let props = |props: &[(Tag, Value)], out: &mut Vec<u8>| {
        out.extend((props.len() as u32).to_le_bytes());
        for (tag, v) in props {
            out.extend(tag.to_le_bytes());
            out.extend(v.prop_type().to_le_bytes());
            match v {
                Value::Long(n) => out.extend(n.to_le_bytes()),
                Value::Boolean(b) => out.push(u8::from(*b)),
                Value::SysTime(t) => out.extend(t.to_le_bytes()),
                Value::Unicode(s) => bytes(s.as_bytes(), out),
                Value::Binary(b) => bytes(b, out),
            }
        }
    };
    bytes(m.class.as_bytes(), out);
    props(&m.props, out);
    out.extend((m.recipients.len() as u32).to_le_bytes());
    m.recipients.iter().for_each(|r| props(&r.props, out));
    out.extend((m.attachments.len() as u32).to_le_bytes());
    for a in &m.attachments {
        props(&a.props, out);
        match &a.embedded {
            Some(e) => {
                out.push(1);
                encode_message(e, out);
            }
            None => out.push(0),
        }
    }
}

/// Байты ответа → письмо MAPI. Тип значения — как у тега, вложенность — не глубже [`MAX_NESTING`], байты — все;
/// иначе — `None`.
pub(crate) fn decode(bin: &[u8]) -> Option<MapiMessage> {
    let mut r = Reader(bin);
    let m = r.message(0)?;
    r.0.is_empty().then_some(m)
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let (head, rest) = self.0.split_at_checked(n)?;
        self.0 = rest;
        Some(head)
    }

    fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }

    fn u32(&mut self) -> Option<u32> {
        self.array().map(u32::from_le_bytes)
    }

    fn bytes(&mut self) -> Option<Vec<u8>> {
        let n = self.u32()? as usize;
        self.take(n).map(<[u8]>::to_vec)
    }

    fn props(&mut self) -> Option<Vec<(Tag, Value)>> {
        let n = self.u32()?;
        let mut out = Vec::new();
        for _ in 0..n {
            let tag = self.u32()?;
            let ty = u16::from_le_bytes(self.array()?);
            if ty != tag as u16 {
                return None;
            }
            let v = match ty {
                PT_LONG => Value::Long(i32::from_le_bytes(self.array()?)),
                PT_BOOLEAN => Value::Boolean(self.array::<1>()?[0] != 0),
                PT_SYSTIME => Value::SysTime(u64::from_le_bytes(self.array()?)),
                PT_UNICODE => Value::Unicode(String::from_utf8(self.bytes()?).ok()?),
                PT_BINARY => Value::Binary(self.bytes()?),
                _ => return None,
            };
            out.push((tag, v));
        }
        Some(out)
    }

    fn message(&mut self, depth: usize) -> Option<MapiMessage> {
        if depth > MAX_NESTING {
            return None;
        }
        let class = String::from_utf8(self.bytes()?).ok()?;
        let props = self.props()?;
        let mut recipients = Vec::new();
        for _ in 0..self.u32()? {
            recipients.push(Recipient { props: self.props()? });
        }
        let mut attachments = Vec::new();
        for _ in 0..self.u32()? {
            let props = self.props()?;
            let embedded = match self.array::<1>()?[0] {
                0 => None,
                1 => Some(Box::new(self.message(depth + 1)?)),
                _ => return None,
            };
            attachments.push(Attachment { props, embedded });
        }
        Some(MapiMessage { class, props, recipients, attachments })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use postman_export::mapi::*;

    use super::{MAX_NESTING, decode, encode, mapi};

    fn get(props: &[(Tag, Value)], tag: Tag) -> Option<&Value> {
        props.iter().find(|(t, _)| *t == tag).map(|(_, v)| v)
    }

    fn text(props: &[(Tag, Value)], tag: Tag) -> &str {
        match get(props, tag) {
            Some(Value::Unicode(s)) => s,
            other => panic!("{tag:08X}: {other:?}"),
        }
    }

    fn long(props: &[(Tag, Value)], tag: Tag) -> i32 {
        match get(props, tag) {
            Some(Value::Long(n)) => *n,
            other => panic!("{tag:08X}: {other:?}"),
        }
    }

    fn binary(props: &[(Tag, Value)], tag: Tag) -> &[u8] {
        match get(props, tag) {
            Some(Value::Binary(b)) => b,
            other => panic!("{tag:08X}: {other:?}"),
        }
    }

    /// Каждое значение — того же типа, что тег, ответ воркера разбирается в то же письмо.
    fn consistent(m: &MapiMessage) {
        let all = m.props.iter().chain(m.recipients.iter().flat_map(|r| &r.props)).chain(m.attachments.iter().flat_map(|a| &a.props));
        for (tag, v) in all {
            assert_eq!(*tag as u16, v.prop_type(), "{tag:08X}");
        }
        assert_eq!(decode(&encode(m)).as_ref(), Some(m));
    }

    const SIMPLE: &str = "From: Анна Иванова <anna@example.test>\r\nTo: Пётр <petr@example.test>, ivan@example.test\r\nCc: team@example.test\r\nSubject: Re: Отчёт\r\nDate: Fri, 30 Oct 2026 10:15:00 +0300\r\nMessage-ID: <m1@example.test>\r\nIn-Reply-To: <m0@example.test>\r\nReceived: from mx.example.test by mail.example.test; Fri, 30 Oct 2026 07:16:00 +0000\r\n\r\nПривет!\r\n";

    #[test]
    fn simple_letter() {
        let m = mapi(SIMPLE.as_bytes(), true).unwrap();
        consistent(&m);
        assert_eq!(m.class, IPM_NOTE);
        let p = &m.props;
        assert_eq!(text(p, PR_SUBJECT_W), "Re: Отчёт");
        assert_eq!((text(p, PR_SUBJECT_PREFIX_W), text(p, PR_NORMALIZED_SUBJECT_W)), ("Re: ", "Отчёт"));
        assert_eq!((text(p, PR_SENDER_NAME_W), text(p, PR_SENDER_EMAIL_ADDRESS_W)), ("Анна Иванова", "anna@example.test"));
        assert_eq!((text(p, PR_SENDER_ADDRTYPE_W), text(p, PR_SENT_REPRESENTING_SMTP_ADDRESS_W)), ("SMTP", "anna@example.test"));
        assert_eq!(text(p, PR_DISPLAY_TO_W), "Пётр; ivan@example.test");
        assert_eq!(text(p, PR_DISPLAY_CC_W), "team@example.test");
        // 2026-10-30 07:15:00 UTC и 07:16:00 UTC — FILETIME.
        let at = |unix: u64| (unix + 11_644_473_600) * 10_000_000;
        assert_eq!(get(p, PR_CLIENT_SUBMIT_TIME), Some(&Value::SysTime(at(1_793_344_500))));
        assert_eq!(get(p, PR_MESSAGE_DELIVERY_TIME), Some(&Value::SysTime(at(1_793_344_560))));
        assert_eq!((text(p, PR_INTERNET_MESSAGE_ID_W), text(p, PR_IN_REPLY_TO_ID_W)), ("<m1@example.test>", "<m0@example.test>"));
        assert!(text(p, PR_TRANSPORT_MESSAGE_HEADERS_W).starts_with("From: Анна Иванова"));
        assert!(text(p, PR_TRANSPORT_MESSAGE_HEADERS_W).contains("Received: from mx"));
        assert_eq!(text(p, PR_BODY_W).trim(), "Привет!");
        assert!(get(p, PR_HTML).is_none(), "у текстового письма HTML нет");
        assert_eq!((long(p, PR_MESSAGE_FLAGS), long(p, PR_INTERNET_CPID)), (MSGFLAG_READ, CP_UTF8));
        assert_eq!(long(&mapi(SIMPLE.as_bytes(), false).unwrap().props, PR_MESSAGE_FLAGS), 0);
        let r: Vec<_> = m
            .recipients
            .iter()
            .map(|r| (long(&r.props, PR_RECIPIENT_TYPE), text(&r.props, PR_DISPLAY_NAME_W), text(&r.props, PR_SMTP_ADDRESS_W)))
            .collect();
        assert_eq!(
            r,
            [
                (MAPI_TO, "Пётр", "petr@example.test"),
                (MAPI_TO, "ivan@example.test", "ivan@example.test"),
                (MAPI_CC, "team@example.test", "team@example.test")
            ]
        );
        assert_eq!(text(&m.recipients[0].props, PR_EMAIL_ADDRESS_W), "petr@example.test");
        assert!(m.attachments.is_empty());
    }

    #[test]
    fn html_and_text() {
        let raw = "From: a@example.test\r\nTo: b@example.test\r\nSubject: s\r\nMIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nТекст\r\n--b\r\nContent-Type: text/html; charset=koi8-r\r\nContent-Transfer-Encoding: base64\r\n\r\nPHA+9MXL09Q8L3A+\r\n--b--\r\n";
        let m = mapi(raw.as_bytes(), true).unwrap();
        consistent(&m);
        assert_eq!(text(&m.props, PR_BODY_W).trim(), "Текст");
        // HTML — в UTF-8 (PR_INTERNET_CPID), из KOI8-R.
        assert_eq!(binary(&m.props, PR_HTML), "<p>Текст</p>".as_bytes());
        assert_eq!(text(&m.props, PR_SUBJECT_PREFIX_W), "");
        assert!(m.attachments.is_empty(), "{:?}", m.attachments);
    }

    #[test]
    fn attachment_and_cid_picture() {
        let raw = "From: a@example.test\r\nTo: b@example.test\r\nSubject: s\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"m\"\r\n\r\n--m\r\nContent-Type: multipart/related; boundary=\"r\"\r\n\r\n--r\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p><img src=\"cid:logo@x\"></p>\r\n--r\r\nContent-Type: image/png\r\nContent-ID: <logo@x>\r\nContent-Disposition: inline; filename=\"logo.png\"\r\nContent-Transfer-Encoding: base64\r\n\r\niVBORw==\r\n--r--\r\n--m\r\nContent-Type: application/pdf; name=\"=?utf-8?B?0L7RgtGH0ZHRgi5wZGY=?=\"\r\nContent-Disposition: attachment\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERg==\r\n--m--\r\n";
        let m = mapi(raw.as_bytes(), true).unwrap();
        consistent(&m);
        assert!(binary(&m.props, PR_HTML).starts_with(b"<p><img src=\"cid:logo@x\">"));
        assert_eq!(m.attachments.len(), 2, "{:?}", m.attachments);
        let (logo, pdf) = (&m.attachments[0].props, &m.attachments[1].props);
        assert_eq!(long(logo, PR_ATTACH_METHOD), ATTACH_BY_VALUE);
        assert_eq!((text(logo, PR_ATTACH_LONG_FILENAME_W), text(logo, PR_ATTACH_MIME_TAG_W)), ("logo.png", "image/png"));
        assert_eq!(text(logo, PR_ATTACH_CONTENT_ID_W), "logo@x");
        assert_eq!(binary(logo, PR_ATTACH_DATA_BIN), b"\x89PNG");
        assert_eq!((text(pdf, PR_ATTACH_LONG_FILENAME_W), text(pdf, PR_ATTACH_MIME_TAG_W)), ("отчёт.pdf", "application/pdf"));
        assert!(get(pdf, PR_ATTACH_CONTENT_ID_W).is_none());
        assert_eq!(binary(pdf, PR_ATTACH_DATA_BIN), b"%PDF");
    }

    fn forward(inner: &str) -> String {
        format!(
            "From: a@example.test\r\nTo: b@example.test\r\nSubject: Fwd: x\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"f\"\r\n\r\n--f\r\nContent-Type: text/plain\r\n\r\nПересылаю\r\n--f\r\nContent-Type: message/rfc822\r\nContent-Disposition: attachment; filename=\"x.eml\"\r\n\r\n{inner}\r\n--f--\r\n"
        )
    }

    #[test]
    fn embedded_letter_and_nesting_limit() {
        let m = mapi(forward(SIMPLE).as_bytes(), false).unwrap();
        consistent(&m);
        assert_eq!(m.attachments.len(), 1);
        let a = &m.attachments[0];
        assert_eq!(long(&a.props, PR_ATTACH_METHOD), ATTACH_EMBEDDED_MSG);
        assert_eq!(text(&a.props, PR_ATTACH_LONG_FILENAME_W), "x.eml");
        let e = a.embedded.as_deref().unwrap();
        assert_eq!((e.class.as_str(), text(&e.props, PR_SUBJECT_W)), (IPM_NOTE, "Re: Отчёт"));
        assert_eq!(long(&e.props, PR_MESSAGE_FLAGS), MSGFLAG_READ);
        assert_eq!(e.recipients.len(), 3);

        // Четыре уровня: три — письмами, четвёртое — байтами message/rfc822.
        let deep = (0..=MAX_NESTING).fold(SIMPLE.to_owned(), |inner, _| forward(&inner));
        let mut m = &mapi(deep.as_bytes(), true).unwrap();
        consistent(m);
        for _ in 0..MAX_NESTING {
            m = m.attachments[0].embedded.as_deref().unwrap();
        }
        let last = &m.attachments[0];
        assert!(last.embedded.is_none());
        assert_eq!((long(&last.props, PR_ATTACH_METHOD), text(&last.props, PR_ATTACH_MIME_TAG_W)), (ATTACH_BY_VALUE, "message/rfc822"));
        assert!(binary(&last.props, PR_ATTACH_DATA_BIN).starts_with(b"From: "));
    }

    #[test]
    fn signed_letter_is_one_smime_attachment() {
        let raw = "From: a@example.test\r\nTo: b@example.test\r\nSubject: s\r\nMIME-Version: 1.0\r\nContent-Type: multipart/signed; protocol=\"application/pkcs7-signature\"; micalg=sha-256; boundary=\"s\"\r\n\r\n--s\r\nContent-Type: multipart/mixed; boundary=\"m\"\r\n\r\n--m\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nПодписано\r\n--m\r\nContent-Type: application/pdf; name=\"a.pdf\"\r\nContent-Disposition: attachment\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERg==\r\n--m--\r\n\r\n--s\r\nContent-Type: application/pkcs7-signature; name=\"smime.p7s\"\r\nContent-Transfer-Encoding: base64\r\n\r\nMIIB\r\n--s--\r\n";
        let m = mapi(raw.as_bytes(), true).unwrap();
        consistent(&m);
        assert_eq!(m.class, IPM_NOTE_SMIME_MULTIPART_SIGNED);
        assert_eq!(text(&m.props, PR_BODY_W).trim(), "Подписано");
        assert_eq!(m.attachments.len(), 1);
        let a = &m.attachments[0].props;
        assert_eq!((text(a, PR_ATTACH_LONG_FILENAME_W), text(a, PR_ATTACH_MIME_TAG_W)), ("SMIME.p7m", "multipart/signed"));
        assert_eq!(binary(a, PR_ATTACH_DATA_BIN), raw.as_bytes(), "всё письмо, исходными байтами");
    }

    #[test]
    fn encrypted_letter_is_cms_without_body() {
        let raw = "From: a@example.test\r\nTo: b@example.test\r\nSubject: Секрет\r\nMIME-Version: 1.0\r\nContent-Type: application/pkcs7-mime; smime-type=enveloped-data; name=\"smime.p7m\"\r\nContent-Disposition: attachment; filename=\"smime.p7m\"\r\nContent-Transfer-Encoding: base64\r\n\r\nMIIBAgME\r\n";
        let m = mapi(raw.as_bytes(), true).unwrap();
        consistent(&m);
        assert_eq!(m.class, IPM_NOTE_SMIME);
        assert!(get(&m.props, PR_BODY_W).is_none() && get(&m.props, PR_HTML).is_none(), "расшифрованного текста нет");
        assert_eq!(text(&m.props, PR_SUBJECT_W), "Секрет");
        assert_eq!(m.attachments.len(), 1);
        let a = &m.attachments[0].props;
        assert_eq!((text(a, PR_ATTACH_LONG_FILENAME_W), text(a, PR_ATTACH_MIME_TAG_W)), ("smime.p7m", "application/pkcs7-mime"));
        assert_eq!(binary(a, PR_ATTACH_DATA_BIN), [0x30, 0x82, 0x01, 0x02, 0x03, 0x04], "CMS без base64");
    }

    /// Тема в RFC 2047 (base64 и quoted-printable) → строки `_W` (`PT_UNICODE`, в файле — UTF-16LE).
    #[test]
    fn cyrillic_subject_is_unicode() {
        let raw = "From: =?utf-8?B?0JDQvdC90LA=?= <anna@example.test>\r\nTo: b@example.test\r\nSubject: =?utf-8?B?0J7RgtCy0LXRgjog0L/Qu9Cw0L0g?= =?koi8-r?Q?=CE=C1_=CE=CF=D1=C2=D2=D8?=\r\n\r\nx\r\n";
        let m = mapi(raw.as_bytes(), true).unwrap();
        consistent(&m);
        let subject = text(&m.props, PR_SUBJECT_W);
        assert_eq!(subject, "Ответ: план на ноябрь");
        assert_eq!(text(&m.props, PR_SUBJECT_PREFIX_W), "", "больше трёх букв — не приставка");
        assert_eq!(text(&m.props, PR_SENDER_NAME_W), "Анна");
        assert_eq!(PR_SUBJECT_W as u16, PT_UNICODE);
        assert_eq!(subject.encode_utf16().count(), 21);
    }

    #[test]
    fn junk_and_broken_answers() {
        for junk in [&b""[..], b"\r\n\r\n", b"\x00\xff\xfe\r\n\r\n"] {
            assert!(mapi(junk, true).is_err(), "{junk:?}");
        }
        let bin = encode(&mapi(forward(SIMPLE).as_bytes(), true).unwrap());
        for n in 0..bin.len() {
            assert!(decode(&bin[..n]).is_none(), "обрезано до {n}");
        }
        let mut longer = bin.clone();
        longer.push(0);
        assert!(decode(&longer).is_none());
        // Тип значения не как у тега.
        let wrong = MapiMessage { props: vec![(PR_SUBJECT_W, Value::Long(1))], ..Default::default() };
        assert!(decode(&encode(&wrong)).is_none());
    }
}
