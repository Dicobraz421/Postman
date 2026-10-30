//! Поиск в командной строке (S5-10, `docs/DESIGN.md` «Палитра команд и поиск»): «Везде | Текущая папка»,
//! синтаксис `from:`, `to:`, `subject:`, `has:attachment`, `is:unread`, `before:`/`after:`, `category:`
//! (`postman_core::query`), результаты группами по папкам (как «Все папки» S5-03, `postman_core::filters::arrange`),
//! подсветка совпадений янтарным (роль `app.search.hit`) в строках списка и в письме, чипы «Уточнить».
//!
//! Поток интерфейса не ищет: ввод ждёт паузы ([`DEBOUNCE`]), поиск по своим данным (индекс tantivy или кэш, если
//! индекса нет) и проверка условий — в фоне; живой ящик в «Везде» и «Текущей папке» ещё спрашивает сервер
//! (`Email/query` с теми же условиями), найденное сводится по id. Каждый запуск — свой номер: ответы прежних
//! запусков (фон, сервер) отбрасываются.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::Metric;
use postman_cache::{Cache, EmailRow, Recipients, Scan};
use postman_core::filters::{Days, Group, Hit, Sort, arrange};
use postman_core::fuzzy::term_spans;
use postman_core::query::{Env, Query, Refine, suggest};

use crate::Mailer;
use crate::roles::{Look, Pm};

/// Пауза ввода перед поиском.
pub(crate) const DEBOUNCE: Duration = Duration::from_millis(250);
/// Найти не больше — как у сервера.
const LIMIT: usize = 1000;
/// Писем для чипов «Уточнить» (первые по релевантности).
const SAMPLE: usize = 500;

/// Поиск экрана почты: что ищется и что уже пришло.
#[derive(Default)]
pub(crate) struct Find {
    /// Номер нажатия: поиск после паузы — только для последнего.
    typed: u64,
    /// Номер запуска: ответы прежних запусков отбрасываются.
    generation: u64,
    /// Номер расстановки найденного: поздняя расстановка прежнего набора не затирает новую.
    arranged: u64,
    /// Строка запроса последнего запуска (пусто — поиска нет).
    pub(crate) text: String,
    query: Query,
    /// Найдено у себя (индекс или кэш) и на сервере; `None` — ответа ещё нет.
    local: Option<Vec<String>>,
    server: Option<Vec<String>>,
    /// Запрос к серверу этого запуска.
    ticket: Option<u64>,
    /// «Везде» (иначе — текущая папка).
    all: bool,
}

impl Find {
    pub(crate) fn active(&self) -> bool {
        !self.text.is_empty()
    }
}

/// Что показывает список во время поиска: строка запроса, слова для подсветки, чипы «Уточнить».
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Found {
    pub(crate) text: String,
    pub(crate) marks: Arc<Vec<String>>,
    /// Чипы: условие и включено ли оно.
    pub(crate) chips: Vec<(Refine, bool)>,
    pub(crate) all: bool,
    /// Имя папки «Текущая папка».
    pub(crate) folder: String,
    /// Сервер ещё ищет.
    pub(crate) waiting: bool,
}

/// Щелчок по чипу «Уточнить» (список → экран почты: строка поиска — у него).
pub(crate) struct RefineChip(pub(crate) Refine);

impl EventEmitter<RefineChip> for crate::list::MailList {}

/// Поиск по своим данным в фоне: слова — индексом (или кэшем, пока индекса нет), только условия — обходом папки
/// или всех папок аккаунта; затем условия по кэшу.
struct Local {
    cache: Cache,
    index: postman_core::sync::Index,
    query: Query,
    env: Env,
    mailbox: Option<String>,
    account: Option<String>,
}

