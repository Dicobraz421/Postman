//! «Отправить позже» и отмена отправки (S5-06): время отправки и окно «Отменить».
//!
//! Обе функции — на сервере, FUTURERELEASE (RFC 4865, `docs/STALWART.md`): письмо уходит в `EmailSubmission` сразу,
//! сервер держит его до срока, пока срок не наступил, отправку отменяет `undoStatus: canceled`. Поэтому Postman
//! ничего не держит у себя: закрыли программу в окне «Отменить» — письмо уйдёт по сроку, запланированное — в свой
//! час, и без запущенного Postman.
//!
//! Здесь — без UI и сети: варианты времени в местном поясе, проверка срока, разбор введённой даты и состояние окна
//! «Отменить» ([`Undo`]); тексты — у интерфейса.

use std::time::{Duration, Instant};

use jiff::civil::{Date, Time};
use jiff::{ToSpan, Zoned};

/// Сколько виден тост «Письмо отправлено · Отменить» (позже — настройка).
pub const UNDO_WINDOW: Duration = Duration::from_secs(10);
/// Запас сервера сверх окна: тост появляется после ответа сервера, отмена в последнюю секунду окна ещё успевает.
pub const UNDO_MARGIN: Duration = Duration::from_secs(3);
/// Сколько сервер держит обычную отправку (`HOLDFOR`), секунды.
pub const UNDO_HOLD_SECS: u32 = (UNDO_WINDOW.as_secs() + UNDO_MARGIN.as_secs()) as u32;
/// Предел задержки очереди Stalwart: `maxDelayedSend` в сессии — 30 дней, а SMTP отвечает «Requested hold time
/// exceeds maximum of 604800 seconds» (`docs/STALWART.md`). Берём меньшее из объявленного и этого.
pub const SERVER_HOLD_MAX: u64 = 7 * 24 * 3600;
/// Запланировать можно не раньше, чем через минуту: иначе это обычная отправка.
pub const MIN_LEAD_SECS: i64 = 60;
/// Утро и день в вариантах меню.
pub const MORNING: i8 = 9;
pub const AFTERNOON: i8 = 13;

/// Сколько сервер на деле позволяет задержать письмо (секунды); 0 — отложенной отправки нет.
pub fn max_delay(advertised: u64) -> u64 {
    advertised.min(SERVER_HOLD_MAX)
}

/// Отправка, которую сервер держит: ключи кэша (с префиксом общего ящика) и срок.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scheduled {
    /// EmailSubmission (ключ: аккаунт отправителя + id на сервере).
    pub submission: String,
    /// Письмо в «Отправленных» (ключ кэша).
    pub email: String,
    /// Unix-секунды.
    pub send_at: i64,
}

/// Вариант меню «Отправить позже».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// Завтра в 09:00.
    TomorrowMorning,
    /// Завтра в 13:00.
    TomorrowAfternoon,
    /// В ближайший понедельник после сегодняшнего дня в 09:00 (в понедельник — через неделю).
    Monday,
}

impl Choice {
    pub const ALL: [Choice; 3] = [Choice::TomorrowMorning, Choice::TomorrowAfternoon, Choice::Monday];

    /// Момент в поясе `now`; `None` — такого времени нет (дата вне диапазона).
    pub fn at(self, now: &Zoned) -> Option<Zoned> {
        let today = now.date();
        let (date, hour) = match self {
            Choice::TomorrowMorning => (today.tomorrow().ok()?, MORNING),
            Choice::TomorrowAfternoon => (today.tomorrow().ok()?, AFTERNOON),
            Choice::Monday => (next_monday(today)?, MORNING),
        };
        local(date, Time::new(hour, 0, 0, 0).ok()?, now)
    }
}

/// Понедельник строго после `today`.
fn next_monday(today: Date) -> Option<Date> {
    let days = 8 - i64::from(today.weekday().to_monday_one_offset());
    today.checked_add(days.days()).ok()
}

/// Дата и время в поясе `now`. Перевод часов: несуществующее время сдвигается вперёд, двойное — первое.
fn local(date: Date, time: Time, now: &Zoned) -> Option<Zoned> {
    date.to_datetime(time).to_zoned(now.time_zone().clone()).ok()
}

