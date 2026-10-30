//! MSG (S8-30): наш файл читает `msg_parser` (чужой читатель, MIT) — тема, отправитель, получатели, тело, вложения
//! совпадают с письмом; вложенное письмо; подписанное — вложение с исходными байтами письма. Письма — как их собирает
//! воркер (S8-29), адреса — только `example.test`.
//!
//! Файлы для ручной проверки в Outlook (`docs/manual-tests/S8-33а.md`): `POSTMAN_EXPORT_SAMPLES=<каталог>` — тесты
//! кладут туда свои `.msg`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // тест: «не падать» — для кода приложения (S2-03)

use std::io::{Cursor, Read, Write};
use std::path::Path;

use cfb::CompoundFile;
use msg_parser::Outlook;
use postman_export::mapi::*;
use postman_export::msg;

fn text(tag: Tag, s: &str) -> (Tag, Value) {
    (tag, Value::Unicode(s.into()))
}

fn person(kind: i32, name: &str, email: &str) -> Recipient {
    Recipient {
        props: vec![
            (PR_RECIPIENT_TYPE, Value::Long(kind)),
            text(PR_DISPLAY_NAME_W, name),
            text(PR_ADDRTYPE_W, ADDRTYPE_SMTP),
            text(PR_EMAIL_ADDRESS_W, email),
            text(PR_SMTP_ADDRESS_W, email),
        ],
    }
}

fn by_value(name: &str, mime: &str, cid: Option<&str>, data: &[u8]) -> Attachment {
    let mut props = vec![(PR_ATTACH_METHOD, Value::Long(ATTACH_BY_VALUE)), text(PR_ATTACH_LONG_FILENAME_W, name), text(PR_ATTACH_MIME_TAG_W, mime)];
    if let Some(cid) = cid {
        props.push(text(PR_ATTACH_CONTENT_ID_W, cid));
    }
    props.push((PR_ATTACH_DATA_BIN, Value::Binary(data.to_vec())));
    Attachment { props, embedded: None }
}

/// 2026-10-30 07:15:00 UTC в FILETIME.
const SENT: u64 = FILETIME_UNIX_EPOCH + 1_793_344_500 * 10_000_000;

/// Письмо, как его собирает воркер: тема с приставкой, отправитель, кому, копия, скрытая, тело, HTML, два вложения.
fn letter() -> MapiMessage {
    MapiMessage {
        class: IPM_NOTE.into(),
        props: vec![
            text(PR_SUBJECT_W, "Re: Отчёт за октябрь"),
            text(PR_SUBJECT_PREFIX_W, "Re: "),
            text(PR_NORMALIZED_SUBJECT_W, "Отчёт за октябрь"),
            text(PR_SENDER_NAME_W, "Анна Иванова"),
            text(PR_SENDER_ADDRTYPE_W, ADDRTYPE_SMTP),
            text(PR_SENDER_EMAIL_ADDRESS_W, "anna@example.test"),
            text(PR_SENDER_SMTP_ADDRESS_W, "anna@example.test"),
            text(PR_SENT_REPRESENTING_NAME_W, "Анна Иванова"),
            text(PR_SENT_REPRESENTING_ADDRTYPE_W, ADDRTYPE_SMTP),
            text(PR_SENT_REPRESENTING_EMAIL_ADDRESS_W, "anna@example.test"),
            text(PR_SENT_REPRESENTING_SMTP_ADDRESS_W, "anna@example.test"),
            text(PR_DISPLAY_TO_W, "Пётр; ivan@example.test"),
            text(PR_DISPLAY_CC_W, "team@example.test"),
            text(PR_DISPLAY_BCC_W, "boss@example.test"),
            (PR_CLIENT_SUBMIT_TIME, Value::SysTime(SENT)),
            (PR_MESSAGE_DELIVERY_TIME, Value::SysTime(SENT + 60 * 10_000_000)),
            text(PR_INTERNET_MESSAGE_ID_W, "<m1@example.test>"),
            text(PR_IN_REPLY_TO_ID_W, "<m0@example.test>"),
            text(
                PR_TRANSPORT_MESSAGE_HEADERS_W,
                "From: =?utf-8?b?0JDQvdC90LAg0JjQstCw0L3QvtCy0LA=?= <anna@example.test>\r\nSubject: Re: report\r\nDate: Fri, 30 Oct 2026 10:15:00 +0300\r\nMessage-ID: <m1@example.test>\r\n\r\n",
            ),
            text(PR_BODY_W, "Привет!\r\nОтчёт во вложении.\r\n"),
            (PR_HTML, Value::Binary("<p>Привет!</p><p><img src=\"cid:logo@example.test\"></p>".as_bytes().to_vec())),
            (PR_MESSAGE_FLAGS, Value::Long(MSGFLAG_READ)),
            (PR_INTERNET_CPID, Value::Long(CP_UTF8)),
        ],
        recipients: vec![
            person(MAPI_TO, "Пётр", "petr@example.test"),
            person(MAPI_TO, "ivan@example.test", "ivan@example.test"),
            person(MAPI_CC, "team@example.test", "team@example.test"),
            person(MAPI_BCC, "boss@example.test", "boss@example.test"),
        ],
        attachments: vec![
            by_value("отчёт.pdf", "application/pdf", None, b"%PDF-1.4 not really a pdf\n\x00\x01\x02\xff"),
            by_value("logo.png", "image/png", Some("logo@example.test"), b"\x89PNG\r\n\x1a\n...."),
        ],
    }
}

