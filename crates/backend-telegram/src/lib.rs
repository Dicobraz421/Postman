//! Канал Telegram (ADR 0012, CH-03): плагин `ChannelPlugin` ядра, клиентский протокол MTProto — `grammers`
//! (чистый Rust, без TDLib). Канал — только протокол: вход ([`signin`]), сессия (секрет ядра — [`vault`]), догон и
//! поток обновлений, история, поиск, действия, файлы (`session`), перевод TL ↔ общая модель ([`convert`],
//! [`text`]). Хранение, поиск, уведомления — у ядра.
//!
//! - **Ключ приложения** (`api_id`/`api_hash`) — при сборке из переменных окружения `POSTMAN_TG_API_ID`,
//!   `POSTMAN_TG_API_HASH` (секреты CI); в репозитории его нет. Без ключа канал в сборке есть, а вход и подключение
//!   отвечают `ChannelError::Denied(`[`NO_API_KEY`]`)` — интерфейс говорит «нет ключа приложения».
//! - **Сеть** — только по действию пользователя (вход) и для учёток, которые он добавил; ни телеметрии, ни
//!   запросов при старте без учётки.
//! - **Тесты без сети**: подставной сервер `fake` (признак `testkit`) отвечает теми же байтами TL; канал проходит
//!   `postman_core::channel::conformance` (`tests/conformance.rs`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use postman_core::backend::BoxFuture;
use postman_core::channel::{
    AccountId, Address, ChannelError, ChannelFeatures, ChannelId, ChannelPlugin, ChannelSession, ConnectCtx, ConversationId, E2e, Flow, FlowCtx,
    Limit, Participant, Reactions,
};

pub mod convert;
#[cfg(feature = "testkit")]
pub mod fake;
pub mod ids;
pub mod net;
mod session;
pub mod signin;
pub mod text;
pub mod vault;
pub mod wire;

use ids::{Peer, parse_msg};
use vault::Blob;
use wire::{Dial, WireError};

/// Id канала: адрес `postman://telegram/…`, ключ политики `AllowChannelTelegram`.
pub const ID: &str = "telegram";
/// Длина текста сообщения, знаков (UTF-16).
pub const MAX_TEXT: u32 = 4096;
/// Длина подписи к файлу.
pub const CAPTION_MAX: u32 = 1024;
/// Файл — не больше 2000 МиБ (без Telegram Premium).
pub const MAX_FILE: u64 = 2000 * 1024 * 1024;
/// Папка архива Telegram (`folder_id = 1`); название даёт интерфейс.
pub const ARCHIVE: &str = "archive";

/// Коды отказов входа и подключения в `ChannelError::Denied` — интерфейс переводит их в слова (Fluent).
pub const NO_API_KEY: &str = "no-api-key";
pub const BAD_PHONE: &str = "phone-invalid";
pub const BAD_CODE: &str = "code-invalid";
pub const CODE_EXPIRED: &str = "code-expired";
pub const BAD_PASSWORD: &str = "password-invalid";
/// Номер не зарегистрирован: регистрация — в официальном приложении.
pub const SIGN_UP: &str = "sign-up-required";

/// Ключ приложения из сборки; нет — `None`.
pub fn api_key() -> Option<(i32, String)> {
    let id = option_env!("POSTMAN_TG_API_ID")?.trim().parse().ok().filter(|v: &i32| *v > 0)?;
    let hash = option_env!("POSTMAN_TG_API_HASH")?.trim();
    (!hash.is_empty()).then(|| (id, hash.to_owned()))
}

/// Что умеет Telegram (ADR 0012 «Проверка договора»): правка 48 ч, удаление у всех, любые реакции, «прочитано»,
/// «печатает», ответ с цитатой фрагмента, упоминания, пересылка сервером, отложенная отправка, черновики, заглушить
/// и закрепить на сервере, поиск и счётчики сервера, папки; без веток и сквозного шифрования (секретных чатов нет).
pub fn features() -> ChannelFeatures {
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
        threads: false,
        e2e: E2e::No,
        max_attachment: Some(MAX_FILE),
        max_text: Some(MAX_TEXT),
    }
}

/// Общее для всех сессий плагина: учётки (для разбора ссылок `t.me`) и публичные имена бесед.
#[derive(Default)]
pub struct Shared {
    accounts: Mutex<Vec<AccountId>>,
    usernames: Mutex<HashMap<String, String>>,
}

impl Shared {
    pub(crate) fn add_account(&self, a: &AccountId) {
        if let Ok(mut v) = self.accounts.lock()
            && !v.contains(a)
        {
            v.push(a.clone());
        }
    }

    fn account(&self) -> Option<AccountId> {
        self.accounts.lock().ok()?.first().cloned()
    }

    pub(crate) fn set_username(&self, conv: &ConversationId, name: &str) {
        if let Ok(mut m) = self.usernames.lock() {
            m.insert(conv.0.clone(), name.to_ascii_lowercase());
        }
    }

    fn username(&self, conv: &ConversationId) -> Option<String> {
        self.usernames.lock().ok()?.get(&conv.0).cloned()
    }

