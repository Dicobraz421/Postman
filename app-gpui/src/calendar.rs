//! Календарь (S3-05, вид — S5-11 по макету «Календарь: рабочая неделя», виды — S5-21): командная строка раздела
//! ([`CalendarBar`]: «Создать встречу», «Сегодня», листание периода, диапазон, переключатель видов «День / Раб.
//! неделя / Неделя / Месяц» по правому краю сетки, поиск над панелью события), слева — месяц, мои календари,
//! категории и «Открыть мою занятость», в середине — сетка вида (весь день, часы 8–19, линия «сейчас», блоки событий;
//! месяц — клетки с полосами многодневных и строками событий), справа — подробности события с ответом на
//! приглашение, пересечениями и участниками (легенда-счётчик ответов) или найденное поиском.
//!
//! События — демо (`postman_demo::calendar`) или с сервера (`CalendarEvent/query` на показанный период, повторения
//! разворачивает сервер, команды S3-05). Поиск — по загруженным событиям сразу и `CalendarEvent/query` с
//! `filter.text` на год назад и вперёд (`Command::SearchEvents`). Дни вида, листание и раскладка месяца —
//! `cal_model`. Цвет блока — категория события (JSCalendar `keywords`/`categories` → таблица категорий,
//! `Categories::for_event`), иначе роли `app.event.*`; ждёт ответа — без заливки, пунктир и бледная полоса, «под
//! вопросом» — штрихованная полоса, отклонённое — не показывается; выбранное — свечение по контуру.
//!
//! Вид, первый день недели, шаг сетки и второй пояс — «Настройки → Календарь» (S5-20, `Prefs::calendar`).
//!
//! Своя кэшируемая view: перерисовывается только по своему `notify`. С сервером говорит событиями
//! [`CalendarAction`] (`Mailer` отдаёт их в `Backend`); события синхронизации приходят в [`CalendarView::on_sync`].

use std::time::Duration;

use gpui_kit::component::input::{Input, InputEvent, InputState, MoveDown, MoveUp};
use gpui_kit::prelude::*;
use gpui_kit::*;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use pets_i18n::tr;
use pets_ui::Level as Toast;
use pets_ui::theme::Metric;
use postman_calendar::{
    Busy, BusyKind, Event, EventStatus, Invitation, Owner, Participant, Partstat, Span, WorkHours, layout_day,
};
use postman_compose::{Address, Draft, parse_addresses};
use postman_core::labels::Categories;
use postman_core::prefs::{CalendarView as View, WeekStart};
use postman_core::sync::{Command, Event as SyncEvent};
use postman_demo::calendar as demo;

use crate::cal_model::{self as model, MonthItem};
use crate::chrome::icon;
use crate::roles::{Look, Pm};

pub(crate) const FIRST_HOUR: i64 = 8;
pub(crate) const HOURS: i64 = 11;
const HOUR_PX: f32 = 60.;
/// Шаг сетки помощника и выравнивание предложенных окон.
pub(crate) const STEP: i64 = 1800;
const GUTTER: f32 = 52.;
/// Шкала второго пояса слева от основной (S5-20, «Показывать второй пояс»).
const GUTTER2: f32 = 44.;
const SIDE_W: f32 = 236.;
const DETAILS_W: f32 = 344.;
/// Месяц: высота строки с числом, строки события и видимых дорожек в клетке (дальше — «+N ещё»).
const MONTH_NUM_H: f32 = 26.;
const MONTH_ITEM_H: f32 = 20.;
const MONTH_LANES: usize = 3;
/// Поиск на сервере — когда запрос перестал меняться; на год назад и вперёд.
const SEARCH_DELAY: Duration = Duration::from_millis(400);
const SEARCH_DAYS: i64 = 365;
/// Найденного в списке — не больше.
const RESULTS: usize = 50;

/// Значки Segoe Fluent Icons календаря, встречи и контактов.
pub(crate) mod g {
    pub(crate) const MORE: char = '\u{E712}';
    pub(crate) const PREV: char = '\u{E76B}';
    pub(crate) const NEXT: char = '\u{E76C}';
    pub(crate) const DOWN: char = '\u{E70D}';
    pub(crate) const ADD: char = '\u{E710}';
    pub(crate) const CALENDAR: char = '\u{E787}';
    pub(crate) const TODAY: char = '\u{E8D1}';
    pub(crate) const CLOCK: char = '\u{E823}';
    pub(crate) const REPEAT: char = '\u{E8EE}';
    pub(crate) const PIN: char = '\u{E707}';
    pub(crate) const PEOPLE: char = '\u{E716}';
    pub(crate) const PERSON: char = '\u{E77B}';
    pub(crate) const MAIL: char = '\u{E715}';
    pub(crate) const CHECK: char = '\u{E73E}';
    pub(crate) const CANCEL: char = '\u{E711}';
    pub(crate) const OVERLAP: char = '\u{E7C4}';
    pub(crate) const COPY: char = '\u{E8C8}';
    pub(crate) const EDIT: char = '\u{E70F}';
    pub(crate) const DELETE: char = '\u{E74D}';
    pub(crate) const GLOBE: char = '\u{E774}';
    pub(crate) const SEND: char = '\u{E724}';
    pub(crate) const HIDDEN: char = '\u{ED1A}';
    pub(crate) const SEARCH: char = '\u{E721}';
    pub(crate) const INFO: char = '\u{E946}';
}

/// Короткое имя дня недели: 0 — понедельник.
pub(crate) fn day_name(d: usize) -> String {
    tr!("cal-day", d = d)
}

/// Короткое имя дня недели даты.
fn weekday_name(date: Date) -> String {
    day_name(date.weekday().to_monday_zero_offset() as usize)
}

/// Понедельник недели, в которую попадает `date`.
pub(crate) fn monday_of(date: Date) -> Date {
    model::week_first(date, WeekStart::Monday)
}

pub(crate) fn add_days(date: Date, n: i64) -> Date {
    model::add_days(date, n)
}

/// Начало дня в зоне `tz` (Unix-секунды): смена летнего времени учтена, сутки не всегда 86 400 с.
pub(crate) fn day_start(date: Date, tz: &TimeZone) -> i64 {
    model::day_start(date, tz)
}

pub(crate) fn zoned(ts: i64, tz: &TimeZone) -> Option<jiff::Zoned> {
    jiff::Timestamp::from_second(ts).ok().map(|t| t.to_zoned(tz.clone()))
}

pub(crate) fn hm(ts: i64, tz: &TimeZone) -> String {
    zoned(ts, tz).map(|z| z.strftime("%H:%M").to_string()).unwrap_or_default()
}

/// «Вторник, 20 октября».
pub(crate) fn long_day(date: Date) -> String {
    tr!(
        "cal-long-day",
        wd = tr!("cal-weekday", d = date.weekday().to_monday_zero_offset()),
        day = date.day(),
        month = tr!("cal-month-of", m = date.month())
    )
}

/// «Ср, 21 окт.» — дата в списке найденного.
fn short_day(date: Date) -> String {
    tr!("cal-short-day", wd = weekday_name(date), day = date.day(), month = tr!("cal-month-of", m = date.month()))
}

/// «1 ч 30 мин».
pub(crate) fn duration_text(secs: i64) -> String {
    let (h, m) = (secs / 3600, secs % 3600 / 60);
    match (h, m) {
        (0, m) => tr!("cal-minutes", n = m),
        (h, 0) => tr!("cal-hours", n = h),
        (h, m) => format!("{} {}", tr!("cal-hours", n = h), tr!("cal-minutes", n = m)),
    }
}

/// «МСК (UTC+3)» — зона пользователя для подписей времени.
pub(crate) fn zone_label(tz: &TimeZone) -> String {
    let now = jiff::Zoned::now().with_time_zone(tz.clone());
    let offset = now.offset().seconds();
    let (sign, abs) = if offset < 0 { ('−', -offset) } else { ('+', offset) };
    let utc = if abs % 3600 == 0 { format!("UTC{sign}{}", abs / 3600) } else { format!("UTC{sign}{}:{:02}", abs / 3600, abs % 3600 / 60) };
    let short = now.strftime("%Z").to_string();
    if short.is_empty() || short.starts_with(['+', '-']) { utc } else { format!("{short} ({utc})") }
}

/// Диапазон недели в заголовке: «19–23 октября 2026», через месяц — «29 сентября – 3 октября 2026».
pub(crate) fn range_label(first: Date, last: Date) -> String {
    let month = |d: Date| tr!("cal-month-of", m = d.month());
    if first.year() != last.year() {
        format!("{} {} {} – {} {} {}", first.day(), month(first), first.year(), last.day(), month(last), last.year())
    } else if first.month() != last.month() {
        format!("{} {} – {} {} {}", first.day(), month(first), last.day(), month(last), last.year())
    } else if first == last {
        format!("{} {} {}", first.day(), month(first), first.year())
    } else {
        format!("{}–{} {} {}", first.day(), last.day(), month(last), last.year())
    }
}

/// Заголовок периода вида: день — «Среда, 21 октября 2026», неделя — диапазон, месяц — «Октябрь 2026».
pub(crate) fn period_label(view: View, anchor: Date, days: &[Date]) -> String {
    match (view, days.first(), days.last()) {
        (View::Day, _, _) => format!("{} {}", long_day(anchor), anchor.year()),
        (View::Month, _, _) => tr!("cal-month-title", month = tr!("cal-month", m = anchor.month()), year = anchor.year()),
        (_, Some(&a), Some(&b)) => range_label(a, b),
        _ => String::new(),
    }
}

/// События дня `date` (по началу): со временем и на весь день — индексы в `events`.
/// Ответы учётных записей профиля за период (S7-03б; основная — первой) → события (встреча, которая есть в двух
/// учётных записях, — один раз, из первой), календари (сначала другие мои учётные записи — у их ответа владелец первым,
/// `sync::owned_by`, — затем коллеги) и сколько первых календарей — мои.
pub(crate) fn merge_parts(parts: &[(Option<String>, Vec<Event>, Vec<Owner>)]) -> (Vec<Event>, Vec<Owner>, usize) {
    let mut events: Vec<Event> = Vec::new();
    for (_, list, _) in parts {
        for e in list {
            if !events.iter().any(|x| x.uid == e.uid && x.span.start == e.span.start) {
                events.push(e.clone());
            }
        }
    }
    let mine: Vec<Owner> = parts.iter().filter(|(a, _, _)| a.is_some()).filter_map(|(_, _, o)| o.first().cloned()).collect();
    let mut owners = mine.clone();
    for (_, _, list) in parts {
        for o in list {
            if !owners.iter().any(|x| x.key() == o.key()) {
                owners.push(o.clone());
            }
        }
    }
    (events, owners, mine.len())
}

pub(crate) fn day_events(events: &[Event], date: Date, tz: &TimeZone) -> (Vec<usize>, Vec<usize>) {
    let (from, to) = (day_start(date, tz), day_start(add_days(date, 1), tz));
    let today = |e: &Event| e.span.start >= from && e.span.start < to;
    let (mut timed, mut all_day) = (Vec::new(), Vec::new());
    for (i, e) in events.iter().enumerate().filter(|(_, e)| today(e)) {
        if e.all_day { all_day.push(i) } else { timed.push(i) }
    }
    (timed, all_day)
}

/// Мой ответ в событии: организатор — «принято»; меня нет — `None`.
pub(crate) fn my_part<'a>(e: &'a Event, me: Option<&str>) -> Option<&'a Participant> {
    let me = me?;
    e.participants.iter().find(|p| p.email.eq_ignore_ascii_case(me))
}

/// Как показать событие в сетке.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shown {
    /// Принято, моё.
    Normal,
    /// Ждёт моего ответа: без заливки, пунктир и бледная полоса.
    Pending,
    /// Под вопросом: штрихованная полоса.
    Tentative,
    /// Отклонено мной: в сетке не показываем.
    Hidden,
}

/// Вид события по моему ответу и его статусу.
pub(crate) fn event_look(e: &Event, me: Option<&str>) -> Shown {
    let mine = my_part(e, me);
    match mine.map(|p| (p.owner, p.status)) {
        Some((false, Partstat::Declined)) => Shown::Hidden,
        Some((false, Partstat::NeedsAction)) => Shown::Pending,
        Some((false, Partstat::Tentative)) => Shown::Tentative,
        _ if e.status == EventStatus::Tentative => Shown::Tentative,
        _ => Shown::Normal,
    }
}

/// Свечение выбранного события по контуру (DESIGN «Календарь»; макет — кольцо 2 px на 30 % и тень 10 px на 45 %
/// цвета календаря).
fn glow(color: Hsla) -> Vec<BoxShadow> {
    vec![
        BoxShadow { color: color.opacity(0.3), offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(2.), inset: false },
        BoxShadow { color: color.opacity(0.45), offset: point(px(0.), px(0.)), blur_radius: px(10.), spread_radius: px(3.), inset: false },
    ]
}

/// Что календарь просит у экрана: сервер, вкладка встречи, окно письма, тост.
pub(crate) enum CalendarAction {
    /// События периода с сервера (`Command::Events`).
    Load {
        from: i64,
        to: i64,
    },
    /// Поиск в календаре на сервере (`Command::SearchEvents`).
    Search {
        text: String,
        from: i64,
        to: i64,
    },
    /// Открыть коллеге мою занятость (`Command::ShareFreeBusy`).
    Share(String),
    /// Ответ на приглашение из подробностей события (`Command::RespondInvite`, путь S3-03).
    Respond {
        event: Box<Event>,
        status: Partstat,
    },
    /// Письмо участникам встречи.
    Write(Vec<Address>),
    /// «Изменить» своей встречи (S7-07б): вкладка встречи с её полями; `series` — у повтора вся серия.
    Edit {
        event: Box<Event>,
        series: bool,
    },
    /// «Отменить встречу» (S7-07б): `Command::CancelMeeting` → `MeetingCancelled` или `MeetingFailed`.
    Cancel {
        event: Box<Event>,
        series: bool,
        ticket: u64,
    },
    Toast(Toast, String),
}

/// Что спрашиваем о своей встрече строкой у подробностей (S7-07б).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Own {
    /// «Изменить» повтора: эту или всю серию.
    Edit,
    /// «Отменить встречу»: подтверждение (у повтора — эту или всю серию).
    Cancel,
}

/// Поиск в календаре: найденное сервером по тексту и ошибка.
#[derive(Default)]
struct Found {
    text: String,
    events: Vec<Event>,
    error: Option<String>,
}

