//! «Правила» (S4-07, T2-50, макет `SettingsRules`): список правил почты по порядку (стрелки — порядок), имя,
//! условия «ЕСЛИ … ИЛИ … И …», несколько действий, «Остановить обработку», включить/выключить, удалить
//! (подтверждение строкой). Редактор — на месте строки. «Проверить на последних 100 письмах» и «Применять к
//! существующим — найдено N» — по письмам кэша (`postman_core::rulerun`, в фоне). «Выполнять на сервере, где
//! возможно»: выключено — правила выполняет Postman по новым письмам, пока запущен (`rules_run.rs`); их срабатывания
//! — в «Журнале срабатываний». Пересылка выполняется только сервером.
//!
//! Правила хранит сервер (Sieve, `postman_core::rules`); каждое изменение сразу уходит на сервер целым списком,
//! сервер проверяет скрипт (`SieveScript/validate`) — ошибка видна в панели. Скрипты сервера, написанные не в
//! Postman, — только текстом; заменить их можно лишь подтверждением «Заменить».

use std::rc::Rc;

use gpui_kit::component::input::{Input, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::{Metric, Role};
use postman_cache::{Cache, EmailRow};
use postman_core::rulerun;
use postman_core::rules::{Action, Cond, Field, Op, Rule, clean_name};
use postman_core::settings::RulesState;

use crate::buttons::plain as button;
use crate::chrome::{glyph, icon};
use crate::vacation::checkbox;

/// Действие в редакторе (без данных: папка и адрес — отдельно).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Kind {
    Move,
    MarkRead,
    Flag,
    Discard,
    Redirect,
    Category,
}

const FIELDS: [(Field, &str); 3] = [(Field::From, "rules-field-from"), (Field::To, "rules-field-to"), (Field::Subject, "rules-field-subject")];
const OPS: [(Op, &str); 4] = [
    (Op::Contains, "rules-op-contains"),
    (Op::NotContains, "rules-op-not-contains"),
    (Op::Domain, "rules-op-domain"),
    (Op::NotDomain, "rules-op-not-domain"),
];
const KINDS: [(Kind, &str); 6] = [
    (Kind::Move, "rules-kind-move"),
    (Kind::MarkRead, "rules-kind-read"),
    (Kind::Flag, "rules-kind-flag"),
    (Kind::Discard, "rules-kind-discard"),
    (Kind::Redirect, "rules-kind-redirect"),
    (Kind::Category, "rules-kind-category"),
];
/// Строк журнала видно без «Весь журнал».
const LOG_SHORT: usize = 5;
/// Писем из проверки в списке «показать».
const TEST_ROWS: usize = 8;

/// Проверки поля: у темы домена нет.
fn ops_for(f: Field) -> &'static [(Op, &'static str)] {
    if f == Field::Subject { &OPS[..2] } else { &OPS }
}

pub(crate) fn field_name(f: Field) -> String {
    FIELDS.iter().find(|(x, _)| *x == f).map(|(_, k)| tr!(k)).unwrap_or_default()
}

pub(crate) fn op_name(o: Op) -> String {
    OPS.iter().find(|(x, _)| *x == o).map(|(_, k)| tr!(k)).unwrap_or_default()
}

fn kind_name(k: Kind) -> String {
    KINDS.iter().find(|(x, _)| *x == k).map(|(_, key)| tr!(key)).unwrap_or_default()
}

/// Действие в редакторе: вид и его данные.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ActDraft {
    pub(crate) kind: Kind,
    /// Папка: (id, имя).
    pub(crate) folder: Option<(String, String)>,
    pub(crate) address: String,
    /// Категория: ключевое слово (S5-26).
    pub(crate) category: Option<String>,
}

impl ActDraft {
    fn new(kind: Kind, folder: Option<(String, String)>) -> Self {
        ActDraft { kind, folder, address: String::new(), category: None }
    }

    fn of(a: &Action) -> Self {
        let mut d = ActDraft::new(Kind::Move, None);
        d.kind = match a {
            Action::Move { mailbox_id, name } => {
                d.folder = Some((mailbox_id.clone(), name.clone()));
                Kind::Move
            }
            Action::MarkRead => Kind::MarkRead,
            Action::Flag => Kind::Flag,
            Action::Discard => Kind::Discard,
            Action::Redirect(to) => {
                d.address = to.clone();
                Kind::Redirect
            }
            Action::Category(keyword) => {
                d.category = Some(keyword.clone());
                Kind::Category
            }
        };
        d
    }

    fn action(&self) -> Result<Action, DraftError> {
        Ok(match self.kind {
            Kind::Move => {
                let (mailbox_id, name) = self.folder.clone().ok_or(DraftError::Folder)?;
                Action::Move { mailbox_id, name }
            }
            Kind::MarkRead => Action::MarkRead,
            Kind::Flag => Action::Flag,
            Kind::Discard => Action::Discard,
            Kind::Redirect => {
                let to = self.address.trim();
                if !is_address(to) {
                    return Err(DraftError::Address);
                }
                Action::Redirect(to.to_owned())
            }
            Kind::Category => Action::Category(self.category.clone().ok_or(DraftError::Category)?),
        })
    }
}

/// Похоже на адрес: `user@host.zone` без пробелов (адрес пересылки сервер не проверяет, `docs/STALWART.md`).
fn is_address(to: &str) -> bool {
    to.split_once('@').is_some_and(|(user, host)| !user.is_empty() && host.contains('.')) && !to.contains(char::is_whitespace)
}

/// Правило в редакторе: `index` — какое правило правим (`None` — новое).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Draft {
    pub(crate) index: Option<usize>,
    pub(crate) enabled: bool,
    pub(crate) name: String,
    /// Условия: группы «или», между группами — «и».
    pub(crate) when: Vec<Vec<Cond>>,
    pub(crate) actions: Vec<ActDraft>,
    pub(crate) stop: bool,
    pub(crate) local: bool,
    /// «Применять к существующим письмам» при сохранении.
    pub(crate) apply: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum DraftError {
    Value,
    Folder,
    Address,
    Category,
}

impl DraftError {
    fn text(self) -> String {
        match self {
            DraftError::Value => tr!("rules-bad-value"),
            DraftError::Folder => tr!("rules-bad-folder"),
            DraftError::Address => tr!("rules-bad-address"),
            DraftError::Category => tr!("rules-bad-category"),
        }
    }
}

impl Draft {
    pub(crate) fn new(folder: Option<(String, String)>) -> Self {
        Draft {
            index: None,
            enabled: true,
            name: String::new(),
            when: vec![vec![Cond::contains(Field::Subject, "")]],
            actions: vec![ActDraft::new(Kind::Move, folder)],
            stop: false,
            local: false,
            apply: false,
        }
    }

    pub(crate) fn of(index: usize, r: &Rule) -> Self {
        Draft {
            index: Some(index),
            enabled: r.enabled,
            name: r.name.clone(),
            when: if r.when.is_empty() { vec![vec![Cond::contains(Field::Subject, "")]] } else { r.when.clone() },
            actions: if r.actions.is_empty() { vec![ActDraft::new(Kind::MarkRead, None)] } else { r.actions.iter().map(ActDraft::of).collect() },
            stop: r.stop,
            local: r.local,
            apply: false,
        }
    }

