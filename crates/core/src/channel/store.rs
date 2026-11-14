//! Хранение каналов в базе профиля (ADR 0012 п. 9): ядро переводит события канала в записи `postman_cache`
//! одной транзакцией на пачку — вместе с курсором. Значения — версионированные конверты модели (формат знает ядро,
//! байты хранит база), ключи и индексы — у базы.
//!
//! - Сообщение — по id сервера, идемпотентно: повтор, расшифровка позже, реакции — та же строка; новая правка —
//!   прежняя версия в истории правок.
//! - «Удалить у всех» и удалённое на сервере — строка без текста и вложений («Сообщение удалено» в ленте), история
//!   правок и части уходят; «удалить у меня» — строки нет совсем.
//! - Удаление без беседы — беседу находит база по id сообщения в учётке.
//! - «Печатает» в базу не пишется: оно уходит в [`Applied::typing`] (память потока).

use postman_cache::{Cache, ChanDownload, ChanKey, ConvKey, Hash, MsgDoc, MsgKey, MsgPut, Stored};
use serde::{Deserialize, Serialize};

use super::{
    AccountId, Address, AttachmentSource, ByteSink, ChannelError, ChannelEvent, ChannelFeatures, ChannelFolder, ChannelId, ChannelState,
    Conversation, ConversationId, Cursor, Member, Message, MessageId, MessageKind, Participant, ParticipantId, Reader, Role, Secret,
};

type Result<T> = std::result::Result<T, ChannelError>;

fn enc<T: Serialize>(v: &T) -> Result<Vec<u8>> {
    Ok(postman_cache::encode(v)?)
}

fn dec<T: for<'de> Deserialize<'de>>(b: &[u8]) -> Result<T> {
    Ok(postman_cache::decode(b)?)
}

/// Значение учётки канала.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AccountValue {
    name: String,
    features: ChannelFeatures,
    folders: Vec<ChannelFolder>,
    state: Option<ChannelState>,
    /// Кто я в канале (`ChannelSession::me`): свои сообщения, упоминания меня, автор исходящих в очереди.
    me: Option<Participant>,
}

/// Значение учётки до `me` (CH-02а).
#[derive(Serialize, Deserialize)]
struct AccountValueV1 {
    name: String,
    features: ChannelFeatures,
    folders: Vec<ChannelFolder>,
    state: Option<ChannelState>,
}

#[derive(Serialize, Deserialize)]
enum AccountEnvelope {
    V1(AccountValueV1),
    V2(AccountValue),
}

impl AccountEnvelope {
    fn value(self) -> AccountValue {
        match self {
            AccountEnvelope::V1(v) => AccountValue { name: v.name, features: v.features, folders: v.folders, state: v.state, me: None },
            AccountEnvelope::V2(v) => v,
        }
    }
}

/// Значение беседы: беседа сервера и кто прочитал до какого сообщения.
#[derive(Serialize, Deserialize)]
enum ConvEnvelope {
    V1 { conversation: Conversation, read_by: Vec<(ParticipantId, MessageId)> },
}

// Конверт живёт миг (запись и чтение строки): коробка ничего не сэкономит.
#[allow(clippy::large_enum_variant)]
#[derive(Serialize, Deserialize)]
enum MsgEnvelope {
    /// Сообщение до дополнений IR (`Marks::spoiler`, CH-02б): postcard не знает имён полей, прежние байты под новую
    /// модель не читаются — такая строка не разбирается ([`dec_msg`]) и перепишется, когда канал пришлёт сообщение
    /// снова.
    Retired,
    /// Удалено у всех: без текста и вложений.
    Deleted {
        author: ParticipantId,
        sent: i64,
    },
    V2(Message),
}

/// Конверт сообщения; строка прежней модели ([`MsgEnvelope::Retired`], вариант 0) — ошибка без разбора байтов.
fn dec_msg(b: &[u8]) -> Result<MsgEnvelope> {
    if b.first() == Some(&0) {
        return Err(ChannelError::Storage("message stored before the IR additions".into()));
    }
    dec(b)
}

#[derive(Serialize, Deserialize)]
enum OutboxEnvelope {
    V1(super::outbox::Queued),
}

#[derive(Serialize, Deserialize)]
enum PersonEnvelope {
    V1(Participant),
}

/// Беседа сервера и кто прочитал до какого сообщения.
type ConvParts = (Conversation, Vec<(ParticipantId, MessageId)>);

/// Беседа из базы: адрес, беседа сервера, кто прочитал и время последнего сообщения.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredConversation {
    pub key: ConvKey,
    pub address: Address,
    pub conversation: Conversation,
    pub read_by: Vec<(ParticipantId, MessageId)>,
    /// Unix, мс; 0 — сообщений нет.
    pub last: i64,
}

