//! Поручения (S6-09): действия сторон над задачей и письма iTIP, приём писем — над моделью
//! `postman_calendar::assign` (статусы, таблица переходов, лента шагов, VTODO). Без I/O: письма уходят командой
//! синхронизации `Command::Delegate`, своя копия задачи — правкой `tasks` (кэш, затем сервер без рассылки).
//!
//! Путь (S6-01, `docs/STALWART.md` п. 40, решение 2): поручивший — своя запись задачи без рассылки и своё письмо
//! iMIP REQUEST (текст для людей без Postman + `text/calendar`); сервер исполнителя кладёт VTODO в его календарь
//! сам. Ответы исполнителя — письмом REPLY (сервер поручившего применяет PARTSTAT, COMMENT теряет — Postman берёт
//! его из письма, [`receive`]); «предложить срок» — COUNTER, «оставить срок» — DECLINECOUNTER, «забрать» — CANCEL.
//! Наблюдатели (`ROLE=NON-PARTICIPANT`) получают первое поручение и только итоги (решение 9): готово, отклонено,
//! результат принят, забрано.

use postman_calendar::assign::{self, Action, Incoming, Invalid, Status, Step, StepKind};
use postman_calendar::{Moment, Progress, Role, Task, TaskParticipant, TaskPartstat};
use postman_compose::{Address, CalendarPart, Draft};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(Invalid),
    #[error("not a delegation")]
    NotDelegation,
    /// Действие другой стороны (исполнитель не принимает результат).
    #[error("this action belongs to the other side")]
    WrongSide,
    /// «Напомнить» — не чаще раза в сутки: можно с `next`.
    #[error("reminded recently")]
    TooSoon { next: i64 },
    #[error("no assignee")]
    NoAssignee,
    /// Действию нужен срок (изменить, предложить).
    #[error("no due date")]
    NoDue,
}

/// Письмо шага: метод iTIP, VCALENDAR и кому.
#[derive(Debug, Clone, PartialEq)]
pub struct Outgoing {
    pub method: String,
    pub ics: String,
    pub to: Vec<Address>,
    /// Что за шаг — интерфейсу для темы и текста письма.
    pub kind: StepKind,
}

/// Итог действия: своя копия задачи (записать), статус после него, письма (отправить).
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub task: Task,
    pub status: Status,
    pub outgoing: Vec<Outgoing>,
}

/// Что ввёл человек: комментарий, срок (изменить, предложить), новый исполнитель («Поручить другому»).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Input {
    pub comment: String,
    pub due: Option<Moment>,
    pub to: Option<Address>,
}

fn address(p: &TaskParticipant) -> Address {
    Address::new(&p.name, &p.email)
}

fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

fn observers(t: &Task) -> Vec<Address> {
    t.participants.iter().filter(|p| p.role == Role::Observer).map(address).collect()
}

fn due_text(m: &Moment) -> String {
    m.local.strftime("%Y-%m-%dT%H:%M:%S").to_string()
}

fn step(kind: StepKind, who: &str, comment: &str, due: Option<&Moment>, at: i64) -> Step {
    Step { at, who: who.to_owned(), kind, comment: comment.trim().to_owned(), due: due.map(due_text) }
}

/// Поручить: задача становится поручением `input.to` (наблюдатели — по желанию), срок и комментарий (`input`) — в
/// ней и в письме.
/// Письмо — исполнителю и наблюдателям (им — чтобы видеть ход, решение 9).
pub fn assign(task: &Task, me: &Address, watchers: &[Address], input: &Input, now: i64, tz: &jiff::tz::TimeZone) -> Result<Plan, Error> {
    let to = input.to.as_ref().filter(|a| !a.email.trim().is_empty()).ok_or(Error::NoAssignee)?;
    let (due, comment) = (input.due.clone(), input.comment.trim());
    let mut t = task.clone();
    let person = |key: &str, a: &Address, role| TaskParticipant {
        key: key.into(),
        name: a.name.clone(),
        email: a.email.clone(),
        role,
        status: TaskPartstat::NeedsAction,
    };
    t.participants = vec![person("owner", me, Role::Owner), person("assignee", to, Role::Assignee)];
    for (i, w) in watchers.iter().filter(|w| !same(&w.email, &to.email) && !same(&w.email, &me.email)).enumerate() {
        t.participants.push(person(&format!("observer{}", i + 1), w, Role::Observer));
    }
    if due.is_some() {
        t.due = due;
    }
    t.sequence = 0;
    t.progress = Progress::NeedsAction;
    let first = step(StepKind::Assigned, &me.email, comment, t.due.as_ref(), now);
    assign::push_step(&mut t, first);
    let mut recipients = vec![to.clone()];
    recipients.extend(observers(&t));
    let ics = assign::request(&t, comment, false, now, tz);
    Ok(Plan {
        task: t,
        status: Status::Waiting,
        outgoing: vec![Outgoing { method: "REQUEST".into(), ics, to: recipients, kind: StepKind::Assigned }],
    })
}

