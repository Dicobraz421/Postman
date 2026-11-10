//! История обычной задачи (T2-33, макет «Задачи»: «Создана из письма «…» · пн 18:12», «Срок перенесён с пн на
//! сегодня · 09:05», «Подзадачи: 2 из 5 · 13:50»). Записи — своё свойство `X-POSTMAN-HISTORY` (JSON, как шаги
//! поручения `X-POSTMAN-STEP`): сервер хранит его в `iCalendar.properties`, другие клиенты не трогают. У поручения
//! история — его шаги (`assign::steps`).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{Moment, Task};

pub const X_HISTORY: &str = "x-postman-history";
/// Записей не больше: старые уходят.
pub const KEEP: usize = 50;

/// Что случилось.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Created,
    /// Создана из письма (`subject` — тема).
    FromMail,
    /// Срок: `from` → `to` (местное время срока; нет — без срока).
    DueMoved,
    Done,
    Reopened,
    /// Подзадачи: `done` из `total`.
    Subtasks,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Unix-секунды.
    pub at: i64,
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subject: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub done: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub total: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

fn local(m: Option<&Moment>) -> Option<String> {
    m.map(|m| m.local.strftime("%Y-%m-%dT%H:%M:%S").to_string())
}

impl Entry {
    pub fn new(kind: Kind, at: i64) -> Self {
        Self { at, kind, subject: String::new(), from: None, to: None, done: 0, total: 0 }
    }

    pub fn from_mail(subject: &str, at: i64) -> Self {
        Self { subject: subject.trim().to_owned(), ..Self::new(Kind::FromMail, at) }
    }

    pub fn due_moved(from: Option<&Moment>, to: Option<&Moment>, at: i64) -> Self {
        Self { from: local(from), to: local(to), ..Self::new(Kind::DueMoved, at) }
    }

    pub fn subtasks(done: usize, total: usize, at: i64) -> Self {
        Self { done, total, ..Self::new(Kind::Subtasks, at) }
    }

    /// Местное время срока записи (`from` или `to`).
    pub fn moment(s: Option<&str>) -> Option<jiff::civil::DateTime> {
        s.and_then(|s| s.parse().ok())
    }
}

fn is_entry(p: &Value) -> bool {
    p[0].as_str().is_some_and(|n| n.eq_ignore_ascii_case(X_HISTORY))
}

/// Записи по времени (старые первыми); битые — пропускаются.
pub fn entries(t: &Task) -> Vec<Entry> {
    let mut out: Vec<Entry> =
        t.extra.iter().filter(|p| is_entry(p)).filter_map(|p| p[3].as_str().and_then(|s| serde_json::from_str(s).ok())).collect();
    out.sort_by_key(|e| e.at);
    out
}

/// Добавить запись. Подзадачи подряд — одной записью (последней); срок, вернувшийся к прежнему подряд, — тоже.
/// Лишние старые уходят ([`KEEP`]).
pub fn push(t: &mut Task, e: Entry) {
    let mut list = entries(t);
    if list.last().is_some_and(|l| l.kind == e.kind && matches!(e.kind, Kind::Subtasks)) {
        list.pop();
    }
    if list.last() == Some(&e) {
        return;
    }
    list.push(e);
    if list.len() > KEEP {
        list.drain(..list.len() - KEEP);
    }
    t.extra.retain(|p| !is_entry(p));
    t.extra.extend(list.iter().filter_map(|e| serde_json::to_string(e).ok()).map(|v| json!([X_HISTORY, {}, "unknown", v])));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moment(s: &str) -> Moment {
        Moment::new(s.parse().unwrap(), None)
    }

    /// Записи живут в своём свойстве и читаются по времени; подзадачи подряд — одной записью; старые уходят.
    #[test]
    fn history_round_trips_and_merges_subtasks() {
        let mut t = Task::new("u", "c", "Отчёт");
        push(&mut t, Entry::from_mail("Re: Договор", 10));
        push(&mut t, Entry::due_moved(Some(&moment("2026-11-09T00:00:00")), Some(&moment("2026-11-10T00:00:00")), 20));
        push(&mut t, Entry::subtasks(1, 5, 30));
        push(&mut t, Entry::subtasks(2, 5, 40));
        let e = entries(&t);
        assert_eq!(e.iter().map(|e| e.kind).collect::<Vec<_>>(), [Kind::FromMail, Kind::DueMoved, Kind::Subtasks]);
        assert_eq!((e[2].done, e[2].total, e[2].at), (2, 5, 40));
        assert_eq!(e[0].subject, "Re: Договор");
        assert_eq!(Entry::moment(e[1].to.as_deref()), Some("2026-11-10T00:00:00".parse().unwrap()));
        // Повтор той же записи — не дублируется.
        push(&mut t, Entry::new(Kind::Done, 50));
        push(&mut t, Entry::new(Kind::Done, 50));
        assert_eq!(entries(&t).len(), 4);
        for i in 0..KEEP {
            push(&mut t, Entry::new(if i % 2 == 0 { Kind::Reopened } else { Kind::Done }, 100 + i as i64));
        }
        let e = entries(&t);
        assert_eq!(e.len(), KEEP);
        assert_eq!(e.last().map(|e| e.at), Some(100 + KEEP as i64 - 1));
        // Свойство своё: чужие свойства не тронуты.
        t.extra.push(json!(["x-other", {}, "text", "a"]));
        push(&mut t, Entry::new(Kind::Created, 1000));
        assert!(t.extra.iter().any(|p| p[0] == "x-other"));
    }
}
