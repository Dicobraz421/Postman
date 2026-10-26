//! Фильтры списка писем (S5-03, `docs/DESIGN.md` «Фильтры», «Быстрые фильтры»): условия панели и закладок, их
//! счётчики, сортировка и группы (по датам, по папкам), условие `Email/query` для «Все папки» на сервере.
//!
//! Логика: флажки панели (непрочитанные, упоминания, вложения, флаг, внешние) — И; категории панели — ИЛИ между
//! собой, И с флажками; закладки быстрых фильтров — ИЛИ между собой, И с панелью.
//!
//! Что значит условие — по данным кэша, без чтения тела:
//! - «Упоминания» — в тексте письма (начало тела, `preview`) есть `@` и сразу за ним моё имя, любое слово имени
//!   или имя ящика до `@` (`@Анна`, `@Смирнова Анна`, `@me`), либо мой полный адрес вне `<…>` (адрес в угловых
//!   скобках — строка цитаты «…, Анна <me@corp.example>:», это не упоминание). Обращение без `@` («Анна, добрый
//!   день») — не упоминание: так начинается почти каждое письмо мне, это закладка «Мне». На сервере («Все папки»)
//!   — то же `@имя` во всём теле (`body`).
//! - «Мне» — мой адрес среди To (не Cc).
//! - «Внешние» — домен отправителя не мой и не поддомен моего (домены — из моих адресов отправителя).
//! - «Угрозы» — ключевое слово `$phishing` (IANA) или подсказка безопасности `pm.threat` (поставит S5-08).
//! - «Срок», «Ждём», «Истекают» — ключевые слова жизненного цикла `pm.due`, `pm.wait`, `pm.expire` (поставит
//!   S5-04; слово с уточнением через точку, `pm.due.20261023`, — то же условие).

use std::collections::{BTreeMap, BTreeSet};

use crate::model::Filter;
use postman_cache::{EmailRow, Recipients};

/// Фишинг: ключевое слово IANA (RFC 8621 §4.1.1, реестр ключевых слов IMAP/JMAP).
pub const PHISHING: &str = "$phishing";
/// Подсказка безопасности Postman (проверки письма, S5-08).
pub const THREAT: &str = "pm.threat";
/// Жизненный цикл (S5-04): срок, ждём ответа, письмо исчезнет или уйдёт в архив.
pub const DUE: &str = "pm.due";
pub const WAIT: &str = "pm.wait";
pub const EXPIRE: &str = "pm.expire";

/// Писем поручений в условии сервера — не больше (условие `Email/query` не растёт без конца).
const WAITING_MAX: usize = 64;

/// Есть ли у письма слово `base` или его уточнение (`base.…`).
fn has_word(keywords: &[String], base: &str) -> bool {
    keywords.iter().any(|k| k.strip_prefix(base).is_some_and(|rest| rest.is_empty() || rest.starts_with('.')))
}

// --- кто я ---

/// Мои адреса и имена: упоминания, «Мне», внешние отправители. Всё в нижнем регистре.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Me {
    addresses: Vec<String>,
    /// Что может стоять после `@` в упоминании: полное имя, слова имени, имя ящика до `@`.
    handles: Vec<String>,
    domains: Vec<String>,
    /// Слова писем моих невыполненных поручений (`assign::waiting_keywords`, S6-11): такие письма — в «Ждём».
    waiting: BTreeSet<String>,
}

impl Me {
    pub fn new<A: AsRef<str>, N: AsRef<str>>(addresses: impl IntoIterator<Item = A>, names: impl IntoIterator<Item = N>) -> Me {
        let mut me = Me::default();
        for a in addresses {
            let a = a.as_ref().trim().to_lowercase();
            let Some((local, domain)) = a.split_once('@') else { continue };
            push(&mut me.handles, local);
            push(&mut me.domains, domain);
            push(&mut me.addresses, &a);
        }
        for n in names {
            let n = n.as_ref().trim().to_lowercase();
            let words: Vec<&str> = n.split_whitespace().filter(|w| w.chars().count() >= 2).collect();
            if words.len() > 1 {
                push(&mut me.handles, &words.join(" "));
            }
            for w in words {
                push(&mut me.handles, w);
            }
        }
        // Длинные первыми: `@смирнова анна` совпадёт целиком, а не по первому слову.
        me.handles.sort_by_key(|h| std::cmp::Reverse(h.len()));
        me
    }

