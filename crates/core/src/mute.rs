//! «Беседа: заглушить / вернуть звук» (CMF-01, `docs/decisions/2026-11-14-messenger-comforts.md`): у заглушённой
//! беседы новые письма не дают уведомлений и звука и не считаются в непрочитанных папок, трея, Dock и строки меню; в
//! списке беседа остаётся (значок «без звука»). Это не «Игнорировать» Outlook — оно удаляет беседу.
//!
//! Признак — ключевое слово [`MUTED`] у каждого письма беседы: синхронизируется сервером, его видят другие клиенты.
//! Новое письмо беседы наследует признак ([`inherits`], проверка новых писем — `crate::newmail`). Без gpui и сети.

use postman_cache::{Cache, EmailRow, MailboxRow, Patch};

/// `$muted` — ключевое слово из реестра IANA «IMAP and JMAP Keywords» (draft-ietf-mailmaint-messageflag-mailboxattribute):
/// пользователь не хочет знать о новых письмах беседы. В RFC 9051 такого нет; своё слово не нужно.
pub const MUTED: &str = "$muted";

/// Письмо заглушённой беседы.
pub fn is_muted(row: &EmailRow) -> bool {
    row.has_keyword(MUTED)
}

/// Беседа заглушена: признак хоть у одного письма (другой клиент мог поставить его не всем).
pub fn thread_muted(thread: &[EmailRow]) -> bool {
    thread.iter().any(is_muted)
}

/// Правки: поставить (`on`) или снять признак у писем беседы — только у тех, где он другой.
pub fn patches(thread: &[EmailRow], on: bool) -> Vec<Patch> {
    thread
        .iter()
        .filter(|r| is_muted(r) != on)
        .map(|r| Patch { id: r.id.clone(), keywords: vec![(MUTED.to_owned(), on)], ..Patch::default() })
        .collect()
}

/// Команда над беседами выбранных писем: есть незаглушённая — заглушить все, иначе вернуть звук. Что сделано и правки.
pub fn toggle(threads: &[Vec<EmailRow>]) -> (bool, Vec<Patch>) {
    let on = threads.iter().any(|t| !thread_muted(t));
    (on, threads.iter().flat_map(|t| patches(t, on)).collect())
}

/// Новое письмо наследует признак: другие письма его беседы заглушены, а у него признака ещё нет.
pub fn inherits(row: &EmailRow, thread: &[EmailRow]) -> bool {
    !is_muted(row) && thread.iter().any(|r| r.id != row.id && is_muted(r))
}

/// Непрочитанных писем заглушённых бесед в папке — по индексу ключевых слов кэша (без обхода папки).
pub fn muted_unread(cache: &Cache, mailbox: &str) -> postman_cache::Result<u32> {
    if cache.keyword_count(MUTED, mailbox)? == 0 {
        return Ok(0);
    }
    let rows = cache.keyword_page(MUTED, mailbox, 0, usize::MAX)?;
    Ok(u32::try_from(rows.iter().filter(|r| !r.seen).count()).unwrap_or(u32::MAX))
}

/// Непрочитанные папок без заглушённых бесед: число сервера (`Mailbox.unreadEmails`) минус их непрочитанные в кэше.
/// Ошибка кэша — число сервера (лишняя подсветка лучше пропавшей).
pub fn without_muted(cache: &Cache, rows: &mut [MailboxRow]) {
    for m in rows.iter_mut().filter(|m| m.unread > 0) {
        match muted_unread(cache, &m.id) {
            Ok(n) => m.unread = m.unread.saturating_sub(n),
            Err(e) => tracing::warn!("mute: muted unread not counted, server count kept: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, thread: &str, seen: bool) -> EmailRow {
        EmailRow {
            id: id.into(),
            thread_id: thread.into(),
            mailbox_ids: vec!["in".into()],
            received_at: 0,
            subject: String::new(),
            from_name: String::new(),
            from_addr: "a@corp.example".into(),
            preview: String::new(),
            seen,
            flagged: false,
            has_attachment: false,
            size: 0,
            keywords: Vec::new(),
        }
    }

    fn muted(mut r: EmailRow) -> EmailRow {
        r.set_keyword(MUTED, true);
        r
    }

    /// Заглушить — всем письмам беседы; второй раз — вернуть звук; новое письмо наследует признак.
    #[test]
    fn mute_covers_the_thread_and_new_letters_inherit() {
        let quiet = vec![row("a", "t", true), row("b", "t", true)];
        let (on, p) = toggle(std::slice::from_ref(&quiet));
        assert!(on);
        assert_eq!(p.iter().map(|p| p.keywords.clone()).collect::<Vec<_>>(), vec![vec![(MUTED.to_string(), true)]; 2], "всем письмам беседы");
        // Признак хоть у одного письма (поставил другой клиент) — беседа заглушена: команда возвращает звук всем.
        let partly = vec![row("a", "t", true), muted(row("b", "t", true))];
        let (on, p) = toggle(std::slice::from_ref(&partly));
        assert!(!on);
        assert_eq!(p, [Patch { id: "b".into(), keywords: vec![(MUTED.into(), false)], ..Patch::default() }]);
        // Две беседы, одна не заглушена — заглушить обе.
        let (on, p) = toggle(&[partly.clone(), vec![row("c", "u", true)]]);
        assert!(on);
        assert_eq!(p.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["a", "c"]);
        let all: Vec<EmailRow> = partly.into_iter().map(muted).collect();
        let fresh = row("c", "t", false);
        let mut thread = all.clone();
        thread.push(fresh.clone());
        assert!(inherits(&fresh, &thread));
        assert!(!inherits(&muted(fresh), &thread), "признак уже есть");
        assert!(!inherits(&row("d", "u", false), &[row("d", "u", false), row("e", "u", true)]), "беседа не заглушена");
    }

    /// Непрочитанные заглушённых бесед не подсвечиваются: из числа сервера вычитаются.
    #[test]
    fn muted_letters_leave_the_unread_count() {
        let cache = Cache::open_in_memory().unwrap();
        cache.upsert_emails(&[muted(row("a", "t", false)), muted(row("b", "t", true)), row("c", "u", false)]).unwrap();
        assert_eq!(muted_unread(&cache, "in").unwrap(), 1);
        let mb = |id: &str, unread: u32| MailboxRow {
            id: id.into(),
            parent_id: None,
            name: id.into(),
            role: None,
            sort_order: 0,
            total: 3,
            unread,
            rights: Default::default(),
        };
        let mut rows = vec![mb("in", 2), mb("other", 4)];
        without_muted(&cache, &mut rows);
        assert_eq!(rows.iter().map(|m| m.unread).collect::<Vec<_>>(), [1, 4]);
    }
}
