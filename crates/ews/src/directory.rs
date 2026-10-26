//! Справочник (GAL) по строке с данными человека — имя, адрес, должность, отдел, телефон: подсказки адресов и
//! «Контакты → Справочник» (S7-08).
//!
//! - **`FindPeople`** (Exchange 2013+) по папке `directory` — поиск по началу слов имени, фамилии, адреса, как в OWA;
//!   не больше `max` человек за запрос. Свойства — только те, что боевой Exchange 2013 CU23 принимает у GAL
//!   (`docs/EXCHANGE.md`): должность, отделы, компания, офис; телефонов (`BusinessPhoneNumbers`, `MobilePhones`,
//!   `PhoneNumber`) и `Department` он не отдаёт — `ErrorInvalidArgument` на весь запрос, телефон здесь пустой.
//! - **`ResolveNames`** — запасной путь (сервер не знает `FindPeople` или отказал): только Active Directory, с данными
//!   контакта, телефоном в том числе.

use crate::soap;
use crate::xml::{El, esc};
use crate::{Client, Result};

/// Человек или список рассылки из справочника.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Person {
    pub name: String,
    pub email: String,
    pub title: String,
    pub department: String,
    /// Рабочий, иначе мобильный, иначе первый из данных.
    pub phone: String,
    pub company: String,
    pub office: String,
    /// Список рассылки (`PublicDL`), не человек.
    pub group: bool,
}

pub(crate) fn request(query: &str) -> String {
    format!(
        r#"<m:ResolveNames ReturnFullContactData="true" SearchScope="ActiveDirectory"><m:UnresolvedEntry>{}</m:UnresolvedEntry></m:ResolveNames>"#,
        esc(query)
    )
}

fn smtp(addr: &str) -> String {
    addr.strip_prefix("SMTP:").or_else(|| addr.strip_prefix("smtp:")).unwrap_or(addr).to_owned()
}

fn person(r: &El) -> Option<Person> {
    let mailbox = r.child("Mailbox")?;
    let contact = r.child("Contact");
    let c = |path: &[&str]| contact.map(|c| c.str_at(path)).unwrap_or_default();
    // У ящика по SMTP адрес — `EmailAddress`; иначе (`EX`) — первый SMTP из данных контакта.
    let email = match mailbox.text_at(&["RoutingType"]) {
        Some("SMTP") | None => mailbox.str_at(&["EmailAddress"]),
        Some(_) => contact
            .and_then(|c| c.child("EmailAddresses"))
            .and_then(|e| e.all("Entry").find(|x| x.text.trim().to_ascii_lowercase().starts_with("smtp:")))
            .map(|e| smtp(e.text.trim()))
            .unwrap_or_default(),
    };
    let phones: Vec<(&str, &str)> = contact
        .and_then(|c| c.child("PhoneNumbers"))
        .into_iter()
        .flat_map(|p| p.all("Entry"))
        .filter(|e| !e.text.trim().is_empty())
        .map(|e| (e.attr("Key").unwrap_or(""), e.text.trim()))
        .collect();
    let phone = ["BusinessPhone", "MobilePhone"]
        .iter()
        .find_map(|k| phones.iter().find(|(key, _)| key == k))
        .or(phones.first())
        .map(|(_, v)| (*v).to_owned())
        .unwrap_or_default();
    let name = match c(&["DisplayName"]) {
        n if !n.is_empty() => n,
        _ => mailbox.str_at(&["Name"]),
    };
    Some(Person {
        name,
        email,
        title: c(&["JobTitle"]),
        department: c(&["Department"]),
        phone,
        company: c(&["CompanyName"]),
        office: c(&["OfficeLocation"]),
        group: mailbox.text_at(&["MailboxType"]) == Some("PublicDL"),
    })
}

/// Найденные: «не найдено» — пусто; несколько совпадений (`Warning`) — все.
pub(crate) fn parse(resp: &El, max: usize) -> Result<Vec<Person>> {
    let Some(msg) = soap::messages(resp).into_iter().next() else { return Ok(Vec::new()) };
    if soap::code(msg) == "ErrorNameResolutionNoResults" {
        return Ok(Vec::new());
    }
    soap::check(msg)?;
    Ok(msg.child("ResolutionSet").into_iter().flat_map(|s| s.all("Resolution")).filter_map(person).take(max).collect())
}

