//! Демо-канал `demo` (ADR 0012 п. 11): все возможности договора, сервер в памяти, сценарий событий «от других»
//! ([`DemoServer`] как `conformance::Script`), свои часы. На нём договор каналов проверяется без сети
//! (`postman_core::channel::conformance`), им же наполняются демо и снимки чатов (CH-04).
//!
//! Как настоящий сервер: номера сообщений уникальны на учётку, удаление в личной беседе приходит без беседы
//! (как у Telegram), повтор отправки с тем же `client_id` не даёт дубля, курсор — номер последнего события журнала.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard};

use postman_core::backend::BoxFuture;
use postman_core::channel::conformance::{Script, Step};
use postman_core::channel::{
    AccountId, Action, Address, Answer, Attachment, AttachmentKind, AttachmentSource, Body, ByteSink, ByteSource, ChannelError, ChannelEvent,
    ChannelFeatures, ChannelFolder, ChannelId, ChannelPlugin, ChannelSession, ChannelState, CodeVia, ConnectCtx, Conversation, ConversationId,
    ConversationKind, Counts, Cursor, Done, E2e, FileMeta, Flow, FlowCtx, FlowKind, FolderRef, Incoming, Limit, Member, Message, MessageId, Origin,
    Page, Participant, ParticipantId, PluginStore, Prompt, Quote, Reaction, Reactions, Reader, Role, Secret, Uploaded, doc_text, text_doc,
};

/// id канала.
pub const ID: &str = "demo";
/// Учётка демо-канала — владелец демо-ящика.
pub const ACCOUNT: &str = crate::ME;
/// Секрет действующей сессии.
pub const SESSION: &[u8] = b"demo-channel-session-1";
/// Вход: телефон (любой), код и пароль двухэтапной проверки.
pub const CODE: &str = "12345";
pub const PASSWORD: &[u8] = b"demo-2fa";
/// Начало часов демо-сервера (Unix, мс): 2026-01-15 09:00 UTC.
pub const CLOCK: i64 = 1_768_467_600_000;
/// Шаг часов на событие.
const TICK: i64 = 60_000;
/// Сообщений беседы в первом догоне.
const LAST: usize = 50;

const ME_ID: &str = "u-me";
const ANNA: &str = "u-anna";
const BORIS: &str = "u-boris";
const BOT: &str = "u-bot";
/// Личная беседа с Анной.
pub const DIRECT: &str = "d-anna";
/// Группа.
pub const GROUP: &str = "g-team";
/// Канал-рассылка: только чтение.
pub const NEWS: &str = "b-news";

/// Ссылка демо-канала на сообщение: `https://chat.example.test/<беседа>/<сообщение>`.
const LINK: &str = "https://chat.example.test/";

/// Всё, что умеет договор.
pub fn all_features() -> ChannelFeatures {
    ChannelFeatures {
        edit: Limit::Within(48 * 3600),
        delete_for_all: Limit::Always,
        reactions: Reactions::Any,
        read_receipts: true,
        typing: true,
        reply_to: true,
        quote_fragment: true,
        mentions: true,
        forward_native: true,
        scheduled_send: true,
        server_drafts: true,
        server_mute: true,
        server_pin: true,
        server_search: true,
        server_unread: true,
        folders: true,
        threads: true,
        e2e: E2e::Optional,
        max_attachment: Some(50 * 1024 * 1024),
        max_text: Some(4096),
    }
}

struct Conv {
    info: Conversation,
    messages: Vec<Message>,
    members: Vec<Member>,
}

#[derive(Default)]
struct State {
    clock: i64,
    /// Номер последнего события журнала — курсор.
    seq: u64,
    log: Vec<(u64, ChannelEvent)>,
    convs: Vec<Conv>,
    people: Vec<Participant>,
    folders: Vec<ChannelFolder>,
    files: HashMap<Vec<u8>, Vec<u8>>,
    /// `client_id` → сообщение: повтор отправки без дубля.
    sent: HashMap<u64, MessageId>,
    next: u64,
    subscribers: Vec<(u64, Incoming)>,
    /// Подключений всего; сессии с номером не больше `dropped` — без связи.
    sessions: u64,
    dropped: u64,
    /// Следующее действие — «подождите».
    wait: bool,
    features: ChannelFeatures,
}

