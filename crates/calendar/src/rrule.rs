//! Правило повтора (T2-13): нейтральная форма модели Postman — строка RRULE RFC 5545 (`FREQ=WEEKLY;BYDAY=TU`), как
//! её отдаёт и разбор Exchange (`Recurrence::rrule`). Здесь — перевод в `recurrenceRule` JSCalendar (JMAP, CalDAV через
//! воркер) и обратно. Подмножество: частота, интервал, дни недели (с номером в периоде), числа месяца, месяцы,
//! позиция, число повторов, «до даты», первый день недели. Что вне него — `None`: правило не трогаем.

use jiff::civil::{Date, DateTime};
use serde_json::{Map, Value, json};

const FREQS: [&str; 7] = ["secondly", "minutely", "hourly", "daily", "weekly", "monthly", "yearly"];
const DAYS: [&str; 7] = ["mo", "tu", "we", "th", "fr", "sa", "su"];

/// `recurrenceRule` JSCalendar → RRULE.
pub fn to_rrule(r: &Value) -> Option<String> {
    let freq = r["frequency"].as_str().filter(|f| FREQS.contains(f))?.to_ascii_uppercase();
    let mut parts = vec![format!("FREQ={freq}")];
    if let Some(i) = r["interval"].as_u64().filter(|i| *i > 1) {
        parts.push(format!("INTERVAL={i}"));
    }
    if let Some(days) = r["byDay"].as_array().filter(|d| !d.is_empty()) {
        let list: Vec<String> = days
            .iter()
            .filter_map(|d| {
                let day = d["day"].as_str().filter(|d| DAYS.contains(d))?.to_ascii_uppercase();
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
            let list: Vec<String> = a.iter().map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned)).collect();
            parts.push(format!("{name}={}", list.join(",")));
        }
    }
    if let Some(c) = r["count"].as_u64() {
        parts.push(format!("COUNT={c}"));
    }
    if let Some(u) = r["until"].as_str() {
        let until = u
            .parse::<DateTime>()
            .map(|d| d.strftime("%Y%m%dT%H%M%S").to_string())
            .or_else(|_| u.parse::<Date>().map(|d| d.strftime("%Y%m%d").to_string()));
        parts.push(format!("UNTIL={}", until.ok()?));
    }
    if let Some(w) = r["firstDayOfWeek"].as_str().filter(|d| DAYS.contains(d)) {
        parts.push(format!("WKST={}", w.to_ascii_uppercase()));
    }
    Some(parts.join(";"))
}

/// RRULE → `recurrenceRule` JSCalendar (`@type: RecurrenceRule`). Часть вне подмножества или ошибка — `None`.
pub fn from_rrule(s: &str) -> Option<Value> {
    let mut r = Map::new();
    r.insert("@type".into(), json!("RecurrenceRule"));
    for part in s.trim().trim_start_matches("RRULE:").split(';').filter(|p| !p.is_empty()) {
        let (k, v) = part.split_once('=')?;
        let v = v.trim();
        match k.trim().to_ascii_uppercase().as_str() {
            "FREQ" => {
                let f = v.to_ascii_lowercase();
                r.insert("frequency".into(), json!(FREQS.iter().find(|x| **x == f)?));
            }
            "INTERVAL" => {
                r.insert("interval".into(), json!(v.parse::<u32>().ok()?));
            }
            "COUNT" => {
                r.insert("count".into(), json!(v.parse::<u32>().ok()?));
            }
            "UNTIL" => {
                let d = v.trim_end_matches('Z');
                let until = if d.len() > 8 {
                    DateTime::strptime("%Y%m%dT%H%M%S", d).ok()?.strftime("%Y-%m-%dT%H:%M:%S").to_string()
                } else {
                    Date::strptime("%Y%m%d", d).ok()?.strftime("%Y-%m-%dT23:59:59").to_string()
                };
                r.insert("until".into(), json!(until));
            }
            "BYDAY" => {
                let days: Option<Vec<Value>> = v
                    .split(',')
                    .map(|d| {
                        let d = d.trim().to_ascii_lowercase();
                        let at = d.len().checked_sub(2)?;
                        let (n, day) = d.split_at(at);
                        let day = DAYS.iter().find(|x| **x == day)?;
                        Some(if n.is_empty() {
                            json!({ "@type": "NDay", "day": day })
                        } else {
                            json!({ "@type": "NDay", "day": day, "nthOfPeriod": n.parse::<i32>().ok()? })
                        })
                    })
                    .collect();
                r.insert("byDay".into(), Value::Array(days?));
            }
            "BYMONTHDAY" | "BYSETPOS" => {
                let list: Option<Vec<Value>> = v.split(',').map(|x| x.trim().parse::<i32>().ok().map(Value::from)).collect();
                let key = if k.eq_ignore_ascii_case("BYMONTHDAY") { "byMonthDay" } else { "bySetPosition" };
                r.insert(key.into(), Value::Array(list?));
            }
            "BYMONTH" => {
                let list: Vec<Value> = v.split(',').map(|x| Value::from(x.trim())).collect();
                r.insert("byMonth".into(), Value::Array(list));
            }
            "WKST" => {
                let d = v.to_ascii_lowercase();
                r.insert("firstDayOfWeek".into(), json!(DAYS.iter().find(|x| **x == d)?));
            }
            _ => return None,
        }
    }
    r.contains_key("frequency").then_some(Value::Object(r))
}

