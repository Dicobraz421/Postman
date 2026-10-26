//! Путь CalDAV и CardDAV — плагин областей ядра (ADR 0004, S7-02б): календарь, задачи и контакты учётной записи
//! «стандарты» (ADR 0003, S7-05б) рядом с IMAP — в её синхронизации (`postman-backend-imap` подключает области по
//! адресу `?dav=https://…`, `postman_core::backend::Areas`), с тем же договором команд и событий, что у JMAP.
//! Интерфейс видит те же экраны календаря, задач и контактов: что есть у сервера — по `Features` (S7-03а).
//!
//! - **Неделя** — `calendar-query` с `expand` (повторы разворачивает сервер, как у JMAP), .ics — в воркере
//!   (`postman_worker::parse_calendar`, JSCalendar) → `postman_calendar::Event`.
//! - **Встреча** — VEVENT с ORGANIZER и ATTENDEE (`SCHEDULE-AGENT=SERVER`, `postman_calendar::ics`): приглашения,
//!   ответы и отмены рассылает сервер. Правка — новая версия своего ресурса (экземпляр серии — исключение с
//!   `RECURRENCE-ID`), отмена — DELETE (экземпляр — `EXDATE`), S7-07б. **Ответ на приглашение** — PARTSTAT своей копии с `If-Schedule-Tag-Match`;
//!   копии нет — письмом iMIP (это делает почтовый путь).
//! - **Занятость** — POST VFREEBUSY в outbox; моя — из моих событий. Пустая занятость коллеги — «нет данных», как у
//!   JMAP (`docs/STALWART.md` п. 46). **Открыть занятость** — ACL `read-free-busy` календарей.
//! - **Задачи** — VTODO тех же календарей ([`DavTasks`] — `tasks::TaskStore`): записи с `SCHEDULE-AGENT=CLIENT`
//!   (письма поручения шлёт Postman), дельта — `sync-collection`; полный список вместо дельты (`Changes::full`) или
//!   токен, которого сервер не знает, — полная загрузка (`cannotCalculateChanges`).
//! - **Контакты** — `sync-collection` + `addressbook-multiget`, vCard — в воркере (JSContact → `ContactCard`); полный
//!   список — сверка по ETag (`postman_dav::diff`). Правка — своя карточка построчно (`postman_compose::vcard`).
//! - **Изменения** — опрос по таймеру синхронизации: ctag и sync-token календарей → `CalendarChanged`.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use postman_core::backend::{AreaCtx, AreaPlugin, Areas, BoxFuture, Login};
use postman_core::model::ErrorKind;
use postman_core::sync::{Command, Event as SyncEvent, owned_by};

type CoreResult<T> = postman_core::model::Result<T>;

use jiff::tz::TimeZone;
use postman_calendar::{Busy, Event, Partstat, Span, Task};
use postman_dav::{Client, Collection, Config, Content, Discovery, Error, Item, Precondition, Privilege, Tokens, child_href, diff, same_href};
use serde_json::{Map, Value};

use postman_core::model::{ContactCard, Feature, Features};
use postman_core::schedule::{Meeting, SEARCH_LIMIT, busy_from_events, parse_events};
use postman_core::tasks::{Delta, TaskStore};

/// Сколько ждать воркер на разборе .ics и .vcf.
const WORKER_TIMEOUT: Duration = Duration::from_secs(10);

/// Карточка адресной книги: ETag, текст на сервере (правка — построчно поверх него) и модель.
#[derive(Debug, Clone)]
struct Stored {
    etag: String,
    data: String,
    card: ContactCard,
}

/// Адресная книга: токен и карточки по путям.
#[derive(Debug, Default)]
struct Book {
    token: Option<String>,
    cards: BTreeMap<String, Stored>,
}

/// CalDAV и CardDAV одной учётной записи.
pub struct Dav {
    client: Client,
    found: Discovery,
    /// Адрес участника (`calendar-user-address-set`, иначе вход).
    me: String,
    calendars: Vec<Collection>,
    books: Vec<Collection>,
    book_state: BTreeMap<String, Book>,
    /// Признаки изменений календарей с прошлого опроса.
    seen: Vec<(String, Tokens)>,
    worker: Mutex<Option<postman_worker::Client>>,
}

impl std::fmt::Debug for Dav {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dav").field("calendars", &self.calendars.len()).field("books", &self.books.len()).finish_non_exhaustive()
    }
}

/// Род ошибки DAV — как у JMAP: HTTP — по ответу (401 — вход), отказ сервера — `Server`, прочее — `Protocol`.
fn kind(e: &Error) -> ErrorKind {
    match e {
        Error::Http(h) => match h.status().map(|s| s.as_u16()) {
            Some(401) => ErrorKind::Auth,
            Some(_) => ErrorKind::Server,
            None if h.is_decode() => ErrorKind::Protocol,
            None => ErrorKind::Network,
        },
        Error::SyncToken | Error::NotFound | Error::Precondition | Error::Auth | Error::Forbidden => ErrorKind::Server,
        _ => ErrorKind::Protocol,
    }
}

/// Ошибка DAV → нейтральная (её понимают `postman_core::tasks` и `sync::ErrorKind`): отказы сервера — с кодами
/// JMAP (`cannotCalculateChanges`, `notFound`, `stateMismatch`…), текст HTTP и адреса — как у JMAP.
pub fn core_error(e: Error) -> postman_core::model::Error {
    use postman_core::model::Error as C;
    let method = |code: &str| C::Method { method: "CalDAV".into(), kind: code.into(), description: String::new() };
    let k = kind(&e);
    match e {
        Error::Http(h) => C::other(k, format!("HTTP: {h}")),
        Error::Url(u) => C::other(k, format!("URL: {u}")),
        Error::SyncToken => method("cannotCalculateChanges"),
        Error::NotFound => method("notFound"),
        Error::Precondition => method("stateMismatch"),
        Error::Auth => method("unauthorized"),
        Error::Forbidden => method("forbidden"),
        other => C::Protocol(other.to_string()),
    }
}