impl State {
    fn tick(&mut self) -> i64 {
        self.clock += TICK;
        self.clock
    }

    fn id(&mut self) -> MessageId {
        self.next += 1;
        MessageId(format!("m{}", self.next))
    }

    /// Событие — в журнал (курсор) и всем подключённым.
    fn emit(&mut self, ev: ChannelEvent) {
        self.seq += 1;
        self.log.push((self.seq, ev.clone()));
        self.subscribers.retain(|(_, tx)| tx.send(ev.clone()).is_ok());
    }

    fn conv(&mut self, id: &ConversationId) -> Result<&mut Conv, ChannelError> {
        self.convs.iter_mut().find(|c| c.info.id == *id).ok_or(ChannelError::NotFound)
    }

    fn file(&mut self, name: &str, bytes: Vec<u8>) -> AttachmentSource {
        let src = AttachmentSource(format!("demo-file:{}:{name}", self.files.len() + 1).into_bytes());
        self.files.insert(src.0.clone(), bytes);
        src
    }

    fn message(&mut self, author: &str, text: &str) -> Message {
        let sent = self.tick();
        let id = self.id();
        Message { id, author: ParticipantId::new(author), sent, body: Body::Ir(text_doc(text)), mine: author == ME_ID, ..Message::default() }
    }

    fn me(&self) -> Participant {
        self.people.iter().find(|p| p.id.0 == ME_ID).cloned().unwrap_or_default()
    }
}

/// Сервер демо-канала в памяти: общий для плагина, его сессий и сценария.
#[derive(Clone)]
pub struct DemoServer(Arc<Mutex<State>>);

impl Default for DemoServer {
    fn default() -> Self {
        Self::new()
    }
}

fn person(id: &str, name: &str, email: Option<&str>, bot: bool) -> Participant {
    Participant {
        id: ParticipantId::new(id),
        name: name.into(),
        handle: Some(id.trim_start_matches("u-").into()),
        phone: None,
        emails: email.map(|e| vec![e.to_owned()]).unwrap_or_default(),
        avatar: None,
        bot,
    }
}

fn members(ids: &[(&str, Role)]) -> Vec<Member> {
    ids.iter().map(|(id, role)| Member { id: ParticipantId::new(*id), role: *role }).collect()
}

