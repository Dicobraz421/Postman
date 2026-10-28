//! Беседа (S5-05): письма одной цепочки из всех папок (входящие, «Отправленные», архив) по времени, свои и чужие,
//! разделители по дням. Тела и разбор — отдельно: здесь только порядок и разметка ленты. Вид «в списке, как
//! Outlook» (S5-05б) — [`Outline`]: строка на беседу, под ней — её письма, когда беседа развёрнута.

use std::collections::{HashMap, HashSet};

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use postman_cache::EmailRow;

pub use crate::prefs::ThreadOrder;

/// Письмо беседы.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub row: EmailRow,
    /// Отправлено мной: адрес «От» — один из моих (Identity, в том числе общих ящиков).
    pub mine: bool,
    /// День получения в часовом поясе ленты.
    pub day: Date,
}

/// Строка ленты: разделитель дня или письмо (индекс в [`Conversation::messages`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Day(Date),
    Message(usize),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Conversation {
    /// Ключ цепочки в кэше (`EmailRow::thread_id`).
    pub thread: String,
    /// В порядке беседы ([`ThreadOrder`]; у [`Conversation::assemble`] — старые первыми).
    pub messages: Vec<Message>,
    pub items: Vec<Item>,
}

impl Conversation {
    /// Собрать ленту: письма по времени получения (при равном — по id), старые первыми, повторы id отброшены; перед
    /// первым письмом каждого дня — разделитель. `me` — мои адреса, без учёта регистра.
    pub fn assemble(thread: &str, rows: Vec<EmailRow>, me: &[String], tz: &TimeZone) -> Self {
        Self::assemble_in(thread, rows, me, tz, ThreadOrder::OldestFirst)
    }

    /// [`Conversation::assemble`] в порядке `order` (T1-16а): сначала новые — и письма, и дни от новых к старым.
    pub fn assemble_in(thread: &str, mut rows: Vec<EmailRow>, me: &[String], tz: &TimeZone, order: ThreadOrder) -> Self {
        rows.sort_by(|a, b| a.received_at.cmp(&b.received_at).then_with(|| a.id.cmp(&b.id)));
        rows.dedup_by(|a, b| a.id == b.id);
        if order == ThreadOrder::NewestFirst {
            rows.reverse();
        }
        let mut c = Self { thread: thread.to_owned(), ..Self::default() };
        for row in rows {
            let day = Timestamp::from_second(row.received_at).map(|t| t.to_zoned(tz.clone()).date()).unwrap_or_default();
            if c.messages.last().is_none_or(|m| m.day != day) {
                c.items.push(Item::Day(day));
            }
            c.items.push(Item::Message(c.messages.len()));
            let mine = is_mine(&row.from_addr, me);
            c.messages.push(Message { row, mine, day });
        }
        c
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Место письма в беседе.
    pub fn position(&self, id: &str) -> Option<usize> {
        self.messages.iter().position(|m| m.row.id == id)
    }

    /// Соседнее письмо (Ctrl+→ / Ctrl+←): `delta` писем от `id`, у края — `None`.
    pub fn neighbor(&self, id: &str, delta: isize) -> Option<&EmailRow> {
        let at = self.position(id)?.checked_add_signed(delta)?;
        self.messages.get(at).map(|m| &m.row)
    }
}

// --- беседы в списке, как Outlook (S5-05б) ---

/// Беседа в списке: письма одной цепочки в открытой папке по порядку списка. Первое — головная строка (при сортировке
/// по дате — самое новое), остальные — строки под ней, когда беседа развёрнута.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    /// Ключ цепочки; у письма без цепочки — его id.
    pub key: String,
    pub ids: Vec<String>,
    /// Номер группы списка (дата, папка) головного письма.
    pub group: usize,
}

/// Строка списка: голова беседы (номер в [`Outline::heads`]) или письмо под ней (номер беседы, номер письма в
/// [`Head::ids`], от 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line {
    Head(usize),
    Child(usize, usize),
}

/// Список, сгруппированный по беседам: видимые строки с учётом развёрнутых бесед. Номера строк — номера списка:
/// j/k идут по ним, ← / → сворачивают и разворачивают (`collapse`, `expand`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Outline {
    heads: Vec<Head>,
    /// Развёрнутые беседы (ключи).
    open: HashSet<String>,
    lines: Vec<Line>,
    /// id видимых строк — по порядку `lines`.
    ids: Vec<String>,
}

