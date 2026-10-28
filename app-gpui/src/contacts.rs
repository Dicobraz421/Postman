//! Контакты (S4-02; вид — S5-11 по макету «Контакты и справочник», Ctrl+3): слева — источники («Часто пишу» — S5-25,
//! «Справочник компании», «Мои контакты»), список с буквами и указателем, справа — карточка: «Написать», «Встреча» (вкладка
//! встречи с этим участником), «Копировать адрес», поля в две колонки, занятость сегодня (сотрудники и переговорные).
//! Командная строка раздела — [`ContactsBar`] с поиском. Личный контакт меняется на месте (форма вместо карточки,
//! `ContactCard/set`), удаление — после строки «Удалить контакт? Да / Нет». Справочник — только просмотр; из него —
//! «Добавить в мои контакты».
//!
//! Демо: контакты из `postman_demo::contacts`; правка и удаление — только в памяти этого окна (на сервер ничего
//! не уходит, после перезапуска — снова демо-набор). Живой ящик: данные — событиями синхронизации
//! (`ContactsLoaded`, `DirectoryLoaded`), изменения — командами (`SaveContact`, `DeleteContact`), занятость —
//! `Availability`.
//!
//! Своя кэшируемая view: перерисовывается только по своему `notify`. С экраном говорит событиями [`ContactsAction`].

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::prelude::*;
use gpui_kit::*;
use jiff::tz::TimeZone;
use pets_i18n::tr;
use pets_ui::Level as Toast;
use pets_ui::theme::Metric;
use postman_calendar::{Busy, BusyKind, Span};
use postman_compose::{Address, Draft, parse_addresses};
use postman_core::contacts::AddressBook;
use postman_core::model::{ContactCard, Principal};
use postman_core::sync::{Command, Event as SyncEvent};

use crate::buttons::plain as button;
use crate::calendar::{add_days, avatar, chip_outline, day_start, g, hm};
use crate::chrome::icon;
use crate::roles::{Look, Pm};

const NAV_W: f32 = 236.;
const LIST_W: f32 = 452.;
const ROW_H: f32 = 52.;
const HEAD_H: f32 = 26.;
/// Занятость в карточке: часы дня.
const DAY_FROM: i64 = 8;
const DAY_TO: i64 = 20;

/// Откуда список: личная адресная книга, справочник организации или «Часто пишу» (S5-25: адресаты переписки за
/// 30 дней по частоте и давности, `AddressBook::frequent`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Source {
    Mine,
    Directory,
    Frequent,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Contact,
    Person,
    Room,
    Resource,
    /// Адресат переписки — ни в справочнике, ни в моих контактах («Часто пишу», S5-25): только чтение, можно добавить
    /// в мои контакты.
    Correspondent,
}

impl Kind {
    /// Тип Principal (RFC 9670): `individual`, `location`, `resource`…
    fn of_principal(kind: &str) -> Self {
        match kind {
            "location" => Kind::Room,
            "resource" => Kind::Resource,
            _ => Kind::Person,
        }
    }

    fn label(self) -> String {
        tr!(match self {
            Kind::Contact => "contacts-kind-contact",
            Kind::Person => "contacts-kind-person",
            Kind::Room => "contacts-kind-room",
            Kind::Resource => "contacts-kind-resource",
            Kind::Correspondent => "contacts-kind-correspondent",
        })
    }
}

/// Строка списка: карточка и слова для поиска (считаются один раз).
#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub(crate) card: ContactCard,
    pub(crate) kind: Kind,
    keys: Vec<String>,
    /// Цифры телефонов: «495 123» находит «+7 (495) 123-45-67».
    digits: Vec<String>,
}

impl Entry {
    pub(crate) fn new(card: ContactCard, kind: Kind) -> Self {
        let mut keys = Vec::new();
        for text in [&card.name, &card.organization, &card.title] {
            keys.extend(normalize(text).split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string));
        }
        for e in &card.emails {
            let e = normalize(e);
            keys.extend(e.split(['.', '-', '_', '@']).filter(|p| !p.is_empty()).map(str::to_string));
            keys.extend(e.split_once('@').map(|(_, domain)| domain.to_string()));
            keys.push(e);
        }
        let digits = card.phones.iter().map(|p| p.chars().filter(char::is_ascii_digit).collect()).collect();
        Self { card, kind, keys, digits }
    }

    /// Имя — ФИО из `description`, без него — `name` (у Stalwart это логин). Должность, отдел и телефон (S7-08) — в
    /// поля карточки: отдел — строкой «Организация» (подразделение там и у личных карточек: «Организация, отдел»).
    fn from_principal(p: Principal) -> Self {
        let name = p.display_name().to_owned();
        let card = ContactCard {
            id: p.id,
            name,
            emails: p.email.into_iter().collect(),
            phones: Some(p.phone.trim().to_owned()).filter(|t| !t.is_empty()).into_iter().collect(),
            organization: p.department.trim().to_owned(),
            title: p.title.trim().to_owned(),
        };
        Self::new(card, Kind::of_principal(&p.kind))
    }

    /// Слово запроса: начало слова имени, организации, должности или адреса; номер — часть телефона.
    fn matches_word(&self, word: &str) -> bool {
        let phone_like = word.chars().all(|c| c.is_ascii_digit() || "+-()".contains(c));
        let digits: String = word.chars().filter(char::is_ascii_digit).collect();
        if phone_like && !digits.is_empty() && self.digits.iter().any(|d| d.contains(&digits)) {
            return true;
        }
        self.keys.iter().any(|k| k.starts_with(word))
    }

    /// Весь запрос: каждое слово — начало слова или часть телефона; иначе — как в подсказках адреса (другая раскладка,
    /// транслит; `postman_core::contacts::matches`).
    fn matches(&self, words: &[String], query: &str) -> bool {
        words.iter().all(|w| self.matches_word(w))
            || self.card.emails.iter().any(|m| postman_core::contacts::matches(&self.card.name, m, query))
            || (self.card.emails.is_empty() && postman_core::contacts::matches(&self.card.name, "", query))
    }

    fn name(&self) -> String {
        match (self.card.name.trim(), self.card.emails.first()) {
            ("", Some(e)) => e.clone(),
            ("", None) => tr!("contacts-no-name"),
            (n, _) => n.to_string(),
        }
    }

    /// Буква указателя: первая буква имени, цифры и знаки — «#».
    fn letter(&self) -> char {
        let c = self.name().chars().next().map(|c| c.to_uppercase().next().unwrap_or(c)).unwrap_or('#');
        match c {
            'Ё' => 'Е', // i18n-ok: Ё в указателе — вместе с Е
            c if c.is_alphabetic() => c,
            _ => '#',
        }
    }
}

fn normalize(s: &str) -> String {
    s.to_lowercase().replace('ё', "е") // i18n-ok: ё = е в поиске
}

/// Индексы строк, подходящих под запрос: каждое слово запроса где-то совпало. Пустой запрос — все.
pub(crate) fn filter(entries: &[Entry], query: &str) -> Vec<usize> {
    let words: Vec<String> = normalize(query).split_whitespace().map(str::to_string).collect();
    if words.is_empty() {
        return (0..entries.len()).collect();
    }
    (0..entries.len()).filter(|&i| entries[i].matches(&words, query)).collect()
}

/// Адресат «Часто пишу» из адресной книги (`postman_core::contacts::Entry`): имя, адрес, последняя переписка
/// (Unix-секунды).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Frequent {
    pub(crate) name: String,
    pub(crate) email: String,
    pub(crate) last: i64,
}

/// Порядок «Часто пишу» (T1-24): как считает адресная книга (частота и давность), по имени, по адресу, сначала
/// недавние. Помнится на время работы.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Order {
    #[default]
    Frequency,
    Name,
    Address,
    Recent,
}

impl Order {
    const ALL: [Order; 4] = [Order::Frequency, Order::Name, Order::Address, Order::Recent];

    fn key(self) -> &'static str {
        match self {
            Order::Frequency => "contacts-sort-frequency",
            Order::Name => "contacts-sort-name",
            Order::Address => "contacts-sort-address",
            Order::Recent => "contacts-sort-recent",
        }
    }
}

/// Ключ сравнения имён с учётом языка, без регистра: ё = е, сначала цифры и знаки, затем буквы языка интерфейса
/// (`ru` — кириллица), затем остальные.
fn collate_key(s: &str, ru: bool) -> Vec<(u8, char)> {
    normalize(s)
        .chars()
        .map(|c| {
            let cyrillic = ('\u{0400}'..='\u{04FF}').contains(&c);
            let rank = match c {
                c if !c.is_alphabetic() => 0,
                _ if cyrillic == ru => 1,
                _ => 2,
            };
            (rank, c)
        })
        .collect()
}

/// «Часто пишу» (S5-25): адресаты переписки — запись справочника или моих контактов с этим адресом (та же карточка),
/// иначе — адресат переписки. Адрес — одна строка. Порядок `list` — частота (`AddressBook::frequent`); другие
/// порядки (T1-24) — по имени (`ru` — язык интерфейса), по адресу, по последней переписке.
pub(crate) fn frequent(list: &[Frequent], directory: &[Entry], mine: &[Entry], order: Order, ru: bool) -> Vec<Entry> {
    let has = |e: &Entry, email: &str| e.card.emails.iter().any(|m| m.eq_ignore_ascii_case(email));
    let mut out: Vec<(Entry, i64)> = Vec::new();
    for f in list {
        if out.iter().any(|(e, _)| has(e, &f.email)) {
            continue;
        }
        let known = directory.iter().find(|e| has(e, &f.email)).or_else(|| mine.iter().find(|e| has(e, &f.email)));
        let entry = match known {
            Some(e) => e.clone(),
            None => Entry::new(
                ContactCard { id: format!("freq:{}", f.email), name: f.name.clone(), emails: vec![f.email.clone()], ..Default::default() },
                Kind::Correspondent,
            ),
        };
        out.push((entry, f.last));
    }
    let email = |e: &Entry| e.card.emails.first().map(|m| m.to_lowercase()).unwrap_or_default();
    match order {
        Order::Frequency => {}
        Order::Name => out.sort_by_cached_key(|(e, _)| (collate_key(&e.name(), ru), email(e))),
        Order::Address => out.sort_by_cached_key(|(e, _)| email(e)),
        Order::Recent => out.sort_by_key(|(_, last)| std::cmp::Reverse(*last)),
    }
    out.into_iter().map(|(e, _)| e).collect()
}

/// По имени (без регистра), затем по адресу.
fn sort(entries: &mut [Entry]) {
    entries.sort_by_cached_key(|e| (normalize(&e.name()), e.card.emails.first().cloned()));
}

/// Пункт контекстного меню строки (T1-23): действия карточки правой панели.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MenuItem {
    Write,
    Meet,
    Copy,
    Edit,
    Delete,
    AddMine,
}

impl MenuItem {
    fn label(self) -> &'static str {
        match self {
            MenuItem::Write => "contacts-write",
            MenuItem::Meet => "contacts-meet",
            MenuItem::Copy => "contacts-copy",
            MenuItem::Edit => "contacts-edit",
            MenuItem::Delete => "contacts-delete",
            MenuItem::AddMine => "contacts-add-mine",
        }
    }

    fn glyph(self) -> char {
        match self {
            MenuItem::Write => g::MAIL,
            MenuItem::Meet => g::CALENDAR,
            MenuItem::Copy => g::COPY,
            MenuItem::Edit => g::EDIT,
            MenuItem::Delete => g::DELETE,
            MenuItem::AddMine => g::ADD,
        }
    }

    /// Команда реестра — клавиша справа.
    fn command(self) -> &'static str {
        match self {
            MenuItem::Write => "contacts.write",
            MenuItem::Meet => "contacts.meeting",
            MenuItem::Copy => "contacts.copy_address",
            MenuItem::Edit | MenuItem::AddMine => "contacts.edit",
            MenuItem::Delete => "contacts.delete",
        }
    }

    /// Пункт выбирает строку в панели: форма правки и строка «Удалить контакт?» живут в карточке справа. Письмо,
    /// встреча и адрес — над строкой под курсором, панель остаётся на своей записи.
    fn selects(self) -> bool {
        matches!(self, MenuItem::Edit | MenuItem::Delete | MenuItem::AddMine)
    }
}