pub(crate) struct CalendarView {
    tz: TimeZone,
    /// Вид и опорная дата: по ней — дни вида (`cal_model::days`).
    pub(crate) view: View,
    /// Вид по умолчанию из настроек (сменили там — календарь переходит на новый).
    pref_view: View,
    anchor: Date,
    week_start: WeekStart,
    /// Шаг сетки, мин (линии в часе).
    step: i64,
    /// Второй пояс — шкалой слева (`None` — выключен).
    second: Option<TimeZone>,
    /// Месяц слева (первое число).
    month: Date,
    events: Vec<Event>,
    live: bool,
    /// Ждём период с сервера: его `[from, to)`; пришёл другой — уже не нужен.
    pending: Option<(i64, i64)>,
    /// Показанный период пришёл с сервера (снимки `--screens --live` ждут его).
    pub(crate) loaded: bool,
    /// Подробности события сбоку: индекс в `events`.
    selected: Option<usize>,
    /// Открыть событие (uid, начало), когда придёт его период (переход из найденного).
    want: Option<(String, i64)>,
    /// Мой адрес: мой ответ в событиях.
    me: Option<String>,
    /// Таблица категорий: цвет событий по их меткам.
    categories: Categories,
    /// Коллеги, открывшие мне календарь (S5-22); первые `mine` — другие мои учётные записи (S7-03б): их календари —
    /// в «Мои календари», цветом и подписью владельца, как календари коллег.
    colleagues: Vec<Owner>,
    mine: usize,
    /// Ответы учётных записей профиля за показанный период (S7-03б): учётная запись, её события и календари коллег.
    parts: Vec<(Option<String>, Vec<Event>, Vec<Owner>)>,
    /// Период, который показывают `parts`.
    parts_range: Option<(i64, i64)>,
    /// Цвета календарей: ключ владельца (`""` — мой) → цвет `palette::CALENDAR`.
    colors: std::collections::BTreeMap<String, String>,
    /// Календарь, у которого открыт выбор цвета (ключ владельца).
    color_menu: Option<String>,
    /// Скрытые календари коллег и «Цвета по владельцу», «Имя владельца» — «Настройки → Календарь».
    hidden: Vec<String>,
    owner_colors: bool,
    owner_names: bool,
    /// Ответ ушёл, итога нет: (uid, ответ).
    answering: Option<(String, Partstat)>,
    /// Своя встреча (S7-07б): что спрашиваем одной строкой у подробностей — серию или подтверждение отмены.
    own: Option<Own>,
    /// Отмена, итог которой ждём: тикет.
    cancelling: Option<u64>,
    /// «Изменить ответ»: кнопки ответа снова видны.
    changing: bool,
    /// Снимки: «сейчас» — здесь, а не по часам (линия «сейчас» видна в рабочее время).
    pub(crate) clock: Option<i64>,
    share_open: bool,
    share: Entity<InputState>,
    /// Итог «Открыть»: текст и ошибка ли это.
    share_status: Option<(String, bool)>,
    sharing: Option<String>,
    hours: WorkHours,
    /// Поиск над панелью события: поле, запрос, найденное сервером, ждём ли его, строка под курсором.
    pub(crate) search: Entity<InputState>,
    query: String,
    found: Found,
    searching: Option<String>,
    search_gen: u64,
    /// Найденное показано (выбор результата открывает событие и прячет список до новой правки запроса).
    results_open: bool,
    cursor: usize,
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<CalendarAction> for CalendarView {}

impl CalendarView {
    /// Демо-события; живой ящик переключает [`CalendarView::go_live`].
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Пояс, рабочее время, вид, первый день недели, шаг и второй пояс — «Настройки → Календарь» (S5-20).
        let cal = crate::prefs::get(cx).calendar;
        let tz = cal.zone();
        let share = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("cal-share-placeholder")));
        let on_share = cx.subscribe_in(&share, window, |this: &mut Self, _, ev: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                this.submit_share(window, cx);
            }
        });
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("cal-search-placeholder")));
        let on_search = cx.subscribe_in(&search, window, |this: &mut Self, state, ev: &InputEvent, _, cx| match ev {
            InputEvent::Change => {
                let text = state.read(cx).value().to_string();
                this.set_query(text, cx);
            }
            InputEvent::PressEnter { .. } => this.open_cursor(cx),
            _ => {}
        });
        let anchor = zoned(demo::week_start(), &tz).map(|z| z.date()).unwrap_or_else(|| jiff::Zoned::now().date());
        let on_prefs = crate::prefs::observe_with(cx, |this: &mut Self, cx| this.prefs_changed(cx));
        let mut events = demo::my_events();
        events.extend(demo::other_events());
        events.extend(demo::colleague_events());
        events.extend(demo::second_events());
        events.sort_by_key(|e| e.span.start);
        // Вторая учётная запись демо (S7-03б) — первой среди календарей: она в «Моих календарях».
        let colleagues: Vec<Owner> = std::iter::once(demo::second_calendar()).chain(demo::shared_calendars()).collect();
        Self {
            hours: cal.work_hours(tz.clone()),
            view: cal.view,
            pref_view: cal.view,
            anchor,
            week_start: cal.week_start,
            step: i64::from(cal.step),
            second: second_zone(&cal),
            tz,
            month: anchor.first_of_month(),
            events,
            live: false,
            pending: None,
            loaded: false,
            selected: None,
            want: None,
            me: Some(postman_demo::ME.to_string()),
            categories: crate::labels::default_table(),
            colors: Self::assigned(&colleagues, &cal.calendar_colors),
            colleagues,
            mine: 1,
            parts: Vec::new(),
            parts_range: None,
            color_menu: None,
            hidden: cal.hidden_calendars.clone(),
            owner_colors: cal.owner_colors,
            owner_names: cal.owner_names,
            answering: None,
            own: None,
            cancelling: None,
            changing: false,
            clock: None,
            share_open: false,
            share,
            share_status: None,
            sharing: None,
            search,
            query: String::new(),
            found: Found::default(),
            searching: None,
            search_gen: 0,
            results_open: false,
            cursor: 0,
            scroll: ScrollHandle::new(),
            _subscriptions: vec![on_share, on_search, on_prefs],
        }
    }

    /// «Настройки → Календарь» изменились: рабочее время, шаг и второй пояс — сразу; пояс, первый день недели и
    /// вид по умолчанию — период заново.
    fn prefs_changed(&mut self, cx: &mut Context<Self>) {
        let cal = crate::prefs::get(cx).calendar;
        // Календари коллег (S5-22): цвета, скрытые, «Цвета по владельцу», «Имя владельца».
        let colors = Self::assigned(&self.colleagues, &cal.calendar_colors);
        let owners = (colors, cal.hidden_calendars.clone(), cal.owner_colors, cal.owner_names);
        if owners != (self.colors.clone(), self.hidden.clone(), self.owner_colors, self.owner_names) {
            (self.colors, self.hidden, self.owner_colors, self.owner_names) = owners;
            cx.notify();
        }
        let tz = cal.zone();
        let hours = cal.work_hours(tz.clone());
        let second = second_zone(&cal);
        let step = i64::from(cal.step);
        let same = hours == self.hours && tz == self.tz && cal.week_start == self.week_start && step == self.step && second == self.second;
        let view_changed = cal.view != self.pref_view;
        if same && !view_changed {
            return;
        }
        let before = self.range();
        self.tz = tz;
        self.hours = hours;
        self.week_start = cal.week_start;
        self.step = step;
        self.second = second;
        if view_changed {
            self.view = cal.view;
            self.pref_view = cal.view;
        }
        self.period_changed(before, cx);
        cx.notify();
    }

    /// Живой ящик: события — с сервера, демо-события убираются.
    pub(crate) fn go_live(&mut self, cx: &mut Context<Self>) {
        self.live = true;
        self.events.clear();
        self.colleagues.clear();
        self.mine = 0;
        self.parts.clear();
        self.parts_range = None;
        self.loaded = false;
        self.selected = None;
        self.me = None;
        self.anchor = self.now_zoned().date();
        self.month = self.anchor.first_of_month();
        cx.notify();
    }

    fn now(&self) -> i64 {
        self.clock.unwrap_or_else(|| jiff::Timestamp::now().as_second())
    }

    fn now_zoned(&self) -> jiff::Zoned {
        zoned(self.now(), &self.tz).unwrap_or_else(|| jiff::Zoned::now().with_time_zone(self.tz.clone()))
    }

    /// Таблица категорий аккаунта (цвета событий).
    pub(crate) fn set_categories(&mut self, table: Categories, cx: &mut Context<Self>) {
        if self.categories != table {
            self.categories = table;
            cx.notify();
        }
    }

    /// Дни показанного вида.
    pub(crate) fn days(&self) -> Vec<Date> {
        model::days(self.view, self.anchor, self.week_start, &self.hours.days)
    }

    fn range(&self) -> (i64, i64) {
        model::range(&self.days(), &self.tz)
    }

    /// Учётная запись выбранного события, если это другая моя учётная запись (S7-03б): «От» новой встречи.
    pub(crate) fn selected_account(&self) -> Option<String> {
        let owner = self.selected.and_then(|i| self.events.get(i))?.owner.as_ref()?;
        self.colleagues[..self.mine.min(self.colleagues.len())].iter().any(|o| o.key() == owner.key()).then(|| owner.email.clone())
    }

    /// Перечитать период с сервера (раздел открыт, период сменился, создана встреча).
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        if !self.live {
            return;
        }
        let (from, to) = self.range();
        self.pending = Some((from, to));
        cx.emit(CalendarAction::Load { from, to });
        cx.notify();
    }

    /// Поток синхронизации не принял команду: ждать нечего.
    pub(crate) fn not_sent(&mut self, cx: &mut Context<Self>) {
        self.pending = None;
        self.answering = None;
        self.cancelling = None;
        self.searching = None;
        if self.sharing.take().is_some() {
            self.share_status = None;
        }
        cx.notify();
    }

    /// PgUp / PgDn: период вида назад и вперёд.
    pub(crate) fn shift(&mut self, n: i64, cx: &mut Context<Self>) {
        self.go_to(model::shift(self.view, self.anchor, n), cx);
    }

    pub(crate) fn today(&mut self, cx: &mut Context<Self>) {
        self.go_to(self.now_zoned().date(), cx);
    }

    /// 1…4 и переключатель над сеткой: вид, та же опорная дата.
    pub(crate) fn set_view(&mut self, view: View, cx: &mut Context<Self>) {
        if self.view == view {
            return;
        }
        let before = self.range();
        self.view = view;
        self.period_changed(before, cx);
        cx.notify();
    }

    fn go_to(&mut self, date: Date, cx: &mut Context<Self>) {
        let before = self.range();
        self.anchor = date;
        self.month = date.first_of_month();
        self.period_changed(before, cx);
        cx.notify();
    }

    /// Период сменился (`before` — прежний): подробности закрыты, живой ящик — события заново.
    fn period_changed(&mut self, before: (i64, i64), cx: &mut Context<Self>) {
        if self.range() == before {
            return;
        }
        if !self.selected_shown() {
            self.selected = None;
        }
        self.changing = false;
        if self.live {
            self.events.clear();
            self.selected = None;
            self.loaded = false;
        }
        self.refresh(cx);
    }

    /// Выбранное событие — в показанном периоде (демо: события всех недель загружены сразу).
    fn selected_shown(&self) -> bool {
        let (from, to) = self.range();
        self.selected.and_then(|i| self.events.get(i)).is_some_and(|e| e.span.start < to && e.span.end > from)
    }

    pub(crate) fn on_sync(&mut self, ev: &SyncEvent, cx: &mut Context<Self>) {
        match ev {
            SyncEvent::EventsLoaded { from, to, events, colleagues, account }
                if self.pending == Some((*from, *to)) || self.parts_range == Some((*from, *to)) =>
            {
                self.pending = None;
                // Учётные записи профиля отвечают каждая за себя (S7-03б): период — сумма их ответов.
                if self.parts_range != Some((*from, *to)) {
                    self.parts.clear();
                    self.parts_range = Some((*from, *to));
                }
                match self.parts.iter_mut().find(|(a, _, _)| a == account) {
                    Some(part) => *part = (account.clone(), events.clone(), colleagues.clone()),
                    None => self.parts.push((account.clone(), events.clone(), colleagues.clone())),
                }
                // Основная — первой.
                self.parts.sort_by_key(|(a, _, _)| a.clone());
                let (merged, owners, mine) = merge_parts(&self.parts);
                self.colleagues = owners;
                self.mine = mine;
                self.remember_colors(cx);
                // Тот же период перечитан (push): подробности открытого события остаются, если оно есть; переход из
                // найденного — открыть искомое.
                let open = self.want.take().or_else(|| self.selected.and_then(|i| self.events.get(i)).map(|e| (e.uid.clone(), e.span.start)));
                self.events = merged;
                self.loaded = true;
                self.selected = open.and_then(|(uid, start)| {
                    self.events.iter().position(|e| e.uid == uid && e.span.start == start)
                });
            }
            SyncEvent::EventsFound { text, events, error } if self.searching.as_deref() == Some(text.as_str()) => {
                self.searching = None;
                self.found = Found { text: text.clone(), events: events.clone(), error: error.clone() };
            }
            SyncEvent::Me { address, .. } => self.me = Some(address.email.clone()),
            SyncEvent::InviteAnswered { uid, status, .. } => {
                if self.answering.as_ref().is_some_and(|(u, _)| u == uid) {
                    self.answering = None;
                }
                self.changing = false;
                let me = self.me.clone();
                for e in self.events.iter_mut().filter(|e| &e.uid == uid) {
                    if let Some(p) = e.participants.iter_mut().find(|p| me.as_deref().is_some_and(|m| p.email.eq_ignore_ascii_case(m))) {
                        p.status = *status;
                    }
                }
                self.refresh(cx);
            }
            SyncEvent::InviteFailed { uid, .. } if self.answering.as_ref().is_some_and(|(u, _)| u == uid) => {
                self.answering = None;
            }
            SyncEvent::MeetingCancelled { ticket, .. } if self.cancelling == Some(*ticket) => {
                self.cancelling = None;
                self.own = None;
                self.selected = None;
                cx.emit(CalendarAction::Toast(Toast::Success, tr!("cal-meeting-cancelled")));
                self.refresh(cx);
            }
            SyncEvent::MeetingFailed { ticket, error } if self.cancelling == Some(*ticket) => {
                self.cancelling = None;
                cx.emit(CalendarAction::Toast(Toast::Error, tr!("cal-cancel-failed", error = error.clone())));
            }
            SyncEvent::FreeBusyShared { email } => {
                self.sharing = None;
                let text = tr!("cal-share-done", email = email.clone());
                self.share_status = Some((text.clone(), false));
                cx.emit(CalendarAction::Toast(Toast::Success, text));
            }
            SyncEvent::CalendarFailed(error) => {
                self.pending = None;
                let text = tr!("cal-failed", error = error.clone());
                if self.sharing.take().is_some() {
                    self.share_status = Some((text.clone(), true));
                }
                cx.emit(CalendarAction::Toast(Toast::Error, text));
            }
            _ => return,
        }
        cx.notify();
    }

    fn submit_share(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let text = self.share.read(cx).value().to_string();
        let (ok, bad) = parse_addresses(&text);
        match (ok.as_slice(), bad.first()) {
            ([one], None) => {
                let email = one.email.clone();
                self.share_status = Some((tr!("cal-share-sending", email = email.clone()), false));
                self.sharing = Some(email.clone());
                cx.emit(CalendarAction::Share(email));
            }
            _ => {
                let shown = bad.first().cloned().unwrap_or_else(|| text.trim().to_string());
                self.share_status = Some((tr!("cal-share-bad", email = shown), true));
            }
        }
        cx.notify();
    }

    // --- поиск ---

    /// Ctrl+E в календаре.
    pub(crate) fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| s.focus(window, cx));
    }

    /// Запрос изменился: найденное в загруженном — сразу; живой ящик — сервер, когда запрос перестал меняться.
    fn set_query(&mut self, text: String, cx: &mut Context<Self>) {
        if text == self.query {
            return;
        }
        self.query = text;
        self.results_open = !self.query.trim().is_empty();
        self.cursor = 0;
        self.search_gen += 1;
        if self.live && self.results_open {
            let generation = self.search_gen;
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(SEARCH_DELAY).await;
                let _ = this.update(cx, |c, cx| {
                    if c.search_gen == generation {
                        c.search_server(cx);
                    }
                });
            })
            .detach();
        }
        cx.notify();
    }

    fn search_server(&mut self, cx: &mut Context<Self>) {
        let text = self.query.trim().to_string();
        if text.is_empty() || self.found.text == text {
            return;
        }
        let now = self.now();
        self.searching = Some(text.clone());
        cx.emit(CalendarAction::Search { text, from: now - SEARCH_DAYS * 86_400, to: now + SEARCH_DAYS * 86_400 });
        cx.notify();
    }

    /// Найденное по запросу: сервер (если уже ответил на этот запрос), иначе — загруженные события; без отклонённых,
    /// по времени начала, не больше [`RESULTS`].
    fn results(&self) -> Vec<Event> {
        let text = self.query.trim();
        if text.is_empty() {
            return Vec::new();
        }
        let me = self.me.as_deref();
        let from_server = self.live && self.found.text == text && self.found.error.is_none();
        let mut out: Vec<Event> = if from_server {
            self.found.events.clone()
        } else {
            self.events.iter().filter(|e| model::matches(e, text)).cloned().collect()
        };
        out.retain(|e| event_look(e, me) != Shown::Hidden);
        out.sort_by_key(|e| (e.span.start, e.span.end));
        out.dedup_by(|a, b| a.uid == b.uid && a.span.start == b.span.start);
        out.truncate(RESULTS);
        out
    }

    /// ↑/↓ в поле поиска: строка найденного под курсором. `false` — списка нет.
    pub(crate) fn step_result(&mut self, delta: isize, cx: &mut Context<Self>) -> bool {
        let n = if self.results_open { self.results().len() } else { 0 };
        if n == 0 {
            return false;
        }
        self.cursor = (self.cursor as isize + delta).clamp(0, n as isize - 1) as usize;
        cx.notify();
        true
    }

    /// Enter в поле поиска: к событию под курсором.
    fn open_cursor(&mut self, cx: &mut Context<Self>) {
        if let Some(e) = self.results().get(self.cursor).cloned() {
            self.open_result(e, cx);
        }
    }

    /// К найденному событию: его день в виде (неделя, месяц — как есть), подробности — его.
    fn open_result(&mut self, e: Event, cx: &mut Context<Self>) {
        self.results_open = false;
        let date = model::dates(&e, &self.tz).0;
        let key = (e.uid.clone(), e.span.start);
        self.go_to(date, cx);
        match self.events.iter().position(|x| x.uid == key.0 && x.span.start == key.1) {
            Some(i) => self.selected = Some(i),
            None if self.live => self.want = Some(key),
            None => {}
        }
        self.changing = false;
        cx.notify();
    }

    /// Esc: очистить поиск. `false` — нечего.
    pub(crate) fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.query.is_empty() && !self.results_open {
            return false;
        }
        self.search.update(cx, |s, cx| s.set_value("", window, cx));
        self.query.clear();
        self.results_open = false;
        self.found = Found::default();
        self.searching = None;
        cx.notify();
        true
    }

    /// Снимки (`calendar-search`): запрос как набранный.
    pub(crate) fn type_query(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| s.set_value(text, window, cx));
        self.set_query(text.to_string(), cx);
    }

    /// Найденное готово к снимку: список открыт, сервер (если есть) ответил.
    pub(crate) fn results_ready(&self) -> bool {
        self.results_open && self.searching.is_none() && !self.results().is_empty()
    }

    // --- выбор ---

    /// Подробности события с самым большим числом участников в показанном периоде (снимок `event`).
    pub(crate) fn select_busiest(&mut self, cx: &mut Context<Self>) {
        let me = self.me.clone();
        let (from, to) = self.range();
        self.selected = (0..self.events.len())
            .filter(|&i| event_look(&self.events[i], me.as_deref()) != Shown::Hidden)
            .filter(|&i| self.events[i].span.start < to && self.events[i].span.end > from)
            .max_by_key(|&i| (self.events[i].participants.len(), std::cmp::Reverse(i)));
        cx.notify();
    }

    /// Подробности первого события из календаря коллеги (снимок `calendar-colleagues`), иначе — самого людного.
    pub(crate) fn select_colleague(&mut self, cx: &mut Context<Self>) {
        match self.events.iter().position(|e| e.owner.is_some() && !e.all_day) {
            Some(i) => {
                self.selected = Some(i);
                cx.notify();
            }
            None => self.select_busiest(cx),
        }
    }

    /// Подробности приглашения, которое ждёт моего ответа (снимок `calendar`), иначе — самого людного события.
    pub(crate) fn select_pending(&mut self, cx: &mut Context<Self>) {
        let me = self.me.clone();
        let (from, to) = self.range();
        let pending = (0..self.events.len()).find(|&i| {
            let e = &self.events[i];
            event_look(e, me.as_deref()) == Shown::Pending && e.span.start < to && e.span.end > from
        });
        match pending {
            Some(i) => {
                self.selected = Some(i);
                cx.notify();
            }
            None => self.select_busiest(cx),
        }
    }

    /// Снимки: опорная дата — понедельник демо-недели (как при запуске).
    pub(crate) fn go_to_demo_week(&mut self, cx: &mut Context<Self>) {
        let date = zoned(demo::week_start(), &self.tz).map(|z| z.date()).unwrap_or(self.anchor);
        self.go_to(date, cx);
    }

    /// Снимки: опорная дата — `n` дней от понедельника демо-недели.
    pub(crate) fn go_to_demo_day(&mut self, n: i64, cx: &mut Context<Self>) {
        let date = zoned(demo::week_start(), &self.tz).map(|z| z.date()).unwrap_or(self.anchor);
        self.go_to(add_days(date, n), cx);
    }

    pub(crate) fn close_details(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        cx.notify();
    }

    /// Своя занятость с сегодня на неделю вперёд (S6-08, «Время в календаре»): загруженные события, кроме
    /// свободных и отменённых.
    pub(crate) fn busy_today(&self) -> Vec<Busy> {
        let now = self.clock.unwrap_or_else(|| jiff::Timestamp::now().as_second());
        let Some(today) = zoned(now, &self.tz).map(|z| z.date()) else { return Vec::new() };
        let (from, to) = (day_start(today, &self.tz), day_start(add_days(today, 8), &self.tz));
        let day = Span::new(from, to);
        self.events
            .iter()
            .filter(|e| e.span.overlaps(&day) && !e.all_day)
            .filter(|e| e.free_busy == postman_calendar::FreeBusy::Busy && e.status != postman_calendar::EventStatus::Cancelled)
            .map(|e| Busy {
                span: e.span,
                kind: if e.status == postman_calendar::EventStatus::Tentative { BusyKind::Tentative } else { BusyKind::Busy },
            })
            .collect()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub(crate) fn selected(&self) -> bool {
        self.selected.is_some()
    }

    fn select(&mut self, i: usize, cx: &mut Context<Self>) {
        self.selected = if self.selected == Some(i) { None } else { Some(i) };
        self.changing = false;
        self.own = None;
        self.results_open = false;
        cx.notify();
    }

    fn respond(&mut self, i: usize, status: Partstat, cx: &mut Context<Self>) {
        let Some(e) = self.events.get(i) else { return };
        if self.answering.is_some() {
            return;
        }
        self.answering = Some((e.uid.clone(), status));
        cx.emit(CalendarAction::Respond { event: Box::new(e.clone()), status });
        cx.notify();
    }

    /// «Изменить» или «Отменить встречу» своей встречи: у повтора — сначала одной строкой «эту или всю серию», у
    /// отмены — подтверждение одной строкой (модальных окон нет).
    fn own_step(&mut self, step: Own, cx: &mut Context<Self>) {
        let Some(e) = self.selected.and_then(|i| self.events.get(i)) else { return };
        if step == Own::Edit && !e.recurring {
            cx.emit(CalendarAction::Edit { event: Box::new(e.clone()), series: false });
            self.own = None;
        } else {
            self.own = Some(step);
        }
        cx.notify();
    }

    /// Ответ на вопрос строки: `series` — у повтора вся серия.
    fn own_answer(&mut self, series: bool, cx: &mut Context<Self>) {
        let Some(e) = self.selected.and_then(|i| self.events.get(i)).cloned() else { return };
        match self.own.take() {
            Some(Own::Edit) => cx.emit(CalendarAction::Edit { event: Box::new(e), series }),
            Some(Own::Cancel) if self.cancelling.is_none() => {
                let ticket = crate::meeting::next_ticket();
                self.cancelling = Some(ticket);
                self.own = Some(Own::Cancel);
                cx.emit(CalendarAction::Cancel { event: Box::new(e), series, ticket });
            }
            other => self.own = other,
        }
        cx.notify();
    }

    /// Строка вопроса о своей встрече: серия («Только эту» / «Всю серию») или подтверждение отмены.
    fn own_line(&self, e: &Event, step: Own, l: &Look, cx: &mut Context<Self>) -> Div {
        let busy = self.cancelling.is_some();
        let cancel = step == Own::Cancel;
        let answer = |id: &'static str, text: String, strong: bool, act: Option<bool>, cx: &mut Context<Self>| {
            div()
                .id(id)
                .flex_none()
                .px_3()
                .py_0p5()
                .rounded_md()
                .cursor_pointer()
                .when(strong, |d| d.bg(l.error_bg()).text_color(l.error()).font_weight(FontWeight::SEMIBOLD))
                .when(!strong, |d| d.bg(l.fill()).hover(|d| d.bg(l.pressed())))
                .child(text)
                .on_click(cx.listener(move |this, _, _, cx| match act {
                    Some(series) => this.own_answer(series, cx),
                    None => {
                        this.own = None;
                        cx.notify();
                    }
                }))
        };
        let question = match (busy, cancel, e.recurring) {
            (true, _, _) => tr!("cal-cancelling"),
            (false, true, true) => tr!("cal-cancel-series-ask"),
            (false, true, false) => tr!("cal-cancel-ask"),
            (false, false, _) => tr!("cal-edit-series-ask"),
        };
        let answers = div()
            .flex()
            .flex_wrap()
            .gap_2()
            .when(e.recurring, |d| {
                d.child(answer("own-one", tr!("cal-series-one"), cancel, Some(false), cx))
                    .child(answer("own-all", tr!("cal-series-all"), cancel, Some(true), cx))
            })
            .when(!e.recurring && cancel, |d| d.child(answer("own-yes", tr!("cal-cancel-meeting"), true, Some(false), cx)))
            .child(answer("own-no", tr!(if cancel { "cal-cancel-keep" } else { "cal-cancel-back" }), false, None, cx));
        div()
            .flex()
            .flex_col()
            .gap_2()
            .mx(px(-20.))
            .px(px(20.))
            .py(px(10.))
            .bg(l.warning_bg())
            .child(question)
            .when(!busy, |d| d.child(answers))
    }

    /// Снимки (`event-organizer`, `event-cancel`): своя встреча с участниками, у повтора — первый экземпляр.
    pub(crate) fn select_own(&mut self, cancel: bool, cx: &mut Context<Self>) {
        let me = self.me.clone();
        let (from, to) = self.range();
        self.selected = (0..self.events.len()).find(|&i| {
            let e = &self.events[i];
            let mine = my_part(e, me.as_deref());
            mine.is_some_and(|p| p.owner) && e.participants.len() > 1 && e.span.start < to && e.span.end > from
        });
        self.own = cancel.then_some(Own::Cancel);
        cx.notify();
    }

    /// Отмена не ушла (демо: нужен сервер).
    pub(crate) fn cancel_refused(&mut self, cx: &mut Context<Self>) {
        self.cancelling = None;
        cx.notify();
    }

    /// Ответ не ушёл (демо: нужен сервер).
    pub(crate) fn respond_refused(&mut self, cx: &mut Context<Self>) {
        self.answering = None;
        cx.notify();
    }

    /// Цвета календарей владельцев: сохранённые в «Настройки → Календарь» и назначенные новым (S5-22).
    fn assigned(colleagues: &[Owner], saved: &std::collections::BTreeMap<String, String>) -> std::collections::BTreeMap<String, String> {
        let keys: Vec<String> = std::iter::once(String::new()).chain(colleagues.iter().map(Owner::key)).collect();
        postman_core::palette::assign_calendar_colors(&keys, saved)
    }

    /// Строка календаря слева: флажок показа, имя, образец цвета; щелчок по образцу — выбор из цветов календарей.
    fn calendar_row(&self, id: impl Into<ElementId>, key: String, name: String, check: impl IntoElement, l: &Look, cx: &mut Context<Self>) -> Div {
        let shown = key.is_empty() || !self.hidden.contains(&key);
        let open = self.color_menu.as_deref() == Some(key.as_str());
        let current = self.calendar_color(&key).to_owned();
        let id: ElementId = id.into();
        let menu_key = key.clone();
        let swatch = div()
            .id(id.clone())
            .size(px(18.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .cursor_pointer()
            .hover(|d| d.bg(l.hover()))
            .text_color(l.text3())
            .child(icon(g::MORE, 10.))
            .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tr!("cal-color-change")).build(window, cx))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.color_menu = if this.color_menu.as_deref() == Some(menu_key.as_str()) { None } else { Some(menu_key.clone()) };
                cx.notify();
            }));
        let row = div()
            .h(px(32.))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(10.))
            .child(check)
            .child(div().flex_1().min_w_0().truncate().when(!shown, |d| d.text_color(l.text3())).child(name))
            .child(swatch);
        let colors = postman_core::palette::CALENDAR.iter().enumerate().map(|(n, c)| {
            let (k, c) = (key.clone(), (*c).to_owned());
            let on = c == current;
            div()
                .id(("cal-color", n))
                .size(px(18.))
                .rounded(px(4.))
                .bg(l.cat(&c).dot)
                .border_2()
                .border_color(if on { l.text() } else { l.none() })
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| this.set_calendar_color(&k, &c, cx)))
        });
        div().flex().flex_col().child(row).when(open, |d| d.child(div().flex().gap(px(6.)).pl(px(34.)).pb(px(6.)).children(colors)))
    }

    /// Цвет календаря выбран вручную — в «Настройки → Календарь».
    fn set_calendar_color(&mut self, key: &str, color: &str, cx: &mut Context<Self>) {
        let (k, c) = (key.to_owned(), color.to_owned());
        self.colors.insert(k.clone(), c.clone());
        self.color_menu = None;
        crate::prefs::update(cx, move |p| {
            p.calendar.calendar_colors.insert(k, c);
        });
        cx.notify();
    }

    /// Показать или скрыть календарь коллеги.
    fn toggle_calendar(&mut self, key: &str, cx: &mut Context<Self>) {
        if let Some(i) = self.hidden.iter().position(|k| k == key) {
            self.hidden.remove(i);
        } else {
            self.hidden.push(key.to_owned());
        }
        let hidden = self.hidden.clone();
        crate::prefs::update(cx, move |p| p.calendar.hidden_calendars = hidden);
        cx.notify();
    }

    /// Назначенные цвета новых коллег — в настройки: цвет не меняется между запусками.
    fn remember_colors(&mut self, cx: &mut Context<Self>) {
        let saved = crate::prefs::get(cx).calendar.calendar_colors;
        let next = Self::assigned(&self.colleagues, &saved);
        if next != saved {
            let keep = next.clone();
            crate::prefs::update(cx, move |p| p.calendar.calendar_colors = keep);
        }
        self.colors = next;
    }

    /// Цвет календаря владельца `key` (`""` — мой).
    fn calendar_color(&self, key: &str) -> &str {
        self.colors.get(key).map_or(postman_core::palette::BLUE, String::as_str)
    }

    /// Календарь коллеги скрыт флажком слева.
    fn hidden(&self, e: &Event) -> bool {
        e.owner.as_ref().is_some_and(|o| self.hidden.contains(&o.key()))
    }

    /// Цвета события: (полоса, подложка, текст). Подложка и текст — цвет календаря владельца (S5-22, DESIGN
    /// «Календарь»), полоса — категория события, как в строке письма; без категории — цвет календаря. «Цвета по
    /// владельцу» выключены — событие коллеги цветом категории, как своё до S5-22.
    fn colors(&self, e: &Event, l: &Look) -> (Hsla, Hsla, Hsla) {
        let key = e.owner.as_ref().map(Owner::key).unwrap_or_default();
        let cal = l.cat(self.calendar_color(&key));
        let cat = self.categories.for_event(&e.keywords).map(|c| l.cat(c.shown_color()));
        match cat {
            Some(c) if e.owner.is_some() && !self.owner_colors => (c.dot, c.bg, c.text),
            Some(c) => (c.dot, cal.bg, cal.text),
            None => (cal.dot, cal.bg, cal.text),
        }
    }

    /// Имя «календаря» события в подробностях: владелец (календарь коллеги), категория или «Мой календарь».
    fn source_name(&self, e: &Event) -> String {
        if let Some(o) = &e.owner {
            return tr!("cal-colleague-calendar", name = o.name.clone());
        }
        self.categories.for_event(&e.keywords).map(|c| c.name.clone()).unwrap_or_else(|| tr!("cal-my-calendar"))
    }

    // --- слева: месяц, календари, категории, занятость ---

    fn side(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let l = crate::roles::look(cx);
        let month = self.month;
        let today = self.now_zoned().date();
        let start = model::week_first(month.first_of_month(), self.week_start);
        // Показанные дни — полосой (месяц — без полосы: он весь в сетке).
        let days = self.days();
        let band = (self.view != View::Month).then(|| (days.first().copied(), days.last().copied()));
        let (band_from, band_to) = band.unwrap_or((None, None));
        let header = div()
            .flex()
            .items_center()
            .h(px(32.))
            .pl(px(8.))
            .child(
                div()
                    .flex_1()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr!("cal-month-title", month = tr!("cal-month", m = month.month()), year = month.year())),
            )
            .child(icon_button("month-prev", g::PREV, &l).on_click(cx.listener(|this, _, _, cx| {
                this.month = add_days(this.month.first_of_month(), -1).first_of_month();
                cx.notify();
            })))
            .child(icon_button("month-next", g::NEXT, &l).on_click(cx.listener(|this, _, _, cx| {
                this.month = add_days(this.month.last_of_month(), 1);
                cx.notify();
            })));
        let names = div().flex().pl(px(4.)).children((0..7).map(|d| {
            div()
                .flex_1()
                .h(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(11.))
                .text_color(l.text2())
                .child(weekday_name(add_days(start, d)))
        }));
        let weeks: Vec<_> = (0..6).map(|w| {
            div().flex().pl(px(4.)).children((0..7).map(|d| {
                let date = add_days(start, w * 7 + d);
                let other = date.month() != month.month();
                let in_band = band_from.zip(band_to).is_some_and(|(a, b)| date >= a && date <= b);
                let is_today = date == today;
                let off = !self.hours.days.contains(&date.weekday());
                let n = w * 7 + d;
                div()
                    .id(("day", n as usize))
                    .flex_1()
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(in_band, |c| {
                        c.bg(l.picked())
                            .when(Some(date) == band_from, |c| c.rounded_l(px(13.)))
                            .when(Some(date) == band_to, |c| c.rounded_r(px(13.)))
                    })
                    .cursor_pointer()
                    .child(
                        div()
                            .size(px(26.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .text_size(px(12.))
                            .when(is_today, |c| c.bg(l.accent()).text_color(l.on_accent()).font_weight(FontWeight::SEMIBOLD))
                            .when(!is_today && other, |c| c.text_color(l.text3()))
                            .when(!is_today && !other && off, |c| c.text_color(l.text2()))
                            .when(!is_today, |c| c.hover(|c| c.bg(l.hover())))
                            .child(date.day().to_string()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.go_to(date, cx)))
            }))
        }).collect();
        let title = |key: &str| div().text_size(px(12.)).text_color(l.text2()).px(px(8.)).pt(px(10.)).pb(px(4.)).child(tr!(key));
        let check = div()
            .size(px(16.))
            .flex_none()
            .rounded(px(4.))
            .flex()
            .items_center()
            .justify_center()
            .bg(l.cat(self.calendar_color("")).dot)
            .text_color(l.on_accent())
            .child(icon(g::CHECK, 10.));
        let mine = self.calendar_row("cal-mine", String::new(), tr!("cal-my-calendar"), check, &l, cx);
        let mut colleagues: Vec<AnyElement> = self
            .colleagues
            .iter()
            .enumerate()
            .map(|(n, o)| {
                let key = o.key();
                let shown = !self.hidden.contains(&key);
                let color = l.cat(self.calendar_color(&key)).dot;
                let toggle = key.clone();
                let check = div()
                    .id(("cal-colleague-check", n))
                    .size(px(16.))
                    .flex_none()
                    .rounded(px(4.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .border_1()
                    .border_color(color)
                    .when(shown, |d| d.bg(color).text_color(l.on_accent()).child(icon(g::CHECK, 10.)))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_calendar(&toggle, cx)));
                self.calendar_row(("cal-colleague", n), key, o.name.clone(), check, &l, cx).into_any_element()
            })
            .collect();
        // Календари других моих учётных записей (S7-03б) — под «Моим календарём».
        let my_others: Vec<AnyElement> = colleagues.drain(..self.mine.min(colleagues.len())).collect();
        let cats = self.categories.visible().map(|c| {
            let color = l.cat(c.shown_color());
            div()
                .h(px(28.))
                .px(px(8.))
                .flex()
                .items_center()
                .gap(px(10.))
                .child(div().size(px(10.)).flex_none().rounded(px(3.)).bg(color.dot))
                .child(div().min_w_0().truncate().child(c.name.clone()))
        });
        let share = if self.share_open {
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .px(px(8.))
                .pt(px(8.))
                .child(div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).child(tr!("cal-share-label")))
                .child(Input::new(&self.share))
                .child(
                    div()
                        .flex()
                        .gap(px(6.))
                        .child(
                            crate::buttons::primary("share", &l)
                                .child(tr!("cal-share-button"))
                                .on_click(cx.listener(|this, _, window, cx| this.submit_share(window, cx))),
                        )
                        .child(crate::buttons::secondary("share-cancel", &l).child(tr!("cal-share-cancel")).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.share_open = false;
                                cx.notify();
                            },
                        ))),
                )
                .when_some(self.share_status.clone(), |d, (text, error)| {
                    d.child(div().text_size(px(12.)).text_color(if error { l.error() } else { l.text2() }).child(text))
                })
                .child(div().text_size(px(11.)).line_height(px(15.)).text_color(l.text2()).child(tr!("cal-share-note")))
                .into_any_element()
        } else {
            div()
                .id("share-open")
                .h(px(30.))
                .px(px(8.))
                .flex()
                .items_center()
                .gap(px(8.))
                .text_size(px(12.))
                .text_color(l.accent_text())
                .cursor_pointer()
                .hover(|d| d.bg(l.hover()))
                .rounded(px(4.))
                .child(icon(g::ADD, 12.))
                .child(tr!("cal-share-open"))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.share_open = true;
                    this.share.update(cx, |s, cx| s.focus(window, cx));
                    cx.notify();
                }))
                .into_any_element()
        };
        div()
            .id("cal-side")
            .w(px(SIDE_W))
            .flex_none()
            .h_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(2.))
            .pt(px(4.))
            .pr(px(8.))
            .pl(px(4.))
            .text_size(px(13.))
            .child(header)
            .child(div().flex().flex_col().gap(px(2.)).child(names).children(weeks))
            .child(div().h(px(1.)).mx(px(8.)).mt(px(10.)).mb(px(2.)).bg(l.border()))
            .child(title("cal-group-mine"))
            .child(mine)
            .children(my_others)
            .when(!colleagues.is_empty(), |d| d.child(title("cal-group-colleagues")).children(colleagues))
            .child(title("cal-group-categories"))
            .children(cats)
            .child(div().h(px(1.)).mx(px(8.)).mt(px(10.)).mb(px(2.)).bg(l.border()))
            .child(title("cal-group-share"))
            .child(share)
    }

    // --- сетка дня и недели ---

    fn grid(&self, days: &[Date], cx: &mut Context<Self>) -> impl IntoElement {
        let l = crate::roles::look(cx);
        let n = days.len().max(1);
        let height = HOURS as f32 * HOUR_PX;
        let tz = &self.tz;
        let me = self.me.as_deref();
        let now = self.now();
        let now_z = self.now_zoned();
        let today = now_z.date();
        let now_min = i64::from(now_z.hour()) * 60 + i64::from(now_z.minute());
        let now_y = ((now_min - FIRST_HOUR * 60) as f32 / 60. * HOUR_PX).clamp(-1., height + 1.);
        let now_visible = (0. ..=height).contains(&now_y);
        let today_shown = days.contains(&today);
        let second = self.second.clone();
        let gutter_w = GUTTER + if second.is_some() { GUTTER2 } else { 0. };
        let header = div()
            .h(px(44.))
            .flex_none()
            .flex()
            .border_b_1()
            .border_color(l.border())
            .when_some(second.clone(), |d, z| {
                d.child(
                    div()
                        .w(px(GUTTER2))
                        .flex_none()
                        .flex()
                        .items_end()
                        .justify_end()
                        .pr(px(6.))
                        .pb(px(6.))
                        .text_size(px(11.))
                        .text_color(l.text3())
                        .child(zone_short(&z)),
                )
            })
            .child(
                div()
                    .w(px(GUTTER))
                    .flex_none()
                    .flex()
                    .items_end()
                    .justify_end()
                    .pr(px(6.))
                    .pb(px(6.))
                    .text_size(px(11.))
                    .text_color(l.text3())
                    .child(zone_short(tz)),
            )
            .children(days.iter().map(|&date| {
                let is_today = date == today;
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(10.))
                    .border_l_1()
                    .border_color(l.border())
                    .child(
                        div()
                            .min_w(px(26.))
                            .h(px(26.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(18.))
                            .when(is_today, |c| {
                                c.rounded(px(13.)).bg(l.accent()).text_color(l.on_accent()).font_weight(FontWeight::SEMIBOLD)
                            })
                            .when(!is_today && date < today, |c| c.text_color(l.text2()))
                            .child(date.day().to_string()),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .when(is_today, |c| c.text_color(l.accent_text()).font_weight(FontWeight::SEMIBOLD))
                            .when(!is_today, |c| c.text_color(l.text2()))
                            .child(if days.len() == 1 { tr!("cal-weekday", d = date.weekday().to_monday_zero_offset()) } else { weekday_name(date) }),
                    )
            }));
        // Весь день: событие от дня начала до дня конца — по показанным столбцам.
        let all_day: Vec<AnyElement> = self
            .events
            .iter()
            .enumerate()
            .filter(|(_, e)| e.all_day && event_look(e, me) != Shown::Hidden && !self.hidden(e))
            .filter_map(|(i, e)| {
                let (a, b) = model::dates(e, tz);
                let (c0, c1) = model::columns(days, a, b)?;
                let (bar, bg, fg) = self.colors(e, &l);
                let selected = self.selected == Some(i);
                Some(
                    div()
                        .id(("all-day", i))
                        .absolute()
                        .top(px(3.))
                        .h(px(22.))
                        .left(relative(c0 as f32 / n as f32))
                        .w(relative((c1 - c0 + 1) as f32 / n as f32))
                        .px(px(2.))
                        .child(
                            div()
                                .size_full()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .pl(px(3.))
                                .pr(px(8.))
                                .bg(bg)
                                .text_color(fg)
                                .text_size(px(12.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .overflow_hidden()
                                .child(div().w(px(3.)).h_full().flex_none().bg(bar))
                                .child(div().truncate().child(e.title.clone()))
                                .when(selected, |d| d.shadow(glow(bar))),
                        )
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| this.select(i, cx)))
                        .into_any_element(),
                )
            })
            .collect();
        let all_day_row = div()
            .h(px(30.))
            .flex_none()
            .flex()
            .border_b_1()
            .border_color(l.border())
            .child(
                div()
                    .w(px(gutter_w))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .items_end()
                    .justify_center()
                    .pr(px(6.))
                    .text_size(px(10.))
                    .line_height(px(11.))
                    .text_color(l.text3())
                    .child(tr!("cal-all-day-1"))
                    .child(tr!("cal-all-day-2")),
            )
            .child(
                div()
                    .flex_1()
                    .relative()
                    .h_full()
                    .children((0..n).map(|d| {
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left(relative(d as f32 / n as f32))
                            .w(relative(1. / n as f32))
                            .border_l_1()
                            .border_color(l.border())
                    }))
                    .children(all_day),
            );
        // Время слева: часы и «сейчас»; второй пояс — своей шкалой левее (часы по началу показанного дня).
        let anchor0 = day_start(days.first().copied().unwrap_or(self.anchor), tz);
        let times2 = second.as_ref().map(|z| {
            div().w(px(GUTTER2)).flex_none().relative().h(px(height)).children((0..HOURS).map(|h| {
                div()
                    .absolute()
                    .top(px(h as f32 * HOUR_PX + 3.))
                    .right(px(6.))
                    .text_size(px(11.))
                    .text_color(l.text3())
                    .child(hm(anchor0 + (FIRST_HOUR + h) * 3600, z))
            }))
        });
        let times = div()
            .w(px(GUTTER))
            .flex_none()
            .relative()
            .h(px(height))
            .children((0..HOURS).map(|h| {
                let near_now = now_visible && today_shown && ((FIRST_HOUR + h) * 60 - now_min).abs() < 25;
                div()
                    .absolute()
                    .top(px(h as f32 * HOUR_PX + 3.))
                    .right(px(6.))
                    .text_size(px(11.))
                    .text_color(l.text2())
                    .when(near_now, |d| d.invisible())
                    .child(format!("{:02}:00", FIRST_HOUR + h))
            }))
            .when(now_visible && today_shown, |d| {
                d.child(
                    div()
                        .absolute()
                        .top(px(now_y - 9.))
                        .right(px(2.))
                        .px(px(4.))
                        .py(px(1.))
                        .rounded(px(4.))
                        .bg(l.accent())
                        .text_color(l.on_accent())
                        .text_size(px(11.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(hm(now, tz)),
                )
            });
        // Рабочее время — «Настройки → Календарь» (S5-20): часы до минуты, нерабочий день затенён целиком.
        let y = |t: jiff::civil::Time| {
            ((i64::from(t.hour()) * 60 + i64::from(t.minute()) - FIRST_HOUR * 60) as f32 / 60. * HOUR_PX).clamp(0., height)
        };
        let (work_start, work_end) = (y(self.hours.start), y(self.hours.end));
        // Линии: час — сплошная, шаг сетки (S5-20: 30 или 15 мин) — бледнее.
        let per_hour = (60 / self.step.clamp(5, 60)).max(1);
        let (hour_line, step_line) = (l.border(), l.border().opacity(0.45));
        let mut columns = Vec::new();
        for &date in days {
            let is_today = date == today;
            let day0 = day_start(date, tz) + FIRST_HOUR * 3600;
            let (timed, _) = day_events(&self.events, date, tz);
            let timed: Vec<usize> = timed.into_iter().filter(|&i| event_look(&self.events[i], me) != Shown::Hidden && !self.hidden(&self.events[i])).collect();
            let placed = layout_day(&timed.iter().map(|&i| self.events[i].span).collect::<Vec<_>>());
            let (ws, we) = if self.hours.days.contains(&date.weekday()) { (work_start, work_end) } else { (height, height) };
            let shade = [(0., ws), (we, height)].map(|(a, b)| {
                div().absolute().left_0().right_0().top(px(a)).h(px(b - a)).bg(l.pm(Pm::CalOff))
            });
            let lines = (0..HOURS).flat_map(|h| {
                let y = h as f32 * HOUR_PX;
                std::iter::once(div().absolute().top(px(y)).left_0().right_0().h(px(1.)).bg(hour_line)).chain((1..per_hour).map(move |k| {
                    div()
                        .absolute()
                        .top(px(y + HOUR_PX * k as f32 / per_hour as f32))
                        .left_0()
                        .right_0()
                        .h(px(1.))
                        .bg(step_line)
                }))
            });
            let blocks: Vec<AnyElement> = placed
                .into_iter()
                .map(|p| {
                    let i = timed[p.index];
                    let e = &self.events[i];
                    let top = ((e.span.start - day0) as f32 / 3600. * HOUR_PX).clamp(0., height - 18.) + 1.;
                    let h = (e.span.len() as f32 / 3600. * HOUR_PX).clamp(18., height - top) - 3.;
                    div()
                        .absolute()
                        .top(px(top))
                        .h(px(h))
                        .left(relative(p.column as f32 / p.columns as f32))
                        .w(relative(p.span_columns as f32 / p.columns as f32))
                        .px(px(2.))
                        .child(self.block(i, e, h, &l, cx))
                        .into_any_element()
                })
                .collect();
            let now_line = (now_visible && today_shown).then(|| {
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px(now_y))
                    .h(px(if is_today { 2. } else { 1. }))
                    .bg(if is_today { l.accent() } else { l.accent().opacity(0.3) })
            });
            let now_dot = (now_visible && is_today)
                .then(|| div().absolute().left(px(-5.)).top(px(now_y - 4.)).size(px(9.)).rounded_full().bg(l.accent()));
            columns.push(
                div()
                    .flex_1()
                    .min_w_0()
                    .relative()
                    .h(px(height))
                    .border_l_1()
                    .border_color(l.border())
                    .children(shade)
                    .children(lines)
                    .children(blocks)
                    .children(now_line)
                    .children(now_dot),
            );
        }
        crate::mailer::card(&l)
            .flex_1()
            .mr(px(8.))
            .child(header)
            .child(all_day_row)
            .child(
                div()
                    .id("week-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(div().flex().children(times2).child(times).children(columns)),
            )
            .when(self.pending.is_some() && self.events.is_empty(), |d| {
                d.child(div().px(px(14.)).py(px(6.)).text_color(l.text2()).child(tr!("cal-loading")))
            })
    }

    /// Блок события в сетке дня и недели.
    fn block(&self, i: usize, e: &Event, h: f32, l: &Look, cx: &mut Context<Self>) -> Stateful<Div> {
        let tz = &self.tz;
        let look = event_look(e, self.me.as_deref());
        let (bar, bg, fg) = self.colors(e, l);
        let selected = self.selected == Some(i);
        let place = if e.location.trim().is_empty() { String::new() } else { format!(" · {}", e.location.trim()) };
        let sub = match look {
            Shown::Pending => format!("{}–{} · {}", hm(e.span.start, tz), hm(e.span.end, tz), tr!("cal-waits-answer")),
            _ => format!("{}–{}{place}", hm(e.span.start, tz), hm(e.span.end, tz)),
        };
        // Имя владельца у события коллеги (S5-22, «Показывать имя владельца»).
        let sub = match e.owner.as_ref().filter(|_| self.owner_names) {
            Some(o) => format!("{} · {sub}", o.name),
            None => sub,
        };
        let stripe: AnyElement = match look {
            // Без ответа (DESIGN «Календарь»): без заливки, контур и полоса цветом календаря на 70 % и 45 %.
            Shown::Pending => div().absolute().left_0().top_0().bottom_0().w(px(4.)).bg(bar.opacity(0.45)).into_any_element(),
            Shown::Tentative => div()
                .absolute()
                .left_0()
                .top_0()
                .bottom_0()
                .w(px(4.))
                .overflow_hidden()
                .flex()
                .flex_col()
                .gap(px(3.))
                .children((0..((h / 6.) as usize + 1)).map(|_| div().h(px(3.)).flex_none().bg(bar)))
                .into_any_element(),
            _ => div().absolute().left_0().top_0().bottom_0().w(px(4.)).bg(bar).into_any_element(),
        };
        div()
            .id(("event", i))
            .relative()
            .size_full()
            .overflow_hidden()
            .pl(px(9.))
            .pr(px(6.))
            .pt(px(3.))
            .flex()
            .flex_col()
            .gap(px(1.))
            .text_size(px(12.))
            .line_height(px(15.))
            .text_color(fg)
            .when(look == Shown::Pending, |d| d.bg(l.none()).border_1().border_dashed().border_color(bar.opacity(0.7)))
            .when(look != Shown::Pending, |d| d.bg(bg))
            .when(selected, |d| d.shadow(glow(bar)))
            .cursor_pointer()
            .tooltip({
                let tip = format!("{}, {}–{}", e.title, hm(e.span.start, tz), hm(e.span.end, tz));
                move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
            })
            .child(stripe)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .min_w_0()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(div().min_w_0().truncate().child(e.title.clone()))
                    .when(e.recurring, |d| d.child(icon(g::REPEAT, 10.).opacity(0.8))),
            )
            .when(h >= 36., |d| d.child(div().truncate().text_size(px(11.)).opacity(0.9).child(sub)))
            .on_click(cx.listener(move |this, _, _, cx| this.select(i, cx)))
    }

    // --- месяц ---

    fn month_grid(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let l = crate::roles::look(cx);
        let tz = &self.tz;
        let me = self.me.as_deref();
        let today = self.now_zoned().date();
        let weeks = model::month_weeks(self.anchor, self.week_start);
        let items = model::month_layout(&self.events, |i| event_look(&self.events[i], me) != Shown::Hidden, &weeks, tz);
        let first = weeks.first().copied().unwrap_or(self.anchor);
        let header = div().h(px(32.)).flex_none().flex().border_b_1().border_color(l.border()).children((0..7).map(|d| {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .px(px(10.))
                .border_l_1()
                .border_color(l.border())
                .text_size(px(12.))
                .text_color(l.text2())
                .child(weekday_name(add_days(first, d)))
        }));
        let rows: Vec<Div> = weeks.iter().enumerate().map(|(w, &start)| {
            let cells: Vec<Div> = (0..7).map(|c| {
                let date = add_days(start, c as i64);
                let other = date.month() != self.anchor.month();
                let off = !self.hours.days.contains(&date.weekday());
                let is_today = date == today;
                let hidden = model::hidden_in(&items, w, c, MONTH_LANES);
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(relative(c as f32 / 7.))
                    .w(relative(1. / 7.))
                    .border_l_1()
                    .border_color(l.border())
                    .when(other || off, |d| d.bg(l.pm(Pm::CalOff)))
                    .child(
                        div()
                            .id(("month-day", w * 7 + c))
                            .absolute()
                            .top(px(3.))
                            .left(px(6.))
                            .min_w(px(22.))
                            .h(px(22.))
                            .px(px(4.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(11.))
                            .text_size(px(12.))
                            .cursor_pointer()
                            .when(is_today, |d| d.bg(l.accent()).text_color(l.on_accent()).font_weight(FontWeight::SEMIBOLD))
                            .when(!is_today && other, |d| d.text_color(l.text3()))
                            .when(!is_today, |d| d.hover(|d| d.bg(l.hover())))
                            .child(if date.day() == 1 { format!("{} {}", date.day(), tr!("cal-month-of", m = date.month())) } else { date.day().to_string() })
                            .on_click(cx.listener(move |this, _, _, cx| this.open_day(date, cx))),
                    )
                    .when(hidden > 0, |d| {
                        d.child(
                            div()
                                .id(("month-more", w * 7 + c))
                                .absolute()
                                .top(px(MONTH_NUM_H + MONTH_LANES as f32 * MONTH_ITEM_H))
                                .left(px(6.))
                                .px(px(4.))
                                .rounded(px(4.))
                                .text_size(px(11.))
                                .text_color(l.text2())
                                .cursor_pointer()
                                .hover(|d| d.bg(l.hover()))
                                .child(tr!("cal-month-more", n = hidden))
                                .on_click(cx.listener(move |this, _, _, cx| this.open_day(date, cx))),
                        )
                    })
            }).collect();
            let row_items: Vec<AnyElement> =
                items.iter().filter(|m| m.week == w && m.lane < MONTH_LANES).map(|m| self.month_item(*m, start, &l, cx)).collect();
            div()
                .flex_1()
                .min_h(px(MONTH_NUM_H + (MONTH_LANES + 1) as f32 * MONTH_ITEM_H))
                .relative()
                .overflow_hidden()
                .border_b_1()
                .border_color(l.border())
                .children(cells)
                .children(row_items)
        }).collect();
        crate::mailer::card(&l)
            .flex_1()
            .mr(px(8.))
            .child(header)
            .child(div().flex_1().min_h_0().flex().flex_col().children(rows))
            .when(self.pending.is_some() && self.events.is_empty(), |d| {
                d.child(div().px(px(14.)).py(px(6.)).text_color(l.text2()).child(tr!("cal-loading")))
            })
    }

    /// Событие в клетках месяца: многодневное — полосой (полоса цвета — у начала), остальное — строкой
    /// «время · тема» с точкой цвета.
    fn month_item(&self, m: MonthItem, week_start: Date, l: &Look, cx: &Context<Self>) -> AnyElement {
        let i = m.index;
        let Some(e) = self.events.get(i) else { return div().into_any_element() };
        let tz = &self.tz;
        let look = event_look(e, self.me.as_deref());
        let (bar, bg, fg) = self.colors(e, l);
        let selected = self.selected == Some(i);
        let starts_here = model::dates(e, tz).0 >= week_start;
        let tip = if e.all_day { e.title.clone() } else { format!("{}, {}–{}", e.title, hm(e.span.start, tz), hm(e.span.end, tz)) };
        let inner = if m.bar {
            div()
                .size_full()
                .flex()
                .items_center()
                .gap(px(6.))
                .pr(px(6.))
                .rounded(px(3.))
                .overflow_hidden()
                .text_size(px(12.))
                .text_color(fg)
                .when(look == Shown::Pending, |d| d.bg(l.panel()).border_1().border_dashed().border_color(bar.opacity(0.7)))
                .when(look != Shown::Pending, |d| d.bg(bg))
                .child(div().w(px(3.)).h_full().flex_none().when(starts_here, |d| d.bg(bar)))
                .child(div().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD).child(e.title.clone()))
        } else {
            div()
                .size_full()
                .flex()
                .items_center()
                .gap(px(6.))
                .px(px(4.))
                .rounded(px(3.))
                .overflow_hidden()
                .text_size(px(12.))
                .hover(|d| d.bg(l.hover()))
                .child(
                    div()
                        .size(px(8.))
                        .flex_none()
                        .rounded_full()
                        .when(look == Shown::Pending, |d| d.border_1().border_dashed().border_color(bar))
                        .when(look != Shown::Pending, |d| d.bg(bar)),
                )
                .child(div().flex_none().text_color(l.text2()).child(hm(e.span.start, tz)))
                .child(div().min_w_0().truncate().child(e.title.clone()))
        };
        div()
            .id(("month-event", i * 8 + m.week))
            .absolute()
            .top(px(MONTH_NUM_H + m.lane as f32 * MONTH_ITEM_H))
            .h(px(MONTH_ITEM_H - 2.))
            .left(relative(m.col0 as f32 / 7.))
            .w(relative((m.col1 - m.col0 + 1) as f32 / 7.))
            .px(px(3.))
            .cursor_pointer()
            .child(inner.when(selected, |d| d.shadow(glow(bar))))
            .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx))
            .on_click(cx.listener(move |this, _, _, cx| this.select(i, cx)))
            .into_any_element()
    }

    /// Число в месяце и «+N ещё»: этот день видом «День».
    fn open_day(&mut self, date: Date, cx: &mut Context<Self>) {
        let before = self.range();
        self.anchor = date;
        self.month = date.first_of_month();
        self.view = View::Day;
        self.period_changed(before, cx);
        cx.notify();
    }

    // --- подробности и найденное ---

    fn details(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let l = crate::roles::look(cx);
        let body = if self.results_open {
            self.found_list(&l, cx).into_any_element()
        } else {
            match self.selected.and_then(|i| self.events.get(i).map(|e| (i, e))) {
                Some((i, e)) => self.event_details(i, e, &l, cx).into_any_element(),
                None => div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(8.))
                    .px(px(24.))
                    .text_color(l.text2())
                    .child(icon(g::CALENDAR, 28.).text_color(l.text3()))
                    .child(div().text_center().child(tr!("cal-details-empty")))
                    .into_any_element(),
            }
        };
        crate::mailer::card(&l).w(px(DETAILS_W)).flex_none().child(body)
    }

    /// Найденное поиском: строки с датой, временем, темой и местом; Enter или щелчок — к событию.
    fn found_list(&self, l: &Look, cx: &mut Context<Self>) -> impl IntoElement {
        let tz = &self.tz;
        let results = self.results();
        let status = if self.searching.is_some() {
            tr!("cal-search-server")
        } else if let Some(e) = self.found.error.as_ref().filter(|_| self.found.text == self.query.trim()) {
            tr!("cal-search-failed", error = e.clone())
        } else if self.live {
            tr!("cal-search-scope-server")
        } else {
            tr!("cal-search-scope-loaded")
        };
        let rows = results.into_iter().enumerate().map(|(n, e)| {
            let on = n == self.cursor;
            let (bar, _, _) = self.colors(&e, l);
            let date = model::dates(&e, tz).0;
            let when = if e.all_day { tr!("cal-all-day") } else { format!("{}–{}", hm(e.span.start, tz), hm(e.span.end, tz)) };
            let place = e.location.trim().to_string();
            let open = e.clone();
            div()
                .id(("found", n))
                .flex()
                .gap(px(10.))
                .px(px(12.))
                .py(px(8.))
                .rounded(px(6.))
                .cursor_pointer()
                .when(on, |d| d.bg(l.picked()))
                .when(!on, |d| d.hover(|d| d.bg(l.hover())))
                .child(div().w(px(3.)).flex_none().rounded(px(2.)).bg(bar))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(div().truncate().font_weight(FontWeight::SEMIBOLD).child(e.title.clone()))
                        .child(div().text_size(px(12.)).text_color(l.text2()).truncate().child(format!("{} · {when}", short_day(date))))
                        .when(!place.is_empty(), |d| d.child(div().text_size(px(12.)).text_color(l.text2()).truncate().child(place))),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.open_result(open.clone(), cx)))
        });
        let rows: Vec<_> = rows.collect();
        let count = rows.len();
        div()
            .id("cal-found")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(2.))
            .px(px(8.))
            .pt(px(14.))
            .pb(px(12.))
            .child(
                div()
                    .px(px(12.))
                    .pb(px(6.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(tr!("cal-search-found", n = count)))
                    .child(div().text_size(px(12.)).text_color(l.text2()).child(status)),
            )
            .children(rows)
            .when(count == 0 && self.searching.is_none(), |d| {
                d.child(div().px(px(12.)).py(px(8.)).text_color(l.text2()).child(tr!("cal-search-nothing")))
            })
    }

    fn event_details(&self, i: usize, e: &Event, l: &Look, cx: &mut Context<Self>) -> impl IntoElement {
        let tz = &self.tz;
        let me = self.me.as_deref();
        let mine = my_part(e, me);
        let owner = mine.is_some_and(|p| p.owner) || e.organizer().is_none();
        let status = mine.map(|p| p.status);
        let (bar, _, _) = self.colors(e, l);
        let answering = self.answering.as_ref().filter(|(u, _)| *u == e.uid).map(|(_, s)| *s);
        let (chip, outlined) = match (owner, status) {
            _ if e.owner.is_some() && mine.is_none() => (tr!("cal-chip-colleague"), false),
            (true, _) => (tr!("cal-chip-organizer"), false),
            (false, Some(Partstat::NeedsAction)) | (false, None) => (tr!("cal-chip-waits"), true),
            (false, Some(Partstat::Tentative)) => (tr!("cal-chip-tentative"), false),
            (false, Some(Partstat::Declined)) => (tr!("cal-chip-declined"), false),
            (false, Some(_)) => (tr!("cal-chip-accepted"), false),
        };
        let top = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .text_size(px(12.))
            .text_color(l.text2())
            .child(div().size(px(10.)).flex_none().rounded(px(3.)).bg(bar))
            .child(div().flex_1().min_w_0().truncate().child(self.source_name(e)))
            .child(
                div()
                    .px(px(6.))
                    .py(px(1.))
                    .rounded(px(4.))
                    .text_size(px(11.))
                    .when(outlined, |d| d.border_1().border_color(bar).text_color(l.text()))
                    .when(!outlined, |d| d.bg(l.fill()).text_color(l.text()))
                    .child(chip),
            )
            .child(icon_button("details-close", g::CANCEL, l).on_click(cx.listener(|this, _, _, cx| this.close_details(cx))));
        let title = div().text_size(px(20.)).font_weight(FontWeight::SEMIBOLD).line_height(px(26.)).child(e.title.clone());
        // Ответ на приглашение: полоса во всю ширину панели; кнопки — пока ответа нет или по «Изменить ответ».
        // «Принять» — заливка, остальные — тонированные, у всех — значок (DESIGN «Календарь и контакты»).
        let invited = !owner && mine.is_some();
        let asking = invited && (status == Some(Partstat::NeedsAction) || self.changing || answering.is_some());
        let invite_block = asking.then(|| {
            let from = e.organizer().map(|o| if o.name.is_empty() { o.email.clone() } else { o.name.clone() }).unwrap_or_default();
            let button = |id: &'static str, key: &str, s: Partstat, cx: &mut Context<Self>| {
                let sending = answering == Some(s);
                let primary = s == Partstat::Accepted;
                let b = if primary { crate::buttons::primary(id, l) } else { crate::buttons::secondary(id, l) };
                let ink = crate::buttons::ink(primary, l);
                b.flex_1()
                    .px(px(6.))
                    .justify_center()
                    .text_size(px(12.))
                    .when(answering.is_some(), |d| d.opacity(if sending { 1. } else { 0.5 }))
                    .child(crate::invite_card::status_mark(s, ink))
                    .child(tr!(key))
                    .on_click(cx.listener(move |this, _, _, cx| this.respond(i, s, cx)))
            };
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .mx(px(-20.))
                .px(px(20.))
                .py(px(12.))
                .bg(l.base())
                .child(div().text_size(px(12.)).text_color(l.text2()).child(tr!("cal-invited-by", name = from)))
                .child(
                    div()
                        .flex()
                        .gap(px(4.))
                        .child(button("accept", "cal-accept", Partstat::Accepted, cx))
                        .child(button("tentative", "cal-tentative-answer", Partstat::Tentative, cx))
                        .child(button("decline", "cal-decline", Partstat::Declined, cx)),
                )
                .when(answering.is_some(), |d| {
                    d.child(div().text_size(px(12.)).text_color(l.text2()).child(tr!("reading-invite-sending")))
                })
        });
        // Своя встреча (S7-07б, решение владельца 2026-10-23): «Изменить» — вкладка встречи с её полями, «Отменить
        // встречу» — подтверждение одной строкой; у повтора — сначала «Только эту / Всю серию». Календарь коллеги — нет.
        let organizer = owner && !(e.owner.is_some() && mine.is_none());
        let own_buttons = organizer.then(|| {
            div()
                .flex()
                .gap(px(4.))
                .child(
                    crate::buttons::secondary("own-edit", l)
                        .flex_1()
                        .justify_center()
                        .text_size(px(12.))
                        .child(icon(g::EDIT, 14.))
                        .child(tr!("cal-edit-meeting"))
                        .on_click(cx.listener(|this, _, _, cx| this.own_step(Own::Edit, cx))),
                )
                .child(
                    crate::buttons::secondary("own-cancel", l)
                        .flex_1()
                        .justify_center()
                        .text_size(px(12.))
                        .child(icon(g::CANCEL, 12.))
                        .child(tr!("cal-cancel-meeting"))
                        .on_click(cx.listener(|this, _, _, cx| this.own_step(Own::Cancel, cx))),
                )
        });
        let own_line = self.own.filter(|_| organizer).map(|step| self.own_line(e, step, l, cx));
        let answered = (invited && !asking).then(|| {
            let text = match status {
                Some(Partstat::Tentative) => tr!("cal-answered-tentative"),
                Some(Partstat::Declined) => tr!("cal-answered-declined"),
                _ => tr!("cal-answered-accepted"),
            };
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .px(px(12.))
                .py(px(8.))
                .rounded(px(6.))
                .bg(l.fill_passive())
                .child(div().flex_1().min_w_0().child(text))
                .child(
                    div()
                        .id("change-answer")
                        .flex_none()
                        .text_color(l.accent_text())
                        .cursor_pointer()
                        .child(tr!("cal-change-answer"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.changing = true;
                            cx.notify();
                        })),
                )
        });
        let when = if e.all_day {
            let (a, b) = model::dates(e, tz);
            if a == b {
                format!("{} · {}", long_day(a), tr!("cal-all-day"))
            } else {
                format!("{} – {} · {}", long_day(a), long_day(b), tr!("cal-all-day"))
            }
        } else {
            zoned(e.span.start, tz)
                .map(|z| format!("{} · {}–{}", long_day(z.date()), hm(e.span.start, tz), hm(e.span.end, tz)))
                .unwrap_or_default()
        };
        let row = |glyph: char, main: AnyElement| {
            div()
                .flex()
                .gap(px(10.))
                .items_start()
                .child(icon(glyph, 16.).mt(px(1.)).text_color(l.text2()))
                .child(div().flex_1().min_w_0().child(main))
        };
        let my_events: Vec<Event> = self.events.iter().filter(|e| e.owner.is_none()).cloned().collect();
        let conflicts = postman_core::invite::conflicts(e.span, &e.uid, &my_events, me);
        let conflict_text = conflicts.first().map(|c| {
            let more = conflicts.len().saturating_sub(1);
            let base = tr!("cal-conflict", title = c.title.clone(), time = format!("{}–{}", hm(c.span.start, tz), hm(c.span.end, tz)));
            if more > 0 { format!("{base} {}", tr!("cal-conflict-more", n = more)) } else { base }
        });
        let info = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .pb(px(12.))
            .border_b_1()
            .border_color(l.border())
            .child(row(
                g::CLOCK,
                div()
                    .flex()
                    .flex_col()
                    .child(when)
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(l.text2())
                            .child(format!("{} · {}", duration_text(e.span.len()), zone_label(tz))),
                    )
                    .into_any_element(),
            ))
            .when(e.recurring, |d| d.child(row(g::REPEAT, div().child(tr!("cal-details-recurring")).into_any_element())))
            .when(!e.location.trim().is_empty(), |d| d.child(row(g::PIN, div().child(e.location.clone()).into_any_element())))
            .when_some(conflict_text, |d, t| {
                d.child(
                    div()
                        .flex()
                        .gap(px(10.))
                        .items_start()
                        .px(px(8.))
                        .py(px(6.))
                        .mx(px(-8.))
                        .rounded(px(6.))
                        .bg(l.pm(Pm::ConflictBg))
                        .text_color(l.pm(Pm::ConflictFg))
                        .child(icon(g::OVERLAP, 16.).mt(px(1.)))
                        .child(div().flex_1().min_w_0().child(t)),
                )
            });
        // Участники: организатор первым, затем по имени; над списком — легенда-счётчик ответов.
        let mut people: Vec<&Participant> = e.participants.iter().filter(|p| p.attendee || p.owner).collect();
        people.sort_by_key(|p| (!p.owner, p.name.to_lowercase(), p.email.to_lowercase()));
        let my_domain = me.and_then(|m| m.split_once('@')).map(|(_, d)| d.to_lowercase());
        let counts = model::counts(people.iter().copied());
        let legend = [
            (Partstat::Accepted, counts.accepted, "cal-legend-accepted"),
            (Partstat::Tentative, counts.tentative, "cal-legend-tentative"),
            (Partstat::Declined, counts.declined, "cal-legend-declined"),
            (Partstat::NeedsAction, counts.none, "cal-legend-none"),
        ]
        .into_iter()
        .filter(|(_, n, _)| *n > 0)
        .enumerate()
        .map(|(k, (s, n, key))| {
            let tip = tr!(key, n = n);
            div()
                .id(("legend", k))
                .flex()
                .items_center()
                .gap(px(4.))
                .child(status_glyph_sized(s, 16., l))
                .child(n.to_string())
                .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx))
        });
        let list = people.iter().map(|p| {
            let name = if p.name.trim().is_empty() { p.email.clone() } else { p.name.clone() };
            let you = me.is_some_and(|m| p.email.eq_ignore_ascii_case(m));
            let shown = if you { tr!("cal-you", name = name.clone()) } else { name.clone() };
            let external = my_domain.as_ref().is_some_and(|d| {
                p.email.split_once('@').is_some_and(|(_, pd)| !pd.eq_ignore_ascii_case(d))
            });
            let note = if p.owner {
                tr!("cal-organizer")
            } else if p.optional {
                tr!("cal-optional")
            } else {
                String::new()
            };
            let status = if p.owner { Partstat::Accepted } else { p.status };
            div()
                .h(px(34.))
                .flex()
                .items_center()
                .gap(px(10.))
                .child(avatar(&name, 28., l))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .truncate()
                                .when(status == Partstat::Declined, |d| d.line_through().text_color(l.text3()))
                                .child(shown),
                        )
                        .when(!note.is_empty(), |d| d.child(div().truncate().text_size(px(11.)).text_color(l.text2()).child(note))),
                )
                .when(external, |d| d.child(chip_outline(tr!("cal-external"), l)))
                .child(status_glyph(status, l))
        });
        let write: Vec<Address> = people
            .iter()
            .filter(|p| !me.is_some_and(|m| p.email.eq_ignore_ascii_case(m)) && !p.email.is_empty())
            .map(|p| Address::new(&p.name, &p.email))
            .collect();
        let has_people = people.len() > 1;
        div()
            .id("event-details")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(12.))
            .px(px(20.))
            .pt(px(18.))
            .pb(px(16.))
            .child(top)
            .child(title)
            .children(invite_block)
            .children(answered)
            .children(own_buttons)
            .children(own_line)
            .child(info)
            .when(has_people, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(tr!("cal-participants-n", n = people.len())),
                        )
                        .child(div().flex().items_center().gap(px(12.)).text_size(px(12.)).text_color(l.text2()).children(legend)),
                )
                .child(div().flex().flex_col().children(list))
            })
            .when(!e.description.trim().is_empty(), |d| {
                d.child(
                    div()
                        .pt(px(8.))
                        .border_t_1()
                        .border_color(l.border())
                        .line_height(px(19.))
                        .child(e.description.trim().to_string()),
                )
            })
            .when(!write.is_empty() && has_people, |d| {
                d.child(div().flex_1()).child(
                    crate::buttons::secondary("write-all", l)
                        .justify_center()
                        .child(icon(g::MAIL, 14.))
                        .child(tr!("cal-write-participants"))
                        .on_click(cx.listener(move |_, _, _, cx| cx.emit(CalendarAction::Write(write.clone())))),
                )
            })
    }
}