    pub(crate) fn conv_by_username(&self, name: &str) -> Option<ConversationId> {
        let name = name.to_ascii_lowercase();
        self.usernames.lock().ok()?.iter().find(|(_, n)| **n == name).map(|(c, _)| ConversationId(c.clone()))
    }
}

/// Начало ссылки `t.me`.
pub(crate) enum LinkHead {
    /// `t.me/c/<id>/…` — закрытая супергруппа или канал.
    Channel(i64),
    /// `t.me/<имя>/…`.
    Name(String),
}

/// Ссылка `https://t.me/…` (и `telegram.me`) → беседа и, если есть, сообщение; приглашения (`+…`, `joinchat`) —
/// не адрес.
pub(crate) fn split_link(url: &str) -> Option<(LinkHead, Option<i32>)> {
    let u = url.trim();
    let rest = ["https://", "http://", ""].iter().find_map(|s| u.strip_prefix(s))?;
    let rest = ["t.me/", "telegram.me/", "www.t.me/"].iter().find_map(|h| rest.strip_prefix(h))?;
    let path = rest.split(['?', '#']).next()?;
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    match parts.as_slice() {
        ["c", id, tail @ ..] => {
            let id: i64 = id.parse().ok().filter(|v| *v > 0)?;
            let msg = tail.last().and_then(|m| m.parse().ok()).filter(|v: &i32| *v > 0);
            Some((LinkHead::Channel(id), msg))
        }
        [name, tail @ ..] => {
            let ok = name.len() >= 4
                && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                && !["joinchat", "addstickers", "share"].contains(name);
            if !ok {
                return None;
            }
            let msg = tail.last().and_then(|m| m.parse().ok()).filter(|v: &i32| *v > 0);
            Some((LinkHead::Name((*name).to_owned()), msg))
        }
        _ => None,
    }
}

/// Известное простое SRP Telegram (2048 бит, core.telegram.org/api/srp): проверка «безопасное простое» на каждом
/// входе — секунды; другое простое сервера проверяется полностью.
pub(crate) fn srp_known_prime(p: &[u8]) -> bool {
    const P: &str = "c71caeb9c6b1c9048e6c522f70f13f73980d40238e3e21c14934d037563d930f48198a0aa7c14058229493d22530f4dbfa336f6e0ac925139543aed44cce7c3720fd51f69458705ac68cd4fe6b6b13abdc9746512969328454f18faf8c595f642477fe96bb2a941d5bcd1d4ac8cc49880708fa9b378e3c4f3a9060bee67cf9a4a4a695811051907e162753b56b0f6b410dba74d8a84b2a14b3144e0ef1284754fd17ed950d5965b4b9dd46582db1178d169c6bc465b0d6ff9ca3928fef5b9ae4e418fc15e83ebea0f87fa9ff5eed70050ded2849f47bf959d956850ce929851f0d8115f635b105ee2e4e15d04b2454bf6f4fadf034b10403119cd8e3b92fcc5b";
    p.len() * 2 == P.len()
        && p.iter().zip(P.as_bytes().chunks(2)).all(|(b, h)| std::str::from_utf8(h).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) == Some(*b))
}

/// Канал Telegram.
pub struct TelegramChannel {
    dial: Arc<dyn Dial>,
    shared: Arc<Shared>,
}

/// Сборка без ключа: вход и подключение — отказ «нет ключа приложения».
struct NoKey;

impl Dial for NoKey {
    fn dial(&self, _blob: Option<Blob>) -> BoxFuture<'_, Result<std::rc::Rc<dyn wire::Wire>, WireError>> {
        Box::pin(async { Err(WireError::Protocol(NO_API_KEY.into())) })
    }

    fn api(&self) -> Option<(i32, String)> {
        None
    }

    fn srp_ok(&self, _p: &[u8], _g: i32) -> bool {
        false
    }
}

impl Default for TelegramChannel {
    fn default() -> Self {
        Self::new()
    }
}

impl TelegramChannel {
    /// Настоящая сеть с ключом приложения из сборки ([`api_key`]).
    pub fn new() -> Self {
        let dial: Arc<dyn Dial> = match api_key() {
            Some((api_id, api_hash)) => Arc::new(net::NetDial { api_id, api_hash }),
            None => Arc::new(NoKey),
        };
        Self::with_dial(dial)
    }

    /// Свой способ подключения (подставной сервер в тестах).
    pub fn with_dial(dial: Arc<dyn Dial>) -> Self {
        Self { dial, shared: Arc::new(Shared::default()) }
    }

    /// Ключ приложения есть в этой сборке.
    pub fn has_key(&self) -> bool {
        self.dial.api().is_some()
    }
}

