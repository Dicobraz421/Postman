//! JMAP for Calendars (draft-ietf-jmap-calendars) и Principals (RFC 9670).
//! События возвращаются как JSON (JSCalendar) — разбор в `postman-calendar`.
//! Имена методов и аргументов — по черновику; поведение Stalwart проверено на стенде (`docs/STALWART.md`,
//! `crates/core/tests/stand.rs`).

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::mail::{GetResponse, inv, set_error};
use crate::{CALENDARS, CORE, Client, Error, PRINCIPALS, PRINCIPALS_AVAILABILITY, Result, take};

const USING: &[&str] = &[CORE, CALENDARS, PRINCIPALS];

/// Расширение The Post: занятость по адресам, в том числе внешним (`Availability/get`). У других серверов его нет.
pub const AVAILABILITY: &str = "urn:thepost:availability";
/// Расширение The Post: ссылки публикации календаря (`CalendarPublication/get`, `/set`).
pub const CALENDAR_PUBLISH: &str = "urn:thepost:calendarpublish";

/// Свойства события, нужные для отображения; `utcStart`/`utcEnd` вычисляет сервер.
pub const EVENT_PROPERTIES: &[&str] = &[
    "id",
    "uid",
    "calendarIds",
    "title",
    "description",
    "start",
    "timeZone",
    "duration",
    "showWithoutTime",
    "status",
    "freeBusyStatus",
    "participants",
    "locations",
    "recurrenceRules",
    "recurrenceRule",
    "recurrenceOverrides",
    "recurrenceId",
    "utcStart",
    "utcEnd",
    "keywords",
    "categories",
    "sequence",
    "alerts",
    "useDefaultAlerts",
];

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Calendar {
    pub id: String,
    pub name: String,
    pub color: Option<String>,
    #[serde(default)]
    pub sort_order: u32,
    #[serde(default = "yes")]
    pub is_visible: bool,
    #[serde(default)]
    pub is_subscribed: bool,
    /// Кому открыт: принципал → права (`shareWith`); нет — никому.
    #[serde(default)]
    pub share_with: Option<std::collections::HashMap<String, Rights>>,
}

/// Права на календарь у принципала.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rights {
    #[serde(default)]
    pub may_read_items: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Principal {
    pub id: String,
    /// У Stalwart — логин (адрес), а не ФИО (`docs/STALWART.md` п. 9); показывать — [`Principal::display_name`].
    #[serde(default)]
    pub name: String,
    /// У Stalwart — ФИО учётной записи («Test user1»).
    #[serde(default)]
    pub description: Option<String>,
    pub email: Option<String>,
    /// individual, group, resource, location…
    #[serde(rename = "type", default)]
    pub kind: String,
}

impl Principal {
    /// Имя для людей: `description` (ФИО), а если его нет — `name`.
    pub fn display_name(&self) -> &str {
        match self.description.as_deref().map(str::trim) {
            Some(d) if !d.is_empty() => d,
            _ => &self.name,
        }
    }
}

/// Ответ `Availability/get` по адресу: `busy_periods: None` — нет данных, это не «свободен».
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddressAvailability {
    pub address: String,
    #[serde(default)]
    pub busy_periods: Option<Vec<BusyPeriod>>,
}

/// Ссылка публикации календаря. Адрес сервер отдаёт только при создании: хранит он хэш токена.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Publication {
    pub id: String,
    /// `freeBusy` или `details`.
    pub detail: String,
}

/// Период занятости из `Principal/getAvailability`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BusyPeriod {
    pub utc_start: String,
    pub utc_end: String,
    /// confirmed | tentative | unavailable
    #[serde(default)]
    pub busy_status: Option<String>,
}

#[derive(Deserialize)]
struct AvailabilityResponse {
    list: Vec<BusyPeriod>,
}

/// Ответ `Availability/get`: не стандартный `/get`, состояния в нём нет.
#[derive(Deserialize)]
struct AddressesResponse {
    list: Vec<AddressAvailability>,
}

/// Идентификаторы участников из `members` принципала: список идентификаторов или словарь `id → true` (как множества
/// JMAP). Свойства нет или оно другого вида — `None`: сервер состав не отдаёт.
fn member_ids(principal: &Value) -> Option<Vec<String>> {
    match principal.get("members")? {
        Value::Array(ids) => Some(ids.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()),
        Value::Object(set) => Some(set.iter().filter(|(_, on)| on.as_bool() == Some(true)).map(|(id, _)| id.clone()).collect()),
        _ => None,
    }
}