impl Render for CalendarView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let days = self.days();
        let middle = if self.view == View::Month { self.month_grid(cx).into_any_element() } else { self.grid(&days, cx).into_any_element() };
        div()
            .size_full()
            .flex()
            .min_w_0()
            .text_size(px(13.))
            .child(self.side(cx))
            .child(middle)
            .child(self.details(cx))
    }
}

/// Второй пояс из настроек: включён и известен.
fn second_zone(cal: &postman_core::prefs::Calendar) -> Option<TimeZone> {
    cal.second_zone_on.then(|| TimeZone::get(&cal.second_zone).ok()).flatten()
}

/// «МСК» для шапки сетки (короткое имя зоны или смещение).
fn zone_short(tz: &TimeZone) -> String {
    let now = jiff::Zoned::now().with_time_zone(tz.clone());
    let s = now.strftime("%Z").to_string();
    if s.is_empty() || s.starts_with(['+', '-']) { now.strftime("%:z").to_string() } else { s }
}

// --- командная строка раздела ---

/// Виды переключателя: вид, команда (клавиша 1…4), короткое и полное название.
const VIEWS: [(View, &str, &str, &str); 4] = [
    (View::Day, "calendar.view_day", "cal-view-day", "cal-view-day-full"),
    (View::WorkWeek, "calendar.view_work_week", "cal-view-work-week", "cal-view-work-week-full"),
    (View::Week, "calendar.view_week", "cal-view-week", "cal-view-week-full"),
    (View::Month, "calendar.view_month", "cal-view-month", "cal-view-month-full"),
];

