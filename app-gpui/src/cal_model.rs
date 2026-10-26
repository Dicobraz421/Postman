//! Модель видов календаря (S5-21, DESIGN «Календарь и контакты»): какие дни показывает вид «День / Раб. неделя /
//! Неделя / Месяц», листание периода, раскладка месяца (многодневные полосами по неделям, остальное — строками),
//! поиск по загруженным событиям и счётчик ответов участников. Без gpui: всё проверяют тесты.
//!
//! Первый день недели и рабочие дни — «Настройки → Календарь» (S5-20, `Prefs::calendar`).

use jiff::civil::{Date, Weekday};
use jiff::tz::TimeZone;
use postman_calendar::{Event, Participant, Partstat};
use postman_core::prefs::{CalendarView, WeekStart};

/// Недель в месяце — не больше (месяц с понедельника 31-го числа на 6 строк).
pub(crate) const MAX_WEEKS: usize = 6;

pub(crate) fn add_days(date: Date, n: i64) -> Date {
    date.checked_add(jiff::Span::new().days(n)).unwrap_or(date)
}

/// Первый день недели, в которую попадает `date`.
pub(crate) fn week_first(date: Date, ws: WeekStart) -> Date {
    let back = match ws {
        WeekStart::Monday => date.weekday().to_monday_zero_offset(),
        WeekStart::Sunday => date.weekday().to_sunday_zero_offset(),
    };
    add_days(date, -i64::from(back))
}

/// Первые дни строк сетки месяца `month` (любое число месяца): от недели с 1-м числом до недели с последним.
pub(crate) fn month_weeks(month: Date, ws: WeekStart) -> Vec<Date> {
    let (first, last) = (month.first_of_month(), month.last_of_month());
    let mut out = Vec::new();
    let mut w = week_first(first, ws);
    while w <= last && out.len() < MAX_WEEKS {
        out.push(w);
        w = add_days(w, 7);
    }
    out
}

/// Дни вида с опорной датой `anchor`, по порядку. Рабочая неделя — рабочие дни недели `anchor` (нет рабочих дней —
/// вся неделя); месяц — все дни строк сетки месяца.
pub(crate) fn days(view: CalendarView, anchor: Date, ws: WeekStart, work: &[Weekday]) -> Vec<Date> {
    match view {
        CalendarView::Day => vec![anchor],
        CalendarView::Week => week(anchor, ws),
        CalendarView::WorkWeek => {
            let all = week(anchor, ws);
            let shown: Vec<Date> = all.iter().copied().filter(|d| work.contains(&d.weekday())).collect();
            if shown.is_empty() { all } else { shown }
        }
        CalendarView::Month => month_weeks(anchor, ws).into_iter().flat_map(|w| (0..7).map(move |i| add_days(w, i))).collect(),
    }
}

fn week(anchor: Date, ws: WeekStart) -> Vec<Date> {
    let first = week_first(anchor, ws);
    (0..7).map(|i| add_days(first, i)).collect()
}

/// Опорная дата на `n` периодов вперёд (назад — `n < 0`): день, неделя или месяц. Месяц — то же число, у короткого
/// месяца — последнее.
pub(crate) fn shift(view: CalendarView, anchor: Date, n: i64) -> Date {
    match view {
        CalendarView::Day => add_days(anchor, n),
        CalendarView::WorkWeek | CalendarView::Week => add_days(anchor, n * 7),
        CalendarView::Month => anchor.checked_add(jiff::Span::new().months(n)).unwrap_or(anchor),
    }
}

/// Начало дня в зоне `tz` (Unix-секунды).
pub(crate) fn day_start(date: Date, tz: &TimeZone) -> i64 {
    date.to_zoned(tz.clone()).map(|z| z.timestamp().as_second()).unwrap_or(0)
}

/// Что загрузить с сервера для дней вида: `[начало первого дня, начало дня после последнего)`.
pub(crate) fn range(days: &[Date], tz: &TimeZone) -> (i64, i64) {
    match (days.first(), days.last()) {
        (Some(&a), Some(&b)) => (day_start(a, tz), day_start(add_days(b, 1), tz)),
        _ => (0, 0),
    }
}

