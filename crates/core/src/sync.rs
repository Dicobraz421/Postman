//! Синхронизация: договор команд и событий между интерфейсом и путями к серверам, и общий цикл (ADR 0004).
//! UI шлёт команды, в ответ получает события и перечитывает кэш. Каждая учётная запись — свой поток с tokio и
//! своим соединением с кэшем; что внутри потока (JMAP, IMAP…) — дело плагина пути (`postman-backend-*`), `core`
//! даёт ему поток ([`spawn_thread`]), цикл «без связи» ([`supervise`]) и этот договор.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use postman_calendar::{Busy, Invitation, Partstat};
use postman_compose::{Address, Draft};

use crate::backend::BoxFuture;
use crate::categories;
use crate::export;
use crate::invite::Delivery;
use crate::lifecycle;
use crate::link::{Connection, Link};
use crate::model::{Auth, ContactCard, Filter, FolderId, ImportError, JobId, LetterId, MigrationRequest, MigrationState, Principal, Remote};
pub use crate::model::{Error, ErrorKind, Release};
use crate::schedule::Meeting;
use crate::send_later::Scheduled;
use crate::settings;
use crate::tasks;
use postman_cache::MailboxRow;
use postman_search::SearchIndex;
use tokio::sync::mpsc::UnboundedReceiver;
/// Канал команд синхронизации (UI держит отправителя).
pub use tokio::sync::mpsc::UnboundedSender;

#[derive(Clone)]
pub struct Account {
    pub url: String,
    pub auth: Auth,
}

impl Account {
    pub fn from_env() -> Option<Self> {
        let url = std::env::var("POSTMAN_URL").ok()?;
        let auth = match std::env::var("POSTMAN_TOKEN") {
            Ok(t) => Auth::Bearer(t),
            Err(_) => Auth::Basic { user: std::env::var("POSTMAN_USER").ok()?, password: std::env::var("POSTMAN_PASSWORD").ok()? },
        };
        Some(Self { url, auth })
    }
}

