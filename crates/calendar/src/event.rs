//! Событие JSCalendar → нормализованное событие с интервалом в UTC.

use std::collections::HashMap;

use jiff::civil::DateTime;
use jiff::tz::TimeZone;
use serde::Deserialize;
use serde_json::Value;

use crate::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EventStatus {
    #[default]
    Confirmed,
    Tentative,
    Cancelled,
}

/// Как событие влияет на занятость (JSCalendar `freeBusyStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FreeBusy {
    #[default]
    Busy,
    Free,
}

/// Ответ участника на приглашение (`participationStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Partstat {
    #[default]
    NeedsAction,
    Accepted,
    Declined,
    Tentative,
    Delegated,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Participant {
    pub name: String,
    pub email: String,
    pub owner: bool,
    pub attendee: bool,
    /// Обязательный (`required`) или по желанию (`optional`).
    pub optional: bool,
    pub status: Partstat,
}

/// Владелец чужого календаря, открытого мне (S5-22): адрес (имя аккаунта у Stalwart) и имя из справочника.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Owner {
    pub email: String,
    pub name: String,
}

impl Owner {
    /// Ключ цвета и показа: адрес в нижнем регистре.
    pub fn key(&self) -> String {
        self.email.to_lowercase()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub id: String,
    pub uid: String,
    pub calendar_ids: Vec<String>,
    pub title: String,
    pub description: String,
    pub location: String,
    pub span: Span,
    pub all_day: bool,
    pub status: EventStatus,
    pub free_busy: FreeBusy,
    pub participants: Vec<Participant>,
    /// Повторяющееся событие (сами экземпляры разворачивает сервер).
    pub recurring: bool,
    /// Метки события: JSCalendar `keywords` и `categories` (ключи со значением `true`), по алфавиту. Postman
    /// сопоставляет их с таблицей категорий (`pm.cat.<id>` или имя категории) — цвет блока в неделе.
    pub keywords: Vec<String>,
    /// Номер версии (`sequence`): организатор увеличивает его при изменении встречи.
    pub sequence: i64,
    /// Чей календарь: `None` — мой, иначе — коллеги, открывшего его мне (S5-22).
    pub owner: Option<Owner>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Raw {
    #[serde(default)]
    id: String,
    #[serde(default)]
    uid: String,
    #[serde(default)]
    calendar_ids: HashMap<String, bool>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    start: Option<String>,
    time_zone: Option<String>,
    duration: Option<String>,
    #[serde(default)]
    show_without_time: bool,
    /// Вычисляемые сервером JMAP свойства (если запрошены).
    utc_start: Option<String>,
    utc_end: Option<String>,
    status: Option<String>,
    free_busy_status: Option<String>,
    #[serde(default)]
    participants: HashMap<String, Value>,
    #[serde(default)]
    locations: HashMap<String, Value>,
    /// RFC 8984 (JMAP): массив правил.
    recurrence_rules: Option<Value>,
    /// JSCalendar 2.0 (воркер, `calcard`, — приглашения из писем): одно правило.
    recurrence_rule: Option<Value>,
    recurrence_id: Option<String>,
    #[serde(default)]
    keywords: HashMap<String, bool>,
    #[serde(default)]
    categories: HashMap<String, bool>,
    sequence: Option<i64>,
}

impl Event {
    /// Разобрать объект `CalendarEvent` из JMAP. `default_tz` — для «плавающего»
    /// времени без часового пояса (так показывает и Outlook: в зоне пользователя).
    pub fn from_jmap(v: &Value, default_tz: &TimeZone) -> Result<Self, String> {
        let raw: Raw = serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
        let span = match (&raw.utc_start, &raw.utc_end) {
            (Some(s), Some(e)) => Span::new(utc(s)?, utc(e)?),
            _ => {
                let start = raw.start.as_deref().ok_or("нет start")?;
                let local: DateTime = start.parse().map_err(|e| format!("start {start}: {e}"))?;
                let tz = match raw.time_zone.as_deref() {
                    Some(name) => TimeZone::get(name).map_err(|e| format!("часовой пояс {name}: {e}"))?,
                    None => default_tz.clone(),
                };
                let begin = local.to_zoned(tz).map_err(|e| e.to_string())?;
                let secs = raw.duration.as_deref().map(parse_duration).transpose()?.unwrap_or(0);
                let start = begin.timestamp().as_second();
                Span::new(start, start + secs)
            }
        };
        let participants = raw.participants.values().map(participant).collect();
        let location = raw.locations.values().filter_map(|l| l.get("name").and_then(Value::as_str)).collect::<Vec<_>>().join("; ");
        Ok(Event {
            id: raw.id,
            uid: raw.uid,
            calendar_ids: raw.calendar_ids.into_iter().filter(|(_, v)| *v).map(|(k, _)| k).collect(),
            title: raw.title,
            description: raw.description,
            location,
            span,
            all_day: raw.show_without_time,
            status: match raw.status.as_deref() {
                Some("tentative") => EventStatus::Tentative,
                Some("cancelled") => EventStatus::Cancelled,
                _ => EventStatus::Confirmed,
            },
            free_busy: match raw.free_busy_status.as_deref() {
                Some("free") => FreeBusy::Free,
                _ => FreeBusy::Busy,
            },
            participants,
            recurring: [raw.recurrence_rules, raw.recurrence_rule].into_iter().flatten().any(|r| !r.is_null()) || raw.recurrence_id.is_some(),
            keywords: {
                let mut k: Vec<String> = raw.keywords.into_iter().chain(raw.categories).filter(|(_, on)| *on).map(|(k, _)| k).collect();
                k.sort();
                k.dedup();
                k
            },
            sequence: raw.sequence.unwrap_or(0),
            owner: None,
        })
    }

    pub fn organizer(&self) -> Option<&Participant> {
        self.participants.iter().find(|p| p.owner)
    }
}

fn participant(v: &Value) -> Participant {
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let roles = v.get("roles");
    let role = |r: &str| roles.and_then(|x| x.get(r)).and_then(Value::as_bool).unwrap_or(false);
    // Адрес: `email`, `calendarAddress` (JSCalendar 2.0) или `sendTo.imip` = "mailto:…".
    let mailto = |m: &str| m.trim_start_matches("mailto:").trim_start_matches("MAILTO:").to_string();
    let email = match s("email") {
        e if !e.is_empty() => e,
        _ => match s("calendarAddress") {
            a if !a.is_empty() => mailto(&a),
            _ => v.get("sendTo").and_then(|t| t.get("imip")).and_then(Value::as_str).map(mailto).unwrap_or_default(),
        },
    };
    Participant {
        name: s("name"),
        email,
        owner: role("owner"),
        attendee: role("attendee") || role("required") || role("optional") || roles.is_none(),
        optional: role("optional") || role("informational"),
        status: match s("participationStatus").as_str() {
            "accepted" => Partstat::Accepted,
            "declined" => Partstat::Declined,
            "tentative" => Partstat::Tentative,
            "delegated" => Partstat::Delegated,
            _ => Partstat::NeedsAction,
        },
    }
}

fn utc(s: &str) -> Result<i64, String> {
    s.parse::<jiff::Timestamp>().map(|t| t.as_second()).map_err(|e| format!("{s}: {e}"))
}

/// Длительность ISO 8601 в смысле JSCalendar: `PnW`, `PnDTnHnMnS` → секунды.
pub fn parse_duration(s: &str) -> Result<i64, String> {
    let err = || format!("длительность «{s}»");
    let body = s.strip_prefix('P').ok_or_else(err)?;
    let (date, time) = body.split_once('T').unwrap_or((body, ""));
    let mut total = 0i64;
    let mut take = |part: &str, units: &[(char, i64)]| -> Result<(), String> {
        let mut num = String::new();
        for c in part.chars() {
            if c.is_ascii_digit() {
                num.push(c);
                continue;
            }
            let mult = units.iter().find(|(u, _)| *u == c).map(|(_, m)| *m).ok_or_else(err)?;
            let n: i64 = num.parse().map_err(|_| err())?;
            total += n * mult;
            num.clear();
        }
        if num.is_empty() { Ok(()) } else { Err(err()) }
    };
    take(date, &[('W', 7 * 86_400), ('D', 86_400)])?;
    take(time, &[('H', 3600), ('M', 60), ('S', 1)])?;
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("PT1H30M").unwrap(), 5400);
        assert_eq!(parse_duration("P1D").unwrap(), 86_400);
        assert_eq!(parse_duration("P1W").unwrap(), 604_800);
        assert_eq!(parse_duration("P1DT2H").unwrap(), 93_600);
        assert!(parse_duration("1H").is_err());
        assert!(parse_duration("PT1X").is_err());
    }

