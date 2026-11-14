//! Данные клиента (решение владельца 2026-11-14 «Все что можно в бд», ADR 0006 «Что не в базе»): то, что раньше
//! лежало файлами рядом с базой, — таблица `local`. Ключ — строка, значение — байты (формат знает ядро,
//! `postman_core::local`), крупные байты (картинки подписей, файлы подписок) — частями хранилища содержимого по хэшу:
//! одинаковые хранятся один раз, вхождения считаются вместе с частями писем ([`Cache::check_refs`]). Таблица
//! добавочная — создаётся при открытии, формат не меняется; следующие переносы формата обязаны сохранить её ссылки.

use std::collections::HashMap;

use redb::{ReadTransaction, ReadableDatabase, ReadableTable, TableDefinition, WriteTransaction};
use serde::{Deserialize, Serialize};

use crate::content::{self, Hash};
use crate::{Cache, META, Result, dec, enc};

/// Ключ → [`Row`].
const LOCAL: TableDefinition<&str, &[u8]> = TableDefinition::new("local");
/// Отметка в `meta`: прежние файлы перенесены ([`Cache::import_local`]).
const IMPORTED: &str = "local_import";

#[derive(Serialize, Deserialize)]
struct Row {
    value: Vec<u8>,
    parts: Vec<Hash>,
}

#[derive(Serialize)]
struct RowRef<'a> {
    value: &'a [u8],
    parts: &'a [Hash],
}

/// Запись данных клиента: значение и хэши её частей по порядку.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalDoc {
    pub value: Vec<u8>,
    pub parts: Vec<Hash>,
}

/// Что записать: ключ, значение и части — байты с типом MIME (по нему решается, жать ли).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalPut {
    pub key: String,
    pub value: Vec<u8>,
    pub parts: Vec<(Vec<u8>, String)>,
}

pub(crate) fn init(tx: &WriteTransaction) -> Result<()> {
    tx.open_table(LOCAL)?;
    Ok(())
}

/// Вхождения частей из записей — в `want` (сверка счётчиков).
pub(crate) fn count_parts(tx: &ReadTransaction, want: &mut HashMap<Hash, u32>) -> Result<()> {
    for r in tx.open_table(LOCAL)?.iter()? {
        for h in dec::<Row>(r?.1.value())?.parts {
            *want.entry(h).or_default() += 1;
        }
    }
    Ok(())
}

fn doc(row: Row) -> LocalDoc {
    LocalDoc { value: row.value, parts: row.parts }
}

/// Записать `put` в транзакции: новые части — вхождение + 1, части прежней записи — − 1 (после, чтобы общие остались).
fn write(tx: &WriteTransaction, put: &LocalPut, level: Option<i32>) -> Result<()> {
    let mut hashes = Vec::with_capacity(put.parts.len());
    for (bytes, mime) in &put.parts {
        hashes.push(content::acquire_bytes(tx, bytes, mime, level)?);
    }
    let old = {
        let mut t = tx.open_table(LOCAL)?;
        let prev = t.insert(put.key.as_str(), enc(&RowRef { value: &put.value, parts: &hashes })?.as_slice())?;
        prev.map(|v| dec::<Row>(v.value())).transpose()?
    };
    if let Some(old) = old {
        content::release_parts(tx, old.parts)?;
    }
    Ok(())
}

/// Удалить в транзакции записи с префиксом `prefix` и снять вхождения их частей. Ответ — сколько удалено.
pub(crate) fn remove_prefix(tx: &WriteTransaction, prefix: &str) -> Result<usize> {
    let mut parts = Vec::new();
    let mut n = 0;
    {
        let mut t = tx.open_table(LOCAL)?;
        let keys: Vec<String> = t
            .range(prefix..)?
            .map(|r| r.map(|(k, _)| k.value().to_owned()))
            .take_while(|k| k.as_ref().map_or(true, |k| k.starts_with(prefix)))
            .collect::<std::result::Result<_, _>>()?;
        for k in keys {
            if let Some(v) = t.remove(k.as_str())? {
                parts.extend(dec::<Row>(v.value())?.parts);
                n += 1;
            }
        }
    }
    content::release_parts(tx, parts)?;
    Ok(n)
}

impl Cache {
    /// Запись данных клиента; нет — `None`.
    pub fn local(&self, key: &str) -> Result<Option<LocalDoc>> {
        let tx = self.db.begin_read()?;
        Ok(tx.open_table(LOCAL)?.get(key)?.map(|v| dec::<Row>(v.value())).transpose()?.map(doc))
    }