fn file(m: &MapiMessage) -> Vec<u8> {
    let mut out = Vec::new();
    msg::write(&mut out, m).unwrap();
    out
}

/// Файл для ручной проверки, если задан `POSTMAN_EXPORT_SAMPLES`.
fn sample(name: &str, bytes: &[u8]) {
    if let Some(dir) = std::env::var_os("POSTMAN_EXPORT_SAMPLES") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(Path::new(&dir).join(name), bytes).unwrap();
    }
}

fn names(list: &[msg_parser::Person]) -> Vec<(&str, &str)> {
    list.iter().map(|p| (p.name.as_str(), p.email.as_str())).collect()
}

#[test]
fn letter_reads_back() {
    let bytes = file(&letter());
    sample("S8-30 письмо.msg", &bytes);
    let o = Outlook::from_slice(&bytes).unwrap();
    assert_eq!(o.message_class, IPM_NOTE);
    assert_eq!(o.subject, "Re: Отчёт за октябрь");
    assert_eq!((o.sender.name.as_str(), o.sender.email.as_str()), ("Анна Иванова", "anna@example.test"));
    assert_eq!(names(&o.to), [("Пётр", "petr@example.test"), ("ivan@example.test", "ivan@example.test")]);
    assert_eq!(names(&o.cc), [("team@example.test", "team@example.test")]);
    assert_eq!(names(&o.bcc), [("boss@example.test", "boss@example.test")]);
    assert_eq!(o.body, "Привет!\r\nОтчёт во вложении.\r\n");
    assert_eq!(hex_decode(&o.html), "<p>Привет!</p><p><img src=\"cid:logo@example.test\"></p>".as_bytes());
    assert_eq!(o.client_submit_time, "2026-10-30T07:15:00Z");
    assert_eq!(o.message_delivery_time, "2026-10-30T07:16:00Z");
    assert_eq!(o.headers.message_id, "<m1@example.test>");

    assert_eq!(o.attachments.len(), 2);
    let (pdf, png) = (&o.attachments[0], &o.attachments[1]);
    assert_eq!((pdf.long_file_name.as_str(), pdf.display_name.as_str(), pdf.mime_tag.as_str()), ("отчёт.pdf", "отчёт.pdf", "application/pdf"));
    assert_eq!(pdf.payload_bytes, b"%PDF-1.4 not really a pdf\n\x00\x01\x02\xff");
    assert_eq!(pdf.attach_method, 1);
    assert_eq!((png.long_file_name.as_str(), png.content_id.as_str()), ("logo.png", "logo@example.test"));
    assert_eq!(png.payload_bytes, b"\x89PNG\r\n\x1a\n....");
}

/// Вложенное письмо: у вложения метод 5 и хранилище письма. `msg_parser` вложенное письмо как хранилище не читает
/// (и сливает его получателей с получателями верхнего), поэтому хранилище выносится в отдельный файл — как это делает
/// Outlook при «Сохранить как»: заголовок потока свойств 24 байта → 32.
#[test]
fn embedded_letter() {
    let inner = MapiMessage {
        class: IPM_NOTE.into(),
        props: vec![
            text(PR_SUBJECT_W, "Исходное письмо"),
            text(PR_SENDER_NAME_W, "Пётр"),
            text(PR_SENDER_SMTP_ADDRESS_W, "petr@example.test"),
            text(PR_BODY_W, "Вложенный текст"),
            (PR_MESSAGE_DELIVERY_TIME, Value::SysTime(SENT)),
        ],
        recipients: vec![person(MAPI_TO, "Анна Иванова", "anna@example.test")],
        attachments: vec![],
    };
    let mut outer = letter();
    outer.attachments.push(Attachment {
        props: vec![(PR_ATTACH_METHOD, Value::Long(ATTACH_EMBEDDED_MSG)), text(PR_ATTACH_MIME_TAG_W, "message/rfc822")],
        embedded: Some(Box::new(inner)),
    });
    let bytes = file(&outer);
    sample("S8-30 вложенное письмо.msg", &bytes);

    let o = Outlook::from_slice(&bytes).unwrap();
    assert_eq!(o.subject, "Re: Отчёт за октябрь");
    assert_eq!(o.attachments.len(), 3);
    let att = &o.attachments[2];
    assert!(att.is_embedded_message());
    assert_eq!(att.display_name, "Исходное письмо");

    let nested = Outlook::from_slice(extract(&bytes, "/__attach_version1.0_#00000002/__substg1.0_3701000D")).unwrap();
    assert_eq!(nested.message_class, IPM_NOTE);
    assert_eq!(nested.subject, "Исходное письмо");
    assert_eq!((nested.sender.name.as_str(), nested.sender.email.as_str()), ("Пётр", "petr@example.test"));
    assert_eq!(names(&nested.to), [("Анна Иванова", "anna@example.test")]);
    assert_eq!(nested.body, "Вложенный текст");
    assert_eq!(nested.message_delivery_time, "2026-10-30T07:15:00Z");
}

