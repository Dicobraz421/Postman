//! Ответ на приглашение, пришедшее письмом (iMIP), и состояние карточки приглашения (S5-11).
//!
//! Если событие уже есть в календаре на сервере (сервер разобрал письмо сам) — меняем
//! свой статус там, ответ организатору шлёт сервер. Иначе отвечаем письмом METHOD:REPLY,
//! а принятое/«под вопросом» кладём в свой календарь без рассылки. Запросы к серверу — у плагина пути
//! (`postman-backend-jmap`: `via_server`, `import`; ADR 0004), здесь — письмо-ответ и состояние карточки.
//!
//! Состояние карточки ([`classify`]): новое, отвечено, изменено организатором (что именно), отменено, устарело;
//! пересечения с моим календарём — [`conflicts`]. Чистые функции: письмо, мой запомненный ответ ([`Seen`]),
//! копия встречи в моём календаре и «сейчас» — на входе.

use postman_calendar::{Busy, Event, EventStatus, FreeBusy, Invitation, Partstat, Span, imip_reply, partstat_json, reply_subject};
use postman_compose::{Address, CalendarPart, Draft};
use serde_json::{Value, json};

/// Как ушёл ответ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Статус изменён в событии на сервере, организатору ответил сервер.
    Server,
    /// Ответ письмом; `imported` — событие добавлено в мой календарь.
    Mail { imported: bool },
}

/// Письмо-ответ организатору с частью `text/calendar; method=REPLY`.
pub fn reply_draft(inv: &Invitation, me: &Address, status: Partstat, in_reply_to: Option<String>, now: i64) -> Option<Draft> {
    let org = inv.event.organizer().filter(|o| !o.email.is_empty())?;
    let subject = reply_subject(&inv.event.title, status);
    Some(Draft {
        from: me.clone(),
        to: vec![Address::new(&org.name, &org.email)],
        markdown: subject.clone(),
        subject,
        references: in_reply_to.iter().cloned().collect(),
        in_reply_to,
        calendar: Some(CalendarPart { method: "REPLY".into(), ics: imip_reply(inv, (&me.name, &me.email), status, now) }),
        ..Default::default()
    })
}

/// Событие для импорта в календарь: мой статус, календарь назначения, без серверного id.
pub fn import_event(inv: &Invitation, me: &str, status: Partstat, calendar_id: &str) -> Value {
    let mut ev = inv.with_my_status(me, status);
    if let Some(o) = ev.as_object_mut() {
        o.remove("id");
        o.insert("calendarIds".into(), json!({ calendar_id: true }));
    }
    ev
}

// --- состояние карточки приглашения ---

/// Версия встречи: номер и то, что видно в карточке.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub sequence: i64,
    pub span: Span,
    pub title: String,
    pub location: String,
}

impl Version {
    pub fn of(e: &Event) -> Self {
        Self { sequence: e.sequence, span: e.span, title: e.title.clone(), location: e.location.clone() }
    }
}

/// Мой ответ на приглашение, запомненный локально по UID: статус и версия встречи, на которую он дан (`None` —
/// запись S3-03, только статус: считаем, что ответ дан на версию из письма).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub status: Partstat,
    pub version: Option<Version>,
}

impl Seen {
    pub fn of(inv: &Invitation, status: Partstat) -> Self {
        let mut v = Version::of(&inv.event);
        v.sequence = sequence(inv);
        Self { status, version: Some(v) }
    }

    /// Строка для хранилища: JSON.
    pub fn encode(&self) -> String {
        let mut v = serde_json::json!({ "status": partstat_json(self.status) });
        if let Some(x) = &self.version {
            v["sequence"] = x.sequence.into();
            v["start"] = x.span.start.into();
            v["end"] = x.span.end.into();
            v["title"] = x.title.clone().into();
            v["location"] = x.location.clone().into();
        }
        v.to_string()
    }

    /// Из хранилища: JSON или прежняя запись S3-03 — одно слово статуса (`accepted`).
    pub fn decode(s: &str) -> Option<Self> {
        let s = s.trim();
        if let Some(status) = partstat(s) {
            return Some(Self { status, version: None });
        }
        let v: Value = serde_json::from_str(s).ok()?;
        let status = partstat(v["status"].as_str()?)?;
        let version = match (v["sequence"].as_i64(), v["start"].as_i64(), v["end"].as_i64()) {
            (Some(sequence), Some(start), Some(end)) => Some(Version {
                sequence,
                span: Span::new(start, end),
                title: v["title"].as_str().unwrap_or("").to_string(),
                location: v["location"].as_str().unwrap_or("").to_string(),
            }),
            _ => None,
        };
        Some(Self { status, version })
    }
}

