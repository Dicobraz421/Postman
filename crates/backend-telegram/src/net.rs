//! Настоящая сеть: MTProto — `grammers-mtsender` (соединения, ключи авторизации, повтор при коротком `FLOOD_WAIT`),
//! пропуски обновлений (`getDifference`, `getChannelDifference`) — `UpdatesReceiver` grammers. Сессия — в памяти
//! ([`VaultSession`]) из секрета ядра; файлов нет.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use grammers_mtsender::{ConnectionParams, InvocationError, SenderPool, SenderPoolFatHandle, UpdatesConfiguration, UpdatesReceiver};
use grammers_session::types::{ChannelKind, PeerAuth, PeerInfo};
use grammers_session::updates::UpdatesLike;
use grammers_tl_types::{self as tl, Cursor as TlCursor, Deserializable, RemoteCall, Serializable};
use postman_core::backend::BoxFuture;
use tokio::sync::mpsc;

use crate::vault::{Blob, UpdState, VaultSession};
use crate::wire::{Dial, Feed, UpdateBatch, Wire, WireError};

/// Сеть с ключом приложения.
pub struct NetDial {
    pub api_id: i32,
    pub api_hash: String,
}

/// Уже сериализованная функция TL: grammers отправит байты как есть и вернёт ответ байтами.
struct Raw(Vec<u8>);

struct RawReturn(Vec<u8>);

impl Serializable for Raw {
    fn serialize(&self, sink: &mut impl FnMut(&[u8])) {
        sink(&self.0);
    }
}

impl Deserializable for RawReturn {
    fn deserialize(buf: &mut TlCursor) -> tl::deserialize::Result<Self> {
        let mut out = Vec::new();
        buf.read_to_end(&mut out)?;
        Ok(RawReturn(out))
    }
}

impl RemoteCall for Raw {
    type Return = RawReturn;
}

fn wire_error(e: InvocationError) -> WireError {
    match e {
        InvocationError::Rpc(r) => WireError::Rpc { code: r.code, name: r.name, value: r.value },
        InvocationError::Io(e) => WireError::Network(e.kind().to_string()),
        InvocationError::Transport(e) => WireError::Network(e.to_string()),
        InvocationError::Dropped(why) => WireError::Network(format!("dropped: {why}")),
        InvocationError::Authentication(e) => WireError::Network(format!("auth key: {e}")),
        InvocationError::Deserialize(e) => WireError::Protocol(e.to_string()),
        InvocationError::InvalidDc => WireError::Protocol("invalid dc".into()),
        InvocationError::Session(e) => WireError::Protocol(format!("session: {e}")),
    }
}

fn params() -> ConnectionParams {
    ConnectionParams {
        device_model: "Postman".into(),
        system_version: std::env::consts::OS.into(),
        app_version: env!("CARGO_PKG_VERSION").into(),
        system_lang_code: "en".into(),
        lang_code: "en".into(),
        ..ConnectionParams::default()
    }
}

impl Dial for NetDial {
    fn dial(&self, blob: Option<Blob>) -> BoxFuture<'_, Result<Rc<dyn Wire>, WireError>> {
        Box::pin(async move {
            let session = Arc::new(VaultSession::new(blob.as_ref()));
            let SenderPool { runner, handle, updates } = SenderPool::with_configuration(Arc::clone(&session), self.api_id, params());
            let runner = tokio::spawn(runner.run());
            let wire: Rc<dyn Wire> = Rc::new(NetWire { session, handle, updates: RefCell::new(Some(updates)), runner });
            Ok(wire)
        })
    }

    fn api(&self) -> Option<(i32, String)> {
        Some((self.api_id, self.api_hash.clone()))
    }

    fn srp_ok(&self, p: &[u8], g: i32) -> bool {
        crate::srp_known_prime(p) || grammers_crypto::two_factor_auth::check_p_and_g(p, &g)
    }
}

struct NetWire {
    session: Arc<VaultSession>,
    handle: SenderPoolFatHandle,
    /// Сырые обновления сети до `feed` (вход по QR ждёт в них `updateLoginToken`).
    updates: RefCell<Option<mpsc::Receiver<UpdatesLike>>>,
    runner: tokio::task::JoinHandle<()>,
}

