//! Письма формата 6 (ADR 0006 п. 1, 3): строка метаданных с локальным [`LetterId`], индексы на числовых ключах,
//! перенос пачкой одной транзакцией.
//!
//! Все записи писем идут через [`Letters`]: он держит таблицы писем и индексов одной транзакции, правит индексы по
//! разнице старой и новой строки, а счётчики копит и пишет в конце ([`Letters::finish`]).

use std::collections::{BTreeSet, HashMap};

use redb::{ReadableDatabase, ReadableTable, Table, TableDefinition, WriteTransaction};
use serde::{Deserialize, Serialize};

use crate::content::Hash;
use crate::folder::{self, FolderId, Owner, Scope};
use crate::remote::{LETTER_REMOTE, REMOTE};
use crate::{Cache, Error, FLAGGED, META, Result, dec, enc};

/// Локальный id письма: выдаётся счётчиком `meta.next_letter` и не меняется никогда — ни при переносе между
/// папками и учётками, ни при выгрузке на сервер. Интерфейс и поиск работают с ним.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LetterId(pub u64);

/// Задание переноса: общий номер всех пачек одного [`Cache::move_letters`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct JobId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Place {
    /// Есть на сервере своей учётки.
    #[default]
    Synced,
    /// Только в базе.
    Local,
    /// Переносится в другую учётку или «На этом компьютере»: шаги — в [`Cache::outbound`].
    Outbound(JobId),
}

/// Строка письма: то же, что [`crate::EmailRow`], без ключа сервера, с папками, содержимым и местом.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LetterRow {
    /// Ключ цепочки (беседы).
    pub thread_id: String,
    /// Unix-время, секунды.
    pub received_at: i64,
    pub subject: String,
    pub from_name: String,
    pub from_addr: String,
    pub preview: String,
    pub seen: bool,
    pub flagged: bool,
    pub has_attachment: bool,
    pub size: u64,
    /// Как у [`crate::EmailRow::keywords`]: без `$seen` и `$flagged`, в нижнем регистре, по алфавиту.
    pub keywords: Vec<String>,
    /// Членство в папках (у JMAP папок может быть несколько).
    pub folders: Vec<FolderId>,
    /// Письмо целиком; `None` — ещё не загружено. Ведёт кэш: запись с сервера не меняет.
    pub content: Option<Hash>,
    /// Ведёт кэш: запись с сервера не меняет.
    pub place: Place,
}

impl LetterRow {
    /// Ключевые слова в индексе: свои и `$flagged` (`$seen` не индексируется, как в формате 5).
    fn indexed(&self) -> impl Iterator<Item = &str> {
        self.keywords.iter().map(String::as_str).chain(self.flagged.then_some(FLAGGED))
    }
}

/// Шаг выгрузки письма (ADR 0006 п. 4); задание начинается с первого.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Step {
    /// Письмо целиком есть в базе (нет — загрузить у источника).
    Content,
    /// Выгрузить в папку цели.
    Upload,
    /// Сервер цели подтвердил: новая связь, `place = Synced`.
    Confirm,
    /// Удалить у источника и снять `detached`-связь.
    DeleteSource,
}

/// Строка задания переноса: куда и на каком шаге.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outbound {
    pub to: FolderId,
    pub step: Step,
}

/// Итог [`Cache::move_letters`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MoveTicket {
    /// Задание выгрузки; `None` — ни одному письму выгрузка не нужна (перенос внутри учётки или обратно).
    pub job: Option<JobId>,
    pub moved: usize,
    /// Сколько писем ждут выгрузки по заданию.
    pub outbound: usize,
}

/// Больше стольких писем перенос идёт пачками, каждая — своя транзакция (ADR 0006 п. 3, вопрос №8 S8).
pub const MOVE_BATCH: usize = 100_000;

pub(crate) const NEXT_LETTER: &str = "next_letter";
pub(crate) const NEXT_FOLDER: &str = "next_folder";
const NEXT_JOB: &str = "next_job";

pub(crate) const LETTER: TableDefinition<u64, &[u8]> = TableDefinition::new("letter");
/// (папка, −дата, письмо) → () — порядок списка.
pub(crate) const BY_FOLDER: TableDefinition<(u64, i64, u64), ()> = TableDefinition::new("by_folder");
/// Папка → число писем.
pub(crate) const COUNT: TableDefinition<u64, u64> = TableDefinition::new("count");
/// (ключевое слово, папка или «все папки» владельца, −дата, письмо) → ().
pub(crate) const BY_KEYWORD: TableDefinition<(&str, u64, i64, u64), ()> = TableDefinition::new("by_keyword");
/// (ключевое слово, папка или «все папки» владельца) → число писем; нулевые не хранятся.
pub(crate) const KEYWORD_COUNT: TableDefinition<(&str, u64), u64> = TableDefinition::new("keyword_count");
/// (цепочка, дата, письмо) → ().
pub(crate) const BY_THREAD: TableDefinition<(&str, i64, u64), ()> = TableDefinition::new("by_thread");
/// Письмо → [`crate::Recipients`].
pub(crate) const RECIPIENTS: TableDefinition<u64, &[u8]> = TableDefinition::new("recipients");
/// (задание, письмо) → [`Outbound`].
const OUTBOUND: TableDefinition<(u64, u64), &[u8]> = TableDefinition::new("outbound");