    /// Письма поручений, которых ждём (S6-11): «Ждём» — ещё и они.
    pub fn with_waiting<S: AsRef<str>>(mut self, keywords: impl IntoIterator<Item = S>) -> Me {
        self.waiting = keywords.into_iter().map(|k| k.as_ref().to_ascii_lowercase()).collect();
        self
    }

    pub fn waiting(&self) -> &BTreeSet<String> {
        &self.waiting
    }

    /// Адреса ещё неизвестны (живой ящик до `Event::Me`): упоминаний, «Мне» и внешних нет.
    pub fn is_empty(&self) -> bool {
        self.addresses.is_empty()
    }

    pub fn addresses(&self) -> &[String] {
        &self.addresses
    }

    pub fn domains(&self) -> &[String] {
        &self.domains
    }

    /// Отправитель снаружи: домен не мой и не поддомен моего. Без адреса или без моих доменов — не внешний.
    pub fn external(&self, from: &str) -> bool {
        let from = from.trim().to_lowercase();
        let Some((_, domain)) = from.rsplit_once('@') else { return false };
        if self.domains.is_empty() || domain.is_empty() {
            return false;
        }
        !self.domains.iter().any(|d| domain == d || domain.strip_suffix(d.as_str()).is_some_and(|sub| sub.ends_with('.')))
    }

    /// Меня упомянули в тексте (см. описание модуля).
    pub fn mentioned(&self, text: &str) -> bool {
        if self.is_empty() || text.is_empty() {
            return false;
        }
        let lower = text.to_lowercase();
        for (at, _) in lower.match_indices('@') {
            let rest = &lower[at + 1..];
            let hit = self
                .handles
                .iter()
                .any(|h| rest.strip_prefix(h.as_str()).is_some_and(|tail| tail.chars().next().is_none_or(|c| !c.is_alphanumeric())));
            if hit {
                return true;
            }
        }
        self.addresses.iter().any(|a| lower.match_indices(a.as_str()).any(|(i, _)| !lower[..i].ends_with('<')))
    }

    /// Письмо мне напрямую: мой адрес в To.
    pub fn to_me(&self, r: &Recipients) -> bool {
        r.to.iter().any(|t| self.addresses.iter().any(|a| t.eq_ignore_ascii_case(a)))
    }
}

fn push(list: &mut Vec<String>, v: &str) {
    if !v.is_empty() && !list.iter().any(|x| x == v) {
        list.push(v.to_owned());
    }
}

// --- условия ---

/// Флажок группы «Состояние» или «Отправитель» панели фильтров.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Check {
    Unread,
    Mentions,
    Attachments,
    Flagged,
    External,
}

/// Флажки в порядке панели.
pub const CHECKS: [Check; 5] = [Check::Unread, Check::Mentions, Check::Attachments, Check::Flagged, Check::External];

/// Закладка быстрых фильтров.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Quick {
    Threats,
    Due,
    Waiting,
    ToMe,
    Expiring,
}

/// Закладки по срочности (DESIGN): Угрозы → Срок → Ждём → Мне → Истекают.
pub const QUICK: [Quick; 5] = [Quick::Threats, Quick::Due, Quick::Waiting, Quick::ToMe, Quick::Expiring];

/// Закладка из настроек (`prefs::QuickFilters` — порядок и видимость, S5-09).
impl From<crate::prefs::Quick> for Quick {
    fn from(q: crate::prefs::Quick) -> Quick {
        use crate::prefs::Quick as P;
        match q {
            P::Threats => Quick::Threats,
            P::Deadline => Quick::Due,
            P::Waiting => Quick::Waiting,
            P::Mine => Quick::ToMe,
            P::Expiring => Quick::Expiring,
        }
    }
}

/// Что известно о письме для фильтров.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    pub unread: bool,
    pub flagged: bool,
    pub attachment: bool,
    pub mention: bool,
    pub external: bool,
    pub to_me: bool,
    pub threat: bool,
    pub due: bool,
    pub waiting: bool,
    pub expiring: bool,
}

impl Facts {
    pub fn of(me: &Me, row: &EmailRow, to: &Recipients) -> Facts {
        let k = row.keywords.as_slice();
        Facts {
            unread: !row.seen,
            flagged: row.flagged,
            attachment: row.has_attachment,
            mention: me.mentioned(&row.preview),
            external: me.external(&row.from_addr),
            to_me: me.to_me(to),
            threat: has_word(k, PHISHING) || has_word(k, THREAT),
            due: has_word(k, DUE),
            // «Ждём ответа» или письмо моего невыполненного поручения — одно письмо один раз.
            waiting: has_word(k, WAIT) || k.iter().any(|w| me.waiting.contains(w)),
            expiring: has_word(k, EXPIRE),
        }
    }

