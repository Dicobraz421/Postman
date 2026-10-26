//! Уведомления (S5-08, DESIGN «Действия и состояния», макет «Состояния»): стопка тостов справа сверху — видно не
//! больше трёх, остальные — кнопкой «+N ещё»; предупреждения нейтральные, красный — только ошибки; у тоста — кнопки
//! действий, «N с» до скрытия и полоса времени. История — панель справа (🔔 в строке состояния): фильтр «Все /
//! Ошибки / Действия», группы по дням, действие можно выполнить и оттуда, пока оно не выполнено.
//!
//! Обёртка Postman вместо `pets_ui::Toasts` (тот держит три тоста, четвёртый вытесняет первый, и без «+N»): API тот
//! же (`push`, `push_action`, `toggle_history`, `unread`), типы важности и действия — из `pets_ui`. Перенести в
//! `pets-ui` — очередь [`Stack`] (видно ≤ 3 + «+N», действие в истории до выполнения, время до скрытия) и
//! панель истории с фильтром.
//!
//! Таймер — один на стопку и только пока тосты на экране: раз в секунду «N с» и полоса; в простое кадров нет.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::{Metric, Role};
use pets_ui::{ACTION_DURATION, Level, ToastAction};

use crate::chrome::{glyph, icon};
use crate::roles::Look;

/// Тостов на экране — не больше; остальные — «+N ещё».
pub(crate) const MAX_SHOWN: usize = 3;
/// Записей в истории — не больше (старые уходят).
pub(crate) const MAX_HISTORY: usize = 200;
const WIDTH: f32 = 340.;
const PANEL_W: f32 = 360.;

/// Уведомление: на экране, пока не истекло время, не закрыли и не нажали действие; в истории — всегда.
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub(crate) id: u64,
    pub(crate) level: Level,
    pub(crate) text: SharedString,
    pub(crate) at: Instant,
    /// Местное время появления — «14:06» и день в истории.
    pub(crate) clock: jiff::Zoned,
    pub(crate) action: Option<ToastAction>,
    /// Действие выполнено («выполнено» в истории).
    pub(crate) done: bool,
    /// На экране (тостом).
    pub(crate) live: bool,
    pub(crate) until: Instant,
}

impl Entry {
    fn duration(&self) -> Duration {
        self.until.saturating_duration_since(self.at)
    }

    /// Секунд до скрытия (не меньше 1, пока тост на экране).
    pub(crate) fn left(&self, now: Instant) -> u64 {
        self.until.saturating_duration_since(now).as_secs_f32().ceil() as u64
    }

    /// Доля оставшегося времени (полоса внизу тоста).
    pub(crate) fn share(&self, now: Instant) -> f32 {
        let total = self.duration().as_secs_f32();
        if total <= 0. { 0. } else { (self.until.saturating_duration_since(now).as_secs_f32() / total).clamp(0., 1.) }
    }

    /// Действие ещё можно выполнить.
    pub(crate) fn pending(&self) -> bool {
        self.action.is_some() && !self.done
    }
}

/// Очередь уведомлений: история (новые первыми) и что из неё на экране. Без окна и таймеров — тесты.
#[derive(Debug, Default)]
pub(crate) struct Stack {
    entries: VecDeque<Entry>,
    unread: usize,
    next: u64,
}

impl Stack {
    /// Новое уведомление (`toast` — ещё и тостом на экране) → id.
    pub(crate) fn push(&mut self, level: Level, text: SharedString, action: Option<ToastAction>, toast: bool, now: Instant) -> u64 {
        self.next += 1;
        let duration = if action.is_some() { ACTION_DURATION } else { level.duration() };
        self.entries.push_front(Entry {
            id: self.next,
            level,
            text,
            at: now,
            clock: jiff::Zoned::now(),
            action,
            done: false,
            live: toast,
            until: now + duration,
        });
        self.entries.truncate(MAX_HISTORY);
        self.unread = (self.unread + 1).min(MAX_HISTORY);
        self.next
    }