    #[test]
    fn jscalendar_event() {
        let v = json!({
            "@type": "Event",
            "id": "e1",
            "uid": "uid-1@corp.example",
            "calendarIds": { "cal1": true },
            "title": "Совещание по проекту",
            "start": "2026-10-20T10:00:00",
            "timeZone": "Europe/Moscow",
            "duration": "PT1H",
            "locations": { "l1": { "name": "Переговорная 3" } },
            "participants": {
                "p1": { "name": "Иван", "email": "ivan@corp.example", "roles": { "owner": true, "attendee": true }, "participationStatus": "accepted" },
                "p2": { "name": "Мария", "sendTo": { "imip": "mailto:maria@corp.example" }, "roles": { "attendee": true, "optional": true }, "participationStatus": "tentative" }
            }
        });
        let e = Event::from_jmap(&v, &TimeZone::UTC).unwrap();
        // 10:00 МСК = 07:00 UTC.
        assert_eq!(e.span.start, "2026-10-20T07:00:00Z".parse::<jiff::Timestamp>().unwrap().as_second());
        assert_eq!(e.span.len(), 3600);
        assert_eq!(e.location, "Переговорная 3");
        assert_eq!(e.organizer().unwrap().email, "ivan@corp.example");
        let maria = e.participants.iter().find(|p| p.name == "Мария").unwrap();
        assert_eq!(maria.email, "maria@corp.example");
        assert!(maria.optional);
        assert_eq!(maria.status, Partstat::Tentative);
        assert!(e.keywords.is_empty());
        assert_eq!(e.sequence, 0);
    }

