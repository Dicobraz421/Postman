//! Поручение на экране (S6-10; макеты «Поручил я» `TasksDelegated`, «Поручено мне» `TaskAssigned`, DESIGN «Задачи
//! (Ctrl+4)»): форма «Поручить» в карточке задачи, карточка поручения у поручившего (ход, лента шагов, «Напомнить»,
//! «Принять результат» / «Вернуть с комментарием», «Изменить срок», «Забрать») и у исполнителя («Принять» /
//! «Предложить срок» / «Отклонить», затем «В работе» → «Готово — на проверку»), круги статусов 20 px; та же
//! карточка — над текстом письма-поручения ([`mail_card`]).
//!
//! Модель — `postman_core::assign` (действия, письма iTIP, приём писем) над `postman_calendar::assign` (статусы,
//! таблица переходов, лента). Своя копия задачи пишется как правка задачи (кэш, затем сервер без рассылки), письма
//! шага — `TasksAction::Delegate` → `Command::Delegate`. Демо: правка — в памяти окна, писем нет (тост).

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use pets_i18n::tr;
use pets_ui::Level as Toast;
use postman_calendar::assign::{self, Action, Incoming, Status, Step, StepKind};
use postman_calendar::{Moment, Role, Task};
use postman_compose::Address;
use postman_core::assign::{self as flow, Plan, Received};
use postman_core::send_later::Scheduled;

use super::{TasksAction, TasksView, add_days, day_of, g};
use crate::chrome::icon;
use crate::roles::{Look, Pm};

/// Варианты срока в формах (дней от сегодня), как в макете: сегодня, завтра, +2, +3, +6.
pub(crate) const DUE_DAYS: [i64; 5] = [0, 1, 2, 3, 6];
/// Подсказок исполнителя — не больше.
const SUGGEST: usize = 5;

/// Что открыто в карточке поручения.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Edit {
    /// «Поручить» (`reassign` — «Поручить другому» после отказа).
    Assign {
        reassign: bool,
    },
    Return,
    Decline,
    Propose,
    ChangeDue,
}

/// Письма отмены, которые сервер держит окно «Отменить» (S6-11: «Забрать», «Удалить поручение»).
#[derive(Debug, Clone)]
pub(crate) struct Held {
    uid: String,
    /// Ответ сервера (`Event::Delegated`); `None` — ещё нет.
    scheduled: Option<Vec<Scheduled>>,
    /// «Отменить» нажато до ответа сервера.
    undone: bool,
    at: std::time::Instant,
}

/// Состояние форм поручения в разделе «Задачи».
pub(crate) struct State {
    pub(crate) edit: Option<Edit>,
    pub(crate) person: Entity<InputState>,
    pub(crate) watcher: Entity<InputState>,
    pub(crate) comment: Entity<InputState>,
    pub(crate) to: Option<Address>,
    pub(crate) watchers: Vec<Address>,
    /// Срок формы: дней от сегодня.
    pub(crate) due_days: Option<i64>,
    pub(crate) suggest: Vec<Address>,
    /// Подсказки — для поля наблюдателя (иначе — исполнителя).
    suggest_watcher: bool,
    held: Vec<Held>,
}

impl State {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<TasksView>) -> (Self, Vec<Subscription>) {
        let field = |key: &str, window: &mut Window, cx: &mut Context<TasksView>| {
            let p = tr!(key);
            cx.new(|cx| InputState::new(window, cx).placeholder(p))
        };
        let person = field("assign-form-person-placeholder", window, cx);
        let watcher = field("assign-form-watcher-placeholder", window, cx);
        let comment = field("assign-form-comment-placeholder", window, cx);
        let subs = vec![
            cx.subscribe(&person, |this: &mut TasksView, state, ev: &InputEvent, cx| {
                let text = state.read(cx).value().to_string();
                this.deleg_typed(false, &text, ev, cx);
            }),
            cx.subscribe(&watcher, |this: &mut TasksView, state, ev: &InputEvent, cx| {
                let text = state.read(cx).value().to_string();
                this.deleg_typed(true, &text, ev, cx);
            }),
            cx.subscribe(&comment, |this: &mut TasksView, _, ev: &InputEvent, cx| {
                // Enter — отправить (Ctrl+Enter — тоже: макет «Поручить», Ctrl+Enter).
                if let InputEvent::PressEnter { .. } = ev {
                    this.deleg_submit(cx);
                }
            }),
        ];
        let s = Self {
            edit: None,
            person,
            watcher,
            comment,
            to: None,
            watchers: Vec::new(),
            due_days: None,
            suggest: Vec::new(),
            suggest_watcher: false,
            held: Vec::new(),
        };
        (s, subs)
    }
}

/// Кто я в поручении.
fn owner_side(t: &Task, me: &str) -> bool {
    t.organizer().is_some_and(|o| o.email.eq_ignore_ascii_case(me))
}

/// Главное действие поручения для Space: что по макету залито акцентом.
pub(crate) fn primary(st: Status, owner: bool) -> Option<Action> {
    match (owner, st) {
        (true, Status::Done) => Some(Action::AcceptResult),
        (true, Status::Proposed) => Some(Action::AgreeDue),
        (false, Status::Waiting) => Some(Action::Accept),
        (false, Status::Accepted) => Some(Action::Start),
        (false, Status::InProgress) => Some(Action::Finish),
        _ => None,
    }
}

/// Срок формы (`days` от сегодня): время — как у прежнего срока, иначе 18:00 (конец рабочего дня).
fn due_at(days: i64, old: Option<&Moment>, today: Date) -> Moment {
    let time = old.map(|m| m.local.time()).filter(|t| *t != jiff::civil::Time::midnight()).unwrap_or(jiff::civil::time(18, 0, 0, 0));
    Moment { local: add_days(today, days).to_datetime(time), tz: old.and_then(|m| m.tz.clone()) }
}

fn due_label(days: i64, today: Date) -> String {
    match days {
        0 => tr!("assign-due-today"),
        1 => tr!("assign-due-tomorrow"),
        n => {
            let d = add_days(today, n);
            format!("{} {}", crate::calendar::day_name(d.weekday().to_monday_zero_offset() as usize), d.day())
        }
    }
}

/// «чт 22.10, до 18:00».
fn moment_text(m: &Moment, tz: &TimeZone) -> String {
    let Some(ts) = m.timestamp(tz) else { return String::new() };
    let Some(z) = crate::calendar::zoned(ts, tz) else { return String::new() };
    tr!("assign-due-text", day = crate::calendar::long_day(z.date()), time = z.strftime("%H:%M").to_string())
}

/// Когда: сегодня — «14:05», раньше — «19.10 14:05».
fn when(ts: i64, now: i64, tz: &TimeZone) -> String {
    let t = crate::calendar::hm(ts, tz);
    if day_of(ts, tz) == day_of(now, tz) {
        return t;
    }
    crate::calendar::zoned(ts, tz).map(|z| format!("{} {t}", z.strftime("%d.%m"))).unwrap_or(t)
}

/// Имя участника по адресу (в задаче), «Вы» — я.
fn who(t: &Task, email: &str, me: &str) -> String {
    if email.eq_ignore_ascii_case(me) {
        return tr!("assign-you");
    }
    t.participants
        .iter()
        .find(|p| p.email.eq_ignore_ascii_case(email))
        .map(|p| if p.name.is_empty() { p.email.clone() } else { p.name.clone() })
        .unwrap_or_else(|| email.to_owned())
}

fn kind_id(k: StepKind) -> &'static str {
    match k {
        StepKind::Assigned => "assigned",
        StepKind::Accepted => "accepted",
        StepKind::Declined => "declined",
        StepKind::Proposed => "proposed",
        StepKind::Started => "started",
        StepKind::Finished => "finished",
        StepKind::Recalled => "recalled",
        StepKind::Reopened => "reopened",
        StepKind::Reminded => "reminded",
        StepKind::DueChanged => "due-changed",
        StepKind::Agreed => "agreed",
        StepKind::Kept => "kept",
        StepKind::ResultAccepted => "result-accepted",
        StepKind::Returned => "returned",
        StepKind::Revoked => "revoked",
        StepKind::Reassigned => "reassigned",
    }
}

