//! Каналы бесед в базе профиля (ADR 0012 п. 9): учётки каналов, беседы, сообщения, история правок, участники.
//! Таблицы добавочные — создаются при открытии, формат остаётся 7, миграции нет (как `local`).
//!
//! Хранилище знает ключи, индексы и ссылки на части, но не модель: значения бесед, сообщений и участников — байты,
//! их формат знает ядро (`postman_core::channel`). Так канал не видит хранилища, а хранилище — протокола.
//!
//! - Запись сообщения идемпотентна по id сервера (`msg_remote`): то же сообщение заново (повтор после сбоя,
//!   расшифровалось позже) — та же строка; с новым временем правки — прежняя версия уходит в `msg_edit`.
//! - Удаление без беседы — по `msg_by_id` (учётка, id сообщения → беседа): у Telegram номер сообщения в личных и
//!   группах уникален на учётку, а событие удаления беседу не называет.
//! - Вложения — части хранилища содержимого по SHA-256 раскодированных байтов (те же, что у писем и `local`):
//!   загрузка пишется кусками во временную таблицу и одной транзакцией переносится под хэш ([`ChanDownload`]).
//! - Пишет ядро пачкой ([`ChanBatch`]): события канала и курсор — одной транзакцией.

use std::collections::HashMap;
use std::sync::Arc;

use redb::{Database, ReadTransaction, ReadableDatabase, ReadableTable, TableDefinition, WriteTransaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::content::{self, CHUNK, Hash, Packed};
use crate::letter::next_id;
use crate::{Cache, Result, dec, enc};

/// Учётка канала → [`AccountRow`].
const CHAN_ACCOUNT: TableDefinition<u64, &[u8]> = TableDefinition::new("chan_account");
/// Беседа → [`ConvRow`].
const CONV: TableDefinition<u64, &[u8]> = TableDefinition::new("conv");
/// (учётка, id беседы на сервере) → беседа.
const CONV_REMOTE: TableDefinition<(u64, &str), u64> = TableDefinition::new("conv_remote");
/// (−время последнего сообщения, беседа): список бесед всех каналов, новые сверху.
const CONV_BY_TIME: TableDefinition<(i64, u64), ()> = TableDefinition::new("conv_by_time");
/// Сообщение → [`MsgRow`].
const MSG: TableDefinition<u64, &[u8]> = TableDefinition::new("msg");
/// (беседа, id сообщения на сервере) → сообщение.
const MSG_REMOTE: TableDefinition<(u64, &str), u64> = TableDefinition::new("msg_remote");
/// (учётка, id сообщения на сервере) → беседа: удаление без беседы.
const MSG_BY_ID: TableDefinition<(u64, &str), u64> = TableDefinition::new("msg_by_id");
/// (беседа, время, сообщение): лента.
const BY_CONV: TableDefinition<(u64, i64, u64), ()> = TableDefinition::new("by_conv");
/// (сообщение, номер) → прежняя версия ([`EditRow`]).
const MSG_EDIT: TableDefinition<(u64, u32), &[u8]> = TableDefinition::new("msg_edit");
/// (учётка, id участника) → участник.
const PARTICIPANT: TableDefinition<(u64, &str), &[u8]> = TableDefinition::new("participant");
/// (беседа, id участника) → роль.
const MEMBER: TableDefinition<(u64, &str), &[u8]> = TableDefinition::new("member");
/// (загрузка, номер куска) → куски загрузки до переноса под хэш; при открытии базы — пусто.
const CHAN_TMP: TableDefinition<(u64, u32), &[u8]> = TableDefinition::new("chan_tmp");
/// Очередь отправки (ADR 0012 п. 9, 11): `(учётка, client_id)` → исходящее, шаг и срок — значение ядра; переживает
/// перезапуск.
const CHAN_OUTBOX: TableDefinition<(u64, u64), &[u8]> = TableDefinition::new("chan_outbox");

const NEXT_CHAN: &str = "next_chan";
const NEXT_CONV: &str = "next_conv";
const NEXT_MSG: &str = "next_msg";
const NEXT_TMP: &str = "next_chan_tmp";

/// Значение сообщения от этой длины хранится сжатым zstd (ADR 0012 п. 9).
pub(crate) const PACK_FROM: usize = 1024;
/// «Выйти» из учётки: сообщений за транзакцию (как перенос писем).
pub const FORGET_BATCH: usize = 100_000;

/// Учётка канала в профиле (счётчик `meta.next_chan`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ChanKey(pub u64);

/// Беседа (счётчик `meta.next_conv`): только внутри профиля, наружу — адрес из id сервера.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ConvKey(pub u64);

/// Сообщение (счётчик `meta.next_msg`): только внутри профиля.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MsgKey(pub u64);

#[derive(Serialize, Deserialize)]
struct AccountRow {
    channel: String,
    account: String,
    cursor: Option<Vec<u8>>,
    #[serde(with = "crate::content::bytes")]
    value: Vec<u8>,
}

/// Учётка канала: канал, id учётки в нём, курсор и значение ядра (имя, возможности, состояние).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChanAccount {
    pub key: ChanKey,
    pub channel: String,
    pub account: String,
    pub cursor: Option<Vec<u8>>,
    pub value: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct ConvRow {
    account: u64,
    remote: String,
    last: i64,
    #[serde(with = "crate::content::bytes")]
    value: Vec<u8>,
}

/// Беседа: учётка, id сервера, время последнего сообщения и значение ядра.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvDoc {
    pub key: ConvKey,
    pub account: ChanKey,
    pub remote: String,
    pub last: i64,
    pub value: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct MsgRow {
    conv: u64,
    remote: String,
    sent: i64,
    edited: Option<i64>,
    value: Packed,
    /// Загруженные вложения: источник канала → часть.
    loaded: Vec<(Vec<u8>, Hash)>,
}

/// Сообщение: беседа, id сервера, время отправки и правки, значение ядра, загруженные вложения (источник → часть).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MsgDoc {
    pub key: MsgKey,
    pub conv: ConvKey,
    pub remote: String,
    pub sent: i64,
    pub edited: Option<i64>,
    pub value: Vec<u8>,
    pub loaded: Vec<(Vec<u8>, Hash)>,
}

