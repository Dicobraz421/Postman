//! Связь писем с сервером (ADR 0006 п. 1; таблица `remote` из ADR 0003): `(учётка, id сервера) → LetterId` и
//! обратно `LetterId → [RemoteRef]`. Плагин пишет и читает письма через неё.

use redb::{ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

use crate::letter::{LetterId, LetterRow, Letters, Place};
use crate::{Cache, Error, Result, dec, enc};

/// Письмо на сервере учётки.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteRef {
    pub account: String,
    /// id письма на сервере (ключ связи).
    pub id: String,
    /// Что ещё нужно протоколу (IMAP — папка, `UIDVALIDITY` и UID; EWS — `ItemId` и `ChangeKey`): формат плагина.
    pub data: Vec<u8>,
    /// Письмо перенесено в другую учётку или «На этом компьютере»: синхронизация этой учётки его не возвращает, но
    /// удаление на сервере видит ([`Cache::delete_remote`]).
    pub detached: bool,
}

/// (учётка, id сервера) → [`LetterId`], и для `detached`-связей.
pub(crate) const REMOTE: TableDefinition<(&str, &str), u64> = TableDefinition::new("remote");
/// [`LetterId`] → `Vec<RemoteRef>`.
pub(crate) const LETTER_REMOTE: TableDefinition<u64, &[u8]> = TableDefinition::new("letter_remote");

impl Letters<'_> {
    pub(crate) fn refs(&self, id: LetterId) -> Result<Vec<RemoteRef>> {
        Ok(self.letter_remote.get(id.0)?.map(|v| dec(v.value())).transpose()?.unwrap_or_default())
    }

    pub(crate) fn set_refs(&mut self, id: LetterId, refs: &[RemoteRef]) -> Result<()> {
        if refs.is_empty() {
            self.letter_remote.remove(id.0)?;
        } else {
            self.letter_remote.insert(id.0, enc(&refs)?.as_slice())?;
        }
        Ok(())
    }

    /// Письмо по связи и `detached` ли она.
    pub(crate) fn find(&self, account: &str, id: &str) -> Result<Option<(LetterId, bool)>> {
        let Some(letter) = self.remote.get((account, id))?.map(|v| LetterId(v.value())) else { return Ok(None) };
        let detached = self.refs(letter)?.iter().any(|r| r.account == account && r.id == id && r.detached);
        Ok(Some((letter, detached)))
    }

    /// Письмо по действующей (не `detached`) связи.
    pub(crate) fn attached(&self, account: &str, id: &str) -> Result<Option<LetterId>> {
        Ok(self.find(account, id)?.and_then(|(l, detached)| (!detached).then_some(l)))
    }

    /// Строка письма с сервера ([`Cache::upsert_remote`]).
    pub(crate) fn upsert_remote(&mut self, account: &str, id: &str, row: &LetterRow) -> Result<LetterId> {
        match self.find(account, id)? {
            Some((letter, true)) => Ok(letter),
            Some((letter, false)) => {
                let old = self.get(letter)?;
                let (content, place) = old.as_ref().map_or((None, Place::Synced), |o| (o.content, o.place));
                self.put(letter, old.as_ref(), &LetterRow { content, place, ..row.clone() })?;
                Ok(letter)
            }
            None => {
                let letter = self.create(&LetterRow { content: None, place: Place::Synced, ..row.clone() })?;
                self.remote.insert((account, id), letter.0)?;
                let link = RemoteRef { account: account.to_owned(), id: id.to_owned(), data: Vec::new(), detached: false };
                self.set_refs(letter, &[link])?;
                Ok(letter)
            }
        }
    }

    /// Письма `id` больше нет на сервере учётки ([`Cache::delete_remote`]); ответ — письмо, если оно удалено из базы.
    pub(crate) fn drop_remote(&mut self, account: &str, id: &str) -> Result<Option<LetterId>> {
        let Some(letter) = self.remote.remove((account, id))?.map(|v| LetterId(v.value())) else { return Ok(None) };
        let mut refs = self.refs(letter)?;
        let gone = refs.iter().position(|r| r.account == account && r.id == id).map(|i| refs.remove(i));
        self.set_refs(letter, &refs)?;
        if gone.is_some_and(|r| r.detached) || refs.iter().any(|r| !r.detached) {
            return Ok(None);
        }
        match self.get(letter)? {
            Some(row) if row.place == Place::Synced => {
                self.remove(letter)?;
                Ok(Some(letter))
            }
            _ => Ok(None),
        }
    }
}

