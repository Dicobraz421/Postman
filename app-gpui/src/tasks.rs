//! Модуль «Задачи» (S6-05, Ctrl+4; макет `Tasks.dc.html`, DESIGN «Задачи (Ctrl+4)»): слева — списки (Входящие,
//! Сегодня, Предстоящие, Из почты, Когда-нибудь, Выполненные), проекты с прогрессом и категории в цветах почты;
//! посередине — список с группами («Просрочено» сверху), строкой «Добавить задачу»; справа — карточка задачи.
//! Командная строка раздела — [`TasksBar`]: «Новая задача», «Выполнить», «Срок», «Изменить», «Удалить», поиск.
//!
//! Модель — `postman_core::tasks` (списки, проекты, подзадачи) и `postman_core::mailtask` (письма со сроком —
//! задачи «Из почты»). Выполнение — зачёркивание, через 2 с задача уходит в «Выполненные» ([`Completing`]), тост
//! «Отменить». Правка пишется в кэш (`tasks::save_local`) и уходит на сервер (`tasks::Request::Flush`); у письма —
//! слова срока (`Command::Patch`, как сроки S5-04).
//!
//! Демо: задачи из [`demo`] — только в памяти окна (на сервер ничего не уходит). Живой ящик: задачи — событиями
//! синхронизации (`Event::Tasks`).
//!
//! Своя кэшируемая view: перерисовывается только по своему `notify`. С экраном говорит событиями [`TasksAction`].

use std::time::{Duration, Instant};

use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputState, TextDecoration, TextDecorationCollection};
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::prelude::*;
use gpui_kit::*;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use pets_i18n::tr;
use pets_ui::Level as Toast;
use pets_ui::theme::Metric;
use postman_cache::{Cache, EmailRow};
use postman_calendar::{Alert, Anchor, Busy, Moment, Progress, Span, Task, TimeBlock, Trigger, WorkHours};
use postman_core::assign::{self as flow, Bucket, Tab};
use postman_core::labels::Categories;
use postman_core::lifecycle::{self as cycle, Badge, Patch, Pick};
use postman_core::mailtask;
use postman_core::marks::{self, Kind, When};
use postman_core::recur::{self, Freq, Rule, Snooze};
use postman_core::tasks::{self as model, ChipKind, Hours, Lang, List, Parsed, Reply, Request};

use crate::chrome::icon;
use crate::roles::Look;

mod delegation;
pub(crate) use delegation::mail_card;
use delegation::Edit;

const NAV_W: f32 = 236.;
const CARD_W: f32 = 440.;
const ROW_H: f32 = 44.;
const ROW_TWO: f32 = 54.;
const HEAD_H: f32 = 30.;
/// Выполненная задача остаётся на месте зачёркнутой столько, потом уходит в «Выполненные» (DESIGN).
pub(crate) const DONE_DELAY: Duration = Duration::from_secs(2);

/// Значки Segoe Fluent Icons модуля.
pub(crate) mod g {
    pub(crate) const TASKS: char = '\u{E9D5}';
    pub(crate) const INBOX: char = '\u{E8B7}';
    pub(crate) const TODAY: char = '\u{E8D1}';
    pub(crate) const UPCOMING: char = '\u{E787}';
    pub(crate) const FLAG: char = '\u{E7C1}';
    pub(crate) const SOMEDAY: char = '\u{E7B8}';
    pub(crate) const DONE: char = '\u{E930}';
    pub(crate) const CHECK: char = '\u{E73E}';
    pub(crate) const ADD: char = '\u{E710}';
    pub(crate) const MAIL: char = '\u{E715}';
    pub(crate) const OPEN: char = '\u{E8A7}';
    pub(crate) const RINGER: char = '\u{EA8F}';
    pub(crate) const REPEAT: char = '\u{E8EE}';
    pub(crate) const FOLDER: char = '\u{E8B7}';
    pub(crate) const TAG: char = '\u{E8EC}';
    pub(crate) const SUBTASKS: char = '\u{E762}';
    pub(crate) const NOTES: char = '\u{E70B}';
    pub(crate) const EDIT: char = '\u{E70F}';
    pub(crate) const DELETE: char = '\u{E74D}';
    pub(crate) const SEARCH: char = '\u{E721}';
    /// Поручения (S6-10): «Поручить», «Забрать», «в работе», «отклонил».
    pub(crate) const DELEGATE: char = '\u{E8FA}';
    pub(crate) const REVOKE: char = '\u{E7A7}';
    pub(crate) const PLAY: char = '\u{E768}';
    pub(crate) const CANCEL: char = '\u{E711}';
}

// ---------------------------------------------------------------------------------------------------------------
// Модель экрана без gpui: что показывать слева, группы списка, выполнение с задержкой.

/// Что открыто слева.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Nav {
    List(List),
    /// Проект по имени.
    Project(String),
    /// Категория по ключевому слову.
    Category(String),
    /// «Поручения» (S6-11): вкладка «Я поручил» | «Мне поручили».
    Delegations(Tab),
}

/// Группа списка (заголовок над задачами).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Group {
    /// Без заголовка (Входящие, Когда-нибудь).
    Plain,
    Overdue,
    /// Сегодня ко времени.
    TodayTimed,
    /// Сегодня без времени.
    NoTime,
    Today,
    Tomorrow,
    /// День недели этой недели.
    Day(Date),
    /// В ближайшую неделю.
    Soon,
    Later,
    NoDue,
    DoneToday,
    DoneYesterday,
    DoneEarlier,
    /// Группа раздела «Поручения».
    Deleg(Bucket),
}

/// Поле карточки с вариантами (S6-06).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Field {
    Remind,
    Repeat,
    Project,
    Category,
}

/// Напоминание: за столько секунд до срока (0 — в срок).
const REMINDERS: [i64; 4] = [0, -15 * 60, -3600, -86_400];

/// Строка списка.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Item {
    Head(Group, usize),
    /// Индекс в [`TasksView::shown`].
    Row(usize),
}

fn day_of(ts: i64, tz: &TimeZone) -> Option<Date> {
    jiff::Timestamp::from_second(ts).ok().map(|t| t.to_zoned(tz.clone()).date())
}

fn add_days(d: Date, n: i64) -> Date {
    d.checked_add(jiff::Span::new().days(n)).unwrap_or(d)
}

/// Задачи раздела `nav` по группам: в «Сегодня» — просроченные сверху, затем ко времени и без времени; в
/// «Предстоящих» — по дням недели и «Позже»; в «Выполненных» — сегодня, вчера, ранее; в проекте, категории и
/// «Из почты» — просрочено, сегодня, скоро, позже, без срока. `pending` — выполняемые сейчас: остаются на месте.
pub(crate) fn groups(shown: &[Task], nav: &Nav, pending: &[String], now: i64, tz: &TimeZone) -> Vec<Item> {
    let today = day_of(now, tz).unwrap_or_default();
    let due_day = |t: &Task| t.due.as_ref().and_then(|d| d.date_in(tz));
    let overdue = |t: &Task| model::is_overdue(t, now, tz) && !pending.contains(&t.uid);
    let mut out: Vec<Item> = Vec::new();
    let mut push = |g: Group, idx: Vec<usize>| {
        if !idx.is_empty() {
            if g != Group::Plain {
                out.push(Item::Head(g, idx.len()));
            }
            out.extend(idx.into_iter().map(Item::Row));
        }
    };
    let all: Vec<usize> = (0..shown.len()).collect();
    let pick = |f: &dyn Fn(&Task) -> bool| all.iter().copied().filter(|&i| f(&shown[i])).collect::<Vec<_>>();
    match nav {
        Nav::List(List::Inbox) | Nav::List(List::Someday) => push(Group::Plain, all.clone()),
        Nav::List(List::Today) => {
            push(Group::Overdue, pick(&|t| overdue(t)));
            push(Group::TodayTimed, pick(&|t| !overdue(t) && !t.all_day && t.due.is_some()));
            push(Group::NoTime, pick(&|t| !overdue(t) && (t.all_day || t.due.is_none())));
        }
        Nav::List(List::Upcoming) => {
            let mut days: Vec<Date> = shown.iter().filter_map(due_day).filter(|d| *d < add_days(today, 7)).collect();
            days.sort();
            days.dedup();
            for d in days {
                let g = if d == add_days(today, 1) { Group::Tomorrow } else { Group::Day(d) };
                push(g, pick(&|t| due_day(t) == Some(d)));
            }
            push(Group::Later, pick(&|t| due_day(t).is_some_and(|d| d >= add_days(today, 7))));
        }
        Nav::Delegations(tab) => {
            for &b in flow::buckets(*tab) {
                push(Group::Deleg(b), pick(&|t| postman_calendar::assign::status(t).and_then(|s| flow::bucket(s, *tab)) == Some(b)));
            }
        }
        Nav::List(List::Done) => {
            let done_day = |t: &Task| t.completed_at.and_then(|c| day_of(c, tz));
            push(Group::DoneToday, pick(&|t| done_day(t) == Some(today)));
            push(Group::DoneYesterday, pick(&|t| done_day(t) == Some(add_days(today, -1))));
            push(Group::DoneEarlier, pick(&|t| done_day(t).is_none_or(|d| d < add_days(today, -1))));
        }
        _ => {
            push(Group::Overdue, pick(&|t| overdue(t)));
            push(Group::Today, pick(&|t| !overdue(t) && due_day(t) == Some(today)));
            push(Group::Soon, pick(&|t| !overdue(t) && due_day(t).is_some_and(|d| d > today && d < add_days(today, 7))));
            push(Group::Later, pick(&|t| due_day(t).is_some_and(|d| d >= add_days(today, 7))));
            push(Group::NoDue, pick(&|t| t.due.is_none()));
        }
    }
    out
}

/// Задачи раздела по порядку (до групп): списки — `tasks::list`, «Из почты» — с письмами, проект и категория —
/// открытые этого проекта или с этой категорией. `pending` — выполняемые сейчас: остаются в своём списке.
pub(crate) fn section(tasks: &[Task], letters: &[EmailRow], nav: &Nav, pending: &[String], now: i64, tz: &TimeZone) -> Vec<Task> {
    // Выполняемая сейчас — ещё как открытая.
    let open_copy = |t: &Task| {
        let mut t = t.clone();
        if pending.contains(&t.uid) {
            t.progress = Progress::NeedsAction;
        }
        t
    };
    let view: Vec<Task> = tasks.iter().map(open_copy).collect();
    let by_due = |a: &Task, b: &Task| {
        a.due_at(tz).unwrap_or(i64::MAX).cmp(&b.due_at(tz).unwrap_or(i64::MAX)).then_with(|| b.important.cmp(&a.important)).then_with(|| a.title.cmp(&b.title))
    };
    match nav {
        Nav::List(List::FromMail) => mailtask::from_mail(&view, letters, tz),
        // «Поручения» — по моему адресу и «Только свои» (`TasksView::reshow`).
        Nav::Delegations(_) => Vec::new(),
        Nav::List(l) => model::list(&view, *l, now, tz).into_iter().cloned().collect(),
        Nav::Project(p) => {
            let mut v: Vec<Task> = view
                .into_iter()
                .filter(|t| t.parent.is_none() && !t.progress.is_closed() && t.project.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(p)))
                .collect();
            v.sort_by(by_due);
            v
        }
        Nav::Category(k) => {
            let mut v: Vec<Task> = view
                .into_iter()
                .filter(|t| t.parent.is_none() && !t.progress.is_closed() && t.keywords.iter().any(|x| x.eq_ignore_ascii_case(k)))
                .collect();
            v.sort_by(by_due);
            v
        }
    }
}

/// Выполнение с задержкой: задача зачёркнута на месте [`DONE_DELAY`], затем уходит; «Отменить» до срока — ничего не
/// было.
#[derive(Debug, Default, Clone)]
pub(crate) struct Completing {
    items: Vec<(String, Instant)>,
}

impl Completing {
    pub(crate) fn start(&mut self, uid: &str, now: Instant) {
        self.items.retain(|(u, _)| u != uid);
        self.items.push((uid.to_owned(), now));
    }

    /// Отменить; `true` — задача ещё ждала (выполнение не случилось).
    pub(crate) fn cancel(&mut self, uid: &str) -> bool {
        let n = self.items.len();
        self.items.retain(|(u, _)| u != uid);
        n != self.items.len()
    }

    pub(crate) fn is_pending(&self, uid: &str) -> bool {
        self.items.iter().any(|(u, _)| u == uid)
    }

    pub(crate) fn uids(&self) -> Vec<String> {
        self.items.iter().map(|(u, _)| u.clone()).collect()
    }

    /// Срок вышел — эти выполнить (и забыть).
    pub(crate) fn due(&mut self, now: Instant) -> Vec<String> {
        let (ready, wait): (Vec<_>, Vec<_>) = self.items.drain(..).partition(|(_, at)| now.duration_since(*at) >= DONE_DELAY);
        self.items = wait;
        ready.into_iter().map(|(u, _)| u).collect()
    }

    /// Через сколько срок у ближайшей.
    pub(crate) fn next_in(&self, now: Instant) -> Option<Duration> {
        self.items.iter().map(|(_, at)| DONE_DELAY.saturating_sub(now.duration_since(*at))).min()
    }
}

// ---------------------------------------------------------------------------------------------------------------

/// Просьба раздела к экрану.
#[derive(Clone, Debug)]
pub(crate) enum TasksAction {
    /// Команда синхронизации задач.
    Send(Request),
    /// Слова срока писем (задачи «Из почты»).
    Patch(Vec<Patch>),
    /// Открыть письмо по ключу кэша.
    OpenLetter(String),
    /// Сработало напоминание (S6-07): тост с «Отложить», при свёрнутом окне — уведомление Windows.
    Remind { uid: String, title: String },
    Toast(Toast, String),
    /// Тост с «Отменить»: что вернуть.
    Undo { text: String, undo: Undo },
    /// Письма шага поручения (S6-10): своя копия уже записана — `Command::Delegate`; `hold` — сервер держит их окно
    /// «Отменить» (S6-11: «Забрать», «Удалить поручение»).
    Delegate { uid: String, drafts: Vec<postman_compose::Draft>, hold: bool },
    /// «Отменить» шага: письма, которые сервер держит, не отправлять (`Command::Withhold`).
    Withhold(Vec<postman_core::send_later::Scheduled>),
    /// Слова писем моих невыполненных поручений изменились — «Ждём» в почте (S6-11).
    Waiting(Vec<String>),
    /// «Открыть в почте»: почта с быстрым фильтром «Ждём».
    OpenWaiting,
    /// Форме «Поручить» нужна адресная книга окна письма (подсказки исполнителя).
    NeedBook,
}

/// Что вернуть по «Отменить».
#[derive(Clone, Debug)]
pub(crate) enum Undo {
    /// Задачи, как были (выполнение или удаление).
    Tasks(Vec<Task>),
    /// Слова срока писем.
    Letters(Vec<Patch>),
    /// Поручение до «Забрать» или «Удалить» (S6-11): копия — как была, письма отмены — не отправлять.
    Delegation(Box<Task>),
}

