//! Области Exchange по EWS (ADR 0004, `Areas`): календарь, ответы на встречи, занятость, личные контакты и справочник
//! (GAL) — тем же договором событий, что у JMAP и CalDAV; экраны те же, чего нет (задач) — скрыто по `Features`.
//!
//! - **Неделя** — `FindItem` + `CalendarView`: повторы разворачивает сервер (экземпляры и изменённые экземпляры —
//!   отдельными событиями со своим id и общим UID серии; удалённые экземпляры сервер не показывает), подробности —
//!   `GetItem` пачками; встреча той же версии (`ChangeKey`) второй раз не запрашивается. Отменённые встречи (у участника
//!   после отмены организатором — `IsCancelled`) не показываются, как у JMAP.
//! - **Встреча** — `CreateItem` с рассылкой сервером (копия — в «Отправленные», как у Outlook); пояс — Windows id
//!   (`postman_ews::tz`, таблица CLDR целиком); сервер не знает этот пояс — встреча уходит в UTC. UID задаёт сервер —
//!   он и уходит в `MeetingCreated`. Правка и отмена (`Command::UpdateMeeting`, `CancelMeeting`, S7-07б) — `UpdateItem`
//!   и `DeleteItem` с рассылкой: экземпляр серии — его исключение, серия — по серии со сдвигом на ту же разницу.
//! - **Ответ на приглашение** — `AcceptItem`/`TentativelyAcceptItem`/`DeclineItem` по встрече в календаре (её кладёт
//!   туда сам Exchange при доставке приглашения), найденной по UID ([`pick`]); у серии — по самой серии, у приглашения
//!   на один экземпляр (`recurrenceId`) — по экземпляру. Встречи с этим UID нет — ответ письмом iMIP (почтовый путь).
//! - **Занятость** — `GetUserAvailability` (моя — тем же запросом); ошибка по адресу — «нет данных», не «свободен».
//! - **Справочник** — `FindPeople` по GAL (`Command::FindPeople` → `PeopleFound`) с должностью, отделом и телефоном;
//!   сервер его не знает — `ResolveNames`. Справочник у Exchange — поиск с подсказкой (решение владельца 2026-10-28):
//!   без строки — пусто, `DirectoryLoaded` пустой, страница (`DirectoryPage`) — `search_only`; GAL целиком не читаем.
//!   Телефон для карточки — `ResolveNames` по адресу (`Command::PersonDetails`): `FindPeople` у Exchange 2013 его не отдаёт.
//! - **Мои контакты** (T1-25в) — папка «Контакты»: `FindItem` по страницам и `GetItem` (`ContactsLoaded`), запись —
//!   `CreateItem`, `UpdateItem` (поля карточки поверх прочитанного с сервера), `DeleteItem` в «Удалённые». Списки
//!   рассылки — не карточки (только чтение у сервера), в список не идут.
//! - **Изменения** — опрос `SyncFolderItems` календаря и контактов по таймеру синхронизации и по событиям сервера →
//!   `CalendarChanged`, `ContactsLoaded`.

use std::collections::HashMap;

use postman_calendar::{Busy, Event, EventStatus, Invitation, Partstat, Span};
use postman_core::backend::{AreaCtx, Areas, BoxFuture};
use postman_core::model::{ContactCard, Feature, Features, Principal};
use postman_core::schedule::{Meeting, SEARCH_LIMIT};
use postman_core::sync::{Command, Event as SyncEvent, owned_by};
use postman_ews::{CalendarItem, Changes, Client, DeleteMode, Error, FolderRef, ItemId, ItemKind, NewItem, Notify, Reply, SyncState, WellKnown};

/// Встреч в окне календаря — не больше (месяц с запасом).
const VIEW_LIMIT: u32 = 1000;
/// Изменений за один `SyncFolderItems`.
const SYNC_PAGE: u32 = 512;
/// Страниц `SyncFolderItems` за один опрос (остальное — в следующий).
const SYNC_PAGES: usize = 20;
/// Людей справочника на одну строку поиска.
pub const PEOPLE_LIMIT: usize = 20;
/// Поиск справочника — со скольки символов.
pub const PEOPLE_FROM: usize = 3;
/// Где искать встречу приглашения: сутки до и после него.
const AROUND: i64 = 86_400;

/// Занятость: моя и по адресам (`None` у адреса — нет данных), как у JMAP и CalDAV.
pub type Availability = (Vec<Busy>, Vec<(String, Option<Vec<Busy>>)>);

/// Встреча, которую уже видели: версия, вид (экземпляр серии или нет) и событие.
#[derive(Debug, Clone)]
struct Known {
    change_key: Option<String>,
    kind: ItemKind,
    event: Event,
}

/// Области EWS одной учётной записи.
pub struct EwsAreas {
    client: Client,
    me: String,
    state: Option<SyncState>,
    /// Встречи по `ItemId.Id`.
    items: HashMap<String, Known>,
    /// `FindPeople` работает на этом сервере; отказал однажды — справочник по `ResolveNames`.
    find_people: bool,
    /// Состояние `SyncFolderItems` папки «Контакты»: изменились ли контакты.
    contacts: Option<SyncState>,
}