/// Ошибка DAV для договора областей: род — [`kind`], текст — как у DAV (его показывает интерфейс).
fn shown(e: Error) -> postman_core::model::Error {
    postman_core::model::Error::other(kind(&e), e)
}

fn now() -> i64 {
    jiff::Timestamp::now().as_second()
}

/// Новый UID: время и счётчик (как у задач, `tasks::new_uid`).
/// `recurrenceId` экземпляра (JSCalendar, `2026-11-02T07:00:00`, время `expand` — UTC) → `RECURRENCE-ID` в UTC.
fn rid_ical(rid: &str) -> String {
    match jiff::civil::DateTime::strptime("%Y-%m-%dT%H:%M:%S", rid.trim().trim_end_matches('Z')) {
        Ok(dt) => dt.strftime("%Y%m%dT%H%M%SZ").to_string(),
        Err(_) => rid.trim().to_owned(),
    }
}

fn new_uid() -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    postman_core::tasks::new_uid(nanos)
}

/// Записи JSCalendar ответа воркера (`{ method, data }`): группа или одна запись.
fn entries(v: &Value) -> Vec<Value> {
    let data = &v["data"];
    match data["entries"].as_array() {
        Some(e) => e.clone(),
        None if data.is_object() => vec![data.clone()],
        None => Vec::new(),
    }
}

/// Коллекция ресурса по его пути (`/dav/cal/u/default/x.ics` → `/dav/cal/u/default/`).
fn parent(href: &str) -> &str {
    match href.trim_end_matches('/').rfind('/') {
        Some(i) => &href[..=i],
        None => href,
    }
}

/// Принципал коллеги по адресу — соседний со своим (`/dav/pal/<адрес>/`, так у Stalwart); проверяется по его
/// `calendar-user-address-set`.
pub fn sibling_principal(mine: &str, email: &str) -> String {
    const KEEP: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');
    format!("{}{}/", parent(mine), percent_encoding::utf8_percent_encode(email, KEEP))
}

/// Применить патч JMAP (`путь/к/свойству` → значение, `null` — убрать; `~1` — `/`, `~0` — `~`) к объекту.
pub fn apply_patch(v: &mut Value, patch: &Map<String, Value>) {
    for (path, value) in patch {
        let parts: Vec<String> = path.split('/').map(|p| p.replace("~1", "/").replace("~0", "~")).collect();
        let Some((last, head)) = parts.split_last() else { continue };
        let mut cur = Some(&mut *v);
        for p in head {
            cur = cur.and_then(|c| c.as_object_mut()).map(|m| m.entry(p.clone()).or_insert_with(|| Value::Object(Map::new())));
        }
        if let Some(m) = cur.and_then(|c| c.as_object_mut()) {
            if value.is_null() {
                m.remove(last);
            } else {
                m.insert(last.clone(), value.clone());
            }
        }
    }
}

impl Dav {
    /// Войти и найти календари и адресные книги. `email` — адрес учётной записи (если сервер не дал свой).
    pub async fn connect(url: &str, user: &str, password: &str, email: &str) -> Result<Dav, Error> {
        let client = Client::new(&Config { url: url.to_owned(), user: user.to_owned(), password: password.to_owned() })?;
        let found = client.discover().await?;
        let me = found.email().unwrap_or(email).to_owned();
        let mut dav = Dav {
            client,
            found,
            me,
            calendars: Vec::new(),
            books: Vec::new(),
            book_state: BTreeMap::new(),
            seen: Vec::new(),
            worker: Mutex::new(None),
        };
        dav.refresh_collections().await?;
        Ok(dav)
    }

    async fn refresh_collections(&mut self) -> Result<(), Error> {
        let mut calendars = Vec::new();
        for home in &self.found.calendar_home {
            calendars.extend(self.client.calendars(home).await?);
        }
        let mut books = Vec::new();
        for home in &self.found.addressbook_home {
            books.extend(self.client.address_books(home).await?);
        }
        self.seen = calendars.iter().map(|c| (c.href.clone(), Tokens { ctag: c.ctag.clone(), sync_token: c.sync_token.clone() })).collect();
        self.calendars = calendars;
        self.books = books;
        Ok(())
    }

    /// Что есть у сервера: календарь, задачи (календарь с VTODO), контакты, занятость (outbox). Справочника у
    /// CardDAV нет.
    pub fn features(&self) -> Features {
        Features::NONE
            .with(Feature::Calendar, self.calendars.iter().any(|c| c.supports("VEVENT")))
            .with(Feature::Tasks, self.calendars.iter().any(|c| c.supports("VTODO")))
            .with(Feature::Contacts, !self.books.is_empty())
            .with(Feature::FreeBusy, self.found.has_scheduling())
    }

    pub fn me(&self) -> &str {
        &self.me
    }

