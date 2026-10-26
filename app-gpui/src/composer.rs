//! View письма (S5-06б, DESIGN «Новое письмо»): вкладка (`Place::Tab`) — слева письмо, справа «Перед отправкой»,
//! вложения и «Отправить позже»; ответ в области чтения (`Place::Inline`) — то же в одну колонку, «Развернуть» — во
//! вкладку. Модель — `compose::Model`; экрану — события [`ComposerEvent`] и команды через шов `Backend::send`.
//!
//! Адресаты — чипами: готовые адреса — чипы (с меткой «Внешний»), в поле ввода — только набираемый адрес;
//! «;», «,» и Enter делают его чипом, Backspace в пустом поле возвращает последний чип в поле.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use gpui_kit::component::input::{
    Backspace, Editor, EditorState, Enter, IndentInline, Input, InputEvent, InputState, MoveDown, MoveUp, TextDecoration, TextDecorationCollection,
};
use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::{Metric, Role};
use postman_compose::{Address, Attachment, Draft, parse_addresses};
use postman_core::checks::{self, Check, Secret};
use postman_core::marks;
use postman_core::prefs::AccountPrefs;
use postman_core::send_later::{self, Choice, Reject};
use postman_core::sync::{Command, Event, Sender};

use crate::Mailer;
use crate::chrome::{glyph, icon};
use crate::compose::compose;
use crate::compose::{
    Book, CloseAnswer, CloseStep, Field, Format, Model, Purpose, SentDraft, Status, accept_suggestion, apply_format, compose_field, current_token,
    next_ticket, segments, size_text,
};
use crate::roles::{Look, Pm};

/// Подсказок адреса под полем.
const SUGGESTIONS: usize = 6;
/// Подсказки — с этого числа набранных букв.
const SUGGEST_FROM: usize = 2;
/// Колонка подписей полей и правая колонка вкладки (макет «Новое письмо»).
const LABEL_W: f32 = 64.0;
const W_SIDE: f32 = 360.0;
const ROW_H: f32 = 40.0;

/// Где письмо.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Place {
    /// Вкладка в заголовке окна.
    Tab,
    /// Ответ в области чтения над письмом.
    Inline,
}

/// Экрану: закрыть письмо (отправлено, «Нет», удалён черновик) или развернуть ответ во вкладку.
pub(crate) enum ComposerEvent {
    Close,
    Expand,
}

impl EventEmitter<ComposerEvent> for Composer {}

/// Что письму нужно от экрана при открытии.
pub(crate) struct Open {
    /// Сертификаты шифрования адресатов (S5-12 Ф4).
    pub(crate) certs: crate::smime_seal::Certs,
    /// Ответ на письмо общего ящика (S5-28): предупреждение у «От» и подсказки меню адресов.
    pub(crate) shared: Option<crate::shared::Reply>,
    /// Свои адреса с сертификатом подписи (S5-12 Ф3).
    pub(crate) signable: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    pub(crate) me: Option<(Address, String)>,
    pub(crate) senders: Vec<Sender>,
    pub(crate) max_delay: u64,
    pub(crate) book: Book,
    pub(crate) mailer: WeakEntity<Mailer>,
    /// Категории аккаунта: ключевое слово, имя, цвет Fluent.
    pub(crate) categories: Vec<(String, String, &'static str)>,
    pub(crate) place: Place,
    pub(crate) screens: bool,
    /// Подписи учётной записи (S5-09): подпись по умолчанию — вместо подписи Identity, все — в меню «Подпись».
    pub(crate) prefs: AccountPrefs,
}

/// Подсказки адреса под активным полем.
#[derive(Default)]
struct Suggest {
    field: Option<Field>,
    items: Vec<String>,
    selected: usize,
}

/// «Отправить позже» (S5-06): меню у кнопки «Отправить», список справа и строка выбора даты и времени.
struct Later {
    /// Сколько сервер держит письма (секунды); 0 — «Отправить позже» нет.
    max_delay: u64,
    menu: bool,
    picking: bool,
    date: Entity<InputState>,
    time: Entity<InputState>,
    error: Option<Reject>,
}

/// Открытое меню письма.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Menu {
    None,
    From,
    Template,
    Signature,
    Mark,
    Category,
}

