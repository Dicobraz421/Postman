//! Проверки договора канала (ADR 0012 п. 11 «Демо и тесты»): набор, который обязан пройти любой канал — демо-канал
//! (`postman_demo::channel`) и каждый настоящий на записанных ответах своего сервера. Договор проверяется без сети.
//!
//! [`run_all`] подключается к каналу и проверяет: вход, отказ чужой сессии, возможности сессии в пределах возможностей
//! канала, первый догон (беседы, последние сообщения, курсор в конце), повтор пачки без записей, догон от курсора,
//! историю, отправку без дублей по `client_id`, честность возможностей («нет у сервера» — `Unsupported`), правку,
//! реакцию, удаление, загрузку и выгрузку, неизвестную беседу и весь поток учётки ([`super::run`]) с базой.
//! События «от других» (правка, реакция, «прочитано», «печатает», удаление без беседы, `Gap`, `Wait`, обрыв связи)
//! сервер показывает по сценарию ([`Script`]).
//!
//! Итог — [`Report`]: проверка не падает, а записывает причину; тест канала требует [`Report::ok`].

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use postman_cache::Cache;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use super::{
    AccountId, Action, Answer, ChannelError, ChannelEvent, ChannelFeatures, ChannelId, ChannelPlugin, ChannelSession, ChannelState, ChannelStore,
    ConnectCtx, ConversationId, Cursor, Done, FileMeta, FlowCtx, Limit, LocalStore, MemorySecrets, Message, MessageId, Outgoing, PluginStore, Prompt,
    Reactions, Reader, Request, RunCtx, Secret, Secrets, Update, text_doc,
};

/// Что сервер канала показывает по сценарию: событие «от других» подключённым сессиям.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Собеседник правит своё сообщение: `Messages` с `edited`.
    EditByOther,
    /// Собеседник ставит реакцию: `Reactions`.
    ReactByOther,
    /// Собеседник прочитал: `Read { Them }`.
    ReadByOther,
    /// Собеседник печатает: `Typing { on }`.
    TypingByOther,
    /// Удаление без беседы (Telegram в личных и группах): `Deleted { conversation: None }`.
    DeleteWithoutConversation,
    /// Сервер не отдал всё: `Gap` беседы, история дозапрашивается.
    Gap,
    /// Сервер просит подождать: следующее действие — `Wait`.
    Wait,
    /// Связь пропала: `State(Offline)` или конец потока событий.
    LinkDrop,
}

/// Сценарий сервера: демо-сервер в памяти или записанные ответы настоящего.
pub trait Script {
    /// Шаги, которые этот сервер умеет показать.
    fn steps(&self) -> Vec<Step>;
    /// Сыграть шаг: события — подключённым сессиям.
    fn play(&self, step: Step);
}

/// Что нужно проверкам.
pub struct Harness {
    pub plugin: Arc<dyn ChannelPlugin>,
    pub account: AccountId,
    /// Действующая сессия учётки.
    pub secret: Secret,
    /// Ответы на вопросы входа по порядку; `None` — вход не проверяется.
    pub sign_in: Option<Vec<Answer>>,
    pub script: Option<Rc<dyn Script>>,
    /// Сколько ждать события после действия.
    pub wait: Duration,
}

/// Итог проверок.
#[derive(Debug, Default)]
pub struct Report {
    pub passed: Vec<&'static str>,
    pub failed: Vec<(&'static str, String)>,
    pub skipped: Vec<&'static str>,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.failed.is_empty()
    }

    fn record(&mut self, name: &'static str, r: Check) {
        match r {
            Ok(Some(())) => self.passed.push(name),
            Ok(None) => self.skipped.push(name),
            Err(e) => self.failed.push((name, e)),
        }
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "passed {}, failed {}, skipped {}", self.passed.len(), self.failed.len(), self.skipped.len())?;
        for (name, why) in &self.failed {
            writeln!(f, "FAILED {name}: {why}")?;
        }
        for name in &self.skipped {
            writeln!(f, "skipped {name}")?;
        }
        Ok(())
    }
}

/// `Ok(Some)` — прошла, `Ok(None)` — не к этому каналу (нет возможности или шага сценария), `Err` — причина.
type Check = Result<Option<()>, String>;

fn ensure(ok: bool, why: impl FnOnce() -> String) -> Result<(), String> {
    if ok { Ok(()) } else { Err(why()) }
}

/// Все проверки; свой рантайм tokio на один поток и `LocalSet`, как поток учётки.
pub fn run_all(h: &Harness) -> Report {
    let mut report = Report::default();
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            report.failed.push(("runtime", e.to_string()));
            return report;
        }
    };
    tokio::task::LocalSet::new().block_on(&rt, all(h, &mut report));
    report
}

/// Подключённая сессия проверок: события — в `rx`.
struct Conn {
    session: Box<dyn ChannelSession>,
    rx: UnboundedReceiver<ChannelEvent>,
    wait: Duration,
}

