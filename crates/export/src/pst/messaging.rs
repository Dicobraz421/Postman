//! Messaging (MS-PST 2.4): хранилище, папки, письма, получатели, вложения поверх LTP и NDB. Набор узлов — обязательный
//! по MS-PST 2.7.1 (как в новом пустом файле Outlook): хранилище, таблица имён, корневая папка, IPM Subtree («Top of
//! Outlook data file»), «Search Root» (Finder), «Deleted Items», папка поиска спама, шаблоны таблиц; наши папки —
//! деревом в IPM Subtree, письма — в своих папках. Столбцы таблиц — шаблоны нового файла Outlook (порядок задаёт
//! раскладку).
//!
//! Сверх MS-PST — узлы, без которых scanpst чинит файл (S8-40; имена — по pstsdk Microsoft, содержимое — как в пустом
//! файле Outlook): таблица папок приёма, очередь исходящих, шаблоны таблиц истории изменений и «надгробий», HMP,
//! пустой узел 0xEC1; у хранилища — свойства версии, как у Outlook. Строки таблиц — те же свойства, что у объектов
//! (MS-PST 2.4.4.3): scanpst их сравнивает.
//!
//! Письма — потоком ([`Writer`]): узел письма пишется сразу, строка таблицы содержимого — в таблицу текущей папки
//! (тоже потоком); папки, их иерархия и служебные узлы — в конце тома. Том больше предела — следующий,
//! `Имя (2).pst`: в нём то же дерево папок (только те, где есть письма тома).

use std::collections::HashMap;
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use super::ltp::{self, Object, Row, Subs, TcStream};
use super::ndb::{Ndb, Sub};
use super::{Files, Memory};
use crate::mapi::{
    ADDRTYPE_SMTP, ATTACH_EMBEDDED_MSG, FILETIME_UNIX_EPOCH, MAPI_TO, MSGFLAG_READ, MapiMessage, PR_ADDRTYPE_W, PR_ATTACH_DATA_BIN,
    PR_ATTACH_LONG_FILENAME_W, PR_ATTACH_METHOD, PR_CLIENT_SUBMIT_TIME, PR_DISPLAY_CC_W, PR_DISPLAY_NAME_W, PR_DISPLAY_TO_W, PR_EMAIL_ADDRESS_W,
    PR_MESSAGE_CLASS_W, PR_MESSAGE_DELIVERY_TIME, PR_MESSAGE_FLAGS, PR_NORMALIZED_SUBJECT_W, PR_RECIPIENT_TYPE, PR_SENT_REPRESENTING_NAME_W,
    PR_SUBJECT_W, Tag, Value,
};

/// Предел тома: 50 ГиБ — `MaxLargeFileSize` Outlook по умолчанию; больше — следующий том.
pub const VOLUME_LIMIT: u64 = 50 * 1024 * 1024 * 1024;

/// Имя файла тома `n` (с 1): `Имя.pst`, `Имя (2).pst`…
pub fn volume_name(base: &str, n: u32) -> String {
    if n <= 1 { format!("{base}.pst") } else { format!("{base} ({n}).pst") }
}

/// Папка в IPM Subtree и её письма (для [`write`]).
#[derive(Debug, Clone, Copy)]
pub struct Folder<'a> {
    pub name: &'a str,
    pub messages: &'a [MapiMessage],
}