/// Строка ленты.
// Строки ленты — почти все сообщения: коробка дала бы лишнее выделение на каждую.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    Message {
        key: MsgKey,
        message: Message,
        loaded: Vec<(AttachmentSource, Hash)>,
    },
    /// «Сообщение удалено».
    Deleted {
        key: MsgKey,
        id: MessageId,
        author: ParticipantId,
        sent: i64,
    },
}

impl Entry {
    pub fn id(&self) -> &MessageId {
        match self {
            Entry::Message { message, .. } => &message.id,
            Entry::Deleted { id, .. } => id,
        }
    }

    pub fn message(&self) -> Option<&Message> {
        match self {
            Entry::Message { message, .. } => Some(message),
            Entry::Deleted { .. } => None,
        }
    }
}

/// Итог пачки: что сказать интерфейсу и что сделать потоку.
#[derive(Debug, Default)]
pub struct Applied {
    /// Беседы, что изменились (новые сообщения, правки, реакции, отметки), по порядку первой встречи.
    pub changed: Vec<ConversationId>,
    pub gone: Vec<ConversationId>,
    /// Новые сообщения сейчас, не свои, — для уведомлений.
    pub arrived: Vec<(ConversationId, MessageId)>,
    /// Дозапросить историю (`None` — догнать сервер заново).
    pub gaps: Vec<Option<ConversationId>>,
    pub typing: Vec<(ConversationId, ParticipantId, bool)>,
    /// Новый секрет сессии — в хранилище ОС.
    pub secret: Option<Secret>,
    pub features: Option<ChannelFeatures>,
    pub state: Option<ChannelState>,
    /// Курсор записан этой пачкой.
    pub cursor: Option<Cursor>,
    /// Сообщений записано (новых или изменённых).
    pub written: usize,
    /// Сообщения, чей текст записан или изменён, — в индекс поиска.
    pub indexed: Vec<MsgKey>,
    /// Сообщения, которых больше нет (или «удалено у всех»), — из индекса.
    pub unindexed: Vec<MsgKey>,
}

impl Applied {
    fn change(&mut self, id: &ConversationId) {
        if !self.changed.contains(id) {
            self.changed.push(id.clone());
        }
    }
}

pub(super) fn entry(doc: MsgDoc) -> Result<Entry> {
    let loaded = doc.loaded.into_iter().map(|(s, h)| (AttachmentSource(s), h)).collect();
    Ok(match dec_msg(&doc.value)? {
        MsgEnvelope::V2(message) => Entry::Message { key: doc.key, message, loaded },
        MsgEnvelope::Retired => return Err(ChannelError::Storage("retired message envelope".into())),
        MsgEnvelope::Deleted { author, sent } => Entry::Deleted { key: doc.key, id: super::MessageId(doc.remote), author, sent },
    })
}

/// Источники вложений и превью сообщения.
fn sources(m: &Message) -> Vec<Vec<u8>> {
    m.attachments.iter().flat_map(|a| std::iter::once(&a.source).chain(a.thumb.as_ref())).map(|s| s.0.clone()).collect()
}

/// Загрузка вложения в хранилище содержимого ([`ChannelStore::begin_download`]).
pub struct Download(ChanDownload);

impl ByteSink for Download {
    fn write(&mut self, chunk: &[u8]) -> Result<()> {
        Ok(self.0.write(chunk)?)
    }
}

/// Хранение одной учётки канала поверх базы профиля.
#[derive(Clone)]
pub struct ChannelStore {
    cache: Cache,
    key: ChanKey,
    channel: ChannelId,
    account: AccountId,
}

impl ChannelStore {
    /// Учётка канала в базе: есть — она (её курсор и возможности остаются), нет — новая с именем `name`.
    pub fn open(cache: Cache, channel: &ChannelId, account: &AccountId, name: &str) -> Result<Self> {
        let known = cache.chan_accounts()?.into_iter().find(|a| a.channel == channel.0 && a.account == account.0);
        let key = match known {
            Some(a) => a.key,
            None => {
                let b = cache.chan_batch()?;
                let value = AccountValue { name: name.to_owned(), ..AccountValue::default() };
                let key = b.add_account(&channel.0, &account.0, &enc(&AccountEnvelope::V2(value))?)?;
                b.commit()?;
                key
            }
        };
        Ok(Self { cache, key, channel: channel.clone(), account: account.clone() })
    }

