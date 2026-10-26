//! «Настройки → Автоответ» (S4-05, вид — S5-29 по макету «Настройки → Автоответ»): вкл/выкл, период, тексты
//! «Внутри компании | Внешним» (Markdown, живое оформление и предпросмотр), «Отвечать только контактам», «не
//! отвечать повторно 4 дня», «Вне офиса» в календаре; внизу — «Отмена» и «Сохранить». Полоса «Автоответ включён… ·
//! Изменить · Выключить» — под командной строкой, пока он включён. Сам ответ шлёт сервер, и когда Postman закрыт:
//! `VacationResponse` и свой скрипт Sieve (`postman_core::autoreply`).
//!
//! Даты в полях — местное время `ДД.ММ.ГГГГ` или `ДД.ММ.ГГГГ ЧЧ:ММ`; «по» без времени — включая этот день.

use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputState, TextDecoration, TextDecorationCollection};
use gpui_kit::prelude::*;
use gpui_kit::*;
use jiff::Timestamp;
use jiff::civil::{Date, DateTime};
use jiff::tz::TimeZone;
use pets_i18n::tr;
use pets_ui::theme::{Metric, Role};
use postman_core::autoreply::{AutoReply, Reach, Text};
use postman_core::model::VacationResponse;

use crate::chrome::{glyph, icon};
use crate::compose::{Format, apply_format};
use crate::roles::{Look, Pm};
use crate::settings_tab::{group, seg, seg_group, switch};

/// Высота полосы «Автоответ включён» с отступом снизу: кэшированное view не измеряется по содержимому.
pub(crate) const BAR_HEIGHT: f32 = 38.0;
const ASIDE_W: f32 = 300.0;

/// Поля раздела (чистая модель — тестируется без окна).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Form {
    pub(crate) enabled: bool,
    pub(crate) period: bool,
    pub(crate) from: String,
    pub(crate) to: String,
    /// Тексты для своих и внешних и остальные настройки автоответа Postman.
    pub(crate) reply: AutoReply,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum FormError {
    From,
    To,
    Order,
}

impl FormError {
    fn text(self) -> String {
        match self {
            FormError::From => tr!("vacation-bad-from"),
            FormError::To => tr!("vacation-bad-to"),
            FormError::Order => tr!("vacation-bad-order"),
        }
    }
}

/// Текст какой группы правится.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tab {
    Internal,
    External,
}

impl Tab {
    fn index(self) -> usize {
        match self {
            Tab::Internal => 0,
            Tab::External => 1,
        }
    }
}

fn text_of(reply: &AutoReply, tab: Tab) -> &Text {
    match tab {
        Tab::Internal => &reply.internal,
        Tab::External => &reply.external,
    }
}

fn text_mut(reply: &mut AutoReply, tab: Tab) -> &mut Text {
    match tab {
        Tab::Internal => &mut reply.internal,
        Tab::External => &mut reply.external,
    }
}

/// Дата и время из поля: пусто — `Ok(None)`; дата без времени — начало дня (`end` — начало следующего).
pub(crate) fn parse_local(text: &str, end: bool, tz: &TimeZone) -> Result<Option<Timestamp>, ()> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let civil = match DateTime::strptime("%d.%m.%Y %H:%M", text) {
        Ok(dt) => dt,
        Err(_) => {
            let d = Date::strptime("%d.%m.%Y", text).map_err(|_| ())?;
            let d = if end { d.tomorrow().map_err(|_| ())? } else { d };
            d.to_datetime(jiff::civil::Time::midnight())
        }
    };
    civil.to_zoned(tz.clone()).map(|z| Some(z.timestamp())).map_err(|_| ())
}

/// UTCDate сервера → поле: полночь — только дата («по» — предыдущий день, как его и вводили).
pub(crate) fn format_local(utc: &str, end: bool, tz: &TimeZone) -> String {
    let Ok(ts) = utc.parse::<Timestamp>() else { return String::new() };
    let z = ts.to_zoned(tz.clone());
    if z.time() == jiff::civil::Time::midnight() {
        let d = if end { z.date().yesterday().unwrap_or(z.date()) } else { z.date() };
        d.strftime("%d.%m.%Y").to_string()
    } else {
        z.strftime("%d.%m.%Y %H:%M").to_string()
    }
}

/// Поля → `VacationResponse` (включение, период, текст для других клиентов) и настройки Postman.
pub(crate) fn to_save(f: &Form, tz: &TimeZone) -> Result<(VacationResponse, AutoReply), FormError> {
    let (from, to) = if f.period {
        let from = parse_local(&f.from, false, tz).map_err(|_| FormError::From)?;
        let to = parse_local(&f.to, true, tz).map_err(|_| FormError::To)?;
        if let (Some(a), Some(b)) = (from, to)
            && b <= a
        {
            return Err(FormError::Order);
        }
        (from, to)
    } else {
        (None, None)
    };
    let mut reply = f.reply.clone();
    for t in [&mut reply.internal, &mut reply.external] {
        t.subject = t.subject.trim().to_owned();
        t.body = t.body.trim_end().to_owned();
    }
    let primary = reply.primary();
    let text = |s: &str| Some(s.to_owned()).filter(|s| !s.is_empty());
    let v = VacationResponse {
        is_enabled: f.enabled,
        from_date: from.map(|t| t.to_string()),
        to_date: to.map(|t| t.to_string()),
        subject: text(&primary.subject),
        text_body: text(&primary.body),
        html_body: None,
    };
    Ok((v, reply))
}

