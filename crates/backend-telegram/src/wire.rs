//! Граница с сетью: сериализованные вызовы TL и поток обновлений. Настоящая — `net` (grammers, MTProto); в
//! тестах — подставной сервер (`fake`, признак `testkit`): канал проверяется без сети на тех же байтах TL.

use std::rc::Rc;
use std::time::Duration;

use grammers_tl_types::{self as tl, Deserializable as _, RemoteCall};
use postman_core::backend::BoxFuture;
use postman_core::channel::ChannelError;

use crate::vault::{Blob, UpdState};

/// Ошибка вызова: ответ сервера (`rpc_error`), связь или непонятный ответ.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    /// `rpc_error`: код, имя без числа (`FLOOD_WAIT`), число из имени (`FLOOD_WAIT_30` → 30).
    #[error("rpc {code} {name}")]
    Rpc { code: i32, name: String, value: Option<u32> },
    #[error("network: {0}")]
    Network(String),
    #[error("protocol: {0}")]
    Protocol(String),
}

impl WireError {
    /// Ответ сервера с именем `name`; `*` в конце — по началу имени (`PHONE_CODE_*`).
    pub fn is(&self, name: &str) -> bool {
        match self {
            WireError::Rpc { name: n, .. } => match name.strip_suffix('*') {
                Some(prefix) => n.starts_with(prefix),
                None => n == name,
            },
            _ => false,
        }
    }

    /// Ошибка сервера из полного имени (`FLOOD_WAIT_30`): число в конце — отдельно, как у grammers.
    pub fn rpc(code: i32, full: &str) -> Self {
        let (name, value) = match full.rsplit_once('_') {
            Some((head, tail)) if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => (head.to_owned(), tail.parse().ok()),
            _ => (full.to_owned(), None),
        };
        WireError::Rpc { code, name, value }
    }

    /// «Проблема → причина → исправление» ядра (ADR 0012 «Проверка договора», ошибки).
    pub fn channel(self) -> ChannelError {
        match self {
            WireError::Rpc { code, name, value } => {
                if code == 420 || name.ends_with("_WAIT") || name == "FLOOD_WAIT" || name == "FLOOD_PREMIUM_WAIT" {
                    return ChannelError::Wait { secs: value.unwrap_or(1).max(1) };
                }
                match name.as_str() {
                    "AUTH_KEY_UNREGISTERED"
                    | "AUTH_KEY_INVALID"
                    | "SESSION_REVOKED"
                    | "SESSION_EXPIRED"
                    | "USER_DEACTIVATED"
                    | "USER_DEACTIVATED_BAN"
                    | "AUTH_KEY_PERM_EMPTY" => ChannelError::SignedOut(name),
                    "CHAT_WRITE_FORBIDDEN"
                    | "USER_BANNED_IN_CHANNEL"
                    | "CHAT_ADMIN_REQUIRED"
                    | "MESSAGE_AUTHOR_REQUIRED"
                    | "CHAT_FORBIDDEN"
                    | "USER_IS_BLOCKED"
                    | "YOU_BLOCKED_USER"
                    | "PRIVACY_PREMIUM_REQUIRED"
                    | "MESSAGE_EDIT_TIME_EXPIRED"
                    | "MESSAGE_DELETE_FORBIDDEN"
                    | "CHAT_SEND_MEDIA_FORBIDDEN"
                    | "CHAT_RESTRICTED"
                    | "USER_RESTRICTED"
                    | "REACTION_INVALID" => ChannelError::Denied(name),
                    "PEER_ID_INVALID"
                    | "CHANNEL_INVALID"
                    | "CHANNEL_PRIVATE"
                    | "CHAT_ID_INVALID"
                    | "MESSAGE_ID_INVALID"
                    | "MSG_ID_INVALID"
                    | "USERNAME_NOT_OCCUPIED"
                    | "USERNAME_INVALID"
                    | "USER_ID_INVALID"
                    | "INPUT_USER_DEACTIVATED" => ChannelError::NotFound,
                    "MESSAGE_TOO_LONG" => ChannelError::TooLarge { max: u64::from(crate::MAX_TEXT) },
                    "FILE_PARTS_INVALID" | "FILE_PART_TOO_BIG" | "FILE_PART_SIZE_INVALID" => ChannelError::TooLarge { max: crate::MAX_FILE },
                    _ if code == 401 => ChannelError::SignedOut(name),
                    _ if code == 403 => ChannelError::Denied(name),
                    _ => ChannelError::Protocol(format!("{code} {name}")),
                }
            }
            WireError::Network(e) => ChannelError::Network(e),
            WireError::Protocol(e) => ChannelError::Protocol(e),
        }
    }
}

impl From<WireError> for ChannelError {
    fn from(e: WireError) -> Self {
        e.channel()
    }
}