    pub fn check(&self, c: Check) -> bool {
        match c {
            Check::Unread => self.unread,
            Check::Mentions => self.mention,
            Check::Attachments => self.attachment,
            Check::Flagged => self.flagged,
            Check::External => self.external,
        }
    }

    pub fn quick(&self, q: Quick) -> bool {
        match q {
            Quick::Threats => self.threat,
            Quick::Due => self.due,
            Quick::Waiting => self.waiting,
            Quick::ToMe => self.to_me,
            Quick::Expiring => self.expiring,
        }
    }
}

/// Включённые фильтры: флажки и категории панели, закладки.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Criteria {
    pub checks: BTreeSet<Check>,
    /// Ключевые слова категорий (`pm.cat.…`).
    pub categories: BTreeSet<String>,
    pub quick: BTreeSet<Quick>,
}

impl Criteria {
    pub fn is_empty(&self) -> bool {
        self.checks.is_empty() && self.categories.is_empty() && self.quick.is_empty()
    }

    /// Фильтров панели (флажки и категории) — счётчик на воронке; закладки не считаются.
    pub fn panel_len(&self) -> usize {
        self.checks.len() + self.categories.len()
    }

    pub fn toggle_check(&mut self, c: Check) {
        if !self.checks.remove(&c) {
            self.checks.insert(c);
        }
    }

    pub fn toggle_category(&mut self, keyword: &str) {
        if !self.categories.remove(keyword) {
            self.categories.insert(keyword.to_owned());
        }
    }

    pub fn toggle_quick(&mut self, q: Quick) {
        if !self.quick.remove(&q) {
            self.quick.insert(q);
        }
    }

    /// Сбросить фильтры панели (чип «×», «Сбросить»); закладки остаются.
    pub fn clear_panel(&mut self) {
        self.checks.clear();
        self.categories.clear();
    }

    fn categories_pass(&self, keywords: &[String]) -> bool {
        self.categories.is_empty() || self.categories.iter().any(|c| keywords.iter().any(|k| k == c))
    }

    fn quick_pass(&self, f: &Facts) -> bool {
        self.quick.is_empty() || self.quick.iter().any(|q| f.quick(*q))
    }

    /// Письмо проходит все фильтры.
    pub fn matches(&self, f: &Facts, keywords: &[String]) -> bool {
        self.checks.iter().all(|c| f.check(*c)) && self.categories_pass(keywords) && self.quick_pass(f)
    }

    /// Условие `Email/query` для сервера («Все папки», живой ящик); пусто — `None`. Чего сервер не умеет
    /// (внешние без моих доменов, «Мне» без адреса) — не ставится: такие письма отсеет проверка в кэше.
    pub fn server_filter(&self, me: &Me) -> Option<Filter> {
        let mut and = Vec::new();
        for c in &self.checks {
            match c {
                Check::Unread => and.push(Filter::NotKeyword(postman_cache::SEEN.into())),
                Check::Flagged => and.push(Filter::HasKeyword(postman_cache::FLAGGED.into())),
                Check::Attachments => and.push(Filter::HasAttachment(true)),
                Check::Mentions if !me.handles.is_empty() => {
                    and.push(Filter::Or(me.handles.iter().map(|h| Filter::Body(format!("@{h}"))).collect()));
                }
                Check::External if !me.domains.is_empty() => {
                    and.push(Filter::Not(me.domains.iter().map(|d| Filter::From(format!("@{d}"))).collect()));
                }
                Check::Mentions | Check::External => {}
            }
        }
        if !self.categories.is_empty() {
            and.push(Filter::Or(self.categories.iter().map(|k| Filter::HasKeyword(k.clone())).collect()));
        }
        let quick: Vec<Filter> = self
            .quick
            .iter()
            .flat_map(|q| match q {
                Quick::Threats => vec![Filter::HasKeyword(PHISHING.into()), Filter::HasKeyword(THREAT.into())],
                Quick::Due => vec![Filter::HasKeyword(DUE.into())],
                Quick::Waiting => {
                    std::iter::once(WAIT.to_owned()).chain(me.waiting.iter().take(WAITING_MAX).cloned()).map(Filter::HasKeyword).collect()
                }
                Quick::Expiring => vec![Filter::HasKeyword(EXPIRE.into())],
                Quick::ToMe => me.addresses.iter().map(|a| Filter::To(a.clone())).collect(),
            })
            .collect();
        if !quick.is_empty() {
            and.push(Filter::Or(quick));
        }
        match and.len() {
            0 => None,
            1 => and.pop(),
            _ => Some(Filter::And(and)),
        }
    }
}