pub(crate) struct Composer {
    /// Постоянный номер письма (вкладка `TabKind::Compose(id)`).
    pub(crate) id: u64,
    pub(crate) model: Model,
    place: Place,
    /// Выбор «От» (S4-06).
    senders: Vec<Sender>,
    /// Сертификаты шифрования адресатов (S5-12 Ф4) и «Снять „Конфиденциально“» ждёт подтверждения.
    certs: crate::smime_seal::Certs,
    confirm_unmark: bool,
    /// Ответ на письмо общего ящика (S5-28).
    shared: Option<crate::shared::Reply>,
    to: Entity<InputState>,
    cc: Entity<InputState>,
    bcc: Entity<InputState>,
    subject: Entity<InputState>,
    /// Текст письма — поле с живым отображением Markdown (S5-27): разметка — `postman_compose::display`, оформление —
    /// украшения поля ([`Composer::paint_markdown`]).
    body: Entity<EditorState>,
    /// Украшения текста письма и тема, в которой они посчитаны (сменилась — заново).
    marks: Option<TextDecorationCollection>,
    marks_look: Option<Rc<crate::roles::Look>>,
    focus: FocusHandle,
    mailer: WeakEntity<Mailer>,
    book: Book,
    suggest: Suggest,
    /// Таймер автосохранения запущен.
    autosave_armed: bool,
    /// Текст модели поменялся не от ввода (подпись, маска, сцена снимка) — переписать поля при отрисовке.
    pub(crate) sync_inputs: bool,
    show_cc: bool,
    show_bcc: bool,
    menu: Menu,
    categories: Vec<(String, String, &'static str)>,
    /// Свои домены (адреса отправителя): чужие получатели — «Внешний».
    own: Vec<String>,
    /// Слепок адресатов и метки, по которому посчитаны замечания по адресатам.
    recipients_key: u64,
    /// Адреса для поиска сертификатов (S5-12 Ф4) — отдаются экрану почты после отрисовки.
    resolve_pending: Option<(Vec<String>, String)>,
    later: Later,
    /// Подписи учётной записи (Настройки → Подписи и шаблоны).
    prefs: AccountPrefs,
    /// `--screens`: сколько раз письмо нарисовано.
    screens: Option<u32>,
    _subscriptions: Vec<Subscription>,
}

static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Свои домены: домены адресов «От».
fn own_domains(senders: &[Sender], me: Option<&Address>) -> Vec<String> {
    let mut out: Vec<String> = senders.iter().map(|s| &s.address).chain(me).filter_map(|a| checks::domain(&a.email)).collect();
    out.sort();
    out.dedup();
    out
}

impl Composer {
    pub(crate) fn new(draft: Draft, open: Open, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (from, signature) = open.me.clone().map_or((None, String::new()), |(a, s)| (Some(a), s));
        let mut model = Model::new(draft, from, &signature, Instant::now());
        // «Подписать» — по «Подписывать все письма», если для адреса «От» есть сертификат (решение владельца №7).
        let signable = open.signable.lock().unwrap_or_else(std::sync::PoisonError::into_inner).contains(&model.from().email.to_lowercase());
        model.sign = crate::prefs::get(cx).security.sign_all && signable;
        // Демо: Identity нет — «От» — первый адрес списка.
        if model.from().email.is_empty()
            && let Some(first) = open.senders.first()
        {
            model.set_from(first.address.clone());
        }
        default_signature(&mut model, &open.prefs, open.place == Place::Inline);
        let mut line = |value: String, placeholder: &str, cx: &mut Context<Self>| {
            let placeholder = tr!(placeholder);
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(value))
        };
        let open_part = |f: Field| segments(model.field(f)).1;
        let to = line(open_part(Field::To), "compose-to-placeholder", cx);
        let cc = line(open_part(Field::Cc), "compose-cc-placeholder", cx);
        let bcc = line(open_part(Field::Bcc), "compose-bcc-placeholder", cx);
        let subject = line(model.subject.clone(), "compose-subject-placeholder", cx);
        let date = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("send-later-date-placeholder")));
        let time = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("send-later-time-placeholder")));
        let later = Later { max_delay: open.max_delay, menu: false, picking: false, date, time, error: None };
        let body_text = model.body.clone();
        // Поле редактора без кода: без номеров строк, сворачивания, направляющих отступа, поиска и автопар скобок;
        // перенос строк — по ширине.
        let body = cx.new(|cx| {
            EditorState::new(window, cx)
                .line_number(false)
                .folding(false)
                .indent_guides(false)
                .searchable(false)
                .auto_close(false)
                .soft_wrap(true)
                .placeholder(tr!("compose-body-placeholder"))
                .default_value(body_text)
        });
        let mut subscriptions = Vec::new();
        for (f, state) in [(Field::To, &to), (Field::Cc, &cc), (Field::Bcc, &bcc)] {
            subscriptions.push(cx.subscribe_in(state, window, move |this, state, ev: &InputEvent, window, cx| {
                let value = state.read(cx).value().to_string();
                this.on_address_input(f, value, ev, window, cx);
            }));
        }
        subscriptions.push(cx.subscribe_in(&subject, window, |this, state, ev: &InputEvent, _, cx| {
            let value = state.read(cx).value().to_string();
            this.on_input(Field::Subject, value, ev, cx);
        }));
        subscriptions.push(cx.subscribe_in(&body, window, |this, state, ev: &InputEvent, _, cx| {
            let value = state.read(cx).value().to_string();
            this.on_input(Field::Body, value, ev, cx);
            if matches!(ev, InputEvent::Change) {
                this.paint_markdown(cx);
            }
        }));
        // Enter в строке «Отправить позже» — «Запланировать».
        for state in [&later.date, &later.time] {
            subscriptions.push(cx.subscribe_in(state, window, |this, _, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    this.schedule_picked(window, cx);
                }
            }));
        }
        // Новое письмо — курсор в «Кому», ответ (адресаты уже есть) — в текст.
        if model.to.trim().is_empty() {
            to.update(cx, |s, cx| s.focus(window, cx));
        } else {
            body.update(cx, |s, cx| s.focus(window, cx));
        }
        let own = own_domains(&open.senders, open.me.as_ref().map(|(a, _)| a));
        let (show_cc, show_bcc) = (!model.cc.trim().is_empty(), !model.bcc.trim().is_empty());
        Self {
            id: NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            model,
            place: open.place,
            senders: open.senders,
            certs: open.certs,
            confirm_unmark: false,
            shared: open.shared,
            to,
            cc,
            bcc,
            subject,
            body,
            focus: cx.focus_handle(),
            mailer: open.mailer,
            book: open.book,
            marks: None,
            marks_look: None,
            suggest: Suggest::default(),
            autosave_armed: false,
            sync_inputs: false,
            show_cc,
            show_bcc,
            menu: Menu::None,
            categories: open.categories,
            own,
            recipients_key: 0,
            resolve_pending: None,
            later,
            prefs: open.prefs,
            screens: open.screens.then_some(0),
            _subscriptions: subscriptions,
        }
    }

    pub(crate) fn set_place(&mut self, place: Place, cx: &mut Context<Self>) {
        self.place = place;
        cx.notify();
    }

    /// Снимок `--screens`: письмо нарисовано несколько раз.
    pub(crate) fn drawn(&self) -> bool {
        self.screens.is_some_and(|n| n >= 3)
    }

    pub(crate) fn focus_body(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.body.update(cx, |s, cx| s.focus(window, cx));
    }

    fn input(&self, f: Field) -> Option<&Entity<InputState>> {
        match f {
            Field::To => Some(&self.to),
            Field::Cc => Some(&self.cc),
            Field::Bcc => Some(&self.bcc),
            Field::Subject => Some(&self.subject),
            Field::Body => None,
        }
    }

    fn on_input(&mut self, f: Field, value: String, ev: &InputEvent, cx: &mut Context<Self>) {
        match ev {
            InputEvent::Change => {
                self.model.set(f, value);
                self.schedule_autosave(cx);
            }
            InputEvent::Focus => {
                self.suggest.field = None;
                self.suggest.items.clear();
            }
            _ => return,
        }
        cx.notify();
    }

    /// Поле адреса: в поле ввода — набираемый адрес; «;» или «,» делают готовые адреса чипами.
    fn on_address_input(&mut self, f: Field, value: String, ev: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            InputEvent::Change => {
                let (mut closed, _) = segments(self.model.field(f));
                let open = if value.contains([';', ',']) {
                    let (more, open) = segments(&value);
                    closed.extend(more);
                    if let Some(state) = self.input(f) {
                        let open = open.clone();
                        state.update(cx, |s, cx| s.set_value(open, window, cx));
                    }
                    open
                } else {
                    value
                };
                self.model.set(f, compose_field(&closed, &open));
                self.refresh_suggestions();
                // Справочник, который сервер отдаёт только поиском (Exchange, S7-08).
                let token = current_token(&open).to_owned();
                let _ = self.mailer.update(cx, |m, cx| m.find_people(&token, cx));
                self.schedule_autosave(cx);
            }
            InputEvent::Focus => {
                self.suggest.field = Some(f);
                self.refresh_suggestions();
            }
            _ => return,
        }
        cx.notify();
    }

    fn set_field(&mut self, f: Field, closed: &[String], open: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.model.set(f, compose_field(closed, open));
        if let Some(state) = self.input(f) {
            let open = open.to_owned();
            state.update(cx, |s, cx| s.set_value(open, window, cx));
        }
        self.schedule_autosave(cx);
        cx.notify();
    }

    /// Enter без подсказки: набранный адрес — чипом.
    fn commit(&mut self, f: Field, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let (mut closed, open) = segments(self.model.field(f));
        if open.trim().is_empty() {
            return false;
        }
        closed.push(open.trim().to_owned());
        self.set_field(f, &closed, "", window, cx);
        true
    }

    /// Backspace в пустом поле — последний чип обратно в поле.
    fn uncommit(&mut self, f: Field, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let (mut closed, open) = segments(self.model.field(f));
        if !open.is_empty() {
            return false;
        }
        let Some(last) = closed.pop() else { return false };
        self.set_field(f, &closed, &last, window, cx);
        true
    }

    fn remove_chip(&mut self, f: Field, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (mut closed, open) = segments(self.model.field(f));
        if i < closed.len() {
            closed.remove(i);
            self.set_field(f, &closed, &open, window, cx);
        }
    }

    fn refresh_suggestions(&mut self) {
        self.suggest.selected = 0;
        self.suggest.items.clear();
        let Some(f) = self.suggest.field else { return };
        let token = current_token(self.model.field(f));
        if token.chars().count() < SUGGEST_FROM || token.contains('<') {
            return;
        }
        let now = jiff::Timestamp::now().as_second();
        self.suggest.items = self.book.borrow().suggest(token, SUGGESTIONS, now).into_iter().map(|e| e.display()).collect();
    }

    fn suggesting(&self, f: Field) -> bool {
        self.suggest.field == Some(f) && !self.suggest.items.is_empty()
    }

    fn accept(&mut self, f: Field, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.suggest.items.get(i) else { return };
        let value = accept_suggestion(self.model.field(f), item);
        let (closed, open) = segments(&value);
        self.set_field(f, &closed, &open, window, cx);
        if let Some(state) = self.input(f) {
            state.update(cx, |s, cx| s.focus(window, cx));
        }
        self.refresh_suggestions();
    }

    fn move_suggestion(&mut self, f: Field, delta: isize, cx: &mut Context<Self>) -> bool {
        if !self.suggesting(f) {
            return false;
        }
        let last = self.suggest.items.len() - 1;
        self.suggest.selected = self.suggest.selected.saturating_add_signed(delta).min(last);
        cx.notify();
        true
    }

    /// Таймер автосохранения — только при изменениях: в простое письмо не просыпается.
    fn schedule_autosave(&mut self, cx: &mut Context<Self>) {
        if self.autosave_armed {
            return;
        }
        let Some(wait) = self.model.autosave_wait(Instant::now()) else { return };
        self.autosave_armed = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |c, cx| {
                c.autosave_armed = false;
                if c.model.should_autosave(Instant::now()) {
                    c.dispatch(Purpose::Autosave, cx);
                } else {
                    c.schedule_autosave(cx);
                }
            });
        })
        .detach();
    }

    /// Черновик — в синхронизацию через шов экрана. Нет сервера — строка в письме (и тост, если просил
    /// пользователь), текст остаётся.
    fn dispatch(&mut self, purpose: Purpose, cx: &mut Context<Self>) {
        if self.model.busy() {
            return;
        }
        let Some(command) = self.model.begin(purpose, next_ticket(), Instant::now()) else {
            cx.notify();
            return;
        };
        let accepted = self
            .mailer
            .update(cx, |m, cx| {
                let ok = m.backend.send(command);
                if !ok && purpose != Purpose::Autosave {
                    m.needs_server(cx);
                }
                ok
            })
            .unwrap_or(false);
        if !accepted {
            self.model.refused();
        }
        cx.notify();
    }

    /// Итог отправки или сохранения — по тикету. Своё отправленное письмо — экрану (окно «Отменить»).
    pub(crate) fn on_sync(&mut self, ev: &Event, cx: &mut Context<Self>) -> Option<SentDraft> {
        let mut sent = None;
        match ev {
            Event::Sent { ticket, .. } => {
                let later = self.model.sending_later();
                let draft = self.model.draft(false).ok();
                if self.model.on_sent(*ticket) {
                    cx.emit(ComposerEvent::Close);
                    sent = draft.map(|draft| SentDraft { draft, later });
                }
            }
            Event::Scheduled { max_delay, .. } => self.later.max_delay = *max_delay,
            Event::DraftSaved { ticket, id } => {
                let at = jiff::Zoned::now().strftime("%H:%M").to_string();
                if self.model.on_saved(*ticket, id.clone(), at) {
                    cx.emit(ComposerEvent::Close);
                }
                // Пока сохранялось, могли набрать ещё.
                self.schedule_autosave(cx);
            }
            Event::ComposeFailed { ticket, error } => {
                self.model.on_failed(*ticket, error.clone());
            }
            // Справочник нашёл людей (они уже в книге адресов) — подсказки по набранному заново.
            Event::PeopleFound { .. } if self.suggest.field.is_some() => self.refresh_suggestions(),
            Event::Me { address, signature } => {
                // Нетронутое письмо получило подпись Identity — своя по умолчанию (настройки) сильнее.
                if self.model.set_identity(address.clone(), signature) {
                    default_signature(&mut self.model, &self.prefs, self.place == Place::Inline);
                    self.sync_inputs = true;
                }
                self.own = own_domains(&self.senders, Some(address));
            }
            Event::Senders(list) => {
                self.senders = list.clone();
                self.own = own_domains(&self.senders, None);
            }
            _ => return None,
        }
        cx.notify();
        sent
    }

    /// Меню «Отправить позже» открыто (снимок `send-later`).
    pub(crate) fn later_menu_open(&self) -> bool {
        self.later.menu
    }

    /// Меню «Отправить позже» у кнопки «Отправить».
    pub(crate) fn toggle_later_menu(&mut self, cx: &mut Context<Self>) {
        self.later.menu = !self.later.menu && self.later.max_delay > 0;
        self.menu = Menu::None;
        cx.notify();
    }

    /// Вариант меню: проверить срок и отправить; срок не годится — строка выбора с этим временем и причиной.
    fn send_at_choice(&mut self, choice: Choice, window: &mut Window, cx: &mut Context<Self>) {
        let now = jiff::Zoned::now();
        self.later.menu = false;
        if let Some(at) = choice.at(&now) {
            self.schedule(at, &now, window, cx);
        }
        cx.notify();
    }

    /// «Выбрать дату и время…»: строка в письме, поля — завтра 09:00.
    fn pick_later(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = jiff::Zoned::now();
        let start = Choice::TomorrowMorning.at(&now).unwrap_or_else(|| now.clone());
        self.open_picker(&start, None, window, cx);
    }

    fn open_picker(&mut self, at: &jiff::Zoned, error: Option<Reject>, window: &mut Window, cx: &mut Context<Self>) {
        let (date, time) = (at.strftime("%d.%m.%Y").to_string(), at.strftime("%H:%M").to_string());
        self.later.date.update(cx, |s, cx| s.set_value(date, window, cx));
        self.later.time.update(cx, |s, cx| s.set_value(time, window, cx));
        self.later.time.update(cx, |s, cx| s.focus(window, cx));
        self.later.menu = false;
        self.later.picking = true;
        self.later.error = error;
        cx.notify();
    }

    /// «Запланировать» в строке выбора.
    fn schedule_picked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = jiff::Zoned::now();
        let (date, time) = (self.later.date.read(cx).value().to_string(), self.later.time.read(cx).value().to_string());
        match send_later::parse(&date, &time, &now) {
            Ok(at) => self.schedule(at, &now, window, cx),
            Err(e) => self.later.error = Some(e),
        }
        cx.notify();
    }

    fn schedule(&mut self, at: jiff::Zoned, now: &jiff::Zoned, window: &mut Window, cx: &mut Context<Self>) {
        match send_later::check(&at, now, self.later.max_delay) {
            Ok(ts) => {
                self.later.picking = false;
                self.later.error = None;
                self.dispatch(Purpose::SendLater(ts), cx);
            }
            Err(e) => self.open_picker(&at, Some(e), window, cx),
        }
    }

    fn close_later(&mut self, cx: &mut Context<Self>) -> bool {
        let open = self.later.menu || self.later.picking;
        self.later.menu = false;
        self.later.picking = false;
        self.later.error = None;
        cx.notify();
        open
    }

    /// Esc: меню, подсказки, строки вопросов; затем — закрыть письмо (с изменениями — строкой «Сохранить черновик?»).
    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.close_later(cx) {
            return;
        }
        if self.menu != Menu::None {
            self.menu = Menu::None;
        } else if !self.suggest.items.is_empty() {
            self.suggest.items.clear();
        } else if self.model.confirm_delete {
            self.model.confirm_delete = false;
        } else if self.model.confirm_close {
            self.answer(CloseAnswer::Cancel, window, cx);
        } else {
            self.close_or_ask(window, cx);
        }
        cx.notify();
    }

    /// Закрыть (Ctrl+W, «×», Esc): без изменений — сразу, иначе строка «Сохранить черновик? Да / Нет / Отмена».
    pub(crate) fn close_or_ask(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.model.request_close() {
            cx.emit(ComposerEvent::Close);
        }
        cx.notify();
    }

    fn answer(&mut self, answer: CloseAnswer, _window: &mut Window, cx: &mut Context<Self>) {
        match self.model.answer_close(answer) {
            CloseStep::Close => cx.emit(ComposerEvent::Close),
            CloseStep::Save => self.dispatch(Purpose::Save, cx),
            CloseStep::Stay => {}
        }
        cx.notify();
    }

    /// «Удалить черновик» → «Да»: версия на сервере удаляется, письмо закрывается.
    fn discard(&mut self, cx: &mut Context<Self>) {
        self.model.confirm_delete = false;
        if let Some(id) = self.model.server_id().map(str::to_owned) {
            let _ = self.mailer.update(cx, |m, cx| {
                if !m.backend.send(Command::Destroy(id)) {
                    m.needs_server(cx);
                }
            });
        }
        cx.emit(ComposerEvent::Close);
    }

    fn pick_files(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: None });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                let _ = this.update(cx, |c, cx| c.attach(paths, cx));
            }
        })
        .detach();
    }

    /// Файлы читаются в фоне: поток интерфейса не ждёт диск.
    fn attach(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let task = cx.background_spawn(async move {
            paths
                .into_iter()
                .map(|p| {
                    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    (postman_core::files::safe_name(&name), std::fs::read(&p))
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let files = task.await;
            let _ = this.update(cx, |c, cx| {
                for (name, data) in files {
                    match data {
                        Ok(data) => c.model.add_attachment(Attachment {
                            mime: postman_core::files::mime_for(&name).into(),
                            name,
                            data,
                            inline_cid: None,
                            blob_id: None,
                        }),
                        Err(e) => c.model.status = Status::Attach { name, error: e.to_string() },
                    }
                }
                c.schedule_autosave(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn remove_attachment(&mut self, i: usize, cx: &mut Context<Self>) {
        self.model.remove_attachment(i);
        self.schedule_autosave(cx);
        cx.notify();
    }

    /// Кнопка панели форматирования: разметка вокруг выделения текста письма.
    fn format(&mut self, f: Format, window: &mut Window, cx: &mut Context<Self>) {
        let (text, sel) = {
            let s = self.body.read(cx);
            (s.value().to_string(), s.selected_range())
        };
        let (next, sel) = apply_format(&text, sel, f);
        self.body.update(cx, |s, cx| {
            s.replace_all(next.clone(), window, cx);
            s.set_selected_range(sel, cx);
            s.focus(window, cx);
        });
        self.model.set(Field::Body, next);
        self.schedule_autosave(cx);
        cx.notify();
    }

    fn sync_body(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let body = self.model.body.clone();
        self.body.update(cx, |s, cx| s.replace_all(body, window, cx));
        self.paint_markdown(cx);
    }

    /// Живое отображение Markdown (S5-27): разметка текста письма — оформлением поля. Свой текст разбирается здесь же
    /// (`postman_compose::display`, без чужих данных); цвета — роли темы.
    pub(crate) fn paint_markdown(&mut self, cx: &mut Context<Self>) {
        let l = crate::roles::look(cx);
        let text = self.body.read(cx).value().to_string();
        let marks: Vec<TextDecoration> =
            postman_compose::display::display(&text).into_iter().map(|s| TextDecoration::new(s.range, md_style(&s.look, &l))).collect();
        self.marks_look = Some(l);
        match &self.marks {
            Some(c) => c.set(marks, cx),
            None => {
                let c = self.body.update(cx, |s, cx| s.create_decorations_collection(marks, cx));
                self.marks = Some(c);
            }
        }
    }

    /// Действие замечания «Перед отправкой»: первое (`primary`) или второе.
    fn check_action(&mut self, check: Check, primary: bool, window: &mut Window, cx: &mut Context<Self>) {
        match (&check, primary) {
            (Check::Card { .. } | Check::Secret { .. }, true) => {
                if self.model.mask(&check) {
                    self.sync_body(window, cx);
                }
            }
            (Check::MissingAttachment { .. }, true) => self.pick_files(cx),
            (Check::External { .. }, true) => self.to.update(cx, |s, cx| s.focus(window, cx)),
            (Check::NoCertificate { addresses, .. }, true) => {
                if self.model.remove_addresses(addresses) {
                    self.sync_inputs = true;
                }
            }
            // «Снять „Конфиденциально“» — строкой подтверждения (решение владельца №6).
            (Check::NoCertificate { .. }, false) if !self.confirm_unmark => self.confirm_unmark = true,
            (Check::NoCertificate { .. }, false) => {
                self.model.confidential = false;
                self.confirm_unmark = false;
            }
            (Check::Moved { from, to, .. }, true) => {
                if self.model.replace_address(from, to) {
                    for f in [Field::To, Field::Cc, Field::Bcc] {
                        let open = segments(self.model.field(f)).1;
                        if let Some(state) = self.input(f) {
                            state.update(cx, |s, cx| s.set_value(open, window, cx));
                        }
                    }
                }
            }
            (Check::Split { .. }, true) | (_, false) => self.model.dismiss(&check),
        }
        self.schedule_autosave(cx);
        cx.notify();
    }

    /// Замечания по адресатам — когда сменились адресаты, метка или адресная книга (она строится в фоне).
    fn refresh_recipient_checks(&mut self) {
        let mut h = DefaultHasher::new();
        (&self.model.to, &self.model.cc, &self.model.bcc, self.model.confidential).hash(&mut h);
        (self.book.borrow().len(), &self.own, crate::smime_seal::lock(&self.certs).version, &self.model.from().email).hash(&mut h);
        let key = h.finish();
        if key != self.recipients_key {
            self.recipients_key = key;
            let book = self.book.borrow();
            self.model.refresh_recipient_checks(&self.own, &book);
            drop(book);
            self.refresh_encryption();
        }
    }

    /// «Конфиденциально» (S5-12 Ф4): замечания по сертификатам, получатели шифрования; неизвестные адреса — на поиск.
    fn refresh_encryption(&mut self) {
        if !self.model.confidential {
            self.model.set_encryption(Vec::new(), Vec::new());
            return;
        }
        let recipients = self.model.recipients();
        let from = self.model.from().email.clone();
        let (checks, to, unknown) = {
            let b = crate::smime_seal::lock(&self.certs);
            let checks = checks::certificates(&recipients, &from, &|a| b.families(a));
            let to = b.encrypt_to(&recipients, &from);
            let unknown = b.unknown(recipients.iter().map(|a| a.email.as_str()).chain(std::iter::once(from.as_str())));
            (checks, to, unknown)
        };
        self.model.set_encryption(checks, to);
        if !unknown.is_empty() {
            self.resolve_pending = Some((unknown, from));
        }
    }

    fn set_menu(&mut self, menu: Menu, cx: &mut Context<Self>) {
        self.menu = if self.menu == menu { Menu::None } else { menu };
        self.later.menu = false;
        cx.notify();
    }

    /// Подписи на выбор: «без подписи», свои подписи для адреса «От» (настройки, S5-09) и подписи адресов «От»
    /// (своя и общих ящиков) без повторов.
    fn signatures(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for s in self.prefs.signatures_for(&self.model.from().email) {
            let s = s.text.trim_end();
            if !s.is_empty() && !out.iter().any(|o| o == s) {
                out.push(s.to_owned());
            }
        }
        for s in &self.senders {
            let s = s.signature.trim_end();
            if !s.is_empty() && !out.iter().any(|o| o == s) {
                out.push(s.to_owned());
            }
        }
        let current = self.model.signature.trim_end();
        if !current.is_empty() && !out.iter().any(|o| o == current) {
            out.insert(0, current.to_owned());
        }
        out
    }
}

/// Подпись по умолчанию из настроек (новые письма или ответы и пересылка) для адреса «От»; `true` — текст изменился.
fn default_signature(model: &mut Model, prefs: &AccountPrefs, reply: bool) -> bool {
    let Some(s) = prefs.signature(&model.from().email, reply) else { return false };
    let text = s.text.trim_end().to_owned();
    if model.signature == text {
        return false;
    }
    model.set_signature(&text);
    true
}

// --- отрисовка ---

/// Первая строка подписи — её название в меню.
fn signature_title(s: &str) -> String {
    s.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().trim().to_owned()
}

fn quiet(id: impl Into<ElementId>, l: &Look) -> Stateful<Div> {
    quiet_plain(id, l).hover(|d| d.bg(l.hover()))
}

/// Тихая кнопка без подложки при наведении (поле «От»). Стиль наведения у элемента задаётся один раз: второй
/// `.hover` поверх `quiet` в отладочной сборке gpui — паника «hover style already set».
pub(crate) fn quiet_plain(id: impl Into<ElementId>, l: &Look) -> Stateful<Div> {
    div().id(id).h(px(30.)).px(px(8.)).flex().flex_none().items_center().gap(px(6.)).rounded(px(4.)).cursor_pointer().active(|d| d.bg(l.pressed()))
}

pub(crate) fn menu_box(id: &'static str, l: &Look) -> Stateful<Div> {
    div()
        .id(id)
        .absolute()
        .top(px(32.))
        .left_0()
        .min_w(px(220.))
        .occlude()
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
}

pub(crate) fn menu_item(id: ElementId, l: &Look, on: bool) -> Stateful<Div> {
    div()
        .id(id)
        .h(l.px(Metric::HMenuItem))
        .px(px(10.))
        .flex()
        .items_center()
        .gap(px(10.))
        .rounded(l.px(Metric::RadiusControl))
        .cursor_pointer()
        .when(on, |d| d.bg(l.picked()))
        .hover(|d| d.bg(l.hover()))
}

fn card(l: &Look) -> Div {
    div().flex().flex_col().p(px(14.)).px(px(16.)).rounded(l.px(Metric::RadiusOverlay)).border_1().border_color(l.c(Role::StrokeCard)).bg(l.panel())
}

fn card_title(text: String) -> Div {
    div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(text)
}

/// Сколько дней назад: «сегодня», «3 дня назад».
fn ago(ts: i64) -> String {
    let days = (jiff::Timestamp::now().as_second() - ts).max(0) / 86_400;
    if days == 0 { tr!("checks-today") } else { tr!("checks-days-ago", n = days) }
}

impl Composer {
    fn status_text(&self) -> (String, bool) {
        let busy = |p: Purpose| match p {
            Purpose::Send => tr!("compose-sending"),
            Purpose::SendLater(_) => tr!("send-later-scheduling"),
            Purpose::Save => tr!("compose-saving"),
            Purpose::Autosave => tr!("compose-autosaving"),
        };
        let (text, error) = match &self.model.status {
            Status::None => (String::new(), false),
            Status::Busy(p) => (busy(*p), false),
            Status::Saved { at } => (tr!("compose-saved", time = at.clone()), false),
            Status::BadAddresses(bad) => (tr!("compose-bad-addresses", list = bad.join(", ")), true),
            Status::NoRecipients => (tr!("compose-no-recipients"), true),
            Status::Blocked => (tr!("compose-blocked"), true),
            Status::NoServer(Purpose::Send | Purpose::SendLater(_)) => (tr!("compose-no-server-send"), true),
            Status::NoServer(_) => (tr!("compose-no-server-save"), true),
            Status::Failed(e) => (failed_text(e), true),
            Status::Attach { name, error } => (tr!("compose-attach-failed", name = name.clone(), error = error.clone()), true),
            Status::Large(total) => (tr!("compose-large", size = size_text(*total)), true),
        };
        // Окно «Отменить» после отправки — если сервер держит письма.
        if error || self.later.max_delay == 0 {
            return (text, error);
        }
        let secs = send_later::UNDO_WINDOW.as_secs();
        if text.is_empty() {
            (tr!("compose-undo-hint", s = secs), false)
        } else {
            (format!("{text} {}", tr!("compose-undo-suffix", s = secs)), false)
        }
    }

    fn label(&self, key: &str, bad: bool, l: &Look) -> Div {
        div().w(px(LABEL_W)).flex_none().text_size(px(13.)).text_color(if bad { l.error() } else { l.pm(Pm::LabelAccent) }).child(tr!(key))
    }

    fn row(&self, l: &Look) -> Div {
        div().flex().items_center().gap(px(12.)).min_h(px(ROW_H)).border_b_1().border_color(l.border())
    }

    /// «От»: адрес и меню адресов (свой и общих ящиков, S4-06), если их больше одного.
    fn sender_row(&self, l: &Look, cx: &mut Context<Self>) -> Div {
        let from = self.model.from();
        let many = self.senders.len() > 1;
        let current = from.email.to_lowercase();
        let button = quiet_plain("from", l)
            .px(px(0.))
            .child(from.display())
            .when(many, |d| d.child(icon(glyph::CHEVRON, 10.).text_color(l.text2())))
            .when(many, |d| d.on_click(cx.listener(|this, _, _, cx| this.set_menu(Menu::From, cx))));
        let menu = (self.menu == Menu::From).then(|| {
            let items = self.senders.iter().enumerate().map(|(i, s)| {
                let address = s.address.clone();
                // Другая моя учётная запись (S7-03б) — просто адрес; общий ящик — с пометкой.
                let text = match &s.account {
                    Some(a) if postman_cache::profile_of(Some(a)) != Some(a.as_str()) => {
                        tr!("shared-compose-from-shared", address = s.address.display())
                    }
                    _ => s.address.display(),
                };
                // Ответ на письмо общего ящика (S5-28, макет): под адресом — куда уйдёт копия и кого увидит клиент.
                let hint = self.shared.as_ref().map(|r| div().text_size(px(11.)).text_color(l.text2()).child(r.hint(&s.address.email)));
                let text = div().flex().flex_col().child(text).children(hint);
                menu_item(ElementId::NamedInteger("from-item".into(), i as u64), l, s.address.email.to_lowercase() == current)
                    .when(self.shared.is_some(), |d| d.h_auto().py(px(6.)))
                    .child(text)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model.set_from(address.clone());
                        this.menu = Menu::None;
                        this.schedule_autosave(cx);
                        cx.notify();
                    }))
            });
            deferred(menu_box("from-menu", l).left(px(LABEL_W + 12.)).children(items).on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.menu = Menu::None;
                cx.notify();
            })))
            .with_priority(1)
        });
        self.row(l).relative().child(self.label("shared-compose-from", false, l)).child(div().text_size(px(13.)).child(button)).children(menu)
    }

    /// Чип адресата: инициалы, имя (или адрес), «Внешний», «×»; нераспознанный — красной рамкой.
    fn chip(&self, f: Field, i: usize, text: &str, l: &Look, cx: &mut Context<Self>) -> AnyElement {
        let parsed = parse_addresses(text).0.into_iter().next();
        let bad = parsed.is_none();
        let (name, email) = match &parsed {
            Some(a) if !a.name.is_empty() => (a.name.clone(), a.email.clone()),
            Some(a) => (a.email.clone(), a.email.clone()),
            None => (text.to_owned(), String::new()),
        };
        let external = !bad && checks::is_external(&email, &self.own);
        let neutral = l.cat(postman_core::palette::NEUTRAL);
        let id = ElementId::NamedInteger(format!("chip-{f:?}").into(), i as u64);
        let remove = ElementId::NamedInteger(format!("chip-x-{f:?}").into(), i as u64);
        div()
            .id(id)
            .h(px(26.))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .pl(px(3.))
            .pr(px(4.))
            .rounded(px(13.))
            .bg(l.fill())
            .border_1()
            .border_color(if bad { l.error() } else { l.none() })
            .text_size(px(13.))
            .child(
                div()
                    .size(px(20.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(neutral.bg)
                    .text_color(neutral.text)
                    .text_size(px(10.))
                    .child(crate::list::initials(&name)),
            )
            .child(div().max_w(px(220.)).truncate().child(name))
            // «Конфиденциально» (S5-12 Ф4): замок — сертификат есть, знак — нет.
            .when(self.model.confidential && !bad, |d| match crate::smime_seal::lock(&self.certs).families(&email) {
                Some(f) if !f.is_empty() => d.child(icon(glyph::LOCK, 12.).text_color(l.text2())),
                Some(_) => d.child(icon(glyph::WARNING, 12.).text_color(l.pm(Pm::ThreatFg))),
                None => d,
            })
            .when(external, |d| {
                d.child(
                    div()
                        .px(px(5.))
                        .rounded(px(4.))
                        .border_1()
                        .border_color(l.stroke())
                        .text_size(px(11.))
                        .text_color(l.text2())
                        .child(tr!("compose-external")),
                )
            })
            .child(
                div()
                    .id(remove)
                    .size(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .cursor_pointer()
                    .text_color(l.text3())
                    .hover(|d| d.bg(l.pressed()).text_color(l.text()))
                    .child(icon(glyph::CLOSE, 8.))
                    .on_click(cx.listener(move |this, _, window, cx| this.remove_chip(f, i, window, cx))),
            )
            .when(!email.is_empty(), |d| d.tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(email.clone()).build(window, cx)))
            .into_any_element()
    }

    /// Строка адресатов: подпись, чипы, поле набора, подсказки; у «Кому» — ссылки «Копия» и «Скрытая».
    fn address_row(&self, f: Field, label: &str, l: &Look, cx: &mut Context<Self>) -> AnyElement {
        let Some(state) = self.input(f) else { return div().into_any_element() };
        let (closed, _) = segments(self.model.field(f));
        let bad = closed.iter().any(|c| parse_addresses(c).0.is_empty());
        let chips: Vec<AnyElement> = closed.iter().enumerate().map(|(i, c)| self.chip(f, i, c, l, cx)).collect();
        let field = div()
            .flex_1()
            .min_w(px(96.))
            .child(Input::new(state).appearance(false).px_0().text_size(px(13.)))
            .capture_action(cx.listener(move |this, _: &MoveDown, _, cx| {
                if this.move_suggestion(f, 1, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(move |this, _: &MoveUp, _, cx| {
                if this.move_suggestion(f, -1, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(move |this, a: &Enter, window, cx| {
                if a.secondary {
                    return;
                }
                if this.suggesting(f) {
                    this.accept(f, this.suggest.selected, window, cx);
                    cx.stop_propagation();
                } else if this.commit(f, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(move |this, _: &IndentInline, window, cx| {
                if this.suggesting(f) {
                    this.accept(f, this.suggest.selected, window, cx);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(move |this, _: &Backspace, window, cx| {
                if this.uncommit(f, window, cx) {
                    cx.stop_propagation();
                }
            }));
        let link = |id: &'static str, key: &'static str, cx: &mut Context<Self>, show: fn(&mut Self)| {
            div()
                .id(id)
                .flex_none()
                .px(px(4.))
                .text_size(px(12.))
                .text_color(l.accent_text())
                .cursor_pointer()
                .hover(|d| d.underline())
                .child(tr!(key))
                .on_click(cx.listener(move |this, _, _, cx| {
                    show(this);
                    cx.notify();
                }))
        };
        let mut row = self.row(l).flex_wrap().gap(px(6.)).py(px(4.)).child(self.label(label, bad, l).mr(px(6.))).children(chips).child(field);
        if f == Field::To {
            row = row
                .when(!self.show_cc, |d| d.child(link("show-cc", "compose-cc", cx, |c| c.show_cc = true)))
                .when(!self.show_bcc, |d| d.child(link("show-bcc", "compose-bcc", cx, |c| c.show_bcc = true)));
        }
        let suggestions = self.suggesting(f).then(|| {
            let items = self.suggest.items.iter().enumerate().map(|(i, s)| {
                menu_item(ElementId::NamedInteger("suggest".into(), i as u64), l, i == self.suggest.selected)
                    .child(s.clone())
                    .on_click(cx.listener(move |this, _, window, cx| this.accept(f, i, window, cx)))
            });
            deferred(menu_box("suggest-menu", l).top(px(ROW_H)).left(px(LABEL_W + 12.)).min_w(px(320.)).children(items)).with_priority(1)
        });
        div().relative().child(row).children(suggestions).into_any_element()
    }

    fn subject_row(&self, l: &Look) -> Div {
        self.row(l).child(self.label("compose-subject", false, l)).child(
            div().flex_1().min_w_0().child(Input::new(&self.subject).appearance(false).px_0().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD)),
        )
    }

    /// Панель: Ж К Ч | списки, ссылка, цитата | Вложить, Шаблон, Подпись … Метка.
    fn toolbar(&self, l: &Look, cx: &mut Context<Self>) -> Div {
        let tb = l.pm(Pm::LabelAccent);
        let fmt = |id: &'static str, f: Format, tip: &'static str, cx: &mut Context<Self>| {
            quiet(id, l)
                .w(px(30.))
                .px(px(0.))
                .justify_center()
                .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tr!(tip)).build(window, cx))
                .on_click(cx.listener(move |this, _, window, cx| this.format(f, window, cx)))
        };
        let sep = || div().w_px().h(px(18.)).mx(px(6.)).bg(l.stroke());
        let signature = if self.model.signature.is_empty() { tr!("compose-signature-none") } else { signature_title(&self.model.signature) };
        let mark = if self.model.confidential { tr!("compose-mark-confidential") } else { tr!("compose-mark-none") };
        let menu = |m: Menu| self.menu == m;
        // Ответ в области чтения уже вкладки: шаблон и подпись — во вкладке («Развернуть»), метка — значком.
        let wide = self.place == Place::Tab;
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(2.))
            .px(px(20.))
            .py(px(6.))
            .border_b_1()
            .border_color(l.border())
            .text_size(px(13.))
            .text_color(tb)
            .child(fmt("fmt-bold", Format::Bold, "cmd-compose-bold", cx).font_weight(FontWeight::BOLD).child(tr!("compose-fmt-bold")))
            .child(fmt("fmt-italic", Format::Italic, "cmd-compose-italic", cx).italic().child(tr!("compose-fmt-italic")))
            .child(fmt("fmt-underline", Format::Underline, "cmd-compose-underline", cx).underline().child(tr!("compose-fmt-underline")))
            .child(sep())
            .child(fmt("fmt-bullets", Format::Bullets, "cmd-compose-bullets", cx).child(icon(glyph::BULLETS, 16.)))
            .child(fmt("fmt-numbers", Format::Numbers, "cmd-compose-numbers", cx).text_size(px(12.)).child("1."))
            .child(fmt("fmt-link", Format::Link, "cmd-compose-link", cx).child(icon(glyph::LINK, 16.)))
            .child(fmt("fmt-quote", Format::Quote, "cmd-compose-quote", cx).child(icon(glyph::QUOTE, 16.)))
            .child(sep())
            .child(
                quiet("tb-attach", l)
                    .child(icon(glyph::ATTACH, 16.))
                    .child(tr!("compose-attach"))
                    .on_click(cx.listener(|this, _, _, cx| this.pick_files(cx))),
            )
            .when(wide, |d| {
                d.child(
                    div()
                        .relative()
                        .child(
                            quiet("tb-template", l)
                                .when(menu(Menu::Template), |d| d.bg(l.hover()))
                                .child(tr!("compose-template"))
                                .child(icon(glyph::CHEVRON, 10.))
                                .on_click(cx.listener(|this, _, _, cx| this.set_menu(Menu::Template, cx))),
                        )
                        .children(menu(Menu::Template).then(|| self.template_menu(l, cx))),
                )
            })
            .when(wide, |d| {
                d.child(
                    div()
                        .relative()
                        .child(
                            quiet("tb-signature", l)
                                .when(menu(Menu::Signature), |d| d.bg(l.hover()))
                                .child(div().max_w(px(180.)).truncate().child(tr!("compose-signature", name = signature)))
                                .child(icon(glyph::CHEVRON, 10.))
                                .on_click(cx.listener(|this, _, _, cx| this.set_menu(Menu::Signature, cx))),
                        )
                        .children(menu(Menu::Signature).then(|| self.signature_menu(l, cx))),
                )
            })
            .child(div().flex_1())
            // «Подписать» (S5-12 Ф3): S/MIME при отправке сертификатом адреса «От».
            .child({
                let on = self.model.sign;
                let b = quiet("tb-sign", l)
                    .border_1()
                    .border_color(if on { l.stroke() } else { l.none() })
                    .when(on, |d| d.bg(l.picked()))
                    .child(icon(glyph::CERTIFICATE, 14.))
                    .when(wide || on, |d| d.child(tr!("compose-sign")))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.model.sign = !this.model.sign;
                        cx.notify();
                    }));
                b.tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(tr!(if on { "compose-sign-on" } else { "compose-sign-off" })).build(window, cx)
                })
            })
            .child(
                div()
                    .relative()
                    .child(
                        quiet("tb-mark", l)
                            .border_1()
                            .border_color(if self.model.confidential || wide { l.stroke() } else { l.none() })
                            .when(menu(Menu::Mark), |d| d.bg(l.hover()))
                            .child(icon(glyph::LOCK, 14.))
                            .when(wide || self.model.confidential, |d| d.child(mark))
                            .child(icon(glyph::CHEVRON, 10.))
                            .on_click(cx.listener(|this, _, _, cx| this.set_menu(Menu::Mark, cx))),
                    )
                    .children(menu(Menu::Mark).then(|| self.mark_menu(l, cx))),
            )
    }

    fn close_menu_out(&self, d: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        d.on_mouse_down_out(cx.listener(|this, _, _, cx| {
            this.menu = Menu::None;
            cx.notify();
        }))
    }

    /// «Шаблон»: шаблоны появятся в настройках (S5-09) — пока пустой список.
    fn template_menu(&self, l: &Look, cx: &mut Context<Self>) -> AnyElement {
        let menu = menu_box("template-menu", l).child(div().px(px(10.)).py(px(6.)).text_color(l.text2()).child(tr!("compose-template-empty")));
        deferred(self.close_menu_out(menu, cx)).with_priority(1).into_any_element()
    }

    /// «Подпись»: без подписи и подписи адресов «От».
    fn signature_menu(&self, l: &Look, cx: &mut Context<Self>) -> AnyElement {
        let current = self.model.signature.trim_end().to_owned();
        let none = menu_item("sig-none".into(), l, current.is_empty())
            .child(tr!("compose-signature-none"))
            .on_click(cx.listener(|this, _, window, cx| this.choose_signature(String::new(), window, cx)));
        let items = self.signatures().into_iter().enumerate().map(|(i, s)| {
            let on = s == current;
            menu_item(ElementId::NamedInteger("sig".into(), i as u64), l, on)
                .child(div().max_w(px(260.)).truncate().child(signature_title(&s)))
                .on_click(cx.listener(move |this, _, window, cx| this.choose_signature(s.clone(), window, cx)))
        });
        let menu = menu_box("signature-menu", l).child(none).children(items);
        deferred(self.close_menu_out(menu, cx)).with_priority(1).into_any_element()
    }

    fn choose_signature(&mut self, s: String, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = Menu::None;
        self.model.set_signature(&s);
        self.sync_body(window, cx);
        self.schedule_autosave(cx);
        cx.notify();
    }

    /// «Метка»: без метки или «Конфиденциально» (ключевое слово `pm.conf`; шифрование — S5-12).
    fn mark_menu(&self, l: &Look, cx: &mut Context<Self>) -> AnyElement {
        let item = |id: &'static str, key: &'static str, on: bool, value: bool, cx: &mut Context<Self>| {
            menu_item(id.into(), l, on).child(tr!(key)).on_click(cx.listener(move |this, _, _, cx| {
                this.model.confidential = value;
                this.menu = Menu::None;
                this.schedule_autosave(cx);
                cx.notify();
            }))
        };
        let c = self.model.confidential;
        let menu = menu_box("mark-menu", l)
            .left_auto()
            .right_0()
            .child(item("mark-none", "compose-mark-none", !c, false, cx))
            .child(item("mark-conf", "compose-mark-confidential", c, true, cx))
            .child(div().px(px(10.)).py(px(6.)).max_w(px(260.)).text_size(px(12.)).text_color(l.text2()).child(tr!("compose-mark-hint")));
        deferred(self.close_menu_out(menu, cx)).with_priority(1).into_any_element()
    }

    /// Текст письма: Markdown с живым отображением (S5-27), шрифт интерфейса, а не кода.
    fn content(&self, window: &Window) -> AnyElement {
        // Шрифт и размер — как у текста окна (редактор по умолчанию — моноширинный).
        let text = window.text_style();
        div()
            .flex_1()
            .min_h_0()
            .px(px(16.))
            .py(px(8.))
            .child(
                Editor::new(&self.body)
                    .appearance(false)
                    .font_family(text.font_family.clone())
                    .text_size(text.font_size)
                    .line_height(relative(1.5))
                    .h(relative(1.)),
            )
            .into_any_element()
    }

    /// Строки вопросов в письме: «Сохранить черновик?» и «Удалить черновик?».
    fn confirm_lines(&self, l: &Look, cx: &mut Context<Self>) -> Option<Div> {
        let line = || div().flex().items_center().gap_2().px(px(20.)).py(px(6.)).bg(l.warning_bg()).border_t_1().border_color(l.border());
        if self.model.confirm_close {
            return Some(
                line()
                    .child(tr!("compose-confirm-close"))
                    .child(
                        crate::buttons::button("close-save", tr!("compose-confirm-yes"), true, l)
                            .on_click(cx.listener(|this, _, window, cx| this.answer(CloseAnswer::Save, window, cx))),
                    )
                    .child(
                        crate::buttons::button("close-discard", tr!("compose-confirm-no"), false, l)
                            .on_click(cx.listener(|this, _, window, cx| this.answer(CloseAnswer::Discard, window, cx))),
                    )
                    .child(
                        crate::buttons::button("close-cancel", tr!("compose-confirm-cancel"), false, l)
                            .on_click(cx.listener(|this, _, window, cx| this.answer(CloseAnswer::Cancel, window, cx))),
                    ),
            );
        }
        if self.model.confirm_delete {
            return Some(
                line()
                    .child(tr!("compose-delete-confirm"))
                    .child(
                        crate::buttons::button("delete-yes", tr!("compose-confirm-yes"), true, l)
                            .on_click(cx.listener(|this, _, _, cx| this.discard(cx))),
                    )
                    .child(crate::buttons::button("delete-no", tr!("compose-confirm-no"), false, l).on_click(cx.listener(|this, _, _, cx| {
                        this.model.confirm_delete = false;
                        cx.notify();
                    }))),
            );
        }
        None
    }

    /// «Отправить» и рядом «▾» — меню «Отправить позже» (если сервер умеет отложенную отправку).
    fn send_group(&self, l: &Look, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.model.busy();
        let blocked = self.model.blocked();
        let send = div()
            .id("send")
            .h(px(32.))
            .px(px(16.))
            .flex()
            .items_center()
            .gap(px(8.))
            .bg(l.accent())
            .text_color(l.on_accent())
            .cursor_pointer()
            .hover(|d| d.bg(l.accent_hover()))
            .when(busy || blocked, |d| d.opacity(0.5))
            .child(icon(glyph::SEND, 14.))
            .child(tr!("compose-send"))
            .on_click(cx.listener(|this, _, _, cx| this.dispatch(Purpose::Send, cx)))
            .tooltip({
                let tip = crate::keytips::tip("compose.send", cx);
                move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
            });
        let group = div().relative().flex().flex_none().rounded(px(4.)).overflow_hidden().child(send);
        // Плашка KeyTips — снаружи группы (у неё `overflow_hidden`).
        let key = crate::keytips::key("compose.send", cx);
        if self.later.max_delay == 0 {
            return crate::keytips::badge(div().flex_none().child(group), key, l, cx).into_any_element();
        }
        let caret = div()
            .id("send-later")
            .w(px(30.))
            .h(px(32.))
            .flex()
            .items_center()
            .justify_center()
            .bg(l.accent())
            .text_color(l.on_accent())
            .border_l_1()
            .border_color(l.accent_hover())
            .cursor_pointer()
            .hover(|d| d.bg(l.accent_hover()))
            .when(busy || blocked, |d| d.opacity(0.5))
            .when(self.later.menu, |d| d.bg(l.accent_hover()))
            .child(icon(glyph::CHEVRON, 10.))
            .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tr!("send-later-menu")).build(window, cx))
            .on_click(cx.listener(|this, _, _, cx| this.toggle_later_menu(cx)));
        div()
            .relative()
            .child(group.child(caret))
            .when(self.later.menu, |d| d.child(deferred(self.later_menu(l, cx)).with_priority(1)))
            .into_any_element()
    }

    /// Меню под кнопкой (над ней — внизу письма): завтра утром и днём, понедельник, выбор даты.
    fn later_menu(&self, l: &Look, cx: &mut Context<Self>) -> impl IntoElement {
        let mut menu = menu_box("later-menu", l)
            .top_auto()
            .bottom(px(36.))
            .w(px(300.))
            .child(div().px(px(10.)).py(px(4.)).text_size(px(12.)).text_color(l.text2()).child(tr!("send-later-menu")));
        menu = menu.children(self.later_items(l, "later-choice", cx));
        menu.on_mouse_down_out(cx.listener(|this, _, _, cx| {
            this.later.menu = false;
            cx.notify();
        }))
    }

    /// Варианты «Отправить позже» со сроком справа и «Выбрать дату и время…».
    fn later_items(&self, l: &Look, prefix: &'static str, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let now = jiff::Zoned::now();
        let mut out: Vec<AnyElement> = Choice::ALL
            .into_iter()
            .enumerate()
            .map(|(i, choice)| {
                let hint = choice.at(&now).map(|at| crate::send_later::menu_hint(choice, &at)).unwrap_or_default();
                menu_item(ElementId::NamedInteger(prefix.into(), i as u64), l, false)
                    .child(div().flex_1().child(crate::send_later::choice_label(choice)))
                    .child(div().text_color(l.text2()).child(hint))
                    .on_click(cx.listener(move |this, _, window, cx| this.send_at_choice(choice, window, cx)))
                    .into_any_element()
            })
            .collect();
        out.push(
            menu_item(ElementId::NamedInteger(prefix.into(), 99), l, false)
                .child(tr!("send-later-pick"))
                .on_click(cx.listener(|this, _, window, cx| this.pick_later(window, cx)))
                .into_any_element(),
        );
        out
    }

    /// Строка «Выбрать дату и время» в письме (без модального окна).
    fn later_picker(&self, l: &Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.later.picking {
            return None;
        }
        let label = |text: String| div().flex_none().text_color(l.text2()).child(text);
        let error = self.later.error.map(|e| crate::send_later::reject_text(e, self.later.max_delay));
        let row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .px(px(20.))
            .py(px(6.))
            .bg(l.hover())
            .border_b_1()
            .border_color(l.border())
            .child(div().flex_none().text_color(l.accent_text()).child(tr!("send-later-menu")))
            .child(label(tr!("send-later-date")))
            .child(div().w(px(120.)).child(Input::new(&self.later.date)))
            .child(label(tr!("send-later-time")))
            .child(div().w(px(72.)).child(Input::new(&self.later.time)))
            .child(
                crate::buttons::button("later-schedule", tr!("send-later-schedule"), true, l)
                    .on_click(cx.listener(|this, _, window, cx| this.schedule_picked(window, cx))),
            )
            .child(crate::buttons::button("later-cancel", tr!("send-later-cancel"), false, l).on_click(cx.listener(|this, _, _, cx| {
                this.close_later(cx);
            })))
            .children(error.map(|e| div().text_color(l.error()).child(e)));
        Some(row.into_any_element())
    }

    /// «Ждать ответа · N дней»: переключатель; число дней — по щелчку на нём, по кругу.
    fn wait_toggle(&self, l: &Look, cx: &mut Context<Self>) -> Div {
        let on = self.model.wait.is_some();
        let days = self.model.wait.unwrap_or_else(|| crate::lifecycle::wait_default(cx));
        let knob = div()
            .w(px(28.))
            .h(px(16.))
            .flex_none()
            .relative()
            .rounded(px(8.))
            .when(on, |d| d.bg(l.accent()))
            .when(!on, |d| d.border_1().border_color(l.text2()))
            .child(div().absolute().top(px(if on { 3. } else { 2. })).left(px(if on { 15. } else { 3. })).size(px(10.)).rounded_full().bg(if on {
                l.on_accent()
            } else {
                l.text2()
            }));
        let toggle = quiet("wait", l).child(knob).child(tr!("compose-wait")).on_click(cx.listener(|this, _, _, cx| {
            this.model.wait = if this.model.wait.is_some() { None } else { Some(crate::lifecycle::wait_default(cx)) };
            this.schedule_autosave(cx);
            cx.notify();
        }));
        let count = quiet("wait-days", l)
            .px(px(4.))
            .when(!on, |d| d.text_color(l.text3()))
            .child(tr!("compose-wait-days", n = days))
            .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tr!("compose-wait-days-hint")).build(window, cx))
            .on_click(cx.listener(|this, _, _, cx| {
                let now = this.model.wait.unwrap_or_else(|| crate::lifecycle::wait_default(cx));
                let i = marks::WAIT_DAYS.iter().position(|d| *d == now).map_or(0, |i| (i + 1) % marks::WAIT_DAYS.len());
                this.model.wait = Some(marks::WAIT_DAYS[i]);
                this.schedule_autosave(cx);
                cx.notify();
            }));
        div()
            .flex()
            .flex_none()
            .items_center()
            .rounded(px(4.))
            .border_1()
            .border_color(l.stroke())
            .text_size(px(12.))
            .child(toggle)
            .child(div().text_color(l.text3()).child("·"))
            .child(count)
    }

    /// Категория письма: чип с цветом, меню категорий аккаунта.
    fn category_chip(&self, l: &Look, cx: &mut Context<Self>) -> Div {
        let current = self.model.category.as_deref().and_then(|k| self.categories.iter().find(|c| c.0.eq_ignore_ascii_case(k)));
        let chip = quiet("category", l)
            .border_1()
            .border_color(l.stroke())
            .text_size(px(12.))
            .when(self.menu == Menu::Category, |d| d.bg(l.hover()))
            .map(|d| match current {
                Some((_, name, color)) => d.child(div().size(px(8.)).rounded(px(2.)).bg(l.cat(color).dot)).child(name.clone()),
                None => d.text_color(l.text2()).child(icon(glyph::TAG, 12.)).child(tr!("compose-category")),
            })
            .on_click(cx.listener(|this, _, _, cx| this.set_menu(Menu::Category, cx)));
        let menu = (self.menu == Menu::Category).then(|| {
            let set = |value: Option<String>| {
                cx.listener(move |this: &mut Self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>| {
                    this.model.category = value.clone();
                    this.menu = Menu::None;
                    this.schedule_autosave(cx);
                    cx.notify();
                })
            };
            let none = menu_item("cat-none".into(), l, current.is_none()).child(tr!("compose-category-none")).on_click(set(None));
            let items: Vec<AnyElement> = self
                .categories
                .iter()
                .enumerate()
                .map(|(i, (keyword, name, color))| {
                    let on = current.is_some_and(|c| &c.0 == keyword);
                    menu_item(ElementId::NamedInteger("cat".into(), i as u64), l, on)
                        .child(div().size(px(10.)).rounded(px(2.)).bg(l.cat(color).dot))
                        .child(name.clone())
                        .on_click(set(Some(keyword.clone())))
                        .into_any_element()
                })
                .collect();
            let menu = menu_box("category-menu", l).top_auto().bottom(px(34.)).child(none).children(items);
            deferred(self.close_menu_out(menu, cx)).with_priority(1)
        });
        div().relative().child(chip).children(menu)
    }

    /// Нижняя строка: «Отправить ▾», Ctrl+Enter, «Ждать ответа», категория, состояние, «Удалить черновик».
    fn bottom_row(&self, l: &Look, cx: &mut Context<Self>) -> Div {
        let (status, error) = self.status_text();
        // Ответ в области чтения уже: подсказка клавиши и категория — во вкладке.
        let wide = self.place == Place::Tab;
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(20.))
            .py(px(10.))
            .border_t_1()
            .border_color(l.border())
            .child(self.send_group(l, cx))
            .when(wide, |d| d.child(div().w_px().h(px(20.)).mx(px(6.)).bg(l.stroke())))
            .child(self.wait_toggle(l, cx))
            .when(wide, |d| d.child(self.category_chip(l, cx)))
            // Состояние — справа, длинное обрезается (строка не переносится).
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .justify_end()
                    .text_size(px(12.))
                    .text_color(if error { l.error() } else { l.text2() })
                    .child(div().min_w_0().truncate().child(status)),
            )
            .child(
                quiet("delete-draft", l)
                    .w(px(30.))
                    .px(px(0.))
                    .justify_center()
                    .text_color(l.text2())
                    .child(icon(glyph::DELETE, 16.))
                    .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tr!("cmd-compose-discard")).build(window, cx))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.model.confirm_close = false;
                        this.model.confirm_delete = true;
                        cx.notify();
                    })),
            )
    }

    /// Заголовок и описание замечания.
    fn check_texts(&self, c: &Check) -> (String, String, String, String) {
        let external = self.model.recipients().iter().any(|a| checks::is_external(&a.email, &self.own));
        match c {
            Check::Card { .. } => (
                tr!("checks-card"),
                if external { tr!("checks-card-external") } else { tr!("checks-card-detail") },
                tr!("checks-mask"),
                tr!("checks-not-card"),
            ),
            Check::Secret { kind, .. } => (
                match kind {
                    Secret::Password => tr!("checks-password"),
                    Secret::Token => tr!("checks-token"),
                    Secret::PrivateKey => tr!("checks-private-key"),
                },
                tr!("checks-secret-detail"),
                tr!("checks-mask"),
                tr!("checks-not-secret"),
            ),
            Check::MissingAttachment { word } => {
                (tr!("checks-attachment", word = word.clone()), tr!("checks-attachment-detail"), tr!("checks-attach"), tr!("checks-ignore"))
            }
            Check::External { count, domains } => (
                tr!("checks-external", n = *count, domains = domains.join(", ")),
                tr!("checks-external-detail"),
                tr!("checks-review"),
                tr!("checks-ok"),
            ),
            Check::NoCertificate { names, .. } => (
                tr!("checks-no-cert", names = names.join(", ")),
                tr!("checks-no-cert-detail"),
                tr!("checks-no-cert-remove"),
                if self.confirm_unmark { tr!("checks-no-cert-unmark-confirm") } else { tr!("checks-no-cert-unmark") },
            ),
            Check::Split { gost } => (tr!("checks-split", names = gost.join(", ")), tr!("checks-split-detail"), tr!("checks-ok"), tr!("checks-ok")),
            Check::Moved { name, from, to, last } => (
                tr!("checks-moved", name = name.clone()),
                tr!("checks-moved-detail", from = from.clone(), to = to.clone(), when = ago(*last)),
                tr!("checks-replace"),
                tr!("checks-keep"),
            ),
        }
    }

    /// Замечание: знак, заголовок, описание, два действия. DLP — красной подложкой (роль безопасности).
    fn check_item(&self, i: usize, c: &Check, compact: bool, l: &Look, cx: &mut Context<Self>) -> AnyElement {
        let (title, detail, a1, a2) = self.check_texts(c);
        let dlp = c.blocks_send();
        let mark = div()
            .size(px(20.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .text_size(px(12.))
            .font_weight(FontWeight::SEMIBOLD)
            .when(dlp, |d| d.bg(l.pm(Pm::ThreatDot)).text_color(l.pm(Pm::ThreatBg)).child("!"))
            .when(!dlp, |d| d.border_1().border_color(l.text3()).text_color(l.text2()).child("i"));
        let action = |n: u64, text: String, primary: bool, c: Check, cx: &mut Context<Self>| {
            div()
                .id(ElementId::NamedInteger(if primary { "check-a1".into() } else { "check-a2".into() }, n))
                .cursor_pointer()
                .text_color(if primary { l.accent_text() } else { l.text2() })
                .hover(|d| d.underline())
                .child(text)
                .on_click(cx.listener(move |this, _, window, cx| this.check_action(c.clone(), primary, window, cx)))
        };
        let actions = div().flex().gap(px(12.)).text_size(px(12.)).child(action(i as u64, a1, true, c.clone(), cx)).child(action(
            i as u64,
            a2,
            false,
            c.clone(),
            cx,
        ));
        let text = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(if compact { 2. } else { 6. }))
            .child(div().text_size(px(13.)).line_height(px(18.)).when(dlp, |d| d.text_color(l.pm(Pm::ThreatFg))).child(title))
            .when(!compact, |d| d.child(div().text_size(px(12.)).text_color(l.text2()).child(detail)))
            .child(actions);
        div()
            .flex()
            .gap(px(10.))
            .p(px(if dlp { 8. } else { 0. }))
            .py(px(if dlp { 8. } else { 10. }))
            .when(dlp, |d| d.my(px(4.)).rounded(px(6.)).bg(l.pm(Pm::ThreatBg)))
            .when(!dlp && i > 0, |d| d.border_t_1().border_color(l.border()))
            .child(mark)
            .child(text)
            .into_any_element()
    }

    /// Карточка «Перед отправкой».
    fn checks_card(&self, l: &Look, cx: &mut Context<Self>) -> Div {
        let list: Vec<Check> = self.model.checks().cloned().collect();
        let count = if list.is_empty() { tr!("checks-none") } else { tr!("checks-count", n = list.len()) };
        let items: Vec<AnyElement> = list.iter().enumerate().map(|(i, c)| self.check_item(i, c, false, l, cx)).collect();
        card(l)
            .gap(px(2.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .pb(px(6.))
                    .child(card_title(tr!("checks-title")).flex_1())
                    .child(div().text_size(px(12.)).text_color(l.text2()).child(count)),
            )
            .when(list.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .py(px(6.))
                        .text_size(px(13.))
                        .text_color(l.text2())
                        .child(icon(glyph::CHECK, 14.).text_color(l.success()))
                        .child(tr!("checks-clear")),
                )
            })
            .children(items)
    }

    /// Карточка «Вложения»: файлы письма; упомянутый, но не вложенный — пунктиром с «Вложить файл».
    fn attachments_card(&self, l: &Look, cx: &mut Context<Self>) -> Div {
        let missing = self.model.checks().any(|c| matches!(c, Check::MissingAttachment { .. }));
        let blue = l.cat(postman_core::palette::BLUE);
        let files: Vec<AnyElement> = self
            .model
            .attachments()
            .map(|(i, a)| {
                let name = postman_core::files::safe_name(&a.name);
                let ext = name.rsplit_once('.').map(|(_, e)| e.to_uppercase()).unwrap_or_default();
                let ext: String = ext.chars().take(4).collect();
                let size = if a.data.is_empty() { String::new() } else { size_text(a.data.len() as u64) };
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(10.))
                    .py(px(8.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(l.stroke())
                    .text_size(px(13.))
                    .child(
                        div()
                            .w(px(30.))
                            .h(px(32.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(3.))
                            .bg(blue.bg)
                            .text_color(blue.text)
                            .text_size(px(9.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(ext),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().truncate().child(name))
                            .child(div().text_size(px(12.)).text_color(l.text2()).child(size)),
                    )
                    .child(
                        div()
                            .id(ElementId::NamedInteger("remove-attachment".into(), i as u64))
                            .size(px(24.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .text_color(l.text2())
                            .hover(|d| d.bg(l.hover()).text_color(l.error()))
                            .child(icon(glyph::CLOSE, 10.))
                            .on_click(cx.listener(move |this, _, _, cx| this.remove_attachment(i, cx))),
                    )
                    .into_any_element()
            })
            .collect();
        let empty = files.is_empty();
        card(l)
            .gap(px(8.))
            .child(card_title(tr!("compose-attachments")))
            .children(files)
            .when(missing, |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .px(px(10.))
                        .py(px(8.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(l.text3())
                        .text_size(px(13.))
                        .text_color(l.text2())
                        .child(icon(glyph::ATTACH, 16.))
                        .child(
                            div().flex_1().flex().flex_col().child(tr!("checks-attachment-card")).child(
                                div()
                                    .id("attach-missing")
                                    .text_size(px(12.))
                                    .text_color(l.accent_text())
                                    .cursor_pointer()
                                    .hover(|d| d.underline())
                                    .child(tr!("checks-attach"))
                                    .on_click(cx.listener(|this, _, _, cx| this.pick_files(cx))),
                            ),
                        ),
                )
            })
            .when(empty && !missing, |d| d.child(div().text_size(px(12.)).text_color(l.text2()).child(tr!("compose-attachments-empty"))))
    }

    /// Карточка «Отправить позже» (если сервер держит письма).
    fn later_card(&self, l: &Look, cx: &mut Context<Self>) -> Option<Div> {
        (self.later.max_delay > 0).then(|| {
            card(l)
                .gap(px(2.))
                .text_size(px(13.))
                .child(card_title(tr!("send-later-menu")).pb(px(4.)))
                .children(self.later_items(l, "later-side", cx).into_iter().map(|e| div().mx(px(-10.)).child(e)))
        })
    }

    /// Вложения строкой (ответ в области чтения: правой колонки нет).
    fn attachment_chips(&self, l: &Look, cx: &mut Context<Self>) -> Option<Div> {
        let chips: Vec<AnyElement> = self
            .model
            .attachments()
            .map(|(i, a)| {
                let size = if a.data.is_empty() { String::new() } else { format!(" ({})", size_text(a.data.len() as u64)) };
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_0p5()
                    .rounded(px(4.))
                    .bg(l.fill())
                    .text_size(px(12.))
                    .child(icon(glyph::ATTACH, 12.))
                    .child(format!("{}{size}", postman_core::files::safe_name(&a.name)))
                    .child(
                        div()
                            .id(ElementId::NamedInteger("remove-attachment".into(), i as u64))
                            .px_1()
                            .cursor_pointer()
                            .text_color(l.text2())
                            .hover(|d| d.text_color(l.error()))
                            .child(icon(glyph::CLOSE, 8.))
                            .on_click(cx.listener(move |this, _, _, cx| this.remove_attachment(i, cx))),
                    )
                    .into_any_element()
            })
            .collect();
        (!chips.is_empty()).then(|| div().flex().flex_wrap().gap_2().px(px(20.)).py(px(6.)).border_b_1().border_color(l.border()).children(chips))
    }

    /// Шапка ответа в области чтения: тема, «Развернуть», «×».
    fn inline_header(&self, l: &Look, cx: &mut Context<Self>) -> Div {
        let button = |id: &'static str, g: char, key: &'static str| {
            quiet(id, l)
                .text_color(l.pm(Pm::LabelAccent))
                .child(icon(g, 14.))
                .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tr!(key)).build(window, cx))
        };
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(20.))
            .pt(px(8.))
            .text_size(px(12.))
            .child(icon(glyph::REPLY, 14.).text_color(l.text2()))
            .child(div().flex_1().min_w_0().truncate().text_color(l.text2()).child(self.model.subject.clone()))
            .child(
                button("expand", glyph::EXPAND, "cmd-compose-expand")
                    .child(tr!("compose-expand"))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ComposerEvent::Expand))),
            )
            .child(
                button("inline-close", glyph::CLOSE, "cmd-compose-close").on_click(cx.listener(|this, _, window, cx| this.close_or_ask(window, cx))),
            )
    }

    /// Замечания строкой под текстом (ответ в области чтения).
    fn inline_checks(&self, l: &Look, cx: &mut Context<Self>) -> Option<Div> {
        let list: Vec<Check> = self.model.checks().cloned().collect();
        (!list.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .px(px(20.))
                .py(px(4.))
                .border_t_1()
                .border_color(l.border())
                .children(list.iter().enumerate().map(|(i, c)| self.check_item(i, c, true, l, cx)))
        })
    }
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let l = crate::roles::look(cx);
        // Оформление Markdown — в первый кадр и при смене темы (цвета — роли темы).
        if self.marks_look.as_ref().is_none_or(|was| !Rc::ptr_eq(was, &l)) {
            self.paint_markdown(cx);
        }
        if std::mem::take(&mut self.sync_inputs) {
            for f in [Field::To, Field::Cc, Field::Bcc] {
                let open = segments(self.model.field(f)).1;
                if let Some(state) = self.input(f) {
                    state.update(cx, |s, cx| s.set_value(open, window, cx));
                }
            }
            let subject = self.model.subject.clone();
            self.subject.update(cx, |s, cx| s.set_value(subject, window, cx));
            let body = self.model.body.clone();
            self.body.update(cx, |s, cx| s.set_value(body, window, cx));
            self.show_cc |= !self.model.cc.trim().is_empty();
            self.show_bcc |= !self.model.bcc.trim().is_empty();
        }
        self.refresh_recipient_checks();
        // Поиск сертификатов (S5-12 Ф4) — экрану почты, не посреди отрисовки.
        if let Some((addrs, from)) = self.resolve_pending.take() {
            let mailer = self.mailer.clone();
            cx.defer(move |cx| {
                mailer.update(cx, |m, cx| m.resolve_certs(addrs, from, cx)).ok();
            });
        }
        if let Some(n) = &mut self.screens {
            *n += 1;
            if *n < 8 {
                window.request_animation_frame();
            }
        }
        let inline = self.place == Place::Inline;
        let fields = div()
            .flex()
            .flex_col()
            .px(px(24.))
            .when(!inline, |d| d.pt(px(12.)))
            .when(!inline || self.senders.len() > 1, |d| d.child(self.sender_row(&l, cx)))
            .child(self.address_row(Field::To, "compose-to", &l, cx))
            .when(self.show_cc, |d| d.child(self.address_row(Field::Cc, "compose-cc", &l, cx)))
            .when(self.show_bcc, |d| d.child(self.address_row(Field::Bcc, "compose-bcc", &l, cx)))
            .when(!inline || !self.model.is_reply(), |d| d.child(self.subject_row(&l)));
        let editor = div()
            .size_full()
            .min_w_0()
            .flex()
            .flex_col()
            .when(inline, |d| d.child(self.inline_header(&l, cx)))
            .child(fields)
            .child(self.toolbar(&l, cx))
            .children(self.later_picker(&l, cx))
            .when(inline, |d| d.children(self.attachment_chips(&l, cx)))
            .child(self.content(window))
            .when(inline, |d| d.children(self.inline_checks(&l, cx)))
            // Ответ на письмо общего ящика (S5-28): «От» — свой адрес или письмо у коллеги.
            .children(self.shared.as_ref().and_then(|r| r.note(&self.model.from().email)).map(|t| crate::shared::note_line(t, &l).my(px(6.))))
            .children(self.confirm_lines(&l, cx))
            .child(self.bottom_row(&l, cx));
        let root = div()
            .key_context("Composer")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &compose::Send, _, cx| this.dispatch(Purpose::Send, cx)))
            .on_action(cx.listener(|this, _: &compose::Attach, _, cx| this.pick_files(cx)))
            .on_action(cx.listener(|this, _: &compose::Close, window, cx| this.escape(window, cx)))
            .on_action(cx.listener(|this, _: &compose::Bold, window, cx| this.format(Format::Bold, window, cx)))
            .on_action(cx.listener(|this, _: &compose::Italic, window, cx| this.format(Format::Italic, window, cx)))
            .on_action(cx.listener(|this, _: &compose::Underline, window, cx| this.format(Format::Underline, window, cx)))
            .on_action(cx.listener(|this, _: &compose::Bullets, window, cx| this.format(Format::Bullets, window, cx)))
            .on_action(cx.listener(|this, _: &compose::Numbers, window, cx| this.format(Format::Numbers, window, cx)))
            .on_action(cx.listener(|this, _: &compose::Link, window, cx| this.format(Format::Link, window, cx)))
            .on_action(cx.listener(|this, _: &compose::Quote, window, cx| this.format(Format::Quote, window, cx)))
            .on_action(cx.listener(|this, _: &compose::Expand, _, cx| {
                if this.place == Place::Inline {
                    cx.emit(ComposerEvent::Expand);
                }
            }))
            .on_action(cx.listener(|this, _: &compose::Discard, _, cx| {
                this.model.confirm_close = false;
                this.model.confirm_delete = true;
                cx.notify();
            }))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.attach(paths.paths().to_vec(), cx)))
            .drag_over::<ExternalPaths>({
                let hover = l.hover();
                move |s, _, _, _| s.bg(hover)
            })
            .size_full()
            .flex()
            .text_color(l.text())
            .text_size(px(13.));
        if inline {
            return root.bg(l.panel()).border_b_1().border_color(l.c(Role::StrokeCard)).child(editor);
        }
        let side = div()
            .id("compose-side")
            .w(px(W_SIDE))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(8.))
            .pb(px(8.))
            .overflow_y_scroll()
            .child(self.checks_card(&l, cx))
            .child(self.attachments_card(&l, cx))
            .children(self.later_card(&l, cx));
        root.gap(px(8.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .bg(l.panel())
                    .rounded_t(l.px(Metric::RadiusOverlay))
                    .border_1()
                    .border_b_0()
                    .border_color(l.c(Role::StrokeCard))
                    .overflow_hidden()
                    .child(editor),
            )
            .child(side)
    }
}