impl Conn {
    /// События до первого, на котором `stop` — `true` (оно последнее), или до срока.
    async fn until(&mut self, what: &str, mut stop: impl FnMut(&ChannelEvent) -> bool) -> Result<Vec<ChannelEvent>, String> {
        let mut out = Vec::new();
        let deadline = tokio::time::Instant::now() + self.wait;
        loop {
            match tokio::time::timeout_at(deadline, self.rx.recv()).await {
                Ok(Some(ev)) => {
                    let done = stop(&ev);
                    out.push(ev);
                    if done {
                        return Ok(out);
                    }
                }
                Ok(None) => return Err(format!("{what}: event stream closed")),
                Err(_) => return Err(format!("{what}: no event in {:?} (got {} others)", self.wait, out.len())),
            }
        }
    }

    /// Всё, что пришло, без ожидания.
    fn drain(&mut self) -> Vec<ChannelEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            out.push(ev);
        }
        out
    }
}

fn store_of(cache: &Cache, h: &Harness) -> Rc<dyn PluginStore> {
    Rc::new(LocalStore::new(cache.clone(), &ChannelId::new(h.plugin.id()), &h.account))
}

async fn connect(h: &Harness, cache: &Cache, secret: Secret) -> Result<Conn, ChannelError> {
    let (tx, rx) = unbounded_channel();
    let ctx = ConnectCtx { account: h.account.clone(), secret, incoming: tx, store: store_of(cache, h) };
    let session = h.plugin.connect(ctx).await?;
    Ok(Conn { session, rx, wait: h.wait })
}

/// Что дал первый догон.
#[derive(Default)]
struct Seen {
    events: Vec<ChannelEvent>,
    cursor: Option<Cursor>,
    /// Беседы, куда можно писать, с сообщениями.
    writable: Option<ConversationId>,
    /// Беседа с сообщениями и её сообщения (старые первыми).
    messages: Vec<(ConversationId, Message)>,
}

async fn all(h: &Harness, report: &mut Report) {
    report.record("id", check_id(h));
    report.record("sign_in", check_sign_in(h).await);
    let cache = match Cache::open_in_memory() {
        Ok(c) => c,
        Err(e) => return report.failed.push(("cache", e.to_string())),
    };
    report.record("bad_session_is_signed_out", check_bad_session(h, &cache).await);
    let mut conn = match connect(h, &cache, h.secret.clone()).await {
        Ok(c) => c,
        Err(e) => return report.failed.push(("connect", e.to_string())),
    };
    report.record("features_within_plugin", check_features(h, &conn));
    let mut seen = Seen::default();
    report.record("catch_up_first", check_catch_up(&mut conn, &mut seen).await);
    report.record("store_is_idempotent", check_store(h, &seen));
    report.record("catch_up_from_cursor", check_cursor(&mut conn, &seen).await);
    report.record("history", check_history(&mut conn, &seen).await);
    report.record("unknown_conversation", check_unknown(&mut conn).await);
    report.record("send_without_duplicates", check_send(&mut conn, &seen).await);
    report.record("features_are_honest", check_honest(&mut conn, &seen).await);
    report.record("edit", check_edit(&mut conn, &seen).await);
    report.record("react", check_react(&mut conn, &seen).await);
    report.record("delete", check_delete(&mut conn, &seen).await);
    report.record("download", check_download(&mut conn, &seen).await);
    report.record("upload_and_send", check_upload(&mut conn, &seen).await);
    if let Some(script) = h.script.clone() {
        let steps = script.steps();
        for (step, name) in [
            (Step::EditByOther, "script_edit_by_other"),
            (Step::ReactByOther, "script_react_by_other"),
            (Step::ReadByOther, "script_read_by_other"),
            (Step::TypingByOther, "script_typing_by_other"),
            (Step::DeleteWithoutConversation, "script_delete_without_conversation"),
            (Step::Gap, "script_gap"),
            (Step::Wait, "script_wait"),
        ] {
            let r = if steps.contains(&step) { check_step(h, &mut conn, &*script, step, &seen).await } else { Ok(None) };
            report.record(name, r);
        }
        drop(conn);
        let r = if steps.contains(&Step::LinkDrop) { check_link_drop(h, &cache, &*script).await } else { Ok(None) };
        report.record("script_link_drop", r);
    } else {
        drop(conn);
    }
    report.record("run_loop", check_run(h).await);
}