    /// Учётка канала, если она уже есть в базе (чтение: новой строки не заводит).
    pub fn find(cache: &Cache, channel: &ChannelId, account: &AccountId) -> Result<Option<Self>> {
        let known = cache.chan_accounts()?.into_iter().find(|a| a.channel == channel.0 && a.account == account.0);
        Ok(known.map(|a| Self { cache: cache.clone(), key: a.key, channel: channel.clone(), account: account.clone() }))
    }

    /// Учётки каналов профиля.
    pub fn all(cache: &Cache) -> Result<Vec<Self>> {
        Ok(cache
            .chan_accounts()?
            .into_iter()
            .map(|a| Self { cache: cache.clone(), key: a.key, channel: ChannelId(a.channel), account: AccountId(a.account) })
            .collect())
    }

    pub fn key(&self) -> ChanKey {
        self.key
    }

    pub fn cache(&self) -> &Cache {
        &self.cache
    }

    pub fn channel(&self) -> &ChannelId {
        &self.channel
    }

    pub fn account(&self) -> &AccountId {
        &self.account
    }

    /// Учётка канала (маршрут запросов, события интерфейсу).
    pub fn chan(&self) -> super::ChanAccount {
        super::ChanAccount { channel: self.channel.clone(), account: self.account.clone() }
    }

    fn value(&self) -> Result<AccountValue> {
        let a = self.cache.chan_account(self.key)?.ok_or(ChannelError::NotFound)?;
        Ok(dec::<AccountEnvelope>(&a.value)?.value())
    }

    /// Курсор: всё до него записано.
    pub fn cursor(&self) -> Result<Option<Cursor>> {
        Ok(self.cache.chan_account(self.key)?.and_then(|a| a.cursor).map(Cursor))
    }

    /// Возможности по последнему `ChannelEvent::Features` (или сессии при подключении).
    pub fn features(&self) -> Result<ChannelFeatures> {
        Ok(self.value()?.features)
    }

    pub fn state(&self) -> Result<Option<ChannelState>> {
        Ok(self.value()?.state)
    }

    pub fn folders(&self) -> Result<Vec<ChannelFolder>> {
        Ok(self.value()?.folders)
    }

    pub fn name(&self) -> Result<String> {
        Ok(self.value()?.name)
    }

    /// Кто я в канале — по последнему подключению; не подключались — `None`.
    pub fn me(&self) -> Result<Option<Participant>> {
        Ok(self.value()?.me)
    }

    /// Запомнить, кто я (подключение): упоминания меня и автор исходящих — без связи.
    pub fn set_me(&self, me: &Participant) -> Result<()> {
        let mut v = self.value()?;
        if v.me.as_ref() == Some(me) {
            return Ok(());
        }
        v.me = Some(me.clone());
        let b = self.cache.chan_batch()?;
        b.set_account_value(self.key, &enc(&AccountEnvelope::V2(v))?)?;
        Ok(b.commit()?)
    }

    // --- очередь отправки (`chan_outbox`, ADR 0012 п. 11) ---

    /// Поставить исходящее в очередь: строка `chan_outbox` и сообщение «отправляется» в ленте
    /// ([`super::outbox::pending_id`]). Тот же `client_id` — замена (повтор после сбоя).
    pub fn queue(&self, q: &super::outbox::Queued, now: i64) -> Result<Applied> {
        let b = self.cache.chan_batch()?;
        b.put_outbox(self.key, q.outgoing.client_id, &enc(&OutboxEnvelope::V1(q.clone()))?)?;
        b.commit()?;
        let me = self.me()?.map(|p| p.id).unwrap_or_default();
        let pending = super::outbox::message(&q.outgoing, super::outbox::pending_id(q.outgoing.client_id), me, now);
        self.apply(vec![ChannelEvent::Messages { conversation: q.outgoing.conversation.clone(), messages: vec![pending], live: false }])
    }

    /// Очередь учётки, по номерам отправки.
    pub fn queued(&self) -> Result<Vec<super::outbox::Queued>> {
        self.cache
            .chan_outbox(self.key)?
            .into_iter()
            .map(|(_, v)| {
                let OutboxEnvelope::V1(q) = dec(&v)?;
                Ok(q)
            })
            .collect()
    }

    /// Убрать из очереди («Отменить»): строка и сообщение «отправляется». Не было — `None`.
    pub fn unqueue(&self, client_id: u64) -> Result<Option<(super::outbox::Queued, Applied)>> {
        let b = self.cache.chan_batch()?;
        let Some(v) = b.outbox(self.key, client_id)? else { return Ok(None) };
        let OutboxEnvelope::V1(q) = dec(&v)?;
        b.remove_outbox(self.key, client_id)?;
        b.commit()?;
        let gone = ChannelEvent::Deleted {
            conversation: Some(q.outgoing.conversation.clone()),
            ids: vec![super::outbox::pending_id(client_id)],
            for_all: false,
        };
        let applied = self.apply(vec![gone])?;
        Ok(Some((q, applied)))
    }

