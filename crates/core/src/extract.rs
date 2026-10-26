//! Фоновое извлечение текста писем для поиска. text/plain индексируется как есть
//! (это просто слова для токенизатора), HTML разбирается только в воркере-песочнице —
//! своём, чтобы не мешать области чтения.

use std::sync::{Arc, mpsc};
use std::time::Duration;

use postman_cache::Cache;
use postman_search::SearchIndex;
use postman_worker::{Client, Request};

/// Коммит индекса не реже, чем через столько писем.
const COMMIT_EVERY: usize = 200;

#[derive(Clone)]
pub struct Extractor {
    tx: mpsc::Sender<Vec<String>>,
}

impl Extractor {
    pub fn spawn(cache: Cache, index: Arc<SearchIndex>) -> Self {
        let (tx, rx) = mpsc::channel::<Vec<String>>();
        // Поток не запустился — очередь никто не читает, текст писем просто не индексируется.
        // В журнал — с S2-06 (pets-shell); пока stderr.
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
            eprintln!("поток извлечения текста не запустился: {e}");
        }
        Self { tx }
    }

    pub fn queue(&self, ids: Vec<String>) {
        if !ids.is_empty() {
            let _ = self.tx.send(ids);
        }
    }
}

fn extract_one(cache: &Cache, index: &SearchIndex, worker: &mut Client, id: &str) {
    let (Ok(Some(body)), Ok(rows)) = (cache.body(id), cache.rows_by_ids(&[id.to_string()])) else { return };
    let Some(row) = rows.first() else { return };
    let text = match (&body.text, &body.html) {
        (Some(t), _) => t.clone(),
        (None, Some(h)) => match worker.render(&Request::Html(h.clone())) {
            Ok(doc) => postman_ir::to_markdown(&doc),
            Err(_) => return,
        },
        (None, None) => String::new(),
    };
    crate::indexer::full(index, row, &body, &text);
}
