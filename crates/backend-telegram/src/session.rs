//! Подключённая учётка Telegram (`ChannelSession`): догон (`getState`, диалоги, папки, последние сообщения), поток
//! обновлений (ADR 0012 «Проверка договора», поток событий), история, поиск на сервере, действия, файлы. Пишет
//! события в `incoming`; хранение — у ядра.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use grammers_tl_types as tl;
use postman_core::backend::BoxFuture;
use postman_core::channel::{
    AccountId, Action, Address, AttachmentKind, AttachmentSource, ByteSink, ByteSource, ChannelError, ChannelEvent, ChannelFeatures, ChannelFolder,
    ChannelId, ChannelSession, ChannelState, Conversation, ConversationId, Counts, Cursor, Done, FileMeta, Flow, FlowKind, FolderRef, Incoming,
    Member, Message, MessageId, Outgoing, Page, Participant, ParticipantId, PluginStore, Reader, Role, Uploaded,
};
use postman_ir::{Align, Block, Document, Inline};

use crate::convert::{self, Ctx, Kindly, Source, UpFile};
use crate::ids::{Peer, PeerBook, msg_id, parse_msg, user_of};
use crate::text;
use crate::vault::UpdState;
use crate::wire::{Feed, UpdateBatch, Wire, WireError, call, call_in};
use crate::{ARCHIVE, CAPTION_MAX, ID, MAX_FILE, MAX_TEXT, Shared};

/// Диалогов при догоне: основной список и архив.
const MAX_DIALOGS: usize = 500;
const MAX_ARCHIVE: usize = 200;
/// Последние сообщения при первом входе — у стольких самых свежих бесед (остальные — по прокрутке, ADR 0012 п. 9).
const FIRST_HISTORY: usize = 20;
/// Кусок загрузки и выгрузки файлов.
const PART: usize = 512 * 1024;
/// Больше — `saveBigFilePart`.
const BIG_FILE: u64 = 10 * 1024 * 1024;
/// Проверка связи (`updates.getState`): сеть тихо пропала — заметить и догнать.
const HEARTBEAT: Duration = Duration::from_secs(60);

pub(crate) struct Inner {
    wire: Rc<dyn Wire>,
    shared: Arc<Shared>,
    account: AccountId,
    me: Participant,
    my_id: i64,
    incoming: Incoming,
    store: Rc<dyn PluginStore>,
    book: PeerBook,
    convs: RefCell<HashMap<ConversationId, Conversation>>,
    names: RefCell<HashMap<i64, String>>,
    state: RefCell<Option<UpdState>>,
    feeding: Cell<bool>,
    authed_dcs: RefCell<HashSet<i32>>,
    tasks: RefCell<Vec<tokio::task::JoinHandle<()>>>,
}

pub(crate) struct TgSession(Rc<Inner>);

impl Drop for TgSession {
    fn drop(&mut self) {
        for t in self.0.tasks.borrow_mut().drain(..) {
            t.abort();
        }
        self.0.wire.close();
    }
}

fn now_s() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX)).unwrap_or(0)
}

/// Случайное число без своей зависимости: хэш с ключом процесса и счётчиком (номер файла, `random_id` пересылки).
fn random_i64() -> i64 {
    use std::hash::{BuildHasher, RandomState};
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    RandomState::new().hash_one((n, std::time::SystemTime::now())) as i64
}

fn messages_parts(m: tl::enums::messages::Messages) -> (Vec<tl::enums::Message>, Vec<tl::enums::User>, Vec<tl::enums::Chat>, Option<i32>) {
    match m {
        tl::enums::messages::Messages::Messages(m) => (m.messages, m.users, m.chats, None),
        tl::enums::messages::Messages::Slice(m) => (m.messages, m.users, m.chats, Some(m.count)),
        tl::enums::messages::Messages::ChannelMessages(m) => (m.messages, m.users, m.chats, Some(m.count)),
        tl::enums::messages::Messages::NotModified(_) => (Vec::new(), Vec::new(), Vec::new(), None),
    }
}

fn kind_of(mime: &str) -> AttachmentKind {
    if mime == "image/jpeg" || mime == "image/png" { AttachmentKind::Image { w: 0, h: 0 } } else { AttachmentKind::File }
}

impl Inner {
    fn emit(&self, ev: ChannelEvent) {
        let _ = self.incoming.send(ev);
    }

    /// Ключи ДЦ изменились — новый секрет ядру.
    fn check_secret(&self) {
        if self.wire.take_changed() {
            self.emit(ChannelEvent::Secret(self.wire.blob(self.my_id).to_secret()));
        }
    }

    fn peer_of(&self, conv: &ConversationId) -> Result<(Peer, tl::enums::InputPeer), ChannelError> {
        let p = Peer::parse(conv).ok_or(ChannelError::NotFound)?;
        let ip = self.book.input_peer(p).ok_or(ChannelError::NotFound)?;
        Ok((p, ip))
    }

    fn learn(&self, users: &[tl::enums::User], chats: &[tl::enums::Chat]) {
        self.book.learn(users, chats);
        for c in chats {
            if let tl::enums::Chat::Channel(ch) = c
                && let Some(h) = ch.access_hash
                && !ch.min
            {
                self.wire.remember_channel(ch.id, h);
            }
            if let (Some(p), Some(name)) = (convert::chat_peer(c), convert::chat_username(c)) {
                self.shared.set_username(&p.conv(), &name);
            }
        }
        let mut names = self.names.borrow_mut();
        for u in users {
            if let tl::enums::User::User(u) = u {
                names.insert(u.id, convert::user_name(u));
                if let Some(n) = &u.username {
                    self.shared.set_username(&Peer::User(u.id).conv(), n);
                }
            }
        }
    }

    fn people(&self, users: &[tl::enums::User]) {
        let people = convert::people(users);
        if !people.is_empty() {
            self.emit(ChannelEvent::People(people));
        }
    }

    /// Беседа известна ядру; нет — рассказать о ней до её сообщений.
    fn ensure_conv(&self, conv: &ConversationId, cx: &Ctx) {
        if self.convs.borrow().contains_key(conv) {
            return;
        }
        let Some(p) = Peer::parse(conv) else { return };
        let mut c = convert::conversation_of(p, cx);
        c.unread = Some(Counts::default());
        self.convs.borrow_mut().insert(conv.clone(), c.clone());
        self.emit(ChannelEvent::Conversations(vec![c]));
    }

    fn emit_convs(&self, ids: &HashSet<ConversationId>) {
        if ids.is_empty() {
            return;
        }
        let convs = self.convs.borrow();
        let list: Vec<Conversation> = ids.iter().filter_map(|id| convs.get(id).cloned()).collect();
        drop(convs);
        if !list.is_empty() {
            self.emit(ChannelEvent::Conversations(list));
        }
    }

