# ADR 0012. Каналы бесед: протокол — в канале, всё остальное — в ядре один раз

Статус: **принято** (владелец, 2026-11-14; класс B, карточка `docs/tasks/CH-01.md`). Основание — решения владельца
2026-11-14: `docs/decisions/2026-11-14-channels.md` (каналы, раздел «Вид» — гибрид), `2026-11-14-all-in-db.md` (всё
в базе профиля), `2026-11-14-messenger-comforts.md` (фрагменты, быстрый ответ, заглушить, закрепить, отмена
отправки). Продолжает ADR 0004 (ядро функционально, протоколы — плагины) и ADR 0006 (единое хранилище, формат 7).

## Контекст

- Postman — почтовый клиент; беседа почты (`thread`, как в Outlook) уже стала лентой с быстрым ответом,
  @упоминаниями, фрагментами, «заглушить» и «закрепить», «Медиа беседы» (MED-01). Тот же интерфейс нужен чатам.
- Почтовые пути живут по ADR 0004: `MailPlugin::spawn` — свой поток учётки с tokio за договором `sync::Command` →
  `sync::Event`; плагин сам пишет кэш. Для чатов так нельзя: каждый канал заново написал бы хранение, поиск,
  счётчики, уведомления, очередь отправки — и делал бы это по-своему.
- Владелец: «общие механизмы, хранение, поиск, кросс-ссылки — всё, что возможно, сразу в договор. Канал реализует
  только протокол». Почта — первый канал, в ней ничего не упрощается ради чатов.
- Чаты устроены не как почта: поток событий в реальном времени (Telegram updates, Matrix `/sync`), правки и удаление
  у всех, реакции, «прочитано», «печатает», сквозное шифрование. Договор проверяется на двух настоящих протоколах
  (раздел «Проверка договора»), а не на почте.

## Решение

### 1. Граница: канал не видит хранилища

`ChannelPlugin` — протокол и только он: вход и сессия, догнать сервер от курсора, поток событий, действия (отправка,
правка, удаление, реакции, отметки), загрузка и выгрузка файлов, перевод формата в общую модель и обратно. Канал
**не открывает** `Cache`, поисковый индекс, воркер и хранилище секретов: он отдаёт значения модели (`ChannelEvent`),
пишет их ядро. Поэтому хранение, дедуп, поиск, счётчики, уведомления, очередь отправки, кросс-ссылки и экспорт
написаны один раз и одинаковы для всех каналов.

Отличие от `MailPlugin` осознанное: почтовый путь сам пишет кэш, потому что почтовая модель старше договора и ADR 0006
только что её перестроил. Почта не переписывается: она подключается адаптером (п. 7).

### 2. Поток учётки канала

Как у почты: одна учётка канала — свой поток (`sync::spawn_thread`), рантайм tokio на один поток и `LocalSet`
(плагин может `spawn_local` свой цикл событий). Ядро ведёт цикл `channel::run`:

1. секрет сессии — из хранилища ОС (п. 13), `connect` → `Box<dyn ChannelSession>`;
2. `catch_up(cursor)` — догнать сервер от сохранённого курсора (у Telegram — `getDifference`, у Matrix — `/sync` с
   `since`), события идут в `incoming`;
3. дальше `select!` по трём входам: запросы интерфейса (`sync::Command::Channel`), события канала (`incoming`),
   таймеры ядра (окно «Отменить», отложенная отправка, повтор без связи, сброс пачки событий);
4. связь пропала — `ChannelState::Offline`, повтор с паузой `link::delay` (5…60 с, как у почты), затем снова
   `catch_up`; после сна ОС — сразу `catch_up`.

Методы сессии — `&self`: клиенты протоколов разделяемые (`grammers::Client`, `matrix_sdk::Client` — `Clone`), ядро
держит несколько вызовов сразу (большая загрузка не задерживает отправку). Футуры — `BoxFuture` из
`postman_core::backend` (без `Send`, поток однопоточный): трейты dyn-совместимы, реестр собирается из разнородных
плагинов.

**Реальное время против опроса.** Чаты — поток событий: ядро копит их и пишет пачкой — одна транзакция redb на
≤ 100 мс или ≤ 500 событий, затем одно `Event::Channel(Update::Changed)` интерфейсу (поток сообщений большой группы
не превращается в тысячу перерисовок). «Печатает» и «в сети» — только в памяти потока, с истечением через 6 с, в базу
не пишутся. Курсор сохраняется **той же транзакцией**, что и события до него: сбой между ними повторит события, а
запись по `MessageId` идемпотентна. Почта — как сейчас (push JMAP, IDLE IMAP, опрос EWS); адаптер переводит
`Event::Status`/`Event::Connection` в общее состояние канала.

### 3. Договор на Rust (`postman_core::channel`)

```rust
use crate::backend::BoxFuture;
use postman_ir::Document;

/// Канал по адресу учётки и входу. Регистрируется в `backend::Registry::with_channel`.
pub trait ChannelPlugin: Send + Sync {
    /// `telegram`, `matrix`, `demo` — часть адреса `postman://` и ключа политики; `mail` занят адаптером почты.
    fn id(&self) -> &'static str;
    /// Что канал может уметь вообще — для «Настройки → Каналы» до входа. После входа — `ChannelSession::features`.
    fn features(&self) -> ChannelFeatures;
    /// Ссылка канала (`https://t.me/c/…`, `https://matrix.to/#/…`) → адрес Postman: открыть у себя.
    fn parse_link(&self, _url: &str) -> Option<Address> {
        None
    }
    /// Вход: шаги с вопросами пользователю; итог — `Prompt::Done` с секретом сессии (его хранит ядро).
    fn sign_in<'a>(&'a self, ctx: FlowCtx) -> BoxFuture<'a, Result<Box<dyn Flow + 'a>, ChannelError>>;
    /// Подключиться с сохранённой сессией; события — в `ctx.incoming`.
    fn connect<'a>(&'a self, ctx: ConnectCtx) -> BoxFuture<'a, Result<Box<dyn ChannelSession>, ChannelError>>;
}

/// Что ядро даёт каналу при подключении. Всё владеющее: сессия живёт дольше вызова `connect`.
pub struct ConnectCtx {
    pub account: AccountId,
    /// Байты сессии из хранилища секретов ОС (ключ авторизации Telegram, токен и устройство Matrix).
    pub secret: Secret,
    /// Куда канал шлёт события (`tokio::sync::mpsc::UnboundedSender<ChannelEvent>`).
    pub incoming: Incoming,
    /// Своё постоянное состояние канала — в таблице `local` профиля, ключи `chan/<канал>/<учётка>/…` (п. 9).
    pub store: std::rc::Rc<dyn PluginStore>,
}

pub struct FlowCtx {
    pub store: std::rc::Rc<dyn PluginStore>,
}

