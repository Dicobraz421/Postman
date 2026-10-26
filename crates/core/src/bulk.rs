//! Массовые действия (S5-08, DESIGN «Действия и состояния»): выделенные письма — прочитано, флаг, категория,
//! в папку, архив, удаление, «Сообщить о фишинге». Решает, что поменять у каждого письма ([`plan`]): правки
//! ([`Patch`], как у жизненного цикла S5-04 — сразу в кэш и серверу одним `Command::Patch` на аккаунт), обратные
//! правки для «Отменить» и письма, которые удаляются насовсем (из «Удалённых»). Без UI и без I/O.
//!
//! Правка несёт только то, что действительно меняется: обратная правка тогда возвращает ровно прежнее (метка,
//! которая уже стояла, после «Отменить» остаётся).

use std::collections::HashMap;

use postman_cache::{EmailRow, MailboxRow, account_of};

use crate::filters::PHISHING;
use crate::lifecycle::{ARCHIVE, Patch};

/// Ключевые слова нежелательной почты (RFC 8621 §4.1.1, реестр IANA): «Сообщить о фишинге» ставит `$junk` и
/// [`PHISHING`], снимает `$notjunk`.
pub const JUNK: &str = "$junk";
pub const NOT_JUNK: &str = "$notjunk";

/// Что сделать с выделенными письмами.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    /// Прочитано (`true`) или не прочитано.
    Seen(bool),
    /// Поставить (`true`) или снять флаг.
    Flag(bool),
    /// Ключевые слова: категории (`labels::toggle` решает, ставить или снимать).
    Keywords { add: Vec<String>, remove: Vec<String> },
    /// В папку (ключ кэша): только письма того же аккаунта.
    Move { to: String },
    /// В «Архив» аккаунта (нет — создаётся, [`ARCHIVE`]).
    Archive,
    /// В «Удалённые»; уже там (или такой папки нет) — насовсем, после подтверждения.
    Trash,
    /// В «Нежелательные» с `$phishing` и `$junk` (быстрый фильтр «Угрозы»); нет такой папки — только слова.
    Phishing,
}

/// Какое право нужно действию в папке общего ящика (S4-06).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    Seen,
    Keywords,
    /// Убрать из папки (перемещение, удаление).
    Remove,
}

impl Act {
    fn needs(&self) -> &'static [Need] {
        match self {
            Act::Seen(_) => &[Need::Seen],
            Act::Flag(_) | Act::Keywords { .. } => &[Need::Keywords],
            Act::Move { .. } | Act::Archive | Act::Trash => &[Need::Remove],
            Act::Phishing => &[Need::Keywords, Need::Remove],
        }
    }
}

/// План действия: правки (кэш и сервер), обратные правки («Отменить»), удалить насовсем, пропущено.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub patches: Vec<Patch>,
    pub undo: Vec<Patch>,
    /// Ключи писем из «Удалённых»: удалить насовсем (после «Удалить навсегда? Да»).
    pub purge: Vec<String>,
    /// Писем, у которых нет прав на действие (общий ящик только для чтения) или другой аккаунт.
    pub skipped: usize,
}

impl Plan {
    /// Писем, которые изменятся (правкой или удалением насовсем).
    pub fn touched(&self) -> usize {
        self.patches.len() + self.purge.len()
    }
}

/// Папка с ролью `role` аккаунта письма `id`.
fn by_role(mailboxes: &[MailboxRow], id: &str, role: &str) -> Option<String> {
    let account = account_of(id);
    mailboxes.iter().find(|m| m.role.as_deref() == Some(role) && account_of(&m.id) == account).map(|m| m.id.clone())
}

/// Ключевое слово меняется: `(слово, вкл)` — только если у письма оно сейчас другое.
fn keyword(row: &EmailRow, k: &str, on: bool) -> Option<(String, bool)> {
    (row.has_keyword(k) != on).then(|| (k.to_ascii_lowercase(), on))
}