    /// Задачи — хранилище для `tasks::handle`.
    pub fn tasks(&self) -> DavTasks<'_> {
        DavTasks { dav: self }
    }

    fn event_calendars(&self) -> impl Iterator<Item = &Collection> {
        self.calendars.iter().filter(|c| c.supports("VEVENT"))
    }

    /// Календарь для новых встреч: объявленный сервером для приглашений, иначе первый с VEVENT, куда можно писать.
    fn meeting_calendar(&self) -> Option<&str> {
        let declared = self.found.schedule_default_calendar.as_deref().filter(|d| self.calendars.iter().any(|c| same_href(&c.href, d)));
        declared.or_else(|| self.event_calendars().find(|c| c.writable).map(|c| c.href.as_str()))
    }

    /// Работа воркера-песочницы — в отдельном потоке (он блокирует).
    async fn in_worker<T: Send + 'static>(&self, f: impl FnOnce(&mut postman_worker::Client) -> T + Send + 'static) -> Result<T, Error> {
        let taken = self.worker.lock().unwrap_or_else(PoisonError::into_inner).take();
        let mut worker = taken.unwrap_or_else(|| postman_worker::Client::new(postman_worker::Client::default_exe(), WORKER_TIMEOUT));
        let (worker, out) = tokio::task::spawn_blocking(move || {
            let out = f(&mut worker);
            (worker, out)
        })
        .await
        .map_err(|e| Error::Protocol(format!("worker task: {e}")))?;
        *self.worker.lock().unwrap_or_else(PoisonError::into_inner) = Some(worker);
        Ok(out)
    }

    /// .ics ресурсов → записи JSCalendar с `id` (путь, у экземпляра повтора — ещё `#recurrenceId`) и `calendarIds`.
    /// Неразборчивый ресурс пропускается.
    async fn parse_ics(&self, resources: Vec<postman_dav::Resource>) -> Result<Vec<Value>, Error> {
        let parsed =
            self.in_worker(move |w| resources.into_iter().map(|r| (r.href, w.parse_calendar(r.data.as_bytes()))).collect::<Vec<_>>()).await?;
        let mut out = Vec::new();
        for (href, r) in parsed {
            match r {
                Ok(v) => {
                    for mut e in entries(&v) {
                        let id = match e["recurrenceId"].as_str() {
                            Some(rid) => format!("{href}#{rid}"),
                            None => href.clone(),
                        };
                        e["id"] = Value::from(id);
                        e["calendarIds"] = serde_json::json!({ parent(&href): true });
                        out.push(e);
                    }
                }
                Err(e) => tracing::warn!("caldav: resource skipped: {e}"),
            }
        }
        Ok(out)
    }

    /// События в `[from, to)` всех календарей с VEVENT: повторы развёрнуты сервером, без отменённых, по времени.
    pub async fn events(&self, from: i64, to: i64) -> Result<Vec<Event>, Error> {
        let mut resources = Vec::new();
        for c in self.event_calendars() {
            resources.extend(self.client.calendar_expand(&c.href, "VEVENT", Span::new(from, to)).await?);
        }
        let values: Vec<Value> = self.parse_ics(resources).await?.into_iter().filter(|v| v["@type"] == "Event").collect();
        Ok(parse_events(&values, &TimeZone::system()))
    }

    /// Поиск в календаре по тексту (тема, место, описание, участники) в `[from, to)` — клиентом по событиям окна.
    pub async fn search(&self, text: &str, from: i64, to: i64) -> Result<Vec<Event>, Error> {
        let needle = text.trim().to_lowercase();
        let mut found: Vec<Event> = self
            .events(from, to)
            .await?
            .into_iter()
            .filter(|e| {
                let people = e.participants.iter().flat_map(|p| [p.name.to_lowercase(), p.email.to_lowercase()]);
                [e.title.to_lowercase(), e.location.to_lowercase(), e.description.to_lowercase()]
                    .into_iter()
                    .chain(people)
                    .any(|s| s.contains(&needle))
            })
            .collect();
        found.truncate(SEARCH_LIMIT);
        Ok(found)
    }

    /// Занятость: моя (из моих событий) и по адресам через outbox. `None` у адреса — нет данных (не наш
    /// пользователь, календарь не открыт, сервер не ответил): это не «свободен».
    pub async fn availability(&self, emails: &[String], from: i64, to: i64) -> Result<(Vec<Busy>, Vec<(String, Option<Vec<Busy>>)>), Error> {
        let mine = busy_from_events(&self.events(from, to).await?);
        let Some(outbox) = self.found.schedule_outbox.as_deref().filter(|_| !emails.is_empty()) else {
            return Ok((mine, emails.iter().map(|e| (e.clone(), None)).collect()));
        };
        let who: Vec<&str> = emails.iter().map(String::as_str).collect();
        let replies = self.client.free_busy(outbox, &self.me, &who, Span::new(from, to), &format!("postman-fb-{}", new_uid())).await?;
        let people = emails
            .iter()
            .map(|e| {
                let busy = replies.iter().find(|r| r.email.eq_ignore_ascii_case(e)).and_then(|r| r.busy.clone()).filter(|b| !b.is_empty());
                (e.clone(), busy)
            })
            .collect();
        Ok((mine, people))
    }

    /// Встреча: VEVENT в календарь для встреч; есть участники — приглашения рассылает сервер.
    pub async fn create_meeting(&self, meeting: &Meeting) -> Result<String, Error> {
        let cal = self.meeting_calendar().ok_or_else(|| Error::Protocol("no calendar for meetings".into()))?.to_owned();
        let attendees = meeting.attendee_refs();
        let ics = meeting.new_event(&cal, ("", &self.me), &attendees).to_ics(now()).map_err(Error::Protocol)?;
        let href = child_href(&cal, &meeting.uid, Content::Calendar);
        self.client.put(&href, Content::Calendar, &ics, Precondition::Create).await?;
        Ok(href)
    }

    /// Своя встреча недели: ресурс (путь до `#`) и экземпляр серии — `RECURRENCE-ID` в UTC (`20261102T070000Z`; id
    /// экземпляра — `путь#recurrenceId`, время экземпляров `expand` отдаёт в UTC).
    async fn own_meeting(&self, event: &Event) -> Result<(postman_dav::Resource, Option<String>), Error> {
        let (href, rid) = match event.id.split_once('#') {
            Some((h, r)) => (h, Some(rid_ical(r))),
            None => (event.id.as_str(), None),
        };
        let res = self.client.get(href).await?;
        // У исключения Stalwart отдаёт в `expand` его новое начало вместо исходного `RECURRENCE-ID` (№49).
        let rid = rid.map(|r| postman_calendar::ics::instance_rid(&res.data, &r));
        Ok((res, rid))
    }

    /// Изменить свою встречу (организатор): новая версия ресурса, изменения участникам рассылает сервер. У экземпляра
    /// серии — его исключение (`RECURRENCE-ID`) или, с `series`, вся серия со сдвигом на ту же разницу.
    pub async fn update_meeting(&self, event: &Event, meeting: &Meeting, series: bool) -> Result<String, Error> {
        let (res, rid) = self.own_meeting(event).await?;
        let attendees = meeting.attendee_refs();
        let ev = meeting.new_event("", ("", &self.me), &attendees);
        let shift = meeting.shift_from(event).ok_or_else(|| Error::Protocol(format!("time zone {}", meeting.time_zone)))?;
        let edit = match rid.as_deref() {
            Some(r) if !series => postman_calendar::ics::Edit::Instance { rid: r },
            _ => postman_calendar::ics::Edit::Whole { shift },
        };
        let ics = postman_calendar::ics::edited(&res.data, &ev, edit, now()).ok_or_else(|| Error::Protocol("no meeting in the resource".into()))?;
        let pre = res.etag.as_deref().map_or(Precondition::None, Precondition::Etag);
        self.client.put(&res.href, Content::Calendar, &ics, pre).await?;
        Ok(event.uid.clone())
    }

    /// Отменить свою встречу: ресурс удаляется — участникам отмену рассылает сервер (RFC 6638 §3.2.1.2). Экземпляр
    /// серии без `series` — `EXDATE` у серии (отмена только его).
    pub async fn cancel_meeting(&self, event: &Event, series: bool) -> Result<String, Error> {
        let (res, rid) = self.own_meeting(event).await?;
        let pre = res.etag.as_deref().map_or(Precondition::None, Precondition::Etag);
        match rid.as_deref().filter(|_| !series && postman_calendar::ics::is_series(&res.data)) {
            Some(r) => {
                let ics = postman_calendar::ics::without_instance(&res.data, r, now())
                    .ok_or_else(|| Error::Protocol("no series in the resource".into()))?;
                self.client.put(&res.href, Content::Calendar, &ics, pre).await?;
            }
            None => {
                self.client.delete(&res.href, pre).await?;
            }
        }
        Ok(event.uid.clone())
    }

    /// Ответ на приглашение своей копией (сервер шлёт REPLY организатору). `false` — копии нет (приглашение пришло
    /// письмом мимо сервера): отвечать письмом.
    pub async fn respond(&self, uid: &str, around: Span, status: Partstat) -> Result<bool, Error> {
        let window = Span::new(around.start - 86_400, around.end.max(around.start) + 86_400);
        let marker = format!("UID:{uid}");
        for c in self.event_calendars() {
            let found = self.client.calendar_query(&c.href, "VEVENT", Some(window)).await?;
            let Some(copy) = found.into_iter().find(|r| r.data.lines().any(|l| l.trim_end() == marker)) else { continue };
            let Some(answered) = postman_calendar::ics::with_partstat(&copy.data, &self.me, status) else { return Ok(false) };
            let pre = match (&copy.schedule_tag, &copy.etag) {
                (Some(t), _) => Precondition::ScheduleTag(t),
                (None, Some(e)) => Precondition::Etag(e),
                (None, None) => Precondition::None,
            };
            self.client.put(&copy.href, Content::Calendar, &answered, pre).await?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Открыть коллеге только занятость своих календарей (ACL `read-free-busy`).
    pub async fn share_free_busy(&self, email: &str) -> Result<(), Error> {
        let principal = sibling_principal(&self.found.principal, email);
        // Чужой принципал Stalwart не показывает (403, `docs/STALWART.md` п. 48): проверяем адрес, если он виден, иначе
        // запись ACL на несуществующего принципала отклонит сервер.
        let theirs = self.client.principal(&principal).await.map_err(|_| Error::Protocol(format!("principal of {email} not found")))?;
        let same = |a: &String| postman_dav::discover::strip_mailto(a).is_some_and(|x| x.eq_ignore_ascii_case(email));
        if !theirs.addresses.is_empty() && !theirs.addresses.iter().any(same) {
            return Err(Error::Protocol(format!("principal of {email} not found")));
        }
        let mut shared = 0;
        for c in self.event_calendars().filter(|c| c.writable) {
            self.client.share(&c.href, &principal, &[Privilege::read_free_busy()]).await?;
            shared += 1;
        }
        if shared == 0 {
            return Err(Error::Protocol("no calendar to share".into()));
        }
        Ok(())
    }

    /// Изменились ли календари с прошлого опроса (ctag, sync-token).
    pub async fn changed(&mut self) -> Result<bool, Error> {
        let mut changed = false;
        for (href, seen) in self.seen.iter_mut() {
            let now = self.client.tokens(href).await?;
            if now != *seen {
                *seen = now;
                changed = true;
            }
        }
        Ok(changed)
    }

    // ---------------------------------------------------------------------------------------------------------
    // Контакты.

    /// Личные контакты всех адресных книг: дельта по `sync-collection`, полный список — сверка по ETag.
    pub async fn contacts(&mut self) -> Result<Vec<ContactCard>, Error> {
        for book in self.books.iter().map(|b| b.href.clone()).collect::<Vec<_>>() {
            self.sync_book(&book).await?;
        }
        let mut cards: Vec<ContactCard> = self.book_state.values().flat_map(|b| b.cards.values().map(|s| s.card.clone())).collect();
        cards.sort_by_key(|c| c.name.to_lowercase());
        Ok(cards)
    }

    async fn sync_book(&mut self, book: &str) -> Result<(), Error> {
        let token = self.book_state.get(book).and_then(|b| b.token.clone());
        let ch = match self.client.sync(book, token.as_deref()).await {
            Err(Error::SyncToken) => self.client.sync(book, None).await?,
            other => other?,
        };
        let known: Vec<Item> = self
            .book_state
            .get(book)
            .map(|b| b.cards.iter().map(|(h, s)| Item { href: h.clone(), etag: s.etag.clone() }).collect())
            .unwrap_or_default();
        let (changed, removed) = if ch.full {
            // Полный список вместо изменений (первый раз или сервер не узнал токен): сверка по ETag.
            let listed = if token.is_some() { self.client.etags(book).await? } else { ch.changed.clone() };
            diff(&known, &listed)
        } else {
            (ch.changed.clone(), ch.removed.clone())
        };
        let hrefs: Vec<String> = changed.iter().map(|i| i.href.clone()).collect();
        let got = if hrefs.is_empty() { Default::default() } else { self.client.addressbook_multiget(book, &hrefs).await? };
        let pairs: Vec<(String, String, String)> = got.resources.into_iter().map(|r| (r.href, r.etag.unwrap_or_default(), r.data)).collect();
        let datas: Vec<String> = pairs.iter().map(|(_, _, d)| d.clone()).collect();
        let parsed = self.in_worker(move |w| datas.into_iter().map(|d| w.parse_contacts(d.as_bytes())).collect::<Vec<_>>()).await?;
        let state = self.book_state.entry(book.to_owned()).or_default();
        for h in removed.iter().chain(got.missing.iter()) {
            state.cards.retain(|k, _| !same_href(k, h));
        }
        for ((href, etag, data), r) in pairs.into_iter().zip(parsed) {
            match r.as_deref().map(|cards| cards.first()) {
                Ok(Some(js)) => {
                    let mut card = ContactCard::from_jscontact(js);
                    card.id = href.clone();
                    state.cards.insert(href, Stored { etag, data, card });
                }
                Ok(None) | Err(_) => tracing::warn!("carddav: card skipped"),
            }
        }
        state.token = Some(ch.token);
        Ok(())
    }

    /// Создать (`old: None`) или изменить карточку; ответ — её id (путь).
    pub async fn save_contact(&mut self, card: &ContactCard, old: Option<&ContactCard>) -> Result<String, Error> {
        let fields = |uid: &str| -> String {
            let c = postman_compose::vcard::Card {
                uid,
                name: &card.name,
                emails: &card.emails,
                phones: &card.phones,
                organization: &card.organization,
                title: &card.title,
            };
            match old.and_then(|o| self.stored(&o.id)) {
                Some(s) => postman_compose::vcard::patch(&s.data, &c),
                None => postman_compose::vcard::build(&c),
            }
        };
        match old.filter(|o| !o.id.is_empty()) {
            Some(o) => {
                let etag = self.stored(&o.id).map(|s| s.etag.clone()).filter(|e| !e.is_empty());
                let body = fields("");
                let pre = etag.as_deref().map_or(Precondition::None, Precondition::Etag);
                self.client.put(&o.id, Content::VCard, &body, pre).await?;
                Ok(o.id.clone())
            }
            None => {
                let book = self.books.iter().find(|b| b.writable).ok_or_else(|| Error::Protocol("no address book".into()))?.href.clone();
                let uid = new_uid();
                let href = child_href(&book, &uid, Content::VCard);
                self.client.put(&href, Content::VCard, &fields(&uid), Precondition::Create).await?;
                Ok(href)
            }
        }
    }

    pub async fn delete_contact(&mut self, id: &str) -> Result<(), Error> {
        let etag = self.stored(id).map(|s| s.etag.clone()).filter(|e| !e.is_empty());
        self.client.delete(id, etag.as_deref().map_or(Precondition::None, Precondition::Etag)).await?;
        Ok(())
    }

    fn stored(&self, id: &str) -> Option<&Stored> {
        self.book_state.values().find_map(|b| b.cards.iter().find(|(h, _)| same_href(h, id)).map(|(_, s)| s))
    }
}

/// Задачи в календарях CalDAV (VTODO) — хранилище `tasks` (S6-02) для учётной записи «стандарты». id записи —
/// путь ресурса, состояние — sync-token (иначе ctag) каждого календаря с VTODO.
pub struct DavTasks<'a> {
    dav: &'a Dav,
}

