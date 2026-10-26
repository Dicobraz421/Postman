//! Повтор задач и напоминания (S6-07, решения 7 и 8 S6). Без I/O.
//!
//! Повтор — подмножество RRULE (JSCalendar 2.0 `recurrenceRule`): каждые N дней, по будням, по неделе (дни недели),
//! по месяцу (число), по году; ещё «после выполнения через N дней» — своё свойство `X-POSTMAN-REPEAT-AFTER` (в RRULE
//! такого нет). Редкое правило (2-й вторник месяца) — [`Rule::Other`]: показывается текстом, правится только целиком.
//! Выполнение повторяющейся задачи — следующая копия со сдвигом срока ([`next_task`]).
//!
//! Число месяца, которого нет (31-е в ноябре), — последний день месяца; 29 февраля в невисокосный год — 28-е (как
//! в Outlook; RRULE пропустил бы такие месяцы — Postman считает следующий срок сам). Время срока — местное
//! («плавающее»): при смене часового пояса и перехода на летнее время задача остаётся в те же часы.
//!
//! Напоминания — [`plan`]: что сработало с прошлой проверки и когда следующее (один таймер, как у сроков писем).

use jiff::ToSpan;
use jiff::civil::{Date, DateTime, Weekday};
use jiff::tz::TimeZone;
use postman_calendar::{Anchor, Moment, Task, Trigger};
use serde_json::{Value, json};

/// Частота повтора.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Freq {
    Daily,
    /// Пн–Пт.
    Weekdays,
    /// По дням недели (пусто — в день срока).
    Weekly(Vec<Weekday>),
    /// Число месяца.
    Monthly(i8),
    Yearly,
}

/// Повтор задачи.
#[derive(Debug, Clone, PartialEq)]
pub enum Rule {
    /// От срока: частота и интервал (каждые N).
    Every { freq: Freq, interval: u32 },
    /// Через N дней после выполнения.
    AfterDone(u32),
    /// Правило другого клиента вне подмножества — как есть.
    Other(Value),
}

const WEEKDAYS: [(&str, Weekday); 7] = [
    ("mo", Weekday::Monday),
    ("tu", Weekday::Tuesday),
    ("we", Weekday::Wednesday),
    ("th", Weekday::Thursday),
    ("fr", Weekday::Friday),
    ("sa", Weekday::Saturday),
    ("su", Weekday::Sunday),
];

fn wd_code(w: Weekday) -> &'static str {
    WEEKDAYS.iter().find(|(_, x)| *x == w).map(|(c, _)| *c).unwrap_or("mo")
}

/// Повтор задачи (или `None`).
pub fn rule_of(t: &Task) -> Option<Rule> {
    if let Some(n) = t.repeat_after {
        return Some(Rule::AfterDone(n));
    }
    let r = t.recurrence.as_ref()?;
    let interval = r["interval"].as_u64().unwrap_or(1).max(1) as u32;
    // Ограничения и уточнения вне подмножества — правило как есть.
    let has = |k: &str| r.get(k).is_some_and(|v| !v.is_null());
    if has("bySetPosition") || has("byYearDay") || has("byWeekNo") || has("byHour") || has("count") || has("until") {
        return Some(Rule::Other(r.clone()));
    }
    let days: Vec<(Option<i64>, Weekday)> = r["byDay"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| {
            let code = d["day"].as_str()?;
            let w = WEEKDAYS.iter().find(|(c, _)| *c == code)?.1;
            Some((d["nthOfPeriod"].as_i64(), w))
        })
        .collect();
    let freq = match r["frequency"].as_str()? {
        "daily" if days.is_empty() => Freq::Daily,
        "weekly" if days.iter().all(|(n, _)| n.is_none()) => {
            let set: Vec<Weekday> = days.iter().map(|(_, w)| *w).collect();
            let weekdays = [Weekday::Monday, Weekday::Tuesday, Weekday::Wednesday, Weekday::Thursday, Weekday::Friday];
            if interval == 1 && set.len() == 5 && weekdays.iter().all(|w| set.contains(w)) { Freq::Weekdays } else { Freq::Weekly(set) }
        }
        "monthly" if days.is_empty() => match r["byMonthDay"].as_array().map(|a| a.as_slice()) {
            Some([d]) => Freq::Monthly(d.as_i64().unwrap_or(1).clamp(1, 31) as i8),
            None => Freq::Monthly(0),
            _ => return Some(Rule::Other(r.clone())),
        },
        "yearly" if days.is_empty() && !has("byMonth") => Freq::Yearly,
        _ => return Some(Rule::Other(r.clone())),
    };
    Some(Rule::Every { freq, interval })
}

