//! Messaging (MS-PST 2.4): хранилище, папки, письма, получатели, вложения поверх LTP и NDB. Набор узлов — обязательный
//! по MS-PST 2.7.1 (как в новом пустом файле Outlook): хранилище, таблица имён, корневая папка, IPM Subtree («Top of
//! Outlook data file»), «Search Root» (Finder), «Deleted Items», папка поиска спама, шаблоны таблиц; наши папки —
//! в IPM Subtree, письма — в своих папках. Столбцы таблиц — шаблоны нового файла Outlook (порядок задаёт раскладку).
//!
//! Сверх MS-PST — узлы, без которых scanpst чинит файл (S8-40; имена — по pstsdk Microsoft, содержимое — как в пустом
//! файле Outlook): таблица папок приёма, очередь исходящих, шаблоны таблиц истории изменений и «надгробий», HMP,
//! пустой узел 0xEC1. Строки таблиц — те же свойства, что у объектов (MS-PST 2.4.4.3): scanpst их сравнивает.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use super::ltp::{self, Row, Subs};
use super::ndb::{Ndb, Sub};
use crate::mapi::{
    ADDRTYPE_SMTP, FILETIME_UNIX_EPOCH, MAPI_TO, MSGFLAG_READ, MapiMessage, PR_ADDRTYPE_W, PR_ATTACH_LONG_FILENAME_W, PR_ATTACH_METHOD,
    PR_CLIENT_SUBMIT_TIME, PR_DISPLAY_CC_W, PR_DISPLAY_NAME_W, PR_DISPLAY_TO_W, PR_EMAIL_ADDRESS_W, PR_MESSAGE_CLASS_W, PR_MESSAGE_DELIVERY_TIME,
    PR_MESSAGE_FLAGS, PR_NORMALIZED_SUBJECT_W, PR_RECIPIENT_TYPE, PR_SENT_REPRESENTING_NAME_W, PR_SUBJECT_W, Tag, Value,
};

/// Папка в IPM Subtree и её письма.
#[derive(Debug, Clone, Copy)]
pub struct Folder<'a> {
    pub name: &'a str,
    pub messages: &'a [MapiMessage],
}

// Типы NID (MS-PST 2.2.2.1).
const NID_TYPE_NORMAL_FOLDER: u32 = 0x02;
const NID_TYPE_SEARCH_FOLDER: u32 = 0x03;
const NID_TYPE_NORMAL_MESSAGE: u32 = 0x04;
const NID_TYPE_ATTACHMENT: u32 = 0x05;
const NID_TYPE_SEARCH_UPDATE_QUEUE: u32 = 0x06;
const NID_TYPE_SEARCH_CRITERIA_OBJECT: u32 = 0x07;
const NID_TYPE_HIERARCHY_TABLE: u32 = 0x0D;
const NID_TYPE_CONTENTS_TABLE: u32 = 0x0E;
const NID_TYPE_ASSOC_CONTENTS_TABLE: u32 = 0x0F;
const NID_TYPE_SEARCH_CONTENTS_TABLE: u32 = 0x10;

const fn nid(kind: u32, index: u32) -> u32 {
    index << 5 | kind
}

// Особые NID (MS-PST 2.4.1).
const NID_MESSAGE_STORE: u32 = 0x21;
const NID_NAME_TO_ID_MAP: u32 = 0x61;
const NID_ROOT_FOLDER: u32 = 0x122;
const NID_SEARCH_MANAGEMENT_QUEUE: u32 = 0x1E1;
const NID_SEARCH_ACTIVITY_LIST: u32 = 0x201;
const NID_SEARCH_DOMAIN_OBJECT: u32 = 0x261;
const NID_HIERARCHY_TABLE_TEMPLATE: u32 = 0x60D;
const NID_CONTENTS_TABLE_TEMPLATE: u32 = 0x60E;
const NID_ASSOC_CONTENTS_TABLE_TEMPLATE: u32 = 0x60F;
const NID_SEARCH_CONTENTS_TABLE_TEMPLATE: u32 = 0x610;
/// Шаблон таблицы вложений; в поддереве письма — его таблица вложений.
const NID_ATTACHMENT_TABLE: u32 = 0x671;
/// Шаблон таблицы получателей; в поддереве письма — его таблица получателей.
const NID_RECIPIENT_TABLE: u32 = 0x692;
// Узлы нового файла Outlook вне MS-PST 2.7.1 (имена — pstsdk): папки приёма, очередь исходящих, шаблоны таблиц истории
// изменений, «надгробий» и их дат, HMP; 0xEC1 — пустой, scanpst считает его очередью обновлений поиска.
const NID_RECEIVE_FOLDER_TABLE: u32 = 0x62B;
const NID_OUTGOING_QUEUE: u32 = 0x64C;
const NID_CHANGE_HISTORY_TABLE_TEMPLATE: u32 = 0x6B6;
const NID_TOMBSTONE_TABLE_TEMPLATE: u32 = 0x6D7;
const NID_TOMBSTONE_DATE_TABLE_TEMPLATE: u32 = 0x6F8;
const NID_HST_HMP: u32 = 0xC01;
const NID_SEARCH_EC1: u32 = 0xEC1;