impl ChannelPlugin for TelegramChannel {
    fn id(&self) -> &'static str {
        ID
    }

    fn features(&self) -> ChannelFeatures {
        features()
    }

    fn parse_link(&self, url: &str) -> Option<Address> {
        let (head, msg) = split_link(url)?;
        let account = self.shared.account()?;
        let conv = match head {
            LinkHead::Channel(id) => Peer::Channel(id).conv(),
            LinkHead::Name(name) => self.shared.conv_by_username(&name)?,
        };
        let channel = ChannelId::new(ID);
        Some(match msg {
            Some(m) => Address::message(&channel, &account, &conv, &ids::msg_id(m)),
            None => Address::conversation(&channel, &account, &conv),
        })
    }

    fn link(&self, at: &Address) -> Option<String> {
        let peer = Peer::parse(&at.conversation)?;
        let msg = at.message.as_ref().and_then(parse_msg);
        if let Some(name) = self.shared.username(&at.conversation) {
            return Some(match (peer, msg) {
                (Peer::Channel(_), Some(m)) => format!("https://t.me/{name}/{m}"),
                _ => format!("https://t.me/{name}"),
            });
        }
        match (peer, msg) {
            (Peer::Channel(id), Some(m)) => Some(format!("https://t.me/c/{id}/{m}")),
            _ => None,
        }
    }

    fn person_url(&self, p: &Participant) -> Option<String> {
        p.handle.as_ref().filter(|h| !h.is_empty()).map(|h| format!("https://t.me/{h}"))
    }

    fn sign_in<'a>(&'a self, _ctx: FlowCtx) -> BoxFuture<'a, Result<Box<dyn Flow + 'a>, ChannelError>> {
        Box::pin(async move {
            let flow: Box<dyn Flow + 'a> = Box::new(signin::SignIn::new(Arc::clone(&self.dial), Arc::clone(&self.shared))?);
            Ok(flow)
        })
    }

    fn connect<'a>(&'a self, ctx: ConnectCtx) -> BoxFuture<'a, Result<Box<dyn ChannelSession>, ChannelError>> {
        Box::pin(async move {
            if self.dial.api().is_none() {
                return Err(ChannelError::Denied(NO_API_KEY.into()));
            }
            let blob = Blob::from_secret(&ctx.secret).ok_or_else(|| ChannelError::SignedOut("not a telegram session".into()))?;
            if blob.user.to_string() != ctx.account.0 {
                return Err(ChannelError::SignedOut("session of another account".into()));
            }
            let wire = self.dial.dial(Some(blob)).await.map_err(WireError::channel)?;
            let s = session::TgSession::open(wire, Arc::clone(&self.shared), ctx.account, ctx.incoming, ctx.store).await?;
            let s: Box<dyn ChannelSession> = Box::new(s);
            Ok(s)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links() {
        assert!(matches!(split_link("https://t.me/c/123/45"), Some((LinkHead::Channel(123), Some(45)))));
        assert!(matches!(split_link("https://t.me/c/123/7/45?single"), Some((LinkHead::Channel(123), Some(45)))));
        assert!(matches!(split_link("t.me/durov_test"), Some((LinkHead::Name(n), None)) if n == "durov_test"));
        assert!(matches!(split_link("https://telegram.me/news_example/10"), Some((LinkHead::Name(_), Some(10)))));
        for bad in ["https://t.me/+AbCdEf", "https://t.me/joinchat/xyz", "https://example.test/c/1/2", "https://t.me/", "https://t.me/c/x/1"] {
            assert!(split_link(bad).is_none(), "{bad}");
        }
        let p = TelegramChannel::with_dial(Arc::new(NoKey));
        assert_eq!(p.parse_link("https://t.me/c/123/45"), None, "нет учётки — нет адреса");
        p.shared.add_account(&AccountId::new("9990001"));
        p.shared.set_username(&ConversationId::new("c77"), "News_Example");
        let a = p.parse_link("https://t.me/c/123/45").unwrap();
        assert_eq!(a.to_string(), "postman://telegram/9990001/c123/45");
        assert_eq!(p.link(&a).as_deref(), Some("https://t.me/c/123/45"));
        let n = p.parse_link("https://t.me/news_example/9").unwrap();
        assert_eq!(n.conversation.0, "c77");
        assert_eq!(p.link(&n).as_deref(), Some("https://t.me/news_example/9"));
        assert_eq!(p.link(&Address::conversation(&ChannelId::new(ID), &AccountId::new("1"), &ConversationId::new("u5"))), None);
        let anna = Participant { handle: Some("anna_example".into()), ..Participant::default() };
        assert_eq!(p.person_url(&anna).as_deref(), Some("https://t.me/anna_example"));
    }

    /// Без ключа приложения — понятный отказ, без сети.
    #[test]
    fn without_key_says_so() {
        struct Mem;
        impl postman_core::channel::PluginStore for Mem {
            fn get(&self, _: &str) -> Result<Option<Vec<u8>>, ChannelError> {
                Ok(None)
            }
            fn put(&self, _: &str, _: &[u8]) -> Result<(), ChannelError> {
                Ok(())
            }
            fn remove_prefix(&self, _: &str) -> Result<(), ChannelError> {
                Ok(())
            }
        }
        let p = TelegramChannel::with_dial(Arc::new(NoKey));
        assert!(!p.has_key());
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let r = rt.block_on(p.sign_in(FlowCtx { store: std::rc::Rc::new(Mem) }));
        assert!(matches!(r, Err(ChannelError::Denied(ref c)) if c == NO_API_KEY));
    }
}
