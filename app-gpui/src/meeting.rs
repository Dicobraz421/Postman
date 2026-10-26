//! Новая встреча (S3-05; вид — S5-11 по макету «Новая встреча: подбор времени в одной форме»): вкладка окна
//! (модель вкладок S5-01), а не отдельное окно. Сверху — поля (тема, обязательные и необязательные участники
//! чипами с занятостью в выбранное время, переговорная, место, начало–конец, описание), ниже — помощник
//! планирования в той же форме: день, «Только общее свободное время», длительность, «Учитывать необязательных»,
//! строки участников с занятостью, общие окна — столбцами, выбранное время — рамкой.
//!
//! Модель ([`MeetingModel`]) — без gpui: поля → `postman_core::schedule::Meeting`, ошибки времени и адресов,
//! итог отправки по тикету; её проверяют тесты. View ([`MeetingView`]) — поля, подсказки адресов (адресная книга
//! окна письма), занятость с сервера (`Command::Availability` → `Principal/getAvailability`), «нет данных» отдельно
//! от «свободен». Отправка — `Command::CreateMeeting` (сервер рассылает приглашения); итог — тостом, вкладка
//! закрывается, встреча — в неделе. Демо: занятость — демо-коллеги, отправка — «нужен сервер».

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use gpui_kit::component::input::{
    Enter, IndentInline, Input, InputEvent, InputState, MoveDown, MoveUp, Textarea, TextareaState,
};
use gpui_kit::prelude::*;
use gpui_kit::*;
use jiff::civil::{Date, DateTime, Time};
use jiff::tz::TimeZone;
use pets_i18n::tr;
use pets_ui::Level as Toast;
use postman_calendar::{Busy, BusyKind, Cell, Span, WorkHours, grid};
use postman_compose::parse_addresses;
use postman_core::prefs::FreeColor;
use postman_core::schedule::Meeting;
use postman_core::sync::{Command, Event};
use postman_demo::calendar as demo;

use crate::calendar::{
    FIRST_HOUR, HOURS, STEP, add_days, avatar, day_start, duration_text, g, hm, long_day, monday_of,
    zone_label,
};
use crate::chrome::icon;
use crate::compose::{Book, accept_suggestion, current_token};
use crate::roles::{Look, Pm};
use crate::Mailer;

const LABEL_W: f32 = 120.0;
const NAME_W: f32 = 244.0;
const ROW_H: f32 = 36.0;
const SUGGESTIONS: usize = 6;
const SUGGEST_FROM: usize = 2;
/// Занятость спрашиваем, когда участники перестали меняться.
const QUERY_DELAY: Duration = Duration::from_millis(400);
const DURATIONS: [i64; 4] = [1800, 3600, 5400, 7200];
/// Самый мелкий шаг сетки (S5-20, «Шаг сетки» — 15 или 30 мин): встреча не короче.
const MIN_STEP: i64 = 15 * 60;
/// Переговорных в строке выбора — не больше.
const ROOMS: usize = 6;

static NEXT_TICKET: AtomicU64 = AtomicU64::new(1);

/// Тикет команды встречи: итог синхронизации находит свою вкладку по нему.
pub(crate) fn next_ticket() -> u64 {
    NEXT_TICKET.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Field {
    Title,
    Location,
    Date,
    Start,
    Attendees,
    Optional,
    Description,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Status {
    None,
    Sending,
    /// Дата или время не разобраны: что ввёл пользователь.
    BadTime(String),
    BadAddresses(Vec<String>),
    NoServer,
    Failed(String),
}

/// Поля встречи и итог отправки — без gpui.
#[derive(Debug)]
pub(crate) struct MeetingModel {
    pub(crate) title: String,
    pub(crate) location: String,
    /// «дд.мм.гггг» (или «дд.мм», «гггг-мм-дд»).
    pub(crate) date: String,
    /// «чч:мм».
    pub(crate) start: String,
    pub(crate) duration: i64,
    /// Обязательные участники: «Имя <адрес>; …».
    pub(crate) attendees: String,
    /// Необязательные участники и переговорные.
    pub(crate) optional: String,
    pub(crate) description: String,
    /// «От» (S7-03б): адрес организатора — учётная запись профиля, куда уходит встреча; `None` — основная.
    pub(crate) from: Option<String>,
    pub(crate) status: Status,
    /// Тикет отправки, итог которой ждём.
    sending: Option<u64>,
    /// Правка своей встречи (S7-07б): вкладка отправляет `UpdateMeeting`, а не `CreateMeeting`.
    pub(crate) editing: Option<Editing>,
}

/// Какую встречу правит вкладка: событие недели и у повтора — вся ли серия.
#[derive(Debug, Clone)]
pub(crate) struct Editing {
    pub(crate) event: Box<postman_calendar::Event>,
    pub(crate) series: bool,
}

/// Участник строкой поля вкладки: «Имя <адрес>», без имени — адрес.
fn person_line(name: &str, email: &str) -> String {
    match name.trim() {
        "" => email.to_string(),
        n if n.contains([',', ';', '<', '"']) => format!("\"{}\" <{email}>", n.replace('"', "")),
        n => format!("{n} <{email}>"),
    }
}

/// Дата: «дд.мм.гггг», «дд.мм» (год — текущий), «гггг-мм-дд».
pub(crate) fn parse_date(s: &str, today: Date) -> Option<Date> {
    let s = s.trim();
    if let Ok(d) = s.parse::<Date>() {
        return Some(d);
    }
    let parts: Vec<&str> = s.split(['.', '/']).collect();
    let n = |i: usize| parts.get(i).and_then(|p| p.trim().parse::<i16>().ok());
    let (day, month) = (n(0)?, n(1)?);
    let year = match parts.get(2) {
        None => today.year(),
        Some(_) => match n(2)? {
            y if y < 100 => 2000 + y,
            y => y,
        },
    };
    if parts.len() > 3 {
        return None;
    }
    Date::new(year, i8::try_from(month).ok()?, i8::try_from(day).ok()?).ok()
}

/// Время: «чч:мм», «чч.мм» или «чч».
pub(crate) fn parse_time(s: &str) -> Option<Time> {
    let s = s.trim();
    let (h, m) = s.split_once([':', '.']).unwrap_or((s, "0"));
    Time::new(h.trim().parse().ok()?, m.trim().parse().ok()?, 0, 0).ok()
}

/// Начало новой встречи по умолчанию: ближайшие полчаса в рабочее время, иначе 10:00 следующего рабочего дня.
pub(crate) fn default_start(now: &jiff::Zoned) -> DateTime {
    let mins = i64::from(now.hour()) * 60 + i64::from(now.minute());
    let next = (mins / 30 + 1) * 30;
    let workday = |d: Date| d.weekday().to_monday_zero_offset() < 5;
    let mut date = now.date();
    let at = |h: i64| {
        Time::new(i8::try_from(h / 60).unwrap_or(10), i8::try_from(h % 60).unwrap_or(0), 0, 0)
            .unwrap_or(Time::midnight())
    };
    if workday(date) && next <= 17 * 60 {
        return date.to_datetime(at(next.max(9 * 60)));
    }
    loop {
        date = add_days(date, 1);
        if workday(date) {
            return date.to_datetime(at(10 * 60));
        }
    }
}

/// Общие свободные окна дня `range`: шаги `STEP`, где у всех с известной занятостью (`Some`) рабочее время и
/// «свободно» («под вопросом» — занято), подряд не короче `duration`. «Нет данных» (`None`) окна не закрывает.
pub(crate) fn free_runs(rows: &[Option<Vec<Busy>>], range: Span, hours: &WorkHours, duration: i64) -> Vec<Span> {
    let known: Vec<Option<Vec<Busy>>> = rows.iter().filter(|r| r.is_some()).cloned().collect();
    // Никого не известно — окна по рабочему времени (строка «никто не занят»).
    let known = if known.is_empty() { vec![Some(Vec::new())] } else { known };
    let cells = grid(&known, range, STEP, hours);
    let n = cells.first().map_or(0, Vec::len);
    let free = |i: usize| cells.iter().all(|row| row.get(i) == Some(&Cell::Free));
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        if !free(i) {
            i += 1;
            continue;
        }
        let start = i;
        while i < n && free(i) {
            i += 1;
        }
        let span = Span::new(range.start + start as i64 * STEP, (range.start + i as i64 * STEP).min(range.end));
        if span.len() >= duration {
            out.push(span);
        }
    }
    out
}

/// Окно по ←/→ (S5-23): от начала `from` (окно под курсором или выбранное время) вправо — первое окно, начало
/// которого позже, влево — последнее, начало которого раньше; без `from` — первое или последнее. За краем дня и без
/// окон — `None`: курсор остаётся (день листают кнопки).
pub(crate) fn step_slot(runs: &[Span], from: Option<i64>, forward: bool) -> Option<Span> {
    match (from, forward) {
        (None, true) => runs.first().copied(),
        (None, false) => runs.last().copied(),
        (Some(t), true) => runs.iter().find(|r| r.start > t).copied(),
        (Some(t), false) => runs.iter().rev().find(|r| r.start < t).copied(),
    }
}

/// Что тянут у выбранного времени (S5-23, макет встречи): рамку целиком или ручку края.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Grip {
    Move,
    Start,
    End,
}

/// Выбранное время после сдвига на `dt` секунд за `grip` (перетаскивание мышью; Shift+←/→ — конец на шаг): края —
/// по шагу сетки от начала сетки `range`, внутри `range`, не короче шага.
pub(crate) fn dragged(grip: Grip, from: Span, dt: i64, range: Span, step: i64) -> Span {
    let step = step.max(MIN_STEP);
    let snap = |t: i64| range.start + ((t - range.start) as f64 / step as f64).round() as i64 * step;
    let len = from.len().max(step);
    match grip {
        Grip::Move => {
            let start = snap(from.start + dt).clamp(range.start, (range.end - len).max(range.start));
            Span::new(start, start + len)
        }
        Grip::Start => Span::new(snap(from.start + dt).clamp(range.start, from.end - step), from.end),
        Grip::End => Span::new(from.start, snap(from.end + dt).clamp(from.start + step, range.end.max(from.start + step))),
    }
}

/// Пустой «призрак» перетаскивания: рамка двигается сама.
struct DragGhost;

impl Render for DragGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

impl MeetingModel {
    pub(crate) fn new(start: DateTime, duration: i64) -> Self {
        Self {
            title: String::new(),
            location: String::new(),
            date: start.strftime("%d.%m.%Y").to_string(),
            start: start.strftime("%H:%M").to_string(),
            duration,
            attendees: String::new(),
            optional: String::new(),
            description: String::new(),
            from: None,
            status: Status::None,
            sending: None,
            editing: None,
        }
    }

    /// Вкладка правки своей встречи (S7-07б): поля — из события недели в поясе `tz`, «От» — организатор (встреча
    /// остаётся в своей учётной записи), участники — кроме меня-организатора.
    pub(crate) fn editing(e: &postman_calendar::Event, series: bool, tz: &TimeZone) -> Self {
        let start = jiff::Timestamp::from_second(e.span.start).map(|t| t.to_zoned(tz.clone()).datetime()).unwrap_or_default();
        let mut m = Self::new(start, e.span.len().max(MIN_STEP));
        m.title = e.title.clone();
        m.location = e.location.clone();
        m.description = e.description.clone();
        let list = |optional: bool| {
            e.participants
                .iter()
                .filter(|p| p.attendee && !p.owner && p.optional == optional && !p.email.is_empty())
                .map(|p| person_line(&p.name, &p.email))
                .collect::<Vec<_>>()
                .join("; ")
        };
        m.attendees = list(false);
        m.optional = list(true);
        m.from = e.organizer().map(|o| o.email.clone()).filter(|x| !x.is_empty());
        m.editing = Some(Editing { event: Box::new(e.clone()), series });
        m
    }

    pub(crate) fn field(&self, f: Field) -> &str {
        match f {
            Field::Title => &self.title,
            Field::Location => &self.location,
            Field::Date => &self.date,
            Field::Start => &self.start,
            Field::Attendees => &self.attendees,
            Field::Optional => &self.optional,
            Field::Description => &self.description,
        }
    }

    pub(crate) fn set(&mut self, f: Field, value: String) {
        let slot = match f {
            Field::Title => &mut self.title,
            Field::Location => &mut self.location,
            Field::Date => &mut self.date,
            Field::Start => &mut self.start,
            Field::Attendees => &mut self.attendees,
            Field::Optional => &mut self.optional,
            Field::Description => &mut self.description,
        };
        *slot = value;
        if !self.busy() {
            self.status = Status::None;
        }
    }

    pub(crate) fn busy(&self) -> bool {
        self.sending.is_some()
    }

