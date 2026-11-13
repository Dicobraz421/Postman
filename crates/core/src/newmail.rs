//! Уведомления о новых письмах (T2-10, «Настройки → Общие → Уведомления»): что пришло во «Входящие» всех ящиков после
//! прошлой проверки и не прочитано; «важные» — упоминание меня, ответ в моей беседе (в ней есть моё письмо), письмо со
//! сроком (подсказка настройки). Тихие часы — уведомление не всплывает, остаётся в истории. Заглушённые беседы
//! (CMF-01, `crate::mute`) — без уведомлений, а новое письмо такой беседы получает признак. Без gpui и сети: читает кэш.

use postman_cache::{Cache, EmailRow};

use crate::filters::Me;
use crate::marks::{self, Kind};
use crate::mute;
use crate::prefs::Notify;

/// Писем «Входящих» на ящик за одну проверку — не больше (новые сверху; остальное ждёт следующей).
const PAGE: usize = 200;

/// Новое письмо для уведомления.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fresh {
    pub id: String,
    pub from: String,
    pub subject: String,
    pub received_at: i64,
    pub important: bool,
}

/// Важное ли письмо: меня упомянули, ответ в беседе с моим письмом (`mine_in_thread`), есть срок.
pub fn important(row: &EmailRow, me: &Me, mine_in_thread: bool) -> bool {
    marks::has(&row.keywords, Kind::Due) || mine_in_thread || me.mentioned(&row.subject) || me.mentioned(&row.preview)
}

/// Сейчас (`hour`, 0–23) тихие часы с `from` до `to` (через полночь — тоже: 22–8).
pub fn quiet(hour: u8, from: u8, to: u8) -> bool {
    match from.cmp(&to) {
        std::cmp::Ordering::Less => (from..to).contains(&hour),
        std::cmp::Ordering::Greater => hour >= from || hour < to,
        std::cmp::Ordering::Equal => false,
    }
}

/// Итог проверки: что показать, каким письмам заглушённых бесед поставить признак и самое новое из просмотренных.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Found {
    /// Новые непрочитанные, новые первыми; заглушённых бесед здесь нет.
    pub fresh: Vec<Fresh>,
    /// Новые письма заглушённых бесед без признака (`mute::inherits`): поставить [`mute::MUTED`].
    pub inherit: Vec<String>,
    /// Время самого нового просмотренного письма (и заглушённого): следующая проверка — после него.
    pub latest: i64,
}

/// Новые непрочитанные письма «Входящих» после `since` (Unix-секунды).
pub fn scan(cache: &Cache, since: i64, me: &Me) -> postman_cache::Result<Found> {
    let inboxes: Vec<String> = cache.mailboxes()?.into_iter().filter(|m| m.role.as_deref() == Some("inbox")).map(|m| m.id).collect();
    let mut found = Found { latest: since, ..Found::default() };
    for inbox in &inboxes {
        if cache.count_since(inbox, since.saturating_add(1))? == 0 {
            continue;
        }
        for row in cache.page(inbox, 0, PAGE)?.into_iter().filter(|r| r.received_at > since && !r.seen) {
            if found.fresh.iter().any(|f| f.id == row.id) || found.inherit.contains(&row.id) {
                continue;
            }
            found.latest = found.latest.max(row.received_at);
            let thread = cache.thread(&row.thread_id)?;
            if mute::is_muted(&row) || mute::thread_muted(&thread) {
                if mute::inherits(&row, &thread) {
                    found.inherit.push(row.id);
                }
                continue;
            }
            let mine = thread.iter().any(|r| r.id != row.id && me.addresses().iter().any(|a| r.from_addr.eq_ignore_ascii_case(a)));
            let from = if row.from_name.trim().is_empty() { row.from_addr.clone() } else { row.from_name.clone() };
            found.fresh.push(Fresh { important: important(&row, me, mine), id: row.id, from, subject: row.subject, received_at: row.received_at });
        }
    }
    found.fresh.sort_by_key(|f| std::cmp::Reverse(f.received_at));
    Ok(found)
}