fn partstat(s: &str) -> Option<Partstat> {
    [Partstat::Accepted, Partstat::Tentative, Partstat::Declined, Partstat::Delegated, Partstat::NeedsAction]
        .into_iter()
        .find(|p| partstat_json(*p) == s)
}

/// Номер версии приглашения: из письма (`SEQUENCE`) или из события.
fn sequence(inv: &Invitation) -> i64 {
    inv.sequence.max(inv.event.sequence)
}

/// Что организатор изменил относительно версии, на которую я отвечал (или той, что в моём календаре).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// Время; было — это.
    Time(Span),
    /// Место; было — это.
    Location(String),
    /// Тема; была — эта.
    Title(String),
}

/// Почему приглашение устарело.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outdated {
    /// Есть более новая версия встречи (в календаре или уже отвеченная).
    Newer,
    /// Встреча прошла.
    Past,
}

/// Состояние карточки приглашения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InviteState {
    /// Не запрос и не отмена (ответ участника, встречное предложение) — только сведения.
    Info,
    /// Меня нет среди участников (или мой адрес неизвестен).
    NotInvited,
    /// Ждёт моего ответа.
    New,
    /// Я ответил на эту версию.
    Answered(Partstat),
    /// Организатор изменил встречу после версии, которую я видел: что изменилось. Ответ нужен заново.
    Updated(Vec<Change>),
    /// Организатор отменил встречу.
    Cancelled,
    Outdated(Outdated),
}

/// Что известно кроме письма: мой запомненный ответ, копия встречи в моём календаре (тот же UID), «сейчас».
#[derive(Debug, Clone, Copy, Default)]
pub struct Known<'a> {
    pub seen: Option<&'a Seen>,
    pub server: Option<&'a Event>,
    pub now: i64,
}

/// Состояние приглашения из письма для меня (`me` — мой адрес).
pub fn classify(inv: &Invitation, me: Option<&str>, known: &Known) -> InviteState {
    let seq = sequence(inv);
    let seen_seq = known.seen.and_then(|s| s.version.as_ref()).map(|v| v.sequence);
    let newer = known.server.map(|e| e.sequence).into_iter().chain(seen_seq).any(|s| s > seq);
    match inv.method.as_str() {
        "CANCEL" => {
            // Отмену перекрыла более новая версия (встречу вернули) — письмо об отмене устарело.
            let revived = known.server.is_some_and(|e| e.sequence > seq && e.status != EventStatus::Cancelled);
            return if revived { InviteState::Outdated(Outdated::Newer) } else { InviteState::Cancelled };
        }
        "REQUEST" => {}
        _ => return InviteState::Info,
    }
    let Some(invited) = me.and_then(|me| inv.my_status(me)) else { return InviteState::NotInvited };
    if newer {
        return InviteState::Outdated(Outdated::Newer);
    }
    if inv.event.span.end <= known.now {
        return InviteState::Outdated(Outdated::Past);
    }
    // Прежняя версия: на которую я отвечал, иначе — копия в календаре.
    let prior = known
        .seen
        .and_then(|s| s.version.clone())
        .filter(|v| v.sequence < seq)
        .or_else(|| known.server.filter(|e| e.sequence < seq).map(Version::of));
    if let Some(p) = &prior {
        let changes = changes(p, &inv.event);
        if !changes.is_empty() {
            return InviteState::Updated(changes);
        }
    }
    // Мой ответ на эту версию: запомненный, из календаря, из самого письма.
    let remembered = known.seen.map(|s| s.status);
    let in_calendar = known.server.filter(|e| e.sequence == seq).and_then(|e| me.and_then(|me| my_status(e, me)));
    match remembered.into_iter().chain(in_calendar).chain([invited]).find(|s| *s != Partstat::NeedsAction) {
        Some(s) => InviteState::Answered(s),
        None => InviteState::New,
    }
}

