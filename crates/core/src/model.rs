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
    /// Квота ящика.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Features(u16);

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
        if on { Features(self.0 | f.bit()) } else { Features(self.0 & !f.bit()) }
    }

    /// Есть хоть у одного: области учётной записи от нескольких плагинов (почта IMAP, календарь CalDAV; ADR 0004).
    #[must_use]
    pub fn union(self, o: Features) -> Features {
        Features(self.0 | o.0)
    }
}

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
