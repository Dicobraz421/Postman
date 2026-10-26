//! Поручение (S6-09, решение владельца 2): iTIP по RFC 5546 для VTODO — модель без I/O.
//!
//! - Статус поручения ([`Status`]) — из задачи: ответ исполнителя (PARTSTAT), ход задачи и свои отметки Postman
//!   (`X-POSTMAN-PROPOSED` — предложенный срок, `X-POSTMAN-RESULT` — результат принят). Переходы — таблица
//!   [`next`]: что можно сделать из какого статуса и кому (поручившему или исполнителю); недопустимое — ошибка.
//! - Лента шагов ([`Step`]: кто, что, когда, комментарий) — свойства `X-POSTMAN-STEP` своей копии задачи (JSON в
//!   значении); сервер хранит их как есть (`docs/STALWART.md` п. 38).
//! - Письма iTIP: REQUEST (поручить, изменить срок, вернуть, напомнить, принять результат), REPLY (принял, в
//!   работе, готово, отклонил), COUNTER (предложить срок), DECLINECOUNTER (оставить срок), CANCEL (забрать).
//!   Комментарий шага — COMMENT: сервер его не хранит (GAPS 41, 42), Postman читает его из письма ([`Incoming`]).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::invite::{fold, text};
use crate::task::{Moment, Progress, Role, Task, TaskPartstat, cn, utc_ical};

/// Свойство шага ленты (одно на шаг).
pub const X_STEP: &str = "x-postman-step";
/// Предложенный исполнителем срок (COUNTER), ждёт решения поручившего: местное время срока задачи.
pub const X_PROPOSED: &str = "x-postman-proposed";
/// Результат принят поручившим (`accepted`) — задача закрыта у обоих.
pub const X_RESULT: &str = "x-postman-result";
/// «Напомнить»: REQUEST без изменения `SEQUENCE` (решение 10).
pub const X_REMINDER: &str = "x-postman-reminder";
/// Напоминать не чаще (решение 10).
pub const REMIND_EVERY: i64 = 24 * 60 * 60;

/// Статус поручения: круги 20 px (DESIGN «Задачи», S6-10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    /// Ждёт принятия (серый точечный круг).
    Waiting,
    /// Исполнитель предложил срок — ждёт решения поручившего.
    Proposed,
    /// Принято (синий ✓).
    Accepted,
    /// В работе (синий ▶).
    InProgress,
    /// Готово — на проверке у поручившего (зелёный ✓✓).
    Done,
    /// Отклонено (тёмно-серый ✕).
    Declined,
    /// Результат принят — задача закрыта.
    Closed,
    /// Забрано поручившим (CANCEL).
    Revoked,
}

impl Status {
    /// Поручение больше не движется.
    pub fn is_final(self) -> bool {
        matches!(self, Status::Closed | Status::Revoked)
    }

    pub fn id(self) -> &'static str {
        match self {
            Status::Waiting => "waiting",
            Status::Proposed => "proposed",
            Status::Accepted => "accepted",
            Status::InProgress => "in-progress",
            Status::Done => "done",
            Status::Declined => "declined",
            Status::Closed => "closed",
            Status::Revoked => "revoked",
        }
    }
}

/// Действие с поручением.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    // --- поручивший ---
    /// Напомнить исполнителю (не чаще раза в сутки).
    Remind,
    ChangeDue,
    /// Согласиться с предложенным сроком.
    AgreeDue,
    /// Оставить прежний срок (DECLINECOUNTER).
    KeepDue,
    AcceptResult,
    /// Вернуть с комментарием — снова в работе.
    Return,
    /// Забрать задачу себе (CANCEL исполнителю).
    Revoke,
    /// Поручить другому (после отказа).
    Reassign,
    // --- исполнитель ---
    Accept,
    Decline,
    ProposeDue,
    /// В работе.
    Start,
    /// Готово — на проверку.
    Finish,
    /// Отозвать с проверки — снова в работе.
    Recall,
    /// «Изменить ответ» после отказа или предложения срока — снова ждёт ответа.
    Reopen,
}

impl Action {
    /// Действие поручившего (иначе — исполнителя).
    pub fn by_owner(self) -> bool {
        matches!(
            self,
            Action::Remind
                | Action::ChangeDue
                | Action::AgreeDue
                | Action::KeepDue
                | Action::AcceptResult
                | Action::Return
                | Action::Revoke
                | Action::Reassign
        )
    }
}

