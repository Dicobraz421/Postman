//! Где подпись в письме (RFC 8551 §3.5): `multipart/signed` с отдельной подписью или `application/pkcs7-mime`.
//!
//! Заголовки — `mail-parser` (только заголовки корня, без разбора тела: письмо до 62 МБ); границы частей —
//! свой разбор по RFC 2046 §5.1.1: подписанная часть — байты от строки-разделителя до CRLF перед следующей, как
//! пришли, затем канонизация концов строк в CRLF (RFC 8551 §3.1.1: подпись считается над каноническим видом).

use std::borrow::Cow;

use mail_parser::{MessageParser, MimeHeaders};

use crate::{oid, signed};

pub(crate) enum Found {
    None {
        from: String,
    },
    /// Шифровано: DER CMS (`None` — тело не снялось).
    Encrypted {
        cms: Option<Vec<u8>>,
        from: String,
    },
    /// Подписанная часть (канонизированная) и DER подписи (`smime.p7s`).
    Detached {
        signed: Vec<u8>,
        signature: Vec<u8>,
        from: String,
    },
    /// DER `signed-data` (`smime.p7m`).
    Opaque {
        cms: Vec<u8>,
        from: String,
    },
}

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
pub(crate) fn split<'a>(body: &'a [u8], boundary: &str) -> Option<Vec<&'a [u8]>> {
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

/// Концы строк — CRLF (одиночный LF → CRLF).
pub(crate) fn canonical(bytes: &[u8]) -> Cow<'_, [u8]> {
    let bare = bytes.iter().enumerate().any(|(i, &b)| b == b'\n' && (i == 0 || bytes[i - 1] != b'\r'));
    if !bare {
        return Cow::Borrowed(bytes);
    }
    let mut out = Vec::with_capacity(bytes.len() + bytes.len() / 32);
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' && (i == 0 || bytes[i - 1] != b'\r') {
            out.push(b'\r');
        }
        out.push(b);
    }
    Cow::Owned(out)
}

/// Содержимое части (заголовки + тело) после снятия Content-Transfer-Encoding.
fn decoded(part: &[u8]) -> Option<Vec<u8>> {
    let msg = MessageParser::default().parse(part)?;
    Some(msg.root_part().contents().to_vec())
}

fn is_pkcs7_signature(t: &str) -> bool {
    matches!(t, "application/pkcs7-signature" | "application/x-pkcs7-signature")
}

/// Найти подпись S/MIME в корне письма.
pub(crate) fn find(message: &[u8]) -> Result<Found, String> {
    let head = MessageParser::default().parse_headers(message).ok_or("no headers")?;
    let from = from_of(&head);
    let Some(ct) = head.content_type() else { return Ok(Found::None { from }) };
    let full = format!("{}/{}", ct.ctype(), ct.subtype().unwrap_or_default()).to_ascii_lowercase();
    match full.as_str() {
        "multipart/signed" => {
            let protocol = ct.attribute("protocol").unwrap_or_default().to_ascii_lowercase();
            if !is_pkcs7_signature(&protocol) {
                // OpenPGP/MIME и прочее — не S/MIME.
                return Ok(Found::None { from });
            }
            let boundary = ct.attribute("boundary").ok_or("no boundary")?.to_string();
            let start = body_start(message).ok_or("no body")?;
            let parts = split(&message[start..], &boundary).ok_or("multipart/signed is truncated")?;
            let [content, sig, ..] = parts.as_slice() else { return Err("multipart/signed needs two parts".into()) };
            let signature = decoded(sig).ok_or("signature part")?;
            Ok(Found::Detached { signed: canonical(content).into_owned(), signature, from })
        }
        "application/pkcs7-mime" | "application/x-pkcs7-mime" => {
            let smime = ct.attribute("smime-type").unwrap_or_default().to_ascii_lowercase();
            if smime == "enveloped-data" || smime == "authenveloped-data" {
                return Ok(Found::Encrypted { cms: decoded(message), from });
            }
            let cms = decoded(message).ok_or("pkcs7-mime body")?;
            match signed::content_type(&cms) {
                Some(oid::ENVELOPED_DATA) | Some(oid::AUTH_ENVELOPED_DATA) => Ok(Found::Encrypted { cms: Some(cms), from }),
                Some(oid::SIGNED_DATA) => Ok(Found::Opaque { cms, from }),
                Some(_) => Ok(Found::None { from }),
                None => Err("pkcs7-mime is not CMS".into()),
            }
        }
        _ => Ok(Found::None { from }),
    }
}

#[cfg(test)]
mod tests {
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
    fn canonicalizes_line_ends() {
        assert_eq!(&*canonical(b"a\r\nb"), b"a\r\nb");
        assert!(matches!(canonical(b"a\r\nb"), Cow::Borrowed(_)));
        assert_eq!(&*canonical(b"a\nb\r\nc\n"), b"a\r\nb\r\nc\r\n");
        assert_eq!(&*canonical(b"\n"), b"\r\n");
    }

    #[test]
    fn plain_letter_is_not_signed() {
        let m = b"From: A <A@Example.ru>\r\nContent-Type: text/plain\r\n\r\nhi";
        assert!(matches!(find(m), Ok(Found::None { from }) if from == "a@example.ru"));
        let pgp = b"From: a@x\r\nContent-Type: multipart/signed; protocol=\"application/pgp-signature\"; boundary=b\r\n\r\n--b\r\nx\r\n--b--";
        assert!(matches!(find(pgp), Ok(Found::None { .. })));
        let enc = b"From: a@x\r\nContent-Type: application/pkcs7-mime; smime-type=enveloped-data\r\n\r\nAAAA";
        assert!(matches!(find(enc), Ok(Found::Encrypted { .. })));
        assert!(find(b"From: a@x\r\nContent-Type: multipart/signed; protocol=\"application/pkcs7-signature\"; boundary=b\r\n\r\n--b\r\nx").is_err());
    }
}