/// Сцены `--screens` (S5-06б): письмо во вкладке, ответ в области чтения, замечания «Перед отправкой».
pub(crate) mod scenes {
    use super::*;

    /// Образец адресата вне своих доменов (`partner.example`) для «Внешний» и замечаний.
    pub(crate) const PARTNER: &str = "partner.example";

    /// Письмо сцены `compose`: два адресата (один внешний), тема, текст со списком, вложение.
    pub(crate) fn compose_draft() -> Draft {
        Draft {
            to: vec![
                Address::new("Анна Смирнова", "anna.smirnova@corp.example"), // i18n-ok: образец письма
                Address::new("Сергей Волков", format!("s.volkov@{PARTNER}")), // i18n-ok: образец письма
            ],
            subject: "Отчёт за третий квартал".into(), // i18n-ok: образец письма
            markdown: "Анна, Сергей, добрый день!\n\nОтчёт за **третий квартал** прилагаю. Коротко:\n\n- выручка +12 %\n- расходы без изменений\n\nС уважением,\nПётр".into(), // i18n-ok: образец письма
            keywords: vec!["pm.cat.clients".into()],
            ..Default::default()
        }
    }

    /// Письмо сцены `checks`: номер карты, «во вложении» без файла, внешние при «Конфиденциально», сменённый адрес.
    pub(crate) fn checks_draft() -> Draft {
        Draft {
            to: vec![
                Address::new("Сергей Волков", format!("s.volkov@{PARTNER}")), // i18n-ok: образец письма
                Address::new("Ирина Лебедева", format!("i.lebedeva@{PARTNER}")), // i18n-ok: образец письма
            ],
            subject: "Коммерческое предложение — обновлённая редакция".into(), // i18n-ok: образец письма
            markdown: "Сергей, Ирина, добрый день!\n\nНаправляю обновлённое предложение. Расчёт стоимости — во вложении.\n\nРеквизиты для договора: карта 4111 1111 1111 1111 — уточню у бухгалтерии.\n\nС уважением,\nПётр".into(), // i18n-ok: образец письма
            keywords: vec![marks::CONFIDENTIAL.into(), "pm.cat.clients".into()],
            ..Default::default()
        }
    }

