//! Экземпляры событий `.ics` за период: занятость по ссылке (S8-42) и подписка (S8-44).
//!
//! Правила повторов считает `calcard::expand_dates`, своего итератора у него наружу нет. Но на настоящих файлах Outlook и
//! Google его развёртка ошибается, поэтому календарь ему подаётся подготовленным, по серии за раз:
//! - предел развёртки у него общий на файл, первая бесконечная серия съедает всё: здесь у каждой серии свой вызов;
//! - серия с `DTSTART:…Z` идёт в поясе показа, `UNTIL=…Z` сравнивается с местным временем как с UTC, в «дыре» и «складке»
//!   перевода часов дата остаётся без пояса, `DTSTART` в них роняет всю серию: здесь все даты приведены к местному времени
//!   серии без пояса, а в UTC их переводит [`instant`] по RFC 5545 §3.3.5;
//! - переопределение ищется по `SEQUENCE` без `UID`: здесь серия — компоненты одного `UID`, `SEQUENCE` убран;
//! - событие без `DTEND` тянется до конца суток; RFC 5545 §3.6.1 — нулевой длины;
//! - `TZID` не из базы, без `X-LIC-LOCATION`, для него — пояс показа: здесь время считается по правилам файла ([`zone`]).
//!
//! Серии обрезаны концом периода, старые — передвинуты к его началу ([`plan`]): цена не зависит от истории файла. Всё — в
//! песочнице воркера; `chrono` в зависимостях воркера нет, поэтому время здесь — секунды, местное или UTC.
//!
//! Что не вошло из-за пределов, считает [`Cut`]: только числа, журнала в песочнице нет, запись делает приложение.

mod zone;

use std::collections::{HashMap, HashSet};
use std::fmt;

use calcard::common::PartialDateTime;
use calcard::common::timezone::Tz;
use calcard::icalendar::dates::TimeOrDelta;
use calcard::icalendar::timezone::TzResolver;
use serde::{Deserialize, Serialize};
use zone::{MAX_ZONE_WORK, Refused, Rules};

use calcard::icalendar::{
    ICalendar, ICalendarComponent, ICalendarComponentType as Kind, ICalendarEntry, ICalendarFrequency as Freq, ICalendarParameterName as Param,
    ICalendarPeriod as Period, ICalendarProperty as Prop, ICalendarRecurrenceRule as Rule, ICalendarValue as Value,
};

const DAY: i64 = 86_400;
/// Запас у границ периода в местном времени: смещение пояса меньше суток, плюс сутки на всякий случай.
const MARGIN: i64 = 2 * DAY;
/// Дат одной серии — не больше.
const MAX_DATES: usize = 200_000;
/// Работы на файл (шаги правил и выданные даты) — не больше: дальше серии не разворачиваются. Даёт доли секунды в выпуске и
/// меньше сторожа воркера в отладочной сборке; файл из тысяч обычных серий укладывается с запасом.
const MAX_WORK: i64 = 2_000_000;
/// Экземпляров периода — не больше: показать столько всё равно нельзя.
const MAX_INSTANCES: usize = 20_000;

/// Экземпляр события: компонент файла и время UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Instance {
    pub comp: usize,
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
}

/// Что отсекли пределы: числа для журнала приложения. Названий, адресов и другого содержимого здесь нет.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cut {
    /// Серий с правилом в файле и сколько из них не развёрнуто: кончилась работа [`MAX_WORK`] или место [`MAX_INSTANCES`].
    pub series: u32,
    pub series_skipped: u32,
    /// Серий, развёрнутых не до конца: предел дат [`MAX_DATES`].
    pub series_truncated: u32,
    /// Экземпляров за пределом [`MAX_INSTANCES`].
    pub instances_dropped: u32,
    /// Событий за пределом числа событий подписки.
    pub events_dropped: u32,
    /// Интервалов за пределом занятости.
    pub busy_dropped: u32,
    /// Поясов файла и сколько из них не построено: кончилась работа [`MAX_ZONE_WORK`]; такие идут в поясе показа.
    pub zones: u32,
    pub zones_skipped: u32,
}

impl Cut {
    pub fn is_empty(&self) -> bool {
        let Cut { series: _, zones: _, series_skipped, series_truncated, instances_dropped, events_dropped, busy_dropped, zones_skipped } = *self;
        series_skipped == 0 && series_truncated == 0 && instances_dropped == 0 && events_dropped == 0 && busy_dropped == 0 && zones_skipped == 0
    }
}

impl fmt::Display for Cut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts = [
            (self.series_skipped > 0).then(|| format!("{} of {} series not expanded (work or instance limit)", self.series_skipped, self.series)),
            (self.series_truncated > 0).then(|| format!("{} series truncated (date limit)", self.series_truncated)),
            (self.instances_dropped > 0).then(|| format!("{} instances dropped (instance limit)", self.instances_dropped)),
            (self.events_dropped > 0).then(|| format!("{} events dropped (event limit)", self.events_dropped)),
            (self.busy_dropped > 0).then(|| format!("{} busy spans dropped (busy limit)", self.busy_dropped)),
            (self.zones_skipped > 0).then(|| format!("{} of {} file time zones not built (zone work limit)", self.zones_skipped, self.zones)),
        ];
        f.write_str(&parts.into_iter().flatten().collect::<Vec<_>>().join("; "))
    }
}

/// Местное время `local` пояса `zone` → UTC. Часов нет («дыра») — смещение до перевода; два раза («складка») — первый:
/// смещение до перевода в обоих случаях (RFC 5545 §3.3.5). Двух переводов за двое суток не бывает.
fn instant(local: i64, zone: Tz) -> i64 {
    let at = |seconds: i64| PartialDateTime::from_naive_timestamp(seconds).to_date_time_with_tz(zone).map(|t| t.timestamp());
    at(local).or_else(|| at(local - MARGIN).map(|t| t + MARGIN)).unwrap_or(local)
}

/// Пояс времени: из базы или по правилам файла (номер в [`Zones::files`]).
#[derive(Clone, Copy, PartialEq)]
enum Zone {
    Named(Tz),
    File(usize),
}

struct Zones<'a> {
    resolver: TzResolver<&'a str>,
    /// Пояс показа: события на весь день и без пояса.
    shown: Tz,
    /// `TZID` с правилами файла.
    by_name: HashMap<&'a str, usize>,
    files: Vec<Rules>,
}

impl<'a> Zones<'a> {
    /// Пояса, которых нет в базе: только те, на что ссылаются события. `VTIMEZONE` без дела работы не стоит.
    fn new(ical: &'a ICalendar, shown: Tz, from: i64, to: i64, cut: &mut Cut) -> Self {
        let mut zones = Zones { resolver: ical.build_tz_resolver().with_default(shown), shown, by_name: HashMap::new(), files: Vec::new() };
        let mut defined: HashMap<&str, &ICalendarComponent> = HashMap::new();
        for comp in ical.timezones() {
            if let Some(Value::Text(name)) = comp.entries.iter().find(|e| e.name == Prop::Tzid).and_then(|e| e.values.first()) {
                defined.entry(name.as_str()).or_insert(comp);
            }
        }
        if defined.is_empty() {
            return zones;
        }
        // Год вокруг периода: перевод в начале и конце окна нужен целиком, а в период попадают и соседние сутки.
        let window = (from - 400 * DAY, to + 400 * DAY);
        let mut work = MAX_ZONE_WORK;
        let mut seen = HashSet::new();
        let events = ical.components.iter().filter(|c| c.component_type == Kind::VEvent);
        let used = events
            .flat_map(|c| &c.entries)
            .filter(|e| matches!(e.name, Prop::Dtstart | Prop::Dtend | Prop::Exdate | Prop::Rdate | Prop::RecurrenceId));
        for name in used.filter_map(ICalendarEntry::tz_id) {
            if !seen.insert(name) || zones.resolver.resolve(name).is_some() {
                continue;
            }
            let Some(definition) = defined.get(name) else { continue };
            cut.zones += 1;
            match zone::rules(ical, definition, window, &mut work) {
                Ok(rules) => {
                    zones.by_name.insert(name, zones.files.len());
                    zones.files.push(rules);
                }
                Err(Refused::Work) => cut.zones_skipped += 1,
                Err(Refused::Broken) => {}
            }
        }
        zones
    }

    /// Пояс по имени: непонятный `TZID` — пояс показа.
    fn named(&self, tzid: Option<&str>) -> Zone {
        match tzid.and_then(|name| self.by_name.get(name)) {
            Some(&index) => Zone::File(index),
            None => Zone::Named(self.resolver.resolve_or_default(tzid)),
        }
    }

    /// Пояс даты: `Z` важнее `TZID`.
    fn own(&self, entry: &ICalendarEntry, date: &PartialDateTime) -> Zone {
        if !date.has_time() {
            return Zone::Named(self.shown);
        }
        date.to_date_time().and_then(|d| d.tz()).map_or_else(|| self.named(entry.tz_id()), Zone::Named)
    }

    /// Местное время пояса → UTC.
    fn instant(&self, local: i64, zone: Zone) -> i64 {
        match zone {
            Zone::Named(tz) => instant(local, tz),
            Zone::File(index) => self.files.get(index).map_or(local, |rules| rules.instant(local)),
        }
    }

    /// UTC → местное время пояса.
    fn at(&self, utc: i64, zone: Zone) -> i64 {
        match zone {
            Zone::Named(tz) => PartialDateTime::from_utc_timestamp(utc)
                .to_date_time_with_tz(tz)
                .map_or(utc, |t| t.with_timezone(&tz).naive_local().and_utc().timestamp()),
            Zone::File(index) => self.files.get(index).map_or(utc, |rules| utc + rules.offset_at(utc)),
        }
    }

    /// Дата → местное время пояса `to`.
    fn local(&self, date: &PartialDateTime, tzid: Option<&str>, to: Zone) -> Option<i64> {
        let dt = date.to_date_time()?;
        let naive = dt.date_time.and_utc().timestamp();
        let own = dt.tz().map(Zone::Named).or_else(|| tzid.map(|_| self.named(tzid)));
        Some(match own {
            Some(own) if own != to => self.at(self.instant(naive, own), to),
            _ => naive,
        })
    }
}