// Индексы папок нового файла Outlook (MS-PST 2.7.1): IPM Subtree, Finder, Deleted Items; папка поиска спама.
const IPM_SUBTREE: u32 = 0x401;
const FINDER: u32 = 0x402;
const WASTEBASKET: u32 = 0x403;
const SPAM_SEARCH: u32 = 0x111;
/// Индексы писем, вложений и подузлов LTP — после этих (последние занятые в новом файле, `rgnid`).
const FIRST_MESSAGE: u32 = 0x10000;
const FIRST_NID: u32 = 0x400;

// Свойства хранилища, папок, писем и вложений (MS-OXPROPS).
const PR_REPL_FLAGS: Tag = 0x0E38_0003;
const PR_RECORD_KEY: Tag = 0x0FF9_0102;
const PR_VALID_FOLDER_MASK: Tag = 0x35DF_0003;
const PR_IPM_SUBTREE_ENTRYID: Tag = 0x35E0_0102;
const PR_IPM_WASTEBASKET_ENTRYID: Tag = 0x35E3_0102;
const PR_FINDER_ENTRYID: Tag = 0x35E7_0102;
/// CRC пароля файла; 0 — без пароля.
const PR_PST_PASSWORD: Tag = 0x67FF_0003;
const PR_CONTENT_COUNT: Tag = 0x3602_0003;
const PR_CONTENT_UNREAD: Tag = 0x3603_0003;
const PR_SUBFOLDERS: Tag = 0x360A_000B;
const PR_CONTAINER_CLASS_W: Tag = 0x3613_001F;
const PR_MESSAGE_SIZE: Tag = 0x0E08_0003;
const PR_MESSAGE_STATUS: Tag = 0x0E17_0003;
const PR_CONVERSATION_TOPIC_W: Tag = 0x0070_001F;
const PR_CREATION_TIME: Tag = 0x3007_0040;
const PR_LAST_MODIFICATION_TIME: Tag = 0x3008_0040;
const PR_SEARCH_KEY: Tag = 0x300B_0102;
const PR_OBJECT_TYPE: Tag = 0x0FFE_0003;
const PR_DISPLAY_TYPE: Tag = 0x3900_0003;
const PR_ATTACH_SIZE: Tag = 0x0E20_0003;
const PR_ATTACH_FILENAME_W: Tag = 0x3704_001F;
const PR_RENDERING_POSITION: Tag = 0x370B_0003;
const PR_NAMEID_BUCKET_COUNT: Tag = 0x0001_0003;
const PR_NAMEID_STREAM_GUID: Tag = 0x0002_0102;
const PR_NAMEID_STREAM_ENTRY: Tag = 0x0003_0102;
const PR_NAMEID_STREAM_STRING: Tag = 0x0004_0102;
/// Свойство объекта условий папки поиска, как в новом файле Outlook.
const PR_SEARCH_CRITERIA_FLAGS: Tag = 0x660B_0003;
/// Таблица папок приёма: NID папки для класса сообщений.
const PR_RECEIVE_FOLDER_NID: Tag = 0x6605_0003;
/// Сумма `PR_ATTACH_SIZE` вложений письма (Outlook, вне MS-OXPROPS).
const PR_ATTACHMENTS_SIZE: Tag = 0x6604_0003;
const PR_SUBMIT_FLAGS: Tag = 0x0E14_0003;
const PR_SPOOLER_STATUS: Tag = 0x0E10_0003;
const PR_REPL_CHANGENUM: Tag = 0x0E33_0014;
const PR_RESPONSIBILITY: Tag = 0x0E0F_000B;
const PR_ENTRYID: Tag = 0x0FFF_0102;
const PR_SEND_RICH_INFO: Tag = 0x3A40_000B;
const PR_7BIT_DISPLAY_NAME_W: Tag = 0x39FF_001F;
/// Поставщик one-off EntryID (MS-OXCDATA 2.2.5.1).
const ONE_OFF_PROVIDER: [u8; 16] = [0x81, 0x2B, 0x1F, 0xA4, 0xBE, 0xA3, 0x10, 0x19, 0x9D, 0x6E, 0x00, 0xDD, 0x01, 0x0F, 0x54, 0x02];

