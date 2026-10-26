//! «Настройки → Категории» (S5-09, DESIGN «Цвет»): таблица категорий своей учётной записи, правка на месте — имя, цвет
//! из общих цветов Fluent 2 (близкие к цветам других категорий и к занятым красному и янтарному — перечёркнуты, с
//! причиной), клавиша 1–9. Удаление скрывает категорию; снять её с писем — отдельным явным выбором. Проверки — в
//! `postman_core::labels::Categories` (`add`, `rename`, `recolor`, `set_key`, `delete`); запись на сервер — экран
//! почты (`SettingsEvent::Categories`, FileNode).

use gpui_kit::component::input::{Input, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::Metric;
use postman_core::labels::{Categories, Category, Problem};
use postman_core::palette::{self, Clash, Reserved};

use crate::buttons::button;
use crate::roles::Look;
use crate::settings_tab::{SettingsEvent, SettingsView, aside_card, link, now_ms};

/// Правка категории на месте.
pub(crate) struct CatEdit {
    /// Ключевое слово правимой; `None` — новая.
    pub(crate) keyword: Option<String>,
    pub(crate) name: Entity<InputState>,
    pub(crate) color: &'static str,
    pub(crate) key: Option<u8>,
    pub(crate) error: Option<String>,
}

/// Почему цвет нельзя дать категории (для перечёркнутого образца).
pub(crate) fn clash_text(cats: &Categories, clash: Clash) -> String {
    match clash {
        Clash::Reserved(Reserved::Threat) => tr!("settings-cat-color-threat"),
        Clash::Reserved(Reserved::Deadline) => tr!("settings-cat-color-deadline"),
        Clash::Color(color) => {
            let name = cats.visible().find(|c| c.shown_color() == color).map(|c| c.name.clone()).unwrap_or_default();
            tr!("settings-cat-color-near", name = name)
        }
        Clash::Unknown => tr!("settings-cat-color-unknown"),
    }
}

/// Отказ таблицы — по-человечески.
pub(crate) fn problem_text(cats: &Categories, p: &Problem) -> String {
    match p {
        Problem::BadName => tr!("settings-cat-bad-name", max = postman_core::labels::NAME_MAX),
        Problem::DuplicateName(n) => tr!("settings-cat-duplicate-name", name = n.clone()),
        Problem::DuplicateKey(k) => {
            let owner = cats.by_key(*k).map(|c| c.name.clone()).unwrap_or_default();
            tr!("settings-cat-duplicate-key", key = *k, name = owner)
        }
        Problem::BadKey(_) => tr!("settings-cat-bad-key"),
        Problem::Colour { clash, .. } => clash_text(cats, *clash),
        Problem::NotFound(_) => tr!("settings-cat-not-found"),
        Problem::BadKeyword(_) | Problem::DuplicateKeyword(_) => tr!("settings-cat-bad-keyword"),
    }
}

/// Применить правку к таблице: новая — в конец, правимая — имя, цвет, клавиша. Ответ — новая таблица.
pub(crate) fn apply(cats: &Categories, keyword: Option<&str>, name: &str, color: &str, key: Option<u8>, now: i64) -> Result<Categories, Problem> {
    let mut t = cats.clone();
    match keyword {
        None => {
            t.add(name, color, key, now)?;
        }
        Some(k) => {
            t.rename(k, name, now)?;
            t.recolor(k, color, now)?;
            t.set_key(k, key, now)?;
        }
    }
    Ok(t)
}

/// Свободный цвет для новой категории (первый без близких); все заняты — первый из палитры (отказ покажет причину).
fn free_color(cats: &Categories) -> &'static str {
    let colors = cats.colors(None);
    colors.iter().find(|(_, c)| c.is_none()).or(colors.first()).map_or(palette::BLUE, |(k, _)| *k)
}

/// Свободная клавиша 1–9.
fn free_key(cats: &Categories) -> Option<u8> {
    (1..=9u8).find(|k| cats.by_key(*k).is_none())
}

