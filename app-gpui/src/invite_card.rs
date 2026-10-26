//! Карточка приглашения на встречу над текстом письма (вложение `text/calendar`; вид — S5-11 по макету
//! «Приглашение в письме»). `.ics` разбирает воркер-песочница; здесь — готовое [`Invitation`], мой ответ и мой день.
//!
//! Состояние — `postman_core::invite::classify`: новое (кнопки «Принять / Под вопросом / Отклонить», клавиши
//! A / T / N), отвечено («Вы приняли · Изменить ответ»), изменено организатором (что было — зачёркнуто), отменено,
//! устарело (есть новее, встреча прошла). Раскладка — Г1 (S5-24): сверху детали, под ними лента дня на всю ширину
//! (мои события серые, приглашение синее, пересечение фиолетовое — [`day_blocks`], `invite::conflicts`), ниже кнопки
//! в строку. Опция «Настройки → Чтение → Боковая панель справа» — над письмом строка, подробности, кнопки столбиком и
//! лента дня 8–19 — колонкой 300 px ([`render_side`]). Ответ — путь S3-03 (`Command::RespondInvite`); мой день —
//! `Command::Events` на сутки встречи.

use gpui_kit::prelude::*;
use gpui_kit::*;
use jiff::tz::TimeZone;
use pets_i18n::tr;
use postman_calendar::{Event, Invitation, Partstat, Span, layout_day};
use postman_core::invite::{Change, InviteState, Known, Outdated, Seen, classify, conflicts};
use postman_core::sync::Event as SyncEvent;

use crate::calendar::{FIRST_HOUR, HOURS, add_days, day_start, duration_text, g, hm, zoned};
use crate::chrome::icon;
use crate::reading::{ReadingEvent, ReadingPane, SyncRequest};
use crate::roles::{Look, Pm};

/// Лента дня: часов в окне карточки, высота часа, ширина боковой панели (макет Г1 и вариант с панелью справа).
const CARD_HOURS: i64 = 6;
const ROW_PX: f32 = 32.;
const SIDE_W: f32 = 300.;

pub(crate) enum State {
    /// Ждём байты вложения или ответ воркера.
    Loading {
        blob_id: String,
    },
    Ready(Box<Invitation>),
    Failed(String),
}

pub(crate) struct Card {
    pub(crate) state: State,
    /// Мой ответ, принятый сервером (или запомненный в хранилище), и версия встречи, на которую он дан.
    pub(crate) answer: Option<Seen>,
    /// Ответ ушёл, итога ещё нет: кнопки неактивны.
    pub(crate) sending: Option<Partstat>,
    /// Мои события в сутки встречи (пересечения, «Ваш день», копия встречи в календаре); `None` — ещё нет.
    pub(crate) day: Option<Vec<Event>>,
    /// Ждём их с сервера: `[from, to)`.
    pub(crate) day_pending: Option<(i64, i64)>,
    /// «Изменить ответ»: кнопки ответа снова видны.
    pub(crate) changing: bool,
    /// Снимки: «сейчас» — здесь, а не по часам.
    pub(crate) clock: Option<i64>,
}

impl Card {
    pub(crate) fn loading(blob_id: String) -> Self {
        Self { state: State::Loading { blob_id }, answer: None, sending: None, day: None, day_pending: None, changing: false, clock: None }
    }

    pub(crate) fn invitation(&self) -> Option<&Invitation> {
        match &self.state {
            State::Ready(inv) => Some(inv),
            _ => None,
        }
    }

    pub(crate) fn uid(&self) -> Option<&str> {
        self.invitation().map(|i| i.event.uid.as_str())
    }

    pub(crate) fn waits_for(&self, blob: &str) -> bool {
        matches!(&self.state, State::Loading { blob_id } if blob_id == blob)
    }

    /// Ответ принят: запомнить с версией встречи; строка для хранилища.
    pub(crate) fn remember(&mut self, status: Partstat) -> String {
        let seen = match self.invitation() {
            Some(inv) => Seen::of(inv, status),
            None => Seen { status, version: None },
        };
        let value = seen.encode();
        self.answer = Some(seen);
        self.sending = None;
        self.changing = false;
        value
    }

    fn now(&self) -> i64 {
        self.clock.unwrap_or_else(|| jiff::Timestamp::now().as_second())
    }

    /// Копия встречи в моём календаре (тот же UID) — из событий дня.
    fn server_copy(&self) -> Option<&Event> {
        let uid = self.uid()?;
        self.day.as_ref()?.iter().find(|e| e.uid == uid)
    }

    /// Состояние для меня (`me` — мой адрес).
    pub(crate) fn state_for(&self, me: Option<&str>) -> Option<InviteState> {
        let inv = self.invitation()?;
        let known = Known { seen: self.answer.as_ref(), server: self.server_copy(), now: self.now() };
        Some(classify(inv, me, &known))
    }

    /// Можно ответить (кнопки видны): новое, изменённое, «Изменить ответ».
    pub(crate) fn can_answer(&self, me: Option<&str>) -> bool {
        match self.state_for(me) {
            Some(InviteState::New | InviteState::Updated(_)) => true,
            Some(InviteState::Answered(_)) => self.changing,
            _ => false,
        }
    }

    /// Снимки: состояние карточки — подменой входов классификации (ответ, версия, метод, «сейчас»).
    pub(crate) fn screen_state(&mut self, which: &str) {
        // Прошлая сцена могла сделать письмо отменой — вернуть запрос.
        if let State::Ready(inv) = &mut self.state
            && inv.method == "CANCEL"
        {
            inv.method = "REQUEST".into();
            inv.sequence -= 1;
        }
        let Some(inv) = self.invitation().cloned() else { return };
        self.clock = Some(inv.event.span.start - 86_400);
        match which {
            "answered" => self.answer = Some(Seen::of(&inv, Partstat::Accepted)),
            "updated" => {
                let mut seen = Seen::of(&inv, Partstat::Accepted);
                if let Some(v) = seen.version.as_mut() {
                    v.sequence -= 1;
                    v.span = Span::new(v.span.start - 3600, v.span.end - 3600);
                    v.location = tr!("invite-sample-old-place");
                }
                self.answer = Some(seen);
            }
            "cancelled" => {
                if let State::Ready(inv) = &mut self.state {
                    inv.method = "CANCEL".into();
                    inv.sequence += 1;
                }
                self.answer = None;
            }
            "outdated" => {
                self.answer = None;
                self.clock = Some(inv.event.span.end + 3600);
            }
            _ => self.answer = None,
        }
        self.changing = false;
    }
}

/// Ключ хранилища для запомненного ответа.
pub(crate) fn state_key(uid: &str) -> String {
    format!("invite:{uid}")
}

/// Ответ без версии (запись S3-03): одно слово статуса.
pub(crate) fn encode(s: Partstat) -> String {
    Seen { status: s, version: None }.encode()
}

/// Запомненный ответ: JSON с версией или прежнее слово статуса.
pub(crate) fn decode(s: &str) -> Option<Seen> {
    Seen::decode(s)
}

