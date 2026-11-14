//! Командная строка вместо ленты (S5-01, DESIGN «Главное окно», макеты `MainNav_title`, `MainIcoFluent`, T1-27): одна
//! акцентная кнопка «Новое письмо», действия с письмом — тихими кнопками (Удалить, Архив, Переместить, Категория, Срок,
//! Отложить | Ответить, Всем, Переслать), справа поиск и «Вид» (автоответы, правила, настройки, быстрые фильтры). Кнопки,
//! которым не хватило места, уходят в «…» перед поиском. Поле поиска в фокусе или с запросом — шире, с охватом
//! «Везде | Текущая папка» внутри (S5-10, макет «Поиск»). Кнопка — та же команда реестра, что и хоткей (S3-02).
//! Перед поиском — «Отправить и получить — все папки» значком (T3-02, F9).

use gpui_kit::component::input::{Input, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::Metric;

use crate::actions::mail;
use crate::chrome::{self, glyph};
use crate::icons::{self, Ico};
use crate::keytips;
use crate::roles::{Look, Pm, look};

/// Действие кнопки (новое на каждое нажатие).
type MakeAction = fn() -> Box<dyn Action>;

/// Что делает кнопка.
#[derive(Clone, Copy)]
pub(crate) enum Run {
    /// Действие в фокус окна (экран почты), как от клавиши.
    Action(MakeAction),
    /// Меню письма с открытым подменю «Категория»: у `mail.categorize` клавиши — цифры категорий, одного действия нет.
    Category,
}

/// Тихая кнопка командной строки.
#[derive(Clone, Copy)]
pub(crate) struct Button {
    /// Id элемента и ключ названия в подсказке («Ответить всем»).
    pub(crate) id: &'static str,
    /// Ключ подписи на кнопке («Всем»).
    pub(crate) label: &'static str,
    /// Значок Fluent и его токен (без токена — цветом текста, DESIGN «Значки»).
    pub(crate) ico: Ico,
    pub(crate) tint: Option<Pm>,
    /// Команда реестра: есть ли она у сервера (S7-03), клавиша в подсказке и KeyTips (S5-18).
    pub(crate) cmd: &'static str,
    /// Клавиша команды — в подсказке и плашке (у категорий клавиш девять — без неё).
    pub(crate) keyed: bool,
    pub(crate) run: Run,
}

const fn button(id: &'static str, label: &'static str, ico: Ico, tint: Option<Pm>, cmd: &'static str, run: MakeAction) -> Button {
    Button { id, label, ico, tint, cmd, keyed: true, run: Run::Action(run) }
}

/// Кнопки по макету: до разделителя — что сделать с письмом, после — ответ.
pub(crate) const BUTTONS: [Button; 9] = [
    button("frame-delete", "frame-delete", icons::DELETE, None, "mail.delete", || Box::new(mail::Delete)),
    button("frame-archive", "frame-archive", icons::ARCHIVE, Some(Pm::IconGreen), "mail.archive", || Box::new(mail::Archive)),
    button("frame-move", "frame-move", icons::MOVE, None, "mail.move", || Box::new(mail::Move)),
    Button {
        id: "frame-category",
        label: "frame-category",
        ico: icons::TAG,
        tint: Some(Pm::IconPink),
        cmd: "mail.categorize",
        keyed: false,
        run: Run::Category,
    },
    button("frame-due", "frame-due", icons::FLAG, Some(Pm::IconAmber), "mail.due", || Box::new(mail::Due)),
    button("frame-snooze", "frame-snooze", icons::CLOCK, None, "mail.remind", || Box::new(mail::Remind)),
    button("frame-reply", "frame-reply", icons::REPLY, Some(Pm::IconBlue), "mail.reply", || Box::new(mail::Reply)),
    button("frame-reply-all", "frame-all", icons::REPLY_ALL, Some(Pm::IconBlue), "mail.reply_all", || Box::new(mail::ReplyAll)),
    button("frame-forward", "frame-forward", icons::FORWARD, Some(Pm::IconBlue), "mail.forward", || Box::new(mail::Forward)),
];

/// Сколько первых кнопок — до разделителя.
const FIRST_GROUP: usize = 6;

/// Высота командной строки: кэшированное view не измеряется по содержимому, размер задаётся явно.
pub(crate) const BAR_HEIGHT: f32 = 48.0;
/// Ширина поля поиска (макет главного окна) и во время поиска (макет «Поиск»).
const SEARCH_W: f32 = 220.0;
const SEARCH_W_ACTIVE: f32 = 420.0;
/// Кегль командной строки (макет: 13).
const TEXT: f32 = 13.0;
/// Зазор между элементами строки.
const GAP: f32 = 2.0;
/// Тихая кнопка без подписи: поля 10 + значок 20 + зазор 6 + поля 10.
const BUTTON_PAD: f32 = 46.0;
/// macOS (макет `MainMac`): ответы — значком без подписи, поля 7 + значок 20 + поля 7.
const ICON_ONLY_W: f32 = 34.0;
/// Ответы (вторая группа) — без подписи: только на macOS.
const ANSWERS_ICON_ONLY: bool = cfg!(target_os = "macos");
/// Разделитель групп: 1 px и поля 8 по бокам.
const SEP_W: f32 = 17.0;
/// «…»: поля 10 + значок 16 + поля 10.
const MORE_W: f32 = 36.0;
/// «Вид»: поля 6 + значок 16 + зазор 2 + шеврон 10 + поля 6, отступ 4.
const VIEW_W: f32 = 44.0;
/// «Отправить и получить» (T3-02): поля 8 + значок 20 + поля 8.
const SYNC_W: f32 = 36.0;

/// Где искать: во всех папках или в открытой. По умолчанию — везде (макет «Поиск»).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub(crate) enum Scope {
    #[default]
    All,
    Folder,
}