    /// Локальное начало из полей «Дата» и «Начало».
    pub(crate) fn start_at(&self, today: Date) -> Option<DateTime> {
        Some(parse_date(&self.date, today)?.to_datetime(parse_time(&self.start)?))
    }

    /// Время встречи в Unix-секундах (рамка в помощнике).
    pub(crate) fn span(&self, tz: &TimeZone, today: Date) -> Option<Span> {
        let start = self.start_at(today)?.to_zoned(tz.clone()).ok()?.timestamp().as_second();
        Some(Span::new(start, start + self.duration))
    }

    /// Распознанные обязательные участники: (имя, адрес).
    pub(crate) fn people(&self) -> Vec<(String, String)> {
        parse_addresses(&self.attendees).0.into_iter().map(|a| (a.name, a.email)).collect()
    }

    /// Распознанные необязательные участники и переговорные.
    pub(crate) fn optional_people(&self) -> Vec<(String, String)> {
        parse_addresses(&self.optional).0.into_iter().map(|a| (a.name, a.email)).collect()
    }

    pub(crate) fn bad_addresses(&self) -> Vec<String> {
        let mut bad = parse_addresses(&self.attendees).1;
        bad.extend(parse_addresses(&self.optional).1);
        bad
    }

    /// Встреча для синхронизации; ошибки — до отправки, строкой во вкладке.
    pub(crate) fn meeting(&self, uid: String, tz: &TimeZone, today: Date) -> Result<Meeting, Status> {
        let bad = self.bad_addresses();
        if !bad.is_empty() {
            return Err(Status::BadAddresses(bad));
        }
        let start = self
            .start_at(today)
            .ok_or_else(|| Status::BadTime(format!("{} {}", self.date.trim(), self.start.trim())))?;
        // Зона без имени IANA (редко на Windows) — время в UTC: сервер его поймёт.
        let (start, time_zone) = match tz.iana_name() {
            Some(name) => (start, name.to_string()),
            None => {
                let utc = start.to_zoned(tz.clone()).map_err(|e| Status::BadTime(e.to_string()))?;
                (utc.with_time_zone(TimeZone::UTC).datetime(), "Etc/UTC".to_string())
            }
        };
        let required = self.people();
        let optional =
            self.optional_people().into_iter().filter(|(_, e)| !required.iter().any(|(_, r)| r.eq_ignore_ascii_case(e))).collect();
        Ok(Meeting {
            uid,
            title: self.title.trim().to_string(),
            location: self.location.trim().to_string(),
            description: self.description.trim().to_string(),
            start,
            time_zone,
            duration_secs: self.duration,
            attendees: required,
            optional,
            from: self.from.clone(),
        })
    }

    /// Начать отправку: команда для `Backend::send` или `None` (ошибка — в `status`).
    pub(crate) fn begin(&mut self, ticket: u64, uid: String, tz: &TimeZone, today: Date) -> Option<Command> {
        match self.meeting(uid, tz, today) {
            Ok(mut m) => {
                self.sending = Some(ticket);
                self.status = Status::Sending;
                Some(match &self.editing {
                    Some(ed) => {
                        m.uid = ed.event.uid.clone();
                        Command::UpdateMeeting { event: ed.event.clone(), meeting: Box::new(m), series: ed.series, ticket }
                    }
                    None => Command::CreateMeeting { meeting: Box::new(m), ticket },
                })
            }
            Err(s) => {
                self.status = s;
                None
            }
        }
    }

    /// Сервера нет (демо) или синхронизация остановилась: поля остаются.
    pub(crate) fn refused(&mut self) {
        self.sending = None;
        self.status = Status::NoServer;
    }

    /// Встреча создана: вкладка закрывается.
    pub(crate) fn on_created(&mut self, ticket: u64) -> bool {
        let mine = self.sending == Some(ticket);
        if mine {
            self.sending = None;
            self.status = Status::None;
        }
        mine
    }

    pub(crate) fn on_failed(&mut self, ticket: u64, error: String) -> bool {
        let mine = self.sending == Some(ticket);
        if mine {
            self.sending = None;
            self.status = Status::Failed(error);
        }
        mine
    }

    /// Выбрано окно: дата и начало — из него.
    pub(crate) fn set_slot(&mut self, s: Span, tz: &TimeZone) {
        if let Ok(t) = jiff::Timestamp::from_second(s.start) {
            let z = t.to_zoned(tz.clone());
            self.date = z.strftime("%d.%m.%Y").to_string();
            self.start = z.strftime("%H:%M").to_string();
        }
        self.duration = s.len().max(MIN_STEP);
    }

    /// Другой день: время остаётся.
    pub(crate) fn set_day(&mut self, date: Date) {
        self.date = date.strftime("%d.%m.%Y").to_string();
    }
}

// --- вкладки встречи у экрана ---

/// Встреча создана (итог для тоста), вкладка `tab` закрывается.
pub(crate) struct Created {
    pub(crate) title: String,
    pub(crate) invited: bool,
    /// Правка своей встречи (S7-07б): изменения разосланы.
    pub(crate) edited: bool,
    pub(crate) tab: u64,
}

#[derive(Default)]
pub(crate) struct Meetings {
    /// Вкладки встреч: номер вкладки и её view.
    tabs: Vec<(u64, Entity<MeetingView>)>,
    /// Открыть, когда будет окно (следующий кадр экрана): модели новых вкладок.
    pub(crate) pending: Vec<MeetingModel>,
}

impl Meetings {
    /// Событие синхронизации — вкладкам (каждая ищет свой тикет); созданные встречи — экрану.
    pub(crate) fn on_sync(&mut self, ev: &Event, cx: &mut App) -> Vec<Created> {
        let mut created = Vec::new();
        for (tab, view) in &self.tabs {
            view.update(cx, |m, cx| {
                if let Some(mut c) = m.on_sync(ev, cx) {
                    c.tab = *tab;
                    created.push(c);
                }
            });
        }
        created
    }

    /// Последняя открытая вкладка (снимки `--screens`).
    pub(crate) fn last(&self) -> Option<Entity<MeetingView>> {
        self.tabs.last().map(|(_, v)| v.clone())
    }

    pub(crate) fn view(&self, tab: u64) -> Option<&Entity<MeetingView>> {
        self.tabs.iter().find(|(t, _)| *t == tab).map(|(_, v)| v)
    }

    /// Название вкладки: «Новая встреча · тема» или «Изменение встречи · тема».
    pub(crate) fn title(&self, tab: u64, cx: &App) -> String {
        let model = self.view(tab).map(|v| &v.read(cx).model);
        let edit = model.is_some_and(|m| m.editing.is_some());
        match model.map(|m| m.title.trim().to_string()) {
            Some(t) if !t.is_empty() => tr!(if edit { "meeting-tab-edit-titled" } else { "meeting-tab-titled" }, title = t),
            _ => tr!(if edit { "meeting-title-edit" } else { "meeting-title-new" }),
        }
    }

    pub(crate) fn remove(&mut self, tab: u64) {
        self.tabs.retain(|(t, _)| *t != tab);
    }
}

/// Что вкладка встречи просит у экрана.
pub(crate) enum MeetingEvent {
    /// Закрыть вкладку (Esc, встреча создана).
    Close,
    /// Обновить название вкладки.
    Title,
}

impl Mailer {
    /// Новая встреча — вкладкой; откроется на следующем кадре (полям ввода нужно окно).
    pub(crate) fn open_meeting(&mut self, model: Option<MeetingModel>, cx: &mut Context<Self>) {
        self.load_address_book(cx);
        let now = jiff::Zoned::now();
        // Длительность новой встречи — «Настройки → Календарь» (S5-20).
        let duration = crate::prefs::get(cx).calendar.duration_secs();
        self.meetings.pending.push(model.unwrap_or_else(|| MeetingModel::new(default_start(&now), duration)));
        cx.notify();
    }

    /// Вкладки ждущих встреч (из `render`, где есть окно).
    pub(crate) fn create_meetings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for model in std::mem::take(&mut self.meetings.pending) {
            let me = self.reading.read(cx).me.clone().or_else(|| (!self.backend.is_live()).then(|| postman_demo::ME.to_string()));
            let open = Open {
                book: self.compose.book.clone(),
                mailer: cx.entity().downgrade(),
                live: self.backend.is_live(),
                rooms: self.contacts.read(cx).rooms(),
                me,
                senders: self.compose.own_senders(self.backend.is_live()),
                from: self.cal.read(cx).selected_account(),
            };
            let view = cx.new(|cx| MeetingView::new(model, open, window, cx));
            let tab = self.tabs.open(crate::tabs::TabKind::Meeting);
            let sub = cx.subscribe_in(&view, window, move |this: &mut Self, _, ev: &MeetingEvent, window, cx| match ev {
                MeetingEvent::Close => this.close_meeting(tab, window, cx),
                MeetingEvent::Title => cx.notify(),
            });
            self._subscriptions.push(sub);
            self.meetings.tabs.push((tab, view));
        }
        cx.notify();
    }

    /// Закрыть все вкладки встреч (снимки `--screens`).
    pub(crate) fn close_all_meetings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tabs: Vec<u64> = self.meetings.tabs.iter().map(|(t, _)| *t).collect();
        for tab in tabs {
            self.close_meeting(tab, window, cx);
        }
    }

    /// Закрыть вкладку встречи.
    pub(crate) fn close_meeting(&mut self, tab: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.meetings.remove(tab);
        self.tabs.close(tab);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Встречи созданы: тост, вкладка закрывается, неделя заново.
    pub(crate) fn meetings_created(&mut self, created: Vec<Created>, cx: &mut Context<Self>) {
        for c in &created {
            let title = if c.title.is_empty() { tr!("meeting-untitled") } else { c.title.clone() };
            let text = match (c.edited, c.invited) {
                (true, _) => tr!("meeting-updated", title = title),
                (false, true) => tr!("meeting-sent", title = title),
                (false, false) => tr!("meeting-created", title = title),
            };
            self.toasts.update(cx, |t, cx| t.push(Toast::Success, text, cx));
            self.meetings.remove(c.tab);
            self.tabs.close(c.tab);
        }
        if !created.is_empty() {
            self.cal.update(cx, |c, cx| c.refresh(cx));
            cx.notify();
        }
    }

    /// Сцена `meeting` для `--screens`: участники демо (один — без данных, один — внешний), переговорная и
    /// помощник. Живой ящик — участники из `POSTMAN_SCREENS_ATTENDEES`, время по умолчанию, занятость с сервера.
    pub(crate) fn open_meeting_screen(&mut self, cx: &mut Context<Self>) {
        if self.backend.is_live() {
            let duration = crate::prefs::get(cx).calendar.duration_secs();
            let mut model = MeetingModel::new(default_start(&jiff::Zoned::now()), duration);
            model.attendees = std::env::var("POSTMAN_SCREENS_ATTENDEES").unwrap_or_default();
            self.open_meeting(Some(model), cx);
            return;
        }
        let tz = crate::prefs::get(cx).calendar.zone();
        let monday = jiff::Timestamp::from_second(demo::week_start())
            .map(|t| monday_of(t.to_zoned(tz).date()))
            .unwrap_or_else(|_| monday_of(jiff::Zoned::now().date()));
        let mut model = MeetingModel::new(add_days(monday, 2).at(13, 0, 0, 0), 3600);
        model.title = "Обсуждение пилота".into(); // i18n-ok: образец встречи
        let colleagues = demo::colleagues();
        let person = |i: usize| colleagues.get(i).map(|(n, e, _)| format!("{n} <{e}>; ")).unwrap_or_default();
        model.attendees = format!("{}{}guest@partner.example; ", person(0), person(1));
        model.optional = format!("{}{}", person(2), "n.fedorova@corp.example; ");
        self.open_meeting(Some(model), cx);
    }
}

// --- вкладка встречи ---

struct Open {
    book: Book,
    mailer: WeakEntity<Mailer>,
    live: bool,
    /// Переговорные и ресурсы справочника: (имя, адрес).
    rooms: Vec<(String, String)>,
    me: Option<String>,
    /// Адреса «От» моих учётных записей (S7-03б): (адрес, как показывать); одна — строки «От» нет.
    senders: Vec<(String, String)>,
    /// «От» по умолчанию: учётная запись календаря, выбранного слева (событие другой моей учётной записи).
    from: Option<String>,
}

/// Занятость для помощника: неделя и адреса запроса, ответ.
#[derive(Default)]
struct Availability {
    /// Что спрошено: понедельник недели и адреса.
    asked: Option<(Date, Vec<String>)>,
    ticket: Option<u64>,
    me: Option<Vec<Busy>>,
    people: HashMap<String, Option<Vec<Busy>>>,
    error: Option<String>,
    /// Таймер запроса запущен.
    armed: bool,
}

