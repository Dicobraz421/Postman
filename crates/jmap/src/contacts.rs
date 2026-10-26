//! JMAP for Contacts (RFC 9610): карточки JSContact (RFC 9553) — имя, адреса, телефоны, организация, должность;
//! создание, правка (патч только изменённых свойств) и удаление (`ContactCard/set`).

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::mail::{GetResponse, inv, set_error};
use crate::{CONTACTS, CORE, Client, Error, Result, take};

const USING: &[&str] = &[CORE, CONTACTS];
/// Свойства карточки, которые показывает и правит клиент.
const PROPERTIES: &[&str] = &["id", "name", "emails", "phones", "organizations", "titles"];

/// Карточка контакта. У новой (ещё не на сервере) `id` пустой.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ContactCard {
    pub id: String,
    pub name: String,
    pub emails: Vec<String>,
    pub phones: Vec<String>,
    pub organization: String,
    /// Должность (`titles` с `kind: title`, иначе роль).
    pub title: String,
}

/// Адресная книга (`AddressBook/get`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddressBook {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub is_default: bool,
}

impl ContactCard {
    /// Карточка из объекта JSContact (RFC 9553): ответа JMAP или карточки vCard, разобранной воркером (CardDAV,
    /// S7-05б). `id` — из объекта (у CardDAV его ставит адаптер — путь ресурса).
    pub fn from_jscontact(v: &Value) -> ContactCard {
        card(v)
    }

    /// Карточка для `ContactCard/set create` в книге `book`; пустые свойства не передаются.
    pub fn to_jmap(&self, book: &str) -> Value {
        let mut card = Map::new();
        card.insert("@type".into(), json!("Card"));
        card.insert("version".into(), json!("1.0"));
        card.insert("addressBookIds".into(), json!({ book: true }));
        for (key, value) in self.properties() {
            if !value.is_null() {
                card.insert(key.into(), value);
            }
        }
        Value::Object(card)
    }

    /// Патч `ContactCard/set update`: только свойства, изменённые относительно `old`; убранное — `null`.
    /// Неизменённые адреса и телефоны не переписываются — их метки и контексты на сервере остаются.
    pub fn patch(&self, old: &ContactCard) -> Map<String, Value> {
        let old: Vec<(&str, Value)> = old.properties();
        self.properties().into_iter().zip(old).filter(|((_, new), (_, was))| new != was).map(|((k, v), _)| (k.into(), v)).collect()
    }

    /// Свойства JSContact в порядке `PROPERTIES` (без `id`); пустое — `null`.
    fn properties(&self) -> Vec<(&'static str, Value)> {
        let name = self.name.trim();
        let name = if name.is_empty() { Value::Null } else { json!({ "full": name }) };
        let map = |prefix: &str, items: &[String], entry: &dyn Fn(&str) -> Value| -> Value {
            let items: Map<String, Value> =
                items.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).enumerate().map(|(i, s)| (format!("{prefix}{}", i + 1), entry(s))).collect();
            if items.is_empty() { Value::Null } else { Value::Object(items) }
        };
        let emails = map("e", &self.emails, &|a| json!({ "address": a }));
        let phones = map("p", &self.phones, &|n| json!({ "number": n }));
        let organizations = map("o", std::slice::from_ref(&self.organization), &|n| json!({ "name": n }));
        let titles = map("t", std::slice::from_ref(&self.title), &|n| json!({ "name": n, "kind": "title" }));
        vec![("name", name), ("emails", emails), ("phones", phones), ("organizations", organizations), ("titles", titles)]
    }
}

impl Client {
    /// Все карточки личных адресных книг.
    pub async fn contact_cards(&self) -> Result<Vec<ContactCard>> {
        let acc = self.account_for(CONTACTS)?;
        let args = json!({ "accountId": acc, "ids": null, "properties": PROPERTIES });
        let mut r = self.call(USING, &[inv("ContactCard/get", args, "c")]).await?;
        let g: GetResponse<Value> = take(&mut r, "c")?;
        Ok(g.list.iter().map(card).collect())
    }