pub(crate) enum BarEvent {
    /// «Вид» (`false`) или «…» с кнопками, которым не хватило места (`true`): открыть или закрыть меню (рисует экран
    /// почты поверх всего).
    More(bool),
    /// «Категория»: меню письма с открытым подменю категорий.
    Category,
    /// Охват поиска сменился: повторить поиск, если он идёт.
    Scope,
}

impl EventEmitter<BarEvent> for CommandBar {}

pub(crate) struct CommandBar {
    pub(crate) search: Entity<InputState>,
    pub(crate) scope: Scope,
    /// Меню «Вид» или «…» открыто: кнопка нажата.
    pub(crate) more_open: bool,
    /// Открыто меню «…», а не «Вид».
    pub(crate) overflow_open: bool,
    /// Кнопки, которым не хватило места (последняя раскладка), и левый край «…» в окне — под ним меню.
    pub(crate) hidden: Vec<Button>,
    pub(crate) overflow_x: f32,
    /// Поле поиска в фокусе.
    pub(crate) focused: bool,
    /// Открыта папка общего ящика (S5-28): кнопка «Взять в работу» и что она сделает с письмом.
    pub(crate) take: Option<crate::shared::Act>,
    /// Идёт «Отправить и получить» (T3-02): кнопка недоступна, подсказка — «Отправка и получение…».
    pub(crate) syncing: bool,
    /// Ширина слева от строки в окне (macOS, макет `MainMac`: разделы над деревом папок) — для места под кнопки и
    /// положения меню «…». Иначе 0.
    pub(crate) lead: f32,
}

impl CommandBar {
    pub(crate) fn new(search: Entity<InputState>) -> Self {
        Self {
            search,
            scope: Scope::default(),
            more_open: false,
            overflow_open: false,
            hidden: Vec::new(),
            overflow_x: 0.,
            focused: false,
            take: None,
            syncing: false,
            lead: 0.,
        }
    }