    /// Первое действие (сценарии и правило из письма).
    pub(crate) fn first(&self) -> &ActDraft {
        &self.actions[0]
    }

    fn cond_count(&self) -> usize {
        self.when.iter().map(Vec::len).sum()
    }

    fn cond_mut(&mut self, flat: usize) -> Option<&mut Cond> {
        self.when.iter_mut().flatten().nth(flat)
    }

    /// Убрать условие; опустевшая группа уходит, последнее условие остаётся.
    fn remove_cond(&mut self, flat: usize) {
        if self.cond_count() <= 1 {
            return;
        }
        let mut n = 0;
        for g in &mut self.when {
            if flat < n + g.len() {
                g.remove(flat - n);
                break;
            }
            n += g.len();
        }
        self.when.retain(|g| !g.is_empty());
    }

    /// Правило из редактора; проверки — до сервера.
    pub(crate) fn rule(&self) -> Result<Rule, DraftError> {
        let mut when = Vec::new();
        for g in &self.when {
            let mut group = Vec::new();
            for c in g {
                let mut value = c.value.trim().to_owned();
                if c.op.is_domain() {
                    value = value.trim_start_matches('@').to_owned();
                }
                if value.is_empty() {
                    return Err(DraftError::Value);
                }
                group.push(Cond { field: c.field, op: c.op, value });
            }
            when.push(group);
        }
        let actions = self.actions.iter().map(ActDraft::action).collect::<Result<Vec<_>, _>>()?;
        // Пересылку выполняет только сервер: письмо уходит от имени сервера, без копии в «Отправленных».
        let local = self.local && !actions.iter().any(|a| matches!(a, Action::Redirect(_)));
        Ok(Rule { enabled: self.enabled, name: clean_name(&self.name), when, actions, stop: self.stop, local })
    }
}

/// Список правил после правки: заменить `index` или добавить в конец.
pub(crate) fn apply(rules: &[Rule], index: Option<usize>, rule: Rule) -> Vec<Rule> {
    let mut out = rules.to_vec();
    match index.filter(|i| *i < out.len()) {
        Some(i) => out[i] = rule,
        None => out.push(rule),
    }
    out
}

/// Список после «выше» / «ниже» у правила `i`; край списка — без изменений (`None`).
pub(crate) fn shift(rules: &[Rule], i: usize, delta: isize) -> Option<Vec<Rule>> {
    let j = i.checked_add_signed(delta).filter(|j| *j < rules.len() && i < rules.len())?;
    let mut out = rules.to_vec();
    out.swap(i, j);
    Some(out)
}

/// Категория для правила: (ключевое слово, имя, цвет).
pub(crate) type CategoryChoice = (String, String, &'static str);

/// Кусок строки правила: слово (ЕСЛИ/ИЛИ/И), значение (в рамке), текст.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Tok {
    Word(String),
    Value(String),
    Text(String),
}

/// Правило кусками: «ЕСЛИ от содержит [x] ИЛИ [y] → в папку [Отчёты], категория [Тендеры] · стоп». Папка — по id из
/// текущих (её могли переименовать), категория — по ключевому слову из таблицы (удалённая — словом).
pub(crate) fn tokens(r: &Rule, mailboxes: &[(String, String)], categories: &[CategoryChoice]) -> Vec<Tok> {
    let mut out = Vec::new();
    for (gi, g) in r.when.iter().enumerate() {
        for (ci, c) in g.iter().enumerate() {
            let word = match (gi, ci) {
                (0, 0) => "rules-tok-if",
                (_, 0) => "rules-tok-and",
                _ => "rules-tok-or",
            };
            out.push(Tok::Word(tr!(word)));
            out.push(Tok::Text(tr!("rules-tok-cond", field = field_name(c.field).to_lowercase(), op = op_name(c.op)))); // i18n-ok: строчные
            out.push(Tok::Value(c.value.clone()));
        }
    }
    out.push(Tok::Text("→".into()));
    for (i, a) in r.actions.iter().enumerate() {
        if i > 0 {
            out.push(Tok::Text("·".into()));
        }
        match a {
            Action::Move { mailbox_id, name } => {
                let folder = mailboxes.iter().find(|(id, _)| id == mailbox_id).map_or(name, |(_, n)| n);
                out.push(Tok::Text(tr!("rules-tok-move")));
                out.push(Tok::Value(folder.clone()));
            }
            Action::MarkRead => out.push(Tok::Text(tr!("rules-do-read"))),
            Action::Flag => out.push(Tok::Text(tr!("rules-do-flag"))),
            Action::Discard => out.push(Tok::Text(tr!("rules-do-discard"))),
            Action::Redirect(to) => {
                out.push(Tok::Text(tr!("rules-tok-redirect")));
                out.push(Tok::Value(to.clone()));
            }
            Action::Category(keyword) => {
                let name = categories.iter().find(|(k, _, _)| k == keyword).map_or(keyword, |(_, n, _)| n);
                out.push(Tok::Text(tr!("rules-tok-category")));
                out.push(Tok::Value(name.clone()));
            }
        }
    }
    if r.stop {
        out.push(Tok::Text(tr!("rules-tok-stop")));
    }
    out
}

