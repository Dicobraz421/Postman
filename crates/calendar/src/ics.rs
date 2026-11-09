//! Сборка iCalendar для CalDAV (S7-05б): встреча (VEVENT с участниками), блок времени задачи, задача на хранение
//! (VTODO), правка своего ответа участника и `SCHEDULE-AGENT` (RFC 6638 §7.1); правка и отмена своей встречи —
//! всей или одного экземпляра серии (S7-07б, [`edited`], [`without_instance`]). Разбор чужих .ics — только в
//! воркере; здесь — своё событие или своя копия на своём сервере, построчно.
//!
//! `SCHEDULE-AGENT`: встреча — `SERVER` (приглашения, ответы, отмены рассылает сервер, неявное планирование);
//! задача и поручение — `CLIENT` (письма iTIP шлёт сам Postman, S6-09; сервер не должен слать свои).

use jiff::tz::TimeZone;

use crate::invite::{fold, text};
use crate::task::{cn, utc_ical};
use crate::{Event, NewEvent, Partstat, Task};

/// Кто рассылает сообщения планирования участнику (RFC 6638 §7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleAgent {
    Server,
    Client,
}

impl ScheduleAgent {
    fn ical(self) -> &'static str {
        match self {
            ScheduleAgent::Server => "SERVER",
            ScheduleAgent::Client => "CLIENT",
        }
    }
}

/// PARTSTAT iCalendar.
pub fn partstat_ical(s: Partstat) -> &'static str {
    match s {
        Partstat::NeedsAction => "NEEDS-ACTION",
        Partstat::Accepted => "ACCEPTED",
        Partstat::Declined => "DECLINED",
        Partstat::Tentative => "TENTATIVE",
        Partstat::Delegated => "DELEGATED",
    }
}

fn join(lines: &[String]) -> String {
    lines.iter().map(|x| fold(x)).collect::<Vec<_>>().join("\r\n") + "\r\n"
}

impl NewEvent<'_> {
    /// Встреча для PUT в календарь CalDAV: время — UTC (без VTIMEZONE: зону сервер не обязан знать), организатор —
    /// `ORGANIZER` и участник-председатель, участники — `SCHEDULE-AGENT=SERVER`: приглашения рассылает сервер
    /// (`docs/STALWART.md` п. 47).
    pub fn to_ics(&self, now: i64) -> Result<String, String> {
        let tz = TimeZone::get(self.time_zone).unwrap_or(TimeZone::UTC);
        let start = self.start.to_zoned(tz).map_err(|e| e.to_string())?.timestamp().as_second();
        let mut l = vec![
            "BEGIN:VCALENDAR".to_string(),
            "PRODID:-//Postman//Postman//RU".into(),
            "VERSION:2.0".into(),
            "BEGIN:VEVENT".into(),
            format!("UID:{}", text(self.uid)),
            format!("DTSTAMP:{}", utc_ical(now)),
            format!("DTSTART:{}", utc_ical(start)),
            format!("DTEND:{}", utc_ical(start + self.duration_secs.max(0))),
            "SEQUENCE:0".into(),
            format!("SUMMARY:{}", text(self.title)),
        ];
        // Весь день (T2-14): даты без времени, конец — следующий день после последнего.
        if self.all_day {
            let first = self.start.date();
            let days = (self.duration_secs.max(86_400) / 86_400).max(1);
            let last = first.checked_add(jiff::Span::new().days(days)).unwrap_or(first);
            l[6] = format!("DTSTART;VALUE=DATE:{}", first.strftime("%Y%m%d"));
            l[7] = format!("DTEND;VALUE=DATE:{}", last.strftime("%Y%m%d"));
        }
        if self.private {
            l.push("CLASS:PRIVATE".into());
        }
        // Повтор (T2-13): DTSTART — в UTC, а дни правила — по местному времени встречи; встреча около полуночи в UTC
        // приходится на другой день — дни недели и числа месяца сдвигаются на эту разницу (у «весь день» — дата та же).
        if let Some(rule) = self.recurrence {
            let local = self.start.date();
            let start = if self.all_day { self.start.to_zoned(TimeZone::UTC).map_or(start, |z| z.timestamp().as_second()) } else { start };
            let utc = jiff::Timestamp::from_second(start).map(|t| t.to_zoned(TimeZone::UTC).date()).unwrap_or(local);
            l.push(format!("RRULE:{}", crate::rrule::shift_days(rule, (utc - local).get_days())));
        }
        if !self.description.is_empty() {
            l.push(format!("DESCRIPTION:{}", text(self.description)));
        }
        if !self.location.is_empty() {
            l.push(format!("LOCATION:{}", text(self.location)));
        }
        if !self.attendees.is_empty() {
            let (name, email) = self.organizer;
            l.push(format!("ORGANIZER{}:mailto:{email}", cn(name)));
            l.push(format!("ATTENDEE{};ROLE=CHAIR;PARTSTAT=ACCEPTED:mailto:{email}", cn(name)));
            for (name, email, optional) in self.attendees {
                let role = if *optional { "OPT-PARTICIPANT" } else { "REQ-PARTICIPANT" };
                l.push(format!("ATTENDEE{};ROLE={role};PARTSTAT=NEEDS-ACTION;RSVP=TRUE;SCHEDULE-AGENT=SERVER:mailto:{email}", cn(name)));
            }
        }
        // Моё напоминание (T2-08б): VALARM в моей копии; приглашения участникам сервер собирает сам.
        if let Some(secs) = self.alert_secs.filter(|s| *s > 0) {
            l.extend([
                "BEGIN:VALARM".to_string(),
                "ACTION:DISPLAY".into(),
                format!("DESCRIPTION:{}", text(self.title)),
                format!("TRIGGER:-{}", crate::format_duration(secs)),
                "END:VALARM".into(),
            ]);
        }
        l.extend(["END:VEVENT".to_string(), "END:VCALENDAR".into()]);
        Ok(join(&l))
    }
}

