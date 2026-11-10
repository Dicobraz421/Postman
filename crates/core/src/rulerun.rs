//! Правила по письмам кэша (T2-50): «Проверить на последних 100», «Применять к существующим — найдено N» и
//! правила «в приложении» по новым письмам «Входящих». Читает кэш (вызывать в фоне); правки писем делает
//! интерфейс тем же путём, что и кнопки над письмами (`bulk`).

use postman_cache::{Cache, EmailRow, Scan};

use crate::rules::{Action, Mail, Rule};

/// Сколько писем смотрит «Проверить на последних …».
pub const RECENT: usize = 100;
/// «Применять к существующим»: писем — не больше (дальше — по мере прихода).
pub const EXISTING: usize = 5000;

/// Письмо в виде, который проверяют правила.
fn check(rule: &Rule, row: &EmailRow, to: &[String], cc: &[String]) -> bool {
    rule.matches(&Mail { from_name: &row.from_name, from_addr: &row.from_addr, to, cc, subject: &row.subject })
}

/// Папки «Входящие» своего ящика (ключи кэша).
pub fn inboxes(cache: &Cache) -> postman_cache::Result<Vec<String>> {
    Ok(cache
        .mailboxes()?
        .into_iter()
        .filter(|m| m.role.as_deref() == Some("inbox") && postman_cache::account_of(&m.id).is_none())
        .map(|m| m.id)
        .collect())
}

/// Подходящие правилу письма среди последних `limit` писем «Входящих» (новые первыми) и сколько писем просмотрено.
pub fn matching(cache: &Cache, rule: &Rule, limit: usize) -> postman_cache::Result<(Vec<EmailRow>, usize)> {
    let mut out = Vec::new();
    let mut seen = 0;
    for inbox in inboxes(cache)? {
        cache.scan(&Scan::Mailbox(inbox), |row, r| {
            if seen >= limit {
                return false;
            }
            seen += 1;
            if check(rule, &row, &r.to, &r.cc) {
                out.push(row);
            }
            true
        })?;
    }
    out.sort_by_key(|r| std::cmp::Reverse(r.received_at));
    Ok((out, seen))
}

/// Сработавшее правило: его номер в списке и письмо.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub rule: usize,
    pub id: String,
    pub subject: String,
    pub received_at: i64,
}

/// Правила «в приложении» по письмам «Входящих», пришедшим после `since`: что сработало, по порядку правил, с
/// «Остановить обработку». Правило, которое уводит письмо из папки или удаляет его, тоже останавливает следующие.
pub fn fresh_hits(cache: &Cache, rules: &[Rule], since: i64) -> postman_cache::Result<Vec<Hit>> {
    let local: Vec<(usize, &Rule)> = rules.iter().enumerate().filter(|(_, r)| r.enabled && r.local).collect();
    let mut out = Vec::new();
    if local.is_empty() {
        return Ok(out);
    }
    for inbox in inboxes(cache)? {
        cache.scan(&Scan::Mailbox(inbox), |row, r| {
            if row.received_at <= since {
                return false;
            }
            for (i, rule) in &local {
                if !check(rule, &row, &r.to, &r.cc) {
                    continue;
                }
                out.push(Hit { rule: *i, id: row.id.clone(), subject: row.subject.clone(), received_at: row.received_at });
                let leaves = rule.actions.iter().any(|a| matches!(a, Action::Move { .. } | Action::Discard));
                if rule.stop || leaves {
                    break;
                }
            }
            true
        })?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{Cond, Field};
    use postman_cache::MailboxRow;

    fn row(id: &str, at: i64, from: &str, subject: &str) -> EmailRow {
        EmailRow {
            id: id.into(),
            thread_id: id.into(),
            mailbox_ids: vec!["in".into()],
            received_at: at,
            subject: subject.into(),
            from_name: String::new(),
            from_addr: from.into(),
            preview: String::new(),
            seen: false,
            flagged: false,
            has_attachment: false,
            size: 0,
            keywords: vec![],
        }
    }

    fn cache() -> Cache {
        let c = Cache::open_in_memory().unwrap();
        let inbox = MailboxRow {
            id: "in".into(),
            parent_id: None,
            name: "Inbox".into(),
            role: Some("inbox".into()),
            sort_order: 0,
            total: 0,
            unread: 0,
            rights: postman_cache::Rights::default(),
        };
        c.replace_mailboxes(None, &[inbox]).unwrap();
        c.upsert_emails(&[row("a", 10, "boss@corp", "Отчёт"), row("b", 20, "x@spam", "Розыгрыш"), row("c", 30, "boss@corp", "Счёт")]).unwrap();
        c
    }

    #[test]
    fn recent_matches_newest_first() {
        let c = cache();
        let rule = Rule::simple(true, Field::From, "boss", Action::Flag);
        let (hits, seen) = matching(&c, &rule, RECENT).unwrap();
        assert_eq!(hits.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["c", "a"]);
        assert_eq!(seen, 3);
        // Смотрит не больше `limit` последних.
        assert_eq!(matching(&c, &rule, 1).unwrap().0.len(), 1);
    }

    #[test]
    fn local_rules_on_fresh_mail_stop_and_order() {
        let c = cache();
        let local = |r: Rule| Rule { local: true, ..r };
        let rules = vec![
            Rule::simple(true, Field::From, "boss", Action::Flag), // на сервере — не наше
            local(Rule { stop: true, ..Rule::simple(true, Field::Subject, "Счёт", Action::MarkRead) }),
            local(Rule { when: vec![vec![Cond::contains(Field::From, "corp")]], ..Rule::simple(true, Field::From, "", Action::Flag) }),
        ];
        let hits = fresh_hits(&c, &rules, 15).unwrap();
        // «c» — второе правило со стопом; «a» — раньше отметки; «b» — не подходит.
        assert_eq!(hits.iter().map(|h| (h.rule, h.id.as_str())).collect::<Vec<_>>(), [(1, "c")]);
        let hits = fresh_hits(&c, &rules, 0).unwrap();
        assert_eq!(hits.iter().map(|h| (h.rule, h.id.as_str())).collect::<Vec<_>>(), [(1, "c"), (2, "a")]);
    }
}