pub(crate) struct TasksView {
    live: bool,
    cache: Cache,
    pub(crate) tasks: Vec<Task>,
    /// Письма со сроком (свой аккаунт): задачи «Из почты».
    letters: Vec<EmailRow>,
    /// Письма задач со ссылкой (без срока у письма) — из кэша по выбору задачи: блок письма в карточке.
    linked: Vec<EmailRow>,
    pub(crate) nav: Nav,
    shown: Vec<Task>,
    items: Vec<Item>,
    list: ListState,
    pub(crate) selected: Option<String>,
    completing: Completing,
    categories: Categories,
    pub(crate) search: Entity<InputState>,
    query: String,
    /// «Добавить задачу» над списком.
    pub(crate) quick: Entity<InputState>,
    /// Новая задача (Ctrl+N): карточка с полем названия, ещё не записана.
    draft: Option<Task>,
    /// Правка названия (F2) — поле вместо заголовка карточки.
    title: Option<Entity<InputState>>,
    /// Варианты срока в карточке открыты (Insert).
    pub(crate) due_open: bool,
    /// Открытые варианты поля карточки (S6-06): напоминание, проект, категория.
    pub(crate) open: Option<Field>,
    /// Новый проект — строкой в вариантах проекта.
    project_input: Entity<InputState>,
    /// «Добавить подзадачу»: Enter — подзадача, поле остаётся для следующей.
    sub_input: Entity<InputState>,
    /// Заметки — поле с живым отображением Markdown (как письмо, S5-27).
    notes: Entity<EditorState>,
    notes_marks: Option<TextDecorationCollection>,
    /// Чьи заметки в поле; сохранение — с задержкой после набора.
    notes_uid: Option<String>,
    notes_loading: bool,
    notes_save: Option<gpui_kit::Task<()>>,
    /// Шрифт интерфейса для поля заметок (по умолчанию у поля — моноширинный).
    font: SharedString,
    /// Разбор строки «Добавить задачу» (S6-04) и снятые крестиком чипы.
    pub(crate) parsed: Parsed,
    ignored: Vec<String>,
    /// «Время в календаре» (S6-08): занятость сегодня из своего календаря и выбранное окно.
    busy: Vec<Busy>,
    pub(crate) slot: usize,
    /// Напоминания (S6-07): проверено до этого момента, отложенные, таймер к ближайшему.
    remind_since: i64,
    snoozed: Vec<(String, i64)>,
    remind_timer: Option<gpui_kit::Task<()>>,
    tz: TimeZone,
    /// Задачи пришли (снимки ждут их).
    pub(crate) loaded: bool,
    timer: Option<gpui_kit::Task<()>>,
    /// Мой адрес: сторона в поручении, «От» писем шага. Демо — владелец демо-ящика.
    me: Option<postman_compose::Address>,
    /// Адресная книга окна письма: исполнитель и наблюдатели из справочника и контактов.
    book: Option<crate::compose::Book>,
    /// Формы поручения (S6-10).
    deleg: delegation::State,
    /// «Поручения» (S6-11): последняя вкладка и «Только свои».
    tab: Tab,
    pub(crate) own_only: bool,
    /// Слова «Ждём», отданные почте последними.
    waiting_sent: Vec<String>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<TasksAction> for TasksView {}

impl TasksView {
    pub(crate) fn new(cache: Cache, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("tasks-search")));
        let quick = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("tasks-quick-placeholder")));
        let on_search = cx.subscribe(&search, |this: &mut Self, state, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                this.query = state.read(cx).value().to_string();
                this.reshow(cx);
            }
        });
        let on_quick = cx.subscribe_in(&quick, window, |this: &mut Self, state, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { .. } => {
                let text = state.read(cx).value().to_string();
                if this.quick_add(&text, cx) {
                    state.update(cx, |s, cx| s.set_value("", window, cx));
                    this.ignored.clear();
                    this.parsed = Parsed::default();
                }
            }
            InputEvent::Change => {
                let text = state.read(cx).value().to_string();
                if text.trim().is_empty() {
                    this.ignored.clear();
                }
                this.parsed = this.parse(&text, cx);
                cx.notify();
            }
            _ => {}
        });
        let project_input = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("tasks-project-new")));
        let on_project = cx.subscribe_in(&project_input, window, |this: &mut Self, state, ev: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                let name = state.read(cx).value().trim().to_string();
                if !name.is_empty() {
                    this.set_project(Some(name), cx);
                    state.update(cx, |s, cx| s.set_value("", window, cx));
                }
            }
        });
        let sub_input = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("tasks-sub-add")));
        let on_sub = cx.subscribe_in(&sub_input, window, |this: &mut Self, state, ev: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                let title = state.read(cx).value().trim().to_string();
                if !title.is_empty() {
                    this.add_subtask(title, cx);
                    state.update(cx, |s, cx| s.set_value("", window, cx));
                }
            }
        });
        let notes = cx.new(|cx| {
            EditorState::new(window, cx)
                .line_number(false)
                .folding(false)
                .indent_guides(false)
                .searchable(false)
                .auto_close(false)
                .soft_wrap(true)
                .placeholder(tr!("tasks-notes-placeholder"))
        });
        let on_notes = cx.subscribe(&notes, |this: &mut Self, _, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev
                && !this.notes_loading
            {
                this.paint_notes(cx);
                this.schedule_notes(cx);
            }
        });
        let (deleg, deleg_subs) = delegation::State::new(window, cx);
        let mut subscriptions = vec![on_search, on_quick, on_project, on_sub, on_notes];
        subscriptions.extend(deleg_subs);
        let mut this = Self {
            live: false,
            cache,
            tasks: demo(jiff::Timestamp::now().as_second(), &crate::prefs::get(cx).calendar.zone()),
            letters: Vec::new(),
            linked: Vec::new(),
            nav: Nav::List(List::Today),
            shown: Vec::new(),
            items: Vec::new(),
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            selected: None,
            completing: Completing::default(),
            categories: crate::labels::default_table(),
            search,
            query: String::new(),
            quick,
            draft: None,
            title: None,
            due_open: false,
            open: None,
            project_input,
            sub_input,
            notes,
            notes_marks: None,
            notes_uid: None,
            notes_loading: false,
            notes_save: None,
            font: SharedString::default(),
            parsed: Parsed::default(),
            ignored: Vec::new(),
            busy: Vec::new(),
            slot: 0,
            // Напоминания, сработавшие до запуска, не показываются (кроме последней минуты).
            remind_since: jiff::Timestamp::now().as_second() - 60,
            snoozed: Vec::new(),
            remind_timer: None,
            tz: crate::prefs::get(cx).calendar.zone(),
            loaded: true,
            timer: None,
            me: Some(postman_compose::Address::new(postman_demo::ME_NAME, postman_demo::ME)),
            book: None,
            deleg,
            tab: Tab::ByMe,
            own_only: true,
            waiting_sent: Vec::new(),
            _subscriptions: subscriptions,
        };
        this.load_letters(cx);
        this.reshow(cx);
        this
    }

    /// Живой ящик: задачи — с сервера (поток синхронизации присылает их сам после входа).
    pub(crate) fn go_live(&mut self, cx: &mut Context<Self>) {
        self.live = true;
        self.me = None;
        self.tasks.clear();
        self.loaded = false;
        self.selected = None;
        self.reshow(cx);
    }

    pub(crate) fn set_categories(&mut self, table: Categories, cx: &mut Context<Self>) {
        self.categories = table;
        cx.notify();
    }

    /// Раздел открыт: письма со сроком — заново из кэша.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.load_letters(cx);
    }

    fn load_letters(&mut self, cx: &mut Context<Self>) {
        let cache = self.cache.clone();
        let task = cx.background_spawn(async move { mailtask::letters(&cache) });
        cx.spawn(async move |this, cx| {
            let rows = task.await;
            let _ = this.update(cx, |v, cx| match rows {
                Ok(rows) => {
                    v.letters = rows;
                    v.reshow(cx);
                }
                Err(e) => tracing::warn!("tasks: letters with due: {e}"),
            });
        })
        .detach();
    }

    pub(crate) fn on_reply(&mut self, reply: &Reply, cx: &mut Context<Self>) {
        match reply {
            Reply::Loaded(all) => {
                self.tasks = all.clone();
                self.loaded = true;
            }
            Reply::Changed(list) => {
                for t in list {
                    match self.tasks.iter_mut().find(|x| x.uid == t.uid) {
                        Some(x) => *x = t.clone(),
                        None => self.tasks.push(t.clone()),
                    }
                }
            }
            Reply::Removed(uids) => self.tasks.retain(|t| !uids.contains(&t.uid)),
            Reply::Failed { error, .. } => cx.emit(TasksAction::Toast(Toast::Error, tr!("tasks-failed", error = error.clone()))),
        }
        self.reshow(cx);
    }

    /// Письма в кэше изменились: задачи «Из почты» — заново.
    pub(crate) fn letters_changed(&mut self, cx: &mut Context<Self>) {
        self.load_letters(cx);
    }

    fn now(&self) -> i64 {
        jiff::Timestamp::now().as_second()
    }

    fn reshow(&mut self, cx: &mut Context<Self>) {
        let pending = self.completing.uids();
        let now = self.now();
        let mut shown = match &self.nav {
            Nav::Delegations(tab) => flow::section(&self.tasks, &self.me_email(), *tab, self.own_only, &self.tz)
                .into_iter()
                .flat_map(|(_, v)| v.into_iter().cloned())
                .collect(),
            nav => section(&self.tasks, &self.letters, nav, &pending, now, &self.tz),
        };
        let waiting = self.waiting_keywords();
        if waiting != self.waiting_sent {
            self.waiting_sent = waiting.clone();
            cx.emit(TasksAction::Waiting(waiting));
        }
        let q = self.query.trim().to_lowercase();
        if !q.is_empty() {
            shown.retain(|t| t.title.to_lowercase().contains(&q) || t.notes.to_lowercase().contains(&q));
        }
        self.items = groups(&shown, &self.nav, &pending, now, &self.tz);
        self.shown = shown;
        let drafting = self.draft.as_ref().is_some_and(|d| self.selected.as_ref() == Some(&d.uid));
        if !drafting && self.selected.as_ref().is_none_or(|s| !self.shown.iter().any(|t| &t.uid == s)) {
            self.selected = self.shown.first().map(|t| t.uid.clone());
            self.due_open = false;
            self.title = None;
        }
        self.list.reset(self.items.len());
        self.remind_pass(cx);
        cx.notify();
    }

    /// Напоминания: сработавшие с прошлой проверки — экрану (тост, уведомление); один таймер к ближайшему (без кадров
    /// между сроками, как у сроков писем).
    fn remind_pass(&mut self, cx: &mut Context<Self>) {
        let now = self.now();
        let plan = recur::plan(&self.tasks, &self.snoozed, self.remind_since, now, &self.tz);
        self.remind_since = now;
        self.snoozed.retain(|(_, at)| *at > now);
        for f in plan.fire {
            cx.emit(TasksAction::Remind { uid: f.uid, title: f.title });
        }
        self.remind_timer = plan.next.map(|at| {
            let wait = Duration::from_secs((at - now).clamp(1, 86_400) as u64);
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wait).await;
                let _ = this.update(cx, |v, cx| v.remind_pass(cx));
            })
        });
    }

    /// «Отложить» напоминание.
    pub(crate) fn snooze(&mut self, uid: String, s: Snooze, cx: &mut Context<Self>) {
        let cal = &crate::prefs::get(cx).calendar;
        let morning = jiff::civil::Time::new((cal.work_start / 60).min(23) as i8, (cal.work_start % 60) as i8, 0, 0).unwrap_or(jiff::civil::Time::midnight());
        let at = recur::snooze_until(s, self.now(), morning, &self.tz);
        self.snoozed.push((uid, at));
        self.remind_pass(cx);
    }

    /// Слова писем моих невыполненных поручений: «Ждём» в почте.
    pub(crate) fn waiting_keywords(&self) -> Vec<String> {
        flow::waiting_keywords(&self.tasks, &self.me_email())
    }

    /// «Поручения»: вкладка.
    pub(crate) fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.set_nav(Nav::Delegations(tab), cx);
    }

    /// «Только свои» на «Я поручил».
    pub(crate) fn toggle_own(&mut self, cx: &mut Context<Self>) {
        self.own_only = !self.own_only;
        self.reshow(cx);
    }

    pub(crate) fn set_nav(&mut self, nav: Nav, cx: &mut Context<Self>) {
        if let Nav::Delegations(tab) = nav {
            self.tab = tab;
        }
        if self.nav != nav {
            self.nav = nav;
            self.selected = None;
            self.reshow(cx);
        }
    }

    fn select(&mut self, uid: String, cx: &mut Context<Self>) {
        if self.selected.as_deref() != Some(uid.as_str()) {
            self.draft = None;
            self.deleg.edit = None;
            self.open = None;
            self.slot = 0;
            self.load_linked(cx);
            self.selected = Some(uid);
            self.due_open = false;
            self.title = None;
            cx.notify();
        }
    }

    /// Разбор строки «Добавить задачу» (S6-04): язык интерфейса, «утром» / «вечером» — из рабочих часов.
    fn parse(&self, text: &str, cx: &App) -> Parsed {
        let lang = if pets_i18n::current().lang().starts_with("en") { Lang::En } else { Lang::Ru };
        let cal = &crate::prefs::get(cx).calendar;
        let at = |m: u16| jiff::civil::Time::new((m / 60).min(23) as i8, (m % 60) as i8, 0, 0).unwrap_or(jiff::civil::Time::midnight());
        let hours = Hours { morning: at(cal.work_start), evening: at(cal.work_end) };
        let now = jiff::Zoned::now().with_time_zone(self.tz.clone());
        model::parse(text, lang, now.date(), now.time(), hours, &self.categories, &self.ignored)
    }

    /// Крестик на чипе: кусок — снова часть названия.
    fn unchip(&mut self, text: String, cx: &mut Context<Self>) {
        self.ignored.push(text);
        let value = self.quick.read(cx).value().to_string();
        self.parsed = self.parse(&value, cx);
        cx.notify();
    }

    /// Снимок: строка «Добавить задачу» с текстом.
    pub(crate) fn set_quick(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.quick.update(cx, |s, cx| s.set_value(text.to_owned(), window, cx));
        self.parsed = self.parse(text, cx);
        cx.notify();
    }

    /// Поле карточки (S6-06): открыть или закрыть его варианты.
    pub(crate) fn toggle_field(&mut self, f: Field, cx: &mut Context<Self>) {
        self.open = if self.open == Some(f) { None } else { Some(f) };
        self.due_open = false;
        cx.notify();
    }

    fn edit_selected(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Task)) {
        let Some(mut t) = self.selected_task().cloned() else { return };
        if mailtask::is_letter(&t) {
            return;
        }
        f(&mut t);
        self.open = None;
        self.save(vec![t], cx);
    }

    fn set_remind(&mut self, secs: Option<i64>, cx: &mut Context<Self>) {
        self.edit_selected(cx, |t| {
            t.alerts = secs.map(|s| vec![Alert { trigger: Trigger::Offset { secs: s, from: Anchor::Due }, email: false }]).unwrap_or_default();
        });
    }

    fn set_repeat(&mut self, rule: Option<Rule>, cx: &mut Context<Self>) {
        self.edit_selected(cx, |t| recur::set_rule(t, rule));
    }

    fn set_project(&mut self, p: Option<String>, cx: &mut Context<Self>) {
        self.edit_selected(cx, |t| t.project = p);
    }

    fn set_category(&mut self, k: Option<String>, cx: &mut Context<Self>) {
        let table = self.categories.clone();
        self.edit_selected(cx, |t| {
            // Категории таблицы снимаются, чужие метки остаются.
            t.keywords.retain(|x| table.for_event(std::slice::from_ref(x)).is_none());
            t.keywords.extend(k);
            t.keywords.sort();
        });
    }

    /// Подзадача в конец списка подзадач выбранной.
    fn add_subtask(&mut self, title: String, cx: &mut Context<Self>) {
        let Some(parent) = self.selected_task().cloned().filter(|t| !mailtask::is_letter(t)) else { return };
        let order = model::subtasks(&self.tasks, &parent.uid).iter().filter_map(|s| s.order).max().map_or(0, |o| o + 1);
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let mut s = Task::new(&model::new_uid(nanos), parent.calendar_ids.first().map(String::as_str).unwrap_or(""), &title);
        s.parent = Some(parent.uid.clone());
        s.order = Some(order);
        s.project = parent.project.clone();
        self.save(vec![s], cx);
    }

    /// Заметки выбранной — в поле (при смене задачи); прежние несохранённые — записать.
    fn sync_notes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let uid = self.selected_task().filter(|t| !mailtask::is_letter(t)).map(|t| t.uid.clone());
        if uid == self.notes_uid {
            return;
        }
        if self.notes_save.take().is_some() {
            self.save_notes(cx);
        }
        let text = self.selected_task().map(|t| t.notes.clone()).unwrap_or_default();
        self.notes_loading = true;
        self.notes.update(cx, |s, cx| s.replace_all(text, window, cx));
        self.notes_loading = false;
        self.notes_uid = uid;
        self.paint_notes(cx);
    }

    /// Живое отображение Markdown заметок — как в письме (`postman_compose::display`, роли темы).
    fn paint_notes(&mut self, cx: &mut Context<Self>) {
        let l = crate::roles::look(cx);
        let text = self.notes.read(cx).value().to_string();
        let marks: Vec<TextDecoration> = postman_compose::display::display(&text)
            .into_iter()
            .map(|s| TextDecoration::new(s.range, crate::composer::md_style(&s.look, &l)))
            .collect();
        match &self.notes_marks {
            Some(c) => c.set(marks, cx),
            None => {
                let c = self.notes.update(cx, |s, cx| s.create_decorations_collection(marks, cx));
                self.notes_marks = Some(c);
            }
        }
    }

    fn schedule_notes(&mut self, cx: &mut Context<Self>) {
        self.notes_save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(800)).await;
            let _ = this.update(cx, |v, cx| {
                v.notes_save = None;
                v.save_notes(cx);
            });
        }));
    }

    fn save_notes(&mut self, cx: &mut Context<Self>) {
        let Some(uid) = self.notes_uid.clone() else { return };
        let text = self.notes.read(cx).value().to_string();
        let Some(mut t) = self.tasks.iter().find(|t| t.uid == uid).cloned() else { return };
        if t.notes != text {
            t.notes = text;
            self.save(vec![t], cx);
        }
    }

    /// Занятость сегодня (своё «занят» и «под вопросом») — от календаря экрана.
    pub(crate) fn set_busy(&mut self, busy: Vec<Busy>, cx: &mut Context<Self>) {
        if self.busy != busy {
            self.busy = busy;
            cx.notify();
        }
    }

    /// Свободные окна сегодня от «сейчас» в рабочие часы под оценку задачи (по умолчанию — длительность из
    /// «Настройки → Календарь»).
    fn slots(&self, t: &Task, cx: &App) -> (Vec<Span>, WorkHours, i64) {
        let cal = &crate::prefs::get(cx).calendar;
        let hours = cal.work_hours(self.tz.clone());
        let len = t.estimate.unwrap_or(i64::from(cal.duration) * 60).max(15 * 60);
        let now = self.now();
        // Ближайшее свободное окно: сегодня, а вечером и в выходные — в следующие рабочие дни (до недели).
        let end = day_of(now, &self.tz).and_then(|d| add_days(d, 8).at(0, 0, 0, 0).to_zoned(self.tz.clone()).ok()).map_or(now, |z| z.timestamp().as_second());
        let step = i64::from(cal.step.max(5)) * 60;
        let mine: Vec<Busy> = self
            .busy
            .iter()
            .copied()
            .chain(self.tasks.iter().filter(|x| x.uid != t.uid).filter_map(|x| x.block.as_ref()).map(|b| Busy { span: Span::new(b.start, b.end), kind: postman_calendar::BusyKind::Busy }))
            .collect();
        let slots = postman_calendar::find_slots(&[mine], &[], Span::new(now, end), len, step, &hours, 24);
        (slots, hours, len)
    }

    /// ←/→: соседнее свободное окно.
    pub(crate) fn step_slot(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(t) = self.selected_task().cloned() else { return };
        if t.block.is_some() {
            return;
        }
        let n = self.slots(&t, cx).0.len();
        if n > 0 {
            self.slot = (self.slot as isize + delta).rem_euclid(n as isize) as usize;
            cx.notify();
        }
    }

    /// Поставить блок в выбранное окно (S6-08): событие «занят» в своём календаре со ссылкой на задачу.
    pub(crate) fn place_block(&mut self, cx: &mut Context<Self>) {
        let Some(t) = self.selected_task().cloned().filter(|t| !mailtask::is_letter(t) && t.block.is_none()) else { return };
        let (slots, _, _) = self.slots(&t, cx);
        let Some(s) = slots.get(self.slot.min(slots.len().saturating_sub(1))).copied() else { return };
        if self.live {
            cx.emit(TasksAction::Send(Request::Block { uid: t.uid.clone(), start: s.start, end: s.end }));
        } else {
            let mut t = t;
            t.block = Some(TimeBlock { event_id: "demo".into(), start: s.start, end: s.end });
            self.save(vec![t], cx);
        }
        cx.emit(TasksAction::Toast(Toast::Success, tr!("tasks-block-placed", when = slot_label(s, &self.tz))));
    }

    fn remove_block(&mut self, cx: &mut Context<Self>) {
        let Some(t) = self.selected_task().cloned().filter(|t| t.block.is_some()) else { return };
        if self.live {
            cx.emit(TasksAction::Send(Request::Unblock { uid: t.uid.clone() }));
        } else {
            let mut t = t;
            t.block = None;
            self.save(vec![t], cx);
        }
    }

    pub(crate) fn selected_task(&self) -> Option<&Task> {
        let s = self.selected.as_deref()?;
        self.draft.as_ref().filter(|d| d.uid == s).or_else(|| self.shown.iter().find(|t| t.uid == s))
    }

    /// По списку вверх или вниз.
    pub(crate) fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let rows: Vec<usize> = self.items.iter().filter_map(|i| if let Item::Row(r) = i { Some(*r) } else { None }).collect();
        if rows.is_empty() {
            return;
        }
        let cur = self.selected.as_ref().and_then(|s| rows.iter().position(|&r| &self.shown[r].uid == s));
        let next = match cur {
            None => 0,
            Some(c) => (c as isize + delta).clamp(0, rows.len() as isize - 1) as usize,
        };
        let uid = self.shown[rows[next]].uid.clone();
        if let Some(ix) = self.items.iter().position(|i| *i == Item::Row(rows[next])) {
            self.list.scroll_to_reveal_item(ix);
        }
        self.select(uid, cx);
    }

    // --- правки ---

    fn task_by_uid(&self, uid: &str) -> Option<Task> {
        self.shown.iter().chain(self.tasks.iter()).find(|t| t.uid == uid).cloned()
    }

    fn letter(&self, key: &str) -> Option<&EmailRow> {
        self.letters.iter().chain(self.linked.iter()).find(|r| r.id == key)
    }

    /// Письмо выбранной задачи — из кэша (в фоне), если его нет среди писем со сроком.
    fn load_linked(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.selected_task().and_then(mailtask::letter_key).map(str::to_owned) else { return };
        if self.letter(&key).is_some() {
            return;
        }
        let cache = self.cache.clone();
        let task = cx.background_spawn(async move { cache.rows_by_ids(std::slice::from_ref(&key)) });
        cx.spawn(async move |this, cx| {
            if let Ok(rows) = task.await {
                let _ = this.update(cx, |v, cx| {
                    for r in rows {
                        if v.letter(&r.id).is_none() {
                            v.linked.push(r);
                        }
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Снимок `task-card-mail`: задача демо со ссылкой на письмо ящика (первое во «Входящих»).
    pub(crate) fn attach_demo_letter(&mut self, row: EmailRow, cx: &mut Context<Self>) {
        let Some(t) = self.tasks.iter_mut().find(|t| t.title == postman_demo::tasks::MAIN) else { return };
        t.mail = Some(postman_calendar::MailLink { message_id: String::new(), email_id: Some(row.id.clone()) });
        let uid = t.uid.clone();
        self.linked.push(row);
        self.reshow(cx);
        self.select(uid, cx);
    }

    /// Снимок `task-repeat`: варианты повтора открыты.
    pub(crate) fn demo_repeat(&mut self, cx: &mut Context<Self>) {
        self.toggle_field(Field::Repeat, cx);
    }

    /// Снимок `task-timeblock`: блок в выбранное окно.
    pub(crate) fn demo_block(&mut self, cx: &mut Context<Self>) {
        self.place_block(cx);
    }

    /// Снимок `task-notes`: фокус в заметки.
    pub(crate) fn focus_notes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_notes(window, cx);
        self.notes.update(cx, |s, cx| s.focus(window, cx));
    }

    /// Записать задачи у себя и отправить; демо — только в памяти.
    fn save(&mut self, list: Vec<Task>, cx: &mut Context<Self>) {
        for t in &list {
            match self.tasks.iter_mut().find(|x| x.uid == t.uid) {
                Some(x) => *x = t.clone(),
                None if !mailtask::is_letter(t) => self.tasks.push(t.clone()),
                None => {}
            }
        }
        if self.live {
            let cache = self.cache.clone();
            let write = cx.background_spawn(async move {
                for t in list {
                    model::save_local(&cache, t)?;
                }
                Ok::<(), postman_cache::Error>(())
            });
            cx.spawn(async move |this, cx| {
                let done = write.await;
                let _ = this.update(cx, |_, cx| match done {
                    Ok(()) => cx.emit(TasksAction::Send(Request::Flush)),
                    Err(e) => tracing::warn!("tasks: save: {e}"),
                });
            })
            .detach();
        }
        self.reshow(cx);
    }

    fn remove(&mut self, uid: &str, cx: &mut Context<Self>) {
        self.tasks.retain(|t| t.uid != uid);
        if self.live {
            let cache = self.cache.clone();
            let uid = uid.to_owned();
            let write = cx.background_spawn(async move { model::delete_local(&cache, &uid) });
            cx.spawn(async move |this, cx| {
                let done = write.await;
                let _ = this.update(cx, |_, cx| match done {
                    Ok(()) => cx.emit(TasksAction::Send(Request::Flush)),
                    Err(e) => tracing::warn!("tasks: delete: {e}"),
                });
            })
            .detach();
        }
        self.reshow(cx);
    }

    /// Правка задачи-письма или задачи со ссылкой — и слова срока письма.
    fn letter_patches(&self, t: &Task) -> Vec<Patch> {
        let Some(key) = mailtask::letter_key(t) else { return Vec::new() };
        self.letter(key).and_then(|row| mailtask::letter_patch(t, row, &self.tz)).into_iter().collect()
    }

    /// Выполнить или вернуть выбранную (Space, круг в строке).
    pub(crate) fn toggle_done(&mut self, cx: &mut Context<Self>) {
        // Поручение: Space — главное действие стороны (S6-10).
        if self.deleg_primary(cx) {
            return;
        }
        if let Some(uid) = self.selected.clone() {
            self.toggle_done_of(&uid, cx);
        }
    }

    fn toggle_done_of(&mut self, uid: &str, cx: &mut Context<Self>) {
        if self.completing.cancel(uid) {
            self.reshow(cx);
            return;
        }
        let Some(t) = self.task_by_uid(uid) else { return };
        if t.progress.is_closed() {
            let mut back = t.clone();
            back.set_done(false, 0);
            self.save(vec![back], cx);
            cx.emit(TasksAction::Toast(Toast::Info, tr!("tasks-reopened")));
            return;
        }
        self.completing.start(uid, Instant::now());
        self.arm(cx);
        self.reshow(cx);
    }

    /// Таймер к ближайшему сроку выполнения.
    fn arm(&mut self, cx: &mut Context<Self>) {
        let Some(wait) = self.completing.next_in(Instant::now()) else {
            self.timer = None;
            return;
        };
        self.timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |v, cx| v.finish_due(cx));
        }));
    }

    fn finish_due(&mut self, cx: &mut Context<Self>) {
        let ready = self.completing.due(Instant::now());
        for uid in ready {
            self.finish(&uid, cx);
        }
        self.arm(cx);
        self.reshow(cx);
    }

    fn finish(&mut self, uid: &str, cx: &mut Context<Self>) {
        let Some(t) = self.task_by_uid(uid) else { return };
        let mut done = t.clone();
        done.set_done(true, self.now());
        // Повторяющаяся (S6-07): следующая копия со сдвигом срока, у выполненной повтора больше нет.
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let next = (!mailtask::is_letter(&t)).then(|| recur::next_task(&t, &model::new_uid(nanos), self.now(), &self.tz)).flatten();
        if next.is_some() {
            recur::set_rule(&mut done, None);
        }
        let patches = self.letter_patches(&done);
        let text = match &next {
            Some(n) => tr!("tasks-done-next", when = due_text(n, &self.tz)),
            None if t.mail.is_some() => tr!("tasks-done-mail"),
            None => tr!("tasks-done", title = t.title.clone()),
        };
        let undo = if mailtask::is_letter(&t) {
            // Задача-письмо: вернуть — срок у письма снова.
            let back = self.letter(mailtask::letter_key(&t).unwrap_or_default()).and_then(|row| {
                mailtask::letter_due(row).map(|w| cycle::set(row, Kind::Due, Some(w)))
            });
            Undo::Letters(back.into_iter().collect())
        } else {
            Undo::Tasks(vec![t.clone()])
        };
        if !patches.is_empty() {
            cx.emit(TasksAction::Patch(patches));
        }
        if !mailtask::is_letter(&t) {
            let mut list = vec![done];
            list.extend(next);
            self.save(list, cx);
        }
        cx.emit(TasksAction::Undo { text, undo });
    }

    /// «Отменить» тоста: вернуть задачи (выполнение, удаление) или слова срока письма.
    pub(crate) fn undo(&mut self, undo: Undo, cx: &mut Context<Self>) {
        match undo {
            Undo::Tasks(list) => self.save(list, cx),
            Undo::Letters(p) => cx.emit(TasksAction::Patch(p)),
            Undo::Delegation(t) => self.undo_delegation(*t, cx),
        }
    }

    /// Удалить выбранную (Del); у задачи-письма — снять срок письма.
    pub(crate) fn delete(&mut self, cx: &mut Context<Self>) {
        // Поручение: удалить у себя и отменить у исполнителя (S6-11); чужое — не трогать.
        if self.delete_delegation(cx) {
            return;
        }
        let Some(t) = self.selected_task().cloned() else { return };
        if mailtask::is_letter(&t) {
            let mut gone = t.clone();
            gone.due = None;
            let patches = self.letter_patches(&gone);
            let back = self.letter(mailtask::letter_key(&t).unwrap_or_default()).and_then(|row| {
                mailtask::letter_due(row).map(|w| cycle::set(row, Kind::Due, Some(w)))
            });
            cx.emit(TasksAction::Patch(patches));
            cx.emit(TasksAction::Undo { text: tr!("tasks-deleted-mail"), undo: Undo::Letters(back.into_iter().collect()) });
            return;
        }
        self.remove(&t.uid, cx);
        cx.emit(TasksAction::Undo { text: tr!("tasks-deleted"), undo: Undo::Tasks(vec![t]) });
    }

    /// «!» в карточке.
    fn toggle_important(&mut self, cx: &mut Context<Self>) {
        let Some(mut t) = self.selected_task().cloned() else { return };
        if mailtask::is_letter(&t) {
            return;
        }
        t.important = !t.important;
        self.save(vec![t], cx);
    }

    /// Подзадача: выполнить или вернуть сразу (без задержки — она в карточке, не в списке).
    fn toggle_sub(&mut self, uid: String, cx: &mut Context<Self>) {
        let Some(mut t) = self.tasks.iter().find(|t| t.uid == uid).cloned() else { return };
        let done = !t.progress.is_closed();
        t.set_done(done, self.now());
        self.save(vec![t], cx);
    }

    /// Срок (Insert): варианты в карточке.
    pub(crate) fn toggle_due(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Поручение: Insert — изменить срок (поручивший) или предложить (исполнитель).
        if self.deleg_due(window, cx) {
            return;
        }
        if self.selected.is_some() {
            self.due_open = !self.due_open;
            cx.notify();
        }
    }

    fn set_due(&mut self, when: Option<When>, cx: &mut Context<Self>) {
        let Some(mut t) = self.selected_task().cloned() else { return };
        self.due_open = false;
        match when {
            Some(w) => {
                let (m, all_day) = mailtask::due_moment(w);
                t.due = Some(m);
                t.all_day = all_day;
            }
            None => t.due = None,
        }
        let patches = self.letter_patches(&t);
        if !patches.is_empty() {
            cx.emit(TasksAction::Patch(patches));
        }
        if !mailtask::is_letter(&t) {
            self.save(vec![t], cx);
        } else {
            cx.notify();
        }
    }

    /// Переименовать (F2): поле вместо названия в карточке; Enter — сохранить, Esc — отменить.
    pub(crate) fn rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.selected_task().cloned() else { return };
        if mailtask::is_letter(&t) {
            return;
        }
        let state = cx.new(|cx| InputState::new(window, cx).default_value(t.title.clone()));
        let sub = cx.subscribe(&state, |this: &mut Self, state, ev: &InputEvent, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                let text = state.read(cx).value().trim().to_string();
                this.finish_rename(text, cx);
            }
        });
        state.update(cx, |s, cx| s.focus(window, cx));
        self._subscriptions.push(sub);
        self.title = Some(state);
        cx.notify();
    }

    fn finish_rename(&mut self, text: String, cx: &mut Context<Self>) {
        self.title = None;
        if let Some(mut d) = self.draft.take() {
            // Новая задача: без названия — не создавать.
            if text.is_empty() {
                self.selected = None;
                self.reshow(cx);
                return;
            }
            d.title = text;
            let uid = d.uid.clone();
            self.save(vec![d], cx);
            self.selected = Some(uid);
            cx.notify();
            return;
        }
        let Some(mut t) = self.selected_task().cloned() else { return };
        if !text.is_empty() && text != t.title {
            t.title = text;
            self.save(vec![t], cx);
        }
        cx.notify();
    }

    /// Esc: правка названия, варианты срока, поиск.
    pub(crate) fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.deleg_escape(cx) {
            return true;
        }
        if self.draft.take().is_some() {
            self.title = None;
            self.selected = None;
            self.reshow(cx);
            return true;
        }
        if self.title.take().is_some() || std::mem::take(&mut self.due_open) {
            cx.notify();
            return true;
        }
        if !self.query.is_empty() {
            self.search.update(cx, |s, cx| s.set_value("", window, cx));
            self.query.clear();
            self.reshow(cx);
            return true;
        }
        false
    }

    pub(crate) fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| s.focus(window, cx));
    }

    /// «Добавить задачу»: название — строкой (разбор фразы — S6-04). В «Сегодня» — со сроком сегодня, в проекте и
    /// категории — с ними, в «Когда-нибудь» — туда.
    fn quick_add(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        let title = text.trim();
        if title.is_empty() {
            return false;
        }
        let p = self.parse(text, cx);
        let mut t = self.draft_for_nav(&p.title);
        if t.title.trim().is_empty() {
            t.title = title.to_owned();
        }
        if let Some(date) = p.date {
            let w = match p.time {
                Some(time) => When::at(date, time),
                None => When::day(date),
            };
            let (m, all_day) = mailtask::due_moment(w);
            t.due = Some(m);
            t.all_day = all_day;
        }
        if let Some(k) = p.category {
            t.keywords = vec![k];
        }
        t.important = p.important;
        let uid = t.uid.clone();
        self.save(vec![t], cx);
        self.select(uid, cx);
        true
    }

    /// Enter: письмо задачи.
    pub(crate) fn open_letter(&mut self, cx: &mut Context<Self>) {
        if let Some(key) = self.selected_task().and_then(mailtask::letter_key).map(str::to_owned) {
            cx.emit(TasksAction::OpenLetter(key));
        }
    }

    /// Строка состояния: «N задач из писем»; в «Поручениях» — сколько их в «Ждём».
    pub(crate) fn status_text(&self) -> String {
        if matches!(self.nav, Nav::Delegations(_)) {
            let c = flow::counts(&self.tasks, &self.me_email(), self.own_only);
            return tr!("deleg-status-waiting", n = c.waiting as i64);
        }
        let n = mailtask::from_mail(&self.tasks, &self.letters, &self.tz).len();
        tr!("tasks-status-mail", n = n as i64)
    }

    /// Ctrl+N в модуле: новая задача — карточка с полем названия (в текущем списке: в «Сегодня» — срок сегодня,
    /// в проекте — с ним). Enter — сохранить, Esc или пустое название — не создавать.
    pub(crate) fn new_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let t = self.draft_for_nav("");
        self.selected = Some(t.uid.clone());
        self.draft = Some(t);
        self.due_open = false;
        self.rename(window, cx);
    }

    /// Задача для текущего списка: срок, «Когда-нибудь», проект или категория — по нему.
    fn draft_for_nav(&self, title: &str) -> Task {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let mut t = Task::new(&model::new_uid(nanos), "", title);
        let today = day_of(self.now(), &self.tz).unwrap_or_default();
        let due = |t: &mut Task, d: Date| {
            let (m, all_day) = mailtask::due_moment(When::day(d));
            t.due = Some(m);
            t.all_day = all_day;
        };
        match &self.nav {
            Nav::List(List::Today) => due(&mut t, today),
            Nav::List(List::Upcoming) => due(&mut t, add_days(today, 1)),
            Nav::List(List::Someday) => t.someday = true,
            Nav::Project(p) => t.project = Some(p.clone()),
            Nav::Category(k) => t.keywords = vec![k.clone()],
            _ => {}
        }
        t
    }

    /// Снимок: выбрать задачу по названию (демо).
    pub(crate) fn select_title(&mut self, title: &str, cx: &mut Context<Self>) {
        if let Some(t) = self.shown.iter().find(|t| t.title == title) {
            let uid = t.uid.clone();
            self.select(uid, cx);
        }
    }

    pub(crate) fn ready(&self) -> bool {
        self.loaded
    }

    // --- отрисовка ---

    fn nav_view(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let l = crate::roles::look(cx);
        let now = self.now();
        let open: Vec<Task> = self.tasks.clone();
        let count = |which: List| match which {
            List::FromMail => mailtask::from_mail(&open, &self.letters, &self.tz).len(),
            List::Done => {
                let today = day_of(now, &self.tz);
                open.iter().filter(|t| t.parent.is_none() && t.progress.is_closed() && t.completed_at.and_then(|c| day_of(c, &self.tz)) == today).count()
            }
            w => model::count(&open, w, now, &self.tz),
        };
        let overdue = open.iter().filter(|t| t.parent.is_none() && model::is_overdue(t, now, &self.tz)).count();
        let item = |id: ElementId, glyph: Option<char>, lead: Option<AnyElement>, text: String, n: usize, badge: usize, nav: Nav, cx: &mut Context<Self>| {
            let on = self.nav == nav;
            div()
                .id(id)
                .h(px(32.))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(10.))
                .px(px(8.))
                .rounded(px(4.))
                .cursor_pointer()
                .when(on, |d| d.bg(l.hover()).font_weight(FontWeight::SEMIBOLD))
                .when(!on, |d| d.hover(|d| d.bg(l.hover())))
                .when_some(glyph, |d, gl| d.child(icon(gl, 16.).text_color(l.text2())))
                .when_some(lead, |d, e| d.child(div().size(px(16.)).flex_none().flex().items_center().justify_center().child(e)))
                .child(div().flex_1().min_w_0().truncate().child(text))
                .when(badge > 0, |d| {
                    d.child(
                        div()
                            .min_w(px(18.))
                            .h(px(18.))
                            .px(px(5.))
                            .rounded(px(9.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .bg(l.pm(crate::roles::Pm::DeadlineFill))
                            .text_color(l.pm(crate::roles::Pm::DeadlineOn))
                            .child(badge.to_string()),
                    )
                })
                .when(n > 0, |d| d.child(div().text_color(l.text2()).font_weight(FontWeight::NORMAL).child(n.to_string())))
                .on_click(cx.listener(move |this, _, _, cx| this.set_nav(nav.clone(), cx)))
        };
        let lists = [
            (List::Inbox, g::INBOX, "tasks-inbox"),
            (List::Today, g::TODAY, "tasks-today"),
            (List::Upcoming, g::UPCOMING, "tasks-upcoming"),
            (List::FromMail, g::FLAG, "tasks-from-mail"),
            (List::Someday, g::SOMEDAY, "tasks-someday"),
            (List::Done, g::DONE, "tasks-done-list"),
        ];
        let mut col = div()
            .id("tasks-nav")
            .overflow_y_scroll()
            .w(px(NAV_W))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .gap(px(2.))
            .pt(px(10.))
            .pr(px(8.))
            .pl(px(4.))
            .text_size(px(13.));
        for (i, (which, gl, key)) in lists.into_iter().enumerate() {
            let badge = if which == List::Today { overdue } else { 0 };
            col = col.child(item(("tasks-nav", i).into(), Some(gl), None, tr!(key), count(which), badge, Nav::List(which), cx));
        }
        let dc = flow::counts(&self.tasks, &self.me_email(), self.own_only);
        col = col.child(item(
            "tasks-nav-deleg".into(),
            Some(g::DELEGATE),
            None,
            tr!("tasks-delegations"),
            dc.by_me + dc.to_me,
            dc.new + dc.review,
            Nav::Delegations(self.tab),
            cx,
        ));
        let heading = |key: &str| div().text_size(px(12.)).text_color(l.text2()).px(px(8.)).pt(px(14.)).pb(px(4.)).child(tr!(key));
        let projects = model::projects(&self.tasks);
        if !projects.is_empty() {
            col = col.child(heading("tasks-projects"));
            for (i, p) in projects.into_iter().enumerate() {
                let open_n = p.total - p.done;
                let ring = progress_ring(p.done, p.total, 14., &l).into_any_element();
                col = col.child(item(("tasks-proj", i).into(), None, Some(ring), p.name.clone(), open_n, 0, Nav::Project(p.name.clone()), cx));
            }
        }
        let cats: Vec<(String, String, String, usize)> = self
            .categories
            .list
            .iter()
            .filter(|c| !c.hidden)
            .map(|c| {
                let n = self.tasks.iter().filter(|t| !t.progress.is_closed() && t.keywords.iter().any(|k| k.eq_ignore_ascii_case(&c.keyword))).count();
                (c.keyword.clone(), c.name.clone(), c.color.clone(), n)
            })
            .collect();
        if !cats.is_empty() {
            col = col.child(heading("tasks-categories"));
            for (i, (kw, name, color, n)) in cats.into_iter().enumerate() {
                let dot = div().size(px(10.)).rounded(px(3.)).bg(l.cat(&color).dot).into_any_element();
                col = col.child(item(("tasks-cat", i).into(), None, Some(dot), name, n, 0, Nav::Category(kw), cx));
            }
        }
        col.child(div().flex_1()).child(
            div()
                .mx(px(4.))
                .mb(px(12.))
                .px(px(12.))
                .py(px(10.))
                .rounded(px(6.))
                .bg(l.pm(crate::roles::Pm::CalOff))
                .border_1()
                .border_color(l.border())
                .text_size(px(12.))
                .line_height(px(17.))
                .text_color(l.text2())
                .child(tr!("tasks-nav-note")),
        )
    }

    fn title_of(&self) -> String {
        match &self.nav {
            Nav::List(l) => tr!(list_key(*l)),
            Nav::Project(p) => p.clone(),
            Nav::Category(k) => self.categories.list.iter().find(|c| &c.keyword == k).map(|c| c.name.clone()).unwrap_or_else(|| k.clone()),
            Nav::Delegations(_) => tr!("tasks-delegations"),
        }
    }

    fn summary(&self) -> String {
        let open = self.shown.iter().filter(|t| !t.progress.is_closed()).count() as i64;
        let now = self.now();
        match &self.nav {
            Nav::List(List::Today) => {
                let over = self.shown.iter().filter(|t| model::is_overdue(t, now, &self.tz)).count() as i64;
                let day = day_of(now, &self.tz).map(crate::calendar::long_day).unwrap_or_default();
                if over > 0 { tr!("tasks-summary-today-over", day = day, n = open, over = over) } else { tr!("tasks-summary-today", day = day, n = open) }
            }
            Nav::List(List::FromMail) => tr!("tasks-summary-mail", n = open),
            Nav::List(List::Done) => tr!("tasks-summary", n = self.shown.len() as i64),
            Nav::Delegations(tab) => {
                let c = flow::counts(&self.tasks, &self.me_email(), self.own_only);
                let n = self.shown.len() as i64;
                match tab {
                    Tab::ByMe => tr!("deleg-summary-by-me", n = n, review = c.review as i64),
                    Tab::ToMe => tr!("deleg-summary-to-me", n = n, new = c.new as i64),
                }
            }
            Nav::Project(p) => {
                let pr = model::projects(&self.tasks).into_iter().find(|x| x.name.eq_ignore_ascii_case(p));
                let (done, total) = pr.map(|x| (x.done as i64, x.total as i64)).unwrap_or((0, 0));
                tr!("tasks-summary-project", done = done, total = total)
            }
            _ => tr!("tasks-summary", n = open),
        }
    }

    fn list_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let l = crate::roles::look(cx);
        let header = div()
            .h(px(52.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(10.))
            .pl(px(18.))
            .pr(px(8.))
            .child(div().text_size(px(20.)).font_weight(FontWeight::SEMIBOLD).whitespace_nowrap().child(self.title_of()))
            .child(div().pt(px(3.)).text_color(l.text2()).truncate().child(self.summary()));
        let quick_shown = !matches!(self.nav, Nav::List(List::Done) | Nav::List(List::FromMail) | Nav::Delegations(_));
        let deleg_bar = matches!(self.nav, Nav::Delegations(_)).then(|| self.deleg_bar(&l, cx));
        let deleg_hint = matches!(self.nav, Nav::Delegations(Tab::ByMe)).then(|| self.deleg_hint(&l, cx));
        let chips = if quick_shown { self.quick_chips(&l, cx) } else { None };
        let quick = quick_shown.then(|| {
            div().flex_none().px(px(14.)).pb(px(10.)).child(
                Input::new(&self.quick).prefix(icon(g::ADD, 14.).text_color(l.accent_text())),
            ).children(chips)
        });
        let empty = self.shown.is_empty().then(|| {
            div().px(px(24.)).py(px(48.)).text_center().text_color(l.text2()).line_height(px(20.)).child(tr!(empty_key(&self.nav)))
        });
        let body = list(self.list.clone(), cx.processor(|this, ix: usize, _window, cx| this.item(ix, cx))).size_full();
        crate::mailer::card(&l)
            .flex_1()
            .min_w_0()
            .mr(px(8.))
            .child(header)
            .children(deleg_bar)
            .children(quick)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .border_t_1()
                    .border_color(l.border())
                    .children(empty)
                    .child(body)
                    .child(Scrollbar::vertical(&self.list).mode(ScrollbarMode::Hover)),
            )
            .children(deleg_hint)
    }

    fn item(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let l = crate::roles::look(cx);
        match self.items.get(ix).cloned() {
            Some(Item::Head(gr, n)) => div()
                .w_full()
                .h(px(HEAD_H))
                .flex()
                .items_center()
                .gap(px(8.))
                .pl(px(18.))
                .pr(px(14.))
                .bg(l.pm(crate::roles::Pm::CalOff))
                .border_b_1()
                .border_color(l.border())
                .text_size(px(12.))
                .child(div().font_weight(FontWeight::SEMIBOLD).child(group_label(gr)))
                .child(div().text_color(l.text2()).child(n.to_string()))
                .when_some(group_sub(gr), |d, s| d.child(div().text_color(l.text3()).child(s)))
                .when(gr == Group::Deleg(Bucket::Waiting) && self.remind_all_count() > 0, |d| {
                    d.child(div().flex_1()).child(
                        crate::settings_tab::link("deleg-remind-all", tr!("deleg-remind-all"), &l)
                            .on_click(cx.listener(|this, _, _, cx| this.remind_all(cx))),
                    )
                })
                .into_any_element(),
            Some(Item::Row(i)) => match self.shown.get(i).cloned() {
                Some(t) => self.row(ix, &t, &l, cx),
                None => div().into_any_element(),
            },
            None => div().into_any_element(),
        }
    }

    fn row(&self, ix: usize, t: &Task, l: &Look, cx: &mut Context<Self>) -> AnyElement {
        let pending = self.completing.is_pending(&t.uid);
        let done = t.progress.is_closed();
        let checked = pending || done;
        let selected = self.selected.as_deref() == Some(t.uid.as_str());
        let cat = model::category(t, &self.categories).map(|c| (c.name.clone(), c.color.clone()));
        let mut meta: Vec<AnyElement> = Vec::new();
        if let Some(m) = &t.mail {
            let from = mailtask::letter_key(t).and_then(|k| self.letter(k)).map(|r| if r.from_name.is_empty() { r.from_addr.clone() } else { r.from_name.clone() });
            let text = from.unwrap_or_else(|| if m.message_id.is_empty() { tr!("tasks-letter") } else { m.message_id.clone() });
            meta.push(div().flex().items_center().gap(px(5.)).min_w_0().text_color(l.text()).child(icon(g::MAIL, 12.)).child(div().truncate().child(text)).into_any_element());
        }
        if let Some(p) = &t.project
            && !matches!(self.nav, Nav::Project(_))
        {
            let pr = model::projects(&self.tasks).into_iter().find(|x| x.name.eq_ignore_ascii_case(p));
            let (d, n) = pr.map(|x| (x.done, x.total)).unwrap_or((0, 0));
            meta.push(div().flex().items_center().gap(px(5.)).min_w_0().child(progress_ring(d, n, 11., l)).child(div().truncate().child(p.clone())).into_any_element());
        }
        let (sd, sn) = model::subtask_progress(&self.tasks, &t.uid);
        if sn > 0 && !done {
            meta.push(div().flex().items_center().gap(px(4.)).child(icon(g::SUBTASKS, 12.)).child(format!("{sd}/{sn}")).into_any_element());
        }
        if let Some(r) = recur::rule_of(t) {
            meta.push(div().flex().items_center().gap(px(4.)).child(icon(g::REPEAT, 12.)).child(repeat_text(&r)).into_any_element());
        }
        if !t.notes.trim().is_empty() && !l.compact() {
            meta.push(icon(g::NOTES, 12.).into_any_element());
        }
        if done && let Some(c) = t.completed_at {
            meta.insert(0, div().child(crate::calendar::hm(c, &self.tz)).into_any_element());
        }
        let in_deleg = matches!(self.nav, Nav::Delegations(_));
        let watched = in_deleg && postman_calendar::assign::is_watched_by(t, &self.me_email());
        let fresh = matches!(self.nav, Nav::Delegations(Tab::ToMe)) && postman_calendar::assign::status(t) == Some(postman_calendar::assign::Status::Waiting);
        if in_deleg {
            meta = self.deleg_meta(t, l);
        }
        let two = !meta.is_empty() && !l.compact();
        let mut chips: Vec<AnyElement> = Vec::new();
        if watched {
            chips.push(
                div().px(px(6.)).rounded(px(3.)).bg(l.fill()).text_size(px(11.)).line_height(px(16.)).text_color(l.text2()).child(tr!("deleg-watching")).into_any_element(),
            );
        }
        if !checked && let Some(b) = due_badge(t, self.now(), &self.tz) {
            chips.push(crate::lifecycle::chip(&b, l).into_any_element());
        }
        if let Some((name, color)) = &cat {
            chips.push(crate::labels::chip(name.clone(), color, l).into_any_element());
        }
        let uid = t.uid.clone();
        let check_uid = t.uid.clone();
        let deleg = postman_calendar::assign::status(t);
        let circle = div()
            .id(("task-check", ix))
            .size(px(20.))
            .flex_none()
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .when_some(deleg, |d, st| d.child(delegation::status_circle(st, 20., l)))
            .when(deleg.is_none() && checked, |d| d.bg(if done { l.text3() } else { l.accent() }).text_color(l.on_accent()).child(icon(g::CHECK, 11.)))
            .when(deleg.is_none() && !checked, |d| d.border_2().border_color(l.text3()).hover(|d| d.border_color(l.accent_text())))
            .when(deleg.is_none(), |d| {
                d.on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.toggle_done_of(&check_uid, cx)
                }))
            });
        let title = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .min_w_0()
            .text_size(px(14.))
            .when(t.important && !checked, |d| d.child(div().font_weight(FontWeight::BOLD).text_color(l.accent_text()).child("!")))
            .child(
                div()
                    .truncate()
                    .when(checked, |d| d.line_through().text_color(l.text3()))
                    .when((selected || fresh) && !checked, |d| d.font_weight(FontWeight::SEMIBOLD))
                    .child(t.title.clone()),
            );
        let inner = if two {
            div().flex_1().min_w_0().flex().flex_col().gap(px(2.)).child(title).child(
                div().flex().items_center().gap(px(12.)).min_w_0().overflow_hidden().text_size(px(12.)).text_color(l.text2()).children(meta),
            )
        } else {
            div().flex_1().min_w_0().flex().items_center().gap(px(12.)).child(title)
        };
        let stripe = cat.as_ref().map(|(_, c)| l.cat(c).dot);
        div()
            .id(("task", ix))
            .relative()
            .w_full()
            .h(px(if two { ROW_TWO } else if l.compact() { 34. } else { ROW_H }))
            .flex()
            .items_center()
            .gap(px(10.))
            .pl(px(14.))
            .pr(px(12.))
            .border_b_1()
            .border_color(l.border())
            .when(selected, |d| d.bg(l.picked()))
            .when(!selected, |d| d.hover(|d| d.bg(l.hover())))
            .when(pending, |d| d.opacity(0.75))
            .when_some(stripe, |d, c| d.child(div().absolute().left_0().top(px(6.)).bottom(px(6.)).w(px(3.)).rounded_r(px(2.)).bg(c)))
            .child(circle)
            .child(inner)
            .child(div().flex_none().flex().items_center().gap(px(4.)).when(two, |d| d.self_start().mt(px(12.))).children(chips))
            .on_click(cx.listener(move |this, _, _, cx| this.select(uid.clone(), cx)))
            .into_any_element()
    }

    /// «Время в календаре» (S6-08, макет): оценка, ближайшее свободное окно сегодня кнопкой (←/→ — соседние),
    /// поставленный блок — со «Убрать»; ниже — рабочий день полосой: занятость, окно, «сейчас».
    fn block_view(&self, t: &Task, l: &Look, cx: &mut Context<Self>) -> AnyElement {
        let (slots, hours, len) = self.slots(t, cx);
        let est = crate::calendar::duration_text(len);
        let now = self.now();
        let chosen = t.block.as_ref().map(|b| Span::new(b.start, b.end)).or_else(|| slots.get(self.slot.min(slots.len().saturating_sub(1))).copied());
        // Полоса — день выбранного окна (сегодня, если окна нет).
        let today = chosen.and_then(|s| day_of(s.start, &self.tz)).or_else(|| day_of(now, &self.tz)).unwrap_or_default();
        let at = |time: jiff::civil::Time| today.to_datetime(time).to_zoned(self.tz.clone()).map_or(now, |z| z.timestamp().as_second());
        let (day_from, day_to) = (at(hours.start), at(hours.end).max(at(hours.start) + 3600));
        let span = (day_to - day_from) as f32;
        let x = |ts: i64| ((ts.clamp(day_from, day_to) - day_from) as f32 / span).clamp(0., 1.);
        let head = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.))
            .child(div().font_weight(FontWeight::SEMIBOLD).whitespace_nowrap().child(tr!("tasks-block")))
            .child(div().text_size(px(12.)).text_color(l.text2()).whitespace_nowrap().child(est))
            .child(div().flex_1());
        // Окно и кнопки — своей строкой: подпись с датой длинная.
        let controls = div().flex().items_center().gap(px(8.));
        let controls = match (&t.block, chosen) {
            (Some(b), _) => controls
                .child(div().text_size(px(12.)).text_color(l.accent_text()).child(slot_label(Span::new(b.start, b.end), &self.tz)))
                .child(
                    div()
                        .id("block-remove")
                        .text_size(px(12.))
                        .text_color(l.text2())
                        .cursor_pointer()
                        .child(tr!("tasks-block-remove"))
                        .on_click(cx.listener(|this, _, _, cx| this.remove_block(cx))),
                ),
            (None, Some(s)) => controls
                .child(crate::calendar::icon_button("slot-prev", crate::calendar::g::PREV, l).on_click(cx.listener(|this, _, _, cx| this.step_slot(-1, cx))))
                .child(
                    div()
                        .id("block-place")
                        .h(px(26.))
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .rounded(px(4.))
                        .border_1()
                        .border_color(l.stroke())
                        .text_size(px(12.))
                        .cursor_pointer()
                        .hover(|d| d.bg(l.hover()))
                        .child(slot_label(s, &self.tz))
                        .on_click(cx.listener(|this, _, _, cx| this.place_block(cx))),
                )
                .child(crate::calendar::icon_button("slot-next", crate::calendar::g::NEXT, l).on_click(cx.listener(|this, _, _, cx| this.step_slot(1, cx)))),
            (None, None) => controls.child(div().text_size(px(12.)).text_color(l.text3()).child(tr!("tasks-block-none"))),
        };
        let mut bar = div().relative().h(px(22.)).rounded(px(4.)).bg(l.fill()).overflow_hidden();
        for b in &self.busy {
            let (a, z) = (x(b.span.start), x(b.span.end));
            if z > a {
                bar = bar.child(div().absolute().top(px(3.)).bottom(px(3.)).left(relative(a)).w(relative(z - a)).rounded(px(3.)).bg(l.text3()));
            }
        }
        if let Some(s) = chosen {
            let (a, z) = (x(s.start), x(s.end));
            bar = bar.child(
                div()
                    .absolute()
                    .top(px(2.))
                    .bottom(px(2.))
                    .left(relative(a))
                    .w(relative((z - a).max(0.01)))
                    .rounded(px(3.))
                    .border_2()
                    .border_color(l.accent_text())
                    .when(t.block.is_some(), |d| d.bg(l.picked())),
            );
        }
        let nx = x(now);
        bar = bar
            .child(div().absolute().top_0().bottom_0().left_0().w(relative(nx)).bg(l.panel()).opacity(0.5))
            .child(div().absolute().top_0().bottom_0().left(relative(nx)).w(px(2.)).bg(l.text()));
        let mut ticks = div().relative().h(px(13.)).text_size(px(11.)).text_color(l.text2());
        let mut h = hours.start.hour();
        while h <= hours.end.hour() && h < 24 {
            let ts = at(jiff::civil::Time::new(h, 0, 0, 0).unwrap_or(jiff::civil::Time::midnight()));
            ticks = ticks.child(div().absolute().top_0().left(relative(x(ts))).child(format!("{h}")));
            h += 2;
        }
        div()
            .mx(px(20.))
            .py(px(10.))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(6.))
            .border_b_1()
            .border_color(l.border())
            .child(head)
            .child(controls)
            .child(bar)
            .child(ticks)
            .into_any_element()
    }

    /// Чипы распознанного под строкой «Добавить задачу» (S6-04): × — снять распознавание; ниже — итог.
    fn quick_chips(&self, l: &Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = &self.parsed;
        if p.chips.is_empty() {
            return None;
        }
        let chips = p.chips.iter().enumerate().map(|(i, c)| {
            let text = c.text.clone();
            let label = match c.kind {
                ChipKind::Category => p.category.as_ref().and_then(|k| self.categories.list.iter().find(|x| &x.keyword == k)).map(|x| format!("#{}", x.name)).unwrap_or_else(|| c.text.clone()),
                _ => c.text.clone(),
            };
            let color = match c.kind {
                ChipKind::Category => p.category.as_ref().and_then(|k| self.categories.list.iter().find(|x| &x.keyword == k)).map(|x| l.cat(&x.color)),
                _ => None,
            };
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .h(px(24.))
                .pl(px(7.))
                .pr(px(4.))
                .rounded(px(4.))
                .text_size(px(13.))
                .map(|d| match (&color, c.kind) {
                    (Some(cat), _) => d.bg(cat.bg).text_color(cat.text),
                    (None, ChipKind::Important) => d.bg(l.fill()).text_color(l.accent_text()).font_weight(FontWeight::BOLD),
                    _ => d.bg(l.fill()).text_color(l.text()),
                })
                .when(c.kind == ChipKind::Due, |d| d.child(icon(crate::calendar::g::CALENDAR, 11.)))
                .child(label)
                .child(
                    div()
                        .id(("unchip", i))
                        .size(px(16.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(3.))
                        .cursor_pointer()
                        .hover(|d| d.bg(l.hover()))
                        .child(icon(crate::calendar::g::CANCEL, 8.))
                        .on_click(cx.listener(move |this, _, _, cx| this.unchip(text.clone(), cx))),
                )
                .into_any_element()
        });
        let mut parts: Vec<String> = Vec::new();
        if let Some(d) = p.date {
            let day = crate::calendar::long_day(d);
            parts.push(match p.time {
                Some(t) => tr!("tasks-due-at", day = day, time = t.strftime("%H:%M").to_string()),
                None => day,
            });
        }
        if let Some(name) = p.category.as_ref().and_then(|k| self.categories.list.iter().find(|x| &x.keyword == k)).map(|x| x.name.clone()) {
            parts.push(name);
        }
        if p.important {
            parts.push(tr!("tasks-important"));
        }
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .pt(px(6.))
                .child(div().flex().flex_wrap().gap(px(4.)).children(chips))
                .child(div().flex().items_center().gap(px(6.)).pl(px(2.)).text_size(px(12.)).text_color(l.text2()).child(icon(g::CHECK, 11.)).child(parts.join(" · ")))
                .into_any_element(),
        )
    }

    fn card_pane(&self, t: &Task, cx: &mut Context<Self>) -> AnyElement {
        if let Some(Edit::Assign { reassign }) = self.deleg.edit {
            return self.assign_form(t, reassign, cx);
        }
        if let Some((_, st, owner)) = self.delegation().filter(|(d, _, _)| d.uid == t.uid) {
            return self.deleg_pane(t, st, owner, cx);
        }
        let l = crate::roles::look(cx);
        let done = t.progress.is_closed();
        let pending = self.completing.is_pending(&t.uid);
        let checked = done || pending;
        let letter = mailtask::is_letter(t);
        let kind = div()
            .flex()
            .items_center()
            .gap(px(5.))
            .h(px(22.))
            .px(px(8.))
            .rounded(px(11.))
            .bg(l.fill())
            .text_color(l.text())
            .whitespace_nowrap()
            .when(t.mail.is_some(), |d| d.child(icon(g::FLAG, 11.)))
            .child(tr!(if t.mail.is_some() { "tasks-kind-mail" } else { "tasks-kind-task" }));
        let where_ = t.project.clone().unwrap_or_else(|| self.title_of());
        let top = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .text_size(px(12.))
            .text_color(l.text2())
            .child(kind)
            .child(div().flex_1().min_w_0().truncate().child(where_))
            .when(done, |d| {
                let uid = t.uid.clone();
                d.child(
                    div()
                        .id("task-reopen")
                        .cursor_pointer()
                        .text_color(l.accent_text())
                        .child(tr!("tasks-reopen"))
                        .on_click(cx.listener(move |this, _, _, cx| this.toggle_done_of(&uid, cx))),
                )
            });
        let uid = t.uid.clone();
        let circle = div()
            .id("card-check")
            .mt(px(3.))
            .size(px(24.))
            .flex_none()
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .when(checked, |d| d.bg(if done { l.text3() } else { l.accent() }).text_color(l.on_accent()).child(icon(g::CHECK, 13.)))
            .when(!checked, |d| d.border_2().border_color(l.text3()).hover(|d| d.border_color(l.accent_text())))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_done_of(&uid, cx)));
        let title: AnyElement = match &self.title {
            Some(state) => div().flex_1().min_w_0().child(Input::new(state)).into_any_element(),
            None => div()
                .flex_1()
                .min_w_0()
                .text_size(px(20.))
                .font_weight(FontWeight::SEMIBOLD)
                .line_height(px(26.))
                .when(checked, |d| d.line_through().text_color(l.text3()))
                .child(t.title.clone())
                .into_any_element(),
        };
        let prio = div()
            .id("card-prio")
            .size(px(28.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .text_size(px(16.))
            .font_weight(FontWeight::BOLD)
            .when(t.important, |d| d.bg(l.picked()).text_color(l.accent_text()))
            .when(!t.important, |d| d.text_color(l.text3()))
            .when(!letter, |d| d.cursor_pointer().hover(|d| d.bg(l.hover())).on_click(cx.listener(|this, _, _, cx| this.toggle_important(cx))))
            .child("!");
        let head = div().pt(px(16.)).px(px(20.)).flex().flex_col().gap(px(10.)).flex_none().child(top).child(
            div().flex().items_start().gap(px(12.)).child(circle).child(title).child(prio),
        );
        // Письмо задачи.
        let mail = t.mail.as_ref().map(|_| {
            let row = mailtask::letter_key(t).and_then(|k| self.letter(k)).cloned();
            let from = row.as_ref().map(|r| if r.from_name.is_empty() { r.from_addr.clone() } else { r.from_name.clone() }).unwrap_or_else(|| tr!("tasks-letter"));
            let subject = row.as_ref().map(|r| r.subject.clone()).unwrap_or_default();
            let when = row.as_ref().map(|r| crate::calendar::hm(r.received_at, &self.tz)).unwrap_or_default();
            let has = row.is_some() || mailtask::letter_key(t).is_some();
            div()
                .id("card-mail")
                .mx(px(20.))
                .mt(px(12.))
                .flex_none()
                .flex()
                .gap(px(10.))
                .py(px(8.))
                .pr(px(10.))
                .rounded(px(6.))
                .border_1()
                .border_color(l.stroke())
                .when(has, |d| d.cursor_pointer().hover(|d| d.bg(l.hover())).on_click(cx.listener(|this, _, _, cx| this.open_letter(cx))))
                .child(div().w(px(4.)).rounded_r(px(2.)).bg(l.accent()))
                .child(icon(g::MAIL, 14.).mt(px(2.)).text_color(l.text2()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .child(div().flex().gap(px(8.)).child(div().flex_1().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD).child(from)).child(div().text_size(px(12.)).text_color(l.text2()).child(when)))
                        .child(div().truncate().text_color(l.text()).child(subject)),
                )
                .child(div().flex().items_center().gap(px(4.)).text_size(px(12.)).text_color(l.accent_text()).child(tr!("tasks-open")).child(icon(g::OPEN, 11.)))
        });
        // Поля.
        let field = |gl: char, key: &str, value: AnyElement| {
            div()
                .h(px(30.))
                .flex()
                .items_center()
                .gap(px(10.))
                .child(icon(gl, 14.).text_color(l.text2()))
                .child(div().w(px(88.)).flex_none().text_size(px(12.)).text_color(l.text2()).child(tr!(key)))
                .child(value)
        };
        let muted = |key: &str| div().text_color(l.text3()).child(tr!(key)).into_any_element();
        let due_value = match &t.due {
            Some(_) => {
                let badge = (!checked).then(|| due_badge(t, self.now(), &self.tz)).flatten();
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().truncate().child(due_text(t, &self.tz)))
                    .children(badge.map(|b| crate::lifecycle::chip(&b, &l)))
                    .into_any_element()
            }
            None => muted(if t.someday { "tasks-someday" } else { "tasks-no-due" }),
        };
        let due_row = div()
            .id("card-due")
            .rounded(px(4.))
            .cursor_pointer()
            .hover(|d| d.bg(l.hover()))
            .child(field(g::FLAG, "tasks-field-due", due_value))
            .on_click(cx.listener(|this, _, window, cx| this.toggle_due(window, cx)));
        let due_opts = self.due_open.then(|| {
            let now = jiff::Zoned::now().with_time_zone(self.tz.clone());
            let mut opts: Vec<AnyElement> = cycle::picks(Kind::Due)
                .into_iter()
                .enumerate()
                .map(|(i, p): (usize, Pick)| {
                    let when = p.when(Kind::Due, &now);
                    div()
                        .id(("due-opt", i))
                        .h(px(26.))
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .rounded(px(13.))
                        .text_size(px(12.))
                        .border_1()
                        .border_color(l.stroke())
                        .cursor_pointer()
                        .hover(|d| d.bg(l.hover()))
                        .child(crate::lifecycle::pick_text(p, Kind::Due))
                        .on_click(cx.listener(move |this, _, _, cx| this.set_due(when, cx)))
                        .into_any_element()
                })
                .collect();
            opts.push(
                div()
                    .id("due-none")
                    .h(px(26.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .rounded(px(13.))
                    .text_size(px(12.))
                    .text_color(l.text2())
                    .cursor_pointer()
                    .hover(|d| d.bg(l.hover()))
                    .child(tr!("tasks-no-due"))
                    .on_click(cx.listener(|this, _, _, cx| this.set_due(None, cx)))
                    .into_any_element(),
            );
            div().flex().flex_wrap().gap(px(6.)).pl(px(26.)).pt(px(4.)).pb(px(8.)).children(opts)
        });
        let editable = !letter && self.draft.as_ref().is_none_or(|d| d.uid != t.uid);
        let remind = match t.alerts.first().map(|a| &a.trigger) {
            None => muted("tasks-no-remind"),
            Some(tr) => div().child(remind_text(tr)).into_any_element(),
        };
        let repeat = match recur::rule_of(t) {
            Some(r) => div().child(repeat_text(&r)).into_any_element(),
            None => muted("tasks-no-repeat"),
        };
        let project = match &t.project {
            Some(p) => {
                let pr = model::projects(&self.tasks).into_iter().find(|x| x.name.eq_ignore_ascii_case(p));
                let (d, n) = pr.map(|x| (x.done, x.total)).unwrap_or((0, 0));
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .min_w_0()
                    .child(progress_ring(d, n, 12., &l))
                    .child(div().truncate().child(p.clone()))
                    .child(div().text_size(px(12.)).text_color(l.text2()).child(tr!("tasks-progress", done = d as i64, total = n as i64)))
                    .into_any_element()
            }
            None => muted("tasks-no-project"),
        };
        let category = match model::category(t, &self.categories) {
            Some(c) => crate::labels::chip(c.name.clone(), &c.color, &l).into_any_element(),
            None => muted("tasks-no-category"),
        };
        let clickable = |id: &'static str, f: Field, row: Div, cx: &mut Context<Self>| {
            div()
                .id(id)
                .rounded(px(4.))
                .when(editable, |d| d.cursor_pointer().hover(|d| d.bg(l.hover())).on_click(cx.listener(move |this, _, _, cx| this.toggle_field(f, cx))))
                .child(row)
        };
        let pill = |id: ElementId, text: String, on: bool| {
            div()
                .id(id)
                .h(px(26.))
                .px(px(10.))
                .flex()
                .items_center()
                .rounded(px(13.))
                .text_size(px(12.))
                .border_1()
                .border_color(if on { l.accent_text() } else { l.stroke() })
                .when(on, |d| d.bg(l.picked()))
                .cursor_pointer()
                .hover(|d| d.bg(l.hover()))
                .child(text)
        };
        let options = |items: Vec<AnyElement>| div().flex().flex_wrap().gap(px(6.)).pl(px(26.)).pt(px(4.)).pb(px(8.)).children(items);
        let remind_opts = (self.open == Some(Field::Remind)).then(|| {
            let cur = t.alerts.first().and_then(|a| match a.trigger {
                Trigger::Offset { secs, from: Anchor::Due } => Some(secs),
                _ => None,
            });
            // По умолчанию — из «Настройки → Календарь» (S5-20), первым.
            let default = -(i64::from(crate::prefs::get(cx).calendar.reminder) * 60);
            let mut offsets = vec![default];
            offsets.extend(REMINDERS.iter().copied().filter(|s| *s != default));
            let mut items: Vec<AnyElement> = offsets
                .iter()
                .enumerate()
                .map(|(i, secs)| {
                    let secs = *secs;
                    pill(("remind-opt", i).into(), remind_text(&Trigger::Offset { secs, from: Anchor::Due }), cur == Some(secs))
                        .on_click(cx.listener(move |this, _, _, cx| this.set_remind(Some(secs), cx)))
                        .into_any_element()
                })
                .collect();
            items.push(pill("remind-none".into(), tr!("tasks-no-remind"), cur.is_none()).on_click(cx.listener(|this, _, _, cx| this.set_remind(None, cx))).into_any_element());
            options(items)
        });
        let repeat_opts = (self.open == Some(Field::Repeat)).then(|| {
            let cur = recur::rule_of(t);
            let weekday = t.due.as_ref().and_then(|d| d.date_in(&self.tz)).map(|d| d.weekday());
            let choices: Vec<Option<Rule>> = vec![
                Some(Rule::Every { freq: Freq::Daily, interval: 1 }),
                Some(Rule::Every { freq: Freq::Weekdays, interval: 1 }),
                Some(Rule::Every { freq: Freq::Weekly(weekday.into_iter().collect()), interval: 1 }),
                Some(Rule::Every { freq: Freq::Monthly(0), interval: 1 }),
                Some(Rule::Every { freq: Freq::Yearly, interval: 1 }),
                Some(Rule::AfterDone(1)),
                Some(Rule::AfterDone(7)),
                None,
            ];
            let items: Vec<AnyElement> = choices
                .into_iter()
                .enumerate()
                .map(|(i, r)| {
                    let text = r.as_ref().map(repeat_text).unwrap_or_else(|| tr!("tasks-no-repeat"));
                    let on = match (&r, &cur) {
                        (None, None) => true,
                        (Some(a), Some(b)) => a == b,
                        _ => false,
                    };
                    pill(("repeat-opt", i).into(), text, on).on_click(cx.listener(move |this, _, _, cx| this.set_repeat(r.clone(), cx))).into_any_element()
                })
                .collect();
            options(items)
        });
        let project_opts = (self.open == Some(Field::Project)).then(|| {
            let mut items: Vec<AnyElement> = model::projects(&self.tasks)
                .into_iter()
                .enumerate()
                .map(|(i, p)| {
                    let on = t.project.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(&p.name));
                    let name = p.name.clone();
                    pill(("proj-opt", i).into(), p.name, on).on_click(cx.listener(move |this, _, _, cx| this.set_project(Some(name.clone()), cx))).into_any_element()
                })
                .collect();
            items.push(pill("proj-none".into(), tr!("tasks-no-project"), t.project.is_none()).on_click(cx.listener(|this, _, _, cx| this.set_project(None, cx))).into_any_element());
            items.push(div().w(px(180.)).child(Input::new(&self.project_input)).into_any_element());
            options(items)
        });
        let category_opts = (self.open == Some(Field::Category)).then(|| {
            let cur = model::category(t, &self.categories).map(|c| c.keyword.clone());
            let mut items: Vec<AnyElement> = self
                .categories
                .list
                .iter()
                .filter(|c| !c.hidden)
                .enumerate()
                .map(|(i, c)| {
                    let kw = c.keyword.clone();
                    let on = cur.as_deref() == Some(c.keyword.as_str());
                    let dot = div().size(px(8.)).mr(px(6.)).rounded(px(2.)).bg(l.cat(&c.color).dot);
                    pill(("cat-opt", i).into(), String::new(), on)
                        .child(dot)
                        .child(c.name.clone())
                        .on_click(cx.listener(move |this, _, _, cx| this.set_category(Some(kw.clone()), cx)))
                        .into_any_element()
                })
                .collect();
            items.push(pill("cat-none".into(), tr!("tasks-no-category"), cur.is_none()).on_click(cx.listener(|this, _, _, cx| this.set_category(None, cx))).into_any_element());
            options(items)
        });
        let fields = div()
            .mx(px(20.))
            .mt(px(10.))
            .pb(px(8.))
            .flex_none()
            .flex()
            .flex_col()
            .border_b_1()
            .border_color(l.border())
            .child(due_row)
            .children(due_opts)
            .child(clickable("card-remind", Field::Remind, field(g::RINGER, "tasks-field-remind", remind), cx))
            .children(remind_opts)
            .child(clickable("card-repeat", Field::Repeat, field(g::REPEAT, "tasks-field-repeat", repeat), cx))
            .children(repeat_opts)
            .child(clickable("card-project", Field::Project, field(g::FOLDER, "tasks-field-project", project), cx))
            .children(project_opts)
            .child(clickable("card-category", Field::Category, field(g::TAG, "tasks-field-category", category), cx))
            .children(category_opts);
        // Время в календаре (S6-08).
        let block = editable.then(|| self.block_view(t, &l, cx));
        // Подзадачи.
        let subs = model::subtasks(&self.tasks, &t.uid);
        let (sd, sn) = model::subtask_progress(&self.tasks, &t.uid);
        let bar = (sn > 0).then(|| {
            div().flex_1().h(px(3.)).rounded(px(2.)).bg(l.fill()).flex().child(div().h_full().rounded(px(2.)).bg(l.accent()).w(relative(sd as f32 / sn as f32)))
        });
        let sub_rows: Vec<AnyElement> = subs
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let on = s.progress.is_closed();
                let suid = s.uid.clone();
                div()
                    .id(("sub", i))
                    .h(px(25.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .cursor_pointer()
                    .child(
                        div()
                            .size(px(16.))
                            .flex_none()
                            .rounded(px(4.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(on, |d| d.bg(l.accent()).text_color(l.on_accent()).child(icon(g::CHECK, 9.)))
                            .when(!on, |d| d.border_1().border_color(l.text3())),
                    )
                    .child(div().flex_1().min_w_0().truncate().when(on, |d| d.line_through().text_color(l.text3())).child(s.title.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_sub(suid.clone(), cx)))
                    .into_any_element()
            })
            .collect();
        let subtasks = div()
            .mx(px(20.))
            .pt(px(10.))
            .pb(px(6.))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(2.))
            .border_b_1()
            .border_color(l.border())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .mb(px(4.))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(tr!("tasks-subtasks")))
                    .when(sn > 0, |d| d.child(div().text_size(px(12.)).text_color(l.text2()).child(format!("{sd}/{sn}"))))
                    .children(bar),
            )
            .children(sub_rows)
            .when(editable, |d| d.child(div().pt(px(4.)).child(Input::new(&self.sub_input).prefix(icon(g::ADD, 12.).text_color(l.text3())))))
            .when(sn == 0 && !editable, |d| d.child(div().h(px(25.)).flex().items_center().text_color(l.text3()).child(tr!("tasks-no-subtasks"))));
        // Заметки — Markdown с живым отображением (S6-06): то же поле, что текст письма.
        let notes = div()
            .mx(px(20.))
            .pt(px(10.))
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(6.))
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(tr!("tasks-notes")))
                    .child(div().px(px(6.)).rounded(px(4.)).border_1().border_color(l.stroke()).text_size(px(11.)).text_color(l.text2()).child("Markdown")),
            )
            .child(if editable {
                div().flex_1().min_h(px(60.)).child(Editor::new(&self.notes).appearance(false).font_family(self.font.clone()).text_size(px(13.)).line_height(relative(1.45)).h(relative(1.))).into_any_element()
            } else if t.notes.trim().is_empty() {
                div().text_color(l.text3()).child(tr!("tasks-no-notes")).into_any_element()
            } else {
                div().line_height(px(19.)).text_color(l.text()).children(t.notes.lines().map(|line| div().child(line.to_string()))).into_any_element()
            });
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(head)
            .children(mail)
            .child(fields)
            .children(block)
            .child(subtasks)
            .child(notes)
            .into_any_element()
    }
}

