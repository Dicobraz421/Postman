//! Поток учётки канала (ADR 0012 п. 2): секрет — из хранилища ОС, `connect`, `catch_up(cursor)`, дальше `select!` по
//! запросам интерфейса, событиям канала и таймерам ядра. События копятся и пишутся пачкой — одна транзакция на
//! ≤ [`BATCH_MS`] мс или ≤ [`BATCH_EVENTS`] событий, затем одно [`Update::Changed`]; курсор — той же транзакцией.
//! Связь пропала — [`ChannelState::Offline`], повтор с паузой `link::delay` (5…60 с), затем снова `catch_up`.
//! «Печатает» — только в памяти потока, истекает через [`TYPING_SECS`] с.
//!
//! Очередь отправки с окном «Отменить» и «позже» (`chan_outbox`) — CH-02б: здесь [`Request::Act`] с
//! [`Action::Send`] уходит сразу.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use postman_cache::{Cache, Hash};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::time::Instant;

use super::store::Download;
use super::{
    AccountId, Action, AttachmentSource, ByteSource, ChannelError, ChannelEvent, ChannelFeatures, ChannelId, ChannelPlugin, ChannelSession,
    ChannelState, ChannelStore, ConnectCtx, ConversationId, Done, FileMeta, LocalStore, MessageId, Page, Participant, ParticipantId, PluginStore,
    Secrets, TYPING_SECS, Uploaded,
};
use crate::link;

/// Пачка событий — не дольше стольких миллисекунд.
pub const BATCH_MS: u64 = 100;
/// Пачка событий — не больше стольких.
pub const BATCH_EVENTS: usize = 500;
/// Сообщений беседы при первом входе и при дозапросе после `Gap`.
pub const HISTORY_PAGE: u32 = 50;

/// Запрос интерфейса потоку учётки канала (то, что видит пользователь).
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// Догрузить ленту раньше `before` (прокрутка вверх, «Медиа беседы»): найденное — в базу, затем `Changed`.
    History { conversation: ConversationId, before: Option<MessageId>, limit: u32 },
    /// Поиск на сервере: найденное — в базу, ответ — [`Update::Found`].
    Search { ticket: u64, query: String, conversation: Option<ConversationId>, limit: u32 },
    /// Действие; чего канал не умеет по возможностям — отказ `Unsupported` без запроса к серверу.
    Act { ticket: u64, action: Action },
    /// Загрузить вложение в хранилище содержимого.
    Download { conversation: ConversationId, message: MessageId, source: AttachmentSource, mime: String },
    /// Выгрузить файл перед отправкой (байты — из хранилища у интерфейса).
    Upload { ticket: u64, meta: FileMeta, bytes: Vec<u8> },
    /// «Повторить сейчас».
    Reconnect,
    /// «Выйти»: сессия на сервере, строки учётки, её `local` и секрет.
    SignOut,
}

/// Ответ потока интерфейсу.
#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    /// Беседы изменились (перечитать из базы); `gone` — их больше нет.
    Changed {
        conversations: Vec<ConversationId>,
        gone: Vec<ConversationId>,
    },
    /// Новые чужие сообщения сейчас — для уведомлений.
    Arrived {
        conversation: ConversationId,
        messages: Vec<MessageId>,
    },
    Typing {
        conversation: ConversationId,
        who: ParticipantId,
        on: bool,
    },
    State(ChannelState),
    /// Подключились: кто я и что умеет учётка.
    Connected {
        me: Participant,
        features: ChannelFeatures,
    },
    Features(ChannelFeatures),
    Sent {
        client_id: u64,
        id: MessageId,
    },
    SendFailed {
        client_id: u64,
        error: ChannelError,
    },
    Acted {
        ticket: u64,
        result: Result<Done, ChannelError>,
    },
    /// Найдено на сервере (уже в базе).
    Found {
        ticket: u64,
        result: Result<Vec<(ConversationId, MessageId)>, ChannelError>,
    },
    /// Вложение загружено: часть по хэшу; сообщения уже нет — `Ok(None)`.
    Downloaded {
        conversation: ConversationId,
        message: MessageId,
        source: AttachmentSource,
        result: Result<Option<Hash>, ChannelError>,
    },
    Uploaded {
        ticket: u64,
        result: Result<Uploaded, ChannelError>,
    },
    /// Вышли: данных учётки в базе нет.
    SignedOut,
}