/// Таблицы формата 6 — при открытии кэша.
pub(crate) fn init(tx: &WriteTransaction) -> Result<()> {
    tx.open_table(LETTER)?;
    tx.open_table(BY_FOLDER)?;
    tx.open_table(COUNT)?;
    tx.open_table(BY_KEYWORD)?;
    tx.open_table(KEYWORD_COUNT)?;
    tx.open_table(BY_THREAD)?;
    tx.open_table(RECIPIENTS)?;
    tx.open_table(OUTBOUND)?;
    tx.open_table(REMOTE)?;
    tx.open_table(LETTER_REMOTE)?;
    tx.open_table(folder::FOLDER)?;
    tx.open_table(folder::FOLDER_REMOTE)?;
    tx.open_table(folder::SCOPE)?;
    Ok(())
}

/// Следующий id счётчика `key` в `meta` (с 1).
pub(crate) fn next_id(tx: &WriteTransaction, key: &str) -> Result<u64> {
    let mut meta = tx.open_table(META)?;
    let id = meta.get(key)?.map_or(1, |v| v.value());
    meta.insert(key, id + 1)?;
    Ok(id)
}

/// Таблицы писем одной транзакции записи. Таблицы папок и содержимого он не держит: их открывают на время.
pub(crate) struct Letters<'t> {
    pub(crate) tx: &'t WriteTransaction,
    letter: Table<'t, u64, &'static [u8]>,
    pub(crate) remote: Table<'t, (&'static str, &'static str), u64>,
    pub(crate) letter_remote: Table<'t, u64, &'static [u8]>,
    by_folder: Table<'t, (u64, i64, u64), ()>,
    by_keyword: Table<'t, (&'static str, u64, i64, u64), ()>,
    by_thread: Table<'t, (&'static str, i64, u64), ()>,
    pub(crate) recipients: Table<'t, u64, &'static [u8]>,
    outbound: Table<'t, (u64, u64), &'static [u8]>,
    owners: HashMap<FolderId, Owner>,
    scopes: HashMap<Owner, u64>,
    counts: HashMap<u64, i64>,
    keyword_counts: HashMap<(String, u64), i64>,
}

impl<'t> Letters<'t> {
    pub(crate) fn open(tx: &'t WriteTransaction) -> Result<Self> {
        Ok(Self {
            tx,
            letter: tx.open_table(LETTER)?,
            remote: tx.open_table(REMOTE)?,
            letter_remote: tx.open_table(LETTER_REMOTE)?,
            by_folder: tx.open_table(BY_FOLDER)?,
            by_keyword: tx.open_table(BY_KEYWORD)?,
            by_thread: tx.open_table(BY_THREAD)?,
            recipients: tx.open_table(RECIPIENTS)?,
            outbound: tx.open_table(OUTBOUND)?,
            owners: HashMap::new(),
            scopes: HashMap::new(),
            counts: HashMap::new(),
            keyword_counts: HashMap::new(),
        })
    }

    pub(crate) fn get(&self, id: LetterId) -> Result<Option<LetterRow>> {
        self.letter.get(id.0)?.map(|v| dec(v.value())).transpose()
    }

    /// Владелец папки; папки нет — ошибка: письмо не может ссылаться на несуществующую папку.
    pub(crate) fn owner(&mut self, f: FolderId) -> Result<Owner> {
        if let Some(o) = self.owners.get(&f) {
            return Ok(o.clone());
        }
        let row = folder::get(self.tx, f)?.ok_or_else(|| Error(format!("no folder {}", f.0)))?;
        self.owners.insert(f, row.owner.clone());
        Ok(row.owner)
    }

    fn scope(&mut self, owner: &Owner) -> Result<u64> {
        if let Some(s) = self.scopes.get(owner) {
            return Ok(*s);
        }
        let s = folder::scope_id(self.tx, owner)?;
        self.scopes.insert(owner.clone(), s);
        Ok(s)
    }

    /// Ключи письма в индексе ключевых слов: (слово, папка или «все папки» её владельца).
    fn keyword_keys(&mut self, row: &LetterRow) -> Result<BTreeSet<(String, u64)>> {
        let mut places: BTreeSet<u64> = row.folders.iter().map(|f| f.0).collect();
        for f in &row.folders {
            let owner = self.owner(*f)?;
            places.insert(self.scope(&owner)?);
        }
        Ok(row.indexed().flat_map(|k| places.iter().map(move |p| (k.to_owned(), *p))).collect())
    }

