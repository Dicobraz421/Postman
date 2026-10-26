//! Календарь по JMAP (S3-05): неделя из `CalendarEvent/query` (повторения разворачивает сервер), занятость коллег
//! из `Principal/getAvailability`, встреча с приглашениями (`sendSchedulingMessages`), открытие своей занятости
//! коллеге (`Calendar/set shareWith`), правка и отмена своей встречи (`update` с `recurrenceOverrides` у экземпляра,
//! `destroy`; S7-07б). Поведение сервера — `docs/STALWART.md`. Встреча и разбор недели —
//! `postman_core::schedule`.
//!
//! Сетевые функции проверяются тестом `stand`. Вызывает их поток синхронизации (`sync`).

use jiff::tz::TimeZone;
use postman_calendar::{Busy, BusyKind, Event, Owner};
pub use postman_core::schedule::Meeting;
use postman_core::schedule::{SEARCH_LIMIT, busy_from_events, parse_events, utc};
use postman_jmap::{BusyPeriod, Calendar, Client, Error, Identity, Result};
use serde_json::{Map, Value};

/// Ответ `Principal/getAvailability` → занятость. Пустой список — `None`, «нет данных»: Stalwart отдаёт пустой
/// список без ошибки, если календарь не открыт (`docs/STALWART.md` п. 5), — это не «свободен».
pub fn busy_from_periods(periods: &[BusyPeriod]) -> Option<Vec<Busy>> {
    let busy: Vec<Busy> = periods
        .iter()
        .filter_map(|p| {
            let span = postman_calendar::Span::new(ts(&p.utc_start)?, ts(&p.utc_end)?);
            let kind = match p.busy_status.as_deref() {
                Some("tentative") => BusyKind::Tentative,
                Some("unavailable") => BusyKind::Unavailable,
                _ => BusyKind::Busy,
            };
            Some(Busy { span, kind })
        })
        .collect();
    (!busy.is_empty()).then_some(busy)
}

fn ts(s: &str) -> Option<i64> {
    s.parse::<jiff::Timestamp>().ok().map(|t| t.as_second())
}

/// Календарь для новых встреч: первый видимый по порядку.
pub fn default_calendar(calendars: &[Calendar]) -> Option<&Calendar> {
    calendars.iter().min_by_key(|c| (!c.is_visible, c.sort_order))
}

/// События в `[from, to)`, повторения развёрнуты сервером.
pub async fn week(client: &Client, from: i64, to: i64) -> Result<Vec<Event>> {
    let values = client.events(None, &utc(from), &utc(to)).await?;
    Ok(parse_events(&values, &TimeZone::system()))
}

/// Неделя с календарями коллег (S5-22): мои события и события календарей, открытых мне (`mayReadItems`) — отдельные
/// аккаунты сессии с календарями (`docs/STALWART.md` п. 44); у их событий — владелец (адрес аккаунта и имя из
/// справочника). Календарь коллеги, который не ответил, пропускается: его неделя — без него. Коллеги — все открывшие,
/// и без событий в неделе (список слева в календаре).
pub async fn week_with_colleagues(client: &Client, from: i64, to: i64) -> Result<(Vec<Event>, Vec<Owner>)> {
    let mut events = week(client, from, to).await?;
    let mut colleagues = Vec::new();
    let tz = TimeZone::system();
    for (account, email) in client.session().shared_calendar_accounts() {
        let name = match client.principal_by_email(&email).await {
            Ok(Some(p)) => p.display_name().to_owned(),
            _ => email.clone(),
        };
        let owner = Owner { email, name };
        if let Ok(values) = client.events_of(&account, None, &utc(from), &utc(to)).await {
            // Встреча, где и я участник, уже есть в моём календаре — копию коллеги не показываем.
            let theirs: Vec<Event> = parse_events(&values, &tz)
                .into_iter()
                .filter(|e| !events.iter().any(|m| m.owner.is_none() && m.uid == e.uid && m.span.start == e.span.start))
                .map(|mut e| {
                    e.owner = Some(owner.clone());
                    e
                })
                .collect();
            events.extend(theirs);
        }
        colleagues.push(owner);
    }
    events.sort_by(|a, b| (a.span.start, a.span.end, &a.title).cmp(&(b.span.start, b.span.end, &b.title)));
    Ok((events, colleagues))
}

/// Поиск в календаре (S5-21): события с текстом `text` в `[from, to)` — по теме, месту, описанию и участникам на
/// сервере (`filter.text`), по времени начала.
pub async fn search(client: &Client, text: &str, from: i64, to: i64) -> Result<Vec<Event>> {
    let values = client.search_events(text, &utc(from), &utc(to), SEARCH_LIMIT).await?;
    Ok(parse_events(&values, &TimeZone::system()))
}

