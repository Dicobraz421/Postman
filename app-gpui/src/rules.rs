//! «Правила» (S4-07): список правил почты, простой редактор «условие → действие», включить/выключить, удалить
//! (подтверждение строкой). Панель на месте содержимого окна, как «Автоматические ответы». Правила выполняет
//! сервер (Sieve, `postman_core::rules`); каждое изменение сразу уходит на сервер целым списком, сервер проверяет
//! скрипт (`SieveScript/validate`) — ошибка видна в панели. Скрипты сервера, написанные не в Postman, — только
//! текстом; заменить их можно лишь подтверждением «Заменить».

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use postman_core::rules::{Action, Field, Rule};
use postman_core::settings::RulesState;

use crate::buttons::plain as button;
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
const KINDS: [(Kind, &str); 6] = [
    (Kind::Move, "rules-kind-move"),
    (Kind::MarkRead, "rules-kind-read"),
    (Kind::Flag, "rules-kind-flag"),
    (Kind::Discard, "rules-kind-discard"),
    (Kind::Redirect, "rules-kind-redirect"),
    (Kind::Category, "rules-kind-category"),
];

/// Правило в редакторе: `index` — какое правило правим (`None` — новое).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Draft {
    pub(crate) index: Option<usize>,
    pub(crate) enabled: bool,
    pub(crate) field: Field,
    pub(crate) value: String,
    pub(crate) kind: Kind,
    /// Папка: (id, имя).
    pub(crate) folder: Option<(String, String)>,
    pub(crate) address: String,
    /// Категория: ключевое слово (S5-26).
    pub(crate) category: Option<String>,
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
    fn new() -> Self {
        Draft {
            index: None,
            enabled: true,
            field: Field::Subject,
            value: String::new(),
            kind: Kind::Move,
            folder: None,
            address: String::new(),
            category: None,
        }
    }

    pub(crate) fn of(index: usize, r: &Rule) -> Self {
        let mut d = Draft { index: Some(index), enabled: r.enabled, field: r.field, value: r.value.clone(), ..Self::new() };
        d.kind = match &r.action {
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

    /// Правило из редактора; проверки — до сервера (адрес пересылки сервер не проверяет, `docs/STALWART.md`).
    pub(crate) fn rule(&self) -> Result<Rule, DraftError> {
        let value = self.value.trim();
        if value.is_empty() {
            return Err(DraftError::Value);
        }
        let action = match self.kind {
            Kind::Move => {
                let (mailbox_id, name) = self.folder.clone().ok_or(DraftError::Folder)?;
                Action::Move { mailbox_id, name }
            }
            Kind::MarkRead => Action::MarkRead,
            Kind::Flag => Action::Flag,
            Kind::Discard => Action::Discard,
            Kind::Redirect => {
                let to = self.address.trim();
                let ok = to.split_once('@').is_some_and(|(user, host)| !user.is_empty() && host.contains('.')) && !to.contains(char::is_whitespace);
                if !ok {
                    return Err(DraftError::Address);
                }
                Action::Redirect(to.to_owned())
            }
            Kind::Category => Action::Category(self.category.clone().ok_or(DraftError::Category)?),
        };
        Ok(Rule { enabled: self.enabled, field: self.field, value: value.to_owned(), action })
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

/// Категория для правила: (ключевое слово, имя, цвет).
pub(crate) type CategoryChoice = (String, String, &'static str);

/// Правило словами: «Тема содержит «X» → переместить в «Y»». Папка — по id из текущих (её могли переименовать),
/// категория — по ключевому слову из таблицы (удалённая — словом).
pub(crate) fn describe(r: &Rule, mailboxes: &[(String, String)], categories: &[CategoryChoice]) -> String {
    let when = match r.field {
        Field::From => tr!("rules-when-from", value = r.value.clone()),
        Field::To => tr!("rules-when-to", value = r.value.clone()),
        Field::Subject => tr!("rules-when-subject", value = r.value.clone()),
    };
    let then = match &r.action {
        Action::Move { mailbox_id, name } => {
            let folder = mailboxes.iter().find(|(id, _)| id == mailbox_id).map_or(name, |(_, n)| n);
            tr!("rules-do-move", folder = folder.clone())
        }
        Action::MarkRead => tr!("rules-do-read"),
        Action::Flag => tr!("rules-do-flag"),
        Action::Discard => tr!("rules-do-discard"),
        Action::Redirect(to) => tr!("rules-do-redirect", address = to.clone()),
        Action::Category(keyword) => {
            let name = categories.iter().find(|(k, _, _)| k == keyword).map_or(keyword, |(_, n, _)| n);
            tr!("rules-do-category", category = name.clone())
        }
    };
    tr!("rules-line", when = when, then = then)
}

pub(crate) enum RulesEvent {
    /// Записать список правил; `replace` — подтверждена замена чужого скрипта.
    Save { rules: Vec<Rule>, replace: bool },
}

impl EventEmitter<RulesEvent> for RulesPanel {}

pub(crate) struct RulesPanel {
    live: bool,
    /// Правила на сервере; `None` — ещё не пришли.
    pub(crate) state: Option<RulesState>,
    /// Папки ящика: (id, имя).
    mailboxes: Vec<(String, String)>,
    /// Категории своего ящика (таблица S5-02).
    categories: Vec<CategoryChoice>,
    pub(crate) editor: Option<Draft>,
    value: Entity<InputState>,
    address: Entity<InputState>,
    sync_inputs: bool,
    /// Список ушёл на сервер, ждём ответа.
    pub(crate) busy: bool,
    /// Ждёт «Заменить»: имя чужого скрипта, заменится ли его текст, список правил.
    conflict: Option<(String, bool, Vec<Rule>)>,
    /// Последний отправленный список.
    pending: Option<Vec<Rule>>,
    confirm_delete: Option<usize>,
    error: Option<String>,
    note: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl RulesPanel {
    pub(crate) fn new(
        state: Option<RulesState>,
        mailboxes: Vec<(String, String)>,
        categories: Vec<CategoryChoice>,
        live: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let value = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("rules-value-placeholder")));
        let address = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("rules-address-placeholder")));
        let read = |this: &mut Self, ev: &InputEvent, cx: &mut Context<Self>| {
            if let InputEvent::Change = ev {
                this.read_inputs(cx);
            }
        };
        let subscriptions = vec![
            cx.subscribe_in(&value, window, move |this, _, ev: &InputEvent, _, cx| read(this, ev, cx)),
            cx.subscribe_in(&address, window, move |this, _, ev: &InputEvent, _, cx| read(this, ev, cx)),
        ];
        Self {
            live,
            state,
            mailboxes,
            categories,
            editor: None,
            value,
            address,
            sync_inputs: false,
            busy: false,
            conflict: None,
            pending: None,
            confirm_delete: None,
            error: None,
            note: None,
            _subscriptions: subscriptions,
        }
    }

    fn rules(&self) -> &[Rule] {
        self.state.as_ref().map_or(&[], |s| s.rules.as_slice())
    }

    fn read_inputs(&mut self, cx: &App) {
        if let Some(d) = &mut self.editor {
            d.value = self.value.read(cx).value().to_string();
            d.address = self.address.read(cx).value().to_string();
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
        self.note = Some(tr!("rules-saved"));
        cx.notify();
    }

    /// Сервер не принял скрипт или запрос не удался: сообщение в панели, редактор остаётся.
    pub(crate) fn failed(&mut self, error: String, cx: &mut Context<Self>) {
        self.busy = false;
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
        cx.notify();
    }

    /// Последний отправленный список (для строки «Заменить?» после ответа сервера).
    pub(crate) fn pending(&self) -> Option<&Vec<Rule>> {
        self.pending.as_ref()
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
        self.error = None;
        self.note = None;
        self.confirm_delete = None;
        cx.notify();
    }

    fn save_editor(&mut self, cx: &mut Context<Self>) {
        self.read_inputs(cx);
        let Some(d) = &self.editor else { return };
        match d.rule() {
            Ok(rule) => {
                let rules = apply(self.rules(), d.index, rule);
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
            self.save(rules, false, cx);
        }
    }

    fn set_editor(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Draft)) {
        if let Some(d) = &mut self.editor {
            f(d);
            self.error = None;
            cx.notify();
        }
    }
}

fn chip(id: impl Into<ElementId>, label: String, selected: bool) -> Stateful<Div> {
    let l = crate::roles::now();
    div()
        .id(id)
        .px_2()
        .py_0p5()
        .rounded_md()
        .cursor_pointer()
        .border_1()
        .border_color(if selected { l.accent() } else { l.border() })
        .when(selected, |d| d.bg(l.picked()))
        .when(!selected, |d| d.hover(|d| d.bg(l.fill())))
        .child(label)
}

fn link(id: impl Into<ElementId>, label: String) -> Stateful<Div> {
    let l = crate::roles::now();
    div().id(id).px_1().rounded_md().cursor_pointer().text_color(l.accent_text()).hover(|d| d.bg(l.hover())).child(label)
}

impl RulesPanel {
    fn render_row(&self, i: usize, r: &Rule, cx: &mut Context<Self>) -> Div {
        let l = crate::roles::look(cx);
        let confirm = self.confirm_delete == Some(i);
        let editing = self.editor.as_ref().is_some_and(|d| d.index == Some(i));
        div()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .when(editing, |d| d.bg(l.hover()))
            .child(
                div().id(("rule-on", i)).cursor_pointer().child(checkbox(r.enabled)).on_click(cx.listener(move |this, _, _, cx| this.toggle(i, cx))),
            )
            .child(div().flex_1().min_w_0().when(!r.enabled, |d| d.text_color(l.text2())).child(describe(r, &self.mailboxes, &self.categories)))
            .when(confirm, |d| {
                d.child(div().text_color(l.error()).child(tr!("rules-delete-confirm")))
                    .child(link(("rule-delete-yes", i), tr!("rules-delete")).on_click(cx.listener(move |this, _, _, cx| this.delete(i, cx))))
                    .child(link(("rule-delete-no", i), tr!("rules-cancel")).on_click(cx.listener(|this, _, _, cx| {
                        this.confirm_delete = None;
                        cx.notify();
                    })))
            })
            .when(!confirm, |d| {
                let rule = r.clone();
                d.child(link(("rule-edit", i), tr!("rules-edit")).on_click(cx.listener(move |this, _, _, cx| this.edit(Draft::of(i, &rule), cx))))
                    .child(link(("rule-delete", i), tr!("rules-delete")).on_click(cx.listener(move |this, _, _, cx| {
                        this.confirm_delete = Some(i);
                        cx.notify();
                    })))
            })
    }

    fn render_editor(&self, d: &Draft, cx: &mut Context<Self>) -> Div {
        let l = crate::roles::look(cx);
        let label = |text: String| div().w(px(90.)).flex_none().text_color(l.text2()).child(text);
        let fields = FIELDS.iter().enumerate().map(|(n, (field, key))| {
            let field = *field;
            chip(("rules-field", n), tr!(key), d.field == field).on_click(cx.listener(move |this, _, _, cx| this.set_editor(cx, |d| d.field = field)))
        });
        let kinds = KINDS.iter().enumerate().map(|(n, (kind, key))| {
            let kind = *kind;
            chip(("rules-kind", n), tr!(key), d.kind == kind).on_click(cx.listener(move |this, _, _, cx| this.set_editor(cx, |d| d.kind = kind)))
        });
        let picked = d.folder.as_ref().map(|(id, _)| id.clone());
        let folders = self.mailboxes.iter().enumerate().map(|(n, (id, name))| {
            let folder = (id.clone(), name.clone());
            chip(("rules-folder", n), name.clone(), picked.as_deref() == Some(id.as_str())).on_click(cx.listener(move |this, _, _, cx| {
                let folder = folder.clone();
                this.set_editor(cx, |d| d.folder = Some(folder))
            }))
        });
        let chosen = d.category.clone();
        let categories = self.categories.iter().enumerate().map(|(n, (keyword, name, color))| {
            let pick = keyword.clone();
            chip(("rules-category", n), name.clone(), chosen.as_deref() == Some(keyword.as_str()))
                .flex()
                .items_center()
                .gap_1p5()
                .child(div().size(px(10.)).rounded(px(3.)).bg(l.cat(color).dot))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let pick = pick.clone();
                    this.set_editor(cx, |d| d.category = Some(pick))
                }))
        });
        let busy = self.busy;
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(l.c(pets_ui::theme::Role::StrokeCard))
            .bg(l.hover())
            .child(div().font_weight(FontWeight::SEMIBOLD).child(if d.index.is_some() { tr!("rules-edit-title") } else { tr!("rules-new-title") }))
            .child(div().flex().items_center().gap_2().child(label(tr!("rules-if"))).children(fields))
            .child(div().flex().items_center().gap_2().child(label(tr!("rules-contains"))).child(div().flex_1().child(Input::new(&self.value))))
            .child(div().flex().flex_wrap().items_center().gap_2().child(label(tr!("rules-then"))).children(kinds))
            .when(d.kind == Kind::Move, |el| {
                el.child(div().flex().flex_wrap().items_center().gap_2().child(label(tr!("rules-folder"))).children(folders))
            })
            .when(d.kind == Kind::Category, |el| {
                el.child(div().flex().flex_wrap().items_center().gap_2().child(label(tr!("rules-category"))).children(categories))
            })
            .when(d.kind == Kind::Redirect, |el| {
                el.child(
                    div().flex().items_center().gap_2().child(label(tr!("rules-address"))).child(div().flex_1().child(Input::new(&self.address))),
                )
            })
            .when(d.kind == Kind::Discard, |el| el.child(div().text_color(l.text2()).child(tr!("rules-discard-hint"))))
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(
                        button("rules-editor-save", if busy { tr!("rules-saving") } else { tr!("rules-editor-save") }, !busy)
                            .on_click(cx.listener(|this, _, _, cx| this.save_editor(cx))),
                    )
                    .child(button("rules-editor-cancel", tr!("rules-cancel"), false).on_click(cx.listener(|this, _, _, cx| {
                        this.editor = None;
                        this.error = None;
                        cx.notify();
                    }))),
            )
    }
}

