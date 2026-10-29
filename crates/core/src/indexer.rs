//! Наполнение поискового индекса. Ошибки индекса не мешают почте:
//! поиск тогда откатывается на простой поиск по кэшу.
//!
//! Письма формата 6 (S8-09) — документами по `LetterId` ([`store::letter_ref`]): id не меняется при переносе между
//! папками и учётками. После переноса формата 5 и с пустой базой индекс пересобирается в фоне ([`rebuild`]); до
//! готовности [`search`] ищет по кэшу. [`rows`] и [`full`] — документы по ключам кэша для интерфейса до его перехода на
//! `LetterId`.

use std::collections::HashSet;

use postman_cache::{BodyRow, Cache, EmailRow, LetterRow};
use postman_search::{Doc, SearchIndex};

use crate::model::{FolderId, LetterId};
use crate::store::{self, cache_id};

/// Индекс по `LetterId` собран ([`rebuild`]): состояние в хранилище. После переноса формата 5 и в пустой базе его нет.
const READY: &str = "search.letters";
/// Писем за одно чтение и один коммит при пересборке.
const PAGE: usize = 500;

/// Строки списка: тема, отправитель, превью.
pub fn rows(index: &SearchIndex, rows: &[EmailRow]) {
    let people: Vec<String> = rows.iter().map(|r| format!("{} {}", r.from_name, r.from_addr)).collect();
    let docs: Vec<Doc> = rows
        .iter()
        .zip(&people)
        .map(|(r, p)| Doc { id: &r.id, subject: &r.subject, people: p, body: &r.preview, attachments: "", received_at: r.received_at })
        .collect();
    if index.upsert(&docs).is_ok() {
        let _ = index.commit();
    }
}

/// Письмо целиком, когда известен его текст: получатели, тело, имена вложений.
pub fn full(index: &SearchIndex, row: &EmailRow, body: &BodyRow, text: &str) {
    let people = people(&row.from_name, &row.from_addr, Some(body));
    let attachments = attachments(body);
    let doc = Doc { id: &row.id, subject: &row.subject, people: &people, body: text, attachments: &attachments, received_at: row.received_at };
    // Коммит — пакетно, из UI-цикла (см. `App::commit_index`): открытие писем подряд не плодит сегменты.
    let _ = index.upsert(&[doc]);
}

fn people(name: &str, addr: &str, body: Option<&BodyRow>) -> String {
    let mut people = format!("{name} {addr}");
    for a in body.into_iter().flat_map(|b| b.to.iter().chain(&b.cc)) {
        people.push(' ');
        people.push_str(&a.name);
        people.push(' ');
        people.push_str(&a.email);
    }
    people
}

fn attachments(body: &BodyRow) -> String {
    body.attachments.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(" ")
}

/// Строки писем формата 6: тема, отправитель, превью.
pub fn letters(index: &SearchIndex, rows: &[(LetterId, LetterRow)]) {
    let ids: Vec<String> = rows.iter().map(|(id, _)| store::letter_ref(*id)).collect();
    let people: Vec<String> = rows.iter().map(|(_, r)| people(&r.from_name, &r.from_addr, None)).collect();
    let docs: Vec<Doc> = rows
        .iter()
        .zip(&ids)
        .zip(&people)
        .map(|(((_, r), id), p)| Doc { id, subject: &r.subject, people: p, body: &r.preview, attachments: "", received_at: r.received_at })
        .collect();
    if index.upsert(&docs).is_ok() {
        let _ = index.commit();
    }
}

/// Письмо формата 6 целиком, когда известен его текст. Коммит — пакетно, у вызывающего.
pub fn letter(index: &SearchIndex, id: LetterId, row: &LetterRow, body: &BodyRow, text: &str) {
    let (key, people, attachments) = (store::letter_ref(id), people(&row.from_name, &row.from_addr, Some(body)), attachments(body));
    let doc = Doc { id: &key, subject: &row.subject, people: &people, body: text, attachments: &attachments, received_at: row.received_at };
    let _ = index.upsert(&[doc]);
}

/// Письма удалены из базы — из индекса тоже.
pub fn forget(index: &SearchIndex, ids: &[LetterId]) {
    let keys: Vec<String> = ids.iter().map(|id| store::letter_ref(*id)).collect();
    if index.delete(&keys).is_ok() {
        let _ = index.commit();
    }
}

/// Индекс по `LetterId` собран: [`search`] ищет по нему.
pub fn ready(cache: &Cache) -> bool {
    cache.state(READY).ok().flatten().is_some()
}

