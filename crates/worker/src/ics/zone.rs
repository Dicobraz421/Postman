//! Пояс по правилам самого файла (RFC 5545 §3.6.5): `TZID`, которого нет ни в базе поясов, ни в `X-LIC-LOCATION`, — например
//! `Customized Time Zone` у Outlook. Каждое правило (`STANDARD`, `DAYLIGHT`) даёт переводы часов: `DTSTART`, `RRULE`, `RDATE`
//! в местном времени по `TZOFFSETFROM`, после перевода действует `TZOFFSETTO`.
//!
//! Нужны переводы вокруг запрошенного периода, а правила Outlook начинаются в 1601 году: начало годового правила подвигается
//! к периоду целыми `INTERVAL`, цена не зависит от года в файле. Вне периода смещение может быть неточным: там из него
//! считаются только границы, которые показанное не меняют.

use calcard::common::PartialDateTime;
use calcard::common::timezone::Tz;
use calcard::icalendar::{
    ICalendar, ICalendarComponent, ICalendarComponentType as Kind, ICalendarFrequency as Freq, ICalendarProperty as Prop, ICalendarValue as Value,
};

use super::{dates, rule_of, stamp, steps_of, value};

/// Работы на все пояса файла — не больше: отдельно от работы серий, тысячи правил не отнимают время у событий.
pub(super) const MAX_ZONE_WORK: i64 = 50_000;
/// Переводов одного правила — не больше.
const MAX_ONSETS: usize = 4096;
/// Смещение пояса меньше суток (RFC 5545 §3.3.14).
const MAX_OFFSET: i64 = 86_400;

/// Перевод часов: момент UTC и смещения до и после него.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Onset {
    utc: i64,
    before: i64,
    after: i64,
}

/// Переводы часов пояса по времени.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Rules {
    onsets: Vec<Onset>,
}

impl Rules {
    /// Смещение в момент `utc`: действует последний перевод, до первого — смещение перед ним.
    pub(super) fn offset_at(&self, utc: i64) -> i64 {
        let passed = self.onsets.partition_point(|o| o.utc <= utc);
        match passed.checked_sub(1).and_then(|i| self.onsets.get(i)) {
            Some(onset) => onset.after,
            None => self.onsets.first().map_or(0, |o| o.before),
        }
    }

    /// Местное время → UTC. Часов нет («дыра») — смещение до перевода; два раза («складка») — первый: тоже смещение до
    /// перевода (RFC 5545 §3.3.5), как у поясов из базы.
    pub(super) fn instant(&self, local: i64) -> i64 {
        let passed = self.onsets.partition_point(|o| o.utc + o.before.max(o.after) <= local);
        let next = self.onsets.get(passed).filter(|o| o.utc + o.before.min(o.after) <= local);
        let offset = match (next, passed.checked_sub(1).and_then(|i| self.onsets.get(i))) {
            (Some(onset), _) => onset.before,
            (None, Some(onset)) => onset.after,
            (None, None) => self.onsets.first().map_or(0, |o| o.before),
        };
        local - offset
    }
}

/// Почему правил пояса нет.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Refused {
    /// В `VTIMEZONE` нет ни одного разборчивого правила.
    Broken,
    /// Работа на пояса кончилась.
    Work,
}

fn offset_of(comp: &ICalendarComponent, name: &Prop) -> Option<i64> {
    let (_, offset) = stamp(comp, name)?;
    let seconds = i64::from(offset.tz_hour?) * 3600 + i64::from(offset.tz_minute.unwrap_or(0)) * 60;
    (seconds < MAX_OFFSET).then_some(if offset.tz_minus { -seconds } else { seconds })
}

fn seconds_of(date: &PartialDateTime) -> Option<i64> {
    Some(date.to_date_time()?.date_time.and_utc().timestamp())
}

