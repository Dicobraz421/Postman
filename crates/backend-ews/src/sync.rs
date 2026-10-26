//! Синхронизация почты учётной записи Exchange по EWS: свой поток, как у JMAP и IMAP
//! (`postman_core::sync::spawn_thread`), тот же договор команд и событий на общем кэше.
//!
//! - **Папки** — `SyncFolderHierarchy`; сервер на нём падает (Exchange 2013 CU23 — `ErrorInternalServerError`,
//!   `docs/EXCHANGE.md`) — дерево `FindFolder` (Deep). Роли — по стандартным папкам (`GetFolder`). Ключ папки в
//!   кэше — `f<хэш id>` (в id EWS бывает `/`, а он — разделитель аккаунта в ключах кэша).
//! - **Письма** — ключ `m<id EWS>` (`+` → `-`, `/` → `_`). Первый вход — последние [`INITIAL`] писем папки
//!   (`FindItem` по дате), затем состояние `SyncFolderItems` «сейчас»; дальше — изменения по состоянию: новые и
//!   изменённые — заголовки заново, удалённые — из кэша, «прочитано» — по `ReadFlagChange`. Перенос на сервере —
//!   новое письмо в другой папке (у EWS при переносе меняется id).
//! - **Тело** — `MimeContent` целиком, разбор MIME — воркер-песочница (`postman_worker::mime`), как у IMAP; письмо
//!   целиком — блоб под своим ключом, вложения — `<ключ>#<номер>`. Цепочка — по `References` из MIME (заголовков
//!   `Message-ID`/`References` Exchange 2013 в списке не отдаёт).
//! - **Прочитано и флаг** — `UpdateItem`; другие ключевые слова EWS не хранит (метки Postman у Exchange — нет).
//! - **Отправка** — `CreateItem` из MIME, копия в «Отправленные» — сервером (`SendAndSaveCopy`); «Отменить» — письмо
//!   держит синхронизация, как у IMAP; «Отправить позже» — нет. **Черновик** — `SaveOnly` в «Черновики».
//! - **Поиск и фильтры** — `FindItem` с AQS по папкам.
//! - **Изменения** — опрос по таймеру ([`POLL`]); календарь, занятость и справочник — области ([`crate::areas`]).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use postman_cache::{AccountRow, Addr, AttachmentRow, BodyRow, Cache, EmailRow, MailboxRow, Rights, in_profile, scoped, unscoped};
use postman_compose::{Address, Draft};
use postman_core::backend::{AreaCtx, Spawn};
use postman_core::sync::{Command, ErrorKind, Event, Sender, Stage, Status, SyncError};
use postman_ews::{Client, DeleteMode, FolderId, FolderRef, ItemId, MailHeader, MailRole, SyncState};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::areas::EwsAreas;
use crate::{auth, sync_error};

/// Писем папки при первом входе — не больше (дальше — «Загрузить ещё»).
pub const INITIAL: u32 = 500;
/// Догрузка при прокрутке.
const PAGE: u32 = 500;
/// Опрос изменений (push у EWS — streaming, его не берём: S7 «Вне сессии»).
pub const POLL: Duration = Duration::from_secs(120);
/// Изменений за один `SyncFolderItems`.
const SYNC_PAGE: u32 = 512;
/// Страниц изменений за опрос.
const SYNC_PAGES: usize = 20;
/// Страниц при первом входе, чтобы дойти до состояния «сейчас» (100 тыс. писем папки).
const DRAIN_PAGES: usize = 200;
/// Найдено поиском в папке — не больше.
const SEARCH_LIMIT: u32 = 200;
/// Сколько ждать воркер на разборе письма.
const WORKER_TIMEOUT: Duration = Duration::from_secs(10);

/// FNV-1a 64: устойчивый короткий ключ по строке.
fn hash(s: &str) -> String {
    let h = s.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    format!("{h:016x}")
}

/// Id письма EWS → часть ключа кэша (без `/`, который разделяет аккаунт).
pub fn encode_id(id: &str) -> String {
    format!("m{}", id.replace('+', "-").replace('/', "_"))
}

/// Ключ письма (или вложения `<ключ>#<номер>`) → id EWS.
pub fn parse_key(key: &str) -> Option<ItemId> {
    let id = unscoped(key).1;
    let id = id.split_once('#').map_or(id, |(k, _)| k);
    let enc = id.strip_prefix('m')?;
    (!enc.is_empty()).then(|| ItemId::new(enc.replace('-', "+").replace('_', "/")))
}

fn email_key(scope: Option<&str>, id: &ItemId) -> String {
    scoped(scope, &encode_id(&id.id))
}

/// Роль папки → роль JMAP в кэше. «Исходящие» — обычная папка.
fn jmap_role(r: MailRole) -> Option<&'static str> {
    match r {
        MailRole::Inbox => Some("inbox"),
        MailRole::Drafts => Some("drafts"),
        MailRole::Sent => Some("sent"),
        MailRole::Trash => Some("trash"),
        MailRole::Junk => Some("junk"),
        MailRole::Archive => Some("archive"),
        MailRole::Outbox => None,
    }
}

fn role_rank(role: Option<MailRole>) -> u32 {
    match role {
        Some(MailRole::Inbox) => 0,
        Some(MailRole::Drafts) => 1,
        Some(MailRole::Sent) => 2,
        Some(MailRole::Archive) => 3,
        Some(MailRole::Junk) => 4,
        Some(MailRole::Trash) => 5,
        _ => 6,
    }
}

/// Ключ цепочки: начало `References`, иначе `In-Reply-To`, иначе своё `Message-ID`; без них — само письмо.
fn thread_key(scope: Option<&str>, refs: &[String], in_reply_to: Option<&str>, message_id: Option<&str>, key: &str) -> String {
    let root = refs.first().map(String::as_str).or(in_reply_to).or(message_id);
    match root.map(|r| r.trim().to_ascii_lowercase()).filter(|r| !r.is_empty()) {
        Some(r) => scoped(scope, &format!("t{}", hash(&r))),
        None => key.to_owned(),
    }
}