    /// Шаг исходящего в очереди: отказ сервера без повтора — `Failed`, строка остаётся до «Отменить» или повтора.
    pub fn set_step(&self, client_id: u64, step: super::outbox::Step) -> Result<()> {
        let b = self.cache.chan_batch()?;
        let Some(v) = b.outbox(self.key, client_id)? else { return Ok(()) };
        let OutboxEnvelope::V1(mut q) = dec(&v)?;
        q.step = step;
        b.put_outbox(self.key, client_id, &enc(&OutboxEnvelope::V1(q))?)?;
        Ok(b.commit()?)
    }

    /// Сервер принял исходящее (`Done::Sent`): «отправляется» уходит, сообщение с id сервера — в ленту (канал пришлёт
    /// его и сам — запись идемпотентна), строка очереди — последней: сбой между ними повторит отправку с тем же
    /// `client_id`, без дубля.
    pub fn sent(&self, client_id: u64, id: &MessageId, at: i64) -> Result<Applied> {
        let Some(v) = self.cache.chan_outbox(self.key)?.into_iter().find(|(c, _)| *c == client_id).map(|(_, v)| v) else {
            return Ok(Applied::default());
        };
        let OutboxEnvelope::V1(q) = dec(&v)?;
        let me = self.me()?.map(|p| p.id).unwrap_or_default();
        let conv = q.outgoing.conversation.clone();
        let real = super::outbox::message(&q.outgoing, id.clone(), me, at);
        let applied = self.apply(vec![
            ChannelEvent::Deleted { conversation: Some(conv.clone()), ids: vec![super::outbox::pending_id(client_id)], for_all: false },
            ChannelEvent::Messages { conversation: conv, messages: vec![real], live: false },
        ])?;
        let b = self.cache.chan_batch()?;
        b.remove_outbox(self.key, client_id)?;
        b.commit()?;
        Ok(applied)
    }