/// Пункты меню строки группами, как кнопки карточки: с адресом — «Написать», «Встреча» (`meet` — у сервера есть
/// календарь), «Копировать адрес»; свой контакт — «Изменить», «Удалить»; справочник и переписка — только чтение,
/// «Добавить в мои контакты» (`personal` — у сервера есть личные контакты).
pub(crate) fn menu_items(e: &Entry, personal: bool, meet: bool) -> Vec<Vec<MenuItem>> {
    let mut groups = Vec::new();
    if !e.card.emails.is_empty() {
        let mut g = vec![MenuItem::Write];
        if meet {
            g.push(MenuItem::Meet);
        }
        g.push(MenuItem::Copy);
        groups.push(g);
    }
    if personal {
        groups.push(if e.kind == Kind::Contact { vec![MenuItem::Edit, MenuItem::Delete] } else { vec![MenuItem::AddMine] });
    }
    groups
}

/// Открытое контекстное меню строки: у точки окна, над записью `id`.
struct RowMenu {
    at: Point<Pixels>,
    id: String,
    groups: Vec<Vec<MenuItem>>,
    cursor: usize,
    focus: FocusHandle,
    /// Фокус до меню — вернуть при закрытии.
    before: Option<FocusHandle>,
    focus_pending: bool,
}

impl RowMenu {
    fn items(&self) -> impl Iterator<Item = MenuItem> + '_ {
        self.groups.iter().flatten().copied()
    }
}

/// Строка списка: буква-заголовок (буква, сколько) или контакт (индекс в источнике).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Item {
    Letter(char, usize),
    /// Группа поиска по всем источникам (S5-25): источник и сколько найдено.
    Group(Source, usize),
    Row(usize),
}

/// Список с буквами: строки `shown` (уже по алфавиту) — под заголовком своей буквы. При поиске букв нет.
pub(crate) fn items(entries: &[Entry], shown: &[usize], letters: bool) -> Vec<Item> {
    if !letters {
        return shown.iter().map(|&i| Item::Row(i)).collect();
    }
    let mut out = Vec::new();
    let mut k = 0;
    while k < shown.len() {
        let letter = entries[shown[k]].letter();
        let n = shown[k..].iter().take_while(|&&i| entries[i].letter() == letter).count();
        out.push(Item::Letter(letter, n));
        out.extend(shown[k..k + n].iter().map(|&i| Item::Row(i)));
        k += n;
    }
    out
}

// --- форма карточки (без gpui) ---

/// Текст полей формы.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct FormText {
    pub(crate) name: String,
    pub(crate) emails: String,
    pub(crate) phones: String,
    pub(crate) organization: String,
    pub(crate) title: String,
}

#[derive(Debug, PartialEq)]
pub(crate) enum FormError {
    NeedName,
    BadEmail(String),
}

impl FormError {
    fn text(&self) -> String {
        match self {
            FormError::NeedName => tr!("contacts-need-name"),
            FormError::BadEmail(e) => tr!("contacts-bad-email", email = e.clone()),
        }
    }
}

impl FormText {
    pub(crate) fn of(card: &ContactCard) -> Self {
        Self {
            name: card.name.clone(),
            emails: card.emails.join("; "),
            phones: card.phones.join("; "),
            organization: card.organization.clone(),
            title: card.title.clone(),
        }
    }

    /// Карточка из полей (`id` — прежней карточки, у новой пустой): адреса и телефоны — через `;` или `,`.
    pub(crate) fn card(&self, id: &str) -> Result<ContactCard, FormError> {
        let (ok, bad) = parse_addresses(&self.emails);
        if let Some(b) = bad.into_iter().next() {
            return Err(FormError::BadEmail(b));
        }
        let card = ContactCard {
            id: id.to_string(),
            name: self.name.trim().to_string(),
            emails: ok.into_iter().map(|a| a.email).collect(),
            phones: self.phones.split([';', ',']).map(str::trim).filter(|p| !p.is_empty()).map(str::to_string).collect(),
            organization: self.organization.trim().to_string(),
            title: self.title.trim().to_string(),
        };
        if card.name.is_empty() && card.emails.is_empty() {
            return Err(FormError::NeedName);
        }
        Ok(card)
    }
}

/// Поля формы на экране.
struct Form {
    /// Прежняя карточка (правка); `None` — новая.
    old: Option<ContactCard>,
    name: Entity<InputState>,
    emails: Entity<InputState>,
    phones: Entity<InputState>,
    organization: Entity<InputState>,
    title: Entity<InputState>,
    error: Option<String>,
}

impl Form {
    fn text(&self, cx: &App) -> FormText {
        let v = |s: &Entity<InputState>| s.read(cx).value().to_string();
        FormText { name: v(&self.name), emails: v(&self.emails), phones: v(&self.phones), organization: v(&self.organization), title: v(&self.title) }
    }
}

enum Mode {
    View,
    Edit(Box<Form>),
    /// Строка «Удалить контакт? Да / Нет».
    Confirm,
}

/// Страниц справочника (T1-25г) — по столько людей.
pub(crate) const DIR_PAGE: u32 = 100;

/// Справочник страницами (T1-25г): первая — при открытии, следующая — у конца прокрутки; один запрос на страницу.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct DirPages {
    /// С какого человека следующая страница.
    next: u32,
    /// Запрошенная страница, ответа ещё нет.
    asked: Option<u32>,
    /// Всего людей, если путь знает.
    pub(crate) total: Option<u32>,
    /// Справочник у сервера только поиском (EWS): страниц нет, поле поиска.
    pub(crate) search_only: bool,
    /// Страниц больше нет.
    done: bool,
}

impl DirPages {
    /// Следующая страница — если не запрошена и есть ещё: её `offset` (запрос уходит один раз).
    pub(crate) fn want(&mut self) -> Option<u32> {
        if self.asked.is_some() || self.done || self.search_only {
            return None;
        }
        self.asked = Some(self.next);
        self.asked
    }

    /// Ответ на страницу `offset`: `n` людей. `false` — не та страница (устарела).
    pub(crate) fn got(&mut self, offset: u32, n: u32, total: Option<u32>, search_only: bool) -> bool {
        if self.asked != Some(offset) {
            return false;
        }
        self.asked = None;
        self.next = offset + n;
        self.total = total;
        self.search_only = search_only;
        self.done = search_only || n < DIR_PAGE || total.is_some_and(|t| self.next >= t);
        true
    }
}

/// Что раздел просит у экрана: сервер, письмо, встреча, тост.
pub(crate) enum ContactsAction {
    /// Перечитать контакты и справочник (`Command::Contacts`).
    Load,
    /// Страница справочника (T1-25г, `Command::DirectoryPage`).
    DirectoryPage(u32),
    /// Подробности человека справочника при открытии карточки (`Command::PersonDetails`, S7-08б): телефон, которого
    /// не дал поиск (у Exchange — `ResolveNames`).
    PersonDetails(String),
    Save {
        card: Box<ContactCard>,
        old: Option<Box<ContactCard>>,
        ticket: u64,
    },
    Delete {
        id: String,
        ticket: u64,
    },
    /// Занятость адреса на сутки (`Command::Availability`).
    Availability {
        ticket: u64,
        email: String,
        from: i64,
        to: i64,
    },
    /// Поиск в справочнике по строке (`Command::FindPeople`, S7-08): у Exchange справочник — только поиском.
    FindPeople(String),
    /// Новое письмо с адресом.
    Write(Address),
    /// Вкладка встречи с этим участником.
    Meet(Address),
    Toast(Toast, String),
}

/// Занятость выбранного сегодня: адрес, тикет запроса, ответ (`None` в ответе — нет данных).
#[derive(Default)]
struct Today {
    email: String,
    ticket: Option<u64>,
    busy: Option<Option<Vec<Busy>>>,
}