/// Участник чипом: имя, адрес (у неразобранного — сам текст), разобран ли.
#[derive(Clone, Debug, PartialEq)]
struct Chip {
    name: String,
    email: String,
    bad: bool,
}

impl Chip {
    fn display(&self) -> String {
        if self.bad || self.name.is_empty() { self.email.clone() } else { format!("{} <{}>", self.name, self.email) }
    }

    fn shown(&self) -> String {
        if self.name.is_empty() { self.email.clone() } else { self.name.clone() }
    }
}

/// Чипы из текста адресов: разобранные и неразобранные куски.
fn chips_of(text: &str) -> Vec<Chip> {
    let (ok, bad) = parse_addresses(text);
    let mut out: Vec<Chip> = ok.into_iter().map(|a| Chip { name: a.name, email: a.email, bad: false }).collect();
    out.extend(bad.into_iter().map(|b| Chip { name: String::new(), email: b, bad: true }));
    out
}

/// Состояние участника в выбранное время.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Presence {
    Free,
    Busy,
    Tentative,
    Away,
    Unknown,
    External,
}

/// Список участников: обязательные (0) и необязательные (1).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Required,
    Optional,
}

impl Kind {
    fn index(self) -> usize {
        match self {
            Kind::Required => 0,
            Kind::Optional => 1,
        }
    }
}

pub(crate) struct MeetingView {
    pub(crate) model: MeetingModel,
    title: Entity<InputState>,
    location: Entity<InputState>,
    date: Entity<InputState>,
    start: Entity<InputState>,
    /// Набираемый участник (не чип): обязательные и необязательные.
    inputs: [Entity<InputState>; 2],
    description: Entity<TextareaState>,
    chips: [Vec<Chip>; 2],
    focus: FocusHandle,
    mailer: WeakEntity<Mailer>,
    book: Book,
    suggest: Vec<String>,
    /// Подсказки — для этого списка.
    suggest_for: Option<Kind>,
    selected: usize,
    tz: TimeZone,
    hours: WorkHours,
    live: bool,
    rooms: Vec<(String, String)>,
    me: Option<String>,
    /// «От» (S7-03б): адреса моих учётных записей и открыто ли меню.
    senders: Vec<(String, String)>,
    from_menu: bool,
    busy: Availability,
    /// Поля модели поменялись не от ввода (выбрано окно, день) — переписать поля при отрисовке.
    sync_inputs: bool,
    /// Помощник: только общие окна (занятость приглушена) или занятость каждого.
    pub(crate) only_free: bool,
    with_optional: bool,
    /// Цвет свободного времени (S5-20): окна, образец в легенде, точка «свободен».
    free_color: FreeColor,
    /// Окно под курсором ←/→ (S5-23): его начало; Enter ставит встречу в него.
    pub(crate) slot_cursor: Option<i64>,
    /// Шаг сетки, с (S5-20): перетаскивание выбранного времени и Shift+←/→.
    step: i64,
    /// Перетаскивание выбранного времени: где нажата мышь и какое было время.
    drag_from: Option<(Pixels, Span)>,
    desc_open: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<MeetingEvent> for MeetingView {}

impl MeetingView {
    fn new(model: MeetingModel, open: Open, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut line = |placeholder: &str, value: String, cx: &mut Context<Self>| {
            let placeholder = tr!(placeholder);
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(value))
        };
        let title = line("meeting-subject-placeholder", model.title.clone(), cx);
        let location = line("meeting-location-placeholder", model.location.clone(), cx);
        let date = line("meeting-date-placeholder", model.date.clone(), cx);
        let start = line("meeting-start-placeholder", model.start.clone(), cx);
        let required = line("meeting-attendees-placeholder", String::new(), cx);
        let optional = line("meeting-attendees-placeholder", String::new(), cx);
        let desc_text = model.description.clone();
        let description =
            cx.new(|cx| TextareaState::new(window, cx).placeholder(tr!("meeting-description-placeholder")).default_value(desc_text));
        let chips = [chips_of(&model.attendees), chips_of(&model.optional)];
        let mut subscriptions = Vec::new();
        for (f, state) in [
            (Field::Title, &title),
            (Field::Location, &location),
            (Field::Date, &date),
            (Field::Start, &start),
            (Field::Attendees, &required),
            (Field::Optional, &optional),
        ] {
            subscriptions.push(cx.subscribe_in(state, window, move |this, state, ev: &InputEvent, window, cx| {
                let value = state.read(cx).value().to_string();
                this.on_input(f, value, ev, window, cx);
            }));
        }
        subscriptions.push(cx.subscribe_in(&description, window, |this, state, ev: &InputEvent, _, cx| {
            if let InputEvent::Change = ev {
                let value = state.read(cx).value().to_string();
                this.model.set(Field::Description, value);
            }
        }));
        title.update(cx, |s, cx| s.focus(window, cx));
        // «Настройки → Календарь» (S5-20): пояс, рабочее время, необязательные, цвет свободного времени; цвет и часы
        // меняются на лету, пояс и флажок — у новой вкладки.
        let cal = crate::prefs::get(cx).calendar;
        subscriptions.push(crate::prefs::observe_with(cx, |this: &mut Self, cx| {
            let cal = crate::prefs::get(cx).calendar;
            this.free_color = cal.free_color;
            this.hours = cal.work_hours(this.tz.clone());
            this.step = i64::from(cal.step) * 60;
            cx.notify();
        }));
        let tz = cal.zone();
        let desc_open = !model.description.trim().is_empty();
        let mut model = model;
        if model.from.is_none() {
            model.from = open.from.clone();
        }
        let mut w = Self {
            model,
            title,
            location,
            date,
            start,
            inputs: [required, optional],
            description,
            chips,
            focus: cx.focus_handle(),
            mailer: open.mailer,
            book: open.book,
            suggest: Vec::new(),
            suggest_for: None,
            selected: 0,
            hours: cal.work_hours(tz.clone()),
            tz,
            live: open.live,
            rooms: open.rooms,
            me: open.me,
            senders: open.senders,
            from_menu: false,
            busy: Availability::default(),
            sync_inputs: false,
            only_free: true,
            with_optional: cal.with_optional,
            free_color: cal.free_color,
            slot_cursor: None,
            step: i64::from(cal.step) * 60,
            drag_from: None,
            desc_open,
            _subscriptions: subscriptions,
        };
        w.sync_model(cx);
        // Вкладка создаётся посреди обновления экрана (`Mailer`): запрос к нему — следующим шагом.
        cx.spawn(async move |this, cx| {
            let _ = this.update(cx, |w, cx| w.query(cx));
        })
        .detach();
        w
    }

    /// Занятость получена (снимки `--screens`).
    pub(crate) fn ready(&self) -> bool {
        self.busy.asked.is_some() && self.busy.ticket.is_none()
    }

    fn today(&self) -> Date {
        jiff::Zoned::now().with_time_zone(self.tz.clone()).date()
    }

    /// День встречи (поле «Дата»; не разобрано — сегодня).
    fn day(&self) -> Date {
        let today = self.today();
        parse_date(&self.model.date, today).unwrap_or(today)
    }

    /// Неделя встречи: понедельник по полю «Дата».
    fn monday(&self) -> Date {
        monday_of(self.day())
    }

    /// Текст участников модели = чипы + набираемое.
    fn sync_model(&mut self, cx: &mut Context<Self>) {
        for kind in [Kind::Required, Kind::Optional] {
            let i = kind.index();
            let pending = self.inputs[i].read(cx).value().to_string();
            let mut text: String = self.chips[i].iter().map(|c| format!("{}; ", c.display())).collect();
            text.push_str(&pending);
            let f = if kind == Kind::Required { Field::Attendees } else { Field::Optional };
            self.model.set(f, text);
        }
    }

    fn on_input(&mut self, f: Field, value: String, ev: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        let kind = match f {
            Field::Attendees => Some(Kind::Required),
            Field::Optional => Some(Kind::Optional),
            _ => None,
        };
        match ev {
            InputEvent::Change => {
                if let Some(kind) = kind {
                    // «;» или «,» — набранное до него становится чипами.
                    if let Some(cut) = value.rfind([';', ',']) {
                        let (head, tail) = value.split_at(cut);
                        let tail = tail[1..].trim_start().to_string();
                        self.chips[kind.index()].extend(chips_of(head));
                        self.inputs[kind.index()].update(cx, |s, cx| s.set_value(tail, window, cx));
                    }
                    self.sync_model(cx);
                    self.refresh_suggestions(cx);
                    // Справочник, который сервер отдаёт только поиском (Exchange, S7-08).
                    let typed = self.inputs[kind.index()].read(cx).value().to_string();
                    let token = current_token(&typed).to_owned();
                    let _ = self.mailer.update(cx, |m, cx| m.find_people(&token, cx));
                    self.schedule_query(cx);
                } else {
                    self.model.set(f, value);
                    if f == Field::Date {
                        self.schedule_query(cx);
                    }
                    if f == Field::Title {
                        cx.emit(MeetingEvent::Title);
                    }
                }
            }
            InputEvent::Focus => {
                self.suggest_for = kind;
                self.refresh_suggestions(cx);
            }
            InputEvent::PressEnter { .. } if kind.is_some() && !self.suggesting() => {
                if let Some(kind) = kind {
                    self.commit(kind, window, cx);
                }
            }
            _ => return,
        }
        cx.notify();
    }

