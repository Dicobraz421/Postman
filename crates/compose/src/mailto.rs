//! Ссылка `mailto:` (RFC 6068) — в новое письмо (S5-30: Postman — почтовый клиент по умолчанию). Адресаты до `?`,
//! поля `to`, `cc`, `bcc`, `subject`, `body`; `%XX` — байты UTF-8, `+` в теле — сам знак (не пробел).

use crate::{Address, parse_addresses};

/// Что взять в новое письмо.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mailto {
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub subject: String,
    pub body: String,
}

/// Разобрать `mailto:…`; не `mailto:` — `None`. Неразобранные адреса пропускаются.
pub fn parse_mailto(uri: &str) -> Option<Mailto> {
    let uri = uri.trim();
    let rest = uri.get(..7).filter(|p| p.eq_ignore_ascii_case("mailto:")).map(|_| &uri[7..])?;
    let (to, query) = rest.split_once('?').unwrap_or((rest, ""));
    let mut m = Mailto { to: parse_addresses(&decode(to)).0, ..Mailto::default() };
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = decode(v);
        match k.to_ascii_lowercase().as_str() {
            "to" => m.to.extend(parse_addresses(&v).0),
            "cc" => m.cc.extend(parse_addresses(&v).0),
            "bcc" => m.bcc.extend(parse_addresses(&v).0),
            "subject" => m.subject = v,
            "body" => m.body = v.replace("\r\n", "\n"),
            _ => {}
        }
    }
    Some(m)
}

/// `%XX` → байты, затем UTF-8 (битые последовательности — замена).
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        match (b[i], b.get(i + 1).copied().and_then(hex), b.get(i + 2).copied().and_then(hex)) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (c, _, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emails(a: &[Address]) -> Vec<&str> {
        a.iter().map(|a| a.email.as_str()).collect()
    }

    #[test]
    fn addresses_and_fields() {
        let m = parse_mailto(
            "mailto:anna@corp.example,boris@corp.example?cc=vera@x.ru&subject=%D0%9F%D1%80%D0%B8%D0%B2%D0%B5%D1%82&body=1%2B1%0D%0A%D0%B4%D0%B0",
        )
        .unwrap();
        assert_eq!(emails(&m.to), ["anna@corp.example", "boris@corp.example"]);
        assert_eq!(emails(&m.cc), ["vera@x.ru"]);
        assert_eq!(m.subject, "Привет");
        assert_eq!(m.body, "1+1\nда");
        let m = parse_mailto("MAILTO:?to=a@x.ru&bcc=b@x.ru&unknown=1").unwrap();
        assert_eq!(emails(&m.to), ["a@x.ru"]);
        assert_eq!(emails(&m.bcc), ["b@x.ru"]);
    }

    #[test]
    fn not_mailto_and_broken_input() {
        assert_eq!(parse_mailto("https://corp.example"), None);
        assert_eq!(parse_mailto("--minimized"), None);
        let m = parse_mailto("mailto:%ZZ?subject=%").unwrap();
        assert!(m.to.is_empty());
        assert_eq!(m.subject, "%");
    }
}
