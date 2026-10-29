//! Локальное хранилище клиента на redb (встраиваемое ключ-значение, чистый Rust).
//!
//! Главная копия почты — на сервере; здесь то, что нужно для мгновенного старта,
//! чтения без сети и списка без держания ящика в памяти. Порядок писем в папке
//! задаёт составной ключ `(папка, -дата, id)`: страница списка — чтение диапазона.
//! Хранилище одноразовое: при смене формата оно пересоздаётся и заполняется синхронизацией.
//!
//! Несколько аккаунтов (S4-06): ключи основного аккаунта — id сервера как есть, ключи общего ящика — с префиксом
//! аккаунта (`<аккаунт>/<id>`, [`scoped`]): id JMAP у разных аккаунтов совпадают (у Stalwart — `a`, `b`…). Префикс
//! у id папок, писем, цепочек, блобов и состояний синхронизации; интерфейс работает с ключами, не разбирая их.
//!
//! Формат 6 (ADR 0006, признак `store6` до приёмки S8): письмо — строка с локальным [`LetterId`], связь с сервером —
//! отдельно ([`RemoteRef`]), папки — с владельцем (учётка или «На этом компьютере»). Прежний API писем — обёртка
//! над ним до S8-09.

use std::path::Path;
use std::sync::Arc;

#[cfg(not(feature = "store6"))]
use redb::TableHandle;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub struct Error(String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

macro_rules! from_err {
    ($($t:ty),*) => {$(
        impl From<$t> for Error {
            fn from(e: $t) -> Self {
                Error(e.to_string())
            }
        }
    )*};
}
from_err!(
    redb::Error,
    redb::DatabaseError,
    redb::TransactionError,
    redb::TableError,
    redb::StorageError,
    redb::CommitError,
    postcard::Error,
    std::io::Error
);

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(feature = "store6")]
mod compat;
#[cfg(feature = "store6")]
mod content;
#[cfg(feature = "store6")]
mod folder;
#[cfg(feature = "store6")]
mod letter;
#[cfg(feature = "store6")]
mod remote;

#[cfg(feature = "store6")]
pub use content::Hash;
#[cfg(feature = "store6")]
pub use folder::{FolderId, FolderRow, Owner, Scope};
#[cfg(feature = "store6")]
pub use letter::{JobId, LetterId, LetterRow, MOVE_BATCH, MoveTicket, Outbound, Place, Step};
#[cfg(feature = "store6")]
pub use remote::RemoteRef;

/// Разделитель аккаунта и id в ключах: id JMAP — только `A-Za-z0-9-_` (RFC 8620 §1.2), `/` в них не бывает.
const SCOPE: char = '/';

/// Ключ кэша для id сервера: основной аккаунт (`None`) — id как есть, общий — `<аккаунт>/<id>`.
pub fn scoped(account: Option<&str>, id: &str) -> String {
    match account {
        Some(acc) => format!("{acc}{SCOPE}{id}"),
        None => id.to_owned(),
    }
}

/// Начало префикса дополнительной учётной записи профиля (S7-03б): `@2`, `@3`… Ключи её писем — `@2/<id>`, её общих
/// ящиков — `@2.<аккаунт>/<id>`. В id JMAP (`A-Za-z0-9-_`) ни `@`, ни `.` не бывает.
pub const PROFILE: char = '@';
const PROFILE_SHARED: char = '.';

/// Учётная запись профиля, к которой относится аккаунт ключа: основная — `None`, дополнительная — `@<n>`.
pub fn profile_of(account: Option<&str>) -> Option<&str> {
    match account {
        Some(a) if a.starts_with(PROFILE) => Some(a.split_once(PROFILE_SHARED).map_or(a, |(p, _)| p)),
        _ => None,
    }
}

/// Аккаунт для ключей: аккаунт сервера `account` (`None` — свой) в учётной записи профиля `profile`.
pub fn in_profile(profile: Option<&str>, account: Option<&str>) -> Option<String> {
    match (profile, account) {
        (None, a) => a.map(str::to_owned),
        (Some(p), None) => Some(p.to_owned()),
        (Some(p), Some(a)) => Some(format!("{p}{PROFILE_SHARED}{a}")),
    }
}

/// Ключ кэша → (аккаунт, id сервера); основной аккаунт — `None`.
pub fn unscoped(key: &str) -> (Option<&str>, &str) {
    match key.split_once(SCOPE) {
        Some((acc, id)) => (Some(acc), id),
        None => (None, key),
    }
}

/// Аккаунт ключа: `None` — основной.
pub fn account_of(key: &str) -> Option<&str> {
    unscoped(key).0
}

/// Права на папку (`Mailbox.myRights`): в общем ящике часть действий запрещена.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rights {
    pub read: bool,
    pub add: bool,
    pub remove: bool,
    pub seen: bool,
    pub keywords: bool,
    pub submit: bool,
}

impl Rights {
    /// Своя папка: можно всё.
    pub const ALL: Self = Self { read: true, add: true, remove: true, seen: true, keywords: true, submit: true };

    /// Только чтение: письма не добавить, не перенести и не удалить. Флаги — по своим правам (Stalwart при
    /// шаринге «только чтение» оставляет `maySetSeen` и `maySetKeywords`).
    pub fn read_only(&self) -> bool {
        !(self.add || self.remove)
    }
}