    /// На экране сейчас (новые первыми), без учёта предела [`MAX_SHOWN`].
    fn live(&self, now: Instant) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(move |e| e.live && e.until > now)
    }

    /// Тосты на экране: не больше [`MAX_SHOWN`], новые сверху.
    pub(crate) fn shown(&self, now: Instant) -> Vec<&Entry> {
        self.live(now).take(MAX_SHOWN).collect()
    }

    /// Сколько ещё на экране сверх видимых («+N ещё»).
    pub(crate) fn more(&self, now: Instant) -> usize {
        self.live(now).count().saturating_sub(MAX_SHOWN)
    }

    /// Время вышло — тосты уходят с экрана. `true` — что-то ушло.
    pub(crate) fn expire(&mut self, now: Instant) -> bool {
        let mut changed = false;
        for e in self.entries.iter_mut().filter(|e| e.live && e.until <= now) {
            e.live = false;
            changed = true;
        }
        changed
    }

    /// Закрыть тост (в истории остаётся). `false` — его уже нет на экране.
    pub(crate) fn hide(&mut self, id: u64) -> bool {
        match self.entries.iter_mut().find(|e| e.id == id && e.live) {
            Some(e) => {
                e.live = false;
                true
            }
            None => false,
        }
    }

    /// Нажато действие (в тосте или в истории): отдаётся один раз, запись — «выполнено», тост уходит.
    pub(crate) fn take_action(&mut self, id: u64) -> Option<ToastAction> {
        let e = self.entries.iter_mut().find(|e| e.id == id)?;
        e.live = false;
        if e.done {
            return None;
        }
        let a = e.action.clone()?;
        e.done = true;
        Some(a)
    }

    /// История, новые первыми.
    pub(crate) fn history(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter()
    }

    pub(crate) fn unread(&self) -> usize {
        self.unread
    }

    pub(crate) fn mark_read(&mut self) {
        self.unread = 0;
    }

    /// Когда истечёт ближайший тост на экране (для таймера); `None` — экран пуст.
    fn next_expiry(&self, now: Instant) -> Option<Instant> {
        self.live(now).map(|e| e.until).min()
    }
}

/// Фильтр истории.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Show {
    #[default]
    All,
    Errors,
    /// Записи с невыполненным действием.
    Actions,
}

impl Show {
    fn keeps(self, e: &Entry) -> bool {
        match self {
            Show::All => true,
            Show::Errors => e.level == Level::Error,
            Show::Actions => e.pending(),
        }
    }
}

/// Уведомления окна. Встраивается последним ребёнком корня с `.relative()`: рисуется поверх, фокус не берёт.
#[derive(Default)]
pub(crate) struct Notices {
    stack: Stack,
    history_open: bool,
    show: Show,
    /// Таймер «раз в секунду» стоит (один на стопку).
    ticking: bool,
}

impl Notices {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Тост; скроется сам через [`Level::duration`].
    pub(crate) fn push(&mut self, level: Level, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.stack.push(level, text.into(), None, true, Instant::now());
        self.tick(cx);
        cx.notify();
    }

    /// Тост с кнопкой («Отменить»); виден [`ACTION_DURATION`], из истории действие доступно до выполнения.
    pub(crate) fn push_action(&mut self, level: Level, text: impl Into<SharedString>, action: ToastAction, cx: &mut Context<Self>) {
        self.stack.push(level, text.into(), Some(action), true, Instant::now());
        self.tick(cx);
        cx.notify();
    }

    /// Только в историю, без тоста (связь пропала и вернулась — это видно в строке состояния).
    pub(crate) fn note(&mut self, level: Level, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.stack.push(level, text.into(), None, false, Instant::now());
        cx.notify();
    }

    pub(crate) fn toggle_history(&mut self, cx: &mut Context<Self>) {
        self.history_open = !self.history_open;
        if self.history_open {
            self.stack.mark_read();
        }
        cx.notify();
    }

    /// Убрать все тосты с экрана (в истории остаются) — снимки `--screens` между сценами.
    pub(crate) fn hide_all(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<u64> = self.stack.history().filter(|e| e.live).map(|e| e.id).collect();
        for id in ids {
            self.stack.hide(id);
        }
        cx.notify();
    }

    pub(crate) fn is_history_open(&self) -> bool {
        self.history_open
    }

    /// Тостов на экране сейчас (снимок `quota` ждёт свой).
    pub(crate) fn on_screen(&self) -> usize {
        self.stack.shown(Instant::now()).len()
    }