/// Состояние сервера → поля; настроек Postman нет (скрипт не наш) — один текст сервера обеим группам.
pub(crate) fn from_server(v: &VacationResponse, reply: Option<&AutoReply>, tz: &TimeZone) -> Form {
    let from = v.from_date.as_deref().map(|d| format_local(d, false, tz)).unwrap_or_default();
    let to = v.to_date.as_deref().map(|d| format_local(d, true, tz)).unwrap_or_default();
    Form {
        enabled: v.is_enabled,
        period: v.from_date.is_some() || v.to_date.is_some(),
        from,
        to,
        reply: reply.cloned().unwrap_or_else(|| AutoReply::from_server(v.subject.as_deref(), v.text_body.as_deref())),
    }
}

/// «пт, 23 октября 18:00»; полночь «по» — предыдущий день без времени.
pub(crate) fn short_moment(utc: &str, end: bool, tz: &TimeZone) -> String {
    let Ok(ts) = utc.parse::<Timestamp>() else { return String::new() };
    let z = ts.to_zoned(tz.clone());
    let midnight = z.time() == jiff::civil::Time::midnight();
    let d = if midnight && end { z.date().yesterday().unwrap_or(z.date()) } else { z.date() };
    let day = tr!(
        "vacation-day",
        wd = tr!("cal-day", d = d.weekday().to_monday_zero_offset()).to_lowercase(),
        day = d.day(),
        month = tr!("cal-month-of", m = d.month())
    );
    if midnight { day } else { format!("{day} {}", z.strftime("%H:%M")) }
}

/// Период словами: «ср, 21 октября 09:00 — пт, 23 октября 18:00»; нет периода — пусто.
pub(crate) fn period_text(v: &VacationResponse, tz: &TimeZone) -> String {
    let from = v.from_date.as_deref().map(|d| short_moment(d, false, tz));
    let to = v.to_date.as_deref().map(|d| short_moment(d, true, tz));
    match (from, to) {
        (Some(a), Some(b)) => format!("{a} — {b}"),
        (Some(a), None) => tr!("vacation-since", from = a),
        (None, Some(b)) => tr!("vacation-until", to = b),
        (None, None) => String::new(),
    }
}

/// Кому уходит ответ — строкой.
pub(crate) fn reach_text(r: Reach) -> String {
    tr!(&format!("vacation-reach-{}", r.id()))
}

/// Полоса «Автоответ включён»: (после жирного «Автоответ включён» — « до …»; кому — серым).
pub(crate) fn bar_text(v: &VacationResponse, reply: Option<&AutoReply>, tz: &TimeZone) -> (String, Option<String>) {
    let until = v.to_date.as_deref().map(|d| tr!("vacation-bar-until", to = short_moment(d, true, tz))).unwrap_or_default();
    (until, reply.map(|r| reach_text(r.audience())))
}

/// Рабочих дней в периоде (по «Настройки → Календарь»): «3 рабочих дня»; периода нет или он не разобран — `None`.
pub(crate) fn work_days(f: &Form, tz: &TimeZone, days: &[bool; 7]) -> Option<usize> {
    let from = parse_local(&f.from, false, tz).ok()??.to_zoned(tz.clone()).date();
    let to = parse_local(&f.to, true, tz).ok()??.to_zoned(tz.clone());
    // Конец исключительно: полночь — этот день уже не входит.
    let last = if to.time() == jiff::civil::Time::midnight() { to.date().yesterday().ok()? } else { to.date() };
    let mut n = 0;
    let mut d = from;
    while d <= last {
        if days[d.weekday().to_monday_zero_offset() as usize] {
            n += 1;
        }
        d = d.tomorrow().ok()?;
    }
    Some(n)
}

/// Markdown текста — без знаков разметки, с оформлением: предпросмотр ответа.
fn preview_text(md: &str, l: &Look) -> StyledText {
    let mut out = String::new();
    let mut hl = Vec::new();
    let mut at = 0;
    for s in postman_compose::display::display(md) {
        if s.range.start > at {
            out.push_str(&md[at..s.range.start]);
        }
        let piece = &md[s.range.clone()];
        if s.look.list {
            out.push_str(if piece.trim_start().starts_with(|c: char| c.is_ascii_digit()) { piece } else { "•" });
        } else if !s.look.markup {
            let start = out.len();
            out.push_str(piece);
            hl.push((start..out.len(), crate::composer::md_style(&s.look, l)));
        }
        at = s.range.end;
    }
    out.push_str(&md[at..]);
    StyledText::new(out).with_highlights(hl)
}

