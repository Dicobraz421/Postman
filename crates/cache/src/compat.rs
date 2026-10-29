//! Прежний API писем (ключи кэша `[<аккаунт>/]<id>`, [`EmailRow`]) поверх формата 6 — до S8-09, чтобы ядро
//! собиралось. Аккаунт ключа — учётка связи с сервером (основной — пустая строка), ключ папки — папка учётки с этим
//! id сервера, «все папки» аккаунта ([`all_folders`]) — [`Scope::All`]. Письма без действующей связи (перенесённые,
//! локальные) этому API не видны. Тела, блобы и S/MIME остаются по ключу кэша.

use std::collections::{HashMap, HashSet};

use redb::{ReadOnlyTable, ReadTransaction, ReadableDatabase, ReadableTable, WriteTransaction};

use crate::folder::{self, FOLDER, FOLDER_REMOTE, FolderId, FolderRow, Owner, Scope};
use crate::letter::{BY_FOLDER, BY_KEYWORD, BY_THREAD, COUNT, KEYWORD_COUNT, LETTER, LetterId, LetterRow, Letters, RECIPIENTS};
use crate::remote::{LETTER_REMOTE, REMOTE, RemoteRef};
use crate::{BODY, Cache, EmailRow, MAX_ID, MailboxRow, Patch, Recipients, Result, Scan, dec, enc, scoped, unscoped};

/// Учётка ключа: основной аккаунт — пустая строка.
pub(crate) fn account(key: &str) -> (&str, &str) {
    let (a, id) = unscoped(key);
    (a.unwrap_or(""), id)
}

fn key_of(account: &str, id: &str) -> String {
    scoped((!account.is_empty()).then_some(account), id)
}

/// Ключ папки прежнего API; у локальной папки его нет.
fn folder_key(row: &FolderRow) -> Option<String> {
    match (&row.owner, &row.remote) {
        (Owner::Account(a), Some(r)) => Some(key_of(a, r)),
        _ => None,
    }
}

/// Строка формата 6 → [`EmailRow`]: ключ — по первой действующей связи; без неё — `None`.
fn email(row: LetterRow, refs: &[RemoteRef], mut folder: impl FnMut(FolderId) -> Result<Option<String>>) -> Result<Option<EmailRow>> {
    let Some(link) = refs.iter().find(|r| !r.detached) else { return Ok(None) };
    let mut mailbox_ids = Vec::with_capacity(row.folders.len());
    for f in &row.folders {
        mailbox_ids.extend(folder(*f)?);
    }
    Ok(Some(EmailRow {
        id: key_of(&link.account, &link.id),
        thread_id: row.thread_id,
        mailbox_ids,
        received_at: row.received_at,
        subject: row.subject,
        from_name: row.from_name,
        from_addr: row.from_addr,
        preview: row.preview,
        seen: row.seen,
        flagged: row.flagged,
        has_attachment: row.has_attachment,
        size: row.size,
        keywords: row.keywords,
    }))
}

/// [`EmailRow`] → строка формата 6; папки по ключам — известные или новые. Содержимое и место — из `old`.
pub(crate) fn letter(tx: &WriteTransaction, e: &EmailRow, old: Option<&LetterRow>) -> Result<LetterRow> {
    let mut folders = Vec::with_capacity(e.mailbox_ids.len());
    for m in &e.mailbox_ids {
        let (a, id) = account(m);
        folders.push(folder::ensure_remote(tx, a, id)?);
    }
    Ok(LetterRow {
        thread_id: e.thread_id.clone(),
        received_at: e.received_at,
        subject: e.subject.clone(),
        from_name: e.from_name.clone(),
        from_addr: e.from_addr.clone(),
        preview: e.preview.clone(),
        seen: e.seen,
        flagged: e.flagged,
        has_attachment: e.has_attachment,
        size: e.size,
        keywords: e.keywords.clone(),
        folders,
        content: old.and_then(|o| o.content),
        place: old.map(|o| o.place).unwrap_or_default(),
    })
}

/// Строка письма для записи: [`EmailRow`] письма `id` (через ключи папок записи).
fn email_in(l: &mut Letters, id: LetterId, row: LetterRow) -> Result<Option<EmailRow>> {
    let refs = l.refs(id)?;
    let tx = l.tx;
    email(row, &refs, |f| Ok(folder::get(tx, f)?.as_ref().and_then(folder_key)))
}