impl DemoServer {
    /// Сервер с демо-беседами: личная с Анной (файл и картинка), группа (реакции, ответ, цитата, упоминание),
    /// рассылка только для чтения, папка «Работа».
    pub fn new() -> Self {
        let mut s = State { clock: CLOCK, features: all_features(), ..State::default() };
        s.people = vec![
            person(ME_ID, "Я (демо)", Some(ACCOUNT), false),
            person(ANNA, "Анна Смирнова", Some("anna@example.test"), false),
            person(BORIS, "Борис Петров", Some("b.petrov@corp.example"), false),
            person(BOT, "Бот сборки", None, true),
        ];
        let report = "Отчёт за неделю: всё по плану.\n".repeat(120).into_bytes();
        let image: Vec<u8> = (0..4096u32).map(|i| (i % 253) as u8).collect();
        let report_src = s.file("report.txt", report.clone());
        let image_src = s.file("photo.png", image.clone());

        let mut direct = Vec::new();
        direct.push(s.message(ANNA, "Привет! Посмотришь отчёт?"));
        let mut m = s.message(ANNA, "Вот файл и фото со стенда.");
        m.attachments = vec![
            Attachment {
                name: "report.txt".into(),
                mime: "text/plain".into(),
                size: Some(report.len() as u64),
                kind: AttachmentKind::File,
                source: report_src,
                thumb: None,
            },
            Attachment {
                name: "photo.png".into(),
                mime: "image/png".into(),
                size: Some(image.len() as u64),
                kind: AttachmentKind::Image { w: 64, h: 64 },
                source: image_src,
                thumb: None,
            },
        ];
        direct.push(m);
        direct.push(s.message(ME_ID, "Да, вечером посмотрю."));
        direct.push(s.message(ANNA, "Спасибо!"));

        let mut group = Vec::new();
        group.push(s.message(BORIS, "Коллеги, сборка 0.1 готова к проверке."));
        let first = group[0].id.clone();
        let mut m = s.message(BOT, "Сборка №128: тесты зелёные.");
        m.reactions = vec![Reaction { key: "👍".into(), count: 2, mine: true, who: vec![ParticipantId::new(ME_ID), ParticipantId::new(BORIS)] }];
        group.push(m);
        let mut m = s.message(ANNA, "@Я посмотри, пожалуйста, раздел рисков.");
        m.mentions = vec![ParticipantId::new(ME_ID)];
        m.reply_to = Some(first.clone());
        m.quotes = vec![Quote { text: "сборка 0.1 готова".into(), source: Some(first) }];
        group.push(m);
        let mut m = s.message(BORIS, "Переслал заметки со встречи.");
        m.forwarded = Some(Origin { name: "Анна Смирнова".into(), author: Some(ParticipantId::new(ANNA)), sent: Some(CLOCK) });
        group.push(m);
        group.push(s.message(ME_ID, "Принято, беру риски на себя."));

        let news = vec![s.message(BORIS, "Обновление 0.1.0-beta.7 вышло."), s.message(BORIS, "Плановые работы в субботу.")];

        let conv = |id: &str, kind, title: &str, role, messages: Vec<Message>, ms: Vec<Member>| Conv {
            info: Conversation {
                id: ConversationId::new(id),
                kind,
                title: title.into(),
                my_role: role,
                members: Some(ms.len() as u32),
                unread: Some(Counts { unread: 1, mentions: u32::from(id == GROUP) }),
                ..Conversation::default()
            },
            messages,
            members: ms,
        };
        s.convs = vec![
            conv(DIRECT, ConversationKind::Direct, "Анна Смирнова", Role::Member, direct, members(&[(ME_ID, Role::Member), (ANNA, Role::Member)])),
            conv(
                GROUP,
                ConversationKind::Group,
                "Команда проекта",
                Role::Admin,
                group,
                members(&[(ME_ID, Role::Admin), (ANNA, Role::Member), (BORIS, Role::Owner), (BOT, Role::Member)]),
            ),
            conv(
                NEWS,
                ConversationKind::Broadcast,
                "Новости corp.example",
                Role::ReadOnly,
                news,
                members(&[(BORIS, Role::Owner), (ME_ID, Role::ReadOnly)]),
            ),
        ];
        s.folders = vec![ChannelFolder {
            id: FolderRef::new("f-work"),
            name: "Работа".into(),
            parent: None,
            conversations: vec![ConversationId::new(GROUP), ConversationId::new(NEWS)],
        }];
        Self(Arc::new(Mutex::new(s)))
    }

    /// Сервер, который умеет только `features` (проверка «Нет у сервера — нет на экране»: канал без реакций, правки…).
    pub fn with_features(features: ChannelFeatures) -> Self {
        let s = Self::new();
        s.lock().features = features;
        s
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // Отравленный замок — паника в другом потоке теста; данные сервера остаются годными.
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Беседы сервера (для демо и снимков).
    pub fn conversations(&self) -> Vec<Conversation> {
        self.lock().convs.iter().map(|c| c.info.clone()).collect()
    }

    /// Сообщения беседы, старые первыми.
    pub fn messages(&self, conv: &str) -> Vec<Message> {
        self.lock().convs.iter().find(|c| c.info.id.0 == conv).map(|c| c.messages.clone()).unwrap_or_default()
    }
}

impl Script for DemoServer {
    fn steps(&self) -> Vec<Step> {
        vec![
            Step::EditByOther,
            Step::ReactByOther,
            Step::ReadByOther,
            Step::TypingByOther,
            Step::DeleteWithoutConversation,
            Step::Gap,
            Step::Wait,
            Step::LinkDrop,
        ]
    }