pub enum Command {
    Body(String),
    /// Беседа (ключ цепочки, `EmailRow::thread_id`): письма из всех папок и их тела — в кэш → `ThreadReady`.
    Thread(String),
    Keyword {
        id: String,
        keyword: &'static str,
        on: bool,
    },
    /// Поставить `add` и снять `remove` (ключевые слова — категории, метки, флаг) у писем `ids` (ключи кэша) — по
    /// одному `Email/set` на аккаунт (S5-02). UI уже поменял кэш; письма, которые сервер не изменил, — с сервера
    /// заново.
    SetKeywords {
        ids: Vec<String>,
        add: Vec<String>,
        remove: Vec<String>,
    },
    /// Письма по фильтру на сервере (`hasKeyword`, `notKeyword`…) → `SearchResults`. `mailbox` — ключ папки кэша;
    /// `None` — все папки своего аккаунта («Где искать: Все папки»).
    Query {
        filter: Filter,
        mailbox: Option<String>,
        ticket: u64,
    },
    /// Число писем по каждому фильтру на сервере (в папке `mailbox` или во всех) → `Counts`.
    Counts {
        filters: Vec<Filter>,
        mailbox: Option<String>,
        ticket: u64,
    },
    /// Перенести письмо (удаление в «Удалённые»). UI уже перенёс его в кэше; сбой — письмо с сервера заново.
    Move {
        id: String,
        from: String,
        to: String,
    },
    /// Удалить письмо насовсем (из «Удалённых»). UI уже убрал его из кэша; сбой — письмо с сервера заново.
    Destroy(String),
    /// Новая папка (T1-04б) → дерево перечитано, `MailboxesChanged`; отказ — `FolderFailed`. `account` — аккаунт
    /// ключей кэша (`postman_cache::account_of`; `None` — основной), `parent` — ключ папки-родителя, `None` — верхний
    /// уровень ящика.
    CreateFolder {
        account: Option<String>,
        parent: Option<String>,
        name: String,
    },
    /// Переименовать папку (ключ кэша) → `MailboxesChanged` или `FolderFailed`.
    RenameFolder {
        mailbox: String,
        name: String,
    },
    /// Перенести папку под `parent` (`None` — верхний уровень ящика) → `MailboxesChanged` или `FolderFailed`. В саму
    /// себя, в свою подпапку и в другой аккаунт — отказ ядра до плагина ([`admit`]).
    MoveFolder {
        mailbox: String,
        parent: Option<String>,
    },
    /// `replace` — id прежней версии черновика на сервере. `release` — когда сервер выпустит письмо (S5-06):
    /// `After` — окно «Отменить» (без FUTURERELEASE у сервера — сразу), `At` — «Отправить позже» → `Sent`.
    Send {
        draft: Box<Draft>,
        replace: Option<String>,
        ticket: u64,
        release: Release,
    },
    /// Отменить отправку, которую сервер ещё держит, и вернуть письмо в «Черновики» → `Unsent` или `UnsendFailed`,
    /// затем `Scheduled`.
    Unsend {
        scheduled: Scheduled,
        ticket: u64,
    },
    /// Перечитать отложенные отправки → `Scheduled`.
    Scheduled,
    SaveDraft {
        draft: Box<Draft>,
        replace: Option<String>,
        ticket: u64,
    },
    /// Догрузить следующую страницу ящика (после самого старого письма в кэше).
    LoadMore {
        mailbox: String,
    },
    /// Серверный поиск; `mailbox: None` — во всех папках.
    Search {
        query: String,
        mailbox: Option<String>,
        ticket: u64,
    },
    /// Скачать вложение/картинку.
    Blob {
        blob_id: String,
        name: String,
        mime: String,
    },
    /// Ответить на приглашение из письма; `in_reply_to` — Message-ID письма с приглашением.
    RespondInvite {
        invite: Box<Invitation>,
        status: Partstat,
        in_reply_to: Option<String>,
    },
    /// События календаря в `[from, to)` (Unix-секунды), повторения разворачивает сервер → `EventsLoaded`.
    Events {
        from: i64,
        to: i64,
    },
    /// Поиск в календаре по тексту в `[from, to)` → `EventsFound` (S5-21).
    SearchEvents {
        text: String,
        from: i64,
        to: i64,
    },
    /// Занятость адресов и моя в `[from, to)` → `Availability`. `sender` — адрес «От» встречи (S7-03б): занятость
    /// — путём его учётной записи профиля; `None` — основной.
    Availability {
        ticket: u64,
        emails: Vec<String>,
        from: i64,
        to: i64,
        sender: Option<String>,
    },
    /// Создать встречу и разослать приглашения → `MeetingCreated` или `MeetingFailed`.
    CreateMeeting {
        meeting: Box<Meeting>,
        ticket: u64,
    },
    /// Изменить свою встречу (я — организатор) и разослать изменения → `MeetingUpdated` или `MeetingFailed` (S7-07б,
    /// решение владельца 2026-10-23). `event` — событие недели (id и UID — как их дал путь); `meeting` — поля вкладки
    /// встречи (`uid` — UID события, `from` — организатор). `series` — у экземпляра повтора изменить всю серию (время
    /// серии сдвигается на ту же разницу), иначе — только этот экземпляр; у одиночной не важно.
    UpdateMeeting {
        event: Box<postman_calendar::Event>,
        meeting: Box<Meeting>,
        series: bool,
        ticket: u64,
    },
    /// Отменить свою встречу: участникам — отмена, у меня встречи нет → `MeetingCancelled` или `MeetingFailed`.
    /// `series` — как у `UpdateMeeting` (иначе — только этот экземпляр); `from` — адрес организатора (учётная запись).
    CancelMeeting {
        event: Box<postman_calendar::Event>,
        series: bool,
        from: Option<String>,
        ticket: u64,
    },
    /// Открыть коллеге (адрес) только мою занятость → `FreeBusyShared` или `CalendarFailed`.
    ShareFreeBusy {
        email: String,
    },
    /// Настройки ящика на сервере: автоответ, квота (`settings.rs`) → `Event::Settings`.
    Settings(settings::Request),
    /// Перечитать личные контакты и справочник → `ContactsLoaded`, `DirectoryLoaded` (и `Contacts` для подсказок).
    Contacts,
    /// Страница справочника (T1-25б): до `max` людей с `offset` по порядку пути → `Event::DirectoryPage`. `account` —
    /// учётная запись профиля (`None` — основная).
    DirectoryPage {
        account: Option<String>,
        offset: u32,
        max: u32,
    },
    /// Люди справочника по набранной строке → `PeopleFound` (S7-08): поиск людей — функция ядра, каждый путь отвечает
    /// своим поиском (JMAP — `Principal/query`, EWS — `FindPeople`, запасной — `ResolveNames`); справочника нет — пусто.
    /// `offset` — с какого найденного (подгрузка по прокрутке, решение владельца 2026-10-28): `0` — первая страница;
    /// страница — по пути (20 человек).
    FindPeople {
        text: String,
        offset: u32,
    },
    /// Подробности человека справочника по адресу — при открытии его карточки → `PersonDetails` (S7-08б): у EWS
    /// `FindPeople` телефона не отдаёт, его даёт `ResolveNames`. У кого подробностей нет — `person: None`.
    PersonDetails {
        email: String,
    },
    /// Создать (`old: None`) или изменить карточку → `ContactSaved` или `ContactFailed`, затем `ContactsLoaded`.
    SaveContact {
        card: Box<ContactCard>,
        old: Option<Box<ContactCard>>,
        ticket: u64,
    },
    /// Удалить карточку → `ContactDeleted` или `ContactFailed`, затем `ContactsLoaded`.
    DeleteContact {
        id: String,
        ticket: u64,
    },
    /// Таблица категорий аккаунта на сервере (`categories.rs`, S5-02б) → `Event::Categories`.
    Categories(categories::Request),
    /// Жизненный цикл (S5-04, `lifecycle.rs`): правки писем (ключи кэша; папки — ключи кэша или
    /// [`lifecycle::ARCHIVE`] — «Архив» аккаунта, нет — создаётся с именем `archive_name`) — один `Email/set` на
    /// аккаунт. UI уже поменял кэш; письма, которые сервер не изменил, — с сервера заново.
    Patch {
        patches: Vec<lifecycle::Patch>,
        archive_name: String,
    },
    /// Рассылки «Входящих» своего аккаунта: прочитанные раньше `cutoff` (Unix-секунды), кроме последней от каждого
    /// отправителя, — в «Архив» → `Event::Newsletters`.
    Newsletters {
        cutoff: i64,
        archive_name: String,
    },
    /// «Сообщить о фишинге» (S5-08): письма `ids` (ключи кэша) — вложениями `message/rfc822` одним письмом на адрес
    /// `to` от своего адреса → `Event::Reported`. Без связи — в очередь, как правки писем.
    ReportPhishing {
        ids: Vec<String>,
        to: Address,
        subject: String,
        text: String,
        ticket: u64,
    },
    /// «Повторить сейчас» (S5-08): без связи — попытка сразу, не дожидаясь паузы; со связью — ничего.
    Reconnect,
    /// Задачи (S6-02, `tasks.rs`): загрузка, сверка, правки → `Event::Tasks`. Правка без связи — в кэше, уходит
    /// по возвращении связи (`tasks::Request::Flush`).
    Tasks(tasks::Request),
    /// Шаг поручения (S6-09, `assign.rs`): своя копия задачи уже в кэше (`tasks::save_local`) — сначала она на
    /// сервер (без рассылки), затем письма iTIP шага → `Event::Delegated`. Без связи — в очередь, как правки.
    /// `release` — как у `Send`: «Забрать» и «Удалить поручение» (S6-11) держатся сервером окно «Отменить».
    Delegate {
        uid: String,
        drafts: Vec<Draft>,
        ticket: u64,
        release: Release,
    },
    /// «Отменить» шага поручения (S6-11): письма, которые сервер ещё держит, не отправлять и удалить из
    /// «Отправленных» → `Event::Withheld`.
    Withhold {
        scheduled: Vec<Scheduled>,
        ticket: u64,
    },
    /// Перенести письма в папку (ADR 0006 п. 3–4) — в поток ядра: перенос в базе одной транзакцией, затем ядро решает
    /// само: письма своей учётки цели — перенос на её сервере, прочие — задание переноса (`Event::Migration`).
    MoveLetters {
        ids: Vec<LetterId>,
        to: FolderId,
    },
    /// Письмо целиком (как его отдаёт сервер) — у учётки письма → `Event::Raw`. Письмо «На этом компьютере» — из
    /// базы, без сети.
    FetchRaw {
        letter: LetterId,
    },
    /// Выгрузить письмо (содержимое — в базе) в папку `folder` учётки цели с ключевыми словами (и `$seen`,
    /// `$flagged`) и датой получения (Unix-секунды) → `Event::Imported` или `Event::ImportFailed`. Путь без выгрузки
    /// ([`Features::import`](crate::model::Features::import)) отвечает отказом. `retry` — повтор после сбоя (выгрузка
    /// могла дойти, ответ — нет): путь, у которого сервер не отвечает `alreadyExists`, сначала ищет письмо по
    /// `message_id` в папке цели и, если нашёл, отвечает `Imported` с ним — без дубля. `message_id` — с угловыми
    /// скобками, из ответа воркера (`MimeSplit`): письмо в процессе приложения путь не разбирает; `None` — у письма его
    /// нет (или воркер не ответил), искать нечем.
    Import {
        job: JobId,
        letter: LetterId,
        folder: FolderId,
        keywords: Vec<String>,
        received_at: i64,
        message_id: Option<String>,
        retry: bool,
    },
    /// Сервер сам переносит письма в папку `to` другого аккаунта той же сессии, без выгрузки
    /// ([`Features::server_copy`](crate::model::Features::server_copy)) → по письму `Event::Imported` или
    /// `Event::ImportFailed`.
    ServerCopy {
        job: JobId,
        letters: Vec<LetterId>,
        to: FolderId,
    },
    /// Удалить письма задания у источника — учётки `account` (аккаунт ключей кэша, основной — пустая строка) по их
    /// `detached`-связям → по письму `Event::Destroyed` или `Event::DestroyFailed` (решение владельца 2026-10-29:
    /// письмо у источника удалено только по ответу сервера). Письма уже нет на сервере — тоже `Destroyed`.
    DestroyLetters {
        job: JobId,
        account: String,
        letters: Vec<LetterId>,
    },
    /// Ответ, пауза, продолжение, отмена задания переноса — в поток ядра → `Event::Migration`.
    Migration(MigrationRequest),
    /// Экспорт писем в файлы в каталог `dir` (S8-31) — в поток ядра: новое задание ([`export::Exporter::start`]) →
    /// `Event::Export`; письма без содержимого — `FetchRaw` у их учёток.
    Export {
        scope: export::Scope,
        format: export::Format,
        dir: std::path::PathBuf,
    },
    /// «Остановить» экспорт — в поток ядра: готовые файлы остаются, недописанный mbox удаляется → `Event::Export`.
    StopExport {
        job: export::ExportId,
    },
}

impl Command {
    /// Копия команды, которую можно повторить, когда связь вернётся (`link`): правки писем и сообщение о фишинге.
    /// Остальное (тела, поиск, отправка с окном письма) без связи не ждёт — интерфейс показывает ошибку сам.
    pub fn replayable(&self) -> Option<Command> {
        Some(match self {
            Command::Keyword { id, keyword, on } => Command::Keyword { id: id.clone(), keyword, on: *on },
            Command::SetKeywords { ids, add, remove } => Command::SetKeywords { ids: ids.clone(), add: add.clone(), remove: remove.clone() },
            Command::Move { id, from, to } => Command::Move { id: id.clone(), from: from.clone(), to: to.clone() },
            Command::Destroy(id) => Command::Destroy(id.clone()),
            Command::Patch { patches, archive_name } => Command::Patch { patches: patches.clone(), archive_name: archive_name.clone() },
            Command::Tasks(r) => Command::Tasks(r.replayable()?),
            Command::Delegate { uid, drafts, ticket, release } => {
                Command::Delegate { uid: uid.clone(), drafts: drafts.clone(), ticket: *ticket, release: *release }
            }
            Command::ReportPhishing { ids, to, subject, text, ticket } => {
                Command::ReportPhishing { ids: ids.clone(), to: to.clone(), subject: subject.clone(), text: text.clone(), ticket: *ticket }
            }
            _ => return None,
        })
    }
}