/// Строка письма из заголовка EWS.
pub fn row(scope: Option<&str>, folder_key: &str, h: &MailHeader) -> EmailRow {
    let key = email_key(scope, &h.id);
    let refs: Vec<String> = h.references.as_deref().unwrap_or("").split_whitespace().map(str::to_owned).collect();
    let mut r = EmailRow {
        thread_id: thread_key(scope, &refs, h.in_reply_to.as_deref(), h.message_id.as_deref(), &key),
        id: key,
        mailbox_ids: vec![folder_key.to_owned()],
        received_at: h.received_at,
        subject: h.subject.clone(),
        from_name: h.from_name.clone(),
        from_addr: h.from_addr.clone(),
        preview: String::new(),
        seen: false,
        flagged: false,
        has_attachment: h.has_attachments,
        size: u64::from(h.size),
        keywords: Vec::new(),
    };
    r.set_keyword("$seen", h.is_read);
    r.set_keyword("$flagged", h.flagged);
    r
}

/// Условие фильтра Postman → строка AQS (`QueryString`) и папка, если условие её задаёт. `None` — AQS так не умеет.
pub fn aqs(f: &postman_core::model::Filter) -> Option<(String, Option<String>)> {
    use postman_core::model::Filter as F;
    fn word(s: &str) -> String {
        format!("\"{}\"", s.replace('"', " "))
    }
    fn key(f: &F) -> Option<String> {
        Some(match f {
            F::InMailbox(_) => String::new(),
            F::Text(t) => word(t),
            F::From(t) => format!("from:{}", word(t)),
            F::To(t) => format!("to:{}", word(t)),
            F::Subject(t) => format!("subject:{}", word(t)),
            F::Body(t) => format!("body:{}", word(t)),
            F::HasAttachment(on) => format!("hasattachment:{on}"),
            F::HasKeyword(k) if k.eq_ignore_ascii_case("$seen") => "isread:true".into(),
            F::NotKeyword(k) if k.eq_ignore_ascii_case("$seen") => "isread:false".into(),
            F::HasKeyword(k) if k.eq_ignore_ascii_case("$flagged") => "isflagged:true".into(),
            F::NotKeyword(k) if k.eq_ignore_ascii_case("$flagged") => "isflagged:false".into(),
            F::And(v) => {
                let parts: Vec<String> = v.iter().map(key).collect::<Option<Vec<_>>>()?.into_iter().filter(|p| !p.is_empty()).collect();
                format!("({})", parts.join(" AND "))
            }
            F::Or(v) => format!("({})", v.iter().map(key).collect::<Option<Vec<_>>>()?.join(" OR ")),
            F::Not(v) => format!("NOT ({})", v.iter().map(key).collect::<Option<Vec<_>>>()?.join(" AND ")),
            // Даты и размер в AQS Exchange 2013 — [проверить]; ключевых слов, кроме «прочитано» и флага, у EWS нет.
            _ => return None,
        })
    }
    let inside = match f {
        F::InMailbox(m) => Some(m.clone()),
        F::And(v) => v.iter().find_map(|c| if let F::InMailbox(m) = c { Some(m.clone()) } else { None }),
        _ => None,
    };
    let q = key(f)?;
    (!q.is_empty() && q != "()").then_some((q, inside))
}

/// Папка ящика в кэше.
#[derive(Debug, Clone)]
struct Folder {
    /// Ключ папки в кэше (`scoped(аккаунт, f<хэш>)`).
    key: String,
    /// `f<хэш id>`.
    hash: String,
    fid: FolderId,
    name: String,
    parent: Option<String>,
    role: Option<MailRole>,
}

impl Folder {
    fn at(&self) -> FolderRef {
        FolderRef::Id(self.fid.clone())
    }
}

/// Все письма папки в кэше.
fn letters(cache: &Cache, folder: &str) -> postman_cache::Result<Vec<EmailRow>> {
    let n = cache.count(folder)?;
    cache.page(folder, 0, n)
}

fn db(e: postman_cache::Error) -> postman_ews::Error {
    postman_ews::Error::Xml(format!("cache: {e}"))
}

/// Синхронизация учётной записи EWS: свой поток.
pub(crate) fn spawn(s: Spawn, user: String, password: String) -> UnboundedSender<Command> {
    let (tx, rx) = unbounded_channel();
    let Spawn { account, profile, cache, events, repaint, .. } = s;
    postman_core::sync::spawn_thread("sync-ews", events, repaint, move |notify| async move {
        run(account.url, user, password, profile, cache, rx, &|e| notify.send(e)).await;
    });
    tx
}

struct Ews {
    client: Client,
    email: String,
    profile: Option<String>,
    scope: Option<String>,
    roles: Vec<(MailRole, FolderId)>,
    folders: Vec<Folder>,
    /// `SyncFolderHierarchy` не работает на этом сервере — дерево обходом `FindFolder`.
    find_folders: bool,
    cache: Cache,
    worker: Option<postman_worker::Client>,
    held: Vec<Held>,
    next_hold: u64,
    areas: Option<EwsAreas>,
}

/// Письмо, которое держит синхронизация до срока окна «Отменить».
struct Held {
    id: String,
    draft: Draft,
    replace: Option<String>,
    at: tokio::time::Instant,
}

/// Логин похож на адрес почты (`user@domain`, не `DOMAIN\user`).
fn is_mail_address(s: &str) -> bool {
    s.split_once('@').is_some_and(|(u, d)| !u.is_empty() && d.contains('.') && !s.contains('\\'))
}

fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let host = rest.split(['/', '?']).next()?;
    let host = host.rsplit_once(':').map_or(host, |(h, _)| h);
    (!host.is_empty()).then(|| host.to_owned())
}

