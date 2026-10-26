//! Занятость участников и поиск общего свободного времени — ядро «Помощника планирования».

use jiff::civil::{Time, Weekday};
use jiff::tz::TimeZone;

use crate::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BusyKind {
    /// Под вопросом (tentative) — слабее «занят».
    Tentative,
    Busy,
    /// Вне офиса / недоступен.
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Busy {
    pub span: Span,
    pub kind: BusyKind,
}

/// Состояние ячейки сетки помощника (по убыванию важности для глаза).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Cell {
    Free,
    /// Нерабочее время участника/организации.
    OffHours,
    Tentative,
    Busy,
    Unavailable,
    /// Занятость не известна (нет данных от сервера).
    Unknown,
}

/// Рабочее время организации: дни недели и часы в часовом поясе пользователя.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkHours {
    pub tz: TimeZone,
    pub start: Time,
    pub end: Time,
    pub days: Vec<Weekday>,
}

impl WorkHours {
    /// Пн–Пт, 9:00–18:00.
    pub fn standard(tz: TimeZone) -> Self {
        use Weekday::*;
        Self { tz, start: Time::constant(9, 0, 0, 0), end: Time::constant(18, 0, 0, 0), days: vec![Monday, Tuesday, Wednesday, Thursday, Friday] }
    }

    /// Рабочие интервалы внутри `range`.
    pub fn spans(&self, range: Span) -> Vec<Span> {
        let mut out = Vec::new();
        let Ok(first) = jiff::Timestamp::from_second(range.start) else { return out };
        let mut day = first.to_zoned(self.tz.clone()).date();
        loop {
            let Ok(s) = day.to_datetime(self.start).to_zoned(self.tz.clone()) else { break };
            if s.timestamp().as_second() >= range.end {
                break;
            }
            if self.days.contains(&day.weekday())
                && let Ok(e) = day.to_datetime(self.end).to_zoned(self.tz.clone())
            {
                let span = Span::new(s.timestamp().as_second().max(range.start), e.timestamp().as_second().min(range.end));
                if !span.is_empty() {
                    out.push(span);
                }
            }
            let Ok(next) = day.tomorrow() else { break };
            day = next;
        }
        out
    }
}

/// Слить пересекающиеся и смежные интервалы занятости; при наложении берётся более сильный вид.
pub fn merge(mut items: Vec<Busy>) -> Vec<Busy> {
    items.sort_by_key(|b| (b.span.start, b.span.end));
    let mut out: Vec<Busy> = Vec::new();
    for b in items.into_iter().filter(|b| !b.span.is_empty()) {
        match out.last_mut() {
            Some(last) if b.span.start <= last.span.end && b.kind == last.kind => {
                last.span.end = last.span.end.max(b.span.end);
            }
            _ => out.push(b),
        }
    }
    out
}

/// Сетка помощника: для каждого участника — ячейки шага `step` секунд в `range`.
/// `None` у участника — занятость неизвестна (сервер не отдал).
pub fn grid(people: &[Option<Vec<Busy>>], range: Span, step: i64, hours: &WorkHours) -> Vec<Vec<Cell>> {
    let work = hours.spans(range);
    let cells = ((range.len() + step - 1) / step).max(0) as usize;
    people
        .iter()
        .map(|busy| {
            (0..cells)
                .map(|n| {
                    let cell = Span::new(range.start + n as i64 * step, (range.start + (n as i64 + 1) * step).min(range.end));
                    let Some(busy) = busy else { return Cell::Unknown };
                    let strongest = busy.iter().filter(|b| b.span.overlaps(&cell)).map(|b| b.kind).max();
                    match strongest {
                        Some(BusyKind::Unavailable) => Cell::Unavailable,
                        Some(BusyKind::Busy) => Cell::Busy,
                        Some(BusyKind::Tentative) => Cell::Tentative,
                        None if work.iter().any(|w| w.start <= cell.start && cell.end <= w.end) => Cell::Free,
                        None => Cell::OffHours,
                    }
                })
                .collect()
        })
        .collect()
}