    /// Повтор — и массивом RFC 8984 (`recurrenceRules`, JMAP), и одним правилом JSCalendar 2.0 (`recurrenceRule`,
    /// воркер — приглашения из писем); экземпляр (`recurrenceId`) — тоже повтор.
    #[test]
    fn recurrence_in_both_forms() {
        let base = || json!({ "start": "2026-10-20T10:00:00", "duration": "PT1H" });
        let with = |k: &str, v: serde_json::Value| {
            let mut e = base();
            e[k] = v;
            Event::from_jmap(&e, &TimeZone::UTC).unwrap().recurring
        };
        assert!(!Event::from_jmap(&base(), &TimeZone::UTC).unwrap().recurring);
        assert!(with("recurrenceRules", json!([{ "frequency": "weekly" }])));
        assert!(with("recurrenceRule", json!({ "frequency": "daily", "interval": 7, "count": 2 })));
        assert!(with("recurrenceId", json!("2026-10-27T10:00:00")));
        assert!(!with("recurrenceRule", serde_json::Value::Null));
    }

    /// Метки события — `keywords` и `categories` вместе (выключенные — нет), версия — `sequence`.
    #[test]
    fn keywords_categories_and_sequence() {
        let v = json!({
            "start": "2026-10-20T10:00:00", "duration": "PT1H", "sequence": 3,
            "keywords": { "pm.cat.clients": true, "old": false },
            "categories": { "Клиенты": true, "pm.cat.clients": true }
        });
        let e = Event::from_jmap(&v, &TimeZone::UTC).unwrap();
        assert_eq!(e.keywords, ["pm.cat.clients", "Клиенты"]);
        assert_eq!(e.sequence, 3);
    }

    #[test]
    fn utc_properties_win_and_floating_uses_default_zone() {
        let v = json!({ "start": "2026-10-20T10:00:00", "duration": "PT30M", "utcStart": "2026-10-20T08:00:00Z", "utcEnd": "2026-10-20T08:30:00Z" });
        let e = Event::from_jmap(&v, &TimeZone::UTC).unwrap();
        assert_eq!(e.span.len(), 1800);
        let floating = json!({ "start": "2026-10-20T10:00:00", "duration": "PT30M" });
        let msk = TimeZone::get("Europe/Moscow").unwrap();
        let e = Event::from_jmap(&floating, &msk).unwrap();
        assert_eq!(e.span.start, "2026-10-20T07:00:00Z".parse::<jiff::Timestamp>().unwrap().as_second());
    }
}

/// Новое событие для `CalendarEvent/set` (JSCalendar). Организатор — `me`;
/// приглашения уходят сервером (iTIP по JMAP своим, iMIP письмом внешним).
pub struct NewEvent<'a> {
    pub uid: &'a str,
    pub calendar_id: &'a str,
    pub title: &'a str,
    pub description: &'a str,
    pub location: &'a str,
    /// Локальное начало и зона: так событие корректно переживает смену летнего времени.
    pub start: DateTime,
    pub time_zone: &'a str,
    pub duration_secs: i64,
    pub organizer: (&'a str, &'a str),
    /// (имя, адрес, необязательный)
    pub attendees: &'a [(&'a str, &'a str, bool)],
}