/// Письмо — только в папке `to`: снять с остальных, поставить в неё.
fn relocate(row: &EmailRow, to: &str) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = row.mailbox_ids.iter().filter(|m| *m != to).map(|m| (m.clone(), false)).collect();
    if !row.mailbox_ids.iter().any(|m| m == to) {
        out.push((to.to_owned(), true));
    }
    out
}

/// Обратная правка: каждое изменение — назад.
pub fn invert(p: &Patch) -> Patch {
    let back = |v: &[(String, bool)]| v.iter().map(|(k, on)| (k.clone(), !on)).collect();
    Patch { id: p.id.clone(), keywords: back(&p.keywords), mailboxes: back(&p.mailboxes) }
}

/// Правки для писем `rows`: `allowed` — есть ли у письма право (папки общего ящика). Письма без изменений в план не
/// попадают (уже прочитано, уже в этой папке).
pub fn plan(rows: &[EmailRow], act: &Act, mailboxes: &[MailboxRow], allowed: impl Fn(&EmailRow, Need) -> bool) -> Plan {
    let mut out = Plan::default();
    for row in rows {
        if !act.needs().iter().all(|n| allowed(row, *n)) {
            out.skipped += 1;
            continue;
        }
        let mut p = Patch { id: row.id.clone(), keywords: Vec::new(), mailboxes: Vec::new() };
        match act {
            Act::Seen(on) => p.keywords.extend(keyword(row, postman_cache::SEEN, *on)),
            Act::Flag(on) => p.keywords.extend(keyword(row, postman_cache::FLAGGED, *on)),
            Act::Keywords { add, remove } => {
                p.keywords.extend(remove.iter().filter_map(|k| keyword(row, k, false)));
                p.keywords.extend(add.iter().filter_map(|k| keyword(row, k, true)));
            }
            Act::Move { to } => {
                if account_of(to) != account_of(&row.id) {
                    out.skipped += 1;
                    continue;
                }
                p.mailboxes = relocate(row, to);
            }
            Act::Archive => {
                let to = by_role(mailboxes, &row.id, "archive").unwrap_or_else(|| ARCHIVE.to_owned());
                p.mailboxes = relocate(row, &to);
            }
            Act::Trash => match by_role(mailboxes, &row.id, "trash") {
                Some(trash) if !row.mailbox_ids.contains(&trash) => p.mailboxes = relocate(row, &trash),
                _ => {
                    out.purge.push(row.id.clone());
                    continue;
                }
            },
            Act::Phishing => {
                p.keywords.extend(keyword(row, PHISHING, true));
                p.keywords.extend(keyword(row, JUNK, true));
                p.keywords.extend(keyword(row, NOT_JUNK, false));
                if let Some(junk) = by_role(mailboxes, &row.id, "junk") {
                    p.mailboxes = relocate(row, &junk);
                }
            }
        }
        if !p.is_empty() {
            out.undo.push(invert(&p));
            out.patches.push(p);
        }
    }
    out
}

/// «Прочитано» на выделении: хоть одно непрочитанное — прочитать все, иначе — все непрочитанными.
pub fn seen_target(rows: &[EmailRow]) -> bool {
    rows.iter().any(|r| !r.seen)
}

/// «Флаг» на выделении: хоть одно без флага — поставить всем, иначе снять.
pub fn flag_target(rows: &[EmailRow]) -> bool {
    rows.iter().any(|r| !r.flagged)
}

/// Сводка выделения для области чтения: сколько, от кого, общий размер.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub count: usize,
    /// Отправители по числу писем (больше — раньше; поровну — по первому появлению): имя или адрес и сколько.
    pub senders: Vec<(String, usize)>,
    /// Байт всего (`Email.size`).
    pub size: u64,
    pub unread: usize,
    pub flagged: usize,
}

