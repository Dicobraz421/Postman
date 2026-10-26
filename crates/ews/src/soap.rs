//! Конверт SOAP и разбор ответа EWS: сбой (`Fault`), сообщения ответа (`ResponseClass`, `ResponseCode`),
//! `ErrorServerBusy` с `BackOffMilliseconds`, версия сервера.

use crate::xml::{El, esc};
use crate::{Error, Result, ServerVersion};

/// Версия схемы запросов: её понимают все Exchange 2016+ (у 2016 своей `Exchange2016` в схеме on-prem нет).
pub(crate) const REQUEST_VERSION: &str = "Exchange2013_SP1";
/// Пауза после `ErrorServerBusy` — не дольше (сервер просит до нескольких минут; дольше — пусть решает вызывающий).
pub(crate) const MAX_BACK_OFF_MS: u64 = 5 * 60 * 1000;

const NS: &str = concat!(
    r#"xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/" "#,
    r#"xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types" "#,
    r#"xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages""#
);

/// Конверт с телом операции (префиксы `t:` — типы, `m:` — сообщения).
pub(crate) fn envelope(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><soap:Envelope {NS}><soap:Header><t:RequestServerVersion Version="{REQUEST_VERSION}"/></soap:Header><soap:Body>{body}</soap:Body></soap:Envelope>"#
    )
}

/// `<t:ItemId Id=… ChangeKey=…/>` и подобные: имя элемента, id, ключ изменения.
pub(crate) fn id_el(name: &str, id: &str, change_key: Option<&str>) -> String {
    match change_key {
        Some(ck) => format!(r#"<{name} Id="{}" ChangeKey="{}"/>"#, esc(id), esc(ck)),
        None => format!(r#"<{name} Id="{}"/>"#, esc(id)),
    }
}

/// Элемент с текстом: `<t:Subject>…</t:Subject>`.
pub(crate) fn text_el(name: &str, text: &str) -> String {
    format!("<{name}>{}</{name}>", esc(text))
}

pub(crate) fn server_version(doc: &El) -> Option<ServerVersion> {
    let v = doc.at(&["Header", "ServerVersionInfo"])?;
    let n = |k: &str| v.attr(k).and_then(|s| s.parse().ok()).unwrap_or(0);
    Some(ServerVersion { major: n("MajorVersion"), minor: n("MinorVersion"), build: n("MajorBuildNumber"), revision: n("MinorBuildNumber") })
}

/// `BackOffMilliseconds` из `MessageXml` (сбой или сообщение ответа).
fn back_off(el: &El) -> u64 {
    el.find("MessageXml")
        .and_then(|x| x.all("Value").find(|v| v.attr("Name") == Some("BackOffMilliseconds")))
        .and_then(|v| v.text.trim().parse().ok())
        .unwrap_or(1000)
}

fn error(code: &str, message: String, el: &El) -> Error {
    if code == "ErrorServerBusy" { Error::ServerBusy { back_off_ms: back_off(el) } } else { Error::Ews { code: code.to_owned(), message } }
}

/// Ответ операции: первый элемент `Body`. `Fault` и `ErrorServerBusy` в любом сообщении — ошибка.
pub(crate) fn response(doc: El) -> Result<El> {
    if doc.name != "Envelope" {
        return Err(Error::Xml(format!("expected SOAP Envelope, got {}", doc.name)));
    }
    let body = doc.children.into_iter().find(|c| c.name == "Body").ok_or_else(|| Error::Xml("no SOAP Body".into()))?;
    let first = body.children.into_iter().next().ok_or_else(|| Error::Xml("empty SOAP Body".into()))?;
    if first.name == "Fault" {
        let code = first
            .find("ResponseCode")
            .map(|c| c.text.trim().to_owned())
            .or_else(|| first.text_at(&["faultcode"]).map(|c| c.rsplit(':').next().unwrap_or(c).to_owned()))
            .unwrap_or_default();
        let message = first.text_at(&["faultstring"]).unwrap_or("").to_owned();
        return Err(error(&code, message, &first));
    }
    if let Some(busy) = messages(&first).into_iter().find(|m| m.text_at(&["ResponseCode"]) == Some("ErrorServerBusy")) {
        return Err(Error::ServerBusy { back_off_ms: back_off(busy) });
    }
    Ok(first)
}

/// Сообщения ответа: `<m:ResponseMessages>` → `<m:{Op}ResponseMessage>` по одному на объект запроса.
pub(crate) fn messages(resp: &El) -> Vec<&El> {
    resp.child("ResponseMessages").map(|m| m.children.iter().collect()).unwrap_or_default()
}

/// Код сообщения ответа (`NoError`, `ErrorItemNotFound`…).
pub(crate) fn code(msg: &El) -> &str {
    msg.text_at(&["ResponseCode"]).unwrap_or("")
}

/// Сообщение без ошибки (`Success` или `Warning`), иначе — его ошибка.
pub(crate) fn check(msg: &El) -> Result<()> {
    if msg.attr("ResponseClass") == Some("Error") {
        let message = msg.text_at(&["MessageText"]).unwrap_or("").to_owned();
        return Err(error(code(msg), message, msg));
    }
    Ok(())
}

/// Единственное сообщение ответа, проверенное.
pub(crate) fn single(resp: &El) -> Result<&El> {
    let msg = messages(resp).into_iter().next().ok_or_else(|| Error::Xml(format!("{}: no response message", resp.name)))?;
    check(msg)?;
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml::parse;

    #[test]
    fn envelope_is_well_formed() {
        let e = envelope("<m:GetFolder/>");
        let doc = parse(e.as_bytes()).unwrap();
        assert_eq!(doc.at(&["Header", "RequestServerVersion"]).and_then(|v| v.attr("Version")), Some(REQUEST_VERSION));
        assert!(doc.at(&["Body", "GetFolder"]).is_some());
        assert_eq!(id_el("t:ItemId", "a\"b", Some("c")), r#"<t:ItemId Id="a&quot;b" ChangeKey="c"/>"#);
    }

    #[test]
    fn server_busy_fault() {
        let doc = parse(include_bytes!("../tests/fixtures/fault_server_busy.xml")).unwrap();
        assert!(matches!(response(doc), Err(Error::ServerBusy { back_off_ms: 29_230 })));
    }

    #[test]
    fn schema_fault() {
        let doc = parse(include_bytes!("../tests/fixtures/fault_schema.xml")).unwrap();
        let err = response(doc).unwrap_err();
        assert_eq!(err.code(), Some("ErrorSchemaValidation"), "{err}");
    }

    #[test]
    fn version_from_header() {
        let doc = parse(include_bytes!("../tests/fixtures/get_folder_calendar.xml")).unwrap();
        assert_eq!(server_version(&doc), Some(ServerVersion { major: 15, minor: 1, build: 2507, revision: 6 }));
    }
}