/// Вход: адрес EWS из Autodiscover (логин — адрес почты), иначе адрес учётной записи; роли стандартных папок.
async fn sign_in(url: &str, user: &str, password: &str) -> Result<(Client, String, Vec<(MailRole, FolderId)>), postman_ews::Error> {
    let a = auth(user, password);
    let mut urls = Vec::new();
    if is_mail_address(user) {
        match postman_ews::discover(user, &a, host_of(url).as_deref()).await {
            Ok(found) if found.source == postman_ews::Source::Autodiscover => urls.extend(found.urls),
            Ok(_) => {}
            // Отказ во входе — сразу, без второй попытки (не блокировать учётку).
            Err(e @ postman_ews::Error::Auth(_)) => return Err(e),
            Err(e) => tracing::debug!("autodiscover: {e}"),
        }
    }
    if !urls.iter().any(|u| u.eq_ignore_ascii_case(url)) {
        urls.push(url.to_owned());
    }
    let mut last = None;
    for u in urls {
        let mut client = Client::new(&u, a.clone())?;
        match client.mail_roles().await {
            Ok(roles) => return Ok((client, u, roles)),
            Err(e @ (postman_ews::Error::Auth(_) | postman_ews::Error::AuthScheme)) => return Err(e),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or(postman_ews::Error::Closed))
}

async fn run(
    url: String,
    user: String,
    password: String,
    profile: Option<String>,
    cache: Cache,
    mut rx: UnboundedReceiver<Command>,
    notify: &impl Fn(Event),
) {
    notify(Event::Status(Status::Connecting { url: host_of(&url).unwrap_or_default() }));
    let (client, ews_url, roles) = match sign_in(&url, &user, &password).await {
        Ok(x) => x,
        Err(e) => return notify(Event::Error(sync_error(Stage::Connect, &e))),
    };
    if let Some(v) = client.server_version() {
        tracing::info!("exchange {}.{}.{}", v.major, v.minor, v.build);
    }
    let scope = in_profile(profile.as_deref(), None);
    let mut me = Ews {
        client,
        email: user.clone(),
        profile,
        scope,
        roles,
        folders: Vec::new(),
        find_folders: false,
        cache,
        worker: None,
        held: Vec::new(),
        next_hold: 0,
        areas: None,
    };
    notify(Event::Status(Status::SignedIn { user: me.email.clone() }));
    match EwsAreas::connect(&ews_url, &user, &password, &me.email).await {
        Ok(a) => me.areas = Some(a),
        // Почта работает и без календаря: ошибка — тостом календаря, области скрыты.
        Err(e) => {
            tracing::warn!("ews areas: {e}");
            notify(Event::CalendarFailed(e.to_string()));
        }
    }
    let address = Address::new("", &me.email);
    if me.profile.is_none() {
        // Основная учётная запись: почта и области EWS (S7-03а «Нет у сервера — нет на экране»).
        notify(Event::Features(me.areas.as_ref().map_or(postman_core::model::Features::NONE, EwsAreas::features)));
        notify(Event::Me { address: address.clone(), signature: String::new() });
    }
    notify(Event::Senders(vec![Sender { account: me.scope.clone(), address, signature: String::new() }]));
    // Сервер письмо не держит: «Отправить позже» нет (интерфейс прячет его по `max_delay: 0`).
    notify(Event::Scheduled { list: Vec::new(), max_delay: 0 });
    let rows: Vec<AccountRow> = match &me.profile {
        Some(p) => vec![AccountRow { id: p.clone(), name: me.email.clone(), is_personal: true, read_only: false }],
        None => Vec::new(),
    };
    if let Err(e) = me.cache.replace_accounts_of(me.profile.as_deref(), &rows) {
        return notify(Event::Error(sync_error(Stage::Load, &db(e))));
    }
    if let Err(e) = me.refresh_all(notify).await {
        let stop = e.kind == ErrorKind::Auth;
        notify(Event::Error(e));
        if stop {
            return;
        }
    }
    notify(Event::Status(Status::Synced));
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
                    tracing::warn!("ews command: {e}");
                    notify(Event::Error(sync_error(Stage::Command, &e)));
                }
            }
            () = held_until(me.held.iter().map(|h| h.at).min()) => me.release_held(false, notify).await,
            _ = poll.tick() => {
                if let Err(e) = me.refresh_all(notify).await {
                    notify(Event::Error(e));
                }
                if let Some(a) = me.areas.as_mut() {
                    match a.poll().await {
                        Ok(true) => notify(Event::CalendarChanged),
                        Ok(false) => {}
                        Err(e) => tracing::debug!("ews calendar poll: {e}"),
                    }
                }
            }
        }
    }
}