/// Недопустимый переход.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Invalid {
    pub from: Status,
    pub action: Action,
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "delegation: {:?} is not allowed from {:?}", self.action, self.from)
    }
}

impl std::error::Error for Invalid {}

/// Таблица переходов: статус после действия; недопустимое — [`Invalid`].
pub fn next(from: Status, action: Action) -> Result<Status, Invalid> {
    use Action as A;
    use Status as S;
    let to = match (from, action) {
        (S::Waiting, A::Accept) => S::Accepted,
        (S::Waiting, A::Decline) => S::Declined,
        (S::Waiting | S::Accepted | S::InProgress, A::ProposeDue) => S::Proposed,
        (S::Accepted, A::Start) => S::InProgress,
        (S::Accepted | S::InProgress, A::Finish) => S::Done,
        (S::Done, A::Recall) => S::InProgress,
        (S::Proposed | S::Declined, A::Reopen) => S::Waiting,
        (s @ (S::Waiting | S::Accepted | S::InProgress), A::Remind) => s,
        (s @ (S::Waiting | S::Accepted | S::InProgress | S::Done), A::ChangeDue) => s,
        (S::Proposed, A::AgreeDue) => S::Accepted,
        (S::Proposed, A::KeepDue) => S::Waiting,
        (S::Done, A::AcceptResult) => S::Closed,
        (S::Done, A::Return) => S::InProgress,
        (S::Waiting | S::Proposed | S::Accepted | S::InProgress | S::Done | S::Declined, A::Revoke) => S::Revoked,
        (S::Declined, A::Reassign) => S::Waiting,
        _ => return Err(Invalid { from, action }),
    };
    Ok(to)
}

/// Что может сделать сторона (`owner` — поручивший) из статуса: кнопки карточки.
pub fn actions(from: Status, owner: bool) -> Vec<Action> {
    use Action as A;
    let all = if owner {
        [A::AcceptResult, A::AgreeDue, A::Reassign, A::Return, A::KeepDue, A::Remind, A::ChangeDue, A::Revoke].as_slice()
    } else {
        [A::Accept, A::Start, A::Finish, A::ProposeDue, A::Decline, A::Recall, A::Reopen].as_slice()
    };
    all.iter().copied().filter(|a| next(from, *a).is_ok()).collect()
}

/// Это поручение: есть поручивший и исполнитель.
pub fn is_delegation(t: &Task) -> bool {
    t.organizer().is_some() && t.assignees().next().is_some()
}

/// Я наблюдаю за поручением (`ROLE=NON-PARTICIPANT`): не поручивший и не исполнитель.
pub fn is_watched_by(t: &Task, me: &str) -> bool {
    is_delegation(t) && t.participants.iter().any(|p| p.role == Role::Observer && p.email.eq_ignore_ascii_case(me))
}

/// Слово письма-поручения в «Отправленных» (S6-11): `pm.assign.<16 hex>` — FNV-1a от UID в нижнем регистре
/// (UID в ключевое слово JMAP целиком не годится: знаки и длина). По нему «Ждём» находит письма моих
/// невыполненных поручений.
pub const KEYWORD: &str = "pm.assign";

pub fn keyword(uid: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in uid.trim().to_ascii_lowercase().bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{KEYWORD}.{h:016x}")
}

/// «Забрать»: своя копия снова обычная задача — без участников, своих отметок поручения и хода; лента шагов
/// остаётся (история).
pub fn taken_back(t: &Task) -> Task {
    let mut t = t.clone();
    t.participants.clear();
    t.progress = Progress::NeedsAction;
    t.percent = None;
    t.completed_at = None;
    set_proposed(&mut t, None);
    set_prop(&mut t, X_RESULT, None);
    t
}

/// Исполнитель поручения (исполнитель — один, макет «Поручить»).
pub fn assignee(t: &Task) -> Option<&crate::TaskParticipant> {
    t.assignees().next()
}