/// Всё для потока учётки канала.
pub struct RunCtx {
    pub cache: Cache,
    pub secrets: Arc<dyn Secrets>,
    pub account: AccountId,
    /// Имя учётки для новой строки в базе.
    pub name: String,
    pub requests: UnboundedReceiver<Request>,
    pub notify: Box<dyn Fn(Update)>,
}

/// Поток учётки канала: свой поток ОС, рантайм tokio на один поток и `LocalSet` (плагин может `spawn_local`).
/// Ответ — канал запросов; закрыли его — поток кончается.
pub fn spawn(
    plugin: Arc<dyn ChannelPlugin>,
    cache: Cache,
    secrets: Arc<dyn Secrets>,
    account: AccountId,
    name: String,
    notify: Box<dyn Fn(Update) + Send>,
) -> UnboundedSender<Request> {
    let (tx, requests) = unbounded_channel();
    let thread = format!("channel {} {account}", plugin.id());
    let started = std::thread::Builder::new().name(thread).spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(e) => return notify(Update::State(ChannelState::Problem(ChannelError::Storage(format!("tokio runtime: {e}"))))),
        };
        let ctx = RunCtx { cache, secrets, account, name, requests, notify };
        tokio::task::LocalSet::new().block_on(&rt, run(&*plugin, ctx));
    });
    if let Err(e) = started {
        tracing::error!("channel thread: {e}");
    }
    tx
}

/// Чем кончилась подключённая сессия.
enum Ended {
    /// Интерфейс закрыл канал запросов.
    Closed,
    /// Вышли или сервер отклонил сессию: повторять нечего.
    Stopped,
    /// Связь пропала: повтор с паузой.
    Lost(ChannelError),
    /// «Повторить сейчас».
    Reconnect,
}

/// Завершённая работа сессии (задачи `spawn_local`).
enum Finished {
    CatchUp(Result<(), ChannelError>),
    History(Result<Page, ChannelError>),
    Search {
        ticket: u64,
        result: Result<Page, ChannelError>,
    },
    Act {
        ticket: u64,
        client_id: Option<u64>,
        result: Result<Done, ChannelError>,
    },
    Download {
        conversation: ConversationId,
        message: MessageId,
        source: AttachmentSource,
        mime: String,
        sink: Download,
        result: Result<(), ChannelError>,
    },
    Upload {
        ticket: u64,
        result: Result<Uploaded, ChannelError>,
    },
}

/// Пауза перед повтором после ошибки; `None` — не повторять (вход истёк, нет прав, не умеет).
fn retry_after(e: &ChannelError, attempt: &mut u32) -> Option<Duration> {
    match e {
        ChannelError::Network(_) | ChannelError::Protocol(_) => {
            let wait = link::delay(*attempt);
            *attempt = attempt.saturating_add(1);
            Some(wait)
        }
        ChannelError::Wait { secs } => Some(Duration::from_secs(u64::from(*secs))),
        _ => None,
    }
}

/// Цикл учётки канала. Зовётся внутри `tokio::task::LocalSet` (как в [`spawn`]): работа сессии идёт задачами
/// `spawn_local`, чтобы большая загрузка не держала отправку.
pub async fn run(plugin: &dyn ChannelPlugin, mut ctx: RunCtx) {
    let channel = ChannelId::new(plugin.id());
    let store = match ChannelStore::open(ctx.cache.clone(), &channel, &ctx.account, &ctx.name) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(channel = %channel, "channel store: {e}");
            return (ctx.notify)(Update::State(ChannelState::Problem(e)));
        }
    };
    let mut attempt = 0;
    loop {
        let Some(secret) = ctx.secrets.get(&channel, &ctx.account) else {
            return stopped(&store, &ctx, ChannelError::SignedOut("no session".into()));
        };
        set_state(&store, &ctx, ChannelState::Connecting);
        let (incoming_tx, mut incoming) = unbounded_channel();
        let local: Rc<dyn PluginStore> = Rc::new(LocalStore::new(ctx.cache.clone(), &channel, &ctx.account));
        let cx = ConnectCtx { account: ctx.account.clone(), secret, incoming: incoming_tx, store: local };
        let error = match plugin.connect(cx).await {
            Ok(session) => {
                attempt = 0;
                match connected(&store, Rc::from(session), &mut incoming, &mut ctx).await {
                    Ended::Closed | Ended::Stopped => return,
                    Ended::Reconnect => continue,
                    Ended::Lost(e) => e,
                }
            }
            Err(e) => e,
        };
        tracing::warn!(channel = %channel, "channel connection: {error}");
        let Some(wait) = retry_after(&error, &mut attempt) else { return stopped(&store, &ctx, error) };
        set_state(&store, &ctx, ChannelState::Offline { retry_in: wait.as_secs() });
        if !wait_offline(&store, &mut ctx, wait).await {
            return;
        }
    }
}

