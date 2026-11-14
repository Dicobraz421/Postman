//! Почта — канал `mail` адаптером (ADR 0012 п. 7): почта не идёт через `ChannelSession` и не копируется в таблицы
//! чатов. Две вещи:
//!
//! - **чтение** — [`MailSource`] ([`ConversationSource`]) над таблицами писем: беседа — `EmailRow::thread_id`
//!   (`kind = MailThread`, название — тема), сообщение — письмо (автор — «От», время — `received_at`, тело — текст
//!   или HTML тела; HTML в IR переводит воркер, как сейчас), строятся на лету, без записи;
//! - **действия** — [`MailSource::plan`]: `channel::Request` → прежние `sync::Command` (и правки кэша, которые
//!   интерфейс делает до ответа сервера, как сейчас); события почты → `channel::Update` ([`update`]).
//!
//! Адрес письма: учётка — адрес ящика строчными (основной, дополнительный, общий), беседа — id цепочки на сервере
//! (без префикса учётки кэша), сообщение — `Message-ID` без `<>`; нет `Message-ID` (тело ещё не загружено) —
//! `~<LetterId>`, только этот компьютер. Почтовая модель, ключи, формат кэша и `MailPlugin` не меняются.

use std::collections::BTreeSet;

use postman_cache::{BodyRow, Cache, ENCRYPTED, EmailRow, SEEN};

use super::{
    AccountId, Address, Attachment, AttachmentKind, AttachmentSource, Body, ChanAccount, ChannelError, ChannelFeatures, ChannelId, ChannelState,
    Conversation, ConversationId, ConversationKind, ConversationSource, Counts, E2e, Limit, Line, Message, MessageId, Participant, ParticipantId,
    Request, Summary, Update, source,
};
use crate::filters::Me;
use crate::lifecycle::Patch;
use crate::sync::{Command, Event, Status};

type Result<T> = std::result::Result<T, ChannelError>;

/// id сообщения без `Message-ID`: `~<LetterId>`.
const LOCAL: char = '~';

/// id письма без `Message-ID` (`~<LetterId>`): открывается только на этом компьютере.
pub fn is_local(id: &MessageId) -> bool {
    id.0.starts_with(LOCAL)
}

/// Письмо id `~<LetterId>`; иначе — `None`.
pub fn local_letter(id: &MessageId) -> Option<crate::model::LetterId> {
    id.0.strip_prefix(LOCAL)?.parse().ok().map(crate::model::LetterId)
}

/// Отметка в состоянии хранилища: тела, записанные до индекса `by_message_id`, в него дописаны.
const MID_INDEXED: &str = "channel.message_ids";
/// Тел за один проход дописывания.
const MID_BATCH: usize = 2_000;

/// Дописать в индекс `by_message_id` тела, записанные до него (один раз на базу, в фоне): `false` — прервано `stop`,
/// следующий вызов продолжит с того же места.
pub fn index_message_ids(cache: &Cache, stop: &dyn Fn() -> bool) -> Result<bool> {
    let state = cache.state(MID_INDEXED)?;
    if state.as_deref() == Some("") {
        return Ok(true);
    }
    let mut after = state;
    loop {
        if stop() {
            return Ok(false);
        }
        match cache.index_message_ids(after.as_deref(), MID_BATCH)? {
            Some(k) => {
                cache.set_state(MID_INDEXED, &k)?;
                after = Some(k);
            }
            None => {
                cache.set_state(MID_INDEXED, "")?;
                return Ok(true);
            }
        }
    }
}

/// Ящик почты в адресах каналов: учётка кэша (`None` — основная, `@<n>` — дополнительная, id общего ящика) и адрес.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailAccount {
    pub scope: Option<String>,
    pub address: String,
}

impl MailAccount {
    pub fn new(scope: Option<&str>, address: &str) -> Self {
        Self { scope: scope.map(str::to_owned), address: address.trim().to_lowercase() }
    }
}