/// Участники, кроме организатора: (имя или адрес, ответ), по алфавиту — порядок в приглашении случаен.
pub(crate) fn attendees(inv: &Invitation) -> Vec<(String, Partstat)> {
    let org = inv.organizer_email();
    let mut list: Vec<(String, Partstat)> = inv
        .event
        .participants
        .iter()
        .filter(|p| p.attendee && Some(p.email.as_str()) != org)
        .map(|p| (if p.name.is_empty() { p.email.clone() } else { p.name.clone() }, p.status))
        .collect();
    list.sort_by_key(|(name, _)| name.to_lowercase());
    list
}

/// «Чт, 23 октября · 11:00–12:00».
fn when_line(span: Span, all_day: bool, tz: &TimeZone) -> String {
    let short = day_line(span, tz);
    if all_day { format!("{short} · {}", tr!("cal-all-day")) } else { format!("{short} · {}", time_range(span, tz)) }
}

/// «Чт, 23 октября».
fn day_line(span: Span, tz: &TimeZone) -> String {
    let Some(a) = zoned(span.start, tz) else { return String::new() };
    tr!("invite-day", wd = tr!("cal-day", d = a.weekday().to_monday_zero_offset()), day = a.day(), month = tr!("cal-month-of", m = a.month()))
}

/// «11:00–12:00».
fn time_range(span: Span, tz: &TimeZone) -> String {
    format!("{}–{}", hm(span.start, tz), hm(span.end, tz))
}

/// Листок календаря: день недели и число.
fn date_tile(span: Span, dim: bool, small: bool, tz: &TimeZone, l: &Look) -> Div {
    let z = zoned(span.start, tz);
    let wd = z.as_ref().map(|z| tr!("cal-day", d = z.weekday().to_monday_zero_offset()).to_uppercase()).unwrap_or_default();
    let day = z.as_ref().map(|z| z.day().to_string()).unwrap_or_default();
    let (w, h, head, wd_size) = if small { (30., 34., 11., 9.) } else { (40., 44., 16., 11.) };
    div()
        .w(px(w))
        .h(px(h))
        .flex_none()
        .flex()
        .flex_col()
        .rounded(px(4.))
        .overflow_hidden()
        .border_1()
        .border_color(l.border())
        .bg(l.panel())
        .child(
            div()
                .h(px(head))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(wd_size))
                .font_weight(FontWeight::SEMIBOLD)
                .bg(if dim { l.text3() } else { l.accent() })
                .text_color(l.on_accent())
                .child(wd),
        )
        .child(
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(13.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(if dim { l.text3() } else { l.text() })
                .child(day),
        )
}

// --- лента дня (модель) ---

/// Вид блока в ленте дня: свои события — серые, приглашение — синее, своё, пересекающееся с приглашением, —
/// фиолетовое (DESIGN «Приглашение в письме»).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lane {
    Own,
    Invite,
    Conflict,
}

/// Блок ленты: время, подпись, вид и место по колонкам (`calendar::layout_day`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Block {
    pub(crate) span: Span,
    pub(crate) title: String,
    pub(crate) lane: Lane,
    pub(crate) column: usize,
    pub(crate) span_columns: usize,
    pub(crate) columns: usize,
}

/// Окно ленты, часы `[первый, первый + n)`: в карточке — шесть часов от «за два часа до встречи», в боковой панели —
/// день 8–19; встреча целиком в окне.
pub(crate) fn day_window(span: Span, tz: &TimeZone, side: bool) -> (i64, i64) {
    let Some(z) = zoned(span.start, tz) else { return (FIRST_HOUR, HOURS) };
    let start = i64::from(z.hour());
    let len_h = (span.len() + 3599) / 3600;
    let end = (start + len_h.max(1)).min(24);
    let (first, last) = if side {
        (FIRST_HOUR.min(start), (FIRST_HOUR + HOURS).max(end))
    } else {
        let first = (start - 2).clamp(0, 24 - CARD_HOURS);
        (first, (first + CARD_HOURS).max(end))
    };
    (first, (last.min(24) - first).max(1))
}

/// Блоки ленты в окне `window`: мои события (без отклонённых, событий «весь день» и копии самой встречи) и встреча;
/// мои, что пересекаются со встречей (`invite::conflicts`), — [`Lane::Conflict`].
pub(crate) fn day_blocks(e: &Event, day: &[Event], me: Option<&str>, window: Span) -> Vec<Block> {
    let hits = conflicts(e.span, &e.uid, day, me);
    let mine: Vec<&Event> = day
        .iter()
        .filter(|x| x.uid != e.uid && !x.all_day && x.span.overlaps(&window))
        .filter(|x| crate::calendar::event_look(x, me) != crate::calendar::Shown::Hidden)
        .collect();
    let mut spans: Vec<Span> = mine.iter().map(|x| x.span).collect();
    spans.push(e.span);
    layout_day(&spans)
        .into_iter()
        .map(|p| {
            let (span, title, lane) = match mine.get(p.index) {
                Some(x) => {
                    let hit = hits.iter().any(|h| h.uid == x.uid && h.span == x.span);
                    (x.span, x.title.clone(), if hit { Lane::Conflict } else { Lane::Own })
                }
                None => (e.span, e.title.clone(), Lane::Invite),
            };
            Block { span, title, lane, column: p.column, span_columns: p.span_columns, columns: p.columns }
        })
        .collect()
}

// --- отрисовка ---

/// Боковая панель вместо карточки (S5-24, «Настройки → Чтение»): только у приглашения с ответом или кнопками;
/// отмена и устаревшее — строкой над письмом, как без опции.
pub(crate) fn side_wanted(card: &Card, me: Option<&str>, cx: &App) -> bool {
    crate::prefs::get(cx).reading.invite_side
        && matches!(card.state_for(me), Some(InviteState::New | InviteState::Updated(_) | InviteState::Answered(_) | InviteState::NotInvited))
}

/// Подписи и цвета карточки для одного приглашения: общее у карточки и боковой панели.
struct Parts {
    state: InviteState,
    organizer: String,
    accepted: usize,
    total: usize,
    names: String,
    changes: Vec<Change>,
    answered: Option<Partstat>,
}

impl Parts {
    fn of(card: &Card, inv: &Invitation, me: Option<&str>) -> Self {
        let e = &inv.event;
        let state = card.state_for(me).unwrap_or(InviteState::Info);
        let organizer = e.organizer().map(|o| if o.name.is_empty() { o.email.clone() } else { o.name.clone() }).unwrap_or_default();
        let people = attendees(inv);
        let accepted = people.iter().filter(|(_, s)| *s == Partstat::Accepted).count() + 1;
        let names = people
            .iter()
            .map(|(n, s)| format!("{n} — {}", tr!("reading-partstat", s = postman_calendar::partstat_json(*s))))
            .collect::<Vec<_>>()
            .join(", ");
        let changes = match &state {
            InviteState::Updated(c) => c.clone(),
            _ => Vec::new(),
        };
        let answered = match &state {
            InviteState::Answered(s) if !card.changing => Some(*s),
            _ => None,
        };
        Parts { total: people.len() + 1, state, organizer, accepted, names, changes, answered }
    }
}