impl DavTasks<'_> {
    fn task_calendars(&self) -> impl Iterator<Item = &Collection> {
        self.dav.calendars.iter().filter(|c| c.supports("VTODO"))
    }

    /// Состояние: `путь\tsync:<токен>` или `путь\tctag:<ctag>` построчно.
    async fn state(&self) -> Result<String, Error> {
        let mut lines = Vec::new();
        for c in self.task_calendars() {
            let t = self.dav.client.tokens(&c.href).await?;
            let mark = match (t.sync_token, t.ctag) {
                (Some(s), _) => format!("sync:{s}"),
                (None, Some(c)) => format!("ctag:{c}"),
                (None, None) => "none:".into(),
            };
            lines.push(format!("{}\t{mark}", c.href));
        }
        Ok(lines.join("\n"))
    }

    async fn fetch(&self, hrefs: &[String]) -> Result<Vec<Value>, Error> {
        let mut by_collection: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        for h in hrefs {
            by_collection.entry(parent(h)).or_default().push(h.clone());
        }
        let mut resources = Vec::new();
        for (c, list) in by_collection {
            resources.extend(self.dav.client.calendar_multiget(c, &list).await?.resources);
        }
        Ok(self.dav.parse_ics(resources).await?.into_iter().filter(Task::is_task).collect())
    }
}

