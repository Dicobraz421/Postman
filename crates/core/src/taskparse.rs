//! Быстрое добавление задачи (S6-04, решение 5 S6): разбор фразы «Позвонить Беловой пт 11:00 #Команда !» на
//! название, срок (дата и время), категорию и «!». Без I/O: «сегодня», часы «утром» / «вечером» и таблица категорий
//! — параметрами.
//!
//! Язык — интерфейса (ru или en); числа и даты (`15.11`, `15:30`) понимаются в обоих. Неоднозначность — ближайшая
//! дата впереди: «пт» в пятницу — следующая, «15.03» после 15 марта — в следующем году, время без даты, если оно
//! уже прошло, — завтра. Проект в строке не разбирается (решение 5) — он в карточке.
//!
//! Распознанное — чипами ([`Chip`]: что и где в строке); пользователь снимает распознавание крестиком — текст чипа
//! передаётся в `ignored`, и он остаётся частью названия.

use std::ops::Range;

use jiff::ToSpan;
use jiff::civil::{Date, Time, Weekday};

use crate::fuzzy::{folded, other_layout};
use crate::labels::Categories;

/// Язык разбора — язык интерфейса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Ru,
    En,
}

/// Что распознано.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipKind {
    /// Дата и (или) время срока.
    Due,
    Category,
    Important,
}

/// Распознанный кусок строки (байтовый диапазон по исходному тексту) — чип в поле.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    pub kind: ChipKind,
    pub range: Range<usize>,
    /// Текст куска как в строке (по нему снимают распознавание).
    pub text: String,
}

/// Итог разбора.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Parsed {
    pub title: String,
    pub date: Option<Date>,
    pub time: Option<Time>,
    /// Ключевое слово категории (`pm.cat.<id>` или как в таблице).
    pub category: Option<String>,
    pub important: bool,
    pub chips: Vec<Chip>,
}

/// «Утром» и «вечером» — часы из «Настройки → Календарь» (начало и конец рабочего дня).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hours {
    pub morning: Time,
    pub evening: Time,
}

impl Default for Hours {
    fn default() -> Self {
        Self { morning: Time::constant(9, 0, 0, 0), evening: Time::constant(18, 0, 0, 0) }
    }
}

/// Слово строки: текст (без знаков по краям для сравнения) и диапазон.
#[derive(Debug, Clone)]
struct Word<'a> {
    raw: &'a str,
    key: String,
    range: Range<usize>,
}

fn words(text: &str) -> Vec<Word<'_>> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in text.char_indices().chain(std::iter::once((text.len(), ' '))) {
        match (c.is_whitespace(), start) {
            (false, None) => start = Some(i),
            (true, Some(s)) => {
                let raw = &text[s..i];
                let key = folded(raw.trim_end_matches([',', '.', ';']).trim_start_matches('('));
                out.push(Word { raw, key, range: s..i });
                start = None;
            }
            _ => {}
        }
    }
    out
}

const RU_WEEKDAYS: [(&[&str], Weekday); 7] = [
    (&["пн", "пон", "понедельник", "понедельника"], Weekday::Monday),
    (&["вт", "вторник", "вторника"], Weekday::Tuesday),
    (&["ср", "среда", "среду", "среды"], Weekday::Wednesday),
    (&["чт", "четверг", "четверга"], Weekday::Thursday),
    (&["пт", "пятница", "пятницу", "пятницы"], Weekday::Friday),
    (&["сб", "суббота", "субботу", "субботы"], Weekday::Saturday),
    (&["вс", "воскресенье", "воскресенья"], Weekday::Sunday),
];
const EN_WEEKDAYS: [(&[&str], Weekday); 7] = [
    (&["mon", "monday"], Weekday::Monday),
    (&["tue", "tues", "tuesday"], Weekday::Tuesday),
    (&["wed", "wednesday"], Weekday::Wednesday),
    (&["thu", "thur", "thurs", "thursday"], Weekday::Thursday),
    (&["fri", "friday"], Weekday::Friday),
    (&["sat", "saturday"], Weekday::Saturday),
    (&["sun", "sunday"], Weekday::Sunday),
];
/// Месяцы: начала слов (родительный падеж и сокращения).
const RU_MONTHS: [&str; 12] = ["янв", "фев", "мар", "апр", "ма", "июн", "июл", "авг", "сен", "окт", "ноя", "дек"];
const EN_MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];

