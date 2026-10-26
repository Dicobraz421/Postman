//! Список писем: окно строк в фоне, выделение, флаги (S1-03, S1-06); полоса прокрутки и позиция в каждой
//! папке, строка «Удалить навсегда?» (S3-02); полоса категории и метки, категории клавишами 1–9 (S5-02);
//! фильтры, «Все папки», сортировка, группы по датам и папкам, быстрые фильтры (S5-03, `filters.rs`).
//!
//! Строки — `v_virtual_list` gpui-kit: высоты элементов известны заранее (письмо — по плотности, заголовок группы
//! — 30 px), поэтому группы с заголовками ниже строк, а не строкой той же высоты, как было бы в `uniform_list`.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::component::{VirtualListScrollHandle, v_virtual_list};
use gpui_kit::*;
use gpui_kit::prelude::*;
use pets_i18n::tr;
use postman_cache::{Cache, EmailRow, MailboxRow, Rights, Scan};
use postman_core::filters::{Arranged, Counts, Days, Facts, Group, Hit, Me, Sort, arrange};
use postman_core::labels::{self, Book};

use pets_ui::theme::{Density, Metric};

use crate::filters::{Changed, FilterModel, HEADER_H, Layout, QUICK_H, Query, Slot, Where};
use crate::roles::{Look, Pm, look};
use crate::*;

/// Высота строки письма при обычной плотности: аватар, отправитель, тема и превью (макет главного окна).
pub(crate) const ROW_HEIGHT: f32 = 66.0;

/// Высота строки письма по плотности темы: компактная — две строки без аватара и превью (DESIGN «Главное окно»).
pub(crate) fn row_height(d: Density) -> f32 {
    match d {
        Density::Compact => 46.0,
        Density::Normal => ROW_HEIGHT,
        Density::Comfortable => 74.0,
        Density::Spacious => 82.0,
    }
}
pub(crate) const WINDOW_MARGIN: usize = 100;

/// Запись флагов и меток в хранилище по очереди в отдельном потоке: поток интерфейса не ждёт диск
/// (каждый коммит redb — fsync, на листании с открытием писем это до 30 мс на кадр).
pub(crate) struct FlagWriter {
    pub(crate) tx: std::sync::mpsc::Sender<Write>,
    pub(crate) cache: Cache,
}

/// Что записать в хранилище.
pub(crate) enum Write {
    Flags { id: String, seen: bool, flagged: bool },
    /// Ключевые слова (категории): поставить `add`, снять `remove`.
    Keywords { id: String, add: Vec<String>, remove: Vec<String> },
}

impl Write {
    fn apply(self, cache: &Cache) {
        let _ = match self {
            Write::Flags { id, seen, flagged } => cache.set_flags(&id, seen, flagged),
            Write::Keywords { id, add, remove } => cache.set_keywords(&id, &add, &remove).map(|_| ()),
        };
    }
}

impl FlagWriter {
    pub(crate) fn new(cache: Cache) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<Write>();
        let writer = cache.clone();
        // Не запустился поток — получатель закрыт, `send` пишет сам (медленнее, но без потерь).
        let _ = std::thread::Builder::new().name("cache-flags".into()).spawn(move || {
            for w in rx {
                w.apply(&writer);
            }
        });
        Self { tx, cache }
    }

    pub(crate) fn set(&self, id: &str, seen: bool, flagged: bool) {
        self.send(Write::Flags { id: id.to_owned(), seen, flagged });
    }

    pub(crate) fn send(&self, w: Write) {
        if let Err(std::sync::mpsc::SendError(w)) = self.tx.send(w) {
            w.apply(&self.cache);
        }
    }
}

// --- список писем ---

/// Список писем — отдельное view: при листании gpui раскладывает и рисует только его,
/// остальные части экрана берутся из прошлого кадра (S1-06).
pub(crate) struct MailList {
    pub(crate) cache: Cache,
    pub(crate) flags: FlagWriter,
    pub(crate) mailbox: Option<String>,
    /// id результатов поиска вместо папки.
    pub(crate) search_ids: Option<Vec<String>>,
    pub(crate) count: usize,
    pub(crate) rows: (usize, Vec<EmailRow>),
    /// Окно строк, которое читается в фоне, и номер запроса: ответы на устаревшие запросы отбрасываются.
    pub(crate) page_pending: Option<Range<usize>>,
    pub(crate) page_gen: u64,
    /// Письмо выбрано, а его строка ещё не прочитана — открыть, когда страница придёт.
    pub(crate) pending_open: Option<usize>,
    /// Видимые письма (номера по порядку списка, без заголовков групп).
    pub(crate) visible: Range<usize>,
    pub(crate) selected: Option<usize>,
    /// Письмо, которое выделить, когда придут его строки (вкладка почты вернула своё состояние, S5-16).
    pub(crate) restore: Option<String>,
    pub(crate) scroll: VirtualListScrollHandle,
    /// Прокрутка каждой папки: вернулись в папку — список там же, где был.
    pub(crate) positions: Positions,
    /// Строка «Удалить навсегда? Да / Нет» над списком: письмо из «Удалённых» и его номер.
    pub(crate) confirm: Option<(usize, EmailRow)>,
    /// То же для выделения (S5-08): письма из «Удалённых» — насовсем.
    pub(crate) confirm_many: Option<Vec<String>>,
    /// Догрузка с сервера при прокрутке (живой ящик, S4-03).
    pub(crate) more: More,
    /// Папки с урезанными правами (общие ящики, S4-06): id → права. Своих папок здесь нет.
    pub(crate) rights: HashMap<String, Rights>,
    /// Таблицы категорий по аккаунтам (S5-02): имена и цвета меток строк, клавиши 1–9.
    pub(crate) labels: Book,
    /// Письма, которые сервер держит до срока (S5-06): ключ → Unix-время отправки; метка «⏱ ср 09:00».
    pub(crate) scheduled: HashMap<String, i64>,
    /// Имена папок для заголовка карточки списка.
    pub(crate) names: HashMap<String, String>,
    /// Высота строки по плотности темы (последний кадр).
    pub(crate) row_h: f32,
    /// Фильтры (S5-03): модель, общая с панелью папок; `query` — её условия, по которым посчитан список.
    pub(crate) filters: Entity<FilterModel>,
    pub(crate) query: Query,
    /// Свой набор писем по фильтрам и порядку (`Query::own_list`), посчитанный в фоне; `None` — страницы папки.
    pub(crate) view: Option<Arc<Vec<String>>>,
    /// Группы подряд (даты или папки) с числом писем; свёрнутые группы; раскладка с заголовками.
    pub(crate) groups: Vec<(Group, usize)>,
    pub(crate) collapsed: HashSet<Group>,
    pub(crate) layout: Layout,
    /// Высоты элементов списка и для чего они посчитаны (высота строки, место под закладками).
    pub(crate) sizes: Rc<Vec<Size<Pixels>>>,
    sizes_for: Option<(f32, bool)>,
    /// Пересчёт фильтров: номер запроса, устаревшие ответы отбрасываются; `computing` — ответа ещё нет.
    compute_gen: u64,
    pub(crate) computing: bool,
    /// Кто я — упоминания, «Мне», внешние: демо — владелец демо-ящика; живой ящик — `Event::Me`, `Event::Senders`.
    pub(crate) me: Me,
    identity: Vec<(String, String)>,
    /// Слова писем моих невыполненных поручений (`TasksAction::Waiting`).
    waiting: Vec<String>,
    /// Письма, которые нашёл сервер по фильтрам «Все папки» (живой ящик): в списке без проверки по кэшу.
    pub(crate) server_hits: Arc<HashSet<String>>,
    /// Папки по порядку дерева — группы «Все папки».
    pub(crate) order: Vec<String>,
    /// Меню сортировки открыто.
    pub(crate) sort_open: bool,
    /// Выделенные письма (S5-08, `select.rs`); пусто — выделения нет, флажки только по наведению.
    pub(crate) selection: crate::select::Selection,
    /// Сводка выделения для области чтения и меню (считается в фоне, `bulk.rs`); номер запроса.
    pub(crate) summary: Option<Arc<crate::bulk::Selected>>,
    pub(crate) summary_gen: u64,
    /// Строка с открытым контекстным меню — с рамкой (S5-08).
    pub(crate) menu_row: Option<usize>,
    /// Открытый выпадающий список панели массовых действий: «Категория ▾» или «В папку ▾».
    pub(crate) bulk_menu: Option<crate::bulk::Drop>,
    /// Полоса «Нет подключения…» над списком (S5-08, `states.rs`): сервер и правок в очереди; скрыта крестиком.
    pub(crate) offline: Option<(String, usize)>,
    pub(crate) offline_hidden: bool,
    /// Где список в окне (последний кадр): контекстное меню с клавиатуры — у выбранной строки.
    pub(crate) bounds: Rc<std::cell::Cell<Bounds<Pixels>>>,
    /// Вид «В списке, как Outlook» (S5-05б, `outline.rs`): строки по беседам; `outlook` — вид включён в этой папке,
    /// `threads_open` — развёрнутые беседы, `thread_groups` — группы списка по номеру `Head::group`.
    pub(crate) outline: Option<postman_core::thread::Outline>,
    pub(crate) outlook: bool,
    pub(crate) threads_open: HashSet<String>,
    pub(crate) thread_groups: Vec<Group>,
    /// Поиск (S5-10, `search.rs`): строка над списком, чипы, подсветка; группы найденного — по папкам или датам.
    pub(crate) found: Option<crate::search::Found>,
    search_groups: Vec<(Group, usize)>,
    _subscriptions: Vec<Subscription>,
}

