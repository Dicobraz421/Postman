//! MIME писем S/MIME (S8-36): заголовки корня, части `multipart/signed`, Content-Transfer-Encoding, тело и вложения
//! расшифрованного письма — только здесь, в песочнице. CMS, подпись и шифр — `postman_smime` по готовым частям
//! ([`postman_smime::Letter`]); вне воркера письмо не разбирается.
//!
//! Заголовки — `mail-parser` (только заголовки корня, без разбора тела: письмо до 62 МБ); границы частей — свой разбор
//! по RFC 2046 §5.1.1: подписанная часть — байты от строки-разделителя до CRLF перед следующей, как пришли (концы строк
//! канонизирует `postman_smime`).

use std::panic::{AssertUnwindSafe, catch_unwind};

use mail_parser::{MessageParser, MimeHeaders};
use postman_smime::envelope::{Cipher, Envelope, OpenError, Opened, Part};
use postman_smime::{Kind, Letter, MAX_MESSAGE, Parts, Report, Status, Trust};
use zeroize::Zeroizing;

/// Адрес «От» корня письма, в нижнем регистре.
fn from_of(msg: &mail_parser::Message<'_>) -> String {
    msg.from().and_then(|a| a.first()).and_then(|a| a.address.as_deref()).unwrap_or_default().trim().to_lowercase()
}

/// Начало тела: после первой пустой строки.
fn body_start(message: &[u8]) -> Option<usize> {
    let mut pos = 0;
    while pos < message.len() {
        let end = message[pos..].iter().position(|&b| b == b'\n').map(|i| pos + i)?;
        let line = &message[pos..end];
        if line.is_empty() || line == b"\r" {
            return Some(end + 1);
        }
        pos = end + 1;
    }
    None
}

/// Части `multipart` между разделителями `--boundary`: байты каждой части без CRLF перед следующим разделителем.
fn split<'a>(body: &'a [u8], boundary: &str) -> Option<Vec<&'a [u8]>> {
    let delim = format!("--{boundary}");
    let delim = delim.as_bytes();
    let mut parts = Vec::new();
    let mut start: Option<usize> = None;
    let mut pos = 0;
    while pos <= body.len() {
        let end = body[pos..].iter().position(|&b| b == b'\n').map_or(body.len(), |i| pos + i);
        let line = &body[pos..end];
        if let Some(rest) = line.strip_prefix(delim) {
            let closing = rest.starts_with(b"--");
            let tail = if closing { &rest[2..] } else { rest };
            if tail.iter().all(|b| matches!(b, b' ' | b'\t' | b'\r')) {
                if let Some(s) = start {
                    // Перевод строки перед разделителем принадлежит разделителю.
                    let mut e = pos.saturating_sub(1);
                    if e > s && body.get(e - 1) == Some(&b'\r') {
                        e -= 1;
                    }
                    parts.push(&body[s..e.max(s)]);
                }
                if closing {
                    return Some(parts);
                }
                start = Some((end + 1).min(body.len()));
            }
        }
        pos = end + 1;
    }
    // Без закрывающего разделителя — письмо обрезано.
    None
}

/// Содержимое части (заголовки + тело) после снятия Content-Transfer-Encoding.
fn decoded(part: &[u8]) -> Option<Vec<u8>> {
    let msg = MessageParser::default().parse(part)?;
    Some(msg.root_part().contents().to_vec())
}

fn is_pkcs7_signature(t: &str) -> bool {
    matches!(t, "application/pkcs7-signature" | "application/x-pkcs7-signature")
}

/// S/MIME-части корня письма по его заголовкам.
fn parts(message: &[u8], head: &mail_parser::Message<'_>) -> Result<Parts, String> {
    let Some(ct) = head.content_type() else { return Ok(Parts::None) };
    let full = format!("{}/{}", ct.ctype(), ct.subtype().unwrap_or_default()).to_ascii_lowercase();
    match full.as_str() {
        "multipart/signed" => {
            let protocol = ct.attribute("protocol").unwrap_or_default().to_ascii_lowercase();
            if !is_pkcs7_signature(&protocol) {
                // OpenPGP/MIME и прочее — не S/MIME.
                return Ok(Parts::None);
            }
            let boundary = ct.attribute("boundary").ok_or("no boundary")?.to_string();
            let start = body_start(message).ok_or("no body")?;
            let parts = split(&message[start..], &boundary).ok_or("multipart/signed is truncated")?;
            let [content, sig, ..] = parts.as_slice() else { return Err("multipart/signed needs two parts".into()) };
            let signature = decoded(sig).ok_or("signature part")?;
            Ok(Parts::Detached { content: content.to_vec(), signature })
        }
        "application/pkcs7-mime" | "application/x-pkcs7-mime" => {
            let smime = ct.attribute("smime-type").unwrap_or_default().to_ascii_lowercase();
            let enveloped = smime == "enveloped-data" || smime == "authenveloped-data";
            Ok(Parts::Pkcs7 { cms: decoded(message), enveloped })
        }
        _ => Ok(Parts::None),
    }
}

