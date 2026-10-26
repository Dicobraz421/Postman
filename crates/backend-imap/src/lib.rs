//! Путь IMAP + SMTP — плагин ядра (ADR 0004, S7-02б): учётная запись «стандарты» и почта Exchange (ADR 0003,
//! S7-04б) — своя синхронизация с тем же договором команд и событий, что у JMAP (`postman_core::sync::Command`,
//! `Event`), на том же кэше. Интерфейс не знает, какой сервер у учётной записи: адрес `imaps://…` или `imap://…`
//! вместо адреса JMAP — и только.
//!
//! - **Папки** — `LIST` с ролями (SPECIAL-USE, иначе по имени); ключ папки в кэше — `f<хэш имени>` (в имени IMAP
//!   бывает `/`, а он — разделитель аккаунта в ключах кэша).
//! - **Письма** — ключ `f<хэш>.<UIDVALIDITY>.<UID>`: письмо, перенесённое на сервере, — новое письмо в другой папке
//!   (ADR 0003). При первом входе — последние [`INITIAL`] писем папки; дальше — по состоянию папки
//!   (`postman_imap::state`): новые с `UIDNEXT`, флаги после `HIGHESTMODSEQ` (без CONDSTORE — все), удалённые —
//!   сверкой UID.
//! - **Push** — IDLE на «Входящих» отдельным соединением; остальные папки — опросом ([`POLL`]).
//! - **Тело** — письмо целиком (`BODY.PEEK[]`), разбор MIME — в воркере-песочнице (`postman_worker::mime`);
//!   письмо целиком лежит в кэше блобом под своим ключом, вложения — `<ключ>#<номер>`.
//! - **Отправка** — SMTP submission, копия — `APPEND` в «Отправленные». «Отменить» — письмо держит сама синхронизация
//!   (`Release::After`, S7-04в): до срока его можно вернуть в черновики; закрыли Postman — уходит сразу. «Отправить
//!   позже» — нет (сервер не держит письмо): `Scheduled` с `max_delay: 0` — интерфейс прячет «позже».
//! - Календарь, задачи, занятость и контакты — пути областей рядом (`?<ключ>=https://…` в адресе: `dav=` — плагин
//!   `postman-backend-dav`, S7-05б; `postman_core::backend::Areas`): `Features` — по тому, что нашлось на сервере.
//!   Без них, как и правил и автоответа, — нет.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use postman_cache::{AccountRow, Addr, AttachmentRow, BodyRow, Cache, EmailRow, MailboxRow, Rights, in_profile, scoped, unscoped};
use postman_compose::{Address, Draft};
use postman_imap::state::{FolderState, Plan};
use postman_imap::{Config, Header, Role, Security, Session, flags, smtp, uid_set};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use postman_core::backend::Login;
use postman_core::backend::{AreaCtx, AreaPlugin, Areas, BoxFuture, MailPlugin, Spawn};
use postman_core::model::{Auth, Feature, Features};
use postman_core::sync::{Account, Command, ErrorKind, Event, Index, Sender, Stage, Status, SyncError};

/// Писем папки при первом входе — не больше (дальше — «Загрузить ещё»).
pub const INITIAL: usize = 500;
/// Догрузка при прокрутке.
const PAGE: usize = 500;
/// Опрос остальных папок; «Входящие» — по IDLE.
pub const POLL: Duration = Duration::from_secs(120);
/// IDLE упал — новая попытка через столько.
const IDLE_RETRY: Duration = Duration::from_secs(30);
/// Найдено поиском в папке — не больше.
const SEARCH_LIMIT: usize = 200;
/// Сколько ждать воркер на разборе письма.
const WORKER_TIMEOUT: Duration = Duration::from_secs(10);

/// Учётная запись IMAP: адрес (он же «От»), IMAP и SMTP.
#[derive(Clone, Debug)]
pub struct ImapAccount {
    pub email: String,
    pub imap: Config,
    pub smtp: Config,
    /// Адреса путей областей по ключам (`dav` → `https://узел[/путь]`, S7-05б); нет — только почта.
    pub areas: Vec<(String, String)>,
}

/// Адрес учётной записи IMAP: `imaps://imap.host[:порт][?smtp=smtp.host[:порт]][&dav=https://dav.host]` (TLS) или
/// `imap://…` (STARTTLS). Кроме `smtp=` — адреса путей областей: `<ключ>=https://…` (`dav` — CalDAV и CardDAV).
pub fn is_imap_url(url: &str) -> bool {
    let u = url.trim().to_ascii_lowercase();
    u.starts_with("imaps://") || u.starts_with("imap://")
}

fn host_port(s: &str, default: u16) -> Option<(String, u16)> {
    let s = s.trim().trim_end_matches('/');
    if s.is_empty() {
        return None;
    }
    match s.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => Some((h.to_owned(), p.parse().ok()?)),
        _ => Some((s.to_owned(), default)),
    }
}

impl ImapAccount {
    /// Из адреса сервера и входа; SMTP — `?smtp=`, иначе тот же узел (465 у `imaps://`, 587 у `imap://`).
    pub fn from_url(url: &str, user: &str, password: &str) -> Option<ImapAccount> {
        let url = url.trim();
        let (tls, rest) = if url.len() > 8 && url[..8].eq_ignore_ascii_case("imaps://") {
            (true, &url[8..])
        } else if url.len() > 7 && url[..7].eq_ignore_ascii_case("imap://") {
            (false, &url[7..])
        } else {
            return None;
        };
        let (server, query) = rest.split_once('?').unwrap_or((rest, ""));
        let (host, port) = host_port(server, if tls { 993 } else { 143 })?;
        let smtp_at = query.split('&').find_map(|kv| kv.strip_prefix("smtp=")).and_then(|s| host_port(s, if tls { 465 } else { 587 }));
        let (smtp_host, smtp_port) = smtp_at.unwrap_or((host.clone(), if tls { 465 } else { 587 }));
        let security = |port: u16, implicit: u16| if port == implicit { Security::Tls } else { Security::StartTls };
        let imap = Config {
            host,
            port,
            security: if tls { security(port, 993) } else { Security::StartTls },
            user: user.to_owned(),
            password: password.to_owned(),
        };
        let smtp =
            Config { host: smtp_host, port: smtp_port, security: security(smtp_port, 465), user: user.to_owned(), password: password.to_owned() };
        let areas = query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .filter(|(k, _)| *k != "smtp" && !k.is_empty())
            .map(|(k, v)| (k.to_owned(), v.trim().to_owned()))
            .filter(|(_, v)| v.starts_with("https://") || v.starts_with("http://"))
            .collect();
        Some(ImapAccount { email: user.to_owned(), imap, smtp, areas })
    }

    /// Адрес пути областей с ключом `key`.
    pub fn area(&self, key: &str) -> Option<&str> {
        self.areas.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    /// Из учётной записи договора (адрес и пароль); вход по токену у IMAP нет.
    pub fn of(a: &Account) -> Option<ImapAccount> {
        match &a.auth {
            Auth::Basic { user, password } => ImapAccount::from_url(&a.url, user, password),
            Auth::Bearer(_) => None,
        }
    }
}

/// Узел адреса IMAP (`imaps://узел[:порт][?…]` → `узел`) — там ищут пути областей.
fn host_of(url: &str) -> Option<String> {
    let (_, rest) = url.split_once("://")?;
    let server = rest.split('?').next().unwrap_or_default().trim_end_matches('/');
    let host = server.rsplit_once(':').map_or(server, |(h, _)| h);
    (!host.is_empty()).then(|| host.to_owned())
}

/// Плагин пути IMAP + SMTP для реестра (`postman_core::backend::Registry`): адреса `imaps://` и `imap://`.
#[derive(Debug, Default, Clone, Copy)]
pub struct ImapPlugin;

impl MailPlugin for ImapPlugin {
    fn name(&self) -> &'static str {
        "imap"
    }

    fn claims(&self, url: &str) -> bool {
        is_imap_url(url)
    }

