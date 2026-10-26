//! Скрипты Sieve на сервере (RFC 9661, `SieveScript/*`): список, текст (blob), проверка, запись с загрузкой,
//! включение. Активен не больше одного скрипта; активный удалить нельзя.

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::mail::{GetResponse, inv, set_error};
use crate::{CORE, Client, Error, Result, SIEVE, take};

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SieveScript {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub blob_id: String,
    #[serde(default)]
    pub is_active: bool,
}

/// Что сделать с активностью после записи (`onSuccessActivateScript` / `onSuccessDeactivateScript`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Activate {
    /// Записанный скрипт — активный.
    This,
    /// Активного не будет.
    None,
    /// Не трогать.
    Keep,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SaveError {
    /// Скрипт не прошёл проверку сервера: его сообщение (строка, столбец).
    Invalid(String),
    Other(String),
}

const USING: &[&str] = &[CORE, SIEVE];
const MIME: &str = "application/sieve";

impl Client {
    pub async fn sieve_scripts(&self) -> Result<Vec<SieveScript>> {
        let acc = self.account_for(SIEVE)?;
        let args = json!({ "accountId": acc, "ids": null });
        let mut r = self.call(USING, &[inv("SieveScript/get", args, "g")]).await?;
        let g: GetResponse<SieveScript> = take(&mut r, "g")?;
        Ok(g.list)
    }

    /// Текст скрипта (blob).
    pub async fn sieve_text(&self, script: &SieveScript) -> Result<String> {
        let acc = self.account_for(SIEVE)?;
        let bytes = self.download(acc, &script.blob_id, "script.sieve", MIME).await?;
        String::from_utf8(bytes).map_err(|e| Error::Protocol(format!("скрипт Sieve не в UTF-8: {e}")))
    }

    /// Проверить и записать скрипт: загрузка один раз, `SieveScript/validate`, затем `SieveScript/set` (новый —
    /// `id: None`). Возвращает id скрипта.
    pub async fn save_sieve(&self, id: Option<&str>, name: &str, text: &str, activate: Activate) -> std::result::Result<String, SaveError> {
        let other = |e: Error| SaveError::Other(e.to_string());
        let acc = self.account_for(SIEVE).map_err(other)?.to_string();
        let blob = self.upload(&acc, text.as_bytes().to_vec(), MIME).await.map_err(other)?;
        let mut r = self.call(USING, &[inv("SieveScript/validate", json!({ "accountId": acc, "blobId": blob }), "v")]).await.map_err(other)?;
        let v: Value = take(&mut r, "v").map_err(other)?;
        if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
            let text = e["description"].as_str().or(e["type"].as_str()).unwrap_or("invalidScript");
            return Err(SaveError::Invalid(text.to_string()));
        }
        let mut args = Map::new();
        args.insert("accountId".into(), json!(acc));
        let (key, field) = match id {
            Some(id) => {
                args.insert("update".into(), json!({ id: { "blobId": blob } }));
                (id.to_string(), "notUpdated")
            }
            None => {
                args.insert("create".into(), json!({ "new": { "name": name, "blobId": blob } }));
                ("new".to_string(), "notCreated")
            }
        };
        match activate {
            Activate::This => {
                let target = if id.is_some() { key.clone() } else { "#new".into() };
                args.insert("onSuccessActivateScript".into(), json!(target));
            }
            Activate::None => {
                args.insert("onSuccessDeactivateScript".into(), json!(true));
            }
            Activate::Keep => {}
        }
        let mut r = self.call(USING, &[inv("SieveScript/set", Value::Object(args), "s")]).await.map_err(other)?;
        let v: Value = take(&mut r, "s").map_err(other)?;
        if let Some(e) = v.get(field).and_then(|m| m.get(&key)) {
            let text = e["description"].as_str().unwrap_or("").to_string();
            return Err(match e["type"].as_str() {
                Some("invalidScript") => SaveError::Invalid(text),
                kind => SaveError::Other(format!("SieveScript/set: {} {text}", kind.unwrap_or("?"))),
            });
        }
        match id {
            Some(id) => Ok(id.to_string()),
            None => v["created"]["new"]["id"].as_str().map(str::to_string).ok_or_else(|| SaveError::Other("SieveScript/set не вернул id".into())),
        }
    }

    /// Сделать активным скрипт `id`; `None` — ни одного.
    pub async fn activate_sieve(&self, id: Option<&str>) -> Result<()> {
        let acc = self.account_for(SIEVE)?;
        let args = match id {
            Some(id) => json!({ "accountId": acc, "onSuccessActivateScript": id }),
            None => json!({ "accountId": acc, "onSuccessDeactivateScript": true }),
        };
        let mut r = self.call(USING, &[inv("SieveScript/set", args, "s")]).await?;
        let _: Value = take(&mut r, "s")?;
        Ok(())
    }

    /// Удалить скрипт (активный сервер не удаляет — сначала `activate_sieve(None)`).
    pub async fn destroy_sieve(&self, id: &str) -> Result<()> {
        let acc = self.account_for(SIEVE)?;
        let mut r = self.call(USING, &[inv("SieveScript/set", json!({ "accountId": acc, "destroy": [id] }), "s")]).await?;
        let v: Value = take(&mut r, "s")?;
        set_error(&v, "notDestroyed", id, "SieveScript/set")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stalwart_shape() {
        let s: SieveScript = serde_json::from_value(json!({ "id": "b", "name": "vacation", "blobId": "c1", "isActive": true })).unwrap();
        assert_eq!((s.id.as_str(), s.name.as_deref(), s.is_active), ("b", Some("vacation"), true));
    }
}