// --- счётчики ---

/// Счётчики панели и закладок по области поиска. Число у фильтра — сколько писем будет, если включить его к
/// остальным: флажок — среди писем, прошедших все прочие фильтры; категория — флажки и закладки (категории
/// между собой — ИЛИ); закладка — фильтры панели (закладки между собой — ИЛИ).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub checks: [usize; 5],
    /// Ключевое слово → писем; считаются все слова писем, интерфейс берёт категории таблицы.
    pub categories: BTreeMap<String, usize>,
    pub quick: [usize; 5],
    /// Прошли все фильтры («N» в «N из M»).
    pub shown: usize,
    /// Всего в области поиска («M»).
    pub total: usize,
}

impl Counts {
    /// Учесть письмо; `true` — оно проходит все фильтры.
    pub fn add(&mut self, c: &Criteria, f: &Facts, keywords: &[String]) -> bool {
        self.total += 1;
        let failing = c.checks.iter().filter(|k| !f.check(**k)).count();
        let cats = c.categories_pass(keywords);
        let quick = c.quick_pass(f);
        // Письмо с признаком `k` сам `k` не проваливает: число у флажка — прошедшие все фильтры с этим признаком.
        for (i, k) in CHECKS.iter().enumerate() {
            if f.check(*k) && failing == 0 && cats && quick {
                self.checks[i] += 1;
            }
        }
        if failing == 0 && quick {
            for k in keywords {
                *self.categories.entry(k.clone()).or_default() += 1;
            }
        }
        if failing == 0 && cats {
            for (i, q) in QUICK.iter().enumerate() {
                if f.quick(*q) {
                    self.quick[i] += 1;
                }
            }
        }
        let pass = failing == 0 && cats && quick;
        self.shown += usize::from(pass);
        pass
    }

    pub fn check(&self, c: Check) -> usize {
        CHECKS.iter().position(|x| *x == c).map_or(0, |i| self.checks[i])
    }

    pub fn quick(&self, q: Quick) -> usize {
        QUICK.iter().position(|x| *x == q).map_or(0, |i| self.quick[i])
    }

    pub fn category(&self, keyword: &str) -> usize {
        self.categories.get(keyword).copied().unwrap_or(0)
    }
}

// --- сортировка и группы ---

/// Порядок списка (меню сортировки над списком).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Sort {
    /// Новые сверху; в одной папке — группы по датам.
    #[default]
    Date,
    /// По имени отправителя (А→Я), внутри — новые сверху.
    Sender,
    /// Крупные сверху.
    Size,
}

/// Группа по дате письма.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Day {
    Today,
    Yesterday,
    ThisWeek,
    LastWeek,
    Earlier,
}

/// Начала групп по датам (Unix-секунды) для «сейчас» в часовом поясе пользователя.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Days {
    today: i64,
    yesterday: i64,
    week: i64,
    last_week: i64,
}

impl Days {
    pub fn at(now: &jiff::Zoned) -> Days {
        let start = |d: jiff::civil::Date| d.to_zoned(now.time_zone().clone()).map_or(0, |z| z.timestamp().as_second());
        let date = now.date();
        let back = |n: i64| date.checked_sub(jiff::Span::new().days(n)).unwrap_or(date);
        let monday = back(i64::from(date.weekday().to_monday_zero_offset()));
        let last_monday = monday.checked_sub(jiff::Span::new().days(7)).unwrap_or(monday);
        Days { today: start(date), yesterday: start(back(1)), week: start(monday), last_week: start(last_monday) }
    }

    /// Сейчас, системный часовой пояс.
    pub fn now() -> Days {
        Days::at(&jiff::Zoned::now())
    }

    pub fn of(&self, ts: i64) -> Day {
        if ts >= self.today {
            Day::Today
        } else if ts >= self.yesterday {
            Day::Yesterday
        } else if ts >= self.week {
            Day::ThisWeek
        } else if ts >= self.last_week {
            Day::LastWeek
        } else {
            Day::Earlier
        }
    }