/// Контактов папки «Контакты» — не больше (страницами по [`CONTACT_PAGE`]).
const CONTACTS_LIMIT: u32 = 5000;
const CONTACT_PAGE: u32 = 100;

/// Контакт Exchange → карточка Postman: отображаемое имя, адреса, телефоны, компания, должность. Списков рассылки
/// нет (`None`).
pub fn card(c: postman_ews::Contact) -> Option<ContactCard> {
    if c.members.is_some() {
        return None;
    }
    Some(ContactCard {
        id: c.id.id,
        name: c.data.shown_name(),
        emails: c.data.emails,
        phones: c.data.phones.into_iter().map(|p| p.number).collect(),
        organization: c.data.company,
        title: c.data.job_title,
    })
}

/// Карточка Postman → поля контакта Exchange поверх прежних (`base`: имя и фамилия, отдел, заметки, виды телефонов
/// остаются): телефон на своём месте — с прежним видом, новый — рабочий, мобильный, домашний, другой.
pub fn contact_data(card: &ContactCard, base: Option<&postman_ews::ContactData>) -> postman_ews::ContactData {
    use postman_ews::{Phone, PhoneKind};
    let mut d = base.cloned().unwrap_or_default();
    d.display_name = card.name.trim().to_owned();
    d.emails = card.emails.iter().map(|e| e.trim().to_owned()).filter(|e| !e.is_empty()).take(postman_ews::contacts::MAX_EMAILS).collect();
    let old: Vec<PhoneKind> = base.map(|b| b.phones.iter().map(|p| p.kind).collect()).unwrap_or_default();
    let mut free = [PhoneKind::Business, PhoneKind::Mobile, PhoneKind::Home, PhoneKind::Other].into_iter().filter(|k| !old.contains(k));
    d.phones = card
        .phones
        .iter()
        .map(|n| n.trim())
        .filter(|n| !n.is_empty())
        .enumerate()
        .map(|(i, n)| Phone { kind: old.get(i).copied().or_else(|| free.next()).unwrap_or(PhoneKind::Other), number: n.to_owned() })
        .collect();
    d.company = card.organization.trim().to_owned();
    d.job_title = card.title.trim().to_owned();
    d
}

impl std::fmt::Debug for EwsAreas {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EwsAreas").field("items", &self.items.len()).finish_non_exhaustive()
    }
}

/// Ответ на приглашение → операция EWS; «ждёт ответа» и «передано» — не ответ.
pub fn reply(status: Partstat) -> Option<Reply> {
    match status {
        Partstat::Accepted => Some(Reply::Accept),
        Partstat::Tentative => Some(Reply::Tentative),
        Partstat::Declined => Some(Reply::Decline),
        Partstat::NeedsAction | Partstat::Delegated => None,
    }
}

/// Новая встреча вкладки «Встреча» → `CreateItem`. Организатор — владелец ящика (поле «От» выбрало учётку).
pub fn new_item(meeting: &Meeting) -> Result<NewItem, String> {
    let refs = meeting.attendee_refs();
    NewItem::from_event(&meeting.new_event("", ("", ""), &refs))
}

/// Кому рассылать новую встречу: без участников — никому (своё событие), иначе — всем, с копией в «Отправленные».
pub fn notify_for(item: &NewItem) -> Notify {
    if item.required.is_empty() && item.optional.is_empty() { Notify::None } else { Notify::AllAndSaveCopy }
}

/// Сервер не знает Windows id пояса (старый Exchange, новый пояс): встречу — в UTC без пояса.
fn bad_time_zone(e: &Error) -> bool {
    e.code().is_some_and(|c| c.contains("TimeZone"))
}

/// Человек справочника → `Principal` модели (как у JMAP): id — адрес, вид — человек или список рассылки; должность,
/// отдел и телефон — из GAL.
pub fn principal(p: postman_ews::Person) -> Principal {
    Principal {
        id: p.email.to_lowercase(),
        name: if p.name.is_empty() { p.email.clone() } else { p.name },
        description: None,
        email: (!p.email.is_empty()).then_some(p.email),
        kind: if p.group { "group".into() } else { "individual".into() },
        title: p.title,
        department: p.department,
        phone: p.phone,
    }
}

/// Найденное по строке: название, место, описание, участники (как у CalDAV).
pub fn matching(events: Vec<Event>, text: &str) -> Vec<Event> {
    let needle = text.trim().to_lowercase();
    let mut found: Vec<Event> = events
        .into_iter()
        .filter(|e| {
            let people = e.participants.iter().flat_map(|p| [p.name.to_lowercase(), p.email.to_lowercase()]);
            [e.title.to_lowercase(), e.location.to_lowercase(), e.description.to_lowercase()].into_iter().chain(people).any(|s| s.contains(&needle))
        })
        .collect();
    found.truncate(SEARCH_LIMIT);
    found
}

/// Занятость `GetUserAvailability` → (моя, по адресам): первый адрес запроса — мой; ответы — в порядке адресов;
/// ошибка у адреса (не найден, нет доступа) — `None`, «нет данных».
pub fn split_availability(mut all: Vec<postman_ews::Availability>, emails: &[String]) -> Availability {
    let me = if all.is_empty() { Vec::new() } else { all.remove(0).busy };
    let people = emails.iter().enumerate().map(|(i, e)| (e.clone(), all.get(i).and_then(|a| a.error.is_none().then(|| a.busy.clone())))).collect();
    (me, people)
}

