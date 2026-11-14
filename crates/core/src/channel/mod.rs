//! Каналы бесед (ADR 0012): протокол — в канале, всё остальное — в ядре один раз.
//!
//! - **Договор** ([`ChannelPlugin`], [`ChannelSession`], [`Flow`]): канал — вход и сессия, догнать сервер от курсора,
//!   поток событий ([`ChannelEvent`]), действия ([`Action`]), загрузка и выгрузка файлов, перевод формата в общую
//!   модель. Канал **не видит** хранилища, индекса, воркера и секретов: отдаёт значения модели, пишет их ядро.
//! - **Модель** (этот модуль): адреса `postman://` из id сервера ([`Address`]), беседа, сообщение, участник,
//!   вложение; что умеет канал — [`ChannelFeatures`] («Нет у сервера — нет на экране»).
//! - **Хранение** ([`ChannelStore`]): таблицы базы профиля (`postman_cache`, ADR 0012 п. 9) — пачка событий и курсор
//!   одной транзакцией, запись идемпотентна по [`MessageId`], удаление без беседы, вложения по хэшу.
//! - **Поток учётки** ([`run`], [`spawn`]): связь, повтор с паузой, пачки событий, запросы интерфейса ([`Request`] →
//!   [`Update`]).
//! - **Проверки договора** ([`conformance`]): набор, который обязан пройти любой канал; демо-канал (`postman_demo`)
//!   проходит его без сети.
//!
//! - **Беседы для чтения** ([`ConversationSource`]): чаты — из таблиц каналов ([`Chats`]), почта — из таблиц писем на
//!   лету ([`mail::MailSource`]); одна модель для списка, ленты и общих функций.
//! - **Поиск** ([`index`]): сообщения каналов — документами `c<MsgKey>` в общем индексе с почтой, фильтры канала,
//!   беседы и автора.
//! - **Связи** (CH-02в): кросс-ссылки и обратные ссылки ([`xref`]), связанные беседы ([`linked`]), участник канала =
//!   контакт ([`person`]), адрес письма по `Message-ID` (`mail::MailSource::find`); «Переслать в…», «Ответить в…»
//!   между каналами ([`forward`]), экспорт ленты любой беседы ([`export`]), «Медиа беседы» — `crate::thread_media`.
//!
//! Почта — канал `mail` адаптером ([`mail`], ADR 0012 п. 7) и через [`ChannelSession`] не идёт.

pub mod conformance;
pub mod export;
pub mod forward;
pub mod index;
pub mod linked;
pub mod mail;
pub mod notice;
pub mod outbox;
pub mod person;
mod plugin;
mod run;
pub mod smart;
mod source;
mod store;
pub mod xref;

pub use plugin::{
    Answer, ByteSink, ByteSource, ChannelPlugin, ChannelSession, CodeVia, ConnectCtx, Flow, FlowCtx, FlowKind, Incoming, LocalStore, MemorySecrets,
    PluginStore, Prompt, Secrets,
};
pub use run::{BATCH_EVENTS, BATCH_MS, HISTORY_PAGE, Render, Request, RunCtx, Start, Update, run, spawn, to_ir, worker_render};
pub use source::{Chats, ConversationSource, Line, Summary};
pub use store::{Applied, ChannelStore, Entry, StoredConversation};
pub use xref::Links;

use postman_ir::Document;
use serde::{Deserialize, Serialize};

// --- адреса и ключи ---

macro_rules! id {
    ($($(#[$m:meta])* $name:ident),*) => {$(
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(s: impl Into<String>) -> Self {
                Self(s.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    )*};
}

id!(
    /// Канал: `mail`, `telegram`, `matrix`, `demo`.
    ChannelId,
    /// Учётка в канале: адрес почты, id пользователя Telegram, `@user:server` Matrix.
    AccountId,
    /// Беседа на сервере канала.
    ConversationId,
    /// Сообщение на сервере канала, в пределах беседы.
    MessageId,
    /// Человек в канале.
    ParticipantId,
    /// Папка Telegram, пространство Matrix.
    FolderRef
);

/// Канал почты (адаптер, ADR 0012 п. 7): id занят, у других каналов его нет.
pub const MAIL: &str = "mail";

/// Учётка канала: куда идёт запрос интерфейса (`sync::Route::Channel`) и чьё событие (`sync::Event::Channel`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChanAccount {
    pub channel: ChannelId,
    pub account: AccountId,
}

impl ChanAccount {
    pub fn new(channel: impl Into<String>, account: impl Into<String>) -> Self {
        Self { channel: ChannelId(channel.into()), account: AccountId(account.into()) }
    }
}

impl std::fmt::Display for ChanAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.channel, self.account)
    }
}

