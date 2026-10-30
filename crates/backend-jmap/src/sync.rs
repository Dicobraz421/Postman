//! Синхронизация JMAP: сессия учётной записи в общем цикле `postman_core::sync::supervise` (поток, «без связи» с
//! очередью правок) — вход, первая загрузка, push (EventSource), опрос-страховка, предзагрузка тел — и команды
//! договора `postman_core::sync::Command` → события `Event`. UI шлёт команды, в ответ получает события и перечитывает
//! кэш.
//!
//! Перенос из другой учётки (S8-13, ADR 0006 п. 4) — шаги задания ядра: письмо целиком (`FetchRaw` — блоб
//! `Email.blobId`), выгрузка (`Import` — `uploadUrl` и `Email/import` в папку со всеми ключевыми словами и датой
//! получения; повтор без дубля — `alreadyExists`), перенос сервером между аккаунтами сессии (`ServerCopy` —
//! `Email/copy` с `onSuccessDestroyOriginal`), удаление у источника (`DestroyLetters` — `Email/set` `destroy`, ответ по
//! письму).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use postman_cache::{AccountRow, Addr, AttachmentRow, BodyRow, Cache, EmailRow, MailboxRow, Recipients, Rights, in_profile, scoped, unscoped};
use postman_calendar::{Invitation, Partstat};
use postman_compose::{Address, Draft};
use postman_core::backend::BoxFuture;
use postman_core::invite::Delivery;
use postman_core::lifecycle;
use postman_core::link::{Connection, Link};
use postman_core::model::{ContactCard, Filter, FolderId, ImportError, JobId, LetterId, Principal, Release, Remote};
use postman_core::send_later::{self, Scheduled};
use postman_core::sync::{
    Account, Command, Debounce, ErrorKind, Event, Index, Sender, Session, Stage, Status, SyncError, UnboundedSender, owned_by, retry_at, supervise,
    went_down,
};
use postman_core::tasks;
use postman_jmap::{Client, EmailAddress, EmailBody, EmailSummary, Envelope, Identity, Mailbox, utc_to_unix};
use postman_jmap::{MailboxRights, SharedAccount, UnsendError};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::convert::{FromJmap, ToJmap};
use crate::error::{Error, Kinded, db_err, kind_of, sync_error};
use crate::tasks::JmapStore;
use crate::{categories, invite, schedule, settings};

/// Сколько писем на ящик тянуть при первом входе (остальное — по мере прокрутки, позже).
const INITIAL_PAGE: u64 = 500;
/// Опрос — только страховка: изменения приходят через push (EventSource).
const POLL: Duration = Duration::from_secs(120);
/// Сервер шлёт ping с этим периодом; тишина втрое дольше — обрыв, переподключаемся.
const PUSH_PING: u32 = 60;
/// Предзагрузка тел: сколько писем за запрос, их размер и общий охват.
const PREFETCH_BATCH: usize = 25;
const PREFETCH_MAX_BYTES: u64 = 256 * 1024;
const PREFETCH_LIMIT: usize = 20_000;
/// Догрузка при прокрутке.
const PAGE: u64 = 500;
const SEARCH_LIMIT: u64 = 1000;
/// Изменения календаря приходят пачкой (событие, уведомление, ответ участника): неделя — одним запросом.
const CALENDAR_DEBOUNCE: Duration = Duration::from_millis(1500);
/// Справочник целиком — не больше (подсказки адресов и раздел «Контакты»).
const DIRECTORY_LIMIT: u64 = 10_000;
/// Людей справочника на одну строку поиска (`FindPeople`), как у EWS.
const PEOPLE_LIMIT: u64 = 20;
/// Поиск справочника — со скольки символов.
const PEOPLE_FROM: usize = 3;
/// Тела писем беседы: части больше — усекаются сервером (как у открытого письма — до 4 МБ, здесь писем много).
const THREAD_BODY_BYTES: u64 = 1024 * 1024;

pub fn spawn(
    account: Account,
    cache: Cache,
    index: Index,
    events: mpsc::Sender<Event>,
    repaint: impl Fn() + Send + 'static,
) -> UnboundedSender<Command> {
    spawn_as(account, None, cache, index, events, repaint)
}

/// Синхронизация учётной записи профиля (S7-03б): `profile` — `None` у основной, `@<n>` у дополнительной. Ключи
/// кэша дополнительной — с её префиксом (`postman_cache::in_profile`), её общие ящики — тоже. Дополнительная
/// синхронизирует почту, отправителей и календарь; задачи, контакты, справочник, правила, автоответ и категории — у
/// основной (`Event::Me` и `Event::Features` — тоже только у неё).
pub fn spawn_as(
    account: Account,
    profile: Option<String>,
    cache: Cache,
    index: Index,
    events: mpsc::Sender<Event>,
    repaint: impl Fn() + Send + 'static,
) -> UnboundedSender<Command> {
    let (tx, rx) = unbounded_channel();
    postman_core::sync::spawn_thread("sync", events, Box::new(repaint), move |notify| async move {
        let mut session = Jmap { account, profile, cache, index };
        supervise(&mut session, rx, &|e| notify.send(e)).await;
    });
    tx
}

/// Сессия JMAP для общего цикла: каждая попытка входа — с начала (`run`).
struct Jmap {
    account: Account,
    profile: Option<String>,
    cache: Cache,
    index: Index,
}

impl Session for Jmap {
    fn run<'a>(
        &'a mut self,
        rx: &'a mut UnboundedReceiver<Command>,
        link: &'a mut Link<Command>,
        notify: &'a dyn Fn(Event),
    ) -> BoxFuture<'a, Result<(), SyncError>> {
        let (account, profile, cache, index) = (self.account.clone(), self.profile.clone(), self.cache.clone(), self.index.clone());
        Box::pin(async move { run(account, profile, cache, index, rx, link, &notify).await })
    }
}

/// Ошибку — в интерфейс; `true` — остановить синхронизацию (отказ во входе, `postman_core::sync::report`).
fn report(e: impl Kinded, stage: Stage, notify: &impl Fn(Event)) -> bool {
    postman_core::sync::report(sync_error(stage, &e), notify)
}

/// Где изменилась почта по событию push.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum MailChange {
    #[default]
    None,
    /// В этих аккаунтах (id сервера): свой и общие ящики (S4-06) сверяются по отдельности.
    In(BTreeSet<String>),
    /// Неизвестно где (событие не разобралось, связь восстановилась) — сверить все аккаунты.
    All,
}

impl MailChange {
    /// Сверить аккаунт `account`.
    pub fn touches(&self, account: &str) -> bool {
        match self {
            Self::None => false,
            Self::In(set) => set.contains(account),
            Self::All => true,
        }
    }

    fn or(self, o: Self) -> Self {
        match (self, o) {
            (Self::All, _) | (_, Self::All) => Self::All,
            (Self::None, x) | (x, Self::None) => x,
            (Self::In(mut a), Self::In(b)) => {
                a.extend(b);
                Self::In(a)
            }
        }
    }
}

/// Какие данные изменились по событию push (`state`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changed {
    pub mail: MailChange,
    pub calendar: bool,
    /// Файлы аккаунтов (FileNode): в них таблица категорий (S5-02б).
    pub files: MailChange,
}

impl Changed {
    const ALL: Self = Self { mail: MailChange::All, calendar: true, files: MailChange::All };

    fn or(self, o: Self) -> Self {
        Self { mail: self.mail.or(o.mail), calendar: self.calendar || o.calendar, files: self.files.or(o.files) }
    }

    /// Разбор `StateChange` (RFC 8620 §7.1): `{"@type":"StateChange","changed":{"<аккаунт>":{"Email":"…",…}}}`.
    /// Не разобралось — считаем, что изменилось всё (лишняя сверка дешевле пропущенного письма).
    pub fn of(data: &str) -> Self {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else { return Self::ALL };
        let Some(accounts) = v.get("changed").and_then(|c| c.as_object()) else { return Self::ALL };
        let mut mail = BTreeSet::new();
        let mut files = BTreeSet::new();
        let mut calendar = false;
        for (account, types) in accounts.iter().filter_map(|(a, t)| Some((a, t.as_object()?))) {
            for kind in types.keys() {
                match kind.as_str() {
                    "Email" | "Mailbox" | "Thread" | "EmailDelivery" => {
                        mail.insert(account.clone());
                    }
                    "CalendarEvent" | "Calendar" | "CalendarEventNotification" => calendar = true,
                    "FileNode" => {
                        files.insert(account.clone());
                    }
                    _ => {}
                }
            }
        }
        let set = |s: BTreeSet<String>| if s.is_empty() { MailChange::None } else { MailChange::In(s) };
        Self { mail: set(mail), calendar, files: set(files) }
    }
}

/// Почтовый аккаунт синхронизации: свой или общий ящик (S4-06). Письма и папки общего — в кэше с префиксом
/// аккаунта (`postman_cache::scoped`), команды интерфейса приходят с этими ключами.
struct MailAccount {
    /// id аккаунта на сервере.
    id: String,
    /// Префикс ключей кэша: `None` — свой аккаунт.
    scope: Option<String>,
    /// Почтовые методы — в этом аккаунте (`Client::in_account`).
    client: Client,
    /// Состояние `Email` — точка отсчёта `Email/changes`; `None` — первая загрузка ещё не удалась.
    state: Option<String>,
}

impl MailAccount {
    fn scope(&self) -> Option<&str> {
        self.scope.as_deref()
    }

    fn key(&self, id: &str) -> String {
        scoped(self.scope(), id)
    }

    fn state_key(&self) -> String {
        self.key("Email")
    }
}

/// Свой аккаунт и общие ящики.
struct Accounts {
    own: MailAccount,
    shared: Vec<MailAccount>,
    /// Учётная запись профиля (S7-03б): `None` — основная, `@<n>` — дополнительная.
    profile: Option<String>,
    /// Владелец событий дополнительной учётной записи в общем календаре (как у календарей коллег, S5-22).
    me: Option<postman_calendar::Owner>,
}

impl Accounts {
    fn all_mut(&mut self) -> impl Iterator<Item = &mut MailAccount> {
        std::iter::once(&mut self.own).chain(self.shared.iter_mut())
    }

    fn by_scope(&self, scope: Option<&str>) -> Option<&MailAccount> {
        std::iter::once(&self.own).chain(&self.shared).find(|a| a.scope() == scope)
    }

    /// Аккаунт и id сервера по ключу кэша.
    fn locate<'a>(&'a self, key: &'a str) -> postman_jmap::Result<(&'a MailAccount, &'a str)> {
        let (scope, id) = unscoped(key);
        self.by_scope(scope).map(|a| (a, id)).ok_or_else(|| postman_jmap::Error::Protocol(format!("no account for {key}")))
    }
}

/// Отправитель для синхронизации: Identity и аккаунт, из которого уходит письмо.
#[derive(Debug, Clone)]
struct SendAs {
    scope: Option<String>,
    identity: Identity,
}

/// Отправители по порядку: свои Identity, затем общих ящиков. Адрес, который есть и у своего аккаунта, и у общего
/// ящика (Stalwart заводит члену группы Identity с адресом группы), — из общего: копия ляжет в его «Отправленные»,
/// где её видят все, кто работает с ящиком.
/// `profile` — учётная запись профиля (S7-03б): аккаунт отправителя — с её префиксом.
fn senders(own: &[Identity], shared: &[(String, Vec<Identity>)], profile: Option<&str>) -> Vec<SendAs> {
    let theirs = |email: &str| shared.iter().any(|(_, l)| l.iter().any(|i| i.email.eq_ignore_ascii_case(email)));
    let mut out: Vec<SendAs> =
        own.iter().filter(|i| !theirs(&i.email)).map(|i| SendAs { scope: in_profile(profile, None), identity: i.clone() }).collect();
    for (account, list) in shared {
        out.extend(list.iter().map(|i| SendAs { scope: in_profile(profile, Some(account)), identity: i.clone() }));
    }
    out
}

fn account_row(a: &SharedAccount) -> AccountRow {
    AccountRow { id: a.id.clone(), name: a.name.clone(), is_personal: a.is_personal, read_only: a.is_read_only }
}

