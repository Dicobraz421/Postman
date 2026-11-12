//! «Настройки → Категории» (S5-09, DESIGN «Цвет»): таблица категорий своей учётной записи, правка на месте — имя, цвет
//! из общих цветов Fluent 2 (близкие к цветам других категорий и к занятым красному и янтарному — перечёркнуты, с
//! причиной), клавиша 1–9. Удаление скрывает категорию; снять её с писем — отдельным явным выбором. Проверки — в
//! `postman_core::labels::Categories` (`add`, `rename`, `recolor`, `set_key`, `delete`); запись на сервер — экран
//! почты (`SettingsEvent::Categories`, FileNode).
//!
//! Авто-назначение (T2-53, макет `Settings`): правило сервера «если домен отправителя один из … или тема содержит …
//! → категория» — то же правило, что в «Правилах» (одно на категорию: его единственное действие — эта категория).
//! Правило сложнее двух полей правится в «Правилах». Столбец «Писем» — по кэшу своей учётной записи (в фоне).

use gpui_kit::component::input::{Input, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::Metric;
use postman_core::labels::{Categories, Category, Problem};
use postman_core::palette::{self, Clash, Reserved};
use postman_core::rules::{Action, Cond, Field, Op, Rule};

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
    /// Авто-назначение: домены отправителя и слова темы через запятую.
    pub(crate) domains: Entity<InputState>,
    pub(crate) words: Entity<InputState>,
    /// Правило категории сложнее двух полей — правится в «Правилах», здесь не трогается.
    pub(crate) complex: bool,
    /// «Применить к уже полученным» и сколько писем подходит (`None` — ещё считаем).
    pub(crate) apply: bool,
    pub(crate) found: Option<usize>,
}

/// Правило авто-назначения категории: первое, чьё единственное действие — эта категория.
pub(crate) fn auto_rule<'a>(rules: &'a [Rule], keyword: &str) -> Option<(usize, &'a Rule)> {
    rules.iter().enumerate().find(|(_, r)| matches!(r.actions.as_slice(), [Action::Category(k)] if k.eq_ignore_ascii_case(keyword)))
}

/// Поля «домены» и «слова темы» правила; `None` — правило сложнее (другие условия, «и»).
pub(crate) fn auto_fields(r: &Rule) -> Option<(Vec<String>, Vec<String>)> {
    let [g] = r.when.as_slice() else { return None };
    let (mut domains, mut words) = (Vec::new(), Vec::new());
    for c in g {
        match (c.field, c.op) {
            (Field::From, Op::Domain) => domains.push(c.value.clone()),
            (Field::Subject, Op::Contains) => words.push(c.value.clone()),
            _ => return None,
        }
    }
    Some((domains, words))
}

/// Список через запятую (домены — и через пробел, без «@»), без пустых и повторов.
fn split(text: &str, domains: bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in text.split(|c: char| c == ',' || c == ';' || (domains && c.is_whitespace())) {
        let v = part.trim();
        let v = if domains { v.trim_start_matches('@').to_lowercase() } else { v.to_owned() };
        if !v.is_empty() && !out.contains(&v) {
            out.push(v);
        }
    }
    out
}

/// Правило авто-назначения из полей: «домен один из … или тема содержит …» → категория; поля пусты — `None`.
pub(crate) fn auto_build(keyword: &str, name: String, domains: &str, words: &str) -> Option<Rule> {
    let mut when: Vec<Cond> = split(domains, true).into_iter().map(|d| Cond { field: Field::From, op: Op::Domain, value: d }).collect();
    when.extend(split(words, false).into_iter().map(|w| Cond::contains(Field::Subject, w)));
    (!when.is_empty()).then(|| Rule {
        enabled: true,
        name,
        when: vec![when],
        actions: vec![Action::Category(keyword.to_owned())],
        stop: false,
        local: false,
    })
}

/// Список правил с новым правилом авто-назначения категории (вместо прежнего; `None` — убрать прежнее). Включено ли
/// и где выполняется — как было.
pub(crate) fn with_auto(rules: &[Rule], keyword: &str, rule: Option<Rule>) -> Vec<Rule> {
    let mut out = rules.to_vec();
    match (auto_rule(rules, keyword).map(|(i, _)| i), rule) {
        (Some(i), Some(r)) => out[i] = Rule { enabled: out[i].enabled, local: out[i].local, ..r },
        (Some(i), None) => {
            out.remove(i);
        }
        (None, Some(r)) => out.push(r),
        (None, None) => {}
    }
    out
}