/// Что сделано шагом: «готово, на проверку», «новый срок: …».
fn step_what(t: &Task, s: &Step, tz: &TimeZone) -> String {
    let due = s
        .due
        .as_deref()
        .and_then(|d| d.parse().ok())
        .map(|local| moment_text(&Moment { local, tz: t.due.as_ref().and_then(|m| m.tz.clone()) }, tz))
        .unwrap_or_default();
    tr!(&format!("assign-what-{}", kind_id(s.kind)), due = due)
}

/// Строка ленты: «Петров Алексей — готово, на проверку».
fn step_text(t: &Task, s: &Step, me: &str, tz: &TimeZone) -> String {
    tr!("assign-step", who = who(t, &s.who, me), what = step_what(t, s, tz))
}

/// Текст статуса со стороны поручившего или исполнителя.
pub(crate) fn status_text(st: Status, owner: bool) -> String {
    tr!(&format!("assign-{}-{}", if owner { "st" } else { "me-st" }, st.id()))
}

/// Круг статуса 20 px: ждёт — серый точечный, принял — синий ✓, в работе — синий ▶, готово — зелёный ✓✓,
/// отклонил — тёмно-серый ✕ (DESIGN «Задачи»).
pub(crate) fn status_circle(st: Status, size: f32, l: &Look) -> Div {
    let base = div().size(px(size)).flex_none().rounded_full().flex().items_center().justify_center().text_color(l.pm(Pm::AssignGlyph));
    let glyph = size * 0.5;
    match st {
        Status::Waiting | Status::Proposed => base.border_2().border_dashed().border_color(l.pm(Pm::AssignWait)),
        Status::Accepted => base.bg(l.pm(Pm::AssignBlue)).child(icon(g::CHECK, glyph)),
        Status::InProgress => base.bg(l.pm(Pm::AssignBlue)).child(icon(g::PLAY, glyph)),
        Status::Done => base.bg(l.pm(Pm::AssignGreen)).child(icon(g::CHECK, glyph * 0.85)).child(icon(g::CHECK, glyph * 0.85).ml(px(-size * 0.22))),
        Status::Closed => base.bg(l.pm(Pm::AssignGreen)).child(icon(g::CHECK, glyph)),
        Status::Declined => base.bg(l.pm(Pm::AssignDeclined)).child(icon(g::CANCEL, glyph * 0.85)),
        Status::Revoked => base.border_2().border_color(l.text3()).text_color(l.text3()).child(icon(g::CANCEL, glyph * 0.8)),
    }
}

/// Ход поручения: Поручено → Принято → В работе → Готово → Принято вами; время — первый шаг этапа.
fn ribbon(t: &Task, st: Status, owner: bool, now: i64, tz: &TimeZone, l: &Look) -> Div {
    let steps = assign::steps(t);
    let first = |k: &[StepKind]| steps.iter().find(|s| k.contains(&s.kind)).map(|s| when(s.at, now, tz)).unwrap_or_default();
    let cur = match st {
        Status::Waiting | Status::Proposed | Status::Declined | Status::Revoked => 0,
        Status::Accepted => 1,
        Status::InProgress => 2,
        Status::Done => 3,
        Status::Closed => 4,
    };
    let times = [
        first(&[StepKind::Assigned]),
        first(&[StepKind::Accepted, StepKind::Agreed]),
        first(&[StepKind::Started]),
        first(&[StepKind::Finished]),
        first(&[StepKind::ResultAccepted]),
    ];
    let mut row = div().flex().items_start().w_full();
    for (i, time) in times.iter().enumerate() {
        let reached = i <= cur && !(st == Status::Declined && i > 0);
        let declined = st == Status::Declined && i == 1;
        let next = st == Status::Done && i == 4;
        let dot = div()
            .size(px(10.))
            .rounded_full()
            .when(reached, |d| d.bg(l.accent()))
            .when(declined, |d| d.bg(l.pm(Pm::AssignDeclined)))
            .when(next, |d| d.border_2().border_color(l.accent_text()))
            .when(!reached && !declined && !next, |d| d.border_1().border_color(l.stroke()).bg(l.panel()));
        let line = |on: bool, hidden: bool| div().flex_1().h(px(2.)).when(hidden, |d| d.invisible()).bg(if on { l.accent() } else { l.border() });
        let label = match (declined, i) {
            (true, _) => tr!("assign-rib-declined"),
            // Последний этап у исполнителя и наблюдателя — «Проверено» (макеты).
            (false, 4) if !owner => tr!("assign-rib-checked"),
            _ => tr!(&format!("assign-rib-{}", i + 1)),
        };
        row = row.child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(4.))
                .child(div().w_full().flex().items_center().child(line(i <= cur && i > 0, i == 0)).child(dot).child(line(i < cur, i == 4)))
                .child(
                    div()
                        .text_size(px(11.))
                        .truncate()
                        .when(i == cur || declined, |d| d.font_weight(FontWeight::SEMIBOLD).text_color(l.text()))
                        .when(next, |d| d.font_weight(FontWeight::SEMIBOLD).text_color(l.accent_text()))
                        .when(i != cur && !declined && !next, |d| d.text_color(if reached { l.text2() } else { l.text3() }))
                        .child(label),
                )
                .child(div().h(px(14.)).text_size(px(10.)).text_color(l.text3()).child(time.clone())),
        );
    }
    row
}

impl TasksView {
    /// Мой адрес (Identity): поручения — с моей стороны.
    pub(crate) fn set_me(&mut self, me: Address, cx: &mut Context<Self>) {
        self.me = Some(me);
        self.reshow(cx);
    }

    pub(crate) fn set_book(&mut self, book: crate::compose::Book) {
        self.book = Some(book);
    }

    pub(crate) fn me_email(&self) -> String {
        self.me.as_ref().map(|a| a.email.clone()).unwrap_or_default()
    }

    /// Выбранная задача — поручение: статус и моя сторона.
    pub(crate) fn delegation(&self) -> Option<(Task, Status, bool)> {
        let t = self.selected_task()?.clone();
        let st = assign::status(&t)?;
        let owner = owner_side(&t, &self.me_email());
        Some((t, st, owner))
    }

    fn deleg_reset(&mut self, cx: &mut Context<Self>) {
        let d = &mut self.deleg;
        d.edit = None;
        d.to = None;
        d.watchers.clear();
        d.suggest.clear();
        d.due_days = None;
        cx.notify();
    }

    /// Открыть форму или ввод на месте; поля — пустые.
    pub(crate) fn deleg_open(&mut self, edit: Edit, window: &mut Window, cx: &mut Context<Self>) {
        self.deleg_reset(cx);
        self.deleg.edit = Some(edit);
        self.due_open = false;
        for f in [self.deleg.person.clone(), self.deleg.watcher.clone(), self.deleg.comment.clone()] {
            f.update(cx, |s, cx| s.set_value("", window, cx));
        }
        if self.book.is_none() {
            cx.emit(TasksAction::NeedBook);
        }
        let focus = match edit {
            Edit::Assign { .. } => Some(self.deleg.person.clone()),
            Edit::Return | Edit::Decline | Edit::Propose => Some(self.deleg.comment.clone()),
            Edit::ChangeDue => None,
        };
        if let Some(f) = focus {
            f.update(cx, |s, cx| s.focus(window, cx));
        }
        if let (Edit::Assign { .. }, Some(t)) = (edit, self.selected_task()) {
            // Срок задачи — выбранным, если он среди вариантов.
            let today = day_of(self.now(), &self.tz).unwrap_or_default();
            self.deleg.due_days =
                t.due.as_ref().and_then(|d| d.date_in(&self.tz)).map(|d| (d - today).get_days() as i64).filter(|n| DUE_DAYS.contains(n)).or(Some(2));
        }
        cx.notify();
    }

