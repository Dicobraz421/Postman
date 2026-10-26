//! Задача (S6-02): JSCalendar `Task` (JSCalendar 2.0, как его хранит Stalwart — `CalendarEvent` с `@type: Task`,
//! `docs/STALWART.md` п. 37–40) ↔ модель Postman ↔ VTODO (iCalendar, RFC 5545). Без I/O.
//!
//! Поля Postman, которых нет в JSCalendar (проект, «Когда-нибудь», порядок подзадач, письмо задачи, дата
//! выполнения), — свойства `X-POSTMAN-*` и COMPLETED в `iCalendar.properties` (jCal). Остальные чужие свойства
//! оттуда сохраняются как есть и пишутся обратно ([`Task::extra`]).

use std::collections::BTreeMap;

use jiff::civil::{Date, DateTime};
use jiff::tz::TimeZone;
use serde_json::{Map, Value, json};

use crate::event::{format_duration, parse_duration};
use crate::invite::{fold, text};

/// Свойства задачи для `CalendarEvent/get`: `iCalendar` по умолчанию сервер не отдаёт.
pub const TASK_PROPERTIES: &[&str] = &[
    "id",
    "@type",
    "uid",
    "calendarIds",
    "title",
    "description",
    "start",
    "due",
    "timeZone",
    "showWithoutTime",
    "estimatedDuration",
    "priority",
    "progress",
    "percentComplete",
    "keywords",
    "recurrenceRule",
    "alerts",
    "relatedTo",
    "links",
    "organizerCalendarAddress",
    "participants",
    "sequence",
    "updated",
    "iCalendar",
];

/// Имена своих свойств iCalendar (нижний регистр — так их отдаёт jCal).
pub const X_PROJECT: &str = "x-postman-project";
pub const X_SOMEDAY: &str = "x-postman-someday";
pub const X_ORDER: &str = "x-postman-order";
pub const X_MESSAGE_ID: &str = "x-postman-message-id";
pub const X_EMAIL_ID: &str = "x-postman-email-id";
/// «Время в календаре» (S6-08): `<начало UTC>/<конец UTC>/<id события>`.
pub const X_BLOCK: &str = "x-postman-block";
/// Повтор «через N дней после выполнения» (S6-07): в RRULE такого нет.
pub const X_REPEAT_AFTER: &str = "x-postman-repeat-after";
const COMPLETED: &str = "completed";
const OURS: &[&str] = &[X_PROJECT, X_SOMEDAY, X_ORDER, X_MESSAGE_ID, X_EMAIL_ID, X_BLOCK, X_REPEAT_AFTER, COMPLETED];

/// Ход задачи (JSCalendar `progress`, iCalendar STATUS).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum Progress {
    #[default]
    NeedsAction,
    InProcess,
    Completed,
    Failed,
    Cancelled,
}

impl Progress {
    pub fn from_json(s: &str) -> Self {
        match s {
            "in-process" => Self::InProcess,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => Self::NeedsAction,
        }
    }

    pub fn json(self) -> &'static str {
        match self {
            Self::NeedsAction => "needs-action",
            Self::InProcess => "in-process",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn ical(self) -> &'static str {
        match self {
            Self::NeedsAction => "NEEDS-ACTION",
            Self::InProcess => "IN-PROCESS",
            Self::Completed => "COMPLETED",
            Self::Failed => "FAILED",
            Self::Cancelled => "CANCELLED",
        }
    }

    /// Задача закрыта: выполнена, не удалась или отменена — в «Выполненных», не в работе.
    pub fn is_closed(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// Момент задачи (начало, срок): местное время и пояс; без пояса — «плавающее» (в поясе пользователя).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Moment {
    pub local: DateTime,
    pub tz: Option<String>,
}

impl Moment {
    pub fn new(local: DateTime, tz: Option<&str>) -> Self {
        Self { local, tz: tz.map(str::to_owned) }
    }

    fn zone(&self, default: &TimeZone) -> TimeZone {
        self.tz.as_deref().and_then(|n| TimeZone::get(n).ok()).unwrap_or_else(|| default.clone())
    }

    /// Unix-секунды; «плавающее» — в поясе `default`.
    pub fn timestamp(&self, default: &TimeZone) -> Option<i64> {
        self.local.to_zoned(self.zone(default)).ok().map(|z| z.timestamp().as_second())
    }

    /// День момента для того, кто смотрит из пояса `viewer` (срок 01:00 по Москве — ещё вчера по Лондону).
    pub fn date_in(&self, viewer: &TimeZone) -> Option<Date> {
        let t = self.local.to_zoned(self.zone(viewer)).ok()?;
        Some(t.with_time_zone(viewer.clone()).date())
    }
}

/// От чего отсчитывается напоминание.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Anchor {
    Start,
    Due,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Trigger {
    /// За `secs` секунд до (отрицательное) или после якоря.
    Offset { secs: i64, from: Anchor },
    /// В момент (Unix-секунды).
    At(i64),
}

/// Напоминание (`alerts`, VALARM). Письмо сервер шлёт только по `email` от начала (`docs/STALWART.md` п. 41) —
/// остальные показывает Postman (S6-07).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Alert {
    pub trigger: Trigger,
    pub email: bool,
}

/// Письмо, из которого задача: `Message-ID` (виден другим клиентам ссылкой `mid:`; пустой — ещё не известен) и
/// id письма в ящике.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MailLink {
    pub message_id: String,
    pub email_id: Option<String>,
}

/// «Время в календаре» (S6-08): блок «занят» в своём календаре под задачу — событие `event_id`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TimeBlock {
    pub event_id: String,
    /// Unix-секунды.
    pub start: i64,
    pub end: i64,
}

impl TimeBlock {
    fn to_x(&self) -> String {
        format!("{}/{}/{}", utc_ical(self.start), utc_ical(self.end), self.event_id)
    }

    fn from_x(s: &str) -> Option<Self> {
        let mut it = s.splitn(3, '/');
        let start = ical_utc(it.next()?)?;
        let end = ical_utc(it.next()?)?;
        let event_id = it.next()?.to_owned();
        (end > start && !event_id.is_empty()).then_some(Self { event_id, start, end })
    }
}

/// Роль в поручении (S6-09).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// Поручивший (`roles.owner`, ORGANIZER).
    Owner,
    /// Исполнитель.
    Assignee,
    /// Наблюдатель (`roles.informational`, `ROLE=NON-PARTICIPANT`).
    Observer,
}