/// Начало годового правила без `COUNT`, передвинутое к году `target`: те же месяц и день, шаг кратен `INTERVAL`. Год с другой
/// длиной февраля такой даты может не иметь: тогда начало прежнее, это только дороже.
fn near(date: &PartialDateTime, interval: i32, target: i32) -> Option<i64> {
    let year = i32::from(date.year?);
    let moved = year + (target - year).div_euclid(interval).max(0) * interval;
    let mut near = date.clone();
    near.year = Some(u16::try_from(moved).ok()?);
    seconds_of(&near)
}

/// Переводы одного правила. Работа списывается до развёртки; не хватило — `Err`.
fn onsets_of(comp: &ICalendarComponent, window: (i64, i64), work: &mut i64) -> Result<Option<Vec<Onset>>, Refused> {
    let (Some(before), Some(after), Some((_, date))) =
        (offset_of(comp, &Prop::Tzoffsetfrom), offset_of(comp, &Prop::Tzoffsetto), stamp(comp, &Prop::Dtstart))
    else {
        return Ok(None);
    };
    let Some(start) = seconds_of(date) else { return Ok(None) };
    let mut locals = vec![start];
    for entry in comp.entries.iter().filter(|e| e.name == Prop::Rdate) {
        locals.extend(entry.values.iter().filter_map(|v| if let Value::PartialDateTime(d) = v { seconds_of(d) } else { None }));
    }
    let rdates = i64::try_from(locals.len()).unwrap_or(i64::MAX);
    if rdates > *work {
        return Err(Refused::Work);
    }
    *work -= rdates;
    if let Some(rule) = rule_of(comp) {
        // `UNTIL` в UTC: местное время этого правила — по `TZOFFSETFROM`.
        let until = rule.until.as_ref().and_then(|until| {
            let at = seconds_of(until)?;
            Some(if until.has_time() { at + if until.has_zone() { before } else { 0 } } else { at + super::DAY - 1 })
        });
        let last = until.map_or(window.1, |until| until.min(window.1));
        let first = if rule.freq == Freq::Yearly && rule.count.is_none() {
            // Год до окна: нужен и последний перевод перед ним, он задаёт смещение в начале окна.
            let target = PartialDateTime::from_naive_timestamp(window.0.min(last)).year.map_or(0, i32::from) - 1;
            near(date, i32::from(rule.interval.unwrap_or(1).max(1)), target).unwrap_or(start)
        } else {
            start
        };
        let steps = steps_of(rule, last - first);
        if steps > *work {
            return Err(Refused::Work);
        }
        *work -= steps;
        let mut trimmed = rule.clone();
        trimmed.until = Some(PartialDateTime::from_naive_timestamp(last));
        let entries = vec![
            dates(Prop::Dtstart, vec![value(first, false)]),
            dates(Prop::Dtend, vec![value(first, false)]),
            dates(Prop::Rrule, vec![Value::RecurrenceRule(Box::new(trimmed))]),
        ];
        let lean = ICalendar { components: vec![ICalendarComponent { component_type: Kind::VEvent, entries, component_ids: Vec::new() }] };
        locals.extend(lean.expand_dates(Tz::Floating, MAX_ONSETS).events.iter().map(|e| e.start.timestamp()));
    }
    Ok(Some(locals.into_iter().map(|local| Onset { utc: local - before, before, after }).collect()))
}

/// Правила пояса `zone` для времени внутри `window` (местные секунды). Разборчивое правило работает и без соседа: половина
/// года со своим смещением лучше пояса показа круглый год.
pub(super) fn rules(ical: &ICalendar, zone: &ICalendarComponent, window: (i64, i64), work: &mut i64) -> Result<Rules, Refused> {
    let mut onsets = Vec::new();
    let observances = zone.component_ids.iter().filter_map(|&id| ical.components.get(id as usize));
    for comp in observances.filter(|c| matches!(c.component_type, Kind::Standard | Kind::Daylight)) {
        if let Some(found) = onsets_of(comp, window, work)? {
            onsets.extend(found);
        }
    }
    if onsets.is_empty() {
        return Err(Refused::Broken);
    }
    onsets.sort_by_key(|o| (o.utc, o.before, o.after));
    onsets.dedup_by_key(|o| o.utc);
    Ok(Rules { onsets })
}
