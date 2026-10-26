//! Autodiscover POX (`autodiscover.xml`, схема Outlook 2006a): адрес EWS по адресу почты. Порядок — как у
//! Outlook для on-prem без DNS: `https://<домен>/autodiscover/autodiscover.xml`, затем
//! `https://autodiscover.<домен>/…`; ответы `redirectAddr`/`redirectUrl` и HTTP-переадресации — не больше
//! [`MAX_REDIRECTS`]. Не нашлось — `https://<хост>/EWS/Exchange.asmx` (хост — подсказка вызывающего, иначе домен и
//! `mail.<домен>`); его проверит первый запрос клиента.
//!
//! Вход — тот же SSPI, что у EWS, поэтому переадресации — только на https и только в домен адреса (и его
//! поддомены): чужому хосту рукопожатие Windows не отдаём.

use std::collections::VecDeque;
use std::time::Duration;

use crate::xml::{El, esc};
use crate::{Auth, Error, Result};

/// Переадресаций всех видов за один поиск.
pub const MAX_REDIRECTS: usize = 5;
/// Один адрес Autodiscover — не дольше (на домене компании часто сайт, который не отвечает на POST).
const TIMEOUT: Duration = Duration::from_secs(15);

const REQUEST_SCHEMA: &str = "http://schemas.microsoft.com/exchange/autodiscover/outlook/requestschema/2006";
const RESPONSE_SCHEMA: &str = "http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a";

/// Откуда адрес EWS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Autodiscover,
    /// Autodiscover не ответил — догадка `https://<хост>/EWS/Exchange.asmx`.
    Fallback,
}

/// Найденные адреса EWS — по порядку предпочтения (внутренний `EXCH`, затем внешний `EXPR`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub urls: Vec<String>,
    pub source: Source,
    /// Имя владельца ящика из ответа (пусто у догадки).
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pox {
    Settings { urls: Vec<String>, name: String },
    RedirectAddr(String),
    RedirectUrl(String),
    Error { code: String, message: String },
}

pub(crate) fn request(email: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><Autodiscover xmlns="{REQUEST_SCHEMA}"><Request><EMailAddress>{}</EMailAddress><AcceptableResponseSchema>{RESPONSE_SCHEMA}</AcceptableResponseSchema></Request></Autodiscover>"#,
        esc(email)
    )
}

pub(crate) fn parse(body: &[u8]) -> Result<Pox> {
    let doc = crate::xml::parse(body)?;
    if doc.name != "Autodiscover" {
        return Err(Error::Autodiscover(format!("not an Autodiscover response: {}", doc.name)));
    }
    let resp = doc.child("Response").ok_or_else(|| Error::Autodiscover("no Response".into()))?;
    if let Some(e) = resp.child("Error") {
        return Ok(Pox::Error { code: e.str_at(&["ErrorCode"]), message: e.str_at(&["Message"]) });
    }
    let account = resp.child("Account").ok_or_else(|| Error::Autodiscover("no Account".into()))?;
    match account.text_at(&["Action"]) {
        Some("redirectAddr") => {
            return account
                .text_at(&["RedirectAddr"])
                .map(|a| Pox::RedirectAddr(a.to_owned()))
                .ok_or_else(|| Error::Autodiscover("no RedirectAddr".into()));
        }
        Some("redirectUrl") => {
            return account
                .text_at(&["RedirectUrl"])
                .map(|a| Pox::RedirectUrl(a.to_owned()))
                .ok_or_else(|| Error::Autodiscover("no RedirectUrl".into()));
        }
        _ => {}
    }
    let protocols: Vec<&El> = account.all("Protocol").collect();
    let mut urls: Vec<String> = Vec::new();
    for kind in ["EXCH", "EXPR"] {
        for p in protocols.iter().filter(|p| p.text_at(&["Type"]) == Some(kind)) {
            if let Some(u) = p.text_at(&["EwsUrl"]).filter(|u| !u.is_empty())
                && !urls.iter().any(|x| x.eq_ignore_ascii_case(u))
            {
                urls.push(u.to_owned());
            }
        }
    }
    Ok(Pox::Settings { urls, name: resp.str_at(&["User", "DisplayName"]) })
}

fn domain_of(email: &str) -> Result<String> {
    email
        .rsplit_once('@')
        .map(|(_, d)| d.trim().to_ascii_lowercase())
        .filter(|d| !d.is_empty() && !d.contains('/'))
        .ok_or_else(|| Error::Autodiscover("not an e-mail address".into()))
}

/// https и хост — домен адреса или его поддомен.
fn allowed(url: &url::Url, domain: &str) -> bool {
    url.scheme() == "https"
        && url.host_str().is_some_and(|h| {
            let h = h.to_ascii_lowercase();
            h == domain || h.ends_with(&format!(".{domain}"))
        })
}

/// Один адрес Autodiscover (`https://…/autodiscover/autodiscover.xml`) без переадресаций: настройки — адреса EWS,
/// иначе ошибка с видом ответа. Для разведки и ручного адреса Autodiscover.
pub async fn discover_at(url: &str, email: &str, auth: &Auth) -> Result<Endpoint> {
    let parsed = url::Url::parse(url)?;
    let mut http = crate::http::Http::new(auth.clone());
    http.timeout = TIMEOUT;
    let reply = http.post(&parsed, request(email).into_bytes()).await?;
    if reply.status != 200 {
        return Err(Error::Status(reply.status));
    }
    match parse(&reply.body)? {
        Pox::Settings { urls, name } if !urls.is_empty() => Ok(Endpoint { urls, source: Source::Autodiscover, display_name: name }),
        Pox::Settings { .. } => Err(Error::Autodiscover("no EWS URL in settings".into())),
        Pox::RedirectAddr(_) => Err(Error::Autodiscover("redirectAddr".into())),
        Pox::RedirectUrl(_) => Err(Error::Autodiscover("redirectUrl".into())),
        Pox::Error { code, .. } => Err(Error::Autodiscover(format!("error {code}"))),
    }
}