fn check_id(h: &Harness) -> Check {
    let id = h.plugin.id();
    ensure(!id.is_empty() && id != super::MAIL, || format!("channel id {id:?}: empty or taken by mail"))?;
    ensure(id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'), || format!("channel id {id:?}: only a-z, 0-9, -"))?;
    Ok(Some(()))
}

async fn check_sign_in(h: &Harness) -> Check {
    let Some(answers) = &h.sign_in else { return Ok(None) };
    let cache = Cache::open_in_memory().map_err(|e| e.to_string())?;
    let mut flow = h.plugin.sign_in(FlowCtx { store: store_of(&cache, h) }).await.map_err(|e| format!("sign_in: {e}"))?;
    let mut prompt = flow.step(None).await.map_err(|e| format!("first step: {e}"))?;
    for a in answers {
        if matches!(prompt, Prompt::Done { .. } | Prompt::Finished) {
            break;
        }
        prompt = flow.step(Some(a.clone())).await.map_err(|e| format!("step after {prompt:?}: {e}"))?;
    }
    match prompt {
        Prompt::Done { account, name, secret } => {
            ensure(account == h.account, || format!("signed in as {account}, expected {}", h.account))?;
            ensure(!name.is_empty() && !secret.expose().is_empty(), || "Done without name or secret".into())?;
            Ok(Some(()))
        }
        other => Err(format!("sign-in ended with {other:?}, expected Done")),
    }
}

async fn check_bad_session(h: &Harness, cache: &Cache) -> Check {
    match connect(h, cache, Secret::new(b"not-a-session-of-this-channel".to_vec())).await {
        Err(ChannelError::SignedOut(_)) => Ok(Some(())),
        Err(e) => Err(format!("foreign session: {e}, expected SignedOut")),
        Ok(_) => Err("foreign session accepted".into()),
    }
}

/// Возможность сессии есть и у канала.
fn within(s: &ChannelFeatures, p: &ChannelFeatures) -> Result<(), String> {
    let flags = [
        ("read_receipts", s.read_receipts, p.read_receipts),
        ("typing", s.typing, p.typing),
        ("reply_to", s.reply_to, p.reply_to),
        ("quote_fragment", s.quote_fragment, p.quote_fragment),
        ("mentions", s.mentions, p.mentions),
        ("forward_native", s.forward_native, p.forward_native),
        ("scheduled_send", s.scheduled_send, p.scheduled_send),
        ("server_drafts", s.server_drafts, p.server_drafts),
        ("server_mute", s.server_mute, p.server_mute),
        ("server_pin", s.server_pin, p.server_pin),
        ("server_search", s.server_search, p.server_search),
        ("server_unread", s.server_unread, p.server_unread),
        ("folders", s.folders, p.folders),
        ("threads", s.threads, p.threads),
        ("edit", s.edit != Limit::No, p.edit != Limit::No),
        ("delete_for_all", s.delete_for_all != Limit::No, p.delete_for_all != Limit::No),
        ("reactions", s.reactions != Reactions::No, p.reactions != Reactions::No),
        ("e2e", s.e2e != super::E2e::No, p.e2e != super::E2e::No),
    ];
    let extra: Vec<&str> = flags.iter().filter(|(_, s, p)| *s && !*p).map(|(n, _, _)| *n).collect();
    ensure(extra.is_empty(), || format!("session can do what the channel says it cannot: {extra:?}"))
}

fn check_features(h: &Harness, conn: &Conn) -> Check {
    within(&conn.session.features(), &h.plugin.features())?;
    ensure(!conn.session.me().id.0.is_empty(), || "me() without id".into())?;
    Ok(Some(()))
}

/// Сообщения события в беседе.
fn messages_of(events: &[ChannelEvent]) -> Vec<(ConversationId, Message)> {
    let mut out = Vec::new();
    for ev in events {
        if let ChannelEvent::Messages { conversation, messages, .. } = ev {
            out.extend(messages.iter().map(|m| (conversation.clone(), m.clone())));
        }
    }
    out
}

async fn check_catch_up(conn: &mut Conn, seen: &mut Seen) -> Check {
    conn.session.catch_up(None).await.map_err(|e| format!("catch_up(None): {e}"))?;
    let events = conn.until("cursor after catch-up", |e| matches!(e, ChannelEvent::Cursor(_))).await?;
    let me = conn.session.me().id;
    let mut convs = Vec::new();
    let mut writable = HashSet::new();
    let mut per_conv: Vec<(ConversationId, usize)> = Vec::new();
    for ev in &events {
        match ev {
            ChannelEvent::Conversations(cs) => {
                for c in cs {
                    ensure(!c.id.0.is_empty(), || "conversation without id".into())?;
                    if c.my_role.can_write() && c.kind != super::ConversationKind::Broadcast {
                        writable.insert(c.id.clone());
                    }
                    convs.push(c.id.clone());
                }
            }
            ChannelEvent::Messages { conversation, messages, .. } => {
                ensure(convs.contains(conversation), || format!("messages of {conversation} before the conversation itself"))?;
                let mut ids = HashSet::new();
                for m in messages {
                    ensure(!m.id.0.is_empty() && ids.insert(m.id.clone()), || format!("message id {:?} empty or repeated in one event", m.id))?;
                    ensure(m.sent > 0 && !m.author.0.is_empty(), || format!("message {}: no time or author", m.id))?;
                    ensure(m.edited.is_none_or(|e| e >= m.sent), || format!("message {}: edited before sent", m.id))?;
                    ensure(!m.mine || m.author == me, || format!("message {}: mine, but author {} is not me", m.id, m.author))?;
                }
                match per_conv.iter_mut().find(|(c, _)| c == conversation) {
                    Some((_, n)) => *n += messages.len(),
                    None => per_conv.push((conversation.clone(), messages.len())),
                }
            }
            _ => {}
        }
    }
    ensure(!convs.is_empty(), || "first catch-up gave no conversations".into())?;
    let too_many: Vec<_> = per_conv.iter().filter(|(_, n)| *n > super::run::HISTORY_PAGE as usize).collect();
    ensure(too_many.is_empty(), || format!("first catch-up: more than the last {} messages: {too_many:?}", super::run::HISTORY_PAGE))?;
    let Some(ChannelEvent::Cursor(c)) = events.last() else { return Err("no cursor at the end".into()) };
    seen.cursor = Some(c.clone());
    seen.messages = messages_of(&events);
    seen.messages.sort_by_key(|(_, m)| m.sent);
    seen.writable = seen.messages.iter().map(|(c, _)| c).find(|c| writable.contains(*c)).cloned().or_else(|| writable.iter().next().cloned());
    seen.events = events;
    Ok(Some(()))
}

fn check_store(h: &Harness, seen: &Seen) -> Check {
    let cache = Cache::open_in_memory().map_err(|e| e.to_string())?;
    let store = ChannelStore::open(cache, &ChannelId::new(h.plugin.id()), &h.account, "check").map_err(|e| e.to_string())?;
    let first = store.apply(seen.events.clone()).map_err(|e| format!("first batch: {e}"))?;
    ensure(first.written == seen.messages.len(), || format!("written {} of {} messages", first.written, seen.messages.len()))?;
    ensure(store.cursor().map_err(|e| e.to_string())? == seen.cursor, || "cursor is not stored with the batch".into())?;
    let again = store.apply(seen.events.clone()).map_err(|e| format!("repeated batch: {e}"))?;
    ensure(again.written == 0 && again.arrived.is_empty(), || format!("repeated batch wrote {} messages", again.written))?;
    for (c, m) in &seen.messages {
        ensure(store.message(c, &m.id).map_err(|e| e.to_string())?.is_some(), || format!("message {} of {c} is not stored", m.id))?;
    }
    Ok(Some(()))
}

async fn check_cursor(conn: &mut Conn, seen: &Seen) -> Check {
    conn.drain();
    conn.session.catch_up(seen.cursor.clone()).await.map_err(|e| format!("catch_up(cursor): {e}"))?;
    let events = conn.until("cursor after catch-up from cursor", |e| matches!(e, ChannelEvent::Cursor(_))).await?;
    let known: HashSet<(&ConversationId, &MessageId)> = seen.messages.iter().map(|(c, m)| (c, &m.id)).collect();
    let new: Vec<_> = messages_of(&events).into_iter().filter(|(c, m)| !known.contains(&(c, &m.id))).map(|(_, m)| m.id).collect();
    ensure(new.is_empty(), || format!("nothing happened, yet catch-up from cursor gave new messages {new:?}"))?;
    Ok(Some(()))
}

async fn check_history(conn: &mut Conn, seen: &Seen) -> Check {
    let Some((conv, _)) = seen.messages.last() else { return Ok(None) };
    let page = conn.session.history(conv, None, 2).await.map_err(|e| format!("history: {e}"))?;
    ensure(page.messages.len() <= 2, || format!("history limit 2, got {}", page.messages.len()))?;
    ensure(page.messages.iter().all(|(c, _)| c == conv), || "history of one conversation has others".into())?;
    let Some((_, oldest)) = page.messages.iter().min_by_key(|(_, m)| m.sent) else { return Ok(Some(())) };
    let older = conn.session.history(conv, Some(&oldest.id), 50).await.map_err(|e| format!("history before: {e}"))?;
    ensure(older.messages.iter().all(|(_, m)| m.sent <= oldest.sent && m.id != oldest.id), || "history before a message has newer ones".into())?;
    Ok(Some(()))
}

async fn check_unknown(conn: &mut Conn) -> Check {
    let nobody = ConversationId::new("no-such-conversation-in-conformance");
    match conn.session.history(&nobody, None, 10).await {
        Err(ChannelError::NotFound) => {}
        other => return Err(format!("history of unknown conversation: {other:?}, expected NotFound")),
    }
    let send = Action::Send(outgoing(&nobody, 1, "x"));
    match conn.session.act(send).await {
        Err(ChannelError::NotFound) => Ok(Some(())),
        other => Err(format!("send to unknown conversation: {other:?}, expected NotFound")),
    }
}

fn outgoing(conv: &ConversationId, client_id: u64, text: &str) -> Outgoing {
    Outgoing {
        conversation: conv.clone(),
        client_id,
        body: text_doc(text),
        attachments: Vec::new(),
        reply_to: None,
        quotes: Vec::new(),
        mentions: Vec::new(),
        at: None,
        silent: false,
    }
}

/// Своё сообщение в беседе для проверок правки и удаления.
async fn send_one(conn: &mut Conn, conv: &ConversationId, client_id: u64, text: &str) -> Result<MessageId, String> {
    conn.drain();
    match conn.session.act(Action::Send(outgoing(conv, client_id, text))).await {
        Ok(Done::Sent { id, client_id: c, .. }) if c == client_id => {
            conn.until("own message after send", |e| matches!(e, ChannelEvent::Messages { messages, .. } if messages.iter().any(|m| m.id == id)))
                .await?;
            Ok(id)
        }
        other => Err(format!("send: {other:?}, expected Sent with the same client_id")),
    }
}

async fn check_send(conn: &mut Conn, seen: &Seen) -> Check {
    let Some(conv) = seen.writable.clone() else { return Ok(None) };
    let id = send_one(conn, &conv, 7001, "проверка договора").await?;
    let events = conn.drain();
    let again = conn.session.act(Action::Send(outgoing(&conv, 7001, "проверка договора"))).await.map_err(|e| format!("repeat send: {e}"))?;
    let Done::Sent { id: id2, .. } = again else { return Err(format!("repeat send: {again:?}")) };
    ensure(id2 == id, || format!("same client_id gave another message {id2} (first {id})"))?;
    let mine = messages_of(&events).into_iter().chain(messages_of(&conn.drain())).filter(|(_, m)| m.id != id).filter(|(_, m)| m.mine).count();
    ensure(mine == 0, || format!("repeat send produced {mine} more messages"))?;
    let page = conn.session.history(&conv, None, 50).await.map_err(|e| e.to_string())?;
    let copies = page.messages.iter().filter(|(_, m)| m.id == id).count();
    ensure(copies == 1, || format!("history has the sent message {copies} times"))?;
    let m = &page.messages.iter().find(|(_, m)| m.id == id).ok_or("sent message not in history")?.1;
    ensure(m.mine && m.body.plain().contains("проверка договора"), || "sent message: not mine or another text".into())?;
    Ok(Some(()))
}

async fn check_honest(conn: &mut Conn, seen: &Seen) -> Check {
    let Some(conv) = seen.writable.clone() else { return Ok(None) };
    let f = conn.session.features();
    let msg = seen.messages.iter().find(|(c, _)| *c == conv).map(|(_, m)| m.id.clone()).unwrap_or_default();
    let probes = [
        Action::React { conversation: conv.clone(), message: msg.clone(), key: "🧪".into(), on: true },
        Action::Typing { conversation: conv.clone(), on: true },
        Action::Mute { conversation: conv.clone(), until: None },
        Action::Pin { conversation: conv.clone(), on: false },
        Action::SaveDraft { conversation: conv.clone(), draft: None },
        Action::Edit { conversation: conv.clone(), message: msg.clone(), body: text_doc("x"), mentions: Vec::new() },
        Action::Delete { conversation: conv.clone(), ids: Vec::new(), for_all: true },
    ];
    let mut tried = 0;
    for a in probes.into_iter().filter(|a| !f.allows(a)) {
        tried += 1;
        match conn.session.act(a.clone()).await {
            Err(ChannelError::Unsupported) => {}
            other => return Err(format!("{a:?} is not in features, yet the channel answered {other:?}")),
        }
    }
    if tried == 0 {
        return Ok(None);
    }
    Ok(Some(()))
}

async fn check_edit(conn: &mut Conn, seen: &Seen) -> Check {
    let Some(conv) = seen.writable.clone() else { return Ok(None) };
    if conn.session.features().edit == Limit::No {
        return Ok(None);
    }
    let id = send_one(conn, &conv, 7002, "до правки").await?;
    conn.session
        .act(Action::Edit { conversation: conv.clone(), message: id.clone(), body: text_doc("после правки"), mentions: Vec::new() })
        .await
        .map_err(|e| format!("edit: {e}"))?;
    let evs = conn
        .until(
            "edited message",
            |e| matches!(e, ChannelEvent::Messages { messages, .. } if messages.iter().any(|m| m.id == id && m.edited.is_some())),
        )
        .await?;
    let m = messages_of(&evs).into_iter().rev().find(|(_, m)| m.id == id).ok_or("no edited message")?.1;
    ensure(m.body.plain().contains("после правки"), || "edited message has the old text".into())?;
    Ok(Some(()))
}

async fn check_react(conn: &mut Conn, seen: &Seen) -> Check {
    let Some(conv) = seen.writable.clone() else { return Ok(None) };
    let f = conn.session.features();
    let key = match &f.reactions {
        Reactions::No => return Ok(None),
        Reactions::Fixed(keys) => keys.first().cloned().ok_or("fixed reactions without keys")?,
        Reactions::Any => "👍".to_owned(),
    };
    let Some((_, m)) = seen.messages.iter().rev().find(|(c, _)| *c == conv) else { return Ok(None) };
    conn.drain();
    conn.session
        .act(Action::React { conversation: conv.clone(), message: m.id.clone(), key: key.clone(), on: true })
        .await
        .map_err(|e| format!("react: {e}"))?;
    let evs = conn.until("reactions", |e| matches!(e, ChannelEvent::Reactions { message, .. } if *message == m.id)).await?;
    let Some(ChannelEvent::Reactions { reactions, .. }) = evs.last() else { return Err("no reactions".into()) };
    ensure(reactions.iter().any(|r| r.key == key && r.mine && r.count >= 1), || format!("my reaction {key} is not in {reactions:?}"))?;
    Ok(Some(()))
}

async fn check_delete(conn: &mut Conn, seen: &Seen) -> Check {
    let Some(conv) = seen.writable.clone() else { return Ok(None) };
    let for_all = conn.session.features().delete_for_all != Limit::No;
    let id = send_one(conn, &conv, 7003, "удалить").await?;
    conn.session.act(Action::Delete { conversation: conv.clone(), ids: vec![id.clone()], for_all }).await.map_err(|e| format!("delete: {e}"))?;
    if for_all {
        conn.until("deleted", |e| matches!(e, ChannelEvent::Deleted { ids, .. } if ids.contains(&id))).await?;
        let page = conn.session.history(&conv, None, 50).await.map_err(|e| e.to_string())?;
        ensure(page.messages.iter().all(|(_, m)| m.id != id), || "deleted for all, but still in history".into())?;
    }
    Ok(Some(()))
}

async fn check_download(conn: &mut Conn, seen: &Seen) -> Check {
    let Some((_, a)) = seen.messages.iter().find_map(|(c, m)| m.attachments.first().map(|a| (c, a))) else { return Ok(None) };
    let mut sink = Collect(Vec::new());
    conn.session.download(&a.source, &mut sink).await.map_err(|e| format!("download: {e}"))?;
    ensure(!sink.0.is_empty(), || "download gave no bytes".into())?;
    ensure(a.size.is_none_or(|s| s == sink.0.len() as u64), || format!("size {:?}, downloaded {}", a.size, sink.0.len()))?;
    match conn.session.download(&super::AttachmentSource(b"no-such-file".to_vec()), &mut Collect(Vec::new())).await {
        Err(ChannelError::NotFound) => Ok(Some(())),
        other => Err(format!("download of unknown source: {other:?}, expected NotFound")),
    }
}

struct Collect(Vec<u8>);

impl super::ByteSink for Collect {
    fn write(&mut self, chunk: &[u8]) -> Result<(), ChannelError> {
        self.0.extend_from_slice(chunk);
        Ok(())
    }
}

struct Slice<'a>(&'a [u8], usize);