    /// Группы с началом (письма не раньше), новые первыми; «Ранее» — без начала. Пустые группы (неделя началась
    /// вчера) тоже здесь: их число писем — ноль.
    pub fn starts(&self) -> [(Day, i64); 4] {
        [
            (Day::Today, self.today),
            (Day::Yesterday, self.yesterday),
            (Day::ThisWeek, self.week.min(self.yesterday)),
            (Day::LastWeek, self.last_week.min(self.week).min(self.yesterday)),
        ]
    }

    /// Группы по датам папки из числа писем не раньше начала каждой группы (`count_since`) и всего; пустые — нет.
    pub fn groups(&self, total: usize, mut since: impl FnMut(i64) -> usize) -> Vec<(Group, usize)> {
        let mut out = Vec::new();
        let mut above = 0;
        for (day, start) in self.starts() {
            let n = since(start).min(total);
            if n > above {
                out.push((Group::Day(day), n - above));
            }
            above = above.max(n);
        }
        if total > above {
            out.push((Group::Day(Day::Earlier), total - above));
        }
        out
    }
}

/// Группа строк списка: дата (одна папка, по дате) или папка («Все папки»); в одной папке над ними — «Срочное»
/// (S5-04: просроченный срок, сработавшее напоминание — `crate::lifecycle::on_top`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Group {
    Top,
    Day(Day),
    Folder(String),
}

/// Письмо, прошедшее фильтры: что нужно для порядка и групп.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub id: String,
    pub received_at: i64,
    /// Имя отправителя (или адрес) в нижнем регистре.
    pub sender: String,
    pub size: u64,
    /// Номер папки в порядке дерева («Все папки»); в одной папке — 0.
    pub folder: usize,
    /// Наверху списка одной папки (группа [`Group::Top`]).
    pub top: bool,
}

impl Hit {
    pub fn of(row: &EmailRow, folder: usize) -> Hit {
        let sender = if row.from_name.is_empty() { &row.from_addr } else { &row.from_name };
        Hit { id: row.id.clone(), received_at: row.received_at, sender: sender.to_lowercase(), size: row.size, folder, top: false }
    }
}

/// Расставленный список: id по порядку и группы подряд с числом писем.
pub type Arranged = (Vec<String>, Vec<(Group, usize)>);