/// Что изменилось во встрече от версии `was`.
pub fn changes(was: &Version, now: &Event) -> Vec<Change> {
    let mut out = Vec::new();
    if was.span != now.span {
        out.push(Change::Time(was.span));
    }
    if was.location.trim() != now.location.trim() {
        out.push(Change::Location(was.location.clone()));
    }
    if was.title.trim() != now.title.trim() {
        out.push(Change::Title(was.title.clone()));
    }
    out
}

fn my_status(e: &Event, me: &str) -> Option<Partstat> {
    e.participants.iter().find(|p| p.email.eq_ignore_ascii_case(me)).map(|p| p.status)
}

/// Мои события, с которыми встреча `span` пересекается: занимающие время (не «свободно», не отменённые, не
/// отклонённые мной), кроме самой встречи (тот же `uid`) и событий на весь день. По времени начала.
pub fn conflicts<'a>(span: Span, uid: &str, events: &'a [Event], me: Option<&str>) -> Vec<&'a Event> {
    let mut out: Vec<&Event> = events
        .iter()
        .filter(|e| e.uid != uid && !e.all_day && e.span.overlaps(&span))
        .filter(|e| e.free_busy == FreeBusy::Busy && e.status != EventStatus::Cancelled)
        .filter(|e| me.and_then(|me| my_status(e, me)) != Some(Partstat::Declined))
        .collect();
    out.sort_by_key(|e| e.span.start);
    out
}