/// Пачка обновлений после разбора пропусков (`getDifference` — внутри grammers): обновления и люди, беседы пачки.
#[derive(Debug, Clone, Default)]
pub struct UpdateBatch {
    pub updates: Vec<tl::enums::Update>,
    pub users: Vec<tl::enums::User>,
    pub chats: Vec<tl::enums::Chat>,
}

/// Поток обновлений учётки (одна на подключение).
pub trait Feed {
    /// Следующая пачка; связь пропала — `Network`.
    fn next(&mut self) -> BoxFuture<'_, Result<UpdateBatch, WireError>>;
    /// Состояние обновлений после отданных пачек — курсор канала.
    fn state(&mut self) -> BoxFuture<'_, UpdState>;
}

/// Подключение к Telegram: вызовы TL в байтах.
pub trait Wire {
    /// Сериализованная функция TL → сериализованный ответ; `dc` — `None`: домашний ДЦ.
    fn invoke(&self, dc: Option<i32>, body: Vec<u8>) -> BoxFuture<'_, Result<Vec<u8>, WireError>>;
    fn home_dc(&self) -> i32;
    /// Переезд домашнего ДЦ (`PHONE_MIGRATE_N`, `USER_MIGRATE_N`, вход по QR в другом ДЦ).
    fn set_home_dc(&self, dc: i32) -> BoxFuture<'_, ()>;
    /// Сессия для хранилища секретов ОС: ключи ДЦ и кто вошёл.
    fn blob(&self, user: i64) -> Blob;
    /// Ключи ДЦ изменились с прошлого вызова (новый ДЦ для файлов): секрет пора переписать.
    fn take_changed(&self) -> bool;
    /// Поток обновлений от состояния `state` (`None` — с текущего у сервера). Один раз на подключение.
    fn feed(&self, state: Option<UpdState>) -> BoxFuture<'_, Result<Box<dyn Feed>, WireError>>;
    /// Ждать `updateLoginToken` (QR отсканирован на другом устройстве) не дольше `wait`; `true` — пришло.
    fn login_token(&self, wait: Duration) -> BoxFuture<'_, bool>;
    /// Ключи доступа каналов для `getChannelDifference` (сервер не скажет их сам).
    fn remember_channel(&self, id: i64, access_hash: i64);
    /// Закрыть соединения.
    fn close(&self);
}

/// Как подключаться: настоящая сеть или подставной сервер.
pub trait Dial: Send + Sync {
    /// Подключение с сессией (`None` — новая, для входа).
    fn dial(&self, blob: Option<Blob>) -> BoxFuture<'_, Result<Rc<dyn Wire>, WireError>>;
    /// Ключ приложения (`api_id`, `api_hash`); `None` — сборка без ключа.
    fn api(&self) -> Option<(i32, String)>;
    /// Параметры SRP двухэтапной проверки годятся (`p` — безопасное простое, `g` — порождающий).
    fn srp_ok(&self, p: &[u8], g: i32) -> bool;
}

/// Вызов типизированной функции TL.
pub async fn call<R: RemoteCall>(w: &dyn Wire, request: &R) -> Result<R::Return, WireError> {
    call_in(w, None, request).await
}

/// Вызов в заданном ДЦ.
pub async fn call_in<R: RemoteCall>(w: &dyn Wire, dc: Option<i32>, request: &R) -> Result<R::Return, WireError> {
    let body = w.invoke(dc, request.to_bytes()).await?;
    R::Return::from_bytes(&body).map_err(|e| WireError::Protocol(format!("{e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_names_and_mapping() {
        let e = WireError::rpc(420, "FLOOD_WAIT_30");
        assert!(e.is("FLOOD_WAIT") && e.is("FLOOD_*"));
        assert_eq!(e.clone().channel(), ChannelError::Wait { secs: 30 });
        assert_eq!(WireError::rpc(401, "AUTH_KEY_UNREGISTERED").channel(), ChannelError::SignedOut("AUTH_KEY_UNREGISTERED".into()));
        assert_eq!(WireError::rpc(403, "CHAT_WRITE_FORBIDDEN").channel(), ChannelError::Denied("CHAT_WRITE_FORBIDDEN".into()));
        assert_eq!(WireError::rpc(400, "PEER_ID_INVALID").channel(), ChannelError::NotFound);
        assert_eq!(WireError::rpc(303, "PHONE_MIGRATE_2"), WireError::Rpc { code: 303, name: "PHONE_MIGRATE".into(), value: Some(2) });
        assert!(matches!(WireError::rpc(400, "SOMETHING_NEW").channel(), ChannelError::Protocol(_)));
        assert!(matches!(WireError::Network("reset".into()).channel(), ChannelError::Network(_)));
    }
}
