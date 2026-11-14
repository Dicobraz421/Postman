//! Подставной сервер Telegram (признак `testkit`): договор канала проверяется без сети (ADR 0012 п. 11). Сервер в
//! памяти разбирает запросы TL теми же типами слоя, что настоящий клиент, и отвечает байтами TL: личные беседы, группа,
//! супергруппа, канал в архиве, папка; сообщения с сущностями, ответом с цитатой и файлом; вход телефоном, кодом и
//! паролем (SRP), вход по QR; отправка (короткий ответ в личной беседе, полный — в группах и с файлом), правка,
//! удаление, реакции, «прочитано», файлы кусками, просроченная ссылка на файл. События «от других» — сценарием
//! ([`Script`]). Данные — вымышленные (`+999…`, `*_example`).

use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use grammers_crypto::two_factor_auth::calculate_2fa;
use grammers_tl_types::{self as tl, Deserializable, Identifiable, RemoteCall, Serializable};
use postman_core::backend::BoxFuture;
use postman_core::channel::Secret;
use postman_core::channel::conformance::{Script, Step};
use tokio::sync::mpsc;

use crate::ids::Peer;
use crate::session::empty_message;
use crate::vault::{Blob, DcKey, UpdState};
use crate::wire::{Dial, Feed, UpdateBatch, Wire, WireError};

/// Я — учётка подставного сервера.
pub const ME: i64 = 9_990_001;
pub const ANNA: i64 = 9_990_002;
pub const BORIS: i64 = 9_990_003;
pub const BOT: i64 = 9_990_004;
pub const ACCOUNT: &str = "9990001";
pub const PHONE: &str = "+999 12 345-67-80";
pub const CODE: &str = "24680";
pub const PASSWORD: &str = "пароль проверки";
pub const GROUP: i64 = 5001;
pub const TEAM: i64 = 7001;
pub const NEWS: i64 = 7002;
const HOME_DC: i32 = 2;
const SESSION_KEY: [u8; 256] = [0xab; 256];
const BASE: i32 = 1_763_000_000;
const SALT1: &[u8] = b"salt-one-example";
const SALT2: &[u8] = b"salt-two-example";
const SRP_B: [u8; 256] = [0x5a; 256];
const SECURE_RANDOM: [u8; 256] = [0x3c; 256];
/// Простое SRP Telegram — канал сверяет его со своим и не гоняет долгую проверку.
const P_HEX: &str = "c71caeb9c6b1c9048e6c522f70f13f73980d40238e3e21c14934d037563d930f48198a0aa7c14058229493d22530f4dbfa336f6e0ac925139543aed44cce7c3720fd51f69458705ac68cd4fe6b6b13abdc9746512969328454f18faf8c595f642477fe96bb2a941d5bcd1d4ac8cc49880708fa9b378e3c4f3a9060bee67cf9a4a4a695811051907e162753b56b0f6b410dba74d8a84b2a14b3144e0ef1284754fd17ed950d5965b4b9dd46582db1178d169c6bc465b0d6ff9ca3928fef5b9ae4e418fc15e83ebea0f87fa9ff5eed70050ded2849f47bf959d956850ce929851f0d8115f635b105ee2e4e15d04b2454bf6f4fadf034b10403119cd8e3b92fcc5b";

fn srp_p() -> Vec<u8> {
    (0..P_HEX.len() / 2).filter_map(|i| u8::from_str_radix(&P_HEX[i * 2..i * 2 + 2], 16).ok()).collect()
}

/// Сессия учётки подставного сервера — секрет для `Harness`.
pub fn session() -> Secret {
    blob(SESSION_KEY, ME).to_secret()
}

fn blob(key: [u8; 256], user: i64) -> Blob {
    Blob {
        home_dc: HOME_DC,
        user,
        dcs: vec![DcKey {
            id: HOME_DC,
            ipv4: SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 2), 443),
            ipv6: SocketAddrV6::new(Ipv6Addr::LOCALHOST, 443, 0, 0),
            key: Some(key),
        }],
    }
}

#[derive(Clone)]
struct Msg {
    id: i32,
    peer: Peer,
    /// Автор: человек; `None` — пост канала.
    from: Option<i64>,
    date: i32,
    edit: Option<i32>,
    text: String,
    entities: Vec<tl::enums::MessageEntity>,
    file: Option<i64>,
    reply_to: Option<(i32, Option<String>)>,
    /// Реакции: ключ → кто.
    reactions: Vec<(String, Vec<i64>)>,
}

#[derive(Clone)]
struct File {
    hash: i64,
    fref: Vec<u8>,
    mime: String,
    name: String,
    bytes: Vec<u8>,
    photo: bool,
}

struct Person {
    first: &'static str,
    last: &'static str,
    username: Option<&'static str>,
    phone: &'static str,
    hash: i64,
    contact: bool,
    bot: bool,
}

struct Conn {
    id: u64,
    tx: Option<mpsc::UnboundedSender<UpdateBatch>>,
    alive: bool,
}

struct Server {
    clock: i32,
    pts: i32,
    seq: i32,
    next_id: i32,
    channel_ids: HashMap<i64, i32>,
    messages: Vec<Msg>,
    files: HashMap<i64, File>,
    next_file: i64,
    parts: HashMap<i64, Vec<(i32, Vec<u8>)>>,
    keys: HashMap<[u8; 256], i64>,
    next_key: u8,
    conns: Vec<Conn>,
    next_conn: u64,
    /// Следующий вызов — `FLOOD_WAIT_30`.
    flood: bool,
    sent: HashMap<(Peer, i64), i32>,
    log: Vec<(i32, tl::enums::Update)>,
    qr_scanned: bool,
    pinned: HashSet<Peer>,
    muted: HashSet<Peer>,
}

/// Подставной сервер: `Dial` для канала и `Script` для проверок договора.
#[derive(Clone)]
pub struct FakeTelegram(Arc<Mutex<Server>>);

impl Default for FakeTelegram {
    fn default() -> Self {
        Self::new()
    }
}

fn person(id: i64) -> Option<Person> {
    Some(match id {
        ME => {
            Person {
                first: "Иван", last: "Тестов", username: Some("ivan_example"), phone: "99912345680", hash: 0x1000, contact: false, bot: false
            }
        }
        ANNA => {
            Person {
                first: "Анна", last: "Смирнова", username: Some("anna_example"), phone: "99912345681", hash: 0x1111, contact: true, bot: false
            }
        }
        BORIS => Person { first: "Борис", last: "", username: None, phone: "", hash: 0x2222, contact: false, bot: false },
        BOT => {
            Person { first: "Помощник", last: "", username: Some("helper_example_bot"), phone: "", hash: 0x3333, contact: false, bot: true }
        }
        _ => return None,
    })
}

