//! Нейтральная модель (ADR 0003, S7-02; ADR 0004): типы, которые видят `core`, плагины путей к серверам и
//! интерфейс, — без привязки к протоколу сервера. Плагины (`postman-backend-*`) переводят в них свои типы; ни
//! `core`, ни интерфейс протокольных крейтов не знают (тест `layers.rs`). Поля — те же, что были у типов JMAP,
//! поведение не меняется.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Вход на сервер.
#[derive(Clone)]
pub enum Auth {
    Basic { user: String, password: String },
    Bearer(String),
}

/// Условие отбора писем на сервере — для фильтров панели и «Все папки».
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    InMailbox(String),
    HasKeyword(String),
    NotKeyword(String),
    Text(String),
    /// Текст в From, To, теле (поиск подстроки в заголовке или тексте).
    From(String),
    To(String),
    Body(String),
    /// Текст в теме (S5-10).
    Subject(String),
    /// Есть вложение.
    HasAttachment(bool),
    /// Все условия.
    And(Vec<Filter>),
    /// Любое из условий: закладки быстрых фильтров между собой.
    Or(Vec<Filter>),
    /// Ни одно из условий: «внешние» — отправитель не из своих доменов.
    Not(Vec<Filter>),
    /// Получено раньше (UTCDate `2026-10-21T00:00:00Z`).
    Before(String),
    /// Получено не раньше (UTCDate; S5-10).
    After(String),
    /// Размер не меньше, байт («Очистить вложения…», S5-29).
    MinSize(u64),
}

/// Когда сервер выпускает письмо.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Release {
    /// Сразу.
    Now,
    /// Через столько секунд после создания отправки: окно «Отменить» (S5-06).
    After(u32),
    /// В момент (Unix-секунды): «Отправить позже».
    At(i64),
}

/// Карточка контакта. У новой (ещё не на сервере) `id` пустой.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ContactCard {
    pub id: String,
    pub name: String,
    pub emails: Vec<String>,
    pub phones: Vec<String>,
    pub organization: String,
    /// Должность.
    pub title: String,
}

impl ContactCard {
    /// Карточка из объекта JSContact (RFC 9553): ответа сервера или карточки vCard, разобранной воркером (CardDAV,
    /// S7-05б). `id` — из объекта (у CardDAV его ставит плагин — путь ресурса). Разбор — тот же, что у
    /// `postman_jmap::ContactCard::from_jscontact`: JSContact — формат модели, а не протокола (ADR 0004).
    pub fn from_jscontact(v: &Value) -> ContactCard {
        // Значения записей (`emails`, `phones`…) по предпочтению (`pref`: 1 — первое), затем по ключу.
        let entries = |field: &str| -> Vec<&Value> {
            let mut list: Vec<(&String, &Value)> = v[field].as_object().into_iter().flatten().collect();
            list.sort_by_key(|(k, e)| (e["pref"].as_u64().unwrap_or(u64::MAX), k.as_str()));
            list.into_iter().map(|(_, e)| e).collect()
        };
        let name = &v["name"];
        let full = name["full"].as_str().map(str::to_string).unwrap_or_else(|| {
            // Нет `full` — собираем из компонентов: фамилия, имя, отчество.
            let part = |kind: &str| -> Vec<&str> {
                name["components"].as_array().into_iter().flatten().filter(|c| c["kind"] == kind).filter_map(|c| c["value"].as_str()).collect()
            };
            [part("surname"), part("given"), part("given2")].concat().join(" ")
        });
        let strings =
            |field: &str, key: &str| -> Vec<String> { entries(field).into_iter().filter_map(|e| e[key].as_str()).map(str::to_string).collect() };
        let organization = entries("organizations")
            .into_iter()
            .find_map(|o| {
                let units = o["units"].as_array().into_iter().flatten().filter_map(|u| u["name"].as_str());
                let parts: Vec<&str> = o["name"].as_str().into_iter().chain(units).filter(|s| !s.is_empty()).collect();
                (!parts.is_empty()).then(|| parts.join(", "))
            })
            .unwrap_or_default();
        let titles = entries("titles");
        let title = titles
            .iter()
            .find(|t| t["kind"].as_str().unwrap_or("title") == "title")
            .or(titles.first())
            .and_then(|t| t["name"].as_str())
            .unwrap_or_default()
            .to_string();
        ContactCard {
            id: v["id"].as_str().unwrap_or_default().to_string(),
            name: full,
            emails: strings("emails", "address"),
            phones: strings("phones", "number"),
            organization,
            title,
        }
    }
}

