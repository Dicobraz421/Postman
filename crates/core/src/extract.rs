//! Фоновое извлечение текста писем для поиска. text/plain индексируется как есть
//! (это просто слова для токенизатора), HTML разбирается только в воркере-песочнице —
//! своём, чтобы не мешать области чтения.
//!
//! Текст вложений (T3-10) — тем же воркером, встроенным просмотром ([`postman_worker::Client::preview`]): текст,
//! документ, книга, таблица, презентация, письмо. Только вложения, чьи байты уже на этом компьютере (содержимое письма
//! в базе или скачанное вложение): ради поиска ничего не скачивается.

use std::sync::{Arc, mpsc};
use std::time::Duration;

use postman_cache::{AttachmentRow, BodyRow, Cache};
use postman_search::SearchIndex;
use postman_worker::preview::{Preview, SectionTitle};
use postman_worker::{Client, Request};

use crate::attach::{self, Viewer};

/// Коммит индекса не реже, чем через столько писем.
const COMMIT_EVERY: usize = 200;
/// Вложение крупнее — текст для поиска из него не достаём.
const MAX_ATTACHMENT: u64 = 16 * 1024 * 1024;

#[derive(Clone)]
pub struct Extractor {
    tx: mpsc::Sender<Vec<String>>,
}

impl Extractor {
    pub fn spawn(cache: Cache, index: Arc<SearchIndex>) -> Self {
        let (tx, rx) = mpsc::channel::<Vec<String>>();
        // Поток не запустился — очередь никто не читает, текст писем просто не индексируется.
        let started = std::thread::Builder::new().name("extract".into()).spawn(move || {
            let mut worker = Client::new(Client::default_exe(), Duration::from_secs(5));
            while let Ok(first) = rx.recv() {
                let ids: Vec<String> = std::iter::once(first).chain(rx.try_iter()).flatten().collect();
                for (n, id) in ids.iter().enumerate() {
                    extract_one(&cache, &index, &mut worker, id);
                    if (n + 1) % COMMIT_EVERY == 0 {
                        let _ = index.commit();
                    }
                }
                let _ = index.commit();
            }
        });
        if let Err(e) = started {
            tracing::error!("extract: text extraction thread not started: {e}");
        }
        Self { tx }
    }

    pub fn queue(&self, ids: Vec<String>) {
        if !ids.is_empty() {
            let _ = self.tx.send(ids);
        }
    }
}

/// `id` — ключ тела в кэше: письма с сервера или ссылка `#<LetterId>` (тело из содержимого).
fn extract_one(cache: &Cache, index: &SearchIndex, worker: &mut Client, id: &str) {
    let Ok(Some(letter)) = crate::store::letter_of_key(cache, id) else { return };
    let (Ok(Some(body)), Ok(Some(row))) = (cache.body(id), cache.letter(crate::store::cache_id(letter))) else { return };
    let text = match (&body.text, &body.html) {
        (Some(t), _) => t.clone(),
        (None, Some(h)) => match worker.render(&Request::Html(h.clone())) {
            Ok(doc) => postman_ir::to_markdown(&doc),
            Err(_) => return,
        },
        (None, None) => String::new(),
    };
    // Ссылки письма (ADR 0012 п. 11): `postman://`, `mid:`, ссылки каналов — в `xref`, текстом тела (Markdown
    // разобранного HTML несёт и адреса ссылок).
    if let Err(e) = crate::channel::xref::index_letter(cache, &crate::channel::Links::installed(), letter, &text) {
        tracing::warn!("letter links not stored: {e}");
    }
    let attached = attachment_texts(cache, worker, &body);
    crate::indexer::letter(index, letter, &row, &body, &text, &attached);
}

/// Тексты вложений по порядку (пусто — текста нет): не больше, чем берёт индекс на письмо.
fn attachment_texts(cache: &Cache, worker: &mut Client, body: &BodyRow) -> Vec<String> {
    let mut left = postman_search::LETTER_TEXT;
    body.attachments
        .iter()
        .map(|a| {
            if left == 0 {
                return String::new();
            }
            let text = attachment_text(cache, worker, a).unwrap_or_default();
            left = left.saturating_sub(text.len().min(postman_search::ATTACHMENT_TEXT));
            text
        })
        .collect()
}

/// Текст вложения: байты — из содержимого письма или из скачанного, разбор — в воркере. `None` — текста у такого
/// файла нет, байт на этом компьютере нет или воркер не разобрал.
fn attachment_text(cache: &Cache, worker: &mut Client, a: &AttachmentRow) -> Option<String> {
    if a.size > MAX_ATTACHMENT {
        return None;
    }
    let kind = match attach::viewer(&a.name, &a.mime) {
        v @ (Viewer::Text | Viewer::Document | Viewer::Sheet | Viewer::Table | Viewer::Slides | Viewer::Letter) => v.kind()?,
        _ => return None,
    };
    let bytes = match crate::store::attachment(cache, worker, &a.blob_id) {
        Some(part) => part.ok()?,
        None => cache.blob(&a.blob_id).ok()??,
    };
    if bytes.len() as u64 > MAX_ATTACHMENT {
        return None;
    }
    let (preview, _) = worker.preview(kind, &bytes, 0, 0, 0).ok()?;
    Some(preview_text(preview))
}

/// Слова просмотра для индекса: текст как есть, документ — разделами (имя листа — тоже слова), письмо — тема и тело.
fn preview_text(preview: Preview) -> String {
    let text = |blocks: Vec<postman_ir::Block>| postman_ir::to_markdown(&postman_ir::Document { blocks, ..Default::default() });
    match preview {
        Preview::Text { text, .. } => text,
        Preview::Document { sections, .. } => {
            let parts = sections.into_iter().flat_map(|s| {
                let title = match s.title {
                    SectionTitle::Sheet(name) => Some(name),
                    SectionTitle::None | SectionTitle::Slide(_) => None,
                };
                title.into_iter().chain(std::iter::once(text(s.blocks)))
            });
            parts.collect::<Vec<_>>().join("\n")
        }
        Preview::Letter { subject, blocks, .. } => format!("{subject}\n{}", text(blocks)),
        Preview::Image { .. } | Preview::Pdf { .. } | Preview::Archive(_) | Preview::Office(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use postman_ir::{Block, Inline};
    use postman_worker::preview::Section;

    /// T3-10: из просмотра в индекс идут слова — текст, разделы документа с именами листов, тема и тело письма.
    #[test]
    fn preview_text_for_the_index() {
        let para = |s: &str| Block::Paragraph { align: Default::default(), content: vec![Inline::text(s)] };
        assert_eq!(preview_text(Preview::Text { text: "Смета".into(), truncated: false }), "Смета");
        let sheets = vec![
            Section { title: SectionTitle::Sheet("Итоги".into()), blocks: vec![para("Аренда склада")] },
            Section { title: SectionTitle::Slide(2), blocks: vec![para("Выводы")] },
        ];
        let text = preview_text(Preview::Document { sections: sheets, truncated: false });
        assert!(text.contains("Итоги") && text.contains("Аренда склада") && text.contains("Выводы"), "{text}");
        let letter = Preview::Letter {
            from: vec![],
            to: vec![],
            cc: vec![],
            date: None,
            subject: "Счёт".into(),
            blocks: vec![para("Оплатить до пятницы")],
            attachments: vec![],
            truncated: false,
        };
        let text = preview_text(letter);
        assert!(text.contains("Счёт") && text.contains("Оплатить до пятницы"), "{text}");
    }
}