/// Командная строка календаря (макет: на месте строки почты): «Создать встречу», «Сегодня», ‹ ›, период,
/// переключатель видов (правый край — по сетке) и поиск над панелью события.
pub(crate) struct CalendarBar {
    cal: Entity<CalendarView>,
    _subscription: Subscription,
}

impl CalendarBar {
    pub(crate) fn new(cal: Entity<CalendarView>, cx: &mut Context<Self>) -> Self {
        let sub = cx.observe(&cal, |_, _, cx| cx.notify());
        Self { cal, _subscription: sub }
    }
}

impl Render for CalendarBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let l = crate::roles::look(cx);
        let c = self.cal.read(cx);
        let days = c.days();
        let range = period_label(c.view, c.anchor, &days);
        let current = c.view;
        let search_state = c.search.clone();
        let loading = c.pending.is_some();
        let cal = self.cal.clone();
        let nav = |id: &'static str, glyph: char, n: i64, command: &'static str| {
            let cal = cal.clone();
            let b = crate::chrome::quiet_button(id, &l)
                .w(px(32.))
                .justify_center()
                .px_0()
                .child(icon(glyph, 14.))
                .on_click(move |_, _, cx| cal.update(cx, |c, cx| c.shift(n, cx)));
            crate::keytips::command(b, command, None, &l, cx)
        };
        let today = {
            let cal = self.cal.clone();
            let b = crate::chrome::quiet_button("cal-today", &l)
                .child(icon(g::TODAY, 16.))
                .child(tr!("cal-today"))
                .on_click(move |_, _, cx| cal.update(cx, |c, cx| c.today(cx)));
            crate::keytips::command(b, "calendar.today", Some(tr!("cal-today-tip")), &l, cx)
        };
        let new = div()
            .id("cal-new")
            .h(px(32.))
            .px(px(14.))
            .mr(px(12.))
            .flex()
            .items_center()
            .gap(px(8.))
            .rounded(l.px(Metric::RadiusControl))
            .bg(l.accent())
            .text_color(l.on_accent())
            .cursor_pointer()
            .hover(|d| d.bg(l.accent_hover()))
            .child(icon(g::ADD, 14.))
            .child(tr!("cal-new-meeting"))
            .on_click(|_, window, cx| window.dispatch_action(Box::new(crate::calendar::calendar::NewMeeting), cx));
        let new = crate::keytips::command(new, "calendar.new_meeting", Some(tr!("cal-new-meeting-tip")), &l, cx);
        let views = VIEWS.into_iter().map(|(v, command, short, full)| {
            let cal = self.cal.clone();
            let on = v == current;
            let b = div()
                .id(command)
                .h(px(28.))
                .px(px(12.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .cursor_pointer()
                .when(on, |d| d.bg(l.panel()).font_weight(FontWeight::SEMIBOLD).shadow_sm())
                .when(!on, |d| d.text_color(l.text2()).hover(|d| d.bg(l.hover())))
                .child(tr!(short))
                .on_click(move |_, _, cx| cal.update(cx, |c, cx| c.set_view(v, cx)));
            crate::keytips::command(b, command, Some(tr!(full)), &l, cx)
        });
        let next_cal = self.cal.clone();
        let prev_cal = self.cal.clone();
        let search = div()
            .w(px(DETAILS_W))
            .ml(px(8.))
            .flex_none()
            .child(Input::new(&search_state).prefix(icon(g::SEARCH, 14.).text_color(l.text3())))
            .capture_action(move |_: &MoveDown, _, cx| {
                if next_cal.update(cx, |c, cx| c.step_result(1, cx)) {
                    cx.stop_propagation();
                }
            })
            .capture_action(move |_: &MoveUp, _, cx| {
                if prev_cal.update(cx, |c, cx| c.step_result(-1, cx)) {
                    cx.stop_propagation();
                }
            });
        div()
            .size_full()
            .flex()
            .items_center()
            .gap(px(2.))
            .pl(px(12.))
            .pr(px(8.))
            .text_color(l.text())
            .child(new)
            .child(today)
            .child(nav("cal-prev", g::PREV, -1, "calendar.prev_week"))
            .child(nav("cal-next", g::NEXT, 1, "calendar.next_week"))
            .child(div().px(px(8.)).text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child(range))
            .when(loading, |d| d.child(div().text_color(l.text2()).child(tr!("cal-loading"))))
            .child(div().flex_1())
            .child(div().flex().p(px(2.)).rounded(px(6.)).bg(l.fill()).children(views))
            .child(crate::keytips::badge(search, crate::keytips::key("search.focus", cx), &l, cx))
    }
}