impl SettingsView {
    pub(crate) fn edit_category(&mut self, c: Option<&Category>, window: &mut Window, cx: &mut Context<Self>) {
        let (keyword, name, color, key) = match c {
            Some(c) => (Some(c.keyword.clone()), c.name.clone(), c.shown_color(), c.key),
            None => (None, String::new(), free_color(&self.cats), free_key(&self.cats)),
        };
        let state = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("settings-cat-name-placeholder")).default_value(name));
        state.update(cx, |s, cx| s.focus(window, cx));
        self.cat_edit = Some(CatEdit { keyword, name: state, color, key, error: None });
        self.cat_purge = None;
        cx.notify();
    }

    fn save_category(&mut self, cx: &mut Context<Self>) {
        let Some(e) = &self.cat_edit else { return };
        let name = e.name.read(cx).value().to_string();
        match apply(&self.cats, e.keyword.as_deref(), &name, e.color, e.key, now_ms()) {
            Ok(t) => {
                self.cats = t.clone();
                self.cat_edit = None;
                cx.emit(SettingsEvent::Categories(t));
            }
            Err(p) => {
                let text = problem_text(&self.cats, &p);
                if let Some(e) = &mut self.cat_edit {
                    e.error = Some(text);
                }
            }
        }
        cx.notify();
    }

    fn delete_category(&mut self, keyword: String, cx: &mut Context<Self>) {
        let mut t = self.cats.clone();
        if let Err(p) = t.delete(&keyword, now_ms()) {
            tracing::warn!("categories: delete {keyword}: {p}");
            return;
        }
        self.cats = t.clone();
        self.cat_edit = None;
        cx.emit(SettingsEvent::Categories(t));
        cx.notify();
    }

    /// Вернуть удалённую категорию: прежний цвет может быть уже занят — тогда правка на месте с причиной.
    fn restore_category(&mut self, c: Category, window: &mut Window, cx: &mut Context<Self>) {
        let mut t = self.cats.clone();
        match t.adopt(&c.keyword, &c.name, c.shown_color(), None, now_ms()) {
            Ok(()) => {
                self.cats = t.clone();
                cx.emit(SettingsEvent::Categories(t));
                cx.notify();
            }
            Err(p) => {
                // Прежний цвет занят: вернуть со свободным и открыть правку с причиной.
                let text = problem_text(&self.cats, &p);
                let mut t = self.cats.clone();
                if let Err(e) = t.adopt(&c.keyword, &c.name, free_color(&self.cats), None, now_ms()) {
                    tracing::warn!("categories: restore {}: {e}", c.keyword);
                    return;
                }
                self.cats = t.clone();
                cx.emit(SettingsEvent::Categories(t));
                let restored = self.cats.by_keyword(&c.keyword).map(|(_, c)| c.clone());
                self.edit_category(restored.as_ref(), window, cx);
                if let Some(e) = &mut self.cat_edit {
                    e.error = Some(text);
                }
            }
        }
    }

    fn swatch(l: &Look, color: &str, size: f32) -> Div {
        div().size(px(size)).flex_none().rounded(px(4.)).bg(l.cat(color).dot)
    }

    fn cat_editor(&self, e: &CatEdit, l: &Look, cx: &mut Context<Self>) -> Div {
        let colors = self.cats.colors(e.keyword.as_deref());
        let free = colors.iter().filter(|(_, c)| c.is_none()).count();
        let mut swatches = div().flex().flex_wrap().gap_2();
        for (i, (key, clash)) in colors.iter().copied().enumerate() {
            let on = key == e.color;
            let reason = clash.map(|(c, _)| clash_text(&self.cats, c));
            let s = l.cat(key);
            let mut b = div()
                .id(("settings-cat-color", i))
                .size(px(28.))
                .relative()
                .rounded(px(6.))
                .bg(s.dot)
                .cursor_pointer()
                .when(on, |d| d.border_2().border_color(l.text()));
            if let Some(r) = reason.clone() {
                b = b
                    .child(div().absolute().inset_0().rounded(px(6.)).bg(l.panel()).opacity(0.55))
                    .child(div().absolute().left(px(3.)).right(px(3.)).top(px(13.)).h(px(2.)).bg(l.text2()))
                    .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(r.clone()).build(window, cx));
            }
            swatches = swatches.child(b.on_click(cx.listener(move |this, _, _, cx| {
                if let Some(e) = &mut this.cat_edit {
                    match &reason {
                        Some(r) => e.error = Some(r.clone()),
                        None => {
                            e.color = key;
                            e.error = None;
                        }
                    }
                }
                cx.notify();
            })));
        }
        let mut keys = div().flex().flex_wrap().gap_1();
        for k in 1..=9u8 {
            let owner = self.cats.by_key(k).filter(|c| Some(c.keyword.as_str()) != e.keyword.as_deref()).map(|c| c.name.clone());
            let on = e.key == Some(k);
            let mut chip = div()
                .id(("settings-cat-key", k as usize))
                .size(px(28.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(l.px(Metric::RadiusControl))
                .border_1()
                .when(on, |d| d.border_color(l.accent()).bg(l.picked()).font_weight(FontWeight::SEMIBOLD))
                .when(!on, |d| d.border_color(l.stroke()))
                .child(k.to_string());
            match owner {
                Some(name) => {
                    let tip = tr!("settings-cat-key-taken", name = name);
                    chip = chip
                        .text_color(l.c(pets_ui::theme::Role::TextDisabled))
                        .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx));
                }
                None => {
                    chip = chip.cursor_pointer().hover(|d| d.bg(l.hover())).on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(e) = &mut this.cat_edit {
                            e.key = Some(k);
                        }
                        cx.notify();
                    }));
                }
            }
            keys = keys.child(chip);
        }
        let none_on = e.key.is_none();
        keys = keys.child(
            div()
                .id("settings-cat-key-none")
                .h(px(28.))
                .px_2()
                .flex()
                .items_center()
                .rounded(l.px(Metric::RadiusControl))
                .border_1()
                .border_color(if none_on { l.accent() } else { l.stroke() })
                .when(none_on, |d| d.bg(l.picked()))
                .cursor_pointer()
                .child(tr!("settings-cat-key-none"))
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(e) = &mut this.cat_edit {
                        e.key = None;
                    }
                    cx.notify();
                })),
        );
        let label = |t: String| div().w(px(110.)).flex_none().pt_1().text_color(l.text2()).child(t);
        let title = if e.keyword.is_some() { tr!("settings-cat-edit") } else { tr!("settings-cat-new") };
        div()
            .my_3()
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .rounded(l.px(Metric::RadiusOverlay))
            .bg(l.fill_passive())
            .border_1()
            .border_color(l.border())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(Self::swatch(l, e.color, 14.))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                    .child(div().text_size(px(12.)).text_color(l.text2()).child(tr!("settings-cat-inplace"))),
            )
            .child(div().flex().gap_4().child(label(tr!("settings-cat-name"))).child(div().w(px(280.)).child(Input::new(&e.name))))
            .child(
                div().flex().gap_4().child(label(tr!("settings-cat-color"))).child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(swatches)
                        .child(div().text_size(px(12.)).text_color(l.text2()).child(tr!("settings-cat-colors-free", n = free, total = colors.len())))
                        // Новая категория, а свободных цветов нет (цветов в палитре пять, решение владельца 2026-10-22).
                        .when(free == 0 && e.keyword.is_none(), |d| {
                            d.child(div().text_color(l.text()).child(tr!("settings-cat-colors-none", total = colors.len())))
                        }),
                ),
            )
            .child(div().flex().gap_4().child(label(tr!("settings-cat-key"))).child(keys))
            .when_some(e.error.clone(), |d, err| d.child(div().text_color(l.error()).child(err)))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(button("settings-cat-save", tr!("settings-save"), true, l).on_click(cx.listener(|this, _, _, cx| this.save_category(cx))))
                    .child(button("settings-cat-cancel", tr!("settings-cancel"), false, l).on_click(cx.listener(|this, _, _, cx| {
                        this.cat_edit = None;
                        cx.notify();
                    }))),
            )
    }

    pub(crate) fn categories(&mut self, l: &Look, _window: &mut Window, cx: &mut Context<Self>) -> (Option<Stateful<Div>>, AnyElement, Vec<Div>) {
        let add = button("settings-cat-add", format!("+ {}", tr!("settings-cat-add")), false, l)
            .on_click(cx.listener(|this, _, window, cx| this.edit_category(None, window, cx)));
        let editing = self.cat_edit.as_ref().map(|e| e.keyword.clone());
        let head = div()
            .flex()
            .items_center()
            .gap_3()
            .py_2()
            .border_b_1()
            .border_color(l.border())
            .text_size(px(12.))
            .text_color(l.text2())
            .child(div().w(px(14.)))
            .child(div().flex_1().child(tr!("settings-cat-name")))
            .child(div().w(px(70.)).child(tr!("settings-cat-key")))
            .child(div().w(px(170.)));
        let mut table = div().flex().flex_col().child(head);
        let visible: Vec<Category> = self.cats.visible().cloned().collect();
        for (i, c) in visible.iter().enumerate() {
            let kw = c.keyword.clone();
            let edit_c = c.clone();
            table = table.child(
                div()
                    .h(px(40.))
                    .flex()
                    .items_center()
                    .gap_3()
                    .border_b_1()
                    .border_color(l.border())
                    .child(Self::swatch(l, c.shown_color(), 14.))
                    .child(div().flex_1().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD).child(c.name.clone()))
                    .child(
                        div()
                            .w(px(70.))
                            .flex()
                            .child(c.key.map_or_else(|| div().text_color(l.text3()).child("—"), |k| crate::settings_tab::keycap(k.to_string(), l))),
                    )
                    .child(
                        div()
                            .w(px(170.))
                            .flex()
                            .justify_end()
                            .gap_1()
                            .child(
                                link(("settings-cat-edit", i), tr!("settings-edit"), l)
                                    .on_click(cx.listener(move |this, _, window, cx| this.edit_category(Some(&edit_c), window, cx))),
                            )
                            .child(
                                link(("settings-cat-delete", i), tr!("settings-delete"), l)
                                    .on_click(cx.listener(move |this, _, _, cx| this.delete_category(kw.clone(), cx))),
                            ),
                    ),
            );
            if editing.as_ref().is_some_and(|k| k.as_deref() == Some(c.keyword.as_str()))
                && let Some(e) = &self.cat_edit
            {
                table = table.child(self.cat_editor(e, l, cx));
            }
        }
        if editing == Some(None)
            && let Some(e) = &self.cat_edit
        {
            table = table.child(self.cat_editor(e, l, cx));
        }
        let hidden: Vec<Category> = self.cats.list.iter().filter(|c| c.hidden).cloned().collect();
        let mut deleted = div().flex().flex_col();
        for (i, c) in hidden.iter().enumerate() {
            let confirm = self.cat_purge.as_deref() == Some(c.keyword.as_str());
            let (restore, purge, kw) = (c.clone(), c.keyword.clone(), c.keyword.clone());
            let row = div()
                .min_h(px(36.))
                .flex()
                .items_center()
                .gap_3()
                .border_b_1()
                .border_color(l.border())
                .child(Self::swatch(l, c.shown_color(), 12.).opacity(0.5))
                .child(div().flex_1().min_w_0().truncate().text_color(l.text2()).child(c.name.clone()))
                .when(!confirm, |d| {
                    d.child(
                        link(("settings-cat-restore", i), tr!("settings-cat-restore"), l)
                            .on_click(cx.listener(move |this, _, window, cx| this.restore_category(restore.clone(), window, cx))),
                    )
                    .child(link(("settings-cat-purge", i), tr!("settings-cat-purge"), l).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.cat_purge = Some(purge.clone());
                            cx.notify();
                        },
                    )))
                })
                .when(confirm, |d| {
                    d.child(div().text_color(l.warning()).child(tr!("settings-cat-purge-confirm", name = c.name.clone())))
                        .child(button(("settings-cat-purge-yes", i), tr!("settings-cat-purge-yes"), true, l).on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.cat_purge = None;
                                cx.emit(SettingsEvent::Purge(kw.clone()));
                                cx.notify();
                            },
                        )))
                        .child(link(("settings-cat-purge-no", i), tr!("settings-cancel"), l).on_click(cx.listener(|this, _, _, cx| {
                            this.cat_purge = None;
                            cx.notify();
                        })))
                });
            deleted = deleted.child(row);
        }
        let body = div()
            .flex()
            .flex_col()
            .pt_2()
            .child(table)
            .when(!hidden.is_empty(), |d| {
                d.child(crate::settings_tab::group(tr!("settings-cat-deleted"), l))
                    .child(div().text_size(px(12.)).text_color(l.text2()).child(tr!("settings-cat-deleted-hint")))
                    .child(deleted)
            })
            .child(div().pt_3().text_size(px(12.)).text_color(l.text2()).child(if self.info.live {
                tr!("settings-cat-server")
            } else {
                tr!("settings-cat-demo")
            }));
        let aside = vec![aside_card(
            tr!("settings-cat-rules"),
            vec![tr!("settings-cat-rules-1"), tr!("settings-cat-rules-2"), tr!("settings-cat-rules-3")],
            l,
        )];
        (Some(add), body.into_any_element(), aside)
    }
}

