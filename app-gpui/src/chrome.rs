//! Каркас окна (S5-01, DESIGN «Главное окно»): свой заголовок — значок приложения, разделы, вкладки, место для
//! перетаскивания, шестерёнка «Настройки» и «─ □ ✕» (зоны `WindowControlArea`: перетаскивание, двойной щелчок —
//! развернуть, Snap Layouts на «□»), строка состояния и меню «…» командной строки.
//!
//! Заголовок рисует сам экран почты, не кэшируемая view: gpui не повторяет `WindowControlArea` из кэша
//! (`window_control_hitboxes` нет в `reuse_paint`), закэшированный заголовок потерял бы перетаскивание и кнопки.
//! Кнопки «─ □ ✕» и зона перетаскивания — без своих обработчиков мыши: нажатие обрабатывает Windows
//! (`WM_NCLBUTTONDOWN` по `HTCAPTION`/`HTMAXBUTTON`), иначе пропали бы Snap и развёртывание двойным щелчком.
//! Зоны перетаскивания — соседи кнопок, а не их родители: gpui спрашивает зону по всем элементам под мышью, и кнопка
//! внутри зоны двигала бы окно вместо щелчка. Зазоры между кнопками — тоже заголовок окна ([`drag_gap`]).

use std::time::{Duration, Instant};

use gpui_kit::component::scroll::{Scrollbar, ScrollbarHandle, ScrollbarMode};
use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::Metric;

use crate::icons::{self, Ico};
use crate::roles::{Look, Pm};
use crate::settings::settings;
use crate::tabs::{MailState, Tab, TabKind};
use crate::{Mailer, Section};

/// Шрифт значков ОС (базис Pets): Segoe Fluent Icons, Windows 11.
pub(crate) const ICON_FONT: &str = "Segoe Fluent Icons";

/// Значки Segoe Fluent Icons.
pub(crate) mod glyph {
    pub(crate) const MAIL: char = '\u{E715}';
    pub(crate) const CALENDAR: char = '\u{E787}';
    pub(crate) const CONTACTS: char = '\u{E77B}';
    pub(crate) const SETTINGS: char = '\u{E713}';
    /// Сертификат: подробности подписи S/MIME (S5-12).
    pub(crate) const CERTIFICATE: char = '\u{EB95}';
    pub(crate) const NEW_MAIL: char = '\u{E70F}';
    pub(crate) const REPLY: char = '\u{E97A}';
    pub(crate) const REPLY_ALL: char = '\u{E8C2}';
    pub(crate) const FORWARD: char = '\u{E89C}';
    pub(crate) const DELETE: char = '\u{E74D}';
    pub(crate) const FLAG: char = '\u{E7C1}';
    pub(crate) const MORE: char = '\u{E712}';
    pub(crate) const SEARCH: char = '\u{E721}';
    pub(crate) const RULES: char = '\u{E71C}';
    pub(crate) const VACATION: char = '\u{E8BD}';
    pub(crate) const ATTACH: char = '\u{E723}';
    pub(crate) const CLOSE: char = '\u{E711}';
    pub(crate) const FOLDER: char = '\u{E8B7}';
    pub(crate) const SEND: char = '\u{E724}';
    pub(crate) const DRAFT: char = '\u{E70B}';
    pub(crate) const ARCHIVE: char = '\u{E7B8}';
    pub(crate) const JUNK: char = '\u{E8D8}';
    pub(crate) const MIN: char = '\u{E921}';
    pub(crate) const MAX: char = '\u{E922}';
    pub(crate) const RESTORE: char = '\u{E923}';
    pub(crate) const CAPTION_CLOSE: char = '\u{E8BB}';
    // Письмо (S5-06б): панель форматирования, метка, категория, «Развернуть».
    pub(crate) const CHEVRON: char = '\u{E70D}';
    pub(crate) const BULLETS: char = '\u{E8FD}';
    pub(crate) const LINK: char = '\u{E71B}';
    pub(crate) const QUOTE: char = '\u{E9B2}';
    pub(crate) const LOCK: char = '\u{E72E}';
    pub(crate) const TAG: char = '\u{E8EC}';
    pub(crate) const CHECK: char = '\u{E73E}';
    pub(crate) const EXPAND: char = '\u{E8A7}';
    pub(crate) const PREVIEW: char = '\u{E7B3}';
    // Фильтры (S5-03).
    pub(crate) const FILTER: char = '\u{E71C}';
    pub(crate) const BACK: char = '\u{E72B}';
    pub(crate) const CHEVRON_DOWN: char = '\u{E70D}';
    pub(crate) const CHEVRON_RIGHT: char = '\u{E76C}';
    pub(crate) const SORT: char = '\u{E8CB}';
    /// Срок и напоминание (S5-04): часы и звонок.
    pub(crate) const CLOCK: char = '\u{E121}';
    pub(crate) const RINGER: char = '\u{EA8F}';
    // Выделение, меню и состояния (S5-08).
    pub(crate) const READ: char = '\u{E8C3}';
    pub(crate) const MOVE: char = '\u{E8DE}';
    pub(crate) const MULTI: char = '\u{E762}';
    pub(crate) const WAIT: char = '\u{E916}';
    pub(crate) const EXPIRE: char = '\u{E823}';
    pub(crate) const INFO: char = '\u{E946}';
    pub(crate) const WARNING: char = '\u{E7BA}';
    pub(crate) const ERROR: char = '\u{EA39}';
    pub(crate) const OFFLINE: char = '\u{F384}';
    /// Беседа (меню папки «Вид беседы», S5-05б).
    pub(crate) const CONVERSATION: char = '\u{E8F2}';
    // Значки команд палитры (S5-14) и переход по совпадениям поиска.
    pub(crate) const UP: char = '\u{E74A}';
    pub(crate) const DOWN: char = '\u{E74B}';
    pub(crate) const CHEVRON_UP: char = '\u{E70E}';
    pub(crate) const CHEVRON_LEFT: char = '\u{E76B}';
    pub(crate) const SELECT_ALL: char = '\u{E8B3}';
    pub(crate) const CLEAR: char = '\u{E894}';
    pub(crate) const PHISHING: char = '\u{E730}';
    pub(crate) const SAVE: char = '\u{E74E}';
    pub(crate) const GLOBE: char = '\u{E774}';
    pub(crate) const HISTORY: char = '\u{E81C}';
    pub(crate) const TODAY: char = '\u{E8D1}';
    pub(crate) const BOLD: char = '\u{E8DD}';
    pub(crate) const ITALIC: char = '\u{E8DB}';
    pub(crate) const UNDERLINE: char = '\u{E8DC}';
    pub(crate) const ADD_CONTACT: char = '\u{E8FA}';
    pub(crate) const EDIT: char = '\u{E70F}';
    pub(crate) const COPY: char = '\u{E8C8}';
    pub(crate) const TENTATIVE: char = '\u{E897}';
    /// «Отправить и получить — все папки» (T3-02) в палитре.
    pub(crate) const SYNC: char = '\u{E895}';
}