    fn scope_switch(&self, l: &Look, cx: &mut Context<Self>) -> impl IntoElement {
        let seg = |id: &'static str, key: &'static str, scope: Scope| {
            let on = self.scope == scope;
            div()
                .id(id)
                .children(crate::screens::spot(id))
                .h_full()
                .px_2()
                .flex()
                .items_center()
                .rounded(px(l.m(Metric::RadiusControl) - 1.))
                .text_size(l.px(Metric::TypeCaption))
                .cursor_pointer()
                .when(on, |d| d.bg(l.panel()).text_color(l.text()).font_weight(FontWeight::SEMIBOLD))
                .when(!on, |d| d.text_color(l.text2()).hover(|d| d.bg(l.hover())))
                .child(tr!(key))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.scope != scope {
                        this.scope = scope;
                        cx.emit(BarEvent::Scope);
                        cx.notify();
                    }
                }))
        };
        div()
            .flex_none()
            .h(px(24.))
            .p(px(2.))
            .flex()
            .gap(px(2.))
            .rounded(l.px(Metric::RadiusControl))
            .bg(l.fill())
            .child(seg("scope-all", "frame-scope-all", Scope::All))
            .child(seg("scope-folder", "frame-scope-folder", Scope::Folder))
    }

    /// Тихая кнопка: значок, подпись, подсказка «Название (клавиша)» и плашка KeyTips.
    /// `icon_only` — без подписи (macOS, ответы): название — в подсказке.
    fn button(&self, b: Button, icon_only: bool, l: &Look, cx: &mut Context<Self>) -> Stateful<Div> {
        let el = chrome::quiet_button(b.id, l)
            .flex_none()
            .when(icon_only, |d| d.px(px(7.)))
            .child(icons::icon(b.ico, 20., b.tint.map_or(l.text(), |t| l.pm(t))))
            .when(!icon_only, |d| d.child(tr!(b.label)))
            .on_click(cx.listener(move |_, _, window, cx| match b.run {
                Run::Action(action) => window.dispatch_action(action(), cx),
                Run::Category => cx.emit(BarEvent::Category),
            }));
        if b.keyed {
            keytips::command(el, b.cmd, Some(tr!(b.id)), l, cx)
        } else {
            let tip = tr!(b.id);
            el.tooltip(move |window, cx| crate::screens::tip(tip.clone(), window, cx))
        }
    }
}

/// Ширина строки текста командной строки (13 px, шрифт окна).
fn text_w(text: &str, window: &Window) -> f32 {
    let text = SharedString::from(text.to_owned());
    let run = window.text_style().to_run(text.len());
    window.text_system().shape_line(text, px(TEXT), &[run], None).width / px(1.)
}

/// Сколько первых кнопок помещается в `room` px: остальные — в «…» (ему тоже нужно место), разделитель — только
/// если видна кнопка второй группы.
fn fitting(widths: &[f32], first: usize, room: f32) -> usize {
    let used = |k: usize| {
        let buttons: f32 = widths[..k].iter().map(|w| w + GAP).sum();
        let sep = if k > first { SEP_W + GAP } else { 0. };
        let more = if k < widths.len() { MORE_W + GAP } else { 0. };
        buttons + sep + more
    };
    (0..=widths.len()).rev().find(|&k| used(k) <= room).unwrap_or(0)
}

