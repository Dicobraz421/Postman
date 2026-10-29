//! Отправка (RFC 8621 §7): письмо собирается на клиенте целиком, загружается
//! blob'ом, импортируется в «Черновики» и отправляется через EmailSubmission;
//! после успеха сервер сам переносит его в «Отправленные».
//!
//! Отложенная отправка (S5-06) — расширение SMTP FUTURERELEASE (RFC 4865) через параметры `mailFrom` конверта
//! (`HOLDFOR`/`HOLDUNTIL`): `sendAt` у EmailSubmission сервер ставит сам, клиенту его задавать нельзя (RFC 8621 §7).
//! Пока письмо ждёт (`undoStatus: pending`), отправку отменяет `undoStatus: canceled` (`docs/STALWART.md`).

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::mail::{GetResponse, USING, inv, set_error};
use crate::{CORE, Client, Error, MAIL, Result, SUBMISSION, take};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub email: String,
    #[serde(default)]
    pub text_signature: String,
}

/// SMTP-конверт: отправитель и все получатели, включая скрытых.
#[derive(Debug, Clone)]
pub struct Envelope {
    pub mail_from: String,
    pub rcpt_to: Vec<String>,
}

/// Когда сервер выпускает письмо (FUTURERELEASE, RFC 4865).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Release {
    /// Сразу.
    Now,
    /// Через столько секунд после создания отправки (`HOLDFOR`): окно «Отменить» (S5-06).
    After(u32),
    /// В момент (Unix-секунды, `HOLDUNTIL`): «Отправить позже».
    At(i64),
}

impl Release {
    /// Параметры `mailFrom` конверта.
    fn parameters(self) -> Option<Value> {
        match self {
            Release::Now => None,
            Release::After(s) => Some(json!({ "HOLDFOR": s.to_string() })),
            Release::At(t) => Some(json!({ "HOLDUNTIL": unix_to_utc(t) })),
        }
    }
}

/// Итог `EmailSubmission/set create`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submitted {
    /// Письмо (оно уже в «Отправленных»: сервер переносит его при создании отправки, и отложенной тоже).
    pub email_id: String,
    pub submission_id: String,
    /// Когда сервер выпустит письмо (Unix-секунды; `sendAt` сервера).
    pub send_at: Option<i64>,
    /// `undoStatus: pending` — отправку ещё можно отменить.
    pub pending: bool,
}

/// Отправка, которую сервер ещё держит (`undoStatus: pending`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingSubmission {
    pub id: String,
    pub email_id: String,
    /// UTCDate сервера; [`PendingSubmission::send_at_unix`].
    #[serde(default)]
    pub send_at: String,
}

impl PendingSubmission {
    pub fn send_at_unix(&self) -> Option<i64> {
        crate::utc_to_unix(&self.send_at)
    }
}

/// Отмена не удалась: письмо уже ушло (`cannotUnsend`) или другая ошибка.
#[derive(Debug, thiserror::Error)]
pub enum UnsendError {
    #[error("the message has already been released")]
    TooLate,
    #[error(transparent)]
    Jmap(#[from] Error),
}

/// Письмо, выгруженное `Email/import` или скопированное `Email/copy`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
    pub id: String,
    /// Такое письмо в аккаунте уже было (`alreadyExists`): `id` — его, новое не создано.
    pub existed: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Uploaded {
    blob_id: String,
}

impl Client {
    pub async fn identities(&self) -> Result<Vec<Identity>> {
        let acc = self.account_for(SUBMISSION)?;
        let mut r = self.call(&[CORE, MAIL, SUBMISSION], &[inv("Identity/get", json!({ "accountId": acc, "ids": null }), "i")]).await?;
        let g: GetResponse<Identity> = take(&mut r, "i")?;
        Ok(g.list)
    }

    pub async fn upload(&self, account: &str, data: Vec<u8>, mime: &str) -> Result<String> {
        let url = self.session().upload_url.replace("{accountId}", account);
        let up: Uploaded = self
            .authorized(self.http().post(url))
            .header(reqwest::header::CONTENT_TYPE, mime)
            .body(data)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(up.blob_id)
    }

    /// Выгрузить письмо (блоб из [`Client::upload`]) в папки `mailbox_ids` аккаунта `account` (`Email/import`, RFC 8621
    /// §4.8): ключевые слова как есть (`$seen`, `$flagged`, `pm.*`), дата получения — `received_at` (Unix-секунды;
    /// `None` — время выгрузки). Сервер узнал письмо (`alreadyExists`) — id существующего, без дубля.
    pub async fn import_email(
        &self,
        account: &str,
        blob: &str,
        mailbox_ids: &[String],
        keywords: &[String],
        received_at: Option<i64>,
    ) -> Result<Imported> {
        let email = import_object(blob, mailbox_ids, keywords, received_at);
        let call = inv("Email/import", json!({ "accountId": account, "emails": { "m": email } }), "i");
        let mut r = self.call(USING, &[call]).await?;
        let v: Value = take(&mut r, "i")?;
        created_or_existing(&v, "m", "Email/import")
    }