/// Куда идёт команда, когда учётных записей в профиле несколько (S7-03б, `docs/specs/S7.md`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// В синхронизацию учётной записи профиля: `None` — основной, `@<n>` — дополнительной.
    Profile(Option<String>),
    /// Во все: поиск и фильтры «Все папки», неделя календаря, отложенные отправки, «Повторить сейчас».
    All,
    /// Туда, чей адрес отправителя (`Send`, `SaveDraft`): учётную запись знает список «От» интерфейса.
    Sender(String),
}

/// Учётная запись профиля ключа кэша.
fn profile_of_key(key: &str) -> Option<String> {
    postman_cache::profile_of(postman_cache::account_of(key)).map(str::to_owned)
}

/// Чьё письмо или папка формата 6 (ADR 0006 п. 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Home {
    /// Учётки: ключ — аккаунт ключей кэша (основной — пустая строка), как у `postman_cache::Owner::Account`.
    Account(String),
    /// «На этом компьютере»: без сервера.
    Local,
}

impl Home {
    /// Поток, который работает с письмами и папками этой учётки: синхронизация её учётной записи профиля; «На этом
    /// компьютере» — ядро.
    pub fn route(&self) -> Route {
        match self {
            Home::Local => Route::Core,
            Home::Account(a) => {
                Route::Sync(Target::Profile(postman_cache::profile_of(Some(a.as_str()).filter(|a| !a.is_empty())).map(str::to_owned)))
            }
        }
    }
}

/// Откуда маршрут знает, чьи письма и папки: письмо — по действующей связи с сервером (`letter_remote`), без неё —
/// [`Home::Local`]; папка — по владельцу. Даёт хранилище формата 6.
pub trait Homes {
    fn letter(&self, id: LetterId) -> Home;
    fn folder(&self, id: FolderId) -> Home;
}

/// Куда идёт команда в профиле с письмами формата 6 ([`Command::routes_by`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// Поток ядра, без сети: письма и папки «На этом компьютере», перенос между учётками и его задания.
    Core,
    /// Поток синхронизации учётной записи.
    Sync(Target),
}

impl Command {
    /// Маршрут с письмами и папками формата 6: команда над [`LetterId`] — по учётке письма (`FetchRaw`), выгрузка —
    /// по учётке папки цели (`Import`, `ServerCopy`), удаление у источника — по учётке источника (`DestroyLetters`);
    /// «На этом компьютере», перенос (`MoveLetters`), задания (`Migration`) и экспорт (`Export`, `StopExport`) — в
    /// ядро. Прочие команды — по ключам
    /// кэша, как [`Command::routes`].
    pub fn routes_by(self, homes: &dyn Homes) -> Vec<(Route, Command)> {
        let route = match &self {
            Command::FetchRaw { letter } => homes.letter(*letter).route(),
            Command::Import { folder, .. } => homes.folder(*folder).route(),
            Command::ServerCopy { to, .. } => homes.folder(*to).route(),
            Command::DestroyLetters { account, .. } => Home::Account(account.clone()).route(),
            Command::MoveLetters { .. } | Command::Migration(_) | Command::Export { .. } | Command::StopExport { .. } => Route::Core,
            _ => return self.routes().into_iter().map(|(t, c)| (Route::Sync(t), c)).collect(),
        };
        vec![(route, self)]
    }

    /// Куда идёт команда и в каком виде: правки писем разных учётных записей — по команде на учётную запись. По
    /// ключам кэша (префикс учётной записи); команды над [`LetterId`] здесь — в основную, их маршрут —
    /// [`Command::routes_by`].
    pub fn routes(self) -> Vec<(Target, Command)> {
        let one = |key: &str| Target::Profile(profile_of_key(key));
        let scope = |mailbox: &Option<String>| mailbox.as_deref().map_or(Target::All, one);
        let target = match &self {
            Command::Body(k) | Command::Thread(k) | Command::Destroy(k) => one(k),
            Command::Keyword { id, .. } | Command::Move { id, .. } => one(id),
            Command::RenameFolder { mailbox, .. } | Command::MoveFolder { mailbox, .. } => one(mailbox),
            Command::CreateFolder { account, .. } => Target::Profile(postman_cache::profile_of(account.as_deref()).map(str::to_owned)),
            Command::DirectoryPage { account, .. } => Target::Profile(account.clone()),
            Command::Blob { blob_id, .. } => one(blob_id),
            Command::LoadMore { mailbox } => one(mailbox),
            Command::Unsend { scheduled, .. } => one(&scheduled.submission),
            Command::Withhold { scheduled, .. } => scheduled.first().map_or(Target::Profile(None), |s| one(&s.submission)),
            Command::Query { mailbox, .. } | Command::Counts { mailbox, .. } | Command::Search { mailbox, .. } => scope(mailbox),
            Command::Send { draft, .. } | Command::SaveDraft { draft, .. } => Target::Sender(draft.from.email.clone()),
            Command::CreateMeeting { meeting, .. } | Command::UpdateMeeting { meeting, .. } => {
                meeting.from.clone().map_or(Target::Profile(None), Target::Sender)
            }
            Command::CancelMeeting { from, .. } => from.clone().map_or(Target::Profile(None), Target::Sender),
            Command::Availability { sender, .. } => sender.clone().map_or(Target::Profile(None), Target::Sender),
            Command::Events { .. } | Command::Scheduled | Command::Reconnect | Command::Newsletters { .. } => Target::All,
            Command::SetKeywords { .. } | Command::Patch { .. } | Command::ReportPhishing { .. } => return self.split(),
            _ => Target::Profile(None),
        };
        vec![(target, self)]
    }

    /// Команда над письмами — по учётным записям их ключей (порядок — первой встречи).
    fn split(self) -> Vec<(Target, Command)> {
        fn groups<T>(items: Vec<T>, key: impl Fn(&T) -> &str) -> Vec<(Option<String>, Vec<T>)> {
            let mut out: Vec<(Option<String>, Vec<T>)> = Vec::new();
            for item in items {
                let p = profile_of_key(key(&item));
                match out.iter_mut().find(|(q, _)| *q == p) {
                    Some((_, v)) => v.push(item),
                    None => out.push((p, vec![item])),
                }
            }
            out
        }
        match self {
            Command::SetKeywords { ids, add, remove } => groups(ids, |s| s.as_str())
                .into_iter()
                .map(|(p, ids)| (Target::Profile(p), Command::SetKeywords { ids, add: add.clone(), remove: remove.clone() }))
                .collect(),
            Command::Patch { patches, archive_name } => groups(patches, |p| p.id.as_str())
                .into_iter()
                .map(|(p, patches)| (Target::Profile(p), Command::Patch { patches, archive_name: archive_name.clone() }))
                .collect(),
            Command::ReportPhishing { ids, to, subject, text, ticket } => groups(ids, |s| s.as_str())
                .into_iter()
                .map(|(p, ids)| {
                    let c = Command::ReportPhishing { ids, to: to.clone(), subject: subject.clone(), text: text.clone(), ticket };
                    (Target::Profile(p), c)
                })
                .collect(),
            other => vec![(Target::Profile(None), other)],
        }
    }

    /// Копия команды для каждой учётной записи ([`Target::All`]); у прочих — `None`.
    pub fn for_each_account(&self) -> Option<Command> {
        Some(match self {
            Command::Query { filter, mailbox, ticket } => Command::Query { filter: filter.clone(), mailbox: mailbox.clone(), ticket: *ticket },
            Command::Counts { filters, mailbox, ticket } => Command::Counts { filters: filters.clone(), mailbox: mailbox.clone(), ticket: *ticket },
            Command::Search { query, mailbox, ticket } => Command::Search { query: query.clone(), mailbox: mailbox.clone(), ticket: *ticket },
            Command::Events { from, to } => Command::Events { from: *from, to: *to },
            Command::Scheduled => Command::Scheduled,
            Command::Reconnect => Command::Reconnect,
            Command::Newsletters { cutoff, archive_name } => Command::Newsletters { cutoff: *cutoff, archive_name: archive_name.clone() },
            _ => return None,
        })
    }
}