    /// Пачка обновлений → события ядра.
    fn on_batch(&self, b: UpdateBatch) {
        use tl::enums::Update as U;
        self.learn(&b.users, &b.chats);
        self.people(&b.users);
        let cx = Ctx::new(self.my_id, &b.users, &b.chats);
        let mut changed: HashSet<ConversationId> = HashSet::new();
        let typing_on = |a: &tl::enums::SendMessageAction| !matches!(a, tl::enums::SendMessageAction::SendMessageCancelAction);
        for u in &b.updates {
            match u {
                U::NewMessage(tl::types::UpdateNewMessage { message, .. })
                | U::NewChannelMessage(tl::types::UpdateNewChannelMessage { message, .. }) => {
                    let Some((conv, m)) = convert::message(message, &cx) else { continue };
                    self.ensure_conv(&conv, &cx);
                    if !m.mine
                        && let Some(c) = self.convs.borrow_mut().get_mut(&conv)
                    {
                        let n = c.unread.get_or_insert_with(Counts::default);
                        n.unread = n.unread.saturating_add(1);
                        if m.mentions.iter().any(|p| p.0 == self.my_id.to_string()) {
                            n.mentions = n.mentions.saturating_add(1);
                        }
                        changed.insert(conv.clone());
                    }
                    self.emit(ChannelEvent::Messages { conversation: conv, messages: vec![m], live: true });
                }
                U::EditMessage(tl::types::UpdateEditMessage { message, .. })
                | U::EditChannelMessage(tl::types::UpdateEditChannelMessage { message, .. }) => {
                    if let Some((conv, m)) = convert::message(message, &cx) {
                        self.ensure_conv(&conv, &cx);
                        self.emit(ChannelEvent::Messages { conversation: conv, messages: vec![m], live: false });
                    }
                }
                U::DeleteMessages(d) => {
                    self.emit(ChannelEvent::Deleted { conversation: None, ids: d.messages.iter().map(|&i| msg_id(i)).collect(), for_all: true })
                }
                U::DeleteChannelMessages(d) => self.emit(ChannelEvent::Deleted {
                    conversation: Some(Peer::Channel(d.channel_id).conv()),
                    ids: d.messages.iter().map(|&i| msg_id(i)).collect(),
                    for_all: true,
                }),
                U::ReadHistoryInbox(r) => self.read_me(Peer::from_tl(&r.peer).conv(), r.max_id, r.still_unread_count, &mut changed),
                U::ReadChannelInbox(r) => self.read_me(Peer::Channel(r.channel_id).conv(), r.max_id, r.still_unread_count, &mut changed),
                U::ReadHistoryOutbox(r) => {
                    let p = Peer::from_tl(&r.peer);
                    self.emit(ChannelEvent::Read { conversation: p.conv(), who: Reader::Them(p.participant()), up_to: msg_id(r.max_id) });
                }
                U::ReadChannelOutbox(r) => {
                    let p = Peer::Channel(r.channel_id);
                    self.emit(ChannelEvent::Read { conversation: p.conv(), who: Reader::Them(p.participant()), up_to: msg_id(r.max_id) });
                }
                U::UserTyping(t) => self.emit(ChannelEvent::Typing {
                    conversation: Peer::User(t.user_id).conv(),
                    who: ParticipantId(t.user_id.to_string()),
                    on: typing_on(&t.action),
                }),
                U::ChatUserTyping(t) => self.emit(ChannelEvent::Typing {
                    conversation: Peer::Chat(t.chat_id).conv(),
                    who: Peer::from_tl(&t.from_id).participant(),
                    on: typing_on(&t.action),
                }),
                U::ChannelUserTyping(t) => self.emit(ChannelEvent::Typing {
                    conversation: Peer::Channel(t.channel_id).conv(),
                    who: Peer::from_tl(&t.from_id).participant(),
                    on: typing_on(&t.action),
                }),
                U::MessageReactions(r) => self.emit(ChannelEvent::Reactions {
                    conversation: Peer::from_tl(&r.peer).conv(),
                    message: msg_id(r.msg_id),
                    reactions: convert::reactions(&r.reactions),
                }),
                U::DialogPinned(d) => {
                    if let tl::enums::DialogPeer::Peer(p) = &d.peer {
                        let conv = Peer::from_tl(&p.peer).conv();
                        if let Some(c) = self.convs.borrow_mut().get_mut(&conv) {
                            c.pinned = d.pinned;
                            changed.insert(conv);
                        }
                    }
                }
                U::NotifySettings(n) => {
                    if let tl::enums::NotifyPeer::Peer(p) = &n.peer {
                        let conv = Peer::from_tl(&p.peer).conv();
                        if let Some(c) = self.convs.borrow_mut().get_mut(&conv) {
                            c.muted_until = convert::muted_until(&n.notify_settings, now_s());
                            changed.insert(conv);
                        }
                    }
                }
                U::ChannelTooLong(t) => self.emit(ChannelEvent::Gap { conversation: Some(Peer::Channel(t.channel_id).conv()) }),
                _ => {}
            }
        }
        self.emit_convs(&changed);
    }

    fn read_me(&self, conv: ConversationId, max_id: i32, still: i32, changed: &mut HashSet<ConversationId>) {
        if let Some(c) = self.convs.borrow_mut().get_mut(&conv) {
            c.last_read = Some(msg_id(max_id));
            let n = c.unread.get_or_insert_with(Counts::default);
            n.unread = u32::try_from(still).unwrap_or(0);
            if still == 0 {
                n.mentions = 0;
            }
            changed.insert(conv.clone());
        }
        self.emit(ChannelEvent::Read { conversation: conv, who: Reader::Me, up_to: msg_id(max_id) });
    }

    /// Ответ сервера с обновлениями (отправка, правка, реакция, пересылка) → события; номер сообщения по `random_id`.
    fn absorb(&self, updates: tl::enums::Updates, sent: Option<&Sent>) -> Option<(i32, i32)> {
        use tl::enums::Updates as Us;
        let mut found = None;
        let batch = match updates {
            Us::UpdateShortSentMessage(u) => {
                if let Some(s) = sent {
                    let mut m =
                        convert::short_sent(self.my_id, u.id, u.date, &s.text, u.entities.as_deref().unwrap_or(&s.entities), s.reply_to.clone());
                    if let Some(media) = &u.media {
                        let fake = tl::types::Message {
                            id: u.id,
                            peer_id: s.peer.to_tl(),
                            date: u.date,
                            out: true,
                            media: Some(media.clone()),
                            ..empty_message()
                        };
                        if let Some((_, full)) = convert::message(&tl::enums::Message::Message(fake), &Ctx::new(self.my_id, &[], &[])) {
                            m.attachments = full.attachments;
                        }
                    }
                    self.emit(ChannelEvent::Messages { conversation: s.peer.conv(), messages: vec![m], live: false });
                }
                return Some((u.id, u.date));
            }
            Us::UpdateShort(u) => UpdateBatch { updates: vec![u.update], ..UpdateBatch::default() },
            Us::Updates(u) => UpdateBatch { updates: u.updates, users: u.users, chats: u.chats },
            Us::Combined(u) => UpdateBatch { updates: u.updates, users: u.users, chats: u.chats },
            Us::UpdateShortMessage(_) | Us::UpdateShortChatMessage(_) | Us::TooLong => {
                self.emit(ChannelEvent::Gap { conversation: sent.map(|s| s.peer.conv()) });
                return None;
            }
        };
        if let Some(s) = sent {
            for u in &batch.updates {
                if let tl::enums::Update::MessageId(m) = u
                    && m.random_id == s.random_id
                {
                    found = Some((m.id, 0));
                }
            }
            for u in &batch.updates {
                let msg = match u {
                    tl::enums::Update::NewMessage(n) => Some(&n.message),
                    tl::enums::Update::NewChannelMessage(n) => Some(&n.message),
                    tl::enums::Update::NewScheduledMessage(n) => Some(&n.message),
                    _ => None,
                };
                if let Some(tl::enums::Message::Message(m)) = msg
                    && (found.map(|f| f.0) == Some(m.id) || (found.is_none() && m.out))
                {
                    found = Some((m.id, m.date));
                }
            }
        }
        let scheduled: Vec<tl::enums::Update> =
            batch.updates.iter().filter(|u| matches!(u, tl::enums::Update::NewScheduledMessage(_))).cloned().collect();
        self.on_batch(batch);
        if !scheduled.is_empty() {
            tracing::debug!(n = scheduled.len(), "telegram: scheduled messages accepted");
        }
        found
    }