#[derive(Serialize, Deserialize)]
struct EditRow {
    edited: Option<i64>,
    value: Packed,
}

/// Что записать о сообщении: id сервера, время, время правки, значение ядра и источники его вложений (загруженные
/// вложения с другим источником — снимаются).
#[derive(Debug, Clone, Copy)]
pub struct MsgPut<'a> {
    pub remote: &'a str,
    pub sent: i64,
    pub edited: Option<i64>,
    pub value: &'a [u8],
    pub sources: &'a [Vec<u8>],
}

/// Чем стала запись сообщения ([`ChanBatch::put_msg`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stored {
    /// Нового id — новая строка.
    New,
    /// То же сообщение теми же байтами: ничего не изменилось.
    Same,
    /// То же сообщение иначе без правки (расшифровалось, реакции, отметки): строка заменена, истории нет.
    Replaced,
    /// Правка (новое время правки): прежняя версия — в истории правок.
    Edited,
}

pub(crate) fn init(tx: &WriteTransaction) -> Result<()> {
    tx.open_table(CHAN_ACCOUNT)?;
    tx.open_table(CONV)?;
    tx.open_table(CONV_REMOTE)?;
    tx.open_table(CONV_BY_TIME)?;
    tx.open_table(MSG)?;
    tx.open_table(MSG_REMOTE)?;
    tx.open_table(MSG_BY_ID)?;
    tx.open_table(BY_CONV)?;
    tx.open_table(MSG_EDIT)?;
    tx.open_table(PARTICIPANT)?;
    tx.open_table(MEMBER)?;
    tx.open_table(CHAN_OUTBOX)?;
    // Куски загрузок, прерванных выходом или сбоем, — мусор: загрузка начнётся заново.
    tx.open_table(CHAN_TMP)?.retain(|_, _| false)?;
    Ok(())
}

/// Вхождения частей из сообщений — в `want` (сверка счётчиков, [`Cache::check_refs`]).
pub(crate) fn count_parts(tx: &ReadTransaction, want: &mut HashMap<Hash, u32>) -> Result<()> {
    for r in tx.open_table(MSG)?.iter()? {
        for (_, h) in dec::<MsgRow>(r?.1.value())?.loaded {
            *want.entry(h).or_default() += 1;
        }
    }
    Ok(())
}

fn pack(value: &[u8], level: Option<i32>) -> Packed {
    Packed::new(value.to_vec(), level.filter(|_| value.len() >= PACK_FROM))
}

fn account_doc(key: u64, row: AccountRow) -> ChanAccount {
    ChanAccount { key: ChanKey(key), channel: row.channel, account: row.account, cursor: row.cursor, value: row.value }
}

fn conv_doc(key: u64, row: ConvRow) -> ConvDoc {
    ConvDoc { key: ConvKey(key), account: ChanKey(row.account), remote: row.remote, last: row.last, value: row.value }
}

fn msg_doc(key: u64, row: MsgRow) -> Result<MsgDoc> {
    Ok(MsgDoc {
        key: MsgKey(key),
        conv: ConvKey(row.conv),
        remote: row.remote,
        sent: row.sent,
        edited: row.edited,
        value: row.value.into_bytes()?,
        loaded: row.loaded,
    })
}

/// Время в ключе списка бесед: новые — первыми.
fn neg(t: i64) -> i64 {
    t.saturating_neg()
}

/// Ключ `(учётка, id)` и все строки этой учётки: `(key, "")..(key + 1, "")`.
fn of_account(key: u64) -> std::ops::Range<(u64, &'static str)> {
    (key, "")..(key.saturating_add(1), "")
}

/// Записи событий канала одной транзакцией ([`Cache::chan_batch`]): ядро копит события (≤ 100 мс или ≤ 500) и курсор
/// и пишет их вместе. Пока пачка открыта, другие записи в базу ждут — пачку не держат открытой во время сети.
pub struct ChanBatch {
    tx: WriteTransaction,
    level: Option<i32>,
}

impl ChanBatch {
    pub fn commit(self) -> Result<()> {
        self.tx.commit()?;
        Ok(())
    }

    // --- учётки ---

    /// Учётка `account` канала `channel`: есть — её ключ (значение заменяется), нет — новая.
    pub fn add_account(&self, channel: &str, account: &str, value: &[u8]) -> Result<ChanKey> {
        let mut t = self.tx.open_table(CHAN_ACCOUNT)?;
        let mut found = None;
        for r in t.iter()? {
            let (k, v) = r?;
            let row: AccountRow = dec(v.value())?;
            if row.channel == channel && row.account == account {
                found = Some((k.value(), row));
                break;
            }
        }
        let (key, row) = match found {
            Some((k, row)) => (k, AccountRow { value: value.to_vec(), ..row }),
            None => {
                let k = next_id(&self.tx, NEXT_CHAN)?;
                (k, AccountRow { channel: channel.to_owned(), account: account.to_owned(), cursor: None, value: value.to_vec() })
            }
        };
        t.insert(key, enc(&row)?.as_slice())?;
        Ok(ChanKey(key))
    }

    pub fn account(&self, key: ChanKey) -> Result<Option<ChanAccount>> {
        let t = self.tx.open_table(CHAN_ACCOUNT)?;
        t.get(key.0)?.map(|v| dec::<AccountRow>(v.value()).map(|r| account_doc(key.0, r))).transpose()
    }

    fn update_account(&self, key: ChanKey, f: impl FnOnce(&mut AccountRow)) -> Result<bool> {
        let mut t = self.tx.open_table(CHAN_ACCOUNT)?;
        let Some(mut row) = t.get(key.0)?.map(|v| dec::<AccountRow>(v.value())).transpose()? else { return Ok(false) };
        f(&mut row);
        t.insert(key.0, enc(&row)?.as_slice())?;
        Ok(true)
    }