/// Что делает синхронизация. Текст — у интерфейса (`sync.ftl`), здесь только код и данные.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Connecting {
        url: String,
    },
    SignedIn {
        user: String,
    },
    /// Первая загрузка: папка `mailbox` (имя с сервера).
    Loading {
        mailbox: String,
    },
    Synced,
}

impl ErrorKind {
    pub fn of(e: &Error) -> Self {
        e.kind()
    }
}

/// На каком шаге синхронизации случилась ошибка (для журнала).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Start,
    Connect,
    Load,
    Refresh,
    Prefetch,
    Command,
    Restore,
}

/// Ошибка синхронизации: род, шаг и подробность. `detail` — текст ошибки как есть (может быть ответом сервера),
/// интерфейс показывает его только как подробность после своего текста.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncError {
    pub kind: ErrorKind,
    pub stage: Stage,
    pub detail: String,
}

impl SyncError {
    pub fn new(stage: Stage, e: &Error) -> Self {
        Self { kind: e.kind(), stage, detail: e.to_string() }
    }

    /// Синхронизация не запустилась (поток, рантайм, нет пути к серверу).
    pub fn internal(detail: impl std::fmt::Display) -> Self {
        Self { kind: ErrorKind::Internal, stage: Stage::Start, detail: detail.to_string() }
    }
}

/// Для журнала (по-английски); интерфейсу — `kind` и `detail`.
impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?} failed ({:?}): {}", self.stage, self.kind, self.detail)
    }
}

/// Для журнала и запасного интерфейса (по-английски).
impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Status::Connecting { url } => write!(f, "connecting to {url}"),
            Status::SignedIn { user } => write!(f, "signed in: {user}"),
            Status::Loading { mailbox } => write!(f, "loading {mailbox}"),
            Status::Synced => write!(f, "synced"),
        }
    }
}

/// Отправитель для окна письма: свой адрес или общий ящик (S4-06).
#[derive(Debug, Clone, PartialEq)]
pub struct Sender {
    /// Общий ящик, из которого уходит письмо (id аккаунта, он же префикс ключей кэша); `None` — свой.
    pub account: Option<String>,
    pub address: Address,
    pub signature: String,
}

pub enum Event {
    Status(Status),
    MailboxesChanged,
    EmailsChanged,
    BodyReady(String),
    /// Беседа загружена: ключ цепочки и ключи её писем по времени (старые первыми), тела — в кэше.
    ThreadReady {
        thread: String,
        ids: Vec<String>,
    },
    BlobReady {
        blob_id: String,
        data: Arc<Vec<u8>>,
    },
    /// Тела загружены в фоне — их текст можно добавить в поиск.
    BodiesStored(Vec<String>),
    /// Догрузка ящика (`LoadMore`) пришла: `added` писем; 0 — на сервере старше нет.
    LoadedMore {
        mailbox: String,
        added: usize,
    },
    /// В календаре на сервере что-то изменилось (push, с задержкой на пачку) — открытую неделю перечитать.
    CalendarChanged,
    SearchResults {
        ticket: u64,
        ids: Vec<String>,
        total: u64,
    },
    /// Ответ на `Counts`: по числу на фильтр; `None` — запрос не удался.
    Counts {
        ticket: u64,
        counts: Option<Vec<u64>>,
    },
    BlobFailed {
        blob_id: String,
        error: String,
    },
    /// Учётная запись отправителя и подпись (из Identity).
    Me {
        address: Address,
        signature: String,
    },
    /// Что умеет сервер (ADR 0003, S7-03): интерфейс прячет то, чего нет. Приходит сразу после входа.
    Features(crate::model::Features),
    /// Адреса, от которых можно отправить письмо (S4-06): свои Identity и Identity общих ящиков.
    Senders(Vec<Sender>),
    /// Личные контакты (имя, адрес) — для автодополнения.
    Contacts(Vec<(String, String)>),
    /// Письмо отправлено; `scheduled` — сервер держит его до срока (окно «Отменить» или «Отправить позже»).
    Sent {
        ticket: u64,
        scheduled: Option<Scheduled>,
    },
    /// Отправка отменена, письмо — снова черновик в «Черновиках» (`email` — ключ кэша).
    Unsent {
        ticket: u64,
        email: String,
    },
    /// Отмена не удалась: `too_late` — письмо уже ушло.
    UnsendFailed {
        ticket: u64,
        too_late: bool,
        error: String,
    },
    /// Отправки, которые сервер держит, и сколько он позволяет задержать письмо (секунды, `send_later::max_delay`;
    /// 0 — отложенной отправки нет).
    Scheduled {
        list: Vec<Scheduled>,
        max_delay: u64,
    },
    DraftSaved {
        ticket: u64,
        id: String,
    },
    /// Ошибка конкретной отправки/сохранения — показывается в окне письма.
    ComposeFailed {
        ticket: u64,
        error: String,
    },
    InviteAnswered {
        uid: String,
        status: Partstat,
        delivery: Delivery,
    },
    InviteFailed {
        uid: String,
        error: String,
    },
    /// События календаря в `[from, to)` — без отменённых, по времени начала.
    /// События недели: мои и календарей коллег (`Event::owner`, S5-22); коллеги — все, кто открыл мне календарь.
    /// `account` — учётная запись профиля, чьи это события (S7-03б): у дополнительной её события — с владельцем, как
    /// календарь коллеги; ответы учётных записей за тот же период складываются.
    EventsLoaded {
        from: i64,
        to: i64,
        events: Vec<postman_calendar::Event>,
        colleagues: Vec<postman_calendar::Owner>,
        account: Option<String>,
    },
    /// Найдено в календаре по тексту `text` (S5-21); `error` — запрос не удался.
    EventsFound {
        text: String,
        events: Vec<postman_calendar::Event>,
        error: Option<String>,
    },
    /// Занятость для помощника: моя и по адресам; `None` — нет данных (календарь не открыт мне, сотрудник
    /// не найден): это не «свободен». `error` — запрос не удался целиком.
    Availability {
        ticket: u64,
        me: Vec<Busy>,
        people: Vec<(String, Option<Vec<Busy>>)>,
        error: Option<String>,
    },
    MeetingCreated {
        ticket: u64,
        uid: String,
    },
    /// Встреча изменена, изменения разосланы (`UpdateMeeting`).
    MeetingUpdated {
        ticket: u64,
        uid: String,
    },
    /// Встреча отменена, участникам ушла отмена (`CancelMeeting`).
    MeetingCancelled {
        ticket: u64,
        uid: String,
    },
    /// Ошибка создания, правки или отмены встречи с этим тикетом.
    MeetingFailed {
        ticket: u64,
        error: String,
    },
    FreeBusyShared {
        email: String,
    },
    /// Ошибка календаря (неделя, открытие занятости) — тостом.
    CalendarFailed(String),
    /// Ответ по настройкам ящика (`settings.rs`).
    Settings(settings::Reply),
    /// Личные контакты целиком (раздел «Контакты»).
    ContactsLoaded(Vec<ContactCard>),
    /// Справочник: сотрудники, переговорные и ресурсы с адресами (раздел «Контакты», подсказки адресов).
    DirectoryLoaded(Vec<Principal>),
    /// Страница справочника (`Command::DirectoryPage`, T1-25б): `people` с `offset`; `total` — всего людей, если путь
    /// знает; `search_only` — справочник у сервера только поиском (`FindPeople`), страниц не будет. Пути без
    /// справочника — пустая страница ([`Event::no_directory`]).
    DirectoryPage {
        account: Option<String>,
        offset: u32,
        people: Vec<Principal>,
        total: Option<u32>,
        search_only: bool,
    },
    /// Найдено в справочнике по строке `FindPeople` — к уже загруженному (подсказки адресов, поиск в «Контактах»).
    /// `offset` — как в команде; `more` — у сервера есть ещё найденные (следующая страница — `offset` + сколько пришло).
    PeopleFound {
        text: String,
        offset: u32,
        people: Vec<Principal>,
        more: bool,
    },
    /// Подробности человека справочника (`PersonDetails`): `None` — путь их не даёт или человек не найден.
    PersonDetails {
        email: String,
        person: Option<Principal>,
    },
    ContactSaved {
        ticket: u64,
        id: String,
    },
    ContactDeleted {
        ticket: u64,
        id: String,
    },
    /// Ошибка контактов: `ticket` — сохранения или удаления, `0` — загрузки.
    ContactFailed {
        ticket: u64,
        error: String,
    },
    /// Таблица категорий: прочитана при входе или по push, записана, снята с писем (`categories.rs`).
    Categories(categories::Reply),
    /// Задачи: из кэша и с сервера, правки (`tasks.rs`, S6-02).
    Tasks(tasks::Reply),
    /// Рассылки перенесены в «Архив» (`Command::Newsletters`): сколько.
    Newsletters {
        archived: usize,
    },
    /// Связь с сервером (S5-08, `link`): пропала (повтор через N с, правок в очереди) или вернулась.
    Connection(Connection),
    /// Сообщение о фишинге (`Command::ReportPhishing`) отправлено (`error: None`) или не отправлено. Без связи
    /// события нет — команда ждёт в очереди.
    Reported {
        ticket: u64,
        error: Option<String>,
    },
    /// Шаг поручения (`Command::Delegate`) ушёл (`error: None`) или нет. Без связи события нет — ждёт в очереди.
    /// `scheduled` — письма, которые сервер держит (окно «Отменить»).
    Delegated {
        ticket: u64,
        uid: String,
        error: Option<String>,
        scheduled: Vec<Scheduled>,
    },
    /// Письма шага не ушли (`Command::Withhold`); `too_late` — хотя бы одно уже отправлено.
    Withheld {
        ticket: u64,
        too_late: bool,
        error: Option<String>,
    },
    /// Письмо целиком (`Command::FetchRaw`); ошибка — ответ сервера или сети.
    Raw {
        letter: LetterId,
        data: std::result::Result<Arc<Vec<u8>>, String>,
    },
    /// Сервер цели принял письмо задания (`Command::Import`, `Command::ServerCopy`): `remote` — где оно у него.
    Imported {
        job: JobId,
        letter: LetterId,
        remote: Remote,
    },
    /// Сервер цели письмо не принял.
    ImportFailed {
        job: JobId,
        letter: LetterId,
        error: ImportError,
    },
    /// Сервер источника удалил письмо задания (`Command::DestroyLetters`) или его там уже нет.
    Destroyed {
        job: JobId,
        letter: LetterId,
    },
    /// Сервер источника письмо не удалил: ответ сервера или сети как есть. Задание повторит (не больше
    /// [`crate::migrate::DESTROY_ATTEMPTS`] раз).
    DestroyFailed {
        job: JobId,
        letter: LetterId,
        error: String,
    },
    /// Итог задания переноса: `letters` писем источник так и не удалил (все попытки — отказ). Они у цели (или в базе)
    /// с пометкой «не удалено у источника» — его `detached`-связью; интерфейс показывает тост с числом.
    SourceKept {
        job: JobId,
        letters: u64,
    },
    /// Обновление хранилища (перенос формата 5 в 6, ADR 0006 п. 6): писем перенесено из `total` — строка
    /// «Обновление хранилища: N %» вместо списка; первый кадр её не ждёт.
    StoreUpgrade {
        done: u64,
        total: u64,
    },
    /// Ход задания переноса: писем готово из `total`, байт выгружено, состояние.
    Migration {
        job: JobId,
        done: u64,
        total: u64,
        bytes: u64,
        state: MigrationState,
    },
    /// Ход экспорта (`Command::Export`) — не чаще 4 раз в секунду, смена состояния — сразу: писем записано из
    /// `total`, байт записано, состояние (конец — последнее событие задания).
    Export {
        job: export::ExportId,
        done: u64,
        total: u64,
        bytes: u64,
        state: export::State,
    },
    /// Папку не создать, не переименовать или не перенести (T1-04б). `mailbox` — папка команды, у `CreateFolder` —
    /// родитель (`None` — верхний уровень); `error` — ответ сервера или отказ ядра ([`FOLDER_UNSUPPORTED`],
    /// [`FOLDER_INTO_ITSELF`], [`FOLDER_OTHER_ACCOUNT`]: текст у интерфейса).
    FolderFailed {
        mailbox: Option<String>,
        error: String,
    },
    Error(SyncError),
}

