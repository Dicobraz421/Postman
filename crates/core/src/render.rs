//! Разбор тел писем в изолированном процессе (`postman-worker`).
//! В UI-процессе HTML не разбирается никогда: при сбое воркера —
//! text/plain через тот же воркер или сообщение об ошибке.

use std::sync::mpsc;
use std::time::Duration;

use jiff::tz::TimeZone;
use postman_cache::{Addr, AttachmentRow, BodyRow, Cache};
use postman_calendar::Invitation;
use postman_ir::{Align, Block, Document, Inline};
use std::sync::Arc;

use postman_worker::mime::Mime;
use postman_worker::{Client, Level, Request, Rgba};

use crate::model::LetterId;
use crate::store;

const TIMEOUT: Duration = Duration::from_secs(5);

/// Наибольшая сторона картинки в области чтения.
pub const MAX_IMAGE_SIDE: u32 = 1600;

pub enum Rendered {
    Body {
        id: String,
        doc: Document,
        level: Level,
    },
    Image {
        key: String,
        result: Result<Rgba, String>,
    },
    /// Тело, собранное воркером из письма целиком ([`Renderer::request_raw`]), — производное содержимого: его кладут
    /// в хранилище (`put_body`), сеть не нужна. Следом — `Body` того же письма.
    Parsed {
        id: String,
        body: Result<Box<BodyRow>, String>,
    },
    /// Приглашение из вложения: `key` — письмо, к которому оно относится.
    Invite {
        key: String,
        result: Result<Box<Invitation>, String>,
    },
}

/// Откуда тело: разобранное или письмо целиком.
enum Source {
    Row(Box<BodyRow>),
    Raw(Arc<Vec<u8>>),
}

enum Job {
    Body(String, Source),
    Image(String, Arc<Vec<u8>>),
    Calendar(String, Arc<Vec<u8>>),
}

pub struct Renderer {
    tx: mpsc::Sender<Job>,
    pub rx: mpsc::Receiver<Rendered>,
}

impl Renderer {
    pub fn spawn(repaint: impl Fn() + Send + 'static) -> Self {
        let (tx, jobs) = mpsc::channel::<Job>();
        let (done, rx) = mpsc::channel();
        // Поток не запустился — письма показываются без разбора (заглушкой ожидания), без падения.
        // В журнал — с S2-06 (pets-shell); пока stderr.
        let started = std::thread::Builder::new().name("render".into()).spawn(move || {
            let mut client = Client::new(Client::default_exe(), TIMEOUT);
            // Изоляция не полная — в журнал один раз, с причиной (отказ AppContainer).
            let mut warned = false;
            while let Ok(first) = jobs.recv() {
                // При быстром листании важно только последнее тело; картинки — все по порядку.
                let mut body = None;
                let mut images = Vec::new();
                let mut calendars = Vec::new();
                for job in std::iter::once(first).chain(jobs.try_iter()) {
                    match job {
                        Job::Body(id, b) => body = Some((id, b)),
                        Job::Image(key, bytes) => images.push((key, bytes)),
                        Job::Calendar(key, bytes) => calendars.push((key, bytes)),
                    }
                }
                let mut out = Vec::new();
                if let Some((id, source)) = body {
                    let doc = match source {
                        Source::Row(b) => render(&mut client, &b),
                        Source::Raw(raw) => {
                            let parsed = client.mime(&raw).map(|m| Box::new(body_of(m, &id)));
                            let doc = match &parsed {
                                Ok(b) => render(&mut client, b),
                                Err(e) => failed(e),
                            };
                            out.push(Rendered::Parsed { id: id.clone(), body: parsed });
                            doc
                        }
                    };
                    out.push(Rendered::Body { id, doc, level: client.level() });
                    if client.level() == postman_worker::Level::Partial && !warned {
                        warned = true;
                        tracing::warn!(reason = client.fallback().unwrap_or("-"), "worker sandbox is partial");
                    }
                }
                for (key, bytes) in images {
                    let result = client.decode_image(&bytes, MAX_IMAGE_SIDE);
                    out.push(Rendered::Image { key, result });
                }
                for (key, bytes) in calendars {
                    let result = client.parse_calendar(&bytes).and_then(|v| Invitation::from_worker(&v, &TimeZone::system()).map(Box::new));
                    out.push(Rendered::Invite { key, result });
                }
                for r in out {
                    if done.send(r).is_err() {
                        return;
                    }
                }
                repaint();
            }
        });
        if let Err(e) = started {
            eprintln!("поток рендера не запустился: {e}");
        }
        Self { tx, rx }
    }

    pub fn request(&self, id: String, body: BodyRow) {
        let _ = self.tx.send(Job::Body(id, Source::Row(Box::new(body))));
    }