    async fn get_state(&self) -> Result<UpdState, ChannelError> {
        let tl::enums::updates::State::State(s) = call(&*self.wire, &tl::functions::updates::GetState {}).await?;
        Ok(UpdState { pts: s.pts, qts: s.qts, date: s.date, seq: s.seq, channels: Vec::new() })
    }

    /// Диалоги одного списка (`folder` — `None` основной, `Some(1)` архив) страницами по 100.
    async fn dialogs(&self, folder: Option<i32>, cap: usize, out: &mut Dialogs) -> Result<(), ChannelError> {
        let (mut date, mut id, mut peer) = (0, 0, tl::enums::InputPeer::Empty);
        loop {
            let req = tl::functions::messages::GetDialogs {
                exclude_pinned: false,
                folder_id: folder,
                offset_date: date,
                offset_id: id,
                offset_peer: peer.clone(),
                limit: 100,
                hash: 0,
            };
            let (dialogs, messages, users, chats, slice) = match call(&*self.wire, &req).await? {
                tl::enums::messages::Dialogs::Dialogs(d) => (d.dialogs, d.messages, d.users, d.chats, false),
                tl::enums::messages::Dialogs::Slice(d) => (d.dialogs, d.messages, d.users, d.chats, true),
                tl::enums::messages::Dialogs::NotModified(_) => return Ok(()),
            };
            self.learn(&users, &chats);
            let got = dialogs.len();
            let mut last = None;
            for d in dialogs {
                if let tl::enums::Dialog::Dialog(d) = d {
                    last = Some((Peer::from_tl(&d.peer), d.top_message));
                    out.dialogs.push(d);
                }
            }
            out.messages.extend(messages.iter().cloned());
            out.users.extend(users);
            out.chats.extend(chats);
            let count = out.dialogs.iter().filter(|d| (folder == Some(1)) == (d.folder_id == Some(1))).count();
            let Some((p, top)) = last else { return Ok(()) };
            if !slice || got < 100 || count >= cap {
                return Ok(());
            }
            let top_date = messages.iter().find_map(|m| match m {
                tl::enums::Message::Message(m) if m.id == top && Peer::from_tl(&m.peer_id) == p => Some(m.date),
                tl::enums::Message::Service(m) if m.id == top && Peer::from_tl(&m.peer_id) == p => Some(m.date),
                _ => None,
            });
            let Some(ip) = self.book.input_peer(p) else { return Ok(()) };
            (date, id, peer) = (top_date.unwrap_or(0), top, ip);
        }
    }

    async fn catch_up(self: &Rc<Self>, cursor: Option<Cursor>) -> Result<(), ChannelError> {
        let first = cursor.is_none();
        let start = if self.feeding.get() {
            None
        } else {
            Some(match cursor.as_ref().and_then(UpdState::from_cursor) {
                Some(s) => s,
                None => self.get_state().await?,
            })
        };
        let mut d = Dialogs::default();
        self.dialogs(None, MAX_DIALOGS, &mut d).await?;
        self.dialogs(Some(1), MAX_ARCHIVE, &mut d).await?;
        let filters = match call(&*self.wire, &tl::functions::messages::GetDialogFilters {}).await {
            Ok(tl::enums::messages::DialogFilters::Filters(f)) => f.filters,
            Err(e) => {
                tracing::warn!("telegram: folders not loaded: {e}");
                Vec::new()
            }
        };
        let cx = Ctx::new(self.my_id, &d.users, &d.chats);
        let now = now_s();
        let mut convs: Vec<Conversation> = d.dialogs.iter().map(|x| convert::conversation(x, &cx, now)).collect();
        let known: Vec<Kindly> = convs
            .iter()
            .map(|c| {
                let user = Peer::parse(&c.id).and_then(|p| if let Peer::User(id) = p { cx.users.get(&id).copied() } else { None });
                Kindly { id: c.id.clone(), kind: c.kind, contact: user.is_some_and(|u| u.contact), bot: user.is_some_and(|u| u.bot) }
            })
            .collect();
        let mut folders = convert::folders(&filters, &known, self.my_id);
        let archived: Vec<ConversationId> = convs.iter().filter(|c| c.folders.iter().any(|f| f.0 == ARCHIVE)).map(|c| c.id.clone()).collect();
        if !archived.is_empty() {
            folders.push(ChannelFolder { id: FolderRef::new(ARCHIVE), name: String::new(), parent: None, conversations: archived });
        }
        for c in &mut convs {
            for f in &folders {
                if f.id.0 != ARCHIVE && f.conversations.contains(&c.id) {
                    c.folders.push(f.id.clone());
                }
            }
        }
        self.people(&d.users);
        {
            let mut cache = self.convs.borrow_mut();
            for c in &convs {
                cache.insert(c.id.clone(), c.clone());
            }
        }
        let order: Vec<ConversationId> = convs.iter().map(|c| c.id.clone()).collect();
        self.emit(ChannelEvent::Conversations(convs));
        self.emit(ChannelEvent::Folders(folders));
        let mut tops: HashMap<ConversationId, Vec<Message>> = HashMap::new();
        for (conv, m) in convert::messages(&d.messages, &cx) {
            tops.entry(conv).or_default().push(m);
        }
        for (n, conv) in order.iter().enumerate() {
            if first && n < FIRST_HISTORY {
                match self.history(conv, None, postman_core::channel::HISTORY_PAGE).await {
                    Ok(page) if !page.messages.is_empty() => {
                        let messages: Vec<Message> = page.messages.into_iter().map(|(_, m)| m).collect();
                        self.emit(ChannelEvent::Messages { conversation: conv.clone(), messages, live: false });
                        continue;
                    }
                    Ok(_) => {}
                    Err(e @ (ChannelError::Network(_) | ChannelError::SignedOut(_))) => return Err(e),
                    Err(e) => tracing::warn!("telegram: history not loaded: {e}"),
                }
            }
            if let Some(ms) = tops.remove(conv) {
                self.emit(ChannelEvent::Messages { conversation: conv.clone(), messages: ms, live: false });
            }
        }
        if let Some(s) = start {
            *self.state.borrow_mut() = Some(s.clone());
            let feed = self.wire.feed(Some(s)).await?;
            self.feeding.set(true);
            self.start_tasks(feed);
        }
        let s = self.state.borrow().clone().unwrap_or_default();
        self.emit(ChannelEvent::Cursor(s.to_cursor()));
        self.check_secret();
        Ok(())
    }

