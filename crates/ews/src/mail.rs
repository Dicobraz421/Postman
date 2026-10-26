//! Почта Exchange по EWS (решение владельца 2026-10-23: почта Exchange идёт через EWS, не через IMAP/SMTP — у
//! тестового Exchange компании IMAP4 и приём SMTP выключены, см. `docs/EXCHANGE.md`). Договор — как у календаря:
//! дерево папок с ролями, изменения опросом, письмо целиком с `MimeContent` (разбирает воркер плагина позже),
//! прочитано/флаг, перенос и удаление, отправка и черновики, поиск, крупные вложения.
//!
//! - **Дерево папок** — `SyncFolderHierarchy`; роли — по стандартным папкам (`GetFolder` их id сопоставляет
//!   `ItemId` папки с ролью).
//! - **Письма папки** — `SyncFolderItems` (`sync_mail`): новые, изменённые, удалённые и смена «прочитано»
//!   (`ReadFlagChange`); состояние непрозрачно, как у календаря.
//! - **Список** — `mail_headers`: тема, отправитель, дата, размер, прочитано, флаг, вложения, `Message-ID`,
//!   `In-Reply-To`, `References` — без тела.
//! - **Письмо** — `get_mail`: `MimeContent` (RFC 5322 целиком) + прочитано; разбор MIME — воркер плагина.
//! - **Прочитано и флаг** — `UpdateItem` (`message:IsRead`, `item:Flag`).
//! - **Перенос и удаление** — `MoveItem`, `DeleteItem` (в «Удалённые» или насовсем).
//! - **Отправка** — `CreateItem` из MIME с `SendAndSaveCopy` (копия — в «Отправленные»); черновик — `SaveOnly`.
//! - **Поиск** — `FindItem` с `QueryString` (AQS; Exchange 2013+).
//! - **Вложения** — `GetAttachment` для крупных (в `MimeContent` вложения уже есть; метод — на случай выборки
//!   письма без MIME).

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;

use crate::calendar::parse_time;
use crate::soap::{self, text_el};
use crate::xml::{El, esc};
use crate::{BATCH, Client, Error, FolderId, FolderRef, ItemId, Result, SyncState, WellKnown};

/// Роль почтовой папки — по стандартной папке Exchange (`DistinguishedFolderId`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailRole {
    Inbox,
    Drafts,
    Sent,
    Trash,
    Junk,
    Outbox,
    Archive,
}

/// (роль, стандартный id) — порядок запроса `GetFolder` в [`Client::mail_roles`].
const ROLES: &[(MailRole, &str)] = &[
    (MailRole::Inbox, "inbox"),
    (MailRole::Drafts, "drafts"),
    (MailRole::Sent, "sentitems"),
    (MailRole::Trash, "deleteditems"),
    (MailRole::Junk, "junkemail"),
    (MailRole::Outbox, "outbox"),
    // «archive» (сетевой архив) не запрашиваем: на Exchange 2013 без архива `GetFolder` с этим id роняет весь
    // запрос `ErrorInternalServerError` (боевой сервер, docs/EXCHANGE.md). Папку «Архив» плагин метит по имени.
];

/// Почтовая папка дерева.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailFolder {
    pub id: FolderId,
    /// Родитель — его `Id`; `None` — верхний уровень ящика.
    pub parent: Option<String>,
    pub name: String,
    pub role: Option<MailRole>,
    pub total: u32,
    pub unread: u32,
}

/// Изменения дерева папок (`SyncFolderHierarchy`): создано и изменено — папка целиком, удалено — только `Id`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HierarchyChanges {
    pub state: SyncState,
    pub created: Vec<MailFolder>,
    pub updated: Vec<MailFolder>,
    pub deleted: Vec<String>,
    /// `false` — это не все изменения: запросить ещё с новым `state`.
    pub done: bool,
}

/// Изменения писем папки: как у календаря, плюс смена «прочитано» (`ReadFlagChange`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MailSync {
    pub state: SyncState,
    pub created: Vec<ItemId>,
    pub updated: Vec<ItemId>,
    pub deleted: Vec<String>,
    /// (`Id`, прочитано) — сервер отметил письмо прочитанным или непрочитанным, без других изменений.
    pub read_flags: Vec<(String, bool)>,
    pub done: bool,
}

/// Заголовок письма для списка (без тела).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailHeader {
    pub id: ItemId,
    /// Папка письма (`ParentFolderId`), если запрошена.
    pub folder_id: Option<String>,
    pub subject: String,
    pub from_name: String,
    pub from_addr: String,
    /// Unix-секунды (`DateTimeReceived`).
    pub received_at: i64,
    pub size: u32,
    pub is_read: bool,
    pub flagged: bool,
    pub has_attachments: bool,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
}

/// Письмо целиком: RFC 5322 (`MimeContent`), разбор — воркер плагина.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailItem {
    pub id: ItemId,
    pub mime: Vec<u8>,
    pub is_read: bool,
}

/// Вложение письма (из `GetItem` без MIME): id, имя, тип, размер, встроенное.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub id: String,
    pub name: String,
    pub content_type: String,
    pub size: u32,
    pub inline: bool,
}

