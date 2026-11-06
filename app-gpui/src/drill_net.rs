//! Сценарии с вводом для того, что идёт в сеть мимо почтового сервера: занятость по ссылке (S8-42), подписка на календарь
//! (S8-44), каталог сертификатов (S5-12.5). Вторая сторона — `docs/stand/net.py`: https-источники за CONNECT-прокси на 127.0.0.1,
//! JMAP без писем и контейнер OpenLDAP (`docs/DEV.md`, «Стенд сети»). Все данные — `example.test`.
//!
//! Цепочку сертификата проверяет Windows, как всегда; корень стенда добавляет только отладочная сборка (`POSTMAN_TEST_CA`).
//! Сцены `net-tls-…` и `net-s5125-chain` показывают обратное: недоверенный сертификат, чужое имя, запуск без корня — отказ.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use gpui_kit::*;
use jiff::civil::Date;
use pets_i18n::tr;
use postman_calendar::{Busy, BusyKind, EventStatus, FreeBusy};
use postman_core::prefs::{Calendar, CalendarView as View, DirectoryLogin, Prefs};

use crate::calendar::{CalendarView, add_days, day_start};
use crate::meeting::MeetingView;
use crate::screens::{self, Input, Scene, compose_tab, settings_on, spot_bounds};
use crate::settings_tab::{Section as S, SettingsView};
use crate::smime_seal::DirectoryStatus;
use crate::tabs::TabKind;
use crate::{Mailer, Section};

pub(crate) const PART: crate::part::Part = crate::part::Part { name: "drill-net", scenes: SCENES, ..crate::part::Part::NONE };

/// Шаг со своим условием.
macro_rules! until {
    (|$m:tt, $cx:tt| $done:expr) => {
        Input::Until(|_, $m, $cx| $done)
    };
}

const fn scene(name: &'static str, enter: screens::Enter, ready: screens::Ready, input: &'static [Input]) -> Scene {
    Scene::new(name, enter, ready).input(input).live().on_request()
}

const fn more(name: &'static str, ready: screens::Ready, input: &'static [Input]) -> Scene {
    Scene::new(name, start, ready).input(input).live().on_request().continues()
}

