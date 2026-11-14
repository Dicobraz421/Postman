//! Беседы для чтения в общей модели (ADR 0012 п. 7): список, лента, сообщение, люди — одинаково для любого канала.
//! Чаты — из таблиц каналов ([`Chats`]), почта — из таблиц писем на лету (`super::mail::MailSource`), без записи.
//! Чтение базы — I/O: звать из фона, не из потока интерфейса.

use postman_cache::{Cache, Hash};

use super::{Address, AttachmentSource, ChannelError, ChannelStore, Conversation, Entry, Message, MessageId, Participant, ParticipantId};

type Result<T> = std::result::Result<T, ChannelError>;

/// Беседа в списке: адрес, беседа общей модели, время и текст последнего сообщения.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub address: Address,
    pub conversation: Conversation,
    /// Unix, мс; 0 — сообщений нет.
    pub last: i64,
    /// Начало последнего сообщения.
    pub preview: String,
    /// Автор последнего сообщения.
    pub author: Option<ParticipantId>,
}

/// Строка ленты.
#[allow(clippy::large_enum_variant)] // как `Entry`: строки ленты — почти все сообщения
#[derive(Debug, Clone, PartialEq)]
pub enum Line {
    /// Сообщение и загруженные вложения (источник → часть по хэшу).
    Message { message: Message, loaded: Vec<(AttachmentSource, Hash)> },
    /// «Сообщение удалено».
    Deleted { id: MessageId, author: ParticipantId, sent: i64 },
}

impl Line {
    pub fn id(&self) -> &MessageId {
        match self {
            Line::Message { message, .. } => &message.id,
            Line::Deleted { id, .. } => id,
        }
    }

    pub fn message(&self) -> Option<&Message> {
        match self {
            Line::Message { message, .. } => Some(message),
            Line::Deleted { .. } => None,
        }
    }
}

impl From<Entry> for Line {
    fn from(e: Entry) -> Self {
        match e {
            Entry::Message { message, loaded, .. } => Line::Message { message, loaded },
            Entry::Deleted { id, author, sent, .. } => Line::Deleted { id, author, sent },
        }
    }
}

/// Длина [`Summary::preview`], знаков.
pub const PREVIEW: usize = 160;

/// Начало текста для списка: одной строкой, не длиннее [`PREVIEW`] знаков.
pub fn preview(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(PREVIEW).collect()
}

/// Откуда читать беседы канала: список, лента, сообщение, люди беседы. Адрес — `postman://…` (ADR 0012 п. 4).
pub trait ConversationSource {
    /// Беседа по адресу; нет — `None`.
    fn conversation(&self, at: &Address) -> Result<Option<Summary>>;
    /// Лента: до `limit` последних строк раньше `before` (`None` — с конца), старые первыми.
    fn timeline(&self, at: &Address, before: Option<&MessageId>, limit: usize) -> Result<Vec<Line>>;
    /// Сообщение адреса (`at.message`); нет — `None`.
    fn message(&self, at: &Address) -> Result<Option<Line>>;
    /// Люди беседы (подсказка `@`, «Человек во всех каналах»).
    fn people(&self, at: &Address) -> Result<Vec<Participant>>;
}

/// Беседы чатов из таблиц каналов базы профиля.
#[derive(Clone)]
pub struct Chats {
    cache: Cache,
}

impl Chats {
    pub fn new(cache: Cache) -> Self {
        Self { cache }
    }

    fn store(&self, at: &Address) -> Result<Option<ChannelStore>> {
        ChannelStore::find(&self.cache, &at.channel, &at.account)
    }

    /// Беседы всех каналов: новые сверху, `limit` с `offset`.
    pub fn list(&self, offset: usize, limit: usize) -> Result<Vec<Summary>> {
        let stores = ChannelStore::all(&self.cache)?;
        let mut out = Vec::new();
        for c in ChannelStore::conversations(&self.cache, offset, limit)? {
            let Some(store) = stores.iter().find(|s| s.chan() == c.address.chan()) else { continue };
            out.push(summary(store, c)?);
        }
        Ok(out)
    }
}

