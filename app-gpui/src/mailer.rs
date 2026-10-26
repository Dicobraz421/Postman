//! Экран почты: связывает части, держит действия (хоткеи), поиск, календарь, тосты и замер.

use std::time::Instant;

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::Level as Toast;

use crate::notices::Notices;
use postman_cache::Cache;
use postman_core::bench::{self, Bench, Step};
use postman_worker::Level;

use crate::backend::Backend;
use crate::bar::*;
use crate::folders::*;
use crate::list::*;
use crate::reading::*;
use crate::*;

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Section {
    Mail,
    Calendar,
    Contacts,
    Tasks,
}

impl Section {
    /// Команда рейла этого модуля.
    pub(crate) fn command(self) -> &'static str {
        match self {
            Section::Mail => "view.mail",
            Section::Calendar => "view.calendar",
            Section::Contacts => "view.contacts",
            Section::Tasks => "view.tasks",
        }
    }
}

// --- экран ---

/// Экран почты: связывает части, держит хоткеи, поиск, календарь и строку состояния.
/// Части — отдельные кэшируемые view; заново строится та, что получила `notify`.
pub(crate) struct Mailer {
    pub(crate) cache: Cache,
    pub(crate) focus: FocusHandle,
    pub(crate) list: Entity<MailList>,
    pub(crate) folders: Entity<Folders>,
    /// Фильтры (S5-03): общая модель панели папок и списка.
    pub(crate) filters: Entity<crate::filters::FilterModel>,
    pub(crate) reading: Entity<ReadingPane>,
    pub(crate) bar: Entity<CommandBar>,
    pub(crate) search: Entity<InputState>,
    pub(crate) toasts: Entity<Notices>,
    /// Сводка выделения на месте письма (S5-08, `bulk.rs`).
    pub(crate) summary: Entity<crate::bulk::SummaryView>,
    /// Контекстное меню письма (S5-08, `menu.rs`); после закрытия — фокус обратно экрану.
    pub(crate) menu: Option<crate::menu::Menu>,
    /// Меню папки (S5-05б, `folder_menu.rs`): «Вид беседы ▸».
    pub(crate) folder_menu: Option<crate::folder_menu::FolderMenu>,
    pub(crate) refocus: bool,
    /// Связи с сервером нет (S5-08, `states.rs`): строка состояния, метка у аккаунта, полоса над списком.
    pub(crate) offline: crate::states::Offline,
    /// Таймер обратного отсчёта «повтор через N с» стоит.
    pub(crate) offline_ticking: bool,
    /// Сервер или демо (S3-04 подключает живой ящик).
    pub(crate) backend: Backend,
    /// Живой ящик: статус синхронизации, строка входа, поиск на сервере (`live.rs`).
    pub(crate) live: crate::live::Live,
    /// Письма: вкладки и ответ в области чтения (S5-06б).
    pub(crate) compose: crate::compose::Composers,
    /// Окно экрана: полям ввода нового письма нужно окно, а письмо открывается и из событий синхронизации.
    pub(crate) window: AnyWindowHandle,
    /// Ответ, пересылка, удаление (S3-02).
    pub(crate) actions: crate::actions::Actions,
    /// Уровень песочницы последнего разбора; о слабой песочнице — один тост за запуск.
    pub(crate) sandbox: Option<Level>,
    /// Последнее нажатие и сработавшее действие — проверка хоткеев в любой раскладке (S1-01).
    pub(crate) last_key: String,
    pub(crate) bench: Option<Bench>,
    /// `--screens`: снимки экранов без человека (S2-05).
    pub(crate) screens: Option<screens::Driver>,
    pub(crate) section: Section,
    /// KeyTips (S5-18): удержание Ctrl и пробуждение через 0,4 с (только пока Ctrl нажат).
    pub(crate) keytips: crate::keytips::Timer,
    pub(crate) keytips_task: Option<Task<()>>,
    /// В области чтения — приглашение без ответа: у корня контекст `Invite` (S5-17).
    pub(crate) invite_waiting: bool,
    /// Раздел главной вкладки, пока открыта вкладка почты (в ней раздел — всегда «Почта», S5-16).
    pub(crate) main_section: Option<Section>,
    /// Календарь — своя кэшируемая view (S3-05); окна новой встречи — `meeting.rs`.
    pub(crate) cal: Entity<calendar::CalendarView>,
    /// Командные строки календаря и контактов (S5-11) — на месте строки почты в своём разделе.
    pub(crate) cal_bar: Entity<calendar::CalendarBar>,
    pub(crate) contacts_bar: Entity<crate::contacts::ContactsBar>,
    /// Вкладки новых встреч (S5-11).
    pub(crate) meetings: crate::meeting::Meetings,
    /// Автоответ, квота (S4-05): панель настроек, полоса автоответа, строка состояния (`settings.rs`).
    pub(crate) settings: crate::settings::ServerSettings,
    /// Контакты и справочник — своя кэшируемая view (S4-02).
    pub(crate) contacts: Entity<crate::contacts::ContactsView>,
    /// Задачи (S6-05) — своя кэшируемая view и командная строка раздела.
    pub(crate) tasks: Entity<crate::tasks::TasksView>,
    pub(crate) tasks_bar: Entity<crate::tasks::TasksBar>,
    /// «Отправить позже» и окно «Отменить» отправки (S5-06, `send_later.rs`).
    pub(crate) send_later: crate::send_later::SendLater,
    /// Сроки, напоминания, «Ждём ответа», «Исчезнет» (S5-04, `lifecycle.rs`).
    pub(crate) lifecycle: crate::lifecycle::Lifecycle,
    /// Вкладки в заголовке окна (S5-01).
    pub(crate) tabs: crate::tabs::Tabs,
    /// Меню «…» командной строки.
    pub(crate) more: crate::chrome::MoreMenu,
    /// Поиск в командной строке (S5-10, `search.rs`).
    pub(crate) find: crate::search::Find,
    /// Палитра команд Ctrl+K (S5-10, `palette.rs`) и недавние её строки (новые первыми).
    pub(crate) palette: Option<crate::palette::Open>,
    pub(crate) recent: Vec<String>,
    pub(crate) _subscriptions: Vec<Subscription>,
}