/// Путь не умеет команду ([`Command::unsupported`]): над папками, письмо целиком, выгрузку.
pub const FOLDER_UNSUPPORTED: &str = "not supported";
/// Перенос папки в саму себя или в свою подпапку ([`admit`]).
pub const FOLDER_INTO_ITSELF: &str = "folder cannot be moved into itself or its subfolder";
/// Перенос папки в другой аккаунт ([`admit`]): у сервера это не перенос, а копия.
pub const FOLDER_OTHER_ACCOUNT: &str = "folder cannot be moved to another account";

impl Command {
    /// Ответ пути, который команду не умеет, — чтобы интерфейс и задание переноса не ждали: у команд над папками —
    /// `FolderFailed` «not supported», у страницы справочника — пустая страница, у письма целиком, выгрузки и
    /// удаления у источника — отказ по письму. У прочих — пусто (путь пишет в журнал).
    pub fn unsupported(&self) -> Vec<Event> {
        let not = || FOLDER_UNSUPPORTED.to_owned();
        let failed = |job: JobId, letter: LetterId| Event::ImportFailed { job, letter, error: ImportError::Rejected(not()) };
        let mailbox = match self {
            Command::CreateFolder { parent, .. } => parent.clone(),
            Command::RenameFolder { mailbox, .. } | Command::MoveFolder { mailbox, .. } => Some(mailbox.clone()),
            Command::DirectoryPage { account, offset, .. } => return vec![Event::no_directory(account.clone(), *offset)],
            Command::FetchRaw { letter } => return vec![Event::Raw { letter: *letter, data: Err(not()) }],
            Command::Import { job, letter, .. } => return vec![failed(*job, *letter)],
            Command::ServerCopy { job, letters, .. } => return letters.iter().map(|l| failed(*job, *l)).collect(),
            Command::DestroyLetters { job, letters, .. } => {
                return letters.iter().map(|l| Event::DestroyFailed { job: *job, letter: *l, error: not() }).collect();
            }
            _ => return Vec::new(),
        };
        vec![Event::FolderFailed { mailbox, error: not() }]
    }
}

impl Event {
    /// Ответ на `DirectoryPage` срезом справочника, загруженного целиком (JMAP: тот же список, что в
    /// `DirectoryLoaded`): `max` людей с `offset`, `total` — длина списка.
    pub fn directory_slice(account: Option<String>, all: &[Principal], offset: u32, max: u32) -> Event {
        let people = all.iter().skip(offset as usize).take(max as usize).cloned().collect();
        let total = Some(u32::try_from(all.len()).unwrap_or(u32::MAX));
        Event::DirectoryPage { account, offset, people, total, search_only: false }
    }

    /// Ответ пути без справочника: пустая страница, больше нет.
    pub fn no_directory(account: Option<String>, offset: u32) -> Event {
        Event::DirectoryPage { account, offset, people: Vec::new(), total: Some(0), search_only: false }
    }
}

/// Пропустить ли команду в плагин (T1-04б): перенос папки в саму себя, в свою подпапку или в другой аккаунт ядро
/// отклоняет само: `FolderFailed` — в `notify`, плагину — `None`. `folders` — дерево папок кэша (`Cache::mailboxes`),
/// читается только для переноса папки. Плагин: `let Some(cmd) = admit(cmd, || …, notify) else { continue };`.
pub fn admit(cmd: Command, folders: impl FnOnce() -> Vec<MailboxRow>, notify: &dyn Fn(Event)) -> Option<Command> {
    let Command::MoveFolder { mailbox, parent: Some(parent) } = &cmd else { return Some(cmd) };
    let error = if postman_cache::account_of(mailbox) != postman_cache::account_of(parent) {
        FOLDER_OTHER_ACCOUNT
    } else if is_within(parent, mailbox, &folders()) {
        FOLDER_INTO_ITSELF
    } else {
        return Some(cmd);
    };
    notify(Event::FolderFailed { mailbox: Some(mailbox.clone()), error: error.into() });
    None
}