/// Записать повтор в задачу (`None` — не повторяется). Число месяца по умолчанию (0) — из срока.
pub fn set_rule(t: &mut Task, rule: Option<Rule>) {
    t.recurrence = None;
    t.repeat_after = None;
    match rule {
        None => {}
        Some(Rule::AfterDone(n)) => t.repeat_after = Some(n.max(1)),
        Some(Rule::Other(v)) => t.recurrence = Some(v),
        Some(Rule::Every { freq, interval }) => {
            let mut r = match freq {
                Freq::Daily => json!({ "frequency": "daily" }),
                Freq::Weekdays => {
                    let days: Vec<Value> = ["mo", "tu", "we", "th", "fr"].iter().map(|d| json!({ "day": d })).collect();
                    json!({ "frequency": "weekly", "byDay": days })
                }
                Freq::Weekly(days) if days.is_empty() => json!({ "frequency": "weekly" }),
                Freq::Weekly(days) => {
                    let days: Vec<Value> = days.iter().map(|w| json!({ "day": wd_code(*w) })).collect();
                    json!({ "frequency": "weekly", "byDay": days })
                }
                Freq::Monthly(0) => json!({ "frequency": "monthly" }),
                Freq::Monthly(d) => json!({ "frequency": "monthly", "byMonthDay": [d] }),
                Freq::Yearly => json!({ "frequency": "yearly" }),
            };
            if interval > 1 {
                r["interval"] = json!(interval);
            }
            t.recurrence = Some(r);
        }
    }
}

/// Последний день месяца.
fn last_day(y: i16, m: i8) -> i8 {
    Date::new(y, m, 1).map(|d| d.days_in_month()).unwrap_or(28)
}

/// Число месяца `day` в месяце (y, m): нет такого — последний день.
fn clamp_day(y: i16, m: i8, day: i8) -> Option<Date> {
    Date::new(y, m, day.min(last_day(y, m))).ok()
}

/// Следующий срок после `due` (дата срока), выполнено `done` (дата). `None` — правило вне подмножества.
pub fn next_date(rule: &Rule, due: Date, done: Date) -> Option<Date> {
    match rule {
        Rule::AfterDone(n) => done.checked_add(i64::from(*n).days()).ok(),
        Rule::Other(_) => None,
        Rule::Every { freq, interval } => {
            let n = i64::from(*interval);
            match freq {
                Freq::Daily => due.checked_add(n.days()).ok(),
                Freq::Weekdays => {
                    let mut d = due.checked_add(1.days()).ok()?;
                    while matches!(d.weekday(), Weekday::Saturday | Weekday::Sunday) {
                        d = d.checked_add(1.days()).ok()?;
                    }
                    Some(d)
                }
                Freq::Weekly(days) if days.is_empty() => due.checked_add((7 * n).days()).ok(),
                Freq::Weekly(days) => {
                    // Позже на этой неделе (с понедельника), иначе — первый день через N недель.
                    let monday = due.checked_sub(i64::from(due.weekday().to_monday_zero_offset()).days()).ok()?;
                    let mut sorted: Vec<i8> = days.iter().map(|w| w.to_monday_zero_offset()).collect();
                    sorted.sort();
                    sorted.dedup();
                    let cur = due.weekday().to_monday_zero_offset();
                    match sorted.iter().find(|&&d| d > cur) {
                        Some(&d) => monday.checked_add(i64::from(d).days()).ok(),
                        None => monday.checked_add((7 * n + i64::from(*sorted.first()?)).days()).ok(),
                    }
                }
                Freq::Monthly(day) => {
                    let day = if *day == 0 { due.day() } else { *day };
                    let first = due.first_of_month().checked_add(n.months()).ok()?;
                    clamp_day(first.year(), first.month(), day)
                }
                Freq::Yearly => {
                    // 29 февраля — 28-е в невисокосный год; день срока, а не сдвинутый прошлым годом.
                    let y = due.year().checked_add(i16::try_from(n).ok()?)?;
                    clamp_day(y, due.month(), due.day())
                }
            }
        }
    }
}