async fn run(
    account: Account,
    profile: Option<String>,
    mut cache: Cache,
    index: Index,
    rx: &mut UnboundedReceiver<Command>,
    link: &mut Link<Command>,
    notify: &impl Fn(Event),
) -> Result<(), SyncError> {
    notify(Event::Status(Status::Connecting { url: account.url.clone() }));
    let client = Client::connect(&account.url, account.auth.to_jmap()).await.map_err(|e| sync_error(Stage::Connect, &e))?;
    notify(Event::Status(Status::SignedIn { user: client.session().username.clone() }));
    let own_id = client.mail_account().map_err(|e| sync_error(Stage::Connect, &e))?.to_string();

    // Общие и делегированные ящики (S4-06) — аккаунты сессии, кроме своего.
    let shared = client.session().shared_accounts();
    // Что видно на экране — по основной учётной записи (S7-03б). Выгрузка писем из других учёток (S8-13) — всегда,
    // перенос сервером (`Email/copy`) — между аккаунтами сессии, если их несколько.
    if profile.is_none() {
        notify(Event::Features(crate::convert::features(&client).with_import(true).with_server_copy(!shared.is_empty())));
    }
    let main = profile.is_none();
    let p = profile.as_deref();
    // Дополнительная учётная запись — группой в дереве папок, как общий ящик (S7-03б): строка её самой и её общих.
    let mut rows: Vec<AccountRow> = Vec::new();
    if let Some(p) = p {
        let name = client.session().username.clone();
        rows.push(AccountRow { id: p.to_owned(), name, is_personal: true, read_only: false });
    }
    rows.extend(shared.iter().map(|a| AccountRow { id: in_profile(p, Some(&a.id)).unwrap_or_default(), ..account_row(a) }));
    cache.replace_accounts_of(p, &rows).map_err(|e| sync_error(Stage::Load, &db_err(e)))?;
    let identities = client.identities().await.unwrap_or_default();
    if let Some(i) = identities.first().filter(|_| main) {
        notify(Event::Me { address: Address::new(&i.name, &i.email), signature: i.text_signature.clone() });
    }
    let me = identities
        .first()
        .map(|i| postman_calendar::Owner { email: i.email.clone(), name: if i.name.trim().is_empty() { i.email.clone() } else { i.name.clone() } });
    let mut theirs = Vec::new();
    for a in shared.iter().filter(|a| a.can_submit && !a.is_read_only) {
        // Ящик открыт, но не мой (Stalwart: `forbidden`, «not an owner») — отправлять от него нельзя, это не ошибка.
        if let Ok(list) = client.in_account(&a.id).identities().await {
            theirs.push((a.id.clone(), list));
        }
    }
    let send_as = senders(&identities, &theirs, p);
    notify(Event::Senders(
        send_as
            .iter()
            .map(|f| Sender {
                account: f.scope.clone(),
                address: Address::new(&f.identity.name, &f.identity.email),
                signature: f.identity.text_signature.clone(),
            })
            .collect(),
    ));
    // Контакты, справочник, правила, автоответ, категории и задачи — у основной учётной записи (S7-03б).
    if main {
        // Контакты и справочник — до писем: один-два запроса, нужны подсказкам адресов с первого окна письма.
        load_people(&client, notify).await;
        settings::handle(&client, settings::Request::Load, &|r| notify(Event::Settings(r))).await;
        // Таблицы категорий (свои и общих ящиков) — до писем: полосы и метки списка — сразу с ними.
        let shared_ids: Vec<String> = shared.iter().map(|a| a.id.clone()).collect();
        categories::load_all(&client, &own_id, &shared_ids, &|r| notify(Event::Categories(r))).await;
        // Задачи — из кэша сразу, с сервера — дельта; сбой не мешает почте (повтор — по push календаря).
        if let Err(e) = tasks::handle(&JmapStore::new(&client), &cache, tasks::Request::Load, &|r| notify(Event::Tasks(r))).await {
            tracing::warn!("tasks load failed: {e}");
        }
    }

    let mail = |id: &str, scope: Option<String>, client: Client| MailAccount { id: id.into(), scope, client, state: None };
    let mut accounts = Accounts {
        own: mail(&own_id, in_profile(p, None), client.clone()),
        shared: shared.iter().map(|a| mail(&a.id, in_profile(p, Some(&a.id)), client.in_account(&a.id))).collect(),
        profile: profile.clone(),
        me: me.filter(|_| !main),
    };
    accounts.own.state = Some(start_state(&accounts.own, &mut cache, &index, notify).await.map_err(|e| sync_error(Stage::Load, &e))?);
    for acc in &mut accounts.shared {
        match start_state(acc, &mut cache, &index, notify).await {
            Ok(s) => acc.state = Some(s),
            // Общий ящик не загрузился — свой работает; повтор — со следующей сверкой.
            Err(e) => {
                if report(e, Stage::Load, notify) {
                    return Ok(());
                }
            }
        }
    }
    notify(Event::Status(Status::Synced));
    notify_scheduled(&accounts, &send_as, notify).await;
    let mut poll = tokio::time::interval(POLL);
    poll.tick().await;
    let (push_tx, mut push_rx) = unbounded_channel::<Changed>();
    tokio::spawn(push_loop(client.clone(), push_tx));
    let mut calendar = Debounce::default();
    // Тела писем без тела — в фоне, свежие первыми: чтение без сети и полнотекстовый поиск.
    let mut prefetch: VecDeque<String> = cache.missing_bodies(PREFETCH_LIMIT).unwrap_or_default().into();
    let mut refill = false;
    // Без связи (S5-08): срок следующей попытки; правки писем без связи — в `link`.
    let mut retry: Option<tokio::time::Instant> = None;
    // Вход удался — связь есть: правки, сделанные без связи (в том числе до входа), — на сервер.
    if let Some(queued) = link.ok() {
        notify(Event::Connection(Connection::Online));
        let ctx = Ctx { accounts: &accounts, identities: &identities, send_as: &send_as };
        match replay(&ctx, &mut cache, &index, link, queued, notify).await {
            Replayed::Stop => return Ok(()),
            Replayed::Down(at) => retry = Some(at),
            Replayed::Done => {}
        }
    }
    loop {
        tokio::select! {
            // Команды пользователя важнее фоновой работы.
            biased;
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { return Ok(()) };
                // Перенос папки в себя, в подпапку, в другой аккаунт — отказ ядра до сервера.
                let Some(cmd) = postman_core::sync::admit(cmd, || cache.mailboxes().unwrap_or_default(), notify) else { continue };
                if matches!(cmd, Command::Reconnect) {
                    if link.is_down() {
                        retry = Some(tokio::time::Instant::now());
                    }
                    continue;
                }
                // Без связи правки писем — сразу в очередь: каждая попытка ждала бы тайм-аута сети.
                if link.is_down()
                    && let Some(c) = cmd.replayable()
                {
                    link.queue(c);
                    let left = retry.map(|t| t.saturating_duration_since(tokio::time::Instant::now())).unwrap_or_default();
                    notify(Event::Connection(link.connection(left)));
                    continue;
                }
                // Догруженная страница — тоже в предзагрузку тел.
                let more = matches!(cmd, Command::LoadMore { .. });
                let again = cmd.replayable();
                let ctx = Ctx { accounts: &accounts, identities: &identities, send_as: &send_as };
                match handle(ctx.accounts, &mut cache, &index, ctx.identities, ctx.send_as, cmd, notify).await {
                    Ok(()) => {
                        if let Some(queued) = link.ok() {
                            notify(Event::Connection(Connection::Online));
                            retry = None;
                            match replay(&ctx, &mut cache, &index, link, queued, notify).await {
                                Replayed::Stop => return Ok(()),
                                Replayed::Down(at) => retry = Some(at),
                                Replayed::Done => {}
                            }
                        }
                    }
                    Err(e) => {
                        if kind_of(&e) == ErrorKind::Network {
                            if let Some(c) = again {
                                link.queue(c);
                            }
                            if !link.is_down() {
                                retry = Some(went_down(link, notify));
                            }
                        }
                        if report(e, Stage::Command, notify) {
                            return Ok(());
                        }
                    }
                }
                if more && prefetch.is_empty() {
                    prefetch.extend(cache.missing_bodies(PREFETCH_LIMIT).unwrap_or_default());
                }
            }
            () = retry_at(retry) => {
                retry = None;
                let mut outcome = Refreshed::Done;
                for acc in accounts.all_mut() {
                    outcome = outcome.max(apply_refresh(acc, &mut cache, &index, &mut prefetch, notify).await);
                    if outcome != Refreshed::Done {
                        break;
                    }
                }
                match after_refresh(outcome, true, &accounts, &identities, &send_as, &mut cache, &index, link, notify).await {
                    Next::Stop => return Ok(()),
                    Next::Retry(at) => retry = at,
                    Next::Keep => {}
                }
            }
            Some(first) = push_rx.recv() => {
                let mut changed = first;
                while let Ok(c) = push_rx.try_recv() {
                    changed = changed.or(c);
                }
                if changed.calendar {
                    calendar.poke(tokio::time::Instant::now(), CALENDAR_DEBOUNCE);
                }
                // Таблицу категорий поправил другой клиент (или эта же запись вернулась push) — перечитать.
                let files = std::iter::once(&accounts.own).chain(&accounts.shared).filter(|_| accounts.profile.is_none());
                for acc in files.filter(|a| changed.files.touches(&a.id)) {
                    let request = categories::Request::Load { account: acc.scope.clone() };
                    categories::handle(&client, &own_id, None, request, &|r| notify(Event::Categories(r))).await;
                }
                let mut outcome = Refreshed::Done;
                for acc in accounts.all_mut().filter(|a| changed.mail.touches(&a.id)) {
                    outcome = outcome.max(apply_refresh(acc, &mut cache, &index, &mut prefetch, notify).await);
                    if outcome == Refreshed::Stop {
                        return Ok(());
                    }
                }
                match after_refresh(outcome, false, &accounts, &identities, &send_as, &mut cache, &index, link, notify).await {
                    Next::Stop => return Ok(()),
                    Next::Retry(at) => retry = at,
                    Next::Keep => {}
                }
            }
            () = calendar.wait() => {
                if calendar.fire(tokio::time::Instant::now()) {
                    // Задачи — те же записи календаря (`CalendarEvent`): сверить и их (задачи — у основной учётной записи).
                    let store = JmapStore::new(&accounts.own.client);
                    if accounts.profile.is_none()
                        && let Err(e) = tasks::handle(&store, &cache, tasks::Request::Refresh, &|r| notify(Event::Tasks(r))).await
                    {
                        tracing::warn!("tasks refresh failed: {e}");
                    }
                    notify(Event::CalendarChanged);
                }
            }
            _ = poll.tick() => {
                let mut outcome = Refreshed::Done;
                for acc in accounts.all_mut() {
                    outcome = outcome.max(apply_refresh(acc, &mut cache, &index, &mut prefetch, notify).await);
                    if outcome == Refreshed::Stop {
                        return Ok(());
                    }
                }
                match after_refresh(outcome, false, &accounts, &identities, &send_as, &mut cache, &index, link, notify).await {
                    Next::Stop => return Ok(()),
                    Next::Retry(at) => retry = at,
                    Next::Keep => {}
                }
                // Предзагрузка прервалась ошибкой — остаток со следующего опроса. Полный обход кэша (а в большом
                // ящике это десятки тысяч строк) — только тогда, а не на каждое событие push.
                if std::mem::take(&mut refill) && prefetch.is_empty() {
                    prefetch.extend(cache.missing_bodies(PREFETCH_LIMIT).unwrap_or_default());
                }
            }
            // Без связи тела не тянем: каждая попытка — тайм-аут сети.
            _ = std::future::ready(()), if !prefetch.is_empty() && !link.is_down() => {
                let n = prefetch.len().min(PREFETCH_BATCH);
                let keys: Vec<String> = prefetch.drain(..n).collect();
                match prefetch_bodies(&accounts, &cache, &keys).await {
                    Ok(stored) => notify(Event::BodiesStored(stored)),
                    Err(e) => {
                        // Не долбим сервер: остаток подберёт следующий опрос.
                        prefetch.clear();
                        refill = true;
                        if kind_of(&e) == ErrorKind::Network && !link.is_down() {
                            retry = Some(went_down(link, notify));
                        }
                        if report(e, Stage::Prefetch, notify) {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
}

/// Что нужно командам писем помимо кэша: аккаунты и отправители (для `handle` и отправки очереди).
struct Ctx<'a> {
    accounts: &'a Accounts,
    identities: &'a [Identity],
    send_as: &'a [SendAs],
}

/// Итог отправки очереди: всё ушло, связь снова пропала (срок повтора) или синхронизацию остановить.
enum Replayed {
    Done,
    Down(tokio::time::Instant),
    Stop,
}

/// Связь вернулась: правки из очереди — на сервер по порядку. Сеть пропала посреди — остаток снова в очередь.
async fn replay(
    ctx: &Ctx<'_>,
    cache: &mut Cache,
    index: &Index,
    link: &mut Link<Command>,
    queued: Vec<Command>,
    notify: &impl Fn(Event),
) -> Replayed {
    let mut rest = queued.into_iter();
    while let Some(cmd) = rest.next() {
        let again = cmd.replayable();
        let Err(e) = handle(ctx.accounts, cache, index, ctx.identities, ctx.send_as, cmd, notify).await else { continue };
        if kind_of(&e) == ErrorKind::Network {
            let mut back: Vec<Command> = again.into_iter().collect();
            back.extend(rest);
            link.requeue(back);
            let at = went_down(link, notify);
            report(e, Stage::Command, notify);
            return Replayed::Down(at);
        }
        if report(e, Stage::Command, notify) {
            return Replayed::Stop;
        }
    }
    Replayed::Done
}

/// Что делать со сроком повтора после сверки.
enum Next {
    /// Срок прежний.
    Keep,
    /// Новый срок (или `None` — связь есть, повтора нет).
    Retry(Option<tokio::time::Instant>),
    Stop,
}

/// После сверки (push, опрос, повтор по сроку `timer`): сервер ответил — связь есть, очередь — на сервер; не
/// ответил — без связи (повтор по сроку не удался или связь только что пропала — следующий срок, длиннее).
#[allow(clippy::too_many_arguments)]
async fn after_refresh(
    outcome: Refreshed,
    timer: bool,
    accounts: &Accounts,
    identities: &[Identity],
    send_as: &[SendAs],
    cache: &mut Cache,
    index: &Index,
    link: &mut Link<Command>,
    notify: &impl Fn(Event),
) -> Next {
    match outcome {
        Refreshed::Stop => Next::Stop,
        Refreshed::Network if link.is_down() && !timer => Next::Keep,
        Refreshed::Network => Next::Retry(Some(went_down(link, notify))),
        Refreshed::Done => {
            // Почта сверена: задачи со ссылкой на письмо — по письмам (срок снят — выполнена, S6-03).
            let store = JmapStore::new(&accounts.own.client);
            if accounts.profile.is_none()
                && let Err(e) = tasks::handle(&store, cache, tasks::Request::Reconcile, &|r| notify(Event::Tasks(r))).await
            {
                tracing::warn!("mail tasks reconcile failed: {e}");
            }
            let Some(queued) = link.ok() else { return Next::Keep };
            notify(Event::Connection(Connection::Online));
            let ctx = Ctx { accounts, identities, send_as };
            match replay(&ctx, cache, index, link, queued, notify).await {
                Replayed::Down(at) => Next::Retry(Some(at)),
                Replayed::Done => Next::Retry(None),
                Replayed::Stop => Next::Stop,
            }
        }
    }
}

/// Тела писем по ключам кэша — запросом на аккаунт; ключи аккаунтов, которых больше нет, пропускаются.
async fn prefetch_bodies(accounts: &Accounts, cache: &Cache, keys: &[String]) -> postman_jmap::Result<Vec<String>> {
    let mut by_account: BTreeMap<Option<&str>, Vec<String>> = BTreeMap::new();
    for key in keys {
        let (scope, id) = unscoped(key);
        by_account.entry(scope).or_default().push(id.to_owned());
    }
    let mut stored = Vec::new();
    for (scope, ids) in by_account {
        let Some(acc) = accounts.by_scope(scope) else { continue };
        for (id, b) in acc.client.email_bodies(&ids, PREFETCH_MAX_BYTES).await? {
            let key = acc.key(&id);
            if cache.put_body(&key, &body_row(b, acc.scope())).is_ok() {
                stored.push(key);
            }
        }
    }
    Ok(stored)
}

/// Итог сверки аккаунта; по порядку тяжести (`max` по аккаунтам).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Refreshed {
    Done,
    /// Сервер не ответил (сеть): без связи.
    Network,
    /// Сервер отклонил вход: синхронизацию остановить.
    Stop,
}

/// Ошибка сверки — в интерфейс; что дальше.
fn refresh_failed(e: postman_jmap::Error, notify: &impl Fn(Event)) -> Refreshed {
    let network = kind_of(&e) == ErrorKind::Network;
    if report(e, Stage::Refresh, notify) {
        Refreshed::Stop
    } else if network {
        Refreshed::Network
    } else {
        Refreshed::Done
    }
}

/// Сверка аккаунта с сервером.
async fn apply_refresh(
    acc: &mut MailAccount,
    cache: &mut Cache,
    index: &Index,
    prefetch: &mut VecDeque<String>,
    notify: &impl Fn(Event),
) -> Refreshed {
    // Первая загрузка общего ящика не удалась — снова целиком.
    let Some(since) = acc.state.clone() else {
        return match start_state(acc, cache, index, notify).await {
            Ok(s) => {
                acc.state = Some(s);
                notify(Event::MailboxesChanged);
                notify(Event::EmailsChanged);
                Refreshed::Done
            }
            Err(e) => refresh_failed(e, notify),
        };
    };
    match refresh(acc, cache, index, &since, notify).await {
        Ok(Some((s, created))) => {
            acc.state = Some(s);
            // Новые письма — в начало очереди предзагрузки.
            for id in created.into_iter().rev() {
                prefetch.push_front(id);
            }
            notify(Event::MailboxesChanged);
            notify(Event::EmailsChanged);
            // Квота — своего ящика (строка состояния); у общего — своя, её не показываем.
            if acc.scope.is_none() {
                settings::handle(&acc.client, settings::Request::Quota, &|r| notify(Event::Settings(r))).await;
            }
            Refreshed::Done
        }
        Ok(None) => Refreshed::Done,
        Err(e) => refresh_failed(e, notify),
    }
}

/// Держит EventSource и будит основной цикл на каждое событие `state` (с тем, что изменилось).
async fn push_loop(client: Client, wake: UnboundedSender<Changed>) {
    let mut backoff = 5;
    // Первое подключение — сразу после загрузки: календарь ещё не открывали, сверять только почту.
    let mut first = true;
    let mail = || Changed { mail: MailChange::All, calendar: false, files: MailChange::None };
    loop {
        match client.event_source(PUSH_PING).await {
            Ok(mut es) => {
                backoff = 5;
                // Сразу после подключения — сверка: изменения могли случиться, пока связи не было.
                let missed = if first { mail() } else { Changed::ALL };
                first = false;
                if wake.send(missed).is_err() {
                    return;
                }
                let silence = Duration::from_secs(u64::from(PUSH_PING) * 3);
                while let Ok(Ok(Some(ev))) = tokio::time::timeout(silence, es.next()).await {
                    if ev.event == "state" && wake.send(Changed::of(&ev.data)).is_err() {
                        return;
                    }
                }
            }
            // Отказ во входе: сверка почты покажет его основному циклу, тот остановит синхронизацию.
            Err(e) if kind_of(&e) == ErrorKind::Auth => {
                if wake.send(mail()).is_err() {
                    return;
                }
            }
            Err(_) => {}
        }
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(300);
    }
}

/// Папки аккаунта — в кэш (ключи с префиксом аккаунта).
async fn load_mailboxes(acc: &MailAccount, cache: &Cache) -> postman_jmap::Result<Vec<Mailbox>> {
    let (mailboxes, _) = acc.client.mailboxes().await?;
    let rows: Vec<MailboxRow> = mailboxes.iter().map(|m| mailbox_row(m, acc.scope())).collect();
    cache.replace_mailboxes(acc.scope(), &rows).map_err(db_err)?;
    Ok(mailboxes)
}

/// Состояние почты аккаунта при запуске. Кэш уже синхронизирован (есть состояние `Email`) — папки и дельта с
/// прошлого раза; сервер дельту не считает (`cannotCalculateChanges`) или кэш пуст — первые страницы папок.
async fn start_state(acc: &MailAccount, cache: &mut Cache, index: &Index, notify: &impl Fn(Event)) -> postman_jmap::Result<String> {
    if let Some(since) = cache.state(&acc.state_key()).map_err(db_err)? {
        load_mailboxes(acc, cache).await?;
        notify(Event::MailboxesChanged);
        match refresh(acc, cache, index, &since, notify).await {
            Ok(changed) => {
                notify(Event::MailboxesChanged);
                notify(Event::EmailsChanged);
                return Ok(changed.map_or(since, |(s, _)| s));
            }
            Err(postman_jmap::Error::Method { kind, .. }) if kind == "cannotCalculateChanges" => {}
            Err(e) => return Err(e),
        }
    }
    initial_load(acc, cache, index, notify).await
}

async fn initial_load(acc: &MailAccount, cache: &mut Cache, index: &Index, notify: &impl Fn(Event)) -> postman_jmap::Result<String> {
    let mailboxes = load_mailboxes(acc, cache).await?;
    notify(Event::MailboxesChanged);

    // Входящие первыми — их пользователь видит сразу.
    let mut order: Vec<&Mailbox> = mailboxes.iter().collect();
    order.sort_by_key(|m| m.role.as_deref() != Some("inbox"));
    let mut state = String::new();
    for mb in order {
        notify(Event::Status(Status::Loading { mailbox: mb.name.clone() }));
        let page = acc.client.email_page(&mb.id, 0, INITIAL_PAGE).await?;
        save_summaries(cache, index, &page.emails, acc.scope(), notify)?;
        state = page.state;
        notify(Event::EmailsChanged);
    }
    // Ни одной папки (общий ящик без открытых мне папок) — состояние всё равно нужно для дельты.
    if state.is_empty() {
        state = acc.client.email_get(&[]).await?.1;
    }
    cache.set_state(&acc.state_key(), &state).map_err(db_err)?;
    Ok(state)
}

/// Дельта через `Email/changes`: `Some((новое состояние, ключи новых писем))`, если что-то изменилось.
async fn refresh(
    acc: &MailAccount,
    cache: &mut Cache,
    index: &Index,
    since: &str,
    notify: &impl Fn(Event),
) -> postman_jmap::Result<Option<(String, Vec<String>)>> {
    let mut state = since.to_string();
    let mut created = Vec::new();
    loop {
        let ch = acc.client.changes("Email", &state, 500).await?;
        if ch.new_state == state {
            return Ok(None);
        }
        created.extend(ch.created.iter().map(|id| acc.key(id)));
        let ids: Vec<String> = ch.created.into_iter().chain(ch.updated).collect();
        if !ids.is_empty() {
            let (emails, _) = acc.client.email_get(&ids).await?;
            save_summaries(cache, index, &emails, acc.scope(), notify)?;
        }
        let destroyed: Vec<String> = ch.destroyed.iter().map(|id| acc.key(id)).collect();
        cache.delete_emails(&destroyed).map_err(db_err)?;
        if let Some(i) = index {
            let _ = i.delete(&destroyed);
            let _ = i.commit();
        }
        state = ch.new_state;
        if !ch.has_more_changes {
            break;
        }
    }
    load_mailboxes(acc, cache).await?;
    cache.set_state(&acc.state_key(), &state).map_err(db_err)?;
    Ok(Some((state, created)))
}

async fn handle(
    accounts: &Accounts,
    cache: &mut Cache,
    index: &Index,
    identities: &[Identity],
    send_as: &[SendAs],
    cmd: Command,
    notify: &impl Fn(Event),
) -> Result<(), Error> {
    let client = &accounts.own.client;
    match cmd {
        Command::Body(key) => {
            let (acc, id) = accounts.locate(&key)?;
            let row = body_row(acc.client.email_body(id).await?, acc.scope());
            cache.put_body(&key, &row).map_err(db_err)?;
            notify(Event::BodyReady(key));
        }
        Command::Thread(key) => {
            let (acc, id) = accounts.locate(&key)?;
            let ids = load_thread(acc, cache, index, id, notify).await?;
            notify(Event::ThreadReady { thread: key, ids });
        }
        Command::Keyword { id: key, keyword, on } => {
            let (acc, id) = accounts.locate(&key)?;
            acc.client.set_keyword(id, keyword, on).await?;
        }
        Command::SetKeywords { ids, add, remove } => {
            let mut first_error = None;
            for (scope, keys) in by_account(&ids) {
                let Some(acc) = accounts.by_scope(scope) else { continue };
                let server: Vec<String> = keys.iter().map(|k| unscoped(k).1.to_owned()).collect();
                let failed: Vec<String> = match acc.client.set_keywords(&server, &server_words(&add), &server_words(&remove)).await {
                    Ok(f) => f
                        .into_iter()
                        .map(|(id, e)| {
                            first_error.get_or_insert(e);
                            acc.key(&id)
                        })
                        .collect(),
                    Err(e) => {
                        first_error.get_or_insert(e);
                        keys.clone()
                    }
                };
                // Сервер не поставил метки — в кэш письма такими, какие они на сервере.
                for key in &failed {
                    restore(accounts, cache, index, key, notify).await;
                }
            }
            if let Some(e) = first_error {
                return Err(e.into());
            }
        }
        Command::Query { filter, mailbox, ticket } => {
            let (acc, mb) = query_scope(accounts, mailbox.as_deref())?;
            let page = acc.client.filtered(&in_mailbox(mb, filter).to_jmap(), SEARCH_LIMIT).await?;
            save_summaries(cache, index, &page.emails, acc.scope(), notify)?;
            let ids = page.ids.iter().map(|id| acc.key(id)).collect();
            notify(Event::SearchResults { ticket, ids, total: page.total });
        }
        Command::Counts { filters, mailbox, ticket } => {
            let counted = match query_scope(accounts, mailbox.as_deref()) {
                Ok((acc, mb)) => {
                    let filters: Vec<postman_jmap::Filter> = filters.into_iter().map(|f| in_mailbox(mb, f).to_jmap()).collect();
                    acc.client.count(&filters).await
                }
                Err(e) => Err(e),
            };
            notify(Event::Counts { ticket, counts: counted.as_ref().ok().cloned() });
            counted?;
        }
        Command::Move { id: key, from, to } => {
            let moved = match accounts.locate(&key) {
                Ok((acc, id)) => acc.client.move_email(id, unscoped(&from).1, unscoped(&to).1).await,
                Err(e) => Err(e),
            };
            if let Err(e) = moved {
                restore(accounts, cache, index, &key, notify).await;
                return Err(e.into());
            }
        }
        Command::Destroy(key) => {
            let destroyed = match accounts.locate(&key) {
                Ok((acc, id)) => acc.client.destroy_email(id).await,
                Err(e) => Err(e),
            };
            if let Err(e) = destroyed {
                restore(accounts, cache, index, &key, notify).await;
                return Err(e.into());
            }
            if let Some(i) = index {
                let _ = i.delete(std::slice::from_ref(&key));
                let _ = i.commit();
            }
        }
        Command::Send { draft, replace, ticket, release } => {
            // «Конфиденциально» с получателями ГОСТ и RSA/EC — два письма (S5-12 Ф4): сначала ГОСТ, итог — по последнему.
            let r = match postman_core::smime::split_for_encryption(&draft) {
                Err(code) => Err(postman_jmap::Error::Protocol(code)),
                Ok(parts) => {
                    let last = parts.len().saturating_sub(1);
                    let mut r = Ok(None);
                    for (i, part) in parts.iter().enumerate() {
                        let replace = if i == last { replace.as_deref() } else { None };
                        r = send(accounts, cache, send_as, part, replace, release).await;
                        if r.is_err() {
                            break;
                        }
                    }
                    r
                }
            };
            let later = matches!((&r, release), (Ok(Some(_)), Release::At(_)));
            notify(match r {
                Ok(scheduled) => Event::Sent { ticket, scheduled },
                Err(e) => Event::ComposeFailed { ticket, error: e.to_string() },
            });
            // Запланированное — в список отложенных («Отправится в …»); окно «Отменить» закроется само.
            if later {
                notify_scheduled(accounts, send_as, notify).await;
            }
        }
        Command::Unsend { scheduled, ticket } => {
            notify(match unsend(accounts, cache, &scheduled).await {
                Ok(()) => Event::Unsent { ticket, email: scheduled.email.clone() },
                Err(UnsendError::TooLate) => Event::UnsendFailed { ticket, too_late: true, error: String::new() },
                Err(UnsendError::Jmap(e)) => Event::UnsendFailed { ticket, too_late: false, error: e.to_string() },
            });
            notify_scheduled(accounts, send_as, notify).await;
        }
        Command::Scheduled => notify_scheduled(accounts, send_as, notify).await,
        Command::LoadMore { mailbox } => {
            // Ответ — всегда: интерфейс ждёт его, прежде чем просить следующую страницу.
            let added = match accounts.locate(&mailbox) {
                Ok((acc, _)) => load_more(acc, cache, index, &mailbox, notify).await,
                Err(e) => Err(e),
            };
            notify(Event::LoadedMore { mailbox, added: *added.as_ref().unwrap_or(&0) });
            notify(Event::EmailsChanged);
            added?;
        }
        Command::Search { query, mailbox, ticket } => {
            // В папке — в её аккаунте; во всех папках — в своём (общие ящики ищутся по своему кэшу).
            let (acc, mb) = match &mailbox {
                Some(key) => {
                    let (acc, id) = accounts.locate(key)?;
                    (acc, Some(id))
                }
                None => (&accounts.own, None),
            };
            let page = acc.client.search(&query, mb, SEARCH_LIMIT).await?;
            save_summaries(cache, index, &page.emails, acc.scope(), notify)?;
            let ids = page.ids.iter().map(|id| acc.key(id)).collect();
            notify(Event::SearchResults { ticket, ids, total: page.total });
        }
        Command::Blob { blob_id, name, mime } => {
            let (acc, id) = accounts.locate(&blob_id)?;
            match acc.client.download(&acc.id, id, &name, &mime).await {
                Ok(data) => {
                    cache.put_blob(&blob_id, &data).map_err(db_err)?;
                    notify(Event::BlobReady { blob_id, data: Arc::new(data) });
                }
                Err(e) => notify(Event::BlobFailed { blob_id, error: e.to_string() }),
            }
        }
        Command::RespondInvite { invite, status, in_reply_to } => {
            let uid = invite.event.uid.clone();
            notify(match respond_invite(accounts, cache, identities, send_as, &invite, status, in_reply_to).await {
                Ok(delivery) => Event::InviteAnswered { uid, status, delivery },
                Err(e) => Event::InviteFailed { uid, error: e.to_string() },
            });
        }
        Command::Events { from, to } => {
            notify(match schedule::week_with_colleagues(client, from, to).await {
                Ok((events, colleagues)) => {
                    let (events, colleagues) = owned_by(&accounts.me, events, colleagues);
                    Event::EventsLoaded { from, to, events, colleagues, account: accounts.profile.clone() }
                }
                Err(e) => Event::CalendarFailed(e.to_string()),
            });
        }
        Command::SearchEvents { text, from, to } => {
            notify(match schedule::search(client, &text, from, to).await {
                Ok(events) => Event::EventsFound { text, events, error: None },
                Err(e) => Event::EventsFound { text, events: Vec::new(), error: Some(e.to_string()) },
            });
        }
        Command::Availability { ticket, emails, from, to, .. } => {
            notify(match schedule::availability(client, &emails, from, to).await {
                Ok((me, people)) => Event::Availability { ticket, me, people, error: None },
                Err(e) => Event::Availability {
                    ticket,
                    me: Vec::new(),
                    people: emails.into_iter().map(|e| (e, None)).collect(),
                    error: Some(e.to_string()),
                },
            });
        }
        Command::CreateMeeting { meeting, ticket } => {
            notify(match schedule::create(client, identities, &meeting).await {
                Ok(_) => Event::MeetingCreated { ticket, uid: meeting.uid.clone() },
                Err(e) => Event::MeetingFailed { ticket, error: e.to_string() },
            });
        }
        Command::UpdateMeeting { event, meeting, series, ticket } => {
            notify(match schedule::update(client, identities, &event, &meeting, series).await {
                Ok(uid) => Event::MeetingUpdated { ticket, uid },
                Err(e) => Event::MeetingFailed { ticket, error: e.to_string() },
            });
        }
        Command::CancelMeeting { event, series, ticket, .. } => {
            notify(match schedule::cancel(client, &event, series).await {
                Ok(uid) => Event::MeetingCancelled { ticket, uid },
                Err(e) => Event::MeetingFailed { ticket, error: e.to_string() },
            });
        }
        // Должности, отдела и телефона у JMAP `Principal` нет — подробностей сверх найденного тоже.
        Command::PersonDetails { email } => notify(Event::PersonDetails { email, person: None }),
        Command::ShareFreeBusy { email } => {
            notify(match schedule::share_free_busy(client, &email).await {
                Ok(()) => Event::FreeBusyShared { email },
                Err(e) => Event::CalendarFailed(e.to_string()),
            });
        }
        Command::Settings(request) => settings::handle(client, request, &|r| notify(Event::Settings(r))).await,
        Command::Categories(request) => {
            let notify = |r| notify(Event::Categories(r));
            categories::handle(client, &accounts.own.id, Some(&*cache), request, &notify).await;
        }
        Command::Tasks(request) => {
            tasks::handle(&JmapStore::new(client), cache, request, &|r| notify(Event::Tasks(r))).await?;
        }
        Command::Contacts => load_people(client, notify).await,
        Command::FindPeople { text, offset } => find_people(client, text, offset, notify).await,
        Command::SaveContact { card, old, ticket } => {
            let saved = match &old {
                Some(old) => client.update_contact(&old.to_jmap(), &card.to_jmap()).await.map(|()| old.id.clone()),
                None => client.create_contact(&card.to_jmap()).await,
            };
            notify(match saved {
                Ok(id) => Event::ContactSaved { ticket, id },
                Err(e) => Event::ContactFailed { ticket, error: e.to_string() },
            });
            load_cards(client, notify).await;
        }
        Command::DeleteContact { id, ticket } => {
            notify(match client.destroy_contact(&id).await {
                Ok(()) => Event::ContactDeleted { ticket, id },
                Err(e) => Event::ContactFailed { ticket, error: e.to_string() },
            });
            load_cards(client, notify).await;
        }
        Command::Patch { patches, archive_name } => patch(accounts, cache, index, patches, &archive_name, notify).await?,
        Command::Newsletters { cutoff, archive_name } => {
            let archived = newsletters(&accounts.own, cache, cutoff, &archive_name, notify).await;
            notify(Event::Newsletters { archived: *archived.as_ref().unwrap_or(&0) });
            if archived.as_ref().is_ok_and(|n| *n > 0) {
                notify(Event::EmailsChanged);
            }
            archived?;
        }
        Command::SaveDraft { draft, replace, ticket } => {
            let r = save_draft(accounts, cache, send_as, &draft, replace.as_deref()).await;
            notify(match r {
                Ok(id) => Event::DraftSaved { ticket, id },
                Err(e) => Event::ComposeFailed { ticket, error: e.to_string() },
            });
        }
        Command::ReportPhishing { ids, to, subject, text, ticket } => {
            let r = report_phishing(accounts, cache, send_as, &ids, to, subject, text).await;
            match &r {
                Ok(()) => notify(Event::Reported { ticket, error: None }),
                // Без связи — ждёт в очереди (`link`), события нет.
                Err(e) if kind_of(e) == ErrorKind::Network => {}
                Err(e) => notify(Event::Reported { ticket, error: Some(e.to_string()) }),
            }
            r?;
        }
        Command::Delegate { uid, drafts, ticket, release } => {
            let mut r: Result<(), Error> =
                tasks::handle(&JmapStore::new(client), cache, tasks::Request::Flush, &|r| notify(Event::Tasks(r))).await.map_err(Error::from);
            let mut scheduled = Vec::new();
            if r.is_ok() {
                for d in &drafts {
                    match send(accounts, cache, send_as, d, None, release).await {
                        Ok(s) => scheduled.extend(s),
                        Err(e) => {
                            r = Err(e.into());
                            break;
                        }
                    }
                }
            }
            match &r {
                Ok(()) => notify(Event::Delegated { ticket, uid, error: None, scheduled }),
                Err(e) if kind_of(e) == ErrorKind::Network => {}
                Err(e) => notify(Event::Delegated { ticket, uid, error: Some(e.to_string()), scheduled }),
            }
            r?;
        }
        Command::Withhold { scheduled, ticket } => {
            let (mut too_late, mut error) = (false, None);
            for s in &scheduled {
                match withhold(accounts, s).await {
                    Ok(()) => {}
                    Err(UnsendError::TooLate) => too_late = true,
                    Err(UnsendError::Jmap(e)) => error = Some(e.to_string()),
                }
            }
            notify(Event::Withheld { ticket, too_late, error });
            notify_scheduled(accounts, send_as, notify).await;
        }
        // «Повторить сейчас» — забота цикла синхронизации.
        Command::Reconnect => {}
        // Папки (T1-04в): `Mailbox/set`, затем папки заново; отказ сервера — `FolderFailed`.
        cmd @ (Command::CreateFolder { .. } | Command::RenameFolder { .. } | Command::MoveFolder { .. }) => {
            folder_command(accounts, cache, cmd, notify).await;
        }
        // Страница справочника (T1-25в): срез того же списка, что в `DirectoryLoaded`.
        Command::DirectoryPage { account, offset, max } => {
            if !client.has(postman_jmap::PRINCIPALS) {
                notify(Event::no_directory(account, offset));
                return Ok(());
            }
            let people = postman_core::contacts::directory(client.directory(DIRECTORY_LIMIT).await?.into_iter().map(Principal::from_jmap).collect());
            notify(Event::directory_slice(account, &people, offset, max));
        }
        // Шаги задания переноса (S8-13): ответ — по письму, задание ведёт ядро.
        Command::FetchRaw { letter } => notify(fetch_raw(accounts, cache, letter).await),
        Command::Import { job, letter, folder, keywords, received_at, .. } => {
            notify(match import(accounts, cache, letter, folder, &keywords, received_at).await {
                Ok(remote) => Event::Imported { job, letter, remote },
                Err(error) => {
                    tracing::info!(job = job.0, letter = letter.0, "jmap import refused: {error}");
                    Event::ImportFailed { job, letter, error }
                }
            });
        }
        Command::ServerCopy { job, letters, to } => server_copy(accounts, cache, job, &letters, to).await.into_iter().for_each(notify),
        Command::DestroyLetters { job, account, letters } => {
            destroy_letters(accounts, cache, job, &account, &letters).await.into_iter().for_each(notify);
        }
        // Перенос в базе и задания — в потоке ядра (`Command::routes_by`), сюда не приходят.
        cmd @ (Command::MoveLetters { .. } | Command::Migration(_)) => cmd.unsupported().into_iter().for_each(notify),
    }
    Ok(())
}

impl Accounts {
    /// Аккаунт по учётке связи с сервером (аккаунт ключей кэша; свой основной — пустая строка).
    fn by_account(&self, account: &str) -> Option<&MailAccount> {
        self.by_scope(Some(account).filter(|a| !a.is_empty()))
    }
}

/// Связь письма с сервером в одном из аккаунтов этой учётной записи — и `detached` (письмо задания переноса у
/// источника): аккаунт и id письма на сервере.
fn remote_in<'a>(accounts: &'a Accounts, cache: &Cache, letter: LetterId) -> postman_jmap::Result<Option<(&'a MailAccount, String)>> {
    let refs = cache.remotes(postman_cache::LetterId(letter.0)).map_err(db_err)?;
    Ok(refs.into_iter().find_map(|r| Some((accounts.by_account(&r.account)?, r.id))))
}

/// Папка хранилища → аккаунт этой учётной записи, его учётка связей и id папки на сервере.
fn folder_in<'a>(accounts: &'a Accounts, cache: &Cache, folder: FolderId) -> Option<(&'a MailAccount, String, String)> {
    let row = cache.folder(postman_cache::FolderId(folder.0)).ok()??;
    let postman_cache::Owner::Account(account) = row.owner else { return None };
    Some((accounts.by_account(&account)?, account, row.remote?))
}

/// Ошибка JMAP при выгрузке → отказ задания переноса: больше предела, нет места; сервер просит подождать или связи нет
/// — пауза повтора задания; прочее — отказ с ответом сервера.
fn import_error(e: postman_jmap::Error) -> ImportError {
    let pause = || ImportError::Throttled { retry_after: postman_core::migrate::RETRY.as_secs() };
    match &e {
        postman_jmap::Error::Method { kind, .. } if kind == "overQuota" => ImportError::Quota,
        postman_jmap::Error::Method { kind, .. } if kind == "tooLarge" => ImportError::TooLarge,
        postman_jmap::Error::Method { kind, .. } if kind == "rateLimit" => pause(),
        postman_jmap::Error::Http(h) => match h.status().map(|s| s.as_u16()) {
            Some(413) => ImportError::TooLarge,
            Some(429 | 503) => pause(),
            _ if kind_of(&e) == ErrorKind::Network => pause(),
            _ => ImportError::Rejected(e.to_string()),
        },
        _ => ImportError::Rejected(e.to_string()),
    }
}

/// `Command::FetchRaw`: письмо целиком — блоб `Email.blobId` его аккаунта.
async fn fetch_raw(accounts: &Accounts, cache: &Cache, letter: LetterId) -> Event {
    let data = async {
        let (acc, id) =
            remote_in(accounts, cache, letter)?.ok_or_else(|| postman_jmap::Error::Protocol("the letter has no link with this account".into()))?;
        let blob =
            acc.client.email_blobs(&[id]).await?.pop().ok_or_else(|| postman_jmap::Error::Protocol("the letter is gone from the server".into()))?;
        acc.client.download(&acc.id, &blob.blob_id, "message.eml", "message/rfc822").await
    }
    .await;
    Event::Raw { letter, data: data.map(Arc::new).map_err(|e| e.to_string()) }
}

/// `Command::Import`: письмо из хранилища — блобом в аккаунт папки и `Email/import` в неё (ключевые слова — все, дата
/// получения — как у письма). Повтор без дубля — `alreadyExists` сервера. Ответ — связь нового письма с сервером.
async fn import(
    accounts: &Accounts,
    cache: &Cache,
    letter: LetterId,
    folder: FolderId,
    keywords: &[String],
    received_at: i64,
) -> Result<Remote, ImportError> {
    let (acc, account, mailbox) = folder_in(accounts, cache, folder).ok_or_else(|| ImportError::Rejected("no such folder".into()))?;
    let raw = match cache.letter_content(postman_cache::LetterId(letter.0)) {
        Ok(Some(raw)) => raw,
        Ok(None) => return Err(ImportError::Rejected("letter content is not stored".into())),
        Err(e) => return Err(ImportError::Rejected(e.to_string())),
    };
    if raw.len() as u64 > acc.client.session().limits().max_size_upload {
        return Err(ImportError::TooLarge);
    }
    let blob = acc.client.upload(&acc.id, raw, "message/rfc822").await.map_err(import_error)?;
    let imported = acc
        .client
        .import_email(&acc.id, &blob, &[mailbox], &server_words(keywords), Some(received_at).filter(|t| *t > 0))
        .await
        .map_err(import_error)?;
    Ok(Remote { account, id: imported.id, data: Vec::new() })
}

/// `Command::ServerCopy`: письма аккаунтов этой сессии — в папку другого аккаунта сервером (`Email/copy` с удалением
/// источника), по аккаунту источника. Источник остался (сервер не удалил) — удалить отдельно: задание считает его
/// удалённым сервером.
async fn server_copy(accounts: &Accounts, cache: &Cache, job: JobId, letters: &[LetterId], to: FolderId) -> Vec<Event> {
    let failed = |letter: LetterId, error: ImportError| Event::ImportFailed { job, letter, error };
    let Some((target, account, mailbox)) = folder_in(accounts, cache, to) else {
        return letters.iter().map(|l| failed(*l, ImportError::Rejected("no such folder".into()))).collect();
    };
    let mut out = Vec::new();
    let mut by: Vec<(&MailAccount, Vec<(LetterId, String)>)> = Vec::new();
    for &letter in letters {
        match remote_in(accounts, cache, letter) {
            Ok(Some((acc, id))) => match by.iter_mut().find(|(a, _)| a.id == acc.id) {
                Some((_, list)) => list.push((letter, id)),
                None => by.push((acc, vec![(letter, id)])),
            },
            Ok(None) => out.push(failed(letter, ImportError::Rejected("the letter has no link with this account".into()))),
            Err(e) => out.push(failed(letter, ImportError::Rejected(e.to_string()))),
        }
    }
    for (from, list) in by {
        let ids: Vec<String> = list.iter().map(|(_, id)| id.clone()).collect();
        let copied = match accounts.own.client.copy_emails(&from.id, &target.id, &ids, &mailbox, true).await {
            Ok(c) => c,
            Err(e) => {
                let error = import_error(e);
                out.extend(list.iter().map(|(l, _)| failed(*l, error.clone())));
                continue;
            }
        };
        for ((letter, id), c) in list.into_iter().zip(copied) {
            match c.copy {
                Ok(copy) => {
                    if c.original_left
                        && let Err(e) = from.client.destroy_email(&id).await
                    {
                        tracing::warn!(job = job.0, letter = letter.0, "jmap copy left the original: {e}");
                    }
                    out.push(Event::Imported { job, letter, remote: Remote { account: account.clone(), id: copy.id, data: Vec::new() } });
                }
                Err(e) => out.push(failed(letter, import_error(e))),
            }
        }
    }
    out
}

/// `Command::DestroyLetters`: письма задания — насовсем (`Email/set` `destroy`) по `detached`-связям учётки
/// `account`; ответ по письму. Письма уже нет на сервере (`notFound`, связи больше нет) — удалено.
async fn destroy_letters(accounts: &Accounts, cache: &Cache, job: JobId, account: &str, letters: &[LetterId]) -> Vec<Event> {
    let Some(acc) = accounts.by_account(account) else {
        return letters.iter().map(|l| Event::DestroyFailed { job, letter: *l, error: format!("no account {account}") }).collect();
    };
    let mut out = Vec::with_capacity(letters.len());
    for &letter in letters {
        let id = match cache.remotes(postman_cache::LetterId(letter.0)) {
            Ok(refs) => refs.into_iter().find(|r| r.account == account).map(|r| r.id),
            Err(e) => {
                out.push(Event::DestroyFailed { job, letter, error: e.to_string() });
                continue;
            }
        };
        let done = match id {
            Some(id) => acc.client.destroy_email(&id).await,
            None => Ok(()),
        };
        out.push(match done {
            Ok(()) => Event::Destroyed { job, letter },
            Err(postman_jmap::Error::Method { kind, .. }) if kind == "notFound" => Event::Destroyed { job, letter },
            Err(e) => Event::DestroyFailed { job, letter, error: e.to_string() },
        });
    }
    out
}

/// Создать, переименовать или перенести папку (T1-04в): `Mailbox/set` в аккаунте папки, затем папки заново и
/// `MailboxesChanged`; отказ — `FolderFailed` с ответом сервера.
async fn folder_command(accounts: &Accounts, cache: &Cache, cmd: Command, notify: &impl Fn(Event)) {
    let server = |key: &str| unscoped(key).1.to_owned();
    let (mailbox, scope, result) = match cmd {
        Command::CreateFolder { account, parent, name } => {
            let scope = account.clone();
            let result = match accounts.by_scope(account.as_deref()) {
                Some(acc) => acc.client.create_mailbox_in(parent.as_deref().map(server).as_deref(), name.trim(), None).await.map(|_| ()),
                None => Err(postman_jmap::Error::Protocol("no such account".into())),
            };
            (parent, scope, result)
        }
        Command::RenameFolder { mailbox, name } => {
            let scope = postman_cache::account_of(&mailbox).map(str::to_owned);
            let result = match accounts.locate(&mailbox) {
                Ok((acc, id)) => acc.client.update_mailbox(id, Some(name.trim()), None).await,
                Err(e) => Err(e),
            };
            (Some(mailbox), scope, result)
        }
        Command::MoveFolder { mailbox, parent } => {
            let scope = postman_cache::account_of(&mailbox).map(str::to_owned);
            let to = parent.as_deref().map(server);
            let result = match accounts.locate(&mailbox) {
                Ok((acc, id)) => acc.client.update_mailbox(id, None, Some(to.as_deref())).await,
                Err(e) => Err(e),
            };
            (Some(mailbox), scope, result)
        }
        _ => return,
    };
    if let Err(e) = result {
        tracing::info!("jmap folder command refused: {e}");
        return notify(Event::FolderFailed { mailbox, error: e.to_string() });
    }
    if let Some(acc) = accounts.by_scope(scope.as_deref())
        && let Err(e) = load_mailboxes(acc, cache).await
    {
        tracing::warn!("jmap mailboxes after folder command: {e}");
    }
    notify(Event::MailboxesChanged);
}

/// Имя вложения `.eml` из темы письма: без знаков, которые Windows не пускает в имя файла; пустая — «message».
fn eml_name(subject: Option<&str>) -> String {
    let clean: String = subject.unwrap_or("").chars().map(|c| if c.is_control() || r#"\/:*?"<>|"#.contains(c) { '_' } else { c }).take(80).collect();
    let clean = clean.trim().trim_end_matches('.');
    format!("{}.eml", if clean.is_empty() { "message" } else { clean })
}

/// `Command::ReportPhishing`: письма — вложениями `message/rfc822` (байты скачивает отправка по `blobId`) одним
/// письмом на `to` от своего адреса, сразу (без окна «Отменить»).
async fn report_phishing(
    accounts: &Accounts,
    cache: &Cache,
    send_as: &[SendAs],
    keys: &[String],
    to: Address,
    subject: String,
    text: String,
) -> postman_jmap::Result<()> {
    let me = send_as
        .iter()
        .find(|s| s.scope.is_none())
        .or(send_as.first())
        .ok_or_else(|| postman_jmap::Error::Protocol("no Identity on the server to send from".into()))?;
    let mut draft = Draft { from: Address::new(&me.identity.name, &me.identity.email), to: vec![to], subject, markdown: text, ..Default::default() };
    for (scope, keys) in by_account(keys) {
        let Some(acc) = accounts.by_scope(scope) else { continue };
        let ids: Vec<String> = keys.iter().map(|k| unscoped(k).1.to_owned()).collect();
        for e in acc.client.email_blobs(&ids).await? {
            draft.attachments.push(postman_compose::Attachment {
                name: eml_name(e.subject.as_deref()),
                mime: "message/rfc822".into(),
                data: Vec::new(),
                inline_cid: None,
                blob_id: Some(acc.key(&e.blob_id)),
            });
        }
    }
    if draft.attachments.is_empty() {
        return Err(postman_jmap::Error::Protocol("the reported messages are gone from the server".into()));
    }
    send(accounts, cache, send_as, &draft, None, Release::Now).await?;
    Ok(())
}

/// «Архив» аккаунта (id на сервере): из кэша папок; нет — создать папку с ролью `archive` (у Stalwart по умолчанию
/// её нет, `docs/STALWART.md`) и перечитать папки.
async fn ensure_archive(acc: &MailAccount, cache: &Cache, name: &str, notify: &impl Fn(Event)) -> postman_jmap::Result<String> {
    let known = cache
        .mailboxes()
        .map_err(db_err)?
        .into_iter()
        .find(|m| m.role.as_deref() == Some("archive") && postman_cache::account_of(&m.id) == acc.scope());
    if let Some(m) = known {
        return Ok(unscoped(&m.id).1.to_owned());
    }
    let id = acc.client.create_mailbox(name, Some("archive")).await?;
    load_mailboxes(acc, cache).await?;
    notify(Event::MailboxesChanged);
    Ok(id)
}

/// Правка жизненного цикла для сервера: id и папки — без префикса аккаунта; [`lifecycle::ARCHIVE`] — `archive`.
fn server_patch(p: &lifecycle::Patch, archive: Option<&str>) -> (String, postman_jmap::EmailPatch) {
    let mut out = postman_jmap::EmailPatch::default();
    for (m, on) in &p.mailboxes {
        let id = if m == lifecycle::ARCHIVE { archive } else { Some(unscoped(m).1) };
        if let Some(id) = id {
            out = out.mailbox(id, *on);
        }
    }
    for (k, on) in p.keywords.iter().filter(|(k, _)| !k.eq_ignore_ascii_case(postman_cache::ENCRYPTED)) {
        out = out.keyword(k, *on);
    }
    (unscoped(&p.id).1.to_owned(), out)
}

/// `Command::Patch`: по аккаунтам, одним `Email/set` (по 500); не принятые — с сервера заново.
async fn patch(
    accounts: &Accounts,
    cache: &mut Cache,
    index: &Index,
    patches: Vec<lifecycle::Patch>,
    archive_name: &str,
    notify: &impl Fn(Event),
) -> postman_jmap::Result<()> {
    let mut first_error = None;
    let mut by: BTreeMap<Option<String>, Vec<lifecycle::Patch>> = BTreeMap::new();
    for p in patches {
        by.entry(postman_cache::account_of(&p.id).map(str::to_owned)).or_default().push(p);
    }
    for (scope, list) in by {
        let Some(acc) = accounts.by_scope(scope.as_deref()) else { continue };
        let wants_archive = list.iter().any(|p| p.mailboxes.iter().any(|(m, _)| m == lifecycle::ARCHIVE));
        let archive = if wants_archive {
            match ensure_archive(acc, cache, archive_name, notify).await {
                Ok(id) => Some(id),
                Err(e) => {
                    first_error.get_or_insert(e);
                    None
                }
            }
        } else {
            None
        };
        let server: Vec<(String, postman_jmap::EmailPatch)> = list.iter().map(|p| server_patch(p, archive.as_deref())).collect();
        let failed: Vec<String> = match acc.client.patch_emails(&server).await {
            Ok(f) => f
                .into_iter()
                .map(|(id, e)| {
                    first_error.get_or_insert(e);
                    acc.key(&id)
                })
                .collect(),
            Err(e) => {
                first_error.get_or_insert(e);
                list.iter().map(|p| p.id.clone()).collect()
            }
        };
        // Папку «Архив» UI ещё не знал (в кэше письмо без неё) — письма такими, какие они теперь на сервере.
        let refetch: Vec<String> = if wants_archive { list.iter().map(|p| p.id.clone()).collect() } else { failed };
        for key in &refetch {
            restore(accounts, cache, index, key, notify).await;
        }
    }
    match first_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// `Command::Newsletters`: письма «Входящих» с `List-Id` или `List-Unsubscribe` (сырые заголовки — у сервера) →
/// [`lifecycle::newsletter_sweep`] → в «Архив». Ответ — сколько перенесено.
async fn newsletters(acc: &MailAccount, cache: &mut Cache, cutoff: i64, archive_name: &str, notify: &impl Fn(Event)) -> postman_jmap::Result<usize> {
    let mailboxes = cache.mailboxes().map_err(db_err)?;
    let roles = lifecycle::Roles::of(&mailboxes, acc.scope());
    let Some(inbox_key) = roles.inbox else { return Ok(0) };
    let inbox = unscoped(&inbox_key).1.to_owned();
    let seen = || vec![Filter::InMailbox(inbox.clone()), Filter::HasKeyword(postman_cache::SEEN.into())];
    // Свежие — чтобы знать последнюю от отправителя; старые — чтобы дойти до них в большом ящике.
    let mut letters = acc.client.list_letters(&Filter::And(seen()).to_jmap(), lifecycle::NEWS_LIMIT).await?;
    let mut old = seen();
    old.push(Filter::Before(postman_jmap::unix_to_utc(cutoff)));
    for l in acc.client.list_letters(&Filter::And(old).to_jmap(), lifecycle::NEWS_LIMIT).await? {
        if !letters.iter().any(|x| x.id == l.id) {
            letters.push(l);
        }
    }
    let items: Vec<lifecycle::NewsItem> = letters
        .into_iter()
        .filter(|l| l.list)
        .map(|l| lifecycle::NewsItem { id: l.id, sender: l.from, received_at: l.received_at, seen: l.seen })
        .collect();
    let ids = lifecycle::newsletter_sweep(&items, cutoff);
    if ids.is_empty() {
        return Ok(0);
    }
    let archive = ensure_archive(acc, cache, archive_name, notify).await?;
    let patches: Vec<(String, postman_jmap::EmailPatch)> =
        ids.iter().map(|id| (id.clone(), postman_jmap::EmailPatch::default().mailbox(&inbox, false).mailbox(&archive, true))).collect();
    let failed = acc.client.patch_emails(&patches).await?;
    let moved: Vec<lifecycle::Patch> = ids
        .iter()
        .filter(|id| !failed.iter().any(|(f, _)| f == *id))
        .map(|id| lifecycle::Patch { id: acc.key(id), keywords: Vec::new(), mailboxes: vec![(inbox_key.clone(), false), (acc.key(&archive), true)] })
        .collect();
    cache.patch_emails(&moved).map_err(db_err)?;
    Ok(moved.len())
}

/// Ключи кэша по аккаунтам (`None` — свой); порядок внутри аккаунта — как в `keys`.
fn by_account(keys: &[String]) -> BTreeMap<Option<&str>, Vec<String>> {
    let mut out: BTreeMap<Option<&str>, Vec<String>> = BTreeMap::new();
    for key in keys {
        out.entry(postman_cache::account_of(key)).or_default().push(key.clone());
    }
    out
}

/// Где искать на сервере: аккаунт папки `mailbox` (ключ кэша) и её id; без папки — все папки своего аккаунта
/// (общие ящики в «Все папки» — по своему кэшу, как поиск).
fn query_scope<'a>(accounts: &'a Accounts, mailbox: Option<&'a str>) -> postman_jmap::Result<(&'a MailAccount, Option<&'a str>)> {
    match mailbox {
        Some(key) => accounts.locate(key).map(|(acc, id)| (acc, Some(id))),
        None => Ok((&accounts.own, None)),
    }
}

/// Фильтр в папке (`inMailbox` вместе с ним) или как есть.
fn in_mailbox(mailbox: Option<&str>, filter: Filter) -> Filter {
    match mailbox {
        Some(id) => Filter::And(vec![Filter::InMailbox(id.to_owned()), filter]),
        None => filter,
    }
}

/// Беседа с сервера: `Thread/get` и краткие записи писем (из всех папок, в том числе «Отправленных» за пределами
/// первой страницы), затем тела, которых нет в кэше. Ключи писем — по времени. Новые письма в беседе потом приходят
/// обычной дельтой `Email/changes` (в краткой записи есть `threadId`), `Thread/changes` не нужен.
async fn load_thread(
    acc: &MailAccount,
    cache: &mut Cache,
    index: &Index,
    thread: &str,
    notify: &impl Fn(Event),
) -> postman_jmap::Result<Vec<String>> {
    let (ids, emails) = acc.client.thread(thread).await?;
    let keys: Vec<String> = ids.iter().map(|id| acc.key(id)).collect();
    let known = cache.rows_by_ids(&keys).map_err(db_err)?.len();
    save_summaries(cache, index, &emails, acc.scope(), notify)?;
    if known < emails.len() {
        notify(Event::EmailsChanged);
    }
    let mut missing = Vec::new();
    for (id, key) in ids.iter().zip(&keys) {
        if cache.body(key).map_err(db_err)?.is_none() {
            missing.push(id.clone());
        }
    }
    if !missing.is_empty() {
        let mut stored = Vec::new();
        for (id, b) in acc.client.email_bodies(&missing, THREAD_BODY_BYTES).await? {
            let key = acc.key(&id);
            cache.put_body(&key, &body_row(b, acc.scope())).map_err(db_err)?;
            stored.push(key);
        }
        notify(Event::BodiesStored(stored));
    }
    Ok(keys)
}

/// Следующая страница папки (ключ `mailbox`) после самого старого письма в кэше; сколько писем пришло.
async fn load_more(acc: &MailAccount, cache: &mut Cache, index: &Index, mailbox: &str, notify: &impl Fn(Event)) -> postman_jmap::Result<usize> {
    let mb = unscoped(mailbox).1;
    let page = match cache.oldest(mailbox).map_err(db_err)? {
        Some(anchor) => acc.client.email_page_after(mb, unscoped(&anchor).1, PAGE).await,
        None => acc.client.email_page(mb, 0, PAGE).await,
    };
    // Якорь мог исчезнуть на сервере (удалён) — тогда по позиции.
    let page = match page {
        Err(postman_jmap::Error::Method { kind, .. }) if kind == "anchorNotFound" => {
            acc.client.email_page(mb, cache.count(mailbox).map_err(db_err)? as u64, PAGE).await?
        }
        p => p?,
    };
    let added = page.emails.len();
    save_summaries(cache, index, &page.emails, acc.scope(), notify)?;
    Ok(added)
}

/// Личные контакты и справочник; чего сервер не объявил — того нет (без ошибки).
async fn load_people(client: &Client, notify: &impl Fn(Event)) {
    load_cards(client, notify).await;
    if !client.has(postman_jmap::PRINCIPALS) {
        return notify(Event::DirectoryLoaded(Vec::new()));
    }
    match client.directory(DIRECTORY_LIMIT).await {
        Ok(people) => notify(Event::DirectoryLoaded(postman_core::contacts::directory(people.into_iter().map(Principal::from_jmap).collect()))),
        Err(e) => notify(Event::ContactFailed { ticket: 0, error: e.to_string() }),
    }
}

/// Люди справочника по строке страницей с `offset` (`Principal/query` с фильтром `text`, `position`) — тот же
/// договор, что у EWS. Справочника у сервера нет или запрос не удался — пусто (подсказки — из уже загруженного).
async fn find_people(client: &Client, text: String, offset: u32, notify: &impl Fn(Event)) {
    let (people, more) = if client.has(postman_jmap::PRINCIPALS) && text.trim().chars().count() >= PEOPLE_FROM {
        match client.principals_page(text.trim(), u64::from(offset), PEOPLE_LIMIT).await {
            Ok((found, more)) => (postman_core::contacts::directory(found.into_iter().map(Principal::from_jmap).collect()), more),
            Err(e) => {
                tracing::debug!("jmap principal query: {e}");
                (Vec::new(), false)
            }
        }
    } else {
        (Vec::new(), false)
    };
    notify(Event::PeopleFound { text, offset, people, more });
}

/// Личные контакты — разделу целиком и подсказкам адресов парами (имя, адрес).
async fn load_cards(client: &Client, notify: &impl Fn(Event)) {
    if !client.has(postman_jmap::CONTACTS) {
        // Раздел «Контакты» не ждёт того, чего сервер не умеет.
        return notify(Event::ContactsLoaded(Vec::new()));
    }
    match client.contact_cards().await {
        Ok(cards) => {
            let cards: Vec<ContactCard> = cards.into_iter().map(ContactCard::from_jmap).collect();
            let pairs = cards.iter().flat_map(|c| c.emails.iter().map(|e| (c.name.clone(), e.clone()))).collect();
            notify(Event::Contacts(pairs));
            notify(Event::ContactsLoaded(cards));
        }
        Err(e) => notify(Event::ContactFailed { ticket: 0, error: e.to_string() }),
    }
}

/// Сервер не выполнил перенос или удаление: письмо — в кэш таким, какое оно на сервере (UI уже поменял кэш сам).
async fn restore(accounts: &Accounts, cache: &mut Cache, index: &Index, key: &str, notify: &impl Fn(Event)) {
    let restored = match accounts.locate(key) {
        Ok((acc, id)) => match acc.client.email_get(&[id.to_owned()]).await {
            Ok((emails, _)) => save_summaries(cache, index, &emails, acc.scope(), notify),
            Err(e) => Err(e),
        },
        Err(e) => Err(e),
    };
    match restored {
        Ok(()) => notify(Event::EmailsChanged),
        Err(e) => {
            report(e, Stage::Restore, notify);
        }
    }
}

/// id сервера папки с ролью `role` в аккаунте `scope`.
fn mailbox_by_role(cache: &Cache, scope: Option<&str>, role: &str) -> postman_jmap::Result<String> {
    cache
        .mailboxes()
        .map_err(db_err)?
        .into_iter()
        .find(|m| m.role.as_deref() == Some(role) && postman_cache::account_of(&m.id) == scope)
        .map(|m| unscoped(&m.id).1.to_owned())
        .ok_or_else(|| postman_jmap::Error::Protocol(format!("no mailbox with role {role}")))
}

/// Код ошибки подписи для интерфейса: `smime-sign:<код>[:подробности]`.
pub fn sign_code(e: &postman_smime::sign::SignError) -> String {
    use postman_smime::sign::SignError as E;
    match e {
        E::NeedPin => "smime-sign:need-pin".into(),
        E::NoCard => "smime-sign:no-card".into(),
        E::Cancelled => "smime-sign:cancelled".into(),
        E::NeedSkzi => "smime-sign:need-skzi".into(),
        E::NoCertificate(a) => format!("smime-sign:no-cert:{a}"),
        E::EightBit => "smime-sign:eight-bit".into(),
        E::Failed(t) => format!("smime-sign:failed:{t}"),
    }
}

/// Код ошибки шифрования для интерфейса: `smime-seal:<код>[:подробности]`.
pub fn seal_code(e: &postman_smime::seal::SealError) -> String {
    use postman_smime::seal::SealError as E;
    match e {
        E::NoRecipients => "smime-seal:no-recipients".into(),
        E::Gost => "smime-seal:need-skzi".into(),
        E::Failed(t) => format!("smime-seal:failed:{t}"),
    }
}

/// Письмо для сохранения черновиком: «Конфиденциально» — зашифровано себе (решение владельца №5).
fn draft_message(draft: &Draft) -> postman_jmap::Result<Vec<u8>> {
    let raw = raw_message(draft)?;
    if draft.encrypt_to.is_empty() {
        return Ok(raw);
    }
    postman_core::smime::seal_outgoing(&raw, &draft.encrypt_to).map_err(|e| postman_jmap::Error::Protocol(seal_code(&e)))
}

fn raw_message(draft: &Draft) -> postman_jmap::Result<Vec<u8>> {
    let now = jiff::Timestamp::now().as_second();
    postman_compose::build(draft, now, &postman_compose::new_message_id(&draft.from, now))
        .map_err(|e| postman_jmap::Error::Protocol(format!("building the message: {e}")))
}

/// Догрузить вложения оригинала (пересылка), у которых есть только blob_id (ключ кэша: оригинал мог быть в
/// общем ящике, а уходит письмо из своего).
async fn fill_attachments(accounts: &Accounts, cache: &Cache, draft: &mut Draft) -> postman_jmap::Result<()> {
    for a in draft.attachments.iter_mut().filter(|a| a.data.is_empty()) {
        let Some(blob) = a.blob_id.clone() else { continue };
        a.data = match cache.blob(&blob).map_err(db_err)? {
            Some(d) => d,
            None => {
                let (acc, id) = accounts.locate(&blob)?;
                acc.client.download(&acc.id, id, &a.name, &a.mime).await?
            }
        };
    }
    Ok(())
}

/// Отправитель черновика: Identity с адресом «От» (свой аккаунт или общий ящик), иначе первая своя.
fn sender_of<'a>(send_as: &'a [SendAs], draft: &Draft) -> postman_jmap::Result<&'a SendAs> {
    send_as
        .iter()
        .find(|f| f.identity.email.eq_ignore_ascii_case(&draft.from.email))
        .or_else(|| send_as.iter().find(|f| f.scope.is_none()))
        .or(send_as.first())
        .ok_or_else(|| postman_jmap::Error::Protocol("no Identity on the server to send from".into()))
}