/// Ширина кнопки «─ □ ✕» (Windows 11).
const W_CAPTION: f32 = 46.0;
/// Заголовок окна (макет `MainNav_title`): высота; метрика pets-ui `HTitlebar` на 4 px ниже макета.
pub(crate) const TITLEBAR_H: f32 = 44.0;
/// Значок приложения: 12 px слева, 20 px значок, 14 px до разделов.
const W_APP_ICON: f32 = 46.0;
/// Кнопка раздела, зазор между кнопками; от последней до разделителя и от него до вкладок.
const W_SECTION: f32 = 34.0;
const H_SECTION: f32 = 30.0;
const SECTION_GAP: f32 = 2.0;
const SECTIONS_END: f32 = 10.0;
const TABS_START: f32 = 8.0;
/// Высота вкладки в заголовке (макет `MainNav_title`).
const H_TAB: f32 = 34.0;
/// Шестерёнка «Настройки».
const W_SETTINGS: f32 = 40.0;
/// Вкладка: ширина и скругление верха.
const W_TAB_MIN: f32 = 96.0;
const W_TAB_MAX: f32 = 220.0;
/// Строка состояния.
pub(crate) const STATUS_HEIGHT: f32 = 28.0;
/// Меню «…» не открывается снова тем же щелчком, которым его закрыли (нажатие вне меню — по кнопке «…»).
const REOPEN_GUARD: Duration = Duration::from_millis(400);

/// Значок шрифтом ОС.
pub(crate) fn icon(g: char, size: f32) -> Div {
    div().flex_none().font_family(ICON_FONT).text_size(px(size)).line_height(px(size)).child(String::from(g))
}

/// Вертикальная полоса прокрутки: бегунок в 2 px от края области (у gpui-kit — 4 px, полоса уходила на строки).
/// Область полосы — до края панели или карточки; место под бегунок — правый отступ строк.
pub(crate) fn vbar<H: ScrollbarHandle + Clone>(handle: &H, mode: ScrollbarMode) -> Scrollbar {
    Scrollbar::vertical(handle).mode(mode).styles(|s| s.thumb(|t| t.inset(px(2.))))
}