impl Render for TasksView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.font = window.text_style().font_family.clone();
        self.sync_notes(window, cx);
        let l = crate::roles::look(cx);
        let pane = match self.selected_task().cloned() {
            Some(t) => self.card_pane(&t, cx),
            None => div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .text_color(l.text2())
                .child(icon(g::TASKS, 28.).text_color(l.text3()))
                .child(tr!("tasks-pick"))
                .into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .min_w_0()
            .text_size(px(13.))
            .child(self.nav_view(cx))
            .child(self.list_card(cx))
            .child(crate::mailer::card(&l).w(px(CARD_W)).flex_none().child(pane))
    }
}

/// Кольцо прогресса проекта: доля выполненного — дугой заливки (кругом внутри, по площади).
fn progress_ring(done: usize, total: usize, size: f32, l: &Look) -> Div {
    let share = if total == 0 { 0.0 } else { done as f32 / total as f32 };
    let inner = (size - 4.) * share.sqrt();
    div()
        .size(px(size))
        .flex_none()
        .rounded_full()
        .border_1()
        .border_color(l.text2())
        .flex()
        .items_center()
        .justify_center()
        .when(share > 0.0, |d| d.child(div().size(px(inner.max(2.))).rounded_full().bg(l.text2())))
}

/// Метка срока, как у письма (`lifecycle::badges` по слову срока задачи): «До чт», «Просрочено 1 д».
pub(crate) fn due_badge(t: &Task, now: i64, tz: &TimeZone) -> Option<Badge> {
    let when = mailtask::due_when(t, tz)?;
    let kws = vec![marks::DUE.to_owned(), Kind::Due.keyword(when)];
    let now = jiff::Timestamp::from_second(now).ok()?.to_zoned(tz.clone());
    cycle::badges(&kws, &now).into_iter().find(|b| matches!(b, Badge::Due(_) | Badge::Overdue(_)))
}