impl Drop for NetWire {
    fn drop(&mut self) {
        self.handle.quit();
        self.runner.abort();
    }
}

impl Wire for NetWire {
    fn invoke(&self, dc: Option<i32>, body: Vec<u8>) -> BoxFuture<'_, Result<Vec<u8>, WireError>> {
        Box::pin(async move {
            let dc = dc.unwrap_or_else(|| self.session.home());
            self.handle.invoke_in_dc(dc, &Raw(body)).await.map(|r| r.0).map_err(wire_error)
        })
    }

    fn home_dc(&self) -> i32 {
        self.session.home()
    }

    fn set_home_dc(&self, dc: i32) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            use grammers_session::Session as _;
            let old = self.session.home();
            if old != dc {
                self.handle.disconnect_from_dc(old);
                if self.session.set_home_dc_id(dc).await.is_err() {
                    tracing::warn!("telegram: home dc not changed");
                }
            }
        })
    }

    fn blob(&self, user: i64) -> Blob {
        self.session.blob(user)
    }

    fn take_changed(&self) -> bool {
        self.session.take_changed()
    }

    fn feed(&self, state: Option<UpdState>) -> BoxFuture<'_, Result<Box<dyn Feed>, WireError>> {
        Box::pin(async move {
            let Some(rx) = self.updates.borrow_mut().take() else { return Err(WireError::Protocol("updates already taken".into())) };
            if let Some(s) = &state {
                self.session.set_updates(s);
            }
            let receiver = UpdatesReceiver::create(
                self.handle.thin.clone(),
                Arc::clone(&self.handle.session),
                rx,
                UpdatesConfiguration { catch_up: state.is_some() },
            )
            .await
            .map_err(|e| WireError::Protocol(format!("updates: {e}")))?;
            let feed: Box<dyn Feed> = Box::new(NetFeed { receiver, session: Arc::clone(&self.session) });
            Ok(feed)
        })
    }

    fn login_token(&self, wait: Duration) -> BoxFuture<'_, bool> {
        Box::pin(async move {
            let Some(mut rx) = self.updates.borrow_mut().take() else { return false };
            let until = tokio::time::Instant::now() + wait;
            let mut got = false;
            while let Ok(Some(u)) = tokio::time::timeout_at(until, rx.recv()).await {
                if is_login_token(&u) {
                    got = true;
                    break;
                }
            }
            *self.updates.borrow_mut() = Some(rx);
            got
        })
    }

    fn remember_channel(&self, id: i64, access_hash: i64) {
        self.session.remember(PeerInfo::Channel { id, auth: Some(PeerAuth::from_hash(access_hash)), kind: None::<ChannelKind> });
    }

    fn close(&self) {
        self.handle.quit();
    }
}

fn is_login_token(u: &UpdatesLike) -> bool {
    match u {
        UpdatesLike::Updates(tl::enums::Updates::UpdateShort(s)) => matches!(s.update, tl::enums::Update::LoginToken),
        UpdatesLike::Updates(tl::enums::Updates::Updates(s)) => s.updates.iter().any(|u| matches!(u, tl::enums::Update::LoginToken)),
        _ => false,
    }
}

struct NetFeed {
    receiver: UpdatesReceiver,
    session: Arc<VaultSession>,
}

impl Feed for NetFeed {
    fn next(&mut self) -> BoxFuture<'_, Result<UpdateBatch, WireError>> {
        Box::pin(async move {
            let (updates, users, chats) = self.receiver.next().await.map_err(wire_error)?;
            Ok(UpdateBatch { updates: updates.into_iter().map(|(u, _)| u).collect(), users, chats })
        })
    }

    fn state(&mut self) -> BoxFuture<'_, UpdState> {
        Box::pin(async move {
            if let Err(e) = self.receiver.sync_update_state().await {
                tracing::warn!("telegram: update state not synced: {e}");
            }
            self.session.updates()
        })
    }
}