/// Даты события в зоне `tz`: день начала и последний день (конец не включается: до 00:00 — предыдущий день).
pub(crate) fn dates(e: &Event, tz: &TimeZone) -> (Date, Date) {
    let date = |t: i64| jiff::Timestamp::from_second(t).map(|t| t.to_zoned(tz.clone()).date()).ok();
    let first = date(e.span.start).unwrap_or(Date::ZERO);
    let last = date((e.span.end - 1).max(e.span.start)).unwrap_or(first);
    (first, last.max(first))
}

/// Столбцы `days` (по порядку), которые занимает событие с днями `[first, last]`: первый и последний показанный;
/// ни одного — `None` (рабочая неделя без выходных: событие на выходные не видно).
pub(crate) fn columns(days: &[Date], first: Date, last: Date) -> Option<(usize, usize)> {
    let inside: Vec<usize> = days.iter().enumerate().filter(|(_, d)| **d >= first && **d <= last).map(|(i, _)| i).collect();
    Some((*inside.first()?, *inside.last()?))
}

/// Многодневное: на весь день или идёт больше одного дня — полосой (весь день, месяц).
pub(crate) fn spans_days(e: &Event, tz: &TimeZone) -> bool {
    let (a, b) = dates(e, tz);
    e.all_day || a != b
}

/// Элемент месяца: событие `index` в строке `week`, столбцы `col0..=col1`, дорожка `lane` (0 — верхняя).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MonthItem {
    pub(crate) index: usize,
    pub(crate) week: usize,
    pub(crate) col0: usize,
    pub(crate) col1: usize,
    pub(crate) lane: usize,
    /// Полоса (многодневное) или строка «время · тема».
    pub(crate) bar: bool,
}

/// Раскладка месяца со строками с `weeks` (первые дни недель): в каждой неделе сначала многодневные (раньше
/// начало, длиннее — выше), затем события одного дня по времени; дорожка — первая свободная во всех её столбцах.
/// Событие через границу недели — полоса в каждой неделе. `shown` — какие события видны (отклонённые — нет).
pub(crate) fn month_layout(events: &[Event], shown: impl Fn(usize) -> bool, weeks: &[Date], tz: &TimeZone) -> Vec<MonthItem> {
    let mut out = Vec::new();
    for (w, &first) in weeks.iter().enumerate() {
        let row: Vec<Date> = (0..7).map(|i| add_days(first, i)).collect();
        let mut items: Vec<(usize, usize, usize, bool, i64, i64)> = events
            .iter()
            .enumerate()
            .filter(|(i, _)| shown(*i))
            .filter_map(|(i, e)| {
                let (a, b) = dates(e, tz);
                let (c0, c1) = columns(&row, a, b)?;
                Some((i, c0, c1, spans_days(e, tz), e.span.start, e.span.len()))
            })
            .collect();
        // Полосы — первыми; среди них раньше и длиннее — выше; затем по времени.
        items.sort_by_key(|&(i, c0, c1, bar, start, len)| (!bar, c0, std::cmp::Reverse(c1), start, std::cmp::Reverse(len), i));
        let mut lanes: Vec<[bool; 7]> = Vec::new();
        for (index, col0, col1, bar, _, _) in items {
            let lane = (0..).find(|&k| lanes.get(k).is_none_or(|l: &[bool; 7]| (col0..=col1).all(|c| !l[c]))).unwrap_or(0);
            if lanes.len() <= lane {
                lanes.resize(lane + 1, [false; 7]);
            }
            lanes[lane][col0..=col1].fill(true);
            out.push(MonthItem { index, week: w, col0, col1, lane, bar });
        }
    }
    out
}

/// Скрыто в клетке (неделя `week`, столбец `col`) при `lanes` видимых дорожках — «+N ещё».
pub(crate) fn hidden_in(items: &[MonthItem], week: usize, col: usize, lanes: usize) -> usize {
    items.iter().filter(|m| m.week == week && m.col0 <= col && col <= m.col1 && m.lane >= lanes).count()
}

