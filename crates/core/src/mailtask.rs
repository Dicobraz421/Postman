//! Письмо ↔ задача (S6-03, решение 4 S6): письмо со «Сроком» (`pm.due` + `pm.due.<дата>[t<время>]`, S5-04) — задача
//! списка «Из почты». Это представление над индексом ключевых слов кэша, без копии; запись в хранилище задач
//! появляется, только когда у такой задачи правят что-то кроме срока (заметки, подзадачи, проект) — со ссылкой на
//! письмо (`Message-ID` и id письма).
//!
//! Срок живёт в письме: выполнение задачи снимает оба слова срока одним `Email/set` (`sync::Command::Patch`), смена
//! срока в задаче меняет его у письма и наоборот; снятие срока в почте закрывает задачу; письмо удалено — задача
//! остаётся без ссылки. Без I/O, кроме чтения кэша ([`letters`], [`reconcile_cache`]).

use jiff::civil::{Date, Time};
use jiff::tz::TimeZone;
use postman_cache::{Cache, EmailRow, all_folders};
use postman_calendar::{MailLink, Moment, Task};

use crate::lifecycle::{self, Patch};
use crate::marks::{self, Kind, When};

/// Префикс UID задачи-письма (не записанной в хранилище): `mail:<ключ письма в кэше>`.
pub const LETTER_UID: &str = "mail:";
/// Писем со сроком за раз — не больше (как у планировщика сроков).
pub const LIMIT: usize = lifecycle::LIMIT;

/// Срок письма (самый ранний разобранный).
pub fn letter_due(row: &EmailRow) -> Option<When> {
    marks::find(&row.keywords, Kind::Due)
}

/// Письма со сроком своего аккаунта, во всех папках — по индексу ключевых слов кэша.
pub fn letters(cache: &Cache) -> postman_cache::Result<Vec<EmailRow>> {
    let rows = cache.keyword_prefix(Kind::Due.base(), &all_folders(None), LIMIT)?;
    Ok(rows.into_iter().filter(|r| letter_due(r).is_some()).collect())
}

/// Задача-письмо (ещё не записана): UID с [`LETTER_UID`].
pub fn is_letter(t: &Task) -> bool {
    t.uid.starts_with(LETTER_UID)
}

/// Ключ письма задачи-письма или записанной задачи со ссылкой.
pub fn letter_key(t: &Task) -> Option<&str> {
    t.uid.strip_prefix(LETTER_UID).or_else(|| t.mail.as_ref().and_then(|m| m.email_id.as_deref()))
}

/// Срок письма → срок задачи: «плавающее» местное время (сроки писем — местные), без времени — весь день.
pub fn due_moment(when: When) -> (Moment, bool) {
    let time = when.time.unwrap_or(Time::midnight());
    (Moment::new(when.date.to_datetime(time), None), when.time.is_none())
}

/// Срок задачи → срок письма (в поясе `tz` компьютера).
pub fn due_when(t: &Task, tz: &TimeZone) -> Option<When> {
    let due = t.due.as_ref()?;
    let date: Date = due.date_in(tz)?;
    if t.all_day {
        return Some(When::day(date));
    }
    let local = match &due.tz {
        None => due.local,
        Some(_) => {
            let ts = jiff::Timestamp::from_second(due.timestamp(tz)?).ok()?;
            ts.to_zoned(tz.clone()).datetime()
        }
    };
    Some(When::at(local.date(), Time::new(local.hour(), local.minute(), 0, 0).ok()?))
}

/// Записанная задача, связанная с письмом `row` (по id письма или по Message-ID).
pub fn linked<'a>(tasks: &'a [Task], row: &EmailRow, message_id: Option<&str>) -> Option<&'a Task> {
    tasks.iter().find(|t| {
        t.mail.as_ref().is_some_and(|m| {
            m.email_id.as_deref() == Some(row.id.as_str()) || message_id.is_some_and(|id| !id.is_empty() && m.message_id.eq_ignore_ascii_case(id))
        })
    })
}

/// Задача письма для списка «Из почты»: записанная (если есть) со сроком письма, иначе — задача-письмо.
pub fn as_task(row: &EmailRow, stored: Option<&Task>) -> Option<Task> {
    let when = letter_due(row)?;
    let (due, all_day) = due_moment(when);
    let mut t = match stored {
        Some(s) => s.clone(),
        None => {
            let mut t = Task::new(&format!("{LETTER_UID}{}", row.id), "", &row.subject);
            t.mail = Some(MailLink { message_id: String::new(), email_id: Some(row.id.clone()) });
            t
        }
    };
    t.due = Some(due);
    t.all_day = all_day;
    Some(t)
}