/// Почему срок не годится.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    /// Дату или время не разобрать.
    Format,
    /// Срок прошёл или ближе [`MIN_LEAD_SECS`].
    Past,
    /// Дальше, чем сервер держит письма.
    TooFar,
    /// Сервер не умеет отложенную отправку.
    Unsupported,
}

/// Проверить срок: Unix-секунды для сервера или причина отказа. `max` — [`max_delay`].
pub fn check(at: &Zoned, now: &Zoned, max: u64) -> Result<i64, Reject> {
    if max == 0 {
        return Err(Reject::Unsupported);
    }
    let (at, now) = (at.timestamp().as_second(), now.timestamp().as_second());
    let ahead = at - now;
    if ahead < MIN_LEAD_SECS {
        return Err(Reject::Past);
    }
    if u64::try_from(ahead).unwrap_or(u64::MAX) > max {
        return Err(Reject::TooFar);
    }
    Ok(at)
}

/// Разобрать введённые дату (`ДД.ММ.ГГГГ`, `ДД.ММ` — этот год, `ГГГГ-ММ-ДД`) и время (`ЧЧ:ММ`, `ЧЧ.ММ`) в поясе `now`.
pub fn parse(date: &str, time: &str, now: &Zoned) -> Result<Zoned, Reject> {
    let date = parse_date(date.trim(), now.date()).ok_or(Reject::Format)?;
    let time = parse_time(time.trim()).ok_or(Reject::Format)?;
    local(date, time, now).ok_or(Reject::Format)
}

fn numbers(s: &str, sep: &[char]) -> Option<Vec<i32>> {
    s.split(sep).map(|p| p.trim().parse::<i32>().ok()).collect()
}

fn parse_date(s: &str, today: Date) -> Option<Date> {
    let (y, m, d) = if s.contains('-') {
        match numbers(s, &['-'])?.as_slice() {
            [y, m, d] => (*y, *m, *d),
            _ => return None,
        }
    } else {
        match numbers(s, &['.', '/'])?.as_slice() {
            [d, m] => (i32::from(today.year()), *m, *d),
            [d, m, y] if *y < 100 => (2000 + *y, *m, *d),
            [d, m, y] => (*y, *m, *d),
            _ => return None,
        }
    };
    Date::new(i16::try_from(y).ok()?, i8::try_from(m).ok()?, i8::try_from(d).ok()?).ok()
}

fn parse_time(s: &str) -> Option<Time> {
    match numbers(s, &[':', '.'])?.as_slice() {
        [h, m] => Time::new(i8::try_from(*h).ok()?, i8::try_from(*m).ok()?, 0, 0).ok(),
        [h] => Time::new(i8::try_from(*h).ok()?, 0, 0, 0).ok(),
        _ => None,
    }
}

// --- окно «Отменить» ---

/// Что с предложением отменить.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfferState {
    /// Тост виден, «Отменить» можно нажать до `until`.
    Open,
    /// Нажали «Отменить»: ждём ответа сервера с этим тикетом (тост остаётся, пока не придёт ответ).
    Canceling(u64),
}

/// Отправка, которую ещё можно отменить, и то, что вернуть в окно письма (`content`).
#[derive(Debug, Clone)]
pub struct Offer<D> {
    pub id: u64,
    pub scheduled: Scheduled,
    pub content: D,
    pub until: Instant,
    pub state: OfferState,
}

/// Окно «Отменить» отправки: по предложению на отправленное письмо, на экране — последнее.
#[derive(Debug)]
pub struct Undo<D> {
    offers: Vec<Offer<D>>,
    next: u64,
}

impl<D> Default for Undo<D> {
    fn default() -> Self {
        Self { offers: Vec::new(), next: 0 }
    }
}

impl<D> Undo<D> {
    /// Письмо отправлено с задержкой: предложить «Отменить» на [`UNDO_WINDOW`].
    pub fn offer(&mut self, scheduled: Scheduled, content: D, now: Instant) -> u64 {
        self.next += 1;
        self.offers.push(Offer { id: self.next, scheduled, content, until: now + UNDO_WINDOW, state: OfferState::Open });
        self.next
    }

    /// Что показать: последнее предложение, которое не истекло (или ждёт ответа на отмену).
    pub fn shown(&self, now: Instant) -> Option<&Offer<D>> {
        self.offers.iter().rev().find(|o| o.state != OfferState::Open || now < o.until)
    }