    /// Значение ядра у учётки (имя, возможности, состояние). Учётки нет — `false`.
    pub fn set_account_value(&self, key: ChanKey, value: &[u8]) -> Result<bool> {
        self.update_account(key, |r| r.value = value.to_vec())
    }

    /// Курсор учётки: всё до него передано и записано этой же транзакцией.
    pub fn set_cursor(&self, key: ChanKey, cursor: &[u8]) -> Result<bool> {
        self.update_account(key, |r| r.cursor = Some(cursor.to_vec()))
    }

    // --- беседы ---

    pub fn conv_key(&self, account: ChanKey, remote: &str) -> Result<Option<ConvKey>> {
        Ok(self.tx.open_table(CONV_REMOTE)?.get((account.0, remote))?.map(|v| ConvKey(v.value())))
    }

    pub fn conv(&self, key: ConvKey) -> Result<Option<ConvDoc>> {
        let t = self.tx.open_table(CONV)?;
        t.get(key.0)?.map(|v| dec::<ConvRow>(v.value()).map(|r| conv_doc(key.0, r))).transpose()
    }

    /// Беседа по id сервера: есть — значение заменяется (время последнего сообщения остаётся), нет — новая.
    pub fn put_conv(&self, account: ChanKey, remote: &str, value: &[u8]) -> Result<ConvKey> {
        let (key, last) = match self.conv_key(account, remote)? {
            Some(k) => (k, self.conv(k)?.map_or(0, |c| c.last)),
            None => {
                let k = ConvKey(next_id(&self.tx, NEXT_CONV)?);
                self.tx.open_table(CONV_REMOTE)?.insert((account.0, remote), k.0)?;
                self.tx.open_table(CONV_BY_TIME)?.insert((neg(0), k.0), ())?;
                (k, 0)
            }
        };
        let row = ConvRow { account: account.0, remote: remote.to_owned(), last, value: value.to_vec() };
        self.tx.open_table(CONV)?.insert(key.0, enc(&row)?.as_slice())?;
        Ok(key)
    }

    /// Время последнего сообщения беседы — не раньше `at` (место в списке бесед).
    pub fn touch_conv(&self, key: ConvKey, at: i64) -> Result<()> {
        let mut t = self.tx.open_table(CONV)?;
        let Some(mut row) = t.get(key.0)?.map(|v| dec::<ConvRow>(v.value())).transpose()? else { return Ok(()) };
        if at <= row.last {
            return Ok(());
        }
        let mut by_time = self.tx.open_table(CONV_BY_TIME)?;
        by_time.remove((neg(row.last), key.0))?;
        by_time.insert((neg(at), key.0), ())?;
        row.last = at;
        t.insert(key.0, enc(&row)?.as_slice())?;
        Ok(())
    }

    /// Беседа со всеми сообщениями, правками и участниками. Ответ — сколько сообщений удалено.
    pub fn remove_conv(&self, key: ConvKey) -> Result<usize> {
        let msgs: Vec<u64> = {
            let t = self.tx.open_table(BY_CONV)?;
            t.range((key.0, i64::MIN, 0)..=(key.0, i64::MAX, u64::MAX))?
                .map(|r| r.map(|(k, _)| k.value().2))
                .collect::<std::result::Result<_, _>>()?
        };
        for m in &msgs {
            self.remove_msg(MsgKey(*m))?;
        }
        {
            let mut members = self.tx.open_table(MEMBER)?;
            let ids: Vec<String> =
                members.range(of_account(key.0))?.map(|r| r.map(|(k, _)| k.value().1.to_owned())).collect::<std::result::Result<_, _>>()?;
            for id in ids {
                members.remove((key.0, id.as_str()))?;
            }
        }
        let Some(row) = self.tx.open_table(CONV)?.remove(key.0)?.map(|v| dec::<ConvRow>(v.value())).transpose()? else { return Ok(msgs.len()) };
        self.tx.open_table(CONV_REMOTE)?.remove((row.account, row.remote.as_str()))?;
        self.tx.open_table(CONV_BY_TIME)?.remove((neg(row.last), key.0))?;
        Ok(msgs.len())
    }

    // --- сообщения ---

    pub fn msg_key(&self, conv: ConvKey, remote: &str) -> Result<Option<MsgKey>> {
        Ok(self.tx.open_table(MSG_REMOTE)?.get((conv.0, remote))?.map(|v| MsgKey(v.value())))
    }

    pub fn msg(&self, key: MsgKey) -> Result<Option<MsgDoc>> {
        let row = self.tx.open_table(MSG)?.get(key.0)?.map(|v| dec::<MsgRow>(v.value())).transpose()?;
        row.map(|r| msg_doc(key.0, r)).transpose()
    }

    /// Беседа сообщения по id сервера в учётке (удаление без беседы); нет — `None`.
    pub fn conv_of_id(&self, account: ChanKey, remote: &str) -> Result<Option<ConvKey>> {
        Ok(self.tx.open_table(MSG_BY_ID)?.get((account.0, remote))?.map(|v| ConvKey(v.value())))
    }

