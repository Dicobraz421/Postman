//! Личные контакты Exchange (T1-25а): папка «Контакты» (`contacts`) и её подпапки класса `IPF.Contact`.
//!
//! - **Папки** — `GetFolder` `contacts` и `FindFolder` (Deep) под ней.
//! - **Первое чтение** — `FindItem` по страницам (id), затем карточки `GetItem`; **изменения** — `SyncFolderItems`.
//! - **Карточка** — имя, фамилия, отображаемое имя, адреса (до трёх), телефоны, компания, должность, отдел,
//!   заметки (тело текстом). Списки рассылки (`DistributionList`) — только чтение: имя и участники.
//! - **Запись** — `CreateItem`, `UpdateItem` (все поля карточки: есть — поставить, пусто — снять), `DeleteItem`
//!   в «Удалённые».

use crate::calendar::SyncChanges;
use crate::soap::{self, text_el};
use crate::xml::{El, esc};
use crate::{BATCH, Client, Error, FolderId, FolderRef, ItemId, Result, SyncState, WellKnown};

/// Папка контактов.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactFolder {
    pub id: FolderId,
    /// Родитель — его `Id`; `None` — сама папка «Контакты».
    pub parent: Option<String>,
    pub name: String,
    pub total: u32,
}

/// Вид телефона карточки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhoneKind {
    Business,
    Mobile,
    Home,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Phone {
    pub kind: PhoneKind,
    pub number: String,
}

/// Участник списка рассылки.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Member {
    pub name: String,
    pub email: String,
}

/// Поля карточки, которые Postman показывает и правит.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContactData {
    pub display_name: String,
    pub given_name: String,
    pub surname: String,
    /// Адреса по порядку (`EmailAddress1`…`3`), не больше трёх.
    pub emails: Vec<String>,
    pub phones: Vec<Phone>,
    pub company: String,
    pub job_title: String,
    pub department: String,
    /// Заметки — тело карточки текстом.
    pub notes: String,
}

/// Контакт или список рассылки папки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contact {
    pub id: ItemId,
    pub folder_id: Option<String>,
    pub data: ContactData,
    /// Список рассылки (`DistributionList`) — участники; только чтение. У контакта — `None`.
    pub members: Option<Vec<Member>>,
}

/// Адресов в карточке Exchange.
pub const MAX_EMAILS: usize = 3;
const EMAIL_KEYS: [&str; MAX_EMAILS] = ["EmailAddress1", "EmailAddress2", "EmailAddress3"];
/// Ключи телефонов при записи: по два рабочих и домашних, мобильный, другой.
const PHONE_KEYS: [(&str, PhoneKind); 6] = [
    ("BusinessPhone", PhoneKind::Business),
    ("BusinessPhone2", PhoneKind::Business),
    ("MobilePhone", PhoneKind::Mobile),
    ("HomePhone", PhoneKind::Home),
    ("HomePhone2", PhoneKind::Home),
    ("OtherTelephone", PhoneKind::Other),
];

fn phone_kind(key: &str) -> PhoneKind {
    match key {
        "BusinessPhone" | "BusinessPhone2" | "CompanyMainPhone" | "AssistantPhone" | "Callback" => PhoneKind::Business,
        "MobilePhone" | "CarPhone" | "Pager" | "RadioPhone" => PhoneKind::Mobile,
        "HomePhone" | "HomePhone2" => PhoneKind::Home,
        _ => PhoneKind::Other,
    }
}

impl ContactData {
    /// Отображаемое имя: своё, иначе «имя фамилия», иначе первый адрес.
    pub fn shown_name(&self) -> String {
        if !self.display_name.trim().is_empty() {
            return self.display_name.trim().to_owned();
        }
        let full = format!("{} {}", self.given_name.trim(), self.surname.trim()).trim().to_owned();
        if !full.is_empty() {
            return full;
        }
        self.emails.first().cloned().unwrap_or_default()
    }