    /// «Отменить»: отправка для команды отмены; `None` — окно закрылось или отмена уже идёт.
    pub fn cancel(&mut self, id: u64, ticket: u64, now: Instant) -> Option<Scheduled> {
        let o = self.offers.iter_mut().find(|o| o.id == id && o.state == OfferState::Open && now < o.until)?;
        o.state = OfferState::Canceling(ticket);
        Some(o.scheduled.clone())
    }

    /// Сервер отменил отправку: что вернуть в окно письма.
    pub fn unsent(&mut self, ticket: u64) -> Option<D> {
        let i = self.offers.iter().position(|o| o.state == OfferState::Canceling(ticket))?;
        Some(self.offers.remove(i).content)
    }

    /// Отмена не удалась (письмо ушло, сбой): предложение снято. `true` — оно было нашим.
    pub fn failed(&mut self, ticket: u64) -> bool {
        let before = self.offers.len();
        self.offers.retain(|o| o.state != OfferState::Canceling(ticket));
        self.offers.len() != before
    }

    /// Убрать истёкшие. `true` — что-то убрано (перерисовать).
    pub fn expire(&mut self, now: Instant) -> bool {
        let before = self.offers.len();
        self.offers.retain(|o| o.state != OfferState::Open || now < o.until);
        self.offers.len() != before
    }

