//! Живой ящик (S3-04): поток синхронизации учётной записи (плагин пути, `crate::plugins`) вместо демо, события — в
//! поток интерфейса.
//!
//! События идут по `std::sync::mpsc`; поток синхронизации после каждого события зовёт `repaint`, а тот будит
//! задачу gpui через канал `futures` (`unbounded_send`). Задача спит на `next().await` — таймеров и опроса нет:
//! в простое процессор не занят, кадры — только когда что-то изменилось. Проснувшись, задача забирает все
//! накопившиеся события разом: каждая часть экрана получает своё (`Mailer::on_sync`), общее — здесь (`plan`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::Level as Toast;
use postman_cache::{Cache, MailboxRow};
use postman_core::categories;
use postman_core::extract::Extractor;
use postman_core::sync::{self, Account, Command, ErrorKind, Event, Index, Status, SyncError};

use crate::Mailer;
use crate::account::{self, Settings};
use crate::backend::Backend;
use crate::list::Flagged;
use crate::signin::{SignIn, Submit};

/// Сколько последних строк поиска в справочнике помнить.
const PEOPLE_ASKED: usize = 64;
/// Поиск в справочнике — со скольки символов.
const PEOPLE_FROM: usize = 3;

/// Найти не больше — как у сервера (`SEARCH_LIMIT` синхронизации).
const SEARCH_LIMIT: usize = 1000;

/// Номер запуска синхронизации: события прежней учётной записи (до повторного входа) отбрасываются.
static SESSION: AtomicU64 = AtomicU64::new(1);

/// Состояние живого ящика в `Mailer`. В демо — пустое.
#[derive(Default)]
pub(crate) struct Live {
    session: u64,
    /// Строка состояния: «Подключение…», «Синхронизировано».
    pub(crate) status: String,
    /// Когда синхронизация последний раз закончилась (ЧЧ:ММ) — строка состояния задач (макет «Задачи»).
    pub(crate) synced_at: Option<String>,
    /// Строка входа (нет учётной записи или вход ещё не подтверждён сервером).
    pub(crate) signin: Option<Entity<SignIn>>,
    /// Вход со строки входа: запомнить после первого ответа сервера, при ошибке — вернуть строку входа.
    pending: Option<Account>,
    /// Строки, по которым уже искали в справочнике (`FindPeople`, S7-08): тот же запрос — не повторять.
    people_asked: Vec<String>,
    /// Пароль входа со строки входа — в диспетчер учётных данных (`false` — «Не хранить»).
    pending_store: bool,
    index: Index,
    extractor: Option<Extractor>,
    next_ticket: u64,
    /// Последняя показанная ошибка (её род): тот же род подряд (сервер недоступен при каждом опросе) — без нового
    /// тоста, пока сервер снова не ответит.
    last_error: Option<String>,
    /// Пришло первое `EmailsChanged` (снимки `--screens --live` ждут его).
    pub(crate) synced: bool,
    /// Адрес и логин учётной записи, с которой идёт синхронизация: для строки входа после отказа сервера.
    account: Option<Settings>,
    /// Файл кэша этой учётной записи: повторный вход в неё же — на открытом кэше (второй раз redb его не откроет).
    path: Option<PathBuf>,
    /// Сервер отклонил вход: показать строку входа на следующем кадре (ей нужно окно).
    pub(crate) reauth: Option<Settings>,
    /// Подписка на флаги списка уже есть (повторный вход не дублирует команды `Keyword`).
    flags_watched: bool,
    /// Локальная копия таблиц категорий прочитана; до этого ответы сервера ждут в `labels_early` (иначе копия,
    /// прочитанная позже, затёрла бы таблицу с сервера, а несохранённая правка — потерялась).
    labels_local: bool,
    labels_early: Vec<categories::Reply>,
    /// Запрос фильтров «Все папки» на сервере (S5-03, `Command::Query`): номер последнего.
    pub(crate) filter_ticket: Option<u64>,
    /// Учётные записи профиля вместе (S7-03б, `fold`): отправители и отложенные отправки каждой, ответы на поиск.
    pub(crate) profile: Profile,
}

/// Что приходит от нескольких учётных записей профиля и показывается вместе (S7-03б).
#[derive(Default)]
pub(crate) struct Profile {
    /// Отправители по учётным записям (`None` — основная): «От» — все адреса, основная первой.
    senders: Vec<(Option<String>, Vec<sync::Sender>)>,
    /// Отложенные отправки по учётным записям; предел «Отправить позже» — основной.
    scheduled: Vec<(Option<String>, Vec<postman_core::send_later::Scheduled>)>,
    max_delay: u64,
    /// Ответы на поиск «Все папки» (номер запроса → найденное и сколько всего): учётные записи отвечают по одной.
    results: Vec<(u64, Vec<String>, u64)>,
    /// Дополнительные учётные записи, у которых идёт синхронизация (`@<n>` → адрес и логин): «Настройки → Аккаунты»
    /// и подпись их ошибок.
    pub(crate) names: Vec<(String, Settings)>,
    /// Добавляется со строки «Добавить аккаунт» (`@<n>`): ждём первого ответа её сервера.
    pub(crate) adding: Option<String>,
}

/// Ответов на поиск — помнить не больше (старые запросы уже не показываются).
const RESULTS_KEPT: usize = 8;

impl Profile {
    /// Пачка событий учётной записи `profile` → общий вид. Отправители и отложенные отправки — всех учётных записей
    /// вместе; ответы на один поиск — вместе. У дополнительной (`Some`) — только почта, календарь и итоги её команд:
    /// кто я, контакты, справочник, настройки, категории, задачи, связь и состояние — у основной. Её ошибки —
    /// отдельно (тостом с её именем, без строки входа основной).
    pub(crate) fn fold(&mut self, profile: Option<&str>, batch: Vec<Event>) -> (Vec<Event>, Vec<SyncError>) {
        let mut out = Vec::with_capacity(batch.len());
        let mut errors = Vec::new();
        let key = profile.map(str::to_owned);
        for ev in batch {
            match ev {
                Event::Senders(list) => {
                    put(&mut self.senders, key.clone(), list);
                    out.push(Event::Senders(self.senders.iter().flat_map(|(_, l)| l.iter().cloned()).collect()));
                }
                Event::Scheduled { list, max_delay } => {
                    if profile.is_none() {
                        self.max_delay = max_delay;
                    }
                    put(&mut self.scheduled, key.clone(), list);
                    let list = self.scheduled.iter().flat_map(|(_, l)| l.iter().cloned()).collect();
                    out.push(Event::Scheduled { list, max_delay: self.max_delay });
                }
                Event::SearchResults { ticket, ids, total } => {
                    let (ids, total) = self.result(ticket, ids, total);
                    out.push(Event::SearchResults { ticket, ids, total });
                }
                Event::Error(e) if profile.is_some() => errors.push(e),
                Event::Status(_)
                | Event::Me { .. }
                | Event::Contacts(_)
                | Event::Settings(_)
                | Event::ContactsLoaded(_)
                | Event::DirectoryLoaded(_)
                | Event::PeopleFound { .. }
                | Event::ContactSaved { .. }
                | Event::ContactDeleted { .. }
                | Event::ContactFailed { .. }
                | Event::Categories(_)
                | Event::Tasks(_)
                | Event::Connection(_)
                | Event::Delegated { .. }
                | Event::Withheld { .. }
                | Event::FreeBusyShared { .. }
                | Event::Features(_)
                | Event::EventsFound { .. }
                | Event::CalendarFailed(_)
                    if profile.is_some() =>
                {
                    if let Event::CalendarFailed(e) = &ev {
                        tracing::warn!("calendar of {profile:?}: {e}");
                    }
                }
                ev => out.push(ev),
            }
        }
        (out, errors)
    }

