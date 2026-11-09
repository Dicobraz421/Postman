//! Календари коллег, открытые из справочника (T2-02, макет `Calendar` — «Открыть календарь коллеги»): как в Outlook
//! «Открыть календарь → из адресной книги». Коллега не обязан открывать календарь сам: показываем то, что разрешает
//! сервер, — занятость запросом занятости (`Command::Availability`: Exchange — `GetUserAvailability`, JMAP —
//! `Principal/getAvailability`, CalDAV — free-busy). Сервер не дал данных — строка «Занятость скрыта» (это не «свободен»).
//! Список — в настройках (`Prefs::calendar.opened_calendars`), между запусками остаётся.
//!
//! В модели календаря такой календарь — как календарь коллеги: `Owner` с адресом даёт цвет и скрытие; события —
//! блоки занятости только для чтения (ключ события `fb:…`).

use gpui_kit::*;
use pets_i18n::tr;
use postman_calendar::{Busy, BusyKind, BusyNote, Event, EventStatus, FreeBusy, Owner, Span};
use postman_core::prefs::OpenedCalendar;

use super::{CalendarAction, CalendarView};

/// Ключ блока занятости в `Event::id`; у блока с темой и местом от сервера — ещё `NOTED`.
const PREFIX: &str = "fb:";
const NOTED: &str = "n:";
/// Подсказок под полем выбора — не больше.
const SUGGESTIONS: usize = 6;
/// Подсказки — с этой длины набранного.
const SUGGEST_FROM: usize = 2;

/// Открытый календарь на экране.
pub(super) struct Opened {
    pub(super) email: String,
    pub(super) name: String,
    /// Занятость показанного периода; `None` — сервер не дал (нет доступа, не найден) или ещё не пришла.
    busy: Option<Vec<Busy>>,
    /// Тема и место интервалов, где сервер их дал (T2-02б).
    notes: Vec<BusyNote>,
    /// Ответ сервера пришёл: `busy: None` теперь значит «нет доступа».
    pub(super) answered: bool,
}

impl Opened {
    fn new(c: OpenedCalendar) -> Self {
        Self { email: c.email, name: c.name, busy: None, notes: Vec::new(), answered: false }
    }

    pub(super) fn key(&self) -> String {
        self.email.to_lowercase()
    }

    fn owner(&self) -> Owner {
        Owner { email: self.email.clone(), name: self.shown_name() }
    }

    pub(super) fn shown_name(&self) -> String {
        if self.name.trim().is_empty() { self.email.clone() } else { self.name.clone() }
    }

    /// Занятость скрыта: сервер ответил без данных.
    pub(super) fn denied(&self) -> bool {
        self.answered && self.busy.is_none()
    }
}

/// Выбор сотрудника: поле, подсказки справочника, подсветка, ошибка ввода.
#[derive(Default)]
pub(super) struct Picker {
    pub(super) open: bool,
    pub(super) suggest: Vec<(String, String)>,
    pub(super) cursor: usize,
    pub(super) bad: bool,
}

/// Блок занятости открытого календаря (только для чтения, без подробностей).
pub(crate) fn is_busy_block(e: &Event) -> bool {
    e.id.starts_with(PREFIX)
}

/// У блока есть тема или место от сервера (T2-02б).
pub(crate) fn has_note(e: &Event) -> bool {
    e.id.strip_prefix(PREFIX).is_some_and(|r| r.starts_with(NOTED))
}

fn title(kind: BusyKind) -> String {
    match kind {
        BusyKind::Busy => tr!("cal-fb-busy"),
        BusyKind::Tentative => tr!("cal-fb-tentative"),
        BusyKind::Unavailable => tr!("cal-fb-away"),
    }
}