/// Действие стороны `me` над поручением: проверка по таблице переходов, своя копия после него и письма.
pub fn act(task: &Task, me: &str, action: Action, input: &Input, now: i64, tz: &jiff::tz::TimeZone) -> Result<Plan, Error> {
    let from = assign::status(task).ok_or(Error::NotDelegation)?;
    let owner = task.organizer().ok_or(Error::NotDelegation)?.clone();
    let worker = assign::assignee(task).ok_or(Error::NoAssignee)?.clone();
    let mine = if action.by_owner() { same(&owner.email, me) } else { same(&worker.email, me) };
    if !mine {
        return Err(Error::WrongSide);
    }
    let to = assign::next(from, action).map_err(Error::Invalid)?;
    let mut t = task.clone();
    let comment = input.comment.trim();
    let mut out = Vec::new();
    let mut send = |method: &str, ics: String, to: Vec<Address>, kind: StepKind| {
        out.push(Outgoing { method: method.into(), ics, to, kind });
    };
    let with_observers = |t: &Task, first: Address| {
        let mut v = vec![first];
        v.extend(observers(t));
        v
    };
    let me_pair = (worker.name.as_str(), worker.email.as_str());
    let kind = match action {
        Action::Remind => {
            if let Some(next) = assign::next_reminder(&t).filter(|n| now < *n) {
                return Err(Error::TooSoon { next });
            }
            StepKind::Reminded
        }
        Action::ChangeDue => {
            t.due = Some(input.due.clone().ok_or(Error::NoDue)?);
            t.sequence += 1;
            StepKind::DueChanged
        }
        Action::AgreeDue => {
            t.due = Some(assign::proposed(&t).ok_or(Error::NoDue)?);
            assign::set_proposed(&mut t, None);
            assign::set_partstat(&mut t, &worker.email, TaskPartstat::Accepted);
            t.sequence += 1;
            StepKind::Agreed
        }
        Action::KeepDue => {
            assign::set_proposed(&mut t, None);
            assign::set_partstat(&mut t, &worker.email, TaskPartstat::NeedsAction);
            StepKind::Kept
        }
        Action::AcceptResult => {
            t.set_done(true, now);
            assign::set_prop(&mut t, assign::X_RESULT, Some("accepted"));
            StepKind::ResultAccepted
        }
        Action::Return => {
            assign::set_partstat(&mut t, &worker.email, TaskPartstat::InProcess);
            t.set_done(false, now);
            t.progress = Progress::InProcess;
            t.sequence += 1;
            StepKind::Returned
        }
        Action::Revoke => {
            t.progress = Progress::Cancelled;
            t.sequence += 1;
            StepKind::Revoked
        }
        Action::Reassign => {
            let new = input.to.clone().filter(|a| !a.email.trim().is_empty()).ok_or(Error::NoAssignee)?;
            if let Some(p) = t.participants.iter_mut().find(|p| p.role == Role::Assignee) {
                p.name = new.name.clone();
                p.email = new.email.clone();
                p.status = TaskPartstat::NeedsAction;
            }
            assign::set_proposed(&mut t, None);
            t.sequence += 1;
            StepKind::Reassigned
        }
        Action::Accept => {
            assign::set_proposed(&mut t, None);
            assign::set_partstat(&mut t, me, TaskPartstat::Accepted);
            StepKind::Accepted
        }
        Action::Decline => {
            assign::set_partstat(&mut t, me, TaskPartstat::Declined);
            StepKind::Declined
        }
        Action::ProposeDue => {
            let due = input.due.clone().ok_or(Error::NoDue)?;
            assign::set_proposed(&mut t, Some(&due));
            StepKind::Proposed
        }
        Action::Start => {
            assign::set_partstat(&mut t, me, TaskPartstat::InProcess);
            t.progress = Progress::InProcess;
            StepKind::Started
        }
        Action::Finish => {
            // Задача закрыта только приёмкой: ход — «в работе», 100 % — до решения поручившего.
            assign::set_partstat(&mut t, me, TaskPartstat::Completed);
            t.progress = Progress::InProcess;
            t.percent = Some(100);
            StepKind::Finished
        }
        Action::Recall => {
            assign::set_partstat(&mut t, me, TaskPartstat::InProcess);
            t.progress = Progress::InProcess;
            t.percent = None;
            StepKind::Recalled
        }
        Action::Reopen => {
            assign::set_proposed(&mut t, None);
            assign::set_partstat(&mut t, me, TaskPartstat::NeedsAction);
            StepKind::Reopened
        }
    };
    let due = match action {
        Action::ChangeDue | Action::AgreeDue => t.due.clone(),
        Action::ProposeDue => input.due.clone(),
        _ => None,
    };
    assign::push_step(&mut t, step(kind, me, comment, due.as_ref(), now));
    let worker_now = assign::assignee(&t).map(address).unwrap_or_else(|| address(&worker));
    match action {
        Action::Remind => send("REQUEST", assign::request(&t, comment, true, now, tz), vec![worker_now], kind),
        Action::ChangeDue | Action::AgreeDue | Action::Return | Action::Reassign => {
            send("REQUEST", assign::request(&t, comment, false, now, tz), vec![worker_now], kind)
        }
        Action::KeepDue => send("DECLINECOUNTER", assign::decline_counter(&t, comment, now), vec![worker_now], kind),
        Action::AcceptResult => send("REQUEST", assign::request(&t, comment, false, now, tz), with_observers(&t, worker_now), kind),
        Action::Revoke => send("CANCEL", assign::cancel(&t, now, tz), with_observers(&t, worker_now), kind),
        Action::ProposeDue => {
            let due = input.due.clone().ok_or(Error::NoDue)?;
            send("COUNTER", assign::counter(&t, me_pair, &due, comment, now, tz), vec![address(&owner)], kind)
        }
        Action::Accept | Action::Decline | Action::Start | Action::Finish | Action::Recall | Action::Reopen => {
            let status = assign::assignee(&t).map_or(TaskPartstat::NeedsAction, |p| p.status);
            // Итоги (готово, отклонено) — ещё и наблюдателям (решение 9).
            let to = if matches!(action, Action::Finish | Action::Decline) { with_observers(&t, address(&owner)) } else { vec![address(&owner)] };
            send("REPLY", assign::reply(&t, me_pair, status, comment, now), to, kind)
        }
    }
    Ok(Plan { task: t, status: to, outgoing: out })
}