    /// Набранное — чипом.
    fn commit(&mut self, kind: Kind, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.inputs[kind.index()].read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }
        self.chips[kind.index()].extend(chips_of(&text));
        self.inputs[kind.index()].update(cx, |s, cx| s.set_value("", window, cx));
        self.sync_model(cx);
        self.schedule_query(cx);
    }

    fn remove_chip(&mut self, kind: Kind, i: usize, cx: &mut Context<Self>) {
        if i < self.chips[kind.index()].len() {
            self.chips[kind.index()].remove(i);
            self.sync_model(cx);
            self.schedule_query(cx);
            cx.notify();
        }
    }

    /// Переговорная: выбрать — в необязательные (место — её имя, если пусто), снять — убрать.
    fn toggle_room(&mut self, room: (String, String), window: &mut Window, cx: &mut Context<Self>) {
        let list = &mut self.chips[Kind::Optional.index()];
        match list.iter().position(|c| c.email.eq_ignore_ascii_case(&room.1)) {
            Some(i) => {
                list.remove(i);
            }
            None => {
                list.push(Chip { name: room.0.clone(), email: room.1.clone(), bad: false });
                if self.model.location.trim().is_empty() {
                    self.model.location = room.0.clone();
                    self.location.update(cx, |s, cx| s.set_value(room.0, window, cx));
                }
            }
        }
        self.sync_model(cx);
        self.schedule_query(cx);
        cx.notify();
    }

    fn refresh_suggestions(&mut self, cx: &App) {
        self.selected = 0;
        self.suggest.clear();
        let Some(kind) = self.suggest_for else { return };
        let token = self.inputs[kind.index()].read(cx).value().to_string();
        let token = current_token(&token);
        if token.chars().count() < SUGGEST_FROM || token.contains('<') {
            return;
        }
        let now = jiff::Timestamp::now().as_second();
        self.suggest = self.book.borrow().suggest(token, SUGGESTIONS, now).into_iter().map(|e| e.display()).collect();
    }

    fn suggesting(&self) -> bool {
        self.suggest_for.is_some() && !self.suggest.is_empty()
    }

    fn accept(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(item), Some(kind)) = (self.suggest.get(i).cloned(), self.suggest_for) else { return };
        let value = accept_suggestion("", &item);
        self.chips[kind.index()].extend(chips_of(&value));
        self.inputs[kind.index()].update(cx, |s, cx| {
            s.set_value("", window, cx);
            s.focus(window, cx);
        });
        self.suggest.clear();
        self.sync_model(cx);
        self.schedule_query(cx);
        cx.notify();
    }

    fn move_suggestion(&mut self, delta: isize, cx: &mut Context<Self>) -> bool {
        if !self.suggesting() {
            return false;
        }
        self.selected = self.selected.saturating_add_signed(delta).min(self.suggest.len() - 1);
        cx.notify();
        true
    }

    /// Запрос занятости — когда участники и дата перестали меняться: не на каждую букву.
    fn schedule_query(&mut self, cx: &mut Context<Self>) {
        if std::mem::replace(&mut self.busy.armed, true) {
            return;
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(QUERY_DELAY).await;
            let _ = this.update(cx, |w, cx| {
                w.busy.armed = false;
                w.query(cx);
            });
        })
        .detach();
    }

    /// Все адреса участников (обязательные и необязательные, без неразобранных), без повторов.
    fn emails(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for c in self.chips.iter().flatten().filter(|c| !c.bad) {
            let e = c.email.to_lowercase();
            if !out.contains(&e) {
                out.push(e);
            }
        }
        out
    }

    /// Занятость недели встречи для участников и переговорных. Сервер — `Command::Availability`; демо — демо-коллеги.
    fn query(&mut self, cx: &mut Context<Self>) {
        let monday = self.monday();
        let mut emails = self.emails();
        for (_, e) in self.rooms.iter().take(ROOMS) {
            let e = e.to_lowercase();
            if !emails.contains(&e) {
                emails.push(e);
            }
        }
        let want = (monday, emails.clone());
        if self.busy.asked.as_ref() == Some(&want) {
            return;
        }
        self.busy.asked = Some(want);
        self.busy.error = None;
        if !self.live {
            let colleagues = demo::colleagues();
            self.busy.me = Some(demo::my_busy());
            self.busy.people = emails
                .into_iter()
                .map(|e| {
                    let busy =
                        colleagues.iter().find(|(_, mail, _)| mail.eq_ignore_ascii_case(&e)).map(|(_, _, b)| b.clone());
                    (e, busy)
                })
                .collect();
            self.busy.ticket = None;
            cx.notify();
            return;
        }
        let ticket = next_ticket();
        let (from, to) = (day_start(monday, &self.tz), day_start(add_days(monday, 7), &self.tz));
        let sent = self
            .mailer
            .update(cx, |m, _| m.backend.send(Command::Availability { ticket, emails, from, to, sender: self.model.from.clone() }))
            .unwrap_or(false);
        self.busy.ticket = sent.then_some(ticket);
        cx.notify();
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.model.busy() {
            return;
        }
        // Набранное, но не ставшее чипом, — тоже участник.
        self.commit(Kind::Required, window, cx);
        self.commit(Kind::Optional, window, cx);
        let ticket = next_ticket();
        let uid = format!("postman-{}-{ticket}", jiff::Timestamp::now().as_nanosecond());
        let today = self.today();
        let Some(command) = self.model.begin(ticket, uid, &self.tz, today) else {
            cx.notify();
            return;
        };
        let accepted = self
            .mailer
            .update(cx, |m, cx| {
                let ok = m.backend.send(command);
                if !ok {
                    m.needs_server(cx);
                }
                ok
            })
            .unwrap_or(false);
        if !accepted {
            self.model.refused();
        }
        cx.notify();
    }

    /// Итог по тикету: занятость; встреча создана — экран закроет вкладку и покажет тост.
    fn on_sync(&mut self, ev: &Event, cx: &mut Context<Self>) -> Option<Created> {
        match ev {
            Event::Availability { ticket, me, people, error } if self.busy.ticket == Some(*ticket) => {
                self.busy.ticket = None;
                self.busy.me = Some(me.clone());
                self.busy.people = people.iter().map(|(e, b)| (e.to_lowercase(), b.clone())).collect();
                self.busy.error = error.clone();
                cx.notify();
                None
            }
            Event::Me { address, .. } => {
                self.me = Some(address.email.clone());
                None
            }
            Event::PeopleFound { .. } if self.suggest_for.is_some() => {
                self.refresh_suggestions(cx);
                cx.notify();
                None
            }
            Event::MeetingCreated { ticket, .. } | Event::MeetingUpdated { ticket, .. } if self.model.on_created(*ticket) => {
                cx.notify();
                let invited = !self.model.people().is_empty() || !self.model.optional_people().is_empty();
                let edited = matches!(ev, Event::MeetingUpdated { .. });
                Some(Created { title: self.model.title.trim().to_string(), invited, edited, tab: 0 })
            }
            Event::MeetingFailed { ticket, error } => {
                if self.model.on_failed(*ticket, error.clone()) {
                    cx.notify();
                }
                None
            }
            _ => None,
        }
    }

    fn escape(&mut self, cx: &mut Context<Self>) {
        if self.suggesting() {
            self.suggest.clear();
            cx.notify();
        } else {
            cx.emit(MeetingEvent::Close);
        }
    }

    /// Сетка помощника — часы 8–19 дня встречи — и общие свободные окна в ней: я и обязательные, необязательные —
    /// по флажку.
    fn windows(&self) -> (Span, Vec<Span>) {
        let start = day_start(self.day(), &self.tz);
        let range = Span::new(start + FIRST_HOUR * 3600, start + (FIRST_HOUR + HOURS) * 3600);
        let runs = free_runs(&self.counted(), range, &self.hours, self.model.duration);
        (range, runs)
    }

    /// Чья занятость закрывает общие окна: я и обязательные, необязательные — по флажку.
    fn counted(&self) -> Vec<Option<Vec<Busy>>> {
        let optional_from = 1 + self.chips[0].iter().filter(|c| !c.bad).count();
        self.rows()
            .into_iter()
            .enumerate()
            .filter(|(i, _)| *i < optional_from || self.with_optional)
            .map(|(_, r)| r.busy)
            .collect()
    }

    /// ←/→ (S5-23): курсор — на соседнее общее окно; от окна под курсором, без него — от выбранного времени.
    pub(crate) fn step_cursor(&mut self, forward: bool, cx: &mut Context<Self>) {
        let (range, runs) = self.windows();
        let picked = self.model.span(&self.tz, self.today()).filter(|p| p.overlaps(&range)).map(|p| p.start);
        let from = self.slot_cursor.filter(|t| runs.iter().any(|r| r.start == *t)).or(picked);
        if let Some(r) = step_slot(&runs, from, forward) {
            self.slot_cursor = Some(r.start);
            cx.notify();
        }
    }

    /// Enter (S5-23): встреча — в окно под курсором.
    fn pick_cursor(&mut self, cx: &mut Context<Self>) {
        let (_, runs) = self.windows();
        if let Some(r) = self.slot_cursor.and_then(|t| runs.iter().find(|r| r.start == t)) {
            self.pick_slot(Span::new(r.start, r.start + self.model.duration), cx);
        }
    }

    /// Ctrl+Shift+B (S5-23, «Адресная книга» Outlook): к полю обязательных — подсказки из справочника и контактов.
    fn add_person(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.inputs[Kind::Required.index()].update(cx, |s, cx| s.focus(window, cx));
    }

    fn pick_slot(&mut self, s: Span, cx: &mut Context<Self>) {
        self.model.set_slot(s, &self.tz);
        self.sync_inputs = true;
        cx.notify();
    }

    fn pick_day(&mut self, date: Date, cx: &mut Context<Self>) {
        self.model.set_day(date);
        self.slot_cursor = None;
        self.sync_inputs = true;
        cx.notify();
    }

    /// PgUp/PgDn (S5-23, макет встречи): соседний день; время остаётся.
    fn shift_day(&mut self, days: i64, cx: &mut Context<Self>) {
        self.pick_day(add_days(self.day(), days), cx);
    }

    /// Выбранное время в сетке дня.
    fn picked_in(&self, range: Span) -> Option<Span> {
        self.model.span(&self.tz, self.today()).filter(|p| p.overlaps(&range))
    }

    /// Shift+←/→ (S5-23): конец выбранного времени — на шаг сетки.
    fn resize(&mut self, steps: i64, cx: &mut Context<Self>) {
        let (range, _) = self.windows();
        if let Some(p) = self.picked_in(range) {
            self.set_picked(dragged(Grip::End, p, steps * self.step, range, self.step), cx);
        }
    }

    fn set_picked(&mut self, span: Span, cx: &mut Context<Self>) {
        if self.model.span(&self.tz, self.today()) != Some(span) {
            self.model.set_slot(span, &self.tz);
            self.sync_inputs = true;
            cx.notify();
        }
    }

    /// Мышь тянет рамку или ручку выбранного времени: `x` — курсор, `width` — ширина дорожек.
    fn drag_to(&mut self, grip: Grip, x: Pixels, width: Pixels, cx: &mut Context<Self>) {
        let Some((x0, from)) = self.drag_from else { return };
        let w = f32::from(width);
        if w <= 0. {
            return;
        }
        let (range, _) = self.windows();
        let dt = (f32::from(x - x0) / w * range.len() as f32) as i64;
        self.set_picked(dragged(grip, from, dt, range, self.step), cx);
    }

    /// Нажата мышь на рамке или ручке: запомнить начало перетаскивания, фокус — вкладке.
    fn grab(&mut self, x: Pixels, window: &mut Window, cx: &mut Context<Self>) {
        let (range, _) = self.windows();
        self.drag_from = self.picked_in(range).map(|p| (x, p));
        self.focus.focus(window, cx);
    }

    fn status_line(&self) -> Option<(String, bool)> {
        Some(match &self.model.status {
            Status::None => return None,
            Status::Sending => (tr!("meeting-sending"), false),
            Status::BadTime(t) => (tr!("meeting-bad-time", text = t.clone()), true),
            Status::BadAddresses(b) => (tr!("meeting-bad-addresses", list = b.join(", ")), true),
            Status::NoServer => (tr!("meeting-no-server"), true),
            Status::Failed(e) => (tr!("meeting-failed", error = e.clone()), true),
        })
    }

    fn my_domain(&self) -> Option<String> {
        self.me.as_deref().and_then(|m| m.split_once('@')).map(|(_, d)| d.to_lowercase())
    }

    fn external(&self, email: &str) -> bool {
        match (self.my_domain(), email.split_once('@')) {
            (Some(mine), Some((_, d))) => !d.eq_ignore_ascii_case(&mine),
            _ => false,
        }
    }

    fn is_room(&self, email: &str) -> bool {
        self.rooms.iter().any(|(_, e)| e.eq_ignore_ascii_case(email))
    }

    /// Занятость участника: `None` — нет данных.
    fn busy_of(&self, email: &str) -> Option<Vec<Busy>> {
        self.busy.people.get(&email.to_lowercase()).cloned().flatten()
    }

    fn presence(&self, busy: Option<&Vec<Busy>>, email: Option<&str>, at: Option<Span>) -> Presence {
        let Some(busy) = busy else {
            return if email.is_some_and(|e| self.external(e)) { Presence::External } else { Presence::Unknown };
        };
        let Some(at) = at else { return Presence::Free };
        match busy.iter().filter(|b| b.span.overlaps(&at)).map(|b| b.kind).max() {
            Some(BusyKind::Unavailable) => Presence::Away,
            Some(BusyKind::Busy) => Presence::Busy,
            Some(BusyKind::Tentative) => Presence::Tentative,
            None => Presence::Free,
        }
    }

    fn label(text: String) -> Div {
        let l = crate::roles::now();
        div().w(px(LABEL_W)).flex_none().text_color(l.pm(Pm::LabelAccent)).child(text)
    }

    /// «От» (S7-03б, решение владельца 2026-10-23): то же поле, что в письме, — адрес и меню адресов моих учётных
    /// записей; учётная запись одна — строки нет.
    fn sender_row(&self, cx: &mut Context<Self>) -> Option<Div> {
        // Правка своей встречи: организатор не меняется (встреча остаётся в своей учётной записи).
        if self.senders.len() < 2 || self.model.editing.is_some() {
            return None;
        }
        let l = crate::roles::look(cx);
        let current = self.model.from.clone().or_else(|| self.senders.first().map(|(e, _)| e.clone())).unwrap_or_default();
        let shown = self.senders.iter().find(|(e, _)| e.eq_ignore_ascii_case(&current)).map_or(current.clone(), |(_, d)| d.clone());
        let button = crate::composer::quiet_plain("meeting-from", &l)
            .px(px(0.))
            .child(shown)
            .child(icon(crate::chrome::glyph::CHEVRON, 10.).text_color(l.text2()))
            .on_click(cx.listener(|this, _, _, cx| {
                this.from_menu = !this.from_menu;
                cx.notify();
            }));
        let menu = self.from_menu.then(|| {
            let items = self.senders.iter().enumerate().map(|(i, (email, display))| {
                let pick = email.clone();
                crate::composer::menu_item(ElementId::NamedInteger("meeting-from-item".into(), i as u64), &l, email.eq_ignore_ascii_case(&current))
                    .child(display.clone())
                    .on_click(cx.listener(move |this, _, _, cx| this.set_from(pick.clone(), cx)))
            });
            deferred(
                crate::composer::menu_box("meeting-from-menu", &l).left(px(LABEL_W + 8.)).children(items).on_mouse_down_out(cx.listener(
                    |this, _, _, cx| {
                        this.from_menu = false;
                        cx.notify();
                    },
                )),
            )
            .with_priority(1)
        });
        Some(
            Self::field_row(&l)
                .relative()
                .child(Self::label(tr!("shared-compose-from")))
                .child(div().child(button))
                .children(menu),
        )
    }

    /// Выбран адрес «От»: занятость — заново, путём его учётной записи.
    fn set_from(&mut self, email: String, cx: &mut Context<Self>) {
        self.from_menu = false;
        let main = self.senders.first().is_some_and(|(e, _)| e.eq_ignore_ascii_case(&email));
        self.model.from = (!main).then_some(email);
        self.busy.asked = None;
        self.query(cx);
        cx.notify();
    }

    fn field_row(l: &Look) -> Div {
        div().min_h(px(40.)).flex().items_center().gap(px(8.)).border_b_1().border_color(l.border())
    }

    /// Строка участников: чипы с занятостью в выбранное время и поле для нового.
    fn people_row(&self, kind: Kind, label: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let l = crate::roles::look(cx);
        let free = l.free(self.free_color);
        let at = self.model.span(&self.tz, self.today());
        let chips = self.chips[kind.index()].iter().enumerate().map(|(i, c)| {
            let p = if c.bad { Presence::Unknown } else { self.presence(self.busy_of(&c.email).as_ref(), Some(&c.email), at) };
            div()
                .id(("chip", kind.index() * 1000 + i))
                .h(px(24.))
                .flex()
                .items_center()
                .gap(px(6.))
                .pl(px(8.))
                .pr(px(4.))
                .rounded(px(12.))
                .bg(if c.bad { l.error_bg() } else { l.fill() })
                .when(c.bad, |d| d.text_color(l.error()))
                .child(presence_dot(p, 8., &l, free.line))
                .child(c.shown())
                .when(!c.bad && self.external(&c.email), |d| {
                    d.child(div().text_size(px(12.)).text_color(l.text2()).child(tr!("meeting-external-short")))
                })
                .when(self.is_room(&c.email), |d| d.child(icon(g::PIN, 11.).text_color(l.text2())))
                .child(
                    div()
                        .id(("chip-x", kind.index() * 1000 + i))
                        .size(px(18.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .cursor_pointer()
                        .hover(|d| d.bg(l.hover()))
                        .child(icon(g::CANCEL, 8.))
                        .on_click(cx.listener(move |this, _, _, cx| this.remove_chip(kind, i, cx))),
                )
                .tooltip({
                    let tip = tr!("meeting-presence-tip", name = c.shown(), state = presence_text(p));
                    move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                })
        });
        let input = div()
            .flex_1()
            .min_w(px(160.))
            .child(Input::new(&self.inputs[kind.index()]).appearance(false))
            .capture_action(cx.listener(|this, _: &MoveDown, _, cx| {
                if this.move_suggestion(1, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &MoveUp, _, cx| {
                if this.move_suggestion(-1, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, a: &Enter, window, cx| {
                if !a.secondary && this.suggesting() {
                    this.accept(this.selected, window, cx);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
                if this.suggesting() {
                    this.accept(this.selected, window, cx);
                    cx.stop_propagation();
                }
            }));
        let row = Self::field_row(&l)
            .py(px(4.))
            .child(Self::label(tr!(label)))
            .child(div().flex_1().min_w_0().flex().flex_wrap().items_center().gap(px(6.)).children(chips).child(input));
        let suggest = (self.suggest_for == Some(kind) && self.suggesting()).then(|| {
            let items = self.suggest.iter().enumerate().map(|(i, s)| {
                div()
                    .id(("suggest", i))
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .cursor_pointer()
                    .when(i == self.selected, |d| d.bg(l.picked()))
                    .hover(|d| d.bg(l.fill()))
                    .child(s.clone())
                    .on_click(cx.listener(move |this, _, window, cx| this.accept(i, window, cx)))
            });
            div()
                .ml(px(LABEL_W + 8.))
                .my(px(4.))
                .p_1()
                .flex()
                .flex_col()
                .border_1()
                .border_color(l.border())
                .rounded_md()
                .bg(l.c(pets_ui::theme::Role::LayerOverlay))
                .shadow_md()
                .children(items)
        });
        div().flex().flex_col().child(row).children(suggest)
    }

    fn rooms_row(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if self.rooms.is_empty() {
            return None;
        }
        let l = crate::roles::look(cx);
        let free = l.free(self.free_color);
        let at = self.model.span(&self.tz, self.today());
        let picked = |e: &str| self.chips[Kind::Optional.index()].iter().any(|c| c.email.eq_ignore_ascii_case(e));
        let items = self.rooms.iter().take(ROOMS).enumerate().map(|(i, (name, email))| {
            let on = picked(email);
            let p = self.presence(self.busy_of(email).as_ref(), None, at);
            let room = (name.clone(), email.clone());
            div()
                .id(("room", i))
                .h(px(26.))
                .px(px(10.))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded(px(13.))
                .border_1()
                .cursor_pointer()
                .when(on, |d| d.bg(l.picked()).border_color(l.accent()))
                .when(!on, |d| d.border_color(l.stroke()).hover(|d| d.bg(l.hover())))
                .child(presence_dot(p, 8., &l, free.line))
                .child(name.clone())
                .on_click(cx.listener(move |this, _, window, cx| this.toggle_room(room.clone(), window, cx)))
        });
        Some(
            Self::field_row(&l)
                .py(px(6.))
                .child(Self::label(tr!("meeting-rooms")))
                .child(div().flex_1().min_w_0().flex().flex_wrap().gap(px(6.)).children(items)),
        )
    }

    fn time_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let l = crate::roles::look(cx);
        let end = self
            .model
            .span(&self.tz, self.today())
            .map(|s| hm(s.end, &self.tz))
            .unwrap_or_else(|| "—".to_string());
        let boxed = |w: f32| div().w(px(w)).flex_none();
        Self::field_row(&l)
            .child(Self::label(tr!("meeting-when")))
            .child(boxed(124.).child(Input::new(&self.date)))
            .child(boxed(72.).child(Input::new(&self.start)))
            .child(div().text_color(l.text2()).child("–"))
            .child(
                boxed(64.)
                    .h(px(30.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(l.stroke())
                    .text_color(l.text2())
                    .child(end),
            )
            .child(div().ml(px(4.)).text_size(px(12.)).text_color(l.text2()).child(duration_text(self.model.duration)))
    }

    fn description_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let l = crate::roles::look(cx);
        let open = self.desc_open;
        let toggle = div()
            .id("desc-toggle")
            .flex()
            .items_center()
            .gap(px(6.))
            .text_color(l.accent_text())
            .cursor_pointer()
            .child(icon(if open { g::DOWN } else { g::NEXT }, 10.))
            .child(tr!(if open { "meeting-desc-hide" } else { "meeting-desc-add" }))
            .on_click(cx.listener(|this, _, _, cx| {
                this.desc_open = !this.desc_open;
                cx.notify();
            }));
        div()
            .flex()
            .flex_col()
            .child(
                Self::field_row(&l)
                    .child(Self::label(tr!("meeting-description")))
                    .child(toggle)
                    .when(!open, |d| d.child(div().text_size(px(12.)).text_color(l.text2()).child(tr!("meeting-desc-hint")))),
            )
            .when(open, |d| {
                d.child(
                    div()
                        .flex()
                        .py(px(8.))
                        .border_b_1()
                        .border_color(l.border())
                        .child(div().w(px(LABEL_W + 8.)).flex_none())
                        .child(div().flex_1().h(px(72.)).child(Textarea::new(&self.description).h(relative(1.)))),
                )
            })
    }

    /// Строки помощника: я, обязательные, необязательные.
    fn rows(&self) -> Vec<Row> {
        let mut rows = vec![Row {
            name: tr!("meeting-me"),
            sub: tr!("meeting-role-organizer"),
            email: None,
            busy: self.busy.me.clone(),
        }];
        for (kind, list) in [(Kind::Required, &self.chips[0]), (Kind::Optional, &self.chips[1])] {
            for c in list.iter().filter(|c| !c.bad) {
                let busy = self.busy_of(&c.email);
                let mut sub = if self.is_room(&c.email) {
                    tr!("meeting-role-room")
                } else if kind == Kind::Required {
                    tr!("meeting-role-required")
                } else {
                    tr!("meeting-role-optional")
                };
                if busy.is_none() {
                    sub = tr!("meeting-not-counted", role = sub);
                }
                rows.push(Row { name: c.shown(), sub, email: Some(c.email.clone()), busy });
            }
        }
        rows
    }

    fn assistant(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let l = crate::roles::look(cx);
        let free = l.free(self.free_color);
        let today = self.today();
        let day = self.day();
        let (range, runs) = self.windows();
        let frac = |t: i64| ((t - range.start) as f32 / range.len() as f32).clamp(0., 1.);
        let rows = self.rows();
        let unknown = rows.iter().filter(|r| r.busy.is_none()).count();
        let picked = self.model.span(&self.tz, today);
        let at = picked;
        // Шапка помощника: день, переключатели.
        let day_nav = div()
            .w(px(NAME_W + 84.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(crate::buttons::secondary("day-prev", &l).w(px(32.)).px_0().justify_center().child(icon(g::PREV, 12.)).on_click(
                cx.listener(move |this, _, _, cx| this.pick_day(add_days(day, -1), cx)),
            ))
            .child(div().w(px(170.)).text_center().font_weight(FontWeight::SEMIBOLD).child(long_day(day)))
            .child(crate::buttons::secondary("day-next", &l).w(px(32.)).px_0().justify_center().child(icon(g::NEXT, 12.)).on_click(
                cx.listener(move |this, _, _, cx| this.pick_day(add_days(day, 1), cx)),
            ))
            .child(
                crate::buttons::secondary("day-today", &l)
                    .child(tr!("cal-today"))
                    .on_click(cx.listener(move |this, _, _, cx| this.pick_day(today, cx))),
            );
        let toggle = |on: bool| {
            div()
                .w(px(40.))
                .h(px(20.))
                .flex_none()
                .relative()
                .rounded(px(10.))
                .when(on, |d| d.bg(l.accent()))
                .when(!on, |d| d.border_1().border_color(l.text2()))
                .child(
                    div()
                        .absolute()
                        .top(px(if on { 4. } else { 3. }))
                        .left(px(if on { 24. } else { 3. }))
                        .size(px(12.))
                        .rounded_full()
                        .bg(if on { l.on_accent() } else { l.text2() }),
                )
        };
        let only = crate::buttons::secondary("only-free", &l)
            .child(toggle(self.only_free))
            .child(tr!("meeting-only-free"))
            .on_click(cx.listener(|this, _, _, cx| {
                this.only_free = !this.only_free;
                cx.notify();
            }));
        let durations = div().flex().h(px(32.)).rounded(px(4.)).border_1().border_color(l.stroke()).overflow_hidden().children(
            DURATIONS.map(|secs| {
                let on = secs == self.model.duration;
                div()
                    .id(("duration", secs as usize))
                    .h_full()
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .when(on, |d| d.bg(l.accent()).text_color(l.on_accent()))
                    .when(!on, |d| d.hover(|d| d.bg(l.hover())))
                    .child(duration_text(secs))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model.duration = secs;
                        cx.notify();
                    }))
            }),
        );
        let check = div()
            .id("with-optional")
            .flex()
            .items_center()
            .gap(px(8.))
            .ml(px(8.))
            .cursor_pointer()
            .child(
                div()
                    .size(px(16.))
                    .rounded(px(4.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(self.with_optional, |d| d.bg(l.accent()).text_color(l.on_accent()).child(icon(g::CHECK, 10.)))
                    .when(!self.with_optional, |d| d.border_1().border_color(l.text2())),
            )
            .child(tr!("meeting-with-optional"))
            .on_click(cx.listener(|this, _, _, cx| {
                this.with_optional = !this.with_optional;
                cx.notify();
            }));
        let controls = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(24.))
            .pt(px(10.))
            .child(day_nav)
            .child(only)
            .child(div().ml(px(8.)).text_size(px(12.)).text_color(l.text2()).child(tr!("meeting-duration")))
            .child(durations)
            .child(check);
        let swatch = |bg: Hsla, border: Option<Hsla>| {
            div().w(px(16.)).h(px(8.)).rounded(px(2.)).bg(bg).when_some(border, |d, b| d.border_1().border_color(b))
        };
        // Неизвестная занятость и внешние — заливка с прозрачностью, как дорожки (S5-23).
        let (unknown_fill, external_fill) = (l.pm(Pm::FbUnknown).opacity(UNKNOWN_ALPHA), l.pm(Pm::FbExt).opacity(EXTERNAL_ALPHA));
        let legend: Vec<(Div, String)> = if self.only_free {
            vec![
                (swatch(l.none(), Some(free.line.opacity(FRAME_ALPHA))), tr!("meeting-legend-free")),
                (swatch(l.pm(Pm::FbMute), None), tr!("meeting-legend-busy")),
                (swatch(unknown_fill, None), tr!("meeting-legend-unknown")),
                (swatch(external_fill, None), tr!("meeting-legend-external")),
            ]
        } else {
            vec![
                (swatch(l.pm(Pm::FbBusy), None), tr!("cal-busy")),
                (swatch(l.pm(Pm::FbTentative), Some(l.pm(Pm::FbBusyLine))), tr!("cal-tentative")),
                (swatch(l.pm(Pm::FbOof), None), tr!("cal-unavailable")),
                (swatch(unknown_fill, None), tr!("meeting-legend-unknown")),
                (swatch(external_fill, None), tr!("meeting-legend-external")),
            ]
        };
        let note = if self.busy.ticket.is_some() {
            Some((tr!("meeting-busy-loading"), false))
        } else if let Some(e) = &self.busy.error {
            Some((tr!("meeting-busy-failed", error = e.clone()), true))
        } else {
            (unknown > 0).then(|| (tr!("meeting-not-counted-n", n = unknown), false))
        };
        let legend_row = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .h(px(28.))
            .px(px(24.))
            .child(div().w(px(NAME_W + 84.)).flex_none())
            .when_some(note, |d, (text, error)| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .text_size(px(12.))
                        .text_color(if error { l.error() } else { l.text2() })
                        .child(presence_dot(Presence::Unknown, 8., &l, free.line))
                        .child(text),
                )
            })
            .child(div().flex_1())
            .children(legend.into_iter().map(|(sw, text)| {
                div().flex().items_center().gap(px(8.)).ml(px(8.)).text_size(px(12.)).text_color(l.text2()).child(sw).child(text)
            }));
        // Сетка: имена слева, дорожки справа (позиции — доли дня 8–19).
        let block = |b: &Busy, label: Option<String>| -> Option<AnyElement> {
            let (s, e) = (b.span.start.max(range.start), b.span.end.min(range.end));
            if e <= s {
                return None;
            }
            let (bg, fg, border) = if self.only_free {
                match b.kind {
                    BusyKind::Tentative => (l.panel(), l.pm(Pm::FbMuteFg), Some(l.pm(Pm::FbMute))),
                    _ => (l.pm(Pm::FbMute), l.pm(Pm::FbMuteFg), None),
                }
            } else {
                match b.kind {
                    BusyKind::Busy => (l.pm(Pm::FbBusy), l.pm(Pm::FbBusyFg), None),
                    BusyKind::Tentative => (l.pm(Pm::FbTentative), l.pm(Pm::FbTentativeFg), Some(l.pm(Pm::FbBusyLine))),
                    BusyKind::Unavailable => (l.pm(Pm::FbOof), l.pm(Pm::FbOofFg), None),
                }
            };
            Some(
                div()
                    .absolute()
                    .top(px(6.))
                    .h(px(24.))
                    .left(relative(frac(s)))
                    .w(relative(frac(e) - frac(s)))
                    .px(px(2.))
                    .child(
                        div()
                            .size_full()
                            .rounded(px(4.))
                            .bg(bg)
                            .text_color(fg)
                            .when_some(border, |d, c| d.border_1().border_color(c))
                            .px(px(6.))
                            .text_size(px(12.))
                            .line_height(px(22.))
                            .overflow_hidden()
                            .truncate()
                            .children(label),
                    )
                    .into_any_element(),
            )
        };
        let summary: Vec<Busy> = postman_calendar::merge(self.counted().iter().flatten().flatten().copied().collect());
        let names = div()
            .w(px(NAME_W))
            .flex_none()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(24.))
                    .text_size(px(12.))
                    .text_color(l.text2())
                    .child(tr!("meeting-participants-n", n = rows.len())),
            )
            .child(
                div()
                    .h(px(ROW_H))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .border_b_1()
                    .border_color(l.stroke())
                    .child(div().size(px(24.)).rounded_full().flex().items_center().justify_center().bg(l.fill()).child(icon(g::PEOPLE, 13.)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .line_height(px(16.))
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(tr!("meeting-everyone")))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(l.text2())
                                    .child(tr!("meeting-known", known = rows.len() - unknown, n = rows.len())),
                            ),
                    ),
            )
            .children(rows.iter().enumerate().map(|(i, Row { name, sub, email, busy })| {
                let p = self.presence(busy.as_ref(), email.as_deref(), at);
                div()
                    .h(px(ROW_H))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .border_b_1()
                    .border_color(l.border())
                    .child(
                        div()
                            .relative()
                            .child(avatar(name, 24., &l).when(i == 0, |d| d.bg(l.pm(Pm::FbTentative)).text_color(l.pm(Pm::FbTentativeFg))))
                            .child(div().absolute().right(px(-2.)).bottom(px(-2.)).child(presence_dot(p, 10., &l, free.line))),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .line_height(px(16.))
                            .child(div().truncate().when(i == 0, |d| d.font_weight(FontWeight::SEMIBOLD)).child(name.clone()))
                            .child(div().truncate().text_size(px(12.)).text_color(l.text2()).child(sub.clone())),
                    )
            }))
            .child(
                div().h(px(ROW_H)).flex().items_center().child(
                    div()
                        .id("add-person")
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .text_color(l.accent_text())
                        .cursor_pointer()
                        .child(
                            div()
                                .size(px(24.))
                                .rounded_full()
                                .border_1()
                                .border_color(l.stroke())
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(icon(g::ADD, 11.)),
                        )
                        .child(tr!("meeting-add-person"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.inputs[0].update(cx, |s, cx| s.focus(window, cx));
                        })),
                ),
            );
        let hour_labels = div().h(px(24.)).relative().children((0..HOURS).map(|h| {
            div()
                .absolute()
                .top(px(5.))
                .left(relative(h as f32 / HOURS as f32))
                .pl(px(4.))
                .text_size(px(11.))
                .text_color(l.text2())
                .child(format!("{:02}:00", FIRST_HOUR + h))
        }));
        let lines = (0..HOURS * 2).map(|i| {
            div()
                .absolute()
                .top(px(24.))
                .bottom_0()
                .left(relative(i as f32 / (HOURS * 2) as f32))
                .w(px(1.))
                .bg(if i % 2 == 0 { l.stroke() } else { l.border() })
        });
        // Нерабочее время — подложкой.
        let work = self.hours.spans(range);
        let off: Vec<AnyElement> = {
            let mut edges = vec![range.start];
            for w in &work {
                edges.push(w.start);
                edges.push(w.end);
            }
            edges.push(range.end);
            edges
                .chunks(2)
                .filter(|p| p.len() == 2 && p[1] > p[0])
                .map(|p| {
                    div()
                        .absolute()
                        .top(px(24.))
                        .bottom_0()
                        .left(relative(frac(p[0])))
                        .w(relative(frac(p[1]) - frac(p[0])))
                        .bg(l.pm(Pm::CalOff))
                        .into_any_element()
                })
                .collect()
        };
        let summary_row = div()
            .h(px(ROW_H))
            .relative()
            .border_b_1()
            .border_color(l.stroke())
            .children(summary.iter().filter_map(|b| block(b, None)));
        let tracks = rows.iter().map(|Row { email, busy, .. }| {
            let external = email.as_deref().is_some_and(|e| self.external(e));
            let row = div().h(px(ROW_H)).relative().border_b_1().border_color(l.border());
            match busy {
                Some(list) => row.children(list.iter().filter_map(|b| block(b, None))),
                None => row
                    .bg(if external { external_fill } else { unknown_fill })
                    .child(
                        div()
                            .absolute()
                            .left(px(8.))
                            .top(px(6.))
                            .h(px(24.))
                            .px(px(8.))
                            .flex()
                            .items_center()
                            .rounded(px(4.))
                            .bg(l.panel())
                            .border_1()
                            .border_color(l.border())
                            .text_size(px(12.))
                            .text_color(l.text2())
                            .child(tr!(if external { "meeting-row-external" } else { "meeting-row-hidden" })),
                    ),
            }
        });
        // Общие окна (S5-23, макет встречи): тонкая мягкая рамка цвета свободного времени без заливки, подпись снизу
        // столбца; наведение и курсор ←/→ — рамка плотнее и свечение. В «занятости каждого» окна не рисуются, кроме
        // окна под курсором.
        let cursor = self.slot_cursor.filter(|t| runs.iter().any(|r| r.start == *t));
        let slots: Vec<Span> = runs.iter().copied().filter(|r| self.only_free || cursor == Some(r.start)).collect();
        let slot_cols = slots.iter().enumerate().map(|(n, s)| {
            let s = *s;
            let wide = s.len() >= 3600;
            let label = if wide { format!("{}–{}", hm(s.start, &self.tz), hm(s.end, &self.tz)) } else { hm(s.start, &self.tz) };
            let pick = Span::new(s.start, s.start + self.model.duration);
            let lit = cursor == Some(s.start);
            div()
                .id(("free", n))
                .absolute()
                .top(px(24.))
                .bottom_0()
                .left(relative(frac(s.start)))
                .w(relative(frac(s.end) - frac(s.start)))
                .flex()
                .items_end()
                .justify_center()
                .pb(px(12.))
                .rounded(px(4.))
                .border_1()
                .border_color(if lit { free.line } else { free.line.opacity(FRAME_ALPHA) })
                .text_size(px(12.))
                .text_color(free.fg)
                .cursor_pointer()
                .hover(|d| d.border_color(free.line))
                .when(lit, |d| d.children(glow(free.line)))
                .child(label)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.slot_cursor = Some(pick.start);
                    this.focus.focus(window, cx);
                    this.pick_slot(pick, cx);
                }))
        });
        // Выбранное время — рамка цвета свободного времени со свечением и ручками краёв (макет встречи): рамку
        // тянут целиком, ручки — начало и конец; шаг — шаг сетки. Рамка закрывает окна под собой от щелчка.
        let frame_h = ROW_H * (rows.len() + 1) as f32;
        let handle = |id: &'static str, grip: Grip| {
            div()
                .id(id)
                .absolute()
                .top(px(frame_h / 2. - 12.))
                .when(grip == Grip::Start, |d| d.left(px(-5.)))
                .when(grip == Grip::End, |d| d.right(px(-5.)))
                .w(px(8.))
                .h(px(24.))
                .rounded(px(4.))
                .bg(l.panel())
                .border_1()
                .border_color(free.line)
                .cursor(CursorStyle::ResizeLeftRight)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        this.grab(ev.position.x, window, cx);
                    }),
                )
                .on_drag(grip, |_, _, _, cx| cx.new(|_| DragGhost))
        };
        let frame = picked.filter(|p| p.overlaps(&range)).map(|p| {
            div()
                .id("picked")
                .absolute()
                .top(px(24.))
                .h(px(frame_h))
                .left(relative(frac(p.start)))
                .w(relative(frac(p.end) - frac(p.start)))
                .rounded(px(4.))
                .border_2()
                .border_color(free.line)
                .occlude()
                .cursor(CursorStyle::OpenHand)
                .children(glow(free.line))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, ev: &MouseDownEvent, window, cx| this.grab(ev.position.x, window, cx)),
                )
                .on_drag(Grip::Move, |_, _, _, cx| cx.new(|_| DragGhost))
                .child(handle("picked-start", Grip::Start))
                .child(handle("picked-end", Grip::End))
        });
        // Подсказка у сетки (S5-23): клавиши окон — из реестра, с учётом своих.
        let hint = slot_hint(cx);
        let grid = div()
            .id("assistant")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(hint.clone()).build(window, cx))
            // Щелчок по сетке — фокус вкладке (не полю): ←/→ и Enter — по окнам.
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.focus.focus(window, cx)))
            .px(px(24.))
            .child(
                div().flex().child(names).child(div().w(px(8.)).flex_none()).child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .relative()
                        .on_drag_move(cx.listener(|this, ev: &DragMoveEvent<Grip>, _, cx| {
                            let grip = *ev.drag(cx);
                            this.drag_to(grip, ev.event.position.x, ev.bounds.size.width, cx);
                        }))
                        .children(off)
                        .children(lines)
                        .child(hour_labels)
                        // Общие окна — под строками: подписи «нет данных» поверх них читаются.
                        .children(slot_cols)
                        .child(summary_row)
                        .children(tracks)
                        .children(frame),
                ),
            );
        div().flex().flex_col().flex_1().min_h_0().child(controls).child(legend_row).child(grid)
    }

    fn footer(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let l = crate::roles::look(cx);
        let external: Vec<String> = self.chips.iter().flatten().filter(|c| !c.bad && self.external(&c.email)).map(Chip::shown).collect();
        (!external.is_empty()).then(|| {
            div()
                .h(px(40.))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(24.))
                .border_t_1()
                .border_color(l.border())
                .child(icon(g::MAIL, 14.).text_color(l.text2()))
                .child(tr!("meeting-external-note"))
                .child(div().text_size(px(12.)).text_color(l.text2()).truncate().child(external.join(", ")))
        })
    }
}