    fn start_tasks(self: &Rc<Self>, mut feed: Box<dyn Feed>) {
        let me = Rc::clone(self);
        let pump = tokio::task::spawn_local(async move {
            loop {
                match feed.next().await {
                    Ok(batch) => {
                        me.on_batch(batch);
                        let s = feed.state().await;
                        *me.state.borrow_mut() = Some(s.clone());
                        me.emit(ChannelEvent::Cursor(s.to_cursor()));
                        me.check_secret();
                    }
                    Err(e) => {
                        tracing::info!("telegram: updates stopped: {e}");
                        me.lost(e);
                        break;
                    }
                }
            }
        });
        let me = Rc::clone(self);
        let beat = tokio::task::spawn_local(async move {
            loop {
                tokio::time::sleep(HEARTBEAT).await;
                if let Err(e) = call(&*me.wire, &tl::functions::updates::GetState {}).await
                    && !matches!(e, WireError::Rpc { code: 420 | 500.., .. })
                {
                    tracing::info!("telegram: link check failed: {e}");
                    me.lost(e);
                    break;
                }
            }
        });
        self.tasks.borrow_mut().extend([pump, beat]);
    }

    fn lost(&self, e: WireError) {
        match e.channel() {
            ChannelError::SignedOut(why) => self.emit(ChannelEvent::State(ChannelState::Problem(ChannelError::SignedOut(why)))),
            _ => self.emit(ChannelEvent::State(ChannelState::Offline { retry_in: 5 })),
        }
    }

    async fn history(&self, conv: &ConversationId, before: Option<&MessageId>, limit: u32) -> Result<Page, ChannelError> {
        let (_, ip) = self.peer_of(conv)?;
        let req = tl::functions::messages::GetHistory {
            peer: ip,
            offset_id: before.and_then(parse_msg).unwrap_or(0),
            offset_date: 0,
            add_offset: 0,
            limit: i32::try_from(limit).unwrap_or(100),
            max_id: 0,
            min_id: 0,
            hash: 0,
        };
        let (ms, users, chats, _) = messages_parts(call(&*self.wire, &req).await?);
        self.learn(&users, &chats);
        self.people(&users);
        let cx = Ctx::new(self.my_id, &users, &chats);
        let messages: Vec<(ConversationId, Message)> = convert::messages(&ms, &cx).into_iter().filter(|(c, _)| c == conv).collect();
        let more = messages.len() >= limit as usize && limit > 0;
        Ok(Page { messages, more })
    }

    async fn search(&self, query: &str, conv: Option<&ConversationId>, limit: u32) -> Result<Page, ChannelError> {
        let limit_i = i32::try_from(limit).unwrap_or(100);
        let res = match conv {
            Some(c) => {
                let (_, ip) = self.peer_of(c)?;
                let req = tl::functions::messages::Search {
                    peer: ip,
                    q: query.to_owned(),
                    from_id: None,
                    saved_peer_id: None,
                    saved_reaction: None,
                    top_msg_id: None,
                    filter: tl::enums::MessagesFilter::InputMessagesFilterEmpty,
                    min_date: 0,
                    max_date: 0,
                    offset_id: 0,
                    add_offset: 0,
                    limit: limit_i,
                    max_id: 0,
                    min_id: 0,
                    hash: 0,
                };
                call(&*self.wire, &req).await?
            }
            None => {
                let req = tl::functions::messages::SearchGlobal {
                    broadcasts_only: false,
                    groups_only: false,
                    users_only: false,
                    folder_id: None,
                    community: None,
                    q: query.to_owned(),
                    filter: tl::enums::MessagesFilter::InputMessagesFilterEmpty,
                    min_date: 0,
                    max_date: 0,
                    offset_rate: 0,
                    offset_peer: tl::enums::InputPeer::Empty,
                    offset_id: 0,
                    limit: limit_i,
                };
                call(&*self.wire, &req).await?
            }
        };
        let (ms, users, chats, count) = messages_parts(res);
        self.learn(&users, &chats);
        self.people(&users);
        let cx = Ctx::new(self.my_id, &users, &chats);
        let messages = convert::messages(&ms, &cx);
        let more = count.is_some_and(|n| usize::try_from(n).unwrap_or(0) > messages.len());
        Ok(Page { messages, more })
    }

    fn user_for_mention(&self, id: &str) -> Option<tl::enums::InputUser> {
        self.book.input_user(user_of(&ParticipantId::new(id))?)
    }

    async fn send(&self, o: Outgoing) -> Result<Done, ChannelError> {
        let key = format!("sent/{}", o.client_id);
        if let Some(prev) = self.store.get(&key)?
            && let Some((c, m)) = String::from_utf8_lossy(&prev).split_once('\t').map(|(a, b)| (a.to_owned(), b.to_owned()))
            && c == o.conversation.0
        {
            return Ok(Done::Sent { client_id: o.client_id, id: MessageId(m), at: now_s() * 1000 });
        }
        let (peer, ip) = self.peer_of(&o.conversation)?;
        let mut body = o.body.clone();
        // Цитаты без сообщения-источника — блоком в теле; с источником — цитатой ответа на сервере.
        let target = o.reply_to.clone().or_else(|| o.quotes.iter().find_map(|q| q.source.clone()));
        let quote = o.quotes.iter().find(|q| q.source.is_some() && q.source == target).map(|q| q.text.clone());
        let loose: Vec<Block> = o
            .quotes
            .iter()
            .filter(|q| q.source.is_none() || q.source != target)
            .map(|q| Block::Quote(vec![Block::Paragraph { align: Align::Start, content: vec![Inline::text(q.text.clone())] }]))
            .collect();
        if !loose.is_empty() {
            body = Document { blocks: loose.into_iter().chain(body.blocks).collect(), ..Document::default() };
        }
        let (text, entities) = text::from_ir(&body, &|id| self.user_for_mention(id));
        let limit = if o.attachments.is_empty() { MAX_TEXT } else { CAPTION_MAX };
        if text::utf16_len(&text) > i32::try_from(limit).unwrap_or(i32::MAX) {
            return Err(ChannelError::TooLarge { max: u64::from(limit) });
        }
        let reply_to = target.as_ref().and_then(parse_msg).map(|id| reply_header(id, quote.clone()));
        let schedule = o.at.map(|ms| i32::try_from(ms / 1000).unwrap_or(i32::MAX));
        let random_id = o.client_id as i64;
        let ents = (!entities.is_empty()).then(|| entities.clone());
        let sent = Sent { peer, random_id, text: text.clone(), entities: entities.clone(), reply_to: target.clone() };
        let (id, date) = if o.attachments.is_empty() {
            let req = tl::functions::messages::SendMessage {
                no_webpage: false,
                silent: o.silent,
                background: false,
                clear_draft: true,
                noforwards: false,
                update_stickersets_order: false,
                invert_media: false,
                allow_paid_floodskip: false,
                peer: ip,
                reply_to,
                message: text,
                random_id,
                reply_markup: None,
                entities: ents,
                schedule_date: schedule,
                schedule_repeat_period: None,
                send_as: None,
                quick_reply_shortcut: None,
                effect: None,
                allow_paid_stars: None,
                suggested_post: None,
                rich_message: None,
            };
            let updates = self.send_call(&req).await?;
            self.absorb(updates, Some(&sent)).ok_or_else(|| ChannelError::Protocol("sent, but no message id".into()))?
        } else {
            let mut first = None;
            for (i, up) in o.attachments.iter().enumerate() {
                let rid = random_id.wrapping_add(i as i64);
                let req = tl::functions::messages::SendMedia {
                    silent: o.silent,
                    background: false,
                    clear_draft: i == 0,
                    noforwards: false,
                    update_stickersets_order: false,
                    invert_media: false,
                    allow_paid_floodskip: false,
                    peer: ip.clone(),
                    reply_to: if i == 0 { reply_to.clone() } else { None },
                    media: input_media(up)?,
                    message: if i == 0 { text.clone() } else { String::new() },
                    random_id: rid,
                    reply_markup: None,
                    entities: if i == 0 { ents.clone() } else { None },
                    schedule_date: schedule,
                    schedule_repeat_period: None,
                    send_as: None,
                    quick_reply_shortcut: None,
                    effect: None,
                    allow_paid_stars: None,
                    suggested_post: None,
                };
                let updates = self.send_call(&req).await?;
                let s = Sent { random_id: rid, ..sent.clone() };
                let got = self.absorb(updates, Some(&s));
                if first.is_none() {
                    first = got;
                }
            }
            first.ok_or_else(|| ChannelError::Protocol("sent, but no message id".into()))?
        };
        if let Err(e) = self.store.put(&key, format!("{}\t{id}", o.conversation.0).as_bytes()) {
            tracing::warn!("telegram: sent id not stored: {e}");
        }
        let at = if date > 0 { i64::from(date) * 1000 } else { now_s() * 1000 };
        Ok(Done::Sent { client_id: o.client_id, id: msg_id(id), at })
    }

