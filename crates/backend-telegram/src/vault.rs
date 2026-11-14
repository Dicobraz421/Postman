//! Что канал отдаёт ядру на хранение: сессия — секретом в хранилище ОС ([`Blob`]: ключи авторизации ДЦ, домашний
//! ДЦ, кто вошёл), состояние обновлений — курсором в базе профиля ([`UpdState`]: `pts`/`qts`/`date`/`seq` и `pts`
//! каналов). Файлов сессии на диске нет (решение «всё в базе»). [`VaultSession`] — хранилище grammers в памяти над
//! ними: подключение берёт из него ключи ДЦ и пишет новые, ядро переписывает секрет по `ChannelEvent::Secret`.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use grammers_session::types::{ChannelState, DcOption, PeerId, PeerInfo, UpdateState, UpdatesState};
use grammers_session::{BoxFuture, Session, SessionData};
use postman_core::channel::{Cursor, Secret};

const BLOB_MAGIC: &[u8; 4] = b"PTG1";
const CURSOR_MAGIC: &[u8; 4] = b"PTU1";

/// Ключ авторизации и адрес одного ДЦ.
#[derive(Clone, PartialEq, Eq)]
pub struct DcKey {
    pub id: i32,
    pub ipv4: SocketAddrV4,
    pub ipv6: SocketAddrV6,
    pub key: Option<[u8; 256]>,
}

impl std::fmt::Debug for DcKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DcKey({}, key: {})", self.id, self.key.is_some())
    }
}

/// Сессия Telegram в секрете ядра.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Blob {
    pub home_dc: i32,
    /// Кто вошёл (id пользователя); `0` — ещё никто (вход идёт).
    pub user: i64,
    pub dcs: Vec<DcKey>,
}

fn put_i32(out: &mut Vec<u8>, v: i32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_i64(out: &mut Vec<u8>, v: i64) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Чтение без паники: кончились байты — `None`.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Some(head)
    }

    fn i32(&mut self) -> Option<i32> {
        Some(i32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }

    fn u8(&mut self) -> Option<u8> {
        Some(*self.take(1)?.first()?)
    }
}

impl Blob {
    pub fn to_secret(&self) -> Secret {
        let mut out = Vec::with_capacity(16 + self.dcs.len() * 300);
        out.extend_from_slice(BLOB_MAGIC);
        put_i32(&mut out, self.home_dc);
        put_i64(&mut out, self.user);
        out.push(u8::try_from(self.dcs.len()).unwrap_or(u8::MAX));
        for dc in self.dcs.iter().take(usize::from(u8::MAX)) {
            put_i32(&mut out, dc.id);
            out.extend_from_slice(&dc.ipv4.ip().octets());
            out.extend_from_slice(&dc.ipv4.port().to_le_bytes());
            out.extend_from_slice(&dc.ipv6.ip().octets());
            out.extend_from_slice(&dc.ipv6.port().to_le_bytes());
            match &dc.key {
                Some(k) => {
                    out.push(1);
                    out.extend_from_slice(k);
                }
                None => out.push(0),
            }
        }
        Secret::new(out)
    }

    /// Секрет этого канала; чужие байты — `None` (ядро скажет «вход истёк»).
    pub fn from_secret(s: &Secret) -> Option<Blob> {
        let mut r = Reader(s.expose());
        if r.take(4)? != BLOB_MAGIC {
            return None;
        }
        let home_dc = r.i32()?;
        let user = r.i64()?;
        let n = r.u8()?;
        let mut dcs = Vec::with_capacity(usize::from(n));
        for _ in 0..n {
            let id = r.i32()?;
            let v4: [u8; 4] = r.take(4)?.try_into().ok()?;
            let p4 = r.u16()?;
            let v6: [u8; 16] = r.take(16)?.try_into().ok()?;
            let p6 = r.u16()?;
            let key = match r.u8()? {
                0 => None,
                1 => Some(r.take(256)?.try_into().ok()?),
                _ => return None,
            };
            dcs.push(DcKey { id, ipv4: SocketAddrV4::new(Ipv4Addr::from(v4), p4), ipv6: SocketAddrV6::new(Ipv6Addr::from(v6), p6, 0, 0), key });
        }
        if !r.0.is_empty() || dcs.iter().all(|d| d.key.is_none()) {
            return None;
        }
        Some(Blob { home_dc, user, dcs })
    }
}