/// Ответы участников точками: организатор и участники, до 12.
fn dots(inv: &Invitation, l: &Look) -> Div {
    let people = attendees(inv);
    div().flex().gap(px(4.)).children(std::iter::once(Partstat::Accepted).chain(people.iter().map(|(_, s)| *s)).take(12).map(|s| {
        let d = div().size(px(6.)).rounded_full();
        match s {
            Partstat::Accepted => d.bg(l.success()),
            Partstat::Tentative => d.bg(l.text3()),
            Partstat::Declined => d.bg(l.error().opacity(0.6)),
            _ => d.border_1().border_color(l.text3()),
        }
    }))
}

/// Метка состояния справа от темы: «Ждёт ответа», «Принято», «Под вопросом», «Отклонено».
fn chip(answered: Option<Partstat>, l: &Look) -> Div {
    let (key, bg, fg) = match answered {
        None => ("invite-chip-waiting", l.pm(Pm::Secondary), l.pm(Pm::SecondaryFg)),
        Some(Partstat::Accepted) => ("invite-chip-accepted", l.pm(Pm::OkBg), l.pm(Pm::OkFg)),
        Some(Partstat::Declined) => ("invite-chip-declined", l.fill(), l.text2()),
        Some(_) => ("invite-chip-tentative", l.fill(), l.text2()),
    };
    div().flex_none().px(px(8.)).rounded(px(4.)).text_size(px(11.)).line_height(px(16.)).bg(bg).text_color(fg).child(tr!(key))
}

/// Длительность меткой: «1 ч».
fn length_chip(secs: i64, l: &Look) -> Div {
    div().flex_none().px(px(8.)).rounded(px(4.)).text_size(px(11.)).line_height(px(16.)).bg(l.fill()).text_color(l.text2()).child(duration_text(secs))
}

/// Кнопки ответа: «Принять» — заливка, «Под вопросом» и «Отклонить» — тонированные, у всех — значок в круге; клавиша
/// (A / T / N в схеме «Одиночные клавиши») — подсказкой «Название (клавиша)» и KeyTips, не в тексте кнопки (S5-18).
fn answer_buttons(card: &Card, side: bool, l: &Look, cx: &mut Context<ReadingPane>) -> Vec<Stateful<Div>> {
    let sending = card.sending;
    [
        ("invite-accept", "cal-accept", "invite.accept", Partstat::Accepted),
        ("invite-tentative", "cal-tentative-answer", "invite.tentative", Partstat::Tentative),
        ("invite-decline", "cal-decline", "invite.decline", Partstat::Declined),
    ]
    .into_iter()
    .map(|(id, key, command, s)| {
        let primary = s == Partstat::Accepted;
        let b = if primary {
            crate::buttons::primary(id, l).justify_center().font_weight(FontWeight::SEMIBOLD)
        } else {
            crate::buttons::secondary(id, l)
        };
        let ink = crate::buttons::ink(primary, l);
        let b = b
            .when(!side, |d| d.w(px(160.)).min_w(px(88.)).px(px(8.)))
            .when(sending.is_some(), |d| d.opacity(if sending == Some(s) { 1. } else { 0.5 }))
            .child(status_mark(s, ink))
            .child(tr!(key))
            .on_click(cx.listener(move |this, _, _, cx| {
                if sending.is_none() {
                    this.answer_invite(s, cx);
                }
            }));
        crate::keytips::command(b, command, Some(tr!(key)), l, cx)
    })
    .collect()
}

/// Значок ответа в круге (DESIGN «Цвет», кнопки статуса): ✓ принять, ? под вопросом, ✕ отклонить.
pub(crate) fn status_mark(s: Partstat, ink: Hsla) -> Div {
    let mark = div().size(px(16.)).flex_none().flex().items_center().justify_center().rounded_full().border_1().border_color(ink).text_color(ink);
    match s {
        Partstat::Accepted => mark.child(icon(g::CHECK, 9.)),
        Partstat::Declined => mark.child(icon(g::CANCEL, 8.)),
        _ => mark.text_size(px(11.)).line_height(px(14.)).font_weight(FontWeight::BOLD).child("?"),
    }
}

/// «Вы приняли» и «Изменить ответ».
fn done_text(s: Partstat, l: &Look) -> Div {
    let (text, color) = match s {
        Partstat::Accepted => (tr!("invite-done-accepted"), l.pm(Pm::OkFg)),
        Partstat::Tentative => (tr!("invite-done-tentative"), l.text2()),
        Partstat::Declined => (tr!("invite-done-declined"), l.text2()),
        _ => (tr!("reading-invite-mine", status = tr!("reading-partstat", s = postman_calendar::partstat_json(s))), l.text2()),
    };
    div().flex().items_center().gap(px(8.)).font_weight(FontWeight::SEMIBOLD).text_color(color).child(icon(g::CHECK, 12.)).child(text)
}

fn change_answer(cx: &mut Context<ReadingPane>) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    cx.listener(|this, _, _, cx| {
        if let Some(card) = this.reading.as_mut().and_then(|r| r.invite.as_mut()) {
            card.changing = true;
        }
        cx.notify();
    })
}

