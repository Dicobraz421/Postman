//! PST, пробный файл (S8-33а): наш файл открывает `outlook-pst` (Microsoft, только чтение) — хранилище, IPM Subtree,
//! наша папка в нём, письмо со свойствами, получателями и вложением читаются. Адреса — только `example.test`.
//!
//! Файл для ручной проверки в Outlook (`docs/manual-tests/S8-33а.md`): `POSTMAN_EXPORT_SAMPLES=<каталог>` — тест
//! кладёт туда `S8-33а пробный.pst`.
//!
//! Требования scanpst (S8-40): узлы нового файла Outlook (папки приёма, очередь исходящих, шаблоны, HMP, SAL), столбцы
//! шаблонов, ключи BTH, `PR_ATTACH_SIZE`, строки таблиц — те же свойства, что у объектов.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // тест: «не падать» — для кода приложения (S2-03)

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::Path;
use std::rc::Rc;

use outlook_pst::ltp::prop_context::PropertyValue;
use outlook_pst::ltp::table_context::{TableContext, TableRowId, UnicodeTableContext};
use outlook_pst::messaging::attachment::{Attachment as _, AttachmentData, UnicodeAttachment};
use outlook_pst::messaging::message::{Message as _, UnicodeMessage};
use outlook_pst::messaging::store::{Store as _, UnicodeStore};
use outlook_pst::ndb::node_id::NodeId;
use outlook_pst::ndb::page::NodeBTreeEntry as _;
use outlook_pst::{PstFile as _, UnicodePstFile};
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
    // PR_ATTACH_SIZE — сумма размеров всех свойств вложения, как у Outlook (scanpst: «missing or invalid»).
    assert_eq!(att.properties().attachment_size().unwrap(), props_size(att.properties().iter()));
    assert!(att.properties().attachment_size().unwrap() > attachment.len() as i32);
    assert_eq!(att.properties().rendering_position().unwrap(), -1);
    assert_eq!(unicode(att.properties().get(0x3707)), "отчёт.txt");
    assert_eq!(unicode(att.properties().get(0x370E)), "text/plain");
    match att.data() {
        Some(AttachmentData::Binary(b)) => assert_eq!(b.buffer(), attachment),
        _ => panic!("нет данных вложения"),
    }
}

const MSGFLAG_HASATTACH: i32 = 0x10;