/// Условия правила строкой столбца «Авто-назначение»: «@a.ru, @b.ru, «тендер»».
pub(crate) fn auto_text(r: &Rule) -> String {
    r.when
        .iter()
        .map(|g| {
            g.iter()
                .map(|c| match (c.field, c.op) {
                    (Field::From, Op::Domain) => format!("@{}", c.value),
                    (Field::Subject, Op::Contains) => format!("«{}»", c.value),
                    _ => format!("{} {} «{}»", crate::rules::field_name(c.field).to_lowercase(), crate::rules::op_name(c.op), c.value),
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .collect::<Vec<_>>()
        .join(" + ")
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
        let rule = keyword.as_deref().and_then(|k| self.info.rules.as_deref().and_then(|rs| auto_rule(rs, k)).map(|(_, r)| r.clone()));
        let fields = rule.as_ref().map(auto_fields);
        let complex = matches!(fields, Some(None));
        let (d, w) = fields.flatten().map(|(d, w)| (d.join(", "), w.join(", "))).unwrap_or_default();
        let domains = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("settings-cat-auto-domains-placeholder")).default_value(d));
        let words = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("settings-cat-auto-words-placeholder")).default_value(w));
        self.cat_edit = Some(CatEdit { keyword, name: state, color, key, error: None, domains, words, complex, apply: false, found: None });
        self.cat_purge = None;
        cx.notify();
    }

    fn save_category(&mut self, cx: &mut Context<Self>) {
        let Some(e) = &self.cat_edit else { return };
        let name = e.name.read(cx).value().to_string();
        match apply(&self.cats, e.keyword.as_deref(), &name, e.color, e.key, now_ms()) {
            Ok(t) => {
                // Ключевое слово новой категории — из новой таблицы (по имени).
                let keyword = e.keyword.clone().or_else(|| t.visible().find(|c| c.name == name.trim()).map(|c| c.keyword.clone()));
                let auto = keyword.zip(self.info.rules.clone()).filter(|_| !e.complex).map(|(k, rules)| {
                    let rule = auto_build(
                        &k,
                        tr!("settings-cat-auto-name", name = name.trim().to_owned()),
                        &e.domains.read(cx).value(),
                        &e.words.read(cx).value(),
                    );
                    (with_auto(&rules, &k, rule.clone()), rules, rule)
                });
                let apply_now = e.apply;
                self.cats = t.clone();
                self.cat_edit = None;
                cx.emit(SettingsEvent::Categories(t));
                if let Some((new, old, rule)) = auto {
                    if new != old {
                        self.info.rules = Some(new.clone());
                        cx.emit(SettingsEvent::SaveRules(new));
                    }
                    if let Some(rule) = rule.filter(|_| apply_now) {
                        cx.emit(SettingsEvent::ApplyRule(rule));
                    }
                }
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
            tracing::warn!("categories: категория не удалена: {p}");
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
                    tracing::error!("categories: категория не восстановлена: {e}");
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

    /// Сколько писем у каждой категории — по кэшу своей учётной записи (в фоне).
    pub(crate) fn count_categories(&mut self, cx: &mut Context<Self>) {
        let Some(db) = self.info.db.clone() else { return };
        let task = cx.background_spawn(async move {
            let mut out: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
            for m in db.mailboxes()?.into_iter().filter(|m| postman_cache::account_of(&m.id).is_none()) {
                for (k, n) in db.keyword_counts(&m.id)? {
                    *out.entry(k).or_default() += n;
                }
            }
            Ok::<_, postman_cache::Error>(out)
        });
        cx.spawn(async move |this, cx| {
            let counts = task.await;
            let _ = this.update(cx, |v, cx| {
                match counts {
                    Ok(c) => v.cat_counts = Some(c),
                    Err(e) => tracing::error!("categories: число писем с категориями не подсчитано: {e}"),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// «Применить к уже полученным — подходит N писем»: по кэшу (в фоне).
    pub(crate) fn count_auto(&mut self, cx: &mut Context<Self>) {
        let (Some(e), Some(db)) = (&self.cat_edit, self.info.db.clone()) else { return };
        let keyword = e.keyword.clone().unwrap_or_default();
        let Some(rule) = auto_build(&keyword, String::new(), &e.domains.read(cx).value(), &e.words.read(cx).value()) else {
            if let Some(e) = &mut self.cat_edit {
                e.found = Some(0);
            }
            return cx.notify();
        };
        let task = cx.background_spawn(async move { postman_core::rulerun::matching(&db, &rule, postman_core::rulerun::EXISTING) });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            let _ = this.update(cx, |v, cx| {
                if let (Some(e), Ok((rows, _))) = (&mut v.cat_edit, found) {
                    e.found = Some(rows.len());
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// «Назначать, если»: домены отправителя или слова темы — правило сервера; без правил у сервера — нет строки.
    fn auto_editor(&self, e: &CatEdit, l: &Look, cx: &mut Context<Self>) -> Option<Div> {
        self.info.rules.as_ref()?;
        let label = |t: String| div().w(px(110.)).flex_none().pt_1().text_color(l.text2()).child(t);
        if e.complex {
            return Some(
                div()
                    .flex()
                    .gap_4()
                    .child(label(tr!("settings-cat-auto")))
                    .child(div().flex_1().text_color(l.text2()).child(tr!("settings-cat-auto-complex"))),
            );
        }
        let found = match (e.apply, e.found) {
            (true, Some(n)) => tr!("settings-cat-apply-found", n = n as i64),
            (true, None) => tr!("settings-cat-apply-counting"),
            _ => tr!("settings-cat-apply"),
        };
        let field = |title: String, input: &Entity<InputState>, id: &'static str| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().w(px(170.)).flex_none().text_size(px(12.)).text_color(l.text2()).child(title))
                .child(div().flex_1().relative().children(crate::screens::spot(id)).child(Input::new(input)))
        };
        Some(
            div().flex().gap_4().child(label(tr!("settings-cat-auto"))).child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(field(tr!("settings-cat-auto-domains"), &e.domains, "settings-cat-domains"))
                    .child(div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(l.text2()).child(tr!("rules-tok-or")))
                    .child(field(tr!("settings-cat-auto-words"), &e.words, "settings-cat-words"))
                    .child(
                        div()
                            .id("settings-cat-apply")
                            .children(crate::screens::spot("settings-cat-apply"))
                            .flex()
                            .items_center()
                            .gap_2()
                            .cursor_pointer()
                            .child(crate::vacation::checkbox(e.apply))
                            .child(found)
                            .on_click(cx.listener(|this, _, _, cx| {
                                let on = this.cat_edit.as_mut().map(|e| {
                                    e.apply = !e.apply;
                                    e.found = None;
                                    e.apply
                                });
                                if on == Some(true) {
                                    this.count_auto(cx);
                                }
                                cx.notify();
                            })),
                    ),
            ),
        )
    }

    /// «Предпросмотр в списке»: строка письма с правимой категорией и соседи в списке «Категория».
    fn cat_preview(&self, l: &Look, cx: &App) -> Div {
        let (name, color) = match &self.cat_edit {
            Some(e) => {
                let typed = e.name.read(cx).value().trim().to_owned();
                (if typed.is_empty() { tr!("settings-cat-preview-name") } else { typed }, e.color)
            }
            None => self.cats.visible().next().map_or((tr!("settings-cat-preview-name"), palette::BLUE), |c| (c.name.clone(), c.shown_color())),
        };
        let row = div()
            .flex()
            .gap_3()
            .p_3()
            .rounded(l.px(Metric::RadiusControl))
            .border_1()
            .border_color(l.border())
            .bg(l.panel())
            .child(div().w(px(3.)).flex_none().rounded_sm().bg(l.cat(color).dot))
            .child(
                div()
                    .size(px(32.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(l.picked())
                    .text_color(l.accent_text())
                    .text_size(px(12.))
                    .child(tr!("settings-cat-preview-initials")),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().flex_1().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD).child(tr!("settings-cat-preview-from")))
                            .child(crate::labels::chip(name.clone(), color, l))
                            .child(div().text_size(px(12.)).text_color(l.text2()).child("12:10")),
                    )
                    .child(div().truncate().text_color(l.accent_text()).child(tr!("settings-cat-preview-subject")))
                    .child(div().truncate().text_size(px(12.)).text_color(l.text2()).child(tr!("settings-cat-preview-text"))),
            );
        let editing = self.cat_edit.as_ref().and_then(|e| e.keyword.clone());
        let mut near = div().flex().flex_wrap().gap_1();
        for c in self.cats.visible().filter(|c| Some(&c.keyword) != editing.as_ref()) {
            near = near.child(crate::labels::chip(c.name.clone(), c.shown_color(), l));
        }
        if self.cat_edit.is_some() {
            near = near.child(crate::labels::chip(name, color, l));
        }
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .rounded(l.px(Metric::RadiusOverlay))
            .border_1()
            .border_color(l.c(pets_ui::theme::Role::StrokeCard))
            .bg(l.panel())
            .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(tr!("settings-preview-title")))
            .child(row)
            .child(div().text_size(px(12.)).text_color(l.text2()).child(tr!("settings-cat-preview-near")))
            .child(near)
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
                .children(crate::screens::spot(("settings-cat-color", i)))
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
                .relative()
                .children(crate::screens::spot(("settings-cat-key", k as usize)))
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
                        .min_w_0()
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
            .children(self.auto_editor(e, l, cx))
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
            .child(div().w(px(160.)).child(tr!("settings-cat-name")))
            .child(div().flex_1().child(tr!("settings-cat-auto")))
            .child(div().w(px(70.)).child(tr!("settings-cat-key")))
            .child(div().w(px(70.)).flex().justify_end().child(tr!("settings-cat-count")))
            .child(div().w(px(150.)));
        let mut table = div().flex().flex_col().child(head);
        let visible: Vec<Category> = self.cats.visible().cloned().collect();
        for (i, c) in visible.iter().enumerate() {
            let kw = c.keyword.clone();
            let edit_c = c.clone();
            let auto = self.info.rules.as_deref().and_then(|rs| auto_rule(rs, &c.keyword)).map(|(_, r)| auto_text(r));
            let count = self.cat_counts.as_ref().map(|m| m.get(&c.keyword.to_lowercase()).copied().unwrap_or(0));
            table = table.child(
                div()
                    .h(px(40.))
                    .flex()
                    .items_center()
                    .gap_3()
                    .border_b_1()
                    .border_color(l.border())
                    .child(Self::swatch(l, c.shown_color(), 14.))
                    .child(div().w(px(146.)).flex_none().truncate().font_weight(FontWeight::SEMIBOLD).child(c.name.clone()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.))
                            .text_color(l.text2())
                            .child(auto.unwrap_or_else(|| tr!("settings-cat-auto-manual"))),
                    )
                    .child(
                        div()
                            .w(px(70.))
                            .flex()
                            .child(c.key.map_or_else(|| div().text_color(l.text3()).child("—"), |k| crate::settings_tab::keycap(k.to_string(), l))),
                    )
                    .child(div().w(px(70.)).flex().justify_end().text_color(l.text2()).child(count.map_or_else(|| "…".to_owned(), |n| n.to_string())))
                    .child(
                        div()
                            .w(px(150.))
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
        let aside = vec![
            self.cat_preview(l, cx),
            aside_card(tr!("settings-cat-rules"), vec![tr!("settings-cat-rules-1"), tr!("settings-cat-rules-2"), tr!("settings-cat-rules-3")], l),
        ];
        (Some(add), body.into_any_element(), aside)
    }
}

#[cfg(test)]
mod tests {
    use super::{apply, auto_build, auto_fields, auto_rule, auto_text, with_auto};
    use postman_core::rules::{Action, Cond, Field, Op, Rule};

    /// Авто-назначение (T2-53): поля → правило «домен один из или тема содержит» → категория; замена, снятие.
    #[test]
    fn auto_assign_rule() {
        let r =
            auto_build("pm.cat.partners", "x".into(), " @Softline-Partner.ru vendor-sec.ru, softline-partner.ru", "партнёр, дистрибьютор ,").unwrap();
        assert_eq!(
            auto_fields(&r),
            Some((vec!["softline-partner.ru".into(), "vendor-sec.ru".into()], vec!["партнёр".into(), "дистрибьютор".into()]))
        );
        assert_eq!(auto_text(&r), "@softline-partner.ru, @vendor-sec.ru, «партнёр», «дистрибьютор»");
        assert_eq!(auto_build("k", "x".into(), " ", ""), None);
        let other = Rule::simple(true, Field::From, "boss", Action::Flag);
        let list = vec![other.clone()];
        let added = with_auto(&list, "pm.cat.partners", Some(r.clone()));
        assert_eq!(auto_rule(&added, "PM.CAT.PARTNERS").map(|(i, _)| i), Some(1));
        // Замена сохраняет «выключено»; пустые поля снимают правило.
        let mut off = added.clone();
        off[1].enabled = false;
        let replaced = with_auto(&off, "pm.cat.partners", auto_build("pm.cat.partners", "x".into(), "a.ru", ""));
        assert!(!replaced[1].enabled && replaced[1].when[0].len() == 1);
        assert_eq!(with_auto(&added, "pm.cat.partners", None), list);
        // Правило сложнее двух полей — не наше поле ввода.
        let complex = Rule { when: vec![vec![Cond { field: Field::To, op: Op::Contains, value: "x".into() }]], ..r };
        assert_eq!(auto_fields(&complex), None);
    }
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