const SCENES: &[Scene] = &[
    // S8-42, шаги 3, 4: ссылка на `.ics`: «Загружаю…», занятость участника, подпись, счётчик и окна; повторы, пояса, «под вопросом»,
    // прозрачные и отменённые не показаны.
    scene("net-s842-link", meeting, guest_series, LINK),
    // S8-42, шаг 5: «Ссылка занятости …» открывает строку, «Убрать» — снова «Указать ссылку».
    more("net-s842-change", link_removed, CHANGE),
    // S8-42, шаг 6: ссылка отвечает 404.
    more("net-s842-missing", in_time, MISSING),
    // S8-42, шаг 6: сервера нет.
    more("net-s842-nohost", in_time, NO_HOST),
    // S8-42, шаг 6: сервер молчит: «недоступна» не позже чем через 5 с, окно не замирает.
    more("net-s842-silent", in_time, SILENT),
    // Цепочка: сертификат чужого корня — отказ, запроса нет (сверяет `net.py`).
    more("net-tls-untrusted", in_time, UNTRUSTED),
    // Цепочка: сертификат другого имени — отказ, запроса нет.
    more("net-tls-name", in_time, WRONG_NAME),
    // S8-42, шаги 3, 4: файл VFREEBUSY.
    more("net-s842-vfb", guest_vfb, VFB),
    // S8-42, шаг 8: другая неделя — занятость запрошена для неё.
    more("net-s842-next", next_week, NEXT_WEEK),
    // S8-42, шаг 7: контакт с `FBURL`: занятость сама, вводить ссылку не нужно.
    scene("net-s842-contact", meeting, booked_shown, CONTACT),
    // S8-42, шаг 7: правка контакта в Postman ссылку не стирает.
    scene("net-s842-edit", start, booked_shown, EDIT_CONTACT),
    // S8-42, шаг 8: перезапуск: ссылка из настроек, занятость без ввода.
    scene("net-s842-kept", meeting, guest_vfb, KEPT_LINK),
    // Цепочка: тот же сервер без корня стенда — отказ.
    scene("net-tls-noroot", meeting, in_time, NO_ROOT),
    // S8-44, шаг 2: `https://`: календарь с именем из файла, события в неделе.
    scene("net-s844-add", week, |_, m, cx| subs(m, cx).len() == 1 && subs(m, cx)[0].0 == FIRST_NAME && m.cal.read(cx).has_subscription_events(), ADD),
    // S8-44, шаг 2: `webcal://`.
    more("net-s844-webcal", |_, m, cx| subs(m, cx).len() == 2 && subs(m, cx)[1].0 == SECOND_NAME, WEBCAL_ADD),
    // S8-44, шаг 3: весь день — в свой день, повторы, часовые пояса, отменённые не показаны.
    more("net-s844-week", week_matches, &[]),
    // S8-44, шаг 4: «Подписка», ответа нет, изменить и удалить нельзя.
    more("net-s844-event", event_read_only, OPEN_EVENT),
    // S8-44, шаг 5: флажок скрывает события.
    more("net-s844-hide", |_, m, cx| visible(m, cx).is_empty() && hidden(cx), HIDE),
    // S8-44, шаг 5: цвет; события снова видны.
    more("net-s844-color", colored, COLOR),
    // S8-44, шаг 8: «Обновить»: запрос с `If-None-Match` получает 304 (сверяет `net.py`).
    more("net-s844-304", |_, m, cx| !fetching(m, cx) && !failed(m, cx), REFRESH),
    // S8-44, шаг 8: файл изменился: новая версия по тому же запросу, событие в неделе.
    more("net-s844-fresh", |_, m, cx| !fetching(m, cx) && visible(m, cx).contains(&FRESH.to_owned()), REFRESH_MENU_OPEN),
    // S8-44, шаг 8: «Обновлять подписки»: срок вышел — файл спрошен снова.
    scene("net-s844-timer", calendar_settings, |_, m, cx| !fetching(m, cx) && colors_named(cx), TIMER),
    // S8-44, шаг 9: встреча поверх события подписки: на мою занятость подписка не влияет.
    scene("net-s844-busy", meeting, my_time_free, BUSY_FREE),
    // S8-44, шаг 6: перезапуск без сети: подписка, цвет, прежние события, «Не обновился».
    scene("net-s844-kept", week, kept_offline, KEPT),
    // S8-44, шаг 7: «Удалить»: календари и события ушли (файлы сверяет `net.py`).
    scene("net-s844-remove", week, |_, m, cx| subs(m, cx).is_empty() && visible(m, cx).is_empty(), REMOVE),
    // Известные дефекты воркера (группа `defects`), S8-44, шаг 3, и S8-42, шаг 4: бесконечные серии календаря.
    scene("net-series-endless", week, all_series, ENDLESS),
    // Серия со временем UTC (`DTSTART:…Z`) — в свой час при поясе показа не UTC.
    scene("net-series-utc", week, utc_series, UTC),
    // S5-12.5, шаги 1, 6: `ldaps://`, имя и пароль: «Сохранён…», запись в Диспетчере; «Проверить» — отвечает.
    scene("net-s5125-ldaps", password_login, |_, m, cx| check_ok(m, cx, 1) && password_kept(LDAPS), LDAPS_CHECK),
    // S5-12.5, шаг 7: имя и пароль по `ldap://` без StartTLS: «Каталог недоступен», входа на сервере нет (сверяет `net.py`).
    scene("net-s5125-plain", password_plain, |_, m, cx| check_is(m, cx, &tr!("directory-unavailable")), PLAIN_CHECK),
    // S5-12.5, шаг 7: со StartTLS — отвечает.
    more("net-s5125-starttls", |_, m, cx| check_ok(m, cx, 1) && password_kept(PLAIN), STARTTLS_CHECK),
    // S5-12.5, шаг 8: «Без входа», база пустая — из `namingContexts`, сертификат находится.
    scene("net-s5125-anon", anonymous, |_, m, cx| check_ok(m, cx, 1), ANON),
    // S5-12.5, шаг 2: «Найти в каталоге»: «Ищу…», замечание пропало, у адресата — замок.
    scene("net-s5125-find", letter, |_, m, cx| no_certificate(m, cx).is_empty() && locked(m, cx, BOB) && locked(m, cx, ALICE), FIND),
    // S5-12.5, шаг 4: адресата нет в каталоге.
    scene("net-s5125-none", letter, |_, m, cx| found_none(m, cx, DAVE), NONE),
    // S5-12.5, шаг 5: сертификат чужого корня: воркер не принял, замка нет.
    scene("net-s5125-untrusted", letter, |_, m, cx| found_none(m, cx, CAROL), FOREIGN),
    // S5-12.5, шаг 4: сеть выключена: «Каталог недоступен…», окно не замирает.
    scene("net-s5125-down", server_down, |_, m, cx| directory_failed(m, cx, "directory-unavailable"), DOWN),
    // S5-12.5, шаг 4: каталог молчит: не позже чем через 5 с.
    scene("net-s5125-silent", server_silent, |_, m, cx| directory_failed(m, cx, "directory-timeout"), HANGS),
    // Цепочка каталога без корня стенда: `ldaps://` и StartTLS отказывают, открытый канал отвечает — сервер жив.
    scene("net-s5125-chain", anonymous_ldaps, |_, m, cx| check_ok(m, cx, 1), CHAIN),
];

/// Источники (`docs/stand/net.py`): имена настоящие для клиента, сервера за прокси на 127.0.0.1.
const SUBSCRIPTION: &str = "https://cal.example.test/public/basic.ics";
const ENDLESS_ICS: &str = "https://cal.example.test/public/endless.ics";
const UTC_ICS: &str = "https://cal.example.test/public/utc.ics";
const SECOND_WEBCAL: &str = "webcal://cal.example.test/public/second.ics";
const LINK_ICS: &str = "https://cal.example.test/free-busy/guest.ics";
const LINK_VFB: &str = "https://cal.example.test/free-busy/guest.vfb";
const LINK_MISSING: &str = "https://cal.example.test/free-busy/missing.vfb";
const LINK_NO_HOST: &str = "https://gone.example.test/a.vfb";
const LINK_SILENT: &str = "https://silent.example.test/a.vfb";
const LINK_UNTRUSTED: &str = "https://untrusted.example.test/a.vfb";
const LINK_WRONG_NAME: &str = "https://wrongname.example.test/a.vfb";
const FIRST_NAME: &str = "Стенд подписка"; // i18n-ok: данные источника
const SECOND_NAME: &str = "Стенд второй"; // i18n-ok: данные источника
/// Событие новой версии файла.
const FRESH: &str = "После обновления"; // i18n-ok: данные источника
/// Событие подписки в рабочие часы: на него щёлкает сцена.
const PLAIN_EVENT: &str = "Время UTC"; // i18n-ok: данные источника

const GUEST: &str = "guest@partner.example.test";
const GUEST_ROW: &str = "link-guest@partner.example.test";
const GUEST_CHANGE: &str = "link-change-guest@partner.example.test";
const BOOKED: &str = "booked@partner.example.test";
const ALICE: &str = "alice@example.test";
const BOB: &str = "bob@example.test";
const CAROL: &str = "carol@example.test";
const DAVE: &str = "dave@example.test";
const ERIN: &str = "erin@example.test";
const FRANK: &str = "frank@example.test";