/// «Из почты»: письма со сроком (с записанными задачами поверх), затем записанные открытые задачи со ссылкой на
/// письмо, которого среди писем со сроком нет (письмо не в кэше — ссылку уберёт [`reconcile`]). По сроку.
pub fn from_mail(tasks: &[Task], letters: &[EmailRow], tz: &TimeZone) -> Vec<Task> {
    let mut out: Vec<Task> = letters.iter().filter_map(|r| as_task(r, linked(tasks, r, None))).collect();
    for t in tasks.iter().filter(|t| t.mail.is_some() && !t.progress.is_closed() && t.parent.is_none()) {
        if !out.iter().any(|o| o.uid == t.uid) {
            out.push(t.clone());
        }
    }
    out.sort_by(|a, b| a.due_at(tz).unwrap_or(i64::MAX).cmp(&b.due_at(tz).unwrap_or(i64::MAX)).then_with(|| a.title.cmp(&b.title)));
    out
}

/// Правка у задачи-письма, которая требует записи (заметки, подзадачи, проект): задача в хранилище со ссылкой на
/// письмо. Срок — из письма; `message_id` — из тела письма в кэше, если оно загружено.
pub fn promote(row: &EmailRow, message_id: Option<&str>, uid: &str) -> Task {
    let mut t = Task::new(uid, "", &row.subject);
    t.mail = Some(MailLink { message_id: message_id.unwrap_or_default().to_owned(), email_id: Some(row.id.clone()) });
    if let Some(when) = letter_due(row) {
        let (due, all_day) = due_moment(when);
        t.due = Some(due);
        t.all_day = all_day;
    }
    t
}

/// Что сделать с письмом после правки задачи `task` (задачи-письма или записанной со ссылкой): выполнена — снять
/// срок (оба слова одним патчем); срок изменён или снят — то же у письма. Ничего не изменилось — `None`.
pub fn letter_patch(task: &Task, row: &EmailRow, tz: &TimeZone) -> Option<Patch> {
    let has = marks::has(&row.keywords, Kind::Due);
    let want = if task.progress.is_closed() { None } else { due_when(task, tz) };
    match (want, letter_due(row)) {
        (None, _) if has => Some(lifecycle::set(row, Kind::Due, None)),
        (None, _) => None,
        (Some(w), Some(cur)) if w == cur => None,
        (Some(w), _) => Some(lifecycle::set(row, Kind::Due, Some(w))),
    }
}

/// Записанные задачи со ссылкой на письмо — по письмам (`lookup`: письмо по id, `None` — письма нет): срока у письма
/// нет — задача выполнена (`now`); срок другой — у задачи срок письма; письма нет — задача без ссылки. Ответ —
/// изменённые задачи (записать, `tasks::save_local`).
pub fn reconcile(tasks: &[Task], lookup: impl Fn(&str) -> Option<EmailRow>, now: i64, tz: &TimeZone) -> Vec<Task> {
    let mut out = Vec::new();
    for t in tasks.iter().filter(|t| !t.progress.is_closed()) {
        let Some(id) = t.mail.as_ref().and_then(|m| m.email_id.clone()) else { continue };
        let mut fixed = t.clone();
        match lookup(&id) {
            None => {
                // Письмо удалено: задача остаётся, ссылка — нет (Message-ID — тоже: открыть нечего).
                fixed.mail = None;
            }
            Some(row) => match letter_due(&row) {
                None => fixed.set_done(true, now),
                Some(when) => {
                    if due_when(t, tz) != Some(when) {
                        let (due, all_day) = due_moment(when);
                        fixed.due = Some(due);
                        fixed.all_day = all_day;
                    }
                }
            },
        }
        if fixed != *t {
            out.push(fixed);
        }
    }
    out
}

/// [`reconcile`] по кэшу: задачи и письма — из него.
pub fn reconcile_cache(cache: &Cache, now: i64, tz: &TimeZone) -> postman_cache::Result<Vec<Task>> {
    let tasks: Vec<Task> = cache.tasks()?.iter().filter_map(crate::tasks::from_row).collect();
    if !tasks.iter().any(|t| t.mail.as_ref().is_some_and(|m| m.email_id.is_some())) {
        return Ok(Vec::new());
    }
    let lookup = |id: &str| cache.rows_by_ids(&[id.to_owned()]).ok().and_then(|mut v| v.pop());
    Ok(reconcile(&tasks, lookup, now, tz))
}

