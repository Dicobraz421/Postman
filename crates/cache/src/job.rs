//! Задания переноса (ADR 0006 п. 4): строка задания — режим, состояние, счётчики; шаги писем — строки `outbound`.
//!
//! Ведёт их движок ядра (`postman_core::migrate`): шаг письма пишется до команды пути и после ответа, каждый — одной
//! транзакцией, повтор той же записи после перезапуска ничего не портит. Письмо, которого в задании уже нет
//! (перенесено снова или удалено, пока ждали ответа сервера), шаги задания не трогают.

use redb::{ReadableDatabase, ReadableTable, TableDefinition, WriteTransaction};
use serde::{Deserialize, Serialize};

use crate::folder::{self, Owner};
use crate::letter::{JobId, LetterId, LetterRow, Letters, Outbound, Place, Refused, Step};
use crate::remote::RemoteRef;
use crate::{Cache, Error, Result, dec, enc};

/// Режим задания — ответ строки над списком папки (ADR 0006 п. 5); тот же, что у ядра.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MigrationMode {
    /// Ещё не ответили: шаг «содержимое» уже идёт.
    Ask,
    /// «Перенести на сервер».
    Upload,
    /// «Оставить локальным архивом».
    LocalArchive,
}

/// Где задание; то же, что у ядра.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MigrationState {
    Asking,
    Running,
    Paused,
    /// На сервере цели нет места.
    QuotaFull,
    Done,
    Cancelled,
}

/// Строка задания переноса: режим, состояние, всего писем, байт выгружено.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRow {
    pub id: JobId,
    pub mode: MigrationMode,
    pub state: MigrationState,
    pub total: u64,
    pub bytes: u64,
}

/// Задание → [`JobRow`].
pub(crate) const JOB: TableDefinition<u64, &[u8]> = TableDefinition::new("job");

/// Задание `job` переносом в папку владельца `target` получило ещё `added` писем; нового — завести.
pub(crate) fn add(tx: &WriteTransaction, job: JobId, target: &Owner, added: u64) -> Result<()> {
    let mut t = tx.open_table(JOB)?;
    let old: Option<JobRow> = t.get(job.0)?.map(|v| dec(v.value())).transpose()?;
    let row = match old {
        Some(row) => JobRow { total: row.total + added, ..row },
        None => {
            let (mode, state) = match target {
                Owner::Local => (MigrationMode::LocalArchive, MigrationState::Running),
                Owner::Account(_) => (MigrationMode::Ask, MigrationState::Asking),
            };
            JobRow { id: job, mode, state, total: added, bytes: 0 }
        }
    };
    t.insert(job.0, enc(&row)?.as_slice())?;
    Ok(())
}

impl Letters<'_> {
    /// Строка письма в задании; `None` — письма в задании нет.
    pub(crate) fn step(&self, job: JobId, id: LetterId) -> Result<Option<Outbound>> {
        self.outbound.get((job.0, id.0))?.map(|v| dec(v.value())).transpose()
    }

    /// Убрать строку письма из задания; ответ — какой она была.
    pub(crate) fn take_step(&mut self, job: JobId, id: LetterId) -> Result<Option<Outbound>> {
        self.outbound.remove((job.0, id.0))?.map(|v| dec(v.value())).transpose()
    }

    fn set_step(&mut self, job: JobId, id: LetterId, ob: &Outbound) -> Result<()> {
        self.outbound.insert((job.0, id.0), enc(ob)?.as_slice())?;
        Ok(())
    }

    /// Письмо задания `job`: строка шага и строка письма; `None` — письма в задании нет.
    fn in_job(&self, job: JobId, id: LetterId) -> Result<Option<(Outbound, LetterRow)>> {
        let Some(ob) = self.step(job, id)? else { return Ok(None) };
        Ok(self.get(id)?.filter(|r| r.place == Place::Outbound(job)).map(|r| (ob, r)))
    }

    /// Связь `link` — письму `letter`. id сервера у другого письма, которое синхронизация цели завела раньше
    /// подтверждения (только эта связь, `Synced`), — то письмо удаляется: это та же выгрузка. Иначе — ошибка.
    fn link(&mut self, letter: LetterId, link: RemoteRef) -> Result<()> {
        if let Some((other, _)) = self.find(&link.account, &link.id)?
            && other != letter
        {
            let dup = self.refs(other)?.len() == 1 && self.get(other)?.is_some_and(|r| r.place == Place::Synced);
            if !dup {
                return Err(Error(format!("remote id of letter {} is taken by letter {}", letter.0, other.0)));
            }
            self.remove(other)?;
        }
        self.remote.insert((link.account.as_str(), link.id.as_str()), letter.0)?;
        let mut refs = self.refs(letter)?;
        refs.retain(|r| !(r.account == link.account && r.id == link.id));
        refs.push(RemoteRef { detached: false, ..link });
        self.set_refs(letter, &refs)
    }
}