    /// Сообщение в беседе `conv` по id сервера — идемпотентно: тот же id — та же строка; новое время правки — прежняя
    /// версия в историю правок. Время последнего сообщения беседы растёт. Беседы нет — ошибка.
    pub fn put_msg(&self, conv: ConvKey, put: &MsgPut) -> Result<(MsgKey, Stored)> {
        let Some(c) = self.conv(conv)? else { return Err(crate::Error(format!("conversation {} is not stored", conv.0))) };
        let known = self.msg_key(conv, put.remote)?;
        let old = match known {
            Some(k) => self.tx.open_table(MSG)?.get(k.0)?.map(|v| dec::<MsgRow>(v.value())).transpose()?,
            None => None,
        };
        let (key, stored, loaded) = match (known, old) {
            (Some(k), Some(old)) => {
                let edited = put.edited.is_some() && put.edited != old.edited;
                let (keep, drop): (Vec<_>, Vec<_>) = old.loaded.into_iter().partition(|(src, _)| put.sources.contains(src));
                let same =
                    !edited && drop.is_empty() && old.sent == put.sent && old.edited == put.edited && old.value.clone().into_bytes()? == put.value;
                content::release_parts(&self.tx, drop.into_iter().map(|(_, h)| h))?;
                if old.sent != put.sent {
                    let mut by = self.tx.open_table(BY_CONV)?;
                    by.remove((conv.0, old.sent, k.0))?;
                    by.insert((conv.0, put.sent, k.0), ())?;
                }
                if edited {
                    let mut edits = self.tx.open_table(MSG_EDIT)?;
                    let n = edits.range((k.0, 0)..=(k.0, u32::MAX))?.next_back().transpose()?.map_or(0, |(k, _)| k.value().1 + 1);
                    edits.insert((k.0, n), enc(&EditRow { edited: old.edited, value: old.value })?.as_slice())?;
                }
                let stored = if edited {
                    Stored::Edited
                } else if same {
                    Stored::Same
                } else {
                    Stored::Replaced
                };
                (k, stored, keep)
            }
            _ => {
                let k = MsgKey(next_id(&self.tx, NEXT_MSG)?);
                self.tx.open_table(MSG_REMOTE)?.insert((conv.0, put.remote), k.0)?;
                self.tx.open_table(BY_CONV)?.insert((conv.0, put.sent, k.0), ())?;
                (k, Stored::New, Vec::new())
            }
        };
        if stored != Stored::Same {
            let row = MsgRow {
                conv: conv.0,
                remote: put.remote.to_owned(),
                sent: put.sent,
                edited: put.edited,
                value: pack(put.value, self.level),
                loaded,
            };
            self.tx.open_table(MSG)?.insert(key.0, enc(&row)?.as_slice())?;
        }
        self.tx.open_table(MSG_BY_ID)?.insert((c.account.0, put.remote), conv.0)?;
        self.touch_conv(conv, put.sent)?;
        Ok((key, stored))
    }

    /// Значение сообщения без истории правок (реакции, отметки). Сообщения нет — `false`.
    pub fn set_msg_value(&self, key: MsgKey, value: &[u8]) -> Result<bool> {
        let mut t = self.tx.open_table(MSG)?;
        let Some(mut row) = t.get(key.0)?.map(|v| dec::<MsgRow>(v.value())).transpose()? else { return Ok(false) };
        row.value = pack(value, self.level);
        t.insert(key.0, enc(&row)?.as_slice())?;
        Ok(true)
    }

    /// Сообщение уходит со своими правками; вхождения его частей убывают. Не было — `false`.
    pub fn remove_msg(&self, key: MsgKey) -> Result<bool> {
        let Some(row) = self.tx.open_table(MSG)?.remove(key.0)?.map(|v| dec::<MsgRow>(v.value())).transpose()? else { return Ok(false) };
        self.tx.open_table(MSG_REMOTE)?.remove((row.conv, row.remote.as_str()))?;
        self.tx.open_table(BY_CONV)?.remove((row.conv, row.sent, key.0))?;
        {
            let mut edits = self.tx.open_table(MSG_EDIT)?;
            let ns: Vec<u32> =
                edits.range((key.0, 0)..=(key.0, u32::MAX))?.map(|r| r.map(|(k, _)| k.value().1)).collect::<std::result::Result<_, _>>()?;
            for n in ns {
                edits.remove((key.0, n))?;
            }
        }
        if let Some(c) = self.conv(ConvKey(row.conv))? {
            let mut by_id = self.tx.open_table(MSG_BY_ID)?;
            if by_id.get((c.account.0, row.remote.as_str()))?.is_some_and(|v| v.value() == row.conv) {
                by_id.remove((c.account.0, row.remote.as_str()))?;
            }
        }
        content::release_parts(&self.tx, row.loaded.into_iter().map(|(_, h)| h))?;
        Ok(true)
    }

    // --- люди ---

    pub fn put_participant(&self, account: ChanKey, id: &str, value: &[u8]) -> Result<()> {
        self.tx.open_table(PARTICIPANT)?.insert((account.0, id), value)?;
        Ok(())
    }

    pub fn participant(&self, account: ChanKey, id: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.tx.open_table(PARTICIPANT)?.get((account.0, id))?.map(|v| v.value().to_vec()))
    }

    pub fn put_member(&self, conv: ConvKey, id: &str, value: &[u8]) -> Result<()> {
        self.tx.open_table(MEMBER)?.insert((conv.0, id), value)?;
        Ok(())
    }

    pub fn remove_member(&self, conv: ConvKey, id: &str) -> Result<()> {
        self.tx.open_table(MEMBER)?.remove((conv.0, id))?;
        Ok(())
    }

    /// Исходящее в очереди отправки учётки: записать или заменить.
    pub fn put_outbox(&self, account: ChanKey, client_id: u64, value: &[u8]) -> Result<()> {
        self.tx.open_table(CHAN_OUTBOX)?.insert((account.0, client_id), value)?;
        Ok(())
    }

    /// Исходящее из очереди; `false` — его там не было.
    pub fn remove_outbox(&self, account: ChanKey, client_id: u64) -> Result<bool> {
        Ok(self.tx.open_table(CHAN_OUTBOX)?.remove((account.0, client_id))?.is_some())
    }

    pub fn outbox(&self, account: ChanKey, client_id: u64) -> Result<Option<Vec<u8>>> {
        Ok(self.tx.open_table(CHAN_OUTBOX)?.get((account.0, client_id))?.map(|v| v.value().to_vec()))
    }
}

impl Cache {
    /// Пачка записей событий канала: одна транзакция до [`ChanBatch::commit`].
    pub fn chan_batch(&self) -> Result<ChanBatch> {
        Ok(ChanBatch { tx: self.db.begin_write()?, level: self.tuning.level })
    }