/// Папка `key` — это `root` или её подпапка (вверх по `parent_id`; цикл в дереве не зацикливает).
fn is_within(key: &str, root: &str, folders: &[MailboxRow]) -> bool {
    let parent: HashMap<&str, &str> = folders.iter().filter_map(|m| Some((m.id.as_str(), m.parent_id.as_deref()?))).collect();
    let mut at = key;
    for _ in 0..=parent.len() {
        if at == root {
            return true;
        }
        match parent.get(at) {
            Some(up) => at = up,
            None => return false,
        }
    }
    false
}

/// Поисковый индекс (если открылся); пишут в него и синхронизация, и UI.
pub type Index = Option<Arc<SearchIndex>>;

// ---------------------------------------------------------------------------------------------------------------
// Общий цикл: поток учётной записи и «без связи» (ADR 0004). Плагин пути даёт свою сессию с сервером.

/// Куда поток синхронизации отдаёт события: канал интерфейса и пробуждение его цикла.
pub struct Notifier {
    events: mpsc::Sender<Event>,
    repaint: Box<dyn Fn() + Send>,
}

impl Notifier {
    pub fn new(events: mpsc::Sender<Event>, repaint: Box<dyn Fn() + Send>) -> Self {
        Self { events, repaint }
    }

    pub fn send(&self, e: Event) {
        let _ = self.events.send(e);
        (self.repaint)();
    }
}

/// Поток синхронизации учётной записи: свой рантайм tokio (один поток) и `body` в нём. Поток или рантайм не
/// запустились — ошибка видна в окне (как любая ошибка синхронизации); команды уходят в никуда, клиент работает с
/// кэшем.
pub fn spawn_thread<F, Fut>(name: &str, events: mpsc::Sender<Event>, repaint: Box<dyn Fn() + Send>, body: F)
where
    F: FnOnce(Notifier) -> Fut + Send + 'static,
    Fut: Future<Output = ()>,
{
    let failed = events.clone();
    let started = std::thread::Builder::new().name(name.into()).spawn(move || {
        let notify = Notifier::new(events, repaint);
        let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(e) => return notify.send(Event::Error(SyncError::internal(format!("tokio runtime: {e}")))),
        };
        rt.block_on(body(notify));
    });
    if let Err(e) = started {
        let _ = failed.send(Event::Error(SyncError::internal(format!("sync thread: {e}"))));
    }
}

/// Сессия с сервером одной учётной записи (плагин пути): вход и работа до конца канала команд или до ошибки.
/// Правки без связи — в `link` (их повторит сама сессия, когда связь вернётся).
pub trait Session {
    fn run<'a>(
        &'a mut self,
        rx: &'a mut UnboundedReceiver<Command>,
        link: &'a mut Link<Command>,
        notify: &'a dyn Fn(Event),
    ) -> BoxFuture<'a, Result<(), SyncError>>;
}

/// Общий цикл учётной записи: сессия; сервер не ответил при входе или первой загрузке — без связи, повтор с
/// паузой (S5-08), правки писем — в очередь (уйдут после входа). Отказ во входе и прочие ошибки останавливают
/// синхронизацию.
pub async fn supervise(session: &mut impl Session, mut rx: UnboundedReceiver<Command>, notify: &dyn Fn(Event)) {
    let mut link = Link::default();
    loop {
        match session.run(&mut rx, &mut link, notify).await {
            Ok(()) => return,
            Err(e) if e.kind == ErrorKind::Network => {
                notify(Event::Error(e));
                let wait = link.failed();
                notify(Event::Connection(link.connection(wait)));
                if !wait_offline(&mut rx, &mut link, wait, notify).await {
                    return;
                }
            }
            Err(e) => return notify(Event::Error(e)),
        }
    }
}

/// Без связи до входа: ждать паузу `wait` (или «Повторить сейчас»); правки писем — в очередь (уйдут после входа),
/// прочие команды без сервера выполнить нельзя. `false` — интерфейс закрыл канал команд: синхронизации конец.
async fn wait_offline(rx: &mut UnboundedReceiver<Command>, link: &mut Link<Command>, wait: Duration, notify: &dyn Fn(Event)) -> bool {
    let until = tokio::time::Instant::now() + wait;
    loop {
        tokio::select! {
            () = tokio::time::sleep_until(until) => return true,
            cmd = rx.recv() => match cmd {
                None => return false,
                Some(Command::Reconnect) => return true,
                Some(cmd) => {
                    if let Some(c) = cmd.replayable() {
                        link.queue(c);
                        let left = until.saturating_duration_since(tokio::time::Instant::now());
                        notify(Event::Connection(link.connection(left)));
                    }
                }
            },
        }
    }
}

/// Связь пропала (или повтор не удался): пауза до следующей попытки — интерфейсу и циклу.
pub fn went_down(link: &mut Link<Command>, notify: &impl Fn(Event)) -> tokio::time::Instant {
    let wait = link.failed();
    notify(Event::Connection(link.connection(wait)));
    tokio::time::Instant::now() + wait
}

/// Ждать срока повтора; срока нет — никогда.
pub async fn retry_at(at: Option<tokio::time::Instant>) {
    match at {
        Some(t) => tokio::time::sleep_until(t).await,
        None => std::future::pending().await,
    }
}

/// Ошибку — в интерфейс; `true` — остановить синхронизацию: сервер отклонил вход — повторять запросы с тем же
/// паролем бессмысленно (и сервер может заблокировать адрес за перебор). Интерфейс показывает строку входа.
pub fn report(e: SyncError, notify: &impl Fn(Event)) -> bool {
    let stop = e.kind == ErrorKind::Auth;
    notify(Event::Error(e));
    stop
}

/// Задержка перед перечитыванием календаря: первое изменение пачки ставит срок, следующие до срока — к нему же.
#[derive(Debug, Default)]
pub struct Debounce {
    due: Option<tokio::time::Instant>,
}

impl Debounce {
    pub fn poke(&mut self, now: tokio::time::Instant, delay: Duration) {
        self.due.get_or_insert(now + delay);
    }

    /// Срок наступил: сбросить и вернуть `true`.
    pub fn fire(&mut self, now: tokio::time::Instant) -> bool {
        let due = self.due.is_some_and(|d| d <= now);
        if due {
            self.due = None;
        }
        due
    }

    pub async fn wait(&self) {
        match self.due {
            Some(d) => tokio::time::sleep_until(d).await,
            None => std::future::pending().await,
        }
    }
}