/// Карточка; кнопки ответа зовут [`ReadingPane::answer_invite`]. В режиме боковой панели — строка над письмом, сама
/// панель — [`render_side`].
pub(crate) fn render(card: &Card, me: Option<&str>, cx: &mut Context<ReadingPane>) -> AnyElement {
    let l = crate::roles::look(cx);
    let tz = crate::prefs::get(cx).calendar.zone();
    let frame = || div().rounded(px(8.)).border_1().border_color(l.c(pets_ui::theme::Role::StrokeCard)).text_size(px(13.));
    let inv = match &card.state {
        State::Loading { .. } => {
            return frame().p_3().text_color(l.text2()).child(tr!("reading-invite-loading")).into_any_element();
        }
        State::Failed(e) => {
            return frame().p_3().child(tr!("reading-invite-failed-parse", error = e.as_str())).into_any_element();
        }
        State::Ready(inv) => inv,
    };
    let e = &inv.event;
    let p = Parts::of(card, inv, me);
    // Компактная строка: отмена, устаревшее, прочие методы (макет «Приглашение · состояния»).
    let compact = |reason: String, glyph: char, dim: bool, strike: bool| {
        frame()
            .rounded(px(6.))
            .h(px(44.))
            .flex()
            .items_center()
            .gap(px(10.))
            .pl(px(6.))
            .pr(px(10.))
            .child(date_tile(e.span, dim, true, &tz, &l))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .line_height(px(17.))
                    .child(div().truncate().when(dim, |d| d.text_color(l.text2())).when(strike, |d| d.line_through()).child(format!(
                        "{} · {}",
                        e.title,
                        when_line(e.span, e.all_day, &tz)
                    )))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(5.))
                            .text_size(px(12.))
                            .text_color(l.text2())
                            .child(icon(glyph, 12.))
                            .child(div().truncate().child(reason)),
                    ),
            )
            .into_any_element()
    };
    match &p.state {
        InviteState::Cancelled => return compact(tr!("invite-cancelled", organizer = p.organizer.clone()), g::CANCEL, true, true),
        InviteState::Outdated(why) => {
            let reason = match why {
                Outdated::Newer => tr!("invite-outdated-newer"),
                Outdated::Past => tr!("invite-outdated-past"),
            };
            return compact(reason, g::CLOCK, true, false);
        }
        InviteState::Info => return compact(tr!("reading-invite-head", method = inv.method.as_str()), g::INFO, false, false),
        _ => {}
    }
    // Боковая панель (опция «Чтение»): над письмом — тонированная строка.
    if side_wanted(card, me, cx) {
        let when = format!("{} {}", short_date_title(e.span, &tz), time_range(e.span, &tz));
        let hint = tr!("invite-side-strip-hint");
        return div()
            .id("invite-strip")
            .h(px(32.))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .px(px(8.))
            .rounded(px(6.))
            .bg(l.pm(Pm::Secondary))
            .text_color(l.pm(Pm::SecondaryFg))
            .text_size(px(12.))
            .child(icon(g::CALENDAR, 14.))
            .child(div().min_w_0().truncate().child(tr!("invite-side-strip", when = when)))
            .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(hint.clone()).build(window, cx))
            .into_any_element();
    }
    let changed = |c: fn(&Change) -> bool| p.changes.iter().any(c);
    let highlight = |d: Div, on: bool| if on { d.px(px(4.)).rounded(px(3.)).bg(l.picked()) } else { d };
    let was_time = p.changes.iter().find_map(|c| match c {
        Change::Time(s) => Some(*s),
        _ => None,
    });
    let was_place = p.changes.iter().find_map(|c| match c {
        Change::Location(p) => Some(p.clone()),
        _ => None,
    });
    let was_title = p.changes.iter().find_map(|c| match c {
        Change::Title(t) => Some(t.clone()),
        _ => None,
    });
    let old = |text: String| div().text_size(px(12.)).text_color(l.text3()).line_through().child(text);
    // Детали: тема и метка, день и время, место, организатор, ответы.
    let title_row = div()
        .flex()
        .items_center()
        .gap(px(8.))
        .min_w_0()
        .child(highlight(div().flex_1().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD), was_title.is_some()).child(e.title.clone()))
        .when_some(was_title.clone(), |d, t| d.child(old(t)))
        .child(chip(p.answered, &l));
    let when_row = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x(px(8.))
        .min_w_0()
        .child(day_line(e.span, &tz))
        .child(div().text_color(l.text3()).child("·"))
        .child(highlight(div().flex_none().font_weight(FontWeight::SEMIBOLD), changed(|c| matches!(c, Change::Time(_)))).child(if e.all_day {
            tr!("cal-all-day")
        } else {
            time_range(e.span, &tz)
        }))
        .child(length_chip(e.span.len(), &l))
        .when_some(was_time, |d, s| d.child(old(when_line(s, e.all_day, &tz))));
    let place = (!e.location.trim().is_empty() || was_place.is_some()).then(|| {
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(icon(g::PIN, 12.))
            .child(highlight(div().flex_none(), was_place.is_some()).child(e.location.clone()))
            .when_some(was_place.clone(), |d, p| d.when(!p.trim().is_empty(), |d| d.child(old(p))))
    });
    let names = p.names.clone();
    let people = div().flex().items_center().gap(px(8.)).child(dots(inv, &l)).child(
        div()
            .id("invite-people")
            .flex_none()
            .text_color(l.text())
            .child(tr!("invite-accepted-of", n = p.accepted, total = p.total))
            .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(names.clone()).build(window, cx)),
    );
    let facts = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x(px(20.))
        .gap_y(px(4.))
        .text_size(px(12.))
        .text_color(l.text2())
        .children(place)
        .child(div().flex().items_center().gap(px(8.)).child(icon(g::PERSON, 12.)).child(tr!("invite-organizer", name = p.organizer.clone())))
        .child(people);
    let updated_banner = updated_banner(&p.changes, &l);
    let details = div()
        .flex()
        .items_start()
        .gap(px(16.))
        .p(px(16.))
        .child(date_tile(e.span, false, false, &tz, &l))
        .child(div().flex_1().min_w_0().flex().flex_col().gap(px(8.)).children(updated_banner).child(title_row).child(when_row).child(facts));
    // День: на всю ширину карточки.
    let day = div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .px(px(16.))
        .pt(px(12.))
        .pb(px(16.))
        .border_t_1()
        .border_color(l.border())
        .bg(l.pm(Pm::CalOff))
        .child(day_head(card, inv, me, &tz, &l))
        .child(timeline(card, inv, me, &tz, false, &l));
    // Ответ: кнопки в строку или итог; справа — куда уйдёт ответ.
    let actions = div()
        .h(px(56.))
        .flex()
        .items_center()
        .gap(px(4.))
        .px(px(16.))
        .border_t_1()
        .border_color(l.border())
        .min_w_0()
        .map(|d| match (&p.state, p.answered) {
            (InviteState::NotInvited, _) => d.child(div().text_size(px(12.)).text_color(l.text2()).child(tr!("reading-invite-not-invited"))),
            (_, Some(s)) => d.child(done_text(s, &l)).child(
                crate::buttons::secondary("invite-change", &l)
                    .ml(px(12.))
                    .child(tr!("cal-change-answer"))
                    .child(icon(g::DOWN, 10.))
                    .on_click(change_answer(cx)),
            ),
            _ => d.children(answer_buttons(card, false, &l, cx)).when(card.sending.is_some(), |d| {
                d.child(div().ml(px(8.)).text_size(px(12.)).text_color(l.text2()).child(tr!("reading-invite-sending")))
            }),
        })
        .child(route(p.answered.is_some(), "invite-route", &l).ml_auto().pl(px(8.)));
    frame().flex().flex_col().min_w_0().flex_none().child(details).child(day).child(actions).into_any_element()
}

