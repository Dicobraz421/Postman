//! Приглашения по почте (iMIP, RFC 6047): разбор результата воркера и ответ METHOD:REPLY.

use jiff::tz::TimeZone;
use serde_json::Value;

use crate::{Event, Partstat};

/// Приглашение из вложения `text/calendar`.
#[derive(Debug, Clone)]
pub struct Invitation {
    /// REQUEST, CANCEL, REPLY, COUNTER…
    pub method: String,
    pub event: Event,
    pub sequence: i64,
    /// Исходный объект JSCalendar Event — для импорта в календарь.
    pub raw: Value,
}

impl Invitation {
    /// Из ответа воркера `{ "method", "data": JSCalendar }` (событие или группа с событиями).
    pub fn from_worker(v: &Value, default_tz: &TimeZone) -> Result<Self, String> {
        let data = &v["data"];
        let raw = if data["@type"] == "Group" {
            data["entries"].as_array().and_then(|e| e.iter().find(|x| x["@type"] == "Event")).cloned()
        } else if data["@type"] == "Event" {
            Some(data.clone())
        } else {
            None
        }
        .ok_or("в приглашении нет события")?;
        let mut event = Event::from_jmap(&raw, default_tz)?;
        if event.id.is_empty() {
            event.id = event.uid.clone();
        }
        let method = match v["method"].as_str() {
            Some(m) if !m.is_empty() => m.to_ascii_uppercase(),
            _ => raw["method"].as_str().unwrap_or("").to_ascii_uppercase(),
        };
        Ok(Self { method, sequence: raw["sequence"].as_i64().unwrap_or(0), event, raw })
    }

    /// Мой текущий ответ, если я в списке участников.
    pub fn my_status(&self, me: &str) -> Option<Partstat> {
        self.event.participants.iter().find(|p| p.email.eq_ignore_ascii_case(me)).map(|p| p.status)
    }

    /// Ключ моего участника в `participants` (для патча статуса).
    pub fn my_participant_id(&self, me: &str) -> Option<String> {
        participant_id(&self.raw, me)
    }

    pub fn organizer_email(&self) -> Option<&str> {
        self.event.organizer().map(|p| p.email.as_str()).filter(|e| !e.is_empty())
    }

    /// Копия события с моим ответом — для импорта в свой календарь.
    pub fn with_my_status(&self, me: &str, status: Partstat) -> Value {
        let mut raw = self.raw.clone();
        if let Some(id) = participant_id(&raw, me) {
            raw["participants"][&id]["participationStatus"] = Value::from(partstat_json(status));
        }
        if let Some(o) = raw.as_object_mut() {
            o.remove("method");
            o.remove("iCalendar");
        }
        raw
    }
}

/// Ключ участника в объекте события по адресу.
pub fn participant_id(event: &Value, email: &str) -> Option<String> {
    let parts = event["participants"].as_object()?;
    parts
        .iter()
        .find(|(_, p)| {
            ["email", "calendarAddress"]
                .iter()
                .filter_map(|k| p[*k].as_str())
                .chain(p["sendTo"]["imip"].as_str())
                .any(|a| strip_mailto(a).eq_ignore_ascii_case(email))
        })
        .map(|(k, _)| k.clone())
}

/// `mailto:` в адресе календаря — в любом регистре (`MAILTO:` из iCalendar).
fn strip_mailto(a: &str) -> &str {
    match a.get(..7) {
        Some(p) if p.eq_ignore_ascii_case("mailto:") => &a[7..],
        _ => a,
    }
}

pub fn partstat_json(s: Partstat) -> &'static str {
    match s {
        Partstat::Accepted => "accepted",
        Partstat::Declined => "declined",
        Partstat::Tentative => "tentative",
        Partstat::Delegated => "delegated",
        Partstat::NeedsAction => "needs-action",
    }
}

fn partstat_ical(s: Partstat) -> &'static str {
    match s {
        Partstat::Accepted => "ACCEPTED",
        Partstat::Declined => "DECLINED",
        Partstat::Tentative => "TENTATIVE",
        Partstat::Delegated => "DELEGATED",
        Partstat::NeedsAction => "NEEDS-ACTION",
    }
}

/// Тема ответа, как в Outlook.
pub fn reply_subject(title: &str, s: Partstat) -> String {
    let prefix = match s {
        Partstat::Accepted => "Принято",
        Partstat::Tentative => "Под вопросом",
        Partstat::Declined => "Отклонено",
        _ => "Ответ",
    };
    format!("{prefix}: {title}")
}

/// Ответ организатору: iCalendar METHOD:REPLY с моим PARTSTAT (RFC 5546 §3.2.3).
pub fn imip_reply(inv: &Invitation, me: (&str, &str), status: Partstat, now: i64) -> String {
    let utc = |t: i64| jiff::Timestamp::from_second(t).map(|t| t.strftime("%Y%m%dT%H%M%SZ").to_string()).unwrap_or_default();
    let e = &inv.event;
    let org = e.organizer();
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "PRODID:-//Postman//Postman//RU".into(),
        "VERSION:2.0".into(),
        "METHOD:REPLY".into(),
        "BEGIN:VEVENT".into(),
        format!("UID:{}", text(&e.uid)),
        format!("SEQUENCE:{}", inv.sequence),
        format!("DTSTAMP:{}", utc(now)),
        format!("DTSTART:{}", utc(e.span.start)),
        format!("DTEND:{}", utc(e.span.end)),
        format!("SUMMARY:{}", text(&e.title)),
    ];
    if let Some(o) = org {
        lines.push(format!("ORGANIZER{}:mailto:{}", cn(&o.name), o.email));
    }
    let (name, email) = me;
    lines.push(format!("ATTENDEE;PARTSTAT={}{}:mailto:{email}", partstat_ical(status), cn(name)));
    lines.extend(["END:VEVENT".to_string(), "END:VCALENDAR".into()]);
    lines.iter().map(|l| fold(l)).collect::<Vec<_>>().join("\r\n") + "\r\n"
}