impl super::ByteSource for Slice<'_> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, ChannelError> {
        let rest = &self.0[self.1.min(self.0.len())..];
        let n = rest.len().min(buf.len());
        buf[..n].copy_from_slice(&rest[..n]);
        self.1 += n;
        Ok(n)
    }

    fn len(&self) -> u64 {
        self.0.len() as u64
    }
}

async fn check_upload(conn: &mut Conn, seen: &Seen) -> Check {
    let Some(conv) = seen.writable.clone() else { return Ok(None) };
    let bytes = "файл проверки договора\n".repeat(100).into_bytes();
    let meta = FileMeta { name: "check.txt".into(), mime: "text/plain".into(), size: bytes.len() as u64 };
    let up = conn.session.upload(&meta, &mut Slice(&bytes, 0)).await.map_err(|e| format!("upload: {e}"))?;
    ensure(up.size == meta.size, || format!("uploaded {} of {}", up.size, meta.size))?;
    conn.drain();
    let mut o = outgoing(&conv, 7004, "с вложением");
    o.attachments.push(up);
    let Done::Sent { id, .. } = conn.session.act(Action::Send(o)).await.map_err(|e| format!("send with attachment: {e}"))? else {
        return Err("send with attachment: not Sent".into());
    };
    let evs = conn
        .until("message with attachment", |e| matches!(e, ChannelEvent::Messages { messages, .. } if messages.iter().any(|m| m.id == id)))
        .await?;
    let m = messages_of(&evs).into_iter().find(|(_, m)| m.id == id).ok_or("no message")?.1;
    let a = m.attachments.first().ok_or("message without the attachment")?;
    let mut sink = Collect(Vec::new());
    conn.session.download(&a.source, &mut sink).await.map_err(|e| format!("download uploaded: {e}"))?;
    ensure(sink.0 == bytes, || "downloaded bytes differ from uploaded".into())?;
    Ok(Some(()))
}