impl Cache {
    /// Письмо с сервера учётки `account`: известное по связи — строка обновляется (папки и метки — с сервера,
    /// содержимое и место — свои), новое — получает [`LetterId`]. Связь `detached` (письмо перенесено) строку не
    /// меняет: письмо не возвращается в старую папку.
    pub fn upsert_remote(&self, account: &str, id: &str, row: &LetterRow) -> Result<LetterId> {
        let rows = [(id.to_owned(), row.clone())];
        self.upsert_remotes(account, &rows)?.pop().ok_or_else(|| Error("upsert_remote: no letter".into()))
    }

    /// [`Cache::upsert_remote`] пачкой — одной транзакцией.
    pub fn upsert_remotes(&self, account: &str, rows: &[(String, LetterRow)]) -> Result<Vec<LetterId>> {
        let tx = self.db.begin_write()?;
        let mut out = Vec::with_capacity(rows.len());
        {
            let mut l = Letters::open(&tx)?;
            for (id, row) in rows {
                out.push(l.upsert_remote(account, id, row)?);
            }
            l.finish()?;
        }
        tx.commit()?;
        Ok(out)
    }

    /// Письмо по действующей связи; `detached` — `None`: для учётки источника письма больше нет.
    pub fn letter_of(&self, account: &str, id: &str) -> Result<Option<LetterId>> {
        let tx = self.db.begin_read()?;
        let Some(letter) = tx.open_table(REMOTE)?.get((account, id))?.map(|v| LetterId(v.value())) else { return Ok(None) };
        let refs: Vec<RemoteRef> = tx.open_table(LETTER_REMOTE)?.get(letter.0)?.map(|v| dec(v.value())).transpose()?.unwrap_or_default();
        Ok((!refs.iter().any(|r| r.account == account && r.id == id && r.detached)).then_some(letter))
    }

    /// Все связи письма, и `detached`.
    pub fn remotes(&self, letter: LetterId) -> Result<Vec<RemoteRef>> {
        let tx = self.db.begin_read()?;
        Ok(tx.open_table(LETTER_REMOTE)?.get(letter.0)?.map(|v| dec(v.value())).transpose()?.unwrap_or_default())
    }

    /// Связать письмо с сервером (сервер цели подтвердил выгрузку) или обновить связь (данные протокола, `detached`).
    /// id сервера уже у другого письма — ошибка.
    pub fn attach_remote(&self, letter: LetterId, link: RemoteRef) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut l = Letters::open(&tx)?;
            if l.get(letter)?.is_none() {
                return Err(Error(format!("no letter {}", letter.0)));
            }
            if let Some((other, _)) = l.find(&link.account, &link.id)?
                && other != letter
            {
                return Err(Error(format!("remote id of letter {} is taken by letter {}", letter.0, other.0)));
            }
            l.remote.insert((link.account.as_str(), link.id.as_str()), letter.0)?;
            let mut refs = l.refs(letter)?;
            match refs.iter_mut().find(|r| r.account == link.account && r.id == link.id) {
                Some(r) => *r = link,
                None => refs.push(link),
            }
            l.set_refs(letter, &refs)?;
            l.finish()?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Пометить связь `detached`; `false` — связи нет или уже помечена.
    pub fn detach_remote(&self, account: &str, id: &str) -> Result<bool> {
        let tx = self.db.begin_write()?;
        let changed = {
            let mut l = Letters::open(&tx)?;
            let mut changed = false;
            if let Some((letter, false)) = l.find(account, id)? {
                let mut refs = l.refs(letter)?;
                for r in refs.iter_mut().filter(|r| r.account == account && r.id == id) {
                    r.detached = true;
                }
                l.set_refs(letter, &refs)?;
                changed = true;
            }
            l.finish()?;
            changed
        };
        tx.commit()?;
        Ok(changed)
    }

    /// Писем `ids` больше нет на сервере учётки: связь убирается. Письмо удаляется из базы, если связь была
    /// действующей, других действующих нет и письмо `Synced`; `detached`-связь просто снимается (шаг 4 переноса),
    /// письмо остаётся в новой папке. Ответ — удалённые письма.
    pub fn delete_remote(&self, account: &str, ids: &[String]) -> Result<Vec<LetterId>> {
        let tx = self.db.begin_write()?;
        let mut out = Vec::new();
        {
            let mut l = Letters::open(&tx)?;
            for id in ids {
                out.extend(l.drop_remote(account, id)?);
            }
            l.finish()?;
        }
        tx.commit()?;
        Ok(out)
    }
}