/// Статус поручения; не поручение — `None`.
pub fn status(t: &Task) -> Option<Status> {
    let a = assignee(t)?;
    t.organizer()?;
    if t.progress == Progress::Cancelled {
        return Some(Status::Revoked);
    }
    // Принято поручившим: своя отметка или (у исполнителя) STATUS:COMPLETED из его REQUEST при «готово» —
    // своё «готово» исполнитель держит «в работе» до приёмки. Свои X-свойства из REQUEST сервер исполнителя в
    // копию не берёт (стенд, `postman_delegation_cycle`).
    let accepted = prop(t, X_RESULT).is_some_and(|r| r.eq_ignore_ascii_case("accepted"));
    if accepted || (t.progress == Progress::Completed && a.status == TaskPartstat::Completed) {
        return Some(Status::Closed);
    }
    if proposed(t).is_some() && a.status != TaskPartstat::Declined && a.status != TaskPartstat::Completed {
        return Some(Status::Proposed);
    }
    Some(match a.status {
        TaskPartstat::Declined | TaskPartstat::Delegated => Status::Declined,
        TaskPartstat::Completed => Status::Done,
        TaskPartstat::InProcess => Status::InProgress,
        TaskPartstat::Accepted | TaskPartstat::Tentative => Status::Accepted,
        TaskPartstat::NeedsAction => Status::Waiting,
    })
}

// --- свои свойства в `Task::extra` ---

fn is_prop(p: &Value, name: &str) -> bool {
    p[0].as_str().is_some_and(|n| n.eq_ignore_ascii_case(name))
}

fn prop_text(p: &Value) -> String {
    match &p[3] {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        x => x.to_string(),
    }
}

/// Значение своего свойства (первое).
pub fn prop(t: &Task, name: &str) -> Option<String> {
    t.extra.iter().find(|p| is_prop(p, name)).map(prop_text)
}

/// Поставить (или снять — `None`) своё свойство.
pub fn set_prop(t: &mut Task, name: &str, value: Option<&str>) {
    let Some(v) = value else {
        t.extra.retain(|p| !is_prop(p, name));
        return;
    };
    let new = json!([name, {}, "unknown", v]);
    // На месте прежнего: порядок свойств не меняется — повторная правка та же.
    match t.extra.iter().position(|p| is_prop(p, name)) {
        Some(i) => {
            t.extra[i] = new;
            let mut seen = false;
            t.extra.retain(|p| !is_prop(p, name) || !std::mem::replace(&mut seen, true));
        }
        None => t.extra.push(new),
    }
}

/// Момент `m` в поясе `like` (срок своей копии): письма iTIP приходят в UTC, своя копия — в своём поясе.
pub fn in_zone_of(m: &Moment, like: Option<&Moment>) -> Moment {
    let Some(tz_name) = like.and_then(|l| l.tz.clone()) else { return m.clone() };
    let Ok(zone) = jiff::tz::TimeZone::get(&tz_name) else { return m.clone() };
    let Some(ts) = m.timestamp(&zone) else { return m.clone() };
    match jiff::Timestamp::from_second(ts) {
        Ok(t) => Moment { local: t.to_zoned(zone).datetime(), tz: Some(tz_name) },
        Err(_) => m.clone(),
    }
}

/// Предложенный исполнителем срок.
pub fn proposed(t: &Task) -> Option<Moment> {
    let v = prop(t, X_PROPOSED)?;
    let local = v.trim().parse().ok()?;
    Some(Moment { local, tz: t.due.as_ref().and_then(|d| d.tz.clone()) })
}

/// Поставить (или снять) предложенный срок.
pub fn set_proposed(t: &mut Task, due: Option<&Moment>) {
    let v = due.map(|m| m.local.strftime("%Y-%m-%dT%H:%M:%S").to_string());
    set_prop(t, X_PROPOSED, v.as_deref());
}

// --- лента шагов ---

/// Что произошло.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StepKind {
    Assigned,
    Accepted,
    Declined,
    Proposed,
    Started,
    Finished,
    Recalled,
    Reopened,
    Reminded,
    DueChanged,
    Agreed,
    Kept,
    ResultAccepted,
    Returned,
    Revoked,
    Reassigned,
}

/// Шаг ленты поручения.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Step {
    /// Unix-секунды.
    pub at: i64,
    /// Адрес того, кто сделал шаг.
    pub who: String,
    pub kind: StepKind,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub comment: String,
    /// Срок шага (новый или предложенный): местное время срока задачи.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<String>,
}