/// Подключённая учётка канала.
pub trait ChannelSession {
    fn me(&self) -> Participant;
    fn features(&self) -> ChannelFeatures;
    /// Догнать сервер от курсора (`None` — первый вход): беседы и их последние сообщения — событиями, в конце —
    /// `ChannelEvent::Cursor`. Дальше события идут сами, пока жива связь.
    fn catch_up<'a>(&'a self, cursor: Option<Cursor>) -> BoxFuture<'a, Result<(), ChannelError>>;
    /// История беседы до сообщения `before` (`None` — с конца): прокрутка ленты вверх, «Медиа беседы», поиск.
    fn history<'a>(&'a self, conv: &'a ConversationId, before: Option<&'a MessageId>, limit: u32)
        -> BoxFuture<'a, Result<Page, ChannelError>>;
    /// Поиск на сервере — дополнение к своему индексу, когда беседа загружена не вся (`features.server_search`).
    fn search<'a>(&'a self, query: &'a str, conv: Option<&'a ConversationId>, limit: u32)
        -> BoxFuture<'a, Result<Page, ChannelError>>;
    /// Действие пользователя (отправка, правка, удаление, реакция, отметки…).
    fn act<'a>(&'a self, action: Action) -> BoxFuture<'a, Result<Done, ChannelError>>;
    /// Байты вложения — в `out` кусками (ядро считает SHA-256 и кладёт в хранилище содержимого).
    fn download<'a>(&'a self, src: &'a AttachmentSource, out: &'a mut dyn ByteSink) -> BoxFuture<'a, Result<(), ChannelError>>;
    /// Выгрузить файл перед отправкой; байты ядро читает из хранилища — с источника заново не скачиваются.
    fn upload<'a>(&'a self, meta: &'a FileMeta, src: &'a mut dyn ByteSource) -> BoxFuture<'a, Result<Uploaded, ChannelError>>;
    /// Особый поток вопросов канала после входа: проверка устройства, ключ резервной копии (Matrix).
    fn flow<'a>(&'a self, kind: FlowKind) -> BoxFuture<'a, Result<Box<dyn Flow + 'a>, ChannelError>>;
    fn sign_out<'a>(&'a self) -> BoxFuture<'a, Result<(), ChannelError>>;
}

/// Шаги входа и проверок: вопрос → ответ → следующий вопрос. Тексты вопросов — у интерфейса (Fluent).
pub trait Flow {
    /// `None` — начать; иначе ответ на прошлый вопрос.
    fn step<'a>(&'a mut self, answer: Option<Answer>) -> BoxFuture<'a, Result<Prompt, ChannelError>>;
}

pub enum Prompt {
    Server,                                          // адрес сервера (Matrix)
    Phone,                                           // номер телефона (Telegram)
    Login,                                           // имя и пароль (Matrix)
    Code { via: CodeVia, length: Option<u8> },       // код из приложения, SMS, звонка, почты
    Password { hint: Option<String> },               // пароль двухэтапной проверки
    Qr { url: String, expires: i64 },                // вход с другого устройства
    Browser { url: String },                         // SSO (Matrix): открыть в браузере
    Compare { emoji: Vec<(String, String)> },        // сверка устройств (SAS), символ и подпись
    RecoveryKey,                                     // ключ резервной копии ключей
    Done { account: AccountId, name: String, secret: Secret },
    Finished,
}
pub enum Answer { Text(String), Secret(Secret), Confirm(bool), Resend }

/// Состояние канала в базе профиля (п. 9): курсор Matrix, хранилище ключей сквозного шифрования, состояние
/// обновлений Telegram сверх курсора. Синхронно: redb в потоке учётки.
pub trait PluginStore {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, ChannelError>;
    fn put(&self, key: &str, value: &[u8]) -> Result<(), ChannelError>;
    fn remove_prefix(&self, prefix: &str) -> Result<(), ChannelError>;
}

pub trait ByteSink { fn write(&mut self, chunk: &[u8]) -> Result<(), ChannelError>; }
pub trait ByteSource { fn read(&mut self, buf: &mut [u8]) -> Result<usize, ChannelError>; fn len(&self) -> u64; }
```

`Secret` — байты с `Debug` без значения; пароль двухэтапной проверки и коды не сохраняются нигде. Канал не пишет в
хранилище ОС сам: отдаёт секрет `Prompt::Done` или событием `ChannelEvent::Secret` (Telegram сменил ключ при переезде
на другой ДЦ), ядро кладёт его в хранилище ОС тем же путём, что пароли учёток.

### 4. Модель

Адреса и ключи:

```rust
pub struct ChannelId(pub String);        // mail, telegram, matrix, demo
pub struct AccountId(pub String);        // адрес почты учётки; id пользователя Telegram; @user:server Matrix
pub struct ConversationId(pub String);   // id беседы на сервере канала
pub struct MessageId(pub String);        // id сообщения на сервере канала, в пределах беседы
pub struct ParticipantId(pub String);    // id человека в канале

/// `postman://<канал>/<учётка>/<беседа>[/<сообщение>]` — части в процент-кодировке (RFC 3986 pchar).
pub struct Address { pub channel: ChannelId, pub account: AccountId, pub conversation: ConversationId, pub message: Option<MessageId> }
```

- **Адрес — из id сервера, не из ключей базы.** Ссылка на сообщение лежит в задаче, заметке, встрече на сервере и
  должна открываться на другом компьютере того же человека. Ключи базы (`ConvKey`, `MsgKey`, `LetterId`) — только
  внутри профиля.
- **Адрес не несёт действий**: только «что открыть». Поэтому ссылка `postman://` из чужого письма безопасна — она
  может только открыть своё, и то, что есть у пользователя.
- Где живёт: ссылки в теле (`Inline::Link`), «Открыть», закладки, командная строка (`postman.exe postman://…`;
  регистрация схемы в ОС — отдельной карточкой платформ, ADR 0011). Наружу (в письмо, в Telegram) уходит ссылка
  канала, если она есть: `mid:` (RFC 2392) у почты, `https://t.me/c/…` у Telegram, `https://matrix.to/#/…` у Matrix.

Беседа, сообщение, участник, вложение:

