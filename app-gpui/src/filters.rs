//! Фильтры в интерфейсе (S5-03, `docs/DESIGN.md` «Фильтры (боковая панель)», «Быстрые фильтры»): общая модель
//! панели папок и списка ([`FilterModel`]), раскладка списка с группами ([`Layout`]), панель фильтров, воронка,
//! закладки быстрых фильтров, заголовок группы и меню сортировки. Условия, счётчики, группы и сортировка —
//! `postman_core::filters`; пересчёт по кэшу — в списке (`list.rs`, в фоне).
//!
//! Модель — сущность без вида: панель папок и список её наблюдают (`observe` → перерисовка). Смена условий —
//! событие [`Changed`]: по нему список пересчитывает письма и счётчики, а счётчики записывает обратно в модель.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::{Metric, Role};
use postman_core::filters::{CHECKS, Check, Counts, Criteria, Day, Group, Quick, Sort};
use postman_core::palette;

use crate::chrome::{glyph, icon};
use crate::roles::{Look, Pm};

/// Где искать: открытая папка или все папки аккаунта.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Where {
    #[default]
    Folder,
    All,
}

/// Что показывает список: фильтры, где искать, порядок.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Query {
    pub(crate) criteria: Criteria,
    pub(crate) scope: Where,
    pub(crate) sort: Sort,
}

impl Query {
    /// Список — свой набор писем (фильтры, «Все папки», порядок не по дате), а не страницы папки из кэша.
    pub(crate) fn own_list(&self) -> bool {
        self.scope == Where::All || !self.criteria.is_empty() || self.sort != Sort::Date
    }
}

/// Общая модель фильтров: условия, режим панели слева, закладки, счётчики последнего пересчёта.
pub(crate) struct FilterModel {
    pub(crate) query: Query,
    /// Панель слева — фильтры (воронка), иначе папки.
    pub(crate) panel: bool,
    /// Панель быстрых фильтров открыта (наведение на закладки).
    pub(crate) quick_open: bool,
    /// Счётчики панели и закладок (пишет список после пересчёта).
    pub(crate) counts: Counts,
    /// Категории таблицы своего аккаунта: ключевое слово, имя, цвет (имя цвета Fluent).
    pub(crate) categories: Vec<(String, String, String)>,
    /// Имя открытой папки — «Где искать», заголовок списка.
    pub(crate) folder: String,
}

/// Условия изменились: список пересчитывает письма.
pub(crate) struct Changed;

impl EventEmitter<Changed> for FilterModel {}

impl FilterModel {
    pub(crate) fn new() -> Self {
        Self { query: Query::default(), panel: false, quick_open: false, counts: Counts::default(), categories: Vec::new(), folder: String::new() }
    }

    /// Изменить условия: изменились — `Changed` (пересчёт); перерисовка — всегда.
    pub(crate) fn change(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Query)) {
        let before = self.query.clone();
        f(&mut self.query);
        if self.query != before {
            cx.emit(Changed);
        }
        cx.notify();
    }

    /// Изменить вид (панель, закладки, счётчики) без пересчёта.
    pub(crate) fn set(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Self)) {
        f(self);
        cx.notify();
    }

    /// Фильтры панели одной строкой для чипа над списком («непрочитанные, вложения»); нет — `None`.
    pub(crate) fn chip_text(&self) -> Option<String> {
        let c = &self.query.criteria;
        let mut parts: Vec<String> = CHECKS.iter().filter(|k| c.checks.contains(k)).map(|k| tr!(chip_key(*k))).collect();
        for kw in &c.categories {
            let name = self.categories.iter().find(|(k, ..)| k == kw).map_or(kw.as_str(), |(_, n, _)| n.as_str());
            parts.push(name.to_lowercase());
        }
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

fn check_key(c: Check) -> &'static str {
    match c {
        Check::Unread => "filters-unread",
        Check::Mentions => "filters-mentions",
        Check::Attachments => "filters-attachments",
        Check::Flagged => "filters-flagged",
        Check::External => "filters-external",
    }
}

fn chip_key(c: Check) -> &'static str {
    match c {
        Check::Unread => "filters-chip-unread",
        Check::Mentions => "filters-chip-mentions",
        Check::Attachments => "filters-chip-attachments",
        Check::Flagged => "filters-chip-flagged",
        Check::External => "filters-chip-external",
    }
}

/// Название закладки: короткое (на закладке) и полное (в панели).
pub(crate) fn quick_keys(q: Quick) -> (&'static str, &'static str) {
    match q {
        Quick::Threats => ("filters-quick-threats", "filters-quick-threats-long"),
        Quick::Due => ("filters-quick-due", "filters-quick-due-long"),
        Quick::Waiting => ("filters-quick-waiting", "filters-quick-waiting-long"),
        Quick::ToMe => ("filters-quick-to-me", "filters-quick-to-me-long"),
        Quick::Expiring => ("filters-quick-expiring", "filters-quick-expiring-long"),
    }
}