const LDAPS: &str = "ldaps://localhost:16360";
const PLAIN: &str = "ldap://localhost:13890";
const CLOSED: &str = "ldap://localhost:13899";
const QUIET: &str = "ldap://localhost:13891";
const READER: &str = "cn=reader,dc=example,dc=test";

/// «Не позже чем через 5 с»: срок сети и запас на кадр.
const LIMIT: Duration = Duration::from_millis(5600);
/// Окно не замирает: самый долгий промежуток между кадрами отладочной сборки.
const STALL: Duration = Duration::from_millis(1500);

thread_local! {
    /// Шаг, с которого идёт ожидание, и последний кадр.
    static STARTED: Cell<Option<Instant>> = const { Cell::new(None) };
    static FRAME: Cell<Option<Instant>> = const { Cell::new(None) };
    /// Самая долгая пауза между кадрами в ожидании.
    static GAP: Cell<Duration> = const { Cell::new(Duration::ZERO) };
    /// Время ответа последнего ожидания.
    static TOOK: Cell<Option<Duration>> = const { Cell::new(None) };
    /// Положение элемента на прошлом кадре и сколько кадров оно не менялось.
    static STEADY: Cell<(Option<Pixels>, u32)> = const { Cell::new((None, 0)) };
    /// Строк помощника без данных до ссылки.
    static UNKNOWN: Cell<usize> = const { Cell::new(0) };
    static LAST_NOTE: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Несошедшееся значение — в поток вывода, один раз: разбор падения сцены.
fn note(text: String) {
    if LAST_NOTE.with_borrow(|l| *l != text) {
        println!("net: {text}");
        LAST_NOTE.set(text);
    }
}

fn start(_: &Scene, _: &mut Mailer, _: &mut Window, _: &mut Context<Mailer>) -> Result<(), String> {
    Ok(())
}

// --- занятость по ссылке ---

fn meeting(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    m.close_all_meetings(window, cx);
    m.set_section(Section::Calendar, cx);
    m.open_meeting_screen(cx);
    Ok(())
}

fn view<'a>(m: &Mailer, cx: &'a App) -> Option<&'a MeetingView> {
    m.meetings.last().map(|v| v.read(cx))
}

const OPENED: Input =
    until!(|m, cx| m.tabs.is_active(TabKind::Meeting) && view(m, cx).is_some_and(MeetingView::ready) && spot_bounds("assistant").is_some());

/// Участник с внешним адресом: чип, ответ сервера получен.
const ADD_GUEST: &[Input] = &[
    OPENED,
    Input::Keys(&["ctrl-shift-b"]),
    Input::Text(GUEST),
    Input::Keys(&["enter"]),
    until!(|m, cx| view(m, cx).is_some_and(|v| v.chips().0 == 1)),
];

const LINK: &[Input] = &[
    OPENED,
    Input::Keys(&["ctrl-shift-b"]),
    Input::Text(GUEST),
    Input::Keys(&["enter"]),
    until!(|m, cx| view(m, cx).is_some_and(|v| v.chips().0 == 1 && v.ready())),
    Input::Run(remember_unknown),
    Input::Press(GUEST_ROW),
    Input::Text(LINK_ICS),
    Input::Keys(&["enter"]),
    until!(|m, cx| view(m, cx).is_some_and(|v| v.link_loading(GUEST))),
];

/// Строк без данных до ссылки: «счётчик вырос на одного».
fn remember_unknown(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    UNKNOWN.set(view(m, cx).map_or(0, MeetingView::without_data));
    Ok(())
}

/// Время в поясе `zone`.
fn at(day: Date, hour: i8, minute: i8, zone: &str) -> Option<i64> {
    day.at(hour, minute, 0, 0).in_tz(zone).ok().map(|z| z.timestamp().as_second())
}

fn span_of(b: &Busy) -> (i64, i64, BusyKind) {
    (b.span.start, b.span.end, b.kind)
}

/// Что должно быть в неделе от `guest.ics` (`ICS_EVENTS` в `net.py`): прозрачное и отменённое — нет.
fn series(monday: Date) -> Option<Vec<(i64, i64, BusyKind)>> {
    let span = |day: i64, from: (i8, i8), minutes: i64, zone: &str, kind: BusyKind| {
        at(add_days(monday, day), from.0, from.1, zone).map(|start| (start, start + minutes * 60, kind))
    };
    let mut out = vec![
        span(0, (12, 0), 60, "Europe/Moscow", BusyKind::Busy)?,
        span(1, (10, 0), 60, "Asia/Tokyo", BusyKind::Busy)?,
        span(1, (15, 0), 60, "UTC", BusyKind::Tentative)?,
        span(3, (9, 0), 90, "UTC", BusyKind::Busy)?,
        span(4, (9, 0), 60, "America/New_York", BusyKind::Busy)?,
    ];
    out.sort_by_key(|s| (s.0, s.1));
    Some(out)
}

/// Что должно быть в неделе от VFREEBUSY (`free_busy` в `net.py`): свободный интервал не показан.
fn vfb(monday: Date, tz: &jiff::tz::TimeZone) -> Option<Vec<(i64, i64, BusyKind)>> {
    let span = |hour: i8, kind: BusyKind| at(monday, hour, 0, "UTC").map(|start| (start, start + 3600, kind));
    let mut out = vec![span(5, BusyKind::Busy)?, span(7, BusyKind::Tentative)?, span(15, BusyKind::Unavailable)?];
    for day in 0..7 {
        let start = add_days(monday, day).at(11, 0, 0, 0).to_zoned(tz.clone()).ok()?.timestamp().as_second();
        out.push((start, start + 3600, BusyKind::Busy));
    }
    out.sort_by_key(|s| (s.0, s.1));
    Some(out)
}