/// `PR_VALID_FOLDER_MASK`: есть IPM Subtree, «Удалённые», Finder.
const FOLDER_MASK: i32 = 0x01 | 0x08 | 0x80;
const MSGFLAG_HASATTACH: i32 = 0x10;
const MAPI_MAILUSER: i32 = 6;

// Шаблоны таблиц нового файла Outlook: столбцы после LtpRowId и LtpRowVer, в порядке битов CEB.
const HIERARCHY: [Tag; 11] = [
    PR_DISPLAY_NAME_W,
    PR_CONTENT_COUNT,
    PR_CONTENT_UNREAD,
    PR_SUBFOLDERS,
    0x0E30_0102,
    0x0E33_0014,
    0x0E34_0102,
    PR_REPL_FLAGS,
    PR_CONTAINER_CLASS_W,
    0x6635_0003,
    0x6636_0003,
];
const CONTENTS: [Tag; 26] = [
    PR_MESSAGE_STATUS,
    PR_MESSAGE_CLASS_W,
    PR_MESSAGE_FLAGS,
    0x0017_0003,
    PR_SENT_REPRESENTING_NAME_W,
    PR_SUBJECT_W,
    PR_MESSAGE_DELIVERY_TIME,
    PR_CLIENT_SUBMIT_TIME,
    PR_MESSAGE_SIZE,
    PR_DISPLAY_TO_W,
    PR_DISPLAY_CC_W,
    0x0057_000B,
    0x0058_000B,
    0x0036_0003,
    0x1097_0003,
    PR_CONVERSATION_TOPIC_W,
    0x0071_0102,
    0x3013_0102,
    0x65C6_0003,
    PR_LAST_MODIFICATION_TIME,
    0x0E30_0102,
    0x0E33_0014,
    0x0E34_0102,
    0x0E3D_0102,
    0x0E3C_0102,
    PR_REPL_FLAGS,
];
const ASSOC_CONTENTS: [Tag; 13] = [
    PR_MESSAGE_STATUS,
    PR_MESSAGE_CLASS_W,
    PR_MESSAGE_FLAGS,
    PR_DISPLAY_NAME_W,
    0x7003_0003,
    0x7004_0102,
    0x7005_0102,
    0x7006_001F,
    0x7007_0003,
    0x6800_001F,
    0x6803_000B,
    0x6805_1003,
    0x682F_001F,
];
const SEARCH_CONTENTS: [Tag; 17] = [
    0x67F1_0003,
    0x0E05_001F,
    PR_MESSAGE_STATUS,
    PR_MESSAGE_CLASS_W,
    PR_MESSAGE_FLAGS,
    0x0017_0003,
    PR_SENT_REPRESENTING_NAME_W,
    PR_SUBJECT_W,
    PR_MESSAGE_DELIVERY_TIME,
    PR_MESSAGE_SIZE,
    PR_DISPLAY_TO_W,
    PR_DISPLAY_CC_W,
    0x0057_000B,
    0x0058_000B,
    0x0036_0003,
    0x0E2A_000B,
    PR_LAST_MODIFICATION_TIME,
];
/// Столбцы шаблона получателей; scanpst требует их и в таблице письма.
const RECIPIENTS: [Tag; 12] = [
    PR_RESPONSIBILITY,
    PR_ADDRTYPE_W,
    PR_EMAIL_ADDRESS_W,
    PR_ENTRYID,
    PR_DISPLAY_NAME_W,
    PR_RECIPIENT_TYPE,
    PR_SEARCH_KEY,
    PR_RECORD_KEY,
    PR_OBJECT_TYPE,
    PR_DISPLAY_TYPE,
    PR_SEND_RICH_INFO,
    PR_7BIT_DISPLAY_NAME_W,
];
const ATTACHMENTS: [Tag; 4] = [PR_RENDERING_POSITION, PR_ATTACH_SIZE, PR_ATTACH_METHOD, PR_ATTACH_FILENAME_W];
const RECEIVE_FOLDERS: [Tag; 2] = [PR_RECEIVE_FOLDER_NID, PR_MESSAGE_CLASS_W];
const OUTGOING_QUEUE: [Tag; 3] = [PR_SUBMIT_FLAGS, PR_SPOOLER_STATUS, PR_CLIENT_SUBMIT_TIME];
const CHANGE_HISTORY: [Tag; 3] = [PR_REPL_CHANGENUM, 0x0E37_0102, PR_REPL_FLAGS];
const TOMBSTONES: [Tag; 7] = [0x0E3E_0102, 0x0E31_0102, 0x0E30_0102, PR_REPL_FLAGS, 0x0E34_0102, PR_REPL_CHANGENUM, PR_MESSAGE_CLASS_W];
const TOMBSTONE_DATES: [Tag; 2] = [PR_CREATION_TIME, PR_REPL_CHANGENUM];