```rust
pub struct Conversation {
    pub id: ConversationId,
    pub kind: ConversationKind,              // Direct | Group | Broadcast (канал, рассылка) | MailThread
    pub title: String,
    pub avatar: Option<AttachmentSource>,
    pub parent: Option<ConversationId>,      // тема форума Telegram — беседа внутри группы
    pub folders: Vec<FolderRef>,             // папки Telegram, пространства Matrix — дерево (CH-04)
    pub my_role: Role,                       // Owner | Admin | Member | ReadOnly | Guest
    pub members: Option<u32>,                // сколько участников; сами — `Action::Members`, большие группы — лениво
    pub last_read: Option<MessageId>,        // моё «прочитано» по серверу
    pub unread: Option<Counts>,              // счётчики сервера (Telegram, Matrix), иначе считает ядро
    pub muted_until: Option<i64>,            // заглушено на сервере (`features.server_mute`)
    pub pinned: bool,                        // закреплено на сервере (`features.server_pin`)
    pub encrypted: bool,
    pub extra: Extra,                        // поля канала сверх модели — непрозрачно, формат и версия канала
}

pub struct Message {
    pub id: MessageId,
    pub author: ParticipantId,
    pub sent: i64,                           // Unix, мс
    pub edited: Option<i64>,
    pub kind: MessageKind,                   // Normal | Service(ServiceKind) | Undecryptable
    pub body: Body,                          // Ir(Document) | Html { html, plain } | Plain(String)
    pub attachments: Vec<Attachment>,
    pub reply_to: Option<MessageId>,
    pub quotes: Vec<Quote>,                  // цитаты фрагментов: текст и, если есть, сообщение-источник
    pub thread: Option<MessageId>,           // ветка Matrix (`m.thread`) — корень
    pub forwarded: Option<Origin>,           // переслано: от кого и когда
    pub reactions: Vec<Reaction>,            // ключ (эмодзи), сколько, мои, кто (если сервер дал)
    pub mentions: Vec<ParticipantId>,
    pub mine: bool,
    pub extra: Extra,
}

pub struct Participant { pub id: ParticipantId, pub name: String, pub handle: Option<String>, pub phone: Option<String>,
                         pub emails: Vec<String>, pub avatar: Option<AttachmentSource>, pub bot: bool }

pub struct Attachment { pub name: String, pub mime: String, pub size: Option<u64>, pub kind: AttachmentKind,
                        pub source: AttachmentSource, pub thumb: Option<AttachmentSource> }
// AttachmentKind: File | Image { w, h } | Video { w, h, secs } | Audio { secs } | Voice { secs } | Sticker | Contact
// AttachmentSource: непрозрачные байты канала (file reference Telegram, mxc:// и ключ шифрования Matrix).
```

- **Тело — в IR** (`crates/ir`, общее представление форматированного текста). Канал отдаёт `Body::Ir`, если формат
  структурный (сущности Telegram — смещения и типы, разбирать нечего), и `Body::Html`/`Body::Plain`, если это чужая
  разметка (Matrix `formatted_body`): HTML **ядро** переводит в IR в воркере (`from_html`), как тела писем, — чужая
  разметка в процессе интерфейса не разбирается. Обратно: ядро отдаёт `Document`, канал переводит в свой формат
  (Telegram — сущности; Matrix — `markdown_to_html` и `to_markdown` из `crates/ir`).
- **Дополнения IR** (CH-02, добавочно — старый IR читается как был): `Inline::Mention { id, text }` последним
  вариантом; `Marks::spoiler` с `serde(default)`. Своих эмодзи Telegram нет — их текстовая замена.
- **Правки.** Канал отдаёт сообщение заново с `edited`; прежнюю версию ядро кладёт в историю правок (`msg_edit`) —
  история есть, даже если сервер её не хранит (Telegram).
- **`extra`** — довесок канала, который ядро хранит, но не понимает (кнопки ботов, опросы, геометка): канал сам
  читает его, когда рисует особое или отвечает серверу.

**Участник ↔ контакт.** Человек один во всех каналах: у карточки контакта, кроме `emails`, `phones`, `mobile`,
появляются `services: Vec<String>` — адреса в каналах ссылками (`https://t.me/<имя>`, `matrix:u/<user>:<server>`),
в JSContact — `onlineServices` (RFC 9553), в vCard — `IMPP`, у Exchange — `ImAddresses`. Связь находит ядро:
телефон участника Telegram ↔ `mobile`/`phones` контакта, почта Matrix (3PID) ↔ `emails`, адрес в `services`; иначе —
«Связать с контактом» вручную. Связи — таблица `person_link`; где хранить ручную связь — вопрос владельцу №2.
Отсюда — умная папка «Человек во всех каналах» и `от:` в поиске по одному человеку.

### 5. Возможности: `ChannelFeatures` и «Нет у сервера — нет на экране»

```rust
pub struct ChannelFeatures {
    pub edit: Limit,                 // No | Within(секунды) | Always
    pub delete_for_all: Limit,
    pub reactions: Reactions,        // No | Fixed(Vec<String>) | Any
    pub read_receipts: bool,         // видно «прочитано» собеседником
    pub typing: bool,
    pub reply_to: bool,
    pub quote_fragment: bool,        // ответ с цитатой части сообщения на сервере
    pub mentions: bool,
    pub forward_native: bool,        // пересылка сервером с подписью источника
    pub scheduled_send: bool,        // отложенная отправка держит сервер
    pub server_drafts: bool,
    pub server_mute: bool,
    pub server_pin: bool,
    pub server_search: bool,
    pub server_unread: bool,         // счётчики непрочитанного даёт сервер
    pub folders: bool,               // папки, пространства
    pub threads: bool,               // ветки внутри беседы
    pub e2e: E2e,                    // No | Optional | Always
    pub max_attachment: Option<u64>, // байт
    pub max_text: Option<u32>,       // знаков
}
```

Отдельно от `model::Features` (почта; 16 бит уже заняты): у почты — сервер и области, у канала — умения беседы.
Правило:

- **Нет у сервера — нет на экране**: реакции, правка, «удалить у всех», «печатает», «прочитано» — только где канал
  умеет; срок правки (`Within`) — пункт пропадает, когда срок прошёл.
- **Запасной путь — только когда получатель увидит то же.** Цитата фрагмента без `quote_fragment` — блок цитаты в
  теле (как в почте); упоминание без `mentions` — «@Имя» текстом; отложенная отправка без `scheduled_send` —
  держит ядро (п. 11), как держат IMAP и Exchange; заглушить и закрепить без `server_*` — в базе профиля. Чего так
  не повторить (реакция, правка у собеседника) — того нет.
- Возможности пересылаются событием `ChannelEvent::Features` (у Matrix зависят от сервера и комнаты); у почты их
  даёт адаптер по `model::Features` учётки.

### 6. События и команды

От канала к ядру:

```rust
pub enum ChannelEvent {
    Conversations(Vec<Conversation>),                       // новые и изменённые
    ConversationGone(ConversationId),                       // вышел, удалили, исключили
    Messages { conversation: ConversationId, messages: Vec<Message>, live: bool },  // live — для уведомлений
    Deleted { conversation: Option<ConversationId>, ids: Vec<MessageId>, for_all: bool },
    Reactions { conversation: ConversationId, message: MessageId, reactions: Vec<Reaction> },
    Read { conversation: ConversationId, who: Reader, up_to: MessageId },           // Reader: Me | Them(ParticipantId)
    Typing { conversation: ConversationId, who: ParticipantId, on: bool },
    Members { conversation: ConversationId, changed: Vec<Member>, left: Vec<ParticipantId> },
    People(Vec<Participant>),
    Folders(Vec<ChannelFolder>),
    Gap { conversation: Option<ConversationId> },            // сервер не отдал всё: ядро дозапросит историю
    Cursor(Cursor),                                          // всё до этого — передано
    Secret(Secret),
    Features(ChannelFeatures),
    State(ChannelState),                                     // Connecting | Online | Offline | Problem(ChannelError)
}
```