fn user(id: i64, me: bool) -> tl::enums::User {
    let p = person(id).unwrap_or(Person { first: "?", last: "", username: None, phone: "", hash: 1, contact: false, bot: false });
    tl::types::User {
        is_self: me,
        contact: p.contact,
        mutual_contact: p.contact,
        deleted: false,
        bot: p.bot,
        bot_chat_history: false,
        bot_nochats: false,
        verified: false,
        restricted: false,
        min: false,
        bot_inline_geo: false,
        support: false,
        scam: false,
        apply_min_photo: false,
        fake: false,
        bot_attach_menu: false,
        premium: false,
        attach_menu_enabled: false,
        bot_can_edit: false,
        close_friend: false,
        stories_hidden: false,
        stories_unavailable: false,
        contact_require_premium: false,
        bot_business: false,
        bot_has_main_app: false,
        bot_forum_view: false,
        bot_forum_can_manage_topics: false,
        bot_can_manage_bots: false,
        bot_guestchat: false,
        bot_guard: false,
        bot_forum_sidebar: false,
        id,
        access_hash: Some(p.hash),
        first_name: Some(p.first.into()),
        last_name: (!p.last.is_empty()).then(|| p.last.into()),
        username: p.username.map(Into::into),
        phone: (!p.phone.is_empty()).then(|| p.phone.into()),
        photo: None,
        status: None,
        bot_info_version: p.bot.then_some(1),
        restriction_reason: None,
        bot_inline_placeholder: None,
        lang_code: None,
        emoji_status: None,
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        bot_active_users: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_community_id: None,
    }
    .into()
}

fn group() -> tl::enums::Chat {
    tl::types::Chat {
        creator: false,
        left: false,
        deactivated: false,
        call_active: false,
        call_not_empty: false,
        noforwards: false,
        id: GROUP,
        title: "Семья".into(),
        photo: tl::enums::ChatPhoto::Empty,
        participants_count: 3,
        date: BASE,
        version: 1,
        migrated_to: None,
        admin_rights: None,
        default_banned_rights: None,
    }
    .into()
}

fn channel(id: i64) -> tl::enums::Chat {
    let news = id == NEWS;
    tl::types::Channel {
        creator: false,
        left: false,
        broadcast: news,
        verified: false,
        megagroup: !news,
        restricted: false,
        signatures: false,
        min: false,
        scam: false,
        has_link: false,
        has_geo: false,
        slowmode_enabled: false,
        call_active: false,
        call_not_empty: false,
        fake: false,
        gigagroup: false,
        noforwards: false,
        join_to_send: false,
        join_request: false,
        forum: false,
        stories_hidden: false,
        stories_hidden_min: false,
        stories_unavailable: false,
        signature_profiles: false,
        autotranslation: false,
        broadcast_messages_allowed: false,
        monoforum: false,
        forum_tabs: false,
        in_news: false,
        forum_sidebar: false,
        id,
        access_hash: Some(id * 10),
        title: if news { "Новости".into() } else { "Команда".into() },
        username: Some(if news { "news_example".into() } else { "team_example".into() }),
        photo: tl::enums::ChatPhoto::Empty,
        date: BASE,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(if news { 120 } else { 4 }),
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        emoji_status: None,
        level: None,
        subscription_until_date: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_monoforum_id: None,
        linked_community_id: None,
        pinned_msg_id: None,
    }
    .into()
}

fn all_users() -> Vec<tl::enums::User> {
    [ME, ANNA, BORIS, BOT].iter().map(|&id| user(id, id == ME)).collect()
}

fn all_chats() -> Vec<tl::enums::Chat> {
    vec![group(), channel(TEAM), channel(NEWS)]
}

fn rpc(code: i32, name: &str) -> WireError {
    WireError::rpc(code, name)
}

fn bold(offset: i32, length: i32) -> tl::enums::MessageEntity {
    tl::types::MessageEntityBold { offset, length }.into()
}

impl FakeTelegram {
    pub fn new() -> Self {
        let mut s = Server {
            clock: BASE,
            pts: 10,
            seq: 1,
            next_id: 100,
            channel_ids: HashMap::new(),
            messages: Vec::new(),
            files: HashMap::new(),
            next_file: 50_000,
            parts: HashMap::new(),
            keys: HashMap::from([(SESSION_KEY, ME)]),
            next_key: 1,
            conns: Vec::new(),
            next_conn: 1,
            flood: false,
            sent: HashMap::new(),
            log: Vec::new(),
            qr_scanned: false,
            pinned: HashSet::new(),
            muted: HashSet::new(),
        };
        let anna = Peer::User(ANNA);
        let first = "Привет! Это первое сообщение";
        s.add(anna, Some(ANNA), first, vec![bold(13, 6)], None, None);
        s.add(anna, Some(ME), "Привет, Анна", vec![], None, None);
        let pdf = s.file("отчёт.pdf", "application/pdf", b"%PDF-1.4 example\n".repeat(200), false);
        s.add(anna, Some(ANNA), "Посмотри отчёт", vec![], Some(pdf), None);
        s.add(anna, Some(ANNA), "Как дела?", vec![], None, None);
        s.add(Peer::User(BOT), Some(BOT), "Я помощник", vec![], None, None);
        s.add(Peer::Chat(GROUP), Some(BORIS), "Семейный чат", vec![], None, None);
        s.add(Peer::Chat(GROUP), Some(ANNA), "Ужин в семь", vec![], None, None);
        let mention: tl::enums::MessageEntity = tl::types::MessageEntityMentionName { offset: 0, length: 4, user_id: ME }.into();
        let t1 = s.add(Peer::Channel(TEAM), Some(BORIS), "Иван, посмотри задачу", vec![mention], None, None);
        s.add(Peer::Channel(TEAM), Some(ANNA), "Уже смотрю", vec![], None, Some((t1, Some("посмотри задачу".into()))));
        s.add(Peer::Channel(NEWS), None, "Новость дня", vec![], None, None);
        s.pinned.insert(Peer::Channel(TEAM));
        Self(Arc::new(Mutex::new(s)))
    }