    /// «Конфиденциально» адресату с известным сертификатом (S5-12 Ф4, снимок `compose-confidential`).
    pub(crate) fn confidential_draft() -> Draft {
        Draft {
            to: vec![Address::new("Петров Иван", "i.petrov@corp.example")], // i18n-ok: образец письма
            subject: "Условия сделки — для согласования".into(),            // i18n-ok: образец письма
            markdown: "Иван, добрый день!\n\nНаправляю условия сделки. Письмо зашифровано.\n\nАнна".into(), // i18n-ok: образец письма
            keywords: vec![marks::CONFIDENTIAL.into()],
            ..Default::default()
        }
    }

    /// История адресной книги для замечания «писала с другого адреса»: новый адрес три дня назад.
    pub(crate) fn seed_book(book: &Book) {
        let now = jiff::Timestamp::now().as_second();
        let mut b = book.borrow_mut();
        let (old, new) = (format!("i.lebedeva@{PARTNER}"), format!("irina.l@{PARTNER}"));
        b.touch("Ирина Лебедева", &old, postman_core::contacts::Source::Recent, 2, now - 40 * 86_400); // i18n-ok: образец
        b.touch("Ирина Лебедева", &new, postman_core::contacts::Source::Recent, 1, now - 3 * 86_400); // i18n-ok: образец
    }