/// Тихая кнопка командной строки: без заливки, наведение — слабая заливка.
pub(crate) fn quiet_button(id: &'static str, l: &Look) -> Stateful<Div> {
    div()
        .id(id)
        .children(crate::screens::spot(id))
        .h(px(32.))
        .px(px(10.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(l.px(Metric::RadiusControl))
        .cursor_pointer()
        .hover(|d| d.bg(l.hover()))
        .active(|d| d.bg(l.pressed()))
}

/// Меню «…» командной строки.
#[derive(Default)]
pub(crate) struct MoreMenu {
    pub(crate) open: bool,
    /// Открыто кнопкой «…» (кнопки, которым не хватило места), а не «Вид».
    pub(crate) overflow: bool,
    closed_at: Option<Instant>,
}

impl MoreMenu {
    /// Щелчок по «Вид» (`overflow = false`) или «…».
    pub(crate) fn toggle(&mut self, overflow: bool) {
        let just_closed = self.closed_at.is_some_and(|t| t.elapsed() < REOPEN_GUARD);
        self.open = !self.open && !just_closed;
        self.overflow = overflow;
        self.closed_at = None;
    }

    pub(crate) fn close(&mut self) -> bool {
        let was = self.open;
        if was {
            self.open = false;
            self.closed_at = Some(Instant::now());
        }
        was
    }
}

/// Раздел окна: кнопка в заголовке.
#[derive(Clone, Copy)]
pub(crate) struct SectionButton {
    pub(crate) id: &'static str,
    pub(crate) icon: Ico,
    tint: Pm,
    key: &'static str,
    /// Команда: подсказка «Название (клавиша)», KeyTips и есть ли раздел у сервера (S7-03).
    pub(crate) command: &'static str,
    section: Section,
    action: fn() -> Box<dyn Action>,
}

/// Разделы окна (DESIGN «Главное окно», «Значки»): значок 20 px, цвет — токен `icon.*`.
pub(crate) const SECTIONS: [SectionButton; 4] = [
    SectionButton {
        id: "nav-mail",
        icon: icons::MAIL,
        tint: Pm::IconBlue,
        key: "frame-section-mail",
        command: "view.mail",
        section: Section::Mail,
        action: || Box::new(view::Mail),
    },
    SectionButton {
        id: "nav-calendar",
        icon: icons::CALENDAR,
        tint: Pm::IconPink,
        key: "frame-section-calendar",
        command: "view.calendar",
        section: Section::Calendar,
        action: || Box::new(view::Calendar),
    },
    SectionButton {
        id: "nav-contacts",
        icon: icons::PEOPLE,
        tint: Pm::IconBlue,
        key: "frame-section-contacts",
        command: "view.contacts",
        section: Section::Contacts,
        action: || Box::new(view::Contacts),
    },
    SectionButton {
        id: "nav-tasks",
        icon: icons::TASKS,
        tint: Pm::IconGreen,
        key: "frame-section-tasks",
        command: "view.tasks",
        section: Section::Tasks,
        action: || Box::new(view::Tasks),
    },
];

/// Свободное место заголовка рядом с кнопками: перетаскивание, двойной щелчок — развернуть.
fn drag_gap(id: impl Into<ElementId>, w: f32) -> Stateful<Div> {
    let id = id.into();
    div().id(id.clone()).children(crate::screens::spot(id)).w(px(w)).h_full().flex_none().window_control_area(WindowControlArea::Drag)
}

impl Mailer {
    /// Название вкладки и есть ли в ней несохранённое (письмо — точка после названия).
    pub(crate) fn tab_title(&self, t: &Tab, cx: &App) -> (String, bool) {
        match t.kind {
            // Главная вкладка — по модулю (макеты: «Задачи», «Календарь», «Контакты»); во вкладке почты раздел главной
            // ждёт в `main_section`.
            TabKind::Main => {
                let section = if self.tabs.active().kind == TabKind::Mail { self.main_section.unwrap_or(self.section) } else { self.section };
                let key = match section {
                    Section::Mail => "frame-tab-mail",
                    Section::Calendar => "frame-section-calendar",
                    Section::Contacts => "frame-section-contacts",
                    Section::Tasks => "frame-section-tasks",
                };
                (tr!(key), false)
            }
            TabKind::Mail => (self.mail_tab_title(t.id, cx), false),
            TabKind::Settings => (tr!("frame-tab-settings"), false),
            TabKind::Meeting => (self.meetings.title(t.id, cx), false),
            TabKind::Compose(id) => {
                let Some(c) = self.compose.get(id) else { return (tr!("compose-title-new"), false) };
                let m = &c.read(cx).model;
                let subject = m.subject.trim();
                (if subject.is_empty() { tr!("compose-title-new") } else { subject.to_owned() }, m.is_dirty())
            }
        }
    }

    /// Заголовок окна: [значок][разделы][вкладки][перетаскивание][настройки][─ □ ✕].
    pub(crate) fn titlebar(&self, l: &Look, window: &Window, cx: &mut Context<Self>) -> Div {
        let active = self.tabs.active().id;
        // «Закрыть вкладку (Ctrl+W)»: подсказка и плашка KeyTips у активной (S5-18).
        let close_tip = crate::keytips::tip("tab.close", cx);
        let close_key = crate::keytips::key("tab.close", cx);
        let tabs = self.tabs.iter().map(|t| {
            let on = t.id == active;
            let id = t.id;
            let close = t.kind.closable().then(|| {
                div()
                    .id(("tab-close", id as usize))
                    .relative()
                    .children(crate::screens::spot(("tab-close", id as usize)))
                    .size(px(20.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(l.px(Metric::RadiusControl))
                    .hover(|d| d.bg(l.hover()))
                    .child(icon(glyph::CLOSE, 10.))
                    .tooltip({
                        let tip = close_tip.clone();
                        move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_tab(id, window, cx);
                    }))
            });
            // «×» (T2-24, макет `Main`): у активной — всегда, у остальных — при наведении на вкладку.
            let group: SharedString = format!("tab-{id}").into();
            let close = close.map(|c| {
                let g = group.clone();
                c.when(!on, move |c| c.invisible().group_hover(g, |s| s.visible()))
            });
            let close = close.map(|c| crate::keytips::badge(c, close_key.clone().filter(|_| on), l, cx));
            let (title, dirty) = self.tab_title(t, cx);
            let full = title.clone();
            let letter = matches!(t.kind, TabKind::Compose(_));
            div()
                .id(("tab", id as usize))
                .group(group)
                .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(full.clone()).build(window, cx))
                .h(px(H_TAB))
                .min_w(px(W_TAB_MIN))
                .max_w(px(W_TAB_MAX))
                .flex()
                .items_center()
                .gap(px(8.))
                .pl(px(14.))
                .pr(px(if close.is_some() { 6. } else { 14. }))
                .rounded_t(l.px(Metric::RadiusOverlay))
                .when(on, |d| d.bg(l.panel()).text_color(l.text()))
                .when(!on, |d| d.text_color(l.text2()).hover(|d| d.bg(l.hover())))
                .when(letter, |d| d.child(icon(glyph::NEW_MAIL, 12.)))
                .when(t.kind == TabKind::Meeting, |d| d.child(icon(crate::calendar::g::CALENDAR, 13.)))
                .when(t.kind == TabKind::Mail, |d| d.child(icon(glyph::MAIL, 12.)))
                .child(div().flex_1().min_w_0().truncate().child(title))
                // Несохранённый черновик — точка (DESIGN «Новое письмо», макет вкладки).
                .when(dirty, |d| {
                    d.child(
                        div()
                            .id(("tab-dirty", id as usize))
                            .size(px(6.))
                            .flex_none()
                            .rounded_full()
                            .bg(l.text2())
                            .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tr!("tab-dirty-tip")).build(window, cx)),
                    )
                })
                .children(close)
                .on_click(cx.listener(move |this, _, window, cx| this.activate_tab(id, window, cx)))
        });
        let caption = |id: &'static str, g: char, area: WindowControlArea, close: bool| {
            div()
                .id(id)
                .children(crate::screens::spot(id))
                .w(px(W_CAPTION))
                .h_full()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .text_color(l.text())
                .when(close, |d| d.hover(|d| d.bg(l.pm(Pm::CaptionClose)).text_color(l.pm(Pm::CaptionCloseOn))))
                .when(!close, |d| d.hover(|d| d.bg(l.hover())))
                .window_control_area(area)
                .child(icon(g, 10.))
        };
        let max = if window.is_maximized() { glyph::RESTORE } else { glyph::MAX };
        div()
            .h(px(TITLEBAR_H))
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .text_size(px(13.))
            .child(
                div()
                    .id("app-icon")
                    .children(crate::screens::spot("app-icon"))
                    .w(px(W_APP_ICON))
                    .h_full()
                    .pl(px(12.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .window_control_area(WindowControlArea::Drag)
                    .child(img(icons::APP).size(px(20.)).flex_none()),
            )
            .children(self.sections(l, cx))
            // Обрезка только по ширине: плашка KeyTips «×» активной вкладки выходит за низ строки.
            .child(div().h_full().flex().items_end().gap(px(2.)).min_w_0().overflow_x_hidden().children(tabs))
            // «+» — новая вкладка почты (T2-24, макет `Main`, Ctrl+T).
            .child({
                let tip = crate::keytips::tip("tab.new", cx);
                div()
                    .id("tab-new")
                    .children(crate::screens::spot("tab-new"))
                    .size(px(28.))
                    .ml(px(4.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(l.px(Metric::RadiusControl))
                    .text_color(l.text2())
                    .hover(|d| d.bg(l.hover()))
                    .child(icon(crate::calendar::g::ADD, 12.))
                    .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx))
                    .on_click(cx.listener(|this, _, window, cx| this.new_mail_tab(window, cx)))
            })
            // Пустое место — заголовок окна: перетаскивание, двойной щелчок — развернуть.
            .child(
                div().id("drag").children(crate::screens::spot("drag")).flex_1().min_w(px(48.)).h_full().window_control_area(WindowControlArea::Drag),
            )
            .children(self.settings_button(l, cx))
            .child(caption("caption-min", glyph::MIN, WindowControlArea::Min, false))
            .child(caption("caption-max", max, WindowControlArea::Max, false))
            .child(caption("caption-close", glyph::CAPTION_CLOSE, WindowControlArea::Close, true))
    }

    /// Первый запуск: на месте почты — добавление учётной записи.
    fn first_run(&self) -> bool {
        self.section == Section::Mail && self.live.signin.is_some()
    }

    /// Разделы в заголовке (DESIGN «Главное окно»): Почта, Календарь, Контакты, Задачи и разделитель за ними. Первый
    /// запуск — без них: учётной записи ещё нет, переключать нечего.
    fn sections(&self, l: &Look, cx: &App) -> Vec<AnyElement> {
        if self.first_run() {
            return Vec::new();
        }
        // «Нет у сервера — нет на экране» (S7-03): раздела, которого у сервера нет, в заголовке нет.
        let features = crate::features::get(cx);
        let mut out = Vec::new();
        for (i, s) in SECTIONS.into_iter().filter(|s| crate::features::available(s.command, features)).enumerate() {
            let on = self.tabs.active().kind.shows_mail() && self.section == s.section;
            if i > 0 {
                out.push(drag_gap(("nav-gap", i), SECTION_GAP).into_any_element());
            }
            let action = s.action;
            let b = div()
                .id(s.id)
                .children(crate::screens::spot(s.id))
                .w(px(W_SECTION))
                .h(px(H_SECTION))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .cursor_pointer()
                // Контур снаружи плашки, размер кнопки тот же: `box-shadow` макета.
                .when(on, |d| {
                    let outline =
                        BoxShadow { color: l.border(), offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(1.), inset: false };
                    d.bg(l.panel()).shadow(vec![outline])
                })
                .when(!on, |d| d.hover(|d| d.bg(l.hover())))
                .child(icons::icon(s.icon, 20., l.pm(s.tint)))
                .on_click(move |_, window, cx| window.dispatch_action(action(), cx));
            out.push(crate::keytips::command(b, s.command, Some(tr!(s.key)), l, cx).into_any_element());
        }
        // Разделитель 1 px: в тёмной теме макет берёт цвет разделителей панелей, в светлых — рамки контролов.
        let line = if l.dark() { l.border() } else { l.stroke() };
        out.push(
            drag_gap("nav-end", SECTIONS_END + 1.)
                .flex()
                .items_center()
                .justify_end()
                .child(div().w(px(1.)).h(px(H_SECTION)).bg(line))
                .into_any_element(),
        );
        out.push(drag_gap("nav-tabs", TABS_START).into_any_element());
        out
    }

    /// Шестерёнка перед кнопками окна; подсказка и KeyTips — как у разделов. Нажата, пока открыта вкладка «Настройки».
    fn settings_button(&self, l: &Look, cx: &App) -> Option<Stateful<Div>> {
        if self.first_run() {
            return None;
        }
        let on = self.tabs.is_active(TabKind::Settings);
        let b = div()
            .id("nav-settings")
            .children(crate::screens::spot("nav-settings"))
            .w(px(W_SETTINGS))
            .h_full()
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|d| d.bg(l.hover()))
            .child(icons::icon(icons::SETTINGS, 20., if on { l.accent_text() } else { l.text2() }))
            .on_click(|_, window, cx| window.dispatch_action(Box::new(settings::Open), cx));
        Some(crate::keytips::command(b, "settings.open", Some(tr!("frame-section-settings")), l, cx))
    }

    /// Заполненность ящика (S5-29, макет «Главное окно: автоответ»): «Ящик: 9,4 из 10 ГБ» и полоска 64 px; больше
    /// 90 % — значок и жирный текст, щелчок — тост «Очистить вложения…». Квоты нет — ничего.
    fn quota_indicator(&self, l: &Look, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let u = self.settings.quota?;
        let percent = u.percent();
        let full = percent > postman_core::settings::WARN_PERCENT;
        let me = self.compose.me().map(|(a, _)| a.email.clone()).unwrap_or_default();
        let tip = tr!("status-quota-tip", account = me, quota = self.settings.quota_text(), percent = percent.min(999));
        let fill = (percent.min(100) as f32) / 100.;
        Some(
            div()
                .id("quota")
                .children(crate::screens::spot("quota"))
                .h(px(22.))
                .px(px(6.))
                .mr(px(-6.))
                .flex()
                .items_center()
                .gap(px(8.))
                .rounded(l.px(Metric::RadiusControl))
                .when(full, |d| d.text_color(l.text()).font_weight(FontWeight::SEMIBOLD).cursor_pointer().hover(|d| d.bg(l.hover())))
                .when(full, |d| d.child(icon(glyph::WARNING, 12.)))
                .child(self.settings.quota_text())
                .child(
                    div()
                        .w(px(64.))
                        .h(px(4.))
                        .rounded(px(2.))
                        .overflow_hidden()
                        .bg(l.c(pets_ui::theme::Role::StrokeDivider))
                        .child(div().h_full().w(relative(fill)).bg(if full { l.text() } else { l.text3() })),
                )
                .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx))
                .when(full, |d| d.on_click(cx.listener(|this, _, _, cx| this.quota_toast(cx)))),
        )
    }

    /// Нет воркера (S8-38): короткая красная отметка, пока его нет; щелчок — проверить снова, нет — что случилось,
    /// почему и как исправить, с кнопкой «Скачать» ([`worker_notice`]).
    fn worker_status(&self, l: &Look, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        if !crate::reading::worker_missing() {
            return None;
        }
        Some(
            div()
                .id("worker")
                .h(px(22.))
                .px(px(6.))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded(l.px(Metric::RadiusControl))
                .cursor_pointer()
                .hover(|d| d.bg(l.hover()))
                .text_color(l.error())
                .child(icon(glyph::ERROR, 12.))
                .child(tr!("status-worker-missing"))
                .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tr!("status-worker-tip")).build(window, cx))
                .on_click(cx.listener(|this, _, _, cx| this.worker_help(cx))),
        )
    }

    /// Щелчок по отметке «нет компонента»: файл уже положили — убрать отметку и разобрать письмо, нет — описание.
    fn worker_help(&mut self, cx: &mut Context<Self>) {
        let task = cx.background_spawn(async { postman_worker::Client::default_exe().exists() });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            this.update(cx, |this, cx| {
                if found {
                    this.reading.update(cx, |r, cx| r.worker_found(cx));
                } else {
                    this.toasts.update(cx, worker_notice);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// «2 аккаунта · 1 общий ящик» (T2-24, макет `Main`): учётные записи профиля и общие ящики дерева; одна учётка
    /// без общих — ничего.
    fn accounts_status(&self, cx: &App) -> Option<String> {
        let accounts = &self.folders.read(cx).accounts;
        let own = accounts.iter().filter(|a| a.is_personal).count().max(1);
        let shared = accounts.iter().filter(|a| !a.is_personal).count();
        match (own, shared) {
            (1, 0) => None,
            (n, 0) => Some(tr!("status-accounts", n = n as i64)),
            (n, s) => Some(format!("{} · {}", tr!("status-accounts", n = n as i64), tr!("status-shared", n = s as i64))),
        }
    }

    /// Строка состояния: элементы, синхронизация, квота, последнее нажатие, песочница, уведомления.
    pub(crate) fn status_bar(&self, l: &Look, cx: &mut Context<Self>) -> Div {
        // Задачи (макет): слева — состояние синхронизации с точкой, затем «N задач из писем».
        let tasks = self.section == Section::Tasks && self.tabs.active().kind == TabKind::Main;
        // Календарь (T2-22, макет): вид, события, ждут ответа, отклонённых скрыто, пояс.
        let calendar = self.section == Section::Calendar && self.tabs.active().kind == TabKind::Main;
        // Контакты (T2-46, макет): «Справочник обновлён 13:50» — после синхронизации.
        let directory = (self.section == Section::Contacts && self.tabs.active().kind == TabKind::Main)
            .then(|| self.contacts.read(cx).directory_status())
            .flatten();
        let status = if tasks {
            self.tasks.read(cx).status_text()
        } else if calendar {
            self.cal.read(cx).status_text()
        } else {
            self.items_status(cx)
        };
        let (unread, open) = {
            let t = self.toasts.read(cx);
            (t.unread(), t.is_history_open())
        };
        // «Отправить и получить» (T3-02): пока идёт — серая точка и «Отправка и получение…» прежде всего.
        // Без связи (S5-08): серая точка и «Нет связи · повтор через N с» вместо состояния синхронизации.
        let sending = self.live.send_receive.busy().then(|| tr!("frame-send-receive-running"));
        let sync = match sending.or_else(|| crate::states::status_text(&self.offline, Instant::now())) {
            Some(text) => div().flex().items_center().gap(px(6.)).child(div().size(px(8.)).rounded_full().bg(l.text3())).child(text),
            // Синхронизация (T2-24, макет `Main`): точка и «Синхронизировано 14:01» — во всех разделах.
            None if !self.live.status.is_empty() => div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(div().size(px(8.)).rounded_full().bg(if self.live.synced { l.success() } else { l.text3() }))
                .child(match &self.live.synced_at {
                    Some(t) if self.live.synced => format!("{} {t}", self.live.status),
                    _ => self.live.status.clone(),
                }),
            None => div().child(self.live.status.clone()),
        };
        div()
            .h(px(STATUS_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(16.))
            .px(px(14.))
            .text_size(l.px(Metric::TypeCaption))
            .text_color(l.text2())
            .child(sync)
            .children(directory)
            .children(self.accounts_status(cx))
            .child(div().flex_1())
            .child(status)
            .when(!self.last_key.is_empty(), |d| d.child(self.last_key.clone()))
            .child(self.sandbox.map(crate::reading::sandbox_label).unwrap_or_default())
            .children(self.worker_status(l, cx))
            .children(self.migration_status(l, cx))
            .children(self.export_status(l, cx))
            .children(self.quota_indicator(l, cx))
            .child(
                // 🔔 — история уведомлений панелью справа (S5-08); нажата, пока панель открыта.
                div()
                    .id("notifications")
                    .children(crate::screens::spot("notifications"))
                    .h(px(22.))
                    .px(px(6.))
                    .mr(px(-6.))
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .rounded(l.px(Metric::RadiusControl))
                    .cursor_pointer()
                    .when(open, |d| d.bg(l.pressed()).text_color(l.text()))
                    .when(!open, |d| d.hover(|d| d.bg(l.hover())))
                    .when(unread > 0 && !open, |d| d.text_color(l.accent_text()))
                    .child(icon(glyph::RINGER, 14.))
                    .child(unread.to_string())
                    .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tr!("status-notifications-hint")).build(window, cx))
                    .on_click(cx.listener(|this, _, _, cx| this.toasts.update(cx, |t, cx| t.toggle_history(cx)))),
            )
    }

    /// Меню «Вид» под командной строкой справа: автоответы, правила и все настройки (вкладка «Настройки», S5-09),
    /// быстрые фильтры. Открытое кнопкой «…» — кнопки, которым не хватило места в строке (T1-27).
    pub(crate) fn more_menu(&self, l: &Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.more.open {
            return None;
        }
        if self.more.overflow {
            return Some(self.overflow_menu(l, cx));
        }
        // Вид → Быстрые фильтры (S5-03): галочка — закладки видны.
        let quick_on = !crate::prefs::get(cx).quick.hidden;
        let item = |id: &'static str, g: char, key: &'static str, action: fn() -> Box<dyn Action>| {
            div()
                .id(id)
                .children(crate::screens::spot(id))
                .h(l.px(Metric::HMenuItem))
                .px(px(10.))
                .flex()
                .items_center()
                .gap(px(10.))
                .rounded(l.px(Metric::RadiusControl))
                .cursor_pointer()
                .hover(|d| d.bg(l.hover()))
                .child(icon(g, 16.).text_color(l.text2()))
                .child(tr!(key))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.more.close();
                    this.bar.update(cx, |b, cx| {
                        b.more_open = false;
                        cx.notify();
                    });
                    window.dispatch_action(action(), cx);
                }))
        };
        let view_caption = div()
            .mt(px(4.))
            .pt(px(6.))
            .px(px(10.))
            .pb(px(2.))
            .border_t_1()
            .border_color(l.border())
            .text_size(l.px(Metric::TypeCaption))
            .text_color(l.text2())
            .child(tr!("filters-menu-view"));
        let quick =
            item("more-quick", if quick_on { glyph::CHECK } else { ' ' }, "filters-menu-quick", || Box::new(crate::filters::view::QuickFilters));
        let top = TITLEBAR_H + crate::bar::BAR_HEIGHT - 6.;
        let menu = div()
            .id("more-menu")
            .absolute()
            .top(px(top))
            .right(px(12.))
            .min_w(px(220.))
            .p(px(4.))
            .flex()
            .flex_col()
            .rounded(l.px(Metric::RadiusOverlay))
            .border_1()
            .border_color(l.c(pets_ui::theme::Role::StrokeCard))
            .bg(l.c(pets_ui::theme::Role::LayerOverlay))
            .text_color(l.text())
            .shadow_lg()
            .child(item("more-vacation", glyph::VACATION, "frame-vacation", || Box::new(settings::Vacation)))
            .child(item("more-rules", glyph::RULES, "frame-rules", || Box::new(settings::Rules)))
            .child(item("more-settings", glyph::SETTINGS, "frame-settings", || Box::new(settings::Open)))
            .child(view_caption)
            .child(quick)
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.close_more(cx);
            }));
        Some(menu.into_any_element())
    }

    pub(crate) fn toggle_more(&mut self, overflow: bool, cx: &mut Context<Self>) {
        self.more.toggle(overflow);
        let open = self.more.open;
        self.bar.update(cx, |b, cx| {
            b.more_open = open;
            b.overflow_open = overflow;
            cx.notify();
        });
        cx.notify();
    }

    /// Меню «…»: скрытые кнопки командной строки по порядку — значок, подпись, клавиша; щелчок — как по кнопке.
    fn overflow_menu(&self, l: &Look, cx: &mut Context<Self>) -> AnyElement {
        let (hidden, x) = {
            let b = self.bar.read(cx);
            (b.hidden.clone(), b.overflow_x)
        };
        let items = hidden.into_iter().map(|b| {
            let key = if b.keyed { crate::keytips::key(b.cmd, cx) } else { None };
            let id = ElementId::Name(format!("overflow-{}", b.id).into());
            div()
                .id(id.clone())
                .children(crate::screens::spot(id))
                .h(l.px(Metric::HMenuItem))
                .px(px(10.))
                .flex()
                .items_center()
                .gap(px(10.))
                .rounded(l.px(Metric::RadiusControl))
                .cursor_pointer()
                .hover(|d| d.bg(l.hover()))
                .child(icons::icon(b.ico, 16., b.tint.map_or(l.text(), |t| l.pm(t))))
                .child(div().flex_1().child(tr!(b.id)))
                .children(key.map(|k| div().pl(px(16.)).text_color(l.text2()).child(k)))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.close_more(cx);
                    match b.run {
                        crate::bar::Run::Action(action) => window.dispatch_action(action(), cx),
                        crate::bar::Run::Category => this.category_key(cx),
                    }
                }))
        });
        div()
            .id("overflow-menu")
            .absolute()
            .top(px(TITLEBAR_H + crate::bar::BAR_HEIGHT - 6.))
            .left(px(x))
            .min_w(px(200.))
            .p(px(4.))
            .flex()
            .flex_col()
            .rounded(l.px(Metric::RadiusOverlay))
            .border_1()
            .border_color(l.c(pets_ui::theme::Role::StrokeCard))
            .bg(l.c(pets_ui::theme::Role::LayerOverlay))
            .text_color(l.text())
            .shadow_lg()
            .children(items)
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.close_more(cx);
            }))
            .into_any_element()
    }

    /// Закрыть меню «Вид» или «…»; `false` — не было открыто (Esc идёт дальше).
    pub(crate) fn close_more(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.more.close() {
            return false;
        }
        self.bar.update(cx, |b, cx| {
            b.more_open = false;
            cx.notify();
        });
        cx.notify();
        true
    }

    pub(crate) fn activate_tab(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.activate(id) {
            self.sync_mail_tab(cx);
            self.focus_active_tab(window, cx);
            cx.notify();
        }
    }

    /// Фокус — в активную вкладку: письмо — в его текст, иначе — экран (клавиши писем, вкладок).
    pub(crate) fn focus_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.tabs.active().kind {
            TabKind::Compose(id) => {
                if let Some(c) = self.compose.get(id).cloned() {
                    c.update(cx, |c, cx| c.focus_body(window, cx));
                }
            }
            _ => window.focus(&self.focus, cx),
        }
    }

    /// Закрыть вкладку (× или Ctrl+W): вкладка настроек закрывает панель, письмо — с вопросом о черновике.
    pub(crate) fn close_tab(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.iter().find(|t| t.id == id).copied() else { return };
        match tab.kind {
            TabKind::Main => {}
            TabKind::Mail => {
                self.tabs.close(id);
                self.sync_mail_tab(cx);
                self.focus_active_tab(window, cx);
            }
            TabKind::Settings => {
                self.close_settings(window, cx);
            }
            TabKind::Compose(letter) => self.close_compose_tab(letter, window, cx),
            TabKind::Meeting => self.close_meeting(id, window, cx),
        }
        cx.notify();
    }

    /// Ctrl+Tab / Ctrl+Shift+Tab.
    pub(crate) fn step_tab(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.step(delta);
        self.sync_mail_tab(cx);
        self.focus_active_tab(window, cx);
        cx.notify();
    }

    // --- вкладки почты (S5-16) ---

    /// Ctrl+T: ещё вкладка почты — на текущей папке, фильтрах и письме; дальше у неё своё состояние. Поиск в новую
    /// вкладку не переходит.
    pub(crate) fn new_mail_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.mail_state(cx);
        self.tabs.open_mail(state);
        self.sync_mail_tab(cx);
        if self.find.active() {
            self.search.update(cx, |s, cx| s.set_value("", window, cx));
            self.end_search(cx);
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Показать почту: активная вкладка уже с почтой — она; иначе — вкладка, чья почта в частях экрана, или главная.
    pub(crate) fn show_mail_tab(&mut self, cx: &mut Context<Self>) {
        if !self.tabs.active().kind.shows_mail() {
            let back = self.tabs.owner().is_some_and(|id| self.tabs.activate(id));
            if !back {
                self.tabs.activate_kind(TabKind::Main);
            }
        }
        self.sync_mail_tab(cx);
        cx.notify();
    }

    /// Состояние почты в частях экрана: папка, фильтры, панель слева, выбранное (или открытое) письмо.
    pub(crate) fn mail_state(&self, cx: &App) -> MailState {
        let list = self.list.read(cx);
        let f = self.filters.read(cx);
        let open = || self.reading.read(cx).reading.as_ref().map(|r| r.row.id.clone());
        let letter = list.selected_row().map(|r| r.id).or_else(open);
        MailState { mailbox: list.mailbox.clone(), query: f.query.clone(), panel: f.panel, letter }
    }

    /// Активная вкладка сменилась: во вкладке почты раздел — «Почта» (раздел главной запоминается до возврата в неё),
    /// части почты — с состоянием активной вкладки. Та же вкладка — ничего.
    pub(crate) fn sync_mail_tab(&mut self, cx: &mut Context<Self>) {
        match self.tabs.active().kind {
            TabKind::Mail if self.section != Section::Mail => {
                self.main_section = Some(self.section);
                self.section = Section::Mail;
            }
            TabKind::Main => {
                if let Some(s) = self.main_section.take() {
                    self.section = s;
                }
            }
            _ => {}
        }
        // Кадр за кадром здесь ничего не меняется: та же вкладка или не почта — без перерисовки.
        let active = self.tabs.active();
        if !active.kind.shows_mail() || self.tabs.owner() == Some(active.id) {
            return;
        }
        let current = self.mail_state(cx);
        if let Some(state) = self.tabs.hand_over(|| current) {
            self.restore_mail(state, cx);
        }
        cx.notify();
    }

    /// Поставить состояние вкладки почты в части экрана — теми же путями, что щелчок по папке и фильтрам.
    fn restore_mail(&mut self, state: MailState, cx: &mut Context<Self>) {
        if self.find.active() {
            self.end_search(cx);
            // Поле поиска — после кадра: ему нужно окно.
            let (search, window) = (self.search.clone(), self.window);
            cx.defer(move |cx| {
                let _ = window.update(cx, |_, window, cx| search.update(cx, |s, cx| s.set_value("", window, cx)));
            });
        }
        if let Some(mailbox) = state.mailbox {
            self.open_mailbox(mailbox, cx);
        }
        self.filters.update(cx, |m, cx| {
            m.set(cx, |m| m.panel = state.panel);
            m.change(cx, |q| *q = state.query);
        });
        let Some(id) = state.letter else { return };
        self.list.update(cx, |l, cx| l.reselect_id(id.clone(), cx));
        if let Some(row) = self.cache.rows_by_ids(std::slice::from_ref(&id)).ok().and_then(|mut r| r.pop()) {
            self.open_letter(row, cx);
        }
    }

    /// Письмо — в новой вкладке почты (папка письма, письмо выбрано); вкладка, откуда открыли, остаётся как была
    /// (S6-05: «Открыть письмо» из задачи).
    pub(crate) fn open_letter_tab(&mut self, row: postman_cache::EmailRow, cx: &mut Context<Self>) {
        let current = self.mail_state(cx);
        let state = MailState { mailbox: row.mailbox_ids.first().cloned(), letter: Some(row.id.clone()), ..Default::default() };
        if self.tabs.active().kind == TabKind::Main && self.section != Section::Mail {
            self.main_section = Some(self.section);
            self.section = Section::Mail;
        }
        self.tabs.open_mail(current);
        self.restore_mail(state, cx);
        cx.notify();
    }

    /// Вкладки окна сейчас (T2-11): раздел и папка главной, остальные по порядку. Новая встреча и письмо без
    /// сохранённой версии не сохраняются — открыть их снова не из чего.
    pub(crate) fn session(&self, cx: &App) -> postman_core::prefs::Session {
        use postman_core::prefs::{SavedTab, Session};
        let list = self.list.read(cx);
        let mailbox =
            |id: u64| if self.tabs.owner() == Some(id) { list.mailbox.clone() } else { self.tabs.saved(id).and_then(|s| s.mailbox.clone()) };
        let section = self.main_section.unwrap_or(self.section);
        let main = self.tabs.iter().find(|t| t.kind == TabKind::Main).map_or(0, |t| t.id);
        let mut out = Session { section: (section != Section::Mail).then(|| section.key().to_owned()), mailbox: mailbox(main), ..Default::default() };
        let active = self.tabs.active().id;
        for t in self.tabs.iter() {
            let saved = match t.kind {
                TabKind::Main | TabKind::Meeting => continue,
                TabKind::Mail => SavedTab::Mail { mailbox: mailbox(t.id) },
                TabKind::Settings => SavedTab::Settings,
                TabKind::Compose(id) => match self.compose.get(id).and_then(|c| c.read(cx).model.server_id().map(str::to_owned)) {
                    Some(id) => SavedTab::Draft { id },
                    None => continue,
                },
            };
            out.tabs.push(saved);
            if t.id == active {
                out.active = out.tabs.len();
            }
        }
        out
    }

    /// Вкладки сменились — в настройки (T2-11), если при запуске открывать последние; запись — в фоне.
    pub(crate) fn keep_session(&mut self, cx: &mut Context<Self>) {
        if !self.session_ready {
            return;
        }
        let now = self.session(cx);
        if self.session_saved.as_ref() == Some(&now) {
            return;
        }
        self.session_saved = Some(now.clone());
        cx.defer(move |cx| {
            if crate::prefs::get(cx).general.start == postman_core::prefs::StartView::LastTabs {
                crate::prefs::update(cx, |p| p.session = now);
            }
        });
    }

    /// Запуск (T2-11): «Открывать при запуске: Последняя вкладка» — вкладки прошлого запуска. Папки, которой больше
    /// нет, и черновика, которого нет в кэше, — нет; черновики открываются последними (окну письма нужен кадр).
    pub(crate) fn restore_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use postman_core::prefs::SavedTab;
        self.session_ready = true;
        let prefs = crate::prefs::get(cx);
        if prefs.general.start != postman_core::prefs::StartView::LastTabs {
            return;
        }
        let s = prefs.session;
        let boxes: Vec<String> = self.folders.read(cx).mailboxes.iter().map(|b| b.id.clone()).collect();
        let known = |m: &Option<String>| m.as_ref().is_none_or(|m| boxes.contains(m));
        if let Some(m) = s.mailbox.clone().filter(|_| known(&s.mailbox)) {
            self.open_mailbox(m, cx);
        }
        if let Some(section) = s.section.as_deref().and_then(Section::from_key) {
            self.set_section(section, cx);
        }
        // Номер сохранённой вкладки → вкладка сейчас; черновики — потом.
        let mut ids = vec![self.tabs.active().id];
        let mut drafts = Vec::new();
        for (i, t) in s.tabs.iter().enumerate() {
            match t {
                SavedTab::Mail { mailbox } => {
                    let mailbox = mailbox.clone().filter(|_| known(mailbox));
                    ids.push(self.tabs.restore_mail(MailState { mailbox, ..Default::default() }));
                }
                SavedTab::Settings => {
                    let back = self.tabs.active().id;
                    self.open_settings(None, window, cx);
                    ids.push(self.tabs.iter().find(|t| t.kind == TabKind::Settings).map_or(0, |t| t.id));
                    self.tabs.activate(back);
                }
                SavedTab::Draft { id } => {
                    ids.push(0);
                    drafts.push((i + 1, id.clone()));
                }
            }
        }
        if let Some(&id) = ids.get(s.active).filter(|_| !drafts.iter().any(|(i, _)| *i == s.active)) {
            self.activate_tab(id, window, cx);
        }
        // Активный черновик — последним: его вкладка станет активной.
        drafts.sort_by_key(|(i, _)| *i == s.active);
        for (i, key) in drafts {
            let Some(row) = self.cache.rows_by_ids(std::slice::from_ref(&key)).ok().and_then(|mut r| r.pop()) else { continue };
            let Some(body) = self.cache.body(&key).ok().flatten() else { continue };
            let draft = crate::send_later::draft_of(self, row, body);
            let how = crate::compose::OpenAs { server_id: Some(key), behind: i != s.active, ..Default::default() };
            self.open_draft(draft, how, cx);
        }
        cx.notify();
    }

    /// Название вкладки почты: открытая папка или «Все папки».
    fn mail_tab_title(&self, id: u64, cx: &App) -> String {
        let list = self.list.read(cx);
        let (mailbox, all) = match self.tabs.saved(id) {
            Some(s) if self.tabs.owner() != Some(id) => (s.mailbox.clone(), s.query.scope == crate::filters::Where::All),
            _ => (list.mailbox.clone(), self.filters.read(cx).query.scope == crate::filters::Where::All),
        };
        if all {
            return tr!("filters-all-folders");
        }
        mailbox.and_then(|m| list.names.get(&m).cloned()).unwrap_or_else(|| tr!("frame-tab-mail"))
    }
}