/// Правило словами (строка списка без рамок, сценарии).
pub(crate) fn describe(r: &Rule, mailboxes: &[(String, String)], categories: &[CategoryChoice]) -> String {
    tokens(r, mailboxes, categories)
        .into_iter()
        .map(|t| match t {
            Tok::Word(s) | Tok::Text(s) => s,
            Tok::Value(v) => format!("«{v}»"),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Имя правила в списке и журнале: своё или условиями.
pub(crate) fn title(r: &Rule, mailboxes: &[(String, String)], categories: &[CategoryChoice]) -> String {
    if r.name.trim().is_empty() { describe(r, mailboxes, categories) } else { r.name.clone() }
}

/// Строка журнала срабатываний: когда, какое правило, тема письма.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LogEntry {
    pub(crate) at: i64,
    pub(crate) rule: String,
    pub(crate) subject: String,
}

/// Журнал строкой для кэша: строка на запись `время<TAB>правило<TAB>тема`.
pub(crate) fn log_text(log: &[LogEntry]) -> String {
    let flat = |s: &str| s.replace(['\t', '\n', '\r'], " ");
    log.iter().map(|e| format!("{}\t{}\t{}", e.at, flat(&e.rule), flat(&e.subject))).collect::<Vec<_>>().join("\n")
}

pub(crate) fn log_parse(text: &str) -> Vec<LogEntry> {
    text.lines()
        .filter_map(|l| {
            let mut p = l.splitn(3, '\t');
            let at = p.next()?.parse().ok()?;
            Some(LogEntry { at, rule: p.next()?.to_owned(), subject: p.next().unwrap_or_default().to_owned() })
        })
        .collect()
}

/// «10:42» сегодня, «Вчера», иначе «12.10».
pub(crate) fn when_text(at: i64) -> String {
    let tz = jiff::tz::TimeZone::system();
    let Ok(t) = jiff::Timestamp::from_second(at) else { return String::new() };
    let z = t.to_zoned(tz.clone());
    let today = jiff::Timestamp::now().to_zoned(tz).date();
    if z.date() == today {
        format!("{:02}:{:02}", z.hour(), z.minute())
    } else if today.yesterday().ok() == Some(z.date()) {
        tr!("rules-log-yesterday")
    } else {
        format!("{:02}.{:02}", z.day(), z.month())
    }
}

/// Итог «Проверить на последних 100 письмах».
#[derive(Debug, Clone)]
pub(crate) struct Test {
    pub(crate) rows: Vec<EmailRow>,
    pub(crate) seen: usize,
    pub(crate) show: bool,
}

/// Открытый список выбора в редакторе: поле, проверка (номер условия), вид, папка, категория (номер действия).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pick {
    Field(usize),
    Op(usize),
    Kind(usize),
    Folder(usize),
    Category(usize),
}

type Choice = (String, bool, Rc<dyn Fn(&mut Draft)>);

pub(crate) enum RulesEvent {
    /// Записать список правил; `replace` — подтверждена замена чужого скрипта.
    Save { rules: Vec<Rule>, replace: bool },
    /// Правило сохранено с «Применять к существующим»: его действия — подходящим письмам кэша.
    Apply(Rule),
}

impl EventEmitter<RulesEvent> for RulesPanel {}

pub(crate) struct RulesPanel {
    live: bool,
    /// Правила на сервере; `None` — ещё не пришли.
    pub(crate) state: Option<RulesState>,
    /// Папки ящика: (id, имя).
    pub(crate) mailboxes: Vec<(String, String)>,
    /// Категории своего ящика (таблица S5-02).
    pub(crate) categories: Vec<CategoryChoice>,
    pub(crate) editor: Option<Draft>,
    name: Entity<InputState>,
    conds: Vec<Entity<InputState>>,
    addresses: Vec<Entity<InputState>>,
    sync_inputs: bool,
    focus_first: bool,
    /// Список ушёл на сервер, ждём ответа.
    pub(crate) busy: bool,
    /// Ждёт «Заменить»: имя чужого скрипта, заменится ли его текст, список правил.
    pub(crate) conflict: Option<(String, bool, Vec<Rule>)>,
    /// Последний отправленный список.
    pending: Option<Vec<Rule>>,
    confirm_delete: Option<usize>,
    pub(crate) error: Option<String>,
    note: Option<String>,
    pub(crate) menu: Option<Pick>,
    /// Кэш писем: проверка правила на последних письмах.
    cache: Option<Cache>,
    pub(crate) test: Option<Test>,
    /// «Применять к существующим — найдено N».
    pub(crate) found: Option<usize>,
    /// Правило, которое после записи применится к существующим письмам.
    after_save: Option<Rule>,
    /// Журнал срабатываний правил «в приложении» (новые первыми).
    pub(crate) log: Vec<LogEntry>,
    log_all: bool,
    /// Прокрутка панели (снимок `rules-test` — внизу).
    pub(crate) scroll: ScrollHandle,
    /// Итог проверки — сразу со списком писем (снимок `rules-test`).
    pub(crate) show_test: bool,
}

impl RulesPanel {
    pub(crate) fn new(
        state: Option<RulesState>,
        mailboxes: Vec<(String, String)>,
        categories: Vec<CategoryChoice>,
        cache: Option<Cache>,
        live: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("rules-name-placeholder")));
        Self {
            live,
            state,
            mailboxes,
            categories,
            editor: None,
            name,
            conds: Vec::new(),
            addresses: Vec::new(),
            sync_inputs: false,
            focus_first: false,
            busy: false,
            conflict: None,
            pending: None,
            confirm_delete: None,
            error: None,
            note: None,
            menu: None,
            cache,
            test: None,
            found: None,
            after_save: None,
            log: Vec::new(),
            log_all: false,
            scroll: ScrollHandle::new(),
            show_test: false,
        }
    }

    fn rules(&self) -> &[Rule] {
        self.state.as_ref().map_or(&[], |s| s.rules.as_slice())
    }

    /// «Выполнять на сервере, где возможно»: выключено, когда все правила — «в приложении».
    pub(crate) fn server_on(&self) -> bool {
        let rules = self.rules();
        rules.is_empty() || rules.iter().any(|r| !r.local)
    }

    fn read_inputs(&mut self, cx: &App) {
        // Поля ещё не построены по черновику (новый редактор, смена состава): правда — в черновике.
        if self.sync_inputs {
            return;
        }
        let Some(d) = &mut self.editor else { return };
        d.name = self.name.read(cx).value().to_string();
        for (i, input) in self.conds.iter().enumerate() {
            let v = input.read(cx).value().to_string();
            if let Some(c) = d.cond_mut(i) {
                c.value = v;
            }
        }
        for (a, input) in d.actions.iter_mut().zip(&self.addresses) {
            if a.kind == Kind::Redirect {
                a.address = input.read(cx).value().to_string();
            }
        }
    }

    /// Правила с сервера (вход, после записи автоответа): редактор и строки подтверждения не трогаем.
    pub(crate) fn server_state(&mut self, state: RulesState, cx: &mut Context<Self>) {
        if !self.busy {
            self.state = Some(state);
        }
        cx.notify();
    }

    pub(crate) fn saved(&mut self, state: RulesState, cx: &mut Context<Self>) {
        self.state = Some(state);
        self.busy = false;
        self.editor = None;
        self.conflict = None;
        self.confirm_delete = None;
        self.error = None;
        self.menu = None;
        self.note = Some(tr!("rules-saved"));
        if let Some(rule) = self.after_save.take() {
            cx.emit(RulesEvent::Apply(rule));
        }
        cx.notify();
    }

    /// Сервер не принял скрипт или запрос не удался: сообщение в панели, редактор остаётся.
    pub(crate) fn failed(&mut self, error: String, cx: &mut Context<Self>) {
        self.busy = false;
        self.after_save = None;
        self.error = Some(error);
        cx.notify();
    }

    /// Чужой скрипт: строка «Заменить?» со списком, который ждёт записи.
    pub(crate) fn conflict(&mut self, name: String, overwrite: bool, rules: Vec<Rule>, cx: &mut Context<Self>) {
        self.busy = false;
        self.conflict = Some((name, overwrite, rules));
        cx.notify();
    }

    /// Демо: записи не будет.
    pub(crate) fn not_saved(&mut self, cx: &mut Context<Self>) {
        self.busy = false;
        self.after_save = None;
        cx.notify();
    }

    /// Последний отправленный список (для строки «Заменить?» после ответа сервера).
    pub(crate) fn pending(&self) -> Option<&Vec<Rule>> {
        self.pending.as_ref()
    }

    /// Журнал срабатываний (экран почты: правила «в приложении», «Применять к существующим»).
    pub(crate) fn set_log(&mut self, log: Vec<LogEntry>, cx: &mut Context<Self>) {
        self.log = log;
        cx.notify();
    }

    fn save(&mut self, rules: Vec<Rule>, replace: bool, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = self.live;
        self.error = None;
        self.note = None;
        self.pending = Some(rules.clone());
        cx.emit(RulesEvent::Save { rules, replace });
        cx.notify();
    }

    pub(crate) fn edit(&mut self, draft: Draft, cx: &mut Context<Self>) {
        self.editor = Some(draft);
        self.sync_inputs = true;
        self.focus_first = true;
        self.error = None;
        self.note = None;
        self.confirm_delete = None;
        self.menu = None;
        self.test = None;
        self.found = None;
        cx.notify();
    }

    fn save_editor(&mut self, cx: &mut Context<Self>) {
        self.read_inputs(cx);
        let Some(d) = &self.editor else { return };
        match d.rule() {
            Ok(rule) => {
                let rules = apply(self.rules(), d.index, rule.clone());
                self.after_save = d.apply.then_some(rule);
                self.save(rules, false, cx);
            }
            Err(e) => {
                self.error = Some(e.text());
                cx.notify();
            }
        }
    }

    fn toggle(&mut self, i: usize, cx: &mut Context<Self>) {
        let mut rules = self.rules().to_vec();
        if let Some(r) = rules.get_mut(i) {
            r.enabled = !r.enabled;
            self.save(rules, false, cx);
        }
    }

    fn delete(&mut self, i: usize, cx: &mut Context<Self>) {
        let mut rules = self.rules().to_vec();
        if i < rules.len() {
            rules.remove(i);
            self.confirm_delete = None;
            self.editor = None;
            self.save(rules, false, cx);
        }
    }

    fn move_rule(&mut self, i: usize, delta: isize, cx: &mut Context<Self>) {
        if self.editor.is_some() {
            return;
        }
        if let Some(rules) = shift(self.rules(), i, delta) {
            self.save(rules, false, cx);
        }
    }

    /// Все правила — на сервер или «в приложении» (пересылка — всегда на сервере).
    fn set_server(&mut self, on: bool, cx: &mut Context<Self>) {
        let rules: Vec<Rule> =
            self.rules().iter().map(|r| Rule { local: !on && !r.actions.iter().any(|a| matches!(a, Action::Redirect(_))), ..r.clone() }).collect();
        if let Some(d) = &mut self.editor {
            d.local = !on;
        }
        if rules.is_empty() {
            // Пустой список: настройка видна по новым правилам.
            return cx.notify();
        }
        self.save(rules, false, cx);
    }

    /// Правка черновика; `structure` — изменился состав полей ввода (их — заново).
    fn set_editor(&mut self, structure: bool, cx: &mut Context<Self>, f: impl FnOnce(&mut Draft)) {
        self.read_inputs(cx);
        if let Some(d) = &mut self.editor {
            f(d);
            self.error = None;
            self.test = None;
            self.found = None;
            self.sync_inputs |= structure;
            cx.notify();
        }
    }

    /// Подходящие письма кэша (в фоне): «Проверить» — последние 100, «Применять» — сколько найдено.
    pub(crate) fn run_matching(&mut self, existing: bool, cx: &mut Context<Self>) {
        self.read_inputs(cx);
        let Some(d) = &self.editor else { return };
        let rule = match d.rule() {
            Ok(r) => r,
            Err(e) => {
                self.error = Some(e.text());
                return cx.notify();
            }
        };
        let Some(cache) = self.cache.clone() else { return };
        let limit = if existing { rulerun::EXISTING } else { rulerun::RECENT };
        let task = cx.background_spawn(async move { rulerun::matching(&cache, &rule, limit) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |p, cx| {
                match result {
                    Ok((rows, seen)) if existing => {
                        let _ = seen;
                        p.found = Some(rows.len());
                    }
                    Ok((rows, seen)) => p.test = Some(Test { rows, seen, show: p.show_test }),
                    Err(e) => tracing::warn!("rules: test: {e}"),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

fn link(id: impl Into<ElementId>, label: String) -> Stateful<Div> {
    let l = crate::roles::now();
    let id = id.into();
    div()
        .id(id.clone())
        .children(crate::screens::spot(id))
        .px_1()
        .rounded_md()
        .cursor_pointer()
        .text_color(l.accent_text())
        .hover(|d| d.bg(l.hover()))
        .child(label)
}

/// Кнопка-значок (порядок, удалить, раскрыть).
fn icon_button(id: impl Into<ElementId>, g: char, enabled: bool) -> Stateful<Div> {
    let l = crate::roles::now();
    let id = id.into();
    div()
        .id(id.clone())
        .children(crate::screens::spot(id))
        .size(px(24.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(l.px(Metric::RadiusControl))
        .when(enabled, |d| d.cursor_pointer().text_color(l.text2()).hover(|d| d.bg(l.hover())))
        .when(!enabled, |d| d.text_color(l.c(Role::TextDisabled)))
        .child(icon(g, 12.))
}

/// Строка правила кусками: слова — серым, значения — в голубой рамке.
fn token_line(toks: Vec<Tok>) -> Div {
    let l = crate::roles::now();
    div().flex().flex_wrap().items_center().gap_1().text_size(px(12.)).children(toks.into_iter().map(move |t| match t {
        Tok::Word(s) => div().px_1().rounded_sm().bg(l.fill()).text_color(l.text2()).font_weight(FontWeight::SEMIBOLD).child(s),
        Tok::Value(s) => div().px_1p5().rounded_sm().bg(l.picked()).text_color(l.accent_text()).child(s),
        Tok::Text(s) => div().text_color(l.text2()).child(s),
    }))
}

impl RulesPanel {
    /// Список выбора: кнопка с текущим значением, пункты под ней.
    #[allow(clippy::too_many_arguments)]
    fn select(
        &self,
        pick: Pick,
        id: (&'static str, usize),
        label: String,
        width: f32,
        items: Vec<Choice>,
        item_id: &'static str,
        cx: &mut Context<Self>,
    ) -> Div {
        let l = crate::roles::look(cx);
        let open = self.menu == Some(pick);
        let field = div()
            .id(id)
            .children(crate::screens::spot(id))
            .w(px(width))
            .h(px(28.))
            .px_2()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .rounded(l.px(Metric::RadiusControl))
            .border_1()
            .border_color(if open { l.accent() } else { l.stroke() })
            .bg(l.c(Role::FillControlInput))
            .cursor_pointer()
            .hover(|d| d.bg(l.hover()))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.menu = if this.menu == Some(pick) { None } else { Some(pick) };
                cx.notify();
            }))
            .child(div().min_w_0().overflow_hidden().whitespace_nowrap().child(label))
            .child(icon(glyph::CHEVRON, 10.).text_color(l.text2()));
        let list = open.then(|| {
            let rows = items.into_iter().enumerate().map(|(k, (text, on, set))| {
                div()
                    .id((item_id, k))
                    .children(crate::screens::spot((item_id, k)))
                    .h(px(28.))
                    .px_2()
                    .flex()
                    .flex_none()
                    .items_center()
                    .rounded(l.px(Metric::RadiusControl))
                    .whitespace_nowrap()
                    .cursor_pointer()
                    .hover(|d| d.bg(l.hover()))
                    .when(on, |d| d.bg(l.picked()).font_weight(FontWeight::SEMIBOLD))
                    .child(text)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let set = set.clone();
                        this.menu = None;
                        this.set_editor(true, cx, |d| set(d));
                    }))
            });
            deferred(
                div()
                    .id(("rules-menu", item_id.len()))
                    .absolute()
                    .top(px(32.))
                    .left_0()
                    .min_w(px(width))
                    .max_h(px(300.))
                    .overflow_y_scroll()
                    .occlude()
                    .p(px(4.))
                    .flex()
                    .flex_col()
                    .rounded(l.px(Metric::RadiusOverlay))
                    .border_1()
                    .border_color(l.c(Role::StrokeCard))
                    .bg(l.c(Role::LayerOverlay))
                    .text_color(l.text())
                    .shadow_lg()
                    .children(rows)
                    .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                        this.menu = None;
                        cx.notify();
                    })),
            )
            .with_priority(1)
        });
        div().relative().flex_none().child(field).children(list)
    }

    fn hits_text(&self, r: &Rule) -> String {
        let name = title(r, &self.mailboxes, &self.categories);
        let mine: Vec<&LogEntry> = self.log.iter().filter(|e| e.rule == name).collect();
        let where_ = if r.local { tr!("rules-where-app") } else { tr!("rules-where-server") };
        match mine.first() {
            Some(last) => tr!("rules-where-hits", place = where_, n = mine.len() as i64, when = when_text(last.at)),
            None => where_,
        }
    }

    fn render_row(&self, i: usize, r: &Rule, n: usize, cx: &mut Context<Self>) -> Div {
        let l = crate::roles::look(cx);
        let confirm = self.confirm_delete == Some(i);
        let editing = self.editor.as_ref().is_some_and(|d| d.index == Some(i));
        let free = self.editor.is_none() && !self.busy;
        let name = title(r, &self.mailboxes, &self.categories);
        let toks = tokens(r, &self.mailboxes, &self.categories);
        let rule = r.clone();
        div()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1p5()
            .rounded_md()
            .border_b_1()
            .border_color(l.border())
            .when(editing, |d| d.bg(l.hover()).border_l_2().border_color(l.accent()))
            .child(
                div()
                    .w(px(56.))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(
                        icon_button(("rule-up", i), glyph::CHEVRON_UP, free && i > 0)
                            .when(free && i > 0, |d| d.on_click(cx.listener(move |this, _, _, cx| this.move_rule(i, -1, cx)))),
                    )
                    .child(
                        icon_button(("rule-down", i), glyph::CHEVRON_DOWN, free && i + 1 < n)
                            .when(free && i + 1 < n, |d| d.on_click(cx.listener(move |this, _, _, cx| this.move_rule(i, 1, cx)))),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .when(!r.enabled, |d| d.opacity(0.5))
                    .child(div().font_weight(FontWeight::SEMIBOLD).truncate().child(name))
                    .child(token_line(toks)),
            )
            .child(div().w(px(180.)).flex_none().text_size(px(12.)).text_color(l.text2()).child(self.hits_text(r)))
            .when(confirm, |d| {
                d.child(div().text_color(l.error()).child(tr!("rules-delete-confirm")))
                    .child(link(("rule-delete-yes", i), tr!("rules-delete")).on_click(cx.listener(move |this, _, _, cx| this.delete(i, cx))))
                    .child(link(("rule-delete-no", i), tr!("rules-cancel")).on_click(cx.listener(|this, _, _, cx| {
                        this.confirm_delete = None;
                        cx.notify();
                    })))
            })
            .when(!confirm, |d| {
                d.child(crate::settings_tab::toggle(("rule-on", i), r.enabled, &l).on_click(cx.listener(move |this, _, _, cx| this.toggle(i, cx))))
                    .child(icon_button(("rule-edit", i), if editing { glyph::CHEVRON_UP } else { glyph::CHEVRON_DOWN }, true).on_click(cx.listener(
                        move |this, _, _, cx| {
                            if this.editor.as_ref().is_some_and(|d| d.index == Some(i)) {
                                this.editor = None;
                                cx.notify();
                            } else {
                                this.edit(Draft::of(i, &rule), cx);
                            }
                        },
                    )))
                    .child(icon_button(("rule-delete", i), glyph::DELETE, true).on_click(cx.listener(move |this, _, _, cx| {
                        this.confirm_delete = Some(i);
                        cx.notify();
                    })))
            })
    }

    fn render_conditions(&self, d: &Draft, cx: &mut Context<Self>) -> Div {
        let l = crate::roles::look(cx);
        let mut col = div().flex().flex_col().gap_1p5();
        let single = d.cond_count() <= 1;
        let mut flat = 0usize;
        for (gi, g) in d.when.iter().enumerate() {
            for (ci, c) in g.iter().enumerate() {
                let i = flat;
                flat += 1;
                let word = match (gi, ci) {
                    (0, 0) => tr!("rules-tok-if"),
                    (_, 0) => tr!("rules-tok-and"),
                    _ => tr!("rules-tok-or"),
                };
                let fields: Vec<Choice> = FIELDS
                    .iter()
                    .map(|(f, key)| {
                        let f = *f;
                        let set: Rc<dyn Fn(&mut Draft)> = Rc::new(move |d: &mut Draft| {
                            if let Some(c) = d.cond_mut(i) {
                                c.field = f;
                                if f == Field::Subject && c.op.is_domain() {
                                    c.op = Op::Contains;
                                }
                            }
                        });
                        (tr!(key), c.field == f, set)
                    })
                    .collect();
                let ops: Vec<Choice> = ops_for(c.field)
                    .iter()
                    .map(|(o, key)| {
                        let o = *o;
                        let set: Rc<dyn Fn(&mut Draft)> = Rc::new(move |d: &mut Draft| {
                            if let Some(c) = d.cond_mut(i) {
                                c.op = o;
                            }
                        });
                        (tr!(key), c.op == o, set)
                    })
                    .collect();
                let input = self.conds.get(i).cloned();
                let spot: ElementId = if i == 0 { "rules-value".into() } else { ("rules-value", i).into() };
                col = col.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .pl_1p5()
                        .border_l_2()
                        .border_color(if g.len() > 1 { l.stroke() } else { l.none() })
                        .child(div().w(px(44.)).flex_none().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(l.text2()).child(word))
                        .child(self.select(Pick::Field(i), ("rules-cond-field", i), field_name(c.field), 140., fields, "rules-field", cx))
                        .child(self.select(Pick::Op(i), ("rules-cond-op", i), op_name(c.op), 140., ops, "rules-op", cx))
                        .child(div().flex_1().min_w_0().relative().children(crate::screens::spot(spot)).children(input.map(|s| Input::new(&s))))
                        .when(!single, |d| {
                            d.child(
                                icon_button(("rules-cond-remove", i), glyph::CLOSE, true)
                                    .on_click(cx.listener(move |this, _, _, cx| this.set_editor(true, cx, |d| d.remove_cond(i)))),
                            )
                        }),
                );
            }
        }
        col
    }

    fn render_actions(&self, d: &Draft, cx: &mut Context<Self>) -> Div {
        let l = crate::roles::look(cx);
        let mut col = div().flex().flex_col().gap_1p5();
        let single = d.actions.len() <= 1;
        for (i, a) in d.actions.iter().enumerate() {
            let kinds: Vec<Choice> = KINDS
                .iter()
                .map(|(k, key)| {
                    let k = *k;
                    let folder = self.mailboxes.first().cloned();
                    let set: Rc<dyn Fn(&mut Draft)> = Rc::new(move |d: &mut Draft| {
                        if let Some(a) = d.actions.get_mut(i) {
                            a.kind = k;
                            if k == Kind::Move && a.folder.is_none() {
                                a.folder = folder.clone();
                            }
                        }
                    });
                    (tr!(key), a.kind == k, set)
                })
                .collect();
            let value: AnyElement = match a.kind {
                Kind::Move => {
                    let picked = a.folder.as_ref().map(|(id, _)| id.clone());
                    let items: Vec<Choice> = self
                        .mailboxes
                        .iter()
                        .map(|(id, name)| {
                            let folder = (id.clone(), name.clone());
                            let set: Rc<dyn Fn(&mut Draft)> = Rc::new(move |d: &mut Draft| {
                                if let Some(a) = d.actions.get_mut(i) {
                                    a.folder = Some(folder.clone());
                                }
                            });
                            (name.clone(), picked.as_deref() == Some(id.as_str()), set)
                        })
                        .collect();
                    let label = a
                        .folder
                        .as_ref()
                        .map(|(id, name)| self.mailboxes.iter().find(|(x, _)| x == id).map_or(name.clone(), |(_, n)| n.clone()))
                        .unwrap_or_else(|| tr!("rules-folder-pick"));
                    self.select(Pick::Folder(i), ("rules-act-folder", i), label, 220., items, "rules-folder", cx).into_any_element()
                }
                Kind::Category => {
                    let chosen = a.category.clone();
                    let items: Vec<Choice> = self
                        .categories
                        .iter()
                        .map(|(keyword, name, _)| {
                            let pick = keyword.clone();
                            let set: Rc<dyn Fn(&mut Draft)> = Rc::new(move |d: &mut Draft| {
                                if let Some(a) = d.actions.get_mut(i) {
                                    a.category = Some(pick.clone());
                                }
                            });
                            (name.clone(), chosen.as_deref() == Some(keyword.as_str()), set)
                        })
                        .collect();
                    let label = chosen
                        .as_ref()
                        .map(|k| self.categories.iter().find(|(x, _, _)| x == k).map_or(k.clone(), |(_, n, _)| n.clone()))
                        .unwrap_or_else(|| tr!("rules-category-pick"));
                    self.select(Pick::Category(i), ("rules-act-category", i), label, 220., items, "rules-category", cx).into_any_element()
                }
                Kind::Redirect => {
                    let spot: ElementId = if i == 0 { "rules-address".into() } else { ("rules-address", i).into() };
                    let input = self.addresses.get(i).cloned();
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(div().relative().children(crate::screens::spot(spot)).children(input.map(|s| Input::new(&s))))
                        .child(div().text_size(px(12.)).text_color(l.text2()).child(tr!("rules-redirect-note")))
                        .into_any_element()
                }
                Kind::Discard => div().text_size(px(12.)).text_color(l.text2()).child(tr!("rules-discard-hint")).into_any_element(),
                Kind::MarkRead | Kind::Flag => div().into_any_element(),
            };
            col = col.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().w(px(44.)).flex_none().text_color(l.text2()).child(format!("{}", i + 1)))
                    .child(self.select(Pick::Kind(i), ("rules-act-kind", i), kind_name(a.kind), 200., kinds, "rules-kind", cx))
                    .child(div().flex_1().min_w_0().flex().child(value))
                    .when(!single, |d| {
                        d.child(icon_button(("rules-act-remove", i), glyph::CLOSE, true).on_click(cx.listener(move |this, _, _, cx| {
                            this.set_editor(true, cx, |d| {
                                if d.actions.len() > 1 {
                                    d.actions.remove(i);
                                }
                            })
                        })))
                    }),
            );
        }
        col
    }

    fn render_editor(&self, d: &Draft, cx: &mut Context<Self>) -> Div {
        let l = crate::roles::look(cx);
        let section = |text: String| div().pt_1().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(l.text2()).child(text);
        let busy = self.busy;
        let check_row = |id: &'static str, on: bool, label: String| {
            div().id(id).children(crate::screens::spot(id)).flex().items_center().gap_2().cursor_pointer().child(checkbox(on)).child(label)
        };
        let found = match (d.apply, self.found) {
            (true, Some(n)) => tr!("rules-apply-found", n = n as i64),
            (true, None) => tr!("rules-apply-counting"),
            _ => tr!("rules-apply"),
        };
        let test = self.test.clone();
        let folder = self.mailboxes.first().cloned();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(l.c(Role::StrokeCard))
            .bg(l.panel())
            .child(div().font_weight(FontWeight::SEMIBOLD).child(if d.index.is_some() { tr!("rules-edit-title") } else { tr!("rules-new-title") }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().w(px(90.)).flex_none().text_color(l.text2()).child(tr!("rules-name")))
                    .child(div().flex_1().relative().children(crate::screens::spot("rules-name")).child(Input::new(&self.name))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(section(tr!("rules-conditions")))
                    .child(div().flex_1())
                    .child(
                        link("rules-cond-add", tr!("rules-cond-add")).on_click(
                            cx.listener(|this, _, _, cx| this.set_editor(true, cx, |d| d.when.push(vec![Cond::contains(Field::Subject, "")]))),
                        ),
                    )
                    .child(link("rules-cond-or", tr!("rules-cond-or")).on_click(cx.listener(|this, _, _, cx| {
                        this.set_editor(true, cx, |d| {
                            let field = d.when.last().and_then(|g| g.last()).map_or(Field::Subject, |c| c.field);
                            if let Some(g) = d.when.last_mut() {
                                g.push(Cond::contains(field, ""));
                            }
                        })
                    }))),
            )
            .child(self.render_conditions(d, cx))
            .child(div().flex().items_center().gap_3().child(section(tr!("rules-actions"))).child(div().flex_1()).child(
                link("rules-act-add", tr!("rules-act-add")).on_click(cx.listener(move |this, _, _, cx| {
                    let folder = folder.clone();
                    this.set_editor(true, cx, |d| d.actions.push(ActDraft::new(Kind::MarkRead, folder)))
                })),
            ))
            .child(self.render_actions(d, cx))
            .child(section(tr!("rules-options")))
            .child(
                check_row("rules-stop", d.stop, tr!("rules-stop"))
                    .on_click(cx.listener(|this, _, _, cx| this.set_editor(false, cx, |d| d.stop = !d.stop))),
            )
            .child(check_row("rules-apply", d.apply, found).on_click(cx.listener(|this, _, _, cx| {
                this.set_editor(false, cx, |d| d.apply = !d.apply);
                if this.editor.as_ref().is_some_and(|d| d.apply) {
                    this.run_matching(true, cx);
                }
            })))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        link("rules-test", tr!("rules-test", n = rulerun::RECENT as i64))
                            .on_click(cx.listener(|this, _, _, cx| this.run_matching(false, cx))),
                    )
                    .when_some(test.clone(), |el, t| {
                        el.child(div().text_color(l.text2()).child(tr!("rules-test-result", n = t.rows.len() as i64, seen = t.seen as i64))).when(
                            !t.rows.is_empty(),
                            |el| {
                                el.child(link("rules-test-show", if t.show { tr!("rules-test-hide") } else { tr!("rules-test-show") }).on_click(
                                    cx.listener(|this, _, _, cx| {
                                        if let Some(t) = &mut this.test {
                                            t.show = !t.show;
                                        }
                                        cx.notify();
                                    }),
                                ))
                            },
                        )
                    }),
            )
            .when_some(test.filter(|t| t.show), |el, t| {
                el.child(div().flex().flex_col().rounded_md().border_1().border_color(l.border()).children(t.rows.into_iter().take(TEST_ROWS).map(
                    |r| {
                        let from = if r.from_name.trim().is_empty() { r.from_addr.clone() } else { r.from_name.clone() };
                        div()
                            .flex()
                            .gap_3()
                            .px_2()
                            .py_1()
                            .border_b_1()
                            .border_color(l.border())
                            .text_size(px(12.))
                            .child(div().w(px(160.)).flex_none().truncate().child(from))
                            .child(div().flex_1().min_w_0().truncate().child(r.subject))
                            .child(div().flex_none().text_color(l.text2()).child(when_text(r.received_at)))
                    },
                )))
            })
            .child(
                div()
                    .flex()
                    .gap_3()
                    .pt_1()
                    .child(
                        button("rules-editor-save", if busy { tr!("rules-saving") } else { tr!("rules-editor-save") }, !busy)
                            .on_click(cx.listener(|this, _, _, cx| this.save_editor(cx))),
                    )
                    .child(button("rules-editor-cancel", tr!("rules-cancel"), false).on_click(cx.listener(|this, _, _, cx| {
                        this.editor = None;
                        this.error = None;
                        this.menu = None;
                        cx.notify();
                    }))),
            )
    }

    fn render_aside(&self, cx: &mut Context<Self>) -> Div {
        let l = crate::roles::look(cx);
        let n = if self.log_all { self.log.len() } else { LOG_SHORT };
        let all = self.log_all;
        let journal = div()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .rounded(l.px(Metric::RadiusOverlay))
            .border_1()
            .border_color(l.c(Role::StrokeCard))
            .bg(l.panel())
            .child(
                div().flex().items_center().child(div().flex_1().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(tr!("rules-log"))).when(
                    self.log.len() > LOG_SHORT,
                    |d| {
                        d.child(link("rules-log-all", if all { tr!("rules-log-short") } else { tr!("rules-log-all") }).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.log_all = !this.log_all;
                                cx.notify();
                            },
                        )))
                    },
                ),
            )
            .when(self.log.is_empty(), |d| d.child(div().text_color(l.text2()).child(tr!("rules-log-empty"))))
            .children(self.log.iter().take(n).map(|e| {
                div().flex().gap_2().text_size(px(12.)).child(div().w(px(44.)).flex_none().text_color(l.text2()).child(when_text(e.at))).child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().font_weight(FontWeight::SEMIBOLD).truncate().child(e.rule.clone()))
                        .child(div().text_color(l.text2()).truncate().child(e.subject.clone())),
                )
            }));
        let hint = crate::settings_tab::aside_card(tr!("rules-hint"), vec![tr!("rules-hint-1"), tr!("rules-hint-2")], &l);
        div().w(px(300.)).flex_none().flex().flex_col().gap_2().child(journal).child(hint)
    }

    /// Поля ввода редактора — по составу черновика (новые сущности; значения — из черновика).
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.editor.clone() else { return };
        self.name.update(cx, |s, cx| s.set_value(d.name.clone(), window, cx));
        self.conds = d
            .when
            .iter()
            .flatten()
            .map(|c| {
                let placeholder = if c.op.is_domain() { tr!("rules-domain-placeholder") } else { tr!("rules-value-placeholder") };
                let value = c.value.clone();
                cx.new(|cx| {
                    let mut s = InputState::new(window, cx).placeholder(placeholder);
                    s.set_value(value, window, cx);
                    s
                })
            })
            .collect();
        self.addresses = d
            .actions
            .iter()
            .map(|a| {
                let value = a.address.clone();
                cx.new(|cx| {
                    let mut s = InputState::new(window, cx).placeholder(tr!("rules-address-placeholder"));
                    s.set_value(value, window, cx);
                    s
                })
            })
            .collect();
        if std::mem::take(&mut self.focus_first)
            && let Some(first) = self.conds.first()
        {
            first.update(cx, |s, cx| s.focus(window, cx));
        }
    }
}