    /// Тело из письма целиком (`content` хранилища, ADR 0006 п. 2) — без сети: разбор в воркере → `Parsed`, затем
    /// `Body`.
    pub fn request_raw(&self, id: String, raw: Arc<Vec<u8>>) {
        let _ = self.tx.send(Job::Body(id, Source::Raw(raw)));
    }

    /// Письмо формата 6 без сети (S8-09): тело — как есть, нет тела — из письма целиком (`Parsed` кладут в хранилище
    /// под ключом `id`). Ответы — с `id` = [`store::letter_ref`]. `false` — ни тела, ни содержимого: нужен сервер.
    pub fn request_letter(&self, cache: &Cache, letter: LetterId) -> postman_cache::Result<bool> {
        let id = store::letter_ref(letter);
        match store::source(cache, letter)? {
            store::Source::Body(b) => self.request(id, *b),
            store::Source::Raw(raw) => self.request_raw(id, raw),
            store::Source::Missing => return Ok(false),
        }
        Ok(true)
    }

    pub fn decode(&self, key: String, bytes: Arc<Vec<u8>>) {
        let _ = self.tx.send(Job::Image(key, bytes));
    }

    /// Разобрать `.ics` в песочнице.
    pub fn invitation(&self, key: String, bytes: Arc<Vec<u8>>) {
        let _ = self.tx.send(Job::Calendar(key, bytes));
    }
}

fn render(client: &mut Client, body: &BodyRow) -> Document {
    let html_err = match &body.html {
        Some(h) => match client.render(&Request::Html(h.clone())) {
            Ok(doc) => return doc,
            Err(e) => Some(e),
        },
        None => None,
    };
    let text_err = match &body.text {
        Some(t) => match client.render(&Request::Text(t.clone())) {
            Ok(mut doc) => {
                if let Some(e) = html_err {
                    doc.prepend(notice(&format!("HTML-версия не показана ({e}); показан текст.")));
                }
                return doc;
            }
            Err(e) => Some(e),
        },
        None => None,
    };
    match html_err.or(text_err) {
        Some(e) => failed(&e),
        None => Document::default(),
    }
}

fn failed(e: &str) -> Document {
    Document { blocks: vec![notice(&format!("Не удалось безопасно показать письмо: {e}"))], ..Default::default() }
}

/// Письмо, разобранное воркером ([`Client::mime`]), → тело хранилища. Вложения — по номеру в письме (`<key>#<n>`),
/// письмо целиком — `key`: так их читают пути IMAP и EWS.
pub fn body_of(m: Mime, key: &str) -> BodyRow {
    let addrs = |v: Vec<postman_worker::mime::Addr>| v.into_iter().map(|a| Addr { name: a.name, email: a.email }).collect();
    BodyRow {
        to: addrs(m.to),
        cc: addrs(m.cc),
        reply_to: addrs(m.reply_to),
        message_id: m.message_id,
        references: m.references,
        html: m.html,
        text: m.text,
        attachments: m
            .parts
            .into_iter()
            .map(|p| AttachmentRow { blob_id: format!("{key}#{}", p.index), name: p.name, mime: p.mime, size: p.size, cid: p.cid, inline: p.inline })
            .collect(),
        blob_id: Some(key.to_owned()),
    }
}

fn notice(text: &str) -> Block {
    Block::Paragraph { align: Align::Start, content: vec![Inline::text(format!("⚠ {text}"))] }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Тело из письма целиком (S8-09): поля и вложения по номеру; разбор — тем же кодом, что в воркере.
    #[test]
    fn body_from_content() {
        let raw = b"From: a@x.ru\r\nTo: =?utf-8?B?0JjQstCw0L0=?= <i@x.ru>\r\nCc: c@x.ru\r\nMessage-ID: <m1@x.ru>\r\n\
References: <r1@x.ru>\r\nSubject: s\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
hi\r\n--b\r\nContent-Type: application/pdf\r\nContent-Disposition: attachment; filename=a.pdf\r\n\r\n%PDF\r\n--b--\r\n";
        let body = body_of(postman_worker::mime::parse(raw).unwrap(), "L7");
        assert_eq!(body.to, [Addr { name: "Иван".into(), email: "i@x.ru".into() }]);
        assert_eq!(body.cc.len(), 1);
        assert_eq!((body.message_id.as_deref(), body.references.as_slice()), (Some("m1@x.ru"), &["r1@x.ru".to_string()][..]));
        assert_eq!(body.text.as_deref().map(str::trim), Some("hi"));
        assert_eq!(body.blob_id.as_deref(), Some("L7"));
        let a = &body.attachments[0];
        assert_eq!((a.blob_id.as_str(), a.name.as_str(), a.mime.as_str(), a.inline), ("L7#0", "a.pdf", "application/pdf", false));
    }
}