impl Render for CommandBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let l = look(cx);
        let new = div()
            .id("bar-new")
            .relative()
            .children(crate::screens::spot("bar-new"))
            .h(px(32.))
            .px(px(14.))
            .mr(px(12.))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .rounded(l.px(Metric::RadiusControl))
            .bg(l.accent())
            .text_color(l.on_accent())
            .cursor_pointer()
            .hover(|d| d.bg(l.accent_hover()))
            .child(icons::icon(icons::COMPOSE, 20., l.on_accent()))
            .child(tr!("frame-new"))
            .on_click(|_, window, cx| window.dispatch_action(Box::new(mail::New), cx));
        // Подсказка «Название (клавиша)» и плашка KeyTips (S5-18); клавиш в подписях нет.
        let new = keytips::command(new, "mail.new", Some(tr!("frame-new")), &l, cx);
        // Общий ящик (S5-28, макет `SharedMailboxes`): тонированная «Взять в работу» после «Новое письмо»; у моего
        // письма — «Вернуть в очередь». Как в макете: у письма коллеги — тоже «Взять в работу» (в шапке письма —
        // «Забрать себе»).
        let take_label = self.take.map(|t| if t == crate::shared::Act::Release { t.label() } else { crate::shared::Act::Take.label() });
        let take = self.take.zip(take_label.clone()).map(|(t, label)| {
            let hint = tr!(if t == crate::shared::Act::Release { "shared-release-hint" } else { "shared-take-hint" });
            let b = crate::buttons::secondary("bar-take", &l)
                .mr(px(8.))
                .child(icons::icon(icons::PEOPLE, 20., l.pm(Pm::SecondaryFg)))
                .child(label)
                .on_click(|_, window, cx| window.dispatch_action(Box::new(mail::Take), cx));
            keytips::command(b, "mail.take", Some(hint), &l, cx)
        });
        // «Нет у сервера — нет на экране» (S7-03).
        let features = crate::features::get(cx);
        let shown: Vec<(usize, Button)> = BUTTONS.iter().copied().enumerate().filter(|(_, b)| crate::features::available(b.cmd, features)).collect();
        let first = shown.iter().filter(|(i, _)| *i < FIRST_GROUP).count();
        // Во время поиска — шире, охват внутри поля. Клавиша поиска — плашкой KeyTips, не в поле.
        let active = self.focused || !self.search.read(cx).value().trim().is_empty();
        let search_w = if active { SEARCH_W_ACTIVE } else { SEARCH_W };
        // Сколько кнопок влезает: окно минус поля, «Новое письмо», «Взять в работу», «Отправить и получить», поиск и «Вид».
        let new_w = 14. + 20. + 8. + text_w(&tr!("frame-new"), window) + 14. + 12. + GAP;
        let take_w = take_label.as_deref().map_or(0., |t| 12. + 20. + 6. + text_w(t, window) + 12. + 8. + GAP);
        let fixed = 12. + new_w + take_w + SYNC_W + GAP + 12. + search_w + GAP + VIEW_W + 12.;
        let room = window.viewport_size().width / px(1.) - self.lead - fixed;
        let icon_only = |i: usize| ANSWERS_ICON_ONLY && i >= FIRST_GROUP;
        let widths: Vec<f32> =
            shown.iter().map(|(i, b)| if icon_only(*i) { ICON_ONLY_W } else { BUTTON_PAD + text_w(&tr!(b.label), window) }).collect();
        let fit = fitting(&widths, first, room);
        self.hidden = shown[fit..].iter().map(|(_, b)| *b).collect();
        let sep = if fit > first && first > 0 { SEP_W + GAP } else { 0. };
        self.overflow_x = self.lead + 12. + new_w + take_w + widths[..fit].iter().map(|w| w + GAP).sum::<f32>() + sep;
        let mut row: Vec<AnyElement> = Vec::new();
        for (k, (i, b)) in shown[..fit].iter().enumerate() {
            if k == first && k > 0 {
                row.push(div().flex_none().w(px(1.)).h(px(20.)).mx(px(8.)).bg(l.stroke()).into_any_element());
            }
            row.push(self.button(*b, icon_only(*i), &l, cx).into_any_element());
        }
        let pressed = |overflow: bool| self.more_open && self.overflow_open == overflow;
        let overflow = (fit < shown.len()).then(|| {
            chrome::quiet_button("bar-overflow", &l)
                .flex_none()
                .when(pressed(true), |d| d.bg(l.pressed()))
                .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tr!("frame-more")).build(window, cx))
                .child(chrome::icon(glyph::MORE, 16.))
                .on_click(cx.listener(|_, _, _, cx| cx.emit(BarEvent::More(true))))
        });
        // «Отправить и получить — все папки» (T3-02, F9): всем учётным записям, не письму — у поиска, отдельно от действий.
        // Пока идёт — недоступна: значок цветом третьего текста, без наведения и щелчка.
        let syncing = self.syncing;
        let sync = if syncing {
            div()
                .id("bar-sync")
                .children(crate::screens::spot("bar-sync"))
                .h(px(32.))
                .flex()
                .items_center()
                .rounded(l.px(Metric::RadiusControl))
                .child(icons::icon(icons::SYNC, 20., l.text3()))
        } else {
            chrome::quiet_button("bar-sync", &l)
                .child(icons::icon(icons::SYNC, 20., l.text()))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(chrome::sync::SendReceive), cx))
        }
        .flex_none()
        .px(px(8.));
        let sync = keytips::command(sync, "sync.send_receive", syncing.then(|| tr!("frame-send-receive-running")), &l, cx);
        let search = Input::new(&self.search).prefix(icons::icon(icons::SEARCH, 18., l.text3()));
        let search = if active { search.suffix(self.scope_switch(&l, cx)) } else { search };
        let search_key = keytips::key("search.focus", cx);
        // «Вид» (макет): значок раскладки и шеврон; меню — автоответы, правила, настройки, быстрые фильтры.
        let view = div()
            .id("bar-more")
            .children(crate::screens::spot("bar-more"))
            .h(px(32.))
            .px(px(6.))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.))
            .rounded(l.px(Metric::RadiusControl))
            .cursor_pointer()
            .text_color(l.text2())
            .hover(|d| d.bg(l.hover()))
            .when(pressed(false), |d| d.bg(l.pressed()))
            .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tr!("frame-view")).build(window, cx))
            .child(icons::icon(icons::VIEW, 16., l.text2()))
            .child(chrome::icon(glyph::CHEVRON_DOWN, 10.))
            .on_click(cx.listener(|_, _, _, cx| cx.emit(BarEvent::More(false))));
        div()
            .flex()
            .size_full()
            .items_center()
            .gap(px(GAP))
            .px(px(12.))
            .text_size(px(TEXT))
            .text_color(l.text())
            .whitespace_nowrap()
            .child(new)
            .children(take)
            .children(row)
            .children(overflow)
            .child(div().flex_1())
            .child(sync)
            .child(keytips::badge(div().ml(px(12.)).w(px(search_w)).flex_none().child(search), search_key, &l, cx))
            .child(div().ml(px(4.)).child(view))
    }
}