impl Mailer {
    pub(crate) fn new(cache: Cache, bench: Option<Bench>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("frame-search")));
        let keylog = std::env::var_os("POSTMAN_KEYLOG").is_some();
        let on_search = cx.subscribe_in(&search, window, move |this: &mut Self, state, ev: &InputEvent, _window, cx| match ev {
            InputEvent::PressEnter { .. } => {
                let q = state.read(cx).value().to_string();
                this.run_search(&q, cx);
            }
            // Поиск по мере набора — после паузы (S5-10).
            InputEvent::Change => {
                let q = state.read(cx).value().to_string();
                if keylog {
                    println!("search \"{q}\"");
                }
                this.search_typed(q, cx);
            }
            InputEvent::Focus | InputEvent::Blur => {
                let on = matches!(ev, InputEvent::Focus);
                this.bar.update(cx, |b, cx| {
                    b.focused = on;
                    cx.notify();
                });
            }
        });
        let mailboxes = cache.mailboxes().unwrap_or_default();
        let accounts = cache.accounts().unwrap_or_default();
        let inbox = crate::live::inbox(&mailboxes);
        let filters = cx.new(|_| crate::filters::FilterModel::new());
        let list = cx.new(|cx| {
            let mut l = MailList::new(cache.clone(), filters.clone(), cx);
            l.set_rights(&mailboxes);
            if let Some(id) = inbox.clone() {
                l.show_mailbox(id, cx);
            }
            l
        });
        let folders = cx.new(|cx| Folders::new(mailboxes, accounts, inbox.clone()).watch(filters.clone(), cx));
        let reading = cx.new(|_| {
            let mut r = ReadingPane::new(cache.clone());
            r.folder = inbox.clone();
            r
        });
        let bar = cx.new(|_| CommandBar::new(search.clone()));
        let toasts = cx.new(|_| Notices::new());
        let summary = cx.new(|_| crate::bulk::SummaryView::new());
        let cal = cx.new(|cx| calendar::CalendarView::new(window, cx));
        let contacts = cx.new(|cx| crate::contacts::ContactsView::new(window, cx));
        let cal_bar = cx.new(|cx| calendar::CalendarBar::new(cal.clone(), cx));
        let contacts_bar = cx.new(|cx| crate::contacts::ContactsBar::new(contacts.clone(), cx));
        let tasks = cx.new(|cx| crate::tasks::TasksView::new(cache.clone(), window, cx));
        let tasks_bar = cx.new(|cx| crate::tasks::TasksBar::new(tasks.clone(), cx));
        reading.update(cx, |r, _| r.tasks = Some(tasks.downgrade()));
        // «Ждём» в почте — ещё и письма моих невыполненных поручений (S6-11); дальше — `TasksAction::Waiting`.
        let waiting = tasks.read(cx).waiting_keywords();
        list.update(cx, |l, cx| l.set_waiting(waiting, cx));
        let subscriptions = vec![
            cx.subscribe(&tasks, |this, _, action: &crate::tasks::TasksAction, cx| this.on_tasks(action, cx)),
            cx.subscribe(&contacts, |this, _, action: &crate::contacts::ContactsAction, cx| this.on_contacts(action, cx)),
            cx.subscribe(&cal, |this, _, action: &calendar::CalendarAction, cx| this.on_calendar(action, cx)),
            on_search,
            cx.subscribe(&bar, |this, bar, ev: &BarEvent, cx| match ev {
                BarEvent::More => this.toggle_more(cx),
                BarEvent::Scope => {
                    // Охват сменился посреди поиска — искать заново.
                    let q = bar.read(cx).search.read(cx).value().to_string();
                    if !q.trim().is_empty() {
                        this.run_search(&q, cx);
                    }
                }
            }),
            Self::observe_keys(cx),
            cx.subscribe(&list, |this, _, OpenRow(row), cx| this.open_letter(row.clone(), cx)),
            // Чип «Уточнить» (S5-10): условие — в строку поиска.
            cx.subscribe_in(&list, window, |this, _, crate::search::RefineChip(r), window, cx| this.refine(r, window, cx)),
            cx.subscribe(&list, |this, _, NeedMore(mailbox), cx| this.load_more(mailbox.clone(), cx)),
            cx.subscribe(&list, |this, _, _: &ReadOnly, cx| {
                this.toasts.update(cx, |t, cx| t.push(Toast::Info, tr!("shared-read-only-flags"), cx));
            }),
            cx.subscribe(&list, |this, _, n: &LabelNotice, cx| {
                let text = match n {
                    LabelNotice::NoCategory(key) => tr!("labels-no-category", key = *key),
                    LabelNotice::ReadOnly => tr!("labels-read-only"),
                };
                this.toasts.update(cx, |t, cx| t.push(Toast::Info, text, cx));
            }),
            crate::actions::watch_list(&list, cx),
            // Сводка выделения: кнопки — как у панели; «В папку» и «Категория» — меню у кнопки.
            cx.subscribe(&summary, |this, _, a: &crate::bulk::SummaryAsk, cx| this.on_summary(a, cx)),
            cx.subscribe(&folders, |this, _, ev: &FolderEvent, cx| match ev {
                FolderEvent::Open(id) => {
                    this.show_mail_tab(cx);
                    this.open_mailbox(id.clone(), cx)
                }
                FolderEvent::Menu { id, at } => this.open_folder_menu(id.clone(), *at, cx),
            }),
            cx.subscribe(&reading, |this, _, ev: &ReadingEvent, cx| match ev {
                ReadingEvent::Sandbox(level) => {
                    if *level != Level::Full && this.sandbox.is_none() {
                        let text = tr!("sandbox-weak", level = sandbox_label(*level));
                        this.toasts.update(cx, |t, cx| t.push(Toast::Warning, text, cx));
                    }
                    this.sandbox = Some(*level);
                    cx.notify();
                }
                ReadingEvent::Error(text) => this.toasts.update(cx, |t, cx| t.push(Toast::Error, text.clone(), cx)),
                ReadingEvent::Toast(level, text) => this.toasts.update(cx, |t, cx| t.push(*level, text.clone(), cx)),
                ReadingEvent::NeedsServer => this.needs_server(cx),
                ReadingEvent::Open(row) => this.open_letter(row.clone(), cx),
                ReadingEvent::Unsend(s) => this.unsend_scheduled(s.clone(), cx),
                ReadingEvent::Respond(row, mode) => this.respond_to(row.clone(), *mode, cx),
                ReadingEvent::TaskMail(inc) => this.tasks.update(cx, |t, cx| t.receive_mail(inc, cx)),
                ReadingEvent::TaskAction { uid, action } => {
                    let found = this.tasks.update(cx, |t, cx| t.mail_action(uid, *action, cx));
                    if found && action.is_none() {
                        this.set_section(Section::Tasks, cx);
                    }
                    this.reading.update(cx, |_, cx| cx.notify());
                }
                ReadingEvent::TrashAnswered(row) => this.trash_answered(row.clone(), cx),
                ReadingEvent::Sync(req) => {
                    if !this.backend.send(req.command()) {
                        this.needs_server(cx);
                        this.reading.update(cx, |r, cx| r.not_sent(req, cx));
                    }
                }
            }),
            // Плашки KeyTips и клавиши кнопок (S5-18): части с кнопками — заново.
            cx.observe_global::<crate::keytips::KeyTips>(|this: &mut Self, cx| this.keytips_changed(cx)),
            // Значок уведомлений в строке состояния.
            cx.observe(&toasts, |_, _, cx| cx.notify()),
            // Настройки (S5-09): «Панель чтения» меняет раскладку; вид беседы (S5-05б) — область чтения.
            crate::prefs::observe(cx),
            crate::prefs::observe_with(cx, |this: &mut Self, cx| this.reading.update(cx, |r, cx| r.view_changed(cx))),
            // Ответ в области чтения — над своим письмом: открыли другое — ответ уходит во вкладку или закрывается.
            // Скрытая панель чтения (S5-09) — письмо на месте списка: раскладка меняется вместе с письмом.
            cx.observe(&reading, |this, _, cx| {
                this.inline_follows_letter(cx);
                // Контекст `Invite` корня (клавиши A / T / N) — по области чтения.
                let waiting = invite_waiting(this.reading.read(cx));
                if waiting != this.invite_waiting {
                    this.invite_waiting = waiting;
                    cx.notify();
                }
                if crate::prefs::get(cx).reading.pane == postman_core::prefs::Pane::Hidden {
                    cx.notify();
                }
            }),
            // Фильтры «Все папки» живого ящика — ещё и на сервер (S5-03).
            cx.subscribe(&filters, |this, _, _: &crate::filters::Changed, cx| this.filters_changed(cx)),
            // Закладки быстрых фильтров рисует экран почты; какие — по настройкам.
            cx.observe(&filters, |_, _, cx| cx.notify()),
            crate::prefs::observe_with(cx, |this: &mut Self, cx| this.prune_quick(cx)),
        ];
        let mut subscriptions = subscriptions;
        subscriptions.extend(Self::watch_bulk(&list, cx));
        // Кнопка «Взять в работу» командной строки (S5-28) — по открытой папке и письму.
        subscriptions.push(cx.observe(&list, |this, _, cx| this.sync_take_button(cx)));
        subscriptions.push(cx.observe(&reading, |this, _, cx| this.sync_take_button(cx)));
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        // Планировщик жизненного цикла — после создания экрана (снимки и замер ставят себя сразу после `new`).
        cx.spawn(async move |this, cx| {
            let _ = this.update(cx, |m: &mut Mailer, cx| m.lifecycle_pass(false, cx));
        })
        .detach();
        // Кто я и имена коллег — строке списка и шапке письма общего ящика (S5-28, `shared.rs`).
        let compose = crate::compose::Composers::default();
        cx.set_global(crate::shared::People { book: compose.book.clone(), me: Some(postman_demo::ME.to_owned()) });
        Self {
            cache,
            focus,
            list,
            folders,
            filters,
            reading,
            bar,
            search,
            toasts,
            summary,
            menu: None,
            folder_menu: None,
            refocus: false,
            offline: Default::default(),
            offline_ticking: false,
            backend: Backend::Demo,
            live: Default::default(),
            compose,
            window: window.window_handle(),
            actions: Default::default(),
            sandbox: None,
            last_key: String::new(),
            bench,
            screens: None,
            section: Section::Mail,
            main_section: None,
            invite_waiting: false,
            keytips: Default::default(),
            keytips_task: None,
            cal,
            cal_bar,
            contacts_bar,
            meetings: Default::default(),
            settings: Default::default(),
            contacts,
            tasks,
            tasks_bar,
            send_later: Default::default(),
            lifecycle: Default::default(),
            tabs: Default::default(),
            more: Default::default(),
            find: Default::default(),
            palette: None,
            recent: Vec::new(),
            _subscriptions: subscriptions,
        }
    }

    /// «Ctrl-e (у) → FocusSearch» в строке состояния; `POSTMAN_KEYLOG=1` — то же строкой `key …` в stdout.
    pub(crate) fn observe_keys(cx: &mut Context<Self>) -> Subscription {
        let to_stdout = std::env::var_os("POSTMAN_KEYLOG").is_some();
        cx.observe_keystrokes(move |this, ev, _window, cx| {
            let k = &ev.keystroke;
            let action = ev.action.as_ref().map_or("—", |a| a.name());
            this.last_key = format!("{} ({}) → {action}", k.unparse(), k.key_char.as_deref().unwrap_or(""));
            // Нажата клавиша — KeyTips не нужны (S5-18).
            this.keytips_key(cx);
            if to_stdout {
                println!("key {}", this.last_key);
            }
            cx.notify();
        })
    }

    /// Событие синхронизации: каждая часть берёт своё (`on_sync`); общее (папки, список, статус, ошибки,
    /// поиск) — `live.rs`.
    pub(crate) fn on_sync(&mut self, ev: &postman_core::sync::Event, cx: &mut Context<Self>) {
        if let postman_core::sync::Event::Me { address, .. } = ev {
            crate::shared::set_me(Some(address.email.clone()), cx);
        }
        if let postman_core::sync::Event::Features(f) = ev {
            self.set_features(*f, cx);
        }
        self.reading.update(cx, |r, cx| r.on_sync(ev, cx));
        self.list.update(cx, |l, cx| l.on_sync(ev, cx));
        let sent = self.compose.on_sync(ev, cx);
        self.on_send_sync(ev, sent, cx);
        self.cal.update(cx, |c, cx| c.on_sync(ev, cx));
        self.contacts.update(cx, |c, cx| c.on_sync(ev, cx));
        if let postman_core::sync::Event::Tasks(reply) = ev {
            self.tasks.update(cx, |t, cx| t.on_reply(reply, cx));
        }
        if let postman_core::sync::Event::Me { address, .. } = ev {
            let (me, book) = (address.clone(), self.compose.book.clone());
            self.tasks.update(cx, |t, cx| {
                t.set_book(book);
                t.set_me(me, cx);
            });
        }
        let created = self.meetings.on_sync(ev, cx);
        self.meetings_created(created, cx);
        self.on_settings_sync(ev, cx);
        self.on_lifecycle_sync(ev, cx);
        match ev {
            postman_core::sync::Event::Connection(c) => self.on_connection(*c, cx),
            postman_core::sync::Event::Reported { error, .. } => self.on_reported(error.as_deref(), cx),
            postman_core::sync::Event::Delegated { error, uid, scheduled, .. } => {
                if let Some(e) = error {
                    let text = tr!("assign-failed", error = e.clone());
                    self.toasts.update(cx, |t, cx| t.push(Toast::Error, text, cx));
                }
                self.tasks.update(cx, |t, cx| t.on_delegated(uid, scheduled, cx));
            }
            postman_core::sync::Event::Withheld { too_late, error, .. } => {
                let text = match (too_late, error) {
                    (true, _) => Some((Toast::Info, tr!("assign-withhold-late"))),
                    (false, Some(e)) => Some((Toast::Error, tr!("assign-failed", error = e.clone()))),
                    (false, None) => None,
                };
                if let Some((level, text)) = text {
                    self.toasts.update(cx, |t, cx| t.push(level, text, cx));
                }
            }
            _ => {}
        }
    }

    /// Действию нужен сервер, а его нет (демо): сказать тостом.
    pub(crate) fn needs_server(&mut self, cx: &mut Context<Self>) {
        self.toasts.update(cx, |t, cx| t.push(Toast::Info, tr!("needs-server"), cx));
    }

    pub(crate) fn open_mailbox(&mut self, id: String, cx: &mut Context<Self>) {
        self.drop_search(cx);
        // Общий ящик (S5-28): имена тех, кто взял письма в работу, — из адресной книги.
        if postman_cache::account_of(&id).is_some() {
            self.load_address_book(cx);
        }
        // Демо (S7-09б): функции — учётки открытой папки, как ответ её сервера.
        if !self.backend.is_live() {
            self.set_features(crate::features::of_demo_folder(&id), cx);
        }
        self.folders.update(cx, |f, cx| {
            f.current = Some(id.clone());
            f.results = None;
            cx.notify();
        });
        self.list.update(cx, |l, cx| l.show_mailbox(id.clone(), cx));
        self.sync_total(cx);
        self.reading.update(cx, |r, cx| {
            r.folder = Some(id);
            r.clear(cx)
        });
    }

    /// Папка поиска по охвату командной строки: «Текущая папка» — открытая, «Везде» — все (`None`).
    pub(crate) fn search_mailbox(&self, cx: &App) -> Option<String> {
        match self.bar.read(cx).scope {
            crate::bar::Scope::All => None,
            crate::bar::Scope::Folder => self.list.read(cx).mailbox.clone(),
        }
    }

    /// Что умеет сервер (S7-03, `features.rs`): открыт модуль, которого у сервера нет, — назад в «Почту».
    pub(crate) fn set_features(&mut self, features: postman_core::model::Features, cx: &mut Context<Self>) {
        crate::features::set(features, cx);
        // Закладка быстрого фильтра пропала (нет ключевых слов) — её фильтр снимается.
        self.prune_quick(cx);
        if !crate::features::available(self.section.command(), features) {
            self.set_section(Section::Mail, cx);
        }
        cx.notify();
    }

    /// «Нет у сервера этой учётной записи» — клавиша команды, которой у сервера нет (`features::Unavailable`).
    pub(crate) fn unavailable(&mut self, cx: &mut Context<Self>) {
        self.toasts.update(cx, |t, cx| t.push(Toast::Info, tr!("feature-unavailable"), cx));
    }

    pub(crate) fn set_section(&mut self, section: Section, cx: &mut Context<Self>) {
        // Модуль рейла — во вкладке «Почта» (вкладка настроек остаётся открытой); «Почта» во вкладке почты (S5-16) —
        // там же.
        if section == Section::Mail {
            self.show_mail_tab(cx);
        } else {
            self.tabs.activate_kind(crate::tabs::TabKind::Main);
            self.sync_mail_tab(cx);
        }
        if section == Section::Calendar {
            // Цвета событий — по таблице категорий своего аккаунта.
            let table = self.list.read(cx).labels.for_account(None).cloned().unwrap_or_else(crate::labels::default_table);
            self.cal.update(cx, |c, cx| c.set_categories(table, cx));
        }
        if section == Section::Calendar && self.section != section {
            self.cal.update(cx, |c, cx| c.refresh(cx));
        }
        if section == Section::Contacts && self.section != section {
            self.contacts.update(cx, |c, cx| c.refresh(cx));
            // «Часто пишу» (S5-25) — из адресной книги переписки (собирается в фоне один раз).
            self.load_address_book(cx);
            self.contacts_frequent(cx);
        }
        if section == Section::Tasks {
            // Категории — та же таблица, что у почты; письма со сроком — заново из кэша.
            let table = self.list.read(cx).labels.for_account(None).cloned().unwrap_or_else(crate::labels::default_table);
            // «Время в календаре»: своя занятость сегодня — из недели календаря.
            let busy = self.cal.read(cx).busy_today();
            self.tasks.update(cx, |t, cx| {
                t.set_busy(busy, cx);
                t.set_categories(table, cx);
                if self.section != section {
                    t.refresh(cx);
                }
            });
        }
        self.section = section;
        cx.notify();
    }

    /// Сдвиг выделения; `delta` считается по состоянию списка (страница, число писем).
    pub(crate) fn step(&mut self, delta: impl Fn(&MailList) -> isize, cx: &mut Context<Self>) {
        self.list.update(cx, |l, cx| {
            let d = delta(l);
            l.step(d, cx);
        });
    }
}