/// «Посчитать изменения нельзя» — `tasks::refresh` загрузит всё заново.
fn cannot_calculate() -> postman_core::model::Error {
    core_error(Error::SyncToken)
}

impl TaskStore for DavTasks<'_> {
    async fn load(&self) -> CoreResult<(Vec<Value>, String)> {
        let state = self.state().await.map_err(core_error)?;
        let mut resources = Vec::new();
        for c in self.task_calendars() {
            resources.extend(self.dav.client.calendar_query(&c.href, "VTODO", None).await.map_err(core_error)?);
        }
        let tasks = self.dav.parse_ics(resources).await.map_err(core_error)?.into_iter().filter(Task::is_task).collect();
        Ok((tasks, state))
    }

    async fn changes(&self, since: &str) -> CoreResult<Delta> {
        let calendars: Vec<&Collection> = self.task_calendars().collect();
        let marks: Vec<(&str, &str)> = since.lines().filter_map(|l| l.split_once('\t')).collect();
        if marks.len() != calendars.len() {
            return Err(cannot_calculate());
        }
        let (mut changed, mut destroyed, mut lines) = (Vec::new(), Vec::new(), Vec::new());
        for c in calendars {
            let Some((_, mark)) = marks.iter().find(|(h, _)| same_href(h, &c.href)) else { return Err(cannot_calculate()) };
            let Some(token) = mark.strip_prefix("sync:") else {
                // Без sync-collection: ctag тот же — изменений нет, иначе — всё заново (сверка `tasks::refresh`).
                let now = self.dav.client.tokens(&c.href).await.map_err(core_error)?;
                if mark.strip_prefix("ctag:").is_some_and(|t| now.ctag.as_deref() == Some(t)) {
                    lines.push(format!("{}\t{mark}", c.href));
                    continue;
                }
                return Err(cannot_calculate());
            };
            let ch = self.dav.client.sync(&c.href, Some(token)).await.map_err(core_error)?;
            if ch.full || ch.truncated {
                return Err(cannot_calculate());
            }
            if !ch.removed.is_empty() {
                // Только имена ресурсов: путь коллекции содержит адрес пользователя.
                let names: Vec<&str> = ch.removed.iter().map(|h| h.trim_end_matches('/').rsplit('/').next().unwrap_or_default()).collect();
                tracing::debug!("caldav tasks: removed {names:?}");
            }
            // Состояние — токен этого же ответа, а не новый PROPFIND: изменение между двумя запросами (другой клиент
            // удалил задачу) иначе потерялось бы навсегда — следующая дельта начиналась бы уже после него.
            lines.push(format!("{}\tsync:{}", c.href, ch.token));
            changed.extend(ch.changed.into_iter().map(|i| i.href));
            destroyed.extend(ch.removed);
        }
        let state = lines.join("\n");
        let changed = self.fetch(&changed).await.map_err(core_error)?;
        Ok(Delta { changed, destroyed, state })
    }

    async fn get(&self, ids: &[String]) -> CoreResult<Vec<Value>> {
        self.fetch(ids).await.map_err(core_error)
    }

    async fn create(&self, task: &Value) -> CoreResult<String> {
        let tz = TimeZone::system();
        let calendar = task["calendarIds"].as_object().and_then(|m| m.keys().next().cloned());
        let calendar = match calendar {
            Some(c) if self.dav.calendars.iter().any(|x| same_href(&x.href, &c)) => c,
            _ => self.default_calendar().await?,
        };
        let (uid, ics) = if task["@type"] == "Event" {
            // Блок времени задачи (`tasks::block_event`).
            let uid = task["uid"].as_str().map(str::to_owned).unwrap_or_else(new_uid);
            let start =
                task["start"].as_str().and_then(|s| format!("{s}Z").parse::<jiff::Timestamp>().ok()).map(|t| t.as_second()).unwrap_or_else(now);
            let secs = task["duration"].as_str().and_then(|d| postman_calendar::parse_duration(d).ok()).unwrap_or(1800);
            let parent = task["relatedTo"].as_object().and_then(|m| m.keys().next().cloned()).unwrap_or_default();
            let title = task["title"].as_str().unwrap_or_default();
            let ics = postman_calendar::ics::block(&uid, title, start, start + secs, &parent, now());
            (uid, ics)
        } else {
            let t = Task::from_jmap(task).map_err(postman_core::model::Error::Protocol)?;
            (t.uid.clone(), postman_calendar::ics::stored_task(&t, now(), &tz))
        };
        let href = child_href(&calendar, &uid, Content::Calendar);
        self.dav.client.put(&href, Content::Calendar, &ics, Precondition::Create).await.map_err(core_error)?;
        Ok(href)
    }

    async fn update(&self, id: &str, patch: &Map<String, Value>) -> CoreResult<()> {
        let current = self.dav.client.get(id).await.map_err(core_error)?;
        let etag = current.etag.clone();
        let mut values = self.dav.parse_ics(vec![current]).await.map_err(core_error)?;
        let Some(mut v) = values.drain(..).find(Task::is_task) else { return Err(core_error(Error::NotFound)) };
        apply_patch(&mut v, patch);
        let t = Task::from_jmap(&v).map_err(postman_core::model::Error::Protocol)?;
        let ics = postman_calendar::ics::stored_task(&t, now(), &TimeZone::system());
        let pre = etag.as_deref().map_or(Precondition::None, Precondition::Etag);
        self.dav.client.put(id, Content::Calendar, &ics, pre).await.map_err(core_error)?;
        Ok(())
    }

    async fn destroy(&self, id: &str) -> CoreResult<()> {
        self.dav.client.delete(id, Precondition::None).await.map(|_| ()).map_err(core_error)
    }

    async fn default_calendar(&self) -> CoreResult<String> {
        self.task_calendars()
            .find(|c| c.writable)
            .map(|c| c.href.clone())
            .ok_or_else(|| postman_core::model::Error::Protocol("no calendar for tasks".into()))
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Плагин областей (ADR 0004).

/// Плагин CalDAV и CardDAV для реестра (`postman_core::backend::Registry`): ключ адреса — `dav`.
#[derive(Debug, Default, Clone, Copy)]
pub struct DavPlugin;

impl AreaPlugin for DavPlugin {
    fn key(&self) -> &'static str {
        "dav"
    }

    /// CalDAV на узле учётной записи: отвечает `https://<узел>/.well-known/caldav` (RFC 6764).
    fn probe<'a>(&'a self, host: &'a str) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            let url = format!("https://{host}");
            postman_dav::has_dav(&url).await.then_some(url)
        })
    }

    fn set_tls13_only(&self, on: bool) {
        postman_dav::set_tls13_only(on);
    }

    fn connect<'a>(&'a self, url: &'a str, login: &'a Login) -> BoxFuture<'a, CoreResult<Box<dyn Areas>>> {
        Box::pin(async move {
            match Dav::connect(url, &login.user, &login.password, &login.email).await {
                Ok(d) => Ok(Box::new(d) as Box<dyn Areas>),
                Err(e) => Err(shown(e)),
            }
        })
    }
}