async fn check_step(h: &Harness, conn: &mut Conn, script: &dyn Script, step: Step, seen: &Seen) -> Check {
    conn.drain();
    script.play(step);
    match step {
        Step::EditByOther => {
            let evs = conn
                .until(
                    "edit by other",
                    |e| matches!(e, ChannelEvent::Messages { messages, .. } if messages.iter().any(|m| m.edited.is_some() && !m.mine)),
                )
                .await?;
            let cache = Cache::open_in_memory().map_err(|e| e.to_string())?;
            let store = ChannelStore::open(cache, &ChannelId::new(h.plugin.id()), &h.account, "check").map_err(|e| e.to_string())?;
            store.apply(seen.events.clone()).map_err(|e| e.to_string())?;
            store.apply(evs.clone()).map_err(|e| e.to_string())?;
            let (c, m) = messages_of(&evs).into_iter().rev().find(|(_, m)| m.edited.is_some()).ok_or("no edited")?;
            if seen.messages.iter().any(|(sc, sm)| *sc == c && sm.id == m.id) {
                let edits = store.edits(&c, &m.id).map_err(|e| e.to_string())?;
                ensure(!edits.is_empty(), || "edit by other: previous version is not in edit history".into())?;
            }
        }
        Step::ReactByOther => {
            conn.until(
                "reaction by other",
                |e| matches!(e, ChannelEvent::Reactions { reactions, .. } if reactions.iter().any(|r| !r.mine || r.count > 1)),
            )
            .await?;
        }
        Step::ReadByOther => {
            conn.until("read by other", |e| matches!(e, ChannelEvent::Read { who: Reader::Them(_), .. })).await?;
        }
        Step::TypingByOther => {
            conn.until("typing by other", |e| matches!(e, ChannelEvent::Typing { on: true, .. })).await?;
        }
        Step::DeleteWithoutConversation => {
            let evs = conn.until("delete without conversation", |e| matches!(e, ChannelEvent::Deleted { conversation: None, .. })).await?;
            let Some(ChannelEvent::Deleted { ids, .. }) = evs.last() else { return Err("no delete".into()) };
            let cache = Cache::open_in_memory().map_err(|e| e.to_string())?;
            let store = ChannelStore::open(cache, &ChannelId::new(h.plugin.id()), &h.account, "check").map_err(|e| e.to_string())?;
            store.apply(seen.events.clone()).map_err(|e| e.to_string())?;
            store.apply(evs.clone()).map_err(|e| e.to_string())?;
            for (c, m) in seen.messages.iter().filter(|(_, m)| ids.contains(&m.id)) {
                let left = store.message(c, &m.id).map_err(|e| e.to_string())?;
                ensure(left.as_ref().is_none_or(|e| e.message().is_none()), || {
                    format!("message {} still has text after delete without conversation", m.id)
                })?;
            }
            ensure(seen.messages.iter().any(|(_, m)| ids.contains(&m.id)), || "deleted ids are not among caught-up messages".into())?;
        }
        Step::Gap => {
            let evs = conn.until("gap", |e| matches!(e, ChannelEvent::Gap { .. })).await?;
            if let Some(ChannelEvent::Gap { conversation: Some(c) }) = evs.last() {
                let page = conn.session.history(c, None, super::run::HISTORY_PAGE).await.map_err(|e| format!("history after gap: {e}"))?;
                let known: HashSet<&MessageId> = seen.messages.iter().map(|(_, m)| &m.id).collect();
                ensure(page.messages.iter().any(|(_, m)| !known.contains(&m.id)), || "history after gap has nothing new".into())?;
            }
        }
        Step::Wait => {
            let Some(conv) = seen.writable.clone() else { return Ok(None) };
            match conn
                .session
                .act(Action::MarkRead { conversation: conv, up_to: seen.messages.last().map(|(_, m)| m.id.clone()).unwrap_or_default() })
                .await
            {
                Err(ChannelError::Wait { secs }) if secs > 0 => {}
                other => return Err(format!("after Wait step: {other:?}, expected Wait")),
            }
        }
        Step::LinkDrop => return Ok(None),
    }
    Ok(Some(()))
}