    fn lock(&self) -> MutexGuard<'_, Server> {
        match self.0.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }

    /// QR отсканирован на другом устройстве: следующий `exportLoginToken` — вход.
    pub fn scan_qr(&self) {
        self.lock().qr_scanned = true;
    }

    /// Сколько у сервера живых подключений с потоком обновлений.
    pub fn feeds(&self) -> usize {
        self.lock().conns.iter().filter(|c| c.alive && c.tx.is_some()).count()
    }

    /// Ссылка на файл устарела (Telegram меняет `file_reference`): загрузка по старой — `FILE_REFERENCE_EXPIRED`.
    pub fn expire_file_refs(&self) {
        for f in self.lock().files.values_mut() {
            f.fref.push(b'+');
        }
    }

    /// Собеседница пишет (сообщение — обновлением всем подключениям).
    pub fn anna_writes(&self, text: &str) -> i32 {
        let mut s = self.lock();
        let id = s.add(Peer::User(ANNA), Some(ANNA), text, vec![], None, None);
        let m = s.tl(id, Peer::User(ANNA));
        s.push_pts(|pts| tl::types::UpdateNewMessage { message: m.clone(), pts, pts_count: 1 }.into());
        id
    }

    /// Текст сообщения на сервере.
    pub fn text(&self, peer: Peer, id: i32) -> Option<String> {
        self.lock().messages.iter().find(|m| m.peer == peer && m.id == id).map(|m| m.text.clone())
    }
}

impl Server {
    fn tick(&mut self) -> i32 {
        self.clock += 60;
        self.clock
    }

    fn file(&mut self, name: &str, mime: &str, bytes: Vec<u8>, photo: bool) -> i64 {
        self.next_file += 1;
        let id = self.next_file;
        self.files.insert(id, File { hash: id ^ 0x5555, fref: b"ref".to_vec(), mime: mime.into(), name: name.into(), bytes, photo });
        id
    }

    fn add(
        &mut self,
        peer: Peer,
        from: Option<i64>,
        text: &str,
        entities: Vec<tl::enums::MessageEntity>,
        file: Option<i64>,
        reply_to: Option<(i32, Option<String>)>,
    ) -> i32 {
        let id = match peer {
            Peer::Channel(c) => {
                let n = self.channel_ids.entry(c).or_insert(0);
                *n += 1;
                *n
            }
            _ => {
                self.next_id += 1;
                self.next_id
            }
        };
        let date = self.tick();
        self.messages.push(Msg { id, peer, from, date, edit: None, text: text.into(), entities, file, reply_to, reactions: Vec::new() });
        id
    }

    fn find(&self, peer: Peer, id: i32) -> Option<&Msg> {
        self.messages.iter().find(|m| m.peer == peer && m.id == id)
    }