    pub(crate) fn unread(&self) -> usize {
        self.stack.unread()
    }

    /// Раз в секунду, пока тосты на экране: «N с», полоса, уход по времени.
    fn tick(&mut self, cx: &mut Context<Self>) {
        if self.ticking {
            return;
        }
        let now = Instant::now();
        let Some(next) = self.stack.next_expiry(now) else { return };
        self.ticking = true;
        let wait = next.saturating_duration_since(now).min(Duration::from_secs(1)).max(Duration::from_millis(50));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |n, cx| {
                n.ticking = false;
                n.stack.expire(Instant::now());
                n.tick(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn run_action(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(a) = self.stack.take_action(id) {
            cx.notify();
            // Обработчик может трогать эти же уведомления: не внутри их обновления.
            let run = a.run;
            window.defer(cx, move |window, cx| run(window, cx));
        }
    }
}

/// Значок и цвет важности: предупреждение нейтральное, красный — только ошибка (DESIGN).
fn mark(level: Level, l: &Look) -> (char, Hsla) {
    match level {
        Level::Info => (glyph::INFO, l.accent_text()),
        Level::Success => (glyph::CHECK, l.success()),
        Level::Warning => (glyph::WARNING, l.text2()),
        Level::Error => (glyph::ERROR, l.error()),
    }
}

fn bar_color(level: Level, l: &Look) -> Hsla {
    match level {
        Level::Info => l.accent(),
        Level::Success => l.success(),
        Level::Warning => l.text2(),
        Level::Error => l.error(),
    }
}

fn shadow(l: &Look) -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: l.c(Role::ShadowOverlay),
        offset: point(px(0.), px(8.)),
        blur_radius: px(24.),
        spread_radius: px(0.),
        inset: false,
    }]
}

fn close_button(id: impl Into<ElementId>, l: &Look) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(24.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(l.px(Metric::RadiusControl))
        .text_color(l.text2())
        .cursor_pointer()
        .hover(|d| d.bg(l.hover()))
        .child(icon(glyph::CLOSE, 9.))
}