/// Пересечения встречи `span` с моей занятостью (`Principal/getAvailability` или мои события): задетые периоды.
pub fn busy_conflicts(span: Span, busy: &[Busy]) -> Vec<Busy> {
    let mut out: Vec<Busy> = busy.iter().filter(|b| b.span.overlaps(&span)).copied().collect();
    out.sort_by_key(|b| b.span.start);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::tz::TimeZone;

    fn inv() -> Invitation {
        let v = json!({ "method": "request", "data": {
            "@type": "Event", "uid": "u-1@partner.example", "title": "Созвон", "id": "srv",
            "start": "2026-10-20T10:00:00", "timeZone": "Europe/Moscow", "duration": "PT1H",
            "participants": {
                "o": { "name": "Иван", "calendarAddress": "mailto:ivan@partner.example", "roles": { "owner": true } },
                "m": { "calendarAddress": "mailto:me@corp.example", "roles": { "attendee": true }, "participationStatus": "needs-action" }
            }
        }});
        Invitation::from_worker(&v, &TimeZone::UTC).unwrap()
    }

    #[test]
    fn reply_goes_to_organizer() {
        let me = Address::new("Я", "me@corp.example");
        let d = reply_draft(&inv(), &me, Partstat::Tentative, Some("m1@partner.example".into()), 0).unwrap();
        assert_eq!(d.to, vec![Address::new("Иван", "ivan@partner.example")]);
        assert_eq!(d.subject, "Под вопросом: Созвон");
        assert_eq!(d.references, ["m1@partner.example"]);
        let cal = d.calendar.unwrap();
        assert_eq!(cal.method, "REPLY");
        assert!(cal.ics.contains("PARTSTAT=TENTATIVE"), "{}", cal.ics);
    }

    #[test]
    fn import_sets_calendar_and_status() {
        let ev = import_event(&inv(), "me@corp.example", Partstat::Accepted, "cal1");
        assert_eq!(ev["calendarIds"], json!({ "cal1": true }));
        assert_eq!(ev["participants"]["m"]["participationStatus"], "accepted");
        assert!(ev.get("id").is_none() && ev.get("method").is_none());
    }

    // --- состояние карточки ---

    const ME: &str = "me@corp.example";

    fn at(s: &str) -> i64 {
        s.parse::<jiff::Timestamp>().unwrap().as_second()
    }

    /// Приглашение версии `seq` на 10:00–11:00 МСК 20.10 (07:00–08:00 UTC), мой статус `mine`.
    fn letter(method: &str, seq: i64, start: &str, location: &str, mine: &str) -> Invitation {
        let v = json!({ "method": method, "data": {
            "@type": "Event", "uid": "u-1@partner.example", "title": "Созвон",
            "start": start, "timeZone": "Europe/Moscow", "duration": "PT1H", "sequence": seq,
            "locations": { "l": { "name": location } },
            "participants": {
                "o": { "calendarAddress": "mailto:ivan@partner.example", "roles": { "owner": true } },
                "m": { "calendarAddress": "mailto:me@corp.example", "roles": { "attendee": true }, "participationStatus": mine }
            }
        }});
        Invitation::from_worker(&v, &TimeZone::UTC).unwrap()
    }

    fn request(seq: i64) -> Invitation {
        letter("REQUEST", seq, "2026-10-20T10:00:00", "Teams", "needs-action")
    }

    fn before() -> i64 {
        at("2026-10-19T12:00:00Z")
    }

    fn known<'a>(seen: Option<&'a Seen>, server: Option<&'a Event>) -> Known<'a> {
        Known { seen, server, now: before() }
    }

    #[test]
    fn new_answered_cancelled_info_and_not_invited() {
        let k = known(None, None);
        assert_eq!(classify(&request(0), Some(ME), &k), InviteState::New);
        assert_eq!(classify(&request(0), Some("other@corp.example"), &k), InviteState::NotInvited);
        assert_eq!(classify(&request(0), None, &k), InviteState::NotInvited);
        // Ответ запомнен (на эту версию или старой записью S3-03 — только статус).
        let seen = Seen::of(&request(0), Partstat::Tentative);
        assert_eq!(classify(&request(0), Some(ME), &known(Some(&seen), None)), InviteState::Answered(Partstat::Tentative));
        let legacy = Seen::decode("accepted").unwrap();
        assert_eq!(classify(&request(0), Some(ME), &known(Some(&legacy), None)), InviteState::Answered(Partstat::Accepted));
        // Ответ уже в моём календаре (другой клиент) или в самом письме.
        let mut server = request(0).event;
        if let Some(p) = server.participants.iter_mut().find(|p| p.email == ME) {
            p.status = Partstat::Declined;
        }
        assert_eq!(classify(&request(0), Some(ME), &known(None, Some(&server))), InviteState::Answered(Partstat::Declined));
        let resent = letter("REQUEST", 0, "2026-10-20T10:00:00", "Teams", "accepted");
        assert_eq!(classify(&resent, Some(ME), &k), InviteState::Answered(Partstat::Accepted));
        // Отмена и прочие методы.
        let cancel = letter("CANCEL", 1, "2026-10-20T10:00:00", "Teams", "needs-action");
        assert_eq!(classify(&cancel, Some(ME), &k), InviteState::Cancelled);
        assert_eq!(classify(&letter("REPLY", 0, "2026-10-20T10:00:00", "Teams", "accepted"), Some(ME), &k), InviteState::Info);
    }

    #[test]
    fn update_lists_what_changed_since_my_answer() {
        let seen = Seen::of(&request(0), Partstat::Accepted);
        // Перенесли на час и сменили место — ответ нужен заново, видно, что было.
        let moved = letter("REQUEST", 1, "2026-10-20T11:00:00", "Переговорная 2", "needs-action");
        let state = classify(&moved, Some(ME), &known(Some(&seen), None));
        assert_eq!(
            state,
            InviteState::Updated(vec![
                Change::Time(Span::new(at("2026-10-20T07:00:00Z"), at("2026-10-20T08:00:00Z"))),
                Change::Location("Teams".into()),
            ])
        );
        // Без моего ответа прежняя версия — копия в календаре.
        let server = request(0).event;
        assert!(matches!(classify(&moved, Some(ME), &known(None, Some(&server))), InviteState::Updated(c) if c.len() == 2));
        // Новая версия без видимых изменений (например, добавили участника) — прежний ответ в силе.
        assert_eq!(classify(&request(1), Some(ME), &known(Some(&seen), None)), InviteState::Answered(Partstat::Accepted));
    }

    #[test]
    fn outdated_by_a_newer_version_or_time() {
        // Я ответил на версию 2 — письмо с версией 1 устарело; так же, если в календаре версия новее.
        let seen = Seen::of(&request(2), Partstat::Accepted);
        assert_eq!(classify(&request(1), Some(ME), &known(Some(&seen), None)), InviteState::Outdated(Outdated::Newer));
        let server = request(3).event;
        assert_eq!(classify(&request(1), Some(ME), &known(None, Some(&server))), InviteState::Outdated(Outdated::Newer));
        // Отмену перекрыла новая версия встречи — отмена устарела; отменённая копия — нет.
        let cancel = letter("CANCEL", 1, "2026-10-20T10:00:00", "Teams", "needs-action");
        assert_eq!(classify(&cancel, Some(ME), &known(None, Some(&server))), InviteState::Outdated(Outdated::Newer));
        let mut gone = server.clone();
        gone.status = EventStatus::Cancelled;
        assert_eq!(classify(&cancel, Some(ME), &known(None, Some(&gone))), InviteState::Cancelled);
        // Встреча прошла.
        let late = Known { now: at("2026-10-20T08:00:00Z"), ..Known::default() };
        assert_eq!(classify(&request(0), Some(ME), &late), InviteState::Outdated(Outdated::Past));
        let during = Known { now: at("2026-10-20T07:59:00Z"), ..Known::default() };
        assert_eq!(classify(&request(0), Some(ME), &during), InviteState::New);
    }

    #[test]
    fn seen_round_trip_and_legacy_record() {
        let seen = Seen::of(&request(4), Partstat::Declined);
        assert_eq!(Seen::decode(&seen.encode()), Some(seen.clone()));
        assert_eq!(seen.version.as_ref().map(|v| v.sequence), Some(4));
        assert_eq!(Seen::decode("tentative"), Some(Seen { status: Partstat::Tentative, version: None }));
        assert_eq!(Seen::decode("{not json"), None);
        assert_eq!(Seen::decode(r#"{"status":"maybe"}"#), None);
    }

    fn mine(id: &str, start: &str, end: &str) -> Event {
        let v = json!({ "id": id, "uid": format!("{id}@corp.example"), "title": id, "utcStart": start, "utcEnd": end,
            "participants": { "m": { "calendarAddress": "mailto:me@corp.example", "participationStatus": "accepted" } } });
        Event::from_jmap(&v, &TimeZone::UTC).unwrap()
    }

    #[test]
    fn conflicts_with_my_calendar() {
        let inv = request(0);
        let span = inv.event.span; // 07:00–08:00 UTC
        let mut copy = mine("self", "2026-10-20T07:00:00Z", "2026-10-20T08:00:00Z");
        copy.uid = inv.event.uid.clone();
        let mut free = mine("free", "2026-10-20T07:00:00Z", "2026-10-20T07:30:00Z");
        free.free_busy = FreeBusy::Free;
        let mut declined = mine("declined", "2026-10-20T07:15:00Z", "2026-10-20T07:45:00Z");
        declined.participants[0].status = Partstat::Declined;
        let mut cancelled = mine("cancelled", "2026-10-20T07:00:00Z", "2026-10-20T08:00:00Z");
        cancelled.status = EventStatus::Cancelled;
        let mut all_day = mine("all-day", "2026-10-19T21:00:00Z", "2026-10-20T21:00:00Z");
        all_day.all_day = true;
        let events = vec![
            mine("standup", "2026-10-20T07:30:00Z", "2026-10-20T08:30:00Z"),
            mine("before", "2026-10-20T06:00:00Z", "2026-10-20T07:00:00Z"),
            mine("after", "2026-10-20T08:00:00Z", "2026-10-20T09:00:00Z"),
            mine("early", "2026-10-20T06:30:00Z", "2026-10-20T07:10:00Z"),
            copy,
            free,
            declined,
            cancelled,
            all_day,
        ];
        let ids: Vec<&str> = conflicts(span, &inv.event.uid, &events, Some(ME)).iter().map(|e| e.id.as_str()).collect();
        // Встык — не пересечение; сама встреча, «свободно», отклонённое, отменённое и весь день — не мешают.
        assert_eq!(ids, ["early", "standup"]);
        // Чужое «отклонено» (меня в событии нет) — пересечение.
        assert_eq!(conflicts(span, &inv.event.uid, &events, Some("x@corp.example")).len(), 3);
    }

    #[test]
    fn conflicts_with_my_busy_times() {
        let span = Span::new(at("2026-10-20T07:00:00Z"), at("2026-10-20T08:00:00Z"));
        let b = |s: &str, e: &str, kind| Busy { span: Span::new(at(s), at(e)), kind };
        use postman_calendar::BusyKind::{Busy as B, Tentative as T};
        let busy = [
            b("2026-10-20T07:45:00Z", "2026-10-20T09:00:00Z", T),
            b("2026-10-20T06:00:00Z", "2026-10-20T07:00:00Z", B),
            b("2026-10-20T06:30:00Z", "2026-10-20T07:15:00Z", B),
        ];
        let hit = busy_conflicts(span, &busy);
        assert_eq!(hit.iter().map(|b| b.kind).collect::<Vec<_>>(), [B, T]);
        assert!(busy_conflicts(span, &[]).is_empty());
    }
}