fn summary(store: &ChannelStore, c: super::StoredConversation) -> Result<Summary> {
    let last = store.timeline(&c.conversation.id, None, 1)?.pop();
    let (preview, author) = match &last {
        Some(Entry::Message { message, .. }) => (self::preview(&message.body.plain()), Some(message.author.clone())),
        Some(Entry::Deleted { author, .. }) => (String::new(), Some(author.clone())),
        None => (String::new(), None),
    };
    Ok(Summary { address: c.address, conversation: c.conversation, last: c.last, preview, author })
}

impl ConversationSource for Chats {
    fn conversation(&self, at: &Address) -> Result<Option<Summary>> {
        let Some(store) = self.store(at)? else { return Ok(None) };
        store.conversation(&at.conversation)?.map(|c| summary(&store, c)).transpose()
    }

    fn timeline(&self, at: &Address, before: Option<&MessageId>, limit: usize) -> Result<Vec<Line>> {
        let Some(store) = self.store(at)? else { return Ok(Vec::new()) };
        Ok(store.timeline(&at.conversation, before, limit)?.into_iter().map(Line::from).collect())
    }

    fn message(&self, at: &Address) -> Result<Option<Line>> {
        let (Some(store), Some(id)) = (self.store(at)?, at.message.as_ref()) else { return Ok(None) };
        Ok(store.message(&at.conversation, id)?.map(Line::from))
    }

    fn people(&self, at: &Address) -> Result<Vec<Participant>> {
        let Some(store) = self.store(at)? else { return Ok(Vec::new()) };
        store
            .members(&at.conversation)?
            .into_iter()
            .map(|m| Ok(store.participant(&m.id)?.unwrap_or_else(|| Participant { name: m.id.0.clone(), id: m.id, ..Participant::default() })))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{AccountId, Body, ChannelEvent, ChannelId, ConversationId, Member, Role};

    /// Список, лента, сообщение и люди беседы чата — по адресу; чужой адрес — пусто.
    #[test]
    fn chats_read_by_address() {
        let cache = Cache::open_in_memory().unwrap();
        let store = ChannelStore::open(cache.clone(), &ChannelId::new("demo"), &AccountId::new("me@example.test"), "Я").unwrap();
        let c = ConversationId::new("g");
        let m = |id: &str, sent, text: &str| Message {
            id: MessageId::new(id),
            author: ParticipantId::new("u-anna"),
            sent,
            body: Body::Plain(text.into()),
            ..Message::default()
        };
        store
            .apply(vec![
                ChannelEvent::Messages {
                    conversation: c.clone(),
                    messages: vec![m("1", 10, "первое"), m("2", 20, "второе   сообщение")],
                    live: false,
                },
                ChannelEvent::Members {
                    conversation: c.clone(),
                    changed: vec![Member { id: ParticipantId::new("u-anna"), role: Role::Member }],
                    left: vec![],
                },
            ])
            .unwrap();
        let chats = Chats::new(cache);
        let at = Address::conversation(&ChannelId::new("demo"), &AccountId::new("me@example.test"), &c);
        let s = chats.conversation(&at).unwrap().unwrap();
        assert_eq!((s.last, s.preview.as_str()), (20, "второе сообщение"));
        assert_eq!(chats.list(0, 10).unwrap(), vec![s]);
        let lines = chats.timeline(&at, Some(&MessageId::new("2")), 10).unwrap();
        assert_eq!(lines.iter().map(|l| l.id().0.as_str()).collect::<Vec<_>>(), ["1"]);
        let one = Address { message: Some(MessageId::new("2")), ..at.clone() };
        assert!(chats.message(&one).unwrap().is_some());
        assert_eq!(chats.people(&at).unwrap()[0].name, "u-anna", "человек без карточки — по id");
        let other = Address::conversation(&ChannelId::new("telegram"), &AccountId::new("1"), &c);
        assert!(chats.conversation(&other).unwrap().is_none() && chats.timeline(&other, None, 5).unwrap().is_empty());
    }
}