/// Цвет закладки: красный — только угрозы, янтарный — только сроки (DESIGN «Цвет»); остальные — из палитры.
pub(crate) fn quick_color(q: Quick, l: &Look) -> Hsla {
    match q {
        Quick::Threats => l.pm(Pm::ThreatDot),
        Quick::Due => l.pm(Pm::DeadlineFg),
        Quick::Waiting => l.cat(palette::BLUE).dot,
        Quick::ToMe => l.cat(palette::GREEN).dot,
        Quick::Expiring => l.cat(palette::NEUTRAL).dot,
    }
}

fn day_key(d: Day) -> &'static str {
    match d {
        Day::Today => "filters-day-today",
        Day::Yesterday => "filters-day-yesterday",
        Day::ThisWeek => "filters-day-week",
        Day::LastWeek => "filters-day-last-week",
        Day::Earlier => "filters-day-earlier",
    }
}

/// Подпись группы: дата или имя папки.
pub(crate) fn group_label(g: &Group, names: &HashMap<String, String>) -> String {
    match g {
        Group::Top => tr!("lifecycle-group-top"),
        Group::Day(d) => tr!(day_key(*d)),
        Group::Folder(id) => names.get(id).cloned().unwrap_or_else(|| id.clone()),
    }
}

pub(crate) fn sort_key(s: Sort) -> &'static str {
    match s {
        Sort::Date => "filters-sort-date",
        Sort::Sender => "filters-sort-sender",
        Sort::Size => "filters-sort-size",
    }
}

// --- раскладка списка ---

/// Элемент списка: заголовок группы (номер группы) или письмо (номер строки по порядку списка).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
    Header(usize),
    Row(usize),
}

/// Группа в раскладке: строки `rows` (по порядку списка), свёрнута ли, где начинается (номер элемента).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Seg {
    pub(crate) group: Option<Group>,
    pub(crate) rows: Range<usize>,
    pub(crate) collapsed: bool,
    start: usize,
}

impl Seg {
    fn header(&self) -> usize {
        usize::from(self.group.is_some())
    }

    fn shown(&self) -> usize {
        if self.collapsed { 0 } else { self.rows.len() }
    }
}

/// Строки списка с заголовками групп: номер элемента ↔ номер письма; свёрнутые группы — только заголовок.
/// Без групп — письма подряд, номер элемента и есть номер письма.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Layout {
    segs: Vec<Seg>,
    len: usize,
}

impl Layout {
    /// Группы подряд с числом писем; сумма не сошлась с `count` (письма пришли между чтениями) — последняя
    /// группа берёт остаток.
    pub(crate) fn new(groups: &[(Group, usize)], count: usize, collapsed: &HashSet<Group>) -> Layout {
        if groups.is_empty() {
            return Layout { segs: vec![Seg { group: None, rows: 0..count, collapsed: false, start: 0 }], len: count };
        }
        let (mut segs, mut row, mut at) = (Vec::new(), 0, 0);
        for (i, (g, n)) in groups.iter().enumerate() {
            let left = count - row;
            let n = if i + 1 == groups.len() { left } else { (*n).min(left) };
            if n == 0 {
                continue;
            }
            let seg = Seg { group: Some(g.clone()), rows: row..row + n, collapsed: collapsed.contains(g), start: at };
            at += seg.header() + seg.shown();
            row += n;
            segs.push(seg);
        }
        Layout { segs, len: at }
    }

    /// Элементов (заголовки и видимые письма).
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn segs(&self) -> &[Seg] {
        &self.segs
    }

    #[cfg(test)]
    pub(crate) fn grouped(&self) -> bool {
        self.segs.iter().any(|s| s.group.is_some())
    }

    fn seg_at(&self, item: usize) -> Option<usize> {
        let i = self.segs.partition_point(|s| s.start <= item).checked_sub(1)?;
        let s = &self.segs[i];
        (item < s.start + s.header() + s.shown()).then_some(i)
    }

    pub(crate) fn slot(&self, item: usize) -> Option<Slot> {
        let i = self.seg_at(item)?;
        let s = &self.segs[i];
        if s.group.is_some() && item == s.start {
            return Some(Slot::Header(i));
        }
        Some(Slot::Row(s.rows.start + item - s.start - s.header()))
    }

    fn seg_of_row(&self, row: usize) -> Option<usize> {
        let i = self.segs.partition_point(|s| s.rows.start <= row).checked_sub(1)?;
        self.segs[i].rows.contains(&row).then_some(i)
    }

    /// Номер элемента письма; письмо в свёрнутой группе — `None`.
    pub(crate) fn item_of(&self, row: usize) -> Option<usize> {
        let s = &self.segs[self.seg_of_row(row)?];
        (!s.collapsed).then(|| s.start + s.header() + row - s.rows.start)
    }

    /// Группа письма.
    pub(crate) fn group_of(&self, row: usize) -> Option<&Group> {
        self.seg_of_row(row).and_then(|i| self.segs[i].group.as_ref())
    }