pub(crate) struct ContactsView {
    /// Экран в фокусе (T1-D): выбранная строка — тоном акцента, иначе нейтральной заливкой.
    pub(crate) focused: bool,
    live: bool,
    source: Source,
    mine: Vec<Entry>,
    directory: Vec<Entry>,
    /// «Часто пишу»: адресаты из адресной книги по частоте и строки списка из них (S5-25) в выбранном порядке (T1-24).
    frequent_src: Vec<Frequent>,
    frequent: Vec<Entry>,
    order: Order,
    /// Открыт выбор порядка «Часто пишу».
    order_menu: bool,
    /// Поиск — по всем трём источникам сразу: найденные группами (справочник, мои, переписка).
    pool: Vec<Entry>,
    /// Ждём контакты с сервера.
    loading: bool,
    /// Справочник страницами (T1-25г).
    pages: DirPages,
    /// Контакты пришли с сервера (снимки `--screens --live` ждут их).
    pub(crate) loaded: bool,
    pub(crate) search: Entity<InputState>,
    query: String,
    /// Индексы показанных строк текущего источника (после поиска).
    shown: Vec<usize>,
    /// Строки списка с буквами.
    items: Vec<Item>,
    list: ListState,
    selected: Option<String>,
    mode: Mode,
    /// Тикет сохранения или удаления, которого ждём с сервера.
    pending: Option<u64>,
    next_ticket: u64,
    next_local: u64,
    /// Ошибка загрузки — строкой над списком.
    error: Option<String>,
    /// Демо: о том, что правка только в окне, — один тост за запуск.
    demo_noted: bool,
    /// Снимок `contacts`: выделить первую строку, как только она появится (живой ящик — после ответа сервера).
    want_first: bool,
    /// Мой адрес: «Внешний» у чужих доменов, заголовок источников.
    me: Option<String>,
    today: Today,
    /// Контекстное меню строки (T1-23).
    menu: Option<RowMenu>,
    /// Адреса, чьи подробности уже спрошены (`PersonDetails`): карточка спрашивает один раз.
    details_asked: std::collections::HashSet<String>,
    tz: TimeZone,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ContactsAction> for ContactsView {}

impl ContactsView {
    /// Демо-контакты; живой ящик переключает [`ContactsView::go_live`].
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("contacts-search-placeholder")));
        let on_search = cx.subscribe(&search, |this: &mut Self, state, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                this.query = state.read(cx).value().to_string();
                if this.live {
                    cx.emit(ContactsAction::FindPeople(this.query.clone()));
                }
                this.refilter(cx);
            }
        });
        let mut this = Self {
            focused: true,
            live: false,
            source: Source::Directory,
            mine: demo_mine(),
            directory: demo_directory(),
            frequent_src: Vec::new(),
            frequent: Vec::new(),
            order: Order::default(),
            order_menu: false,
            pool: Vec::new(),
            loading: false,
            pages: DirPages::default(),
            loaded: false,
            search,
            query: String::new(),
            shown: Vec::new(),
            items: Vec::new(),
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            selected: None,
            mode: Mode::View,
            pending: None,
            next_ticket: 1,
            next_local: 1,
            error: None,
            demo_noted: false,
            want_first: false,
            me: Some(postman_demo::ME.to_string()),
            today: Today::default(),
            menu: None,
            details_asked: std::collections::HashSet::new(),
            tz: crate::prefs::get(cx).calendar.zone(),
            _subscriptions: vec![on_search],
        };
        this.reshow();
        this
    }

    /// Живой ящик: контакты — с сервера (поток синхронизации присылает их сам после входа).
    pub(crate) fn go_live(&mut self, cx: &mut Context<Self>) {
        self.live = true;
        self.mine.clear();
        self.directory.clear();
        self.loading = true;
        self.loaded = false;
        self.selected = None;
        self.mode = Mode::View;
        self.pending = None;
        self.me = None;
        self.today = Today::default();
        self.refilter(cx);
    }

    /// Раздел открыт: перечитать с сервера.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.live {
            self.loading = true;
            cx.emit(ContactsAction::Load);
            // Справочник — с первой страницы (T1-25г).
            self.pages = DirPages::default();
            self.next_page(cx);
            cx.notify();
        }
    }

    /// Следующая страница справочника, если её ещё не просили (T1-25г).
    fn next_page(&mut self, cx: &mut Context<Self>) {
        if !self.live {
            return;
        }
        if let Some(offset) = self.pages.want() {
            cx.emit(ContactsAction::DirectoryPage(offset));
        }
    }

    /// Поток синхронизации не принял команду: ждать нечего.
    pub(crate) fn not_sent(&mut self, cx: &mut Context<Self>) {
        self.loading = false;
        self.pending = None;
        self.today.ticket = None;
        cx.notify();
    }

    fn entries(&self) -> &[Entry] {
        if self.searching() {
            return &self.pool;
        }
        match self.source {
            Source::Mine => &self.mine,
            Source::Directory => &self.directory,
            Source::Frequent => &self.frequent,
        }
    }

    fn searching(&self) -> bool {
        !self.query.trim().is_empty()
    }

    /// «Часто пишу» из адресной книги (экран — при открытии раздела и когда книга собрана).
    pub(crate) fn set_frequent(&mut self, list: Vec<Frequent>, cx: &mut Context<Self>) {
        if self.frequent_src != list {
            self.frequent_src = list;
            self.refilter(cx);
        }
    }

    /// Открыть «Часто пишу» с первой строкой (снимок `contacts-frequent`).
    pub(crate) fn show_frequent(&mut self, cx: &mut Context<Self>) {
        self.set_source(Source::Frequent, cx);
        let first = self.frequent.first().map(|e| e.card.id.clone());
        if let Some(id) = first {
            self.select(id, cx);
        }
    }

    /// Открыт «Часто пишу» и в нём выбрана строка.
    pub(crate) fn frequent_shown(&self) -> bool {
        self.source == Source::Frequent && !self.frequent.is_empty() && self.selected.is_some()
    }

    fn selected_entry(&self) -> Option<&Entry> {
        self.entry(self.selected.as_deref()?)
    }

    fn entry(&self, id: &str) -> Option<&Entry> {
        self.entries().iter().find(|e| e.card.id == id)
    }

    /// Адреса моих контактов: автоответ «только контактам» (S5-29).
    pub(crate) fn personal_emails(&self) -> Vec<String> {
        self.mine.iter().flat_map(|e| e.card.emails.iter().cloned()).collect()
    }

    /// Переговорные и ресурсы справочника — во вкладку встречи: (имя, адрес).
    pub(crate) fn rooms(&self) -> Vec<(String, String)> {
        self.directory
            .iter()
            .filter(|e| matches!(e.kind, Kind::Room | Kind::Resource))
            .filter_map(|e| e.card.emails.first().map(|m| (e.name(), m.clone())))
            .collect()
    }

    fn reshow(&mut self) {
        let ru = !pets_i18n::current().lang().starts_with("en");
        self.frequent = frequent(&self.frequent_src, &self.directory, &self.mine, self.order, ru);
        if self.searching() {
            // По всем трём источникам: справочник, мои, адресаты переписки (кого нет ни там, ни там).
            let groups: [(Source, Vec<Entry>); 3] = [
                (Source::Directory, self.directory.clone()),
                (Source::Mine, self.mine.clone()),
                (Source::Frequent, self.frequent.iter().filter(|e| e.kind == Kind::Correspondent).cloned().collect()),
            ];
            self.pool.clear();
            self.items.clear();
            for (source, entries) in groups {
                let found: Vec<Entry> = filter(&entries, &self.query).into_iter().map(|i| entries[i].clone()).collect();
                if found.is_empty() {
                    continue;
                }
                self.items.push(Item::Group(source, found.len()));
                let start = self.pool.len();
                self.pool.extend(found);
                self.items.extend((start..self.pool.len()).map(Item::Row));
            }
            self.shown = (0..self.pool.len()).collect();
        } else {
            self.pool.clear();
            self.shown = (0..self.entries().len()).collect();
            // «Часто пишу» — в своём порядке, без букв.
            self.items = items(self.entries(), &self.shown, self.source != Source::Frequent);
        }
        self.list.reset(self.items.len());
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        self.reshow();
        cx.notify();
    }

    fn set_source(&mut self, source: Source, cx: &mut Context<Self>) {
        if self.source != source {
            self.source = source;
            self.selected = None;
            self.mode = Mode::View;
            self.refilter(cx);
        }
    }

    fn select(&mut self, id: String, cx: &mut Context<Self>) {
        if matches!(self.mode, Mode::Edit(_)) && self.selected.as_deref() == Some(id.as_str()) {
            return;
        }
        self.selected = Some(id);
        self.mode = Mode::View;
        self.ask_today(cx);
        self.ask_details(cx);
        cx.notify();
    }

    /// Карточка человека справочника без телефона: подробности с сервера (S7-08б, решение владельца 2026-10-23 —
    /// у Exchange телефон даёт `ResolveNames`, `FindPeople` его не отдаёт). Один раз на адрес; демо — нет.
    fn ask_details(&mut self, cx: &mut Context<Self>) {
        let Some(e) = self.selected_entry() else { return };
        if !self.live || e.kind != Kind::Person || !e.card.phones.is_empty() {
            return;
        }
        let Some(email) = e.card.emails.first().map(|m| m.to_lowercase()) else { return };
        if self.details_asked.insert(email.clone()) {
            cx.emit(ContactsAction::PersonDetails(email));
        }
    }

    /// Занятость выбранного сегодня: сотрудники и переговорные (у личных контактов — нет).
    fn ask_today(&mut self, cx: &mut Context<Self>) {
        let Some(e) = self.selected_entry() else { return };
        let Some(email) = e.card.emails.first().cloned().filter(|_| matches!(e.kind, Kind::Person | Kind::Room | Kind::Resource)) else {
            self.today = Today::default();
            return;
        };
        if self.today.email.eq_ignore_ascii_case(&email) {
            return;
        }
        let day = jiff::Zoned::now().with_time_zone(self.tz.clone()).date();
        let (from, to) = (day_start(day, &self.tz), day_start(add_days(day, 1), &self.tz));
        if !self.live {
            let busy = postman_demo::calendar::colleagues()
                .into_iter()
                .find(|(_, m, _)| m.eq_ignore_ascii_case(&email))
                .map(|(_, _, b)| shift_to_today(b, from, &self.tz));
            self.today = Today { email, ticket: None, busy: Some(busy) };
            return;
        }
        let ticket = self.ticket();
        self.today = Today { email: email.clone(), ticket: Some(ticket), busy: None };
        cx.emit(ContactsAction::Availability { ticket, email, from, to });
    }

    /// Первая строка списка (снимок `contacts`); в справочнике пусто — первый личный контакт.
    pub(crate) fn select_first(&mut self, cx: &mut Context<Self>) {
        self.want_first = true;
        if self.directory.is_empty() && !self.mine.is_empty() {
            self.set_source(Source::Mine, cx);
        }
        // Снимок — сотрудник с занятостью (не «Бухгалтерия»): первый, у кого есть должность или занятость.
        let pick = self
            .shown
            .iter()
            .filter_map(|&i| self.entries().get(i))
            .find(|e| e.kind == Kind::Person && e.card.emails.first().is_some_and(|m| m.split('@').next().is_some_and(|p| p.contains('.'))))
            .or_else(|| self.shown.first().and_then(|&i| self.entries().get(i)))
            .map(|e| e.card.id.clone());
        self.selected = pick;
        self.mode = Mode::View;
        self.ask_today(cx);
        cx.notify();
    }

    /// Выделить запись с адресом (снимок `contacts` в демо: коллега с занятостью).
    pub(crate) fn select_email(&mut self, email: &str, cx: &mut Context<Self>) {
        let found = self.entries().iter().find(|e| e.card.emails.iter().any(|m| m.eq_ignore_ascii_case(email))).map(|e| e.card.id.clone());
        if let Some(id) = found {
            self.select(id, cx);
        }
    }

    /// Что показывать уже есть: демо — сразу, живой ящик — после ответа сервера.
    pub(crate) fn ready(&self) -> bool {
        (!self.live || self.loaded) && (self.selected.is_some() || self.shown.is_empty()) && self.today.ticket.is_none()
    }

    /// Правка первого личного контакта (снимок `contact-edit`).
    pub(crate) fn edit_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let card = match self.selected_entry().filter(|e| e.kind == Kind::Contact) {
            Some(e) => Some(e.card.clone()),
            None => self.mine.first().map(|e| e.card.clone()),
        };
        if let Some(card) = card {
            self.set_source(Source::Mine, cx);
            self.selected = Some(card.id.clone());
            self.start_edit(Some(card), window, cx);
        }
    }

    pub(crate) fn editing(&self) -> bool {
        matches!(self.mode, Mode::Edit(_))
    }

    /// Ctrl+E в разделе.
    pub(crate) fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| s.focus(window, cx));
    }

    /// Esc: меню строки, строка подтверждения, затем форма, затем поиск. `false` — сбрасывать было нечего.
    pub(crate) fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.menu.is_some() {
            self.close_menu(window, cx);
            return true;
        }
        match self.mode {
            Mode::Confirm | Mode::Edit(_) => {
                self.mode = Mode::View;
                self.pending = None;
            }
            Mode::View if !self.query.is_empty() => {
                self.search.update(cx, |s, cx| s.set_value("", window, cx));
                self.query.clear();
                self.reshow();
            }
            Mode::View => return false,
        }
        cx.notify();
        true
    }

    /// «Новый контакт» (Ctrl+Shift+C).
    pub(crate) fn new_contact(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.start_edit(None, window, cx);
    }

    /// «Изменить» (F2): личный контакт — форма; сотрудник справочника — новый личный с его именем и адресом.
    pub(crate) fn edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.selected_entry().map(|e| (e.kind, e.card.clone())) {
            Some((Kind::Contact, card)) => self.start_edit(Some(card), window, cx),
            Some((_, card)) => self.copy_to_mine(card, window, cx),
            None => {}
        }
    }

    /// «Удалить» (Delete): строка подтверждения у личного контакта.
    pub(crate) fn ask_delete(&mut self, cx: &mut Context<Self>) {
        if self.selected_entry().is_some_and(|e| e.kind == Kind::Contact) && matches!(self.mode, Mode::View) {
            self.mode = Mode::Confirm;
            cx.notify();
        }
    }

    /// Адрес записи: (имя, адрес).
    fn address(&self, id: &str) -> Option<Address> {
        let e = self.entry(id)?;
        e.card.emails.first().map(|m| Address::new(&e.card.name, m))
    }

    fn selected_address(&self) -> Option<Address> {
        self.address(self.selected.as_deref()?)
    }

    pub(crate) fn write(&mut self, cx: &mut Context<Self>) {
        if let Some(a) = self.selected_address() {
            cx.emit(ContactsAction::Write(a));
        }
    }

    pub(crate) fn meet(&mut self, cx: &mut Context<Self>) {
        if let Some(a) = self.selected_address() {
            cx.emit(ContactsAction::Meet(a));
        }
    }

    pub(crate) fn copy_address(&mut self, cx: &mut Context<Self>) {
        if let Some(a) = self.selected_address() {
            self.copy(a, cx);
        }
    }

    fn copy(&mut self, a: Address, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(a.email.clone()));
        cx.emit(ContactsAction::Toast(Toast::Info, tr!("contacts-copied", email = a.email)));
    }

    // --- контекстное меню строки (T1-23) ---

    /// Меню над записью `id` у точки окна `at` (правая кнопка; с клавиатуры — у строки).
    fn open_menu(&mut self, id: String, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(e) = self.entry(&id) else { return };
        let features = crate::features::get(cx);
        let groups = menu_items(e, crate::features::available("contacts.new", features), crate::features::available("contacts.meeting", features));
        if groups.is_empty() {
            return;
        }
        let before = window.focused(cx);
        self.menu = Some(RowMenu { at, id, groups, cursor: 0, focus: cx.focus_handle(), before, focus_pending: true });
        cx.notify();
    }

    /// `contacts.context_menu` (Shift+F10, «Меню»): у выбранной строки.
    pub(crate) fn context_menu_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected.clone() else { return };
        let ix = self.items.iter().position(|it| matches!(it, Item::Row(i) if self.entries().get(*i).is_some_and(|e| e.card.id == id)));
        let at = ix
            .and_then(|ix| self.list.bounds_for_item(ix))
            .map(|b| point(b.origin.x + px(140.), b.origin.y + px(ROW_H * 0.6)))
            .unwrap_or_else(|| point(px(NAV_W + 140.), px(200.)));
        self.open_menu(id, at, window, cx);
    }

    /// Снимки: меню у записи с адресом `email` (как правая кнопка по её строке).
    pub(crate) fn screen_menu(&mut self, email: &str, window: &mut Window, cx: &mut Context<Self>) {
        let found = self.entries().iter().find(|e| e.card.emails.iter().any(|m| m.eq_ignore_ascii_case(email))).map(|e| e.card.id.clone());
        let Some(id) = found else { return };
        let ix = self.items.iter().position(|it| matches!(it, Item::Row(i) if self.entries().get(*i).is_some_and(|e| e.card.id == id)));
        let at = ix
            .and_then(|ix| self.list.bounds_for_item(ix))
            .map(|b| point(b.origin.x + px(180.), b.origin.y + px(ROW_H * 0.6)))
            .unwrap_or_else(|| point(px(NAV_W + 180.), px(260.)));
        self.open_menu(id, at, window, cx);
    }

    pub(crate) fn menu_open(&self) -> bool {
        self.menu.is_some()
    }

    fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(m) = self.menu.take() {
            if let Some(f) = m.before {
                window.focus(&f, cx);
            }
            cx.notify();
        }
    }

    /// Пункт выбран: письмо, встреча и адрес — над строкой меню, без смены записи в панели; правка и удаление —
    /// в карточке этой записи.
    fn choose(&mut self, it: MenuItem, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.menu.as_ref().map(|m| m.id.clone()) else { return };
        self.close_menu(window, cx);
        if it.selects() {
            self.select(id.clone(), cx);
        }
        match it {
            MenuItem::Write | MenuItem::Meet | MenuItem::Copy => {
                let Some(a) = self.address(&id) else { return };
                match it {
                    MenuItem::Write => cx.emit(ContactsAction::Write(a)),
                    MenuItem::Meet => cx.emit(ContactsAction::Meet(a)),
                    _ => self.copy(a, cx),
                }
            }
            MenuItem::Edit | MenuItem::AddMine => self.edit(window, cx),
            MenuItem::Delete => self.ask_delete(cx),
        }
    }

    /// Клавиши меню, как у меню письма: стрелки, Home/End, Enter и пробел — выполнить (Esc — `search.clear`, `escape`).
    fn menu_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(m) = &mut self.menu else { return };
        let n = m.items().count();
        let chosen = m.items().nth(m.cursor);
        match ev.keystroke.key.as_str() {
            "down" => m.cursor = (m.cursor + 1) % n,
            "up" => m.cursor = (m.cursor + n - 1) % n,
            "home" => m.cursor = 0,
            "end" => m.cursor = n - 1,
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

    /// Меню поверх экрана: слой и строки — как у контекстного меню письма (`menu.rs`).
    fn row_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let m = self.menu.as_mut()?;
        if std::mem::take(&mut m.focus_pending) {
            window.focus(&m.focus, cx);
        }
        let l = crate::roles::look(cx);
        let m = self.menu.as_ref()?;
        let mut col = div().flex().flex_col();
        let mut i = 0usize;
        for (k, group) in m.groups.iter().enumerate() {
            if k > 0 {
                col = col.child(crate::bulk::separator(&l));
            }
            for &it in group {
                let n = i;
                i += 1;
                let key = crate::bulk::key_hint(it.command(), cx);
                col = col.child(
                    crate::bulk::menu_line(("contact-menu-item", n), &l)
                        .when(m.cursor == n, |d| d.bg(l.hover()))
                        .child(icon(it.glyph(), 16.).text_color(l.text2()))
                        .child(div().flex_1().min_w_0().truncate().child(tr!(it.label())))
                        .when(!key.is_empty(), |d| d.child(div().text_size(px(11.)).text_color(l.text3()).child(key)))
                        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                            if let Some(m) = &mut this.menu
                                && *hovered
                                && m.cursor != n
                            {
                                m.cursor = n;
                                cx.notify();
                            }
                        }))
                        .on_click(cx.listener(move |this, _, window, cx| this.choose(it, window, cx))),
                );
            }
        }
        let body = div()
            .id("contact-menu")
            .key_context("ContextMenu")
            .track_focus(&m.focus)
            .occlude()
            .w(px(260.))
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
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close_menu(window, cx)));
        Some(deferred(anchored().position(m.at).snap_to_window_with_margin(px(8.)).child(body)).with_priority(2))
    }

    /// Сотрудник справочника → новый личный контакт (форма с его именем и адресом).
    fn copy_to_mine(&mut self, card: ContactCard, window: &mut Window, cx: &mut Context<Self>) {
        let prefill = ContactCard { id: String::new(), ..card };
        self.set_source(Source::Mine, cx);
        self.start_form(None, Some(prefill), window, cx);
    }

    fn start_edit(&mut self, old: Option<ContactCard>, window: &mut Window, cx: &mut Context<Self>) {
        let prefill = old.clone();
        self.start_form(old, prefill, window, cx);
    }

    fn start_form(&mut self, old: Option<ContactCard>, prefill: Option<ContactCard>, window: &mut Window, cx: &mut Context<Self>) {
        let text = prefill.as_ref().map(FormText::of).unwrap_or_default();
        let mut field = |value: String, placeholder: Option<&str>, cx: &mut Context<Self>| {
            let state = cx.new(|cx| {
                let s = InputState::new(window, cx).default_value(value);
                match placeholder {
                    Some(p) => s.placeholder(tr!(p)),
                    None => s,
                }
            });
            self._subscriptions.push(cx.subscribe_in(&state, window, |this, _, ev: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    this.save(cx);
                }
            }));
            state
        };
        let name = field(text.name, Some("contacts-name-placeholder"), cx);
        let emails = field(text.emails, Some("contacts-emails-placeholder"), cx);
        let phones = field(text.phones, Some("contacts-phones-placeholder"), cx);
        let organization = field(text.organization, None, cx);
        let title = field(text.title, None, cx);
        name.update(cx, |s, cx| s.focus(window, cx));
        if old.is_none() {
            self.set_source(Source::Mine, cx);
        }
        self.pending = None;
        self.mode = Mode::Edit(Box::new(Form { old, name, emails, phones, organization, title, error: None }));
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let Mode::Edit(form) = &mut self.mode else { return };
        if self.pending.is_some() {
            return;
        }
        let id = form.old.as_ref().map(|c| c.id.clone()).unwrap_or_default();
        let card = match form.text(cx).card(&id) {
            Ok(c) => c,
            Err(e) => {
                form.error = Some(e.text());
                cx.notify();
                return;
            }
        };
        form.error = None;
        let old = form.old.clone();
        if !self.live {
            return self.save_local(card, old.is_none(), cx);
        }
        let ticket = self.ticket();
        self.pending = Some(ticket);
        cx.emit(ContactsAction::Save { card: Box::new(card), old: old.map(Box::new), ticket });
        cx.notify();
    }

    /// Демо: правка в памяти окна.
    fn save_local(&mut self, mut card: ContactCard, new: bool, cx: &mut Context<Self>) {
        if new {
            card.id = format!("local-{}", self.next_local);
            self.next_local += 1;
        }
        let name = card.name.clone();
        self.mine.retain(|e| e.card.id != card.id);
        self.selected = Some(card.id.clone());
        self.mine.push(Entry::new(card, Kind::Contact));
        sort(&mut self.mine);
        self.mode = Mode::View;
        self.refilter(cx);
        cx.emit(ContactsAction::Toast(Toast::Success, tr!("contacts-saved", name = name)));
        self.note_demo(cx);
    }

    fn note_demo(&mut self, cx: &mut Context<Self>) {
        if !std::mem::replace(&mut self.demo_noted, true) {
            cx.emit(ContactsAction::Toast(Toast::Info, tr!("contacts-demo-local")));
        }
    }

    fn ticket(&mut self) -> u64 {
        let t = self.next_ticket;
        self.next_ticket += 1;
        t
    }

    fn delete(&mut self, cx: &mut Context<Self>) {
        let Some(e) = self.selected_entry().filter(|e| e.kind == Kind::Contact) else { return };
        let (id, name) = (e.card.id.clone(), e.name());
        if !self.live {
            self.mine.retain(|e| e.card.id != id);
            self.selected = None;
            self.mode = Mode::View;
            self.refilter(cx);
            cx.emit(ContactsAction::Toast(Toast::Success, tr!("contacts-deleted", name = name)));
            return self.note_demo(cx);
        }
        let ticket = self.ticket();
        self.pending = Some(ticket);
        cx.emit(ContactsAction::Delete { id, ticket });
        cx.notify();
    }

    pub(crate) fn on_sync(&mut self, ev: &SyncEvent, cx: &mut Context<Self>) {
        match ev {
            SyncEvent::ContactsLoaded(cards) if self.live => {
                self.mine = cards.iter().cloned().map(|c| Entry::new(c, Kind::Contact)).collect();
                sort(&mut self.mine);
                self.loading = false;
                self.loaded = true;
                self.error = None;
            }
            SyncEvent::DirectoryLoaded(people) if self.live => {
                self.directory = people.iter().cloned().map(Entry::from_principal).collect();
                sort(&mut self.directory);
            }
            // Страница справочника (T1-25г): к показанному, без повторов.
            SyncEvent::DirectoryPage { offset, people, total, search_only, .. } if self.live => {
                if !self.pages.got(*offset, people.len() as u32, *total, *search_only) {
                    return;
                }
                for p in people {
                    if !self.directory.iter().any(|e| e.card.id == p.id) {
                        self.directory.push(Entry::from_principal(p.clone()));
                    }
                }
                sort(&mut self.directory);
            }
            // Найденное поиском в справочнике (Exchange) — к уже показанному, без повторов.
            SyncEvent::PeopleFound { people, .. } if self.live && !people.is_empty() => {
                for p in people {
                    if !self.directory.iter().any(|e| e.card.id == p.id) {
                        self.directory.push(Entry::from_principal(p.clone()));
                    }
                }
                sort(&mut self.directory);
            }
            // Подробности человека (S7-08б): телефон, должность и отдел — в пустые поля его карточек справочника.
            SyncEvent::PersonDetails { email, person: Some(p) } if self.live => {
                for e in self.directory.iter_mut().filter(|e| e.card.emails.iter().any(|m| m.eq_ignore_ascii_case(email))) {
                    let mut card = e.card.clone();
                    if card.phones.is_empty() && !p.phone.trim().is_empty() {
                        card.phones = vec![p.phone.trim().to_owned()];
                    }
                    if card.title.is_empty() {
                        card.title = p.title.trim().to_owned();
                    }
                    if card.organization.is_empty() {
                        card.organization = p.department.trim().to_owned();
                    }
                    *e = Entry::new(card, e.kind);
                }
            }
            SyncEvent::Me { address, .. } => {
                self.me = Some(address.email.clone());
                cx.notify();
                return;
            }
            SyncEvent::Availability { ticket, people, .. } if self.today.ticket == Some(*ticket) => {
                self.today.ticket = None;
                self.today.busy = Some(people.first().and_then(|(_, b)| b.clone()));
                cx.notify();
                return;
            }
            SyncEvent::ContactSaved { ticket, id } if self.pending == Some(*ticket) => {
                self.pending = None;
                let name = match &self.mode {
                    Mode::Edit(f) => f.text(cx).name,
                    _ => String::new(),
                };
                self.mode = Mode::View;
                self.selected = Some(id.clone());
                cx.emit(ContactsAction::Toast(Toast::Success, tr!("contacts-saved", name = name)));
            }
            SyncEvent::ContactDeleted { ticket, id } if self.pending == Some(*ticket) => {
                self.pending = None;
                let name = self.mine.iter().find(|e| &e.card.id == id).map(Entry::name).unwrap_or_default();
                self.mode = Mode::View;
                self.selected = None;
                cx.emit(ContactsAction::Toast(Toast::Success, tr!("contacts-deleted", name = name)));
            }
            SyncEvent::ContactFailed { ticket, error } => {
                let text = tr!("contacts-failed", error = error.clone());
                if *ticket == 0 {
                    self.loading = false;
                    self.error = Some(text);
                } else if self.pending == Some(*ticket) {
                    self.pending = None;
                    match &mut self.mode {
                        Mode::Edit(f) => f.error = Some(text.clone()),
                        _ => self.mode = Mode::View,
                    }
                    cx.emit(ContactsAction::Toast(Toast::Error, text));
                } else {
                    return;
                }
            }
            _ => return,
        }
        self.refilter(cx);
        if self.want_first && self.selected.is_none() && matches!(self.mode, Mode::View) {
            self.select_first(cx);
        }
    }

    // --- слева: источники ---

    fn nav(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let l = crate::roles::look(cx);
        let item = |id: &'static str, key: &str, n: usize, source: Source, cx: &mut Context<Self>| {
            let on = self.source == source;
            div()
                .id(id)
                .h(px(32.))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(10.))
                .px(px(8.))
                .rounded(px(4.))
                .cursor_pointer()
                .when(on, |d| d.bg(l.panel()).font_weight(FontWeight::SEMIBOLD))
                .when(!on, |d| d.hover(|d| d.bg(l.hover())))
                .child(div().flex_1().child(tr!(key)))
                .child(div().text_color(l.text2()).font_weight(FontWeight::NORMAL).child(n.to_string()))
                .on_click(cx.listener(move |this, _, _, cx| this.set_source(source, cx)))
        };
        let account = self.me.clone().unwrap_or_else(|| tr!("contacts-account"));
        div()
            .w(px(NAV_W))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .gap(px(2.))
            .pt(px(4.))
            .pr(px(8.))
            .pl(px(4.))
            .text_size(px(13.))
            .child(div().text_size(px(12.)).text_color(l.text2()).px(px(8.)).pt(px(8.)).pb(px(4.)).child(tr!("contacts-favorites")))
            .child(item("src-freq", "contacts-frequent", self.frequent.len(), Source::Frequent, cx))
            .child(div().text_size(px(12.)).text_color(l.text2()).px(px(8.)).pt(px(12.)).pb(px(4.)).truncate().child(account))
            .child(item("src-dir", "contacts-directory", self.pages.total.map_or(self.directory.len(), |t| t as usize), Source::Directory, cx))
            .child(item("src-mine", "contacts-mine", self.mine.len(), Source::Mine, cx))
            .when(self.loading, |d| d.child(div().px(px(8.)).pt(px(6.)).text_size(px(12.)).text_color(l.text2()).child(tr!("contacts-loading"))))
            .child(div().flex_1())
            .child(
                div()
                    .mx(px(4.))
                    .mb(px(12.))
                    .px(px(12.))
                    .py(px(10.))
                    .rounded(px(6.))
                    .bg(l.pm(Pm::CalOff))
                    .border_1()
                    .border_color(l.border())
                    .text_size(px(12.))
                    .line_height(px(17.))
                    .text_color(l.text2())
                    .child(tr!("contacts-nav-note")),
            )
    }

    // --- список ---

    fn list_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let l = crate::roles::look(cx);
        let searching = self.searching();
        let title = tr!(match self.source {
            _ if searching => "contacts-search-title",
            Source::Mine => "contacts-mine",
            Source::Directory => "contacts-directory",
            Source::Frequent => "contacts-frequent",
        });
        let summary = match self.source {
            _ if searching => tr!("contacts-found", n = self.shown.len()),
            Source::Frequent => tr!("contacts-frequent-summary", days = postman_core::contacts::FREQUENT_DAYS),
            _ => tr!("contacts-count", n = self.entries().len()),
        };
        let note = if self.source == Source::Directory && !searching && self.pages.search_only && self.directory.is_empty() {
            // Справочник Exchange — только поиском (T1-25г): строка и поле поиска.
            Some(tr!("contacts-directory-search-only"))
        } else if self.loading && self.entries().is_empty() {
            Some(tr!("contacts-loading"))
        } else if self.shown.is_empty() {
            Some(tr!(if self.entries().is_empty() { "contacts-empty" } else { "contacts-nothing" }))
        } else {
            None
        };
        let header = div()
            .h(px(40.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.))
            .pl(px(14.))
            .pr(px(10.))
            .border_b_1()
            .border_color(l.border())
            .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
            .child(div().text_size(px(12.)).text_color(l.text2()).child(summary))
            .child(div().flex_1())
            .child(if self.source == Source::Frequent && !searching {
                self.order_picker(&l, cx).into_any_element()
            } else {
                div()
                    .text_size(px(12.))
                    .text_color(l.text2())
                    .child(tr!(if searching { "contacts-sort-match" } else { "contacts-sort-name" }))
                    .into_any_element()
            });
        let list = list(self.list.clone(), cx.processor(|this, ix: usize, _window, cx| this.item(ix, cx))).size_full();
        let letters: Vec<(char, usize)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| match it {
                Item::Letter(c, _) => Some((*c, i)),
                Item::Group(..) | Item::Row(_) => None,
            })
            .collect();
        let index = (!searching && letters.len() > 1).then(|| {
            div().w(px(24.)).flex_none().flex().flex_col().py(px(6.)).border_l_1().border_color(l.border()).children(letters.into_iter().map(
                |(c, i)| {
                    div()
                        .id(("letter", i))
                        .h(px(20.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(11.))
                        .text_color(l.text2())
                        .cursor_pointer()
                        .hover(|d| d.text_color(l.accent_text()))
                        .child(c.to_string())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.list.scroll_to(ListOffset { item_ix: i, offset_in_item: px(0.) });
                            cx.notify();
                        }))
                },
            ))
        });
        crate::mailer::card(&l)
            .w(px(LIST_W))
            .flex_none()
            .mr(px(8.))
            .child(header)
            .when_some(self.error.clone(), |d, e| d.child(div().px_3().py_1().text_color(l.error()).child(e)))
            .when_some(note, |d, n| d.child(div().px(px(24.)).py(px(32.)).text_center().text_color(l.text2()).child(n)))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(div().relative().flex_1().min_w_0().h_full().child(list).child(Scrollbar::vertical(&self.list).mode(ScrollbarMode::Hover)))
                    .children(index),
            )
    }

    /// Выбор порядка «Часто пишу» (T1-24): подпись с ▾, по щелчку — меню из четырёх порядков.
    fn order_picker(&self, l: &Look, cx: &mut Context<Self>) -> impl IntoElement {
        let menu = self.order_menu.then(|| {
            let items = Order::ALL.into_iter().map(|o| {
                crate::composer::menu_item(ElementId::NamedInteger("freq-order".into(), o as u64), l, o == self.order)
                    .child(tr!(o.key()))
                    .on_click(cx.listener(move |this, _, _, cx| this.set_order(o, cx)))
            });
            let menu =
                crate::composer::menu_box("freq-order-menu", l).top(px(26.)).left_auto().right_0().min_w(px(180.)).children(items).on_mouse_down_out(
                    cx.listener(|this, _, _, cx| {
                        this.order_menu = false;
                        cx.notify();
                    }),
                );
            deferred(menu).with_priority(1)
        });
        div()
            .relative()
            .child(
                div()
                    .id("freq-order")
                    .h(px(24.))
                    .px(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .rounded(l.px(Metric::RadiusControl))
                    .text_size(px(12.))
                    .text_color(l.text2())
                    .cursor_pointer()
                    .when(self.order_menu, |d| d.bg(l.pressed()))
                    .hover(|d| d.bg(l.hover()))
                    .child(tr!(self.order.key()))
                    .child(icon(crate::chrome::glyph::CHEVRON_DOWN, 8.))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.order_menu = !this.order_menu;
                        cx.notify();
                    })),
            )
            .children(menu)
    }

    fn set_order(&mut self, order: Order, cx: &mut Context<Self>) {
        self.order_menu = false;
        if self.order != order {
            self.order = order;
            self.reshow();
        }
        cx.notify();
    }

    /// Снимок `contacts-frequent`: выбор порядка открыт.
    pub(crate) fn open_order_menu(&mut self, cx: &mut Context<Self>) {
        self.order_menu = true;
        cx.notify();
    }

    fn item(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let l = crate::roles::look(cx);
        // Конец прокрутки справочника — следующая страница (T1-25г; после кадра, один раз).
        if self.live && self.source == Source::Directory && !self.searching() && ix + 10 >= self.items.len() {
            let this = cx.entity().downgrade();
            cx.defer(move |cx| {
                this.update(cx, |this, cx| this.next_page(cx)).ok();
            });
        }
        match self.items.get(ix).copied() {
            Some(Item::Letter(c, n)) => div()
                .w_full()
                .h(px(HEAD_H))
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(14.))
                .bg(l.pm(Pm::CalOff))
                .border_b_1()
                .border_color(l.border())
                .text_size(px(12.))
                .child(div().font_weight(FontWeight::SEMIBOLD).child(c.to_string()))
                .child(div().text_color(l.text2()).child(n.to_string()))
                .into_any_element(),
            Some(Item::Group(source, n)) => div()
                .w_full()
                .h(px(HEAD_H))
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(14.))
                .bg(l.pm(Pm::CalOff))
                .border_b_1()
                .border_color(l.border())
                .text_size(px(12.))
                .child(div().font_weight(FontWeight::SEMIBOLD).child(tr!(match source {
                    Source::Directory => "contacts-directory",
                    Source::Mine => "contacts-mine",
                    Source::Frequent => "contacts-group-correspondents",
                })))
                .child(div().text_color(l.text2()).child(n.to_string()))
                .into_any_element(),
            Some(Item::Row(i)) => match self.entries().get(i) {
                Some(e) => row(ix, e, self.selected.as_deref() == Some(e.card.id.as_str()), self.focused, self.external(e), &l, cx),
                None => div().into_any_element(),
            },
            None => div().into_any_element(),
        }
    }

    /// «Внешний»: личный контакт с адресом не моего домена.
    fn external(&self, e: &Entry) -> bool {
        let Some(mine) = self.me.as_deref().and_then(|m| m.split_once('@')).map(|(_, d)| d) else { return false };
        e.kind == Kind::Contact && e.card.emails.first().and_then(|m| m.split_once('@')).is_some_and(|(_, d)| !d.eq_ignore_ascii_case(mine))
    }

    // --- карточка ---

    fn card_pane(&self, e: &Entry, cx: &mut Context<Self>) -> AnyElement {
        let l = crate::roles::look(cx);
        let c = &e.card;
        let mine = e.kind == Kind::Contact;
        let busy = self.pending.is_some();
        let confirm = matches!(self.mode, Mode::Confirm).then(|| {
            let answer = |id: &'static str, text: String, yes: bool, cx: &mut Context<Self>| {
                div()
                    .id(id)
                    .px_3()
                    .py_0p5()
                    .rounded_md()
                    .cursor_pointer()
                    .when(yes, |d| d.bg(l.error_bg()).text_color(l.error()).font_weight(FontWeight::SEMIBOLD))
                    .when(!yes, |d| d.bg(l.fill()).hover(|d| d.bg(l.pressed())))
                    .child(text)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if yes {
                            this.delete(cx);
                        } else {
                            this.mode = Mode::View;
                            cx.notify();
                        }
                    }))
            };
            div()
                .flex()
                .items_center()
                .gap_2()
                .px(px(24.))
                .py_1p5()
                .bg(l.warning_bg())
                .border_b_1()
                .border_color(l.border())
                .child(div().flex_1().child(if busy { tr!("contacts-deleting") } else { tr!("contacts-delete-confirm", name = e.name()) }))
                .when(!busy, |d| {
                    d.child(answer("delete-yes", tr!("contacts-yes"), true, cx)).child(answer("delete-no", tr!("contacts-no"), false, cx))
                })
        });
        // Клавиша «Изменить» (F2) — подсказкой и KeyTips, не в тексте (S5-18).
        let (badge, link) = match e.kind {
            _ if mine => (tr!("contacts-badge-mine"), tr!("contacts-edit")),
            Kind::Correspondent => (tr!("contacts-group-correspondents"), tr!("contacts-add-mine")),
            _ => (tr!("contacts-badge-directory"), tr!("contacts-add-mine")),
        };
        let second = if !c.title.is_empty() { c.title.clone() } else { e.kind.label() };
        let head = div()
            .flex()
            .items_center()
            .gap(px(16.))
            .child(avatar(&e.name(), 64., &l))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .line_height(px(24.))
                    .child(div().text_size(px(22.)).font_weight(FontWeight::SEMIBOLD).line_height(px(30.)).child(e.name()))
                    .child(div().text_size(px(14.)).child(second))
                    .when(!c.organization.is_empty(), |d| d.child(div().text_color(l.text2()).child(c.organization.clone()))),
            )
            .child(
                div()
                    .self_start()
                    .flex()
                    .flex_col()
                    .items_end()
                    .gap(px(6.))
                    .child(div().h(px(24.)).px(px(10.)).flex().items_center().rounded(px(12.)).bg(l.fill()).text_size(px(12.)).child(badge))
                    .child(
                        div()
                            .id("card-link")
                            .text_size(px(12.))
                            .text_color(l.accent_text())
                            .cursor_pointer()
                            .child(link)
                            .on_click(cx.listener(|this, _, window, cx| this.edit(window, cx))),
                    ),
            );
        let has_mail = !c.emails.is_empty();
        let actions = div().flex().flex_wrap().gap(px(8.)).when(has_mail, |d| {
            d.child(
                crate::buttons::primary("contact-write", &l)
                    .child(icon(g::MAIL, 14.))
                    .child(tr!("contacts-write"))
                    .on_click(cx.listener(|this, _, _, cx| this.write(cx))),
            )
            .child(
                crate::buttons::secondary("contact-meet", &l)
                    .child(icon(g::CALENDAR, 14.))
                    .child(tr!("contacts-meet"))
                    .on_click(cx.listener(|this, _, _, cx| this.meet(cx))),
            )
            .child(
                crate::buttons::secondary("contact-copy", &l)
                    .child(icon(g::COPY, 14.))
                    .child(tr!("contacts-copy"))
                    .on_click(cx.listener(|this, _, _, cx| this.copy_address(cx))),
            )
        });
        let cell = |label: &str, values: Vec<AnyElement>| {
            div().min_w_0().flex().flex_col().gap(px(2.)).child(div().text_size(px(12.)).text_color(l.text2()).child(tr!(label))).children(values)
        };
        let plain = |s: &String| div().truncate().child(s.clone()).into_any_element();
        let dash = || div().text_color(l.text3()).child("—").into_any_element();
        let emails: Vec<AnyElement> = c
            .emails
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let to = Address::new(&c.name, m);
                div()
                    .id(("mail", i))
                    .truncate()
                    .text_color(l.accent_text())
                    .cursor_pointer()
                    .child(m.clone())
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(ContactsAction::Write(to.clone()))))
                    .into_any_element()
            })
            .collect();
        let phones: Vec<AnyElement> = c.phones.iter().map(plain).collect();
        let grid_rows = [
            ("contacts-emails", if emails.is_empty() { vec![dash()] } else { emails }),
            ("contacts-phones", if phones.is_empty() { vec![dash()] } else { phones }),
            ("contacts-organization", vec![if c.organization.is_empty() { dash() } else { plain(&c.organization) }]),
            ("contacts-title", vec![if c.title.is_empty() { dash() } else { plain(&c.title) }]),
        ];
        let mut cells: Vec<AnyElement> = grid_rows.into_iter().map(|(k, v)| cell(k, v).into_any_element()).collect();
        if !mine {
            cells.push(cell("contacts-kind", vec![div().child(e.kind.label()).into_any_element()]).into_any_element());
        }
        // Две колонки.
        let mut left = Vec::new();
        let mut right = Vec::new();
        for (i, c) in cells.into_iter().enumerate() {
            if i % 2 == 0 { left.push(c) } else { right.push(c) }
        }
        let fields = div()
            .flex()
            .gap(px(24.))
            .py(px(14.))
            .border_t_1()
            .border_b_1()
            .border_color(l.border())
            .child(div().flex_1().min_w_0().flex().flex_col().gap(px(12.)).children(left))
            .child(div().flex_1().min_w_0().flex().flex_col().gap(px(12.)).children(right));
        let today = (!mine).then(|| self.today_block(e, &l, cx));
        div()
            .flex()
            .flex_col()
            .size_full()
            .children(confirm)
            .child(
                div()
                    .id("contact-card")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(16.))
                    .px(px(24.))
                    .pt(px(20.))
                    .pb(px(16.))
                    .child(head)
                    .child(actions)
                    .child(fields)
                    .children(today)
                    .when(!mine, |d| d.child(div().text_size(px(12.)).text_color(l.text2()).child(tr!("contacts-readonly")))),
            )
            .into_any_element()
    }

    /// «Сегодня»: занятость сотрудника или переговорной на день (без названий встреч).
    fn today_block(&self, e: &Entry, l: &Look, cx: &mut Context<Self>) -> impl IntoElement {
        let day = jiff::Zoned::now().with_time_zone(self.tz.clone());
        let start = day_start(day.date(), &self.tz);
        let range = Span::new(start + DAY_FROM * 3600, start + DAY_TO * 3600);
        let frac = |t: i64| ((t - range.start) as f32 / range.len() as f32).clamp(0., 1.);
        let head = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(div().font_weight(FontWeight::SEMIBOLD).child(tr!("cal-today")))
            .child(div().text_size(px(12.)).text_color(l.text2()).child(crate::calendar::long_day(day.date())))
            .child(div().flex_1())
            .when(!e.card.emails.is_empty(), |d| {
                d.child(
                    crate::buttons::secondary("today-meet", l)
                        .h(px(26.))
                        .text_size(px(12.))
                        .child(tr!(if e.kind == Kind::Room { "contacts-book-room" } else { "contacts-meet" }))
                        .on_click(cx.listener(|this, _, _, cx| this.meet(cx))),
                )
            });
        let body: AnyElement = match &self.today.busy {
            _ if self.today.ticket.is_some() => div().text_size(px(12.)).text_color(l.text2()).child(tr!("meeting-busy-loading")).into_any_element(),
            Some(Some(busy)) => {
                let now = jiff::Timestamp::now().as_second();
                let segs = busy.iter().filter(|b| b.span.overlaps(&range)).map(|b| {
                    let (s, x) = (b.span.start.max(range.start), b.span.end.min(range.end));
                    let color = match b.kind {
                        BusyKind::Busy => l.pm(Pm::FbBusy),
                        BusyKind::Tentative => l.pm(Pm::FbTentative),
                        BusyKind::Unavailable => l.pm(Pm::FbOof),
                    };
                    div().absolute().top(px(3.)).bottom(px(3.)).left(relative(frac(s))).w(relative(frac(x) - frac(s))).rounded(px(3.)).bg(color)
                });
                let ticks = (0..(DAY_TO - DAY_FROM))
                    .map(|h| div().absolute().top_0().bottom_0().w(px(1.)).left(relative(h as f32 / (DAY_TO - DAY_FROM) as f32)).bg(l.border()));
                let now_in = (range.start..range.end).contains(&now);
                let now_x = now_in.then(|| div().absolute().top(px(-2.)).bottom(px(-2.)).w(px(2.)).left(relative(frac(now))).bg(l.text()));
                let past =
                    (now > range.start).then(|| div().absolute().top_0().bottom_0().left_0().w(relative(frac(now))).bg(l.panel().opacity(0.5)));
                let hours = (0..=(DAY_TO - DAY_FROM)).step_by(2).map(|h| {
                    div()
                        .absolute()
                        .top_0()
                        .left(relative(h as f32 / (DAY_TO - DAY_FROM) as f32))
                        .text_size(px(11.))
                        .text_color(l.text2())
                        .child(format!("{}", DAY_FROM + h))
                });
                let legend = |color: Hsla, key: &str| {
                    div().flex().items_center().gap(px(6.)).child(div().w(px(12.)).h(px(8.)).rounded(px(2.)).bg(color)).child(tr!(key))
                };
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .relative()
                            .h(px(26.))
                            .rounded(px(4.))
                            .bg(l.fill_passive())
                            .overflow_hidden()
                            .children(ticks)
                            .children(segs)
                            .children(past)
                            .children(now_x),
                    )
                    .child(div().relative().h(px(14.)).children(hours))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_x(px(14.))
                            .text_size(px(12.))
                            .text_color(l.text2())
                            .child(legend(l.pm(Pm::FbBusy), "cal-busy"))
                            .child(legend(l.pm(Pm::FbTentative), "cal-tentative"))
                            .child(legend(l.pm(Pm::FbOof), "cal-unavailable"))
                            .when(now_in, |d| d.child(div().child(tr!("contacts-now", time = hm(now, &self.tz)))))
                            .child(div().child(tr!("contacts-no-titles"))),
                    )
                    .into_any_element()
            }
            _ => div()
                .flex()
                .items_center()
                .gap(px(10.))
                .px(px(12.))
                .py(px(10.))
                .rounded(px(6.))
                .bg(l.pm(Pm::CalOff))
                .border_1()
                .border_color(l.border())
                .child(icon(g::HIDDEN, 14.).text_color(l.text2()))
                .child(div().flex_1().child(tr!("contacts-busy-hidden")))
                .into_any_element(),
        };
        div().flex().flex_col().gap(px(8.)).child(head).child(body)
    }

    fn form_pane(&self, f: &Form, cx: &mut Context<Self>) -> AnyElement {
        let l = crate::roles::look(cx);
        let busy = self.pending.is_some();
        let field = |label: &str, state: &Entity<InputState>| {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(div().text_size(px(12.)).text_color(l.text2()).child(tr!(label)))
                .child(Input::new(state))
        };
        let title = tr!(if f.old.is_some() { "contacts-form-edit" } else { "contacts-form-new" });
        let note = tr!(if self.live { "contacts-form-where" } else { "contacts-form-where-demo" });
        div()
            .id("contact-form")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(14.))
            .px(px(24.))
            .pt(px(20.))
            .pb(px(16.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(16.))
                    .child(
                        div()
                            .size(px(64.))
                            .rounded_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(l.fill())
                            .text_color(l.text2())
                            .child(icon(if f.old.is_some() { g::EDIT } else { g::ADD }, 22.)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(div().text_size(px(22.)).font_weight(FontWeight::SEMIBOLD).child(title))
                            .child(div().text_color(l.text2()).child(note)),
                    ),
            )
            .child(div().flex().child(field("contacts-name", &f.name)))
            .child(div().flex().gap(px(16.)).child(field("contacts-organization", &f.organization)).child(field("contacts-title", &f.title)))
            .child(div().flex().child(field("contacts-emails", &f.emails)))
            .child(div().flex().child(field("contacts-phones", &f.phones)))
            .child(div().text_size(px(12.)).line_height(px(17.)).text_color(l.text2()).child(tr!("contacts-form-hint")))
            .when_some(f.error.clone(), |d, e| d.child(div().text_color(l.error()).child(e)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .when(!busy, |d| d.child(button("form-save", tr!("contacts-save"), true).on_click(cx.listener(|this, _, _, cx| this.save(cx)))))
                    .when(busy, |d| d.child(div().text_color(l.text2()).child(tr!("contacts-saving"))))
                    .child(button("form-cancel", tr!("contacts-cancel"), false).on_click(cx.listener(|this, _, _, cx| {
                        this.mode = Mode::View;
                        this.pending = None;
                        cx.notify();
                    }))),
            )
            .into_any_element()
    }
}