/// Пересчёт фильтров в фоне: что перебрать и как.
struct Job {
    scan: Scan,
    query: Query,
    me: Me,
    server_hits: Arc<HashSet<String>>,
    order: Vec<String>,
    /// Нужен свой набор писем (иначе — только счётчики).
    own: bool,
    /// «Просроченный срок — наверх» (S5-04): такие письма и сработавшие напоминания — группой «Срочное».
    overdue_first: bool,
    /// Вид «как Outlook» (S5-05б): свой набор писем, сгруппированный по беседам; `open` — развёрнутые беседы.
    outlook: bool,
    open: HashSet<String>,
}

/// Итог пересчёта: счётчики и, если нужен, список с группами.
struct Computed {
    counts: Counts,
    list: Option<Arranged>,
    outline: Option<postman_core::thread::Outline>,
}

impl Job {
    fn run(self, cache: &Cache) -> Computed {
        let now = jiff::Zoned::now();
        let days = Days::at(&now);
        let folders = matches!(self.scan, Scan::Account(_));
        // Папка по дате без фильтров — страницы из кэша; есть «Срочное» (S5-04) — свой набор с ним наверху.
        let pins = !folders && self.query.sort == Sort::Date;
        let mut own = self.own || self.outlook;
        loop {
            let (counts, hits, tops, threads) = self.pass(cache, own, pins, folders, &now);
            if !own && tops > 0 {
                own = true;
                continue;
            }
            let list = own.then(|| arrange(hits, self.query.sort, folders.then_some(self.order.as_slice()), &days));
            let outline = list
                .as_ref()
                .filter(|_| self.outlook)
                .map(|(ids, groups)| crate::outline::outline_of(ids, groups, &threads, folders, self.open.clone()));
            return Computed { counts, list, outline };
        }
    }

    /// Один обход: счётчики, письма своего набора (`own`), сколько из них — наверху, и цепочки писем набора (вид
    /// «как Outlook»).
    fn pass(&self, cache: &Cache, own: bool, pins: bool, folders: bool, now: &jiff::Zoned) -> Pass {
        let index: HashMap<&str, usize> = self.order.iter().enumerate().map(|(i, id)| (id.as_str(), i)).collect();
        let mut counts = Counts::default();
        let mut hits = Vec::new();
        let mut tops = 0;
        let mut threads = HashMap::new();
        let scanned = cache.scan(&self.scan, |row, to| {
            let f = Facts::of(&self.me, &row, &to);
            let mut pass = counts.add(&self.query.criteria, &f, &row.keywords);
            if !pass && self.server_hits.contains(&row.id) {
                pass = true;
                counts.shown += 1;
            }
            let top = pass && pins && postman_core::lifecycle::on_top(&row, now, self.overdue_first);
            tops += usize::from(top);
            if own && pass {
                let folder = if folders { row.mailbox_ids.iter().filter_map(|m| index.get(m.as_str())).min().copied() } else { Some(0) };
                if let Some(folder) = folder {
                    hits.push(Hit { top, ..Hit::of(&row, folder) });
                    if self.outlook {
                        threads.insert(row.id.clone(), row.thread_id.clone());
                    }
                }
            }
            true
        });
        if let Err(e) = scanned {
            tracing::warn!("filters: {e}");
        }
        (counts, hits, tops, threads)
    }
}

/// Итог обхода: счётчики, письма набора, сколько наверху, цепочки (id → ключ цепочки).
type Pass = (Counts, Vec<Hit>, usize, HashMap<String, String>);

/// Откуда группы списка при перечитывании.
enum Groups {
    None,
    /// Папка по дате: группы по датам из числа писем не раньше начала каждой (`count_since`).
    Days(String),
    /// Посчитаны вместе со своим набором писем.
    Fixed(Vec<(Group, usize)>),
}

impl Groups {
    fn read(&self, cache: &Cache, count: usize) -> Vec<(Group, usize)> {
        match self {
            Groups::None => Vec::new(),
            Groups::Days(mb) => Days::now().groups(count, |t| cache.count_since(mb, t).unwrap_or(0)),
            Groups::Fixed(g) => g.clone(),
        }
    }
}

/// Догрузка папки: в кэше первые страницы, остальное — с сервера, когда прокрутка подходит к концу.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct More {
    /// Писем в папке на сервере (`Mailbox.totalEmails`); `None` — демо или поиск: догружать нечего.
    pub(crate) total: Option<usize>,
    /// Страница запрошена, ответа (`LoadedMore`) ещё нет.
    pub(crate) asked: bool,
    /// Сервер вернул пустую страницу (или ошибку): не просить, пока не изменится число писем в папке.
    pub(crate) exhausted: bool,
}

impl More {
    /// Просить следующую страницу: до конца загруженного меньше запаса окна, а на сервере писем больше.
    pub(crate) fn wanted(&self, visible_end: usize, count: usize) -> bool {
        !self.asked
            && !self.exhausted
            && self.total.is_some_and(|t| count < t)
            && visible_end + WINDOW_MARGIN >= count
    }

    /// Число писем на сервере (папки перечитаны); изменилось — снова можно просить.
    pub(crate) fn set_total(&mut self, total: Option<usize>) {
        if self.total != total {
            self.exhausted = false;
        }
        self.total = total;
    }

    /// Ответ на догрузку: `added` писем пришло.
    pub(crate) fn loaded(&mut self, added: usize) {
        self.asked = false;
        self.exhausted = added == 0;
    }
}

/// Список подошёл к концу загруженного: догрузить папку с сервера (`Command::LoadMore`).
pub(crate) struct NeedMore(pub(crate) String);

impl EventEmitter<NeedMore> for MailList {}

/// Позиция прокрутки (смещение по вертикали, px) каждой папки; результаты поиска — всегда сверху.
#[derive(Default)]
pub(crate) struct Positions(HashMap<String, f32>);

impl Positions {
    /// Уходим из вида `from` (папка; `None` — поиск) с прокруткой `y`; куда прокрутить вид `to`.
    pub(crate) fn switch(&mut self, from: Option<&str>, y: f32, to: Option<&str>) -> f32 {
        if let Some(mb) = from {
            self.0.insert(mb.to_owned(), y);
        }
        to.and_then(|mb| self.0.get(mb)).copied().unwrap_or(0.)
    }
}

/// Видимые строки при прокрутке `y` (смещение содержимого вверх — отрицательное) и прежнем их числе.
pub(crate) fn rows_at(y: f32, len: usize, row_h: f32) -> Range<usize> {
    let start = (-y / row_h).max(0.) as usize;
    start..start + len
}

/// «Да» в строке «Удалить навсегда?»: письмо и его номер в списке.
pub(crate) struct Purge {
    pub(crate) index: usize,
    pub(crate) row: EmailRow,
}

impl EventEmitter<Purge> for MailList {}

/// «Да» в строке «Удалить навсегда N писем?» (выделение, S5-08).
pub(crate) struct PurgeMany(pub(crate) Vec<String>);

impl EventEmitter<PurgeMany> for MailList {}

/// Письмо открыто (строка уже помечена прочитанной).
pub(crate) struct OpenRow(pub(crate) EmailRow);

impl EventEmitter<OpenRow> for MailList {}

/// Флаг письма изменён пользователем (в кэше уже записан): живой ящик отдаёт его серверу (`Keyword`).
pub(crate) struct Flagged {
    pub(crate) id: String,
    pub(crate) keyword: &'static str,
    pub(crate) on: bool,
}

impl EventEmitter<Flagged> for MailList {}

/// Флаг не изменён: письмо в папке общего ящика, открытой только для чтения (S4-06).
pub(crate) struct ReadOnly;

impl EventEmitter<ReadOnly> for MailList {}

/// Категории письма изменены пользователем (в кэше уже записаны): живой ящик отдаёт их серверу (`SetKeywords`).
pub(crate) struct Labeled {
    pub(crate) ids: Vec<String>,
    pub(crate) add: Vec<String>,
    pub(crate) remove: Vec<String>,
}

impl EventEmitter<Labeled> for MailList {}

