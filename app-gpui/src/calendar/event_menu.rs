//! Меню события по правой кнопке (#660) и новая встреча двойным щелчком по пустому месту сетки (#661).
//!
//! Пункты меню — команды реестра (`calendar.open_event`, `calendar.accept` …): клавиша справа — из таблицы команд,
//! выполнение — то же, что у кнопок подробностей. Отмена встречи подтверждается строкой в подробностях (без модальных
//! окон). Двойной щелчок по свободному месту дня или недели — вкладка встречи с началом в этом шаге сетки; в месяце —
//! в этот день, время — как у «Новой встречи».

use gpui_kit::prelude::*;
use gpui_kit::*;
use jiff::civil::{Date, DateTime, Time};
use pets_i18n::tr;
use pets_ui::theme::Metric;
use postman_calendar::Partstat;

use super::{CalendarAction, CalendarView, GRID_HOURS, HOUR_PX, Opens, Own, g, my_part, write_list};
use crate::chrome::icon;

/// Пункт меню события.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EventItem {
    Open,
    Answer(Partstat),
    Write,
    Cancel,
}

impl EventItem {
    fn label(self) -> &'static str {
        match self {
            EventItem::Open => "cal-open-event",
            EventItem::Answer(Partstat::Accepted) => "cal-accept",
            EventItem::Answer(Partstat::Tentative) => "cal-tentative-answer",
            EventItem::Answer(_) => "cal-decline",
            EventItem::Write => "cal-write-participants",
            EventItem::Cancel => "cal-cancel-meeting",
        }
    }

    /// Команда реестра: клавиша справа и то же действие из палитры.
    pub(crate) fn command(self) -> &'static str {
        match self {
            EventItem::Open => "calendar.open_event",
            EventItem::Answer(Partstat::Accepted) => "calendar.accept",
            EventItem::Answer(Partstat::Tentative) => "calendar.tentative",
            EventItem::Answer(_) => "calendar.decline",
            EventItem::Write => "calendar.write_participants",
            EventItem::Cancel => "calendar.cancel_meeting",
        }
    }
}

/// Состав меню: «Открыть» — если событие открывается; приглашение — ответы, кроме уже данного; «Написать
/// участникам» — если есть кому; своя встреча — «Отменить встречу» последней группой.
pub(crate) fn event_menu(opens: Option<Opens>, status: Option<Partstat>, can_write: bool) -> Vec<Vec<EventItem>> {
    let mut groups = Vec::new();
    if opens.is_some() {
        groups.push(vec![EventItem::Open]);
    }
    if opens == Some(Opens::Letter) {
        let answers: Vec<EventItem> =
            [Partstat::Accepted, Partstat::Tentative, Partstat::Declined].into_iter().filter(|&s| status != Some(s)).map(EventItem::Answer).collect();
        groups.push(answers);
    }
    if can_write {
        groups.push(vec![EventItem::Write]);
    }
    if opens == Some(Opens::Edit) {
        groups.push(vec![EventItem::Cancel]);
    }
    groups
}

/// Начало новой встречи по месту щелчка в сетке дня: `y` — от 00:00, вниз до шага сетки `step` (мин).
pub(crate) fn slot_start(date: Date, y: f32, step: i64) -> DateTime {
    let step = step.clamp(5, 60);
    let minutes = (y.max(0.) / HOUR_PX * 60.) as i64;
    let minutes = (minutes / step * step).min(GRID_HOURS * 60 - step);
    let time = Time::new((minutes / 60) as i8, (minutes % 60) as i8, 0, 0).unwrap_or(Time::midnight());
    date.to_datetime(time)
}

/// Начало новой встречи в день месяца: время — как у «Новой встречи» (`meeting::default_start`); если оно
/// уходит на другой день (сегодня рабочее время кончилось, выходной), — 10:00 этого дня.
pub(crate) fn month_start(date: Date, now: &jiff::Zoned) -> DateTime {
    let start = crate::meeting::default_start(now);
    if start.date() == date && date == now.date() { start } else { date.to_datetime(Time::new(10, 0, 0, 0).unwrap_or(Time::midnight())) }
}

/// Открытое меню события: у точки окна, над событием `index`.
pub(super) struct EventMenu {
    at: Point<Pixels>,
    index: usize,
    groups: Vec<Vec<EventItem>>,
    cursor: usize,
    focus: FocusHandle,
    before: Option<FocusHandle>,
    focus_pending: bool,
}

impl EventMenu {
    fn items(&self) -> impl Iterator<Item = EventItem> + '_ {
        self.groups.iter().flatten().copied()
    }
}

impl CalendarView {
    /// Нажатие по событию: щелчок по пустому месту под ним — уже не «пусто» (сетка и клетка месяца ниже по дереву).
    pub(super) fn event_down(&mut self) {
        self.event_hit = true;
    }