    /// Письма, видимые в элементах `items` (окно чтения из кэша).
    pub(crate) fn rows_in(&self, items: Range<usize>) -> Range<usize> {
        let rows: Vec<usize> = items
            .filter_map(|i| match self.slot(i) {
                Some(Slot::Row(r)) => Some(r),
                _ => None,
            })
            .collect();
        match (rows.first(), rows.last()) {
            (Some(&a), Some(&b)) => a..b + 1,
            _ => {
                // Видны одни заголовки (группы свёрнуты): окно — у первой группы в элементах.
                let at = self.seg_at(0).map_or(0, |i| self.segs[i].rows.start);
                at..at
            }
        }
    }

    /// Ближайшее видимое письмо к `row` (в свёрнутой группе — первое следующей развёрнутой, назад — последнее
    /// предыдущей); видимых нет — `None`.
    pub(crate) fn visible(&self, row: usize, forward: bool) -> Option<usize> {
        if self.item_of(row).is_some() {
            return Some(row);
        }
        let open = |s: &&Seg| !s.collapsed && !s.rows.is_empty();
        if forward {
            self.segs.iter().filter(open).find(|s| s.rows.start > row).map(|s| s.rows.start)
        } else {
            self.segs.iter().rev().filter(open).find(|s| s.rows.end <= row).map(|s| s.rows.end - 1)
        }
    }

    /// Высоты элементов для списка; `tail` — пустое место в конце (под закладками быстрых фильтров).
    pub(crate) fn sizes(&self, row_h: f32, header_h: f32, tail: f32) -> Vec<Size<Pixels>> {
        let mut out = Vec::with_capacity(self.len + 1);
        for s in &self.segs {
            if s.group.is_some() {
                out.push(size(px(0.), px(header_h)));
            }
            out.extend(std::iter::repeat_n(size(px(0.), px(row_h)), s.shown()));
        }
        if tail > 0. {
            out.push(size(px(0.), px(tail)));
        }
        out
    }
}

// --- вид ---

/// Высота заголовка группы (макет).
pub(crate) const HEADER_H: f32 = 30.0;
/// Высота полосы закладок быстрых фильтров; столько же пустого места в конце списка.
pub(crate) const QUICK_H: f32 = 24.0;

/// Флажок панели фильтров.
fn checkbox(on: bool, l: &Look) -> Div {
    div()
        .size(px(16.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.))
        .when(on, |d| d.bg(l.accent()).text_color(l.on_accent()).child(icon(glyph::CHECK, 10.)))
        .when(!on, |d| d.border_1().border_color(l.c(Role::StrokeStrong)))
}

/// Строка панели: флажок, точка цвета (категории), название, число; ноль — бледно.
fn item(id: SharedString, name: String, n: usize, on: bool, dot: Option<Hsla>, l: &Look) -> Stateful<Div> {
    div()
        .id(id)
        .w_full()
        .h(px(28.))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(8.))
        .px(px(8.))
        .rounded(l.px(Metric::RadiusControl))
        .cursor_pointer()
        .when(on, |d| d.bg(l.fill()))
        .when(!on, |d| d.hover(|d| d.bg(l.hover())))
        .child(checkbox(on, l))
        .when_some(dot, |d, c| d.child(div().size(px(10.)).flex_none().rounded(px(3.)).bg(c)))
        .child(div().flex_1().min_w_0().truncate().when(n == 0 && !on, |d| d.text_color(l.text3())).child(name))
        .child(div().flex_none().text_color(l.text2()).child(if n > 0 { n.to_string() } else { String::new() }))
}

fn caption(text: String, l: &Look) -> Div {
    div().px(px(8.)).pt(px(10.)).pb(px(2.)).text_size(l.px(Metric::TypeCaption)).text_color(l.text2()).child(text)
}

