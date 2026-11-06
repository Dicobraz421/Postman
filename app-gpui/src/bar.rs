//! Командная строка вместо ленты (S5-01, DESIGN «Главное окно»): одна акцентная кнопка «Новое письмо», действия
//! с письмом — тихими кнопками, справа поиск и «…» (автоответы, правила). Поле поиска в фокусе или с запросом —
//! шире, с охватом «Везде | Текущая папка» внутри (S5-10, макет «Поиск»). Кнопка — та же команда реестра, что и
//! хоткей (S3-02).

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

/// Тихие кнопки командной строки: значок Fluent и его токен (без токена — цветом текста, DESIGN «Значки»), подпись,
/// команда (подсказка и KeyTips, S5-18), действие.
pub(crate) const BUTTONS: [(Ico, Option<Pm>, &str, &str, MakeAction); 5] = [
    (icons::REPLY, Some(Pm::IconBlue), "frame-reply", "mail.reply", || Box::new(mail::Reply)),
    (icons::REPLY_ALL, Some(Pm::IconBlue), "frame-reply-all", "mail.reply_all", || Box::new(mail::ReplyAll)),
    (icons::FORWARD, Some(Pm::IconBlue), "frame-forward", "mail.forward", || Box::new(mail::Forward)),
    (icons::DELETE, None, "frame-delete", "mail.delete", || Box::new(mail::Delete)),
    (icons::FLAG, Some(Pm::IconAmber), "frame-flag", "mail.toggle_flag", || Box::new(mail::ToggleFlag)),
];

/// Высота командной строки: кэшированное view не измеряется по содержимому, размер задаётся явно.
pub(crate) const BAR_HEIGHT: f32 = 48.0;
/// Ширина поля поиска (макет главного окна) и во время поиска (макет «Поиск»).
const SEARCH_W: f32 = 300.0;
const SEARCH_W_ACTIVE: f32 = 420.0;

/// Где искать: во всех папках или в открытой. По умолчанию — везде (макет «Поиск»).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub(crate) enum Scope {
    #[default]
    All,
    Folder,
}

pub(crate) enum BarEvent {
    /// «…»: открыть или закрыть меню (рисует экран почты поверх всего).
    More,
    /// Охват поиска сменился: повторить поиск, если он идёт.
    Scope,
}

impl EventEmitter<BarEvent> for CommandBar {}

pub(crate) struct CommandBar {
    pub(crate) search: Entity<InputState>,
    pub(crate) scope: Scope,
    /// Меню «…» открыто: кнопка нажата.
    pub(crate) more_open: bool,
    /// Поле поиска в фокусе.
    pub(crate) focused: bool,
    /// Открыта папка общего ящика (S5-28): кнопка «Взять в работу» и что она сделает с письмом.
    pub(crate) take: Option<crate::shared::Act>,
}

impl CommandBar {
    pub(crate) fn new(search: Entity<InputState>) -> Self {
        Self { search, scope: Scope::default(), more_open: false, focused: false, take: None }
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
}

impl Render for CommandBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let l = look(cx);
        let new = div()
            .id("bar-new")
            .relative()
            .children(crate::screens::spot("bar-new"))
            .h(px(32.))
            .px(px(14.))
            .mr(px(12.))
            .flex()
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
        // письма — «Вернуть в очередь».
        let take = self.take.map(|t| {
            let hint = tr!(if t == crate::shared::Act::Release { "shared-release-hint" } else { "shared-take-hint" });
            let b = crate::buttons::secondary("bar-take", &l)
                .mr(px(8.))
                .child(icons::icon(icons::PEOPLE, 20., l.pm(Pm::SecondaryFg)))
                // Как в макете: у письма коллеги — тоже «Взять в работу» (в шапке письма — «Забрать себе»).
                .child(if t == crate::shared::Act::Release { t.label() } else { crate::shared::Act::Take.label() })
                .on_click(|_, window, cx| window.dispatch_action(Box::new(mail::Take), cx));
            keytips::command(b, "mail.take", Some(hint), &l, cx)
        });
        let buttons = BUTTONS.map(|(ico, tint, key, cmd, action)| {
            let b = chrome::quiet_button(key, &l)
                .child(icons::icon(ico, 20., tint.map_or(l.text(), |t| l.pm(t))))
                .child(tr!(key))
                // Действие уходит в фокус окна (экран почты), как от клавиши.
                .on_click(move |_, window, cx| window.dispatch_action(action(), cx));
            keytips::command(b, cmd, Some(tr!(key)), &l, cx)
        });
        // Во время поиска — шире, охват внутри поля. Клавиша поиска — плашкой KeyTips, не в поле.
        let active = self.focused || !self.search.read(cx).value().trim().is_empty();
        let search = Input::new(&self.search).prefix(icons::icon(icons::SEARCH, 18., l.text3()));
        let search = if active { search.suffix(self.scope_switch(&l, cx)) } else { search };
        let search_key = keytips::key("search.focus", cx);
        let more = chrome::quiet_button("bar-more", &l)
            .when(self.more_open, |d| d.bg(l.pressed()))
            .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tr!("frame-more")).build(window, cx))
            .child(chrome::icon(glyph::MORE, 16.))
            .on_click(cx.listener(|_, _, _, cx| cx.emit(BarEvent::More)));
        div()
            .flex()
            .size_full()
            .items_center()
            .gap(px(2.))
            .px(px(12.))
            .text_color(l.text())
            .child(new)
            .children(take)
            .children(buttons)
            .child(div().flex_1())
            .child(keytips::badge(
                div().ml(px(12.)).w(px(if active { SEARCH_W_ACTIVE } else { SEARCH_W })).flex_none().child(search),
                search_key,
                &l,
                cx,
            ))
            .child(div().ml(px(4.)).child(more))
    }
}