impl Local {
    fn run(self) -> Vec<String> {
        let text = self.query.text();
        if text.is_empty() {
            let scan = match &self.mailbox {
                Some(mb) => Scan::Mailbox(mb.clone()),
                None => Scan::Account(self.account.clone()),
            };
            let mut hits: Vec<(i64, String)> = Vec::new();
            let scanned = self.cache.scan(&scan, |row, to| {
                if self.query.matches(&self.env, &row, &to) {
                    hits.push((row.received_at, row.id));
                }
                true
            });
            if let Err(e) = scanned {
                tracing::warn!("search scan: {e}");
            }
            hits.sort_by_key(|h| std::cmp::Reverse(h.0));
            return hits.into_iter().take(LIMIT).map(|(_, id)| id).collect();
        }
        let ids = crate::live::local_search(&self.index, &self.cache, &text, self.mailbox.as_deref());
        if !self.query.has_conditions() {
            return ids;
        }
        let rows = self.cache.rows_by_ids(&ids).unwrap_or_default();
        rows.into_iter().filter(|r| self.query.matches(&self.env, r, &self.cache.recipients(&r.id).unwrap_or_default())).map(|r| r.id).collect()
    }
}

/// Найденное по порядку: группы по папкам («Везде») или по датам (папка), чипы — по первым письмам.
struct Arranged {
    ids: Vec<String>,
    groups: Vec<(Group, usize)>,
    chips: Vec<Refine>,
}

struct Arrange {
    cache: Cache,
    ids: Vec<String>,
    all: bool,
    order: Vec<String>,
    query: Query,
    categories: Vec<(String, String)>,
    me: Vec<String>,
}

impl Arrange {
    fn run(self) -> Arranged {
        let rows = self.cache.rows_by_ids(&self.ids).unwrap_or_default();
        let index: HashMap<&str, usize> = self.order.iter().enumerate().map(|(i, id)| (id.as_str(), i)).collect();
        let now = jiff::Zoned::now();
        let sample: Vec<(EmailRow, Recipients)> =
            rows.iter().take(SAMPLE).map(|r| (r.clone(), self.cache.recipients(&r.id).unwrap_or_default())).collect();
        let chips = suggest(&self.query, &sample, &now, &self.categories, &self.me);
        let hits: Vec<Hit> = rows
            .iter()
            .map(|r| {
                // Папка письма — первая по дереву; папки нет в дереве — в конце.
                let folder = r.mailbox_ids.iter().filter_map(|m| index.get(m.as_str())).min().copied();
                Hit::of(r, if self.all { folder.unwrap_or(usize::MAX) } else { 0 })
            })
            .collect();
        let folders = self.all.then_some(self.order.as_slice());
        let (ids, groups) = arrange(hits, Sort::Date, folders, &Days::at(&now));
        Arranged { ids, groups, chips }
    }
}