/// Опись части (ADR 0005, `parts.rs`).
pub(crate) const PART: crate::part::Part =
    crate::part::Part { name: "frame", texts: TEXTS, commands: COMMANDS, handlers, scenes: scenes::SCENES, reset: Some(scenes::reset) };

/// Обработчики команд части — на корне экрана почты (`Mailer::render`).
fn handlers(root: Div, cx: &mut Context<crate::Mailer>) -> Div {
    root.on_action(cx.listener(|this, _: &crate::features::Unavailable, _, cx| this.unavailable(cx)))
        .on_action(cx.listener(|this, _: &view::Mail, _, cx| this.set_section(crate::Section::Mail, cx)))
        .on_action(cx.listener(|this, _: &view::Calendar, _, cx| this.set_section(crate::Section::Calendar, cx)))
        .on_action(cx.listener(|this, _: &view::Contacts, _, cx| this.set_section(crate::Section::Contacts, cx)))
        .on_action(cx.listener(|this, _: &view::Tasks, _, cx| this.set_section(crate::Section::Tasks, cx)))
        .on_action(cx.listener(|this, _: &tab::New, window, cx| this.new_mail_tab(window, cx)))
        .on_action(cx.listener(|this, _: &tab::Next, window, cx| this.step_tab(1, window, cx)))
        .on_action(cx.listener(|this, _: &tab::Prev, window, cx| this.step_tab(-1, window, cx)))
        .on_action(cx.listener(|this, _: &tab::Close, window, cx| {
            let id = this.tabs.active().id;
            this.close_tab(id, window, cx);
        }))
        .on_action(cx.listener(|this, _: &view::Notifications, _, cx| {
            this.toasts.update(cx, |t, cx| t.toggle_history(cx));
        }))
        .on_action(cx.listener(|this, _: &sync::SendReceive, _, cx| this.send_receive(cx)))
}