impl Render for Mailer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let started = Instant::now();
        // Сервер отклонил сохранённый вход: строка входа (ей нужно окно — здесь оно есть).
        if let Some(prefill) = self.live.reauth.take() {
            cx.defer_in(window, move |this, window, cx| this.show_reauth(prefill, window, cx));
        }
        // Новые встречи — вкладками (полям ввода нужно окно).
        if !self.meetings.pending.is_empty() {
            cx.defer_in(window, |this, window, cx| this.create_meetings(window, cx));
        }
        // Вкладку сменили мимо `sync_mail_tab` (закрыли письмо или встречу) — части почты по активной вкладке.
        self.sync_mail_tab(cx);
        self.drive_bench(window, cx);
        screens::drive(self, window, cx);
        if std::mem::take(&mut self.refocus) {
            window.focus(&self.focus, cx);
        }
        let l = crate::roles::look(cx);
        let menu = self.context_menu(window, &l, cx);
        let folder_menu = self.folder_menu(window, &l, cx);
        let picker = self.lifecycle_overlay(&l, cx);
        // Вкладка «Настройки» (S5-09) — на месте рейла и модуля.
        let settings_tab = self.tabs.is_active(crate::tabs::TabKind::Settings);
        let settings: Option<AnyView> = self.settings.view.clone().filter(|_| settings_tab).map(Into::into);
        // Вкладка письма (S5-06б): письмо на всю ширину, без командной строки и рейла (макет «Новое письмо»).
        let letter = match self.tabs.active().kind {
            crate::tabs::TabKind::Compose(id) => self.compose.get(id).cloned(),
            _ => None,
        };
        // Вкладка встречи (S5-11) — так же: карточка на всю ширину, своя строка «Отправить» (макет «Новая встреча»).
        let active = self.tabs.active();
        let meeting =
            (active.kind == crate::tabs::TabKind::Meeting).then(|| self.meetings.view(active.id).map(|v| AnyView::from(v.clone()))).flatten();
        let free = settings.is_none();
        let first_run = self.section == Section::Mail && self.live.signin.is_some();
        // Командная строка и полоса автоответа — у почты; во вкладках «Настройки», письма и при первом запуске их нет.
        let bars = free && !first_run && letter.is_none() && meeting.is_none();
        // Контекст раздела: клавиши писем (j/k, Delete, Ctrl+R…) — только в «Почте» без открытых настроек (`commands.rs`).
        let context = match self.section {
            _ if letter.is_some() => "Mailer ComposeView",
            _ if meeting.is_some() => "Mailer MeetingTab",
            _ if !free => "Mailer SettingsView",
            // Приглашение без ответа в области чтения: A / T / N схемы «Одиночные клавиши» — ответ (`commands::INVITE`).
            Section::Mail if self.invite_waiting => "Mailer MailView Invite",
            Section::Mail => "Mailer MailView",
            Section::Calendar => "Mailer CalendarView",
            Section::Contacts => "Mailer ContactsView",
            Section::Tasks => "Mailer TasksView",
        };
        let root = crate::parts::PARTS
            .iter()
            .fold(div(), |root, p| (p.handlers)(root, cx))
            .key_context(context)
            .track_focus(&self.focus)
            // KeyTips (S5-18): удержание одного Ctrl.
            .on_modifiers_changed(cx.listener(|this, ev: &ModifiersChangedEvent, _, cx| this.keytips_modifiers(ev.modifiers, cx)))
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(l.base())
            .text_color(l.text())
            // Размер шрифта (S5-30, «Внешний вид»): список, письма и панели.
            .text_size(px(f32::from(crate::prefs::with(cx, |p| p.look.font_px()))))
            .child(self.titlebar(&l, window, cx))
            .when(bars, |d| d.child(self.section_bar(free)))
            .when_some(self.settings.bar.clone().filter(|_| bars), |d, bar| {
                d.child(bar.cached(StyleRefinement::default().w_full().h(px(crate::vacation::BAR_HEIGHT)).flex_none()))
            })
            .map(|d| match (letter, meeting) {
                (Some(view), _) => d.child(div().flex().flex_1().min_h_0().px(px(8.)).child(view.cached(StyleRefinement::default().size_full()))),
                (None, Some(view)) => d.child(
                    div().flex().flex_1().min_h_0().px(px(8.)).child(card(&l).flex_1().child(view.cached(StyleRefinement::default().size_full()))),
                ),
                // Закладки быстрых фильтров — поверх карточки списка (S5-03).
                // Панель выбора срока (S5-04) — там же, у шапки письма.
                (None, None) => d.child(self.body(&l, settings, free, cx).relative().children(self.quick_overlay(&l, cx)).children(picker)),
            })
            .child(self.status_bar(&l, cx))
            .children(self.more_menu(&l, cx))
            // Палитра команд (S5-10) — поверх строки команд, под заголовком окна.
            .children(self.palette_overlay(window, cx))
            // Окно «Отменить» отправки — внизу, над строкой состояния; над ним — напоминания (S5-04).
            .children(self.undo_card(cx))
            .children(self.reminder_cards(cx))
            // Контекстное меню письма (S5-08) и папки (S5-05б) — над всем, кроме тостов.
            .children(menu)
            .children(folder_menu)
            // Образцы кнопок — только снимок `buttons` (S5-19).
            .children(self.screens.as_ref().filter(|d| d.gallery()).map(|_| crate::buttons::gallery(&l)))
            // Тосты — поверх всего, последним ребёнком.
            .child(self.toasts.clone());
        if let Some(b) = &mut self.bench {
            b.ui_time(started.elapsed());
        }
        root
    }
}