#[cfg(test)]
mod tests {
    use super::*;
    use postman_calendar::Progress;

    fn tz() -> TimeZone {
        TimeZone::get("Europe/Moscow").unwrap()
    }

    /// Строка письма; ключевые слова — по алфавиту, как в кэше (`EmailRow::set_keyword` на это рассчитывает).
    fn row(id: &str, kws: &[&str]) -> EmailRow {
        let mut keywords: Vec<String> = kws.iter().map(|k| k.to_string()).collect();
        keywords.sort();
        EmailRow {
            id: id.into(),
            thread_id: id.into(),
            mailbox_ids: vec!["in".into()],
            received_at: 1,
            subject: format!("Письмо {id}"),
            from_name: String::new(),
            from_addr: String::new(),
            preview: String::new(),
            seen: true,
            flagged: false,
            has_attachment: false,
            size: 1,
            keywords,
        }
    }

    fn day(y: i16, m: i8, d: i8) -> Date {
        jiff::civil::date(y, m, d)
    }

    fn apply(p: &Patch, r: &EmailRow) -> EmailRow {
        let mut r = r.clone();
        p.apply(&mut r);
        r
    }

    /// Срок письма → задача → срок письма: с временем и без.
    #[test]
    fn due_round_trip() {
        for when in [When::day(day(2026, 10, 29)), When::at(day(2026, 10, 29), Time::new(15, 0, 0, 0).unwrap())] {
            let r = row("m1", &["pm.due", &Kind::Due.keyword(when)]);
            let t = as_task(&r, None).unwrap();
            assert!(is_letter(&t) && letter_key(&t) == Some("m1"));
            assert_eq!(t.title, "Письмо m1");
            assert_eq!(t.all_day, when.time.is_none());
            assert_eq!(due_when(&t, &tz()), Some(when));
            assert_eq!(letter_patch(&t, &r, &tz()), None, "ничего не изменилось");
        }
        // Срок в другом поясе (задача из другого клиента) — в местное время письма.
        let mut t = Task::new("u", "", "x");
        t.due = Some(Moment::new("2026-10-29T12:00:00".parse().unwrap(), Some("UTC")));
        assert_eq!(due_when(&t, &tz()), Some(When::at(day(2026, 10, 29), Time::new(15, 0, 0, 0).unwrap())));
    }

    /// Выполнение задачи → оба слова срока сняты одним патчем; смена срока → новое слово, старое снято; вернуть
    /// срок — слова снова.
    #[test]
    fn task_edits_become_keyword_patches() {
        let r = row("m1", &["pm.due", "pm.due.2026-10-29", "pm.cat.clients"]);
        let mut t = as_task(&r, None).unwrap();
        t.set_done(true, 1);
        let p = letter_patch(&t, &r, &tz()).unwrap();
        let after = apply(&p, &r);
        assert_eq!(after.keywords, ["pm.cat.clients"], "оба слова срока сняты, категория осталась");
        assert_eq!(p.id, "m1");
        let mut moved = as_task(&r, None).unwrap();
        moved.due = Some(Moment::new("2026-10-30T09:30:00".parse().unwrap(), None));
        moved.all_day = false;
        let after = apply(&letter_patch(&moved, &r, &tz()).unwrap(), &r);
        assert_eq!(after.keywords, ["pm.cat.clients", "pm.due", "pm.due.2026-10-30t0930"]);
        let mut cleared = as_task(&r, None).unwrap();
        cleared.due = None;
        assert_eq!(apply(&letter_patch(&cleared, &r, &tz()).unwrap(), &r).keywords, ["pm.cat.clients"]);
        // У письма без срока задача без срока — ничего.
        assert_eq!(letter_patch(&cleared, &row("m2", &[]), &tz()), None);
    }