impl Notices {
    fn toast(&self, e: &Entry, now: Instant, l: &Look, cx: &mut Context<Self>) -> AnyElement {
        let id = e.id;
        let (g, color) = mark(e.level, l);
        // Предупреждение и ошибка — с полосой слева (нейтральной и красной), сведение — без.
        let side = matches!(e.level, Level::Warning | Level::Error).then(|| {
            let c = if e.level == Level::Error { l.error() } else { l.c(Role::TextSecondary) };
            div().absolute().left_0().top_0().bottom_0().w(px(3.)).bg(c)
        });
        let acts = e.action.as_ref().filter(|_| !e.done).map(|a| {
            div()
                .id(("notice-act", id as usize))
                .text_color(l.accent_text())
                .font_weight(FontWeight::SEMIBOLD)
                .cursor_pointer()
                .child(a.label.clone())
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.run_action(id, window, cx);
                }))
        });
        div()
            .id(("notice", id as usize))
            .occlude()
            .relative()
            .overflow_hidden()
            .w_full()
            .flex()
            .gap(px(10.))
            .pt(px(12.))
            .pb(px(12.))
            .pl(px(14.))
            .pr(px(6.))
            .rounded(l.px(Metric::RadiusOverlay))
            .bg(l.c(Role::LayerOverlay))
            .border_1()
            .border_color(l.c(Role::StrokeCard))
            .shadow(shadow(l))
            .cursor_pointer()
            .children(side)
            .child(icon(g, 16.).mt(px(1.)).text_color(color))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(div().text_size(px(13.)).line_height(px(18.)).text_color(l.text()).child(e.text.clone()))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(14.))
                            .text_size(px(12.))
                            .children(acts)
                            .child(div().flex_1())
                            .child(div().text_color(l.text3()).child(tr!("notices-left", n = e.left(now) as i64))),
                    ),
            )
            .child(close_button(("notice-close", id as usize), l).on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                if this.stack.hide(id) {
                    cx.notify();
                }
            })))
            .child(
                div()
                    .absolute()
                    .left_0()
                    .bottom_0()
                    .h(px(2.))
                    .w(relative(e.share(now)))
                    .bg(bar_color(e.level, l)),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if this.stack.hide(id) {
                    cx.notify();
                }
            }))
            .into_any_element()
    }

    fn history_panel(&self, l: &Look, cx: &mut Context<Self>) -> AnyElement {
        let entries: Vec<&Entry> = self.stack.history().collect();
        let chip = |show: Show, key: &'static str, n: usize| {
            let on = self.show == show;
            div()
                .id(key)
                .h(px(26.))
                .px(px(10.))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded(px(13.))
                .text_size(px(12.))
                .cursor_pointer()
                .when(on, |d| d.bg(l.text()).text_color(l.panel()))
                .when(!on, |d| d.border_1().border_color(l.c(Role::StrokeCard)).text_color(l.text2()).hover(|d| d.bg(l.hover())))
                .child(tr!(key))
                .child(div().child(n.to_string()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.show = show;
                    cx.notify();
                }))
        };
        let errors = entries.iter().filter(|e| e.level == Level::Error).count();
        let pending = entries.iter().filter(|e| e.pending()).count();
        let today = jiff::Zoned::now().date();
        let mut list = div().id("notices-history").flex().flex_col().flex_1().min_h_0().overflow_y_scroll();
        let mut day = None;
        let mut empty = true;
        for e in entries.iter().filter(|e| self.show.keeps(e)) {
            empty = false;
            let d = e.clock.date();
            if day != Some(d) {
                day = Some(d);
                let title = if d == today {
                    tr!("notices-today")
                } else if today.yesterday().ok() == Some(d) {
                    tr!("notices-yesterday")
                } else {
                    d.strftime("%d.%m.%Y").to_string()
                };
                list = list.child(
                    div()
                        .px(px(16.))
                        .pt(px(10.))
                        .pb(px(4.))
                        .text_size(px(12.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(l.text2())
                        .child(title),
                );
            }
            let id = e.id;
            let (g, color) = mark(e.level, l);
            let state = if e.pending() {
                e.action.as_ref().map(|a| {
                    div()
                        .id(("history-act", id as usize))
                        .text_color(l.accent_text())
                        .font_weight(FontWeight::SEMIBOLD)
                        .cursor_pointer()
                        .child(a.label.clone())
                        .on_click(cx.listener(move |this, _, window, cx| this.run_action(id, window, cx)))
                        .into_any_element()
                })
            } else if e.done {
                Some(div().text_color(l.text3()).child(tr!("notices-done")).into_any_element())
            } else {
                None
            };
            list = list.child(
                div()
                    .flex()
                    .gap(px(10.))
                    .pl(px(16.))
                    .pr(px(14.))
                    .py(px(8.))
                    .border_b_1()
                    .border_color(l.border())
                    .child(icon(g, 16.).mt(px(1.)).text_color(color))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(3.))
                            .child(div().text_size(px(13.)).line_height(px(18.)).child(e.text.clone()))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(10.))
                                    .text_size(px(12.))
                                    .child(div().text_color(l.text3()).child(e.clock.strftime("%H:%M").to_string()))
                                    .children(state),
                            ),
                    ),
            );
        }
        if empty {
            list = list.child(div().p(px(16.)).text_color(l.text2()).child(tr!("notices-empty")));
        }
        // Под командной строкой, справа (макет «Состояния»).
        let top = l.m(Metric::HTitlebar) + crate::bar::BAR_HEIGHT + 4.;
        div()
            .id("notices-panel")
            .occlude()
            .absolute()
            .top(px(top))
            .right(px(8.))
            .bottom(px(crate::chrome::STATUS_HEIGHT + 8.))
            .w(px(PANEL_W))
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(l.px(Metric::RadiusOverlay))
            .bg(l.c(Role::LayerOverlay))
            .border_1()
            .border_color(l.c(Role::StrokeCard))
            .shadow(shadow(l))
            .text_color(l.text())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .pt(px(12.))
                    .pb(px(2.))
                    .pl(px(16.))
                    .pr(px(8.))
                    .child(div().flex_1().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child(tr!("notices-title")))
                    .child(close_button("notices-close", l).on_click(cx.listener(|this, _, _, cx| this.toggle_history(cx)))),
            )
            .child(div().px(px(16.)).pb(px(10.)).text_size(px(12.)).text_color(l.text3()).child(tr!("notices-kept")))
            .child(
                div()
                    .flex()
                    .gap(px(6.))
                    .px(px(16.))
                    .pb(px(10.))
                    .border_b_1()
                    .border_color(l.border())
                    .child(chip(Show::All, "notices-all", entries.len()))
                    .child(chip(Show::Errors, "notices-errors", errors))
                    .child(chip(Show::Actions, "notices-actions", pending)),
            )
            .child(list)
            .child(
                div()
                    .px(px(16.))
                    .py(px(10.))
                    .border_t_1()
                    .border_color(l.border())
                    .text_size(px(12.))
                    .text_color(l.text3())
                    .child(tr!("notices-log")),
            )
            .into_any_element()
    }
}