    /// Вложение образца.
    pub(crate) fn sample_attachment() -> Attachment {
        Attachment {
            name: "otchet-q3.xlsx".into(),
            mime: postman_core::files::mime_for("otchet-q3.xlsx").into(),
            data: vec![0; 48 * 1024],
            inline_cid: None,
            blob_id: None,
        }
    }
}

/// Оформление отрезка Markdown в поле письма: знаки разметки приглушены, ссылка и маркер списка — акцентом, цитата —
/// вторичным текстом, код — подложкой (роли темы).
pub(crate) fn md_style(m: &postman_compose::display::Look, l: &Look) -> HighlightStyle {
    let mut s = HighlightStyle::default();
    if m.strong || m.heading > 0 {
        s.font_weight = Some(FontWeight::BOLD);
    }
    if m.emphasis {
        s.font_style = Some(FontStyle::Italic);
    }
    if m.underline || m.link {
        s.underline = Some(UnderlineStyle { thickness: px(1.), color: None, wavy: false });
    }
    if m.strike {
        s.strikethrough = Some(StrikethroughStyle { thickness: px(1.), color: None });
    }
    if m.code {
        s.background_color = Some(l.fill());
    }
    // Роли текста бывают полупрозрачными, а цвет украшения ложится поверх цвета текста (`blend`): берём их видимый
    // цвет на подложке письма.
    let solid = |c: Hsla| l.panel().blend(c);
    s.color = if m.markup {
        Some(solid(l.text3()))
    } else if m.link || m.list {
        Some(solid(l.accent_text()))
    } else if m.quote {
        Some(solid(l.text2()))
    } else {
        None
    };
    s
}