/// Новый файл PST (Unicode, без шифрования): хранилище `name`, папки в IPM Subtree с письмами. Вложенные письма — не
/// для пробного файла (S8-33б): ошибка `Unsupported`.
pub fn write(out: &mut impl Write, name: &str, folders: &[Folder]) -> io::Result<()> {
    let mut w = Writer { ndb: Ndb::new(), record_key: guid(), unique: 0, ltp: FIRST_NID, attach: FIRST_NID };
    let mut message = FIRST_MESSAGE;
    let mut ipm_children = vec![w.row(nid(NID_TYPE_NORMAL_FOLDER, WASTEBASKET), folder_props("Deleted Items", 0, 0, false, None))];
    let mut index = WASTEBASKET;
    for f in folders {
        index += 1;
        let folder = nid(NID_TYPE_NORMAL_FOLDER, index);
        let mut contents = Vec::with_capacity(f.messages.len());
        let mut unread = 0;
        for m in f.messages {
            message += 1;
            let id = nid(NID_TYPE_NORMAL_MESSAGE, message);
            let props = w.message(id, folder, m)?;
            if long(&props, PR_MESSAGE_FLAGS) & MSGFLAG_READ == 0 {
                unread += 1;
            }
            contents.push(w.row(id, props));
        }
        let props = folder_props(f.name, contents.len(), unread, false, Some("IPF.Note"));
        w.folder(index, nid(NID_TYPE_NORMAL_FOLDER, IPM_SUBTREE), &props, &[], &contents)?;
        ipm_children.push(w.row(folder, props));
    }

    let deleted = folder_props("Deleted Items", 0, 0, false, None);
    w.folder(WASTEBASKET, nid(NID_TYPE_NORMAL_FOLDER, IPM_SUBTREE), &deleted, &[], &[])?;
    let ipm = folder_props("Top of Outlook data file", 0, 0, true, None);
    w.folder(IPM_SUBTREE, NID_ROOT_FOLDER, &ipm, &ipm_children, &[])?;
    let finder = folder_props("Search Root", 0, 0, false, None);
    w.folder(FINDER, NID_ROOT_FOLDER, &finder, &[], &[])?;

    // Папка поиска спама: своя очередь обновлений (пустая), условия и таблица найденного.
    let spam_nid = nid(NID_TYPE_SEARCH_FOLDER, SPAM_SEARCH);
    let spam = folder_props("SPAM Search Folder 2", 0, 0, false, None);
    w.pc(spam_nid, NID_ROOT_FOLDER, &spam)?;
    w.ndb.node(nid(NID_TYPE_SEARCH_UPDATE_QUEUE, SPAM_SEARCH), 0, 0, 0);
    w.pc(nid(NID_TYPE_SEARCH_CRITERIA_OBJECT, SPAM_SEARCH), 0, &[(PR_SEARCH_CRITERIA_FLAGS, Value::Long(0))])?;
    w.tc(nid(NID_TYPE_SEARCH_CONTENTS_TABLE, SPAM_SEARCH), &SEARCH_CONTENTS, &[])?;

    let root_children =
        [w.row(nid(NID_TYPE_NORMAL_FOLDER, IPM_SUBTREE), ipm), w.row(nid(NID_TYPE_NORMAL_FOLDER, FINDER), finder), w.row(spam_nid, spam)];
    // Корень: имя пустое, родитель — он сам.
    let root = NID_ROOT_FOLDER >> 5;
    w.folder(root, NID_ROOT_FOLDER, &folder_props("", 0, 0, true, None), &root_children, &[])?;

    let entry = |index: u32| -> Value {
        let mut e = vec![0; 4];
        e.extend(w.record_key);
        e.extend(nid(NID_TYPE_NORMAL_FOLDER, index).to_le_bytes());
        Value::Binary(e)
    };
    let store = [
        (PR_REPL_FLAGS, Value::Long(0)),
        (PR_RECORD_KEY, Value::Binary(w.record_key.to_vec())),
        (PR_DISPLAY_NAME_W, Value::Unicode(name.into())),
        (PR_VALID_FOLDER_MASK, Value::Long(FOLDER_MASK)),
        (PR_IPM_SUBTREE_ENTRYID, entry(IPM_SUBTREE)),
        (PR_IPM_WASTEBASKET_ENTRYID, entry(WASTEBASKET)),
        (PR_FINDER_ENTRYID, entry(FINDER)),
        (PR_PST_PASSWORD, Value::Long(0)),
    ];
    w.pc(NID_MESSAGE_STORE, 0, &store)?;
    // Таблица именованных свойств пуста: 251 корзина, потоки без записей.
    let nameid = [
        (PR_NAMEID_BUCKET_COUNT, Value::Long(251)),
        (PR_NAMEID_STREAM_GUID, Value::Binary(Vec::new())),
        (PR_NAMEID_STREAM_ENTRY, Value::Binary(Vec::new())),
        (PR_NAMEID_STREAM_STRING, Value::Binary(Vec::new())),
    ];
    w.pc(NID_NAME_TO_ID_MAP, 0, &nameid)?;
    for n in [NID_SEARCH_MANAGEMENT_QUEUE, NID_SEARCH_DOMAIN_OBJECT, NID_SEARCH_EC1] {
        w.ndb.node(n, 0, 0, 0);
    }
    // Список активности поиска (MS-PST 2.4.8.4.2): NID папок, у которых есть папка поиска, — папка поиска спама.
    let sal = w.ndb.chunks(&[spam_nid.to_le_bytes().to_vec()])?;
    w.ndb.node(NID_SEARCH_ACTIVITY_LIST, sal, 0, 0);
    let hmp = ltp::hmp()?;
    let hmp = w.ndb.chunks(&hmp)?;
    w.ndb.node(NID_HST_HMP, hmp, 0, 0);
    w.tc(NID_HIERARCHY_TABLE_TEMPLATE, &HIERARCHY, &[])?;
    w.tc(NID_CONTENTS_TABLE_TEMPLATE, &CONTENTS, &[])?;
    w.tc(NID_ASSOC_CONTENTS_TABLE_TEMPLATE, &ASSOC_CONTENTS, &[])?;
    w.tc(NID_SEARCH_CONTENTS_TABLE_TEMPLATE, &SEARCH_CONTENTS, &[])?;
    w.tc(NID_ATTACHMENT_TABLE, &ATTACHMENTS, &[])?;
    w.tc(NID_RECIPIENT_TABLE, &RECIPIENTS, &[])?;
    // Папка приёма по умолчанию (класс "") — корневая, как в новом файле Outlook; очередь исходящих пуста.
    let receive = vec![(PR_RECEIVE_FOLDER_NID, Value::Long(NID_ROOT_FOLDER as i32)), (PR_MESSAGE_CLASS_W, Value::Unicode(String::new()))];
    let receive = w.row(1, receive);
    w.tc(NID_RECEIVE_FOLDER_TABLE, &RECEIVE_FOLDERS, &[receive])?;
    w.tc(NID_OUTGOING_QUEUE, &OUTGOING_QUEUE, &[])?;
    w.tc(NID_CHANGE_HISTORY_TABLE_TEMPLATE, &CHANGE_HISTORY, &[])?;
    w.tc(NID_TOMBSTONE_TABLE_TEMPLATE, &TOMBSTONES, &[])?;
    w.tc(NID_TOMBSTONE_DATE_TABLE_TEMPLATE, &TOMBSTONE_DATES, &[])?;

    // rgnid: последний занятый индекс по типам (по умолчанию — как в новом файле Outlook).
    let mut nids = [FIRST_NID; 32];
    nids[NID_TYPE_SEARCH_FOLDER as usize] = 0x4000;
    nids[NID_TYPE_NORMAL_MESSAGE as usize] = message;
    nids[0x08] = 0x8000;
    for t in [NID_TYPE_NORMAL_FOLDER, NID_TYPE_HIERARCHY_TABLE, NID_TYPE_CONTENTS_TABLE, NID_TYPE_ASSOC_CONTENTS_TABLE] {
        nids[t as usize] = index;
    }
    nids[NID_TYPE_ATTACHMENT as usize] = w.attach;
    nids[0x1F] = w.ltp;
    let unique = w.unique;
    w.ndb.finish(out, unique, &nids)
}

