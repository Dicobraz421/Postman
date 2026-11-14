//! @упоминания в окне письма и в быстром ответе ленты (MSGR-02, решение владельца
//! `docs/decisions/2026-11-14-messenger-comforts.md`): набрали `@` — подсказка из участников беседы и адресной книги;
//! выбранный человек — `**@Имя**` в тексте (полужирным) и в «Кому», если его там ещё нет. Разбор набранного —
//! `postman_compose::mention`; признак «упомянули меня» у пришедших писем — `postman_core::filters::Me::mentioned`.

use gpui_kit::component::input::{Enter, IndentInline, MoveDown, MoveUp};
use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::{Metric, Role};
use postman_compose::{Address, mention, parse_addresses};

use crate::compose::{Book, Field, Model, compose_field, segments};
use crate::composer::Composer;
use crate::roles::Look;

/// Подсказок под полем.
const SHOWN: usize = 6;

/// Подсказка упоминания: где `@`, где курсор, кто подходит, кто выбран.
#[derive(Default, Debug, Clone, PartialEq)]
pub(crate) struct Mentions {
    start: usize,
    cursor: usize,
    pub(crate) items: Vec<Address>,
    pub(crate) selected: usize,
}

impl Mentions {
    pub(crate) fn open(&self) -> bool {
        !self.items.is_empty()
    }

    pub(crate) fn clear(&mut self) {
        self.items.clear();
        self.selected = 0;
    }

    /// По тексту и курсору: запрос `@…` и подходящие люди — сначала участники беседы, затем адресная книга, без
    /// повторов и без меня.
    pub(crate) fn refresh(&mut self, text: &str, cursor: usize, people: &[Address], book: Option<&Book>, me: &[String]) {
        let Some((start, query)) = mention::query_at(text, cursor) else { return self.clear() };
        let now = jiff::Timestamp::now().as_second();
        let mut items: Vec<Address> = people.iter().filter(|a| mention::matches(&a.name, &a.email, &query)).cloned().collect();
        if let Some(book) = book
            && !query.is_empty()
        {
            let book = book.borrow();
            items.extend(book.suggest(&query, SHOWN, now).into_iter().map(|e| Address::new(&e.name, &e.email)));
        }
        let mut seen = std::collections::HashSet::new();
        items.retain(|a| !a.email.is_empty() && !me.iter().any(|m| m.eq_ignore_ascii_case(&a.email)) && seen.insert(a.email.to_lowercase()));
        items.truncate(SHOWN);
        let same = self.start == start && self.items == items;
        (self.start, self.cursor, self.items) = (start, cursor, items);
        if !same {
            self.selected = 0;
        }
    }

    pub(crate) fn step(&mut self, delta: isize) {
        let n = self.items.len();
        if n > 0 {
            self.selected = (self.selected as isize + delta).rem_euclid(n as isize) as usize;
        }
    }

    /// Выбранный человек и что заменить в поле: набранное `@…` (байты) — на `**@Имя** `.
    pub(crate) fn take(&mut self, i: Option<usize>) -> Option<(std::ops::Range<usize>, String, Address)> {
        let who = self.items.get(i.unwrap_or(self.selected))?.clone();
        let token = format!("**@{}** ", mention::shown(&who.name, &who.email));
        let range = self.start..self.cursor.max(self.start);
        self.clear();
        Some((range, token, who))
    }

    /// Выбранный человек: текст с упоминанием вместо `@…`, курсор после него.
    #[cfg(test)]
    pub(crate) fn apply(&mut self, text: &str, i: Option<usize>) -> Option<(String, usize, Address)> {
        let who = self.items.get(i.unwrap_or(self.selected))?.clone();
        let (next, at) = mention::insert(text, self.start, self.cursor, &mention::shown(&who.name, &who.email));
        self.clear();
        Some((next, at, who))
    }
}

/// Человек — в «Кому», если его нет ни в «Кому», ни в «Копии», ни в «Скрытой». `true` — добавлен.
pub(crate) fn add_recipient(m: &mut Model, who: &Address) -> bool {
    let known =
        [Field::To, Field::Cc, Field::Bcc].iter().any(|f| parse_addresses(m.field(*f)).0.iter().any(|a| a.email.eq_ignore_ascii_case(&who.email)));
    if known {
        return false;
    }
    let (mut closed, open) = segments(m.field(Field::To));
    closed.push(who.display());
    m.set(Field::To, compose_field(&closed, &open));
    true
}