/// Что за приглашение: на один экземпляр серии (`recurrenceId` в JSCalendar) или на всю встречу.
pub fn for_instance(invite: &Invitation) -> bool {
    invite.raw.get("recurrenceId").is_some_and(|v| !v.is_null())
}

/// Встреча календаря для ответа: сначала — та же (`id` — ответ из подробностей события), затем — по UID: одиночная или
/// серия; у экземпляров — тот, что в это время (`start`), иначе ближайший. `None` — встречи нет (ответ письмом).
pub fn pick<'a>(known: impl IntoIterator<Item = (&'a str, ItemKind, &'a Event)>, id: &str, uid: &str, start: i64) -> Option<(&'a str, ItemKind)> {
    let mut best: Option<(&str, ItemKind, i64)> = None;
    for (item, kind, e) in known {
        if !id.is_empty() && item == id {
            return Some((item, kind));
        }
        if uid.is_empty() || !e.uid.eq_ignore_ascii_case(uid) {
            continue;
        }
        // Одиночная и серия — точнее экземпляра; экземпляры — по близости ко времени приглашения.
        let rank = match kind {
            ItemKind::Single | ItemKind::RecurringMaster => -1,
            ItemKind::Occurrence | ItemKind::Exception => (e.span.start - start).abs(),
        };
        if best.is_none_or(|(_, _, r)| rank < r) {
            best = Some((item, kind, rank));
        }
    }
    best.map(|(item, kind, _)| (item, kind))
}

/// Правка встречи полями вкладки «Встреча»: всё заново; время — сдвиг события недели (`was`), применённый к
/// правимому объекту (`target`: сама встреча или серия), с новой длительностью.
pub fn edit_changes(item: NewItem, was: Span, target: Span) -> Changes {
    let shift = item.span.start - was.start;
    Changes {
        title: Some(item.title),
        description: Some(item.description),
        location: Some(item.location),
        time: Some((Span::new(target.start + shift, target.start + shift + item.span.len()), false)),
        time_zone: item.time_zone,
        required: Some(item.required),
        optional: Some(item.optional),
        ..Changes::default()
    }
}

/// Перенос встречи: новое время (`Span`, UTC) в поясе IANA `time_zone` — правка `UpdateItem`.
pub fn moved(span: Span, time_zone: &str) -> Changes {
    Changes { time: Some((span, false)), time_zone: Some(time_zone.to_owned()), ..Changes::default() }
}

impl EwsAreas {
    /// Войти (основной календарь) и запомнить состояние календаря «сейчас».
    pub async fn connect(url: &str, user: &str, password: &str, email: &str) -> Result<EwsAreas, Error> {
        let mut client = Client::new(url, crate::auth(user, password))?;
        client.calendar_folder().await?;
        let mut a = EwsAreas { client, me: email.to_owned(), state: None, items: HashMap::new(), find_people: true, contacts: None };
        a.poll().await?;
        Ok(a)
    }

    /// Календарь, занятость, личные контакты, справочник. Задач, правил и автоответа по EWS у Postman нет.
    pub fn features(&self) -> Features {
        Features::NONE.with(Feature::Calendar, true).with(Feature::FreeBusy, true).with(Feature::Contacts, true).with(Feature::Directory, true)
    }

    /// Личные контакты папки «Контакты» — карточками (страницами `FindItem`, затем `GetItem`).
    pub async fn load_contacts(&mut self) -> Result<Vec<ContactCard>, Error> {
        let folder = FolderRef::WellKnown(WellKnown::Contacts);
        let mut ids = Vec::new();
        while (ids.len() as u32) < CONTACTS_LIMIT {
            let (page, more) = self.client.contacts_page(&folder, ids.len() as u32, CONTACT_PAGE).await?;
            let got = page.len();
            ids.extend(page);
            if !more || got == 0 {
                break;
            }
        }
        let mut cards: Vec<ContactCard> = self.client.get_contacts(&ids).await?.into_iter().filter_map(card).collect();
        cards.sort_by_key(|c| c.name.to_lowercase());
        Ok(cards)
    }

    /// Изменились ли контакты с прошлого раза (`SyncFolderItems` папки «Контакты»). Первый вызов — только состояние.
    pub async fn poll_contacts(&mut self) -> Result<bool, Error> {
        let first = self.contacts.is_none();
        let mut changed = false;
        for _ in 0..SYNC_PAGES {
            let page = match self.client.sync_contacts(&FolderRef::WellKnown(WellKnown::Contacts), self.contacts.as_ref(), SYNC_PAGE).await {
                Err(e) if e.is_invalid_sync_state() && self.contacts.is_some() => {
                    self.contacts = None;
                    changed = true;
                    continue;
                }
                r => r?,
            };
            changed |= !page.created.is_empty() || !page.updated.is_empty() || !page.deleted.is_empty();
            self.contacts = Some(page.state);
            if page.done {
                break;
            }
        }
        Ok(changed && !first)
    }

