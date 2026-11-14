//! Наполнение поискового индекса. Ошибки индекса не мешают почте:
//! поиск тогда откатывается на простой поиск по кэшу.
//!
//! Письма — документами по `LetterId` ([`store::letter_ref`], S8-09): id не меняется при переносе между папками и
//! учётками, письмо находится и после удаления у источника. Индекс собирает [`rebuild`] в фоне (после переноса
//! формата 5, с пустой базой, со стёртым индексом); до готовности [`search`] ищет по кэшу. Письма, пришедшие в базу мимо
//! [`rows`] (пути IMAP и EWS), [`search`] догоняет сам, а документы писем, которых в базе больше нет, убирает, встретив
//! в выдаче: удаляют письма многие пути, и ни один из них индекс не трогает.
//!
//! Вложения (T3-10): имена — из тела, текст — от воркера ([`crate::extract`]); [`search_hits`] называет вложение, в
//! котором совпало. Формат базы тот же: схема индекса сменилась — он пересоздаётся пустым и собирается в фоне заново.
//!
//! Индекс общий с каналами бесед (ADR 0012 п. 10): у письма — канал `mail`, адрес беседы `postman://mail/<ящик>/…`
//! (`crate::channel::mail::conversation_of`; ящик ещё не известен — пусто) и адрес «От» в полях фильтров, поиск писем
//! — только среди них ([`search_in`] — с `беседа:`); сообщения каналов пишет и ищет `crate::channel::index`,
//! пересборка — вместе ([`rebuild`]). Адреса ящиков стали известны или сменились — документы писем без них:
//! [`needs_rebuild`], индекс пересобирается в фоне при следующем запуске (до готовности — поиск по кэшу, как всегда).

use std::collections::{HashMap, HashSet};

use postman_cache::{BodyRow, Cache, EmailRow, LetterRow};
pub use postman_search::AttachmentHit;
use postman_search::{Attachment, Doc, Scope, SearchIndex};

use crate::model::{FolderId, LetterId};
use crate::store::{self, cache_id};

/// Индекс по `LetterId` собран ([`rebuild`]): состояние в хранилище — номер письма, с которого индекс базу ещё не видел
/// (номера только растут). Нет числа — индекс не готов.
const READY: &str = "search.letters";
/// Писем за одно чтение и один коммит при пересборке.
const PAGE: usize = 500;
/// Индекс собирался при этих адресах ящиков: отметку ставит [`rebuild`], снимает [`stale`] (адрес ящика сменился).
const CONVS: &str = "search.letters.conv";

/// Адреса бесед писем ([`crate::channel::mail::conversation_of`]) с запоминанием адреса ящика на проход.
struct Convs<'a> {
    cache: &'a Cache,
    boxes: HashMap<Option<String>, Option<String>>,
}

impl<'a> Convs<'a> {
    fn new(cache: &'a Cache) -> Self {
        Self { cache, boxes: HashMap::new() }
    }

    /// Адрес беседы цепочки `thread_key`; ящик неизвестен — пусто.
    fn of(&mut self, thread_key: &str) -> String {
        let scope = postman_cache::account_of(thread_key);
        let cache = self.cache;
        let mailbox = self.boxes.entry(scope.map(str::to_owned)).or_insert_with(|| crate::channel::mail::mailbox(cache, scope));
        mailbox.as_deref().and_then(|m| crate::channel::mail::conversation_in(m, thread_key)).map(|a| a.to_string()).unwrap_or_default()
    }
}