/// Что дал разбор письма iTIP для своей копии.
#[derive(Debug, Clone, PartialEq)]
pub enum Received {
    /// Письмо старше копии (`SEQUENCE` меньше) — пропущено (запись в журнал).
    Stale,
    /// Уже учтено (письмо открыто второй раз) или не о нас.
    Same,
    /// Копия изменилась: записать.
    Applied(Box<Task>),
}

/// Учесть письмо iTIP в своей копии задачи: ответ исполнителя, предложенный срок, напоминание, возврат, итог,
/// отмена — шагом ленты с комментарием (сервер COMMENT не хранит). Сопоставление — по UID и `SEQUENCE`, как
/// приглашения (`invite::classify`).
pub fn receive(own: &Task, inc: &Incoming, now: i64) -> Received {
    if inc.task.uid != own.uid {
        return Received::Same;
    }
    // Ответ исполнителя несёт SEQUENCE запроса, на который отвечает: старее своей — устарел.
    if inc.task.sequence < own.sequence {
        tracing::info!(uid = %own.uid, got = inc.task.sequence, have = own.sequence, "stale iTIP message skipped");
        return Received::Stale;
    }
    let Some(sender) = inc.sender() else { return Received::Same };
    let who = sender.email.clone();
    let at = if inc.at > 0 { inc.at } else { now };
    let mut t = own.clone();
    let before = assign::status(own);
    let kind = match inc.method.as_str() {
        "REPLY" => {
            let status = sender.status;
            if !assign::set_partstat(&mut t, &who, status) {
                return Received::Same;
            }
            if let Some(p) = inc.task.percent {
                t.percent = Some(p);
            }
            match status {
                TaskPartstat::Accepted | TaskPartstat::Tentative => {
                    assign::set_proposed(&mut t, None);
                    StepKind::Accepted
                }
                TaskPartstat::Declined | TaskPartstat::Delegated => StepKind::Declined,
                TaskPartstat::InProcess if before == Some(Status::Done) => StepKind::Recalled,
                TaskPartstat::InProcess => StepKind::Started,
                TaskPartstat::Completed => StepKind::Finished,
                TaskPartstat::NeedsAction => {
                    assign::set_proposed(&mut t, None);
                    StepKind::Reopened
                }
            }
        }
        "COUNTER" => {
            let Some(due) = inc.task.due.as_ref().map(|d| assign::in_zone_of(d, own.due.as_ref())) else { return Received::Same };
            assign::set_proposed(&mut t, Some(&due));
            let s = step(StepKind::Proposed, &who, &inc.comment, Some(&due), at);
            return applied(own, t, s);
        }
        "DECLINECOUNTER" => {
            assign::set_proposed(&mut t, None);
            StepKind::Kept
        }
        "CANCEL" => {
            t.progress = Progress::Cancelled;
            StepKind::Revoked
        }
        "REQUEST" => {
            // Поля поручения — как у поручившего (сервер обычно уже обновил копию сам).
            let utc = jiff::tz::TimeZone::UTC;
            let due_changed = inc.task.due_at(&utc) != own.due_at(&utc);
            t.title = inc.task.title.clone();
            t.notes = inc.task.notes.clone();
            if due_changed {
                t.due = inc.task.due.as_ref().map(|d| assign::in_zone_of(d, own.due.as_ref()));
            }
            t.all_day = inc.task.all_day;
            t.sequence = inc.task.sequence;
            if inc.result_accepted {
                assign::set_prop(&mut t, assign::X_RESULT, Some("accepted"));
                t.progress = Progress::Completed;
                StepKind::ResultAccepted
            } else if inc.reminder {
                StepKind::Reminded
            } else if before == Some(Status::Done) && inc.task.progress == Progress::InProcess {
                if let Some(me) = assign::assignee(own).map(|p| p.email.clone()) {
                    assign::set_partstat(&mut t, &me, TaskPartstat::InProcess);
                }
                t.progress = Progress::InProcess;
                StepKind::Returned
            } else if before == Some(Status::Proposed) && due_changed {
                assign::set_proposed(&mut t, None);
                StepKind::Agreed
            } else if due_changed {
                StepKind::DueChanged
            } else if assign::steps(own).is_empty() {
                StepKind::Assigned
            } else if assign::assignee(&inc.task).is_some_and(|a| assign::assignee(own).is_some_and(|o| !same(&a.email, &o.email))) {
                StepKind::Reassigned
            } else {
                return if t == *own { Received::Same } else { Received::Applied(Box::new(t)) };
            }
        }
        _ => return Received::Same,
    };
    let due = matches!(kind, StepKind::DueChanged | StepKind::Agreed | StepKind::Assigned).then(|| t.due.clone()).flatten();
    let s = step(kind, &who, &inc.comment, due.as_ref(), at);
    applied(own, t, s)
}