    fn media(&self, file: i64) -> Option<tl::enums::MessageMedia> {
        let f = self.files.get(&file)?;
        if f.photo {
            let photo = tl::types::Photo {
                has_stickers: false,
                id: file,
                access_hash: f.hash,
                file_reference: f.fref.clone(),
                date: BASE,
                sizes: vec![tl::types::PhotoSize { r#type: "y".into(), w: 100, h: 80, size: i32::try_from(f.bytes.len()).unwrap_or(0) }.into()],
                video_sizes: None,
                dc_id: HOME_DC,
            };
            return Some(
                tl::types::MessageMediaPhoto { spoiler: false, live_photo: false, photo: Some(photo.into()), ttl_seconds: None, video: None }.into(),
            );
        }
        let doc = tl::types::Document {
            id: file,
            access_hash: f.hash,
            file_reference: f.fref.clone(),
            date: BASE,
            mime_type: f.mime.clone(),
            size: f.bytes.len() as i64,
            thumbs: None,
            video_thumbs: None,
            dc_id: HOME_DC,
            attributes: vec![tl::types::DocumentAttributeFilename { file_name: f.name.clone() }.into()],
        };
        Some(
            tl::types::MessageMediaDocument {
                nopremium: false,
                spoiler: false,
                video: false,
                round: false,
                voice: false,
                document: Some(doc.into()),
                alt_documents: None,
                video_cover: None,
                video_timestamp: None,
                ttl_seconds: None,
            }
            .into(),
        )
    }

    fn reactions(&self, m: &Msg) -> Option<tl::enums::MessageReactions> {
        if m.reactions.is_empty() {
            return None;
        }
        let results = m
            .reactions
            .iter()
            .map(|(k, who)| {
                tl::types::ReactionCount {
                    chosen_order: who.contains(&ME).then_some(0),
                    reaction: tl::types::ReactionEmoji { emoticon: k.clone() }.into(),
                    count: i32::try_from(who.len()).unwrap_or(0),
                }
                .into()
            })
            .collect();
        let recent = m
            .reactions
            .iter()
            .flat_map(|(k, who)| {
                who.iter().map(move |w| {
                    tl::types::MessagePeerReaction {
                        big: false,
                        unread: false,
                        my: *w == ME,
                        peer_id: Peer::User(*w).to_tl(),
                        date: BASE,
                        reaction: tl::types::ReactionEmoji { emoticon: k.clone() }.into(),
                    }
                    .into()
                })
            })
            .collect();
        Some(
            tl::types::MessageReactions {
                min: false,
                can_see_list: true,
                reactions_as_tags: false,
                results,
                recent_reactions: Some(recent),
                top_reactors: None,
            }
            .into(),
        )
    }

    /// Сообщение как его видит учётка.
    fn tl(&self, id: i32, peer: Peer) -> tl::enums::Message {
        let Some(m) = self.find(peer, id) else { return tl::types::MessageEmpty { id, peer_id: Some(peer.to_tl()) }.into() };
        let out = m.from == Some(ME);
        let from_id = match (m.peer, m.from) {
            (Peer::User(_), _) | (_, None) => None,
            (_, Some(u)) => Some(Peer::User(u).to_tl()),
        };
        let reply_to = m.reply_to.as_ref().map(|(to, quote)| {
            tl::types::MessageReplyHeader {
                reply_to_scheduled: false,
                forum_topic: false,
                quote: quote.is_some(),
                reply_to_ephemeral: false,
                reply_to_msg_id: Some(*to),
                reply_to_peer_id: None,
                reply_from: None,
                reply_media: None,
                reply_to_top_id: None,
                quote_text: quote.clone(),
                quote_entities: None,
                quote_offset: None,
                todo_item_id: None,
                poll_option: None,
                quote_rich: None,
            }
            .into()
        });
        tl::types::Message {
            out,
            mentioned: m.entities.iter().any(|e| matches!(e, tl::enums::MessageEntity::MentionName(n) if n.user_id == ME)),
            id: m.id,
            from_id,
            peer_id: m.peer.to_tl(),
            reply_to,
            date: m.date,
            message: m.text.clone(),
            media: m.file.and_then(|f| self.media(f)),
            entities: (!m.entities.is_empty()).then(|| m.entities.clone()),
            edit_date: m.edit,
            reactions: self.reactions(m),
            post: m.from.is_none(),
            ..empty_message()
        }
        .into()
    }

    fn state(&self) -> UpdState {
        UpdState { pts: self.pts, qts: 0, date: self.clock, seq: self.seq, channels: Vec::new() }
    }

    /// Обновление с новым `pts` — в журнал (догон от курсора) и всем живым подключениям.
    fn push_pts(&mut self, make: impl Fn(i32) -> tl::enums::Update) {
        self.pts += 1;
        let u = make(self.pts);
        self.log.push((self.pts, u.clone()));
        self.push(vec![u]);
    }

    fn push(&mut self, updates: Vec<tl::enums::Update>) {
        let batch = UpdateBatch { updates, users: all_users(), chats: all_chats() };
        for c in self.conns.iter().filter(|c| c.alive) {
            if let Some(tx) = &c.tx {
                let _ = tx.send(batch.clone());
            }
        }
    }

    fn updates(&mut self, updates: Vec<tl::enums::Update>) -> tl::enums::Updates {
        self.seq += 1;
        tl::types::Updates { updates, users: all_users(), chats: all_chats(), date: self.clock, seq: self.seq }.into()
    }

    fn input_peer(p: &tl::enums::InputPeer) -> Result<Peer, WireError> {
        Ok(match p {
            tl::enums::InputPeer::PeerSelf => Peer::User(ME),
            tl::enums::InputPeer::User(u) if person(u.user_id).is_some_and(|p| p.hash == u.access_hash) => Peer::User(u.user_id),
            tl::enums::InputPeer::Chat(c) if c.chat_id == GROUP => Peer::Chat(GROUP),
            tl::enums::InputPeer::Channel(c) if [TEAM, NEWS].contains(&c.channel_id) && c.access_hash == c.channel_id * 10 => {
                Peer::Channel(c.channel_id)
            }
            _ => return Err(rpc(400, "PEER_ID_INVALID")),
        })
    }

    fn input_channel(c: &tl::enums::InputChannel) -> Result<Peer, WireError> {
        match c {
            tl::enums::InputChannel::Channel(c) if [TEAM, NEWS].contains(&c.channel_id) && c.access_hash == c.channel_id * 10 => {
                Ok(Peer::Channel(c.channel_id))
            }
            _ => Err(rpc(400, "CHANNEL_INVALID")),
        }
    }

    fn dialogs(&self, archive: bool) -> tl::enums::messages::Dialogs {
        let peers = [Peer::User(ANNA), Peer::User(BOT), Peer::Chat(GROUP), Peer::Channel(TEAM), Peer::Channel(NEWS)];
        let mut ds: Vec<(i32, tl::types::Dialog)> = Vec::new();
        let mut msgs = Vec::new();
        for p in peers {
            if (p == Peer::Channel(NEWS)) != archive {
                continue;
            }
            let Some(top) = self.messages.iter().filter(|m| m.peer == p).max_by_key(|m| (m.date, m.id)) else { continue };
            msgs.push(self.tl(top.id, p));
            let unread = self.messages.iter().filter(|m| m.peer == p && m.from != Some(ME)).count();
            ds.push((
                top.date,
                tl::types::Dialog {
                    pinned: self.pinned.contains(&p),
                    unread_mark: false,
                    view_forum_as_messages: false,
                    peer: p.to_tl(),
                    top_message: top.id,
                    read_inbox_max_id: 0,
                    read_outbox_max_id: 0,
                    unread_count: i32::try_from(unread).unwrap_or(0),
                    unread_mentions_count: i32::from(p == Peer::Channel(TEAM)),
                    unread_reactions_count: 0,
                    unread_poll_votes_count: 0,
                    notify_settings: tl::types::PeerNotifySettings {
                        show_previews: None,
                        silent: None,
                        mute_until: self.muted.contains(&p).then_some(i32::MAX),
                        ios_sound: None,
                        android_sound: None,
                        other_sound: None,
                        stories_muted: None,
                        stories_hide_sender: None,
                        stories_ios_sound: None,
                        stories_android_sound: None,
                        stories_other_sound: None,
                    }
                    .into(),
                    pts: p.is_channel().then_some(1),
                    draft: None,
                    folder_id: archive.then_some(1),
                    ttl_period: None,
                },
            ));
        }
        ds.sort_by_key(|(d, _)| std::cmp::Reverse(*d));
        tl::types::messages::Dialogs {
            dialogs: ds.into_iter().map(|(_, d)| d.into()).collect(),
            messages: msgs,
            chats: all_chats(),
            users: all_users(),
        }
        .into()
    }

    fn slice(&self, ms: Vec<tl::enums::Message>, count: usize) -> tl::enums::messages::Messages {
        tl::types::messages::MessagesSlice {
            inexact: false,
            count: i32::try_from(count).unwrap_or(0),
            next_rate: None,
            offset_id_offset: None,
            search_flood: None,
            messages: ms,
            topics: Vec::new(),
            chats: all_chats(),
            users: all_users(),
        }
        .into()
    }

    fn history(&self, peer: Peer, offset_id: i32, limit: i32) -> tl::enums::messages::Messages {
        let mut all: Vec<&Msg> = self.messages.iter().filter(|m| m.peer == peer).collect();
        all.sort_by_key(|m| std::cmp::Reverse(m.id));
        let total = all.len();
        let page: Vec<tl::enums::Message> = all
            .into_iter()
            .filter(|m| offset_id == 0 || m.id < offset_id)
            .take(usize::try_from(limit).unwrap_or(0))
            .map(|m| self.tl(m.id, m.peer))
            .collect();
        self.slice(page, total)
    }

    fn authorization(&self) -> tl::enums::auth::Authorization {
        tl::types::auth::Authorization {
            setup_password_required: false,
            otherwise_relogin_days: None,
            tmp_sessions: None,
            future_auth_token: None,
            user: user(ME, true),
        }
        .into()
    }

    fn password(&self) -> tl::enums::account::Password {
        let algo = || -> tl::enums::PasswordKdfAlgo {
            tl::types::PasswordKdfAlgoSha256Sha256Pbkdf2Hmacsha512iter100000Sha256ModPow {
                salt1: SALT1.to_vec(),
                salt2: SALT2.to_vec(),
                g: 3,
                p: srp_p(),
            }
            .into()
        };
        tl::types::account::Password {
            has_recovery: false,
            has_secure_values: false,
            has_password: true,
            current_algo: Some(algo()),
            srp_b: Some(SRP_B.to_vec()),
            srp_id: Some(77),
            hint: Some("подсказка".into()),
            email_unconfirmed_pattern: None,
            new_algo: algo(),
            new_secure_algo: tl::enums::SecurePasswordKdfAlgo::Unknown,
            secure_random: SECURE_RANDOM.to_vec(),
            pending_reset_date: None,
            login_email_pattern: None,
        }
        .into()
    }

    /// Новое своё сообщение: дубль `random_id` — `RANDOM_ID_DUPLICATE`, как у Telegram.
    fn send(
        &mut self,
        peer: Peer,
        random_id: i64,
        text: &str,
        entities: Vec<tl::enums::MessageEntity>,
        file: Option<i64>,
        reply: Option<(i32, Option<String>)>,
    ) -> Result<i32, WireError> {
        if self.sent.contains_key(&(peer, random_id)) {
            return Err(rpc(400, "RANDOM_ID_DUPLICATE"));
        }
        if peer == Peer::Channel(NEWS) {
            return Err(rpc(403, "CHAT_WRITE_FORBIDDEN"));
        }
        let id = self.add(peer, Some(ME), text, entities, file, reply);
        self.sent.insert((peer, random_id), id);
        Ok(id)
    }

    fn new_message_update(&mut self, peer: Peer, id: i32, random_id: i64) -> tl::enums::Updates {
        let m = self.tl(id, peer);
        self.pts += 1;
        let new: tl::enums::Update = if peer.is_channel() {
            tl::types::UpdateNewChannelMessage { message: m, pts: 1, pts_count: 1 }.into()
        } else {
            tl::types::UpdateNewMessage { message: m, pts: self.pts, pts_count: 1 }.into()
        };
        self.updates(vec![tl::types::UpdateMessageId { id, random_id }.into(), new])
    }

    fn take_file(&mut self, f: &tl::enums::InputFile, name: &str, mime: &str, photo: bool) -> Result<i64, WireError> {
        let (id, parts) = match f {
            tl::enums::InputFile::File(f) => (f.id, f.parts),
            tl::enums::InputFile::Big(f) => (f.id, f.parts),
            _ => return Err(rpc(400, "FILE_PARTS_INVALID")),
        };
        let mut got = self.parts.remove(&id).ok_or_else(|| rpc(400, "FILE_PARTS_INVALID"))?;
        got.sort_by_key(|p| p.0);
        if got.len() != usize::try_from(parts).unwrap_or(0) || got.iter().enumerate().any(|(i, p)| p.0 != i32::try_from(i).unwrap_or(-1)) {
            return Err(rpc(400, "FILE_PARTS_INVALID"));
        }
        let bytes: Vec<u8> = got.into_iter().flat_map(|p| p.1).collect();
        Ok(self.file(name, mime, bytes, photo))
    }

    fn reply(r: &Option<tl::enums::InputReplyTo>) -> Option<(i32, Option<String>)> {
        match r {
            Some(tl::enums::InputReplyTo::Message(m)) => Some((m.reply_to_msg_id, m.quote_text.clone())),
            _ => None,
        }
    }
}

/// Разбор запроса: функция по номеру конструктора.
fn parse<F: Deserializable + Identifiable>(body: &[u8]) -> Result<F, WireError> {
    F::from_boxed_bytes(body).map_err(|e| WireError::Protocol(format!("fake: bad request: {e}")))
}

fn ok<R: RemoteCall>(_req: &R, v: R::Return) -> Vec<u8>
where
    R::Return: Serializable,
{
    v.to_bytes()
}

struct FakeWire {
    server: FakeTelegram,
    conn: u64,
    key: [u8; 256],
}

impl FakeWire {
    fn user(&self, s: &Server) -> Option<i64> {
        s.keys.get(&self.key).copied()
    }

    fn handle(&self, body: &[u8]) -> Result<Vec<u8>, WireError> {
        use tl::functions as f;
        let id = body.get(..4).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes).ok_or_else(|| WireError::Protocol("empty".into()))?;
        let mut guard = self.server.lock();
        let s = &mut *guard;
        if !s.conns.iter().any(|c| c.id == self.conn && c.alive) {
            return Err(WireError::Network("connection reset".into()));
        }
        if s.flood {
            s.flood = false;
            return Err(WireError::Rpc { code: 420, name: "FLOOD_WAIT".into(), value: Some(30) });
        }
        // Вход — без авторизации.
        macro_rules! on {
            ($t:ty, |$r:ident| $body:expr) => {
                if id == <$t>::CONSTRUCTOR_ID {
                    let $r: $t = parse(body)?;
                    return Ok(ok(&$r, $body));
                }
            };
        }
        on!(f::auth::SendCode, |r| {
            if crate::signin::normalize_phone(&r.phone_number) != crate::signin::normalize_phone(PHONE) {
                return Err(rpc(400, "PHONE_NUMBER_INVALID"));
            }
            if r.api_id <= 0 || r.api_hash.is_empty() {
                return Err(rpc(400, "API_ID_INVALID"));
            }
            tl::types::auth::SentCode {
                r#type: tl::types::auth::SentCodeTypeApp { length: 5 }.into(),
                phone_code_hash: "hash-1".into(),
                next_type: None,
                timeout: None,
            }
            .into()
        });
        on!(f::auth::ResendCode, |_r| {
            tl::types::auth::SentCode {
                r#type: tl::types::auth::SentCodeTypeSms { length: 5 }.into(),
                phone_code_hash: "hash-1".into(),
                next_type: None,
                timeout: None,
            }
            .into()
        });
        if id == f::auth::SignIn::CONSTRUCTOR_ID {
            let r: f::auth::SignIn = parse(body)?;
            let right = r.phone_code.as_deref() == Some(CODE) && r.phone_code_hash == "hash-1";
            return Err(if right { rpc(401, "SESSION_PASSWORD_NEEDED") } else { rpc(400, "PHONE_CODE_INVALID") });
        }
        on!(f::account::GetPassword, |_r| s.password());
        on!(f::auth::CheckPassword, |r| {
            let tl::enums::InputCheckPasswordSrp::Srp(c) = &r.password else { return Err(rpc(400, "PASSWORD_HASH_INVALID")) };
            let (m1, g_a) = calculate_2fa(SALT1, SALT2, &srp_p(), &3, SRP_B.to_vec(), SECURE_RANDOM.to_vec(), PASSWORD);
            if c.srp_id != 77 || c.m1 != m1 || c.a != g_a {
                return Err(rpc(400, "PASSWORD_HASH_INVALID"));
            }
            s.keys.insert(self.key, ME);
            s.authorization()
        });
        on!(f::auth::ExportLoginToken, |r| {
            if r.api_id <= 0 {
                return Err(rpc(400, "API_ID_INVALID"));
            }
            if s.qr_scanned {
                s.keys.insert(self.key, ME);
                tl::types::auth::LoginTokenSuccess { authorization: s.authorization() }.into()
            } else {
                let expires =
                    i32::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) + 30)
                        .unwrap_or(i32::MAX);
                tl::types::auth::LoginToken { expires, token: b"qr-token-example".to_vec() }.into()
            }
        });
        let Some(_me) = self.user(s) else { return Err(rpc(401, "AUTH_KEY_UNREGISTERED")) };
        on!(f::updates::GetState, |_r| tl::types::updates::State { pts: s.pts, qts: 0, date: s.clock, seq: s.seq, unread_count: 0 }.into());
        on!(f::users::GetUsers, |r| r.id.iter().filter(|u| matches!(u, tl::enums::InputUser::UserSelf)).map(|_| user(ME, true)).collect());
        on!(f::auth::LogOut, |_r| {
            s.keys.remove(&self.key);
            tl::types::auth::LoggedOut { future_auth_token: None }.into()
        });
        on!(f::messages::GetDialogs, |r| s.dialogs(r.folder_id == Some(1)));
        on!(f::messages::GetDialogFilters, |_r| {
            let filter = tl::types::DialogFilter {
                contacts: true,
                non_contacts: false,
                groups: false,
                broadcasts: false,
                bots: false,
                exclude_muted: false,
                exclude_read: false,
                exclude_archived: false,
                title_noanimate: false,
                id: 2,
                title: tl::types::TextWithEntities { text: "Работа".into(), entities: Vec::new() }.into(),
                emoticon: None,
                color: None,
                pinned_peers: Vec::new(),
                include_peers: vec![tl::types::InputPeerChannel { channel_id: TEAM, access_hash: TEAM * 10 }.into()],
                exclude_peers: Vec::new(),
            };
            tl::types::messages::DialogFilters { tags_enabled: false, filters: vec![tl::enums::DialogFilter::Default, filter.into()] }.into()
        });
        on!(f::messages::GetHistory, |r| {
            let p = Server::input_peer(&r.peer)?;
            s.history(p, r.offset_id, r.limit)
        });
        on!(f::messages::Search, |r| {
            let p = Server::input_peer(&r.peer)?;
            let q = r.q.to_lowercase();
            let hits: Vec<tl::enums::Message> =
                s.messages.iter().filter(|m| m.peer == p && m.text.to_lowercase().contains(&q)).map(|m| s.tl(m.id, m.peer)).collect();
            let n = hits.len();
            s.slice(hits, n)
        });
        on!(f::messages::SearchGlobal, |r| {
            let q = r.q.to_lowercase();
            let hits: Vec<tl::enums::Message> =
                s.messages.iter().filter(|m| m.text.to_lowercase().contains(&q)).map(|m| s.tl(m.id, m.peer)).collect();
            let n = hits.len();
            s.slice(hits, n)
        });
        on!(f::messages::GetMessages, |r| {
            let ids: Vec<i32> = r.id.iter().filter_map(|m| if let tl::enums::InputMessage::Id(i) = m { Some(i.id) } else { None }).collect();
            let hits: Vec<tl::enums::Message> =
                s.messages.iter().filter(|m| !m.peer.is_channel() && ids.contains(&m.id)).map(|m| s.tl(m.id, m.peer)).collect();
            let n = hits.len();
            s.slice(hits, n)
        });
        on!(f::channels::GetMessages, |r| {
            let p = Server::input_channel(&r.channel)?;
            let ids: Vec<i32> = r.id.iter().filter_map(|m| if let tl::enums::InputMessage::Id(i) = m { Some(i.id) } else { None }).collect();
            let hits: Vec<tl::enums::Message> =
                s.messages.iter().filter(|m| m.peer == p && ids.contains(&m.id)).map(|m| s.tl(m.id, m.peer)).collect();
            let n = hits.len();
            s.slice(hits, n)
        });
        on!(f::messages::SendMessage, |r| {
            let p = Server::input_peer(&r.peer)?;
            let id = s.send(p, r.random_id, &r.message, r.entities.clone().unwrap_or_default(), None, Server::reply(&r.reply_to))?;
            if let Peer::User(_) = p {
                s.pts += 1;
                let date = s.clock;
                tl::types::UpdateShortSentMessage {
                    out: true,
                    id,
                    pts: s.pts,
                    pts_count: 1,
                    date,
                    media: None,
                    entities: r.entities.clone(),
                    ttl_period: None,
                }
                .into()
            } else {
                s.new_message_update(p, id, r.random_id)
            }
        });
        on!(f::messages::SendMedia, |r| {
            let p = Server::input_peer(&r.peer)?;
            let file = match &r.media {
                tl::enums::InputMedia::UploadedDocument(d) => {
                    let name = d
                        .attributes
                        .iter()
                        .find_map(|a| if let tl::enums::DocumentAttribute::Filename(f) = a { Some(f.file_name.clone()) } else { None })
                        .unwrap_or_default();
                    s.take_file(&d.file, &name, &d.mime_type, false)?
                }
                tl::enums::InputMedia::UploadedPhoto(ph) => s.take_file(&ph.file, "photo.jpg", "image/jpeg", true)?,
                _ => return Err(rpc(400, "MEDIA_INVALID")),
            };
            let id = s.send(p, r.random_id, &r.message, r.entities.clone().unwrap_or_default(), Some(file), Server::reply(&r.reply_to))?;
            s.new_message_update(p, id, r.random_id)
        });
        on!(f::messages::EditMessage, |r| {
            let p = Server::input_peer(&r.peer)?;
            let date = s.tick();
            let m = s.messages.iter_mut().find(|m| m.peer == p && m.id == r.id).ok_or_else(|| rpc(400, "MESSAGE_ID_INVALID"))?;
            if m.from != Some(ME) {
                return Err(rpc(403, "MESSAGE_AUTHOR_REQUIRED"));
            }
            m.text = r.message.clone().unwrap_or_default();
            m.entities = r.entities.clone().unwrap_or_default();
            m.edit = Some(date);
            let msg = s.tl(r.id, p);
            s.pts += 1;
            let u: tl::enums::Update = if p.is_channel() {
                tl::types::UpdateEditChannelMessage { message: msg, pts: 1, pts_count: 1 }.into()
            } else {
                tl::types::UpdateEditMessage { message: msg, pts: s.pts, pts_count: 1 }.into()
            };
            s.updates(vec![u])
        });
        on!(f::messages::DeleteMessages, |r| {
            s.messages.retain(|m| m.peer.is_channel() || !r.id.contains(&m.id) || (!r.revoke && m.from != Some(ME)));
            s.pts += 1;
            tl::types::messages::AffectedMessages { pts: s.pts, pts_count: 1 }.into()
        });
        on!(f::channels::DeleteMessages, |r| {
            let p = Server::input_channel(&r.channel)?;
            s.messages.retain(|m| m.peer != p || !r.id.contains(&m.id));
            tl::types::messages::AffectedMessages { pts: 2, pts_count: 1 }.into()
        });
        on!(f::messages::SendReaction, |r| {
            let p = Server::input_peer(&r.peer)?;
            let key = r
                .reaction
                .as_ref()
                .and_then(|v| v.first())
                .and_then(|x| if let tl::enums::Reaction::Emoji(e) = x { Some(e.emoticon.clone()) } else { None });
            let m = s.messages.iter_mut().find(|m| m.peer == p && m.id == r.msg_id).ok_or_else(|| rpc(400, "MESSAGE_ID_INVALID"))?;
            for (_, who) in &mut m.reactions {
                who.retain(|w| *w != ME);
            }
            m.reactions.retain(|(_, who)| !who.is_empty());
            if let Some(k) = key {
                match m.reactions.iter_mut().find(|(rk, _)| *rk == k) {
                    Some((_, who)) => who.push(ME),
                    None => m.reactions.push((k, vec![ME])),
                }
            }
            let m = m.clone();
            let reactions = s.reactions(&m).unwrap_or_else(|| {
                tl::types::MessageReactions {
                    min: false,
                    can_see_list: true,
                    reactions_as_tags: false,
                    results: Vec::new(),
                    recent_reactions: None,
                    top_reactors: None,
                }
                .into()
            });
            s.updates(vec![
                tl::types::UpdateMessageReactions { peer: p.to_tl(), msg_id: r.msg_id, top_msg_id: None, saved_peer_id: None, reactions }.into(),
            ])
        });
        on!(f::messages::ReadHistory, |r| {
            Server::input_peer(&r.peer)?;
            s.pts += 1;
            tl::types::messages::AffectedMessages { pts: s.pts, pts_count: 1 }.into()
        });
        on!(f::channels::ReadHistory, |r| {
            Server::input_channel(&r.channel)?;
            true
        });
        on!(f::messages::SetTyping, |r| {
            Server::input_peer(&r.peer)?;
            true
        });
        on!(f::messages::ForwardMessages, |r| {
            let from = Server::input_peer(&r.from_peer)?;
            let to = Server::input_peer(&r.to_peer)?;
            let mut ups = Vec::new();
            for (i, id) in r.id.iter().enumerate() {
                let m = s.find(from, *id).cloned().ok_or_else(|| rpc(400, "MESSAGE_ID_INVALID"))?;
                let rid = r.random_id.get(i).copied().unwrap_or(0);
                let new = s.send(to, rid, &m.text, m.entities.clone(), m.file, None)?;
                if let tl::enums::Updates::Updates(u) = s.new_message_update(to, new, rid) {
                    ups.extend(u.updates);
                }
            }
            s.updates(ups)
        });
        on!(f::account::UpdateNotifySettings, |r| {
            let tl::enums::InputNotifyPeer::Peer(p) = &r.peer else { return Err(rpc(400, "PEER_ID_INVALID")) };
            let p = Server::input_peer(&p.peer)?;
            let tl::enums::InputPeerNotifySettings::Settings(set) = &r.settings;
            if set.mute_until.unwrap_or(0) > 0 {
                s.muted.insert(p);
            } else {
                s.muted.remove(&p);
            }
            true
        });
        on!(f::messages::ToggleDialogPin, |r| {
            let tl::enums::InputDialogPeer::Peer(p) = &r.peer else { return Err(rpc(400, "PEER_ID_INVALID")) };
            let p = Server::input_peer(&p.peer)?;
            if r.pinned {
                s.pinned.insert(p);
            } else {
                s.pinned.remove(&p);
            }
            true
        });
        on!(f::messages::SaveDraft, |r| {
            Server::input_peer(&r.peer)?;
            true
        });
        on!(f::contacts::ResolveUsername, |r| {
            let peer = match r.username.to_ascii_lowercase().as_str() {
                "anna_example" => Peer::User(ANNA),
                "team_example" => Peer::Channel(TEAM),
                "news_example" => Peer::Channel(NEWS),
                _ => return Err(rpc(400, "USERNAME_NOT_OCCUPIED")),
            };
            tl::types::contacts::ResolvedPeer { peer: peer.to_tl(), chats: all_chats(), users: all_users() }.into()
        });
        on!(f::upload::SaveFilePart, |r| {
            s.parts.entry(r.file_id).or_default().push((r.file_part, r.bytes.clone()));
            true
        });
        on!(f::upload::SaveBigFilePart, |r| {
            s.parts.entry(r.file_id).or_default().push((r.file_part, r.bytes.clone()));
            true
        });
        on!(f::upload::GetFile, |r| {
            let (fid, fref) = match &r.location {
                tl::enums::InputFileLocation::InputDocumentFileLocation(l) => (l.id, l.file_reference.clone()),
                tl::enums::InputFileLocation::InputPhotoFileLocation(l) => (l.id, l.file_reference.clone()),
                _ => return Err(rpc(400, "LOCATION_INVALID")),
            };
            let file = s.files.get(&fid).ok_or_else(|| rpc(400, "FILE_ID_INVALID"))?;
            if file.fref != fref {
                return Err(rpc(400, "FILE_REFERENCE_EXPIRED"));
            }
            let start = usize::try_from(r.offset).unwrap_or(usize::MAX).min(file.bytes.len());
            let end = start.saturating_add(usize::try_from(r.limit).unwrap_or(0)).min(file.bytes.len());
            tl::types::upload::File { r#type: tl::enums::storage::FileType::FilePartial, mtime: 0, bytes: file.bytes[start..end].to_vec() }.into()
        });
        Err(rpc(400, &format!("FAKE_METHOD_UNKNOWN_{}", tl::name_for_id(id).replace('.', "_").to_uppercase())))
    }
}