#[cfg(test)]
mod tests {
    use super::apply;
    use postman_core::labels::{Categories, Problem};
    use postman_core::palette;

    fn table() -> Categories {
        Categories::defaults(|id| id.to_owned())
    }

    /// Правка на месте проверяется таблицей: близкий цвет (ΔE < 25) и занятая клавиша — отказ, остальное — новая таблица.
    #[test]
    fn category_edit_rules() {
        let full = table();
        // Цветов в таблице пять — все заняты категориями по умолчанию; серый освобождается удалением «services».
        assert!(full.colors(None).iter().all(|(_, c)| c.is_some()));
        let mut t = full.clone();
        t.delete(&full.by_key(5).unwrap().keyword, 1).unwrap();
        // Новая категория: свободный цвет и клавиша.
        let free = t.colors(None).into_iter().find(|(_, c)| c.is_none()).map(|(k, _)| k).unwrap();
        assert_eq!(free, palette::GREY);
        let added = apply(&t, None, "Partners", free, Some(6), 1).unwrap();
        assert_eq!(added.visible().count(), 5);
        assert_eq!(added.by_key(6).map(|c| c.name.as_str()), Some("Partners"));
        // Тот же цвет, что у «clients», — отказ ΔE.
        assert!(matches!(apply(&t, None, "Partners", palette::BLUE, None, 1), Err(Problem::Colour { .. })));
        // Цвета вне палитры — отказ.
        assert!(matches!(apply(&t, None, "Partners", "chartreuse", None, 1), Err(Problem::Colour { .. })));
        // Клавиша занята «team».
        assert_eq!(apply(&t, None, "Partners", free, Some(2), 1), Err(Problem::DuplicateKey(2)));
        // Повтор имени и пустое имя.
        assert_eq!(apply(&t, None, "team", free, None, 1), Err(Problem::DuplicateName("team".into())));
        assert_eq!(apply(&t, None, "  ", free, None, 1), Err(Problem::BadName));
        // Правка своей: своя клавиша и свой цвет — не конфликт; чужая клавиша — отказ.
        let kw = t.by_key(1).unwrap().keyword.clone();
        let renamed = apply(&t, Some(&kw), "Customers", palette::BLUE, Some(1), 2).unwrap();
        assert_eq!(renamed.by_key(1).map(|c| c.name.as_str()), Some("Customers"));
        assert_eq!(apply(&t, Some(&kw), "Customers", palette::BLUE, Some(3), 2), Err(Problem::DuplicateKey(3)));
        // Удалённая освобождает клавишу.
        let mut d = t.clone();
        d.delete(&t.by_key(3).unwrap().keyword, 3).unwrap();
        assert!(apply(&d, Some(&kw), "Customers", palette::BLUE, Some(3), 4).is_ok());
    }
}