    /// Индексы и счётчики письма `id`: было `old`, стало `new`. Меняется только разница; дата другая — все ключи.
    fn index(&mut self, id: LetterId, old: Option<&LetterRow>, new: Option<&LetterRow>) -> Result<()> {
        let same_date = old.map(|r| r.received_at) == new.map(|r| r.received_at);
        let folders = |r: Option<&LetterRow>| r.map(|r| r.folders.iter().map(|f| f.0).collect::<BTreeSet<u64>>()).unwrap_or_default();
        let (of, nf) = (folders(old), folders(new));
        if let Some(o) = old {
            for f in of.iter().filter(|f| !(same_date && nf.contains(f))) {
                if self.by_folder.remove((*f, -o.received_at, id.0))?.is_some() {
                    *self.counts.entry(*f).or_default() -= 1;
                }
            }
        }
        if let Some(n) = new {
            for f in nf.iter().filter(|f| !(same_date && of.contains(f))) {
                if self.by_folder.insert((*f, -n.received_at, id.0), ())?.is_none() {
                    *self.counts.entry(*f).or_default() += 1;
                }
            }
        }

        let ok = match old {
            Some(o) => self.keyword_keys(o)?,
            None => BTreeSet::new(),
        };
        let nk = match new {
            Some(n) => self.keyword_keys(n)?,
            None => BTreeSet::new(),
        };
        if let Some(o) = old {
            for (kw, p) in ok.iter().filter(|k| !(same_date && nk.contains(*k))) {
                if self.by_keyword.remove((kw.as_str(), *p, -o.received_at, id.0))?.is_some() {
                    *self.keyword_counts.entry((kw.clone(), *p)).or_default() -= 1;
                }
            }
        }
        if let Some(n) = new {
            for (kw, p) in nk.iter().filter(|k| !(same_date && ok.contains(*k))) {
                if self.by_keyword.insert((kw.as_str(), *p, -n.received_at, id.0), ())?.is_none() {
                    *self.keyword_counts.entry((kw.clone(), *p)).or_default() += 1;
                }
            }
        }

        let thread = |r: Option<&LetterRow>| r.map(|r| (r.thread_id.clone(), r.received_at));
        let (ot, nt) = (thread(old), thread(new));
        if ot != nt {
            if let Some((t, at)) = ot {
                self.by_thread.remove((t.as_str(), at, id.0))?;
            }
            if let Some((t, at)) = nt {
                self.by_thread.insert((t.as_str(), at, id.0), ())?;
            }
        }
        Ok(())
    }

    /// Записать строку письма с индексами; `old` — прежняя строка (`None` — новое письмо).
    pub(crate) fn put(&mut self, id: LetterId, old: Option<&LetterRow>, new: &LetterRow) -> Result<()> {
        self.index(id, old, Some(new))?;
        self.letter.insert(id.0, enc(new)?.as_slice())?;
        Ok(())
    }

    /// Новое письмо: id из счётчика.
    pub(crate) fn create(&mut self, row: &LetterRow) -> Result<LetterId> {
        let id = LetterId(next_id(self.tx, NEXT_LETTER)?);
        self.put(id, None, row)?;
        Ok(id)
    }

    /// Удалить письмо целиком: строку, индексы, связи с сервером, получателей, строку задания.
    pub(crate) fn remove(&mut self, id: LetterId) -> Result<bool> {
        let Some(old) = self.get(id)? else { return Ok(false) };
        self.index(id, Some(&old), None)?;
        self.letter.remove(id.0)?;
        self.recipients.remove(id.0)?;
        for r in self.refs(id)? {
            self.remote.remove((r.account.as_str(), r.id.as_str()))?;
        }
        self.letter_remote.remove(id.0)?;
        if let Place::Outbound(job) = old.place {
            self.outbound.remove((job.0, id.0))?;
        }
        Ok(true)
    }

    /// Счётчики — в таблицы.
    pub(crate) fn finish(self) -> Result<()> {
        let mut count = self.tx.open_table(COUNT)?;
        for (f, d) in self.counts {
            let cur = count.get(f)?.map_or(0, |v| v.value());
            match cur.saturating_add_signed(d) {
                0 => count.remove(f)?,
                n => count.insert(f, n)?,
            };
        }
        let mut kw_count = self.tx.open_table(KEYWORD_COUNT)?;
        for ((kw, p), d) in self.keyword_counts {
            let cur = kw_count.get((kw.as_str(), p))?.map_or(0, |v| v.value());
            match cur.saturating_add_signed(d) {
                0 => kw_count.remove((kw.as_str(), p))?,
                n => kw_count.insert((kw.as_str(), p), n)?,
            };
        }
        Ok(())
    }
}