impl Wire for FakeWire {
    fn invoke(&self, _dc: Option<i32>, body: Vec<u8>) -> BoxFuture<'_, Result<Vec<u8>, WireError>> {
        let r = self.handle(&body);
        Box::pin(async move {
            tokio::task::yield_now().await;
            r
        })
    }

    fn home_dc(&self) -> i32 {
        HOME_DC
    }

    fn set_home_dc(&self, _dc: i32) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    fn blob(&self, user: i64) -> Blob {
        blob(self.key, user)
    }

    fn take_changed(&self) -> bool {
        false
    }

    fn feed(&self, state: Option<UpdState>) -> BoxFuture<'_, Result<Box<dyn Feed>, WireError>> {
        let r = (|| {
            let mut s = self.server.lock();
            let conn = s.conns.iter_mut().find(|c| c.id == self.conn && c.alive).ok_or_else(|| WireError::Network("connection reset".into()))?;
            if conn.tx.is_some() {
                return Err(WireError::Protocol("feed taken".into()));
            }
            let (tx, rx) = mpsc::unbounded_channel();
            // Догон от курсора: пропущенное — первой пачкой (как `getDifference`).
            if let Some(st) = &state {
                let missed: Vec<tl::enums::Update> = s.log.iter().filter(|(pts, _)| *pts > st.pts).map(|(_, u)| u.clone()).collect();
                if !missed.is_empty() {
                    let _ = tx.send(UpdateBatch { updates: missed, users: all_users(), chats: all_chats() });
                }
            }
            if let Some(c) = s.conns.iter_mut().find(|c| c.id == self.conn) {
                c.tx = Some(tx);
            }
            let feed: Box<dyn Feed> = Box::new(FakeFeed { rx, server: self.server.clone() });
            Ok(feed)
        })();
        Box::pin(async move { r })
    }

    fn login_token(&self, wait: Duration) -> BoxFuture<'_, bool> {
        Box::pin(async move {
            let until = tokio::time::Instant::now() + wait;
            while tokio::time::Instant::now() < until {
                if self.server.lock().qr_scanned {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            false
        })
    }

    fn remember_channel(&self, _id: i64, _access_hash: i64) {}

    fn close(&self) {
        let mut s = self.server.lock();
        s.conns.retain(|c| c.id != self.conn);
    }
}