/// Состояние обновлений — курсор канала.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UpdState {
    pub pts: i32,
    pub qts: i32,
    pub date: i32,
    pub seq: i32,
    /// `pts` каналов и супергрупп: (id канала, pts).
    pub channels: Vec<(i64, i32)>,
}

impl UpdState {
    pub fn to_cursor(&self) -> Cursor {
        let mut out = Vec::with_capacity(24 + self.channels.len() * 12);
        out.extend_from_slice(CURSOR_MAGIC);
        for v in [self.pts, self.qts, self.date, self.seq] {
            put_i32(&mut out, v);
        }
        put_i32(&mut out, i32::try_from(self.channels.len()).unwrap_or(i32::MAX));
        for (id, pts) in &self.channels {
            put_i64(&mut out, *id);
            put_i32(&mut out, *pts);
        }
        Cursor(out)
    }

    pub fn from_cursor(c: &Cursor) -> Option<UpdState> {
        let mut r = Reader(&c.0);
        if r.take(4)? != CURSOR_MAGIC {
            return None;
        }
        let (pts, qts, date, seq) = (r.i32()?, r.i32()?, r.i32()?, r.i32()?);
        let n = usize::try_from(r.i32()?).ok()?;
        let mut channels = Vec::with_capacity(n.min(10_000));
        for _ in 0..n {
            channels.push((r.i64()?, r.i32()?));
        }
        Some(UpdState { pts, qts, date, seq, channels })
    }

    pub fn from_grammers(s: &UpdatesState) -> Self {
        UpdState { pts: s.pts, qts: s.qts, date: s.date, seq: s.seq, channels: s.channels.iter().map(|c| (c.id, c.pts)).collect() }
    }

    pub fn to_grammers(&self) -> UpdatesState {
        UpdatesState {
            pts: self.pts,
            qts: self.qts,
            date: self.date,
            seq: self.seq,
            channels: self.channels.iter().map(|&(id, pts)| ChannelState { id, pts }).collect(),
        }
    }
}

/// Сколько ДЦ с ключами в секрете.
pub const MAX_KEYED_DCS: usize = 4;

/// Хранилище сессии grammers в памяти над секретом ядра: ключи ДЦ — из [`Blob`], новые ключи помечают сессию
/// изменённой ([`VaultSession::take_changed`]), ядро перепишет секрет. Люди и каналы (ключи доступа) — только в памяти:
/// их постоянная копия — у канала в базе профиля (`PluginStore`).
pub struct VaultSession {
    data: Mutex<SessionData>,
    user: Mutex<Option<PeerInfo>>,
    changed: AtomicBool,
}

#[derive(Debug, thiserror::Error)]
#[error("session lock is poisoned")]
pub struct Poisoned;

impl VaultSession {
    pub fn new(blob: Option<&Blob>) -> Self {
        let mut data = SessionData::default();
        let mut user = None;
        if let Some(b) = blob {
            data.home_dc = b.home_dc;
            for dc in &b.dcs {
                data.dc_options.insert(dc.id, DcOption { id: dc.id, ipv4: dc.ipv4, ipv6: dc.ipv6, auth_key: dc.key });
            }
            if b.user != 0 {
                user = Some(PeerInfo::User { id: b.user, auth: None, bot: Some(false), is_self: Some(true) });
            }
        }
        Self { data: Mutex::new(data), user: Mutex::new(user), changed: AtomicBool::new(false) }
    }

    /// Секрет — только ДЦ с ключами, домашний первым, не больше [`MAX_KEYED_DCS`]: запись хранилища ОС невелика
    /// (Windows — 2560 байт, секрет в нём — шестнадцатеричной строкой). Адреса прочих ДЦ grammers знает сам.
    pub fn blob(&self, user: i64) -> Blob {
        let Ok(d) = self.data.lock() else { return Blob::default() };
        let mut dcs: Vec<DcKey> =
            d.dc_options.values().filter(|o| o.auth_key.is_some()).map(|o| DcKey { id: o.id, ipv4: o.ipv4, ipv6: o.ipv6, key: o.auth_key }).collect();
        dcs.sort_by_key(|x| (x.id != d.home_dc, x.id));
        dcs.truncate(MAX_KEYED_DCS);
        Blob { home_dc: d.home_dc, user, dcs }
    }

