//! Правило повтора (T2-13): нейтральная форма модели Postman — строка RRULE RFC 5545 (`FREQ=WEEKLY;BYDAY=TU`), как
//! её отдаёт и разбор Exchange (`Recurrence::rrule`). Здесь — перевод в `recurrenceRule` JSCalendar (JMAP, CalDAV через
//! воркер) и обратно. Подмножество: частота, интервал, дни недели (с номером в периоде), числа месяца, месяцы,
//! позиция, число повторов, «до даты», первый день недели. Что вне него — `None`: правило не трогаем.
//!
//! Конец серии (T3-08) — [`End`]: `COUNT` или `UNTIL`. В модели `UNTIL` — местное время встречи (как `until`
//! JSCalendar и дата Exchange), без `Z`; в UTC его переводит запись CalDAV ([`until_utc`]).

use jiff::civil::{Date, DateTime};
use jiff::tz::TimeZone;
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

/// Конец серии (T3-08): без конца, до даты включительно (`UNTIL`), после N встреч (`COUNT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum End {
    #[default]
    Never,
    Until(Date),
    Count(u32),
}

/// Части правила: (ключ в верхнем регистре, значение).
fn parts(rule: &str) -> impl Iterator<Item = (String, &str)> {
    rule.trim().trim_start_matches("RRULE:").split(';').filter_map(|p| {
        let (k, v) = p.split_once('=')?;
        Some((k.trim().to_ascii_uppercase(), v.trim()))
    })
}

/// Конец серии правила. `UNTIL` — дата как записана (день включён); `COUNT=0` и нечитаемое — «без конца».
pub fn end_of(rule: &str) -> End {
    let mut end = End::Never;
    for (k, v) in parts(rule) {
        match k.as_str() {
            // RFC 5545: `COUNT` и `UNTIL` вместе не бывают; встретились оба — число повторов.
            "COUNT" => {
                if let Some(n) = v.parse::<u32>().ok().filter(|n| *n > 0) {
                    return End::Count(n);
                }
            }
            "UNTIL" => {
                if let Some(d) = v.get(..8).and_then(|d| Date::strptime("%Y%m%d", d).ok()) {
                    end = End::Until(d);
                }
            }
            _ => {}
        }
    }
    end
}

/// Правило с другим концом серии: прежние `COUNT` и `UNTIL` убраны. «До даты» — конец этого дня по местному
/// времени встречи (`UNTIL=20261230T235959`): так его отдаёт и JMAP — правило не меняется от круга через сервер.
pub fn with_end(rule: &str, end: End) -> String {
    let mut out: Vec<String> = parts(rule).filter(|(k, _)| k != "COUNT" && k != "UNTIL").map(|(k, v)| format!("{k}={v}")).collect();
    match end {
        End::Never => {}
        End::Until(d) => out.push(format!("UNTIL={}T235959", d.strftime("%Y%m%d"))),
        End::Count(n) => out.push(format!("COUNT={}", n.max(1))),
    }
    out.join(";")
}

