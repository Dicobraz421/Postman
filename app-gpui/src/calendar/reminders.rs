//! Напоминания о встречах в Postman (T2-08б, «Настройки → Календарь → Напоминание по умолчанию»: «тостом в углу
//! окна»). Напоминание хранит сервер в моей копии события (`Event::alert_secs`: JSCalendar `alerts`, iCalendar
//! `VALARM` через воркер, EWS `ReminderMinutesBeforeStart`) — его видят и телефон, и другой клиент. Postman держит
//! мои события на сегодня и завтра отдельным запросом (показанная неделя может быть другой) и в срок шлёт тост,
//! а при неактивном окне — и уведомление Windows.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui_kit::*;
use postman_calendar::{Event, EventStatus, Partstat};

use super::{CalendarAction, CalendarView, day_start, my_part};

/// Как часто проверять сроки.
const TICK: Duration = Duration::from_secs(30);
/// Как часто перечитывать ближайшие события с сервера.
const REFRESH: i64 = 10 * 60;
/// Опоздавшее напоминание (Postman был закрыт) — ещё показать, если встреча началась не раньше этого.
const LATE: i64 = 5 * 60;

/// Ближайшие события по учётным записям и что уже напомнили.
#[derive(Default)]
pub(super) struct Upcoming {
    by_account: HashMap<Option<String>, Vec<Event>>,
    /// Ждём этот период с сервера.
    pub(super) range: Option<(i64, i64)>,
    asked_at: Option<i64>,
    fired: HashSet<(String, i64)>,
}

/// Напомнить ли о событии сейчас: моё, не отменено, я не отказался, срок напоминания наступил, встреча не прошла.
pub(crate) fn due(e: &Event, me: Option<&str>, now: i64) -> bool {
    let Some(before) = e.alert_secs else { return false };
    let declined = my_part(e, me).is_some_and(|p| p.status == Partstat::Declined);
    e.owner.is_none() && !e.all_day && e.status != EventStatus::Cancelled && !declined && now >= e.span.start - before && now < e.span.start + LATE
}

impl CalendarView {
    /// Таймер напоминаний — с созданием календаря.
    pub(crate) fn start_reminders(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                if this.update(cx, |v, cx| v.remind_tick(cx)).is_err() {
                    return;
                }
                cx.background_executor().timer(TICK).await;
            }
        })
        .detach();
    }

    fn remind_tick(&mut self, cx: &mut Context<Self>) {
        // Снимки: тост по часам съёмки сбил бы кадр.
        if crate::screens::screening() {
            return;
        }
        let now = self.now();
        if self.live && self.upcoming.asked_at.is_none_or(|t| now - t >= REFRESH) {
            let from = day_start(self.now_zoned().date(), &self.tz);
            let range = (from, from + 2 * 86_400);
            self.upcoming.range = Some(range);
            self.upcoming.asked_at = Some(now);
            cx.emit(CalendarAction::Load { from: range.0, to: range.1 });
        }
        let me = self.me.clone();
        let events: Vec<Event> = if self.live { self.upcoming.by_account.values().flatten().cloned().collect() } else { self.events.clone() };
        for e in events.into_iter().filter(|e| due(e, me.as_deref(), now)) {
            if self.upcoming.fired.insert((e.uid.clone(), e.span.start)) {
                cx.emit(CalendarAction::Remind(Box::new(e)));
            }
        }
    }

    /// Ответ сервера на запрос ближайших событий; `false` — это ответ на другой запрос.
    pub(super) fn upcoming_loaded(&mut self, range: (i64, i64), account: &Option<String>, events: &[Event]) -> bool {
        if self.upcoming.range != Some(range) {
            return false;
        }
        self.upcoming.by_account.insert(account.clone(), events.to_vec());
        true
    }

    /// Живой ящик: прежние ближайшие события — не его.
    pub(super) fn upcoming_reset(&mut self) {
        self.upcoming = Upcoming::default();
    }
}

#[cfg(test)]
mod tests {
    use postman_calendar::{Event, EventStatus, FreeBusy, Span};

    use super::due;

    fn ev(start: i64, alert: Option<i64>) -> Event {
        Event {
            private: false,
            recurrence: None,
            id: "e".into(),
            uid: "u".into(),
            calendar_ids: Vec::new(),
            title: "Планёрка".into(),
            description: String::new(),
            location: String::new(),
            span: Span::new(start, start + 1800),
            all_day: false,
            status: EventStatus::Confirmed,
            free_busy: FreeBusy::Busy,
            participants: Vec::new(),
            recurring: false,
            keywords: Vec::new(),
            sequence: 0,
            owner: None,
            alert_secs: alert,
        }
    }

    /// За 15 мин — да, раньше — нет, без напоминания — нет, давно началась — нет, отменённая — нет.
    #[test]
    fn reminder_window() {
        let e = ev(10_000, Some(900));
        assert!(!due(&e, None, 10_000 - 901));
        assert!(due(&e, None, 10_000 - 900));
        assert!(due(&e, None, 10_000 + 60), "опоздало на минуту — ещё напомнить");
        assert!(!due(&e, None, 10_000 + 600));
        assert!(!due(&ev(10_000, None), None, 10_000 - 60));
        let mut cancelled = ev(10_000, Some(900));
        cancelled.status = EventStatus::Cancelled;
        assert!(!due(&cancelled, None, 10_000 - 60));
    }
}