    /// Записать карточку: новая — `CreateItem` в «Контакты», правка — поверх прочитанной с сервера → id.
    async fn save_contact(&mut self, card: &ContactCard, old: Option<&ContactCard>) -> Result<String, Error> {
        let Some(old) = old.filter(|o| !o.id.is_empty()) else {
            let id = self.client.create_contact(&FolderRef::WellKnown(WellKnown::Contacts), &contact_data(card, None)).await?;
            return Ok(id.id);
        };
        let id = ItemId::new(old.id.clone());
        let base = self.client.get_contacts(std::slice::from_ref(&id)).await?.into_iter().find(|c| c.id.id == id.id && c.members.is_none());
        let base = base.ok_or_else(|| Error::Ews { code: "ErrorItemNotFound".into(), message: String::new() })?;
        self.client.update_contact(&id, &contact_data(card, Some(&base.data))).await?;
        Ok(old.id.clone())
    }

    /// Контакты заново → `ContactsLoaded`, ошибка — `ContactFailed` с `ticket`.
    async fn contacts_loaded(&mut self, ticket: u64, notify: &dyn Fn(SyncEvent)) {
        match self.load_contacts().await {
            Ok(cards) => notify(SyncEvent::ContactsLoaded(cards)),
            Err(e) => notify(SyncEvent::ContactFailed { ticket, error: e.to_string() }),
        }
    }

    /// Изменился ли календарь с прошлого раза (`SyncFolderItems`). Первый вызов — только состояние.
    pub async fn poll(&mut self) -> Result<bool, Error> {
        let first = self.state.is_none();
        let mut changed = false;
        for _ in 0..SYNC_PAGES {
            let page = match self.client.sync(&FolderRef::WellKnown(WellKnown::Calendar), self.state.as_ref(), SYNC_PAGE).await {
                Err(e) if e.is_invalid_sync_state() && self.state.is_some() => {
                    // Сервер забыл состояние: начать заново, неделю — перечитать.
                    self.state = None;
                    self.items.clear();
                    changed = true;
                    continue;
                }
                r => r?,
            };
            for id in page.updated.iter().map(|i| &i.id).chain(&page.deleted) {
                self.items.remove(id);
            }
            changed |= !page.created.is_empty() || !page.updated.is_empty() || !page.deleted.is_empty();
            self.state = Some(page.state);
            if page.done {
                break;
            }
        }
        Ok(changed && !first)
    }

    /// Встречи окна (экземпляры серий — отдельно, как разворачивает сервер), по началу; отменённые — нет.
    pub async fn events(&mut self, from: i64, to: i64) -> Result<Vec<Event>, Error> {
        let (refs, all) = self.client.calendar_view(&FolderRef::WellKnown(WellKnown::Calendar), Span::new(from, to), VIEW_LIMIT).await?;
        if !all {
            tracing::warn!(shown = refs.len(), "exchange: calendar window truncated");
        }
        let missing: Vec<ItemId> =
            refs.iter().filter(|r| self.items.get(&r.id.id).is_none_or(|k| k.change_key != r.id.change_key)).map(|r| r.id.clone()).collect();
        if !missing.is_empty() {
            for it in self.client.get_items(&missing).await? {
                self.remember(it);
            }
        }
        let mut out: Vec<Event> = refs
            .iter()
            .filter_map(|r| self.items.get(&r.id.id))
            .filter(|k| k.event.status != EventStatus::Cancelled)
            .map(|k| k.event.clone())
            .collect();
        out.sort_by_key(|e| (e.span.start, e.span.end));
        Ok(out)
    }

    fn remember(&mut self, it: CalendarItem) {
        self.items.insert(it.id.id.clone(), Known { change_key: it.id.change_key.clone(), kind: it.kind, event: it.event });
    }

    pub async fn search(&mut self, text: &str, from: i64, to: i64) -> Result<Vec<Event>, Error> {
        Ok(matching(self.events(from, to).await?, text))
    }

    /// Занятость: моя и по адресам одним запросом.
    pub async fn availability(&mut self, emails: &[String], from: i64, to: i64) -> Result<Availability, Error> {
        let mut all: Vec<&str> = vec![self.me.as_str()];
        all.extend(emails.iter().map(String::as_str));
        let got = self.client.availability(&all, Span::new(from, to)).await?;
        Ok(split_availability(got, emails))
    }

    /// Встреча с рассылкой приглашений сервером → её id и UID (у Exchange UID задаёт сервер).
    pub async fn create_meeting(&mut self, meeting: &Meeting) -> Result<(ItemId, String), Error> {
        let mut item = new_item(meeting).map_err(|e| Error::Ews { code: "ClientMeeting".into(), message: e })?;
        let notify = notify_for(&item);
        let id = match self.client.create(None, &item, notify).await {
            Err(e) if bad_time_zone(&e) && item.time_zone.is_some() => {
                tracing::warn!("exchange: unknown time zone id, meeting in UTC");
                item.time_zone = None;
                self.client.create(None, &item, notify).await?
            }
            r => r?,
        };
        let created = self.client.get_items(std::slice::from_ref(&id)).await?.pop();
        let uid = created.as_ref().map(|it| it.event.uid.clone()).unwrap_or_default();
        if let Some(it) = created {
            self.remember(it);
        }
        Ok((id, if uid.is_empty() { meeting.uid.clone() } else { uid }))
    }