/// Куда отправлять письмо при создании (`MessageDisposition`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Отправить и положить копию в «Отправленные».
    SendAndSaveCopy,
    /// Отправить без копии.
    SendOnly,
    /// Не отправлять — сохранить (черновик).
    SaveOnly,
}

impl Disposition {
    fn ews(self) -> &'static str {
        match self {
            Disposition::SendAndSaveCopy => "SendAndSaveCopy",
            Disposition::SendOnly => "SendOnly",
            Disposition::SaveOnly => "SaveOnly",
        }
    }
}

/// Свойства письма для списка (`mail_headers`): без тела и без MIME.
/// Доп. свойства списка сверх `Default` (тема, дата, размер, вложения, отправитель, прочитано). `Message-ID`,
/// `In-Reply-To` и `References` берёт из MIME воркер плагина: `message:*`-свойства GetItem на Exchange 2013 CU23
/// отвечают `ErrorInternalServerError` (боевой сервер, `docs/EXCHANGE.md`).
const HEADER_PROPS: &[&str] = &["item:Flag"];

fn field_uris(props: &[&str]) -> String {
    props.iter().map(|p| format!(r#"<t:FieldURI FieldURI="{p}"/>"#)).collect()
}

/// base64 с любыми пробелами (сервер переносит строки) → байты.
fn b64(text: &str) -> Vec<u8> {
    let packed: String = text.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    B64.decode(packed).unwrap_or_default()
}

/// (имя, адрес) из `<t:Mailbox>` (у `From` — вложенный `Mailbox`, у записи получателя — тоже).
fn mailbox(el: &El) -> (String, String) {
    let m = el.child("Mailbox").unwrap_or(el);
    (m.str_at(&["Name"]), m.str_at(&["EmailAddress"]))
}

fn mail_folder(el: &El) -> Option<MailFolder> {
    let id = el.child("FolderId")?;
    Some(MailFolder {
        id: FolderId { id: id.attr("Id")?.to_owned(), change_key: id.attr("ChangeKey").map(str::to_owned) },
        parent: el.child("ParentFolderId").and_then(|p| p.attr("Id")).map(str::to_owned),
        name: el.str_at(&["DisplayName"]),
        role: None, // роли проставляются по id из `mail_roles` (`apply_roles`)
        total: el.text_at(&["TotalCount"]).and_then(|t| t.parse().ok()).unwrap_or(0),
        unread: el.text_at(&["UnreadCount"]).and_then(|t| t.parse().ok()).unwrap_or(0),
    })
}

fn header(el: &El) -> Option<MailHeader> {
    let id = ItemId::from_el(el.child("ItemId")?)?;
    let (from_name, from_addr) = el.child("From").map(mailbox).unwrap_or_default();
    Some(MailHeader {
        id,
        folder_id: el.child("ParentFolderId").and_then(|p| p.attr("Id")).map(str::to_owned),
        subject: el.str_at(&["Subject"]),
        from_name,
        from_addr,
        received_at: ["DateTimeReceived", "DateTimeSent", "DateTimeCreated"].iter().find_map(|k| el.text_at(&[*k]).and_then(parse_time)).unwrap_or(0),
        size: el.text_at(&["Size"]).and_then(|s| s.parse().ok()).unwrap_or(0),
        is_read: el.bool_at(&["IsRead"]),
        flagged: el.at(&["Flag", "FlagStatus"]).map(|f| f.text.trim()) == Some("Flagged"),
        has_attachments: el.bool_at(&["HasAttachments"]),
        message_id: el.text_at(&["InternetMessageId"]).map(str::to_owned),
        in_reply_to: el.text_at(&["InReplyTo"]).map(str::to_owned),
        references: el.text_at(&["References"]).map(str::to_owned),
    })
}

fn attachment(el: &El) -> Option<Attachment> {
    Some(Attachment {
        id: el.at(&["AttachmentId"]).and_then(|a| a.attr("Id"))?.to_owned(),
        name: el.str_at(&["Name"]),
        content_type: el.str_at(&["ContentType"]),
        size: el.text_at(&["Size"]).and_then(|s| s.parse().ok()).unwrap_or(0),
        inline: el.bool_at(&["IsInline"]),
    })
}

/// Письма в ответе `GetItem`: обычные (`Message`) и письма встреч — приглашение, ответ, отмена (`MeetingRequest`,
/// `MeetingResponse`, `MeetingCancellation`, `MeetingMessage`). Письма встреч — тоже письма (`MessageType` схемы), у них
/// та же тема, отправитель, «прочитано» и MIME с частью `text/calendar`: по ней карточка приглашения в письме.
const MESSAGES: [&str; 5] = ["Message", "MeetingRequest", "MeetingResponse", "MeetingCancellation", "MeetingMessage"];

fn messages(items: Option<&El>) -> impl Iterator<Item = &El> {
    items.into_iter().flat_map(|i| i.children.iter()).filter(|m| MESSAGES.contains(&m.name.as_str()))
}

/// Проставить роли папкам по стандартным id (`id` папки совпадает с ролью из [`Client::mail_roles`]).
pub fn apply_roles(folders: &mut [MailFolder], roles: &[(MailRole, FolderId)]) {
    for f in folders {
        f.role = roles.iter().find(|(_, id)| id.id == f.id.id).map(|(r, _)| *r);
    }
}

pub(crate) mod parse {
    //! Разбор ответов почты — отдельно от запросов, для тестов на корпусе.
    use super::*;

    pub(crate) fn hierarchy(resp: &El) -> Result<HierarchyChanges> {
        let msg = soap::single(resp)?;
        let mut out = HierarchyChanges {
            state: SyncState(msg.str_at(&["SyncState"])),
            done: msg.bool_at(&["IncludesLastFolderInRange"]),
            ..HierarchyChanges::default()
        };
        for c in msg.child("Changes").into_iter().flat_map(|c| c.children.iter()) {
            match c.name.as_str() {
                "Create" | "Update" => {
                    let Some(f) = c.children.first().and_then(mail_folder) else { continue };
                    if c.name == "Create" { out.created.push(f) } else { out.updated.push(f) }
                }
                "Delete" => out.deleted.extend(c.child("FolderId").and_then(|i| i.attr("Id")).map(str::to_owned)),
                _ => {}
            }
        }
        Ok(out)
    }

    /// Дерево `FindFolder` (Deep): почтовые папки (класс `IPF.Note` или без класса); родитель `root` — верхний
    /// уровень (`None`).
    pub(crate) fn find_folders(resp: &El, root: &str) -> Result<Vec<MailFolder>> {
        let msg = soap::single(resp)?;
        Ok(msg
            .at(&["RootFolder", "Folders"])
            .into_iter()
            .flat_map(|f| f.children.iter())
            .filter(|f| f.name == "Folder" && f.text_at(&["FolderClass"]).is_none_or(|c| c.is_empty() || c.starts_with("IPF.Note")))
            .filter_map(mail_folder)
            .map(|mut f| {
                if f.parent.as_deref() == Some(root) {
                    f.parent = None;
                }
                f
            })
            .collect())
    }

    /// Id корня из `GetFolder`.
    pub(crate) fn folder_id(resp: &El) -> Result<String> {
        let msg = soap::single(resp)?;
        msg.at(&["Folders"])
            .and_then(|f| f.children.first())
            .and_then(|f| f.child("FolderId"))
            .and_then(|i| i.attr("Id"))
            .map(str::to_owned)
            .ok_or_else(|| Error::Xml("GetFolder: no folder".into()))
    }

    /// Страница `FindItem`: id по порядку и есть ли ещё.
    pub(crate) fn page(resp: &El) -> Result<(Vec<ItemId>, bool)> {
        let msg = soap::single(resp)?;
        let root = msg.child("RootFolder").ok_or_else(|| Error::Xml("FindItem: no RootFolder".into()))?;
        let ids =
            root.child("Items").into_iter().flat_map(|i| i.children.iter()).filter_map(|m| m.child("ItemId").and_then(ItemId::from_el)).collect();
        Ok((ids, root.attr("IncludesLastItemInRange") == Some("false")))
    }

    pub(crate) fn roles(resp: &El) -> Vec<(MailRole, FolderId)> {
        // Сообщения — в порядке запроса (ROLES); не найденная папка (нет такой в 2013) — пропускается.
        let mut out = Vec::new();
        for (msg, (role, _)) in soap::messages(resp).into_iter().zip(ROLES) {
            if soap::code(msg) == "NoError"
                && let Some(fid) = msg.at(&["Folders"]).and_then(|f| f.children.first()).and_then(|f| f.child("FolderId"))
                && let Some(id) = fid.attr("Id")
            {
                out.push((*role, FolderId { id: id.to_owned(), change_key: fid.attr("ChangeKey").map(str::to_owned) }));
            }
        }
        out
    }

    pub(crate) fn sync(resp: &El) -> Result<MailSync> {
        let msg = soap::single(resp)?;
        let mut out =
            MailSync { state: SyncState(msg.str_at(&["SyncState"])), done: msg.bool_at(&["IncludesLastItemInRange"]), ..MailSync::default() };
        for c in msg.child("Changes").into_iter().flat_map(|c| c.children.iter()) {
            match c.name.as_str() {
                "Create" | "Update" => {
                    let Some(id) = c.children.first().and_then(|i| i.child("ItemId")).and_then(ItemId::from_el) else { continue };
                    if c.name == "Create" { out.created.push(id) } else { out.updated.push(id) }
                }
                "Delete" => out.deleted.extend(c.child("ItemId").and_then(|i| i.attr("Id")).map(str::to_owned)),
                "ReadFlagChange" => {
                    if let Some(id) = c.child("ItemId").and_then(|i| i.attr("Id")) {
                        out.read_flags.push((id.to_owned(), c.bool_at(&["IsRead"])));
                    }
                }
                _ => {}
            }
        }
        Ok(out)
    }

    pub(crate) fn headers(resp: &El) -> Result<Vec<MailHeader>> {
        let mut out = Vec::new();
        for msg in soap::messages(resp) {
            if soap::code(msg) == "ErrorItemNotFound" {
                continue;
            }
            soap::check(msg)?;
            out.extend(messages(msg.child("Items")).filter_map(header));
        }
        Ok(out)
    }

    pub(crate) fn mail(resp: &El) -> Result<Vec<MailItem>> {
        let mut out = Vec::new();
        for msg in soap::messages(resp) {
            if soap::code(msg) == "ErrorItemNotFound" {
                continue;
            }
            soap::check(msg)?;
            for m in messages(msg.child("Items")) {
                let Some(id) = m.child("ItemId").and_then(ItemId::from_el) else { continue };
                out.push(MailItem { id, mime: b64(&m.str_at(&["MimeContent"])), is_read: m.bool_at(&["IsRead"]) });
            }
        }
        Ok(out)
    }

    /// Новые id после `MoveItem` (в порядке запроса; у перенесённого — новый `Id` в целевой папке).
    pub(crate) fn moved(resp: &El) -> Result<Vec<ItemId>> {
        let mut out = Vec::new();
        for msg in soap::messages(resp) {
            soap::check(msg)?;
            out.extend(msg.child("Items").into_iter().flat_map(|i| i.children.iter()).filter_map(|m| m.child("ItemId")).filter_map(ItemId::from_el));
        }
        Ok(out)
    }

    pub(crate) fn attachments(resp: &El) -> Result<Vec<Attachment>> {
        let msg = soap::single(resp)?;
        Ok(messages(msg.child("Items"))
            .flat_map(|m| m.child("Attachments").into_iter().flat_map(|a| a.children.iter()))
            .filter_map(attachment)
            .collect())
    }

    /// Содержимое вложения (`GetAttachment` → `FileAttachment/Content`).
    pub(crate) fn attachment_content(resp: &El) -> Result<Vec<u8>> {
        let msg = soap::single(resp)?;
        let content = msg
            .at(&["Attachments", "FileAttachment"])
            .map(|a| a.str_at(&["Content"]))
            .ok_or_else(|| Error::Xml("GetAttachment: no content".into()))?;
        Ok(b64(&content))
    }

    /// Первый id из `<m:Items>` (черновик `SaveOnly`).
    pub(crate) fn saved(resp: &El) -> Result<ItemId> {
        let msg = soap::single(resp)?;
        msg.child("Items")
            .and_then(|i| i.children.first())
            .and_then(|m| m.child("ItemId"))
            .and_then(ItemId::from_el)
            .ok_or_else(|| Error::Xml("no ItemId in response".into()))
    }

    pub(crate) fn updated(resp: &El, fallback: &ItemId) -> ItemId {
        soap::single(resp)
            .ok()
            .and_then(|m| m.child("Items"))
            .and_then(|i| i.children.first())
            .and_then(|m| m.child("ItemId"))
            .and_then(ItemId::from_el)
            .unwrap_or_else(|| fallback.clone())
    }

    pub(crate) fn all_ok(resp: &El) -> Result<()> {
        for msg in soap::messages(resp) {
            if soap::code(msg) != "ErrorItemNotFound" {
                soap::check(msg)?;
            }
        }
        Ok(())
    }
}

/// `<t:Message>` с одним изменённым полем — для `UpdateItem`.
fn set_field(uri: &str, inner: String) -> String {
    format!(r#"<t:SetItemField><t:FieldURI FieldURI="{uri}"/><t:Message>{inner}</t:Message></t:SetItemField>"#)
}

impl Client {
    /// Дерево почтовых папок с ролями под корнем IPM (`msgfolderroot`; без него Exchange 2013 обходит и
    /// служебный корень и отвечает `ErrorInternalServerError` — боевой сервер, `docs/EXCHANGE.md`). `state` —
    /// `None` с начала (все папки как созданные); `roles` — из
    /// [`Client::mail_roles`] (роли проставляются по id). `done == false` — повторить с новым состоянием.
    pub async fn sync_folders(&mut self, state: Option<&SyncState>, roles: &[(MailRole, FolderId)]) -> Result<HierarchyChanges> {
        let state = state.filter(|s| !s.0.is_empty()).map(|s| text_el("m:SyncState", &s.0)).unwrap_or_default();
        let body = format!(
            r#"<m:SyncFolderHierarchy><m:FolderShape><t:BaseShape>Default</t:BaseShape></m:FolderShape><m:SyncFolderId><t:DistinguishedFolderId Id="msgfolderroot"/></m:SyncFolderId>{state}</m:SyncFolderHierarchy>"#
        );
        let mut changes = parse::hierarchy(&self.call("SyncFolderHierarchy", &body).await?)?;
        apply_roles(&mut changes.created, roles);
        apply_roles(&mut changes.updated, roles);
        Ok(changes)
    }

    /// Дерево почтовых папок обходом `FindFolder` (Deep) под корнем IPM — запасной путь, когда
    /// `SyncFolderHierarchy` не работает (боевой Exchange 2013 CU23 отвечает на него `ErrorInternalServerError`,
    /// `docs/EXCHANGE.md`). Роли — по `roles`, как у [`Client::sync_folders`]; изменений не отслеживает — дерево
    /// целиком.
    pub async fn find_folders(&mut self, roles: &[(MailRole, FolderId)]) -> Result<Vec<MailFolder>> {
        let body = r#"<m:GetFolder><m:FolderShape><t:BaseShape>IdOnly</t:BaseShape></m:FolderShape><m:FolderIds><t:DistinguishedFolderId Id="msgfolderroot"/></m:FolderIds></m:GetFolder>"#;
        let root = parse::folder_id(&self.call("GetFolder", body).await?)?;
        let body = r#"<m:FindFolder Traversal="Deep"><m:FolderShape><t:BaseShape>Default</t:BaseShape><t:AdditionalProperties><t:FieldURI FieldURI="folder:ParentFolderId"/><t:FieldURI FieldURI="folder:FolderClass"/></t:AdditionalProperties></m:FolderShape><m:IndexedPageFolderView MaxEntriesReturned="1000" Offset="0" BasePoint="Beginning"/><m:ParentFolderIds><t:DistinguishedFolderId Id="msgfolderroot"/></m:ParentFolderIds></m:FindFolder>"#;
        let mut folders = parse::find_folders(&self.call("FindFolder", body).await?, &root)?;
        apply_roles(&mut folders, roles);
        Ok(folders)
    }

    /// Письма папки по убыванию даты получения, страницей с `offset` (первая загрузка и «Загрузить ещё»): id и есть
    /// ли ещё.
    pub async fn mail_page(&mut self, folder: &FolderRef, offset: u32, max: u32) -> Result<(Vec<ItemId>, bool)> {
        let body = format!(
            r#"<m:FindItem Traversal="Shallow"><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape></m:ItemShape><m:IndexedPageItemView MaxEntriesReturned="{}" Offset="{offset}" BasePoint="Beginning"/><m:SortOrder><t:FieldOrder Order="Descending"><t:FieldURI FieldURI="item:DateTimeReceived"/></t:FieldOrder></m:SortOrder><m:ParentFolderIds>{}</m:ParentFolderIds></m:FindItem>"#,
            max.clamp(1, 1000),
            folder.xml()
        );
        parse::page(&self.call("FindItem", &body).await?)
    }

    /// Id стандартных папок (вход, черновики, отправленные, удалённые, спам, исходящие, архив) → роль. Папки нет в
    /// этой версии Exchange — пропускается.
    pub async fn mail_roles(&mut self) -> Result<Vec<(MailRole, FolderId)>> {
        let ids: String = ROLES.iter().map(|(_, id)| format!(r#"<t:DistinguishedFolderId Id="{id}"/>"#)).collect();
        let body =
            format!(r#"<m:GetFolder><m:FolderShape><t:BaseShape>IdOnly</t:BaseShape></m:FolderShape><m:FolderIds>{ids}</m:FolderIds></m:GetFolder>"#);
        Ok(parse::roles(&self.call("GetFolder", &body).await?))
    }

    /// Письма папки с прошлого состояния (`SyncFolderItems`): новые, изменённые, удалённые, смена «прочитано».
    pub async fn sync_mail(&mut self, folder: &FolderRef, state: Option<&SyncState>, max: u32) -> Result<MailSync> {
        let state = state.filter(|s| !s.0.is_empty()).map(|s| text_el("m:SyncState", &s.0)).unwrap_or_default();
        let body = format!(
            r#"<m:SyncFolderItems><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape></m:ItemShape><m:SyncFolderId>{}</m:SyncFolderId>{state}<m:MaxChangesReturned>{}</m:MaxChangesReturned><m:SyncScope>NormalItems</m:SyncScope></m:SyncFolderItems>"#,
            folder.xml(),
            max.clamp(1, 512)
        );
        parse::sync(&self.call("SyncFolderItems", &body).await?)
    }

    /// Заголовки писем для списка (пачками по [`BATCH`]); без тела. Удалённые за это время — пропускаются.
    pub async fn mail_headers(&mut self, ids: &[ItemId]) -> Result<Vec<MailHeader>> {
        let mut out = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(BATCH) {
            let ids: String = chunk.iter().map(|i| i.xml("t:ItemId")).collect();
            let extra = if HEADER_PROPS.is_empty() {
                String::new()
            } else {
                format!("<t:AdditionalProperties>{}</t:AdditionalProperties>", field_uris(HEADER_PROPS))
            };
            let body =
                format!(r#"<m:GetItem><m:ItemShape><t:BaseShape>Default</t:BaseShape>{extra}</m:ItemShape><m:ItemIds>{ids}</m:ItemIds></m:GetItem>"#);
            out.extend(parse::headers(&self.call("GetItem", &body).await?)?);
        }
        Ok(out)
    }

    /// Письмо целиком (`MimeContent`) — для чтения; разбор MIME — воркер плагина. Не отмечает прочитанным.
    pub async fn get_mail(&mut self, id: &ItemId) -> Result<Option<MailItem>> {
        let body = format!(
            r#"<m:GetItem><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape><t:IncludeMimeContent>true</t:IncludeMimeContent><t:AdditionalProperties><t:FieldURI FieldURI="message:IsRead"/></t:AdditionalProperties></m:ItemShape><m:ItemIds>{}</m:ItemIds></m:GetItem>"#,
            id.xml("t:ItemId")
        );
        Ok(parse::mail(&self.call("GetItem", &body).await?)?.into_iter().next())
    }

    /// Вложения письма (без MIME): их список — для выборки крупных через [`Client::get_attachment`].
    pub async fn attachments(&mut self, id: &ItemId) -> Result<Vec<Attachment>> {
        let body = format!(
            r#"<m:GetItem><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape><t:AdditionalProperties><t:FieldURI FieldURI="item:Attachments"/></t:AdditionalProperties></m:ItemShape><m:ItemIds>{}</m:ItemIds></m:GetItem>"#,
            id.xml("t:ItemId")
        );
        parse::attachments(&self.call("GetItem", &body).await?)
    }

    /// Содержимое вложения по его id (`GetAttachment`) — для крупных файлов по запросу.
    pub async fn get_attachment(&mut self, attachment_id: &str) -> Result<Vec<u8>> {
        let body = format!(
            r#"<m:GetAttachment><m:AttachmentShape><t:IncludeMimeContent>true</t:IncludeMimeContent></m:AttachmentShape><m:AttachmentIds><t:AttachmentId Id="{}"/></m:AttachmentIds></m:GetAttachment>"#,
            esc(attachment_id)
        );
        parse::attachment_content(&self.call("GetAttachment", &body).await?)
    }

    /// Отметить письмо прочитанным или непрочитанным (`message:IsRead`). Ответ — новая версия.
    pub async fn set_read(&mut self, id: &ItemId, read: bool) -> Result<ItemId> {
        let update = set_field("message:IsRead", format!("<t:IsRead>{read}</t:IsRead>"));
        self.update_item(id, &update).await
    }

    /// Поставить или снять флаг (`item:Flag`). Ответ — новая версия.
    pub async fn set_flag(&mut self, id: &ItemId, flagged: bool) -> Result<ItemId> {
        let status = if flagged { "Flagged" } else { "NotFlagged" };
        let update = set_field("item:Flag", format!("<t:Flag><t:FlagStatus>{status}</t:FlagStatus></t:Flag>"));
        self.update_item(id, &update).await
    }

    async fn update_item(&mut self, id: &ItemId, updates: &str) -> Result<ItemId> {
        let body = format!(
            r#"<m:UpdateItem ConflictResolution="AlwaysOverwrite" MessageDisposition="SaveOnly"><m:ItemChanges><t:ItemChange>{}<t:Updates>{updates}</t:Updates></t:ItemChange></m:ItemChanges></m:UpdateItem>"#,
            id.xml("t:ItemId")
        );
        let resp = self.call("UpdateItem", &body).await?;
        soap::single(&resp)?;
        Ok(parse::updated(&resp, id))
    }

    /// Перенести письма в папку (`MoveItem`) — возвращает их новые id в целевой папке.
    pub async fn move_mail(&mut self, ids: &[ItemId], to: &FolderRef) -> Result<Vec<ItemId>> {
        let mut out = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(BATCH) {
            let item_ids: String = chunk.iter().map(|i| i.xml("t:ItemId")).collect();
            let body = format!(r#"<m:MoveItem><m:ToFolderId>{}</m:ToFolderId><m:ItemIds>{item_ids}</m:ItemIds></m:MoveItem>"#, to.xml());
            out.extend(parse::moved(&self.call("MoveItem", &body).await?)?);
        }
        Ok(out)
    }

    /// Удалить письма (`DeleteItem`): в «Удалённые» или насовсем. Уже удалённое — не ошибка.
    pub async fn delete_mail(&mut self, ids: &[ItemId], mode: crate::DeleteMode) -> Result<()> {
        for chunk in ids.chunks(BATCH) {
            let item_ids: String = chunk.iter().map(|i| i.xml("t:ItemId")).collect();
            let body = format!(r#"<m:DeleteItem DeleteType="{}"><m:ItemIds>{item_ids}</m:ItemIds></m:DeleteItem>"#, mode.ews());
            parse::all_ok(&self.call("DeleteItem", &body).await?)?;
        }
        Ok(())
    }

    /// Отправить письмо из готового MIME (RFC 5322, собрал компоновщик Postman): `SendAndSaveCopy` — копия в
    /// «Отправленные». Сервер шлёт — ответ без объекта.
    pub async fn send_mail(&mut self, mime: &[u8], save_copy: bool) -> Result<()> {
        let disposition = if save_copy { Disposition::SendAndSaveCopy } else { Disposition::SendOnly };
        let body = create_item(mime, disposition, None);
        parse::all_ok(&self.call("CreateItem", &body).await?)
    }

    /// Сохранить черновик из MIME (`SaveOnly` в «Черновики») — возвращает его id. `replace` — старый черновик,
    /// удаляется после сохранения нового (правка MIME на месте EWS не умеет).
    pub async fn save_draft(&mut self, mime: &[u8], replace: Option<&ItemId>) -> Result<ItemId> {
        let body = create_item(mime, Disposition::SaveOnly, Some(WellKnown::Drafts));
        let id = parse::saved(&self.call("CreateItem", &body).await?)?;
        if let Some(old) = replace {
            self.delete_mail(std::slice::from_ref(old), crate::DeleteMode::HardDelete).await?;
        }
        Ok(id)
    }

    /// Поиск писем в папке по строке AQS (`QueryString`, Exchange 2013+): id, не больше `max`.
    pub async fn search_mail(&mut self, folder: &FolderRef, query: &str, max: u32) -> Result<Vec<ItemId>> {
        let body = format!(
            r#"<m:FindItem Traversal="Shallow"><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape></m:ItemShape><m:IndexedPageItemView MaxEntriesReturned="{}" Offset="0" BasePoint="Beginning"/><m:ParentFolderIds>{}</m:ParentFolderIds><m:QueryString>{}</m:QueryString></m:FindItem>"#,
            max.max(1),
            folder.xml(),
            esc(query)
        );
        let resp = self.call("FindItem", &body).await?;
        let msg = soap::single(&resp)?;
        Ok(msg
            .at(&["RootFolder", "Items"])
            .into_iter()
            .flat_map(|i| i.children.iter())
            .filter_map(|m| m.child("ItemId").and_then(ItemId::from_el))
            .collect())
    }
}

fn create_item(mime: &[u8], disposition: Disposition, folder: Option<WellKnown>) -> String {
    let saved = folder
        .map(|w| format!(r#"<m:SavedItemFolderId><t:DistinguishedFolderId Id="{}"/></m:SavedItemFolderId>"#, well_known(w)))
        .unwrap_or_default();
    format!(
        r#"<m:CreateItem MessageDisposition="{}">{saved}<m:Items><t:Message><t:MimeContent>{}</t:MimeContent></t:Message></m:Items></m:CreateItem>"#,
        disposition.ews(),
        B64.encode(mime)
    )
}

fn well_known(w: WellKnown) -> &'static str {
    match w {
        WellKnown::Calendar => "calendar",
        WellKnown::Inbox => "inbox",
        WellKnown::Drafts => "drafts",
        WellKnown::SentItems => "sentitems",
        WellKnown::DeletedItems => "deleteditems",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml::parse as xml;

    fn resp(fixture: &[u8]) -> El {
        soap::response(xml(fixture).unwrap()).unwrap()
    }

    #[test]
    fn folder_hierarchy_with_roles() {
        let roles = parse::roles(&resp(include_bytes!("../tests/fixtures/get_folder_mail_roles.xml")));
        // «Архив» нет в 2013 — пропущен; остальные по порядку.
        assert_eq!(
            roles.iter().map(|(r, _)| *r).collect::<Vec<_>>(),
            [MailRole::Inbox, MailRole::Drafts, MailRole::Sent, MailRole::Trash, MailRole::Junk, MailRole::Outbox]
        );
        let mut h = parse::hierarchy(&resp(include_bytes!("../tests/fixtures/sync_folder_hierarchy.xml"))).unwrap();
        assert!(!h.done);
        assert_eq!((h.created.len(), h.updated.len(), h.deleted.len()), (3, 1, 1));
        apply_roles(&mut h.created, &roles);
        apply_roles(&mut h.updated, &roles);
        let inbox = h.created.iter().find(|f| f.role == Some(MailRole::Inbox)).expect("Входящие");
        assert_eq!((inbox.name.as_str(), inbox.total, inbox.unread), ("Inbox", 42, 3));
        let project = h.created.iter().find(|f| f.name == "Проект").unwrap();
        assert_eq!((project.role, project.parent.as_deref()), (None, Some("AAMkADinbox")));
        assert_eq!(h.deleted, ["AAMkADgone"]);
        assert_eq!(h.updated[0].unread, 1);
    }

    #[test]
    fn find_folder_tree_and_page() {
        let root = parse::folder_id(&resp(include_bytes!("../tests/fixtures/get_folder_msgroot.xml"))).unwrap();
        assert_eq!(root, "AAMkADroot");
        let roles = parse::roles(&resp(include_bytes!("../tests/fixtures/get_folder_mail_roles.xml")));
        let mut f = parse::find_folders(&resp(include_bytes!("../tests/fixtures/find_folder_mail.xml")), &root).unwrap();
        apply_roles(&mut f, &roles);
        // Календарь и контакты — не почта; «Проект» — во «Входящих».
        assert_eq!(f.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["Inbox", "Sent Items", "Проект"]);
        assert_eq!((f[0].role, f[0].parent.as_deref(), f[0].unread), (Some(MailRole::Inbox), None, 3));
        assert_eq!(f[2].parent.as_deref(), Some("AAMkADinbox"));
        let (ids, more) = parse::page(&resp(include_bytes!("../tests/fixtures/find_item_mail_page.xml"))).unwrap();
        assert_eq!(ids.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["AAMkADmsg2", "AAMkADmsg1"]);
        assert!(more);
    }

    #[test]
    fn mail_item_sync() {
        let s = parse::sync(&resp(include_bytes!("../tests/fixtures/sync_mail_items.xml"))).unwrap();
        assert_eq!(s.state.as_str(), "bWFpbC1zdGF0ZQ==");
        assert!(s.done);
        assert_eq!(s.created.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["AAMkADmsg1", "AAMkADmsg2"]);
        assert_eq!(s.updated.len(), 1);
        assert_eq!(s.deleted, ["AAMkADmsgDel"]);
        assert_eq!(s.read_flags, [("AAMkADmsgRead".to_owned(), true), ("AAMkADmsgUnread".to_owned(), false)]);
    }

    #[test]
    fn headers_for_the_list() {
        let h = parse::headers(&resp(include_bytes!("../tests/fixtures/get_item_headers.xml"))).unwrap();
        assert_eq!(h.len(), 2, "ErrorItemNotFound пропущен");
        let first = &h[0];
        assert_eq!(first.subject, "Отчёт за неделю");
        assert_eq!((first.from_name.as_str(), first.from_addr.as_str()), ("Анна Петрова", "anna@example.test"));
        assert_eq!(first.received_at, "2026-10-20T07:00:00Z".parse::<jiff::Timestamp>().unwrap().as_second());
        assert_eq!((first.size, first.is_read, first.flagged, first.has_attachments), (24576, false, true, true));
        assert_eq!(first.message_id.as_deref(), Some("<a1b2@example.test>"));
        assert_eq!(first.folder_id.as_deref(), Some("AAMkADinbox"));
        assert_eq!(first.references.as_deref(), Some("<root@example.test> <a0@example.test>"));
        assert!(h[1].is_read && !h[1].flagged);
    }

    /// Письма встреч (приглашение, отмена) — в списке и с MIME, как обычные: по ним карточка приглашения (S7-07).
    #[test]
    fn meeting_messages_are_mail() {
        let as_kind = |fixture: &[u8], kind: &str| {
            let text = String::from_utf8(fixture.to_vec())
                .unwrap()
                .replace("<t:Message>", &format!("<t:{kind}>"))
                .replace("</t:Message>", &format!("</t:{kind}>"));
            resp(text.as_bytes())
        };
        let h = parse::headers(&as_kind(include_bytes!("../tests/fixtures/get_item_headers.xml"), "MeetingRequest")).unwrap();
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].subject, "Отчёт за неделю");
        let m = parse::mail(&as_kind(include_bytes!("../tests/fixtures/get_item_message_mime.xml"), "MeetingCancellation")).unwrap();
        assert_eq!(m.len(), 1);
        assert!(parse::headers(&as_kind(include_bytes!("../tests/fixtures/get_item_headers.xml"), "Contact")).unwrap().is_empty(), "не письма — нет");
    }

    #[test]
    fn whole_message_mime() {
        let m = parse::mail(&resp(include_bytes!("../tests/fixtures/get_item_message_mime.xml"))).unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].id.id, "AAMkADmsg1");
        assert!(!m[0].is_read);
        let mime = String::from_utf8(m[0].mime.clone()).unwrap();
        assert!(mime.starts_with("From: Анна Петрова <anna@example.test>"), "{mime}");
        assert!(mime.contains("Subject: Отчёт за неделю"));
    }

    #[test]
    fn move_and_update_and_draft() {
        let moved = parse::moved(&resp(include_bytes!("../tests/fixtures/move_item.xml"))).unwrap();
        assert_eq!(moved.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["AAMkADmoved"]);
        let before = ItemId::new("AAMkADmsg1");
        let after = parse::updated(&resp(include_bytes!("../tests/fixtures/update_item_read.xml")), &before);
        assert_eq!((after.id.as_str(), after.change_key.as_deref()), ("AAMkADmsg1", Some("CQAAABYAAAB9")));
        assert_eq!(parse::saved(&resp(include_bytes!("../tests/fixtures/create_item_draft.xml"))).unwrap().id, "AAMkADdraft");
        parse::all_ok(&resp(include_bytes!("../tests/fixtures/create_item_send.xml"))).unwrap();
    }

    #[test]
    fn attachment_list_and_content() {
        let a = parse::attachments(&resp(include_bytes!("../tests/fixtures/get_item_attachments.xml"))).unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!((a[0].name.as_str(), a[0].content_type.as_str(), a[0].size, a[0].inline), ("отчёт.pdf", "application/pdf", 20480, false));
        let bytes = parse::attachment_content(&resp(include_bytes!("../tests/fixtures/get_attachment.xml"))).unwrap();
        assert_eq!(bytes, b"%PDF-1.4 fake");
    }

    #[test]
    fn requests_are_well_formed() {
        let send = create_item(b"From: a@example.test\r\n\r\nhi", Disposition::SendAndSaveCopy, None);
        let x = xml(soap::envelope(&send).as_bytes()).unwrap();
        let ci = x.at(&["Body", "CreateItem"]).unwrap();
        assert_eq!(ci.attr("MessageDisposition"), Some("SendAndSaveCopy"));
        let mime = ci.at(&["Items", "Message", "MimeContent"]).unwrap().text.clone();
        assert_eq!(b64(&mime), b"From: a@example.test\r\n\r\nhi");
        let draft = create_item(b"x", Disposition::SaveOnly, Some(WellKnown::Drafts));
        assert!(draft.contains(r#"<t:DistinguishedFolderId Id="drafts"/>"#));
        let upd = set_field("message:IsRead", "<t:IsRead>true</t:IsRead>".into());
        assert!(upd.contains(r#"<t:FieldURI FieldURI="message:IsRead"/>"#) && upd.contains("<t:IsRead>true</t:IsRead>"));
    }
}