/// Свойства человека `FindPeople`: базовый вид `IdOnly` плюс нужное карточке справочника — только то, что Exchange 2013
/// принимает у GAL (лишнее свойство роняет весь запрос).
const PERSONA: &[&str] = &[
    "persona:PersonaType",
    "persona:DisplayName",
    "persona:EmailAddress",
    "persona:Title",
    "persona:Departments",
    "persona:CompanyName",
    "persona:OfficeLocations",
];

pub(crate) fn find_request(query: &str, max: usize) -> String {
    let props: String = PERSONA.iter().map(|p| format!(r#"<t:FieldURI FieldURI="{p}"/>"#)).collect();
    format!(
        r#"<m:FindPeople><m:PersonaShape><t:BaseShape>IdOnly</t:BaseShape><t:AdditionalProperties>{props}</t:AdditionalProperties></m:PersonaShape><m:IndexedPageItemView BasePoint="Beginning" MaxEntriesReturned="{}" Offset="0"/><m:ParentFolderId><t:DistinguishedFolderId Id="directory"/></m:ParentFolderId><m:QueryString>{}</m:QueryString></m:FindPeople>"#,
        max.clamp(1, 100),
        esc(query)
    )
}

/// Первое непустое значение `ArrayOfStringAttributedValues` (`Departments`, `OfficeLocations`).
fn first_value(list: Option<&El>) -> String {
    list.into_iter()
        .flat_map(|l| l.all("StringAttributedValue"))
        .filter_map(|v| v.text_at(&["Value"]))
        .map(str::trim)
        .find(|v| !v.is_empty())
        .unwrap_or("")
        .to_owned()
}

fn persona(p: &El) -> Option<Person> {
    let mail = p.child("EmailAddress")?;
    let email = match mail.text_at(&["RoutingType"]) {
        Some("SMTP") | None => mail.str_at(&["EmailAddress"]),
        Some(_) => return None, // `EX` без SMTP — писать некуда
    };
    if email.is_empty() {
        return None;
    }
    let name = match p.str_at(&["DisplayName"]) {
        n if !n.is_empty() => n,
        _ => mail.str_at(&["Name"]),
    };
    Some(Person {
        name,
        email,
        title: p.str_at(&["Title"]),
        department: first_value(p.child("Departments")),
        // Телефонов у GAL через `FindPeople` Exchange 2013 не отдаёт.
        phone: String::new(),
        company: p.str_at(&["CompanyName"]),
        office: first_value(p.child("OfficeLocations")),
        group: p.text_at(&["PersonaType"]) == Some("DistributionList") || mail.text_at(&["MailboxType"]) == Some("PublicDL"),
    })
}

/// Найденные `FindPeople`: ответ без `ResponseMessages` — сам ответ с `ResponseClass`. Без адреса — пропускаются.
pub(crate) fn find_parse(resp: &El, max: usize) -> Result<Vec<Person>> {
    soap::check(resp)?;
    Ok(resp.child("People").into_iter().flat_map(|p| p.all("Persona")).filter_map(persona).take(max).collect())
}

impl Client {
    /// Люди и списки справочника по строке (`FindPeople` по GAL), не больше `max`. Сервер без `FindPeople` — ошибка
    /// (`ErrorInvalidRequest`, `ErrorInvalidOperation`…): тогда — [`Client::resolve_names`].
    pub async fn find_people(&mut self, query: &str, max: usize) -> Result<Vec<Person>> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let resp = self.call("FindPeople", &find_request(query, max)).await?;
        let people = find_parse(&resp, max)?;
        tracing::debug!(found = people.len(), "ews find people");
        Ok(people)
    }

    /// Люди и списки справочника по строке (начало имени, фамилии, адреса), не больше `max`.
    pub async fn resolve_names(&mut self, query: &str, max: usize) -> Result<Vec<Person>> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let resp = self.call("ResolveNames", &request(query)).await?;
        let people = parse(&resp, max)?;
        tracing::debug!(found = people.len(), "ews resolve names");
        Ok(people)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml::parse as xml;

    fn resp(b: &[u8]) -> El {
        soap::response(xml(b).unwrap()).unwrap()
    }

    #[test]
    fn people_and_groups() {
        let p = parse(&resp(include_bytes!("../tests/fixtures/resolve_names.xml")), 10).unwrap();
        assert_eq!(p.len(), 3);
        assert_eq!(
            p[0],
            Person {
                name: "Петрова Анна Сергеевна".into(),
                email: "anna@example.test".into(),
                title: "Ведущий инженер".into(),
                department: "Отдел разработки".into(),
                phone: "+7 495 000-00-01".into(),
                company: "Пример".into(),
                office: "Москва, 5 этаж".into(),
                group: false,
            }
        );
        assert_eq!((p[1].email.as_str(), p[1].phone.as_str()), ("anton@example.test", "+7 900 000-00-02"));
        assert!(p[2].group && p[2].email == "dev-team@example.test");
        assert_eq!(parse(&resp(include_bytes!("../tests/fixtures/resolve_names.xml")), 1).unwrap().len(), 1);
    }

    #[test]
    fn nothing_found() {
        assert!(parse(&resp(include_bytes!("../tests/fixtures/resolve_names_none.xml")), 10).unwrap().is_empty());
    }

    #[test]
    fn query_is_escaped() {
        assert!(request("a<b").contains("<m:UnresolvedEntry>a&lt;b</m:UnresolvedEntry>"));
        assert!(find_request("a<b", 20).contains("<m:QueryString>a&lt;b</m:QueryString>"));
    }

    #[test]
    fn find_people_request_shape() {
        let r = find_request("Петр", 500);
        let x = xml(format!(r#"<x xmlns:m="m" xmlns:t="t">{r}</x>"#).as_bytes()).unwrap();
        let f = x.child("FindPeople").unwrap();
        let order: Vec<&str> = f.children.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(order, ["PersonaShape", "IndexedPageItemView", "ParentFolderId", "QueryString"], "порядок схемы FindPeopleType");
        assert_eq!(f.child("IndexedPageItemView").and_then(|v| v.attr("MaxEntriesReturned")), Some("100"), "предел сервера");
        assert_eq!(f.at(&["ParentFolderId", "DistinguishedFolderId"]).and_then(|d| d.attr("Id")), Some("directory"));
        let props: Vec<&str> = f.at(&["PersonaShape", "AdditionalProperties"]).unwrap().all("FieldURI").filter_map(|p| p.attr("FieldURI")).collect();
        assert!(props.contains(&"persona:Title") && props.contains(&"persona:Departments"));
        // Exchange 2013 CU23 на них отвечает `ErrorInvalidArgument` на весь запрос (боевой сервер).
        for bad in ["persona:Department", "persona:BusinessPhoneNumbers", "persona:MobilePhones", "persona:PhoneNumber", "persona:Titles"] {
            assert!(!props.contains(&bad), "{bad}");
        }
    }

    #[test]
    fn people_found() {
        let p = find_parse(&resp(include_bytes!("../tests/fixtures/find_people.xml")), 10).unwrap();
        assert_eq!(p.len(), 3, "без адреса — не в подсказки");
        assert_eq!(
            p[0],
            Person {
                name: "Петрова Анна Сергеевна".into(),
                email: "anna@example.test".into(),
                title: "Ведущий инженер".into(),
                department: "Отдел разработки".into(),
                phone: String::new(),
                company: "Пример".into(),
                office: "Москва, 5 этаж".into(),
                group: false,
            }
        );
        assert_eq!((p[1].title.as_str(), p[1].department.as_str()), ("", ""), "нет данных — пусто");
        assert!(p[2].group && p[2].email == "spb@example.test");
        assert_eq!(find_parse(&resp(include_bytes!("../tests/fixtures/find_people.xml")), 1).unwrap().len(), 1);
    }

    #[test]
    fn find_people_refused() {
        let doc = r#"<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><FindPeopleResponse ResponseClass="Error" xmlns="http://schemas.microsoft.com/exchange/services/2006/messages"><MessageText>x</MessageText><ResponseCode>ErrorInvalidRequest</ResponseCode></FindPeopleResponse></s:Body></s:Envelope>"#;
        let err = find_parse(&resp(doc.as_bytes()), 10).unwrap_err();
        assert_eq!(err.code(), Some("ErrorInvalidRequest"));
    }
}