impl Render for RulesPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let l = crate::roles::look(cx);
        if std::mem::take(&mut self.sync_inputs) {
            self.sync(window, cx);
        }
        let rules = self.rules().to_vec();
        let n = rules.len();
        let editor = self.editor.clone();
        let mut list = div().flex().flex_col();
        for (i, r) in rules.iter().enumerate() {
            list = list.child(self.render_row(i, r, n, cx));
            if let Some(d) = editor.as_ref().filter(|d| d.index == Some(i)) {
                list = list.child(self.render_editor(d, cx));
            }
        }
        let new_editor = editor.filter(|d| d.index.is_none()).map(|d| self.render_editor(&d, cx));
        let foreign = self.state.as_ref().map(|s| s.foreign.clone()).unwrap_or_default();
        let active = self.state.as_ref().is_some_and(|s| s.active);
        let server = self.server_on();
        let any_server = rules.iter().any(Rule::on_server);
        let server_row =
            div()
                .flex()
                .items_center()
                .gap_4()
                .py_2()
                .border_b_1()
                .border_color(l.border())
                .child(div().flex_1().min_w_0().flex().flex_col().gap_0p5().child(tr!("rules-server")).child(
                    div().text_size(px(12.)).text_color(l.text2()).child(if server { tr!("rules-server-on") } else { tr!("rules-server-off") }),
                ))
                .child(
                    crate::settings_tab::toggle("rules-server", server, &l)
                        .when(!self.busy && self.editor.is_none(), |d| d.on_click(cx.listener(move |this, _, _, cx| this.set_server(!server, cx)))),
                );
        let header = div()
            .flex()
            .gap_2()
            .px_2()
            .pt_2()
            .pb_1()
            .text_size(px(12.))
            .text_color(l.text2())
            .child(div().w(px(56.)).flex_none().whitespace_nowrap().child(tr!("rules-col-order")))
            .child(div().flex_1().child(tr!("rules-col-rule")))
            .child(div().w(px(180.)).child(tr!("rules-col-where")))
            .child(div().w(px(96.)));
        // Раздел вкладки «Настройки» (S5-09): заголовок и пояснение — у вкладки, здесь — список и редактор.
        let card = div()
            .flex_1()
            .min_w_0()
            .max_w(px(900.))
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_color(l.text2()).child(tr!("rules-order-note")))
            .child(server_row)
            .when(!self.live, |d| d.child(div().text_color(l.text2()).child(tr!("rules-demo"))))
            .when(self.live && self.state.is_none(), |d| d.child(div().text_color(l.text2()).child(tr!("rules-loading"))))
            .when(!rules.is_empty() && !active && self.live && any_server, |d| d.child(div().text_color(l.text2()).child(tr!("rules-all-off"))))
            .when(rules.is_empty() && self.state.is_some(), |d| d.child(div().text_color(l.text2()).child(tr!("rules-empty"))))
            .when(!rules.is_empty(), |d| d.child(div().flex().flex_col().child(header).child(list)))
            .children(new_editor)
            .when_some(self.conflict.clone(), |d, (name, overwrite, pending)| {
                let text = if overwrite { tr!("rules-conflict-overwrite", name = name) } else { tr!("rules-conflict-active", name = name) };
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .p_2()
                        .rounded_md()
                        .bg(l.warning_bg())
                        .child(div().flex_1().child(text))
                        .child(link("rules-replace", tr!("rules-replace")).on_click(cx.listener(move |this, _, _, cx| {
                            this.conflict = None;
                            this.save(pending.clone(), true, cx);
                        })))
                        .child(link("rules-replace-no", tr!("rules-cancel")).on_click(cx.listener(|this, _, _, cx| {
                            this.conflict = None;
                            cx.notify();
                        }))),
                )
            })
            .when_some(self.error.clone(), |d, e| d.child(div().text_color(l.error()).child(e)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .when(self.editor.is_none(), |d| {
                        d.child(button("rules-add", tr!("rules-add"), true).on_click(cx.listener(move |this, _, _, cx| {
                            let mut draft = Draft::new(this.mailboxes.first().cloned());
                            draft.local = !this.server_on();
                            this.edit(draft, cx)
                        })))
                    })
                    .when_some(self.note.clone(), |d, n| d.child(div().text_color(l.success()).child(n))),
            )
            .when(!foreign.is_empty(), |d| {
                d.child(div().pt_2().font_weight(FontWeight::SEMIBOLD).child(tr!("rules-foreign-title")))
                    .child(div().text_color(l.text2()).child(tr!("rules-foreign-hint")))
                    .children(foreign.into_iter().map(|f| {
                        let title = if f.active { tr!("rules-foreign-active", name = f.name) } else { tr!("rules-foreign-inactive", name = f.name) };
                        div().flex().flex_col().gap_1().child(div().text_color(l.text2()).child(title)).child(
                            div()
                                .p_2()
                                .rounded_md()
                                .bg(l.fill_passive())
                                .font_family("Consolas")
                                .text_size(px(12.))
                                .whitespace_normal()
                                .children(f.text.lines().map(|l| div().child(l.to_owned())).collect::<Vec<_>>()),
                        )
                    }))
            });
        div()
            .id("rules-panel")
            .flex()
            .items_start()
            .gap_4()
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .px_6()
            .py_4()
            .child(card)
            .child(self.render_aside(cx))
    }
}

