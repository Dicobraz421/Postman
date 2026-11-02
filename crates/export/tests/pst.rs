//! PST, пробный файл (S8-33а): наш файл открывает `outlook-pst` (Microsoft, только чтение) — хранилище, IPM Subtree,
//! наша папка в нём, письмо со свойствами, получателями и вложением читаются. Адреса — только `example.test`.
//!
//! Файл для ручной проверки в Outlook (`docs/manual-tests/S8-33а.md`): `POSTMAN_EXPORT_SAMPLES=<каталог>` — тест
//! кладёт туда `S8-33а пробный.pst`.
//!
//! Требования scanpst (S8-40): узлы нового файла Outlook (папки приёма, очередь исходящих, шаблоны, HMP, SAL), столбцы
//! шаблонов, ключи BTH, `PR_ATTACH_SIZE`, строки таблиц — те же свойства, что у объектов.
//!
//! Полный писатель (S8-33б): 10 000 писем в трёх папках (две — в одной ветке) читаются все, память писателя — не
//! больше 64 МиБ (счётчик аллокатора по потоку теста); вложения больше 8 КиБ и больше 1 МиБ — байт в байт; вложенное
//! письмо (подузел вложения, MS-PST 2.3.3.5) — разбором узлов: `outlook-pst` 1.2 ищет его в поддереве письма, а не
//! вложения; тома — с малым пределом. `POSTMAN_EXPORT_SAMPLES` — ещё `S8-33б 10000.pst`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // тест: «не падать» — для кода приложения (S2-03)

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

use outlook_pst::ltp::prop_context::PropertyValue;
use outlook_pst::ltp::table_context::{TableContext, TableRowId, UnicodeTableContext};
use outlook_pst::messaging::attachment::{Attachment as _, AttachmentData, UnicodeAttachment};
use outlook_pst::messaging::message::{Message as _, UnicodeMessage};
use outlook_pst::messaging::store::{Store as _, UnicodeStore};
use outlook_pst::ndb::block_id::UnicodeBlockId;
use outlook_pst::ndb::node_id::NodeId;
use outlook_pst::ndb::page::{NodeBTreeEntry as _, UnicodeNodeBTreeEntry};
use outlook_pst::{PstFile as _, UnicodePstFile};
use postman_export::mapi::*;
use postman_export::pst::{self, Files, Folder, Memory, Storage, Writer};

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

// --- Полный писатель (S8-33б) ---

/// Счётчик аллокатора по потоку: тесты идут параллельно, писатель — в одном потоке.
struct Counting;

thread_local! {
    static HELD: Cell<i64> = const { Cell::new(0) };
    static PEAK: Cell<i64> = const { Cell::new(0) };
}

fn track(delta: i64) {
    let _ = HELD.try_with(|h| {
        let v = h.get() + delta;
        h.set(v);
        let _ = PEAK.try_with(|p| p.set(p.get().max(v)));
    });
}

// SAFETY: всё выделение — у `System`; сверху только счёт байт.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: тот же договор, что у вызывающего.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            track(layout.size() as i64);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` выделен `System` с этим `layout`.
        unsafe { System.dealloc(ptr, layout) };
        track(-(layout.size() as i64));
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        // SAFETY: `ptr` выделен `System` с этим `layout`.
        let p = unsafe { System.realloc(ptr, layout, new) };
        if !p.is_null() {
            track(new as i64 - layout.size() as i64);
        }
        p
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Начать замер: пик — от того, что поток держит сейчас.
fn measure() -> i64 {
    let now = HELD.with(Cell::get);
    PEAK.with(|p| p.set(now));
    now
}

fn peak_since(base: i64) -> i64 {
    PEAK.with(Cell::get) - base
}

/// Тома на диске: каталог теста во временном каталоге.
struct Dir(PathBuf);

impl Files for Dir {
    fn create(&mut self, name: &str) -> std::io::Result<Box<dyn Storage>> {
        let f = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(self.0.join(name))?;
        Ok(Box::new(f))
    }