/// Корень письма (RFC 5322 как пришло) → «От» и S/MIME-части для `postman_smime`.
pub fn letter(message: &[u8]) -> Letter {
    let Some(head) = MessageParser::default().parse_headers(message) else { return Letter { from: String::new(), parts: Err("no headers".into()) } };
    Letter { from: from_of(&head), parts: parts(message, &head) }
}

/// Корень письма без паники: сбой разборщика на чужих данных — «повреждено».
fn guarded_letter(message: &[u8]) -> Result<Letter, OpenError> {
    catch_unwind(|| letter(message)).map_err(|_| OpenError::Malformed("parser failure".into()))
}

fn guarded<T>(f: impl FnOnce() -> Result<T, OpenError>) -> Result<T, OpenError> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| Err(OpenError::Malformed("parser failure".into())))
}

/// Проверить подпись письма `message` (RFC 5322 как пришло) к якорям `trust` на момент `now` (Unix, секунды).
/// Не паникует: любой сбой разбора — [`Status::Malformed`].
pub fn verify(message: &[u8], trust: &Trust, now: u64) -> Report {
    if message.len() > MAX_MESSAGE {
        return Report::new(Kind::None, Status::Malformed("message too large".into()));
    }
    match guarded_letter(message) {
        Ok(l) => postman_smime::verify(&l, trust, now),
        Err(_) => Report::new(Kind::None, Status::Malformed("parser failure".into())),
    }
}

/// Тело подписанного содержимого `signed-data` (внутренний MIME): HTML и текст — для показа письма вместо пустого
/// тела (сервер видит только `smime.p7m`).
pub fn inner_body(content: &[u8]) -> (Option<String>, Option<String>) {
    let Some(msg) = MessageParser::default().parse(content) else { return (None, None) };
    let html = msg.body_html(0).map(|h| h.into_owned());
    let text = msg.body_text(0).map(|t| t.into_owned());
    (html, text)
}

/// Получатели и шифр письма `message` (RFC 5322 целиком) — для ядра. Ничего не расшифровывает.
pub fn envelope(message: &[u8]) -> Result<Envelope, OpenError> {
    if message.len() > MAX_MESSAGE {
        return Err(OpenError::TooLarge);
    }
    postman_smime::envelope::envelope(&guarded_letter(message)?)
}

fn malformed(what: &str) -> OpenError {
    OpenError::Malformed(what.into())
}

/// Расшифрованное внутреннее письмо: подпись внутри и тело для показа (шифрование внутри — не открываем).
fn inner_layers(inner: &[u8], from: &str, trust: &Trust, now: u64) -> Result<(Vec<u8>, Report), OpenError> {
    if postman_smime::is_encrypted(&guarded_letter(inner)?) {
        return Err(OpenError::Nested);
    }
    let mut report = verify(inner, trust, now);
    // Внутренняя часть без заголовков письма: «От» — внешнего письма (его и показывает список).
    if report.from.is_empty() {
        report.address_ok =
            report.status.signature_ok() && report.certs.first().is_some_and(|c| c.emails.iter().any(|e| e.eq_ignore_ascii_case(from)));
        report.from = from.to_owned();
    }
    // Подпись внутри (`signed-data`): тело — подписанное содержимое; иначе — само внутреннее письмо.
    let body = match &report.content {
        Some(c) if report.kind == Kind::Opaque => {
            if postman_smime::is_encrypted(&guarded_letter(c)?) {
                return Err(OpenError::Nested);
            }
            c.clone()
        }
        _ => inner.to_vec(),
    };
    Ok((body, report))
}

/// Вложения расшифрованного письма без подписи (`smime.p7s` — значком, не вложением).
fn attachments<'a>(msg: &'a mail_parser::Message<'a>) -> Vec<&'a mail_parser::MessagePart<'a>> {
    msg.attachments()
        .filter(|a| {
            let t = a.content_type().map(|c| format!("{}/{}", c.ctype(), c.subtype().unwrap_or_default()).to_ascii_lowercase()).unwrap_or_default();
            !is_pkcs7_signature(&t)
        })
        .collect()
}

fn parts_of(msg: &mail_parser::Message<'_>) -> Vec<Part> {
    attachments(msg)
        .into_iter()
        .enumerate()
        .map(|(i, a)| {
            let mime = a
                .content_type()
                .map(|c| format!("{}/{}", c.ctype(), c.subtype().unwrap_or("octet-stream")))
                .unwrap_or_else(|| "application/octet-stream".into());
            let cid = a.content_id().map(|c| c.trim_matches(['<', '>']).to_string());
            let inline = a.content_disposition().is_some_and(|d| d.is_inline());
            Part {
                index: u32::try_from(i).unwrap_or(u32::MAX),
                name: a.attachment_name().unwrap_or("attachment").to_string(),
                mime: mime.to_ascii_lowercase(),
                size: a.contents().len() as u64,
                cid,
                inline,
            }
        })
        .collect()
}