/// Журнал scanpst пробного файла S8-33а (`docs/tasks/S8-40.md`) — по строке на проверку.
#[test]
fn scanpst_requirements() {
    let messages = [letter("Строка отчёта\r\n".repeat(4).into_bytes(), false)];
    let bytes = write("Postman", &[Folder { name: "Входящие", messages: &messages }]);
    let pst = Rc::new(UnicodePstFile::read_from(Box::new(Cursor::new(bytes))).unwrap());
    let store = UnicodeStore::read(pst.clone()).unwrap();
    let file = File { pst, store: store.clone() };

    // Шаблоны: столбцы — как в новом файле Outlook; строк нет. Ключи BTH — по возрастанию, без повторов и без нуля.
    let templates: [(u32, &[Tag]); 9] = [
        (0x60D, &[0x0E30_0102, 0x0E33_0014, 0x0E34_0102, 0x0E38_0003, 0x3001_001F, 0x3602_0003, 0x3603_0003, 0x360A_000B, 0x3613_001F]),
        (0x60E, &[0x001A_001F, 0x0037_001F, 0x0042_001F, 0x0E06_0040, 0x0E07_0003, 0x0E08_0003, 0x0E17_0003]),
        (0x60F, &[0x001A_001F, 0x0E07_0003, 0x3001_001F, 0x6800_001F, 0x7003_0003]),
        (0x610, &[0x0E05_001F, 0x0E2A_000B, 0x67F1_0003]),
        (0x671, &[0x0E20_0003, 0x3704_001F, 0x3705_0003, 0x370B_0003]),
        (0x692, &RECIPIENT_COLUMNS),
        (0x6B6, &[0x0E33_0014, 0x0E37_0102, 0x0E38_0003]),
        (0x6D7, &[0x001A_001F, 0x0E30_0102, 0x0E31_0102, 0x0E33_0014, 0x0E34_0102, 0x0E38_0003, 0x0E3E_0102]),
        (0x6F8, &[0x0E33_0014, 0x3007_0040]),
    ];
    for (nid, columns) in templates {
        let t = table(&file, nid);
        let have = tags(t.as_ref());
        for c in columns {
            assert!(have.contains(c), "шаблон {nid:X}: нет столбца {c:08X}");
        }
        assert_eq!(t.rows_matrix().count(), 0, "шаблон {nid:X}");
        assert!(bth_keys(&file, nid).is_empty());
    }

    // Таблица папок приёма: класс по умолчанию ("") — в корневую папку. Очередь исходящих — пустая таблица.
    let receive = table(&file, 0x62B);
    assert_eq!(tags(receive.as_ref()), [0x001A_001F, 0x6605_0003, 0x67F2_0003, 0x67F3_0003]);
    let rows: Vec<_> = receive.rows_matrix().map(|r| u32::from(r.id())).collect();
    assert_eq!(rows, [1]);
    assert_eq!(int(column(receive.as_ref(), 0x6605_0003)[0].1.as_ref()), 0x122);
    // Класс "" — ячейка есть, HNID 0, как у Outlook (`outlook-pst` пустую строку с HNID 0 в таблице не читает).
    let context = receive.context();
    let class = context.columns().iter().position(|c| c.prop_id() == 0x001A).unwrap();
    let cells = receive.rows_matrix().next().unwrap().columns(context).unwrap();
    assert!(cells[class].is_some());
    assert_eq!(bth_keys(&file, 0x62B), [1]);
    let outgoing = table(&file, 0x64C);
    assert_eq!(tags(outgoing.as_ref()), [0x0039_0040, 0x0E10_0003, 0x0E14_0003, 0x67F2_0003, 0x67F3_0003]);
    assert_eq!(outgoing.rows_matrix().count(), 0);

    // HMP (0xC01): куча клиента 0x9C, корень — HID пустого BTH (ключ 16 байт, значение 4).
    let hmp = node_bytes(&file, 0xC01);
    assert_eq!(hmp[2..4], [0xEC, 0x9C]);
    let root = u32::from_le_bytes(hmp[4..8].try_into().unwrap());
    let bth = heap(&hmp, u32::from_le_bytes(heap(&hmp, root).try_into().unwrap()));
    assert_eq!(bth, [0xB5, 16, 4, 0, 0, 0, 0, 0]);
    // Узел 0xEC1 есть (пустой, как в новом файле Outlook); очередь обновлений папки поиска спама — тоже.
    assert_eq!(u64::from(file.pst.read_node(NodeId::from(0xEC1)).unwrap().data()), 0);
    file.pst.read_node(NodeId::from(0x2226)).unwrap();
    // SAL: NID папки поиска спама, она — в корне.
    assert_eq!(node_bytes(&file, 0x201), 0x2223u32.to_le_bytes());
    let root_rows: Vec<u32> = table(&file, 0x12D).rows_matrix().map(|r| u32::from(r.id())).collect();
    assert!(root_rows.contains(&0x2223));

    // Строки иерархии, содержимого и вложений — ровно свойства объектов; ключи BTH — по возрастанию.
    let props = store.properties();
    let ipm = store.open_folder(&props.ipm_sub_tree_entry_id().unwrap()).unwrap();
    let hierarchy = ipm.hierarchy_table().unwrap();
    for row in hierarchy.rows_matrix() {
        let folder = store.open_folder(&props.make_entry_id(NodeId::from(u32::from(row.id()))).unwrap()).unwrap();
        let object: BTreeMap<u16, String> = folder.properties().iter().map(|(id, v)| (*id, format!("{v:?}"))).collect();
        same(hierarchy.as_ref(), u32::from(row.id()), &object);
    }
    assert_eq!(bth_keys(&file, 0x802D), [0x8062, 0x8082]);
    let inbox = store.open_folder(&props.make_entry_id(NodeId::from(0x8082)).unwrap()).unwrap();
    let contents = inbox.contents_table().unwrap();
    let id = u32::from(contents.rows_matrix().next().unwrap().id());
    assert_eq!(bth_keys(&file, 0x808E), [id]);
    let message = UnicodeMessage::read(store.clone(), &props.make_entry_id(NodeId::from(id)).unwrap(), None).unwrap();
    let object: BTreeMap<u16, String> = message.properties().iter().map(|(id, v)| (*id, format!("{v:?}"))).collect();
    same(contents.as_ref(), id, &object);

    let attachments = message.attachment_table().unwrap();
    let mut total = 0;
    for row in attachments.rows_matrix() {
        let att = UnicodeAttachment::read(message.clone(), NodeId::from(u32::from(row.id())), None).unwrap();
        let ap = att.properties();
        assert_eq!(ap.attachment_size().unwrap(), props_size(ap.iter()));
        total += ap.attachment_size().unwrap();
        let object: BTreeMap<u16, String> = ap.iter().map(|(id, v)| (*id, format!("{v:?}"))).collect();
        same(attachments.as_ref(), u32::from(row.id()), &object);
    }
    // Сумма вложений (0x6604, Outlook); размер письма — больше суммы его вложений.
    assert_eq!(int(message.properties().get(0x6604)), total);
    assert!(message.properties().message_size().unwrap() > total);

    // Получатели: все обязательные столбцы шаблона заполнены; LtpRowId — с 1, без повторов, индекс строк сходится.
    let recipients = message.recipient_table().unwrap();
    let ids: Vec<u32> = recipients.rows_matrix().map(|r| u32::from(r.id())).collect();
    assert_eq!(ids, [1, 2]);
    for id in ids {
        assert_eq!(u32::from(recipients.find_row(TableRowId::new(id)).unwrap().id()), id);
        let row = row_values(recipients.as_ref(), id);
        for c in RECIPIENT_COLUMNS {
            assert!(row.contains_key(&c), "получатель {id}: нет значения {c:08X}");
        }
    }
    assert_eq!(binary(column(recipients.as_ref(), 0x300B_0102)[0].1.as_ref()), b"SMTP:PETR@EXAMPLE.TEST\0");
    let petr = row_values(recipients.as_ref(), 1);
    assert_eq!(petr[&0x0FFF_0102], petr[&0x0FF9_0102]);
}

