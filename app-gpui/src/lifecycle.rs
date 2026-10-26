//! Жизненный цикл писем в интерфейсе (S5-04, `docs/DESIGN.md` «Строка письма и метки»): метки сроков в строке и в
//! шапке письма, действия «Срок…», «Напомнить…», «Ждать ответа…», «Исчезнет…» (выбор — панелью на месте, не окном),
//! планировщик и карточка «Напоминание · Открыть».
//!
//! Решает всё `postman_core::lifecycle` (без UI): метки ([`Badge`]), правки писем ([`Patch`]), план на сейчас
//! ([`plan`]). Здесь — тексты, цвета ролей и связь с экраном: правка — сразу в кэш (в фоне) и серверу одним
//! `Command::Patch`; проход планировщика — в фоне по кэшу, после изменений почты и к ближайшему сроку одним таймером
//! gpui (между сроками таймеров нет: в простое — 0 % CPU). Работает, пока Postman запущен (решение владельца №2).

use std::time::Duration;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use jiff::Zoned;
use jiff::civil::Weekday;
use pets_i18n::{I18n, tr};
use pets_ui::Level as Toast;
use pets_ui::theme::{Metric, Role};
use postman_cache::EmailRow;
use postman_core::lifecycle::{self, Ahead, Bad, Badge, Day, Notice, Patch, Pick, Plan, Roles, Span};
use postman_core::marks::{Kind, When};
use postman_core::sync::Command;

use crate::Mailer;
use crate::actions::mail;
use crate::chrome::{glyph, icon};
use crate::roles::{Look, Pm};

/// Меток сроков в строке письма — не больше (после категорий).
pub(crate) const ROW_BADGES: usize = 2;
/// Карточек «Напоминание» на экране — не больше (новые — сверху).
const CARDS: usize = 3;

/// Состояние экрана почты: панель выбора, планировщик, карточки напоминаний.
#[derive(Default)]
pub(crate) struct Lifecycle {
    pub(crate) picker: Option<Picker>,
    /// Проход планировщика идёт; `dirty` — пока шёл, почта изменилась: ещё один.
    running: bool,
    dirty: bool,
    /// К какому сроку стоит таймер (Unix-секунды) и номер таймера: устаревшие срабатывания пропускаются.
    armed: Option<i64>,
    timer: u64,
    /// Когда последний раз проверяли рассылки (Unix-секунды).
    last_news: Option<i64>,
    /// Сработавшие напоминания: письмо и тема.
    pub(crate) cards: Vec<(String, String)>,
}

/// Панель выбора срока у письма.
pub(crate) struct Picker {
    pub(crate) kind: Kind,
    pub(crate) row: EmailRow,
    /// Остальные письма выделения (S5-08): срок ставится всем.
    pub(crate) rest: Vec<EmailRow>,
    /// «Выбрать дату…»: поле ввода и ошибка разбора.
    pub(crate) input: Option<Entity<InputState>>,
    pub(crate) error: Option<Bad>,
    _enter: Option<Subscription>,
}

impl Picker {
    pub(crate) fn new(kind: Kind, row: EmailRow) -> Picker {
        Picker { kind, row, rest: Vec::new(), input: None, error: None, _enter: None }
    }
}

/// Срок ожидания по умолчанию (Настройки → «Ждём ответа · N д»).
pub(crate) fn wait_default(cx: &App) -> u8 {
    u8::try_from(crate::prefs::get(cx).lifecycle.wait_reply_days).unwrap_or(postman_core::marks::WAIT_DEFAULT)
}

// --- тексты ---

fn weekday(wd: Weekday) -> String {
    weekday_in(&pets_i18n::current(), wd)
}

fn weekday_in(t: &I18n, wd: Weekday) -> String {
    t.get(&format!("send-weekday-{}", wd.to_monday_one_offset()))
}

/// Текст с одним параметром.
fn fmt1(t: &I18n, key: &str, name: &'static str, value: impl Into<pets_i18n::FluentValue<'static>>) -> String {
    let mut args = pets_i18n::FluentArgs::new();
    args.set(name, value);
    t.fmt(key, &args)
}

fn hhmm(t: jiff::civil::Time) -> String {
    t.strftime("%H:%M").to_string()
}

fn ddmm(d: jiff::civil::Date) -> String {
    d.strftime("%d.%m").to_string()
}

/// День срока: «завтра», «чт», «24.10» (сегодня — `None`).
fn day_text(t: &I18n, a: &Ahead) -> Option<String> {
    match a.day {
        Day::Today => None,
        Day::Tomorrow => Some(t.get("lifecycle-tomorrow-short")),
        Day::Weekday(wd) => Some(weekday_in(t, wd)),
        Day::Date(d) => Some(ddmm(d)),
    }
}

/// Текст метки: «До чт», «До 15:00», «Просрочено 1 д», «Ждём ответа · 2 д», «Напомнит чт 09:00», «Исчезнет через 3 д».
pub(crate) fn badge_text(b: &Badge) -> String {
    badge_text_in(&pets_i18n::current(), b)
}