fn stamp<'a>(comp: &'a ICalendarComponent, name: &Prop) -> Option<(&'a ICalendarEntry, &'a PartialDateTime)> {
    comp.entries.iter().filter(|e| e.name == *name).find_map(|e| match e.values.first() {
        Some(Value::PartialDateTime(date)) => Some((e, date.as_ref())),
        _ => None,
    })
}

fn start_of(comp: &ICalendarComponent) -> Option<(&ICalendarEntry, &PartialDateTime)> {
    stamp(comp, &Prop::Dtstart).or_else(|| stamp(comp, &Prop::RecurrenceId))
}

fn rule_of(comp: &ICalendarComponent) -> Option<&Rule> {
    comp.entries.iter().filter(|e| e.name == Prop::Rrule).find_map(|e| match e.values.first() {
        Some(Value::RecurrenceRule(rule)) => Some(rule.as_ref()),
        _ => None,
    })
}

fn uid_of(comp: &ICalendarComponent) -> Option<&str> {
    comp.entries.iter().filter(|e| e.name == Prop::Uid).find_map(|e| match e.values.first() {
        Some(Value::Text(uid)) => Some(uid.as_str()),
        _ => None,
    })
}

/// Длительность события по его свойствам, секунды; нет — ноль.
fn length_of(comp: &ICalendarComponent, zones: &Zones, zone: Zone, start: i64) -> i64 {
    let to_end = stamp(comp, &Prop::Dtend).and_then(|(e, date)| zones.local(date, e.tz_id(), zone)).map(|end| end - start);
    let duration = comp.entries.iter().find(|e| e.name == Prop::Duration).and_then(|e| match e.values.first() {
        Some(Value::Duration(d)) => Some(d.as_seconds()),
        _ => None,
    });
    to_end.or(duration).unwrap_or(0).max(0)
}

/// Серия: компоненты одного `UID`, среди них правило.
struct Series {
    /// Пояс начала серии.
    zone: Zone,
    /// Начало в местном времени.
    start: i64,
    all_day: bool,
    /// Сдвиг начала к периоду, секунды: кратен шагу правила.
    moved: i64,
    /// Последняя дата правила в местном времени: `UNTIL`, не дальше конца периода.
    last: i64,
    /// Оценка работы: шагов правила до конца периода.
    work: i64,
}

impl Series {
    /// Дата исключения или переопределения → местное время серии. Google и Outlook пишут её по-разному: `Z`, `TZID` или
    /// только дату, поэтому сравнивается приведённая к виду экземпляров.
    fn fit(&self, date: &PartialDateTime, tzid: Option<&str>, zones: &Zones) -> Option<i64> {
        let at = zones.local(date, tzid, self.zone)?;
        Some(if self.all_day {
            at.div_euclid(DAY) * DAY
        } else if date.has_time() {
            at
        } else {
            at + self.start.rem_euclid(DAY)
        })
    }
}

/// Шаг правила в секундах: для месяца и года — приблизительно. Сколько дат даёт один шаг: только если времён суток несколько.
fn step_of(rule: &Rule) -> (i64, i64, i64) {
    let (unit, days) = match rule.freq {
        Freq::Secondly => (1, 1),
        Freq::Minutely => (60, 1),
        Freq::Hourly => (3600, 1),
        Freq::Daily => (DAY, 1),
        Freq::Weekly => (7 * DAY, 7),
        Freq::Monthly => (30 * DAY, 31),
        Freq::Yearly => (365 * DAY, 366),
    };
    // Все времена суток правила: одно обычное, а произведение `BYHOUR`, `BYMINUTE`, `BYSECOND` раздувает каждый шаг.
    let times = [&rule.byhour, &rule.byminute, &rule.bysecond].iter().map(|by| by.len().max(1) as i64).product::<i64>();
    (unit, unit * i64::from(rule.interval.unwrap_or(1).max(1)), if times > 1 { times.saturating_mul(days) } else { 1 })
}

/// Оценка работы правила на `span` секунд.
fn steps_of(rule: &Rule, span: i64) -> i64 {
    let (_, step, width) = step_of(rule);
    (span.max(0) / step + 1).saturating_mul(width)
}

/// План развёртки компонентов одного `UID`, если среди них есть правило. Старое начало правила без `COUNT` с шагом постоянной
/// длины передвигается к периоду: фаза `INTERVAL` и день недели те же, экземпляры до периода никому не нужны, а бесконечная
/// ежедневная серия с 2010 года стоит как новая. `COUNT` считается от начала, `RANGE=THISANDFUTURE` — от своего экземпляра:
/// их начало не трогаем. Шаг месяца и года непостоянен, да и шагов там единицы в год.
fn plan(comps: &[usize], ical: &ICalendar, zones: &Zones, from: i64, to: i64) -> Option<Series> {
    let master = comps.iter().filter_map(|&i| ical.components.get(i)).find(|c| rule_of(c).is_some())?;
    let rule = rule_of(master)?;
    let (entry, date) = start_of(master)?;
    let zone = zones.own(entry, date);
    let start = zones.local(date, entry.tz_id(), zone)?;
    let (unit, step, _) = step_of(rule);
    let future_ranges = comps
        .iter()
        .filter_map(|&i| ical.components.get(i))
        .any(|c| c.entries.iter().any(|e| e.name == Prop::RecurrenceId && e.params.iter().any(|p| p.name == Param::Range)));
    let moved = if unit <= 7 * DAY && rule.count.is_none() && !future_ranges {
        let length = length_of(master, zones, zone, start);
        (from - MARGIN - length - start).div_euclid(step).saturating_mul(step).max(0)
    } else {
        0
    };
    let until = rule.until.as_ref().and_then(|until| {
        let at = zones.local(until, None, zone)?;
        Some(if until.has_time() { at } else { at + DAY - 1 })
    });
    let last = until.map_or(to + MARGIN, |until| until.min(to + MARGIN));
    Some(Series { zone, start, all_day: !date.has_time(), moved, last, work: steps_of(rule, last - start - moved) })
}

fn date_value(at: i64, all_day: bool) -> PartialDateTime {
    if all_day { PartialDateTime::from_date_timestamp(at) } else { PartialDateTime::from_naive_timestamp(at) }
}

fn dates(name: Prop, values: Vec<Value>) -> ICalendarEntry {
    ICalendarEntry { name, params: Vec::new(), values }
}

fn value(at: i64, all_day: bool) -> Value {
    Value::PartialDateTime(Box::new(date_value(at, all_day)))
}

/// Откуда экземпляр подготовленного календаря.
struct Source {
    comp: usize,
    zone: Zone,
    all_day: bool,
}

/// Подготовленный календарь: только то, что нужно развёртке, все даты — местное время без пояса.
#[derive(Default)]
struct Lean {
    calendar: ICalendar,
    sources: Vec<Source>,
}

impl Lean {
    /// `series` — компонент серии; без неё `RECURRENCE-ID` не нужен: у таких переопределений нет своей серии в файле.
    fn add(&mut self, ical: &ICalendar, index: usize, zones: &Zones, series: Option<&Series>) -> Option<()> {
        let comp = ical.components.get(index)?;
        let (entry, date) = if series.is_some() { start_of(comp)? } else { stamp(comp, &Prop::Dtstart)? };
        let zone = zones.own(entry, date);
        let all_day = !date.has_time();
        let start = zones.local(date, entry.tz_id(), zone)?;
        let rule = rule_of(comp);
        let moved = series.filter(|_| rule.is_some()).map_or(0, |s| s.moved);
        let mut entries = vec![dates(Prop::Dtstart, vec![value(start + moved, all_day)])];
        let local = |date: &PartialDateTime, e: &ICalendarEntry| zones.local(date, e.tz_id(), zone);
        for e in &comp.entries {
            match &e.name {
                Prop::Dtend => {
                    if let Some(end) = e.values.first().and_then(|end| if let Value::PartialDateTime(end) = end { local(end, e) } else { None }) {
                        entries.push(dates(Prop::Dtend, vec![value(end + moved, all_day)]));
                    }
                }
                Prop::Duration => entries.push(e.clone()),
                Prop::Rdate => {
                    let values = e.values.iter().filter_map(|item| match item {
                        Value::PartialDateTime(date) => Some(value(local(date, e)?, all_day)),
                        Value::Period(Period::Range { start, end }) => Some(Value::Period(Period::Range {
                            start: date_value(local(start, e)?, all_day),
                            end: date_value(local(end, e)?, all_day),
                        })),
                        Value::Period(Period::Duration { start, duration }) => {
                            Some(Value::Period(Period::Duration { start: date_value(local(start, e)?, all_day), duration: duration.clone() }))
                        }
                        _ => None,
                    });
                    entries.push(dates(Prop::Rdate, values.collect()));
                }
                Prop::Rrule => {
                    if let (Some(rule), Some(series)) = (rule, series) {
                        let mut rule = rule.clone();
                        rule.until = Some(PartialDateTime::from_naive_timestamp(series.last));
                        entries.push(dates(Prop::Rrule, vec![Value::RecurrenceRule(Box::new(rule))]));
                    }
                }
                Prop::Exdate => {
                    if let (Some(_), Some(series)) = (rule, series) {
                        let values = e.values.iter().filter_map(|item| match item {
                            Value::PartialDateTime(date) => Some(value(series.fit(date, e.tz_id(), zones)?, series.all_day)),
                            _ => None,
                        });
                        entries.push(dates(Prop::Exdate, values.collect()));
                    }
                }
                Prop::RecurrenceId => {
                    if let (Some(series), Some(Value::PartialDateTime(date))) = (series, e.values.first())
                        && let Some(at) = series.fit(date, e.tz_id(), zones)
                    {
                        let params = e.params.iter().filter(|p| p.name == Param::Range).cloned().collect();
                        entries.push(ICalendarEntry { name: Prop::RecurrenceId, params, values: vec![value(at, series.all_day)] });
                    }
                }
                _ => {}
            }
        }
        if !all_day && !entries.iter().any(|e| matches!(e.name, Prop::Dtend | Prop::Duration)) {
            entries.push(dates(Prop::Dtend, vec![value(start + moved, all_day)]));
        }
        self.calendar.components.push(ICalendarComponent { component_type: Kind::VEvent, entries, component_ids: Vec::new() });
        self.sources.push(Source { comp: index, zone, all_day });
        Some(())
    }