/// Панель фильтров на месте дерева папок: «Где искать», Состояние, Отправитель, Категория.
pub(crate) fn panel(model: &Entity<FilterModel>, m: &FilterModel, l: &Look) -> Stateful<Div> {
    let c = &m.query.criteria;
    let toggle = |f: fn(&mut Query)| {
        let model = model.clone();
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| model.update(cx, |m, cx| m.change(cx, f))
    };
    let check = |k: Check| {
        let model = model.clone();
        item(SharedString::from(format!("flt-{k:?}")), tr!(check_key(k)), m.counts.check(k), c.checks.contains(&k), None, l)
            .on_click(move |_, _, cx| model.update(cx, |m, cx| m.change(cx, |q| q.criteria.toggle_check(k))))
    };
    let back = {
        let model = model.clone();
        div()
            .id("flt-back")
            .size(px(28.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(l.px(Metric::RadiusControl))
            .cursor_pointer()
            .hover(|d| d.bg(l.hover()))
            .child(icon(glyph::BACK, 14.))
            .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tr!("filters-back")).build(window, cx))
            .on_click(move |_, _, cx| model.update(cx, |m, cx| m.set(cx, |m| m.panel = false)))
    };
    let reset = (c.panel_len() > 0).then(|| {
        div()
            .id("flt-reset")
            .pr(px(6.))
            .text_size(l.px(Metric::TypeCaption))
            .text_color(l.accent_text())
            .cursor_pointer()
            .child(tr!("filters-reset"))
            .on_click(toggle(|q| q.criteria.clear_panel()))
    });
    let seg = |id: &'static str, text: String, scope: Where| {
        let on = m.query.scope == scope;
        let model = model.clone();
        div()
            .id(id)
            .flex_1()
            .min_w_0()
            .h(px(26.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(l.m(Metric::RadiusControl)))
            .cursor_pointer()
            .when(on, |d| d.bg(l.panel()).shadow_sm().font_weight(FontWeight::SEMIBOLD))
            .when(!on, |d| d.text_color(l.text2()).hover(|d| d.bg(l.hover())))
            .child(div().truncate().child(text))
            .on_click(move |_, _, cx| model.update(cx, |m, cx| m.change(cx, |q| q.scope = scope)))
    };
    let scopes = div()
        .flex()
        .p(px(2.))
        .gap(px(2.))
        .rounded(px(6.))
        .bg(l.fill())
        .text_size(l.px(Metric::TypeCaption))
        .child(seg("flt-where-folder", m.folder.clone(), Where::Folder))
        .child(seg("flt-where-all", tr!("filters-all-folders"), Where::All));
    let categories = m.categories.iter().map(|(kw, name, color)| {
        let model = model.clone();
        let keyword = kw.clone();
        item(SharedString::from(format!("flt-cat-{kw}")), name.clone(), m.counts.category(kw), c.categories.contains(kw), Some(l.cat(color).dot), l)
            .on_click(move |_, _, cx| {
                let keyword = keyword.clone();
                model.update(cx, |m, cx| m.change(cx, move |q| q.criteria.toggle_category(&keyword)))
            })
    });
    div()
        .id("filters-panel")
        .size_full()
        .flex()
        .flex_col()
        .overflow_y_scroll()
        .pt(px(4.))
        .px(px(10.))
        .bg(l.panel())
        .rounded_t(l.px(Metric::RadiusOverlay))
        .border_1()
        .border_b_0()
        .border_color(l.c(Role::StrokeCard))
        .child(
            div()
                .h(px(36.))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(back)
                .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(tr!("filters-title")))
                .children(reset),
        )
        .child(caption(tr!("filters-where"), l).pt(px(6.)).pb(px(4.)))
        .child(scopes)
        .child(caption(tr!("filters-group-state"), l))
        .children([Check::Unread, Check::Mentions, Check::Attachments, Check::Flagged].map(check))
        .child(caption(tr!("filters-group-sender"), l))
        .child(check(Check::External))
        .when(!m.categories.is_empty(), |d| d.child(caption(tr!("filters-group-category"), l)).children(categories))
        .child(div().h(px(12.)).flex_none())
}

/// Воронка у открытой папки: панель Папки ↔ Фильтры; число — фильтров панели (закладки не считаются).
pub(crate) fn funnel(model: &Entity<FilterModel>, m: &FilterModel, l: &Look) -> Stateful<Div> {
    let n = m.query.criteria.panel_len();
    let model = model.clone();
    div()
        .id("funnel")
        .h(px(24.))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(3.))
        .px(px(5.))
        .rounded(l.px(Metric::RadiusControl))
        .cursor_pointer()
        .when(n > 0 || m.panel, |d| d.bg(l.pressed()).text_color(l.text()))
        .when(n == 0 && !m.panel, |d| d.text_color(l.text2()).hover(|d| d.bg(l.hover())))
        .child(icon(glyph::FILTER, 13.))
        .when(n > 0, |d| d.child(div().text_size(px(11.)).child(n.to_string())))
        .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tr!("filters-funnel")).build(window, cx))
        .on_click(move |_, _, cx| {
            cx.stop_propagation();
            model.update(cx, |m, cx| m.set(cx, |m| m.panel = !m.panel));
        })
}

/// Заголовок группы: стрелка, значок папки («Все папки»), подпись, число.
pub(crate) fn group_header(id: usize, label: String, n: usize, folder: bool, collapsed: bool, l: &Look) -> Stateful<Div> {
    div()
        .id(("group", id))
        .w_full()
        .h(px(HEADER_H))
        .flex()
        .items_center()
        .gap(px(6.))
        .pl(px(10.))
        .pr(px(14.))
        .bg(l.fill_passive())
        .border_b_1()
        .border_color(l.border())
        .text_size(l.px(Metric::TypeCaption))
        .text_color(l.text2())
        .cursor_pointer()
        .child(icon(if collapsed { glyph::CHEVRON_RIGHT } else { glyph::CHEVRON_DOWN }, 10.).w(px(12.)))
        .when(folder, |d| d.child(icon(glyph::FOLDER, 12.)))
        .child(div().font_weight(FontWeight::SEMIBOLD).text_color(l.text()).child(label))
        .child(div().child(n.to_string()))
}