    /// IMAP на том же узле (S7-04в): адрес без схемы и пути, порт 993 отвечает → `imaps://узел`. Явный порт или
    /// путь — это адрес JMAP; угадывать не будем.
    fn probe<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            let host = url.split_once("://").map_or(url, |(_, r)| r);
            let has_path = host.contains('/') || host.contains('?');
            let host = host.split(['/', '?']).next().unwrap_or_default();
            if host.is_empty() || has_path || host.contains(':') {
                return None;
            }
            let open = tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect((host, 993))).await;
            matches!(open, Ok(Ok(_))).then(|| format!("imaps://{host}"))
        })
    }

    fn area_host(&self, url: &str) -> Option<String> {
        host_of(url)
    }

    fn has_area(&self, url: &str, key: &str) -> bool {
        let query = url.split_once('?').map_or("", |(_, q)| q);
        query.split('&').any(|kv| kv.strip_prefix(key).is_some_and(|r| r.starts_with('=')))
    }

    fn with_area(&self, url: &str, key: &str, area: &str) -> String {
        format!("{url}{}{key}={area}", if url.contains('?') { '&' } else { '?' })
    }

    fn spawn(&self, s: Spawn) -> UnboundedSender<Command> {
        match ImapAccount::of(&s.account) {
            Some(a) => spawn(a, s.profile, s.cache, s.index, s.events, s.repaint, s.areas),
            None => {
                let detail = format!("not an IMAP account: {}", s.account.url);
                let _ = s.events.send(Event::Error(SyncError { kind: ErrorKind::Protocol, stage: Stage::Start, detail }));
                (s.repaint)();
                unbounded_channel().0
            }
        }
    }
}

/// Папка сервера в кэше.
#[derive(Debug, Clone)]
struct Folder {
    /// Ключ папки в кэше (`scoped(аккаунт, f<хэш>)`).
    key: String,
    /// `f<хэш>` — часть ключей её писем.
    id: String,
    raw: String,
    name: String,
    parent: Option<String>,
    role: Option<Role>,
    selectable: bool,
}

fn role_rank(role: Option<Role>) -> u32 {
    match role {
        Some(Role::Inbox) => 0,
        Some(Role::Drafts) => 1,
        Some(Role::Sent) => 2,
        Some(Role::Archive) => 3,
        Some(Role::Junk) => 4,
        Some(Role::Trash) => 5,
        None => 6,
    }
}

/// FNV-1a 64: устойчивый короткий ключ папки по её имени на сервере.
fn folder_id(raw: &str) -> String {
    let h = raw.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    format!("f{h:016x}")
}

/// Ключ письма: `f<хэш>.<UIDVALIDITY>.<UID>` в аккаунте `scope`.
fn email_key(scope: Option<&str>, folder: &str, validity: u32, uid: u32) -> String {
    scoped(scope, &format!("{folder}.{validity}.{uid}"))
}

/// Ключ письма → (папка `f<хэш>`, UIDVALIDITY, UID); вложение `<ключ>#<номер>` — тоже.
fn parse_key(key: &str) -> Option<(&str, u32, u32)> {
    let id = unscoped(key).1;
    let id = id.split_once('#').map_or(id, |(k, _)| k);
    let mut it = id.split('.');
    let (f, v, u) = (it.next()?, it.next()?.parse().ok()?, it.next()?.parse().ok()?);
    (it.next().is_none() && f.starts_with('f')).then_some((f, v, u))
}

/// Строка письма из заголовка IMAP.
/// Ключ цепочки письма: начало `References`, иначе `In-Reply-To`, иначе своё `Message-ID` (S7-04в). Без них —
/// само письмо.
fn thread_key(scope: Option<&str>, h: &Header, key: &str) -> String {
    let root = h.references.first().cloned().or_else(|| h.in_reply_to.clone()).or_else(|| h.message_id.clone());
    match root.map(|r| r.trim().to_ascii_lowercase()).filter(|r| !r.is_empty()) {
        Some(r) => scoped(scope, &format!("t{}", &folder_id(&r)[1..])),
        None => key.to_owned(),
    }
}

fn row(scope: Option<&str>, f: &Folder, validity: u32, h: &Header) -> EmailRow {
    let key = email_key(scope, &f.id, validity, h.uid);
    let mut r = EmailRow {
        thread_id: thread_key(scope, h, &key),
        id: key,
        mailbox_ids: vec![f.key.clone()],
        received_at: h.received_at,
        subject: h.subject.clone(),
        from_name: h.from_name.clone(),
        from_addr: h.from_addr.clone(),
        preview: String::new(),
        seen: false,
        flagged: false,
        has_attachment: h.has_attachment,
        size: u64::from(h.size),
        keywords: Vec::new(),
    };
    set_flags(&mut r, &h.flags);
    r
}

/// Флаги сервера → поля и ключевые слова строки (всё прежнее — заново).
fn set_flags(r: &mut EmailRow, server: &[String]) {
    r.seen = false;
    r.flagged = false;
    r.keywords.clear();
    for k in server.iter().filter_map(|f| flags::keyword(f)) {
        r.set_keyword(&k, true);
    }
}

/// Все письма папки в кэше (`page` выделяет память под предел — поэтому сначала число).
fn letters(cache: &Cache, folder: &str) -> postman_cache::Result<Vec<EmailRow>> {
    let n = cache.count(folder)?;
    cache.page(folder, 0, n)
}

fn error(stage: Stage, e: &postman_imap::Error) -> SyncError {
    use postman_imap::Error as E;
    let kind = match e {
        E::Auth(_) => ErrorKind::Auth,
        E::Io(_) | E::Tls(_) | E::Timeout => ErrorKind::Network,
        E::Protocol(_) => ErrorKind::Protocol,
        E::Imap(_) | E::Smtp(_) => ErrorKind::Server,
    };
    SyncError { kind, stage, detail: e.to_string() }
}

fn db(e: postman_cache::Error) -> postman_imap::Error {
    postman_imap::Error::Protocol(format!("cache: {e}"))
}

/// Синхронизация учётной записи IMAP: свой поток, как у JMAP (`postman_core::sync::spawn_thread`). `areas` — пути
/// областей реестра: подключаются те, чьи адреса есть у учётной записи.
pub fn spawn(
    account: ImapAccount,
    profile: Option<String>,
    cache: Cache,
    index: Index,
    events: mpsc::Sender<Event>,
    repaint: impl Fn() + Send + 'static,
    areas: Vec<Arc<dyn AreaPlugin>>,
) -> UnboundedSender<Command> {
    let (tx, rx) = unbounded_channel();
    postman_core::sync::spawn_thread("sync-imap", events, Box::new(repaint), move |notify| async move {
        run(account, profile, cache, index, rx, areas, &|e| notify.send(e)).await;
    });
    tx
}