/// Строка списка: кружок с буквами, имя, ниже — должность и организация или адрес (тип у переговорных).
fn row(ix: usize, e: &Entry, selected: bool, focused: bool, external: bool, l: &Look, cx: &mut Context<ContactsView>) -> AnyElement {
    let name = e.name();
    let email = e.card.emails.first().filter(|m| !m.eq_ignore_ascii_case(&name)).cloned();
    let second = match (e.kind, e.card.title.as_str(), e.card.organization.as_str(), email) {
        (Kind::Room | Kind::Resource, _, _, _) => e.kind.label(),
        (_, t, o, _) if !t.is_empty() && !o.is_empty() => format!("{t} · {o}"),
        (_, t, _, _) if !t.is_empty() => t.to_string(),
        (_, _, o, Some(m)) if !o.is_empty() => format!("{m} · {o}"),
        (_, _, _, Some(m)) => m,
        (_, _, o, None) => o.to_string(),
    };
    let second = if second.is_empty() { e.kind.label() } else { second };
    let id = e.card.id.clone();
    let menu_id = id.clone();
    div()
        .id(("contact", ix))
        .w_full()
        .h(px(ROW_H))
        .flex()
        .items_center()
        .gap(px(10.))
        .pl(px(14.))
        .pr(px(12.))
        .relative()
        .border_b_1()
        .border_color(l.border())
        .cursor_pointer()
        // Как строка писем (T1-D): тон акцента в фокусе, нейтральная без; полоса — акцент или цвет подписей.
        .when(selected, |d| d.bg(l.row_fill(focused)).child(div().absolute().left_0().top_0().bottom_0().w(px(3.)).bg(l.row_strip(focused))))
        .when(!selected, |d| d.hover(|d| d.bg(l.row_hover())))
        .child(avatar(&name, 36., l))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(1.))
                .child(div().truncate().child(name))
                .child(div().truncate().text_size(px(12.)).text_color(l.text2()).child(second)),
        )
        .when(matches!(e.kind, Kind::Room | Kind::Resource), |d| d.child(icon(g::PIN, 12.).text_color(l.text2())))
        .when(external, |d| d.child(chip_outline(tr!("cal-external"), l)))
        .on_click(cx.listener(move |this, _, _, cx| this.select(id.clone(), cx)))
        // Правая кнопка — меню действий карточки над этой строкой (T1-23); выбор в панели не меняется.
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                this.open_menu(menu_id.clone(), ev.position, window, cx);
            }),
        )
        .into_any_element()
}