/// Состояние — в базу и интерфейсу.
fn set_state(store: &ChannelStore, ctx: &RunCtx, state: ChannelState) {
    if let Err(e) = store.apply(vec![ChannelEvent::State(state.clone())]) {
        tracing::warn!("channel state: {e}");
    }
    (ctx.notify)(Update::State(state));
}

fn stopped(store: &ChannelStore, ctx: &RunCtx, e: ChannelError) {
    set_state(store, ctx, ChannelState::Problem(e));
}

/// Без связи: пауза или «Повторить сейчас». Запросы, которым нужен сервер, получают отказ «нет связи». `false` —
/// конец потока (закрыли запросы или вышли).
async fn wait_offline(store: &ChannelStore, ctx: &mut RunCtx, wait: Duration) -> bool {
    let until = Instant::now() + wait;
    loop {
        tokio::select! {
            () = tokio::time::sleep_until(until) => return true,
            req = ctx.requests.recv() => match req {
                None => return false,
                Some(Request::Reconnect) => return true,
                Some(Request::SignOut) => {
                    sign_out(store, ctx);
                    return false;
                }
                Some(r) => refuse(r, ChannelError::Network("offline".into()), &*ctx.notify),
            },
        }
    }
}

/// Отказ на запрос без выполнения.
fn refuse(r: Request, error: ChannelError, notify: &dyn Fn(Update)) {
    match r {
        Request::Act { action: Action::Send(o), .. } => notify(Update::SendFailed { client_id: o.client_id, error }),
        Request::Act { ticket, .. } => notify(Update::Acted { ticket, result: Err(error) }),
        Request::Search { ticket, .. } => notify(Update::Found { ticket, result: Err(error) }),
        Request::Upload { ticket, .. } => notify(Update::Uploaded { ticket, result: Err(error) }),
        Request::Download { conversation, message, source, .. } => notify(Update::Downloaded { conversation, message, source, result: Err(error) }),
        Request::History { .. } | Request::Reconnect | Request::SignOut => {}
    }
}

/// «Выйти» без сессии на сервере: строки учётки, `local` и секрет.
fn sign_out(store: &ChannelStore, ctx: &RunCtx) {
    if let Err(e) = store.forget() {
        tracing::warn!("channel sign-out: {e}");
    }
    if let Err(e) = ctx.secrets.remove(store.channel(), store.account()) {
        tracing::warn!("channel sign-out: secret: {e}");
    }
    (ctx.notify)(Update::SignedOut);
}

/// Байты выгрузки из памяти.
struct Bytes(Vec<u8>, usize);

impl ByteSource for Bytes {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, ChannelError> {
        let rest = self.0.get(self.1..).unwrap_or_default();
        let n = rest.len().min(buf.len());
        buf[..n].copy_from_slice(&rest[..n]);
        self.1 += n;
        Ok(n)
    }

    fn len(&self) -> u64 {
        self.0.len() as u64
    }
}

/// Пачка событий, что ждёт записи.
struct Pending {
    events: Vec<ChannelEvent>,
    deadline: Option<Instant>,
}