/// Закладки быстрых фильтров поверх нижнего края карточки списка (ширина `strip_w`) и панель по наведению — над
/// ними, во всю ширину списка и области чтения, ничего не сдвигая (рисует экран почты поверх частей). Панель
/// открывает наведение на закладки, закрывает — уход мыши с закладок и панели. Щелчок по закладке или ячейке —
/// включить или выключить. Какие закладки и в каком порядке — `tabs` (Настройки → Быстрые фильтры).
pub(crate) fn quick(model: &Entity<FilterModel>, m: &FilterModel, tabs: &[Quick], l: &Look, strip_w: f32) -> Stateful<Div> {
    let on = |q: Quick| m.query.criteria.quick.contains(&q);
    let toggle = |q: Quick| {
        let model = model.clone();
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            model.update(cx, |m, cx| m.change(cx, |x| x.criteria.toggle_quick(q)));
        }
    };
    let cells_of = tabs;
    let tabs = tabs.iter().map(|&q| {
        let (n, active) = (m.counts.quick(q), on(q));
        let (short, _) = quick_keys(q);
        div()
            .id(SharedString::from(format!("quick-tab-{q:?}")))
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(3.))
            .px(px(4.))
            .rounded_t(px(6.))
            .text_size(px(11.))
            .cursor_pointer()
            .relative()
            .when(active, |d| {
                d.h(px(QUICK_H))
                    .bg(l.panel())
                    .text_color(l.text())
                    .font_weight(FontWeight::SEMIBOLD)
                    .border_l_1()
                    .border_r_1()
                    .border_color(l.stroke())
                    // Цветная полоса сверху.
                    .child(div().absolute().top_0().left_0().right_0().h(px(3.)).rounded_t(px(6.)).bg(quick_color(q, l)))
            })
            .when(!active, |d| d.h(px(20.)).bg(l.fill()).text_color(if n > 0 { l.text2() } else { l.text3() }))
            .child(div().size(px(6.)).flex_none().rounded(px(2.)).bg(quick_color(q, l)))
            .child(div().flex_1().min_w_0().truncate().child(tr!(short)))
            .child(div().flex_none().child(if n > 0 { n.to_string() } else { String::new() }))
            .on_click(toggle(q))
    });
    let hide = div()
        .id("quick-hide")
        .ml(px(3.))
        .h(px(20.))
        .w(px(16.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_t(px(6.))
        .bg(l.fill())
        .text_color(l.text2())
        .cursor_pointer()
        .hover(|d| d.text_color(l.text()))
        .child(icon(glyph::CLOSE, 9.))
        .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tr!("filters-quick-hide")).build(window, cx))
        .on_click(|_, window, cx| window.dispatch_action(Box::new(crate::filters::view::QuickFilters), cx));
    let strip = {
        let model = model.clone();
        div()
            .id("quick-strip")
            .w(px(strip_w))
            .h(px(QUICK_H))
            .flex_none()
            .flex()
            .items_end()
            .gap(px(3.))
            .px(px(2.))
            // Строки списка под закладками не просвечивают.
            .bg(l.panel())
            .children(tabs)
            .child(hide)
            .on_hover(move |hovered, _, cx| {
                if *hovered {
                    model.update(cx, |m, cx| {
                        if !m.quick_open {
                            m.set(cx, |m| m.quick_open = true);
                        }
                    });
                }
            })
    };
    let panel = m.quick_open.then(|| {
        let cells = cells_of.iter().map(|&q| {
            let (n, active) = (m.counts.quick(q), on(q));
            let (_, long) = quick_keys(q);
            let faded = n == 0 && !active;
            div()
                .id(SharedString::from(format!("quick-cell-{q:?}")))
                .flex_1()
                .min_w_0()
                .h(px(54.))
                .flex()
                .flex_col()
                .justify_center()
                .gap(px(2.))
                .px(px(10.))
                .rounded(px(6.))
                .cursor_pointer()
                .relative()
                .when(active, |d| {
                    d.bg(l.fill()).child(div().absolute().bottom_0().left_0().right_0().h(px(3.)).rounded_b(px(6.)).bg(quick_color(q, l)))
                })
                .when(!active, |d| d.border_1().border_color(l.border()).hover(|d| d.bg(l.hover())))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .text_size(l.px(Metric::TypeCaption))
                        .text_color(if faded { l.text3() } else { l.text2() })
                        .child(div().size(px(8.)).flex_none().rounded(px(2.)).bg(quick_color(q, l)))
                        .child(div().min_w_0().truncate().child(tr!(long))),
                )
                .child(
                    div()
                        .text_size(px(18.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(if faded { l.text3() } else { l.text() })
                        .child(n.to_string()),
                )
                .on_click(toggle(q))
        });
        div()
            .p(px(8.))
            .flex()
            .gap(px(6.))
            .rounded(l.px(Metric::RadiusOverlay))
            .border_1()
            .border_color(l.c(Role::StrokeCard))
            .bg(l.c(Role::LayerOverlay))
            .shadow_lg()
            .children(cells)
    });
    let model = model.clone();
    div().id("quick-zone").flex().flex_col().children(panel).child(strip).on_hover(move |hovered, _, cx| {
        if !*hovered {
            model.update(cx, |m, cx| {
                if m.quick_open {
                    m.set(cx, |m| m.quick_open = false);
                }
            });
        }
    })
}