impl Client {
    pub fn calendar_account(&self) -> Result<&str> {
        self.account_for(CALENDARS)
    }

    pub async fn calendars(&self) -> Result<Vec<Calendar>> {
        let acc = self.calendar_account()?.to_owned();
        self.calendars_of(&acc).await
    }

    /// Календари аккаунта `acc`: свой или открытый мне коллегой ([`crate::Session::shared_calendar_accounts`], S5-22).
    pub async fn calendars_of(&self, acc: &str) -> Result<Vec<Calendar>> {
        let mut r = self.call(USING, &[inv("Calendar/get", json!({ "accountId": acc, "ids": null }), "c")]).await?;
        let g: GetResponse<Calendar> = take(&mut r, "c")?;
        Ok(g.list)
    }

    /// События в интервале (UTC, `YYYY-MM-DDTHH:MM:SSZ`), повторения разворачивает сервер.
    pub async fn events(&self, calendar_ids: Option<&[String]>, utc_after: &str, utc_before: &str) -> Result<Vec<Value>> {
        let acc = self.calendar_account()?.to_owned();
        self.events_of(&acc, calendar_ids, utc_after, utc_before).await
    }

    /// События аккаунта `acc` в окне времени (свой или открытый мне календарь коллеги, S5-22).
    pub async fn events_of(&self, acc: &str, calendar_ids: Option<&[String]>, utc_after: &str, utc_before: &str) -> Result<Vec<Value>> {
        let mut filter = json!({ "after": utc_after, "before": utc_before });
        if let Some(ids) = calendar_ids {
            filter["inCalendars"] = json!(ids);
        }
        let calls = [
            inv(
                "CalendarEvent/query",
                json!({ "accountId": acc, "filter": filter, "expandRecurrences": true, "sort": [{ "property": "start" }] }),
                "q",
            ),
            inv(
                "CalendarEvent/get",
                json!({
                    "accountId": acc,
                    "#ids": { "resultOf": "q", "name": "CalendarEvent/query", "path": "/ids" },
                    "properties": EVENT_PROPERTIES,
                }),
                "g",
            ),
        ];
        let mut r = self.call(USING, &calls).await?;
        let g: GetResponse<Value> = take(&mut r, "g")?;
        Ok(g.list)
    }

    /// Поиск событий по тексту (RFC 8984 / JMAP Calendars: `filter.text` — тема, описание, место, участники) в
    /// интервале (UTC), повторения разворачивает сервер (S5-21) [проверить на стенде: `stand_calendar_search`].
    pub async fn search_events(&self, text: &str, utc_after: &str, utc_before: &str, limit: usize) -> Result<Vec<Value>> {
        let acc = self.calendar_account()?;
        let filter = json!({ "text": text, "after": utc_after, "before": utc_before });
        let calls = [
            inv(
                "CalendarEvent/query",
                json!({ "accountId": acc, "filter": filter, "expandRecurrences": true, "sort": [{ "property": "start" }], "limit": limit }),
                "q",
            ),
            inv(
                "CalendarEvent/get",
                json!({
                    "accountId": acc,
                    "#ids": { "resultOf": "q", "name": "CalendarEvent/query", "path": "/ids" },
                    "properties": EVENT_PROPERTIES,
                }),
                "g",
            ),
        ];
        let mut r = self.call(USING, &calls).await?;
        let g: GetResponse<Value> = take(&mut r, "g")?;
        Ok(g.list)
    }

    /// Создать событие; `send` — разослать приглашения (iTIP/iMIP силами сервера).
    pub async fn create_event(&self, event: Value, send: bool) -> Result<String> {
        let acc = self.calendar_account()?;
        let args = json!({ "accountId": acc, "create": { "new": event }, "sendSchedulingMessages": send });
        let mut r = self.call(USING, &[inv("CalendarEvent/set", args, "s")]).await?;
        let v: Value = take(&mut r, "s")?;
        set_error(&v, "notCreated", "new", "CalendarEvent/set")?;
        v["created"]["new"]["id"].as_str().map(str::to_string).ok_or_else(|| Error::Protocol("CalendarEvent/set не вернул id".into()))
    }