/// «В срок», «За 15 мин», «За 1 ч», «За 1 д», «В 12:00».
fn remind_text(t: &Trigger) -> String {
    match *t {
        Trigger::Offset { secs: 0, .. } => tr!("tasks-remind-at-due"),
        Trigger::Offset { secs, .. } if secs <= -86_400 && secs % 86_400 == 0 => tr!("tasks-remind-days", n = -secs / 86_400),
        Trigger::Offset { secs, .. } if secs <= -3600 && secs % 3600 == 0 => tr!("tasks-remind-hours", n = -secs / 3600),
        Trigger::Offset { secs, .. } => tr!("tasks-remind-minutes", n = (-secs / 60).max(0)),
        Trigger::At(ts) => tr!("tasks-remind-at", when = crate::calendar::hm(ts, &TimeZone::system())),
    }
}

/// «Каждый день», «По будням», «Каждую неделю: пн, чт», «Каждый месяц, 20-го», «Через 7 д после выполнения».
fn repeat_text(r: &Rule) -> String {
    match r {
        Rule::AfterDone(n) => tr!("tasks-repeat-after", n = i64::from(*n)),
        Rule::Other(_) => tr!("tasks-repeat-other"),
        Rule::Every { freq, interval } => {
            let n = i64::from(*interval);
            match freq {
                Freq::Daily if n == 1 => tr!("tasks-repeat-daily"),
                Freq::Daily => tr!("tasks-repeat-days", n = n),
                Freq::Weekdays => tr!("tasks-repeat-weekdays"),
                Freq::Weekly(days) => {
                    let names: Vec<String> = days.iter().map(|w| tr!("cal-weekday", d = w.to_monday_zero_offset())).collect();
                    let base = if n == 1 { tr!("tasks-repeat-weekly") } else { tr!("tasks-repeat-weeks", n = n) };
                    if names.is_empty() { base } else { format!("{base}: {}", names.join(", ")) }
                }
                Freq::Monthly(0) => if n == 1 { tr!("tasks-repeat-monthly") } else { tr!("tasks-repeat-months", n = n) },
                Freq::Monthly(d) => tr!("tasks-repeat-monthly-day", day = i64::from(*d)),
                Freq::Yearly => tr!("tasks-repeat-yearly"),
            }
        }
    }
}

