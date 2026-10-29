//! Письмо целиком (RFC 5322) → поля тела и вложения (S7-04б): у JMAP разбор MIME делает сервер, у IMAP — Postman, и
//! только здесь, в песочнице (`docs/HANDOFF.md` §3a). Вложения — по номеру в списке; байты — отдельным запросом.
//! Для хранилища (S8-07, ADR 0006 §2) — диапазоны крупных листовых частей и их раскодированные байты ([`split`]).

use std::ops::Range;

use mail_parser::decoders::base64::base64_decode;
use mail_parser::decoders::quoted_printable::quoted_printable_decode;
use mail_parser::{Address, MimeHeaders, PartType};
use serde::{Deserialize, Serialize};

/// Больше не разбираем (письмо IMAP целиком; S/MIME — свой предел).
pub const MAX_MESSAGE: usize = 64 * 1024 * 1024;

/// Адрес: имя и адрес.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Addr {
    pub name: String,
    pub email: String,
}

/// Вложение: номер в списке вложений письма.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Part {
    pub index: u32,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub cid: Option<String>,
    pub inline: bool,
}

/// Разобранное письмо: то, что у JMAP даёт сервер (`Email/get` с телом).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Mime {
    pub to: Vec<Addr>,
    pub cc: Vec<Addr>,
    pub reply_to: Vec<Addr>,
    pub message_id: Option<String>,
    pub references: Vec<String>,
    pub html: Option<String>,
    pub text: Option<String>,
    pub parts: Vec<Part>,
}

fn addrs(a: Option<&Address<'_>>) -> Vec<Addr> {
    a.map(|a| {
        a.iter()
            .filter_map(|x| {
                let email = x.address.as_deref()?.to_owned();
                Some(Addr { name: x.name.as_deref().unwrap_or_default().to_owned(), email })
            })
            .collect()
    })
    .unwrap_or_default()
}

fn attachments<'a>(msg: &'a mail_parser::Message<'a>) -> Vec<&'a mail_parser::MessagePart<'a>> {
    msg.attachments().collect()
}

/// Поля тела и список вложений.
pub fn parse(raw: &[u8]) -> Result<Mime, String> {
    if raw.len() > MAX_MESSAGE {
        return Err("письмо слишком большое".into());
    }
    let msg = mail_parser::MessageParser::default().parse(raw).ok_or("не письмо RFC 5322")?;
    let references = match msg.references() {
        mail_parser::HeaderValue::Text(t) => vec![t.to_string()],
        mail_parser::HeaderValue::TextList(l) => l.iter().map(|t| t.to_string()).collect(),
        _ => Vec::new(),
    };
    let parts = attachments(&msg)
        .into_iter()
        .enumerate()
        .map(|(i, a)| {
            let mime = a
                .content_type()
                .map(|c| format!("{}/{}", c.ctype(), c.subtype().unwrap_or("octet-stream")))
                .unwrap_or_else(|| "application/octet-stream".into());
            Part {
                index: u32::try_from(i).unwrap_or(u32::MAX),
                name: a.attachment_name().unwrap_or("attachment").to_owned(),
                mime: mime.to_ascii_lowercase(),
                size: a.contents().len() as u64,
                cid: a.content_id().map(|c| c.trim_matches(['<', '>']).to_owned()),
                inline: a.content_disposition().is_some_and(|d| d.is_inline()),
            }
        })
        .collect();
    Ok(Mime {
        to: addrs(msg.to()),
        cc: addrs(msg.cc()),
        reply_to: addrs(msg.reply_to()),
        message_id: msg.message_id().map(str::to_owned),
        references,
        html: msg.body_html(0).map(|h| h.into_owned()),
        text: msg.body_text(0).map(|t| t.into_owned()),
        parts,
    })
}

/// Байты вложения `index` (номер из [`parse`]).
pub fn part(raw: &[u8], index: u32) -> Result<Vec<u8>, String> {
    if raw.len() > MAX_MESSAGE {
        return Err("письмо слишком большое".into());
    }
    let msg = mail_parser::MessageParser::default().parse(raw).ok_or("не письмо RFC 5322")?;
    let all = attachments(&msg);
    let a = all.get(index as usize).ok_or("нет такого вложения")?;
    Ok(a.contents().to_vec())
}