#[cfg(test)]
mod tests {
    // Не `super::*`: из `gpui_kit::*` пришёл бы атрибут `test` gpui вместо встроенного.
    use super::{ActDraft, Draft, DraftError, Kind, LogEntry, apply, log_parse, log_text, shift};
    use postman_core::rules::{Action, Cond, Field, Op, Rule};

    fn draft(kind: Kind) -> Draft {
        let mut d = Draft::new(Some(("f1".into(), "Отчёты".into())));
        d.when = vec![vec![Cond::contains(Field::Subject, "  отчёт ")]];
        d.actions = vec![ActDraft {
            kind,
            folder: Some(("f1".into(), "Отчёты".into())),
            address: " boss@corp.example ".into(),
            category: Some("pm.cat.tenders".into()),
        }];
        d
    }

    #[test]
    fn editor_makes_a_rule() {
        let r = draft(Kind::Move).rule().unwrap();
        assert_eq!(r.when[0][0].value, "отчёт");
        assert_eq!(r.actions, vec![Action::Move { mailbox_id: "f1".into(), name: "Отчёты".into() }]);
        assert_eq!(draft(Kind::Redirect).rule().unwrap().actions, vec![Action::Redirect("boss@corp.example".into())]);
        let mut empty = draft(Kind::Flag);
        empty.when[0][0].value = " ".into();
        assert_eq!(empty.rule(), Err(DraftError::Value));
        let mut no_folder = draft(Kind::Move);
        no_folder.actions[0].folder = None;
        assert_eq!(no_folder.rule(), Err(DraftError::Folder));
        for bad in ["", "boss", "@corp.example", "boss@corp", "bo ss@corp.example"] {
            let mut d = draft(Kind::Redirect);
            d.actions[0].address = bad.into();
            assert_eq!(d.rule(), Err(DraftError::Address), "{bad}");
        }
        // Категория (S5-26): слово из таблицы; не выбрана — отказ.
        assert_eq!(draft(Kind::Category).rule().unwrap().actions, vec![Action::Category("pm.cat.tenders".into())]);
        let mut no_cat = draft(Kind::Category);
        no_cat.actions[0].category = None;
        assert_eq!(no_cat.rule(), Err(DraftError::Category));
        // Туда и обратно.
        for kind in [Kind::Redirect, Kind::Category] {
            let r = draft(kind).rule().unwrap();
            assert_eq!(Draft::of(3, &r).rule(), Ok(r));
        }
    }

