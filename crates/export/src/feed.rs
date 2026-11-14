//! Лента беседы любого канала в файл (ADR 0012 п. 8 «Экспорт, печать»): HTML для чтения и печати (из браузера — и в
//! PDF) и JSON для других программ. Без I/O: на входе — [`Feed`], на выходе — байты файла; что в ленте и как назвать
//! людей, решает ядро (`postman_core::channel::export`).
//!
//! HTML — один файл без скриптов и внешних ресурсов, текст экранирован; JSON — UTF-8, поля по-английски, время —
//! секунды Unix (UTC) и строкой ISO 8601 с местным сдвигом.

use std::fmt::Write as _;

use crate::Civil;

/// Беседа для выгрузки.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Feed {
    pub title: String,
    /// Канал: `mail`, `telegram`, `matrix`, `demo`.
    pub channel: String,
    /// Адрес беседы `postman://…`.
    pub address: String,
    /// Сдвиг местного времени от UTC, секунды — для дат в HTML и ISO.
    pub offset: i32,
    pub entries: Vec<Entry>,
    /// Подписи HTML (язык интерфейса): «изменено», «удалено», «переслано от», «вложения».
    pub labels: Labels,
}

/// Подписи в HTML на языке интерфейса (тексты — у интерфейса, Fluent).
#[derive(Debug, Clone, PartialEq)]
pub struct Labels {
    pub edited: String,
    pub deleted: String,
    pub forwarded: String,
    pub reply_to: String,
}

impl Default for Labels {
    fn default() -> Self {
        Self { edited: "edited".into(), deleted: "deleted".into(), forwarded: "forwarded from".into(), reply_to: "in reply to".into() }
    }
}

/// Строка ленты.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Entry {
    /// id сообщения у сервера канала (`Message-ID` у письма).
    pub id: String,
    pub author: String,
    /// Адрес автора (почта, ник, id) — в JSON и подсказке HTML.
    pub author_id: String,
    /// Секунды Unix.
    pub at: i64,
    /// Время правки, секунды Unix.
    pub edited: Option<i64>,
    /// «Сообщение удалено»: без текста и вложений.
    pub deleted: bool,
    /// Текст — абзацами (строка — абзац).
    pub text: String,
    pub reply_to: Option<String>,
    /// Переслано от (имя).
    pub forwarded: Option<String>,
    pub quotes: Vec<String>,
    pub attachments: Vec<File>,
    /// Реакция и сколько.
    pub reactions: Vec<(String, u32)>,
    pub mine: bool,
}

/// Вложение строки: имя, тип, размер (байт), если известен.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct File {
    pub name: String,
    pub mime: String,
    pub size: Option<u64>,
}

fn html_escape(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
}

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    html_escape(s, &mut out);
    out
}

/// Время ISO 8601 с местным сдвигом: `2026-11-14T10:05:00+03:00`.
pub fn iso(secs: i64, offset: i32) -> String {
    let c = Civil::from_unix(secs.saturating_add(i64::from(offset)));
    let sign = if offset < 0 { '-' } else { '+' };
    let off = offset.unsigned_abs();
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{sign}{:02}:{:02}", c.year, c.month, c.day, c.hour, c.minute, c.second, off / 3600, off / 60 % 60)
}

/// Дата и время для людей: `14.11.2026 10:05`.
fn human(secs: i64, offset: i32) -> String {
    let c = Civil::from_unix(secs.saturating_add(i64::from(offset)));
    format!("{:02}.{:02}.{:04} {:02}:{:02}", c.day, c.month, c.year, c.hour, c.minute)
}

fn size(n: u64) -> String {
    const K: f64 = 1024.0;
    let f = n as f64;
    if f < K {
        format!("{n} B")
    } else if f < K * K {
        format!("{:.1} KiB", f / K)
    } else if f < K * K * K {
        format!("{:.1} MiB", f / K / K)
    } else {
        format!("{:.1} GiB", f / K / K / K)
    }
}