/// Подключённая сессия: до закрытия запросов, выхода или потери связи.
async fn connected(store: &ChannelStore, session: Rc<dyn ChannelSession>, incoming: &mut UnboundedReceiver<ChannelEvent>, ctx: &mut RunCtx) -> Ended {
    let features = session.features();
    if let Err(e) = store.apply(vec![ChannelEvent::Features(features.clone()), ChannelEvent::State(ChannelState::Online)]) {
        tracing::warn!("channel features: {e}");
    }
    (ctx.notify)(Update::Connected { me: session.me(), features });
    (ctx.notify)(Update::State(ChannelState::Online));
    let (done_tx, mut done) = unbounded_channel::<Finished>();
    let mut pending = Pending { events: Vec::new(), deadline: None };
    let mut typing: Vec<(ConversationId, ParticipantId, Instant)> = Vec::new();
    // Догнать заново после `Gap` без беседы — один раз за подключение: иначе канал, который каждый раз отвечает
    // «слишком длинно», крутил бы догон по кругу.
    let mut regap = true;
    catch_up(&session, store, &done_tx);
    loop {
        let expire = typing.iter().map(|t| t.2).min();
        tokio::select! {
            req = ctx.requests.recv() => match req {
                None => {
                    flush(store, ctx, &session, &done_tx, &mut pending, &mut regap);
                    return Ended::Closed;
                }
                Some(Request::Reconnect) => {
                    flush(store, ctx, &session, &done_tx, &mut pending, &mut regap);
                    return Ended::Reconnect;
                }
                Some(Request::SignOut) => {
                    if let Err(e) = session.sign_out().await {
                        tracing::warn!("channel sign-out on server: {e}");
                    }
                    sign_out(store, ctx);
                    return Ended::Stopped;
                }
                Some(r) => request(r, store, &session, &done_tx, &*ctx.notify),
            },
            ev = incoming.recv() => {
                let Some(ev) = ev else {
                    flush(store, ctx, &session, &done_tx, &mut pending, &mut regap);
                    return Ended::Lost(ChannelError::Network("session closed".into()));
                };
                match ev {
                    ChannelEvent::State(ChannelState::Offline { .. }) | ChannelEvent::State(ChannelState::Problem(ChannelError::Network(_))) => {
                        flush(store, ctx, &session, &done_tx, &mut pending, &mut regap);
                        return Ended::Lost(ChannelError::Network("link lost".into()));
                    }
                    ChannelEvent::State(ChannelState::Problem(e @ ChannelError::SignedOut(_))) => {
                        flush(store, ctx, &session, &done_tx, &mut pending, &mut regap);
                        stopped(store, ctx, e);
                        return Ended::Stopped;
                    }
                    ChannelEvent::Typing { conversation, who, on } => {
                        typing.retain(|(c, p, _)| !(*c == conversation && *p == who));
                        if on {
                            typing.push((conversation.clone(), who.clone(), Instant::now() + Duration::from_secs(TYPING_SECS)));
                        }
                        (ctx.notify)(Update::Typing { conversation, who, on });
                    }
                    ev => {
                        let now = matches!(ev, ChannelEvent::Cursor(_) | ChannelEvent::Secret(_));
                        pending.events.push(ev);
                        pending.deadline.get_or_insert_with(|| Instant::now() + Duration::from_millis(BATCH_MS));
                        if now || pending.events.len() >= BATCH_EVENTS {
                            flush(store, ctx, &session, &done_tx, &mut pending, &mut regap);
                        }
                    }
                }
            }
            fin = done.recv() => {
                let Some(fin) = fin else { continue };
                if let Some(e) = finished(fin, store, ctx, &mut pending) {
                    flush(store, ctx, &session, &done_tx, &mut pending, &mut regap);
                    return Ended::Lost(e);
                }
                flush(store, ctx, &session, &done_tx, &mut pending, &mut regap);
            }
            () = crate::sync::retry_at(pending.deadline) => flush(store, ctx, &session, &done_tx, &mut pending, &mut regap),
            () = crate::sync::retry_at(expire) => {
                let now = Instant::now();
                for (conversation, who, _) in typing.iter().filter(|t| t.2 <= now) {
                    (ctx.notify)(Update::Typing { conversation: conversation.clone(), who: who.clone(), on: false });
                }
                typing.retain(|t| t.2 > now);
            }
        }
    }
}