/// Категория не поставлена — сказать тостом.
pub(crate) enum LabelNotice {
    /// На этой клавише категории нет.
    NoCategory(u8),
    /// Папка общего ящика без права менять ключевые слова.
    ReadOnly,
}

impl EventEmitter<LabelNotice> for MailList {}

/// Можно ли действие с письмом: хоть в одной его папке есть право (`check`). Папки без записи в `rights` — свои.
pub(crate) fn may(rights: &HashMap<String, Rights>, row: &EmailRow, check: impl Fn(&Rights) -> bool) -> bool {
    row.mailbox_ids.is_empty() || row.mailbox_ids.iter().any(|m| rights.get(m).is_none_or(&check))
}

/// Ключевые слова JMAP, которые меняются при переходе флагов строки `old` в (`seen`, `flagged`).
pub(crate) fn keyword_changes(old: &EmailRow, seen: bool, flagged: bool) -> Vec<(&'static str, bool)> {
    let mut out = Vec::new();
    if old.seen != seen {
        out.push(("$seen", seen));
    }
    if old.flagged != flagged {
        out.push(("$flagged", flagged));
    }
    out
}

/// Выделение после перечитывания: то же письмо по id в новом окне строк; не нашлось — без выделения.
pub(crate) fn reselect(id: Option<&str>, offset: usize, rows: &[EmailRow], count: usize) -> Option<usize> {
    let pos = rows.iter().position(|r| Some(r.id.as_str()) == id)?;
    Some(offset + pos).filter(|&i| i < count)
}