/// Тексты части (`i18n/<язык>/<часть>.ftl`).
const TEXTS: &[pets_i18n::Resource] = &[
    ("ru", include_str!("../i18n/ru/app.ftl")),
    ("ru", include_str!("../i18n/ru/commands.ftl")),
    ("ru", include_str!("../i18n/ru/frame.ftl")),
    ("ru", include_str!("../i18n/ru/sync.ftl")),
    ("en", include_str!("../i18n/en/app.ftl")),
    ("en", include_str!("../i18n/en/commands.ftl")),
    ("en", include_str!("../i18n/en/frame.ftl")),
    ("en", include_str!("../i18n/en/sync.ftl")),
];

/// Разделы окна (заголовок, Ctrl+1…4) и история уведомлений.
pub(crate) mod view {
    gpui_kit::actions!(view, [Mail, Calendar, Contacts, Tasks, Notifications]);
}

/// Вкладки в заголовке окна (S5-01): следующая, предыдущая, закрыть; новая вкладка почты (S5-16).
pub(crate) mod tab {
    gpui_kit::actions!(tab, [New, Next, Prev, Close]);
}

/// «Отправить и получить — все папки» (T3-02): сверка всех учётных записей сейчас.
pub(crate) mod sync {
    gpui_kit::actions!(sync, [SendReceive]);
}