fn applied(own: &Task, mut t: Task, s: Step) -> Received {
    let added = assign::push_step(&mut t, s);
    if !added && t == *own {
        return Received::Same;
    }
    Received::Applied(Box::new(t))
}

/// Письмо шага: тема и текст (Markdown → текст и HTML для людей без Postman, решение 3) — от интерфейса (Fluent),
/// часть `text/calendar` — iTIP шага.
pub fn draft(out: &Outgoing, from: &Address, subject: String, markdown: String) -> Draft {
    // Шаг поручения не подписывается и не шифруется (как ответ на приглашение): остальное — по умолчанию.
    Draft {
        from: from.clone(),
        to: out.to.clone(),
        subject,
        markdown,
        calendar: Some(CalendarPart { method: out.method.clone(), ics: out.ics.clone() }),
        ..Default::default()
    }
}

// --- раздел «Поручения» (S6-11) ---

/// Вкладка раздела.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Tab {
    /// «Я поручил»: я поручивший (и наблюдатель, если «Только свои» снят).
    #[default]
    ByMe,
    /// «Мне поручили»: я исполнитель.
    ToMe,
}

/// Группа вкладки (макеты «Поручил я», «Поручено мне»).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bucket {
    /// «На проверку» — готово, ждут моего решения.
    Review,
    /// «В работе» — принято или в работе.
    InWork,
    /// «Ждут принятия» — отклонено, предложен срок, ждёт ответа.
    Waiting,
    /// «Новые» — мне поручили, ждут моего ответа (и мои «отклонил», «предложил срок»).
    New,
    /// «На проверке» у автора — я отправил результат.
    Checking,
}

/// Группа поручения во вкладке по статусу.
pub fn bucket(st: Status, tab: Tab) -> Option<Bucket> {
    if st.is_final() {
        return None;
    }
    Some(match (tab, st) {
        (Tab::ByMe, Status::Done) => Bucket::Review,
        (Tab::ToMe, Status::Done) => Bucket::Checking,
        (_, Status::Accepted | Status::InProgress) => Bucket::InWork,
        (Tab::ByMe, _) => Bucket::Waiting,
        (Tab::ToMe, _) => Bucket::New,
    })
}

/// Порядок групп во вкладке.
pub fn buckets(tab: Tab) -> &'static [Bucket] {
    match tab {
        Tab::ByMe => &[Bucket::Review, Bucket::InWork, Bucket::Waiting],
        Tab::ToMe => &[Bucket::New, Bucket::InWork, Bucket::Checking],
    }
}

/// Поручение во вкладке: «Я поручил» — мои (и где я наблюдатель, если `own` снят), «Мне поручили» — мне;
/// закрытые и забранные — нет.
pub fn in_tab(t: &Task, me: &str, tab: Tab, own: bool) -> bool {
    let open = assign::status(t).is_some_and(|s| !s.is_final());
    open && t.parent.is_none()
        && match tab {
            Tab::ByMe => t.is_delegated_by(me) || (!own && assign::is_watched_by(t, me)),
            Tab::ToMe => t.is_assigned_to(me),
        }
}