/// Подписанное `multipart/signed`: класс `IPM.Note.SMIME.MultipartSigned`, вложение — всё письмо исходными байтами.
#[test]
fn signed_letter() {
    let raw =
        b"Content-Type: multipart/signed; protocol=\"application/pkcs7-signature\"; boundary=b\r\nSubject: s\r\n\r\n--b\r\n\r\ntext\r\n--b--\r\n";
    let m = MapiMessage {
        class: IPM_NOTE_SMIME_MULTIPART_SIGNED.into(),
        props: vec![text(PR_SUBJECT_W, "Подписанное"), text(PR_BODY_W, "text")],
        recipients: vec![person(MAPI_TO, "Пётр", "petr@example.test")],
        attachments: vec![by_value("SMIME.p7m", "multipart/signed", None, raw)],
    };
    let bytes = file(&m);
    sample("S8-30 подписанное.msg", &bytes);
    let o = Outlook::from_slice(&bytes).unwrap();
    assert_eq!(o.message_class, IPM_NOTE_SMIME_MULTIPART_SIGNED);
    assert_eq!(o.attachments.len(), 1);
    assert_eq!(o.attachments[0].long_file_name, "SMIME.p7m");
    assert_eq!(o.attachments[0].mime_tag, "multipart/signed");
    assert_eq!(o.attachments[0].payload_bytes, raw);
    assert_eq!(o.body, "text");

    // Зашифрованное (`application/pkcs7-mime`): `IPM.Note.SMIME`, тела нет, вложение `smime.p7m` — CMS как есть.
    let cms = [0x30, 0x80, 0x06, 0x09, 0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x07, 0x03, 0xA0, 0x80];
    let m = MapiMessage {
        class: IPM_NOTE_SMIME.into(),
        props: vec![text(PR_SUBJECT_W, "Зашифрованное")],
        recipients: vec![],
        attachments: vec![by_value("smime.p7m", "application/pkcs7-mime", None, &cms)],
    };
    let o = Outlook::from_slice(file(&m)).unwrap();
    assert_eq!(o.message_class, IPM_NOTE_SMIME);
    assert_eq!(o.body, "");
    assert_eq!((o.attachments[0].long_file_name.as_str(), o.attachments[0].payload_bytes.as_slice()), ("smime.p7m", &cms[..]));
}

/// Хранилище `root` → отдельный файл `.msg` (заголовок потока свойств — как у верхнего письма).
fn extract(file: &[u8], root: &str) -> Vec<u8> {
    let mut src = CompoundFile::open(Cursor::new(file)).unwrap();
    let mut dst = CompoundFile::create_with_version(cfb::Version::V3, Cursor::new(Vec::new())).unwrap();
    let entries: Vec<_> = src.walk_storage(root).unwrap().map(|e| (e.path().to_owned(), e.is_storage())).collect();
    for (path, is_storage) in entries {
        let rel = path.strip_prefix(root).unwrap();
        if rel.as_os_str().is_empty() {
            continue;
        }
        let target = Path::new("/").join(rel);
        if is_storage {
            dst.create_storage(&target).unwrap();
            continue;
        }
        let mut data = Vec::new();
        src.open_stream(&path).unwrap().read_to_end(&mut data).unwrap();
        if target == Path::new("/__properties_version1.0") {
            data.splice(24..24, [0; 8]);
        }
        dst.create_stream(&target).unwrap().write_all(&data).unwrap();
    }
    dst.flush().unwrap();
    dst.into_inner().into_inner()
}

fn hex_decode(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}