/// Повторяющаяся задача выполнена в `done_at`: следующая копия (новый UID, срок и начало сдвинуты, подзадачи — нет).
/// У выполненной повтор снимается (иначе выполненная породила бы копию снова). `None` — не повторяется или правило
/// вне подмножества (такую выполняют как обычную).
pub fn next_task(t: &Task, uid: &str, done_at: i64, tz: &TimeZone) -> Option<Task> {
    let rule = rule_of(t)?;
    let done = jiff::Timestamp::from_second(done_at).ok()?.to_zoned(tz.clone()).date();
    let due = t.due.as_ref().and_then(|d| d.date_in(tz)).unwrap_or(done);
    let next = next_date(&rule, due, done)?;
    let shift = due.until(next).ok()?;
    let moved = |m: &Moment| -> Option<Moment> {
        let local: DateTime = m.local.checked_add(shift).ok()?;
        Some(Moment { local, tz: m.tz.clone() })
    };
    let mut n = t.clone();
    n.id = String::new();
    n.uid = uid.to_owned();
    n.due = match &t.due {
        Some(m) => Some(moved(m)?),
        // Без срока: срок — следующая дата, весь день.
        None => {
            n.all_day = true;
            Some(Moment::new(next.at(0, 0, 0, 0), None))
        }
    };
    n.start = t.start.as_ref().and_then(moved);
    n.set_done(false, 0);
    n.block = None;
    n.mail = None;
    n.participants.clear();
    n.sequence = 0;
    Some(n)
}

// ---------------------------------------------------------------------------------------------------------------
// Напоминания.

/// Напоминание к показу.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fire {
    pub uid: String,
    pub title: String,
    pub at: i64,
}

/// Что сделать сейчас: показать сработавшие, следующий раз проверить в `next`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub fire: Vec<Fire>,
    pub next: Option<i64>,
}

/// Когда срабатывают напоминания задачи (Unix-секунды): от срока, от начала или в момент.
pub fn times(t: &Task, tz: &TimeZone) -> Vec<i64> {
    t.alerts
        .iter()
        .filter_map(|a| match a.trigger {
            Trigger::At(ts) => Some(ts),
            Trigger::Offset { secs, from: Anchor::Due } => t.due_at(tz).map(|d| d + secs),
            Trigger::Offset { secs, from: Anchor::Start } => t.start.as_ref().and_then(|s| s.timestamp(tz)).map(|s| s + secs),
        })
        .collect()
}

