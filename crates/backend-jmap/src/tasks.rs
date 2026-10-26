//! Задачи по JMAP (S6-02): `CalendarEvent` с `@type: Task` (решение владельца 2026-10-22, S6-01,
//! `docs/STALWART.md` п. 37–40) — хранилище `postman_core::tasks::TaskStore`; дельта — `CalendarEvent/changes`
//! (push календаря — тот же тип). Списки, кэш и сверка — `postman_core::tasks`.

use postman_calendar::{TASK_PROPERTIES, Task};
use postman_core::tasks::{Delta, TaskStore};
use postman_jmap::{CALENDARS, CORE, Client, Error, Invocation, Result};
use serde_json::{Map, Value, json};

use crate::error::core_error;

/// Объектов в одном `CalendarEvent/get` (`maxObjectsInGet` Stalwart — не меньше).
const CHUNK: usize = 500;

/// Задачи в календаре сервера по JMAP (без рассылки: поручения — S6-09).
pub struct JmapStore<'a> {
    client: &'a Client,
}

const USING: &[&str] = &[CORE, CALENDARS];

impl<'a> JmapStore<'a> {
    pub fn new(client: &'a Client) -> Self {
        Self { client }
    }

    fn account(&self) -> Result<String> {
        Ok(self.client.calendar_account()?.to_owned())
    }

    async fn call(&self, calls: Vec<(&str, Value)>) -> Result<Vec<Value>> {
        let inv: Vec<Invocation> = calls.into_iter().enumerate().map(|(i, (m, a))| Invocation(m.into(), a, format!("c{i}"))).collect();
        Ok(self.client.call(USING, &inv).await?.into_iter().map(|r| r.1).collect())
    }

    async fn state(&self) -> Result<String> {
        let acc = self.account()?;
        let r = self.call(vec![("CalendarEvent/get", json!({ "accountId": acc, "ids": [] }))]).await?;
        r[0]["state"].as_str().map(str::to_owned).ok_or_else(|| Error::Protocol("CalendarEvent/get без state".into()))
    }

    /// Только задачи из `ids`: сначала `@type` (дёшево), затем задачи целиком.
    async fn tasks_of(&self, ids: &[String]) -> Result<Vec<Value>> {
        let acc = self.account()?;
        let mut task_ids = Vec::new();
        for chunk in ids.chunks(CHUNK) {
            let r = self.call(vec![("CalendarEvent/get", json!({ "accountId": acc, "ids": chunk, "properties": ["id", "@type"] }))]).await?;
            task_ids.extend(
                r[0]["list"].as_array().into_iter().flatten().filter(|v| Task::is_task(v)).filter_map(|v| v["id"].as_str().map(str::to_owned)),
            );
        }
        self.jmap_get(&task_ids).await
    }

    fn set_failed(v: &Value, field: &str, id: &str) -> Option<Error> {
        let err = v.get(field)?.get(id)?;
        Some(Error::Method {
            method: "CalendarEvent/set".into(),
            kind: err["type"].as_str().unwrap_or("?").into(),
            description: err["description"].as_str().unwrap_or("").into(),
        })
    }
}

