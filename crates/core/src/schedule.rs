//! Календарь (S3-05): встреча из окна встречи, разбор недели (повторения разворачивает сервер), моя занятость из
//! моих событий. Запросы к серверу — у плагинов путей (`postman-backend-jmap`: `CalendarEvent/query`,
//! `Principal/getAvailability`; `postman-backend-dav`: `calendar-query`, outbox; ADR 0004).
//!
//! Чистые функции (разбор, перевод в занятость, встреча → JSCalendar) проверяются тестами здесь; сетевые — тестами
//! стенда плагинов.

use jiff::civil::DateTime;
use jiff::tz::TimeZone;
use postman_calendar::{Busy, BusyKind, Event, EventStatus, FreeBusy, NewEvent};
use serde_json::Value;

/// Новая встреча из окна встречи: всё своё (команда уходит в поток синхронизации). Календарь и организатора
/// подставляет синхронизация: календарь по умолчанию и адрес учётной записи.
#[derive(Debug, Clone, PartialEq)]
pub struct Meeting {
    pub uid: String,
    pub title: String,
    pub location: String,
    pub description: String,
    /// Локальное начало в зоне `time_zone`.
    pub start: DateTime,
    pub time_zone: String,
    pub duration_secs: i64,
    /// Обязательные участники: (имя, адрес).
    pub attendees: Vec<(String, String)>,
    /// Необязательные участники (и переговорные, ресурсы): (имя, адрес).
    pub optional: Vec<(String, String)>,
    /// Организатор — адрес «От» вкладки встречи (S7-03б): по нему встреча уходит в свою учётную запись профиля, а
    /// организатором становится этот Identity; `None` — первый Identity основной учётной записи.
    pub from: Option<String>,
}

impl Meeting {
    /// Участники в форме `NewEvent`: обязательные, затем необязательные.
    pub fn attendee_refs(&self) -> Vec<(&str, &str, bool)> {
        let required = self.attendees.iter().map(|(n, e)| (n.as_str(), e.as_str(), false));
        required.chain(self.optional.iter().map(|(n, e)| (n.as_str(), e.as_str(), true))).collect()
    }

    /// Событие для `CalendarEvent/set`; `attendees` — из [`Meeting::attendee_refs`].
    pub fn new_event<'a>(&'a self, calendar_id: &'a str, organizer: (&'a str, &'a str), attendees: &'a [(&'a str, &'a str, bool)]) -> NewEvent<'a> {
        NewEvent {
            uid: &self.uid,
            calendar_id,
            title: &self.title,
            description: &self.description,
            location: &self.location,
            start: self.start,
            time_zone: &self.time_zone,
            duration_secs: self.duration_secs,
            organizer,
            attendees,
        }
    }

    /// Начало встречи в Unix-секундах (местное начало в её поясе); пояса нет в базе — `None`.
    pub fn start_ts(&self) -> Option<i64> {
        let tz = TimeZone::get(&self.time_zone).ok()?;
        self.start.to_zoned(tz).ok().map(|z| z.timestamp().as_second())
    }

    /// Насколько правка сдвигает встречу `event` (секунды): у серии — каждый экземпляр на столько же (S7-07б).
    pub fn shift_from(&self, event: &Event) -> Option<i64> {
        Some(self.start_ts()? - event.span.start)
    }

    pub fn to_jmap(&self, calendar_id: &str, organizer: (&str, &str)) -> Value {
        let attendees = self.attendee_refs();
        self.new_event(calendar_id, organizer, &attendees).to_jmap()
    }
}