#[cfg(test)]
mod tests {
    use super::{BUTTONS, FIRST_GROUP, GAP, MORE_W, SEP_W, fitting};

    /// Всё влезает — «…» нет; не влезает — с конца, с местом под «…»; разделитель — только при второй группе.
    #[test]
    fn buttons_fit_from_the_start() {
        let w = [50.; 4];
        let all = 4. * (50. + GAP) + SEP_W + GAP;
        assert_eq!(fitting(&w, 2, 1000.), 4);
        assert_eq!(fitting(&w, 2, all), 4);
        assert_eq!(fitting(&w, 2, all - 1.), 3);
        assert_eq!(fitting(&w, 2, 2. * (50. + GAP) + MORE_W + GAP), 2);
        assert_eq!(fitting(&w, 2, 0.), 0);
    }

    /// Состав по макету: шесть действий, разделитель, три ответа.
    #[test]
    fn buttons_follow_the_mockup() {
        let cmds: Vec<&str> = BUTTONS.iter().map(|b| b.cmd).collect();
        assert_eq!(
            cmds,
            [
                "mail.delete",
                "mail.archive",
                "mail.move",
                "mail.categorize",
                "mail.due",
                "mail.remind",
                "mail.reply",
                "mail.reply_all",
                "mail.forward"
            ]
        );
        assert_eq!(FIRST_GROUP, 6);
    }
}