/// Новый файл PST целиком в `out` (Unicode, без шифрования): хранилище `name`, папки в IPM Subtree с письмами. Для
/// малых файлов и проверок; выгрузка — [`Writer`] потоком.
pub fn write(out: &mut impl Write, name: &str, folders: &[Folder]) -> io::Result<()> {
    let files = Memory::default();
    let mut w = Writer::new(files.clone(), name, name, VOLUME_LIMIT)?;
    for f in folders {
        w.folder(&[f.name.to_owned()])?;
        for m in f.messages {
            w.message(m)?;
        }
    }
    w.finish()?;
    let bytes = files.take(&volume_name(name, 1)).ok_or_else(|| io::Error::other("PST: нет тома"))?;
    out.write_all(&bytes)
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
/// Индексы писем, вложений и подузлов LTP — после этих (последние занятые в новом файле, `rgnid`). NID писем — с
/// `FIRST_MESSAGE` и выше любого NID папки: в NBT они идут потоком, по возрастанию.
const FIRST_MESSAGE: u32 = 0x10000;
const FIRST_NID: u32 = 0x400;
/// Первый индекс наших папок.
const FIRST_FOLDER: u32 = 0x404;

// Свойства хранилища, папок, писем и вложений (MS-OXPROPS).
const PR_REPL_FLAGS: Tag = 0x0E38_0003;
const PR_RECORD_KEY: Tag = 0x0FF9_0102;
const PR_VALID_FOLDER_MASK: Tag = 0x35DF_0003;
const PR_IPM_SUBTREE_ENTRYID: Tag = 0x35E0_0102;
const PR_IPM_WASTEBASKET_ENTRYID: Tag = 0x35E3_0102;
const PR_FINDER_ENTRYID: Tag = 0x35E7_0102;
/// CRC пароля файла; 0 — без пароля.
const PR_PST_PASSWORD: Tag = 0x67FF_0003;
/// Свойства хранилища нового файла Outlook (по `Empty.pst` из `outlook-pst`, вне MS-OXPROPS): `0x6633` — да,
/// `0x66FA` и `0x66FC` — версия и сборка, которой файл создан (без них scanpst пишет «Store created by build: (not
/// available)»).
const PR_PST_LR_NO_RESTRICTIONS: Tag = 0x6633_000B;
const PR_PST_ENSURE: Tag = 0x66FA_0003;
const PR_PST_BUILD: Tag = 0x66FC_0003;
const PST_ENSURE: i32 = 0x000E_0011;
const PST_BUILD: i32 = 1_618_611;
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
/// Вложенное письмо: `PtypObject` — подузел вложения с письмом (MS-PST 2.3.3.5).
const PR_ATTACH_DATA_OBJ: Tag = 0x3701_000D;
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
/// Имя «Удалённых» нового файла Outlook: папка учётки с таким именем в корне — она и есть.
const DELETED_ITEMS: &str = "Deleted Items";

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

/// Писатель PST потоком: [`Writer::folder`] — папка (путь от корня, родители создаются сами), затем её письма
/// [`Writer::message`]; [`Writer::finish`] — последний том готов. Письма папки идут подряд: к папке, которая уже
/// была текущей, вернуться нельзя. Память не растёт с числом писем: узлы писем и записи деревьев — в рабочих файлах
/// рядом с томом (`Имя.pst.nbt.tmp`, `Имя.pst.bbt.tmp`, удаляются в конце).
pub struct Writer<F: Files> {
    files: F,
    /// Имя файла без `.pst` и имя хранилища в Outlook.
    base: String,
    store: String,
    limit: u64,
    volume: u32,
    vol: Option<Volume>,
    done: Vec<String>,
    /// Путь текущей папки — для нового тома.
    path: Option<Vec<String>>,
}

impl<F: Files> Writer<F> {
    /// Первый том `base.pst` (хранилище `store`); `limit` — предел тома в байтах ([`VOLUME_LIMIT`]).
    pub fn new(mut files: F, base: &str, store: &str, limit: u64) -> io::Result<Self> {
        let vol = Volume::new(&mut files, &volume_name(base, 1), store)?;
        Ok(Self { files, base: base.into(), store: store.into(), limit, volume: 1, vol: Some(vol), done: Vec::new(), path: None })
    }

    /// Томов начато.
    pub fn volumes(&self) -> u32 {
        self.volume
    }

    /// Текущая папка — `path` (имена от корня учётки; пустой путь — папка с именем хранилища).
    pub fn folder(&mut self, path: &[String]) -> io::Result<()> {
        let path = if path.is_empty() { vec![self.store.clone()] } else { path.to_vec() };
        self.vol()?.open(&path)?;
        self.path = Some(path);
        Ok(())
    }

    /// Письмо в текущую папку; том полон — следующий том.
    pub fn message(&mut self, m: &MapiMessage) -> io::Result<()> {
        let limit = self.limit;
        let full = {
            let v = self.vol()?;
            v.messages > 0 && v.ndb.size().saturating_add(estimate(m)).saturating_add(v.reserve()) > limit
        };
        if full {
            self.next_volume()?;
        }
        self.vol()?.message(m)
    }

    /// Последний том готов → имена файлов томов.
    pub fn finish(mut self) -> io::Result<Vec<String>> {
        if let Some(v) = self.vol.take() {
            self.done.push(v.file.clone());
            v.finish(&mut self.files)?;
        }
        Ok(self.done)
    }

    /// Прервать: недописанный том и рабочие файлы удаляются, готовые тома остаются.
    pub fn abort(mut self) {
        if let Some(v) = self.vol.take() {
            let file = v.file.clone();
            drop(v);
            for name in [file.clone(), spill(&file, "nbt"), spill(&file, "bbt")] {
                let _ = self.files.remove(&name);
            }
        }
    }

    fn vol(&mut self) -> io::Result<&mut Volume> {
        self.vol.as_mut().ok_or_else(|| io::Error::other("PST: том закрыт"))
    }

    fn next_volume(&mut self) -> io::Result<()> {
        if let Some(v) = self.vol.take() {
            self.done.push(v.file.clone());
            v.finish(&mut self.files)?;
        }
        self.volume += 1;
        let file = volume_name(&self.base, self.volume);
        let store = format!("{} ({})", self.store, self.volume);
        self.vol = Some(Volume::new(&mut self.files, &file, &store)?);
        if let Some(path) = self.path.clone() {
            self.vol()?.open(&path)?;
        }
        Ok(())
    }
}

fn spill(file: &str, kind: &str) -> String {
    format!("{file}.{kind}.tmp")
}

/// Папка тома: номер, родитель (`None` — IPM Subtree), имя, счётчики, подпапки, таблица содержимого.
struct FolderState {
    index: u32,
    parent: Option<usize>,
    name: String,
    class: Option<&'static str>,
    count: u32,
    unread: i32,
    children: Vec<usize>,
    /// Таблица содержимого записана (данные, поддерево).
    contents: Option<(u64, u64)>,
    /// Была текущей.
    used: bool,
}

struct Volume {
    ndb: Ndb,
    file: String,
    store: String,
    record_key: [u8; 16],
    /// Счётчик версий строк таблиц (`LtpRowVer`) и `dwUnique` заголовка.
    unique: u32,
    ltp: u32,
    attach: u32,
    message: u32,
    next_folder: u32,
    folders: Vec<FolderState>,
    paths: HashMap<Vec<String>, usize>,
    /// Текущая папка и её таблица содержимого.
    open: Option<(usize, TcStream)>,
    messages: u64,
}

impl Volume {
    fn new(files: &mut impl Files, file: &str, store: &str) -> io::Result<Self> {
        let out = files.create(file)?;
        let bbt = files.create(&spill(file, "bbt"))?;
        let nbt = files.create(&spill(file, "nbt"))?;
        let ndb = Ndb::new(out, bbt, nbt, FIRST_MESSAGE << 5)?;
        let deleted = FolderState {
            index: WASTEBASKET,
            parent: None,
            name: DELETED_ITEMS.into(),
            class: None,
            count: 0,
            unread: 0,
            children: Vec::new(),
            contents: None,
            used: false,
        };
        Ok(Self {
            ndb,
            file: file.into(),
            store: store.into(),
            record_key: guid(),
            unique: 0,
            ltp: FIRST_NID,
            attach: FIRST_NID,
            message: FIRST_MESSAGE,
            next_folder: FIRST_FOLDER,
            folders: vec![deleted],
            paths: HashMap::new(),
            open: None,
            messages: 0,
        })
    }

    /// Запас на конец тома: деревья NBT и BBT, папки и их таблицы, последняя область AMap.
    fn reserve(&self) -> u64 {
        self.ndb.blocks * 28 + self.ndb.node_count() * 40 + self.folders.len() as u64 * 16 * 1024 + 512 * 1024
    }

    fn row(&mut self, id: u32, props: &[(Tag, &Value)], tags: &[Tag]) -> Row {
        self.unique += 1;
        let values = props.iter().filter(|(t, v)| tags.contains(t) && *t as u16 == v.prop_type()).map(|(t, v)| (*t, (*v).clone())).collect();
        Row { id, ver: self.unique, values }
    }

    /// Папка по пути (родители — по пути, создаются).
    fn ensure(&mut self, path: &[String]) -> io::Result<usize> {
        let mut parent = None;
        for i in 1..=path.len() {
            let key = path[..i].to_vec();
            if let Some(&f) = self.paths.get(&key) {
                parent = Some(f);
                continue;
            }
            let f = if i == 1 && path[0].eq_ignore_ascii_case(DELETED_ITEMS) {
                0
            } else {
                if self.next_folder >= FIRST_MESSAGE {
                    return Err(io::Error::other("PST: слишком много папок"));
                }
                let f = self.folders.len();
                self.folders.push(FolderState {
                    index: self.next_folder,
                    parent,
                    name: path[i - 1].clone(),
                    class: Some("IPF.Note"),
                    count: 0,
                    unread: 0,
                    children: Vec::new(),
                    contents: None,
                    used: false,
                });
                self.next_folder += 1;
                if let Some(p) = parent {
                    self.folders[p].children.push(f);
                }
                f
            };
            self.paths.insert(key, f);
            parent = Some(f);
        }
        parent.ok_or_else(|| io::Error::other("PST: пустой путь папки"))
    }

    /// Сделать папку текущей: прежняя таблица содержимого — в файл.
    fn open(&mut self, path: &[String]) -> io::Result<()> {
        self.seal()?;
        let f = self.ensure(path)?;
        if self.folders[f].used {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "PST: письма папки — подряд, к записанной папке не вернуться"));
        }
        self.folders[f].used = true;
        let tc = TcStream::new(&mut self.ndb, &CONTENTS)?;
        self.open = Some((f, tc));
        Ok(())
    }

    fn seal(&mut self) -> io::Result<()> {
        if let Some((f, tc)) = self.open.take() {
            let node = tc.finish(&mut self.ndb, &mut self.ltp)?;
            self.folders[f].contents = Some(node);
        }
        Ok(())
    }

    fn message(&mut self, m: &MapiMessage) -> io::Result<()> {
        let Some((f, _)) = &self.open else { return Err(io::Error::other("PST: нет текущей папки")) };
        let f = *f;
        let folder = nid(NID_TYPE_NORMAL_FOLDER, self.folders[f].index);
        self.message += 1;
        let id = nid(NID_TYPE_NORMAL_MESSAGE, self.message);
        let built = self.object(m, &CONTENTS)?;
        self.ndb.node(id, built.data, built.sub, folder)?;
        if built.flags & MSGFLAG_READ == 0 {
            self.folders[f].unread += 1;
        }
        self.folders[f].count += 1;
        self.unique += 1;
        let ver = self.unique;
        let row: Vec<(Tag, &Value)> = built.row.iter().map(|(t, v)| (*t, v)).collect();
        if let Some((_, tc)) = self.open.as_mut() {
            tc.add(&mut self.ndb, &mut self.ltp, id, ver, &row)?;
        }
        self.messages += 1;
        Ok(())
    }

    /// Письмо (верхнее или вложенное): контекст свойств, в поддереве — таблица получателей, таблица вложений и
    /// вложения (с вложенными письмами). Размеры — как у Outlook (по PSTFileFormat): `PR_ATTACH_SIZE` — сумма размеров
    /// свойств вложения, `PR_MESSAGE_SIZE` — байт данных узла письма со всем его поддеревом. `row_tags` — столбцы строки
    /// таблицы содержимого (значения — те же, что у письма).
    fn object(&mut self, m: &MapiMessage, row_tags: &[Tag]) -> io::Result<Built> {
        let mut subs = Vec::new();
        // Байт данных в поддереве письма и сумма размеров вложений.
        let mut tree = 0;
        let mut attachments = 0;

        let mut rows = Vec::with_capacity(m.attachments.len());
        for a in &m.attachments {
            self.attach += 1;
            let anid = nid(NID_TYPE_ATTACHMENT, self.attach);
            let mut own = vec![(PR_ATTACH_SIZE, Value::Long(0)), (PR_RENDERING_POSITION, Value::Long(-1))];
            let (mut objects, mut extra, mut object_size) = (Vec::<Object>::new(), Vec::new(), 0);
            let mut name = text(&a.props, PR_ATTACH_LONG_FILENAME_W).map(str::to_owned);
            if let Some(e) = &a.embedded {
                let inner = self.object(e, &[])?;
                self.ltp += 1;
                let enid = nid(NID_TYPE_NORMAL_MESSAGE, self.ltp);
                extra.push(Sub { nid: enid, data: inner.data, sub: inner.sub });
                object_size = inner.len;
                objects.push((PR_ATTACH_DATA_OBJ, enid, u32::try_from(inner.len).unwrap_or(u32::MAX)));
                if !a.props.iter().any(|(t, _)| *t == PR_ATTACH_METHOD) {
                    own.push((PR_ATTACH_METHOD, Value::Long(ATTACH_EMBEDDED_MSG)));
                }
                if name.is_none() {
                    name = text(&e.props, PR_SUBJECT_W).map(str::to_owned);
                }
            }
            if let Some(name) = name {
                own.push((PR_ATTACH_FILENAME_W, Value::Unicode(name.clone())));
                own.push((PR_DISPLAY_NAME_W, Value::Unicode(name)));
            }
            let embedded = a.embedded.is_some();
            let theirs = || a.props.iter().filter(|(t, _)| !embedded || *t >> 16 != PR_ATTACH_DATA_BIN >> 16);
            let size = props_size(&chain(&own, theirs())) + object_size;
            own[0].1 = Value::Long(clamp(size));
            attachments += size;
            let props = chain(&own, theirs());
            let (data, sub, len) = pc_node(&mut self.ndb, &mut self.ltp, &props, &objects, extra)?;
            tree += len + object_size;
            subs.push(Sub { nid: anid, data, sub });
            rows.push(self.row(anid, &props, &ATTACHMENTS));
        }
        if !rows.is_empty() {
            let (data, sub, len) = tc_node(&mut self.ndb, &mut self.ltp, &ATTACHMENTS, &rows)?;
            tree += len;
            subs.push(Sub { nid: NID_ATTACHMENT_TABLE, data, sub });
        }
        drop(rows);

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
            self.unique += 1;
            recipients.push(Row { id: i as u32 + 1, ver: self.unique, values });
        }
        let (data, sub, len) = tc_node(&mut self.ndb, &mut self.ltp, &tags, &recipients)?;
        drop(recipients);
        tree += len;
        subs.push(Sub { nid: NID_RECIPIENT_TABLE, data, sub });

        let mut flags = long(&m.props, PR_MESSAGE_FLAGS);
        if !m.attachments.is_empty() {
            flags |= MSGFLAG_HASATTACH;
        }
        let time = time(&m.props, PR_MESSAGE_DELIVERY_TIME).or_else(|| time(&m.props, PR_CLIENT_SUBMIT_TIME)).unwrap_or_else(now);
        let mut own = vec![
            (PR_MESSAGE_CLASS_W, Value::Unicode(m.class.clone())),
            (PR_MESSAGE_FLAGS, Value::Long(flags)),
            (PR_MESSAGE_SIZE, Value::Long(0)),
            (PR_MESSAGE_STATUS, Value::Long(0)),
            (PR_CREATION_TIME, Value::SysTime(time)),
            (PR_LAST_MODIFICATION_TIME, Value::SysTime(time)),
            (PR_SEARCH_KEY, Value::Binary(guid().to_vec())),
        ];
        if !m.attachments.is_empty() {
            own.push((PR_ATTACHMENTS_SIZE, Value::Long(clamp(attachments))));
        }
        if let Some(topic) = text(&m.props, PR_NORMALIZED_SUBJECT_W).or_else(|| text(&m.props, PR_SUBJECT_W)) {
            own.push((PR_CONVERSATION_TOPIC_W, Value::Unicode(topic.into())));
        }
        // Объём контекста свойств от значения `PR_MESSAGE_SIZE` не зависит (число — в самой записи BTH): черновик без
        // записи даёт размер, письмо пишется один раз.
        let mut scratch = self.ltp;
        let mut draft = Subs::new(&mut scratch);
        let pages = ltp::pc(None, &mut draft, &chain(&own, &m.props), &[])?;
        let size = pages.iter().map(|p| p.len() as u64).sum::<u64>() + draft.bytes + tree;
        own[2].1 = Value::Long(clamp(size));

        let props = chain(&own, &m.props);
        let mut subs_own = Subs::new(&mut self.ltp);
        let blocks = ltp::pc(Some(&mut self.ndb), &mut subs_own, &props, &[])?;
        subs.extend(subs_own.list);
        let data = self.ndb.chunks(&blocks)?;
        let sub = self.ndb.subnodes(subs)?;
        let row = props.iter().filter(|(t, v)| row_tags.contains(t) && *t as u16 == v.prop_type()).map(|(t, v)| (*t, (*v).clone())).collect();
        Ok(Built { data, sub, len: size, flags, row })
    }

    /// Том готов: таблица текущей папки, папки и их таблицы, служебные узлы, деревья, заголовок.
    fn finish(mut self, files: &mut impl Files) -> io::Result<()> {
        self.seal()?;
        let ipm_nid = nid(NID_TYPE_NORMAL_FOLDER, IPM_SUBTREE);
        let props: Vec<Vec<(Tag, Value)>> = self
            .folders
            .iter()
            .map(|f| folder_props(&f.name, f.count, f.unread, !f.children.is_empty(), if f.index == WASTEBASKET { None } else { f.class }))
            .collect();
        let mut ipm_children = Vec::new();
        for i in 0..self.folders.len() {
            let (index, parent) = (self.folders[i].index, self.folders[i].parent);
            let parent_nid = parent.map_or(ipm_nid, |p| nid(NID_TYPE_NORMAL_FOLDER, self.folders[p].index));
            self.pc(nid(NID_TYPE_NORMAL_FOLDER, index), parent_nid, &refs(&props[i]))?;
            let children: Vec<usize> = self.folders[i].children.clone();
            let rows: Vec<Row> = children
                .iter()
                .map(|&c| {
                    let id = nid(NID_TYPE_NORMAL_FOLDER, self.folders[c].index);
                    self.row(id, &refs(&props[c]), &HIERARCHY)
                })
                .collect();
            self.tc(nid(NID_TYPE_HIERARCHY_TABLE, index), &HIERARCHY, &rows)?;
            match self.folders[i].contents {
                Some((data, sub)) => self.ndb.node(nid(NID_TYPE_CONTENTS_TABLE, index), data, sub, 0)?,
                None => self.tc(nid(NID_TYPE_CONTENTS_TABLE, index), &CONTENTS, &[])?,
            }
            self.tc(nid(NID_TYPE_ASSOC_CONTENTS_TABLE, index), &ASSOC_CONTENTS, &[])?;
            if parent.is_none() {
                ipm_children.push(self.row(nid(NID_TYPE_NORMAL_FOLDER, index), &refs(&props[i]), &HIERARCHY));
            }
        }

        let ipm = folder_props("Top of Outlook data file", 0, 0, true, None);
        self.folder_node(IPM_SUBTREE, NID_ROOT_FOLDER, &ipm, &ipm_children)?;
        let finder = folder_props("Search Root", 0, 0, false, None);
        self.folder_node(FINDER, NID_ROOT_FOLDER, &finder, &[])?;

        // Папка поиска спама: своя очередь обновлений (пустая), условия и таблица найденного.
        let spam_nid = nid(NID_TYPE_SEARCH_FOLDER, SPAM_SEARCH);
        let spam = folder_props("SPAM Search Folder 2", 0, 0, false, None);
        self.pc(spam_nid, NID_ROOT_FOLDER, &refs(&spam))?;
        self.ndb.node(nid(NID_TYPE_SEARCH_UPDATE_QUEUE, SPAM_SEARCH), 0, 0, 0)?;
        self.pc(nid(NID_TYPE_SEARCH_CRITERIA_OBJECT, SPAM_SEARCH), 0, &[(PR_SEARCH_CRITERIA_FLAGS, &Value::Long(0))])?;
        self.tc(nid(NID_TYPE_SEARCH_CONTENTS_TABLE, SPAM_SEARCH), &SEARCH_CONTENTS, &[])?;

        let root_children = [
            self.row(ipm_nid, &refs(&ipm), &HIERARCHY),
            self.row(nid(NID_TYPE_NORMAL_FOLDER, FINDER), &refs(&finder), &HIERARCHY),
            self.row(spam_nid, &refs(&spam), &HIERARCHY),
        ];
        // Корень: имя пустое, родитель — он сам.
        self.folder_node(NID_ROOT_FOLDER >> 5, NID_ROOT_FOLDER, &folder_props("", 0, 0, true, None), &root_children)?;

        let entry = |index: u32| -> Value {
            let mut e = vec![0; 4];
            e.extend(self.record_key);
            e.extend(nid(NID_TYPE_NORMAL_FOLDER, index).to_le_bytes());
            Value::Binary(e)
        };
        let store = [
            (PR_REPL_FLAGS, Value::Long(0)),
            (PR_RECORD_KEY, Value::Binary(self.record_key.to_vec())),
            (PR_DISPLAY_NAME_W, Value::Unicode(self.store.clone())),
            (PR_VALID_FOLDER_MASK, Value::Long(FOLDER_MASK)),
            (PR_IPM_SUBTREE_ENTRYID, entry(IPM_SUBTREE)),
            (PR_IPM_WASTEBASKET_ENTRYID, entry(WASTEBASKET)),
            (PR_FINDER_ENTRYID, entry(FINDER)),
            (PR_PST_LR_NO_RESTRICTIONS, Value::Boolean(true)),
            (PR_PST_ENSURE, Value::Long(PST_ENSURE)),
            (PR_PST_BUILD, Value::Long(PST_BUILD)),
            (PR_PST_PASSWORD, Value::Long(0)),
        ];
        self.pc(NID_MESSAGE_STORE, 0, &refs(&store))?;
        // Таблица именованных свойств пуста: 251 корзина, потоки без записей.
        let nameid = [
            (PR_NAMEID_BUCKET_COUNT, Value::Long(251)),
            (PR_NAMEID_STREAM_GUID, Value::Binary(Vec::new())),
            (PR_NAMEID_STREAM_ENTRY, Value::Binary(Vec::new())),
            (PR_NAMEID_STREAM_STRING, Value::Binary(Vec::new())),
        ];
        self.pc(NID_NAME_TO_ID_MAP, 0, &refs(&nameid))?;
        for n in [NID_SEARCH_MANAGEMENT_QUEUE, NID_SEARCH_DOMAIN_OBJECT, NID_SEARCH_EC1] {
            self.ndb.node(n, 0, 0, 0)?;
        }
        // Список активности поиска (MS-PST 2.4.8.4.2): NID папок, у которых есть папка поиска, — папка поиска спама.
        let sal = self.ndb.block(&spam_nid.to_le_bytes(), false)?;
        self.ndb.node(NID_SEARCH_ACTIVITY_LIST, sal, 0, 0)?;
        let hmp = ltp::hmp()?;
        let hmp = self.ndb.chunks(&hmp)?;
        self.ndb.node(NID_HST_HMP, hmp, 0, 0)?;
        self.tc(NID_HIERARCHY_TABLE_TEMPLATE, &HIERARCHY, &[])?;
        self.tc(NID_CONTENTS_TABLE_TEMPLATE, &CONTENTS, &[])?;
        self.tc(NID_ASSOC_CONTENTS_TABLE_TEMPLATE, &ASSOC_CONTENTS, &[])?;
        self.tc(NID_SEARCH_CONTENTS_TABLE_TEMPLATE, &SEARCH_CONTENTS, &[])?;
        self.tc(NID_ATTACHMENT_TABLE, &ATTACHMENTS, &[])?;
        self.tc(NID_RECIPIENT_TABLE, &RECIPIENTS, &[])?;
        // Папка приёма по умолчанию (класс "") — корневая, как в новом файле Outlook; очередь исходящих пуста.
        let receive = [(PR_RECEIVE_FOLDER_NID, Value::Long(NID_ROOT_FOLDER as i32)), (PR_MESSAGE_CLASS_W, Value::Unicode(String::new()))];
        let receive = self.row(1, &refs(&receive), &RECEIVE_FOLDERS);
        self.tc(NID_RECEIVE_FOLDER_TABLE, &RECEIVE_FOLDERS, &[receive])?;
        self.tc(NID_OUTGOING_QUEUE, &OUTGOING_QUEUE, &[])?;
        self.tc(NID_CHANGE_HISTORY_TABLE_TEMPLATE, &CHANGE_HISTORY, &[])?;
        self.tc(NID_TOMBSTONE_TABLE_TEMPLATE, &TOMBSTONES, &[])?;
        self.tc(NID_TOMBSTONE_DATE_TABLE_TEMPLATE, &TOMBSTONE_DATES, &[])?;

        // rgnid: последний занятый индекс по типам (по умолчанию — как в новом файле Outlook).
        let mut nids = [FIRST_NID; 32];
        nids[NID_TYPE_SEARCH_FOLDER as usize] = 0x4000;
        nids[NID_TYPE_NORMAL_MESSAGE as usize] = self.message;
        nids[0x08] = 0x8000;
        let last_folder = (self.next_folder - 1).max(WASTEBASKET);
        for t in [NID_TYPE_NORMAL_FOLDER, NID_TYPE_HIERARCHY_TABLE, NID_TYPE_CONTENTS_TABLE, NID_TYPE_ASSOC_CONTENTS_TABLE] {
            nids[t as usize] = last_folder;
        }
        nids[NID_TYPE_ATTACHMENT as usize] = self.attach;
        nids[0x1F] = self.ltp;
        let unique = self.unique;
        let file = self.file.clone();
        self.ndb.finish(unique, &nids)?;
        for kind in ["nbt", "bbt"] {
            files.remove(&spill(&file, kind))?;
        }
        Ok(())
    }

    /// Узел NBT с контекстом свойств.
    fn pc(&mut self, nid: u32, parent: u32, props: &[(Tag, &Value)]) -> io::Result<()> {
        let (data, sub, _) = pc_node(&mut self.ndb, &mut self.ltp, props, &[], Vec::new())?;
        self.ndb.node(nid, data, sub, parent)
    }

    /// Узел NBT с контекстом таблицы в памяти.
    fn tc(&mut self, nid: u32, tags: &[Tag], rows: &[Row]) -> io::Result<()> {
        let (data, sub, _) = tc_node(&mut self.ndb, &mut self.ltp, tags, rows)?;
        self.ndb.node(nid, data, sub, 0)
    }

    /// Служебная папка: свойства и три таблицы — иерархии (подпапки), содержимого и связанного содержимого (пустые).
    fn folder_node(&mut self, index: u32, parent: u32, props: &[(Tag, Value)], children: &[Row]) -> io::Result<()> {
        self.pc(nid(NID_TYPE_NORMAL_FOLDER, index), parent, &refs(props))?;
        self.tc(nid(NID_TYPE_HIERARCHY_TABLE, index), &HIERARCHY, children)?;
        self.tc(nid(NID_TYPE_CONTENTS_TABLE, index), &CONTENTS, &[])?;
        self.tc(nid(NID_TYPE_ASSOC_CONTENTS_TABLE, index), &ASSOC_CONTENTS, &[])
    }
}