impl Mailer {
    /// Набор в строке поиска: искать после паузы (последнее нажатие).
    pub(crate) fn search_typed(&mut self, text: String, cx: &mut Context<Self>) {
        self.find.typed += 1;
        let typed = self.find.typed;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            let _ = this.update(cx, |m, cx| {
                if m.find.typed == typed && text.trim() != m.find.text {
                    m.run_search(&text, cx);
                }
            });
        })
        .detach();
    }

    /// Искать сейчас (Enter, чип, охват); пустой запрос — назад к папке.
    pub(crate) fn run_search(&mut self, q: &str, cx: &mut Context<Self>) {
        self.find.typed += 1;
        let text = q.trim().to_owned();
        let query = Query::parse(&text);
        if query.is_empty() {
            return self.end_search(cx);
        }
        let all = self.bar.read(cx).scope == crate::bar::Scope::All;
        let mailbox = self.search_mailbox(cx);
        let list = self.list.read(cx);
        let account = list.mailbox.as_deref().and_then(postman_cache::account_of).map(str::to_owned);
        let me = list.me.addresses().to_vec();
        let categories = own_categories(list);
        let env = query.env(&jiff::Zoned::now(), |name| category_keyword(&categories, name), &me);
        let generation = self.find.generation + 1;
        let mut find = Find {
            typed: self.find.typed,
            generation,
            arranged: self.find.arranged,
            text: text.clone(),
            query: query.clone(),
            all,
            ..Find::default()
        };
        // Живой ящик: те же условия — серверу (в кэше — лишь первые страницы папок).
        if self.backend.is_live()
            && let Some(filter) = query.server_filter(&env)
        {
            let ticket = self.live.ticket();
            if self.backend.send(postman_core::sync::Command::Query { filter, mailbox: mailbox.clone(), ticket }) {
                find.ticket = Some(ticket);
            } else {
                self.sync_stopped(cx);
            }
        }
        if find.ticket.is_none() {
            find.server = Some(Vec::new());
        }
        self.find = find;
        self.reading.update(cx, |r, cx| r.clear(cx));
        let job = Local { cache: self.cache.clone(), index: self.live.index(), query, env, mailbox, account };
        let task = cx.background_spawn(async move { job.run() });
        cx.spawn(async move |this, cx| {
            let ids = task.await;
            let _ = this.update(cx, |m, cx| {
                if m.find.generation == generation {
                    m.find.local = Some(ids);
                    m.arrange_found(cx);
                }
            });
        })
        .detach();
    }

    /// Ответ сервера на поиск; `false` — не этого поиска (фильтры «Все папки» или прежний запуск).
    pub(crate) fn on_search_results(&mut self, ticket: u64, ids: &[String], cx: &mut Context<Self>) -> bool {
        if self.find.ticket != Some(ticket) {
            return false;
        }
        self.find.server = Some(ids.to_vec());
        self.arrange_found(cx);
        true
    }

    /// Свести найденное (своё — первым, затем то, что нашёл только сервер) и расставить в фоне.
    fn arrange_found(&mut self, cx: &mut Context<Self>) {
        let Some(local) = &self.find.local else { return };
        let ids = crate::live::merge(local, self.find.server.as_deref().unwrap_or_default());
        // Ничего не нашлось — строкой в списке («search-empty»), без тоста.
        let waiting = self.find.server.is_none();
        self.find.arranged += 1;
        let (generation, arranged) = (self.find.generation, self.find.arranged);
        let list = self.list.read(cx);
        let job = Arrange {
            cache: self.cache.clone(),
            ids,
            all: self.find.all,
            order: list.order.clone(),
            query: self.find.query.clone(),
            categories: own_categories(list),
            me: list.me.addresses().to_vec(),
        };
        let folder = list.mailbox.as_ref().and_then(|mb| list.names.get(mb)).cloned().unwrap_or_default();
        let found =
            Found { text: self.find.text.clone(), marks: Arc::new(self.find.query.marks()), chips: Vec::new(), all: self.find.all, folder, waiting };
        let task = cx.background_spawn(async move { job.run() });
        cx.spawn(async move |this, cx| {
            let out = task.await;
            let _ = this.update(cx, |m, cx| {
                if m.find.generation != generation || m.find.arranged != arranged {
                    return;
                }
                let query = m.find.query.clone();
                let chips = out.chips.into_iter().map(|r| (r.clone(), r.active(&query))).collect();
                let found = Found { chips, ..found };
                let n = out.ids.len();
                m.folders.update(cx, |f, cx| {
                    f.results = Some(n);
                    cx.notify();
                });
                let marks = Some((found.text.clone(), found.marks.clone()));
                m.reading.update(cx, |r, cx| r.set_marks(marks, cx));
                m.list.update(cx, |l, cx| l.show_found(out.ids, out.groups, found, cx));
            });
        })
        .detach();
    }

    /// Поиска нет: список — папка, подсветки нет; поздние ответы отбрасываются.
    pub(crate) fn end_search(&mut self, cx: &mut Context<Self>) {
        self.find = Find { typed: self.find.typed, generation: self.find.generation + 1, ..Find::default() };
        self.folders.update(cx, |f, cx| {
            f.results = None;
            cx.notify();
        });
        self.reading.update(cx, |r, cx| {
            r.set_marks(None, cx);
            r.clear(cx);
        });
        self.list.update(cx, |l, cx| {
            l.found = None;
            l.show_search(None, cx);
        });
    }

    /// Открыта папка (дерево, палитра): поиск закончен без перечитывания списка — его перечитает папка.
    pub(crate) fn drop_search(&mut self, cx: &mut Context<Self>) {
        if !self.find.active() {
            return;
        }
        self.find = Find { typed: self.find.typed, generation: self.find.generation + 1, ..Find::default() };
        self.folders.update(cx, |f, _| f.results = None);
        self.reading.update(cx, |r, cx| r.set_marks(None, cx));
        self.list.update(cx, |l, _| l.found = None);
    }

    /// Чип «Уточнить» или команда уточнения: условие — в строку поиска (или из неё) и искать.
    pub(crate) fn refine(&mut self, r: &Refine, window: &mut Window, cx: &mut Context<Self>) {
        let now = self.search.read(cx).value().to_string();
        let next = r.apply(&now);
        self.search.update(cx, |s, cx| s.set_value(next.clone(), window, cx));
        self.run_search(&next, cx);
    }

    /// «Везде | Текущая папка» командой: охват — в командную строку, поиск — заново.
    pub(crate) fn set_scope(&mut self, scope: crate::bar::Scope, cx: &mut Context<Self>) {
        self.bar.update(cx, |b, cx| {
            b.scope = scope;
            cx.notify();
        });
        if self.find.active() {
            let text = self.find.text.clone();
            self.run_search(&text, cx);
        }
    }
}