// --- раскладка (DESIGN «Главное окно»): рейл | папки | список | чтение ---

/// Ширина панели папок и карточки списка (макет главного окна).
pub(crate) const W_FOLDERS: f32 = 236.0;
pub(crate) const W_LIST: f32 = 452.0;
/// Доля высоты области чтения под ответом (остальное — письмо, на которое отвечают).
const INLINE_SHARE: f32 = 0.6;

/// Карточка на фоне каркаса: скругление сверху, рамка без нижней стороны.
pub(crate) fn card(l: &crate::roles::Look) -> Div {
    div()
        .h_full()
        .min_w_0()
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(l.panel())
        .rounded_t(l.px(pets_ui::theme::Metric::RadiusOverlay))
        .border_1()
        .border_b_0()
        .border_color(l.c(pets_ui::theme::Role::StrokeCard))
}

impl Mailer {
    /// Панель чтения скрыта и письмо открыто: оно — на месте списка (Esc — назад).
    pub(crate) fn reading_instead_of_list(&self, cx: &App) -> bool {
        crate::prefs::get(cx).reading.pane == postman_core::prefs::Pane::Hidden && self.reading.read(cx).reading.is_some()
    }

    fn body(&self, l: &crate::roles::Look, settings: Option<AnyView>, free: bool, cx: &App) -> Div {
        use postman_core::prefs::Pane;
        let full = || StyleRefinement::default().size_full();
        let mail = free && self.section == Section::Mail;
        // Вкладка «Настройки» — на всю ширину: своя навигация вместо рейла (макет «Настройки»).
        if let Some(p) = settings {
            return div().flex().flex_1().min_h_0().px(px(8.)).child(p.cached(full()));
        }
        // Нет учётной записи (первый запуск) — добавление аккаунта на месте всего экрана почты.
        if mail && let Some(s) = self.live.signin.clone() {
            return div().flex().flex_1().min_h_0().px(px(8.)).child(card(l).flex_1().child(s));
        }
        let mut body = div().flex().flex_1().min_h_0().pr(px(8.)).child(self.rail(l, cx));
        if mail {
            body = body.child(self.folders.clone().cached(StyleRefinement::default().w(px(W_FOLDERS)).h_full().flex_none()));
            let list = || self.list.clone().cached(full());
            // Ответ — в области чтения над письмом (S5-06б): своя кэшируемая view сверху карточки.
            // Выделено несколько писем — сводка на месте письма (S5-08).
            let picked = self.list.read(cx).selection.len() > 1;
            let reading = || match self.compose.inline_view() {
                _ if picked => card(l).flex_1().child(self.summary.clone().cached(full())),
                Some(reply) => card(l)
                    .flex_1()
                    .child(reply.clone().cached(StyleRefinement::default().w_full().h(relative(INLINE_SHARE)).flex_none()))
                    .child(self.reading.clone().cached(StyleRefinement::default().w_full().flex_1().min_h_0())),
                None => card(l).flex_1().child(self.reading.clone().cached(full())),
            };
            // Панель чтения — справа, снизу или скрыта (Настройки → Чтение).
            return match crate::prefs::get(cx).reading.pane {
                Pane::Right => body.child(card(l).w(px(W_LIST)).flex_none().mr(px(8.)).child(list())).child(reading()),
                Pane::Bottom => body.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(
                            card(l).h(relative(0.45)).flex_none().rounded_b(l.px(pets_ui::theme::Metric::RadiusOverlay)).border_b_1().child(list()),
                        )
                        .child(reading()),
                ),
                Pane::Hidden if self.reading_instead_of_list(cx) => body.child(reading()),
                Pane::Hidden => body.child(card(l).flex_1().child(list())),
            };
        }
        // Календарь и контакты рисуют свои панели и карточки сами (макеты S5-11).
        match self.section {
            Section::Calendar => body.child(div().flex_1().min_w_0().h_full().child(self.cal.clone().cached(full()))),
            Section::Contacts => body.child(div().flex_1().min_w_0().h_full().child(self.contacts.clone().cached(full()))),
            Section::Tasks => body.child(div().flex_1().min_w_0().h_full().child(self.tasks.clone().cached(full()))),
            Section::Mail => body,
        }
    }

    /// Командная строка: своя у календаря и контактов (вкладка «Почта»), иначе — строка почты.
    fn section_bar(&self, free: bool) -> AnyElement {
        let style = StyleRefinement::default().w_full().h(px(BAR_HEIGHT)).flex_none();
        match self.section {
            Section::Calendar if free => self.cal_bar.clone().cached(style).into_any_element(),
            Section::Contacts if free => self.contacts_bar.clone().cached(style).into_any_element(),
            Section::Tasks if free => self.tasks_bar.clone().cached(style).into_any_element(),
            _ => self.bar.clone().cached(style).into_any_element(),
        }
    }

    /// Клавиши A / T / N: ответ на приглашение открытого письма (в «Почте»).
    pub(crate) fn invite_key(&mut self, status: postman_calendar::Partstat, cx: &mut Context<Self>) {
        if self.section == Section::Mail {
            self.reading.update(cx, |r, cx| r.invite_hotkey(status, cx));
        }
    }
}