    /// «Поручить» (командная строка): своя задача без поручения; у отклонённого — «Поручить другому».
    pub(crate) fn delegate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.selected_task().cloned() else { return };
        if postman_core::mailtask::is_letter(&t) {
            cx.emit(TasksAction::Toast(Toast::Info, tr!("assign-letter-first")));
            return;
        }
        match assign::status(&t) {
            None => self.deleg_open(Edit::Assign { reassign: false }, window, cx),
            Some(Status::Declined) if owner_side(&t, &self.me_email()) => self.deleg_open(Edit::Assign { reassign: true }, window, cx),
            Some(_) => cx.emit(TasksAction::Toast(Toast::Info, tr!("assign-invalid"))),
        }
    }

    /// Esc: закрыть форму поручения.
    pub(crate) fn deleg_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if self.deleg.edit.is_none() {
            return false;
        }
        self.deleg_reset(cx);
        true
    }

    fn deleg_typed(&mut self, watcher: bool, text: &str, ev: &InputEvent, cx: &mut Context<Self>) {
        match ev {
            InputEvent::Change | InputEvent::Focus => {
                self.deleg.suggest_watcher = watcher;
                let q = text.trim();
                self.deleg.suggest = match (&self.book, q.len()) {
                    (Some(book), n) if n >= 2 => book
                        .borrow()
                        .suggest(q, SUGGEST, self.now())
                        .into_iter()
                        .map(|e| Address::new(&e.name, &e.email))
                        .filter(|a| self.me.as_ref().is_none_or(|m| !m.email.eq_ignore_ascii_case(&a.email)))
                        .collect(),
                    _ => Vec::new(),
                };
                cx.notify();
            }
            InputEvent::PressEnter { .. } => {
                // Первая подсказка; иначе — набранный адрес как есть.
                let pick = self.deleg.suggest.first().cloned().or_else(|| postman_compose::parse_addresses(text).0.into_iter().next());
                if let Some(a) = pick {
                    self.deleg_pick(watcher, a, cx);
                }
            }
            _ => {}
        }
    }

    fn deleg_pick(&mut self, watcher: bool, a: Address, cx: &mut Context<Self>) {
        if watcher {
            if !self.deleg.watchers.iter().any(|w| w.email.eq_ignore_ascii_case(&a.email)) {
                self.deleg.watchers.push(a);
            }
        } else {
            self.deleg.to = Some(a);
        }
        self.deleg.suggest.clear();
        cx.notify();
    }

    /// Ввод формы отправить: «Поручить», «Вернуть», «Отклонить», «Отправить» (срок).
    pub(crate) fn deleg_submit(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.deleg.edit else { return };
        let comment = self.deleg.comment.read(cx).value().to_string();
        let action = match edit {
            Edit::Assign { reassign: false } => return self.deleg_assign(comment, cx),
            Edit::Assign { reassign: true } => Action::Reassign,
            Edit::Return => Action::Return,
            Edit::Decline => Action::Decline,
            Edit::Propose => Action::ProposeDue,
            Edit::ChangeDue => Action::ChangeDue,
        };
        if matches!(edit, Edit::Assign { .. }) && self.deleg.to.is_none() {
            return cx.emit(TasksAction::Toast(Toast::Info, tr!("assign-form-pick-first")));
        }
        self.deleg_act(action, comment, cx);
    }

    fn deleg_assign(&mut self, comment: String, cx: &mut Context<Self>) {
        let (Some(t), Some(me)) = (self.selected_task().cloned(), self.me.clone()) else { return };
        let Some(to) = self.deleg.to.clone() else {
            return cx.emit(TasksAction::Toast(Toast::Info, tr!("assign-form-pick-first")));
        };
        let today = day_of(self.now(), &self.tz).unwrap_or_default();
        let due = self.deleg.due_days.map(|d| due_at(d, t.due.as_ref(), today));
        let watchers = self.deleg.watchers.clone();
        let input = flow::Input { comment, due, to: Some(to) };
        match flow::assign(&t, &me, &watchers, &input, self.now(), &self.tz) {
            Ok(plan) => self.deleg_run(plan, cx),
            Err(e) => tracing::warn!("delegate: {e}"),
        }
    }

    /// Действие поручения выбранной задачи (кнопки карточки и письма, Space).
    pub(crate) fn deleg_act(&mut self, action: Action, comment: String, cx: &mut Context<Self>) {
        let Some(t) = self.selected_task().cloned() else { return };
        self.deleg_act_on(t, action, comment, cx);
    }

    fn deleg_act_on(&mut self, t: Task, action: Action, comment: String, cx: &mut Context<Self>) {
        if action == Action::Revoke {
            return self.take_back(t, false, cx);
        }
        let today = day_of(self.now(), &self.tz).unwrap_or_default();
        let due = match action {
            Action::ChangeDue | Action::ProposeDue => self.deleg.due_days.map(|d| due_at(d, t.due.as_ref(), today)),
            _ => None,
        };
        if matches!(action, Action::ChangeDue | Action::ProposeDue) && due.is_none() {
            return cx.emit(TasksAction::Toast(Toast::Info, tr!("assign-pick-due")));
        }
        let input = flow::Input { comment, due, to: self.deleg.to.clone() };
        match flow::act(&t, &self.me_email(), action, &input, self.now(), &self.tz) {
            Ok(plan) => self.deleg_run(plan, cx),
            Err(flow::Error::TooSoon { next }) => {
                cx.emit(TasksAction::Toast(Toast::Info, tr!("assign-too-soon", time = when(next, self.now(), &self.tz))));
            }
            Err(e) => {
                tracing::warn!(uid = %t.uid, "delegation: {e}");
                cx.emit(TasksAction::Toast(Toast::Info, tr!("assign-invalid")));
            }
        }
    }

    /// Письма шага. Мои письма поручившего — со словом поручения (`assign::keyword`): по нему «Ждём» находит их в
    /// «Отправленных» (S6-11).
    fn drafts_of(&self, plan: &Plan) -> Vec<postman_compose::Draft> {
        let me = self.me.clone().unwrap_or_else(|| Address::new("", ""));
        let title = &plan.task.title;
        let mine = plan.task.is_delegated_by(&me.email);
        plan.outgoing
            .iter()
            .map(|o| {
                let mut d = flow::draft(o, &me, letter_subject(o.kind, title), letter_body(&plan.task, o.kind, &me, &self.tz));
                if mine {
                    d.keywords.push(assign::keyword(&plan.task.uid));
                }
                d
            })
            .collect()
    }

    /// Итог действия: своя копия — правкой задачи, письма шага — синхронизации; демо — только копия и тост.
    fn deleg_run(&mut self, plan: Plan, cx: &mut Context<Self>) {
        let title = plan.task.title.clone();
        let first = plan.outgoing.first().map(|o| o.kind);
        let sent = self.deleg_send(plan, cx);
        self.deleg_reset(cx);
        let text = first.map_or_else(|| tr!("assign-toast-saved"), |k| tr!(&format!("assign-toast-{}", kind_id(k)), title = title));
        if sent {
            cx.emit(TasksAction::Toast(Toast::Success, text));
        } else {
            cx.emit(TasksAction::Toast(Toast::Info, tr!("assign-toast-demo", text = text)));
        }
    }

    /// Своя копия — правкой, письма — синхронизации; `false` — демо (писем нет).
    fn deleg_send(&mut self, plan: Plan, cx: &mut Context<Self>) -> bool {
        let uid = plan.task.uid.clone();
        let drafts = self.drafts_of(&plan);
        self.save(vec![plan.task], cx);
        if self.live && !drafts.is_empty() {
            cx.emit(TasksAction::Delegate { uid, drafts, hold: false });
            return true;
        }
        false
    }

    /// «Забрать» (`delete: false` — задача снова моя, обычная) или «Удалить поручение» (Del): у исполнителя —
    /// отмена (CANCEL), у меня — копия без поручения или ничего. Письмо отмены сервер держит окно «Отменить»
    /// (как отправку письма, S5-06): «Отменить» в тосте — копия как была, письмо не уходит (решение владельца
    /// 2026-10-22, макет «Поручил я»).
    fn take_back(&mut self, t: Task, delete: bool, cx: &mut Context<Self>) {
        let plan = match flow::act(&t, &self.me_email(), Action::Revoke, &flow::Input::default(), self.now(), &self.tz) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(uid = %t.uid, "delegation: {e}");
                return cx.emit(TasksAction::Toast(Toast::Info, tr!("assign-invalid")));
            }
        };
        let drafts = self.drafts_of(&plan);
        let who = assign::assignee(&t).map(|p| if p.name.is_empty() { p.email.clone() } else { p.name.clone() }).unwrap_or_default();
        let text = if delete {
            self.remove(&t.uid, cx);
            tr!("assign-toast-deleted", who = who)
        } else {
            let mine = assign::taken_back(&plan.task);
            let list = self.list_of(&mine);
            self.save(vec![mine], cx);
            tr!("assign-toast-taken", list = list)
        };
        self.deleg_reset(cx);
        let text = if self.live && !drafts.is_empty() {
            let now = std::time::Instant::now();
            self.deleg.held.retain(|h| h.uid != t.uid && now.duration_since(h.at) < std::time::Duration::from_secs(120));
            self.deleg.held.push(Held { uid: t.uid.clone(), scheduled: None, undone: false, at: now });
            cx.emit(TasksAction::Delegate { uid: t.uid.clone(), drafts, hold: true });
            text
        } else {
            tr!("assign-toast-demo", text = text)
        };
        cx.emit(TasksAction::Undo { text, undo: super::Undo::Delegation(Box::new(t)) });
    }

    /// Список, куда задача попадёт: «Сегодня», «Предстоящие» или «Входящие».
    fn list_of(&self, t: &Task) -> String {
        let today = day_of(self.now(), &self.tz).unwrap_or_default();
        let key = match t.due.as_ref().and_then(|d| d.date_in(&self.tz)) {
            Some(d) if d <= today => "tasks-today",
            Some(_) => "tasks-upcoming",
            None if t.project.is_some() => return t.project.clone().unwrap_or_default(),
            None => "tasks-inbox",
        };
        tr!(key)
    }

    /// Del: моё поручение — удалить (с отменой у исполнителя); чужое (мне поручили, наблюдаю) — не удалять.
    /// `false` — выбранная не поручение.
    pub(crate) fn delete_delegation(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((t, st, owner)) = self.delegation() else { return false };
        if !owner {
            cx.emit(TasksAction::Toast(Toast::Info, tr!("assign-delete-not-mine")));
        } else if st.is_final() {
            return false;
        } else {
            self.take_back(t, true, cx);
        }
        true
    }

    /// «Отменить» после «Забрать» или «Удалить»: копия — как была; письма отмены — не отправлять (сервер ещё
    /// держит их) или, если ответа сервера ещё нет, — сразу по ответу.
    pub(crate) fn undo_delegation(&mut self, before: Task, cx: &mut Context<Self>) {
        let uid = before.uid.clone();
        self.save(vec![before], cx);
        let Some(i) = self.deleg.held.iter().position(|h| h.uid == uid) else { return };
        match self.deleg.held[i].scheduled.clone() {
            Some(s) => {
                self.deleg.held.remove(i);
                self.withhold(s, cx);
            }
            None => self.deleg.held[i].undone = true,
        }
    }

    /// Сервер принял письма шага (`Event::Delegated`).
    pub(crate) fn on_delegated(&mut self, uid: &str, scheduled: &[Scheduled], cx: &mut Context<Self>) {
        let Some(i) = self.deleg.held.iter().position(|h| h.uid == uid) else { return };
        if self.deleg.held[i].undone {
            self.deleg.held.remove(i);
            self.withhold(scheduled.to_vec(), cx);
        } else {
            self.deleg.held[i].scheduled = Some(scheduled.to_vec());
        }
    }

    fn withhold(&mut self, scheduled: Vec<Scheduled>, cx: &mut Context<Self>) {
        if scheduled.is_empty() {
            // Сервер не держал письмо (нет FUTURERELEASE) — оно уже ушло.
            cx.emit(TasksAction::Toast(Toast::Info, tr!("assign-withhold-late")));
        } else {
            cx.emit(TasksAction::Withhold(scheduled));
        }
    }

    /// Сколько исполнителей ждут принятия — «Напомнить всем» (мои, не наблюдаю).
    pub(crate) fn remind_all_count(&self) -> usize {
        let me = self.me_email();
        self.tasks.iter().filter(|t| t.is_delegated_by(&me) && assign::status(t) == Some(Status::Waiting)).count()
    }

    /// «Напомнить всем» над «Ждут принятия»: каждому, кто ещё не принял, — напоминание (не чаще раза в сутки).
    pub(crate) fn remind_all(&mut self, cx: &mut Context<Self>) {
        let me = self.me_email();
        let list: Vec<Task> = self.tasks.iter().filter(|t| t.is_delegated_by(&me) && assign::status(t) == Some(Status::Waiting)).cloned().collect();
        let (mut n, mut soon, mut sent) = (0i64, 0i64, false);
        for t in list {
            match flow::act(&t, &me, Action::Remind, &flow::Input::default(), self.now(), &self.tz) {
                Ok(plan) => {
                    sent |= self.deleg_send(plan, cx);
                    n += 1;
                }
                Err(flow::Error::TooSoon { .. }) => soon += 1,
                Err(e) => tracing::warn!(uid = %t.uid, "delegation: {e}"),
            }
        }
        let text = if n > 0 { tr!("assign-toast-reminded-all", n = n, soon = soon) } else { tr!("assign-remind-all-soon") };
        let text = if sent || n == 0 { text } else { tr!("assign-toast-demo", text = text) };
        cx.emit(TasksAction::Toast(if n > 0 { Toast::Success } else { Toast::Info }, text));
    }

    /// Вторая строка строки поручения: кто (исполнитель или поручивший) и последний шаг; наблюдаю — ещё и кто
    /// поручил; предложенный срок — янтарём.
    pub(crate) fn deleg_meta(&self, t: &Task, l: &Look) -> Vec<AnyElement> {
        let me = self.me_email();
        let owner = owner_side(t, &me);
        let watched = assign::is_watched_by(t, &me);
        let person = if owner || watched { assign::assignee(t) } else { t.organizer() };
        let name = person.map(|p| if p.name.is_empty() { p.email.clone() } else { p.name.clone() }).unwrap_or_default();
        let now = self.now();
        let mut last = assign::steps(t)
            .last()
            .map(|s| {
                let what = step_what(t, s, &self.tz);
                tr!("deleg-last", when = when(s.at, now, &self.tz), what = what)
            })
            .unwrap_or_default();
        if watched {
            let by = t.organizer().map(|p| if p.name.is_empty() { p.email.clone() } else { p.name.clone() }).unwrap_or_default();
            last = tr!("deleg-last-watched", who = by, last = last);
        }
        let mut out = vec![
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(6.))
                .text_color(l.text())
                .child(crate::calendar::avatar(&name, 18., l).text_size(px(9.)))
                .child(div().whitespace_nowrap().child(name))
                .into_any_element(),
        ];
        if !last.is_empty() {
            out.push(div().min_w_0().truncate().child(last).into_any_element());
        }
        if let Some(p) = assign::proposed(t).filter(|_| assign::status(t) == Some(Status::Proposed)) {
            let today = day_of(now, &self.tz).unwrap_or_default();
            let d = p.date_in(&self.tz).unwrap_or(today);
            out.push(
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(l.pm(Pm::DeadlineFg))
                    .child(tr!("deleg-proposes", due = due_label((d - today).get_days() as i64, today)))
                    .into_any_element(),
            );
        }
        out
    }

    /// Над списком «Поручений»: вкладки «Я поручил» | «Мне поручили» и «Только свои» (на «Я поручил»).
    pub(crate) fn deleg_bar(&self, l: &Look, cx: &mut Context<Self>) -> Div {
        let c = flow::counts(&self.tasks, &self.me_email(), self.own_only);
        let tab = match self.nav {
            super::Nav::Delegations(t) => t,
            _ => flow::Tab::ByMe,
        };
        let seg = |id: &'static str, label: String, n: usize, which: flow::Tab, cx: &mut Context<Self>| {
            let on = tab == which;
            div()
                .id(id)
                .h(px(26.))
                .px(px(12.))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded(px(4.))
                .cursor_pointer()
                .when(on, |d| d.bg(l.panel()).shadow_sm().font_weight(FontWeight::SEMIBOLD))
                .when(!on, |d| d.text_color(l.text2()))
                .child(label)
                .child(div().text_color(l.text2()).font_weight(FontWeight::NORMAL).child(n.to_string()))
                .on_click(cx.listener(move |this, _, _, cx| this.set_tab(which, cx)))
        };
        let segs = div()
            .flex()
            .flex_none()
            .p(px(2.))
            .gap(px(2.))
            .rounded(px(6.))
            .bg(l.fill())
            .child(seg("deleg-tab-by-me", tr!("deleg-tab-by-me"), c.by_me_shown, flow::Tab::ByMe, cx))
            .child(seg("deleg-tab-to-me", tr!("deleg-tab-to-me"), c.to_me, flow::Tab::ToMe, cx));
        let own = (tab == flow::Tab::ByMe).then(|| {
            div()
                .id("deleg-own")
                .flex()
                .items_center()
                .gap(px(8.))
                .cursor_pointer()
                .child(crate::list::checkbox(self.own_only, l))
                .child(tr!("deleg-own-only"))
                .on_click(cx.listener(|this, _, _, cx| this.toggle_own(cx)))
        });
        div().flex_none().flex().items_center().gap(px(16.)).px(px(14.)).pb(px(10.)).text_size(px(13.)).child(segs).children(own)
    }

    /// Под списком «Я поручил»: невыполненные поручения — в «Ждём» почты, «Открыть в почте».
    pub(crate) fn deleg_hint(&self, l: &Look, cx: &mut Context<Self>) -> Div {
        let c = flow::counts(&self.tasks, &self.me_email(), self.own_only);
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(14.))
            .py(px(8.))
            .border_t_1()
            .border_color(l.border())
            .text_size(px(12.))
            .text_color(l.text2())
            .child(div().flex_1().min_w_0().child(tr!("deleg-wait-hint", n = c.waiting as i64)))
            .child(
                crate::settings_tab::link("deleg-open-wait", tr!("deleg-open-wait"), l)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(TasksAction::OpenWaiting))),
            )
    }

    /// Space у поручения: главное действие стороны (принять результат, согласиться, принять, в работе, готово).
    /// `false` — у поручения его нет (Space ничего не делает).
    pub(crate) fn deleg_primary(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((_, st, owner)) = self.delegation() else { return false };
        if let Some(a) = primary(st, owner) {
            self.deleg_act(a, String::new(), cx);
        }
        true
    }

    /// Insert у поручения: срок — изменить (поручивший) или предложить (исполнитель).
    pub(crate) fn deleg_due(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some((_, st, owner)) = self.delegation() else { return false };
        let edit = if owner { Edit::ChangeDue } else { Edit::Propose };
        let action = if owner { Action::ChangeDue } else { Action::ProposeDue };
        if assign::next(st, action).is_ok() {
            self.deleg_open(edit, window, cx);
        }
        true
    }

    /// «Напомнить» (командная строка).
    pub(crate) fn remind(&mut self, cx: &mut Context<Self>) {
        if let Some((_, st, true)) = self.delegation()
            && assign::next(st, Action::Remind).is_ok()
        {
            self.deleg_act(Action::Remind, String::new(), cx);
        }
    }

    /// «Забрать» (командная строка): сразу, тост «Отменить» (S6-11).
    pub(crate) fn revoke(&mut self, cx: &mut Context<Self>) {
        if let Some((t, st, true)) = self.delegation()
            && assign::next(st, Action::Revoke).is_ok()
        {
            self.take_back(t, false, cx);
        }
    }

    /// Письмо iTIP открыто (область чтения): учесть его в своей копии — шаг с комментарием, предложенный срок.
    pub(crate) fn receive_mail(&mut self, inc: &Incoming, cx: &mut Context<Self>) {
        let Some(own) = self.tasks.iter().find(|t| t.uid == inc.task.uid).cloned() else { return };
        if let Received::Applied(t) = flow::receive(&own, inc, self.now()) {
            self.save(vec![*t], cx);
        }
    }

    /// Кнопка карточки в письме: задача поручения — в разделе, действие — над ней; ещё не пришла — сказать.
    pub(crate) fn mail_action(&mut self, uid: &str, action: Option<Action>, cx: &mut Context<Self>) -> bool {
        let Some(t) = self.tasks.iter().find(|t| t.uid == uid).cloned() else {
            cx.emit(TasksAction::Toast(Toast::Info, tr!("assign-not-yet")));
            return false;
        };
        match action {
            Some(a) => self.deleg_act_on(t, a, String::new(), cx),
            None => {
                self.nav = super::Nav::List(postman_core::tasks::List::Today);
                self.selected = Some(t.uid.clone());
                self.reshow(cx);
                self.selected = Some(t.uid);
                cx.notify();
            }
        }
        true
    }

    /// Снимок `delegate`: форма «Поручить» с исполнителем, наблюдателем, сроком и комментарием (демо).
    pub(crate) fn screen_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use postman_demo::delegations as d;
        self.deleg_open(Edit::Assign { reassign: false }, window, cx);
        self.deleg_pick(false, Address::new(d::KUZNETSOVA.0, d::KUZNETSOVA.1), cx);
        self.deleg_pick(true, Address::new(d::PETROV.0, d::PETROV.1), cx);
        self.deleg.due_days = Some(2);
        self.deleg.comment.update(cx, |s, cx| s.set_value(tr!("assign-screen-comment"), window, cx));
        cx.notify();
    }

    pub(crate) fn screen_form_open(&self) -> bool {
        matches!(self.deleg.edit, Some(Edit::Assign { .. })) && self.deleg.to.is_some()
    }

    // --- отрисовка ---

    fn deleg_button(&self, id: &'static str, label: String, primary: bool, act: Action, l: &Look, cx: &mut Context<Self>) -> Stateful<Div> {
        crate::buttons::button(id, label, primary, l).on_click(cx.listener(move |this, _, _, cx| this.deleg_act(act, String::new(), cx)))
    }

    fn deleg_opener(&self, id: &'static str, label: String, primary: bool, edit: Edit, l: &Look, cx: &mut Context<Self>) -> Stateful<Div> {
        crate::buttons::button(id, label, primary, l).on_click(cx.listener(move |this, _, window, cx| this.deleg_open(edit, window, cx)))
    }

    /// Варианты срока формы.
    fn due_chips(&self, l: &Look, cx: &mut Context<Self>) -> Div {
        let today = day_of(self.now(), &self.tz).unwrap_or_default();
        let chips = DUE_DAYS.iter().map(|&n| {
            let on = self.deleg.due_days == Some(n);
            div()
                .id(("assign-due", n as usize))
                .h(px(26.))
                .px(px(10.))
                .flex()
                .items_center()
                .rounded(px(13.))
                .text_size(px(12.))
                .cursor_pointer()
                .when(on, |d| d.bg(l.accent()).text_color(l.on_accent()))
                .when(!on, |d| d.border_1().border_color(l.stroke()).hover(|d| d.bg(l.hover())))
                .child(due_label(n, today))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.deleg.due_days = Some(n);
                    cx.notify();
                }))
        });
        div().flex().flex_wrap().gap(px(6.)).children(chips)
    }

    /// Ввод на месте: комментарий, срок, кнопки «Отправить» / «Отмена».
    fn inline_form(&self, edit: Edit, t: &Task, l: &Look, cx: &mut Context<Self>) -> Div {
        let other = if owner_side(t, &self.me_email()) { assign::assignee(t).map(|p| p.name.clone()) } else { t.organizer().map(|p| p.name.clone()) }
            .unwrap_or_default();
        let (label, send) = match edit {
            Edit::Return => (tr!("assign-comment-return", who = other.clone()), tr!("assign-do-return")),
            Edit::Decline => (tr!("assign-comment-decline", who = other.clone()), tr!("assign-decline")),
            Edit::Propose => (tr!("assign-comment-propose", who = other.clone()), tr!("assign-send")),
            Edit::ChangeDue => (tr!("assign-due-note", who = other.clone()), tr!("assign-send")),
            Edit::Assign { .. } => (String::new(), String::new()),
        };
        let due = matches!(edit, Edit::Propose | Edit::ChangeDue);
        let comment = matches!(edit, Edit::Return | Edit::Decline | Edit::Propose);
        div()
            .mx(px(20.))
            .mt(px(12.))
            .p(px(12.))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(8.))
            .rounded(px(6.))
            .bg(l.fill_passive())
            .when(due, |d| d.child(div().text_size(px(12.)).text_color(l.text2()).child(tr!("assign-new-due"))).child(self.due_chips(l, cx)))
            .child(div().text_size(px(12.)).text_color(l.text2()).child(label))
            .when(comment, |d| d.child(Input::new(&self.deleg.comment)))
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(crate::buttons::button("assign-send", send, true, l).on_click(cx.listener(|this, _, _, cx| this.deleg_submit(cx))))
                    .child(
                        crate::buttons::button("assign-cancel", tr!("assign-cancel"), false, l)
                            .on_click(cx.listener(|this, _, _, cx| this.deleg_reset(cx))),
                    ),
            )
    }

    /// Карточка поручения (вместо карточки задачи): поручивший или исполнитель.
    pub(crate) fn deleg_pane(&self, t: &Task, st: Status, owner: bool, cx: &mut Context<Self>) -> AnyElement {
        let l = crate::roles::look(cx);
        let me = self.me_email();
        let now = self.now();
        let tz = self.tz.clone();
        let other = if owner { assign::assignee(t) } else { t.organizer() };
        let other_name = other.map(|p| if p.name.is_empty() { p.email.clone() } else { p.name.clone() }).unwrap_or_default();
        let chip = div()
            .h(px(22.))
            .px(px(8.))
            .flex()
            .items_center()
            .rounded(px(11.))
            .bg(l.pm(Pm::Secondary))
            .text_color(l.pm(Pm::SecondaryFg))
            .whitespace_nowrap()
            .child(tr!(if owner { "assign-chip-mine" } else { "assign-chip-theirs" }));
        let top = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .text_size(px(12.))
            .text_color(l.text2())
            .child(chip)
            .child(div().flex_1().min_w_0().truncate().child(t.project.clone().unwrap_or_default()));
        let title = div().text_size(px(20.)).font_weight(FontWeight::SEMIBOLD).line_height(px(26.)).child(t.title.clone());
        let person = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(crate::calendar::avatar(&other_name, 32., &l))
            .child(
                div().flex_1().min_w_0().flex().flex_col().child(div().font_weight(FontWeight::SEMIBOLD).truncate().child(other_name.clone())).child(
                    div().text_size(px(12.)).text_color(l.text2()).child(tr!(if owner { "assign-role-assignee" } else { "assign-role-owner" })),
                ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(status_circle(st, 20., &l))
                    .child(div().text_size(px(12.)).child(status_text(st, owner))),
            );
        let head = div().pt(px(16.)).px(px(20.)).flex().flex_col().gap(px(10.)).flex_none().child(top).child(title).child(person);
        let rib = div().mx(px(20.)).mt(px(14.)).flex_none().child(ribbon(t, st, owner, now, &tz, &l));
        // Поля: срок, наблюдатели.
        let field = |key: &str, value: AnyElement| {
            div()
                .h(px(30.))
                .flex()
                .items_center()
                .gap(px(10.))
                .child(div().w(px(110.)).flex_none().text_size(px(12.)).text_color(l.text2()).child(tr!(key)))
                .child(value)
        };
        let due = match &t.due {
            Some(_) => {
                let badge = (!st.is_final()).then(|| super::due_badge(t, now, &tz)).flatten();
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .min_w_0()
                    .child(div().truncate().child(super::due_text(t, &tz)))
                    .children(badge.map(|b| crate::lifecycle::chip(&b, &l)))
                    .into_any_element()
            }
            None => div().text_color(l.text3()).child(tr!("tasks-no-due")).into_any_element(),
        };
        let watchers: Vec<String> = t
            .participants
            .iter()
            .filter(|p| p.role == Role::Observer)
            .map(|p| if p.name.is_empty() { p.email.clone() } else { p.name.clone() })
            .collect();
        let watchers =
            if watchers.is_empty() { div().text_color(l.text3()).child(tr!("assign-none")) } else { div().truncate().child(watchers.join(", ")) };
        let proposed = assign::proposed(t).map(|m| moment_text(&m, &tz));
        let fields = div()
            .mx(px(20.))
            .mt(px(10.))
            .pb(px(8.))
            .flex_none()
            .flex()
            .flex_col()
            .border_b_1()
            .border_color(l.border())
            .child(field("assign-field-due", due))
            .when_some(proposed.clone(), |d, p| {
                d.child(field(
                    "assign-field-proposed",
                    div().text_color(l.pm(Pm::DeadlineFg)).font_weight(FontWeight::SEMIBOLD).child(p).into_any_element(),
                ))
            })
            .child(field("assign-field-watchers", watchers.into_any_element()));
        // Заметка последнего шага: результат, предложенный срок, причина отказа, что доработать.
        let steps = assign::steps(t);
        let note_kind = match st {
            Status::Done | Status::Closed => Some((StepKind::Finished, "assign-note-result")),
            Status::Proposed => Some((StepKind::Proposed, "assign-note-proposed")),
            Status::Declined => Some((StepKind::Declined, "assign-note-declined")),
            Status::InProgress => Some((StepKind::Returned, "assign-note-returned")),
            Status::Waiting | Status::Accepted => Some((StepKind::Assigned, "assign-note-assigned")),
            Status::Revoked => None,
        };
        let note = note_kind.and_then(|(k, head)| steps.iter().rev().find(|s| s.kind == k && !s.comment.is_empty()).map(|s| (head, s.clone()))).map(
            |(head, s)| {
                div()
                    .mx(px(20.))
                    .mt(px(12.))
                    .p(px(12.))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(l.stroke())
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(tr!(head)))
                            .child(div().text_size(px(12.)).text_color(l.text2()).child(when(s.at, now, &tz))),
                    )
                    .child(div().line_height(px(19.)).child(s.comment.clone()))
            },
        );
        // Кнопки — по статусу и стороне (макеты).
        let mut b1: Vec<Stateful<Div>> = Vec::new();
        let mut b2: Vec<Stateful<Div>> = Vec::new();
        let mut done_text: Option<String> = None;
        if owner {
            match st {
                Status::Done => {
                    b1.push(self.deleg_button("assign-accept-result", tr!("assign-accept-result"), true, Action::AcceptResult, &l, cx));
                    b1.push(self.deleg_opener("assign-return", tr!("assign-return"), false, Edit::Return, &l, cx));
                    b2.push(self.deleg_opener("assign-change-due", tr!("assign-change-due"), false, Edit::ChangeDue, &l, cx));
                    b2.push(self.deleg_button("assign-revoke", tr!("assign-revoke"), false, Action::Revoke, &l, cx));
                }
                Status::Proposed => {
                    b1.push(self.deleg_button(
                        "assign-agree",
                        tr!("assign-agree", due = proposed.clone().unwrap_or_default()),
                        true,
                        Action::AgreeDue,
                        &l,
                        cx,
                    ));
                    b1.push(self.deleg_button("assign-keep", tr!("assign-keep"), false, Action::KeepDue, &l, cx));
                    b2.push(self.deleg_button("assign-revoke", tr!("assign-revoke"), false, Action::Revoke, &l, cx));
                }
                Status::Declined => {
                    b1.push(self.deleg_opener("assign-reassign", tr!("assign-reassign"), true, Edit::Assign { reassign: true }, &l, cx));
                    b1.push(self.deleg_button("assign-revoke", tr!("assign-revoke"), false, Action::Revoke, &l, cx));
                }
                Status::Waiting | Status::Accepted | Status::InProgress => {
                    let next = assign::next_reminder(t).filter(|n| now < *n);
                    let remind = match next {
                        Some(n) => crate::buttons::secondary("assign-remind", &l)
                            .opacity(0.55)
                            .cursor_default()
                            .child(tr!("assign-reminded", time = when(n - assign::REMIND_EVERY, now, &tz))),
                        None => self.deleg_button("assign-remind", tr!("assign-remind"), false, Action::Remind, &l, cx),
                    };
                    b1.push(remind);
                    b1.push(self.deleg_opener("assign-change-due", tr!("assign-change-due"), false, Edit::ChangeDue, &l, cx));
                    b1.push(self.deleg_button("assign-revoke", tr!("assign-revoke"), false, Action::Revoke, &l, cx));
                }
                Status::Closed => done_text = Some(tr!("assign-closed")),
                Status::Revoked => done_text = Some(tr!("assign-revoked-mine")),
            }
        } else {
            match st {
                Status::Waiting => {
                    b1.push(self.deleg_button("assign-accept", tr!("assign-accept"), true, Action::Accept, &l, cx));
                    b1.push(self.deleg_opener("assign-propose", tr!("assign-propose"), false, Edit::Propose, &l, cx));
                    b1.push(self.deleg_opener("assign-decline", tr!("assign-decline"), false, Edit::Decline, &l, cx));
                }
                Status::Accepted => {
                    b1.push(self.deleg_button("assign-start", tr!("assign-start"), true, Action::Start, &l, cx));
                    b1.push(self.deleg_button("assign-finish", tr!("assign-finish"), false, Action::Finish, &l, cx));
                }
                Status::InProgress => {
                    b1.push(self.deleg_button("assign-finish", tr!("assign-finish"), true, Action::Finish, &l, cx));
                    b1.push(self.deleg_opener("assign-propose", tr!("assign-propose"), false, Edit::Propose, &l, cx));
                }
                Status::Done => {
                    done_text = Some(tr!("assign-sent-review"));
                    b1.push(self.deleg_button("assign-recall", tr!("assign-recall"), false, Action::Recall, &l, cx));
                }
                Status::Proposed => {
                    done_text = Some(tr!("assign-proposed-due", due = proposed.clone().unwrap_or_default()));
                    b1.push(self.deleg_button("assign-reopen", tr!("assign-reopen"), false, Action::Reopen, &l, cx));
                }
                Status::Declined => {
                    done_text = Some(tr!("assign-me-st-declined"));
                    b1.push(self.deleg_button("assign-reopen", tr!("assign-reopen"), false, Action::Reopen, &l, cx));
                }
                Status::Closed => done_text = Some(tr!("assign-closed-theirs")),
                Status::Revoked => done_text = Some(tr!("assign-revoked")),
            }
        }
        let form = self.deleg.edit.filter(|e| !matches!(e, Edit::Assign { .. })).map(|e| self.inline_form(e, t, &l, cx));
        let buttons = form.is_none().then(|| {
            div()
                .mx(px(20.))
                .mt(px(14.))
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(8.))
                .when_some(done_text, |d, text| {
                    d.child(div().flex().items_center().gap(px(6.)).text_color(l.text2()).child(status_circle(st, 16., &l)).child(text))
                })
                .when(!b1.is_empty(), |d| d.child(div().flex().flex_wrap().gap(px(8.)).children(b1)))
                .when(!b2.is_empty(), |d| d.child(div().flex().flex_wrap().gap(px(8.)).children(b2)))
        });
        // История: новые сверху.
        let history = div()
            .mx(px(20.))
            .mt(px(14.))
            .pt(px(10.))
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(6.))
            .overflow_hidden()
            .border_t_1()
            .border_color(l.border())
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(tr!("assign-history")))
                    .child(div().text_color(l.text2()).child(steps.len().to_string())),
            )
            .children(steps.iter().rev().enumerate().map(|(i, s)| {
                div()
                    .flex()
                    .gap(px(10.))
                    .text_size(px(12.))
                    .child(div().w(px(76.)).flex_none().text_color(l.text3()).child(when(s.at, now, &tz)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .when(i == 0, |d| d.text_color(l.text()))
                            .when(i > 0, |d| d.text_color(l.text2()))
                            .child(step_text(t, s, &me, &tz)),
                    )
            }));
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(head)
            .child(rib)
            .child(fields)
            .children(note)
            .children(form)
            .children(buttons)
            .child(history)
            .into_any_element()
    }

    /// Форма «Поручить»: исполнитель из справочника, наблюдатели, срок, комментарий.
    pub(crate) fn assign_form(&self, t: &Task, reassign: bool, cx: &mut Context<Self>) -> AnyElement {
        let l = crate::roles::look(cx);
        let label = |key: &str| div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(l.text2()).child(tr!(key));
        let picked = self.deleg.to.clone();
        let person: AnyElement = match &picked {
            Some(a) => div()
                .flex()
                .items_center()
                .gap(px(10.))
                .p(px(8.))
                .rounded(px(6.))
                .border_1()
                .border_color(l.stroke())
                .child(crate::calendar::avatar(if a.name.is_empty() { &a.email } else { &a.name }, 28., &l))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().font_weight(FontWeight::SEMIBOLD).truncate().child(if a.name.is_empty() {
                            a.email.clone()
                        } else {
                            a.name.clone()
                        }))
                        .child(div().text_size(px(12.)).text_color(l.text2()).truncate().child(a.email.clone())),
                )
                .child(
                    div()
                        .id("assign-unpick")
                        .size(px(24.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .cursor_pointer()
                        .hover(|d| d.bg(l.hover()))
                        .child(icon(g::CANCEL, 10.))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.deleg.to = None;
                            cx.notify();
                        })),
                )
                .into_any_element(),
            None => div().child(Input::new(&self.deleg.person)).into_any_element(),
        };
        let suggest = |watcher: bool, cx: &mut Context<Self>| -> Option<Div> {
            (self.deleg.suggest_watcher == watcher && !self.deleg.suggest.is_empty()).then(|| {
                div().flex().flex_col().rounded(px(6.)).border_1().border_color(l.stroke()).bg(l.c(pets_ui::theme::Role::LayerOverlay)).children(
                    self.deleg.suggest.iter().enumerate().map(|(i, a)| {
                        let pick = a.clone();
                        div()
                            .id(("assign-suggest", i))
                            .h(px(36.))
                            .px(px(10.))
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .cursor_pointer()
                            .when(i == 0, |d| d.bg(l.hover()))
                            .hover(|d| d.bg(l.hover()))
                            .child(crate::calendar::avatar(if a.name.is_empty() { &a.email } else { &a.name }, 24., &l))
                            .child(div().flex_1().min_w_0().truncate().child(a.display()))
                            .on_click(cx.listener(move |this, _, _, cx| this.deleg_pick(watcher, pick.clone(), cx)))
                    }),
                )
            })
        };
        let watchers = div().flex().flex_wrap().gap(px(6.)).children(self.deleg.watchers.iter().enumerate().map(|(i, w)| {
            let email = w.email.clone();
            div()
                .id(("assign-watcher", i))
                .h(px(24.))
                .pl(px(8.))
                .pr(px(4.))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded(px(12.))
                .bg(l.fill())
                .child(if w.name.is_empty() { w.email.clone() } else { w.name.clone() })
                .child(div().id(("assign-watcher-x", i)).cursor_pointer().child(icon(g::CANCEL, 8.)).on_click(cx.listener(move |this, _, _, cx| {
                    this.deleg.watchers.retain(|x| x.email != email);
                    cx.notify();
                })))
        }));
        let note = match &picked {
            Some(a) => tr!("assign-form-note", who = if a.name.is_empty() { a.email.clone() } else { a.name.clone() }),
            None => tr!("assign-form-note-empty"),
        };
        let send = crate::buttons::button("assign-submit", tr!("assign-delegate"), true, &l)
            .when(picked.is_none(), |d| d.opacity(0.45))
            .on_click(cx.listener(|this, _, _, cx| this.deleg_submit(cx)));
        let cancel =
            crate::buttons::button("assign-cancel", tr!("assign-cancel"), false, &l).on_click(cx.listener(|this, _, _, cx| this.deleg_reset(cx)));
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(12.))
            .pt(px(16.))
            .px(px(20.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(12.))
                    .text_color(l.text2())
                    .child(icon(g::DELEGATE, 14.))
                    .child(tr!(if reassign { "assign-reassign" } else { "assign-form-title" }))
                    .child(div().flex_1().min_w_0().truncate().child(t.project.clone().unwrap_or_default())),
            )
            .child(div().text_size(px(20.)).font_weight(FontWeight::SEMIBOLD).line_height(px(26.)).child(t.title.clone()))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(label("assign-form-person"))
                    .child(person)
                    .children(suggest(false, cx))
                    .when(picked.is_none(), |d| d.child(div().text_size(px(11.)).text_color(l.text3()).child(tr!("assign-form-person-hint")))),
            )
            .when(!reassign, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(
                            div()
                                .flex()
                                .gap(px(6.))
                                .child(label("assign-form-watchers"))
                                .child(div().text_size(px(12.)).text_color(l.text3()).child(tr!("assign-form-watchers-opt"))),
                        )
                        .child(watchers)
                        .child(Input::new(&self.deleg.watcher))
                        .children(suggest(true, cx)),
                )
            })
            .child(div().flex().flex_col().gap(px(6.)).child(label("assign-form-due")).child(self.due_chips(&l, cx)))
            .child(div().flex().flex_col().gap(px(6.)).child(label("assign-form-comment")).child(Input::new(&self.deleg.comment)))
            .child(div().flex().items_center().gap(px(8.)).child(send).child(cancel))
            .child(div().text_size(px(12.)).text_color(l.text2()).child(note))
            .into_any_element()
    }
}