`Deleted` без беседы — у Telegram в личных и группах номер сообщения уникален на учётку, а событие удаления беседу не
называет: беседу находит ядро по индексу `msg_by_id`. Это не подгонка под почту — это требование, найденное на
настоящем протоколе.

От ядра к каналу — `ChannelSession::act`:

```rust
pub enum Action {
    Send(Outgoing),
    Edit { conversation: ConversationId, message: MessageId, body: Document, mentions: Vec<ParticipantId> },
    Delete { conversation: ConversationId, ids: Vec<MessageId>, for_all: bool },
    React { conversation: ConversationId, message: MessageId, key: String, on: bool },
    MarkRead { conversation: ConversationId, up_to: MessageId },
    Typing { conversation: ConversationId, on: bool },
    Forward { from: ConversationId, ids: Vec<MessageId>, to: ConversationId },     // только forward_native
    Mute { conversation: ConversationId, until: Option<i64> },                     // только server_mute
    Pin { conversation: ConversationId, on: bool },                                // только server_pin
    SaveDraft { conversation: ConversationId, draft: Option<Document> },          // только server_drafts
    Start { with: Vec<ParticipantId> },                                            // новая личная беседа или группа
    Members { conversation: ConversationId },
    Resolve { link: String },                                                      // t.me/…, matrix.to/…
}

pub struct Outgoing {
    pub conversation: ConversationId,
    /// Номер отправки ядра: Telegram `random_id`, Matrix `txn_id` — повтор после сбоя не даст дубля.
    pub client_id: u64,
    pub body: Document,
    pub attachments: Vec<Uploaded>,
    pub reply_to: Option<MessageId>,
    pub quotes: Vec<Quote>,
    pub mentions: Vec<ParticipantId>,
    pub at: Option<i64>,                 // только scheduled_send
    pub silent: bool,
}

pub enum Done { Ok, Sent { client_id: u64, id: MessageId, at: i64 }, Conversation(ConversationId), Members(Vec<Member>), Address(Address) }

pub enum ChannelError {
    Network(String), SignedOut(String), Wait { secs: u32 }, Denied(String), NotFound,
    TooLarge { max: u64 }, Unsupported, Protocol(String), Storage(String),
}
```

С интерфейсом ядро говорит прежним договором: `sync::Command::Channel { account: ChanAccount, request:
channel::Request }` и `sync::Event::Channel(channel::Update)`; маршрут — `Route::Channel(ChanAccount)` рядом с
`Route::Core` и `Route::Sync` (`Command::routes_by`). `Request` — то, что видит пользователь (открыть беседу,
отправить, ответить на фрагменты, переслать в…, заглушить…); `Update` — `Changed { conversations }`, `Arrived` (для
уведомлений), `SignIn(Prompt)`, `State`, `Sent`/`SendFailed { client_id }`.

### 7. Почта — канал `mail` адаптером

Почта **не** идёт через `ChannelSession` и не копируется в таблицы чатов: у неё свой поток (`MailPlugin`), свои
таблицы (ADR 0006) и своё поведение. Адаптер `postman_core::channel::mail` — две вещи:

- **чтение** — реализация `ConversationSource` над таблицами писем (`letter`, `by_thread`, `body`): беседы и
  сообщения общей модели строятся на лету, без записи;
- **действия** — перевод `channel::Request` в существующие `sync::Command`; события почты (`EmailsChanged`,
  `ThreadReady`, `BodiesStored`, `Status`, `Connection`) — в `channel::Update`.

| Общая модель | Почта |
|---|---|
| `ChannelId` | `mail` |
| `AccountId` | адрес учётки (основной, дополнительной, общего ящика) строчными — переносим между компьютерами |
| `Conversation` | беседа `EmailRow::thread_id`, `kind = MailThread`; название — тема; участники — «От», «Кому», «Копия» писем |
| `Message` | письмо (`LetterId`): автор — «От»; время — `received_at`; тело — IR тела (`body`), новый текст — `Document::new_text` |
| `MessageId` в адресе | `Message-ID` без `<>` — разрешается по индексу `by_message_id`, нет в базе — поиск на сервере по заголовку (JMAP `header:Message-ID`, IMAP `SEARCH HEADER`, EWS `InternetMessageId`); нет `Message-ID` — `~<LetterId>`, только этот компьютер |
| `ConversationId` в адресе | `thread_id`; у ссылки на сообщение — подсказка: не совпал — сообщение открывается по `Message-ID` |
| `Attachment` | вложения письма; байты — части содержимого по хэшу (те же записи `part`) |
| `reply_to`, `quotes` | `In-Reply-To`; фрагменты MSGR-01 |
| `mentions` | `filters::Me::mentioned` по тексту (как сейчас) |
| «прочитано» мной | `$seen` (`Command::SetKeywords`) |
| заглушить, закрепить | `$muted` (CMF-01), `pm.pin` (CMF-02) — без изменений |
| отправка, отмена, «позже» | `Command::Send` с `Release` (CMF-03, S5-06) — без изменений |
| `ChannelFeatures` | правка, удаление у всех, реакции, «печатает», «прочитано» — нет; ответ, цитаты, упоминания — да; `scheduled_send` — по `Release` пути; `e2e` — S/MIME `Optional`; `max_attachment` — предел сервера |