/// Сверить индексы и счётчики писем с пересчётом из строк; ответ — расхождения (пусто — сходится).
fn check_index(tx: &redb::ReadTransaction) -> Result<Vec<String>> {
    let letter = tx.open_table(LETTER)?;
    let folder = tx.open_table(folder::FOLDER)?;
    let scope = tx.open_table(folder::SCOPE)?;
    let mut by_folder = BTreeSet::new();
    let mut by_keyword = BTreeSet::new();
    let mut by_thread = BTreeSet::new();
    let mut counts: HashMap<u64, u64> = HashMap::new();
    let mut keyword_counts: HashMap<(String, u64), u64> = HashMap::new();
    let mut problems = Vec::new();
    for r in letter.iter()? {
        let (k, v) = r?;
        let (id, row): (u64, LetterRow) = (k.value(), dec(v.value())?);
        let mut places = BTreeSet::new();
        for f in &row.folders {
            places.insert(f.0);
            let Some(fr) = folder.get(f.0)? else {
                problems.push(format!("letter {id}: no folder {}", f.0));
                continue;
            };
            match scope.get(enc(&dec::<folder::FolderRow>(fr.value())?.owner)?.as_slice())? {
                Some(s) => places.insert(s.value()),
                None => {
                    problems.push(format!("letter {id}: no scope for the owner of folder {}", f.0));
                    false
                }
            };
        }
        for f in row.folders.iter().map(|f| f.0).collect::<BTreeSet<_>>() {
            by_folder.insert((f, -row.received_at, id));
            *counts.entry(f).or_default() += 1;
        }
        for kw in row.indexed() {
            for p in &places {
                by_keyword.insert((kw.to_owned(), *p, -row.received_at, id));
                *keyword_counts.entry((kw.to_owned(), *p)).or_default() += 1;
            }
        }
        by_thread.insert((row.thread_id.clone(), row.received_at, id));
    }
    let mut have = BTreeSet::new();
    for r in tx.open_table(BY_FOLDER)?.iter()? {
        have.insert(r?.0.value());
    }
    if have != by_folder {
        problems.push(format!("by_folder: {} keys, expected {}", have.len(), by_folder.len()));
    }
    let mut have = BTreeSet::new();
    for r in tx.open_table(BY_KEYWORD)?.iter()? {
        let (k, _) = r?;
        let (kw, p, at, id) = k.value();
        have.insert((kw.to_owned(), p, at, id));
    }
    if have != by_keyword {
        problems.push(format!("by_keyword: {} keys, expected {}", have.len(), by_keyword.len()));
    }
    let mut have = BTreeSet::new();
    for r in tx.open_table(BY_THREAD)?.iter()? {
        let (k, _) = r?;
        let (t, at, id) = k.value();
        have.insert((t.to_owned(), at, id));
    }
    if have != by_thread {
        problems.push(format!("by_thread: {} keys, expected {}", have.len(), by_thread.len()));
    }
    let mut have = HashMap::new();
    for r in tx.open_table(COUNT)?.iter()? {
        let (k, v) = r?;
        have.insert(k.value(), v.value());
    }
    if have != counts {
        problems.push(format!("count: {have:?}, expected {counts:?}"));
    }
    let mut have = HashMap::new();
    for r in tx.open_table(KEYWORD_COUNT)?.iter()? {
        let (k, v) = r?;
        let (kw, p) = k.value();
        have.insert((kw.to_owned(), p), v.value());
    }
    if have != keyword_counts {
        problems.push(format!("keyword_count: {have:?}, expected {keyword_counts:?}"));
    }
    Ok(problems)
}

impl Cache {
    pub fn letter(&self, id: LetterId) -> Result<Option<LetterRow>> {
        let tx = self.db.begin_read()?;
        tx.open_table(LETTER)?.get(id.0)?.map(|v| dec(v.value())).transpose()
    }

    /// Писем в папке.
    pub fn folder_count(&self, folder: FolderId) -> Result<usize> {
        let tx = self.db.begin_read()?;
        Ok(tx.open_table(COUNT)?.get(folder.0)?.map_or(0, |v| v.value() as usize))
    }

    /// Окно списка папки: новые сверху.
    pub fn folder_page(&self, folder: FolderId, offset: usize, limit: usize) -> Result<Vec<(LetterId, LetterRow)>> {
        let tx = self.db.begin_read()?;
        let by = tx.open_table(BY_FOLDER)?;
        let letter = tx.open_table(LETTER)?;
        let mut out = Vec::with_capacity(limit.min(1024));
        for k in by.range((folder.0, i64::MIN, 0)..=(folder.0, i64::MAX, u64::MAX))?.skip(offset).take(limit) {
            let id = k?.0.value().2;
            if let Some(v) = letter.get(id)? {
                out.push((LetterId(id), dec(v.value())?));
            }
        }
        Ok(out)
    }