    /// Письмо → записанная задача (правка заметок): ссылка и срок; в «Из почты» — один раз, со сроком письма.
    #[test]
    fn promote_and_from_mail_without_duplicates() {
        let r = row("m1", &["pm.due", "pm.due.2026-10-29t1500"]);
        let other = row("m2", &["pm.due", "pm.due.2026-10-27"]);
        let mut t = promote(&r, Some("abc@itt.test"), "u1");
        t.notes = "Заметка".into();
        assert_eq!(t.mail, Some(MailLink { message_id: "abc@itt.test".into(), email_id: Some("m1".into()) }));
        assert_eq!(due_when(&t, &tz()), letter_due(&r));
        let mut orphan = Task::new("u2", "b", "Ссылка на письмо не в кэше");
        orphan.mail = Some(MailLink { message_id: String::new(), email_id: Some("m9".into()) });
        let list = from_mail(&[t.clone(), orphan], &[r.clone(), other], &tz());
        let uids: Vec<&str> = list.iter().map(|x| x.uid.as_str()).collect();
        assert_eq!(uids, ["mail:m2", "u1", "u2"]);
        assert_eq!(list[1].notes, "Заметка", "записанная поверх письма");
        assert_eq!(linked(&[t], &row("mX", &[]), Some("ABC@itt.test")).map(|x| x.uid.as_str()), Some("u1"));
    }

    /// Срок снят в почте → задача выполнена; срок другой → у задачи срок письма; письмо удалено → без ссылки.
    #[test]
    fn reconcile_with_letters() {
        let mut a = promote(&row("m1", &["pm.due", "pm.due.2026-10-29"]), None, "a");
        let b = promote(&row("m2", &["pm.due", "pm.due.2026-10-29"]), None, "b");
        let c = promote(&row("m3", &["pm.due", "pm.due.2026-10-29"]), None, "c");
        let d = promote(&row("m4", &["pm.due", "pm.due.2026-10-29"]), None, "d");
        a.notes = "у a".into();
        let letters = [row("m1", &["pm.cat.team"]), row("m2", &["pm.due", "pm.due.2026-11-02t1000"]), row("m4", &["pm.due", "pm.due.2026-10-29"])];
        let lookup = |id: &str| letters.iter().find(|r| r.id == id).cloned();
        let fixed = reconcile(&[a, b, c, d], lookup, 42, &tz());
        let by = |uid: &str| fixed.iter().find(|t| t.uid == uid);
        let a = by("a").unwrap();
        assert_eq!((a.progress, a.completed_at), (Progress::Completed, Some(42)));
        assert_eq!(due_when(by("b").unwrap(), &tz()), Some(When::at(day(2026, 11, 2), Time::new(10, 0, 0, 0).unwrap())));
        let c = by("c").unwrap();
        assert_eq!((c.mail.clone(), c.progress), (None, Progress::NeedsAction), "письмо удалено — задача без ссылки");
        assert!(by("d").is_none(), "без изменений — не записывать");
        // Выполненные не трогаются.
        let mut done = promote(&row("m1", &[]), None, "e");
        done.set_done(true, 1);
        assert!(reconcile(&[done], |_| None, 42, &tz()).is_empty());
    }

    /// По кэшу: письма со сроком из индекса; связанная задача в кэше закрывается, когда у письма срок снят.
    #[test]
    fn letters_and_reconcile_in_cache() {
        let c = Cache::open_in_memory().unwrap();
        c.upsert_emails(&[row("m1", &["pm.due", "pm.due.2026-10-29"]), row("m2", &["pm.wait", "pm.wait.2026-10-29"]), row("m3", &["pm.due"])])
            .unwrap();
        let ls: Vec<String> = letters(&c).unwrap().into_iter().map(|r| r.id).collect();
        assert_eq!(ls, ["m1"], "только письма с разобранным сроком");
        assert!(reconcile_cache(&c, 1, &tz()).unwrap().is_empty(), "задач нет — письма не читаются");
        crate::tasks::save_local(&c, promote(&row("m1", &[]), None, "u1")).unwrap();
        assert!(reconcile_cache(&c, 1, &tz()).unwrap().iter().all(|t| t.uid != "u1" || t.progress == Progress::NeedsAction));
        c.set_keywords("m1", &[], &["pm.due".into(), "pm.due.2026-10-29".into()]).unwrap();
        let fixed = reconcile_cache(&c, 7, &tz()).unwrap();
        assert_eq!(fixed.len(), 1);
        assert_eq!(fixed[0].progress, Progress::Completed);
        c.delete_emails(&["m1".into()]).unwrap();
        let fixed = reconcile_cache(&c, 7, &tz()).unwrap();
        assert_eq!(fixed[0].mail, None, "письмо удалено — не падает, ссылка снята");
    }
}