    fn play(&self, step: Step) {
        let mut s = self.lock();
        let group = ConversationId::new(GROUP);
        let direct = ConversationId::new(DIRECT);
        match step {
            Step::EditByOther => {
                let edited = s.tick();
                let Ok(c) = s.conv(&group) else { return };
                let Some(m) = c.messages.iter_mut().rev().find(|m| !m.mine) else { return };
                m.body = Body::Ir(text_doc(&format!("{} (исправлено)", doc_body(&m.body))));
                m.edited = Some(edited);
                let m = m.clone();
                s.emit(ChannelEvent::Messages { conversation: group, messages: vec![m], live: true });
            }
            Step::ReactByOther => {
                let Ok(c) = s.conv(&group) else { return };
                let Some(m) = c.messages.last_mut() else { return };
                set_reaction(m, "🎉", ANNA, true);
                let (message, reactions) = (m.id.clone(), m.reactions.clone());
                s.emit(ChannelEvent::Reactions { conversation: group, message, reactions });
            }
            Step::ReadByOther => {
                let Some(up_to) = s.conv(&direct).ok().and_then(|c| c.messages.last().map(|m| m.id.clone())) else { return };
                s.emit(ChannelEvent::Read { conversation: direct, who: Reader::Them(ParticipantId::new(ANNA)), up_to });
            }
            Step::TypingByOther => {
                let ev = ChannelEvent::Typing { conversation: direct, who: ParticipantId::new(ANNA), on: true };
                s.subscribers.retain(|(_, tx)| tx.send(ev.clone()).is_ok());
            }
            Step::DeleteWithoutConversation => {
                let Ok(c) = s.conv(&direct) else { return };
                let Some(i) = c.messages.iter().position(|m| !m.mine && m.attachments.is_empty()) else { return };
                let id = c.messages.remove(i).id;
                s.emit(ChannelEvent::Deleted { conversation: None, ids: vec![id], for_all: true });
            }
            Step::Gap => {
                // Сервер «потерял» три сообщения: их нет в потоке, есть в истории.
                for text in ["Пропущенное 1", "Пропущенное 2", "Пропущенное 3"] {
                    let m = s.message(BORIS, text);
                    if let Ok(c) = s.conv(&group) {
                        c.messages.push(m);
                    }
                }
                s.emit(ChannelEvent::Gap { conversation: Some(group) });
            }
            Step::Wait => s.wait = true,
            Step::LinkDrop => {
                s.dropped = s.sessions;
                let ev = ChannelEvent::State(ChannelState::Offline { retry_in: 0 });
                for (_, tx) in std::mem::take(&mut s.subscribers) {
                    let _ = tx.send(ev.clone());
                }
            }
        }
    }
}

fn doc_body(b: &Body) -> String {
    match b {
        Body::Ir(d) => doc_text(d),
        other => other.plain(),
    }
}

fn set_reaction(m: &mut Message, key: &str, who: &str, on: bool) {
    let who = ParticipantId::new(who);
    let mine = who.0 == ME_ID;
    match m.reactions.iter_mut().find(|r| r.key == key) {
        Some(r) => {
            let had = r.who.contains(&who);
            if on && !had {
                r.who.push(who);
                r.count += 1;
            } else if !on && had {
                r.who.retain(|p| *p != who);
                r.count = r.count.saturating_sub(1);
            }
            if mine {
                r.mine = on;
            }
        }
        None if on => m.reactions.push(Reaction { key: key.into(), count: 1, mine, who: vec![who] }),
        None => {}
    }
    m.reactions.retain(|r| r.count > 0);
}

/// Плагин демо-канала.
#[derive(Clone, Default)]
pub struct DemoChannel {
    pub server: DemoServer,
}

impl DemoChannel {
    pub fn new(server: DemoServer) -> Self {
        Self { server }
    }
}

impl ChannelPlugin for DemoChannel {
    fn id(&self) -> &'static str {
        ID
    }

    fn features(&self) -> ChannelFeatures {
        self.server.lock().features.clone()
    }

    fn parse_link(&self, url: &str) -> Option<Address> {
        let rest = url.strip_prefix(LINK)?;
        let (conv, msg) = rest.split_once('/').map_or((rest, None), |(c, m)| (c, Some(m)));
        let known = self.server.lock().convs.iter().any(|c| c.info.id.0 == conv);
        known.then(|| Address {
            channel: ChannelId::new(ID),
            account: AccountId::new(ACCOUNT),
            conversation: ConversationId::new(conv),
            message: msg.filter(|m| !m.is_empty()).map(MessageId::new),
        })
    }