/// «15:00–16:00»; не сегодня — с днём: «Пятница, 23 октября, 09:00–09:30».
fn slot_label(s: Span, tz: &TimeZone) -> String {
    let time = format!("{}–{}", crate::calendar::hm(s.start, tz), crate::calendar::hm(s.end, tz));
    let today = day_of(jiff::Timestamp::now().as_second(), tz);
    match day_of(s.start, tz) {
        Some(d) if Some(d) != today => format!("{}, {time}", crate::calendar::long_day(d)),
        _ => time,
    }
}

/// «Чт 22.10, до 18:00», «Сегодня», «Завтра, до 10:00».
pub(crate) fn due_text(t: &Task, tz: &TimeZone) -> String {
    let Some(w) = mailtask::due_when(t, tz) else { return String::new() };
    let day = crate::calendar::long_day(w.date);
    match w.time {
        Some(time) => tr!("tasks-due-at", day = day, time = time.strftime("%H:%M").to_string()),
        None => day,
    }
}

fn list_key(l: List) -> &'static str {
    match l {
        List::Inbox => "tasks-inbox",
        List::Today => "tasks-today",
        List::Upcoming => "tasks-upcoming",
        List::FromMail => "tasks-from-mail",
        List::Someday => "tasks-someday",
        List::Done => "tasks-done-list",
    }
}