/// Занятость в `[from, to)`: моя (из моих событий) и каждого адреса (`None` — нет данных: сотрудник не найден,
/// календарь не открыт мне или сервер не ответил).
pub async fn availability(client: &Client, emails: &[String], from: i64, to: i64) -> Result<(Vec<Busy>, Vec<(String, Option<Vec<Busy>>)>)> {
    let mine = busy_from_events(&week(client, from, to).await?);
    let mut people = Vec::with_capacity(emails.len());
    for email in emails {
        let busy = match principal(client, email).await {
            Ok(Some(id)) => client.availability(&id, &utc(from), &utc(to)).await.ok().and_then(|p| busy_from_periods(&p)),
            _ => None,
        };
        people.push((email.clone(), busy));
    }
    Ok((mine, people))
}

/// Сотрудник по адресу: фильтр `email`, иначе поиск по тексту.
async fn principal(client: &Client, email: &str) -> Result<Option<String>> {
    if let Some(p) = client.principal_by_email(email).await? {
        return Ok(Some(p.id));
    }
    let found = client.principals(email, 5).await?;
    Ok(found.into_iter().find(|p| p.email.as_deref().is_some_and(|e| e.eq_ignore_ascii_case(email))).map(|p| p.id))
}

/// Создать встречу и разослать приглашения (сервер); организатор — первая Identity. Возвращает id события.
pub async fn create(client: &Client, identities: &[Identity], meeting: &Meeting) -> Result<String> {
    let chosen = meeting.from.as_deref().and_then(|f| identities.iter().find(|i| i.email.eq_ignore_ascii_case(f)));
    let me = chosen.or_else(|| identities.first()).ok_or_else(|| Error::Protocol("на сервере нет Identity для организатора".into()))?;
    let calendars = client.calendars().await?;
    let cal = default_calendar(&calendars).ok_or_else(|| Error::Protocol("у учётной записи нет календаря".into()))?;
    let send = !meeting.attendees.is_empty() || !meeting.optional.is_empty();
    client.create_event(meeting.to_jmap(&cal.id, (&me.name, &me.email)), send).await
}

/// Открыть коллеге (по адресу) только занятость всех своих календарей — без подробностей событий.
pub async fn share_free_busy(client: &Client, email: &str) -> Result<()> {
    let id = principal(client, email).await?.ok_or_else(|| Error::Protocol(format!("сотрудник {email} не найден")))?;
    let calendars = client.calendars().await?;
    if calendars.is_empty() {
        return Err(Error::Protocol("у учётной записи нет календаря".into()));
    }
    for c in &calendars {
        client.share_calendar(&c.id, &id, false).await?;
    }
    Ok(())
}

// --- Правка и отмена своей встречи (организатор, S7-07б) ---

/// Адрес участника JSCalendar (`calendarAddress` или `sendTo.imip`, без `mailto:`), строчными.
fn participant_email(p: &Value) -> Option<String> {
    let addr = p["calendarAddress"].as_str().or_else(|| p["sendTo"]["imip"].as_str()).or_else(|| p["email"].as_str())?;
    Some(addr.trim().trim_start_matches("mailto:").to_lowercase())
}

/// Участники новой версии встречи; `keep` — время не менялось: ответы прежних участников остаются (иначе — снова
/// «ждут ответа», как приглашение заново).
pub fn merged_participants(new: &Value, old: &Value, keep: bool) -> Value {
    let mut out = new.as_object().cloned().unwrap_or_default();
    if keep {
        let before: Vec<(String, Value)> =
            old.as_object().into_iter().flatten().filter_map(|(_, p)| Some((participant_email(p)?, p["participationStatus"].clone()))).collect();
        for p in out.values_mut() {
            let status = participant_email(p).and_then(|e| before.iter().find(|(b, _)| *b == e).map(|(_, s)| s.clone()));
            if let Some(s) = status.filter(|s| s.is_string()) {
                p["participationStatus"] = s;
            }
        }
    }
    Value::Object(out)
}

/// Свойства встречи для правки (`CalendarEvent/set` `update` или исключение экземпляра): тема, описание, место,
/// начало (местное в поясе `time_zone`), длительность, участники.
pub fn meeting_props(new: &Value, start: &str, time_zone: &str, participants: Value) -> Map<String, Value> {
    let mut m = Map::new();
    for k in ["title", "description", "duration"] {
        m.insert(k.into(), new[k].clone());
    }
    m.insert("start".into(), Value::from(start));
    m.insert("timeZone".into(), Value::from(time_zone));
    m.insert("locations".into(), new.get("locations").cloned().unwrap_or(Value::Null));
    m.insert("participants".into(), participants);
    m
}