impl Cache {
    /// Все задания переноса.
    pub fn jobs(&self) -> Result<Vec<JobRow>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(JOB)?;
        t.iter()?.map(|r| dec(r?.1.value())).collect()
    }

    pub fn job(&self, id: JobId) -> Result<Option<JobRow>> {
        let tx = self.db.begin_read()?;
        tx.open_table(JOB)?.get(id.0)?.map(|v| dec(v.value())).transpose()
    }

    /// Записать строку задания: режим, состояние, счётчики.
    pub fn save_job(&self, job: &JobRow) -> Result<()> {
        let tx = self.db.begin_write()?;
        tx.open_table(JOB)?.insert(job.id.0, enc(job)?.as_slice())?;
        tx.commit()?;
        Ok(())
    }

    /// Задание кончилось: строки задания нет. Письма к этому времени из него уже вышли.
    pub fn drop_job(&self, job: JobId) -> Result<()> {
        let tx = self.db.begin_write()?;
        tx.open_table(JOB)?.remove(job.0)?;
        tx.commit()?;
        Ok(())
    }

    /// Шаг письма в задании — до команды пути и после ответа.
    pub fn set_step(&self, job: JobId, letter: LetterId, step: Step) -> Result<()> {
        self.letter_step(job, letter, |l, ob, _| l.set_step(job, letter, &Outbound { step, ..ob }))
    }

    /// Сервер цели принял письмо: связь с ним (`link`, действующая), шаг — [`Step::DeleteSource`]. Место —
    /// `Outbound` до [`Cache::finish`]: перенос обратно до удаления у источника ещё снимает задание.
    pub fn confirm(&self, job: JobId, letter: LetterId, link: RemoteRef) -> Result<()> {
        self.letter_step(job, letter, |l, ob, _| {
            l.link(letter, link)?;
            l.set_step(job, letter, &Outbound { step: Step::DeleteSource, ..ob })
        })
    }

    /// Локальный архив: письмо целиком в базе — шаг [`Step::DeleteSource`], по [`Cache::finish`] — `Local`. Без
    /// содержимого — ошибка: удалять у источника нельзя.
    pub fn archive(&self, job: JobId, letter: LetterId) -> Result<()> {
        self.letter_step(job, letter, |l, ob, row| {
            if row.content.is_none() {
                return Err(Error(format!("letter {} has no content to archive", letter.0)));
            }
            l.set_step(job, letter, &Outbound { step: Step::DeleteSource, ..ob })
        })
    }

    /// Сервер цели письмо не берёт: оно остаётся в папке цели только в базе с пометкой `why`
    /// ([`Place::Refused`]), строка задания убрана. У источника не удаляется: его связи остаются `detached`.
    pub fn refused(&self, job: JobId, letter: LetterId, why: Refused) -> Result<()> {
        self.letter_step(job, letter, |l, _, row| {
            l.take_step(job, letter)?;
            l.put(letter, Some(&row), &LetterRow { place: Place::Refused(why), ..row.clone() })
        })
    }

    /// Удаление у источника ушло: строка задания и `detached`-связи убраны; письмо — `Synced`, если цель его
    /// подтвердила, иначе (локальный архив) — `Local`.
    pub fn finish(&self, job: JobId, letter: LetterId) -> Result<()> {
        self.letter_step(job, letter, |l, _, row| {
            l.take_step(job, letter)?;
            let (gone, refs): (Vec<RemoteRef>, Vec<RemoteRef>) = l.refs(letter)?.into_iter().partition(|r| r.detached);
            for r in &gone {
                l.remote.remove((r.account.as_str(), r.id.as_str()))?;
            }
            l.set_refs(letter, &refs)?;
            let place = if refs.is_empty() { Place::Local } else { Place::Synced };
            l.put(letter, Some(&row), &LetterRow { place, ..row.clone() })
        })
    }

    /// Источник письмо так и не удалил (повторы кончились): строка задания убрана, письмо — `Synced`, если цель его
    /// подтвердила, иначе — `Local`; `detached`-связи остаются — пометка «не удалено у источника» (синхронизация
    /// источника снимет связь, когда письма там не станет).
    pub fn kept(&self, job: JobId, letter: LetterId) -> Result<()> {
        self.letter_step(job, letter, |l, _, row| {
            l.take_step(job, letter)?;
            let place = if l.refs(letter)?.iter().any(|r| !r.detached) { Place::Synced } else { Place::Local };
            l.put(letter, Some(&row), &LetterRow { place, ..row.clone() })
        })
    }

    /// Отмена: письма — одной транзакцией обратно в папки до переноса, связи учётки источника снова действуют
    /// (`Synced`; без связей — `Local`), строки задания убраны. Папок источника больше нет — письмо остаётся где
    /// есть, `Local`.
    pub fn restore(&self, job: JobId, letters: &[LetterId]) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut l = Letters::open(&tx)?;
            for &id in letters {
                let Some((ob, old)) = l.in_job(job, id)? else { continue };
                l.take_step(job, id)?;
                let mut from = Vec::with_capacity(ob.from.len());
                for f in ob.from {
                    if folder::get(&tx, f)?.is_some() {
                        from.push(f);
                    }
                }
                let mut refs = l.refs(id)?;
                let mut new = old.clone();
                if let Some(&first) = from.first() {
                    let owner = l.owner(first)?;
                    for r in &mut refs {
                        r.detached &= owner != Owner::Account(r.account.clone());
                    }
                    l.set_refs(id, &refs)?;
                    new.folders = from;
                }
                new.place = if refs.iter().any(|r| !r.detached) { Place::Synced } else { Place::Local };
                l.put(id, Some(&old), &new)?;
            }
            l.finish()?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Запись шага `f` над письмом задания — одной транзакцией; письма в задании нет — ничего.
    fn letter_step(&self, job: JobId, letter: LetterId, f: impl FnOnce(&mut Letters, Outbound, LetterRow) -> Result<()>) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut l = Letters::open(&tx)?;
            if let Some((ob, row)) = l.in_job(job, letter)? {
                f(&mut l, ob, row)?;
            }
            l.finish()?;
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FolderId, FolderRow, Scope};

    fn account_folder(c: &Cache, account: &str, remote: &str) -> FolderId {
        let row = FolderRow { owner: Owner::Account(account.into()), parent: None, name: remote.into(), role: None, remote: Some(remote.into()) };
        c.upsert_folder(&row).unwrap()
    }

    fn row(at: i64, folders: &[FolderId], keywords: &[&str]) -> LetterRow {
        LetterRow {
            thread_id: format!("t{at}"),
            received_at: at,
            keywords: keywords.iter().map(|k| k.to_string()).collect(),
            folders: folders.to_vec(),
            ..LetterRow::default()
        }
    }

    fn raw(n: u32) -> Vec<u8> {
        format!("Message-ID: <m{n}@x.test>\r\nSubject: {n}\r\n\r\nbody {n}\r\n").into_bytes()
    }

    fn link(account: &str, id: &str) -> RemoteRef {
        RemoteRef { account: account.into(), id: id.into(), data: vec![7], detached: false }
    }

    fn steps(c: &Cache, job: JobId) -> Vec<(LetterId, Step)> {
        c.outbound(job).unwrap().into_iter().map(|(id, o)| (id, o.step)).collect()
    }

    fn place(c: &Cache, id: LetterId) -> Place {
        c.letter(id).unwrap().unwrap().place
    }

    fn assert_consistent(c: &Cache) {
        assert_eq!(c.check_index().unwrap(), Vec::<String>::new());
        assert_eq!(c.check_refs().unwrap(), Vec::<String>::new());
    }

    /// Перезапуск посреди задания: строка задания и шаги писем — с того же места; повтор записи шага ничего не
    /// портит; связь, которую синхронизация цели завела раньше подтверждения, — то же письмо, не дубль.
    #[test]
    fn restart_midway() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.redb");
        let (x, y, job, b_in) = {
            let c = Cache::open(&path).unwrap();
            let (a_in, b_in) = (account_folder(&c, "a", "in"), account_folder(&c, "b", "in"));
            let x = c.upsert_remote("a", "x", &row(10, &[a_in], &["k"])).unwrap();
            let y = c.upsert_remote("a", "y", &row(20, &[a_in], &[])).unwrap();
            let job = c.move_letters(&[x, y], b_in).unwrap().job.unwrap();
            c.save_job(&JobRow { id: job, mode: MigrationMode::Upload, state: MigrationState::Running, total: 2, bytes: 0 }).unwrap();
            c.put_content(x, &raw(1), &[]).unwrap();
            c.set_step(job, x, Step::Upload).unwrap();
            // До команды выгрузки — и выход.
            c.set_step(job, x, Step::Confirm).unwrap();
            (x, y, job, b_in)
        };

        let c = Cache::open(&path).unwrap();
        assert_eq!(c.jobs().unwrap(), [JobRow { id: job, mode: MigrationMode::Upload, state: MigrationState::Running, total: 2, bytes: 0 }]);
        assert_eq!(steps(&c, job), [(x, Step::Confirm), (y, Step::Content)]);
        assert_eq!(c.letter_content(x).unwrap(), Some(raw(1)));
        assert_eq!(c.letter_content(y).unwrap(), None);
        assert_eq!(c.letter_content(LetterId(999)).unwrap(), None);

        // Повтор выгрузки подтвердил сервер — дважды (ответ пришёл, а выход случился до записи хода).
        c.confirm(job, x, link("b", "bx")).unwrap();
        c.confirm(job, x, link("b", "bx")).unwrap();
        assert_eq!(steps(&c, job), [(x, Step::DeleteSource), (y, Step::Content)]);
        assert_eq!(c.remotes(x).unwrap().iter().map(|r| (r.account.as_str(), r.detached)).collect::<Vec<_>>(), [("a", true), ("b", false)]);
        assert_eq!(place(&c, x), Place::Outbound(job), "до удаления у источника — ещё в задании");
        drop(c);

        let c = Cache::open(&path).unwrap();
        assert_eq!(steps(&c, job), [(x, Step::DeleteSource), (y, Step::Content)]);
        c.finish(job, x).unwrap();
        c.finish(job, x).unwrap();
        assert_eq!(c.remotes(x).unwrap(), [link("b", "bx")]);
        assert_eq!((place(&c, x), c.letter_of("b", "bx").unwrap()), (Place::Synced, Some(x)));
        // Источник сообщил об удалении — письма это не касается: связи с ним уже нет.
        assert_eq!(c.delete_remote("a", &["x".into()]).unwrap(), []);
        assert_eq!(c.letter(x).unwrap().unwrap().folders, [b_in]);

        // Синхронизация цели успела завести выгруженное письмо до подтверждения: это то же письмо.
        c.put_content(y, &raw(2), &[]).unwrap();
        c.set_step(job, y, Step::Confirm).unwrap();
        let dup = c.upsert_remote("b", "by", &row(20, &[b_in], &[])).unwrap();
        assert_eq!(c.folder_count(b_in).unwrap(), 3);
        c.confirm(job, y, link("b", "by")).unwrap();
        assert_eq!(c.letter(dup).unwrap(), None);
        assert_eq!(c.letter_of("b", "by").unwrap(), Some(y));
        assert_eq!(c.folder_count(b_in).unwrap(), 2);
        c.finish(job, y).unwrap();
        assert!(c.outbound(job).unwrap().is_empty());
        // Шаги письма вне задания — ничего.
        c.set_step(job, y, Step::Upload).unwrap();
        assert!(c.outbound(job).unwrap().is_empty());
        c.drop_job(job).unwrap();
        assert!(c.jobs().unwrap().is_empty());
        assert_consistent(&c);
    }

    /// Отмена с возвратом: одной транзакцией обратно во все прежние папки, связи источника снова действуют; из
    /// «На этом компьютере» — обратно `Local`; перенесённое дважды — в папки до первого переноса.
    #[test]
    fn cancel_returns_letters() {
        let c = Cache::open_in_memory().unwrap();
        let (a_in, a_arch, b_in, c_in) =
            (account_folder(&c, "a", "in"), account_folder(&c, "a", "arch"), account_folder(&c, "b", "in"), account_folder(&c, "c", "in"));
        let local = c.create_folder(None, "Архив").unwrap();
        let x = c.upsert_remote("a", "x", &row(10, &[a_in, a_arch], &["k"])).unwrap();
        let y = c.upsert_remote("a", "y", &row(20, &[a_in], &[])).unwrap();

        let job = c.move_letters(&[x, y], b_in).unwrap().job.unwrap();
        c.set_step(job, x, Step::Upload).unwrap();
        c.restore(job, &[x, y]).unwrap();
        assert!(c.outbound(job).unwrap().is_empty());
        assert_eq!(c.letter(x).unwrap().unwrap().folders, [a_in, a_arch]);
        assert_eq!(c.letter(y).unwrap().unwrap().folders, [a_in]);
        assert!([x, y].iter().all(|id| place(&c, *id) == Place::Synced && c.remotes(*id).unwrap().iter().all(|r| !r.detached)));
        assert_eq!((c.letter_of("a", "x").unwrap(), c.letter_of("a", "y").unwrap()), (Some(x), Some(y)));
        assert_eq!((c.folder_count(a_in).unwrap(), c.folder_count(a_arch).unwrap(), c.folder_count(b_in).unwrap()), (2, 1, 0));
        let all = |a: &str| c.keyword_count_in("k", &Scope::All(Owner::Account(a.into()))).unwrap();
        assert_eq!((all("a"), all("b")), (1, 0));
        // Повтор отмены — ничего: писем в задании уже нет.
        c.restore(job, &[x, y]).unwrap();
        assert_eq!(c.letter(y).unwrap().unwrap().folders, [a_in]);

        // Перенесённое снова, пока ждало выгрузки, — задание новое, «откуда» — прежнее.
        let job2 = c.move_letters(&[x], b_in).unwrap().job.unwrap();
        let job3 = c.move_letters(&[x], c_in).unwrap().job.unwrap();
        assert!(c.outbound(job2).unwrap().is_empty());
        assert_eq!(c.outbound(job3).unwrap()[0].1.from, [a_in, a_arch]);
        c.restore(job3, &[x]).unwrap();
        assert_eq!((c.letter(x).unwrap().unwrap().folders, place(&c, x)), (vec![a_in, a_arch], Place::Synced));

        // Из «На этом компьютере» в учётку и обратно отменой — снова `Local`.
        let ja = c.move_letters(&[y], local).unwrap().job.unwrap();
        c.put_content(y, &raw(2), &[]).unwrap();
        c.archive(ja, y).unwrap();
        c.finish(ja, y).unwrap();
        assert_eq!((place(&c, y), c.remotes(y).unwrap()), (Place::Local, vec![]));
        let jb = c.move_letters(&[y], b_in).unwrap().job.unwrap();
        assert_eq!(place(&c, y), Place::Outbound(jb));
        c.restore(jb, &[y]).unwrap();
        assert_eq!((c.letter(y).unwrap().unwrap().folders, place(&c, y)), (vec![local], Place::Local));
        assert_consistent(&c);
    }

    /// «Слишком большое для цели»: письмо остаётся в папке цели только в базе с пометкой; у источника не удаляется,
    /// синхронизация источника его не возвращает, но удаление видит. Архив без содержимого — ошибка.
    #[test]
    fn too_large_stays_local() {
        let c = Cache::open_in_memory().unwrap();
        let (a_in, b_in) = (account_folder(&c, "a", "in"), account_folder(&c, "b", "in"));
        let x = c.upsert_remote("a", "x", &row(10, &[a_in], &[])).unwrap();
        let y = c.upsert_remote("a", "y", &row(20, &[a_in], &[])).unwrap();
        let job = c.move_letters(&[x, y], b_in).unwrap().job.unwrap();
        c.put_content(x, &raw(1), &[]).unwrap();
        c.set_step(job, x, Step::Confirm).unwrap();

        c.refused(job, x, Refused::TooLarge).unwrap();
        assert_eq!(place(&c, x), Place::Refused(Refused::TooLarge));
        assert_eq!(c.letter(x).unwrap().unwrap().folders, [b_in]);
        assert_eq!(steps(&c, job), [(y, Step::Content)]);
        assert!(c.remotes(x).unwrap()[0].detached, "у источника не удаляется");
        assert_eq!(c.letter_of("a", "x").unwrap(), None);
        assert_eq!(c.letter_content(x).unwrap(), Some(raw(1)));
        // Источник прислал письмо снова — оно не возвращается.
        assert_eq!(c.upsert_remote("a", "x", &row(10, &[a_in], &[])).unwrap(), x);
        assert_eq!((c.folder_count(a_in).unwrap(), c.folder_count(b_in).unwrap()), (0, 2));
        // Источник удалил его сам — связь снята, письмо на месте.
        assert_eq!(c.delete_remote("a", &["x".into()]).unwrap(), []);
        assert_eq!((c.remotes(x).unwrap(), place(&c, x)), (vec![], Place::Refused(Refused::TooLarge)));

        // Локальный архив — только с содержимым.
        assert!(c.archive(job, y).is_err());
        assert_eq!(steps(&c, job), [(y, Step::Content)]);
        c.put_content(y, &raw(2), &[]).unwrap();
        c.archive(job, y).unwrap();
        c.finish(job, y).unwrap();
        assert_eq!((place(&c, y), c.remotes(y).unwrap(), c.letter_of("a", "y").unwrap()), (Place::Local, vec![], None));
        assert!(c.outbound(job).unwrap().is_empty());
        assert_consistent(&c);
    }

    /// Источник так и не удалил: письмо вышло из задания (у цели — `Synced`, архив — `Local`), `detached`-связь
    /// источника — пометка; синхронизация источника письмо не возвращает, его удаление там связь снимает.
    #[test]
    fn source_kept_marks_the_letter() {
        let c = Cache::open_in_memory().unwrap();
        let (a_in, b_in) = (account_folder(&c, "a", "in"), account_folder(&c, "b", "in"));
        let local = c.create_folder(None, "Архив").unwrap();
        let x = c.upsert_remote("a", "x", &row(10, &[a_in], &[])).unwrap();
        let y = c.upsert_remote("a", "y", &row(20, &[a_in], &[])).unwrap();
        let job = c.move_letters(&[x], b_in).unwrap().job.unwrap();
        c.put_content(x, &raw(1), &[]).unwrap();
        c.confirm(job, x, link("b", "bx")).unwrap();
        c.kept(job, x).unwrap();
        assert!(c.outbound(job).unwrap().is_empty());
        assert_eq!(place(&c, x), Place::Synced);
        assert_eq!(c.remotes(x).unwrap().iter().map(|r| (r.account.as_str(), r.detached)).collect::<Vec<_>>(), [("a", true), ("b", false)]);
        assert_eq!(c.upsert_remote("a", "x", &row(10, &[a_in], &[])).unwrap(), x);
        assert_eq!((c.folder_count(a_in).unwrap(), c.folder_count(b_in).unwrap()), (1, 1));
        assert_eq!(c.delete_remote("a", &["x".into()]).unwrap(), []);
        assert_eq!(c.remotes(x).unwrap(), [link("b", "bx")]);

        let ja = c.move_letters(&[y], local).unwrap().job.unwrap();
        c.put_content(y, &raw(2), &[]).unwrap();
        c.archive(ja, y).unwrap();
        c.kept(ja, y).unwrap();
        assert_eq!((place(&c, y), c.remotes(y).unwrap().iter().all(|r| r.detached)), (Place::Local, true));
        assert_consistent(&c);
    }
}
