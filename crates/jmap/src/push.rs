//! Push по JMAP EventSource (RFC 8620 §7.3): поток Server-Sent Events,
//! событие `state` означает «что-то изменилось — запросите `*/changes`».

use crate::{Client, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
}

pub struct EventSource {
    resp: reqwest::Response,
    buf: Vec<u8>,
}

impl Client {
    /// Подписка на все типы; сервер шлёт `ping` раз в `ping_secs`, чтобы обрыв был заметен.
    pub async fn event_source(&self, ping_secs: u32) -> Result<EventSource> {
        let url = self.session().event_source_url.replace("{types}", "*").replace("{closeafter}", "no").replace("{ping}", &ping_secs.to_string());
        let resp = self.authorized(self.http().get(url)).header(reqwest::header::ACCEPT, "text/event-stream").send().await?.error_for_status()?;
        Ok(EventSource { resp, buf: Vec::new() })
    }
}

impl EventSource {
    /// Следующее событие; `None` — сервер закрыл поток.
    pub async fn next(&mut self) -> Result<Option<SseEvent>> {
        loop {
            if let Some((ev, used)) = parse(&self.buf) {
                self.buf.drain(..used);
                if let Some(ev) = ev {
                    return Ok(Some(ev));
                }
                continue;
            }
            // Защита от бесконечной строки без разделителя.
            if self.buf.len() > 1024 * 1024 {
                self.buf.clear();
            }
            match self.resp.chunk().await? {
                Some(c) => self.buf.extend_from_slice(&c),
                None => return Ok(None),
            }
        }
    }
}

/// Разобрать одно событие из начала буфера: `(событие или комментарий, сколько байт съедено)`.
fn parse(buf: &[u8]) -> Option<(Option<SseEvent>, usize)> {
    let text = std::str::from_utf8(buf).ok().or_else(|| {
        // Неполный UTF-8 в конце буфера — берём только целую часть.
        let valid = std::str::from_utf8(buf).err()?.valid_up_to();
        std::str::from_utf8(&buf[..valid]).ok()
    })?;
    let normalized_end = ["\r\n\r\n", "\n\n", "\r\r"].iter().filter_map(|sep| text.find(sep).map(|i| (i, sep.len()))).min()?;
    let (end, sep_len) = normalized_end;
    let block = &text[..end];
    let mut event = String::from("message");
    let mut data: Vec<&str> = Vec::new();
    for line in block.lines() {
        if line.starts_with(':') {
            continue;
        }
        let (field, value) = line.split_once(':').map_or((line, ""), |(f, v)| (f, v.strip_prefix(' ').unwrap_or(v)));
        match field {
            "event" => event = value.to_string(),
            "data" => data.push(value),
            _ => {}
        }
    }
    let ev = (!data.is_empty() || event != "message").then(|| SseEvent { event, data: data.join("\n") });
    Some((ev, end + sep_len))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_parsing() {
        let buf = b": comment\n\nevent: state\ndata: {\"@type\":\"StateChange\"}\n\nevent: ping\ndata: {}\n\nevent: sta";
        let (ev, n) = parse(buf).unwrap();
        assert!(ev.is_none());
        let rest = &buf[n..];
        let (ev, n2) = parse(rest).unwrap();
        assert_eq!(ev.unwrap(), SseEvent { event: "state".into(), data: "{\"@type\":\"StateChange\"}".into() });
        let rest = &rest[n2..];
        let (ev, n3) = parse(rest).unwrap();
        assert_eq!(ev.unwrap().event, "ping");
        assert!(parse(&rest[n3..]).is_none(), "неполное событие ждёт данных");
        let crlf = b"event: state\r\ndata: a\r\ndata: b\r\n\r\n";
        assert_eq!(parse(crlf).unwrap().0.unwrap().data, "a\nb");
    }
}