/// Копия приглашения из письма для моего календаря CalDAV (T2-08в): участники и организатор — как в приглашении, мой
/// ответ — `NEEDS-ACTION`, у всех `SCHEDULE-AGENT=CLIENT` (сервер ничего не рассылает). Повторяющуюся встречу модель
/// события целиком не держит (только признак) — её не копируем: `None`, приглашение остаётся карточкой в письме.
pub fn invite_copy(e: &Event, me: &str, now: i64) -> Option<String> {
    // Серия без известного правила (T2-13) — не копируем: вышла бы одна встреча вместо серии.
    if (e.recurring && e.recurrence.is_none()) || e.uid.is_empty() {
        return None;
    }
    let mut l = vec![
        "BEGIN:VCALENDAR".to_string(),
        "PRODID:-//Postman//Postman//RU".into(),
        "VERSION:2.0".into(),
        "BEGIN:VEVENT".into(),
        format!("UID:{}", text(&e.uid)),
        format!("DTSTAMP:{}", utc_ical(now)),
        format!("DTSTART:{}", utc_ical(e.span.start)),
        format!("DTEND:{}", utc_ical(e.span.end.max(e.span.start))),
        format!("SEQUENCE:{}", e.sequence),
        format!("SUMMARY:{}", text(&e.title)),
    ];
    if let Some(rule) = &e.recurrence {
        l.push(format!("RRULE:{rule}"));
    }
    if !e.description.is_empty() {
        l.push(format!("DESCRIPTION:{}", text(&e.description)));
    }
    if !e.location.is_empty() {
        l.push(format!("LOCATION:{}", text(&e.location)));
    }
    if let Some(o) = e.participants.iter().find(|p| p.owner) {
        l.push(format!("ORGANIZER{};SCHEDULE-AGENT=CLIENT:mailto:{}", cn(&o.name), o.email));
    }
    for p in e.participants.iter().filter(|p| p.attendee) {
        let mine = p.email.eq_ignore_ascii_case(me);
        let stat = if mine { "NEEDS-ACTION" } else { partstat_name(p.status) };
        let role = if p.owner {
            "CHAIR"
        } else if p.optional {
            "OPT-PARTICIPANT"
        } else {
            "REQ-PARTICIPANT"
        };
        l.push(format!("ATTENDEE{};ROLE={role};PARTSTAT={stat};SCHEDULE-AGENT=CLIENT:mailto:{}", cn(&p.name), p.email));
    }
    l.extend(["END:VEVENT".to_string(), "END:VCALENDAR".into()]);
    Some(join(&l))
}

fn partstat_name(s: Partstat) -> &'static str {
    match s {
        Partstat::Accepted => "ACCEPTED",
        Partstat::Tentative => "TENTATIVE",
        Partstat::Declined => "DECLINED",
        Partstat::Delegated => "DELEGATED",
        Partstat::NeedsAction => "NEEDS-ACTION",
    }
}

/// Задача на хранение в календаре CalDAV: VTODO без METHOD, участникам — `SCHEDULE-AGENT=CLIENT` (письма шага
/// поручения шлёт Postman, как JMAP без `sendSchedulingMessages`).
pub fn stored_task(t: &Task, now: i64, tz: &TimeZone) -> String {
    schedule_agent(&t.to_ics(None, now, tz), ScheduleAgent::Client)
}

/// Блок времени задачи (S6-08): «занят», без участников, со ссылкой на задачу (`RELATED-TO;RELTYPE=PARENT`) и
/// меткой `pm.task` — как `tasks::block_event` у JMAP.
pub fn block(uid: &str, title: &str, start: i64, end: i64, task_uid: &str, now: i64) -> String {
    join(&[
        "BEGIN:VCALENDAR".to_string(),
        "PRODID:-//Postman//Postman//RU".into(),
        "VERSION:2.0".into(),
        "BEGIN:VEVENT".into(),
        format!("UID:{}", text(uid)),
        format!("DTSTAMP:{}", utc_ical(now)),
        format!("DTSTART:{}", utc_ical(start)),
        format!("DTEND:{}", utc_ical(end.max(start + 60))),
        format!("SUMMARY:{}", text(title)),
        "TRANSP:OPAQUE".into(),
        "CATEGORIES:pm.task".into(),
        format!("RELATED-TO;RELTYPE=PARENT:{}", text(task_uid)),
        "END:VEVENT".into(),
        "END:VCALENDAR".into(),
    ])
}