/// Тема письма шага.
fn letter_subject(kind: StepKind, title: &str) -> String {
    tr!(&format!("assign-mail-subject-{}", kind_id(kind)), title = title.to_owned())
}

/// Текст письма шага (Markdown → текст и HTML): что случилось, срок, комментарий — читаемо без Postman (решение 3).
fn letter_body(t: &Task, kind: StepKind, me: &Address, tz: &TimeZone) -> String {
    let name = if me.name.is_empty() { me.email.clone() } else { me.name.clone() };
    let mut out = vec![tr!(&format!("assign-mail-body-{}", kind_id(kind)), who = name.clone(), title = t.title.clone())];
    if let Some(d) = &t.due {
        out.push(tr!("assign-mail-due", due = moment_text(d, tz)));
    }
    if let Some(s) = assign::steps(t).into_iter().rev().find(|s| s.kind == kind && !s.comment.is_empty()) {
        out.push(tr!("assign-mail-comment", text = s.comment));
    }
    if !t.notes.trim().is_empty() && kind == StepKind::Assigned {
        out.push(t.notes.clone());
    }
    out.push(tr!("assign-mail-footer", who = name));
    out.join("\n\n")
}

/// Карточка письма-поручения над текстом письма: что, от кого, срок, комментарий, статус; главное действие и
/// «Открыть в задачах». `task` — своя копия (если уже пришла), `me` — мой адрес.
pub(crate) fn mail_card(
    inc: &Incoming,
    task: Option<&Task>,
    me: &str,
    tz: &TimeZone,
    cx: &mut App,
    on: impl Fn(Option<Action>, &mut Window, &mut App) + Clone + 'static,
) -> AnyElement {
    let l = crate::roles::look(cx);
    let shown = task.unwrap_or(&inc.task);
    let owner = owner_side(shown, me);
    let st = assign::status(shown);
    let from = inc.sender().map(|p| if p.name.is_empty() { p.email.clone() } else { p.name.clone() }).unwrap_or_default();
    let head_key = match inc.method.as_str() {
        "REPLY" => "assign-mail-card-reply",
        "COUNTER" => "assign-mail-card-counter",
        "CANCEL" => "assign-mail-card-cancel",
        "DECLINECOUNTER" => "assign-mail-card-declinecounter",
        _ => "assign-mail-card-request",
    };
    let due = shown.due.as_ref().map(|d| moment_text(d, tz));
    let main = st.and_then(|s| primary(s, owner)).filter(|_| task.is_some());
    let main_label = main.map(|a| match a {
        Action::Accept => tr!("assign-accept"),
        Action::Start => tr!("assign-start"),
        Action::Finish => tr!("assign-finish"),
        Action::AcceptResult => tr!("assign-accept-result"),
        Action::AgreeDue => tr!("assign-agree", due = assign::proposed(shown).map(|m| moment_text(&m, tz)).unwrap_or_default()),
        _ => String::new(),
    });
    let on_main = on.clone();
    let on_open = on;
    div()
        .mx(px(20.))
        .mt(px(12.))
        .p(px(14.))
        .flex()
        .flex_col()
        .gap(px(8.))
        .rounded(px(8.))
        .border_1()
        .border_color(l.stroke())
        .bg(l.panel())
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .text_size(px(12.))
                .text_color(l.text2())
                .child(icon(g::DELEGATE, 14.))
                .child(div().font_weight(FontWeight::SEMIBOLD).child(tr!(head_key)))
                .child(div().flex_1())
                .children(st.map(|s| div().flex().items_center().gap(px(6.)).child(status_circle(s, 18., &l)).child(status_text(s, owner)))),
        )
        .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child(inc.task.title.clone()))
        .child(
            div()
                .flex()
                .gap(px(16.))
                .text_size(px(12.))
                .child(div().text_color(l.text2()).child(tr!("assign-mail-from", who = from)))
                .children(due.map(|d| div().child(tr!("assign-mail-due", due = d)))),
        )
        .when(!inc.comment.is_empty(), |d| {
            d.child(div().p(px(8.)).rounded(px(6.)).bg(l.fill_passive()).line_height(px(19.)).child(inc.comment.clone()))
        })
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(8.))
                .when_some(main.zip(main_label), |d, (a, label)| {
                    d.child(crate::buttons::button("mail-assign-main", label, true, &l).on_click(move |_, window, cx| on_main(Some(a), window, cx)))
                })
                .child(
                    crate::buttons::button("mail-assign-open", tr!("assign-open-tasks"), main.is_none(), &l)
                        .on_click(move |_, window, cx| on_open(None, window, cx)),
                )
                .when(task.is_none(), |d| d.child(div().text_size(px(12.)).text_color(l.text3()).self_center().child(tr!("assign-not-yet")))),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{Action, DUE_DAYS, Status, due_at, primary};
    use postman_calendar::Moment;

    /// Space — главное действие стороны; у остальных статусов его нет.
    #[test]
    fn primary_action_by_side() {
        assert_eq!(primary(Status::Done, true), Some(Action::AcceptResult));
        assert_eq!(primary(Status::Proposed, true), Some(Action::AgreeDue));
        assert_eq!(primary(Status::Waiting, true), None);
        assert_eq!(primary(Status::Waiting, false), Some(Action::Accept));
        assert_eq!(primary(Status::InProgress, false), Some(Action::Finish));
        assert_eq!(primary(Status::Closed, false), None);
        // Каждое главное действие допустимо по таблице.
        for owner in [true, false] {
            for st in [
                Status::Waiting,
                Status::Proposed,
                Status::Accepted,
                Status::InProgress,
                Status::Done,
                Status::Declined,
                Status::Closed,
                Status::Revoked,
            ] {
                if let Some(a) = primary(st, owner) {
                    assert!(postman_calendar::assign::next(st, a).is_ok(), "{st:?} {a:?}");
                    assert_eq!(a.by_owner(), owner);
                }
            }
        }
    }

    /// Срок формы: время прежнего срока, без него — 18:00; пояс — прежний.
    #[test]
    fn form_due_keeps_time_and_zone() {
        let today = jiff::civil::date(2026, 10, 20);
        let old = Moment::new("2026-10-21T12:30:00".parse().unwrap(), Some("Europe/Moscow"));
        let d = due_at(3, Some(&old), today);
        assert_eq!(d.local, "2026-10-23T12:30:00".parse().unwrap());
        assert_eq!(d.tz.as_deref(), Some("Europe/Moscow"));
        assert_eq!(due_at(0, None, today).local, "2026-10-20T18:00:00".parse().unwrap());
        assert_eq!(DUE_DAYS[0], 0);
    }
}