    /// Событие по UID (приглашение, пришедшее письмом, могло уже лечь в календарь сервером).
    /// У приглашённого своё id события — искать только так (фильтр `uid` черновика; Stalwart — да).
    /// Фильтр `uid` у Stalwart — по подстроке: берём несколько и сверяем uid целиком (иначе первым может прийти
    /// другое событие с похожим uid).
    pub async fn event_by_uid(&self, uid: &str) -> Result<Option<Value>> {
        let acc = self.calendar_account()?;
        let calls = [
            inv("CalendarEvent/query", json!({ "accountId": acc, "filter": { "uid": uid }, "limit": 50 }), "q"),
            inv(
                "CalendarEvent/get",
                json!({
                    "accountId": acc,
                    "#ids": { "resultOf": "q", "name": "CalendarEvent/query", "path": "/ids" },
                    "properties": EVENT_PROPERTIES,
                }),
                "g",
            ),
        ];
        let mut r = self.call(USING, &calls).await?;
        let g: GetResponse<Value> = take(&mut r, "g")?;
        Ok(g.list.into_iter().find(|e| e["uid"].as_str() == Some(uid)))
    }

    /// Событие по id — у экземпляра повтора (id из `expandRecurrences`) с `recurrenceId`; нет такого — `None`.
    pub async fn event_by_id(&self, id: &str) -> Result<Option<Value>> {
        let acc = self.calendar_account()?;
        let args = json!({ "accountId": acc, "ids": [id], "properties": EVENT_PROPERTIES });
        let mut r = self.call(USING, &[inv("CalendarEvent/get", args, "g")]).await?;
        let g: GetResponse<Value> = take(&mut r, "g")?;
        Ok(g.list.into_iter().next())
    }

    /// Изменить событие патчем (`"title": …`, `"participants/<id>/participationStatus": …`).
    pub async fn update_event(&self, id: &str, patch: Map<String, Value>, send: bool) -> Result<()> {
        let acc = self.calendar_account()?;
        let args = json!({ "accountId": acc, "update": { id: patch }, "sendSchedulingMessages": send });
        let mut r = self.call(USING, &[inv("CalendarEvent/set", args, "s")]).await?;
        let v: Value = take(&mut r, "s")?;
        set_error(&v, "notUpdated", id, "CalendarEvent/set")
    }

    /// Удалить событие; `send` — организатор рассылает отмену (CANCEL), участник — отказ.
    pub async fn destroy_event(&self, id: &str, send: bool) -> Result<()> {
        let acc = self.calendar_account()?;
        let args = json!({ "accountId": acc, "destroy": [id], "sendSchedulingMessages": send });
        let mut r = self.call(USING, &[inv("CalendarEvent/set", args, "s")]).await?;
        let v: Value = take(&mut r, "s")?;
        set_error(&v, "notDestroyed", id, "CalendarEvent/set")
    }

    /// Открыть календарь сотруднику (`principal_id`): только занятость или ещё и события.
    /// Без этого `Principal/getAvailability` у него — пустой список без ошибки (Stalwart).
    pub async fn share_calendar(&self, calendar_id: &str, principal_id: &str, items: bool) -> Result<()> {
        let acc = self.calendar_account()?;
        let mut patch = Map::new();
        patch.insert(format!("shareWith/{principal_id}"), json!({ "mayReadFreeBusy": true, "mayReadItems": items }));
        let args = json!({ "accountId": acc, "update": { calendar_id: patch } });
        let mut r = self.call(USING, &[inv("Calendar/set", args, "s")]).await?;
        let v: Value = take(&mut r, "s")?;
        set_error(&v, "notUpdated", calendar_id, "Calendar/set")
    }

    /// Ответ на приглашение: accepted | tentative | declined.
    /// Закрыть календарь для `principal_id` (`shareWith/<id>: null`).
    pub async fn unshare_calendar(&self, calendar_id: &str, principal_id: &str) -> Result<()> {
        let acc = self.calendar_account()?;
        let mut patch = Map::new();
        patch.insert(format!("shareWith/{principal_id}"), Value::Null);
        let args = json!({ "accountId": acc, "update": { calendar_id: patch } });
        let mut r = self.call(USING, &[inv("Calendar/set", args, "s")]).await?;
        let v: Value = take(&mut r, "s")?;
        set_error(&v, "notUpdated", calendar_id, "Calendar/set")
    }