/// Отправить из аккаунта отправителя: черновик — в его «Черновики», копия — в его «Отправленные». Сервер держит
/// письмо до срока (`release`) — отправка, которую можно отменить.
async fn send(
    accounts: &Accounts,
    cache: &Cache,
    send_as: &[SendAs],
    draft: &Draft,
    replace: Option<&str>,
    release: Release,
) -> postman_jmap::Result<Option<Scheduled>> {
    let mut draft = draft.clone();
    fill_attachments(accounts, cache, &mut draft).await?;
    let draft = &draft;
    let sender = sender_of(send_as, draft)?;
    let acc = accounts.by_scope(sender.scope.as_deref()).ok_or_else(|| postman_jmap::Error::Protocol("the sender's account is gone".into()))?;
    let envelope = Envelope { mail_from: sender.identity.email.clone(), rcpt_to: draft.recipients().map(|a| a.email.clone()).collect() };
    let (drafts, sent) = (mailbox_by_role(cache, acc.scope(), "drafts")?, mailbox_by_role(cache, acc.scope(), "sent")?);
    // Без FUTURERELEASE окна «Отменить» нет — письмо уходит сразу; «Отправить позже» — ошибка, а не отправка.
    let release = match release {
        Release::After(_) if send_later::max_delay(acc.client.max_delayed_send()) == 0 => Release::Now,
        Release::At(_) if send_later::max_delay(acc.client.max_delayed_send()) == 0 => {
            return Err(postman_jmap::Error::Protocol("the server does not support delayed sending".into()));
        }
        r => r,
    };
    let mut raw = raw_message(draft)?;
    // Подпись S/MIME (S5-12 Ф3) — молча: нужен PIN, нет карты или сертификата — письмо не уходит, ошибка с кодом
    // (`smime-sign:<код>`) — строкой во вкладке письма.
    if draft.sign {
        let now = jiff::Timestamp::now().as_second();
        raw = postman_core::smime::sign_outgoing(&raw, &draft.from.email, None, now).map_err(|e| postman_jmap::Error::Protocol(sign_code(&e)))?;
    }
    // Шифрование S/MIME (S5-12 Ф4): после подписи (RFC 8551 §3.7); ошибка — код `smime-seal:<код>`.
    if !draft.encrypt_to.is_empty() {
        raw = postman_core::smime::seal_outgoing(&raw, &draft.encrypt_to).map_err(|e| postman_jmap::Error::Protocol(seal_code(&e)))?;
    }
    let sub = acc.client.submit(raw, &sender.identity.id, &envelope, &drafts, &sent, release.to_jmap(), &server_words(&draft.keywords)).await?;
    if let Some(old) = replace {
        let (acc, id) = accounts.locate(old)?;
        acc.client.destroy_email(id).await?;
    }
    Ok((sub.pending && release != Release::Now).then(|| Scheduled {
        submission: acc.key(&sub.submission_id),
        email: acc.key(&sub.email_id),
        send_at: sub.send_at.unwrap_or_else(|| jiff::Timestamp::now().as_second()),
    }))
}