impl Outline {
    /// Сгруппировать: `items` — (id, ключ цепочки, номер группы) по порядку списка. Беседа — там, где её первое
    /// письмо; `split` — беседы не переходят из группы в группу («Все папки»: беседа своя у каждой папки). Голова и
    /// письма под ней — по порядку списка.
    pub fn build(items: impl IntoIterator<Item = (String, String, usize)>, split: bool, open: HashSet<String>) -> Self {
        // Порядок списка вместо времени: раньше в списке — новее.
        let items = items.into_iter().enumerate().map(|(n, (id, thread, group))| (id, thread, group, -(n as i64)));
        Self::build_in(items, split, open, ThreadOrder::NewestFirst)
    }

    /// [`Outline::build`] со временем писем (T1-16а): `items` — (id, ключ цепочки, номер группы, время получения) по
    /// порядку списка. Беседа — там, где её первое письмо; голова — самое новое письмо беседы (как Outlook), письма
    /// под ней — в порядке `order`; при равном времени — по порядку списка.
    pub fn build_in(items: impl IntoIterator<Item = (String, String, usize, i64)>, split: bool, open: HashSet<String>, order: ThreadOrder) -> Self {
        let mut heads: Vec<Head> = Vec::new();
        let mut times: Vec<Vec<i64>> = Vec::new();
        let mut at: HashMap<(String, usize), usize> = HashMap::new();
        for (id, thread, group, time) in items {
            let key = if thread.is_empty() { id.clone() } else { thread };
            let slot = (key.clone(), if split { group } else { 0 });
            match at.get(&slot) {
                Some(&h) => {
                    if let (Some(head), Some(t)) = (heads.get_mut(h), times.get_mut(h))
                        && !head.ids.contains(&id)
                    {
                        head.ids.push(id);
                        t.push(time);
                    }
                }
                None => {
                    at.insert(slot, heads.len());
                    heads.push(Head { key, ids: vec![id], group });
                    times.push(vec![time]);
                }
            }
        }
        for (head, times) in heads.iter_mut().zip(times) {
            // Сортировка устойчивая: при равном времени — порядок списка.
            let mut by: Vec<(i64, String)> = times.into_iter().zip(std::mem::take(&mut head.ids)).collect();
            by.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
            let mut ids = by.into_iter().map(|(_, id)| id);
            head.ids.extend(ids.next());
            match order {
                ThreadOrder::NewestFirst => head.ids.extend(ids),
                ThreadOrder::OldestFirst => head.ids.extend(ids.rev()),
            }
        }
        let mut o = Self { heads, open, ..Self::default() };
        o.open.retain(|k| !k.is_empty());
        o.relines();
        o
    }

    fn relines(&mut self) {
        self.lines.clear();
        self.ids.clear();
        for (h, head) in self.heads.iter().enumerate() {
            self.lines.push(Line::Head(h));
            self.ids.extend(head.ids.first().cloned());
            if head.ids.len() > 1 && self.open.contains(&head.key) {
                for (k, id) in head.ids.iter().enumerate().skip(1) {
                    self.lines.push(Line::Child(h, k));
                    self.ids.push(id.clone());
                }
            }
        }
    }

    pub fn heads(&self) -> &[Head] {
        &self.heads
    }

    /// Видимые строки.
    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// id видимых строк по порядку.
    pub fn ids(&self) -> &[String] {
        &self.ids
    }

    /// Писем во всех беседах.
    pub fn total(&self) -> usize {
        self.heads.iter().map(|h| h.ids.len()).sum()
    }

    /// Все письма всех бесед (выделить все, проверка выделения).
    pub fn all_ids(&self) -> impl Iterator<Item = &String> {
        self.heads.iter().flat_map(|h| &h.ids)
    }

    /// Развёрнутые беседы (ключи) — сохранить при пересчёте списка.
    pub fn open(&self) -> &HashSet<String> {
        &self.open
    }

    pub fn line(&self, row: usize) -> Option<Line> {
        self.lines.get(row).copied()
    }

    /// Беседа строки.
    pub fn head_of(&self, row: usize) -> Option<&Head> {
        match self.line(row)? {
            Line::Head(h) | Line::Child(h, _) => self.heads.get(h),
        }
    }

    /// Строка головы беседы `h`.
    fn head_row(&self, h: usize) -> Option<usize> {
        self.lines.iter().position(|l| *l == Line::Head(h))
    }

    pub fn is_open(&self, h: &Head) -> bool {
        h.ids.len() > 1 && self.open.contains(&h.key)
    }

