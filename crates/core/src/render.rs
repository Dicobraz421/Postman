//! Разбор тел писем в изолированном процессе (`postman-worker`).
//! В UI-процессе HTML не разбирается никогда: при сбое воркера —
//! text/plain через тот же воркер или сообщение об ошибке.

use std::sync::mpsc;
use std::time::Duration;

use jiff::tz::TimeZone;
use postman_cache::BodyRow;
use postman_calendar::Invitation;
use postman_ir::{Align, Block, Document, Inline};
use std::sync::Arc;

use postman_worker::{Client, Level, Request, Rgba};

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
    /// Приглашение из вложения: `key` — письмо, к которому оно относится.
    Invite {
        key: String,
        result: Result<Box<Invitation>, String>,
    },
}

enum Job {
    Body(String, Box<BodyRow>),
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
                if let Some((id, b)) = body {
                    let doc = render(&mut client, &b);
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
        let _ = self.tx.send(Job::Body(id, Box::new(body)));
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
        Some(e) => Document {
            blocks: vec![notice(&format!("Не удалось безопасно показать письмо: {e}"))], ..Default::default()
        },
        None => Document::default(),
    }
}

fn notice(text: &str) -> Block {
    Block::Paragraph { align: Align::Start, content: vec![Inline::text(format!("⚠ {text}"))] }
}