fn event_of(o: &Opened, b: &Busy) -> Event {
    let note = o.notes.iter().find(|n| n.span == b.span);
    Event {
        time_zone: None,
        private: false,
        recurrence: None,
        alert_secs: None,
        id: format!("{PREFIX}{}{}-{}-{}", if note.is_some() { NOTED } else { "" }, o.key(), b.span.start, b.span.end),
        uid: format!("{PREFIX}{}-{}", o.key(), b.span.start),
        calendar_ids: Vec::new(),
        title: note.map(|n| n.subject.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| title(b.kind)),
        description: String::new(),
        location: note.map(|n| n.location.clone()).unwrap_or_default(),
        span: b.span,
        all_day: false,
        status: if b.kind == BusyKind::Tentative { EventStatus::Tentative } else { EventStatus::Confirmed },
        free_busy: FreeBusy::Busy,
        participants: Vec::new(),
        recurring: false,
        keywords: Vec::new(),
        sequence: 0,
        owner: Some(o.owner()),
    }
}

impl CalendarView {
    /// Открытые календари из настроек (при создании и после перехода на живой ящик).
    pub(super) fn load_opened_prefs(&mut self, cx: &mut Context<Self>) {
        self.opened = crate::prefs::get(cx).calendar.opened_calendars.into_iter().map(Opened::new).collect();
    }

    /// Занятость открытых календарей за показанный период: сервер — запросом, демо — демо-коллеги.
    pub(super) fn ask_opened(&mut self, cx: &mut Context<Self>) {
        let shared: Vec<String> = self.server_colleagues();
        let emails: Vec<String> = self.opened.iter().filter(|o| !shared.contains(&o.key())).map(|o| o.email.clone()).collect();
        for o in &mut self.opened {
            o.busy = None;
            o.answered = false;
        }
        if emails.is_empty() {
            self.apply_subs(cx);
            return;
        }
        let (from, to) = self.range();
        if !self.live {
            let demo = postman_demo::calendar::colleagues();
            let span = Span::new(from, to);
            for o in &mut self.opened {
                o.answered = true;
                o.busy = demo
                    .iter()
                    .find(|(_, mail, _)| mail.eq_ignore_ascii_case(&o.email))
                    .map(|(_, _, b)| b.iter().filter(|x| x.span.start < span.end && x.span.end > span.start).copied().collect());
            }
            self.apply_subs(cx);
            return;
        }
        let ticket = crate::meeting::next_ticket();
        self.opened_ticket = Some(ticket);
        cx.emit(CalendarAction::Busy { ticket, emails, from, to });
    }

    /// Ответ занятости: свой тикет — разложить по календарям.
    pub(super) fn opened_answer(&mut self, people: &[(String, Option<Vec<Busy>>)], notes: &[(String, Vec<BusyNote>)], cx: &mut Context<Self>) {
        self.opened_ticket = None;
        for o in &mut self.opened {
            o.answered = true;
            o.busy = people.iter().find(|(e, _)| e.eq_ignore_ascii_case(&o.email)).and_then(|(_, b)| b.clone());
            o.notes = notes.iter().find(|(e, _)| e.eq_ignore_ascii_case(&o.email)).map(|(_, n)| n.clone()).unwrap_or_default();
        }
        self.apply_subs(cx);
    }

    /// Запрос не ушёл или сервер ответил ошибкой: всё — «нет данных».
    pub(super) fn opened_failed(&mut self, cx: &mut Context<Self>) {
        self.opened_ticket = None;
        for o in &mut self.opened {
            o.answered = true;
            o.busy = None;
        }
        self.apply_subs(cx);
    }

    /// Ключи коллег, которых дал сервер (открыли мне календарь сами): им запрос занятости не нужен.
    fn server_colleagues(&self) -> Vec<String> {
        self.parts.iter().flat_map(|(_, _, owners)| owners.iter().map(Owner::key)).collect()
    }

    /// Блоки и владельцы открытых календарей — в события и календари вида (вызывает `apply_subs`).
    pub(super) fn mix_opened(&mut self) {
        self.events.retain(|e| !is_busy_block(e));
        let shared = self.server_colleagues();
        let mine: Vec<&Opened> = self.opened.iter().filter(|o| !shared.contains(&o.key())).collect();
        self.events.extend(mine.iter().flat_map(|o| o.busy.iter().flatten().map(move |b| event_of(o, b))));
        let keys: Vec<String> = self.opened.iter().map(Opened::key).collect();
        self.colleagues.retain(|c| !keys.contains(&c.key()) || shared.contains(&c.key()));
        self.colleagues.extend(mine.iter().map(|o| o.owner()));
    }