struct Imap {
    account: ImapAccount,
    profile: Option<String>,
    scope: Option<String>,
    session: Option<Session>,
    folders: Vec<Folder>,
    cache: Cache,
    _index: Index,
    worker: Option<postman_worker::Client>,
    /// Письма в окне «Отменить» (S7-04в): уходят по сроку, отмена возвращает их в черновики.
    held: Vec<Held>,
    next_hold: u64,
    /// Календарь, задачи и контакты — пути областей (S7-05б): ключ и подключение; пусто — у учётной записи только
    /// почта.
    areas: Vec<(&'static str, Box<dyn Areas>)>,
}

/// Письмо, которое держит синхронизация до срока окна «Отменить».
struct Held {
    /// Ключ отправки (`<аккаунт>/hold.<n>`): по нему интерфейс отменяет её (`Command::Unsend`).
    id: String,
    draft: Draft,
    replace: Option<String>,
    at: tokio::time::Instant,
}

async fn run(
    account: ImapAccount,
    profile: Option<String>,
    cache: Cache,
    index: Index,
    mut rx: UnboundedReceiver<Command>,
    plugins: Vec<Arc<dyn AreaPlugin>>,
    notify: &impl Fn(Event),
) {
    notify(Event::Status(Status::Connecting { url: format!("{}:{}", account.imap.host, account.imap.port) }));
    let session = match Session::connect(&account.imap).await {
        Ok(s) => s,
        Err(e) => return notify(Event::Error(error(Stage::Connect, &e))),
    };
    let scope = in_profile(profile.as_deref(), None);
    let mut me = Imap {
        account,
        profile,
        scope,
        session: Some(session),
        folders: Vec::new(),
        cache,
        _index: index,
        worker: None,
        held: Vec::new(),
        next_hold: 0,
        areas: Vec::new(),
    };
    notify(Event::Status(Status::SignedIn { user: me.account.email.clone() }));
    let login = Login { user: me.account.imap.user.clone(), password: me.account.imap.password.clone(), email: me.account.email.clone() };
    for (key, url) in me.account.areas.clone() {
        let Some(plugin) = plugins.iter().find(|p| p.key() == key) else {
            tracing::warn!("imap: no area backend for {key}=");
            continue;
        };
        match plugin.connect(&url, &login).await {
            Ok(a) => me.areas.push((plugin.key(), a)),
            // Без календаря почта всё равно работает: ошибка — тостом календаря, области скрыты.
            Err(e) => {
                tracing::warn!("{key}: {e}");
                notify(Event::CalendarFailed(e.to_string()));
            }
        }
    }
    let address = Address::new("", &me.account.email);
    if me.profile.is_none() {
        // Основная учётная запись IMAP: почта и то, что нашлось по CalDAV/CardDAV (S7-03а «Нет у сервера — нет на
        // экране»). Ключевые слова (категории, сроки, «Ждём») — если сервер их хранит: `PERMANENTFLAGS \*` у «Входящих».
        let keywords = match me.session().await {
            Ok(s) => s.select("INBOX").await.is_ok_and(|sel| sel.keywords),
            Err(_) => false,
        };
        let areas = me.areas.iter().fold(Features::NONE, |f, (_, a)| f.union(a.features()));
        notify(Event::Features(areas.with(Feature::Keywords, keywords)));
        notify(Event::Me { address: address.clone(), signature: String::new() });
    }
    notify(Event::Senders(vec![Sender { account: me.scope.clone(), address, signature: String::new() }]));
    // Сервер не держит письма: «Отправить позже» нет (интерфейс прячет его по `max_delay: 0`).
    notify(Event::Scheduled { list: Vec::new(), max_delay: 0 });
    let rows: Vec<AccountRow> = match &me.profile {
        Some(p) => vec![AccountRow { id: p.clone(), name: me.account.email.clone(), is_personal: true, read_only: false }],
        None => Vec::new(),
    };
    if let Err(e) = me.cache.replace_accounts_of(me.profile.as_deref(), &rows) {
        return notify(Event::Error(error(Stage::Load, &db(e))));
    }
    if let Err(e) = me.refresh_all(notify).await {
        let stop = e.kind == ErrorKind::Auth;
        notify(Event::Error(e));
        if stop {
            return;
        }
    }
    notify(Event::Status(Status::Synced));
    let (idle_tx, mut idle_rx) = unbounded_channel::<()>();
    tokio::spawn(idle_loop(me.account.imap.clone(), idle_tx));
    let mut poll = tokio::time::interval(POLL);
    poll.tick().await;
    loop {
        tokio::select! {
            biased;
            cmd = rx.recv() => {
                let Some(cmd) = cmd else {
                    // Интерфейс закрылся: письма в окне «Отменить» — отправить сейчас, а не потерять.
                    me.release_held(true, notify).await;
                    return;
                };
                if let Err(e) = me.handle(cmd, notify).await {
                    tracing::warn!("imap command: {e}");
                    if matches!(e, postman_imap::Error::Io(_) | postman_imap::Error::Timeout | postman_imap::Error::Tls(_)) {
                        me.session = None;
                    }
                    notify(Event::Error(error(Stage::Command, &e)));
                }
            }
            Some(()) = idle_rx.recv() => {
                while idle_rx.try_recv().is_ok() {}
                if let Some(inbox) = me.folder_by_role(Role::Inbox).cloned() {
                    match me.refresh(&inbox).await {
                        Ok(true) => notify(Event::EmailsChanged),
                        Ok(false) => {}
                        Err(e) => notify(Event::Error(error(Stage::Refresh, &e))),
                    }
                    notify(Event::MailboxesChanged);
                }
            }
            () = held_until(me.held.iter().map(|h| h.at).min()) => me.release_held(false, notify).await,
            _ = poll.tick() => {
                if let Err(e) = me.refresh_all(notify).await {
                    notify(Event::Error(e));
                }
                // Области без push (CalDAV): изменения календарей — опросом (ctag и sync-token).
                for (key, a) in me.areas.iter_mut() {
                    match a.changed().await {
                        Ok(true) => notify(Event::CalendarChanged),
                        Ok(false) => {}
                        Err(e) => tracing::debug!("{key} poll: {e}"),
                    }
                }
            }
        }
    }
}

/// Срок ближайшего удержанного письма; нет — никогда.
async fn held_until(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// IDLE на «Входящих» отдельным соединением: изменение — сигнал циклу синхронизации.
async fn idle_loop(cfg: Config, tx: UnboundedSender<()>) {
    loop {
        let attempt = async {
            let mut s = Session::connect(&cfg).await?;
            if !s.has("IDLE") {
                return Ok::<bool, postman_imap::Error>(false);
            }
            s.select("INBOX").await?;
            loop {
                let (next, changed) = s.idle(postman_imap::IDLE_MAX).await?;
                s = next;
                if changed && tx.send(()).is_err() {
                    return Ok(true);
                }
            }
        };
        match attempt.await {
            // IDLE нет — «Входящие» опрашиваются вместе с остальными папками.
            Ok(false) => return,
            Ok(true) => return,
            Err(e) => tracing::debug!("imap idle: {e}"),
        }
        if tx.is_closed() {
            return;
        }
        tokio::time::sleep(IDLE_RETRY).await;
    }
}

impl Imap {
    async fn session(&mut self) -> postman_imap::Result<&mut Session> {
        if self.session.is_none() {
            self.session = Some(Session::connect(&self.account.imap).await?);
        }
        self.session.as_mut().ok_or_else(|| postman_imap::Error::Protocol("no session".into()))
    }

    fn scope(&self) -> Option<&str> {
        self.scope.as_deref()
    }

    fn folder_by_role(&self, role: Role) -> Option<&Folder> {
        self.folders.iter().find(|f| f.role == Some(role))
    }

    fn folder_by_key(&self, key: &str) -> Option<&Folder> {
        self.folders.iter().find(|f| f.key == key)
    }

    fn folder_by_id(&self, id: &str) -> Option<&Folder> {
        self.folders.iter().find(|f| f.id == id)
    }

    /// Папки заново и каждая сверена; итог — событиями.
    async fn refresh_all(&mut self, notify: &impl Fn(Event)) -> Result<(), SyncError> {
        self.load_folders().await.map_err(|e| error(Stage::Load, &e))?;
        notify(Event::MailboxesChanged);
        let mut changed = false;
        for f in self.folders.clone().iter().filter(|f| f.selectable) {
            notify(Event::Status(Status::Loading { mailbox: f.name.clone() }));
            match self.refresh(f).await {
                Ok(c) => changed |= c,
                Err(e) => {
                    let e = error(Stage::Refresh, &e);
                    if e.kind == ErrorKind::Auth || e.kind == ErrorKind::Network {
                        self.session = None;
                        return Err(e);
                    }
                    tracing::warn!("imap folder {}: {e}", f.name);
                }
            }
        }
        self.write_folders().map_err(|e| error(Stage::Load, &e))?;
        notify(Event::MailboxesChanged);
        if changed {
            notify(Event::EmailsChanged);
        }
        Ok(())
    }

    async fn load_folders(&mut self) -> postman_imap::Result<()> {
        let list = self.session().await?.folders().await?;
        let scope = self.scope.clone();
        let key = |raw: &str| scoped(scope.as_deref(), &folder_id(raw));
        self.folders = list
            .into_iter()
            .map(|f| Folder {
                key: key(&f.raw),
                id: folder_id(&f.raw),
                parent: f.parent.as_deref().map(key),
                // `INBOX` — служебное имя IMAP; как у серверов JMAP — «Inbox».
                name: if f.role == Some(Role::Inbox) { "Inbox".into() } else { f.name.clone() },
                raw: f.raw,
                role: f.role,
                selectable: f.selectable,
            })
            .collect();
        self.write_folders()
    }

    /// Папки — в кэш, счётчики — по письмам в кэше.
    fn write_folders(&self) -> postman_imap::Result<()> {
        let rows: Vec<MailboxRow> = self
            .folders
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let letters = letters(&self.cache, &f.key).unwrap_or_default();
                MailboxRow {
                    id: f.key.clone(),
                    parent_id: f.parent.clone(),
                    name: f.name.clone(),
                    role: f.role.map(|r| r.jmap().to_owned()),
                    // Как у Outlook и JMAP: «Входящие», «Черновики», «Отправленные», «Архив», «Спам», «Удалённые», затем прочие.
                    sort_order: role_rank(f.role) * 1000 + u32::try_from(i).unwrap_or(999).min(999),
                    total: u32::try_from(letters.len()).unwrap_or(u32::MAX),
                    unread: u32::try_from(letters.iter().filter(|r| !r.seen).count()).unwrap_or(u32::MAX),
                    rights: Rights::ALL,
                }
            })
            .collect();
        self.cache.replace_mailboxes(self.scope(), &rows).map_err(db)
    }