impl MailList {
    pub(crate) fn new(cache: Cache, filters: Entity<FilterModel>, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.subscribe(&filters, |this, _, _: &Changed, cx| this.on_filters(cx)),
            // Панель и закладки — из модели: её перерисовка — и списка (закладки, чип, «N из M»).
            cx.observe(&filters, |_, _, cx| cx.notify()),
            // Закладки скрыты или показаны (настройки) — место под ними в конце списка; вид беседы «как Outlook»
            // (S5-05б) включён или выключен в открытой папке — пересчитать.
            crate::prefs::observe_with(cx, |this: &mut Self, cx| {
                if this.outlook_mode(cx) != this.outlook {
                    this.recompute(cx);
                }
                cx.notify();
            }),
        ];
        let identity = vec![(postman_demo::ME.to_owned(), postman_demo::ME_NAME.to_owned())];
        let mut list = Self {
            flags: FlagWriter::new(cache.clone()),
            cache,
            mailbox: None,
            search_ids: None,
            count: 0,
            rows: (0, Vec::new()),
            page_pending: None,
            page_gen: 0,
            pending_open: None,
            visible: 0..30,
            selected: None,
            restore: None,
            scroll: VirtualListScrollHandle::new(),
            positions: Positions::default(),
            confirm: None,
            confirm_many: None,
            more: More::default(),
            rights: HashMap::new(),
            labels: crate::labels::default_book(),
            scheduled: HashMap::new(),
            names: HashMap::new(),
            row_h: ROW_HEIGHT,
            filters,
            query: Query::default(),
            view: None,
            groups: Vec::new(),
            collapsed: HashSet::new(),
            layout: Layout::default(),
            sizes: Rc::new(Vec::new()),
            sizes_for: None,
            compute_gen: 0,
            computing: false,
            me: Me::default(),
            identity,
            waiting: Vec::new(),
            server_hits: Arc::new(HashSet::new()),
            order: Vec::new(),
            sort_open: false,
            selection: Default::default(),
            summary: None,
            summary_gen: 0,
            menu_row: None,
            bulk_menu: None,
            offline: None,
            offline_hidden: false,
            bounds: Rc::new(std::cell::Cell::new(Bounds::default())),
            outline: None,
            outlook: false,
            threads_open: HashSet::new(),
            thread_groups: Vec::new(),
            found: None,
            search_groups: Vec::new(),
            _subscriptions: subscriptions,
        };
        list.me = list.who_am_i();
        list.share_categories(cx);
        list
    }

    // --- фильтры (S5-03) ---

    fn who_am_i(&self) -> Me {
        Me::new(self.identity.iter().map(|(a, _)| a), self.identity.iter().map(|(_, n)| n)).with_waiting(&self.waiting)
    }

    /// Слова писем моих невыполненных поручений (S6-11): «Ждём» — ещё и эти письма.
    pub(crate) fn set_waiting(&mut self, words: Vec<String>, cx: &mut Context<Self>) {
        if self.waiting == words {
            return;
        }
        self.waiting = words;
        self.me = self.who_am_i();
        self.recompute(cx);
    }

    /// Мой адрес и мои адреса отправителя (живой ящик): кто я для упоминаний, «Мне» и внешних.
    pub(crate) fn on_sync(&mut self, ev: &postman_core::sync::Event, cx: &mut Context<Self>) {
        use postman_core::sync::Event;
        match ev {
            Event::Me { address, .. } => {
                self.identity.retain(|(a, _)| !a.eq_ignore_ascii_case(&address.email));
                self.identity.insert(0, (address.email.clone(), address.name.clone()));
            }
            Event::Senders(list) => {
                for s in list.iter().filter(|s| s.account.is_none()) {
                    if !self.identity.iter().any(|(a, _)| a.eq_ignore_ascii_case(&s.address.email)) {
                        self.identity.push((s.address.email.clone(), s.address.name.clone()));
                    }
                }
            }
            _ => return,
        }
        let me = self.who_am_i();
        if me != self.me {
            self.me = me;
            self.recompute(cx);
        }
    }

    /// Живой ящик: до `Event::Me` я неизвестен (демо-владелец — не я).
    pub(crate) fn forget_me(&mut self, cx: &mut Context<Self>) {
        self.identity.clear();
        self.me = Me::default();
        self.recompute(cx);
    }

    /// Сервер нашёл письма по фильтрам «Все папки» (они уже в кэше): пересчитать с ними.
    pub(crate) fn set_server_hits(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        self.server_hits = Arc::new(ids.into_iter().collect());
        self.recompute(cx);
    }

    /// Категории своей таблицы — панели фильтров.
    fn share_categories(&self, cx: &mut Context<Self>) {
        let cats: Vec<(String, String, String)> = self
            .labels
            .for_account(None)
            .map(|t| t.visible().map(|c| (c.keyword.clone(), c.name.clone(), c.shown_color().to_owned())).collect())
            .unwrap_or_default();
        self.filters.update(cx, |m, cx| {
            if m.categories != cats {
                m.set(cx, |m| m.categories = cats);
            }
        });
    }

    /// Условия фильтров изменились: пересчитать, список — сверху.
    fn on_filters(&mut self, cx: &mut Context<Self>) {
        let query = self.filters.read(cx).query.clone();
        if query == self.query {
            return;
        }
        // Другие условия — другой набор: прежний не показывать как новый, прокрутка — сверху.
        let own = query.own_list();
        self.query = query;
        self.server_hits = Arc::new(HashSet::new());
        // Беседы (S5-05б) — свой набор всегда: прежний остаётся до пересчёта.
        if !own && self.outline.is_none() {
            self.view = None;
        }
        self.selected = None;
        self.set_offset_y(0.);
        self.visible = rows_at(0., self.visible.len(), self.row_h);
        self.reload();
        self.recompute(cx);
        cx.notify();
    }

    /// Что перебирать для счётчиков: открытая папка или все папки её аккаунта.
    fn scan_target(&self) -> Option<Scan> {
        let mb = self.mailbox.as_deref()?;
        Some(match self.query.scope {
            Where::Folder => Scan::Mailbox(mb.to_owned()),
            Where::All => Scan::Account(postman_cache::account_of(mb).map(str::to_owned)),
        })
    }

    /// Пересчитать счётчики и (если нужен) свой набор писем — в фоне; поток интерфейса кэш не перебирает.
    pub(crate) fn recompute(&mut self, cx: &mut Context<Self>) {
        self.compute_gen += 1;
        let generation = self.compute_gen;
        let outlook = self.outlook_mode(cx);
        if outlook != self.outlook {
            self.outlook = outlook;
            self.outline = None;
        }
        let Some(scan) = self.scan_target() else {
            self.computing = false;
            return;
        };
        self.computing = true;
        let job = Job {
            scan,
            query: self.query.clone(),
            me: self.me.clone(),
            server_hits: self.server_hits.clone(),
            order: self.order.clone(),
            own: self.query.own_list(),
            overdue_first: crate::prefs::get(cx).lifecycle.overdue_first,
            outlook,
            open: self.threads_open.clone(),
        };
        let cache = self.cache.clone();
        let task = cx.background_spawn(async move { job.run(&cache) });
        cx.spawn(async move |this, cx| {
            let out = task.await;
            let _ = this.update(cx, |l, cx| l.computed(generation, out, cx));
        })
        .detach();
    }

    fn computed(&mut self, generation: u64, out: Computed, cx: &mut Context<Self>) {
        if generation != self.compute_gen {
            return;
        }
        self.computing = false;
        let counts = out.counts;
        self.filters.update(cx, |m, cx| {
            if m.counts != counts {
                m.set(cx, |m| m.counts = counts);
            }
        });
        match (out.list, out.outline) {
            (Some((_, groups)), Some(outline)) => self.set_outline(outline, &groups),
            (Some((ids, groups)), None) => {
                self.outline = None;
                self.view = Some(Arc::new(ids));
                self.groups = groups;
            }
            (None, _) if self.view.is_some() => {
                self.outline = None;
                self.view = None;
            }
            (None, _) => return,
        }
        self.reread(None, cx);
    }

    /// Откуда группы: поиск — расставленные поиском; свой набор — посчитанные; папка по дате — по датам.
    fn group_source(&self) -> Groups {
        match (&self.search_ids, &self.view, &self.mailbox) {
            (Some(_), _, _) => Groups::Fixed(self.search_groups.clone()),
            (None, Some(_), _) => Groups::Fixed(self.groups.clone()),
            (None, None, Some(mb)) if self.query.sort == Sort::Date => Groups::Days(mb.clone()),
            _ => Groups::None,
        }
    }

    /// Раскладка строк с заголовками групп — после смены числа писем, групп или свёрнутых.
    pub(crate) fn relayout(&mut self) {
        self.layout = Layout::new(&self.groups, self.count, &self.collapsed);
        self.sizes_for = None;
    }

    /// Щелчок по заголовку группы: свернуть или развернуть.
    pub(crate) fn toggle_group(&mut self, seg: usize, cx: &mut Context<Self>) {
        let Some(g) = self.layout.segs().get(seg).and_then(|s| s.group.clone()) else { return };
        if !self.collapsed.remove(&g) {
            self.collapsed.insert(g);
        }
        self.relayout();
        cx.notify();
    }

    /// Закладки быстрых фильтров видны (не скрыты, не поиск).
    fn quick_shown(&self, cx: &App) -> bool {
        self.search_ids.is_none() && crate::prefs::get(cx).quick.tabs().next().is_some()
    }

    /// Отложенные отправки с сервера (`Event::Scheduled`).
    pub(crate) fn set_scheduled(&mut self, scheduled: HashMap<String, i64>, cx: &mut Context<Self>) {
        self.scheduled = scheduled;
        cx.notify();
    }

    /// Таблицы категорий учётной записи (прочитаны из файла): строки — с их именами и цветами.
    pub(crate) fn set_labels(&mut self, book: Book, cx: &mut Context<Self>) {
        self.labels = book;
        self.share_categories(cx);
        cx.notify();
    }

    /// Клавиша 1–9: категория с этой клавишей на выбранном письме — поставить или снять (DESIGN.md, «Настройки»).
    pub(crate) fn toggle_category(&mut self, key: u8, cx: &mut Context<Self>) {
        let Some(mut row) = self.selected_row() else { return };
        let keyword = self
            .labels
            .for_account(postman_cache::account_of(&row.id))
            .and_then(|t| t.by_key(key))
            .map(|c| c.keyword.clone());
        let Some(keyword) = keyword else {
            cx.emit(LabelNotice::NoCategory(key));
            return;
        };
        if !may(&self.rights, &row, |r| r.keywords) {
            cx.emit(LabelNotice::ReadOnly);
            return;
        }
        let (add, remove) = labels::toggle([row.keywords.as_slice()], &keyword);
        for k in &remove {
            row.set_keyword(k, false);
        }
        for k in &add {
            row.set_keyword(k, true);
        }
        self.flags.send(Write::Keywords { id: row.id.clone(), add: add.clone(), remove: remove.clone() });
        self.patch_row(&row);
        cx.emit(Labeled { ids: vec![row.id.clone()], add, remove });
        cx.notify();
    }

    /// Права и имена папок после перечитывания папок: права запоминаются только урезанные.
    pub(crate) fn set_rights(&mut self, mailboxes: &[MailboxRow]) {
        self.names = mailboxes.iter().map(|m| (m.id.clone(), m.name.clone())).collect();
        self.rights = mailboxes.iter().filter(|m| m.rights != Rights::ALL).map(|m| (m.id.clone(), m.rights)).collect();
        self.order = mailboxes.iter().map(|m| m.id.clone()).collect();
    }

    pub(crate) fn show_mailbox(&mut self, id: String, cx: &mut Context<Self>) {
        // Другая папка — своя догрузка; число писем на сервере ставит экран (`Mailer::sync_total`).
        if self.mailbox.as_deref() != Some(id.as_str()) {
            self.more = More::default();
            self.threads_open.clear();
        }
        // Папка выбрана в дереве — искать в ней (не «Все папки»); фильтры остаются.
        let name = self.names.get(&id).cloned().unwrap_or_default();
        self.query.scope = Where::Folder;
        self.filters.update(cx, |m, cx| {
            m.set(cx, |m| {
                m.folder = name;
                m.query.scope = Where::Folder;
            })
        });
        self.view = None;
        self.outline = None;
        self.server_hits = Arc::new(HashSet::new());
        self.found = None;
        self.switch_view(Some(id), None);
        self.selected = None;
        self.restore = None;
        // Другая папка — выделение снято (S5-08).
        self.clear_selection(cx);
        self.reload();
        self.recompute(cx);
        cx.notify();
    }

    pub(crate) fn show_search(&mut self, ids: Option<Vec<String>>, cx: &mut Context<Self>) {
        let mailbox = self.mailbox.clone();
        self.switch_view(mailbox, ids);
        self.selected = None;
        self.clear_selection(cx);
        self.reload();
        cx.notify();
    }

    /// Найденное поиском (S5-10): тот же запрос — дополнить (выделение остаётся), новый — сверху без выделения.
    pub(crate) fn show_found(&mut self, ids: Vec<String>, groups: Vec<(Group, usize)>, found: crate::search::Found, cx: &mut Context<Self>) {
        let same = self.search_ids.is_some() && self.found.as_ref().is_some_and(|f| f.text == found.text && f.all == found.all);
        self.search_groups = groups;
        self.found = Some(found);
        if same {
            self.search_ids = Some(ids);
            self.reread(None, cx);
        } else {
            self.show_search(Some(ids), cx);
        }
    }

    /// Другая папка или поиск: запомнить прокрутку прежнего вида, поставить прокрутку нового.
    fn switch_view(&mut self, mailbox: Option<String>, search: Option<Vec<String>>) {
        let key = |mailbox: &Option<String>, search: &Option<Vec<String>>| match search {
            Some(_) => None,
            None => mailbox.clone(),
        };
        let from = key(&self.mailbox, &self.search_ids);
        let to = key(&mailbox, &search);
        let y = self.positions.switch(from.as_deref(), self.offset_y(), to.as_deref());
        self.mailbox = mailbox;
        self.search_ids = search;
        self.confirm = None;
        self.confirm_many = None;
        self.set_offset_y(y);
        self.visible = rows_at(y, self.visible.len(), self.row_h);
    }

    fn offset_y(&self) -> f32 {
        f32::from(self.scroll.offset().y)
    }

    fn set_offset_y(&self, y: f32) {
        self.scroll.set_offset(point(px(0.), px(y)));
    }

    pub(crate) fn selected_row(&self) -> Option<EmailRow> {
        self.selected.and_then(|i| self.row(i)).cloned()
    }

    /// «Удалить» в «Удалённых»: строка подтверждения над списком.
    pub(crate) fn ask_purge(&mut self, index: usize, row: EmailRow, cx: &mut Context<Self>) {
        self.confirm = Some((index, row));
        cx.notify();
    }

    /// Ответ на «Удалить навсегда?»; `false` — «Нет» и Esc.
    pub(crate) fn answer_purge(&mut self, yes: bool, cx: &mut Context<Self>) {
        if let Some(ids) = self.confirm_many.take()
            && yes
        {
            cx.emit(PurgeMany(ids));
        }
        if let Some((index, row)) = self.confirm.take()
            && yes
        {
            cx.emit(Purge { index, row });
        }
        cx.notify();
    }

    /// Письмо ушло из списка (в «Удалённые» или насовсем): перечитать и выделить следующее — оно теперь на
    /// месте удалённого.
    pub(crate) fn removed(&mut self, id: &str, index: usize, cx: &mut Context<Self>) {
        if let Some(ids) = &mut self.search_ids {
            ids.retain(|x| x != id);
            // Найденное поиском (S5-10): письмо уходит и из своей группы.
            if let Some(g) = self.layout.group_of(index).cloned()
                && let Some((_, n)) = self.search_groups.iter_mut().find(|(x, _)| *x == g)
            {
                *n = n.saturating_sub(1);
            }
        }
        // Беседы (вид «как Outlook», S5-05б): письмо уходит из своей беседы. Свой набор (фильтры): из него и из
        // своей группы.
        if self.search_ids.is_none() && self.outline.is_some() {
            self.outline_removed(id);
        } else if self.search_ids.is_none()
            && let Some(view) = &mut self.view
        {
            if let Some(g) = self.layout.group_of(index).cloned()
                && let Some((_, n)) = self.groups.iter_mut().find(|(x, _)| *x == g)
            {
                *n = n.saturating_sub(1);
            }
            Arc::make_mut(view).retain(|x| x != id);
        }
        self.confirm = None;
        self.reread(Some(index), cx);
    }

    pub(crate) fn reload(&mut self) {
        let source = self.page_source();
        self.count = source.count(&self.cache);
        self.groups = self.group_source().read(&self.cache, self.count);
        self.relayout();
        // Первое окно — сразу: первый кадр папки рисуется со строками, а не с заглушками.
        self.page_gen += 1;
        self.page_pending = None;
        self.pending_open = None;
        let (offset, limit) = page_window(&self.visible);
        self.rows = (offset, source.read(&self.cache, offset, limit));
        self.take_restore();
    }

    /// Выделить письмо `id`, когда его строка будет в окне (сейчас или после перечитывания) — без открытия:
    /// область чтения вкладка ставит сама (S5-16).
    pub(crate) fn reselect_id(&mut self, id: String, cx: &mut Context<Self>) {
        self.restore = Some(id);
        self.take_restore();
        if self.restore.is_some() {
            self.reread(None, cx);
        }
    }

    /// Ждущее выделение — если строка письма уже прочитана.
    fn take_restore(&mut self) {
        if let Some(i) = reselect(self.restore.as_deref(), self.rows.0, &self.rows.1, self.count) {
            self.selected = Some(i);
            self.restore = None;
        }
    }

    /// Данные изменились (синхронизация): число писем и окно строк — в фоне, выделение остаётся на том же письме.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.reread(None, cx);
        // Счётчики фильтров и свой набор — по новым данным.
        self.recompute(cx);
    }

    /// Перечитать в фоне; `select` — затем выделить и открыть письмо с этим номером (иначе — то же письмо).
    fn reread(&mut self, select: Option<usize>, cx: &mut Context<Self>) {
        let selected = self.selected.and_then(|i| self.row(i)).map(|r| r.id.clone()).or_else(|| self.restore.clone());
        let (offset, limit) = page_window(&self.visible);
        self.page_gen += 1;
        let generation = self.page_gen;
        self.page_pending = Some(offset..offset + limit);
        let (source, groups, cache) = (self.page_source(), self.group_source(), self.cache.clone());
        let task = cx.background_spawn(async move {
            let count = source.count(&cache);
            (count, groups.read(&cache, count), source.read(&cache, offset, limit))
        });
        cx.spawn(async move |this, cx| {
            let (count, groups, rows) = task.await;
            let _ = this.update(cx, |m, cx| {
                if m.page_gen != generation {
                    return;
                }
                m.page_pending = None;
                m.count = count;
                m.groups = groups;
                m.relayout();
                m.selected = reselect(selected.as_deref(), offset, &rows, count);
                if m.selected.is_some() {
                    m.restore = None;
                }
                m.rows = (offset, rows);
                if let Some(i) = select {
                    m.selected = None;
                    if count > 0 {
                        m.select(i.min(count - 1), cx);
                    }
                }
                // Выделение — по id: письма, которых в списке больше нет, уходят из него (S5-08).
                m.prune_selection(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Окно строк вокруг видимого диапазона. Читается в фоне — поток интерфейса не ждёт хранилище;
    /// следующее окно запрашивается заранее, когда до края загруженного меньше половины запаса.
    pub(crate) fn ensure(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        let (off, rows) = (self.rows.0, self.rows.1.len());
        let end = off + rows;
        let loaded = rows > 0 && range.start >= off && range.end <= end;
        let near_edge = (off > 0 && range.start < off + WINDOW_MARGIN / 2)
            || (end < self.count && range.end + WINDOW_MARGIN / 2 > end);
        if loaded && !near_edge {
            return;
        }
        if self.page_pending.as_ref().is_some_and(|p| range.start >= p.start && range.end <= p.end) {
            return;
        }
        let (offset, limit) = page_window(&range);
        self.page_gen += 1;
        let generation = self.page_gen;
        self.page_pending = Some(offset..offset + limit);
        let (source, cache) = (self.page_source(), self.cache.clone());
        let task = cx.background_spawn(async move { source.read(&cache, offset, limit) });
        cx.spawn(async move |this, cx| {
            let rows = task.await;
            let _ = this.update(cx, |m, cx| {
                if m.page_gen != generation {
                    return;
                }
                m.page_pending = None;
                m.rows = (offset, rows);
                if let Some(row) = m.pending_open.take().and_then(|i| m.row(i).cloned()) {
                    m.open_row(row, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Видно конец загруженного — попросить следующую страницу у сервера (один запрос до ответа).
    fn ask_more(&mut self, visible_end: usize, cx: &mut Context<Self>) {
        let Some(mb) = self.mailbox.clone().filter(|_| self.search_ids.is_none()) else { return };
        if self.more.wanted(visible_end, self.count) {
            self.more.asked = true;
            cx.emit(NeedMore(mb));
        }
    }

    /// Откуда читать строки списка: результаты поиска, свой набор по фильтрам или папка. Свой набор ещё
    /// считается — пока страницы папки («Все папки» — пусто, чтобы не выдать папку за все).
    pub(crate) fn page_source(&self) -> PageSource {
        match (&self.search_ids, &self.view, &self.mailbox) {
            (Some(ids), _, _) => PageSource::Ids(Arc::new(ids.clone())),
            (None, Some(view), _) => PageSource::Ids(view.clone()),
            (None, None, Some(_)) if self.query.scope == Where::All => PageSource::Empty,
            (None, None, Some(mb)) => PageSource::Mailbox(mb.clone()),
            _ => PageSource::Empty,
        }
    }

    pub(crate) fn row(&self, i: usize) -> Option<&EmailRow> {
        i.checked_sub(self.rows.0).and_then(|j| self.rows.1.get(j))
    }

    pub(crate) fn select(&mut self, i: usize, cx: &mut Context<Self>) {
        if self.count == 0 {
            return;
        }
        let i = i.min(self.count - 1);
        self.restore = None;
        // Щелчок по письму (без Ctrl и Shift) и стрелки — одно письмо: выделение снято (S5-08).
        self.clear_selection(cx);
        // Другое письмо — вопрос об удалении прежнего снят.
        if self.confirm.as_ref().is_some_and(|(c, _)| *c != i) {
            self.confirm = None;
        }
        // Письмо в свёрнутой группе — развернуть её.
        if self.layout.item_of(i).is_none()
            && let Some(g) = self.layout.group_of(i).cloned()
        {
            self.collapsed.remove(&g);
            self.relayout();
        }
        self.selected = Some(i);
        self.scroll.scroll_to_item(self.layout.item_of(i).unwrap_or(i), ScrollStrategy::Center);
        self.ensure(i..i + 1, cx);
        cx.notify();
        let Some(row) = self.row(i).cloned() else {
            self.pending_open = Some(i);
            return;
        };
        self.pending_open = None;
        self.open_row(row, cx);
    }

    /// При первом открытии письмо помечается прочитанным, затем уходит в область чтения.
    pub(crate) fn open_row(&mut self, mut row: EmailRow, cx: &mut Context<Self>) {
        // Папка общего ящика без права `maySetSeen` — письмо остаётся непрочитанным.
        if !row.seen && may(&self.rights, &row, |r| r.seen) {
            row.seen = true;
            self.flags.set(&row.id, true, row.flagged);
            self.patch_row(&row);
            cx.emit(Flagged { id: row.id.clone(), keyword: "$seen", on: true });
        }
        cx.emit(OpenRow(row));
    }

    pub(crate) fn patch_row(&mut self, row: &EmailRow) {
        for r in self.rows.1.iter_mut().filter(|r| r.id == row.id) {
            *r = row.clone();
        }
    }

    pub(crate) fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let cur = self.selected.map_or(0, |i| i as isize);
        let next = if self.selected.is_none() { 0 } else { (cur + delta).max(0) as usize };
        let next = next.min(self.count.saturating_sub(1));
        // Свёрнутые группы пропускаются: дальше по ходу, а если там нет — назад.
        let forward = delta >= 0;
        let Some(next) = self.layout.visible(next, forward).or_else(|| self.layout.visible(next, !forward)) else { return };
        self.select(next, cx);
    }

    pub(crate) fn set_flags(&mut self, seen: Option<bool>, toggle_flag: bool, cx: &mut Context<Self>) {
        let Some(mut row) = self.selected.and_then(|i| self.row(i)).cloned() else { return };
        let allowed = (seen.is_none() || may(&self.rights, &row, |r| r.seen))
            && (!toggle_flag || may(&self.rights, &row, |r| r.keywords));
        if !allowed {
            cx.emit(ReadOnly);
            return;
        }
        let old = row.clone();
        if let Some(s) = seen {
            row.seen = s;
        }
        if toggle_flag {
            row.flagged = !row.flagged;
        }
        self.flags.set(&row.id, row.seen, row.flagged);
        self.patch_row(&row);
        for (keyword, on) in keyword_changes(&old, row.seen, row.flagged) {
            cx.emit(Flagged { id: row.id.clone(), keyword, on });
        }
        cx.notify();
    }

    pub(crate) fn page(&self) -> isize {
        self.visible.len().max(1) as isize
    }

    /// Замер: строка `i` — первая видимая.
    pub(crate) fn scroll_to_top(&mut self, i: usize, cx: &mut Context<Self>) {
        self.scroll.scroll_to_item(self.layout.item_of(i).unwrap_or(i), ScrollStrategy::Top);
        cx.notify();
    }

    /// Высоты элементов: пересчёт, когда сменились раскладка, плотность или видимость закладок.
    fn sizes(&mut self, tail: bool) -> Rc<Vec<Size<Pixels>>> {
        let key = (self.row_h, tail);
        if self.sizes_for != Some(key) {
            let mut sizes = self.layout.sizes(self.row_h, HEADER_H, if tail { QUICK_H } else { 0. });
            // Письма под головой беседы (S5-05б) — строки ниже обычных.
            self.child_sizes(&mut sizes);
            self.sizes = Rc::new(sizes);
            self.sizes_for = Some(key);
        }
        self.sizes.clone()
    }

    /// Строка над списком: папка или «Все папки», чип фильтров панели с «×», «N из M», сортировка.
    fn header(&self, l: &Look, cx: &mut Context<Self>) -> Stateful<Div> {
        let m = self.filters.read(cx);
        let title = match (&self.search_ids, &self.mailbox) {
            (Some(_), _) => tr!("frame-list-search"),
            _ if self.query.scope == Where::All => tr!("filters-all-folders"),
            (None, Some(id)) => self.names.get(id).cloned().unwrap_or_default(),
            (None, None) => String::new(),
        };
        let caption = |text: String| div().flex_none().text_size(l.px(Metric::TypeCaption)).text_color(l.text2()).child(text);
        let filtered = self.search_ids.is_none() && !self.query.criteria.is_empty();
        // Беседы (вид «как Outlook», S5-05б): писем, а не строк.
        let letters = self.threads().map_or(self.count, |o| o.total());
        let summary = if filtered && m.counts.total > 0 {
            tr!("filters-shown", n = letters, total = m.counts.total)
        } else {
            tr!("frame-list-count", n = letters)
        };
        let chip = m.chip_text().filter(|_| self.search_ids.is_none()).map(|text| {
            let model = self.filters.clone();
            div()
                .flex()
                .flex_initial()
                .min_w_0()
                .items_center()
                .gap(px(4.))
                .h(px(24.))
                .pl(px(8.))
                .pr(px(2.))
                .rounded(px(12.))
                .bg(l.fill())
                .text_size(l.px(Metric::TypeCaption))
                .child(div().min_w_0().truncate().child(text))
                .child(
                    div()
                        .id("chip-reset")
                        .size(px(20.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(10.))
                        .cursor_pointer()
                        .hover(|d| d.bg(l.hover()))
                        .child(crate::chrome::icon(crate::chrome::glyph::CLOSE, 9.))
                        .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tr!("filters-chip-reset")).build(window, cx))
                        .on_click(move |_, _, cx| model.update(cx, |m, cx| m.change(cx, |q| q.criteria.clear_panel()))),
                )
        });
        let sort = div()
            .id("sort-button")
            .h(px(28.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(4.))
            .px(px(6.))
            .rounded(l.px(Metric::RadiusControl))
            .text_size(l.px(Metric::TypeCaption))
            .text_color(l.text2())
            .cursor_pointer()
            .when(self.sort_open, |d| d.bg(l.pressed()))
            .hover(|d| d.bg(l.hover()))
            .child(crate::chrome::icon(crate::chrome::glyph::SORT, 12.))
            .child(tr!(crate::filters::sort_key(self.query.sort)))
            .on_click(cx.listener(|this, _, _, cx| {
                this.sort_open = !this.sort_open;
                cx.notify();
            }));
        div()
            .id("list-header")
            .h(px(40.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.))
            .pl(px(14.))
            .pr(px(6.))
            .border_b_1()
            .border_color(l.border())
            .child(div().flex_none().max_w(px(160.)).truncate().font_weight(FontWeight::SEMIBOLD).child(title))
            .children(chip)
            .child(caption(summary))
            .child(div().flex_1())
            .child(sort)
    }

    /// Заголовок группы `seg` раскладки.
    fn group_el(&self, seg: usize, l: &Look, cx: &mut Context<Self>) -> AnyElement {
        let Some(s) = self.layout.segs().get(seg) else { return div().into_any_element() };
        let Some(g) = &s.group else { return div().into_any_element() };
        let label = crate::filters::group_label(g, &self.names);
        let folder = matches!(g, Group::Folder(_));
        crate::filters::group_header(seg, label, s.rows.len(), folder, s.collapsed, l)
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_group(seg, cx)))
            .into_any_element()
    }
}

impl Render for MailList {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let l = look(cx);
        self.row_h = row_height(l.density());
        let confirm = (self.confirm.is_some() || self.confirm_many.is_some()).then(|| {
            // Цвет кнопки — роль, не смысл (DESIGN «Цвет»): «да» — главная, «нет» — тонированная.
            let button = |id: &'static str, text: String, yes: bool| {
                crate::buttons::button(id, text, yes, &l).on_click(cx.listener(move |this, _, _, cx| this.answer_purge(yes, cx)))
            };
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_2()
                .px_3()
                .py_1p5()
                .bg(l.warning_bg())
                .border_b_1()
                .border_color(l.border())
                .child(div().flex_1().child(match &self.confirm_many {
                    Some(ids) => tr!("bulk-purge-confirm", n = ids.len()),
                    None => tr!("actions-purge-confirm"),
                }))
                .child(button("purge-yes", tr!("actions-purge-yes"), true))
                .child(button("purge-no", tr!("actions-purge-no"), false))
        });
        // Что-то выделено — панель массовых действий на месте строки над списком (S5-08); идёт поиск — строка поиска
        // с чипами «Уточнить» (S5-10).
        let header = match (self.selection.is_empty(), self.found.clone()) {
            (false, _) => self.bulk_header(&l, cx).into_any_element(),
            (true, Some(f)) => crate::search::header(&f, self.count, &l, cx).into_any_element(),
            (true, None) => self.header(&l, cx).into_any_element(),
        };
        let banner = self.offline_banner(&l, cx);
        let dropdown = self.bulk_dropdown(&l, cx);
        let quick = self.quick_shown(cx);
        let sizes = self.sizes(quick);
        let empty = self.count == 0 && self.search_ids.is_none() && self.view.is_some() && !self.query.criteria.is_empty();
        let nothing = self.count == 0 && self.found.as_ref().is_some_and(|f| !f.waiting) && self.page_pending.is_none();
        let marks = self.found.as_ref().map(|f| f.marks.clone());
        let list = v_virtual_list(cx.entity(), "messages", sizes, move |this, items: Range<usize>, _window, cx| {
            // Один элемент gpui запрашивает, чтобы измерить ширину, — это не видимая область.
            if items.len() > 1 {
                let rows = this.layout.rows_in(items.clone());
                this.visible = rows.clone();
                if !rows.is_empty() {
                    this.ensure(rows.clone(), cx);
                }
                this.ask_more(rows.end, cx);
            }
            let l = look(cx);
            let now = jiff::Zoned::now();
            items
                .map(|item| match this.layout.slot(item) {
                    Some(Slot::Header(seg)) => this.group_el(seg, &l, cx),
                    Some(Slot::Row(i)) => match this.row(i).cloned() {
                        Some(r) => {
                            let mut labels = crate::labels::row_labels(&this.labels, &r);
                            // Сроки (S5-04) — после категорий.
                            labels.marks = crate::lifecycle::row_badges(&r, &now);
                            labels.assigned = crate::shared::assigned(&r, cx);
                            // Ждёт отправки — первой меткой, нейтральной (DESIGN.md: служебные — серые).
                            let now = jiff::Timestamp::now().as_second();
                            if let Some(&at) = this.scheduled.get(&r.id).filter(|&&at| at > now) {
                                let text = tr!("send-scheduled-row", time = crate::send_later::short(at));
                                labels.chips.insert(0, (text, postman_core::palette::NEUTRAL));
                            }
                            // Выделение (S5-08): флажки у всех строк, пока что-то выделено; подсветка — выделенных.
                            let boxes = !this.selection.is_empty();
                            let state = RowState {
                                open: !boxes && this.selected == Some(i),
                                picked: this.selection.contains(&r.id),
                                boxes,
                                menu: this.menu_row == Some(i),
                                thread: this.thread_mark(i),
                            };
                            match state.thread {
                                crate::outline::Mark::Child { .. } => crate::outline::child_row(i, &r, labels, state, &l, cx),
                                _ => list_row(i, &r, labels, state, marks.as_deref().map(Vec::as_slice), &l, cx),
                            }
                        }
                        None if matches!(this.thread_mark(i), crate::outline::Mark::Child { .. }) => crate::outline::empty_child(i, &l),
                        None => empty_row(i, &l),
                    },
                    // Место под закладками быстрых фильтров: последнее письмо прокручивается над ними.
                    None => div().id(("tail", item)).w_full().h(px(QUICK_H)).into_any_element(),
                })
                .collect::<Vec<_>>()
        })
        .track_scroll(&self.scroll)
        .size_full();
        // Полоса прокрутки видна всегда и перетаскивается; кадры — только пока её двигают.
        // Закладки быстрых фильтров рисует экран почты поверх карточки (`Mailer::quick_overlay`): панель по
        // наведению шире списка. Здесь — только место под ними в конце списка.
        let bounds = self.bounds.clone();
        let mut body = div()
            .relative()
            .flex_1()
            .min_h_0()
            // Где список в окне — для контекстного меню с клавиатуры (Shift+F10) у выбранной строки.
            .child(canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {}).absolute().top_0().left_0().size_full());
        body = if nothing {
            body.child(div().pt(px(40.)).px(px(24.)).flex().justify_center().text_color(l.text2()).child(tr!("search-empty")))
        } else if empty {
            let model = self.filters.clone();
            body.child(
                div()
                    .pt(px(40.))
                    .px(px(24.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(8.))
                    .text_color(l.text2())
                    .child(tr!("filters-empty"))
                    .child(
                        div()
                            .id("empty-reset")
                            .text_color(l.accent_text())
                            .cursor_pointer()
                            .child(tr!("filters-chip-reset"))
                            .on_click(move |_, _, cx| {
                                model.update(cx, |m, cx| {
                                    m.change(cx, |q| {
                                        q.criteria.clear_panel();
                                        q.criteria.quick.clear();
                                    })
                                })
                            }),
                    ),
            )
        } else {
            body.child(list).child(Scrollbar::vertical(&self.scroll).mode(ScrollbarMode::Always))
        };
        let sort_menu = self.sort_open.then(|| {
            let menu = crate::filters::sort_menu(self.query.sort, &l, |s| match s {
                Sort::Date => Box::new(crate::filters::view::SortByDate),
                Sort::Sender => Box::new(crate::filters::view::SortBySender),
                Sort::Size => Box::new(crate::filters::view::SortBySize),
            });
            div()
                .id("sort-menu")
                .absolute()
                .top(px(38.))
                .right(px(6.))
                .child(menu)
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.sort_open = false;
                    cx.notify();
                }))
        });
        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .text_color(l.text())
            .child(header)
            .children(banner)
            .children(confirm)
            .child(body)
            .children(sort_menu)
            .children(dropdown)
    }
}

/// Окно чтения вокруг диапазона: (смещение, число строк).
pub(crate) fn page_window(range: &Range<usize>) -> (usize, usize) {
    (range.start.saturating_sub(WINDOW_MARGIN), range.len() + 2 * WINDOW_MARGIN)
}

pub(crate) enum PageSource {
    Mailbox(String),
    Ids(Arc<Vec<String>>),
    Empty,
}

impl PageSource {
    pub(crate) fn count(&self, cache: &Cache) -> usize {
        match self {
            PageSource::Mailbox(mb) => cache.count(mb).unwrap_or(0),
            PageSource::Ids(ids) => ids.len(),
            PageSource::Empty => 0,
        }
    }

    pub(crate) fn read(&self, cache: &Cache, offset: usize, limit: usize) -> Vec<EmailRow> {
        match self {
            PageSource::Mailbox(mb) => cache.page(mb, offset, limit).unwrap_or_default(),
            PageSource::Ids(ids) => {
                let end = (offset + limit).min(ids.len());
                cache.rows_by_ids(&ids[offset.min(end)..end]).unwrap_or_default()
            }
            PageSource::Empty => Vec::new(),
        }
    }
}

/// Строка, которая ещё читается из хранилища: та же высота, без текста.
pub(crate) fn empty_row(i: usize, l: &Look) -> AnyElement {
    div().id(i).w_full().h(px(row_height(l.density()))).border_b_1().border_color(l.border()).into_any_element()
}

/// Инициалы для аватара: первые буквы двух первых слов имени (или адреса).
pub(crate) fn initials(name: &str) -> String {
    name.split(|c: char| c.is_whitespace() || c == '.' || c == '@')
        .filter_map(|w| w.chars().find(|c| c.is_alphanumeric()))
        .take(2)
        .flat_map(char::to_uppercase)
        .collect()
}

/// Как рисовать строку: открыта (курсор), выделена, флажки видны у всех (что-то выделено), под контекстным меню;
/// место в беседе (вид «как Outlook», S5-05б).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RowState {
    pub(crate) open: bool,
    pub(crate) picked: bool,
    pub(crate) boxes: bool,
    pub(crate) menu: bool,
    pub(crate) thread: crate::outline::Mark,
}

/// Группа наведения строки: флажок на месте аватара по наведению (S5-08).
const ROW_GROUP: &str = "mail-row";

/// Флажок выделения (18 px): выделено — заливка акцентом с галочкой, иначе — рамка.
pub(crate) fn checkbox(on: bool, l: &Look) -> Div {
    div()
        .size(px(18.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .when(on, |d| d.bg(l.accent()).text_color(l.on_accent()).child(crate::chrome::icon(crate::chrome::glyph::CHECK, 12.)))
        .when(!on, |d| d.bg(l.panel()).border_1().border_color(l.c(pets_ui::theme::Role::TextTertiary)))
}

/// Строка письма (DESIGN «Строка письма и метки»): слева полоса категории; отправитель (сокращается, ≥ 60 px),
/// справа метки (обрезаются слева, категория первой), вложение, флаг и время; ниже тема; при обычной плотности —
/// аватар и превью. Выделение (S5-08): флажок на месте аватара — по наведению и у всех строк, пока что-то выделено;
/// Ctrl+щелчок — добавить или убрать, Shift+щелчок — диапазон, правая кнопка — контекстное меню.
pub(crate) fn list_row(
    i: usize,
    r: &EmailRow,
    labels: crate::labels::RowLabels,
    state: RowState,
    marks: Option<&[String]>,
    l: &Look,
    cx: &mut Context<MailList>,
) -> AnyElement {
    let from = if r.from_name.is_empty() { r.from_addr.clone() } else { r.from_name.clone() };
    let roomy = !l.compact();
    // Шифрованное (S5-12 Ф2): сервер текста не видит — замок и «Зашифрованное письмо» вместо превью.
    let encrypted = crate::smime_enc::is_encrypted(r);
    // Аватары и превью — «Настройки → Внешний вид» (S5-30); в компактной плотности их нет всегда.
    let (avatars, previews) = crate::prefs::with(cx, |p| (p.look.avatars, p.look.preview));
    let stripe = labels.stripe.map(|c| l.cat(c).dot);
    let avatar = (roomy && avatars).then(|| {
        let c = l.cat(labels.stripe.unwrap_or(postman_core::palette::NEUTRAL));
        div()
            .size(px(32.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(c.bg)
            .text_color(c.text)
            .text_size(px(12.))
            .when(!state.boxes, |d| d.group_hover(ROW_GROUP, |s| s.invisible()))
            .when(state.boxes, |d| d.invisible())
            .child(initials(&from))
    });
    // Флажок — поверх аватара (при компактной плотности — на его месте, 18 px).
    let check = div()
        .id(("pick", i))
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .when(!state.boxes, |d| d.invisible().group_hover(ROW_GROUP, |s| s.visible()))
        .child(checkbox(state.picked, l))
        .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tr!("bulk-pick")).build(window, cx))
        .on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            this.pick(i, false, cx);
        }));
    let slot = div()
        .relative()
        .flex_none()
        .w(px(if roomy { 32. } else { 18. }))
        .h(px(32.))
        .flex()
        .items_center()
        .justify_center()
        .children(avatar)
        .child(check);
    let chips = div()
        .flex_1()
        .min_w_0()
        .flex()
        .justify_end()
        .gap(px(4.))
        .overflow_hidden()
        .children(labels.chips.into_iter().map(|(name, color)| crate::labels::chip(name, color, l)))
        .children(labels.marks.iter().map(|b| crate::lifecycle::chip(b, l)));
    let line1 = div()
        .flex()
        .items_center()
        .gap(px(6.))
        .child(
            div()
                .flex_initial()
                .min_w(px(60.))
                .truncate()
                .when(!r.seen, |d| d.font_weight(FontWeight::SEMIBOLD))
                .child(crate::search::marked(&from, marks, l)),
        )
        .child(chips)
        // Число писем беседы (S5-05б).
        .children(state.thread.count(l))
        .when(encrypted, |d| d.child(crate::chrome::icon(crate::smime_enc::LOCK, 13.).text_color(l.text3())))
        .when(r.has_attachment && !encrypted, |d| d.child(crate::chrome::icon(crate::chrome::glyph::ATTACH, 13.).text_color(l.text3())))
        .when(r.flagged, |d| d.child(crate::chrome::icon(crate::chrome::glyph::FLAG, 13.).text_color(l.pm(Pm::Flag))))
        .child(div().flex_none().text_size(l.px(Metric::TypeCaption)).text_color(l.text2()).child(format_date(r.received_at)));
    let subject = div()
        .flex_1()
        .min_w_0()
        .truncate()
        .when(!r.seen, |d| d.text_color(l.accent_text()).font_weight(FontWeight::SEMIBOLD))
        .when(r.seen, |d| d.text_color(l.text()))
        .child(crate::search::marked(&r.subject, marks, l));
    // «Назначено: …» — справа в строке темы, нейтральной меткой (S5-28, макет `SharedMailboxes`).
    let assigned = labels.assigned.clone().map(|t| crate::labels::chip(t, postman_core::palette::NEUTRAL, l).max_w(px(160.)));
    let subject = div().flex().items_center().gap(px(8.)).min_w_0().child(subject).children(assigned);
    let preview = (roomy && previews).then(|| {
        let p = div().truncate().text_size(l.px(Metric::TypeCaption)).text_color(l.text2());
        if encrypted { p.child(tr!("smime-list-preview")) } else { p.child(crate::search::marked(&r.preview, marks, l)) }
    });
    let lit = state.open || state.picked;
    // Выделенная строка — нейтральная подложка «выбранное» и полоса акцента слева (решение владельца 2026-10-22).
    let stripe = if lit { Some(l.accent()) } else { stripe };
    let open_head = state.thread.open_head();
    div()
        .id(i)
        .group(ROW_GROUP)
        .relative()
        .w_full()
        .h(px(row_height(l.density())))
        .flex()
        .gap(px(10.))
        .pr(px(12.))
        // Развёрнутая беседа — без черты снизу, к письмам под ней — линия (макет «В списке, как Outlook»).
        .when(!open_head, |d| d.border_b_1())
        .border_color(l.border())
        .when(lit, |d| d.bg(l.selected()))
        .when(!lit, |d| d.hover(|d| d.bg(l.hover())))
        // Строка под контекстным меню — с рамкой акцента (макет «Выделение»).
        .when(state.menu, |d| d.border_1().border_color(l.accent_text()))
        .cursor_pointer()
        // Слева — полоса категории (S5-02). Синей полосы «не прочитано» нет: её не отличить от синей категории;
        // непрочитанное — жирным отправителем и цветной темой (DESIGN.md).
        .child(div().w(px(4.)).h_full().flex_none().rounded_r(px(2.)).when_some(stripe, |d, c| d.bg(c)))
        // Стрелка беседы и линия к письмам под головой (S5-05б).
        .children(state.thread.chevron(i, l, cx))
        .children(open_head.then(|| crate::outline::guide(l)))
        .child(div().flex_none().flex().items_center().child(slot))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .justify_center()
                .gap(px(2.))
                .line_height(px(18.))
                .child(line1)
                .child(subject)
                .children(preview),
        )
        .on_click(cx.listener(move |this, ev: &ClickEvent, _, cx| {
            let m = ev.modifiers();
            if m.shift {
                this.extend_to(i, cx);
            } else if m.control {
                this.pick(i, true, cx);
            } else {
                this.select(i, cx);
            }
        }))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |_, ev: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                cx.emit(crate::bulk::OpenMenu { index: i, at: Some(ev.position) });
            }),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    // Не `super::*`: из `gpui_kit::*` пришёл бы атрибут `test` gpui вместо встроенного.
    use super::{EmailRow, More, Positions, ROW_HEIGHT, WINDOW_MARGIN, keyword_changes, may, reselect, rows_at};
    use postman_cache::Rights;
    use std::collections::HashMap;

    fn row(id: &str, seen: bool, flagged: bool) -> EmailRow {
        EmailRow {
            id: id.into(),
            thread_id: String::new(),
            mailbox_ids: Vec::new(),
            received_at: 0,
            subject: String::new(),
            from_name: String::new(),
            from_addr: String::new(),
            preview: String::new(),
            seen,
            flagged,
            has_attachment: false,
            size: 0,
            keywords: Vec::new(),
        }
    }

    #[test]
    fn flags_become_keywords() {
        let r = row("a", false, false);
        assert_eq!(keyword_changes(&r, false, false), vec![]);
        assert_eq!(keyword_changes(&r, true, false), vec![("$seen", true)]);
        assert_eq!(keyword_changes(&r, true, true), vec![("$seen", true), ("$flagged", true)]);
        assert_eq!(keyword_changes(&row("a", true, true), true, false), vec![("$flagged", false)]);
    }

    #[test]
    fn each_folder_keeps_its_scroll_position() {
        let mut p = Positions::default();
        // Входящие прокручены на 10 строк вниз → Архив открывается сверху.
        let inbox = -10. * ROW_HEIGHT;
        assert_eq!(p.switch(Some("inbox"), inbox, Some("archive")), 0.);
        // Архив прокручен на 3 строки → во Входящих снова 10-я строка, в Архиве — 3-я.
        assert_eq!(p.switch(Some("archive"), -3. * ROW_HEIGHT, Some("inbox")), inbox);
        assert_eq!(p.switch(Some("inbox"), inbox, Some("archive")), -3. * ROW_HEIGHT);
        // Поиск — сверху и не запоминается; сброс поиска — назад в папку, где была прокрутка.
        assert_eq!(p.switch(Some("archive"), -3. * ROW_HEIGHT, None), 0.);
        assert_eq!(p.switch(None, -50. * ROW_HEIGHT, Some("archive")), -3. * ROW_HEIGHT);
        // Окно строк для первого чтения — там, куда прокручено.
        assert_eq!(rows_at(inbox, 30, ROW_HEIGHT), 10..40);
        assert_eq!(rows_at(0., 30, ROW_HEIGHT), 0..30);
    }

    #[test]
    fn more_is_asked_near_the_end_once() {
        // Демо и поиск: числа на сервере нет — не просить.
        let mut m = More::default();
        assert!(!m.wanted(500, 500));
        // В кэше 500 из 20 000: далеко от конца — рано, у конца — просить.
        m.set_total(Some(20_000));
        assert!(!m.wanted(30, 500));
        assert!(m.wanted(500 - WINDOW_MARGIN, 500));
        // Запрошено — второй раз не просить, пока нет ответа.
        m.asked = true;
        assert!(!m.wanted(500, 500));
        m.loaded(500);
        assert!(m.wanted(1000, 1000));
        // Всё загружено — не просить.
        assert!(!m.wanted(20_000, 20_000));
        // Пустая страница: больше нет; новые письма на сервере (число изменилось) — снова можно.
        m.loaded(0);
        assert!(!m.wanted(1000, 1000));
        m.set_total(Some(20_000));
        assert!(!m.wanted(1000, 1000), "the same total keeps the mailbox exhausted");
        m.set_total(Some(20_001));
        assert!(m.wanted(1000, 1000));
    }

    #[test]
    fn selection_follows_the_letter() {
        let rows = vec![row("new", false, false), row("a", true, false), row("b", true, false)];
        // Пришло новое письмо сверху: выбранное «a» сдвинулось на одну строку.
        assert_eq!(reselect(Some("a"), 0, &rows, 3), Some(1));
        assert_eq!(reselect(Some("b"), 100, &rows, 200), Some(102));
        // Письмо исчезло (удалено, перенесено) — выделения нет.
        assert_eq!(reselect(Some("gone"), 0, &rows, 3), None);
        assert_eq!(reselect(None, 0, &rows, 3), None);
    }

    /// Права общих ящиков (S4-06): действие можно, если хоть одна папка письма его разрешает; свои папки — всегда.
    #[test]
    fn shared_folder_rights_gate_actions() {
        let ro = Rights { read: true, add: false, remove: false, seen: true, keywords: false, submit: false };
        let rights: HashMap<String, Rights> = [("j/in".to_string(), ro)].into();
        let mut r = row("j/1", false, false);
        r.mailbox_ids = vec!["j/in".into()];
        assert!(may(&rights, &r, |x| x.seen));
        assert!(!may(&rights, &r, |x| x.keywords) && !may(&rights, &r, |x| x.remove));
        r.mailbox_ids.push("j/other".into());
        assert!(may(&rights, &r, |x| x.remove), "папка без урезанных прав");
        let own = row("1", false, false);
        assert!(may(&rights, &own, |x| x.remove));
    }
}