    /// Пачка событий канала — одной транзакцией, курсор — вместе с событиями до него.
    pub fn apply(&self, events: Vec<ChannelEvent>) -> Result<Applied> {
        let b = self.cache.chan_batch()?;
        let mut out = Applied::default();
        let mut account: Option<AccountValue> = None;
        let conv_of = |id: &ConversationId, out: &mut Applied| -> Result<ConvKey> {
            if let Some(k) = b.conv_key(self.key, &id.0)? {
                return Ok(k);
            }
            out.change(id);
            let value = ConvEnvelope::V1 { conversation: Conversation::unknown(id.clone()), read_by: Vec::new() };
            Ok(b.put_conv(self.key, &id.0, &enc(&value)?)?)
        };
        let read_conv = |k: ConvKey| -> Result<Option<ConvParts>> {
            Ok(match b.conv(k)? {
                Some(d) => {
                    let ConvEnvelope::V1 { conversation, read_by } = dec(&d.value)?;
                    Some((conversation, read_by))
                }
                None => None,
            })
        };
        for ev in events {
            match ev {
                ChannelEvent::Conversations(convs) => {
                    for c in convs {
                        let read_by = match b.conv_key(self.key, &c.id.0)? {
                            Some(k) => read_conv(k)?.map(|(_, r)| r).unwrap_or_default(),
                            None => Vec::new(),
                        };
                        out.change(&c.id);
                        let id = c.id.0.clone();
                        b.put_conv(self.key, &id, &enc(&ConvEnvelope::V1 { conversation: c, read_by })?)?;
                    }
                }
                ChannelEvent::ConversationGone(id) => {
                    if let Some(k) = b.conv_key(self.key, &id.0)? {
                        b.remove_conv(k)?;
                    }
                    out.changed.retain(|c| *c != id);
                    out.gone.push(id);
                }
                ChannelEvent::Messages { conversation, messages, live } => {
                    let k = conv_of(&conversation, &mut out)?;
                    for m in messages {
                        let value = enc(&MsgEnvelope::V2(m.clone()))?;
                        let srcs = sources(&m);
                        let put = MsgPut { remote: &m.id.0, sent: m.sent, edited: m.edited, value: &value, sources: &srcs };
                        let (mk, stored) = b.put_msg(k, &put)?;
                        if stored == Stored::Same {
                            continue;
                        }
                        out.written += 1;
                        if !out.indexed.contains(&mk) {
                            out.indexed.push(mk);
                        }
                        out.change(&conversation);
                        if stored == Stored::New && live && !m.mine && m.kind == MessageKind::Normal {
                            out.arrived.push((conversation.clone(), m.id.clone()));
                        }
                    }
                }
                ChannelEvent::Deleted { conversation, ids, for_all } => {
                    for id in ids {
                        let conv = match &conversation {
                            Some(c) => b.conv_key(self.key, &c.0)?,
                            None => b.conv_of_id(self.key, &id.0)?,
                        };
                        let Some(ck) = conv else { continue };
                        let Some(mk) = b.msg_key(ck, &id.0)? else { continue };
                        let Some(doc) = b.msg(mk)? else { continue };
                        let sent = doc.sent;
                        let old = entry(doc).ok();
                        b.remove_msg(mk)?;
                        out.indexed.retain(|k| *k != mk);
                        out.unindexed.push(mk);
                        if for_all {
                            let (author, sent) = match &old {
                                Some(Entry::Message { message, .. }) => (message.author.clone(), message.sent),
                                Some(Entry::Deleted { author, sent, .. }) => (author.clone(), *sent),
                                None => (ParticipantId::default(), sent),
                            };
                            let value = enc(&MsgEnvelope::Deleted { author, sent })?;
                            b.put_msg(ck, &MsgPut { remote: &id.0, sent, edited: None, value: &value, sources: &[] })?;
                        }
                        if let Some(d) = b.conv(ck)? {
                            out.change(&ConversationId(d.remote));
                        }
                    }
                }
                ChannelEvent::Reactions { conversation, message, reactions } => {
                    let Some(ck) = b.conv_key(self.key, &conversation.0)? else { continue };
                    let Some(mk) = b.msg_key(ck, &message.0)? else { continue };
                    let Some(doc) = b.msg(mk)? else { continue };
                    if let Ok(MsgEnvelope::V2(mut m)) = dec_msg(&doc.value) {
                        m.reactions = reactions;
                        b.set_msg_value(mk, &enc(&MsgEnvelope::V2(m))?)?;
                        out.change(&conversation);
                    }
                }
                ChannelEvent::Read { conversation, who, up_to } => {
                    let k = conv_of(&conversation, &mut out)?;
                    let Some((mut c, mut read_by)) = read_conv(k)? else { continue };
                    match who {
                        Reader::Me => c.last_read = Some(up_to),
                        Reader::Them(p) => match read_by.iter_mut().find(|(q, _)| *q == p) {
                            Some(r) => r.1 = up_to,
                            None => read_by.push((p, up_to)),
                        },
                    }
                    b.put_conv(self.key, &conversation.0, &enc(&ConvEnvelope::V1 { conversation: c, read_by })?)?;
                    out.change(&conversation);
                }
                ChannelEvent::Typing { conversation, who, on } => out.typing.push((conversation, who, on)),
                ChannelEvent::Members { conversation, changed, left } => {
                    let k = conv_of(&conversation, &mut out)?;
                    for m in changed {
                        b.put_member(k, &m.id.0, &enc(&m.role)?)?;
                    }
                    for p in left {
                        b.remove_member(k, &p.0)?;
                    }
                    out.change(&conversation);
                }
                ChannelEvent::People(people) => {
                    for p in people {
                        let id = p.id.0.clone();
                        b.put_participant(self.key, &id, &enc(&PersonEnvelope::V1(p))?)?;
                    }
                }
                ChannelEvent::Folders(folders) => account.get_or_insert(self.value()?).folders = folders,
                ChannelEvent::Gap { conversation } => out.gaps.push(conversation),
                ChannelEvent::Cursor(c) => {
                    b.set_cursor(self.key, &c.0)?;
                    out.cursor = Some(c);
                }
                ChannelEvent::Secret(s) => out.secret = Some(s),
                ChannelEvent::Features(f) => {
                    account.get_or_insert(self.value()?).features = f.clone();
                    out.features = Some(f);
                }
                ChannelEvent::State(s) => {
                    account.get_or_insert(self.value()?).state = Some(s.clone());
                    out.state = Some(s);
                }
            }
        }
        if let Some(v) = account {
            b.set_account_value(self.key, &enc(&AccountEnvelope::V2(v))?)?;
        }
        b.commit()?;
        Ok(out)
    }

    fn stored(&self, d: postman_cache::ConvDoc, address: Address) -> Result<StoredConversation> {
        let ConvEnvelope::V1 { conversation, read_by } = dec(&d.value)?;
        Ok(StoredConversation { key: d.key, address, conversation, read_by, last: d.last })
    }