/// Подсказка под полем: имя и адрес, выбранный — подсветкой; щелчок — выбрать.
pub(crate) fn panel<V: 'static>(
    m: &Mentions,
    l: &Look,
    cx: &mut Context<V>,
    pick: impl Fn(&mut V, usize, &mut Window, &mut Context<V>) + Clone + 'static,
) -> Option<Stateful<Div>> {
    if !m.open() {
        return None;
    }
    let rows = m.items.iter().enumerate().map(|(i, a)| {
        let pick = pick.clone();
        crate::composer::menu_item(ElementId::from(("mention", i)), l, i == m.selected)
            .child(div().flex_none().child(mention::shown(&a.name, &a.email)))
            .child(div().min_w_0().truncate().text_color(l.text2()).child(a.email.clone()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |v, _, window, cx| {
                    cx.stop_propagation();
                    pick(v, i, window, cx);
                }),
            )
    });
    Some(
        div()
            .id("mentions")
            .children(crate::screens::spot("mentions"))
            .mx(px(16.))
            .mb(px(4.))
            .flex()
            .flex_col()
            .p(px(4.))
            .rounded(l.px(Metric::RadiusOverlay))
            .border_1()
            .border_color(l.c(Role::StrokeCard))
            .bg(l.c(Role::LayerOverlay))
            .shadow_md()
            .text_size(px(13.))
            .child(div().px(px(10.)).py(px(2.)).text_size(px(12.)).text_color(l.text2()).child(tr!("mentions-title")))
            .children(rows),
    )
}

// --- окно письма ---

impl Composer {
    /// Текст письма изменился: подсказка упоминания у курсора.
    pub(crate) fn mention_changed(&mut self, cursor: usize, cx: &mut Context<Self>) {
        let people = self.mention_people(cx);
        let me: Vec<String> = std::iter::once(self.model.from().email.clone()).collect();
        let was = (self.mention.clone(), self.board.mention_in);
        self.board.mention_in = None;
        self.mention.refresh(&self.model.body, cursor, &people, Some(&self.book), &me);
        if (self.mention.clone(), self.board.mention_in) != was {
            cx.notify();
        }
    }

    /// Ответ на реплику `id` изменился (MSGR-01): подсказка упоминания у курсора — под этим полем.
    pub(crate) fn answer_mention_changed(&mut self, id: u64, text: &str, cursor: usize, cx: &mut Context<Self>) {
        let people = self.mention_people(cx);
        let me: Vec<String> = std::iter::once(self.model.from().email.clone()).collect();
        let was = (self.mention.clone(), self.board.mention_in);
        let mut m = if self.board.mention_in == Some(id) { self.mention.clone() } else { Mentions::default() };
        m.refresh(text, cursor, &people, Some(&self.book), &me);
        if m.open() {
            (self.mention, self.board.mention_in) = (m, Some(id));
        } else if self.board.mention_in == Some(id) {
            self.mention.clear();
            self.board.mention_in = None;
        }
        if (self.mention.clone(), self.board.mention_in) != was {
            cx.notify();
        }
    }

    /// Подсказка упоминания открыта у своего текста письма (а не у ответа на реплику).
    fn body_mention_open(&self) -> bool {
        self.mention.open() && self.board.mention_in.is_none()
    }

    /// Упоминание в ответе на реплику `id`: `@…` — на `**@Имя**`, человек — в «Кому».
    pub(crate) fn answer_mention_pick(&mut self, id: u64, i: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.board.answers.get(&id).map(|(s, _)| s.clone()) else { return };
        let Some((range, token, who)) = self.mention.take(i) else { return };
        self.board.mention_in = None;
        state.update(cx, |s, cx| {
            s.set_selected_range(range, cx);
            s.replace(token, window, cx);
            s.focus(window, cx);
        });
        add_recipient(&mut self.model, &who);
        cx.notify();
    }

    /// Упомянуть `i`-го из подсказки (или выбранного).
    pub(crate) fn mention_pick(&mut self, i: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some((range, token, who)) = self.mention.take(i) else { return };
        // Как набор: выделить `@…` и заменить — правка поля, отмена Ctrl+Z, оформление Markdown.
        self.body.update(cx, |s, cx| {
            s.set_selected_range(range, cx);
            s.replace(token, window, cx);
            s.focus(window, cx);
        });
        // Чип «Кому» рисуется по модели: поля ввода не переписываем (это сбросило бы курсор в тексте).
        add_recipient(&mut self.model, &who);
        cx.notify();
    }

    /// Участники беседы открытого письма (лента) и адресаты этого письма.
    fn mention_people(&self, cx: &App) -> Vec<Address> {
        let mut out: Vec<Address> = [Field::To, Field::Cc].iter().flat_map(|f| parse_addresses(self.model.field(*f)).0).collect();
        if let Some(m) = self.mailer.upgrade() {
            out.extend(crate::quick::participants(m.read(cx).reading.read(cx)));
        }
        out
    }
}