impl Render for Notices {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let l = crate::roles::look(cx);
        let now = Instant::now();
        let shown: Vec<Entry> = self.stack.shown(now).into_iter().cloned().collect();
        let more = self.stack.more(now);
        let panel = self.history_open.then(|| self.history_panel(&l, cx));
        let stack = (!shown.is_empty()).then(|| {
            let top = l.m(Metric::HTitlebar) + crate::bar::BAR_HEIGHT + 8.;
            let right = if self.history_open { PANEL_W + 28. } else { 16. };
            let plus = (more > 0).then(|| {
                div()
                    .id("notices-more")
                    .occlude()
                    .self_end()
                    .h(px(26.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .rounded(px(13.))
                    .bg(l.c(Role::LayerOverlay))
                    .border_1()
                    .border_color(l.c(Role::StrokeCard))
                    .shadow(shadow(&l))
                    .text_size(px(12.))
                    .text_color(l.text2())
                    .cursor_pointer()
                    .child(tr!("notices-more", n = more as i64))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if !this.history_open {
                            this.toggle_history(cx);
                        }
                    }))
            });
            let toasts: Vec<AnyElement> = shown.iter().map(|e| self.toast(e, now, &l, cx)).collect();
            div()
                .absolute()
                .top(px(top))
                .right(px(right))
                .w(px(WIDTH))
                .flex()
                .flex_col()
                .gap(px(8.))
                .children(toasts)
                .children(plus)
        });
        div().absolute().top_0().left_0().size_full().children(panel).children(stack)
    }
}

/// Опись части (ADR 0005, `parts.rs`).
pub(crate) const PART: crate::part::Part = crate::part::Part { name: "notices", scenes: scenes::SCENES, ..crate::part::Part::NONE };

/// Сцены `--screens` уведомлений (ADR 0005).
pub(crate) mod scenes {
    use gpui_kit::*;
    use pets_i18n::tr;
    use pets_ui::{Level, ToastAction};

    use crate::Mailer;
    use crate::screens::Scene;

    pub(crate) const SCENES: &[Scene] = &[
        // Стопка тостов (три и «+N ещё») рядом с открытой историей уведомлений.
        Scene::new("toasts", toasts, toasts_ready),
    ];

    fn toasts(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        // Связь вернулась (запись в истории), затем пять уведомлений: видно три и «+2 ещё»; история открыта.
        m.on_connection(postman_core::link::Connection::Online, cx);
        m.folders.update(cx, |f, cx| {
            f.own = None;
            cx.notify();
        });
        let undo = || ToastAction::new(tr!("bulk-undo"), |_, _| {});
        let weak = crate::reading::sandbox_label(postman_worker::Level::Partial);
        m.toasts.update(cx, |t, cx| {
            t.push_action(Level::Success, tr!("bulk-done-archive", n = 3), undo(), cx);
            t.push(Level::Info, tr!("bulk-done-read", n = 12), cx);
            t.push(Level::Error, tr!("bulk-phishing-failed", error = String::from("552")), cx);
            t.push(Level::Warning, tr!("sandbox-weak", level = weak), cx);
            t.push_action(Level::Success, tr!("bulk-done-archive", n = 14), undo(), cx);
            if !t.is_history_open() {
                t.toggle_history(cx);
            }
        });
        Ok(())
    }

    fn toasts_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        m.toasts.read(cx).is_history_open() && !m.offline.is_down()
    }
}