    pub async fn address_books(&self) -> Result<Vec<AddressBook>> {
        let acc = self.account_for(CONTACTS)?;
        let mut r = self.call(USING, &[inv("AddressBook/get", json!({ "accountId": acc, "ids": null }), "b")]).await?;
        let g: GetResponse<AddressBook> = take(&mut r, "b")?;
        Ok(g.list)
    }

    /// Новая карточка в книге по умолчанию (иначе в первой); id карточки на сервере.
    pub async fn create_contact(&self, card: &ContactCard) -> Result<String> {
        let books = self.address_books().await?;
        let book = books.iter().find(|b| b.is_default).or(books.first()).ok_or_else(|| Error::Protocol("на сервере нет адресной книги".into()))?;
        let acc = self.account_for(CONTACTS)?;
        let args = json!({ "accountId": acc, "create": { "new": card.to_jmap(&book.id) } });
        let mut r = self.call(USING, &[inv("ContactCard/set", args, "s")]).await?;
        let v: Value = take(&mut r, "s")?;
        set_error(&v, "notCreated", "new", "ContactCard/set")?;
        v["created"]["new"]["id"].as_str().map(str::to_string).ok_or_else(|| Error::Protocol("ContactCard/set не вернул id".into()))
    }

    /// Изменить карточку `old.id` на `card` (патч изменённых свойств; нечего менять — без запроса).
    pub async fn update_contact(&self, old: &ContactCard, card: &ContactCard) -> Result<()> {
        let patch = card.patch(old);
        if patch.is_empty() {
            return Ok(());
        }
        let acc = self.account_for(CONTACTS)?;
        let args = json!({ "accountId": acc, "update": { old.id.as_str(): patch } });
        let mut r = self.call(USING, &[inv("ContactCard/set", args, "s")]).await?;
        let v: Value = take(&mut r, "s")?;
        set_error(&v, "notUpdated", &old.id, "ContactCard/set")
    }

    pub async fn destroy_contact(&self, id: &str) -> Result<()> {
        let acc = self.account_for(CONTACTS)?;
        let args = json!({ "accountId": acc, "destroy": [id] });
        let mut r = self.call(USING, &[inv("ContactCard/set", args, "s")]).await?;
        let v: Value = take(&mut r, "s")?;
        set_error(&v, "notDestroyed", id, "ContactCard/set")
    }
}

/// Значения записей карточки (`emails`, `phones`…) по предпочтению (`pref`: 1 — первое), затем по ключу.
fn entries<'a>(v: &'a Value, field: &str) -> Vec<&'a Value> {
    let mut list: Vec<(&String, &Value)> = v[field].as_object().into_iter().flatten().collect();
    list.sort_by_key(|(k, e)| (e["pref"].as_u64().unwrap_or(u64::MAX), k.as_str()));
    list.into_iter().map(|(_, e)| e).collect()
}