const STYLE: &str = "body{font:15px/1.45 system-ui,sans-serif;max-width:46em;margin:2em auto;padding:0 1em;color:#1b1b1b;background:#fff}\
h1{font-size:1.4em;margin:0 0 .2em}.meta{color:#666;font-size:.85em;margin-bottom:1.5em}\
.m{border-top:1px solid #e3e3e3;padding:.6em 0}.m.mine{background:#f6f9ff}.h{font-size:.85em;color:#555}\
.h b{color:#1b1b1b}.t p{margin:.25em 0;white-space:pre-wrap}.q{border-left:3px solid #ccd;padding-left:.6em;color:#555}\
.a,.r{font-size:.85em;color:#444}.d{color:#888;font-style:italic}\
@media print{body{margin:0;max-width:none}.m{break-inside:avoid}}";

/// Лента в HTML: заголовок, строки по времени, вложения — именами (файлы не встраиваются).
pub fn to_html(feed: &Feed) -> String {
    let mut out = String::with_capacity(4096 + feed.entries.len() * 256);
    out.push_str("<!doctype html>\n<html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\">");
    let _ = writeln!(out, "<title>{}</title><style>{STYLE}</style></head><body>", esc(&feed.title));
    let _ = writeln!(out, "<h1>{}</h1><div class=\"meta\">{} · {}</div>", esc(&feed.title), esc(&feed.channel), esc(&feed.address));
    for e in &feed.entries {
        let _ = write!(out, "<div class=\"m{}\" id=\"{}\">", if e.mine { " mine" } else { "" }, esc(&e.id));
        let _ = write!(out, "<div class=\"h\"><b title=\"{}\">{}</b> · {}", esc(&e.author_id), esc(&e.author), human(e.at, feed.offset));
        if let Some(t) = e.edited {
            let _ = write!(out, " · {} {}", esc(&feed.labels.edited), human(t, feed.offset));
        }
        if let Some(f) = &e.forwarded {
            let _ = write!(out, " · {} {}", esc(&feed.labels.forwarded), esc(f));
        }
        if let Some(r) = &e.reply_to {
            let _ = write!(out, " · {} <a href=\"#{}\">↑</a>", esc(&feed.labels.reply_to), esc(r));
        }
        out.push_str("</div>");
        if e.deleted {
            let _ = writeln!(out, "<div class=\"d\">{}</div></div>", esc(&feed.labels.deleted));
            continue;
        }
        for q in &e.quotes {
            let _ = write!(out, "<div class=\"q\">{}</div>", esc(q));
        }
        out.push_str("<div class=\"t\">");
        for line in e.text.lines() {
            let _ = write!(out, "<p>{}</p>", esc(line));
        }
        out.push_str("</div>");
        if !e.attachments.is_empty() {
            out.push_str("<div class=\"a\">📎 ");
            let files: Vec<String> =
                e.attachments.iter().map(|f| format!("{}{}", esc(&f.name), f.size.map(|s| format!(" ({})", size(s))).unwrap_or_default())).collect();
            out.push_str(&files.join(", "));
            out.push_str("</div>");
        }
        if !e.reactions.is_empty() {
            let r: Vec<String> = e.reactions.iter().map(|(k, n)| format!("{} {n}", esc(k))).collect();
            let _ = write!(out, "<div class=\"r\">{}</div>", r.join(" "));
        }
        out.push_str("</div>\n");
    }
    out.push_str("</body></html>\n");
    out
}

/// Строка JSON (RFC 8259): кавычки, обратная косая, управляющие — экранированы.
fn json_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn json_opt(s: Option<&str>, out: &mut String) {
    match s {
        Some(s) => json_str(s, out),
        None => out.push_str("null"),
    }
}