    pub fn take_changed(&self) -> bool {
        self.changed.swap(false, Ordering::Relaxed)
    }

    pub fn home(&self) -> i32 {
        self.data.lock().map(|d| d.home_dc).unwrap_or(grammers_default_dc())
    }

    /// Вошли: кто я (нужен `UpdatesReceiver`, чтобы спросить состояние у сервера).
    pub fn set_user(&self, id: i64) {
        if let Ok(mut u) = self.user.lock() {
            *u = Some(PeerInfo::User { id, auth: None, bot: Some(false), is_self: Some(true) });
        }
    }

    /// Ключ доступа канала: grammers спрашивает его для `getChannelDifference`.
    pub fn remember(&self, info: PeerInfo) {
        if let Ok(mut d) = self.data.lock() {
            d.peer_infos.entry(info.id()).or_insert_with(|| info.clone()).extend_info(&info);
        }
    }

    pub fn set_updates(&self, s: &UpdState) {
        if let Ok(mut d) = self.data.lock() {
            d.updates_state = s.to_grammers();
        }
    }

    pub fn updates(&self) -> UpdState {
        self.data.lock().map(|d| UpdState::from_grammers(&d.updates_state)).unwrap_or_default()
    }
}

fn grammers_default_dc() -> i32 {
    SessionData::default().home_dc
}

impl Session for VaultSession {
    type Error = Poisoned;

    fn home_dc_id(&self) -> Result<i32, Poisoned> {
        Ok(self.data.lock().map_err(|_| Poisoned)?.home_dc)
    }

    fn set_home_dc_id(&self, dc_id: i32) -> BoxFuture<'_, Result<(), Poisoned>> {
        Box::pin(async move {
            let mut d = self.data.lock().map_err(|_| Poisoned)?;
            if d.home_dc != dc_id {
                d.home_dc = dc_id;
                self.changed.store(true, Ordering::Relaxed);
            }
            Ok(())
        })
    }

    fn dc_option(&self, dc_id: i32) -> Result<Option<DcOption>, Poisoned> {
        Ok(self.data.lock().map_err(|_| Poisoned)?.dc_options.get(&dc_id).cloned())
    }

    fn set_dc_option(&self, dc_option: &DcOption) -> BoxFuture<'_, Result<(), Poisoned>> {
        let dc_option = dc_option.clone();
        Box::pin(async move {
            let mut d = self.data.lock().map_err(|_| Poisoned)?;
            // Новый адрес ДЦ приходит с каждым подключением (`help.getConfig`) — секрет меняется только с ключом.
            let key_changed = d.dc_options.get(&dc_option.id).map(|o| o.auth_key) != Some(dc_option.auth_key);
            d.dc_options.insert(dc_option.id, dc_option);
            if key_changed {
                self.changed.store(true, Ordering::Relaxed);
            }
            Ok(())
        })
    }

    fn peer(&self, peer: PeerId) -> BoxFuture<'_, Result<Option<PeerInfo>, Poisoned>> {
        Box::pin(async move {
            if peer == PeerId::self_user() {
                return Ok(self.user.lock().map_err(|_| Poisoned)?.clone());
            }
            Ok(self.data.lock().map_err(|_| Poisoned)?.peer_infos.get(&peer).cloned())
        })
    }

    fn cache_peer(&self, peer: PeerInfo) -> BoxFuture<'_, Result<(), Poisoned>> {
        Box::pin(async move {
            if let PeerInfo::User { is_self: Some(true), id, .. } = &peer {
                self.set_user(*id);
            }
            self.remember(peer);
            Ok(())
        })
    }

    fn updates_state(&self) -> BoxFuture<'_, Result<UpdatesState, Poisoned>> {
        Box::pin(async move { Ok(self.data.lock().map_err(|_| Poisoned)?.updates_state.clone()) })
    }

    fn set_update_state(&self, update: UpdateState) -> BoxFuture<'_, Result<(), Poisoned>> {
        Box::pin(async move {
            let mut d = self.data.lock().map_err(|_| Poisoned)?;
            let s = &mut d.updates_state;
            match update {
                UpdateState::All(all) => *s = all,
                UpdateState::Primary { pts, date, seq } => {
                    s.pts = pts;
                    s.date = date;
                    s.seq = seq;
                }
                UpdateState::Secondary { qts } => s.qts = qts,
                UpdateState::Channel { id, pts } => {
                    s.channels.retain(|c| c.id != id);
                    s.channels.push(ChannelState { id, pts });
                }
            }
            Ok(())
        })
    }
}