/// Обязательные столбцы таблицы получателей (scanpst: «TC (nid=692) missing required column»).
const RECIPIENT_COLUMNS: [Tag; 12] = [
    0x0E0F_000B,
    0x3002_001F,
    0x3003_001F,
    0x0FFF_0102,
    0x3001_001F,
    0x0C15_0003,
    0x300B_0102,
    0x0FF9_0102,
    0x0FFE_0003,
    0x3900_0003,
    0x3A40_000B,
    0x39FF_001F,
];

/// Файл и хранилище поверх него: узлы — по NID через `outlook-pst`.
struct File {
    pst: Rc<UnicodePstFile>,
    store: Rc<UnicodeStore>,
}

fn table(file: &File, nid: u32) -> Rc<dyn TableContext> {
    let node = file.pst.read_node(NodeId::from(nid)).unwrap();
    UnicodeTableContext::read(file.store.clone(), node).unwrap()
}

fn tags(table: &dyn TableContext) -> Vec<Tag> {
    let mut tags: Vec<Tag> = table.context().columns().iter().map(|c| u32::from(c.prop_id()) << 16 | u32::from(u16::from(c.prop_type()))).collect();
    tags.sort();
    tags
}

/// Данные узла (один блок кучи) — через `outlook-pst`.
fn node_bytes(file: &File, nid: u32) -> Vec<u8> {
    let node = file.pst.read_node(NodeId::from(nid)).unwrap();
    file.pst.read_block(node.data()).unwrap()
}