struct FakeFeed {
    rx: mpsc::UnboundedReceiver<UpdateBatch>,
    server: FakeTelegram,
}

impl Feed for FakeFeed {
    fn next(&mut self) -> BoxFuture<'_, Result<UpdateBatch, WireError>> {
        Box::pin(async move { self.rx.recv().await.ok_or_else(|| WireError::Network("connection reset".into())) })
    }

    fn state(&mut self) -> BoxFuture<'_, UpdState> {
        let s = self.server.lock().state();
        Box::pin(async move { s })
    }
}

impl Dial for FakeTelegram {
    fn dial(&self, blob: Option<Blob>) -> BoxFuture<'_, Result<Rc<dyn Wire>, WireError>> {
        let mut s = self.lock();
        let key = match blob.and_then(|b| b.dcs.into_iter().find(|d| d.id == b.home_dc).and_then(|d| d.key)) {
            Some(k) => k,
            None => {
                s.next_key = s.next_key.wrapping_add(1);
                [s.next_key; 256]
            }
        };
        s.next_conn += 1;
        let conn = s.next_conn;
        s.conns.push(Conn { id: conn, tx: None, alive: true });
        drop(s);
        let wire: Rc<dyn Wire> = Rc::new(FakeWire { server: self.clone(), conn, key });
        Box::pin(async move { Ok(wire) })
    }

    fn api(&self) -> Option<(i32, String)> {
        Some((12345, "fake-api-hash".into()))
    }

    fn srp_ok(&self, p: &[u8], g: i32) -> bool {
        crate::srp_known_prime(p) && g == 3
    }
}

