//! Файлы аккаунта (JMAP FileNode, черновик IETF `draft-ietf-jmap-filenode`; Stalwart, `docs/STALWART.md` п. 24, 29–32):
//! найти узел по имени в папке, создать папку, записать файл (новый или заменить содержимое) с проверкой состояния
//! (`ifInState`), удалить. Содержимое файла — blob: загрузка и скачивание — как у вложений.

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::mail::{GetResponse, inv};
use crate::{CORE, Client, Error, Result, take};

pub const FILENODE: &str = "urn:ietf:params:jmap:filenode";
const USING: &[&str] = &[CORE, FILENODE];
const PROPERTIES: &[&str] = &["id", "parentId", "blobId", "name", "type", "size"];

/// Узел FileNode: файл (`blob_id` есть) или папка.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileNode {
    pub id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub blob_id: Option<String>,
    pub name: String,
    #[serde(default, rename = "type")]
    pub mime: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
}

/// Чем закончилась запись с проверкой состояния.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Written {
    /// Записано: id узла и новое состояние FileNode аккаунта.
    Done { id: String, state: String },
    /// Состояние уже другое (`stateMismatch`: кто-то изменил файлы аккаунта) — перечитать и повторить.
    Stale,
    /// Узел с этим именем уже есть (`alreadyExists`, создал другой клиент) — его id.
    Exists(String),
}

impl Client {
    /// В аккаунте `account` есть файлы (capability FileNode у аккаунта в сессии).
    pub fn has_files(&self, account: &str) -> bool {
        self.session().accounts.get(account).is_some_and(|a| a.account_capabilities.contains_key(FILENODE))
    }

    /// Узел с именем `name` в папке `parent` (`None` — корень) и состояние FileNode аккаунта. Корень — условие
    /// `isTopLevel: true` (draft-ietf-jmap-filenode-18: `parentId` там типа `Id`, `null` по спецификации отвергается).
    /// Фильтр `name` у Stalwart — точное совпадение с учётом регистра (`docs/STALWART.md` п. 30).
    pub async fn file_node(&self, account: &str, parent: Option<&str>, name: &str) -> Result<(Option<FileNode>, String)> {
        let filter = match parent {
            Some(p) => json!({ "parentId": p, "name": name }),
            None => json!({ "isTopLevel": true, "name": name }),
        };
        let calls = [
            inv("FileNode/query", json!({ "accountId": account, "filter": filter }), "q"),
            inv(
                "FileNode/get",
                json!({
                    "accountId": account,
                    "#ids": { "resultOf": "q", "name": "FileNode/query", "path": "/ids" },
                    "properties": PROPERTIES,
                }),
                "g",
            ),
        ];
        let mut r = self.call(USING, &calls).await?;
        let g: GetResponse<FileNode> = take(&mut r, "g")?;
        Ok((g.list.into_iter().next(), g.state))
    }

    /// Создать папку `name` в `parent` (`None` — корень). Ответ — id папки и, если создал этот вызов, новое
    /// состояние; папка уже была (`alreadyExists`) — её id и `None`.
    pub async fn create_folder(&self, account: &str, parent: Option<&str>, name: &str) -> Result<(String, Option<String>)> {
        let args = json!({ "accountId": account, "create": { "d": { "name": name, "parentId": parent } } });
        let mut r = self.call(USING, &[inv("FileNode/set", args, "s")]).await?;
        let v: Value = take(&mut r, "s")?;
        if let Some(id) = v["created"]["d"]["id"].as_str() {
            return Ok((id.to_owned(), v["newState"].as_str().map(str::to_owned)));
        }
        let err = &v["notCreated"]["d"];
        match (err["type"].as_str(), err["existingId"].as_str()) {
            (Some("alreadyExists"), Some(id)) => Ok((id.to_owned(), None)),
            _ => Err(set_failed("FileNode/set", err)),
        }
    }

    /// Записать файл `name` в папку `parent`: `existing` — заменить содержимое этого узла (`blobId`), иначе создать.
    /// `if_in_state` — записать, только если файлы аккаунта с тех пор не менялись.
    #[allow(clippy::too_many_arguments)]
    pub async fn write_file(
        &self,
        account: &str,
        parent: &str,
        existing: Option<&str>,
        name: &str,
        blob_id: &str,
        mime: &str,
        if_in_state: Option<&str>,
    ) -> Result<Written> {
        let mut args = Map::new();
        args.insert("accountId".into(), json!(account));
        if let Some(s) = if_in_state {
            args.insert("ifInState".into(), json!(s));
        }
        match existing {
            Some(id) => args.insert("update".into(), json!({ id: { "blobId": blob_id } })),
            None => args.insert("create".into(), json!({ "f": { "name": name, "parentId": parent, "blobId": blob_id, "type": mime } })),
        };
        let r = self.call(USING, &[inv("FileNode/set", Value::Object(args), "s")]).await;
        let mut r = match r {
            Err(Error::Method { kind, .. }) if kind == "stateMismatch" => {
                return Ok(Written::Stale);
            }
            other => other?,
        };
        let v: Value = take(&mut r, "s")?;
        let state = v["newState"].as_str().unwrap_or_default().to_owned();
        match existing {
            Some(id) => {
                if v["updated"].get(id).is_some() {
                    return Ok(Written::Done { id: id.to_owned(), state });
                }
                Err(set_failed("FileNode/set", &v["notUpdated"][id]))
            }
            None => {
                if let Some(id) = v["created"]["f"]["id"].as_str() {
                    return Ok(Written::Done { id: id.to_owned(), state });
                }
                let err = &v["notCreated"]["f"];
                match (err["type"].as_str(), err["existingId"].as_str()) {
                    (Some("alreadyExists"), Some(id)) => Ok(Written::Exists(id.to_owned())),
                    _ => Err(set_failed("FileNode/set", err)),
                }
            }
        }
    }

    /// Удалить узлы; папки — вместе с содержимым (`onDestroyRemoveChildren`, иначе непустую папку сервер не удаляет).
    pub async fn destroy_file_nodes(&self, account: &str, ids: &[String]) -> Result<()> {
        let args = json!({ "accountId": account, "destroy": ids, "onDestroyRemoveChildren": true });
        let mut r = self.call(USING, &[inv("FileNode/set", args, "s")]).await?;
        let v: Value = take(&mut r, "s")?;
        match v["notDestroyed"].as_object().and_then(|m| m.values().next()) {
            Some(err) => Err(set_failed("FileNode/set", err)),
            None => Ok(()),
        }
    }
}

fn set_failed(method: &str, err: &Value) -> Error {
    Error::Method {
        method: method.into(),
        kind: err["type"].as_str().unwrap_or("?").into(),
        description: err["description"].as_str().unwrap_or("").into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stalwart_shape() {
        let file: FileNode = serde_json::from_value(json!({
            "id": "g", "parentId": "f", "nodeType": "file", "blobId": "c1", "name": "categories.json",
            "type": "application/json", "size": 8, "myRights": {}
        }))
        .unwrap();
        assert_eq!((file.parent_id.as_deref(), file.blob_id.as_deref(), file.size), (Some("f"), Some("c1"), Some(8)));
        let dir: FileNode = serde_json::from_value(json!({
            "id": "f", "parentId": null, "nodeType": "directory", "blobId": null, "name": ".postman", "type": null
        }))
        .unwrap();
        assert_eq!((dir.parent_id, dir.blob_id, dir.mime), (None, None, None));
    }
}