    /// Объект сервера для правки и отмены по событию недели и его время: сама встреча, а у экземпляра серии с
    /// `series` — серия (её время — время первого экземпляра).
    async fn target(&mut self, event: &Event, series: bool) -> Result<Option<(ItemId, Span)>, Error> {
        let known = self.items.get(&event.id).map(|k| (ItemId { id: event.id.clone(), change_key: k.change_key.clone() }, k.kind, k.event.span));
        let (id, kind, span) = match known {
            Some(k) => k,
            None => match self.client.get_items(&[ItemId::new(event.id.clone())]).await?.pop() {
                Some(it) => {
                    let k = (it.id.clone(), it.kind, it.event.span);
                    self.remember(it);
                    k
                }
                None => return Ok(None),
            },
        };
        if series && matches!(kind, ItemKind::Occurrence | ItemKind::Exception) {
            return Ok(self.client.master_of(&id).await?.map(|m| (m.id, m.event.span)));
        }
        Ok(Some((id, span)))
    }

    /// Правка с рассылкой участникам (копия — в «Отправленные»); сервер не знает пояс — без пояса (UTC).
    async fn update_item(&mut self, id: &ItemId, changes: &Changes) -> Result<(), Error> {
        match self.client.update(id, changes, Notify::AllAndSaveCopy).await {
            Err(e) if bad_time_zone(&e) && changes.time_zone.is_some() => {
                tracing::warn!("exchange: unknown time zone id, meeting in UTC");
                let utc = Changes { time_zone: None, ..changes.clone() };
                self.client.update(id, &utc, Notify::AllAndSaveCopy).await?;
            }
            r => {
                r?;
            }
        }
        self.items.remove(&id.id);
        Ok(())
    }

    /// Перенести или изменить встречу (организатор) готовой правкой: изменение — всем участникам, копия — в
    /// «Отправленные». `series` — у экземпляра серии изменить всю серию. Встречи нет — `false`.
    pub async fn reschedule(&mut self, event: &Event, changes: &Changes, series: bool) -> Result<bool, Error> {
        let Some((id, _)) = self.target(event, series).await? else { return Ok(false) };
        self.update_item(&id, changes).await?;
        Ok(true)
    }

    /// Изменить свою встречу полями вкладки «Встреча» (`Command::UpdateMeeting`): тема, место, описание, участники,
    /// время и пояс. У серии (`series`) время серии сдвигается на ту же разницу, что у экземпляра; без `series` у
    /// экземпляра — исключение (так делает сервер). Встречи нет — `false`.
    pub async fn update_meeting(&mut self, event: &Event, meeting: &Meeting, series: bool) -> Result<bool, Error> {
        let item = new_item(meeting).map_err(|e| Error::Ews { code: "ClientMeeting".into(), message: e })?;
        let Some((id, span)) = self.target(event, series).await? else { return Ok(false) };
        let changes = edit_changes(item, event.span, span);
        self.update_item(&id, &changes).await?;
        Ok(true)
    }

    /// Отменить встречу (организатор): участникам — отмена, встреча — в «Удалённые», как у Outlook. `series` — у
    /// экземпляра серии отменить всю серию (иначе — только этот экземпляр). Встречи нет — `false`.
    pub async fn cancel(&mut self, event: &Event, series: bool) -> Result<bool, Error> {
        let Some((id, _)) = self.target(event, series).await? else { return Ok(false) };
        self.client.delete(std::slice::from_ref(&id), DeleteMode::MoveToDeletedItems, Notify::AllAndSaveCopy).await?;
        self.items.remove(&id.id);
        Ok(true)
    }

    /// Подробности человека справочника по адресу (`ResolveNames`, с телефоном — `FindPeople` его не отдаёт).
    pub async fn person(&mut self, email: &str) -> Result<Option<Principal>, Error> {
        let found = self.client.resolve_names(email, 5).await?;
        Ok(found.into_iter().find(|p| p.email.eq_ignore_ascii_case(email.trim())).map(principal))
    }

    /// Ответ на приглашение по встрече в календаре около приглашения ([`pick`]); встречи нет — `false` (ответ письмом).
    /// Приглашение на серию — ответ всей серии (по экземпляру сервер ответил бы только ему), на один экземпляр — ему.
    pub async fn answer(&mut self, invite: &Invitation, status: Partstat) -> Result<bool, Error> {
        let Some(reply) = reply(status) else { return Ok(false) };
        let ev = &invite.event;
        self.events(ev.span.start - AROUND, ev.span.end.max(ev.span.start) + AROUND).await?;
        let found = pick(self.items.iter().map(|(id, k)| (id.as_str(), k.kind, &k.event)), &ev.id, &ev.uid, ev.span.start)
            .map(|(id, kind)| (ItemId { id: id.to_owned(), change_key: self.items.get(id).and_then(|k| k.change_key.clone()) }, kind));
        let Some((mut id, kind)) = found else { return Ok(false) };
        if !for_instance(invite)
            && matches!(kind, ItemKind::Occurrence | ItemKind::Exception)
            && let Some(master) = self.client.master_of(&id).await?
        {
            id = master.id;
        }
        self.client.respond(&id, reply, None, true).await?;
        // Ответ меняет встречу (у «Отклонить» — убирает): эту серию — перечитать.
        let uid = self.items.get(&id.id).map(|k| k.event.uid.clone()).unwrap_or_else(|| ev.uid.clone());
        self.items.retain(|item, k| *item != id.id && !(!uid.is_empty() && k.event.uid.eq_ignore_ascii_case(&uid)));
        Ok(true)
    }