    fn sign_in<'a>(&'a self, _ctx: FlowCtx) -> BoxFuture<'a, Result<Box<dyn Flow + 'a>, ChannelError>> {
        Box::pin(async { Ok(Box::new(SignIn { step: 0 }) as Box<dyn Flow>) })
    }

    fn connect<'a>(&'a self, ctx: ConnectCtx) -> BoxFuture<'a, Result<Box<dyn ChannelSession>, ChannelError>> {
        Box::pin(async move {
            if ctx.secret.expose() != SESSION || ctx.account.0 != ACCOUNT {
                return Err(ChannelError::SignedOut("unknown session".into()));
            }
            let mut s = self.server.lock();
            s.sessions += 1;
            let generation = s.sessions;
            s.subscribers.push((generation, ctx.incoming.clone()));
            drop(s);
            // Своё состояние канала — в базе профиля (`PluginStore`), не файлом.
            ctx.store.put("state/connections", generation.to_string().as_bytes())?;
            Ok(Box::new(DemoSession { server: self.server.clone(), incoming: ctx.incoming, generation, _store: ctx.store })
                as Box<dyn ChannelSession>)
        })
    }
}

/// Вход: телефон → код → пароль двухэтапной проверки → сессия.
struct SignIn {
    step: u8,
}

impl Flow for SignIn {
    fn step<'a>(&'a mut self, answer: Option<Answer>) -> BoxFuture<'a, Result<Prompt, ChannelError>> {
        Box::pin(async move {
            let prompt = match (self.step, answer) {
                (0, None) => Prompt::Phone,
                (1, Some(Answer::Text(phone))) if !phone.trim().is_empty() => Prompt::Code { via: CodeVia::App, length: Some(CODE.len() as u8) },
                (2, Some(Answer::Resend)) => return Ok(Prompt::Code { via: CodeVia::Sms, length: Some(CODE.len() as u8) }),
                (2, Some(Answer::Text(code))) if code.trim() == CODE => Prompt::Password { hint: Some("демо".into()) },
                (2, Some(_)) => return Err(ChannelError::Denied("wrong code".into())),
                (3, Some(Answer::Secret(p))) if p.expose() == PASSWORD => {
                    Prompt::Done { account: AccountId::new(ACCOUNT), name: "Я (демо)".into(), secret: Secret::new(SESSION.to_vec()) }
                }
                (3, Some(_)) => return Err(ChannelError::Denied("wrong password".into())),
                (_, _) => return Err(ChannelError::Protocol("unexpected answer".into())),
            };
            self.step += 1;
            Ok(prompt)
        })
    }
}

/// Сверка устройств: символы → «совпадает».
struct Verify {
    asked: bool,
}

impl Flow for Verify {
    fn step<'a>(&'a mut self, answer: Option<Answer>) -> BoxFuture<'a, Result<Prompt, ChannelError>> {
        Box::pin(async move {
            match (self.asked, answer) {
                (false, None) => {
                    self.asked = true;
                    Ok(Prompt::Compare {
                        emoji: vec![("🐶".into(), "Собака".into()), ("🔑".into(), "Ключ".into()), ("🌲".into(), "Дерево".into())]
                    })
                }
                (true, Some(Answer::Confirm(true))) => Ok(Prompt::Finished),
                (true, Some(Answer::Confirm(false))) => Err(ChannelError::Denied("devices do not match".into())),
                _ => Err(ChannelError::Protocol("unexpected answer".into())),
            }
        })
    }
}

struct DemoSession {
    server: DemoServer,
    incoming: Incoming,
    generation: u64,
    _store: Rc<dyn PluginStore>,
}

impl DemoSession {
    /// Сервер, если связь есть.
    fn online(&self) -> Result<MutexGuard<'_, State>, ChannelError> {
        let s = self.server.lock();
        if self.generation <= s.dropped {
            return Err(ChannelError::Network("link dropped".into()));
        }
        Ok(s)
    }

    fn send(&self, ev: ChannelEvent) {
        let _ = self.incoming.send(ev);
    }
}

