//! «Кому…» (T3-03, как «Выбор имён» Outlook): подписи «Кому», «Копия», «Скрытая» окна письма — кнопки; щелчок
//! открывает панель под строками адресатов, в самом письме (без модальных окон, CLAUDE.md). В панели — поиск, список
//! адресной книги письма (`compose::Book`: мои контакты, частые адресаты переписки, справочник; Exchange отдаёт
//! справочник только поиском — `Mailer::find_people`, найденные приходят в книгу), списки рассылки — со своей меткой.
//! Флажками отмечают нескольких, «Кому →», «Копия →», «Скрытая →» откладывают отмеченных в поле внизу панели; «ОК»
//! переносит отложенных в письмо чипами (отмеченные, но не отложенные, — в поле, у которого открыли панель), «Отмена» и
//! Esc ничего не меняют. Клавиши — таблицей команд (`recipients.*`): ↑/↓ — по списку (из поля поиска ↓ уводит фокус в
//! список, ↑ на первой строке — обратно в поле), Пробел — флажок, Enter — «ОК», Esc — закрыть.

use std::collections::HashSet;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::{Metric, Role};
use postman_core::contacts::{AddressBook, Source};

use crate::Mailer;
use crate::chrome::{glyph, icon};
use crate::compose::{Book, Field};
use crate::roles::Look;

/// Строк в списке — не больше (книга живого ящика — все корреспонденты кэша).
const LIMIT: usize = 200;
const ROW_H: f32 = 32.;
/// Видно строк списка.
const ROWS: f32 = 7.;

/// Откуда адресат — метка справа в строке.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Contact,
    Directory,
    Recent,
    /// Список рассылки справочника (`contacts::Kind::List`).
    List,
}

impl Kind {
    fn key(self) -> &'static str {
        match self {
            Kind::Contact => "recipients-kind-contact",
            Kind::Directory => "recipients-kind-directory",
            Kind::Recent => "recipients-kind-recent",
            Kind::List => "recipients-kind-list",
        }
    }
}

/// Строка списка.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Person {
    pub(crate) name: String,
    pub(crate) email: String,
    pub(crate) kind: Kind,
}

impl Person {
    /// Как подсказка адреса: «Имя <адрес>» — тот же чип, что набранный.
    pub(crate) fn display(&self) -> String {
        if self.name.is_empty() { self.email.clone() } else { format!("{} <{}>", self.name, self.email) }
    }

    fn same(&self, other: &Person) -> bool {
        self.email.eq_ignore_ascii_case(&other.email)
    }
}