struct Writer {
    ndb: Ndb,
    record_key: [u8; 16],
    /// Счётчик версий строк таблиц (`LtpRowVer`) и `dwUnique` заголовка.
    unique: u32,
    ltp: u32,
    attach: u32,
}

impl Writer {
    fn row(&mut self, id: u32, values: Vec<(Tag, Value)>) -> Row {
        self.unique += 1;
        Row { id, ver: self.unique, values }
    }

    /// Узел NBT с контекстом свойств.
    fn pc(&mut self, nid: u32, parent: u32, props: &[(Tag, Value)]) -> io::Result<()> {
        let (data, sub, _) = pc_node(&mut self.ndb, &mut self.ltp, props)?;
        self.ndb.node(nid, data, sub, parent);
        Ok(())
    }

    /// Узел NBT с контекстом таблицы.
    fn tc(&mut self, nid: u32, tags: &[Tag], rows: &[Row]) -> io::Result<()> {
        let (data, sub, _) = tc_node(&mut self.ndb, &mut self.ltp, tags, rows)?;
        self.ndb.node(nid, data, sub, 0);
        Ok(())
    }

    /// Папка: свойства и три таблицы — иерархии (подпапки), содержимого (письма), связанного содержимого (пустая).
    fn folder(&mut self, index: u32, parent: u32, props: &[(Tag, Value)], children: &[Row], contents: &[Row]) -> io::Result<()> {
        self.pc(nid(NID_TYPE_NORMAL_FOLDER, index), parent, props)?;
        self.tc(nid(NID_TYPE_HIERARCHY_TABLE, index), &HIERARCHY, children)?;
        self.tc(nid(NID_TYPE_CONTENTS_TABLE, index), &CONTENTS, contents)?;
        self.tc(nid(NID_TYPE_ASSOC_CONTENTS_TABLE, index), &ASSOC_CONTENTS, &[])
    }

