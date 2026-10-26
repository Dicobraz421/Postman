//! Календарь без UI и без сети: модель события (JSCalendar, RFC 8984) и задачи (JSCalendar `Task`, S6-02),
//! раскладка пересекающихся событий в дне, занятость участников и поиск общего свободного
//! времени («Помощник планирования»). Интерфейса не знает.

pub mod assign;
mod event;
mod freebusy;
pub mod ics;
mod invite;
mod layout;
mod task;

pub use event::{Event, EventStatus, FreeBusy, NewEvent, Owner, Participant, Partstat, format_duration, parse_duration};
pub use freebusy::{Busy, BusyKind, Cell, WorkHours, find_slots, grid, merge};
pub use invite::{Invitation, imip_reply, participant_id, partstat_json, reply_subject};
pub use layout::{Placed, layout_day};
pub use task::{
    Alert, Anchor, MailLink, Moment, Progress, Role, TASK_PROPERTIES, Task, TaskParticipant, TaskPartstat, TimeBlock, Trigger, X_BLOCK, X_EMAIL_ID,
    X_MESSAGE_ID, X_ORDER, X_PROJECT, X_REPEAT_AFTER, X_SOMEDAY,
};

/// Промежуток времени в Unix-секундах, `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Span {
    pub start: i64,
    pub end: i64,
}

impl Span {
    pub fn new(start: i64, end: i64) -> Self {
        Self { start, end: end.max(start) }
    }

    pub fn overlaps(&self, o: &Span) -> bool {
        self.start < o.end && o.start < self.end
    }

    pub fn len(&self) -> i64 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