/// Догнать сервер от сохранённого курсора — задачей.
fn catch_up(session: &Rc<dyn ChannelSession>, store: &ChannelStore, done: &UnboundedSender<Finished>) {
    let cursor = store.cursor().unwrap_or_else(|e| {
        tracing::warn!("channel cursor: {e}");
        None
    });
    let (s, done) = (Rc::clone(session), done.clone());
    tokio::task::spawn_local(async move {
        let r = s.catch_up(cursor).await;
        let _ = done.send(Finished::CatchUp(r));
    });
}

/// История беседы — задачей; ответ — события в пачку.
fn history(session: &Rc<dyn ChannelSession>, done: &UnboundedSender<Finished>, conv: ConversationId, before: Option<MessageId>, limit: u32) {
    let (s, done) = (Rc::clone(session), done.clone());
    tokio::task::spawn_local(async move {
        let r = s.history(&conv, before.as_ref(), limit).await;
        let _ = done.send(Finished::History(r));
    });
}

/// Запрос интерфейса — задачей сессии.
fn request(r: Request, store: &ChannelStore, session: &Rc<dyn ChannelSession>, done: &UnboundedSender<Finished>, notify: &dyn Fn(Update)) {
    let (s, tx) = (Rc::clone(session), done.clone());
    match r {
        Request::History { conversation, before, limit } => history(session, done, conversation, before, limit),
        Request::Search { ticket, query, conversation, limit } => {
            tokio::task::spawn_local(async move {
                let result = s.search(&query, conversation.as_ref(), limit).await;
                let _ = tx.send(Finished::Search { ticket, result });
            });
        }
        Request::Act { ticket, action } => {
            let features = store.features().unwrap_or_else(|_| session.features());
            if !features.allows(&action) {
                return refuse(Request::Act { ticket, action }, ChannelError::Unsupported, notify);
            }
            if let (Action::Send(o), Some(max)) = (&action, features.max_text)
                && super::doc_text(&o.body).chars().count() > max as usize
            {
                return refuse(Request::Act { ticket, action }, ChannelError::TooLarge { max: u64::from(max) }, notify);
            }
            let client_id = match &action {
                Action::Send(o) => Some(o.client_id),
                _ => None,
            };
            tokio::task::spawn_local(async move {
                let result = s.act(action).await;
                let _ = tx.send(Finished::Act { ticket, client_id, result });
            });
        }
        Request::Download { conversation, message, source, mime } => {
            let mut sink = store.begin_download();
            tokio::task::spawn_local(async move {
                let result = s.download(&source, &mut sink).await;
                let _ = tx.send(Finished::Download { conversation, message, source, mime, sink, result });
            });
        }
        Request::Upload { ticket, meta, bytes } => {
            let max = store.features().ok().and_then(|f| f.max_attachment);
            if let Some(max) = max.filter(|&m| meta.size > m || bytes.len() as u64 > m) {
                return notify(Update::Uploaded { ticket, result: Err(ChannelError::TooLarge { max }) });
            }
            tokio::task::spawn_local(async move {
                let mut src = Bytes(bytes, 0);
                let result = s.upload(&meta, &mut src).await;
                let _ = tx.send(Finished::Upload { ticket, result });
            });
        }
        Request::Reconnect | Request::SignOut => {}
    }
}

/// Итог задачи: ответ интерфейсу, найденное — в пачку. `Some` — связь пропала.
fn finished(fin: Finished, store: &ChannelStore, ctx: &RunCtx, pending: &mut Pending) -> Option<ChannelError> {
    let notify = &*ctx.notify;
    let page = |pending: &mut Pending, page: &Page| {
        for (conversation, m) in &page.messages {
            pending.events.push(ChannelEvent::Messages { conversation: conversation.clone(), messages: vec![m.clone()], live: false });
        }
    };
    match fin {
        Finished::CatchUp(Ok(())) => None,
        Finished::CatchUp(Err(e)) => match e {
            ChannelError::Network(_) | ChannelError::Wait { .. } | ChannelError::Protocol(_) => Some(e),
            e => {
                tracing::warn!("channel catch-up: {e}");
                stopped(store, ctx, e);
                None
            }
        },
        Finished::History(Ok(p)) => {
            page(pending, &p);
            None
        }
        Finished::History(Err(e)) => {
            tracing::warn!("channel history: {e}");
            matches!(e, ChannelError::Network(_)).then_some(e)
        }
        Finished::Search { ticket, result } => {
            let result = result.map(|p| {
                page(pending, &p);
                p.messages.into_iter().map(|(c, m)| (c, m.id)).collect()
            });
            notify(Update::Found { ticket, result });
            None
        }
        Finished::Act { client_id: Some(client_id), result, ticket } => {
            match result {
                Ok(Done::Sent { id, .. }) => notify(Update::Sent { client_id, id }),
                Ok(other) => notify(Update::Acted { ticket, result: Ok(other) }),
                Err(error) => notify(Update::SendFailed { client_id, error }),
            }
            None
        }
        Finished::Act { ticket, result, client_id: None } => {
            notify(Update::Acted { ticket, result });
            None
        }
        Finished::Download { conversation, message, source, mime, sink, result } => {
            let result = result.and_then(|()| store.finish_download(sink, &conversation, &message, &source, &mime));
            notify(Update::Downloaded { conversation, message, source, result });
            None
        }
        Finished::Upload { ticket, result } => {
            notify(Update::Uploaded { ticket, result });
            None
        }
    }
}