/// Запись справочника: человек, группа, ресурс.
///
/// Должность, отдел и телефон (S7-08) — функция ядра «карточка справочника», а не деталь протокола: их дают те пути,
/// у кого они есть (EWS — `FindPeople`/`ResolveNames` из GAL); у JMAP (`Principal`, RFC 9670) таких свойств нет — пусто,
/// у CardDAV справочника нет (личные карточки — [`ContactCard`] со своими должностью, организацией и телефонами).
#[derive(Debug, Clone, Default)]
pub struct Principal {
    pub id: String,
    /// У Stalwart — логин (адрес), а не ФИО (`docs/STALWART.md` п. 9); показывать — [`Principal::display_name`].
    pub name: String,
    /// У Stalwart — ФИО учётной записи («Test user1»).
    pub description: Option<String>,
    pub email: Option<String>,
    /// individual, group, resource, location…
    pub kind: String,
    /// Должность; нет — пусто.
    pub title: String,
    /// Отдел (подразделение); нет — пусто.
    pub department: String,
    /// Рабочий телефон, иначе мобильный; нет — пусто.
    pub phone: String,
}

impl Principal {
    /// Имя для людей: `description` (ФИО), а если его нет — `name`.
    pub fn display_name(&self) -> &str {
        match self.description.as_deref().map(str::trim) {
            Some(d) if !d.is_empty() => d,
            _ => &self.name,
        }
    }
}

/// Автоответ сервера. Даты — UTCDate (`2026-10-20T00:00:00Z`); `None` — без границы.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VacationResponse {
    #[serde(default)]
    pub is_enabled: bool,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
    pub subject: Option<String>,
    pub text_body: Option<String>,
    pub html_body: Option<String>,
}

/// Род ошибки: от него зависят текст и поведение интерфейса (`Auth` — снова строка входа).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// Сервер отклонил вход (HTTP 401): пароль сменён или сессия отозвана. Синхронизация останавливается.
    Auth,
    /// Сервер недоступен: соединение, DNS, TLS, тайм-аут.
    Network,
    /// Сервер ответил ошибкой (HTTP 4xx/5xx, ошибка метода JMAP).
    Server,
    /// Ответ не тот, что ждали (разбор, нет capability, неверный адрес).
    Protocol,
    /// Локальная копия (кэш) не прочиталась или не записалась.
    Storage,
    /// Синхронизация не запустилась (поток, рантайм).
    Internal,
}

/// Ошибка пути к серверу для общего кода `core` (задачи, договор плагинов — ADR 0004). Плагин переводит в неё
/// ошибки своего протокола, сохраняя род ([`ErrorKind`]) и текст.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// Сервер отказал методу с кодом (у JMAP — `type` ошибки: `cannotCalculateChanges`, `notFound`,
    /// `stateMismatch`, `forbidden`…; другие пути переводят свои отказы в те же коды). Род — [`ErrorKind::Server`].
    #[error("{method}: {kind} {description}")]
    Method { method: String, kind: String, description: String },
    /// Ответ не тот, что ждали.
    #[error("неожиданный ответ: {0}")]
    Protocol(String),
    /// Локальная копия (кэш).
    #[error("cache: {0}")]
    Storage(String),
    /// Прочее — род и текст, как их дал плагин.
    #[error("{detail}")]
    Other { kind: ErrorKind, detail: String },
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn kind(&self) -> ErrorKind {
        match self {
            Error::Method { .. } => ErrorKind::Server,
            Error::Protocol(_) => ErrorKind::Protocol,
            Error::Storage(_) => ErrorKind::Storage,
            Error::Other { kind, .. } => *kind,
        }
    }

    /// Код отказа сервера ([`Error::Method`]).
    pub fn code(&self) -> Option<&str> {
        match self {
            Error::Method { kind, .. } => Some(kind),
            _ => None,
        }
    }

    pub fn other(kind: ErrorKind, detail: impl std::fmt::Display) -> Error {
        Error::Other { kind, detail: detail.to_string() }
    }

    /// Сеть: соединение, DNS, TLS, тайм-аут.
    pub fn network(detail: impl std::fmt::Display) -> Error {
        Error::other(ErrorKind::Network, detail)
    }

    pub fn storage(e: impl std::fmt::Display) -> Error {
        Error::Storage(e.to_string())
    }

    /// Неразборчивый JSON (как `JSON: …` у JMAP).
    pub fn json(e: serde_json::Error) -> Error {
        Error::other(ErrorKind::Protocol, format!("JSON: {e}"))
    }
}