    /// Беседа по ключу базы (поиск, пересборка индекса); чужая учётка — `None`.
    pub(super) fn conversation_by_key(&self, key: ConvKey) -> Result<Option<StoredConversation>> {
        let Some(d) = self.cache.chan_conv(key)?.filter(|d| d.account == self.key) else { return Ok(None) };
        let address = Address::conversation(&self.channel, &self.account, &ConversationId(d.remote.clone()));
        self.stored(d, address).map(Some)
    }

    /// Сообщение по ключу базы и ключ его беседы.
    pub(super) fn entry_by_key(&self, key: MsgKey) -> Result<Option<(ConvKey, Entry)>> {
        let Some(doc) = self.cache.chan_msg(key)? else { return Ok(None) };
        let conv = doc.conv;
        Ok(Some((conv, entry(doc)?)))
    }

    pub fn conversation(&self, id: &ConversationId) -> Result<Option<StoredConversation>> {
        let Some(k) = self.cache.chan_conv_key(self.key, &id.0)? else { return Ok(None) };
        let Some(d) = self.cache.chan_conv(k)? else { return Ok(None) };
        let address = Address::conversation(&self.channel, &self.account, id);
        self.stored(d, address).map(Some)
    }

    /// Беседы всех каналов профиля: новые сверху, `limit` с `offset`.
    pub fn conversations(cache: &Cache, offset: usize, limit: usize) -> Result<Vec<StoredConversation>> {
        let accounts = cache.chan_accounts()?;
        let mut out = Vec::new();
        for d in cache.chan_convs(offset, limit)? {
            let Some(a) = accounts.iter().find(|a| a.key == d.account) else { continue };
            let address = Address::conversation(&ChannelId(a.channel.clone()), &AccountId(a.account.clone()), &ConversationId(d.remote.clone()));
            let ConvEnvelope::V1 { conversation, read_by } = dec(&d.value)?;
            out.push(StoredConversation { key: d.key, address, conversation, read_by, last: d.last });
        }
        Ok(out)
    }

    /// Лента: до `limit` последних строк раньше `before` (`None` — с конца), старые первыми.
    pub fn timeline(&self, conv: &ConversationId, before: Option<&MessageId>, limit: usize) -> Result<Vec<Entry>> {
        let Some(k) = self.cache.chan_conv_key(self.key, &conv.0)? else { return Ok(Vec::new()) };
        let before = match before {
            Some(m) => match self.cache.chan_msg_key(k, &m.0)? {
                Some(mk) => Some(mk),
                None => return Ok(Vec::new()),
            },
            None => None,
        };
        // Строка, которую не разобрать (прежняя модель), — мимо ленты: она перепишется, когда канал пришлёт её снова.
        Ok(self
            .cache
            .chan_timeline(k, before, limit)?
            .into_iter()
            .filter_map(|d| entry(d).inspect_err(|e| tracing::warn!("channel timeline: {e}")).ok())
            .collect())
    }

    pub fn message(&self, conv: &ConversationId, id: &MessageId) -> Result<Option<Entry>> {
        let Some(k) = self.cache.chan_conv_key(self.key, &conv.0)? else { return Ok(None) };
        let Some(mk) = self.cache.chan_msg_key(k, &id.0)? else { return Ok(None) };
        self.cache.chan_msg(mk)?.map(entry).transpose()
    }

    /// История правок, старые первыми: время прежней правки (`None` — исходное) и версия.
    pub fn edits(&self, conv: &ConversationId, id: &MessageId) -> Result<Vec<(Option<i64>, Message)>> {
        let Some(Entry::Message { key, .. }) = self.message(conv, id)? else { return Ok(Vec::new()) };
        let mut out = Vec::new();
        for (at, v) in self.cache.chan_edits(key)? {
            if let Ok(MsgEnvelope::V2(m)) = dec_msg(&v) {
                out.push((at, m));
            }
        }
        Ok(out)
    }

    pub fn participant(&self, id: &ParticipantId) -> Result<Option<Participant>> {
        let Some(v) = self.cache.chan_participant(self.key, &id.0)? else { return Ok(None) };
        let PersonEnvelope::V1(p) = dec(&v)?;
        Ok(Some(p))
    }

    /// Все люди учётки.
    pub fn participants(&self) -> Result<Vec<Participant>> {
        self.cache
            .chan_participants(self.key)?
            .into_iter()
            .map(|(_, v)| {
                let PersonEnvelope::V1(p) = dec(&v)?;
                Ok(p)
            })
            .collect()
    }