Почтовая модель, ключи, формат кэша, `MailPlugin` и тесты почты не меняются. «Чаты поверх почты» (Postman#566) —
этот же адаптер с видом ленты, а не отдельный канал.

### 8. Таблица «механизм — ядро или канал»

| Механизм | Ядро (один раз) | Канал (протокол) |
|---|---|---|
| Вход, сессия | шаги `Flow` в интерфейсе, секрет — в хранилище ОС, «Выйти» | протокол входа, `Prompt`, байты сессии |
| Синхронизация | курсор в базе той же транзакцией, повтор без связи, `Gap` → история | `catch_up`, поток событий, `history` |
| Хранение | таблицы redb, дедуп вложений по SHA-256, история правок, офлайн-чтение | — |
| Перевод формата | HTML → IR в воркере; IR → Markdown/HTML для канала (`crates/ir`) | свой формат ↔ IR или HTML |
| Поиск | один индекс tantivy, фильтры канал/беседа/автор, словоформы, опечатки, раскладка | `search` на сервере — дополнением |
| Список бесед, умные папки | все каналы, сортировка, закреплённые сверху | папки и пространства — событием `Folders` |
| Непрочитанное, счётчики | по `last_read` и своим сообщениям или по счётчикам сервера | `Read`, `unread` |
| Уведомления | тосты и трей по `live`, заглушённое молчит, упоминания меня | правила сервера (`muted_until`) |
| Заглушить, закрепить | в базе или на сервере по `server_*` | `Mute`, `Pin` |
| Черновики | `local["draft/<адрес беседы>"]`; на сервер — по `server_drafts` | `SaveDraft` |
| Очередь отправки, «Отменить», «позже» | `chan_outbox`, окно CMF-03, повтор без дублей по `client_id` | `Send`, `upload` |
| Быстрый ответ, фрагменты, @упоминания | поле, реплики, подсказка из участников | перевод цитат и упоминаний в свой формат |
| Реакции, правка, удаление | показ по `ChannelFeatures`, история правок | `React`, `Edit`, `Delete` |
| Кросс-ссылки, обратные ссылки | `postman://`, `xref`, ссылки канала в теле | `parse_link`, `Resolve` |
| «Переслать в…», «Ответить в…» | сборка из хранилища, без повторной загрузки | `upload` + `Send`; `Forward` внутри своей учётки |
| Связанные беседы | `conv_link`, рядом в чтении, общий поиск и медиа | — |
| Медиа беседы (MED-01) | по общей модели, для связанных бесед вместе | — |
| Участник ↔ контакт | `person_link`, поиск совпадений, `services` у контакта | телефон, почта, имя участника |
| Правила, быстрые фильтры | по общей модели (`filters`), не по протоколу | — |
| Экспорт, печать | беседа любого канала (`crates/export`: HTML/PDF ленты, JSON) | — |
| Состояние и ошибки | «Проблема → причина → исправление» по `ChannelError` | перевод своих ошибок в `ChannelError` |
| Включение | настройка и ключ политики на канал | — |
| Тесты договора | демо-канал, набор проверок для любого канала | записанные ответы своего сервера |

### 9. Хранение (redb профиля, ADR 0006)

Новые таблицы — **добавочные**: создаются при открытии, формат кэша остаётся 7, миграции нет (как `local` и задачи в
формате 5). Старая версия Postman их не видит; данные остаются в базе и вернутся с новой версией. Части содержимого,
на которые ссылаются сообщения каналов, считаются в `check_refs` вместе с частями писем и `local`; следующие переносы
формата обязаны их сохранить.

| Таблица | Ключ → значение | Зачем |
|---|---|---|
| `chan_account` | `u64` → канал, `AccountId`, имя, `ChannelFeatures`, курсор, состояние | учётки каналов профиля |
| `conv` | `ConvKey` (`u64`, счётчик `meta.next_conv`) → строка беседы | беседа |
| `conv_remote` | `(учётка, &str ConversationId)` → `ConvKey` | id сервера → ключ |
| `conv_by_time` | `(−время последнего, ConvKey)` → `()` | список бесед всех каналов |
| `msg` | `MsgKey` (`u64`, `meta.next_msg`) → строка сообщения: автор, время, IR тела (zstd от 1 КиБ), вложения с `Hash` загруженных, реакции, отметки, `extra` | сообщение по своему ключу |
| `msg_remote` | `(ConvKey, &str MessageId)` → `MsgKey` | id сервера → ключ |
| `msg_by_id` | `(учётка, &str MessageId)` → `ConvKey` | удаление без беседы (Telegram) |
| `by_conv` | `(ConvKey, время, MsgKey)` → `()` | лента |
| `msg_edit` | `(MsgKey, n)` → прежняя версия и время | история правок |
| `participant` | `(учётка, &str ParticipantId)` → участник | люди канала |
| `member` | `(ConvKey, &str ParticipantId)` → роль | участники беседы |
| `person_link` | `(канал, учётка, &str ParticipantId)` → id контакта, как связано | один человек во всех каналах |
| `chan_outbox` | `(учётка, client_id)` → исходящее, шаг, срок | очередь отправки, переживает перезапуск |
| `xref` | `(&str адрес цели, &str адрес источника)` → `()` | обратные ссылки |
| `conv_link` | `(&str адрес беседы, &str адрес беседы)` → `()`, в обе стороны | связанные беседы (и почтовые) |
| `by_message_id` | `(&str Message-ID, LetterId)` → `()` | адрес почтового сообщения; пишется вместе с телом |

- **Вложения — по содержимому.** Загруженный файл канала — часть `part` по SHA-256 раскодированных байтов, как
  вложение письма: картинка из почты и та же из Telegram — одна запись, `part_refs` считает вхождения. Ядро пишет куски
  загрузки во временную запись и по концу переносит под хэш одной транзакцией; такой хэш уже есть — временная
  запись удаляется.
- **Что загружается.** Беседы и последние 50 сообщений каждой — сразу; дальше — по прокрутке, «Медиа беседы» и
  поиску. Вложения — превью сразу, файлы до 1 МиБ в открытой беседе — сразу, крупнее — по щелчку (как вложения
  писем). Загруженное читается без связи.
- **Свои данные канала** (`PluginStore`) — таблица `local`, ключи `chan/<канал>/<учётка>/…`: состояние обновлений
  Telegram, хранилище состояния и ключей шифрования Matrix — в базе профиля, не отдельной базой рядом (решение «всё в
  базе»). Ключи сквозного шифрования лежат в нём зашифрованными ключом из хранилища секретов ОС.
- **Удаление.** «Удалить у всех» и удалённое на сервере — строка сообщения уходит, `refs` частей убывают, документ
  поиска удаляется; в ленте — строка «Сообщение удалено» без текста. «Выйти» из учётки — её строки и `local`-ключи
  удаляются одной транзакцией (пачками по 100 000, как перенос писем).

### 10. Поиск — один индекс

Один индекс tantivy (`crates/search`) на почту и все каналы. Схема получает поля `channel`, `conv` (адрес беседы),
`author` (адрес участника или почты, строчными) — `STRING`; документ сообщения канала — id `c<MsgKey>`, письма —
как сейчас. Смена схемы — индекс пересобирается из базы в фоне один раз (`SearchIndex::open` уже так делает).

- Ищется: тело, автор, участники, название беседы, имена и текст вложений (воркер, T3-10), ссылки.
- Фильтры запроса (`core::query`): `канал:telegram`, `беседа:` (из открытой беседы — сразу), `от:` (человек — все
  его адреса через `person_link`); связанные беседы ищутся вместе.
- Словоформы, опечатки, раскладка, ё = е — как у почты, без отличий по каналам.
- Правка — документ заменяется; удаление — удаляется. Сквозное шифрование — по ответу на вопрос владельца №1.
- Беседа загружена не вся — внизу выдачи «Искать на сервере» (`ChannelSession::search`), найденное ложится в базу и
  в индекс.

### 11. Общие механизмы ядра

- **Кросс-ссылки.** «Копировать ссылку» у любого сообщения и беседы — `postman://…`; в письмо, заметку, задачу,
  встречу — ссылкой в теле. Ядро вынимает из каждого сохранённого тела (письма, сообщения, задачи, встречи) ссылки
  `postman://`, `mid:` и ссылки каналов (`parse_link`) и пишет `xref` — у сообщения видно «Где на него сослались».
  Ссылка канала в чужом письме (`https://t.me/c/…`) открывается в Postman, если учётка этого канала есть.
- **«Переслать в…», «Ответить в…»** между каналами: ядро собирает исходящее из хранилища — тело в IR со строкой
  «Переслано от Имя, дата», вложения по хэшу из `part` (у источника заново не скачиваются; не загружены — сначала
  загрузка у источника). В почту — черновик `compose::Draft` с этими вложениями; в чат — `upload` + `Send`. Внутри
  одной учётки канала с `forward_native` — пересылка сервером (без выгрузки). «Ответить в…» — цитата фрагмента
  источника и ссылка канала на него, если она есть. Предел цели (`max_attachment`, `max_text`) проверяется **до**
  отправки: не влезает — строка с причиной, отправки нет.
- **Связанные беседы**: «Связать с беседой…» — почтовая беседа ↔ группа Telegram ↔ комната Matrix (`conv_link`, в
  базе профиля). Связанные видны рядом в чтении, «Медиа беседы» и поиск идут по всем. Ядро предлагает связь, но не
  ставит её само (совпали участники и тема) — ставит пользователь.
- **Медиа беседы** (MED-01): `thread_media::collect` получает вход общей модели (`Letter` → сообщение любого канала,
  `Facts` по хэшам); правила отбора те же. Для связанных бесед — вместе, строка показывает канал.
- **Черновики, очередь, отмена, «позже»** (`send_later.rs`): черновик беседы — `local`, на сервер — по
  `server_drafts`. «Отправить» — строка `chan_outbox` и сообщение в ленте «отправляется»; окно «Отменить» CMF-03
  держит ядро (у чатов сервер не держит), закрыли Postman в окне — уходит сразу при выходе, как у IMAP и Exchange.
  «Отправить позже» — сервер при `scheduled_send` (Telegram `schedule_date`), иначе ядро держит до срока, пока Postman
  запущен; был закрыт — уходит при следующем запуске, с отметкой времени в ленте. Без связи — в очереди; повтор —
  с тем же `client_id`, без дублей.
- **Непрочитанное, счётчики, уведомления.** Счётчики сервера при `server_unread`, иначе — сообщения после `last_read`,
  кроме своих. Уведомление — по `live`, не своё, беседа не заглушена (сервером или в базе); упоминание меня — счётчик
  упоминаний и «важное» уведомление (`filters::Me` + `Message::mentions`). Заглушённое не считается в трее и папках —
  как CMF-01. Уведомления одной беседы за 5 с — один тост.
- **Заглушить, закрепить** — одни команды (`mail.mute`, `mail.pin` → общие `conv.mute`, `conv.pin`); почта —
  ключевыми словами, каналы — сервером или базой. Предел закреплённых — свой у канала, если сервер его задаёт
  (Telegram), иначе `pin::LIMIT`.
- **Быстрый ответ, ответ на фрагменты, @упоминания** (MSGR-01, MSGR-02) — поле ввода ленты, реплики, подсказка `@` из
  участников беседы и адресной книги; канал получает `Outgoing` с `quotes` и `mentions` и переводит их в свой формат
  по возможностям.
- **Правила и фильтры** клиента — по общей модели (автор, беседа, упоминание, вложение, канал), не по протоколу.
  Серверные правила почты (Sieve, Exchange) — как были, только для почты.
- **Экспорт и печать** — беседа любого канала: печать и PDF ленты тем же путём, что беседа почты; выгрузка — HTML
  ленты с вложениями и JSON общей модели. Форматы почты (EML, mbox, PST) — только для писем.
- **Состояние и ошибки** — общий статус «Проблема → причина → исправление»: `Network` — «нет связи, повтор через N с ·
  Повторить сейчас»; `SignedOut` — «вход истёк · Войти»; `Wait` (Telegram `FLOOD_WAIT`, Matrix `M_LIMIT_EXCEEDED`) —
  «сервер просит подождать N с»; `Denied` — «нет прав в беседе / учётка ограничена»; `TooLarge` — до отправки.
  Журнал — по-английски, без текста сообщений и номеров телефонов.
- **Секреты** — хранилище секретов ОС (`pets_shell::credentials`, `app-gpui/src/sys`), имя записи
  `Postman channel <канал> <учётка>`; нет хранилища — вход каждый раз (ADR 0011 п. 4). Ядро получает доступ к нему
  трейтом от композиционного корня; канал секретов не видит, кроме своего `ConnectCtx::secret`.
- **Включение.** Канал собирается признаком сборки (`channel-telegram`, `channel-matrix` в `app-gpui`) и включается
  настройкой «Настройки → Каналы» (до стабилизации — выключен по умолчанию, решение владельца). Ключ политики
  организации на канал — `AllowChannelTelegram`, `AllowChannelMatrix` (`0`/`1`, как `StorePasswords`; таблица POL-01):
  `0` — канала нет в настройках, поток учётки не запускается, данные в базе остаются до «Выйти» или удаления профиля.
- **Демо и тесты.** `crates/demo` — демо-канал `demo` со всеми возможностями: сервер в памяти, сценарий событий
  (правки, реакции, «прочитано», «печатает», удаление без беседы, `Gap`, `Wait`, обрыв связи), свои часы. Набор
  проверок договора `channel::conformance` гоняется на демо-канале и на записанных ответах каждого настоящего канала
  — договор проверяется без сети.

### 12. Слои

`postman_core::channel` — в ядре, без протоколов (тест слоёв). Плагины каналов — L2b: `backend-telegram`
(MTProto — `grammers`), `backend-matrix` (`matrix-sdk`); их протокольные крейты — в `PROTOCOL_CRATES` теста
`crates/core/tests/layers.rs`, плагины — в `PLUGINS`; друг от друга и от почтовых плагинов не зависят. Реестр:
`Registry::with_channel(p)` рядом с `with_mail` и `with_area`; строка в `app-gpui/src/plugins.rs` за признаком сборки.

## Проверка договора на двух протоколах

### Telegram (MTProto, `grammers`)

| Договор | Telegram |
|---|---|
| вход `Flow` | `Phone` → `auth.sendCode` → `Code { via: App/Sms/Call }` → `Password { hint }` (SRP 2FA); `Qr` — `auth.exportLoginToken`, ссылка `tg://login?token=…` |
| секрет | ключ авторизации и номер ДЦ (`grammers-session`); переезд ДЦ — `ChannelEvent::Secret` |
| курсор | состояние обновлений `pts`/`qts`/`date`/`seq` и `pts` каналов (`UpdatesState`); пропуск — `getDifference`/`getChannelDifference` внутри `grammers`, `Gap` — если сервер ответил «слишком длинно» |
| поток событий | `UpdateNewMessage`/`UpdateNewChannelMessage` → `Messages{live}`; `UpdateEditMessage` → `Messages` с `edited`; `UpdateDeleteMessages` → `Deleted { conversation: None }`; `UpdateDeleteChannelMessages` → `Deleted { Some }`; `UpdateReadHistoryInbox`/`Outbox` → `Read { Me / Them }`; `UpdateUserTyping` → `Typing`; `UpdateMessageReactions` → `Reactions` |
| беседы | `messages.getDialogs`: личные — `Direct`, группы и супергруппы — `Group`, каналы — `Broadcast`; темы форума — беседы с `parent`; папки — `messages.getDialogFilters` → `Folders` |
| тело | текст + сущности (смещения в UTF-16) → IR без разбора разметки: жирный, курсив, подчёркнутый, зачёркнутый, код, `pre` → `Block::Code`, `text_url` → `Link`, `mention_name` → `Mention`, `blockquote` → `Quote`, `spoiler` → `Marks::spoiler`, свои эмодзи → текстовая замена |
| ответ и фрагменты | `reply_to` с `quote_text` (`quote_fragment = true`) |
| правка, удаление | `Within(48 ч)` у обычных сообщений (сервер отвечает `MESSAGE_EDIT_TIME_EXPIRED` — пункт уже скрыт); удаление у всех — личные и свои в группах |
| отправка | `messages.sendMessage`/`sendMedia`, `random_id = client_id`, `schedule_date` (`scheduled_send = true`), `silent` |
| файлы | `upload.saveFilePart` кусками; загрузка — `upload.getFile`; просроченный file reference канал обновляет сам, перечитав сообщение |
| заглушить, закрепить, черновики | `account.updateNotifySettings`, `messages.toggleDialogPin`, `messages.saveDraft` — `server_* = true` |
| поиск на сервере | `messages.search` / `messages.searchGlobal` |
| ошибки | `FLOOD_WAIT_N` → `Wait { N }`; `AUTH_KEY_UNREGISTERED`, `SESSION_REVOKED` → `SignedOut`; `CHAT_WRITE_FORBIDDEN`, `USER_BANNED_IN_CHANNEL` → `Denied` |
| нет в договоре | секретные чаты (`e2e = No`: их нет и в официальных настольных клиентах), звонки, истории, платежи, игры; кнопки ботов и опросы — в `extra`, показ — текстом |

### Matrix (`matrix-sdk`)

| Договор | Matrix |
|---|---|
| вход `Flow` | `Server` → `Login` (пароль) или `Browser` (SSO, OIDC); после входа — `flow(Verify)`: `Compare { emoji }` (SAS) и `RecoveryKey` (резервная копия ключей) |
| секрет | токен доступа, id устройства, ключ шифрования хранилища ключей |
| курсор | `next_batch` `/sync` (или позиция упрощённого sliding sync, MSC4186) — непрозрачные байты |
| поток событий | `m.room.message` → `Messages`; `m.replace` (правка, отношение) → `Messages` с `edited`; `m.redaction` → `Deleted { Some, for_all }`; `m.annotation` → `Reactions`; `m.receipt` (`m.read`, `m.read.private`) → `Read`; `m.typing` → `Typing`; `limited` таймлайн → `Gap` (дозапрос по `prev_batch`) |
| беседы | комнаты: `m.direct` — `Direct`, прочие — `Group`; пространства (`m.space.child`) → `Folders`; ветки `m.thread` — поле `thread`, не отдельная беседа |
| тело | `formatted_body` (`org.matrix.custom.html`) → `Body::Html` — IR делает ядро в воркере; без него — `Body::Plain`; запасные цитаты ответа (`mx-reply`) отрезаются; упоминания — `m.mentions` |
| ответ и фрагменты | `m.in_reply_to`; цитаты фрагмента в протоколе нет — `quote_fragment = false`, цитата блоком в теле |
| правка, удаление | `edit = Always`, `delete_for_all = Always` (по правам комнаты); история правок — и с сервера (отношения), и своя |
| отправка | `PUT /send/{txn_id}`, `txn_id = client_id`; отложенной отправки нет — `scheduled_send = false` (держит ядро) |
| файлы | `mxc://` с проверкой подлинности (Matrix 1.11); в шифрованной комнате — ключ файла в `AttachmentSource` |
| сквозное шифрование | `e2e = Optional`: расшифровывает `matrix-sdk`; не расшифровалось — `MessageKind::Undecryptable`, пришёл ключ — то же сообщение заново (`Messages`, тот же `MessageId`) |
| заглушить, закрепить | правила push (`server_mute`), тег `m.favourite` (`server_pin`) |
| поиск на сервере | `/search` — только нешифрованные комнаты; шифрованные — только свой индекс |
| ошибки | `M_LIMIT_EXCEEDED` (`retry_after_ms`) → `Wait`; `M_UNKNOWN_TOKEN` (`soft_logout`) → `SignedOut`; `M_FORBIDDEN` → `Denied` |
| нет в договоре | мосты к другим сетям в клиенте, звонки (VoIP, Element Call), виджеты |

**Итог проверки.** Оба протокола ложатся без исключений в ядре. Отличия ушли в возможности (`quote_fragment`,
`scheduled_send`, `e2e`, сроки правки) и в два требования к ядру, которых почта не дала бы: удаление без беседы
(`msg_by_id`) и сообщение, которое приходит заново после расшифровки (запись по `MessageId` идемпотентна).

## Что не входит

- **WhatsApp, Signal** через неофициальные библиотеки — нарушение условий сервисов, блокировки, поломки при обновлениях
  (решение владельца). Договор их не исключает — вернёмся, если появится законный путь.
- **Мосты Matrix** к закрытым сетям в клиенте — только на сервере и отдельным решением.
- Секретные чаты Telegram, звонки и видео (Postman#566 — отложено владельцем 2026-11-11), истории, боты как платформа,
  платежи.
- Свой сервер сообщений: каналы — клиенты чужих серверов; The Post — почтовый сервер.
- Перенос переписки между каналами (как перенос писем между учётками ADR 0006) — только «Переслать в…».

## Последствия и риски

- **Плюс:** новый канал — крейт протокола, плагин и строка в корне; хранение, поиск, уведомления, интерфейс — уже
  есть. Удобства (фрагменты, упоминания, медиа, заглушить) делаются один раз для почты и чатов.
- **Почта не трогается:** адаптер только читает её таблицы и шлёт прежние команды; тесты почты — критерий CH-02.
- **Размер `postman.exe`:** `grammers` невелик; `matrix-sdk` с шифрованием (`vodozemac`) — заметная доля (оценка
  10–15 МиБ, замер — в CH-05). Бюджет растёт с функциями (решение владельца), но выключенный признаком сборки канал не
  весит ничего.
- **Хранилище Matrix:** `matrix-sdk` штатно держит состояние и ключи в SQLite. Решение «всё в базе» требует своё
  хранилище `StateStore`/`CryptoStore` поверх `PluginStore` — это работа CH-05; если не выйдет в разумный объём,
  исключение (файл рядом с базой) придёт владельцу отдельным вопросом с замером.
- **`grammers`** — небольшая команда, слой TL меняется с каждым выпуском Telegram; держим версию свежей (Dependabot),
  тесты договора на записанных ответах ловят поломку перевода. Запасной путь — TDLib (C++, больше размер) — только
  если `grammers` не справится.
- **Ограничения Telegram для сторонних клиентов:** подозрительное поведение (массовые действия, частый вход) ведёт к
  ограничению учётки. Ядро не делает массовых действий в каналах и уважает `FLOOD_WAIT`; вход — только по действию
  пользователя.
- **Чужие данные:** HTML Matrix — в воркере; картинки, стикеры (WebP; анимированные TGS — только превью), видео — как
  вложения писем, через воркер. Протокольные структуры (TL, JSON) разбирают библиотеки в потоке учётки — как JMAP.
- **Рост базы:** большие группы — только последние 50 сообщений и прокрутка; «Медиа беседы» и поиск берут файлы по
  запросу; вложения дедуплицируются с почтой.
- **Смена схемы поиска** — разовая пересборка индекса в фоне после обновления.

## Отвергнутые варианты

- **Почта — полноценный `ChannelSession` с копией в таблицы чатов** — две правды об одном письме, двойной диск,
  ломает «почта не упрощается».
- **Перевести почту на таблицы чатов** — переписать ADR 0006 сразу после формата 7; риск для главного канала.
- **Канал пишет кэш сам, как `MailPlugin`** — каждый канал заново пишет хранение, поиск и счётчики; противоречит
  «канал реализует только протокол».
- **Чат как письма** (сообщение → `LetterRow`) — отвергнуто владельцем в «Вид»: почтовые поля и действия к чатам не
  подходят.
- **`async fn` в трейтах** — не dyn-совместимы (ADR 0004), реестр из разнородных плагинов не собрать.

## План по карточкам

1. **CH-02** (ядро, класс B): `postman_core::channel` — типы, трейты, `channel::run`; таблицы п. 9; `ConversationSource`
   для чатов и адаптер `mail`; поля поиска; `xref`, `conv_link`, `person_link`; `chan_outbox` с окном «Отменить» и
   «позже»; дополнения IR и `ContactCard::services`; демо-канал и `channel::conformance`. Объёмно — предлагается
   резать на волны одной полосы: **CH-02а** договор + хранение + демо-канал + проверки; **CH-02б** адаптер почты,
   список бесед и умные папки, поиск, счётчики и уведомления; **CH-02в** кросс-ссылки, «Переслать в…»/«Ответить в…»,
   связанные беседы, медиа беседы по общей модели, экспорт.
2. **CH-03** Telegram (`backend-telegram`, `grammers`): вход, диалоги, история, поток, отправка, правки, файлы;
   за признаком сборки, ключ `api_id`/`api_hash` — секретом сборки; `AllowChannelTelegram`.
3. **CH-04** интерфейс (гибрид «Вид»): учётки каналов в дереве, строка чата в списке, лента с полем ввода, умные
   папки, «Настройки → Каналы», вход; дизайн — по «Запросу в дизайн» карточки.
4. **CH-05** Matrix (`backend-matrix`, `matrix-sdk`): вход, комнаты, sync, шифрование, проверка устройств, резервная
   копия ключей; своё хранилище на `PluginStore`; `AllowChannelMatrix`.

## Решения для владельца

### 1. Переписка со сквозным шифрованием (Matrix): хранить ли расшифрованный текст в базе и искать ли по нему

Что это: в шифрованной комнате Matrix сервер видит только шифр; расшифровывает Postman у себя. Вопрос — оставлять ли
расшифрованный текст в базе профиля (тогда он читается без связи и находится поиском) или держать его только в
памяти. Для писем S/MIME сейчас правило строгое: расшифрованное не хранится и не ищется (ADR 0006 п. 7).

- **А. Хранить и искать, как обычные сообщения** (так делают Element и другие клиенты). Плюсы: поиск, офлайн-чтение,
  «Медиа беседы» работают в шифрованных комнатах. Минусы: на диске — открытый текст под той же защитой, что письма
  (права профиля, BitLocker организации); ключи сквозного шифрования и так лежат в этой же базе, поэтому стойкость
  к краже файла почти не меняется.
- **Б. Хранить, но не искать.** Плюсы: офлайн-чтение. Минусы: поиск «молчит» по самым важным беседам — непонятно
  пользователю.
- **В. Только в памяти, как S/MIME.** Плюсы: на диске только шифр. Минусы: каждый запуск — расшифровка всей видимой
  истории заново, поиска и офлайн-чтения нет; для чата, где сообщений тысячи, это заметно медленно.

**Решено владельцем (2026-11-14): настройкой.** «Настройки → Каналы → Шифрованные беседы»: «Хранить на этом
компьютере и искать» (вариант А, по умолчанию) / «Хранить, не искать» (Б) / «Только в памяти» (В). Ключ политики
`ChannelE2EStorePlaintext` задаёт вариант за пользователя (`2` — А, `1` — Б, `0` — В) и прячет выбор. Смена на Б
убирает текст шифрованных бесед из индекса, на В — и из базы (остаётся шифр). Для S/MIME правило не меняется.

### 2. Где хранить ручную связь «участник канала = контакт»

Что это: Postman сам связывает участника Telegram или Matrix с контактом по телефону или почте. Когда совпадения нет,
пользователь говорит «это тот же человек». Вопрос — куда записать эту связь.

- **А. В карточку контакта на сервере** (поле адресов в мессенджерах: JSContact `onlineServices`, vCard `IMPP`,
  у Exchange — адреса чата). Плюсы: связь есть на всех компьютерах и в телефоне, переживает переустановку. Минусы:
  карточка меняется на сервере — это видно в других программах (Outlook покажет адрес чата у контакта); у контакта
  справочника организации (GAL) так нельзя — там только вариант Б.
- **Б. Только в базе профиля на этом компьютере.** Плюсы: ничего не меняется на сервере. Минусы: на другом
  компьютере связи нет, переустановка её теряет.

**Решено владельцем (2026-11-14): на сервере** — вариант А. Связь пишется в карточку личного контакта на сервере
(JSContact `onlineServices`, vCard `IMPP`, адреса чата Exchange) только по явному «Связать с контактом»;
автоматические совпадения на сервер не пишутся. Контакты справочника организации (GAL) сервер менять не даёт — для
них связь хранится в базе профиля.