impl From<postman_cache::Error> for Error {
    fn from(e: postman_cache::Error) -> Self {
        Error::storage(e)
    }
}

/// Локальный id письма хранилища формата 6 (ADR 0006 п. 1): не меняется ни при переносе между папками и учётками, ни
/// при выгрузке на сервер. Тот же номер, что у `postman_cache::LetterId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LetterId(pub u64);

/// Папка формата 6: учётки или «На этом компьютере». Тот же номер, что у `postman_cache::FolderId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FolderId(pub u64);

/// Задание переноса писем в другую учётку (ADR 0006 п. 4). Тот же номер, что у `postman_cache::JobId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct JobId(pub u64);

/// Режим задания переноса — ответ строки над списком папки (ADR 0006 п. 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MigrationMode {
    /// Ещё не ответили: шаг «содержимое» уже идёт (он нужен обоим режимам).
    Ask,
    /// «Перенести на сервер»: выгрузить в папку цели, затем удалить у источника.
    Upload,
    /// «Оставить локальным архивом»: письмо целиком в базе, затем удалить у источника.
    LocalArchive,
}

/// Где задание переноса.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MigrationState {
    /// Ждёт ответа ([`MigrationMode::Ask`]).
    Asking,
    Running,
    Paused,
    /// На сервере цели нет места: задание стоит до «Продолжить» или «Оставить локально».
    QuotaFull,
    Done,
    Cancelled,
}

/// Что пользователь велел заданию переноса (`sync::Command::Migration`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MigrationRequest {
    /// Ответ на вопрос строки: режим задания.
    Answer {
        job: JobId,
        mode: MigrationMode,
    },
    Pause {
        job: JobId,
    },
    /// Продолжить после паузы или после «нет места».
    Resume {
        job: JobId,
    },
    /// Остановить: `keep_local` — оставшиеся письма — локальным архивом, иначе — одной транзакцией обратно в папку
    /// источника.
    Cancel {
        job: JobId,
        keep_local: bool,
    },
}

/// Почему сервер цели не принял письмо (`sync::Event::ImportFailed`). Текст — для журнала (по-английски).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum ImportError {
    /// Больше предела сервера: письмо остаётся локальным с пометкой, очередь идёт дальше.
    #[error("too large for the server")]
    TooLarge,
    /// Нет места на сервере: задание — [`MigrationState::QuotaFull`].
    #[error("server quota exceeded")]
    Quota,
    /// Сервер просит подождать: пауза очереди, секунды.
    #[error("throttled, retry after {retry_after} s")]
    Throttled { retry_after: u64 },
    /// Прочий отказ: ответ сервера или сети как есть.
    #[error("rejected: {0}")]
    Rejected(String),
}

/// Письмо на сервере цели после выгрузки (`sync::Event::Imported`): из этого ядро пишет связь письма с сервером
/// (`postman_cache::RemoteRef`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remote {
    /// Ключ учётки (аккаунт ключей кэша: основной — пустая строка).
    pub account: String,
    /// id письма на сервере.
    pub id: String,
    /// Что ещё нужно пути (папка и UID, `ItemId`…): формат плагина.
    pub data: Vec<u8>,
}