    /// Учётки каналов профиля по ключу.
    pub fn chan_accounts(&self) -> Result<Vec<ChanAccount>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(CHAN_ACCOUNT)?;
        t.iter()?.map(|r| r.map_err(Into::into).and_then(|(k, v)| Ok(account_doc(k.value(), dec(v.value())?)))).collect()
    }

    pub fn chan_account(&self, key: ChanKey) -> Result<Option<ChanAccount>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(CHAN_ACCOUNT)?;
        t.get(key.0)?.map(|v| dec::<AccountRow>(v.value()).map(|r| account_doc(key.0, r))).transpose()
    }

    /// Беседы всех каналов: новые сверху, `limit` с `offset`.
    pub fn chan_convs(&self, offset: usize, limit: usize) -> Result<Vec<ConvDoc>> {
        let tx = self.db.begin_read()?;
        let (by, conv) = (tx.open_table(CONV_BY_TIME)?, tx.open_table(CONV)?);
        let mut out = Vec::new();
        for r in by.iter()?.skip(offset).take(limit) {
            let k = r?.0.value().1;
            if let Some(v) = conv.get(k)? {
                out.push(conv_doc(k, dec(v.value())?));
            }
        }
        Ok(out)
    }

    pub fn chan_conv(&self, key: ConvKey) -> Result<Option<ConvDoc>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(CONV)?;
        t.get(key.0)?.map(|v| dec::<ConvRow>(v.value()).map(|r| conv_doc(key.0, r))).transpose()
    }

    pub fn chan_conv_key(&self, account: ChanKey, remote: &str) -> Result<Option<ConvKey>> {
        let tx = self.db.begin_read()?;
        Ok(tx.open_table(CONV_REMOTE)?.get((account.0, remote))?.map(|v| ConvKey(v.value())))
    }

    /// Лента беседы: до `limit` последних сообщений раньше `before` (`None` — с конца), старые первыми.
    pub fn chan_timeline(&self, conv: ConvKey, before: Option<MsgKey>, limit: usize) -> Result<Vec<MsgDoc>> {
        let tx = self.db.begin_read()?;
        let (by, msg) = (tx.open_table(BY_CONV)?, tx.open_table(MSG)?);
        let end = match before {
            Some(b) => match msg.get(b.0)?.map(|v| dec::<MsgRow>(v.value())).transpose()? {
                Some(row) => (conv.0, row.sent, b.0),
                None => return Ok(Vec::new()),
            },
            None => (conv.0, i64::MAX, u64::MAX),
        };
        let mut keys = Vec::new();
        for r in by.range((conv.0, i64::MIN, 0)..end)?.rev().take(limit) {
            keys.push(r?.0.value().2);
        }
        keys.reverse();
        let mut out = Vec::with_capacity(keys.len());
        for k in keys {
            if let Some(v) = msg.get(k)? {
                out.push(msg_doc(k, dec(v.value())?)?);
            }
        }
        Ok(out)
    }

    /// Сообщений в беседе.
    pub fn chan_msg_count(&self, conv: ConvKey) -> Result<usize> {
        let tx = self.db.begin_read()?;
        let by = tx.open_table(BY_CONV)?;
        Ok(by.range((conv.0, i64::MIN, 0)..=(conv.0, i64::MAX, u64::MAX))?.count())
    }

    /// Сообщения всех каналов с ключа `from` по порядку ключей, не больше `limit` (пересборка поиска).
    pub fn chan_msgs_from(&self, from: MsgKey, limit: usize) -> Result<Vec<MsgDoc>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(MSG)?;
        let mut out = Vec::new();
        for r in t.range(from.0..)?.take(limit) {
            let (k, v) = r?;
            out.push(msg_doc(k.value(), dec(v.value())?)?);
        }
        Ok(out)
    }

    pub fn chan_msg(&self, key: MsgKey) -> Result<Option<MsgDoc>> {
        let tx = self.db.begin_read()?;
        let row = tx.open_table(MSG)?.get(key.0)?.map(|v| dec::<MsgRow>(v.value())).transpose()?;
        row.map(|r| msg_doc(key.0, r)).transpose()
    }

    pub fn chan_msg_key(&self, conv: ConvKey, remote: &str) -> Result<Option<MsgKey>> {
        let tx = self.db.begin_read()?;
        Ok(tx.open_table(MSG_REMOTE)?.get((conv.0, remote))?.map(|v| MsgKey(v.value())))
    }

    /// История правок сообщения, старые первыми: время прежней правки (`None` — исходное) и значение.
    pub fn chan_edits(&self, key: MsgKey) -> Result<Vec<(Option<i64>, Vec<u8>)>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(MSG_EDIT)?;
        let mut out = Vec::new();
        for r in t.range((key.0, 0)..=(key.0, u32::MAX))? {
            let row: EditRow = dec(r?.1.value())?;
            out.push((row.edited, row.value.into_bytes()?));
        }
        Ok(out)
    }

    pub fn chan_participant(&self, account: ChanKey, id: &str) -> Result<Option<Vec<u8>>> {
        let tx = self.db.begin_read()?;
        Ok(tx.open_table(PARTICIPANT)?.get((account.0, id))?.map(|v| v.value().to_vec()))
    }

    /// Очередь отправки учётки: `client_id` и значение ядра, по порядку номеров.
    pub fn chan_outbox(&self, account: ChanKey) -> Result<Vec<(u64, Vec<u8>)>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(CHAN_OUTBOX)?;
        t.range((account.0, 0)..=(account.0, u64::MAX))?.map(|r| r.map_err(Into::into).map(|(k, v)| (k.value().1, v.value().to_vec()))).collect()
    }

    /// Люди учётки канала: id и значение ядра.
    pub fn chan_participants(&self, account: ChanKey) -> Result<Vec<(String, Vec<u8>)>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(PARTICIPANT)?;
        t.range(of_account(account.0))?.map(|r| r.map_err(Into::into).map(|(k, v)| (k.value().1.to_owned(), v.value().to_vec()))).collect()
    }

    /// Участники беседы по id: id и значение ядра (роль).
    pub fn chan_members(&self, conv: ConvKey) -> Result<Vec<(String, Vec<u8>)>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(MEMBER)?;
        t.range(of_account(conv.0))?.map(|r| r.map_err(Into::into).map(|(k, v)| (k.value().1.to_owned(), v.value().to_vec()))).collect()
    }

    /// Байты загруженного вложения; нет — `None`.
    pub fn chan_part(&self, hash: Hash) -> Result<Option<Vec<u8>>> {
        content::read_part(&self.db.begin_read()?, hash)
    }

    /// Загрузка вложения кусками ([`ChanDownload`]).
    pub fn chan_download(&self) -> ChanDownload {
        ChanDownload { db: Arc::clone(&self.db), level: self.tuning.level, tmp: None, n: 0, len: 0, buf: Vec::new(), hasher: Sha256::new() }
    }

    /// «Выйти» из учётки канала: беседы, сообщения, участники, учётка и свои данные канала в `local` (ключи с
    /// `local_prefix`). Пачками по [`FORGET_BATCH`] сообщений; учётка и `local` — последней транзакцией: прерванное
    /// продолжится следующим вызовом.
    pub fn forget_chan_account(&self, key: ChanKey, local_prefix: &str) -> Result<()> {
        loop {
            let tx = self.chan_batch()?;
            let convs: Vec<u64> = {
                let t = tx.tx.open_table(CONV_REMOTE)?;
                t.range(of_account(key.0))?.map(|r| r.map(|(_, v)| v.value())).collect::<std::result::Result<_, _>>()?
            };
            let mut removed = 0;
            let mut left = false;
            for c in convs {
                if removed >= FORGET_BATCH {
                    left = true;
                    break;
                }
                removed += tx.remove_conv(ConvKey(c))?;
            }
            if left {
                tx.commit()?;
                continue;
            }
            {
                let mut people = tx.tx.open_table(PARTICIPANT)?;
                let ids: Vec<String> =
                    people.range(of_account(key.0))?.map(|r| r.map(|(k, _)| k.value().1.to_owned())).collect::<std::result::Result<_, _>>()?;
                for id in ids {
                    people.remove((key.0, id.as_str()))?;
                }
            }
            tx.tx.open_table(CHAN_OUTBOX)?.retain(|(a, _), _| a != key.0)?;
            tx.tx.open_table(CHAN_ACCOUNT)?.remove(key.0)?;
            crate::local::remove_prefix(&tx.tx, local_prefix)?;
            return tx.commit();
        }
    }
}