fn card(v: &Value) -> ContactCard {
    let name = &v["name"];
    let full = name["full"].as_str().map(str::to_string).unwrap_or_else(|| {
        // Нет `full` — собираем из компонентов: фамилия, имя, отчество.
        let part = |kind: &str| -> Vec<&str> {
            name["components"].as_array().into_iter().flatten().filter(|c| c["kind"] == kind).filter_map(|c| c["value"].as_str()).collect()
        };
        [part("surname"), part("given"), part("given2")].concat().join(" ")
    });
    let strings =
        |field: &str, key: &str| -> Vec<String> { entries(v, field).into_iter().filter_map(|e| e[key].as_str()).map(str::to_string).collect() };
    let organization = entries(v, "organizations")
        .into_iter()
        .find_map(|o| {
            let units = o["units"].as_array().into_iter().flatten().filter_map(|u| u["name"].as_str());
            let parts: Vec<&str> = o["name"].as_str().into_iter().chain(units).filter(|s| !s.is_empty()).collect();
            (!parts.is_empty()).then(|| parts.join(", "))
        })
        .unwrap_or_default();
    let titles = entries(v, "titles");
    let title = titles
        .iter()
        .find(|t| t["kind"].as_str().unwrap_or("title") == "title")
        .or(titles.first())
        .and_then(|t| t["name"].as_str())
        .unwrap_or_default()
        .to_string();
    ContactCard {
        id: v["id"].as_str().unwrap_or_default().to_string(),
        name: full,
        emails: strings("emails", "address"),
        phones: strings("phones", "number"),
        organization,
        title,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jscontact_card() {
        let c = card(&json!({
            "id": "c1",
            "name": { "components": [{ "kind": "given", "value": "Мария" }, { "kind": "surname", "value": "Иванова" }] },
            "emails": { "e1": { "address": "m@x.ru" }, "e2": { "address": "maria@home.ru" } }
        }));
        assert_eq!(c.name, "Иванова Мария");
        assert_eq!(c.emails.len(), 2);
        assert!(c.phones.is_empty() && c.organization.is_empty() && c.title.is_empty());
        let c = card(&json!({ "id": "c2", "name": { "full": "ООО Ромашка" }, "emails": {} }));
        assert_eq!(c.name, "ООО Ромашка");
    }

    #[test]
    fn phones_organization_and_title() {
        let c = card(&json!({
            "id": "c3",
            "name": { "full": "Петров Алексей" },
            "emails": { "b": { "address": "home@x.ru", "pref": 2 }, "a": { "address": "work@x.ru", "pref": 1 } },
            "phones": {
                "p2": { "number": "+7 900 000-00-02", "features": { "mobile": true } },
                "p1": { "@type": "Phone", "number": "+7 495 000-00-01", "contexts": { "work": true }, "pref": 1 }
            },
            "organizations": { "o1": { "name": "ООО Ромашка", "units": [{ "name": "Отдел продаж" }] } },
            "titles": { "r": { "name": "Ответственный за договоры", "kind": "role" }, "t": { "name": "Менеджер", "kind": "title" } }
        }));
        assert_eq!(c.emails, ["work@x.ru", "home@x.ru"], "по pref");
        assert_eq!(c.phones, ["+7 495 000-00-01", "+7 900 000-00-02"]);
        assert_eq!(c.organization, "ООО Ромашка, Отдел продаж");
        assert_eq!(c.title, "Менеджер", "должность важнее роли");
        // Только роль и организация без имени, но с отделом.
        let c = card(
            &json!({ "titles": { "r": { "name": "Секретарь", "kind": "role" } }, "organizations": { "o": { "units": [{ "name": "Канцелярия" }] } } }),
        );
        assert_eq!((c.title.as_str(), c.organization.as_str()), ("Секретарь", "Канцелярия"));
    }

    fn sample() -> ContactCard {
        ContactCard {
            id: "c1".into(),
            name: "Петров Алексей".into(),
            emails: vec!["a.petrov@x.ru".into()],
            phones: vec!["+7 495 000-00-01".into()],
            organization: "ООО Ромашка".into(),
            title: String::new(),
        }
    }

    #[test]
    fn create_skips_empty_properties() {
        let v = sample().to_jmap("b1");
        assert_eq!(v["addressBookIds"], json!({ "b1": true }));
        assert_eq!(v["name"], json!({ "full": "Петров Алексей" }));
        assert_eq!(v["emails"], json!({ "e1": { "address": "a.petrov@x.ru" } }));
        assert_eq!(v["phones"], json!({ "p1": { "number": "+7 495 000-00-01" } }));
        assert_eq!(v["organizations"], json!({ "o1": { "name": "ООО Ромашка" } }));
        assert!(v.get("titles").is_none() && v.get("id").is_none());
    }

    #[test]
    fn patch_has_only_changes() {
        let old = sample();
        assert!(old.patch(&old).is_empty(), "без изменений — пустой патч");
        let mut new = old.clone();
        new.title = " Директор ".into();
        new.phones.clear();
        new.emails.push(" a@home.ru ".into());
        let p = new.patch(&old);
        let keys: Vec<&str> = p.keys().map(String::as_str).collect();
        assert_eq!(keys.len(), 3, "{keys:?}");
        assert_eq!(p["titles"], json!({ "t1": { "name": "Директор", "kind": "title" } }));
        assert_eq!(p["phones"], Value::Null, "убранное — null");
        assert_eq!(p["emails"], json!({ "e1": { "address": "a.petrov@x.ru" }, "e2": { "address": "a@home.ru" } }));
        // Пробелы вокруг — не изменение.
        let mut same = old.clone();
        same.name = "  Петров Алексей ".into();
        assert!(same.patch(&old).is_empty());
    }
}