/// Сколько записей в словаре — для проверок.
#[cfg(test)]
fn dc_count(s: &VaultSession) -> usize {
    s.data.lock().map(|d| d.dc_options.len()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob() -> Blob {
        Blob {
            home_dc: 2,
            user: 9_990_001,
            dcs: vec![
                DcKey {
                    id: 1,
                    ipv4: SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 1), 443),
                    ipv6: SocketAddrV6::new(Ipv6Addr::LOCALHOST, 443, 0, 0),
                    key: None,
                },
                DcKey {
                    id: 2,
                    ipv4: SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 2), 443),
                    ipv6: SocketAddrV6::new(Ipv6Addr::LOCALHOST, 443, 0, 0),
                    key: Some([7; 256]),
                },
            ],
        }
    }

    fn hex_len(b: &Blob) -> usize {
        b.to_secret().expose().len() * 2
    }

    /// Секрет — туда и обратно; чужие байты, обрезанный и без ключей — не сессия.
    #[test]
    fn blob_round_trip() {
        let b = blob();
        let s = b.to_secret();
        assert_eq!(Blob::from_secret(&s), Some(b.clone()));
        assert_eq!(Blob::from_secret(&Secret::new(b"not-a-session-of-this-channel".to_vec())), None);
        let cut = Secret::new(s.expose()[..s.expose().len() - 1].to_vec());
        assert_eq!(Blob::from_secret(&cut), None);
        let keyless = Blob { dcs: vec![b.dcs[0].clone()], ..b.clone() };
        assert_eq!(Blob::from_secret(&keyless.to_secret()), None);
        assert!(!format!("{:?}", b).contains("7, 7"), "ключ не в журнале");
    }

    #[test]
    fn cursor_round_trip() {
        let u = UpdState { pts: 10, qts: 2, date: 1_700_000_000, seq: 5, channels: vec![(1001, 77), (1002, 3)] };
        assert_eq!(UpdState::from_cursor(&u.to_cursor()), Some(u.clone()));
        assert_eq!(UpdState::from_cursor(&Cursor(b"junk".to_vec())), None);
        assert_eq!(UpdState::from_grammers(&u.to_grammers()), u);
    }

    /// Новый ключ ДЦ — сессия изменилась; тот же — нет; кто я — для `UpdatesReceiver`.
    #[test]
    fn vault_session_tracks_changes() {
        let s = VaultSession::new(Some(&blob()));
        assert_eq!(s.home(), 2);
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        rt.block_on(async {
            assert!(s.peer(PeerId::self_user()).await.unwrap().is_some());
            let mut dc = s.dc_option(4).unwrap().unwrap_or(DcOption {
                id: 4,
                ipv4: SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 4), 443),
                ipv6: SocketAddrV6::new(Ipv6Addr::LOCALHOST, 443, 0, 0),
                auth_key: None,
            });
            assert!(!s.take_changed());
            dc.auth_key = Some([9; 256]);
            s.set_dc_option(&dc).await.unwrap();
            assert!(s.take_changed() && !s.take_changed());
            s.set_dc_option(&dc).await.unwrap();
            assert!(!s.take_changed());
        });
        let b = s.blob(1);
        assert!(b.dcs.iter().any(|d| d.id == 4 && d.key.is_some()));
        assert!(b.dcs.iter().all(|d| d.key.is_some()) && b.dcs[0].id == 2, "только ключи, домашний первым");
        assert!(hex_len(&b) <= 2560);
        assert!(dc_count(&s) >= 2);
    }
}