/// Загрузка вложения канала кусками: байты копятся по [`CHUNK`] во временной таблице (своей транзакцией на кусок),
/// хэш считается на лету; [`ChanDownload::finish`] одной транзакцией переносит их под хэш (такая часть уже есть —
/// куски удаляются) и записывает часть в сообщение. Брошенная загрузка убирает свои куски; после сбоя их убирает
/// открытие базы.
pub struct ChanDownload {
    db: Arc<Database>,
    level: Option<i32>,
    tmp: Option<u64>,
    /// Кусков во временной таблице.
    n: u32,
    len: u64,
    buf: Vec<u8>,
    hasher: Sha256,
}

impl ChanDownload {
    /// Байт принято.
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn write(&mut self, mut chunk: &[u8]) -> Result<()> {
        self.hasher.update(chunk);
        self.len += chunk.len() as u64;
        while !chunk.is_empty() {
            let room = CHUNK - self.buf.len();
            let (now, rest) = chunk.split_at(room.min(chunk.len()));
            self.buf.extend_from_slice(now);
            chunk = rest;
            if self.buf.len() == CHUNK {
                self.flush()?;
            }
        }
        Ok(())
    }

    /// Полный кусок — во временную таблицу.
    fn flush(&mut self) -> Result<()> {
        let tx = self.db.begin_write()?;
        let tmp = match self.tmp {
            Some(t) => t,
            None => *self.tmp.insert(next_id(&tx, NEXT_TMP)?),
        };
        tx.open_table(CHAN_TMP)?.insert((tmp, self.n), self.buf.as_slice())?;
        tx.commit()?;
        self.n += 1;
        self.buf.clear();
        Ok(())
    }

    /// Конец загрузки: часть по хэшу (тип `mime` решает, жать ли) — вложением `source` сообщения `msg` (прежняя
    /// часть того же источника снимается). Ответ — хэш; сообщения уже нет — `None`, часть не остаётся.
    pub fn finish(mut self, msg: MsgKey, source: &[u8], mime: &str) -> Result<Option<Hash>> {
        let hash = Hash(std::mem::take(&mut self.hasher).finalize().into());
        let tx = self.db.begin_write()?;
        let tail = std::mem::take(&mut self.buf);
        let stored = self.n;
        let n = stored + u32::from(!tail.is_empty());
        let tmp = self.tmp;
        {
            let read = |i: u32| -> Result<Vec<u8>> {
                if i == stored {
                    return Ok(tail.clone());
                }
                let t = tx.open_table(CHAN_TMP)?;
                let Some(v) = tmp.and_then(|tmp| t.get((tmp, i)).transpose()).transpose()? else {
                    return Err(crate::Error(format!("download: no chunk {i}")));
                };
                Ok(v.value().to_vec())
            };
            content::acquire_chunked(&tx, hash, self.len, n, mime, self.level, read)?;
        }
        if let Some(tmp) = self.tmp.take() {
            let mut t = tx.open_table(CHAN_TMP)?;
            for i in 0..stored {
                t.remove((tmp, i))?;
            }
        }
        let attached = {
            let mut t = tx.open_table(MSG)?;
            let row = t.get(msg.0)?.map(|v| dec::<MsgRow>(v.value())).transpose()?;
            match row {
                Some(mut row) => {
                    let old = row.loaded.iter().position(|(s, _)| s == source).map(|i| row.loaded.remove(i).1);
                    row.loaded.push((source.to_vec(), hash));
                    t.insert(msg.0, enc(&row)?.as_slice())?;
                    Some(old)
                }
                None => None,
            }
        };
        let out = match attached {
            Some(old) => {
                content::release_parts(&tx, old)?;
                Some(hash)
            }
            None => {
                content::release_parts(&tx, [hash])?;
                None
            }
        };
        tx.commit()?;
        Ok(out)
    }
}