/// Адрес `postman://<канал>/<учётка>/<беседа>[/<сообщение>]` — из id сервера, не из ключей базы: ссылка открывается на
/// другом компьютере того же человека. Части — в процент-кодировке (RFC 3986 pchar). Адрес не несёт действий: только
/// «что открыть».
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Address {
    pub channel: ChannelId,
    pub account: AccountId,
    pub conversation: ConversationId,
    pub message: Option<MessageId>,
}

/// Схема адреса.
pub const SCHEME: &str = "postman://";

/// Процент-кодировка части пути (RFC 3986 pchar): `unreserved`, `sub-delims`, `:` и `@` — как есть, прочее — `%XX`
/// байтов UTF-8.
pub fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let keep = b.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@".contains(&b);
        if keep {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Обратно к [`encode_segment`]; неверная кодировка или не UTF-8 — `None`.
pub fn decode_segment(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

impl Address {
    pub fn conversation(channel: &ChannelId, account: &AccountId, conversation: &ConversationId) -> Self {
        Self { channel: channel.clone(), account: account.clone(), conversation: conversation.clone(), message: None }
    }

    pub fn message(channel: &ChannelId, account: &AccountId, conversation: &ConversationId, message: &MessageId) -> Self {
        Self { message: Some(message.clone()), ..Self::conversation(channel, account, conversation) }
    }

    /// Учётка канала адреса.
    pub fn chan(&self) -> ChanAccount {
        ChanAccount { channel: self.channel.clone(), account: self.account.clone() }
    }

    /// Адрес беседы этого адреса (без сообщения).
    pub fn of_conversation(&self) -> Self {
        Self { message: None, ..self.clone() }
    }

    /// Разобрать `postman://…`: ровно три или четыре непустые части, без запроса и якоря. Иначе — `None`.
    pub fn parse(url: &str) -> Option<Self> {
        let rest = url.strip_prefix(SCHEME)?;
        if rest.contains(['?', '#']) {
            return None;
        }
        let parts: Vec<String> = rest.split('/').map(decode_segment).collect::<Option<_>>()?;
        if parts.iter().any(String::is_empty) {
            return None;
        }
        let mut it = parts.into_iter();
        let (channel, account, conversation) = (it.next()?, it.next()?, it.next()?);
        let message = it.next().map(MessageId);
        if it.next().is_some() {
            return None;
        }
        Some(Self { channel: ChannelId(channel), account: AccountId(account), conversation: ConversationId(conversation), message })
    }
}

impl std::fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{SCHEME}{}/{}/{}", encode_segment(&self.channel.0), encode_segment(&self.account.0), encode_segment(&self.conversation.0))?;
        if let Some(m) = &self.message {
            write!(f, "/{}", encode_segment(&m.0))?;
        }
        Ok(())
    }
}

/// Начало адресов учётки канала: `postman://<канал>/<учётка>/` — её беседы и сообщения.
pub fn account_prefix(chan: &ChanAccount) -> String {
    format!("{SCHEME}{}/{}/", encode_segment(&chan.channel.0), encode_segment(&chan.account.0))
}

/// Курсор канала: всё до него передано (`pts` Telegram, `next_batch` Matrix) — непрозрачные байты.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Cursor(pub Vec<u8>);

/// Поля канала сверх модели (кнопки ботов, опросы, геометка): ядро хранит, но не понимает.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Extra(pub Vec<u8>);

/// Байты сессии или ключа: `Debug` без значения, память затирается при освобождении. Хранит их ядро — в хранилище
/// секретов ОС ([`Secrets`]), канал отдаёт и получает только их.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Secret(Vec<u8>);

impl Secret {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Secret({} bytes)", self.0.len())
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.0);
    }
}