/// Что панель просит у экрана.
pub(crate) enum VacationEvent {
    Save(VacationResponse, AutoReply),
}

impl EventEmitter<VacationEvent> for VacationPanel {}

/// Чей ответ в предпросмотре.
#[derive(Clone, Debug, Default)]
pub(crate) struct Me {
    pub(crate) name: String,
    pub(crate) email: String,
}

pub(crate) struct VacationPanel {
    form: Form,
    /// Как на сервере: «Есть несохранённые изменения» и «Отмена».
    saved: Form,
    saved_at: Option<String>,
    tab: Tab,
    from: Entity<InputState>,
    to: Entity<InputState>,
    subjects: [Entity<InputState>; 2],
    bodies: [Entity<EditorState>; 2],
    marks: [Option<TextDecorationCollection>; 2],
    me: Me,
    /// Рабочие дни (Пн … Вс) — «N рабочих дней» у периода.
    week: [bool; 7],
    /// Поля заново из `form` при следующей отрисовке (пришло состояние сервера, а `Window` нет).
    sync_inputs: bool,
    /// Пользователь правил поля: позднее состояние сервера их не затирает.
    touched: bool,
    /// Живой ящик; в демо сохранить нельзя.
    live: bool,
    /// Состояние сервера ещё не пришло.
    pub(crate) loading: bool,
    pub(crate) busy: bool,
    error: Option<String>,
    /// Предупреждение под переключателем (S4-07: другой фильтр сервера будет выключен).
    pub(crate) warning: Option<String>,
    _subscriptions: Vec<Subscription>,
}

fn tz() -> TimeZone {
    TimeZone::system()
}

fn now_hm() -> String {
    jiff::Zoned::now().strftime("%H:%M").to_string()
}