/// Заливка дорожек «нет данных» и «внешний» (макет встречи: 12 % и 14 % цвета).
const UNKNOWN_ALPHA: f32 = 0.12;
const EXTERNAL_ALPHA: f32 = 0.14;

/// Рамка общего окна: тонкая, мягкая — цвет свободного времени на 55 % (макет встречи).
const FRAME_ALPHA: f32 = 0.55;
/// Свечение вокруг окна под курсором и выбранного времени: кольца наружу, от 45 % цвета к прозрачному (макет —
/// тень 12 px; тень gpui рисуется и под прозрачным окном, кольца — нет).
const GLOW: [(f32, f32); 4] = [(2., 0.4), (4., 0.24), (6., 0.13), (8., 0.06)];

/// Кольца свечения вокруг рамки (S5-23): поверх сетки, мышь проходит сквозь.
fn glow(line: Hsla) -> impl Iterator<Item = Div> {
    GLOW.into_iter().map(move |(d, a)| {
        div()
            .absolute()
            .top(px(-d))
            .bottom(px(-d))
            .left(px(-d))
            .right(px(-d))
            .rounded(px(4. + d))
            .border_2()
            .border_color(line.opacity(a))
    })
}

/// Строка помощника: имя, подпись (роль), адрес (у меня — нет), занятость (`None` — нет данных).
struct Row {
    name: String,
    sub: String,
    email: Option<String>,
    busy: Option<Vec<Busy>>,
}