#[cfg(test)]
mod tests {
    use super::{Level, MAX_HISTORY, Show, Stack};
    use pets_ui::ToastAction;
    use std::time::{Duration, Instant};

    /// Видно не больше трёх, новые сверху; остальные — «+N»; закрыли верхний — снизу подтягивается следующий.
    #[test]
    fn stack_overflow_shows_three_and_counts_the_rest() {
        let mut s = Stack::default();
        let now = Instant::now();
        let ids: Vec<u64> = (0..5).map(|i| s.push(Level::Info, format!("t{i}").into(), None, true, now)).collect();
        let shown: Vec<u64> = s.shown(now).iter().map(|e| e.id).collect();
        assert_eq!(shown, [ids[4], ids[3], ids[2]]);
        assert_eq!(s.more(now), 2);
        assert!(s.hide(ids[4]));
        assert!(!s.hide(ids[4]), "уже закрыт");
        assert_eq!(s.shown(now).iter().map(|e| e.id).collect::<Vec<_>>(), [ids[3], ids[2], ids[1]]);
        assert_eq!(s.more(now), 1);
        assert_eq!(s.history().count(), 5, "закрытые — в истории");
        assert_eq!(s.unread(), 5);
        s.mark_read();
        assert_eq!(s.unread(), 0);
    }

    /// Время вышло — тост уходит с экрана; ошибка держится дольше сведения, тост с действием — дольше всех.
    #[test]
    fn toasts_expire_by_level_and_action() {
        let mut s = Stack::default();
        let now = Instant::now();
        let info = s.push(Level::Info, "i".into(), None, true, now);
        let err = s.push(Level::Error, "e".into(), None, true, now);
        let act = s.push(Level::Info, "a".into(), Some(ToastAction::new("Undo", |_, _| {})), true, now);
        let later = now + Duration::from_secs(5);
        assert!(s.expire(later));
        let left: Vec<u64> = s.shown(later).iter().map(|e| e.id).collect();
        assert_eq!(left, [act, err], "сведение ушло через 4 с");
        let e = s.shown(later)[1].clone();
        assert_eq!(e.left(later), 3);
        assert!(e.share(later) > 0.3 && e.share(later) < 0.4);
        let later = now + Duration::from_secs(9);
        s.expire(later);
        assert_eq!(s.shown(later).iter().map(|e| e.id).collect::<Vec<_>>(), [act]);
        assert!(!s.history().any(|e| e.id == info && e.live));
    }

    /// Действие — один раз; из истории — тоже, пока не выполнено; после — «выполнено».
    #[test]
    fn action_runs_once_from_toast_or_history() {
        let mut s = Stack::default();
        let now = Instant::now();
        let id = s.push(Level::Info, "Перемещено".into(), Some(ToastAction::new("Отменить", |_, _| {})), true, now);
        let plain = s.push(Level::Warning, "w".into(), None, true, now);
        assert!(s.take_action(plain).is_none());
        // Тост ушёл по времени — действие осталось в истории.
        s.expire(now + Duration::from_secs(16));
        assert!(s.history().find(|e| e.id == id).is_some_and(|e| e.pending()));
        assert!(Show::Actions.keeps(s.history().find(|e| e.id == id).unwrap()));
        assert_eq!(s.take_action(id).map(|a| a.label.to_string()), Some("Отменить".into()));
        assert!(s.take_action(id).is_none(), "второй раз — ничего");
        let e = s.history().find(|e| e.id == id).unwrap();
        assert!(e.done && !e.pending() && !Show::Actions.keeps(e));
    }

    /// Запись только в историю — без тоста; ошибки — в фильтре «Ошибки»; история ограничена.
    #[test]
    fn notes_filters_and_cap() {
        let mut s = Stack::default();
        let now = Instant::now();
        s.push(Level::Warning, "offline".into(), None, false, now);
        assert!(s.shown(now).is_empty());
        s.push(Level::Error, "e".into(), None, true, now);
        assert_eq!(s.history().filter(|e| Show::Errors.keeps(e)).count(), 1);
        for _ in 0..MAX_HISTORY + 5 {
            s.push(Level::Info, "x".into(), None, true, now);
        }
        assert_eq!(s.history().count(), MAX_HISTORY);
    }
}
