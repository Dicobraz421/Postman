//! «Настройки → Календарь» (S5-20, DESIGN «Настройки → Календарь», макет «Настройки → Календарь»): рабочая неделя
//! и часы, вид, шаг, пояса, параметры новых встреч, цвет свободного времени с образцом, календари коллег,
//! приглашения. Хранение — `Prefs::calendar` (`postman_core::prefs::Calendar`).
//!
//! Действуют сразу: рабочие дни и часы (затенение недели, общие окна встречи), пояс (календарь, встреча,
//! приглашение, контакты), длительность новой встречи, «Учитывать необязательных» (новая вкладка встречи), цвет
//! свободного времени (встреча), удаление письма-приглашения после ответа; вид по умолчанию, первый день недели, шаг
//! сетки и второй пояс — виды календаря (S5-21); цвета календарей, «Цвета по владельцу» и «Имя владельца» (S5-22).
//! Остальное помечено «сохраняется»: напоминание, «на 5 мин позже», «под вопросом» — позже.

use std::rc::Rc;

use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::{Metric, Role};
use postman_core::prefs::{Calendar, CalendarView, DURATIONS, FreeColor, REMINDERS, STEPS, WeekStart, ZONES};

use crate::buttons::button;
use crate::calendar::{duration_text, g};
use crate::chrome::{glyph, icon};
use crate::roles::{Look, Pm};
use crate::settings_tab::{SettingsView, aside_card, group, seg, seg_group, toggle};

/// Открытый список выбора.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Menu {
    Start,
    End,
    Zone,
    SecondZone,
    Reminder,
}

/// Пункт списка: подпись, выбран ли, что поменять.
type Pick = (String, bool, Rc<dyn Fn(&mut Calendar)>);

/// Часы выбора начала и конца рабочего дня, мин.
const STARTS: [u16; 5] = [7 * 60, 8 * 60, 9 * 60, 10 * 60, 11 * 60];
const ENDS: [u16; 5] = [16 * 60, 17 * 60, 18 * 60, 19 * 60, 20 * 60];

/// «09:00».
fn clock(m: u16) -> String {
    format!("{:02}:{:02}", m / 60, m % 60)
}

/// «(UTC+03:00)» — смещение пояса сейчас.
fn offset(tz: &jiff::tz::TimeZone) -> String {
    let s = jiff::Timestamp::now().to_zoned(tz.clone()).offset().seconds();
    let (sign, abs) = if s < 0 { ('−', -s) } else { ('+', s) };
    format!("(UTC{sign}{:02}:{:02})", abs / 3600, abs % 3600 / 60)
}

/// «(UTC+03:00) Москва»; пояс не из списка — его имя IANA.
pub(crate) fn zone_name(name: &str) -> String {
    let tz = jiff::tz::TimeZone::get(name).unwrap_or(jiff::tz::TimeZone::UTC);
    let city = if ZONES.contains(&name) { tr!(&format!("settings-cal-zone-{}", name.to_lowercase().replace('/', "-"))) } else { name.to_owned() };
    format!("{} {city}", offset(&tz))
}

/// Пояс календаря в списке: свой или «Как в Windows».
fn zone_choice(zone: Option<&str>) -> String {
    match zone {
        Some(z) => zone_name(z),
        None => tr!("settings-cal-zone-system", zone = offset(&jiff::tz::TimeZone::system())),
    }
}

fn reminder_text(m: u16) -> String {
    if m == 0 { tr!("settings-cal-reminder-none") } else { tr!("settings-cal-reminder-before", time = duration_text(i64::from(m) * 60)) }
}

fn view_name(v: CalendarView) -> String {
    tr!(&format!("settings-cal-view-{}", v.id()))
}

pub(crate) fn free_name(c: FreeColor) -> String {
    tr!(&format!("settings-cal-free-{}", c.id()))
}

/// Пометка «сохраняется»: настройка запомнена, действует со своей задачей.
fn stored(l: &Look) -> Div {
    div().flex_none().px_1p5().rounded_sm().bg(l.fill()).text_size(px(11.)).text_color(l.text2()).child(tr!("settings-cal-stored"))
}