    pub async fn reply(&self, event_id: &str, participant_id: &str, status: &str, send: bool) -> Result<()> {
        let mut patch = Map::new();
        patch.insert(format!("participants/{participant_id}/participationStatus"), json!(status));
        self.update_event(event_id, patch, send).await
    }

    /// Занятость по адресам, расширение The Post (S8-46): источник выбирает сервер.
    pub async fn address_availability(&self, addresses: &[String], utc_start: &str, utc_end: &str) -> Result<Vec<AddressAvailability>> {
        let acc = self.account_for(AVAILABILITY)?;
        let args = json!({ "accountId": acc, "addresses": addresses, "utcStart": utc_start, "utcEnd": utc_end });
        let mut r = self.call(&[CORE, CALENDARS, AVAILABILITY], &[inv("Availability/get", args, "a")]).await?;
        let a: AddressesResponse = take(&mut r, "a")?;
        Ok(a.list)
    }

    /// Ссылки публикации моих календарей (S8-45).
    pub async fn publications(&self) -> Result<Vec<Publication>> {
        let acc = self.account_for(CALENDAR_PUBLISH)?;
        let args = json!({ "accountId": acc, "ids": null });
        let mut r = self.call(&[CORE, CALENDARS, CALENDAR_PUBLISH], &[inv("CalendarPublication/get", args, "p")]).await?;
        let p: GetResponse<Publication> = take(&mut r, "p")?;
        Ok(p.list)
    }

    /// Создать ссылку публикации календаря → адрес: больше сервер его не покажет.
    pub async fn publish(&self, calendar_id: &str, detail: &str) -> Result<String> {
        let acc = self.account_for(CALENDAR_PUBLISH)?;
        let args = json!({ "accountId": acc, "create": { "new": { "calendarId": calendar_id, "detail": detail } } });
        let mut r = self.call(&[CORE, CALENDARS, CALENDAR_PUBLISH], &[inv("CalendarPublication/set", args, "s")]).await?;
        let v: Value = take(&mut r, "s")?;
        set_error(&v, "notCreated", "new", "CalendarPublication/set")?;
        v["created"]["new"]["url"].as_str().map(str::to_string).ok_or_else(|| Error::Protocol("CalendarPublication/set не вернул url".into()))
    }

    /// Отозвать ссылку публикации.
    pub async fn unpublish(&self, id: &str) -> Result<()> {
        let acc = self.account_for(CALENDAR_PUBLISH)?;
        let args = json!({ "accountId": acc, "destroy": [id] });
        let mut r = self.call(&[CORE, CALENDARS, CALENDAR_PUBLISH], &[inv("CalendarPublication/set", args, "s")]).await?;
        let v: Value = take(&mut r, "s")?;
        set_error(&v, "notDestroyed", id, "CalendarPublication/set")
    }

    /// Принципалы по идентификаторам: кому открыт календарь (S8-43).
    pub async fn principals_by_id(&self, ids: &[String]) -> Result<Vec<Principal>> {
        let acc = self.account_for(PRINCIPALS)?;
        let mut r = self.call(USING, &[inv("Principal/get", json!({ "accountId": acc, "ids": ids }), "g")]).await?;
        let g: GetResponse<Principal> = take(&mut r, "g")?;
        Ok(g.list)
    }

    /// Поиск сотрудников, переговорных и ресурсов (для приглашений и помощника).
    pub async fn principals(&self, text: &str, limit: u64) -> Result<Vec<Principal>> {
        let acc = self.account_for(PRINCIPALS)?;
        let calls = [
            inv("Principal/query", json!({ "accountId": acc, "filter": { "text": text }, "limit": limit }), "q"),
            inv("Principal/get", json!({ "accountId": acc, "#ids": { "resultOf": "q", "name": "Principal/query", "path": "/ids" } }), "g"),
        ];
        let mut r = self.call(USING, &calls).await?;
        let g: GetResponse<Principal> = take(&mut r, "g")?;
        Ok(g.list)
    }