    /// Сверить папку с сервером; `true` — письма в кэше изменились.
    async fn refresh(&mut self, f: &Folder) -> postman_imap::Result<bool> {
        let scope = self.scope.clone();
        let state_key = scoped(scope.as_deref(), &format!("imap.{}", f.id));
        let old = self.cache.state(&state_key).map_err(db)?.and_then(|s| FolderState::decode(&s));
        let session = self.session().await?;
        let sel = session.select(&f.raw).await?;
        let validity = sel.state.uid_validity;
        let here = letters(&self.cache, &f.key).map_err(db)?;
        let mut upsert: Vec<EmailRow> = Vec::new();
        let mut gone: Vec<String> = Vec::new();
        match postman_imap::state::plan(old, sel.state) {
            Plan::Same => {}
            Plan::Reload => {
                gone.extend(here.iter().map(|r| r.id.clone()));
                let session = self.session().await?;
                let uids = session.uids().await?;
                let newest = &uids[uids.len().saturating_sub(INITIAL)..];
                if !newest.is_empty() {
                    let headers = session.headers(&uid_set(newest)).await?;
                    upsert.extend(headers.iter().map(|h| row(scope.as_deref(), f, validity, h)));
                }
                gone.retain(|g| !upsert.iter().any(|r| &r.id == g));
            }
            Plan::Delta { from, modseq } => {
                let session = self.session().await?;
                let server: HashSet<u32> = session.uids().await?.into_iter().collect();
                for r in &here {
                    let ok = parse_key(&r.id).is_some_and(|(fid, v, u)| fid == f.id && v == validity && server.contains(&u));
                    if !ok {
                        gone.push(r.id.clone());
                    }
                }
                if sel.state.uid_next > from {
                    let headers = session.headers(&format!("{from}:*")).await?;
                    upsert.extend(headers.iter().filter(|h| h.uid >= from).map(|h| row(scope.as_deref(), f, validity, h)));
                }
                let changes = match modseq {
                    Some(m) => session.changed_since(m).await?,
                    None => session.all_flags().await?,
                };
                let by_key: HashMap<&str, &EmailRow> = here.iter().map(|r| (r.id.as_str(), r)).collect();
                for c in changes {
                    let key = email_key(scope.as_deref(), &f.id, validity, c.uid);
                    if let Some(old) = by_key.get(key.as_str()) {
                        let mut r = (*old).clone();
                        set_flags(&mut r, &c.flags);
                        if &r != *old {
                            upsert.push(r);
                        }
                    }
                }
            }
        }
        if !gone.is_empty() {
            self.cache.delete_emails(&gone).map_err(db)?;
        }
        if !upsert.is_empty() {
            self.cache.upsert_emails(&upsert).map_err(db)?;
        }
        self.cache.set_state(&state_key, &sel.state.encode()).map_err(db)?;
        Ok(!gone.is_empty() || !upsert.is_empty())
    }

    /// Письмо по ключу: папка и UID; папку — выбрать.
    async fn locate(&mut self, key: &str) -> postman_imap::Result<(Folder, u32)> {
        let (fid, validity, uid) = parse_key(key).ok_or_else(|| postman_imap::Error::Protocol(format!("not an imap key: {key}")))?;
        let f = self.folder_by_id(fid).cloned().ok_or_else(|| postman_imap::Error::Protocol(format!("no folder for {key}")))?;
        let sel = self.session().await?.select(&f.raw).await?;
        if sel.state.uid_validity != validity {
            return Err(postman_imap::Error::Protocol(format!("folder {} was reset on the server", f.name)));
        }
        Ok((f, uid))
    }

    /// Письмо целиком: из кэша (блоб под ключом письма) или с сервера.
    async fn raw(&mut self, key: &str) -> postman_imap::Result<Vec<u8>> {
        let key = key.split_once('#').map_or(key, |(k, _)| k);
        if let Some(b) = self.cache.blob(key).map_err(db)? {
            return Ok(b);
        }
        let (_, uid) = self.locate(key).await?;
        let raw = self.session().await?.message(uid).await?.ok_or_else(|| postman_imap::Error::Protocol(format!("no message {key}")))?;
        self.cache.put_blob(key, &raw).map_err(db)?;
        Ok(raw)
    }