// --- беседа, сообщение, участник, вложение ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ConversationKind {
    /// Личная беседа.
    #[default]
    Direct,
    Group,
    /// Канал, рассылка: пишут только владельцы.
    Broadcast,
    /// Беседа почты (адаптер `mail`).
    MailThread,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Role {
    Owner,
    Admin,
    #[default]
    Member,
    ReadOnly,
    Guest,
}

impl Role {
    /// Может писать в беседу.
    pub fn can_write(self) -> bool {
        !matches!(self, Role::ReadOnly)
    }
}

/// Счётчики сервера.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Counts {
    pub unread: u32,
    pub mentions: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Conversation {
    pub id: ConversationId,
    pub kind: ConversationKind,
    pub title: String,
    pub avatar: Option<AttachmentSource>,
    /// Тема форума Telegram — беседа внутри группы.
    pub parent: Option<ConversationId>,
    /// Папки Telegram, пространства Matrix.
    pub folders: Vec<FolderRef>,
    pub my_role: Role,
    /// Сколько участников; сами — [`Action::Members`], большие группы — лениво.
    pub members: Option<u32>,
    /// Моё «прочитано» по серверу.
    pub last_read: Option<MessageId>,
    /// Счётчики сервера (`features.server_unread`), иначе считает ядро.
    pub unread: Option<Counts>,
    /// Заглушено на сервере до (Unix, мс; `features.server_mute`).
    pub muted_until: Option<i64>,
    /// Закреплено на сервере (`features.server_pin`).
    pub pinned: bool,
    pub encrypted: bool,
    pub extra: Extra,
}

impl Conversation {
    /// Беседа, о которой сервер ещё не рассказал (сообщение пришло раньше беседы): только id.
    pub fn unknown(id: ConversationId) -> Self {
        Self { title: id.0.clone(), id, kind: ConversationKind::Group, ..Self::default() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServiceKind {
    Created,
    Joined(ParticipantId),
    Left(ParticipantId),
    Renamed(String),
    Pinned(MessageId),
    /// Прочее — текстом канала.
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MessageKind {
    #[default]
    Normal,
    Service(ServiceKind),
    /// Не расшифровалось: придёт ключ — то же сообщение заново (тот же [`MessageId`]).
    Undecryptable,
}

/// Тело: структурный формат канала — сразу IR; чужая разметка (Matrix `formatted_body`) — HTML, его переводит в IR
/// ядро в воркере, как тела писем.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Body {
    Ir(Document),
    Html { html: String, plain: String },
    Plain(String),
}

impl Default for Body {
    fn default() -> Self {
        Body::Plain(String::new())
    }
}

impl Body {
    /// Плоский текст тела (поиск, подсказка, проверки).
    pub fn plain(&self) -> String {
        match self {
            Body::Plain(s) | Body::Html { plain: s, .. } => s.clone(),
            Body::Ir(d) => doc_text(d),
        }
    }
}

/// Плоский текст документа: абзацы — строками.
pub fn doc_text(d: &Document) -> String {
    use postman_ir::{Block, plain_text};
    fn blocks(bs: &[Block], out: &mut Vec<String>) {
        for b in bs {
            match b {
                Block::Paragraph { content, .. } | Block::Heading { content, .. } => out.push(plain_text(content)),
                Block::Quote(inner) => blocks(inner, out),
                Block::List { items, .. } => items.iter().for_each(|i| blocks(i, out)),
                Block::Table(rows) => rows.iter().flatten().for_each(|c| out.push(plain_text(c))),
                Block::Code(code) => out.push(code.clone()),
                Block::Rule | Block::Figure => {}
            }
        }
    }
    let mut out = Vec::new();
    blocks(&d.blocks, &mut out);
    out.join("\n")
}

/// Документ из простого текста: строка — абзац.
pub fn text_doc(text: &str) -> Document {
    use postman_ir::{Align, Block, Inline};
    let blocks = text.lines().map(|l| Block::Paragraph { align: Align::Start, content: vec![Inline::text(l)] }).collect();
    Document { blocks, ..Document::default() }
}

/// Цитата фрагмента: текст и, если есть, сообщение-источник.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quote {
    pub text: String,
    pub source: Option<MessageId>,
}

/// Переслано: от кого и когда.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    pub name: String,
    pub author: Option<ParticipantId>,
    pub sent: Option<i64>,
}

/// Реакция: ключ (эмодзи), сколько, моя ли, кто (если сервер дал).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reaction {
    pub key: String,
    pub count: u32,
    pub mine: bool,
    pub who: Vec<ParticipantId>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub author: ParticipantId,
    /// Unix, мс.
    pub sent: i64,
    pub edited: Option<i64>,
    pub kind: MessageKind,
    pub body: Body,
    pub attachments: Vec<Attachment>,
    pub reply_to: Option<MessageId>,
    /// Цитаты фрагментов.
    pub quotes: Vec<Quote>,
    /// Ветка Matrix (`m.thread`) — корень.
    pub thread: Option<MessageId>,
    pub forwarded: Option<Origin>,
    pub reactions: Vec<Reaction>,
    pub mentions: Vec<ParticipantId>,
    pub mine: bool,
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Participant {
    pub id: ParticipantId,
    pub name: String,
    pub handle: Option<String>,
    pub phone: Option<String>,
    pub emails: Vec<String>,
    pub avatar: Option<AttachmentSource>,
    pub bot: bool,
}

/// Участник беседы и его роль.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub id: ParticipantId,
    pub role: Role,
}