impl VacationPanel {
    pub(crate) fn new(
        state: Option<&VacationResponse>,
        reply: Option<&AutoReply>,
        me: Me,
        live: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let form = state.map(|v| from_server(v, reply, &tz())).unwrap_or_default();
        let mut line = |value: &str, placeholder: String, cx: &mut Context<Self>| {
            let value = value.to_owned();
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(value))
        };
        let from = line(&form.from, tr!("vacation-date-placeholder"), cx);
        let to = line(&form.to, tr!("vacation-date-placeholder"), cx);
        let subjects = [
            line(&form.reply.internal.subject, tr!("vacation-subject-placeholder"), cx),
            line(&form.reply.external.subject, tr!("vacation-subject-placeholder"), cx),
        ];
        let mut editor = |value: &str, cx: &mut Context<Self>| {
            let value = value.to_owned();
            cx.new(|cx| {
                EditorState::new(window, cx)
                    .line_number(false)
                    .folding(false)
                    .indent_guides(false)
                    .searchable(false)
                    .auto_close(false)
                    .soft_wrap(true)
                    .placeholder(tr!("vacation-body-placeholder"))
                    .default_value(value)
            })
        };
        let bodies = [editor(&form.reply.internal.body, cx), editor(&form.reply.external.body, cx)];
        let mut subscriptions = Vec::new();
        for state in [&from, &to, &subjects[0], &subjects[1]] {
            subscriptions.push(cx.subscribe_in(state, window, |this, _, ev: &InputEvent, _, cx| this.changed(ev, None, cx)));
        }
        for (i, state) in bodies.iter().enumerate() {
            subscriptions.push(cx.subscribe_in(state, window, move |this, _, ev: &InputEvent, _, cx| this.changed(ev, Some(i), cx)));
        }
        let week = crate::prefs::get(cx).calendar.work_days;
        let mut p = Self {
            saved: form.clone(),
            saved_at: state.is_some().then(now_hm),
            form,
            tab: Tab::Internal,
            from,
            to,
            subjects,
            bodies,
            marks: [None, None],
            me,
            week,
            sync_inputs: false,
            touched: false,
            live,
            loading: live && state.is_none(),
            busy: false,
            error: None,
            warning: None,
            _subscriptions: subscriptions,
        };
        p.paint(0, cx);
        p.paint(1, cx);
        p
    }

    fn changed(&mut self, ev: &InputEvent, body: Option<usize>, cx: &mut Context<Self>) {
        if let InputEvent::Change = ev {
            self.read_inputs(cx);
            self.touched = self.form != self.saved;
            self.error = None;
            if let Some(i) = body {
                self.paint(i, cx);
            }
            cx.notify();
        }
    }

    /// Живое оформление Markdown в поле текста (как в письме, S5-27).
    fn paint(&mut self, i: usize, cx: &mut Context<Self>) {
        let l = crate::roles::look(cx);
        let text = self.bodies[i].read(cx).value().to_string();
        let marks: Vec<TextDecoration> = postman_compose::display::display(&text)
            .into_iter()
            .map(|s| TextDecoration::new(s.range, crate::composer::md_style(&s.look, &l)))
            .collect();
        match &self.marks[i] {
            Some(c) => c.set(marks, cx),
            None => {
                let c = self.bodies[i].update(cx, |s, cx| s.create_decorations_collection(marks, cx));
                self.marks[i] = Some(c);
            }
        }
    }

    /// Состояние сервера: поля заново, если пользователь их ещё не правил.
    pub(crate) fn server_state(&mut self, v: &VacationResponse, reply: Option<&AutoReply>, cx: &mut Context<Self>) {
        self.loading = false;
        if !self.touched && !self.busy {
            self.form = from_server(v, reply, &tz());
            self.saved = self.form.clone();
            self.saved_at = Some(now_hm());
            self.sync_inputs = true;
        }
        cx.notify();
    }

    /// Сервер сохранил: поля — как на сервере.
    pub(crate) fn saved(&mut self, v: &VacationResponse, reply: Option<&AutoReply>, cx: &mut Context<Self>) {
        self.busy = false;
        self.touched = false;
        self.error = None;
        self.form = from_server(v, reply, &tz());
        self.saved = self.form.clone();
        self.saved_at = Some(now_hm());
        self.sync_inputs = true;
        cx.notify();
    }

    pub(crate) fn failed(&mut self, error: String, cx: &mut Context<Self>) {
        self.busy = false;
        self.loading = false;
        self.error = Some(error);
        cx.notify();
    }

    /// Демо: сохранить нельзя (экран говорит тостом).
    pub(crate) fn not_saved(&mut self, cx: &mut Context<Self>) {
        self.busy = false;
        cx.notify();
    }

    fn read_inputs(&mut self, cx: &App) {
        self.form.from = self.from.read(cx).value().to_string();
        self.form.to = self.to.read(cx).value().to_string();
        for tab in [Tab::Internal, Tab::External] {
            let i = tab.index();
            let t = text_mut(&mut self.form.reply, tab);
            t.subject = self.subjects[i].read(cx).value().to_string();
            t.body = self.bodies[i].read(cx).value().to_string();
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.loading {
            return;
        }
        self.read_inputs(cx);
        match to_save(&self.form, &tz()) {
            Ok((v, reply)) => {
                self.error = None;
                self.busy = self.live;
                cx.emit(VacationEvent::Save(v, reply));
            }
            Err(e) => self.error = Some(e.text()),
        }
        cx.notify();
    }

    /// «Отмена»: как на сервере.
    fn cancel(&mut self, cx: &mut Context<Self>) {
        self.form = self.saved.clone();
        self.touched = false;
        self.error = None;
        self.sync_inputs = true;
        cx.notify();
    }

    fn set(&mut self, f: impl FnOnce(&mut Form), cx: &mut Context<Self>) {
        f(&mut self.form);
        self.touched = true;
        cx.notify();
    }

    /// Кнопка панели форматирования: разметка вокруг выделения текста.
    fn format(&mut self, f: Format, window: &mut Window, cx: &mut Context<Self>) {
        let i = self.tab.index();
        let (text, sel) = {
            let s = self.bodies[i].read(cx);
            (s.value().to_string(), s.selected_range())
        };
        let (next, sel) = apply_format(&text, sel, f);
        self.bodies[i].update(cx, |s, cx| {
            s.replace_all(next, window, cx);
            s.set_selected_range(sel, cx);
            s.focus(window, cx);
        });
        self.touched = true;
        self.read_inputs(cx);
        self.paint(i, cx);
        cx.notify();
    }

    fn dirty(&self) -> bool {
        self.form != self.saved
    }
}