    /// Письма строки: у головы — вся беседа (выделить строку беседы — все её письма), у письма под ней — оно одно.
    pub fn members(&self, row: usize) -> &[String] {
        match self.line(row) {
            Some(Line::Head(h)) => self.heads.get(h).map_or(&[][..], |h| &h.ids[..]),
            Some(Line::Child(_, _)) => self.ids.get(row..row + 1).unwrap_or(&[]),
            None => &[],
        }
    }

    /// Номер видимой строки письма.
    pub fn position(&self, id: &str) -> Option<usize> {
        self.ids.iter().position(|x| x == id)
    }

    /// → : свёрнутая беседа — развернуть (курсор остаётся); развёрнутая — курсор на первое письмо под ней.
    /// Нечего делать — `None`.
    pub fn expand(&mut self, row: usize) -> Option<usize> {
        let Line::Head(h) = self.line(row)? else { return None };
        let head = self.heads.get(h)?;
        if head.ids.len() < 2 {
            return None;
        }
        if self.is_open(head) {
            return Some(row + 1);
        }
        self.open.insert(head.key.clone());
        self.relines();
        Some(row)
    }

    /// ← : письмо под головой — курсор на голову; развёрнутая беседа — свернуть. Нечего делать — `None`.
    pub fn collapse(&mut self, row: usize) -> Option<usize> {
        match self.line(row)? {
            Line::Child(h, _) => self.head_row(h),
            Line::Head(h) => {
                let key = self.heads.get(h).filter(|head| self.is_open(head))?.key.clone();
                self.open.remove(&key);
                self.relines();
                Some(row)
            }
        }
    }

    /// Enter и стрелка у строки беседы: развернуть или свернуть; курсор — на голове. У одиночного письма — `None`.
    pub fn toggle(&mut self, row: usize) -> Option<usize> {
        let h = match self.line(row)? {
            Line::Head(h) | Line::Child(h, _) => h,
        };
        let head = self.heads.get(h)?;
        if head.ids.len() < 2 {
            return None;
        }
        let key = head.key.clone();
        if !self.open.remove(&key) {
            self.open.insert(key);
        }
        self.relines();
        self.head_row(h)
    }

    /// Развёрнутые беседы — эти (пересчёт списка шёл, пока беседу сворачивали или разворачивали).
    pub fn set_open(&mut self, open: HashSet<String>) {
        if self.open != open {
            self.open = open;
            self.relines();
        }
    }

    /// Письмо ушло из списка (удалено, перенесено): из своей беседы; беседа без писем — из списка, голова — следующее.
    pub fn remove(&mut self, id: &str) {
        for h in &mut self.heads {
            h.ids.retain(|x| x != id);
        }
        self.heads.retain(|h| !h.ids.is_empty());
        self.relines();
    }

    /// Группы подряд с числом видимых строк: `groups` — группы списка по номеру [`Head::group`].
    pub fn groups<G: Clone + PartialEq>(&self, groups: &[G]) -> Vec<(G, usize)> {
        let mut out: Vec<(G, usize)> = Vec::new();
        for line in &self.lines {
            let h = match *line {
                Line::Head(h) | Line::Child(h, _) => h,
            };
            let Some(g) = self.heads.get(h).and_then(|h| groups.get(h.group)) else { continue };
            match out.last_mut() {
                Some((last, n)) if last == g => *n += 1,
                _ => out.push((g.clone(), 1)),
            }
        }
        out
    }
}

/// Адрес — мой (без учёта регистра).
pub fn is_mine(addr: &str, me: &[String]) -> bool {
    !addr.is_empty() && me.iter().any(|m| m.eq_ignore_ascii_case(addr))
}