/// Unix-секунды → `YYYY-MM-DDTHH:MM:SSZ` (фильтры и запросы JMAP).
pub fn utc(ts: i64) -> String {
    jiff::Timestamp::from_second(ts).unwrap_or(jiff::Timestamp::UNIX_EPOCH).strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// События недели: разобранные, без отменённых, по времени начала. Неразборчивое событие пропускается —
/// неделя показывается без него.
pub fn parse_events(values: &[Value], tz: &TimeZone) -> Vec<Event> {
    let mut out: Vec<Event> = values.iter().filter_map(|v| Event::from_jmap(v, tz).ok()).filter(|e| e.status != EventStatus::Cancelled).collect();
    out.sort_by(|a, b| (a.span.start, a.span.end, &a.title).cmp(&(b.span.start, b.span.end, &b.title)));
    out
}

/// Моя занятость из моих событий: «свободно» (`freeBusyStatus: free`) и отменённые не считаются.
pub fn busy_from_events(events: &[Event]) -> Vec<Busy> {
    events
        .iter()
        .filter(|e| e.free_busy == FreeBusy::Busy && e.status != EventStatus::Cancelled)
        .map(|e| Busy { span: e.span, kind: if e.status == EventStatus::Tentative { BusyKind::Tentative } else { BusyKind::Busy } })
        .collect()
}

/// Найденных событий в календаре — не больше (S5-21: список под поиском).
pub const SEARCH_LIMIT: usize = 100;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(s: &str) -> i64 {
        s.parse::<jiff::Timestamp>().unwrap().as_second()
    }

    #[test]
    fn jmap_events_become_the_week() {
        let values = vec![
            json!({ "id": "b", "title": "Позже", "utcStart": "2026-10-20T12:00:00Z", "utcEnd": "2026-10-20T13:00:00Z" }),
            // Экземпляр повторения: сервер отдал его со своим `recurrenceId` и UTC-временем.
            json!({ "id": "a", "title": "Планёрка", "recurrenceId": "2026-10-20T09:00:00", "start": "2026-10-20T09:00:00",
                    "timeZone": "Europe/Moscow", "duration": "PT30M", "status": "tentative" }),
            json!({ "id": "c", "title": "Отменена", "utcStart": "2026-10-20T08:00:00Z", "utcEnd": "2026-10-20T09:00:00Z",
                    "status": "cancelled" }),
            json!({ "id": "d", "title": "Битое", "start": "не время" }),
        ];
        let week = parse_events(&values, &TimeZone::UTC);
        assert_eq!(week.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(week[0].span.start, at("2026-10-20T06:00:00Z"));
        assert!(week[0].recurring);
        let busy = busy_from_events(&week);
        assert_eq!(busy[0].kind, BusyKind::Tentative);
        assert_eq!(busy[1], Busy { span: postman_calendar::Span::new(at("2026-10-20T12:00:00Z"), at("2026-10-20T13:00:00Z")), kind: BusyKind::Busy });
        // «Свободно» в событии — не занятость.
        let free = parse_events(
            &[json!({ "utcStart": "2026-10-20T12:00:00Z", "utcEnd": "2026-10-20T13:00:00Z", "freeBusyStatus": "free" })],
            &TimeZone::UTC,
        );
        assert!(busy_from_events(&free).is_empty());
    }

    #[test]
    fn meeting_becomes_an_invitation() {
        let m = Meeting {
            uid: "u1@postman".into(),
            title: "Планёрка".into(),
            location: "Переговорная 1".into(),
            description: String::new(),
            start: "2026-10-21T11:00:00".parse().unwrap(),
            time_zone: "Europe/Moscow".into(),
            duration_secs: 1800,
            attendees: vec![("User 3".into(), "user3@itt.test".into())],
            optional: vec![("Переговорная 1".into(), "room1@itt.test".into())],
            from: None,
        };
        let v = m.to_jmap("cal1", ("User 1", "user1@itt.test"));
        assert_eq!(v["calendarIds"]["cal1"], true);
        assert_eq!(v["start"], "2026-10-21T11:00:00");
        assert_eq!(v["duration"], "PT30M");
        assert_eq!(v["organizerCalendarAddress"], "mailto:user1@itt.test");
        assert_eq!(v["participants"]["a0"]["calendarAddress"], "mailto:user3@itt.test");
        assert_eq!(v["participants"]["a1"]["calendarAddress"], "mailto:room1@itt.test");
        assert_eq!(v["participants"]["a1"]["roles"]["optional"], true);
        assert_eq!(v["locations"]["l1"]["name"], "Переговорная 1");
        assert_eq!(utc(at("2026-10-21T08:00:00Z")), "2026-10-21T08:00:00Z");
    }
}