/// Не отправлять письмо шага поручения, которое держит сервер, и удалить его (в «Черновики» оно не нужно).
async fn withhold(accounts: &Accounts, s: &Scheduled) -> Result<(), UnsendError> {
    let (acc, submission) = accounts.locate(&s.submission)?;
    acc.client.cancel_submission(submission).await?;
    let (acc, email) = accounts.locate(&s.email)?;
    acc.client.destroy_email(email).await?;
    Ok(())
}

/// Отменить отправку, которую держит сервер: письмо — обратно в «Черновики» аккаунта отправителя.
async fn unsend(accounts: &Accounts, cache: &Cache, s: &Scheduled) -> Result<(), UnsendError> {
    let (acc, submission) = accounts.locate(&s.submission)?;
    let email = unscoped(&s.email).1;
    let (drafts, sent) = (mailbox_by_role(cache, acc.scope(), "drafts")?, mailbox_by_role(cache, acc.scope(), "sent")?);
    acc.client.unsend(submission, email, &drafts, &sent).await
}

/// Отложенные отправки своего аккаунта и общих ящиков, от которых можно отправлять → `Event::Scheduled`.
async fn notify_scheduled(accounts: &Accounts, send_as: &[SendAs], notify: &impl Fn(Event)) {
    let mut list = Vec::new();
    let sending = std::iter::once(&accounts.own).chain(accounts.shared.iter().filter(|a| send_as.iter().any(|f| f.scope.as_deref() == a.scope())));
    for acc in sending {
        // Не прочиталось — списка этого аккаунта нет; письма всё равно уйдут в срок (их держит сервер).
        let Ok(pending) = acc.client.pending_submissions().await else { continue };
        list.extend(
            pending
                .into_iter()
                .filter_map(|p| Some(Scheduled { send_at: p.send_at_unix()?, submission: acc.key(&p.id), email: acc.key(&p.email_id) })),
        );
    }
    list.sort_by_key(|s| s.send_at);
    notify(Event::Scheduled { list, max_delay: send_later::max_delay(accounts.own.client.max_delayed_send()) });
}