/// Категории своей таблицы: (ключевое слово, имя).
fn own_categories(list: &crate::list::MailList) -> Vec<(String, String)> {
    list.labels.for_account(None).map(|t| t.visible().map(|c| (c.keyword.clone(), c.name.clone())).collect()).unwrap_or_default()
}

fn category_keyword(categories: &[(String, String)], name: &str) -> Option<String> {
    let name = postman_core::fuzzy::folded(name);
    categories.iter().find(|(_, n)| postman_core::fuzzy::folded(n) == name).map(|(k, _)| k.clone())
}

// --- отрисовка ---

/// Подпись чипа «Уточнить».
pub(crate) fn chip_label(r: &Refine) -> String {
    match r {
        Refine::From(name) => tr!("search-refine-from", name = name.clone()),
        Refine::ToMe => tr!("search-refine-to-me"),
        Refine::Attachment => tr!("search-refine-attachment"),
        Refine::Unread => tr!("search-refine-unread"),
        Refine::Within(d) => tr!("search-refine-within", n = *d),
        Refine::Category(name) => tr!("search-refine-category", name = name.clone()),
    }
}

/// Подсветка совпадений поиска: янтарная подложка (DESIGN «Цвет»).
pub(crate) fn hit_style(l: &Look) -> HighlightStyle {
    HighlightStyle { background_color: Some(l.pm(Pm::SearchHit)), ..Default::default() }
}

/// Текущее совпадение в письме (F3 / Shift+F3, S5-14): плотная янтарная заливка, как у просроченного срока, и
/// черта снизу — без полужирного, чтобы строка не перестраивалась.
pub(crate) fn current_hit_style(l: &Look) -> HighlightStyle {
    let on = l.pm(Pm::DeadlineOn);
    HighlightStyle {
        background_color: Some(l.pm(Pm::DeadlineFill)),
        color: Some(on),
        underline: Some(UnderlineStyle { thickness: px(2.), color: Some(on), wavy: false }),
        ..Default::default()
    }
}

/// Текст с подсвеченными совпадениями; совпадений нет — просто текст.
pub(crate) fn marked(text: &str, marks: Option<&[String]>, l: &Look) -> AnyElement {
    let spans = marks.map(|m| term_spans(text, m)).unwrap_or_default();
    if spans.is_empty() {
        return text.to_owned().into_any_element();
    }
    let style = hit_style(l);
    StyledText::new(text.to_owned()).with_highlights(spans.into_iter().map(|r| (r, style))).into_any_element()
}

