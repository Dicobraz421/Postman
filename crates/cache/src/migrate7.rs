//! Перенос формата 6 в 7 (ADR 0006, дополнение 2026-11-14) — на месте, пачками: в базе есть локальные архивы,
//! которых нет на сервере, поэтому ни пересоздания, ни удаления при сбое.
//!
//! В начале файл помечается форматом 7 с незаконченным переносом (`meta.upgrade_from`): прежняя версия Postman его не
//! откроет и ничего не удалит, следующий запуск продолжит с того же места. Каждая пачка — одна транзакция: письмо
//! целиком собирается из записей формата 6, режется заново (воркер — мелкие части и картинки; без него и при его
//! отказе — прежние крупные части), пишется во временные таблицы формата 7, а записи формата 6 освобождаются. Тела
//! (`body`) сжимаются так же пачками. Последняя транзакция удаляет таблицы формата 6 и переименовывает временные;
//! затем файл уплотняется.

use std::path::Path;

use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition, TableHandle, WriteTransaction};
use serde::{Deserialize, Serialize};

use crate::content::{self, Coding, Encoder, Encoding, PartRef, SplitPart, TABLES, Tables, Tuning, bytes};
use crate::{BODY, BodyRow, Cache, META, Migrated, Migration, Result, dec, enc};

/// Таблицы формата 7 на время переноса.
const STAGING: Tables = Tables {
    content: TableDefinition::new("content.7"),
    content_refs: TableDefinition::new("content_refs.7"),
    part: TableDefinition::new("part.7"),
    part_refs: TableDefinition::new("part_refs.7"),
    chunks: TableDefinition::new("chunk.7"),
    body: TableDefinition::new("body.7"),
};

/// Таблицы формата 6: те же имена, что у формата 7, прежние значения.
const OLD_CONTENT: TableDefinition<[u8; 32], &[u8]> = TableDefinition::new("content");
const OLD_PART: TableDefinition<[u8; 32], &[u8]> = TableDefinition::new("part");
const OLD_CHUNKS: TableDefinition<([u8; 32], u32), &[u8]> = TableDefinition::new("chunk");

/// Ключ `meta`: перенос с этого формата не закончен.
const UPGRADE_FROM: &str = "upgrade_from";

/// Писем целиком в одной транзакции — не больше этого и не больше [`BATCH_BYTES`].
const BATCH: usize = 500;
const BATCH_BYTES: usize = 64 * 1024 * 1024;
/// Тел в одной транзакции.
const BODY_BATCH: usize = 5_000;

#[derive(Serialize, Deserialize)]
struct OldContent {
    #[serde(with = "bytes")]
    skeleton: Vec<u8>,
    parts: Vec<PartRef>,
    refs: u32,
}

#[derive(Serialize, Deserialize)]
struct OldPart {
    data: OldData,
    refs: u32,
}