    /// Найденные по строке страницей с `position` (подгрузка по прокрутке): люди и есть ли ещё (`total`
    /// `Principal/query`).
    pub async fn principals_page(&self, text: &str, position: u64, limit: u64) -> Result<(Vec<Principal>, bool)> {
        let acc = self.account_for(PRINCIPALS)?;
        let query = json!({ "accountId": acc, "filter": { "text": text }, "position": position, "limit": limit, "calculateTotal": true });
        let calls = [
            inv("Principal/query", query, "q"),
            inv("Principal/get", json!({ "accountId": acc, "#ids": { "resultOf": "q", "name": "Principal/query", "path": "/ids" } }), "g"),
        ];
        let mut r = self.call(USING, &calls).await?;
        let q: Value = take(&mut r, "q")?;
        let g: GetResponse<Principal> = take(&mut r, "g")?;
        let shown = q["ids"].as_array().map_or(0, Vec::len) as u64;
        let more = q["total"].as_u64().is_some_and(|t| position + shown < t);
        Ok((g.list, more))
    }

    /// Справочник: все сотрудники, переговорные и ресурсы (`Principal/query` без фильтра), не больше `limit`.
    pub async fn directory(&self, limit: u64) -> Result<Vec<Principal>> {
        let acc = self.account_for(PRINCIPALS)?;
        let calls = [
            inv("Principal/query", json!({ "accountId": acc, "limit": limit }), "q"),
            inv("Principal/get", json!({ "accountId": acc, "#ids": { "resultOf": "q", "name": "Principal/query", "path": "/ids" } }), "g"),
        ];
        let mut r = self.call(USING, &calls).await?;
        let g: GetResponse<Principal> = take(&mut r, "g")?;
        Ok(g.list)
    }

    /// Участники группы справочника по её адресу (T3-09): свойство `members` у `Principal` (RFC 9670 его не
    /// описывает — отдают не все серверы), затем сами участники по идентификаторам; вложенная группа — участник
    /// с `type: group`. `None` — группы по адресу нет или сервер состав не отдаёт.
    pub async fn group_members(&self, email: &str) -> Result<Option<Vec<Principal>>> {
        let acc = self.account_for(PRINCIPALS)?;
        let calls = [
            inv("Principal/query", json!({ "accountId": acc, "filter": { "email": email }, "limit": 5 }), "q"),
            inv("Principal/get", json!({ "accountId": acc, "#ids": { "resultOf": "q", "name": "Principal/query", "path": "/ids" } }), "g"),
        ];
        let mut r = self.call(USING, &calls).await?;
        let g: GetResponse<Value> = take(&mut r, "g")?;
        let Some(ids) = g.list.iter().find(|p| p["email"].as_str().is_some_and(|e| e.eq_ignore_ascii_case(email.trim()))).and_then(member_ids) else {
            return Ok(None);
        };
        if ids.is_empty() {
            return Ok(Some(Vec::new()));
        }
        let found = self.principals_by_id(&ids).await?;
        // Порядок — как в `members`: `Principal/get` его не обещает.
        Ok(Some(ids.iter().filter_map(|id| found.iter().find(|p| &p.id == id).cloned()).collect()))
    }

    /// Сотрудник по адресу (фильтр `email`).
    pub async fn principal_by_email(&self, email: &str) -> Result<Option<Principal>> {
        let acc = self.account_for(PRINCIPALS)?;
        let calls = [
            inv("Principal/query", json!({ "accountId": acc, "filter": { "email": email }, "limit": 5 }), "q"),
            inv("Principal/get", json!({ "accountId": acc, "#ids": { "resultOf": "q", "name": "Principal/query", "path": "/ids" } }), "g"),
        ];
        let mut r = self.call(USING, &calls).await?;
        let g: GetResponse<Principal> = take(&mut r, "g")?;
        Ok(g.list.into_iter().find(|p| p.email.as_deref().is_some_and(|e| e.eq_ignore_ascii_case(email))))
    }

    /// Занятость сотрудника в интервале — без подробностей событий. Пусто, если календарь не открыт
    /// (`share_calendar`): ошибки сервер не даёт.
    pub async fn availability(&self, principal_id: &str, utc_start: &str, utc_end: &str) -> Result<Vec<BusyPeriod>> {
        let acc = self.account_for(PRINCIPALS)?;
        let args = json!({ "accountId": acc, "id": principal_id, "utcStart": utc_start, "utcEnd": utc_end, "showDetails": false });
        let using = [CORE, CALENDARS, PRINCIPALS, PRINCIPALS_AVAILABILITY];
        let mut r = self.call(&using, &[inv("Principal/getAvailability", args, "a")]).await?;
        let a: AvailabilityResponse = take(&mut r, "a")?;
        Ok(a.list)
    }
}