    /// Записи с ключом, начинающимся с `prefix`, по порядку ключей.
    pub fn local_prefix(&self, prefix: &str) -> Result<Vec<(String, LocalDoc)>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(LOCAL)?;
        let mut out = Vec::new();
        for r in t.range(prefix..)? {
            let (k, v) = r?;
            if !k.value().starts_with(prefix) {
                break;
            }
            out.push((k.value().to_owned(), doc(dec::<Row>(v.value())?)));
        }
        Ok(out)
    }

    /// Байты части записи; нет — `None`.
    pub fn local_part(&self, hash: Hash) -> Result<Option<Vec<u8>>> {
        content::read_part(&self.db.begin_read()?, hash)
    }

    /// Записать (заменить) запись одной транзакцией: части прежней, на которые больше никто не ссылается, удаляются.
    pub fn put_local(&self, put: &LocalPut) -> Result<()> {
        let tx = self.db.begin_write()?;
        write(&tx, put, self.tuning.level)?;
        tx.commit()?;
        Ok(())
    }

    /// Удалить записи с ключом, начинающимся с `prefix` (и их части без других ссылок), одной транзакцией: свои данные
    /// канала при «Выйти» (`chan/<канал>/<учётка>/`, ADR 0012 п. 9). Ответ — сколько удалено.
    pub fn remove_local_prefix(&self, prefix: &str) -> Result<usize> {
        let tx = self.db.begin_write()?;
        let n = remove_prefix(&tx, prefix)?;
        tx.commit()?;
        Ok(n)
    }

    /// Удалить записи (и их части без других ссылок) одной транзакцией.
    pub fn remove_local(&self, keys: &[&str]) -> Result<()> {
        let tx = self.db.begin_write()?;
        for key in keys {
            let old = tx.open_table(LOCAL)?.remove(*key)?.map(|v| dec::<Row>(v.value())).transpose()?;
            if let Some(old) = old {
                content::release_parts(&tx, old.parts)?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Прежние файлы уже перенесены ([`Cache::import_local`]).
    pub fn local_imported(&self) -> Result<bool> {
        let tx = self.db.begin_read()?;
        Ok(tx.open_table(META)?.get(IMPORTED)?.is_some())
    }

    /// Перенос прежних файлов: все записи и отметка «перенесено» — одной транзакцией; ключ, который уже есть в базе,
    /// не перезаписывается. Ответ — сколько записано; `None` — перенос уже был, ничего не записано.
    pub fn import_local(&self, puts: &[LocalPut]) -> Result<Option<usize>> {
        let tx = self.db.begin_write()?;
        if tx.open_table(META)?.get(IMPORTED)?.is_some() {
            return Ok(None);
        }
        let mut n = 0;
        for put in puts {
            if tx.open_table(LOCAL)?.get(put.key.as_str())?.is_some() {
                continue;
            }
            write(&tx, put, self.tuning.level)?;
            n += 1;
        }
        tx.open_table(META)?.insert(IMPORTED, 1)?;
        tx.commit()?;
        Ok(Some(n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(key: &str, value: &[u8], parts: &[&[u8]]) -> LocalPut {
        LocalPut { key: key.into(), value: value.to_vec(), parts: parts.iter().map(|p| (p.to_vec(), "image/png".into())).collect() }
    }

    /// Запись с частями: одинаковые байты — одна часть, замена и удаление снимают вхождения, сверка сходится.
    #[test]
    fn local_rows_share_parts() {
        let c = Cache::open_in_memory().unwrap();
        let big = vec![7u8; crate::CHUNK + 10];
        c.put_local(&put("a", b"A", &[b"logo", &big])).unwrap();
        c.put_local(&put("b", b"B", &[b"logo"])).unwrap();
        assert_eq!(c.store_stats().unwrap().parts, 2);
        let a = c.local("a").unwrap().unwrap();
        assert_eq!(a.value, b"A");
        assert_eq!(c.local_part(a.parts[0]).unwrap().as_deref(), Some(&b"logo"[..]));
        assert_eq!(c.local_part(a.parts[1]).unwrap(), Some(big), "кусками — обратно целиком");
        assert!(c.check_refs().unwrap().is_empty());
        c.put_local(&put("a", b"A2", &[b"logo"])).unwrap();
        assert_eq!(c.store_stats().unwrap().parts, 1, "крупная часть ушла");
        c.remove_local(&["a", "b", "none"]).unwrap();
        assert_eq!(c.store_stats().unwrap().parts, 0);
        assert!(c.check_refs().unwrap().is_empty());
        assert_eq!(c.local("a").unwrap(), None);
    }

    /// Префикс — только свои ключи, по порядку.
    #[test]
    fn local_prefix_is_bounded() {
        let c = Cache::open_in_memory().unwrap();
        for k in ["n/2", "n/1", "m", "o"] {
            c.put_local(&put(k, k.as_bytes(), &[])).unwrap();
        }
        let keys: Vec<String> = c.local_prefix("n/").unwrap().into_iter().map(|(k, _)| k).collect();
        assert_eq!(keys, ["n/1", "n/2"]);
    }

    /// Перенос — один раз; уже записанный ключ не затирается.
    #[test]
    fn import_once_and_keep_existing() {
        let c = Cache::open_in_memory().unwrap();
        c.put_local(&put("a", b"mine", &[])).unwrap();
        assert!(!c.local_imported().unwrap());
        assert_eq!(c.import_local(&[put("a", b"file", &[]), put("b", b"file", &[b"x"])]).unwrap(), Some(1));
        assert_eq!(c.local("a").unwrap().unwrap().value, b"mine");
        assert!(c.local_imported().unwrap());
        assert_eq!(c.import_local(&[put("c", b"file", &[])]).unwrap(), None);
        assert_eq!(c.local("c").unwrap(), None);
        assert!(c.check_refs().unwrap().is_empty());
    }
}
