//! Задачи (S6-02): списки и проекты над моделью `postman_calendar::Task`, хранилище на сервере, кэш и синхронизация.
//!
//! Хранилище — календарь сервера: JMAP `CalendarEvent` с `@type: Task` (решение владельца 2026-10-22, S6-01,
//! `docs/STALWART.md` п. 37–40) — трейт [`TaskStore`]; реализации — у плагинов путей (`postman-backend-jmap`:
//! `CalendarEvent`, `postman-backend-dav`: VTODO, ADR 0004). Кэш — таблица задач (`postman_cache`):
//! список строится из него сразу, с сервера — дельта `CalendarEvent/changes` (push календаря — тот же тип). Правка
//! пишется в кэш сразу с пометкой «не отправлена» и уходит на сервер; без связи остаётся помеченной и уходит
//! следующей отправкой ([`Request::Flush`] — по возвращении связи), и после перезапуска тоже.

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::model::{Error, ErrorKind, Result};
use jiff::civil::Date;
use jiff::tz::TimeZone;
use postman_cache::{Cache, TaskRow};
use postman_calendar::Task;
use serde_json::{Map, Value, json};

use crate::labels::{Categories, Category};
pub use crate::taskparse::{Chip, ChipKind, Hours, Lang, Parsed, parse};

/// Ключ состояния `CalendarEvent` задач в кэше (своё, не календаря недели).
pub const STATE_KEY: &str = "tasks/CalendarEvent";

// ---------------------------------------------------------------------------------------------------------------
// Списки, проекты, подзадачи — без I/O.

/// Списки модуля «Задачи» (DESIGN.md «Задачи (Ctrl+4)»).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum List {
    /// Без срока и проекта.
    Inbox,
    /// Срок сегодня и раньше: просроченные сверху.
    Today,
    /// Срок после сегодня.
    Upcoming,
    /// Со ссылкой на письмо (S6-03 добавит письма со сроком — представление над `keywords`).
    FromMail,
    /// «Когда-нибудь».
    Someday,
    /// Выполненные (и отменённые): последние сверху.
    Done,
}

/// Задача в списке `list`: подзадачи в списках не показываются (они — в карточке родителя).
pub fn in_list(t: &Task, list: List, today: Date, tz: &TimeZone) -> bool {
    if t.parent.is_some() {
        return false;
    }
    let closed = t.progress.is_closed();
    let due_day = t.due.as_ref().and_then(|d| d.date_in(tz));
    match list {
        List::Done => closed,
        _ if closed => false,
        List::Someday => t.someday,
        _ if t.someday => false,
        List::Inbox => due_day.is_none() && t.project.is_none(),
        List::Today => due_day.is_some_and(|d| d <= today),
        List::Upcoming => due_day.is_some_and(|d| d > today),
        List::FromMail => t.mail.is_some(),
    }
}

/// Задачи списка по порядку: по сроку (просроченные — первыми), без срока — по названию; «Выполненные» — по дате
/// выполнения, последние сверху.
pub fn list<'a>(tasks: &'a [Task], which: List, now: i64, tz: &TimeZone) -> Vec<&'a Task> {
    let today = day_of(now, tz);
    let mut out: Vec<&Task> = tasks.iter().filter(|t| in_list(t, which, today, tz)).collect();
    if which == List::Done {
        out.sort_by(|a, b| b.completed_at.cmp(&a.completed_at).then_with(|| a.title.cmp(&b.title)));
    } else {
        out.sort_by(|a, b| {
            let (da, db) = (a.due_at(tz).unwrap_or(i64::MAX), b.due_at(tz).unwrap_or(i64::MAX));
            da.cmp(&db).then_with(|| b.important.cmp(&a.important)).then_with(|| a.title.cmp(&b.title))
        });
    }
    out
}

/// Сколько задач в списке (счётчики слева).
pub fn count(tasks: &[Task], which: List, now: i64, tz: &TimeZone) -> usize {
    let today = day_of(now, tz);
    tasks.iter().filter(|t| in_list(t, which, today, tz)).count()
}

/// Просрочена: срок прошёл, не закрыта.
pub fn is_overdue(t: &Task, now: i64, tz: &TimeZone) -> bool {
    !t.progress.is_closed() && t.due_at(tz).is_some_and(|d| d < now)
}

fn day_of(now: i64, tz: &TimeZone) -> Date {
    jiff::Timestamp::from_second(now).map(|t| t.to_zoned(tz.clone()).date()).unwrap_or_default()
}

/// Проект с прогрессом: выполнено из всего (задачи верхнего уровня).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub name: String,
    pub done: usize,
    pub total: usize,
}

/// Проекты по имени (без учёта регистра); пустые имена не считаются.
pub fn projects(tasks: &[Task]) -> Vec<Project> {
    let mut out: Vec<Project> = Vec::new();
    for t in tasks.iter().filter(|t| t.parent.is_none()) {
        let Some(name) = t.project.as_deref().map(str::trim).filter(|n| !n.is_empty()) else { continue };
        let done = usize::from(t.progress.is_closed());
        match out.iter_mut().find(|p| p.name.to_lowercase() == name.to_lowercase()) {
            Some(p) => {
                p.total += 1;
                p.done += done;
            }
            None => out.push(Project { name: name.to_owned(), done, total: 1 }),
        }
    }
    out.sort_by_key(|p| p.name.to_lowercase());
    out
}