/// Что умеет почта как канал (ADR 0012 п. 7): правки, удаления у всех, реакций, «печатает» и «прочитано»
/// собеседником нет; ответ, цитаты, упоминания — есть; отложенная отправка — по `Release` пути; заглушить и
/// закрепить — ключевыми словами на сервере; сквозное шифрование — S/MIME по желанию; предел вложения — сервера.
pub fn features(max_attachment: Option<u64>) -> ChannelFeatures {
    ChannelFeatures {
        edit: Limit::No,
        delete_for_all: Limit::No,
        reply_to: true,
        quote_fragment: true,
        mentions: true,
        scheduled_send: true,
        server_mute: true,
        server_pin: true,
        server_search: true,
        e2e: E2e::Optional,
        max_attachment,
        ..ChannelFeatures::default()
    }
}

/// `Message-ID` без `<>` и пробелов.
fn bare(id: &str) -> &str {
    id.trim().trim_start_matches('<').trim_end_matches('>')
}

/// Что сделать с почтой по запросу канала: правки кэша (интерфейс применяет их сразу, как сейчас) и команды
/// синхронизации.
#[derive(Default)]
pub struct Plan {
    pub patches: Vec<Patch>,
    pub commands: Vec<Command>,
}

/// Беседы почты в общей модели — из таблиц писем, на лету.
#[derive(Clone)]
pub struct MailSource {
    cache: Cache,
    accounts: Vec<MailAccount>,
    me: Me,
}

impl MailSource {
    /// `accounts` — ящики профиля с адресами; `me` — мои адреса и имена (свои письма, упоминания).
    pub fn new(cache: Cache, accounts: Vec<MailAccount>, me: Me) -> Self {
        Self { cache, accounts, me }
    }

    fn channel() -> ChannelId {
        ChannelId::new(super::MAIL)
    }

    /// Учётка канала ящика `address`.
    pub fn chan(address: &str) -> ChanAccount {
        ChanAccount { channel: Self::channel(), account: AccountId(address.trim().to_lowercase()) }
    }

    fn scope_of(&self, account: &AccountId) -> Option<Option<&str>> {
        self.accounts.iter().find(|a| a.address.eq_ignore_ascii_case(&account.0)).map(|a| a.scope.as_deref())
    }

    /// Ящик письма или беседы по ключу кэша.
    fn account_of_key(&self, key: &str) -> Option<&MailAccount> {
        let scope = postman_cache::account_of(key);
        self.accounts.iter().find(|a| a.scope.as_deref() == scope)
    }

    /// Адрес беседы письма; ящика нет среди известных — `None`.
    pub fn address_of(&self, row: &EmailRow) -> Option<Address> {
        let a = self.account_of_key(&row.id)?;
        let thread = postman_cache::unscoped(&row.thread_id).1;
        Some(Address::conversation(&Self::channel(), &AccountId(a.address.clone()), &ConversationId::new(thread)))
    }

    /// Письма беседы адреса, старые первыми.
    fn rows(&self, at: &Address) -> Result<Vec<EmailRow>> {
        if at.channel.0 != super::MAIL {
            return Ok(Vec::new());
        }
        let Some(scope) = self.scope_of(&at.account) else { return Ok(Vec::new()) };
        Ok(self.cache.thread(&postman_cache::scoped(scope, &at.conversation.0))?)
    }

    /// id сообщения письма: `Message-ID` тела или `~<LetterId>`.
    pub fn message_id(&self, row: &EmailRow, body: Option<&BodyRow>) -> MessageId {
        if let Some(id) = body.and_then(|b| b.message_id.as_deref()).map(bare).filter(|s| !s.is_empty()) {
            return MessageId::new(id);
        }
        match crate::store::letter_of_key(&self.cache, &row.id) {
            Ok(Some(l)) => MessageId(format!("{LOCAL}{}", l.0)),
            _ => MessageId(format!("{LOCAL}{}", postman_cache::unscoped(&row.id).1)),
        }
    }

    /// Учётки кэша ящиков профиля.
    pub(crate) fn scopes(&self) -> Vec<Option<&str>> {
        self.accounts.iter().map(|a| a.scope.as_deref()).collect()
    }

    pub(crate) fn me(&self) -> &Me {
        &self.me
    }