/// Боковая панель приглашения: подробности, кнопки столбиком, лента дня 8–19 (макет «Приглашение» с панелью
/// справа). `None` — опция выключена или приглашению панель не нужна.
pub(crate) fn render_side(card: &Card, me: Option<&str>, cx: &mut Context<ReadingPane>) -> Option<AnyElement> {
    if !side_wanted(card, me, cx) {
        return None;
    }
    let l = crate::roles::look(cx);
    let tz = crate::prefs::get(cx).calendar.zone();
    let inv = card.invitation()?;
    let e = &inv.event;
    let p = Parts::of(card, inv, me);
    let line = |glyph: char| div().h(px(20.)).flex().items_center().gap(px(8.)).min_w_0().child(icon(glyph, 14.).text_color(l.text2()));
    let names = p.names.clone();
    let details = div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .children(updated_banner(&p.changes, &l))
        .child(
            div()
                .h(px(20.))
                .mb(px(4.))
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().flex_1().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(l.text()).child(tr!("invite-side-title")))
                .child(chip(p.answered, &l)),
        )
        .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(l.text()).child(e.title.clone()))
        .child(
            line(g::CLOCK)
                .text_size(px(13.))
                .text_color(l.text())
                .child(div().font_weight(FontWeight::SEMIBOLD).child(when_line(e.span, e.all_day, &tz)))
                .child(length_chip(e.span.len(), &l)),
        )
        .when(!e.location.trim().is_empty(), |d| d.child(line(g::PIN).child(div().truncate().child(e.location.clone()))))
        .child(line(g::PERSON).child(div().truncate().child(tr!("invite-organizer", name = p.organizer.clone()))))
        .child(
            line(g::PEOPLE)
                .child(
                    div()
                        .id("invite-side-people")
                        .text_color(l.text())
                        .child(tr!("invite-accepted-of", n = p.accepted, total = p.total))
                        .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(names.clone()).build(window, cx)),
                )
                .child(dots(inv, &l)),
        );
    let answer = div().flex().flex_col().gap(px(8.)).map(|d| match (&p.state, p.answered) {
        (InviteState::NotInvited, _) => d.child(div().text_color(l.text2()).child(tr!("reading-invite-not-invited"))),
        (_, Some(s)) => d.child(
            div()
                .h(px(32.))
                .flex()
                .items_center()
                .gap(px(12.))
                .px(px(12.))
                .rounded(px(4.))
                .bg(l.fill())
                .text_size(px(13.))
                .child(div().flex_1().min_w_0().child(done_text(s, &l)))
                .child(
                    div()
                        .id("invite-change")
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .text_size(px(12.))
                        .text_color(l.accent_text())
                        .cursor_pointer()
                        .child(tr!("cal-change-answer"))
                        .child(icon(g::DOWN, 10.))
                        .on_click(change_answer(cx)),
                ),
        ),
        _ => d
            .children(answer_buttons(card, true, &l, cx).into_iter().map(|b| b.text_size(px(13.))))
            .when(card.sending.is_some(), |d| d.child(div().text_color(l.text2()).child(tr!("reading-invite-sending")))),
    });
    let day = div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .pt(px(12.))
        .border_t_1()
        .border_color(l.border())
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().font_weight(FontWeight::SEMIBOLD).text_color(l.text()).child(tr!("invite-your-day-title")))
                .child(div().text_color(l.text2()).child(day_line(e.span, &tz))),
        )
        .child(timeline(card, inv, me, &tz, true, &l))
        .children(conflict_line(card, inv, me, &tz, &l));
    Some(
        div()
            .id("invite-side")
            .w(px(SIDE_W))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .gap(px(16.))
            .p(px(16.))
            .border_l_1()
            .border_color(l.border())
            .overflow_y_scroll()
            .text_size(px(12.))
            .text_color(l.text2())
            .child(details)
            .child(div().flex().flex_col().gap(px(8.)).child(answer).child(route(p.answered.is_some(), "invite-route", &l).text_size(px(11.))))
            .child(day)
            .into_any_element(),
    )
}

/// «Организатор изменил: время, место».
fn updated_banner(changes: &[Change], l: &Look) -> Option<Div> {
    (!changes.is_empty()).then(|| {
        let what: Vec<String> = changes
            .iter()
            .map(|c| match c {
                Change::Time(_) => tr!("invite-changed-time"),
                Change::Location(_) => tr!("invite-changed-place"),
                Change::Title(_) => tr!("invite-changed-title"),
            })
            .collect();
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(12.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(l.accent_text())
            .child(icon(g::EDIT, 12.))
            .child(tr!("invite-updated", what = what.join(", ")))
    })
}

/// Куда уйдёт ответ: коротко, подробно — в подсказке.
fn route(done: bool, id: &'static str, l: &Look) -> Stateful<Div> {
    let long = tr!(if done { "invite-route-done" } else { "invite-route" });
    div()
        .id(id)
        .min_w_0()
        .truncate()
        .text_size(px(12.))
        .text_color(l.text3())
        .child(tr!(if done { "invite-route-short-done" } else { "invite-route-short" }))
        .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(long.clone()).build(window, cx))
}

/// Заголовок дня в карточке: «Ваш день», дата и пересечение справа.
fn day_head(card: &Card, inv: &Invitation, me: Option<&str>, tz: &TimeZone, l: &Look) -> Div {
    div()
        .h(px(16.))
        .flex()
        .items_center()
        .gap(px(8.))
        .text_size(px(12.))
        .min_w_0()
        .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).text_color(l.text()).child(tr!("invite-your-day-title")))
        .child(div().flex_none().text_color(l.text2()).child(day_line(inv.event.span, tz)))
        .child(div().flex_1())
        .children(conflict_line(card, inv, me, tz, l))
}

/// «Пересекается: Планёрка 11:30–12:00» (время — само пересечение) или «Пересечений нет»; день не пришёл — «Календарь
/// загружается…».
fn conflict_line(card: &Card, inv: &Invitation, me: Option<&str>, tz: &TimeZone, l: &Look) -> Option<Div> {
    let e = &inv.event;
    let row = || div().flex().items_center().gap(px(8.)).min_w_0().text_size(px(12.)).text_color(l.text2());
    let Some(day) = &card.day else {
        return Some(row().child(tr!("invite-day-loading")));
    };
    let hits = conflicts(e.span, &e.uid, day, me);
    Some(match hits.first() {
        Some(h) => {
            let both = Span::new(h.span.start.max(e.span.start), h.span.end.min(e.span.end));
            let time = time_range(both, tz);
            let text = if hits.len() > 1 {
                tr!("invite-conflict-many", title = h.title.clone(), time = time, n = hits.len() - 1)
            } else {
                tr!("invite-conflict", title = h.title.clone(), time = time)
            };
            row().child(div().size(px(8.)).flex_none().rounded_full().bg(l.pm(Pm::DayOverlap))).child(div().truncate().child(text))
        }
        None => row().child(icon(g::CHECK, 11.)).child(tr!("invite-no-conflict")),
    })
}