    /// Развернуть, экземпляры `[from, to)` — в `out`. Ответ — сколько дат вышло.
    fn expand(self, limit: usize, zones: &Zones, (from, to): (i64, i64), out: &mut Vec<Instance>, cut: &mut Cut) -> usize {
        let events = self.calendar.expand_dates(Tz::Floating, limit).events;
        let count = events.len();
        for event in events {
            let Some(source) = self.sources.get(event.comp_id as usize) else { continue };
            let (zone, all_day) = (source.zone, source.all_day);
            // Пояса нет, поэтому секунды — местное время.
            let local = event.start.timestamp();
            let start = zones.instant(local, zone);
            let end = match event.end {
                TimeOrDelta::Time(end) => zones.instant(end.timestamp(), zone),
                TimeOrDelta::Delta(length) if all_day => zones.instant(local.saturating_add(length.num_seconds()), zone),
                TimeOrDelta::Delta(length) => start.saturating_add(length.num_seconds()),
            };
            let end = if all_day && end <= start { zones.instant(local + DAY, zone) } else { end.max(start) };
            if start < to && end >= from && !(end == from && end > start) {
                if out.len() < MAX_INSTANCES {
                    out.push(Instance { comp: source.comp, start, end, all_day });
                } else {
                    cut.instances_dropped = cut.instances_dropped.saturating_add(1);
                }
            }
        }
        count
    }
}