/// Поручения вкладки по группам: внутри — «Ждут принятия» по срочности ответа (отклонено, предложен срок, ждёт),
/// остальные — по сроку.
pub fn section<'a>(tasks: &'a [Task], me: &str, tab: Tab, own: bool, tz: &jiff::tz::TimeZone) -> Vec<(Bucket, Vec<&'a Task>)> {
    let rank = |s: Status| match s {
        Status::Declined => 0,
        Status::Proposed => 1,
        _ => 2,
    };
    buckets(tab)
        .iter()
        .filter_map(|&b| {
            let mut v: Vec<&Task> =
                tasks.iter().filter(|t| in_tab(t, me, tab, own) && assign::status(t).and_then(|s| bucket(s, tab)) == Some(b)).collect();
            v.sort_by(|x, y| {
                let (sx, sy) = (assign::status(x).map_or(2, rank), assign::status(y).map_or(2, rank));
                let r = if b == Bucket::Waiting { sx.cmp(&sy) } else { std::cmp::Ordering::Equal };
                r.then_with(|| x.due_at(tz).unwrap_or(i64::MAX).cmp(&y.due_at(tz).unwrap_or(i64::MAX))).then_with(|| x.title.cmp(&y.title))
            });
            (!v.is_empty()).then_some((b, v))
        })
        .collect()
}

/// Счётчики раздела: пункт слева (поручений всего и бейдж «ждут ответа + на проверку»), вкладки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    /// «Я поручил» (только свои).
    pub by_me: usize,
    /// «Я поручил» с учётом «Только свои».
    pub by_me_shown: usize,
    pub to_me: usize,
    /// Мне поручили, ждут моего ответа.
    pub new: usize,
    /// Готово, ждут моей проверки.
    pub review: usize,
    /// Мои невыполненные — в «Ждём».
    pub waiting: usize,
}

pub fn counts(tasks: &[Task], me: &str, own: bool) -> Counts {
    let st = |t: &Task| assign::status(t);
    let mut c = Counts::default();
    for t in tasks {
        if in_tab(t, me, Tab::ByMe, true) {
            c.by_me += 1;
            c.review += usize::from(st(t) == Some(Status::Done));
            c.waiting += usize::from(waits(t, me));
        }
        c.by_me_shown += usize::from(in_tab(t, me, Tab::ByMe, own));
        if in_tab(t, me, Tab::ToMe, true) {
            c.to_me += 1;
            c.new += usize::from(st(t) == Some(Status::Waiting));
        }
    }
    c
}

/// Моё невыполненное поручение — ждём исполнителя (быстрый фильтр «Ждём», макет «Поручил я»): я поручивший,
/// поручение не закрыто и не на проверке у меня.
pub fn waits(t: &Task, me: &str) -> bool {
    t.is_delegated_by(me) && assign::status(t).is_some_and(|s| !s.is_final() && s != Status::Done)
}