async fn respond_invite(
    accounts: &Accounts,
    cache: &Cache,
    identities: &[Identity],
    send_as: &[SendAs],
    inv: &Invitation,
    status: Partstat,
    in_reply_to: Option<String>,
) -> postman_jmap::Result<Delivery> {
    let client = &accounts.own.client;
    // Отвечаем от того адреса, на который пришло приглашение (их может быть несколько).
    let identity = identities
        .iter()
        .find(|i| inv.my_status(&i.email).is_some())
        .or(identities.first())
        .ok_or_else(|| postman_jmap::Error::Protocol("no Identity on the server to send from".into()))?;
    let me = Address::new(&identity.name, &identity.email);
    if invite::via_server(client, inv, &me.email, status).await?.is_some() {
        return Ok(Delivery::Server);
    }
    let now = jiff::Timestamp::now().as_second();
    let draft = invite::reply_draft(inv, &me, status, in_reply_to, now)
        .ok_or_else(|| postman_jmap::Error::Protocol("the invitation has no organizer".into()))?;
    send(accounts, cache, send_as, &draft, None, Release::Now).await?;
    // Ответ ушёл — дальше только своя копия; её сбой не повод слать ответ повторно.
    let imported = invite::import(client, inv, &me.email, status).await.unwrap_or(false);
    Ok(Delivery::Mail { imported })
}