    /// Отправка: тот же `random_id` уже был (сбой после отправки) — не ошибка, ответ — без нового сообщения.
    async fn send_call<R: tl::RemoteCall<Return = tl::enums::Updates>>(&self, req: &R) -> Result<tl::enums::Updates, ChannelError> {
        match call(&*self.wire, req).await {
            Ok(u) => Ok(u),
            Err(e) if e.is("RANDOM_ID_DUPLICATE") => Err(ChannelError::Protocol("RANDOM_ID_DUPLICATE".into())),
            Err(e) => Err(e.channel()),
        }
    }

    async fn act(&self, action: Action) -> Result<Done, ChannelError> {
        if !crate::features().allows(&action) {
            return Err(ChannelError::Unsupported);
        }
        let r = match action {
            Action::Send(o) => self.send(o).await,
            Action::Edit { conversation, message, body, .. } => {
                let (_, ip) = self.peer_of(&conversation)?;
                let id = parse_msg(&message).ok_or(ChannelError::NotFound)?;
                let (text, entities) = text::from_ir(&body, &|id| self.user_for_mention(id));
                let req = tl::functions::messages::EditMessage {
                    no_webpage: false,
                    invert_media: false,
                    peer: ip,
                    id,
                    message: Some(text),
                    media: None,
                    reply_markup: None,
                    entities: Some(entities),
                    schedule_date: None,
                    schedule_repeat_period: None,
                    quick_reply_shortcut_id: None,
                    rich_message: None,
                };
                let u = call(&*self.wire, &req).await?;
                self.absorb(u, None);
                Ok(Done::Ok)
            }
            Action::Delete { conversation, ids, for_all } => {
                let (peer, _) = self.peer_of(&conversation)?;
                let nums: Vec<i32> = ids.iter().filter_map(parse_msg).collect();
                if nums.is_empty() {
                    return Ok(Done::Ok);
                }
                match peer {
                    Peer::Channel(id) => {
                        let channel = self.book.input_channel(id).ok_or(ChannelError::NotFound)?;
                        call(&*self.wire, &tl::functions::channels::DeleteMessages { channel, id: nums }).await?;
                    }
                    _ => {
                        call(&*self.wire, &tl::functions::messages::DeleteMessages { revoke: for_all, id: nums }).await?;
                    }
                }
                self.emit(ChannelEvent::Deleted { conversation: Some(conversation), ids, for_all: for_all || peer.is_channel() });
                Ok(Done::Ok)
            }
            Action::React { conversation, message, key, on } => {
                if key == "⭐" {
                    return Err(ChannelError::Denied("paid-reaction".into()));
                }
                let (_, ip) = self.peer_of(&conversation)?;
                let msg = parse_msg(&message).ok_or(ChannelError::NotFound)?;
                let reaction = if on { vec![tl::types::ReactionEmoji { emoticon: key }.into()] } else { Vec::new() };
                let req = tl::functions::messages::SendReaction { big: false, add_to_recent: on, peer: ip, msg_id: msg, reaction: Some(reaction) };
                let u = call(&*self.wire, &req).await?;
                self.absorb(u, None);
                Ok(Done::Ok)
            }
            Action::MarkRead { conversation, up_to } => {
                let (peer, ip) = self.peer_of(&conversation)?;
                let max_id = parse_msg(&up_to).unwrap_or(0);
                match peer {
                    Peer::Channel(id) => {
                        let channel = self.book.input_channel(id).ok_or(ChannelError::NotFound)?;
                        call(&*self.wire, &tl::functions::channels::ReadHistory { channel, max_id }).await?;
                    }
                    _ => {
                        call(&*self.wire, &tl::functions::messages::ReadHistory { peer: ip, max_id }).await?;
                    }
                }
                let mut changed = HashSet::new();
                self.read_me(conversation, max_id, 0, &mut changed);
                self.emit_convs(&changed);
                Ok(Done::Ok)
            }
            Action::Typing { conversation, on } => {
                let (_, ip) = self.peer_of(&conversation)?;
                let action =
                    if on { tl::enums::SendMessageAction::SendMessageTypingAction } else { tl::enums::SendMessageAction::SendMessageCancelAction };
                call(&*self.wire, &tl::functions::messages::SetTyping { peer: ip, top_msg_id: None, action }).await?;
                Ok(Done::Ok)
            }
            Action::Forward { from, ids, to } => {
                let (_, from_peer) = self.peer_of(&from)?;
                let (_, to_peer) = self.peer_of(&to)?;
                let id: Vec<i32> = ids.iter().filter_map(parse_msg).collect();
                let req = tl::functions::messages::ForwardMessages {
                    silent: false,
                    background: false,
                    with_my_score: false,
                    drop_author: false,
                    drop_media_captions: false,
                    noforwards: false,
                    allow_paid_floodskip: false,
                    from_ephemeral: false,
                    from_peer,
                    random_id: id.iter().map(|_| random_i64()).collect(),
                    id,
                    to_peer,
                    top_msg_id: None,
                    reply_to: None,
                    schedule_date: None,
                    schedule_repeat_period: None,
                    send_as: None,
                    quick_reply_shortcut: None,
                    effect: None,
                    video_timestamp: None,
                    allow_paid_stars: None,
                    suggested_post: None,
                };
                let u = call(&*self.wire, &req).await?;
                self.absorb(u, None);
                Ok(Done::Ok)
            }
            Action::Mute { conversation, until } => {
                let (_, ip) = self.peer_of(&conversation)?;
                let mute_until = match until {
                    None => 0,
                    Some(i64::MAX) => i32::MAX,
                    Some(ms) => i32::try_from(ms / 1000).unwrap_or(i32::MAX),
                };
                let req = tl::functions::account::UpdateNotifySettings {
                    peer: tl::types::InputNotifyPeer { peer: ip }.into(),
                    settings: tl::types::InputPeerNotifySettings {
                        show_previews: None,
                        silent: None,
                        mute_until: Some(mute_until),
                        sound: None,
                        stories_muted: None,
                        stories_hide_sender: None,
                        stories_sound: None,
                    }
                    .into(),
                };
                call(&*self.wire, &req).await?;
                self.patch(&conversation, |c| c.muted_until = until);
                Ok(Done::Ok)
            }
            Action::Pin { conversation, on } => {
                let (_, ip) = self.peer_of(&conversation)?;
                let req = tl::functions::messages::ToggleDialogPin { pinned: on, peer: tl::types::InputDialogPeer { peer: ip }.into() };
                call(&*self.wire, &req).await?;
                self.patch(&conversation, |c| c.pinned = on);
                Ok(Done::Ok)
            }
            Action::SaveDraft { conversation, draft } => {
                let (_, ip) = self.peer_of(&conversation)?;
                let (text, entities) = draft.map(|d| text::from_ir(&d, &|id| self.user_for_mention(id))).unwrap_or_default();
                let req = tl::functions::messages::SaveDraft {
                    no_webpage: false,
                    invert_media: false,
                    reply_to: None,
                    peer: ip,
                    message: text,
                    entities: (!entities.is_empty()).then_some(entities),
                    media: None,
                    effect: None,
                    suggested_post: None,
                    rich_message: None,
                };
                call(&*self.wire, &req).await?;
                Ok(Done::Ok)
            }
            Action::Start { with } => self.start(with).await,
            Action::Members { conversation } => self.members(&conversation).await,
            Action::Resolve { link } => self.resolve(&link).await,
        };
        self.check_secret();
        r
    }