/// Подзадачи задачи `uid` по порядку (`X-POSTMAN-ORDER`, без него — в конце по названию).
pub fn subtasks<'a>(tasks: &'a [Task], uid: &str) -> Vec<&'a Task> {
    let mut out: Vec<&Task> = tasks.iter().filter(|t| t.parent.as_deref() == Some(uid)).collect();
    out.sort_by(|a, b| a.order.unwrap_or(i64::MAX).cmp(&b.order.unwrap_or(i64::MAX)).then_with(|| a.title.cmp(&b.title)));
    out
}

/// Подзадачи: выполнено из всего — счётчик в строке задачи. Выполнение всех не выполняет саму задачу.
pub fn subtask_progress(tasks: &[Task], uid: &str) -> (usize, usize) {
    let subs = subtasks(tasks, uid);
    (subs.iter().filter(|t| t.progress.is_closed()).count(), subs.len())
}

/// Категория задачи — та же таблица, что у почты (`keywords`: `pm.cat.<id>` или имя категории).
pub fn category<'a>(t: &Task, table: &'a Categories) -> Option<&'a Category> {
    table.for_event(&t.keywords)
}

/// UID новой задачи: время и счётчик процесса — уникален в пределах ящика.
pub fn new_uid(now_nanos: u128) -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{now_nanos:x}-{n:x}@postman")
}

// ---------------------------------------------------------------------------------------------------------------
// Хранилище.

/// Изменения задач на сервере с состояния `since`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Delta {
    /// Новые и изменённые задачи (JSCalendar); события календаря сюда не попадают.
    pub changed: Vec<Value>,
    /// id удалённых записей календаря (задач и событий — кэш знает только свои).
    pub destroyed: Vec<String>,
    pub state: String,
}

/// Где живут задачи. Ошибки — нейтральные (`model::Error`): сеть отличается от отказа сервера, «посчитать
/// изменения нельзя» — код `cannotCalculateChanges`.
pub trait TaskStore {
    /// Все задачи и состояние, с которого считать изменения.
    fn load(&self) -> impl Future<Output = Result<(Vec<Value>, String)>>;
    /// Изменения с `since`; сервер не может их посчитать — ошибка `cannotCalculateChanges` (тогда — [`load`]).
    ///
    /// [`load`]: TaskStore::load
    fn changes(&self, since: &str) -> impl Future<Output = Result<Delta>>;
    /// Записи по id (удалённые — не в ответе).
    fn get(&self, ids: &[String]) -> impl Future<Output = Result<Vec<Value>>>;
    /// Создать; ответ — id.
    fn create(&self, task: &Value) -> impl Future<Output = Result<String>>;
    fn update(&self, id: &str, patch: &Map<String, Value>) -> impl Future<Output = Result<()>>;
    /// Удалить; уже удалённая — не ошибка.
    fn destroy(&self, id: &str) -> impl Future<Output = Result<()>>;
    /// Календарь для новых задач.
    fn default_calendar(&self) -> impl Future<Output = Result<String>>;
}

// ---------------------------------------------------------------------------------------------------------------
// Запросы интерфейса и ответы.

#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// Задачи из кэша сразу, затем сверка с сервером → `Loaded`.
    Load,
    /// Сверка с сервером (push календаря) → `Changed`/`Removed`, если что-то изменилось.
    Refresh,
    /// Создать или изменить (новая — `id` пустой; календарь не задан — календарь по умолчанию) → `Changed`.
    Save(Box<Task>),
    /// Удалить по UID → `Removed`.
    Delete(String),
    /// Отправить правки, не дошедшие до сервера (связь вернулась).
    Flush,
    /// «Время в календаре» (S6-08): событие «занят» `[start, end)` в своём календаре со ссылкой на задачу
    /// (`relatedTo`), у задачи — `X-POSTMAN-BLOCK` → `Changed`.
    Block { uid: String, start: i64, end: i64 },
    /// Убрать блок задачи (событие удаляется) → `Changed`.
    Unblock { uid: String },
    /// Сверить задачи со ссылкой на письмо с письмами кэша (S6-03, `mailtask::reconcile`): срок у письма снят —
    /// задача выполнена, письмо удалено — без ссылки → `Changed`, затем отправка. Без сервера, если менять нечего.
    Reconcile,
}

impl Request {
    /// Что повторить, когда вернётся связь. Без связи цикл синхронизации команду не выполняет, а копит: правка
    /// повторяется целиком (запись в кэш повторно — та же). Чтобы правка была видна сразу и без связи, интерфейс
    /// пишет её в кэш сам ([`save_local`], [`delete_local`], как с письмами) и шлёт [`Request::Flush`].
    pub fn replayable(&self) -> Option<Request> {
        match self {
            Request::Save(_) | Request::Delete(_) | Request::Flush => Some(self.clone()),
            // Блок времени создаёт событие: повтор после обрыва мог бы поставить его дважды.
            Request::Load | Request::Refresh | Request::Reconcile | Request::Block { .. } | Request::Unblock { .. } => None,
        }
    }
}