fn badge_text_in(t: &I18n, b: &Badge) -> String {
    let n = |key: &str, n: i64| fmt1(t, key, "n", n);
    match b {
        Badge::Due(None) => t.get("lifecycle-due"),
        Badge::Due(Some(a)) => match (day_text(t, a), a.time) {
            (None, Some(time)) => fmt1(t, "lifecycle-due-by", "when", hhmm(time)),
            (None, None) => t.get("lifecycle-due-today"),
            (Some(d), _) => fmt1(t, "lifecycle-due-by", "when", d),
        },
        Badge::Overdue(Span::Days(d)) => n("lifecycle-overdue-days", *d),
        Badge::Overdue(Span::Hours(h)) => n("lifecycle-overdue-hours", *h),
        Badge::Waiting(None) => t.get("lifecycle-waiting"),
        Badge::Waiting(Some(0)) => t.get("lifecycle-waiting-today"),
        Badge::Waiting(Some(d)) => n("lifecycle-waiting-days", *d),
        Badge::NoReply(d) => n("lifecycle-no-reply", *d),
        Badge::Remind(a) => {
            let time = a.time.map(hhmm).unwrap_or_default();
            match day_text(t, a) {
                None => fmt1(t, "lifecycle-remind-at", "when", time),
                Some(d) => fmt1(t, "lifecycle-remind-at", "when", format!("{d} {time}")),
            }
        }
        Badge::Expires(None) => t.get("lifecycle-expires"),
        Badge::Expires(Some(Span::Days(d))) => n("lifecycle-expires-days", *d),
        Badge::Expires(Some(Span::Hours(h))) => n("lifecycle-expires-hours", *h),
    }
}

/// Метка срока (DESIGN): срок впереди — янтарный текст с часами без подложки; прошёл — плотная янтарная заливка;
/// остальные — нейтральные серые с тонкой рамкой. Цвета — только роли (`app.deadline.*`).
pub(crate) fn chip(b: &Badge, l: &Look) -> Div {
    let base = div().flex().flex_none().items_center().gap(px(3.)).text_size(px(11.)).line_height(px(16.)).whitespace_nowrap();
    let text = badge_text(b);
    match b {
        Badge::Due(_) => base.text_color(l.pm(Pm::DeadlineFg)).font_weight(FontWeight::SEMIBOLD).child(icon(glyph::CLOCK, 10.)).child(text),
        Badge::Overdue(_) => {
            base.px_1p5().rounded_sm().bg(l.pm(Pm::DeadlineFill)).text_color(l.pm(Pm::DeadlineOn)).font_weight(FontWeight::SEMIBOLD).child(text)
        }
        _ => base.px_1p5().rounded_sm().border_1().border_color(l.stroke()).text_color(l.text2()).child(text),
    }
}

/// Метки сроков строки письма (не больше [`ROW_BADGES`]).
pub(crate) fn row_badges(row: &EmailRow, now: &Zoned) -> Vec<Badge> {
    let mut b = lifecycle::badges(&row.keywords, now);
    b.truncate(ROW_BADGES);
    b
}

fn kind_key(kind: Kind) -> &'static str {
    match kind {
        Kind::Due => "lifecycle-act-due",
        Kind::Remind => "lifecycle-act-remind",
        Kind::Wait => "lifecycle-act-wait",
        Kind::Expire => "lifecycle-act-expire",
    }
}

/// Команда реестра срока.
fn command(kind: Kind) -> &'static str {
    match kind {
        Kind::Due => "mail.due",
        Kind::Remind => "mail.remind",
        Kind::Wait => "mail.wait",
        Kind::Expire => "mail.expire",
    }
}

fn action(kind: Kind) -> Box<dyn Action> {
    match kind {
        Kind::Due => Box::new(mail::Due),
        Kind::Remind => Box::new(mail::Remind),
        Kind::Wait => Box::new(mail::Wait),
        Kind::Expire => Box::new(mail::Expire),
    }
}

/// Шапка письма: метки сроков и кнопки «Срок», «Напомнить», «Ждать ответа», «Исчезнет» (команды `mail.due`…).
pub(crate) fn header_row(row: &EmailRow, l: &Look, cx: &App) -> Div {
    let now = Zoned::now();
    let badges = lifecycle::badges(&row.keywords, &now);
    let button = |kind: Kind| {
        let b = div()
            .id(kind_key(kind))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .h(px(24.))
            .px(px(6.))
            .rounded(l.px(Metric::RadiusControl))
            .text_size(l.px(Metric::TypeCaption))
            .text_color(l.text2())
            .cursor_pointer()
            .hover(|d| d.bg(l.hover()))
            .when(kind == Kind::Due, |d| d.child(icon(glyph::CLOCK, 12.)))
            .when(kind == Kind::Remind, |d| d.child(icon(glyph::RINGER, 12.)))
            .child(tr!(kind_key(kind)))
            .on_click(move |_, window, cx| window.dispatch_action(action(kind), cx));
        crate::keytips::command(b, command(kind), Some(tr!(kind_key(kind))), l, cx)
    };
    // Нет ключевых слов у сервера учётки — нет и кнопок сроков (S7, «Нет у сервера — нет на экране»).
    let features = crate::features::get(cx);
    let kinds = [Kind::Due, Kind::Remind, Kind::Wait, Kind::Expire].into_iter().filter(|&k| crate::features::available(command(k), features));
    div().flex().flex_wrap().items_center().gap(px(6.)).children(badges.iter().map(|b| chip(b, l))).child(div().flex_1()).children(kinds.map(button))
}