    /// Люди справочника по строке страницей с `offset` и есть ли ещё: `FindPeople`, сервер его не знает —
    /// `ResolveNames` (и дальше — только он; у него страниц нет — всё найденное сразу).
    pub async fn find_people(&mut self, text: &str, offset: u32) -> Result<(Vec<Principal>, bool), Error> {
        if text.trim().chars().count() < PEOPLE_FROM {
            return Ok((Vec::new(), false));
        }
        if self.find_people {
            match self.client.search_people(text, offset, PEOPLE_LIMIT).await {
                Ok(page) => {
                    let more = page.more();
                    return Ok((page.people.into_iter().map(principal).collect(), more));
                }
                // Отказ сервера (нет операции, схема) — не сеть: дальше — `ResolveNames`.
                Err(e) if e.code().is_some() => {
                    tracing::info!(code = e.code().unwrap_or(""), "exchange: FindPeople refused, using ResolveNames");
                    self.find_people = false;
                }
                Err(e) => return Err(e),
            }
        }
        if offset > 0 {
            return Ok((Vec::new(), false));
        }
        Ok((self.client.resolve_names(text, PEOPLE_LIMIT).await?.into_iter().map(principal).collect(), false))
    }

    /// Команды областей тем же договором событий, что у JMAP; чужая — назад.
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
                Ok((_, uid)) => {
                    notify(SyncEvent::MeetingCreated { ticket, uid });
                    notify(SyncEvent::CalendarChanged);
                }
                Err(e) => notify(SyncEvent::MeetingFailed { ticket, error: e.to_string() }),
            },
            Command::UpdateMeeting { event, meeting, series, ticket } => match self.update_meeting(&event, &meeting, series).await {
                Ok(true) => {
                    notify(SyncEvent::MeetingUpdated { ticket, uid: event.uid.clone() });
                    notify(SyncEvent::CalendarChanged);
                }
                Ok(false) => notify(SyncEvent::MeetingFailed { ticket, error: "the meeting is not in the calendar".into() }),
                Err(e) => notify(SyncEvent::MeetingFailed { ticket, error: e.to_string() }),
            },
            Command::CancelMeeting { event, series, ticket, .. } => match self.cancel(&event, series).await {
                Ok(true) => {
                    notify(SyncEvent::MeetingCancelled { ticket, uid: event.uid.clone() });
                    notify(SyncEvent::CalendarChanged);
                }
                Ok(false) => notify(SyncEvent::MeetingFailed { ticket, error: "the meeting is not in the calendar".into() }),
                Err(e) => notify(SyncEvent::MeetingFailed { ticket, error: e.to_string() }),
            },
            Command::PersonDetails { email } => {
                let person = self.person(&email).await.unwrap_or_else(|e| {
                    tracing::debug!("ews person: {e}");
                    None
                });
                notify(SyncEvent::PersonDetails { email, person });
            }
            Command::Contacts => {
                self.contacts_loaded(0, notify).await;
                // Справочник — поиском с подсказкой (`FindPeople`): GAL целиком не читаем.
                notify(SyncEvent::DirectoryLoaded(Vec::new()));
            }
            Command::DirectoryPage { account, offset, .. } => {
                notify(SyncEvent::DirectoryPage { account, offset, people: Vec::new(), total: None, search_only: true });
            }
            Command::SaveContact { card, old, ticket } => match self.save_contact(&card, old.as_deref()).await {
                Ok(id) => {
                    notify(SyncEvent::ContactSaved { ticket, id });
                    self.contacts_loaded(ticket, notify).await;
                }
                Err(e) => notify(SyncEvent::ContactFailed { ticket, error: e.to_string() }),
            },
            Command::DeleteContact { id, ticket } => match self.client.delete_contacts(&[ItemId::new(id.clone())]).await {
                Ok(()) => {
                    notify(SyncEvent::ContactDeleted { ticket, id });
                    self.contacts_loaded(ticket, notify).await;
                }
                Err(e) => notify(SyncEvent::ContactFailed { ticket, error: e.to_string() }),
            },
            Command::FindPeople { text, offset } => match self.find_people(&text, offset).await {
                Ok((people, more)) => notify(SyncEvent::PeopleFound { text, offset, people, more }),
                Err(e) => {
                    tracing::debug!("ews find people: {e}");
                    notify(SyncEvent::PeopleFound { text, offset, people: Vec::new(), more: false });
                }
            },
            // Занятость в Exchange открывает политика сервера, а не владелец календаря.
            Command::ShareFreeBusy { .. } => notify(SyncEvent::CalendarFailed("Exchange: free/busy sharing is set by the server".into())),
            other => return Some(other),
        }
        None
    }
}