/// Область или функция, которой у сервера учётной записи может не быть (S7, «Нет у сервера — нет на экране»).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    /// Календарь, встречи, приглашения с ответом через сервер.
    Calendar,
    /// Задачи и поручения (S6).
    Tasks,
    /// Личные контакты.
    Contacts,
    /// Справочник организации и подсказки из него.
    Directory,
    /// Занятость коллег («Помощник планирования»).
    FreeBusy,
    /// Правила почты на сервере.
    Rules,
    /// Автоответ на сервере.
    Autoreply,
    /// Квота ящика (`settings::Request::Quota` → `Usage`): перед переносом на сервер и по ходу. У кого её нет —
    /// места не хватит только по ошибке выгрузки ([`ImportError::Quota`]).
    Quota,
    /// Сервер хранит ключевые слова писем: категории, сроки, «Ждём» (JMAP — всегда; IMAP — `PERMANENTFLAGS \*`).
    Keywords,
}

impl Feature {
    pub const ALL: [Feature; 9] = [
        Feature::Calendar,
        Feature::Tasks,
        Feature::Contacts,
        Feature::Directory,
        Feature::FreeBusy,
        Feature::Rules,
        Feature::Autoreply,
        Feature::Quota,
        Feature::Keywords,
    ];

    const fn bit(self) -> u16 {
        1 << self as u16
    }
}

/// Что умеет сервер учётной записи (ADR 0003). Пока сервер не ответил — [`Features::ALL`]: интерфейс не прячет
/// разделы на время входа.
///
/// Перенос писем между учётками (ADR 0006 п. 4) — не раздел интерфейса, а умение пути: [`Features::import`] и
/// [`Features::server_copy`]; в [`Features::ALL`] их нет, пока путь сам не сказал.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Features(u16);

/// Биты умений переноса — после битов [`Feature`].
const IMPORT: u16 = 1 << 14;
const SERVER_COPY: u16 = 1 << 15;

impl Features {
    pub const ALL: Features = {
        let mut bits = 0;
        let mut i = 0;
        while i < Feature::ALL.len() {
            bits |= Feature::ALL[i].bit();
            i += 1;
        }
        Features(bits)
    };
    pub const NONE: Features = Features(0);

    pub fn has(self, f: Feature) -> bool {
        self.0 & f.bit() != 0
    }

    #[must_use]
    pub fn with(self, f: Feature, on: bool) -> Features {
        self.with_bits(f.bit(), on)
    }

    /// Есть хоть у одного: области учётной записи от нескольких плагинов (почта IMAP, календарь CalDAV; ADR 0004).
    #[must_use]
    pub fn union(self, o: Features) -> Features {
        Features(self.0 | o.0)
    }

    /// Путь умеет выгрузку письма в папку на сервере (`sync::Command::Import`).
    pub fn import(self) -> bool {
        self.0 & IMPORT != 0
    }

    #[must_use]
    pub fn with_import(self, on: bool) -> Features {
        self.with_bits(IMPORT, on)
    }

    /// Сервер сам копирует письма между аккаунтами одной сессии (`sync::Command::ServerCopy`, без выгрузки).
    pub fn server_copy(self) -> bool {
        self.0 & SERVER_COPY != 0
    }

    #[must_use]
    pub fn with_server_copy(self, on: bool) -> Features {
        self.with_bits(SERVER_COPY, on)
    }

    fn with_bits(self, bits: u16, on: bool) -> Features {
        if on { Features(self.0 | bits) } else { Features(self.0 & !bits) }
    }
}

const _: () = assert!(Features::ALL.0 & (IMPORT | SERVER_COPY) == 0, "биты переноса заняты областью");