    pub fn members(&self, conv: &ConversationId) -> Result<Vec<Member>> {
        let Some(k) = self.cache.chan_conv_key(self.key, &conv.0)? else { return Ok(Vec::new()) };
        self.cache.chan_members(k)?.into_iter().map(|(id, v)| Ok(Member { id: ParticipantId(id), role: dec::<Role>(&v)? })).collect()
    }

    /// Байты загруженного вложения `source` сообщения; не загружено — `None`.
    pub fn attachment(&self, conv: &ConversationId, id: &MessageId, source: &AttachmentSource) -> Result<Option<Vec<u8>>> {
        let Some(Entry::Message { loaded, .. }) = self.message(conv, id)? else { return Ok(None) };
        let Some((_, h)) = loaded.into_iter().find(|(s, _)| s == source) else { return Ok(None) };
        Ok(self.cache.chan_part(h)?)
    }

    /// Начать загрузку вложения: куски — в базу, не в память.
    pub fn begin_download(&self) -> Download {
        Download(self.cache.chan_download())
    }

    /// Конец загрузки: часть по хэшу — вложением `source` сообщения. Сообщения уже нет — `None`.
    pub fn finish_download(&self, d: Download, conv: &ConversationId, id: &MessageId, source: &AttachmentSource, mime: &str) -> Result<Option<Hash>> {
        let Some(Entry::Message { key, .. }) = self.message(conv, id)? else { return Ok(None) };
        Ok(d.0.finish(key, &source.0, mime)?)
    }

    /// «Выйти»: все строки учётки, её `local`-ключи, отметки бесед в базе (заглушить, закрепить), ссылки из её
    /// сообщений, связи её бесед и её людей с контактами.
    pub fn forget(&self) -> Result<()> {
        self.cache.forget_chan_account(self.key, &super::LocalStore::prefix(&self.channel, &self.account))?;
        let chan = self.chan();
        self.cache.remove_local_prefix(&super::smart::marks_prefix(&chan))?;
        let prefix = super::account_prefix(&chan);
        self.cache.xref_drop_prefix(&prefix)?;
        self.cache.conv_unlink_prefix(&prefix)?;
        self.cache.person_unlink_account(&self.channel.0, &self.account.0)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Attachment, Body, Reaction};
    use super::*;

    fn msg(id: &str, sent: i64, text: &str) -> Message {
        Message { id: MessageId::new(id), author: ParticipantId::new("p1"), sent, body: Body::Plain(text.into()), ..Message::default() }
    }

    fn conv(id: &str) -> Conversation {
        Conversation { id: ConversationId::new(id), title: id.into(), ..Conversation::default() }
    }

    fn store() -> ChannelStore {
        ChannelStore::open(Cache::open_in_memory().unwrap(), &ChannelId::new("demo"), &AccountId::new("me@example.test"), "Я").unwrap()
    }

    fn ev_msgs(c: &str, ms: Vec<Message>, live: bool) -> ChannelEvent {
        ChannelEvent::Messages { conversation: ConversationId::new(c), messages: ms, live }
    }