/// Точка занятости участника.
fn presence_dot(p: Presence, size: f32, l: &Look, free: Hsla) -> Div {
    let d = div().size(px(size)).flex_none().rounded_full();
    match p {
        Presence::Free => d.bg(free),
        Presence::Busy => d.bg(l.pm(Pm::FbBusyLine)),
        Presence::Tentative => d.bg(l.pm(Pm::FbTentative)).border_1().border_color(l.pm(Pm::FbBusyLine)),
        Presence::Away => d.bg(l.pm(Pm::FbOofLine)),
        Presence::Unknown => d.bg(l.panel()).border_2().border_color(l.pm(Pm::FbUnknown)),
        Presence::External => d.bg(l.panel()).border_2().border_color(l.pm(Pm::FbExt)),
    }
}

/// «← → — по свободным окнам, Enter — выбрать»: клавиши команд встречи, как они назначены сейчас.
fn slot_hint(cx: &App) -> String {
    let keys = |id: &str| crate::bulk::key_hint(id, cx);
    tr!("meeting-slots-hint", prev = keys("meeting.prev_slot"), next = keys("meeting.next_slot"), pick = keys("meeting.pick_slot"))
}

fn presence_text(p: Presence) -> String {
    tr!(match p {
        Presence::Free => "meeting-presence-free",
        Presence::Busy => "cal-busy",
        Presence::Tentative => "cal-tentative",
        Presence::Away => "cal-unavailable",
        Presence::Unknown => "meeting-presence-unknown",
        Presence::External => "meeting-presence-external",
    })
}

