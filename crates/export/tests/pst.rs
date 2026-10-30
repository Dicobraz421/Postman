//! PST, пробный файл (S8-33а): наш файл открывает `outlook-pst` (Microsoft, только чтение) — хранилище, IPM Subtree,
//! наша папка в нём, письмо со свойствами, получателями и вложением читаются. Адреса — только `example.test`.
//!
//! Файл для ручной проверки в Outlook (`docs/manual-tests/S8-33а.md`): `POSTMAN_EXPORT_SAMPLES=<каталог>` — тест
//! кладёт туда `S8-33а пробный.pst`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // тест: «не падать» — для кода приложения (S2-03)

use std::io::Cursor;
use std::path::Path;
use std::rc::Rc;

use outlook_pst::UnicodePstFile;
use outlook_pst::ltp::prop_context::PropertyValue;
use outlook_pst::ltp::table_context::TableContext;
use outlook_pst::messaging::attachment::{Attachment as _, AttachmentData, UnicodeAttachment};
use outlook_pst::messaging::message::{Message as _, UnicodeMessage};
use outlook_pst::messaging::store::{Store as _, UnicodeStore};
use outlook_pst::ndb::node_id::NodeId;
use postman_export::mapi::*;
use postman_export::pst::{self, Folder};

fn text(tag: Tag, s: &str) -> (Tag, Value) {
    (tag, Value::Unicode(s.into()))
}

/// 2026-10-30 07:15:00 UTC в FILETIME.
const SENT: u64 = FILETIME_UNIX_EPOCH + 1_793_344_500 * 10_000_000;

/// Письмо, как его собирает воркер: тема, отправитель, двое получателей, тело, HTML и одно вложение.
fn letter(attachment: Vec<u8>, seen: bool) -> MapiMessage {
    let person = |kind: i32, name: &str, email: &str| Recipient {
        props: vec![
            (PR_RECIPIENT_TYPE, Value::Long(kind)),
            text(PR_DISPLAY_NAME_W, name),
            text(PR_ADDRTYPE_W, ADDRTYPE_SMTP),
            text(PR_EMAIL_ADDRESS_W, email),
            text(PR_SMTP_ADDRESS_W, email),
        ],
    };
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
            text(PR_DISPLAY_TO_W, "Пётр"),
            text(PR_DISPLAY_CC_W, "team@example.test"),
            (PR_CLIENT_SUBMIT_TIME, Value::SysTime(SENT)),
            (PR_MESSAGE_DELIVERY_TIME, Value::SysTime(SENT + 60 * 10_000_000)),
            text(PR_INTERNET_MESSAGE_ID_W, "<m1@example.test>"),
            text(PR_TRANSPORT_MESSAGE_HEADERS_W, "From: anna@example.test\r\nTo: petr@example.test\r\nSubject: Re: report\r\n\r\n"),
            text(PR_BODY_W, "Привет!\r\nОтчёт во вложении.\r\n"),
            (PR_HTML, Value::Binary("<p>Привет!</p><p>Отчёт во вложении.</p>".as_bytes().to_vec())),
            (PR_MESSAGE_FLAGS, Value::Long(if seen { MSGFLAG_READ } else { 0 })),
            (PR_INTERNET_CPID, Value::Long(CP_UTF8)),
        ],
        recipients: vec![person(MAPI_TO, "Пётр", "petr@example.test"), person(MAPI_CC, "team@example.test", "team@example.test")],
        attachments: vec![Attachment {
            props: vec![
                (PR_ATTACH_METHOD, Value::Long(ATTACH_BY_VALUE)),
                text(PR_ATTACH_LONG_FILENAME_W, "отчёт.txt"),
                text(PR_ATTACH_MIME_TAG_W, "text/plain"),
                (PR_ATTACH_DATA_BIN, Value::Binary(attachment)),
            ],
            embedded: None,
        }],
    }
}