    /// Письмо: контекст свойств, в поддереве — таблица получателей, таблица вложений и вложения. Возвращает свойства
    /// письма для строки таблицы содержимого. Размеры — как у Outlook (по PSTFileFormat): `PR_ATTACH_SIZE` — сумма
    /// размеров свойств вложения, `PR_MESSAGE_SIZE` — байт данных узла письма со всем его поддеревом.
    fn message(&mut self, id: u32, folder: u32, m: &MapiMessage) -> io::Result<Vec<(Tag, Value)>> {
        if m.attachments.iter().any(|a| a.embedded.is_some()) {
            return Err(io::Error::new(io::ErrorKind::Unsupported, "вложенные письма в PST — S8-33б"));
        }
        let mut subs = Vec::new();
        // Байт данных в поддереве письма и сумма размеров вложений.
        let mut tree = 0;
        let mut attachments = 0;

        let mut rows = Vec::with_capacity(m.attachments.len());
        for a in &m.attachments {
            self.attach += 1;
            let anid = nid(NID_TYPE_ATTACHMENT, self.attach);
            let mut props = vec![(PR_ATTACH_SIZE, Value::Long(0)), (PR_RENDERING_POSITION, Value::Long(-1))];
            if let Some(name) = text(&a.props, PR_ATTACH_LONG_FILENAME_W) {
                props.push((PR_ATTACH_FILENAME_W, Value::Unicode(name.into())));
                props.push((PR_DISPLAY_NAME_W, Value::Unicode(name.into())));
            }
            props.extend(a.props.iter().cloned());
            let size = props_size(&props);
            props[0].1 = Value::Long(clamp(size));
            attachments += size;
            let (data, sub, len) = pc_node(&mut self.ndb, &mut self.ltp, &props)?;
            tree += len;
            subs.push(Sub { nid: anid, data, sub });
            rows.push(self.row(anid, props));
        }
        if !rows.is_empty() {
            let (data, sub, len) = tc_node(&mut self.ndb, &mut self.ltp, &ATTACHMENTS, &rows)?;
            tree += len;
            subs.push(Sub { nid: NID_ATTACHMENT_TABLE, data, sub });
        }

        // Получатели: столбцы шаблона (недостающие — `recipient`) и все остальные свойства получателей (SMTP-адрес и
        // др.). `LtpRowId` — с 1: ключ 0 в BTH scanpst считает перекрытием ключей.
        let mut tags = RECIPIENTS.to_vec();
        let mut recipients = Vec::with_capacity(m.recipients.len());
        for (i, r) in m.recipients.iter().enumerate() {
            let values = recipient(&r.props);
            for (t, v) in &values {
                if *t as u16 == v.prop_type() && !tags.contains(t) {
                    tags.push(*t);
                }
            }
            recipients.push(self.row(i as u32 + 1, values));
        }
        let (data, sub, len) = tc_node(&mut self.ndb, &mut self.ltp, &tags, &recipients)?;
        tree += len;
        subs.push(Sub { nid: NID_RECIPIENT_TABLE, data, sub });

        let mut flags = long(&m.props, PR_MESSAGE_FLAGS);
        if !m.attachments.is_empty() {
            flags |= MSGFLAG_HASATTACH;
        }
        let time = time(&m.props, PR_MESSAGE_DELIVERY_TIME).or_else(|| time(&m.props, PR_CLIENT_SUBMIT_TIME)).unwrap_or_else(now);
        let mut props = vec![
            (PR_MESSAGE_CLASS_W, Value::Unicode(m.class.clone())),
            (PR_MESSAGE_FLAGS, Value::Long(flags)),
            (PR_MESSAGE_SIZE, Value::Long(0)),
            (PR_MESSAGE_STATUS, Value::Long(0)),
            (PR_CREATION_TIME, Value::SysTime(time)),
            (PR_LAST_MODIFICATION_TIME, Value::SysTime(time)),
            (PR_SEARCH_KEY, Value::Binary(guid().to_vec())),
        ];
        if !m.attachments.is_empty() {
            props.push((PR_ATTACHMENTS_SIZE, Value::Long(clamp(attachments))));
        }
        if let Some(topic) = text(&m.props, PR_NORMALIZED_SUBJECT_W).or_else(|| text(&m.props, PR_SUBJECT_W)) {
            props.push((PR_CONVERSATION_TOPIC_W, Value::Unicode(topic.into())));
        }
        props.extend(m.props.iter().cloned());
        // Объём контекста свойств от значения `PR_MESSAGE_SIZE` не зависит (число — в самой записи BTH): черновик
        // в стороннюю NDB даёт размер, письмо пишется один раз.
        let mut scratch = 0;
        let mut draft = Subs::new(&mut scratch);
        let pages = ltp::pc(&mut Ndb::new(), &mut draft, &props)?;
        let own = pages.iter().map(|p| p.len() as u64).sum::<u64>() + draft.bytes;
        props[2].1 = Value::Long(clamp(own + tree));

        let mut own = Subs::new(&mut self.ltp);
        let blocks = ltp::pc(&mut self.ndb, &mut own, &props)?;
        subs.extend(own.list);
        let data = self.ndb.chunks(&blocks)?;
        let sub = self.ndb.subnodes(subs)?;
        self.ndb.node(id, data, sub, folder);
        Ok(props)
    }
}