/// Откуда байты вложения: непрозрачно для ядра (file reference Telegram, `mxc://` и ключ шифрования Matrix).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct AttachmentSource(pub Vec<u8>);

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AttachmentKind {
    #[default]
    File,
    Image {
        w: u32,
        h: u32,
    },
    Video {
        w: u32,
        h: u32,
        secs: u32,
    },
    Audio {
        secs: u32,
    },
    Voice {
        secs: u32,
    },
    Sticker,
    Contact,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Attachment {
    pub name: String,
    pub mime: String,
    pub size: Option<u64>,
    pub kind: AttachmentKind,
    pub source: AttachmentSource,
    pub thumb: Option<AttachmentSource>,
}

/// Папка Telegram, пространство Matrix: дерево (CH-04).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelFolder {
    pub id: FolderRef,
    pub name: String,
    pub parent: Option<FolderRef>,
    pub conversations: Vec<ConversationId>,
}

/// Кто прочитал.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reader {
    Me,
    Them(ParticipantId),
}

/// Страница истории или поиска на сервере: сообщения с беседой; `more` — есть ещё (раньше или дальше).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Page {
    pub messages: Vec<(ConversationId, Message)>,
    pub more: bool,
}

/// Файл перед выгрузкой.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMeta {
    pub name: String,
    pub mime: String,
    pub size: u64,
}

/// Выгруженный файл: что отправить в [`Outgoing::attachments`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Uploaded {
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub kind: AttachmentKind,
    pub source: AttachmentSource,
}

// --- возможности ---

/// Срок действия над своим сообщением.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Limit {
    #[default]
    No,
    /// В течение стольких секунд после отправки.
    Within(u32),
    Always,
}