/// Местное начало JSCalendar (`2026-11-02T10:00:00`), сдвинутое на `secs` секунд.
pub fn local_shifted(start: &str, secs: i64) -> Option<String> {
    let dt: jiff::civil::DateTime = start.parse().ok()?;
    let moved = dt.checked_add(jiff::Span::new().seconds(secs)).ok()?;
    Some(moved.strftime("%Y-%m-%dT%H:%M:%S").to_string())
}

/// Unix-секунды → местное время JSCalendar в поясе `tz`.
fn local_at(ts: i64, tz: &str) -> Option<String> {
    let zone = TimeZone::get(tz).ok()?;
    Some(jiff::Timestamp::from_second(ts).ok()?.to_zoned(zone).strftime("%Y-%m-%dT%H:%M:%S").to_string())
}

/// Патч серии с исключением экземпляра `rid`: `recurrenceOverrides` целиком (Stalwart не принимает путь внутрь
/// пустой карты — `invalidProperties`), прежние исключения — на месте.
pub fn with_override(base: &Value, rid: &str, value: Value) -> Map<String, Value> {
    let mut overrides = base["recurrenceOverrides"].as_object().cloned().unwrap_or_default();
    overrides.insert(rid.to_owned(), value);
    let mut p = Map::new();
    p.insert("recurrenceOverrides".into(), Value::Object(overrides));
    p
}

/// Своя встреча на сервере: само событие (у серии — серия по UID) и `recurrenceId` экземпляра, если это экземпляр.
async fn own_meeting(client: &Client, event: &Event) -> Result<(Value, Option<String>)> {
    let base = client.event_by_uid(&event.uid).await?.ok_or_else(|| Error::Protocol("встреча не найдена на сервере".into()))?;
    let series = base["recurrenceRules"].as_array().is_some_and(|r| !r.is_empty()) || base["recurrenceRule"].is_object();
    let rid = if series && base["id"].as_str() != Some(event.id.as_str()) {
        let tz = base["timeZone"].as_str().unwrap_or("Etc/UTC");
        // `recurrenceId` экземпляра — от сервера; нет — местное начало экземпляра в поясе серии.
        let got = client.event_by_id(&event.id).await.ok().flatten();
        got.and_then(|v| v["recurrenceId"].as_str().map(str::to_owned)).or_else(|| local_at(event.span.start, tz))
    } else {
        None
    };
    Ok((base, rid))
}

/// Изменить свою встречу и разослать изменения (`sendSchedulingMessages`): одиночную — целиком, у серии — всю серию
/// со сдвигом на ту же разницу (`series`) или исключение экземпляра (`recurrenceOverrides`). → UID встречи.
pub async fn update(client: &Client, identities: &[Identity], event: &Event, meeting: &Meeting, series: bool) -> Result<String> {
    let chosen = meeting.from.as_deref().and_then(|f| identities.iter().find(|i| i.email.eq_ignore_ascii_case(f)));
    let me = chosen.or_else(|| identities.first()).ok_or_else(|| Error::Protocol("на сервере нет Identity для организатора".into()))?;
    let (base, rid) = own_meeting(client, event).await?;
    let id = base["id"].as_str().ok_or_else(|| Error::Protocol("у встречи нет id".into()))?.to_owned();
    let new = meeting.to_jmap("", (&me.name, &me.email));
    let shift = meeting.shift_from(event).ok_or_else(|| Error::Protocol(format!("часовой пояс {}", meeting.time_zone)))?;
    let keep = shift == 0 && meeting.duration_secs == event.span.len();
    let send = !meeting.attendees.is_empty() || !meeting.optional.is_empty() || base["participants"].as_object().is_some_and(|p| p.len() > 1);
    let patch = match rid {
        Some(rid) if !series => {
            let tz = base["timeZone"].as_str().unwrap_or("Etc/UTC");
            let start = meeting.start_ts().and_then(|t| local_at(t, tz)).ok_or_else(|| Error::Protocol("время встречи".into()))?;
            let participants = merged_participants(&new["participants"], &base["participants"], keep);
            let mut props = meeting_props(&new, &start, tz, participants);
            // Исключение — правка поверх серии: пояс у него — серии.
            props.remove("timeZone");
            with_override(&base, &rid, Value::Object(props))
        }
        Some(_) => {
            let tz = base["timeZone"].as_str().unwrap_or("Etc/UTC");
            let start = base["start"].as_str().and_then(|s| local_shifted(s, shift)).ok_or_else(|| Error::Protocol("начало серии".into()))?;
            meeting_props(&new, &start, tz, merged_participants(&new["participants"], &base["participants"], keep))
        }
        None => {
            let start = new["start"].as_str().unwrap_or_default().to_owned();
            meeting_props(&new, &start, &meeting.time_zone, merged_participants(&new["participants"], &base["participants"], keep))
        }
    };
    client.update_event(&id, patch, send).await?;
    Ok(event.uid.clone())
}