fn spans_match(v: &MeetingView, email: &str, want: Option<Vec<(i64, i64, BusyKind)>>) -> bool {
    let mut got: Vec<(i64, i64, BusyKind)> = v.busy_list(email).unwrap_or_default().iter().map(span_of).collect();
    got.sort_by_key(|s| (s.0, s.1));
    let same = want.as_ref() == Some(&got);
    if !same {
        note(format!("{email}: got {got:?}, want {want:?}"));
    }
    same
}

/// Занятость `guest.ics`, подпись «· по ссылке», счётчик данных вырос, окна не пересекают занятость.
fn guest_series(_: &Scene, m: &Mailer, cx: &App) -> bool {
    view(m, cx).is_some_and(|v| {
        v.link_shown(GUEST)
            && spans_match(v, GUEST, series(v.shown_monday()))
            && v.without_data() + 1 == UNKNOWN.get()
            && v.row_subtitles().iter().any(|s| s == &tr!("meeting-by-link", role = tr!("meeting-role-required")))
            && free_avoids(v, GUEST)
    })
}

fn free_avoids(v: &MeetingView, email: &str) -> bool {
    let busy = v.busy_list(email).unwrap_or_default();
    v.free_windows().iter().all(|w| !busy.iter().any(|b| b.span.overlaps(w)))
}

const CHANGE: &[Input] = &[
    Input::Press(GUEST_CHANGE),
    until!(|m, cx| view(m, cx).is_some_and(MeetingView::link_open) && spot_bounds("link-remove").is_some()),
    Input::Press("link-remove"),
];

fn link_removed(_: &Scene, m: &Mailer, cx: &App) -> bool {
    view(m, cx).is_some_and(|v| !v.link_open() && !v.link_shown(GUEST))
        && spot_bounds(GUEST_ROW).is_some()
        && !cal_prefs(cx).free_busy_links.contains_key(GUEST)
}

/// Ввести ссылку в строку адреса и ждать отказ не дольше срока.
macro_rules! bad_link {
    ($url:expr) => {
        &[
            Input::Press(GUEST_ROW),
            Input::Keys(&["ctrl-a", "backspace"]),
            Input::Text($url),
            Input::Run(arm),
            Input::Keys(&["enter"]),
            until!(|m, cx| watch(view(m, cx).is_some_and(|v| v.link_failed(GUEST)))),
        ]
    };
}

const MISSING: &[Input] = bad_link!(LINK_MISSING);
const NO_HOST: &[Input] = bad_link!(LINK_NO_HOST);
const SILENT: &[Input] = bad_link!(LINK_SILENT);
const UNTRUSTED: &[Input] = bad_link!(LINK_UNTRUSTED);
const WRONG_NAME: &[Input] = bad_link!(LINK_WRONG_NAME);

/// Без корня стенда: настоящий источник, ссылка та же.
const NO_ROOT: &[Input] = &[
    OPENED,
    Input::Keys(&["ctrl-shift-b"]),
    Input::Text(GUEST),
    Input::Keys(&["enter"]),
    until!(|m, cx| view(m, cx).is_some_and(|v| v.chips().0 == 1 && v.ready())),
    Input::Press(GUEST_ROW),
    Input::Text(LINK_ICS),
    Input::Run(arm),
    Input::Keys(&["enter"]),
    until!(|m, cx| watch(view(m, cx).is_some_and(|v| v.link_failed(GUEST)))),
];

fn arm(_: &Scene, _: &mut Mailer, _: &mut Window, _: &mut Context<Mailer>) -> Result<(), String> {
    STARTED.set(Some(Instant::now()));
    FRAME.set(None);
    GAP.set(Duration::ZERO);
    TOOK.set(None);
    Ok(())
}

/// Вызывается на каждом кадре ожидания: паузы между кадрами и время ответа.
fn watch(done: bool) -> bool {
    let now = Instant::now();
    if let Some(last) = FRAME.get() {
        GAP.set(GAP.get().max(now - last));
    }
    FRAME.set(Some(now));
    if done && TOOK.get().is_none() {
        TOOK.set(STARTED.get().map(|s| now - s));
    }
    done
}

/// Отказ пришёл в срок, окно не замирало.
fn in_time(_: &Scene, _: &Mailer, _: &App) -> bool {
    let (took, gap) = (TOOK.get(), GAP.get());
    let ok = took.is_some_and(|t| t <= LIMIT) && gap <= STALL;
    if !ok {
        note(format!("took {took:?}, longest frame gap {gap:?}"));
    }
    ok
}

const VFB: &[Input] = &[
    Input::Press(GUEST_ROW),
    Input::Keys(&["ctrl-a", "backspace"]),
    Input::Text(LINK_VFB),
    Input::Keys(&["enter"]),
    until!(|m, cx| view(m, cx).is_some_and(|v| v.link_shown(GUEST))),
];

fn guest_vfb(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let tz = m.cal.read(cx).time_zone().clone();
    view(m, cx).is_some_and(|v| v.link_shown(GUEST) && spans_match(v, GUEST, vfb(v.shown_monday(), &tz)) && free_avoids(v, GUEST))
}

const NEXT_WEEK: &[Input] = &[
    Input::Run(remember_week),
    Input::Press("assistant"),
    Input::Keys(&["pagedown", "pagedown", "pagedown", "pagedown", "pagedown", "pagedown", "pagedown"]),
    until!(|m, cx| view(m, cx).is_some_and(|v| v.shown_monday() != first_week() && v.link_shown(GUEST))),
];

thread_local! {
    static WEEK: Cell<Option<Date>> = const { Cell::new(None) };
}

fn remember_week(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    WEEK.set(view(m, cx).map(MeetingView::shown_monday));
    Ok(())
}

fn first_week() -> Date {
    WEEK.get().unwrap_or_default()
}

fn next_week(s: &Scene, m: &Mailer, cx: &App) -> bool {
    guest_vfb(s, m, cx) && view(m, cx).is_some_and(|v| v.shown_monday() == add_days(first_week(), 7))
}