/// Чтение: письма и папки по ключам прежнего API.
struct View {
    tx: ReadTransaction,
    letter: ReadOnlyTable<u64, &'static [u8]>,
    remote: ReadOnlyTable<(&'static str, &'static str), u64>,
    letter_remote: ReadOnlyTable<u64, &'static [u8]>,
    folder: ReadOnlyTable<u64, &'static [u8]>,
    folder_remote: ReadOnlyTable<(&'static str, &'static str), u64>,
    keys: HashMap<u64, Option<String>>,
}

impl View {
    fn open(c: &Cache) -> Result<Self> {
        let tx = c.db.begin_read()?;
        Ok(Self {
            letter: tx.open_table(LETTER)?,
            remote: tx.open_table(REMOTE)?,
            letter_remote: tx.open_table(LETTER_REMOTE)?,
            folder: tx.open_table(FOLDER)?,
            folder_remote: tx.open_table(FOLDER_REMOTE)?,
            keys: HashMap::new(),
            tx,
        })
    }

    fn folder_id(&self, key: &str) -> Result<Option<u64>> {
        let (a, id) = account(key);
        Ok(self.folder_remote.get((a, id))?.map(|v| v.value()))
    }

    /// Ключ индекса ключевых слов для ключа папки или «всех папок» аккаунта.
    fn scope(&self, key: &str) -> Result<Option<u64>> {
        match account(key) {
            (a, "*") => folder::scope_key(&self.tx, &Scope::All(Owner::Account(a.to_owned()))),
            _ => self.folder_id(key),
        }
    }

    /// Письмо по ключу — только по действующей связи.
    fn letter_id(&self, key: &str) -> Result<Option<u64>> {
        let (a, id) = account(key);
        let Some(letter) = self.remote.get((a, id))?.map(|v| v.value()) else { return Ok(None) };
        let detached = self.refs(letter)?.iter().any(|r| r.account == a && r.id == id && r.detached);
        Ok((!detached).then_some(letter))
    }

    fn refs(&self, id: u64) -> Result<Vec<RemoteRef>> {
        Ok(self.letter_remote.get(id)?.map(|v| dec(v.value())).transpose()?.unwrap_or_default())
    }

    fn email(&mut self, id: u64) -> Result<Option<EmailRow>> {
        let Some(row) = self.letter.get(id)?.map(|v| dec::<LetterRow>(v.value())).transpose()? else { return Ok(None) };
        self.email_of(id, row)
    }

    fn email_of(&mut self, id: u64, row: LetterRow) -> Result<Option<EmailRow>> {
        let refs = self.refs(id)?;
        let (folder, keys) = (&self.folder, &mut self.keys);
        email(row, &refs, |f| {
            if let Some(k) = keys.get(&f.0) {
                return Ok(k.clone());
            }
            let k = folder.get(f.0)?.map(|v| dec::<FolderRow>(v.value())).transpose()?.as_ref().and_then(folder_key);
            keys.insert(f.0, k.clone());
            Ok(k)
        })
    }

    /// Письма диапазона индекса по порядку.
    fn emails(&mut self, ids: impl IntoIterator<Item = Result<u64>>) -> Result<Vec<EmailRow>> {
        let mut out = Vec::new();
        for id in ids {
            out.extend(self.email(id?)?);
        }
        Ok(out)
    }

    fn by_folder(&self, folder: u64) -> Result<redb::Range<'_, (u64, i64, u64), ()>> {
        Ok(self.tx.open_table(BY_FOLDER)?.range((folder, i64::MIN, 0)..=(folder, i64::MAX, u64::MAX))?)
    }
}

/// Папки прежнего API — и папками формата 6 (имя, роль, родитель).
pub(crate) fn sync_folders(tx: &WriteTransaction, rows: &[MailboxRow]) -> Result<()> {
    for m in rows {
        let (a, id) = account(&m.id);
        let known = folder::ensure_remote(tx, a, id)?;
        let parent = m.parent_id.as_deref().map(|p| {
            let (pa, pid) = account(p);
            folder::ensure_remote(tx, pa, pid)
        });
        let row = FolderRow {
            owner: Owner::Account(a.to_owned()),
            parent: parent.transpose()?,
            name: m.name.clone(),
            role: m.role.clone(),
            remote: Some(id.to_owned()),
        };
        folder::put(tx, Some(known), &row)?;
    }
    Ok(())
}