/// Шаги по времени (старые первыми); битые — пропускаются.
pub fn steps(t: &Task) -> Vec<Step> {
    let mut out: Vec<Step> = t.extra.iter().filter(|p| is_prop(p, X_STEP)).filter_map(|p| serde_json::from_str(&prop_text(p)).ok()).collect();
    out.sort_by_key(|s| s.at);
    out
}

/// Добавить шаг; такой же (время, кто, что) уже есть — `false` (письмо открыто второй раз).
pub fn push_step(t: &mut Task, step: Step) -> bool {
    if steps(t).iter().any(|s| s.at == step.at && s.kind == step.kind && s.who.eq_ignore_ascii_case(&step.who)) {
        return false;
    }
    let Ok(v) = serde_json::to_string(&step) else { return false };
    t.extra.push(json!([X_STEP, {}, "unknown", v]));
    true
}

/// Последний «Напомнить»; следующий — не раньше суток после него (решение 10).
pub fn next_reminder(t: &Task) -> Option<i64> {
    steps(t).iter().filter(|s| s.kind == StepKind::Reminded).map(|s| s.at + REMIND_EVERY).max()
}

/// Поставить ответ участника `email`.
pub fn set_partstat(t: &mut Task, email: &str, status: TaskPartstat) -> bool {
    match t.participants.iter_mut().find(|p| p.email.eq_ignore_ascii_case(email)) {
        Some(p) => {
            p.status = status;
            true
        }
        None => false,
    }
}

// --- iTIP ---

/// Вставить строки перед `END:VTODO` (уже свёрнутый текст VCALENDAR).
fn with_lines(ics: &str, lines: &[String]) -> String {
    if lines.is_empty() {
        return ics.to_owned();
    }
    let extra: String = lines.iter().map(|l| fold(l) + "\r\n").collect();
    match ics.find("END:VTODO\r\n") {
        Some(at) => format!("{}{extra}{}", &ics[..at], &ics[at..]),
        None => ics.to_owned(),
    }
}

fn comment_line(comment: &str) -> Option<String> {
    let c = comment.trim();
    (!c.is_empty()).then(|| format!("COMMENT:{}", text(c)))
}

/// REQUEST поручившего: поручить, изменить срок, вернуть с комментарием, напомнить, принять результат. Задача —
/// своя копия после действия (участники, срок, ход, `SEQUENCE`).
pub fn request(t: &Task, comment: &str, reminder: bool, now: i64, tz: &jiff::tz::TimeZone) -> String {
    let mut lines: Vec<String> = comment_line(comment).into_iter().collect();
    if reminder {
        lines.push("X-POSTMAN-REMINDER:1".into());
    }
    if let Some(r) = prop(t, X_RESULT) {
        lines.push(format!("X-POSTMAN-RESULT:{}", text(&r)));
    }
    with_lines(&t.to_ics(Some("REQUEST"), now, tz), &lines)
}

/// CANCEL поручившего («Забрать»): задача отменена у исполнителя и наблюдателей.
pub fn cancel(t: &Task, now: i64, tz: &jiff::tz::TimeZone) -> String {
    let mut c = t.clone();
    c.progress = Progress::Cancelled;
    c.to_ics(Some("CANCEL"), now, tz)
}

/// Короткий VTODO ответа: UID, SEQUENCE, ORGANIZER и одна строка участника (RFC 5546 §3.4.3, §3.4.8).
fn short(method: &str, t: &Task, attendee: &str, extra: &[String], now: i64) -> String {
    let mut l = vec![
        "BEGIN:VCALENDAR".to_string(),
        "PRODID:-//Postman//Postman//RU".into(),
        "VERSION:2.0".into(),
        format!("METHOD:{method}"),
        "BEGIN:VTODO".into(),
        format!("UID:{}", text(&t.uid)),
        format!("DTSTAMP:{}", utc_ical(now)),
        format!("SEQUENCE:{}", t.sequence),
        format!("SUMMARY:{}", text(&t.title)),
    ];
    if let Some(o) = t.organizer() {
        l.push(format!("ORGANIZER{}:mailto:{}", cn(&o.name), o.email));
    }
    l.push(attendee.to_owned());
    l.extend(extra.iter().cloned());
    l.extend(["END:VTODO".to_string(), "END:VCALENDAR".into()]);
    l.iter().map(|x| fold(x)).collect::<Vec<_>>().join("\r\n") + "\r\n"
}