    /// (ключ, номер) для записи: телефоны раскладываются по ключам своего вида по порядку, лишние — не пишутся.
    fn phone_slots(&self) -> Vec<(&'static str, &str)> {
        let mut used = [false; PHONE_KEYS.len()];
        let mut out = Vec::new();
        for p in self.phones.iter().filter(|p| !p.number.trim().is_empty()) {
            if let Some(i) = (0..PHONE_KEYS.len()).find(|&i| !used[i] && PHONE_KEYS[i].1 == p.kind) {
                used[i] = true;
                out.push((PHONE_KEYS[i].0, p.number.trim()));
            }
        }
        out
    }

    fn emails(&self) -> impl Iterator<Item = (&'static str, &str)> {
        self.emails.iter().map(|e| e.trim()).filter(|e| !e.is_empty()).take(MAX_EMAILS).zip(EMAIL_KEYS).map(|(e, k)| (k, e))
    }
}

/// Адрес из записи `EmailAddresses`: без `SMTP:`; не SMTP (X500 и т. п.) — `None`.
fn smtp(text: &str) -> Option<String> {
    let t = text.trim();
    let t = t.strip_prefix("SMTP:").or_else(|| t.strip_prefix("smtp:")).unwrap_or(t);
    t.contains('@').then(|| t.to_owned())
}

fn entries<'a>(el: Option<&'a El>) -> impl Iterator<Item = (&'a str, &'a str)> + 'a {
    el.into_iter().flat_map(|e| e.all("Entry")).map(|e| (e.attr("Key").unwrap_or(""), e.text.trim())).filter(|(_, v)| !v.is_empty())
}

fn contact(el: &El) -> Option<Contact> {
    let id = ItemId::from_el(el.child("ItemId")?)?;
    let folder_id = el.child("ParentFolderId").and_then(|p| p.attr("Id")).map(str::to_owned);
    let notes = el.str_at(&["Body"]);
    match el.name.as_str() {
        "Contact" => {
            let mut emails: Vec<(&str, String)> = entries(el.child("EmailAddresses")).filter_map(|(k, v)| Some((k, smtp(v)?))).collect();
            emails.sort_by_key(|(k, _)| EMAIL_KEYS.iter().position(|x| x == k).unwrap_or(usize::MAX));
            let phones = entries(el.child("PhoneNumbers")).map(|(k, v)| Phone { kind: phone_kind(k), number: v.to_owned() }).collect();
            Some(Contact {
                id,
                folder_id,
                data: ContactData {
                    display_name: el.str_at(&["DisplayName"]),
                    given_name: el.str_at(&["GivenName"]),
                    surname: el.str_at(&["Surname"]),
                    emails: emails.into_iter().map(|(_, e)| e).take(MAX_EMAILS).collect(),
                    phones,
                    company: el.str_at(&["CompanyName"]),
                    job_title: el.str_at(&["JobTitle"]),
                    department: el.str_at(&["Department"]),
                    notes,
                },
                members: None,
            })
        }
        "DistributionList" => {
            let members = el
                .child("Members")
                .into_iter()
                .flat_map(|m| m.all("Member"))
                .filter_map(|m| m.child("Mailbox"))
                .map(|m| Member { name: m.str_at(&["Name"]), email: m.str_at(&["EmailAddress"]) })
                .collect();
            Some(Contact {
                id,
                folder_id,
                data: ContactData { display_name: el.str_at(&["DisplayName"]), notes, ..ContactData::default() },
                members: Some(members),
            })
        }
        _ => None,
    }
}

fn folder(el: &El) -> Option<ContactFolder> {
    let id = el.child("FolderId")?;
    Some(ContactFolder {
        id: FolderId { id: id.attr("Id")?.to_owned(), change_key: id.attr("ChangeKey").map(str::to_owned) },
        parent: el.child("ParentFolderId").and_then(|p| p.attr("Id")).map(str::to_owned),
        name: el.str_at(&["DisplayName"]),
        total: el.text_at(&["TotalCount"]).and_then(|t| t.parse().ok()).unwrap_or(0),
    })
}

pub(crate) mod parse {
    //! Разбор ответов контактов — для тестов на корпусе.
    use super::*;