pub(crate) fn pick_text(p: Pick, kind: Kind) -> String {
    match p {
        Pick::Today => tr!("lifecycle-today"),
        Pick::Tomorrow => tr!("lifecycle-tomorrow"),
        Pick::Friday => tr!("lifecycle-friday"),
        Pick::NextWeek => tr!("lifecycle-next-week"),
        Pick::LaterToday => tr!("lifecycle-later-today"),
        Pick::Monday => tr!("lifecycle-monday"),
        Pick::Days(n) if kind == Kind::Expire => tr!("lifecycle-in-days", n = n),
        Pick::Days(n) => tr!("lifecycle-days", n = n),
    }
}

/// Подсказка справа: «пт 23.10», «чт 18:00».
fn when_hint(w: &When) -> String {
    let wd = weekday(w.date.weekday());
    match w.time {
        Some(t) => format!("{wd} {} {}", ddmm(w.date), hhmm(t)),
        None => format!("{wd} {}", ddmm(w.date)),
    }
}

fn picker_title(kind: Kind) -> String {
    match kind {
        Kind::Due => tr!("lifecycle-pick-due"),
        Kind::Remind => tr!("lifecycle-pick-remind"),
        Kind::Wait => tr!("lifecycle-pick-wait"),
        Kind::Expire => tr!("lifecycle-pick-expire"),
    }
}

/// Последний пункт панели: снять срок («Никогда» у «Исчезнет»).
fn remove_text(kind: Kind) -> String {
    match kind {
        Kind::Due => tr!("lifecycle-remove-due"),
        Kind::Remind => tr!("lifecycle-remove-remind"),
        Kind::Wait => tr!("lifecycle-remove-wait"),
        Kind::Expire => tr!("lifecycle-never"),
    }
}

fn notice_text(n: &Notice) -> Option<(Toast, String)> {
    match n {
        Notice::Reminded { .. } => None,
        Notice::NoReply { subject, .. } => Some((Toast::Info, tr!("lifecycle-no-reply-toast", subject = subject.clone()))),
        Notice::Replied { subject, .. } => Some((Toast::Success, tr!("lifecycle-replied-toast", subject = subject.clone()))),
        Notice::Expired(n) => Some((Toast::Info, tr!("lifecycle-expired-toast", n = *n))),
    }
}

fn menu_item(id: ElementId, l: &Look) -> Stateful<Div> {
    div()
        .id(id)
        .h(l.px(Metric::HMenuItem))
        .px(px(10.))
        .flex()
        .items_center()
        .gap(px(10.))
        .rounded(l.px(Metric::RadiusControl))
        .cursor_pointer()
        .hover(|d| d.bg(l.hover()))
}

// --- экран почты ---

impl Mailer {
    /// Письмо для действия: открытое в области чтения, иначе выбранное в списке.
    pub(crate) fn lifecycle_target(&self, cx: &App) -> Option<EmailRow> {
        self.reading.read(cx).reading.as_ref().map(|r| r.row.clone()).or_else(|| self.list.read(cx).selected_row())
    }

    fn roles(&self, cx: &App) -> Roles {
        Roles::of(&self.folders.read(cx).mailboxes, None)
    }

    /// `mail.due`, `mail.remind`, `mail.wait`, `mail.expire`: панель выбора у письма (та же команда — закрыть).
    pub(crate) fn open_picker(&mut self, kind: Kind, cx: &mut Context<Self>) {
        if self.lifecycle.picker.as_ref().is_some_and(|p| p.kind == kind) {
            self.lifecycle.picker = None;
            cx.notify();
            return;
        }
        // Выделено несколько писем — срок всем (S5-08).
        if self.picking(cx) {
            let ids = self.targets(cx);
            return self.open_picker_for(kind, ids, cx);
        }
        let Some(row) = self.lifecycle_target(cx) else {
            self.toasts.update(cx, |t, cx| t.push(Toast::Info, tr!("actions-select-letter"), cx));
            return;
        };
        self.picker_on(kind, vec![row], cx);
    }