/// Записанное письмо: данные и поддерево узла, байт со всем поддеревом, флаги, значения строки таблицы содержимого.
struct Built {
    data: u64,
    sub: u64,
    len: u64,
    flags: i32,
    row: Vec<(Tag, Value)>,
}

/// Контекст свойств в узел или подузел → (данные, поддерево: значения-подузлы и `extra`, байт данных вместе с
/// подузлами-значениями).
fn pc_node(ndb: &mut Ndb, ltp: &mut u32, props: &[(Tag, &Value)], objects: &[Object], extra: Vec<Sub>) -> io::Result<(u64, u64, u64)> {
    let mut subs = Subs::new(ltp);
    let blocks = ltp::pc(Some(&mut *ndb), &mut subs, props, objects)?;
    let len = blocks.iter().map(|b| b.len() as u64).sum::<u64>() + subs.bytes;
    let mut list = subs.list;
    list.extend(extra);
    let data = ndb.chunks(&blocks)?;
    let sub = if list.is_empty() { 0 } else { ndb.subnodes(list)? };
    Ok((data, sub, len))
}

fn tc_node(ndb: &mut Ndb, ltp: &mut u32, tags: &[Tag], rows: &[Row]) -> io::Result<(u64, u64, u64)> {
    let mut subs = Subs::new(ltp);
    let blocks = ltp::tc(ndb, &mut subs, tags, rows)?;
    let len = blocks.iter().map(|b| b.len() as u64).sum::<u64>() + subs.bytes;
    let data = ndb.chunks(&blocks)?;
    let sub = if subs.list.is_empty() { 0 } else { ndb.subnodes(subs.list)? };
    Ok((data, sub, len))
}