#[derive(Serialize, Deserialize)]
enum OldData {
    Bytes(#[serde(with = "bytes")] Vec<u8>),
    Chunks(u32),
}

/// Перенос формата 6 не закончен (файл уже помечен форматом 7).
pub(crate) fn pending(db: &impl ReadableDatabase) -> Result<bool> {
    match db.begin_read()?.open_table(META) {
        Ok(t) => Ok(t.get(UPGRADE_FROM)?.is_some()),
        // Новая пустая база.
        Err(redb::TableError::TableDoesNotExist(_)) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Перенести файл `path` формата 6 (или продолжить перенос) в формат 7; открыть результат.
pub(crate) fn run(path: &Path, how: &mut Migration) -> Result<(Cache, Migrated)> {
    let mut db = Cache::create(path)?;
    upgrade(&db, how, &Tuning::default())?;
    // Место таблиц формата 6 — файлу: без уплотнения оно только переиспользуется. Не вышло — файл рабочий как есть.
    let _ = db.compact();
    let c = Cache::wrap(db);
    c.init()?;
    Ok((c, Migrated::Done))
}

pub(crate) fn upgrade(db: &Database, how: &mut Migration, tuning: &Tuning) -> Result<()> {
    begin(db)?;
    let total = {
        let r = db.begin_read()?;
        r.open_table(OLD_CONTENT)?.len()? + r.open_table(BODY)?.len()?
    };
    let mut done = 0;
    (how.progress)(done, total);
    loop {
        let tx = db.begin_write()?;
        let n = contents(&tx, how, tuning, BATCH)?;
        tx.commit()?;
        if n == 0 {
            break;
        }
        done += n;
        (how.progress)(done, total);
    }
    loop {
        let tx = db.begin_write()?;
        let n = bodies(&tx, tuning)?;
        tx.commit()?;
        if n == 0 {
            break;
        }
        done += n;
        (how.progress)(done, total);
    }
    finish(db)
}

/// Пометить файл форматом 7 с незаконченным переносом и завести временные таблицы.
fn begin(db: &Database) -> Result<()> {
    let tx = db.begin_write()?;
    {
        let mut meta = tx.open_table(META)?;
        if meta.get(UPGRADE_FROM)?.is_none() {
            meta.insert(UPGRADE_FROM, 6)?;
        }
        meta.insert("version", crate::FORMAT_VERSION)?;
    }
    content::open_all(&tx, &STAGING)?;
    tx.open_table(OLD_CONTENT)?;
    tx.open_table(OLD_PART)?;
    tx.open_table(OLD_CHUNKS)?;
    tx.open_table(BODY)?;
    tx.commit()?;
    Ok(())
}

/// Таблицы формата 6 — удалить, временные — на их место; перенос закончен.
fn finish(db: &Database) -> Result<()> {
    let tx = db.begin_write()?;
    tx.delete_table(OLD_CONTENT)?;
    tx.delete_table(OLD_PART)?;
    tx.delete_table(OLD_CHUNKS)?;
    tx.delete_table(BODY)?;
    rename(&tx, STAGING.content, TABLES.content)?;
    rename(&tx, STAGING.content_refs, TABLES.content_refs)?;
    rename(&tx, STAGING.part, TABLES.part)?;
    rename(&tx, STAGING.part_refs, TABLES.part_refs)?;
    rename(&tx, STAGING.chunks, TABLES.chunks)?;
    rename(&tx, STAGING.body, TABLES.body)?;
    tx.open_table(META)?.remove(UPGRADE_FROM)?;
    tx.commit()?;
    Ok(())
}

fn rename<K: redb::Key + 'static, V: redb::Value + 'static>(
    tx: &WriteTransaction,
    from: TableDefinition<K, V>,
    to: TableDefinition<K, V>,
) -> Result<()> {
    if tx.list_tables()?.any(|t| t.name() == to.name()) {
        tx.delete_table(to)?;
    }
    Ok(tx.rename_table(from, to)?)
}

/// Пачка писем целиком формата 6 → формат 7; сколько перенесено (0 — больше нет).
fn contents(tx: &WriteTransaction, how: &mut Migration, tuning: &Tuning, limit: usize) -> Result<u64> {
    let batch: Vec<([u8; 32], OldContent)> = {
        let old = tx.open_table(OLD_CONTENT)?;
        let mut out = Vec::new();
        let mut size = 0;
        for r in old.iter()? {
            let (k, v) = r?;
            size += v.value().len();
            out.push((k.value(), dec::<OldContent>(v.value())?));
            if out.len() >= limit || size >= BATCH_BYTES {
                break;
            }
        }
        out
    };
    for (key, row) in &batch {
        let (raw, old_parts) = assemble(tx, row)?;
        // Воркер даёт мелкие части и картинки с их типом; прежние крупные части — на случай его отказа (пустой
        // ответ или ошибка): при совпадении диапазонов берётся первая, ответ воркера.
        let mut parts = match &mut how.split {
            Some(split) => split(&raw).unwrap_or_default(),
            None => Vec::new(),
        };
        parts.extend(old_parts);
        content::acquire(tx, &STAGING, content::Hash(*key), &raw, &parts, row.refs, tuning)?;
        release_old(tx, key, row)?;
    }
    Ok(batch.len() as u64)
}

/// Письмо целиком из записи формата 6 и его прежние вынутые части (диапазоны в письме, байты).
fn assemble(tx: &WriteTransaction, row: &OldContent) -> Result<(Vec<u8>, Vec<SplitPart>)> {
    let part = tx.open_table(OLD_PART)?;
    let chunks = tx.open_table(OLD_CHUNKS)?;
    let mut raw = Vec::with_capacity(row.skeleton.len());
    let mut parts = Vec::with_capacity(row.parts.len());
    let mut pos = 0;
    for p in &row.parts {
        let at = (p.at as usize).min(row.skeleton.len()).max(pos);
        raw.extend_from_slice(&row.skeleton[pos..at]);
        pos = at;
        let Some(old) = part.get(p.hash.0)?.map(|v| dec::<OldPart>(v.value())).transpose()? else {
            return Err(crate::Error(format!("content: no part {}", content::hex(&p.hash))));
        };
        let decoded = match old.data {
            OldData::Bytes(b) => b,
            OldData::Chunks(n) => {
                let mut out = Vec::new();
                for i in 0..n {
                    let Some(c) = chunks.get((p.hash.0, i))? else {
                        return Err(crate::Error(format!("part {}: no chunk {i}", content::hex(&p.hash))));
                    };
                    out.extend_from_slice(c.value());
                }
                out
            }
        };
        let start = raw.len();
        let mut e = Encoder::new(p.coding);
        e.feed(&decoded, &mut raw);
        e.finish(&mut raw);
        let encoding = match p.coding {
            Coding::Base64 { .. } => Encoding::Base64,
            Coding::QuotedPrintable { .. } => Encoding::QuotedPrintable,
        };
        parts.push(SplitPart { range: start..raw.len(), encoding, decoded, mime: String::new() });
    }
    raw.extend_from_slice(&row.skeleton[pos..]);
    Ok((raw, parts))
}

/// Запись формата 6 — освободить целиком: её `refs` писем перешли в формат 7.
fn release_old(tx: &WriteTransaction, key: &[u8; 32], row: &OldContent) -> Result<()> {
    tx.open_table(OLD_CONTENT)?.remove(*key)?;
    let mut part = tx.open_table(OLD_PART)?;
    let mut chunks = tx.open_table(OLD_CHUNKS)?;
    for p in &row.parts {
        let Some(mut old) = part.get(p.hash.0)?.map(|v| dec::<OldPart>(v.value())).transpose()? else { continue };
        if old.refs > row.refs {
            old.refs -= row.refs;
            part.insert(p.hash.0, enc(&old)?.as_slice())?;
            continue;
        }
        part.remove(p.hash.0)?;
        if let OldData::Chunks(n) = old.data {
            for i in 0..n {
                chunks.remove((p.hash.0, i))?;
            }
        }
    }
    Ok(())
}

/// Пачка тел формата 6 → сжатые в таблицу формата 7; сколько перенесено.
fn bodies(tx: &WriteTransaction, tuning: &Tuning) -> Result<u64> {
    let batch: Vec<(String, BodyRow)> = {
        let old = tx.open_table(BODY)?;
        let mut out = Vec::new();
        for r in old.iter()?.take(BODY_BATCH) {
            let (k, v) = r?;
            out.push((k.value().to_owned(), dec::<BodyRow>(v.value())?));
        }
        out
    };
    let mut old = tx.open_table(BODY)?;
    let mut new = tx.open_table(STAGING.body)?;
    for (k, b) in &batch {
        new.insert(k.as_str(), content::pack_body(b, tuning.level)?.as_slice())?;
        old.remove(k.as_str())?;
    }
    Ok(batch.len() as u64)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::content::tests::{letter_of, noise, part_refs, text};
    use crate::content::{ContentRow, Hash, Packed, PartData, PartRow};
    use crate::{FolderRow, LetterId, LetterRow, Owner, enc, version};

    /// Хранилище формата 7 → формат 6, как его писала прежняя версия: строки содержимого и частей со своими `refs`
    /// (у части — вхождения, умноженные на письма), куски и тела как есть, без таблиц счётчиков.
    fn to_format_6(path: &Path) {
        let db = Database::create(path).unwrap();
        let tx = db.begin_write().unwrap();
        {
            let refs: HashMap<[u8; 32], u32> =
                tx.open_table(TABLES.content_refs).unwrap().iter().unwrap().map(|r| r.unwrap()).map(|(k, v)| (k.value(), v.value().0)).collect();
            let rows: Vec<([u8; 32], ContentRow)> = tx
                .open_table(TABLES.content)
                .unwrap()
                .iter()
                .unwrap()
                .map(|r| r.unwrap())
                .map(|(k, v)| (k.value(), dec(v.value()).unwrap()))
                .collect();
            let mut part_refs: HashMap<Hash, u32> = HashMap::new();
            let mut content = tx.open_table(OLD_CONTENT).unwrap();
            for (k, row) in rows {
                let n = refs[&k];
                for p in &row.parts {
                    *part_refs.entry(p.hash).or_default() += n;
                }
                let old = OldContent { skeleton: row.skeleton.into_bytes().unwrap(), parts: row.parts, refs: n };
                content.insert(k, enc(&old).unwrap().as_slice()).unwrap();
            }
            let parts: Vec<([u8; 32], PartRow)> =
                tx.open_table(TABLES.part).unwrap().iter().unwrap().map(|r| r.unwrap()).map(|(k, v)| (k.value(), dec(v.value()).unwrap())).collect();
            let mut part = tx.open_table(OLD_PART).unwrap();
            for (k, row) in parts {
                let data = match row.data {
                    PartData::Bytes(b) => OldData::Bytes(b.into_bytes().unwrap()),
                    PartData::Chunks(n) => OldData::Chunks(n),
                };
                part.insert(k, enc(&OldPart { data, refs: part_refs[&Hash(k)] }).unwrap().as_slice()).unwrap();
            }
            type Chunk = (([u8; 32], u32), Vec<u8>);
            let chunks: Vec<Chunk> = tx
                .open_table(OLD_CHUNKS)
                .unwrap()
                .iter()
                .unwrap()
                .map(|r| r.unwrap())
                .map(|(k, v)| (k.value(), dec::<Packed>(v.value()).unwrap().into_bytes().unwrap()))
                .collect();
            let mut t = tx.open_table(OLD_CHUNKS).unwrap();
            for (k, v) in chunks {
                t.insert(k, v.as_slice()).unwrap();
            }
            let bodies: Vec<(String, Vec<u8>)> = tx
                .open_table(BODY)
                .unwrap()
                .iter()
                .unwrap()
                .map(|r| r.unwrap())
                .map(|(k, v)| (k.value().to_owned(), enc(&content::unpack_body(v.value()).unwrap()).unwrap()))
                .collect();
            let mut t = tx.open_table(BODY).unwrap();
            for (k, v) in bodies {
                t.insert(k.as_str(), v.as_slice()).unwrap();
            }
            tx.open_table(META).unwrap().insert("version", 6u64).unwrap();
        }
        tx.delete_table(TABLES.content_refs).unwrap();
        tx.delete_table(TABLES.part_refs).unwrap();
        tx.commit().unwrap();
    }

    struct Fixture {
        letters: Vec<(LetterId, Vec<u8>)>,
        /// Части писем, как их даст воркер: по хэшу письма.
        splits: HashMap<Hash, Vec<SplitPart>>,
        bodies: Vec<(String, BodyRow)>,
        logo: Vec<u8>,
        attachment: Vec<u8>,
        big: Vec<u8>,
    }

    /// Файл формата 6: 40 рассылок с общим логотипом (в формате 6 — в скелете) и общим вложением, одно письмо в двух
    /// учётках, письмо с частью больше 1 МиБ, тела.
    fn v6(path: &Path) -> Fixture {
        let c = Cache::open(path).unwrap().tuned(Tuning::FORMAT_6);
        let folder = |a: &str| {
            c.upsert_folder(&FolderRow { owner: Owner::Account(a.into()), parent: None, name: "in".into(), role: None, remote: Some("in".into()) })
                .unwrap()
        };
        let (a, b) = (folder("a"), folder("b"));
        let (logo, attachment, big) = (noise(9 * 1024, 41), noise(100 * 1024, 42), text(2 * content::CHUNK + 5000, 43));
        let mut letters = Vec::new();
        let mut splits = HashMap::new();
        let mut put = |account: &str, id: &str, folder, raw: Vec<u8>, parts: Vec<SplitPart>| {
            let l = c.upsert_remote(account, id, &LetterRow { folders: vec![folder], ..LetterRow::default() }).unwrap();
            c.put_content(l, &raw, &parts).unwrap();
            splits.insert(Hash::of(&raw), parts);
            letters.push((l, raw));
        };
        for i in 0..40 {
            let (raw, parts) = letter_of(&format!("рассылка {i}"), &[("image/png", &logo), ("application/octet-stream", &attachment)], "\r\n");
            put("a", &format!("m{i}"), a, raw, parts);
        }
        let (raw, parts) = letter_of("копия себе", &[("application/octet-stream", &attachment)], "\r\n");
        put("a", "self", a, raw.clone(), parts.clone());
        put("b", "self", b, raw, parts);
        let (raw, parts) = letter_of("большое", &[("text/plain", &big)], "\n");
        put("b", "big", b, raw, parts);
        let mut bodies = Vec::new();
        for i in 0..30 {
            let body = BodyRow { html: Some(String::from_utf8(text(8 * 1024, i)).unwrap()), ..BodyRow::default() };
            c.put_body(&format!("k{i}"), &body).unwrap();
            bodies.push((format!("k{i}"), body));
        }
        assert_eq!(part_refs(&c, &logo), 0, "в формате 6 логотип — в скелете");
        drop(c);
        to_format_6(path);
        Fixture { letters, splits, bodies, logo, attachment, big }
    }

    fn assert_migrated(c: &Cache, f: &Fixture) {
        for (l, raw) in &f.letters {
            assert!(c.letter_content(*l).unwrap().unwrap() == *raw, "письмо {}", l.0);
        }
        for (k, b) in &f.bodies {
            assert_eq!(c.body(k).unwrap().as_ref(), Some(b));
        }
        assert_eq!(c.check_refs().unwrap(), Vec::<String>::new());
        assert_eq!(c.check_index().unwrap(), Vec::<String>::new());
        assert_eq!(part_refs(c, &f.logo), 40, "логотип — одна запись на 40 писем");
        assert_eq!(part_refs(c, &f.attachment), 41);
        assert_eq!(part_refs(c, &f.big), 1);
        assert_eq!(version(&*c.db).unwrap(), Some(crate::FORMAT_VERSION));
        assert!(!pending(&*c.db).unwrap());
        let tx = c.db.begin_read().unwrap();
        let names: Vec<String> = tx.list_tables().unwrap().map(|t| t.name().to_owned()).collect();
        assert!(names.iter().all(|n| !n.ends_with(".7")), "{names:?}");
    }

    /// Формат 6 → 7 на месте: письма — ровно те же байты, логотип вынут по ответу воркера, крупные части — и при
    /// его отказе (письмо с частью больше 1 МиБ), счётчики сходятся, тела сжаты, файл меньше.
    #[test]
    fn format_6_is_migrated_in_place() {
        let dir = tempfile::Builder::new().prefix("postman-cache-").tempdir().unwrap();
        let path = dir.path().join("c.redb");
        let f = v6(&path);
        let before = std::fs::metadata(&path).unwrap().len();
        let mut progress = Vec::new();
        let (c, how) = {
            let splits = &f.splits;
            let big = Hash::of(&f.letters[f.letters.len() - 1].1);
            let mut m = Migration {
                progress: Box::new(|done, total| progress.push((done, total))),
                split: Some(Box::new(move |raw: &[u8]| {
                    let h = Hash::of(raw);
                    if h == big { Err("worker is gone".into()) } else { Ok(splits.get(&h).cloned().unwrap_or_default()) }
                })),
            };
            Cache::open_with(&path, &mut m).unwrap()
        };
        assert_eq!(how, Migrated::Done);
        assert_eq!(progress.first(), Some(&(0, 42 + 30)));
        assert_eq!(progress.last(), Some(&(42 + 30, 42 + 30)));
        assert_migrated(&c, &f);
        let st = c.store_stats().unwrap();
        assert_eq!((st.letters, st.contents, st.bodies), (43, 42, 30));
        drop(c);
        assert!(std::fs::metadata(&path).unwrap().len() < before, "файл меньше");
        // Следующее открытие — своего формата.
        let (c, how) = Cache::open_with(&path, &mut Migration::default()).unwrap();
        assert_eq!(how, Migrated::Current);
        assert_migrated(&c, &f);
    }

    /// Перенос прерван после первой пачки: прежняя версия файл уже не откроет (формат 7), следующее открытие
    /// продолжает с того же места без воркера — данные целы.
    #[test]
    fn interrupted_migration_resumes() {
        let dir = tempfile::Builder::new().prefix("postman-cache-").tempdir().unwrap();
        let path = dir.path().join("c.redb");
        let f = v6(&path);
        {
            let db = Database::create(&path).unwrap();
            begin(&db).unwrap();
            let splits = &f.splits;
            let mut how = Migration { progress: Box::new(|_, _| {}), split: Some(Box::new(|raw: &[u8]| Ok(splits[&Hash::of(raw)].clone()))) };
            let tx = db.begin_write().unwrap();
            assert_eq!(contents(&tx, &mut how, &Tuning::default(), 5).unwrap(), 5);
            tx.commit().unwrap();
            assert_eq!(version(&db).unwrap(), Some(crate::FORMAT_VERSION));
            assert!(pending(&db).unwrap());
        }
        let mut progress = Vec::new();
        let (c, how) = {
            let mut m = Migration { progress: Box::new(|done, total| progress.push((done, total))), split: None };
            Cache::open_with(&path, &mut m).unwrap()
        };
        assert_eq!(how, Migrated::Done);
        assert_eq!(progress.first(), Some(&(0, 42 - 5 + 30)), "продолжение — с оставшегося");
        for (l, raw) in &f.letters {
            assert!(c.letter_content(*l).unwrap().unwrap() == *raw);
        }
        assert_eq!(c.check_refs().unwrap(), Vec::<String>::new());
        assert!(!pending(&*c.db).unwrap());
    }
}