/// Меню сортировки под кнопкой над списком; галочка — у текущего порядка.
pub(crate) fn sort_menu(current: Sort, l: &Look, pick: impl Fn(Sort) -> Box<dyn Action>) -> Div {
    let items = [Sort::Date, Sort::Sender, Sort::Size].map(|s| {
        let action = pick(s);
        div()
            .id(SharedString::from(format!("sort-{s:?}")))
            .h(l.px(Metric::HMenuItem))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(10.))
            .rounded(l.px(Metric::RadiusControl))
            .cursor_pointer()
            .hover(|d| d.bg(l.hover()))
            .child(div().w(px(14.)).flex_none().when(s == current, |d| d.child(icon(glyph::CHECK, 12.))))
            .child(tr!(sort_key(s)))
            .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
    });
    div()
        .min_w(px(180.))
        .p(px(4.))
        .flex()
        .flex_col()
        .rounded(l.px(Metric::RadiusOverlay))
        .border_1()
        .border_color(l.c(Role::StrokeCard))
        .bg(l.c(Role::LayerOverlay))
        .text_color(l.text())
        .shadow_lg()
        .children(items)
}

// --- экран почты: команды и сервер ---

impl crate::Mailer {
    /// Условия изменились. Живой ящик, «Все папки» своего аккаунта: те же фильтры — `Email/query` на сервере
    /// (в кэше лишь первые страницы папок); найденное список добавит к своему (`set_server_hits`).
    pub(crate) fn filters_changed(&mut self, cx: &mut Context<Self>) {
        self.live.filter_ticket = None;
        if !self.backend.is_live() {
            return;
        }
        let query = self.filters.read(cx).query.clone();
        let list = self.list.read(cx);
        let own = list.mailbox.as_deref().is_some_and(|mb| postman_cache::account_of(mb).is_none());
        if query.scope != Where::All || !own {
            return;
        }
        let Some(filter) = query.criteria.server_filter(&list.me) else { return };
        let ticket = self.live.ticket();
        if self.backend.send(postman_core::sync::Command::Query { filter, mailbox: None, ticket }) {
            self.live.filter_ticket = Some(ticket);
        } else {
            self.sync_stopped(cx);
        }
    }

    /// `view.filters` (воронка): панель слева — папки или фильтры.
    pub(crate) fn toggle_filter_panel(&mut self, cx: &mut Context<Self>) {
        self.filters.update(cx, |m, cx| m.set(cx, |m| m.panel = !m.panel));
    }

    /// `view.quick_filters` («×» закладок, Вид → Быстрые фильтры): скрыть или вернуть — настройка
    /// `QuickFilters::hidden` (та же, что в Настройки → Быстрые фильтры); скрыли — сказать, как вернуть.
    pub(crate) fn toggle_quick_filters(&mut self, cx: &mut Context<Self>) {
        let hidden = !crate::prefs::get(cx).quick.hidden;
        crate::prefs::update(cx, |p| p.quick.hidden = hidden);
        if hidden {
            self.toasts.update(cx, |t, cx| t.push(pets_ui::Level::Info, tr!("filters-quick-hidden"), cx));
        }
    }

    /// Закладки в списке по настройкам: показанные, по порядку; полоса скрыта — нет.
    pub(crate) fn quick_tabs(cx: &App) -> Vec<Quick> {
        crate::prefs::get(cx).quick.tabs().map(Quick::from).collect()
    }

    /// Настройки изменились: закладка скрыта (или все) — её фильтр снимается, невидимых условий нет.
    pub(crate) fn prune_quick(&mut self, cx: &mut Context<Self>) {
        let tabs = Self::quick_tabs(cx);
        let gone = self.filters.read(cx).query.criteria.quick.iter().any(|q| !tabs.contains(q));
        if gone {
            self.filters.update(cx, |m, cx| m.change(cx, |q| q.criteria.quick.retain(|x| tabs.contains(x))));
        }
        if tabs.is_empty() && self.filters.read(cx).quick_open {
            self.filters.update(cx, |m, cx| m.set(cx, |m| m.quick_open = false));
        }
    }

    /// Закладки быстрых фильтров и панель по наведению — поверх нижнего края карточки списка (`body` экрана
    /// почты); закладки скрыты, поиск, не «Почта» — нет.
    pub(crate) fn quick_overlay(&self, l: &Look, cx: &App) -> Option<AnyElement> {
        use postman_core::prefs::Pane;
        let m = self.filters.read(cx);
        let tabs = Self::quick_tabs(cx);
        let mail = self.section == crate::Section::Mail && self.tabs.active().kind.shows_mail();
        if !mail || tabs.is_empty() || self.live.signin.is_some() || self.list.read(cx).search_ids.is_some() {
            return None;
        }
        // Панель чтения (Настройки → Чтение): справа и скрыта — список до низа, снизу — список сверху на 45 %.
        let pane = crate::prefs::get(cx).reading.pane;
        if pane == Pane::Hidden && self.reading_instead_of_list(cx) {
            return None;
        }
        let left = crate::chrome::W_RAIL + crate::mailer::W_FOLDERS;
        let overlay = div().absolute().left(px(left)).right(px(8.));
        let overlay = if pane == Pane::Bottom { overlay.bottom(relative(0.55)) } else { overlay.bottom_0() };
        // Ширина закладок — карточка списка без рамки.
        Some(overlay.child(quick(&self.filters, m, &tabs, l, crate::mailer::W_LIST - 2.)).into_any_element())
    }