impl Default for Features {
    fn default() -> Self {
        Features::ALL
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn features_set_and_clear() {
        assert!(Feature::ALL.iter().all(|&f| Features::ALL.has(f) && !Features::NONE.has(f)));
        let f = Features::ALL.with(Feature::Calendar, false).with(Feature::Rules, false);
        assert!(!f.has(Feature::Calendar) && !f.has(Feature::Rules) && f.has(Feature::Tasks));
        assert_eq!(f.with(Feature::Calendar, true).with(Feature::Rules, true), Features::ALL);
        assert_eq!(Features::default(), Features::ALL);
        let cal = Features::NONE.with(Feature::Calendar, true);
        assert_eq!(cal.union(Features::NONE.with(Feature::Rules, true)), cal.with(Feature::Rules, true));
    }

    /// Умения переноса — свои биты: в `ALL` их нет, области они не трогают и объединяются, как области.
    #[test]
    fn features_import_and_server_copy() {
        assert!(!Features::ALL.import() && !Features::ALL.server_copy() && !Features::default().import());
        let f = Features::NONE.with(Feature::Keywords, true).with_import(true);
        assert!(f.import() && !f.server_copy() && f.has(Feature::Keywords) && !f.has(Feature::Calendar));
        assert_eq!(f.with(Feature::Keywords, false), Features::NONE.with_import(true));
        let both = f.union(Features::NONE.with_server_copy(true));
        assert!(both.import() && both.server_copy());
        assert_eq!(both.with_import(false).with_server_copy(false), Features::NONE.with(Feature::Keywords, true));
        assert_eq!(Features::ALL.with_import(true).with_import(false), Features::ALL);
    }

    /// Типы переноса — в журнал и в задание (JSON туда и обратно); текст ошибки выгрузки — по-английски.
    #[test]
    fn migration_types_serialize() {
        fn json(v: &impl Serialize) -> String {
            serde_json::to_string(v).unwrap()
        }
        assert_eq!(json(&MigrationState::QuotaFull), r#""QuotaFull""#);
        assert_eq!(json(&ImportError::Throttled { retry_after: 30 }), r#"{"Throttled":{"retry_after":30}}"#);
        assert_eq!(json(&ImportError::Rejected("550".into())), r#"{"Rejected":"550"}"#);
        assert_eq!(json(&MigrationRequest::Cancel { job: JobId(7), keep_local: true }), r#"{"Cancel":{"job":7,"keep_local":true}}"#);
        assert_eq!(json(&Remote { account: "@2".into(), id: "m1".into(), data: vec![1] }), r#"{"account":"@2","id":"m1","data":[1]}"#);
        let back: MigrationRequest = serde_json::from_str(r#"{"Answer":{"job":3,"mode":"LocalArchive"}}"#).unwrap();
        assert_eq!(back, MigrationRequest::Answer { job: JobId(3), mode: MigrationMode::LocalArchive });
        let e: ImportError = serde_json::from_str(r#""Quota""#).unwrap();
        assert_eq!(e, ImportError::Quota);
        assert_eq!(ImportError::Throttled { retry_after: 5 }.to_string(), "throttled, retry after 5 s");
        assert_eq!(ImportError::TooLarge.to_string(), "too large for the server");
        assert_eq!((LetterId(1), FolderId(2)), serde_json::from_str("[1,2]").unwrap());
    }

    #[test]
    fn errors_keep_kind_and_code() {
        let m = Error::Method { method: "JMAP Email/get".into(), kind: "serverFail".into(), description: String::new() };
        assert_eq!((m.kind(), m.code()), (ErrorKind::Server, Some("serverFail")));
        assert_eq!(m.to_string(), "JMAP Email/get: serverFail ");
        assert_eq!(Error::Protocol("x".into()).kind(), ErrorKind::Protocol);
        assert_eq!(Error::storage("table").kind(), ErrorKind::Storage);
        assert_eq!(Error::network("HTTP: refused").to_string(), "HTTP: refused");
        assert_eq!(Error::network("x").kind(), ErrorKind::Network);
    }

    #[test]
    fn jscontact_card() {
        let v = serde_json::json!({
            "id": "c1",
            "name": { "components": [{ "kind": "surname", "value": "Иванова" }, { "kind": "given", "value": "Анна" }] },
            "emails": { "e2": { "address": "b@example.test" }, "e1": { "address": "a@example.test", "pref": 1 } },
            "phones": { "p1": { "number": "+7 900" } },
            "organizations": { "o1": { "name": "ИТТ", "units": [{ "name": "ИТ" }] } },
            "titles": { "t1": { "name": "инженер" } }
        });
        let c = ContactCard::from_jscontact(&v);
        assert_eq!((c.id.as_str(), c.name.as_str()), ("c1", "Иванова Анна"));
        assert_eq!((c.organization.as_str(), c.title.as_str()), ("ИТТ, ИТ", "инженер"));
        assert_eq!(c.emails, ["a@example.test", "b@example.test"]);
        assert_eq!(c.phones, ["+7 900"]);
    }
}