    /// Ответ одной учётной записи на поиск: найденное — вместе с прежними ответами на тот же запрос.
    fn result(&mut self, ticket: u64, ids: Vec<String>, total: u64) -> (Vec<String>, u64) {
        match self.results.iter_mut().find(|(t, _, _)| *t == ticket) {
            Some((_, all, sum)) => {
                for id in ids {
                    if !all.contains(&id) {
                        all.push(id);
                    }
                }
                all.truncate(SEARCH_LIMIT);
                *sum += total;
                (all.clone(), *sum)
            }
            None => {
                self.results.push((ticket, ids.clone(), total));
                if self.results.len() > RESULTS_KEPT {
                    self.results.remove(0);
                }
                (ids, total)
            }
        }
    }

    /// Адрес «От» → учётная запись (маршрут письма, `Backend::send`).
    pub(crate) fn sender_routes(&self) -> Vec<(String, Option<String>)> {
        self.senders.iter().flat_map(|(p, l)| l.iter().map(move |s| (s.address.email.clone(), p.clone()))).collect()
    }

    /// Дополнительная учётная запись удалена: её отправители и отложенные отправки — тоже.
    pub(crate) fn forget(&mut self, profile: &str) {
        self.senders.retain(|(p, _)| p.as_deref() != Some(profile));
        self.scheduled.retain(|(p, _)| p.as_deref() != Some(profile));
        self.names.retain(|(p, _)| p != profile);
    }

    /// Всё, что знает список «От» сейчас (после удаления учётной записи — заново всем частям экрана).
    pub(crate) fn senders_event(&self) -> Event {
        Event::Senders(self.senders.iter().flat_map(|(_, l)| l.iter().cloned()).collect())
    }
}

/// Значение учётной записи в списке по учётным записям: основная — первой, дополнительные — по префиксу.
fn put<T>(list: &mut Vec<(Option<String>, T)>, key: Option<String>, value: T) {
    match list.iter_mut().find(|(k, _)| *k == key) {
        Some((_, v)) => *v = value,
        None => {
            list.push((key, value));
            list.sort_by(|a, b| a.0.cmp(&b.0));
        }
    }
}

impl Live {
    /// Номер нового запроса к серверу (поиск, фильтры).
    pub(crate) fn ticket(&mut self) -> u64 {
        self.next_ticket += 1;
        self.next_ticket
    }

    /// Индекс поиска учётной записи (демо и до входа — нет: поиск по кэшу).
    pub(crate) fn index(&self) -> Index {
        self.index.clone()
    }
}

/// Что сделать с экраном по пачке событий синхронизации (чистая функция — тестируется без окна).
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Plan {
    /// Сервер ответил (папки пришли): вход подтверждён.
    pub(crate) signed_in: bool,
    pub(crate) folders: bool,
    pub(crate) list: bool,
    /// Последний статус пачки.
    pub(crate) status: Option<Status>,
    pub(crate) errors: Vec<SyncError>,
    /// Сервер отклонил вход (401): синхронизация остановилась, нужна строка входа.
    pub(crate) auth: bool,
    /// Первая загрузка закончилась (`Status::Synced`).
    pub(crate) synced: bool,
    /// Календарь на сервере изменился (push) — перечитать открытую неделю.
    pub(crate) calendar: bool,
    /// Догрузка папок: (папка, сколько писем пришло).
    pub(crate) more: Vec<(String, usize)>,
    pub(crate) results: Vec<(u64, Vec<String>)>,
    /// Тела, пришедшие по запросу (`BodyReady`).
    pub(crate) bodies: Vec<String>,
    /// Письма с телом — в полнотекстовый индекс.
    pub(crate) index: Vec<String>,
}

pub(crate) fn plan(events: &[Event]) -> Plan {
    let mut p = Plan::default();
    for ev in events {
        match ev {
            Event::MailboxesChanged => {
                p.signed_in = true;
                p.folders = true;
            }
            Event::EmailsChanged => p.list = true,
            Event::Status(s) => {
                p.synced |= *s == Status::Synced;
                p.status = Some(s.clone());
            }
            Event::Error(e) => {
                p.auth |= e.kind == ErrorKind::Auth;
                p.errors.push(e.clone());
            }
            Event::CalendarChanged => p.calendar = true,
            Event::LoadedMore { mailbox, added } => p.more.push((mailbox.clone(), *added)),
            Event::SearchResults { ticket, ids, .. } => p.results.push((*ticket, ids.clone())),
            Event::BodyReady(id) => {
                p.bodies.push(id.clone());
                p.index.push(id.clone());
            }
            Event::BodiesStored(ids) => p.index.extend(ids.iter().cloned()),
            // Остальное берут части экрана (`on_sync`): окно письма, область чтения, календарь.
            _ => {}
        }
    }
    p
}

/// Результаты поиска: сначала локальные (нечёткие) совпадения, затем то, что нашёл только сервер.
pub(crate) fn merge(local: &[String], server: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    local.iter().chain(server).filter(|id| seen.insert(id.as_str())).cloned().collect()
}

/// Ошибки для тостов: тот же род ошибки подряд не показывается (сервер недоступен — один тост, а не на каждый
/// опрос); отказ во входе — не тостом, а строкой входа.
pub(crate) fn fresh_errors(last: &mut Option<String>, errors: Vec<SyncError>) -> Vec<SyncError> {
    errors
        .into_iter()
        .filter(|e| e.kind != ErrorKind::Auth)
        .filter(|e| {
            let key = format!("{:?}", e.kind);
            let fresh = last.as_deref() != Some(key.as_str());
            *last = Some(key);
            fresh
        })
        .collect()
}

