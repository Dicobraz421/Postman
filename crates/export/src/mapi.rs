//! Письмо свойствами MAPI (MS-OXCMSG, MS-OXPROPS) — для MSG (S8-30) и PST (S8-33). Из MIME их заполняет воркер
//! (S8-29: разбор чужого письма — только в песочнице), файл собирает этот крейт. Здесь — только типы и значения.
//!
//! Тег свойства — `(id << 16) | тип`; строки `_W` — `PT_UNICODE` (в файле — UTF-16LE), время — FILETIME.

/// Тег свойства: идентификатор в старших 16 битах, тип — в младших.
pub type Tag = u32;

// Типы свойств (MS-OXCDATA 2.11.1).
pub const PT_LONG: u16 = 0x0003;
pub const PT_BOOLEAN: u16 = 0x000B;
pub const PT_OBJECT: u16 = 0x000D;
pub const PT_UNICODE: u16 = 0x001F;
pub const PT_SYSTIME: u16 = 0x0040;
pub const PT_BINARY: u16 = 0x0102;

// Письмо.
pub const PR_MESSAGE_CLASS_W: Tag = 0x001A_001F;
pub const PR_SUBJECT_W: Tag = 0x0037_001F;
pub const PR_CLIENT_SUBMIT_TIME: Tag = 0x0039_0040;
pub const PR_SUBJECT_PREFIX_W: Tag = 0x003D_001F;
pub const PR_SENT_REPRESENTING_NAME_W: Tag = 0x0042_001F;
pub const PR_SENT_REPRESENTING_ADDRTYPE_W: Tag = 0x0064_001F;
pub const PR_SENT_REPRESENTING_EMAIL_ADDRESS_W: Tag = 0x0065_001F;
pub const PR_TRANSPORT_MESSAGE_HEADERS_W: Tag = 0x007D_001F;
pub const PR_SENDER_NAME_W: Tag = 0x0C1A_001F;
pub const PR_SENDER_ADDRTYPE_W: Tag = 0x0C1E_001F;
pub const PR_SENDER_EMAIL_ADDRESS_W: Tag = 0x0C1F_001F;
pub const PR_DISPLAY_BCC_W: Tag = 0x0E02_001F;
pub const PR_DISPLAY_CC_W: Tag = 0x0E03_001F;
pub const PR_DISPLAY_TO_W: Tag = 0x0E04_001F;
pub const PR_MESSAGE_DELIVERY_TIME: Tag = 0x0E06_0040;
pub const PR_MESSAGE_FLAGS: Tag = 0x0E07_0003;
pub const PR_NORMALIZED_SUBJECT_W: Tag = 0x0E1D_001F;
pub const PR_BODY_W: Tag = 0x1000_001F;
pub const PR_HTML: Tag = 0x1013_0102;
pub const PR_INTERNET_MESSAGE_ID_W: Tag = 0x1035_001F;
pub const PR_IN_REPLY_TO_ID_W: Tag = 0x1042_001F;
pub const PR_INTERNET_CPID: Tag = 0x3FDE_0003;
/// Что умеет хранилище: в MSG — [`STORE_UNICODE_OK`], строки `_W`.
pub const PR_STORE_SUPPORT_MASK: Tag = 0x340D_0003;
pub const PR_SENDER_SMTP_ADDRESS_W: Tag = 0x5D01_001F;
pub const PR_SENT_REPRESENTING_SMTP_ADDRESS_W: Tag = 0x5D02_001F;

// Получатель.
pub const PR_RECIPIENT_TYPE: Tag = 0x0C15_0003;
/// Номер получателя в письме.
pub const PR_ROWID: Tag = 0x3000_0003;
pub const PR_DISPLAY_NAME_W: Tag = 0x3001_001F;
pub const PR_ADDRTYPE_W: Tag = 0x3002_001F;
pub const PR_EMAIL_ADDRESS_W: Tag = 0x3003_001F;
pub const PR_SMTP_ADDRESS_W: Tag = 0x39FE_001F;