/// Вертикальная лента дня: часы слева, мои события серые, приглашение синее, пересечение фиолетовое, линия
/// «сейчас».
fn timeline(card: &Card, inv: &Invitation, me: Option<&str>, tz: &TimeZone, side: bool, l: &Look) -> Div {
    let e = &inv.event;
    let (first, hours) = day_window(e.span, tz, side);
    let Some(z) = zoned(e.span.start, tz) else { return div() };
    let day0 = day_start(z.date(), tz);
    let window = Span::new(day0 + first * 3600, day0 + (first + hours) * 3600);
    let (label_w, area) = if side { (40., 44.) } else { (44., 48.) };
    let height = hours as f32 * ROW_PX;
    let y = |t: i64| ((t - window.start) as f32 / 3600. * ROW_PX).clamp(0., height);
    let lines = (0..=hours).map(|h| div().absolute().left(px(label_w)).right_0().top(px(h as f32 * ROW_PX)).h(px(1.)).bg(l.border()));
    let labels = (0..hours).map(|h| {
        div()
            .absolute()
            .left_0()
            .w(px(36.))
            .top(px(h as f32 * ROW_PX - 7.))
            .h(px(14.))
            .text_size(px(11.))
            .line_height(px(14.))
            .text_color(l.text2())
            .child(format!("{:02}:00", first + h))
    });
    let accepted = matches!(card.state_for(me), Some(InviteState::Answered(Partstat::Accepted)));
    let declined = matches!(card.state_for(me), Some(InviteState::Answered(Partstat::Declined)));
    let blocks = card.day.as_ref().map(|day| day_blocks(e, day, me, window)).unwrap_or_default().into_iter().map(|b| {
        let (top, bottom) = (y(b.span.start), y(b.span.end));
        let invite = l.pm(Pm::DayAccepted);
        let gap = if b.columns > 1 { 2. } else { 0. };
        div()
            .absolute()
            .top(px(top + 1.))
            .h(px((bottom - top - 2.).max(10.)))
            .left(relative(b.column as f32 / b.columns as f32))
            .w(relative(b.span_columns as f32 / b.columns as f32))
            .px(px(gap))
            .child(
                div()
                    .size_full()
                    .flex()
                    // Узкая колонка панели: время — второй строкой, если блок не короче двух строк.
                    .when(side && bottom - top >= 28., |d| d.flex_col())
                    .items_start()
                    .gap(px(if side { 2. } else { 8. }))
                    .px(px(if side { 6. } else { 8. }))
                    .py(px(if bottom - top < 24. {
                        1.
                    } else if side {
                        2.
                    } else {
                        4.
                    }))
                    .rounded(px(4.))
                    .overflow_hidden()
                    .text_size(px(if side { 11. } else { 12. }))
                    .line_height(px(14.))
                    .map(|d| match b.lane {
                        Lane::Invite if accepted => d.bg(invite).text_color(l.pm(Pm::DayAcceptedOn)),
                        Lane::Invite if declined => d.border_1().border_dashed().border_color(invite).text_color(l.text2()),
                        Lane::Invite => d.bg(invite.opacity(0.35)).border_1().border_dashed().border_color(invite).text_color(l.pm(Pm::SecondaryFg)),
                        Lane::Conflict => d.bg(l.pm(Pm::DayOverlap)).text_color(l.pm(Pm::DayOverlapOn)),
                        Lane::Own => d.bg(l.pm(Pm::DayOwn)).text_color(l.pm(Pm::DayOwnFg)),
                    })
                    .child(div().min_w_0().max_w_full().truncate().font_weight(FontWeight::SEMIBOLD).child(b.title.clone()))
                    .child(div().flex_none().opacity(0.8).text_size(px(11.)).child(time_range(b.span, tz))),
            )
    });
    // «Сейчас» — только в сутки встречи, внутри окна.
    let now = card.now();
    let now_line = (now >= window.start && now < window.end).then(|| {
        let top = y(now);
        let tip = tr!("invite-now", time = hm(now, tz));
        div()
            .id("invite-now")
            .absolute()
            .left(px(label_w - 2.))
            .right_0()
            .top(px(top - 2.))
            .h(px(5.))
            .flex()
            .items_center()
            .child(div().size(px(5.)).flex_none().rounded_full().bg(l.text()))
            .child(div().flex_1().h(px(1.)).bg(l.text()))
            .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx))
    });
    div()
        .relative()
        .flex_none()
        .h(px(height))
        .my(px(if side { 8. } else { 4. }))
        .children(lines)
        .children(labels)
        .child(div().absolute().left(px(area)).right_0().top_0().bottom_0().children(blocks))
        .children(now_line)
}

/// «Чт 23 окт».
fn short_date_title(span: Span, tz: &TimeZone) -> String {
    let Some(z) = zoned(span.start, tz) else { return String::new() };
    let d = z.date();
    let wd = tr!("reading-weekday", d = d.weekday().to_monday_zero_offset());
    let mut chars = wd.chars();
    let wd = chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default();
    format!("{wd} {} {}", d.day(), tr!("invite-month-short", m = d.month()))
}

// --- область чтения: мой день и клавиши ---

impl ReadingPane {
    /// Приглашение разобрано: мой день встречи — с сервера (`Command::Events` на сутки) или из демо-недели.
    pub(crate) fn invite_ready(&mut self, cx: &mut Context<Self>) {
        let live = self.live;
        let tz = crate::prefs::get(cx).calendar.zone();
        let Some(card) = self.reading.as_mut().and_then(|r| r.invite.as_mut()) else { return };
        let Some(inv) = card.invitation() else { return };
        let Some(date) = zoned(inv.event.span.start, &tz).map(|z| z.date()) else { return };
        let (from, to) = (day_start(date, &tz), day_start(add_days(date, 1), &tz));
        if !live {
            card.day = Some(demo_day(date, &tz));
            return;
        }
        card.day_pending = Some((from, to));
        cx.emit(ReadingEvent::Sync(SyncRequest::Events { from, to }));
    }

    /// События дня пришли с сервера.
    pub(crate) fn invite_events(&mut self, from: i64, to: i64, events: &[Event], cx: &mut Context<Self>) {
        let Some(card) = self.reading.as_mut().and_then(|r| r.invite.as_mut()) else { return };
        if card.day_pending == Some((from, to)) {
            card.day_pending = None;
            card.day = Some(events.to_vec());
            cx.notify();
        }
    }

    /// События дня не придут (сервера нет, ошибка): карточка без «Вашего дня».
    pub(crate) fn invite_events_failed(&mut self) {
        if let Some(card) = self.reading.as_mut().and_then(|r| r.invite.as_mut()) {
            card.day_pending = None;
            card.day.get_or_insert_with(Vec::new);
        }
    }

    /// Клавиши A / T / N: ответ, если карточка его ждёт. `false` — ответить нечего.
    pub(crate) fn invite_hotkey(&mut self, status: Partstat, cx: &mut Context<Self>) -> bool {
        let me = self.me.clone();
        let can = self.reading.as_ref().and_then(|r| r.invite.as_ref()).is_some_and(|c| c.sending.is_none() && c.can_answer(me.as_deref()));
        if can {
            self.answer_invite(status, cx);
        }
        can
    }

    /// Итог синхронизации для карточки: мой день (`EventsLoaded`), сбой календаря.
    pub(crate) fn invite_sync(&mut self, ev: &SyncEvent, cx: &mut Context<Self>) {
        match ev {
            // Мой день — только мои события (календари коллег — в календаре, S5-22).
            SyncEvent::EventsLoaded { from, to, events, account: None, .. } => {
                let mine: Vec<postman_calendar::Event> = events.iter().filter(|e| e.owner.is_none()).cloned().collect();
                self.invite_events(*from, *to, &mine, cx)
            }
            SyncEvent::CalendarFailed(_) => {
                self.invite_events_failed();
                cx.notify();
            }
            _ => {}
        }
    }
}