/// Строки без переносов (RFC 5545 §3.1).
fn unfold(ics: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in ics.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(rest) = line.strip_prefix([' ', '\t'])
            && let Some(last) = out.last_mut()
        {
            last.push_str(rest);
            continue;
        }
        if !line.is_empty() {
            out.push(line.to_owned());
        }
    }
    out
}

/// Имя свойства строки (`ATTENDEE`, `ORGANIZER`), заглавными; часть до `:` вне кавычек — заголовок.
fn split(line: &str) -> Option<(&str, &str)> {
    let mut quoted = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ':' if !quoted => return Some((&line[..i], &line[i + 1..])),
            _ => {}
        }
    }
    None
}

fn set_param(head: &str, name: &str, value: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in head.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            ';' if !quoted => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    let prefix = format!("{}=", name.to_ascii_uppercase());
    let mut found = false;
    for p in parts.iter_mut().skip(1) {
        if p.to_ascii_uppercase().starts_with(&prefix) {
            *p = format!("{prefix}{value}");
            found = true;
        }
    }
    if !found {
        parts.push(format!("{prefix}{value}"));
    }
    parts.join(";")
}

fn is(head: &str, name: &str) -> bool {
    head.split(';').next().is_some_and(|n| n.eq_ignore_ascii_case(name))
}

/// `SCHEDULE-AGENT` у каждого `ATTENDEE` (прежний — заменяется).
pub fn schedule_agent(ics: &str, agent: ScheduleAgent) -> String {
    let lines: Vec<String> = unfold(ics)
        .into_iter()
        .map(|l| match split(&l) {
            Some((head, value)) if is(head, "ATTENDEE") => format!("{}:{value}", set_param(head, "SCHEDULE-AGENT", agent.ical())),
            _ => l,
        })
        .collect();
    join(&lines)
}

/// Мой ответ в своей копии встречи (участник, RFC 6638 §3.2.2): `PARTSTAT` моей строки `ATTENDEE` во всех
/// компонентах, `RSVP` снят. `None` — меня среди участников нет.
pub fn with_partstat(ics: &str, me: &str, status: Partstat) -> Option<String> {
    let mine = format!("mailto:{}", me.to_ascii_lowercase());
    let mut found = false;
    let lines: Vec<String> = unfold(ics)
        .into_iter()
        .map(|l| match split(&l) {
            Some((head, value)) if is(head, "ATTENDEE") && value.trim().to_ascii_lowercase() == mine => {
                found = true;
                let head = set_param(head, "PARTSTAT", partstat_ical(status));
                let head: Vec<&str> = head.split(';').filter(|p| !p.to_ascii_uppercase().starts_with("RSVP=")).collect();
                format!("{}:{value}", head.join(";"))
            }
            _ => l,
        })
        .collect();
    found.then(|| join(&lines))
}

/// Кто рассылает ответы из моей копии встречи (RFC 6638 §7.1, T2-15): `client` — `SCHEDULE-AGENT=CLIENT` у
/// `ORGANIZER`, сервер ответ не шлёт («без ответа организатору» или ответ письмом с комментарием); иначе параметр
/// снимается — ответ шлёт сервер. Нет `ORGANIZER` — копия как есть.
pub fn with_schedule_agent(ics: &str, client: bool) -> String {
    let lines: Vec<String> = unfold(ics)
        .into_iter()
        .map(|l| match split(&l) {
            Some((head, value)) if is(head, "ORGANIZER") => {
                let head: Vec<&str> = head.split(';').filter(|p| !p.to_ascii_uppercase().starts_with("SCHEDULE-AGENT=")).collect();
                let head = head.join(";");
                let head = if client { set_param(&head, "SCHEDULE-AGENT", "CLIENT") } else { head };
                format!("{head}:{value}")
            }
            _ => l,
        })
        .collect();
    join(&lines)
}

// --- Правка и отмена своей встречи (организатор, S7-07б) ---

/// Что править в ресурсе встречи CalDAV.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit<'a> {
    /// Всю встречу (у серии — всю серию): время сдвигается на `shift` секунд (у серии — каждый экземпляр, правило
    /// остаётся), длительность и остальное — из новой встречи. Сдвиг серии теряет изменённые экземпляры, как у
    /// Outlook: их `RECURRENCE-ID` больше не совпадает с экземплярами.
    Whole { shift: i64 },
    /// Только экземпляр серии: `rid` — его `RECURRENCE-ID` в UTC (`20261102T070000Z`, так их отдаёт `expand`).
    Instance { rid: &'a str },
}

/// Компонент VEVENT ресурса: строки с `BEGIN` по `END` и его `RECURRENCE-ID` (у серии и одиночной — нет).
struct Component {
    lines: Vec<String>,
    rid: Option<String>,
}

/// Ресурс: строки вне VEVENT (заголовок, VTIMEZONE…) до и после, компоненты VEVENT по порядку.
struct Resource {
    head: Vec<String>,
    events: Vec<Component>,
    tail: Vec<String>,
}