/// Ответ участника поручения: PARTSTAT iCalendar для VTODO (RFC 5545 §3.2.12). В JSCalendar 2.0 «в работе» и
/// «готово» — `progress` участника, остальное — `participationStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum TaskPartstat {
    #[default]
    NeedsAction,
    Accepted,
    Declined,
    Tentative,
    Delegated,
    InProcess,
    Completed,
}

impl TaskPartstat {
    pub(crate) fn ical(self) -> &'static str {
        match self {
            Self::NeedsAction => "NEEDS-ACTION",
            Self::Accepted => "ACCEPTED",
            Self::Declined => "DECLINED",
            Self::Tentative => "TENTATIVE",
            Self::Delegated => "DELEGATED",
            Self::InProcess => "IN-PROCESS",
            Self::Completed => "COMPLETED",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TaskParticipant {
    /// Ключ в `participants` (сохраняется при записи — патчи участника идут по нему).
    pub key: String,
    pub name: String,
    pub email: String,
    pub role: Role,
    pub status: TaskPartstat,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    /// id на сервере (`CalendarEvent.id`); у новой, ещё не записанной — пусто.
    pub id: String,
    pub uid: String,
    pub calendar_ids: Vec<String>,
    pub title: String,
    /// Заметки — Markdown как текст (`description`).
    pub notes: String,
    pub start: Option<Moment>,
    pub due: Option<Moment>,
    /// Срок без времени (`showWithoutTime`).
    pub all_day: bool,
    /// Оценка («Время в календаре», S6-08), секунды (`estimatedDuration`).
    pub estimate: Option<i64>,
    /// «!»: приоритет 1–4 (RFC 5545: 1 — высший). Другого приоритета в Postman нет.
    pub important: bool,
    pub progress: Progress,
    pub percent: Option<u8>,
    /// Когда выполнена (COMPLETED; сервер её не ставит — клиент, `docs/STALWART.md` п. 38).
    pub completed_at: Option<i64>,
    /// Категории (`keywords`, CATEGORIES) — та же таблица, что у почты.
    pub keywords: Vec<String>,
    /// Повтор (`recurrenceRule` JSCalendar 2.0) как есть; разбор — `postman_core::recur` (S6-07).
    pub recurrence: Option<Value>,
    /// Повтор через столько дней после выполнения (`X-POSTMAN-REPEAT-AFTER`).
    pub repeat_after: Option<u32>,
    pub alerts: Vec<Alert>,
    /// Родительская задача (подзадача): UID родителя (`relatedTo` с `parent`, RELATED-TO;RELTYPE=PARENT).
    pub parent: Option<String>,
    /// Порядок подзадачи у родителя.
    pub order: Option<i64>,
    pub project: Option<String>,
    pub someday: bool,
    pub mail: Option<MailLink>,
    pub participants: Vec<TaskParticipant>,
    /// «Время в календаре» (S6-08).
    pub block: Option<TimeBlock>,
    pub sequence: i64,
    /// Чужие свойства iCalendar (jCal) — пишутся обратно как были.
    pub extra: Vec<Value>,
}

impl Task {
    /// Новая задача с UID `uid` в календаре `calendar`.
    pub fn new(uid: &str, calendar: &str, title: &str) -> Self {
        Self {
            id: String::new(),
            uid: uid.into(),
            calendar_ids: if calendar.is_empty() { Vec::new() } else { vec![calendar.into()] },
            title: title.into(),
            notes: String::new(),
            start: None,
            due: None,
            all_day: false,
            estimate: None,
            important: false,
            progress: Progress::NeedsAction,
            percent: None,
            completed_at: None,
            keywords: Vec::new(),
            recurrence: None,
            repeat_after: None,
            alerts: Vec::new(),
            parent: None,
            order: None,
            project: None,
            someday: false,
            mail: None,
            participants: Vec::new(),
            block: None,
            sequence: 0,
            extra: Vec::new(),
        }
    }

    /// Это задача, а не событие: `@type: Task`.
    pub fn is_task(v: &Value) -> bool {
        v["@type"] == "Task"
    }

    /// Разобрать объект JSCalendar `Task` (JMAP `CalendarEvent` или результат воркера по VTODO).
    pub fn from_jmap(v: &Value) -> Result<Self, String> {
        if !Self::is_task(v) {
            return Err(format!("не задача: @type {}", v["@type"]));
        }
        let s = |k: &str| v[k].as_str().unwrap_or("").to_owned();
        let tz = v["timeZone"].as_str();
        let moment = |k: &str| -> Result<Option<Moment>, String> {
            v[k].as_str().map(|x| x.parse::<DateTime>().map(|d| Moment::new(d, tz)).map_err(|e| format!("{k} «{x}»: {e}"))).transpose()
        };
        let mut ical: BTreeMap<String, String> = BTreeMap::new();
        let mut extra = Vec::new();
        for p in v["iCalendar"]["properties"].as_array().into_iter().flatten() {
            let name = p[0].as_str().unwrap_or("").to_ascii_lowercase();
            if OURS.contains(&name.as_str()) {
                ical.insert(name, value_text(&p[3]));
            } else {
                extra.push(p.clone());
            }
        }
        let mail_link = v["links"]
            .as_object()
            .into_iter()
            .flat_map(|m| m.values())
            .filter_map(|l| l["href"].as_str())
            .find_map(|h| h.strip_prefix("mid:").map(|m| m.trim_start_matches('<').trim_end_matches('>').to_owned()));
        let message_id = ical.get(X_MESSAGE_ID).map(|m| m.trim_start_matches('<').trim_end_matches('>').to_owned()).or(mail_link);
        let email_id = ical.get(X_EMAIL_ID).filter(|x| !x.is_empty()).cloned();
        let message_id = message_id.filter(|m| !m.is_empty());
        // Ссылка — по Message-ID (видна другим клиентам) и/или по id письма в ящике (Message-ID ещё не известен:
        // тело письма не загружено).
        let mail = (message_id.is_some() || email_id.is_some()).then(|| MailLink { message_id: message_id.unwrap_or_default(), email_id });
        let parent = v["relatedTo"].as_object().into_iter().flatten().find(|(_, r)| r["relation"]["parent"] == true).map(|(uid, _)| uid.clone());
        let mut keywords: Vec<String> =
            v["keywords"].as_object().into_iter().flatten().filter(|(_, on)| **on == true).map(|(k, _)| k.clone()).collect();
        keywords.sort();
        let priority = v["priority"].as_i64().unwrap_or(0);
        let mut calendar_ids: Vec<String> =
            v["calendarIds"].as_object().into_iter().flatten().filter(|(_, on)| **on == true).map(|(k, _)| k.clone()).collect();
        calendar_ids.sort();
        let organizer = v["organizerCalendarAddress"].as_str().map(mailto);
        let mut participants: Vec<TaskParticipant> =
            v["participants"].as_object().into_iter().flatten().map(|(k, p)| participant(k, p, organizer.as_deref())).collect();
        participants.sort_by(|a, b| (a.role as u8, &a.email).cmp(&(b.role as u8, &b.email)));
        Ok(Self {
            id: s("id"),
            uid: s("uid"),
            calendar_ids,
            title: s("title"),
            notes: s("description"),
            start: moment("start")?,
            due: moment("due")?,
            all_day: v["showWithoutTime"].as_bool().unwrap_or(false),
            estimate: v["estimatedDuration"].as_str().map(parse_duration).transpose()?,
            important: (1..=4).contains(&priority),
            progress: v["progress"].as_str().map(Progress::from_json).unwrap_or_default(),
            percent: v["percentComplete"].as_u64().map(|p| p.min(100) as u8),
            completed_at: ical.get(COMPLETED).and_then(|c| ical_utc(c)),
            keywords,
            recurrence: v.get("recurrenceRule").filter(|r| r.is_object()).cloned(),
            repeat_after: ical.get(X_REPEAT_AFTER).and_then(|n| n.trim().parse().ok()).filter(|n| *n > 0),
            alerts: v["alerts"].as_object().into_iter().flat_map(|m| m.values()).filter_map(alert).collect(),
            parent,
            order: ical.get(X_ORDER).and_then(|o| o.trim().parse().ok()),
            project: ical.get(X_PROJECT).filter(|p| !p.is_empty()).cloned(),
            someday: ical.get(X_SOMEDAY).is_some_and(|x| x.trim() == "1" || x.eq_ignore_ascii_case("true")),
            mail,
            participants,
            block: ical.get(X_BLOCK).and_then(|b| TimeBlock::from_x(b)),
            sequence: v["sequence"].as_i64().unwrap_or(0),
            extra,
        })
    }

    /// Задачи из ответа воркера по .ics (`{ method, data }`): одна задача или группа.
    pub fn from_worker(v: &Value) -> Vec<Self> {
        let data = &v["data"];
        let items: Vec<&Value> = match data["entries"].as_array() {
            Some(e) => e.iter().collect(),
            None => vec![data],
        };
        items.into_iter().filter(|x| Self::is_task(x)).filter_map(|x| Self::from_jmap(x).ok()).collect()
    }

    /// Поручивший (ORGANIZER).
    pub fn organizer(&self) -> Option<&TaskParticipant> {
        self.participants.iter().find(|p| p.role == Role::Owner)
    }

    pub fn assignees(&self) -> impl Iterator<Item = &TaskParticipant> {
        self.participants.iter().filter(|p| p.role == Role::Assignee)
    }

    /// Мне поручено: в задаче есть поручивший, и это не я.
    pub fn is_assigned_to(&self, me: &str) -> bool {
        self.organizer().is_some_and(|o| !o.email.eq_ignore_ascii_case(me)) && self.assignees().any(|p| p.email.eq_ignore_ascii_case(me))
    }

    /// Я поручил: я — поручивший, исполнители есть.
    pub fn is_delegated_by(&self, me: &str) -> bool {
        self.organizer().is_some_and(|o| o.email.eq_ignore_ascii_case(me)) && self.assignees().next().is_some()
    }

    /// Срок в Unix-секундах (без времени — конец дня, 23:59:59, в поясе `tz`).
    pub fn due_at(&self, tz: &TimeZone) -> Option<i64> {
        let due = self.due.as_ref()?;
        if self.all_day {
            let day = due.local.date();
            let end = day.at(23, 59, 59, 0);
            return Moment { local: end, tz: due.tz.clone() }.timestamp(tz);
        }
        due.timestamp(tz)
    }

    /// Выполнить (`true`) или вернуть в работу: ход, процент и дата выполнения — вместе.
    pub fn set_done(&mut self, done: bool, now: i64) {
        if done {
            self.progress = Progress::Completed;
            self.percent = Some(100);
            self.completed_at = Some(now);
        } else {
            self.progress = Progress::NeedsAction;
            self.percent = None;
            self.completed_at = None;
        }
    }

    /// Свои свойства и чужие — список `iCalendar.properties` (jCal).
    fn ical_properties(&self) -> Vec<Value> {
        let mut out = self.extra.clone();
        let mut push = |name: &str, value: Value| out.push(json!([name, {}, "unknown", value]));
        if let Some(p) = &self.project {
            push(X_PROJECT, json!(p));
        }
        if self.someday {
            push(X_SOMEDAY, json!("1"));
        }
        if let Some(o) = self.order {
            push(X_ORDER, json!(o.to_string()));
        }
        if let Some(m) = &self.mail {
            if !m.message_id.is_empty() {
                push(X_MESSAGE_ID, json!(format!("<{}>", m.message_id)));
            }
            if let Some(e) = &m.email_id {
                push(X_EMAIL_ID, json!(e));
            }
        }
        if let Some(b) = &self.block {
            push(X_BLOCK, json!(b.to_x()));
        }
        if let Some(n) = self.repeat_after {
            push(X_REPEAT_AFTER, json!(n.to_string()));
        }
        if let Some(t) = self.completed_at {
            push(COMPLETED, json!(utc_ical(t)));
        }
        out
    }

    /// Объект JSCalendar для `CalendarEvent/set create` (без `id`).
    pub fn to_jmap(&self) -> Value {
        let mut v = Map::new();
        v.insert("@type".into(), json!("Task"));
        v.insert("uid".into(), json!(self.uid));
        let cals: Map<String, Value> = self.calendar_ids.iter().map(|c| (c.clone(), json!(true))).collect();
        v.insert("calendarIds".into(), Value::Object(cals));
        v.insert("title".into(), json!(self.title));
        v.insert("description".into(), json!(self.notes));
        // Пояс у задачи один (JSCalendar `timeZone`): берётся у срока, иначе у начала.
        let tz = self.due.as_ref().and_then(|d| d.tz.clone()).or_else(|| self.start.as_ref().and_then(|s| s.tz.clone()));
        let fmt = |m: &Moment| m.local.strftime("%Y-%m-%dT%H:%M:%S").to_string();
        v.insert("start".into(), self.start.as_ref().map_or(Value::Null, |m| json!(fmt(m))));
        v.insert("due".into(), self.due.as_ref().map_or(Value::Null, |m| json!(fmt(m))));
        v.insert("timeZone".into(), tz.map_or(Value::Null, Value::from));
        v.insert("showWithoutTime".into(), json!(self.all_day));
        v.insert("estimatedDuration".into(), self.estimate.map_or(Value::Null, |s| json!(format_duration(s))));
        v.insert("priority".into(), json!(if self.important { 1 } else { 0 }));
        v.insert("progress".into(), json!(self.progress.json()));
        v.insert("percentComplete".into(), self.percent.map_or(Value::Null, |p| json!(p)));
        let kws: Map<String, Value> = self.keywords.iter().map(|k| (k.clone(), json!(true))).collect();
        v.insert("keywords".into(), if kws.is_empty() { Value::Null } else { Value::Object(kws) });
        v.insert("recurrenceRule".into(), self.recurrence.clone().unwrap_or(Value::Null));
        let alerts: Map<String, Value> = self.alerts.iter().enumerate().map(|(i, a)| (format!("a{}", i + 1), alert_json(a))).collect();
        v.insert("alerts".into(), if alerts.is_empty() { Value::Null } else { Value::Object(alerts) });
        v.insert(
            "relatedTo".into(),
            self.parent.as_ref().map_or(Value::Null, |p| json!({ p: { "@type": "Relation", "relation": { "parent": true } } })),
        );
        v.insert(
            "links".into(),
            self.mail
                .as_ref()
                .filter(|m| !m.message_id.is_empty())
                .map_or(Value::Null, |m| json!({ "mail": { "@type": "Link", "href": format!("mid:{}", m.message_id), "rel": "describedby" } })),
        );
        if !self.participants.is_empty() {
            if let Some(o) = self.organizer() {
                v.insert("organizerCalendarAddress".into(), json!(format!("mailto:{}", o.email)));
            }
            let parts: Map<String, Value> = self.participants.iter().map(|p| (p.key.clone(), participant_json(p))).collect();
            v.insert("participants".into(), Value::Object(parts));
        }
        v.insert("iCalendar".into(), json!({ "name": "vtodo", "properties": self.ical_properties() }));
        // Пустые поля при создании не нужны: `null` — только в патче.
        v.retain(|_, x| !x.is_null());
        Value::Object(v)
    }

    /// Патч `CalendarEvent/set update`: только изменившиеся свойства (`null` — убрать). Свои поля — путём
    /// `iCalendar/properties`: весь `iCalendar` сервер заменил бы целиком (`docs/STALWART.md` п. 38).
    pub fn patch(&self, old: &Task) -> Map<String, Value> {
        let (new, was) = (self.to_jmap(), old.to_jmap());
        let mut out = Map::new();
        let keys: std::collections::BTreeSet<&String> =
            new.as_object().into_iter().flatten().chain(was.as_object().into_iter().flatten()).map(|(k, _)| k).collect();
        for k in keys {
            if k == "uid" || k == "@type" {
                continue;
            }
            let (a, b) = (new.get(k), was.get(k));
            if a == b {
                continue;
            }
            if k == "iCalendar" {
                let props = a.and_then(|x| x.get("properties")).cloned().unwrap_or_else(|| json!([]));
                // Своих свойств не было — у сервера объекта `iCalendar` может не быть, путь в него не применится
                // (`invalidProperties`): весь объект целиком.
                if old.ical_properties().is_empty() {
                    out.insert("iCalendar".into(), json!({ "name": "vtodo", "properties": props }));
                } else {
                    out.insert("iCalendar/properties".into(), props);
                }
            } else {
                out.insert(k.clone(), a.cloned().unwrap_or(Value::Null));
            }
        }
        out
    }

    /// VTODO (iCalendar): `method` — для iTIP (REQUEST, REPLY, CANCEL…, S6-09), `None` — для экспорта. Время —
    /// в UTC (VTIMEZONE не нужен), срок без времени — `VALUE=DATE`, «плавающее» — без пояса.
    pub fn to_ics(&self, method: Option<&str>, now: i64, default_tz: &TimeZone) -> String {
        let mut l = vec!["BEGIN:VCALENDAR".to_string(), "PRODID:-//Postman//Postman//RU".into(), "VERSION:2.0".into()];
        if let Some(m) = method {
            l.push(format!("METHOD:{m}"));
        }
        l.push("BEGIN:VTODO".into());
        l.push(format!("UID:{}", text(&self.uid)));
        l.push(format!("DTSTAMP:{}", utc_ical(now)));
        if self.sequence > 0 || method.is_some() {
            l.push(format!("SEQUENCE:{}", self.sequence));
        }
        l.push(format!("SUMMARY:{}", text(&self.title)));
        if !self.notes.is_empty() {
            l.push(format!("DESCRIPTION:{}", text(&self.notes)));
        }
        let moment = |name: &str, m: &Moment| -> Option<String> {
            if self.all_day {
                return Some(format!("{name};VALUE=DATE:{}", m.local.date().strftime("%Y%m%d")));
            }
            match &m.tz {
                None => Some(format!("{name}:{}", m.local.strftime("%Y%m%dT%H%M%S"))),
                Some(_) => m.timestamp(default_tz).map(|t| format!("{name}:{}", utc_ical(t))),
            }
        };
        l.extend(self.start.as_ref().and_then(|m| moment("DTSTART", m)));
        l.extend(self.due.as_ref().and_then(|m| moment("DUE", m)));
        if let Some(e) = self.estimate {
            l.push(format!("ESTIMATED-DURATION:{}", format_duration(e)));
        }
        if self.important {
            l.push("PRIORITY:1".into());
        }
        l.push(format!("STATUS:{}", self.progress.ical()));
        if let Some(p) = self.percent {
            l.push(format!("PERCENT-COMPLETE:{p}"));
        }
        if let Some(t) = self.completed_at {
            l.push(format!("COMPLETED:{}", utc_ical(t)));
        }
        if !self.keywords.is_empty() {
            l.push(format!("CATEGORIES:{}", self.keywords.iter().map(|k| text(k)).collect::<Vec<_>>().join(",")));
        }
        if let Some(r) = self.recurrence.as_ref().and_then(rrule) {
            l.push(format!("RRULE:{r}"));
        }
        if let Some(p) = &self.parent {
            l.push(format!("RELATED-TO;RELTYPE=PARENT:{}", text(p)));
        }
        if let Some(p) = &self.project {
            l.push(format!("X-POSTMAN-PROJECT:{}", text(p)));
        }
        if self.someday {
            l.push("X-POSTMAN-SOMEDAY:1".into());
        }
        if let Some(o) = self.order {
            l.push(format!("X-POSTMAN-ORDER:{o}"));
        }
        if let Some(m) = &self.mail {
            if !m.message_id.is_empty() {
                l.push(format!("X-POSTMAN-MESSAGE-ID:<{}>", text(&m.message_id)));
                l.push(format!("URL:mid:{}", m.message_id));
            }
            if let Some(e) = &m.email_id {
                l.push(format!("X-POSTMAN-EMAIL-ID:{}", text(e)));
            }
        }
        if let Some(b) = &self.block {
            l.push(format!("X-POSTMAN-BLOCK:{}", text(&b.to_x())));
        }
        if let Some(n) = self.repeat_after {
            l.push(format!("X-POSTMAN-REPEAT-AFTER:{n}"));
        }
        for p in &self.participants {
            let cn = cn(&p.name);
            match p.role {
                Role::Owner => l.push(format!("ORGANIZER{cn}:mailto:{}", p.email)),
                Role::Assignee => l.push(format!("ATTENDEE;PARTSTAT={};RSVP=TRUE{cn}:mailto:{}", p.status.ical(), p.email)),
                Role::Observer => l.push(format!("ATTENDEE;ROLE=NON-PARTICIPANT;PARTSTAT={}{cn}:mailto:{}", p.status.ical(), p.email)),
            }
        }
        for a in &self.alerts {
            l.push("BEGIN:VALARM".into());
            l.push(format!("ACTION:{}", if a.email { "EMAIL" } else { "DISPLAY" }));
            match a.trigger {
                Trigger::Offset { secs, from } => {
                    let rel = if from == Anchor::Due { ";RELATED=END" } else { "" };
                    l.push(format!("TRIGGER{rel}:{}", signed_duration(secs)));
                }
                Trigger::At(t) => l.push(format!("TRIGGER;VALUE=DATE-TIME:{}", utc_ical(t))),
            }
            l.push(format!("DESCRIPTION:{}", text(&self.title)));
            if a.email {
                l.push(format!("SUMMARY:{}", text(&self.title)));
            }
            l.push("END:VALARM".into());
        }
        l.extend(["END:VTODO".to_string(), "END:VCALENDAR".into()]);
        l.iter().map(|x| fold(x)).collect::<Vec<_>>().join("\r\n") + "\r\n"
    }
}

pub(crate) fn mailto(a: &str) -> String {
    match a.get(..7) {
        Some(p) if p.eq_ignore_ascii_case("mailto:") => a[7..].to_owned(),
        _ => a.to_owned(),
    }
}

fn participant(key: &str, p: &Value, organizer: Option<&str>) -> TaskParticipant {
    let email = ["calendarAddress", "email"]
        .iter()
        .filter_map(|k| p[*k].as_str())
        .chain(p["sendTo"]["imip"].as_str())
        .map(mailto)
        .find(|e| !e.is_empty())
        .unwrap_or_default();
    let roles = &p["roles"];
    let role = if roles["owner"] == true || organizer.is_some_and(|o| o.eq_ignore_ascii_case(&email)) {
        Role::Owner
    } else if roles["informational"] == true {
        Role::Observer
    } else {
        Role::Assignee
    };
    let status = match (p["progress"].as_str(), p["participationStatus"].as_str()) {
        (Some("completed"), _) => TaskPartstat::Completed,
        (Some("in-process"), _) | (_, Some("in-process")) => TaskPartstat::InProcess,
        (_, Some("completed")) => TaskPartstat::Completed,
        (_, Some("accepted")) => TaskPartstat::Accepted,
        (_, Some("declined")) => TaskPartstat::Declined,
        (_, Some("tentative")) => TaskPartstat::Tentative,
        (_, Some("delegated")) => TaskPartstat::Delegated,
        _ => TaskPartstat::NeedsAction,
    };
    TaskParticipant { key: key.to_owned(), name: p["name"].as_str().unwrap_or("").to_owned(), email, role, status }
}

fn participant_json(p: &TaskParticipant) -> Value {
    let mut v = json!({ "@type": "Participant", "calendarAddress": format!("mailto:{}", p.email) });
    if !p.name.is_empty() {
        v["name"] = json!(p.name);
    }
    match p.role {
        Role::Owner => v["roles"] = json!({ "owner": true }),
        Role::Observer => v["roles"] = json!({ "informational": true }),
        // Роль исполнителя — по умолчанию: явный `attendee` Stalwart пишет лишним JSPROP (`docs/STALWART.md` п. 40).
        Role::Assignee => v["expectReply"] = json!(true),
    }
    if p.role != Role::Owner {
        let (status, progress) = match p.status {
            TaskPartstat::NeedsAction => ("needs-action", None),
            TaskPartstat::Accepted => ("accepted", None),
            TaskPartstat::Declined => ("declined", None),
            TaskPartstat::Tentative => ("tentative", None),
            TaskPartstat::Delegated => ("delegated", None),
            TaskPartstat::InProcess => ("accepted", Some("in-process")),
            TaskPartstat::Completed => ("accepted", Some("completed")),
        };
        v["participationStatus"] = json!(status);
        if let Some(pr) = progress {
            v["progress"] = json!(pr);
        }
    }
    v
}

fn alert(a: &Value) -> Option<Alert> {
    let t = &a["trigger"];
    let trigger = match t["@type"].as_str() {
        Some("AbsoluteTrigger") => Trigger::At(t["when"].as_str()?.parse::<jiff::Timestamp>().ok()?.as_second()),
        _ => Trigger::Offset { secs: signed(t["offset"].as_str()?)?, from: if t["relativeTo"] == "end" { Anchor::Due } else { Anchor::Start } },
    };
    Some(Alert { trigger, email: a["action"] == "email" })
}

fn alert_json(a: &Alert) -> Value {
    let trigger = match a.trigger {
        Trigger::Offset { secs, from } => json!({
            "@type": "OffsetTrigger",
            "offset": signed_duration(secs),
            "relativeTo": if from == Anchor::Due { "end" } else { "start" },
        }),
        Trigger::At(t) => json!({ "@type": "AbsoluteTrigger", "when": utc_json(t) }),
    };
    json!({ "@type": "Alert", "trigger": trigger, "action": if a.email { "email" } else { "display" } })
}

/// `-PT30M` → −1800.
fn signed(s: &str) -> Option<i64> {
    let (neg, body) = match s.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let v = parse_duration(body).ok()?;
    Some(if neg { -v } else { v })
}

fn signed_duration(secs: i64) -> String {
    let d = format_duration(secs.abs());
    if secs < 0 { format!("-{d}") } else { d }
}

/// Значение jCal как текст (строка, число или массив значений).
fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(value_text).collect::<Vec<_>>().join(","),
        Value::Null => String::new(),
        x => x.to_string(),
    }
}

