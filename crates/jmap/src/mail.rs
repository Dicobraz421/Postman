//! JMAP Mail (RFC 8621): ящики, письма, изменения.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{CORE, Client, Error, Invocation, MAIL, Result, take};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mailbox {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub role: Option<String>,
    #[serde(default)]
    pub sort_order: u32,
    #[serde(default)]
    pub total_emails: u32,
    #[serde(default)]
    pub unread_emails: u32,
    /// Мои права на папку (RFC 8621 §2): в общем ящике часть действий может быть запрещена.
    #[serde(default)]
    pub my_rights: MailboxRights,
}

/// Права на папку; сервер не прислал — всё разрешено (своя папка).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MailboxRights {
    pub may_read_items: bool,
    pub may_add_items: bool,
    pub may_remove_items: bool,
    pub may_set_seen: bool,
    pub may_set_keywords: bool,
    pub may_submit: bool,
}

impl Default for MailboxRights {
    fn default() -> Self {
        Self { may_read_items: true, may_add_items: true, may_remove_items: true, may_set_seen: true, may_set_keywords: true, may_submit: true }
    }
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct EmailAddress {
    pub name: Option<String>,
    pub email: Option<String>,
}

impl EmailAddress {
    pub fn display(&self) -> &str {
        self.name.as_deref().filter(|n| !n.is_empty()).or(self.email.as_deref()).unwrap_or("")
    }
}

pub const SUMMARY_PROPERTIES: &[&str] = &[
    "id",
    "threadId",
    "mailboxIds",
    "keywords",
    "receivedAt",
    "subject",
    "from",
    "to",
    "cc",
    "preview",
    "hasAttachment",
    "size",
    "header:Content-Type",
];

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailSummary {
    pub id: String,
    pub thread_id: String,
    pub mailbox_ids: HashMap<String, bool>,
    #[serde(default)]
    pub keywords: HashMap<String, bool>,
    pub received_at: String,
    pub subject: Option<String>,
    pub from: Option<Vec<EmailAddress>>,
    /// Получатели (S5-03: быстрый фильтр «Мне» — мой адрес в To).
    #[serde(default)]
    pub to: Option<Vec<EmailAddress>>,
    #[serde(default)]
    pub cc: Option<Vec<EmailAddress>>,
    #[serde(default)]
    pub preview: String,
    #[serde(default)]
    pub has_attachment: bool,
    #[serde(default)]
    pub size: u64,
    /// `Content-Type` корня как пришёл (S5-12 Ф2: шифрованное S/MIME видно без загрузки письма).
    #[serde(default, rename = "header:Content-Type")]
    pub content_type: Option<String>,
}

impl EmailSummary {
    pub fn has_keyword(&self, kw: &str) -> bool {
        self.keywords.get(kw).copied().unwrap_or(false)
    }
}

/// Письмо как сообщение целиком (`Client::email_blobs`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailBlob {
    pub id: String,
    pub blob_id: String,
    #[serde(default)]
    pub subject: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BodyPart {
    pub part_id: Option<String>,
    pub blob_id: Option<String>,
    #[serde(default)]
    pub size: u64,
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub mime: String,
    pub disposition: Option<String>,
    pub cid: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawBody {
    #[serde(default)]
    id: String,
    message_id: Option<Vec<String>>,
    references: Option<Vec<String>>,
    reply_to: Option<Vec<EmailAddress>>,
    sent_at: Option<String>,
    to: Option<Vec<EmailAddress>>,
    cc: Option<Vec<EmailAddress>>,
    #[serde(default)]
    body_values: HashMap<String, BodyValue>,
    #[serde(default)]
    text_body: Vec<BodyPart>,
    #[serde(default)]
    html_body: Vec<BodyPart>,
    #[serde(default)]
    attachments: Vec<BodyPart>,
    /// Письмо целиком (RFC 5322) — для проверки подписи S/MIME (S5-12).
    blob_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct BodyValue {
    value: String,
}

impl RawBody {
    fn into_body(self) -> EmailBody {
        let join = |parts: &[BodyPart], want: &str| -> Option<String> {
            let mut out = String::new();
            for p in parts.iter().filter(|p| p.mime == want) {
                if let Some(v) = p.part_id.as_ref().and_then(|pid| self.body_values.get(pid)) {
                    out.push_str(&v.value);
                }
            }
            (!out.is_empty()).then_some(out)
        };
        EmailBody {
            message_id: self.message_id.as_ref().and_then(|v| v.first().cloned()),
            references: self.references.clone().unwrap_or_default(),
            reply_to: self.reply_to.clone().unwrap_or_default(),
            sent_at: self.sent_at.clone(),
            html: join(&self.html_body, "text/html"),
            text: join(&self.text_body, "text/plain"),
            to: self.to.clone().unwrap_or_default(),
            cc: self.cc.clone().unwrap_or_default(),
            attachments: self.attachments.clone(),
            blob_id: self.blob_id.clone(),
        }
    }
}

/// Тело письма, декодированное сервером (MIME на клиенте не разбираем).
#[derive(Debug, Clone)]
pub struct EmailBody {
    /// Message-ID оригинала (без угловых скобок) — для In-Reply-To.
    pub message_id: Option<String>,
    pub references: Vec<String>,
    pub reply_to: Vec<EmailAddress>,
    /// Дата из заголовка Date (UTCDate/Date JMAP), если есть.
    pub sent_at: Option<String>,
    pub to: Vec<EmailAddress>,
    pub cc: Vec<EmailAddress>,
    pub html: Option<String>,
    pub text: Option<String>,
    pub attachments: Vec<BodyPart>,
    /// `Email.blobId`: письмо целиком (подпись S/MIME проверяется по точным байтам, S5-12).
    pub blob_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EmailPage {
    pub total: u64,
    pub position: u64,
    /// Порядок результатов запроса (Email/get порядок не гарантирует).
    pub ids: Vec<String>,
    pub emails: Vec<EmailSummary>,
    pub query_state: String,
    /// Состояние `Email` — точка отсчёта для `Email/changes`.
    pub state: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Changes {
    pub old_state: String,
    pub new_state: String,
    pub has_more_changes: bool,
    pub created: Vec<String>,
    pub updated: Vec<String>,
    pub destroyed: Vec<String>,
}

/// Беседа (RFC 8621 §3): письма цепочки.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Thread {
    #[serde(default)]
    email_ids: Vec<String>,
}

#[derive(Deserialize)]
pub(crate) struct GetResponse<T> {
    pub(crate) state: String,
    pub(crate) list: Vec<T>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueryResponse {
    query_state: String,
    #[serde(default)]
    total: Option<u64>,
    position: u64,
    #[serde(default)]
    ids: Vec<String>,
}

pub(crate) const USING: &[&str] = &[CORE, MAIL];

pub(crate) fn inv(name: &str, args: Value, id: &str) -> Invocation {
    Invocation(name.into(), args, id.into())
}

/// Писем в одном `Email/set` — не больше `maxObjectsInSet` сервера (Stalwart — 500).
pub const SET_CHUNK: usize = 500;
/// Вызовов в одном запросе — не больше `maxCallsInRequest` (Stalwart — 16).
pub const CALLS_CHUNK: usize = 16;

/// Путь патча `Email/set` к ключевому слову: JSON Pointer (RFC 6901), `~` → `~0`, `/` → `~1` — оба знака
/// в ключевых словах допустимы (RFC 8621 §4.1.1).
pub fn keyword_path(keyword: &str) -> String {
    format!("keywords/{}", keyword.replace('~', "~0").replace('/', "~1"))
}

/// Патч `Email/set update`: `add` — `true`, `remove` — `null` (снять); остальные ключевые слова не меняются.
pub fn keywords_patch(add: &[String], remove: &[String]) -> Value {
    let mut patch = serde_json::Map::new();
    for k in remove {
        patch.insert(keyword_path(k), Value::Null);
    }
    for k in add {
        patch.insert(keyword_path(k), Value::Bool(true));
    }
    Value::Object(patch)
}

/// Письмо с признаком рассылки ([`Client::list_letters`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListLetter {
    pub id: String,
    /// Адрес отправителя.
    pub from: String,
    pub received_at: i64,
    pub seen: bool,
    /// Есть `List-Id` или `List-Unsubscribe`.
    pub list: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawListLetter {
    id: String,
    from: Option<Vec<EmailAddress>>,
    #[serde(default)]
    received_at: String,
    #[serde(default)]
    keywords: HashMap<String, bool>,
    #[serde(rename = "header:List-Id", default)]
    list_id: Option<String>,
    #[serde(rename = "header:List-Unsubscribe", default)]
    unsubscribe: Option<String>,
}

/// Патч одного письма для `Email/set update` (S5-04): папки и ключевые слова — путями, остальное не трогается.
/// `true` — поставить, `false` — снять (`null`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EmailPatch(serde_json::Map<String, Value>);

impl EmailPatch {
    pub fn mailbox(mut self, id: &str, on: bool) -> Self {
        let path = format!("mailboxIds/{}", id.replace('~', "~0").replace('/', "~1"));
        self.0.insert(path, if on { Value::Bool(true) } else { Value::Null });
        self
    }

    pub fn keyword(mut self, keyword: &str, on: bool) -> Self {
        self.0.insert(keyword_path(keyword), if on { Value::Bool(true) } else { Value::Null });
        self
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn to_json(&self) -> Value {
        Value::Object(self.0.clone())
    }
}

/// Условие `Email/query` (RFC 8621 §4.4.1) — для фильтров панели и «Все папки».
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    InMailbox(String),
    HasKeyword(String),
    NotKeyword(String),
    Text(String),
    /// Текст в From, To, теле (RFC 8621: поиск подстроки в заголовке или тексте).
    From(String),
    To(String),
    Body(String),
    /// Текст в теме (`subject`, S5-10).
    Subject(String),
    /// Есть вложение (`hasAttachment`).
    HasAttachment(bool),
    /// Все условия (`operator: AND`).
    And(Vec<Filter>),
    /// Любое из условий (`operator: OR`): закладки быстрых фильтров между собой.
    Or(Vec<Filter>),
    /// Ни одно из условий (`operator: NOT`): «внешние» — отправитель не из своих доменов.
    Not(Vec<Filter>),
    /// Получено раньше (`before`, UTCDate `2026-10-21T00:00:00Z`).
    Before(String),
    /// Получено не раньше (`after`, UTCDate; S5-10).
    After(String),
    /// Размер не меньше, байт (`minSize`; «Очистить вложения…», S5-29).
    MinSize(u64),
}

impl Filter {
    pub fn to_json(&self) -> Value {
        let op = |op: &str, list: &[Filter]| json!({ "operator": op, "conditions": list.iter().map(Filter::to_json).collect::<Vec<_>>() });
        match self {
            Filter::InMailbox(m) => json!({ "inMailbox": m }),
            Filter::HasKeyword(k) => json!({ "hasKeyword": k }),
            Filter::NotKeyword(k) => json!({ "notKeyword": k }),
            Filter::Text(t) => json!({ "text": t }),
            Filter::From(t) => json!({ "from": t }),
            Filter::To(t) => json!({ "to": t }),
            Filter::Body(t) => json!({ "body": t }),
            Filter::Subject(t) => json!({ "subject": t }),
            Filter::HasAttachment(on) => json!({ "hasAttachment": on }),
            Filter::And(list) => op("AND", list),
            Filter::Or(list) => op("OR", list),
            Filter::Not(list) => op("NOT", list),
            Filter::Before(at) => json!({ "before": at }),
            Filter::After(at) => json!({ "after": at }),
            Filter::MinSize(n) => json!({ "minSize": n }),
        }
    }
}

impl Client {
    pub fn mail_account(&self) -> Result<&str> {
        self.account_for(MAIL)
    }

    /// Все ящики и состояние `Mailbox`.
    pub async fn mailboxes(&self) -> Result<(Vec<Mailbox>, String)> {
        let acc = self.mail_account()?;
        let mut r = self.call(USING, &[inv("Mailbox/get", json!({ "accountId": acc, "ids": null }), "m")]).await?;
        let g: GetResponse<Mailbox> = take(&mut r, "m")?;
        Ok((g.list, g.state))
    }

    /// Страница писем ящика, новые сверху: Email/query + Email/get одним запросом.
    pub async fn email_page(&self, mailbox_id: &str, position: u64, limit: u64) -> Result<EmailPage> {
        self.query_page(json!({ "inMailbox": mailbox_id }), Some(position), None, limit).await
    }

    /// Следующая страница после письма `anchor` (устойчиво к новым письмам сверху).
    pub async fn email_page_after(&self, mailbox_id: &str, anchor: &str, limit: u64) -> Result<EmailPage> {
        self.query_page(json!({ "inMailbox": mailbox_id }), None, Some(anchor), limit).await
    }

    /// Серверный поиск по тексту (тема, адреса, тело), опционально в одной папке.
    pub async fn search(&self, text: &str, mailbox_id: Option<&str>, limit: u64) -> Result<EmailPage> {
        let filter = match mailbox_id {
            Some(mb) => json!({ "operator": "AND", "conditions": [{ "text": text }, { "inMailbox": mb }] }),
            None => json!({ "text": text }),
        };
        self.query_page(filter, Some(0), None, limit).await
    }

    async fn query_page(&self, filter: Value, position: Option<u64>, anchor: Option<&str>, limit: u64) -> Result<EmailPage> {
        let acc = self.mail_account()?;
        let mut query = json!({
            "accountId": acc,
            "filter": filter,
            "sort": [{ "property": "receivedAt", "isAscending": false }],
            "limit": limit,
            "calculateTotal": true,
        });
        if let Some(p) = position {
            query["position"] = json!(p);
        }
        if let Some(a) = anchor {
            query["anchor"] = json!(a);
            query["anchorOffset"] = json!(1);
        }
        let calls = [
            inv("Email/query", query, "q"),
            inv(
                "Email/get",
                json!({
                    "accountId": acc,
                    "#ids": { "resultOf": "q", "name": "Email/query", "path": "/ids" },
                    "properties": SUMMARY_PROPERTIES,
                }),
                "g",
            ),
        ];
        let mut r = self.call(USING, &calls).await?;
        let q: QueryResponse = take(&mut r, "q")?;
        let g: GetResponse<EmailSummary> = take(&mut r, "g")?;
        Ok(EmailPage { total: q.total.unwrap_or(0), position: q.position, ids: q.ids, emails: g.list, query_state: q.query_state, state: g.state })
    }

    pub async fn email_get(&self, ids: &[String]) -> Result<(Vec<EmailSummary>, String)> {
        let acc = self.mail_account()?;
        let args = json!({ "accountId": acc, "ids": ids, "properties": SUMMARY_PROPERTIES });
        let mut r = self.call(USING, &[inv("Email/get", args, "g")]).await?;
        let g: GetResponse<EmailSummary> = take(&mut r, "g")?;
        Ok((g.list, g.state))
    }

    /// Беседа целиком (S5-05): `Thread/get` и краткие записи её писем из всех папок одним запросом. Порядок id —
    /// как у сервера (RFC 8621 §3: по `receivedAt`, старые первыми); беседы нет — пусто.
    pub async fn thread(&self, thread_id: &str) -> Result<(Vec<String>, Vec<EmailSummary>)> {
        let acc = self.mail_account()?;
        let calls = [
            inv("Thread/get", json!({ "accountId": acc, "ids": [thread_id] }), "t"),
            inv(
                "Email/get",
                json!({
                    "accountId": acc,
                    "#ids": { "resultOf": "t", "name": "Thread/get", "path": "/list/*/emailIds" },
                    "properties": SUMMARY_PROPERTIES,
                }),
                "g",
            ),
        ];
        let mut r = self.call(USING, &calls).await?;
        let t: GetResponse<Thread> = take(&mut r, "t")?;
        let g: GetResponse<EmailSummary> = take(&mut r, "g")?;
        Ok((t.list.into_iter().flat_map(|t| t.email_ids).collect(), g.list))
    }

    /// Дельта с указанного состояния (`Email/changes` или `Mailbox/changes`).
    pub async fn changes(&self, kind: &str, since: &str, max: u64) -> Result<Changes> {
        let acc = self.mail_account()?;
        let args = json!({ "accountId": acc, "sinceState": since, "maxChanges": max });
        let mut r = self.call(USING, &[inv(&format!("{kind}/changes"), args, "c")]).await?;
        take(&mut r, "c")
    }

    pub async fn email_body(&self, id: &str) -> Result<EmailBody> {
        let mut bodies = self.email_bodies(&[id.to_string()], 4 * 1024 * 1024).await?;
        bodies.pop().map(|(_, b)| b).ok_or_else(|| crate::Error::Protocol(format!("письмо {id} не найдено")))
    }

    /// Тела нескольких писем одним запросом; части больше `max_bytes` усекаются сервером.
    pub async fn email_bodies(&self, ids: &[String], max_bytes: u64) -> Result<Vec<(String, EmailBody)>> {
        let acc = self.mail_account()?;
        let args = json!({
            "accountId": acc,
            "ids": ids,
            "properties": ["id", "blobId", "messageId", "references", "replyTo", "sentAt", "to", "cc", "bodyValues", "textBody", "htmlBody", "attachments"],
            "fetchTextBodyValues": true,
            "fetchHTMLBodyValues": true,
            "maxBodyValueBytes": max_bytes,
        });
        let mut r = self.call(USING, &[inv("Email/get", args, "b")]).await?;
        let g: GetResponse<RawBody> = take(&mut r, "b")?;
        Ok(g.list.into_iter().map(|raw| (raw.id.clone(), raw.into_body())).collect())
    }

    /// Поставить/снять ключевое слово (`$seen`, `$flagged`, …).
    pub async fn set_keyword(&self, id: &str, keyword: &str, on: bool) -> Result<()> {
        self.email_patch(id, json!({ keyword_path(keyword): if on { Value::Bool(true) } else { Value::Null } })).await
    }

    /// Поставить `add` и снять `remove` у писем `ids`: один `Email/set` на [`SET_CHUNK`] писем, патч
    /// `keywords/<слово>` — другие ключевые слова письма не трогаются. Ответ — письма, которые сервер не изменил,
    /// с ошибкой (`notUpdated`); ошибка всего запроса — `Err`.
    pub async fn set_keywords(&self, ids: &[String], add: &[String], remove: &[String]) -> Result<Vec<(String, Error)>> {
        let acc = self.mail_account()?;
        let patch = keywords_patch(add, remove);
        let mut failed = Vec::new();
        for chunk in ids.chunks(SET_CHUNK) {
            let update: serde_json::Map<String, Value> = chunk.iter().map(|id| (id.clone(), patch.clone())).collect();
            let mut r = self.call(USING, &[inv("Email/set", json!({ "accountId": acc, "update": update }), "s")]).await?;
            let v: Value = take(&mut r, "s")?;
            for id in chunk {
                if let Err(e) = set_error(&v, "notUpdated", id, "Email/set") {
                    failed.push((id.clone(), e));
                }
            }
        }
        Ok(failed)
    }

    /// Свой патч каждому письму (`ids` и патчи — парами), один `Email/set` на [`SET_CHUNK`] писем: жизненный цикл
    /// (S5-04) переносит, помечает и снимает сроки пачкой. Ответ — письма, которые сервер не изменил.
    pub async fn patch_emails(&self, patches: &[(String, EmailPatch)]) -> Result<Vec<(String, Error)>> {
        let acc = self.mail_account()?;
        let mut failed = Vec::new();
        for chunk in patches.chunks(SET_CHUNK) {
            let update: serde_json::Map<String, Value> =
                chunk.iter().filter(|(_, p)| !p.is_empty()).map(|(id, p)| (id.clone(), p.to_json())).collect();
            if update.is_empty() {
                continue;
            }
            let mut r = self.call(USING, &[inv("Email/set", json!({ "accountId": acc, "update": update }), "s")]).await?;
            let v: Value = take(&mut r, "s")?;
            for (id, _) in chunk {
                if let Err(e) = set_error(&v, "notUpdated", id, "Email/set") {
                    failed.push((id.clone(), e));
                }
            }
        }
        Ok(failed)
    }

    /// Письма по фильтру с признаком рассылки (S5-04), новые сверху, до `limit`: есть ли `List-Id` или
    /// `List-Unsubscribe`. Заголовки — в сыром виде: условие `header` в `Email/query` Stalwart не выполняет (ни одного
    /// совпадения), а `header:List-Id:asText` отдаёт `null` (`docs/STALWART-GAPS.md`).
    pub async fn list_letters(&self, filter: &Filter, limit: u64) -> Result<Vec<ListLetter>> {
        let acc = self.mail_account()?;
        let query = json!({
            "accountId": acc,
            "filter": filter.to_json(),
            "sort": [{ "property": "receivedAt", "isAscending": false }],
            "limit": limit,
        });
        let get = json!({
            "accountId": acc,
            "#ids": { "resultOf": "q", "name": "Email/query", "path": "/ids" },
            "properties": ["id", "from", "receivedAt", "keywords", "header:List-Id", "header:List-Unsubscribe"],
        });
        let mut r = self.call(USING, &[inv("Email/query", query, "q"), inv("Email/get", get, "g")]).await?;
        let g: GetResponse<RawListLetter> = take(&mut r, "g")?;
        Ok(g.list
            .into_iter()
            .map(|e| ListLetter {
                from: e.from.as_ref().and_then(|f| f.first()).and_then(|a| a.email.clone()).unwrap_or_default(),
                received_at: utc_to_unix(&e.received_at).unwrap_or(0),
                seen: e.keywords.get("$seen").copied().unwrap_or(false),
                list: e.list_id.as_deref().is_some_and(|v| !v.trim().is_empty()) || e.unsubscribe.as_deref().is_some_and(|v| !v.trim().is_empty()),
                id: e.id,
            })
            .collect())
    }

    /// Письма целиком (S5-08, пересылка вложением — «Сообщить о фишинге»): id, `blobId` сообщения RFC 5322 и тема.
    /// Писем, которых нет, в ответе нет.
    pub async fn email_blobs(&self, ids: &[String]) -> Result<Vec<EmailBlob>> {
        let acc = self.mail_account()?;
        let args = json!({ "accountId": acc, "ids": ids, "properties": ["id", "blobId", "subject"] });
        let mut r = self.call(USING, &[inv("Email/get", args, "g")]).await?;
        let g: GetResponse<EmailBlob> = take(&mut r, "g")?;
        Ok(g.list)
    }

    /// Создать папку верхнего уровня с ролью (`archive` — «Архив» для жизненного цикла, S5-04) → id.
    pub async fn create_mailbox(&self, name: &str, role: Option<&str>) -> Result<String> {
        self.create_mailbox_in(None, name, role).await
    }

    /// Создать папку в `parent` (`None` — верхний уровень), с ролью или без → id.
    pub async fn create_mailbox_in(&self, parent: Option<&str>, name: &str, role: Option<&str>) -> Result<String> {
        let acc = self.mail_account()?;
        let args = json!({ "accountId": acc, "create": { "m": mailbox_create(parent, name, role) } });
        let mut r = self.call(USING, &[inv("Mailbox/set", args, "s")]).await?;
        let v: Value = take(&mut r, "s")?;
        set_error(&v, "notCreated", "m", "Mailbox/set")?;
        v["created"]["m"]["id"].as_str().map(str::to_owned).ok_or_else(|| Error::Protocol("Mailbox/set: no id".into()))
    }

    /// Переименовать и/или перенести папку (`Mailbox/set` update, T1-04а): `name` — новое имя, `parent` — новый
    /// родитель (`Some(None)` — на верхний уровень). `None` — поле не меняется.
    pub async fn update_mailbox(&self, id: &str, name: Option<&str>, parent: Option<Option<&str>>) -> Result<()> {
        let acc = self.mail_account()?;
        let args = json!({ "accountId": acc, "update": { id: mailbox_update(name, parent) } });
        let mut r = self.call(USING, &[inv("Mailbox/set", args, "s")]).await?;
        let v: Value = take(&mut r, "s")?;
        set_error(&v, "notUpdated", id, "Mailbox/set")
    }

    /// Удалить пустую папку.
    pub async fn destroy_mailbox(&self, id: &str) -> Result<()> {
        let acc = self.mail_account()?;
        let mut r = self.call(USING, &[inv("Mailbox/set", json!({ "accountId": acc, "destroy": [id] }), "s")]).await?;
        let v: Value = take(&mut r, "s")?;
        set_error(&v, "notDestroyed", id, "Mailbox/set")
    }

    /// Только id писем по фильтру (без кратких записей), новые сверху, до `limit`: планировщик жизненного цикла.
    pub async fn query_ids(&self, filter: &Filter, limit: u64) -> Result<Vec<String>> {
        let acc = self.mail_account()?;
        let args = json!({
            "accountId": acc,
            "filter": filter.to_json(),
            "sort": [{ "property": "receivedAt", "isAscending": false }],
            "limit": limit,
        });
        let mut r = self.call(USING, &[inv("Email/query", args, "q")]).await?;
        let q: QueryResponse = take(&mut r, "q")?;
        Ok(q.ids)
    }

    /// Письма по фильтру, новые сверху (фильтры панели, «Все папки» — без `inMailbox`).
    pub async fn filtered(&self, filter: &Filter, limit: u64) -> Result<EmailPage> {
        self.query_page(filter.to_json(), Some(0), None, limit).await
    }

    /// Число писем по каждому фильтру — `Email/query` с `calculateTotal` и `limit: 0` (без id), до
    /// [`CALLS_CHUNK`] вызовов в запросе: счётчики «N из M» и фильтров панели.
    pub async fn count(&self, filters: &[Filter]) -> Result<Vec<u64>> {
        let acc = self.mail_account()?;
        let mut out = Vec::with_capacity(filters.len());
        for chunk in filters.chunks(CALLS_CHUNK) {
            let calls: Vec<Invocation> = chunk
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let args = json!({ "accountId": acc, "filter": f.to_json(), "limit": 0, "calculateTotal": true });
                    inv("Email/query", args, &format!("c{i}"))
                })
                .collect();
            let mut r = self.call(USING, &calls).await?;
            for i in 0..chunk.len() {
                let q: QueryResponse = take(&mut r, &format!("c{i}"))?;
                out.push(q.total.unwrap_or(0));
            }
        }
        Ok(out)
    }

    pub async fn move_email(&self, id: &str, from: &str, to: &str) -> Result<()> {
        self.email_patch(id, json!({ format!("mailboxIds/{from}"): null, format!("mailboxIds/{to}"): true })).await
    }

    pub async fn destroy_email(&self, id: &str) -> Result<()> {
        let acc = self.mail_account()?;
        let mut r = self.call(USING, &[inv("Email/set", json!({ "accountId": acc, "destroy": [id] }), "s")]).await?;
        check_set(&mut r, id, "notDestroyed")
    }

    async fn email_patch(&self, id: &str, patch: Value) -> Result<()> {
        let acc = self.mail_account()?;
        let args = json!({ "accountId": acc, "update": { id: patch } });
        let mut r = self.call(USING, &[inv("Email/set", args, "s")]).await?;
        check_set(&mut r, id, "notUpdated")
    }
}

fn check_set(r: &mut [Invocation], id: &str, field: &str) -> Result<()> {
    let v: Value = take(r, "s")?;
    set_error(&v, field, id, "Email/set")
}

/// Новая папка для `Mailbox/set` `create`: имя, родитель (`null` — верхний уровень), роль.
pub(crate) fn mailbox_create(parent: Option<&str>, name: &str, role: Option<&str>) -> Value {
    let mut mb = json!({ "name": name, "parentId": parent });
    if let Some(r) = role {
        mb["role"] = json!(r);
    }
    mb
}

/// Патч папки для `Mailbox/set` `update`: только названные поля; `parentId: null` — на верхний уровень.
pub(crate) fn mailbox_update(name: Option<&str>, parent: Option<Option<&str>>) -> Value {
    let mut patch = json!({});
    if let Some(n) = name {
        patch["name"] = json!(n);
    }
    if let Some(p) = parent {
        patch["parentId"] = json!(p);
    }
    patch
}

/// Ошибка из `notCreated`/`notUpdated`/`notDestroyed` по ключу.
pub(crate) fn set_error(v: &Value, field: &str, key: &str, method: &str) -> Result<()> {
    if let Some(err) = v.get(field).and_then(|m| m.get(key)) {
        return Err(crate::Error::Method {
            method: method.into(),
            kind: err["type"].as_str().unwrap_or("?").into(),
            description: err["description"].as_str().unwrap_or("").into(),
        });
    }
    Ok(())
}

/// JMAP UTCDate (`2024-05-01T12:00:00Z`) -> Unix-время в секундах.
pub fn utc_to_unix(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (hh, mm, ss) = (n(11..13)?, n(14..16)?, n(17..19)?);
    // Дни от эпохи по алгоритму Howard Hinnant (days_from_civil).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hh * 3600 + mm * 60 + ss)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_dates() {
        assert_eq!(utc_to_unix("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(utc_to_unix("2026-10-17T12:30:00Z"), Some(1_792_240_200));
        assert_eq!(utc_to_unix("2000-02-29T00:00:00Z"), Some(951_782_400));
        assert_eq!(utc_to_unix("мусор"), None);
    }

    #[test]
    fn mailbox_rights_default_to_all() {
        // Папка, открытая только для чтения (стенд, S4-06): прочие права — как прислал сервер.
        let m: Mailbox = serde_json::from_value(json!({
            "id": "a", "name": "Inbox", "parentId": null, "role": "inbox",
            "myRights": { "mayReadItems": true, "mayAddItems": false, "mayRemoveItems": false, "maySetSeen": true,
                          "maySetKeywords": true, "mayCreateChild": false, "maySubmit": false }
        }))
        .unwrap();
        assert!(m.my_rights.may_read_items && m.my_rights.may_set_seen);
        assert!(!m.my_rights.may_remove_items && !m.my_rights.may_add_items && !m.my_rights.may_submit);
        let own: Mailbox = serde_json::from_value(json!({ "id": "b", "name": "x", "parentId": null, "role": null })).unwrap();
        assert_eq!(own.my_rights, MailboxRights::default());
    }

    /// Патч меток (S5-02): только названные ключевые слова; `~` и `/` экранированы по JSON Pointer.
    #[test]
    fn keyword_patch_touches_only_named_keywords() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let patch = keywords_patch(&s(&["pm.cat.clients", "$flagged"]), &s(&["pm.cat.team"]));
        assert_eq!(patch, json!({ "keywords/pm.cat.clients": true, "keywords/$flagged": true, "keywords/pm.cat.team": null }));
        assert_eq!(keyword_path("a/b~c"), "keywords/a~1b~0c");
        assert_eq!(keywords_patch(&[], &[]), json!({}));
        // Снять и поставить одно слово — ставится (добавления — после снятий).
        assert_eq!(keywords_patch(&s(&["x"]), &s(&["x"])), json!({ "keywords/x": true }));
    }

    #[test]
    fn filters_become_query_conditions() {
        let f = Filter::And(vec![
            Filter::InMailbox("a".into()),
            Filter::Or(vec![Filter::HasKeyword("pm.cat.clients".into()), Filter::HasKeyword("$flagged".into())]),
            Filter::NotKeyword("$seen".into()),
        ]);
        assert_eq!(
            f.to_json(),
            json!({ "operator": "AND", "conditions": [
                { "inMailbox": "a" },
                { "operator": "OR", "conditions": [{ "hasKeyword": "pm.cat.clients" }, { "hasKeyword": "$flagged" }] },
                { "notKeyword": "$seen" },
            ]})
        );
        assert_eq!(Filter::Text("x".into()).to_json(), json!({ "text": "x" }));
        assert_eq!(
            Filter::Not(vec![Filter::From("@corp.example".into())]).to_json(),
            json!({ "operator": "NOT", "conditions": [{ "from": "@corp.example" }] })
        );
        assert_eq!(Filter::HasAttachment(true).to_json(), json!({ "hasAttachment": true }));
        assert_eq!(Filter::To("me@x".into()).to_json(), json!({ "to": "me@x" }));
        assert_eq!(Filter::Body("@anna".into()).to_json(), json!({ "body": "@anna" }));
        assert_eq!(Filter::Before("2026-10-21T00:00:00Z".into()).to_json(), json!({ "before": "2026-10-21T00:00:00Z" }));
        assert_eq!(Filter::After("2026-10-01T00:00:00Z".into()).to_json(), json!({ "after": "2026-10-01T00:00:00Z" }));
        assert_eq!(Filter::Subject("ИБ".into()).to_json(), json!({ "subject": "ИБ" }));
    }

    /// Патч письма (S5-04): папки и слова путями, снять — `null`.
    #[test]
    fn email_patch_paths() {
        let p = EmailPatch::default().mailbox("in", false).mailbox("arch", true).keyword("$seen", false).keyword("pm.remind", true);
        assert_eq!(p.to_json(), json!({ "mailboxIds/in": null, "mailboxIds/arch": true, "keywords/$seen": null, "keywords/pm.remind": true }));
        assert!(EmailPatch::default().is_empty());
    }

    /// Создать, переименовать, перенести папку (T1-04а): `Mailbox/set` с `parentId` и патчем только нужных полей.
    #[test]
    fn mailbox_set_commands() {
        assert_eq!(mailbox_create(None, "Архив", Some("archive")), json!({ "name": "Архив", "parentId": null, "role": "archive" }));
        assert_eq!(mailbox_create(Some("in"), "Проект", None), json!({ "name": "Проект", "parentId": "in" }));
        assert_eq!(mailbox_update(Some("Новое"), None), json!({ "name": "Новое" }), "переименовать — только имя");
        assert_eq!(mailbox_update(None, Some(Some("arch"))), json!({ "parentId": "arch" }), "перенести — только родитель");
        assert_eq!(mailbox_update(None, Some(None)), json!({ "parentId": null }), "на верхний уровень");
        assert_eq!(mailbox_update(Some("x"), Some(Some("p"))), json!({ "name": "x", "parentId": "p" }));
    }

    #[test]
    fn invocation_is_array() {
        let s = serde_json::to_string(&inv("Mailbox/get", json!({}), "m")).unwrap();
        assert_eq!(s, r#"["Mailbox/get",{},"m"]"#);
    }
}