/// Проверка напоминаний: сработавшие в `(since, now]` у открытых задач и отложенные (`snoozed`: UID и когда), и
/// ближайшее следующее. Часы — параметром (тест подменяет).
pub fn plan(tasks: &[Task], snoozed: &[(String, i64)], since: i64, now: i64, tz: &TimeZone) -> Plan {
    let mut out = Plan::default();
    let consider = |uid: &str, title: &str, at: i64, out: &mut Plan| {
        if at > since && at <= now {
            if !out.fire.iter().any(|f| f.uid == uid) {
                out.fire.push(Fire { uid: uid.to_owned(), title: title.to_owned(), at });
            }
        } else if at > now {
            out.next = Some(out.next.map_or(at, |n| n.min(at)));
        }
    };
    for t in tasks.iter().filter(|t| !t.progress.is_closed()) {
        for at in times(t, tz) {
            consider(&t.uid, &t.title, at, &mut out);
        }
    }
    for (uid, at) in snoozed {
        if let Some(t) = tasks.iter().find(|t| &t.uid == uid && !t.progress.is_closed()) {
            consider(&t.uid, &t.title, *at, &mut out);
        }
    }
    out.fire.sort_by_key(|f| f.at);
    out
}

/// «Отложить»: на 10 минут, на час или на завтра утром (`morning` — начало рабочего дня).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Snooze {
    Minutes10,
    Hour,
    Tomorrow,
}