    /// Панель выбора над письмами `rows` (без права менять ключевые слова — пропускаются).
    pub(crate) fn picker_on(&mut self, kind: Kind, rows: Vec<EmailRow>, cx: &mut Context<Self>) {
        let rights = self.list.read(cx).rights.clone();
        let mut rows: Vec<EmailRow> = rows.into_iter().filter(|r| crate::list::may(&rights, r, |x| x.keywords)).collect();
        if rows.is_empty() {
            self.toasts.update(cx, |t, cx| t.push(Toast::Info, tr!("lifecycle-read-only"), cx));
            return;
        }
        let row = rows.remove(0);
        let mut p = Picker::new(kind, row);
        p.rest = rows;
        self.lifecycle.picker = Some(p);
        cx.notify();
    }

    /// Esc: закрыть панель выбора (`true` — была открыта).
    pub(crate) fn close_picker(&mut self, cx: &mut Context<Self>) -> bool {
        let open = self.lifecycle.picker.take().is_some();
        if open {
            cx.notify();
        }
        open
    }

    /// Вариант панели или «Убрать»: правка письма.
    fn choose(&mut self, when: Option<When>, cx: &mut Context<Self>) {
        let Some(p) = self.lifecycle.picker.take() else { return };
        let roles = self.roles(cx);
        let patches: Vec<Patch> = std::iter::once(&p.row)
            .chain(&p.rest)
            .map(|row| match (p.kind, when) {
                (Kind::Remind, Some(w)) => lifecycle::snooze(row, w, &roles),
                (Kind::Expire, None) => lifecycle::keep(row),
                (kind, w) => lifecycle::set(row, kind, w),
            })
            .collect();
        if let (Kind::Remind, Some(w)) = (p.kind, when) {
            let text = tr!("lifecycle-snoozed", when = when_hint(&w));
            self.toasts.update(cx, |t, cx| t.push(Toast::Success, text, cx));
        }
        self.apply_patches(patches, cx);
        cx.notify();
    }

