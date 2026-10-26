//! Раскладка пересекающихся событий дня по колонкам, как в Outlook:
//! группы связанных пересечений делят ширину поровну, событие растягивается
//! вправо на свободные колонки.

use crate::Span;

/// Положение события: колонка, сколько колонок занимает и сколько всего в группе.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placed {
    pub index: usize,
    pub column: usize,
    pub span_columns: usize,
    pub columns: usize,
}

/// `items` — интервалы событий (индексы сохраняются в результате).
pub fn layout_day(items: &[Span]) -> Vec<Placed> {
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by_key(|&i| (items[i].start, std::cmp::Reverse(items[i].end)));
    let mut out = Vec::with_capacity(items.len());
    let mut group: Vec<(usize, usize)> = Vec::new(); // (индекс, колонка)
    let mut columns_end: Vec<i64> = Vec::new();
    let mut group_end = i64::MIN;
    for i in order {
        let s = items[i];
        if s.start >= group_end && !group.is_empty() {
            flush(&mut group, &mut columns_end, items, &mut out);
        }
        // Первая колонка, где последнее событие уже закончилось.
        let col = match columns_end.iter().position(|&end| end <= s.start) {
            Some(c) => c,
            None => {
                columns_end.push(i64::MIN);
                columns_end.len() - 1
            }
        };
        columns_end[col] = s.end.max(s.start + 1);
        group.push((i, col));
        group_end = group_end.max(s.end.max(s.start + 1));
    }
    flush(&mut group, &mut columns_end, items, &mut out);
    out.sort_by_key(|p| p.index);
    out
}

fn flush(group: &mut Vec<(usize, usize)>, columns_end: &mut Vec<i64>, items: &[Span], out: &mut Vec<Placed>) {
    let columns = columns_end.len().max(1);
    for &(i, col) in group.iter() {
        // Растягиваем вправо, пока соседние колонки свободны на время события.
        let mut span = 1;
        while col + span < columns && !group.iter().any(|&(j, c)| c == col + span && items[j].overlaps(&items[i])) {
            span += 1;
        }
        out.push(Placed { index: i, column: col, span_columns: span, columns });
    }
    group.clear();
    columns_end.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(a: i64, b: i64) -> Span {
        Span::new(a, b)
    }

    #[test]
    fn separate_events_take_full_width() {
        let p = layout_day(&[s(0, 10), s(10, 20)]);
        assert!(p.iter().all(|p| p.columns == 1 && p.span_columns == 1 && p.column == 0));
    }

    #[test]
    fn overlapping_share_columns_and_expand() {
        // A 0–30, B 0–10, C 10–20, D 40–50 (отдельная группа).
        let p = layout_day(&[s(0, 30), s(0, 10), s(10, 20), s(40, 50)]);
        assert_eq!((p[0].column, p[0].columns), (0, 2));
        assert_eq!((p[1].column, p[1].columns), (1, 2));
        // C встаёт во вторую колонку после B.
        assert_eq!(p[2].column, 1);
        assert_eq!((p[3].column, p[3].columns, p[3].span_columns), (0, 1, 1));
    }

    #[test]
    fn short_event_expands_into_free_column() {
        // A 0–10 и B 0–30 в двух колонках, C 15–20 — в колонке A, растягивается? Нет: справа B.
        // D 20–40: колонка A свободна, B занята до 30 — D не растягивается.
        let p = layout_day(&[s(0, 30), s(0, 10), s(15, 20)]);
        assert_eq!(p[2].column, 1);
        assert_eq!(p[2].span_columns, 1);
        // Три события, третье — в колонке 2, но первые два не пересекаются с ним справа.
        let p = layout_day(&[s(0, 10), s(0, 30), s(0, 5), s(6, 9)]);
        assert_eq!(p.iter().map(|x| x.columns).max(), Some(3));
        let d = p[3];
        assert_eq!(d.column, 2);
    }
}
