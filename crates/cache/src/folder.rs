//! Папки формата 6 (ADR 0006 п. 1): числовой [`FolderId`] и владелец — учётка или «На этом компьютере»
//! ([`Owner::Local`]). Папки учётки приходят синхронизацией ([`Cache::upsert_folder`]), локальные создаёт и правит
//! пользователь.

use redb::{ReadTransaction, ReadableDatabase, ReadableTable, TableDefinition, WriteTransaction};
use serde::{Deserialize, Serialize};

use crate::letter::{COUNT, NEXT_FOLDER, next_id};
use crate::{Cache, Error, Result, dec, enc};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FolderId(pub u64);

/// Чья папка. Учётка — ключ учётной записи (у прежнего API — префикс ключей кэша, основная — пустая строка).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Owner {
    Account(String),
    /// «На этом компьютере»: локальные ящики и архивы, без сервера.
    Local,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderRow {
    pub owner: Owner,
    pub parent: Option<FolderId>,
    pub name: String,
    pub role: Option<String>,
    /// id папки на сервере; у локальной — `None`.
    pub remote: Option<String>,
}

/// Где считать письма с ключевым словом ([`Cache::keyword_count_in`]): одна папка или все папки владельца.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Folder(FolderId),
    All(Owner),
}

pub(crate) const FOLDER: TableDefinition<u64, &[u8]> = TableDefinition::new("folder");
/// (учётка, id папки на сервере) → [`FolderId`].
pub(crate) const FOLDER_REMOTE: TableDefinition<(&str, &str), u64> = TableDefinition::new("folder_remote");
/// Владелец (postcard) → id «всех папок» владельца в индексе ключевых слов; id — из того же счётчика, что у папок.
pub(crate) const SCOPE: TableDefinition<&[u8], u64> = TableDefinition::new("scope");

/// id «всех папок» владельца; нет — выдать.
pub(crate) fn scope_id(tx: &WriteTransaction, owner: &Owner) -> Result<u64> {
    let key = enc(owner)?;
    let mut t = tx.open_table(SCOPE)?;
    let found = t.get(key.as_slice())?.map(|v| v.value());
    if let Some(id) = found {
        return Ok(id);
    }
    let id = next_id(tx, NEXT_FOLDER)?;
    t.insert(key.as_slice(), id)?;
    Ok(id)
}

/// Ключ индекса ключевых слов для `scope`; `None` — у владельца ещё не было писем с ключевыми словами.
pub(crate) fn scope_key(tx: &ReadTransaction, scope: &Scope) -> Result<Option<u64>> {
    match scope {
        Scope::Folder(f) => Ok(Some(f.0)),
        Scope::All(owner) => Ok(tx.open_table(SCOPE)?.get(enc(owner)?.as_slice())?.map(|v| v.value())),
    }
}

pub(crate) fn get(tx: &WriteTransaction, id: FolderId) -> Result<Option<FolderRow>> {
    tx.open_table(FOLDER)?.get(id.0)?.map(|v| dec(v.value())).transpose()
}

fn existing(tx: &WriteTransaction, id: FolderId) -> Result<FolderRow> {
    get(tx, id)?.ok_or_else(|| Error(format!("no folder {}", id.0)))
}

/// Папка учётки `account` с id сервера `remote`; нет — новая с этим id вместо имени (имя придёт с папками сервера).
pub(crate) fn ensure_remote(tx: &WriteTransaction, account: &str, remote: &str) -> Result<FolderId> {
    let found = tx.open_table(FOLDER_REMOTE)?.get((account, remote))?.map(|v| FolderId(v.value()));
    if let Some(id) = found {
        return Ok(id);
    }
    let row =
        FolderRow { owner: Owner::Account(account.to_owned()), parent: None, name: remote.to_owned(), role: None, remote: Some(remote.to_owned()) };
    put(tx, None, &row)
}

/// Записать папку: `id = None` — новая. Папка учётки с id сервера доступна и по нему ([`Cache::folder_of`]).
pub(crate) fn put(tx: &WriteTransaction, id: Option<FolderId>, row: &FolderRow) -> Result<FolderId> {
    let id = match id {
        Some(id) => id,
        None => FolderId(next_id(tx, NEXT_FOLDER)?),
    };
    let mut by_remote = tx.open_table(FOLDER_REMOTE)?;
    if let Some(old) = get(tx, id)?
        && let (Owner::Account(a), Some(r)) = (&old.owner, &old.remote)
    {
        by_remote.remove((a.as_str(), r.as_str()))?;
    }
    if let (Owner::Account(a), Some(r)) = (&row.owner, &row.remote) {
        by_remote.insert((a.as_str(), r.as_str()), id.0)?;
    }
    tx.open_table(FOLDER)?.insert(id.0, enc(row)?.as_slice())?;
    Ok(id)
}