impl crate::Mailer {
    /// Просьба календаря: команда синхронизации, вкладка встречи, письмо или тост. Нет сервера — «нужен сервер».
    pub(crate) fn on_calendar(&mut self, action: &CalendarAction, cx: &mut Context<Self>) {
        let command = match action {
            CalendarAction::Load { from, to } => Command::Events { from: *from, to: *to },
            CalendarAction::Search { text, from, to } => Command::SearchEvents { text: text.clone(), from: *from, to: *to },
            CalendarAction::Share(email) => Command::ShareFreeBusy { email: email.clone() },
            CalendarAction::Respond { event, status } => Command::RespondInvite {
                invite: Box::new(Invitation {
                    method: "REQUEST".into(),
                    sequence: event.sequence,
                    raw: serde_json::json!({ "uid": event.uid }),
                    event: (**event).clone(),
                }),
                status: *status,
                in_reply_to: None,
            },
            CalendarAction::Write(to) => {
                self.open_compose(Draft { to: to.clone(), ..Default::default() }, cx);
                return;
            }
            CalendarAction::Edit { event, series } => {
                let tz = crate::prefs::get(cx).calendar.zone();
                self.open_meeting(Some(crate::meeting::MeetingModel::editing(event, *series, &tz)), cx);
                return;
            }
            CalendarAction::Cancel { event, series, ticket } => Command::CancelMeeting {
                event: event.clone(),
                series: *series,
                from: event.organizer().map(|o| o.email.clone()).filter(|e| !e.is_empty()),
                ticket: *ticket,
            },
            CalendarAction::Toast(level, text) => {
                self.toasts.update(cx, |t, cx| t.push(*level, text.clone(), cx));
                return;
            }
        };
        let respond = matches!(action, CalendarAction::Respond { .. });
        let cancel = matches!(action, CalendarAction::Cancel { .. });
        if !self.backend.send(command) {
            self.needs_server(cx);
            self.cal.update(cx, |c, cx| match (respond, cancel) {
                (true, _) => c.respond_refused(cx),
                (_, true) => c.cancel_refused(cx),
                _ => c.not_sent(cx),
            });
        }
    }