impl Render for MeetingView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let l = crate::roles::look(cx);
        if std::mem::take(&mut self.sync_inputs) {
            for (f, state) in [(Field::Date, self.date.clone()), (Field::Start, self.start.clone())] {
                let value = self.model.field(f).to_string();
                state.update(cx, |s, cx| s.set_value(value, window, cx));
            }
            self.schedule_query(cx);
        }
        let busy = self.model.busy();
        let status = self.status_line().map(|(text, error)| {
            div().text_size(px(12.)).text_color(if error { l.error() } else { l.text2() }).truncate().child(text)
        });
        let toolbar = div()
            .h(px(48.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(16.))
            .border_b_1()
            .border_color(l.border())
            .child(
                crate::buttons::primary("send", &l)
                    .px(px(16.))
                    .child(icon(g::SEND, 14.))
                    .child(tr!("meeting-send"))
                    .when(busy, |d| d.opacity(0.5))
                    .tooltip({ let tip = crate::keytips::with_key(tr!("meeting-send-tip"), crate::keytips::key("meeting.send", cx)); move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx) })
                    .on_click(cx.listener(|this, _, window, cx| this.send(window, cx))),
            )
            .child(div().w(px(1.)).h(px(20.)).mx(px(4.)).bg(l.border()))
            .child(
                div()
                    .h(px(32.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(l.stroke())
                    .child(icon(g::GLOBE, 14.))
                    .child(zone_label(&self.tz)),
            )
            .child(div().flex_1())
            .children(status)
            // «Удалить черновик» (макет встречи, Ctrl+D): вкладка закрывается без отправки.
            .child(
                crate::buttons::secondary("discard", &l)
                    .w(px(32.))
                    .px_0()
                    .justify_center()
                    .child(icon(g::DELETE, 14.))
                    .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tr!("cmd-meeting-discard")).build(window, cx))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(MeetingEvent::Close))),
            );
        let fields = div()
            .flex_none()
            .flex()
            .flex_col()
            .px(px(24.))
            .children(self.sender_row(cx))
            .child(
                Self::field_row(&l)
                    .child(Self::label(tr!("meeting-subject")))
                    .child(div().flex_1().child(Input::new(&self.title).appearance(false))),
            )
            .child(self.people_row(Kind::Required, "meeting-required", cx))
            .child(self.people_row(Kind::Optional, "meeting-optional", cx))
            .children(self.rooms_row(cx))
            .child(
                Self::field_row(&l)
                    .child(Self::label(tr!("meeting-location")))
                    .child(div().flex_1().child(Input::new(&self.location).appearance(false))),
            )
            .child(self.time_row(cx))
            .child(self.description_row(cx));
        div()
            .key_context("Meeting")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &meeting::Send, window, cx| this.send(window, cx)))
            .on_action(cx.listener(|this, _: &meeting::Close, _, cx| this.escape(cx)))
            .on_action(cx.listener(|this, _: &meeting::NextSlot, _, cx| this.step_cursor(true, cx)))
            .on_action(cx.listener(|this, _: &meeting::PrevSlot, _, cx| this.step_cursor(false, cx)))
            .on_action(cx.listener(|this, _: &meeting::PickSlot, _, cx| this.pick_cursor(cx)))
            .on_action(cx.listener(|this, _: &meeting::Longer, _, cx| this.resize(1, cx)))
            .on_action(cx.listener(|this, _: &meeting::Shorter, _, cx| this.resize(-1, cx)))
            .on_action(cx.listener(|this, _: &meeting::PrevDay, _, cx| this.shift_day(-1, cx)))
            .on_action(cx.listener(|this, _: &meeting::NextDay, _, cx| this.shift_day(1, cx)))
            .on_action(cx.listener(|this, _: &meeting::Today, _, cx| {
                let today = this.today();
                this.pick_day(today, cx);
            }))
            .on_action(cx.listener(|_, _: &meeting::Discard, _, cx| cx.emit(MeetingEvent::Close)))
            .on_action(cx.listener(|this, _: &meeting::AddPerson, window, cx| this.add_person(window, cx)))
            .size_full()
            .flex()
            .flex_col()
            .bg(l.panel())
            .text_color(l.text())
            .text_size(px(13.))
            .child(toolbar)
            .child(fields)
            .child(self.assistant(cx))
            .children(self.footer(cx))
    }
}

/// Опись части (ADR 0005, `parts.rs`).
pub(crate) const PART: crate::part::Part = crate::part::Part { name: "meeting", commands: COMMANDS, ..crate::part::Part::NONE };

/// Вкладка встречи (S3-05, вкладкой — S5-11).
pub(crate) mod meeting {
    gpui_kit::actions!(
        meeting,
        [Send, Close, Discard, NextSlot, PrevSlot, PickSlot, Longer, Shorter, PrevDay, NextDay, Today, AddPerson]
    );
}

/// Команды части (`commands.rs`: контексты и `cmd`).
const COMMANDS: &[pets_commands::Command] = {
    use crate::commands::{MEETING_GRID, MEETING, cmd};
    &[
        cmd("meeting.send", &["ctrl-enter"], MEETING),
        cmd("meeting.close", &["escape"], MEETING),
        // Не Outlook (там Ctrl+D — «Удалить»): «Удалить черновик» встречи — Ctrl+D по макету (решение владельца
        // 2026-10-22, S5.md «Вопросы владельцу» №5).
        cmd("meeting.discard", &["ctrl-d"], MEETING),
        // Не Outlook (там подбор — мышью): ←/→ — по общим свободным окнам, Enter — встреча в окно (S5-23).
        cmd("meeting.next_slot", &["right"], MEETING_GRID),
        cmd("meeting.prev_slot", &["left"], MEETING_GRID),
        cmd("meeting.pick_slot", &["enter"], MEETING_GRID),
        // Макет встречи: Shift+←/→ — конец выбранного времени (длительность) с шагом сетки; PgUp/PgDn — день, Home —
        // сегодня, как в календаре.
        cmd("meeting.longer", &["shift-right"], MEETING_GRID),
        cmd("meeting.shorter", &["shift-left"], MEETING_GRID),
        cmd("meeting.prev_day", &["pageup"], MEETING_GRID),
        cmd("meeting.next_day", &["pagedown"], MEETING_GRID),
        cmd("meeting.today", &["home"], MEETING_GRID),
        // Outlook: Ctrl+Shift+B — «Адресная книга»; здесь — к полю участников с подсказками справочника.
        cmd("meeting.add_person", &["ctrl-shift-b"], MEETING),
    ]
};

#[cfg(test)]
mod tests {
    // Не `use super::*`: вместе с gpui пришёл бы атрибут `gpui::test`, он перекрывает `#[test]`.
    use super::{
        Command, Date, Grip, MeetingModel, Status, TimeZone, chips_of, default_start, dragged, free_runs, parse_date, parse_time,
        step_slot,
    };
    use postman_calendar::{Busy, BusyKind, Span, WorkHours};

    fn today() -> Date {
        Date::constant(2026, 10, 20)
    }

    fn filled() -> MeetingModel {
        let mut m = MeetingModel::new("2026-10-21T11:00:00".parse().unwrap(), 3600);
        m.title = " Планёрка ".into();
        m.location = "Переговорная 1".into();
        m.attendees = "User 3 <user3@itt.test>; ivan@corp.example; ".into();
        m
    }

    #[test]
    fn fields_become_a_new_event() {
        let tz = TimeZone::get("Europe/Moscow").unwrap();
        let mut m = filled();
        m.duration = 1800;
        m.optional = "Переговорная 1 <room1@itt.test>; IVAN@corp.example; ".into();
        m.description = " Повестка ".into();
        let meeting = m.meeting("u1".into(), &tz, today()).unwrap();
        assert_eq!(meeting.title, "Планёрка");
        assert_eq!(meeting.description, "Повестка");
        assert_eq!(meeting.time_zone, "Europe/Moscow");
        assert_eq!(meeting.start, "2026-10-21T11:00:00".parse().unwrap());
        assert_eq!(
            meeting.attendees,
            vec![
                ("User 3".to_string(), "user3@itt.test".to_string()),
                (String::new(), "ivan@corp.example".to_string())
            ]
        );
        // Необязательный, уже обязательный, не повторяется.
        assert_eq!(meeting.optional, vec![("Переговорная 1".to_string(), "room1@itt.test".to_string())]);
        // Дальше — `NewEvent::to_jmap`, как его ждёт Stalwart.
        let attendees = meeting.attendee_refs();
        let v = meeting.new_event("cal", ("User 1", "user1@itt.test"), &attendees).to_jmap();
        assert_eq!(v["duration"], "PT30M");
        assert_eq!(v["timeZone"], "Europe/Moscow");
        assert_eq!(v["participants"]["a0"]["calendarAddress"], "mailto:user3@itt.test");
        assert_eq!(v["participants"]["a1"]["calendarAddress"], "mailto:ivan@corp.example");
        assert_eq!(v["participants"]["a2"]["roles"]["optional"], true);
        assert_eq!(v["locations"]["l1"]["name"], "Переговорная 1");
        // 11:00 МСК — 08:00 UTC: так встреча стоит в помощнике.
        let start = "2026-10-21T08:00:00Z".parse::<jiff::Timestamp>().unwrap().as_second();
        assert_eq!(m.span(&tz, today()), Some(Span::new(start, start + 1800)));
    }

    #[test]
    fn errors_stay_in_the_tab() {
        let tz = TimeZone::UTC;
        let mut m = filled();
        m.attendees.push_str("petr@corp");
        assert!(m.begin(1, "u".into(), &tz, today()).is_none());
        assert_eq!(m.status, Status::BadAddresses(vec!["petr@corp".into()]));
        let mut m = filled();
        m.optional = "bad@".into();
        assert!(m.begin(1, "u".into(), &tz, today()).is_none());
        assert_eq!(m.status, Status::BadAddresses(vec!["bad@".into()]));
        let mut m = filled();
        m.start = "25:00".into();
        assert!(m.begin(1, "u".into(), &tz, today()).is_none());
        assert!(matches!(m.status, Status::BadTime(_)));
        assert!(!m.busy());
    }