/// Пересобрать индекс по письмам хранилища — в фоне (после переноса формата 5, как при первом запуске): строки и
/// тела, которые уже есть (текст; HTML — потом извлечением). `stop` — прервать: индекс останется не готов, следующая
/// пересборка — сначала. `true` — собран.
pub fn rebuild(index: &SearchIndex, cache: &Cache, stop: &dyn Fn() -> bool) -> postman_cache::Result<bool> {
    let mut seen = HashSet::new();
    for (folder, _) in cache.folders()? {
        let mut offset = 0;
        loop {
            if stop() {
                return Ok(false);
            }
            let page = cache.folder_page(folder, offset, PAGE)?;
            offset += page.len();
            for (id, row) in page.iter().filter(|(id, _)| seen.insert(id.0)) {
                let id = LetterId(id.0);
                let body = store::body(cache, id)?.unwrap_or_default();
                let text = body.text.as_deref().unwrap_or(&row.preview);
                letter(index, id, row, &body, text);
            }
            let _ = index.commit();
            if page.len() < PAGE {
                break;
            }
        }
    }
    cache.set_state(READY, "1")?;
    Ok(true)
}

/// Поиск писем формата 6, по релевантности: по индексу, если он собран ([`ready`]); иначе — по кэшу (тема,
/// отправитель, превью), новые сверху. `folder` — только письма этой папки.
pub fn search(
    index: Option<&SearchIndex>,
    cache: &Cache,
    query: &str,
    folder: Option<FolderId>,
    limit: usize,
) -> postman_cache::Result<Vec<LetterId>> {
    let in_folder = |row: &LetterRow| folder.is_none_or(|f| row.folders.contains(&postman_cache::FolderId(f.0)));
    if let Some(index) = index.filter(|_| ready(cache))
        && let Ok(hits) = index.search(query, limit.saturating_mul(4).max(limit))
    {
        let mut out = Vec::with_capacity(limit);
        for id in hits.iter().filter_map(|h| store::parse_ref(h)) {
            if cache.letter(cache_id(id))?.is_some_and(|r| in_folder(&r)) {
                out.push(id);
                if out.len() == limit {
                    break;
                }
            }
        }
        return Ok(out);
    }
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return Ok(Vec::new());
    }
    let folders = match folder {
        Some(f) => vec![postman_cache::FolderId(f.0)],
        None => cache.folders()?.into_iter().map(|(f, _)| f).collect(),
    };
    let mut seen = HashSet::new();
    let mut hits: Vec<(i64, LetterId)> = Vec::new();
    for f in folders {
        for (id, row) in cache.folder_page(f, 0, usize::MAX)? {
            if !seen.insert(id.0) {
                continue;
            }
            let hay = format!("{} {} {} {}", row.subject, row.from_name, row.from_addr, row.preview).to_lowercase();
            if words.iter().all(|w| hay.contains(w.as_str())) {
                hits.push((row.received_at, LetterId(id.0)));
            }
        }
    }
    hits.sort_by_key(|h| std::cmp::Reverse(h.0));
    Ok(hits.into_iter().take(limit).map(|(_, id)| id).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::{demo, id};

    /// Демо-ящик после переноса формата 5 (S8-09): индекса по `LetterId` нет — поиск по кэшу; пересборка в фоне — по
    /// индексу, те же письма; письмо, перенесённое в другую учётку, находится под тем же `LetterId`.
    #[test]
    fn search_after_migration() {
        let (_dir, cache) = demo();
        let index = SearchIndex::in_memory().unwrap();
        let letter = id(&cache, "demo7");
        let subject = cache.letter(cache_id(letter)).unwrap().unwrap().subject;
        assert!(!ready(&cache), "после переноса индекс не готов");
        let by_cache = search(Some(&index), &cache, &subject, None, 50).unwrap();
        assert!(by_cache.contains(&letter), "до готовности — по кэшу");
        assert!(rebuild(&index, &cache, &|| true).is_ok_and(|done| !done) && !ready(&cache), "прервана — не готов");
        assert!(rebuild(&index, &cache, &|| false).unwrap() && ready(&cache));
        let by_index = search(Some(&index), &cache, &subject, None, 50).unwrap();
        assert!(by_index.contains(&letter), "{by_index:?}");
        assert!(search(None, &cache, &subject, None, 50).unwrap().contains(&letter), "индекса нет — по кэшу");

        // Перенос в «Входящие» второй учётки: тот же `LetterId`, папка — новая.
        let inbox = cache.folder_of("", "inbox").unwrap().unwrap();
        let other = cache.folder_of("@2", "inbox").unwrap().unwrap();
        cache.move_letters(&[cache_id(letter)], other).unwrap();
        assert!(search(Some(&index), &cache, &subject, Some(FolderId(other.0)), 50).unwrap().contains(&letter));
        assert!(!search(Some(&index), &cache, &subject, Some(FolderId(inbox.0)), 50).unwrap().contains(&letter));
        forget(&index, &[letter]);
        assert!(!search(Some(&index), &cache, &subject, None, 50).unwrap().contains(&letter));
    }
}