fn resource(ics: &str) -> Resource {
    let mut r = Resource { head: Vec::new(), events: Vec::new(), tail: Vec::new() };
    let mut cur: Option<Vec<String>> = None;
    for l in unfold(ics) {
        match cur.as_mut() {
            Some(c) => {
                c.push(l.clone());
                if l.eq_ignore_ascii_case("END:VEVENT") {
                    let lines = cur.take().unwrap_or_default();
                    let rid = lines.iter().find_map(|x| split(x).filter(|(h, _)| is(h, "RECURRENCE-ID")).map(|(h, v)| instant(h, v)));
                    r.events.push(Component { lines, rid });
                }
            }
            None if l.eq_ignore_ascii_case("BEGIN:VEVENT") => cur = Some(vec![l]),
            None if r.events.is_empty() => r.head.push(l),
            None => r.tail.push(l),
        }
    }
    r
}

fn assemble(r: Resource) -> String {
    let mut lines = r.head;
    for e in r.events {
        lines.extend(e.lines);
    }
    lines.extend(r.tail);
    join(&lines)
}

/// Время iCalendar (`20261102T070000Z`, `20261102T100000` с `TZID` или плавающее, `20261102` — дата) → Unix-секунды;
/// `TZID`, которого нет в базе IANA, и плавающее — как UTC (только для сравнения своих же значений).
fn seconds(head: &str, value: &str) -> Option<i64> {
    let v = value.trim();
    if v.len() == 8 {
        let d = jiff::civil::Date::strptime("%Y%m%d", v).ok()?;
        return d.to_datetime(jiff::civil::Time::midnight()).to_zoned(TimeZone::UTC).ok().map(|z| z.timestamp().as_second());
    }
    let civil = jiff::civil::DateTime::strptime("%Y%m%dT%H%M%S", v.trim_end_matches('Z')).ok()?;
    let tz = param(head, "TZID").and_then(|t| TimeZone::get(t.trim_matches('"')).ok()).filter(|_| !v.ends_with('Z')).unwrap_or(TimeZone::UTC);
    civil.to_zoned(tz).ok().map(|z| z.timestamp().as_second())
}

/// `RECURRENCE-ID` и `EXDATE` для сравнения — в UTC (`20261102T070000Z`); не разобрать — как есть.
fn instant(head: &str, value: &str) -> String {
    seconds(head, value).map_or_else(|| value.trim().to_owned(), utc_ical)
}

fn param<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.split(';').skip(1).find_map(|p| p.split_once('=').filter(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v))
}

/// Значение времени, сдвинутое на `secs` секунд в той же форме (UTC, местное с `TZID`, плавающее, дата — днями).
fn shifted(value: &str, secs: i64) -> Option<String> {
    let v = value.trim();
    if v.len() == 8 {
        let d = jiff::civil::Date::strptime("%Y%m%d", v).ok()?;
        return d.checked_add(jiff::Span::new().days(secs.div_euclid(86_400))).ok().map(|d| d.strftime("%Y%m%d").to_string());
    }
    let utc = v.ends_with('Z');
    let civil = jiff::civil::DateTime::strptime("%Y%m%dT%H%M%S", v.trim_end_matches('Z')).ok()?;
    let moved = civil.checked_add(jiff::Span::new().seconds(secs)).ok()?;
    Some(format!("{}{}", moved.strftime("%Y%m%dT%H%M%S"), if utc { "Z" } else { "" }))
}

fn value_of<'a>(lines: &'a [String], name: &str) -> Option<(&'a str, &'a str)> {
    lines.iter().find_map(|l| split(l).filter(|(h, _)| is(h, name)))
}

/// Ответы участников компонента: адрес (строчными) → `PARTSTAT`.
fn statuses(lines: &[String]) -> Vec<(String, String)> {
    lines
        .iter()
        .filter_map(|l| split(l).filter(|(h, _)| is(h, "ATTENDEE")))
        .map(|(h, v)| {
            (v.trim().to_ascii_lowercase().trim_start_matches("mailto:").to_owned(), param(h, "PARTSTAT").unwrap_or("NEEDS-ACTION").to_owned())
        })
        .collect()
}

/// Организатор и участники новой встречи; `keep` — ответы прежних участников остаются (время не менялось), иначе
/// все снова «ждут ответа» (как приглашение заново).
fn people(ev: &NewEvent<'_>, before: &[(String, String)], keep: bool) -> Vec<String> {
    if ev.attendees.is_empty() {
        return Vec::new();
    }
    let (name, email) = ev.organizer;
    let mut l = vec![format!("ORGANIZER{}:mailto:{email}", cn(name)), format!("ATTENDEE{};ROLE=CHAIR;PARTSTAT=ACCEPTED:mailto:{email}", cn(name))];
    for (name, email, optional) in ev.attendees {
        let role = if *optional { "OPT-PARTICIPANT" } else { "REQ-PARTICIPANT" };
        let was = before.iter().find(|(e, _)| e.eq_ignore_ascii_case(email)).map(|(_, s)| s.as_str()).filter(|_| keep);
        let status = match was {
            Some(s) => s.to_owned(),
            None => "NEEDS-ACTION;RSVP=TRUE".into(),
        };
        l.push(format!("ATTENDEE{};ROLE={role};PARTSTAT={status};SCHEDULE-AGENT=SERVER:mailto:{email}", cn(name)));
    }
    l
}

/// Поля встречи, которые правка пишет заново.
const OWN: [&str; 10] = ["DTSTART", "DTEND", "DURATION", "SEQUENCE", "DTSTAMP", "SUMMARY", "DESCRIPTION", "LOCATION", "ORGANIZER", "ATTENDEE"];