    /// Пачка и курсор — вместе; повтор той же пачки ничего не меняет; живое чужое — в уведомления.
    #[test]
    fn batch_is_idempotent_with_cursor() {
        let s = store();
        let events = vec![
            ChannelEvent::Conversations(vec![conv("c")]),
            ev_msgs("c", vec![msg("1", 10, "привет"), Message { mine: true, ..msg("2", 20, "моё") }], true),
            ChannelEvent::Cursor(Cursor(b"7".to_vec())),
        ];
        let a = s.apply(events.clone()).unwrap();
        assert_eq!(a.written, 2);
        assert_eq!(a.arrived, vec![(ConversationId::new("c"), MessageId::new("1"))]);
        assert_eq!(s.cursor().unwrap(), Some(Cursor(b"7".to_vec())));
        let again = s.apply(events).unwrap();
        assert_eq!(again.written, 0, "повтор после сбоя — без записей");
        assert!(again.arrived.is_empty());
        assert_eq!(s.timeline(&ConversationId::new("c"), None, 10).unwrap().len(), 2);
        let all = ChannelStore::conversations(s.cache(), 0, 10).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].address.to_string(), "postman://demo/me@example.test/c");
        assert_eq!(all[0].last, 20);
    }

    /// Правка — история; реакции и «прочитано» — без истории; удаление без беседы; «удалить у всех» — строка без текста.
    #[test]
    fn edits_reactions_reads_and_deletes() {
        let s = store();
        let c = ConversationId::new("c");
        s.apply(vec![ev_msgs("c", vec![msg("1", 10, "v1"), msg("2", 11, "x")], false)]).unwrap();
        s.apply(vec![ev_msgs("c", vec![Message { edited: Some(15), ..msg("1", 10, "v2") }], true)]).unwrap();
        let r = vec![Reaction { key: "👍".into(), count: 1, mine: false, who: vec![] }];
        s.apply(vec![
            ChannelEvent::Reactions { conversation: c.clone(), message: MessageId::new("1"), reactions: r.clone() },
            ChannelEvent::Read { conversation: c.clone(), who: Reader::Me, up_to: MessageId::new("1") },
            ChannelEvent::Read { conversation: c.clone(), who: Reader::Them(ParticipantId::new("p2")), up_to: MessageId::new("2") },
        ])
        .unwrap();
        let e = s.message(&c, &MessageId::new("1")).unwrap().unwrap();
        assert_eq!(e.message().unwrap().body, Body::Plain("v2".into()));
        assert_eq!(e.message().unwrap().reactions, r);
        let edits = s.edits(&c, &MessageId::new("1")).unwrap();
        assert_eq!(edits.len(), 1, "реакции историю не пишут");
        assert_eq!(edits[0].1.body, Body::Plain("v1".into()));
        let sc = s.conversation(&c).unwrap().unwrap();
        assert_eq!(sc.conversation.last_read, Some(MessageId::new("1")));
        assert_eq!(sc.read_by, vec![(ParticipantId::new("p2"), MessageId::new("2"))]);
        assert_eq!(sc.conversation.title, "c", "беседа, о которой сервер не рассказал, — по id");

        s.apply(vec![ChannelEvent::Deleted { conversation: None, ids: vec![MessageId::new("1")], for_all: true }]).unwrap();
        let e = s.message(&c, &MessageId::new("1")).unwrap().unwrap();
        assert!(matches!(e, Entry::Deleted { sent: 10, .. }), "{e:?}");
        assert!(s.edits(&c, &MessageId::new("1")).unwrap().is_empty(), "история правок ушла вместе с текстом");
        s.apply(vec![ChannelEvent::Deleted { conversation: Some(c.clone()), ids: vec![MessageId::new("2")], for_all: false }]).unwrap();
        assert_eq!(s.message(&c, &MessageId::new("2")).unwrap(), None);
        assert_eq!(s.timeline(&c, None, 10).unwrap().len(), 1);
    }

    /// Учётка: возможности, состояние, папки, участники; вложение — по хэшу; «Выйти» — всё учётки.
    #[test]
    fn account_people_attachments_and_forget() {
        let s = store();
        let c = ConversationId::new("c");
        let src = AttachmentSource(b"file-1".to_vec());
        let m = Message {
            attachments: vec![Attachment { name: "a.txt".into(), mime: "text/plain".into(), source: src.clone(), ..Attachment::default() }],
            ..msg("1", 1, "")
        };
        let f = ChannelFeatures { typing: true, ..ChannelFeatures::default() };
        let applied = s
            .apply(vec![
                ChannelEvent::Features(f.clone()),
                ChannelEvent::State(ChannelState::Online),
                ChannelEvent::People(vec![Participant { id: ParticipantId::new("p1"), name: "Анна".into(), ..Participant::default() }]),
                ChannelEvent::Members {
                    conversation: c.clone(),
                    changed: vec![Member { id: ParticipantId::new("p1"), role: Role::Admin }],
                    left: vec![],
                },
                ev_msgs("c", vec![m], false),
                ChannelEvent::Typing { conversation: c.clone(), who: ParticipantId::new("p1"), on: true },
            ])
            .unwrap();
        assert_eq!(applied.typing.len(), 1);
        assert_eq!(s.features().unwrap(), f);
        assert_eq!(s.state().unwrap(), Some(ChannelState::Online));
        assert_eq!(s.participant(&ParticipantId::new("p1")).unwrap().unwrap().name, "Анна");
        assert_eq!(s.members(&c).unwrap(), vec![Member { id: ParticipantId::new("p1"), role: Role::Admin }]);
        let mut d = s.begin_download();
        d.write(b"hello").unwrap();
        assert!(s.finish_download(d, &c, &MessageId::new("1"), &src, "text/plain").unwrap().is_some());
        assert_eq!(s.attachment(&c, &MessageId::new("1"), &src).unwrap().as_deref(), Some(&b"hello"[..]));
        assert!(s.cache().check_refs().unwrap().is_empty());
        s.forget().unwrap();
        assert!(ChannelStore::conversations(s.cache(), 0, 10).unwrap().is_empty());
        assert!(s.cache().chan_accounts().unwrap().is_empty());
    }
}