fn attendee_line(name: &str, email: &str, status: TaskPartstat) -> String {
    format!("ATTENDEE;PARTSTAT={}{}:mailto:{email}", status.ical(), cn(name))
}

/// REPLY исполнителя: мой PARTSTAT (принял, в работе, готово, отклонил), процент и комментарий.
pub fn reply(t: &Task, me: (&str, &str), status: TaskPartstat, comment: &str, now: i64) -> String {
    let (name, email) = me;
    let mut extra: Vec<String> = comment_line(comment).into_iter().collect();
    if let Some(p) = t.percent {
        extra.push(format!("PERCENT-COMPLETE:{p}"));
    }
    short("REPLY", t, &attendee_line(name, email, status), &extra, now)
}

/// COUNTER исполнителя: та же задача с предложенным сроком (RFC 5546 §3.4.7) и комментарием.
pub fn counter(t: &Task, me: (&str, &str), due: &Moment, comment: &str, now: i64, tz: &jiff::tz::TimeZone) -> String {
    let mut c = t.clone();
    c.due = Some(due.clone());
    let (name, email) = me;
    // Участник — только я (как в ответе): мой статус — как был.
    let mine = t.participants.iter().find(|p| p.email.eq_ignore_ascii_case(email)).map_or(TaskPartstat::NeedsAction, |p| p.status);
    c.participants.retain(|p| p.role == Role::Owner);
    let ics = c.to_ics(Some("COUNTER"), now, tz);
    let mut lines = vec![attendee_line(name, email, mine)];
    lines.extend(comment_line(comment));
    with_lines(&ics, &lines)
}

/// DECLINECOUNTER поручившего («Оставить срок», RFC 5546 §3.4.8).
pub fn decline_counter(t: &Task, comment: &str, now: i64) -> String {
    let a = assignee(t);
    let line = a.map_or_else(String::new, |a| attendee_line(&a.name, &a.email, TaskPartstat::NeedsAction));
    let extra: Vec<String> = comment_line(comment).into_iter().collect();
    short("DECLINECOUNTER", t, &line, &extra, now)
}

/// Письмо iTIP с задачей, разобранное воркером: метод, задача, комментарий шага, отметки Postman.
#[derive(Debug, Clone, PartialEq)]
pub struct Incoming {
    /// REQUEST, REPLY, COUNTER, DECLINECOUNTER, CANCEL.
    pub method: String,
    pub task: Task,
    pub comment: String,
    pub reminder: bool,
    /// Поручивший принял результат.
    pub result_accepted: bool,
    /// Когда отправлено (DTSTAMP); нет — 0.
    pub at: i64,
}

impl Incoming {
    /// Из ответа воркера (`{ method, data }`, как приглашение): первая задача VTODO; нет задачи — ошибка.
    pub fn from_worker(v: &Value) -> Result<Self, String> {
        let data = &v["data"];
        let raw = match data["entries"].as_array() {
            Some(e) => e.iter().find(|x| Task::is_task(x)).cloned(),
            None => Task::is_task(data).then(|| data.clone()),
        }
        .ok_or("в письме нет задачи")?;
        let task = Task::from_jmap(&raw)?;
        let method = match v["method"].as_str() {
            Some(m) if !m.is_empty() => m.to_ascii_uppercase(),
            _ => raw["method"].as_str().unwrap_or("").to_ascii_uppercase(),
        };
        let comment = prop(&task, "comment").unwrap_or_default();
        let reminder = prop(&task, X_REMINDER).is_some_and(|r| r.trim() == "1");
        let result_accepted = prop(&task, X_RESULT).is_some_and(|r| r.eq_ignore_ascii_case("accepted"));
        let at = raw["updated"].as_str().and_then(|u| u.parse::<jiff::Timestamp>().ok()).map_or(0, |t| t.as_second());
        let mut task = task;
        // Служебное письма — не свойства задачи.
        for p in ["comment", X_REMINDER] {
            set_prop(&mut task, p, None);
        }
        Ok(Self { method, task, comment, reminder, result_accepted, at })
    }