    /// T2-50: имя, «или» и «и», домен без «@», несколько действий, стоп; пересылка — только на сервере.
    #[test]
    fn rich_editor_rule() {
        let mut d = draft(Kind::Redirect);
        d.name = " Счета \n поставщиков ".into();
        d.when[0].push(Cond::contains(Field::Subject, "pdf"));
        d.when.push(vec![Cond { field: Field::From, op: Op::NotDomain, value: " @ittask.ru".into() }]);
        d.actions.push(ActDraft::new(Kind::MarkRead, None));
        d.stop = true;
        d.local = true;
        let r = d.rule().unwrap();
        assert_eq!(r.name, "Счета поставщиков");
        assert_eq!(r.when[1][0].value, "ittask.ru");
        assert_eq!(r.actions.len(), 2);
        assert!(r.stop && !r.local, "пересылка — на сервере");
        d.actions.remove(0);
        assert!(d.rule().unwrap().local);
        // Убрать условие: опустевшая группа уходит, последнее остаётся.
        d.remove_cond(2);
        assert_eq!(d.when.len(), 1);
        d.remove_cond(0);
        d.remove_cond(0);
        assert_eq!(d.cond_count(), 1);
    }

    #[test]
    fn edit_replaces_new_appends_and_moves() {
        let r = |v: &str| Rule::simple(true, Field::From, v, Action::Flag);
        let list = vec![r("a"), r("b")];
        assert_eq!(apply(&list, Some(1), r("c")), vec![r("a"), r("c")]);
        assert_eq!(apply(&list, None, r("c")), vec![r("a"), r("b"), r("c")]);
        assert_eq!(apply(&list, Some(9), r("c")).len(), 3);
        assert_eq!(shift(&list, 1, -1), Some(vec![r("b"), r("a")]));
        assert_eq!(shift(&list, 0, -1), None);
        assert_eq!(shift(&list, 1, 1), None);
    }

    #[test]
    fn log_round_trips() {
        let log = vec![
            LogEntry { at: 10, rule: "Коды\tподтверждения".into(), subject: "Ваш код\n48".into() },
            LogEntry { at: 5, rule: "x".into(), subject: String::new() },
        ];
        let back = log_parse(&log_text(&log));
        assert_eq!(back[0], LogEntry { at: 10, rule: "Коды подтверждения".into(), subject: "Ваш код 48".into() });
        assert_eq!(back[1], log[1]);
        assert!(log_parse("мусор").is_empty());
    }
}