impl Render for ContactsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let menu = self.row_menu(window, cx);
        let l = crate::roles::look(cx);
        let pane = match &self.mode {
            Mode::Edit(f) => self.form_pane(f, cx),
            _ => match self.selected_entry() {
                Some(e) => self.card_pane(e, cx),
                None => div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(8.))
                    .text_color(l.text2())
                    .child(icon(crate::chrome::glyph::CONTACTS, 28.).text_color(l.text3()))
                    .child(tr!("contacts-pick"))
                    .into_any_element(),
            },
        };
        div()
            .size_full()
            .flex()
            .min_w_0()
            .text_size(px(13.))
            .child(self.nav(cx))
            .child(self.list_card(cx))
            .child(crate::mailer::card(&l).flex_1().child(pane))
            .children(menu)
    }
}

/// Демо: занятость коллеги из демо-недели — на сегодня (тот же день недели).
fn shift_to_today(busy: Vec<Busy>, today: i64, tz: &TimeZone) -> Vec<Busy> {
    let Some(z) = crate::calendar::zoned(today, tz) else { return busy };
    let wd = z.weekday();
    busy.into_iter()
        .filter_map(|b| {
            let d = crate::calendar::zoned(b.span.start, tz)?.date();
            (d.weekday() == wd).then(|| {
                let shift = today - day_start(d, tz);
                Busy { span: Span::new(b.span.start + shift, b.span.end + shift), kind: b.kind }
            })
        })
        .collect()
}