/// Адрес EWS по хосту сервера.
pub fn fallback_url(host: &str) -> String {
    format!("https://{}/EWS/Exchange.asmx", host.trim().trim_end_matches('/'))
}

/// Найти адрес EWS для `email`. Неверный пароль — [`Error::Auth`] сразу (без догадки); остальные сбои (нет
/// ответа, сайт компании вместо Exchange, только Basic) — следующий адрес, затем догадка по `fallback_host` (обычно хост IMAP той же учётки) или по домену адреса.
pub async fn discover(email: &str, auth: &Auth, fallback_host: Option<&str>) -> Result<Endpoint> {
    let first_domain = domain_of(email)?;
    let mut http = crate::http::Http::new(auth.clone());
    http.timeout = TIMEOUT;
    let mut email = email.trim().to_owned();
    let mut redirects = 0usize;
    'address: loop {
        let domain = domain_of(&email)?;
        let mut queue: VecDeque<String> =
            [format!("https://{domain}/autodiscover/autodiscover.xml"), format!("https://autodiscover.{domain}/autodiscover/autodiscover.xml")]
                .into();
        while let Some(next) = queue.pop_front() {
            let Ok(url) = url::Url::parse(&next) else { continue };
            if !allowed(&url, &domain) {
                tracing::warn!("autodiscover: redirect outside the mail domain ignored");
                continue;
            }
            let reply = match http.post(&url, request(&email).into_bytes()).await {
                Ok(r) => r,
                Err(e @ Error::Auth(_)) => return Err(e),
                Err(e) => {
                    tracing::debug!(error = %e, "autodiscover candidate failed");
                    continue;
                }
            };
            if matches!(reply.status, 301 | 302 | 303 | 307 | 308) {
                redirects += 1;
                if let Some(to) = reply.location.as_deref().and_then(|l| url.join(l).ok())
                    && redirects <= MAX_REDIRECTS
                {
                    queue.push_front(to.to_string());
                }
                continue;
            }
            if reply.status != 200 {
                tracing::debug!(status = reply.status, "autodiscover candidate failed");
                continue;
            }
            match parse(&reply.body) {
                Ok(Pox::Settings { urls, name }) if !urls.is_empty() => {
                    tracing::debug!(urls = urls.len(), "autodiscover found EWS");
                    return Ok(Endpoint { urls, source: Source::Autodiscover, display_name: name });
                }
                Ok(Pox::RedirectAddr(addr)) if redirects < MAX_REDIRECTS => {
                    redirects += 1;
                    email = addr;
                    continue 'address;
                }
                Ok(Pox::RedirectUrl(u)) if redirects < MAX_REDIRECTS => {
                    redirects += 1;
                    queue.push_front(u);
                }
                Ok(other) => tracing::debug!(response = ?std::mem::discriminant(&other), "autodiscover: no EWS URL"),
                Err(e) => tracing::debug!(error = %e, "autodiscover: bad response"),
            }
        }
        break;
    }
    let urls = match fallback_host {
        Some(h) if !h.trim().is_empty() => vec![fallback_url(h)],
        _ => vec![fallback_url(&first_domain), fallback_url(&format!("mail.{first_domain}"))],
    };
    Ok(Endpoint { urls, source: Source::Fallback, display_name: String::new() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_internal_first() {
        let p = parse(include_bytes!("../tests/fixtures/autodiscover_settings.xml")).unwrap();
        assert_eq!(
            p,
            Pox::Settings {
                urls: vec!["https://mail.example.test/EWS/Exchange.asmx".into(), "https://webmail.example.test/EWS/Exchange.asmx".into()],
                name: "Тестовый Пользователь 1".into(),
            }
        );
    }

    #[test]
    fn redirect_and_error() {
        assert_eq!(
            parse(include_bytes!("../tests/fixtures/autodiscover_redirect.xml")).unwrap(),
            Pox::RedirectAddr("user1@corp.example.test".into())
        );
        let Pox::Error { code, .. } = parse(include_bytes!("../tests/fixtures/autodiscover_error.xml")).unwrap() else { panic!() };
        assert_eq!(code, "500");
        assert!(parse(b"<html><body>not here</body></html>").is_err());
    }

    #[test]
    fn request_and_rules() {
        let r = request("user1@example.test");
        assert!(r.contains("<EMailAddress>user1@example.test</EMailAddress>") && r.contains(RESPONSE_SCHEMA));
        assert_eq!(domain_of("User1@Example.Test").unwrap(), "example.test");
        assert!(domain_of("nobody").is_err());
        let u = |s: &str| url::Url::parse(s).unwrap();
        assert!(allowed(&u("https://autodiscover.example.test/x"), "example.test"));
        assert!(!allowed(&u("http://autodiscover.example.test/x"), "example.test"));
        assert!(!allowed(&u("https://example.test.evil.test/x"), "example.test"));
        assert!(!allowed(&u("https://notexample.test/x"), "example.test"));
        assert_eq!(fallback_url("mail.example.test/"), "https://mail.example.test/EWS/Exchange.asmx");
    }
}