fn sequence(lines: &[String]) -> i64 {
    value_of(lines, "SEQUENCE").and_then(|(_, v)| v.trim().parse().ok()).unwrap_or(0)
}

/// Тема, описание, место — строками iCalendar.
fn texts(ev: &NewEvent<'_>) -> Vec<String> {
    let mut l = vec![format!("SUMMARY:{}", text(ev.title))];
    if !ev.description.is_empty() {
        l.push(format!("DESCRIPTION:{}", text(ev.description)));
    }
    if !ev.location.is_empty() {
        l.push(format!("LOCATION:{}", text(ev.location)));
    }
    l
}

/// Новая версия ресурса своей встречи (организатор): изменения участникам рассылает сервер (`SCHEDULE-AGENT=SERVER`),
/// `SEQUENCE` — на единицу больше. `None` — в ресурсе нет встречи (или нет начала).
pub fn edited(ics: &str, ev: &NewEvent<'_>, edit: Edit<'_>, now: i64) -> Option<String> {
    let mut r = resource(ics);
    let master = r.events.iter().position(|c| c.rid.is_none())?;
    let seq = sequence(&r.events[master].lines) + 1;
    match edit {
        Edit::Whole { shift } => {
            let m = &r.events[master].lines;
            let (sh, sv) = value_of(m, "DTSTART")?;
            let new_start = shifted(sv, shift)?;
            let end = shifted(&new_start, ev.duration_secs.max(0))?;
            let old_len = value_of(m, "DTEND").and_then(|(eh, ev)| Some(seconds(eh, ev)? - seconds(sh, sv)?));
            let keep = shift == 0 && old_len == Some(ev.duration_secs);
            let before = statuses(m);
            let tzid = sh.split_once(';').map(|(_, p)| format!(";{p}")).unwrap_or_default();
            let mut lines: Vec<String> = m.iter().filter(|l| !split(l).is_some_and(|(h, _)| OWN.iter().any(|n| is(h, n)))).cloned().collect();
            let end_line = lines.pop().unwrap_or_else(|| "END:VEVENT".into());
            lines.push(format!("DTSTART{tzid}:{new_start}"));
            lines.push(format!("DTEND{tzid}:{end}"));
            lines.push(format!("DTSTAMP:{}", utc_ical(now)));
            lines.push(format!("SEQUENCE:{seq}"));
            lines.extend(texts(ev));
            lines.extend(people(ev, &before, keep));
            lines.push(end_line);
            r.events[master].lines = lines;
            if shift != 0 {
                // Экземпляры сдвинутой серии — на новых местах: прежние исключения к ним не относятся.
                r.events.retain(|c| c.rid.is_none());
            }
        }
        Edit::Instance { rid } => {
            let tz = TimeZone::get(ev.time_zone).unwrap_or(TimeZone::UTC);
            let start = ev.start.to_zoned(tz).ok()?.timestamp().as_second();
            let rid = instant("RECURRENCE-ID", rid);
            let old = r.events.iter().find(|c| c.rid.as_deref() == Some(rid.as_str()));
            let before = statuses(old.map_or(&r.events[master].lines, |c| &c.lines));
            let was = seconds("RECURRENCE-ID", &rid);
            let master_len = {
                let m = &r.events[master].lines;
                value_of(m, "DTEND").zip(value_of(m, "DTSTART")).and_then(|((eh, ev), (sh, sv))| Some(seconds(eh, ev)? - seconds(sh, sv)?))
            };
            let keep = was == Some(start) && master_len == Some(ev.duration_secs);
            let uid = value_of(&r.events[master].lines, "UID").map(|(_, v)| v.to_owned()).unwrap_or_else(|| text(ev.uid));
            let mut lines = vec![
                "BEGIN:VEVENT".to_string(),
                format!("UID:{uid}"),
                format!("RECURRENCE-ID:{rid}"),
                format!("DTSTAMP:{}", utc_ical(now)),
                format!("DTSTART:{}", utc_ical(start)),
                format!("DTEND:{}", utc_ical(start + ev.duration_secs.max(0))),
                format!("SEQUENCE:{seq}"),
            ];
            lines.extend(texts(ev));
            lines.extend(people(ev, &before, keep));
            lines.push("END:VEVENT".into());
            r.events.retain(|c| c.rid.as_deref() != Some(rid.as_str()));
            r.events.push(Component { lines, rid: Some(rid) });
        }
    }
    Some(assemble(r))
}

/// Ресурс серии без экземпляра `rid` (UTC, как у [`Edit::Instance`]): `EXDATE` у серии, его исключение — убрано;
/// сервер рассылает участникам отмену этого экземпляра. `None` — в ресурсе нет серии.
pub fn without_instance(ics: &str, rid: &str, now: i64) -> Option<String> {
    let rid = instant("EXDATE", rid);
    let mut r = resource(ics);
    let master = r.events.iter().position(|c| c.rid.is_none())?;
    let seq = sequence(&r.events[master].lines) + 1;
    let m = &mut r.events[master].lines;
    m.retain(|l| !split(l).is_some_and(|(h, _)| is(h, "SEQUENCE") || is(h, "DTSTAMP")));
    let end = m.pop().unwrap_or_else(|| "END:VEVENT".into());
    m.push(format!("EXDATE:{rid}"));
    m.push(format!("DTSTAMP:{}", utc_ical(now)));
    m.push(format!("SEQUENCE:{seq}"));
    m.push(end);
    r.events.retain(|c| c.rid.as_deref() != Some(rid.as_str()));
    Some(assemble(r))
}