// --- командная строка раздела ---

/// Командная строка контактов (макет: на месте строки почты): «Новый контакт», «Написать», «Встреча»,
/// «Копировать адрес», «Изменить», «Удалить», поиск.
pub(crate) struct ContactsBar {
    contacts: Entity<ContactsView>,
    _subscription: Subscription,
}

impl ContactsBar {
    pub(crate) fn new(contacts: Entity<ContactsView>, cx: &mut Context<Self>) -> Self {
        let sub = cx.observe(&contacts, |_, _, cx| cx.notify());
        Self { contacts, _subscription: sub }
    }
}

impl Render for ContactsBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let l = crate::roles::look(cx);
        let c = self.contacts.read(cx);
        let kind = c.selected_entry().map(|e| e.kind);
        let has_mail = c.selected_entry().is_some_and(|e| !e.card.emails.is_empty());
        let search_state = c.search.clone();
        let action = |id: &'static str, glyph: char, key: &str, on: bool, act: fn() -> Box<dyn Action>| {
            crate::chrome::quiet_button(id, &l)
                .child(icon(glyph, 16.))
                .child(tr!(key))
                .when(!on, |d| d.opacity(0.45))
                .on_click(move |_, window, cx| window.dispatch_action(act(), cx))
        };
        use crate::contacts::contacts as cmd;
        let new = div()
            .id("contacts-new")
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
            .child(icon(g::ADD, 14.))
            .child(tr!("contacts-new"))
            .on_click(|_, window, cx| window.dispatch_action(Box::new(cmd::New), cx));
        // Клавиши — подсказкой и KeyTips (S5-18), не в тексте кнопки и поля.
        let new = crate::keytips::command(new, "contacts.new", Some(tr!("contacts-new")), &l, cx);
        // «Нет у сервера — нет на экране» (S7-03): без личных контактов — без кнопок правки, без календаря — без «Встречи».
        let features = crate::features::get(cx);
        let personal = crate::features::available("contacts.new", features);
        let meet = crate::features::available("contacts.meeting", features);
        let search = Input::new(&search_state).prefix(icon(g::SEARCH, 14.).text_color(l.text3()));
        div()
            .size_full()
            .flex()
            .items_center()
            .gap(px(2.))
            .px(px(12.))
            .text_color(l.text())
            .when(personal, |d| d.child(new))
            .child(action("contacts-write", g::MAIL, "contacts-write", has_mail, || Box::new(cmd::Write)))
            .when(meet, |d| d.child(action("contacts-meet", g::CALENDAR, "contacts-meet", has_mail, || Box::new(cmd::Meeting))))
            .child(action("contacts-copy", g::COPY, "contacts-copy", has_mail, || Box::new(cmd::CopyAddress)))
            .when(personal, |d| {
                d.child(div().w(px(1.)).h(px(20.)).mx(px(8.)).bg(l.stroke()))
                    .child(action(
                        "contacts-edit",
                        g::EDIT,
                        if kind.is_some_and(|k| k != Kind::Contact) { "contacts-add-mine" } else { "contacts-edit" },
                        kind.is_some(),
                        || Box::new(cmd::Edit),
                    ))
                    .child(action("contacts-delete", g::DELETE, "contacts-delete", kind == Some(Kind::Contact), || Box::new(cmd::Delete)))
            })
            .child(div().flex_1())
            .child(crate::keytips::badge(div().ml(px(12.)).w(px(300.)).flex_none().child(search), crate::keytips::key("search.focus", cx), &l, cx))
    }
}