    /// Календарь `key` открыт из справочника (не сервером): своя строка слева.
    pub(super) fn is_opened(&self, key: &str) -> bool {
        self.opened.iter().any(|o| o.key() == key) && !self.server_colleagues().iter().any(|k| k == key)
    }

    /// Открыть календарь сотрудника. Уже открыт (мной или сервером) — `false`.
    pub(crate) fn open_colleague(&mut self, name: &str, email: &str, cx: &mut Context<Self>) -> bool {
        let key = email.trim().to_lowercase();
        if key.is_empty() || self.opened.iter().any(|o| o.key() == key) || self.colleagues.iter().any(|c| c.key() == key) {
            return false;
        }
        let c = OpenedCalendar { email: email.trim().to_owned(), name: name.trim().to_owned() };
        let saved = c.clone();
        crate::prefs::update(cx, move |p| p.calendar.opened_calendars.push(saved));
        self.opened.push(Opened::new(c));
        self.hidden.retain(|k| *k != key);
        self.ask_opened(cx);
        true
    }

    /// Закрыть открытый календарь: блоки уходят, цвет освобождается.
    pub(super) fn close_colleague(&mut self, key: &str, cx: &mut Context<Self>) {
        self.opened.retain(|o| o.key() != key);
        let k = key.to_owned();
        crate::prefs::update(cx, move |p| {
            p.calendar.opened_calendars.retain(|o| o.email.to_lowercase() != k);
            p.calendar.calendar_colors.remove(&k);
        });
        self.color_menu = None;
        self.colleagues.retain(|c| c.key() != key);
        self.apply_subs(cx);
    }

    /// Набранное в поле выбора: подсказки справочника (и поиск на сервере — у Exchange справочник только поиском).
    pub(super) fn pick_typed(&mut self, text: &str, cx: &mut Context<Self>) {
        self.picker.bad = false;
        self.picker.cursor = 0;
        self.picker.suggest.clear();
        let text = text.trim();
        if text.chars().count() < SUGGEST_FROM {
            cx.notify();
            return;
        }
        if let Some(people) = cx.try_global::<crate::shared::People>() {
            let now = jiff::Timestamp::now().as_second();
            let book = people.book.clone();
            self.picker.suggest = book.borrow().suggest(text, SUGGESTIONS, now).into_iter().map(|e| (e.name.clone(), e.email.clone())).collect();
        }
        cx.emit(CalendarAction::FindPeople(text.to_owned()));
        cx.notify();
    }

    /// Справочник ответил: подсказки — заново по тому же тексту.
    pub(super) fn pick_refresh(&mut self, cx: &mut Context<Self>) {
        if self.picker.open {
            let text = self.pick_input.read(cx).value().to_string();
            let cursor = self.picker.cursor;
            self.pick_typed(&text, cx);
            self.picker.cursor = cursor.min(self.picker.suggest.len().saturating_sub(1));
        }
    }

    /// Enter или щелчок: подсказка `n` или набранный адрес.
    pub(super) fn pick_accept(&mut self, n: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = n.or((!self.picker.suggest.is_empty()).then_some(self.picker.cursor)).and_then(|i| self.picker.suggest.get(i).cloned());
        let (name, email) = match chosen {
            Some(p) => p,
            None => {
                let typed = self.pick_input.read(cx).value().to_string();
                match postman_compose::parse_addresses(&typed).0.into_iter().next().filter(|a| a.email.contains('@')) {
                    Some(a) => (a.name, a.email),
                    None => {
                        self.picker.bad = true;
                        cx.notify();
                        return;
                    }
                }
            }
        };
        if !self.open_colleague(&name, &email, cx) {
            cx.emit(CalendarAction::Toast(pets_ui::Level::Info, tr!("cal-open-already", name = if name.is_empty() { email.clone() } else { name })));
        }
        self.pick_close(window, cx);
    }