impl Limit {
    /// Можно ли сейчас: сообщению `age_ms` миллисекунд. Срок прошёл — пункта нет.
    pub fn open(self, age_ms: i64) -> bool {
        match self {
            Limit::No => false,
            Limit::Always => true,
            Limit::Within(secs) => age_ms <= i64::from(secs) * 1000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Reactions {
    #[default]
    No,
    /// Только эти ключи.
    Fixed(Vec<String>),
    Any,
}

impl Reactions {
    pub fn allows(&self, key: &str) -> bool {
        match self {
            Reactions::No => false,
            Reactions::Fixed(keys) => keys.iter().any(|k| k == key),
            Reactions::Any => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum E2e {
    #[default]
    No,
    Optional,
    Always,
}

/// Что умеет канал (ADR 0012 п. 5): «Нет у сервера — нет на экране»; запасной путь — только когда получатель увидит
/// то же. Отдельно от почтовых `model::Features`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ChannelFeatures {
    pub edit: Limit,
    pub delete_for_all: Limit,
    pub reactions: Reactions,
    /// Видно «прочитано» собеседником.
    pub read_receipts: bool,
    pub typing: bool,
    pub reply_to: bool,
    /// Ответ с цитатой части сообщения на сервере.
    pub quote_fragment: bool,
    pub mentions: bool,
    /// Пересылка сервером с подписью источника.
    pub forward_native: bool,
    /// Отложенную отправку держит сервер.
    pub scheduled_send: bool,
    pub server_drafts: bool,
    pub server_mute: bool,
    pub server_pin: bool,
    pub server_search: bool,
    /// Счётчики непрочитанного даёт сервер.
    pub server_unread: bool,
    /// Папки, пространства.
    pub folders: bool,
    /// Ветки внутри беседы.
    pub threads: bool,
    pub e2e: E2e,
    /// Байт.
    pub max_attachment: Option<u64>,
    /// Знаков.
    pub max_text: Option<u32>,
}

impl ChannelFeatures {
    /// Действие, которое канал обязан уметь при этих возможностях; `false` — только [`ChannelError::Unsupported`].
    pub fn allows(&self, action: &Action) -> bool {
        match action {
            Action::Send(o) => o.at.is_none() || self.scheduled_send,
            Action::Edit { .. } => self.edit != Limit::No,
            Action::Delete { for_all, .. } => !for_all || self.delete_for_all != Limit::No,
            Action::React { key, .. } => self.reactions.allows(key),
            Action::Typing { .. } => self.typing,
            Action::Forward { .. } => self.forward_native,
            Action::Mute { .. } => self.server_mute,
            Action::Pin { .. } => self.server_pin,
            Action::SaveDraft { .. } => self.server_drafts,
            Action::MarkRead { .. } | Action::Start { .. } | Action::Members { .. } | Action::Resolve { .. } => true,
        }
    }
}

// --- события и действия ---

/// Состояние учётки канала.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChannelState {
    Connecting,
    Online,
    /// Связи нет: повтор через `retry_in` секунд.
    Offline {
        retry_in: u64,
    },
    Problem(ChannelError),
}

/// От канала к ядру (ADR 0012 п. 6).
#[derive(Debug, Clone, PartialEq)]
pub enum ChannelEvent {
    /// Новые и изменённые.
    Conversations(Vec<Conversation>),
    /// Вышел, удалили, исключили.
    ConversationGone(ConversationId),
    /// `live` — пришло сейчас (для уведомлений), иначе — история.
    Messages {
        conversation: ConversationId,
        messages: Vec<Message>,
        live: bool,
    },
    /// Без беседы — беседу находит ядро по id сообщения в учётке.
    Deleted {
        conversation: Option<ConversationId>,
        ids: Vec<MessageId>,
        for_all: bool,
    },
    Reactions {
        conversation: ConversationId,
        message: MessageId,
        reactions: Vec<Reaction>,
    },
    Read {
        conversation: ConversationId,
        who: Reader,
        up_to: MessageId,
    },
    /// Только в памяти потока, истекает через [`TYPING_SECS`].
    Typing {
        conversation: ConversationId,
        who: ParticipantId,
        on: bool,
    },
    Members {
        conversation: ConversationId,
        changed: Vec<Member>,
        left: Vec<ParticipantId>,
    },
    People(Vec<Participant>),
    Folders(Vec<ChannelFolder>),
    /// Сервер не отдал всё: ядро дозапросит историю (беседы или, без неё, — догонит заново).
    Gap {
        conversation: Option<ConversationId>,
    },
    /// Всё до этого — передано.
    Cursor(Cursor),
    /// Новый секрет сессии (Telegram сменил ДЦ): ядро кладёт его в хранилище ОС.
    Secret(Secret),
    Features(ChannelFeatures),
    State(ChannelState),
}

/// «Печатает» и «в сети» истекают через столько секунд.
pub const TYPING_SECS: u64 = 6;

/// Исходящее сообщение.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Outgoing {
    pub conversation: ConversationId,
    /// Номер отправки ядра (Telegram `random_id`, Matrix `txn_id`): повтор после сбоя не даёт дубля.
    pub client_id: u64,
    pub body: Document,
    pub attachments: Vec<Uploaded>,
    pub reply_to: Option<MessageId>,
    pub quotes: Vec<Quote>,
    pub mentions: Vec<ParticipantId>,
    /// Только `scheduled_send`.
    pub at: Option<i64>,
    pub silent: bool,
}

/// От ядра к каналу ([`ChannelSession::act`]).
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Send(Outgoing),
    Edit {
        conversation: ConversationId,
        message: MessageId,
        body: Document,
        mentions: Vec<ParticipantId>,
    },
    Delete {
        conversation: ConversationId,
        ids: Vec<MessageId>,
        for_all: bool,
    },
    React {
        conversation: ConversationId,
        message: MessageId,
        key: String,
        on: bool,
    },
    MarkRead {
        conversation: ConversationId,
        up_to: MessageId,
    },
    Typing {
        conversation: ConversationId,
        on: bool,
    },
    /// Только `forward_native`.
    Forward {
        from: ConversationId,
        ids: Vec<MessageId>,
        to: ConversationId,
    },
    /// Только `server_mute`.
    Mute {
        conversation: ConversationId,
        until: Option<i64>,
    },
    /// Только `server_pin`.
    Pin {
        conversation: ConversationId,
        on: bool,
    },
    /// Только `server_drafts`.
    SaveDraft {
        conversation: ConversationId,
        draft: Option<Document>,
    },
    /// Новая личная беседа или группа.
    Start {
        with: Vec<ParticipantId>,
    },
    Members {
        conversation: ConversationId,
    },
    /// `t.me/…`, `matrix.to/…`.
    Resolve {
        link: String,
    },
}

/// Итог действия.
#[derive(Debug, Clone, PartialEq)]
pub enum Done {
    Ok,
    Sent { client_id: u64, id: MessageId, at: i64 },
    Conversation(ConversationId),
    Members(Vec<Member>),
    Address(Address),
}

/// Ошибка канала: «Проблема → причина → исправление» у интерфейса; журнал — по-английски, без текста сообщений и
/// телефонов.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum ChannelError {
    #[error("network: {0}")]
    Network(String),
    #[error("signed out: {0}")]
    SignedOut(String),
    /// Сервер просит подождать (Telegram `FLOOD_WAIT`, Matrix `M_LIMIT_EXCEEDED`).
    #[error("server asks to wait {secs} s")]
    Wait { secs: u32 },
    #[error("denied: {0}")]
    Denied(String),
    #[error("not found")]
    NotFound,
    #[error("too large: limit {max} bytes")]
    TooLarge { max: u64 },
    #[error("not supported by the channel")]
    Unsupported,
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("storage: {0}")]
    Storage(String),
}