impl Drop for ChanDownload {
    fn drop(&mut self) {
        let Some(tmp) = self.tmp.take() else { return };
        // Брошенная загрузка: куски не нужны. Не вышло — уберёт открытие базы.
        let clean = || -> Result<()> {
            let tx = self.db.begin_write()?;
            {
                let mut t = tx.open_table(CHAN_TMP)?;
                for i in 0..self.n {
                    t.remove((tmp, i))?;
                }
            }
            tx.commit()?;
            Ok(())
        };
        let _ = clean();
    }
}

#[cfg(test)]
mod tests {
    use redb::ReadableTableMetadata;

    use super::*;
    use crate::LocalPut;

    fn put<'a>(remote: &'a str, sent: i64, edited: Option<i64>, value: &'a [u8], sources: &'a [Vec<u8>]) -> MsgPut<'a> {
        MsgPut { remote, sent, edited, value, sources }
    }

    fn setup() -> (Cache, ChanKey, ConvKey) {
        let c = Cache::open_in_memory().unwrap();
        let b = c.chan_batch().unwrap();
        let acc = b.add_account("demo", "me@example.test", b"A").unwrap();
        let conv = b.put_conv(acc, "c1", b"conv").unwrap();
        b.commit().unwrap();
        (c, acc, conv)
    }

    fn tmp_rows(c: &Cache) -> u64 {
        c.db.begin_read().unwrap().open_table(CHAN_TMP).unwrap().len().unwrap()
    }

    /// Тот же id — та же строка; правка — прежняя версия в истории; иначе без правки — замена без истории.
    #[test]
    fn messages_are_idempotent_and_keep_edits() {
        let (c, _, conv) = setup();
        let b = c.chan_batch().unwrap();
        let (k, s) = b.put_msg(conv, &put("m1", 10, None, b"v1", &[])).unwrap();
        assert_eq!(s, Stored::New);
        assert_eq!(b.put_msg(conv, &put("m1", 10, None, b"v1", &[])).unwrap(), (k, Stored::Same));
        assert_eq!(b.put_msg(conv, &put("m1", 10, None, b"v1-decrypted", &[])).unwrap(), (k, Stored::Replaced));
        assert_eq!(b.put_msg(conv, &put("m1", 10, Some(20), b"v2", &[])).unwrap(), (k, Stored::Edited));
        assert_eq!(b.put_msg(conv, &put("m1", 10, Some(30), b"v3", &[])).unwrap(), (k, Stored::Edited));
        b.commit().unwrap();
        assert_eq!(c.chan_msg_count(conv).unwrap(), 1);
        assert_eq!(c.chan_msg(k).unwrap().unwrap().value, b"v3");
        let edits = c.chan_edits(k).unwrap();
        assert_eq!(edits, vec![(None, b"v1-decrypted".to_vec()), (Some(20), b"v2".to_vec())]);
        assert_eq!(c.chan_conv(conv).unwrap().unwrap().last, 10);
    }

    /// Крупное значение — сжатым, читается обратно как было.
    #[test]
    fn big_values_are_packed() {
        let (c, _, conv) = setup();
        let big = "текст сообщения ".repeat(200).into_bytes();
        let b = c.chan_batch().unwrap();
        let (k, _) = b.put_msg(conv, &put("m", 1, None, &big, &[])).unwrap();
        b.commit().unwrap();
        let tx = c.db.begin_read().unwrap();
        let row: MsgRow = dec(tx.open_table(MSG).unwrap().get(k.0).unwrap().unwrap().value()).unwrap();
        assert!(matches!(row.value, Packed::Zstd(..)), "от 1 КиБ — zstd");
        assert_eq!(c.chan_msg(k).unwrap().unwrap().value, big);
    }

    /// Лента по времени, страницы назад; список бесед — новые сверху.
    #[test]
    fn timeline_pages_and_conversation_order() {
        let (c, acc, conv) = setup();
        let b = c.chan_batch().unwrap();
        let other = b.put_conv(acc, "c2", b"conv2").unwrap();
        let mut keys = Vec::new();
        for i in 0..5 {
            keys.push(b.put_msg(conv, &put(&format!("m{i}"), i * 10, None, b"x", &[])).unwrap().0);
        }
        b.put_msg(other, &put("o", 5, None, b"x", &[])).unwrap();
        b.commit().unwrap();
        let last2: Vec<MsgKey> = c.chan_timeline(conv, None, 2).unwrap().iter().map(|m| m.key).collect();
        assert_eq!(last2, keys[3..]);
        let before: Vec<MsgKey> = c.chan_timeline(conv, Some(keys[3]), 10).unwrap().iter().map(|m| m.key).collect();
        assert_eq!(before, keys[..3]);
        let order: Vec<String> = c.chan_convs(0, 10).unwrap().into_iter().map(|d| d.remote).collect();
        assert_eq!(order, ["c1", "c2"], "c1 — последнее в 40, c2 — в 5");
        assert_eq!(c.chan_convs(1, 10).unwrap().len(), 1);
    }

    /// Удаление без беседы: беседа — по id в учётке; удалённое уходит из индекса.
    #[test]
    fn delete_without_conversation() {
        let (c, acc, conv) = setup();
        let b = c.chan_batch().unwrap();
        let (k, _) = b.put_msg(conv, &put("42", 1, None, b"x", &[])).unwrap();
        assert_eq!(b.conv_of_id(acc, "42").unwrap(), Some(conv));
        assert!(b.remove_msg(k).unwrap());
        assert!(!b.remove_msg(k).unwrap());
        assert_eq!(b.conv_of_id(acc, "42").unwrap(), None);
        assert_eq!(b.msg_key(conv, "42").unwrap(), None);
        b.commit().unwrap();
        assert_eq!(c.chan_msg_count(conv).unwrap(), 0);
    }

    /// Загрузка: мелкая — сразу частью, крупная — кусками; тот же файл в письме и в канале — одна часть; снятое
    /// сообщение снимает вхождение; сверка ссылок сходится.
    #[test]
    fn downloads_share_parts_with_mail() {
        let (c, acc, conv) = setup();
        let small = b"logo".repeat(400);
        c.put_local(&LocalPut { key: "sign".into(), value: Vec::new(), parts: vec![(small.clone(), "image/png".into())] }).unwrap();
        let big: Vec<u8> = (0..CHUNK * 2 + 100).map(|i| (i % 251) as u8).collect();
        let b = c.chan_batch().unwrap();
        let src = vec![b"s1".to_vec(), b"s2".to_vec()];
        let (k, _) = b.put_msg(conv, &put("m", 1, None, b"x", &src)).unwrap();
        b.commit().unwrap();

        let mut d = c.chan_download();
        d.write(&small).unwrap();
        let h1 = d.finish(k, b"s1", "image/png").unwrap().unwrap();
        assert_eq!(c.store_stats().unwrap().parts, 1, "та же картинка, что в local");

        let mut d = c.chan_download();
        for piece in big.chunks(300_000) {
            d.write(piece).unwrap();
        }
        assert!(tmp_rows(&c) >= 2);
        let h2 = d.finish(k, b"s2", "application/octet-stream").unwrap().unwrap();
        assert_eq!(tmp_rows(&c), 0, "куски перенесены под хэш");
        assert_eq!(c.chan_part(h2).unwrap(), Some(big.clone()));
        assert_eq!(c.chan_part(h1).unwrap(), Some(small.clone()));
        assert!(c.check_refs().unwrap().is_empty(), "{:?}", c.check_refs());

        // Правка без второго вложения — его часть снимается.
        let b = c.chan_batch().unwrap();
        b.put_msg(conv, &put("m", 1, Some(2), b"y", &src[..1])).unwrap();
        b.commit().unwrap();
        assert_eq!(c.chan_part(h2).unwrap(), None);
        assert!(c.check_refs().unwrap().is_empty());

        // Сообщения нет — загруженное не остаётся.
        let b = c.chan_batch().unwrap();
        b.remove_msg(k).unwrap();
        b.commit().unwrap();
        let mut d = c.chan_download();
        d.write(&big).unwrap();
        assert_eq!(d.finish(k, b"s2", "application/octet-stream").unwrap(), None);
        assert_eq!(c.chan_part(h2).unwrap(), None);
        assert_eq!(c.store_stats().unwrap().parts, 1, "осталась только картинка local");
        assert!(c.check_refs().unwrap().is_empty());
        let _ = acc;
    }

    /// Брошенная загрузка убирает свои куски.
    #[test]
    fn dropped_download_cleans_up() {
        let (c, _, _) = setup();
        let mut d = c.chan_download();
        d.write(&vec![1u8; CHUNK + 1]).unwrap();
        assert_eq!(tmp_rows(&c), 1);
        drop(d);
        assert_eq!(tmp_rows(&c), 0);
    }

    /// «Выйти»: всё учётки и её `local` — одной операцией; другая учётка и чужие ключи — на месте.
    #[test]
    fn forget_account_removes_everything_of_it() {
        let (c, acc, conv) = setup();
        let b = c.chan_batch().unwrap();
        let other = b.add_account("demo", "other@example.test", b"B").unwrap();
        let oc = b.put_conv(other, "c1", b"conv").unwrap();
        b.put_msg(conv, &put("m", 1, Some(1), b"x", &[b"s".to_vec()])).unwrap();
        b.put_msg(conv, &put("m", 1, Some(2), b"x2", &[b"s".to_vec()])).unwrap();
        b.put_msg(oc, &put("m", 1, None, b"x", &[])).unwrap();
        b.put_participant(acc, "p", b"P").unwrap();
        b.put_member(conv, "p", b"member").unwrap();
        b.set_cursor(acc, b"cur").unwrap();
        b.commit().unwrap();
        let k = c.chan_msg_key(conv, "m").unwrap().unwrap();
        let mut d = c.chan_download();
        d.write(b"file").unwrap();
        d.finish(k, b"s", "text/plain").unwrap();
        for key in ["chan/demo/me@example.test/state", "chan/demo/other@example.test/state", "labels"] {
            c.put_local(&LocalPut { key: key.into(), value: b"v".to_vec(), parts: Vec::new() }).unwrap();
        }
        assert_eq!(c.chan_account(acc).unwrap().unwrap().cursor.as_deref(), Some(&b"cur"[..]));

        c.forget_chan_account(acc, "chan/demo/me@example.test/").unwrap();
        assert_eq!(c.chan_account(acc).unwrap(), None);
        assert_eq!(c.chan_conv(conv).unwrap(), None);
        assert_eq!(c.chan_msg(k).unwrap(), None);
        assert!(c.chan_edits(k).unwrap().is_empty());
        assert_eq!(c.chan_participant(acc, "p").unwrap(), None);
        assert!(c.chan_members(conv).unwrap().is_empty());
        assert_eq!(c.local("chan/demo/me@example.test/state").unwrap(), None);
        assert!(c.local("chan/demo/other@example.test/state").unwrap().is_some());
        assert!(c.local("labels").unwrap().is_some());
        assert_eq!(c.chan_accounts().unwrap().len(), 1);
        assert_eq!(c.chan_msg_count(oc).unwrap(), 1);
        assert_eq!(c.store_stats().unwrap().parts, 0);
        assert!(c.check_refs().unwrap().is_empty());
    }

    /// Учётка — одна на (канал, id); новая — новым ключом.
    #[test]
    fn accounts_are_unique_per_channel() {
        let (c, acc, _) = setup();
        let b = c.chan_batch().unwrap();
        assert_eq!(b.add_account("demo", "me@example.test", b"A2").unwrap(), acc);
        let other = b.add_account("telegram", "me@example.test", b"T").unwrap();
        assert_ne!(other, acc);
        b.commit().unwrap();
        let all = c.chan_accounts().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].value, b"A2");
    }
}