    /// Работа воркера-песочницы — в отдельном потоке (он блокирует).
    async fn in_worker<T: Send + 'static>(
        &mut self,
        f: impl FnOnce(&mut postman_worker::Client) -> Result<T, String> + Send + 'static,
    ) -> postman_imap::Result<T> {
        let mut worker = self.worker.take().unwrap_or_else(|| postman_worker::Client::new(postman_worker::Client::default_exe(), WORKER_TIMEOUT));
        let (worker, out) = tokio::task::spawn_blocking(move || {
            let out = f(&mut worker);
            (worker, out)
        })
        .await
        .map_err(|e| postman_imap::Error::Protocol(format!("worker task: {e}")))?;
        self.worker = Some(worker);
        out.map_err(|e| postman_imap::Error::Protocol(format!("worker: {e}")))
    }

    async fn body(&mut self, key: &str) -> postman_imap::Result<()> {
        let raw = self.raw(key).await?;
        let m = self.in_worker(move |w| w.mime(&raw)).await?;
        let addrs = |v: Vec<postman_worker::mime::Addr>| v.into_iter().map(|a| Addr { name: a.name, email: a.email }).collect();
        let body = BodyRow {
            to: addrs(m.to),
            cc: addrs(m.cc),
            reply_to: addrs(m.reply_to),
            message_id: m.message_id,
            references: m.references,
            html: m.html,
            text: m.text.clone(),
            attachments: m
                .parts
                .into_iter()
                .map(|p| AttachmentRow {
                    blob_id: format!("{key}#{}", p.index),
                    name: p.name,
                    mime: p.mime,
                    size: p.size,
                    cid: p.cid,
                    inline: p.inline,
                })
                .collect(),
            blob_id: Some(key.to_owned()),
        };
        self.cache.put_body(key, &body).map_err(db)?;
        // Превью строки — из текста письма, как у JMAP.
        if let Some(mut r) = self.cache.rows_by_ids(&[key.to_owned()]).map_err(db)?.pop()
            && r.preview.is_empty()
            && let Some(text) = m.text
        {
            r.preview = text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(256).collect();
            self.cache.upsert_emails(&[r]).map_err(db)?;
        }
        Ok(())
    }

    async fn blob(&mut self, blob_id: &str) -> postman_imap::Result<Vec<u8>> {
        if let Some(b) = self.cache.blob(blob_id).map_err(db)? {
            return Ok(b);
        }
        let raw = self.raw(blob_id).await?;
        let Some((_, index)) = blob_id.split_once('#') else { return Ok(raw) };
        let index: u32 = index.parse().map_err(|_| postman_imap::Error::Protocol(format!("bad blob {blob_id}")))?;
        let data = self.in_worker(move |w| w.mime_part(&raw, index)).await?;
        self.cache.put_blob(blob_id, &data).map_err(db)?;
        Ok(data)
    }

    /// Ключи писем → по папкам: (папка, UID).
    fn by_folder(&self, keys: &[String]) -> Vec<(Folder, Vec<u32>)> {
        let mut out: Vec<(Folder, Vec<u32>)> = Vec::new();
        for k in keys {
            let Some((fid, _, uid)) = parse_key(k) else { continue };
            let Some(f) = self.folder_by_id(fid) else { continue };
            match out.iter_mut().find(|(g, _)| g.id == f.id) {
                Some((_, v)) => v.push(uid),
                None => out.push((f.clone(), vec![uid])),
            }
        }
        out
    }

    async fn set_keywords(&mut self, keys: &[String], add: &[String], remove: &[String]) -> postman_imap::Result<()> {
        let add: Vec<String> = add.iter().filter_map(|k| flags::flag(k)).collect();
        let remove: Vec<String> = remove.iter().filter_map(|k| flags::flag(k)).collect();
        for (f, uids) in self.by_folder(keys) {
            let session = self.session().await?;
            session.select(&f.raw).await?;
            session.store(&uid_set(&uids), &add, true).await?;
            session.store(&uid_set(&uids), &remove, false).await?;
        }
        Ok(())
    }

    async fn move_letters(&mut self, keys: &[String], to: &Folder) -> postman_imap::Result<()> {
        for (f, uids) in self.by_folder(keys) {
            if f.id == to.id {
                continue;
            }
            let session = self.session().await?;
            session.select(&f.raw).await?;
            session.move_to(&uid_set(&uids), &to.raw).await?;
        }
        Ok(())
    }

    async fn destroy(&mut self, keys: &[String]) -> postman_imap::Result<()> {
        for (f, uids) in self.by_folder(keys) {
            let session = self.session().await?;
            session.select(&f.raw).await?;
            session.delete(&uid_set(&uids)).await?;
        }
        Ok(())
    }

    /// Сверить папки по ключам и сказать интерфейсу.
    async fn after(&mut self, folders: &[String], notify: &impl Fn(Event)) {
        let list: Vec<Folder> = self.folders.iter().filter(|f| folders.contains(&f.key) || folders.contains(&f.id)).cloned().collect();
        let mut changed = false;
        for f in &list {
            match self.refresh(f).await {
                Ok(c) => changed |= c,
                Err(e) => tracing::warn!("imap refresh {}: {e}", f.name),
            }
        }
        let _ = self.write_folders();
        notify(Event::MailboxesChanged);
        if changed {
            notify(Event::EmailsChanged);
        }
    }

    /// Письмо — в байты (S/MIME «Конфиденциально» — как у JMAP); вложения пересылки — из кэша или с сервера.
    async fn message_bytes(&mut self, draft: &mut Draft, message_id: &str) -> postman_imap::Result<Vec<u8>> {
        for a in draft.attachments.iter_mut().filter(|a| a.data.is_empty()) {
            if let Some(blob) = a.blob_id.clone() {
                a.data = self.blob(&blob).await?;
            }
        }
        let now = jiff::Timestamp::now().as_second();
        let raw = postman_compose::build(draft, now, message_id).map_err(|e| postman_imap::Error::Protocol(format!("building the message: {e}")))?;
        if draft.encrypt_to.is_empty() {
            return Ok(raw);
        }
        postman_core::smime::seal_outgoing(&raw, &draft.encrypt_to).map_err(|e| postman_imap::Error::Protocol(format!("{e:?}")))
    }

    async fn send(&mut self, mut draft: Draft, replace: Option<String>) -> postman_imap::Result<()> {
        let now = jiff::Timestamp::now().as_second();
        let id = postman_compose::new_message_id(&draft.from, now);
        let raw = self.message_bytes(&mut draft, &id).await?;
        let rcpt: Vec<String> = draft.to.iter().chain(&draft.cc).chain(&draft.bcc).map(|a| a.email.clone()).collect();
        smtp::submit(&self.account.smtp, &draft.from.email, &rcpt, &raw).await?;
        if let Some(sent) = self.folder_by_role(Role::Sent).cloned() {
            self.session().await?.append(&sent.raw, &["\\Seen"], &raw).await?;
        }
        if let Some(old) = replace.filter(|k| parse_key(k).is_some()) {
            self.destroy(&[old]).await?;
        }
        Ok(())
    }

    /// Черновик — в «Черновики» (`\Draft`), прежний — удалить; ключ нового — по его Message-ID.
    async fn save_draft(&mut self, mut draft: Draft, replace: Option<String>) -> postman_imap::Result<String> {
        let drafts = self.folder_by_role(Role::Drafts).cloned().ok_or_else(|| postman_imap::Error::Protocol("no drafts folder".into()))?;
        let now = jiff::Timestamp::now().as_second();
        let id = postman_compose::new_message_id(&draft.from, now);
        // Черновик «Конфиденциально» на сервере — открытым: шифруется только отправляемое.
        draft.encrypt_to.clear();
        let raw = self.message_bytes(&mut draft, &id).await?;
        // UID нового черновика — первый с прежнего `UIDNEXT` (поиск по Message-ID серверы выполняют по-разному).
        let session = self.session().await?;
        let next = session.select(&drafts.raw).await?.state.uid_next;
        session.append(&drafts.raw, &["\\Draft", "\\Seen"], &raw).await?;
        let sel = session.select(&drafts.raw).await?;
        let found = session.search(&format!("UID {next}:*")).await?;
        let uid = found.into_iter().filter(|&u| u >= next).max().ok_or_else(|| postman_imap::Error::Protocol("saved draft not found".into()))?;
        if let Some(old) = replace.filter(|k| parse_key(k).is_some()) {
            self.destroy(&[old]).await?;
        }
        Ok(email_key(self.scope(), &drafts.id, sel.state.uid_validity, uid))
    }

    /// Поиск на сервере: в папке или во всех; найденное, которого нет в кэше, — в кэш.
    async fn search(&mut self, text: &str, mailbox: Option<&str>) -> postman_imap::Result<Vec<String>> {
        self.query(&format!("CHARSET UTF-8 TEXT {}", postman_imap::quote(text)), mailbox).await
    }

    /// Сколько писем по условию `SEARCH` в папке или во всех.
    async fn count(&mut self, query: &str, mailbox: Option<&str>) -> postman_imap::Result<usize> {
        let folders: Vec<Folder> = match mailbox {
            Some(k) => self.folder_by_key(k).cloned().into_iter().collect(),
            None => self.folders.iter().filter(|f| f.selectable).cloned().collect(),
        };
        let mut n = 0;
        for f in folders {
            let session = self.session().await?;
            session.select(&f.raw).await?;
            n += session.search(query).await?.len();
        }
        Ok(n)
    }

    /// Письма по условию `SEARCH` в папке или во всех; найденное, которого нет в кэше, — в кэш.
    async fn query(&mut self, query: &str, mailbox: Option<&str>) -> postman_imap::Result<Vec<String>> {
        let folders: Vec<Folder> = match mailbox {
            Some(k) => self.folder_by_key(k).cloned().into_iter().collect(),
            None => self.folders.iter().filter(|f| f.selectable).cloned().collect(),
        };
        let scope = self.scope.clone();
        let mut out = Vec::new();
        for f in folders {
            let session = self.session().await?;
            let sel = session.select(&f.raw).await?;
            let uids = session.search(query).await?;
            let uids = &uids[uids.len().saturating_sub(SEARCH_LIMIT)..];
            let keys: Vec<String> = uids.iter().map(|&u| email_key(scope.as_deref(), &f.id, sel.state.uid_validity, u)).collect();
            let known: HashSet<String> = self.cache.rows_by_ids(&keys).map_err(db)?.into_iter().map(|r| r.id).collect();
            let missing: Vec<u32> = uids.iter().zip(&keys).filter(|(_, k)| !known.contains(*k)).map(|(&u, _)| u).collect();
            if !missing.is_empty() {
                let headers = self.session().await?.headers(&uid_set(&missing)).await?;
                let rows: Vec<EmailRow> = headers.iter().map(|h| row(scope.as_deref(), &f, sel.state.uid_validity, h)).collect();
                self.cache.upsert_emails(&rows).map_err(db)?;
            }
            out.extend(keys);
        }
        Ok(out)
    }

    /// Следующая страница папки: письма старше самого старого в кэше.
    async fn load_more(&mut self, mailbox: &str) -> postman_imap::Result<usize> {
        let f = self.folder_by_key(mailbox).cloned().ok_or_else(|| postman_imap::Error::Protocol(format!("no folder {mailbox}")))?;
        let scope = self.scope.clone();
        let here = letters(&self.cache, &f.key).map_err(db)?;
        let oldest = here.iter().filter_map(|r| parse_key(&r.id).map(|(_, _, u)| u)).min().unwrap_or(u32::MAX);
        let session = self.session().await?;
        let sel = session.select(&f.raw).await?;
        let uids: Vec<u32> = session.uids().await?.into_iter().filter(|&u| u < oldest).collect();
        let page = &uids[uids.len().saturating_sub(PAGE)..];
        if page.is_empty() {
            return Ok(0);
        }
        let headers = session.headers(&uid_set(page)).await?;
        let rows: Vec<EmailRow> = headers.iter().map(|h| row(scope.as_deref(), &f, sel.state.uid_validity, h)).collect();
        self.cache.upsert_emails(&rows).map_err(db)?;
        Ok(rows.len())
    }

    /// Отправить удержанные письма, срок которых вышел (`all` — все).
    async fn release_held(&mut self, all: bool, notify: &impl Fn(Event)) {
        let now = tokio::time::Instant::now();
        let (due, keep): (Vec<Held>, Vec<Held>) = std::mem::take(&mut self.held).into_iter().partition(|h| all || h.at <= now);
        self.held = keep;
        if due.is_empty() {
            return;
        }
        for h in due {
            if let Err(e) = self.send(h.draft, h.replace).await {
                tracing::warn!("imap held send: {e}");
                notify(Event::Error(error(Stage::Command, &e)));
            }
        }
        let folders: Vec<String> = [Role::Sent, Role::Drafts].iter().filter_map(|r| self.folder_by_role(*r).map(|f| f.key.clone())).collect();
        self.after(&folders, notify).await;
    }

    async fn handle(&mut self, cmd: Command, notify: &impl Fn(Event)) -> postman_imap::Result<()> {
        match cmd {
            Command::Body(key) => {
                self.body(&key).await?;
                notify(Event::BodyReady(key));
            }
            Command::Thread(thread) => {
                // Беседа — письма кэша с тем же ключом цепочки (`thread_key`) из всех папок; тела — с сервера.
                let mut rows = self.cache.thread(&thread).map_err(db)?;
                rows.sort_by_key(|r| r.received_at);
                for r in &rows {
                    if self.cache.body(&r.id).map_err(db)?.is_none() {
                        self.body(&r.id).await?;
                    }
                }
                notify(Event::ThreadReady { thread, ids: rows.into_iter().map(|r| r.id).collect() });
            }
            Command::Blob { blob_id, .. } => match self.blob(&blob_id).await {
                Ok(data) => notify(Event::BlobReady { blob_id, data: Arc::new(data) }),
                Err(e) => notify(Event::BlobFailed { blob_id, error: e.to_string() }),
            },
            Command::Keyword { id, keyword, on } => {
                let (add, remove) = if on { (vec![keyword.to_owned()], vec![]) } else { (vec![], vec![keyword.to_owned()]) };
                self.set_keywords(&[id], &add, &remove).await?;
            }
            Command::SetKeywords { ids, add, remove } => self.set_keywords(&ids, &add, &remove).await?,
            Command::Move { id, from, to } => {
                let target = self.folder_by_key(&to).cloned().ok_or_else(|| postman_imap::Error::Protocol(format!("no folder {to}")))?;
                self.move_letters(std::slice::from_ref(&id), &target).await?;
                self.after(&[from, to], notify).await;
            }
            Command::Destroy(id) => {
                let folder = parse_key(&id).map(|(f, _, _)| f.to_owned());
                self.destroy(&[id]).await?;
                self.after(&folder.into_iter().collect::<Vec<_>>(), notify).await;
            }
            Command::Patch { patches, .. } => {
                let mut touched = Vec::new();
                for p in patches {
                    let add: Vec<String> = p.keywords.iter().filter(|(_, on)| *on).map(|(k, _)| k.clone()).collect();
                    let remove: Vec<String> = p.keywords.iter().filter(|(_, on)| !*on).map(|(k, _)| k.clone()).collect();
                    self.set_keywords(std::slice::from_ref(&p.id), &add, &remove).await?;
                    // Перенос: папка, куда письмо добавлено («Архив» — роль archive).
                    let to = p.mailboxes.iter().find(|(_, on)| *on).and_then(|(k, _)| {
                        if k == postman_core::lifecycle::ARCHIVE {
                            self.folder_by_role(Role::Archive).cloned()
                        } else {
                            self.folder_by_key(k).cloned()
                        }
                    });
                    if let Some(to) = to {
                        self.move_letters(std::slice::from_ref(&p.id), &to).await?;
                        touched.push(to.key.clone());
                    }
                    if let Some((f, _, _)) = parse_key(&p.id) {
                        touched.push(f.to_owned());
                    }
                }
                self.after(&touched, notify).await;
            }
            Command::LoadMore { mailbox } => {
                let added = self.load_more(&mailbox).await?;
                notify(Event::LoadedMore { mailbox, added });
                notify(Event::EmailsChanged);
            }
            Command::Search { query, mailbox, ticket } => {
                let ids = self.search(&query, mailbox.as_deref()).await?;
                let total = ids.len() as u64;
                notify(Event::SearchResults { ticket, ids, total });
                notify(Event::EmailsChanged);
            }
            // Фильтры «Все папки» — `SEARCH` сервера (S7-04в); чего IMAP не выразит — пусто.
            Command::Query { filter, mailbox, ticket } => {
                let ids = match imap_query(&filter) {
                    Some((q, inside)) => self.query(&q, inside.as_deref().or(mailbox.as_deref())).await?,
                    None => Vec::new(),
                };
                let total = ids.len() as u64;
                notify(Event::SearchResults { ticket, ids, total });
                notify(Event::EmailsChanged);
            }
            Command::Counts { filters, mailbox, ticket } => {
                let mut counts = Vec::with_capacity(filters.len());
                for f in &filters {
                    counts.push(match imap_query(f) {
                        Some((q, inside)) => self.count(&q, inside.as_deref().or(mailbox.as_deref())).await? as u64,
                        None => 0,
                    });
                }
                notify(Event::Counts { ticket, counts: Some(counts) });
            }
            Command::Send { draft, replace, ticket, release } => {
                let hold = match release {
                    postman_core::model::Release::At(_) => {
                        notify(Event::ComposeFailed { ticket, error: "send later is not available for IMAP".into() });
                        return Ok(());
                    }
                    postman_core::model::Release::After(s) => s,
                    postman_core::model::Release::Now => 0,
                };
                if hold > 0 {
                    // Окно «Отменить» держит синхронизация: письмо уйдёт по сроку (`release_held`).
                    let scheduled = self.hold(*draft, replace, hold);
                    notify(Event::Sent { ticket, scheduled: Some(scheduled) });
                    return Ok(());
                }
                match self.send(*draft, replace).await {
                    Ok(()) => {
                        notify(Event::Sent { ticket, scheduled: None });
                        let folders: Vec<String> =
                            [Role::Sent, Role::Drafts].iter().filter_map(|r| self.folder_by_role(*r).map(|f| f.key.clone())).collect();
                        self.after(&folders, notify).await;
                    }
                    Err(e) => notify(Event::ComposeFailed { ticket, error: e.to_string() }),
                }
            }
            Command::SaveDraft { draft, replace, ticket } => match self.save_draft(*draft, replace).await {
                Ok(id) => {
                    notify(Event::DraftSaved { ticket, id });
                    let drafts: Vec<String> = self.folder_by_role(Role::Drafts).map(|f| f.key.clone()).into_iter().collect();
                    self.after(&drafts, notify).await;
                }
                Err(e) => notify(Event::ComposeFailed { ticket, error: e.to_string() }),
            },
            Command::Scheduled => notify(Event::Scheduled { list: Vec::new(), max_delay: 0 }),
            Command::Unsend { scheduled, ticket } => match self.held.iter().position(|h| h.id == scheduled.submission) {
                // Письмо ещё здесь: не отправлять; черновик откроет интерфейс (он держит его с окна «Отменить»).
                Some(i) => {
                    self.held.remove(i);
                    notify(Event::Unsent { ticket, email: String::new() });
                }
                None => notify(Event::UnsendFailed { ticket, too_late: true, error: String::new() }),
            },
            Command::Reconnect => {
                if self.session.is_none() {
                    self.session().await?;
                }
            }
            Command::RespondInvite { invite, status, in_reply_to } => self.respond_invite(*invite, status, in_reply_to, notify).await,
            Command::Delegate { uid, drafts, ticket, release } => self.delegate(uid, drafts, ticket, release, notify).await,
            Command::Withhold { scheduled, ticket } => {
                // Письма шага поручения в окне «Отменить» держит синхронизация: убрать те, что ещё здесь.
                let mut too_late = false;
                for s in &scheduled {
                    match self.held.iter().position(|h| h.id == s.submission) {
                        Some(i) => drop(self.held.remove(i)),
                        None => too_late = true,
                    }
                }
                notify(Event::Withheld { ticket, too_late, error: None });
            }
            // Календарь, задачи, занятость, контакты — путям областей. Правил и автоответа у IMAP нет, календаря и
            // контактов без путей областей — тоже: интерфейс их не показывает (`Features`).
            other => self.area_command(other, notify).await,
        }
        Ok(())
    }

    /// Окно «Отменить» держит синхронизация: письмо уйдёт по сроку (`release_held`).
    fn hold(&mut self, draft: Draft, replace: Option<String>, secs: u32) -> postman_core::send_later::Scheduled {
        self.next_hold += 1;
        let id = scoped(self.scope(), &format!("hold.{}", self.next_hold));
        let send_at = jiff::Timestamp::now().as_second() + i64::from(secs);
        let at = tokio::time::Instant::now() + Duration::from_secs(u64::from(secs));
        self.held.push(Held { id: id.clone(), draft, replace, at });
        postman_core::send_later::Scheduled { submission: id.clone(), email: id, send_at }
    }

    /// Ответ на приглашение: своей копией на CalDAV (REPLY шлёт сервер), иначе — письмом организатору (iMIP).
    async fn respond_invite(
        &mut self,
        inv: postman_calendar::Invitation,
        status: postman_calendar::Partstat,
        in_reply_to: Option<String>,
        notify: &impl Fn(Event),
    ) {
        let uid = inv.event.uid.clone();
        if let Some((_, a)) = self.areas.iter_mut().find(|(_, a)| a.features().has(Feature::Calendar)) {
            match a.respond(&inv, status).await {
                Ok(true) => {
                    notify(Event::InviteAnswered { uid, status, delivery: postman_core::invite::Delivery::Server });
                    notify(Event::CalendarChanged);
                    return;
                }
                Ok(false) => {}
                Err(e) => return notify(Event::InviteFailed { uid, error: e.to_string() }),
            }
        }
        let me = Address::new("", &self.account.email);
        let Some(draft) = postman_core::invite::reply_draft(&inv, &me, status, in_reply_to, jiff::Timestamp::now().as_second()) else {
            return notify(Event::InviteFailed { uid, error: "the invitation has no organizer".into() });
        };
        match self.send(draft, None).await {
            Ok(()) => notify(Event::InviteAnswered { uid, status, delivery: postman_core::invite::Delivery::Mail { imported: false } }),
            Err(e) => notify(Event::InviteFailed { uid, error: e.to_string() }),
        }
    }

    /// Шаг поручения (S6-09): своя копия задачи — на CalDAV (без рассылки сервером, `SCHEDULE-AGENT=CLIENT`), затем
    /// письма iTIP шага по SMTP; окно «Отменить» — как у `Send`.
    async fn delegate(&mut self, uid: String, drafts: Vec<Draft>, ticket: u64, release: postman_core::model::Release, notify: &impl Fn(Event)) {
        let owner = self.owner();
        let Some((_, a)) = self.areas.iter_mut().find(|(_, a)| a.features().has(Feature::Tasks)) else {
            return notify(Event::Delegated { ticket, uid, error: Some("no tasks on the server".into()), scheduled: Vec::new() });
        };
        let ctx = AreaCtx { cache: &self.cache, profile: self.profile.as_deref(), owner: owner.as_ref(), notify };
        if let Err(e) = a.flush_tasks(ctx).await {
            return notify(Event::Delegated { ticket, uid, error: Some(e.to_string()), scheduled: Vec::new() });
        }
        let mut scheduled = Vec::new();
        for draft in drafts {
            match release {
                postman_core::model::Release::After(s) if s > 0 => scheduled.push(self.hold(draft, None, s)),
                _ => {
                    if let Err(e) = self.send(draft, None).await {
                        return notify(Event::Delegated { ticket, uid, error: Some(e.to_string()), scheduled });
                    }
                }
            }
        }
        notify(Event::Delegated { ticket, uid, error: None, scheduled });
    }

    /// Владелец событий дополнительной учётной записи в общем календаре (S7-03б); у основной — `None`.
    fn owner(&self) -> Option<postman_calendar::Owner> {
        self.profile.as_ref().map(|_| postman_calendar::Owner { email: self.account.email.clone(), name: self.account.email.clone() })
    }

    /// Календарь, задачи и контакты — путям областей по порядку (`postman_core::backend::Areas`), тот же договор
    /// событий, что у JMAP.
    async fn area_command(&mut self, cmd: Command, notify: &impl Fn(Event)) {
        let owner = self.owner();
        let mut cmd = cmd;
        for (_, a) in self.areas.iter_mut() {
            let ctx = AreaCtx { cache: &self.cache, profile: self.profile.as_deref(), owner: owner.as_ref(), notify };
            match a.handle(cmd, ctx).await {
                None => return,
                Some(back) => cmd = back,
            }
        }
        tracing::debug!("imap: command not supported: {}", command_name(&cmd));
    }
}