/// Люди по запросу: пустой — частые и недавние первыми (затем по имени), иначе — как подсказки адреса (`suggest`:
/// начало слов, любая раскладка, транслит). `lists` — адреса списков рассылки, в нижнем регистре.
pub(crate) fn people(book: &AddressBook, lists: &HashSet<String>, query: &str, now: i64) -> Vec<Person> {
    let found: Vec<&postman_core::contacts::Entry> = if query.trim().is_empty() {
        let mut all: Vec<_> = book.entries().collect();
        all.sort_by(|a, b| b.count.cmp(&a.count).then(b.last.cmp(&a.last)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        all.truncate(LIMIT);
        all
    } else {
        book.suggest(query, LIMIT, now)
    };
    found
        .into_iter()
        .map(|e| Person {
            name: e.name.clone(),
            email: e.email.clone(),
            kind: if lists.contains(&e.email.to_lowercase()) {
                Kind::List
            } else {
                match e.source {
                    Source::Contact => Kind::Contact,
                    Source::Directory => Kind::Directory,
                    Source::Recent => Kind::Recent,
                }
            },
        })
        .collect()
}

/// Отложенные адресаты по полям письма.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Picked {
    pub(crate) to: Vec<Person>,
    pub(crate) cc: Vec<Person>,
    pub(crate) bcc: Vec<Person>,
}

impl Picked {
    pub(crate) fn get(&self, f: Field) -> &[Person] {
        match f {
            Field::Cc => &self.cc,
            Field::Bcc => &self.bcc,
            _ => &self.to,
        }
    }

    fn slot(&mut self, f: Field) -> &mut Vec<Person> {
        match f {
            Field::Cc => &mut self.cc,
            Field::Bcc => &mut self.bcc,
            _ => &mut self.to,
        }
    }

    /// В поле — без повторов по адресу.
    fn add(&mut self, f: Field, people: impl IntoIterator<Item = Person>) {
        let slot = self.slot(f);
        for p in people {
            if !slot.iter().any(|q| q.same(&p)) {
                slot.push(p);
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.to.is_empty() && self.cc.is_empty() && self.bcc.is_empty()
    }
}

/// Письму: панель закрыта — с отложенными («ОК») или без («Отмена», Esc).
pub(crate) enum PickerEvent {
    Done(Option<Picked>),
}

impl EventEmitter<PickerEvent> for Picker {}

pub(crate) struct Picker {
    /// Поле, у которого открыли панель: туда — отмеченные без «Кому →» и строка под курсором.
    pub(crate) field: Field,
    pub(crate) input: Entity<InputState>,
    focus: FocusHandle,
    book: Book,
    lists: HashSet<String>,
    mailer: WeakEntity<Mailer>,
    rows: Vec<Person>,
    cursor: usize,
    checked: Vec<Person>,
    pub(crate) picked: Picked,
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl Picker {
    /// `lists` — адреса списков рассылки (из «Контактов»): передаются сюда, а не читаются из экрана почты.
    pub(crate) fn new(
        field: Field,
        book: Book,
        lists: HashSet<String>,
        mailer: WeakEntity<Mailer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("recipients-placeholder")));
        let subscriptions = vec![cx.subscribe(&input, |this: &mut Self, _, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                this.typed(cx);
            }
        })];
        let mut p = Self {
            field,
            input,
            focus: cx.focus_handle(),
            book,
            lists,
            mailer,
            rows: Vec::new(),
            cursor: 0,
            checked: Vec::new(),
            picked: Picked::default(),
            scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        };
        p.refresh(cx);
        p
    }

    fn query(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    /// Набор: строки — сразу; справочник, который сервер отдаёт только поиском, — запросом (ответ — `refresh`).
    pub(crate) fn typed(&mut self, cx: &mut Context<Self>) {
        self.cursor = 0;
        self.refresh(cx);
        let text = self.query(cx);
        let _ = self.mailer.update(cx, |m, cx| m.find_people(&text, cx));
    }

    /// Строки заново (набор, справочник прислал людей).
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        let now = jiff::Timestamp::now().as_second();
        self.rows = people(&self.book.borrow(), &self.lists, &self.query(cx), now);
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
        cx.notify();
    }

    /// Справочник прислал людей: они уже в книге, списки рассылки — новые адреса.
    pub(crate) fn lists_changed(&mut self, lists: HashSet<String>, cx: &mut Context<Self>) {
        self.lists = lists;
        self.refresh(cx);
    }

    pub(crate) fn rows(&self) -> &[Person] {
        &self.rows
    }

    pub(crate) fn checked(&self) -> &[Person] {
        &self.checked
    }

    pub(crate) fn focus_search(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |s, cx| s.focus(window, cx));
    }

    fn in_search(&self, window: &Window, cx: &App) -> bool {
        self.input.focus_handle(cx).is_focused(window)
    }

    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let last = self.rows.len().saturating_sub(1);
        self.cursor = self.cursor.saturating_add_signed(delta).min(last);
        self.scroll.scroll_to_item(self.cursor);
        cx.notify();
    }

    /// ↓: по списку; из поля поиска — фокус в список (там Пробел — флажок, а не пробел в поле).
    fn next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.in_search(window, cx) {
            window.focus(&self.focus, cx);
            cx.notify();
            return;
        }
        self.step(1, cx);
    }

    /// ↑: по списку; на первой строке — обратно в поле поиска.
    fn prev(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.cursor == 0 && !self.in_search(window, cx) {
            self.focus_search(window, cx);
            cx.notify();
            return;
        }
        self.step(-1, cx);
    }

    fn toggle(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(p) = self.rows.get(i).cloned() else { return };
        self.cursor = i;
        match self.checked.iter().position(|q| q.same(&p)) {
            Some(at) => {
                self.checked.remove(at);
            }
            None => self.checked.push(p),
        }
        cx.notify();
    }

    /// «Кому →», «Копия →», «Скрытая →»: отмеченные — в поле внизу панели, флажки сняты; ничего не отмечено — строка
    /// под курсором.
    pub(crate) fn add_to(&mut self, f: Field, cx: &mut Context<Self>) {
        let people =
            if self.checked.is_empty() { self.rows.get(self.cursor).cloned().into_iter().collect() } else { std::mem::take(&mut self.checked) };
        self.picked.add(f, people);
        cx.notify();
    }

    /// «ОК»: отмеченные — в поле, у которого открыли панель; ничего не отложено и не отмечено — строка под курсором.
    pub(crate) fn apply(&mut self, cx: &mut Context<Self>) {
        if !self.checked.is_empty() || self.picked.is_empty() {
            self.add_to(self.field, cx);
        }
        cx.emit(PickerEvent::Done(Some(std::mem::take(&mut self.picked))));
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        cx.emit(PickerEvent::Done(None));
    }

    fn row(&self, i: usize, p: &Person, l: &Look, cx: &mut Context<Self>) -> Stateful<Div> {
        let on = i == self.cursor;
        let checked = self.checked.iter().any(|q| q.same(p));
        let id = ElementId::NamedInteger("recipients-row".into(), i as u64);
        div()
            .id(id.clone())
            .children(crate::screens::spot(id))
            .relative()
            .h(px(ROW_H))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(10.))
            .pl(px(12.))
            .pr(px(10.))
            .rounded(l.px(Metric::RadiusControl))
            .cursor_pointer()
            .when(on, |d| d.bg(l.pressed()))
            .when(!on, |d| d.hover(|d| d.bg(l.hover())))
            .child(div().absolute().left_0().top(px(7.)).bottom(px(7.)).w(px(3.)).rounded(px(2.)).when(on, |d| d.bg(l.accent())))
            .child(crate::list::checkbox(checked, l))
            .when(p.kind == Kind::List, |d| d.child(icon(glyph::MAIL, 12.).text_color(l.text2())))
            .child(div().flex_none().max_w(px(240.)).truncate().child(if p.name.is_empty() { p.email.clone() } else { p.name.clone() }))
            .child(div().flex_1().min_w_0().truncate().text_color(l.text2()).when(!p.name.is_empty(), |d| d.child(p.email.clone())))
            .child(div().flex_none().text_size(l.px(Metric::TypeCaption)).text_color(l.text3()).child(tr!(p.kind.key())))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle(i, cx)))
    }

    /// Строка поля внизу: кнопка «Кому →» и отложенные адресаты.
    fn slot_row(&self, f: Field, l: &Look, cx: &mut Context<Self>) -> Div {
        let (id, key) = match f {
            Field::Cc => ("recipients-add-cc", "recipients-add-cc"),
            Field::Bcc => ("recipients-add-bcc", "recipients-add-bcc"),
            _ => ("recipients-add-to", "recipients-add-to"),
        };
        let names: Vec<String> = self.picked.get(f).iter().map(|p| if p.name.is_empty() { p.email.clone() } else { p.name.clone() }).collect();
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(
                crate::buttons::secondary(id, l)
                    .w(px(112.))
                    .justify_center()
                    .child(tr!(key))
                    .on_click(cx.listener(move |this, _, _, cx| this.add_to(f, cx))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h(px(28.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .rounded(l.px(Metric::RadiusControl))
                    .border_1()
                    .border_color(l.border())
                    .truncate()
                    .child(names.join("; ")),
            )
    }
}

impl Focusable for Picker {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Picker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let l = crate::roles::look(cx);
        let head = div()
            .h(px(40.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(10.))
            .pl(px(14.))
            .pr(px(12.))
            .border_b_1()
            .border_color(l.border())
            .child(icon(glyph::SEARCH, 16.).text_color(l.text3()))
            .child(div().flex_1().min_w_0().child(Input::new(&self.input).appearance(false)))
            .when(!self.checked.is_empty(), |d| {
                d.child(
                    div()
                        .flex_none()
                        .text_size(l.px(Metric::TypeCaption))
                        .text_color(l.text2())
                        .child(tr!("recipients-checked", n = self.checked.len() as i64)),
                )
            });
        let rows: Vec<AnyElement> = self.rows.iter().enumerate().map(|(i, p)| self.row(i, p, &l, cx).into_any_element()).collect();
        let empty = self.rows.is_empty();
        let list = div()
            .id("recipients-list")
            .h(px(ROW_H * ROWS + 8.))
            .flex_none()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .px(px(6.))
            .py(px(4.))
            .flex()
            .flex_col()
            .children(rows)
            .when(empty, |d| d.child(div().h(px(ROW_H)).flex().items_center().px(px(12.)).text_color(l.text2()).child(tr!("recipients-nothing"))));
        let buttons = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(div().flex_1().text_size(l.px(Metric::TypeCaption)).text_color(l.text2()).child(tr!("recipients-hint")))
            .child(
                crate::buttons::button("recipients-cancel", tr!("recipients-cancel"), false, &l)
                    .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
            )
            .child(crate::buttons::button("recipients-ok", tr!("recipients-ok"), true, &l).on_click(cx.listener(|this, _, _, cx| this.apply(cx))));
        let footer = div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .p(px(12.))
            .border_t_1()
            .border_color(l.border())
            .child(self.slot_row(Field::To, &l, cx))
            .child(self.slot_row(Field::Cc, &l, cx))
            .child(self.slot_row(Field::Bcc, &l, cx))
            .child(buttons.pt(px(4.)));
        div()
            .id("recipients")
            .key_context("Recipients")
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .my(px(8.))
            .overflow_hidden()
            .rounded(l.px(Metric::RadiusOverlay))
            .border_1()
            .border_color(l.c(Role::StrokeCard))
            .bg(l.c(Role::LayerOverlay))
            .text_size(px(13.))
            .text_color(l.text())
            .on_action(cx.listener(|this, _: &recipients::Next, window, cx| this.next(window, cx)))
            .on_action(cx.listener(|this, _: &recipients::Prev, window, cx| this.prev(window, cx)))
            .on_action(cx.listener(|this, _: &recipients::Toggle, _, cx| {
                let i = this.cursor;
                this.toggle(i, cx);
            }))
            .on_action(cx.listener(|this, _: &recipients::Apply, _, cx| this.apply(cx)))
            .on_action(cx.listener(|this, _: &recipients::Close, _, cx| this.close(cx)))
            .child(head)
            .child(list)
            .child(footer)
    }
}

/// Опись части (ADR 0005, `parts.rs`): команды панели; обработчики — на самой панели.
pub(crate) const PART: crate::part::Part = crate::part::Part { name: "recipients", texts: TEXTS, commands: COMMANDS, ..crate::part::Part::NONE };

/// Тексты части (`i18n/<язык>/<часть>.ftl`).
const TEXTS: &[pets_i18n::Resource] = &[("ru", include_str!("../i18n/ru/recipients.ftl")), ("en", include_str!("../i18n/en/recipients.ftl"))];

/// Панель «Кому…» (T3-03): по строкам, флажок, «ОК», закрыть.
// Имя — область действий (`recipients::…`), как в id команд: так обработчики читаются и их находит `docs/PARITY.md`.
#[allow(clippy::module_inception)]
pub(crate) mod recipients {
    gpui_kit::actions!(recipients, [Next, Prev, Toggle, Apply, Close]);
}

/// Панель «Кому…», в том числе её поле поиска: ↑/↓, Enter, Esc поля — клавиши панели (как у палитры).
pub(crate) const RECIPIENTS: Option<&str> = Some("Recipients || (Recipients > Input)");

/// Команды части (`commands.rs`: контексты и `cmd`).
const COMMANDS: &[pets_commands::Command] = {
    use crate::commands::cmd;
    &[
        cmd("recipients.next", &["down"], RECIPIENTS),
        cmd("recipients.prev", &["up"], RECIPIENTS),
        // В поле поиска пробел — текст: флажок — в списке (↓ из поля).
        cmd("recipients.toggle", &["space"], Some("Recipients && !Input")),
        cmd("recipients.apply", &["enter"], RECIPIENTS),
        cmd("recipients.close", &["escape"], RECIPIENTS),
    ]
};

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use postman_core::contacts::{AddressBook, Source};

    use super::{Kind, Person, Picked, people};
    use crate::compose::Field;

    fn book() -> AddressBook {
        let mut b = AddressBook::default();
        b.touch("Иван Петров", "petrov@corp.example", Source::Contact, 0, 0);
        b.touch("Ирина Смирнова", "smirnova@corp.example", Source::Directory, 0, 0);
        b.touch("Отдел продаж", "sales@corp.example", Source::Directory, 0, 0);
        b.touch("Пётр Частый", "often@partner.example", Source::Recent, 9, 1_000);
        b
    }

    /// Пустой запрос — частые первыми; запрос — как подсказки адреса, в другой раскладке тоже; список рассылки —
    /// своей меткой.
    #[test]
    fn people_follow_the_query() {
        let lists: HashSet<String> = ["sales@corp.example".to_owned()].into();
        let all = people(&book(), &lists, "", 2_000);
        assert_eq!(all.len(), 4);
        assert_eq!(all[0].email, "often@partner.example");
        let ir = people(&book(), &lists, "ир", 2_000); // i18n-ok: запрос теста
        assert_eq!(ir.iter().map(|p| p.email.as_str()).collect::<Vec<_>>(), ["smirnova@corp.example"]);
        assert_eq!(people(&book(), &lists, "bh", 2_000), ir, "«ир» в английской раскладке");
        let sales = people(&book(), &lists, "отдел", 2_000); // i18n-ok: запрос теста
        assert_eq!(sales[0].kind, Kind::List);
        assert_eq!(people(&book(), &lists, "петров", 2_000)[0].kind, Kind::Contact); // i18n-ok: запрос теста
    }

    /// Отложенные — без повторов по адресу, у каждого поля свои.
    #[test]
    fn picked_has_no_duplicates() {
        let p = |e: &str| Person { name: String::new(), email: e.into(), kind: Kind::Recent };
        let mut picked = Picked::default();
        picked.add(Field::To, [p("a@x.example"), p("A@x.example"), p("b@x.example")]);
        picked.add(Field::Cc, [p("a@x.example")]);
        assert_eq!(picked.get(Field::To).len(), 2);
        assert_eq!(picked.get(Field::Cc).len(), 1);
        assert!(picked.get(Field::Bcc).is_empty());
        assert_eq!(p("a@x.example").display(), "a@x.example");
    }
}
