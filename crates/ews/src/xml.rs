//! Ответы своего сервера (SOAP, Autodiscover) — маленьким деревом элементов на quick-xml (ADR 0003): без DTD и
//! сущностей (только пять предопределённых и ссылки на символы), с пределами глубины и числа узлов. Имена — без
//! префикса пространства имён: у EWS они не пересекаются.

use quick_xml::events::{BytesStart, Event};

use crate::{Error, Result};

/// Глубже ответы EWS не бывают (у `GetItem` — около 12).
const MAX_DEPTH: usize = 64;
/// Узлов в одном ответе — с запасом для пачки в 20 встреч с участниками.
const MAX_NODES: usize = 500_000;

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct El {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    /// Текст листа как есть; у элемента с детьми — пусто.
    pub text: String,
    pub children: Vec<El>,
}

impl El {
    pub fn child(&self, name: &str) -> Option<&El> {
        self.children.iter().find(|c| c.name == name)
    }

    pub fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a El> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }

    /// Потомок по пути имён.
    pub fn at(&self, path: &[&str]) -> Option<&El> {
        path.iter().try_fold(self, |e, n| e.child(n))
    }

    /// Текст потомка по пути, без пробелов по краям; нет элемента — `None`.
    pub fn text_at(&self, path: &[&str]) -> Option<&str> {
        self.at(path).map(|e| e.text.trim())
    }

    /// Текст потомка или пустая строка.
    pub fn str_at(&self, path: &[&str]) -> String {
        self.text_at(path).unwrap_or("").to_owned()
    }

    pub fn bool_at(&self, path: &[&str]) -> bool {
        self.text_at(path).is_some_and(|t| t.eq_ignore_ascii_case("true"))
    }

    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// Первый потомок с таким именем на любой глубине (в глубину).
    pub fn find(&self, name: &str) -> Option<&El> {
        self.children.iter().find_map(|c| if c.name == name { Some(c) } else { c.find(name) })
    }
}

fn local(name: &[u8]) -> String {
    let name = name.rsplit(|b| *b == b':').next().unwrap_or(name);
    String::from_utf8_lossy(name).into_owned()
}

fn element(e: &BytesStart<'_>) -> Result<El> {
    let mut attrs = Vec::new();
    for a in e.attributes() {
        let a = a.map_err(|e| Error::Xml(e.to_string()))?;
        let key = a.key.as_ref();
        if key == b"xmlns" || key.starts_with(b"xmlns:") {
            continue;
        }
        let value = a.normalized_value(quick_xml::XmlVersion::Implicit1_0).map_err(|e| Error::Xml(e.to_string()))?;
        attrs.push((local(key), value.into_owned()));
    }
    Ok(El { name: local(e.name().as_ref()), attrs, ..El::default() })
}

/// Разобрать документ; результат — корневой элемент.
pub(crate) fn parse(bytes: &[u8]) -> Result<El> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error::Xml("not UTF-8".into()))?;
    let mut reader = quick_xml::Reader::from_str(text);
    let mut stack: Vec<El> = Vec::new();
    let mut nodes = 0usize;
    let mut root: Option<El> = None;
    loop {
        let ev = reader.read_event().map_err(|e| Error::Xml(e.to_string()))?;
        match ev {
            Event::Start(_) | Event::Empty(_) if root.is_some() => {
                return Err(Error::Xml("content after the root element".into()));
            }
            Event::Start(e) => {
                nodes += 1;
                if stack.len() >= MAX_DEPTH || nodes > MAX_NODES {
                    return Err(Error::Xml("document too deep or too large".into()));
                }
                stack.push(element(&e)?);
            }
            Event::Empty(e) => {
                nodes += 1;
                if nodes > MAX_NODES {
                    return Err(Error::Xml("document too large".into()));
                }
                let el = element(&e)?;
                match stack.last_mut() {
                    Some(parent) => parent.children.push(el),
                    None => root = Some(el),
                }
            }
            Event::End(_) => {
                let mut el = stack.pop().ok_or_else(|| Error::Xml("unbalanced end tag".into()))?;
                if !el.children.is_empty() {
                    el.text.clear();
                }
                match stack.last_mut() {
                    Some(parent) => parent.children.push(el),
                    None => root = Some(el),
                }
            }
            Event::Text(t) => {
                if let Some(cur) = stack.last_mut() {
                    cur.text.push_str(&t.xml10_content().map_err(|e| Error::Xml(e.to_string()))?);
                }
            }
            Event::CData(t) => {
                if let Some(cur) = stack.last_mut() {
                    cur.text.push_str(&String::from_utf8_lossy(&t));
                }
            }
            Event::GeneralRef(r) => {
                let ch = match r.resolve_char_ref().map_err(|e| Error::Xml(e.to_string()))? {
                    Some(c) => c,
                    None => match r.decode().map_err(|e| Error::Xml(e.to_string()))?.as_ref() {
                        "amp" => '&',
                        "lt" => '<',
                        "gt" => '>',
                        "quot" => '"',
                        "apos" => '\'',
                        // Своих сущностей у EWS нет; чужие (из DTD) не раскрываем.
                        other => return Err(Error::Xml(format!("unknown entity &{other};"))),
                    },
                };
                if let Some(cur) = stack.last_mut() {
                    cur.text.push(ch);
                }
            }
            Event::DocType(_) => return Err(Error::Xml("DTD is not allowed".into())),
            Event::Decl(_) | Event::PI(_) | Event::Comment(_) => {}
            Event::Eof => break,
        }
    }
    if !stack.is_empty() {
        return Err(Error::Xml("unexpected end of document".into()));
    }
    root.ok_or_else(|| Error::Xml("empty document".into()))
}

/// Текст для XML (и для значения атрибута): пять предопределённых сущностей; управляющие символы, которых нет в
/// XML 1.0, выбрасываются.
pub(crate) fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 || c == '\u{FFFE}' || c == '\u{FFFF}' => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{esc, parse};

    #[test]
    fn tree_without_prefixes() {
        let doc = parse(
            br#"<?xml version="1.0"?><s:Envelope xmlns:s="x"><s:Body><m:R a="1 &amp; 2"><t:V>a &lt; b&#33;</t:V><t:E/></m:R></s:Body></s:Envelope>"#,
        )
        .unwrap();
        assert_eq!(doc.name, "Envelope");
        let r = doc.at(&["Body", "R"]).unwrap();
        assert_eq!(r.attr("a"), Some("1 & 2"));
        assert_eq!(r.text_at(&["V"]), Some("a < b!"));
        assert!(r.child("E").is_some());
        assert_eq!(doc.find("V").map(|v| v.text.as_str()), Some("a < b!"));
    }

    #[test]
    fn no_dtd_no_entities() {
        assert!(parse(br#"<!DOCTYPE x [<!ENTITY e "boom">]><x>&e;</x>"#).is_err());
        assert!(parse(b"<x>&e;</x>").is_err());
        assert!(parse(b"<x><y></x>").is_err());
        assert!(parse(b"<x/><y/>").is_err());
        assert!(parse(b"").is_err());
        let deep = "<a>".repeat(100) + &"</a>".repeat(100);
        assert!(parse(deep.as_bytes()).is_err());
    }

    #[test]
    fn escape_text() {
        assert_eq!(esc("a<b & \"c\"\u{1}"), "a&lt;b &amp; &quot;c&quot;");
        let round = parse(format!("<x>{}</x>", esc("отчёт <Q3> & 'итог'")).as_bytes()).unwrap();
        assert_eq!(round.text, "отчёт <Q3> & 'итог'");
    }
}