/// Черновик — в «Черновики» аккаунта отправителя; id — ключ кэша.
async fn save_draft(accounts: &Accounts, cache: &Cache, send_as: &[SendAs], draft: &Draft, replace: Option<&str>) -> postman_jmap::Result<String> {
    let mut draft = draft.clone();
    fill_attachments(accounts, cache, &mut draft).await?;
    let draft = &draft;
    // Нет Identity (сервер не дал) — черновик всё равно в свой аккаунт.
    let scope = sender_of(send_as, draft).ok().and_then(|f| f.scope.clone());
    let acc = accounts.by_scope(scope.as_deref()).unwrap_or(&accounts.own);
    let drafts = mailbox_by_role(cache, acc.scope(), "drafts")?;
    // Прежняя версия — в том же аккаунте; отправитель сменился — прежнюю убрать отдельно.
    let (same, other): (Option<&str>, Option<&str>) = match replace {
        Some(old) if postman_cache::account_of(old) == acc.scope() => (Some(unscoped(old).1), None),
        Some(old) => (None, Some(old)),
        None => (None, None),
    };
    let id = acc.client.save_draft(draft_message(draft)?, &drafts, same, &server_words(&draft.keywords)).await?;
    if let Some(old) = other
        && let Ok((prev, old_id)) = accounts.locate(old)
    {
        prev.client.destroy_email(old_id).await?;
    }
    Ok(acc.key(&id))
}