const CONTACT: &[Input] = &[
    OPENED,
    until!(|m, cx| m.contacts.read(cx).free_busy_url(BOOKED).is_some()),
    Input::Keys(&["ctrl-shift-b"]),
    Input::Text(BOOKED),
    Input::Keys(&["enter"]),
    until!(|m, cx| view(m, cx).is_some_and(|v| v.chips().0 == 1)),
];

fn booked_shown(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let tz = m.cal.read(cx).time_zone().clone();
    view(m, cx).is_some_and(|v| v.link_shown(BOOKED) && spans_match(v, BOOKED, vfb(v.shown_monday(), &tz)))
}

const EDIT_CONTACT: &[Input] = &[
    Input::Keys(&["ctrl-3"]),
    until!(|m, _| m.section == Section::Contacts),
    Input::Run(my_contacts),
    until!(|_, _| screens::spot_first("contact-").is_some()),
    Input::Click(|_, _, _| screens::spot_first("contact-").map(|b| b.center()), screens::Held::No),
    until!(|m, cx| m.contacts.read(cx).selected_email().as_deref() == Some(BOOKED)),
    Input::Keys(&["f2"]),
    until!(|m, cx| m.contacts.read(cx).editing()),
    Input::Keys(&["ctrl-a", "backspace"]),
    Input::Text("Партнёр после правки"), // i18n-ok: ввод сценария
    Input::Keys(&["enter"]),
    until!(|m, cx| !m.contacts.read(cx).editing()),
    Input::Run(meeting),
    OPENED,
    Input::Keys(&["ctrl-shift-b"]),
    Input::Text(BOOKED),
    Input::Keys(&["enter"]),
    until!(|m, cx| view(m, cx).is_some_and(|v| v.chips().0 == 1)),
];

fn my_contacts(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    m.contacts.update(cx, |c, cx| c.set_source(crate::contacts::Source::Mine, cx));
    Ok(())
}

/// Второй запуск: участник тот же, ссылка — из настроек.
const KEPT_LINK: &[Input] = ADD_GUEST;

fn cal_prefs(cx: &App) -> Calendar {
    crate::prefs::get(cx).calendar
}

// --- подписка ---

fn week(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    m.set_section(Section::Calendar, cx);
    m.cal.update(cx, |c, cx| c.set_view(View::Week, cx));
    Ok(())
}

fn subs(m: &Mailer, cx: &App) -> Vec<(String, bool)> {
    m.cal.read(cx).subs_state()
}

fn failed(m: &Mailer, cx: &App) -> bool {
    subs(m, cx).iter().any(|s| s.1)
}

fn fetching(m: &Mailer, cx: &App) -> bool {
    m.cal.read(cx).subs_fetching()
}

/// Названия событий подписок, что видны в неделе.
fn visible(m: &Mailer, cx: &App) -> Vec<String> {
    m.cal.read(cx).visible_sub_titles()
}

fn hidden(cx: &App) -> bool {
    cal_prefs(cx).hidden_calendars.iter().any(|k| k.starts_with("ics:"))
}

const ADD: &[Input] = &[
    until!(|m, cx| m.section == Section::Calendar && m.cal.read(cx).subs_ready()),
    Input::Press("sub-open"),
    Input::Text(SUBSCRIPTION),
    Input::Keys(&["enter"]),
];

const WEBCAL_ADD: &[Input] = &[Input::Press("sub-open"), Input::Text(SECOND_WEBCAL), Input::Keys(&["enter"])];

/// Событие недели от подписки: название, начало, конец, весь день, «под вопросом», занимает ли время.
type Shown = (String, i64, i64, bool, bool, bool);

/// Что должно быть в неделе (`ICS_EVENTS`, `ICS_ALL_DAY` в `net.py`, по событию каждой подписки на весь день).
fn subscribed(c: &CalendarView, monday: Date) -> Option<Vec<Shown>> {
    let timed = |title: &str, day: i64, from: (i8, i8), minutes: i64, zone: &str, tentative: bool, busy: bool| {
        at(add_days(monday, day), from.0, from.1, zone).map(|s| (title.to_owned(), s, s + minutes * 60, false, tentative, busy))
    };
    let (day, next) = (day_start(add_days(monday, 2), c.time_zone()), day_start(add_days(monday, 3), c.time_zone()));
    let whole = ("Весь день".to_owned(), day, next, true, false, false); // i18n-ok: данные источника
    let mut out = vec![
        timed("Пояс Outlook", 0, (12, 0), 60, "Europe/Moscow", false, true)?, // i18n-ok: данные источника
        timed("Пояс Токио", 1, (10, 0), 60, "Asia/Tokyo", false, true)?,      // i18n-ok: данные источника
        timed("Под вопросом", 1, (15, 0), 60, "UTC", true, true)?,            // i18n-ok: данные источника
        timed("Прозрачное", 2, (15, 0), 60, "UTC", false, false)?,            // i18n-ok: данные источника
        timed(PLAIN_EVENT, 3, (9, 0), 90, "UTC", false, true)?,
        timed("Пояс Нью-Йорк", 4, (9, 0), 60, "America/New_York", false, true)?, // i18n-ok: данные источника
        whole.clone(),
        whole,
    ];
    out.sort();
    Some(out)
}

fn week_matches(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let c = m.cal.read(cx);
    let Some(monday) = c.days().first().copied() else { return false };
    let mut got: Vec<Shown> = c
        .sub_events()
        .iter()
        .map(|e| (e.title.clone(), e.span.start, e.span.end, e.all_day, e.status == EventStatus::Tentative, e.free_busy == FreeBusy::Busy))
        .collect();
    got.sort();
    let want = subscribed(c, monday);
    let same = want.as_ref() == Some(&got);
    if !same {
        note(format!("week: got {got:?}, want {want:?}"));
    }
    same && all_day_stacked(c)
}