/// Текст состояния синхронизации.
pub(crate) fn status_text(s: &Status) -> String {
    match s {
        Status::Connecting { url } => tr!("sync-connecting", url = url.clone()),
        Status::SignedIn { user } => tr!("sync-signed-in", user = user.clone()),
        Status::Loading { mailbox } => tr!("sync-loading", mailbox = mailbox.clone()),
        Status::Synced => tr!("sync-synced"),
    }
}

/// Текст ошибки синхронизации: наш текст по роду, текст сервера — только подробностью.
pub(crate) fn error_text(e: &SyncError) -> String {
    let what = match e.kind {
        ErrorKind::Auth => tr!("sync-error-auth"),
        ErrorKind::Network => tr!("sync-error-network"),
        ErrorKind::Server => tr!("sync-error-server"),
        ErrorKind::Protocol => tr!("sync-error-protocol"),
        ErrorKind::Storage => tr!("sync-error-storage"),
        ErrorKind::Internal => tr!("sync-error-internal"),
    };
    tr!("sync-error", what = what, detail = e.detail.clone())
}

/// Свои «Входящие», иначе первая своя папка, иначе первая (общих ящиков — S4-06).
pub(crate) fn inbox(mailboxes: &[MailboxRow]) -> Option<String> {
    let own = |b: &&MailboxRow| postman_cache::account_of(&b.id).is_none();
    mailboxes
        .iter()
        .filter(own)
        .find(|b| b.role.as_deref() == Some("inbox"))
        .or_else(|| mailboxes.iter().find(own))
        .or(mailboxes.first())
        .map(|b| b.id.clone())
}

/// Кэш и индекс поиска учётной записи (`<кэш>.search`). Индекс не открылся — поиск по кэшу.
pub(crate) fn open_store(path: &Path) -> Result<(Cache, Index), postman_cache::Error> {
    let cache = Cache::open(path)?;
    let index = match postman_search::SearchIndex::open(&path.with_extension("search")) {
        Ok(i) => Some(Arc::new(i)),
        Err(e) => {
            tracing::warn!("search index: {e}");
            None
        }
    };
    Ok((cache, index))
}

/// `--bench --live`: кэш и индекс живого ящика — в каталоге данных бенча (H-01), рабочий кэш учётной записи не
/// трогается. Без `warm` — с чистого листа (замер первой синхронизации); без `keep` — удаляется в конце.
/// Подготовка не входит во время запуска (`start`).
pub(crate) fn bench_store(warm: bool, keep: bool, start: &mut std::time::Instant) -> Result<(Cache, Index), String> {
    let prepared = std::time::Instant::now();
    let dir = postman_demo::testdata_dir().join("live-bench");
    if !warm && dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !keep {
        postman_core::bench::remove_on_finish(dir.clone());
    }
    let opened = open_store(&dir.join("cache.redb")).map_err(|e| e.to_string())?;
    *start += prepared.elapsed();
    Ok(opened)
}

/// Поиск по своим данным (в фоне): индекс, пока он пуст (строится) — кэш.
pub(crate) fn local_search(index: &Index, cache: &Cache, query: &str, mailbox: Option<&str>) -> Vec<String> {
    match index.as_ref().filter(|i| !i.is_empty()) {
        Some(i) => {
            // Строки, записанные синхронизацией, видны в поиске после коммита.
            let _ = i.commit();
            let ids = i.search(query, SEARCH_LIMIT).unwrap_or_default();
            match mailbox {
                Some(mb) => cache.filter_mailbox(&ids, mb).unwrap_or_default(),
                None => ids,
            }
        }
        None => cache.search(query, mailbox, SEARCH_LIMIT).unwrap_or_default(),
    }
}

/// Индекс пуст, а в кэше письма есть (кэш старше индекса): построить в фоне — строки, затем тексты тел.
fn build_index(index: Arc<postman_search::SearchIndex>, cache: Cache, extractor: Extractor) {
    let started = std::thread::Builder::new().name("index-build".into()).spawn(move || {
        let mut offset = 0;
        while let Ok(rows) = cache.all_rows(offset, 5000) {
            if rows.is_empty() {
                break;
            }
            offset += rows.len();
            postman_core::indexer::rows(&index, &rows);
        }
        let _ = index.commit();
        let mut offset = 0;
        while let Ok(ids) = cache.body_ids(offset, 5000) {
            if ids.is_empty() {
                break;
            }
            offset += ids.len();
            extractor.queue(ids);
        }
    });
    if let Err(e) = started {
        tracing::warn!("index build thread: {e}");
    }
}