/// Запросы хранилища — ошибками JMAP; договору `TaskStore` — нейтральными (ниже).
impl JmapStore<'_> {
    async fn jmap_load(&self) -> Result<(Vec<Value>, String)> {
        let acc = self.account()?;
        let state = self.state().await?;
        let mut ids: Vec<String> = Vec::new();
        loop {
            let r = self.call(vec![("CalendarEvent/query", json!({ "accountId": acc, "position": ids.len(), "limit": CHUNK }))]).await?;
            let page: Vec<String> = r[0]["ids"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_owned)).collect();
            let got = page.len();
            ids.extend(page);
            if got < CHUNK {
                break;
            }
        }
        Ok((self.tasks_of(&ids).await?, state))
    }

    async fn jmap_changes(&self, since: &str) -> Result<Delta> {
        let acc = self.account()?;
        let mut state = since.to_owned();
        let (mut touched, mut destroyed) = (Vec::new(), Vec::new());
        loop {
            let r = self.call(vec![("CalendarEvent/changes", json!({ "accountId": acc, "sinceState": state, "maxChanges": CHUNK }))]).await?;
            let ch = &r[0];
            let ids = |k: &str| ch[k].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_owned)).collect::<Vec<_>>();
            touched.extend(ids("created"));
            touched.extend(ids("updated"));
            destroyed.extend(ids("destroyed"));
            state = ch["newState"].as_str().unwrap_or(&state).to_owned();
            if ch["hasMoreChanges"] != true {
                break;
            }
        }
        touched.sort();
        touched.dedup();
        Ok(Delta { changed: self.tasks_of(&touched).await?, destroyed, state })
    }

    async fn jmap_get(&self, ids: &[String]) -> Result<Vec<Value>> {
        let acc = self.account()?;
        let mut out = Vec::new();
        for chunk in ids.chunks(CHUNK) {
            let r = self.call(vec![("CalendarEvent/get", json!({ "accountId": acc, "ids": chunk, "properties": TASK_PROPERTIES }))]).await?;
            out.extend(r[0]["list"].as_array().into_iter().flatten().filter(|v| Task::is_task(v)).cloned());
        }
        Ok(out)
    }

    async fn jmap_create(&self, task: &Value) -> Result<String> {
        let acc = self.account()?;
        let r = self.call(vec![("CalendarEvent/set", json!({ "accountId": acc, "create": { "t": task }, "sendSchedulingMessages": false }))]).await?;
        if let Some(e) = Self::set_failed(&r[0], "notCreated", "t") {
            return Err(e);
        }
        r[0]["created"]["t"]["id"].as_str().map(str::to_owned).ok_or_else(|| Error::Protocol("CalendarEvent/set не вернул id".into()))
    }

    async fn jmap_update(&self, id: &str, patch: &Map<String, Value>) -> Result<()> {
        let acc = self.account()?;
        let args = json!({ "accountId": acc, "update": { id: patch }, "sendSchedulingMessages": false });
        let r = self.call(vec![("CalendarEvent/set", args)]).await?;
        Self::set_failed(&r[0], "notUpdated", id).map_or(Ok(()), Err)
    }

    async fn jmap_destroy(&self, id: &str) -> Result<()> {
        let acc = self.account()?;
        let args = json!({ "accountId": acc, "destroy": [id], "sendSchedulingMessages": false });
        let r = self.call(vec![("CalendarEvent/set", args)]).await?;
        match Self::set_failed(&r[0], "notDestroyed", id) {
            Some(Error::Method { kind, .. }) if kind == "notFound" => Ok(()),
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    async fn jmap_default_calendar(&self) -> Result<String> {
        let acc = self.account()?;
        let r = self.call(vec![("Calendar/get", json!({ "accountId": acc, "ids": null, "properties": ["id", "isDefault", "sortOrder"] }))]).await?;
        let list = r[0]["list"].as_array().cloned().unwrap_or_default();
        let pick = list.iter().find(|c| c["isDefault"] == true).or_else(|| list.iter().min_by_key(|c| c["sortOrder"].as_u64().unwrap_or(0)));
        pick.and_then(|c| c["id"].as_str()).map(str::to_owned).ok_or_else(|| Error::Protocol("у аккаунта нет календаря".into()))
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Запросы интерфейса и ответы.

impl TaskStore for JmapStore<'_> {
    async fn load(&self) -> postman_core::model::Result<(Vec<Value>, String)> {
        self.jmap_load().await.map_err(core_error)
    }

    async fn changes(&self, since: &str) -> postman_core::model::Result<Delta> {
        self.jmap_changes(since).await.map_err(core_error)
    }

    async fn get(&self, ids: &[String]) -> postman_core::model::Result<Vec<Value>> {
        self.jmap_get(ids).await.map_err(core_error)
    }

    async fn create(&self, task: &Value) -> postman_core::model::Result<String> {
        self.jmap_create(task).await.map_err(core_error)
    }

    async fn update(&self, id: &str, patch: &Map<String, Value>) -> postman_core::model::Result<()> {
        self.jmap_update(id, patch).await.map_err(core_error)
    }

    async fn destroy(&self, id: &str) -> postman_core::model::Result<()> {
        self.jmap_destroy(id).await.map_err(core_error)
    }

    async fn default_calendar(&self) -> postman_core::model::Result<String> {
        self.jmap_default_calendar().await.map_err(core_error)
    }
}