/// Тело для показа → расшифрованное письмо: HTML, текст и вложения.
fn opened(body: &[u8], cipher: Cipher, authenticated: bool, inner: Report) -> Result<Opened, OpenError> {
    let msg = MessageParser::default().parse(body).ok_or_else(|| malformed("inner MIME"))?;
    let html = msg.body_html(0).map(|h| h.into_owned());
    let text = msg.body_text(0).map(|t| t.into_owned());
    Ok(Opened { cipher, authenticated, inner, html, text, parts: parts_of(&msg) })
}

/// Байты вложения `index` тела для показа.
fn attachment(body: &[u8], index: u32) -> Result<Zeroizing<Vec<u8>>, OpenError> {
    let msg = MessageParser::default().parse(body).ok_or_else(|| malformed("inner MIME"))?;
    let all = attachments(&msg);
    let a = all.get(index as usize).ok_or_else(|| malformed("no such part"))?;
    Ok(Zeroizing::new(a.contents().to_vec()))
}

/// Расшифровать письмо `message` ключом `cek`: тело, вложения и подпись внутри (к якорям `trust`).
pub fn open(message: &[u8], cek: &[u8], trust: &Trust, now: u64) -> Result<Opened, OpenError> {
    if message.len() > MAX_MESSAGE {
        return Err(OpenError::TooLarge);
    }
    let letter = guarded_letter(message)?;
    let d = postman_smime::envelope::decrypt(&letter, cek)?;
    guarded(|| {
        let (body, inner) = inner_layers(&d.inner, &letter.from, trust, now)?;
        opened(&body, d.cipher, d.authenticated, inner)
    })
}

/// Байты вложения `index` расшифрованного письма.
pub fn part(message: &[u8], cek: &[u8], index: u32) -> Result<Zeroizing<Vec<u8>>, OpenError> {
    if message.len() > MAX_MESSAGE {
        return Err(OpenError::TooLarge);
    }
    let letter = guarded_letter(message)?;
    let d = postman_smime::envelope::decrypt(&letter, cek)?;
    guarded(|| {
        let (body, _) = inner_layers(&d.inner, "", &Trust::default(), 0)?;
        attachment(&body, index)
    })
}

/// Письмо, расшифрованное в ядре (ГОСТ — СКЗИ): тело, вложения, подпись внутри — как [`open`], без расшифровки.
/// `from` — «От» внешнего письма.
pub fn open_inner(inner: &[u8], from: &str, cipher: Cipher, trust: &Trust, now: u64) -> Result<Opened, OpenError> {
    guarded(|| {
        let (body, report) = inner_layers(inner, from, trust, now)?;
        opened(&body, cipher, false, report)
    })
}

/// Вложение письма, расшифрованного в ядре.
pub fn part_inner(inner: &[u8], index: u32) -> Result<Zeroizing<Vec<u8>>, OpenError> {
    guarded(|| {
        let (body, _) = inner_layers(inner, "", &Trust::default(), 0)?;
        attachment(&body, index)
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn splits_parts_exactly() {
        let body = b"preamble\r\n--b\r\nContent-Type: text/plain\r\n\r\nhi\r\n--b\r\nsig\r\n--b--\r\nepilogue";
        let parts = split(body, "b").unwrap();
        assert_eq!(parts, vec![&b"Content-Type: text/plain\r\n\r\nhi"[..], &b"sig"[..]]);
        // LF-only и разделитель в начале тела.
        let parts = split(b"--b\nA\n\nx\n--b\ny\n--b--\n", "b").unwrap();
        assert_eq!(parts, vec![&b"A\n\nx"[..], &b"y"[..]]);
        // Похожая строка внутри части — не разделитель.
        let parts = split(b"--b\r\n--bb\r\n--b \r\nz\r\n--b--", "b").unwrap();
        assert_eq!(parts, vec![&b"--bb"[..], &b"z"[..]]);
        assert!(split(b"--b\r\nx\r\n--b\r\n", "b").is_none(), "без закрывающего — обрезано");
    }

    #[test]
    fn plain_letter_is_not_signed() {
        let l = letter(b"From: A <A@Example.ru>\r\nContent-Type: text/plain\r\n\r\nhi");
        assert_eq!((l.from.as_str(), l.parts), ("a@example.ru", Ok(Parts::None)));
        let pgp = b"From: a@x\r\nContent-Type: multipart/signed; protocol=\"application/pgp-signature\"; boundary=b\r\n\r\n--b\r\nx\r\n--b--";
        assert_eq!(letter(pgp).parts, Ok(Parts::None));
        let enc = letter(b"From: a@x\r\nContent-Type: application/pkcs7-mime; smime-type=enveloped-data\r\n\r\nAAAA");
        assert!(matches!(enc.parts, Ok(Parts::Pkcs7 { enveloped: true, .. })));
        assert!(postman_smime::is_encrypted(&enc));
        let truncated = b"From: a@x\r\nContent-Type: multipart/signed; protocol=\"application/pkcs7-signature\"; boundary=b\r\n\r\n--b\r\nx";
        assert!(letter(truncated).parts.is_err());
    }
}