/// Ответы участников: организатор — «принял».
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    pub(crate) accepted: usize,
    pub(crate) tentative: usize,
    pub(crate) declined: usize,
    pub(crate) none: usize,
}

/// Счётчик над участниками (легенда ✓ N · ? N · ✕ N · без ответа N): участники и организатор, без прочих ролей.
pub(crate) fn counts<'a>(people: impl IntoIterator<Item = &'a Participant>) -> Counts {
    let mut c = Counts::default();
    for p in people.into_iter().filter(|p| p.attendee || p.owner) {
        match p.status {
            _ if p.owner => c.accepted += 1,
            Partstat::Accepted | Partstat::Delegated => c.accepted += 1,
            Partstat::Tentative => c.tentative += 1,
            Partstat::Declined => c.declined += 1,
            Partstat::NeedsAction => c.none += 1,
        }
    }
    c
}

fn fold(s: &str) -> String {
    s.to_lowercase().replace('ё', "е") // i18n-ok: поиск без учёта «ё»
}

/// Событие подходит под запрос: каждое слово — в теме, месте, описании или у участника (имя, адрес); регистр и «ё»
/// не важны. Пустой запрос — ничего не ищем.
pub(crate) fn matches(e: &Event, query: &str) -> bool {
    let words: Vec<String> = query.split_whitespace().map(fold).collect();
    if words.is_empty() {
        return false;
    }
    let mut hay = format!("{} {} {}", e.title, e.location, e.description);
    for p in &e.participants {
        hay.push(' ');
        hay.push_str(&p.name);
        hay.push(' ');
        hay.push_str(&p.email);
    }
    let hay = fold(&hay);
    words.iter().all(|w| hay.contains(w.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    fn ev(title: &str, start: &str, end: &str, all_day: bool) -> Event {
        let v = serde_json::json!({ "id": title, "title": title, "utcStart": start, "utcEnd": end, "showWithoutTime": all_day });
        Event::from_jmap(&v, &TimeZone::UTC).unwrap()
    }

    const WORK: [Weekday; 5] = [Weekday::Monday, Weekday::Tuesday, Weekday::Wednesday, Weekday::Thursday, Weekday::Friday];

    /// Дни видов: день, рабочая неделя по рабочим дням, неделя с понедельника или воскресенья, месяц — целыми
    /// неделями через границы месяца.
    #[test]
    fn view_days_and_week_start() {
        let wed = date(2026, 10, 21);
        assert_eq!(days(CalendarView::Day, wed, WeekStart::Monday, &WORK), [wed]);
        let ww = days(CalendarView::WorkWeek, wed, WeekStart::Monday, &WORK);
        assert_eq!((ww.len(), ww[0], ww[4]), (5, date(2026, 10, 19), date(2026, 10, 23)));
        let w = days(CalendarView::Week, wed, WeekStart::Monday, &WORK);
        assert_eq!((w[0], w[6]), (date(2026, 10, 19), date(2026, 10, 25)));
        let ws = days(CalendarView::Week, wed, WeekStart::Sunday, &WORK);
        assert_eq!((ws[0], ws[6]), (date(2026, 10, 18), date(2026, 10, 24)));
        // Неделя с воскресенья: рабочая неделя — те же Пн–Пт, без воскресенья.
        let wws = days(CalendarView::WorkWeek, wed, WeekStart::Sunday, &WORK);
        assert_eq!((wws.len(), wws[0]), (5, date(2026, 10, 19)));
        // Рабочие дни со вторника по субботу — по порядку недели.
        let tue_sat = [Weekday::Tuesday, Weekday::Wednesday, Weekday::Thursday, Weekday::Friday, Weekday::Saturday];
        let custom = days(CalendarView::WorkWeek, wed, WeekStart::Monday, &tue_sat);
        assert_eq!((custom[0], custom[4]), (date(2026, 10, 20), date(2026, 10, 24)));
        assert_eq!(days(CalendarView::WorkWeek, wed, WeekStart::Monday, &[]).len(), 7, "нет рабочих дней — вся неделя");
        // Октябрь 2026: 1-е — четверг; с понедельника — 5 строк (28 сен … 1 ноя).
        let m = days(CalendarView::Month, wed, WeekStart::Monday, &WORK);
        assert_eq!((m.len(), m[0], *m.last().unwrap()), (35, date(2026, 9, 28), date(2026, 11, 1)));
        // Август 2026 с понедельника: 1-е — суббота, 31-е — понедельник: 6 строк.
        assert_eq!(month_weeks(date(2026, 8, 15), WeekStart::Monday).len(), 6);
        // Февраль 2027 с понедельника: 1-е — понедельник, 28 дней — ровно 4 строки.
        assert_eq!(month_weeks(date(2027, 2, 10), WeekStart::Monday), [date(2027, 2, 1), date(2027, 2, 8), date(2027, 2, 15), date(2027, 2, 22)]);
        // С воскресенья: 1 ноября 2026 — воскресенье, первая строка начинается с него.
        assert_eq!(month_weeks(date(2026, 11, 20), WeekStart::Sunday)[0], date(2026, 11, 1));
    }

    #[test]
    fn periods_shift_by_view() {
        let d = date(2026, 1, 31);
        assert_eq!(shift(CalendarView::Day, d, 1), date(2026, 2, 1));
        assert_eq!(shift(CalendarView::Week, d, -1), date(2026, 1, 24));
        assert_eq!(shift(CalendarView::WorkWeek, d, 2), date(2026, 2, 14));
        assert_eq!(shift(CalendarView::Month, d, 1), date(2026, 2, 28), "короткий месяц — последнее число");
        assert_eq!(shift(CalendarView::Month, d, -2), date(2025, 11, 30));
        let tz = TimeZone::get("Europe/Moscow").unwrap();
        let w = days(CalendarView::Week, d, WeekStart::Monday, &WORK);
        let (from, to) = range(&w, &tz);
        assert_eq!(to - from, 7 * 86_400);
        assert_eq!(range(&[], &tz), (0, 0));
    }

    /// День: событие через полночь — в днях начала и конца; конец ровно в 00:00 следующий день не занимает.
    #[test]
    fn day_dates_and_columns() {
        let tz = TimeZone::UTC;
        let night = ev("ночь", "2026-10-21T22:00:00Z", "2026-10-22T02:00:00Z", false);
        assert_eq!(dates(&night, &tz), (date(2026, 10, 21), date(2026, 10, 22)));
        assert!(spans_days(&night, &tz));
        let to_midnight = ev("до полуночи", "2026-10-21T22:00:00Z", "2026-10-22T00:00:00Z", false);
        assert_eq!(dates(&to_midnight, &tz), (date(2026, 10, 21), date(2026, 10, 21)));
        assert!(!spans_days(&to_midnight, &tz));
        let all = ev("весь день", "2026-10-22T00:00:00Z", "2026-10-24T00:00:00Z", true);
        assert_eq!(dates(&all, &tz), (date(2026, 10, 22), date(2026, 10, 23)));
        // Рабочая неделя: событие Чт–Вс занимает Чт–Пт; только выходные — не видно.
        let ww = days(CalendarView::WorkWeek, date(2026, 10, 21), WeekStart::Monday, &WORK);
        assert_eq!(columns(&ww, date(2026, 10, 22), date(2026, 10, 25)), Some((3, 4)));
        assert_eq!(columns(&ww, date(2026, 10, 24), date(2026, 10, 25)), None);
        let day = days(CalendarView::Day, date(2026, 10, 23), WeekStart::Monday, &WORK);
        assert_eq!(columns(&day, date(2026, 10, 22), date(2026, 10, 25)), Some((0, 0)));
    }

    /// Месяц: многодневные — полосами сверху и через границу недели (и месяца); события дня — под ними по времени;
    /// «+N ещё» — то, что не вошло в видимые дорожки.
    #[test]
    fn month_layout_multi_day_and_borders() {
        let tz = TimeZone::UTC;
        let events = vec![
            // Сб 31 октября — Вт 3 ноября: через границу недели и месяца.
            ev("выезд", "2026-10-31T00:00:00Z", "2026-11-04T00:00:00Z", true),
            ev("утро", "2026-10-26T09:00:00Z", "2026-10-26T10:00:00Z", false),
            ev("обед", "2026-10-26T13:00:00Z", "2026-10-26T14:00:00Z", false),
            ev("вечер", "2026-10-26T18:00:00Z", "2026-10-26T19:00:00Z", false),
            // Пн 26 — Ср 28: полоса выше событий понедельника.
            ev("семинар", "2026-10-26T00:00:00Z", "2026-10-29T00:00:00Z", true),
            ev("отклонено", "2026-10-27T09:00:00Z", "2026-10-27T10:00:00Z", false),
        ];
        let weeks = month_weeks(date(2026, 10, 1), WeekStart::Monday);
        assert_eq!(weeks.len(), 5);
        let items = month_layout(&events, |i| i != 5, &weeks, &tz);
        let of = |i: usize| items.iter().filter(|m| m.index == i).copied().collect::<Vec<_>>();
        // «Выезд» в строке 26 окт — 1 ноя: Сб–Вс (5–6); следующей недели в сетке октября нет.
        assert_eq!(of(0), [MonthItem { index: 0, week: 4, col0: 5, col1: 6, lane: 0, bar: true }]);
        let sem = of(4);
        assert_eq!(sem, [MonthItem { index: 4, week: 4, col0: 0, col1: 2, lane: 0, bar: true }]);
        let lanes: Vec<usize> = [1, 2, 3].iter().map(|&i| of(i)[0].lane).collect();
        assert_eq!(lanes, [1, 2, 3], "события дня — под полосой, по времени");
        assert!(of(5).is_empty(), "отклонённое не показывается");
        assert_eq!(hidden_in(&items, 4, 0, 2), 2, "видно две дорожки: обед и вечер — «+2»");
        assert_eq!(hidden_in(&items, 4, 1, 2), 0);
        // Ноябрь: «выезд» начинается в предыдущей строке и продолжается в первой строке ноября (Пн–Вт).
        let nov = month_weeks(date(2026, 11, 1), WeekStart::Monday);
        let items = month_layout(&events, |_| true, &nov, &tz);
        let trip: Vec<_> = items.iter().filter(|m| m.index == 0).map(|m| (m.week, m.col0, m.col1)).collect();
        assert_eq!(trip, [(0, 5, 6), (1, 0, 1)]);
    }

    #[test]
    fn participant_counts() {
        let p = |owner: bool, attendee: bool, status: Partstat| Participant {
            name: String::new(),
            email: String::new(),
            owner,
            attendee,
            optional: false,
            status,
        };
        let people = [
            p(true, true, Partstat::NeedsAction),
            p(false, true, Partstat::Accepted),
            p(false, true, Partstat::Tentative),
            p(false, true, Partstat::Declined),
            p(false, true, Partstat::NeedsAction),
            p(false, true, Partstat::NeedsAction),
            p(false, false, Partstat::Accepted),
        ];
        assert_eq!(counts(&people), Counts { accepted: 2, tentative: 1, declined: 1, none: 2 });
        assert_eq!(counts(&[]), Counts::default());
    }

    #[test]
    fn search_matches_words_everywhere() {
        let mut e = ev("Пресейл: SIEM", "2026-10-21T09:00:00Z", "2026-10-21T10:00:00Z", false);
        e.location = "Переговорная 3".into();
        e.participants.push(Participant {
            name: "Ёлкина Мария".into(),
            email: "m.elkina@corp.example".into(),
            owner: false,
            attendee: true,
            optional: false,
            status: Partstat::Accepted,
        });
        assert!(matches(&e, "siem"));
        assert!(matches(&e, "пресейл переговорная"));
        assert!(matches(&e, "елкина"), "ё = е");
        assert!(matches(&e, "corp.example"));
        assert!(!matches(&e, "siem бюджет"), "все слова запроса");
        assert!(!matches(&e, "  "));
    }
}