impl Script for FakeTelegram {
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
        let anna = Peer::User(ANNA);
        let annas: Vec<i32> = s.messages.iter().filter(|m| m.peer == anna && m.from == Some(ANNA)).map(|m| m.id).collect();
        match step {
            Step::EditByOther => {
                let Some(&id) = annas.first() else { return };
                let date = s.tick();
                if let Some(m) = s.messages.iter_mut().find(|m| m.peer == anna && m.id == id) {
                    m.text = "Привет! Это исправленное сообщение".into();
                    m.entities.clear();
                    m.edit = Some(date);
                }
                let msg = s.tl(id, anna);
                s.push_pts(|pts| tl::types::UpdateEditMessage { message: msg.clone(), pts, pts_count: 1 }.into());
            }
            Step::ReactByOther => {
                let Some(&id) = annas.last() else { return };
                if let Some(m) = s.messages.iter_mut().find(|m| m.peer == anna && m.id == id) {
                    m.reactions.push(("🔥".into(), vec![ANNA]));
                }
                let Some(m) = s.find(anna, id).cloned() else { return };
                let Some(reactions) = s.reactions(&m) else { return };
                s.push(vec![
                    tl::types::UpdateMessageReactions { peer: anna.to_tl(), msg_id: id, top_msg_id: None, saved_peer_id: None, reactions }.into(),
                ]);
            }
            Step::ReadByOther => {
                let max_id = s.messages.iter().filter(|m| m.peer == anna).map(|m| m.id).max().unwrap_or(0);
                s.push_pts(|pts| tl::types::UpdateReadHistoryOutbox { peer: anna.to_tl(), max_id, pts, pts_count: 1 }.into());
            }
            Step::TypingByOther => {
                s.push(vec![
                    tl::types::UpdateUserTyping { user_id: ANNA, top_msg_id: None, action: tl::enums::SendMessageAction::SendMessageTypingAction }
                        .into(),
                ]);
            }
            Step::DeleteWithoutConversation => {
                let Some(&id) = annas.get(1).or(annas.first()) else { return };
                s.messages.retain(|m| !(m.peer == anna && m.id == id));
                s.push_pts(|pts| tl::types::UpdateDeleteMessages { messages: vec![id], pts, pts_count: 1 }.into());
            }
            Step::Gap => {
                s.add(Peer::Channel(TEAM), Some(BORIS), "Пропущенное сообщение", vec![], None, None);
                s.push(vec![tl::types::UpdateChannelTooLong { channel_id: TEAM, pts: None }.into()]);
            }
            Step::Wait => s.flood = true,
            Step::LinkDrop => {
                for c in &mut s.conns {
                    c.alive = false;
                    c.tx = None;
                }
                s.conns.clear();
            }
        }
    }
}