/// Записать пачку одной транзакцией и сказать интерфейсу; пропуски — дозапрос истории.
fn flush(
    store: &ChannelStore,
    ctx: &RunCtx,
    session: &Rc<dyn ChannelSession>,
    done: &UnboundedSender<Finished>,
    pending: &mut Pending,
    regap: &mut bool,
) {
    pending.deadline = None;
    if pending.events.is_empty() {
        return;
    }
    let events = std::mem::take(&mut pending.events);
    let applied = match store.apply(events) {
        Ok(a) => a,
        Err(e) => {
            // Курсор не записан: события придут снова при следующем догоне.
            tracing::error!("channel batch: {e}");
            return (ctx.notify)(Update::State(ChannelState::Problem(e)));
        }
    };
    let notify = &*ctx.notify;
    if let Some(s) = &applied.secret
        && let Err(e) = ctx.secrets.put(store.channel(), store.account(), s)
    {
        tracing::error!("channel secret: {e}");
    }
    if !applied.changed.is_empty() || !applied.gone.is_empty() {
        notify(Update::Changed { conversations: applied.changed, gone: applied.gone });
    }
    let mut arrived: Vec<(ConversationId, Vec<MessageId>)> = Vec::new();
    for (c, m) in applied.arrived {
        match arrived.iter_mut().find(|(k, _)| *k == c) {
            Some((_, ms)) => ms.push(m),
            None => arrived.push((c, vec![m])),
        }
    }
    for (conversation, messages) in arrived {
        notify(Update::Arrived { conversation, messages });
    }
    if let Some(f) = applied.features {
        notify(Update::Features(f));
    }
    if let Some(s) = applied.state {
        notify(Update::State(s));
    }
    for gap in applied.gaps {
        match gap {
            Some(conv) => history(session, done, conv, None, HISTORY_PAGE),
            None if *regap => {
                *regap = false;
                catch_up(session, store, done);
            }
            None => tracing::warn!("channel gap again: waiting for the next reconnect"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_policy() {
        let mut attempt = 0;
        assert_eq!(retry_after(&ChannelError::Network("x".into()), &mut attempt), Some(Duration::from_secs(5)));
        assert_eq!(retry_after(&ChannelError::Network("x".into()), &mut attempt), Some(Duration::from_secs(10)));
        assert_eq!(retry_after(&ChannelError::Wait { secs: 3 }, &mut attempt), Some(Duration::from_secs(3)));
        assert_eq!(retry_after(&ChannelError::SignedOut("x".into()), &mut attempt), None);
        assert_eq!(retry_after(&ChannelError::Denied("x".into()), &mut attempt), None);
    }

    #[test]
    fn bytes_source_reads_in_pieces() {
        let mut b = Bytes(b"hello".to_vec(), 0);
        let mut buf = [0u8; 3];
        assert_eq!(b.read(&mut buf).unwrap(), 3);
        assert_eq!(&buf, b"hel");
        assert_eq!(b.read(&mut buf).unwrap(), 2);
        assert_eq!(b.read(&mut buf).unwrap(), 0);
        assert_eq!(b.len(), 5);
    }
}