    /// Листание периода (`Some(±1)`) и «Сегодня» (`None`) — в разделе «Календарь».
    pub(crate) fn calendar_week(&mut self, periods: Option<i64>, cx: &mut Context<Self>) {
        if self.section != crate::Section::Calendar {
            return;
        }
        self.cal.update(cx, |c, cx| match periods {
            Some(n) => c.shift(n, cx),
            None => c.today(cx),
        });
    }

    /// 1…4: вид календаря — в разделе «Календарь».
    pub(crate) fn calendar_view(&mut self, view: View, cx: &mut Context<Self>) {
        if self.section == crate::Section::Calendar {
            self.cal.update(cx, |c, cx| c.set_view(view, cx));
        }
    }
}

// --- общие элементы раздела (календарь, встреча, контакты) ---

/// Кнопка-значок 28 px без рамки.
pub(crate) fn icon_button(id: impl Into<ElementId>, glyph: char, l: &Look) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(28.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(l.px(Metric::RadiusControl))
        .text_color(l.text2())
        .cursor_pointer()
        .hover(|d| d.bg(l.hover()))
        .child(icon(glyph, 12.))
}

/// Чип с рамкой («Внешний»).
pub(crate) fn chip_outline(text: String, l: &Look) -> Div {
    div()
        .flex_none()
        .px(px(6.))
        .rounded(px(4.))
        .border_1()
        .border_color(l.stroke())
        .text_size(px(11.))
        .text_color(l.text2())
        .child(text)
}