/// Письмо целиком на разбор частей для хранилища: локальный архив хранит письмо любого размера (ADR 0006 §2).
pub const MAX_SPLIT: usize = 256 * 1024 * 1024;
/// Больше частей MIME (контейнеры и вложенные письма — тоже) не разбираем.
pub const MAX_SPLIT_PARTS: usize = 1000;
/// Листовая часть меньше (закодированная, в письме) остаётся в скелете.
pub const MIN_SPLIT_PART: usize = 64 * 1024;

/// Кодировка листовой части в письме.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Encoding {
    Base64,
    QuotedPrintable,
}

/// Листовая часть: байты `range` письма — её тело в кодировке `encoding`; раскодированное — `decoded_len` байт в
/// бинарной части ответа (части подряд, по порядку).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitPart {
    pub range: Range<u64>,
    pub encoding: Encoding,
    pub decoded_len: u64,
}

/// Крупные листовые части письма по порядку в нём.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Split {
    pub parts: Vec<SplitPart>,
}

impl Split {
    /// Ответ воркера согласован с письмом длины `raw_len` и бинарной частью длины `bin_len`: диапазоны внутри
    /// письма, по порядку и без наложений, раскодированное — ровно бинарная часть.
    pub(crate) fn fits(&self, raw_len: usize, bin_len: usize) -> bool {
        let mut end = 0;
        let mut decoded = 0u64;
        for p in &self.parts {
            if p.range.start < end || p.range.end < p.range.start || p.range.end > raw_len as u64 {
                return false;
            }
            end = p.range.end;
            decoded = decoded.saturating_add(p.decoded_len);
        }
        self.parts.len() <= MAX_SPLIT_PARTS && decoded == bin_len as u64
    }
}

/// Письмо целиком → крупные (от [`MIN_SPLIT_PART`]) листовые части в `base64` и `quoted-printable` и их
/// раскодированные байты подряд. Вложенные `message/rfc822` без кодировки — рекурсивно; закодированное вложенное
/// письмо — одна часть; S/MIME (`application/pkcs7-mime`) — не разбирается. Часть, которая строго не
/// раскодировалась, остаётся в скелете.
pub fn split(raw: &[u8]) -> Result<(Split, Vec<u8>), String> {
    if raw.len() > MAX_SPLIT {
        return Err("письмо слишком большое".into());
    }
    let msg = mail_parser::MessageParser::default().parse(raw).ok_or("не письмо RFC 5322")?;
    if msg.parts.first().is_none_or(|p| p.headers.is_empty()) {
        return Err("не письмо RFC 5322".into());
    }
    let mut leaves = Vec::new();
    let mut count = 0;
    collect_leaves(&msg, &mut leaves, &mut count)?;
    // Раскодированные разборщиком части больше не нужны: в памяти — письмо и ответ.
    drop(msg);
    let capacity = leaves
        .iter()
        .map(|(r, e)| match e {
            Encoding::Base64 => r.len() / 4 * 3 + 3,
            Encoding::QuotedPrintable => r.len(),
        })
        .sum();
    let mut out = Vec::with_capacity(capacity);
    let mut parts = Vec::with_capacity(leaves.len());
    for (range, encoding) in leaves {
        let encoded = raw.get(range.clone()).ok_or("письмо повреждено: часть за его пределами")?;
        // Строгие декодеры: сомнительная часть остаётся в скелете (ядро всё равно сверяет обратным кодированием).
        let decoded = match encoding {
            Encoding::Base64 => base64_decode(encoded),
            Encoding::QuotedPrintable => quoted_printable_decode(encoded),
        };
        let Some(decoded) = decoded else { continue };
        parts.push(SplitPart { range: range.start as u64..range.end as u64, encoding, decoded_len: decoded.len() as u64 });
        out.extend_from_slice(&decoded);
    }
    if out.len() > MAX_SPLIT {
        return Err("письмо слишком большое".into());
    }
    Ok((Split { parts }, out))
}