async fn check_link_drop(h: &Harness, cache: &Cache, script: &dyn Script) -> Check {
    let mut conn = connect(h, cache, h.secret.clone()).await.map_err(|e| format!("connect: {e}"))?;
    conn.session.catch_up(None).await.map_err(|e| e.to_string())?;
    conn.until("cursor", |e| matches!(e, ChannelEvent::Cursor(_))).await?;
    script.play(Step::LinkDrop);
    let deadline = tokio::time::Instant::now() + h.wait;
    loop {
        match tokio::time::timeout_at(deadline, conn.rx.recv()).await {
            Ok(Some(ChannelEvent::State(ChannelState::Offline { .. } | ChannelState::Problem(ChannelError::Network(_))))) | Ok(None) => break,
            Ok(Some(_)) => {}
            Err(_) => return Err("link drop: neither Offline nor end of events".into()),
        }
    }
    match conn.session.catch_up(None).await {
        Err(ChannelError::Network(_)) => {}
        other => return Err(format!("catch-up without link: {other:?}, expected Network")),
    }
    drop(conn);
    let mut again = connect(h, cache, h.secret.clone()).await.map_err(|e| format!("reconnect after link drop: {e}"))?;
    again.session.catch_up(None).await.map_err(|e| format!("catch-up after reconnect: {e}"))?;
    again.until("cursor after reconnect", |e| matches!(e, ChannelEvent::Cursor(_))).await?;
    Ok(Some(()))
}