    /// Сохранить черновик с ключевыми словами `keywords` (сверх `$draft`, `$seen`); прежняя версия (`replace`)
    /// удаляется тем же запросом.
    pub async fn save_draft(&self, raw: Vec<u8>, drafts: &str, replace: Option<&str>, keywords: &[String]) -> Result<String> {
        let acc = self.mail_account()?.to_string();
        let blob = self.upload(&acc, raw, "message/rfc822").await?;
        let mut calls = vec![import_call(&acc, &blob, drafts, keywords)];
        if let Some(old) = replace {
            calls.push(inv("Email/set", json!({ "accountId": acc, "destroy": [old] }), "d"));
        }
        let mut r = self.call(USING, &calls).await?;
        imported_id(&mut r)
    }

    /// Отправить собранное письмо. Возвращает id письма (оно окажется в `sent`).
    pub async fn send(&self, raw: Vec<u8>, identity: &str, envelope: &Envelope, drafts: &str, sent: &str) -> Result<String> {
        Ok(self.submit(raw, identity, envelope, drafts, sent, Release::Now, &[]).await?.email_id)
    }

    /// Отправить собранное письмо с задержкой `release` (FUTURERELEASE). Письмо сразу переносится в `sent` — и
    /// при отложенной отправке: `onSuccessUpdateEmail` сервер применяет при создании отправки, а не при выпуске.
    /// `keywords` — ключевые слова письма (категория, «Ждать ответа»): остаются у него в `sent`.
    #[allow(clippy::too_many_arguments)]
    pub async fn submit(
        &self,
        raw: Vec<u8>,
        identity: &str,
        envelope: &Envelope,
        drafts: &str,
        sent: &str,
        release: Release,
        keywords: &[String],
    ) -> Result<Submitted> {
        let acc = self.mail_account()?.to_string();
        let blob = self.upload(&acc, raw, "message/rfc822").await?;
        let rcpt: Vec<Value> = envelope.rcpt_to.iter().map(|e| json!({ "email": e })).collect();
        let mut mail_from = json!({ "email": envelope.mail_from });
        if let Some(p) = release.parameters() {
            mail_from["parameters"] = p;
        }
        let mut on_success = Map::new();
        on_success.insert(format!("mailboxIds/{drafts}"), Value::Null);
        on_success.insert(format!("mailboxIds/{sent}"), Value::Bool(true));
        on_success.insert("keywords/$draft".into(), Value::Null);
        let calls = [
            import_call(&acc, &blob, drafts, keywords),
            inv(
                "EmailSubmission/set",
                json!({
                    "accountId": acc,
                    "create": { "sub": {
                        "identityId": identity,
                        "emailId": "#draft",
                        "envelope": { "mailFrom": mail_from, "rcptTo": rcpt },
                    }},
                    "onSuccessUpdateEmail": { "#sub": on_success },
                }),
                "s",
            ),
        ];
        let mut r = self.call(&[CORE, MAIL, SUBMISSION], &calls).await?;
        let email_id = imported_id(&mut r)?;
        let v: Value = take(&mut r, "s")?;
        set_error(&v, "notCreated", "sub", "EmailSubmission/set")?;
        let created = &v["created"]["sub"];
        let submission_id = created["id"].as_str().map(str::to_string).ok_or_else(|| Error::Protocol("EmailSubmission/set не вернул id".into()))?;
        Ok(Submitted {
            email_id,
            submission_id,
            send_at: created["sendAt"].as_str().and_then(crate::utc_to_unix),
            pending: created["undoStatus"].as_str() == Some("pending"),
        })
    }