fn write(name: &str, folders: &[Folder]) -> Vec<u8> {
    let mut out = Vec::new();
    pst::write(&mut out, name, folders).unwrap();
    out
}

fn open(bytes: Vec<u8>) -> Rc<UnicodeStore> {
    let pst = UnicodePstFile::read_from(Box::new(Cursor::new(bytes))).unwrap();
    UnicodeStore::read(Rc::new(pst)).unwrap()
}

fn unicode(v: Option<&PropertyValue>) -> String {
    match v {
        Some(PropertyValue::Unicode(s)) => s.to_string(),
        other => panic!("не строка: {other:?}"),
    }
}

fn int(v: Option<&PropertyValue>) -> i32 {
    match v {
        Some(PropertyValue::Integer32(n)) => *n,
        other => panic!("не число: {other:?}"),
    }
}

fn binary(v: Option<&PropertyValue>) -> Vec<u8> {
    match v {
        Some(PropertyValue::Binary(b)) => b.buffer().to_vec(),
        other => panic!("не байты: {other:?}"),
    }
}

/// Строки таблицы: (`LtpRowId`, значение столбца `tag`) для столбцов, что есть.
fn column(table: &dyn TableContext, tag: Tag) -> Vec<(u32, Option<PropertyValue>)> {
    let context = table.context();
    let at = context.columns().iter().position(|c| u32::from(c.prop_id()) << 16 | u32::from(u16::from(c.prop_type())) == tag).unwrap();
    table
        .rows_matrix()
        .map(|row| {
            let cell = row.columns(context).unwrap()[at].clone();
            (u32::from(row.id()), cell.map(|c| table.read_column(&c, context.columns()[at].prop_type()).unwrap()))
        })
        .collect()
}

fn names(table: &dyn TableContext) -> Vec<(u32, String)> {
    column(table, PR_DISPLAY_NAME_W).into_iter().map(|(id, v)| (id, unicode(v.as_ref()))).collect()
}