/// Команды части (`commands.rs`: контексты и `cmd`).
const COMMANDS: &[pets_commands::Command] = {
    use crate::commands::{MAILER, cmd};
    &[
        cmd("view.mail", &["ctrl-1"], None),
        cmd("view.calendar", &["ctrl-2"], None),
        cmd("view.contacts", &["ctrl-3"], None),
        cmd("view.tasks", &["ctrl-4"], None),
        // Вкладки окна (S5-01): как в браузере и новом Outlook; «Почта» не закрывается. Ctrl+T — ещё вкладка почты на
        // текущей папке и фильтрах (S5-16); Ctrl+Shift+Tab — исключение правила, пара к Ctrl+Tab.
        cmd("tab.new", &["ctrl-t"], MAILER),
        cmd("tab.next", &["ctrl-tab"], MAILER),
        cmd("tab.prev", &["ctrl-shift-tab"], MAILER),
        cmd("tab.close", &["ctrl-w"], MAILER),
        // Не Outlook: история тостов (значок 🔔 в строке состояния).
        cmd("view.notifications", &[], None),
        // Как в Outlook: F9 — «Отправить и получить — все папки» (T3-02), во всём окне, в полях ввода тоже (там F9 —
        // ничего).
        cmd("sync.send_receive", &["f9"], MAILER),
    ]
};