/// Строка над списком во время поиска: «Поиск: …», «N результатов · все папки», «×» — к папке; под ней — чипы.
pub(crate) fn header(found: &Found, count: usize, l: &Look, cx: &mut Context<crate::list::MailList>) -> Div {
    let place = if found.all { tr!("search-where-all") } else { found.folder.clone() };
    let summary =
        if found.waiting { tr!("search-summary-waiting", n = count, place = place) } else { tr!("search-summary", n = count, place = place) };
    let close = div()
        .id("search-close")
        .size(px(28.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(l.px(Metric::RadiusControl))
        .text_color(l.text2())
        .cursor_pointer()
        .hover(|d| d.bg(l.hover()))
        .child(crate::chrome::icon(crate::chrome::glyph::CLOSE, 10.))
        .tooltip({
            let tip = crate::keytips::with_key(tr!("search-close"), crate::keytips::key("search.clear", cx));
            move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
        })
        .on_click(|_, window, cx| window.dispatch_action(Box::new(crate::search::search::Clear), cx));
    let line = div()
        .id("list-header")
        .h(px(40.))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(8.))
        .pl(px(14.))
        .pr(px(6.))
        .child(div().flex_initial().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD).child(tr!("search-title", query = found.text.clone())))
        .child(div().flex_none().text_size(l.px(Metric::TypeCaption)).text_color(l.text2()).child(summary))
        .child(div().flex_1())
        .child(close);
    let chips = found.chips.iter().enumerate().map(|(i, (r, on))| {
        let r = r.clone();
        div()
            .id(("refine", i))
            .flex_none()
            .h(px(24.))
            .flex()
            .items_center()
            .px(px(6.))
            .rounded(px(12.))
            .text_size(px(11.))
            .cursor_pointer()
            .when(*on, |d| d.bg(l.text()).text_color(l.panel()))
            .when(!*on, |d| d.bg(l.fill_passive()).border_1().border_color(l.stroke()).hover(|d| d.bg(l.hover())))
            .child(chip_label(&r))
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(RefineChip(r.clone()))))
    });
    // Чипы — целиком, в одну строку; не уместились — строка прокручивается вбок (колесом).
    let refine = (!found.chips.is_empty()).then(|| {
        div()
            .id("refine-chips")
            .h(px(36.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(4.))
            .pl(px(10.))
            .pr(px(6.))
            .overflow_x_scroll()
            .border_t_1()
            .border_color(l.border())
            .child(div().flex_none().mr(px(2.)).text_size(px(11.)).text_color(l.text2()).child(tr!("search-refine")))
            .children(chips)
    });
    div().flex_none().flex().flex_col().border_b_1().border_color(l.border()).child(line).children(refine)
}

/// Опись части (ADR 0005, `parts.rs`).
pub(crate) const PART: crate::part::Part =
    crate::part::Part { name: "search", texts: TEXTS, commands: COMMANDS, handlers, scenes: scenes::SCENES, ..crate::part::Part::NONE };