/// Буквы имени для кружка: «Иванова Мария» → «ИМ», адрес → первая буква.
pub(crate) fn initials(name: &str) -> String {
    let name = name.split('@').next().unwrap_or(name);
    let name = if name.contains(' ') { name } else { name.split('.').next_back().unwrap_or(name) };
    let words: Vec<&str> = name.split_whitespace().collect();
    let first = |w: &str| w.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default();
    match words.as_slice() {
        [] => String::new(),
        [one] => first(one),
        [a, b, ..] => first(a) + &first(b),
    }
}

/// Кружок с буквами имени.
pub(crate) fn avatar(name: &str, size: f32, l: &Look) -> Div {
    div()
        .size(px(size))
        .flex_none()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(l.fill())
        .text_color(l.text2())
        .text_size(px((size * 0.4).round().max(10.)))
        .child(initials(name))
}

/// Значок ответа участника 20 px: принял, под вопросом, отклонил, нет ответа.
pub(crate) fn status_glyph(s: Partstat, l: &Look) -> Div {
    status_glyph_sized(s, 20., l)
}

/// Значок ответа участника (DESIGN: заливные кружки со значком; «нет ответа» — серая точечная заливка).
fn status_glyph_sized(s: Partstat, size: f32, l: &Look) -> Div {
    let base = div().size(px(size)).flex_none().rounded_full().flex().items_center().justify_center();
    let glyph = (size * 0.5).round();
    match s {
        Partstat::Accepted | Partstat::Delegated => base.bg(l.success()).text_color(l.on_accent()).child(icon(g::CHECK, glyph)),
        Partstat::Tentative => base
            .bg(l.pm(Pm::FbBusyLine))
            .text_color(l.on_accent())
            .text_size(px(size * 0.6))
            .font_weight(FontWeight::BOLD)
            .child("?"),
        Partstat::Declined => base.bg(l.text3()).text_color(l.on_accent()).child(icon(g::CANCEL, glyph - 1.)),
        Partstat::NeedsAction => dotted(size, l.text3()),
    }
}