fn page(conv: &Conv, before: Option<&MessageId>, limit: u32) -> Page {
    let end = before.and_then(|b| conv.messages.iter().position(|m| m.id == *b)).unwrap_or(conv.messages.len());
    let start = end.saturating_sub(limit as usize);
    Page { messages: conv.messages[start..end].iter().map(|m| (conv.info.id.clone(), m.clone())).collect(), more: start > 0 }
}

fn act(s: &mut State, action: Action) -> Result<Done, ChannelError> {
    if std::mem::take(&mut s.wait) {
        return Err(ChannelError::Wait { secs: 1 });
    }
    if !s.features.allows(&action) {
        return Err(ChannelError::Unsupported);
    }
    match action {
        Action::Send(o) => {
            let c = s.conv(&o.conversation)?;
            if !c.info.my_role.can_write() {
                return Err(ChannelError::Denied("read-only conversation".into()));
            }
            if let Some(id) = s.sent.get(&o.client_id).cloned() {
                let at = s.convs.iter().flat_map(|c| &c.messages).find(|m| m.id == id).map_or(s.clock, |m| m.sent);
                return Ok(Done::Sent { client_id: o.client_id, id, at });
            }
            let mut m = s.message(ME_ID, "");
            m.body = Body::Ir(o.body);
            if let Some(at) = o.at {
                m.sent = at;
            }
            m.attachments = o
                .attachments
                .into_iter()
                .map(|u| Attachment { name: u.name, mime: u.mime, size: Some(u.size), kind: u.kind, source: u.source, thumb: None })
                .collect();
            m.reply_to = o.reply_to;
            m.quotes = o.quotes;
            m.mentions = o.mentions;
            s.sent.insert(o.client_id, m.id.clone());
            let (id, at) = (m.id.clone(), m.sent);
            s.conv(&o.conversation)?.messages.push(m.clone());
            s.emit(ChannelEvent::Messages { conversation: o.conversation, messages: vec![m], live: true });
            Ok(Done::Sent { client_id: o.client_id, id, at })
        }
        Action::Edit { conversation, message, body, mentions } => {
            let at = s.tick();
            let edit = s.features.edit;
            let c = s.conv(&conversation)?;
            let m = c.messages.iter_mut().find(|m| m.id == message).ok_or(ChannelError::NotFound)?;
            if !m.mine {
                return Err(ChannelError::Denied("not my message".into()));
            }
            if !edit.open(at - m.sent) {
                return Err(ChannelError::Denied("edit time expired".into()));
            }
            m.body = Body::Ir(body);
            m.mentions = mentions;
            m.edited = Some(at);
            let m = m.clone();
            s.emit(ChannelEvent::Messages { conversation, messages: vec![m], live: true });
            Ok(Done::Ok)
        }
        Action::Delete { conversation, ids, for_all } => {
            let c = s.conv(&conversation)?;
            let direct = c.info.kind == ConversationKind::Direct;
            if for_all && c.messages.iter().any(|m| ids.contains(&m.id) && !m.mine) && c.info.my_role != Role::Admin && c.info.my_role != Role::Owner
            {
                return Err(ChannelError::Denied("not my message".into()));
            }
            c.messages.retain(|m| !ids.contains(&m.id));
            // Как Telegram: в личных номер уникален на учётку, беседы в событии нет.
            let conversation = (!direct).then_some(conversation);
            s.emit(ChannelEvent::Deleted { conversation, ids, for_all });
            Ok(Done::Ok)
        }
        Action::React { conversation, message, key, on } => {
            let c = s.conv(&conversation)?;
            let m = c.messages.iter_mut().find(|m| m.id == message).ok_or(ChannelError::NotFound)?;
            set_reaction(m, &key, ME_ID, on);
            let reactions = m.reactions.clone();
            s.emit(ChannelEvent::Reactions { conversation, message, reactions });
            Ok(Done::Ok)
        }
        Action::MarkRead { conversation, up_to } => {
            s.conv(&conversation)?.info.last_read = Some(up_to.clone());
            s.emit(ChannelEvent::Read { conversation, who: Reader::Me, up_to });
            Ok(Done::Ok)
        }
        Action::Typing { conversation, .. } => s.conv(&conversation).map(|_| Done::Ok),
        Action::Forward { from, ids, to } => {
            let source: Vec<Message> = s.conv(&from)?.messages.iter().filter(|m| ids.contains(&m.id)).cloned().collect();
            s.conv(&to)?;
            let mut out = Vec::new();
            for m in source {
                let mut f = s.message(ME_ID, "");
                let name = s.people.iter().find(|p| p.id == m.author).map_or_else(|| m.author.0.clone(), |p| p.name.clone());
                f.body = m.body;
                f.attachments = m.attachments;
                f.forwarded = Some(Origin { name, author: Some(m.author), sent: Some(m.sent) });
                out.push(f);
            }
            s.conv(&to)?.messages.extend(out.iter().cloned());
            s.emit(ChannelEvent::Messages { conversation: to, messages: out, live: true });
            Ok(Done::Ok)
        }
        Action::Mute { conversation, until } => {
            let c = s.conv(&conversation)?;
            c.info.muted_until = until;
            let info = c.info.clone();
            s.emit(ChannelEvent::Conversations(vec![info]));
            Ok(Done::Ok)
        }
        Action::Pin { conversation, on } => {
            let c = s.conv(&conversation)?;
            c.info.pinned = on;
            let info = c.info.clone();
            s.emit(ChannelEvent::Conversations(vec![info]));
            Ok(Done::Ok)
        }
        Action::SaveDraft { conversation, .. } => s.conv(&conversation).map(|_| Done::Ok),
        Action::Start { with } => {
            let names: Vec<String> = with.iter().filter_map(|w| s.people.iter().find(|p| p.id == *w).map(|p| p.name.clone())).collect();
            if names.len() != with.len() || with.is_empty() {
                return Err(ChannelError::NotFound);
            }
            s.next += 1;
            let id = ConversationId(format!("d-new-{}", s.next));
            let kind = if with.len() == 1 { ConversationKind::Direct } else { ConversationKind::Group };
            let mut ms = vec![Member { id: ParticipantId::new(ME_ID), role: Role::Owner }];
            ms.extend(with.into_iter().map(|id| Member { id, role: Role::Member }));
            let info = Conversation {
                id: id.clone(),
                kind,
                title: names.join(", "),
                my_role: Role::Owner,
                members: Some(ms.len() as u32),
                ..Conversation::default()
            };
            s.convs.push(Conv { info: info.clone(), messages: Vec::new(), members: ms });
            s.emit(ChannelEvent::Conversations(vec![info]));
            Ok(Done::Conversation(id))
        }
        Action::Members { conversation } => Ok(Done::Members(s.conv(&conversation)?.members.clone())),
        Action::Resolve { link } => {
            let rest = link.strip_prefix(LINK).ok_or(ChannelError::NotFound)?;
            let (conv, msg) = rest.split_once('/').map_or((rest, None), |(c, m)| (c, Some(m)));
            s.conv(&ConversationId::new(conv))?;
            Ok(Done::Address(Address {
                channel: ChannelId::new(ID),
                account: AccountId::new(ACCOUNT),
                conversation: ConversationId::new(conv),
                message: msg.map(MessageId::new),
            }))
        }
    }
}