/// Квадратик флажка: включён — залит акцентом с галочкой.
pub(crate) fn checkbox(on: bool) -> Div {
    let l = crate::roles::now();
    div()
        .size(px(16.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .border_1()
        .border_color(if on { l.accent() } else { l.c(Role::StrokeStrong) })
        .when(on, |d| d.bg(l.accent()).text_color(l.on_accent()).text_size(px(11.)).child("✓"))
}

use crate::buttons::plain as button;

/// Строка «название и пояснение — переключатель».
fn toggle_row(id: &'static str, title: Div, hint: String, on: bool, l: &Look) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_4()
        .min_h(px(44.))
        .py_1p5()
        .border_b_1()
        .border_color(l.border())
        .cursor_pointer()
        .child(div().flex_1().min_w_0().flex().flex_col().gap_0p5().child(title).child(div().text_size(px(12.)).text_color(l.text2()).child(hint)))
        .child(switch(on, l))
}

fn tool(id: &'static str, l: &Look) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(26.))
        .min_w(px(28.))
        .px_1()
        .flex()
        .items_center()
        .justify_center()
        .rounded(l.px(Metric::RadiusControl))
        .cursor_pointer()
        .hover(|d| d.bg(l.hover()))
}

impl VacationPanel {
    fn master(&self, l: &Look, cx: &mut Context<Self>) -> Stateful<Div> {
        let on = self.form.enabled;
        let note = if !on {
            tr!("vacation-master-off")
        } else {
            match to_save(&self.form, &tz()) {
                Ok((v, _)) if self.form.period && (v.from_date.is_some() || v.to_date.is_some()) => {
                    tr!("vacation-master-period", period = period_text(&v, &tz()))
                }
                _ => tr!("vacation-master-on"),
            }
        };
        div()
            .id("vacation-enabled")
            .flex()
            .items_center()
            .gap_3()
            .p_3()
            .rounded(l.px(Metric::RadiusOverlay))
            .border_1()
            .border_color(l.c(Role::StrokeCard))
            .cursor_pointer()
            .child(
                div()
                    .size(px(36.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(l.px(Metric::RadiusControl))
                    .when(on, |d| d.bg(l.pm(Pm::Secondary)).text_color(l.pm(Pm::SecondaryFg)))
                    .when(!on, |d| d.bg(l.fill()).text_color(l.text2()))
                    .child(icon(glyph::MAIL, 16.)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(tr!("vacation-enabled")))
                    .child(div().text_size(px(12.)).text_color(l.text2()).child(note)),
            )
            .child(switch(on, l))
            .on_click(cx.listener(|this, _, _, cx| this.set(|f| f.enabled = !f.enabled, cx)))
    }

    fn period(&self, l: &Look, cx: &mut Context<Self>) -> Div {
        let on = self.form.period;
        let hint = if on { tr!("vacation-period-on") } else { tr!("vacation-period-off") };
        let count = work_days(&self.form, &tz(), &self.week).map(|n| tr!("vacation-work-days", n = n as i64));
        let dates = div()
            .flex()
            .items_center()
            .gap_2()
            .py_2()
            .when(!on, |d| d.opacity(0.45))
            .child(div().text_color(l.text2()).child(tr!("vacation-from")))
            .child(div().w(px(170.)).child(Input::new(&self.from).disabled(!on)))
            .child(div().text_color(l.text2()).child(tr!("vacation-to")))
            .child(div().w(px(170.)).child(Input::new(&self.to).disabled(!on)))
            .when_some(count.filter(|_| on), |d, c| d.child(div().pl_2().text_size(px(12.)).text_color(l.text2()).child(c)));
        div()
            .flex()
            .flex_col()
            .child(group(tr!("vacation-period-group"), l))
            .child(
                toggle_row("vacation-period", div().child(tr!("vacation-period")), hint, on, l)
                    .on_click(cx.listener(|this, _, _, cx| this.set(|f| f.period = !f.period, cx))),
            )
            .child(dates)
    }

    fn texts(&self, l: &Look, window: &Window, cx: &mut Context<Self>) -> Div {
        // Шрифт текста — как у письма, а не моноширинный редактора.
        let text = window.text_style();
        let tab = self.tab;
        let i = tab.index();
        let tabs = seg_group(
            [(Tab::Internal, "vacation-tab-internal"), (Tab::External, "vacation-tab-external")].map(|(t, key)| {
                seg(("vacation-tab", t.index()), tr!(key), t == tab, true, l).on_click(cx.listener(move |this, _, _, cx| {
                    this.tab = t;
                    cx.notify();
                }))
            }),
            l,
        );
        let only = self.form.reply.contacts_only;
        let under = match tab {
            Tab::Internal => div().text_size(px(12.)).text_color(l.text2()).child(tr!("vacation-internal-hint")),
            Tab::External => div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .id("vacation-contacts-only")
                        .flex()
                        .items_center()
                        .gap_2()
                        .cursor_pointer()
                        .child(checkbox(only))
                        .child(tr!("vacation-contacts-only"))
                        .on_click(cx.listener(|this, _, _, cx| this.set(|f| f.reply.contacts_only = !f.reply.contacts_only, cx))),
                )
                .child(div().pl_6().text_size(px(12.)).text_color(l.text2()).child(if only {
                    tr!("vacation-contacts-only-on")
                } else {
                    tr!("vacation-contacts-only-off")
                })),
        };
        let sep = || div().w_px().h(px(16.)).mx_1().bg(l.stroke());
        let fmt = |id: &'static str, f: Format, cx: &mut Context<Self>| {
            tool(id, l).on_click(cx.listener(move |this, _, window, cx| this.format(f, window, cx)))
        };
        let toolbar = div()
            .flex()
            .items_center()
            .gap_0p5()
            .px_1()
            .py_0p5()
            .border_b_1()
            .border_color(l.border())
            .text_color(l.pm(Pm::LabelAccent))
            .child(fmt("vac-bold", Format::Bold, cx).font_weight(FontWeight::BOLD).child(tr!("compose-fmt-bold")))
            .child(fmt("vac-italic", Format::Italic, cx).italic().child(tr!("compose-fmt-italic")))
            .child(fmt("vac-underline", Format::Underline, cx).underline().child(tr!("compose-fmt-underline")))
            .child(sep())
            .child(fmt("vac-bullets", Format::Bullets, cx).child(icon(glyph::BULLETS, 14.)))
            .child(fmt("vac-numbers", Format::Numbers, cx).text_size(px(12.)).child("1."))
            .child(fmt("vac-link", Format::Link, cx).child(icon(glyph::LINK, 14.)))
            .child(div().flex_1())
            .child(div().px_2().text_size(px(11.)).text_color(l.text3()).child(tr!("vacation-markdown")));
        let editor = div()
            .flex()
            .flex_col()
            .rounded(l.px(Metric::RadiusControl))
            .border_1()
            .border_color(l.stroke())
            .bg(l.c(Role::FillControlInput))
            .child(toolbar)
            .child(
                div().h(px(200.)).px_2().child(
                    Editor::new(&self.bodies[i])
                        .appearance(false)
                        .font_family(text.font_family.clone())
                        .text_size(text.font_size)
                        .line_height(relative(1.5))
                        .h(relative(1.)),
                ),
            );
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(group(tr!("vacation-body"), l))
            .child(div().flex().child(tabs))
            .child(under)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().w(px(56.)).flex_none().text_color(l.text2()).child(tr!("vacation-subject")))
                    .child(div().flex_1().child(Input::new(&self.subjects[i]))),
            )
            .child(editor)
    }

    fn extra(&self, l: &Look, cx: &mut Context<Self>) -> Div {
        let r = &self.form.reply;
        let badge = div().px_1p5().rounded_sm().bg(l.fill()).text_size(px(11.)).text_color(l.text2()).child(tr!("vacation-recommended"));
        let once_title = div().flex().items_center().gap_2().child(tr!("vacation-once", n = i64::from(r.days))).child(badge);
        let once_hint = if r.once { tr!("vacation-once-on") } else { tr!("vacation-once-off") };
        let ooo_hint = if r.out_of_office { tr!("vacation-ooo-on") } else { tr!("vacation-ooo-off") };
        div()
            .flex()
            .flex_col()
            .child(group(tr!("vacation-extra-group"), l))
            .child(
                toggle_row("vacation-once", once_title, once_hint, r.once, l)
                    .on_click(cx.listener(|this, _, _, cx| this.set(|f| f.reply.once = !f.reply.once, cx))),
            )
            .child(
                toggle_row("vacation-ooo", div().child(tr!("vacation-ooo")), ooo_hint, r.out_of_office, l)
                    .on_click(cx.listener(|this, _, _, cx| this.set(|f| f.reply.out_of_office = !f.reply.out_of_office, cx))),
            )
    }

    fn footer(&self, l: &Look, cx: &mut Context<Self>) -> Div {
        let busy = self.busy || self.loading;
        let (text, dot) = if self.busy {
            (tr!("vacation-saving"), l.text3())
        } else if self.dirty() {
            (tr!("vacation-dirty"), l.text())
        } else if !self.live {
            (tr!("vacation-demo"), l.text3())
        } else if let Some(at) = &self.saved_at {
            (tr!("vacation-saved-at", time = at.clone()), l.text3())
        } else {
            (tr!("vacation-loading"), l.text3())
        };
        div()
            .flex()
            .items_center()
            .gap_3()
            .pt_4()
            .mt_2()
            .border_t_1()
            .border_color(l.border())
            .child(div().size(px(6.)).rounded_full().bg(dot))
            .child(div().flex_1().min_w_0().text_size(px(12.)).text_color(l.text2()).child(text))
            .child(button("vacation-cancel", tr!("vacation-cancel"), false).on_click(cx.listener(|this, _, _, cx| this.cancel(cx))))
            .child(
                button("vacation-save", tr!("vacation-save"), !busy)
                    .when(busy, |d| d.opacity(0.6))
                    .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
            )
    }

    /// Предпросмотр: письмо-ответ той группе, что правится, и кому и когда он уходит.
    fn preview(&self, l: &Look) -> Div {
        let f = &self.form;
        let t = text_of(&f.reply, self.tab);
        let badge = if !f.enabled {
            tr!("vacation-preview-off")
        } else if self.tab == Tab::Internal {
            tr!("vacation-preview-internal")
        } else {
            tr!("vacation-preview-external")
        };
        let domain = self.me.email.rsplit_once('@').map(|(_, d)| d.to_owned()).unwrap_or_default();
        let to = match self.tab {
            Tab::Internal => tr!("vacation-preview-to-colleague", domain = domain.clone()),
            Tab::External => tr!("vacation-preview-to-external"),
        };
        let who = match self.tab {
            Tab::Internal => tr!("vacation-rule-internal", domain = domain),
            Tab::External if f.reply.contacts_only => tr!("vacation-rule-contacts"),
            Tab::External => tr!("vacation-rule-everyone"),
        };
        let once = if f.reply.once { tr!("vacation-rule-once", n = i64::from(f.reply.days)) } else { tr!("vacation-rule-each") };
        let when = match to_save(f, &tz()) {
            Ok((v, _)) if f.period && (v.from_date.is_some() || v.to_date.is_some()) => {
                tr!("vacation-rule-period", period = period_text(&v, &tz()))
            }
            _ => tr!("vacation-rule-always"),
        };
        let name = if self.me.name.is_empty() { self.me.email.clone() } else { self.me.name.clone() };
        let body = if t.is_empty() {
            div().text_color(l.text3()).child(tr!("vacation-preview-empty")).into_any_element()
        } else {
            div().line_height(px(19.)).child(preview_text(t.body.trim_end(), l)).into_any_element()
        };
        let rule = |g: char, text: String| {
            div().flex().items_start().gap_2().child(icon(g, 12.).text_color(l.text2()).mt(px(3.))).child(div().flex_1().child(text))
        };
        let subject = if t.subject.trim().is_empty() { tr!("vacation-subject-placeholder") } else { t.subject.trim().to_owned() };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded(l.px(Metric::RadiusOverlay))
            .border_1()
            .border_color(l.c(Role::StrokeCard))
            .bg(l.panel())
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(div().flex_1().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(tr!("vacation-preview")))
                    .child(div().px_1p5().rounded_sm().bg(l.fill()).text_size(px(11.)).text_color(l.text2()).child(badge)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .rounded(l.px(Metric::RadiusControl))
                    .bg(l.fill_passive())
                    .when(!f.enabled, |d| d.opacity(0.55))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(crate::calendar::avatar(&name, 28., l))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(div().font_weight(FontWeight::SEMIBOLD).truncate().child(name.clone()))
                                    .child(div().text_size(px(11.)).text_color(l.text2()).truncate().child(to)),
                            )
                            .child(div().text_size(px(11.)).text_color(l.text3()).child(now_hm())),
                    )
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(subject))
                    .child(body),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .text_size(px(12.))
                    .text_color(l.text2())
                    .child(rule('\u{E716}', who))
                    .child(rule('\u{E72C}', once))
                    .child(rule('\u{E787}', when)),
            )
    }
}