// Вложение.
/// Номер вложения в письме.
pub const PR_ATTACH_NUM: Tag = 0x0E21_0003;
pub const PR_ATTACH_DATA_BIN: Tag = 0x3701_0102;
/// Вложенное письмо: в MSG — хранилище `__substg1.0_3701000D` ([`Attachment::embedded`]).
pub const PR_ATTACH_DATA_OBJ: Tag = 0x3701_000D;
pub const PR_ATTACH_METHOD: Tag = 0x3705_0003;
pub const PR_ATTACH_LONG_FILENAME_W: Tag = 0x3707_001F;
pub const PR_ATTACH_MIME_TAG_W: Tag = 0x370E_001F;
pub const PR_ATTACH_CONTENT_ID_W: Tag = 0x3712_001F;

// Значения.
/// `PR_MESSAGE_FLAGS`: прочитано.
pub const MSGFLAG_READ: i32 = 0x0001;
/// `PR_RECIPIENT_TYPE`.
pub const MAPI_TO: i32 = 1;
pub const MAPI_CC: i32 = 2;
pub const MAPI_BCC: i32 = 3;
/// `PR_ATTACH_METHOD`: байты в `PR_ATTACH_DATA_BIN`.
pub const ATTACH_BY_VALUE: i32 = 1;
/// `PR_ATTACH_METHOD`: вложенное письмо.
pub const ATTACH_EMBEDDED_MSG: i32 = 5;
/// `PR_INTERNET_CPID`: UTF-8 (`PR_HTML` — байты в этой кодировке).
pub const CP_UTF8: i32 = 65001;
/// `PR_STORE_SUPPORT_MASK`: строки в Unicode.
pub const STORE_UNICODE_OK: i32 = 0x0004_0000;
/// `PR_*_ADDRTYPE_W`.
pub const ADDRTYPE_SMTP: &str = "SMTP";
/// FILETIME начала эпохи Unix (сотни наносекунд от 1601-01-01 UTC).
pub const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000;

// Классы письма (MS-OXOSMIME 2.1.3).
pub const IPM_NOTE: &str = "IPM.Note";
/// Подписанное `multipart/signed`: вложение `SMIME.p7m` — всё письмо.
pub const IPM_NOTE_SMIME_MULTIPART_SIGNED: &str = "IPM.Note.SMIME.MultipartSigned";
/// Зашифрованное или подписанное `application/pkcs7-mime`: вложение `smime.p7m` — CMS, тела нет.
pub const IPM_NOTE_SMIME: &str = "IPM.Note.SMIME";

/// Значение свойства; тип — как в младших 16 битах тега ([`Value::prop_type`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Long(i32),
    Boolean(bool),
    /// FILETIME UTC.
    SysTime(u64),
    Unicode(String),
    Binary(Vec<u8>),
}

impl Value {
    /// Тип свойства (`PT_*`) этого значения.
    pub fn prop_type(&self) -> u16 {
        match self {
            Value::Long(_) => PT_LONG,
            Value::Boolean(_) => PT_BOOLEAN,
            Value::SysTime(_) => PT_SYSTIME,
            Value::Unicode(_) => PT_UNICODE,
            Value::Binary(_) => PT_BINARY,
        }
    }
}

/// Письмо: класс (`PR_MESSAGE_CLASS_W`), свойства по порядку, получатели и вложения.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MapiMessage {
    pub class: String,
    pub props: Vec<(Tag, Value)>,
    pub recipients: Vec<Recipient>,
    pub attachments: Vec<Attachment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Recipient {
    pub props: Vec<(Tag, Value)>,
}

/// Вложение: свойства и, у `ATTACH_EMBEDDED_MSG`, вложенное письмо.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Attachment {
    pub props: Vec<(Tag, Value)>,
    pub embedded: Option<Box<MapiMessage>>,
}