    fn patch(&self, conv: &ConversationId, f: impl FnOnce(&mut Conversation)) {
        let mut changed = HashSet::new();
        if let Some(c) = self.convs.borrow_mut().get_mut(conv) {
            f(c);
            changed.insert(conv.clone());
        }
        self.emit_convs(&changed);
    }

    /// Название новой группы — имена первых участников.
    fn title_of(&self, ids: &[i64]) -> String {
        let names = self.names.borrow();
        ids.iter().take(3).map(|id| names.get(id).cloned().unwrap_or_else(|| id.to_string())).collect::<Vec<_>>().join(", ")
    }

    async fn start(&self, with: Vec<ParticipantId>) -> Result<Done, ChannelError> {
        let ids: Vec<i64> = with.iter().filter_map(user_of).collect();
        match ids.as_slice() {
            [] => Err(ChannelError::NotFound),
            [one] => {
                self.book.input_user(*one).ok_or(ChannelError::NotFound)?;
                Ok(Done::Conversation(Peer::User(*one).conv()))
            }
            many => {
                let users: Vec<tl::enums::InputUser> = many.iter().filter_map(|id| self.book.input_user(*id)).collect();
                if users.len() != many.len() {
                    return Err(ChannelError::NotFound);
                }
                let title = self.title_of(many);
                let req = tl::functions::messages::CreateChat { users, title, ttl_period: None };
                let tl::enums::messages::InvitedUsers::Users(inv) = call(&*self.wire, &req).await?;
                let chat = match &inv.updates {
                    tl::enums::Updates::Updates(u) => u.chats.iter().find_map(convert::chat_peer),
                    tl::enums::Updates::Combined(u) => u.chats.iter().find_map(convert::chat_peer),
                    _ => None,
                };
                self.absorb(inv.updates, None);
                chat.map(|p| Done::Conversation(p.conv())).ok_or_else(|| ChannelError::Protocol("group created, no id".into()))
            }
        }
    }

    async fn members(&self, conv: &ConversationId) -> Result<Done, ChannelError> {
        let (peer, _) = self.peer_of(conv)?;
        let members = match peer {
            Peer::User(id) => vec![
                Member { id: ParticipantId(self.my_id.to_string()), role: Role::Member },
                Member { id: ParticipantId(id.to_string()), role: Role::Member },
            ],
            Peer::Chat(chat_id) => {
                let tl::enums::messages::ChatFull::Full(full) = call(&*self.wire, &tl::functions::messages::GetFullChat { chat_id }).await?;
                self.learn(&full.users, &full.chats);
                self.people(&full.users);
                match full.full_chat {
                    tl::enums::ChatFull::Full(f) => match f.participants {
                        tl::enums::ChatParticipants::Participants(p) => p
                            .participants
                            .iter()
                            .map(|p| match p {
                                tl::enums::ChatParticipant::Participant(x) => Member { id: ParticipantId(x.user_id.to_string()), role: Role::Member },
                                tl::enums::ChatParticipant::Creator(x) => Member { id: ParticipantId(x.user_id.to_string()), role: Role::Owner },
                                tl::enums::ChatParticipant::Admin(x) => Member { id: ParticipantId(x.user_id.to_string()), role: Role::Admin },
                            })
                            .collect(),
                        tl::enums::ChatParticipants::Forbidden(_) => Vec::new(),
                    },
                    _ => Vec::new(),
                }
            }
            Peer::Channel(id) => {
                let channel = self.book.input_channel(id).ok_or(ChannelError::NotFound)?;
                let req = tl::functions::channels::GetParticipants {
                    channel,
                    filter: tl::enums::ChannelParticipantsFilter::ChannelParticipantsRecent,
                    offset: 0,
                    limit: 200,
                    hash: 0,
                };
                match call(&*self.wire, &req).await? {
                    tl::enums::channels::ChannelParticipants::Participants(p) => {
                        self.learn(&p.users, &p.chats);
                        self.people(&p.users);
                        p.participants
                            .iter()
                            .filter_map(|p| {
                                use tl::enums::ChannelParticipant as P;
                                Some(match p {
                                    P::Participant(x) => Member { id: ParticipantId(x.user_id.to_string()), role: Role::Member },
                                    P::ParticipantSelf(x) => Member { id: ParticipantId(x.user_id.to_string()), role: Role::Member },
                                    P::Creator(x) => Member { id: ParticipantId(x.user_id.to_string()), role: Role::Owner },
                                    P::Admin(x) => Member { id: ParticipantId(x.user_id.to_string()), role: Role::Admin },
                                    P::Banned(x) => Member { id: Peer::from_tl(&x.peer).participant(), role: Role::ReadOnly },
                                    P::Left(_) => return None,
                                })
                            })
                            .collect()
                    }
                    tl::enums::channels::ChannelParticipants::NotModified => Vec::new(),
                }
            }
        };
        Ok(Done::Members(members))
    }