fn weekday(key: &str, lang: Lang) -> Option<Weekday> {
    let table: &[(&[&str], Weekday)] = match lang {
        Lang::Ru => &RU_WEEKDAYS,
        Lang::En => &EN_WEEKDAYS,
    };
    table.iter().find(|(names, _)| names.contains(&key)).map(|(_, w)| *w)
}

fn month(key: &str, lang: Lang) -> Option<i8> {
    let ru = |k: &str| {
        // «ма…» — май (мая, май), но не «март»: март — «мар».
        if k.starts_with("мар") {
            return Some(3);
        }
        if k == "мая" || k == "май" {
            return Some(5);
        }
        RU_MONTHS.iter().position(|m| *m != "ма" && k.starts_with(m)).map(|i| i as i8 + 1)
    };
    match lang {
        Lang::Ru => ru(key),
        Lang::En => EN_MONTHS.iter().position(|m| key.starts_with(m) && key.len() >= 3).map(|i| i as i8 + 1),
    }
}

/// День недели строго после `today` (сегодня пятница — «пт» следующая).
fn next_weekday(today: Date, wd: Weekday) -> Date {
    let ahead = (wd.to_monday_zero_offset() - today.weekday().to_monday_zero_offset()).rem_euclid(7);
    let days = if ahead == 0 { 7 } else { i64::from(ahead) };
    today.checked_add(days.days()).unwrap_or(today)
}

/// Дата дня и месяца ближайшая впереди (сегодня — сегодня).
fn ahead(today: Date, day: i8, month: i8, year: Option<i16>) -> Option<Date> {
    if let Some(y) = year {
        return Date::new(y, month, day).ok();
    }
    // 29.02 — в ближайший високосный год.
    (0..8).filter_map(|n| Date::new(today.year() + n, month, day).ok()).find(|d| *d >= today)
}

/// `15.11`, `15.11.2026`, `15.11.26`, `15/11` (месяц — двумя цифрами).
fn numeric_date(key: &str, today: Date) -> Option<Date> {
    let parts: Vec<&str> = key.split(['.', '/']).collect();
    if !(2..=3).contains(&parts.len()) || parts.iter().any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    // Месяц — двумя цифрами: «7.2» — пункт, а не дата.
    if parts[1].len() != 2 || parts[0].len() > 2 {
        return None;
    }
    let day: i8 = parts[0].parse().ok()?;
    let month: i8 = parts[1].parse().ok()?;
    let year = match parts.get(2) {
        None => None,
        Some(y) if y.len() == 2 => Some(2000 + y.parse::<i16>().ok()?),
        Some(y) if y.len() == 4 => Some(y.parse::<i16>().ok()?),
        Some(_) => return None,
    };
    ahead(today, day, month, year)
}

/// `15`, `15:30`, `9.30`, `3pm`, `11am`, `15ч`.
fn clock(key: &str, lang: Lang) -> Option<Time> {
    let k = key.trim_end_matches('ч').trim_end_matches('h');
    let (k, pm) = match lang {
        Lang::En if k.ends_with("pm") => (&k[..k.len() - 2], Some(true)),
        Lang::En if k.ends_with("am") => (&k[..k.len() - 2], Some(false)),
        _ => (k, None),
    };
    let (h, m) = match k.split_once([':', '.']) {
        Some((h, m)) if m.len() == 2 => (h.parse::<i8>().ok()?, m.parse::<i8>().ok()?),
        Some(_) => return None,
        None => (k.parse::<i8>().ok()?, 0),
    };
    let h = match pm {
        Some(true) if h < 12 => h + 12,
        Some(false) if h == 12 => 0,
        _ => h,
    };
    Time::new(h, m, 0, 0).ok()
}

fn is_time_shape(key: &str) -> bool {
    key.contains(':') && clock(key, Lang::Ru).is_some()
}

/// Категория по слову после `#`: имя без регистра, начало имени, набор в другой раскладке.
fn category(word: &str, table: &Categories) -> Option<String> {
    let w = folded(word);
    if w.is_empty() {
        return None;
    }
    let visible: Vec<_> = table.list.iter().filter(|c| !c.hidden).collect();
    let find = |q: &str| -> Option<String> {
        let exact = visible.iter().find(|c| folded(&c.name) == q || folded(&c.keyword) == q);
        let prefix = || visible.iter().find(|c| folded(&c.name).starts_with(q));
        exact.or_else(prefix).map(|c| c.keyword.clone())
    };
    find(&w).or_else(|| other_layout(&w).and_then(|o| find(&o)))
}