/// Два события на весь день в один день видны оба: друг под другом, а не одно поверх другого.
fn all_day_stacked(c: &CalendarView) -> bool {
    let tops: Vec<Pixels> = c.sub_events_all_day().into_iter().filter_map(|i| spot_bounds(&format!("all-day-{i}"))).map(|b| b.origin.y).collect();
    let stacked = tops.len() == 2 && tops[0] != tops[1];
    if !stacked {
        note(format!("all day: {tops:?}"));
    }
    stacked
}

const ENDLESS: &[Input] = &[
    until!(|m, cx| m.section == Section::Calendar && m.cal.read(cx).subs_ready()),
    Input::Press("sub-open"),
    Input::Text(ENDLESS_ICS),
    Input::Keys(&["enter"]),
];

const UTC: &[Input] = &[
    until!(|m, cx| m.section == Section::Calendar && m.cal.read(cx).subs_ready()),
    Input::Press("sub-open"),
    Input::Text(UTC_ICS),
    Input::Keys(&["enter"]),
];

/// «Время UTC» — в четверг в 09:00 UTC.
fn utc_series(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let c = m.cal.read(cx);
    let Some(monday) = c.days().first().copied() else { return false };
    let got: Vec<i64> = c.sub_events().iter().filter(|e| e.title == PLAIN_EVENT).map(|e| e.span.start).collect();
    let want = at(add_days(monday, 3), 9, 0, "UTC");
    if !got.is_empty() {
        note(format!("utc series: got {got:?}, want {want:?}"));
    }
    want.is_some_and(|w| got == [w])
}

/// Все серии файла в неделе, не только первая.
fn all_series(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let got = visible(m, cx);
    if !got.is_empty() {
        note(format!("endless: {got:?}"));
    }
    got.len() == 6
}

fn event_spot(m: &Mailer, cx: &App) -> Option<String> {
    m.cal.read(cx).event_index(PLAIN_EVENT).map(|i| format!("event-{i}"))
}

const OPEN_EVENT: &[Input] = &[
    until!(|m, cx| event_spot(m, cx).is_some_and(|id| spot_bounds(&id).is_some())),
    Input::Click(|m, window, cx| event_spot(m, cx).and_then(|id| screens::spot_center(&id, window)), screens::Held::No),
];

/// Подпись «Подписка», кнопок ответа и правки нет.
fn event_read_only(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.cal.read(cx).selected_chip().as_deref() == Some(tr!("cal-chip-subscription").as_str())
        && ["own-edit", "own-cancel", "accept", "tentative", "decline"].iter().all(|id| spot_bounds(id).is_none())
}

const HIDE: &[Input] = &[Input::Press("details-close"), Input::Press("cal-sub-check-0"), Input::Press("cal-sub-check-1")];

const COLOR: &[Input] = &[
    until!(|_, _| spot_bounds("cal-sub-0").is_some()),
    Input::Press("cal-sub-check-0"),
    Input::Press("cal-sub-check-1"),
    Input::Press("cal-sub-0"),
    Input::Press("cal-color-3"),
];

fn colored(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let color = postman_core::palette::CALENDAR.get(3).copied().unwrap_or_default();
    !hidden(cx) && !visible(m, cx).is_empty() && cal_prefs(cx).calendar_colors.iter().any(|(k, v)| k.starts_with("ics:") && v.as_str() == color)
}

const REFRESH: &[Input] =
    &[until!(|_, _| spot_bounds("cal-sub-0").is_some()), Input::Run(open_first_menu), Input::Run(arm), Input::Press("cal-sub-refresh-0"), CHECKED];

/// Обновление, начатое после `arm`, закончилось.
const CHECKED: Input = until!(|m, cx| STARTED.get().is_some_and(|t| m.cal.read(cx).subs_checked_since(t)));

/// Меню образца первой подписки открыто.
fn open_first_menu(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    m.cal.update(cx, |c, cx| c.open_sub_menu(0, cx));
    Ok(())
}

const REFRESH_MENU_OPEN: &[Input] = &[Input::Run(arm), Input::Press("cal-sub-refresh-0"), CHECKED];

fn calendar_settings(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    m.close_settings(window, cx);
    m.open_settings(Some(S::Calendar), window, cx);
    Ok(())
}

/// Строка настроек — в окне: она могла остаться ниже края.
fn reveal(id: &str, y: f32, m: &mut Mailer, cx: &mut Context<Mailer>) -> Result<(), String> {
    let spot = spot_bounds(id).ok_or_else(|| format!("no element {id}"))?;
    STEADY.set((None, 0));
    if let Some(v) = m.settings.view.clone() {
        v.update(cx, |v, cx| {
            let top = v.scroll.offset().y - (spot.origin.y - px(y));
            v.scroll.set_offset(point(px(0.), top));
            cx.notify();
        });
    }
    Ok(())
}

/// Элемент стоит на месте три кадра подряд: точка щелчка верна (отметки — с прошлого кадра, прокрутка меняет их на следующем).
fn settled(id: &str) -> bool {
    let y = spot_bounds(id).map(|b| b.origin.y);
    let (last, frames) = STEADY.get();
    let frames = if y.is_some() && y == last { frames + 1 } else { 0 };
    STEADY.set((y, frames));
    frames >= 3
}

/// В цветах календарей настроек — имена подписок, а не ключи.
fn colors_named(cx: &App) -> bool {
    let cal = cal_prefs(cx);
    let names: Vec<Option<String>> =
        cal.calendar_colors.keys().filter(|k| k.starts_with("ics:")).map(|k| crate::calendar::name_of_key(&cal, k)).collect();
    names.len() == 2 && [FIRST_NAME, SECOND_NAME].iter().all(|n| names.iter().any(|g| g.as_deref() == Some(n)))
}