impl Cache {
    /// Письма и папки аккаунта `account` со всеми записями о них.
    pub(crate) fn forget_emails(&self, account: &str) -> Result<()> {
        let ids: Vec<String> = {
            let tx = self.db.begin_read()?;
            let remote = tx.open_table(REMOTE)?;
            let mut ids = Vec::new();
            for r in remote.range((account, "")..=(account, MAX_ID))? {
                ids.push(r?.0.value().1.to_owned());
            }
            ids
        };
        let tx = self.db.begin_write()?;
        {
            let mut l = Letters::open(&tx)?;
            for id in &ids {
                l.drop_remote(account, id)?;
            }
            l.finish()?;
            let mut body = tx.open_table(BODY)?;
            for id in &ids {
                body.remove(key_of(account, id).as_str())?;
            }
            let owner = Owner::Account(account.to_owned());
            let mut gone = Vec::new();
            for r in tx.open_table(FOLDER)?.iter()? {
                let (k, v) = r?;
                if dec::<FolderRow>(v.value())?.owner == owner {
                    gone.push(FolderId(k.value()));
                }
            }
            for f in gone {
                folder::remove(&tx, f)?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn upsert_emails(&self, rows: &[EmailRow]) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut l = Letters::open(&tx)?;
            for e in rows {
                let (a, id) = account(&e.id);
                let row = letter(&tx, e, None)?;
                l.upsert_remote(a, id, &row)?;
            }
            l.finish()?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Изменить письма одной записью: `change` правит [`EmailRow`] письма с ключом `key(item)` (`false` — оставить
    /// как было); ответ — новые строки (писем, которых нет, в нём нет).
    fn edit_emails<T>(&self, items: &[T], key: impl Fn(&T) -> &str, mut change: impl FnMut(&T, &mut EmailRow) -> bool) -> Result<Vec<EmailRow>> {
        let tx = self.db.begin_write()?;
        let mut out = Vec::new();
        {
            let mut l = Letters::open(&tx)?;
            for item in items {
                let (a, id) = account(key(item));
                let Some(letter_id) = l.attached(a, id)? else { continue };
                let Some(old) = l.get(letter_id)? else { continue };
                let Some(mut e) = email_in(&mut l, letter_id, old.clone())? else { continue };
                if !change(item, &mut e) {
                    continue;
                }
                let new = letter(&tx, &e, Some(&old))?;
                l.put(letter_id, Some(&old), &new)?;
                out.push(e);
            }
            l.finish()?;
        }
        tx.commit()?;
        Ok(out)
    }

    /// Правки писем одной записью (S5-04): папки, индексы и счётчики — как при записи с сервера. Ответ — новые
    /// строки; писем, которых нет, в нём нет.
    pub fn patch_emails(&self, patches: &[Patch]) -> Result<Vec<EmailRow>> {
        self.edit_emails(
            patches,
            |p| p.id.as_str(),
            |p, row| {
                p.apply(row);
                !p.is_empty()
            },
        )
    }

    /// Перенести письмо из ящика `from` в `to` (удаление в «Удалённые» — до ответа сервера, чтобы список
    /// изменился сразу). `false` — письма нет или оно не в `from`.
    pub fn move_email(&self, id: &str, from: &str, to: &str) -> Result<bool> {
        let moved = self.edit_emails(
            &[id],
            |id| id,
            |_, row| {
                if !row.mailbox_ids.iter().any(|m| m == from) {
                    return false;
                }
                row.mailbox_ids.retain(|m| m != from);
                if !row.mailbox_ids.iter().any(|m| m == to) {
                    row.mailbox_ids.push(to.to_owned());
                }
                true
            },
        )?;
        Ok(!moved.is_empty())
    }

    pub fn delete_emails(&self, ids: &[String]) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut l = Letters::open(&tx)?;
            for key in ids {
                let (a, id) = account(key);
                l.drop_remote(a, id)?;
            }
            l.finish()?;
            let mut body = tx.open_table(BODY)?;
            for key in ids {
                body.remove(key.as_str())?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn count(&self, mailbox_id: &str) -> Result<usize> {
        let v = View::open(self)?;
        let Some(f) = v.folder_id(mailbox_id)? else { return Ok(0) };
        Ok(v.tx.open_table(COUNT)?.get(f)?.map_or(0, |n| n.value() as usize))
    }

    /// Окно списка: новые сверху. Для виртуального скролла.
    pub fn page(&self, mailbox_id: &str, offset: usize, limit: usize) -> Result<Vec<EmailRow>> {
        let mut v = View::open(self)?;
        let Some(f) = v.folder_id(mailbox_id)? else { return Ok(Vec::new()) };
        let ids: Vec<Result<u64>> = v.by_folder(f)?.skip(offset).take(limit).map(|k| Ok(k?.0.value().2)).collect();
        v.emails(ids)
    }

    /// Письма беседы (ключ цепочки, `EmailRow::thread_id`) из всех папок, старые первыми.
    pub fn thread(&self, thread_id: &str) -> Result<Vec<EmailRow>> {
        let mut v = View::open(self)?;
        let by = v.tx.open_table(BY_THREAD)?;
        let ids: Vec<Result<u64>> = by.range((thread_id, i64::MIN, 0)..=(thread_id, i64::MAX, u64::MAX))?.map(|k| Ok(k?.0.value().2)).collect();
        v.emails(ids)
    }

    /// Сколько писем в беседе (для переключателя «Беседа (N)»).
    pub fn thread_len(&self, thread_id: &str) -> Result<usize> {
        let tx = self.db.begin_read()?;
        let by = tx.open_table(BY_THREAD)?;
        Ok(by.range((thread_id, i64::MIN, 0)..=(thread_id, i64::MAX, u64::MAX))?.count())
    }

    /// Строки по id в заданном порядке (результаты поиска); отсутствующие пропускаются.
    pub fn rows_by_ids(&self, ids: &[String]) -> Result<Vec<EmailRow>> {
        let mut v = View::open(self)?;
        let mut out = Vec::with_capacity(ids.len());
        for key in ids {
            if let Some(id) = v.letter_id(key)? {
                out.extend(v.email(id)?);
            }
        }
        Ok(out)
    }

    /// Оставить из `ids` только письма ящика, сохранив порядок.
    pub fn filter_mailbox(&self, ids: &[String], mailbox_id: &str) -> Result<Vec<String>> {
        Ok(self.rows_by_ids(ids)?.into_iter().filter(|e| e.mailbox_ids.iter().any(|m| m == mailbox_id)).map(|e| e.id).collect())
    }

    /// Все письма порциями (для первичного построения поискового индекса).
    pub fn all_rows(&self, offset: usize, limit: usize) -> Result<Vec<EmailRow>> {
        let mut v = View::open(self)?;
        let rows: Vec<(u64, LetterRow)> = v
            .letter
            .iter()?
            .skip(offset)
            .take(limit)
            .map(|r| {
                let (k, val) = r?;
                Ok((k.value(), dec(val.value())?))
            })
            .collect::<Result<_>>()?;
        let mut out = Vec::with_capacity(rows.len());
        for (id, row) in rows {
            out.extend(v.email_of(id, row)?);
        }
        Ok(out)
    }

    /// Письма без загруженного тела, свежие первыми (для фоновой предзагрузки).
    pub fn missing_bodies(&self, limit: usize) -> Result<Vec<String>> {
        let mut v = View::open(self)?;
        let body = v.tx.open_table(BODY)?;
        let letter = v.tx.open_table(LETTER)?;
        let mut out: Vec<(i64, String)> = Vec::new();
        for r in letter.iter()? {
            let (k, val) = r?;
            let row: LetterRow = dec(val.value())?;
            let at = row.received_at;
            let Some(e) = v.email_of(k.value(), row)? else { continue };
            if body.get(e.id.as_str())?.is_none() {
                out.push((at, e.id));
            }
        }
        out.sort_by_key(|e| std::cmp::Reverse(e.0));
        Ok(out.into_iter().take(limit).map(|(_, id)| id).collect())
    }

    /// Самое старое письмо ящика в кэше — якорь для догрузки следующей страницы.
    pub fn oldest(&self, mailbox_id: &str) -> Result<Option<String>> {
        let mut v = View::open(self)?;
        let Some(f) = v.folder_id(mailbox_id)? else { return Ok(None) };
        let last = v.by_folder(f)?.next_back().transpose()?.map(|(k, _)| k.value().2);
        Ok(match last {
            Some(id) => v.email(id)?.map(|e| e.id),
            None => None,
        })
    }

    /// Простой поиск по кэшу — запасной путь, если поисковый индекс недоступен.
    pub fn search(&self, query: &str, mailbox_id: Option<&str>, limit: usize) -> Result<Vec<String>> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        if words.is_empty() {
            return Ok(Vec::new());
        }
        let mut v = View::open(self)?;
        let folder = match mailbox_id {
            Some(mb) => match v.folder_id(mb)? {
                Some(f) => Some(FolderId(f)),
                None => return Ok(Vec::new()),
            },
            None => None,
        };
        let mut hits: Vec<(i64, String)> = Vec::new();
        let letter = v.tx.open_table(LETTER)?;
        for r in letter.iter()? {
            let (k, val) = r?;
            let row: LetterRow = dec(val.value())?;
            if folder.is_some_and(|f| !row.folders.contains(&f)) {
                continue;
            }
            let hay = format!("{} {} {} {}", row.subject, row.from_name, row.from_addr, row.preview).to_lowercase();
            if words.iter().all(|w| hay.contains(w.as_str()))
                && let Some(e) = v.email_of(k.value(), row)?
            {
                hits.push((e.received_at, e.id));
            }
        }
        hits.sort_by_key(|h| std::cmp::Reverse(h.0));
        Ok(hits.into_iter().take(limit).map(|(_, id)| id).collect())
    }

    pub fn set_flags(&self, id: &str, seen: bool, flagged: bool) -> Result<()> {
        self.edit_emails(
            &[id],
            |id| id,
            |_, row| {
                row.seen = seen;
                row.flagged = flagged;
                true
            },
        )
        .map(|_| ())
    }

    /// Поставить `add` и снять `remove` (ключевые слова JMAP, в том числе `$seen` и `$flagged`) — до ответа сервера,
    /// чтобы список изменился сразу. Новая строка; `None` — письма нет.
    pub fn set_keywords(&self, id: &str, add: &[String], remove: &[String]) -> Result<Option<EmailRow>> {
        let mut rows = self.edit_emails(
            &[id],
            |id| id,
            |_, row| {
                for k in remove {
                    row.set_keyword(k, false);
                }
                for k in add {
                    row.set_keyword(k, true);
                }
                true
            },
        )?;
        Ok(rows.pop())
    }

    /// Писем с ключевым словом `keyword` в папке `mailbox` (ключ кэша) или во всех папках аккаунта
    /// ([`all_folders`](crate::all_folders)). `$seen` не считается (см. [`MailboxRow::unread`]).
    pub fn keyword_count(&self, keyword: &str, mailbox: &str) -> Result<usize> {
        let v = View::open(self)?;
        let Some(place) = v.scope(mailbox)? else { return Ok(0) };
        let k = keyword.to_ascii_lowercase();
        Ok(v.tx.open_table(KEYWORD_COUNT)?.get((k.as_str(), place))?.map_or(0, |n| n.value() as usize))
    }

    /// Все ключевые слова папки (или «всех папок») с числом писем — счётчики фильтров одним проходом.
    pub fn keyword_counts(&self, mailbox: &str) -> Result<Vec<(String, usize)>> {
        let v = View::open(self)?;
        let Some(place) = v.scope(mailbox)? else { return Ok(Vec::new()) };
        let mut out = Vec::new();
        for r in v.tx.open_table(KEYWORD_COUNT)?.iter()? {
            let (k, n) = r?;
            let (kw, p) = k.value();
            if p == place {
                out.push((kw.to_owned(), n.value() as usize));
            }
        }
        Ok(out)
    }

    /// Получатели писем (`id`, [`Recipients`]) — синхронизация пишет их вместе со строками. Писем, которых нет, —
    /// пропускаются.
    pub fn set_recipients(&self, rows: &[(String, Recipients)]) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut l = Letters::open(&tx)?;
            for (key, r) in rows {
                let (a, id) = account(key);
                if let Some(letter) = l.attached(a, id)? {
                    l.recipients.insert(letter.0, enc(r)?.as_slice())?;
                }
            }
            l.finish()?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Получатели письма; не записаны — пусто.
    pub fn recipients(&self, id: &str) -> Result<Recipients> {
        let v = View::open(self)?;
        let Some(letter) = v.letter_id(id)? else { return Ok(Recipients::default()) };
        Ok(v.tx.open_table(RECIPIENTS)?.get(letter)?.map(|r| dec(r.value())).transpose()?.unwrap_or_default())
    }

    /// Перебрать письма папки (новые сверху) или аккаунта (в порядке id сервера) с их получателями — одним чтением,
    /// без списка в памяти (фильтры и их счётчики, S5-03). `f` вернула `false` — остановиться.
    pub fn scan(&self, scan: &Scan, mut f: impl FnMut(EmailRow, Recipients) -> bool) -> Result<()> {
        let mut v = View::open(self)?;
        let recipients = v.tx.open_table(RECIPIENTS)?;
        let ids: Vec<u64> = match scan {
            Scan::Mailbox(mb) => match v.folder_id(mb)? {
                Some(folder) => v.by_folder(folder)?.map(|k| Ok(k?.0.value().2)).collect::<Result<_>>()?,
                None => Vec::new(),
            },
            Scan::Account(acc) => {
                let a = acc.as_deref().unwrap_or("");
                let mut ids = Vec::new();
                for r in v.remote.range((a, "")..=(a, MAX_ID))? {
                    let (k, letter) = r?;
                    if v.letter_id(&key_of(a, k.value().1))?.is_some() {
                        ids.push(letter.value());
                    }
                }
                ids
            }
        };
        let mut seen = HashSet::new();
        for id in ids {
            if !seen.insert(id) {
                continue;
            }
            let Some(e) = v.email(id)? else { continue };
            let to = recipients.get(id)?.map(|r| dec(r.value())).transpose()?.unwrap_or_default();
            if !f(e, to) {
                break;
            }
        }
        Ok(())
    }

    /// Сколько писем папки получено не раньше `since` (Unix-секунды) — группы по датам без чтения строк.
    pub fn count_since(&self, mailbox_id: &str, since: i64) -> Result<usize> {
        let v = View::open(self)?;
        let Some(f) = v.folder_id(mailbox_id)? else { return Ok(0) };
        let by = v.tx.open_table(BY_FOLDER)?;
        Ok(by.range((f, i64::MIN, 0)..=(f, -since, u64::MAX))?.count())
    }

    /// Письма со словом, которое начинается с `prefix` (`pm.due` — и `pm.due.2026-10-24`), в папке или во всех
    /// папках аккаунта; до `limit` писем, без повторов (S5-04: сроки жизненного цикла — слова с датой).
    pub fn keyword_prefix(&self, prefix: &str, mailbox: &str, limit: usize) -> Result<Vec<EmailRow>> {
        let mut v = View::open(self)?;
        let Some(place) = v.scope(mailbox)? else { return Ok(Vec::new()) };
        let by = v.tx.open_table(BY_KEYWORD)?;
        let p = prefix.to_ascii_lowercase();
        let mut seen = HashSet::new();
        let mut ids = Vec::new();
        for r in by.range((p.as_str(), 0, i64::MIN, 0)..)? {
            let (k, _) = r?;
            let (kw, at, _, id) = k.value();
            if !kw.starts_with(p.as_str()) {
                break;
            }
            if at == place && seen.insert(id) {
                ids.push(id);
            }
        }
        let mut out = Vec::new();
        for id in ids {
            out.extend(v.email(id)?);
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    /// Окно писем с ключевым словом в папке (или во всех папках аккаунта), новые сверху.
    pub fn keyword_page(&self, keyword: &str, mailbox: &str, offset: usize, limit: usize) -> Result<Vec<EmailRow>> {
        let mut v = View::open(self)?;
        let Some(place) = v.scope(mailbox)? else { return Ok(Vec::new()) };
        let by = v.tx.open_table(BY_KEYWORD)?;
        let k = keyword.to_ascii_lowercase();
        let (lo, hi) = ((k.as_str(), place, i64::MIN, 0), (k.as_str(), place, i64::MAX, u64::MAX));
        let ids: Vec<Result<u64>> = by.range(lo..=hi)?.skip(offset).take(limit).map(|r| Ok(r?.0.value().3)).collect();
        v.emails(ids)
    }
}