/// Строки писем с сервера: тема, отправитель, превью. Письма, которого нет в базе, — нет и в индексе.
pub fn rows(index: &SearchIndex, cache: &Cache, rows: &[EmailRow]) {
    let ids: Vec<Option<String>> = rows.iter().map(|r| store::letter_of_key(cache, &r.id).ok().flatten().map(store::letter_ref)).collect();
    let people: Vec<String> = rows.iter().map(|r| people(&r.from_name, &r.from_addr, None)).collect();
    let authors: Vec<[String; 1]> = rows.iter().map(|r| [r.from_addr.trim().to_lowercase()]).collect();
    let mut convs = Convs::new(cache);
    let conv: Vec<String> = rows.iter().map(|r| convs.of(&r.thread_id)).collect();
    let docs: Vec<Doc> = rows
        .iter()
        .zip(&ids)
        .zip(&people)
        .zip(&authors)
        .zip(&conv)
        .filter_map(|((((r, id), p), a), c)| {
            Some(Doc {
                id: id.as_deref()?,
                subject: &r.subject,
                people: p,
                body: &r.preview,
                attachments: &[],
                received_at: r.received_at,
                channel: crate::channel::MAIL,
                conv: c,
                authors: a,
            })
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

/// Письмо целиком, когда известен его текст: получатели, тело, вложения — имена и текст (`attached` — по порядку
/// вложений тела, от воркера; нет текста — пусто или короче), адрес беседы — по ящику из хранилища. Коммит — пакетно,
/// у вызывающего.
pub fn letter(index: &SearchIndex, cache: &Cache, id: LetterId, row: &LetterRow, body: &BodyRow, text: &str, attached: &[String]) {
    put_letter(index, &mut Convs::new(cache), id, row, body, text, attached);
}

fn put_letter(index: &SearchIndex, convs: &mut Convs<'_>, id: LetterId, row: &LetterRow, body: &BodyRow, text: &str, attached: &[String]) {
    let (key, people) = (store::letter_ref(id), people(&row.from_name, &row.from_addr, Some(body)));
    let attachments: Vec<Attachment> =
        body.attachments.iter().enumerate().map(|(i, a)| Attachment { name: &a.name, text: attached.get(i).map_or("", String::as_str) }).collect();
    let authors = [row.from_addr.trim().to_lowercase()];
    let conv = convs.of(&row.thread_id);
    let doc = Doc {
        id: &key,
        subject: &row.subject,
        people: &people,
        body: text,
        attachments: &attachments,
        received_at: row.received_at,
        channel: crate::channel::MAIL,
        conv: &conv,
        authors: &authors,
    };
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

/// Индекс надо собрать заново (в фоне, при запуске): не собран или собирался без нынешних адресов ящиков — у
/// документов писем нет адреса беседы ([`stale`]). Пока не собран — поиск по тому, что есть ([`ready`]).
pub fn needs_rebuild(cache: &Cache) -> bool {
    !ready(cache) || !matches!(cache.state(CONVS), Ok(Some(_)))
}

/// Адрес ящика стал известен или сменился (`crate::channel::mail::remember_senders`): при следующем запуске
/// [`needs_rebuild`]. Сейчас ничего не пересобирается — новые и изменённые письма получают адрес беседы сразу.
pub fn stale(cache: &Cache) {
    if let Err(e) = cache.remove_state(CONVS) {
        tracing::warn!("search: rebuild mark not cleared: {e}");
    }
}

/// Письма базы с номера `from`, которых нет в индексе, — в индекс: строки и тела, которые уже есть (текст; HTML — потом
/// извлечением). Ответ — номер, с которого индекс базу ещё не видел; `None` — `stop`.
fn walk(index: &SearchIndex, cache: &Cache, from: u64, stop: &dyn Fn() -> bool) -> postman_cache::Result<Option<u64>> {
    let mut next = from;
    let mut convs = Convs::new(cache);
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
            put_letter(index, &mut convs, id, row, &body, text, &[]);
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
    // До прохода: адрес ящика, ставший известным во время сборки, снимет отметку — соберётся ещё раз.
    cache.set_state(CONVS, "")?;
    if index.clear().is_ok() {
        let _ = index.commit();
    }
    let Some(next) = walk(index, cache, 0, stop)? else { return Ok(false) };
    // Сообщения каналов — тем же проходом: очистка выше убрала и их документы.
    match crate::channel::index::rebuild(index, cache, stop) {
        Ok(true) => {}
        Ok(false) => return Ok(false),
        Err(e) => tracing::warn!("search: channel messages not indexed: {e}"),
    }
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

/// Найденное письмо.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub id: LetterId,
    /// Совпало во вложении: его имя и отрывок текста (T3-10). Поиск по кэшу, пока индекс не собран, вложений не знает.
    pub matched_in: Option<AttachmentHit>,
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
    Ok(search_hits(index, cache, query, folder, limit)?.into_iter().map(|h| h.id).collect())
}

/// [`search`] — и где совпало ([`Hit::matched_in`]).
pub fn search_hits(
    index: Option<&SearchIndex>,
    cache: &Cache,
    query: &str,
    folder: Option<FolderId>,
    limit: usize,
) -> postman_cache::Result<Vec<Hit>> {
    search_in(index, cache, query, &Scope::default(), folder, limit)
}

/// [`search_hits`] с фильтром ADR 0012 п. 10: `scope.convs` — `беседа:` (адреса `postman://mail/…`, со связанными —
/// `crate::channel::index::scope_of`), `scope.authors` — `от:`; канал — всегда `mail`. Пустые слова при фильтре —
/// письма фильтра, новые сверху.
pub fn search_in(
    index: Option<&SearchIndex>,
    cache: &Cache,
    query: &str,
    scope: &Scope,
    folder: Option<FolderId>,
    limit: usize,
) -> postman_cache::Result<Vec<Hit>> {
    let in_folder = |row: &LetterRow| folder.is_none_or(|f| row.folders.contains(&postman_cache::FolderId(f.0)));
    let mail = Scope { channels: vec![crate::channel::MAIL.to_owned()], ..scope.clone() };
    if let Some(index) = index.filter(|_| ready(cache)) {
        if let Err(e) = catch_up(index, cache) {
            tracing::warn!("search: index not updated, searching what is indexed: {e}");
        }
        if let Ok(hits) = index.hits_in(query, &mail, limit.saturating_mul(4).max(limit)) {
            let (mut out, mut gone) = (Vec::with_capacity(limit.min(hits.len())), Vec::new());
            for (id, matched_in) in hits.into_iter().filter_map(|h| Some((store::parse_ref(&h.id)?, h.attachment))) {
                match cache.letter(cache_id(id))? {
                    Some(row) if in_folder(&row) => out.push(Hit { id, matched_in }),
                    Some(_) => {}
                    None => gone.push(id),
                }
                if out.len() == limit {
                    break;
                }
            }
            if !gone.is_empty() {
                forget(index, &gone);
            }
            return Ok(out);
        }
    }
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let filtered = !scope.convs.is_empty() || !scope.authors.is_empty();
    if words.is_empty() && !filtered {
        return Ok(Vec::new());
    }
    let authors: Vec<String> = scope.authors.iter().map(|a| a.trim().to_lowercase()).collect();
    let mut convs = Convs::new(cache);
    let mut fits = |row: &LetterRow| {
        (authors.is_empty() || authors.contains(&row.from_addr.trim().to_lowercase()))
            && (scope.convs.is_empty() || scope.convs.contains(&convs.of(&row.thread_id)))
    };
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
            if words.iter().all(|w| hay.contains(w.as_str())) && fits(&row) {
                hits.push((row.received_at, LetterId(id.0)));
            }
        }
    }
    hits.sort_by_key(|h| std::cmp::Reverse(h.0));
    Ok(hits.into_iter().take(limit).map(|(_, id)| Hit { id, matched_in: None }).collect())
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
        super::letter(&index, &cache, indexed, &row, &BodyRow::default(), "квитанция", &[]);
        let _ = index.commit();
        let before = seen_up_to(&cache);
        let found = search(Some(&index), &cache, "телеграмма", None, 50).unwrap();
        assert!(found.contains(&late) && found.contains(&indexed), "{found:?}");
        assert!(seen_up_to(&cache) > before);
        assert_eq!(search(Some(&index), &cache, "квитанция", None, 50).unwrap(), [indexed]);
    }

    /// Письмо стёрто из базы мимо индекса: в выдаче его нет, документ поиск убирает.
    #[test]
    fn search_drops_letters_gone_from_the_store() {
        let (_dir, cache) = demo();
        let index = SearchIndex::in_memory().unwrap();
        assert!(rebuild(&index, &cache, &|| false).unwrap());
        let (letter, before) = (id(&cache, "demo7"), index.len());
        let subject = cache.letter(cache_id(letter)).unwrap().unwrap().subject;
        cache.delete_emails(&["demo7".to_owned()]).unwrap();
        assert!(!search(Some(&index), &cache, &subject, None, 500).unwrap().contains(&letter));
        assert_eq!(index.len(), before - 1);
    }

    /// Строки с сервера — документами по `LetterId`; документы по ключам кэша пересборка убирает.
    #[test]
    fn documents_by_letter_id() {
        let (_dir, cache) = demo();
        let index = SearchIndex::in_memory().unwrap();
        let row = cache.rows_by_ids(&["demo7".to_owned()]).unwrap().remove(0);
        let old =
            Doc { id: &row.id, subject: "Прежний документ", people: "", body: "", attachments: &[], received_at: 0, ..Doc::default() };
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

    /// T3-10: слова нашлись в тексте вложения — поиск называет вложение и отрывок; в теле — не называет; пока индекс
    /// не собран (поиск по кэшу) — тоже.
    #[test]
    fn search_tells_the_attachment() {
        let (_dir, cache) = demo();
        let index = SearchIndex::in_memory().unwrap();
        assert!(rebuild(&index, &cache, &|| false).unwrap());
        let inbox = cache.folder_of("", "inbox").unwrap().unwrap();
        let row = LetterRow { subject: "Опись бандероли".into(), folders: vec![inbox], ..LetterRow::default() };
        let id = LetterId(cache.upsert_remote("", "attached", &row).unwrap().0);
        let file = |name: &str| postman_cache::AttachmentRow { name: name.into(), ..Default::default() };
        let body = BodyRow { attachments: vec![file("logo.png"), file("Договор.docx")], ..BodyRow::default() };
        super::letter(
            &index,
            &cache,
            id,
            &row,
            &body,
            "Квитанция прилагается",
            &[String::new(), "Ставка аренды причала — 1200 рублей за метр".into()],
        );
        let _ = index.commit();
        let hits = search_hits(Some(&index), &cache, "причал", None, 50).unwrap();
        let hit = AttachmentHit {
            name: "Договор.docx".into(), snippet: "Ставка аренды причала — 1200 рублей за метр".into()
        };
        assert_eq!(hits, [Hit { id, matched_in: Some(hit) }]);
        assert_eq!(search_hits(Some(&index), &cache, "квитанция", None, 50).unwrap(), [Hit { id, matched_in: None }]);
        assert_eq!(search(Some(&index), &cache, "причал", None, 50).unwrap(), [id]);
        assert_eq!(search_hits(None, &cache, "бандероли", None, 50).unwrap(), [Hit { id, matched_in: None }]);
    }
}