/// Ждать ответа потока учётки.
async fn update(rx: &mut UnboundedReceiver<Update>, wait: Duration, what: &str, mut want: impl FnMut(&Update) -> bool) -> Result<Update, String> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(u)) if want(&u) => return Ok(u),
            Ok(Some(_)) => {}
            Ok(None) => return Err(format!("{what}: channel thread ended")),
            Err(_) => return Err(format!("{what}: no update in {wait:?}")),
        }
    }
}

/// Поток учётки целиком: секрет из хранилища, подключение, догон в базу (курсор той же транзакцией), отправка через
/// запрос, отказ без запроса к серверу по возможностям, «Выйти».
async fn check_run(h: &Harness) -> Check {
    let cache = Cache::open_in_memory().map_err(|e| e.to_string())?;
    let channel = ChannelId::new(h.plugin.id());
    let secrets = Arc::new(MemorySecrets::default());
    secrets.put(&channel, &h.account, &h.secret).map_err(|e| e.to_string())?;
    let (req_tx, requests) = unbounded_channel();
    let (up_tx, mut up) = unbounded_channel();
    let ctx = RunCtx {
        cache: cache.clone(),
        secrets: secrets.clone(),
        account: h.account.clone(),
        name: "check".into(),
        requests,
        notify: Box::new(move |u| {
            let _ = up_tx.send(u);
        }),
    };
    let plugin = Arc::clone(&h.plugin);
    let task = tokio::task::spawn_local(async move { super::run(&*plugin, ctx).await });
    let Update::Connected { features, .. } = update(&mut up, h.wait, "connected", |u| matches!(u, Update::Connected { .. })).await? else {
        return Err("not connected".into());
    };
    update(&mut up, h.wait, "changed after catch-up", |u| matches!(u, Update::Changed { .. })).await?;
    let store = ChannelStore::open(cache.clone(), &channel, &h.account, "check").map_err(|e| e.to_string())?;
    // Курсор пишется с последней пачкой догона: подождать её.
    let deadline = tokio::time::Instant::now() + h.wait;
    while store.cursor().map_err(|e| e.to_string())?.is_none() {
        if tokio::time::Instant::now() > deadline {
            return Err("cursor is not stored after catch-up".into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let convs = ChannelStore::conversations(&cache, 0, 1000).map_err(|e| e.to_string())?;
    ensure(!convs.is_empty(), || "no conversations in the store after catch-up".into())?;
    let writable = convs.iter().find(|c| c.conversation.my_role.can_write() && c.conversation.kind != super::ConversationKind::Broadcast);
    if let Some(c) = writable {
        let id = c.conversation.id.clone();
        let _ = req_tx.send(Request::Act { ticket: 1, action: Action::Send(outgoing(&id, 7101, "через поток учётки")) });
        let Update::Sent { id: sent, .. } =
            update(&mut up, h.wait, "sent", |u| matches!(u, Update::Sent { client_id: 7101, .. } | Update::SendFailed { .. })).await?
        else {
            return Err("send through the thread failed".into());
        };
        let deadline = tokio::time::Instant::now() + h.wait;
        while store.message(&id, &sent).map_err(|e| e.to_string())?.is_none() {
            if tokio::time::Instant::now() > deadline {
                return Err("sent message is not in the store".into());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        if features.reactions == Reactions::No {
            let react = Action::React { conversation: id.clone(), message: sent.clone(), key: "👍".into(), on: true };
            let _ = req_tx.send(Request::Act { ticket: 2, action: react });
            let u = update(&mut up, h.wait, "refused reaction", |u| matches!(u, Update::Acted { ticket: 2, .. })).await?;
            ensure(matches!(u, Update::Acted { result: Err(ChannelError::Unsupported), .. }), || format!("reaction without the feature: {u:?}"))?;
        }
    }
    let _ = req_tx.send(Request::SignOut);
    update(&mut up, h.wait, "signed out", |u| matches!(u, Update::SignedOut)).await?;
    ensure(cache.chan_accounts().map_err(|e| e.to_string())?.is_empty(), || "sign-out left the account in the store".into())?;
    ensure(secrets.get(&channel, &h.account).is_none(), || "sign-out left the secret".into())?;
    let _ = tokio::time::timeout(h.wait, task).await;
    Ok(Some(()))
}