/// Демо: мой день встречи — события демо-недели в тот же день недели, перенесённые на дату встречи.
fn demo_day(date: jiff::civil::Date, tz: &TimeZone) -> Vec<Event> {
    let wd = date.weekday();
    postman_demo::calendar::my_events()
        .into_iter()
        .filter_map(|mut e| {
            let d = zoned(e.span.start, tz)?.date();
            if d.weekday() != wd {
                return None;
            }
            let shift = day_start(date, tz) - day_start(d, tz);
            e.span = Span::new(e.span.start + shift, e.span.end + shift);
            Some(e)
        })
        .collect()
}

/// Опись части (ADR 0005, `parts.rs`).
pub(crate) const PART: crate::part::Part =
    crate::part::Part { name: "invite", commands: COMMANDS, handlers, scenes: scenes::SCENES, reset: Some(scenes::reset), ..crate::part::Part::NONE };

/// Обработчики команд части — на корне экрана почты (`Mailer::render`).
fn handlers(root: Div, cx: &mut Context<crate::Mailer>) -> Div {
    root.on_action(cx.listener(|this, _: &invite::Accept, _, cx| this.invite_key(postman_calendar::Partstat::Accepted, cx)))
        .on_action(cx.listener(|this, _: &invite::Tentative, _, cx| this.invite_key(postman_calendar::Partstat::Tentative, cx)))
        .on_action(cx.listener(|this, _: &invite::Decline, _, cx| this.invite_key(postman_calendar::Partstat::Declined, cx)))
}

/// Ответ на приглашение в письме (S5-11): кнопки карточки; клавиши A / T / N — только в схеме «Одиночные клавиши»
/// (S5-17).
pub(crate) mod invite {
    gpui_kit::actions!(invite, [Accept, Tentative, Decline]);
}

/// Команды части (`commands.rs`: контексты и `cmd`).
const COMMANDS: &[pets_commands::Command] = {
    use crate::commands::{LIST, cmd};
    &[
        // Ответ на приглашение — кнопками карточки (схема «Outlook»: буквы без модификатора не допускаются); A / T / N —
        // в схеме «Одиночные клавиши» (S5-17, решение владельца 2026-10-21, вопрос 3).
        cmd("invite.accept", &[], LIST),
        cmd("invite.tentative", &[], LIST),
        cmd("invite.decline", &[], LIST),
    ]
};

/// Сцены `--screens` приглашений (ADR 0005).
pub(crate) mod scenes {
    use gpui_kit::*;
    use postman_cache::BodyRow;

    use crate::Mailer;
    use crate::screens::{self, Scene};

    pub(crate) const SCENES: &[Scene] = &[
        // Письмо с приглашением: карточка встречи (S3-03), новое — с пересечением с моим днём (S5-11).
        Scene::new("invite", invite, invite_ready).after("letter-image").wait(new_invite),
        // Та же карточка: отвечено, изменено организатором, отменено, устарело (S5-11; подмена входов карточки).
        Scene::new("invite-answered", state, state_ready).continues(),
        Scene::new("invite-updated", state, state_ready).continues(),
        Scene::new("invite-cancelled", state, state_ready).continues(),
        Scene::new("invite-outdated", state, state_ready).continues(),
        // Приглашение боковой панелью справа (S5-24, «Настройки → Чтение»): строка над письмом, панель — рядом.
        Scene::new("invite-side", state, state_ready).continues(),
    ];

    /// Боковая панель приглашения — только в своей сцене (S5-24).
    pub(crate) fn reset(s: &Scene, _: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) {
        let side = s.name == "invite-side";
        if crate::prefs::get(cx).reading.invite_side != side {
            crate::prefs::update(cx, |p| p.reading.invite_side = side);
        }
    }

    /// Письмо демо с приглашением (и для сцены `keytips-single`).
    pub(crate) fn invitation(b: &BodyRow) -> bool {
        b.attachments.iter().any(crate::attachments::is_invite)
    }

    fn invite(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        screens::select_demo(m, invitation, cx)
    }

    fn invite_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        let target = screens::demo_id(m.list.read(cx), invitation);
        let shown = |r: &crate::reading::Reading| {
            r.doc.is_some() && r.invite.as_ref().is_some_and(|c| c.invitation().is_some() && c.day.is_some() && c.clock.is_some())
        };
        target.is_some() && screens::reading(m, cx).is_some_and(|r| Some(r.row.id.as_str()) == target && shown(r))
    }

    /// Встреча демо — в прошлом (файл `.ics` с датой): «сейчас» — за сутки до неё, карточка — новое приглашение.
    fn new_invite(_: &Scene, m: &mut Mailer, cx: &mut Context<Mailer>) {
        m.reading.update(cx, |r, cx| {
            let card = r.reading.as_mut().and_then(|r| r.invite.as_mut());
            if let Some(c) = card.filter(|c| c.invitation().is_some() && c.clock.is_none()) {
                c.screen_state("new");
                cx.notify();
            }
        });
    }

    fn state(s: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        let which = s.name.trim_start_matches("invite-");
        m.reading.update(cx, |r, cx| {
            if let Some(c) = r.reading.as_mut().and_then(|r| r.invite.as_mut()) {
                c.screen_state(which);
            }
            cx.notify();
        });
        Ok(())
    }

    fn state_ready(s: &Scene, m: &Mailer, cx: &App) -> bool {
        let side = s.name != "invite-side" || crate::prefs::get(cx).reading.invite_side;
        side && screens::reading(m, cx).and_then(|r| r.invite.as_ref()).is_some_and(|c| c.clock.is_some())
    }
}

#[cfg(test)]
mod tests {
    // Не `use super::*`: вместе с gpui пришёл бы атрибут `gpui::test`, он перекрывает `#[test]`.
    use super::{Card, Lane, State, attendees, day_blocks, day_window, decode, encode, state_key};
    use jiff::tz::TimeZone;
    use postman_calendar::{Invitation, Partstat};
    use postman_core::invite::{InviteState, Seen};
    use serde_json::json;

    fn inv(method: &str) -> Invitation {
        let v = json!({ "method": method, "data": {
            "@type": "Event", "uid": "u-1@partner.example", "title": "Созвон",
            "start": "2026-10-21T11:00:00", "timeZone": "Europe/Moscow", "duration": "PT1H",
            "participants": {
                "o": { "name": "Иван", "calendarAddress": "mailto:ivan@partner.example", "roles": { "owner": true, "attendee": true } },
                "m": { "name": "Я", "calendarAddress": "mailto:me@corp.example", "roles": { "attendee": true }, "participationStatus": "needs-action" },
                "x": { "calendarAddress": "mailto:maria@partner.example", "roles": { "attendee": true }, "participationStatus": "accepted" }
            }
        }});
        Invitation::from_worker(&v, &TimeZone::UTC).unwrap()
    }