/// `RECURRENCE-ID` экземпляра по `rid` из недели (UTC): у исключения `expand` Stalwart отдаёт вместо исходного
/// `RECURRENCE-ID` его новое начало (`docs/STALWART-GAPS.md` №49) — тогда исключение ищется по началу (`DTSTART`).
pub fn instance_rid(ics: &str, rid: &str) -> String {
    let want = instant("RECURRENCE-ID", rid);
    let r = resource(ics);
    if r.events.iter().any(|c| c.rid.as_deref() == Some(want.as_str())) {
        return want;
    }
    let moved =
        r.events.iter().find(|c| c.rid.is_some() && value_of(&c.lines, "DTSTART").map(|(h, v)| instant(h, v)).as_deref() == Some(want.as_str()));
    moved.and_then(|c| c.rid.clone()).unwrap_or(want)
}

/// Есть ли в ресурсе правило повтора (серия).
pub fn is_series(ics: &str) -> bool {
    resource(ics).events.iter().any(|c| c.rid.is_none() && value_of(&c.lines, "RRULE").is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> i64 {
        s.parse::<jiff::Timestamp>().unwrap().as_second()
    }

    #[test]
    fn meeting_is_a_vevent_scheduled_by_the_server() {
        let attendees = [("User 2", "user2@itt.test", false), ("Переговорная", "room@itt.test", true)];
        let e = NewEvent {
            private: false,
            all_day: false,
            recurrence: None,
            alert_secs: None,
            uid: "u1@postman",
            calendar_id: "",
            title: "Планёрка; итоги",
            description: "",
            location: "Каб. 1",
            start: "2026-10-21T11:00:00".parse().unwrap(),
            time_zone: "Europe/Moscow",
            duration_secs: 1800,
            organizer: ("User 1", "user1@itt.test"),
            attendees: &attendees,
        };
        let ics = e.to_ics(at("2026-10-20T00:00:00Z")).unwrap();
        assert!(ics.contains("DTSTART:20261021T080000Z\r\nDTEND:20261021T083000Z\r\n"), "{ics}");
        assert!(ics.contains("SUMMARY:Планёрка\\; итоги\r\n") && ics.contains("LOCATION:Каб. 1\r\n"));
        assert!(ics.contains("ORGANIZER;CN=\"User 1\":mailto:user1@itt.test\r\n"));
        let lines = unfold(&ics);
        assert!(lines.iter().any(|l| l.starts_with("ATTENDEE;CN=\"User 2\";ROLE=REQ-PARTICIPANT;") && l.contains("SCHEDULE-AGENT=SERVER")));
        assert!(lines.iter().any(|l| l.contains("ROLE=OPT-PARTICIPANT") && l.ends_with("mailto:room@itt.test")));
        // Без участников — просто событие: ни организатора, ни рассылки.
        assert!(!ics.contains("VALARM"));
        // Моё напоминание (T2-08б) — VALARM в моей копии.
        let remind = NewEvent { alert_secs: Some(600), ..e };
        let ics = remind.to_ics(0).unwrap();
        assert!(ics.contains("BEGIN:VALARM\r\nACTION:DISPLAY\r\n"), "{ics}");
        assert!(ics.contains("TRIGGER:-PT10M\r\nEND:VALARM\r\nEND:VEVENT"), "{ics}");
        // Весь день и частная (T2-14).
        let day = NewEvent { all_day: true, private: true, duration_secs: 86_400, ..e };
        let ics = day.to_ics(0).unwrap();
        assert!(ics.contains("DTSTART;VALUE=DATE:20261021\r\nDTEND;VALUE=DATE:20261022\r\n") && ics.contains("CLASS:PRIVATE\r\n"), "{ics}");
        // Повтор (T2-13): RRULE; встреча в 01:00 по Москве — в UTC это вторник 22:00 накануне: дни сдвигаются.
        let weekly = NewEvent { recurrence: Some("FREQ=WEEKLY;BYDAY=WE"), ..e };
        assert!(weekly.to_ics(0).unwrap().contains("RRULE:FREQ=WEEKLY;BYDAY=WE\r\n"));
        let night = NewEvent { start: "2026-10-21T01:00:00".parse().unwrap(), recurrence: Some("FREQ=WEEKLY;BYDAY=WE"), ..e };
        assert!(night.to_ics(0).unwrap().contains("RRULE:FREQ=WEEKLY;BYDAY=TU\r\n"));
        let alone = NewEvent { attendees: &[], ..e };
        assert!(!alone.to_ics(0).unwrap().contains("ORGANIZER"));
    }

    /// Приглашение из письма (T2-08в) — копия: я «не ответил», рассылки нет; повторяющуюся не копируем.
    #[test]
    fn invite_copy_needs_action_without_scheduling() {
        let v = serde_json::json!({ "uid": "inv-1", "title": "Созвон", "utcStart": "2026-10-21T08:00:00Z", "utcEnd": "2026-10-21T09:00:00Z", "participants": {
            "o": { "name": "Орг", "calendarAddress": "mailto:org@vendor.example", "roles": { "owner": true, "attendee": true }, "participationStatus": "accepted" },
            "m": { "calendarAddress": "mailto:me@corp.example", "roles": { "attendee": true }, "participationStatus": "needs-action" }
        }});
        let e = Event::from_jmap(&v, &TimeZone::UTC).unwrap();
        let ics = invite_copy(&e, "ME@corp.example", 0).unwrap();
        let lines = unfold(&ics);
        assert!(lines.iter().any(|l| l.starts_with("ORGANIZER;CN=") && l.ends_with("SCHEDULE-AGENT=CLIENT:mailto:org@vendor.example")));
        assert!(lines.iter().any(|l| l.contains("PARTSTAT=NEEDS-ACTION;SCHEDULE-AGENT=CLIENT:mailto:me@corp.example")));
        assert!(lines.iter().all(|l| !l.contains("SCHEDULE-AGENT=SERVER")));
        assert!(ics.contains("DTSTART:20261021T080000Z\r\n"));
        let mut series = e;
        series.recurring = true;
        assert!(invite_copy(&series, "me@corp.example", 0).is_none(), "правило не известно — не копируем");
        series.recurrence = Some("FREQ=WEEKLY;BYDAY=WE".into());
        assert!(invite_copy(&series, "me@corp.example", 0).unwrap().contains("RRULE:FREQ=WEEKLY;BYDAY=WE\r\n"));
    }

    #[test]
    fn schedule_agent_is_set_on_every_attendee() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VTODO\r\nORGANIZER:mailto:a@x\r\nATTENDEE;PARTSTAT=NEEDS-ACTION;CN=\"B; b\":mailto:b@x\r\nATTENDEE;SCHEDULE-AGENT=SERVER:mailto:c@x\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";
        let out = schedule_agent(ics, ScheduleAgent::Client);
        let lines = unfold(&out);
        assert!(lines.contains(&"ATTENDEE;PARTSTAT=NEEDS-ACTION;CN=\"B; b\";SCHEDULE-AGENT=CLIENT:mailto:b@x".to_string()), "{lines:?}");
        assert!(lines.contains(&"ATTENDEE;SCHEDULE-AGENT=CLIENT:mailto:c@x".to_string()));
        assert!(lines.contains(&"ORGANIZER:mailto:a@x".to_string()));
    }

    #[test]
    fn my_answer_in_my_copy() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nORGANIZER:mailto:a@x\r\nATTENDEE;CN=\"Me\";PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:Me\r\n @X\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:other@x\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let out = with_partstat(ics, "me@x", Partstat::Accepted).unwrap();
        let lines = unfold(&out);
        assert!(lines.iter().any(|l| l.starts_with("ATTENDEE;CN=\"Me\";PARTSTAT=ACCEPTED:") && !l.contains("RSVP")), "{lines:?}");
        assert!(lines.contains(&"ATTENDEE;PARTSTAT=ACCEPTED:mailto:other@x".to_string()));
        assert!(with_partstat(ics, "nobody@x", Partstat::Declined).is_none());
        // Без рассылки сервером (T2-15): у организатора SCHEDULE-AGENT=CLIENT; обратно — параметр снят.
        let quiet = with_schedule_agent(&out, true);
        assert!(unfold(&quiet).contains(&"ORGANIZER;SCHEDULE-AGENT=CLIENT:mailto:a@x".to_string()), "{quiet}");
        assert!(unfold(&with_schedule_agent(&quiet, false)).contains(&"ORGANIZER:mailto:a@x".to_string()));
    }

    #[test]
    fn stored_task_and_block() {
        let mut t = Task::new("t1", "cal", "Отчёт");
        t.participants.push(crate::TaskParticipant {
            key: "p1".into(),
            name: "B".into(),
            email: "b@x".into(),
            role: crate::Role::Assignee,
            status: crate::TaskPartstat::NeedsAction,
        });
        let ics = stored_task(&t, 0, &TimeZone::UTC);
        assert!(!ics.contains("METHOD:") && ics.contains("BEGIN:VTODO"));
        assert!(unfold(&ics).iter().filter(|l| l.starts_with("ATTENDEE")).all(|l| l.contains("SCHEDULE-AGENT=CLIENT")));
        let b = block("b1", "Отчёт", 3600, 7200, "t1", 0);
        assert!(b.contains("DTSTART:19700101T010000Z\r\nDTEND:19700101T020000Z\r\n") && b.contains("RELATED-TO;RELTYPE=PARENT:t1\r\n"));
    }
    fn meeting(attendees: &'static [(&'static str, &'static str, bool)]) -> NewEvent<'static> {
        NewEvent {
            private: false,
            all_day: false,
            recurrence: None,
            alert_secs: None,
            uid: "s1",
            calendar_id: "",
            title: "Планёрка, новая",
            description: "",
            location: "Каб. 2",
            start: "2026-11-02T11:00:00".parse().unwrap(),
            time_zone: "Europe/Moscow",
            duration_secs: 3600,
            organizer: ("", "user1@itt.test"),
            attendees,
        }
    }

    const SERIES: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VTIMEZONE\r\nTZID:Europe/Moscow\r\nEND:VTIMEZONE\r\nBEGIN:VEVENT\r\nUID:s1\r\nDTSTART;TZID=Europe/Moscow:20261102T100000\r\nDTEND;TZID=Europe/Moscow:20261102T110000\r\nRRULE:FREQ=DAILY;COUNT=3\r\nSEQUENCE:2\r\nSUMMARY:Планёрка\r\nORGANIZER:mailto:user1@itt.test\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:user1@itt.test\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:user2@itt.test\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:s1\r\nRECURRENCE-ID;TZID=Europe/Moscow:20261103T100000\r\nDTSTART:20261103T090000Z\r\nDTEND:20261103T100000Z\r\nSUMMARY:Планёрка позже\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    const NEW_TITLE: &str = r"SUMMARY:Планёрка\, новая";

    /// Вся серия: начало сдвигается в своей форме (местное с `TZID`), правило остаётся, `SEQUENCE` растёт; тема,
    /// место, участники — новые; время не менялось — ответы остаются; исключения при сдвиге — уходят.
    #[test]
    fn whole_series_edit() {
        let ev = meeting(&[("", "user2@itt.test", false), ("", "user3@itt.test", true)]);
        let same = edited(SERIES, &ev, Edit::Whole { shift: 0 }, 0).unwrap();
        let lines = unfold(&same);
        assert!(lines.contains(&"DTSTART;TZID=Europe/Moscow:20261102T100000".to_string()) && lines.contains(&"RRULE:FREQ=DAILY;COUNT=3".to_string()));
        assert!(lines.contains(&"SEQUENCE:3".to_string()) && lines.contains(&NEW_TITLE.to_string()), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("PARTSTAT=ACCEPTED;SCHEDULE-AGENT=SERVER:mailto:user2@itt.test")), "ответ остался");
        assert!(lines.iter().any(|l| l.contains("ROLE=OPT-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE") && l.ends_with("user3@itt.test")));
        assert!(lines.iter().any(|l| l.starts_with("RECURRENCE-ID")), "без сдвига исключение — на месте");
        assert!(same.contains("BEGIN:VTIMEZONE"), "пояс ресурса — на месте");

        let later = edited(SERIES, &ev, Edit::Whole { shift: 3600 }, 0).unwrap();
        let lines = unfold(&later);
        assert!(
            lines.contains(&"DTSTART;TZID=Europe/Moscow:20261102T110000".to_string())
                && lines.contains(&"DTEND;TZID=Europe/Moscow:20261102T120000".to_string())
        );
        assert!(lines.iter().any(|l| l.contains("PARTSTAT=NEEDS-ACTION;RSVP=TRUE") && l.ends_with("user2@itt.test")), "перенос — ответы заново");
        assert!(!lines.iter().any(|l| l.starts_with("RECURRENCE-ID")), "исключения сдвинутой серии — уходят");
    }

    /// Один экземпляр: исключение с `RECURRENCE-ID` в UTC (прежнее с тем же экземпляром — заменено), серия не тронута;
    /// отмена экземпляра — `EXDATE`.
    #[test]
    fn instance_edit_and_cancel() {
        let ev = meeting(&[("", "user2@itt.test", false)]);
        let out = edited(SERIES, &ev, Edit::Instance { rid: "20261103T070000Z" }, 0).unwrap();
        let lines = unfold(&out);
        assert_eq!(lines.iter().filter(|l| l.starts_with("RECURRENCE-ID")).count(), 1, "прежнее исключение заменено: {lines:?}");
        assert!(lines.contains(&"RECURRENCE-ID:20261103T070000Z".to_string()) && lines.contains(&"DTSTART:20261102T080000Z".to_string()));
        assert!(lines.contains(&"SUMMARY:Планёрка".to_string()), "серия — прежняя");
        let gone = without_instance(SERIES, "20261103T070000Z", 0).unwrap();
        let lines = unfold(&gone);
        assert!(lines.contains(&"EXDATE:20261103T070000Z".to_string()) && lines.contains(&"SEQUENCE:3".to_string()));
        assert!(!lines.iter().any(|l| l.starts_with("RECURRENCE-ID")));
        assert_eq!(instance_rid(SERIES, "20261103T090000Z"), "20261103T070000Z", "исключение — по новому началу");
        assert_eq!(instance_rid(SERIES, "20261104T070000Z"), "20261104T070000Z");
        assert!(is_series(SERIES) && !is_series("BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:x\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"));
    }

    /// Одиночная встреча Postman (UTC): перенос — новое время, ответы — заново.
    #[test]
    fn single_meeting_moved() {
        let ev = meeting(&[("User 2", "user2@itt.test", false)]);
        let stored = NewEvent { title: "Старая", ..ev }.to_ics(0).unwrap();
        let out = edited(&stored, &ev, Edit::Whole { shift: 7200 }, 0).unwrap();
        assert!(out.contains("DTSTART:20261102T100000Z\r\nDTEND:20261102T110000Z\r\n"), "{out}");
        assert!(out.contains("SEQUENCE:1\r\n") && out.contains(NEW_TITLE));
        assert_eq!(unfold(&out).iter().filter(|l| l.starts_with("ORGANIZER")).count(), 1);
    }
}
