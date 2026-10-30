//! Messaging (MS-PST 2.4): хранилище, папки, письма, получатели, вложения поверх LTP и NDB. Набор узлов — обязательный
//! по MS-PST 2.7.1 (как в новом пустом файле Outlook): хранилище, таблица имён, корневая папка, IPM Subtree («Top of
//! Outlook data file»), «Search Root» (Finder), «Deleted Items», папка поиска спама, шаблоны таблиц; наши папки —
//! в IPM Subtree, письма — в своих папках. Столбцы таблиц — шаблоны нового файла Outlook (порядок задаёт раскладку).

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use super::ltp::{self, Row, Subs};
use super::ndb::{Ndb, Sub};
use crate::mapi::{
    FILETIME_UNIX_EPOCH, MSGFLAG_READ, MapiMessage, PR_ATTACH_DATA_BIN, PR_ATTACH_LONG_FILENAME_W, PR_ATTACH_METHOD, PR_CLIENT_SUBMIT_TIME,
    PR_DISPLAY_CC_W, PR_DISPLAY_NAME_W, PR_DISPLAY_TO_W, PR_MESSAGE_CLASS_W, PR_MESSAGE_DELIVERY_TIME, PR_MESSAGE_FLAGS, PR_NORMALIZED_SUBJECT_W,
    PR_RECIPIENT_TYPE, PR_SENT_REPRESENTING_NAME_W, PR_SUBJECT_W, Tag, Value,
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
const RECIPIENTS: [Tag; 12] = [
    0x0E0F_000B,
    0x3002_001F,
    0x3003_001F,
    0x0FFF_0102,
    PR_DISPLAY_NAME_W,
    PR_RECIPIENT_TYPE,
    PR_SEARCH_KEY,
    PR_RECORD_KEY,
    PR_OBJECT_TYPE,
    PR_DISPLAY_TYPE,
    0x3A40_000B,
    0x39FF_001F,
];
const ATTACHMENTS: [Tag; 4] = [PR_RENDERING_POSITION, PR_ATTACH_SIZE, PR_ATTACH_METHOD, PR_ATTACH_FILENAME_W];

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
    for n in [NID_SEARCH_MANAGEMENT_QUEUE, NID_SEARCH_ACTIVITY_LIST, NID_SEARCH_DOMAIN_OBJECT] {
        w.ndb.node(n, 0, 0, 0);
    }
    w.tc(NID_HIERARCHY_TABLE_TEMPLATE, &HIERARCHY, &[])?;
    w.tc(NID_CONTENTS_TABLE_TEMPLATE, &CONTENTS, &[])?;
    w.tc(NID_ASSOC_CONTENTS_TABLE_TEMPLATE, &ASSOC_CONTENTS, &[])?;
    w.tc(NID_SEARCH_CONTENTS_TABLE_TEMPLATE, &SEARCH_CONTENTS, &[])?;
    w.tc(NID_ATTACHMENT_TABLE, &ATTACHMENTS, &[])?;
    w.tc(NID_RECIPIENT_TABLE, &RECIPIENTS, &[])?;

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
        let (data, sub) = pc_node(&mut self.ndb, &mut self.ltp, props)?;
        self.ndb.node(nid, data, sub, parent);
        Ok(())
    }

    /// Узел NBT с контекстом таблицы.
    fn tc(&mut self, nid: u32, tags: &[Tag], rows: &[Row]) -> io::Result<()> {
        let (data, sub) = tc_node(&mut self.ndb, &mut self.ltp, tags, rows)?;
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
    /// письма для строки таблицы содержимого.
    fn message(&mut self, id: u32, folder: u32, m: &MapiMessage) -> io::Result<Vec<(Tag, Value)>> {
        if m.attachments.iter().any(|a| a.embedded.is_some()) {
            return Err(io::Error::new(io::ErrorKind::Unsupported, "вложенные письма в PST — S8-33б"));
        }
        let mut subs = Vec::new();
        let mut size: u64 = m.props.iter().map(|(_, v)| value_size(v)).sum();

        let mut rows = Vec::with_capacity(m.attachments.len());
        for a in &m.attachments {
            self.attach += 1;
            let anid = nid(NID_TYPE_ATTACHMENT, self.attach);
            let data = a.props.iter().find_map(|(t, v)| match v {
                Value::Binary(b) if *t == PR_ATTACH_DATA_BIN => Some(b.len()),
                _ => None,
            });
            let mut props = vec![(PR_ATTACH_SIZE, Value::Long(clamp(data.unwrap_or(0) as u64))), (PR_RENDERING_POSITION, Value::Long(-1))];
            if let Some(name) = text(&a.props, PR_ATTACH_LONG_FILENAME_W) {
                props.push((PR_ATTACH_FILENAME_W, Value::Unicode(name.into())));
                props.push((PR_DISPLAY_NAME_W, Value::Unicode(name.into())));
            }
            props.extend(a.props.iter().cloned());
            size += a.props.iter().map(|(_, v)| value_size(v)).sum::<u64>();
            let (data, sub) = pc_node(&mut self.ndb, &mut self.ltp, &props)?;
            subs.push(Sub { nid: anid, data, sub });
            rows.push(self.row(anid, props));
        }
        if !rows.is_empty() {
            let (data, sub) = tc_node(&mut self.ndb, &mut self.ltp, &ATTACHMENTS, &rows)?;
            subs.push(Sub { nid: NID_ATTACHMENT_TABLE, data, sub });
        }

        // Получатели: столбцы шаблона и все остальные свойства получателей (SMTP-адрес и др.).
        let mut tags = RECIPIENTS.to_vec();
        let mut recipients = Vec::with_capacity(m.recipients.len());
        for (i, r) in m.recipients.iter().enumerate() {
            let mut values = vec![(PR_OBJECT_TYPE, Value::Long(MAPI_MAILUSER)), (PR_DISPLAY_TYPE, Value::Long(0))];
            values.extend(r.props.iter().cloned());
            for (t, v) in &r.props {
                if *t as u16 == v.prop_type() && !tags.contains(t) {
                    tags.push(*t);
                }
            }
            recipients.push(self.row(i as u32, values));
        }
        let (data, sub) = tc_node(&mut self.ndb, &mut self.ltp, &tags, &recipients)?;
        subs.push(Sub { nid: NID_RECIPIENT_TABLE, data, sub });

        let mut flags = long(&m.props, PR_MESSAGE_FLAGS);
        if !m.attachments.is_empty() {
            flags |= MSGFLAG_HASATTACH;
        }
        let time = time(&m.props, PR_MESSAGE_DELIVERY_TIME).or_else(|| time(&m.props, PR_CLIENT_SUBMIT_TIME)).unwrap_or_else(now);
        let mut props = vec![
            (PR_MESSAGE_CLASS_W, Value::Unicode(m.class.clone())),
            (PR_MESSAGE_FLAGS, Value::Long(flags)),
            (PR_MESSAGE_SIZE, Value::Long(clamp(size))),
            (PR_MESSAGE_STATUS, Value::Long(0)),
            (PR_CREATION_TIME, Value::SysTime(time)),
            (PR_LAST_MODIFICATION_TIME, Value::SysTime(time)),
            (PR_SEARCH_KEY, Value::Binary(guid().to_vec())),
        ];
        if let Some(topic) = text(&m.props, PR_NORMALIZED_SUBJECT_W).or_else(|| text(&m.props, PR_SUBJECT_W)) {
            props.push((PR_CONVERSATION_TOPIC_W, Value::Unicode(topic.into())));
        }
        props.extend(m.props.iter().cloned());

        let mut own = Subs { list: Vec::new(), next: &mut self.ltp };
        let blocks = ltp::pc(&mut self.ndb, &mut own, &props)?;
        subs.extend(own.list);
        let data = self.ndb.chunks(&blocks)?;
        let sub = self.ndb.subnodes(subs)?;
        self.ndb.node(id, data, sub, folder);
        Ok(props)
    }
}