/// Расставить письма: порядок `sort`; «Все папки» (`folders` — id папок по номеру `Hit::folder`) — группы по папкам,
/// одна папка по дате — группы по датам, иначе без групп. В одной папке по дате письма `Hit::top` — первыми, группой
/// [`Group::Top`].
pub fn arrange(mut hits: Vec<Hit>, sort: Sort, folders: Option<&[String]>, days: &Days) -> Arranged {
    let by_sort = |a: &Hit, b: &Hit| {
        match sort {
            Sort::Date => b.received_at.cmp(&a.received_at),
            Sort::Sender => a.sender.cmp(&b.sender).then(b.received_at.cmp(&a.received_at)),
            Sort::Size => b.size.cmp(&a.size).then(b.received_at.cmp(&a.received_at)),
        }
        .then_with(|| a.id.cmp(&b.id))
    };
    hits.sort_by(|a, b| match folders {
        Some(_) => a.folder.cmp(&b.folder).then_with(|| by_sort(a, b)),
        None if sort == Sort::Date => b.top.cmp(&a.top).then_with(|| by_sort(a, b)),
        None => by_sort(a, b),
    });
    let key = |h: &Hit| -> Option<Group> {
        match (folders, sort) {
            (Some(f), _) => Some(Group::Folder(f.get(h.folder).cloned().unwrap_or_default())),
            (None, Sort::Date) if h.top => Some(Group::Top),
            (None, Sort::Date) => Some(Group::Day(days.of(h.received_at))),
            (None, _) => None,
        }
    };
    let mut groups: Vec<(Group, usize)> = Vec::new();
    for h in &hits {
        if let Some(g) = key(h) {
            match groups.last_mut() {
                Some((last, n)) if *last == g => *n += 1,
                _ => groups.push((g, 1)),
            }
        }
    }
    (hits.into_iter().map(|h| h.id).collect(), groups)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str) -> EmailRow {
        EmailRow {
            id: id.into(),
            thread_id: String::new(),
            mailbox_ids: vec!["in".into()],
            received_at: 0,
            subject: String::new(),
            from_name: String::new(),
            from_addr: "a@corp.example".into(),
            preview: String::new(),
            seen: true,
            flagged: false,
            has_attachment: false,
            size: 1,
            keywords: Vec::new(),
        }
    }

    fn me() -> Me {
        Me::new(["Me@Corp.Example"], ["Смирнова Анна"])
    }

    #[test]
    fn mention_is_an_at_handle_or_my_address() {
        let me = me();
        assert!(me.mentioned("@Анна, посмотри раздел рисков"));
        assert!(me.mentioned("Коллеги, @смирнова анна просила"));
        assert!(me.mentioned("cc @me — по задаче"));
        assert!(me.mentioned("пишите на me@corp.example"));
        // Обращение без `@`, чужое имя, слово, начинающееся с моего имени, цитата с адресом — не упоминания.
        assert!(!me.mentioned("Анна, добрый день!"));
        assert!(!me.mentioned("@Анатолий, привет"));
        assert!(!me.mentioned("@Аннушка, привет"));
        assert!(!me.mentioned("пн, 3 нояб., Смирнова Анна <me@corp.example>:"));
        assert!(!Me::default().mentioned("@анна"));
    }

    #[test]
    fn external_is_another_domain() {
        let me = me();
        assert!(!me.external("i.petrov@corp.example"));
        assert!(!me.external("noreply@gitlab.corp.example"), "поддомен — свой");
        assert!(me.external("support@vendor.example"));
        assert!(me.external("x@notcorp.example"), "не поддомен, а другой домен");
        assert!(!me.external(""), "без адреса");
        assert!(!Me::default().external("x@vendor.example"), "своих доменов ещё нет");
        let two = Me::new(["a@one.example", "b@two.example"], [""; 0]);
        assert!(!two.external("c@two.example") && two.external("c@three.example"));
    }

    #[test]
    fn to_me_is_my_address_in_to() {
        let me = me();
        let r =
            |to: &[&str], cc: &[&str]| Recipients { to: to.iter().map(|s| s.to_string()).collect(), cc: cc.iter().map(|s| s.to_string()).collect() };
        assert!(me.to_me(&r(&["team@corp.example", "me@corp.example"], &[])));
        assert!(!me.to_me(&r(&["team@corp.example"], &["me@corp.example"])), "копия — не «мне»");
    }

    #[test]
    fn lifecycle_and_threat_keywords() {
        let mut r = row("a");
        r.keywords = vec!["$phishing".into(), "pm.due.20261023".into(), "pm.waiting".into()];
        let f = Facts::of(&me(), &r, &Recipients::default());
        assert!(f.threat && f.due);
        assert!(!f.waiting, "pm.waiting — не уточнение pm.wait");
        r.keywords = vec!["pm.threat".into(), "pm.wait".into(), "pm.expire".into()];
        let f = Facts::of(&me(), &r, &Recipients::default());
        assert!(f.threat && f.waiting && f.expiring && !f.due);
    }

    fn facts(unread: bool, att: bool, threat: bool, to_me: bool) -> Facts {
        Facts { unread, attachment: att, threat, to_me, ..Facts::default() }
    }

    /// Закладки между собой — ИЛИ, с панелью — И; категории — ИЛИ между собой, И с флажками.
    #[test]
    fn quick_tabs_or_panel_and() {
        let mut c = Criteria::default();
        let none: &[String] = &[];
        assert!(c.matches(&facts(false, false, false, false), none), "без фильтров — всё");
        c.toggle_quick(Quick::Threats);
        c.toggle_quick(Quick::ToMe);
        assert!(c.matches(&facts(false, false, true, false), none));
        assert!(c.matches(&facts(false, false, false, true), none));
        assert!(!c.matches(&facts(true, true, false, false), none));
        c.toggle_check(Check::Unread);
        assert!(c.matches(&facts(true, false, false, true), none));
        assert!(!c.matches(&facts(false, false, true, true), none), "И с панелью");
        c.toggle_category("pm.cat.team");
        c.toggle_category("pm.cat.clients");
        let team = ["pm.cat.team".to_string()];
        let other = ["pm.cat.tenders".to_string()];
        assert!(c.matches(&facts(true, false, true, false), &team));
        assert!(!c.matches(&facts(true, false, true, false), &other));
        assert_eq!(c.panel_len(), 3, "закладки на воронке не считаются");
        c.clear_panel();
        assert_eq!(c.panel_len(), 0);
        assert_eq!(c.quick.len(), 2);
        c.toggle_quick(Quick::ToMe);
        assert_eq!(c.quick.len(), 1);
    }

    #[test]
    fn counts_show_what_each_filter_would_add() {
        let mut c = Criteria::default();
        c.toggle_check(Check::Unread);
        c.toggle_quick(Quick::Threats);
        let rows = [
            (facts(true, true, true, false), vec!["pm.cat.team".to_string()]),
            (facts(true, false, false, true), vec![]),
            (facts(false, true, true, false), vec!["pm.cat.team".to_string()]),
            (facts(false, false, false, false), vec![]),
        ];
        let mut n = Counts::default();
        let shown: Vec<bool> = rows.iter().map(|(f, k)| n.add(&c, f, k)).collect();
        assert_eq!(shown, [true, false, false, false]);
        assert_eq!((n.shown, n.total), (1, 4));
        // «Непрочитанные» — среди угроз: одно; «Вложения» — непрочитанные угрозы с вложением: одно.
        assert_eq!(n.check(Check::Unread), 1);
        assert_eq!(n.check(Check::Attachments), 1);
        // Закладки — среди непрочитанных, без учёта других закладок.
        assert_eq!(n.quick(Quick::Threats), 1);
        assert_eq!(n.quick(Quick::ToMe), 1);
        // Категория — непрочитанные угрозы.
        assert_eq!(n.category("pm.cat.team"), 1);
        // Без фильтров счётчики — просто число писем с признаком.
        let mut all = Counts::default();
        for (f, k) in &rows {
            all.add(&Criteria::default(), f, k);
        }
        assert_eq!((all.check(Check::Unread), all.check(Check::Attachments), all.quick(Quick::Threats)), (2, 2, 2));
        assert_eq!((all.category("pm.cat.team"), all.shown), (2, 4));
    }

    fn zoned(s: &str) -> jiff::Zoned {
        s.parse().unwrap()
    }

    #[test]
    fn day_groups() {
        // Четверг, 22 октября 2026, 15:00 UTC.
        let now = zoned("2026-10-22T15:00:00+00:00[UTC]");
        let d = Days::at(&now);
        let ts = |s: &str| zoned(s).timestamp().as_second();
        assert_eq!(d.of(ts("2026-10-22T00:00:00+00:00[UTC]")), Day::Today);
        assert_eq!(d.of(ts("2026-10-21T23:59:59+00:00[UTC]")), Day::Yesterday);
        assert_eq!(d.of(ts("2026-10-19T08:00:00+00:00[UTC]")), Day::ThisWeek);
        assert_eq!(d.of(ts("2026-10-12T00:00:00+00:00[UTC]")), Day::LastWeek);
        assert_eq!(d.of(ts("2026-10-11T23:00:00+00:00[UTC]")), Day::Earlier);
        // Понедельник: «На этой неделе» пуста (вчера — уже прошлая неделя).
        let monday = Days::at(&zoned("2026-10-19T10:00:00+00:00[UTC]"));
        assert_eq!(monday.of(ts("2026-10-18T10:00:00+00:00[UTC]")), Day::Yesterday);
        assert_eq!(monday.of(ts("2026-10-17T10:00:00+00:00[UTC]")), Day::LastWeek);
        // Группы папки по `count_since`: 2 сегодня, 1 вчера, 0 на неделе, 3 на прошлой, 4 ранее.
        let dates = [ts("2026-10-22T10:00:00+00:00[UTC]"), ts("2026-10-22T09:00:00+00:00[UTC]"), ts("2026-10-21T09:00:00+00:00[UTC]")]
            .into_iter()
            .chain([ts("2026-10-15T09:00:00+00:00[UTC]"); 3])
            .chain([ts("2026-01-01T09:00:00+00:00[UTC]"); 4])
            .collect::<Vec<_>>();
        let since = |t: i64| dates.iter().filter(|&&x| x >= t).count();
        assert_eq!(
            d.groups(dates.len(), since),
            [(Group::Day(Day::Today), 2), (Group::Day(Day::Yesterday), 1), (Group::Day(Day::LastWeek), 3), (Group::Day(Day::Earlier), 4),]
        );
    }

    #[test]
    fn arrange_groups_by_folder_or_date() {
        let d = Days::at(&zoned("2026-10-22T15:00:00+00:00[UTC]"));
        let today = zoned("2026-10-22T10:00:00+00:00[UTC]").timestamp().as_second();
        let hit = |id: &str, at: i64, sender: &str, size: u64, folder: usize| Hit {
            id: id.into(),
            received_at: at,
            sender: sender.into(),
            size,
            folder,
            top: false,
        };
        let hits = vec![
            hit("a", today - 86_400 * 30, "борис", 10, 1),
            hit("b", today, "анна", 5, 0),
            hit("c", today - 3600, "яков", 50, 1),
            hit("d", today - 86_400, "анна", 1, 0),
        ];
        let folders = ["in".to_string(), "archive".to_string()];
        let (ids, groups) = arrange(hits.clone(), Sort::Date, Some(&folders), &d);
        assert_eq!(ids, ["b", "d", "c", "a"]);
        assert_eq!(groups, [(Group::Folder("in".into()), 2), (Group::Folder("archive".into()), 2)]);
        let (ids, groups) = arrange(hits.clone(), Sort::Date, None, &d);
        assert_eq!(ids, ["b", "c", "d", "a"]);
        assert_eq!(groups, [(Group::Day(Day::Today), 2), (Group::Day(Day::Yesterday), 1), (Group::Day(Day::Earlier), 1)]);
        let (ids, groups) = arrange(hits.clone(), Sort::Sender, None, &d);
        assert_eq!((ids, groups), (vec!["b".to_string(), "d".into(), "a".into(), "c".into()], vec![]));
        let (ids, _) = arrange(hits.clone(), Sort::Size, None, &d);
        assert_eq!(ids, ["c", "a", "b", "d"]);
        // «Срочное» (S5-04) — первым, своей группой; в «Все папки» — без неё.
        let mut pinned = hits;
        pinned[0].top = true;
        let (ids, groups) = arrange(pinned.clone(), Sort::Date, None, &d);
        assert_eq!(ids, ["a", "b", "c", "d"]);
        assert_eq!(groups, [(Group::Top, 1), (Group::Day(Day::Today), 2), (Group::Day(Day::Yesterday), 1)]);
        let (ids, groups) = arrange(pinned.clone(), Sort::Size, None, &d);
        assert_eq!((ids, groups), (vec!["c".to_string(), "a".into(), "b".into(), "d".into()], vec![]), "не по дате — без «Срочного»");
        let (_, groups) = arrange(pinned, Sort::Date, Some(&folders), &d);
        assert!(!groups.iter().any(|(g, _)| *g == Group::Top));
    }

    /// Закладки по настройкам (S5-09): тот же порядок по срочности; скрытая полоса — без закладок.
    #[test]
    fn prefs_quick_maps_to_filters() {
        let order: Vec<Quick> = crate::prefs::Quick::ALL.iter().map(|&q| q.into()).collect();
        assert_eq!(order, QUICK);
        let mut p = crate::prefs::QuickFilters::default();
        p.toggle(crate::prefs::Quick::Mine);
        assert_eq!(p.tabs().map(Quick::from).collect::<Vec<_>>(), [Quick::Threats, Quick::Due, Quick::Waiting, Quick::Expiring]);
        p.hidden = true;
        assert_eq!(p.tabs().count(), 0);
        assert_eq!(p.shown().count(), 4, "выбор закладок остаётся");
    }

    #[test]
    fn server_filter_mirrors_the_criteria() {
        let me = me();
        assert_eq!(Criteria::default().server_filter(&me), None);
        let mut c = Criteria::default();
        c.toggle_check(Check::Unread);
        assert_eq!(c.server_filter(&me), Some(Filter::NotKeyword("$seen".into())));
        c.toggle_check(Check::External);
        c.toggle_quick(Quick::ToMe);
        c.toggle_quick(Quick::Due);
        assert_eq!(
            c.server_filter(&me),
            Some(Filter::And(vec![
                Filter::NotKeyword("$seen".into()),
                Filter::Not(vec![Filter::From("@corp.example".into())]),
                Filter::Or(vec![Filter::HasKeyword(DUE.into()), Filter::To("me@corp.example".into())]),
            ]))
        );
        // Без моих адресов — условия, которых не проверить, не ставятся.
        let mut m = Criteria::default();
        m.toggle_check(Check::Mentions);
        assert_eq!(m.server_filter(&Me::default()), None);
        assert!(matches!(m.server_filter(&me), Some(Filter::Or(list)) if list.contains(&Filter::Body("@анна".into()))));
    }
}