    /// Правая кнопка по событию: выбрать его (подробности справа) и открыть меню у курсора.
    pub(super) fn open_event_menu(&mut self, i: usize, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(e) = self.events.get(i) else { return };
        let me = self.me.as_deref();
        let to = write_list(e, me);
        let can_write = !to.is_empty() && e.participants.iter().filter(|p| p.attendee || p.owner).count() > 1;
        let groups = event_menu(self.open_target(e), my_part(e, me).map(|p| p.status), can_write);
        if groups.is_empty() {
            return;
        }
        if self.selected != Some(i) {
            self.select(i, cx);
        }
        let before = window.focused(cx);
        self.event_menu = Some(EventMenu { at, index: i, groups, cursor: 0, focus: cx.focus_handle(), before, focus_pending: true });
        cx.notify();
    }

    pub(super) fn close_event_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(m) = self.event_menu.take() {
            if let Some(f) = m.before {
                window.focus(&f, cx);
            }
            cx.notify();
        }
    }

    fn choose(&mut self, it: EventItem, window: &mut Window, cx: &mut Context<Self>) {
        let Some(i) = self.event_menu.as_ref().map(|m| m.index) else { return };
        self.close_event_menu(window, cx);
        if self.selected != Some(i) {
            self.select(i, cx);
        }
        match it {
            EventItem::Open => self.open_selected(cx),
            EventItem::Answer(s) => self.answer_selected(s, cx),
            EventItem::Write => self.write_participants(cx),
            EventItem::Cancel => self.cancel_selected(cx),
        }
    }

    /// `calendar.accept` / `tentative` / `decline`: ответ на выбранное приглашение (как кнопки подробностей).
    pub(crate) fn answer_selected(&mut self, s: Partstat, cx: &mut Context<Self>) {
        let Some(i) = self.selected else { return };
        if self.events.get(i).and_then(|e| self.open_target(e)) == Some(Opens::Letter) {
            self.respond(i, s, Default::default(), cx);
        }
    }

    /// `calendar.cancel_meeting`: своя встреча — строка подтверждения в подробностях (у повтора — эту или серию).
    pub(crate) fn cancel_selected(&mut self, cx: &mut Context<Self>) {
        let Some(i) = self.selected else { return };
        if self.events.get(i).and_then(|e| self.open_target(e)) == Some(Opens::Edit) {
            self.own_step(Own::Cancel, cx);
        }
    }

    /// Клавиши меню: стрелки, Home/End, Enter и пробел — выполнить, Esc — закрыть.
    fn menu_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(m) = &mut self.event_menu else { return };
        let n = m.items().count().max(1);
        let chosen = m.items().nth(m.cursor);
        match ev.keystroke.key.as_str() {
            "down" => m.cursor = (m.cursor + 1) % n,
            "up" => m.cursor = (m.cursor + n - 1) % n,
            "home" => m.cursor = 0,
            "end" => m.cursor = n - 1,
            "escape" => self.close_event_menu(window, cx),
            "enter" | "space" => {
                if let Some(it) = chosen {
                    self.choose(it, window, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// Слой меню поверх экрана — как контекстное меню письма и контакта.
    pub(super) fn event_menu_layer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let m = self.event_menu.as_mut()?;
        if std::mem::take(&mut m.focus_pending) {
            window.focus(&m.focus, cx);
        }
        let l = crate::roles::look(cx);
        let m = self.event_menu.as_ref()?;
        let mut col = div().flex().flex_col();
        let mut n = 0usize;
        for (k, group) in m.groups.iter().enumerate() {
            if k > 0 {
                col = col.child(crate::bulk::separator(&l));
            }
            for &it in group {
                let ix = n;
                n += 1;
                let key = crate::bulk::key_hint(it.command(), cx);
                let mark = match it {
                    EventItem::Answer(s) => crate::invite_card::status_mark(s, l.text2()),
                    EventItem::Open => icon(g::OPEN, 16.).text_color(l.text2()),
                    EventItem::Write => icon(g::MAIL, 16.).text_color(l.text2()),
                    EventItem::Cancel => icon(g::CANCEL, 14.).text_color(l.text2()),
                };
                col = col.child(
                    crate::bulk::menu_line(("event-menu-item", ix), &l)
                        .when(m.cursor == ix, |d| d.bg(l.hover()))
                        .child(div().w(px(16.)).flex_none().flex().justify_center().child(mark))
                        .child(div().flex_1().min_w_0().truncate().child(tr!(it.label())))
                        .when(!key.is_empty(), |d| d.child(div().text_size(px(11.)).text_color(l.text3()).child(key)))
                        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                            if let Some(m) = &mut this.event_menu
                                && *hovered
                                && m.cursor != ix
                            {
                                m.cursor = ix;
                                cx.notify();
                            }
                        }))
                        .on_click(cx.listener(move |this, _, window, cx| this.choose(it, window, cx))),
                );
            }
        }
        let body = div()
            .id("event-menu")
            .key_context("ContextMenu")
            .track_focus(&m.focus)
            .occlude()
            .w(px(240.))
            .p(px(4.))
            .rounded(l.px(Metric::RadiusOverlay))
            .border_1()
            .border_color(l.c(pets_ui::theme::Role::StrokeCard))
            .bg(l.c(pets_ui::theme::Role::LayerOverlay))
            .shadow_lg()
            .text_size(px(13.))
            .text_color(l.text())
            .child(col)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| this.menu_key(ev, window, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close_event_menu(window, cx)));
        Some(deferred(anchored().position(m.at).snap_to_window_with_margin(px(8.)).child(body)).with_priority(2).into_any_element())
    }

    /// Нажатие левой по столбцу дня: двойное по пустому месту — новая встреча с началом в этом шаге сетки.
    pub(super) fn grid_down(&mut self, date: Date, ev: &MouseDownEvent, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.event_hit) || ev.click_count != 2 {
            return;
        }
        let top = self.scroll.bounds().origin.y + self.scroll.offset().y;
        let y = f32::from(ev.position.y - top);
        self.new_meeting_at(slot_start(date, y, self.step), cx);
    }

    /// Нажатие левой по клетке месяца: двойное по пустому месту — новая встреча в этот день.
    pub(super) fn month_down(&mut self, date: Date, ev: &MouseDownEvent, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.event_hit) || ev.click_count != 2 {
            return;
        }
        let now = self.now_zoned();
        self.new_meeting_at(month_start(date, &now), cx);
    }

    fn new_meeting_at(&mut self, start: DateTime, cx: &mut Context<Self>) {
        if crate::features::available("calendar.new_meeting", crate::features::get(cx)) {
            cx.emit(CalendarAction::NewMeeting(start));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{EventItem as I, Opens, Partstat as P, event_menu, month_start, slot_start};
    use jiff::civil::{date, datetime};

    /// Попадание в слот (#661): шаг сетки вниз от места щелчка, низ суток — последний шаг.
    #[test]
    fn double_click_lands_in_the_grid_step() {
        let d = date(2026, 11, 13);
        let h = super::HOUR_PX;
        assert_eq!(slot_start(d, 0., 30), datetime(2026, 11, 13, 0, 0, 0, 0));
        assert_eq!(slot_start(d, 9.4 * h, 30), datetime(2026, 11, 13, 9, 0, 0, 0));
        assert_eq!(slot_start(d, 9.6 * h, 30), datetime(2026, 11, 13, 9, 30, 0, 0));
        assert_eq!(slot_start(d, 9.6 * h, 15), datetime(2026, 11, 13, 9, 30, 0, 0));
        assert_eq!(slot_start(d, 9.8 * h, 15), datetime(2026, 11, 13, 9, 45, 0, 0));
        assert_eq!(slot_start(d, 25. * h, 30), datetime(2026, 11, 13, 23, 30, 0, 0));
        assert_eq!(slot_start(d, -5., 30), datetime(2026, 11, 13, 0, 0, 0, 0));
    }

    /// Месяц (#661): сегодня — время «Новой встречи», другой день — 10:00 этого дня.
    #[test]
    fn month_double_click_takes_the_command_time() {
        let now = datetime(2026, 11, 13, 11, 10, 0, 0).to_zoned(jiff::tz::TimeZone::UTC).unwrap();
        assert_eq!(month_start(date(2026, 11, 13), &now), datetime(2026, 11, 13, 11, 30, 0, 0));
        assert_eq!(month_start(date(2026, 11, 20), &now), datetime(2026, 11, 20, 10, 0, 0, 0));
        let late = datetime(2026, 11, 13, 18, 0, 0, 0).to_zoned(jiff::tz::TimeZone::UTC).unwrap();
        assert_eq!(month_start(date(2026, 11, 13), &late), datetime(2026, 11, 13, 10, 0, 0, 0));
    }

    /// Состав меню (#660): своя — открыть и отменить; приглашение — ответы без данного; чужое — только письмо.
    #[test]
    fn event_menu_by_role() {
        assert_eq!(event_menu(Some(Opens::Edit), Some(P::Accepted), true), vec![vec![I::Open], vec![I::Write], vec![I::Cancel]]);
        assert_eq!(
            event_menu(Some(Opens::Letter), Some(P::NeedsAction), false),
            vec![vec![I::Open], vec![I::Answer(P::Accepted), I::Answer(P::Tentative), I::Answer(P::Declined)]]
        );
        assert_eq!(event_menu(Some(Opens::Letter), Some(P::Accepted), true)[1], vec![I::Answer(P::Tentative), I::Answer(P::Declined)]);
        assert_eq!(event_menu(None, None, true), vec![vec![I::Write]]);
        assert!(event_menu(None, None, false).is_empty());
    }

    /// Каждый пункт — команда реестра календаря.
    #[test]
    fn event_menu_items_are_commands() {
        for it in [I::Open, I::Answer(P::Accepted), I::Answer(P::Tentative), I::Answer(P::Declined), I::Write, I::Cancel] {
            assert!(pets_commands::find(crate::commands::all(), it.command()).is_some(), "{}", it.command());
        }
    }
}