    /// Карточки `GetItem`; удалённые за это время (`ErrorItemNotFound`) — пропускаются.
    pub(crate) fn contacts(resp: &El) -> Result<Vec<Contact>> {
        let mut out = Vec::new();
        for msg in soap::messages(resp) {
            if soap::code(msg) == "ErrorItemNotFound" {
                continue;
            }
            soap::check(msg)?;
            out.extend(msg.child("Items").into_iter().flat_map(|i| i.children.iter()).filter_map(contact));
        }
        Ok(out)
    }

    /// Папка «Контакты» (`GetFolder`) и подпапки контактов (`FindFolder`): сама папка — без родителя.
    pub(crate) fn folders(root: &El, sub: &El) -> Result<Vec<ContactFolder>> {
        let msg = soap::single(root)?;
        let mut top = msg.child("Folders").and_then(|f| f.children.first()).and_then(folder).ok_or_else(|| Error::Xml("GetFolder: no folder".into()))?;
        top.parent = None;
        let msg = soap::single(sub)?;
        let mut out = vec![top];
        out.extend(
            msg.at(&["RootFolder", "Folders"])
                .into_iter()
                .flat_map(|f| f.children.iter())
                // Ровно `IPF.Contact`: служебные подпапки (`IPF.Contact.MOC.QuickContacts`, `…RecipientCache`,
                // `…GalContacts`) скрыты и в Outlook.
                .filter(|f| match f.text_at(&["FolderClass"]) {
                    Some(c) => c == "IPF.Contact",
                    None => f.name == "ContactsFolder",
                })
                .filter_map(folder),
        );
        Ok(out)
    }

    pub(crate) fn created(resp: &El) -> Result<ItemId> {
        let msg = soap::single(resp)?;
        msg.child("Items")
            .and_then(|i| i.children.first())
            .and_then(|c| c.child("ItemId"))
            .and_then(ItemId::from_el)
            .ok_or_else(|| Error::Xml("CreateItem: no ItemId".into()))
    }
}

pub(crate) mod req {
    //! Запросы контактов — для тестов сборки.
    use super::*;

    const SHAPE: &str = "<m:ItemShape><t:BaseShape>AllProperties</t:BaseShape><t:BodyType>Text</t:BodyType></m:ItemShape>";

    pub(crate) fn get(ids: &[ItemId]) -> String {
        let ids: String = ids.iter().map(|i| i.xml("t:ItemId")).collect();
        format!("<m:GetItem>{SHAPE}<m:ItemIds>{ids}</m:ItemIds></m:GetItem>")
    }

    pub(crate) fn page(folder: &FolderRef, offset: u32, max: u32) -> String {
        format!(
            r#"<m:FindItem Traversal="Shallow"><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape></m:ItemShape><m:IndexedPageItemView MaxEntriesReturned="{}" Offset="{offset}" BasePoint="Beginning"/><m:ParentFolderIds>{}</m:ParentFolderIds></m:FindItem>"#,
            max.clamp(1, 1000),
            folder.xml()
        )
    }

    pub(crate) fn sync(folder: &FolderRef, state: Option<&SyncState>, max: u32) -> String {
        let state = state.filter(|s| !s.0.is_empty()).map(|s| text_el("m:SyncState", &s.0)).unwrap_or_default();
        format!(
            r#"<m:SyncFolderItems><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape></m:ItemShape><m:SyncFolderId>{}</m:SyncFolderId>{state}<m:MaxChangesReturned>{}</m:MaxChangesReturned><m:SyncScope>NormalItems</m:SyncScope></m:SyncFolderItems>"#,
            folder.xml(),
            max.clamp(1, 512)
        )
    }