/// Условие фильтра Postman → `SEARCH` IMAP (RFC 3501 §6.4.4) и папка, если условие её задаёт (`InMailbox` в «И»).
/// `None` — IMAP так не умеет.
pub fn imap_query(f: &postman_core::model::Filter) -> Option<(String, Option<String>)> {
    use postman_core::model::Filter as F;
    fn date(utc: &str) -> Option<String> {
        const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
        let d: jiff::civil::Date = utc.get(..10)?.parse().ok()?;
        Some(format!("{}-{}-{}", d.day(), MONTHS[usize::try_from(d.month()).ok()?.checked_sub(1)?], d.year()))
    }
    fn keyword(k: &str, on: bool) -> Option<String> {
        let sys = |yes: &str, no: &str| Some(if on { yes } else { no }.to_owned());
        match k.to_ascii_lowercase().as_str() {
            "$seen" => sys("SEEN", "UNSEEN"),
            "$flagged" => sys("FLAGGED", "UNFLAGGED"),
            "$answered" => sys("ANSWERED", "UNANSWERED"),
            "$draft" => sys("DRAFT", "UNDRAFT"),
            _ => {
                let flag = postman_imap::flags::flag(k)?;
                Some(format!("{} {flag}", if on { "KEYWORD" } else { "UNKEYWORD" }))
            }
        }
    }
    fn key(f: &F) -> Option<String> {
        let q = |s: &str| postman_imap::quote(s);
        Some(match f {
            F::InMailbox(_) => "ALL".into(),
            F::HasKeyword(k) => keyword(k, true)?,
            F::NotKeyword(k) => keyword(k, false)?,
            F::Text(t) => format!("TEXT {}", q(t)),
            F::From(t) => format!("FROM {}", q(t)),
            F::To(t) => format!("TO {}", q(t)),
            F::Body(t) => format!("BODY {}", q(t)),
            F::Subject(t) => format!("SUBJECT {}", q(t)),
            // Вложение — по типу письма: `multipart/mixed` (приближение, как у Thunderbird).
            F::HasAttachment(true) => "HEADER Content-Type \"multipart/mixed\"".into(),
            F::HasAttachment(false) => "NOT HEADER Content-Type \"multipart/mixed\"".into(),
            F::And(v) => format!("({})", v.iter().map(key).collect::<Option<Vec<_>>>()?.join(" ")),
            F::Or(v) => {
                let mut parts = v.iter().map(key).collect::<Option<Vec<_>>>()?;
                let last = parts.pop()?;
                parts.into_iter().rev().fold(last, |acc, p| format!("OR {p} {acc}"))
            }
            F::Not(v) => format!("NOT ({})", v.iter().map(key).collect::<Option<Vec<_>>>()?.join(" ")),
            F::Before(d) => format!("BEFORE {}", date(d)?),
            F::After(d) => format!("SINCE {}", date(d)?),
            F::MinSize(n) => format!("LARGER {}", n.saturating_sub(1)),
        })
    }
    let inside = match f {
        F::InMailbox(m) => Some(m.clone()),
        F::And(v) => v.iter().find_map(|c| if let F::InMailbox(m) = c { Some(m.clone()) } else { None }),
        _ => None,
    };
    Some((format!("CHARSET UTF-8 {}", key(f)?), inside))
}