/// Сцены `--screens` вкладок окна (ADR 0005).
pub(crate) mod scenes {
    use gpui_kit::*;

    use crate::screens::{self, Scene};
    use crate::tabs::TabKind;
    use crate::{Mailer, Section};

    pub(crate) const SCENES: &[Scene] = &[
        // Две вкладки почты (S5-16, Ctrl+T): главная на «Входящих», вторая — «Все папки» с «Непрочитанными», активна.
        Scene::new("tab-mail", tab_mail, tab_mail_ready).after("quick"),
    ];

    /// Вкладка почты сцены `tab-mail` — только в ней: дальше — главная со своим состоянием (как Ctrl+W).
    pub(crate) fn reset(s: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) {
        let mail_tab = m.tabs.iter().find(|t| t.kind == TabKind::Mail).map(|t| t.id);
        if let Some(id) = mail_tab.filter(|_| s.name != "tab-mail") {
            m.close_tab(id, window, cx);
        }
    }

    fn tab_mail(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        use postman_core::filters::Check;
        m.set_section(Section::Mail, cx);
        m.reset_filters(cx);
        m.filters.update(cx, |f, cx| f.set(cx, |f| f.quick_open = false));
        let inbox = crate::live::inbox(&m.folders.read(cx).mailboxes);
        let Some(id) = inbox else { return Err("no inbox".into()) };
        m.open_mailbox(id.clone(), cx);
        // Как Ctrl+T: вторая вкладка на той же папке; в ней — «Все папки» и «Непрочитанные».
        m.new_mail_tab(window, cx);
        m.open_mailbox(id, cx);
        m.filters.update(cx, |f, cx| {
            f.change(cx, |q| {
                q.scope = crate::filters::Where::All;
                q.criteria.toggle_check(Check::Unread);
            })
        });
        Ok(())
    }