impl NewEvent<'_> {
    /// Событие для `CalendarEvent/set` по черновику JMAP Calendars, как его понимает Stalwart: организатор —
    /// `organizerCalendarAddress`, адреса участников — `calendarAddress` (`mailto:`). Поля RFC 8984
    /// `replyTo`/`sendTo` сервер молча игнорирует — тогда приглашения не уходят (проверено на стенде).
    pub fn to_jmap(&self) -> Value {
        let mut participants = serde_json::Map::new();
        let (org_name, org_email) = self.organizer;
        participants.insert(
            "organizer".into(),
            serde_json::json!({
                "@type": "Participant",
                "name": org_name,
                "calendarAddress": format!("mailto:{org_email}"),
                "kind": "individual",
                "roles": { "owner": true, "attendee": true, "chair": true },
                "participationStatus": "accepted",
            }),
        );
        for (n, (name, email, optional)) in self.attendees.iter().enumerate() {
            let mut roles = serde_json::json!({ "attendee": true });
            if *optional {
                roles["optional"] = Value::Bool(true);
            }
            participants.insert(
                format!("a{n}"),
                serde_json::json!({
                    "@type": "Participant",
                    "name": name,
                    "calendarAddress": format!("mailto:{email}"),
                    "kind": "individual",
                    "roles": roles,
                    "participationStatus": "needs-action",
                    "expectReply": true,
                }),
            );
        }
        let mut v = serde_json::json!({
            "@type": "Event",
            "uid": self.uid,
            "calendarIds": { self.calendar_id: true },
            "title": self.title,
            "description": self.description,
            "start": self.start.strftime("%Y-%m-%dT%H:%M:%S").to_string(),
            "timeZone": self.time_zone,
            "duration": format_duration(self.duration_secs),
            "participants": participants,
            "organizerCalendarAddress": format!("mailto:{org_email}"),
        });
        if !self.location.is_empty() {
            v["locations"] = serde_json::json!({ "l1": { "@type": "Location", "name": self.location } });
        }
        v
    }
}

/// Секунды → `PnDTnHnMnS` (без нулевых частей).
pub fn format_duration(secs: i64) -> String {
    let (d, rem) = (secs / 86_400, secs % 86_400);
    let (h, m, s) = (rem / 3600, rem % 3600 / 60, rem % 60);
    let mut out = String::from("P");
    if d > 0 {
        out.push_str(&format!("{d}D"));
    }
    if h > 0 || m > 0 || s > 0 || d == 0 {
        out.push('T');
        if h > 0 {
            out.push_str(&format!("{h}H"));
        }
        if m > 0 {
            out.push_str(&format!("{m}M"));
        }
        if s > 0 || (h == 0 && m == 0) {
            out.push_str(&format!("{s}S"));
        }
    }
    out
}

#[cfg(test)]
mod new_event_tests {
    use super::*;

    #[test]
    fn roundtrip_through_parser() {
        let ev = NewEvent {
            uid: "u1",
            calendar_id: "cal",
            title: "Планёрка",
            description: "",
            location: "Переговорная 1",
            start: "2026-10-21T11:00:00".parse().unwrap(),
            time_zone: "Europe/Moscow",
            duration_secs: 5400,
            organizer: ("Я", "me@corp.example"),
            attendees: &[("Иван", "ivan@corp.example", false), ("Мария", "maria@corp.example", true)],
        };
        let v = ev.to_jmap();
        assert_eq!(v["duration"], "PT1H30M");
        // Stalwart: организатор и адреса — по черновику JMAP Calendars, без replyTo/sendTo.
        assert_eq!(v["organizerCalendarAddress"], "mailto:me@corp.example");
        assert_eq!(v["participants"]["a0"]["calendarAddress"], "mailto:ivan@corp.example");
        assert!(v.get("replyTo").is_none() && v["participants"]["a0"].get("sendTo").is_none());
        let back = Event::from_jmap(&v, &TimeZone::UTC).unwrap();
        assert_eq!(back.span.len(), 5400);
        assert_eq!(back.organizer().unwrap().email, "me@corp.example");
        assert_eq!(back.participants.len(), 3);
        assert!(back.participants.iter().any(|p| p.email == "maria@corp.example" && p.optional));
        assert_eq!(format_duration(86_400), "P1D");
        assert_eq!(format_duration(0), "PT0S");
        assert_eq!(parse_duration(&format_duration(93_784)).unwrap(), 93_784);
    }
}