/// Экземпляры событий в `[from, to)` по времени начала: отменённые здесь ещё есть, переопределение заменяет свой экземпляр.
/// `shown` — пояс показа: в нём события на весь день и без пояса. Второе значение — что отсекли пределы.
pub(crate) fn instances(ical: &ICalendar, shown: Tz, from: i64, to: i64) -> (Vec<Instance>, Cut) {
    let mut cut = Cut::default();
    let zones = Zones::new(ical, shown, from, to, &mut cut);
    // Группы по `UID`. Второе правило с тем же `UID` файл писать не должен: это своя серия, переопределения — первой.
    let mut groups: Vec<(Vec<usize>, bool)> = Vec::new();
    let mut by_uid: HashMap<&str, usize> = HashMap::new();
    for (index, comp) in ical.components.iter().enumerate().filter(|(_, c)| c.component_type == Kind::VEvent) {
        let rule = rule_of(comp).is_some();
        let uid = uid_of(comp);
        let known = uid.and_then(|uid| by_uid.get(uid)).copied().filter(|&place| !rule || groups.get(place).is_some_and(|g| !g.1));
        let place = known.unwrap_or_else(|| {
            groups.push((Vec::new(), false));
            groups.len() - 1
        });
        if let Some(uid) = uid {
            by_uid.entry(uid).or_insert(place);
        }
        if let Some(group) = groups.get_mut(place) {
            group.0.push(index);
            group.1 |= rule;
        }
    }
    let mut out = Vec::new();
    let mut single = Lean::default();
    let mut series = Vec::new();
    for (group, _) in &groups {
        match plan(group, ical, &zones, from, to) {
            Some(plan) => series.push((plan, group)),
            None => {
                for &index in group {
                    single.add(ical, index, &zones, None);
                }
            }
        }
    }
    single.expand(0, &zones, (from, to), &mut out, &mut cut);
    // Первым — дешёвое: дорогая серия чужого файла не вытесняет обычные. Работа не зависит от порядка в файле.
    series.sort_by_key(|(plan, _)| plan.work);
    let mut budget = MAX_WORK;
    cut.series = u32::try_from(series.len()).unwrap_or(u32::MAX);
    for (done, (plan, group)) in series.iter().enumerate() {
        if plan.work > budget || out.len() >= MAX_INSTANCES {
            cut.series_skipped = u32::try_from(series.len() - done).unwrap_or(u32::MAX);
            break;
        }
        let mut lean = Lean::default();
        for &index in group.iter() {
            lean.add(ical, index, &zones, Some(plan));
        }
        let limit = usize::try_from(budget).unwrap_or(MAX_DATES).min(MAX_DATES);
        let count = lean.expand(limit, &zones, (from, to), &mut out, &mut cut);
        if count >= limit {
            cut.series_truncated = cut.series_truncated.saturating_add(1);
        }
        budget -= plan.work.max(i64::try_from(count).unwrap_or(i64::MAX)).max(1);
    }
    out.sort_by_key(|i| (i.start, i.comp));
    (out, cut)
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use calcard::common::PartialDateTime;

    use crate::{BusySpan, BusyType, IcsEvent, TIME_LIMIT, parse_free_busy, parse_ics};

    const HOUR: i64 = 3600;
    const DAY: i64 = 24 * HOUR;

    fn date(year: u16, month: u8, day: u8, hour: u8, minute: u8) -> i64 {
        let value = PartialDateTime {
            year: Some(year),
            month: Some(month),
            day: Some(day),
            hour: Some(hour),
            minute: Some(minute),
            second: Some(0),
            ..Default::default()
        };
        value.to_date_time().unwrap().date_time.and_utc().timestamp()
    }

    /// Неделя с понедельника 2026-11-02 00:00 UTC.
    fn week() -> (i64, i64) {
        let monday = date(2026, 11, 2, 0, 0);
        (monday, monday + 7 * DAY)
    }

    fn file(body: &str) -> Vec<u8> {
        format!("BEGIN:VCALENDAR\nVERSION:2.0\nPRODID:-//example.test//RU\n{body}END:VCALENDAR\n").replace('\n', "\r\n").into_bytes()
    }

    fn event(uid: &str, lines: &str) -> String {
        format!("BEGIN:VEVENT\nUID:{uid}\nDTSTAMP:20260101T000000Z\n{lines}\nEND:VEVENT\n")
    }

    fn events_in(body: &str, from: i64, to: i64, tz: &str) -> Vec<IcsEvent> {
        parse_ics(&file(body), from, to, tz).unwrap().events
    }

    /// `UID` и начало экземпляров по времени.
    fn starts(body: &str, from: i64, to: i64, tz: &str) -> Vec<(String, i64)> {
        events_in(body, from, to, tz).into_iter().map(|e| (e.uid, e.start)).collect()
    }

    fn pairs(list: &[(&str, i64)]) -> Vec<(String, i64)> {
        list.iter().map(|(uid, at)| ((*uid).to_owned(), *at)).collect()
    }

    /// Бесконечные серии: у каждой свой предел, вторая не теряется. Так публикуют Outlook и Google.
    #[test]
    fn endless_series_do_not_eat_each_other() {
        let body = [
            event("a", "DTSTART;TZID=Europe/Moscow:20260105T120000\nDTEND;TZID=Europe/Moscow:20260105T130000\nRRULE:FREQ=WEEKLY;BYDAY=MO"),
            event("b", "DTSTART;TZID=Asia/Tokyo:20260106T100000\nDTEND;TZID=Asia/Tokyo:20260106T110000\nRRULE:FREQ=WEEKLY;BYDAY=TU"),
            event("c", "DTSTART:20260107T150000Z\nDTEND:20260107T160000Z\nRRULE:FREQ=DAILY;INTERVAL=7"),
            event("d", "DTSTART;TZID=America/New_York:20260109T090000\nDURATION:PT1H\nRRULE:FREQ=WEEKLY"),
        ]
        .concat();
        let (from, to) = week();
        let want = pairs(&[
            ("a", date(2026, 11, 2, 9, 0)),
            ("b", date(2026, 11, 3, 1, 0)),
            ("c", date(2026, 11, 4, 15, 0)),
            ("d", date(2026, 11, 6, 14, 0)),
        ]);
        assert_eq!(starts(&body, from, to, "Europe/Moscow"), want);
    }

    /// `DTSTART:…Z` с повтором — в свой час при любом поясе показа: исключения и переопределения в `Z` находят свой экземпляр.
    #[test]
    fn utc_series_keeps_its_hour() {
        let body = event("a", "DTSTART:20260108T090000Z\nDURATION:PT1H30M\nRRULE:FREQ=WEEKLY;BYDAY=TH\nEXDATE:20261112T090000Z");
        let (from, to) = week();
        for tz in ["Europe/Moscow", "America/New_York", "UTC"] {
            let got = events_in(&body, from, to, tz);
            assert_eq!(got.len(), 1, "{tz}");
            assert_eq!((got[0].start, got[0].end), (date(2026, 11, 5, 9, 0), date(2026, 11, 5, 10, 30)), "{tz}");
            assert!(starts(&body, from + 7 * DAY, to + 7 * DAY, tz).is_empty(), "{tz}: EXDATE");
        }
    }

    /// `UNTIL` в UTC у серии с поясом (так пишут Outlook и Google): последний экземпляр на месте, следующего нет.
    #[test]
    fn until_in_utc_keeps_the_last_instance() {
        // 12:00 МСК — 09:00 UTC; наивное сравнение 12:00 с 09:00 отбросило бы последний экземпляр.
        let line = "DTSTART;TZID=Europe/Moscow:20261012T120000\nDTEND;TZID=Europe/Moscow:20261012T130000\nRRULE:FREQ=WEEKLY;UNTIL=20261102T090000Z";
        let body = event("a", line);
        let (from, to) = week();
        assert_eq!(starts(&body, from, to, "UTC"), pairs(&[("a", date(2026, 11, 2, 9, 0))]));
        assert!(starts(&body, from + 7 * DAY, to + 7 * DAY, "UTC").is_empty());
        // Серия из одного экземпляра: `UNTIL` в UTC раньше местного начала.
        let body = event("a", "DTSTART;TZID=Asia/Tokyo:20261102T100000\nDURATION:PT1H\nRRULE:FREQ=DAILY;UNTIL=20261102T010000Z");
        assert_eq!(starts(&body, from, to, "UTC"), pairs(&[("a", date(2026, 11, 2, 1, 0))]));
    }

    /// `UNTIL` датой у серии со временем: день включён.
    #[test]
    fn until_as_a_date_includes_the_day() {
        let body = event("a", "DTSTART;TZID=Europe/Moscow:20261012T120000\nDURATION:PT1H\nRRULE:FREQ=WEEKLY;UNTIL=20261102");
        let (from, to) = week();
        assert_eq!(starts(&body, from, to, "UTC").len(), 1);
    }

    /// Исключения бесконечной серии в разных видах: `TZID`, `Z`, только дата.
    #[test]
    fn exdate_of_an_endless_series() {
        let series = "DTSTART;TZID=America/New_York:20240108T090000\nDTEND;TZID=America/New_York:20240108T100000\nRRULE:FREQ=WEEKLY;BYDAY=MO";
        let (from, to) = week();
        let shown = date(2026, 11, 2, 14, 0);
        assert_eq!(starts(&event("a", series), from, to, "UTC"), pairs(&[("a", shown)]));
        for exdate in ["EXDATE;TZID=America/New_York:20261102T090000", "EXDATE:20261102T140000Z", "EXDATE;VALUE=DATE:20261102"] {
            let body = event("a", &format!("{series}\n{exdate}"));
            assert!(starts(&body, from, to, "UTC").is_empty(), "{exdate}");
            assert_eq!(starts(&body, from + 7 * DAY, to + 7 * DAY, "UTC"), pairs(&[("a", shown + 7 * DAY)]), "{exdate}");
        }
    }

    /// Переопределение бесконечной серии: `SEQUENCE` у Google и Outlook отличается от серии, экземпляр заменён, а не удвоен.
    #[test]
    fn recurrence_id_of_an_endless_series() {
        let series = "DTSTART;TZID=Europe/Moscow:20240101T120000\nDTEND;TZID=Europe/Moscow:20240101T130000\nRRULE:FREQ=WEEKLY;BYDAY=MO\nSEQUENCE:0";
        let (from, to) = week();
        // Перенос на вторник, 15:00 МСК; `RECURRENCE-ID` в `Z`.
        let moved =
            "RECURRENCE-ID:20261102T090000Z\nDTSTART;TZID=Europe/Moscow:20261103T150000\nDTEND;TZID=Europe/Moscow:20261103T160000\nSEQUENCE:3";
        let body = [event("a", series), event("a", moved)].concat();
        let got = events_in(&body, from, to, "UTC");
        let spans: Vec<(i64, i64, bool)> = got.iter().map(|e| (e.start, e.end, e.recurring)).collect();
        assert_eq!(spans, [(date(2026, 11, 3, 12, 0), date(2026, 11, 3, 13, 0), true)]);
        // Экземпляр из-за конца периода перенесён в период, а этот — из периода наружу.
        let body = [
            event("a", series),
            event("a", "RECURRENCE-ID;TZID=Europe/Moscow:20261109T120000\nDTSTART;TZID=Europe/Moscow:20261104T100000\nDURATION:PT1H"),
            event("a", "RECURRENCE-ID;TZID=Europe/Moscow:20261102T120000\nDTSTART;TZID=Europe/Moscow:20261201T100000\nDURATION:PT1H"),
        ]
        .concat();
        assert_eq!(starts(&body, from, to, "UTC"), pairs(&[("a", date(2026, 11, 4, 7, 0))]));
        // Отменённый экземпляр (так пишет Google), у серии `SEQUENCE` нет.
        let cancelled = "RECURRENCE-ID;TZID=Europe/Moscow:20261102T120000\nDTSTART;TZID=Europe/Moscow:20261102T120000\nDURATION:PT1H\nSTATUS:CANCELLED\nSEQUENCE:1";
        let body = [event("a", &series.replace("\nSEQUENCE:0", "")), event("a", cancelled)].concat();
        assert!(starts(&body, from, to, "UTC").is_empty());
        // Изменённый экземпляр в прошлом серии, начало передвинуто к периоду: на неделю не влияет, в своей неделе виден.
        let old = "RECURRENCE-ID;TZID=Europe/Moscow:20240108T120000\nDTSTART;TZID=Europe/Moscow:20240109T120000\nDURATION:PT1H";
        let body = [event("a", series), event("a", old)].concat();
        assert_eq!(starts(&body, from, to, "UTC"), pairs(&[("a", date(2026, 11, 2, 9, 0))]));
        assert_eq!(starts(&body, date(2024, 1, 8, 0, 0), date(2024, 1, 15, 0, 0), "UTC"), pairs(&[("a", date(2024, 1, 9, 9, 0))]));
    }

    /// Переопределение серии на весь день и серии без пояса; второе правило с тем же `UID` — своя серия.
    #[test]
    fn recurrence_id_of_other_kinds() {
        let (from, to) = week();
        let body = [
            event("a", "DTSTART;VALUE=DATE:20250106\nRRULE:FREQ=WEEKLY;BYDAY=MO"),
            event("a", "RECURRENCE-ID;VALUE=DATE:20261102\nDTSTART;VALUE=DATE:20261104\nSEQUENCE:1"),
        ]
        .concat();
        assert_eq!(starts(&body, from, to, "UTC"), pairs(&[("a", date(2026, 11, 4, 0, 0))]));
        let body = [
            event("a", "DTSTART:20250106T120000\nDURATION:PT1H\nRRULE:FREQ=WEEKLY;BYDAY=MO"),
            event("a", "RECURRENCE-ID:20261102T120000\nDTSTART:20261103T120000\nDURATION:PT1H\nSEQUENCE:1"),
        ]
        .concat();
        assert_eq!(starts(&body, from, to, "Europe/Moscow"), pairs(&[("a", date(2026, 11, 3, 9, 0))]));
        let body = [
            event("a", "DTSTART:20100104T090000Z\nDURATION:PT1H\nRRULE:FREQ=WEEKLY;INTERVAL=2"),
            event("a", "DTSTART:20100105T090000Z\nDURATION:PT1H\nRRULE:FREQ=WEEKLY;INTERVAL=3;UNTIL=20200101T000000Z"),
            event("a", "DTSTART:20100106T090000Z\nDURATION:PT1H\nRRULE:FREQ=DAILY;INTERVAL=7"),
        ]
        .concat();
        assert_eq!(starts(&body, from, to, "UTC"), pairs(&[("a", date(2026, 11, 2, 9, 0)), ("a", date(2026, 11, 4, 9, 0))]));
    }

    /// Две серии в один час: переопределение одной не трогает другую (`calcard` не сверяет `UID`).
    #[test]
    fn override_belongs_to_its_uid() {
        let series = "DTSTART:20260105T090000Z\nDURATION:PT1H\nRRULE:FREQ=WEEKLY;BYDAY=MO";
        let moved = "RECURRENCE-ID:20261102T090000Z\nDTSTART:20261103T090000Z\nDURATION:PT1H";
        let body = [event("a", series), event("b", series), event("b", moved)].concat();
        let (from, to) = week();
        assert_eq!(starts(&body, from, to, "UTC"), pairs(&[("a", date(2026, 11, 2, 9, 0)), ("b", date(2026, 11, 3, 9, 0))]));
    }

    /// Изменение «этого и следующих»: дальнейшие экземпляры сдвинуты.
    #[test]
    fn this_and_future_override() {
        let body = [
            event("a", "DTSTART:20240101T090000Z\nDURATION:PT1H\nRRULE:FREQ=WEEKLY;BYDAY=MO"),
            event("a", "RECURRENCE-ID;RANGE=THISANDFUTURE:20260105T090000Z\nDTSTART:20260105T110000Z\nDURATION:PT1H"),
        ]
        .concat();
        let (from, to) = week();
        assert_eq!(starts(&body, from, to, "UTC"), pairs(&[("a", date(2026, 11, 2, 11, 0))]));
    }

    /// `COUNT`: кончилась до периода — экземпляров нет; идёт — считается от начала, исключённый занимает своё место.
    #[test]
    fn count_is_counted_from_the_start() {
        let (from, to) = week();
        assert!(starts(&event("a", "DTSTART:20260105T090000Z\nDURATION:PT1H\nRRULE:FREQ=WEEKLY;COUNT=10"), from, to, "UTC").is_empty());
        // Пятый экземпляр — 2 ноября: 5, 12, 19, 26 октября, 2 ноября.
        let series = "DTSTART:20261005T090000Z\nDURATION:PT1H\nRRULE:FREQ=WEEKLY;COUNT=5";
        assert_eq!(starts(&event("a", series), from, to, "UTC"), pairs(&[("a", date(2026, 11, 2, 9, 0))]));
        assert!(starts(&event("a", series), from + 7 * DAY, to + 7 * DAY, "UTC").is_empty());
        let series = format!("{series}\nEXDATE:20261012T090000Z");
        assert_eq!(starts(&event("a", &series), from, to, "UTC"), pairs(&[("a", date(2026, 11, 2, 9, 0))]));
        assert!(starts(&event("a", &series), from + 7 * DAY, to + 7 * DAY, "UTC").is_empty());
    }

    /// Весь день: в свой день в поясе показа, повтор без конца, несколько дней, `DTEND` равен началу, перевод часов внутри дня.
    #[test]
    fn all_day_stays_in_its_day() {
        let (from, to) = week();
        let midnight = date(2026, 11, 3, 0, 0) - 3 * HOUR;
        // Google: `DTEND` — следующий день.
        let got =
            events_in(&event("a", "DTSTART;VALUE=DATE:20250107\nDTEND;VALUE=DATE:20250108\nRRULE:FREQ=WEEKLY;BYDAY=TU"), from, to, "Europe/Moscow");
        assert_eq!((got.len(), got[0].start, got[0].end, got[0].all_day), (1, midnight, midnight + DAY, true));
        // Исключение датой и датой со временем.
        for exdate in ["EXDATE;VALUE=DATE:20261103", "EXDATE;TZID=Europe/Moscow:20261103T000000"] {
            let body = event("a", &format!("DTSTART;VALUE=DATE:20250107\nRRULE:FREQ=WEEKLY;BYDAY=TU\n{exdate}"));
            assert!(events_in(&body, from, to, "Europe/Moscow").is_empty(), "{exdate}");
        }
        // Несколько дней, конец не включён.
        let got = events_in(&event("a", "DTSTART;VALUE=DATE:20261103\nDTEND;VALUE=DATE:20261106"), from, to, "Europe/Moscow");
        assert_eq!((got[0].start, got[0].end), (midnight, midnight + 3 * DAY));
        // `DTEND` равен началу — один день.
        let got = events_in(&event("a", "DTSTART;VALUE=DATE:20261103\nDTEND;VALUE=DATE:20261103"), from, to, "Europe/Moscow");
        assert_eq!((got[0].start, got[0].end), (midnight, midnight + DAY));
        // Воскресенье 1 ноября в Нью-Йорке длится 25 часов, конец — полночь понедельника.
        let body = event("a", "DTSTART;VALUE=DATE:20261101\nDTEND;VALUE=DATE:20261102");
        let got = events_in(&body, date(2026, 10, 30, 0, 0), date(2026, 11, 5, 0, 0), "America/New_York");
        assert_eq!((got[0].start, got[0].end), (date(2026, 11, 1, 4, 0), date(2026, 11, 2, 5, 0)));
        // Гавана: полуночи 8 марта нет, день начинается в 01:00 (UTC−4).
        let got = events_in(&event("a", "DTSTART;VALUE=DATE:20260308"), date(2026, 3, 6, 0, 0), date(2026, 3, 10, 0, 0), "America/Havana");
        assert_eq!((got[0].start, got[0].end), (date(2026, 3, 8, 5, 0), date(2026, 3, 9, 4, 0)));
    }

    /// Плавающее время и весь день — в поясе показа, а не в UTC: и в подписке, и в занятости по ссылке.
    #[test]
    fn floating_time_is_shown_zone() {
        let (from, to) = week();
        let body = event("a", "DTSTART:20261103T120000\nDTEND:20261103T130000\nRRULE:FREQ=WEEKLY");
        assert_eq!(starts(&body, from, to, "Europe/Moscow"), pairs(&[("a", date(2026, 11, 3, 9, 0))]));
        assert_eq!(starts(&body, from, to, "America/New_York"), pairs(&[("a", date(2026, 11, 3, 17, 0))]));
        let busy = |start: i64, end: i64| BusySpan { start, end, kind: BusyType::Busy };
        let got = parse_free_busy(&file(&body), from, to, "Europe/Moscow").unwrap();
        assert_eq!(got, [busy(date(2026, 11, 3, 9, 0), date(2026, 11, 3, 10, 0))]);
        let midnight = date(2026, 11, 3, 0, 0) - 3 * HOUR;
        let got = parse_free_busy(&file(&event("a", "DTSTART;VALUE=DATE:20261103")), from, to, "Europe/Moscow").unwrap();
        assert_eq!(got, [busy(midnight, midnight + DAY)]);
        // Серия со временем `Z` от пояса занятости не зависит.
        let body = event("a", "DTSTART:20260106T090000Z\nDTEND:20260106T100000Z\nRRULE:FREQ=WEEKLY");
        let got = parse_free_busy(&file(&body), from, to, "Europe/Moscow").unwrap();
        assert_eq!(got, [busy(date(2026, 11, 3, 9, 0), date(2026, 11, 3, 10, 0))]);
    }

    /// Начало за годы до периода: шаг `INTERVAL`, дни недели, месяцы и годы не сбиваются, год до 1970 не пропадает.
    #[test]
    fn series_started_years_ago() {
        let (from, to) = week();
        // Каждый третий день с 3 января 2010: до 2 ноября 2026 — 6147 дней, кратно трём.
        assert_eq!(date(2026, 11, 2, 9, 0) - date(2010, 1, 3, 9, 0), 6147 * DAY);
        let body = event("a", "DTSTART:20100103T090000Z\nDURATION:PT1H\nRRULE:FREQ=DAILY;INTERVAL=3");
        assert_eq!(
            starts(&body, from, to, "UTC"),
            pairs(&[("a", date(2026, 11, 2, 9, 0)), ("a", date(2026, 11, 5, 9, 0)), ("a", date(2026, 11, 8, 9, 0))])
        );
        // Через неделю с понедельника 4 января 2010: до 2 ноября 2026 — 878 недель, чётная подходит, следующая нет.
        assert_eq!(date(2026, 11, 2, 0, 0) - date(2010, 1, 4, 0, 0), 878 * 7 * DAY);
        let body = event("a", "DTSTART:20100104T090000Z\nDURATION:PT1H\nRRULE:FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,TH");
        assert_eq!(starts(&body, from, to, "UTC"), pairs(&[("a", date(2026, 11, 2, 9, 0)), ("a", date(2026, 11, 5, 9, 0))]));
        assert!(starts(&body, from + 7 * DAY, to + 7 * DAY, "UTC").is_empty());
        // Рабочие дни с 2015.
        let body = event("a", "DTSTART;TZID=Europe/Moscow:20150105T100000\nDURATION:PT1H\nRRULE:FREQ=DAILY;BYDAY=MO,TU,WE,TH,FR");
        assert_eq!(starts(&body, from, to, "UTC").len(), 5);
        // Второй вторник месяца с 2001 — 10 ноября 2026.
        let body = event("a", "DTSTART:20010109T090000Z\nDURATION:PT1H\nRRULE:FREQ=MONTHLY;BYDAY=2TU");
        assert_eq!(starts(&body, from + 7 * DAY, to + 7 * DAY, "UTC"), pairs(&[("a", date(2026, 11, 10, 9, 0))]));
        // День рождения с 1950: до 1970 секунды отрицательные.
        let body = event("a", "DTSTART;VALUE=DATE:19501103\nRRULE:FREQ=YEARLY");
        assert_eq!(starts(&body, from, to, "UTC"), pairs(&[("a", date(2026, 11, 3, 0, 0))]));
        // Двухнедельное событие раз в шесть недель началось до периода: сдвиг начала его не теряет. 876 недель кратно шести.
        assert_eq!(date(2026, 10, 19, 9, 0) - date(2010, 1, 4, 9, 0), 876 * 7 * DAY);
        let body = event("a", "DTSTART:20100104T090000Z\nDTEND:20100118T090000Z\nRRULE:FREQ=WEEKLY;INTERVAL=6");
        assert_eq!(starts(&body, from, to, "UTC"), pairs(&[("a", date(2026, 10, 19, 9, 0))]));
    }

    /// Перевод часов: местное время серии держится, «дыра» — смещение до перевода, «складка» — первый раз.
    #[test]
    fn daylight_saving() {
        let series = |start: &str| format!("DTSTART;TZID=America/New_York:{start}\nDURATION:PT1H\nRRULE:FREQ=WEEKLY;BYDAY=SU");
        let day = |year, month, day| (date(year, month, day, 0, 0), date(year, month, day, 23, 59));
        // Обычный час: 09:00 местного до и после перевода — 14:00 и 13:00 UTC.
        let body = event("a", &series("20240107T090000"));
        for ((from, to), want) in
            [(day(2026, 3, 1), date(2026, 3, 1, 14, 0)), (day(2026, 3, 8), date(2026, 3, 8, 13, 0)), (day(2026, 11, 1), date(2026, 11, 1, 14, 0))]
        {
            assert_eq!(starts(&body, from, to, "Europe/Moscow"), pairs(&[("a", want)]));
        }
        // 02:30 8 марта нет: по смещению до перевода (UTC−5) это 07:30 UTC, на часах 03:30.
        let body = event("a", &series("20240107T023000"));
        let (from, to) = day(2026, 3, 8);
        assert_eq!(starts(&body, from, to, "UTC"), pairs(&[("a", date(2026, 3, 8, 7, 30))]));
        // 01:30 1 ноября дважды: первый раз, UTC−4, 05:30 UTC.
        let body = event("a", &series("20240107T013000"));
        let (from, to) = day(2026, 11, 1);
        assert_eq!(starts(&body, from, to, "UTC"), pairs(&[("a", date(2026, 11, 1, 5, 30))]));
        // Исключение на этот час находит экземпляр.
        let body = event("a", &format!("{}\nEXDATE;TZID=America/New_York:20261101T013000", series("20240107T013000")));
        assert!(starts(&body, from, to, "UTC").is_empty());
        // Начало серии само в «дыре»: без поправки пропала бы вся серия.
        let body = event("a", &series("20260308T023000"));
        let (from, to) = day(2026, 3, 15);
        assert_eq!(starts(&body, from, to, "UTC"), pairs(&[("a", date(2026, 3, 15, 6, 30))]));
        // Событие без повтора в «дыре».
        let body = event("a", "DTSTART;TZID=America/New_York:20260308T023000\nDURATION:PT1H");
        let (from, to) = day(2026, 3, 8);
        assert_eq!(starts(&body, from, to, "UTC"), pairs(&[("a", date(2026, 3, 8, 7, 30))]));
        // Весь день серии — полночь пояса показа и по обе стороны перевода.
        let body = event("a", "DTSTART;VALUE=DATE:20240107\nRRULE:FREQ=WEEKLY;BYDAY=SU");
        let got = events_in(&body, date(2026, 3, 7, 0, 0), date(2026, 3, 9, 0, 0), "America/New_York");
        assert_eq!((got[0].start, got[0].end), (date(2026, 3, 8, 5, 0), date(2026, 3, 9, 4, 0)));
    }

    /// Пояс Windows из файла Outlook, `VTIMEZONE` с таким именем, серия и `EXDATE` в нём: пояс показа другой.
    #[test]
    fn outlook_zone_name() {
        let zone = "BEGIN:VTIMEZONE\nTZID:Russian Standard Time\nBEGIN:STANDARD\nDTSTART:16010101T000000\nTZOFFSETFROM:+0300\nTZOFFSETTO:+0300\nEND:STANDARD\nEND:VTIMEZONE\n";
        let series = "DTSTART;TZID=Russian Standard Time:20250106T120000\nDTEND;TZID=Russian Standard Time:20250106T130000\nRRULE:FREQ=WEEKLY;BYDAY=MO;WKST=SU";
        let body = format!("{zone}{}", event("a", &format!("{series}\nEXDATE;TZID=Russian Standard Time:20261109T120000")));
        let (from, to) = week();
        assert_eq!(starts(&body, from, to, "America/New_York"), pairs(&[("a", date(2026, 11, 2, 9, 0))]));
        assert!(starts(&body, from + 7 * DAY, to + 7 * DAY, "America/New_York").is_empty());
    }

    /// Начало и конец в разных поясах (перелёт у Google): длительность настоящая.
    #[test]
    fn end_in_another_zone() {
        let body = event("a", "DTSTART;TZID=Europe/Moscow:20260105T120000\nDTEND;TZID=Asia/Tokyo:20260105T200000\nRRULE:FREQ=WEEKLY");
        let (from, to) = week();
        let got = events_in(&body, from, to, "UTC");
        assert_eq!((got[0].start, got[0].end), (date(2026, 11, 2, 9, 0), date(2026, 11, 2, 11, 0)));
    }

    /// Без `DTEND` и `DURATION` — нулевая длина: занятости нет, а календарь событие показывает.
    #[test]
    fn no_end_is_zero_length() {
        let (from, to) = week();
        let body = event("a", "DTSTART:20261103T090000Z\nRRULE:FREQ=WEEKLY");
        let got = events_in(&body, from, to, "UTC");
        assert_eq!((got[0].start, got[0].end), (date(2026, 11, 3, 9, 0), date(2026, 11, 3, 9, 0)));
        assert!(parse_free_busy(&file(&body), from, to, "UTC").unwrap().is_empty());
    }

    /// Дополнительные даты: `RDATE` с поясом и в UTC, вне правила.
    #[test]
    fn rdate_with_zone() {
        let lines = "DTSTART;TZID=Europe/Moscow:20260105T120000\nDURATION:PT1H\nRRULE:FREQ=WEEKLY;COUNT=2\nRDATE;TZID=Europe/Moscow:20261103T120000\nRDATE:20261104T120000Z";
        let (from, to) = week();
        assert_eq!(starts(&event("a", lines), from, to, "UTC"), pairs(&[("a", date(2026, 11, 3, 9, 0)), ("a", date(2026, 11, 4, 12, 0))]));
    }

    /// Файл из тысяч бесконечных серий со старой датой начала: все на месте, быстрее сторожа воркера.
    #[test]
    fn thousands_of_endless_series() {
        let mut body = String::new();
        for i in 0..4000 {
            let lines = format!("DTSTART:20100104T{:02}{:02}00Z\nDURATION:PT30M\nRRULE:FREQ=WEEKLY;BYDAY=MO", i / 60 % 24, i % 60);
            body += &event(&format!("s{i}@example.test"), &lines);
        }
        let (from, to) = week();
        let bytes = file(&body);
        let clock = Instant::now();
        let got = parse_ics(&bytes, from, to, "Europe/Moscow").unwrap().events;
        println!("{} байт, {} событий: {:?}", bytes.len(), got.len(), clock.elapsed());
        assert_eq!(got.len(), 4000);
        assert!(clock.elapsed() < TIME_LIMIT);
    }

    /// Файл предельного размера из дорогих правил одного вида.
    fn hostile(rule: &str) -> String {
        let mut body = String::new();
        let mut i = 0;
        // Шесть строк серии, перевод строки в файле — два байта.
        while body.len() + 6 * i < crate::MAX_ICS - 2048 {
            body += &format!("BEGIN:VEVENT\nUID:h{i}\nDTSTART:19710101T000000Z\nDURATION:PT1H\nRRULE:{rule}\nEND:VEVENT\n");
            i += 1;
        }
        body
    }

    /// Предел работы: файл предельного размера с дорогими правилами укладывается в сторожа воркера.
    #[test]
    fn hostile_rules_are_bounded() {
        let (from, to) = week();
        for rule in [
            "FREQ=SECONDLY",
            "FREQ=MINUTELY;BYSECOND=0,10,20,30,40,50",
            // Редкое правило: тысячи шагов на один экземпляр.
            "FREQ=SECONDLY;BYHOUR=3;BYMINUTE=3;BYSECOND=3",
            // Ни одного экземпляра: `calcard` сдаётся через 100 000 шагов.
            "FREQ=SECONDLY;BYMONTH=2;BYMONTHDAY=30",
            "FREQ=SECONDLY;COUNT=4000000000",
            "FREQ=DAILY;COUNT=4000000000",
            "FREQ=YEARLY;BYHOUR=0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23;BYMINUTE=0,1,2,3,4,5,6,7,8,9,10;BYSECOND=0,1,2,3,4,5,6,7,8,9,10;BYDAY=MO,TU,WE,TH,FR,SA,SU",
            "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR,SA,SU;BYSETPOS=1,2,3,4,5,6,7,8,9,10",
            "FREQ=YEARLY;BYDAY=MO,TU,WE,TH,FR,SA,SU",
        ] {
            let bytes = file(&hostile(rule));
            let clock = Instant::now();
            let got = parse_ics(&bytes, from, to, "UTC").unwrap().events;
            println!("{rule}: {} байт, {} событий, {:?}", bytes.len(), got.len(), clock.elapsed());
            assert!(clock.elapsed() < TIME_LIMIT, "{rule}");
        }
    }

    /// Огромная длительность и даты на краю: ни переполнения, ни паники.
    #[test]
    fn extreme_values_do_not_panic() {
        let (from, to) = week();
        for lines in [
            "DTSTART:19710101T000000Z\nDURATION:PT99999999W\nRRULE:FREQ=WEEKLY",
            "DTSTART:19710101T000000Z\nDURATION:P99999999D",
            "DTSTART:00010101T000000Z\nDURATION:PT1H\nRRULE:FREQ=DAILY",
            "DTSTART:99991231T235959Z\nDURATION:PT1H\nRRULE:FREQ=SECONDLY",
            "DTSTART;VALUE=DATE:99991231\nDURATION:P99999999W",
            "DTSTART:20261103T090000Z\nDTEND:20261103T080000Z",
            "DTSTART:20261103T090000Z\nDURATION:PT1H\nRRULE:FREQ=WEEKLY;INTERVAL=0",
            "DTSTART:20261103T090000Z\nDTEND:20261399T090000Z\nEXDATE:20261399T090000Z\nRECURRENCE-ID:20261399T090000Z",
            "DTSTART:20261103T090000Z\nDURATION:PT1H\nRRULE:FREQ=WEEKLY;UNTIL=00010101T000000Z",
            "DTSTART;TZID=Нет/Такого:20261103T090000\nDURATION:PT1H\nRRULE:FREQ=WEEKLY",
        ] {
            let _ = parse_ics(&file(&event("a", lines)), from, to, "Europe/Moscow").unwrap();
        }
        let _ = parse_ics(&file(&event("a", "DTSTART:20261103T090000Z\nDURATION:PT1H")), from, to, "Нет/Такого").unwrap();
    }

    /// Дорогие серии идут последними: работу у обычных событий и серий файла они не отнимают.
    #[test]
    fn expensive_series_do_not_starve_plain_ones() {
        let (from, to) = week();
        let mut body = event("plain", "DTSTART:20261103T090000Z\nDURATION:PT1H");
        body += &event("weekly", "DTSTART:20200106T090000Z\nDURATION:PT1H\nRRULE:FREQ=WEEKLY");
        body += &hostile("FREQ=SECONDLY;BYMONTH=1");
        let got = parse_ics(&file(&body), from, to, "UTC").unwrap().events;
        assert_eq!(got.iter().map(|e| e.uid.as_str()).collect::<Vec<_>>(), ["weekly", "plain"]);
    }

    /// Как у Outlook: `TZID` без имени в базе, правила с 1601 года, начало не совпадает с правилом. Те же переводы, что в Нью-Йорке.
    const CUSTOM_NORTH: &str = "BEGIN:VTIMEZONE\nTZID:Customized Time Zone\nBEGIN:STANDARD\nDTSTART:16010101T020000\nTZOFFSETFROM:-0400\nTZOFFSETTO:-0500\nRRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=1SU;BYMONTH=11\nEND:STANDARD\nBEGIN:DAYLIGHT\nDTSTART:16010101T020000\nTZOFFSETFROM:-0500\nTZOFFSETTO:-0400\nRRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=2SU;BYMONTH=3\nEND:DAYLIGHT\nEND:VTIMEZONE\n";
    /// Южное полушарие: лето на стыке лет. Те же переводы, что в Сиднее.
    const CUSTOM_SOUTH: &str = "BEGIN:VTIMEZONE\nTZID:Customized South\nBEGIN:STANDARD\nDTSTART:19700405T030000\nTZOFFSETFROM:+1100\nTZOFFSETTO:+1000\nRRULE:FREQ=YEARLY;BYMONTH=4;BYDAY=1SU\nEND:STANDARD\nBEGIN:DAYLIGHT\nDTSTART:19701004T020000\nTZOFFSETFROM:+1000\nTZOFFSETTO:+1100\nRRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=1SU\nEND:DAYLIGHT\nEND:VTIMEZONE\n";

    fn zoned(zone: &str, lines: &str) -> String {
        format!("{zone}{}", event("a", lines))
    }

    /// Серия в поясе файла и в поясе базы: одно и то же на каждой неделе `weeks` начиная с понедельника `monday`.
    fn same_as_named(zone: &str, custom: &str, named: &str, lines: &str, monday: i64, weeks: i64) {
        for week in 0..weeks {
            let (from, to) = (monday + week * 7 * DAY, monday + (week + 1) * 7 * DAY);
            let times = |body: &str| events_in(body, from, to, "Europe/Moscow").into_iter().map(|e| (e.start, e.end)).collect::<Vec<_>>();
            let got = times(&zoned(zone, &lines.replace("@", custom)));
            let want = times(&event("a", &lines.replace("@", named)));
            assert!(!want.is_empty(), "неделя {week}");
            assert_eq!(got, want, "{custom}, {lines}, неделя {week}");
        }
    }

    /// Серия через переходы весь год, в том числе час «дыры» и час «складки»: как пояс базы с теми же правилами.
    #[test]
    fn file_zone_is_the_same_as_named_one() {
        let series = |time: &str| format!("DTSTART;TZID=@:20250105T{time}\nDURATION:PT1H\nRRULE:FREQ=WEEKLY;BYDAY=SU");
        let monday = date(2026, 1, 5, 0, 0);
        for time in ["090000", "023000", "013000"] {
            same_as_named(CUSTOM_NORTH, "Customized Time Zone", "America/New_York", &series(time), monday, 60);
        }
        for time in ["090000", "023000"] {
            same_as_named(CUSTOM_SOUTH, "Customized South", "Australia/Sydney", &series(time), monday, 60);
        }
    }

    /// Сутки перехода: абсолютные значения, без сравнения с поясом базы.
    #[test]
    fn file_zone_on_transition_days() {
        let one = |zone: &str, tzid: &str, start: &str, end: &str, day: (u16, u8, u8)| {
            let lines = format!("DTSTART;TZID={tzid}:{start}\nDTEND;TZID={tzid}:{end}");
            let from = date(day.0, day.1, day.2, 0, 0) - 2 * DAY;
            let got = events_in(&zoned(zone, &lines), from, from + 5 * DAY, "UTC");
            (got[0].start, got[0].end)
        };
        let north = |start, end, day| one(CUSTOM_NORTH, "Customized Time Zone", start, end, day);
        // Весна, 8 марта: 01:30 ещё UTC−5, 03:30 уже UTC−4, между ними — час, которого нет.
        assert_eq!(north("20260308T013000", "20260308T033000", (2026, 3, 8)), (date(2026, 3, 8, 6, 30), date(2026, 3, 8, 7, 30)));
        // «Дыра»: 02:30 по смещению до перевода — 07:30 UTC.
        assert_eq!(north("20260308T023000", "20260308T033000", (2026, 3, 8)).0, date(2026, 3, 8, 7, 30));
        // Осень, 1 ноября: «складка», 01:30 — первый раз, UTC−4; 03:00 — уже UTC−5.
        assert_eq!(north("20261101T013000", "20261101T030000", (2026, 11, 1)), (date(2026, 11, 1, 5, 30), date(2026, 11, 1, 8, 0)));
        let south = |start, end, day| one(CUSTOM_SOUTH, "Customized South", start, end, day);
        // Сидней, весна 4 октября: 02:30 нет, по UTC+10 это 16:30 UTC прошлых суток.
        assert_eq!(south("20261004T023000", "20261004T040000", (2026, 10, 4)), (date(2026, 10, 3, 16, 30), date(2026, 10, 3, 17, 0)));
        // Сидней, осень 5 апреля: 02:30 дважды, первый раз по UTC+11.
        assert_eq!(south("20260405T023000", "20260405T040000", (2026, 4, 5)).0, date(2026, 4, 4, 15, 30));
        // Сидней, лето на стыке лет: 12:00 — UTC+11, зима — UTC+10.
        assert_eq!(south("20261231T120000", "20261231T130000", (2026, 12, 31)).0, date(2026, 12, 31, 1, 0));
        assert_eq!(south("20270101T120000", "20270101T130000", (2027, 1, 1)).0, date(2027, 1, 1, 1, 0));
        assert_eq!(south("20260701T120000", "20260701T130000", (2026, 7, 1)).0, date(2026, 7, 1, 2, 0));
    }

    /// Смена правил: старые правила с `UNTIL`, новые со своего года, как у часовых поясов США в 2007-м.
    #[test]
    fn file_zone_with_changed_rules() {
        let zone = "BEGIN:VTIMEZONE\nTZID:Customized History\nBEGIN:STANDARD\nDTSTART:19671029T020000\nTZOFFSETFROM:-0400\nTZOFFSETTO:-0500\nRRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU;UNTIL=20061029T060000Z\nEND:STANDARD\nBEGIN:STANDARD\nDTSTART:20071104T020000\nTZOFFSETFROM:-0400\nTZOFFSETTO:-0500\nRRULE:FREQ=YEARLY;BYMONTH=11;BYDAY=1SU\nEND:STANDARD\nBEGIN:DAYLIGHT\nDTSTART:19870405T020000\nTZOFFSETFROM:-0500\nTZOFFSETTO:-0400\nRRULE:FREQ=YEARLY;BYMONTH=4;BYDAY=1SU;UNTIL=20060402T070000Z\nEND:DAYLIGHT\nBEGIN:DAYLIGHT\nDTSTART:20070311T020000\nTZOFFSETFROM:-0500\nTZOFFSETTO:-0400\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=2SU\nEND:DAYLIGHT\nEND:VTIMEZONE\n";
        let lines = "DTSTART;TZID=@:20000102T090000\nDURATION:PT1H\nRRULE:FREQ=WEEKLY;BYDAY=SU";
        same_as_named(zone, "Customized History", "America/New_York", lines, date(2005, 1, 3, 0, 0), 110);
        same_as_named(zone, "Customized History", "America/New_York", lines, date(2026, 1, 5, 0, 0), 60);
        // Правила кончились годы назад: действует их последний перевод, сентябрь 2026 — UTC+3.
        let zone = "BEGIN:VTIMEZONE\nTZID:Customized Ended\nBEGIN:STANDARD\nDTSTART:19961027T030000\nTZOFFSETFROM:+0400\nTZOFFSETTO:+0300\nRRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU;UNTIL=20101030T230000Z\nEND:STANDARD\nBEGIN:DAYLIGHT\nDTSTART:19930328T020000\nTZOFFSETFROM:+0300\nTZOFFSETTO:+0400\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU;UNTIL=20100327T230000Z\nEND:DAYLIGHT\nEND:VTIMEZONE\n";
        let body = zoned(zone, "DTSTART;TZID=Customized Ended:20260907T120000\nDURATION:PT1H");
        assert_eq!(starts(&body, date(2026, 9, 7, 0, 0), date(2026, 9, 8, 0, 0), "UTC"), pairs(&[("a", date(2026, 9, 7, 9, 0))]));
        // Переводы только по `RDATE`, как в файлах по таблицам поясов: те же, что в Хельсинки в 2026.
        let zone = "BEGIN:VTIMEZONE\nTZID:Customized Dates\nBEGIN:STANDARD\nDTSTART:20251026T040000\nRDATE:20261025T040000\nTZOFFSETFROM:+0300\nTZOFFSETTO:+0200\nEND:STANDARD\nBEGIN:DAYLIGHT\nDTSTART:20250330T030000\nRDATE:20260329T030000\nTZOFFSETFROM:+0200\nTZOFFSETTO:+0300\nEND:DAYLIGHT\nEND:VTIMEZONE\n";
        let lines = "DTSTART;TZID=@:20250105T090000\nDURATION:PT1H\nRRULE:FREQ=WEEKLY;BYDAY=SU";
        same_as_named(zone, "Customized Dates", "Europe/Helsinki", lines, date(2026, 1, 5, 0, 0), 50);
    }

    /// Постоянное смещение (`Customized Time Zone` с одним `STANDARD`): серия, исключения и перенос в поясе файла, `Z` и `TZID`.
    #[test]
    fn file_zone_in_series_parts() {
        let zone = "BEGIN:VTIMEZONE\nTZID:Customized Time Zone\nBEGIN:STANDARD\nDTSTART:16010101T000000\nTZOFFSETFROM:+0300\nTZOFFSETTO:+0300\nEND:STANDARD\nEND:VTIMEZONE\n";
        let tzid = "TZID=Customized Time Zone";
        let series = format!("DTSTART;{tzid}:20240101T120000\nDTEND;{tzid}:20240101T130000\nRRULE:FREQ=WEEKLY;BYDAY=MO");
        let (from, to) = week();
        let shown = date(2026, 11, 2, 9, 0);
        assert_eq!(starts(&zoned(zone, &series), from, to, "America/New_York"), pairs(&[("a", shown)]));
        for exdate in [format!("EXDATE;{tzid}:20261102T120000"), "EXDATE:20261102T090000Z".to_owned(), "EXDATE;VALUE=DATE:20261102".to_owned()] {
            assert!(starts(&zoned(zone, &format!("{series}\n{exdate}")), from, to, "America/New_York").is_empty(), "{exdate}");
        }
        for rid in [format!("RECURRENCE-ID;{tzid}:20261102T120000"), "RECURRENCE-ID:20261102T090000Z".to_owned()] {
            let moved = format!("{rid}\nDTSTART;{tzid}:20261103T150000\nDURATION:PT1H\nSEQUENCE:2");
            let body = format!("{zone}{}{}", event("a", &series), event("a", &moved));
            assert_eq!(starts(&body, from, to, "America/New_York"), pairs(&[("a", date(2026, 11, 3, 12, 0))]), "{rid}");
        }
        // `UNTIL` в UTC на последнем экземпляре.
        let last = format!("DTSTART;{tzid}:20261012T120000\nDURATION:PT1H\nRRULE:FREQ=WEEKLY;UNTIL=20261102T090000Z");
        assert_eq!(starts(&zoned(zone, &last), from, to, "UTC"), pairs(&[("a", shown)]));
        assert!(starts(&zoned(zone, &last), from + 7 * DAY, to + 7 * DAY, "UTC").is_empty());
        // Конец в другом поясе и событие на весь день пояс файла не задевает.
        let body = zoned(zone, &format!("DTSTART;{tzid}:20261102T120000\nDTEND;TZID=Asia/Tokyo:20261102T200000"));
        let got = events_in(&body, from, to, "UTC");
        assert_eq!((got[0].start, got[0].end), (shown, date(2026, 11, 2, 11, 0)));
        // Занятость по ссылке — тот же разбор.
        let got = parse_free_busy(&file(&zoned(zone, &series)), from, to, "UTC").unwrap();
        assert_eq!(got, [BusySpan { start: shown, end: shown + HOUR, kind: BusyType::Busy }]);
    }

    /// Битый `VTIMEZONE`: как без него, пояс показа. Одно разборчивое правило из двух работает.
    #[test]
    fn broken_file_zone() {
        let lines = "DTSTART;TZID=Customized Time Zone:20261103T120000\nDURATION:PT1H";
        let (from, to) = week();
        let shown = pairs(&[("a", date(2026, 11, 3, 9, 0))]);
        assert_eq!(starts(&event("a", lines), from, to, "Europe/Moscow"), shown, "без VTIMEZONE");
        let wrap = |inner: &str| format!("BEGIN:VTIMEZONE\nTZID:Customized Time Zone\n{inner}END:VTIMEZONE\n");
        let broken = [
            wrap(""),
            wrap("BEGIN:STANDARD\nDTSTART:16010101T000000\nTZOFFSETFROM:+0900\nEND:STANDARD\n"),
            wrap("BEGIN:STANDARD\nDTSTART:16010101T000000\nTZOFFSETTO:+0900\nEND:STANDARD\n"),
            wrap("BEGIN:STANDARD\nTZOFFSETFROM:+0900\nTZOFFSETTO:+0900\nEND:STANDARD\n"),
            wrap("BEGIN:STANDARD\nDTSTART:нет\nTZOFFSETFROM:+0900\nTZOFFSETTO:+0900\nEND:STANDARD\n"),
            wrap("BEGIN:STANDARD\nDTSTART:16010101T000000\nTZOFFSETFROM:нет\nTZOFFSETTO:+0900\nEND:STANDARD\n"),
            wrap("BEGIN:STANDARD\nDTSTART:16010101T000000\nTZOFFSETFROM:+0900\nTZOFFSETTO:+9900\nEND:STANDARD\n"),
            wrap("BEGIN:X-OTHER\nDTSTART:16010101T000000\nTZOFFSETFROM:+0900\nTZOFFSETTO:+0900\nEND:X-OTHER\n"),
            "BEGIN:VTIMEZONE\nBEGIN:STANDARD\nDTSTART:16010101T000000\nTZOFFSETFROM:+0900\nTZOFFSETTO:+0900\nEND:STANDARD\nEND:VTIMEZONE\n"
                .to_owned(),
        ];
        for zone in &broken {
            assert_eq!(starts(&zoned(zone, lines), from, to, "Europe/Moscow"), shown, "{zone}");
        }
        // Правило с непонятным `RRULE` и правило без конца: `RRULE` не разобран — остаётся начало, смещение всё равно есть.
        let odd = wrap("BEGIN:STANDARD\nDTSTART:16010101T000000\nTZOFFSETFROM:+0900\nTZOFFSETTO:+0900\nRRULE:FREQ=НЕТ\nEND:STANDARD\n");
        let _ = starts(&zoned(&odd, lines), from, to, "Europe/Moscow");
        let half = wrap(
            "BEGIN:STANDARD\nDTSTART:16010101T000000\nTZOFFSETFROM:+0900\nTZOFFSETTO:+0900\nEND:STANDARD\nBEGIN:DAYLIGHT\nDTSTART:нет\nTZOFFSETFROM:+0900\nTZOFFSETTO:+1000\nEND:DAYLIGHT\n",
        );
        assert_eq!(starts(&zoned(&half, lines), from, to, "Europe/Moscow"), pairs(&[("a", date(2026, 11, 3, 3, 0))]));
        // У пояса из базы и с `X-LIC-LOCATION` правила файла не читаются.
        let lying = "BEGIN:VTIMEZONE\nTZID:Customized Time Zone\nX-LIC-LOCATION:Asia/Tokyo\nBEGIN:STANDARD\nDTSTART:16010101T000000\nTZOFFSETFROM:+0100\nTZOFFSETTO:+0100\nEND:STANDARD\nEND:VTIMEZONE\n";
        assert_eq!(starts(&zoned(lying, lines), from, to, "Europe/Moscow"), pairs(&[("a", date(2026, 11, 3, 3, 0))]));
        let lying = "BEGIN:VTIMEZONE\nTZID:Asia/Tokyo\nBEGIN:STANDARD\nDTSTART:16010101T000000\nTZOFFSETFROM:+0100\nTZOFFSETTO:+0100\nEND:STANDARD\nEND:VTIMEZONE\n";
        let body = zoned(lying, "DTSTART;TZID=Asia/Tokyo:20261103T120000\nDURATION:PT1H");
        assert_eq!(starts(&body, from, to, "Europe/Moscow"), pairs(&[("a", date(2026, 11, 3, 3, 0))]));
    }

    /// Тысячи поясов и правил на пределе размера: воркер укладывается, дальше — пояс показа, обычные события на месте.
    #[test]
    fn hostile_file_zones_are_bounded() {
        let (from, to) = week();
        type Maker = fn(usize) -> String;
        let cases: [(&str, Maker); 4] = [
            ("обычные годовые", |i| {
                format!(
                    "BEGIN:VTIMEZONE\nTZID:Z{i}\nBEGIN:STANDARD\nDTSTART:16010101T030000\nTZOFFSETFROM:+0400\nTZOFFSETTO:+0300\nRRULE:FREQ=YEARLY;BYDAY=-1SU;BYMONTH=10\nEND:STANDARD\nBEGIN:DAYLIGHT\nDTSTART:16010101T020000\nTZOFFSETFROM:+0300\nTZOFFSETTO:+0400\nRRULE:FREQ=YEARLY;BYDAY=-1SU;BYMONTH=3\nEND:DAYLIGHT\nEND:VTIMEZONE\n"
                )
            }),
            ("секундные", |i| {
                format!(
                    "BEGIN:VTIMEZONE\nTZID:Z{i}\nBEGIN:STANDARD\nDTSTART:16010101T030000\nTZOFFSETFROM:+0400\nTZOFFSETTO:+0300\nRRULE:FREQ=SECONDLY\nEND:STANDARD\nEND:VTIMEZONE\n"
                )
            }),
            ("годовые COUNT", |i| {
                format!(
                    "BEGIN:VTIMEZONE\nTZID:Z{i}\nBEGIN:STANDARD\nDTSTART:16010101T030000\nTZOFFSETFROM:+0400\nTZOFFSETTO:+0300\nRRULE:FREQ=YEARLY;COUNT=4000000\nEND:STANDARD\nEND:VTIMEZONE\n"
                )
            }),
            ("много RDATE", |i| {
                let list = (0..500).map(|d| format!("RDATE:2026{:02}{:02}T030000\n", d % 12 + 1, d % 28 + 1)).collect::<String>();
                format!(
                    "BEGIN:VTIMEZONE\nTZID:Z{i}\nBEGIN:STANDARD\nDTSTART:16010101T030000\n{list}TZOFFSETFROM:+0400\nTZOFFSETTO:+0300\nEND:STANDARD\nEND:VTIMEZONE\n"
                )
            }),
        ];
        for (name, zone) in cases {
            let mut body = event("plain", "DTSTART;TZID=Asia/Tokyo:20261103T120000\nDURATION:PT1H");
            let mut i = 0;
            // Размер считается с переводами строк файла: в нём их по два байта.
            let mut size = body.len() + 1024;
            while size < crate::MAX_ICS - 16_384 {
                let chunk = zone(i) + &event(&format!("e{i}"), &format!("DTSTART;TZID=Z{i}:20261104T120000\nDURATION:PT1H"));
                size += chunk.len() + chunk.matches('\n').count();
                body += &chunk;
                i += 1;
            }
            let bytes = file(&body);
            let clock = Instant::now();
            let (got, cut) = crate::ics_of(&bytes, from, to, "UTC").unwrap();
            println!("{name}: {} байт, {i} поясов, {} событий, {cut:?}: {:?}", bytes.len(), got.events.len(), clock.elapsed());
            assert!(clock.elapsed() < TIME_LIMIT, "{name}");
            assert!(got.events.iter().any(|e| e.uid == "plain"), "{name}");
            assert!(cut.zones_skipped > 0, "{name}: {cut:?}");
        }
    }

    /// Пределы считаются, а в тексте для журнала только числа и название предела.
    #[test]
    fn cut_counts_limits() {
        let (from, to) = week();
        let quiet = crate::ics_of(&file(&event("a", "DTSTART:20261103T090000Z\nDURATION:PT1H\nRRULE:FREQ=WEEKLY")), from, to, "UTC").unwrap().1;
        assert!(quiet.is_empty(), "{quiet:?}");
        assert_eq!(quiet.to_string(), "");
        // Работа: секундные серии по миллиону с лишним шагов. Название и UID — приметные: в журнал не попадают.
        let series = |i: usize| {
            format!(
                "BEGIN:VEVENT\nUID:secret-{i}@example.test\nDTSTART:20260101T000000Z\nDURATION:PT1S\nRRULE:FREQ=SECONDLY\nSUMMARY:Тайное совещание\nEND:VEVENT\n"
            )
        };
        let body = (0..30).map(series).collect::<String>();
        let cut = crate::ics_of(&file(&body), from, to, "UTC").unwrap().1;
        assert_eq!((cut.series, cut.series_skipped > 0), (30, true), "{cut:?}");
        let text = cut.to_string();
        assert!(text.contains("series not expanded (work or instance limit)"), "{text}");
        assert!(text.is_ascii() && !text.contains("secret") && !text.contains("example"), "{text}");
        // Событий подписки: 6000 в неделе, показано 5000.
        let body = (0..6000).map(|i| event(&format!("e{i}"), "DTSTART:20261103T090000Z\nDURATION:PT1H")).collect::<String>();
        let (got, cut) = crate::ics_of(&file(&body), from, to, "UTC").unwrap();
        assert_eq!((got.events.len(), cut.events_dropped), (crate::MAX_ICS_EVENTS, 1000), "{cut:?}");
        assert!(cut.to_string().contains("1000 events dropped (event limit)"));
        // Занятость: интервалов VFREEBUSY больше предела.
        let lines = (0..20_100).map(|i| format!("FREEBUSY:20261103T{:02}{:02}{:02}Z/PT1S\n", i / 3600, i / 60 % 60, i % 60)).collect::<String>();
        let body = format!("BEGIN:VFREEBUSY\n{lines}END:VFREEBUSY\n");
        let (got, cut) = crate::free_busy_of(&file(&body), from, to, "UTC").unwrap();
        assert_eq!((got.len(), cut.busy_dropped), (20_000, 100), "{cut:?}");
        // Экземпляров: каждая секунда недели в файле, сверх предела места нет.
        let body = event("a", "DTSTART:20261102T000000Z\nDURATION:PT1S\nRRULE:FREQ=SECONDLY");
        let cut = crate::ics_of(&file(&body), from, to, "UTC").unwrap().1;
        assert!(cut.instances_dropped > 0 && cut.events_dropped > 0, "{cut:?}");
    }
}