/// Что из найденного показать по настройке: все, только важные или ничего.
pub fn shown(found: Vec<Fresh>, notify: Notify) -> Vec<Fresh> {
    match notify {
        Notify::All => found,
        Notify::Important => found.into_iter().filter(|f| f.important).collect(),
        Notify::Off => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_hours_wrap_midnight() {
        assert!(quiet(23, 22, 8) && quiet(3, 22, 8) && !quiet(8, 22, 8) && !quiet(12, 22, 8));
        assert!(quiet(13, 13, 14) && !quiet(14, 13, 14));
        assert!(!quiet(5, 6, 6), "с 6 до 6 — не тихие");
    }

    fn row(id: &str, received: i64, seen: bool) -> EmailRow {
        EmailRow {
            id: id.into(),
            thread_id: id.into(),
            mailbox_ids: vec!["in".into()],
            received_at: received,
            subject: format!("s-{id}"),
            from_name: "Анна".into(),
            from_addr: "anna@corp.example".into(),
            preview: String::new(),
            seen,
            flagged: false,
            has_attachment: false,
            size: 0,
            keywords: Vec::new(),
        }
    }

    /// Новое — только непрочитанное после отметки; важное — упоминание, срок, ответ в моей беседе.
    #[test]
    fn fresh_letters_since_the_mark() {
        let cache = Cache::open_in_memory().unwrap();
        let mb = |id: &str, role: Option<&str>| postman_cache::MailboxRow {
            id: id.into(),
            parent_id: None,
            name: id.into(),
            role: role.map(str::to_owned),
            sort_order: 0,
            total: 0,
            unread: 0,
            rights: postman_cache::Rights { read: true, add: true, remove: true, seen: true, keywords: true, submit: true },
        };
        cache.replace_mailboxes(None, &[mb("in", Some("inbox")), mb("arch", Some("archive"))]).unwrap();
        let mut mention = row("c", 300, false);
        mention.preview = "@Антон, посмотри".into();
        let mut reply = row("d", 400, false);
        reply.thread_id = "t".into();
        let mut mine = row("m", 50, true);
        mine.thread_id = "t".into();
        mine.from_addr = "me@corp.example".into();
        mine.mailbox_ids = vec!["arch".into()];
        cache.upsert_emails(&[row("a", 100, false), row("b", 250, true), mention, reply, mine]).unwrap();
        let me = Me::new(["me@corp.example"], ["Антон Антропов"]);
        let found = scan(&cache, 200, &me).unwrap().fresh;
        let ids: Vec<(&str, bool)> = found.iter().map(|f| (f.id.as_str(), f.important)).collect();
        assert_eq!(ids, [("d", true), ("c", true)], "старое и прочитанное — нет; новые первыми");
        let mut plain = row("e", 500, false);
        plain.subject = "Привет".into();
        assert!(!important(&plain, &me, false));
        assert_eq!(shown(scan(&cache, 200, &me).unwrap().fresh, Notify::Off), []);
    }

    /// CMF-01: письмо заглушённой беседы не уведомляет (и звука нет: уведомления нет), а признак наследует; отметка
    /// проверки идёт и за ним.
    #[test]
    fn muted_thread_does_not_notify_and_the_new_letter_inherits() {
        let cache = Cache::open_in_memory().unwrap();
        let inbox = postman_cache::MailboxRow {
            id: "in".into(),
            parent_id: None,
            name: "in".into(),
            role: Some("inbox".into()),
            sort_order: 0,
            total: 0,
            unread: 0,
            rights: Default::default(),
        };
        cache.replace_mailboxes(None, &[inbox]).unwrap();
        let mut old = row("old", 50, true);
        old.thread_id = "t".into();
        old.set_keyword(mute::MUTED, true);
        let mut new = row("new", 300, false);
        new.thread_id = "t".into();
        let mut marked = row("marked", 310, false);
        marked.thread_id = "u".into();
        marked.set_keyword(mute::MUTED, true);
        cache.upsert_emails(&[old, new, marked, row("plain", 250, false)]).unwrap();
        let me = Me::new(["me@corp.example"], ["Антон"]);
        let f = scan(&cache, 200, &me).unwrap();
        assert_eq!(f.fresh.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), ["plain"], "заглушённые — без уведомления");
        assert_eq!(f.inherit, ["new"], "новое письмо беседы наследует признак, у «marked» он уже есть");
        assert_eq!(f.latest, 310);
    }
}