fn empty_key(nav: &Nav) -> &'static str {
    match nav {
        Nav::List(List::Inbox) => "tasks-empty-inbox",
        Nav::List(List::Today) => "tasks-empty-today",
        Nav::List(List::Upcoming) => "tasks-empty-upcoming",
        Nav::List(List::FromMail) => "tasks-empty-mail",
        Nav::List(List::Someday) => "tasks-empty-someday",
        Nav::List(List::Done) => "tasks-empty-done",
        Nav::Delegations(Tab::ByMe) => "deleg-empty-by-me",
        Nav::Delegations(Tab::ToMe) => "deleg-empty-to-me",
        _ => "tasks-empty",
    }
}

fn group_label(g: Group) -> String {
    match g {
        Group::Plain => String::new(),
        Group::Overdue => tr!("tasks-group-overdue"),
        Group::TodayTimed | Group::Today | Group::DoneToday => tr!("tasks-group-today"),
        Group::NoTime => tr!("tasks-group-no-time"),
        Group::Tomorrow => tr!("tasks-group-tomorrow"),
        Group::Day(d) => crate::calendar::long_day(d),
        Group::Soon => tr!("tasks-group-soon"),
        Group::Later => tr!("tasks-group-later"),
        Group::NoDue => tr!("tasks-group-no-due"),
        Group::DoneYesterday => tr!("tasks-group-yesterday"),
        Group::DoneEarlier => tr!("tasks-group-earlier"),
        Group::Deleg(b) => tr!(match b {
            Bucket::Review => "deleg-group-review",
            Bucket::InWork => "deleg-group-work",
            Bucket::Waiting => "deleg-group-waiting",
            Bucket::New => "deleg-group-new",
            Bucket::Checking => "deleg-group-checking",
        }),
    }
}

