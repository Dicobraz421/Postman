//! Перенос хранилища формата 5 в формат 6 (ADR 0006 п. 6): рядом новый файл `*.v6`, по готовности — замена
//! переименованием, прежний файл — `*.v5.bak` до следующего удачного открытия. Сбой — новый файл удаляется, прежний
//! откладывается в `*.v5.bak` как есть, база начинается пустой: её наполняет синхронизация (в формате 5 данных «только
//! в базе» нет).
//!
//! Письму — [`LetterId`](crate::LetterId), связь с сервером — из ключа ([`unscoped`](crate::unscoped)): учётка —
//! аккаунт ключа (основная, `@2`, общий ящик `<ящик>` или `@2.<ящик>`), id сервера — остаток ключа как есть (IMAP —
//! `f<папка>.<UIDVALIDITY>.<UID>`, EWS — `m<Id>`), его разбирают плагины, как прежде. Письмо целиком из `blob` (по
//! [`BodyRow::blob_id`]) — в `content`. Тела, S/MIME, состояния синхронизации, задачи и блобы — по прежним ключам:
//! так их читает формат 6 до S8-09.

use std::path::Path;
use std::sync::Arc;

use redb::{Database, Key, ReadOnlyTable, ReadTransaction, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition, Value};

use crate::content::SplitPart;
use crate::letter::Letters;
use crate::{
    ACCOUNT, BACKUP, BLOB, BODY, BodyRow, CACHE_BYTES, CERTS, Cache, EmailRow, Error, MAILBOX, MailboxRow, Migrated, Result, SMIME, STATE, TASK,
    TASK_ID, TRUSTED, beside, compat, dec, remove_if_exists,
};

/// Новый файл на время переноса.
const NEW: &str = ".v6";
/// Писем в одной транзакции записи.
const BATCH: usize = 5_000;

/// Таблицы писем формата 5: ключ кэша → [`EmailRow`], ключ кэша → [`crate::Recipients`].
const EMAIL: TableDefinition<&str, &[u8]> = TableDefinition::new("email");
const RECIPIENTS: TableDefinition<&str, &[u8]> = TableDefinition::new("recipients");

/// Разбиение письма целиком на листовые части; ошибка — текст для журнала.
pub type Split<'a> = Box<dyn FnMut(&[u8]) -> std::result::Result<Vec<SplitPart>, String> + 'a>;

/// Ход и разбиение писем при переносе ([`Cache::open_with`]).
pub struct Migration<'a> {
    /// Перенесено писем, всего писем — после каждой пачки (строка «Обновление хранилища: N %»).
    pub progress: Box<dyn FnMut(u64, u64) + 'a>,
    /// Листовые части письма целиком (воркер, `MimeSplit`); `None` — письма хранятся целиком. Ошибка прерывает перенос
    /// ([`Migrated::Fresh`]): письмо, которое воркер не разобрал, отдаётся пустым списком — оно хранится целиком.
    pub split: Option<Split<'a>>,
}

impl Default for Migration<'_> {
    fn default() -> Self {
        Self { progress: Box::new(|_, _| {}), split: None }
    }
}

/// Перенести файл `path` формата 5 в формат 6; открыть результат.
pub(crate) fn run(path: &Path, how: &mut Migration) -> Result<(Cache, Migrated)> {
    let (new, backup) = (beside(path, NEW), beside(path, BACKUP));
    match copy(path, &new, how).and_then(|()| swap(path, &new, &backup)) {
        Ok(()) => {
            let c = Cache { db: Arc::new(Cache::create(path)?) };
            c.init()?;
            Ok((c, Migrated::Done))
        }
        Err(e) => {
            remove_if_exists(&new)?;
            remove_if_exists(&backup)?;
            std::fs::rename(path, &backup)?;
            let (c, _) = Cache::fresh(path, String::new())?;
            Ok((c, Migrated::Fresh(format!("migration from format 5 failed: {e}"))))
        }
    }
}

/// Новый файл — на место прежнего, прежний — в `backup`. Не вышло — прежний на месте.
fn swap(path: &Path, new: &Path, backup: &Path) -> Result<()> {
    remove_if_exists(backup)?;
    std::fs::rename(path, backup)?;
    if let Err(e) = std::fs::rename(new, path) {
        std::fs::rename(backup, path)?;
        return Err(e.into());
    }
    Ok(())
}