    /// Писем с ключевым словом в папке или во всех папках владельца.
    pub fn keyword_count_in(&self, keyword: &str, scope: &Scope) -> Result<usize> {
        let tx = self.db.begin_read()?;
        let Some(place) = folder::scope_key(&tx, scope)? else { return Ok(0) };
        let k = keyword.to_ascii_lowercase();
        Ok(tx.open_table(KEYWORD_COUNT)?.get((k.as_str(), place))?.map_or(0, |v| v.value() as usize))
    }

    /// Перенести письма в папку `to` (ADR 0006 п. 3): членство, индексы и счётчики — одной транзакцией на пачку
    /// до [`MOVE_BATCH`] писем; содержимое не трогается. Письмо оказывается только в `to`.
    ///
    /// Цель — у другого владельца: связи с сервером источника помечаются `detached` (синхронизация источника не
    /// вернёт письмо, но увидит удаление), письму — `Outbound` и строка задания (все пачки — под одним номером).
    /// Цель — учётка, где у письма уже есть связь (перенос обратно), — связь снова действует, `Synced`, строка
    /// задания убирается; у письма без связей перенос в «На этом компьютере» — сразу `Local`. Писем, которых нет,
    /// и уже лежащих только в `to` — в итоге нет.
    pub fn move_letters(&self, ids: &[LetterId], to: FolderId) -> Result<MoveTicket> {
        self.move_in_batches(ids, to, MOVE_BATCH)
    }

    fn move_in_batches(&self, ids: &[LetterId], to: FolderId, batch_len: usize) -> Result<MoveTicket> {
        let mut ticket = MoveTicket::default();
        for batch in ids.chunks(batch_len) {
            let tx = self.db.begin_write()?;
            {
                let mut l = Letters::open(&tx)?;
                let target = l.owner(to)?;
                for &id in batch {
                    let Some(old) = l.get(id)? else { continue };
                    if !old.folders.is_empty() && old.folders.iter().all(|f| *f == to) {
                        continue;
                    }
                    let mut new = LetterRow { folders: vec![to], ..old.clone() };
                    let mut cross = false;
                    for f in &old.folders {
                        cross |= l.owner(*f)? != target;
                    }
                    if cross {
                        let mut refs = l.refs(id)?;
                        for r in &mut refs {
                            r.detached = target != Owner::Account(r.account.clone());
                        }
                        l.set_refs(id, &refs)?;
                        if let Place::Outbound(job) = old.place {
                            l.outbound.remove((job.0, id.0))?;
                        }
                        new.place = if refs.iter().any(|r| !r.detached) {
                            Place::Synced
                        } else if refs.is_empty() && target == Owner::Local {
                            Place::Local
                        } else {
                            let job = match ticket.job {
                                Some(j) => j,
                                None => *ticket.job.insert(JobId(next_id(&tx, NEXT_JOB)?)),
                            };
                            l.outbound.insert((job.0, id.0), enc(&Outbound { to, step: Step::Content })?.as_slice())?;
                            ticket.outbound += 1;
                            Place::Outbound(job)
                        };
                    } else if let Place::Outbound(job) = old.place {
                        // Ждёт выгрузки и переложено в другую папку той же цели: задание — в новую папку.
                        let step = l.outbound.get((job.0, id.0))?.map(|v| dec::<Outbound>(v.value())).transpose()?.map_or(Step::Content, |o| o.step);
                        l.outbound.insert((job.0, id.0), enc(&Outbound { to, step })?.as_slice())?;
                    }
                    l.put(id, Some(&old), &new)?;
                    ticket.moved += 1;
                }
                l.finish()?;
            }
            tx.commit()?;
        }
        Ok(ticket)
    }