    pub(super) fn pick_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = Picker::default();
        self.pick_input.update(cx, |s, cx| s.set_value("", window, cx));
        cx.notify();
    }

    pub(super) fn pick_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        if !self.picker.suggest.is_empty() {
            self.picker.cursor = self.picker.cursor.saturating_add_signed(delta).min(self.picker.suggest.len() - 1);
            cx.notify();
        }
    }

    /// Открытые календари: имя и «занятость скрыта» (сцены).
    pub(crate) fn opened_state(&self) -> Vec<(String, bool)> {
        self.opened.iter().map(|o| (o.shown_name(), o.denied())).collect()
    }

    /// Блоков занятости в показанном периоде (сцены).
    pub(crate) fn busy_blocks(&self) -> usize {
        self.events.iter().filter(|e| is_busy_block(e)).count()
    }

    /// Открыть подсказки выбора с текстом (сцены).
    pub(crate) fn pick_with(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.picker.open = true;
        self.pick_input.update(cx, |s, cx| s.set_value(text.to_owned(), window, cx));
        self.pick_typed(text, cx);
    }

    /// Сцена: тема и место у блоков открытого календаря `email`, как их даёт Exchange (вид `Detailed`).
    pub(crate) fn demo_notes(&mut self, email: &str, cx: &mut Context<Self>) {
        for o in self.opened.iter_mut().filter(|o| o.email.eq_ignore_ascii_case(email)) {
            o.notes = o
                .busy
                .iter()
                .flatten()
                .take(2)
                .map(|b| BusyNote { span: b.span, subject: "Аудит доступа".into(), location: "Переговорная 2".into() })
                .collect(); // i18n-ok: демо-данные
        }
        self.apply_subs(cx);
        self.selected = self.events.iter().position(|e| is_busy_block(e) && !e.location.is_empty());
        cx.notify();
    }

    /// Сцена: открыть календари `list` (имя, адрес) заново и показать первый блок занятости в подробностях.
    pub(crate) fn open_demo_colleagues(&mut self, list: &[(&str, &str)], cx: &mut Context<Self>) {
        for key in self.opened.iter().map(Opened::key).collect::<Vec<_>>() {
            self.close_colleague(&key, cx);
        }
        for (name, email) in list {
            self.open_colleague(name, email, cx);
        }
        self.selected = self.events.iter().position(is_busy_block);
        cx.notify();
    }

    /// Подсказок под полем выбора (сцены).
    pub(crate) fn pick_suggestions(&self) -> usize {
        self.picker.suggest.len()
    }
}

#[cfg(test)]
mod tests {
    use super::{Busy, BusyKind, BusyNote, EventStatus, Opened, OpenedCalendar, Span, event_of, is_busy_block};

    /// Блок занятости — только для чтения, владелец — коллега, «под вопросом» — статусом события.
    #[test]
    fn busy_blocks_belong_to_the_colleague() {
        let o = Opened::new(OpenedCalendar { email: "M.Ivanova@corp.example".into(), name: String::new() });
        let e = event_of(&o, &Busy { span: Span::new(10, 20), kind: BusyKind::Tentative });
        assert!(is_busy_block(&e));
        assert_eq!(e.status, EventStatus::Tentative);
        assert_eq!(e.owner.as_ref().map(|w| (w.key(), w.name.clone())), Some(("m.ivanova@corp.example".into(), "M.Ivanova@corp.example".into())));
        assert!(e.participants.is_empty() && e.description.is_empty());
        assert!(!o.denied(), "ответа ещё нет — не «скрыта»");
        // Тема и место от сервера (Exchange `Detailed`) — в блоке вместо «Занят».
        let mut o = o;
        o.notes = vec![BusyNote { span: Span::new(10, 20), subject: "Планёрка".into(), location: "Переговорная 1".into() }];
        let e = event_of(&o, &Busy { span: Span::new(10, 20), kind: BusyKind::Busy });
        assert_eq!((e.title.as_str(), e.location.as_str()), ("Планёрка", "Переговорная 1"));
    }
}