fn optional<K: Key + 'static, V: Value + 'static>(tx: &ReadTransaction, def: TableDefinition<K, V>) -> Result<Option<ReadOnlyTable<K, V>>> {
    match tx.open_table(def) {
        Ok(t) => Ok(Some(t)),
        // Таблицы, которых в файле ещё не было (задачи — новее формата 5).
        Err(redb::TableError::TableDoesNotExist(_)) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Таблица как есть.
fn as_is<K: Key + 'static, V: Value + 'static>(from: &ReadTransaction, to: &redb::WriteTransaction, def: TableDefinition<K, V>) -> Result<()> {
    let Some(src) = optional(from, def)? else { return Ok(()) };
    let mut dst = to.open_table(def)?;
    for r in src.iter()? {
        let (k, v) = r?;
        dst.insert(k.value(), v.value())?;
    }
    Ok(())
}

/// Всё из `from` (формат 5) — в новый файл `to` формата 6.
fn copy(from: &Path, to: &Path, how: &mut Migration) -> Result<()> {
    remove_if_exists(to)?;
    let old = Database::builder().set_cache_size(CACHE_BYTES).open_read_only(from)?;
    let new = Cache { db: Arc::new(Cache::create(to)?) };
    new.init()?;
    let src = old.begin_read()?;

    let tx = new.db.begin_write()?;
    for t in [MAILBOX, ACCOUNT, BODY, BLOB, SMIME, CERTS, TASK] {
        as_is(&src, &tx, t)?;
    }
    as_is(&src, &tx, TRUSTED)?;
    as_is(&src, &tx, STATE)?;
    as_is(&src, &tx, TASK_ID)?;
    if let Some(t) = optional(&src, MAILBOX)? {
        let rows: Vec<MailboxRow> = t.iter()?.map(|r| dec(r?.1.value())).collect::<Result<_>>()?;
        compat::sync_folders(&tx, &rows)?;
    }
    tx.commit()?;

    let Some(email) = optional(&src, EMAIL)? else {
        (how.progress)(0, 0);
        return Ok(());
    };
    let recipients = optional(&src, RECIPIENTS)?;
    let body = optional(&src, BODY)?;
    let blob = optional(&src, BLOB)?;
    let total = email.len()?;
    let mut done = 0;
    (how.progress)(done, total);
    let mut rows = email.iter()?;
    loop {
        let tx = new.db.begin_write()?;
        let mut whole = Vec::new();
        let mut n = 0;
        {
            let mut l = Letters::open(&tx)?;
            for r in rows.by_ref().take(BATCH) {
                let (k, v) = r?;
                let key = k.value();
                let (account, id) = compat::account(key);
                let row = compat::letter(&tx, &dec::<EmailRow>(v.value())?, None)?;
                let letter = l.upsert_remote(account, id, &row)?;
                if let Some(t) = &recipients
                    && let Some(r) = t.get(key)?
                {
                    l.recipients.insert(letter.0, r.value())?;
                }
                if let Some(t) = &body
                    && let Some(b) = t.get(key)?
                    && let Some(blob_id) = dec::<BodyRow>(b.value())?.blob_id
                {
                    whole.push((letter, blob_id));
                }
                n += 1;
            }
            l.finish()?;
        }
        tx.commit()?;
        for (letter, blob_id) in whole {
            let Some(raw) = blob.as_ref().map(|t| t.get(blob_id.as_str())).transpose()?.flatten() else { continue };
            let raw = raw.value();
            let split = match &mut how.split {
                Some(split) => split(raw).map_err(|e| Error(format!("split of letter {}: {e}", letter.0)))?,
                None => Vec::new(),
            };
            new.put_content(letter, raw, &split)?;
        }
        done += n;
        (how.progress)(done, total);
        if n < BATCH as u64 {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use redb::WriteTransaction;

    use super::*;
    use crate::content::{Encoding, Hash};
    use crate::{AccountRow, META, Recipients, Rights, TaskRow, all_folders, enc};

    const IMAP: &str = "@3/f0123456789abcdef.7.42";
    const IMAP_FOLDER: &str = "@3/f0123456789abcdef";
    const EWS: &str = "@4/mAAMkAD-x_y=";

    fn email(id: &str, at: i64, mbs: &[&str], kws: &[&str]) -> EmailRow {
        EmailRow {
            id: id.into(),
            thread_id: format!("t-{id}"),
            mailbox_ids: mbs.iter().map(|s| s.to_string()).collect(),
            received_at: at,
            subject: format!("s {id}"),
            from_name: "Иван".into(),
            from_addr: "ivan@x.ru".into(),
            preview: "привет".into(),
            seen: false,
            flagged: false,
            has_attachment: false,
            size: 10,
            keywords: kws.iter().map(|k| k.to_string()).collect(),
        }
    }

    fn mailbox(id: &str, parent: Option<&str>) -> MailboxRow {
        MailboxRow {
            id: id.into(),
            parent_id: parent.map(Into::into),
            name: format!("n {id}"),
            role: None,
            sort_order: 0,
            total: 1,
            unread: 0,
            rights: Rights::ALL,
        }
    }

    fn emails() -> Vec<EmailRow> {
        let mut e1 = email("e1", 10, &["inbox"], &["pm.cat.a"]);
        e1.seen = true;
        let mut e2 = email("e2", 20, &["inbox", "arch"], &[]);
        e2.flagged = true;
        vec![
            e1,
            e2,
            email("@2/e1", 15, &["@2/inbox"], &["pm.cat.a"]),
            email("s1/e1", 5, &["s1/inbox"], &[]),
            email("@2.s9/e1", 6, &["@2.s9/inbox"], &["$answered"]),
            email(IMAP, 7, &[IMAP_FOLDER], &[]),
            email(EWS, 8, &["@4/ffedcba9876543210"], &["pm.cat.b"]),
        ]
    }

    fn mailboxes() -> Vec<MailboxRow> {
        ["inbox", "@2/inbox", "s1/inbox", "@2.s9/inbox", IMAP_FOLDER, "@4/ffedcba9876543210"]
            .map(|id| mailbox(id, None))
            .into_iter()
            .chain([mailbox("arch", Some("inbox"))])
            .collect()
    }

    fn accounts() -> Vec<AccountRow> {
        ["@2", "@2.s9", "@3", "@4", "s1"].map(|id| AccountRow { id: id.into(), name: id.into(), is_personal: false, read_only: false }).into()
    }

    fn noise(len: usize) -> Vec<u8> {
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    fn b64(data: &[u8]) -> String {
        const B: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::new();
        for c in data.chunks(3) {
            let n = u32::from(c[0]) << 16 | u32::from(*c.get(1).unwrap_or(&0)) << 8 | u32::from(*c.get(2).unwrap_or(&0));
            for i in 0..4 {
                s.push(if i <= c.len() { B[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
            }
        }
        s.as_bytes().chunks(76).map(|l| format!("{}\r\n", std::str::from_utf8(l).unwrap())).collect()
    }

    /// Письмо с вложением base64 и его часть (как от воркера).
    fn raw_letter() -> (Vec<u8>, SplitPart) {
        let data = noise(100 * 1024);
        let head = "Subject: x\r\nContent-Type: multipart/mixed; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: application/pdf\r\n\
                    Content-Transfer-Encoding: base64\r\n\r\n";
        let body = b64(&data);
        let raw = format!("{head}{body}--b--\r\n").into_bytes();
        (raw, SplitPart { range: head.len()..head.len() + body.len(), encoding: Encoding::Base64, decoded: data })
    }

    fn body(blob: Option<&str>) -> BodyRow {
        BodyRow { text: Some("тело".into()), message_id: Some("m@x".into()), blob_id: blob.map(Into::into), ..BodyRow::default() }
    }

    fn task() -> TaskRow {
        TaskRow { uid: "u1".into(), server_id: Some("a".into()), json: "{}".into(), base: None, dirty: true, deleted: false }
    }

    fn put(tx: &WriteTransaction, def: TableDefinition<&str, &[u8]>, key: &str, value: &[u8]) {
        tx.open_table(def).unwrap().insert(key, value).unwrap();
    }

    /// Файл формата 5, как его писала прежняя версия.
    fn v5(dir: &Path) -> PathBuf {
        let path = dir.join("c.redb");
        let db = Database::create(&path).unwrap();
        let tx = db.begin_write().unwrap();
        tx.open_table(META).unwrap().insert("version", 5u64).unwrap();
        for m in mailboxes() {
            put(&tx, MAILBOX, &m.id, &enc(&m).unwrap());
        }
        for a in accounts() {
            put(&tx, ACCOUNT, &a.id, &enc(&a).unwrap());
        }
        for e in emails() {
            put(&tx, EMAIL, &e.id, &enc(&e).unwrap());
        }
        put(&tx, RECIPIENTS, "e1", &enc(&Recipients { to: vec!["me@x".into()], cc: vec![] }).unwrap());
        put(&tx, BODY, "e1", &enc(&body(Some("B1"))).unwrap());
        put(&tx, BODY, IMAP, &enc(&body(Some(IMAP))).unwrap());
        put(&tx, BODY, EWS, &enc(&body(None)).unwrap());
        put(&tx, BLOB, "B1", &raw_letter().0);
        put(&tx, BLOB, IMAP, b"Subject: imap\r\n\r\nhi\r\n");
        put(&tx, BLOB, "B9", b"attachment");
        put(&tx, SMIME, "e1", &enc(&(7u32, "valid".to_string())).unwrap());
        put(&tx, CERTS, "user@x", &enc(&vec![vec![1u8, 2]]).unwrap());
        put(&tx, TASK, "u1", &enc(&task()).unwrap());
        tx.open_table(TASK_ID).unwrap().insert("a", "u1").unwrap();
        let mut state = tx.open_table(STATE).unwrap();
        for (k, v) in [("Email", "s-own"), ("s1/Email", "s-s1"), ("@3/imap.f0123456789abcdef", "7:43")] {
            state.insert(k, v).unwrap();
        }
        drop(state);
        tx.open_table(TRUSTED).unwrap().insert("ivan@x.ru", ()).unwrap();
        tx.commit().unwrap();
        path
    }

    /// Учётные записи, общие ящики, IMAP и EWS, задачи, S/MIME: после переноса те же письма по папкам, счётчики,
    /// ключевые слова, тела; письма целиком — в `content`. Прежний файл — `*.v5.bak` до следующего открытия.
    #[test]
    fn format_5_is_migrated() {
        let dir = tempfile::Builder::new().prefix("postman-cache-").tempdir().unwrap();
        let path = v5(dir.path());
        // Остаток прерванного переноса.
        std::fs::write(beside(&path, NEW), b"half").unwrap();
        let mut progress = Vec::new();
        let mut splits = 0;
        let (c, how) = {
            let mut m = Migration {
                progress: Box::new(|done, total| progress.push((done, total))),
                split: Some(Box::new(|raw: &[u8]| {
                    splits += 1;
                    let (letter, part) = raw_letter();
                    Ok(if raw == letter.as_slice() { vec![part] } else { Vec::new() })
                })),
            };
            Cache::open_with(&path, &mut m).unwrap()
        };
        assert_eq!(how, Migrated::Done);
        assert_eq!(progress, [(0, 7), (7, 7)]);
        assert_eq!(splits, 2);
        assert!(!beside(&path, NEW).exists() && beside(&path, BACKUP).exists());

        let want = emails();
        let keys: Vec<String> = want.iter().map(|e| e.id.clone()).collect();
        assert_eq!(c.rows_by_ids(&keys).unwrap(), want);
        let page = |mb: &str| c.page(mb, 0, 10).unwrap().into_iter().map(|e| e.id).collect::<Vec<_>>();
        assert_eq!(page("inbox"), ["e2", "e1"]);
        assert_eq!(page("arch"), ["e2"]);
        assert_eq!(page("@2/inbox"), ["@2/e1"]);
        assert_eq!(page(IMAP_FOLDER), [IMAP]);
        assert_eq!((c.count("inbox").unwrap(), c.count("arch").unwrap(), c.count("s1/inbox").unwrap()), (2, 1, 1));
        assert_eq!(c.keyword_count("pm.cat.a", &all_folders(None)).unwrap(), 1);
        assert_eq!(c.keyword_count("pm.cat.a", &all_folders(Some("@2"))).unwrap(), 1);
        assert_eq!(c.keyword_count("$flagged", "arch").unwrap(), 1);
        assert_eq!(c.keyword_count("$answered", "@2.s9/inbox").unwrap(), 1);
        assert_eq!(c.keyword_count("pm.cat.b", &all_folders(Some("@4"))).unwrap(), 1);
        assert_eq!(c.thread("t-e2").unwrap().len(), 1);

        // Связь с сервером — учётка и id из ключа.
        let imap = c.letter_of("@3", "f0123456789abcdef.7.42").unwrap().unwrap();
        assert_eq!(c.remotes(imap).unwrap()[0].id, "f0123456789abcdef.7.42");
        assert!(c.letter_of("@4", "mAAMkAD-x_y=").unwrap().is_some());
        assert_ne!(c.letter_of("", "e1").unwrap(), c.letter_of("@2", "e1").unwrap());
        // Папки формата 6 — с владельцем и родителем.
        let arch = c.folder_of("", "arch").unwrap().unwrap();
        assert_eq!(c.folder(arch).unwrap().unwrap().parent, c.folder_of("", "inbox").unwrap());
        assert_eq!(c.mailboxes().unwrap().len(), mailboxes().len());
        assert_eq!(c.accounts().unwrap().len(), accounts().len());

        // Письма целиком: с частью и без, ровно исходные байты.
        let e1 = c.letter(c.letter_of("", "e1").unwrap().unwrap()).unwrap().unwrap();
        let raw = raw_letter().0;
        assert_eq!(e1.content, Some(Hash::of(&raw)));
        assert!(c.content(Hash::of(&raw)).unwrap().unwrap() == raw);
        assert_eq!(c.content(c.letter(imap).unwrap().unwrap().content.unwrap()).unwrap().unwrap(), b"Subject: imap\r\n\r\nhi\r\n");
        assert_eq!(c.check_index().unwrap(), Vec::<String>::new());
        assert_eq!(c.check_refs().unwrap(), Vec::<String>::new());

        // Остальное — по прежним ключам.
        assert_eq!(c.body("e1").unwrap(), Some(body(Some("B1"))));
        assert_eq!(c.body(EWS).unwrap(), Some(body(None)));
        assert_eq!(c.recipients("e1").unwrap().to, ["me@x"]);
        assert_eq!(c.smime::<(u32, String)>("e1").unwrap(), Some((7, "valid".into())));
        assert_eq!(c.certs::<Vec<Vec<u8>>>("user@x").unwrap(), Some(vec![vec![1, 2]]));
        assert_eq!(c.all_tasks().unwrap(), [task()]);
        assert_eq!(c.task_uid("a").unwrap().as_deref(), Some("u1"));
        assert_eq!(c.state("@3/imap.f0123456789abcdef").unwrap().as_deref(), Some("7:43"));
        assert_eq!(c.state("s1/Email").unwrap().as_deref(), Some("s-s1"));
        assert!(c.is_trusted("ivan@x.ru").unwrap());
        assert_eq!(c.blob("B9").unwrap().as_deref(), Some(&b"attachment"[..]));
        assert!(c.missing_bodies(10).unwrap().iter().all(|k| !["e1", IMAP, EWS].contains(&k.as_str())));

        // Следующее удачное открытие убирает прежний файл.
        drop(c);
        let (c, how) = Cache::open_with(&path, &mut Migration::default()).unwrap();
        assert_eq!(how, Migrated::Current);
        assert!(!beside(&path, BACKUP).exists());
        assert_eq!(c.count("inbox").unwrap(), 2);
    }

    /// Сбой посередине (разбиение с ошибкой): новый файл удалён, прежний цел, база пустая и рабочая.
    #[test]
    fn failure_keeps_the_old_file() {
        let dir = tempfile::Builder::new().prefix("postman-cache-").tempdir().unwrap();
        let path = v5(dir.path());
        let before = std::fs::read(&path).unwrap();
        let mut progress = Vec::new();
        let (c, how) = {
            let mut m = Migration {
                progress: Box::new(|done, total| progress.push((done, total))),
                split: Some(Box::new(|_: &[u8]| Err("worker is gone".into()))),
            };
            Cache::open_with(&path, &mut m).unwrap()
        };
        let Migrated::Fresh(reason) = how else { panic!("{how:?}") };
        assert!(reason.contains("worker is gone"), "{reason}");
        assert_eq!(progress, [(0, 7)], "до конца не дошло");
        assert!(!beside(&path, NEW).exists());
        assert!(std::fs::read(beside(&path, BACKUP)).unwrap() == before, "прежний файл не тронут");
        assert_eq!((c.count("inbox").unwrap(), c.mailboxes().unwrap().len(), c.all_tasks().unwrap().len()), (0, 0, 0));
        c.upsert_emails(&[email("n", 1, &["inbox"], &[])]).unwrap();
        assert_eq!(c.count("inbox").unwrap(), 1);
    }

    /// Файл формата 5 без писем и задач: перенос без хода по письмам.
    #[test]
    fn empty_format_5() {
        let dir = tempfile::Builder::new().prefix("postman-cache-").tempdir().unwrap();
        let path = dir.path().join("c.redb");
        {
            let db = Database::create(&path).unwrap();
            let tx = db.begin_write().unwrap();
            tx.open_table(META).unwrap().insert("version", 5u64).unwrap();
            tx.commit().unwrap();
        }
        let mut progress = Vec::new();
        let how = {
            let mut m = Migration { progress: Box::new(|done, total| progress.push((done, total))), split: None };
            Cache::open_with(&path, &mut m).unwrap().1
        };
        assert_eq!((how, progress), (Migrated::Done, vec![(0, 0)]));
    }
}