#[test]
fn trial_file_reads_back() {
    let attachment = "Строка отчёта\r\n".repeat(4).into_bytes();
    let messages = [letter(attachment.clone(), false)];
    let bytes = write("Postman", &[Folder { name: "Входящие", messages: &messages }]);
    sample("S8-33а пробный.pst", &bytes);
    let store = open(bytes);

    let props = store.properties();
    assert_eq!(props.display_name().unwrap(), "Postman");
    assert_eq!(int(props.get(0x35DF)), 0x89);
    let ipm = store.open_folder(&props.ipm_sub_tree_entry_id().unwrap()).unwrap();
    assert_eq!(ipm.properties().display_name().unwrap(), "Top of Outlook data file");
    assert!(ipm.properties().has_sub_folders().unwrap());
    store.open_folder(&props.finder_entry_id().unwrap()).unwrap();
    let deleted = store.open_folder(&props.ipm_wastebasket_entry_id().unwrap()).unwrap();
    assert_eq!(deleted.properties().display_name().unwrap(), "Deleted Items");

    // Корень: IPM Subtree, Finder, папка поиска спама. `root_hierarchy_table` в outlook-pst 1.2 запирает файл дважды
    // (зависает) — корень открывается как папка.
    let root = store.open_folder(&props.make_entry_id(NodeId::from(0x122)).unwrap()).unwrap();
    let root_names: Vec<String> = names(root.hierarchy_table().unwrap().as_ref()).into_iter().map(|(_, n)| n).collect();
    assert_eq!(root_names, ["Top of Outlook data file", "Search Root", "SPAM Search Folder 2"]);

    let children = names(ipm.hierarchy_table().unwrap().as_ref());
    assert_eq!(children.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>(), ["Deleted Items", "Входящие"]);
    let inbox_id = props.make_entry_id(NodeId::from(children[1].0)).unwrap();
    let inbox = store.open_folder(&inbox_id).unwrap();
    let fp = inbox.properties();
    assert_eq!((fp.display_name().unwrap().as_str(), fp.content_count().unwrap(), fp.unread_count().unwrap()), ("Входящие", 1, 1));
    assert!(!fp.has_sub_folders().unwrap());
    assert_eq!(unicode(fp.get(0x3613)), "IPF.Note");

    // Таблица содержимого: одна строка — письмо; тема и отправитель видны без открытия письма.
    let contents = inbox.contents_table().unwrap();
    let subjects = column(contents.as_ref(), PR_SUBJECT_W);
    assert_eq!(subjects.len(), 1);
    assert_eq!(unicode(subjects[0].1.as_ref()), "Re: Отчёт за октябрь");
    assert_eq!(unicode(column(contents.as_ref(), PR_SENT_REPRESENTING_NAME_W)[0].1.as_ref()), "Анна Иванова");
    assert!(inbox.associated_table().unwrap().rows_matrix().next().is_none());

    let message_id = props.make_entry_id(NodeId::from(subjects[0].0)).unwrap();
    let message = UnicodeMessage::read(store.clone(), &message_id, None).unwrap();
    let mp = message.properties();
    assert_eq!(mp.message_class().unwrap(), IPM_NOTE);
    assert_eq!(unicode(mp.get(0x0037)), "Re: Отчёт за октябрь");
    assert_eq!(unicode(mp.get(0x0C1A)), "Анна Иванова");
    assert_eq!(unicode(mp.get(0x5D01)), "anna@example.test");
    assert_eq!(unicode(mp.get(0x1000)), "Привет!\r\nОтчёт во вложении.\r\n");
    assert_eq!(binary(mp.get(0x1013)), "<p>Привет!</p><p>Отчёт во вложении.</p>".as_bytes());
    assert_eq!(unicode(mp.get(0x1035)), "<m1@example.test>");
    assert_eq!(mp.message_flags().unwrap(), MSGFLAG_HASATTACH);
    assert!(mp.message_size().unwrap() > 0);
    assert_eq!(mp.message_status().unwrap(), 0);
    assert_eq!(mp.creation_time().unwrap(), (SENT + 60 * 10_000_000) as i64);
    assert_eq!(mp.last_modification_time().unwrap(), (SENT + 60 * 10_000_000) as i64);
    assert_eq!(mp.search_key().unwrap().len(), 16);
    assert!(matches!(mp.get(0x0039), Some(PropertyValue::Time(t)) if *t == SENT as i64));

    let recipients = message.recipient_table().unwrap();
    let people: Vec<(String, String, i32)> = column(recipients.as_ref(), PR_DISPLAY_NAME_W)
        .into_iter()
        .zip(column(recipients.as_ref(), PR_SMTP_ADDRESS_W))
        .zip(column(recipients.as_ref(), PR_RECIPIENT_TYPE))
        .map(|(((_, n), (_, e)), (_, k))| (unicode(n.as_ref()), unicode(e.as_ref()), int(k.as_ref())))
        .collect();
    assert_eq!(people, [("Пётр".into(), "petr@example.test".into(), MAPI_TO), ("team@example.test".into(), "team@example.test".into(), MAPI_CC)]);

    let attachments = message.attachment_table().unwrap();
    let rows = column(attachments.as_ref(), 0x3704_001F);
    assert_eq!(rows.len(), 1);
    assert_eq!(unicode(rows[0].1.as_ref()), "отчёт.txt");
    let att = UnicodeAttachment::read(message.clone(), NodeId::from(rows[0].0), None).unwrap();
    assert_eq!(att.properties().attachment_method().unwrap(), ATTACH_BY_VALUE);
    assert_eq!(att.properties().attachment_size().unwrap(), attachment.len() as i32);
    assert_eq!(att.properties().rendering_position().unwrap(), -1);
    assert_eq!(unicode(att.properties().get(0x3707)), "отчёт.txt");
    assert_eq!(unicode(att.properties().get(0x370E)), "text/plain");
    match att.data() {
        Some(AttachmentData::Binary(b)) => assert_eq!(b.buffer(), attachment),
        _ => panic!("нет данных вложения"),
    }
}