/// Кружок с точечной заливкой и тонкой обводкой: точки — внутри круга, шаг 4 px.
fn dotted(size: f32, color: Hsla) -> Div {
    let (step, dot, r) = (4.0_f32, 1.6_f32, size / 2.);
    let n = (size / step) as usize;
    let dots = (0..n).flat_map(move |row| (0..n).map(move |col| (row, col))).filter_map(move |(row, col)| {
        let (x, y) = (step / 2. + col as f32 * step, step / 2. + row as f32 * step);
        let inside = (x - r).hypot(y - r) <= r - 2.5;
        inside.then(|| div().absolute().left(px(x - dot / 2.)).top(px(y - dot / 2.)).size(px(dot)).rounded_full().bg(color))
    });
    div().size(px(size)).flex_none().relative().rounded_full().border_1().border_color(color).children(dots)
}

/// Опись части (ADR 0005, `parts.rs`).
pub(crate) const PART: crate::part::Part = crate::part::Part { name: "calendar", texts: TEXTS, commands: COMMANDS, handlers, scenes: scenes::SCENES, ..crate::part::Part::NONE };

/// Обработчики команд части — на корне экрана почты (`Mailer::render`).
fn handlers(root: Div, cx: &mut Context<crate::Mailer>) -> Div {
    root.on_action(cx.listener(|this, _: &calendar::NewMeeting, _, cx| {
            this.open_meeting(None, cx);
        }))
        .on_action(cx.listener(|this, _: &calendar::PrevWeek, _, cx| this.calendar_week(Some(-1), cx)))
        .on_action(cx.listener(|this, _: &calendar::NextWeek, _, cx| this.calendar_week(Some(1), cx)))
        .on_action(cx.listener(|this, _: &calendar::Today, _, cx| this.calendar_week(None, cx)))
        .on_action(cx.listener(|this, _: &calendar::ViewDay, _, cx| this.calendar_view(postman_core::prefs::CalendarView::Day, cx)))
        .on_action(cx.listener(|this, _: &calendar::ViewWorkWeek, _, cx| this.calendar_view(postman_core::prefs::CalendarView::WorkWeek, cx)))
        .on_action(cx.listener(|this, _: &calendar::ViewWeek, _, cx| this.calendar_view(postman_core::prefs::CalendarView::Week, cx)))
        .on_action(cx.listener(|this, _: &calendar::ViewMonth, _, cx| this.calendar_view(postman_core::prefs::CalendarView::Month, cx)))
}

/// Тексты части (`i18n/<язык>/<часть>.ftl`).
const TEXTS: &[pets_i18n::Resource] = &[
    ("ru", include_str!("../i18n/ru/calendar.ftl")),
    ("en", include_str!("../i18n/en/calendar.ftl")),
];

// Имя — область действий (`calendar::…`), как в id команд: так обработчики читаются и их находит `docs/PARITY.md`.
#[allow(clippy::module_inception)]
pub(crate) mod calendar {
    gpui_kit::actions!(calendar, [NewMeeting, PrevWeek, NextWeek, Today, ViewDay, ViewWorkWeek, ViewWeek, ViewMonth]);
}

/// Команды части (`commands.rs`: контексты и `cmd`).
const COMMANDS: &[pets_commands::Command] = {
    use crate::commands::{CALENDAR, cmd};
    &[
        cmd("calendar.new_meeting", &["ctrl-shift-q"], None),
        // DESIGN «Хоткеи»: период вида — PgUp / PgDn (id — с недели, S3-05), «Сегодня» — Home (Alt не используется; в
        // Outlook — Alt+↑/↓, Alt+Home). Виды — 1…4 вне полей ввода (S5-21; в Outlook — Ctrl+Alt+1…4, Alt не берём).
        cmd("calendar.prev_week", &["pageup"], CALENDAR),
        cmd("calendar.next_week", &["pagedown"], CALENDAR),
        cmd("calendar.today", &["home"], CALENDAR),
        cmd("calendar.view_day", &["1"], CALENDAR),
        cmd("calendar.view_work_week", &["2"], CALENDAR),
        cmd("calendar.view_week", &["3"], CALENDAR),
        cmd("calendar.view_month", &["4"], CALENDAR),
    ]
};

/// Сцены `--screens` календаря (ADR 0005).
pub(crate) mod scenes {
    use gpui_kit::*;
    use postman_core::prefs::CalendarView as V;

    use crate::screens::{self, Scene};
    use crate::{Mailer, Section};

    pub(crate) const SCENES: &[Scene] = &[
        // Раздел «Календарь»: неделя и приглашение без ответа в подробностях (S5-11).
        Scene::new("calendar", week, week_ready).live(),
        // Календарь с подробностями самого людного события (S3-05).
        Scene::new("event", event, event_ready).live(),
        // Своя встреча с участниками (S7-07б): в подробностях — «Изменить» и «Отменить встречу».
        Scene::new("event-organizer", own, event_ready),
        // «Отменить встречу» у своей повторяющейся встречи: строка «Только эту / Всю серию / Не отменять».
        Scene::new("event-cancel", own, event_ready),
        // Календари коллег (S5-22): события коллег цветом их календаря и с именем владельца, слева «Календари коллег»;
        // открыто событие из календаря коллеги.
        Scene::new("calendar-colleagues", colleagues, colleagues_ready),
        // Виды календаря (S5-21): «День» — среда демо-недели, «Неделя» — с выходными, «Месяц» — клетки с полосами
        // многодневных и строками событий, «+N ещё».
        Scene::new("calendar-day", view, view_ready),
        Scene::new("calendar-week", view, view_ready),
        Scene::new("calendar-month", view, view_ready),
        // Поиск над панелью события: найденное списком с датами (демо — по всем событиям).
        Scene::new("calendar-search", search, search_ready),
    ];

    /// Запрос снимка `calendar-search`: место нескольких встреч демо (планёрки, демо продукта, итоги недели).
    const QUERY: &str = "Переговорная 1"; // i18n-ok: место в демо-данных

    fn week(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.close_palette(window, cx);
        // Как `search::Clear`: календарь снимается без следов поиска в папках.
        m.search.update(cx, |s, cx| s.set_value("", window, cx));
        m.run_search("", cx);
        m.set_section(Section::Calendar, cx);
        // «Сейчас» — 14:20 сегодня: линия времени видна при любом часе съёмки.
        let today = jiff::Zoned::now().date().at(14, 20, 0, 0);
        let clock = today.to_zoned(jiff::tz::TimeZone::system()).ok().map(|z| z.timestamp().as_second());
        m.cal.update(cx, |c, cx| {
            c.clock = clock;
            c.escape(window, cx);
            c.set_view(V::WorkWeek, cx);
            c.go_to_demo_week(cx);
            c.select_pending(cx);
        });
        Ok(())
    }

    fn week_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        let c = m.cal.read(cx);
        // Живой ящик: неделя пришла с сервера.
        let loaded = c.loaded || screens::demo(m);
        m.section == Section::Calendar && c.clock.is_some() && loaded
    }

    fn event(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.cal.update(cx, |c, cx| c.select_busiest(cx));
        m.set_section(Section::Calendar, cx);
        Ok(())
    }

    fn own(s: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        let cancel = s.name == "event-cancel";
        m.cal.update(cx, |c, cx| c.select_own(cancel, cx));
        m.set_section(Section::Calendar, cx);
        Ok(())
    }

    fn event_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        // Живая неделя может быть пустой: тогда снимок — без подробностей.
        let c = m.cal.read(cx);
        m.section == Section::Calendar && (c.selected() || c.is_empty())
    }

    fn colleagues(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.cal.update(cx, |c, cx| c.select_colleague(cx));
        m.set_section(Section::Calendar, cx);
        Ok(())
    }

    fn colleagues_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        m.section == Section::Calendar && m.cal.read(cx).selected()
    }

    fn view_of(s: &Scene) -> V {
        match s.name {
            "calendar-day" => V::Day,
            "calendar-week" => V::Week,
            _ => V::Month,
        }
    }

    fn view(s: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        let view = view_of(s);
        m.set_section(Section::Calendar, cx);
        m.cal.update(cx, |c, cx| {
            c.set_view(view, cx);
            // День — среда демо-недели: пересечения и «под вопросом».
            if view == V::Day { c.go_to_demo_day(2, cx) } else { c.go_to_demo_week(cx) }
            c.select_pending(cx);
        });
        Ok(())
    }

    fn view_ready(s: &Scene, m: &Mailer, cx: &App) -> bool {
        m.section == Section::Calendar && m.cal.read(cx).view == view_of(s)
    }

    fn search(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.set_section(Section::Calendar, cx);
        m.cal.update(cx, |c, cx| {
            c.set_view(V::WorkWeek, cx);
            c.go_to_demo_week(cx);
            c.type_query(QUERY, window, cx);
        });
        Ok(())
    }

    fn search_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        m.section == Section::Calendar && m.cal.read(cx).results_ready()
    }
}

#[cfg(test)]
mod tests {
    // Не `super::*`: из `gpui_kit::*` пришёл бы атрибут `test` gpui вместо встроенного.
    use super::{Date, Event, Shown, TimeZone, View, WorkHours, add_days, day_events, day_start, event_look, initials, monday_of, period_label, range_label};
    use postman_calendar::{Busy, BusyKind, Cell, Span, grid};

    fn ts(s: &str) -> i64 {
        s.parse::<jiff::Timestamp>().unwrap().as_second()
    }

    #[test]
    fn server_events_land_on_their_days() {
        let tz = TimeZone::get("Europe/Moscow").unwrap();
        let v = |id: &str, start: &str, end: &str, all_day: bool| serde_json::json!({ "id": id, "title": id, "utcStart": start, "utcEnd": end, "showWithoutTime": all_day });
        let values = [
            v("mon", "2026-10-19T07:00:00Z", "2026-10-19T08:00:00Z", false),
            // 23:30 МСК понедельника — ещё понедельник, хотя в UTC уже 20:30 того же дня.
            v("late", "2026-10-19T20:30:00Z", "2026-10-19T21:00:00Z", false),
            v("wed-all", "2026-10-20T21:00:00Z", "2026-10-21T21:00:00Z", true),
            v("sun", "2026-10-25T09:00:00Z", "2026-10-25T10:00:00Z", false),
            v("prev", "2026-10-16T09:00:00Z", "2026-10-16T10:00:00Z", false),
        ];
        let events: Vec<Event> = postman_core::schedule::parse_events(&values, &tz);
        let monday = monday_of(Date::constant(2026, 10, 22));
        assert_eq!(monday, Date::constant(2026, 10, 19));
        let ids = |d: i64| {
            let (timed, all) = day_events(&events, add_days(monday, d), &tz);
            (
                timed.iter().map(|&i| events[i].id.as_str()).collect::<Vec<_>>(),
                all.iter().map(|&i| events[i].id.as_str()).collect::<Vec<_>>(),
            )
        };
        assert_eq!(ids(0), (vec!["mon", "late"], vec![]));
        assert_eq!(ids(1), (vec![], vec![]));
        assert_eq!(ids(2), (vec![], vec!["wed-all"]));
        assert_eq!(ids(6), (vec!["sun"], vec![]), "воскресенье — в неделе «Неделя»");
        // Прошлая неделя в эту не попадает.
        let shown: usize = (0..7).map(|d| ids(d).0.len() + ids(d).1.len()).sum();
        assert_eq!(shown, 4);
        assert_eq!(day_start(add_days(monday, 7), &tz) - day_start(monday, &tz), 7 * 86_400);
    }

    #[test]
    fn no_data_is_not_free() {
        let tz = TimeZone::UTC;
        let hours = WorkHours::standard(tz.clone());
        let day = ts("2026-10-19T00:00:00Z");
        let range = Span::new(day + 8 * 3600, day + 20 * 3600);
        let busy = |h: i64| Busy { span: Span::new(day + h * 3600, day + (h + 1) * 3600), kind: BusyKind::Busy };
        let people = [
            ("me".to_string(), Some(vec![busy(9)])),
            ("shared".to_string(), Some(vec![busy(10)])),
            ("not shared".to_string(), None),
        ];
        let cells = grid(&people.iter().map(|(_, b)| b.clone()).collect::<Vec<_>>(), range, 1800, &hours);
        // 09:00 у меня занято, у коллеги без шаринга — «нет данных» во всех ячейках, не «свободно».
        assert_eq!(cells[0][2], Cell::Busy);
        assert_eq!(cells[1][2], Cell::Free);
        assert!(cells[2].iter().all(|c| *c == Cell::Unknown));
    }

    /// Вид блока по моему ответу: жду ответа — пунктир, «под вопросом» — штрих, отклонено — скрыто; организатор —
    /// обычный.
    #[test]
    fn event_look_follows_my_answer() {
        let ev = |mine: &str, owner: bool| {
            let v = serde_json::json!({ "utcStart": "2026-10-19T07:00:00Z", "utcEnd": "2026-10-19T08:00:00Z", "participants": {
                "o": { "calendarAddress": "mailto:boss@corp.example", "roles": { "owner": !owner } },
                "m": { "calendarAddress": "mailto:me@corp.example", "roles": { "attendee": true, "owner": owner }, "participationStatus": mine }
            }});
            Event::from_jmap(&v, &TimeZone::UTC).unwrap()
        };
        let me = Some("ME@corp.example");
        assert_eq!(event_look(&ev("needs-action", false), me), Shown::Pending);
        assert_eq!(event_look(&ev("tentative", false), me), Shown::Tentative);
        assert_eq!(event_look(&ev("declined", false), me), Shown::Hidden);
        assert_eq!(event_look(&ev("accepted", false), me), Shown::Normal);
        assert_eq!(event_look(&ev("needs-action", true), me), Shown::Normal, "своя встреча");
        assert_eq!(event_look(&ev("needs-action", false), None), Shown::Normal, "адрес не известен");
        let mut t = ev("accepted", false);
        t.status = postman_calendar::EventStatus::Tentative;
        assert_eq!(event_look(&t, me), Shown::Tentative);
    }

    #[test]
    fn labels_for_people_and_weeks() {
        assert_eq!(initials("Иванова Мария"), "ИМ");
        assert_eq!(initials("m.ivanova@corp.example"), "I");
        assert_eq!(initials(""), "");
        assert_eq!(initials("Переговорная 3"), "П3");
        let d = |m, day| Date::constant(2026, m, day);
        assert!(range_label(d(10, 19), d(10, 23)).starts_with("19–23 "));
        assert!(range_label(d(9, 28), d(10, 2)).starts_with("28 "));
        assert!(range_label(d(10, 21), d(10, 21)).starts_with("21 "));
        // Заголовок месяца — без диапазона дней сетки.
        assert!(!period_label(View::Month, d(10, 21), &[d(9, 28), d(11, 1)]).contains('–'));
    }
}