/// События дополнительной учётной записи (S7-03б) — с владельцем: в общем календаре они рисуются цветом и подписью
/// владельца, как календари коллег (S5-22). Основная (`me: None`) — как есть.
pub fn owned_by(
    me: &Option<postman_calendar::Owner>,
    mut events: Vec<postman_calendar::Event>,
    mut colleagues: Vec<postman_calendar::Owner>,
) -> (Vec<postman_calendar::Event>, Vec<postman_calendar::Owner>) {
    if let Some(me) = me {
        for e in events.iter_mut().filter(|e| e.owner.is_none()) {
            e.owner = Some(me.clone());
        }
        if !colleagues.iter().any(|c| c.key() == me.key()) {
            colleagues.insert(0, me.clone());
        }
    }
    (events, colleagues)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Несколько учётных записей (S7-03б): команда идёт в синхронизацию учётной записи своих ключей, правки писем
    /// разных учётных записей делятся, поиск «Все папки» и неделя календаря — во все.
    #[test]
    fn commands_route_by_profile() {
        let target = |c: Command| c.routes().into_iter().map(|(t, _)| t).collect::<Vec<_>>();
        let p = |s: Option<&str>| Target::Profile(s.map(str::to_owned));
        assert_eq!(target(Command::Body("e1".into())), [p(None)]);
        assert_eq!(target(Command::Body("shared/e1".into())), [p(None)]);
        assert_eq!(target(Command::Body("@2/e1".into())), [p(Some("@2"))]);
        assert_eq!(target(Command::Body("@2.shared/e1".into())), [p(Some("@2"))]);
        assert_eq!(target(Command::Move { id: "@3/e".into(), from: "@3/a".into(), to: "@3/b".into() }), [p(Some("@3"))]);
        assert_eq!(target(Command::Search { query: "q".into(), mailbox: None, ticket: 1 }), [Target::All]);
        assert_eq!(target(Command::Search { query: "q".into(), mailbox: Some("@2/inbox".into()), ticket: 1 }), [p(Some("@2"))]);
        assert_eq!(target(Command::Events { from: 0, to: 1 }), [Target::All]);
        assert_eq!(target(Command::Contacts), [p(None)]);
        let draft = Draft { from: Address::new("", "me@b.test"), ..Default::default() };
        let send = Command::Send { draft: Box::new(draft), replace: None, ticket: 1, release: Release::Now };
        assert_eq!(target(send), [Target::Sender("me@b.test".into())]);
        let split = Command::SetKeywords { ids: vec!["a".into(), "@2/b".into(), "s/c".into()], add: vec!["k".into()], remove: vec![] }.routes();
        let got: Vec<(Target, Vec<String>)> = split
            .into_iter()
            .map(|(t, c)| match c {
                Command::SetKeywords { ids, .. } => (t, ids),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(got, [(p(None), vec!["a".to_string(), "s/c".to_string()]), (p(Some("@2")), vec!["@2/b".to_string()])]);
        assert!(Command::Events { from: 0, to: 1 }.for_each_account().is_some());
        assert!(Command::Contacts.for_each_account().is_none());
    }

    /// Команды над папками (T1-04б) — в учётную запись папки (у новой — её аккаунта), основную и дополнительную.
    #[test]
    fn folder_commands_route_by_folder() {
        let target = |c: Command| c.routes().into_iter().map(|(t, _)| t).collect::<Vec<_>>();
        let p = |s: Option<&str>| Target::Profile(s.map(str::to_owned));
        let create = |account: Option<&str>, parent: Option<&str>| Command::CreateFolder {
            account: account.map(str::to_owned),
            parent: parent.map(str::to_owned),
            name: "Новая".into(),
        };
        assert_eq!(target(create(None, None)), [p(None)]);
        assert_eq!(target(create(Some("shared"), Some("shared/f"))), [p(None)], "общий ящик основной учётной записи");
        assert_eq!(target(create(Some("@2"), None)), [p(Some("@2"))]);
        assert_eq!(target(create(Some("@2.shared"), Some("@2.shared/f"))), [p(Some("@2"))]);
        assert_eq!(target(Command::RenameFolder { mailbox: "f".into(), name: "x".into() }), [p(None)]);
        assert_eq!(target(Command::RenameFolder { mailbox: "@3/f".into(), name: "x".into() }), [p(Some("@3"))]);
        assert_eq!(target(Command::MoveFolder { mailbox: "f".into(), parent: None }), [p(None)]);
        assert_eq!(target(Command::MoveFolder { mailbox: "@2/f".into(), parent: Some("@2/g".into()) }), [p(Some("@2"))]);
        assert!(Command::MoveFolder { mailbox: "f".into(), parent: None }.for_each_account().is_none());
        // Путь без папок отвечает отказом, команда не теряется.
        let no = Command::RenameFolder { mailbox: "@2/f".into(), name: "x".into() }.unsupported();
        assert!(matches!(&no[..], [Event::FolderFailed { mailbox: Some(m), error }] if m == "@2/f" && error == FOLDER_UNSUPPORTED));
        assert!(matches!(&create(None, None).unsupported()[..], [Event::FolderFailed { mailbox: None, .. }]));
        assert!(Command::Contacts.unsupported().is_empty());
    }

    /// Перенос папки в саму себя, в подпапку или в другой аккаунт — отказ ядра, плагин команды не получает; прочее —
    /// в плагин (дерево папок для прочих команд не читается).
    #[test]
    fn folder_into_its_subfolder_is_refused_before_the_plugin() {
        let row = |id: &str, parent: Option<&str>| MailboxRow {
            id: id.into(),
            parent_id: parent.map(str::to_owned),
            name: id.into(),
            role: None,
            sort_order: 0,
            total: 0,
            unread: 0,
            rights: Default::default(),
        };
        // a → b → c; d — сосед; x ↔ y — цикл в дереве.
        let tree = vec![row("a", None), row("b", Some("a")), row("c", Some("b")), row("d", None), row("x", Some("y")), row("y", Some("x"))];
        let mv = |m: &str, to: Option<&str>| Command::MoveFolder { mailbox: m.into(), parent: to.map(str::to_owned) };
        let mut plugin = Vec::new();
        let refused = std::cell::RefCell::new(Vec::new());
        let notify = |e: Event| match e {
            Event::FolderFailed { mailbox, error } => refused.borrow_mut().push((mailbox.unwrap_or_default(), error)),
            _ => unreachable!(),
        };
        for cmd in
            [mv("a", Some("c")), mv("a", Some("a")), mv("b", Some("c")), mv("a", Some("@2/z")), mv("c", Some("d")), mv("c", None), mv("x", Some("d"))]
        {
            match admit(cmd, || tree.clone(), &notify) {
                Some(Command::MoveFolder { mailbox, parent }) => plugin.push((mailbox, parent)),
                Some(_) => unreachable!(),
                None => {}
            }
        }
        let into = FOLDER_INTO_ITSELF.to_string();
        assert_eq!(
            refused.take(),
            [("a".into(), into.clone()), ("a".into(), into.clone()), ("b".into(), into), ("a".into(), FOLDER_OTHER_ACCOUNT.to_string())]
        );
        assert_eq!(plugin, [("c".to_string(), Some("d".to_string())), ("c".into(), None), ("x".into(), Some("d".into()))]);
        assert!(admit(mv("y", Some("x")), || tree.clone(), &notify).is_none(), "цикл: x — подпапка y");
        assert_eq!(refused.take().len(), 1);
        let read = std::cell::Cell::new(false);
        let body = admit(
            Command::Body("e".into()),
            || {
                read.set(true);
                Vec::new()
            },
            &notify,
        );
        assert!(matches!(body, Some(Command::Body(_))) && !read.get());
    }

    /// Письма формата 6 (S8-08): письмо — по учётке его связи с сервером, «На этом компьютере» — в ядро; выгрузка — в
    /// поток учётки цели; перенос и задания — в ядро; прочие команды — по ключам, как раньше.
    #[test]
    fn letters_route_by_their_account() {
        struct Table;
        impl Homes for Table {
            fn letter(&self, id: LetterId) -> Home {
                match id.0 {
                    1 => Home::Local,
                    2 => Home::Account("@2".into()),
                    3 => Home::Account(String::new()),
                    _ => Home::Account("@2.shared".into()),
                }
            }
            fn folder(&self, id: FolderId) -> Home {
                match id.0 {
                    10 => Home::Local,
                    _ => Home::Account("@3".into()),
                }
            }
        }
        let route = |c: Command| c.routes_by(&Table).into_iter().map(|(r, _)| r).collect::<Vec<_>>();
        let sync = |s: Option<&str>| Route::Sync(Target::Profile(s.map(str::to_owned)));
        let raw = |n: u64| Command::FetchRaw { letter: LetterId(n) };
        assert_eq!(route(raw(1)), [Route::Core], "письмо «На этом компьютере» — без сети");
        assert_eq!(route(raw(2)), [sync(Some("@2"))]);
        assert_eq!(route(raw(3)), [sync(None)], "основная учётка");
        assert_eq!(route(raw(4)), [sync(Some("@2"))], "общий ящик учётки @2");
        let import = |folder: u64| Command::Import {
            job: JobId(1),
            letter: LetterId(2),
            folder: FolderId(folder),
            keywords: vec![],
            received_at: 0,
            message_id: None,
            retry: false,
        };
        assert_eq!(route(import(11)), [sync(Some("@3"))], "выгрузка — в поток цели, а не источника");
        let copy = Command::ServerCopy { job: JobId(1), letters: vec![LetterId(2), LetterId(3)], to: FolderId(11) };
        assert_eq!(route(copy), [sync(Some("@3"))]);
        assert_eq!(route(Command::MoveLetters { ids: vec![LetterId(2)], to: FolderId(11) }), [Route::Core]);
        assert_eq!(route(Command::MoveLetters { ids: vec![LetterId(1)], to: FolderId(10) }), [Route::Core]);
        assert_eq!(route(Command::Migration(MigrationRequest::Pause { job: JobId(1) })), [Route::Core]);
        let export =
            Command::Export { scope: export::Scope::Folder { id: FolderId(11), subfolders: true }, format: export::Format::Mbox, dir: "x".into() };
        assert_eq!(route(export), [Route::Core], "экспорт — в поток ядра, и папки учётки тоже");
        assert_eq!(route(Command::StopExport { job: export::ExportId(1) }), [Route::Core]);
        let destroy = |account: &str| Command::DestroyLetters { job: JobId(1), account: account.into(), letters: vec![LetterId(1)] };
        assert_eq!(route(destroy("@2.shared")), [sync(Some("@2"))], "удаление — у источника, письмо уже в другой учётке");
        assert_eq!(route(destroy("")), [sync(None)]);
        assert_eq!(route(Command::Body("@2/e1".into())), [sync(Some("@2"))], "прежние команды — по ключам");
        assert_eq!(route(Command::Search { query: "q".into(), mailbox: None, ticket: 1 }), [Route::Sync(Target::All)]);
        let split = Command::SetKeywords { ids: vec!["a".into(), "@2/b".into()], add: vec![], remove: vec![] };
        assert_eq!(route(split), [sync(None), sync(Some("@2"))]);
        for c in [raw(2), import(11), destroy("@2"), Command::Migration(MigrationRequest::Resume { job: JobId(1) })] {
            assert!(c.replayable().is_none() && c.for_each_account().is_none(), "задание переноса повторяет само");
        }
    }

    /// Путь без выгрузки отвечает отказом по каждому письму: задание не ждёт.
    #[test]
    fn unsupported_transfer_answers_per_letter() {
        let failed = |events: Vec<Event>| {
            events
                .into_iter()
                .map(|e| match e {
                    Event::ImportFailed { job, letter, error: ImportError::Rejected(r) } if r == FOLDER_UNSUPPORTED => (job.0, letter.0),
                    _ => unreachable!(),
                })
                .collect::<Vec<_>>()
        };
        let import = Command::Import {
            job: JobId(5),
            letter: LetterId(2),
            folder: FolderId(9),
            keywords: vec![],
            received_at: 0,
            message_id: None,
            retry: false,
        };
        assert_eq!(failed(import.unsupported()), [(5, 2)]);
        let copy = Command::ServerCopy { job: JobId(5), letters: vec![LetterId(2), LetterId(3)], to: FolderId(9) };
        assert_eq!(failed(copy.unsupported()), [(5, 2), (5, 3)]);
        let raw = Command::FetchRaw { letter: LetterId(4) }.unsupported();
        assert!(matches!(&raw[..], [Event::Raw { letter: LetterId(4), data: Err(_) }]));
        let destroy = Command::DestroyLetters { job: JobId(5), account: "@2".into(), letters: vec![LetterId(6), LetterId(7)] }.unsupported();
        assert!(matches!(&destroy[..], [Event::DestroyFailed { letter: LetterId(6), .. }, Event::DestroyFailed { letter: LetterId(7), .. }]));
        assert!(Command::MoveLetters { ids: vec![LetterId(1)], to: FolderId(1) }.unsupported().is_empty());
    }

    /// Справочник по страницам (T1-25б): команда — в свою учётную запись; JMAP отвечает срезом загруженного, путь без
    /// справочника — пустой страницей.
    #[test]
    fn directory_pages_route_and_slice() {
        let target = |c: Command| c.routes().into_iter().map(|(t, _)| t).collect::<Vec<_>>();
        let page = |account: Option<&str>| Command::DirectoryPage { account: account.map(str::to_owned), offset: 0, max: 100 };
        assert_eq!(target(page(None)), [Target::Profile(None)]);
        assert_eq!(target(page(Some("@2"))), [Target::Profile(Some("@2".into()))]);
        assert!(page(None).for_each_account().is_none());
        let all: Vec<Principal> = (0..5).map(|n| Principal { id: n.to_string(), email: Some(format!("p{n}@x.ru")), ..Default::default() }).collect();
        let ids = |e: Event| match e {
            Event::DirectoryPage { offset, people, total, search_only, .. } => {
                (offset, people.into_iter().map(|p| p.id).collect::<Vec<_>>(), total, search_only)
            }
            _ => unreachable!(),
        };
        assert_eq!(ids(Event::directory_slice(None, &all, 0, 2)), (0, vec!["0".into(), "1".into()], Some(5), false));
        assert_eq!(ids(Event::directory_slice(None, &all, 4, 2)), (4, vec!["4".into()], Some(5), false));
        assert_eq!(ids(Event::directory_slice(None, &all, 9, 2)), (9, vec![], Some(5), false));
        let none = page(Some("@2")).unsupported().pop().unwrap();
        assert!(matches!(&none, Event::DirectoryPage { account: Some(a), .. } if a == "@2"));
        assert_eq!(ids(none), (0, vec![], Some(0), false));
    }

    /// События дополнительной учётной записи — с её владельцем, как календарь коллеги; основной — как есть.
    #[test]
    fn second_account_events_have_an_owner() {
        let me = postman_calendar::Owner { email: "me@b.test".into(), name: "Я".into() };
        let raw = serde_json::json!({ "id": "e", "uid": "u", "title": "t", "start": "2026-10-23T10:00:00", "duration": "PT1H" });
        let ev = postman_calendar::Event::from_jmap(&raw, &jiff::tz::TimeZone::UTC).unwrap();
        let (events, colleagues) = owned_by(&Some(me.clone()), vec![ev.clone()], Vec::new());
        assert_eq!(events[0].owner.as_ref().map(|o| o.email.as_str()), Some("me@b.test"));
        assert_eq!(colleagues.len(), 1);
        let (events, colleagues) = owned_by(&None, vec![ev], Vec::new());
        assert!(events[0].owner.is_none() && colleagues.is_empty());
    }

    #[test]
    fn calendar_changes_are_debounced() {
        let t0 = tokio::time::Instant::now();
        let d = Duration::from_millis(1500);
        let mut db = Debounce::default();
        assert!(!db.fire(t0));
        db.poke(t0, d);
        // Следующие изменения пачки срок не сдвигают и второго события не дают.
        db.poke(t0 + Duration::from_millis(500), d);
        assert!(!db.fire(t0 + Duration::from_millis(1000)));
        assert!(db.fire(t0 + d));
        assert!(!db.fire(t0 + d * 2));
    }

    #[test]
    fn errors_have_kinds() {
        assert_eq!(ErrorKind::of(&Error::storage("table")), ErrorKind::Storage);
        assert_eq!(ErrorKind::of(&Error::Protocol("x".into())), ErrorKind::Protocol);
        let m = Error::Method { method: "JMAP Email/get".into(), kind: "serverFail".into(), description: String::new() };
        let e = SyncError::new(Stage::Refresh, &m);
        assert_eq!(e.kind, ErrorKind::Server);
        assert!(e.to_string().starts_with("Refresh failed (Server)"), "{e}");
    }

    /// Общий цикл: сессия не вошла по сети — «без связи» со сроком, правка — в очередь; «Повторить сейчас» —
    /// новая попытка; канал закрыт — конец.
    #[test]
    fn supervise_goes_offline_and_retries() {
        struct Down(u32);
        impl Session for Down {
            fn run<'a>(
                &'a mut self,
                _rx: &'a mut UnboundedReceiver<Command>,
                link: &'a mut Link<Command>,
                _notify: &'a dyn Fn(Event),
            ) -> BoxFuture<'a, Result<(), SyncError>> {
                self.0 += 1;
                let queued = link.queued();
                Box::pin(async move { Err(SyncError { kind: ErrorKind::Network, stage: Stage::Connect, detail: format!("down, queued {queued}") }) })
            }
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tx.send(Command::Destroy("e1".into())).unwrap();
        tx.send(Command::Reconnect).unwrap();
        drop(tx);
        let events = std::sync::Mutex::new(Vec::new());
        let mut down = Down(0);
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(supervise(&mut down, rx, &|e| events.lock().unwrap().push(e)));
        assert_eq!(down.0, 2, "вторая попытка — по «Повторить сейчас»");
        let events = events.into_inner().unwrap();
        let offline: Vec<usize> = events
            .iter()
            .filter_map(|e| match e {
                Event::Connection(Connection::Offline { queued, .. }) => Some(*queued),
                _ => None,
            })
            .collect();
        assert_eq!(offline, [0, 1, 1]);
    }
}