/// Свойства ссылками (контекст свойств и строки таблиц их не копируют).
fn refs(p: &[(Tag, Value)]) -> Vec<(Tag, &Value)> {
    p.iter().map(|(t, v)| (*t, v)).collect()
}

/// Свои свойства объекта, затем его же из письма (в контексте свойств остаётся первое).
fn chain<'c, 'a: 'c, 'b: 'c>(own: &'a [(Tag, Value)], theirs: impl IntoIterator<Item = &'b (Tag, Value)>) -> Vec<(Tag, &'c Value)> {
    own.iter().map(|(t, v)| (*t, v)).chain(theirs.into_iter().map(|(t, v)| (*t, v))).collect()
}

/// Сколько займёт письмо в томе — с запасом: свойства, по узлу на объект, вложенные письма.
fn estimate(m: &MapiMessage) -> u64 {
    let size = |p: &[(Tag, Value)]| props_size(&refs(p));
    let mut n = size(&m.props) + 4096;
    for r in &m.recipients {
        n += size(&r.props) * 2 + 512;
    }
    for a in &m.attachments {
        n += size(&a.props) + 1024 + a.embedded.as_deref().map_or(0, estimate);
    }
    n + n / 16
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
fn props_size(props: &[(Tag, &Value)]) -> u64 {
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

fn folder_props(name: &str, count: u32, unread: i32, subfolders: bool, class: Option<&str>) -> Vec<(Tag, Value)> {
    let mut props = vec![
        (PR_DISPLAY_NAME_W, Value::Unicode(name.into())),
        (PR_CONTENT_COUNT, Value::Long(clamp(u64::from(count)))),
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