    /// Меню сортировки над списком.
    pub(crate) fn sort_by(&mut self, sort: Sort, cx: &mut Context<Self>) {
        self.list.update(cx, |l, cx| {
            l.sort_open = false;
            cx.notify();
        });
        self.filters.update(cx, |m, cx| m.change(cx, |q| q.sort = sort));
    }

    /// `filters.reset`: снять фильтры панели и закладки.
    pub(crate) fn reset_filters(&mut self, cx: &mut Context<Self>) {
        self.filters.update(cx, |m, cx| {
            m.change(cx, |q| {
                q.criteria.clear_panel();
                q.criteria.quick.clear();
            })
        });
    }
}

/// Опись части (ADR 0005, `parts.rs`).
pub(crate) const PART: crate::part::Part =
    crate::part::Part { name: "filters", texts: TEXTS, commands: COMMANDS, handlers, scenes: scenes::SCENES, ..crate::part::Part::NONE };

/// Обработчики команд части — на корне экрана почты (`Mailer::render`).
fn handlers(root: Div, cx: &mut Context<crate::Mailer>) -> Div {
    root.on_action(cx.listener(|this, _: &view::Filters, _, cx| this.toggle_filter_panel(cx)))
        .on_action(cx.listener(|this, _: &view::QuickFilters, _, cx| this.toggle_quick_filters(cx)))
        .on_action(cx.listener(|this, _: &view::SortByDate, _, cx| this.sort_by(postman_core::filters::Sort::Date, cx)))
        .on_action(cx.listener(|this, _: &view::SortBySender, _, cx| this.sort_by(postman_core::filters::Sort::Sender, cx)))
        .on_action(cx.listener(|this, _: &view::SortBySize, _, cx| this.sort_by(postman_core::filters::Sort::Size, cx)))
        .on_action(cx.listener(|this, _: &filters::Reset, _, cx| this.reset_filters(cx)))
}

/// Тексты части (`i18n/<язык>/<часть>.ftl`).
const TEXTS: &[pets_i18n::Resource] = &[("ru", include_str!("../i18n/ru/filters.ftl")), ("en", include_str!("../i18n/en/filters.ftl"))];

/// Вид списка (S5-03): панель фильтров, быстрые фильтры, сортировка.
pub(crate) mod view {
    gpui_kit::actions!(view, [Filters, QuickFilters, SortByDate, SortBySender, SortBySize]);
}

/// Фильтры списка (S5-03).
// Имя — область действий (`filters::…`), как в id команд: так обработчики читаются и их находит `docs/PARITY.md`.
#[allow(clippy::module_inception)]
pub(crate) mod filters {
    gpui_kit::actions!(filters, [Reset]);
}

/// Команды части (`commands.rs`: контексты и `cmd`).
const COMMANDS: &[pets_commands::Command] = {
    use crate::commands::{MAIL, cmd};
    &[
        // Фильтры (S5-03): панель на месте папок — воронкой и из палитры, без клавиши (S5-16, вопрос 2).
        cmd("view.filters", &[], MAIL),
        // Вид → Быстрые фильтры: вернуть скрытые закладки; сортировка — меню над списком; без клавиш.
        cmd("view.quick_filters", &[], MAIL),
        cmd("view.sort_by_date", &[], MAIL),
        cmd("view.sort_by_sender", &[], MAIL),
        cmd("view.sort_by_size", &[], MAIL),
        cmd("filters.reset", &[], MAIL),
    ]
};

/// Сцены `--screens` фильтров (ADR 0005).
pub(crate) mod scenes {
    use gpui_kit::*;
    use postman_core::filters::{Check, Quick};

    use crate::screens::{self, Scene};
    use crate::{Mailer, Section};

    pub(crate) const SCENES: &[Scene] = &[
        // Фильтры (S5-03): панель фильтров на месте папок, «Все папки», «Непрочитанные» — чип, «N из M», группы
        // по папкам.
        Scene::new("filters", panel, panel_ready),
        // Быстрые фильтры (S5-03): закладки «Угрозы» и «Срок» включены (ИЛИ), панель по наведению открыта,
        // группы по датам.
        Scene::new("quick", quick, quick_ready),
    ];

    fn panel(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.set_section(Section::Mail, cx);
        let inbox = crate::live::inbox(&m.folders.read(cx).mailboxes);
        let Some(id) = inbox else { return Err("no inbox".into()) };
        m.open_mailbox(id, cx);
        m.filters.update(cx, |f, cx| {
            f.set(cx, |f| f.panel = true);
            f.change(cx, |q| {
                q.scope = super::Where::All;
                q.criteria.toggle_check(Check::Unread);
            });
        });
        Ok(())
    }