impl Mailer {
    /// Запустить синхронизацию учётной записи на кэше этого экрана. `confirm` — вход со строки входа:
    /// запомнить учётную запись после первого ответа сервера.
    pub(crate) fn start_sync(&mut self, account: Account, index: Index, confirm: bool, cx: &mut Context<Self>) {
        let session = SESSION.fetch_add(1, Ordering::Relaxed);
        let (events, rx) = std::sync::mpsc::channel::<Event>();
        let (wake_tx, mut wake_rx) = futures::channel::mpsc::unbounded::<()>();
        let wake = wake_tx.clone();
        let repaint = move || {
            let _ = wake.unbounded_send(());
        };
        tracing::info!("sync start: {}", account.url);
        self.live.status = status_text(&Status::Connecting { url: account.url.clone() });
        self.live.account = Some(Settings::of(&account));
        // Свой аккаунт — подписью над папками (метка «нет связи» — у неё, S5-08).
        let own = Settings::of(&account).user;
        self.folders.update(cx, |f, cx| {
            f.own = Some(own);
            cx.notify();
        });
        self.live.path = Some(account::cache_path(&crate::data_dir(), &account));
        // Путь к серверу — по адресу (ADR 0004): `imaps://…` — IMAP + SMTP, иначе JMAP.
        let commands = crate::plugins::registry().spawn(account.clone(), None, self.cache.clone(), index.clone(), events, repaint);
        // Поток синхронизации мог не запуститься: его ошибка лежит в канале без пробуждения.
        let _ = wake_tx.unbounded_send(());
        drop(wake_tx);
        self.backend = Backend::Live { commands, extra: Vec::new(), senders: Vec::new() };
        // Область чтения: блобы и ответы на приглашения — через сервер; мой адрес придёт `Event::Me`.
        self.reading.update(cx, |r, _| {
            r.live = true;
            r.me = None;
        });
        crate::shared::set_me(None, cx);
        // Календарь — с сервера (S3-05); раздел уже открыт — неделя сразу.
        let calendar_open = self.section == crate::Section::Calendar;
        self.cal.update(cx, |c, cx| {
            c.go_live(cx);
            if calendar_open {
                c.refresh(cx);
            }
        });
        // Кто я для фильтров (упоминания, «Мне», внешние) — из `Event::Me`, не владелец демо-ящика.
        self.list.update(cx, |l, cx| l.forget_me(cx));
        // Контакты и справочник — с сервера (S4-02): поток синхронизации присылает их после входа.
        self.contacts.update(cx, |c, cx| c.go_live(cx));
        self.tasks.update(cx, |t, cx| t.go_live(cx));
        self.live.session = session;
        self.live.pending = confirm.then_some(account);
        self.live.extractor = index.clone().map(|i| Extractor::spawn(self.cache.clone(), i));
        if let (Some(i), Some(x)) = (index.clone().filter(|i| i.is_empty()), self.live.extractor.clone()) {
            build_index(i, self.cache.clone(), x);
        }
        self.live.index = index;
        // Флаги, изменённые пользователем, — на сервер (в кэше они уже записаны). Подписка одна на экран.
        if !self.live.flags_watched {
            self.live.flags_watched = true;
            self._subscriptions.push(cx.subscribe(&self.list, |this, _, f: &Flagged, cx| {
                let sent = this.backend.send(Command::Keyword { id: f.id.clone(), keyword: f.keyword, on: f.on });
                if !sent {
                    this.sync_stopped(cx);
                }
            }));
            // Категории (S5-02) — тоже на сервер, одним `Email/set` на аккаунт.
            self._subscriptions.push(cx.subscribe(&self.list, |this, _, l: &crate::list::Labeled, cx| {
                let cmd = Command::SetKeywords { ids: l.ids.clone(), add: l.add.clone(), remove: l.remove.clone() };
                if !this.backend.send(cmd) {
                    this.sync_stopped(cx);
                }
            }));
        }
        self.load_labels(cx);
        self.load_account_prefs(cx);
        // Дополнительные учётные записи профиля (S7-03б) — на том же кэше, рядом с основной.
        self.live.profile = Default::default();
        self.start_extras(cx);
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            // Поток синхронизации закончился — отправитель пробуждений закрыт, задача завершается.
            while wake_rx.next().await.is_some() {
                while wake_rx.try_recv().is_ok() {}
                let batch: Vec<Event> = rx.try_iter().collect();
                if batch.is_empty() {
                    continue;
                }
                let alive = this.update(cx, |m, cx| {
                    if m.live.session == session {
                        m.on_profile_events(None, batch, cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    /// Пачка событий синхронизации.
    fn on_events(&mut self, events: &[Event], cx: &mut Context<Self>) {
        for ev in events {
            self.on_sync(ev, cx);
            if let Event::Categories(reply) = ev {
                self.on_categories(reply.clone(), cx);
            }
        }
        let plan = plan(events);
        if plan.signed_in
            && let Some(account) = self.live.pending.take()
        {
            let store = self.live.pending_store;
            self.remember(account, store, cx);
        }
        if let Some(s) = plan.status {
            if s == Status::Synced {
                self.live.synced_at = Some(jiff::Zoned::now().strftime("%H:%M").to_string());
            }
            self.live.status = status_text(&s);
            cx.notify();
        }
        if plan.synced
            && let Some(b) = &mut self.bench
        {
            b.synced();
        }
        if !plan.errors.is_empty() {
            self.sync_errors(plan.errors, plan.auth, cx);
        }
        for (mailbox, added) in plan.more {
            self.list.update(cx, |l, cx| {
                if l.mailbox.as_deref() == Some(mailbox.as_str()) {
                    l.more.loaded(added);
                    cx.notify();
                }
            });
        }
        // Неделя открыта — перечитать; раздел закрыт — перечитается при входе в него (`set_section`).
        if plan.calendar && self.section == crate::Section::Calendar {
            self.cal.update(cx, |c, cx| c.refresh(cx));
        }
        if plan.folders {
            // Сервер снова отвечает: следующая ошибка — новым тостом.
            self.live.last_error = None;
            self.reload_folders(cx);
        }
        if plan.list {
            self.live.synced = true;
            self.list.update(cx, |l, cx| l.refresh(cx));
            // Письма со сроком — задачи «Из почты» (S6-03).
            if self.section == crate::Section::Tasks {
                self.tasks.update(cx, |t, cx| t.letters_changed(cx));
            }
        }
        for (ticket, ids) in plan.results {
            // Поиск командной строки (S5-10, `search.rs`) или фильтры «Все папки».
            if !self.on_search_results(ticket, &ids, cx) && self.live.filter_ticket == Some(ticket) {
                // Фильтры «Все папки»: письма сервера уже в кэше — список пересчитывается с ними.
                self.list.update(cx, |l, cx| l.set_server_hits(ids, cx));
            }
        }
        for id in plan.bodies {
            self.forward_body_ready(&id, cx);
            self.load_body(id, false, cx);
        }
        if let Some(x) = &self.live.extractor
            && !plan.index.is_empty()
        {
            x.queue(plan.index);
        }
    }

    /// Пачка событий учётной записи профиля (S7-03б): общий вид (`Profile::fold`) — частям экрана, ошибки
    /// дополнительной — тостом с её именем.
    pub(crate) fn on_profile_events(&mut self, profile: Option<&str>, batch: Vec<Event>, cx: &mut Context<Self>) {
        let senders_before = self.live.profile.sender_routes();
        let (events, errors) = self.live.profile.fold(profile, batch);
        let routes = self.live.profile.sender_routes();
        if routes != senders_before {
            self.backend.set_senders(routes);
        }
        if !events.is_empty() {
            self.on_events(&events, cx);
        }
        // Добавление со строки «Добавить аккаунт»: сервер ответил — карточка закрывается; отказал — причина в ней же,
        // учётная запись не остаётся.
        if profile.is_some() && self.live.profile.adding.as_deref() == profile {
            if let Some(e) = errors.first() {
                let text = error_text(e);
                if let Some(panel) = &self.settings.add {
                    panel.update(cx, |p, cx| p.failed(&text, cx));
                }
                self.live.profile.adding = None;
                if let Some(p) = profile {
                    self.remove_extra(p.to_owned(), cx);
                }
                return;
            }
            if events.iter().any(|e| matches!(e, Event::MailboxesChanged)) {
                self.live.profile.adding = None;
                self.cancel_add_account(cx);
                self.refresh_settings_info(cx);
            }
        }
        let name = profile.and_then(|p| self.live.profile.names.iter().find(|(q, _)| q == p)).map(|(_, s)| s.user.clone());
        for e in fresh_errors(&mut self.live.last_error, errors) {
            tracing::warn!("sync {profile:?}: {e}");
            let text = tr!("sync-account-error", account = name.clone().unwrap_or_default(), error = error_text(&e));
            self.toasts.update(cx, |t, cx| t.push(Toast::Error, text, cx));
        }
    }

    /// Дополнительная учётная запись профиля (S7-03б): своя синхронизация на том же кэше, ключи — с её префиксом.
    pub(crate) fn start_extra(&mut self, extra: account::Extra, account: Account, cx: &mut Context<Self>) {
        if !self.backend.is_live() {
            return;
        }
        let session = self.live.session;
        let profile = extra.profile.clone();
        let (events, rx) = std::sync::mpsc::channel::<Event>();
        let (wake_tx, mut wake_rx) = futures::channel::mpsc::unbounded::<()>();
        let wake = wake_tx.clone();
        let repaint = move || {
            let _ = wake.unbounded_send(());
        };
        tracing::info!("sync start {profile}: {}", account.url);
        self.live.profile.names.retain(|(p, _)| *p != profile);
        self.live.profile.names.push((profile.clone(), extra.settings.clone()));
        let commands = crate::plugins::registry().spawn(account, Some(profile.clone()), self.cache.clone(), self.live.index.clone(), events, repaint);
        let _ = wake_tx.unbounded_send(());
        drop(wake_tx);
        self.backend.add_extra(profile.clone(), commands);
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            while wake_rx.next().await.is_some() {
                while wake_rx.try_recv().is_ok() {}
                let batch: Vec<Event> = rx.try_iter().collect();
                if batch.is_empty() {
                    continue;
                }
                let alive = this.update(cx, |m, cx| {
                    // Учётную запись удалили или вошли заново — её события уже не нужны.
                    if m.live.session == session && m.live.profile.names.iter().any(|(p, _)| *p == profile) {
                        m.on_profile_events(Some(&profile), batch, cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Дополнительные учётные записи из профиля — после основной (`start_sync`): без пароля в диспетчере — без
    /// синхронизации («Настройки → Аккаунты» покажет их).
    pub(crate) fn start_extras(&mut self, cx: &mut Context<Self>) {
        let dir = crate::data_dir();
        let task = cx.background_spawn(async move {
            account::load_extras(&dir).into_iter().filter_map(|e| account::extra_account(&e).map(|a| (e, a))).collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let list = task.await;
            let _ = this.update(cx, |m, cx| {
                for (e, a) in list {
                    m.start_extra(e, a, cx);
                }
            });
        })
        .detach();
    }

    /// «Добавить аккаунт», когда учётная запись уже есть (S7-03б): ещё одна — рядом, со своей синхронизацией; та же,
    /// что основная, — отказ в карточке.
    pub(crate) fn add_extra_account(&mut self, account: Account, store: bool, cx: &mut Context<Self>) {
        let settings = Settings::of(&account);
        let same = self.live.account.as_ref() == Some(&settings) || self.live.profile.names.iter().any(|(_, s)| *s == settings);
        if same {
            if let Some(panel) = &self.settings.add {
                panel.update(cx, |p, cx| p.failed(&tr!("settings-account-exists"), cx));
            }
            return;
        }
        let store = store && crate::security::effective(&crate::prefs::get(cx)).store_password;
        let dir = crate::data_dir();
        let saved = account.clone();
        let task = cx.background_spawn(async move { account::add_extra(&dir, &saved, store) });
        cx.spawn(async move |this, cx| {
            let added = task.await;
            let _ = this.update(cx, |m, cx| match added {
                Ok(extra) => {
                    m.live.profile.adding = Some(extra.profile.clone());
                    m.start_extra(extra, account, cx);
                }
                Err(e) => {
                    tracing::warn!("{e}");
                    if let Some(panel) = &m.settings.add {
                        panel.update(cx, |p, cx| p.failed(&e, cx));
                    }
                }
            });
        })
        .detach();
    }

    /// «Настройки → Аккаунты → Удалить» у дополнительной учётной записи: синхронизация останавливается, её
    /// письма и папки уходят из кэша, адрес и пароль — из профиля.
    pub(crate) fn remove_extra(&mut self, profile: String, cx: &mut Context<Self>) {
        tracing::info!("account {profile} removed by the user");
        self.backend.remove_extra(&profile);
        self.live.profile.forget(&profile);
        self.backend.set_senders(self.live.profile.sender_routes());
        let senders = self.live.profile.senders_event();
        self.on_events(&[senders], cx);
        let (dir, cache, live) = (crate::data_dir(), self.cache.clone(), self.backend.is_live());
        let task = cx.background_spawn(async move {
            // Демо не трогает профиль на диске: там — настоящие учётные записи.
            if live && let Err(e) = account::remove_extra(&dir, &profile) {
                tracing::warn!("{e}");
            }
            if let Err(e) = cache.forget_profile(&profile) {
                tracing::warn!("cache: {e}");
            }
        });
        cx.spawn(async move |this, cx| {
            task.await;
            let _ = this.update(cx, |m, cx| {
                m.reload_folders(cx);
                m.list.update(cx, |l, cx| l.refresh(cx));
                m.cal.update(cx, |c, cx| c.refresh(cx));
                m.refresh_settings_info(cx);
            });
        })
        .detach();
    }

    fn sync_errors(&mut self, errors: Vec<SyncError>, auth: bool, cx: &mut Context<Self>) {
        for e in &errors {
            tracing::warn!("sync: {e}");
        }
        // Вход со строки входа не удался: синхронизация остановилась, строка входа — с причиной.
        if self.live.pending.take().is_some() {
            self.backend = Backend::Demo;
            // Синхронизация остановлена: «нет связи» к этой попытке входа больше не относится.
            self.offline = Default::default();
            self.show_offline(String::new(), false, cx);
            self.live.status.clear();
            if let (Some(panel), Some(e)) = (&self.live.signin, errors.last()) {
                let text = error_text(e);
                panel.update(cx, |p, cx| p.failed(&text, cx));
            }
            cx.notify();
            return;
        }
        // Сохранённый вход отклонён (пароль сменили, сессию отозвали): синхронизация остановилась сама —
        // строка входа с адресом и логином, новый пароль заменит прежний в диспетчере (`remember`).
        if auth && self.live.signin.is_none() {
            self.live.status.clear();
            self.live.reauth = Some(self.live.account.clone().unwrap_or_default());
            cx.notify();
        }
        // Нет связи — не тостом, а строкой состояния, меткой у аккаунта и полосой над списком (S5-08, `states.rs`).
        let errors: Vec<SyncError> = errors.into_iter().filter(|e| e.kind != ErrorKind::Network).collect();
        for e in fresh_errors(&mut self.live.last_error, errors) {
            let text = tr!("account-sync-error", error = error_text(&e));
            self.toasts.update(cx, |t, cx| t.push(Toast::Error, text, cx));
        }
    }

    /// Строка входа после отказа сервера: поля — прежней учётной записи, причина — сразу под ними.
    pub(crate) fn show_reauth(&mut self, prefill: Settings, window: &mut Window, cx: &mut Context<Self>) {
        tracing::info!("sign-in rejected by the server: asking for the password again");
        self.set_section(crate::Section::Mail, cx);
        self.show_signin(Some(prefill), window, cx);
        if let Some(panel) = &self.live.signin {
            panel.update(cx, |p, cx| p.rejected(tr!("sync-reauth"), cx));
        }
    }

    /// Список дошёл до конца загруженного: следующая страница папки с сервера. Демо — догружать нечего.
    pub(crate) fn load_more(&mut self, mailbox: String, cx: &mut Context<Self>) {
        if !self.backend.is_live() {
            return;
        }
        if !self.backend.send(Command::LoadMore { mailbox }) {
            self.list.update(cx, |l, _| l.more.loaded(0));
            self.sync_stopped(cx);
        }
    }

    /// Число писем открытой папки на сервере — списку (догрузка); демо — без догрузки.
    pub(crate) fn sync_total(&mut self, cx: &mut Context<Self>) {
        let live = self.backend.is_live();
        let mailboxes = &self.folders.read(cx).mailboxes;
        let current = self.list.read(cx).mailbox.clone();
        let total = current.and_then(|id| mailboxes.iter().find(|b| b.id == id)).map(|b| b.total as usize).filter(|_| live);
        self.list.update(cx, |l, _| l.more.set_total(total));
    }

    /// «Элементов: N» в строке состояния; папка загружена не вся — «N из M» (M — на сервере).
    pub(crate) fn items_status(&self, cx: &App) -> String {
        let list = self.list.read(cx);
        // Беседы в списке (S5-05б): писем, а не строк.
        let n = list.threads().map_or(list.count, |o| o.total());
        match list.more.total.filter(|&t| t > n && list.search_ids.is_none()) {
            Some(total) => tr!("status-items-of", n = n, total = total),
            None => tr!("status-items", n = n),
        }
    }

    /// Команда не ушла: поток синхронизации остановился (ошибка уже показана).
    pub(crate) fn sync_stopped(&mut self, cx: &mut Context<Self>) {
        let text = tr!("account-sync-stopped");
        if self.live.last_error.as_deref() != Some(text.as_str()) {
            self.live.last_error = Some(text.clone());
            self.toasts.update(cx, |t, cx| t.push(Toast::Warning, text, cx));
        }
    }

    /// Таблицы категорий учётной записи (S5-02) — сначала локальная копия из файла рядом с кэшем (в фоне), затем
    /// с сервера (`Event::Categories` при входе). Копии нет — по умолчанию (список их уже показывает); копия не
    /// читается — тоже по умолчанию, причина — в журнал.
    fn load_labels(&mut self, cx: &mut Context<Self>) {
        use postman_core::labels::{LocalFile, Store as _};
        let Some(path) = self.live.path.clone() else { return };
        self.live.labels_local = false;
        self.live.labels_early.clear();
        let task = cx.background_spawn(async move { LocalFile::beside(&path).load() });
        cx.spawn(async move |this, cx| {
            let book = match task.await {
                Ok(book) => book,
                Err(e) => {
                    tracing::warn!("{e}");
                    None
                }
            };
            let _ = this.update(cx, |m, cx| {
                let book = book.unwrap_or_else(crate::labels::default_book);
                m.list.update(cx, |l, cx| l.set_labels(book, cx));
                m.live.labels_local = true;
                for reply in std::mem::take(&mut m.live.labels_early) {
                    m.on_categories(reply, cx);
                }
            });
        })
        .detach();
    }

    /// Подписи и шаблоны учётной записи (S5-09) — из файла рядом с кэшем (в фоне), для письма.
    fn load_account_prefs(&mut self, cx: &mut Context<Self>) {
        use postman_core::prefs::AccountPrefs;
        let Some(path) = self.live.path.clone() else { return };
        let task = cx.background_spawn(async move { AccountPrefs::load(&AccountPrefs::beside(&path)) });
        cx.spawn(async move |this, cx| {
            let prefs = match task.await {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("{e}");
                    return;
                }
            };
            let _ = this.update(cx, |m, _| m.compose.prefs = prefs);
        })
        .detach();
    }

    /// Ответ сервера по таблице категорий (`postman_core::categories`): таблица — в список и в локальную копию.
    /// Своя правка ещё не дошла до сервера — не заменяется, а уходит на сервер (сведётся с серверной).
    fn on_categories(&mut self, reply: categories::Reply, cx: &mut Context<Self>) {
        use categories::Reply;
        if !self.live.labels_local {
            self.live.labels_early.push(reply);
            return;
        }
        let mut book = self.list.read(cx).labels.clone();
        match reply {
            Reply::Loaded { account, table } => {
                if let Some(mine) = book.loaded(account.as_deref(), table, crate::labels::default_table) {
                    let request = categories::Request::Save { account, table: mine };
                    if !self.backend.send(Command::Categories(request)) {
                        self.sync_stopped(cx);
                    }
                    return;
                }
            }
            Reply::Saved { account, table } => book.saved(account.as_deref(), table),
            Reply::Unsupported { account } => {
                tracing::info!("categories: no FileNode for account {account:?}, local table only");
                return;
            }
            Reply::Failed { account, error } => {
                tracing::warn!("categories: account {account:?}: {error}");
                return;
            }
            Reply::Purged { keyword, count, .. } => {
                tracing::info!("categories: {keyword} removed from {count} messages");
                self.list.update(cx, |l, cx| l.refresh(cx));
                return;
            }
            Reply::PurgeFailed { error, .. } => {
                tracing::warn!("categories: purge: {error}");
                return;
            }
        }
        if self.list.read(cx).labels == book {
            return;
        }
        self.save_labels(&book, cx);
        self.list.update(cx, |l, cx| l.set_labels(book, cx));
        // Таблица во вкладке «Настройки → Категории» — та же.
        self.categories_changed(cx);
    }

    /// Локальная копия таблиц — в файл рядом с кэшем (в фоне).
    pub(crate) fn save_labels(&self, book: &postman_core::labels::Book, cx: &mut Context<Self>) {
        use postman_core::labels::{LocalFile, Store as _};
        let Some(path) = self.live.path.clone() else { return };
        let book = book.clone();
        cx.background_spawn(async move {
            if let Err(e) = LocalFile::beside(&path).save(&book) {
                tracing::warn!("{e}");
            }
        })
        .detach();
    }

    /// Вход подтверждён сервером: адрес и логин — в файл, пароль — в диспетчер учётных данных (в фоне).
    fn remember(&mut self, account: Account, store: bool, cx: &mut Context<Self>) {
        self.live.signin = None;
        // Аккаунт добавляли из настроек: карточка добавления там больше не нужна.
        self.signed_in(cx);
        let task = cx.background_spawn(async move { account::remember(&crate::data_dir(), &account, store) });
        cx.spawn(async move |this, cx| {
            if let Err(e) = task.await {
                tracing::warn!("remember account: {e}");
                let _ = this.update(cx, |m, cx| {
                    let text = tr!("account-remember-failed", error = e);
                    m.toasts.update(cx, |t, cx| t.push(Toast::Warning, text, cx));
                });
            }
        })
        .detach();
        cx.notify();
    }

    /// Папки изменились: перечитать в фоне; текущей папки больше нет (или не было) — открыть входящие.
    fn reload_folders(&mut self, cx: &mut Context<Self>) {
        let cache = self.cache.clone();
        let task = cx.background_spawn(async move { (cache.mailboxes().unwrap_or_default(), cache.accounts().unwrap_or_default()) });
        cx.spawn(async move |this, cx| {
            let (rows, accounts) = task.await;
            let _ = this.update(cx, |m, cx| {
                let current = m.folders.read(cx).current.clone();
                let open = match current {
                    Some(id) if rows.iter().any(|b| b.id == id) => None,
                    _ => inbox(&rows),
                };
                m.list.update(cx, |l, _| l.set_rights(&rows));
                m.folders.update(cx, |f, cx| {
                    f.mailboxes = rows;
                    f.accounts = accounts;
                    cx.notify();
                });
                if let Some(id) = open {
                    m.open_mailbox(id, cx);
                }
                m.sync_total(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Открыть письмо в области чтения. Живой ящик: тело из кэша (в фоне), нет в кэше — с сервера (`Body`).
    pub(crate) fn open_letter(&mut self, row: postman_cache::EmailRow, cx: &mut Context<Self>) {
        if !self.backend.is_live() {
            self.reading.update(cx, |r, cx| r.open(row, cx));
            return;
        }
        let shown = self.reading.read(cx).reading.as_ref().is_some_and(|r| r.row.id == row.id && r.body.is_some());
        if shown {
            return;
        }
        let id = row.id.clone();
        self.reading.update(cx, |r, cx| r.open_live(row, cx));
        self.load_body(id, true, cx);
    }

    /// Тело письма из кэша — в область чтения, если письмо всё ещё открыто. `fetch` — нет в кэше: запросить.
    fn load_body(&mut self, id: String, fetch: bool, cx: &mut Context<Self>) {
        let open = self.reading.read(cx).reading.as_ref().is_some_and(|r| r.row.id == id);
        if !open {
            return;
        }
        let cache = self.cache.clone();
        let key = id.clone();
        let task = cx.background_spawn(async move { cache.body(&key).ok().flatten() });
        cx.spawn(async move |this, cx| {
            let body = task.await;
            let _ = this.update(cx, |m, cx| {
                if let Some(b) = body {
                    m.reading.update(cx, |r, cx| r.show_body(&id, b, cx));
                } else if fetch && !m.backend.send(Command::Body(id)) {
                    m.sync_stopped(cx);
                }
            });
        })
        .detach();
    }

    /// Нет учётной записи (первый запуск): добавление аккаунта на месте экрана почты.
    pub(crate) fn show_signin(&mut self, prefill: Option<Settings>, window: &mut Window, cx: &mut Context<Self>) {
        let panel = cx.new(|cx| SignIn::new(prefill, true, window, cx));
        self.watch_signin(&panel, window, cx);
        self.live.signin = Some(panel);
        cx.notify();
    }

    pub(crate) fn watch_signin(&mut self, panel: &Entity<SignIn>, window: &mut Window, cx: &mut Context<Self>) {
        self._subscriptions.push(cx.subscribe_in(panel, window, |_, panel, s: &Submit, window, cx| {
            // Вид сервера по адресу (S7-04в): JMAP, иначе IMAP на том же узле — в фоне, до входа.
            let (mut account, store, panel) = (s.account.clone(), s.store, panel.clone());
            let url = account.url.clone();
            let task = cx.background_spawn(async move { crate::plugins::registry().detect_blocking(&url) });
            cx.spawn_in(window, async move |this, cx| {
                account.url = task.await;
                let _ = this.update_in(cx, |m, window, cx| m.submit_account(&panel, account, store, window, cx));
            })
            .detach();
        }));
        self._subscriptions.push(cx.subscribe_in(panel, window, |this, _, _: &crate::signin::Cancel, _, cx| {
            this.cancel_add_account(cx);
        }));
    }

    /// «Проверить и подключить» с адресом, вид сервера которого уже известен.
    fn submit_account(&mut self, panel: &Entity<SignIn>, account: Account, store: bool, window: &mut Window, cx: &mut Context<Self>) {
        // Из настроек, когда учётная запись уже есть: ещё одна — рядом (S7-03б), карточка остаётся в настройках.
        let from_settings = self.settings.add.as_ref().map(|p| p.entity_id()) == Some(panel.entity_id());
        if from_settings && self.backend.is_live() && self.live.pending.is_none() {
            self.add_extra_account(account, store, cx);
            return;
        }
        // Из настроек: дальше вход показывает та же карточка на месте экрана почты.
        if self.live.signin.is_none() {
            self.live.signin = Some(panel.clone());
        }
        self.sign_in(account, store, window, cx);
    }

    /// Поиск в справочнике по набранной строке (S7-08): JMAP — `Principal/query`, Exchange — `ResolveNames`; найденное
    /// придёт `PeopleFound` и ляжет в подсказки адресов и «Контакты» (к уже загруженному справочнику).
    pub(crate) fn find_people(&mut self, text: &str, cx: &App) {
        let text = text.trim();
        if text.chars().count() < PEOPLE_FROM
            || text.contains('<')
            || !self.backend.is_live()
            || !crate::features::get(cx).has(postman_core::model::Feature::Directory)
            || self.live.people_asked.iter().any(|t| t.eq_ignore_ascii_case(text))
        {
            return;
        }
        if self.live.people_asked.len() >= PEOPLE_ASKED {
            self.live.people_asked.remove(0);
        }
        self.live.people_asked.push(text.to_owned());
        let _ = self.backend.send(Command::FindPeople { text: text.to_owned(), offset: 0 });
    }

    /// Адрес и логин учётной записи, с которой идёт синхронизация (для «Настройки → Аккаунты»).
    pub(crate) fn live_account(&self) -> Option<&Settings> {
        self.live.account.as_ref()
    }

    /// Файл кэша учётной записи (рядом — её настройки: подписи и шаблоны); демо — `None`.
    pub(crate) fn account_cache(&self) -> Option<PathBuf> {
        self.live.path.clone()
    }

    /// Удалить аккаунт («Настройки → Аккаунты»): синхронизация останавливается, адрес и логин — из файла, пароль — из
    /// диспетчера учётных данных (в фоне); кэш на диске остаётся. Экран — первый запуск.
    pub(crate) fn remove_account(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        tracing::info!("account removed by the user");
        cx.background_spawn(async move { account::forget(&crate::data_dir()) }).detach();
        let cache = match Cache::open_in_memory() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("in-memory store: {e}");
                return;
            }
        };
        let screens = self.screens.take();
        // Прежний поток синхронизации закончится сам: канал команд закрывается вместе с экраном.
        SESSION.fetch_add(1, Ordering::Relaxed);
        *self = Mailer { screens, ..Mailer::new(cache, None, window, cx) };
        self.show_signin(None, window, cx);
    }

    /// «Проверить и подключить»: открыть кэш этой учётной записи (в фоне), затем экран на нём и синхронизацию.
    fn sign_in(&mut self, account: Account, store: bool, window: &mut Window, cx: &mut Context<Self>) {
        // «Не хранить» пароль (Безопасность или политика, S5-30) — сильнее галочки входа.
        let store = store && crate::security::effective(&crate::prefs::get(cx)).store_password;
        self.live.pending_store = store;
        let path = account::cache_path(&crate::data_dir(), &account);
        // Снова вход в ту же учётную запись (сервер отклонил пароль): кэш и экран уже её.
        if self.live.path.as_ref() == Some(&path) {
            let index = self.live.index.clone();
            self.live.last_error = None;
            self.start_sync(account, index, true, cx);
            return;
        }
        let task = cx.background_spawn(async move { open_store(&path) });
        cx.spawn_in(window, async move |this, cx| {
            let opened = task.await;
            let _ = this.update_in(cx, |m, window, cx| match opened {
                Ok((cache, index)) => m.switch_account(cache, index, account, window, cx),
                Err(e) => {
                    tracing::warn!("account store: {e}");
                    if let Some(panel) = &m.live.signin {
                        panel.update(cx, |p, cx| p.failed(&e.to_string(), cx));
                    }
                }
            });
        })
        .detach();
    }

    /// Экран заново на кэше учётной записи; строка входа остаётся, пока сервер не ответит.
    fn switch_account(&mut self, cache: Cache, index: Index, account: Account, window: &mut Window, cx: &mut Context<Self>) {
        let panel = self.live.signin.take();
        let screens = self.screens.take();
        let store = self.live.pending_store;
        *self = Mailer { screens, ..Mailer::new(cache, None, window, cx) };
        self.live.pending_store = store;
        if let Some(panel) = panel {
            self.watch_signin(&panel, window, cx);
            self.live.signin = Some(panel);
        }
        self.start_sync(account, index, true, cx);
    }
}

#[cfg(test)]
mod tests {
    // Не `super::*`: из `gpui_kit::*` пришёл бы атрибут `test` gpui вместо встроенного.
    use super::{ErrorKind, Event, MailboxRow, Plan, Status, SyncError, fresh_errors, inbox, merge, plan};
    use postman_core::sync::Stage;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn err(kind: ErrorKind, detail: &str) -> SyncError {
        SyncError { kind, stage: Stage::Refresh, detail: detail.into() }
    }

    fn connecting() -> Event {
        Event::Status(Status::Connecting { url: "https://mail".into() })
    }

    #[test]
    fn events_become_a_plan() {
        let events = vec![
            connecting(),
            Event::MailboxesChanged,
            Event::EmailsChanged,
            Event::EmailsChanged,
            Event::BodyReady("a".into()),
            Event::BodiesStored(ids(&["b", "c"])),
            Event::SearchResults { ticket: 7, ids: ids(&["x"]), total: 1 },
            Event::Error(err(ErrorKind::Network, "offline")),
            Event::Status(Status::Synced),
            Event::Sent { ticket: 1, scheduled: None },
            Event::LoadedMore { mailbox: "in".into(), added: 500 },
            Event::CalendarChanged,
        ];
        let p = plan(&events);
        assert_eq!(
            p,
            Plan {
                signed_in: true,
                folders: true,
                list: true,
                status: Some(Status::Synced),
                errors: vec![err(ErrorKind::Network, "offline")],
                auth: false,
                synced: true,
                calendar: true,
                more: vec![("in".into(), 500)],
                results: vec![(7, ids(&["x"]))],
                bodies: ids(&["a"]),
                index: ids(&["a", "b", "c"]),
            }
        );
        assert_eq!(plan(&[Event::Contacts(Vec::new())]), Plan::default());
        // Ошибка до первого ответа сервера — вход не подтверждён.
        let rejected = plan(&[connecting(), Event::Error(err(ErrorKind::Auth, "401"))]);
        assert!(!rejected.signed_in && rejected.auth);
    }

    #[test]
    fn local_results_first_then_server_only() {
        assert_eq!(merge(&ids(&["a", "b"]), &ids(&["c", "a", "d"])), ids(&["a", "b", "c", "d"]));
        assert_eq!(merge(&[], &ids(&["c", "c"])), ids(&["c"]));
        assert_eq!(merge(&[], &[]), Vec::<String>::new());
    }

    #[test]
    fn same_error_is_not_repeated() {
        let mut last = None;
        let net = |d: &str| err(ErrorKind::Network, d);
        // Сервер недоступен при каждом опросе (подробности разные) — один тост.
        assert_eq!(fresh_errors(&mut last, vec![net("timeout"), net("connect")]), vec![net("timeout")]);
        assert_eq!(fresh_errors(&mut last, vec![net("timeout")]), vec![]);
        let server = err(ErrorKind::Server, "500");
        assert_eq!(fresh_errors(&mut last, vec![server.clone(), net("timeout")]), vec![server, net("timeout")]);
        // Отказ во входе — строкой входа, не тостом.
        assert_eq!(fresh_errors(&mut last, vec![err(ErrorKind::Auth, "401")]), vec![]);
    }

    #[test]
    fn inbox_is_preferred() {
        let mb = |id: &str, role: Option<&str>| MailboxRow {
            id: id.into(),
            name: id.into(),
            role: role.map(str::to_owned),
            parent_id: None,
            sort_order: 0,
            total: 0,
            unread: 0,
            rights: postman_cache::Rights::ALL,
        };
        assert_eq!(inbox(&[mb("sent", Some("sent")), mb("in", Some("inbox"))]), Some("in".into()));
        // «Входящие» общего ящика (S4-06) — не свои.
        assert_eq!(inbox(&[mb("i/in", Some("inbox")), mb("x", None), mb("in", Some("inbox"))]), Some("in".into()));
        assert_eq!(inbox(&[mb("i/in", Some("inbox")), mb("x", None)]), Some("x".into()));
        assert_eq!(inbox(&[mb("a", None), mb("b", None)]), Some("a".into()));
        assert_eq!(inbox(&[]), None);
    }
}
