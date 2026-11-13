//! «Закрепить / открепить» (CMF-02, `docs/decisions/2026-11-14-messenger-comforts.md`): закреплённые письма и беседы
//! стоят вверху папки группой «Закреплённые» над датами, в любой сортировке (`crate::filters::arrange`). Признак —
//! ключевое слово [`PINNED`] (синхронизируется); в папке — не больше [`LIMIT`] закреплённых бесед. Без gpui и сети.

use std::collections::HashSet;

use postman_cache::{Cache, EmailRow, Patch};

/// Стандартного слова «закреплено» нет (RFC 9051, реестр IANA): слово Postman по схеме `pm.*` (`crate::marks`).
pub const PINNED: &str = "pm.pin";
/// Закреплённых бесед (письмо без беседы — своя беседа) в одной папке — не больше.
pub const LIMIT: usize = 20;

/// Письмо закреплено.
pub fn is_pinned(row: &EmailRow) -> bool {
    row.has_keyword(PINNED)
}

/// Закреплённые беседы папки (ключи бесед) — по индексу ключевых слов кэша.
pub fn pinned_threads(cache: &Cache, mailbox: &str) -> postman_cache::Result<HashSet<String>> {
    if cache.keyword_count(PINNED, mailbox)? == 0 {
        return Ok(HashSet::new());
    }
    Ok(cache.keyword_page(PINNED, mailbox, 0, usize::MAX)?.into_iter().map(|r| r.thread_id).collect())
}

/// В папке уже [`LIMIT`] закреплённых: новое не закрепить, пока не открепят другое.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Full;

/// Команда над письмами `rows` (в виде бесед — всеми письмами беседы в папке): есть незакреплённое — закрепить все,
/// иначе открепить. `pinned` — закреплённые беседы папки ([`pinned_threads`]). Что сделано и правки.
pub fn toggle(rows: &[EmailRow], pinned: &HashSet<String>) -> Result<(bool, Vec<Patch>), Full> {
    let on = rows.iter().any(|r| !is_pinned(r));
    if on {
        let mut after = pinned.clone();
        after.extend(rows.iter().map(|r| r.thread_id.clone()));
        if after.len() > LIMIT {
            return Err(Full);
        }
    }
    let patches = rows
        .iter()
        .filter(|r| is_pinned(r) != on)
        .map(|r| Patch { id: r.id.clone(), keywords: vec![(PINNED.to_owned(), on)], ..Patch::default() })
        .collect();
    Ok((on, patches))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, thread: &str, pinned: bool) -> EmailRow {
        let mut r = EmailRow {
            id: id.into(),
            thread_id: thread.into(),
            mailbox_ids: vec!["in".into()],
            received_at: 0,
            subject: String::new(),
            from_name: String::new(),
            from_addr: "a@corp.example".into(),
            preview: String::new(),
            seen: true,
            flagged: false,
            has_attachment: false,
            size: 0,
            keywords: Vec::new(),
        };
        r.set_keyword(PINNED, pinned);
        r
    }

    /// Закрепить, открепить, лимит бесед папки; письма уже закреплённой беседы лимит не трогают.
    #[test]
    fn pin_toggles_and_keeps_the_limit() {
        let (on, p) = toggle(&[row("a", "t", false), row("b", "t", true)], &HashSet::new()).unwrap();
        assert!(on);
        assert_eq!(p, [Patch { id: "a".into(), keywords: vec![(PINNED.into(), true)], ..Patch::default() }]);
        let (on, p) = toggle(&[row("a", "t", true)], &HashSet::from(["t".to_string()])).unwrap();
        assert!(!on);
        assert_eq!(p[0].keywords, [(PINNED.to_string(), false)]);
        let full: HashSet<String> = (0..LIMIT).map(|i| format!("t{i}")).collect();
        assert_eq!(toggle(&[row("x", "new", false)], &full), Err(Full), "двадцать первая беседа");
        assert!(toggle(&[row("x", "t3", false)], &full).is_ok(), "письмо закреплённой беседы");
        assert!(toggle(&[row("x", "t3", true)], &full).is_ok_and(|(on, _)| !on), "открепить можно всегда");
        let cache = Cache::open_in_memory().unwrap();
        cache.upsert_emails(&[row("a", "t", true), row("b", "t", true), row("c", "u", true), row("d", "v", false)]).unwrap();
        assert_eq!(pinned_threads(&cache, "in").unwrap(), HashSet::from(["t".to_string(), "u".to_string()]));
    }
}