/// Открыто приглашение, на которое можно ответить (как `ReadingPane::invite_hotkey`).
fn invite_waiting(r: &ReadingPane) -> bool {
    let me = r.me.as_deref();
    r.reading.as_ref().and_then(|x| x.invite.as_ref()).is_some_and(|c| c.sending.is_none() && c.can_answer(me))
}

// --- замер: общий сценарий из postman_core::bench ---

impl Mailer {
    pub(crate) fn drive_bench(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.list.read(cx).count;
        let Some(b) = &mut self.bench else { return };
        match b.frame(count) {
            Step::Wait => {}
            Step::Select(i) => self.list.update(cx, |l, cx| l.select(i, cx)),
            Step::Scroll(i) => self.list.update(cx, |l, cx| l.scroll_to_top(i, cx)),
            Step::Resize(w, h) => window.resize(size(px(w), px(h))),
            Step::Done(report) => {
                // Дальше — простой: кадры больше не запрашиваем, процесс завершит замер.
                bench::finish(format!("bench ui=gpui {report}"));
                self.bench = None;
                // Простой с фоновыми частями (S5-14): планировщик жизненного цикла и обратный отсчёт «нет связи» —
                // проверка, что их таймеры не крутятся (`idle_cpu_pct`). По умолчанию простой — без них (повторяемо).
                if std::env::var_os(bench::IDLE_LIFECYCLE_VAR).is_some() {
                    self.lifecycle_pass(false, cx);
                }
                if std::env::var_os(bench::IDLE_OFFLINE_VAR).is_some() {
                    self.on_connection(postman_core::link::Connection::Offline { retry_in: postman_core::link::MAX_RETRY, queued: 0 }, cx);
                }
                let section = match std::env::var(bench::IDLE_SECTION_VAR).as_deref() {
                    Ok("calendar") => Some(Section::Calendar),
                    Ok("contacts") => Some(Section::Contacts),
                    Ok("tasks") => Some(Section::Tasks),
                    _ => None,
                };
                if let Some(section) = section {
                    self.set_section(section, cx);
                }
                return;
            }
        }
        window.request_animation_frame();
    }
}