pub(crate) fn utc_ical(t: i64) -> String {
    jiff::Timestamp::from_second(t).map(|t| t.strftime("%Y%m%dT%H%M%SZ").to_string()).unwrap_or_default()
}

fn utc_json(t: i64) -> String {
    jiff::Timestamp::from_second(t).map(|t| t.strftime("%Y-%m-%dT%H:%M:%SZ").to_string()).unwrap_or_default()
}

/// `20261021T100000Z` или `2026-10-21T10:00:00Z` → Unix-секунды.
pub(crate) fn ical_utc(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Ok(t) = s.parse::<jiff::Timestamp>() {
        return Some(t.as_second());
    }
    let local = s.trim_end_matches('Z');
    let dt = jiff::civil::DateTime::strptime("%Y%m%dT%H%M%S", local).ok()?;
    dt.to_zoned(TimeZone::UTC).ok().map(|z| z.timestamp().as_second())
}

pub(crate) fn cn(name: &str) -> String {
    let n: String = name.chars().filter(|c| !matches!(c, '"' | '\r' | '\n')).collect();
    if n.is_empty() { String::new() } else { format!(";CN=\"{n}\"") }
}

/// `recurrenceRule` JSCalendar → RRULE (то подмножество, что пишет Postman, S6-07: частота, интервал, дни недели,
/// число месяца, месяцы, число повторов, до даты).
fn rrule(r: &Value) -> Option<String> {
    let freq = r["frequency"].as_str()?.to_ascii_uppercase();
    let mut parts = vec![format!("FREQ={freq}")];
    if let Some(i) = r["interval"].as_u64().filter(|i| *i > 1) {
        parts.push(format!("INTERVAL={i}"));
    }
    if let Some(days) = r["byDay"].as_array().filter(|d| !d.is_empty()) {
        let list: Vec<String> = days
            .iter()
            .filter_map(|d| {
                let day = d["day"].as_str()?.to_ascii_uppercase();
                Some(match d["nthOfPeriod"].as_i64() {
                    Some(n) => format!("{n}{day}"),
                    None => day,
                })
            })
            .collect();
        parts.push(format!("BYDAY={}", list.join(",")));
    }
    for (key, name) in [("byMonthDay", "BYMONTHDAY"), ("byMonth", "BYMONTH"), ("bySetPosition", "BYSETPOS")] {
        if let Some(a) = r[key].as_array().filter(|a| !a.is_empty()) {
            parts.push(format!("{name}={}", a.iter().map(value_text).collect::<Vec<_>>().join(",")));
        }
    }
    if let Some(c) = r["count"].as_u64() {
        parts.push(format!("COUNT={c}"));
    }
    if let Some(u) = r["until"].as_str().and_then(|u| u.parse::<DateTime>().ok()) {
        parts.push(format!("UNTIL={}", u.strftime("%Y%m%dT%H%M%S")));
    }
    Some(parts.join(";"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moscow() -> TimeZone {
        TimeZone::get("Europe/Moscow").unwrap()
    }

    fn full() -> Value {
        json!({
            "id": "cp", "@type": "Task", "uid": "u1@itt.test", "calendarIds": { "b": true },
            "title": "Отправить редакцию договора", "description": "Заметки **Markdown**",
            "start": "2027-03-01T09:00:00", "due": "2027-03-02T16:00:00", "timeZone": "Europe/Moscow",
            "estimatedDuration": "PT1H30M", "priority": 1, "percentComplete": 20, "progress": "in-process",
            "keywords": { "pm.cat.clients": true },
            "recurrenceRule": { "frequency": "monthly", "byMonthDay": [20] },
            "alerts": { "a1": { "@type": "Alert", "action": "display", "trigger": { "@type": "OffsetTrigger", "offset": "-PT30M", "relativeTo": "end" } } },
            "relatedTo": { "parent@itt.test": { "relation": { "parent": true } } },
            "links": { "mail": { "@type": "Link", "href": "mid:letter@itt.test", "rel": "describedby" } },
            "sequence": 2,
            "iCalendar": { "name": "vtodo", "convertedProperties": {}, "properties": [
                ["x-postman-project", {}, "unknown", "vec"],
                ["x-postman-order", {}, "unknown", "3"],
                ["x-postman-email-id", {}, "unknown", "Mabc"],
                ["completed", {}, "unknown", "20261021T100000Z"],
                ["x-moz-generation", {}, "unknown", "4"],
            ] },
        })
    }

    #[test]
    fn parses_all_card_fields() {
        let t = Task::from_jmap(&full()).unwrap();
        assert_eq!((t.id.as_str(), t.uid.as_str(), t.calendar_ids.as_slice()), ("cp", "u1@itt.test", &["b".to_string()][..]));
        assert_eq!(t.title, "Отправить редакцию договора");
        assert_eq!(t.notes, "Заметки **Markdown**");
        let due = t.due.clone().unwrap();
        assert_eq!(due.tz.as_deref(), Some("Europe/Moscow"));
        assert_eq!(
            t.due_at(&TimeZone::UTC),
            Some(jiff::civil::date(2027, 3, 2).at(13, 0, 0, 0).to_zoned(TimeZone::UTC).unwrap().timestamp().as_second())
        );
        assert_eq!(t.estimate, Some(5400));
        assert!(t.important);
        assert_eq!((t.progress, t.percent), (Progress::InProcess, Some(20)));
        assert_eq!(t.keywords, ["pm.cat.clients"]);
        assert_eq!(t.recurrence, Some(json!({ "frequency": "monthly", "byMonthDay": [20] })));
        assert_eq!(t.alerts, [Alert { trigger: Trigger::Offset { secs: -1800, from: Anchor::Due }, email: false }]);
        assert_eq!(t.parent.as_deref(), Some("parent@itt.test"));
        assert_eq!((t.order, t.project.as_deref()), (Some(3), Some("vec")));
        assert_eq!(t.mail, Some(MailLink { message_id: "letter@itt.test".into(), email_id: Some("Mabc".into()) }));
        assert_eq!(t.completed_at, ical_utc("20261021T100000Z"));
        assert_eq!(t.extra, [json!(["x-moz-generation", {}, "unknown", "4"])], "чужое X-свойство сохраняется");
        assert_eq!(t.sequence, 2);
        assert!(Task::from_jmap(&json!({ "@type": "Event", "title": "x" })).is_err());
    }

    /// JSCalendar → модель → JSCalendar → модель: то же самое; чужие свойства — на месте.
    #[test]
    fn jmap_round_trip() {
        let t = Task::from_jmap(&full()).unwrap();
        let mut back = Task::from_jmap(&{
            let mut v = t.to_jmap();
            v["id"] = json!("cp");
            v["sequence"] = json!(2);
            v
        })
        .unwrap();
        back.participants.clear();
        assert_eq!(back, t);
        let v = t.to_jmap();
        assert_eq!(v["links"]["mail"]["href"], "mid:letter@itt.test");
        assert!(v["iCalendar"]["properties"].as_array().unwrap().contains(&json!(["x-moz-generation", {}, "unknown", "4"])));
        assert!(v.get("id").is_none() && v.get("organizerCalendarAddress").is_none());
    }

    /// Патч — только изменения; свои поля — одним путём `iCalendar/properties`; снятое поле — `null`.
    #[test]
    fn patch_has_only_changes() {
        let old = Task::from_jmap(&full()).unwrap();
        let mut t = old.clone();
        assert!(t.patch(&old).is_empty());
        t.set_done(true, 1_800_000_000);
        t.project = Some("soc".into());
        t.important = false;
        t.estimate = None;
        let p = t.patch(&old);
        let mut keys: Vec<&str> = p.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, ["estimatedDuration", "iCalendar/properties", "percentComplete", "priority", "progress"]);
        assert_eq!(p["estimatedDuration"], Value::Null);
        assert_eq!(p["progress"], "completed");
        assert_eq!(p["priority"], 0);
        let props = p["iCalendar/properties"].as_array().unwrap();
        assert!(props.contains(&json!(["x-postman-project", {}, "unknown", "soc"])));
        assert!(props.contains(&json!(["completed", {}, "unknown", "20270115T080000Z"])));
    }

    /// Своих свойств не было: у сервера может не быть объекта `iCalendar` — патч целиком, а не путём внутрь.
    #[test]
    fn patch_without_ical_replaces_whole_object() {
        let old = Task::new("u", "b", "x");
        let mut t = old.clone();
        t.project = Some("p".into());
        let p = t.patch(&old);
        assert_eq!(p["iCalendar"]["properties"], json!([["x-postman-project", {}, "unknown", "p"]]));
        assert!(p.get("iCalendar/properties").is_none());
    }

    #[test]
    fn due_dates_and_floating_time() {
        let mut t = Task::new("u", "b", "x");
        t.due = Some(Moment::new("2027-03-02T01:00:00".parse().unwrap(), Some("Europe/Moscow")));
        // 01:00 по Москве — ещё 1 марта в UTC.
        assert_eq!(t.due.as_ref().unwrap().date_in(&TimeZone::UTC), Some(jiff::civil::date(2027, 3, 1)));
        assert_eq!(t.due.as_ref().unwrap().date_in(&moscow()), Some(jiff::civil::date(2027, 3, 2)));
        // Без времени: срок — конец дня.
        t.all_day = true;
        t.due = Some(Moment::new("2027-03-02T00:00:00".parse().unwrap(), None));
        let end = jiff::civil::date(2027, 3, 2).at(23, 59, 59, 0).to_zoned(moscow()).unwrap().timestamp().as_second();
        assert_eq!(t.due_at(&moscow()), Some(end));
    }

    #[test]
    fn delegation_roles_and_statuses() {
        let v = json!({
            "@type": "Task", "uid": "d1", "title": "Поручение", "organizerCalendarAddress": "mailto:boss@itt.test",
            "participants": {
                "o": { "calendarAddress": "mailto:boss@itt.test", "roles": { "owner": true } },
                "p": { "calendarAddress": "mailto:me@itt.test", "name": "Я", "participationStatus": "accepted", "progress": "in-process", "expectReply": true },
                "w": { "calendarAddress": "mailto:obs@itt.test", "roles": { "informational": true }, "participationStatus": "needs-action" },
            },
        });
        let t = Task::from_jmap(&v).unwrap();
        assert_eq!(t.organizer().map(|o| o.email.as_str()), Some("boss@itt.test"));
        let me = t.assignees().next().unwrap();
        assert_eq!((me.email.as_str(), me.status), ("me@itt.test", TaskPartstat::InProcess));
        assert!(t.participants.iter().any(|p| p.role == Role::Observer && p.email == "obs@itt.test"));
        assert!(t.is_assigned_to("ME@itt.test") && !t.is_delegated_by("me@itt.test"));
        assert!(t.is_delegated_by("boss@itt.test"));
        let back = t.to_jmap();
        assert_eq!(back["organizerCalendarAddress"], "mailto:boss@itt.test");
        assert_eq!(back["participants"]["p"]["progress"], "in-process");
        assert!(back["participants"]["p"].get("roles").is_none(), "роль исполнителя — по умолчанию");
        assert_eq!(back["participants"]["w"]["roles"], json!({ "informational": true }));
        let ics = t.to_ics(Some("REQUEST"), 0, &TimeZone::UTC);
        assert!(ics.contains("ORGANIZER:mailto:boss@itt.test"), "{ics}");
        assert!(ics.contains("ATTENDEE;PARTSTAT=IN-PROCESS;RSVP=TRUE;CN=\"Я\":mailto:me@itt.test"), "{ics}");
        assert!(ics.contains("ATTENDEE;ROLE=NON-PARTICIPANT;PARTSTAT=NEEDS-ACTION:mailto:obs@itt.test"), "{ics}");
    }

    #[test]
    fn ics_lines() {
        let t = Task::from_jmap(&full()).unwrap();
        let ics = t.to_ics(None, 1_792_000_000, &TimeZone::UTC);
        for line in [
            "BEGIN:VTODO",
            "UID:u1@itt.test",
            "SUMMARY:Отправить редакцию договора",
            "DTSTART:20270301T060000Z",
            "DUE:20270302T130000Z",
            "ESTIMATED-DURATION:PT1H30M",
            "PRIORITY:1",
            "STATUS:IN-PROCESS",
            "PERCENT-COMPLETE:20",
            "COMPLETED:20261021T100000Z",
            "CATEGORIES:pm.cat.clients",
            "RRULE:FREQ=MONTHLY;BYMONTHDAY=20",
            "RELATED-TO;RELTYPE=PARENT:parent@itt.test",
            "X-POSTMAN-PROJECT:vec",
            "X-POSTMAN-ORDER:3",
            "X-POSTMAN-MESSAGE-ID:<letter@itt.test>",
            "X-POSTMAN-EMAIL-ID:Mabc",
            "TRIGGER;RELATED=END:-PT30M",
        ] {
            assert!(ics.contains(&format!("{line}\r\n")), "нет «{line}» в\n{ics}");
        }
        assert!(!ics.contains("METHOD:"));
        assert!(ics.lines().all(|l| l.len() <= 75));
        let mut d = Task::new("u2", "", "День");
        d.all_day = true;
        d.due = Some(Moment::new("2027-03-10T00:00:00".parse().unwrap(), None));
        assert!(d.to_ics(None, 0, &TimeZone::UTC).contains("DUE;VALUE=DATE:20270310\r\n"));
    }

    #[test]
    fn time_block_round_trip() {
        let mut t = Task::new("u", "b", "x");
        t.block = Some(TimeBlock { event_id: "ev1".into(), start: 1_800_000_000, end: 1_800_003_600 });
        let back = Task::from_jmap(&t.to_jmap()).unwrap();
        assert_eq!(back.block, t.block);
        assert!(t.to_ics(None, 0, &TimeZone::UTC).contains("X-POSTMAN-BLOCK:20270115T080000Z/20270115T090000Z/ev1"));
        assert_eq!(TimeBlock::from_x("bad"), None);
    }

    #[test]
    fn rrule_subset() {
        let r = json!({ "frequency": "weekly", "interval": 2, "byDay": [{ "day": "mo" }, { "day": "fr" }], "count": 10 });
        assert_eq!(rrule(&r).as_deref(), Some("FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,FR;COUNT=10"));
        let r = json!({ "frequency": "monthly", "byDay": [{ "day": "tu", "nthOfPeriod": 2 }], "until": "2027-12-31T00:00:00" });
        assert_eq!(rrule(&r).as_deref(), Some("FREQ=MONTHLY;BYDAY=2TU;UNTIL=20271231T000000"));
        assert_eq!(signed("-PT30M"), Some(-1800));
        assert_eq!(signed_duration(-1800), "-PT30M");
        assert_eq!(signed_duration(86_400), "P1D");
    }

    fn corpus(name: &str) -> (Value, Task) {
        let path = format!("{}/../worker/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        let v = postman_worker::parse_calendar(&std::fs::read(path).unwrap()).unwrap();
        let mut list = Task::from_worker(&v);
        assert_eq!(list.len(), 1, "{name}: {v}");
        (v, list.remove(0))
    }

    /// Задача → VTODO Postman → тот же разбор, что у входящего .ics (воркер) → та же задача.
    fn round_trip(t: &Task) -> Task {
        let ics = t.to_ics(Some("REQUEST"), 1_792_000_000, &moscow());
        let v = postman_worker::parse_calendar(ics.as_bytes()).unwrap();
        let back = Task::from_worker(&v);
        assert_eq!(
            back.len(),
            1,
            "{ics}
{v}"
        );
        let b = back.into_iter().next().unwrap();
        assert_eq!(b.uid, t.uid);
        assert_eq!(b.title, t.title);
        assert_eq!(b.notes.trim_end(), t.notes.trim_end());
        assert_eq!(b.due_at(&moscow()), t.due_at(&moscow()), "срок: {ics}");
        assert_eq!(b.start.as_ref().and_then(|m| m.timestamp(&moscow())), t.start.as_ref().and_then(|m| m.timestamp(&moscow())));
        assert_eq!(b.all_day, t.all_day);
        assert_eq!((b.progress, b.percent, b.important), (t.progress, t.percent, t.important));
        assert_eq!(b.completed_at, t.completed_at);
        assert_eq!(b.keywords, t.keywords);
        assert_eq!(b.parent, t.parent);
        assert_eq!(b.recurrence, t.recurrence);
        assert_eq!(b.alerts.len(), t.alerts.len());
        assert_eq!((b.project.clone(), b.someday, b.order, b.mail.clone()), (t.project.clone(), t.someday, t.order, t.mail.clone()));
        b
    }

    /// Корпус: VTODO Thunderbird (Lightning), Apple «Напоминания» (iOS), экспорт задачи Outlook.
    #[test]
    fn corpus_thunderbird() {
        let (_, t) = corpus("todo-thunderbird.ics");
        assert_eq!(t.title, "Проверить методику оценки рисков");
        assert!(t.notes.starts_with("Разделы 3–5, сверить с ГОСТ."), "{}", t.notes);
        assert_eq!((t.progress, t.percent, t.important), (Progress::InProcess, Some(40), true));
        assert_eq!(t.keywords, ["Клиенты"]);
        let due = jiff::civil::date(2026, 10, 22).at(18, 0, 0, 0).to_zoned(moscow()).unwrap().timestamp().as_second();
        assert_eq!(t.due_at(&TimeZone::UTC), Some(due));
        assert_eq!(t.alerts, [Alert { trigger: Trigger::Offset { secs: -3600, from: Anchor::Due }, email: false }]);
        assert!(t.extra.iter().any(|p| p[0] == "x-moz-generation"), "{:?}", t.extra);
        round_trip(&t);
    }

    #[test]
    fn corpus_apple() {
        let (_, t) = corpus("todo-apple.ics");
        assert_eq!(t.title, "Продлить лицензии SIEM");
        assert!(t.all_day);
        assert_eq!(t.due.as_ref().unwrap().local.date(), jiff::civil::date(2026, 10, 23));
        assert!(!t.important, "PRIORITY:9 — низкий, не «!»");
        assert_eq!((t.progress, t.percent), (Progress::Completed, Some(100)));
        assert_eq!(t.completed_at, ical_utc("20261021T061500Z"));
        assert_eq!(t.parent.as_deref(), Some("9A8B7C6D-5E4F-4A3B-2C1D-0E9F8A7B6C5D"));
        assert_eq!(t.recurrence.as_ref().unwrap()["frequency"], "monthly");
        round_trip(&t);
    }

    #[test]
    fn corpus_outlook() {
        let (_, t) = corpus("todo-outlook.ics");
        assert_eq!(t.title, "Сверить счета");
        assert!(t.all_day);
        assert_eq!(t.due.as_ref().unwrap().local.date(), jiff::civil::date(2026, 10, 23));
        assert_eq!(t.start.as_ref().unwrap().local.date(), jiff::civil::date(2026, 10, 19));
        assert!(!t.important, "PRIORITY:5 — обычная");
        assert_eq!(t.progress, Progress::NeedsAction);
        let at = ical_utc("20261023T060000Z").unwrap();
        assert_eq!(t.alerts, [Alert { trigger: Trigger::At(at), email: false }]);
        round_trip(&t);
    }

    /// Задача Postman со всеми полями — через VTODO и обратно (поручение письмом, S6-09).
    #[test]
    fn postman_task_through_vtodo() {
        let mut t = Task::from_jmap(&full()).unwrap();
        t.mail = Some(MailLink { message_id: "letter@itt.test".into(), email_id: None });
        t.someday = true;
        let b = round_trip(&t);
        assert_eq!(b.estimate, Some(5400));
    }
}