impl Render for VacationPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let l = crate::roles::look(cx);
        if std::mem::take(&mut self.sync_inputs) {
            let f = self.form.clone();
            for (state, value) in [
                (&self.from, f.from.clone()),
                (&self.to, f.to.clone()),
                (&self.subjects[0], f.reply.internal.subject.clone()),
                (&self.subjects[1], f.reply.external.subject.clone()),
            ] {
                state.update(cx, |s, cx| s.set_value(value, window, cx));
            }
            for (state, value) in [(&self.bodies[0], f.reply.internal.body.clone()), (&self.bodies[1], f.reply.external.body.clone())] {
                state.update(cx, |s, cx| s.replace_all(value, window, cx));
            }
            self.paint(0, cx);
            self.paint(1, cx);
        }
        let main = div()
            .flex_1()
            .min_w_0()
            .max_w(px(760.))
            .flex()
            .flex_col()
            .gap_2()
            .child(self.master(&l, cx))
            .when_some(self.warning.clone(), |d, w| d.child(div().text_color(l.warning()).child(w)))
            .when(self.loading, |d| d.child(div().text_color(l.text2()).child(tr!("vacation-loading"))))
            .child(self.period(&l, cx))
            .child(self.texts(&l, window, cx))
            .child(self.extra(&l, cx))
            .when_some(self.error.clone(), |d, e| d.child(div().pt_2().text_color(l.error()).child(e)))
            .child(self.footer(&l, cx));
        let tip = crate::settings_tab::aside_card(tr!("vacation-tip"), vec![tr!("vacation-tip-1"), tr!("vacation-tip-2")], &l);
        div()
            .id("vacation-panel")
            .flex()
            .items_start()
            .gap_6()
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .px_6()
            .py_4()
            .child(main)
            .child(div().w(px(ASIDE_W)).flex_none().flex().flex_col().gap_2().child(self.preview(&l)).child(tip))
    }
}