/// Слова писем моих невыполненных поручений ([`assign::keyword`]): «Ждём» показывает эти письма из «Отправленных».
pub fn waiting_keywords(tasks: &[Task], me: &str) -> Vec<String> {
    let mut v: Vec<String> = tasks.iter().filter(|t| waits(t, me)).map(|t| assign::keyword(&t.uid)).collect();
    v.sort();
    v.dedup();
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::tz::TimeZone;

    fn tz() -> TimeZone {
        TimeZone::get("Europe/Moscow").unwrap()
    }

    fn at(s: &str) -> Moment {
        Moment::new(s.parse().unwrap(), Some("Europe/Moscow"))
    }

    fn ask(to: &Address, due: Option<Moment>, comment: &str) -> Input {
        Input { comment: comment.into(), due, to: Some(to.clone()) }
    }

    const BOSS: &str = "boss@itt.test";
    const ME: &str = "me@itt.test";
    const OBS: &str = "obs@itt.test";

    fn parse(ics: &str) -> Incoming {
        Incoming::from_worker(&postman_worker::parse_calendar(ics.as_bytes()).unwrap()).unwrap()
    }

    /// Поручивший: поручить → (у исполнителя: из письма) → принять → в работе → готово → принять результат;
    /// каждое письмо исполнителя применяется к копии поручившего со своим комментарием. Наблюдатель — в первом
    /// письме и в итогах.
    #[test]
    fn full_cycle_between_two_copies() {
        let tz = tz();
        let boss = Address::new("Начальник", BOSS);
        let me = Address::new("Исполнитель", ME);
        let task = Task::new("d1@postman", "b", "Проверить отчёт");
        let plan =
            assign(&task, &boss, &[Address::new("Наблюдатель", OBS)], &ask(&me, Some(at("2027-03-04T18:00:00")), "Нужен к четвергу"), 100, &tz)
                .unwrap();
        assert_eq!(plan.status, Status::Waiting);
        let out = &plan.outgoing[0];
        assert_eq!((out.method.as_str(), out.to.iter().map(|a| a.email.as_str()).collect::<Vec<_>>()), ("REQUEST", vec![ME, OBS]));
        let mut theirs = plan.task.clone();
        let mut mine = parse(&out.ics).task;
        mine.id = "copy".into();
        assert!(mine.is_assigned_to(ME) && theirs.is_delegated_by(BOSS));
        // Копия исполнителя из письма: шаг «поручено» с комментарием.
        let Received::Applied(m) = receive(&mine, &parse(&out.ics), 110) else { panic!() };
        mine = *m;
        assert_eq!(assign::steps(&mine)[0].comment, "Нужен к четвергу");

        let ping = |mine: &mut Task, theirs: &mut Task, a: Action, input: Input, now: i64| -> Plan {
            let p = act(mine, ME, a, &input, now, &tz).unwrap();
            *mine = p.task.clone();
            for o in &p.outgoing {
                if let Received::Applied(t) = receive(theirs, &parse(&o.ics), now) {
                    *theirs = *t;
                }
            }
            p
        };
        ping(&mut mine, &mut theirs, Action::Accept, Input::default(), 200);
        assert_eq!(assign::status(&theirs), Some(Status::Accepted));
        ping(&mut mine, &mut theirs, Action::Start, Input::default(), 300);
        let done = ping(&mut mine, &mut theirs, Action::Finish, Input { comment: "Отчёт во вложении".into(), ..Input::default() }, 400);
        assert_eq!(done.outgoing[0].to.iter().map(|a| a.email.as_str()).collect::<Vec<_>>(), [BOSS, OBS], "итог — и наблюдателю");
        assert_eq!(assign::status(&theirs), Some(Status::Done));
        let kinds: Vec<StepKind> = assign::steps(&theirs).iter().map(|s| s.kind).collect();
        assert_eq!(kinds, [StepKind::Assigned, StepKind::Accepted, StepKind::Started, StepKind::Finished]);
        assert_eq!(assign::steps(&theirs)[3].comment, "Отчёт во вложении", "комментарий — из письма");

        // Поручивший принимает результат → у исполнителя закрыто.
        let res = act(&theirs, BOSS, Action::AcceptResult, &Input::default(), 500, &tz).unwrap();
        assert_eq!(res.status, Status::Closed);
        assert_eq!(res.task.progress, Progress::Completed);
        assert_eq!(res.outgoing[0].to.len(), 2);
        let Received::Applied(m) = receive(&mine, &parse(&res.outgoing[0].ics), 510) else { panic!() };
        assert_eq!(assign::status(&m), Some(Status::Closed));
        // То же письмо второй раз — без изменений.
        assert_eq!(receive(&m, &parse(&res.outgoing[0].ics), 520), Received::Same);
    }

    #[test]
    fn sides_due_and_reminders() {
        let tz = tz();
        let plan = assign(
            &Task::new("d2", "b", "КП"),
            &Address::new("", BOSS),
            &[],
            &ask(&Address::new("", ME), Some(at("2027-03-04T18:00:00")), ""),
            0,
            &tz,
        )
        .unwrap();
        let t = plan.task;
        // Исполнитель не принимает результат, поручивший не «принимает» поручение.
        assert_eq!(act(&t, ME, Action::Remind, &Input::default(), 0, &tz), Err(Error::WrongSide));
        assert_eq!(act(&t, BOSS, Action::Accept, &Input::default(), 0, &tz), Err(Error::WrongSide));
        assert_eq!(
            act(&t, BOSS, Action::AcceptResult, &Input::default(), 0, &tz),
            Err(Error::Invalid(Invalid { from: Status::Waiting, action: Action::AcceptResult }))
        );
        assert_eq!(act(&t, BOSS, Action::ChangeDue, &Input::default(), 0, &tz), Err(Error::NoDue));
        assert_eq!(act(&Task::new("x", "", "своя"), BOSS, Action::Remind, &Input::default(), 0, &tz), Err(Error::NotDelegation));

        // «Напомнить» — REQUEST без нового SEQUENCE, не чаще раза в сутки.
        let r = act(&t, BOSS, Action::Remind, &Input::default(), 1000, &tz).unwrap();
        assert_eq!(r.task.sequence, t.sequence);
        assert!(parse(&r.outgoing[0].ics).reminder);
        let next = 1000 + assign::REMIND_EVERY;
        assert_eq!(act(&r.task, BOSS, Action::Remind, &Input::default(), 2000, &tz), Err(Error::TooSoon { next }));
        assert!(act(&r.task, BOSS, Action::Remind, &Input::default(), next, &tz).is_ok());

        // Новый срок — SEQUENCE + 1; у исполнителя — шаг «срок изменён».
        let due = at("2027-03-05T18:00:00");
        let c = act(&t, BOSS, Action::ChangeDue, &Input { due: Some(due.clone()), ..Input::default() }, 3000, &tz).unwrap();
        assert_eq!((c.task.sequence, c.task.due.clone()), (1, Some(due.clone())));
        let mine = parse(&plan.outgoing[0].ics).task;
        let Received::Applied(m) = receive(&mine, &parse(&c.outgoing[0].ics), 3001) else { panic!() };
        assert_eq!(m.due_at(&tz), due.timestamp(&tz));
        assert_eq!(assign::steps(&m).last().map(|s| s.kind), Some(StepKind::DueChanged));
        // Ответ на старый запрос (SEQUENCE 0) к копии с SEQUENCE 1 — устарел.
        let old = act(&mine, ME, Action::Accept, &Input::default(), 3002, &tz).unwrap();
        assert_eq!(receive(&c.task, &parse(&old.outgoing[0].ics), 3003), Received::Stale);
    }

    /// Предложить срок → согласиться (новый срок, принято) или оставить (DECLINECOUNTER, снова ждёт).
    #[test]
    fn counter_agree_and_keep() {
        let tz = tz();
        let plan = assign(
            &Task::new("d3", "b", "ТЗ"),
            &Address::new("", BOSS),
            &[],
            &ask(&Address::new("", ME), Some(at("2027-03-04T12:00:00")), ""),
            0,
            &tz,
        )
        .unwrap();
        let mut boss = plan.task.clone();
        let mine = parse(&plan.outgoing[0].ics).task;
        let want = at("2027-03-09T18:00:00");
        let p =
            act(&mine, ME, Action::ProposeDue, &Input { due: Some(want.clone()), comment: "Жду разъяснений".into(), ..Input::default() }, 10, &tz)
                .unwrap();
        assert_eq!((p.status, p.outgoing[0].method.as_str()), (Status::Proposed, "COUNTER"));
        let Received::Applied(b) = receive(&boss, &parse(&p.outgoing[0].ics), 11) else { panic!() };
        boss = *b;
        assert_eq!(assign::status(&boss), Some(Status::Proposed));
        assert_eq!(assign::proposed(&boss).and_then(|m| m.timestamp(&tz)), want.timestamp(&tz));
        let last = assign::steps(&boss).pop().unwrap();
        assert_eq!((last.kind, last.comment.as_str()), (StepKind::Proposed, "Жду разъяснений"));

        let agree = act(&boss, BOSS, Action::AgreeDue, &Input::default(), 20, &tz).unwrap();
        assert_eq!((agree.status, agree.task.due_at(&tz)), (Status::Accepted, want.timestamp(&tz)));
        let keep = act(&boss, BOSS, Action::KeepDue, &Input::default(), 20, &tz).unwrap();
        assert_eq!((keep.status, keep.outgoing[0].method.as_str()), (Status::Waiting, "DECLINECOUNTER"));
        let Received::Applied(m) = receive(&p.task, &parse(&keep.outgoing[0].ics), 21) else { panic!() };
        assert!(assign::proposed(&m).is_none());
        assert_eq!(assign::steps(&m).last().map(|s| s.kind), Some(StepKind::Kept));
    }

    /// Отказ с причиной → «Поручить другому»; «Забрать» — CANCEL исполнителю и наблюдателям.
    #[test]
    fn decline_reassign_and_revoke() {
        let tz = tz();
        let plan = assign(
            &Task::new("d4", "b", "Пропуска"),
            &Address::new("", BOSS),
            &[Address::new("", OBS)],
            &ask(&Address::new("", ME), None, ""),
            0,
            &tz,
        )
        .unwrap();
        let mine = parse(&plan.outgoing[0].ics).task;
        let d = act(&mine, ME, Action::Decline, &Input { comment: "Нет доступа к системе".into(), ..Input::default() }, 5, &tz).unwrap();
        assert_eq!(d.outgoing[0].to.len(), 2, "отказ — и наблюдателю");
        let Received::Applied(boss) = receive(&plan.task, &parse(&d.outgoing[0].ics), 6) else { panic!() };
        assert_eq!(assign::status(&boss), Some(Status::Declined));
        assert_eq!(assign::steps(&boss).last().unwrap().comment, "Нет доступа к системе");
        let other = Address::new("Другой", "other@itt.test");
        let r = act(&boss, BOSS, Action::Reassign, &Input { to: Some(other), ..Input::default() }, 7, &tz).unwrap();
        assert_eq!(r.status, Status::Waiting);
        assert_eq!(assign::assignee(&r.task).map(|p| p.email.as_str()), Some("other@itt.test"));
        assert_eq!(r.outgoing[0].to[0].email, "other@itt.test");
        let x = act(&r.task, BOSS, Action::Revoke, &Input::default(), 8, &tz).unwrap();
        assert_eq!((x.status, x.outgoing[0].method.as_str(), x.outgoing[0].to.len()), (Status::Revoked, "CANCEL", 2));
        let draft = draft(&x.outgoing[0], &Address::new("", BOSS), "Забрано: Пропуска".into(), "Текст".into());
        assert_eq!(draft.calendar.as_ref().map(|c| c.method.as_str()), Some("CANCEL"));
        assert_eq!(draft.to.len(), 2);
    }

    fn names<'a>(s: &[(Bucket, Vec<&'a Task>)]) -> Vec<(Bucket, Vec<&'a str>)> {
        s.iter().map(|(b, v)| (*b, v.iter().map(|t| t.title.as_str()).collect())).collect()
    }

    /// Вкладки «Я поручил» / «Мне поручили» и «Только свои» на поручениях демо: группы, порядок, счётчики.
    #[test]
    fn tabs_and_own_only() {
        use postman_demo::delegations as d;
        let tz = tz();
        let list = d::all(jiff::civil::date(2026, 10, 21), 1_792_000_000);
        let me = postman_demo::ME;
        let mut by_me = names(&section(&list, me, Tab::ByMe, true, &tz));
        // «Ждут принятия»: отклонено, предложен срок, ждёт.
        assert_eq!(
            by_me,
            [
                (Bucket::Review, vec![d::REVIEW]),
                (Bucket::InWork, vec!["Ресурсный план группы на ноябрь"]),
                (Bucket::Waiting, vec!["Заказать пропуска для аудиторов", "Требования к мониторингу для ТЗ", "Расчёт коммерческого предложения"]),
            ]
        );
        // «Только свои» снят — ещё и где я наблюдатель.
        by_me[1].1.push(d::WATCHED);
        assert_eq!(names(&section(&list, me, Tab::ByMe, false, &tz)), by_me);
        assert_eq!(
            names(&section(&list, me, Tab::ToMe, true, &tz)),
            [(Bucket::New, vec![d::LETTER, d::INCOMING]), (Bucket::InWork, vec!["Комментарии к статусу проектов"]),]
        );
        let c = counts(&list, me, false);
        assert_eq!(c, Counts { by_me: 5, by_me_shown: 6, to_me: 3, new: 2, review: 1, waiting: 4 });
        // Наблюдаю — не мне поручено и не я поручил; «Ждём» — только мои.
        let watched = list.iter().find(|t| t.title == d::WATCHED).unwrap();
        assert!(assign::is_watched_by(watched, me) && !waits(watched, me));
        assert!(!in_tab(watched, me, Tab::ToMe, false));
    }

    /// «Ждём» на демо-ящике: письма моих невыполненных поручений (из «Отправленных», по слову UID) — в фильтре;
    /// готово (ждёт моей проверки) и после приёмки результата — нет. Письмо и с «Ждём ответа», и поручения —
    /// одно.
    #[test]
    fn waiting_filter_on_demo_box() {
        use crate::filters::{Facts, Me};
        use postman_cache::{Cache, Scan};
        use postman_demo::delegations as d;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo.redb");
        postman_demo::ensure(&path, 60).unwrap();
        let cache = Cache::open(&path).unwrap();
        let now = jiff::Timestamp::now().as_second();
        let today = jiff::Timestamp::now().to_zoned(TimeZone::system()).date();
        let me = postman_demo::ME;
        let waiting = |list: &[Task]| {
            let who = Me::new([me], [""; 0]).with_waiting(waiting_keywords(list, me));
            let mut ids = Vec::new();
            cache
                .scan(&Scan::Account(None), |row, to| {
                    if row.id.starts_with("deleg-mail") && Facts::of(&who, &row, &to).waiting {
                        ids.push(row.id);
                    }
                    true
                })
                .unwrap();
            ids.sort();
            ids
        };
        let mut list = d::all(today, now);
        assert_eq!(waiting(&list), ["deleg-mail-2", "deleg-mail-3", "deleg-mail-4", "deleg-mail-5"], "готово (1) ждёт меня, а не исполнителя");
        // Исполнитель сделал (2) — на проверке у меня; я принял результат — закрыто.
        let i = list.iter().position(|t| t.uid == "demo-deleg-2@postman").unwrap();
        let worker = assign::assignee(&list[i]).unwrap().email.clone();
        assign::set_partstat(&mut list[i], &worker, TaskPartstat::Completed);
        assert_eq!(waiting(&list), ["deleg-mail-3", "deleg-mail-4", "deleg-mail-5"]);
        list[i] = act(&list[i], me, Action::AcceptResult, &Input::default(), now, &tz()).unwrap().task;
        assert_eq!(assign::status(&list[i]), Some(Status::Closed));
        assert_eq!(waiting(&list), ["deleg-mail-3", "deleg-mail-4", "deleg-mail-5"]);
        // Слово «Ждём ответа» у письма поручения — письмо в счётчике одно.
        let mut c = crate::filters::Counts::default();
        let crit = crate::filters::Criteria { quick: [crate::filters::Quick::Waiting].into(), ..Default::default() };
        let who = Me::new([me], [""; 0]).with_waiting(waiting_keywords(&list, me));
        let mut row = cache.rows_by_ids(&["deleg-mail-3".to_string()]).unwrap().remove(0);
        row.keywords.push(crate::filters::WAIT.into());
        c.add(&crit, &Facts::of(&who, &row, &Default::default()), &row.keywords);
        assert_eq!(c.quick[2], 1);
    }
}