const TIMER: &[Input] = &[
    until!(|m, cx| settings_on(m, S::Calendar, cx)),
    until!(|_, _| spot_bounds("settings-cal-subs-60").is_some()),
    Input::Run(|_, m, _, cx| reveal("settings-cal-subs-60", 320., m, cx)),
    until!(|_, _| settled("settings-cal-subs-60")),
    Input::Press("settings-cal-subs-60"),
    until!(|_, cx| cal_prefs(cx).subscription_minutes == 60),
    Input::Press("settings-cal-subs-15"),
    until!(|_, cx| cal_prefs(cx).subscription_minutes == 15),
    Input::Run(arm),
    Input::Run(expire),
    CHECKED,
];

/// Срок вышел: как будто с прошлой проверки прошло 15 минут; дальше — тот же шаг таймера, что и в работе.
fn expire(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    m.cal.update(cx, |c, cx| c.expire_subs(Duration::from_secs(15 * 60), cx));
    Ok(())
}

const BUSY_FREE: &[Input] = &[OPENED, until!(|m, cx| m.cal.read(cx).has_subscription_events())];

/// Подписка в неделе есть, а моя занятость пуста: и в помощнике, и в «Время в календаре» (S6-08).
fn my_time_free(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let free = view(m, cx).is_some_and(|v| v.my_busy().is_some_and(<[Busy]>::is_empty));
    let tasks = m.cal.read(cx).busy_today();
    if !tasks.is_empty() {
        note(format!("busy_today: {tasks:?}"));
    }
    free && tasks.is_empty()
}

const KEPT: &[Input] = &[until!(|m, cx| m.cal.read(cx).subs_ready() && !fetching(m, cx))];

/// После перезапуска: обе подписки, цвет, прежние события и «Не обновился».
fn kept_offline(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let color = postman_core::palette::CALENDAR.get(3).copied().unwrap_or_default();
    let list = subs(m, cx);
    let names = list.iter().map(|s| s.0.as_str()).collect::<Vec<_>>();
    if names != [FIRST_NAME, SECOND_NAME] {
        note(format!("subscriptions {list:?}"));
    }
    names == [FIRST_NAME, SECOND_NAME]
        && list.iter().all(|s| s.1)
        && visible(m, cx).contains(&PLAIN_EVENT.to_owned())
        && cal_prefs(cx).calendar_colors.iter().any(|(k, v)| k.starts_with("ics:") && v.as_str() == color)
}

const REMOVE: &[Input] = &[
    until!(|m, cx| m.cal.read(cx).subs_ready() && spot_bounds("cal-sub-0").is_some()),
    Input::Press("cal-sub-0"),
    Input::Press("cal-sub-remove-0"),
    until!(|m, cx| subs(m, cx).len() == 1),
    Input::Press("cal-sub-0"),
    Input::Press("cal-sub-remove-0"),
];

// --- каталог ---

/// Настройки каталога до показа раздела: пустой сервер и вход Windows обратились бы к настоящему контроллеру домена.
fn directory_prefs(cx: &mut App, server: &str, login: DirectoryLogin, starttls: bool) {
    let server = server.to_owned();
    crate::prefs::update(cx, move |p: &mut Prefs| {
        let d = &mut p.security.directory;
        d.server = server;
        d.base_dn = String::new();
        d.user = String::new();
        d.login = login;
        d.starttls = starttls;
    });
}

fn security(
    server: &str,
    login: DirectoryLogin,
    starttls: bool,
    m: &mut Mailer,
    window: &mut Window,
    cx: &mut Context<Mailer>,
) -> Result<(), String> {
    directory_prefs(cx, server, login, starttls);
    m.close_settings(window, cx);
    m.open_settings(Some(S::Security), window, cx);
    Ok(())
}

fn password_login(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    security(CLOSED, DirectoryLogin::Password, false, m, window, cx)
}

fn password_plain(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    security(CLOSED, DirectoryLogin::Password, false, m, window, cx)
}

fn anonymous(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    security(PLAIN, DirectoryLogin::Anonymous, false, m, window, cx)
}

fn anonymous_ldaps(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    security(LDAPS, DirectoryLogin::Anonymous, false, m, window, cx)
}

fn settings_view<'a>(m: &Mailer, cx: &'a App) -> Option<&'a SettingsView> {
    m.settings.view.as_ref().map(|v| v.read(cx))
}

fn check_is(m: &Mailer, cx: &App, text: &str) -> bool {
    let got = settings_view(m, cx).and_then(SettingsView::directory_check);
    if got.is_some_and(|g| g != text) {
        note(format!("directory check: {got:?}, want {text}"));
    }
    got == Some(text)
}

fn check_ok(m: &Mailer, cx: &App, n: usize) -> bool {
    check_is(m, cx, &tr!("settings-directory-ok", n = n))
}

fn password() -> String {
    std::env::var("POSTMAN_NET_LDAP_READER").unwrap_or_default()
}

/// Запись `Postman:ldap:<сервер>` есть, пароль в ней тот, что ввели.
fn password_kept(server: &str) -> bool {
    let target = crate::directory::credential_target(server);
    crate::sys::credentials::read(&target).ok().flatten().is_some_and(|p| p == password())
}

/// Записи стенда в Диспетчере учётных данных: снять после проверки.
fn forget_passwords(_: &Scene, _: &mut Mailer, _: &mut Window, _: &mut Context<Mailer>) -> Result<(), String> {
    for server in [LDAPS, PLAIN] {
        crate::sys::credentials::delete(&crate::directory::credential_target(server))?;
    }
    Ok(())
}

/// Каталог — последняя группа раздела: «Проверить» — над строкой состояния, положение строк устоялось.
const SHOWN: Input = Input::Run(|_, m, _, cx| reveal("settings-directory-check", 600., m, cx));
const STILL: Input =
    until!(|_, _| settled("settings-directory-check") && spot_bounds("settings-directory-check").is_some_and(|b| b.origin.y < px(900.)));