/// Контекст свойств в узел или подузел → (данные, поддерево значений-подузлов, байт данных вместе с подузлами).
fn pc_node(ndb: &mut Ndb, ltp: &mut u32, props: &[(Tag, Value)]) -> io::Result<(u64, u64, u64)> {
    let mut subs = Subs::new(ltp);
    let blocks = ltp::pc(ndb, &mut subs, props)?;
    finish_node(ndb, &blocks, subs)
}

fn tc_node(ndb: &mut Ndb, ltp: &mut u32, tags: &[Tag], rows: &[Row]) -> io::Result<(u64, u64, u64)> {
    let mut subs = Subs::new(ltp);
    let blocks = ltp::tc(ndb, &mut subs, tags, rows)?;
    finish_node(ndb, &blocks, subs)
}

fn finish_node(ndb: &mut Ndb, blocks: &[Vec<u8>], subs: Subs) -> io::Result<(u64, u64, u64)> {
    let len = blocks.iter().map(|b| b.len() as u64).sum::<u64>() + subs.bytes;
    let data = ndb.chunks(blocks)?;
    let sub = if subs.list.is_empty() { 0 } else { ndb.subnodes(subs.list)? };
    Ok((data, sub, len))
}

/// Получатель: свойства письма и недостающие столбцы шаблона — тип адреса и адрес, one-off EntryID (он же ключ
/// записи), ключ поиска `ТИП:АДРЕС`, 7-битное имя, флаги.
fn recipient(props: &[(Tag, Value)]) -> Vec<(Tag, Value)> {
    let mut values = vec![(PR_OBJECT_TYPE, Value::Long(MAPI_MAILUSER)), (PR_DISPLAY_TYPE, Value::Long(0))];
    values.extend(props.iter().cloned());
    let email = text(props, PR_EMAIL_ADDRESS_W).unwrap_or_default().to_owned();
    let name = text(props, PR_DISPLAY_NAME_W).map_or_else(|| email.clone(), str::to_owned);
    let kind = text(props, PR_ADDRTYPE_W).unwrap_or(ADDRTYPE_SMTP).to_owned();
    let entry = one_off(&name, &kind, &email);
    let mut search = format!("{kind}:{email}").to_uppercase().into_bytes();
    search.push(0);
    let seven = if name.is_ascii() || email.is_empty() { name.clone() } else { email.clone() };
    let defaults = [
        (PR_DISPLAY_NAME_W, Value::Unicode(name)),
        (PR_ADDRTYPE_W, Value::Unicode(kind)),
        (PR_EMAIL_ADDRESS_W, Value::Unicode(email)),
        (PR_RECIPIENT_TYPE, Value::Long(MAPI_TO)),
        (PR_RESPONSIBILITY, Value::Boolean(false)),
        (PR_SEND_RICH_INFO, Value::Boolean(false)),
        (PR_ENTRYID, Value::Binary(entry.clone())),
        (PR_RECORD_KEY, Value::Binary(entry)),
        (PR_SEARCH_KEY, Value::Binary(search)),
        (PR_7BIT_DISPLAY_NAME_W, Value::Unicode(seven)),
    ];
    for (t, v) in defaults {
        if !values.iter().any(|(have, hv)| *have == t && hv.prop_type() == t as u16) {
            values.push((t, v));
        }
    }
    values
}

