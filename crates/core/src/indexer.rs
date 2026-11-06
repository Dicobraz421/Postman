//! Наполнение поискового индекса. Ошибки индекса не мешают почте:
//! поиск тогда откатывается на простой поиск по кэшу.
//!
//! Письма — документами по `LetterId` ([`store::letter_ref`], S8-09): id не меняется при переносе между папками и
//! учётками, письмо находится и после удаления у источника. Индекс собирает [`rebuild`] в фоне (после переноса
//! формата 5, с пустой базой, со стёртым индексом); до готовности [`search`] ищет по кэшу. Письма, пришедшие в базу мимо
//! [`rows`] (пути IMAP и EWS), [`search`] догоняет сам.

use std::collections::HashSet;

use postman_cache::{BodyRow, Cache, EmailRow, LetterRow};
use postman_search::{Doc, SearchIndex};

use crate::model::{FolderId, LetterId};
use crate::store::{self, cache_id};

/// Индекс по `LetterId` собран ([`rebuild`]): состояние в хранилище — номер письма, с которого индекс базу ещё не видел
/// (номера только растут). Нет числа — индекс не готов.
const READY: &str = "search.letters";
/// Писем за одно чтение и один коммит при пересборке.
const PAGE: usize = 500;

/// Строки писем с сервера: тема, отправитель, превью. Письма, которого нет в базе, — нет и в индексе.
pub fn rows(index: &SearchIndex, cache: &Cache, rows: &[EmailRow]) {
    let ids: Vec<Option<String>> = rows.iter().map(|r| store::letter_of_key(cache, &r.id).ok().flatten().map(store::letter_ref)).collect();
    let people: Vec<String> = rows.iter().map(|r| people(&r.from_name, &r.from_addr, None)).collect();
    let docs: Vec<Doc> = rows
        .iter()
        .zip(&ids)
        .zip(&people)
        .filter_map(|((r, id), p)| {
            Some(Doc { id: id.as_deref()?, subject: &r.subject, people: p, body: &r.preview, attachments: "", received_at: r.received_at })
        })
        .collect();
    if index.upsert(&docs).is_ok() {
        let _ = index.commit();
    }
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

/// Письмо целиком, когда известен его текст: получатели, тело, имена вложений. Коммит — пакетно, у вызывающего.
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

fn seen_up_to(cache: &Cache) -> Option<u64> {
    cache.state(READY).ok().flatten()?.parse().ok()
}

/// Индекс по `LetterId` собран: [`search`] ищет по нему.
pub fn ready(cache: &Cache) -> bool {
    seen_up_to(cache).is_some()
}

/// Письма базы с номера `from`, которых нет в индексе, — в индекс: строки и тела, которые уже есть (текст; HTML — потом
/// извлечением). Ответ — номер, с которого индекс базу ещё не видел; `None` — `stop`.
fn walk(index: &SearchIndex, cache: &Cache, from: u64, stop: &dyn Fn() -> bool) -> postman_cache::Result<Option<u64>> {
    let mut next = from;
    loop {
        if stop() {
            return Ok(None);
        }
        let page = cache.letters_from(postman_cache::LetterId(next), PAGE)?;
        for (id, row) in &page {
            next = id.0 + 1;
            let id = LetterId(id.0);
            if index.contains(&store::letter_ref(id)).unwrap_or(false) {
                continue;
            }
            let body = store::body(cache, id)?.unwrap_or_default();
            let text = body.text.as_deref().unwrap_or(&row.preview);
            letter(index, id, row, &body, text);
        }
        let _ = index.commit();
        if page.len() < PAGE {
            return Ok(Some(next));
        }
    }
}

/// Пересобрать индекс по письмам хранилища — в фоне (после переноса формата 5, как при первом запуске); прежнее
/// содержимое индекса не нужно. `stop` — прервать: индекс останется не готов, следующая пересборка — сначала. `true` —
/// собран.
pub fn rebuild(index: &SearchIndex, cache: &Cache, stop: &dyn Fn() -> bool) -> postman_cache::Result<bool> {
    if stop() {
        return Ok(false);
    }
    cache.set_state(READY, "")?;
    if index.clear().is_ok() {
        let _ = index.commit();
    }
    let Some(next) = walk(index, cache, 0, stop)? else { return Ok(false) };
    cache.set_state(READY, &next.to_string())?;
    Ok(true)
}

/// Письма, пришедшие в базу после прошлого поиска, — в индекс; что записала синхронизация — видно в поиске.
fn catch_up(index: &SearchIndex, cache: &Cache) -> postman_cache::Result<()> {
    let _ = index.commit();
    let Some(from) = seen_up_to(cache) else { return Ok(()) };
    if let Some(next) = walk(index, cache, from, &|| false)?
        && next != from
    {
        cache.set_state(READY, &next.to_string())?;
    }
    Ok(())
}

/// Поиск писем, по релевантности: по индексу, если он собран ([`ready`]); иначе — по кэшу (тема, отправитель, превью),
/// новые сверху. `folder` — только письма этой папки.
pub fn search(
    index: Option<&SearchIndex>,
    cache: &Cache,
    query: &str,
    folder: Option<FolderId>,
    limit: usize,
) -> postman_cache::Result<Vec<LetterId>> {
    let in_folder = |row: &LetterRow| folder.is_none_or(|f| row.folders.contains(&postman_cache::FolderId(f.0)));
    if let Some(index) = index.filter(|_| ready(cache)) {
        if let Err(e) = catch_up(index, cache) {
            tracing::warn!("search index: {e}");
        }
        if let Ok(hits) = index.search(query, limit.saturating_mul(4).max(limit)) {
            let mut out = Vec::with_capacity(limit.min(hits.len()));
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

    /// Решение 5 (`docs/manual-tests/OWNER.md`): письмо перенесено в «На этом компьютере», источник его удалил — связей
    /// с сервером нет, письмо находится: по индексу, без индекса и пока индекс не собран.
    #[test]
    fn moved_letter_found_after_source_delete() {
        let (_dir, cache) = demo();
        let index = SearchIndex::in_memory().unwrap();
        let letter = id(&cache, "demo7");
        let subject = cache.letter(cache_id(letter)).unwrap().unwrap().subject;
        assert!(rebuild(&index, &cache, &|| false).unwrap());
        let local = cache.create_folder(None, "Локальная").unwrap();
        cache.move_letters(&[cache_id(letter)], local).unwrap();
        cache.delete_remote("", &["demo7".to_owned()]).unwrap();
        assert!(cache.remotes(cache_id(letter)).unwrap().is_empty());
        assert!(search(Some(&index), &cache, &subject, None, 500).unwrap().contains(&letter), "индекс");
        assert_eq!(search(Some(&index), &cache, &subject, Some(FolderId(local.0)), 50).unwrap(), [letter], "папка");
        assert!(search(None, &cache, &subject, None, 500).unwrap().contains(&letter), "индекса нет");
        cache.set_state(READY, "").unwrap();
        assert!(!ready(&cache));
        assert!(search(Some(&index), &cache, &subject, None, 500).unwrap().contains(&letter), "не собран");
    }

    /// Письмо пришло в базу мимо [`rows`] (пути IMAP и EWS): поиск его догоняет, текст, уже извлечённый из тела, не
    /// затирается строкой.
    #[test]
    fn search_catches_up_with_the_store() {
        let (_dir, cache) = demo();
        let index = SearchIndex::in_memory().unwrap();
        assert!(rebuild(&index, &cache, &|| false).unwrap());
        let inbox = cache.folder_of("", "inbox").unwrap().unwrap();
        let row = LetterRow { subject: "Телеграмма запоздалая".into(), folders: vec![inbox], ..LetterRow::default() };
        let (late, indexed) = (cache.upsert_remote("", "late", &row).unwrap(), cache.upsert_remote("", "full", &row).unwrap());
        let (late, indexed) = (LetterId(late.0), LetterId(indexed.0));
        super::letter(&index, indexed, &row, &BodyRow::default(), "квитанция");
        let _ = index.commit();
        let before = seen_up_to(&cache);
        let found = search(Some(&index), &cache, "телеграмма", None, 50).unwrap();
        assert!(found.contains(&late) && found.contains(&indexed), "{found:?}");
        assert!(seen_up_to(&cache) > before);
        assert_eq!(search(Some(&index), &cache, "квитанция", None, 50).unwrap(), [indexed]);
    }

    /// Строки с сервера — документами по `LetterId`; документы по ключам кэша пересборка убирает.
    #[test]
    fn documents_by_letter_id() {
        let (_dir, cache) = demo();
        let index = SearchIndex::in_memory().unwrap();
        let row = cache.rows_by_ids(&["demo7".to_owned()]).unwrap().remove(0);
        let old = Doc { id: &row.id, subject: "Прежний документ", people: "", body: "", attachments: "", received_at: 0 };
        index.upsert(&[old]).unwrap();
        index.commit().unwrap();
        let letter = store::letter_ref(id(&cache, "demo7"));
        rows(&index, &cache, std::slice::from_ref(&row));
        assert!(index.search(&row.subject, 500).unwrap().contains(&letter));
        rows(&index, &cache, &[EmailRow { id: "нет такого".into(), ..row.clone() }]);
        assert!(rebuild(&index, &cache, &|| false).unwrap());
        assert!(index.search("прежний", 10).unwrap().is_empty());
        assert!(!index.contains(&row.id).unwrap() && index.contains(&letter).unwrap());
    }
}