/// Удалить папку вместе с её ключом сервера (писем в ней быть не должно).
pub(crate) fn remove(tx: &WriteTransaction, id: FolderId) -> Result<()> {
    let old: Option<FolderRow> = tx.open_table(FOLDER)?.remove(id.0)?.map(|v| dec(v.value())).transpose()?;
    if let Some(FolderRow { owner: Owner::Account(a), remote: Some(r), .. }) = old {
        tx.open_table(FOLDER_REMOTE)?.remove((a.as_str(), r.as_str()))?;
    }
    tx.open_table(COUNT)?.remove(id.0)?;
    Ok(())
}

/// Локальная папка `id` (переименовать и перенести можно только её: папки учётки правит сервер).
fn local(tx: &WriteTransaction, id: FolderId) -> Result<FolderRow> {
    let row = existing(tx, id)?;
    if row.owner != Owner::Local {
        return Err(Error(format!("folder {} is not local", id.0)));
    }
    Ok(row)
}

impl Cache {
    /// Папка учётки из синхронизации: известная по (учётка, `remote`) — обновляется, иначе новая. Папка без id
    /// сервера или локальная — всегда новая.
    pub fn upsert_folder(&self, row: &FolderRow) -> Result<FolderId> {
        let tx = self.db.begin_write()?;
        let id = {
            let known = match (&row.owner, &row.remote) {
                (Owner::Account(a), Some(r)) => tx.open_table(FOLDER_REMOTE)?.get((a.as_str(), r.as_str()))?.map(|v| FolderId(v.value())),
                _ => None,
            };
            put(&tx, known, row)?
        };
        tx.commit()?;
        Ok(id)
    }

    /// Новая папка «На этом компьютере» в `parent` (`None` — в корне группы).
    pub fn create_folder(&self, parent: Option<FolderId>, name: &str) -> Result<FolderId> {
        let tx = self.db.begin_write()?;
        let id = {
            if let Some(p) = parent {
                local(&tx, p)?;
            }
            let row = FolderRow { owner: Owner::Local, parent, name: name.to_owned(), role: None, remote: None };
            put(&tx, None, &row)?
        };
        tx.commit()?;
        Ok(id)
    }

    pub fn rename_folder(&self, id: FolderId, name: &str) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut row = local(&tx, id)?;
            row.name = name.to_owned();
            put(&tx, Some(id), &row)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Перенести локальную папку в `parent` (тоже локальную; `None` — в корень). В себя или в свою вложенную — ошибка.
    pub fn move_folder(&self, id: FolderId, parent: Option<FolderId>) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut row = local(&tx, id)?;
            let mut up = parent;
            while let Some(p) = up {
                if p == id {
                    return Err(Error(format!("folder {} cannot move into itself", id.0)));
                }
                up = local(&tx, p)?.parent;
            }
            row.parent = parent;
            put(&tx, Some(id), &row)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Удалить пустую папку: без писем и вложенных папок. `false` — не пустая или её нет.
    pub fn delete_folder(&self, id: FolderId) -> Result<bool> {
        let tx = self.db.begin_write()?;
        let deleted = {
            let letters = tx.open_table(COUNT)?.get(id.0)?.map_or(0, |v| v.value());
            let mut children = false;
            for r in tx.open_table(FOLDER)?.iter()? {
                children |= dec::<FolderRow>(r?.1.value())?.parent == Some(id);
            }
            let empty = letters == 0 && !children && get(&tx, id)?.is_some();
            if empty {
                remove(&tx, id)?;
            }
            empty
        };
        tx.commit()?;
        Ok(deleted)
    }

    pub fn folder(&self, id: FolderId) -> Result<Option<FolderRow>> {
        let tx = self.db.begin_read()?;
        tx.open_table(FOLDER)?.get(id.0)?.map(|v| dec(v.value())).transpose()
    }

    /// Все папки в порядке id.
    pub fn folders(&self) -> Result<Vec<(FolderId, FolderRow)>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(FOLDER)?;
        t.iter()?
            .map(|r| {
                let (k, v) = r?;
                Ok((FolderId(k.value()), dec(v.value())?))
            })
            .collect()
    }

    /// Папка учётки по id сервера.
    pub fn folder_of(&self, account: &str, remote: &str) -> Result<Option<FolderId>> {
        let tx = self.db.begin_read()?;
        Ok(tx.open_table(FOLDER_REMOTE)?.get((account, remote))?.map(|v| FolderId(v.value())))
    }
}