impl ChannelSession for DemoSession {
    fn me(&self) -> Participant {
        self.server.lock().me()
    }

    fn features(&self) -> ChannelFeatures {
        self.server.lock().features.clone()
    }

    fn catch_up<'a>(&'a self, cursor: Option<Cursor>) -> BoxFuture<'a, Result<(), ChannelError>> {
        Box::pin(async move {
            let s = self.online()?;
            let since = cursor.and_then(|c| std::str::from_utf8(&c.0).ok().and_then(|t| t.parse::<u64>().ok()));
            let mut events = Vec::new();
            match since {
                None => {
                    events.push(ChannelEvent::People(s.people.clone()));
                    events.push(ChannelEvent::Conversations(s.convs.iter().map(|c| c.info.clone()).collect()));
                    for c in &s.convs {
                        let from = c.messages.len().saturating_sub(LAST);
                        events.push(ChannelEvent::Messages { conversation: c.info.id.clone(), messages: c.messages[from..].to_vec(), live: false });
                        events.push(ChannelEvent::Members { conversation: c.info.id.clone(), changed: c.members.clone(), left: Vec::new() });
                    }
                    events.push(ChannelEvent::Folders(s.folders.clone()));
                }
                Some(seq) => events.extend(s.log.iter().filter(|(n, _)| *n > seq).map(|(_, ev)| ev.clone())),
            }
            events.push(ChannelEvent::Cursor(Cursor(s.seq.to_string().into_bytes())));
            drop(s);
            for ev in events {
                self.send(ev);
            }
            Ok(())
        })
    }

    fn history<'a>(&'a self, conv: &'a ConversationId, before: Option<&'a MessageId>, limit: u32) -> BoxFuture<'a, Result<Page, ChannelError>> {
        Box::pin(async move {
            let mut s = self.online()?;
            Ok(page(s.conv(conv)?, before, limit))
        })
    }

    fn search<'a>(&'a self, query: &'a str, conv: Option<&'a ConversationId>, limit: u32) -> BoxFuture<'a, Result<Page, ChannelError>> {
        Box::pin(async move {
            let s = self.online()?;
            let q = query.to_lowercase();
            let messages: Vec<(ConversationId, Message)> = s
                .convs
                .iter()
                .filter(|c| conv.is_none_or(|id| c.info.id == *id))
                .flat_map(|c| c.messages.iter().map(move |m| (c.info.id.clone(), m)))
                .filter(|(_, m)| doc_body(&m.body).to_lowercase().contains(&q))
                .map(|(c, m)| (c, m.clone()))
                .collect();
            let more = messages.len() > limit as usize;
            Ok(Page { messages: messages.into_iter().take(limit as usize).collect(), more })
        })
    }

    fn act<'a>(&'a self, action: Action) -> BoxFuture<'a, Result<Done, ChannelError>> {
        Box::pin(async move {
            let mut s = self.online()?;
            act(&mut s, action)
        })
    }

    fn download<'a>(&'a self, src: &'a AttachmentSource, out: &'a mut dyn ByteSink) -> BoxFuture<'a, Result<(), ChannelError>> {
        Box::pin(async move {
            let bytes = self.online()?.files.get(&src.0).cloned().ok_or(ChannelError::NotFound)?;
            for chunk in bytes.chunks(64 * 1024) {
                out.write(chunk)?;
            }
            Ok(())
        })
    }

    fn upload<'a>(&'a self, meta: &'a FileMeta, src: &'a mut dyn ByteSource) -> BoxFuture<'a, Result<Uploaded, ChannelError>> {
        Box::pin(async move {
            let max = self.server.lock().features.max_attachment.unwrap_or(u64::MAX);
            if src.len() > max {
                return Err(ChannelError::TooLarge { max });
            }
            let mut bytes = Vec::new();
            let mut buf = [0u8; 16 * 1024];
            loop {
                let n = src.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&buf[..n]);
            }
            let size = bytes.len() as u64;
            let kind = if meta.mime.starts_with("image/") { AttachmentKind::Image { w: 0, h: 0 } } else { AttachmentKind::File };
            let source = self.online()?.file(&meta.name, bytes);
            Ok(Uploaded { name: meta.name.clone(), mime: meta.mime.clone(), size, kind, source })
        })
    }

    fn flow<'a>(&'a self, kind: FlowKind) -> BoxFuture<'a, Result<Box<dyn Flow + 'a>, ChannelError>> {
        Box::pin(async move {
            match kind {
                FlowKind::Verify => Ok(Box::new(Verify { asked: false }) as Box<dyn Flow>),
                FlowKind::Recovery => Err(ChannelError::Unsupported),
            }
        })
    }

    fn sign_out<'a>(&'a self) -> BoxFuture<'a, Result<(), ChannelError>> {
        Box::pin(async move {
            let mut s = self.server.lock();
            let g = self.generation;
            s.subscribers.retain(|(n, _)| *n != g);
            Ok(())
        })
    }
}