/// Строка ошибки отправки; подпись S/MIME (код `smime-sign:…` от ядра) — своими словами.
fn failed_text(e: &str) -> String {
    if let Some(at) = e.find("smime-seal:") {
        let rest = &e[at + "smime-seal:".len()..];
        let (code, detail) = rest.split_once(':').unwrap_or((rest, ""));
        return match code {
            "no-cert" => tr!("compose-seal-no-cert", address = detail.to_owned()),
            "no-own-cert" => tr!("compose-seal-no-own-cert"),
            "need-skzi" => tr!("compose-seal-need-skzi"),
            _ => tr!("compose-seal-failed", error = detail.to_owned()),
        };
    }
    let Some(at) = e.find("smime-sign:") else { return tr!("compose-failed", error = e.to_owned()) };
    let rest = &e[at + "smime-sign:".len()..];
    let (code, detail) = rest.split_once(':').unwrap_or((rest, ""));
    match code {
        "need-pin" => tr!("compose-sign-need-pin"),
        "no-card" => tr!("compose-sign-no-card"),
        "cancelled" => tr!("compose-sign-cancelled"),
        "need-skzi" => tr!("compose-sign-need-skzi"),
        "no-cert" => tr!("compose-sign-no-cert", address = detail.to_owned()),
        "eight-bit" => tr!("compose-sign-eight-bit"),
        _ => tr!("compose-sign-failed", error = detail.to_owned()),
    }
}