async fn held_until(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

impl Ews {
    fn scope(&self) -> Option<&str> {
        self.scope.as_deref()
    }

    fn folder_by_role(&self, role: MailRole) -> Option<&Folder> {
        self.folders.iter().find(|f| f.role == Some(role))
    }

    fn folder_by_key(&self, key: &str) -> Option<&Folder> {
        self.folders.iter().find(|f| f.key == key || f.hash == key)
    }

    /// Папки заново и каждая сверена; итог — событиями.
    async fn refresh_all(&mut self, notify: &impl Fn(Event)) -> Result<(), SyncError> {
        self.load_folders().await.map_err(|e| sync_error(Stage::Load, &e))?;
        notify(Event::MailboxesChanged);
        let mut changed = false;
        for f in self.folders.clone() {
            notify(Event::Status(Status::Loading { mailbox: f.name.clone() }));
            match self.refresh(&f).await {
                Ok(c) => changed |= c,
                Err(e) => {
                    let e = sync_error(Stage::Refresh, &e);
                    if e.kind == ErrorKind::Auth || e.kind == ErrorKind::Network {
                        return Err(e);
                    }
                    tracing::warn!("ews folder: {e}");
                }
            }
        }
        self.write_folders().map_err(|e| sync_error(Stage::Load, &e))?;
        notify(Event::MailboxesChanged);
        if changed {
            notify(Event::EmailsChanged);
        }
        Ok(())
    }

    /// Дерево папок: `SyncFolderHierarchy` целиком, а если сервер на нём падает — `FindFolder` (Deep).
    async fn load_folders(&mut self) -> postman_ews::Result<()> {
        let list = if self.find_folders {
            self.client.find_folders(&self.roles).await?
        } else {
            let mut all = Vec::new();
            let mut state: Option<SyncState> = None;
            let mut failed = false;
            for _ in 0..SYNC_PAGES {
                match self.client.sync_folders(state.as_ref(), &self.roles).await {
                    Ok(ch) => {
                        all.extend(ch.created);
                        all.extend(ch.updated);
                        let done = ch.done;
                        state = Some(ch.state);
                        if done {
                            break;
                        }
                    }
                    Err(e) if e.code() == Some("ErrorInternalServerError") => {
                        failed = true;
                        break;
                    }
                    Err(e) => return Err(e),
                }
            }
            if failed {
                tracing::info!("ews: SyncFolderHierarchy unavailable, using FindFolder");
                self.find_folders = true;
                self.client.find_folders(&self.roles).await?
            } else {
                all
            }
        };
        let scope = self.scope.clone();
        let key = |id: &str| scoped(scope.as_deref(), &format!("f{}", hash(id)));
        let ids: HashSet<&str> = list.iter().map(|f| f.id.id.as_str()).collect();
        self.folders = list
            .iter()
            .map(|f| Folder {
                key: key(&f.id.id),
                hash: format!("f{}", hash(&f.id.id)),
                fid: f.id.clone(),
                // Родитель вне дерева (корень ящика) — верхний уровень.
                parent: f.parent.as_deref().filter(|p| ids.contains(p)).map(key),
                name: if f.role == Some(MailRole::Inbox) { "Inbox".into() } else { f.name.clone() },
                role: f.role,
            })
            .collect();
        self.write_folders()
    }

    /// Папки — в кэш, счётчики — по письмам в кэше.
    fn write_folders(&self) -> postman_ews::Result<()> {
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
                    role: f.role.and_then(jmap_role).map(str::to_owned),
                    sort_order: role_rank(f.role) * 1000 + u32::try_from(i).unwrap_or(999).min(999),
                    total: u32::try_from(letters.len()).unwrap_or(u32::MAX),
                    unread: u32::try_from(letters.iter().filter(|r| !r.seen).count()).unwrap_or(u32::MAX),
                    rights: Rights::ALL,
                }
            })
            .collect();
        self.cache.replace_mailboxes(self.scope(), &rows).map_err(db)
    }

    /// Заголовки по id → строки папки `folder_key` (в кэш).
    async fn headers_into(&mut self, ids: &[ItemId], folder_key: &str) -> postman_ews::Result<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let headers = self.client.mail_headers(ids).await?;
        let scope = self.scope.clone();
        let rows: Vec<EmailRow> = headers.iter().map(|h| row(scope.as_deref(), folder_key, h)).collect();
        self.cache.upsert_emails(&rows).map_err(db)?;
        Ok(rows.len())
    }

    /// Сверить папку с сервером; `true` — письма в кэше изменились.
    async fn refresh(&mut self, f: &Folder) -> postman_ews::Result<bool> {
        let state_key = scoped(self.scope(), &format!("ews.{}", f.hash));
        let old = self.cache.state(&state_key).map_err(db)?.filter(|s| !s.is_empty()).map(SyncState);
        let Some(mut state) = old else { return self.first_load(f, &state_key).await };
        let scope = self.scope.clone();
        let mut fetch: Vec<ItemId> = Vec::new();
        let mut gone: Vec<String> = Vec::new();
        let mut reads: Vec<(String, bool)> = Vec::new();
        for _ in 0..SYNC_PAGES {
            let page = match self.client.sync_mail(&f.at(), Some(&state), SYNC_PAGE).await {
                Err(e) if e.is_invalid_sync_state() => {
                    // Сервер забыл состояние: папку — заново.
                    let here: Vec<String> = letters(&self.cache, &f.key).map_err(db)?.into_iter().map(|r| r.id).collect();
                    self.cache.delete_emails(&here).map_err(db)?;
                    return self.first_load(f, &state_key).await;
                }
                r => r?,
            };
            fetch.extend(page.created);
            fetch.extend(page.updated);
            gone.extend(page.deleted.iter().map(|id| email_key(scope.as_deref(), &ItemId::new(id.clone()))));
            reads.extend(page.read_flags.iter().map(|(id, read)| (email_key(scope.as_deref(), &ItemId::new(id.clone())), *read)));
            state = page.state;
            if page.done {
                break;
            }
        }
        let fetched = self.headers_into(&fetch, &f.key).await?;
        if !gone.is_empty() {
            self.cache.delete_emails(&gone).map_err(db)?;
        }
        let mut flagged = 0;
        if !reads.is_empty() {
            let keys: Vec<String> = reads.iter().map(|(k, _)| k.clone()).collect();
            let by_key: HashMap<&str, bool> = reads.iter().map(|(k, r)| (k.as_str(), *r)).collect();
            let mut rows = self.cache.rows_by_ids(&keys).map_err(db)?;
            for r in &mut rows {
                if let Some(read) = by_key.get(r.id.as_str()) {
                    r.set_keyword("$seen", *read);
                }
            }
            flagged = rows.len();
            self.cache.upsert_emails(&rows).map_err(db)?;
        }
        self.cache.set_state(&state_key, state.as_str()).map_err(db)?;
        Ok(fetched > 0 || !gone.is_empty() || flagged > 0)
    }

    /// Первый вход в папку: последние [`INITIAL`] писем, затем состояние `SyncFolderItems` «сейчас».
    async fn first_load(&mut self, f: &Folder, state_key: &str) -> postman_ews::Result<bool> {
        let (ids, _) = self.client.mail_page(&f.at(), 0, INITIAL).await?;
        let added = self.headers_into(&ids, &f.key).await?;
        let mut state: Option<SyncState> = None;
        for _ in 0..DRAIN_PAGES {
            let page = self.client.sync_mail(&f.at(), state.as_ref(), SYNC_PAGE).await?;
            state = Some(page.state);
            if page.done {
                break;
            }
        }
        if let Some(s) = state {
            self.cache.set_state(state_key, s.as_str()).map_err(db)?;
        }
        Ok(added > 0)
    }

    /// Письмо целиком: из кэша (блоб под ключом письма) или с сервера (`MimeContent`).
    async fn raw(&mut self, key: &str) -> postman_ews::Result<Vec<u8>> {
        let key = key.split_once('#').map_or(key, |(k, _)| k);
        if let Some(b) = self.cache.blob(key).map_err(db)? {
            return Ok(b);
        }
        let id = parse_key(key).ok_or_else(|| postman_ews::Error::Xml(format!("not an ews key: {key}")))?;
        let mail =
            self.client.get_mail(&id).await?.ok_or_else(|| postman_ews::Error::Ews { code: "ErrorItemNotFound".into(), message: String::new() })?;
        self.cache.put_blob(key, &mail.mime).map_err(db)?;
        Ok(mail.mime)
    }

    /// Работа воркера-песочницы — в отдельном потоке (он блокирует).
    async fn in_worker<T: Send + 'static>(
        &mut self,
        f: impl FnOnce(&mut postman_worker::Client) -> Result<T, String> + Send + 'static,
    ) -> postman_ews::Result<T> {
        let mut worker = self.worker.take().unwrap_or_else(|| postman_worker::Client::new(postman_worker::Client::default_exe(), WORKER_TIMEOUT));
        let (worker, out) = tokio::task::spawn_blocking(move || {
            let out = f(&mut worker);
            (worker, out)
        })
        .await
        .map_err(|e| postman_ews::Error::Xml(format!("worker task: {e}")))?;
        self.worker = Some(worker);
        out.map_err(|e| postman_ews::Error::Xml(format!("worker: {e}")))
    }

    async fn body(&mut self, key: &str) -> postman_ews::Result<()> {
        let raw = self.raw(key).await?;
        let m = self.in_worker(move |w| w.mime(&raw)).await?;
        let addrs = |v: Vec<postman_worker::mime::Addr>| v.into_iter().map(|a| Addr { name: a.name, email: a.email }).collect();
        let (message_id, references) = (m.message_id.clone(), m.references.clone());
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
        if let Some(mut r) = self.cache.rows_by_ids(&[key.to_owned()]).map_err(db)?.pop() {
            let mut dirty = false;
            // Превью — из текста письма, как у JMAP.
            if r.preview.is_empty()
                && let Some(text) = m.text
            {
                r.preview = text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(256).collect();
                dirty = true;
            }
            // Цепочка — по `References` из MIME (в списке у Exchange 2013 их нет).
            if r.thread_id == r.id {
                let t = thread_key(self.scope(), &references, None, message_id.as_deref(), &r.id);
                if t != r.thread_id {
                    r.thread_id = t;
                    dirty = true;
                }
            }
            if dirty {
                self.cache.upsert_emails(&[r]).map_err(db)?;
            }
        }
        Ok(())
    }

    async fn blob(&mut self, blob_id: &str) -> postman_ews::Result<Vec<u8>> {
        if let Some(b) = self.cache.blob(blob_id).map_err(db)? {
            return Ok(b);
        }
        let raw = self.raw(blob_id).await?;
        let Some((_, index)) = blob_id.split_once('#') else { return Ok(raw) };
        let index: u32 = index.parse().map_err(|_| postman_ews::Error::Xml(format!("bad blob {blob_id}")))?;
        let data = self.in_worker(move |w| w.mime_part(&raw, index)).await?;
        self.cache.put_blob(blob_id, &data).map_err(db)?;
        Ok(data)
    }

    fn ids(keys: &[String]) -> Vec<ItemId> {
        keys.iter().filter_map(|k| parse_key(k)).collect()
    }

    /// Папки писем по ключам (из кэша) — сверить после правки.
    fn folders_of(&self, keys: &[String]) -> Vec<String> {
        let mut out: Vec<String> = self.cache.rows_by_ids(keys).unwrap_or_default().into_iter().flat_map(|r| r.mailbox_ids).collect();
        out.sort();
        out.dedup();
        out
    }

    /// «Прочитано» и флаг — на сервер; другие ключевые слова EWS не хранит.
    async fn set_keywords(&mut self, keys: &[String], add: &[String], remove: &[String]) -> postman_ews::Result<()> {
        let has = |v: &[String], k: &str| v.iter().any(|x| x.eq_ignore_ascii_case(k));
        let read = if has(add, "$seen") {
            Some(true)
        } else if has(remove, "$seen") {
            Some(false)
        } else {
            None
        };
        let flag = if has(add, "$flagged") {
            Some(true)
        } else if has(remove, "$flagged") {
            Some(false)
        } else {
            None
        };
        for id in Self::ids(keys) {
            if let Some(r) = read {
                self.client.set_read(&id, r).await?;
            }
            if let Some(f) = flag {
                self.client.set_flag(&id, f).await?;
            }
        }
        // Строки кэша — сразу (сервер ответил); опрос сверит.
        let mut rows = self.cache.rows_by_ids(keys).map_err(db)?;
        for r in &mut rows {
            for k in add {
                r.set_keyword(k, true);
            }
            for k in remove {
                r.set_keyword(k, false);
            }
        }
        self.cache.upsert_emails(&rows).map_err(db)
    }

    async fn move_letters(&mut self, keys: &[String], to: &Folder) -> postman_ews::Result<()> {
        let ids = Self::ids(keys);
        if !ids.is_empty() {
            self.client.move_mail(&ids, &to.at()).await?;
            // У перенесённого — новый id: прежние строки убрать, новые придут сверкой папки.
            self.cache.delete_emails(keys).map_err(db)?;
        }
        Ok(())
    }

    async fn destroy(&mut self, keys: &[String]) -> postman_ews::Result<()> {
        let ids = Self::ids(keys);
        if !ids.is_empty() {
            self.client.delete_mail(&ids, DeleteMode::HardDelete).await?;
            self.cache.delete_emails(keys).map_err(db)?;
        }
        Ok(())
    }

    /// Сверить папки по ключам и сказать интерфейсу.
    async fn after(&mut self, folders: &[String], notify: &impl Fn(Event)) {
        let list: Vec<Folder> = self.folders.iter().filter(|f| folders.contains(&f.key) || folders.contains(&f.hash)).cloned().collect();
        let mut changed = false;
        for f in &list {
            match self.refresh(f).await {
                Ok(c) => changed |= c,
                Err(e) => tracing::warn!("ews refresh: {e}"),
            }
        }
        let _ = self.write_folders();
        notify(Event::MailboxesChanged);
        if changed {
            notify(Event::EmailsChanged);
        }
    }

    /// Письмо — в байты (S/MIME «Конфиденциально» — как у JMAP); вложения пересылки — из кэша или с сервера.
    async fn message_bytes(&mut self, draft: &mut Draft, message_id: &str) -> postman_ews::Result<Vec<u8>> {
        for a in draft.attachments.iter_mut().filter(|a| a.data.is_empty()) {
            if let Some(blob) = a.blob_id.clone() {
                a.data = self.blob(&blob).await?;
            }
        }
        let now = jiff::Timestamp::now().as_second();
        let raw = postman_compose::build(draft, now, message_id).map_err(|e| postman_ews::Error::Xml(format!("building the message: {e}")))?;
        if draft.encrypt_to.is_empty() {
            return Ok(raw);
        }
        postman_core::smime::seal_outgoing(&raw, &draft.encrypt_to).map_err(|e| postman_ews::Error::Xml(format!("{e:?}")))
    }

    async fn send(&mut self, mut draft: Draft, replace: Option<String>) -> postman_ews::Result<()> {
        let now = jiff::Timestamp::now().as_second();
        let id = postman_compose::new_message_id(&draft.from, now);
        let raw = self.message_bytes(&mut draft, &id).await?;
        // Копию в «Отправленные» кладёт сервер (`SendAndSaveCopy`).
        self.client.send_mail(&raw, true).await?;
        if let Some(old) = replace.filter(|k| parse_key(k).is_some()) {
            self.destroy(&[old]).await?;
        }
        Ok(())
    }

    /// Черновик — в «Черновики» (`SaveOnly`), прежний — удалить; ключ нового — по его id.
    async fn save_draft(&mut self, mut draft: Draft, replace: Option<String>) -> postman_ews::Result<String> {
        let now = jiff::Timestamp::now().as_second();
        let id = postman_compose::new_message_id(&draft.from, now);
        // Черновик «Конфиденциально» на сервере — открытым: шифруется только отправляемое.
        draft.encrypt_to.clear();
        let raw = self.message_bytes(&mut draft, &id).await?;
        let old = replace.as_deref().and_then(parse_key);
        let saved = self.client.save_draft(&raw, old.as_ref()).await?;
        if let Some(k) = replace {
            self.cache.delete_emails(&[k]).map_err(db)?;
        }
        Ok(email_key(self.scope(), &saved))
    }

    /// Поиск AQS по папке или по всем; найденное, которого нет в кэше, — в кэш.
    async fn query(&mut self, aqs: &str, mailbox: Option<&str>) -> postman_ews::Result<Vec<String>> {
        let folders: Vec<Folder> = match mailbox {
            Some(k) => self.folder_by_key(k).cloned().into_iter().collect(),
            None => self.folders.clone(),
        };
        let scope = self.scope.clone();
        let mut out = Vec::new();
        for f in folders {
            let ids = self.client.search_mail(&f.at(), aqs, SEARCH_LIMIT).await?;
            let keys: Vec<String> = ids.iter().map(|i| email_key(scope.as_deref(), i)).collect();
            let known: HashSet<String> = self.cache.rows_by_ids(&keys).map_err(db)?.into_iter().map(|r| r.id).collect();
            let missing: Vec<ItemId> = ids.iter().zip(&keys).filter(|(_, k)| !known.contains(*k)).map(|(i, _)| i.clone()).collect();
            self.headers_into(&missing, &f.key).await?;
            out.extend(keys);
        }
        Ok(out)
    }

    /// Следующая страница папки: письма старше уже загруженных (по дате получения).
    async fn load_more(&mut self, mailbox: &str) -> postman_ews::Result<usize> {
        let f = self.folder_by_key(mailbox).cloned().ok_or_else(|| postman_ews::Error::Xml(format!("no folder {mailbox}")))?;
        let have = u32::try_from(self.cache.count(&f.key).map_err(db)?).unwrap_or(u32::MAX);
        let (ids, _) = self.client.mail_page(&f.at(), have, PAGE).await?;
        let scope = self.scope.clone();
        let keys: Vec<String> = ids.iter().map(|i| email_key(scope.as_deref(), i)).collect();
        let known: HashSet<String> = self.cache.rows_by_ids(&keys).map_err(db)?.into_iter().map(|r| r.id).collect();
        let missing: Vec<ItemId> = ids.iter().zip(&keys).filter(|(_, k)| !known.contains(*k)).map(|(i, _)| i.clone()).collect();
        self.headers_into(&missing, &f.key).await
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
                tracing::warn!("ews held send: {e}");
                notify(Event::Error(sync_error(Stage::Command, &e)));
            }
        }
        let folders = self.sent_and_drafts();
        self.after(&folders, notify).await;
    }

    fn sent_and_drafts(&self) -> Vec<String> {
        [MailRole::Sent, MailRole::Drafts].iter().filter_map(|r| self.folder_by_role(*r).map(|f| f.key.clone())).collect()
    }

    fn hold(&mut self, draft: Draft, replace: Option<String>, secs: u32) -> postman_core::send_later::Scheduled {
        self.next_hold += 1;
        let id = scoped(self.scope(), &format!("hold.{}", self.next_hold));
        let send_at = jiff::Timestamp::now().as_second() + i64::from(secs);
        let at = tokio::time::Instant::now() + Duration::from_secs(u64::from(secs));
        self.held.push(Held { id: id.clone(), draft, replace, at });
        postman_core::send_later::Scheduled { submission: id.clone(), email: id, send_at }
    }

    async fn handle(&mut self, cmd: Command, notify: &impl Fn(Event)) -> postman_ews::Result<()> {
        match cmd {
            Command::Body(key) => {
                self.body(&key).await?;
                notify(Event::BodyReady(key));
            }
            Command::Thread(thread) => {
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
                let target = self.folder_by_key(&to).cloned().ok_or_else(|| postman_ews::Error::Xml(format!("no folder {to}")))?;
                self.move_letters(std::slice::from_ref(&id), &target).await?;
                self.after(&[from, to], notify).await;
            }
            Command::Destroy(id) => {
                let folders = self.folders_of(std::slice::from_ref(&id));
                self.destroy(&[id]).await?;
                self.after(&folders, notify).await;
            }
            Command::Patch { patches, .. } => {
                let mut touched = Vec::new();
                for p in patches {
                    touched.extend(self.folders_of(std::slice::from_ref(&p.id)));
                    let add: Vec<String> = p.keywords.iter().filter(|(_, on)| *on).map(|(k, _)| k.clone()).collect();
                    let remove: Vec<String> = p.keywords.iter().filter(|(_, on)| !*on).map(|(k, _)| k.clone()).collect();
                    self.set_keywords(std::slice::from_ref(&p.id), &add, &remove).await?;
                    let to = p.mailboxes.iter().find(|(_, on)| *on).and_then(|(k, _)| {
                        if k == postman_core::lifecycle::ARCHIVE {
                            self.folder_by_role(MailRole::Archive).cloned()
                        } else {
                            self.folder_by_key(k).cloned()
                        }
                    });
                    if let Some(to) = to {
                        self.move_letters(std::slice::from_ref(&p.id), &to).await?;
                        touched.push(to.key.clone());
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
                let q = format!("\"{}\"", query.replace('"', " "));
                let ids = self.query(&q, mailbox.as_deref()).await?;
                let total = ids.len() as u64;
                notify(Event::SearchResults { ticket, ids, total });
                notify(Event::EmailsChanged);
            }
            Command::Query { filter, mailbox, ticket } => {
                let ids = match aqs(&filter) {
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
                    counts.push(match aqs(f) {
                        Some((q, inside)) => self.query(&q, inside.as_deref().or(mailbox.as_deref())).await?.len() as u64,
                        None => 0,
                    });
                }
                notify(Event::Counts { ticket, counts: Some(counts) });
            }
            Command::Send { draft, replace, ticket, release } => {
                let hold = match release {
                    postman_core::model::Release::At(_) => {
                        notify(Event::ComposeFailed { ticket, error: "send later is not available for Exchange".into() });
                        return Ok(());
                    }
                    postman_core::model::Release::After(s) => s,
                    postman_core::model::Release::Now => 0,
                };
                if hold > 0 {
                    let scheduled = self.hold(*draft, replace, hold);
                    notify(Event::Sent { ticket, scheduled: Some(scheduled) });
                    return Ok(());
                }
                match self.send(*draft, replace).await {
                    Ok(()) => {
                        notify(Event::Sent { ticket, scheduled: None });
                        let folders = self.sent_and_drafts();
                        self.after(&folders, notify).await;
                    }
                    Err(e) => notify(Event::ComposeFailed { ticket, error: e.to_string() }),
                }
            }
            Command::SaveDraft { draft, replace, ticket } => match self.save_draft(*draft, replace).await {
                Ok(id) => {
                    notify(Event::DraftSaved { ticket, id });
                    let drafts: Vec<String> = self.folder_by_role(MailRole::Drafts).map(|f| f.key.clone()).into_iter().collect();
                    self.after(&drafts, notify).await;
                }
                Err(e) => notify(Event::ComposeFailed { ticket, error: e.to_string() }),
            },
            Command::Scheduled => notify(Event::Scheduled { list: Vec::new(), max_delay: 0 }),
            Command::Unsend { scheduled, ticket } => match self.held.iter().position(|h| h.id == scheduled.submission) {
                Some(i) => {
                    self.held.remove(i);
                    notify(Event::Unsent { ticket, email: String::new() });
                }
                None => notify(Event::UnsendFailed { ticket, too_late: true, error: String::new() }),
            },
            // Соединение EWS поднимается само при следующем запросе; «снова на связи» — сверить сейчас, не ждать опроса.
            Command::Reconnect => {
                if let Err(e) = self.refresh_all(notify).await {
                    notify(Event::Error(e));
                }
            }
            Command::RespondInvite { invite, status, in_reply_to } => self.respond_invite(*invite, status, in_reply_to, notify).await,
            Command::Delegate { uid, ticket, .. } => {
                notify(Event::Delegated { ticket, uid, error: Some("no tasks on the server".into()), scheduled: Vec::new() });
            }
            Command::Withhold { scheduled, ticket } => {
                let mut too_late = false;
                for s in &scheduled {
                    match self.held.iter().position(|h| h.id == s.submission) {
                        Some(i) => drop(self.held.remove(i)),
                        None => too_late = true,
                    }
                }
                notify(Event::Withheld { ticket, too_late, error: None });
            }
            // Календарь, занятость, справочник — областям EWS; правил, автоответа и задач у Exchange в Postman нет.
            other => self.area_command(other, notify).await,
        }
        Ok(())
    }

    /// Ответ на приглашение: по встрече в календаре Exchange (ответ шлёт сервер), иначе — письмом организатору.
    async fn respond_invite(
        &mut self,
        inv: postman_calendar::Invitation,
        status: postman_calendar::Partstat,
        in_reply_to: Option<String>,
        notify: &impl Fn(Event),
    ) {
        let uid = inv.event.uid.clone();
        if let Some(a) = self.areas.as_mut() {
            match a.answer(&inv, status).await {
                Ok(true) => {
                    notify(Event::InviteAnswered { uid, status, delivery: postman_core::invite::Delivery::Server });
                    notify(Event::CalendarChanged);
                    return;
                }
                Ok(false) => {}
                Err(e) => return notify(Event::InviteFailed { uid, error: e.to_string() }),
            }
        }
        let me = Address::new("", &self.email);
        let Some(draft) = postman_core::invite::reply_draft(&inv, &me, status, in_reply_to, jiff::Timestamp::now().as_second()) else {
            return notify(Event::InviteFailed { uid, error: "the invitation has no organizer".into() });
        };
        match self.send(draft, None).await {
            Ok(()) => notify(Event::InviteAnswered { uid, status, delivery: postman_core::invite::Delivery::Mail { imported: false } }),
            Err(e) => notify(Event::InviteFailed { uid, error: e.to_string() }),
        }
    }

    async fn area_command(&mut self, cmd: Command, notify: &impl Fn(Event)) {
        let owner = self.profile.as_ref().map(|_| postman_calendar::Owner { email: self.email.clone(), name: self.email.clone() });
        if let Some(a) = self.areas.as_mut() {
            let ctx = AreaCtx { cache: &self.cache, profile: self.profile.as_deref(), owner: owner.as_ref(), notify };
            match postman_core::backend::Areas::handle(a, cmd, ctx).await {
                None => return,
                Some(back) => return tracing::debug!("ews: command not supported: {}", name(&back)),
            }
        }
        match cmd {
            // Без областей (календарь не подключился) справочник пуст.
            Command::FindPeople { text } => notify(Event::PeopleFound { text, people: Vec::new() }),
            Command::PersonDetails { email } => notify(Event::PersonDetails { email, person: None }),
            other => tracing::debug!("ews: command not supported: {}", name(&other)),
        }
    }
}

/// Имя команды для журнала.
fn name(c: &Command) -> &'static str {
    match c {
        Command::Events { .. } | Command::SearchEvents { .. } => "calendar",
        Command::Contacts | Command::SaveContact { .. } | Command::DeleteContact { .. } | Command::FindPeople { .. } => "contacts",
        Command::Tasks(_) | Command::Delegate { .. } => "tasks",
        Command::Settings(_) => "settings",
        Command::Categories(_) => "categories",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip() {
        let id = ItemId::new("AAMkAD+abc/def==");
        let key = email_key(Some("@1"), &id);
        assert!(!unscoped(&key).1.contains('/'), "в ключе нет разделителя аккаунта");
        assert_eq!(parse_key(&key).map(|i| i.id), Some(id.id.clone()));
        assert_eq!(parse_key(&format!("{key}#3")).map(|i| i.id), Some(id.id));
        assert_eq!(parse_key("f0123"), None);
        assert!(is_mail_address("user1@example.test") && !is_mail_address(r"EXAMPLE\user1"));
        assert_eq!(host_of("https://mail.example.test:443/EWS/Exchange.asmx").as_deref(), Some("mail.example.test"));
    }

    #[test]
    fn header_becomes_row() {
        let h = MailHeader {
            id: ItemId::new("AAMkADmsg1"),
            folder_id: None,
            subject: "Отчёт".into(),
            from_name: "Анна".into(),
            from_addr: "anna@example.test".into(),
            received_at: 100,
            size: 2048,
            is_read: true,
            flagged: true,
            has_attachments: true,
            message_id: Some("<b@example.test>".into()),
            in_reply_to: None,
            references: Some("<root@example.test> <a@example.test>".into()),
        };
        let r = row(None, "finbox", &h);
        assert!(r.seen && r.flagged && r.has_attachment);
        assert_eq!((r.mailbox_ids.as_slice(), r.size), (&["finbox".to_owned()][..], 2048));
        let root_only = thread_key(None, &["<root@example.test>".into()], None, None, "x");
        assert_eq!(r.thread_id, root_only, "цепочка — по началу References");
        let lone = row(None, "f", &MailHeader { message_id: None, references: None, ..h });
        assert_eq!(lone.thread_id, lone.id);
    }

    #[test]
    fn filters_become_aqs() {
        use postman_core::model::Filter as F;
        assert_eq!(aqs(&F::From("anna".into())), Some(("from:\"anna\"".into(), None)));
        let (q, inside) = aqs(&F::And(vec![F::InMailbox("fx".into()), F::NotKeyword("$seen".into()), F::Subject("отчёт".into())])).unwrap();
        assert_eq!((q.as_str(), inside.as_deref()), ("(isread:false AND subject:\"отчёт\")", Some("fx")));
        assert_eq!(aqs(&F::HasKeyword("pm.due".into())), None, "своих ключевых слов у EWS нет");
        assert_eq!(aqs(&F::InMailbox("fx".into())), None);
    }
}