fn group_sub(g: Group) -> Option<String> {
    match g {
        Group::TodayTimed => Some(tr!("tasks-group-timed")),
        Group::Deleg(Bucket::Review) => Some(tr!("deleg-group-review-sub")),
        Group::Deleg(Bucket::New) => Some(tr!("deleg-group-new-sub")),
        Group::Deleg(Bucket::Checking) => Some(tr!("deleg-group-checking-sub")),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------------------------

/// Командная строка раздела: «Новая задача», «Выполнить» / «Вернуть», «Срок», «Изменить», «Удалить», поиск.
pub(crate) struct TasksBar {
    tasks: Entity<TasksView>,
    _subscription: Subscription,
}

impl TasksBar {
    pub(crate) fn new(tasks: Entity<TasksView>, cx: &mut Context<Self>) -> Self {
        let sub = cx.observe(&tasks, |_, _, cx| cx.notify());
        Self { tasks, _subscription: sub }
    }
}

impl Render for TasksBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let l = crate::roles::look(cx);
        let v = self.tasks.read(cx);
        let sel = v.selected_task();
        let has = sel.is_some();
        let done = sel.is_some_and(|t| t.progress.is_closed());
        let editable = sel.is_some_and(|t| !mailtask::is_letter(t));
        let deleg = v.delegation();
        let me_owner = deleg.as_ref().is_some_and(|(_, _, o)| *o);
        let can = |a: postman_calendar::assign::Action| deleg.as_ref().is_some_and(|(_, st, o)| *o && postman_calendar::assign::next(*st, a).is_ok());
        let can_delegate = editable && (deleg.is_none() || can(postman_calendar::assign::Action::Reassign));
        let (can_remind, can_revoke) = (can(postman_calendar::assign::Action::Remind), can(postman_calendar::assign::Action::Revoke));
        let search_state = v.search.clone();
        use crate::tasks::task as cmd;
        let action = |id: &'static str, glyph: char, text: String, command: &'static str, on: bool, act: fn() -> Box<dyn Action>, cx: &App| {
            let b = crate::chrome::quiet_button(id, &l)
                .child(icon(glyph, 16.))
                .child(text.clone())
                .when(!on, |d| d.opacity(0.45))
                .on_click(move |_, window, cx| window.dispatch_action(act(), cx));
            crate::keytips::command(b, command, Some(text), &l, cx)
        };
        let new = div()
            .id("task-new")
            .h(px(32.))
            .px(px(12.))
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
            .child(tr!("tasks-new"))
            .on_click(|_, window, cx| window.dispatch_action(Box::new(cmd::New), cx));
        let new = crate::keytips::command(new, "task.new", Some(tr!("tasks-new")), &l, cx);
        let search = Input::new(&search_state).prefix(icon(g::SEARCH, 14.).text_color(l.text3()));
        div()
            .size_full()
            .flex()
            .items_center()
            .gap(px(2.))
            .px(px(12.))
            .text_color(l.text())
            .child(new)
            .child(action("task-complete", g::DONE, tr!(if done { "tasks-reopen" } else { "tasks-complete" }), "task.complete", has, || Box::new(cmd::Complete), cx))
            .child(action("task-due", g::FLAG, tr!("tasks-field-due"), "task.due", has, || Box::new(cmd::Due), cx))
            .child(div().w(px(1.)).h(px(20.)).mx(px(8.)).bg(l.stroke()))
            .child(action("task-edit", g::EDIT, tr!("tasks-edit"), "task.edit", editable, || Box::new(cmd::Edit), cx))
            .child(action("task-delete", g::DELETE, tr!("tasks-delete"), "task.delete", has, || Box::new(cmd::Delete), cx))
            .child(div().w(px(1.)).h(px(20.)).mx(px(8.)).bg(l.stroke()))
            .child(action("task-delegate", g::DELEGATE, tr!("tasks-delegate"), "task.delegate", can_delegate, || Box::new(cmd::Delegate), cx))
            .when(me_owner, |d| {
                d.child(action("task-remind", g::RINGER, tr!("tasks-remind"), "task.remind", can_remind, || Box::new(cmd::Remind), cx))
                    .child(action("task-revoke", g::REVOKE, tr!("tasks-revoke"), "task.revoke", can_revoke, || Box::new(cmd::Revoke), cx))
            })
            .child(div().flex_1())
            .child(crate::keytips::badge(div().ml(px(12.)).w(px(300.)).flex_none().child(search), crate::keytips::key("search.focus", cx), &l, cx))
    }
}

impl crate::Mailer {
    /// Просьба раздела «Задачи»: команда синхронизации, правка писем, письмо, тост.
    pub(crate) fn on_tasks(&mut self, action: &TasksAction, cx: &mut Context<Self>) {
        match action {
            TasksAction::Send(r) => {
                if self.backend.is_live() && !self.backend.send(postman_core::sync::Command::Tasks(r.clone())) {
                    self.sync_stopped(cx);
                }
            }
            TasksAction::Patch(p) => self.apply_patches(p.clone(), cx),
            TasksAction::OpenLetter(key) => {
                let rows = self.cache.rows_by_ids(std::slice::from_ref(key)).unwrap_or_default();
                match rows.into_iter().next() {
                    Some(row) => self.open_letter_tab(row, cx),
                    None => self.toasts.update(cx, |t, cx| t.push(Toast::Info, tr!("tasks-letter-gone"), cx)),
                }
            }
            TasksAction::Toast(level, text) => self.toasts.update(cx, |t, cx| t.push(*level, text.clone(), cx)),
            TasksAction::Delegate { uid, drafts, hold } => {
                use postman_core::{send_later::UNDO_HOLD_SECS, sync::Release};
                let ticket = jiff::Timestamp::now().as_millisecond() as u64;
                let release = if *hold { Release::After(UNDO_HOLD_SECS) } else { Release::Now };
                let cmd = postman_core::sync::Command::Delegate { uid: uid.clone(), drafts: drafts.clone(), ticket, release };
                if self.backend.is_live() && !self.backend.send(cmd) {
                    self.sync_stopped(cx);
                }
            }
            TasksAction::Withhold(scheduled) => {
                let ticket = jiff::Timestamp::now().as_millisecond() as u64;
                let cmd = postman_core::sync::Command::Withhold { scheduled: scheduled.clone(), ticket };
                if self.backend.is_live() && !self.backend.send(cmd) {
                    self.sync_stopped(cx);
                }
            }
            TasksAction::Waiting(words) => {
                let words = words.clone();
                self.list.update(cx, |l, cx| l.set_waiting(words, cx));
            }
            TasksAction::OpenWaiting => self.open_waiting(cx),
            TasksAction::NeedBook => {
                let book = self.compose.book.clone();
                self.tasks.update(cx, |t, _| t.set_book(book));
            }
            TasksAction::Remind { uid, title } => {
                let view = self.tasks.downgrade();
                let id = uid.clone();
                let act = pets_ui::ToastAction::new(tr!("tasks-snooze"), move |_, cx| {
                    let _ = view.update(cx, |v, cx| v.snooze(id.clone(), Snooze::Minutes10, cx));
                });
                let text = tr!("tasks-remind-toast", title = title.clone());
                self.toasts.update(cx, |t, cx| t.push_action(Toast::Info, text.clone(), act, cx));
                // Окно свёрнуто или не в фокусе — ещё и уведомление Windows (решение 7).
                let active = self.window.update(cx, |_, window, _| window.is_window_active()).unwrap_or(true);
                if !active && let Err(e) = crate::sys::shell::notify(&tr!("tasks-remind-title"), title) {
                    tracing::warn!("task reminder notification: {e}");
                }
            }
            TasksAction::Undo { text, undo } => {
                let view = self.tasks.downgrade();
                let undo = undo.clone();
                let act = pets_ui::ToastAction::new(tr!("bulk-undo"), move |_, cx| {
                    let undo = undo.clone();
                    let _ = view.update(cx, |v, cx| v.undo(undo, cx));
                });
                self.toasts.update(cx, |t, cx| t.push_action(Toast::Success, text.clone(), act, cx));
            }
        }
    }
}