const SECURITY: Input = until!(|m, cx| settings_on(m, S::Security, cx) && spot_bounds("settings-directory-check").is_some());

const LDAPS_CHECK: &[Input] = &[
    SECURITY,
    SHOWN,
    STILL,
    Input::Press("directory-server"),
    Input::Keys(&["ctrl-a", "backspace"]),
    Input::Text(LDAPS),
    Input::Press("directory-user"),
    Input::Text(READER),
    Input::Press("directory-password"),
    Input::Typed(password),
    Input::Keys(&["enter"]),
    until!(|m, cx| settings_view(m, cx).is_some_and(|v| v.directory_password_saved(cx))),
    SHOWN,
    STILL,
    Input::Press("settings-directory-check"),
    until!(|m, cx| settings_view(m, cx).is_some_and(|v| v.directory_check().is_some())),
];

const PLAIN_CHECK: &[Input] = &[
    SECURITY,
    SHOWN,
    STILL,
    Input::Press("directory-server"),
    Input::Keys(&["ctrl-a", "backspace"]),
    Input::Text(PLAIN),
    Input::Press("directory-user"),
    Input::Text(READER),
    Input::Press("directory-password"),
    Input::Typed(password),
    Input::Keys(&["enter"]),
    until!(|m, cx| settings_view(m, cx).is_some_and(|v| v.directory_password_saved(cx))),
    SHOWN,
    STILL,
    Input::Press("settings-directory-check"),
];

const STARTTLS_CHECK: &[Input] = &[
    Input::Press("settings-directory-starttls"),
    until!(|_, cx| crate::prefs::get(cx).security.directory.starttls),
    until!(|m, cx| settings_view(m, cx).is_some_and(|v| v.directory_check().is_none())),
    SHOWN,
    STILL,
    Input::Press("settings-directory-check"),
    until!(|m, cx| settings_view(m, cx).is_some_and(|v| v.directory_check().is_some())),
];

/// Пароли стенда проверены: записи Диспетчера больше не нужны.
const ANON: &[Input] = &[Input::Run(forget_passwords), SECURITY, SHOWN, STILL, Input::Press("settings-directory-check")];

const CHAIN: &[Input] = &[
    SECURITY,
    SHOWN,
    STILL,
    SHOWN,
    STILL,
    Input::Press("settings-directory-check"),
    until!(|m, cx| check_is(m, cx, &tr!("directory-unavailable"))),
    Input::Press("directory-server"),
    Input::Keys(&["ctrl-a", "backspace"]),
    Input::Text(PLAIN),
    Input::Press("settings-directory-starttls"),
    until!(|_, cx| crate::prefs::get(cx).security.directory.starttls),
    SHOWN,
    STILL,
    Input::Press("settings-directory-check"),
    until!(|m, cx| check_is(m, cx, &tr!("directory-unavailable"))),
    Input::Press("settings-directory-starttls"),
    until!(|_, cx| !crate::prefs::get(cx).security.directory.starttls),
    SHOWN,
    STILL,
    Input::Press("settings-directory-check"),
];

fn letter(_: &Scene, _: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    directory_prefs(cx, PLAIN, DirectoryLogin::Anonymous, false);
    Ok(())
}

fn server_down(_: &Scene, _: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    directory_prefs(cx, CLOSED, DirectoryLogin::Anonymous, false);
    Ok(())
}

fn server_silent(_: &Scene, _: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    directory_prefs(cx, QUIET, DirectoryLogin::Anonymous, false);
    Ok(())
}

fn no_certificate(m: &Mailer, cx: &App) -> Vec<String> {
    compose_tab(m).map(|c| c.read(cx).no_certificate()).unwrap_or_default()
}

fn locked(m: &Mailer, cx: &App, address: &str) -> bool {
    compose_tab(m).is_some_and(|c| c.read(cx).locked(address))
}

fn directory(m: &Mailer) -> Option<DirectoryStatus> {
    crate::smime_seal::lock(&m.compose.certs).directory.clone()
}

/// Каталог ответил, сертификата у адресата нет: замечание осталось, замка нет.
fn found_none(m: &Mailer, cx: &App, address: &str) -> bool {
    directory(m) == Some(DirectoryStatus::Done { found: 0 }) && no_certificate(m, cx).iter().any(|a| a == address) && !locked(m, cx, address)
}

/// Письмо «Конфиденциально» адресату и поиск в каталоге.
macro_rules! confidential {
    ($address:expr) => {
        &[
            until!(|m, _| m.live.synced),
            Input::Press("bar-new"),
            until!(|m, _| compose_tab(m).is_some()),
            Input::Text($address),
            Input::Text(";"),
            Input::Press("tb-mark"),
            Input::Press("mark-conf"),
            until!(|m, cx| no_certificate(m, cx).iter().any(|a| a == $address)),
            Input::Run(arm),
            Input::Press("check-directory-0"),
            until!(|m, _| watch(
                !matches!(directory(m), Some(DirectoryStatus::Searching)) && STARTED.get().is_some_and(|s| s.elapsed() > Duration::from_millis(50))
            )),
        ]
    };
}

const FIND: &[Input] = confidential!(BOB);
const NONE: &[Input] = confidential!(DAVE);
const FOREIGN: &[Input] = confidential!(CAROL);
const DOWN: &[Input] = confidential!(ERIN);
const HANGS: &[Input] = confidential!(FRANK);

fn directory_failed(m: &Mailer, cx: &App, key: &str) -> bool {
    let same = directory(m) == Some(DirectoryStatus::Failed(tr!(key)));
    if !same {
        note(format!("directory: {:?}", directory(m)));
    }
    same && in_time(&SCENES[0], m, cx)
}