fn command_name(c: &Command) -> &'static str {
    match c {
        Command::Events { .. } | Command::SearchEvents { .. } => "calendar",
        Command::Contacts | Command::SaveContact { .. } | Command::DeleteContact { .. } => "contacts",
        Command::Tasks(_) | Command::Delegate { .. } => "tasks",
        Command::Settings(_) => "settings",
        Command::Categories(_) => "categories",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::{ImapAccount, email_key, folder_id, is_imap_url, parse_key};
    use postman_imap::Security;

    #[test]
    fn imap_urls() {
        assert!(is_imap_url("imaps://mail.example.test") && is_imap_url("IMAP://x") && !is_imap_url("https://mail.example.test"));
        let a = ImapAccount::from_url("imaps://mail.example.test", "me@example.test", "secret").unwrap();
        assert_eq!((a.imap.host.as_str(), a.imap.port, a.imap.security), ("mail.example.test", 993, Security::Tls));
        assert_eq!((a.smtp.host.as_str(), a.smtp.port, a.smtp.security), ("mail.example.test", 465, Security::Tls));
        let b = ImapAccount::from_url("imap://imap.example.test:143?smtp=smtp.example.test:587", "u", "p").unwrap();
        assert_eq!((b.imap.port, b.imap.security), (143, Security::StartTls));
        assert_eq!((b.smtp.host.as_str(), b.smtp.port, b.smtp.security), ("smtp.example.test", 587, Security::StartTls));
        assert!(ImapAccount::from_url("https://x", "u", "p").is_none());
        assert!(a.areas.is_empty());
        // CalDAV и CardDAV рядом (S7-05б).
        let c = ImapAccount::from_url("imaps://mail.example.test?dav=https://mail.example.test", "u", "p").unwrap();
        assert_eq!((c.area("dav"), c.smtp.port), (Some("https://mail.example.test"), 465));
        let d = ImapAccount::from_url("imaps://m.example.test?smtp=s.example.test&dav=https://d.example.test/dav/", "u", "p").unwrap();
        assert_eq!((d.area("dav"), d.smtp.host.as_str()), (Some("https://d.example.test/dav/"), "s.example.test"));
        assert_eq!(ImapAccount::from_url("imaps://m?dav=ftp://x", "u", "p").unwrap().area("dav"), None);
        assert!(ImapAccount::from_url("imaps://", "u", "p").is_none());
        // Пароль — не в `Debug`.
        assert!(!format!("{a:?}").contains("secret"));
    }

    #[test]
    fn filters_become_imap_search() {
        use postman_core::model::Filter as F;
        let q = |f: F| super::imap_query(&f);
        assert_eq!(q(F::NotKeyword("$seen".into())).unwrap().0, "CHARSET UTF-8 UNSEEN");
        assert_eq!(q(F::HasKeyword("pm.due".into())).unwrap().0, "CHARSET UTF-8 KEYWORD pm.due");
        let (s, m) = q(F::And(vec![F::InMailbox("fabc".into()), F::From("Анна".into()), F::After("2026-10-01T00:00:00Z".into())])).unwrap();
        assert_eq!(s, "CHARSET UTF-8 (ALL FROM \"Анна\" SINCE 1-Oct-2026)");
        assert_eq!(m.as_deref(), Some("fabc"));
        assert_eq!(
            q(F::Or(vec![F::Subject("a".into()), F::Subject("b".into()), F::Subject("c".into())])).unwrap().0,
            "CHARSET UTF-8 OR SUBJECT \"a\" OR SUBJECT \"b\" SUBJECT \"c\""
        );
        assert_eq!(q(F::Not(vec![F::Before("2026-01-31T00:00:00Z".into())])).unwrap().0, "CHARSET UTF-8 NOT (BEFORE 31-Jan-2026)");
        assert_eq!(q(F::MinSize(1024)).unwrap().0, "CHARSET UTF-8 LARGER 1023");
        assert!(q(F::HasKeyword("two words".into())).is_none());
    }

    #[test]
    fn threads_follow_references() {
        let h = |refs: &[&str], reply: Option<&str>, mid: Option<&str>| postman_imap::Header {
            references: refs.iter().map(|s| (*s).to_owned()).collect(),
            in_reply_to: reply.map(str::to_owned),
            message_id: mid.map(str::to_owned),
            ..Default::default()
        };
        let root = super::thread_key(None, &h(&[], None, Some("<a@x>")), "k1");
        assert_eq!(super::thread_key(None, &h(&["<A@x>", "<b@x>"], Some("<b@x>"), Some("<c@x>")), "k2"), root);
        assert_eq!(super::thread_key(None, &h(&[], Some("<a@x>"), Some("<b@x>")), "k3"), root);
        assert_eq!(super::thread_key(None, &h(&[], None, None), "k4"), "k4");
        assert!(super::thread_key(Some("@2"), &h(&[], None, Some("<a@x>")), "k").starts_with("@2/t"));
    }

    #[test]
    fn keys_round_trip() {
        let f = folder_id("Sent/2026");
        assert!(f.starts_with('f') && !f.contains('/'));
        assert_eq!(f, folder_id("Sent/2026"));
        assert_ne!(f, folder_id("Sent/2025"));
        let k = email_key(Some("@2"), &f, 7, 120);
        assert_eq!(parse_key(&k), Some((f.as_str(), 7, 120)));
        assert_eq!(parse_key(&format!("{k}#3")), Some((f.as_str(), 7, 120)));
        assert_eq!(parse_key(&email_key(None, &f, 1, 2)), Some((f.as_str(), 1, 2)));
        assert_eq!(parse_key("Mabc123"), None);
        assert_eq!(parse_key("shared/Mabc"), None);
    }
}