impl Areas for Dav {
    fn features(&self) -> Features {
        Dav::features(self)
    }

    fn handle<'a>(&'a mut self, cmd: Command, ctx: AreaCtx<'a>) -> BoxFuture<'a, Option<Command>> {
        Box::pin(self.command(cmd, ctx))
    }

    fn respond<'a>(&'a mut self, invite: &'a postman_calendar::Invitation, status: Partstat) -> BoxFuture<'a, CoreResult<bool>> {
        Box::pin(async move { Dav::respond(self, &invite.event.uid, invite.event.span, status).await.map_err(shown) })
    }

    fn flush_tasks<'a>(&'a mut self, ctx: AreaCtx<'a>) -> BoxFuture<'a, CoreResult<()>> {
        Box::pin(async move {
            let notify = ctx.notify;
            postman_core::tasks::handle(&self.tasks(), ctx.cache, postman_core::tasks::Request::Flush, &|r| notify(SyncEvent::Tasks(r))).await
        })
    }

    fn changed(&mut self) -> BoxFuture<'_, CoreResult<bool>> {
        Box::pin(async move { Dav::changed(self).await.map_err(shown) })
    }
}

impl Dav {
    /// Календарь, задачи и контакты — тот же договор событий, что у JMAP; чужая команда — назад.
    async fn command(&mut self, cmd: Command, ctx: AreaCtx<'_>) -> Option<Command> {
        let notify = ctx.notify;
        match cmd {
            Command::Events { from, to } => notify(match self.events(from, to).await {
                Ok(events) => {
                    let (events, colleagues) = owned_by(&ctx.owner.cloned(), events, Vec::new());
                    SyncEvent::EventsLoaded { from, to, events, colleagues, account: ctx.profile.map(str::to_owned) }
                }
                Err(e) => SyncEvent::CalendarFailed(e.to_string()),
            }),
            Command::SearchEvents { text, from, to } => notify(match self.search(&text, from, to).await {
                Ok(events) => SyncEvent::EventsFound { text, events, error: None },
                Err(e) => SyncEvent::EventsFound { text, events: Vec::new(), error: Some(e.to_string()) },
            }),
            Command::Availability { ticket, emails, from, to, .. } => notify(match self.availability(&emails, from, to).await {
                Ok((me, people)) => SyncEvent::Availability { ticket, me, people, error: None },
                Err(e) => SyncEvent::Availability {
                    ticket,
                    me: Vec::new(),
                    people: emails.into_iter().map(|e| (e, None)).collect(),
                    error: Some(e.to_string()),
                },
            }),
            Command::CreateMeeting { meeting, ticket } => match self.create_meeting(&meeting).await {
                Ok(_) => {
                    notify(SyncEvent::MeetingCreated { ticket, uid: meeting.uid.clone() });
                    notify(SyncEvent::CalendarChanged);
                }
                Err(e) => notify(SyncEvent::MeetingFailed { ticket, error: e.to_string() }),
            },
            Command::UpdateMeeting { event, meeting, series, ticket } => match self.update_meeting(&event, &meeting, series).await {
                Ok(uid) => {
                    notify(SyncEvent::MeetingUpdated { ticket, uid });
                    notify(SyncEvent::CalendarChanged);
                }
                Err(e) => notify(SyncEvent::MeetingFailed { ticket, error: e.to_string() }),
            },
            Command::CancelMeeting { event, series, ticket, .. } => match self.cancel_meeting(&event, series).await {
                Ok(uid) => {
                    notify(SyncEvent::MeetingCancelled { ticket, uid });
                    notify(SyncEvent::CalendarChanged);
                }
                Err(e) => notify(SyncEvent::MeetingFailed { ticket, error: e.to_string() }),
            },
            // Справочника у CardDAV нет — подробностей человека тоже.
            Command::PersonDetails { email } => notify(SyncEvent::PersonDetails { email, person: None }),
            Command::ShareFreeBusy { email } => notify(match self.share_free_busy(&email).await {
                Ok(()) => SyncEvent::FreeBusyShared { email },
                Err(e) => SyncEvent::CalendarFailed(e.to_string()),
            }),
            Command::Tasks(request) => {
                if let Err(e) = postman_core::tasks::handle(&self.tasks(), ctx.cache, request, &|r| notify(SyncEvent::Tasks(r))).await {
                    tracing::warn!("caldav tasks: {e}");
                    notify(SyncEvent::Tasks(postman_core::tasks::Reply::Failed { uid: None, error: e.to_string() }));
                }
            }
            Command::Contacts => {
                self.load_contacts(notify).await;
                // Справочника у CardDAV нет: раздел «Справочник» скрыт (`Features`), список — пустой.
                notify(SyncEvent::DirectoryLoaded(Vec::new()));
            }
            Command::SaveContact { card, old, ticket } => {
                notify(match self.save_contact(&card, old.as_deref()).await {
                    Ok(id) => SyncEvent::ContactSaved { ticket, id },
                    Err(e) => SyncEvent::ContactFailed { ticket, error: e.to_string() },
                });
                self.load_contacts(notify).await;
            }
            Command::DeleteContact { id, ticket } => {
                notify(match self.delete_contact(&id).await {
                    Ok(()) => SyncEvent::ContactDeleted { ticket, id },
                    Err(e) => SyncEvent::ContactFailed { ticket, error: e.to_string() },
                });
                self.load_contacts(notify).await;
            }
            other => return Some(other),
        }
        None
    }