/// `UNTIL` для iCalendar с началом в UTC (CalDAV): местное время встречи в поясе `tz` → UTC с `Z` (RFC 5545
/// §3.3.10: вид `UNTIL` — как у `DTSTART`); дата — конец дня. У «весь день» (`DTSTART;VALUE=DATE`) — дата.
/// Уже в UTC или не разобрать — как есть.
pub fn until_utc(rule: &str, tz: &TimeZone, all_day: bool) -> String {
    let convert = |v: &str| -> Option<String> {
        if all_day {
            return v.get(..8).filter(|d| Date::strptime("%Y%m%d", d).is_ok()).map(str::to_owned);
        }
        if v.ends_with(['Z', 'z']) {
            return None;
        }
        let local = if v.len() > 8 { DateTime::strptime("%Y%m%dT%H%M%S", v).ok()? } else { Date::strptime("%Y%m%d", v).ok()?.at(23, 59, 59, 0) };
        let utc = local.to_zoned(tz.clone()).ok()?.with_time_zone(TimeZone::UTC);
        Some(utc.strftime("%Y%m%dT%H%M%SZ").to_string())
    };
    rule.split(';')
        .map(|part| match part.split_once('=') {
            Some((k, v)) if k.trim().eq_ignore_ascii_case("UNTIL") => convert(v.trim()).map_or_else(|| part.to_owned(), |u| format!("UNTIL={u}")),
            _ => part.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(";")
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

    /// Конец серии (T3-08): разбор, замена и перевод `UNTIL` в UTC.
    #[test]
    fn series_end() {
        let d = |s: &str| s.parse::<Date>().unwrap();
        assert_eq!(end_of("FREQ=WEEKLY;BYDAY=TU"), End::Never);
        assert_eq!(end_of("FREQ=WEEKLY;COUNT=1"), End::Count(1));
        assert_eq!(end_of("RRULE:FREQ=DAILY;count=12"), End::Count(12));
        assert_eq!(end_of("FREQ=DAILY;COUNT=0"), End::Never, "ноль встреч — не конец");
        // `UNTIL`: дата (Exchange), местное время (JMAP), UTC (чужой iCalendar) — день как записан.
        assert_eq!(end_of("FREQ=WEEKLY;UNTIL=20261230"), End::Until(d("2026-12-30")));
        assert_eq!(end_of("FREQ=WEEKLY;UNTIL=20261230T235959"), End::Until(d("2026-12-30")));
        assert_eq!(end_of("FREQ=WEEKLY;UNTIL=20261230T090000Z;WKST=MO"), End::Until(d("2026-12-30")));
        assert_eq!(end_of("FREQ=WEEKLY;UNTIL=вчера"), End::Never);
        assert_eq!(end_of("FREQ=WEEKLY;UNTIL=20261230;COUNT=3"), End::Count(3));

        let rule = "FREQ=WEEKLY;BYDAY=MO,WE";
        assert_eq!(with_end(rule, End::Never), rule);
        assert_eq!(with_end(rule, End::Count(5)), "FREQ=WEEKLY;BYDAY=MO,WE;COUNT=5");
        assert_eq!(with_end(rule, End::Until(d("2026-12-30"))), "FREQ=WEEKLY;BYDAY=MO,WE;UNTIL=20261230T235959");
        // Замена конца: прежний убран; «без конца» — правило без `COUNT` и `UNTIL`.
        assert_eq!(with_end("FREQ=DAILY;COUNT=3;WKST=MO", End::Until(d("2027-01-01"))), "FREQ=DAILY;WKST=MO;UNTIL=20270101T235959");
        assert_eq!(with_end("FREQ=DAILY;UNTIL=20261230", End::Never), "FREQ=DAILY");
        // Через JSCalendar и обратно — то же правило и тот же конец.
        for end in [End::Count(1), End::Count(10), End::Until(d("2026-12-30"))] {
            let s = with_end(rule, end);
            assert_eq!(to_rrule(&from_rrule(&s).unwrap()).as_deref(), Some(s.as_str()));
            assert_eq!(end_of(&s), end);
        }
        let v = from_rrule(&with_end(rule, End::Until(d("2026-12-30")))).unwrap();
        assert_eq!((v["until"].as_str(), v.get("count")), (Some("2026-12-30T23:59:59"), None));
        assert_eq!(from_rrule(&with_end(rule, End::Count(1))).unwrap()["count"], 1);

        // CalDAV: начало в UTC — и `UNTIL` в UTC; конец 30 декабря по Москве — 20:59:59 UTC.
        let msk = TimeZone::get("Europe/Moscow").unwrap();
        assert_eq!(until_utc("FREQ=WEEKLY;UNTIL=20261230T235959", &msk, false), "FREQ=WEEKLY;UNTIL=20261230T205959Z");
        assert_eq!(until_utc("FREQ=WEEKLY;UNTIL=20261230;WKST=MO", &msk, false), "FREQ=WEEKLY;UNTIL=20261230T205959Z;WKST=MO");
        // К востоку от UTC после полуночи и к западу вечером — дата в UTC другая.
        let ny = TimeZone::get("America/New_York").unwrap();
        assert_eq!(until_utc("FREQ=DAILY;UNTIL=20261230T235959", &ny, false), "FREQ=DAILY;UNTIL=20261231T045959Z");
        assert_eq!(until_utc("FREQ=DAILY;UNTIL=20261230T090000Z", &msk, false), "FREQ=DAILY;UNTIL=20261230T090000Z", "уже UTC");
        assert_eq!(until_utc("FREQ=DAILY;UNTIL=20261230T235959", &msk, true), "FREQ=DAILY;UNTIL=20261230", "весь день — дата");
        assert_eq!(until_utc("FREQ=DAILY;COUNT=3", &msk, false), "FREQ=DAILY;COUNT=3");
    }
}