/// Полоса «Автоответ включён» под строкой команд (макет «Главное окно: автоответ»): нейтральная.
pub(crate) struct VacationBar {
    /// « до пт, 23 октября 18:00» после жирного «Автоответ включён».
    pub(crate) until: String,
    /// «коллегам и внешним из контактов»; настроек Postman нет — нет.
    pub(crate) reach: Option<String>,
}

/// «Выключить»; «Изменить» — команда `settings.vacation`, как из строки команд.
pub(crate) enum BarEvent {
    TurnOff,
}

impl EventEmitter<BarEvent> for VacationBar {}

impl Render for VacationBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let l = crate::roles::look(cx);
        let link = |id: &'static str, text: String| {
            div()
                .id(id)
                .h(px(24.))
                .px_2()
                .flex()
                .items_center()
                .rounded(l.px(Metric::RadiusControl))
                .cursor_pointer()
                .text_color(l.accent_text())
                .hover(|d| d.bg(l.hover()))
                .child(text)
        };
        div().size_full().px_2().pb(px(6.)).child(
            div()
                .flex()
                .size_full()
                .items_center()
                .gap(px(10.))
                .px(px(14.))
                .rounded(px(6.))
                .bg(l.fill())
                .text_size(px(13.))
                .text_color(l.text())
                .child(icon(glyph::MAIL, 14.).text_color(l.text2()))
                .child(div().flex().min_w_0().child(div().font_weight(FontWeight::SEMIBOLD).child(tr!("vacation-bar-on"))).child(self.until.clone()))
                .when_some(self.reach.clone(), |d, r| d.child(div().text_color(l.text2()).truncate().child(format!("· {r}"))))
                .child(div().flex_1())
                .child(
                    link("vacation-bar-edit", tr!("vacation-bar-edit"))
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(crate::settings::settings::Vacation), cx)),
                )
                .child(
                    link("vacation-bar-off", tr!("vacation-bar-off"))
                        .font_weight(FontWeight::SEMIBOLD)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(BarEvent::TurnOff))),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    // Не `super::*`: из `gpui_kit::*` пришёл бы атрибут `test` gpui вместо встроенного.
    use super::{AutoReply, Form, FormError, Text, TimeZone, format_local, from_server, parse_local, to_save, work_days};

    fn moscow() -> TimeZone {
        TimeZone::get("Europe/Moscow").unwrap()
    }

    #[test]
    fn dates_are_local_and_the_end_day_is_included() {
        let tz = moscow();
        let t = |s: &str, end| parse_local(s, end, &tz).unwrap().map(|t| t.to_string());
        assert_eq!(t("", false), None);
        assert_eq!(t("20.10.2026", false).as_deref(), Some("2026-10-19T21:00:00Z"));
        assert_eq!(t("27.10.2026", true).as_deref(), Some("2026-10-27T21:00:00Z"));
        assert_eq!(t(" 27.10.2026 18:30 ", true).as_deref(), Some("2026-10-27T15:30:00Z"));
        assert!(parse_local("27/10/2026", false, &tz).is_err());
        assert!(parse_local("32.10.2026", false, &tz).is_err());
        assert_eq!(format_local("2026-10-19T21:00:00Z", false, &tz), "20.10.2026");
        assert_eq!(format_local("2026-10-27T21:00:00Z", true, &tz), "27.10.2026");
        assert_eq!(format_local("2026-10-27T15:30:00Z", true, &tz), "27.10.2026 18:30");
        assert_eq!(format_local("not a date", true, &tz), "");
    }

    fn form() -> Form {
        Form {
            enabled: true,
            period: true,
            from: "21.10.2026 09:00".into(),
            to: "23.10.2026 18:00".into(),
            reply: AutoReply {
                internal: Text { subject: " Нет на месте ".into(), body: "Коллеги, я в отпуске.\n\n".into() },
                external: Text { subject: "Автоответ".into(), body: "Спасибо за письмо.".into() },
                ..AutoReply::default()
            },
        }
    }

    /// Поля → сервер и обратно: текст `VacationResponse` — внешний (другие клиенты видят один), настройки Postman —
    /// целиком; без периода даты не уходят; ошибки дат.
    #[test]
    fn form_round_trip() {
        let tz = moscow();
        let (v, reply) = to_save(&form(), &tz).unwrap();
        assert_eq!(v.from_date.as_deref(), Some("2026-10-21T06:00:00Z"));
        assert_eq!(v.to_date.as_deref(), Some("2026-10-23T15:00:00Z"));
        assert_eq!((v.subject.as_deref(), v.text_body.as_deref()), (Some("Автоответ"), Some("Спасибо за письмо.")));
        assert_eq!((reply.internal.subject.as_str(), reply.internal.body.as_str()), ("Нет на месте", "Коллеги, я в отпуске."));
        let back = from_server(&v, Some(&reply), &tz);
        assert_eq!(back, Form { reply: reply.clone(), ..form() });
        // Настроек Postman нет — один текст сервера обеим группам.
        let plain = from_server(&v, None, &tz);
        assert_eq!((plain.reply.internal.body.as_str(), plain.reply.contacts_only), ("Спасибо за письмо.", false));
        let (v, _) = to_save(&Form { period: false, ..form() }, &tz).unwrap();
        assert_eq!((v.from_date, v.to_date), (None, None));
        assert_eq!(to_save(&Form { from: "завтра".into(), ..form() }, &tz).map(|_| ()), Err(FormError::From));
        assert_eq!(to_save(&Form { to: "1.1".into(), ..form() }, &tz).map(|_| ()), Err(FormError::To));
        assert_eq!(to_save(&Form { to: "19.10.2026".into(), ..form() }, &tz).map(|_| ()), Err(FormError::Order));
        // Внешнего текста нет — серверу текст коллегам.
        let mut f = form();
        f.reply.external = Text::default();
        assert_eq!(to_save(&f, &tz).unwrap().0.subject.as_deref(), Some("Нет на месте"));
    }

    /// «3 рабочих дня»: ср–пт; «по» без времени включает день; выходные не считаются.
    #[test]
    fn work_days_of_the_period() {
        let tz = moscow();
        let week = [true, true, true, true, true, false, false];
        assert_eq!(work_days(&form(), &tz, &week), Some(3));
        let f = Form { from: "23.10.2026".into(), to: "26.10.2026".into(), ..form() };
        assert_eq!(work_days(&f, &tz, &week), Some(2), "пт и пн");
        assert_eq!(work_days(&Form { to: String::new(), ..form() }, &tz, &week), None);
    }
}