    fn body(notes: &str) -> String {
        format!(r#"<t:Body BodyType="Text">{}</t:Body>"#, esc(notes))
    }

    /// Карточка `<t:Contact>` — поля в порядке схемы `ContactItemType` (Body — из `ItemType`, раньше всех).
    pub(crate) fn create(folder: &FolderRef, d: &ContactData) -> String {
        let opt = |name: &str, v: &str| if v.trim().is_empty() { String::new() } else { text_el(name, v.trim()) };
        let shown = d.shown_name();
        let emails: String = d.emails().map(|(k, e)| format!(r#"<t:Entry Key="{k}">{}</t:Entry>"#, esc(e))).collect();
        let phones: String = d.phone_slots().into_iter().map(|(k, n)| format!(r#"<t:Entry Key="{k}">{}</t:Entry>"#, esc(n))).collect();
        let mut c = String::new();
        if !d.notes.is_empty() {
            c.push_str(&body(&d.notes));
        }
        c.push_str(&opt("t:FileAs", &shown));
        c.push_str(&opt("t:DisplayName", &shown));
        c.push_str(&opt("t:GivenName", &d.given_name));
        c.push_str(&opt("t:CompanyName", &d.company));
        if !emails.is_empty() {
            c.push_str(&format!("<t:EmailAddresses>{emails}</t:EmailAddresses>"));
        }
        if !phones.is_empty() {
            c.push_str(&format!("<t:PhoneNumbers>{phones}</t:PhoneNumbers>"));
        }
        c.push_str(&opt("t:Department", &d.department));
        c.push_str(&opt("t:JobTitle", &d.job_title));
        c.push_str(&opt("t:Surname", &d.surname));
        format!("<m:CreateItem><m:SavedItemFolderId>{}</m:SavedItemFolderId><m:Items><t:Contact>{c}</t:Contact></m:Items></m:CreateItem>", folder.xml())
    }

    fn set(uri: &str, inner: String) -> String {
        format!(r#"<t:SetItemField><t:FieldURI FieldURI="{uri}"/><t:Contact>{inner}</t:Contact></t:SetItemField>"#)
    }

    fn delete(uri: &str) -> String {
        format!(r#"<t:DeleteItemField><t:FieldURI FieldURI="{uri}"/></t:DeleteItemField>"#)
    }

    fn set_indexed(uri: &str, key: &str, list: &str, value: &str) -> String {
        format!(
            r#"<t:SetItemField><t:IndexedFieldURI FieldURI="{uri}" FieldIndex="{key}"/><t:Contact><t:{list}><t:Entry Key="{key}">{}</t:Entry></t:{list}></t:Contact></t:SetItemField>"#,
            esc(value)
        )
    }

    fn delete_indexed(uri: &str, key: &str) -> String {
        format!(r#"<t:DeleteItemField><t:IndexedFieldURI FieldURI="{uri}" FieldIndex="{key}"/></t:DeleteItemField>"#)
    }

    /// Вся карточка: непустое поле — поставить, пустое — снять (заметки — пустым телом).
    pub(crate) fn update(id: &ItemId, d: &ContactData) -> String {
        let mut u = String::new();
        u.push_str(&set("item:Body", body(&d.notes)));
        let shown = d.shown_name();
        for (uri, el, v) in [
            ("contacts:FileAs", "t:FileAs", shown.as_str()),
            ("contacts:DisplayName", "t:DisplayName", shown.as_str()),
            ("contacts:GivenName", "t:GivenName", d.given_name.as_str()),
            ("contacts:CompanyName", "t:CompanyName", d.company.as_str()),
            ("contacts:Department", "t:Department", d.department.as_str()),
            ("contacts:JobTitle", "t:JobTitle", d.job_title.as_str()),
            ("contacts:Surname", "t:Surname", d.surname.as_str()),
        ] {
            u.push_str(&if v.trim().is_empty() { delete(uri) } else { set(uri, text_el(el, v.trim())) });
        }
        let emails: Vec<(&str, &str)> = d.emails().collect();
        for key in EMAIL_KEYS {
            u.push_str(&match emails.iter().find(|(k, _)| *k == key) {
                Some((_, e)) => set_indexed("contacts:EmailAddress", key, "EmailAddresses", e),
                None => delete_indexed("contacts:EmailAddress", key),
            });
        }
        let phones = d.phone_slots();
        for (key, _) in PHONE_KEYS {
            u.push_str(&match phones.iter().find(|(k, _)| *k == key) {
                Some((_, n)) => set_indexed("contacts:PhoneNumber", key, "PhoneNumbers", n),
                None => delete_indexed("contacts:PhoneNumber", key),
            });
        }
        format!(
            r#"<m:UpdateItem ConflictResolution="AlwaysOverwrite"><m:ItemChanges><t:ItemChange>{}<t:Updates>{u}</t:Updates></t:ItemChange></m:ItemChanges></m:UpdateItem>"#,
            id.xml("t:ItemId")
        )
    }

    pub(crate) fn delete_items(ids: &[ItemId]) -> String {
        let ids: String = ids.iter().map(|i| i.xml("t:ItemId")).collect();
        format!(r#"<m:DeleteItem DeleteType="MoveToDeletedItems"><m:ItemIds>{ids}</m:ItemIds></m:DeleteItem>"#)
    }
}

impl Client {
    /// Папка «Контакты» и её подпапки контактов.
    pub async fn contact_folders(&mut self) -> Result<Vec<ContactFolder>> {
        let root = format!(
            r#"<m:GetFolder><m:FolderShape><t:BaseShape>Default</t:BaseShape></m:FolderShape><m:FolderIds>{}</m:FolderIds></m:GetFolder>"#,
            FolderRef::WellKnown(WellKnown::Contacts).xml()
        );
        let root = self.call("GetFolder", &root).await?;
        let sub = format!(
            r#"<m:FindFolder Traversal="Deep"><m:FolderShape><t:BaseShape>Default</t:BaseShape><t:AdditionalProperties><t:FieldURI FieldURI="folder:ParentFolderId"/><t:FieldURI FieldURI="folder:FolderClass"/></t:AdditionalProperties></m:FolderShape><m:IndexedPageFolderView MaxEntriesReturned="200" Offset="0" BasePoint="Beginning"/><m:ParentFolderIds>{}</m:ParentFolderIds></m:FindFolder>"#,
            FolderRef::WellKnown(WellKnown::Contacts).xml()
        );
        let sub = self.call("FindFolder", &sub).await?;
        parse::folders(&root, &sub)
    }

    /// Контакты папки страницей с `offset` (первое чтение): id и есть ли ещё.
    pub async fn contacts_page(&mut self, folder: &FolderRef, offset: u32, max: u32) -> Result<(Vec<ItemId>, bool)> {
        crate::mail::parse::page(&self.call("FindItem", &req::page(folder, offset, max)).await?)
    }

    /// Изменения контактов папки с прошлого состояния (`SyncFolderItems`).
    pub async fn sync_contacts(&mut self, folder: &FolderRef, state: Option<&SyncState>, max: u32) -> Result<SyncChanges> {
        crate::calendar::parse::sync(&self.call("SyncFolderItems", &req::sync(folder, state, max)).await?)
    }

    /// Карточки (пачками по [`BATCH`]): контакты и списки рассылки. Удалённые за это время — пропускаются.
    pub async fn get_contacts(&mut self, ids: &[ItemId]) -> Result<Vec<Contact>> {
        let mut out = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(BATCH) {
            out.extend(parse::contacts(&self.call("GetItem", &req::get(chunk)).await?)?);
        }
        Ok(out)
    }

    /// Новый контакт в папке → его id.
    pub async fn create_contact(&mut self, folder: &FolderRef, data: &ContactData) -> Result<ItemId> {
        parse::created(&self.call("CreateItem", &req::create(folder, data)).await?)
    }

    /// Записать карточку целиком → новая версия.
    pub async fn update_contact(&mut self, id: &ItemId, data: &ContactData) -> Result<ItemId> {
        let resp = self.call("UpdateItem", &req::update(id, data)).await?;
        soap::single(&resp)?;
        Ok(crate::mail::parse::updated(&resp, id))
    }

    /// Контакты папки, в «Хранить как» (`FileAs`; Postman пишет туда отображаемое имя) которых есть `text`, без учёта
    /// регистра, не больше `max`: поиск в своих контактах и уборка тестов. Тема у контакта EWS пустая, а условие по
    /// `contacts:DisplayName` боевой Exchange 2013 не выполняет — ничего не находит.
    pub async fn find_contacts(&mut self, folder: &FolderRef, text: &str, max: u32) -> Result<Vec<ItemId>> {
        let body = format!(
            r#"<m:FindItem Traversal="Shallow"><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape></m:ItemShape><m:IndexedPageItemView MaxEntriesReturned="{}" Offset="0" BasePoint="Beginning"/><m:Restriction><t:Contains ContainmentMode="Substring" ContainmentComparison="IgnoreCase"><t:FieldURI FieldURI="contacts:FileAs"/><t:Constant Value="{}"/></t:Contains></m:Restriction><m:ParentFolderIds>{}</m:ParentFolderIds></m:FindItem>"#,
            max.clamp(1, 1000),
            esc(text),
            folder.xml()
        );
        Ok(crate::mail::parse::page(&self.call("FindItem", &body).await?)?.0)
    }

    /// Удалить контакты в «Удалённые». Уже удалённое — не ошибка.
    pub async fn delete_contacts(&mut self, ids: &[ItemId]) -> Result<()> {
        for chunk in ids.chunks(BATCH) {
            crate::mail::parse::all_ok(&self.call("DeleteItem", &req::delete_items(chunk)).await?)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml::parse as xml;

    fn resp(b: &[u8]) -> El {
        soap::response(xml(b).unwrap()).unwrap()
    }

    fn body(x: String) -> El {
        xml(soap::envelope(&x).as_bytes()).unwrap()
    }

    fn anna() -> ContactData {
        ContactData {
            display_name: String::new(),
            given_name: "Анна".into(),
            surname: "Петрова".into(),
            emails: vec!["anna@example.test".into(), " ".into(), "a.petrova@example.test".into()],
            phones: vec![
                Phone { kind: PhoneKind::Mobile, number: "+7 900 000-00-01".into() },
                Phone { kind: PhoneKind::Business, number: "+7 495 000-00-01".into() },
                Phone { kind: PhoneKind::Mobile, number: "+7 900 000-00-09".into() },
            ],
            company: "Пример".into(),
            job_title: "Инженер".into(),
            department: String::new(),
            notes: "Звонить после <10>".into(),
        }
    }

    #[test]
    fn contacts_and_lists() {
        let c = parse::contacts(&resp(include_bytes!("../tests/fixtures/get_item_contacts.xml"))).unwrap();
        assert_eq!(c.len(), 2, "ErrorItemNotFound пропущен");
        let a = &c[0];
        assert_eq!(a.id.id, "AAMkADcontact1");
        assert_eq!(a.folder_id.as_deref(), Some("AAMkADcontacts"));
        assert_eq!((a.data.display_name.as_str(), a.data.given_name.as_str(), a.data.surname.as_str()), ("Петрова Анна", "Анна", "Петрова"));
        assert_eq!(a.data.emails, ["anna@example.test", "a.petrova@example.test"], "по ключам, без X500");
        assert_eq!(
            a.data.phones,
            [Phone { kind: PhoneKind::Business, number: "+7 495 000-00-01".into() }, Phone { kind: PhoneKind::Mobile, number: "+7 900 000-00-01".into() }]
        );
        assert_eq!((a.data.company.as_str(), a.data.job_title.as_str(), a.data.department.as_str()), ("Пример", "Ведущий инженер", "Отдел разработки"));
        assert_eq!(a.data.notes, "Коллега по проекту.");
        assert!(a.members.is_none());
        let dl = &c[1];
        assert_eq!(dl.data.display_name, "Команда проекта");
        let m = dl.members.as_ref().expect("участники");
        assert_eq!(m.iter().map(|m| m.email.as_str()).collect::<Vec<_>>(), ["anna@example.test", "anton@example.test"]);
    }

    #[test]
    fn contact_folders() {
        let f = parse::folders(
            &resp(include_bytes!("../tests/fixtures/get_folder_contacts.xml")),
            &resp(include_bytes!("../tests/fixtures/find_folder_contacts.xml")),
        )
        .unwrap();
        assert_eq!(f.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["Contacts", "Коллеги"], "служебные папки — не контакты");
        assert_eq!((f[0].parent.as_deref(), f[0].total), (None, 2));
        assert_eq!(f[1].parent.as_deref(), Some("AAMkADcontacts"));
    }

    #[test]
    fn create_request() {
        let x = body(req::create(&FolderRef::WellKnown(WellKnown::Contacts), &anna()));
        let ci = x.at(&["Body", "CreateItem"]).unwrap();
        assert_eq!(ci.at(&["SavedItemFolderId", "DistinguishedFolderId"]).and_then(|d| d.attr("Id")), Some("contacts"));
        let c = ci.at(&["Items", "Contact"]).unwrap();
        let order: Vec<&str> = c.children.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            order,
            ["Body", "FileAs", "DisplayName", "GivenName", "CompanyName", "EmailAddresses", "PhoneNumbers", "JobTitle", "Surname"],
            "порядок схемы ContactItemType; пустой отдел не пишется"
        );
        assert_eq!(c.text_at(&["DisplayName"]), Some("Анна Петрова"), "имя по умолчанию");
        assert_eq!(c.child("Body").and_then(|b| b.attr("BodyType")), Some("Text"));
        assert_eq!(c.text_at(&["Body"]), Some("Звонить после <10>"));
        let emails: Vec<(Option<&str>, &str)> = c.child("EmailAddresses").unwrap().all("Entry").map(|e| (e.attr("Key"), e.text.as_str())).collect();
        assert_eq!(emails, [(Some("EmailAddress1"), "anna@example.test"), (Some("EmailAddress2"), "a.petrova@example.test")]);
        let phones: Vec<Option<&str>> = c.child("PhoneNumbers").unwrap().all("Entry").map(|e| e.attr("Key")).collect();
        assert_eq!(phones, [Some("MobilePhone"), Some("BusinessPhone")], "второй мобильный — некуда, не пишется");
        assert_eq!(parse::created(&resp(include_bytes!("../tests/fixtures/create_item.xml"))).map(|i| !i.id.is_empty()).ok(), Some(true));
    }

    #[test]
    fn update_and_delete_requests() {
        let id = ItemId { id: "AAMkADcontact1".into(), change_key: Some("EQAAAA==".into()) };
        let x = body(req::update(&id, &anna()));
        let change = x.at(&["Body", "UpdateItem", "ItemChanges", "ItemChange"]).unwrap();
        assert_eq!(change.child("ItemId").and_then(|i| i.attr("Id")), Some("AAMkADcontact1"));
        let updates = change.child("Updates").unwrap();
        let field = |e: &El| {
            e.child("FieldURI")
                .or_else(|| e.child("IndexedFieldURI"))
                .map(|f| format!("{}{}", f.attr("FieldURI").unwrap_or(""), f.attr("FieldIndex").map(|i| format!("/{i}")).unwrap_or_default()))
                .unwrap_or_default()
        };
        let set: Vec<String> = updates.all("SetItemField").map(field).collect();
        let del: Vec<String> = updates.all("DeleteItemField").map(field).collect();
        assert!(set.contains(&"contacts:GivenName".into()) && set.contains(&"contacts:EmailAddress/EmailAddress2".into()));
        assert!(set.contains(&"contacts:PhoneNumber/MobilePhone".into()) && set.contains(&"contacts:PhoneNumber/BusinessPhone".into()));
        assert!(del.contains(&"contacts:Department".into()), "пустое — снять");
        assert!(del.contains(&"contacts:EmailAddress/EmailAddress3".into()) && del.contains(&"contacts:PhoneNumber/HomePhone".into()));
        let email2 = updates
            .all("SetItemField")
            .find(|e| field(e) == "contacts:EmailAddress/EmailAddress2")
            .and_then(|e| e.at(&["Contact", "EmailAddresses", "Entry"]))
            .map(|e| (e.attr("Key"), e.text.as_str()));
        assert_eq!(email2, Some((Some("EmailAddress2"), "a.petrova@example.test")));
        let d = body(req::delete_items(std::slice::from_ref(&id)));
        assert_eq!(d.at(&["Body", "DeleteItem"]).and_then(|e| e.attr("DeleteType")), Some("MoveToDeletedItems"), "в «Удалённые»");
        let p = body(req::page(&FolderRef::WellKnown(WellKnown::Contacts), 100, 5000));
        assert_eq!(p.at(&["Body", "FindItem", "IndexedPageItemView"]).and_then(|v| v.attr("Offset")), Some("100"));
        assert!(req::get(std::slice::from_ref(&id)).contains("<t:BodyType>Text</t:BodyType>"));
    }
}