/// Листовые части `msg` (и вложенных писем без кодировки: их смещения — в том же письме) по порядку.
fn collect_leaves(msg: &mail_parser::Message<'_>, out: &mut Vec<(Range<usize>, Encoding)>, count: &mut usize) -> Result<(), String> {
    for part in &msg.parts {
        *count += 1;
        if *count > MAX_SPLIT_PARTS {
            return Err(format!("больше {MAX_SPLIT_PARTS} частей"));
        }
        // Зашифрованное и подписанное целиком S/MIME — один блоб: внутрь не смотрим.
        if part.content_type().is_some_and(|c| {
            c.ctype().eq_ignore_ascii_case("application")
                && c.subtype().is_some_and(|s| s.eq_ignore_ascii_case("pkcs7-mime") || s.eq_ignore_ascii_case("x-pkcs7-mime"))
        }) {
            continue;
        }
        let encoding = match part.encoding {
            mail_parser::Encoding::Base64 => Encoding::Base64,
            mail_parser::Encoding::QuotedPrintable => Encoding::QuotedPrintable,
            mail_parser::Encoding::None => {
                if let PartType::Message(inner) = &part.body {
                    collect_leaves(inner, out, count)?;
                }
                continue;
            }
        };
        let range = part.offset_body as usize..part.offset_end as usize;
        if range.len() >= MIN_SPLIT_PART {
            out.push((range, encoding));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::{Encoding, MAX_SPLIT_PARTS, parse, part, split};

    const SAMPLE: &str = "From: Анна <anna@example.test>\r\nTo: Пётр <petr@example.test>, ivan@example.test\r\nCc: team@example.test\r\nSubject: =?utf-8?B?0J7RgtGH0ZHRgg==?=\r\nMessage-ID: <m1@example.test>\r\nReferences: <a@example.test> <b@example.test>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b1\"\r\n\r\n--b1\r\nContent-Type: multipart/alternative; boundary=\"b2\"\r\n\r\n--b2\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nПривет\r\n--b2\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>Привет</p>\r\n--b2--\r\n--b1\r\nContent-Type: application/pdf; name=\"report.pdf\"\r\nContent-Disposition: attachment; filename=\"report.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\nSGVsbG8=\r\n--b1--\r\n";

    #[test]
    fn message_becomes_body_and_parts() {
        let m = parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(m.to.len(), 2);
        assert_eq!((m.to[0].name.as_str(), m.to[0].email.as_str()), ("Пётр", "petr@example.test"));
        assert_eq!(m.cc[0].email, "team@example.test");
        assert_eq!(m.message_id.as_deref(), Some("m1@example.test"));
        assert_eq!(m.references, ["a@example.test", "b@example.test"]);
        assert!(m.text.as_deref().unwrap().contains("Привет"));
        assert!(m.html.as_deref().unwrap().contains("<p>Привет</p>"));
        assert_eq!(m.parts.len(), 1);
        assert_eq!((m.parts[0].name.as_str(), m.parts[0].mime.as_str(), m.parts[0].size), ("report.pdf", "application/pdf", 5));
        assert_eq!(part(SAMPLE.as_bytes(), 0).unwrap(), b"Hello");
        assert!(part(SAMPLE.as_bytes(), 1).is_err());
        assert!(parse(b"").is_err() || parse(b"").unwrap().text.is_none());
    }

    /// Синтетический «файл»: псевдослучайные байты (все 256 значений).
    fn file(len: usize, seed: u32) -> Vec<u8> {
        let mut x = seed;
        (0..len)
            .map(|_| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (x >> 24) as u8
            })
            .collect()
    }

    /// base64 строками по 76 знаков через CRLF, без перевода строки в конце.
    fn base64(bytes: &[u8]) -> String {
        const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut flat = Vec::new();
        for c in bytes.chunks(3) {
            let n = (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
            for i in 0..4 {
                flat.push(if i <= c.len() { ABC[(n >> (18 - 6 * i)) as usize & 63] } else { b'=' });
            }
        }
        flat.chunks(76).map(|l| std::str::from_utf8(l).unwrap()).collect::<Vec<_>>().join("\r\n")
    }

    /// quoted-printable: печатное ASCII как есть, остальное — `=XX`, мягкие переносы `=` CRLF.
    fn quoted_printable(bytes: &[u8]) -> String {
        let mut out = String::new();
        let mut line = 0;
        for &b in bytes {
            let enc = if (33..=126).contains(&b) && b != b'=' { (b as char).to_string() } else { format!("={b:02X}") };
            if line + enc.len() > 75 {
                out.push_str("=\r\n");
                line = 0;
            }
            line += enc.len();
            out.push_str(&enc);
        }
        out
    }

    fn attachment(boundary: &str, ctype: &str, cte: &str, body: &str) -> String {
        format!("--{boundary}\r\nContent-Type: {ctype}\r\nContent-Disposition: attachment\r\nContent-Transfer-Encoding: {cte}\r\n\r\n{body}\r\n")
    }

    fn letter(ctype: &str, body: &str) -> String {
        format!("From: a@example.test\r\nTo: b@example.test\r\nSubject: test\r\nMIME-Version: 1.0\r\nContent-Type: {ctype}\r\n\r\n{body}")
    }

    /// Два вложения: диапазоны — ровно их тела в письме, раскодированное — исходные файлы; мелкие части — в скелете.
    #[test]
    fn split_gives_ranges_and_decoded_files() {
        let pdf = file(100_000, 1);
        // Текст в KOI8-R: байты — как в файле, без перекодировки в UTF-8.
        let koi = file(90_000, 2);
        let (b64, qp) = (base64(&pdf), quoted_printable(&koi));
        let body = "--b1\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nТело\r\n".to_owned()
            + &attachment("b1", "application/pdf; name=\"a.pdf\"", "base64", &b64)
            + &attachment("b1", "text/plain; charset=koi8-r; name=\"b.txt\"", "quoted-printable", &qp)
            + &attachment("b1", "application/octet-stream", "base64", &base64(b"small"))
            + "--b1--\r\n";
        let raw = letter("multipart/mixed; boundary=\"b1\"", &body);
        let (s, bin) = split(raw.as_bytes()).unwrap();
        assert_eq!(s.parts.len(), 2, "{s:?}");
        let (a, b) = (&s.parts[0], &s.parts[1]);
        assert_eq!((a.encoding, b.encoding), (Encoding::Base64, Encoding::QuotedPrintable));
        let range = |p: &super::SplitPart| &raw.as_bytes()[p.range.start as usize..p.range.end as usize];
        assert_eq!(range(a), b64.as_bytes());
        assert_eq!(range(b), qp.as_bytes());
        assert_eq!((a.decoded_len, b.decoded_len), (100_000, 90_000));
        assert_eq!(&bin[..100_000], pdf.as_slice());
        assert_eq!(&bin[100_000..], koi.as_slice());
        assert!(s.fits(raw.len(), bin.len()));
        assert!(!s.fits(raw.len(), bin.len() + 1));
        assert!(!s.fits(a.range.end as usize, bin.len()));
    }

    /// `multipart/signed`: части внутри подписанного — тоже; вложенное письмо без кодировки — рекурсивно.
    #[test]
    fn split_looks_inside_signed_and_nested() {
        let doc = file(70_000, 3);
        let b64 = base64(&doc);
        let inner = "--m\r\nContent-Type: text/plain\r\n\r\nПодписано\r\n".to_owned()
            + &attachment("m", "application/octet-stream", "base64", &b64)
            + "--m--";
        let signed = "--s\r\nContent-Type: multipart/mixed; boundary=\"m\"\r\n\r\n".to_owned()
            + &inner
            + "\r\n--s\r\nContent-Type: application/pkcs7-signature; name=\"smime.p7s\"\r\nContent-Transfer-Encoding: base64\r\n\r\nMIIB\r\n--s--\r\n";
        let raw = letter("multipart/signed; protocol=\"application/pkcs7-signature\"; micalg=sha-256; boundary=\"s\"", &signed);
        let (s, bin) = split(raw.as_bytes()).unwrap();
        assert_eq!(s.parts.len(), 1, "{s:?}");
        assert_eq!(&raw.as_bytes()[s.parts[0].range.start as usize..s.parts[0].range.end as usize], b64.as_bytes());
        assert_eq!(bin, doc);

        let forwarded = letter("multipart/mixed; boundary=\"m\"", &(inner.clone() + "\r\n"));
        let outer =
            "--f\r\nContent-Type: text/plain\r\n\r\nПересылаю\r\n--f\r\nContent-Type: message/rfc822\r\n\r\n".to_owned() + &forwarded + "--f--\r\n";
        let raw = letter("multipart/mixed; boundary=\"f\"", &outer);
        let (s, bin) = split(raw.as_bytes()).unwrap();
        assert_eq!(s.parts.len(), 1, "{s:?}");
        assert_eq!(&raw.as_bytes()[s.parts[0].range.start as usize..s.parts[0].range.end as usize], b64.as_bytes());
        assert_eq!(bin, doc);
    }

    /// Зашифрованное S/MIME не разбирается: ни целиком, ни вложением.
    #[test]
    fn split_skips_encrypted() {
        let cms = base64(&file(80_000, 4));
        let raw = letter("application/pkcs7-mime; smime-type=enveloped-data; name=\"smime.p7m\"", &format!("{cms}\r\n"));
        let raw = raw.replace("MIME-Version: 1.0\r\n", "MIME-Version: 1.0\r\nContent-Transfer-Encoding: base64\r\n");
        let (s, bin) = split(raw.as_bytes()).unwrap();
        assert!(s.parts.is_empty() && bin.is_empty(), "{s:?}");

        let body = attachment("b", "application/x-pkcs7-mime; smime-type=enveloped-data", "base64", &cms) + "--b--\r\n";
        let (s, bin) = split(letter("multipart/mixed; boundary=\"b\"", &body).as_bytes()).unwrap();
        assert!(s.parts.is_empty() && bin.is_empty(), "{s:?}");
    }

    /// Не письмо — ошибка; повреждённые части (без конца, не base64, не quoted-printable) — в скелете; всё без паники.
    /// Больше частей, чем предел, — ошибка.
    #[test]
    fn split_rejects_broken_and_too_many_parts() {
        for junk in [&b""[..], b"\r\n\r\n", b"\x00\xff\xfe\r\n\r\n"] {
            assert!(split(junk).is_err(), "{junk:?}");
        }
        let b64 = base64(&file(70_000, 5));
        let broken = [
            letter("multipart/mixed; boundary=\"b\"", &attachment("b", "application/pdf", "base64", &b64)),
            letter("multipart/mixed; boundary=\"b\"", &attachment("b", "application/pdf", "base64", &b64[..b64.len() / 2])[..1000]),
            letter("multipart/mixed; boundary=\"b\"", &(attachment("b", "application/pdf", "base64", &"%!".repeat(40_000)) + "--b--\r\n")),
            letter("application/pdf", &"=ZZ".repeat(30_000)).replace("MIME", "Content-Transfer-Encoding: quoted-printable\r\nMIME"),
            letter("message/rfc822", "--\r\n\r\n--"),
            String::from_utf8_lossy(&file(100_000, 6)).into_owned(),
        ];
        for raw in &broken {
            let (s, bin) = split(raw.as_bytes()).unwrap();
            assert!(s.parts.is_empty() && bin.is_empty(), "{s:?}");
        }

        let parts = |n: usize| letter("multipart/mixed; boundary=\"b\"", &("--b\r\nContent-Type: text/plain\r\n\r\nx\r\n".repeat(n) + "--b--\r\n"));
        // Корень и вложения: ровно предел — можно, на одну больше — нет.
        assert!(split(parts(MAX_SPLIT_PARTS - 1).as_bytes()).is_ok());
        let e = split(parts(MAX_SPLIT_PARTS).as_bytes()).unwrap_err();
        assert!(e.contains("частей"), "{e}");
    }
}