impl crate::Mailer {
    /// Просьба раздела: команда синхронизации, письмо, встреча или тост. Демо сюда правки не шлёт (они в окне).
    pub(crate) fn on_contacts(&mut self, action: &ContactsAction, cx: &mut Context<Self>) {
        let command = match action {
            ContactsAction::Load => Command::Contacts,
            ContactsAction::DirectoryPage(offset) => Command::DirectoryPage { account: None, offset: *offset, max: DIR_PAGE },
            ContactsAction::Save { card, old, ticket } => Command::SaveContact { card: card.clone(), old: old.clone(), ticket: *ticket },
            ContactsAction::Delete { id, ticket } => Command::DeleteContact { id: id.clone(), ticket: *ticket },
            ContactsAction::PersonDetails(email) => Command::PersonDetails { email: email.clone() },
            ContactsAction::Availability { ticket, email, from, to } => {
                Command::Availability { ticket: *ticket, emails: vec![email.clone()], from: *from, to: *to, sender: None }
            }
            ContactsAction::Write(address) => {
                self.open_compose(Draft { to: vec![address.clone()], ..Default::default() }, cx);
                return;
            }
            ContactsAction::Meet(address) => {
                let now = jiff::Zoned::now();
                let duration = crate::prefs::get(cx).calendar.duration_secs();
                let mut model = crate::meeting::MeetingModel::new(crate::meeting::default_start(&now), duration);
                model.attendees = format!("{}; ", address.display());
                self.open_meeting(Some(model), cx);
                return;
            }
            ContactsAction::Toast(level, text) => {
                self.toasts.update(cx, |t, cx| t.push(*level, text.clone(), cx));
                return;
            }
            ContactsAction::FindPeople(text) => {
                self.find_people(text, cx);
                return;
            }
        };
        if !self.backend.send(command) {
            self.sync_stopped(cx);
            self.contacts.update(cx, |c, cx| c.not_sent(cx));
        }
    }
}

fn demo_mine() -> Vec<Entry> {
    let mut v: Vec<Entry> = postman_demo::contacts::mine()
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let card = ContactCard {
                id: format!("demo-contact-{i}"),
                name: c.name.into(),
                emails: c.emails.iter().map(|s| s.to_string()).collect(),
                phones: c.phones.iter().map(|s| s.to_string()).collect(),
                organization: c.organization.into(),
                title: c.title.into(),
            };
            Entry::new(card, Kind::Contact)
        })
        .collect();
    sort(&mut v);
    v
}

fn demo_directory() -> Vec<Entry> {
    let mut v: Vec<Entry> = postman_demo::contacts::directory()
        .iter()
        .enumerate()
        .map(|(i, p)| {
            Entry::from_principal(Principal {
                id: format!("demo-principal-{i}"),
                name: p.name.into(),
                description: None,
                email: Some(p.email.into()),
                kind: p.kind.into(),
                title: p.title.into(),
                department: p.department.into(),
                phone: p.phone.into(),
            })
        })
        .collect();
    sort(&mut v);
    v
}

/// Демо: личные контакты и справочник — в подсказки адресов окна письма и вкладки встречи.
pub(crate) fn add_demo(book: &mut AddressBook) {
    let mine = postman_demo::contacts::mine();
    book.add_contacts(mine.iter().flat_map(|c| c.emails.iter().map(|e| (c.name, *e))));
    book.add_directory(postman_demo::contacts::directory().iter().map(|p| (p.name, p.email)));
}

/// Опись части (ADR 0005, `parts.rs`).
pub(crate) const PART: crate::part::Part =
    crate::part::Part { name: "contacts", texts: TEXTS, commands: COMMANDS, handlers, scenes: scenes::SCENES, ..crate::part::Part::NONE };

/// Обработчики команд части — на корне экрана почты (`Mailer::render`).
fn handlers(root: Div, cx: &mut Context<crate::Mailer>) -> Div {
    root.on_action(cx.listener(|this, _: &contacts::New, window, cx| {
        this.contacts.update(cx, |c, cx| c.new_contact(window, cx));
    }))
    .on_action(cx.listener(|this, _: &contacts::Edit, window, cx| {
        this.contacts.update(cx, |c, cx| c.edit(window, cx));
    }))
    .on_action(cx.listener(|this, _: &contacts::Delete, _, cx| this.contacts.update(cx, |c, cx| c.ask_delete(cx))))
    .on_action(cx.listener(|this, _: &contacts::Write, _, cx| this.contacts.update(cx, |c, cx| c.write(cx))))
    .on_action(cx.listener(|this, _: &contacts::Meeting, _, cx| this.contacts.update(cx, |c, cx| c.meet(cx))))
    .on_action(cx.listener(|this, _: &contacts::CopyAddress, _, cx| {
        this.contacts.update(cx, |c, cx| c.copy_address(cx));
    }))
    .on_action(cx.listener(|this, _: &contacts::ContextMenu, window, cx| {
        this.contacts.update(cx, |c, cx| c.context_menu_key(window, cx));
    }))
}

/// Тексты части (`i18n/<язык>/<часть>.ftl`).
const TEXTS: &[pets_i18n::Resource] = &[("ru", include_str!("../i18n/ru/contacts.ftl")), ("en", include_str!("../i18n/en/contacts.ftl"))];

/// Контакты (S5-11): новый, изменить, удалить, написать, встреча, копировать адрес.
// Имя — область действий (`contacts::…`), как в id команд: так обработчики читаются и их находит `docs/PARITY.md`.
#[allow(clippy::module_inception)]
pub(crate) mod contacts {
    gpui_kit::actions!(contacts, [New, Edit, Delete, Write, Meeting, CopyAddress, ContextMenu]);
}

/// Команды части (`commands.rs`: контексты и `cmd`).
const COMMANDS: &[pets_commands::Command] = {
    use crate::commands::{CONTACTS, CONTACTS_ANY, cmd};
    &[
        // Контакты (S5-11): Outlook — Ctrl+Shift+C новый контакт; F2 — правка, Delete — удалить (после «Да»).
        cmd("contacts.new", &["ctrl-shift-c"], CONTACTS_ANY),
        cmd("contacts.edit", &["f2"], CONTACTS),
        cmd("contacts.delete", &["delete"], CONTACTS),
        cmd("contacts.write", &[], CONTACTS_ANY),
        cmd("contacts.meeting", &[], CONTACTS_ANY),
        cmd("contacts.copy_address", &[], CONTACTS_ANY),
        // Меню строки (T1-23): как у письма — Shift+F10 и клавиша «Меню».
        cmd("contacts.context_menu", &["shift-f10", "menu"], CONTACTS),
    ]
};

/// Сцены `--screens` контактов (ADR 0005).
pub(crate) mod scenes {
    use gpui_kit::*;

    use crate::screens::{self, Scene};
    use crate::{Mailer, Section};

    pub(crate) const SCENES: &[Scene] = &[
        // Раздел «Контакты»: список и карточка первого контакта (S4-02).
        Scene::new("contacts", contacts, contacts_ready).live(),
        // Контекстное меню строки (T1-23): правая кнопка по коллеге справочника, в панели — другая запись.
        Scene::new("contacts-menu", menu, menu_ready).continues(),
        // Форма правки первого контакта (демо).
        Scene::new("contact-edit", edit, edit_ready).continues(),
        // «Часто пишу» (S5-25): адресаты переписки по частоте, первая строка выбрана, выбор порядка открыт (T1-24).
        Scene::new("contacts-frequent", frequent, frequent_ready).continues().wait(frequent_wait),
    ];

    fn contacts(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.cal.update(cx, |c, cx| c.close_details(cx));
        m.close_all_meetings(window, cx);
        m.set_section(Section::Contacts, cx);
        let live = screens::live(m);
        m.contacts.update(cx, |c, cx| {
            c.select_first(cx);
            // Демо: коллега, чья занятость есть в демо-неделе, — видна полоса «Сегодня».
            if !live {
                c.select_email("m.ivanova@corp.example", cx);
            }
        });
        Ok(())
    }