impl Default for Rights {
    fn default() -> Self {
        Self::ALL
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MailboxRow {
    pub id: String,
    pub parent_id: Option<String>,
    pub name: String,
    pub role: Option<String>,
    pub sort_order: u32,
    pub total: u32,
    pub unread: u32,
    pub rights: Rights,
}

/// Почтовый аккаунт, кроме основного: общий или делегированный ящик (S4-06).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountRow {
    /// id аккаунта на сервере — он же префикс ключей ([`scoped`]).
    pub id: String,
    pub name: String,
    pub is_personal: bool,
    /// Весь аккаунт только для чтения (по сессии); права на папки — [`MailboxRow::rights`].
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmailRow {
    pub id: String,
    pub thread_id: String,
    pub mailbox_ids: Vec<String>,
    /// Unix-время, секунды.
    pub received_at: i64,
    pub subject: String,
    pub from_name: String,
    pub from_addr: String,
    pub preview: String,
    pub seen: bool,
    pub flagged: bool,
    pub has_attachment: bool,
    pub size: u64,
    /// Ключевые слова JMAP, кроме `$seen` и `$flagged` (у них свои поля): категории, метки, `$answered`…
    /// В нижнем регистре, по алфавиту, без повторов ([`EmailRow::set_keyword`]).
    pub keywords: Vec<String>,
}

pub const SEEN: &str = "$seen";
/// Локальная метка строки (S5-12 Ф2): письмо зашифровано S/MIME (корень — `application/pkcs7-mime`
/// `enveloped-data`) — замок и «Зашифрованное письмо» в списке. Ставит синхронизация по заголовку `Content-Type`,
/// на сервер не уходит (`postman_core::sync`).
pub const ENCRYPTED: &str = "$pm-local-encrypted";
pub const FLAGGED: &str = "$flagged";

impl EmailRow {
    /// Есть ли у письма ключевое слово (регистр не важен), включая `$seen` и `$flagged`.
    pub fn has_keyword(&self, keyword: &str) -> bool {
        if keyword.eq_ignore_ascii_case(SEEN) {
            self.seen
        } else if keyword.eq_ignore_ascii_case(FLAGGED) {
            self.flagged
        } else {
            self.keywords.iter().any(|k| k.eq_ignore_ascii_case(keyword))
        }
    }

    /// Поставить или снять ключевое слово; `$seen` и `$flagged` — в свои поля.
    pub fn set_keyword(&mut self, keyword: &str, on: bool) {
        let k = keyword.to_ascii_lowercase();
        match k.as_str() {
            SEEN => self.seen = on,
            FLAGGED => self.flagged = on,
            _ => match (self.keywords.binary_search(&k), on) {
                (Err(at), true) => self.keywords.insert(at, k),
                (Ok(at), false) => {
                    self.keywords.remove(at);
                }
                _ => {}
            },
        }
    }

    /// Ключевые слова в индексе ([`Cache::keyword_count`]): свои и `$flagged`. `$seen` не индексируется — у почти
    /// всех писем он есть, а непрочитанные папки считает сервер (`MailboxRow::unread`).
    #[cfg(not(feature = "store6"))]
    fn indexed(&self) -> impl Iterator<Item = &str> {
        self.keywords.iter().map(String::as_str).chain(self.flagged.then_some(FLAGGED))
    }

    /// Где письмо в индексе ключевых слов: его папки и «все папки» его аккаунта.
    #[cfg(not(feature = "store6"))]
    fn places(&self) -> Vec<String> {
        let mut out = self.mailbox_ids.clone();
        out.push(all_folders(account_of(&self.id)));
        out
    }
}

/// «Все папки» аккаунта (`None` — основного) в индексе ключевых слов: `*` в id JMAP не бывает.
pub fn all_folders(account: Option<&str>) -> String {
    scoped(account, "*")
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Addr {
    pub name: String,
    pub email: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AttachmentRow {
    pub blob_id: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
    /// Content-ID для картинок в тексте (`cid:`).
    pub cid: Option<String>,
    pub inline: bool,
}

/// Задача (S6-02): объект JSCalendar `Task` как есть (модель — `postman_calendar::Task`, разбор — у ядра).
/// Ключ — UID: у задачи, созданной без связи, id сервера ещё нет.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRow {
    pub uid: String,
    /// id на сервере (`CalendarEvent.id`); `None` — ещё не записана.
    pub server_id: Option<String>,
    /// JSCalendar (JSON).
    pub json: String,
    /// Версия с сервера, с которой начата неотправленная правка: на сервер уходят только поля, изменённые у себя
    /// (правки другого клиента в других полях остаются).
    pub base: Option<String>,
    /// Правка не дошла до сервера (нет связи): уйдёт при следующей отправке.
    pub dirty: bool,
    /// Удалена у себя, на сервере ещё нет (вместе с `dirty`).
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BodyRow {
    pub to: Vec<Addr>,
    pub cc: Vec<Addr>,
    pub reply_to: Vec<Addr>,
    pub message_id: Option<String>,
    pub references: Vec<String>,
    pub html: Option<String>,
    pub text: Option<String>,
    pub attachments: Vec<AttachmentRow>,
    /// `Email.blobId` — письмо целиком (RFC 5322): проверка подписи S/MIME (S5-12) нужна точными байтами.
    pub blob_id: Option<String>,
}

/// Получатели письма (S5-03): адреса To и Cc в нижнем регистре — быстрый фильтр «Мне». Отдельно от
/// [`EmailRow`]: строке списка они не нужны, а фильтрам — без чтения тела.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Recipients {
    pub to: Vec<String>,
    pub cc: Vec<String>,
}

/// Правка письма до ответа сервера (жизненный цикл, S5-04): ключевые слова и папки — поставить (`true`) или
/// снять; `$seen` и `$flagged` — в свои поля ([`EmailRow::set_keyword`]). Папки — ключи кэша.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Patch {
    pub id: String,
    pub keywords: Vec<(String, bool)>,
    pub mailboxes: Vec<(String, bool)>,
}

impl Patch {
    pub fn is_empty(&self) -> bool {
        self.keywords.is_empty() && self.mailboxes.is_empty()
    }

    /// Применить к строке письма. Последней папки письмо не лишается: в JMAP письмо всегда в какой-то папке.
    pub fn apply(&self, row: &mut EmailRow) {
        for (k, on) in &self.keywords {
            row.set_keyword(k, *on);
        }
        let mut boxes = row.mailbox_ids.clone();
        for (m, on) in &self.mailboxes {
            match (*on, boxes.iter().position(|b| b == m)) {
                (true, None) => boxes.push(m.clone()),
                (false, Some(i)) => {
                    boxes.remove(i);
                }
                _ => {}
            }
        }
        if !boxes.is_empty() {
            row.mailbox_ids = boxes;
        }
    }
}

/// Где перебирать письма ([`Cache::scan`]): одна папка (новые сверху) или все письма аккаунта (`None` — основного).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scan {
    Mailbox(String),
    Account(Option<String>),
}

/// Blob'ы больше этого не кэшируются (их скачивают заново по требованию).
pub const MAX_CACHED_BLOB: usize = 16 * 1024 * 1024;

/// Меняется при любом изменении формата значений или ключей. 2 — права папок и аккаунты (S4-06);
/// 3 — ключевые слова писем и их индекс (S5-02); 4 — получатели писем (S5-03, [`Recipients`]); 5 — S/MIME (S5-12):
/// `blobId` письма в теле, итоги проверки подписи и сертификаты адресатов.
#[cfg(not(feature = "store6"))]
const FORMAT_VERSION: u64 = 5;
/// 6 — единое хранилище (ADR 0006): письма по [`LetterId`], связь с сервером, папки с владельцем.
#[cfg(feature = "store6")]
const FORMAT_VERSION: u64 = 6;

const META: TableDefinition<&str, u64> = TableDefinition::new("meta");
const MAILBOX: TableDefinition<&str, &[u8]> = TableDefinition::new("mailbox");
#[cfg(not(feature = "store6"))]
const EMAIL: TableDefinition<&str, &[u8]> = TableDefinition::new("email");
/// (папка, -дата, id) → () — порядок списка.
#[cfg(not(feature = "store6"))]
const BY_MAILBOX: TableDefinition<(&str, i64, &str), ()> = TableDefinition::new("by_mailbox");
/// Число писем в папке — чтобы не считать диапазон при каждом обновлении списка.
#[cfg(not(feature = "store6"))]
const COUNT: TableDefinition<&str, u64> = TableDefinition::new("count");
const BODY: TableDefinition<&str, &[u8]> = TableDefinition::new("body");
const BLOB: TableDefinition<&str, &[u8]> = TableDefinition::new("blob");
const TRUSTED: TableDefinition<&str, ()> = TableDefinition::new("trusted_sender");
const STATE: TableDefinition<&str, &str> = TableDefinition::new("sync_state");
/// Общие ящики: id аккаунта → [`AccountRow`].
const ACCOUNT: TableDefinition<&str, &[u8]> = TableDefinition::new("account");
/// id письма → [`Recipients`] (S5-03).
#[cfg(not(feature = "store6"))]
const RECIPIENTS: TableDefinition<&str, &[u8]> = TableDefinition::new("recipients");
/// id письма → итог проверки подписи S/MIME (S5-12; запись ядра, `postman_core::smime`).
const SMIME: TableDefinition<&str, &[u8]> = TableDefinition::new("smime");
/// Адрес (нижний регистр) → сертификаты адресата, собранные из проверенных подписей (S5-12, для шифрования — Ф4).
const CERTS: TableDefinition<&str, &[u8]> = TableDefinition::new("smime_certs");
/// (ключевое слово, папка или «все папки», -дата, id) → () — письма с меткой в папке, новые сверху (S5-02).
#[cfg(not(feature = "store6"))]
const BY_KEYWORD: TableDefinition<(&str, &str, i64, &str), ()> = TableDefinition::new("by_keyword");
/// (ключевое слово, папка или «все папки») → число писем: «N из M» без обхода диапазона.
#[cfg(not(feature = "store6"))]
const KEYWORD_COUNT: TableDefinition<(&str, &str), u64> = TableDefinition::new("keyword_count");

#[cfg(not(feature = "store6"))]
type ByKeyword<'t> = redb::Table<'t, (&'static str, &'static str, i64, &'static str), ()>;
#[cfg(not(feature = "store6"))]
type KeywordCount<'t> = redb::Table<'t, (&'static str, &'static str), u64>;

/// Индекс ключевых слов: письмо `row` добавить (`on`) или убрать. Повтор ничего не меняет (ключи — множество).
#[cfg(not(feature = "store6"))]
fn index_keywords(by: &mut ByKeyword, count: &mut KeywordCount, row: &EmailRow, on: bool) -> Result<()> {
    let places = row.places();
    for kw in row.indexed() {
        for place in &places {
            let key = (kw, place.as_str(), -row.received_at, row.id.as_str());
            let changed = if on { by.insert(key, ())?.is_none() } else { by.remove(key)?.is_some() };
            if changed {
                let cur = count.get((kw, place.as_str()))?.map_or(0, |v| v.value());
                let next = if on { cur.saturating_add(1) } else { cur.saturating_sub(1) };
                if next == 0 {
                    count.remove((kw, place.as_str()))?;
                } else {
                    count.insert((kw, place.as_str()), next)?;
                }
            }
        }
    }
    Ok(())
}
/// (цепочка, дата, id) → () — письма беседы по времени, из всех папок (S5-05). Таблица новее формата 2: в кэше без
/// неё она строится из писем при открытии ([`Cache::init`]), без пересоздания кэша.
#[cfg(not(feature = "store6"))]
const BY_THREAD: TableDefinition<(&str, i64, &str), ()> = TableDefinition::new("by_thread");
/// UID задачи → [`TaskRow`] (S6-02). Таблицы задач новее формата 5: в старом кэше создаются при открытии, письма
/// остаются.
const TASK: TableDefinition<&str, &[u8]> = TableDefinition::new("task");
/// id задачи на сервере → UID: удаление из `CalendarEvent/changes` приходит по id.
const TASK_ID: TableDefinition<&str, &str> = TableDefinition::new("task_id");

/// Страничный кэш redb: памяти важнее, чем лишнее чтение с SSD. Листание демо-ящика на 50 тыс. заполняло 16 МБ
/// целиком (куча процесса +16 МБ, S4-08, `docs/MEMORY.md`); 4 МБ хватает внутренним страницам деревьев, листовые
/// страницы берутся из файлового кэша ОС — кадр листания не меняется.
const CACHE_BYTES: usize = 4 * 1024 * 1024;

fn enc<T: Serialize>(v: &T) -> Result<Vec<u8>> {
    Ok(postcard::to_stdvec(v)?)
}

fn dec<T: DeserializeOwned>(b: &[u8]) -> Result<T> {
    Ok(postcard::from_bytes(b)?)
}

/// Верхняя граница диапазона ключей папки.
const MAX_ID: &str = "\u{10ffff}";

/// Дешёвый в клонировании дескриптор: один `Database` на процесс, общий для UI и синхронизации.
#[derive(Clone)]
pub struct Cache {
    db: Arc<Database>,
}

impl Cache {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let open = || Database::builder().set_cache_size(CACHE_BYTES).create(path);
        let db = match open() {
            Ok(db) => db,
            // Файл другого формата (например, старый кэш SQLite): пересоздаём.
            Err(redb::DatabaseError::Storage(_)) => {
                std::fs::remove_file(path)?;
                open()?
            }
            Err(e) => return Err(e.into()),
        };
        let mut c = Self { db: Arc::new(db) };
        if c.version()? != Some(FORMAT_VERSION) {
            drop(c);
            std::fs::remove_file(path)?;
            c = Self { db: Arc::new(open()?) };
        }
        c.init()?;
        Ok(c)
    }

    pub fn open_in_memory() -> Result<Self> {
        let db = Database::builder().create_with_backend(redb::backends::InMemoryBackend::new())?;
        let c = Self { db: Arc::new(db) };
        c.init()?;
        Ok(c)
    }

    fn version(&self) -> Result<Option<u64>> {
        let tx = self.db.begin_read()?;
        match tx.open_table(META) {
            Ok(t) => Ok(t.get("version")?.map(|v| v.value())),
            // Пустая новая база — тоже «наш» формат.
            Err(redb::TableError::TableDoesNotExist(_)) => Ok(tx.list_tables()?.next().is_none().then_some(FORMAT_VERSION)),
            Err(e) => Err(e.into()),
        }
    }

    fn init(&self) -> Result<()> {
        let tx = self.db.begin_write()?;
        tx.open_table(META)?.insert("version", FORMAT_VERSION)?;
        // Создаём все таблицы, чтобы чтение не спотыкалось об их отсутствие.
        tx.open_table(MAILBOX)?;
        tx.open_table(BODY)?;
        tx.open_table(BLOB)?;
        tx.open_table(TRUSTED)?;
        tx.open_table(STATE)?;
        tx.open_table(ACCOUNT)?;
        tx.open_table(SMIME)?;
        tx.open_table(CERTS)?;
        tx.open_table(TASK)?;
        tx.open_table(TASK_ID)?;
        #[cfg(feature = "store6")]
        letter::init(&tx)?;
        #[cfg(not(feature = "store6"))]
        {
            let threads_indexed = tx.list_tables()?.any(|t| t.name() == BY_THREAD.name());
            tx.open_table(EMAIL)?;
            tx.open_table(BY_MAILBOX)?;
            tx.open_table(COUNT)?;
            tx.open_table(BY_KEYWORD)?;
            tx.open_table(KEYWORD_COUNT)?;
            tx.open_table(RECIPIENTS)?;
            if !threads_indexed {
                let email = tx.open_table(EMAIL)?;
                let mut by = tx.open_table(BY_THREAD)?;
                for r in email.iter()? {
                    let e: EmailRow = dec(r?.1.value())?;
                    by.insert((e.thread_id.as_str(), e.received_at, e.id.as_str()), ())?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Папки аккаунта (`None` — основного) целиком: прежние папки этого аккаунта заменяются, других — остаются.
    /// Ключи в `rows` — уже с префиксом аккаунта.
    pub fn replace_mailboxes(&self, account: Option<&str>, rows: &[MailboxRow]) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut t = tx.open_table(MAILBOX)?;
            t.retain(|k, _| account_of(k) != account)?;
            for m in rows {
                t.insert(m.id.as_str(), enc(m)?.as_slice())?;
            }
        }
        #[cfg(feature = "store6")]
        compat::sync_folders(&tx, rows)?;
        tx.commit()?;
        Ok(())
    }

    /// Общие ящики из сессии основной учётной записи. Аккаунты, которых в сессии больше нет (доступ отозван), —
    /// вместе с их папками, письмами и состояниями.
    pub fn replace_accounts(&self, rows: &[AccountRow]) -> Result<()> {
        self.replace_accounts_of(None, rows)
    }

    /// Аккаунты учётной записи профиля `profile` (S7-03б): у дополнительной — строка её самой (`@<n>`) и её общие
    /// ящики. Аккаунты других учётных записей не трогаются.
    pub fn replace_accounts_of(&self, profile: Option<&str>, rows: &[AccountRow]) -> Result<()> {
        let ours = |id: &str| profile_of(Some(id)) == profile;
        let gone: Vec<String> = self.accounts()?.into_iter().map(|a| a.id).filter(|id| ours(id) && !rows.iter().any(|r| &r.id == id)).collect();
        for acc in &gone {
            self.forget_account(acc)?;
        }
        let tx = self.db.begin_write()?;
        {
            let mut t = tx.open_table(ACCOUNT)?;
            t.retain(|k, _| !ours(k))?;
            for a in rows.iter().filter(|a| ours(&a.id)) {
                t.insert(a.id.as_str(), enc(a)?.as_slice())?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Убрать из кэша всё дополнительной учётной записи `profile` («Настройки → Аккаунты → Удалить», S7-03б).
    pub fn forget_profile(&self, profile: &str) -> Result<()> {
        self.replace_accounts_of(Some(profile), &[])?;
        // Её собственные письма и папки — под ключом `@<n>`, даже если строки аккаунта ещё не было.
        self.forget_account(profile)
    }

    /// Общие ящики по имени.
    pub fn accounts(&self) -> Result<Vec<AccountRow>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(ACCOUNT)?;
        let mut out: Vec<AccountRow> = t.iter()?.map(|r| dec(r?.1.value())).collect::<Result<_>>()?;
        out.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        Ok(out)
    }

    /// Убрать из кэша всё общего ящика `account`: папки, письма (с телами), счётчики, состояния.
    fn forget_account(&self, account: &str) -> Result<()> {
        self.forget_emails(account)?;
        let tx = self.db.begin_write()?;
        {
            let mine = |k: &str| account_of(k) == Some(account);
            tx.open_table(MAILBOX)?.retain(|k, _| !mine(k))?;
            tx.open_table(STATE)?.retain(|k, _| !mine(k))?;
            tx.open_table(BLOB)?.retain(|k, _| !mine(k))?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn mailboxes(&self) -> Result<Vec<MailboxRow>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(MAILBOX)?;
        let mut out: Vec<MailboxRow> = t.iter()?.map(|r| dec(r?.1.value())).collect::<Result<_>>()?;
        out.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then_with(|| a.name.cmp(&b.name)));
        Ok(out)
    }

    /// id писем, у которых тело уже загружено (для пересборки поискового индекса).
    pub fn body_ids(&self, offset: usize, limit: usize) -> Result<Vec<String>> {
        let tx = self.db.begin_read()?;
        let body = tx.open_table(BODY)?;
        body.iter()?.skip(offset).take(limit).map(|r| Ok(r?.0.value().to_string())).collect()
    }
}

/// Письма формата 5: строки по ключу кэша. В формате 6 — обёртка над письмами по [`LetterId`] (`compat`).
#[cfg(not(feature = "store6"))]
impl Cache {
    /// Письма аккаунта `account` со всеми записями о них.
    fn forget_emails(&self, account: &str) -> Result<()> {
        let prefix = scoped(Some(account), "");
        let ids: Vec<String> = {
            let tx = self.db.begin_read()?;
            let email = tx.open_table(EMAIL)?;
            let mut ids = Vec::new();
            for r in email.range(prefix.as_str()..)? {
                let key = r?.0.value().to_string();
                if !key.starts_with(&prefix) {
                    break;
                }
                ids.push(key);
            }
            ids
        };
        self.delete_emails(&ids)?;
        let tx = self.db.begin_write()?;
        {
            let mine = |k: &str| account_of(k) == Some(account);
            tx.open_table(COUNT)?.retain(|k, _| !mine(k))?;
            tx.open_table(RECIPIENTS)?.retain(|k, _| !mine(k))?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn upsert_emails(&self, rows: &[EmailRow]) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut email = tx.open_table(EMAIL)?;
            let mut by = tx.open_table(BY_MAILBOX)?;
            let mut count = tx.open_table(COUNT)?;
            let mut by_kw = tx.open_table(BY_KEYWORD)?;
            let mut kw_count = tx.open_table(KEYWORD_COUNT)?;
            let mut threads = tx.open_table(BY_THREAD)?;
            for e in rows {
                let old: Option<EmailRow> = email.get(e.id.as_str())?.map(|v| dec(v.value())).transpose()?;
                if let Some(old) = &old {
                    for mb in &old.mailbox_ids {
                        if by.remove((mb.as_str(), -old.received_at, old.id.as_str()))?.is_some() {
                            add(&mut count, mb, -1)?;
                        }
                    }
                    index_keywords(&mut by_kw, &mut kw_count, old, false)?;
                    threads.remove((old.thread_id.as_str(), old.received_at, old.id.as_str()))?;
                }
                threads.insert((e.thread_id.as_str(), e.received_at, e.id.as_str()), ())?;
                for mb in &e.mailbox_ids {
                    if by.insert((mb.as_str(), -e.received_at, e.id.as_str()), ())?.is_none() {
                        add(&mut count, mb, 1)?;
                    }
                }
                index_keywords(&mut by_kw, &mut kw_count, e, true)?;
                email.insert(e.id.as_str(), enc(e)?.as_slice())?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Правки писем одной записью (S5-04): папки, индексы и счётчики — как при записи с сервера. Ответ — новые
    /// строки; писем, которых нет, в нём нет.
    pub fn patch_emails(&self, patches: &[Patch]) -> Result<Vec<EmailRow>> {
        let tx = self.db.begin_write()?;
        let mut out = Vec::new();
        {
            let mut email = tx.open_table(EMAIL)?;
            let mut by = tx.open_table(BY_MAILBOX)?;
            let mut count = tx.open_table(COUNT)?;
            let mut by_kw = tx.open_table(BY_KEYWORD)?;
            let mut kw_count = tx.open_table(KEYWORD_COUNT)?;
            for p in patches.iter().filter(|p| !p.is_empty()) {
                let old: Option<EmailRow> = email.get(p.id.as_str())?.map(|v| dec(v.value())).transpose()?;
                let Some(old) = old else { continue };
                let mut row = old.clone();
                p.apply(&mut row);
                for mb in old.mailbox_ids.iter().filter(|m| !row.mailbox_ids.contains(m)) {
                    if by.remove((mb.as_str(), -old.received_at, old.id.as_str()))?.is_some() {
                        add(&mut count, mb, -1)?;
                    }
                }
                for mb in row.mailbox_ids.iter().filter(|m| !old.mailbox_ids.contains(m)) {
                    if by.insert((mb.as_str(), -row.received_at, row.id.as_str()), ())?.is_none() {
                        add(&mut count, mb, 1)?;
                    }
                }
                index_keywords(&mut by_kw, &mut kw_count, &old, false)?;
                index_keywords(&mut by_kw, &mut kw_count, &row, true)?;
                email.insert(row.id.as_str(), enc(&row)?.as_slice())?;
                out.push(row);
            }
        }
        tx.commit()?;
        Ok(out)
    }

    /// Перенести письмо из ящика `from` в `to` (удаление в «Удалённые» — до ответа сервера, чтобы список
    /// изменился сразу). `false` — письма нет или оно не в `from`.
    pub fn move_email(&self, id: &str, from: &str, to: &str) -> Result<bool> {
        let tx = self.db.begin_write()?;
        let moved = {
            let mut email = tx.open_table(EMAIL)?;
            let mut by = tx.open_table(BY_MAILBOX)?;
            let mut count = tx.open_table(COUNT)?;
            let mut by_kw = tx.open_table(BY_KEYWORD)?;
            let mut kw_count = tx.open_table(KEYWORD_COUNT)?;
            let row: Option<EmailRow> = email.get(id)?.map(|v| dec(v.value())).transpose()?;
            match row {
                Some(mut row) if row.mailbox_ids.iter().any(|m| m == from) => {
                    index_keywords(&mut by_kw, &mut kw_count, &row, false)?;
                    row.mailbox_ids.retain(|m| m != from);
                    if by.remove((from, -row.received_at, id))?.is_some() {
                        add(&mut count, from, -1)?;
                    }
                    if !row.mailbox_ids.iter().any(|m| m == to) {
                        row.mailbox_ids.push(to.to_owned());
                        if by.insert((to, -row.received_at, id), ())?.is_none() {
                            add(&mut count, to, 1)?;
                        }
                    }
                    index_keywords(&mut by_kw, &mut kw_count, &row, true)?;
                    email.insert(id, enc(&row)?.as_slice())?;
                    true
                }
                _ => false,
            }
        };
        tx.commit()?;
        Ok(moved)
    }

    pub fn delete_emails(&self, ids: &[String]) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut email = tx.open_table(EMAIL)?;
            let mut by = tx.open_table(BY_MAILBOX)?;
            let mut count = tx.open_table(COUNT)?;
            let mut body = tx.open_table(BODY)?;
            let mut by_kw = tx.open_table(BY_KEYWORD)?;
            let mut kw_count = tx.open_table(KEYWORD_COUNT)?;
            let mut threads = tx.open_table(BY_THREAD)?;
            let mut recipients = tx.open_table(RECIPIENTS)?;
            for id in ids {
                recipients.remove(id.as_str())?;
                let old: Option<EmailRow> = email.remove(id.as_str())?.map(|v| dec(v.value())).transpose()?;
                let Some(old) = old else { continue };
                threads.remove((old.thread_id.as_str(), old.received_at, id.as_str()))?;
                for mb in &old.mailbox_ids {
                    if by.remove((mb.as_str(), -old.received_at, id.as_str()))?.is_some() {
                        add(&mut count, mb, -1)?;
                    }
                }
                index_keywords(&mut by_kw, &mut kw_count, &old, false)?;
                body.remove(id.as_str())?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn count(&self, mailbox_id: &str) -> Result<usize> {
        let tx = self.db.begin_read()?;
        Ok(tx.open_table(COUNT)?.get(mailbox_id)?.map_or(0, |v| v.value() as usize))
    }

    /// Окно списка: новые сверху. Для виртуального скролла.
    pub fn page(&self, mailbox_id: &str, offset: usize, limit: usize) -> Result<Vec<EmailRow>> {
        let tx = self.db.begin_read()?;
        let by = tx.open_table(BY_MAILBOX)?;
        let email = tx.open_table(EMAIL)?;
        let mut out = Vec::with_capacity(limit);
        for k in by.range((mailbox_id, i64::MIN, "")..=(mailbox_id, i64::MAX, MAX_ID))?.skip(offset).take(limit) {
            let (k, _) = k?;
            if let Some(v) = email.get(k.value().2)? {
                out.push(dec(v.value())?);
            }
        }
        Ok(out)
    }

    /// Письма беседы (ключ цепочки, `EmailRow::thread_id`) из всех папок, старые первыми.
    pub fn thread(&self, thread_id: &str) -> Result<Vec<EmailRow>> {
        let tx = self.db.begin_read()?;
        let by = tx.open_table(BY_THREAD)?;
        let email = tx.open_table(EMAIL)?;
        let mut out = Vec::new();
        for k in by.range((thread_id, i64::MIN, "")..=(thread_id, i64::MAX, MAX_ID))? {
            let (k, _) = k?;
            if let Some(v) = email.get(k.value().2)? {
                out.push(dec(v.value())?);
            }
        }
        Ok(out)
    }

    /// Сколько писем в беседе (для переключателя «Беседа (N)»).
    pub fn thread_len(&self, thread_id: &str) -> Result<usize> {
        let tx = self.db.begin_read()?;
        let by = tx.open_table(BY_THREAD)?;
        Ok(by.range((thread_id, i64::MIN, "")..=(thread_id, i64::MAX, MAX_ID))?.count())
    }

    /// Строки по id в заданном порядке (результаты поиска); отсутствующие пропускаются.
    pub fn rows_by_ids(&self, ids: &[String]) -> Result<Vec<EmailRow>> {
        let tx = self.db.begin_read()?;
        let email = tx.open_table(EMAIL)?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(v) = email.get(id.as_str())? {
                out.push(dec(v.value())?);
            }
        }
        Ok(out)
    }

    /// Оставить из `ids` только письма ящика, сохранив порядок.
    pub fn filter_mailbox(&self, ids: &[String], mailbox_id: &str) -> Result<Vec<String>> {
        Ok(self.rows_by_ids(ids)?.into_iter().filter(|e| e.mailbox_ids.iter().any(|m| m == mailbox_id)).map(|e| e.id).collect())
    }

    /// Все письма порциями (для первичного построения поискового индекса).
    pub fn all_rows(&self, offset: usize, limit: usize) -> Result<Vec<EmailRow>> {
        let tx = self.db.begin_read()?;
        let email = tx.open_table(EMAIL)?;
        email.iter()?.skip(offset).take(limit).map(|r| dec(r?.1.value())).collect()
    }

    /// Письма без загруженного тела, свежие первыми (для фоновой предзагрузки).
    pub fn missing_bodies(&self, limit: usize) -> Result<Vec<String>> {
        let tx = self.db.begin_read()?;
        let email = tx.open_table(EMAIL)?;
        let body = tx.open_table(BODY)?;
        let mut out: Vec<(i64, String)> = Vec::new();
        for r in email.iter()? {
            let (k, v) = r?;
            if body.get(k.value())?.is_none() {
                let e: EmailRow = dec(v.value())?;
                out.push((e.received_at, e.id));
            }
        }
        out.sort_by_key(|e| std::cmp::Reverse(e.0));
        Ok(out.into_iter().take(limit).map(|(_, id)| id).collect())
    }

    /// Самое старое письмо ящика в кэше — якорь для догрузки следующей страницы.
    pub fn oldest(&self, mailbox_id: &str) -> Result<Option<String>> {
        let tx = self.db.begin_read()?;
        let by = tx.open_table(BY_MAILBOX)?;
        let last = by.range((mailbox_id, i64::MIN, "")..=(mailbox_id, i64::MAX, MAX_ID))?.next_back().transpose()?;
        Ok(last.map(|(k, _)| k.value().2.to_string()))
    }

    /// Простой поиск по кэшу — запасной путь, если поисковый индекс недоступен.
    pub fn search(&self, query: &str, mailbox_id: Option<&str>, limit: usize) -> Result<Vec<String>> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        if words.is_empty() {
            return Ok(Vec::new());
        }
        let tx = self.db.begin_read()?;
        let email = tx.open_table(EMAIL)?;
        let mut hits: Vec<(i64, String)> = Vec::new();
        for r in email.iter()? {
            let e: EmailRow = dec(r?.1.value())?;
            if mailbox_id.is_some_and(|mb| !e.mailbox_ids.iter().any(|m| m == mb)) {
                continue;
            }
            let hay = format!("{} {} {} {}", e.subject, e.from_name, e.from_addr, e.preview).to_lowercase();
            if words.iter().all(|w| hay.contains(w.as_str())) {
                hits.push((e.received_at, e.id));
            }
        }
        hits.sort_by_key(|h| std::cmp::Reverse(h.0));
        Ok(hits.into_iter().take(limit).map(|(_, id)| id).collect())
    }

    pub fn set_flags(&self, id: &str, seen: bool, flagged: bool) -> Result<()> {
        self.edit_row(id, |row| {
            row.seen = seen;
            row.flagged = flagged;
        })
        .map(|_| ())
    }

    /// Поставить `add` и снять `remove` (ключевые слова JMAP, в том числе `$seen` и `$flagged`) — до ответа сервера,
    /// чтобы список изменился сразу. Новая строка; `None` — письма нет.
    pub fn set_keywords(&self, id: &str, add: &[String], remove: &[String]) -> Result<Option<EmailRow>> {
        self.edit_row(id, |row| {
            for k in remove {
                row.set_keyword(k, false);
            }
            for k in add {
                row.set_keyword(k, true);
            }
        })
    }

    /// Изменить строку письма на месте вместе с индексом ключевых слов (папки и дата — те же).
    fn edit_row(&self, id: &str, change: impl FnOnce(&mut EmailRow)) -> Result<Option<EmailRow>> {
        let tx = self.db.begin_write()?;
        let row = {
            let mut email = tx.open_table(EMAIL)?;
            let mut by_kw = tx.open_table(BY_KEYWORD)?;
            let mut kw_count = tx.open_table(KEYWORD_COUNT)?;
            let row: Option<EmailRow> = email.get(id)?.map(|v| dec(v.value())).transpose()?;
            match row {
                Some(mut row) => {
                    index_keywords(&mut by_kw, &mut kw_count, &row, false)?;
                    change(&mut row);
                    index_keywords(&mut by_kw, &mut kw_count, &row, true)?;
                    email.insert(id, enc(&row)?.as_slice())?;
                    Some(row)
                }
                None => None,
            }
        };
        tx.commit()?;
        Ok(row)
    }

    /// Писем с ключевым словом `keyword` в папке `mailbox` (ключ кэша) или во всех папках аккаунта
    /// ([`all_folders`]). `$seen` не считается (см. [`MailboxRow::unread`]).
    pub fn keyword_count(&self, keyword: &str, mailbox: &str) -> Result<usize> {
        let tx = self.db.begin_read()?;
        let k = keyword.to_ascii_lowercase();
        Ok(tx.open_table(KEYWORD_COUNT)?.get((k.as_str(), mailbox))?.map_or(0, |v| v.value() as usize))
    }

    /// Все ключевые слова папки (или «всех папок») с числом писем — счётчики фильтров одним проходом.
    pub fn keyword_counts(&self, mailbox: &str) -> Result<Vec<(String, usize)>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(KEYWORD_COUNT)?;
        let mut out = Vec::new();
        for r in t.iter()? {
            let (k, v) = r?;
            let (kw, place) = k.value();
            if place == mailbox {
                out.push((kw.to_owned(), v.value() as usize));
            }
        }
        Ok(out)
    }

    /// Получатели писем (`id`, [`Recipients`]) — синхронизация пишет их вместе со строками.
    pub fn set_recipients(&self, rows: &[(String, Recipients)]) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut t = tx.open_table(RECIPIENTS)?;
            for (id, r) in rows {
                t.insert(id.as_str(), enc(r)?.as_slice())?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Получатели письма; не записаны — пусто.
    pub fn recipients(&self, id: &str) -> Result<Recipients> {
        let tx = self.db.begin_read()?;
        Ok(tx.open_table(RECIPIENTS)?.get(id)?.map(|v| dec(v.value())).transpose()?.unwrap_or_default())
    }

    /// Перебрать письма папки (новые сверху) или аккаунта (в порядке ключей) с их получателями — одним чтением,
    /// без списка в памяти (фильтры и их счётчики, S5-03). `f` вернула `false` — остановиться.
    pub fn scan(&self, scan: &Scan, mut f: impl FnMut(EmailRow, Recipients) -> bool) -> Result<()> {
        let tx = self.db.begin_read()?;
        let email = tx.open_table(EMAIL)?;
        let recipients = tx.open_table(RECIPIENTS)?;
        let to = |id: &str| -> Result<Recipients> { Ok(recipients.get(id)?.map(|v| dec(v.value())).transpose()?.unwrap_or_default()) };
        match scan {
            Scan::Mailbox(mb) => {
                let by = tx.open_table(BY_MAILBOX)?;
                for k in by.range((mb.as_str(), i64::MIN, "")..=(mb.as_str(), i64::MAX, MAX_ID))? {
                    let (k, _) = k?;
                    let id = k.value().2;
                    let Some(v) = email.get(id)? else { continue };
                    if !f(dec(v.value())?, to(id)?) {
                        break;
                    }
                }
            }
            Scan::Account(account) => {
                let prefix = scoped(account.as_deref(), "");
                for r in email.range(prefix.as_str()..)? {
                    let (k, v) = r?;
                    let id = k.value();
                    if account_of(id) != account.as_deref() {
                        // Свои письма — без префикса, вперемешку с ключами общих ящиков: пропустить чужие;
                        // общий ящик — подряд по префиксу: дальше чужие.
                        if account.is_some() {
                            break;
                        }
                        continue;
                    }
                    if !f(dec(v.value())?, to(id)?) {
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    /// Сколько писем папки получено не раньше `since` (Unix-секунды) — группы по датам без чтения строк.
    pub fn count_since(&self, mailbox_id: &str, since: i64) -> Result<usize> {
        let tx = self.db.begin_read()?;
        let by = tx.open_table(BY_MAILBOX)?;
        Ok(by.range((mailbox_id, i64::MIN, "")..=(mailbox_id, -since, MAX_ID))?.count())
    }

    /// Письма со словом, которое начинается с `prefix` (`pm.due` — и `pm.due.2026-10-24`), в папке или во всех
    /// папках аккаунта; до `limit` писем, без повторов (S5-04: сроки жизненного цикла — слова с датой).
    pub fn keyword_prefix(&self, prefix: &str, mailbox: &str, limit: usize) -> Result<Vec<EmailRow>> {
        let tx = self.db.begin_read()?;
        let by = tx.open_table(BY_KEYWORD)?;
        let email = tx.open_table(EMAIL)?;
        let p = prefix.to_ascii_lowercase();
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for r in by.range((p.as_str(), "", i64::MIN, "")..)? {
            let (key, _) = r?;
            let (kw, place, _, id) = key.value();
            if !kw.starts_with(p.as_str()) {
                break;
            }
            if place != mailbox || !seen.insert(id.to_owned()) {
                continue;
            }
            if let Some(v) = email.get(id)? {
                out.push(dec(v.value())?);
            }
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    /// Окно писем с ключевым словом в папке (или во всех папках аккаунта), новые сверху.
    pub fn keyword_page(&self, keyword: &str, mailbox: &str, offset: usize, limit: usize) -> Result<Vec<EmailRow>> {
        let tx = self.db.begin_read()?;
        let by = tx.open_table(BY_KEYWORD)?;
        let email = tx.open_table(EMAIL)?;
        let k = keyword.to_ascii_lowercase();
        let (lo, hi) = ((k.as_str(), mailbox, i64::MIN, ""), (k.as_str(), mailbox, i64::MAX, MAX_ID));
        let mut out = Vec::with_capacity(limit.min(1024));
        for r in by.range(lo..=hi)?.skip(offset).take(limit) {
            let (key, _) = r?;
            if let Some(v) = email.get(key.value().3)? {
                out.push(dec(v.value())?);
            }
        }
        Ok(out)
    }
}

impl Cache {
    pub fn body(&self, id: &str) -> Result<Option<BodyRow>> {
        let tx = self.db.begin_read()?;
        tx.open_table(BODY)?.get(id)?.map(|v| dec(v.value())).transpose()
    }

    pub fn put_body(&self, id: &str, b: &BodyRow) -> Result<()> {
        self.put(BODY, id, &enc(b)?)
    }

    pub fn attachments(&self, email_id: &str) -> Result<Vec<AttachmentRow>> {
        Ok(self.body(email_id)?.map(|b| b.attachments).unwrap_or_default())
    }

    pub fn blob(&self, blob_id: &str) -> Result<Option<Vec<u8>>> {
        let tx = self.db.begin_read()?;
        Ok(tx.open_table(BLOB)?.get(blob_id)?.map(|v| v.value().to_vec()))
    }

    /// Сохраняет blob, если он не больше [`MAX_CACHED_BLOB`].
    pub fn put_blob(&self, blob_id: &str, data: &[u8]) -> Result<()> {
        if data.len() > MAX_CACHED_BLOB {
            return Ok(());
        }
        self.put(BLOB, blob_id, data)
    }

    pub fn is_trusted(&self, email: &str) -> Result<bool> {
        let tx = self.db.begin_read()?;
        Ok(tx.open_table(TRUSTED)?.get(email.to_lowercase().as_str())?.is_some())
    }

    pub fn trust(&self, email: &str) -> Result<()> {
        let tx = self.db.begin_write()?;
        tx.open_table(TRUSTED)?.insert(email.to_lowercase().as_str(), ())?;
        tx.commit()?;
        Ok(())
    }

    pub fn state(&self, kind: &str) -> Result<Option<String>> {
        let tx = self.db.begin_read()?;
        Ok(tx.open_table(STATE)?.get(kind)?.map(|v| v.value().to_string()))
    }

    pub fn set_state(&self, kind: &str, value: &str) -> Result<()> {
        let tx = self.db.begin_write()?;
        tx.open_table(STATE)?.insert(kind, value)?;
        tx.commit()?;
        Ok(())
    }

    /// Итог проверки подписи S/MIME письма (S5-12): запись ядра в своём формате.
    pub fn smime<T: DeserializeOwned>(&self, id: &str) -> Result<Option<T>> {
        let tx = self.db.begin_read()?;
        tx.open_table(SMIME)?.get(id)?.map(|v| dec(v.value())).transpose()
    }

    pub fn set_smime<T: Serialize>(&self, id: &str, record: &T) -> Result<()> {
        self.put(SMIME, id, &enc(record)?)
    }

    /// Сертификаты адресата `address` (S5-12): список ядра в своём формате.
    pub fn certs<T: DeserializeOwned>(&self, address: &str) -> Result<Option<T>> {
        let tx = self.db.begin_read()?;
        tx.open_table(CERTS)?.get(address.to_lowercase().as_str())?.map(|v| dec(v.value())).transpose()
    }

    pub fn set_certs<T: Serialize>(&self, address: &str, list: &T) -> Result<()> {
        self.put(CERTS, &address.to_lowercase(), &enc(list)?)
    }

    /// Задачи (без удалённых у себя).
    pub fn tasks(&self) -> Result<Vec<TaskRow>> {
        Ok(self.all_tasks()?.into_iter().filter(|t| !t.deleted).collect())
    }

    /// Все строки задач, и удалённые у себя.
    pub fn all_tasks(&self) -> Result<Vec<TaskRow>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(TASK)?;
        let mut out = Vec::new();
        for r in t.iter()? {
            out.push(dec::<TaskRow>(r?.1.value())?);
        }
        Ok(out)
    }

    /// Правки задач, не дошедшие до сервера.
    pub fn dirty_tasks(&self) -> Result<Vec<TaskRow>> {
        Ok(self.all_tasks()?.into_iter().filter(|t| t.dirty).collect())
    }

    pub fn task(&self, uid: &str) -> Result<Option<TaskRow>> {
        let tx = self.db.begin_read()?;
        tx.open_table(TASK)?.get(uid)?.map(|v| dec(v.value())).transpose()
    }

    /// UID задачи по id сервера.
    pub fn task_uid(&self, server_id: &str) -> Result<Option<String>> {
        let tx = self.db.begin_read()?;
        Ok(tx.open_table(TASK_ID)?.get(server_id)?.map(|v| v.value().to_owned()))
    }

    /// Записать задачи (новые и изменённые) одной транзакцией.
    pub fn put_tasks(&self, rows: &[TaskRow]) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut t = tx.open_table(TASK)?;
            let mut ids = tx.open_table(TASK_ID)?;
            for row in rows {
                let old: Option<TaskRow> = t.get(row.uid.as_str())?.map(|v| dec(v.value())).transpose()?;
                if let Some(old_id) = old.and_then(|o| o.server_id).filter(|i| Some(i) != row.server_id.as_ref()) {
                    ids.remove(old_id.as_str())?;
                }
                if let Some(id) = &row.server_id {
                    ids.insert(id.as_str(), row.uid.as_str())?;
                }
                t.insert(row.uid.as_str(), enc(row)?.as_slice())?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Убрать задачи совсем (по UID).
    pub fn remove_tasks(&self, uids: &[String]) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut t = tx.open_table(TASK)?;
            let mut ids = tx.open_table(TASK_ID)?;
            for uid in uids {
                let old: Option<TaskRow> = t.remove(uid.as_str())?.map(|v| dec(v.value())).transpose()?;
                if let Some(id) = old.and_then(|o| o.server_id) {
                    ids.remove(id.as_str())?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Полная загрузка с сервера: задачи — как в `rows`, прежние без правок у себя — убраны; не дошедшие до
    /// сервера правки (`dirty`) остаются поверх.
    pub fn replace_tasks(&self, rows: &[TaskRow]) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut t = tx.open_table(TASK)?;
            let mut ids = tx.open_table(TASK_ID)?;
            let mut dirty: Vec<TaskRow> = Vec::new();
            for r in t.iter()? {
                let row: TaskRow = dec(r?.1.value())?;
                if row.dirty {
                    dirty.push(row);
                }
            }
            t.retain(|_, _| false)?;
            ids.retain(|_, _| false)?;
            for row in rows.iter().filter(|r| !dirty.iter().any(|d| d.uid == r.uid)).chain(dirty.iter()) {
                if let Some(id) = &row.server_id {
                    ids.insert(id.as_str(), row.uid.as_str())?;
                }
                t.insert(row.uid.as_str(), enc(row)?.as_slice())?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn put(&self, table: TableDefinition<&str, &[u8]>, key: &str, value: &[u8]) -> Result<()> {
        let tx = self.db.begin_write()?;
        tx.open_table(table)?.insert(key, value)?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(not(feature = "store6"))]
fn add(count: &mut redb::Table<&str, u64>, mailbox: &str, delta: i64) -> Result<()> {
    let cur = count.get(mailbox)?.map_or(0, |v| v.value());
    count.insert(mailbox, cur.saturating_add_signed(delta))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn email(id: &str, at: i64, mbs: &[&str]) -> EmailRow {
        EmailRow {
            id: id.into(),
            thread_id: id.into(),
            mailbox_ids: mbs.iter().map(|s| s.to_string()).collect(),
            received_at: at,
            subject: format!("s{id}"),
            from_name: String::new(),
            from_addr: String::new(),
            preview: String::new(),
            seen: false,
            flagged: false,
            has_attachment: false,
            size: 1,
            keywords: Vec::new(),
        }
    }

    fn ids(rows: Vec<EmailRow>) -> Vec<String> {
        rows.into_iter().map(|e| e.id).collect()
    }

    /// Правка жизненного цикла (S5-04): перенос, «непрочитано», слова — одной записью, со счётчиками и индексами.
    #[test]
    fn patch_moves_and_marks() {
        let c = Cache::open_in_memory().unwrap();
        let mut a = email("a", 10, &["in"]);
        a.seen = true;
        c.upsert_emails(&[a, email("b", 20, &["in"])]).unwrap();
        let hide = Patch {
            id: "a".into(),
            keywords: vec![("pm.remind".into(), true), ("pm.remind.2026-10-29t0900".into(), true)],
            mailboxes: vec![("in".into(), false), ("arch".into(), true)],
        };
        let rows = c.patch_emails(&[hide, Patch { id: "zz".into(), ..Patch::default() }]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!((c.count("in").unwrap(), c.count("arch").unwrap()), (1, 1));
        assert_eq!(c.keyword_count("pm.remind", "arch").unwrap(), 1);
        assert_eq!(ids(c.keyword_page("pm.remind", &all_folders(None), 0, 10).unwrap()), ["a"]);
        let back = Patch {
            id: "a".into(),
            keywords: vec![("pm.remind.2026-10-29t0900".into(), false), ("$seen".into(), false)],
            mailboxes: vec![("arch".into(), false), ("in".into(), true)],
        };
        let row = c.patch_emails(&[back]).unwrap().pop().unwrap();
        assert!(!row.seen && row.keywords == ["pm.remind"]);
        assert_eq!(ids(c.page("in", 0, 10).unwrap()), ["b", "a"]);
        assert_eq!((c.count("in").unwrap(), c.count("arch").unwrap()), (2, 0));
        // Последней папки письмо не лишается.
        let mut r = email("x", 1, &["in"]);
        Patch { id: "x".into(), keywords: vec![], mailboxes: vec![("in".into(), false)] }.apply(&mut r);
        assert_eq!(r.mailbox_ids, ["in"]);
    }

    /// Фильтры (S5-03): перебор папки и аккаунта с получателями, группы по датам без чтения строк.
    #[test]
    fn scan_with_recipients_and_count_since() {
        let c = Cache::open_in_memory().unwrap();
        c.upsert_emails(&[email("a", 10, &["in"]), email("b", 30, &["in"]), email("c", 20, &["x"]), email("j/d", 5, &["j/in"])]).unwrap();
        let me = Recipients { to: vec!["me@x".into()], cc: Vec::new() };
        c.set_recipients(&[("b".into(), me.clone())]).unwrap();
        assert_eq!(c.recipients("b").unwrap(), me);
        assert_eq!(c.recipients("a").unwrap(), Recipients::default());
        let mut got = Vec::new();
        c.scan(&Scan::Mailbox("in".into()), |r, to| {
            got.push((r.id, to.to.len()));
            true
        })
        .unwrap();
        assert_eq!(got, [("b".to_string(), 1), ("a".to_string(), 0)]);
        let all = |acc: Option<&str>| {
            let mut out = Vec::new();
            c.scan(&Scan::Account(acc.map(String::from)), |r, _| {
                out.push(r.id);
                true
            })
            .unwrap();
            out.sort();
            out
        };
        assert_eq!(all(None), ["a", "b", "c"]);
        assert_eq!(all(Some("j")), ["j/d"]);
        // Остановка по `false`.
        let mut n = 0;
        c.scan(&Scan::Account(None), |_, _| {
            n += 1;
            false
        })
        .unwrap();
        assert_eq!(n, 1);
        assert_eq!(c.count_since("in", 20).unwrap(), 1);
        assert_eq!(c.count_since("in", 10).unwrap(), 2);
        assert_eq!(c.count_since("in", 31).unwrap(), 0);
        c.delete_emails(&["b".into()]).unwrap();
        assert_eq!(c.recipients("b").unwrap(), Recipients::default());
    }

    #[test]
    fn paging_and_moves() {
        let c = Cache::open_in_memory().unwrap();
        c.upsert_emails(&[email("a", 1, &["in"]), email("b", 3, &["in"]), email("c", 2, &["in", "x"])]).unwrap();
        assert_eq!(ids(c.page("in", 0, 10).unwrap()), ["b", "c", "a"]);
        assert_eq!(c.page("in", 1, 1).unwrap()[0].id, "c");
        assert_eq!(c.oldest("in").unwrap().as_deref(), Some("a"));
        assert_eq!(ids(c.rows_by_ids(&["c".into(), "zz".into(), "a".into()]).unwrap()), ["c", "a"]);
        // Перемещение: письмо пропадает из старого ящика; повторная запись не удваивает счётчик.
        c.upsert_emails(&[email("c", 2, &["x"])]).unwrap();
        c.upsert_emails(&[email("c", 2, &["x"])]).unwrap();
        assert_eq!(c.count("in").unwrap(), 2);
        assert_eq!(c.count("x").unwrap(), 1);
        assert_eq!(c.filter_mailbox(&["a".into(), "c".into()], "x").unwrap(), ["c"]);
        c.delete_emails(&["c".into()]).unwrap();
        assert_eq!(c.count("x").unwrap(), 0);
        assert_eq!(c.all_rows(0, 10).unwrap().len(), 2);
    }

    #[test]
    fn move_to_trash() {
        let c = Cache::open_in_memory().unwrap();
        c.upsert_emails(&[email("a", 1, &["in", "label"]), email("b", 2, &["in"])]).unwrap();
        assert!(c.move_email("a", "in", "trash").unwrap());
        assert_eq!(ids(c.page("in", 0, 10).unwrap()), ["b"]);
        assert_eq!(ids(c.page("trash", 0, 10).unwrap()), ["a"]);
        assert_eq!((c.count("in").unwrap(), c.count("trash").unwrap(), c.count("label").unwrap()), (1, 1, 1));
        let row = c.rows_by_ids(&["a".into()]).unwrap().remove(0);
        assert_eq!(row.mailbox_ids, ["label", "trash"]);
        // Не в этом ящике или нет такого письма — ничего не меняется.
        assert!(!c.move_email("a", "in", "trash").unwrap());
        assert!(!c.move_email("zz", "in", "trash").unwrap());
        assert_eq!(c.count("trash").unwrap(), 1);
    }

    #[test]
    fn mailboxes_flags_and_search() {
        let c = Cache::open_in_memory().unwrap();
        let mb = |id: &str, order| MailboxRow {
            id: id.into(),
            parent_id: None,
            name: id.into(),
            role: None,
            sort_order: order,
            total: 0,
            unread: 0,
            rights: Rights::ALL,
        };
        c.replace_mailboxes(None, &[mb("b", 2), mb("a", 1)]).unwrap();
        c.replace_mailboxes(None, &[mb("z", 2), mb("a", 1)]).unwrap();
        assert_eq!(c.mailboxes().unwrap().iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["a", "z"]);
        let mut e = email("a", 1, &["in"]);
        e.subject = "Отчёт за КВАРТАЛ".into();
        c.upsert_emails(&[e, email("b", 2, &["x"])]).unwrap();
        c.set_flags("a", true, true).unwrap();
        let row = &c.page("in", 0, 1).unwrap()[0];
        assert!(row.seen && row.flagged);
        assert_eq!(c.search("квартал", None, 10).unwrap(), ["a"]);
        assert!(c.search("квартал", Some("x"), 10).unwrap().is_empty());
    }

    /// Общие ящики (S4-06): id сервера у аккаунтов совпадают — ключи с префиксом не путаются; папки аккаунта
    /// заменяются отдельно; аккаунт, которого больше нет в сессии, уходит из кэша целиком.
    #[test]
    fn shared_accounts_have_their_own_keys() {
        assert_eq!(scoped(None, "a"), "a");
        assert_eq!(scoped(Some("i"), "a"), "i/a");
        assert_eq!(unscoped("i/a"), (Some("i"), "a"));
        assert_eq!(unscoped("a"), (None, "a"));
        assert_eq!(account_of("i/Email"), Some("i"));

        let c = Cache::open_in_memory().unwrap();
        let mb = |id: &str, rights| MailboxRow {
            id: id.into(),
            parent_id: None,
            name: id.into(),
            role: Some("inbox".into()),
            sort_order: 0,
            total: 0,
            unread: 0,
            rights,
        };
        let ro = Rights { read: true, add: false, remove: false, seen: false, keywords: false, submit: false };
        assert!(ro.read_only() && !Rights::ALL.read_only());
        c.replace_mailboxes(None, &[mb("a", Rights::ALL)]).unwrap();
        c.replace_mailboxes(Some("i"), &[mb("i/a", Rights::ALL)]).unwrap();
        c.replace_mailboxes(Some("j"), &[mb("j/a", ro)]).unwrap();
        // Папки общего ящика заменяются, свои — остаются.
        c.replace_mailboxes(Some("i"), &[mb("i/a", Rights::ALL), mb("i/b", Rights::ALL)]).unwrap();
        let ids = |c: &Cache| c.mailboxes().unwrap().into_iter().map(|m| m.id).collect::<Vec<_>>();
        assert_eq!(ids(&c), ["a", "i/a", "i/b", "j/a"]);
        assert_eq!(c.mailboxes().unwrap()[3].rights, ro);

        // Одинаковый id письма в двух аккаунтах — два письма.
        c.upsert_emails(&[email("e1", 1, &["a"]), email("i/e1", 2, &["i/a"]), email("j/e1", 3, &["j/a"])]).unwrap();
        assert_eq!((c.count("a").unwrap(), c.count("i/a").unwrap()), (1, 1));
        assert_eq!(c.oldest("i/a").unwrap().as_deref(), Some("i/e1"));
        c.put_body("i/e1", &BodyRow::default()).unwrap();
        c.put_blob("i/B1", b"x").unwrap();
        c.set_state(&scoped(Some("i"), "Email"), "s-i").unwrap();
        c.set_state("Email", "s-own").unwrap();

        let acc = |id: &str| AccountRow { id: id.into(), name: id.into(), is_personal: false, read_only: false };
        c.replace_accounts(&[acc("j"), acc("i")]).unwrap();
        assert_eq!(c.accounts().unwrap(), [acc("i"), acc("j")]);
        // Доступ к «i» отозван: его папки, письма, тела, блобы и состояние — из кэша.
        c.replace_accounts(&[acc("j")]).unwrap();
        assert_eq!(ids(&c), ["a", "j/a"]);
        assert!(c.rows_by_ids(&["i/e1".into()]).unwrap().is_empty());
        assert_eq!((c.count("i/a").unwrap(), c.count("a").unwrap()), (0, 1));
        assert!(c.body("i/e1").unwrap().is_none() && c.blob("i/B1").unwrap().is_none());
        assert_eq!(c.state("i/Email").unwrap(), None);
        assert_eq!(c.state("Email").unwrap().as_deref(), Some("s-own"));
        assert_eq!(c.missing_bodies(10).unwrap(), ["j/e1", "e1"]);
    }

    /// Индекс ключевых слов (S5-02): «N из M» по папке и по всем папкам аккаунта, окно писем с меткой; индекс
    /// следует за письмом при обновлении, переносе, смене меток и флага, удалении.
    #[test]
    fn keyword_index_follows_the_letters() {
        let c = Cache::open_in_memory().unwrap();
        let tagged =
            |id: &str, at, mbs: &[&str], kws: &[&str]| EmailRow { keywords: kws.iter().map(|k| k.to_string()).collect(), ..email(id, at, mbs) };
        c.upsert_emails(&[
            tagged("a", 1, &["in"], &["pm.cat.clients"]),
            tagged("b", 3, &["in", "x"], &["pm.cat.clients", "pm.cat.team"]),
            tagged("c", 2, &["x"], &[]),
            tagged("i/a", 5, &["i/in"], &["pm.cat.clients"]),
        ])
        .unwrap();
        let n = |k: &str, mb: &str| c.keyword_count(k, mb).unwrap();
        let own = all_folders(None);
        assert_eq!(own, "*");
        assert_eq!((n("pm.cat.clients", "in"), n("pm.cat.clients", "x"), n("pm.cat.clients", &own)), (2, 1, 2));
        assert_eq!(n("PM.CAT.Clients", "in"), 2, "регистр не важен");
        assert_eq!(n("pm.cat.clients", &all_folders(Some("i"))), 1, "общий ящик — свои «все папки»");
        assert_eq!(n("pm.cat.team", &own), 1);
        assert_eq!(ids(c.keyword_page("pm.cat.clients", "in", 0, 10).unwrap()), ["b", "a"]);
        assert_eq!(ids(c.keyword_page("pm.cat.clients", &own, 1, 10).unwrap()), ["a"]);
        let mut counts = c.keyword_counts("in").unwrap();
        counts.sort();
        assert_eq!(counts, [("pm.cat.clients".to_string(), 2), ("pm.cat.team".to_string(), 1)]);

        // Метки на месте: поставить, снять, повтор ничего не меняет; `$flagged` — в индексе, `$seen` — поле.
        let row = c.set_keywords("c", &["pm.cat.team".into(), "$flagged".into(), "$seen".into()], &[]).unwrap().unwrap();
        assert!(row.flagged && row.seen && row.keywords == ["pm.cat.team"]);
        c.set_keywords("c", &["pm.cat.team".into()], &[]).unwrap();
        assert_eq!((n("pm.cat.team", "x"), n("pm.cat.team", &own), n("$flagged", "x")), (2, 2, 1));
        assert_eq!(n("$seen", "x"), 0, "$seen не индексируется");
        c.set_flags("c", true, false).unwrap();
        assert_eq!(n("$flagged", "x"), 0);
        let row = c.set_keywords("a", &[], &["pm.cat.clients".into()]).unwrap().unwrap();
        assert!(row.keywords.is_empty());
        assert_eq!(n("pm.cat.clients", "in"), 1);
        assert!(c.set_keywords("zz", &["x".into()], &[]).unwrap().is_none());

        // Сервер прислал письмо заново (другие метки) — индекс по новому; перенос и удаление — тоже.
        c.upsert_emails(&[tagged("b", 3, &["in", "x"], &["pm.cat.tenders"])]).unwrap();
        assert_eq!((n("pm.cat.clients", &own), n("pm.cat.tenders", "x"), n("pm.cat.team", "x")), (0, 1, 1));
        assert!(c.move_email("b", "in", "trash").unwrap());
        assert_eq!((n("pm.cat.tenders", "in"), n("pm.cat.tenders", "trash"), n("pm.cat.tenders", &own)), (0, 1, 1));
        c.delete_emails(&["b".into(), "c".into()]).unwrap();
        assert_eq!((n("pm.cat.tenders", &own), n("pm.cat.team", &own)), (0, 0));
        assert!(c.keyword_counts("x").unwrap().is_empty(), "нулевые счётчики не хранятся");
        // Общий ящик отозван — его метки уходят вместе с письмами.
        c.replace_accounts(&[]).unwrap();
        c.replace_accounts(&[AccountRow { id: "i".into(), name: "i".into(), is_personal: false, read_only: false }]).unwrap();
        c.replace_accounts(&[]).unwrap();
        assert_eq!(n("pm.cat.clients", &all_folders(Some("i"))), 0);
    }

    #[test]
    fn row_keywords_stay_sorted_and_unique() {
        let mut r = email("a", 1, &["in"]);
        for k in ["pm.cat.b", "PM.cat.A", "pm.cat.b", "$Flagged"] {
            r.set_keyword(k, true);
        }
        assert_eq!(r.keywords, ["pm.cat.a", "pm.cat.b"]);
        assert!(r.flagged && r.has_keyword("$FLAGGED") && r.has_keyword("pm.cat.A") && !r.has_keyword("$seen"));
        r.set_keyword("pm.cat.a", false);
        r.set_keyword("pm.cat.zz", false);
        assert_eq!(r.keywords, ["pm.cat.b"]);
    }

    /// Индекс бесед (S5-05): письма цепочки из всех папок по времени; перенос, смена даты и удаление его держат;
    /// цепочки аккаунтов не смешиваются; кэш без индекса строит его при открытии.
    #[test]
    fn thread_index() {
        let in_thread = |id: &str, thread: &str, at, mbs: &[&str]| EmailRow { thread_id: thread.into(), ..email(id, at, mbs) };
        let c = Cache::open_in_memory().unwrap();
        c.upsert_emails(&[
            in_thread("r2", "t1", 20, &["in"]),
            in_thread("s1", "t1", 10, &["sent"]),
            in_thread("x", "t2", 15, &["in"]),
            in_thread("i/r3", "i/t1", 5, &["i/in"]),
        ])
        .unwrap();
        let thread = |c: &Cache, t: &str| ids(c.thread(t).unwrap());
        assert_eq!(thread(&c, "t1"), ["s1", "r2"]);
        assert_eq!((c.thread_len("t1").unwrap(), c.thread_len("i/t1").unwrap(), c.thread_len("zz").unwrap()), (2, 1, 0));
        // Новое письмо в середину, повторная запись с другой датой — без дублей.
        c.upsert_emails(&[in_thread("r3", "t1", 15, &["in"]), in_thread("r2", "t1", 30, &["in"])]).unwrap();
        assert_eq!(thread(&c, "t1"), ["s1", "r3", "r2"]);
        assert!(c.move_email("r3", "in", "trash").unwrap());
        assert_eq!(thread(&c, "t1"), ["s1", "r3", "r2"]);
        c.delete_emails(&["s1".into()]).unwrap();
        assert_eq!(thread(&c, "t1"), ["r3", "r2"]);
    }

    /// Кэш формата 2 без таблицы бесед: индекс строится при открытии.
    #[cfg(not(feature = "store6"))]
    #[test]
    fn thread_index_is_built_on_open() {
        let in_thread = |id: &str, thread: &str, at, mbs: &[&str]| EmailRow { thread_id: thread.into(), ..email(id, at, mbs) };
        let thread = |c: &Cache, t: &str| ids(c.thread(t).unwrap());
        let dir = tempfile::Builder::new().prefix("postman-cache-").tempdir().unwrap();
        let path = dir.path().join("c.redb");
        {
            let db = Database::create(&path).unwrap();
            let tx = db.begin_write().unwrap();
            tx.open_table(META).unwrap().insert("version", FORMAT_VERSION).unwrap();
            let mut t = tx.open_table(EMAIL).unwrap();
            for e in [in_thread("a", "t", 2, &["in"]), in_thread("b", "t", 1, &["sent"])] {
                t.insert(e.id.as_str(), enc(&e).unwrap().as_slice()).unwrap();
            }
            drop(t);
            tx.commit().unwrap();
        }
        assert_eq!(thread(&Cache::open(&path).unwrap(), "t"), ["b", "a"]);
    }

    /// S/MIME (S5-12): записи ядра по письму и по адресу; кэш прежнего формата (4, без них) — пересоздаётся.
    #[test]
    fn smime_records_and_old_format() {
        let c = Cache::open_in_memory().unwrap();
        assert_eq!(c.smime::<(u32, String)>("m1").unwrap(), None);
        c.set_smime("m1", &(7u32, "valid".to_string())).unwrap();
        assert_eq!(c.smime::<(u32, String)>("m1").unwrap(), Some((7, "valid".into())));
        c.set_certs("User1@ITT.test", &vec![vec![1u8, 2]]).unwrap();
        assert_eq!(c.certs::<Vec<Vec<u8>>>("user1@itt.test").unwrap(), Some(vec![vec![1, 2]]));

        let dir = tempfile::Builder::new().prefix("postman-cache-").tempdir().unwrap();
        let path = dir.path().join("c.redb");
        {
            let db = Database::create(&path).unwrap();
            let tx = db.begin_write().unwrap();
            tx.open_table(META).unwrap().insert("version", 4u64).unwrap();
            let e = email("old", 1, &["in"]);
            // Таблица писем формата 5 (в формате 6 её нет).
            let email: TableDefinition<&str, &[u8]> = TableDefinition::new("email");
            tx.open_table(email).unwrap().insert("old", enc(&e).unwrap().as_slice()).unwrap();
            tx.commit().unwrap();
        }
        let c = Cache::open(&path).unwrap();
        assert!(c.rows_by_ids(&["old".into()]).unwrap().is_empty(), "формат 4 пересоздан");
        assert_eq!(c.smime::<(u32, String)>("old").unwrap(), None);
    }

    fn task(uid: &str, id: Option<&str>, dirty: bool) -> TaskRow {
        TaskRow { uid: uid.into(), server_id: id.map(Into::into), json: format!("{{\"uid\":\"{uid}\"}}"), base: None, dirty, deleted: false }
    }

    /// Задачи (S6-02): запись, поиск по id сервера, смена id, полная замена с сохранением правок без связи.
    #[test]
    fn tasks_put_find_replace() {
        let c = Cache::open_in_memory().unwrap();
        c.put_tasks(&[task("u1", Some("a"), false), task("u2", None, true)]).unwrap();
        assert_eq!(c.task_uid("a").unwrap().as_deref(), Some("u1"));
        assert_eq!(c.dirty_tasks().unwrap(), [task("u2", None, true)]);
        // Записана на сервер — появился id.
        c.put_tasks(&[task("u2", Some("b"), false)]).unwrap();
        assert_eq!(c.task_uid("b").unwrap().as_deref(), Some("u2"));
        // id сменился (пересоздана) — старый id больше не ведёт к ней.
        c.put_tasks(&[task("u2", Some("c"), false)]).unwrap();
        assert_eq!(c.task_uid("b").unwrap(), None);
        // Удалена у себя без связи — в списке её нет, в правках — есть.
        let mut gone = task("u1", Some("a"), true);
        gone.deleted = true;
        c.put_tasks(&[gone.clone(), task("u3", None, true)]).unwrap();
        assert_eq!(c.tasks().unwrap().iter().map(|t| t.uid.as_str()).collect::<Vec<_>>(), ["u2", "u3"]);
        // Полная загрузка: серверные заменяют, правки у себя остаются поверх.
        c.replace_tasks(&[task("u1", Some("a"), false), task("u9", Some("z"), false)]).unwrap();
        let mut uids: Vec<String> = c.all_tasks().unwrap().into_iter().map(|t| t.uid).collect();
        uids.sort();
        assert_eq!(uids, ["u1", "u3", "u9"]);
        assert!(c.task("u1").unwrap().unwrap().deleted, "удаление у себя не потеряно");
        assert_eq!(c.task_uid("c").unwrap(), None);
        c.remove_tasks(&["u9".into()]).unwrap();
        assert_eq!(c.task_uid("z").unwrap(), None);
    }

    /// Миграция: кэш формата 5 без таблиц задач открывается без пересоздания — письма на месте, задачи пишутся.
    #[test]
    fn old_cache_gets_task_tables() {
        let dir = tempfile::Builder::new().prefix("postman-cache-").tempdir().unwrap();
        let path = dir.path().join("c.redb");
        {
            let db = Database::create(&path).unwrap();
            let tx = db.begin_write().unwrap();
            tx.open_table(META).unwrap().insert("version", FORMAT_VERSION).unwrap();
            tx.commit().unwrap();
        }
        {
            let c = Cache::open(&path).unwrap();
            c.upsert_emails(&[email("keep", 1, &["in"])]).unwrap();
        }
        // Как в кэше до S6-02: таблиц задач нет.
        {
            let db = Database::create(&path).unwrap();
            let tx = db.begin_write().unwrap();
            tx.delete_table(TASK).unwrap();
            tx.delete_table(TASK_ID).unwrap();
            tx.commit().unwrap();
        }
        let c = Cache::open(&path).unwrap();
        assert_eq!(c.rows_by_ids(&["keep".into()]).unwrap().len(), 1, "письма не потеряны");
        assert!(c.tasks().unwrap().is_empty());
        c.put_tasks(&[task("u1", Some("a"), false)]).unwrap();
        assert_eq!(c.tasks().unwrap().len(), 1);
    }

    #[test]
    fn profile_accounts_live_side_by_side() {
        assert_eq!(profile_of(None), None);
        assert_eq!(profile_of(Some("shared1")), None);
        assert_eq!(profile_of(Some("@2")), Some("@2"));
        assert_eq!(profile_of(Some("@2.shared1")), Some("@2"));
        assert_eq!(in_profile(None, Some("s")).as_deref(), Some("s"));
        assert_eq!(in_profile(Some("@2"), None).as_deref(), Some("@2"));
        assert_eq!(in_profile(Some("@2"), Some("s")).as_deref(), Some("@2.s"));

        let c = Cache::open_in_memory().unwrap();
        let row = |id: &str| AccountRow { id: id.into(), name: id.into(), is_personal: false, read_only: false };
        c.replace_accounts(&[row("s1")]).unwrap();
        c.replace_accounts_of(Some("@2"), &[row("@2"), row("@2.s9")]).unwrap();
        c.set_state(&scoped(Some("@2"), "Email"), "x").unwrap();
        // Основная учётная запись обновляет свои — дополнительной не касается, и наоборот.
        c.replace_accounts(&[row("s2")]).unwrap();
        let ids = |c: &Cache| c.accounts().unwrap().into_iter().map(|a| a.id).collect::<Vec<_>>();
        assert_eq!(ids(&c), ["@2", "@2.s9", "s2"]);
        c.forget_profile("@2").unwrap();
        assert_eq!(ids(&c), ["s2"]);
        assert_eq!(c.state("@2/Email").unwrap(), None);
    }

    #[test]
    fn bodies_blobs_state() {
        let c = Cache::open_in_memory().unwrap();
        c.upsert_emails(&[email("a", 1, &["in"])]).unwrap();
        assert!(c.body("a").unwrap().is_none());
        let b = BodyRow {
            text: Some("hi".into()),
            to: vec![Addr { name: "Иванов, Иван".into(), email: "i@x.ru".into() }],
            message_id: Some("m@x".into()),
            references: vec!["a@x".into(), "b@x".into()],
            attachments: vec![AttachmentRow {
                blob_id: "B1".into(),
                name: "x.png".into(),
                mime: "image/png".into(),
                size: 3,
                cid: Some("c1".into()),
                inline: true,
            }],
            ..Default::default()
        };
        c.upsert_emails(&[email("z", 5, &["in"])]).unwrap();
        assert_eq!(c.missing_bodies(10).unwrap(), ["z", "a"]);
        c.put_body("a", &b).unwrap();
        assert_eq!(c.missing_bodies(10).unwrap(), ["z"]);
        assert_eq!(c.body_ids(0, 10).unwrap(), ["a"]);
        assert_eq!(c.body("a").unwrap(), Some(b));
        assert_eq!(c.attachments("a").unwrap().len(), 1);
        c.put_blob("B1", b"png").unwrap();
        assert_eq!(c.blob("B1").unwrap().as_deref(), Some(&b"png"[..]));
        c.trust("Ivan@X.ru").unwrap();
        assert!(c.is_trusted("ivan@x.ru").unwrap());
        c.set_state("Email", "s1").unwrap();
        assert_eq!(c.state("Email").unwrap().as_deref(), Some("s1"));
        c.delete_emails(&["a".into()]).unwrap();
        assert!(c.body("a").unwrap().is_none());
    }

    #[test]
    fn reopen_and_replace_foreign_file() {
        // Каталог удаляется при выходе из теста, даже при падении (H-01).
        let dir = tempfile::Builder::new().prefix("postman-cache-").tempdir().unwrap();
        let path = dir.path().join("c.redb");
        std::fs::write(&path, b"SQLite format 3\0 not redb").unwrap();
        {
            let c = Cache::open(&path).unwrap();
            c.upsert_emails(&[email("a", 1, &["in"])]).unwrap();
        }
        assert_eq!(Cache::open(&path).unwrap().count("in").unwrap(), 1);
    }
}