impl From<postman_cache::Error> for ChannelError {
    fn from(e: postman_cache::Error) -> Self {
        ChannelError::Storage(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Адрес — туда и обратно, части с `/`, `@`, пробелами и кириллицей; чужое и с действиями — не адрес.
    #[test]
    fn address_round_trip() {
        let a = Address::message(
            &ChannelId::new("matrix"),
            &AccountId::new("@me:corp.example"),
            &ConversationId::new("!room/1 x"),
            &MessageId::new("$ev"),
        );
        let s = a.to_string();
        assert_eq!(s, "postman://matrix/@me:corp.example/!room%2F1%20x/$ev");
        assert_eq!(Address::parse(&s), Some(a));
        let c = Address::conversation(&ChannelId::new("demo"), &AccountId::new("анна@example.test"), &ConversationId::new("c"));
        assert_eq!(Address::parse(&c.to_string()), Some(c));
        for bad in ["postman://a/b", "postman://a/b/c/d/e", "postman://a//c", "postman://a/b/c?do=delete", "https://a/b/c", "postman://a/b/%ZZ"] {
            assert_eq!(Address::parse(bad), None, "{bad}");
        }
    }

    /// Сроки правки и возможности действий.
    #[test]
    fn limits_and_features() {
        assert!(!Limit::No.open(0));
        assert!(Limit::Always.open(i64::MAX));
        assert!(Limit::Within(60).open(60_000) && !Limit::Within(60).open(60_001));
        let f = ChannelFeatures { reactions: Reactions::Fixed(vec!["👍".into()]), ..ChannelFeatures::default() };
        let react = |key: &str| Action::React { conversation: ConversationId::default(), message: MessageId::default(), key: key.into(), on: true };
        assert!(f.allows(&react("👍")) && !f.allows(&react("🔥")));
        assert!(!f.allows(&Action::Typing { conversation: ConversationId::default(), on: true }));
        assert!(f.allows(&Action::Delete { conversation: ConversationId::default(), ids: vec![], for_all: false }));
        assert!(!f.allows(&Action::Delete { conversation: ConversationId::default(), ids: vec![], for_all: true }));
    }

    #[test]
    fn secret_debug_hides_value() {
        assert_eq!(format!("{:?}", Secret::new(b"key".to_vec())), "Secret(3 bytes)");
    }
}