/// Экранирование TEXT (RFC 5545 §3.3.11).
pub(crate) fn text(s: &str) -> String {
    s.replace('\\', "\\\\").replace(';', "\\;").replace(',', "\\,").replace("\r\n", "\\n").replace('\n', "\\n")
}

/// Параметр CN: в кавычках, без кавычек и переводов строк внутри.
fn cn(name: &str) -> String {
    let n: String = name.chars().filter(|c| !matches!(c, '"' | '\r' | '\n')).collect();
    if n.is_empty() { String::new() } else { format!(";CN=\"{n}\"") }
}

/// Перенос строк длиннее 75 октетов (RFC 5545 §3.1), не разрывая UTF-8.
pub(crate) fn fold(line: &str) -> String {
    let mut out = String::new();
    let mut len = 0;
    for c in line.chars() {
        let w = c.len_utf8();
        if len + w > 75 {
            out.push_str("\r\n ");
            len = 1;
        }
        out.push(c);
        len += w;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn invite() -> Invitation {
        let v = json!({
            "method": "REQUEST",
            "data": { "@type": "Group", "entries": [{
                "@type": "Event",
                "uid": "u-1@partner.example",
                "title": "Обсуждение пилота SIEM, этап 1",
                "start": "2026-10-21T11:00:00",
                "timeZone": "Europe/Moscow",
                "duration": "PT1H",
                "sequence": 2,
                "participants": {
                    "p1": { "calendarAddress": "mailto:ivan@partner.example", "name": "Иван Петров", "roles": { "owner": true } },
                    "p2": { "calendarAddress": "mailto:me@corp.example", "name": "Я", "participationStatus": "needs-action", "roles": { "required": true } }
                }
            }]}
        });
        Invitation::from_worker(&v, &TimeZone::UTC).unwrap()
    }

    #[test]
    fn parses_group_and_finds_me() {
        let inv = invite();
        assert_eq!(inv.method, "REQUEST");
        assert_eq!(inv.sequence, 2);
        assert_eq!(inv.organizer_email(), Some("ivan@partner.example"));
        assert_eq!(inv.my_status("ME@corp.example"), Some(Partstat::NeedsAction));
        assert_eq!(inv.my_participant_id("me@corp.example").as_deref(), Some("p2"));
        let mine = inv.with_my_status("me@corp.example", Partstat::Accepted);
        assert_eq!(mine["participants"]["p2"]["participationStatus"], "accepted");
        assert!(mine.get("method").is_none());
    }

    #[test]
    fn reply_is_valid_ical() {
        let inv = invite();
        let ics = imip_reply(&inv, ("Я", "me@corp.example"), Partstat::Accepted, 1_792_000_000);
        assert!(ics.contains("METHOD:REPLY\r\n"));
        assert!(ics.contains("UID:u-1@partner.example\r\n"));
        assert!(ics.contains("SEQUENCE:2\r\n"));
        assert!(ics.contains("SUMMARY:Обсуждение пилота SIEM\\, этап 1"));
        assert!(ics.contains("ATTENDEE;PARTSTAT=ACCEPTED;CN=\"Я\":mailto:me@corp.example"));
        // 11:00 МСК = 08:00 UTC.
        assert!(ics.contains("DTSTART:20261021T080000Z"));
        assert!(ics.lines().all(|l| l.len() <= 75), "строки свёрнуты");
        assert_eq!(reply_subject("Встреча", Partstat::Tentative), "Под вопросом: Встреча");
    }

    /// Настоящее приглашение Outlook целиком: iCalendar → воркер → карточка.
    #[test]
    fn outlook_invitation_end_to_end() {
        let ics = include_bytes!("../../worker/tests/fixtures/invite-outlook.ics");
        let v = postman_worker::parse_calendar(ics).unwrap();
        let inv = Invitation::from_worker(&v, &TimeZone::UTC).unwrap();
        assert_eq!(inv.method, "REQUEST");
        assert_eq!(inv.event.title, "Обсуждение пилота SIEM");
        assert_eq!(inv.event.location, "Teams");
        assert_eq!(inv.organizer_email(), Some("ivan@partner.example"));
        assert_eq!(inv.my_status("ME@corp.example"), Some(Partstat::NeedsAction));
        assert_eq!(inv.my_status("maria@partner.example"), Some(Partstat::Accepted));
        // «Russian Standard Time» = UTC+3: 11:00 по Москве — 08:00 UTC.
        assert_eq!(inv.event.span.start, jiff::civil::date(2026, 10, 21).at(8, 0, 0, 0).to_zoned(TimeZone::UTC).unwrap().timestamp().as_second());
        assert_eq!(inv.event.span.end - inv.event.span.start, 3600);
        let reply = imip_reply(&inv, ("Я", "me@corp.example"), Partstat::Accepted, 0);
        assert!(reply.contains("UID:040000008200E00074C5B7101A82E008000000001@partner.example"));
        // Наш ответ разбирается тем же парсером.
        let back = postman_worker::parse_calendar(reply.as_bytes()).unwrap();
        assert_eq!(back["method"], "REPLY");
    }
}