    /// Кто прислал: в ответе (REPLY, COUNTER) — исполнитель, иначе — поручивший.
    pub fn sender(&self) -> Option<&crate::TaskParticipant> {
        match self.method.as_str() {
            "REPLY" | "COUNTER" => self.task.participants.iter().find(|p| p.role != Role::Owner),
            _ => self.task.organizer(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TaskParticipant;
    use jiff::tz::TimeZone;

    fn person(key: &str, email: &str, role: Role, status: TaskPartstat) -> TaskParticipant {
        TaskParticipant { key: key.into(), name: format!("Имя {key}"), email: email.into(), role, status }
    }

    fn delegated() -> Task {
        let mut t = Task::new("d1@postman", "b", "Проверить отчёт");
        t.due = Some(Moment::new("2027-03-04T18:00:00".parse().unwrap(), Some("Europe/Moscow")));
        t.participants = vec![
            person("o", "boss@itt.test", Role::Owner, TaskPartstat::NeedsAction),
            person("p", "me@itt.test", Role::Assignee, TaskPartstat::NeedsAction),
            person("w", "obs@itt.test", Role::Observer, TaskPartstat::NeedsAction),
        ];
        t
    }

    /// Таблица переходов: каждая пара (статус, действие) — допустимо или ошибка.
    #[test]
    fn transition_table() {
        use Action as A;
        use Status as S;
        let ok = [
            (S::Waiting, A::Accept, S::Accepted),
            (S::Waiting, A::Decline, S::Declined),
            (S::Waiting, A::ProposeDue, S::Proposed),
            (S::InProgress, A::ProposeDue, S::Proposed),
            (S::Accepted, A::Start, S::InProgress),
            (S::Accepted, A::Finish, S::Done),
            (S::InProgress, A::Finish, S::Done),
            (S::Done, A::Recall, S::InProgress),
            (S::Declined, A::Reopen, S::Waiting),
            (S::Proposed, A::Reopen, S::Waiting),
            (S::Accepted, A::Remind, S::Accepted),
            (S::Done, A::ChangeDue, S::Done),
            (S::Proposed, A::AgreeDue, S::Accepted),
            (S::Proposed, A::KeepDue, S::Waiting),
            (S::Done, A::AcceptResult, S::Closed),
            (S::Done, A::Return, S::InProgress),
            (S::Declined, A::Revoke, S::Revoked),
            (S::Declined, A::Reassign, S::Waiting),
        ];
        for (from, a, to) in ok {
            assert_eq!(next(from, a), Ok(to), "{from:?} + {a:?}");
        }
        let bad = [
            (S::Waiting, A::Start),
            (S::Waiting, A::Finish),
            (S::Waiting, A::AcceptResult),
            (S::Accepted, A::Accept),
            (S::Done, A::Remind),
            (S::Declined, A::Remind),
            (S::Declined, A::ChangeDue),
            (S::InProgress, A::AgreeDue),
            (S::Closed, A::Revoke),
            (S::Revoked, A::Accept),
            (S::Closed, A::Return),
            (S::Accepted, A::Reassign),
        ];
        for (from, a) in bad {
            assert_eq!(next(from, a), Err(Invalid { from, action: a }), "{from:?} + {a:?}");
        }
        // Финальные статусы — без действий у обеих сторон.
        for s in [S::Closed, S::Revoked] {
            assert!(actions(s, true).is_empty() && actions(s, false).is_empty());
        }
        assert_eq!(actions(S::Done, true), [A::AcceptResult, A::Return, A::ChangeDue, A::Revoke]);
        assert_eq!(actions(S::Waiting, false), [A::Accept, A::ProposeDue, A::Decline]);
        assert!(A::Return.by_owner() && !A::Recall.by_owner());
    }

    #[test]
    fn status_from_task() {
        let mut t = delegated();
        assert_eq!(status(&t), Some(Status::Waiting));
        assert!(is_delegation(&t));
        for (ps, want) in [
            (TaskPartstat::Accepted, Status::Accepted),
            (TaskPartstat::InProcess, Status::InProgress),
            (TaskPartstat::Completed, Status::Done),
            (TaskPartstat::Declined, Status::Declined),
        ] {
            set_partstat(&mut t, "ME@itt.test", ps);
            assert_eq!(status(&t), Some(want));
        }
        set_partstat(&mut t, "me@itt.test", TaskPartstat::InProcess);
        set_proposed(&mut t, Some(&Moment::new("2027-03-09T18:00:00".parse().unwrap(), None)));
        assert_eq!(status(&t), Some(Status::Proposed));
        assert_eq!(proposed(&t).unwrap().tz.as_deref(), Some("Europe/Moscow"), "пояс — у срока задачи");
        set_proposed(&mut t, None);
        set_prop(&mut t, X_RESULT, Some("accepted"));
        assert_eq!(status(&t), Some(Status::Closed));
        t.progress = Progress::Cancelled;
        assert_eq!(status(&t), Some(Status::Revoked));
        assert_eq!(status(&Task::new("x", "", "своя")), None);
    }

    #[test]
    fn steps_are_stored_in_extra_and_deduplicated() {
        let mut t = delegated();
        let s = Step { at: 100, who: "me@itt.test".into(), kind: StepKind::Declined, comment: "Нет доступа".into(), due: None };
        assert!(push_step(&mut t, Step { at: 50, kind: StepKind::Assigned, who: "boss@itt.test".into(), comment: String::new(), due: None }));
        assert!(push_step(&mut t, s.clone()));
        assert!(!push_step(&mut t, s.clone()), "то же письмо второй раз");
        let list = steps(&t);
        assert_eq!(list.len(), 2);
        assert_eq!(list[1], s);
        // Шаги переживают JMAP туда и обратно (свои X-свойства — в `iCalendar.properties`).
        let back = Task::from_jmap(&t.to_jmap()).unwrap();
        assert_eq!(steps(&back), list);
        assert_eq!(next_reminder(&t), None);
        push_step(&mut t, Step { at: 1000, who: "boss@itt.test".into(), kind: StepKind::Reminded, comment: String::new(), due: None });
        assert_eq!(next_reminder(&t), Some(1000 + REMIND_EVERY));
    }

    fn parse(ics: &str) -> Incoming {
        let v = postman_worker::parse_calendar(ics.as_bytes()).unwrap();
        Incoming::from_worker(&v).unwrap_or_else(|e| panic!("{e}: {v}\n{ics}"))
    }

    /// Письма Postman разбираются тем же воркером, что чужие: метод, задача, комментарий, отметки.
    #[test]
    fn itip_messages_round_trip_through_worker() {
        let tz = TimeZone::get("Europe/Moscow").unwrap();
        let now = 1_792_000_000;
        let mut t = delegated();
        set_prop(&mut t, X_RESULT, Some("accepted"));
        let req = request(&t, "Добавь оценку риска, пожалуйста", true, now, &tz);
        assert!(req.contains("METHOD:REQUEST\r\n") && req.contains("ATTENDEE;ROLE=NON-PARTICIPANT"), "{req}");
        assert!(req.lines().all(|l| l.len() <= 75));
        let r = parse(&req);
        assert_eq!(r.method, "REQUEST");
        assert_eq!((r.comment.as_str(), r.reminder, r.result_accepted), ("Добавь оценку риска, пожалуйста", true, true));
        assert_eq!(r.sender().map(|s| s.email.as_str()), Some("boss@itt.test"));
        assert_eq!(r.at, now);
        assert_eq!(r.task.uid, "d1@postman");
        assert!(prop(&r.task, X_REMINDER).is_none() && prop(&r.task, "comment").is_none(), "служебное — не в задаче");

        let mut mine = delegated();
        mine.percent = Some(40);
        let rep = reply(&mine, ("Я", "me@itt.test"), TaskPartstat::InProcess, "Начал, жду доступ", now);
        assert!(rep.contains("ATTENDEE;PARTSTAT=IN-PROCESS;CN=\"Я\":mailto:me@itt.test"), "{rep}");
        let r = parse(&rep);
        assert_eq!(r.method, "REPLY");
        assert_eq!(r.comment, "Начал, жду доступ");
        let who = r.sender().unwrap();
        assert_eq!((who.email.as_str(), who.status), ("me@itt.test", TaskPartstat::InProcess));
        assert_eq!(r.task.percent, Some(40));

        let due = Moment::new("2027-03-09T18:00:00".parse().unwrap(), Some("Europe/Moscow"));
        let cnt = counter(&mine, ("Я", "me@itt.test"), &due, "Раньше не соберу", now, &tz);
        let r = parse(&cnt);
        assert_eq!(r.method, "COUNTER");
        assert_eq!(r.task.due_at(&tz), due.timestamp(&tz));
        assert_eq!(r.sender().map(|s| s.email.as_str()), Some("me@itt.test"));
        assert_eq!(r.comment, "Раньше не соберу");

        let dc = decline_counter(&delegated(), "", now);
        assert_eq!(parse(&dc).method, "DECLINECOUNTER");
        let c = parse(&cancel(&delegated(), now, &tz));
        assert_eq!((c.method.as_str(), c.task.progress), ("CANCEL", Progress::Cancelled));
        assert!(Incoming::from_worker(&json!({ "method": "REQUEST", "data": { "@type": "Event" } })).is_err());
    }

    /// Фаззинг входа: письма iTIP Postman с испорченными байтами (обрезка, замена, повтор, вырезка) — разбор
    /// воркером и `Incoming::from_worker` не падают (ошибка — `Err`). Детерминированно: свой xorshift.
    #[test]
    fn mutated_itip_does_not_panic() {
        let tz = TimeZone::get("Europe/Moscow").unwrap();
        let t = delegated();
        let seeds = [
            request(&t, "Комментарий; с, разделителями\\", true, 1, &tz),
            reply(&t, ("Я", "me@itt.test"), TaskPartstat::Completed, "Готово", 1),
            counter(&t, ("Я", "me@itt.test"), &Moment::new("2027-03-09T18:00:00".parse().unwrap(), None), "", 1, &tz),
            cancel(&t, 1, &tz),
        ];
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut rnd = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for seed in &seeds {
            for _ in 0..300 {
                let mut b = seed.as_bytes().to_vec();
                match rnd() % 4 {
                    0 => b.truncate((rnd() as usize) % b.len().max(1)),
                    1 => {
                        for _ in 0..1 + rnd() % 8 {
                            let i = (rnd() as usize) % b.len();
                            b[i] = (rnd() & 0xFF) as u8;
                        }
                    }
                    2 => {
                        let i = (rnd() as usize) % b.len();
                        let j = (i + (rnd() as usize) % 200).min(b.len());
                        let chunk = b[i..j].to_vec();
                        b.splice(i..i, chunk);
                    }
                    _ => {
                        let i = (rnd() as usize) % b.len();
                        let j = (i + (rnd() as usize) % 40).min(b.len());
                        b.drain(i..j);
                    }
                }
                if let Ok(v) = postman_worker::parse_calendar(&b) {
                    let _ = Incoming::from_worker(&v).map(|i| (status(&i.task), i.sender().is_some()));
                }
            }
        }
    }

    /// Корпус: поручение Thunderbird (VTODO REQUEST с ORGANIZER и ATTENDEE) и экспорт задачи Outlook — разбор не
    /// падает, у Outlook (без участников) — не поручение.
    #[test]
    fn corpus_requests() {
        let path = |n: &str| format!("{}/../worker/tests/fixtures/{n}", env!("CARGO_MANIFEST_DIR"));
        let tb = std::fs::read_to_string(path("todo-thunderbird.ics")).unwrap();
        let tb = tb
            .replacen("BEGIN:VCALENDAR\r\n", "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\n", 1)
            .replace("BEGIN:VCALENDAR\n", "BEGIN:VCALENDAR\nMETHOD:REQUEST\n");
        let r = parse(&tb);
        assert_eq!(r.method, "REQUEST");
        let ol = parse(&std::fs::read_to_string(path("todo-outlook.ics")).unwrap());
        assert!(!is_delegation(&ol.task));
    }

    /// Слово письма поручения: одно на UID (регистр не важен), годится в ключевое слово JMAP.
    #[test]
    fn keyword_is_stable_and_valid() {
        let k = keyword("Demo-Deleg-1@Postman");
        assert_eq!(k, keyword(" demo-deleg-1@postman "));
        assert_ne!(k, keyword("demo-deleg-2@postman"));
        assert!(k.starts_with("pm.assign.") && k.len() == "pm.assign.".len() + 16);
        assert!(k.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.'));
    }
}