    /// Моё письмо: «От» — мой адрес.
    pub(crate) fn mine(&self, row: &EmailRow) -> bool {
        self.me.addresses().iter().any(|a| a.eq_ignore_ascii_case(row.from_addr.trim()))
    }

    /// Письмо в общей модели.
    pub fn message_of(&self, row: &EmailRow, body: Option<&BodyRow>) -> Message {
        let text = body.and_then(|b| b.text.clone()).filter(|t| !t.trim().is_empty());
        let body_of = match (body.and_then(|b| b.html.clone()), text) {
            (Some(html), text) => Body::Html { html, plain: text.unwrap_or_else(|| row.preview.clone()) },
            (None, Some(text)) => Body::Plain(text),
            (None, None) => Body::Plain(row.preview.clone()),
        };
        let attachments = body
            .map(|b| {
                b.attachments
                    .iter()
                    .filter(|a| !a.inline)
                    .map(|a| Attachment {
                        name: a.name.clone(),
                        mime: a.mime.clone(),
                        size: Some(a.size),
                        kind: if a.mime.starts_with("image/") { AttachmentKind::Image { w: 0, h: 0 } } else { AttachmentKind::File },
                        source: AttachmentSource(a.blob_id.clone().into_bytes()),
                        thumb: None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mentioned = body.is_some_and(|b| self.me.mentioned_in_body(b));
        let mine = self.mine(row);
        Message {
            id: self.message_id(row, body),
            author: ParticipantId(row.from_addr.trim().to_lowercase()),
            sent: row.received_at.saturating_mul(1000),
            body: body_of,
            attachments,
            reply_to: body.and_then(|b| b.references.last()).map(|r| MessageId::new(bare(r))),
            mentions: if mentioned { self.me.addresses().first().map(|a| vec![ParticipantId(a.clone())]).unwrap_or_default() } else { Vec::new() },
            mine,
            ..Message::default()
        }
    }

    /// Беседа почты по её письмам (старые первыми).
    fn conversation_of(&self, at: &Address, rows: &[EmailRow]) -> Result<Conversation> {
        let mut people = BTreeSet::new();
        let mut mentions = 0;
        for r in rows {
            people.insert(r.from_addr.trim().to_lowercase());
            if !r.seen && !self.mine(r) && !self.me.is_empty() && self.cache.body(&r.id)?.is_some_and(|b| self.me.mentioned_in_body(&b)) {
                mentions += 1;
            }
        }
        let unread = rows.iter().filter(|r| !r.seen && !self.mine(r)).count();
        let last_read = rows.iter().rev().find(|r| r.seen).map(|r| self.message_id(r, self.cache.body(&r.id).ok().flatten().as_ref()));
        Ok(Conversation {
            id: at.conversation.clone(),
            kind: ConversationKind::MailThread,
            title: rows.first().map(|r| r.subject.clone()).unwrap_or_default(),
            members: Some(people.len() as u32),
            last_read,
            unread: Some(Counts { unread: unread as u32, mentions }),
            muted_until: crate::mute::thread_muted(rows).then_some(i64::MAX),
            pinned: rows.iter().any(crate::pin::is_pinned),
            encrypted: rows.iter().any(|r| r.has_keyword(ENCRYPTED)),
            ..Conversation::default()
        })
    }

    /// Беседа адреса в общей модели по письмам `rows` (уже прочитанным) — для списков почты.
    pub fn summary_of(&self, row: &EmailRow) -> Result<Option<Summary>> {
        let Some(at) = self.address_of(row) else { return Ok(None) };
        self.conversation(&at)
    }

    /// Письма беседы до сообщения `up_to` включительно (по времени).
    fn up_to(&self, rows: Vec<EmailRow>, up_to: &MessageId) -> Result<Vec<EmailRow>> {
        let mut out = Vec::new();
        for r in rows {
            let id = self.message_id(&r, self.cache.body(&r.id)?.as_ref());
            out.push(r);
            if id == *up_to {
                return Ok(out);
            }
        }
        // Сообщения нет в беседе (удалили, другое письмо): отметить всю беседу — как «Прочитать беседу».
        Ok(out)
    }

    /// Запрос канала → правки кэша и прежние команды почты. Чего почта не умеет — `Unsupported`; отправка почты —
    /// как сейчас, письмом (`Command::Send` с `Release`, окно «Отменить» и «позже» держит путь почты).
    pub fn plan(&self, request: &Request) -> Result<Plan> {
        let mut plan = Plan::default();
        match request {
            Request::History { conversation, .. } => {
                let Some(row) = self.any_row(conversation)? else { return Err(ChannelError::NotFound) };
                plan.commands.push(Command::Thread(row.thread_id));
            }
            Request::Search { ticket, query, .. } => plan.commands.push(Command::Search { query: query.clone(), mailbox: None, ticket: *ticket }),
            Request::Download { source, mime, .. } => {
                let blob_id = String::from_utf8(source.0.clone()).map_err(|_| ChannelError::NotFound)?;
                plan.commands.push(Command::Blob { blob_id, name: String::new(), mime: mime.clone() });
            }
            Request::Reconnect => plan.commands.push(Command::Reconnect),
            Request::Act { action, .. } => {
                // Заглушить и закрепить — ключевое слово у каждого письма беседы, где оно другое.
                let (conv, word, on) = match action {
                    super::Action::MarkRead { conversation, up_to } => {
                        let rows = self.thread_rows(conversation)?;
                        let ids: Vec<String> = self.up_to(rows, up_to)?.into_iter().filter(|r| !r.seen).map(|r| r.id).collect();
                        if !ids.is_empty() {
                            plan.patches.extend(ids.iter().map(|id| keyword(id, SEEN, true)));
                            plan.commands.push(Command::SetKeywords { ids, add: vec![SEEN.to_owned()], remove: Vec::new() });
                        }
                        return Ok(plan);
                    }
                    super::Action::Mute { conversation, until } => (conversation, crate::mute::MUTED, until.is_some()),
                    super::Action::Pin { conversation, on } => (conversation, crate::pin::PINNED, *on),
                    _ => return Err(ChannelError::Unsupported),
                };
                let rows = self.thread_rows(conv)?;
                let patches: Vec<Patch> = rows.iter().filter(|r| r.has_keyword(word) != on).map(|r| keyword(&r.id, word, on)).collect();
                if !patches.is_empty() {
                    plan.patches.clone_from(&patches);
                    plan.commands.push(Command::Patch { patches, archive_name: String::new() });
                }
            }
            // Отправка почты — письмом (окно «Отменить» и «позже» держит путь почты), не очередью каналов.
            Request::Upload { .. } | Request::SignOut | Request::Send { .. } | Request::Unsend { .. } => return Err(ChannelError::Unsupported),
        }
        Ok(plan)
    }

    /// Письма беседы почты по id беседы у любого ящика профиля (запрос без учётки — по беседе).
    fn thread_rows(&self, conv: &ConversationId) -> Result<Vec<EmailRow>> {
        for a in &self.accounts {
            let rows = self.cache.thread(&postman_cache::scoped(a.scope.as_deref(), &conv.0))?;
            if !rows.is_empty() {
                return Ok(rows);
            }
        }
        Ok(Vec::new())
    }

    fn any_row(&self, conv: &ConversationId) -> Result<Option<EmailRow>> {
        Ok(self.thread_rows(conv)?.into_iter().next())
    }

    /// Строка письма по `LetterId` (действующая связь с сервером); письмо «На этом компьютере» — `None`.
    fn row_of_letter(&self, letter: crate::model::LetterId) -> Result<Option<EmailRow>> {
        let Some(key) = crate::store::key_of(&self.cache, letter)? else { return Ok(None) };
        Ok(self.cache.rows_by_ids(&[key])?.into_iter().next())
    }

    /// Письмо адреса сообщения (ADR 0012 п. 7): сначала в беседе адреса (подсказка), не нашлось — по `Message-ID`
    /// (индекс `by_message_id`; письмо этого ящика — первым), `~<LetterId>` — по ключу письма. Нет в базе — `None`
    /// (искать на сервере по заголовку — путь учётки).
    pub fn find(&self, at: &Address) -> Result<Option<(EmailRow, Option<BodyRow>)>> {
        let Some(want) = at.message.as_ref().filter(|_| at.channel.0 == super::MAIL) else { return Ok(None) };
        for r in self.rows(at)? {
            let body = self.cache.body(&r.id)?;
            if self.message_id(&r, body.as_ref()) == *want {
                return Ok(Some((r, body)));
            }
        }
        let letters = match local_letter(want) {
            Some(l) => vec![l],
            None => self.cache.letters_by_message_id(&want.0)?.into_iter().map(|l| crate::model::LetterId(l.0)).collect(),
        };
        let mut found = Vec::new();
        for l in letters {
            if let Some(r) = self.row_of_letter(l)?
                && self.address_of(&r).is_some()
            {
                found.push(r);
            }
        }
        let mine = self.scope_of(&at.account);
        found.sort_by_key(|r| Some(postman_cache::account_of(&r.id)) != mine);
        let Some(r) = found.into_iter().next() else { return Ok(None) };
        let body = self.cache.body(&r.id)?;
        Ok(Some((r, body)))
    }

    /// Адрес сообщения, как он есть в базе сейчас: ящик и беседа письма (ссылка с другого компьютера или из другого
    /// ящика); нет такого письма — `None`.
    pub fn resolve(&self, at: &Address) -> Result<Option<Address>> {
        let Some((row, body)) = self.find(at)? else { return Ok(None) };
        let Some(conv) = self.address_of(&row) else { return Ok(None) };
        let id = self.message_id(&row, body.as_ref());
        Ok(Some(Address { message: Some(id), ..conv }))
    }
}

/// Правка: поставить или снять ключевое слово.
fn keyword(id: &str, keyword: &str, on: bool) -> Patch {
    Patch { id: id.to_owned(), keywords: vec![(keyword.to_owned(), on)], ..Patch::default() }
}

impl ConversationSource for MailSource {
    fn conversation(&self, at: &Address) -> Result<Option<Summary>> {
        let rows = self.rows(at)?;
        let Some(last) = rows.last() else { return Ok(None) };
        let conversation = self.conversation_of(at, &rows)?;
        let preview = source::preview(&last.preview);
        let author = Some(ParticipantId(last.from_addr.trim().to_lowercase()));
        Ok(Some(Summary { address: at.of_conversation(), conversation, last: last.received_at.saturating_mul(1000), preview, author }))
    }

    fn timeline(&self, at: &Address, before: Option<&MessageId>, limit: usize) -> Result<Vec<Line>> {
        let mut lines = Vec::new();
        for r in self.rows(at)? {
            let body = self.cache.body(&r.id)?;
            let m = self.message_of(&r, body.as_ref());
            if before.is_some_and(|b| *b == m.id) {
                break;
            }
            lines.push(Line::Message { message: m, loaded: Vec::new() });
        }
        let skip = lines.len().saturating_sub(limit);
        Ok(lines.split_off(skip))
    }

    fn message(&self, at: &Address) -> Result<Option<Line>> {
        Ok(self.find(at)?.map(|(r, body)| Line::Message { message: self.message_of(&r, body.as_ref()), loaded: Vec::new() }))
    }

    fn people(&self, at: &Address) -> Result<Vec<Participant>> {
        let mut out: Vec<Participant> = Vec::new();
        let mut add = |name: &str, email: &str| {
            let id = email.trim().to_lowercase();
            if id.is_empty() || out.iter().any(|p| p.id.0 == id) {
                return;
            }
            out.push(Participant {
                id: ParticipantId(id.clone()),
                name: if name.is_empty() { id.clone() } else { name.to_owned() },
                emails: vec![id],
                ..Participant::default()
            });
        };
        for r in self.rows(at)? {
            add(&r.from_name, &r.from_addr);
            if let Some(b) = self.cache.body(&r.id)? {
                for a in b.to.iter().chain(&b.cc) {
                    add(&a.name, &a.email);
                }
            }
        }
        Ok(out)
    }
}

/// Событие почты → обновление общей модели: письма и беседы изменились, состояние связи. Прочие события почты —
/// только почте (`None`).
pub fn update(event: &Event) -> Option<Update> {
    Some(match event {
        Event::EmailsChanged | Event::BodiesStored(_) => Update::Changed { conversations: Vec::new(), gone: Vec::new() },
        Event::ThreadReady { thread, .. } => {
            Update::Changed { conversations: vec![ConversationId::new(postman_cache::unscoped(thread).1)], gone: Vec::new() }
        }
        Event::Status(Status::Connecting { .. }) => Update::State(ChannelState::Connecting),
        Event::Status(Status::Synced) => Update::State(ChannelState::Online),
        Event::Connection(crate::link::Connection::Online) => Update::State(ChannelState::Online),
        Event::Connection(crate::link::Connection::Offline { retry_in, .. }) => Update::State(ChannelState::Offline { retry_in: *retry_in }),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use postman_cache::Addr;

    fn row(id: &str, thread: &str, at: i64, from: &str, seen: bool) -> EmailRow {
        EmailRow {
            id: id.into(),
            thread_id: thread.into(),
            mailbox_ids: vec!["in".into()],
            received_at: at,
            subject: "Отчёт за неделю".into(),
            from_name: String::new(),
            from_addr: from.into(),
            preview: format!("превью {id}"),
            seen,
            flagged: false,
            has_attachment: false,
            size: 0,
            keywords: Vec::new(),
        }
    }

    fn source() -> MailSource {
        let cache = Cache::open_in_memory().unwrap();
        cache
            .upsert_emails(&[
                row("e1", "t1", 100, "anna@example.test", true),
                row("e2", "t1", 200, "me@example.test", true),
                row("e3", "t1", 300, "Anna@Example.test", false),
                row("@2/e9", "@2/t9", 50, "b.petrov@corp.example", false),
            ])
            .unwrap();
        let body = |mid: &str, text: &str| BodyRow {
            message_id: Some(format!("<{mid}>")),
            text: Some(text.into()),
            to: vec![Addr { name: "Я".into(), email: "me@example.test".into() }],
            ..BodyRow::default()
        };
        cache.put_body("e1", &body("m1@example.test", "Посмотри отчёт")).unwrap();
        cache.put_body("e3", &BodyRow { references: vec!["<m1@example.test>".into()], ..body("m3@example.test", "@me посмотри риски") }).unwrap();
        let me = Me::new(["me@example.test"], ["Я"]);
        MailSource::new(cache, vec![MailAccount::new(None, "Me@Example.test"), MailAccount::new(Some("@2"), "me@corp.example")], me)
    }

    fn at(account: &str, thread: &str) -> Address {
        Address::conversation(&ChannelId::new(super::super::MAIL), &AccountId::new(account), &ConversationId::new(thread))
    }

    /// Беседа почты на лету: название — тема, непрочитанное — чужие без `$seen`, упоминание — по телу; лента —
    /// письма по времени, `Message-ID` без `<>`, без него — `~<LetterId>`; ответ — по `References`.
    #[test]
    fn mail_threads_in_the_common_model() {
        let s = source();
        let a = at("me@example.test", "t1");
        let sum = s.conversation(&a).unwrap().unwrap();
        assert_eq!(sum.conversation.kind, ConversationKind::MailThread);
        assert_eq!(sum.conversation.title, "Отчёт за неделю");
        assert_eq!(sum.conversation.unread, Some(Counts { unread: 1, mentions: 1 }));
        assert_eq!(sum.conversation.members, Some(2), "адрес — без регистра");
        assert_eq!((sum.last, sum.preview.as_str()), (300_000, "превью e3"));
        let lines = s.timeline(&a, None, 10).unwrap();
        let ids: Vec<&str> = lines.iter().map(|l| l.id().0.as_str()).collect();
        assert_eq!(ids[0], "m1@example.test");
        assert!(ids[1].starts_with('~'), "{ids:?}");
        assert_eq!(ids[2], "m3@example.test");
        let m3 = lines[2].message().unwrap();
        assert_eq!(m3.reply_to, Some(MessageId::new("m1@example.test")));
        assert_eq!(m3.author, ParticipantId::new("anna@example.test"));
        assert_eq!(m3.mentions, vec![ParticipantId::new("me@example.test")]);
        assert!(lines[1].message().unwrap().mine);
        assert_eq!(s.timeline(&a, Some(&MessageId::new("m3@example.test")), 1).unwrap().len(), 1);
        let one = Address { message: Some(MessageId::new("m3@example.test")), ..a.clone() };
        assert_eq!(s.message(&one).unwrap().unwrap().id().0, "m3@example.test");
        let people: Vec<String> = s.people(&a).unwrap().into_iter().map(|p| p.id.0).collect();
        assert_eq!(people, ["anna@example.test", "me@example.test"]);
        // Дополнительный ящик — по своему адресу; чужой адрес — пусто.
        assert!(s.conversation(&at("me@corp.example", "t9")).unwrap().is_some());
        assert!(s.conversation(&at("other@example.test", "t1")).unwrap().is_none());
        let row = s.cache.thread("@2/t9").unwrap().pop().unwrap();
        assert_eq!(s.address_of(&row).unwrap().to_string(), "postman://mail/me@corp.example/t9");
    }

    /// Сообщение по адресу с другой беседой или из другого ящика — по `Message-ID` (`by_message_id`); `~<LetterId>` —
    /// по ключу письма; тела до индекса дописываются в фоне.
    #[test]
    fn letters_found_by_message_id() {
        let s = source();
        let wrong = Address { message: Some(MessageId::new("m3@example.test")), ..at("me@corp.example", "t-old") };
        let real = s.resolve(&wrong).unwrap().unwrap();
        assert_eq!(real.to_string(), "postman://mail/me@example.test/t1/m3@example.test");
        assert!(s.message(&wrong).unwrap().is_some());
        let local = s.timeline(&at("me@example.test", "t1"), None, 10).unwrap()[1].id().clone();
        assert!(is_local(&local));
        let by_letter = Address { message: Some(local.clone()), ..at("me@example.test", "elsewhere") };
        assert_eq!(s.resolve(&by_letter).unwrap().unwrap().message, Some(local));
        assert!(s.resolve(&Address { message: Some(MessageId::new("none@example.test")), ..wrong }).unwrap().is_none());
        assert!(index_message_ids(&s.cache, &|| false).unwrap());
        assert!(index_message_ids(&s.cache, &|| true).unwrap(), "уже дописано — сразу");
    }

    /// Действия канала → прежние команды почты: «прочитано до», заглушить, закрепить; отправки и правки нет.
    #[test]
    fn requests_become_mail_commands() {
        let s = source();
        let conv = ConversationId::new("t1");
        let act = |action| Request::Act { ticket: 1, action };
        let p = s.plan(&act(super::super::Action::MarkRead { conversation: conv.clone(), up_to: MessageId::new("m3@example.test") })).unwrap();
        assert!(matches!(&p.commands[..], [Command::SetKeywords { ids, .. }] if ids == &["e3".to_owned()]));
        let p = s.plan(&act(super::super::Action::Mute { conversation: conv.clone(), until: Some(i64::MAX) })).unwrap();
        assert_eq!(p.patches.len(), 3);
        assert!(matches!(&p.commands[..], [Command::Patch { .. }]));
        let p = s.plan(&act(super::super::Action::Pin { conversation: conv.clone(), on: false })).unwrap();
        assert!(p.commands.is_empty(), "не закреплено — снимать нечего");
        let edit = super::super::Action::Edit { conversation: conv, message: MessageId::new("x"), body: Default::default(), mentions: vec![] };
        assert!(matches!(s.plan(&act(edit)), Err(ChannelError::Unsupported)));
        assert!(!features(None).allows(&super::super::Action::Typing { conversation: ConversationId::default(), on: true }));
        assert!(matches!(update(&Event::Status(Status::Synced)), Some(Update::State(ChannelState::Online))));
    }
}