/// Контекст свойств в узел или подузел → (данные, поддерево значений-подузлов).
fn pc_node(ndb: &mut Ndb, ltp: &mut u32, props: &[(Tag, Value)]) -> io::Result<(u64, u64)> {
    let mut subs = Subs { list: Vec::new(), next: ltp };
    let blocks = ltp::pc(ndb, &mut subs, props)?;
    finish_node(ndb, &blocks, subs.list)
}

fn tc_node(ndb: &mut Ndb, ltp: &mut u32, tags: &[Tag], rows: &[Row]) -> io::Result<(u64, u64)> {
    let mut subs = Subs { list: Vec::new(), next: ltp };
    let blocks = ltp::tc(ndb, &mut subs, tags, rows)?;
    finish_node(ndb, &blocks, subs.list)
}

fn finish_node(ndb: &mut Ndb, blocks: &[Vec<u8>], subs: Vec<Sub>) -> io::Result<(u64, u64)> {
    let data = ndb.chunks(blocks)?;
    let sub = if subs.is_empty() { 0 } else { ndb.subnodes(subs)? };
    Ok((data, sub))
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

fn value_size(v: &Value) -> u64 {
    match v {
        Value::Unicode(s) => 2 * s.len() as u64,
        Value::Binary(b) => b.len() as u64,
        _ => 8,
    }
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