    #[test]
    fn send_goes_through_a_ticket() {
        let tz = TimeZone::UTC;
        let mut m = filled();
        let Some(Command::CreateMeeting { meeting, ticket }) = m.begin(7, "u7".into(), &tz, today()) else {
            panic!("нет команды")
        };
        assert_eq!((ticket, meeting.uid.as_str()), (7, "u7"));
        assert!(m.busy() && m.status == Status::Sending);
        assert!(!m.on_created(8));
        assert!(m.on_failed(7, "offline".into()));
        assert_eq!(m.status, Status::Failed("offline".into()));
        assert!(m.begin(9, "u9".into(), &tz, today()).is_some());
        assert!(m.on_created(9) && !m.busy());
        // Демо: сервера нет — поля остаются, строка во вкладке.
        assert!(m.begin(10, "u10".into(), &tz, today()).is_some());
        m.refused();
        assert_eq!(m.status, Status::NoServer);
        assert!(!m.busy());
    }

    /// Правка своей встречи (S7-07б): поля — из события, участники — кроме меня-организатора, «От» — организатор;
    /// отправка — `UpdateMeeting` с UID события и выбором «вся серия».
    #[test]
    fn edit_sends_update() {
        use postman_calendar::{Event as CalEvent, EventStatus, FreeBusy, Participant, Partstat};
        let tz = TimeZone::UTC;
        let start = jiff::civil::date(2026, 10, 21).at(9, 30, 0, 0).to_zoned(tz.clone()).unwrap().timestamp().as_second();
        let p = |name: &str, email: &str, owner, optional| Participant {
            name: name.into(),
            email: email.into(),
            owner,
            attendee: true,
            optional,
            status: Partstat::Accepted,
        };
        let e = CalEvent {
            id: "e1".into(),
            uid: "uid-1".into(),
            calendar_ids: Vec::new(),
            title: "Итоги".into(),
            description: "Повестка".into(),
            location: "Каб. 1".into(),
            span: Span::new(start, start + 2700),
            all_day: false,
            status: EventStatus::Confirmed,
            free_busy: FreeBusy::Busy,
            participants: vec![p("Я", "me@x.test", true, false), p("Борис, ИТ", "b@x.test", false, false), p("", "room@x.test", false, true)],
            recurring: true,
            keywords: Vec::new(),
            sequence: 1,
            owner: None,
        };
        let mut m = MeetingModel::editing(&e, true, &tz);
        assert_eq!((m.date.as_str(), m.start.as_str(), m.duration), ("21.10.2026", "09:30", 2700));
        assert_eq!((m.attendees.as_str(), m.optional.as_str()), ("\"Борис, ИТ\" <b@x.test>", "room@x.test"));
        assert_eq!(m.from.as_deref(), Some("me@x.test"));
        m.title = "Итоги недели".into();
        let Some(Command::UpdateMeeting { event, meeting, series, ticket }) = m.begin(3, "new-uid".into(), &tz, today()) else {
            panic!("нет UpdateMeeting")
        };
        assert_eq!((event.id.as_str(), meeting.uid.as_str(), series, ticket), ("e1", "uid-1", true, 3));
        assert_eq!(meeting.attendees, [("Борис, ИТ".to_string(), "b@x.test".to_string())]);
        assert_eq!(meeting.title, "Итоги недели");
        assert!(m.on_created(3));
    }

    #[test]
    fn dates_times_and_slots() {
        assert_eq!(parse_date("21.10.2026", today()), Some(Date::constant(2026, 10, 21)));
        assert_eq!(parse_date("21.10", today()), Some(Date::constant(2026, 10, 21)));
        assert_eq!(parse_date("2026-10-21", today()), Some(Date::constant(2026, 10, 21)));
        assert_eq!(parse_date("31.02.2026", today()), None);
        assert_eq!(parse_date("завтра", today()), None);
        assert_eq!(parse_time("9:30"), Some(jiff::civil::Time::constant(9, 30, 0, 0)));
        assert_eq!(parse_time("14"), Some(jiff::civil::Time::constant(14, 0, 0, 0)));
        assert_eq!(parse_time("24:00"), None);
        let tz = TimeZone::UTC;
        let mut m = filled();
        let at = "2026-10-22T15:30:00Z".parse::<jiff::Timestamp>().unwrap().as_second();
        m.set_slot(Span::new(at, at + 5400), &tz);
        assert_eq!((m.date.as_str(), m.start.as_str(), m.duration), ("22.10.2026", "15:30", 5400));
    }

    #[test]
    fn default_start_is_in_work_hours() {
        let z = |s: &str| s.parse::<jiff::civil::DateTime>().unwrap().to_zoned(TimeZone::UTC).unwrap();
        // Вторник 10:10 → 10:30 того же дня; 7:00 → 9:00; 18:00 → среда 10:00; пятница вечер → понедельник.
        assert_eq!(default_start(&z("2026-10-20T10:10:00")), "2026-10-20T10:30:00".parse().unwrap());
        assert_eq!(default_start(&z("2026-10-20T07:00:00")), "2026-10-20T09:00:00".parse().unwrap());
        assert_eq!(default_start(&z("2026-10-20T18:00:00")), "2026-10-21T10:00:00".parse().unwrap());
        assert_eq!(default_start(&z("2026-10-23T17:45:00")), "2026-10-26T10:00:00".parse().unwrap());
    }

    #[test]
    fn typed_addresses_become_chips() {
        let chips = chips_of("Иван <ivan@corp.example>; petr@; anna@corp.example");
        let shown: Vec<(String, bool)> = chips.iter().map(|c| (c.display(), c.bad)).collect();
        assert_eq!(
            shown,
            [
                ("Иван <ivan@corp.example>".to_string(), false),
                ("anna@corp.example".to_string(), false),
                ("petr@".to_string(), true),
            ]
        );
        assert!(chips_of("  ").is_empty());
    }

    /// Общее свободное время: «нет данных» не закрывает окно, «под вопросом» — закрывает, окно не короче
    /// длительности, нерабочие часы — не окно; никого не известно — рабочее время дня.
    #[test]
    fn free_runs_edge_cases() {
        let hours = WorkHours::standard(TimeZone::UTC);
        let day = "2026-10-19T00:00:00Z".parse::<jiff::Timestamp>().unwrap().as_second();
        let h = |x: f64| day + (x * 3600.) as i64;
        let range = Span::new(h(8.), h(19.));
        let b = |a: f64, e: f64, kind| Busy { span: Span::new(h(a), h(e)), kind };
        let me = Some(vec![b(9., 10., BusyKind::Busy), b(13., 14., BusyKind::Busy)]);
        let colleague = Some(vec![b(10.5, 11., BusyKind::Tentative), b(15., 17.5, BusyKind::Unavailable)]);
        let runs = free_runs(&[me.clone(), colleague.clone(), None], range, &hours, 3600);
        assert_eq!(
            runs,
            [Span::new(h(11.), h(13.)), Span::new(h(14.), h(15.))],
            "10:00–10:30 короче часа, 17:30–18:00 тоже; 8–9 и 18–19 — нерабочие"
        );
        let short = free_runs(&[me.clone(), colleague], range, &hours, 1800);
        assert_eq!(short.first(), Some(&Span::new(h(10.), h(10.5))));
        assert!(short.contains(&Span::new(h(17.5), h(18.))));
        // Окно длиннее любого свободного — пусто; никого не известно — весь рабочий день.
        assert_eq!(free_runs(std::slice::from_ref(&me), range, &hours, 4 * 3600), [Span::new(h(14.), h(18.))]);
        assert!(free_runs(&[me], range, &hours, 5 * 3600).is_empty());
        assert_eq!(free_runs(&[None], range, &hours, 3600), [Span::new(h(9.), h(18.))]);
    }

    /// ←/→ по общим окнам (S5-23): от окна под курсором или выбранного времени к соседнему; край дня — на месте; нет
    /// окон — на месте; длительность меняет окна, и шаг идёт по новым.
    #[test]
    fn arrows_step_through_free_slots() {
        let hours = WorkHours::standard(TimeZone::UTC);
        let day = "2026-10-19T00:00:00Z".parse::<jiff::Timestamp>().unwrap().as_second();
        let h = |x: f64| day + (x * 3600.) as i64;
        let range = Span::new(h(8.), h(19.));
        let b = |a: f64, e: f64| Busy { span: Span::new(h(a), h(e)), kind: BusyKind::Busy };
        let me = Some(vec![b(9., 10.), b(10.5, 11.), b(13., 14.), b(17.5, 18.)]);
        let hour = free_runs(std::slice::from_ref(&me), range, &hours, 3600);
        assert_eq!(hour, [Span::new(h(11.), h(13.)), Span::new(h(14.), h(17.5))]);
        // Без курсора и выбора: → — первое окно, ← — последнее.
        assert_eq!(step_slot(&hour, None, true), Some(hour[0]));
        assert_eq!(step_slot(&hour, None, false), Some(hour[1]));
        // Выбрано 9:00 (до окон): → — первое; ← — края нет.
        assert_eq!(step_slot(&hour, Some(h(9.)), true), Some(hour[0]));
        assert_eq!(step_slot(&hour, Some(h(9.)), false), None);
        // Внутри окна 11–13 (встреча в 12:00): ← — его начало, → — следующее.
        assert_eq!(step_slot(&hour, Some(h(12.)), false), Some(hour[0]));
        assert_eq!(step_slot(&hour, Some(h(12.)), true), Some(hour[1]));
        // Край дня: с последнего окна → — на месте (None), с первого ← — тоже.
        assert_eq!(step_slot(&hour, Some(hour[1].start), true), None);
        assert_eq!(step_slot(&hour, Some(hour[0].start), false), None);
        // Нет окон.
        assert_eq!(step_slot(&[], None, true), None);
        assert_eq!(step_slot(&[], Some(h(11.)), false), None);
        // Полчаса — появляется 10:00–10:30: с 11:00 ← идёт в него.
        let half = free_runs(&[me], range, &hours, 1800);
        assert_eq!(half.first(), Some(&Span::new(h(10.), h(10.5))));
        assert_eq!(step_slot(&half, Some(h(11.)), false), Some(Span::new(h(10.), h(10.5))));
        assert_eq!(step_slot(&hour, Some(h(11.)), false), None, "часовые окна: левее 11:00 окон нет");
    }

    /// Перетаскивание выбранного времени и Shift+←/→ (S5-23, макет встречи): шаг сетки, края дня, не короче шага.
    #[test]
    fn dragging_the_picked_time_snaps_and_stays_in_the_day() {
        let h = |x: f64| (x * 3600.) as i64;
        let range = Span::new(h(8.), h(19.));
        let at = Span::new(h(10.), h(11.));
        let half = 1800;
        // Рамка целиком: сдвиг на 40 мин — к ближайшему шагу (30 мин), длина та же.
        assert_eq!(dragged(Grip::Move, at, 40 * 60, range, half), Span::new(h(10.5), h(11.5)));
        assert_eq!(dragged(Grip::Move, at, 10 * 60, range, half), at, "меньше полшага — на месте");
        // Шаг 15 мин — 40 мин становятся 45.
        assert_eq!(dragged(Grip::Move, at, 40 * 60, range, 900), Span::new(h(10.75), h(11.75)));
        // Края дня.
        assert_eq!(dragged(Grip::Move, at, h(-5.), range, half), Span::new(h(8.), h(9.)));
        assert_eq!(dragged(Grip::Move, at, h(20.), range, half), Span::new(h(18.), h(19.)));
        // Ручки: начало и конец, не короче шага и не за край.
        assert_eq!(dragged(Grip::Start, at, -h(1.5), range, half), Span::new(h(8.5), h(11.)));
        assert_eq!(dragged(Grip::Start, at, h(3.), range, half), Span::new(h(10.5), h(11.)));
        assert_eq!(dragged(Grip::End, at, h(0.5), range, half), Span::new(h(10.), h(11.5)), "Shift+→");
        assert_eq!(dragged(Grip::End, at, -h(0.5), range, half), Span::new(h(10.), h(10.5)), "Shift+←");
        assert_eq!(dragged(Grip::End, Span::new(h(10.), h(10.5)), -h(0.5), range, half), Span::new(h(10.), h(10.5)));
        assert_eq!(dragged(Grip::End, at, h(10.), range, half), Span::new(h(10.), h(19.)));
    }
}