const MSGFLAG_HASATTACH: i32 = 0x10;

/// Большие значения уходят в подузлы, большая куча — в несколько блоков, большой файл — за первую AMap: вложение
/// 300 КиБ (дерево XBLOCK), тело 20 000 символов, 300 писем в папке (строки таблицы содержимого — подузлом).
#[test]
fn large_values_and_many_letters() {
    let attachment: Vec<u8> = (0..300 * 1024u32).map(|i| (i * 7 % 251) as u8).collect();
    let mut big = letter(attachment.clone(), true);
    let body = "Длинная строка письма. ".repeat(870);
    big.props.retain(|(t, _)| *t != PR_BODY_W);
    big.props.push(text(PR_BODY_W, &body));
    let mut messages = vec![big];
    for i in 0..300 {
        let mut m = letter(format!("вложение {i}").into_bytes(), i % 2 == 0);
        m.props.retain(|(t, _)| *t != PR_SUBJECT_W);
        m.props.insert(0, text(PR_SUBJECT_W, &format!("Письмо {i}")));
        messages.push(m);
    }
    let bytes = write("Большой", &[Folder { name: "Архив", messages: &messages }, Folder { name: "Пустая", messages: &[] }]);
    assert!(bytes.len() > 0x4400 + 0x3E000);
    let store = open(bytes);

    let ipm = store.open_folder(&store.properties().ipm_sub_tree_entry_id().unwrap()).unwrap();
    let children = names(ipm.hierarchy_table().unwrap().as_ref());
    assert_eq!(children.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>(), ["Deleted Items", "Архив", "Пустая"]);
    let archive = store.open_folder(&store.properties().make_entry_id(NodeId::from(children[1].0)).unwrap()).unwrap();
    assert_eq!((archive.properties().content_count().unwrap(), archive.properties().unread_count().unwrap()), (301, 150));

    let contents = archive.contents_table().unwrap();
    let subjects = column(contents.as_ref(), PR_SUBJECT_W);
    assert_eq!(subjects.len(), 301);
    assert_eq!(unicode(subjects[300].1.as_ref()), "Письмо 299");
    let first = props_entry(&store, subjects[0].0);
    let message = UnicodeMessage::read(store.clone(), &first, None).unwrap();
    assert_eq!(unicode(message.properties().get(0x1000)), body);
    let rows = column(message.attachment_table().unwrap().as_ref(), 0x3704_001F);
    let att = UnicodeAttachment::read(message.clone(), NodeId::from(rows[0].0), None).unwrap();
    match att.data() {
        Some(AttachmentData::Binary(b)) => assert_eq!(b.buffer(), attachment),
        _ => panic!("нет данных вложения"),
    }
    let last = UnicodeMessage::read(store.clone(), &props_entry(&store, subjects[300].0), None).unwrap();
    assert_eq!(unicode(last.properties().get(0x0037)), "Письмо 299");
}

/// Вложенное письмо — не для пробного файла.
#[test]
fn embedded_letter_is_refused() {
    let mut m = letter(b"x".to_vec(), true);
    m.attachments.push(Attachment { props: vec![(PR_ATTACH_METHOD, Value::Long(ATTACH_EMBEDDED_MSG))], embedded: Some(Box::default()) });
    let err = pst::write(&mut Vec::new(), "x", &[Folder { name: "x", messages: &[m] }]).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
}

fn props_entry(store: &UnicodeStore, nid: u32) -> outlook_pst::messaging::store::EntryId {
    store.properties().make_entry_id(NodeId::from(nid)).unwrap()
}

/// Файл для ручной проверки, если задан `POSTMAN_EXPORT_SAMPLES`.
fn sample(name: &str, bytes: &[u8]) {
    if let Some(dir) = std::env::var_os("POSTMAN_EXPORT_SAMPLES") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(Path::new(&dir).join(name), bytes).unwrap();
    }
}