#[cfg(test)]
mod share_tests {
    #![allow(clippy::unwrap_used)]

    use super::{AddressesResponse, Calendar, Publication, member_ids};

    /// Состав группы (T3-09): список или множество идентификаторов; свойства нет или `null` — не поддерживается.
    #[test]
    fn group_member_ids() {
        let ids = |v: serde_json::Value| member_ids(&v);
        assert_eq!(ids(serde_json::json!({ "id": "g", "members": ["a", "b"] })), Some(vec!["a".to_owned(), "b".to_owned()]));
        assert_eq!(ids(serde_json::json!({ "id": "g", "members": { "a": true, "b": false } })), Some(vec!["a".to_owned()]));
        assert_eq!(ids(serde_json::json!({ "id": "g", "members": [] })), Some(Vec::new()));
        assert_eq!(ids(serde_json::json!({ "id": "g", "type": "group" })), None);
        assert_eq!(ids(serde_json::json!({ "id": "g", "members": null })), None);
    }

    /// `Availability/get`: пустой список — свободен, `null` — нет данных (S8-46).
    #[test]
    fn address_availability_keeps_no_data() {
        let v = serde_json::json!({ "list": [
            { "address": "a@x.test", "source": "organization", "detail": "freeBusy", "busyPeriods": [{ "utcStart": "2026-11-05T09:00:00Z", "utcEnd": "2026-11-05T10:00:00Z", "busyStatus": "tentative" }] },
            { "address": "b@x.test", "busyPeriods": [] },
            { "address": "c@x.test", "busyPeriods": null },
        ]});
        let g: AddressesResponse = serde_json::from_value(v).unwrap();
        assert_eq!(g.list[0].busy_periods.as_ref().unwrap()[0].busy_status.as_deref(), Some("tentative"));
        assert!(g.list[1].busy_periods.as_ref().unwrap().is_empty());
        assert!(g.list[2].busy_periods.is_none());
    }

    /// Ссылка публикации: адреса в списке нет.
    #[test]
    fn publication_is_read() {
        let p: Publication =
            serde_json::from_value(serde_json::json!({ "id": "p1", "calendarId": "c", "detail": "freeBusy", "expires": null })).unwrap();
        assert_eq!(p, Publication { id: "p1".into(), detail: "freeBusy".into() });
    }

    /// Кому открыт календарь: `shareWith` с правами; нет свойства или `null` — никому (S8-43).
    #[test]
    fn share_with_is_read() {
        let v = serde_json::json!({ "id": "c", "name": "К", "shareWith": { "p1": { "mayReadFreeBusy": true, "mayReadItems": true }, "p2": { "mayReadFreeBusy": true } } });
        let c: Calendar = serde_json::from_value(v).unwrap();
        let s = c.share_with.unwrap();
        assert!(s["p1"].may_read_items);
        assert!(!s["p2"].may_read_items);
        let c: Calendar = serde_json::from_value(serde_json::json!({ "id": "c", "name": "К", "shareWith": null })).unwrap();
        assert!(c.share_with.is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn availability_shape() {
        let v = json!({ "list": [{ "utcStart": "2026-10-20T07:00:00Z", "utcEnd": "2026-10-20T08:00:00Z", "busyStatus": "tentative" }] });
        let a: AvailabilityResponse = serde_json::from_value(v).unwrap();
        assert_eq!(a.list[0].busy_status.as_deref(), Some("tentative"));
        let c: Calendar = serde_json::from_value(json!({ "id": "c1", "name": "Рабочий" })).unwrap();
        assert!(c.is_visible);
    }

    #[test]
    fn principal_shows_description_then_name() {
        let p = |v: Value| serde_json::from_value::<Principal>(v).unwrap();
        // Stalwart: `name` — логин, ФИО — в `description`.
        let full = p(json!({ "id": "a", "type": "individual", "name": "user1@itt.test", "description": "Test user1", "email": "user1@itt.test" }));
        assert_eq!(full.display_name(), "Test user1");
        assert_eq!(p(json!({ "id": "b", "name": "user2@itt.test", "description": "  " })).display_name(), "user2@itt.test");
        assert_eq!(p(json!({ "id": "c", "name": "admin", "description": null })).display_name(), "admin");
    }
}