pub fn snooze_until(s: Snooze, now: i64, morning: jiff::civil::Time, tz: &TimeZone) -> i64 {
    match s {
        Snooze::Minutes10 => now + 600,
        Snooze::Hour => now + 3600,
        Snooze::Tomorrow => jiff::Timestamp::from_second(now)
            .ok()
            .map(|t| t.to_zoned(tz.clone()).date())
            .and_then(|d| d.checked_add(1.days()).ok())
            .and_then(|d| d.to_datetime(morning).to_zoned(tz.clone()).ok())
            .map_or(now + 86_400, |z| z.timestamp().as_second()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;
    use postman_calendar::Alert;

    fn every(freq: Freq, interval: u32) -> Rule {
        Rule::Every { freq, interval }
    }

    /// Таблица повтора: правило, срок, дата выполнения → следующий срок.
    #[test]
    fn next_dates() {
        use Weekday::*;
        let d = date;
        let rows: Vec<(Rule, Date, Date, Option<Date>)> = vec![
            (every(Freq::Daily, 1), d(2026, 10, 21), d(2026, 10, 21), Some(d(2026, 10, 22))),
            (every(Freq::Daily, 3), d(2026, 10, 30), d(2026, 10, 30), Some(d(2026, 11, 2))),
            (every(Freq::Daily, 1), d(2026, 12, 31), d(2026, 12, 31), Some(d(2027, 1, 1))),
            (every(Freq::Weekdays, 1), d(2026, 10, 23), d(2026, 10, 23), Some(d(2026, 10, 26))),
            (every(Freq::Weekdays, 1), d(2026, 10, 21), d(2026, 10, 21), Some(d(2026, 10, 22))),
            (every(Freq::Weekly(vec![]), 1), d(2026, 10, 21), d(2026, 10, 21), Some(d(2026, 10, 28))),
            (every(Freq::Weekly(vec![]), 2), d(2026, 10, 21), d(2026, 10, 21), Some(d(2026, 11, 4))),
            (every(Freq::Weekly(vec![Monday, Thursday]), 1), d(2026, 10, 19), d(2026, 10, 19), Some(d(2026, 10, 22))),
            (every(Freq::Weekly(vec![Monday, Thursday]), 1), d(2026, 10, 22), d(2026, 10, 22), Some(d(2026, 10, 26))),
            (every(Freq::Weekly(vec![Monday, Thursday]), 2), d(2026, 10, 22), d(2026, 10, 22), Some(d(2026, 11, 2))),
            (every(Freq::Monthly(20), 1), d(2026, 10, 20), d(2026, 10, 20), Some(d(2026, 11, 20))),
            (every(Freq::Monthly(0), 1), d(2026, 10, 20), d(2026, 10, 20), Some(d(2026, 11, 20))),
            (every(Freq::Monthly(31), 1), d(2026, 10, 31), d(2026, 10, 31), Some(d(2026, 11, 30))),
            (every(Freq::Monthly(31), 1), d(2026, 11, 30), d(2026, 11, 30), Some(d(2026, 12, 31))),
            (every(Freq::Monthly(31), 1), d(2027, 1, 31), d(2027, 1, 31), Some(d(2027, 2, 28))),
            (every(Freq::Monthly(30), 1), d(2028, 1, 30), d(2028, 1, 30), Some(d(2028, 2, 29))),
            (every(Freq::Monthly(0), 3), d(2026, 11, 15), d(2026, 11, 15), Some(d(2027, 2, 15))),
            (every(Freq::Yearly, 1), d(2028, 2, 29), d(2028, 2, 29), Some(d(2029, 2, 28))),
            (every(Freq::Yearly, 4), d(2028, 2, 29), d(2028, 2, 29), Some(d(2032, 2, 29))),
            (every(Freq::Yearly, 1), d(2026, 10, 21), d(2026, 10, 21), Some(d(2027, 10, 21))),
            // Выполнили позже срока — от срока, не от выполнения.
            (every(Freq::Daily, 1), d(2026, 10, 18), d(2026, 10, 21), Some(d(2026, 10, 19))),
            (Rule::AfterDone(7), d(2026, 10, 18), d(2026, 10, 21), Some(d(2026, 10, 28))),
            (Rule::AfterDone(1), d(2026, 12, 31), d(2026, 12, 31), Some(d(2027, 1, 1))),
            (Rule::Other(json!({ "frequency": "monthly", "byDay": [{ "day": "tu", "nthOfPeriod": 2 }] })), d(2026, 10, 13), d(2026, 10, 13), None),
        ];
        for (rule, due, done, want) in rows {
            assert_eq!(next_date(&rule, due, done), want, "{rule:?} {due}");
        }
    }

    #[test]
    fn rule_round_trip_and_other() {
        use Weekday::*;
        for r in [
            every(Freq::Daily, 1),
            every(Freq::Daily, 2),
            every(Freq::Weekdays, 1),
            every(Freq::Weekly(vec![Monday, Friday]), 1),
            every(Freq::Weekly(vec![]), 2),
            every(Freq::Monthly(20), 1),
            every(Freq::Yearly, 1),
            Rule::AfterDone(3),
        ] {
            let mut t = Task::new("u", "b", "x");
            set_rule(&mut t, Some(r.clone()));
            // Через JSCalendar и обратно — то же правило.
            let back = Task::from_jmap(&t.to_jmap()).unwrap();
            assert_eq!(rule_of(&back), Some(r.clone()), "{r:?}");
        }
        let mut t = Task::new("u", "b", "x");
        t.recurrence = Some(json!({ "frequency": "monthly", "byDay": [{ "day": "tu", "nthOfPeriod": 2 }] }));
        assert!(matches!(rule_of(&t), Some(Rule::Other(_))));
        t.recurrence = Some(json!({ "frequency": "daily", "count": 5 }));
        assert!(matches!(rule_of(&t), Some(Rule::Other(_))));
        set_rule(&mut t, None);
        assert_eq!(rule_of(&t), None);
    }

    /// Следующая копия: новый UID, срок сдвинут на ту же местную дату и время — и через переход на зимнее время, и в
    /// другом поясе: «плавающее» время остаётся 09:00.
    #[test]
    fn next_copy_keeps_wall_clock() {
        let berlin = TimeZone::get("Europe/Berlin").unwrap();
        let mut t = Task::new("u1", "b", "Еженедельный отчёт");
        t.due = Some(Moment::new(date(2026, 10, 23).at(9, 0, 0, 0), None));
        t.notes = "заметка".into();
        t.block = Some(postman_calendar::TimeBlock { event_id: "e".into(), start: 1, end: 2 });
        set_rule(&mut t, Some(every(Freq::Weekly(vec![]), 1)));
        let done = date(2026, 10, 23).at(18, 0, 0, 0).to_zoned(berlin.clone()).unwrap().timestamp().as_second();
        let n = next_task(&t, "u2", done, &berlin).unwrap();
        assert_eq!(n.uid, "u2");
        assert_eq!(n.due.as_ref().unwrap().local, date(2026, 10, 30).at(9, 0, 0, 0), "через переход 25.10 — те же 09:00");
        let at = n.due_at(&berlin).unwrap();
        assert_eq!(jiff::Timestamp::from_second(at).unwrap().to_zoned(berlin.clone()).hour(), 9);
        assert_eq!((n.progress, n.block.clone(), n.notes.as_str()), (postman_calendar::Progress::NeedsAction, None, "заметка"));
        assert!(rule_of(&n).is_some(), "повтор — у копии");
        // Срок с поясом (другой клиент) — тот же пояс, местное время то же.
        t.due = Some(Moment::new(date(2026, 10, 23).at(9, 0, 0, 0), Some("Europe/Moscow")));
        let n = next_task(&t, "u3", done, &berlin).unwrap();
        assert_eq!(n.due.as_ref().unwrap().tz.as_deref(), Some("Europe/Moscow"));
        assert_eq!(n.due.as_ref().unwrap().local.date(), date(2026, 10, 30));
        // Не повторяется — копии нет.
        let plain = Task::new("p", "b", "x");
        assert!(next_task(&plain, "p2", done, &berlin).is_none());
    }

    /// Планировщик: сработавшие с прошлой проверки, следующее — одним таймером; выполненные молчат; отложенное — в
    /// свой срок; повторное срабатывание того же — нет (окно `(since, now]`).
    #[test]
    fn reminder_plan_with_fake_clock() {
        let tz = TimeZone::UTC;
        let at = |h: i8, m: i8| date(2026, 10, 21).at(h, m, 0, 0).to_zoned(tz.clone()).unwrap().timestamp().as_second();
        let mut a = Task::new("a", "b", "Отчёт");
        a.due = Some(Moment::new(date(2026, 10, 21).at(16, 0, 0, 0), None));
        a.alerts = vec![Alert { trigger: Trigger::Offset { secs: -1800, from: Anchor::Due }, email: false }];
        let mut b = Task::new("b", "b", "Созвон");
        b.alerts = vec![Alert { trigger: Trigger::At(at(17, 0)), email: false }];
        let mut done = Task::new("c", "b", "Сделано");
        done.alerts = vec![Alert { trigger: Trigger::At(at(15, 0)), email: false }];
        done.set_done(true, at(14, 0));
        let tasks = vec![a, b, done];
        let p = plan(&tasks, &[], at(14, 0), at(15, 0), &tz);
        assert_eq!(p, Plan { fire: vec![], next: Some(at(15, 30)) });
        let p = plan(&tasks, &[], at(15, 0), at(15, 31), &tz);
        assert_eq!(p.fire, vec![Fire { uid: "a".into(), title: "Отчёт".into(), at: at(15, 30) }]);
        assert_eq!(p.next, Some(at(17, 0)));
        // Та же проверка ещё раз — уже не срабатывает.
        assert!(plan(&tasks, &[], at(15, 31), at(15, 32), &tz).fire.is_empty());
        // Отложили на 10 минут.
        let snoozed = vec![("a".to_string(), at(15, 41))];
        let p = plan(&tasks, &snoozed, at(15, 32), at(15, 35), &tz);
        assert_eq!(p.next, Some(at(15, 41)));
        let p = plan(&tasks, &snoozed, at(15, 35), at(15, 45), &tz);
        assert_eq!(p.fire.len(), 1);
        assert_eq!(
            snooze_until(Snooze::Tomorrow, at(15, 45), jiff::civil::Time::constant(9, 0, 0, 0), &tz),
            date(2026, 10, 22).at(9, 0, 0, 0).to_zoned(tz.clone()).unwrap().timestamp().as_second()
        );
        assert_eq!(snooze_until(Snooze::Hour, 100, jiff::civil::Time::midnight(), &tz), 3700);
    }
}