    /// «Выбрать дату…»: поле ввода в панели.
    fn pick_date(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(kind) = self.lifecycle.picker.as_ref().map(|p| p.kind) else { return };
        let placeholder = if matches!(kind, Kind::Wait) { tr!("lifecycle-date-placeholder") } else { tr!("lifecycle-date-time-placeholder") };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        input.update(cx, |s, cx| s.focus(window, cx));
        let enter = cx.subscribe_in(&input, window, |this: &mut Self, _, ev: &InputEvent, _, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                this.pick_typed(cx);
            }
        });
        if let Some(p) = &mut self.lifecycle.picker {
            p.input = Some(input);
            p._enter = Some(enter);
            p.error = None;
        }
        cx.notify();
    }

    /// «Поставить» (или Enter) в поле даты.
    fn pick_typed(&mut self, cx: &mut Context<Self>) {
        let Some(p) = &self.lifecycle.picker else { return };
        let Some(text) = p.input.as_ref().map(|i| i.read(cx).value().to_string()) else { return };
        match lifecycle::parse_when(&text, p.kind, &Zoned::now()) {
            Ok(w) => self.choose(Some(w), cx),
            Err(e) => {
                if let Some(p) = &mut self.lifecycle.picker {
                    p.error = Some(e);
                }
                cx.notify();
            }
        }
    }

    /// Правки писем: кэш — сразу (в фоне), затем список и область чтения; сервер — одним `Command::Patch`.
    pub(crate) fn apply_patches(&mut self, patches: Vec<Patch>, cx: &mut Context<Self>) {
        if patches.is_empty() {
            return;
        }
        let local: Vec<Patch> = patches.iter().map(lifecycle::for_cache).collect();
        if self.backend.is_live() {
            let archive_name = tr!("lifecycle-archive-folder");
            if !self.backend.send(Command::Patch { patches, archive_name }) {
                self.sync_stopped(cx);
            }
        }
        let cache = self.cache.clone();
        let task = cx.background_spawn(async move { cache.patch_emails(&local) });
        cx.spawn(async move |this, cx| {
            let rows = task.await;
            let _ = this.update(cx, |m, cx| match rows {
                Ok(rows) => m.patched(rows, cx),
                Err(e) => tracing::warn!("lifecycle: {e}"),
            });
        })
        .detach();
    }

    /// Кэш изменён: список, письмо в области чтения, затем — планировщик (новый ближайший срок).
    fn patched(&mut self, rows: Vec<EmailRow>, cx: &mut Context<Self>) {
        self.list.update(cx, |l, cx| l.refresh(cx));
        self.reading.update(cx, |r, cx| {
            if let Some(open) = r.reading.as_mut()
                && let Some(row) = rows.iter().find(|x| x.id == open.row.id)
            {
                open.row = row.clone();
                cx.notify();
            }
        });
        self.lifecycle_pass(false, cx);
    }

    /// Проход планировщика (в фоне, по кэшу): что сделать сейчас и к какому сроку проснуться. Снимки и замер — без
    /// него (повторяемы).
    pub(crate) fn lifecycle_pass(&mut self, timer: bool, cx: &mut Context<Self>) {
        if self.screens.is_some() || self.bench.is_some() {
            return;
        }
        if self.lifecycle.running {
            self.lifecycle.dirty = true;
            return;
        }
        self.lifecycle.running = true;
        let cache = self.cache.clone();
        let roles = self.roles(cx);
        let rules = crate::prefs::get(cx).lifecycle;
        let me = self.list.read(cx).me.clone();
        let task = cx.background_spawn(async move {
            let now = Zoned::now();
            let input = lifecycle::gather(&cache, &roles, now.timestamp().as_second())?;
            let plan = lifecycle::plan(&input, &roles, &rules, &me, &now);
            let local: Vec<Patch> = plan.patches.iter().map(lifecycle::for_cache).collect();
            cache.patch_emails(&local)?;
            Ok::<Plan, postman_cache::Error>(plan)
        });
        cx.spawn(async move |this, cx| {
            let plan = task.await;
            let _ = this.update(cx, |m, cx| m.lifecycle_done(plan, timer, cx));
        })
        .detach();
    }

    fn lifecycle_done(&mut self, plan: Result<Plan, postman_cache::Error>, timer: bool, cx: &mut Context<Self>) {
        self.lifecycle.running = false;
        let now = jiff::Timestamp::now().as_second();
        match plan {
            Err(e) => tracing::warn!("lifecycle: {e}"),
            Ok(plan) => {
                let changed = !plan.patches.is_empty();
                if changed && self.backend.is_live() {
                    let archive_name = tr!("lifecycle-archive-folder");
                    if !self.backend.send(Command::Patch { patches: plan.patches, archive_name }) {
                        self.sync_stopped(cx);
                    }
                }
                // Кэш уже изменён (или срок сменил метку «До …» на «Просрочено»): список заново.
                if changed || timer {
                    self.list.update(cx, |l, cx| l.refresh(cx));
                    self.reading.update(cx, |_, cx| cx.notify());
                }
                for n in &plan.notices {
                    if let Notice::Reminded { id, subject } = n {
                        self.lifecycle.cards.retain(|(i, _)| i != id);
                        self.lifecycle.cards.insert(0, (id.clone(), subject.clone()));
                        self.lifecycle.cards.truncate(CARDS);
                    }
                    if let Some((level, text)) = notice_text(n) {
                        self.toasts.update(cx, |t, cx| t.push(level, text, cx));
                    }
                }
                let mut next = plan.next;
                if let Some(at) = self.newsletters(now, cx) {
                    next = Some(next.map_or(at, |n| n.min(at)));
                }
                self.arm_lifecycle(next, now, cx);
                cx.notify();
            }
        }
        if std::mem::take(&mut self.lifecycle.dirty) {
            self.lifecycle_pass(false, cx);
        }
    }

    /// Рассылки в архив (живой ящик, настройка включена): раз в `NEWS_EVERY`. Ответ — когда проверить снова.
    fn newsletters(&mut self, now: i64, cx: &mut Context<Self>) -> Option<i64> {
        let after = crate::prefs::get(cx).lifecycle.archive_newsletters()?;
        if !self.backend.is_live() {
            return None;
        }
        let every = i64::try_from(lifecycle::NEWS_EVERY.as_secs()).unwrap_or(i64::MAX);
        if self.lifecycle.last_news.is_none_or(|t| now - t >= every) {
            self.lifecycle.last_news = Some(now);
            let cutoff = now - i64::try_from(after.as_secs()).unwrap_or(0);
            let archive_name = tr!("lifecycle-archive-folder");
            if !self.backend.send(Command::Newsletters { cutoff, archive_name }) {
                return None;
            }
        }
        self.lifecycle.last_news.map(|t| t.saturating_add(every))
    }

    /// Один таймер к ближайшему сроку; срок тот же — таймер прежний.
    fn arm_lifecycle(&mut self, next: Option<i64>, now: i64, cx: &mut Context<Self>) {
        if next == self.lifecycle.armed {
            return;
        }
        self.lifecycle.armed = next;
        self.lifecycle.timer += 1;
        let Some(at) = next else { return };
        let id = self.lifecycle.timer;
        // Секунда запаса: срок «в 09:00» — уже наступил, когда таймер сработал.
        let wait = Duration::from_secs(u64::try_from(at - now).unwrap_or(0).saturating_add(1));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |m, cx| {
                if m.lifecycle.timer == id {
                    m.lifecycle.armed = None;
                    m.lifecycle_pass(true, cx);
                }
            });
        })
        .detach();
    }

    /// События синхронизации: почта изменилась — проход; рассылки перенесены — тост.
    pub(crate) fn on_lifecycle_sync(&mut self, ev: &postman_core::sync::Event, cx: &mut Context<Self>) {
        use postman_core::sync::{Event, Status};
        match ev {
            Event::EmailsChanged | Event::MailboxesChanged | Event::Status(Status::Synced) => self.lifecycle_pass(false, cx),
            Event::Newsletters { archived } if *archived > 0 => {
                let text = tr!("lifecycle-newsletters-toast", n = *archived);
                self.toasts.update(cx, |t, cx| t.push(Toast::Info, text, cx));
            }
            _ => {}
        }
    }

    /// «Открыть» на карточке напоминания: письмо в области чтения (и прочитано).
    fn open_reminded(&mut self, id: String, cx: &mut Context<Self>) {
        self.lifecycle.cards.retain(|(i, _)| *i != id);
        cx.notify();
        let Some(row) = self.cache.rows_by_ids(std::slice::from_ref(&id)).ok().and_then(|mut r| r.pop()) else { return };
        self.set_section(crate::Section::Mail, cx);
        self.list.update(cx, |l, cx| l.open_row(row, cx));
    }

    /// Панель выбора поверх области чтения (у шапки письма с кнопками сроков).
    pub(crate) fn lifecycle_overlay(&self, l: &Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = self.lifecycle.picker.as_ref()?;
        let now = Zoned::now();
        let kind = p.kind;
        let mut panel = div()
            .id("lifecycle-picker")
            .occlude()
            .w(px(280.))
            .flex()
            .flex_col()
            .p(px(4.))
            .rounded(l.px(Metric::RadiusOverlay))
            .border_1()
            .border_color(l.c(Role::StrokeCard))
            .bg(l.c(Role::LayerOverlay))
            .text_color(l.text())
            .text_size(px(13.))
            .shadow_lg()
            .child(div().px(px(10.)).py(px(4.)).text_size(px(12.)).text_color(l.text2()).truncate().child(match p.rest.len() {
                0 => format!("{} · {}", picker_title(kind), p.row.subject),
                n => format!("{} · {}", picker_title(kind), tr!("bulk-letters", n = n + 1)),
            }));
        for (i, pick) in lifecycle::picks(kind).into_iter().enumerate() {
            let when = pick.when(kind, &now);
            let hint = when.as_ref().map(when_hint).unwrap_or_default();
            panel = panel.child(
                menu_item(ElementId::NamedInteger("lifecycle-pick".into(), i as u64), l)
                    .child(div().flex_1().child(pick_text(pick, kind)))
                    .child(div().text_color(l.text2()).child(hint))
                    .on_click(cx.listener(move |this, _, _, cx| this.choose(when, cx))),
            );
        }
        let date_text = if matches!(kind, Kind::Wait) { tr!("lifecycle-pick-date") } else { tr!("lifecycle-pick-date-time") };
        match &p.input {
            None => {
                panel = panel.child(
                    menu_item("lifecycle-pick-date".into(), l)
                        .child(date_text)
                        .on_click(cx.listener(|this, _, window, cx| this.pick_date(window, cx))),
                )
            }
            Some(input) => {
                let error = p.error.map(|e| match e {
                    Bad::Format => tr!("lifecycle-bad-date"),
                    Bad::Past => tr!("lifecycle-past"),
                });
                panel = panel.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .px(px(10.))
                        .py(px(6.))
                        .child(
                            div().flex().items_center().gap(px(6.)).child(div().flex_1().child(Input::new(input))).child(
                                div()
                                    .id("lifecycle-set")
                                    .px(px(10.))
                                    .h(px(28.))
                                    .flex()
                                    .items_center()
                                    .rounded(l.px(Metric::RadiusControl))
                                    .bg(l.accent())
                                    .text_color(l.on_accent())
                                    .cursor_pointer()
                                    .child(tr!("lifecycle-set"))
                                    .on_click(cx.listener(|this, _, _, cx| this.pick_typed(cx))),
                            ),
                        )
                        .children(error.map(|e| div().text_size(px(12.)).text_color(l.error()).child(e))),
                );
            }
        }
        let has = std::iter::once(&p.row)
            .chain(&p.rest)
            .any(|r| postman_core::marks::has(&r.keywords, kind) || (kind == Kind::Expire && !r.has_keyword(lifecycle::KEEP)));
        if has {
            panel = panel.child(div().h(px(1.)).my(px(4.)).bg(l.border())).child(
                menu_item("lifecycle-remove".into(), l)
                    .text_color(l.text2())
                    .child(remove_text(kind))
                    .on_click(cx.listener(|this, _, _, cx| this.choose(None, cx))),
            );
        }
        let panel = panel.on_mouse_down_out(cx.listener(|this, _, _, cx| {
            this.lifecycle.picker = None;
            cx.notify();
        }));
        Some(div().absolute().top(px(176.)).right(px(28.)).child(panel).into_any_element())
    }

    /// Карточки «Напоминание: тема · Открыть · ×» внизу экрана, над окном «Отменить» отправки.
    pub(crate) fn reminder_cards(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.lifecycle.cards.is_empty() {
            return None;
        }
        let l = crate::roles::look(cx);
        let mut col = div().flex().flex_col().items_center().gap(px(6.));
        for (i, (id, subject)) in self.lifecycle.cards.iter().enumerate() {
            let (open, close) = (id.clone(), id.clone());
            col = col.child(
                div()
                    .id(("reminder-card", i))
                    .occlude()
                    .flex()
                    .items_center()
                    .gap_3()
                    .max_w(px(560.))
                    .px_4()
                    .py_2()
                    .rounded_md()
                    .shadow_lg()
                    .border_1()
                    .border_color(l.c(Role::StrokeCard))
                    .bg(l.c(Role::LayerOverlay))
                    .text_color(l.text())
                    .child(icon(glyph::RINGER, 14.).text_color(l.text2()))
                    .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).child(tr!("lifecycle-reminder")))
                    .child(div().min_w_0().truncate().child(subject.clone()))
                    .child(
                        div()
                            .id(("reminder-open", i))
                            .px_2()
                            .rounded_md()
                            .cursor_pointer()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(l.accent_text())
                            .hover(|d| d.bg(l.hover()))
                            .child(tr!("lifecycle-open"))
                            .on_click(cx.listener(move |this, _, _, cx| this.open_reminded(open.clone(), cx))),
                    )
                    .child(
                        div()
                            .id(("reminder-close", i))
                            .size(px(20.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(10.))
                            .cursor_pointer()
                            .hover(|d| d.bg(l.hover()))
                            .child(icon(glyph::CLOSE, 9.))
                            .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tr!("lifecycle-dismiss")).build(window, cx))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.lifecycle.cards.retain(|(x, _)| *x != close);
                                cx.notify();
                            })),
                    ),
            );
        }
        let bottom = if self.undo_shown() { 84. } else { 36. };
        Some(div().absolute().left_0().right_0().bottom(px(bottom)).flex().justify_center().child(col).into_any_element())
    }
}