/// Обработчики команд части — на корне экрана почты (`Mailer::render`).
fn handlers(root: Div, cx: &mut Context<crate::Mailer>) -> Div {
    root.on_action(cx.listener(|this, _: &search::Focus, window, cx| {
        if this.section == crate::Section::Contacts {
            return this.contacts.update(cx, |c, cx| c.focus_search(window, cx));
        }
        if this.section == crate::Section::Calendar {
            return this.cal.update(cx, |c, cx| c.focus_search(window, cx));
        }
        if this.section == crate::Section::Tasks {
            return this.tasks.update(cx, |t, cx| t.focus_search(window, cx));
        }
        this.search.update(cx, |s, cx| s.focus(window, cx));
    }))
    // Поиск (S5-10): охват и уточнения командами — как переключатель и чипы.
    .on_action(cx.listener(|this, _: &search::All, _, cx| this.set_scope(crate::bar::Scope::All, cx)))
    .on_action(cx.listener(|this, _: &search::Folder, _, cx| this.set_scope(crate::bar::Scope::Folder, cx)))
    .on_action(cx.listener(|this, _: &search::Attachments, window, cx| this.refine(&postman_core::query::Refine::Attachment, window, cx)))
    .on_action(cx.listener(|this, _: &search::Unread, window, cx| this.refine(&postman_core::query::Refine::Unread, window, cx)))
    .on_action(cx.listener(|this, _: &search::Recent, window, cx| this.refine(&postman_core::query::Refine::Within(30), window, cx)))
    // По совпадениям в открытом письме (S5-14).
    .on_action(cx.listener(|this, _: &search::NextMatch, _, cx| this.reading.update(cx, |r, cx| r.step_hit(1, cx))))
    .on_action(cx.listener(|this, _: &search::PrevMatch, _, cx| this.reading.update(cx, |r, cx| r.step_hit(-1, cx))))
    .on_action(cx.listener(|this, _: &search::Clear, window, cx| {
        // Esc сначала закрывает контекстное меню письма (S5-08)…
        if this.close_menu(cx) || this.folders.update(cx, |f, cx| f.escape(cx)) || this.migration_escape(cx) {
            return;
        }
        // …затем историю уведомлений.
        if this.toasts.read(cx).is_history_open() {
            this.toasts.update(cx, |t, cx| t.toggle_history(cx));
            return;
        }
        // …затем меню «…» и панель выбора срока.
        if this.close_more(cx) || this.close_picker(cx) {
            return;
        }
        // …затем просмотр вложения (S5-07).
        if this.section == crate::Section::Mail && this.reading.update(cx, |r, cx| r.close_preview(cx)) {
            return;
        }
        // …затем панель фильтров (S5-03) — назад к папкам.
        if this.filters.read(cx).panel {
            return this.toggle_filter_panel(cx);
        }
        // …затем строку «Удалить навсегда?».
        if this.cancel_purge(cx) {
            return;
        }
        // …затем выделение писем (S5-08).
        if this.section == crate::Section::Mail && this.list.update(cx, |l, cx| l.clear_selection(cx)) {
            return;
        }
        // …затем запись клавиши и правку на месте в настройках, затем саму вкладку.
        if this.settings_escape(cx) {
            return;
        }
        if this.tabs.is_active(crate::tabs::TabKind::Settings) && this.close_settings(window, cx) {
            return;
        }
        // Панель чтения скрыта (Настройки → Чтение): письмо на месте списка — назад к списку.
        if this.section == crate::Section::Mail && this.reading_instead_of_list(cx) {
            this.reading.update(cx, |r, cx| r.clear(cx));
            return;
        }
        // В календаре — поиск над панелью события (S5-21).
        if this.section == crate::Section::Calendar && this.cal.update(cx, |c, cx| c.escape(window, cx)) {
            window.focus(&this.focus, cx);
            return;
        }
        // В контактах — строка «Удалить контакт?», форма, поиск контактов.
        if this.section == crate::Section::Contacts && this.contacts.update(cx, |c, cx| c.escape(window, cx)) {
            return;
        }
        if this.section == crate::Section::Tasks && this.tasks.update(cx, |t, cx| t.escape(window, cx)) {
            return;
        }
        this.search.update(cx, |s, cx| s.set_value("", window, cx));
        this.run_search("", cx);
        window.focus(&this.focus, cx);
    }))
}

/// Тексты части (`i18n/<язык>/<часть>.ftl`).
const TEXTS: &[pets_i18n::Resource] = &[("ru", include_str!("../i18n/ru/search.ftl")), ("en", include_str!("../i18n/en/search.ftl"))];

/// Поиск в командной строке (S5-10): охват «Везде | Текущая папка» и уточнения — как чипы «Уточнить»; по
/// совпадениям в открытом письме (S5-14).
// Имя — область действий (`search::…`), как в id команд: так обработчики читаются и их находит `docs/PARITY.md`.
#[allow(clippy::module_inception)]
pub(crate) mod search {
    gpui_kit::actions!(search, [Focus, Clear, All, Folder, Attachments, Unread, Recent, NextMatch, PrevMatch]);
}