/// Отменить свою встречу: событие удаляется с рассылкой отмены; экземпляр серии без `series` — исключён из серии
/// (`recurrenceOverrides/<recurrenceId>: { excluded: true }`). → UID встречи.
pub async fn cancel(client: &Client, event: &Event, series: bool) -> Result<String> {
    let (base, rid) = own_meeting(client, event).await?;
    let id = base["id"].as_str().ok_or_else(|| Error::Protocol("у встречи нет id".into()))?.to_owned();
    let send = base["participants"].as_object().is_some_and(|p| p.len() > 1);
    match rid.filter(|_| !series) {
        Some(rid) => {
            client.update_event(&id, with_override(&base, &rid, serde_json::json!({ "excluded": true })), send).await?;
        }
        None => client.destroy_event(&id, send).await?,
    }
    Ok(event.uid.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> i64 {
        s.parse::<jiff::Timestamp>().unwrap().as_second()
    }

    #[test]
    fn empty_availability_is_no_data_not_free() {
        assert_eq!(busy_from_periods(&[]), None);
        let p = |s: &str, e: &str, k: Option<&str>| BusyPeriod { utc_start: s.into(), utc_end: e.into(), busy_status: k.map(str::to_owned) };
        let busy = busy_from_periods(&[
            p("2026-10-20T07:00:00Z", "2026-10-20T08:00:00Z", None),
            p("2026-10-20T09:00:00Z", "2026-10-20T10:00:00Z", Some("tentative")),
            p("2026-10-20T11:00:00Z", "2026-10-20T12:00:00Z", Some("unavailable")),
            p("плохо", "2026-10-20T12:00:00Z", None),
        ])
        .unwrap();
        assert_eq!(busy.iter().map(|b| b.kind).collect::<Vec<_>>(), [BusyKind::Busy, BusyKind::Tentative, BusyKind::Unavailable]);
        assert_eq!(busy[0].span.start, at("2026-10-20T07:00:00Z"));
    }

    #[test]
    fn default_calendar_is_first_visible() {
        let c = |id: &str, order: u32, visible: bool| Calendar {
            id: id.into(),
            name: id.into(),
            color: None,
            sort_order: order,
            is_visible: visible,
            is_subscribed: true,
        };
        let cals = [c("hidden", 0, false), c("b", 2, true), c("a", 1, true)];
        assert_eq!(default_calendar(&cals).map(|c| c.id.as_str()), Some("a"));
        assert!(default_calendar(&[]).is_none());
    }
    #[test]
    fn edit_keeps_answers_only_when_time_stays() {
        let new = serde_json::json!({
            "organizer": { "calendarAddress": "mailto:me@x", "participationStatus": "accepted" },
            "a0": { "calendarAddress": "mailto:B@x", "participationStatus": "needs-action" },
            "a1": { "calendarAddress": "mailto:c@x", "participationStatus": "needs-action" }
        });
        let old = serde_json::json!({ "p9": { "sendTo": { "imip": "mailto:b@x" }, "participationStatus": "accepted" } });
        let kept = merged_participants(&new, &old, true);
        assert_eq!(kept["a0"]["participationStatus"], "accepted");
        assert_eq!(kept["a1"]["participationStatus"], "needs-action", "новый участник — ждёт ответа");
        assert_eq!(merged_participants(&new, &old, false)["a0"]["participationStatus"], "needs-action", "перенос — ответы заново");
        assert_eq!(local_shifted("2026-11-02T10:00:00", 3600).as_deref(), Some("2026-11-02T11:00:00"));
        assert_eq!(local_shifted("2026-11-02T23:30:00", 3600).as_deref(), Some("2026-11-03T00:30:00"));
        let props = meeting_props(
            &serde_json::json!({ "title": "T", "description": "", "duration": "PT1H" }),
            "2026-11-02T10:00:00",
            "Europe/Moscow",
            Value::Null,
        );
        assert_eq!(props["locations"], Value::Null, "места нет — снято");
        assert_eq!((props["start"].as_str(), props["timeZone"].as_str()), (Some("2026-11-02T10:00:00"), Some("Europe/Moscow")));
    }
}