    fn remove(&mut self, name: &str) -> std::io::Result<()> {
        std::fs::remove_file(self.0.join(name))
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("postman-pst-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn path(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| n.to_string()).collect()
}

/// Вложение письма `i`: «вложение i», у каждого сотого — 64 КиБ (файл больше 32 МиБ: есть FMap).
fn numbered_data(i: usize) -> Vec<u8> {
    let mut data = format!("вложение {i}").into_bytes();
    if i.is_multiple_of(100) {
        data.extend((0..64 * 1024u32).map(|k| (k.wrapping_mul(2_654_435_761).wrapping_add(i as u32) >> 9) as u8));
    }
    data
}

/// Письмо `i`: тема «Письмо i», вложение [`numbered_data`]; чётные — прочитаны.
fn numbered(i: usize) -> MapiMessage {
    let mut m = letter(numbered_data(i), i.is_multiple_of(2));
    m.props.retain(|(t, _)| *t != PR_SUBJECT_W);
    m.props.insert(0, text(PR_SUBJECT_W, &format!("Письмо {i}")));
    m
}

type PstFolder = Rc<dyn outlook_pst::messaging::folder::Folder>;

fn subfolder(store: &Rc<UnicodeStore>, parent: &PstFolder, name: &str) -> PstFolder {
    let id = names(parent.hierarchy_table().unwrap().as_ref()).into_iter().find(|(_, n)| n == name).unwrap().0;
    store.open_folder(&props_entry(store, id)).unwrap()
}

fn attachment_bytes(message: &Rc<UnicodeMessage>, row: u32) -> Vec<u8> {
    let att = UnicodeAttachment::read(message.clone(), NodeId::from(row), None).unwrap();
    match att.data() {
        Some(AttachmentData::Binary(b)) => b.buffer().to_vec(),
        _ => panic!("нет данных вложения"),
    }
}

/// 10 000 писем в трёх папках («Проекты» — внутри «Входящих»): все папки, письма и вложения читаются, таблицы
/// содержимого сходятся с письмами (многоуровневый BTH, куча в много блоков), память писателя — до 64 МиБ.
#[test]
fn ten_thousand_letters_in_three_folders() {
    const N: usize = 10_000;
    let dir = scratch("10000");
    let folders = [path(&["Входящие"]), path(&["Входящие", "Проекты"]), path(&["Архив"])];
    let started = Instant::now();
    let base = measure();
    let mut w = Writer::new(Dir(dir.clone()), "S8-33б 10000", "Postman", pst::VOLUME_LIMIT).unwrap();
    for (k, f) in folders.iter().enumerate() {
        w.folder(f).unwrap();
        for i in (k..N).step_by(3) {
            w.message(&numbered(i)).unwrap();
        }
    }
    let volumes = w.finish().unwrap();
    let peak = peak_since(base);
    let took = started.elapsed();
    let file = dir.join("S8-33б 10000.pst");
    let size = std::fs::metadata(&file).unwrap().len();
    eprintln!(
        "PST 10 000 писем: пик памяти писателя {:.1} МиБ, {:.1} с, файл {:.1} МиБ",
        peak as f64 / 1_048_576.0,
        took.as_secs_f64(),
        size as f64 / 1_048_576.0
    );
    assert!(peak <= 64 * 1024 * 1024, "пик памяти писателя {peak} байт");
    assert_eq!(volumes, ["S8-33б 10000.pst"]);
    // Рабочие файлы удалены.
    let left: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(left, ["S8-33б 10000.pst"]);
    if let Some(samples) = std::env::var_os("POSTMAN_EXPORT_SAMPLES") {
        std::fs::create_dir_all(&samples).unwrap();
        std::fs::copy(&file, Path::new(&samples).join("S8-33б 10000.pst")).unwrap();
    }

    let pst = Rc::new(UnicodePstFile::open(&file).unwrap());
    let store = UnicodeStore::read(pst.clone()).unwrap();
    let props = store.properties();
    // Версия сборки хранилища — как у Outlook (scanpst: «Store created by build»).
    assert_eq!(int(props.get(0x66FC)), 1_618_611);
    assert_eq!(int(props.get(0x66FA)), 0x000E_0011);
    let ipm = store.open_folder(&props.ipm_sub_tree_entry_id().unwrap()).unwrap();
    let top: Vec<String> = names(ipm.hierarchy_table().unwrap().as_ref()).into_iter().map(|(_, n)| n).collect();
    assert_eq!(top, ["Deleted Items", "Входящие", "Архив"]);
    let inbox = subfolder(&store, &ipm, "Входящие");
    assert!(inbox.properties().has_sub_folders().unwrap());
    let projects = subfolder(&store, &inbox, "Проекты");
    let archive = subfolder(&store, &ipm, "Архив");

    let mut total = 0;
    for (k, folder) in [inbox, projects, archive].iter().enumerate() {
        let expect: Vec<usize> = (k..N).step_by(3).collect();
        let fp = folder.properties();
        assert_eq!(fp.content_count().unwrap() as usize, expect.len());
        assert_eq!(fp.unread_count().unwrap() as usize, expect.iter().filter(|i| *i % 2 == 1).count());
        let contents = folder.contents_table().unwrap();
        let subjects = column(contents.as_ref(), PR_SUBJECT_W);
        assert_eq!(subjects.len(), expect.len());
        for ((id, subject), i) in subjects.iter().zip(&expect) {
            assert_eq!(unicode(subject.as_ref()), format!("Письмо {i}"));
            // Индекс строк (BTH) находит каждую строку.
            assert_eq!(u32::from(contents.find_row(TableRowId::new(*id)).unwrap().id()), *id);
        }
        for (n, ((id, _), i)) in subjects.iter().zip(&expect).enumerate() {
            let message = UnicodeMessage::read(store.clone(), &props_entry(&store, *id), None).unwrap();
            assert_eq!(unicode(message.properties().get(0x0037)), format!("Письмо {i}"));
            let rows = column(message.attachment_table().unwrap().as_ref(), 0x3704_001F);
            assert_eq!(rows.len(), 1);
            assert!(attachment_bytes(&message, rows[0].0) == numbered_data(*i));
            if n % 499 == 0 {
                let object: BTreeMap<u16, String> = message.properties().iter().map(|(id, v)| (*id, format!("{v:?}"))).collect();
                same(contents.as_ref(), *id, &object);
            }
            total += 1;
        }
    }
    assert_eq!(total, N);
    check_maps(&std::fs::read(&file).unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Вложения больше 8 КиБ (дерево XBLOCK) и больше 1 МиБ — байт в байт.
#[test]
fn large_attachments_byte_for_byte() {
    let mid: Vec<u8> = (0..20 * 1024u32).map(|i| (i * 31 % 253) as u8).collect();
    let big: Vec<u8> = (0..3 * 512 * 1024u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
    let mut m = letter(mid.clone(), true);
    m.attachments.push(Attachment {
        props: vec![
            (PR_ATTACH_METHOD, Value::Long(ATTACH_BY_VALUE)),
            text(PR_ATTACH_LONG_FILENAME_W, "большое.bin"),
            (PR_ATTACH_DATA_BIN, Value::Binary(big.clone())),
        ],
        embedded: None,
    });
    let bytes = write("Вложения", &[Folder { name: "Входящие", messages: &[m] }]);
    let store = open(bytes);
    let ipm = store.open_folder(&store.properties().ipm_sub_tree_entry_id().unwrap()).unwrap();
    let inbox = subfolder(&store, &ipm, "Входящие");
    let id = column(inbox.contents_table().unwrap().as_ref(), PR_SUBJECT_W)[0].0;
    let message = UnicodeMessage::read(store.clone(), &props_entry(&store, id), None).unwrap();
    let rows = column(message.attachment_table().unwrap().as_ref(), 0x3704_001F);
    let got: Vec<(String, Vec<u8>)> = rows.iter().map(|(r, n)| (unicode(n.as_ref()), attachment_bytes(&message, *r))).collect();
    assert_eq!(got[0].0, "отчёт.txt");
    assert!(got[0].1 == mid);
    assert_eq!(got[1].0, "большое.bin");
    assert!(got[1].1 == big);
    // Размер письма — больше его вложений.
    assert!(message.properties().message_size().unwrap() as usize > mid.len() + big.len());
}

/// Вложенное письмо (attach method 5): `PR_ATTACH_DATA_OBJ` — NID подузла вложения, в нём письмо со своими
/// получателями и вложениями (и ещё одно вложенное письмо).
#[test]
fn embedded_letter_reads() {
    let inner_data: Vec<u8> = (0..12 * 1024u32).map(|i| (i % 241) as u8).collect();
    let mut deepest = letter("самое внутреннее".as_bytes().to_vec(), true);
    deepest.props.retain(|(t, _)| *t != PR_SUBJECT_W);
    deepest.props.push(text(PR_SUBJECT_W, "Третий уровень"));
    let mut inner = letter(inner_data.clone(), true);
    inner.props.retain(|(t, _)| *t != PR_SUBJECT_W);
    inner.props.push(text(PR_SUBJECT_W, "Пересланное письмо"));
    inner.attachments.push(Attachment {
        props: vec![(PR_ATTACH_METHOD, Value::Long(ATTACH_EMBEDDED_MSG)), text(PR_ATTACH_MIME_TAG_W, "message/rfc822")],
        embedded: Some(Box::new(deepest)),
    });
    let mut m = letter(b"x".to_vec(), false);
    m.attachments.push(Attachment {
        props: vec![
            (PR_ATTACH_METHOD, Value::Long(ATTACH_EMBEDDED_MSG)),
            text(PR_ATTACH_MIME_TAG_W, "message/rfc822"),
            text(PR_ATTACH_LONG_FILENAME_W, "Пересланное.eml"),
        ],
        embedded: Some(Box::new(inner)),
    });
    let bytes = write("Вложенные", &[Folder { name: "Входящие", messages: &[m] }]);
    sample("S8-33б вложенное.pst", &bytes);
    let raw = Raw(bytes.clone());
    let pst = Rc::new(UnicodePstFile::read_from(Box::new(Cursor::new(bytes))).unwrap());
    let store = UnicodeStore::read(pst.clone()).unwrap();
    let ipm = store.open_folder(&store.properties().ipm_sub_tree_entry_id().unwrap()).unwrap();
    let inbox = subfolder(&store, &ipm, "Входящие");
    let id = column(inbox.contents_table().unwrap().as_ref(), PR_SUBJECT_W)[0].0;
    let message = UnicodeMessage::read(store.clone(), &props_entry(&store, id), None).unwrap();
    assert_eq!(message.properties().message_flags().unwrap() & MSGFLAG_HASATTACH, MSGFLAG_HASATTACH);

    // Строка таблицы вложений: метод 5, имя; объект вложения — разбором узлов.
    let table = message.attachment_table().unwrap();
    let methods = column(table.as_ref(), 0x3705_0003);
    assert_eq!(methods.iter().map(|(_, m)| int(m.as_ref())).collect::<Vec<_>>(), [ATTACH_BY_VALUE, ATTACH_EMBEDDED_MSG]);
    let names_col = column(table.as_ref(), 0x3704_001F);
    assert_eq!(unicode(names_col[1].1.as_ref()), "Пересланное.eml");
    let node = pst.read_node(NodeId::from(id)).unwrap();
    let message_subs = raw.subnodes(u64::from(node.sub_node().unwrap()));
    let (adata, asub) = message_subs[&methods[1].0];
    let att = raw.pc(adata, asub);
    let size = int_of(&att[&0x0E20]);
    let (enid, esize) = object(&att[&0x3701]);
    assert!(esize > 12 * 1024 && size as u32 > esize);
    let (edata, esub) = raw.subnodes(asub)[&enid];
    let embedded = raw.pc(edata, esub);
    assert_eq!(text_of(&embedded[&0x0037]), "Пересланное письмо");
    assert_eq!(text_of(&embedded[&0x001A]), IPM_NOTE);
    assert_eq!(int_of(&embedded[&0x0E08]) as u32, esize);
    assert_eq!(text_of(&embedded[&0x1000]), "Привет!\r\nОтчёт во вложении.\r\n");

    // Получатели и вложения вложенного письма — таблицы в его поддереве; данные вложения (12 КиБ, подузел) — те же.
    let inner_subs = raw.subnodes(esub);
    let tc = |nid: u32| {
        let (d, s) = inner_subs[&nid];
        let node = UnicodeNodeBTreeEntry::new(NodeId::from(nid), UnicodeBlockId::from(d), (s != 0).then(|| UnicodeBlockId::from(s)), None);
        UnicodeTableContext::read(store.clone(), node).unwrap()
    };
    let recipients = tc(0x692);
    assert_eq!(names(recipients.as_ref()).into_iter().map(|(_, n)| n).collect::<Vec<_>>(), ["Пётр", "team@example.test"]);
    let attachments = tc(0x671);
    let rows = column(attachments.as_ref(), 0x3705_0003);
    assert_eq!(rows.iter().map(|(_, m)| int(m.as_ref())).collect::<Vec<_>>(), [ATTACH_BY_VALUE, ATTACH_EMBEDDED_MSG]);
    let (d, s) = inner_subs[&rows[0].0];
    let first = raw.pc(d, s);
    assert!(first[&0x3701].1 == inner_data);
    // Третий уровень; имя вложения без имени файла — тема письма.
    let (d, s) = inner_subs[&rows[1].0];
    let second = raw.pc(d, s);
    let (nid3, _) = object(&second[&0x3701]);
    let (d3, s3) = raw.subnodes(s)[&nid3];
    assert_eq!(text_of(&raw.pc(d3, s3)[&0x0037]), "Третий уровень");
    assert_eq!(text_of(&second[&0x3704]), "Третий уровень");
}

/// Карты размещения сходятся с тем, на что ссылаются деревья (scanpst перестраивает AMap по ним): каждая
/// страница карт, страница NBT/BBT и блок из BBT заняты в AMap ровно один раз, лишнего занятого нет; `cbAMapFree` —
/// свободное по картам; FMap — самый длинный свободный участок каждой области.
fn check_maps(bytes: &[u8]) {
    const RANGE: u64 = 496 * 8 * 64;
    let raw = Raw(bytes.to_vec());
    let (eof, last, free) = (raw.u64(184), raw.u64(192), raw.u64(200));
    assert_eq!(eof as usize, bytes.len());
    let regions = ((eof - 0x4400) / RANGE) as usize;
    assert_eq!(last, 0x4400 + (regions as u64 - 1) * RANGE);
    let fmap = |k: usize| k >= 128 && (k - 128).is_multiple_of(496);
    let fpmap = |k: usize| k >= 8192 && (k - 8192).is_multiple_of(31744);
    let mut expect = vec![[0u8; 496]; regions];
    let mut mark = |ib: u64, size: u64| {
        let k = ((ib - 0x4400) / RANGE) as usize;
        let off = ib - 0x4400 - k as u64 * RANGE;
        assert!(off + size <= RANGE, "{ib:X}: через границу области");
        for slot in off / 64..(off + size) / 64 {
            let (byte, bit) = ((slot / 8) as usize, 0x80u8 >> (slot % 8));
            assert_eq!(expect[k][byte] & bit, 0, "{ib:X}: занято дважды");
            expect[k][byte] |= bit;
        }
    };
    for k in 0..regions {
        let pages = 1 + u64::from(k % 8 == 0) + u64::from(fmap(k)) + 2 * u64::from(fpmap(k));
        mark(0x4400 + k as u64 * RANGE, pages * 512);
    }
    let mut stack = vec![(raw.u64(224), false), (raw.u64(240), true)];
    while let Some((page, bbt)) = stack.pop() {
        mark(page, 512);
        let p = page as usize;
        let (n, size, level) = (usize::from(bytes[p + 488]), usize::from(bytes[p + 490]), bytes[p + 491]);
        for i in 0..n {
            let e = p + i * size;
            if level > 0 {
                stack.push((raw.u64(e + 16), bbt));
            } else if bbt {
                mark(raw.u64(e + 8), (u64::from(raw.u16(e + 16)) + 16).next_multiple_of(64));
            }
        }
    }
    let mut zeros = 0;
    for (k, bits) in expect.iter().enumerate() {
        let at = 0x4400 + k * RANGE as usize;
        assert_eq!(bytes[at + 496], 0x84);
        assert!(bytes[at..at + 496] == bits[..], "AMap {k}: не сходится");
        zeros += bits.iter().map(|b| u64::from(b.count_zeros())).sum::<u64>();
        if fmap(k) {
            let f = at + 1024;
            assert_eq!(bytes[f + 496], 0x82);
            for j in 0..496 {
                let longest = expect.get(k + j).map_or(0, |m| {
                    let (mut best, mut run) = (0u32, 0u32);
                    for slot in 0..496 * 8 {
                        run = if m[slot / 8] & (0x80 >> (slot % 8)) == 0 { run + 1 } else { 0 };
                        best = best.max(run);
                    }
                    best.min(255) as u8
                });
                assert_eq!(bytes[f + j], longest, "FMap {k}, область {}", k + j);
            }
        }
    }
    assert_eq!(free, zeros * 64);
}

/// Предел тома мал: письма идут в `Тома.pst`, `Тома (2).pst`…; каждый том открывается, письма не теряются и не
/// повторяются, папки — в каждом томе, где есть их письма.
#[test]
fn volumes_split_by_limit() {
    const LIMIT: u64 = 2 * 1024 * 1024;
    let files = Memory::default();
    let mut w = Writer::new(files.clone(), "Тома", "Тома", LIMIT).unwrap();
    let mut n = 0u32;
    for f in ["Входящие", "Архив"] {
        w.folder(&path(&[f])).unwrap();
        for _ in 0..60 {
            let data: Vec<u8> = (0..30 * 1024u32).map(|i| (i.wrapping_mul(2_654_435_761).wrapping_add(n) >> 11) as u8).collect();
            let mut m = letter(data, true);
            m.props.retain(|(t, _)| *t != PR_SUBJECT_W);
            m.props.insert(0, text(PR_SUBJECT_W, &format!("Письмо {n}")));
            w.message(&m).unwrap();
            n += 1;
        }
    }
    let volumes = w.finish().unwrap();
    assert!(volumes.len() >= 3, "{volumes:?}");
    assert_eq!(volumes[0], "Тома.pst");
    assert_eq!(volumes[1], "Тома (2).pst");
    assert_eq!(files.names().iter().filter(|n| n.ends_with(".tmp")).count(), 0);
    let mut seen = Vec::new();
    for (k, name) in volumes.iter().enumerate() {
        let bytes = files.take(name).unwrap();
        assert!(bytes.len() as u64 <= LIMIT, "{name}: {} байт", bytes.len());
        check_maps(&bytes);
        let store = open(bytes);
        let display = store.properties().display_name().unwrap();
        assert_eq!(display, if k == 0 { "Тома".to_string() } else { format!("Тома ({})", k + 1) });
        let ipm = store.open_folder(&store.properties().ipm_sub_tree_entry_id().unwrap()).unwrap();
        for (id, folder) in names(ipm.hierarchy_table().unwrap().as_ref()) {
            let f = store.open_folder(&props_entry(&store, id)).unwrap();
            let subjects = column(f.contents_table().unwrap().as_ref(), PR_SUBJECT_W);
            assert_eq!(f.properties().content_count().unwrap() as usize, subjects.len());
            if folder != "Deleted Items" {
                assert!(!subjects.is_empty(), "{name}: пустая папка «{folder}»");
            }
            seen.extend(subjects.into_iter().map(|(_, s)| unicode(s.as_ref())));
        }
    }
    let expect: Vec<String> = (0..n).map(|i| format!("Письмо {i}")).collect();
    assert_eq!(seen, expect);
}

/// К папке, что уже была текущей, вернуться нельзя (таблица её содержимого записана).
#[test]
fn folder_order_is_checked() {
    let mut w = Writer::new(Memory::default(), "x", "x", pst::VOLUME_LIMIT).unwrap();
    w.folder(&path(&["А"])).unwrap();
    w.folder(&path(&["Б"])).unwrap();
    assert_eq!(w.folder(&path(&["А"])).unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
    // Родитель, созданный ради подпапки, ещё не был текущим — можно.
    w.folder(&path(&["В", "Г"])).unwrap();
    w.folder(&path(&["В"])).unwrap();
    w.abort();
}

/// Файл разбором узлов (для того, что `outlook-pst` не читает): BBT, блоки, поддеревья, контексты свойств.
struct Raw(Vec<u8>);

impl Raw {
    fn u16(&self, at: usize) -> u16 {
        u16::from_le_bytes(self.0[at..at + 2].try_into().unwrap())
    }

    fn u64(&self, at: usize) -> u64 {
        u64::from_le_bytes(self.0[at..at + 8].try_into().unwrap())
    }

    /// Байты блока по BID (поиск по BBT от корня в заголовке).
    fn block(&self, bid: u64) -> &[u8] {
        let mut page = self.u64(240) as usize;
        loop {
            let (n, size, level) = (usize::from(self.0[page + 488]), usize::from(self.0[page + 490]), self.0[page + 491]);
            let entries: Vec<usize> = (0..n).map(|i| page + i * size).collect();
            if level == 0 {
                let e = entries.into_iter().find(|&e| self.u64(e) == bid).unwrap();
                let (ib, cb) = (self.u64(e + 8) as usize, usize::from(self.u16(e + 16)));
                return &self.0[ib..ib + cb];
            }
            let e = entries.into_iter().take_while(|&e| self.u64(e) <= bid).last().unwrap();
            page = self.u64(e + 16) as usize;
        }
    }

    /// Блоки данных узла (лист или дерево XBLOCK) по порядку.
    fn blocks(&self, bid: u64) -> Vec<Vec<u8>> {
        let b = self.block(bid);
        if bid & 2 != 0 && b[0] == 1 {
            let n = usize::from(u16::from_le_bytes([b[2], b[3]]));
            (0..n).flat_map(|i| self.blocks(u64::from_le_bytes(b[8 + 8 * i..16 + 8 * i].try_into().unwrap()))).collect()
        } else {
            vec![b.to_vec()]
        }
    }

    /// Поддерево (SLBLOCK, SIBLOCK): NID → (данные, поддерево).
    fn subnodes(&self, bid: u64) -> BTreeMap<u32, (u64, u64)> {
        let b = self.block(bid).to_vec();
        let n = usize::from(u16::from_le_bytes([b[2], b[3]]));
        let le = |at: usize| u64::from_le_bytes(b[at..at + 8].try_into().unwrap());
        if b[1] == 0 {
            (0..n).map(|i| (le(8 + 24 * i) as u32, (le(16 + 24 * i), le(24 + 24 * i)))).collect()
        } else {
            (0..n).flat_map(|i| self.subnodes(le(16 + 16 * i))).collect()
        }
    }

    /// Контекст свойств: ID → (тип, байты значения; числа — 4 байта).
    fn pc(&self, data: u64, sub: u64) -> BTreeMap<u16, (u16, Vec<u8>)> {
        let blocks = self.blocks(data);
        let subs = if sub == 0 { BTreeMap::new() } else { self.subnodes(sub) };
        let alloc = |hid: u32| -> Vec<u8> {
            let page = &blocks[(hid >> 16) as usize];
            let map = usize::from(u16::from_le_bytes([page[0], page[1]]));
            let i = ((hid >> 5) & 0x7FF) as usize;
            let at = |k: usize| usize::from(u16::from_le_bytes([page[map + 4 + 2 * k], page[map + 5 + 2 * k]]));
            page[at(i - 1)..at(i)].to_vec()
        };
        let root = u32::from_le_bytes(blocks[0][4..8].try_into().unwrap());
        let header = alloc(root);
        assert_eq!(header[..3], [0xB5, 2, 6]);
        let mut level = vec![(header[3], u32::from_le_bytes(header[4..8].try_into().unwrap()))];
        let mut records = Vec::new();
        while let Some((depth, hid)) = level.pop() {
            let node = alloc(hid);
            if depth == 0 {
                records.extend(node.chunks(8).map(<[u8]>::to_vec));
            } else {
                level.extend(node.chunks(6).map(|r| (depth - 1, u32::from_le_bytes(r[2..6].try_into().unwrap()))));
            }
        }
        records
            .into_iter()
            .map(|r| {
                let (id, kind) = (u16::from_le_bytes([r[0], r[1]]), u16::from_le_bytes([r[2], r[3]]));
                let v = u32::from_le_bytes(r[4..8].try_into().unwrap());
                let bytes = match kind {
                    0x0003 | 0x000B => v.to_le_bytes().to_vec(),
                    _ if v == 0 => Vec::new(),
                    _ if v & 0x1F == 0 => alloc(v),
                    _ => self.blocks(subs[&v].0).concat(),
                };
                (id, (kind, bytes))
            })
            .collect()
    }
}

fn int_of(v: &(u16, Vec<u8>)) -> i32 {
    i32::from_le_bytes(v.1[..4].try_into().unwrap())
}

fn text_of(v: &(u16, Vec<u8>)) -> String {
    assert_eq!(v.0, 0x001F);
    String::from_utf16(&v.1.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>()).unwrap()
}

/// `PtypObject`: NID подузла и размер.
fn object(v: &(u16, Vec<u8>)) -> (u32, u32) {
    assert_eq!(v.0, 0x000D);
    (u32::from_le_bytes(v.1[0..4].try_into().unwrap()), u32::from_le_bytes(v.1[4..8].try_into().unwrap()))
}
