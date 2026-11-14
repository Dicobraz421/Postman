//! Адреса Telegram в общей модели: беседа — `u<id>` (личная), `g<id>` (группа), `c<id>` (супергруппа, канал);
//! сообщение — его номер; участник — id пользователя (канал, пишущий от себя, — `c<id>`); учётка — id пользователя.
//! Ключи доступа (`access_hash`) людей и каналов — [`PeerBook`]: в памяти и в базе профиля (`PluginStore`, ключи
//! `peer/<беседа>`), без них сервер не примет вызов.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use grammers_tl_types as tl;
use postman_core::channel::{ConversationId, MessageId, ParticipantId, PluginStore};

/// Собеседник Telegram.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Peer {
    User(i64),
    Chat(i64),
    Channel(i64),
}

impl Peer {
    pub fn from_tl(p: &tl::enums::Peer) -> Self {
        match p {
            tl::enums::Peer::User(u) => Peer::User(u.user_id),
            tl::enums::Peer::Chat(c) => Peer::Chat(c.chat_id),
            tl::enums::Peer::Channel(c) => Peer::Channel(c.channel_id),
        }
    }

    pub fn to_tl(self) -> tl::enums::Peer {
        match self {
            Peer::User(user_id) => tl::types::PeerUser { user_id }.into(),
            Peer::Chat(chat_id) => tl::types::PeerChat { chat_id }.into(),
            Peer::Channel(channel_id) => tl::types::PeerChannel { channel_id }.into(),
        }
    }

    pub fn conv(self) -> ConversationId {
        ConversationId(match self {
            Peer::User(id) => format!("u{id}"),
            Peer::Chat(id) => format!("g{id}"),
            Peer::Channel(id) => format!("c{id}"),
        })
    }

    pub fn parse(c: &ConversationId) -> Option<Self> {
        let s = c.as_str();
        let (kind, num) = s.split_at_checked(1)?;
        let id: i64 = num.parse().ok().filter(|v| *v > 0)?;
        match kind {
            "u" => Some(Peer::User(id)),
            "g" => Some(Peer::Chat(id)),
            "c" => Some(Peer::Channel(id)),
            _ => None,
        }
    }

    /// Автор сообщения: человек — его id, канал — `c<id>`.
    pub fn participant(self) -> ParticipantId {
        ParticipantId(match self {
            Peer::User(id) => id.to_string(),
            Peer::Chat(id) => format!("g{id}"),
            Peer::Channel(id) => format!("c{id}"),
        })
    }

    pub fn is_channel(self) -> bool {
        matches!(self, Peer::Channel(_))
    }
}

pub fn msg_id(id: i32) -> MessageId {
    MessageId(id.to_string())
}

pub fn parse_msg(m: &MessageId) -> Option<i32> {
    m.as_str().parse().ok().filter(|v| *v > 0)
}

pub fn user_of(p: &ParticipantId) -> Option<i64> {
    p.as_str().parse().ok().filter(|v| *v > 0)
}

/// Ключи доступа людей и каналов.
pub struct PeerBook {
    me: i64,
    hashes: RefCell<HashMap<Peer, i64>>,
    store: Rc<dyn PluginStore>,
}

fn key(p: Peer) -> String {
    format!("peer/{}", p.conv())
}

impl PeerBook {
    pub fn new(me: i64, store: Rc<dyn PluginStore>) -> Self {
        Self { me, hashes: RefCell::new(HashMap::new()), store }
    }

    pub fn me(&self) -> i64 {
        self.me
    }

    fn put(&self, p: Peer, hash: i64) {
        let old = self.hashes.borrow_mut().insert(p, hash);
        if old != Some(hash)
            && let Err(e) = self.store.put(&key(p), &hash.to_le_bytes())
        {
            tracing::warn!("telegram: peer key not stored: {e}");
        }
    }

    /// Ключ из памяти или базы.
    pub fn hash(&self, p: Peer) -> Option<i64> {
        if let Some(h) = self.hashes.borrow().get(&p) {
            return Some(*h);
        }
        let stored = self.store.get(&key(p)).ok().flatten()?;
        let h = i64::from_le_bytes(stored.get(..8)?.try_into().ok()?);
        self.hashes.borrow_mut().insert(p, h);
        Some(h)
    }

    pub fn learn_user(&self, u: &tl::enums::User) {
        if let tl::enums::User::User(u) = u
            && !u.min
            && let Some(h) = u.access_hash
        {
            self.put(Peer::User(u.id), h);
        }
    }

    pub fn learn_chat(&self, c: &tl::enums::Chat) {
        match c {
            tl::enums::Chat::Channel(ch) if !ch.min => {
                if let Some(h) = ch.access_hash {
                    self.put(Peer::Channel(ch.id), h);
                }
            }
            tl::enums::Chat::ChannelForbidden(ch) => self.put(Peer::Channel(ch.id), ch.access_hash),
            tl::enums::Chat::Chat(g) => {
                self.hashes.borrow_mut().insert(Peer::Chat(g.id), 0);
            }
            _ => {}
        }
    }

    pub fn learn(&self, users: &[tl::enums::User], chats: &[tl::enums::Chat]) {
        users.iter().for_each(|u| self.learn_user(u));
        chats.iter().for_each(|c| self.learn_chat(c));
    }

    /// Собеседник для вызова; нет ключа — `None` (сервер беседу не знает или мы её не видели).
    pub fn input_peer(&self, p: Peer) -> Option<tl::enums::InputPeer> {
        Some(match p {
            Peer::User(id) if id == self.me => tl::enums::InputPeer::PeerSelf,
            Peer::User(user_id) => tl::types::InputPeerUser { user_id, access_hash: self.hash(p)? }.into(),
            Peer::Chat(chat_id) => tl::types::InputPeerChat { chat_id }.into(),
            Peer::Channel(channel_id) => tl::types::InputPeerChannel { channel_id, access_hash: self.hash(p)? }.into(),
        })
    }

    pub fn input_user(&self, id: i64) -> Option<tl::enums::InputUser> {
        if id == self.me {
            return Some(tl::enums::InputUser::UserSelf);
        }
        Some(tl::types::InputUser { user_id: id, access_hash: self.hash(Peer::User(id))? }.into())
    }

    pub fn input_channel(&self, id: i64) -> Option<tl::enums::InputChannel> {
        Some(tl::types::InputChannel { channel_id: id, access_hash: self.hash(Peer::Channel(id))? }.into())
    }

    /// Каналы с известными ключами — для `getChannelDifference`.
    pub fn channels(&self) -> Vec<(i64, i64)> {
        self.hashes.borrow().iter().filter_map(|(p, h)| if let Peer::Channel(id) = p { Some((*id, *h)) } else { None }).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip() {
        for p in [Peer::User(5), Peer::Chat(7), Peer::Channel(1_234_567_890_123)] {
            assert_eq!(Peer::parse(&p.conv()), Some(p));
        }
        for bad in ["", "u", "x5", "u-1", "u0", "uabc", "no-such-conversation-in-conformance"] {
            assert_eq!(Peer::parse(&ConversationId::new(bad)), None, "{bad}");
        }
        assert_eq!(parse_msg(&msg_id(42)), Some(42));
        assert_eq!(parse_msg(&MessageId::new("~out1")), None);
        assert_eq!(Peer::Channel(3).participant().as_str(), "c3");
    }
}