    /// Ближайший срок открытого предложения (таймер скрытия тоста).
    pub fn next_deadline(&self) -> Option<Instant> {
        self.offers.iter().filter(|o| o.state == OfferState::Open).map(|o| o.until).min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::tz::TimeZone;

    fn at(s: &str, tz: &str) -> Zoned {
        let tz = TimeZone::get(tz).unwrap();
        s.parse::<jiff::civil::DateTime>().unwrap().to_zoned(tz).unwrap()
    }

    fn hm(z: &Zoned) -> String {
        z.strftime("%a %Y-%m-%d %H:%M %:z").to_string()
    }

    #[test]
    fn choices_are_in_the_local_time_zone() {
        // Вторник, 23:40 в Москве: «завтра» — среда по Москве, хотя в UTC ещё вторник 20:40.
        let now = at("2026-10-20T23:40", "Europe/Moscow");
        assert_eq!(hm(&Choice::TomorrowMorning.at(&now).unwrap()), "Wed 2026-10-21 09:00 +03:00");
        assert_eq!(hm(&Choice::TomorrowAfternoon.at(&now).unwrap()), "Wed 2026-10-21 13:00 +03:00");
        assert_eq!(Choice::TomorrowMorning.at(&now).unwrap().timestamp().as_second(), 1_792_562_400);
        // Тот же момент во Владивостоке — уже среда: «завтра» — четверг.
        let vl = now.with_time_zone(TimeZone::get("Asia/Vladivostok").unwrap());
        assert_eq!(hm(&Choice::TomorrowMorning.at(&vl).unwrap()), "Thu 2026-10-22 09:00 +10:00");
    }

    #[test]
    fn monday_is_the_next_one_after_today() {
        let monday = |s: &str| hm(&Choice::Monday.at(&at(s, "Europe/Moscow")).unwrap());
        assert_eq!(monday("2026-10-20T10:00"), "Mon 2026-10-26 09:00 +03:00"); // вторник
        assert_eq!(monday("2026-10-25T22:00"), "Mon 2026-10-26 09:00 +03:00"); // воскресенье — завтра
        // В понедельник — следующий, даже до 09:00.
        assert_eq!(monday("2026-10-26T07:00"), "Mon 2026-11-02 09:00 +03:00");
        assert_eq!(monday("2026-10-26T18:00"), "Mon 2026-11-02 09:00 +03:00");
    }

    #[test]
    fn daylight_saving_shift_keeps_the_wall_clock() {
        // Берлин: 25.10.2026 часы назад — завтра в 09:00 по местным часам, смещение уже зимнее.
        let now = at("2026-10-24T20:00", "Europe/Berlin");
        assert_eq!(hm(&Choice::TomorrowMorning.at(&now).unwrap()), "Sun 2026-10-25 09:00 +01:00");
    }

    #[test]
    fn past_near_and_far_times_are_rejected() {
        let now = at("2026-10-20T10:00", "Europe/Moscow");
        let max = max_delay(2_592_000);
        assert_eq!(max, SERVER_HOLD_MAX);
        assert_eq!(check(&at("2026-10-20T09:59", "Europe/Moscow"), &now, max), Err(Reject::Past));
        assert_eq!(check(&now, &now, max), Err(Reject::Past));
        assert_eq!(check(&at("2026-10-20T10:00:30", "Europe/Moscow"), &now, max), Err(Reject::Past));
        assert!(check(&at("2026-10-20T10:01", "Europe/Moscow"), &now, max).is_ok());
        assert!(check(&at("2026-10-27T10:00", "Europe/Moscow"), &now, max).is_ok());
        assert_eq!(check(&at("2026-10-27T10:01", "Europe/Moscow"), &now, max), Err(Reject::TooFar));
        assert_eq!(check(&at("2026-10-21T09:00", "Europe/Moscow"), &now, 0), Err(Reject::Unsupported));
        // Понедельник из понедельника до 09:00 — дальше семи дней: сервер его не примет.
        let early = at("2026-10-26T07:00", "Europe/Moscow");
        assert_eq!(check(&Choice::Monday.at(&early).unwrap(), &early, max), Err(Reject::TooFar));
    }

    #[test]
    fn typed_date_and_time() {
        let now = at("2026-10-20T10:00", "Europe/Moscow");
        let p = |d: &str, t: &str| parse(d, t, &now).map(|z| hm(&z));
        assert_eq!(p("22.10.2026", "14:30").as_deref(), Ok("Thu 2026-10-22 14:30 +03:00"));
        assert_eq!(p("22.10", "9.05").as_deref(), Ok("Thu 2026-10-22 09:05 +03:00"));
        assert_eq!(p("2026-10-22", "9").as_deref(), Ok("Thu 2026-10-22 09:00 +03:00"));
        assert_eq!(p(" 1.11.26 ", " 08:00 ").as_deref(), Ok("Sun 2026-11-01 08:00 +03:00"));
        for (d, t) in [("31.02.2026", "10:00"), ("22.10.2026", "25:00"), ("завтра", "10:00"), ("22.10", ""), ("", "10:00")] {
            assert_eq!(p(d, t), Err(Reject::Format), "{d} {t}");
        }
    }

    fn sched(n: &str) -> Scheduled {
        Scheduled { submission: format!("s{n}"), email: format!("e{n}"), send_at: 0 }
    }

    #[test]
    fn undo_offer_is_cancelled_within_the_window() {
        let t0 = Instant::now();
        let mut u = Undo::default();
        let id = u.offer(sched("1"), "draft 1", t0);
        assert_eq!(u.shown(t0).map(|o| o.id), Some(id));
        assert_eq!(u.next_deadline(), Some(t0 + UNDO_WINDOW));
        // «Отменить» — команда с этой отправкой; повторное нажатие — ничего.
        assert_eq!(u.cancel(id, 7, t0 + Duration::from_secs(9)), Some(sched("1")));
        assert_eq!(u.cancel(id, 8, t0 + Duration::from_secs(9)), None);
        // Пока сервер отвечает, тост виден и после окна, а истечение его не убирает.
        assert!(!u.expire(t0 + UNDO_WINDOW * 2));
        assert!(u.shown(t0 + UNDO_WINDOW * 2).is_some());
        assert_eq!(u.unsent(8), None);
        assert_eq!(u.unsent(7), Some("draft 1"));
        assert!(u.shown(t0).is_none());
    }

    #[test]
    fn undo_offer_expires_and_failed_cancel_drops_it() {
        let t0 = Instant::now();
        let mut u = Undo::default();
        let a = u.offer(sched("a"), 1, t0);
        let b = u.offer(sched("b"), 2, t0 + Duration::from_secs(5));
        // На экране — последнее.
        assert_eq!(u.shown(t0 + Duration::from_secs(6)).map(|o| o.id), Some(b));
        // Окно первого закрылось: отменить нельзя, оно убирается; второе ещё открыто.
        let late = t0 + UNDO_WINDOW;
        assert_eq!(u.cancel(a, 1, late), None);
        assert!(u.expire(late));
        assert_eq!(u.shown(late).map(|o| o.id), Some(b));
        // Отмена второго не удалась (письмо уже ушло) — предложение снято.
        assert!(u.cancel(b, 2, late).is_some());
        assert!(u.failed(2));
        assert!(!u.failed(2));
        assert!(u.shown(late).is_none());
        assert_eq!(u.next_deadline(), None);
    }
}