/// Команды части (`commands.rs`: контексты и `cmd`).
const COMMANDS: &[pets_commands::Command] = {
    use crate::commands::{MAIL, cmd};
    &[
        cmd("search.focus", &["ctrl-e"], None),
        cmd("search.clear", &["escape"], None),
        // Не Outlook (там «Область поиска» и «Уточнить» — лентой): охват и уточнения поиска — чипами и в палитре.
        cmd("search.all", &[], MAIL),
        cmd("search.folder", &[], MAIL),
        cmd("search.attachments", &[], MAIL),
        cmd("search.unread", &[], MAIL),
        cmd("search.recent", &[], MAIL),
        // По совпадениям поиска в открытом письме (S5-14): F3 / Shift+F3, как «Найти далее» в браузерах и редакторах
        // (в classic Outlook F3 — ещё одна клавиша поиска, она у нас — Ctrl+E). В поле поиска тоже: набрал — F3.
        cmd("search.next_match", &["f3"], MAIL),
        cmd("search.prev_match", &["shift-f3"], MAIL),
    ]
};

/// Сцены `--screens` поиска (ADR 0005).
pub(crate) mod scenes {
    use gpui_kit::*;

    use crate::Mailer;
    use crate::screens::{self, Scene};

    pub(crate) const SCENES: &[Scene] = &[
        // Поиск «Везде» (S5-10): результаты группами по папкам, подсветка, чипы «Уточнить».
        Scene::new("search", search, search_ready),
        // Письмо из результатов поиска: подсветка в теме и тексте, строка «Совпадения».
        Scene::new("search-letter", letter, letter_ready).continues(),
        // То же письмо (S5-14): прокручено до низа, затем F3 по кругу до первого совпадения (в теме) — оно прокручено в
        // вид, «Совпадение 1 из N». Готово, только если переход прокрутил письмо наверх.
        Scene::new("search-next", next, next_ready).continues(),
    ];

    /// Запрос с результатами в демо-ящике (S5-10): последнее слово первой темы демо («Отчёт за квартал») — письма во
    /// «Входящих», «Архиве» и «Отправленных», в тексте писем Outlook — «итоги квартала».
    fn query() -> &'static str {
        postman_demo::SUBJECTS.first().and_then(|s| s.rsplit(' ').next()).unwrap_or("pipeline")
    }

    fn search(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        // Как «Везде» и ввод запроса с Enter.
        m.bar.update(cx, |b, _| b.scope = crate::bar::Scope::All);
        m.search.update(cx, |s, cx| s.set_value(query(), window, cx));
        m.run_search(query(), cx);
        Ok(())
    }

    fn search_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        let list = m.list.read(cx);
        list.found.as_ref().is_some_and(|f| !f.chips.is_empty()) && screens::rows_loaded(list)
    }

    fn letter(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        let Some(i) = screens::demo_row(m.list.read(cx), screens::outlook_letter) else {
            return Err(format!("no Outlook letter among the results of \"{}\"", query()));
        };
        m.list.update(cx, |l, cx| l.select(i, cx));
        Ok(())
    }

    fn letter_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        let list = m.list.read(cx);
        let selected = list.selected.and_then(|i| list.row(i)).map(|r| r.id.as_str());
        let marked = m.reading.read(cx).marks.is_some();
        marked && selected.is_some() && screens::reading(m, cx).is_some_and(|r| r.doc.is_some() && Some(r.row.id.as_str()) == selected)
    }

    /// Письмо прокручено вниз, затем F3 (`search.next_match`) по кругу — к первому совпадению.
    fn next(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.reading.update(cx, |r, cx| {
            let bottom = r.scroll.max_offset().y;
            if bottom <= px(0.) {
                return Err("the letter fits the reading pane: nothing to scroll".to_string());
            }
            r.scroll.set_offset(point(px(0.), -bottom));
            for _ in 0..r.hits.get() {
                r.step_hit(1, cx);
            }
            Ok(())
        })
    }

    fn next_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        let r = m.reading.read(cx);
        r.hits.get() > 0 && r.hit == 0 && r.scroll.offset().y.abs() < px(1.)
    }
}