    /// Личные контакты CardDAV — разделу целиком и подсказкам адресов парами (имя, адрес), как у JMAP.
    async fn load_contacts(&mut self, notify: &dyn Fn(SyncEvent)) {
        match self.contacts().await {
            Ok(cards) => {
                notify(SyncEvent::Contacts(cards.iter().flat_map(|c| c.emails.iter().map(|e| (c.name.clone(), e.clone()))).collect()));
                notify(SyncEvent::ContactsLoaded(cards));
            }
            Err(e) => notify(SyncEvent::ContactFailed { ticket: 0, error: e.to_string() }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn instance_id_to_recurrence_id() {
        assert_eq!(rid_ical("2026-11-02T07:00:00"), "20261102T070000Z");
        assert_eq!(rid_ical("2026-11-02T07:00:00Z"), "20261102T070000Z");
        assert_eq!(rid_ical("x"), "x");
    }

    #[test]
    fn patch_paths_and_removal() {
        let mut v = json!({ "title": "a", "keywords": { "x": true }, "iCalendar": { "properties": [1] } });
        let patch: Map<String, Value> = serde_json::from_value(json!({
            "title": "b",
            "keywords/y": true,
            "keywords/x": null,
            "iCalendar/properties": [2],
            "relatedTo/p~1q": { "relation": { "parent": true } },
        }))
        .unwrap();
        apply_patch(&mut v, &patch);
        assert_eq!(v["title"], "b");
        assert_eq!(v["keywords"], json!({ "y": true }));
        assert_eq!(v["iCalendar"]["properties"], json!([2]));
        assert_eq!(v["relatedTo"]["p/q"]["relation"]["parent"], true);
    }

    #[test]
    fn paths_and_principals() {
        assert_eq!(parent("/dav/cal/u/default/x.ics"), "/dav/cal/u/default/");
        assert_eq!(parent("/dav/cal/u/default/"), "/dav/cal/u/");
        assert_eq!(sibling_principal("/dav/pal/user1%40itt.test/", "user2@itt.test"), "/dav/pal/user2%40itt.test/");
    }

    #[test]
    fn errors_keep_their_kind() {
        assert_eq!(ErrorKind::of(&core_error(Error::Precondition)), ErrorKind::Server);
        assert_eq!(core_error(Error::SyncToken).code(), Some("cannotCalculateChanges"));
        assert_eq!(core_error(Error::NotFound).to_string(), "CalDAV: notFound ");
        assert_eq!(shown(Error::Forbidden).to_string(), "forbidden");
        assert_eq!(shown(Error::Auth).kind(), ErrorKind::Server);
        assert_eq!(ErrorKind::of(&core_error(Error::Protocol("x".into()))), ErrorKind::Protocol);
    }

    #[test]
    fn worker_entries() {
        let group = json!({ "data": { "@type": "Group", "entries": [{ "@type": "Task" }, { "@type": "Event" }] } });
        assert_eq!(entries(&group).len(), 2);
        assert_eq!(entries(&json!({ "data": { "@type": "Task" } })).len(), 1);
        assert!(entries(&json!({})).is_empty());
    }
}