    fn contacts_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        // Живой ящик: контакты с сервера пришли — выделить первый (до ответа выделять нечего).
        m.section == Section::Contacts && m.contacts.read(cx).ready()
    }

    fn menu(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        // Демо — Петров Алексей (справочник); живой ящик — первая другая запись с адресом.
        m.contacts.update(cx, |c, cx| {
            let demo = c.entries().iter().find(|e| e.card.emails.iter().any(|m| m == "a.petrov@corp.example"));
            let other = || c.entries().iter().filter(|e| Some(&e.card.id) != c.selected.as_ref()).find(|e| !e.card.emails.is_empty());
            if let Some(email) = demo.or_else(other).and_then(|e| e.card.emails.first().cloned()) {
                c.screen_menu(&email, window, cx);
            }
        });
        Ok(())
    }

    fn menu_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        m.section == Section::Contacts && m.contacts.read(cx).menu_open()
    }

    fn edit(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.contacts.update(cx, |c, cx| c.close_menu(window, cx));
        m.contacts.update(cx, |c, cx| c.edit_selected(window, cx));
        Ok(())
    }

    fn edit_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        m.section == Section::Contacts && m.contacts.read(cx).editing()
    }

    fn frequent(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        // Из правки — назад к карточке; «Часто пишу» — из адресной книги переписки (собирается в фоне).
        m.contacts.update(cx, |c, cx| c.escape(window, cx));
        m.load_address_book(cx);
        m.contacts_frequent(cx);
        // Выбор порядка (T1-24) — открытым.
        m.contacts.update(cx, |c, cx| {
            c.show_frequent(cx);
            c.open_order_menu(cx);
        });
        Ok(())
    }

    fn frequent_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        m.contacts.read(cx).frequent_shown() && m.contacts.read(cx).ready()
    }

    /// Книга собирается в фоне: пока «Часто пишу» пуст — открыть снова, когда придёт.
    fn frequent_wait(_: &Scene, m: &mut Mailer, cx: &mut Context<Mailer>) {
        if !m.contacts.read(cx).frequent_shown() {
            m.contacts.update(cx, |c, cx| c.show_frequent(cx));
        }
    }
}

#[cfg(test)]
mod tests {
    // Не `super::*`: из `gpui_kit::*` пришёл бы атрибут `test` gpui вместо встроенного.
    use super::{
        ContactCard, Entry, FormError, FormText, Frequent, Item, Kind, MenuItem, Order, demo_directory, demo_mine, filter, frequent, items,
        menu_items,
    };

    fn freq(name: &str, email: &str, last: i64) -> Frequent {
        Frequent { name: name.into(), email: email.into(), last }
    }

    fn names(entries: &[Entry], q: &str) -> Vec<String> {
        filter(entries, q).into_iter().map(|i| entries[i].card.name.clone()).collect()
    }

    #[test]
    fn search_by_name_address_phone_and_company() {
        let mine = demo_mine();
        assert_eq!(filter(&mine, "").len(), mine.len(), "пустой запрос — все");
        assert_eq!(names(&mine, "бел"), ["Белова Ольга"]);
        assert_eq!(names(&mine, "ольга бел"), ["Белова Ольга"], "слова в любом порядке");
        assert_eq!(names(&mine, "gusev"), ["Гусев Дмитрий"], "часть адреса");
        assert_eq!(names(&mine, "integrator.example"), ["Гусев Дмитрий"], "домен");
        assert_eq!(names(&mine, "495 123"), ["Белова Ольга"], "цифры телефона");
        assert_eq!(names(&mine, "+7 905"), ["Тарасов Олег"]);
        assert_eq!(names(&mine, "интегратор"), ["Гусев Дмитрий"], "организация");
        assert_eq!(names(&mine, "архитектор"), ["Гусев Дмитрий"], "должность");
        assert!(names(&mine, "сидоров").is_empty());
        let dir = demo_directory();
        assert_eq!(names(&dir, "федорова"), ["Фёдорова Наталья"], "ё = е");
        assert_eq!(names(&dir, "ПЕРЕГОВОРНАЯ 3"), ["Переговорная 3"]);
        let room = dir.iter().find(|e| e.card.name == "Переговорная 3").map(|e| e.kind);
        assert_eq!(room, Some(Kind::Room));
    }

    /// «Часто пишу» (S5-25): порядок книги; сотрудник и личный контакт — их карточки (по адресу, без дублей), прочие —
    /// адресаты переписки; поиск находит и в другой раскладке.
    #[test]
    fn frequent_reuses_cards_without_duplicates() {
        let dir = demo_directory();
        let mine = demo_mine();
        let colleague = dir.iter().find(|e| e.kind == Kind::Person && !e.card.emails.is_empty()).unwrap();
        let friend = mine.iter().find(|e| !e.card.emails.is_empty()).unwrap();
        let ce = colleague.card.emails[0].to_uppercase();
        let fe = friend.card.emails[0].clone();
        let list = vec![freq("Партнёр", "partner@outside.example", 0), freq("", &ce, 0), freq("", &fe, 0), freq("", &ce.to_lowercase(), 0)];
        let f = frequent(&list, &dir, &mine, Order::Frequency, true);
        assert_eq!(f.len(), 3, "один адрес — одна строка");
        assert_eq!(f[0].kind, Kind::Correspondent);
        assert_eq!(f[0].card.name, "Партнёр");
        assert_eq!(f[1].card.id, colleague.card.id, "карточка справочника");
        assert_eq!(f[2].card.id, friend.card.id, "карточка моих контактов");
        // Поиск — и в другой раскладке: «gfhn» — «парт».
        assert!(filter(&f, "gfhn").contains(&0));
        assert!(filter(&f, "партн").contains(&0));
        assert!(!filter(&f, "gfhn").contains(&1), "сотрудник не подходит");
    }

    /// Меню строки (T1-23): у справочника — письмо, встреча, адрес и «Добавить в мои контакты» (только чтение); у
    /// своего контакта — ещё «Изменить» и «Удалить»; без календаря — без встречи, без личных контактов — без правки.
    /// Письмо, встреча и адрес выбранную запись панели не меняют.
    #[test]
    fn row_menu_items() {
        let dir = demo_directory();
        let colleague = dir.iter().find(|e| e.kind == Kind::Person && !e.card.emails.is_empty()).unwrap();
        let mine = demo_mine();
        let friend = mine.iter().find(|e| !e.card.emails.is_empty()).unwrap();
        use MenuItem::*;
        assert_eq!(menu_items(colleague, true, true), [vec![Write, Meet, Copy], vec![AddMine]]);
        assert_eq!(menu_items(friend, true, true), [vec![Write, Meet, Copy], vec![Edit, Delete]]);
        assert_eq!(menu_items(friend, false, false), [vec![Write, Copy]]);
        let no_mail = Entry::new(ContactCard { id: "x".into(), name: "x".into(), ..Default::default() }, Kind::Contact);
        assert_eq!(menu_items(&no_mail, true, true), [vec![Edit, Delete]]);
        assert!([Write, Meet, Copy].iter().all(|it| !it.selects()), "панель остаётся на своей записи");
        assert!([Edit, Delete, AddMine].iter().all(|it| it.selects()), "форма и подтверждение — в карточке строки");
    }

    /// Справочник страницами (T1-25г): следующая страница — один запрос, пока нет ответа; неполная — последняя;
    /// `search_only` — страниц нет.
    #[test]
    fn directory_pages() {
        use super::{DIR_PAGE, DirPages};
        let mut p = DirPages::default();
        assert_eq!(p.want(), Some(0));
        assert_eq!(p.want(), None, "та же страница не уходит дважды");
        assert!(!p.got(100, 5, None, false), "чужой ответ");
        assert!(p.got(0, DIR_PAGE, Some(150), false));
        assert_eq!(p.want(), Some(DIR_PAGE));
        assert!(p.got(DIR_PAGE, 50, Some(150), false));
        assert_eq!(p.want(), None, "всё прочитано");
        let mut ews = DirPages::default();
        assert_eq!(ews.want(), Some(0));
        assert!(ews.got(0, 0, None, true));
        assert!(ews.search_only && ews.want().is_none());
    }

    /// Четыре порядка «Часто пишу» (T1-24) на одном наборе: частота — как дала адресная книга; имя — без регистра,
    /// ё = е, буквы языка интерфейса раньше чужих; адрес; давность — сначала последняя переписка.
    #[test]
    fn frequent_orders() {
        let list = vec![
            freq("ёлкин Пётр", "petr@b.example", 300),
            freq("Zoe", "zoe@a.example", 100),
            freq("анна", "anna@c.example", 200),
            freq("Егор", "egor@d.example", 400),
        ];
        let order = |o: Order, ru: bool| -> Vec<String> { frequent(&list, &[], &[], o, ru).into_iter().map(|e| e.card.emails[0].clone()).collect() };
        assert_eq!(order(Order::Frequency, true), ["petr@b.example", "zoe@a.example", "anna@c.example", "egor@d.example"]);
        // «Егор» < «ёлкин» (ё = е, «г» < «л»), кириллица — до латиницы.
        assert_eq!(order(Order::Name, true), ["anna@c.example", "egor@d.example", "petr@b.example", "zoe@a.example"]);
        assert_eq!(
            order(Order::Name, false),
            ["zoe@a.example", "anna@c.example", "egor@d.example", "petr@b.example"],
            "английский — латиница раньше"
        );
        assert_eq!(order(Order::Address, true), ["anna@c.example", "egor@d.example", "petr@b.example", "zoe@a.example"]);
        assert_eq!(order(Order::Recent, true), ["egor@d.example", "petr@b.example", "anna@c.example", "zoe@a.example"]);
    }

    /// Список с буквами: заголовок буквы с числом, «Ё» — с «Е»; при поиске букв нет.
    #[test]
    fn letters_group_the_list() {
        let dir = demo_directory();
        let shown = filter(&dir, "");
        let list = items(&dir, &shown, true);
        let letters: Vec<(char, usize)> = list
            .iter()
            .filter_map(|i| match i {
                Item::Letter(c, n) => Some((*c, *n)),
                Item::Group(..) | Item::Row(_) => None,
            })
            .collect();
        assert_eq!(list.len(), shown.len() + letters.len());
        assert_eq!(letters.iter().map(|(_, n)| n).sum::<usize>(), shown.len());
        assert!(letters.iter().any(|(c, _)| *c == 'П'), "{letters:?}");
        let fe = letters.iter().find(|(c, _)| *c == 'Ф').map(|(_, n)| *n);
        assert_eq!(fe, Some(1), "Фёдорова — под «Ф»");
        assert!(matches!(list.first(), Some(Item::Letter(_, _))));
        let found = filter(&dir, "петров");
        assert_eq!(items(&dir, &found, false), found.iter().map(|&i| Item::Row(i)).collect::<Vec<_>>());
    }

    #[test]
    fn form_becomes_a_card_and_a_patch() {
        let old = ContactCard {
            id: "c1".into(),
            name: "Петров Алексей".into(),
            emails: vec!["a.petrov@x.ru".into()],
            phones: vec!["+7 495 000-00-01".into()],
            organization: "ООО Ромашка".into(),
            title: String::new(),
        };
        let mut text = FormText::of(&old);
        assert_eq!(text.card("c1"), Ok(old.clone()), "форма без правок — та же карточка");

        text.emails = "a.petrov@x.ru, Алексей <a@home.ru>;".into();
        text.phones = " ".into();
        text.title = " Директор ".into();
        let card = text.card("c1").unwrap();
        assert_eq!(card.emails, ["a.petrov@x.ru", "a@home.ru"]);
        assert!(card.phones.is_empty(), "пустой телефон убран");
        assert_eq!(card.title, "Директор");
        assert_eq!((card.name.as_str(), card.organization.as_str()), (old.name.as_str(), old.organization.as_str()));

        text.emails = "a.petrov@x.ru; petr@".into();
        assert_eq!(text.card("c1"), Err(FormError::BadEmail("petr@".into())));
        let empty = FormText { organization: "ООО".into(), ..Default::default() };
        assert_eq!(empty.card(""), Err(FormError::NeedName));
        let only_email = FormText { emails: "x@y.ru".into(), ..Default::default() };
        assert_eq!(only_email.card("").map(|c| c.emails), Ok(vec!["x@y.ru".to_string()]));
    }
}