pub fn summary(rows: &[EmailRow]) -> Summary {
    let mut by: HashMap<String, (usize, usize)> = HashMap::new();
    let mut s = Summary { count: rows.len(), ..Summary::default() };
    for (i, r) in rows.iter().enumerate() {
        let who = if r.from_name.trim().is_empty() { r.from_addr.clone() } else { r.from_name.clone() };
        by.entry(who).or_insert((0, i)).0 += 1;
        s.size = s.size.saturating_add(r.size);
        s.unread += usize::from(!r.seen);
        s.flagged += usize::from(r.flagged);
    }
    let mut senders: Vec<(String, (usize, usize))> = by.into_iter().collect();
    senders.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.1.1.cmp(&b.1.1)));
    s.senders = senders.into_iter().map(|(who, (n, _))| (who, n)).collect();
    s
}

#[cfg(test)]
mod tests {
    use super::{Act, JUNK, NOT_JUNK, Need, Plan, flag_target, invert, plan, seen_target, summary};
    use crate::filters::PHISHING;
    use crate::lifecycle::{ARCHIVE, Patch};
    use postman_cache::{EmailRow, MailboxRow, Rights};

    fn row(id: &str, boxes: &[&str]) -> EmailRow {
        EmailRow {
            id: id.into(),
            thread_id: String::new(),
            mailbox_ids: boxes.iter().map(|b| b.to_string()).collect(),
            received_at: 0,
            subject: format!("s-{id}"),
            from_name: String::new(),
            from_addr: format!("{id}@corp.example"),
            preview: String::new(),
            seen: false,
            flagged: false,
            has_attachment: false,
            size: 100,
            keywords: Vec::new(),
        }
    }

    fn mb(id: &str, role: Option<&str>) -> MailboxRow {
        MailboxRow {
            id: id.into(),
            parent_id: None,
            name: id.into(),
            role: role.map(Into::into),
            sort_order: 0,
            total: 0,
            unread: 0,
            rights: Rights::ALL,
        }
    }

    fn boxes() -> Vec<MailboxRow> {
        vec![mb("in", Some("inbox")), mb("arch", Some("archive")), mb("del", Some("trash")), mb("junk", Some("junk")), mb("p", None)]
    }

    fn all(_: &EmailRow, _: Need) -> bool {
        true
    }

    fn kw(v: &[(&str, bool)]) -> Vec<(String, bool)> {
        v.iter().map(|(k, on)| (k.to_string(), *on)).collect()
    }

    /// Прочитано: письма, которые уже прочитаны, в план не идут; «Отменить» возвращает непрочитанными только те,
    /// что изменились.
    #[test]
    fn seen_changes_only_what_differs() {
        let mut a = row("a", &["in"]);
        a.seen = true;
        let rows = [a, row("b", &["in"])];
        assert!(seen_target(&rows));
        let p = plan(&rows, &Act::Seen(true), &boxes(), all);
        assert_eq!(p.patches, [Patch { id: "b".into(), keywords: kw(&[("$seen", true)]), mailboxes: vec![] }]);
        assert_eq!(p.undo, [Patch { id: "b".into(), keywords: kw(&[("$seen", false)]), mailboxes: vec![] }]);
        let mut read = rows.clone();
        read[1].seen = true;
        assert!(!seen_target(&read), "все прочитаны — кнопка делает непрочитанными");
    }

    /// Категория на выделении: у кого уже стоит — без правки; снять — только там, где стоит.
    #[test]
    fn categories_and_flags_in_batch() {
        let mut a = row("a", &["in"]);
        a.keywords = vec!["pm.cat.team".into()];
        a.flagged = true;
        let rows = [a, row("b", &["in"])];
        let add = Act::Keywords { add: vec!["pm.cat.team".into()], remove: vec![] };
        let p = plan(&rows, &add, &boxes(), all);
        assert_eq!(p.patches.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["b"]);
        let remove = Act::Keywords { add: vec![], remove: vec!["pm.cat.team".into()] };
        assert_eq!(plan(&rows, &remove, &boxes(), all).patches[0].id, "a");
        assert!(flag_target(&rows));
        let p = plan(&rows, &Act::Flag(true), &boxes(), all);
        assert_eq!(p.patches, [Patch { id: "b".into(), keywords: kw(&[("$flagged", true)]), mailboxes: vec![] }]);
    }