impl Areas for EwsAreas {
    fn features(&self) -> Features {
        EwsAreas::features(self)
    }

    fn handle<'a>(&'a mut self, cmd: Command, ctx: AreaCtx<'a>) -> BoxFuture<'a, Option<Command>> {
        Box::pin(self.command(cmd, ctx))
    }

    fn respond<'a>(&'a mut self, invite: &'a Invitation, status: Partstat) -> BoxFuture<'a, postman_core::model::Result<bool>> {
        Box::pin(async move { self.answer(invite, status).await.map_err(crate::core_error) })
    }

    /// Задач по EWS у Postman нет (`Features` их не объявляет) — шагу поручения нечего отправлять.
    fn flush_tasks<'a>(&'a mut self, _ctx: AreaCtx<'a>) -> BoxFuture<'a, postman_core::model::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn changed(&mut self) -> BoxFuture<'_, postman_core::model::Result<bool>> {
        Box::pin(async move { self.poll().await.map_err(crate::core_error) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meeting(attendees: Vec<(String, String)>) -> Meeting {
        Meeting {
            uid: "u1".into(),
            title: "Обзор".into(),
            location: "Зал".into(),
            description: String::new(),
            start: "2026-11-02T10:00:00".parse().unwrap(),
            time_zone: "Europe/Moscow".into(),
            duration_secs: 3600,
            attendees,
            optional: vec![("Вера".into(), "vera@example.test".into())],
            from: None,
        }
    }

    #[test]
    fn meeting_becomes_create_item() {
        let it = new_item(&meeting(vec![("Борис".into(), "boris@example.test".into())])).unwrap();
        assert_eq!(it.span.len(), 3600);
        assert_eq!(it.span.start, "2026-11-02T07:00:00Z".parse::<jiff::Timestamp>().unwrap().as_second());
        assert_eq!((it.required[0].email.as_str(), it.optional[0].email.as_str()), ("boris@example.test", "vera@example.test"));
        assert_eq!(it.time_zone.as_deref(), Some("Europe/Moscow"));
        assert_eq!(notify_for(&it), Notify::AllAndSaveCopy);
        let alone = NewItem { required: vec![], optional: vec![], ..it };
        assert_eq!(notify_for(&alone), Notify::None, "своё событие — без рассылки");
        assert!(bad_time_zone(&Error::Ews { code: "ErrorTimeZone".into(), message: String::new() }));
    }

    /// Карточка ↔ контакт Exchange (T1-25в): поля модели — свои, остальное (имя, фамилия, отдел, заметки, виды
    /// телефонов) — с сервера; списки рассылки — не карточки.
    #[test]
    fn contact_cards() {
        use postman_ews::{Contact, ContactData, Phone, PhoneKind};
        let data = ContactData {
            display_name: "Петрова Анна".into(),
            given_name: "Анна".into(),
            surname: "Петрова".into(),
            emails: vec!["anna@example.test".into()],
            phones: vec![Phone { kind: PhoneKind::Mobile, number: "+7 900".into() }],
            company: "Пример".into(),
            job_title: "Инженер".into(),
            department: "Разработка".into(),
            notes: "Коллега.".into(),
        };
        let c = card(Contact { id: ItemId::new("AAMk1"), folder_id: None, data: data.clone(), members: None }).unwrap();
        assert_eq!((c.id.as_str(), c.name.as_str(), c.organization.as_str(), c.title.as_str()), ("AAMk1", "Петрова Анна", "Пример", "Инженер"));
        assert_eq!(c.phones, ["+7 900"]);
        assert!(card(Contact { id: ItemId::new("AAMk2"), folder_id: None, data: data.clone(), members: Some(vec![]) }).is_none(), "список рассылки");

        let edited = ContactCard { phones: vec!["+7 901".into(), "+7 495".into()], title: "Ведущий".into(), ..c };
        let d = contact_data(&edited, Some(&data));
        assert_eq!((d.given_name.as_str(), d.department.as_str(), d.notes.as_str()), ("Анна", "Разработка", "Коллега."), "прежние поля");
        assert_eq!(d.phones.iter().map(|p| p.kind).collect::<Vec<_>>(), [PhoneKind::Mobile, PhoneKind::Business], "вид на месте, новый — рабочий");
        assert_eq!(d.job_title, "Ведущий");
        let new =
            contact_data(&ContactCard { name: " Борис ".into(), emails: vec![" ".into(), "b@example.test".into()], ..Default::default() }, None);
        assert_eq!((new.display_name.as_str(), new.emails.as_slice()), ("Борис", ["b@example.test".to_owned()].as_slice()));
    }

    #[test]
    fn replies_and_people() {
        assert_eq!(reply(Partstat::Tentative), Some(Reply::Tentative));
        assert_eq!(reply(Partstat::NeedsAction), None);
        let p = principal(postman_ews::Person {
            name: "Анна".into(),
            email: "Anna@example.test".into(),
            title: "Инженер".into(),
            department: "Отдел разработки".into(),
            phone: "+7 495 000-00-01".into(),
            ..Default::default()
        });
        assert_eq!(
            (p.id.as_str(), p.display_name(), p.email.as_deref(), p.kind.as_str()),
            ("anna@example.test", "Анна", Some("Anna@example.test"), "individual")
        );
        assert_eq!((p.title.as_str(), p.department.as_str(), p.phone.as_str()), ("Инженер", "Отдел разработки", "+7 495 000-00-01"));
        let g = principal(postman_ews::Person { email: "dev@example.test".into(), group: true, ..Default::default() });
        assert_eq!((g.display_name(), g.kind.as_str()), ("dev@example.test", "group"));
    }

    #[test]
    fn availability_is_mine_then_theirs() {
        let busy = |s| vec![Busy { span: Span::new(s, s + 60), kind: postman_calendar::BusyKind::Busy }];
        let a = |busy, error: Option<&str>| postman_ews::Availability { email: String::new(), busy, error: error.map(str::to_owned) };
        let (me, people) = split_availability(
            vec![a(busy(1), None), a(busy(2), None), a(Vec::new(), Some("ErrorMailRecipientNotFound"))],
            &["b@example.test".into(), "x@example.test".into()],
        );
        assert_eq!(me, busy(1));
        assert_eq!(people, [("b@example.test".to_owned(), Some(busy(2))), ("x@example.test".to_owned(), None)]);
    }

    fn ev(uid: &str, start: i64) -> Event {
        Event {
            id: String::new(),
            uid: uid.into(),
            calendar_ids: Vec::new(),
            title: String::new(),
            description: String::new(),
            location: String::new(),
            span: Span::new(start, start + 600),
            all_day: false,
            status: EventStatus::Confirmed,
            free_busy: postman_calendar::FreeBusy::Busy,
            participants: Vec::new(),
            recurring: true,
            keywords: Vec::new(),
            sequence: 0,
            owner: None,
        }
    }

    /// Встреча для ответа: та же по id, иначе по UID — одиночная или серия, затем экземпляр ближе ко времени.
    #[test]
    fn invite_finds_its_meeting() {
        let (a, b, c, other) = (ev("UID-1", 1000), ev("uid-1", 5000), ev("UID-1", 9000), ev("UID-2", 5000));
        let known = || {
            vec![
                ("occ-a", ItemKind::Occurrence, &a),
                ("exc-b", ItemKind::Exception, &b),
                ("occ-c", ItemKind::Occurrence, &c),
                ("x", ItemKind::Single, &other),
            ]
        };
        assert_eq!(pick(known(), "", "UID-1", 8000), Some(("occ-c", ItemKind::Occurrence)), "экземпляр — ближайший ко времени");
        assert_eq!(pick(known(), "", "uid-1", 4000), Some(("exc-b", ItemKind::Exception)), "UID — без учёта регистра");
        assert_eq!(pick(known(), "occ-a", "UID-1", 9000), Some(("occ-a", ItemKind::Occurrence)), "из подробностей события — та же");
        assert_eq!(pick(known(), "", "UID-3", 0), None, "нет встречи — ответ письмом");
        assert_eq!(pick(known(), "", "", 0), None, "без UID — не угадывать");
        let single = ev("UID-1", 0);
        let mut list = known();
        list.push(("one", ItemKind::Single, &single));
        assert_eq!(pick(list, "", "UID-1", 9000), Some(("one", ItemKind::Single)), "одиночная — точнее экземпляра");

        let inv = |raw: serde_json::Value| Invitation { method: "REQUEST".into(), event: ev("UID-1", 0), sequence: 0, raw };
        assert!(for_instance(&inv(serde_json::json!({ "uid": "UID-1", "recurrenceId": "2026-11-02T10:00:00" }))));
        assert!(!for_instance(&inv(serde_json::json!({ "uid": "UID-1" }))));
    }

    #[test]
    fn reschedule_changes() {
        let ch = moved(Span::new(3600, 7200), "Europe/Moscow");
        assert_eq!(ch.time, Some((Span::new(3600, 7200), false)));
        assert_eq!(ch.time_zone.as_deref(), Some("Europe/Moscow"));
        assert!(ch.title.is_none() && ch.required.is_none());
    }
    /// Правка вкладкой «Встреча»: экземпляр перенесён на час — у серии (её время раньше) тот же сдвиг, длительность новая.
    #[test]
    fn edit_shifts_the_target() {
        let item = new_item(&meeting(vec![("Борис".into(), "boris@example.test".into())])).unwrap();
        let was = Span::new(item.span.start - 3600, item.span.start - 1800);
        let series = Span::new(was.start - 7 * 86_400, was.start - 7 * 86_400 + 1800);
        let ch = edit_changes(item.clone(), was, series);
        assert_eq!(ch.time, Some((Span::new(series.start + 3600, series.start + 3600 + 3600), false)));
        assert_eq!(ch.title.as_deref(), Some("Обзор"));
        assert_eq!(ch.required.as_ref().map(|r| r[0].email.as_str()), Some("boris@example.test"));
        assert_eq!(ch.optional.as_ref().map(Vec::len), Some(1));
        let same = edit_changes(item.clone(), item.span, item.span);
        assert_eq!(same.time, Some((item.span, false)), "одиночная — новое время как есть");
    }
}