/// Записать письма в кэш и в поисковый индекс. У писем с уже загруженным телом краткая
/// запись затёрла бы текст — их индексирует заново извлечение (событие `BodiesStored`).
fn save(cache: &mut Cache, index: &Index, rows: Vec<EmailRow>, notify: &impl Fn(Event)) -> postman_jmap::Result<()> {
    cache.upsert_emails(&rows).map_err(db_err)?;
    if let Some(i) = index {
        let (full, brief): (Vec<EmailRow>, Vec<EmailRow>) = rows.into_iter().partition(|r| cache.body(&r.id).ok().flatten().is_some());
        postman_core::indexer::rows(i, &brief);
        if !full.is_empty() {
            notify(Event::BodiesStored(full.into_iter().map(|r| r.id).collect()));
        }
    }
    Ok(())
}

/// Краткие записи писем с сервера — в кэш: строки и получатели (S5-03, быстрый фильтр «Мне»).
fn save_summaries(
    cache: &mut Cache,
    index: &Index,
    emails: &[EmailSummary],
    scope: Option<&str>,
    notify: &impl Fn(Event),
) -> postman_jmap::Result<()> {
    let recipients: Vec<(String, Recipients)> = emails.iter().map(|e| (scoped(scope, &e.id), recipients(e))).collect();
    save(cache, index, emails.iter().map(|e| email_row(e, scope)).collect(), notify)?;
    cache.set_recipients(&recipients).map_err(db_err)
}

/// Адреса To и Cc письма в нижнем регистре.
fn recipients(e: &EmailSummary) -> Recipients {
    let list = |a: &Option<Vec<EmailAddress>>| a.iter().flatten().filter_map(|a| a.email.as_deref()).map(str::to_lowercase).collect::<Vec<_>>();
    Recipients { to: list(&e.to), cc: list(&e.cc) }
}