/// Строка настройки, как `settings_tab::row`, с пометкой у названия.
fn line(title: String, badge: Option<Div>, hint: Option<String>, control: impl IntoElement, l: &Look) -> Div {
    div()
        .flex()
        .items_center()
        .gap_4()
        .py_3()
        .border_b_1()
        .border_color(l.border())
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().flex().items_center().gap_2().child(title).children(badge))
                .when_some(hint, |d, h| d.child(div().text_size(px(12.)).text_color(l.text2()).child(h))),
        )
        .child(control)
}

impl SettingsView {
    /// Список выбора: кнопка с текущим значением и пункты под ней.
    fn cal_select(&self, menu: Menu, label: String, width: f32, items: Vec<Pick>, l: &Look, cx: &mut Context<Self>) -> Div {
        let open = self.cal_menu == Some(menu);
        let field = div()
            .id(("settings-cal-select", menu as usize))
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
                this.cal_menu = if this.cal_menu == Some(menu) { None } else { Some(menu) };
                // Новый список — сверху.
                this.cal_scroll.set_offset(point(px(0.), px(0.)));
                cx.notify();
            }))
            .child(div().min_w_0().overflow_hidden().whitespace_nowrap().child(label))
            .child(icon(glyph::CHEVRON, 10.).text_color(l.text2()));
        let list = open.then(|| {
            let rows = items.into_iter().enumerate().map(|(i, (text, on, set))| {
                div()
                    .id(("settings-cal-item", menu as usize * 100 + i))
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
                        crate::prefs::update(cx, |p| set(&mut p.calendar));
                        this.cal_menu = None;
                        cx.notify();
                    }))
            });
            // Список длиннее 336 px прокручивается с полосой при наведении (T1-08); полоса — поверх рамки списка.
            let scroll = self.cal_scroll.clone();
            deferred(
                div()
                    .absolute()
                    .top(px(32.))
                    .right_0()
                    .min_w(px(width))
                    .occlude()
                    .rounded(l.px(Metric::RadiusOverlay))
                    .border_1()
                    .border_color(l.c(Role::StrokeCard))
                    .bg(l.c(Role::LayerOverlay))
                    .text_color(l.text())
                    .shadow_lg()
                    .child(
                        div()
                            .id(("settings-cal-menu", menu as usize))
                            .max_h(px(336.))
                            .overflow_y_scroll()
                            .track_scroll(&scroll)
                            .flex()
                            .flex_col()
                            .p(px(4.))
                            .children(rows),
                    )
                    .child(Scrollbar::vertical(&scroll).mode(ScrollbarMode::Hover))
                    .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                        this.cal_menu = None;
                        cx.notify();
                    })),
            )
            .with_priority(1)
        });
        div().relative().flex_none().child(field).children(list)
    }

    /// Переключатель настройки календаря.
    fn cal_toggle(&self, id: &'static str, on: bool, flip: fn(&mut Calendar), l: &Look, cx: &mut Context<Self>) -> Stateful<Div> {
        toggle(id, on, l).on_click(cx.listener(move |_, _, _, cx| {
            crate::prefs::update(cx, |p| flip(&mut p.calendar));
            cx.notify();
        }))
    }

    /// Цвета календарей (S5-22): мой и каждого коллеги, кого календарь уже видел; щелчок по образцу — цвет.
    fn calendar_colors(&self, c: &Calendar, l: &Look, cx: &mut Context<Self>) -> Vec<Div> {
        c.calendar_colors
            .keys()
            .enumerate()
            .map(|(n, key)| {
                let name = if key.is_empty() { tr!("cal-my-calendar") } else { key.clone() };
                let current = c.calendar_colors.get(key).cloned().unwrap_or_default();
                let swatches = postman_core::palette::CALENDAR.iter().enumerate().map(|(m, color)| {
                    let (k, col) = (key.clone(), (*color).to_owned());
                    div()
                        .id(("settings-cal-color", n * 8 + m))
                        .size(px(20.))
                        .rounded(px(4.))
                        .bg(l.cat(color).dot)
                        .border_2()
                        .border_color(if current == *color { l.text() } else { l.none() })
                        .cursor_pointer()
                        .on_click(cx.listener(move |_, _, _, cx| {
                            let (k, col) = (k.clone(), col.clone());
                            crate::prefs::update(cx, move |p| {
                                p.calendar.calendar_colors.insert(k, col);
                            });
                            cx.notify();
                        }))
                });
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .py_1()
                    .child(div().flex_1().min_w_0().truncate().child(name))
                    .child(div().flex().gap_1p5().children(swatches))
            })
            .collect()
    }

    /// Сегмент «одно из» настройки календаря.
    fn cal_seg(
        &self,
        id: (&'static str, usize),
        label: String,
        on: bool,
        set: impl Fn(&mut Calendar) + 'static,
        l: &Look,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        seg(id, label, on, true, l).on_click(cx.listener(move |_, _, _, cx| {
            crate::prefs::update(cx, |p| set(&mut p.calendar));
            cx.notify();
        }))
    }

    /// Раздел «Календарь»: «По умолчанию» в шапке, содержимое, предпросмотр сбоку.
    pub(crate) fn calendar(&mut self, l: &Look, cx: &mut Context<Self>) -> (Option<Stateful<Div>>, AnyElement, Vec<Div>) {
        let c = crate::prefs::get(cx).calendar;
        let reset = button("settings-cal-reset", tr!("settings-cal-reset"), false, l).on_click(cx.listener(|this, _, _, cx| {
            crate::prefs::update(cx, |p| p.calendar = Calendar::default());
            this.cal_menu = None;
            cx.notify();
        }));
        let body = self.calendar_body(&c, l, cx).into_any_element();
        (Some(reset), body, vec![self.calendar_preview(&c, l)])
    }

    fn calendar_body(&self, c: &Calendar, l: &Look, cx: &mut Context<Self>) -> Div {
        // Рабочие дни — по порядку недели (с понедельника или с воскресенья).
        let order: [usize; 7] = match c.week_start {
            WeekStart::Monday => [0, 1, 2, 3, 4, 5, 6],
            WeekStart::Sunday => [6, 0, 1, 2, 3, 4, 5],
        };
        let days: Vec<Stateful<Div>> = order
            .into_iter()
            .map(|d| {
                self.cal_seg(
                    ("settings-cal-day", d),
                    tr!("settings-cal-day", d = d),
                    c.work_days[d],
                    move |c| c.work_days[d] = !c.work_days[d],
                    l,
                    cx,
                )
            })
            .collect();
        let hours_pick = |values: [u16; 5], current: u16, start: bool| -> Vec<Pick> {
            values
                .into_iter()
                .map(|m| {
                    let set: Rc<dyn Fn(&mut Calendar)> = Rc::new(move |c: &mut Calendar| {
                        if start {
                            c.work_start = m;
                        } else {
                            c.work_end = m;
                        }
                    });
                    (clock(m), m == current, set)
                })
                .collect()
        };
        let hours = div()
            .flex()
            .items_center()
            .gap_2()
            .child(self.cal_select(Menu::Start, clock(c.work_start), 84., hours_pick(STARTS, c.work_start, true), l, cx))
            .child(div().text_color(l.text2()).child("—"))
            .child(self.cal_select(Menu::End, clock(c.work_end), 84., hours_pick(ENDS, c.work_end, false), l, cx));
        let firsts: Vec<Stateful<Div>> = WeekStart::ALL
            .into_iter()
            .enumerate()
            .map(|(i, w)| {
                let key = if w == WeekStart::Monday { "settings-cal-first-monday" } else { "settings-cal-first-sunday" };
                self.cal_seg(("settings-cal-first", i), tr!(key), c.week_start == w, move |c| c.week_start = w, l, cx)
            })
            .collect();
        let views: Vec<Stateful<Div>> = CalendarView::ALL
            .into_iter()
            .enumerate()
            .map(|(i, v)| self.cal_seg(("settings-cal-view", i), view_name(v), c.view == v, move |c| c.view = v, l, cx))
            .collect();
        let steps: Vec<Stateful<Div>> = STEPS
            .into_iter()
            .map(|s| self.cal_seg(("settings-cal-step", s as usize), duration_text(i64::from(s) * 60), c.step == s, move |c| c.step = s, l, cx))
            .collect();
        let mut zones: Vec<Pick> = vec![(zone_choice(None), c.zone.is_none(), Rc::new(|c: &mut Calendar| c.zone = None))];
        zones.extend(ZONES.iter().map(|&z| {
            let set: Rc<dyn Fn(&mut Calendar)> = Rc::new(move |c: &mut Calendar| c.zone = Some(z.to_owned()));
            (zone_name(z), c.zone.as_deref() == Some(z), set)
        }));
        let seconds: Vec<Pick> = ZONES
            .iter()
            .map(|&z| {
                let set: Rc<dyn Fn(&mut Calendar)> = Rc::new(move |c: &mut Calendar| c.second_zone = z.to_owned());
                (zone_name(z), c.second_zone == z, set)
            })
            .collect();
        let second = div()
            .flex()
            .items_center()
            .gap_3()
            .child(self.cal_select(Menu::SecondZone, zone_name(&c.second_zone), 260., seconds, l, cx))
            .child(self.cal_toggle("settings-cal-second", c.second_zone_on, |c| c.second_zone_on = !c.second_zone_on, l, cx));
        // Длительность — как на макете: 30 мин и 1 ч; другая из файла (1,5 и 2 ч) — тоже видна выбранной.
        let durations: Vec<Stateful<Div>> = DURATIONS
            .into_iter()
            .filter(|&d| d <= 60 || d == c.duration)
            .map(|d| {
                self.cal_seg(("settings-cal-duration", d as usize), duration_text(i64::from(d) * 60), c.duration == d, move |c| c.duration = d, l, cx)
            })
            .collect();
        let reminders: Vec<Pick> = REMINDERS
            .into_iter()
            .map(|m| {
                let set: Rc<dyn Fn(&mut Calendar)> = Rc::new(move |c: &mut Calendar| c.reminder = m);
                (reminder_text(m), c.reminder == m, set)
            })
            .collect();
        let recommended = div()
            .flex_none()
            .px_1p5()
            .rounded_sm()
            .bg(l.fill_passive())
            .text_size(px(11.))
            .text_color(l.text2())
            .child(tr!("settings-cal-recommended"));
        let late_badges = div().flex().items_center().gap_2().child(recommended).child(stored(l));
        let late_hint = if c.start_late { tr!("settings-cal-late-on") } else { tr!("settings-cal-late-off") };
        let optional_hint = if c.with_optional { tr!("settings-cal-optional-on") } else { tr!("settings-cal-optional-off") };
        div()
            .flex()
            .flex_col()
            .child(group(tr!("settings-cal-week-group"), l))
            .child(line(tr!("settings-cal-days"), None, Some(tr!("settings-cal-days-hint")), seg_group(days, l), l))
            .child(line(tr!("settings-cal-hours"), None, Some(tr!("settings-cal-hours-hint")), hours, l))
            .child(line(tr!("settings-cal-first"), None, Some(tr!("settings-cal-first-hint")), seg_group(firsts, l), l))
            .child(group(tr!("settings-cal-view-group"), l))
            .child(line(tr!("settings-cal-view"), None, Some(tr!("settings-cal-view-hint")), seg_group(views, l), l))
            .child(line(tr!("settings-cal-step"), None, Some(tr!("settings-cal-step-hint")), seg_group(steps, l), l))
            .child(line(
                tr!("settings-cal-zone"),
                None,
                Some(tr!("settings-cal-zone-hint")),
                self.cal_select(Menu::Zone, zone_choice(c.zone.as_deref()), 260., zones, l, cx),
                l,
            ))
            .child(line(tr!("settings-cal-second"), None, Some(tr!("settings-cal-second-hint")), second, l))
            .child(group(tr!("settings-cal-meet-group"), l))
            .child(line(tr!("settings-cal-duration"), None, Some(tr!("settings-cal-duration-hint")), seg_group(durations, l), l))
            .child(line(
                tr!("settings-cal-reminder"),
                Some(stored(l)),
                Some(tr!("settings-cal-reminder-hint")),
                self.cal_select(Menu::Reminder, reminder_text(c.reminder), 120., reminders, l, cx),
                l,
            ))
            .child(line(
                tr!("settings-cal-late"),
                Some(late_badges),
                Some(late_hint),
                self.cal_toggle("settings-cal-late", c.start_late, |c| c.start_late = !c.start_late, l, cx),
                l,
            ))
            .child(line(
                tr!("settings-cal-optional"),
                None,
                Some(optional_hint),
                self.cal_toggle("settings-cal-optional", c.with_optional, |c| c.with_optional = !c.with_optional, l, cx),
                l,
            ))
            .child(group(tr!("settings-cal-free"), l))
            .child(div().pb_2().text_size(px(12.)).text_color(l.text2()).child(tr!("settings-cal-free-hint")))
            .child(self.free_card(c, l, cx))
            .child(group(tr!("settings-cal-colleagues-group"), l))
            .child(line(
                tr!("settings-cal-owner-colors"),
                None,
                Some(tr!("settings-cal-owner-colors-hint")),
                self.cal_toggle("settings-cal-owner-colors", c.owner_colors, |c| c.owner_colors = !c.owner_colors, l, cx),
                l,
            ))
            .child(line(
                tr!("settings-cal-owner-names"),
                None,
                Some(tr!("settings-cal-owner-names-hint")),
                self.cal_toggle("settings-cal-owner-names", c.owner_names, |c| c.owner_names = !c.owner_names, l, cx),
                l,
            ))
            .children(self.calendar_colors(c, l, cx))
            .child(group(tr!("settings-cal-invites-group"), l))
            .child(line(
                tr!("settings-cal-tentative"),
                Some(stored(l)),
                Some(tr!("settings-cal-tentative-hint")),
                self.cal_toggle("settings-cal-tentative", c.tentative_invites, |c| c.tentative_invites = !c.tentative_invites, l, cx),
                l,
            ))
            .child(line(
                tr!("settings-cal-delete"),
                None,
                Some(tr!("settings-cal-delete-hint")),
                self.cal_toggle("settings-cal-delete", c.delete_answered, |c| c.delete_answered = !c.delete_answered, l, cx),
                l,
            ))
    }

    /// Цвет свободного времени: три образца (цвета бренда) и полоса-пример — занятость участников и общее окно выбранным цветом.
    fn free_card(&self, c: &Calendar, l: &Look, cx: &mut Context<Self>) -> Div {
        let swatches = FreeColor::ALL.into_iter().enumerate().map(|(i, color)| {
            let on = c.free_color == color;
            let line = l.free(color).line;
            div()
                .id(("settings-cal-free", i))
                .w(px(64.))
                .flex()
                .flex_col()
                .items_center()
                .gap_1p5()
                .cursor_pointer()
                .child(
                    div()
                        .size(px(30.))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .border_2()
                        .border_color(if on { line } else { l.none() })
                        .child(
                            div()
                                .size(px(22.))
                                .rounded_full()
                                .bg(line)
                                .flex()
                                .items_center()
                                .justify_center()
                                .when(on, |d| d.child(icon(g::CHECK, 11.).text_color(l.pm(Pm::FbFreeOn)))),
                        ),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .whitespace_nowrap()
                        .when(on, |d| d.font_weight(FontWeight::SEMIBOLD).text_color(l.text()))
                        .when(!on, |d| d.text_color(l.text2()))
                        .child(free_name(color)),
                )
                .on_click(cx.listener(move |_, _, _, cx| {
                    crate::prefs::update(cx, |p| p.calendar.free_color = color);
                    cx.notify();
                }))
        });
        // Образец: часы 08–19, занятость участников, общее окно 13:00–15:30.
        let (h0, h1) = (8.0_f32, 19.0_f32);
        let frac = |h: f32| (h - h0) / (h1 - h0);
        let free = l.free(c.free_color);
        let busy = [(9.0, 10.5), (11.0, 13.0), (15.5, 17.0)].map(|(a, b)| {
            div().absolute().top(px(4.)).bottom(px(4.)).left(relative(frac(a))).w(relative(frac(b) - frac(a))).rounded(px(3.)).bg(l.pm(Pm::FbMute))
        });
        let window = div()
            .absolute()
            .top(px(2.))
            .bottom(px(2.))
            .left(relative(frac(13.0)))
            .w(relative(frac(15.5) - frac(13.0)))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .bg(free.bg)
            .border_1()
            .border_color(free.line)
            .text_size(px(11.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(free.fg)
            .child(format!("{}–{}", clock(13 * 60), clock(15 * 60 + 30)));
        let strip = div()
            .relative()
            .h(px(28.))
            .rounded(px(4.))
            .border_1()
            .border_color(l.stroke())
            .bg(l.panel())
            .overflow_hidden()
            .children(busy)
            .child(window);
        let ticks = div().relative().h(px(14.)).text_size(px(10.)).text_color(l.text3()).children((8..=19).map(|h| {
            let t = div().absolute().top_0().child(format!("{h:02}"));
            if h == 19 { t.right_0() } else { t.left(relative(frac(h as f32))) }
        }));
        div()
            .flex()
            .items_center()
            .gap_6()
            .px_4()
            .py_3()
            .rounded(l.px(Metric::RadiusOverlay))
            .border_1()
            .border_color(l.border())
            .child(div().flex().flex_none().gap_2().children(swatches))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(strip)
                    .child(ticks)
                    .child(div().text_size(px(12.)).text_color(l.text2()).child(tr!("settings-cal-free-note", color = free_name(c.free_color)))),
            )
    }

    /// Сбоку: что получится — вид и шаг, рабочие дни и часы, новая встреча, пояс.
    fn calendar_preview(&self, c: &Calendar, l: &Look) -> Div {
        let days = c.work_days.iter().filter(|&&d| d).count();
        let lines = vec![
            tr!("settings-cal-preview-view", view = view_name(c.view), step = duration_text(i64::from(c.step) * 60)),
            tr!("settings-cal-preview-days", n = days, from = clock(c.work_start), to = clock(c.work_end)),
            tr!("settings-cal-preview-meeting", duration = duration_text(c.duration_secs()), reminder = reminder_text(c.reminder)),
            tr!("settings-cal-preview-zone", zone = zone_choice(c.zone.as_deref())),
            tr!("settings-cal-preview-stored"),
        ];
        aside_card(tr!("settings-cal-preview"), lines, l)
    }
}

#[cfg(test)]
mod tests {
    use super::{clock, zone_name};
    use postman_core::prefs::ZONES;

    #[test]
    fn clock_and_zone_names() {
        assert_eq!((clock(9 * 60), clock(17 * 60 + 30), clock(0)), ("09:00".to_owned(), "17:30".to_owned(), "00:00".to_owned()));
        // Каждый пояс выбора известен jiff и подписан смещением.
        for z in ZONES {
            assert!(jiff::tz::TimeZone::get(z).is_ok(), "{z}");
            assert!(zone_name(z).starts_with("(UTC"), "{z}");
        }
        assert!(zone_name("Asia/Tokyo").ends_with("Asia/Tokyo"), "не из списка — имя IANA");
    }

    /// У каждого пояса выбора — название в обоих языках.
    #[test]
    fn every_zone_has_texts() {
        for (lang, ftl) in [("ru", include_str!("../i18n/ru/settings.ftl")), ("en", include_str!("../i18n/en/settings.ftl"))] {
            for z in ZONES {
                let key = format!("settings-cal-zone-{} =", z.to_lowercase().replace('/', "-"));
                assert!(ftl.lines().any(|l| l.starts_with(&key)), "{lang}: нет {key}");
            }
        }
    }
}