/// Пересылка по теме («FW:», «Fwd:», «Пересл:»): пересланное письмо — содержимое, в ленте показывается целиком.
pub fn is_forward(subject: &str) -> bool {
    let s = subject.trim_start().to_lowercase();
    ["fw:", "fwd:", "пересл:", "переслано:"].iter().any(|p| s.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, at: i64, from: &str) -> EmailRow {
        EmailRow {
            id: id.into(),
            thread_id: "t".into(),
            mailbox_ids: vec!["in".into()],
            received_at: at,
            subject: "Re: План".into(),
            from_name: from.into(),
            from_addr: from.into(),
            preview: String::new(),
            seen: true,
            flagged: false,
            has_attachment: false,
            size: 1,
            keywords: Vec::new(),
        }
    }

    #[test]
    fn ordered_by_time_with_days_and_mine() {
        let tz = TimeZone::UTC;
        // 2026-10-19 23:00, 2026-10-20 09:00 и 10:00 UTC; своё письмо — из «Отправленных».
        let (d1, d2) = (1_792_450_800, 1_792_486_800);
        let rows = vec![row("c", d2 + 3600, "ivan@example.com"), row("a", d1, "ivan@example.com"), row("b", d2, "ME@example.com")];
        let c = Conversation::assemble("t", rows.clone(), &["me@example.com".into()], &tz);
        let ids: Vec<&str> = c.messages.iter().map(|m| m.row.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c"]);
        assert_eq!(c.messages.iter().map(|m| m.mine).collect::<Vec<_>>(), [false, true, false]);
        let day = |s: &str| s.parse::<Date>().unwrap();
        assert_eq!(c.items, [Item::Day(day("2026-10-19")), Item::Message(0), Item::Day(day("2026-10-20")), Item::Message(1), Item::Message(2)]);
        // В часовом поясе +03:00 все три — 20-го: один разделитель.
        let msk = TimeZone::fixed(jiff::tz::offset(3));
        assert_eq!(Conversation::assemble("t", rows, &[], &msk).items.iter().filter(|i| matches!(i, Item::Day(_))).count(), 1);
    }

    #[test]
    fn neighbors_and_duplicates() {
        let rows = vec![row("a", 1, "x@e"), row("b", 2, "x@e"), row("a", 1, "x@e")];
        let c = Conversation::assemble("t", rows, &[], &TimeZone::UTC);
        assert_eq!(c.len(), 2);
        assert_eq!(c.neighbor("a", 1).map(|r| r.id.as_str()), Some("b"));
        assert_eq!(c.neighbor("b", -1).map(|r| r.id.as_str()), Some("a"));
        assert!(c.neighbor("b", 1).is_none() && c.neighbor("a", -1).is_none() && c.neighbor("zz", 1).is_none());
        // Пустой адрес «От» — не мой, даже если среди моих есть пустой.
        assert!(!is_mine("", &[String::new()]));
    }

    /// Список по датам (новые первыми): беседа — на месте самого нового письма, с числом писем; под ней — остальные,
    /// тоже от новых к старым; группы считаются по видимым строкам.
    #[test]
    fn outline_groups_by_newest_with_counts_and_child_order() {
        // (id, цепочка, группа): t1 — три письма в двух днях, t2 — одно, у письма без цепочки — свой ключ.
        let items = [("a3", "t1", 0), ("b", "t2", 0), ("a2", "t1", 1), ("x", "", 1), ("a1", "t1", 1), ("a2", "t1", 1)];
        let items = items.map(|(id, t, g)| (id.to_string(), t.to_string(), g));
        let o = Outline::build(items.clone(), false, HashSet::new());
        let heads: Vec<(&str, usize)> = o.heads().iter().map(|h| (h.ids[0].as_str(), h.ids.len())).collect();
        assert_eq!(heads, [("a3", 3), ("b", 1), ("x", 1)], "повтор id не считается");
        assert_eq!(o.heads()[0].ids, ["a3", "a2", "a1"]);
        assert_eq!(o.heads()[2].key, "x");
        assert_eq!(o.ids(), ["a3", "b", "x"], "все свёрнуты");
        assert_eq!(o.groups(&["today", "yesterday"]), [("today", 2), ("yesterday", 1)]);
        assert_eq!(o.members(0), ["a3", "a2", "a1"], "строка беседы — все её письма");
        assert_eq!((o.all_ids().count(), o.total()), (5, 5));
        // Развёрнута при сборке (пересчёт списка помнит развёрнутые).
        let o = Outline::build(items.clone(), false, ["t1".to_string()].into());
        assert_eq!(o.ids(), ["a3", "a2", "a1", "b", "x"]);
        assert_eq!(o.lines()[1..3], [Line::Child(0, 1), Line::Child(0, 2)]);
        assert_eq!(o.groups(&["today", "yesterday"]), [("today", 4), ("yesterday", 1)], "письма под головой — в её группе");
        assert_eq!(o.members(2), ["a1"]);
        // «Все папки»: беседа своя в каждой группе.
        let o = Outline::build(items, true, HashSet::new());
        assert_eq!(o.ids(), ["a3", "b", "a2", "x"]);
    }

    /// j/k — по видимым строкам; → разворачивает и затем ведёт к первому письму под головой, ← — к голове и
    /// сворачивает; Enter — туда и обратно.
    #[test]
    fn outline_navigation_over_visible_rows() {
        let items = [("a2", "t1"), ("b", "t2"), ("a1", "t1"), ("c", "t3")];
        let mut o = Outline::build(items.map(|(id, t)| (id.to_string(), t.to_string(), 0)), false, HashSet::new());
        assert_eq!(o.ids(), ["a2", "b", "c"]);
        assert_eq!(o.expand(1), None, "одно письмо — нечего разворачивать");
        assert_eq!(o.collapse(0), None, "уже свёрнута");
        assert_eq!(o.expand(0), Some(0));
        assert_eq!(o.ids(), ["a2", "a1", "b", "c"]);
        assert_eq!(o.position("b"), Some(2), "строки ниже сдвинулись");
        assert_eq!(o.expand(0), Some(1), "развёрнута — к первому письму под ней");
        assert_eq!(o.expand(1), None);
        assert_eq!(o.collapse(1), Some(0), "письмо под головой — к голове");
        assert_eq!(o.ids().len(), 4, "и беседа ещё развёрнута");
        assert_eq!(o.collapse(0), Some(0));
        assert_eq!(o.ids(), ["a2", "b", "c"]);
        assert!(!o.open().contains("t1"));
        assert_eq!(o.toggle(0), Some(0));
        assert_eq!(o.toggle(1), Some(0), "Enter на письме под головой — свернуть, курсор на голову");
        assert_eq!(o.ids(), ["a2", "b", "c"]);
        assert_eq!(o.toggle(2), None);
        // Пересчёт с другим набором развёрнутых; удалённое письмо уходит, голова — следующее письмо беседы.
        o.set_open(["t1".to_string()].into());
        assert_eq!(o.ids(), ["a2", "a1", "b", "c"]);
        o.remove("a2");
        assert_eq!(o.ids(), ["a1", "b", "c"]);
        assert_eq!(o.members(0), ["a1"]);
        o.remove("b");
        assert_eq!(o.ids(), ["a1", "c"]);
    }

    /// Порядок беседы (T1-16а): лента и дни — в выбранном порядке; в списке голова — самое новое письмо, под ней —
    /// остальные в выбранном порядке, где бы они ни стояли в списке.
    #[test]
    fn thread_order_in_feed_and_outline() {
        let (d1, d2) = (1_792_450_800, 1_792_486_800);
        let rows = vec![row("c", d2 + 3600, "x@e"), row("a", d1, "x@e"), row("b", d2, "x@e")];
        let ids = |c: &Conversation| c.messages.iter().map(|m| m.row.id.clone()).collect::<Vec<_>>();
        let old = Conversation::assemble_in("t", rows.clone(), &[], &TimeZone::UTC, ThreadOrder::OldestFirst);
        assert_eq!(ids(&old), ["a", "b", "c"]);
        assert_eq!(old, Conversation::assemble("t", rows.clone(), &[], &TimeZone::UTC), "по умолчанию — как раньше");
        let new = Conversation::assemble_in("t", rows, &[], &TimeZone::UTC, ThreadOrder::NewestFirst);
        assert_eq!(ids(&new), ["c", "b", "a"]);
        let day = |s: &str| s.parse::<Date>().unwrap();
        assert_eq!(new.items, [Item::Day(day("2026-10-20")), Item::Message(0), Item::Message(1), Item::Day(day("2026-10-19")), Item::Message(2)]);
        assert_eq!(new.neighbor("c", 1).map(|r| r.id.as_str()), Some("b"), "соседи — по порядку на экране");
        // Список не по дате: беседа — на месте первого письма, голова — самое новое (a3).
        let items = [("a2", "t1", 20), ("b", "t2", 5), ("a3", "t1", 30), ("a1", "t1", 10)];
        let items = items.map(|(id, t, at)| (id.to_string(), t.to_string(), 0, at));
        let open: HashSet<String> = ["t1".to_string()].into();
        let o = Outline::build_in(items.clone(), false, open.clone(), ThreadOrder::OldestFirst);
        assert_eq!(o.heads()[0].ids, ["a3", "a1", "a2"]);
        assert_eq!(o.ids(), ["a3", "a1", "a2", "b"]);
        let o = Outline::build_in(items, false, open, ThreadOrder::NewestFirst);
        assert_eq!(o.heads()[0].ids, ["a3", "a2", "a1"]);
        assert_eq!(o.ids(), ["a3", "a2", "a1", "b"]);
        assert_eq!(o.members(0), ["a3", "a2", "a1"]);
    }

    #[test]
    fn forwards_by_subject() {
        assert!(is_forward("FW: Смета") && is_forward("Fwd: budget") && is_forward(" Пересл: x"));
        assert!(!is_forward("Re: FW: x") && !is_forward("Fwdx"));
    }
}