impl crate::Mailer {
    /// «Открыть в почте» в «Поручениях»: почта, все папки, быстрый фильтр «Ждём» (письма поручений — в
    /// «Отправленных»).
    pub(crate) fn open_waiting(&mut self, cx: &mut Context<Self>) {
        use postman_core::filters::Quick;
        self.set_section(crate::mailer::Section::Mail, cx);
        self.filters.update(cx, |f, cx| {
            f.change(cx, |q| {
                q.scope = crate::filters::Where::All;
                q.criteria.quick.insert(Quick::Waiting);
            })
        });
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Демо: задачи по макету с вымышленными названиями (сроки — от сегодня).

pub(crate) fn demo(now: i64, tz: &TimeZone) -> Vec<Task> {
    use postman_demo::tasks as d;
    let today = day_of(now, tz).unwrap_or_default();
    let mut out: Vec<Task> = Vec::new();
    for (n, x) in d::OPEN.iter().enumerate() {
        let mut t = Task::new(&format!("demo-{n}@postman"), "demo", x.title);
        t.keywords = x.category.map(|c| vec![format!("pm.cat.{c}")]).unwrap_or_default();
        t.project = x.project.map(str::to_owned);
        if let Some((days, hour)) = x.due {
            let day = add_days(today, days);
            t.due = Some(Moment::new(day.at(hour.unwrap_or(0), 0, 0, 0), None));
            t.all_day = hour.is_none();
        }
        t.important = x.important;
        t.someday = x.title == d::SOMEDAY;
        if x.title == d::REPEAT {
            t.recurrence = Some(serde_json::json!({ "frequency": "monthly" }));
        }
        if x.title == d::MAIN {
            t.notes = d::NOTES.to_owned();
            t.alerts = vec![postman_calendar::Alert {
                trigger: postman_calendar::Trigger::Offset { secs: -1800, from: postman_calendar::Anchor::Due },
                email: false,
            }];
            for (i, (title, on)) in d::SUBTASKS.iter().enumerate() {
                let mut s = Task::new(&format!("demo-sub-{i}@postman"), "demo", title);
                s.parent = Some(t.uid.clone());
                s.order = Some(i as i64);
                if *on {
                    s.set_done(true, now - 7200);
                }
                out.push(s);
            }
        }
        out.push(t);
    }
    for (n, (title, cat, project, hours)) in d::DONE.iter().enumerate() {
        let mut t = Task::new(&format!("demo-done-{n}@postman"), "demo", title);
        t.keywords = cat.map(|c| vec![format!("pm.cat.{c}")]).unwrap_or_default();
        t.project = project.map(str::to_owned);
        t.set_done(true, now - hours * 3600);
        out.push(t);
    }
    // Поручения (S6-10): я поручил и мне поручили — все статусы.
    out.extend(postman_demo::delegations::all(today, now));
    out
}

/// Опись части (ADR 0005, `parts.rs`).
pub(crate) const PART: crate::part::Part = crate::part::Part { name: "tasks", texts: TEXTS, commands: COMMANDS, handlers, scenes: scenes::SCENES, ..crate::part::Part::NONE };

/// Обработчики команд части — на корне экрана почты (`Mailer::render`).
fn handlers(root: Div, cx: &mut Context<crate::Mailer>) -> Div {
    root.on_action(cx.listener(|this, _: &task::New, window, cx| {
            if this.section != crate::Section::Tasks {
                this.set_section(crate::Section::Tasks, cx);
            }
            this.tasks.update(cx, |t, cx| t.new_task(window, cx));
        }))
        .on_action(cx.listener(|this, _: &task::Complete, _, cx| this.tasks.update(cx, |t, cx| t.toggle_done(cx))))
        .on_action(cx.listener(|this, _: &task::Due, window, cx| this.tasks.update(cx, |t, cx| t.toggle_due(window, cx))))
        .on_action(cx.listener(|this, _: &task::Delegate, window, cx| this.tasks.update(cx, |t, cx| t.delegate(window, cx))))
        .on_action(cx.listener(|this, _: &task::Remind, _, cx| this.tasks.update(cx, |t, cx| t.remind(cx))))
        .on_action(cx.listener(|this, _: &task::RemindAll, _, cx| this.tasks.update(cx, |t, cx| t.remind_all(cx))))
        .on_action(cx.listener(|this, _: &task::Revoke, _, cx| this.tasks.update(cx, |t, cx| t.revoke(cx))))
        .on_action(cx.listener(|this, _: &task::Edit, window, cx| this.tasks.update(cx, |t, cx| t.rename(window, cx))))
        .on_action(cx.listener(|this, _: &task::Delete, _, cx| this.tasks.update(cx, |t, cx| t.delete(cx))))
        .on_action(cx.listener(|this, _: &task::Open, _, cx| this.tasks.update(cx, |t, cx| t.open_letter(cx))))
        .on_action(cx.listener(|this, _: &task::Next, _, cx| this.tasks.update(cx, |t, cx| t.step(1, cx))))
        .on_action(cx.listener(|this, _: &task::Prev, _, cx| this.tasks.update(cx, |t, cx| t.step(-1, cx))))
        .on_action(cx.listener(|this, _: &task::SlotNext, _, cx| this.tasks.update(cx, |t, cx| t.step_slot(1, cx))))
        .on_action(cx.listener(|this, _: &task::SlotPrev, _, cx| this.tasks.update(cx, |t, cx| t.step_slot(-1, cx))))
        .on_action(cx.listener(|this, _: &task::Block, _, cx| this.tasks.update(cx, |t, cx| t.place_block(cx))))
}

/// Тексты части (`i18n/<язык>/<часть>.ftl`).
const TEXTS: &[pets_i18n::Resource] = &[
    ("ru", include_str!("../i18n/ru/tasks.ftl")),
    ("en", include_str!("../i18n/en/tasks.ftl")),
];

/// Задачи (S6-05): новая, выполнить / вернуть, срок, переименовать, удалить, открыть письмо, по списку.
pub(crate) mod task {
    gpui_kit::actions!(task, [New, Complete, Due, Edit, Delete, Open, Next, Prev, SlotNext, SlotPrev, Block, Delegate, Remind, RemindAll, Revoke]);
}

/// Команды части (`commands.rs`: контексты и `cmd`).
const COMMANDS: &[pets_commands::Command] = {
    use crate::commands::{TASKS_ANY, TASKS, cmd};
    &[
        // Задачи (S6-05, решение 6 S6): Ctrl+N — новая задача (в модуле; в почте — письмо: контекст глубже), Space —
        // выполнено / вернуть, Insert — срок (как в почте, S5-16), Del — удалить, F2 — переименовать, Enter — письмо
        // задачи, стрелки — по списку. Ctrl+Shift+K Outlook не берём (не в исключениях правила).
        cmd("task.new", &["ctrl-n"], TASKS_ANY),
        cmd("task.complete", &["space"], TASKS),
        cmd("task.due", &["insert"], TASKS),
        cmd("task.edit", &["f2"], TASKS),
        cmd("task.delete", &["delete"], TASKS),
        cmd("task.open", &["enter"], TASKS),
        cmd("task.next", &["down"], TASKS),
        cmd("task.prev", &["up"], TASKS),
        // «Время в календаре» (S6-08): ←/→ — соседнее свободное окно, как во встрече; поставить — кнопкой (Enter — письмо).
        cmd("task.slot_next", &["right"], TASKS),
        cmd("task.slot_prev", &["left"], TASKS),
        cmd("task.block", &[], TASKS),
        // Поручения (S6-10): «Поручить», «Напомнить», «Забрать» — кнопки командной строки, без клавиш (главное действие
        // поручения — Space, срок — Insert, как у задачи).
        cmd("task.delegate", &[], TASKS),
        cmd("task.remind", &[], TASKS),
        cmd("task.remind_all", &[], TASKS),
        cmd("task.revoke", &[], TASKS),
    ]
};

/// Сцены `--screens` задач и поручений (ADR 0005).
pub(crate) mod scenes {
    use gpui_kit::*;
    use postman_core::tasks::List;
    use postman_demo::delegations as d;

    use crate::screens::{self, Scene};
    use crate::{Mailer, Section};

    pub(crate) const SCENES: &[Scene] = &[
        // Модуль «Задачи» (S6-05): «Сегодня», карточка задачи с подзадачами и заметкой (демо).
        Scene::new("tasks", list, ready),
        // «Сегодня» с выбранной просроченной задачей: «Просрочено» сверху, метка срока в карточке.
        Scene::new("tasks-today", list, ready),
        // Проект с прогрессом слева и его задачи.
        Scene::new("tasks-project", list, ready),
        // «Выполненные»: сегодня, вчера.
        Scene::new("tasks-done", list, ready),
        // «Добавить задачу» с фразой (S6-04): распознанное — чипами, итог строкой.
        Scene::new("task-quickadd", card, ready),
        // Карточка задачи (S6-06): варианты проекта открыты, подзадачи с полем «Добавить подзадачу».
        Scene::new("task-card", card, ready),
        // Карточка задачи со ссылкой на письмо (блок письма с «Открыть»).
        Scene::new("task-card-mail", card, ready),
        // Заметки задачи — Markdown с живым отображением, фокус в поле.
        Scene::new("task-notes", card, ready),
        // «Время в календаре» (S6-08): блок поставлен в ближайшее свободное окно.
        Scene::new("task-timeblock", card, ready),
        // Поручения (S6-10): форма «Поручить» с выбранным исполнителем, наблюдателем и сроком.
        Scene::new("delegate", delegation, delegation_ready),
        // Поручено мне: карточка с «Принять» / «Предложить срок» / «Отклонить».
        Scene::new("delegation-in", delegation, delegation_ready),
        // Я поручил, готово — на проверку: ход, результат, лента шагов, «Принять результат».
        Scene::new("delegation-steps", delegation, delegation_ready),
        // Проект поручений демо: круги всех статусов в списке.
        Scene::new("delegation-statuses", delegation, delegation_ready),
        // «Поручения» → «Я поручил» (S6-11): группы, «Напомнить всем», «Только свои» снят — и где я наблюдатель.
        Scene::new("delegations-out", delegations, delegations_ready),
        // «Поручения» → «Мне поручили»: новые, в работе, на проверке.
        Scene::new("delegations-in", delegations, delegations_ready),
        // «Отправленные» с быстрым фильтром «Ждём»: письма моих невыполненных поручений и «Ждём ответа».
        Scene::new("quick-wait", quick_wait, quick_wait_ready),
        // Письмо-поручение: карточка поручения над текстом письма.
        Scene::new("delegation-mail", letter, letter_ready),
        // Повтор (S6-07): варианты повтора в карточке.
        Scene::new("task-repeat", card, ready),
    ];

    fn ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        m.section == Section::Tasks && m.tasks.read(cx).ready()
    }

    fn list(s: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.set_section(Section::Tasks, cx);
        m.tasks.update(cx, |t, cx| match s.name {
            "tasks-project" => t.set_nav(crate::tasks::Nav::Project(postman_demo::tasks::PROJECT.into()), cx),
            "tasks-done" => t.set_nav(crate::tasks::Nav::List(List::Done), cx),
            _ => {
                t.set_nav(crate::tasks::Nav::List(List::Today), cx);
                let title = if s.name == "tasks" { postman_demo::tasks::MAIN } else { postman_demo::tasks::OVERDUE };
                t.select_title(title, cx);
            }
        });
        Ok(())
    }

    fn card(s: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.set_section(Section::Tasks, cx);
        let first = m.list.read(cx).row(0).cloned();
        m.tasks.update(cx, |t, cx| {
            t.set_nav(crate::tasks::Nav::List(List::Today), cx);
            t.select_title(postman_demo::tasks::MAIN, cx);
            match s.name {
                "task-quickadd" => t.set_quick(postman_demo::tasks::QUICK, window, cx),
                "task-card" => t.toggle_field(crate::tasks::Field::Project, cx),
                "task-card-mail" => {
                    if let Some(row) = first {
                        t.attach_demo_letter(row, cx);
                    }
                }
                "task-notes" => t.focus_notes(window, cx),
                "task-repeat" => t.demo_repeat(cx),
                _ => t.demo_block(cx),
            }
        });
        Ok(())
    }

    fn delegation(s: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.set_section(Section::Tasks, cx);
        m.tasks.update(cx, |t, cx| match s.name {
            "delegate" => {
                t.set_nav(crate::tasks::Nav::List(List::Today), cx);
                t.select_title(d::TO_ASSIGN, cx);
                t.screen_form(window, cx);
            }
            _ => {
                t.set_nav(crate::tasks::Nav::Project(d::PROJECT.into()), cx);
                // Статусы — первая строка (отклонённое: «Поручить другому»); ход и лента — готовое на проверку.
                match s.name {
                    "delegation-in" => t.select_title(d::INCOMING, cx),
                    "delegation-steps" => t.select_title(d::REVIEW, cx),
                    _ => {}
                }
            }
        });
        Ok(())
    }

    fn delegation_ready(s: &Scene, m: &Mailer, cx: &App) -> bool {
        let t = m.tasks.read(cx);
        m.section == Section::Tasks && t.ready() && (s.name != "delegate" || t.screen_form_open())
    }

    fn delegations(s: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        use postman_core::assign::Tab;
        m.set_section(Section::Tasks, cx);
        m.tasks.update(cx, |t, cx| {
            if s.name == "delegations-out" {
                t.own_only = false;
                t.set_tab(Tab::ByMe, cx);
                t.select_title(d::REVIEW, cx);
            } else {
                t.set_tab(Tab::ToMe, cx);
                t.select_title(d::INCOMING, cx);
            }
        });
        Ok(())
    }

    fn delegations_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        let t = m.tasks.read(cx);
        m.section == Section::Tasks && t.ready() && matches!(t.nav, crate::tasks::Nav::Delegations(_))
    }

    fn quick_wait(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        use postman_core::filters::Quick;
        m.set_section(Section::Mail, cx);
        m.open_mailbox("sent".into(), cx);
        m.filters.update(cx, |f, cx| {
            f.set(cx, |f| f.panel = false);
            f.change(cx, |q| {
                q.scope = crate::filters::Where::Folder;
                q.criteria.clear_panel();
                q.criteria.quick.clear();
                q.criteria.quick.insert(Quick::Waiting);
            });
        });
        Ok(())
    }

    fn quick_wait_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        let list = m.list.read(cx);
        list.query.scope == crate::filters::Where::Folder
            && list.query.criteria.quick.contains(&postman_core::filters::Quick::Waiting)
            && list.view.is_some()
            && !list.computing
            && (screens::rows_loaded(list) || list.count == 0)
    }

    fn todo(b: &postman_cache::BodyRow) -> bool {
        screens::has(b, "todo")
    }

    fn letter(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.set_section(Section::Mail, cx);
        let Some(i) = screens::demo_row(m.list.read(cx), todo) else {
            return Err("no delegation letter in the demo inbox".into());
        };
        m.list.update(cx, |l, cx| l.select(i, cx));
        Ok(())
    }

    fn letter_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        let target = screens::demo_id(m.list.read(cx), todo);
        target.is_some()
            && screens::reading(m, cx)
                .is_some_and(|r| Some(r.row.id.as_str()) == target && r.doc.is_some() && r.task_mail.is_some())
    }
}

#[cfg(test)]
mod tests {
    // Не `use super::*`: вместе с gpui пришёл бы атрибут `gpui::test`, он перекрывает `#[test]`.
    use super::{Badge, Completing, DONE_DELAY, Group, Item, List, Nav, demo, due_badge, groups, model, section};
    use jiff::tz::TimeZone;
    use std::time::{Duration, Instant};

    fn tz() -> TimeZone {
        TimeZone::get("Europe/Moscow").unwrap()
    }

    fn now() -> i64 {
        jiff::civil::date(2026, 10, 21).at(14, 5, 0, 0).to_zoned(tz()).unwrap().timestamp().as_second()
    }

    /// Выполнение: на месте 2 с, потом — выполнить; отмена до срока — ничего не было.
    #[test]
    fn completing_waits_two_seconds_and_cancels() {
        let mut c = Completing::default();
        let t0 = Instant::now();
        c.start("a", t0);
        c.start("b", t0 + Duration::from_millis(500));
        assert!(c.is_pending("a"));
        assert_eq!(c.due(t0 + Duration::from_millis(1900)), Vec::<String>::new());
        assert_eq!(c.next_in(t0 + Duration::from_millis(1900)), Some(Duration::from_millis(100)));
        assert!(c.cancel("b"), "отмена до срока");
        assert!(!c.cancel("b"));
        assert_eq!(c.due(t0 + DONE_DELAY), vec!["a".to_string()]);
        assert!(!c.is_pending("a") && c.next_in(t0).is_none());
    }

    fn heads(items: &[Item]) -> Vec<Group> {
        items.iter().filter_map(|i| if let Item::Head(g, _) = i { Some(*g) } else { None }).collect()
    }

    /// «Сегодня»: просроченные сверху, затем ко времени, затем без времени; выполняемая — на месте, не просрочена.
    #[test]
    fn today_groups_overdue_first() {
        let tasks = demo(now(), &tz());
        let nav = Nav::List(List::Today);
        let shown = section(&tasks, &[], &nav, &[], now(), &tz());
        let items = groups(&shown, &nav, &[], now(), &tz());
        assert_eq!(heads(&items), [Group::Overdue, Group::TodayTimed, Group::NoTime]);
        let first = items.iter().find_map(|i| if let Item::Row(r) = i { Some(*r) } else { None }).unwrap();
        assert!(model::is_overdue(&shown[first], now(), &tz()));
        // Выполняемая сейчас — в списке и не в «Просрочено».
        let over = shown[first].uid.clone();
        let shown2 = section(&tasks, &[], &nav, std::slice::from_ref(&over), now(), &tz());
        assert!(shown2.iter().any(|t| t.uid == over));
    }

    #[test]
    fn upcoming_done_project_and_category() {
        let tasks = demo(now(), &tz());
        let up = Nav::List(List::Upcoming);
        let shown = section(&tasks, &[], &up, &[], now(), &tz());
        let h = heads(&groups(&shown, &up, &[], now(), &tz()));
        assert_eq!(h.first(), Some(&Group::Tomorrow));
        assert!(h.contains(&Group::Later));
        let done = Nav::List(List::Done);
        let shown = section(&tasks, &[], &done, &[], now(), &tz());
        assert_eq!(heads(&groups(&shown, &done, &[], now(), &tz())), [Group::DoneToday, Group::DoneYesterday]);
        let p = Nav::Project(postman_demo::tasks::PROJECT.into());
        let shown = section(&tasks, &[], &p, &[], now(), &tz());
        assert_eq!(shown.len(), 3);
        let c = Nav::Category("pm.cat.tenders".into());
        assert_eq!(section(&tasks, &[], &c, &[], now(), &tz()).len(), 2);
        // Срок задачи — метка, как у письма.
        let over = tasks.iter().find(|t| model::is_overdue(t, now(), &tz())).unwrap();
        assert!(matches!(due_badge(over, now(), &tz()), Some(Badge::Overdue(_))));
    }
}