/// Тело письма для кэша; id блобов вложений — ключи аккаунта `scope`.
fn body_row(b: EmailBody, scope: Option<&str>) -> BodyRow {
    BodyRow {
        to: addresses(&b.to),
        cc: addresses(&b.cc),
        reply_to: addresses(&b.reply_to),
        message_id: b.message_id,
        references: b.references,
        html: b.html,
        text: b.text,
        blob_id: b.blob_id.as_deref().map(|id| scoped(scope, id)),
        attachments: b
            .attachments
            .iter()
            .filter_map(|p| {
                Some(AttachmentRow {
                    blob_id: scoped(scope, p.blob_id.as_deref()?),
                    name: p.name.clone().unwrap_or_else(|| "вложение".into()),
                    mime: p.mime.clone(),
                    size: p.size,
                    cid: p.cid.as_deref().map(|c| c.trim_matches(['<', '>']).to_string()),
                    inline: p.disposition.as_deref() == Some("inline"),
                })
            })
            .collect(),
    }
}

fn addresses(list: &[EmailAddress]) -> Vec<Addr> {
    list.iter().filter_map(|a| Some(Addr { name: a.name.clone().unwrap_or_default(), email: a.email.clone()? })).collect()
}

fn rights(r: &MailboxRights) -> Rights {
    Rights {
        read: r.may_read_items,
        add: r.may_add_items,
        remove: r.may_remove_items,
        seen: r.may_set_seen,
        keywords: r.may_set_keywords,
        submit: r.may_submit,
    }
}

/// Папка для кэша; id и родитель — ключи аккаунта `scope`.
fn mailbox_row(m: &Mailbox, scope: Option<&str>) -> MailboxRow {
    MailboxRow {
        id: scoped(scope, &m.id),
        parent_id: m.parent_id.as_deref().map(|p| scoped(scope, p)),
        name: m.name.clone(),
        role: m.role.clone(),
        sort_order: m.sort_order,
        total: m.total_emails,
        unread: m.unread_emails,
        rights: rights(&m.my_rights),
    }
}

/// Письмо для кэша; id, цепочка и папки — ключи аккаунта `scope`.
fn email_row(e: &EmailSummary, scope: Option<&str>) -> EmailRow {
    let from = e.from.as_ref().and_then(|f| f.first());
    EmailRow {
        id: scoped(scope, &e.id),
        thread_id: scoped(scope, &e.thread_id),
        mailbox_ids: e.mailbox_ids.iter().filter(|(_, v)| **v).map(|(k, _)| scoped(scope, k)).collect(),
        received_at: utc_to_unix(&e.received_at).unwrap_or(0),
        subject: e.subject.clone().unwrap_or_default(),
        from_name: from.map(|a| a.display().to_string()).unwrap_or_default(),
        from_addr: from.and_then(|a| a.email.clone()).unwrap_or_default(),
        preview: e.preview.clone(),
        seen: e.has_keyword("$seen"),
        flagged: e.has_keyword("$flagged"),
        has_attachment: e.has_attachment,
        size: e.size,
        keywords: row_keywords(e),
    }
}

/// Корень письма — шифрованное S/MIME (`application/pkcs7-mime; smime-type=enveloped-data` или
/// `authEnveloped-data`; без `smime-type` — по имени `.p7m` не судим).
fn is_encrypted(content_type: Option<&str>) -> bool {
    let Some(ct) = content_type else { return false };
    let ct = ct.to_ascii_lowercase();
    (ct.contains("application/pkcs7-mime") || ct.contains("application/x-pkcs7-mime"))
        && (ct.contains("enveloped-data") || ct.contains("authenveloped-data"))
}

/// Ключевые слова для сервера: без локальных меток строки.
fn server_words(words: &[String]) -> Vec<String> {
    words.iter().filter(|k| !k.eq_ignore_ascii_case(postman_cache::ENCRYPTED)).cloned().collect()
}

/// Ключевые слова письма для кэша: кроме `$seen` и `$flagged` (свои поля), нижний регистр, по алфавиту.
fn row_keywords(e: &EmailSummary) -> Vec<String> {
    let mut out: Vec<String> = e
        .keywords
        .iter()
        .filter(|(_, on)| **on)
        .map(|(k, _)| k.to_ascii_lowercase())
        .filter(|k| k != postman_cache::SEEN && k != postman_cache::FLAGGED && k != postman_cache::ENCRYPTED)
        .collect();
    if is_encrypted(e.content_type.as_deref()) {
        out.push(postman_cache::ENCRYPTED.to_owned());
    }
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_tells_mail_from_calendar() {
        let mail = r#"{"@type":"StateChange","changed":{"a1":{"Email":"s1","Mailbox":"s2"}}}"#;
        let only = |ids: &[&str]| MailChange::In(ids.iter().map(|s| s.to_string()).collect());
        assert_eq!(Changed::of(mail), Changed { mail: only(&["a1"]), calendar: false, files: MailChange::None });
        let cal = r#"{"@type":"StateChange","changed":{"a1":{"CalendarEvent":"s3"}}}"#;
        assert_eq!(Changed::of(cal), Changed { mail: MailChange::None, calendar: true, files: MailChange::None });
        let other = r#"{"@type":"StateChange","changed":{"a1":{"ContactCard":"s4"}}}"#;
        assert_eq!(Changed::of(other), Changed::default());
        // Не разобралось — сверить всё.
        assert_eq!(Changed::of("not json"), Changed::ALL);
        assert_eq!(Changed::of("{}"), Changed::ALL);
        let both = Changed { mail: only(&["a1"]), calendar: true, files: MailChange::None };
        assert_eq!(Changed::of(mail).or(Changed::of(cal)), both);
        // Таблица категорий — файл аккаунта: push называет аккаунт, почта не сверяется.
        let file = r#"{"@type":"StateChange","changed":{"i":{"FileNode":"sem"}}}"#;
        assert_eq!(Changed::of(file), Changed { mail: MailChange::None, calendar: false, files: only(&["i"]) });
        assert_eq!(Changed::ALL.files, MailChange::All);
    }

    /// Общие ящики (S4-06): push называет аккаунты — сверяются только они.
    #[test]
    fn push_names_the_accounts_to_refresh() {
        let both = r#"{"@type":"StateChange","changed":{"e":{"Email":"s1"},"i":{"Mailbox":"s2"},"j":{"Quota":"q"}}}"#;
        let c = Changed::of(both);
        assert!(c.mail.touches("e") && c.mail.touches("i") && !c.mail.touches("j"));
        let shared = Changed::of(r#"{"@type":"StateChange","changed":{"i":{"EmailDelivery":"s"}}}"#);
        assert!(shared.mail.touches("i") && !shared.mail.touches("e"));
        assert!(!MailChange::None.touches("e") && MailChange::All.touches("e"));
        assert_eq!(shared.clone().or(Changed::ALL).mail, MailChange::All);
        let merged = shared.or(Changed::of(r#"{"@type":"StateChange","changed":{"e":{"Email":"s"}}}"#));
        assert!(merged.mail.touches("e") && merged.mail.touches("i"));
    }

    /// Отказы `Email/import` и `Email/copy` (S8-13) — отказы задания переноса: нет места, больше предела, частота;
    /// прочее — ответ сервера как есть.
    #[test]
    fn import_errors_become_migration_errors() {
        let method = |kind: &str| postman_jmap::Error::Method { method: "Email/import".into(), kind: kind.into(), description: String::new() };
        assert_eq!(import_error(method("overQuota")), ImportError::Quota);
        assert_eq!(import_error(method("tooLarge")), ImportError::TooLarge);
        assert!(matches!(import_error(method("rateLimit")), ImportError::Throttled { retry_after } if retry_after > 0));
        assert!(matches!(import_error(method("invalidEmail")), ImportError::Rejected(e) if e.contains("invalidEmail")));
        assert!(matches!(import_error(postman_jmap::Error::Protocol("x".into())), ImportError::Rejected(_)));
    }

    fn identity(id: &str, email: &str) -> Identity {
        Identity { id: id.into(), name: String::new(), email: email.into(), text_signature: String::new() }
    }

    /// Отправители (S4-06): свои — первыми; адрес общего ящика, который сервер дал и своему аккаунту, — из общего.
    #[test]
    fn shared_identity_wins_over_own_copy() {
        let own = [identity("b", "user3@itt.test"), identity("c", "Shared-Sales@itt.test")];
        let shared = [("i".to_string(), vec![identity("b", "shared-sales@itt.test")])];
        let all = senders(&own, &shared, None);
        let got: Vec<(Option<&str>, &str)> = all.iter().map(|f| (f.scope.as_deref(), f.identity.email.as_str())).collect();
        assert_eq!(got, [(None, "user3@itt.test"), (Some("i"), "shared-sales@itt.test")]);
        // Дополнительная учётная запись профиля (S7-03б): аккаунты — с её префиксом.
        let second = senders(&own, &shared, Some("@2"));
        let got: Vec<Option<&str>> = second.iter().map(|f| f.scope.as_deref()).collect();
        assert_eq!(got, [Some("@2"), Some("@2.i")]);
        let draft = |from: &str| Draft { from: Address::new("", from), ..Default::default() };
        assert_eq!(sender_of(&all, &draft("SHARED-sales@itt.test")).unwrap().scope.as_deref(), Some("i"));
        // Адреса нет среди Identity — своя.
        assert_eq!(sender_of(&all, &draft("x@itt.test")).unwrap().identity.id, "b");
        assert!(sender_of(&[], &draft("x@itt.test")).is_err());
    }

    /// Ключи кэша общего ящика: письмо, папки, цепочка и вложения — с префиксом аккаунта.
    #[test]
    fn shared_rows_are_scoped() {
        let e: EmailSummary = serde_json::from_value(serde_json::json!({
            "id": "eaaaaab", "threadId": "b", "mailboxIds": { "a": true, "d": false },
            "receivedAt": "2026-10-20T18:00:58Z", "subject": "Hi", "from": [{ "name": "S", "email": "s@x" }]
        }))
        .unwrap();
        let row = email_row(&e, Some("i"));
        assert_eq!((row.id.as_str(), row.thread_id.as_str(), row.mailbox_ids.as_slice()), ("i/eaaaaab", "i/b", &["i/a".to_string()][..]));
        assert_eq!(email_row(&e, None).id, "eaaaaab");
        let m: Mailbox = serde_json::from_value(serde_json::json!({
            "id": "c", "name": "Sub", "parentId": "a", "role": null,
            "myRights": { "mayReadItems": true, "mayAddItems": false, "mayRemoveItems": false, "maySetSeen": true, "maySetKeywords": false }
        }))
        .unwrap();
        let mb = mailbox_row(&m, Some("j"));
        assert_eq!((mb.id.as_str(), mb.parent_id.as_deref()), ("j/c", Some("j/a")));
        assert!(mb.rights.seen && !mb.rights.remove && !mb.rights.keywords);
    }

    /// Метки (S5-02): ключевые слова письма — в строку кэша без `$seen`/`$flagged`, в нижнем регистре; команды
    /// меток делятся по аккаунтам, фильтр в папке — с `inMailbox`.
    #[test]
    fn keywords_reach_the_cache_row() {
        let e: EmailSummary = serde_json::from_value(serde_json::json!({
            "id": "m1", "threadId": "t", "mailboxIds": { "a": true },
            "keywords": { "$seen": true, "$flagged": true, "pm.cat.team": true, "$Label1": true, "pm.cat.off": false },
            "receivedAt": "2026-10-20T18:00:58Z"
        }))
        .unwrap();
        let row = email_row(&e, None);
        assert!(row.seen && row.flagged);
        assert_eq!(row.keywords, ["$label1", "pm.cat.team"]);
        // S5-12 Ф2: шифрованное S/MIME — локальная метка по `Content-Type`, на сервер не уходит.
        let enc: EmailSummary = serde_json::from_value(serde_json::json!({
            "id": "m2", "threadId": "t", "mailboxIds": { "a": true }, "receivedAt": "2026-10-20T18:00:58Z",
            "header:Content-Type": " application/pkcs7-mime; smime-type=enveloped-data;
        name=\"smime.p7m\""
        }))
        .unwrap();
        assert_eq!(email_row(&enc, None).keywords, [postman_cache::ENCRYPTED]);
        assert!(!is_encrypted(Some("application/pkcs7-mime; smime-type=signed-data")) && !is_encrypted(None));
        assert!(is_encrypted(Some("application/pkcs7-mime; smime-type=authEnveloped-data")));
        assert_eq!(server_words(&[postman_cache::ENCRYPTED.into(), "pm.conf".into()]), ["pm.conf"]);
        let keys = ["a".to_string(), "i/b".into(), "c".into(), "i/d".into()];
        let groups = by_account(&keys);
        assert_eq!(groups[&None], ["a", "c"]);
        assert_eq!(groups[&Some("i")], ["i/b", "i/d"]);
        let f = Filter::HasKeyword("pm.cat.team".into());
        assert_eq!(in_mailbox(None, f.clone()), f);
        assert_eq!(in_mailbox(Some("x"), f.clone()), Filter::And(vec![Filter::InMailbox("x".into()), f]));
    }
}