    fn panel_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        let (f, list) = (m.filters.read(cx), m.list.read(cx));
        f.panel
            && list.query.scope == super::Where::All
            && !list.query.criteria.is_empty()
            && list.view.is_some()
            && !list.computing
            && f.counts.total > 0
            && screens::rows_loaded(list)
    }

    fn quick(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.filters.update(cx, |f, cx| {
            f.set(cx, |f| f.panel = false);
            f.change(cx, |q| {
                q.scope = super::Where::Folder;
                q.criteria.clear_panel();
                q.criteria.toggle_quick(Quick::Threats);
                q.criteria.toggle_quick(Quick::Due);
            });
        });
        // Панель закладок — как по наведению.
        m.filters.update(cx, |f, cx| f.set(cx, |f| f.quick_open = true));
        Ok(())
    }

    fn quick_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        let (f, list) = (m.filters.read(cx), m.list.read(cx));
        !f.panel
            && list.query.scope == super::Where::Folder
            && list.query.criteria.quick.len() == 2
            && list.view.is_some()
            && !list.computing
            && f.quick_open
            && (screens::rows_loaded(list) || list.count == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::{Layout, Query, Slot, Where};
    use postman_core::filters::{Day, Group, Quick, Sort};
    use std::collections::HashSet;

    fn groups() -> Vec<(Group, usize)> {
        vec![(Group::Day(Day::Today), 2), (Group::Day(Day::Yesterday), 3), (Group::Day(Day::Earlier), 4)]
    }

    #[test]
    fn layout_maps_items_and_rows() {
        let l = Layout::new(&groups(), 9, &HashSet::new());
        assert_eq!(l.len(), 12);
        assert_eq!(l.slot(0), Some(Slot::Header(0)));
        assert_eq!(l.slot(1), Some(Slot::Row(0)));
        assert_eq!(l.slot(3), Some(Slot::Header(1)));
        assert_eq!(l.slot(4), Some(Slot::Row(2)));
        assert_eq!(l.slot(11), Some(Slot::Row(8)));
        assert_eq!(l.slot(12), None);
        assert_eq!(l.item_of(2), Some(4));
        assert_eq!(l.rows_in(0..5), 0..3);
        assert_eq!(l.group_of(5), Some(&Group::Day(Day::Earlier)));
        // Без групп — номер элемента и есть номер письма.
        let flat = Layout::new(&[], 5, &HashSet::new());
        assert_eq!((flat.len(), flat.slot(3), flat.item_of(4), flat.grouped()), (5, Some(Slot::Row(3)), Some(4), false));
    }

    #[test]
    fn collapsed_groups_hide_their_rows() {
        let collapsed: HashSet<Group> = [Group::Day(Day::Yesterday)].into();
        let l = Layout::new(&groups(), 9, &collapsed);
        assert_eq!(l.len(), 9);
        assert_eq!(l.slot(3), Some(Slot::Header(1)));
        assert_eq!(l.slot(4), Some(Slot::Header(2)));
        assert_eq!(l.slot(5), Some(Slot::Row(5)));
        assert_eq!(l.item_of(3), None);
        // По письмам: из свёрнутой группы вперёд — первое следующей, назад — последнее предыдущей.
        assert_eq!(l.visible(3, true), Some(5));
        assert_eq!(l.visible(3, false), Some(1));
        assert_eq!(l.visible(1, true), Some(1));
        // Высоты: заголовки, строки и место под закладками.
        let sizes = l.sizes(66., 30., 24.);
        assert_eq!(sizes.len(), l.len() + 1);
        assert_eq!(f32::from(sizes[0].height), 30.);
        assert_eq!(f32::from(sizes[1].height), 66.);
        assert_eq!(f32::from(sizes[9].height), 24.);
    }

    #[test]
    fn groups_follow_the_count() {
        // Писем стало больше, чем в группах (пришло новое между чтениями): остаток — в последней группе.
        let l = Layout::new(&groups(), 10, &HashSet::new());
        assert_eq!(l.segs().last().map(|s| s.rows.clone()), Some(5..10));
        // Меньше — группы обрезаются, пустые пропадают.
        let l = Layout::new(&groups(), 1, &HashSet::new());
        assert_eq!((l.len(), l.segs().len()), (2, 1));
        let empty = Layout::new(&groups(), 0, &HashSet::new());
        assert_eq!(empty.len(), 0);
    }

    #[test]
    fn own_list_only_when_needed() {
        let mut q = Query::default();
        assert!(!q.own_list(), "папка по дате — страницы из кэша");
        q.sort = Sort::Size;
        assert!(q.own_list());
        q.sort = Sort::Date;
        q.scope = Where::All;
        assert!(q.own_list());
        q.scope = Where::Folder;
        q.criteria.toggle_quick(Quick::ToMe);
        assert!(q.own_list());
    }
}
