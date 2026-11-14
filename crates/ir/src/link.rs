use url::Url;

#[derive(Debug, Clone, PartialEq)]
pub struct LinkCheck {
    pub href: String,
    pub host: Option<String>,
    pub suspicious: bool,
}

/// Проверяет ссылку: разрешены `http(s):`, `mailto:` и ссылки самого Postman — `postman://…` (адрес беседы или
/// сообщения) и `mid:…` (письмо по `Message-ID`, RFC 2392). Адрес Postman не несёт действий (ADR 0012 п. 3), поэтому
/// из чужого письма безопасен; с запросом (`?`) или фрагментом (`#`) — не адрес, отбрасывается.
/// `visible` — видимый текст ссылки; если он выглядит как адрес другого хоста,
/// ссылка помечается подозрительной.
pub fn check_link(href: &str, visible: &str) -> Option<LinkCheck> {
    let url = Url::parse(href.trim()).ok()?;
    match url.scheme() {
        "http" | "https" => {
            let host = url.host_str()?.to_string();
            let suspicious = visible_host(visible).is_some_and(|v| !same_host(&v, &host));
            Some(LinkCheck { href: url.into(), host: Some(host), suspicious })
        }
        "mailto" => Some(LinkCheck { href: url.into(), host: None, suspicious: false }),
        "postman" | "mid" if is_address(&url) => Some(LinkCheck { href: url.into(), host: None, suspicious: false }),
        _ => None,
    }
}

/// Адрес Postman: `postman://<канал>/…` с каналом и путём или `mid:<Message-ID>`; без запроса и фрагмента.
fn is_address(url: &Url) -> bool {
    if url.query().is_some() || url.fragment().is_some() {
        return false;
    }
    match url.scheme() {
        "postman" => url.host_str().is_some_and(|h| !h.is_empty()) && url.path().len() > 1,
        _ => url.cannot_be_a_base() && !url.path().is_empty(),
    }
}

/// Хост из видимого текста, если текст похож на URL или доменное имя.
fn visible_host(text: &str) -> Option<String> {
    let t = text.trim();
    if t.is_empty() || t.chars().any(char::is_whitespace) || !t.contains('.') {
        return None;
    }
    let candidate = if t.contains("://") { t.to_string() } else { format!("http://{t}") };
    let url = Url::parse(&candidate).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = url.host_str()?;
    // «Итого 5.00» или «v1.2» — не домен.
    let tld = host.rsplit('.').next()?;
    if tld.len() < 2 || tld.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(host.to_string())
}

fn same_host(a: &str, b: &str) -> bool {
    a.trim_start_matches("www.") == b.trim_start_matches("www.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemes() {
        assert!(check_link("javascript:alert(1)", "x").is_none());
        assert!(check_link("data:text/html,hi", "x").is_none());
        assert!(check_link("file:///c:/x", "x").is_none());
        assert!(check_link("mailto:a@b.c", "a@b.c").is_some());
        assert!(check_link("vbscript:msgbox(1)", "x").is_none());
        assert!(check_link("tg://resolve?domain=x", "x").is_none());
    }

    /// Ссылки Postman (ADR 0012): адрес беседы или сообщения и `mid:` письма сохраняются как есть.
    #[test]
    fn postman_addresses() {
        let l = check_link("postman://demo/me%40example.test/g-team/m7", "сообщение").unwrap();
        assert_eq!(l.href, "postman://demo/me%40example.test/g-team/m7");
        assert_eq!(l.host, None);
        assert!(!l.suspicious);
        let l = check_link("mid:m1%40example.test", "письмо").unwrap();
        assert_eq!(l.href, "mid:m1%40example.test");
        assert!(!l.suspicious);
        assert!(check_link("MID:m1@example.test", "x").is_some());
        // Пустой адрес, запрос и фрагмент — не адрес Postman.
        assert!(check_link("postman://", "x").is_none());
        assert!(check_link("postman://demo", "x").is_none());
        assert!(check_link("postman:demo/me/g", "x").is_none());
        assert!(check_link("mid:", "x").is_none());
        assert!(check_link("postman://demo/me/g?do=delete", "x").is_none());
        assert!(check_link("mid:m1@example.test#x", "x").is_none());
    }

    /// HTML письма: `postman://` и `mid:` остаются ссылками, `javascript:` — текстом.
    #[test]
    fn html_keeps_postman_links() {
        let doc = crate::html::from_html(
            r#"<p><a href="postman://demo/me/g/1">чат</a> <a href="mid:m1@example.test">письмо</a> <a href="javascript:alert(1)">плохо</a></p>"#,
        );
        let mut hrefs = Vec::new();
        for b in &doc.blocks {
            if let crate::Block::Paragraph { content, .. } = b {
                for i in content {
                    if let crate::Inline::Link { href, .. } = i {
                        hrefs.push(href.clone());
                    }
                }
            }
        }
        assert_eq!(hrefs, ["postman://demo/me/g/1", "mid:m1@example.test"]);
    }

    #[test]
    fn mismatch() {
        let l = check_link("https://evil.example/login", "https://bank.ru").unwrap();
        assert!(l.suspicious);
        let l = check_link("https://www.bank.ru/x", "bank.ru").unwrap();
        assert!(!l.suspicious);
        let l = check_link("https://evil.example/", "Войти в банк").unwrap();
        assert!(!l.suspicious);
    }

    #[test]
    fn idn_shown_as_punycode() {
        let l = check_link("https://аpple.com/", "apple.com").unwrap();
        assert!(l.host.as_deref().unwrap().starts_with("xn--"));
        assert!(l.suspicious);
    }
}