    async fn resolve(&self, link: &str) -> Result<Done, ChannelError> {
        let (head, msg) = crate::split_link(link).ok_or(ChannelError::NotFound)?;
        let conv = match head {
            crate::LinkHead::Channel(id) => Peer::Channel(id).conv(),
            crate::LinkHead::Name(name) => match self.shared.conv_by_username(&name) {
                Some(c) => c,
                None => {
                    let req = tl::functions::contacts::ResolveUsername { username: name, referer: None };
                    let tl::enums::contacts::ResolvedPeer::Peer(r) = call(&*self.wire, &req).await?;
                    self.learn(&r.users, &r.chats);
                    self.people(&r.users);
                    Peer::from_tl(&r.peer).conv()
                }
            },
        };
        let channel = ChannelId::new(ID);
        Ok(Done::Address(match msg {
            Some(m) => Address::message(&channel, &self.account, &conv, &msg_id(m)),
            None => Address::conversation(&channel, &self.account, &conv),
        }))
    }

    /// Ключ авторизации в ДЦ файлов (`auth.exportAuthorization` → `auth.importAuthorization`).
    async fn auth_in(&self, dc: i32) -> Result<(), ChannelError> {
        if dc == self.wire.home_dc() || self.authed_dcs.borrow().contains(&dc) {
            return Ok(());
        }
        let tl::enums::auth::ExportedAuthorization::Authorization(a) =
            call(&*self.wire, &tl::functions::auth::ExportAuthorization { dc_id: dc }).await?;
        call_in(&*self.wire, Some(dc), &tl::functions::auth::ImportAuthorization { id: a.id, bytes: a.bytes }).await?;
        self.authed_dcs.borrow_mut().insert(dc);
        Ok(())
    }

    fn location(&self, s: &Source) -> Result<tl::enums::InputFileLocation, ChannelError> {
        Ok(match s {
            Source::Doc { id, hash, fref, thumb, .. } => {
                tl::types::InputDocumentFileLocation { id: *id, access_hash: *hash, file_reference: fref.clone(), thumb_size: thumb.clone() }.into()
            }
            Source::Photo { id, hash, fref, size, .. } => {
                tl::types::InputPhotoFileLocation { id: *id, access_hash: *hash, file_reference: fref.clone(), thumb_size: size.clone() }.into()
            }
            Source::PeerPhoto { conv, photo, .. } => {
                let (_, peer) = self.peer_of(&ConversationId::new(conv.clone()))?;
                tl::types::InputPeerPhotoFileLocation { big: false, peer, photo_id: *photo }.into()
            }
        })
    }

    /// Просроченная ссылка на файл: перечитать сообщение, взять свежую.
    async fn refresh(&self, s: &Source) -> Result<Source, ChannelError> {
        let (conv, msg) = s.message().ok_or(ChannelError::NotFound)?;
        let conv = ConversationId::new(conv);
        let (peer, _) = self.peer_of(&conv)?;
        let id = vec![tl::enums::InputMessage::Id(tl::types::InputMessageId { id: msg })];
        let res = match peer {
            Peer::Channel(c) => {
                let channel = self.book.input_channel(c).ok_or(ChannelError::NotFound)?;
                call(&*self.wire, &tl::functions::channels::GetMessages { channel, id }).await?
            }
            _ => call(&*self.wire, &tl::functions::messages::GetMessages { id }).await?,
        };
        let (ms, users, chats, _) = messages_parts(res);
        let cx = Ctx::new(self.my_id, &users, &chats);
        let fresh = convert::messages(&ms, &cx);
        let same = |a: &AttachmentSource| Source::parse(a).is_some_and(|n| same_file(&n, s));
        fresh
            .iter()
            .flat_map(|(_, m)| m.attachments.iter())
            .flat_map(|a| std::iter::once(&a.source).chain(a.thumb.iter()))
            .find(|a| same(a))
            .and_then(Source::parse)
            .ok_or(ChannelError::NotFound)
    }

    async fn download(&self, src: &AttachmentSource, out: &mut dyn ByteSink) -> Result<(), ChannelError> {
        let mut s = Source::parse(src).ok_or(ChannelError::NotFound)?;
        let mut dc = s.dc();
        let mut loc = self.location(&s)?;
        let mut offset: i64 = 0;
        let mut refreshed = false;
        let limit = i32::try_from(PART).unwrap_or(i32::MAX);
        loop {
            self.auth_in(dc).await?;
            let target = (dc != self.wire.home_dc()).then_some(dc);
            let req = tl::functions::upload::GetFile { precise: false, cdn_supported: false, location: loc.clone(), offset, limit };
            match call_in(&*self.wire, target, &req).await {
                Ok(tl::enums::upload::File::File(f)) => {
                    out.write(&f.bytes)?;
                    let n = f.bytes.len();
                    if n < PART {
                        break;
                    }
                    offset += n as i64;
                }
                Ok(tl::enums::upload::File::CdnRedirect(_)) => return Err(ChannelError::Protocol("cdn redirect".into())),
                Err(WireError::Rpc { name, value: Some(v), .. }) if name == "FILE_MIGRATE" => dc = i32::try_from(v).unwrap_or(dc),
                Err(e) if e.is("FILE_REFERENCE_*") && !refreshed && offset == 0 => {
                    refreshed = true;
                    s = self.refresh(&s).await?;
                    loc = self.location(&s)?;
                }
                Err(e) if e.is("FILE_ID_INVALID") || e.is("LOCATION_INVALID") || e.is("FILE_REFERENCE_*") => return Err(ChannelError::NotFound),
                Err(e) => return Err(e.channel()),
            }
        }
        self.check_secret();
        Ok(())
    }

    async fn upload(&self, meta: &FileMeta, src: &mut dyn ByteSource) -> Result<Uploaded, ChannelError> {
        if meta.size > MAX_FILE {
            return Err(ChannelError::TooLarge { max: MAX_FILE });
        }
        let big = meta.size > BIG_FILE;
        let parts = i32::try_from(meta.size.div_ceil(PART as u64).max(1)).unwrap_or(i32::MAX);
        let file_id = random_i64();
        let mut buf = vec![0u8; PART];
        let mut sent: u64 = 0;
        for part in 0..parts {
            let mut n = 0;
            while n < PART {
                let r = src.read(&mut buf[n..])?;
                if r == 0 {
                    break;
                }
                n += r;
            }
            let bytes = buf[..n].to_vec();
            sent += n as u64;
            let ok = if big {
                call(&*self.wire, &tl::functions::upload::SaveBigFilePart { file_id, file_part: part, file_total_parts: parts, bytes }).await?
            } else {
                call(&*self.wire, &tl::functions::upload::SaveFilePart { file_id, file_part: part, bytes }).await?
            };
            if !ok {
                return Err(ChannelError::Protocol("file part refused".into()));
            }
        }
        let up = UpFile { id: file_id, parts, name: meta.name.clone(), big };
        Ok(Uploaded {
            name: meta.name.clone(),
            mime: meta.mime.clone(),
            size: sent,
            kind: kind_of(&meta.mime),
            source: AttachmentSource(serde_json::to_vec(&up).map_err(|e| ChannelError::Protocol(e.to_string()))?),
        })
    }
}