    /// Максимальная задержка отправки, которую объявил сервер (`maxDelayedSend`, секунды); 0 — FUTURERELEASE нет.
    pub fn max_delayed_send(&self) -> u64 {
        let Ok(acc) = self.account_for(SUBMISSION) else { return 0 };
        self.session()
            .accounts
            .get(acc)
            .and_then(|a| a.account_capabilities.get(SUBMISSION))
            .and_then(|c| c.get("maxDelayedSend"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    }

    /// Отправки, которые сервер ещё держит (`undoStatus: pending`): «Отправить позже» и окно «Отменить».
    pub async fn pending_submissions(&self) -> Result<Vec<PendingSubmission>> {
        let acc = self.account_for(SUBMISSION)?.to_string();
        let calls = [
            inv("EmailSubmission/query", json!({ "accountId": acc, "filter": { "undoStatus": "pending" } }), "q"),
            inv(
                "EmailSubmission/get",
                json!({
                    "accountId": acc,
                    "#ids": { "resultOf": "q", "name": "EmailSubmission/query", "path": "/ids" },
                    "properties": ["emailId", "sendAt", "undoStatus"],
                }),
                "g",
            ),
        ];
        let mut r = self.call(&[CORE, MAIL, SUBMISSION], &calls).await?;
        let g: GetResponse<PendingSubmission> = take(&mut r, "g")?;
        Ok(g.list)
    }

    /// Отменить отложенную отправку и вернуть письмо из `sent` в `drafts` черновиком. Письмо уже ушло —
    /// [`UnsendError::TooLate`], письмо не трогается.
    pub async fn unsend(&self, submission: &str, email: &str, drafts: &str, sent: &str) -> std::result::Result<(), UnsendError> {
        self.cancel_submission(submission).await?;
        let acc = self.account_for(SUBMISSION)?.to_string();
        // Отменено — письмо снова черновик. Отдельным запросом: в одном с отменой он прошёл бы и при её отказе.
        let mut patch = Map::new();
        patch.insert(format!("mailboxIds/{sent}"), Value::Null);
        patch.insert(format!("mailboxIds/{drafts}"), Value::Bool(true));
        patch.insert("keywords/$draft".into(), Value::Bool(true));
        let update = inv("Email/set", json!({ "accountId": acc, "update": { email: patch } }), "e");
        let mut r = self.call(USING, &[update]).await?;
        let v: Value = take(&mut r, "e")?;
        set_error(&v, "notUpdated", email, "Email/set")?;
        Ok(())
    }

    /// Отменить отправку, которую держит сервер (`undoStatus: canceled`); письмо остаётся, где было. Уже ушло —
    /// [`UnsendError::TooLate`].
    pub async fn cancel_submission(&self, submission: &str) -> std::result::Result<(), UnsendError> {
        let acc = self.account_for(SUBMISSION)?.to_string();
        let cancel = inv("EmailSubmission/set", json!({ "accountId": acc, "update": { submission: { "undoStatus": "canceled" } } }), "c");
        let mut r = self.call(&[CORE, MAIL, SUBMISSION], &[cancel]).await?;
        let v: Value = take(&mut r, "c")?;
        if v["notUpdated"][submission]["type"].as_str() == Some("cannotUnsend") {
            return Err(UnsendError::TooLate);
        }
        set_error(&v, "notUpdated", submission, "EmailSubmission/set")?;
        Ok(())
    }
}

/// Unix-секунды → JMAP UTCDate (`2026-10-21T06:00:00Z`); обратное к [`crate::utc_to_unix`].
pub fn unix_to_utc(t: i64) -> String {
    let (days, secs) = (t.div_euclid(86_400), t.rem_euclid(86_400));
    // Дата по дням от эпохи — Howard Hinnant (civil_from_days).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, secs % 3600 / 60, secs % 60)
}

/// `Email/import` черновика: `$draft`, `$seen` и свои ключевые слова.
fn import_call(acc: &str, blob: &str, drafts: &str, keywords: &[String]) -> crate::Invocation {
    let mut all = vec!["$draft".to_owned(), "$seen".to_owned()];
    all.extend_from_slice(keywords);
    let email = import_object(blob, &[drafts.to_owned()], &all, None);
    inv("Email/import", json!({ "accountId": acc, "emails": { "draft": email } }), "i")
}

/// Письмо для `Email/import`; ключевые слова — в нижнем регистре, неверные для JMAP (RFC 8621 §4.1.1) пропускаются;
/// `received_at` — Unix-секунды (`None` — время выгрузки, ставит сервер).
fn import_object(blob: &str, mailbox_ids: &[String], keywords: &[String], received_at: Option<i64>) -> Value {
    let mut kw = Map::new();
    for k in keywords.iter().map(|k| k.to_ascii_lowercase()) {
        let valid = (1..=255).contains(&k.len()) && k.bytes().all(|b| (0x21..=0x7e).contains(&b) && !b"(){]%*\"\\".contains(&b));
        if valid {
            kw.insert(k, Value::Bool(true));
        }
    }
    let mailboxes: Map<String, Value> = mailbox_ids.iter().map(|m| (m.clone(), Value::Bool(true))).collect();
    let mut email = json!({ "blobId": blob, "mailboxIds": mailboxes, "keywords": kw });
    if let Some(t) = received_at {
        email["receivedAt"] = json!(unix_to_utc(t));
    }
    email
}

/// Итог создания под ключом `key` в ответе `Email/import` или `Email/copy`: `created` → id; `alreadyExists` → id
/// существующего письма (`existingId`, RFC 8620 §5.4); другая ошибка — [`Error::Method`].
pub(crate) fn created_or_existing(v: &Value, key: &str, method: &str) -> Result<Imported> {
    let err = &v["notCreated"][key];
    if err["type"].as_str() == Some("alreadyExists")
        && let Some(id) = err["existingId"].as_str()
    {
        return Ok(Imported { id: id.to_owned(), existed: true });
    }
    set_error(v, "notCreated", key, method)?;
    let id = v["created"][key]["id"].as_str().ok_or_else(|| Error::Protocol(format!("{method} не вернул id")))?;
    Ok(Imported { id: id.to_owned(), existed: false })
}

fn imported_id(r: &mut [crate::Invocation]) -> Result<String> {
    let v: Value = take(r, "i")?;
    set_error(&v, "notCreated", "draft", "Email/import")?;
    v["created"]["draft"]["id"].as_str().map(str::to_string).ok_or_else(|| Error::Protocol("Email/import не вернул id".into()))
}

#[cfg(test)]
mod tests {
    use super::{Imported, Release, created_or_existing, import_object, unix_to_utc};
    use crate::utc_to_unix;
    use serde_json::json;

    #[test]
    fn utc_round_trip() {
        for t in [0, 951_782_400, 1_792_240_200, 1_792_476_000, 4_102_444_799] {
            assert_eq!(utc_to_unix(&unix_to_utc(t)), Some(t), "{t}");
        }
        assert_eq!(unix_to_utc(1_792_240_200), "2026-10-17T12:30:00Z");
    }

    /// S5-06б: свои ключевые слова черновика — рядом с `$draft`; неверные для JMAP пропускаются.
    #[test]
    fn draft_keywords_go_into_the_import() {
        let kw = ["pm.conf".to_string(), "PM.Wait.2026-10-24".into(), "bad word".into(), "pm(x".into()];
        let crate::Invocation(name, v, _) = super::import_call("a", "b1", "d1", &kw);
        assert_eq!(name, "Email/import");
        let v = &v["emails"]["draft"];
        assert_eq!(v["keywords"], json!({ "$draft": true, "$seen": true, "pm.conf": true, "pm.wait.2026-10-24": true }));
        assert_eq!(v["mailboxIds"], json!({ "d1": true }));
        assert!(v.get("receivedAt").is_none(), "черновик — время сервера");
    }

    /// S8-11: выгрузка в любую папку — свои папки, ключевые слова как есть (без `$draft`), дата получения письма.
    #[test]
    fn import_into_any_mailbox() {
        let kw = ["$seen".to_string(), "$Flagged".into(), "pm.cat.clients".into(), "bad word".into()];
        let v = import_object("b1", &["in".into(), "arch".into()], &kw, Some(1_792_240_200));
        assert_eq!(
            v,
            json!({
                "blobId": "b1",
                "mailboxIds": { "in": true, "arch": true },
                "keywords": { "$seen": true, "$flagged": true, "pm.cat.clients": true },
                "receivedAt": "2026-10-17T12:30:00Z",
            })
        );
    }

    /// Ответы `Email/import` (RFC 8621 §4.8): создано, уже есть (`alreadyExists` → `existingId`), отказ.
    #[test]
    fn import_answers() {
        let created = json!({ "accountId": "a", "oldState": "1", "newState": "2",
            "created": { "m0": { "id": "M1", "blobId": "b1", "threadId": "T1", "size": 1024 } }, "notCreated": null });
        assert_eq!(created_or_existing(&created, "m0", "Email/import").unwrap(), Imported { id: "M1".into(), existed: false });
        let exists = json!({ "accountId": "a", "oldState": "1", "newState": "1", "created": null,
            "notCreated": { "m0": { "type": "alreadyExists", "existingId": "M7", "description": "duplicate" } } });
        assert_eq!(created_or_existing(&exists, "m0", "Email/import").unwrap(), Imported { id: "M7".into(), existed: true });
        let over = json!({ "accountId": "a", "oldState": "1", "newState": "1",
            "notCreated": { "m0": { "type": "overQuota", "description": "quota exceeded" } } });
        let err = created_or_existing(&over, "m0", "Email/import").unwrap_err();
        assert!(matches!(&err, crate::Error::Method { kind, .. } if kind == "overQuota"), "{err}");
        assert!(created_or_existing(&json!({ "created": {} }), "m0", "Email/import").is_err());
    }

    #[test]
    fn release_becomes_envelope_parameters() {
        assert_eq!(Release::Now.parameters(), None);
        assert_eq!(Release::After(13).parameters(), Some(json!({ "HOLDFOR": "13" })));
        assert_eq!(Release::At(1_792_240_200).parameters(), Some(json!({ "HOLDUNTIL": "2026-10-17T12:30:00Z" })));
    }
}