impl Render for RulesPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let l = crate::roles::look(cx);
        if std::mem::take(&mut self.sync_inputs)
            && let Some(d) = self.editor.clone()
        {
            self.value.update(cx, |s, cx| s.set_value(d.value, window, cx));
            self.address.update(cx, |s, cx| s.set_value(d.address, window, cx));
            self.value.update(cx, |s, cx| s.focus(window, cx));
        }
        let rules = self.rules().to_vec();
        let mut list = div().flex().flex_col().gap_0p5();
        for (i, r) in rules.iter().enumerate() {
            list = list.child(self.render_row(i, r, cx));
        }
        let editor = self.editor.clone().map(|d| self.render_editor(&d, cx));
        let foreign = self.state.as_ref().map(|s| s.foreign.clone()).unwrap_or_default();
        let active = self.state.as_ref().is_some_and(|s| s.active);
        // Раздел вкладки «Настройки» (S5-09): заголовок и пояснение — у вкладки, здесь — список и редактор.
        let card = div()
            .w_full()
            .max_w(px(820.))
            .flex()
            .flex_col()
            .gap_3()
            .when(!self.live, |d| d.child(div().text_color(l.text2()).child(tr!("rules-demo"))))
            .when(self.live && self.state.is_none(), |d| d.child(div().text_color(l.text2()).child(tr!("rules-loading"))))
            .when(!rules.is_empty() && !active && self.live, |d| d.child(div().text_color(l.text2()).child(tr!("rules-all-off"))))
            .when(rules.is_empty() && self.state.is_some(), |d| d.child(div().text_color(l.text2()).child(tr!("rules-empty"))))
            .child(list)
            .children(editor)
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
                        d.child(button("rules-add", tr!("rules-add"), true).on_click(cx.listener(|this, _, _, cx| {
                            let mut draft = Draft::new();
                            draft.folder = this.mailboxes.first().cloned();
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
        div().id("rules-panel").flex().items_start().flex_1().min_w_0().h_full().overflow_y_scroll().px_6().py_4().child(card)
    }
}

#[cfg(test)]
mod tests {
    // Не `super::*`: из `gpui_kit::*` пришёл бы атрибут `test` gpui вместо встроенного.
    use super::{Action, Draft, DraftError, Field, Kind, Rule, apply};

    fn draft(kind: Kind) -> Draft {
        Draft {
            index: None,
            enabled: true,
            field: Field::Subject,
            value: "  отчёт ".into(),
            kind,
            folder: Some(("f1".into(), "Отчёты".into())),
            address: " boss@corp.example ".into(),
            category: Some("pm.cat.tenders".into()),
        }
    }

    #[test]
    fn editor_makes_a_rule() {
        let r = draft(Kind::Move).rule().unwrap();
        assert_eq!(r.value, "отчёт");
        assert_eq!(r.action, Action::Move { mailbox_id: "f1".into(), name: "Отчёты".into() });
        assert_eq!(draft(Kind::Redirect).rule().unwrap().action, Action::Redirect("boss@corp.example".into()));
        assert_eq!(Draft { value: " ".into(), ..draft(Kind::Flag) }.rule(), Err(DraftError::Value));
        assert_eq!(Draft { folder: None, ..draft(Kind::Move) }.rule(), Err(DraftError::Folder));
        for bad in ["", "boss", "@corp.example", "boss@corp", "bo ss@corp.example"] {
            assert_eq!(Draft { address: bad.into(), ..draft(Kind::Redirect) }.rule(), Err(DraftError::Address), "{bad}");
        }
        // Категория (S5-26): слово из таблицы; не выбрана — отказ.
        assert_eq!(draft(Kind::Category).rule().unwrap().action, Action::Category("pm.cat.tenders".into()));
        assert_eq!(Draft { category: None, ..draft(Kind::Category) }.rule(), Err(DraftError::Category));
        // Туда и обратно.
        for kind in [Kind::Redirect, Kind::Category] {
            let r = draft(kind).rule().unwrap();
            assert_eq!(Draft::of(3, &r).rule(), Ok(r));
        }
    }

    #[test]
    fn edit_replaces_new_appends() {
        let r = |v: &str| Rule { enabled: true, field: Field::From, value: v.into(), action: Action::Flag };
        let list = vec![r("a"), r("b")];
        assert_eq!(apply(&list, Some(1), r("c")), vec![r("a"), r("c")]);
        assert_eq!(apply(&list, None, r("c")), vec![r("a"), r("b"), r("c")]);
        assert_eq!(apply(&list, Some(9), r("c")).len(), 3);
    }
}