/// Опись части (ADR 0005, `parts.rs`).
pub(crate) const PART: crate::part::Part = crate::part::Part { name: "lifecycle", texts: TEXTS, scenes: scenes::SCENES, ..crate::part::Part::NONE };

/// Тексты части (`i18n/<язык>/<часть>.ftl`).
const TEXTS: &[pets_i18n::Resource] = &[("ru", include_str!("../i18n/ru/lifecycle.ftl")), ("en", include_str!("../i18n/en/lifecycle.ftl"))];

/// Сцены `--screens` сроков (ADR 0005).
pub(crate) mod scenes {
    use gpui_kit::*;
    use postman_core::lifecycle;
    use postman_core::marks::{Kind, When};

    use crate::screens::{self, Scene};
    use crate::{Mailer, Section};

    pub(crate) const SCENES: &[Scene] = &[
        // Жизненный цикл (S5-04): «Входящие» со всеми метками сроков (срок впереди, сегодня к часу, ждём ответа,
        // напомнит, исчезнет, просрочено — наверху, «Срочное») и открытая панель «Срок» у первого письма.
        Scene::new("lifecycle", marks, ready),
        // Письмо с просроченным сроком открыто: плотная янтарная метка в строке и в шапке.
        Scene::new("overdue", overdue, ready).continues(),
    ];