/// Клавиши подсказки в поле письма: ↑ / ↓ — по людям, Enter и Tab — выбрать; без подсказки — как обычно.
pub(crate) fn capture(head: Div, cx: &mut Context<Composer>) -> Div {
    keys(head, cx, |c| c.body_mention_open(), |c, window, cx| c.mention_pick(None, window, cx))
}

/// Клавиши подсказки в поле ответа на реплику `id` — как в тексте письма.
pub(crate) fn answer_capture(field: Div, id: u64, cx: &mut Context<Composer>) -> Div {
    keys(field, cx, move |c| c.mention.open() && c.board.mention_in == Some(id), move |c, window, cx| c.answer_mention_pick(id, None, window, cx))
}

/// ↑ / ↓ — по людям, Enter и Tab — выбрать, Esc — закрыть, пока подсказка открыта у этого поля (`open`).
fn keys(
    el: Div,
    cx: &mut Context<Composer>,
    open: impl Fn(&Composer) -> bool + Clone + 'static,
    pick: impl Fn(&mut Composer, &mut Window, &mut Context<Composer>) + Clone + 'static,
) -> Div {
    let (o1, o2, o3, o4, o5) = (open.clone(), open.clone(), open.clone(), open.clone(), open);
    let p1 = pick.clone();
    el.capture_action(cx.listener(move |this, _: &Enter, window, cx| {
        if o1(this) {
            cx.stop_propagation();
            p1(this, window, cx);
        }
    }))
    .capture_action(cx.listener(move |this, _: &IndentInline, window, cx| {
        if o2(this) {
            cx.stop_propagation();
            pick(this, window, cx);
        }
    }))
    .capture_action(cx.listener(move |this, _: &MoveUp, _, cx| {
        if o3(this) {
            cx.stop_propagation();
            this.mention.step(-1);
            cx.notify();
        }
    }))
    .capture_action(cx.listener(move |this, _: &MoveDown, _, cx| {
        if o4(this) {
            cx.stop_propagation();
            this.mention.step(1);
            cx.notify();
        }
    }))
    .capture_action(cx.listener(move |this, _: &crate::compose::compose::Close, _, cx| {
        if o5(this) {
            cx.stop_propagation();
            this.mention.clear();
            this.board.mention_in = None;
            cx.notify();
        }
    }))
}

/// Подсказка упоминания под текстом письма.
pub(crate) fn composer_panel(c: &Composer, cx: &mut Context<Composer>) -> Option<AnyElement> {
    if c.board.mention_in.is_some() {
        return None;
    }
    let l = crate::roles::look(cx);
    panel(&c.mention, &l, cx, |this: &mut Composer, i, window, cx| this.mention_pick(Some(i), window, cx)).map(IntoElement::into_any_element)
}

/// Подсказка упоминания под полем ответа на реплику `id`.
pub(crate) fn answer_panel(c: &Composer, id: u64, l: &Look, cx: &mut Context<Composer>) -> Option<Stateful<Div>> {
    panel(&c.mention, l, cx, move |this: &mut Composer, i, window, cx| this.answer_mention_pick(id, Some(i), window, cx)).map(|p| p.ml(px(20.)))
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use postman_compose::{Address, Draft};

    use super::{Mentions, add_recipient};
    use crate::compose::{Field, Model};

    /// `@` в тексте — подсказка участников беседы; выбранный — `**@Имя**` и в «Кому», уже бывший там — не дважды.
    #[test]
    fn mention_inserts_name_and_adds_to_recipients() {
        let people = [Address::new("Анна Смирнова", "anna@corp.example"), Address::new("Иван Петров", "ivan@corp.example")];
        let mut m = Mentions::default();
        let text = "Коллеги, @смир";
        m.refresh(text, text.len(), &people, None, &["me@corp.example".into()]);
        assert_eq!(m.items, [people[0].clone()]);
        let (next, at, who) = m.apply(text, None).unwrap();
        assert_eq!(next, "Коллеги, **@Анна Смирнова** ");
        assert_eq!(at, next.len());
        assert!(!m.open());
        let mut model =
            Model::new(Draft { to: vec![Address::new("Иван Петров", "ivan@corp.example")], ..Default::default() }, None, "", Instant::now());
        assert!(add_recipient(&mut model, &who));
        assert!(model.field(Field::To).contains("anna@corp.example"), "{}", model.field(Field::To));
        assert!(!add_recipient(&mut model, &who), "второй раз — нет");
        assert!(!add_recipient(&mut model, &people[1]));
        // Себя не подсказываем; адрес — не упоминание.
        m.refresh("@", 1, &[Address::new("Я", "me@corp.example")], None, &["me@corp.example".into()]);
        assert!(!m.open());
        m.refresh("a@b", 3, &people, None, &[]);
        assert!(!m.open());
    }
}