fn same_file(a: &Source, b: &Source) -> bool {
    match (a, b) {
        (Source::Doc { id: x, thumb: t, .. }, Source::Doc { id: y, thumb: u, .. }) => x == y && t == u,
        (Source::Photo { id: x, size: s, .. }, Source::Photo { id: y, size: t, .. }) => x == y && s == t,
        _ => false,
    }
}

fn reply_header(id: i32, quote: Option<String>) -> tl::enums::InputReplyTo {
    tl::types::InputReplyToMessage {
        reply_to_msg_id: id,
        top_msg_id: None,
        reply_to_peer_id: None,
        quote_text: quote,
        quote_entities: None,
        quote_offset: None,
        monoforum_peer_id: None,
        todo_item_id: None,
        poll_option: None,
        quote_rich: None,
    }
    .into()
}

fn input_media(up: &Uploaded) -> Result<tl::enums::InputMedia, ChannelError> {
    let f: UpFile = serde_json::from_slice(&up.source.0).map_err(|_| ChannelError::Protocol("not an uploaded file".into()))?;
    if matches!(up.kind, AttachmentKind::Image { .. }) {
        return Ok(tl::types::InputMediaUploadedPhoto {
            spoiler: false,
            live_photo: false,
            file: f.input(),
            stickers: None,
            ttl_seconds: None,
            video: None,
        }
        .into());
    }
    let media = up.mime.starts_with("video/") || up.mime.starts_with("audio/");
    Ok(tl::types::InputMediaUploadedDocument {
        nosound_video: false,
        force_file: !media,
        spoiler: false,
        file: f.input(),
        thumb: None,
        mime_type: if up.mime.is_empty() { "application/octet-stream".into() } else { up.mime.clone() },
        attributes: vec![tl::types::DocumentAttributeFilename { file_name: up.name.clone() }.into()],
        stickers: None,
        video_cover: None,
        video_timestamp: None,
        ttl_seconds: None,
    }
    .into())
}

/// Пустое сообщение для сборки из короткого ответа.
pub(crate) fn empty_message() -> tl::types::Message {
    tl::types::Message {
        out: false,
        mentioned: false,
        media_unread: false,
        silent: false,
        post: false,
        from_scheduled: false,
        legacy: false,
        edit_hide: false,
        pinned: false,
        noforwards: false,
        invert_media: false,
        offline: false,
        video_processing_pending: false,
        paid_suggested_post_stars: false,
        paid_suggested_post_ton: false,
        id: 0,
        from_id: None,
        from_boosts_applied: None,
        from_rank: None,
        peer_id: tl::types::PeerUser { user_id: 0 }.into(),
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        guestchat_via_from: None,
        reply_to: None,
        date: 0,
        message: String::new(),
        media: None,
        reply_markup: None,
        entities: None,
        views: None,
        forwards: None,
        replies: None,
        edit_date: None,
        post_author: None,
        grouped_id: None,
        reactions: None,
        restriction_reason: None,
        ttl_period: None,
        quick_reply_shortcut_id: None,
        effect: None,
        factcheck: None,
        report_delivery_until_date: None,
        paid_message_stars: None,
        suggested_post: None,
        schedule_repeat_period: None,
        summary_from_language: None,
        rich_message: None,
    }
}

/// Что отправили — чтобы собрать сообщение из короткого ответа.
#[derive(Clone)]
pub(crate) struct Sent {
    peer: Peer,
    random_id: i64,
    text: String,
    entities: Vec<tl::enums::MessageEntity>,
    reply_to: Option<MessageId>,
}

#[derive(Default)]
struct Dialogs {
    dialogs: Vec<tl::types::Dialog>,
    messages: Vec<tl::enums::Message>,
    users: Vec<tl::enums::User>,
    chats: Vec<tl::enums::Chat>,
}

impl TgSession {
    /// После `connect`: кто я — с сервера (`users.getUsers([self])`).
    pub(crate) async fn open(
        wire: Rc<dyn Wire>,
        shared: Arc<Shared>,
        account: AccountId,
        incoming: Incoming,
        store: Rc<dyn PluginStore>,
    ) -> Result<Self, ChannelError> {
        let users = call(&*wire, &tl::functions::users::GetUsers { id: vec![tl::enums::InputUser::UserSelf] }).await?;
        let Some(tl::enums::User::User(u)) = users.into_iter().next() else { return Err(ChannelError::SignedOut("no self user".into())) };
        let me = convert::participant(&u);
        let book = PeerBook::new(u.id, Rc::clone(&store));
        book.learn_user(&tl::enums::User::User(u.clone()));
        shared.add_account(&account);
        if let Some(n) = &u.username {
            shared.set_username(&Peer::User(u.id).conv(), n);
        }
        let inner = Inner {
            wire,
            shared,
            account,
            me,
            my_id: u.id,
            incoming,
            store,
            book,
            convs: RefCell::new(HashMap::new()),
            names: RefCell::new(HashMap::new()),
            state: RefCell::new(None),
            feeding: Cell::new(false),
            authed_dcs: RefCell::new(HashSet::new()),
            tasks: RefCell::new(Vec::new()),
        };
        Ok(TgSession(Rc::new(inner)))
    }
}

impl ChannelSession for TgSession {
    fn me(&self) -> Participant {
        self.0.me.clone()
    }

    fn features(&self) -> ChannelFeatures {
        crate::features()
    }

    fn catch_up<'a>(&'a self, cursor: Option<Cursor>) -> BoxFuture<'a, Result<(), ChannelError>> {
        Box::pin(async move { self.0.catch_up(cursor).await })
    }

    fn history<'a>(&'a self, conv: &'a ConversationId, before: Option<&'a MessageId>, limit: u32) -> BoxFuture<'a, Result<Page, ChannelError>> {
        Box::pin(self.0.history(conv, before, limit))
    }

    fn search<'a>(&'a self, query: &'a str, conv: Option<&'a ConversationId>, limit: u32) -> BoxFuture<'a, Result<Page, ChannelError>> {
        Box::pin(self.0.search(query, conv, limit))
    }

    fn act<'a>(&'a self, action: Action) -> BoxFuture<'a, Result<Done, ChannelError>> {
        Box::pin(self.0.act(action))
    }

    fn download<'a>(&'a self, src: &'a AttachmentSource, out: &'a mut dyn ByteSink) -> BoxFuture<'a, Result<(), ChannelError>> {
        Box::pin(self.0.download(src, out))
    }

    fn upload<'a>(&'a self, meta: &'a FileMeta, src: &'a mut dyn ByteSource) -> BoxFuture<'a, Result<Uploaded, ChannelError>> {
        Box::pin(self.0.upload(meta, src))
    }

    fn flow<'a>(&'a self, _kind: FlowKind) -> BoxFuture<'a, Result<Box<dyn Flow + 'a>, ChannelError>> {
        Box::pin(async { Err(ChannelError::Unsupported) })
    }

    fn sign_out<'a>(&'a self) -> BoxFuture<'a, Result<(), ChannelError>> {
        Box::pin(async move {
            call(&*self.0.wire, &tl::functions::auth::LogOut {}).await?;
            Ok(())
        })
    }
}