    /// Первым шести письмам «Входящих» — сроки всех видов (от «сейчас»), как поставили бы действия «Срок…»,
    /// «Напомнить…», «Ждать ответа…», «Исчезнет…»; у первого письма открыта панель «Срок». Снимаются сценой
    /// `firstrun` (`Driver::undo`).
    fn marks(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.set_section(Section::Mail, cx);
        m.reset_filters(cx);
        m.filters.update(cx, |f, cx| f.set(cx, |f| f.quick_open = false));
        let inbox = crate::live::inbox(&m.folders.read(cx).mailboxes).ok_or("no inbox")?;
        m.open_mailbox(inbox, cx);
        let rows: Vec<postman_cache::EmailRow> = m.list.read(cx).rows.1.iter().take(6).cloned().collect();
        if rows.len() < 6 {
            return Err("fewer than 6 letters in the demo inbox".into());
        }
        let now = jiff::Zoned::now();
        let tz = now.time_zone().clone();
        let ts = now.timestamp().as_second();
        let at = |secs: i64| lifecycle::when_at(ts + secs, &tz).ok_or("time out of range");
        let day = |days: i64| now.date().checked_add(jiff::Span::new().days(days)).map_err(|e| e.to_string());
        let remind_day = day(3)?;
        let marks: [(Kind, When); 6] = [
            (Kind::Due, When::day(day(2)?)),
            (Kind::Due, at(3 * 3600)?),
            (Kind::Wait, When::day(day(2)?)),
            (Kind::Remind, When::at(remind_day, jiff::civil::Time::new(9, 0, 0, 0).map_err(|e| e.to_string())?)),
            (Kind::Expire, at(3 * 86_400)?),
            (Kind::Due, When::day(day(-1)?)),
        ];
        let mut patches = Vec::new();
        let mut undo = Vec::new();
        for (row, (kind, when)) in rows.iter().zip(marks) {
            let p = lifecycle::set(row, kind, Some(when));
            let added: Vec<(String, bool)> = p.keywords.iter().filter(|(_, on)| *on).map(|(k, _)| (k.clone(), false)).collect();
            undo.push(lifecycle::Patch { id: row.id.clone(), keywords: added, mailboxes: Vec::new() });
            patches.push(p);
        }
        // «Напомнить» — словом без переноса в «Архив»: письмо остаётся в списке со своей меткой.
        if let Some(d) = &mut m.screens {
            d.undo = undo;
            d.overdue = Some(rows[5].id.clone());
        }
        m.apply_patches(patches, cx);
        m.open_letter(rows[0].clone(), cx);
        m.lifecycle.picker = Some(super::Picker::new(Kind::Due, rows[0].clone()));
        Ok(())
    }