    /// В папку, архив, удаление: письмо — только в новой папке; из «Удалённых» — насовсем; «Архива» нет — создать.
    #[test]
    fn moves_archive_and_trash() {
        let rows = [row("a", &["in"]), row("b", &["del"]), row("c", &["p"])];
        let p = plan(&rows, &Act::Move { to: "p".into() }, &boxes(), all);
        assert_eq!(p.patches.len(), 2, "c уже в папке");
        assert_eq!(p.patches[0].mailboxes, kw(&[("in", false), ("p", true)]));
        assert_eq!(p.undo[0].mailboxes, kw(&[("in", true), ("p", false)]));

        let p = plan(&rows, &Act::Trash, &boxes(), all);
        assert_eq!(p.purge, ["b"]);
        assert_eq!(p.patches.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["a", "c"]);
        assert_eq!(p.touched(), 3);

        let no_archive: Vec<MailboxRow> = boxes().into_iter().filter(|m| m.id != "arch").collect();
        let p = plan(&rows[..1], &Act::Archive, &no_archive, all);
        assert_eq!(p.patches[0].mailboxes, kw(&[("in", false), (ARCHIVE, true)]));
        let p = plan(&rows[..1], &Act::Archive, &boxes(), all);
        assert_eq!(p.patches[0].mailboxes, kw(&[("in", false), ("arch", true)]));
    }

    /// Фишинг: в «Нежелательные», `$phishing` и `$junk`, без `$notjunk`; «Отменить» — всё назад.
    #[test]
    fn phishing_goes_to_junk_with_keywords() {
        let mut a = row("a", &["in"]);
        a.keywords = vec![NOT_JUNK.into()];
        let p = plan(&[a], &Act::Phishing, &boxes(), all);
        let got = &p.patches[0];
        assert_eq!(got.keywords, kw(&[(PHISHING, true), (JUNK, true), (NOT_JUNK, false)]));
        assert_eq!(got.mailboxes, kw(&[("in", false), ("junk", true)]));
        assert_eq!(p.undo[0], invert(got));
        assert_eq!(invert(&p.undo[0]), *got);
        // Нет папки «Нежелательные» — только слова.
        let no_junk: Vec<MailboxRow> = boxes().into_iter().filter(|m| m.id != "junk").collect();
        let p = plan(&[row("b", &["in"])], &Act::Phishing, &no_junk, all);
        assert!(p.patches[0].mailboxes.is_empty() && p.patches[0].keywords.len() == 2);
    }

    /// Общий ящик только для чтения и другой аккаунт — пропуск, остальные — в плане.
    #[test]
    fn rights_and_accounts_are_respected() {
        let rows = [row("a", &["in"]), row("j/b", &["j/in"])];
        let ro = |r: &EmailRow, n: Need| !(r.id.starts_with("j/") && n == Need::Remove);
        let p = plan(&rows, &Act::Archive, &boxes(), ro);
        assert_eq!((p.patches.len(), p.skipped), (1, 1));
        let p = plan(&rows, &Act::Seen(true), &boxes(), ro);
        assert_eq!((p.patches.len(), p.skipped), (2, 0), "прочитать можно");
        let p = plan(&rows, &Act::Move { to: "p".into() }, &boxes(), all);
        assert_eq!((p.patches.len(), p.skipped), (1, 1), "в папку чужого аккаунта не переносим");
        assert_eq!(plan(&[], &Act::Trash, &boxes(), all), Plan::default());
    }

    #[test]
    fn summary_counts_senders_and_size() {
        let mut rows = vec![row("a", &["in"]), row("b", &["in"]), row("c", &["in"])];
        rows[0].from_name = "Анна".into();
        rows[1].from_name = "Пётр".into();
        rows[2].from_name = "Пётр".into();
        rows[2].seen = true;
        rows[1].flagged = true;
        let s = summary(&rows);
        assert_eq!((s.count, s.size, s.unread, s.flagged), (3, 300, 2, 1));
        assert_eq!(s.senders, [("Пётр".to_string(), 2), ("Анна".to_string(), 1)]);
        // Без имени — адрес.
        assert_eq!(summary(&[row("x", &[])]).senders[0].0, "x@corp.example");
    }
}