/// Размещение кучи из одного блока (MS-PST 2.3.1).
fn heap(page: &[u8], hid: u32) -> Vec<u8> {
    assert_eq!(hid >> 16, 0, "куча в одном блоке");
    let index = ((hid >> 5) & 0x7FF) as usize;
    let map = usize::from(u16::from_le_bytes([page[0], page[1]]));
    let at = |i: usize| usize::from(u16::from_le_bytes([page[map + 4 + 2 * i], page[map + 5 + 2 * i]]));
    page[at(index - 1)..at(index)].to_vec()
}

/// Ключи BTH индекса строк таблицы (`hidRowIndex` в TCINFO) в порядке записи; проверяет порядок — строго по
/// возрастанию — и что ключа 0 нет (scanpst: «keys overlap (dwkey=0, dwkeyMin=0)»).
fn bth_keys(file: &File, nid: u32) -> Vec<u32> {
    let page = node_bytes(file, nid);
    assert_eq!(page[3], 0x7C);
    let info = heap(&page, u32::from_le_bytes(page[4..8].try_into().unwrap()));
    let header = heap(&page, u32::from_le_bytes(info[10..14].try_into().unwrap()));
    assert_eq!(header[..4], [0xB5, 4, 4, 0]);
    let root = u32::from_le_bytes(header[4..8].try_into().unwrap());
    let keys: Vec<u32> =
        if root == 0 { Vec::new() } else { heap(&page, root).chunks(8).map(|r| u32::from_le_bytes(r[..4].try_into().unwrap())).collect() };
    let mut min = 0;
    for &k in &keys {
        assert!(k > min, "{nid:X}: ключ {k:X} после {min:X}");
        min = k;
    }
    keys
}

/// Значения строки таблицы, кроме `LtpRowId` и `LtpRowVer`: тег → значение (`Debug`).
fn row_values(table: &dyn TableContext, id: u32) -> BTreeMap<Tag, String> {
    let context = table.context();
    let row = table.find_row(TableRowId::new(id)).unwrap();
    let cells = row.columns(context).unwrap();
    context
        .columns()
        .iter()
        .zip(cells)
        .filter_map(|(c, cell)| {
            let tag = u32::from(c.prop_id()) << 16 | u32::from(u16::from(c.prop_type()));
            let value = table.read_column(&cell?, c.prop_type()).unwrap();
            (tag >> 16 != 0x67F2 && tag >> 16 != 0x67F3).then(|| (tag, format!("{value:?}")))
        })
        .collect()
}

/// Строка таблицы `id` и объект (свойства по ID): каждое значение строки — у объекта такое же, а каждое свойство
/// объекта, для которого есть столбец, — в строке (MS-PST 2.4.4.3; scanpst: «row doesn't match sub-object»).
fn same(table: &dyn TableContext, id: u32, object: &BTreeMap<u16, String>) {
    let row = row_values(table, id);
    for (tag, value) in &row {
        assert_eq!(object.get(&((tag >> 16) as u16)), Some(value), "строка {id:X}, свойство {tag:08X}");
    }
    for tag in tags(table) {
        let pid = (tag >> 16) as u16;
        if pid != 0x67F2 && pid != 0x67F3 && object.contains_key(&pid) {
            assert!(row.contains_key(&tag), "строка {id:X}: нет {tag:08X}");
        }
    }
}

/// Сумма размеров свойств — `PR_ATTACH_SIZE` по Outlook: числа по ширине, строки (UTF-16) и байты — по длине.
fn props_size<'a>(props: impl Iterator<Item = (&'a u16, &'a PropertyValue)>) -> i32 {
    props
        .map(|(_, v)| match v {
            PropertyValue::Integer32(_) => 4,
            PropertyValue::Boolean(_) => 1,
            PropertyValue::Time(_) => 8,
            PropertyValue::Unicode(s) => 2 * s.to_string().encode_utf16().count() as i32,
            PropertyValue::Binary(b) => b.buffer().len() as i32,
            other => panic!("нет в пробном файле: {other:?}"),
        })
        .sum()
}

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