/// Первые `limit` окон длиной `duration` в рабочее время, когда свободны все обязательные
/// (`required`) участники; «под вопросом» допускается только у необязательных.
/// Начала окон выравниваются на `step` (обычно 30 минут), как в Outlook.
pub fn find_slots(
    required: &[Vec<Busy>],
    optional: &[Vec<Busy>],
    range: Span,
    duration: i64,
    step: i64,
    hours: &WorkHours,
    limit: usize,
) -> Vec<Span> {
    let mut out = Vec::new();
    for work in hours.spans(range) {
        let mut t = align_up(work.start, step);
        while t + duration <= work.end && out.len() < limit {
            let slot = Span::new(t, t + duration);
            let req_free = required.iter().all(|bs| bs.iter().all(|b| !b.span.overlaps(&slot)));
            let opt_ok = optional.iter().all(|bs| bs.iter().all(|b| b.kind == BusyKind::Tentative || !b.span.overlaps(&slot)));
            if req_free && opt_ok {
                out.push(slot);
                // Следующее предложение — не внахлёст с этим.
                t = align_up(slot.end, step);
            } else {
                t += step;
            }
        }
    }
    out
}

fn align_up(t: i64, step: i64) -> i64 {
    if step <= 0 { t } else { t + (step - t.rem_euclid(step)) % step }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-19 — понедельник; время в UTC, рабочие часы тоже в UTC для простоты.
    fn at(day: i64, h: i64, m: i64) -> i64 {
        let monday = "2026-10-19T00:00:00Z".parse::<jiff::Timestamp>().unwrap().as_second();
        monday + day * 86_400 + h * 3600 + m * 60
    }

    fn busy(a: i64, b: i64, kind: BusyKind) -> Busy {
        Busy { span: Span::new(a, b), kind }
    }

    #[test]
    fn work_hours_skip_weekend() {
        let hours = WorkHours::standard(TimeZone::UTC);
        // Пятница 00:00 — понедельник 00:00 следующей недели: только пятница.
        let spans = hours.spans(Span::new(at(4, 0, 0), at(7, 0, 0)));
        assert_eq!(spans, vec![Span::new(at(4, 9, 0), at(4, 18, 0))]);
    }

    #[test]
    fn merge_overlaps() {
        let m = merge(vec![
            busy(at(0, 10, 0), at(0, 11, 0), BusyKind::Busy),
            busy(at(0, 10, 30), at(0, 12, 0), BusyKind::Busy),
            busy(at(0, 12, 0), at(0, 13, 0), BusyKind::Busy),
            busy(at(0, 14, 0), at(0, 15, 0), BusyKind::Tentative),
        ]);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].span, Span::new(at(0, 10, 0), at(0, 13, 0)));
    }

    #[test]
    fn slots_respect_everyone() {
        let hours = WorkHours::standard(TimeZone::UTC);
        let ivan = vec![busy(at(0, 9, 0), at(0, 12, 0), BusyKind::Busy)];
        let maria = vec![busy(at(0, 12, 0), at(0, 13, 30), BusyKind::Busy)];
        // Необязательный под вопросом в 13:30–14:00 — не мешает; занятый в 14:00–15:00 — мешает.
        let petr = vec![busy(at(0, 13, 30), at(0, 14, 0), BusyKind::Tentative), busy(at(0, 14, 0), at(0, 15, 0), BusyKind::Busy)];
        let slots = find_slots(&[ivan, maria], &[petr], Span::new(at(0, 0, 0), at(1, 0, 0)), 3600, 1800, &hours, 3);
        assert_eq!(slots, vec![Span::new(at(0, 15, 0), at(0, 16, 0)), Span::new(at(0, 16, 0), at(0, 17, 0)), Span::new(at(0, 17, 0), at(0, 18, 0))]);
        // 13:30–14:30 не подходит из-за занятости Петра с 14:00.
        assert!(!slots.contains(&Span::new(at(0, 13, 30), at(0, 14, 30))));
    }

    #[test]
    fn grid_cells() {
        let hours = WorkHours::standard(TimeZone::UTC);
        let range = Span::new(at(0, 8, 0), at(0, 11, 0));
        let g = grid(&[Some(vec![busy(at(0, 9, 30), at(0, 10, 0), BusyKind::Busy)]), None], range, 1800, &hours);
        assert_eq!(g[0], vec![Cell::OffHours, Cell::OffHours, Cell::Free, Cell::Busy, Cell::Free, Cell::Free]);
        assert!(g[1].iter().all(|c| *c == Cell::Unknown));
    }
}