    fn ready(method: &str) -> Card {
        let mut c = Card::loading("ics".into());
        c.state = State::Ready(Box::new(inv(method)));
        c.clock = Some("2026-10-20T00:00:00Z".parse::<jiff::Timestamp>().unwrap().as_second());
        c
    }

    #[test]
    fn card_states_and_answer_buttons() {
        let me = Some("ME@corp.example");
        let mut c = ready("REQUEST");
        assert_eq!(c.state_for(me), Some(InviteState::New));
        assert!(c.can_answer(me));
        assert_eq!(c.state_for(Some("other@corp.example")), Some(InviteState::NotInvited));
        assert!(!c.can_answer(Some("other@corp.example")));
        // Ответ принят — запомнен с версией; кнопки — только по «Изменить ответ».
        let stored = c.remember(Partstat::Tentative);
        assert_eq!(decode(&stored).map(|s| s.status), Some(Partstat::Tentative));
        assert_eq!(c.state_for(me), Some(InviteState::Answered(Partstat::Tentative)));
        assert!(!c.can_answer(me));
        c.changing = true;
        assert!(c.can_answer(me));
        assert_eq!(ready("CANCEL").state_for(me), Some(InviteState::Cancelled));
        // Снимки: состояния подменой входов.
        for (which, want) in [("answered", "Answered"), ("updated", "Updated"), ("cancelled", "Cancelled"), ("outdated", "Outdated")] {
            let mut c = ready("REQUEST");
            c.screen_state(which);
            let got = format!("{:?}", c.state_for(me));
            assert!(got.contains(want), "{which}: {got}");
        }
    }

    #[test]
    fn stored_answer_round_trip() {
        for s in [Partstat::Accepted, Partstat::Tentative, Partstat::Declined] {
            assert_eq!(decode(&encode(s)), Some(Seen { status: s, version: None }));
        }
        // Запись S3-03 — одно слово.
        assert_eq!(decode("accepted").map(|s| s.status), Some(Partstat::Accepted));
        assert_eq!(state_key("u-1"), "invite:u-1");
    }

    #[test]
    fn attendees_skip_the_organizer() {
        let a = attendees(&inv("REQUEST"));
        assert_eq!(a, [("maria@partner.example".to_string(), Partstat::Accepted), ("Я".to_string(), Partstat::NeedsAction)]);
    }

    fn event(uid: &str, title: &str, start: &str, end: &str, extra: serde_json::Value) -> postman_calendar::Event {
        let mut v = json!({ "uid": uid, "title": title, "utcStart": start, "utcEnd": end });
        if let (Some(o), Some(x)) = (v.as_object_mut(), extra.as_object()) {
            o.extend(x.clone());
        }
        postman_calendar::Event::from_jmap(&v, &TimeZone::UTC).unwrap()
    }

    /// Лента дня (S5-24): свои события серые, приглашение синее, своё ∩ приглашение — фиолетовое; копия встречи,
    /// отклонённое, «весь день» и события вне окна — не в ленте; пересекающиеся — рядом по колонкам.
    #[test]
    fn day_lane_marks_overlap_with_the_invite() {
        let me = Some("me@corp.example");
        let invite = event("u-1@partner.example", "Созвон", "2026-10-21T11:00:00Z", "2026-10-21T12:00:00Z", json!({}));
        let day = vec![
            event("own-1", "Стендап", "2026-10-21T09:00:00Z", "2026-10-21T09:45:00Z", json!({})),
            event("own-2", "Планёрка", "2026-10-21T11:30:00Z", "2026-10-21T13:00:00Z", json!({})),
            event("own-3", "Обед", "2026-10-21T13:00:00Z", "2026-10-21T14:00:00Z", json!({})),
            // Копия самой встречи в моём календаре — не дублируется.
            event("u-1@partner.example", "Созвон", "2026-10-21T11:00:00Z", "2026-10-21T12:00:00Z", json!({})),
            // Отклонённое мной — скрыто.
            event(
                "own-4",
                "Отклонено",
                "2026-10-21T11:00:00Z",
                "2026-10-21T11:30:00Z",
                json!({ "participants": {
                    "o": { "calendarAddress": "mailto:boss@corp.example", "roles": { "owner": true } },
                    "m": { "calendarAddress": "mailto:me@corp.example", "roles": { "attendee": true }, "participationStatus": "declined" }
                }}),
            ),
            event("own-5", "Весь день", "2026-10-21T00:00:00Z", "2026-10-22T00:00:00Z", json!({ "showWithoutTime": true })),
            // Вне окна 9–15.
            event("own-6", "Вечер", "2026-10-21T18:00:00Z", "2026-10-21T19:00:00Z", json!({})),
        ];
        let (first, hours) = day_window(invite.span, &TimeZone::UTC, false);
        assert_eq!((first, hours), (9, 6), "шесть часов от «за два часа до встречи»");
        let day0 = "2026-10-21T00:00:00Z".parse::<jiff::Timestamp>().unwrap().as_second();
        let window = postman_calendar::Span::new(day0 + first * 3600, day0 + (first + hours) * 3600);
        let blocks = day_blocks(&invite, &day, me, window);
        let lane = |t: &str| blocks.iter().find(|b| b.title == t).map(|b| b.lane);
        assert_eq!(lane("Созвон"), Some(Lane::Invite));
        assert_eq!(lane("Стендап"), Some(Lane::Own));
        assert_eq!(lane("Планёрка"), Some(Lane::Conflict), "свои ∩ приглашение — фиолетовое");
        assert_eq!(lane("Обед"), Some(Lane::Own), "встык — не пересечение");
        assert_eq!(blocks.len(), 4, "{blocks:?}");
        assert_eq!(blocks.iter().filter(|b| b.lane == Lane::Invite).count(), 1);
        // Встреча и планёрка — рядом, по половине.
        let (a, b) = (blocks.iter().find(|b| b.title == "Созвон").unwrap(), blocks.iter().find(|b| b.title == "Планёрка").unwrap());
        assert_eq!((a.columns, b.columns), (2, 2));
        assert_ne!(a.column, b.column);
        // Боковая панель — день 8–19; поздняя встреча — окно сдвигается, она целиком внутри.
        assert_eq!(day_window(invite.span, &TimeZone::UTC, true), (8, 11));
        let late = postman_calendar::Span::new(day0 + 22 * 3600, day0 + 23 * 3600 + 1800);
        let (f, h) = day_window(late, &TimeZone::UTC, false);
        assert!(f <= 22 && f + h >= 24 && f + h <= 24, "{f}+{h}");
        let (f, h) = day_window(late, &TimeZone::UTC, true);
        assert_eq!((f, f + h), (8, 24));
    }

    #[test]
    fn card_waits_for_its_blob() {
        let c = Card::loading("ics".into());
        assert!(c.waits_for("ics") && !c.waits_for("logo"));
        assert_eq!(c.uid(), None);
        let c = ready("REQUEST");
        assert_eq!(c.uid(), Some("u-1@partner.example"));
        assert!(!c.waits_for("ics"));
    }
}