    fn tab_mail_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        let list = m.list.read(cx);
        m.tabs.is_active(TabKind::Mail)
            && m.tabs.iter().filter(|t| t.kind == TabKind::Mail).count() == 1
            && m.section == Section::Mail
            && list.query.scope == crate::filters::Where::All
            && !list.query.criteria.is_empty()
            && list.view.is_some()
            && !list.computing
            && screens::rows_loaded(list)
    }
}

/// Нет воркера: что случилось, почему и как исправить — тостом с кнопкой «Скачать» (файл выпуска в браузере).
pub(crate) fn worker_notice(t: &mut crate::notices::Notices, cx: &mut Context<crate::notices::Notices>) {
    let (file, dir) = (crate::reading::worker_file(), crate::reading::worker_dir());
    let url = crate::diagnostics::component_download(&file);
    let download = pets_ui::ToastAction::new(tr!("worker-download"), move |_, _| {
        if let Err(e) = crate::sys::shell::open(&url) {
            tracing::warn!("worker download page: {e}");
        }
    });
    t.push_action(pets_ui::Level::Error, tr!("worker-missing", file = file, dir = dir), download, cx);
}

#[cfg(test)]
mod tests {
    use super::MoreMenu;

    /// Щелчок по «…» при открытом меню: нажатие вне меню закрыло его — тот же щелчок не открывает снова.
    #[test]
    fn more_menu_closes_on_its_own_button() {
        let mut m = MoreMenu::default();
        m.toggle(false);
        assert!(m.open);
        assert!(m.close());
        m.toggle(false);
        assert!(!m.open, "тот же щелчок");
        m.toggle(true);
        assert!(m.open && m.overflow);
    }
}