/// Сдвиг дней правила на `delta` дней (−1, 0, +1): дни недели `BYDAY` и числа `BYMONTHDAY` — для начала в другом
/// поясе (UTC). Ноль — правило как есть.
pub fn shift_days(rule: &str, delta: i32) -> String {
    if delta == 0 {
        return rule.to_owned();
    }
    let codes = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"];
    let shift_day = |d: &str| -> String {
        let at = d.len().saturating_sub(2);
        let (n, day) = d.split_at(at);
        match codes.iter().position(|c| *c == day) {
            Some(i) => format!("{n}{}", codes[(i as i32 + delta).rem_euclid(7) as usize]),
            None => d.to_owned(),
        }
    };
    rule.split(';')
        .map(|part| match part.split_once('=') {
            Some(("BYDAY", v)) => format!("BYDAY={}", v.split(',').map(shift_day).collect::<Vec<_>>().join(",")),
            Some(("BYMONTHDAY", v)) => {
                let days: Vec<String> = v
                    .split(',')
                    .map(|d| {
                        d.parse::<i32>()
                            .map(|n| if n > 0 { (n + delta).clamp(1, 31).to_string() } else { n.to_string() })
                            .unwrap_or_else(|_| d.to_owned())
                    })
                    .collect();
                format!("BYMONTHDAY={}", days.join(","))
            }
            _ => part.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(";")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for s in [
            "FREQ=WEEKLY;BYDAY=TU",
            "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,FR;COUNT=10",
            "FREQ=MONTHLY;BYDAY=2TU;UNTIL=20271231T000000",
            "FREQ=YEARLY;BYMONTHDAY=8;BYMONTH=3",
            "FREQ=DAILY",
        ] {
            assert_eq!(to_rrule(&from_rrule(s).unwrap()).as_deref(), Some(s), "{s}");
        }
        // Exchange отдаёт INTERVAL=1 и дату UNTIL; WKST — первый день недели.
        let v = from_rrule("FREQ=WEEKLY;INTERVAL=1;BYDAY=MO,WE;WKST=MO;UNTIL=20261230").unwrap();
        assert_eq!(v["until"], "2026-12-30T23:59:59");
        assert_eq!(to_rrule(&v).as_deref(), Some("FREQ=WEEKLY;BYDAY=MO,WE;UNTIL=20261230T235959;WKST=MO"));
        assert!(from_rrule("FREQ=WEEKLY;BYHOUR=9").is_none(), "вне подмножества — не трогаем");
        assert!(from_rrule("BYDAY=MO").is_none(), "без частоты");
        assert_eq!(shift_days("FREQ=WEEKLY;BYDAY=MO,TU", -1), "FREQ=WEEKLY;BYDAY=SU,MO");
        assert_eq!(shift_days("FREQ=MONTHLY;BYDAY=2TU", -1), "FREQ=MONTHLY;BYDAY=2MO");
        assert_eq!(shift_days("FREQ=MONTHLY;BYMONTHDAY=20", -1), "FREQ=MONTHLY;BYMONTHDAY=19");
    }
}