    /// Удалить письма из базы: строки, индексы, связи, строки заданий.
    pub fn delete_letters(&self, ids: &[LetterId]) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut l = Letters::open(&tx)?;
            for id in ids {
                l.remove(*id)?;
            }
            l.finish()?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Строки задания переноса по письмам.
    pub fn outbound(&self, job: JobId) -> Result<Vec<(LetterId, Outbound)>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(OUTBOUND)?;
        t.range((job.0, 0)..=(job.0, u64::MAX))?
            .map(|r| {
                let (k, v) = r?;
                Ok((LetterId(k.value().1), dec(v.value())?))
            })
            .collect()
    }

    /// Пересчёт индексов и счётчиков писем с нуля и сверка с таблицами — для тестов и диагностики. Ответ —
    /// расхождения по-английски (для журнала); пусто — сходится.
    pub fn check_index(&self) -> Result<Vec<String>> {
        check_index(&self.db.begin_read()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FolderRow, RemoteRef};

    fn account_folder(c: &Cache, account: &str, remote: &str) -> FolderId {
        let row = FolderRow { owner: Owner::Account(account.into()), parent: None, name: remote.into(), role: None, remote: Some(remote.into()) };
        c.upsert_folder(&row).unwrap()
    }

    fn row(at: i64, folders: &[FolderId], keywords: &[&str]) -> LetterRow {
        LetterRow {
            thread_id: format!("t{at}"),
            received_at: at,
            subject: format!("s{at}"),
            keywords: keywords.iter().map(|k| k.to_string()).collect(),
            folders: folders.to_vec(),
            ..LetterRow::default()
        }
    }

    fn page(c: &Cache, f: FolderId) -> Vec<LetterId> {
        c.folder_page(f, 0, 100).unwrap().into_iter().map(|(id, _)| id).collect()
    }

    fn assert_index(c: &Cache) {
        assert_eq!(c.check_index().unwrap(), Vec::<String>::new());
    }

    fn all(account: &str) -> Scope {
        Scope::All(Owner::Account(account.into()))
    }

    /// Внутри учётки: членство, порядок, счётчики и метки по папкам; связи и место не меняются, задания нет.
    #[test]
    fn move_within_account() {
        let c = Cache::open_in_memory().unwrap();
        let (inbox, archive) = (account_folder(&c, "a", "in"), account_folder(&c, "a", "arch"));
        let x = c.upsert_remote("a", "x", &row(10, &[inbox], &["pm.cat.a"])).unwrap();
        let y = c.upsert_remote("a", "y", &row(20, &[inbox, archive], &[])).unwrap();
        assert_eq!(page(&c, inbox), [y, x]);
        let t = c.move_letters(&[x, y, LetterId(999)], archive).unwrap();
        assert_eq!(t, MoveTicket { job: None, moved: 2, outbound: 0 });
        assert_eq!((c.folder_count(inbox).unwrap(), c.folder_count(archive).unwrap()), (0, 2));
        assert_eq!(page(&c, archive), [y, x]);
        assert_eq!(c.keyword_count_in("pm.cat.a", &Scope::Folder(archive)).unwrap(), 1);
        assert_eq!(c.keyword_count_in("pm.cat.a", &Scope::Folder(inbox)).unwrap(), 0);
        assert_eq!(c.keyword_count_in("PM.CAT.A", &all("a")).unwrap(), 1);
        assert!(c.remotes(x).unwrap().iter().all(|r| !r.detached));
        assert_eq!(c.letter(x).unwrap().unwrap().place, Place::Synced);
        // Уже только в цели — не переносится.
        assert_eq!(c.move_letters(&[x], archive).unwrap().moved, 0);
        assert_index(&c);
        // Сверка не пустая: сбитый счётчик видно.
        let tx = c.db.begin_write().unwrap();
        tx.open_table(COUNT).unwrap().insert(inbox.0, 5).unwrap();
        tx.commit().unwrap();
        assert_eq!(c.check_index().unwrap().len(), 1);
    }

    /// Между учётками: связь источника `detached`, письмо ждёт выгрузки по заданию; синхронизация источника его не
    /// возвращает, но удаление видит; метки — в «всех папках» новой учётки.
    #[test]
    fn move_between_accounts() {
        let c = Cache::open_in_memory().unwrap();
        let (a_in, b_in) = (account_folder(&c, "a", "in"), account_folder(&c, "b", "in"));
        let x = c.upsert_remote("a", "x", &row(10, &[a_in], &["pm.cat.a"])).unwrap();
        let y = c.upsert_remote("a", "y", &row(20, &[a_in], &[])).unwrap();
        let t = c.move_letters(&[x, y], b_in).unwrap();
        let job = t.job.unwrap();
        assert_eq!((t.moved, t.outbound), (2, 2));
        assert_eq!(c.letter(x).unwrap().unwrap().place, Place::Outbound(job));
        assert_eq!(c.outbound(job).unwrap(), [x, y].map(|id| (id, Outbound { to: b_in, step: Step::Content })));
        assert!(c.remotes(x).unwrap()[0].detached);
        assert_eq!(c.letter_of("a", "x").unwrap(), None);
        assert_eq!((c.keyword_count_in("pm.cat.a", &all("a")).unwrap(), c.keyword_count_in("pm.cat.a", &all("b")).unwrap()), (0, 1));

        // Источник прислал письмо снова — оно не возвращается в старую папку.
        assert_eq!(c.upsert_remote("a", "x", &row(10, &[a_in], &["pm.cat.z"])).unwrap(), x);
        assert_eq!(c.letter(x).unwrap().unwrap().folders, [b_in]);
        assert_eq!((c.folder_count(a_in).unwrap(), c.folder_count(b_in).unwrap()), (0, 2));
        // Источник удалил письмо (шаг 4): связь снята, письмо на месте.
        assert_eq!(c.delete_remote("a", &["x".into()]).unwrap(), []);
        assert!(c.remotes(x).unwrap().is_empty());
        assert_eq!(page(&c, b_in), [y, x]);
        // Цель подтвердила выгрузку: новая связь.
        let link = RemoteRef { account: "b".into(), id: "bx".into(), data: vec![1], detached: false };
        c.attach_remote(x, link.clone()).unwrap();
        assert_eq!(c.letter_of("b", "bx").unwrap(), Some(x));
        assert!(c.attach_remote(y, link).is_err(), "id сервера уже у другого письма");
        assert_index(&c);
    }

    /// В «На этом компьютере» и обратно: туда — задание (содержимое и удаление у источника), обратно — прежняя связь
    /// снова действует, строка задания убрана. Письмо без связей в локальной папке — сразу `Local`.
    #[test]
    fn move_to_local_and_back() {
        let c = Cache::open_in_memory().unwrap();
        let a_in = account_folder(&c, "a", "in");
        let b_in = account_folder(&c, "b", "in");
        let archive = c.create_folder(None, "Архив").unwrap();
        let x = c.upsert_remote("a", "x", &row(10, &[a_in], &["pm.cat.a"])).unwrap();
        let job = c.move_letters(&[x], archive).unwrap().job.unwrap();
        assert_eq!(c.letter(x).unwrap().unwrap().place, Place::Outbound(job));
        assert_eq!(c.keyword_count_in("pm.cat.a", &Scope::All(Owner::Local)).unwrap(), 1);
        assert_eq!(c.letter_of("a", "x").unwrap(), None);

        let t = c.move_letters(&[x], a_in).unwrap();
        assert_eq!(t, MoveTicket { job: None, moved: 1, outbound: 0 });
        assert_eq!(c.letter(x).unwrap().unwrap().place, Place::Synced);
        assert!(c.outbound(job).unwrap().is_empty());
        assert_eq!(c.letter_of("a", "x").unwrap(), Some(x));
        assert_eq!((c.folder_count(a_in).unwrap(), c.folder_count(archive).unwrap()), (1, 0));
        assert_eq!(c.keyword_count_in("pm.cat.a", &Scope::All(Owner::Local)).unwrap(), 0);

        // Снова в архив, источник удалил: связей нет. В другую учётку — задание, обратно в архив — сразу `Local`.
        c.move_letters(&[x], archive).unwrap();
        c.delete_remote("a", &["x".into()]).unwrap();
        let job2 = c.move_letters(&[x], b_in).unwrap().job.unwrap();
        assert_eq!(c.outbound(job2).unwrap().len(), 1);
        // Ждёт выгрузки и переложено внутри цели — задание в новую папку.
        let b_arch = account_folder(&c, "b", "arch");
        c.move_letters(&[x], b_arch).unwrap();
        assert_eq!(c.outbound(job2).unwrap(), [(x, Outbound { to: b_arch, step: Step::Content })]);
        assert_eq!(c.move_letters(&[x], archive).unwrap().job, None);
        assert_eq!(c.letter(x).unwrap().unwrap().place, Place::Local);
        assert!(c.outbound(job2).unwrap().is_empty());
        assert_index(&c);
    }

    /// Пачки: больше пачки — несколько транзакций с общим номером задания.
    #[test]
    fn batches_share_the_job() {
        let c = Cache::open_in_memory().unwrap();
        let (a_in, b_in) = (account_folder(&c, "a", "in"), account_folder(&c, "b", "in"));
        let rows: Vec<(String, LetterRow)> = (0..5).map(|i| (format!("m{i}"), row(i, &[a_in], &["k"]))).collect();
        let ids = c.upsert_remotes("a", &rows).unwrap();
        let t = c.move_in_batches(&ids, b_in, 2).unwrap();
        assert_eq!((t.moved, t.outbound), (5, 5));
        assert_eq!(c.outbound(t.job.unwrap()).unwrap().len(), 5);
        assert_eq!(c.keyword_count_in("k", &Scope::Folder(b_in)).unwrap(), 5);
        assert_index(&c);
    }

    /// Удаление с сервера: действующая связь — письмо из базы целиком; запись с сервера — место своё.
    #[test]
    fn remote_delete_and_upsert() {
        let c = Cache::open_in_memory().unwrap();
        let a_in = account_folder(&c, "a", "in");
        let x = c.upsert_remote("a", "x", &row(10, &[a_in], &["k"])).unwrap();
        let again = c.upsert_remote("a", "x", &row(11, &[a_in], &[])).unwrap();
        assert_eq!(again, x);
        let r = c.letter(x).unwrap().unwrap();
        assert!(r.place == Place::Synced && r.received_at == 11 && r.keywords.is_empty());
        assert_eq!(c.delete_remote("a", &["x".into(), "zz".into()]).unwrap(), [x]);
        assert_eq!(c.letter(x).unwrap(), None);
        assert_eq!(c.folder_count(a_in).unwrap(), 0);
        assert!(c.upsert_remote("a", "y", &row(1, &[FolderId(999)], &[])).is_err(), "папки нет");
        assert_index(&c);
    }

    /// Папки «На этом компьютере»: создать, переименовать, перенести (не в себя), удалить только пустую. Папки учётки
    /// правит сервер.
    #[test]
    fn local_folders() {
        let c = Cache::open_in_memory().unwrap();
        let a_in = account_folder(&c, "a", "in");
        let top = c.create_folder(None, "Архив").unwrap();
        let sub = c.create_folder(Some(top), "2025").unwrap();
        c.rename_folder(sub, "2024").unwrap();
        assert_eq!(c.folder(sub).unwrap().unwrap().name, "2024");
        assert!(c.move_folder(top, Some(sub)).is_err(), "в свою вложенную");
        assert!(c.move_folder(top, Some(top)).is_err());
        assert!(c.move_folder(sub, Some(a_in)).is_err(), "в папку учётки");
        assert!(c.rename_folder(a_in, "x").is_err());
        assert!(c.create_folder(Some(a_in), "x").is_err());
        c.move_folder(sub, None).unwrap();
        assert_eq!(c.folder(sub).unwrap().unwrap().parent, None);
        c.move_folder(sub, Some(top)).unwrap();

        let x = c.upsert_remote("a", "x", &row(1, &[a_in], &[])).unwrap();
        c.move_letters(&[x], sub).unwrap();
        assert!(!c.delete_folder(top).unwrap(), "есть вложенная");
        assert!(!c.delete_folder(sub).unwrap(), "есть письмо");
        c.delete_letters(&[x]).unwrap();
        assert!(c.delete_folder(sub).unwrap());
        assert!(c.delete_folder(top).unwrap());
        assert!(!c.delete_folder(top).unwrap());
        assert_eq!(c.folders().unwrap().into_iter().map(|(id, _)| id).collect::<Vec<_>>(), [a_in]);
        assert_eq!(c.folder_of("a", "in").unwrap(), Some(a_in));
    }

    /// Случайные записи, переносы между учётками и «На этом компьютере», удаления: индексы и счётчики сходятся с
    /// пересчётом с нуля, «все папки» владельца — с моделью.
    #[test]
    fn random_moves_keep_the_index() {
        let c = Cache::open_in_memory().unwrap();
        let folders =
            [account_folder(&c, "a", "in"), account_folder(&c, "a", "arch"), account_folder(&c, "b", "in"), c.create_folder(None, "local").unwrap()];
        let owner = |f: FolderId| c.folder(f).unwrap().unwrap().owner;
        let mut rng = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move |n: u64| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng % n
        };
        for step in 0..400 {
            match next(4) {
                0 => {
                    let f = folders[next(3) as usize];
                    let Owner::Account(a) = owner(f) else { continue };
                    let kws: Vec<&str> = ["k1", "k2"].into_iter().filter(|_| next(2) == 0).collect();
                    let mut r = row(next(20) as i64, &[f], &kws);
                    r.flagged = next(3) == 0;
                    c.upsert_remote(&a, &format!("m{}", next(30)), &r).unwrap();
                }
                1 => {
                    let ids: Vec<LetterId> = (0..1 + next(4)).map(|_| LetterId(1 + next(40))).collect();
                    c.move_letters(&ids, folders[next(4) as usize]).unwrap();
                }
                2 => {
                    c.delete_remote(["a", "b"][next(2) as usize], &[format!("m{}", next(30))]).unwrap();
                }
                _ => c.delete_letters(&[LetterId(1 + next(40))]).unwrap(),
            }
            assert_eq!(c.check_index().unwrap(), Vec::<String>::new(), "шаг {step}");
        }
        let tx = c.db.begin_read().unwrap();
        let letters: Vec<LetterRow> = tx.open_table(LETTER).unwrap().iter().unwrap().map(|r| dec(r.unwrap().1.value()).unwrap()).collect();
        for o in [Owner::Account("a".into()), Owner::Account("b".into()), Owner::Local] {
            let want = letters.iter().filter(|r| r.keywords.iter().any(|k| k == "k1") && r.folders.iter().any(|f| owner(*f) == o)).count();
            assert_eq!(c.keyword_count_in("k1", &Scope::All(o.clone())).unwrap(), want, "{o:?}");
        }
    }
}