/// Лента в JSON: `{"title", "channel", "address", "messages": [...]}`.
pub fn to_json(feed: &Feed) -> String {
    let mut out = String::with_capacity(1024 + feed.entries.len() * 256);
    out.push_str("{\"title\":");
    json_str(&feed.title, &mut out);
    out.push_str(",\"channel\":");
    json_str(&feed.channel, &mut out);
    out.push_str(",\"address\":");
    json_str(&feed.address, &mut out);
    out.push_str(",\"messages\":[");
    for (i, e) in feed.entries.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("\n{\"id\":");
        json_str(&e.id, &mut out);
        out.push_str(",\"author\":");
        json_str(&e.author, &mut out);
        out.push_str(",\"author_id\":");
        json_str(&e.author_id, &mut out);
        let _ = write!(out, ",\"time\":{},\"iso\":", e.at);
        json_str(&iso(e.at, feed.offset), &mut out);
        match e.edited {
            Some(t) => {
                let _ = write!(out, ",\"edited\":{t}");
            }
            None => out.push_str(",\"edited\":null"),
        }
        let _ = write!(out, ",\"deleted\":{},\"mine\":{},\"text\":", e.deleted, e.mine);
        json_str(&e.text, &mut out);
        out.push_str(",\"reply_to\":");
        json_opt(e.reply_to.as_deref(), &mut out);
        out.push_str(",\"forwarded_from\":");
        json_opt(e.forwarded.as_deref(), &mut out);
        out.push_str(",\"quotes\":[");
        for (j, q) in e.quotes.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            json_str(q, &mut out);
        }
        out.push_str("],\"attachments\":[");
        for (j, f) in e.attachments.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            out.push_str("{\"name\":");
            json_str(&f.name, &mut out);
            out.push_str(",\"mime\":");
            json_str(&f.mime, &mut out);
            match f.size {
                Some(s) => {
                    let _ = write!(out, ",\"size\":{s}}}");
                }
                None => out.push_str(",\"size\":null}"),
            }
        }
        out.push_str("],\"reactions\":[");
        for (j, (k, n)) in e.reactions.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            out.push_str("{\"key\":");
            json_str(k, &mut out);
            let _ = write!(out, ",\"count\":{n}}}");
        }
        out.push_str("]}");
    }
    out.push_str("\n]}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed() -> Feed {
        Feed {
            title: "Команда <проекта>".into(),
            channel: "demo".into(),
            address: "postman://demo/me@example.test/g".into(),
            offset: 3 * 3600,
            entries: vec![
                Entry {
                    id: "1".into(),
                    author: "Анна".into(),
                    author_id: "anna@example.test".into(),
                    at: 1_794_636_300,
                    text: "Привет\n<script>alert(1)</script>".into(),
                    attachments: vec![File { name: "plan.pdf".into(), mime: "application/pdf".into(), size: Some(2048) }],
                    reactions: vec![("👍".into(), 2)],
                    ..Entry::default()
                },
                Entry { id: "2".into(), author: "Я".into(), at: 1_794_636_400, deleted: true, mine: true, ..Entry::default() },
            ],
            labels: Labels::default(),
        }
    }

    /// HTML — экранирован, без скриптов; удалённое — подписью; вложения — именами с размером.
    #[test]
    fn html_is_safe_and_complete() {
        let h = to_html(&feed());
        assert!(h.contains("<title>Команда &lt;проекта&gt;</title>"));
        assert!(h.contains("&lt;script&gt;") && !h.contains("<script>"));
        assert!(h.contains("plan.pdf (2.0 KiB)") && h.contains("👍 2"));
        assert!(h.contains("14.11.2026 09:05"), "местное время: {h}");
        assert!(h.contains("class=\"d\">deleted"));
    }

    /// JSON — экранирован, время — числом и ISO с местным сдвигом.
    #[test]
    fn json_round_fields() {
        let j = to_json(&feed());
        assert!(j.contains("\"text\":\"Привет\\n<script>alert(1)</script>\""));
        assert!(j.contains("\"iso\":\"2026-11-14T09:05:00+03:00\""));
        assert!(j.contains("\"attachments\":[{\"name\":\"plan.pdf\",\"mime\":\"application/pdf\",\"size\":2048}]"));
        assert!(j.contains("\"deleted\":true,\"mine\":true"));
        assert_eq!(iso(0, -5400), "1969-12-31T22:30:00-01:30");
    }
}