/// One-off EntryID (MS-OXCDATA 2.2.5.1): Unicode, без поиска в адресной книге; имя, тип и адрес — строки с нулём.
fn one_off(name: &str, kind: &str, email: &str) -> Vec<u8> {
    let mut e = vec![0; 4];
    e.extend(ONE_OFF_PROVIDER);
    e.extend([0, 0, 0x01, 0x90]);
    for s in [name, kind, email] {
        e.extend(s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes));
    }
    e
}

/// Сумма размеров свойств — `PR_ATTACH_SIZE` по Outlook: числа по своей ширине, строки (UTF-16) и байты — по длине
/// значения. Повторы (в контексте свойств остаётся первое) и свойства с чужим типом не считаются.
fn props_size(props: &[(Tag, Value)]) -> u64 {
    let mut seen = Vec::with_capacity(props.len());
    let mut size = 0;
    for (t, v) in props {
        if *t as u16 != v.prop_type() || seen.contains(&(t >> 16)) {
            continue;
        }
        seen.push(t >> 16);
        size += match v {
            Value::Long(_) => 4,
            Value::Boolean(_) => 1,
            Value::SysTime(_) => 8,
            Value::Unicode(s) => 2 * s.encode_utf16().count() as u64,
            Value::Binary(b) => b.len() as u64,
        };
    }
    size
}

fn folder_props(name: &str, count: usize, unread: i32, subfolders: bool, class: Option<&str>) -> Vec<(Tag, Value)> {
    let mut props = vec![
        (PR_DISPLAY_NAME_W, Value::Unicode(name.into())),
        (PR_CONTENT_COUNT, Value::Long(clamp(count as u64))),
        (PR_CONTENT_UNREAD, Value::Long(unread)),
        (PR_SUBFOLDERS, Value::Boolean(subfolders)),
    ];
    if let Some(class) = class {
        props.push((PR_CONTAINER_CLASS_W, Value::Unicode(class.into())));
    }
    props
}

fn text(props: &[(Tag, Value)], tag: Tag) -> Option<&str> {
    props.iter().find_map(|(t, v)| match v {
        Value::Unicode(s) if *t == tag && !s.is_empty() => Some(s.as_str()),
        _ => None,
    })
}

fn long(props: &[(Tag, Value)], tag: Tag) -> i32 {
    props.iter().find_map(|(t, v)| if let (true, Value::Long(n)) = (*t == tag, v) { Some(*n) } else { None }).unwrap_or(0)
}

fn time(props: &[(Tag, Value)], tag: Tag) -> Option<u64> {
    props.iter().find_map(|(t, v)| if let (true, Value::SysTime(n)) = (*t == tag, v) { Some(*n) } else { None })
}

fn clamp(n: u64) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

/// Сейчас в FILETIME — время письма без даты.
fn now() -> u64 {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    FILETIME_UNIX_EPOCH + d.as_secs() * 10_000_000 + u64::from(d.subsec_nanos() / 100)
}

/// 16 случайных байт: ключ записи хранилища, ключ поиска письма.
fn guid() -> [u8; 16] {
    let state = RandomState::new();
    let mut out = [0; 16];
    for (i, part) in out.chunks_mut(8).enumerate() {
        let mut h = state.build_hasher();
        h.write_usize(i);
        h.write_u64(now());
        part.copy_from_slice(&h.finish().to_le_bytes());
    }
    out
}