    fn overdue(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.close_picker(cx);
        let id = m.screens.as_ref().and_then(|d| d.overdue.clone());
        let row = id.and_then(|id| m.cache.rows_by_ids(&[id]).ok()).and_then(|mut r| r.pop());
        let Some(row) = row else { return Err("no overdue letter".into()) };
        m.list.update(cx, |l, cx| l.select(0, cx));
        m.open_letter(row, cx);
        Ok(())
    }

    fn ready(s: &Scene, m: &Mailer, cx: &App) -> bool {
        let list = m.list.read(cx);
        let now = jiff::Zoned::now();
        let (_, rows) = &list.rows;
        let marked = rows.iter().filter(|r| !super::row_badges(r, &now).is_empty()).count();
        let want = m.screens.as_ref().and_then(|d| d.overdue.as_deref());
        let top = list.row(0).map(|r| r.id.as_str()) == want;
        let lifecycle = s.name == "lifecycle";
        let open = screens::reading(m, cx).is_some_and(|r| r.doc.is_some() && (lifecycle || Some(r.row.id.as_str()) == want));
        let picker = m.lifecycle.picker.is_some() == lifecycle;
        marked >= 6 && top && list.view.is_some() && !list.computing && screens::rows_loaded(list) && open && picker
    }
}

#[cfg(test)]
mod tests {
    use super::badge_text_in;
    use jiff::civil::{Date, Time};
    use pets_i18n::I18n;
    use postman_core::lifecycle::{Ahead, Badge, Day, Span};

    fn date(s: &str) -> Date {
        s.parse().unwrap()
    }

    /// Тексты меток (DESIGN «Строка письма и метки»): с заглавной только первое слово.
    #[test]
    fn badge_texts() {
        let (ru, problems) = I18n::new(&crate::parts::texts(), "ru");
        assert!(problems.is_empty(), "{problems:?}");
        let badge_text = |b: &Badge| badge_text_in(&ru, b);
        let t = |h, m| Some(Time::new(h, m, 0, 0).unwrap());
        let ahead = |day, time| Badge::Due(Some(Ahead { day, time }));
        assert_eq!(badge_text(&ahead(Day::Weekday(jiff::civil::Weekday::Thursday), None)), "До чт");
        assert_eq!(badge_text(&ahead(Day::Today, t(15, 0))), "До 15:00");
        assert_eq!(badge_text(&ahead(Day::Today, None)), "Сегодня");
        assert_eq!(badge_text(&ahead(Day::Tomorrow, None)), "До завтра");
        assert_eq!(badge_text(&ahead(Day::Date(date("2026-11-24")), t(9, 0))), "До 24.11");
        assert_eq!(badge_text(&Badge::Overdue(Span::Days(1))), "Просрочено 1 д");
        assert_eq!(badge_text(&Badge::Overdue(Span::Hours(3))), "Просрочено 3 ч");
        assert_eq!(badge_text(&Badge::Waiting(Some(2))), "Ждём ответа · 2 д");
        assert_eq!(badge_text(&Badge::Waiting(Some(0))), "Ждём ответа · сегодня");
        assert_eq!(badge_text(&Badge::NoReply(1)), "Нет ответа · 1 д");
        let remind = Badge::Remind(Ahead { day: Day::Weekday(jiff::civil::Weekday::Thursday), time: t(9, 0) });
        assert_eq!(badge_text(&remind), "Напомнит чт 09:00");
        assert_eq!(badge_text(&Badge::Remind(Ahead { day: Day::Today, time: t(18, 0) })), "Напомнит 18:00");
        assert_eq!(badge_text(&Badge::Expires(Some(Span::Days(3)))), "Исчезнет через 3 д");
        assert_eq!(badge_text(&Badge::Expires(Some(Span::Hours(5)))), "Исчезнет через 5 ч");
        let (en, _) = I18n::new(&crate::parts::texts(), "en");
        assert_eq!(badge_text_in(&en, &Badge::Overdue(Span::Days(1))), "Overdue 1 d");
        assert_eq!(badge_text_in(&en, &ahead(Day::Weekday(jiff::civil::Weekday::Thursday), None)), "Due Thu");
    }
}