/// Правка у себя: задача в кэше с пометкой «не отправлена» (уйдёт по [`Request::Flush`]); ответ — как её покажет
/// список.
/// Задача-письмо (`mailtask::is_letter`) не записывается: её срок — слова письма (`mailtask::letter_patch`).
pub fn save_local(cache: &Cache, task: Task) -> postman_cache::Result<Task> {
    if crate::mailtask::is_letter(&task) {
        return Ok(task);
    }
    let old = cache.task(&task.uid)?;
    // База правки — версия с сервера: у уже правленной без связи — прежняя база.
    let base = old.as_ref().and_then(|o| if o.dirty { o.base.clone() } else { o.server_id.as_ref().map(|_| o.json.clone()) });
    let row = TaskRow {
        uid: task.uid.clone(),
        server_id: old.and_then(|o| o.server_id).or_else(|| Some(task.id.clone()).filter(|i| !i.is_empty())),
        json: task.to_jmap().to_string(),
        base,
        dirty: true,
        deleted: false,
    };
    cache.put_tasks(std::slice::from_ref(&row))?;
    Ok(from_row(&row).unwrap_or(task))
}

/// Удаление у себя: из списка сразу, на сервере — по [`Request::Flush`].
pub fn delete_local(cache: &Cache, uid: &str) -> postman_cache::Result<()> {
    if let Some(mut row) = cache.task(uid)? {
        if row.server_id.is_none() {
            // На сервер не уходила — удалять там нечего.
            return cache.remove_tasks(&[uid.to_owned()]);
        }
        row.dirty = true;
        row.deleted = true;
        cache.put_tasks(&[row])?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    /// Все задачи (вход, полная загрузка).
    Loaded(Vec<Task>),
    /// Новые или изменённые (у себя или с сервера).
    Changed(Vec<Task>),
    /// Удалённые (UID).
    Removed(Vec<String>),
    /// Сервер отклонил правку задачи `uid` — у неё снова серверная версия (или её нет).
    Failed { uid: Option<String>, error: String },
}

/// Модель из строки кэша (битую — пропустить с записью в журнал).
pub fn from_row(row: &TaskRow) -> Option<Task> {
    let v: Value = serde_json::from_str(&row.json).ok()?;
    match Task::from_jmap(&v) {
        Ok(mut t) => {
            t.id = row.server_id.clone().unwrap_or_default();
            Some(t)
        }
        Err(e) => {
            tracing::warn!(uid = %row.uid, "task row is not a task: {e}");
            None
        }
    }
}

fn server_row(v: &Value) -> Option<TaskRow> {
    let uid = v["uid"].as_str()?.to_owned();
    Some(TaskRow { uid, server_id: v["id"].as_str().map(str::to_owned), json: v.to_string(), base: None, dirty: false, deleted: false })
}

fn cached(cache: &Cache) -> Vec<Task> {
    cache.tasks().unwrap_or_default().iter().filter_map(from_row).collect()
}

fn db(e: postman_cache::Error) -> Error {
    Error::storage(e)
}

fn is_network(e: &Error) -> bool {
    e.kind() == ErrorKind::Network
}

/// Выполнить запрос. Сеть недоступна — `Err` (цикл синхронизации уходит «без связи», правки остаются в кэше и
/// уйдут по [`Request::Flush`]); отказ сервера — [`Reply::Failed`].
pub async fn handle(store: &impl TaskStore, cache: &Cache, request: Request, notify: &impl Fn(Reply)) -> Result<()> {
    match request {
        Request::Load => {
            notify(Reply::Loaded(cached(cache)));
            if refresh(store, cache, notify).await? {
                notify(Reply::Loaded(cached(cache)));
            }
            reconcile(cache, notify).map_err(db)?;
            flush(store, cache, notify).await
        }
        Request::Refresh => {
            if refresh(store, cache, notify).await? {
                notify(Reply::Loaded(cached(cache)));
            }
            reconcile(cache, notify).map_err(db)?;
            flush(store, cache, notify).await
        }
        Request::Save(task) => {
            notify(Reply::Changed(vec![save_local(cache, *task).map_err(db)?]));
            flush(store, cache, notify).await
        }
        Request::Delete(uid) => {
            delete_local(cache, &uid).map_err(db)?;
            notify(Reply::Removed(vec![uid]));
            flush(store, cache, notify).await
        }
        Request::Flush => flush(store, cache, notify).await,
        Request::Block { uid, start, end } => {
            let Some(mut t) = cache.task(&uid).map_err(db)?.as_ref().and_then(from_row) else { return Ok(()) };
            let calendar = match t.calendar_ids.first() {
                Some(c) => c.clone(),
                None => store.default_calendar().await?,
            };
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
            let event = block_event(&t, &new_uid(nanos), &calendar, start, end);
            match store.create(&event).await {
                Ok(id) => {
                    t.block = Some(postman_calendar::TimeBlock { event_id: id, start, end });
                    notify(Reply::Changed(vec![save_local(cache, t).map_err(db)?]));
                    flush(store, cache, notify).await
                }
                Err(e) if is_network(&e) => Err(e),
                Err(e) => {
                    notify(Reply::Failed { uid: Some(uid), error: e.to_string() });
                    Ok(())
                }
            }
        }
        Request::Unblock { uid } => {
            let Some(mut t) = cache.task(&uid).map_err(db)?.as_ref().and_then(from_row) else { return Ok(()) };
            if let Some(b) = t.block.take() {
                store.destroy(&b.event_id).await?;
            }
            notify(Reply::Changed(vec![save_local(cache, t).map_err(db)?]));
            flush(store, cache, notify).await
        }
        Request::Reconcile => {
            if reconcile(cache, notify).map_err(db)? {
                flush(store, cache, notify).await?;
            }
            Ok(())
        }
    }
}

/// Событие блока времени задачи (S6-08): «занят», без участников, со ссылкой на задачу (`relatedTo` — задача как
/// родитель), метка `pm.task`.
pub fn block_event(task: &Task, uid: &str, calendar: &str, start: i64, end: i64) -> Value {
    let at = jiff::Timestamp::from_second(start).map(|t| t.strftime("%Y-%m-%dT%H:%M:%S").to_string()).unwrap_or_default();
    json!({
        "@type": "Event",
        "uid": uid,
        "calendarIds": { calendar: true },
        "title": task.title,
        "start": at,
        "timeZone": "Etc/UTC",
        "duration": postman_calendar::format_duration((end - start).max(60)),
        "freeBusyStatus": "busy",
        "keywords": { "pm.task": true },
        "relatedTo": { task.uid.clone(): { "@type": "Relation", "relation": { "parent": true } } },
    })
}

/// Задачи со ссылкой на письмо — по письмам кэша; `true` — что-то изменилось (записано у себя).
fn reconcile(cache: &Cache, notify: &impl Fn(Reply)) -> postman_cache::Result<bool> {
    let now = jiff::Timestamp::now().as_second();
    let fixed = crate::mailtask::reconcile_cache(cache, now, &TimeZone::system())?;
    if fixed.is_empty() {
        return Ok(false);
    }
    let mut shown = Vec::new();
    for t in fixed {
        shown.push(save_local(cache, t)?);
    }
    notify(Reply::Changed(shown));
    Ok(true)
}

/// Сверка с сервером: дельта (или полная загрузка, если дельты нет). Правки, ещё не отправленные, не затираются.
/// `true` — была полная загрузка (интерфейсу — весь список заново).
async fn refresh(store: &impl TaskStore, cache: &Cache, notify: &impl Fn(Reply)) -> Result<bool> {
    let since = cache.state(STATE_KEY).map_err(db)?;
    let delta = match since {
        Some(s) => match store.changes(&s).await {
            Ok(d) => Some(d),
            Err(Error::Method { kind, .. }) if kind == "cannotCalculateChanges" => None,
            Err(e) => return Err(e),
        },
        None => None,
    };
    let Some(delta) = delta else {
        let (all, state) = store.load().await?;
        let rows: Vec<TaskRow> = all.iter().filter_map(server_row).collect();
        cache.replace_tasks(&rows).map_err(db)?;
        cache.set_state(STATE_KEY, &state).map_err(db)?;
        return Ok(true);
    };
    let mut changed = Vec::new();
    for v in &delta.changed {
        let Some(row) = server_row(v) else { continue };
        if cache.task(&row.uid).map_err(db)?.is_some_and(|o| o.dirty) {
            continue;
        }
        changed.push(row);
    }
    cache.put_tasks(&changed).map_err(db)?;
    let mut removed = Vec::new();
    for id in &delta.destroyed {
        if let Some(uid) = cache.task_uid(id).map_err(db)? {
            removed.push(uid);
        }
    }
    cache.remove_tasks(&removed).map_err(db)?;
    cache.set_state(STATE_KEY, &delta.state).map_err(db)?;
    if !changed.is_empty() {
        notify(Reply::Changed(changed.iter().filter_map(from_row).collect()));
    }
    if !removed.is_empty() {
        notify(Reply::Removed(removed));
    }
    Ok(false)
}

/// Отправить правки из кэша по одной. Изменение — патчем «своя версия против базы» (только поля, изменённые у
/// себя; поля, которые тем временем правил другой клиент, остаются его); задачи на сервере уже нет — создаётся
/// заново.
async fn flush(store: &impl TaskStore, cache: &Cache, notify: &impl Fn(Reply)) -> Result<()> {
    for row in cache.dirty_tasks().map_err(db)? {
        match push(store, &row).await {
            Ok(Some(fresh)) => {
                cache.put_tasks(std::slice::from_ref(&fresh)).map_err(db)?;
                notify(Reply::Changed(from_row(&fresh).into_iter().collect()));
            }
            Ok(None) => cache.remove_tasks(std::slice::from_ref(&row.uid)).map_err(db)?,
            Err(e) if is_network(&e) => return Err(e),
            Err(e) => {
                tracing::warn!(uid = %row.uid, "task rejected by server: {e}");
                // Правка отклонена — вернуть серверную версию, иначе она повторялась бы вечно.
                let back = match &row.server_id {
                    Some(id) => store.get(std::slice::from_ref(id)).await?.first().and_then(server_row),
                    None => None,
                };
                match back {
                    Some(b) => {
                        cache.put_tasks(std::slice::from_ref(&b)).map_err(db)?;
                        notify(Reply::Changed(from_row(&b).into_iter().collect()));
                    }
                    None => {
                        cache.remove_tasks(std::slice::from_ref(&row.uid)).map_err(db)?;
                        notify(Reply::Removed(vec![row.uid.clone()]));
                    }
                }
                notify(Reply::Failed { uid: Some(row.uid.clone()), error: e.to_string() });
            }
        }
    }
    Ok(())
}

/// Одна правка на сервер: строка после записи (`None` — удалена).
async fn push(store: &impl TaskStore, row: &TaskRow) -> Result<Option<TaskRow>> {
    if row.deleted {
        if let Some(id) = &row.server_id {
            store.destroy(id).await?;
        }
        return Ok(None);
    }
    let local: Value = serde_json::from_str(&row.json).map_err(Error::json)?;
    let mut mine = Task::from_jmap(&local).map_err(Error::Protocol)?;
    if mine.calendar_ids.is_empty() {
        mine.calendar_ids = vec![store.default_calendar().await?];
    }
    let server = match &row.server_id {
        Some(id) => store.get(std::slice::from_ref(id)).await?.into_iter().next(),
        None => None,
    };
    let base = row.base.as_deref().and_then(|b| serde_json::from_str::<Value>(b).ok()).and_then(|b| Task::from_jmap(&b).ok());
    let id = match server.as_ref().and_then(|s| Task::from_jmap(s).ok().map(|t| (s, t))) {
        Some((s, theirs)) => {
            let id = s["id"].as_str().unwrap_or_default().to_owned();
            let patch = mine.patch(base.as_ref().unwrap_or(&theirs));
            if !patch.is_empty() {
                store.update(&id, &patch).await?;
            }
            id
        }
        None => store.create(&mine.to_jmap()).await?,
    };
    Ok(store.get(std::slice::from_ref(&id)).await?.first().and_then(server_row))
}

#[cfg(test)]
mod tests {
    use super::*;
    use postman_calendar::{MailLink, Moment, Progress};
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    fn tz() -> TimeZone {
        TimeZone::get("Europe/Moscow").unwrap()
    }

    /// 2026-10-21 14:05 по Москве.
    fn now() -> i64 {
        jiff::civil::date(2026, 10, 21).at(14, 5, 0, 0).to_zoned(tz()).unwrap().timestamp().as_second()
    }

    fn task(uid: &str, due: Option<&str>) -> Task {
        let mut t = Task::new(uid, "b", uid);
        t.due = due.map(|d| Moment::new(d.parse().unwrap(), Some("Europe/Moscow")));
        t
    }

    fn uids(list: Vec<&Task>) -> Vec<&str> {
        list.into_iter().map(|t| t.uid.as_str()).collect()
    }

    #[test]
    fn lists_by_due_project_mail_someday_done() {
        let mut ts = vec![
            task("inbox", None),
            task("overdue", Some("2026-10-19T10:00:00")),
            task("today-late", Some("2026-10-21T18:00:00")),
            task("today-early", Some("2026-10-21T09:00:00")),
            task("tomorrow", Some("2026-10-22T10:00:00")),
            task("project", None),
            task("someday", None),
            task("mail", Some("2026-10-25T10:00:00")),
            task("done", Some("2026-10-20T10:00:00")),
            task("sub", Some("2026-10-21T10:00:00")),
        ];
        ts[5].project = Some("Вектор".into());
        ts[6].someday = true;
        ts[7].mail = Some(MailLink { message_id: "m@x".into(), email_id: None });
        ts[8].set_done(true, now() - 60);
        ts[9].parent = Some("project".into());
        let tz = tz();
        assert_eq!(uids(list(&ts, List::Inbox, now(), &tz)), ["inbox"]);
        // Просроченные сверху, затем по времени срока; подзадачи — не в списках.
        assert_eq!(uids(list(&ts, List::Today, now(), &tz)), ["overdue", "today-early", "today-late"]);
        assert_eq!(uids(list(&ts, List::Upcoming, now(), &tz)), ["tomorrow", "mail"]);
        assert_eq!(uids(list(&ts, List::FromMail, now(), &tz)), ["mail"]);
        assert_eq!(uids(list(&ts, List::Someday, now(), &tz)), ["someday"]);
        assert_eq!(uids(list(&ts, List::Done, now(), &tz)), ["done"]);
        assert_eq!(count(&ts, List::Today, now(), &tz), 3);
        assert!(is_overdue(&ts[1], now(), &tz) && !is_overdue(&ts[2], now(), &tz) && !is_overdue(&ts[8], now(), &tz));
    }

    /// Срок 00:30 по Москве 22-го для того, кто в UTC, — ещё 21-е: «Сегодня» считается в поясе смотрящего.
    #[test]
    fn today_is_in_viewer_zone() {
        let t = task("late", Some("2026-10-22T00:30:00"));
        let utc_now = jiff::civil::date(2026, 10, 21).at(12, 0, 0, 0).to_zoned(TimeZone::UTC).unwrap().timestamp().as_second();
        assert!(in_list(&t, List::Today, day_of(utc_now, &TimeZone::UTC), &TimeZone::UTC));
        assert!(in_list(&t, List::Upcoming, day_of(utc_now, &tz()), &tz()));
    }

    #[test]
    fn projects_and_subtasks_progress() {
        let mut ts = vec![task("a", None), task("b", None), task("c", None), task("s1", None), task("s2", None), task("s3", None)];
        ts[0].project = Some("Вектор".into());
        ts[1].project = Some("вектор".into());
        ts[1].set_done(true, 1);
        ts[2].project = Some("Альфа".into());
        for (i, (sub, order)) in [(3, Some(2)), (4, Some(1)), (5, None)].into_iter().enumerate() {
            ts[sub].parent = Some("a".into());
            ts[sub].order = order;
            ts[sub].project = Some("Вектор".into());
            if i == 0 {
                ts[sub].set_done(true, 1);
            }
        }
        assert_eq!(projects(&ts), [Project { name: "Альфа".into(), done: 0, total: 1 }, Project { name: "Вектор".into(), done: 1, total: 2 }]);
        assert_eq!(uids(subtasks(&ts, "a")), ["s2", "s1", "s3"]);
        assert_eq!(subtask_progress(&ts, "a"), (1, 3));
        // Все подзадачи выполнены — сама задача нет.
        for t in &mut ts[3..6] {
            t.set_done(true, 1);
        }
        assert_eq!(subtask_progress(&ts, "a"), (3, 3));
        assert_eq!(ts[0].progress, Progress::NeedsAction);
    }

    #[test]
    fn category_is_from_mail_table() {
        let table = Categories::defaults(|id| format!("Имя {id}"));
        let mut t = task("x", None);
        t.keywords = vec!["pm.cat.clients".into()];
        assert_eq!(category(&t, &table).map(|c| c.keyword.as_str()), Some("pm.cat.clients"));
        t.keywords = vec!["имя TEAM".into()];
        assert_eq!(category(&t, &table).map(|c| c.keyword.as_str()), Some("pm.cat.team"));
        assert_ne!(new_uid(1), new_uid(1));
    }

    // --- Хранилище в памяти: сервер для тестов синхронизации.

    #[derive(Default)]
    struct Mem {
        items: RefCell<BTreeMap<String, Value>>,
        log: RefCell<Vec<String>>,
        state: RefCell<u64>,
        destroyed: RefCell<Vec<(u64, String)>>,
        changed: RefCell<Vec<(u64, String)>>,
        offline: RefCell<bool>,
        next: RefCell<u64>,
    }

    impl Mem {
        fn net(&self) -> Result<()> {
            if *self.offline.borrow() {
                // Ошибка сети, как у пути к серверу без соединения.
                return Err(Error::network("HTTP: connection refused"));
            }
            Ok(())
        }

        fn bump(&self, id: &str, destroyed: bool) {
            let mut s = self.state.borrow_mut();
            *s += 1;
            let list = if destroyed { &self.destroyed } else { &self.changed };
            list.borrow_mut().push((*s, id.to_owned()));
        }

        /// Другой клиент правит задачу на сервере.
        fn edit(&self, id: &str, f: impl FnOnce(&mut Value)) {
            f(self.items.borrow_mut().get_mut(id).unwrap());
            self.bump(id, false);
        }
    }

    impl TaskStore for Mem {
        async fn load(&self) -> Result<(Vec<Value>, String)> {
            self.net()?;
            self.log.borrow_mut().push("load".into());
            Ok((self.items.borrow().values().cloned().collect(), self.state.borrow().to_string()))
        }

        async fn changes(&self, since: &str) -> Result<Delta> {
            self.net()?;
            let Ok(since) = since.parse::<u64>() else {
                return Err(Error::Method {
                    method: "CalendarEvent/changes".into(),
                    kind: "cannotCalculateChanges".into(),
                    description: String::new(),
                });
            };
            let after = |l: &RefCell<Vec<(u64, String)>>| l.borrow().iter().filter(|(s, _)| *s > since).map(|(_, i)| i.clone()).collect::<Vec<_>>();
            let ids = after(&self.changed);
            let changed = ids.iter().filter_map(|i| self.items.borrow().get(i).cloned()).collect();
            Ok(Delta { changed, destroyed: after(&self.destroyed), state: self.state.borrow().to_string() })
        }

        async fn get(&self, ids: &[String]) -> Result<Vec<Value>> {
            self.net()?;
            Ok(ids.iter().filter_map(|i| self.items.borrow().get(i).cloned()).collect())
        }

        async fn create(&self, task: &Value) -> Result<String> {
            self.net()?;
            if task["title"] == "reject" {
                return Err(Error::Method { method: "CalendarEvent/set".into(), kind: "invalidProperties".into(), description: String::new() });
            }
            let id = {
                let mut n = self.next.borrow_mut();
                *n += 1;
                format!("t{n}")
            };
            let mut v = task.clone();
            v["id"] = json!(id);
            self.items.borrow_mut().insert(id.clone(), v);
            self.log.borrow_mut().push(format!("create {id}"));
            self.bump(&id, false);
            Ok(id)
        }

        async fn update(&self, id: &str, patch: &Map<String, Value>) -> Result<()> {
            self.net()?;
            let mut keys: Vec<&String> = patch.keys().collect();
            keys.sort();
            self.log.borrow_mut().push(format!("update {id} {keys:?}"));
            {
                let mut items = self.items.borrow_mut();
                let v = items.get_mut(id).unwrap();
                for (k, x) in patch {
                    if k == "iCalendar/properties" {
                        v["iCalendar"]["properties"] = x.clone();
                    } else if x.is_null() {
                        v.as_object_mut().unwrap().remove(k);
                    } else {
                        v[k] = x.clone();
                    }
                }
            }
            self.bump(id, false);
            Ok(())
        }

        async fn destroy(&self, id: &str) -> Result<()> {
            self.net()?;
            self.log.borrow_mut().push(format!("destroy {id}"));
            self.items.borrow_mut().remove(id);
            self.bump(id, true);
            Ok(())
        }

        async fn default_calendar(&self) -> Result<String> {
            self.net()?;
            Ok("b".into())
        }
    }

    fn run<T>(f: impl Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
    }

    /// Создать → второй клиент видит по дельте → выполнить → удалить; кэш второго клиента — как сервер.
    #[test]
    fn round_trip_through_store_and_two_caches() {
        let server = Mem::default();
        let (a, b) = (Cache::open_in_memory().unwrap(), Cache::open_in_memory().unwrap());
        let log: RefCell<Vec<Reply>> = RefCell::new(Vec::new());
        let note = |r| log.borrow_mut().push(r);
        run(async {
            handle(&server, &a, Request::Load, &note).await.unwrap();
            handle(&server, &b, Request::Load, &note).await.unwrap();
            let mut t = Task::new("u1", "", "Ресурсный план");
            t.project = Some("IV кв.".into());
            handle(&server, &a, Request::Save(Box::new(t)), &note).await.unwrap();
            let saved = cached(&a);
            assert_eq!(saved.len(), 1);
            assert_eq!(saved[0].id, "t1");
            assert_eq!(saved[0].calendar_ids, ["b"], "календарь по умолчанию");
            assert!(a.dirty_tasks().unwrap().is_empty());

            handle(&server, &b, Request::Refresh, &note).await.unwrap();
            let seen = cached(&b);
            assert_eq!((seen[0].title.as_str(), seen[0].project.as_deref()), ("Ресурсный план", Some("IV кв.")));

            let mut done = seen[0].clone();
            done.set_done(true, now());
            handle(&server, &b, Request::Save(Box::new(done)), &note).await.unwrap();
            handle(&server, &a, Request::Refresh, &note).await.unwrap();
            assert_eq!(cached(&a)[0].progress, Progress::Completed);
            assert_eq!(cached(&a)[0].completed_at, Some(now()));

            handle(&server, &a, Request::Delete("u1".into()), &note).await.unwrap();
            handle(&server, &b, Request::Refresh, &note).await.unwrap();
            assert!(cached(&b).is_empty() && cached(&a).is_empty());
        });
        let log = server.log.borrow();
        assert_eq!(log[2], "create t1");
        assert!(log.iter().any(|l| l.starts_with("update t1") && l.contains("progress")), "{log:?}");
        assert!(log.contains(&"destroy t1".to_string()));
    }

    /// Без связи: правка — в кэше сразу и помечена; сеть вернулась — `Flush` отправляет патч к серверной версии,
    /// поле, которое тем временем поменял другой клиент, остаётся его.
    #[test]
    fn offline_edit_is_kept_and_merged_on_flush() {
        let server = Mem::default();
        let c = Cache::open_in_memory().unwrap();
        let log: RefCell<Vec<Reply>> = RefCell::new(Vec::new());
        let note = |r| log.borrow_mut().push(r);
        run(async {
            handle(&server, &c, Request::Load, &note).await.unwrap();
            handle(&server, &c, Request::Save(Box::new(Task::new("u1", "b", "Смета"))), &note).await.unwrap();
            *server.offline.borrow_mut() = true;
            let mut t = cached(&c).remove(0);
            t.important = true;
            let err = handle(&server, &c, Request::Save(Box::new(t)), &note).await.unwrap_err();
            assert!(is_network(&err));
            assert!(cached(&c)[0].important, "правка видна сразу");
            assert_eq!(c.dirty_tasks().unwrap().len(), 1);
            let save = Request::Save(Box::new(Task::new("x", "", "x")));
            assert_eq!(save.replayable(), Some(save.clone()));
            assert_eq!(Request::Refresh.replayable(), None);
            // Тем временем другой клиент переименовал.
            server.edit("t1", |v| v["title"] = json!("Смета v2"));
            // Без связи и новая задача — с UID, без id.
            handle(&server, &c, Request::Save(Box::new(Task::new("u2", "", "Новая"))), &note).await.unwrap_err();
            *server.offline.borrow_mut() = false;
            handle(&server, &c, Request::Flush, &note).await.unwrap();
            handle(&server, &c, Request::Refresh, &note).await.unwrap();
        });
        let all = cached(&c);
        let t1 = all.iter().find(|t| t.uid == "u1").unwrap();
        assert!(t1.important, "своя правка дошла");
        assert_eq!(t1.title, "Смета v2", "чужая правка не затёрта");
        assert!(all.iter().any(|t| t.uid == "u2" && !t.id.is_empty()), "новая без связи создана");
        assert!(c.dirty_tasks().unwrap().is_empty());
        let log = server.log.borrow();
        assert!(log.iter().any(|l| l == "update t1 [\"priority\"]"), "{log:?}");
    }

    /// Правка интерфейсом у себя (как с письмами): видна сразу, уходит по `Flush`; удаление неотправленной — без
    /// сервера.
    #[test]
    fn local_edits_then_flush() {
        let server = Mem::default();
        let c = Cache::open_in_memory().unwrap();
        let quiet = |_: Reply| {};
        let shown = save_local(&c, Task::new("u1", "", "Своя")).unwrap();
        assert_eq!(shown.title, "Своя");
        save_local(&c, Task::new("u2", "", "Передумал")).unwrap();
        delete_local(&c, "u2").unwrap();
        assert!(c.task("u2").unwrap().is_none(), "неотправленная удалена сразу");
        run(async { handle(&server, &c, Request::Flush, &quiet).await.unwrap() });
        let all = cached(&c);
        assert_eq!((all.len(), all[0].id.as_str(), all[0].calendar_ids.as_slice()), (1, "t1", &["b".to_string()][..]));
        delete_local(&c, "u1").unwrap();
        assert!(cached(&c).is_empty());
        run(async { handle(&server, &c, Request::Flush, &quiet).await.unwrap() });
        assert!(server.items.borrow().is_empty() && c.all_tasks().unwrap().is_empty());
    }

    /// Блок времени: событие «занят» со ссылкой на задачу, у задачи — блок; «Убрать» — событие удалено.
    #[test]
    fn time_block_creates_and_removes_event() {
        let server = Mem::default();
        let c = Cache::open_in_memory().unwrap();
        let quiet = |_: Reply| {};
        run(async {
            handle(&server, &c, Request::Save(Box::new(Task::new("u1", "b", "Отчёт"))), &quiet).await.unwrap();
            handle(&server, &c, Request::Block { uid: "u1".into(), start: 1_800_000_000, end: 1_800_003_600 }, &quiet).await.unwrap();
        });
        let t = cached(&c).into_iter().find(|t| t.uid == "u1").unwrap();
        let b = t.block.clone().expect("блок у задачи");
        let ev = server.items.borrow().get(&b.event_id).cloned().expect("событие на сервере");
        assert_eq!((ev["@type"].as_str(), ev["duration"].as_str()), (Some("Event"), Some("PT1H")));
        assert_eq!(ev["relatedTo"]["u1"]["relation"]["parent"], true);
        assert_eq!(ev["start"], "2027-01-15T08:00:00");
        run(async { handle(&server, &c, Request::Unblock { uid: "u1".into() }, &quiet).await.unwrap() });
        assert!(server.items.borrow().get(&b.event_id).is_none());
        assert!(cached(&c)[0].block.is_none());
    }

    /// Отказ сервера: правка не повторяется вечно — серверная версия обратно и `Failed`.
    #[test]
    fn rejected_edit_is_dropped() {
        let server = Mem::default();
        let c = Cache::open_in_memory().unwrap();
        let log: RefCell<Vec<Reply>> = RefCell::new(Vec::new());
        let note = |r| log.borrow_mut().push(r);
        run(async {
            handle(&server, &c, Request::Save(Box::new(Task::new("bad", "b", "reject"))), &note).await.unwrap();
        });
        assert!(cached(&c).is_empty() && c.dirty_tasks().unwrap().is_empty());
        let log = log.borrow();
        assert!(matches!(log.last(), Some(Reply::Failed { uid: Some(u), .. }) if u == "bad"), "{log:?}");
        assert!(log.contains(&Reply::Removed(vec!["bad".into()])));
    }

    /// Дельта с сервера: изменения и удаления, событие календаря задачей не становится; без дельты — полная
    /// загрузка, правки без связи — поверх.
    #[test]
    fn refresh_applies_delta_and_full_reload() {
        let server = Mem::default();
        let c = Cache::open_in_memory().unwrap();
        let log: RefCell<Vec<Reply>> = RefCell::new(Vec::new());
        let note = |r| log.borrow_mut().push(r);
        run(async {
            handle(&server, &c, Request::Load, &note).await.unwrap();
            for uid in ["u1", "u2"] {
                handle(&server, &c, Request::Save(Box::new(Task::new(uid, "b", uid))), &note).await.unwrap();
            }
            server.destroy("t2").await.unwrap();
            server.edit("t1", |v| v["title"] = json!("u1 renamed"));
            handle(&server, &c, Request::Refresh, &note).await.unwrap();
            let all = cached(&c);
            assert_eq!(all.len(), 1);
            assert_eq!(all[0].title, "u1 renamed");
            // Состояние потеряно (сервер не считает дельту) — полная загрузка.
            c.set_state(STATE_KEY, "").unwrap();
            c.remove_tasks(&["u1".into()]).unwrap();
            handle(&server, &c, Request::Refresh, &note).await.unwrap();
        });
        assert_eq!(cached(&c).len(), 1);
        assert!(log.borrow().iter().any(|r| matches!(r, Reply::Removed(u) if u == &["u2".to_string()])));
    }
}