/// Разобрать фразу. `today` и `now` — местные; `ignored` — тексты чипов, с которых сняли распознавание.
pub fn parse(text: &str, lang: Lang, today: Date, now: Time, hours: Hours, table: &Categories, ignored: &[String]) -> Parsed {
    let ws = words(text);
    let mut used = vec![false; ws.len()];
    let mut out = Parsed::default();
    let skip = |w: &Word<'_>| ignored.iter().any(|x| x == w.raw);
    let mut date: Option<(Date, Range<usize>)> = None;
    let mut time: Option<(Time, Range<usize>)> = None;
    let chip = |out: &mut Parsed, kind: ChipKind, range: Range<usize>| {
        out.chips.push(Chip { kind, text: text[range.clone()].to_owned(), range });
    };
    let key = |i: usize| ws.get(i).map(|w| w.key.as_str()).unwrap_or("");
    let mut i = 0;
    while i < ws.len() {
        let w = &ws[i];
        if skip(w) {
            i += 1;
            continue;
        }
        let k = w.key.as_str();
        // «!» — важная (в любом месте строки).
        if !w.raw.is_empty() && w.raw.chars().all(|c| c == '!') {
            out.important = true;
            used[i] = true;
            chip(&mut out, ChipKind::Important, w.range.clone());
            i += 1;
            continue;
        }
        // #категория.
        if let Some(name) = w.raw.strip_prefix('#') {
            if let Some(kw) = category(name.trim_end_matches([',', '.']), table) {
                out.category = Some(kw);
                used[i] = true;
                chip(&mut out, ChipKind::Category, w.range.clone());
            }
            i += 1;
            continue;
        }
        // Сколько слов съел срок: (дата, время, слов).
        let mut found: Option<(Option<Date>, Option<Time>, usize)> = None;
        let next = key(i + 1);
        let next2 = key(i + 2);
        match lang {
            Lang::Ru => {
                let pre = matches!(k, "в" | "во" | "к" | "на" | "до");
                match k {
                    "сегодня" => found = Some((Some(today), None, 1)),
                    "завтра" => found = Some((today.checked_add(1.days()).ok(), None, 1)),
                    "послезавтра" => found = Some((today.checked_add(2.days()).ok(), None, 1)),
                    "утром" => found = Some((None, Some(hours.morning), 1)),
                    "вечером" => found = Some((None, Some(hours.evening), 1)),
                    "через" => {
                        let n: Option<i64> = next.parse().ok();
                        let unit = if n.is_some() { next2 } else { next };
                        let n = n.unwrap_or(1);
                        let words = if next.parse::<i64>().is_ok() { 3 } else { 2 };
                        let d = if unit.starts_with("ден") || unit.starts_with("дн") || unit == "день" {
                            today.checked_add(n.days()).ok()
                        } else if unit.starts_with("недел") {
                            today.checked_add((7 * n).days()).ok()
                        } else if unit.starts_with("месяц") {
                            today.checked_add(n.months()).ok()
                        } else {
                            None
                        };
                        if d.is_some() {
                            found = Some((d, None, words));
                        }
                    }
                    "на" if next == "следующей" && next2.starts_with("недел") => {
                        found = Some((Some(next_weekday(today, Weekday::Monday)), None, 3));
                    }
                    _ if pre => {
                        if let Some(wd) = weekday(next, lang) {
                            found = Some((Some(next_weekday(today, wd)), None, 2));
                        } else if let Some(t) = clock(next, lang).filter(|_| k != "на") {
                            found = Some((None, Some(t), 2));
                        } else if let Some(d) = numeric_date(next, today) {
                            found = Some((Some(d), None, 2));
                        }
                    }
                    _ => {
                        if let Some(wd) = weekday(k, lang) {
                            found = Some((Some(next_weekday(today, wd)), None, 1));
                        } else if let Some(d) = numeric_date(k, today) {
                            found = Some((Some(d), None, 1));
                        } else if is_time_shape(k) {
                            found = Some((None, clock(k, lang), 1));
                        } else if let (Ok(day), Some(m)) = (k.parse::<i8>(), month(next, lang)) {
                            found = Some((ahead(today, day, m, None), None, 2));
                        }
                    }
                }
            }
            Lang::En => match k {
                "today" | "tonight" => {
                    let t = (k == "tonight").then_some(hours.evening);
                    found = Some((Some(today), t, 1));
                }
                "tomorrow" => found = Some((today.checked_add(1.days()).ok(), None, 1)),
                "morning" => found = Some((None, Some(hours.morning), 1)),
                "evening" => found = Some((None, Some(hours.evening), 1)),
                "day" if next == "after" && next2 == "tomorrow" => found = Some((today.checked_add(2.days()).ok(), None, 3)),
                "in" => {
                    let n: Option<i64> = if matches!(next, "a" | "an") { Some(1) } else { next.parse().ok() };
                    let unit = if n.is_some() { next2 } else { next };
                    let words = if n.is_some() { 3 } else { 2 };
                    let n = n.unwrap_or(1);
                    let d = match unit {
                        u if u.starts_with("day") => today.checked_add(n.days()).ok(),
                        u if u.starts_with("week") => today.checked_add((7 * n).days()).ok(),
                        u if u.starts_with("month") => today.checked_add(n.months()).ok(),
                        _ => None,
                    };
                    if d.is_some() {
                        found = Some((d, None, words));
                    }
                }
                "next" if next == "week" => found = Some((Some(next_weekday(today, Weekday::Monday)), None, 2)),
                "next" => {
                    if let Some(wd) = weekday(next, lang) {
                        found = Some((Some(next_weekday(today, wd)), None, 2));
                    }
                }
                "on" | "at" | "by" => {
                    if let Some(wd) = weekday(next, lang) {
                        found = Some((Some(next_weekday(today, wd)), None, 2));
                    } else if let Some(t) = clock(next, lang).filter(|_| k != "on") {
                        found = Some((None, Some(t), 2));
                    } else if let Some(d) = numeric_date(next, today) {
                        found = Some((Some(d), None, 2));
                    } else if let (Some(m), Ok(day)) = (month(next, lang), next2.parse::<i8>()) {
                        found = Some((ahead(today, day, m, None), None, 3));
                    }
                }
                _ => {
                    if let Some(wd) = weekday(k, lang) {
                        found = Some((Some(next_weekday(today, wd)), None, 1));
                    } else if let Some(d) = numeric_date(k, today) {
                        found = Some((Some(d), None, 1));
                    } else if is_time_shape(k) || ((k.ends_with("am") || k.ends_with("pm")) && clock(k, lang).is_some()) {
                        found = Some((None, clock(k, lang), 1));
                    } else if let (Some(m), Ok(day)) = (month(k, lang), next.parse::<i8>()) {
                        found = Some((ahead(today, day, m, None), None, 2));
                    } else if let (Ok(day), Some(m)) = (k.parse::<i8>(), month(next, lang)) {
                        found = Some((ahead(today, day, m, None), None, 2));
                    }
                }
            },
        }
        match found {
            Some((d, t, n)) if (d.is_some() || t.is_some()) && (i..i + n).all(|j| j < ws.len() && !skip(&ws[j])) => {
                let range = ws[i].range.start..ws[i + n - 1].range.end;
                // Первая дата и первое время побеждают; повтор — в название.
                let take_date = d.is_some() && date.is_none();
                let take_time = t.is_some() && time.is_none();
                if take_date || take_time {
                    if let Some(d) = d.filter(|_| take_date) {
                        date = Some((d, range.clone()));
                    }
                    if let Some(t) = t.filter(|_| take_time) {
                        time = Some((t, range.clone()));
                    }
                    for u in used.iter_mut().skip(i).take(n) {
                        *u = true;
                    }
                    i += n;
                    continue;
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    // Срок — один чип от первого до последнего распознанного куска даты и времени, если они рядом, иначе два.
    let mut due_ranges: Vec<Range<usize>> = [date.as_ref().map(|d| d.1.clone()), time.as_ref().map(|t| t.1.clone())].into_iter().flatten().collect();
    due_ranges.sort_by_key(|r| r.start);
    due_ranges.dedup();
    for r in due_ranges {
        chip(&mut out, ChipKind::Due, r);
    }
    out.chips.sort_by_key(|c| c.range.start);
    out.date = date.map(|d| d.0);
    out.time = time.map(|t| t.0);
    // Время без даты: сегодня, а если уже прошло — завтра.
    if out.date.is_none()
        && let Some(t) = out.time
    {
        out.date = if t > now { Some(today) } else { today.checked_add(1.days()).ok() };
    }
    out.title = ws.iter().zip(&used).filter(|(_, u)| !**u).map(|(w, _)| w.raw).collect::<Vec<_>>().join(" ");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    /// «Сегодня» таблицы — среда, 21 октября 2026, 14:05.
    fn today() -> Date {
        date(2026, 10, 21)
    }

    fn now() -> Time {
        Time::constant(14, 5, 0, 0)
    }

    fn table() -> Categories {
        Categories::defaults(|id| match id {
            "clients" => "Клиенты".into(),
            "team" => "Команда".into(),
            "tenders" => "Тендеры".into(),
            "pentest" => "Пентест".into(),
            "services" => "Сервисы".into(),
            other => other.into(),
        })
    }

    type Row = (&'static str, Lang, &'static str, Option<Date>, Option<(i8, i8)>, Option<&'static str>, bool);

    /// Таблица случаев: фраза, язык → название, дата, время, категория, «!».
    fn cases() -> Vec<Row> {
        use Lang::{En, Ru};
        let d = |m: i8, day: i8| Some(date(2026, m, day));
        let y = |yr: i16, m: i8, day: i8| Some(date(yr, m, day));
        vec![
            ("Позвонить Беловой", Ru, "Позвонить Беловой", None, None, None, false),
            ("Позвонить Беловой сегодня", Ru, "Позвонить Беловой", d(10, 21), None, None, false),
            ("Позвонить завтра", Ru, "Позвонить", d(10, 22), None, None, false),
            ("Позвонить послезавтра", Ru, "Позвонить", d(10, 23), None, None, false),
            ("Позвонить в пт", Ru, "Позвонить", d(10, 23), None, None, false),
            ("Позвонить пт", Ru, "Позвонить", d(10, 23), None, None, false),
            ("Позвонить в пятницу", Ru, "Позвонить", d(10, 23), None, None, false),
            ("Позвонить в среду", Ru, "Позвонить", d(10, 28), None, None, false),
            ("Позвонить в ср", Ru, "Позвонить", d(10, 28), None, None, false),
            ("Позвонить во вторник", Ru, "Позвонить", d(10, 27), None, None, false),
            ("Позвонить в понедельник", Ru, "Позвонить", d(10, 26), None, None, false),
            ("Позвонить в вс", Ru, "Позвонить", d(10, 25), None, None, false),
            ("Позвонить через 3 дня", Ru, "Позвонить", d(10, 24), None, None, false),
            ("Позвонить через 1 день", Ru, "Позвонить", d(10, 22), None, None, false),
            ("Позвонить через 12 дней", Ru, "Позвонить", d(11, 2), None, None, false),
            ("Позвонить через неделю", Ru, "Позвонить", d(10, 28), None, None, false),
            ("Позвонить через 2 недели", Ru, "Позвонить", d(11, 4), None, None, false),
            ("Позвонить через месяц", Ru, "Позвонить", d(11, 21), None, None, false),
            ("Позвонить на следующей неделе", Ru, "Позвонить", d(10, 26), None, None, false),
            ("Сдать отчёт 15.11", Ru, "Сдать отчёт", d(11, 15), None, None, false),
            ("Сдать отчёт 15.11.2027", Ru, "Сдать отчёт", y(2027, 11, 15), None, None, false),
            ("Сдать отчёт 15.11.27", Ru, "Сдать отчёт", y(2027, 11, 15), None, None, false),
            ("Сдать отчёт 15/11", Ru, "Сдать отчёт", d(11, 15), None, None, false),
            ("Сдать отчёт 01.03", Ru, "Сдать отчёт", y(2027, 3, 1), None, None, false),
            ("Сдать отчёт 21.10", Ru, "Сдать отчёт", d(10, 21), None, None, false),
            ("Сдать отчёт 20.10", Ru, "Сдать отчёт", y(2027, 10, 20), None, None, false),
            ("Сдать отчёт 31.12", Ru, "Сдать отчёт", d(12, 31), None, None, false),
            ("Сдать отчёт 29.02", Ru, "Сдать отчёт", y(2028, 2, 29), None, None, false),
            ("Сдать отчёт 15 ноября", Ru, "Сдать отчёт", d(11, 15), None, None, false),
            ("Сдать отчёт 1 января", Ru, "Сдать отчёт", y(2027, 1, 1), None, None, false),
            ("Сдать отчёт 3 марта", Ru, "Сдать отчёт", y(2027, 3, 3), None, None, false),
            ("Сдать отчёт 9 мая", Ru, "Сдать отчёт", y(2027, 5, 9), None, None, false),
            ("Сдать отчёт 30 окт", Ru, "Сдать отчёт", d(10, 30), None, None, false),
            ("Созвон в 15", Ru, "Созвон", d(10, 21), Some((15, 0)), None, false),
            ("Созвон в 15:30", Ru, "Созвон", d(10, 21), Some((15, 30)), None, false),
            ("Созвон в 9", Ru, "Созвон", d(10, 22), Some((9, 0)), None, false),
            ("Созвон 16:45", Ru, "Созвон", d(10, 21), Some((16, 45)), None, false),
            ("Созвон к 17", Ru, "Созвон", d(10, 21), Some((17, 0)), None, false),
            ("Созвон утром", Ru, "Созвон", d(10, 22), Some((9, 0)), None, false),
            ("Созвон вечером", Ru, "Созвон", d(10, 21), Some((18, 0)), None, false),
            ("Созвон завтра утром", Ru, "Созвон", d(10, 22), Some((9, 0)), None, false),
            ("Созвон завтра в 10", Ru, "Созвон", d(10, 22), Some((10, 0)), None, false),
            ("Позвонить Беловой пт 11:00 #Команда", Ru, "Позвонить Беловой", d(10, 23), Some((11, 0)), Some("pm.cat.team"), false),
            ("Отправить КП #Клиенты", Ru, "Отправить КП", None, None, Some("pm.cat.clients"), false),
            ("Отправить КП #клиенты", Ru, "Отправить КП", None, None, Some("pm.cat.clients"), false),
            ("Отправить КП #клиен", Ru, "Отправить КП", None, None, Some("pm.cat.clients"), false),
            ("Отправить КП #rkbtyns", Ru, "Отправить КП", None, None, Some("pm.cat.clients"), false),
            ("Отправить КП #Неизвестно", Ru, "Отправить КП #Неизвестно", None, None, None, false),
            ("Отправить ! КП", Ru, "Отправить КП", None, None, None, true),
            ("! Отправить КП", Ru, "Отправить КП", None, None, None, true),
            ("Отправить КП !", Ru, "Отправить КП", None, None, None, true),
            ("Отправить КП Вектору завтра 10:00 #Клиенты !", Ru, "Отправить КП Вектору", d(10, 22), Some((10, 0)), Some("pm.cat.clients"), true),
            ("Купить 3 ноутбука", Ru, "Купить 3 ноутбука", None, None, None, false),
            ("Отчёт за 2026 год", Ru, "Отчёт за 2026 год", None, None, None, false),
            ("Глава 7.2 договора", Ru, "Глава 7.2 договора", None, None, None, false),
            ("Сдать до пт", Ru, "Сдать", d(10, 23), None, None, false),
            ("Сдать на 15.11", Ru, "Сдать", d(11, 15), None, None, false),
            ("Завтра завтра", Ru, "завтра", d(10, 22), None, None, false),
            ("Сегодня в 13", Ru, "", d(10, 21), Some((13, 0)), None, false),
            ("Call Belova", En, "Call Belova", None, None, None, false),
            ("Call Belova today", En, "Call Belova", d(10, 21), None, None, false),
            ("Call tomorrow", En, "Call", d(10, 22), None, None, false),
            ("Call day after tomorrow", En, "Call", d(10, 23), None, None, false),
            ("Call on fri", En, "Call", d(10, 23), None, None, false),
            ("Call friday", En, "Call", d(10, 23), None, None, false),
            ("Call next monday", En, "Call", d(10, 26), None, None, false),
            ("Call in 3 days", En, "Call", d(10, 24), None, None, false),
            ("Call in a week", En, "Call", d(10, 28), None, None, false),
            ("Call in 2 weeks", En, "Call", d(11, 4), None, None, false),
            ("Call next week", En, "Call", d(10, 26), None, None, false),
            ("Report Nov 15", En, "Report", d(11, 15), None, None, false),
            ("Report 15 Nov", En, "Report", d(11, 15), None, None, false),
            ("Report on Nov 15", En, "Report", d(11, 15), None, None, false),
            ("Report 15.11", En, "Report", d(11, 15), None, None, false),
            ("Sync at 3pm", En, "Sync", d(10, 21), Some((15, 0)), None, false),
            ("Sync at 15:30", En, "Sync", d(10, 21), Some((15, 30)), None, false),
            ("Sync 11am", En, "Sync", d(10, 22), Some((11, 0)), None, false),
            ("Sync tomorrow morning", En, "Sync", d(10, 22), Some((9, 0)), None, false),
            ("Sync tonight", En, "Sync", d(10, 21), Some((18, 0)), None, false),
            ("Send proposal #clients !", En, "Send proposal", None, None, Some("pm.cat.clients"), true),
            ("Send proposal #сдшутеы", En, "Send proposal", None, None, Some("pm.cat.clients"), false),
        ]
    }

    #[test]
    fn phrase_table() {
        let rows = cases();
        assert!(rows.len() >= 60, "{} случаев", rows.len());
        let t = table();
        for (text, lang, title, d, time, cat, imp) in rows {
            // Английские категории — по английским именам таблицы.
            let table = if lang == Lang::En { Categories::defaults(|id| id.to_owned()) } else { t.clone() };
            let p = parse(text, lang, today(), now(), Hours::default(), &table, &[]);
            let want_time = time.map(|(h, m)| Time::constant(h, m, 0, 0));
            assert_eq!((p.title.as_str(), p.date, p.time, p.category.as_deref(), p.important), (title, d, want_time, cat, imp), "«{text}»");
        }
    }

    #[test]
    fn chips_ranges_and_ignored() {
        let text = "Позвонить Беловой пт 11:00 #Команда !";
        let p = parse(text, Lang::Ru, today(), now(), Hours::default(), &table(), &[]);
        let kinds: Vec<(ChipKind, &str)> = p.chips.iter().map(|c| (c.kind, c.text.as_str())).collect();
        assert_eq!(kinds, [(ChipKind::Due, "пт"), (ChipKind::Due, "11:00"), (ChipKind::Category, "#Команда"), (ChipKind::Important, "!")]);
        for c in &p.chips {
            assert_eq!(&text[c.range.clone()], c.text);
        }
        // Крестик на «пт» — это снова часть названия; время остаётся сегодня.
        let p = parse(text, Lang::Ru, today(), now(), Hours::default(), &table(), &["пт".into()]);
        // Время без даты уже прошло (11:00 < 14:05) — завтра.
        assert_eq!((p.title.as_str(), p.date), ("Позвонить Беловой пт", Some(date(2026, 10, 22))));
        // «Утром» и «вечером» — часы из настроек.
        let hours = Hours { morning: Time::constant(8, 30, 0, 0), evening: Time::constant(19, 0, 0, 0) };
        let p = parse("Созвон завтра вечером", Lang::Ru, today(), now(), hours, &table(), &[]);
        assert_eq!(p.time, Some(Time::constant(19, 0, 0, 0)));
    }

    /// В пятницу «пт» — следующая; конец года — в новом году.
    #[test]
    fn nearest_ahead() {
        let fri = date(2026, 10, 23);
        let p = parse("Отчёт пт", Lang::Ru, fri, now(), Hours::default(), &table(), &[]);
        assert_eq!(p.date, Some(date(2026, 10, 30)));
        let p = parse("Отчёт через 3 дня", Lang::Ru, date(2026, 12, 30), now(), Hours::default(), &table(), &[]);
        assert_eq!(p.date, Some(date(2027, 1, 2)));
        let p = parse("Отчёт 5 января", Lang::Ru, date(2026, 12, 30), now(), Hours::default(), &table(), &[]);
        assert_eq!(p.date, Some(date(2027, 1, 5)));
        let p = parse("Отчёт через месяц", Lang::Ru, date(2027, 1, 31), now(), Hours::default(), &table(), &[]);
        assert_eq!(p.date, Some(date(2027, 2, 28)), "конец месяца");
    }
}
