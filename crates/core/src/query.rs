//! Запрос поиска в командной строке (S5-10, `docs/DESIGN.md` «Палитра команд и поиск»): слова и условия
//! `from:`, `to:`, `subject:`, `has:attachment`, `is:unread`, `is:flagged`, `before:`/`after:`, `category:`,
//! `larger:` (размер, S5-29) —
//! разбор ([`Query::parse`]), проверка письма по кэшу ([`Query::matches`]), условие `Email/query` для «Везде»
//! ([`Query::server_filter`]) и чипы «Уточнить» ([`Refine`], [`suggest`]).
//!
//! Ключи — без регистра, есть русские: `от:`, `кому:`, `тема:`, `есть:вложения`, `до:`, `после:`, `категория:`,
//! `больше:`. Размер — `5M`, `500K`, `1G` (`5М`, `500К`, `1Г`, можно `МБ`), без единицы — байты.
//! Значение с пробелами — в кавычках (`from:"Анна Смирнова"`). Даты: `2026-10-21`, `21.10.2026` или «назад»:
//! `7d`, `2w`, `3m`, `1y` (`7д`, `2н`, `3м`, `1г`). Неизвестный ключ (`Re:`) — просто слово; ключ без значения
//! (запрос ещё набирается) и непонятное значение — без условия.

use crate::model::Filter;
use postman_cache::{EmailRow, FLAGGED, Recipients, SEEN};

use crate::fuzzy::{folded, highlight_terms};

/// Дата условия `before:`/`after:`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum When {
    Date(jiff::civil::Date),
    /// Столько дней назад (начало того дня).
    Ago(u32),
}

impl When {
    fn parse(v: &str) -> Option<When> {
        let v = v.trim();
        if let Ok(d) = v.parse::<jiff::civil::Date>() {
            return Some(When::Date(d));
        }
        let parts: Vec<&str> = v.split('.').collect();
        if let [d, m, y] = parts.as_slice() {
            let (d, m, y) = (d.parse().ok()?, m.parse().ok()?, y.parse().ok()?);
            return jiff::civil::Date::new(y, m, d).ok().map(When::Date);
        }
        let unit = v.chars().last()?;
        let n: u32 = v[..v.len() - unit.len_utf8()].parse().ok()?;
        let days = match crate::fuzzy::fold(unit) {
            'd' | 'д' => 1,
            'w' | 'н' => 7,
            'm' | 'м' => 30,
            'y' | 'г' => 365,
            _ => return None,
        };
        Some(When::Ago(n.saturating_mul(days)))
    }

    /// Начало дня (Unix-секунды) в часовом поясе `now`.
    pub fn start(&self, now: &jiff::Zoned) -> Option<i64> {
        let date = match self {
            When::Date(d) => *d,
            When::Ago(n) => now.date().checked_sub(jiff::Span::new().days(i64::from(*n))).ok()?,
        };
        date.to_zoned(now.time_zone().clone()).ok().map(|z| z.timestamp().as_second())
    }
}

/// Разобранный запрос.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    /// Слова для полнотекстового поиска (фраза в кавычках — одним словом).
    pub words: Vec<String>,
    pub from: Vec<String>,
    pub to: Vec<String>,
    pub subject: Vec<String>,
    pub attachment: bool,
    pub unread: bool,
    pub flagged: bool,
    pub after: Option<When>,
    pub before: Option<When>,
    /// Имена категорий (как в таблице категорий).
    pub categories: Vec<String>,
    /// Не меньше этого размера, байт.
    pub larger: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    From,
    To,
    Subject,
    Has,
    Is,
    Before,
    After,
    Category,
    Larger,
}

fn key(k: &str) -> Option<Key> {
    Some(match folded(k).as_str() {
        "from" | "от" => Key::From,
        "to" | "кому" => Key::To,
        "subject" | "тема" => Key::Subject,
        "has" | "есть" => Key::Has,
        "is" => Key::Is,
        "before" | "до" => Key::Before,
        "after" | "после" => Key::After,
        "category" | "cat" | "категория" => Key::Category,
        "larger" | "size" | "больше" | "размер" => Key::Larger,
        _ => return None,
    })
}

/// Кусок строки запроса: исходный текст, ключ (если известный) и значение без кавычек.
struct Token<'a> {
    raw: &'a str,
    key: Option<Key>,
    value: String,
}

/// Размер `5M`, `1.5G`, `500К`, `5МБ`; без единицы — байты.
pub fn parse_size(v: &str) -> Option<u64> {
    let v = folded(v.trim());
    let split = v.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ',')).unwrap_or(v.len());
    let (num, unit) = v.split_at(split);
    let n: f64 = num.replace(',', ".").parse().ok()?;
    let mult: f64 = match unit.trim() {
        "" | "b" | "б" => 1.0,
        "k" | "kb" | "к" | "кб" => 1024.0,
        "m" | "mb" | "м" | "мб" => 1024.0 * 1024.0,
        "g" | "gb" | "г" | "гб" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    let bytes = n * mult;
    (bytes.is_finite() && bytes >= 0.0).then_some(bytes as u64)
}

fn unquote(s: &str) -> String {
    s.trim_matches('"').to_owned()
}

/// Куски строки по пробелам; в кавычках пробел — часть куска.
fn tokens(s: &str) -> Vec<Token<'_>> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut quoted = false;
    for (i, c) in s.char_indices().chain(std::iter::once((s.len(), ' '))) {
        if c == '"' {
            quoted = !quoted;
        }
        match (start, c.is_whitespace() && !quoted) {
            (None, false) => start = Some(i),
            (Some(st), true) => {
                out.push(token(&s[st..i]));
                start = None;
            }
            _ => {}
        }
    }
    out
}

fn token(raw: &str) -> Token<'_> {
    if let Some((k, v)) = raw.split_once(':')
        && let Some(key) = key(k)
    {
        return Token { raw, key: Some(key), value: unquote(v) };
    }
    Token { raw, key: None, value: unquote(raw) }
}

fn is_attachment(v: &str) -> bool {
    matches!(folded(v).as_str(), "attachment" | "attachments" | "attach" | "вложение" | "вложения" | "вложением")
}

fn is_unread(v: &str) -> bool {
    matches!(folded(v).as_str(), "unread" | "new" | "непрочитанные" | "непрочитанное" | "непрочитано")
}

fn is_flagged(v: &str) -> bool {
    matches!(folded(v).as_str(), "flagged" | "starred" | "флаг" | "флажок")
}

/// `to:me` — мне.
fn is_me(v: &str) -> bool {
    matches!(folded(v).as_str(), "me" | "мне" | "я")
}

/// Значение в запрос: с пробелами — в кавычках.
fn quote(v: &str) -> String {
    if v.chars().any(char::is_whitespace) { format!("\"{v}\"") } else { v.to_owned() }
}

/// Полнотекстовое условие с языком для Stalwart: с кириллицей — `ru:<текст>`, остальное — как есть. Язык запроса
/// Stalwart угадывает по самому запросу; у короткого запроса уверенность мала, и он берёт язык по умолчанию
/// (`Search.defaultLanguage`, на стенде en-US) — тогда русских словоформ нет (`docs/STALWART-GAPS.md`, 33).
/// Префикс языка — синтаксис запроса Stalwart: `ru:договоры` находит «договор» и «договору».
pub fn with_language(term: &str) -> String {
    let cyrillic = term.chars().any(|c| matches!(c, '\u{0400}'..='\u{052F}'));
    if cyrillic { format!("ru:{term}") } else { term.to_owned() }
}

fn contains(hay: &str, needle: &str) -> bool {
    folded(hay).contains(&folded(needle))
}

/// Условия, которые зависят от «сейчас», таблицы категорий и моих адресов.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Env {
    pub after: Option<i64>,
    pub before: Option<i64>,
    /// Ключевые слова категорий запроса.
    pub categories: Vec<String>,
    /// Мои адреса (`to:me`), в нижнем регистре.
    pub me: Vec<String>,
}

impl Query {
    pub fn parse(input: &str) -> Query {
        let mut q = Query::default();
        for t in tokens(input) {
            let v = t.value.trim();
            match t.key {
                None if !v.is_empty() => q.words.push(v.to_owned()),
                None => {}
                // Ключ без значения: запрос ещё набирается.
                Some(_) if v.is_empty() => {}
                Some(Key::From) => q.from.push(v.to_owned()),
                Some(Key::To) => q.to.push(v.to_owned()),
                Some(Key::Subject) => q.subject.push(v.to_owned()),
                Some(Key::Has) => q.attachment |= is_attachment(v),
                Some(Key::Is) => {
                    q.unread |= is_unread(v);
                    q.flagged |= is_flagged(v);
                }
                Some(Key::Before) => q.before = When::parse(v).or(q.before),
                Some(Key::After) => q.after = When::parse(v).or(q.after),
                Some(Key::Category) => q.categories.push(v.to_owned()),
                Some(Key::Larger) => q.larger = parse_size(v).or(q.larger),
            }
        }
        q
    }

    /// Слова для полнотекстового поиска одной строкой.
    pub fn text(&self) -> String {
        self.words.join(" ")
    }

    /// Есть условия кроме слов.
    pub fn has_conditions(&self) -> bool {
        !(self.from.is_empty() && self.to.is_empty() && self.subject.is_empty() && self.categories.is_empty())
            || self.attachment
            || self.unread
            || self.flagged
            || self.after.is_some()
            || self.before.is_some()
            || self.larger.is_some()
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty() && !self.has_conditions()
    }

    /// Даты, категории (имя → ключевое слово по таблице; нет такой — имя, оно ни у кого не совпадёт) и мои адреса.
    pub fn env(&self, now: &jiff::Zoned, category: impl Fn(&str) -> Option<String>, me: &[String]) -> Env {
        Env {
            after: self.after.and_then(|w| w.start(now)),
            before: self.before.and_then(|w| w.start(now)),
            categories: self.categories.iter().map(|n| category(n).unwrap_or_else(|| folded(n))).collect(),
            me: me.iter().map(|a| a.to_lowercase()).collect(),
        }
    }

    /// Письмо проходит условия запроса (слова проверяет поиск — индекс или сервер).
    pub fn matches(&self, env: &Env, row: &EmailRow, to: &Recipients) -> bool {
        let from = format!("{} {}", row.from_name, row.from_addr);
        let to_me = || to.to.iter().chain(&to.cc).any(|a| env.me.iter().any(|m| a.eq_ignore_ascii_case(m)));
        env.after.is_none_or(|t| row.received_at >= t)
            && env.before.is_none_or(|t| row.received_at < t)
            && (!self.attachment || row.has_attachment)
            && self.larger.is_none_or(|n| row.size >= n)
            && (!self.unread || !row.seen)
            && (!self.flagged || row.flagged)
            && self.from.iter().all(|v| contains(&from, v))
            && self.subject.iter().all(|v| contains(&row.subject, v))
            && self.to.iter().all(|v| if is_me(v) { to_me() } else { to.to.iter().chain(&to.cc).any(|a| contains(a, v)) })
            && env.categories.iter().all(|k| row.keywords.iter().any(|x| x.eq_ignore_ascii_case(k)))
    }

    /// Условие `Email/query` (RFC 8621 §4.4.1); пустой запрос — `None`. `to:me` без моих адресов — без условия.
    /// Слова — каждое своим `text`: одним `text` из нескольких слов Stalwart ищет все слова в одном поле (тема или
    /// текст), слово из темы и слово из текста письма вместе не находятся (`docs/STALWART-GAPS.md`). Слова и тема
    /// по-русски — с языком запроса ([`with_language`]), иначе сервер не находит словоформ.
    pub fn server_filter(&self, env: &Env) -> Option<Filter> {
        let mut and = Vec::new();
        and.extend(self.words.iter().map(|w| Filter::Text(with_language(w))));
        and.extend(self.from.iter().map(|v| Filter::From(v.clone())));
        for v in &self.to {
            match (is_me(v), env.me.as_slice()) {
                (true, []) => {}
                (true, [one]) => and.push(Filter::To(one.clone())),
                (true, many) => and.push(Filter::Or(many.iter().map(|a| Filter::To(a.clone())).collect())),
                (false, _) => and.push(Filter::To(v.clone())),
            }
        }
        and.extend(self.subject.iter().map(|v| Filter::Subject(with_language(v))));
        if self.attachment {
            and.push(Filter::HasAttachment(true));
        }
        if self.unread {
            and.push(Filter::NotKeyword(SEEN.into()));
        }
        if self.flagged {
            and.push(Filter::HasKeyword(FLAGGED.into()));
        }
        if let Some(t) = env.after {
            and.push(Filter::After(crate::schedule::utc(t)));
        }
        if let Some(t) = env.before {
            and.push(Filter::Before(crate::schedule::utc(t)));
        }
        and.extend(env.categories.iter().map(|k| Filter::HasKeyword(k.clone())));
        if let Some(n) = self.larger {
            and.push(Filter::MinSize(n));
        }
        match and.len() {
            0 => None,
            1 => and.pop(),
            _ => Some(Filter::And(and)),
        }
    }

    /// Что подсветить в письме: слова запроса, отправитель и тема из условий (и в другой раскладке).
    pub fn marks(&self) -> Vec<String> {
        let all: Vec<&String> = self.words.iter().chain(&self.from).chain(&self.subject).collect();
        highlight_terms(&all)
    }
}

// --- чипы «Уточнить» ---

/// Чип «Уточнить»: условие, которое чип ставит в строку поиска или снимает.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refine {
    /// От этого отправителя (имя или адрес).
    From(String),
    /// Мне (`to:me`).
    ToMe,
    Attachment,
    Unread,
    /// За последние N дней (`after:Nd`).
    Within(u32),
    /// Категория (имя).
    Category(String),
}

impl Refine {
    /// Условие в записи запроса.
    pub fn token(&self) -> String {
        match self {
            Refine::From(v) => format!("from:{}", quote(v)),
            Refine::ToMe => "to:me".into(),
            Refine::Attachment => "has:attachment".into(),
            Refine::Unread => "is:unread".into(),
            Refine::Within(d) => format!("after:{d}d"),
            Refine::Category(n) => format!("category:{}", quote(n)),
        }
    }

    /// Условие уже в запросе.
    pub fn active(&self, q: &Query) -> bool {
        let has = |list: &[String], v: &str| list.iter().any(|x| folded(x) == folded(v));
        match self {
            Refine::From(v) => has(&q.from, v),
            Refine::ToMe => q.to.iter().any(|v| is_me(v)),
            Refine::Attachment => q.attachment,
            Refine::Unread => q.unread,
            Refine::Within(d) => q.after == Some(When::Ago(*d)),
            Refine::Category(n) => has(&q.categories, n),
        }
    }

    /// Нажатие на чип: условие есть — снять, нет — поставить (другой срок «за N дней» заменяется). Остальной
    /// запрос — как был.
    pub fn apply(&self, input: &str) -> String {
        let on = self.active(&Query::parse(input));
        let same = |t: &Token<'_>| {
            let v = t.value.trim();
            match (self, t.key) {
                (Refine::From(x), Some(Key::From)) | (Refine::Category(x), Some(Key::Category)) => folded(v) == folded(x),
                (Refine::ToMe, Some(Key::To)) => is_me(v),
                (Refine::Attachment, Some(Key::Has)) => is_attachment(v),
                (Refine::Unread, Some(Key::Is)) => is_unread(v),
                (Refine::Within(_), Some(Key::After)) => true,
                _ => false,
            }
        };
        let mut kept: Vec<String> = tokens(input).iter().filter(|t| !same(t)).map(|t| t.raw.to_owned()).collect();
        if !on {
            kept.push(self.token());
        }
        kept.join(" ")
    }
}

/// Сроки чипа «за N дней» по предпочтению: один чип — первый, что сужает найденное (место в строке чипов).
pub const WITHIN: [u32; 2] = [30, 7];

/// Чипы «Уточнить» по найденному: включённые условия — всегда; остальные — если сужают найденное (подходит
/// часть писем, но не все). Отправитель и категория — самые частые среди найденного. `categories` — таблица
/// (ключевое слово, имя); `me` — мои адреса.
pub fn suggest(q: &Query, found: &[(EmailRow, Recipients)], now: &jiff::Zoned, categories: &[(String, String)], me: &[String]) -> Vec<Refine> {
    let n = found.len();
    let narrows = |k: usize| k > 0 && k < n;
    let count = |f: &dyn Fn(&EmailRow, &Recipients) -> bool| found.iter().filter(|(r, t)| f(r, t)).count();
    let top = |key: &dyn Fn(&EmailRow) -> Vec<String>| -> Option<(String, usize)> {
        let mut seen: Vec<(String, usize)> = Vec::new();
        for (r, _) in found {
            for k in key(r) {
                match seen.iter_mut().find(|(x, _)| *x == k) {
                    Some((_, c)) => *c += 1,
                    None => seen.push((k, 1)),
                }
            }
        }
        // Самый частый; при равенстве — встретившийся раньше (найденное — по релевантности или дате).
        seen.into_iter().fold(None, |best: Option<(String, usize)>, x| match best {
            Some(b) if b.1 >= x.1 => Some(b),
            _ => Some(x),
        })
    };
    let mut out = Vec::new();
    let sender = |r: &EmailRow| if r.from_name.is_empty() { r.from_addr.clone() } else { r.from_name.clone() };
    match q.from.first() {
        Some(v) => out.push(Refine::From(v.clone())),
        None => {
            if let Some((name, k)) = top(&|r| vec![sender(r)]).filter(|(s, _)| !s.is_empty())
                && narrows(k)
            {
                out.push(Refine::From(name));
            }
        }
    }
    let mine: Vec<String> = me.iter().map(|a| a.to_lowercase()).collect();
    let to_me = count(&|_, t| t.to.iter().chain(&t.cc).any(|a| mine.iter().any(|m| a.eq_ignore_ascii_case(m))));
    let simple = [(Refine::ToMe, to_me), (Refine::Attachment, count(&|r, _| r.has_attachment)), (Refine::Unread, count(&|r, _| !r.seen))];
    for (r, k) in simple {
        if r.active(q) || narrows(k) {
            out.push(r);
        }
    }
    // Срок: включённый — его чип; иначе — первый из `WITHIN`, что сужает.
    match q.after {
        Some(When::Ago(d)) => out.push(Refine::Within(d)),
        _ => {
            let within = |d: u32| {
                let since = When::Ago(d).start(now).unwrap_or(i64::MIN);
                count(&|row, _| row.received_at >= since)
            };
            if let Some(d) = WITHIN.into_iter().find(|&d| narrows(within(d))) {
                out.push(Refine::Within(d));
            }
        }
    }
    if q.categories.is_empty() {
        let name = |kw: &str| categories.iter().find(|(k, _)| k.eq_ignore_ascii_case(kw)).map(|(_, n)| n.clone());
        if let Some((cat, k)) = top(&|r| r.keywords.iter().filter_map(|k| name(k)).collect())
            && narrows(k)
        {
            out.push(Refine::Category(cat));
        }
    } else {
        out.extend(q.categories.iter().map(|c| Refine::Category(c.clone())));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(from: &str, subject: &str) -> EmailRow {
        EmailRow {
            id: subject.into(),
            thread_id: String::new(),
            mailbox_ids: vec!["inbox".into()],
            received_at: 0,
            subject: subject.into(),
            from_name: from.into(),
            from_addr: format!("{}@corp.example", from.to_lowercase().replace(' ', ".")),
            preview: String::new(),
            seen: true,
            flagged: false,
            has_attachment: false,
            size: 0,
            keywords: Vec::new(),
        }
    }

    fn now() -> jiff::Zoned {
        jiff::civil::date(2026, 10, 21).at(12, 0, 0, 0).to_zoned(jiff::tz::TimeZone::UTC).unwrap()
    }

    #[test]
    fn parses_words_and_conditions() {
        let q = Query::parse(
            r#"договор from:"Ирина Смирнова" to:me subject:ИБ has:attachment is:unread after:30d before:2026-10-01 "точная фраза" category:Клиенты"#,
        );
        assert_eq!(q.words, ["договор", "точная фраза"]);
        assert_eq!(q.from, ["Ирина Смирнова"]);
        assert_eq!(q.to, ["me"]);
        assert_eq!(q.subject, ["ИБ"]);
        assert!(q.attachment && q.unread && !q.flagged);
        assert_eq!(q.after, Some(When::Ago(30)));
        assert_eq!(q.before, Some(When::Date(jiff::civil::date(2026, 10, 1))));
        assert_eq!(q.categories, ["Клиенты"]);
        assert_eq!(q.text(), "договор точная фраза");
        // Русские ключи, ключ без значения (ещё набирается), неизвестный ключ — слово.
        let q = Query::parse("Re: от:Пётр есть:вложения после:21.09.2026 до:2н is:flagged кому:");
        assert_eq!(q.words, ["Re:"]);
        assert_eq!(q.from, ["Пётр"]);
        assert!(q.attachment && q.flagged && q.to.is_empty());
        assert_eq!(q.after, Some(When::Date(jiff::civil::date(2026, 9, 21))));
        assert_eq!(q.before, Some(When::Ago(14)));
        // Непонятная дата — без условия; пустая строка — пустой запрос.
        assert_eq!(Query::parse("after:вчера").after, None);
        assert!(Query::parse("   ").is_empty());
        let q = Query::parse("from:x");
        assert!(q.words.is_empty() && q.has_conditions() && !q.is_empty());
    }

    #[test]
    fn dates_resolve_to_day_starts() {
        let now = now();
        let day = |y, m, d| jiff::civil::date(y, m, d).to_zoned(jiff::tz::TimeZone::UTC).unwrap().timestamp().as_second();
        assert_eq!(When::Ago(1).start(&now), Some(day(2026, 10, 20)));
        assert_eq!(When::parse("1m").and_then(|w| w.start(&now)), Some(day(2026, 9, 21)));
        assert_eq!(When::parse("2026-10-01").and_then(|w| w.start(&now)), Some(day(2026, 10, 1)));
    }

    #[test]
    fn rows_are_checked_against_conditions() {
        let now = now();
        let mut r = row("Ирина Смирнова", "Договор ИБ");
        r.received_at = now.timestamp().as_second() - 86_400;
        r.has_attachment = true;
        r.keywords = vec!["pm.cat.clients".into()];
        let to = Recipients { to: vec!["me@corp.example".into()], cc: vec![] };
        let cat = |n: &str| (folded(n) == "клиенты").then(|| "pm.cat.clients".to_string());
        let me = ["Me@Corp.example".to_string()];
        let check = |s: &str| {
            let q = Query::parse(s);
            q.matches(&q.env(&now, cat, &me), &r, &to)
        };
        assert!(check("from:смирнова subject:договор has:attachment to:me after:7d category:клиенты"));
        assert!(check("from:@corp.example"));
        assert!(!check("is:unread"));
        assert!(!check("from:пётр"));
        assert!(!check("before:3d"));
        assert!(!check("category:тендеры"));
        assert!(!check("to:boss"));
        assert!(check("to:me@corp"));
    }

    #[test]
    fn server_filter_maps_every_condition() {
        let now = now();
        let q = Query::parse("договор from:Ирина to:me subject:ИБ has:attachment is:unread is:flagged after:2026-10-01 category:Клиенты");
        let env = q.env(&now, |_| Some("pm.cat.clients".into()), &["me@corp.example".into(), "boss@corp.example".into()]);
        assert_eq!(
            q.server_filter(&env),
            Some(Filter::And(vec![
                Filter::Text("ru:договор".into()),
                Filter::From("Ирина".into()),
                Filter::Or(vec![Filter::To("me@corp.example".into()), Filter::To("boss@corp.example".into())]),
                Filter::Subject("ru:ИБ".into()),
                Filter::HasAttachment(true),
                Filter::NotKeyword("$seen".into()),
                Filter::HasKeyword("$flagged".into()),
                Filter::After("2026-10-01T00:00:00Z".into()),
                Filter::HasKeyword("pm.cat.clients".into()),
            ]))
        );
        let q = Query::parse("отчёт");
        assert_eq!(q.server_filter(&q.env(&now, |_| None, &[])), Some(Filter::Text("ru:отчёт".into())));
        // Слова — по одному `text` (слово темы и слово текста вместе), фраза в кавычках — одним.
        let q = Query::parse(r#"итоги квартала "точная фраза""#);
        assert_eq!(
            q.server_filter(&q.env(&now, |_| None, &[])),
            Some(Filter::And(vec![Filter::Text("ru:итоги".into()), Filter::Text("ru:квартала".into()), Filter::Text("ru:точная фраза".into()),]))
        );
        // Латиница — без языка; слово, где есть кириллица, — по-русски.
        let q = Query::parse("report subject:Q3 ИБ-2026");
        assert_eq!(
            q.server_filter(&q.env(&now, |_| None, &[])),
            Some(Filter::And(vec![Filter::Text("report".into()), Filter::Text("ru:ИБ-2026".into()), Filter::Subject("Q3".into()),]))
        );
        assert_eq!(Query::parse("").server_filter(&Env::default()), None);
        // `to:me` до того, как известны мои адреса, — без условия.
        assert_eq!(Query::parse("to:me").server_filter(&Env::default()), None);
    }

    #[test]
    fn chips_toggle_their_condition() {
        assert_eq!(Refine::Attachment.apply("договор"), "договор has:attachment");
        assert_eq!(Refine::Attachment.apply("договор has:attachment"), "договор");
        assert_eq!(Refine::Attachment.apply("есть:вложения договор"), "договор");
        assert_eq!(Refine::From("Ирина Смирнова".into()).apply("договор"), r#"договор from:"Ирина Смирнова""#);
        assert_eq!(Refine::From("Ирина Смирнова".into()).apply(r#"from:"ирина смирнова" договор"#), "договор");
        // Другой срок — заменяется.
        assert_eq!(Refine::Within(30).apply("договор after:7d"), "договор after:30d");
        assert_eq!(Refine::Within(30).apply("договор after:30d"), "договор");
        assert_eq!(Refine::Unread.apply(""), "is:unread");
        assert!(Refine::ToMe.active(&Query::parse("кому:мне")));
        assert!(Refine::Category("Клиенты".into()).active(&Query::parse("category:клиенты")));
    }

    #[test]
    fn suggestions_narrow_the_results() {
        let now = now();
        let ts = now.timestamp().as_second();
        let mut a = row("Ирина Смирнова", "Договор 1");
        a.received_at = ts - 3 * 86_400;
        a.has_attachment = true;
        a.keywords = vec!["pm.cat.clients".into()];
        let mut b = row("Ирина Смирнова", "Договор 2");
        b.received_at = ts - 20 * 86_400;
        b.seen = false;
        b.keywords = vec!["pm.cat.clients".into()];
        let mut c = row("Пётр Иванов", "Договор 3");
        c.received_at = ts - 90 * 86_400;
        let to = |me: bool| Recipients { to: if me { vec!["me@corp.example".into()] } else { vec![] }, cc: vec![] };
        let found = vec![(a, to(true)), (b, to(false)), (c, to(false))];
        let cats = vec![("pm.cat.clients".to_string(), "Клиенты".to_string())];
        let me = ["me@corp.example".to_string()];
        let q = Query::parse("договор");
        assert_eq!(
            suggest(&q, &found, &now, &cats, &me),
            vec![
                Refine::From("Ирина Смирнова".into()),
                Refine::ToMe,
                Refine::Attachment,
                Refine::Unread,
                Refine::Within(30),
                Refine::Category("Клиенты".into()),
            ]
        );
        // За 30 дней — все: чип срока — 7 дней; включённый срок — его чип.
        let recent = vec![found[0].clone(), found[1].clone()];
        let chips = suggest(&q, &recent, &now, &cats, &me);
        assert!(chips.contains(&Refine::Within(7)) && !chips.contains(&Refine::Within(30)));
        let chips = suggest(&Query::parse("договор after:30d"), &recent, &now, &cats, &me);
        assert!(chips.contains(&Refine::Within(30)) && !chips.contains(&Refine::Within(7)));
        // Включённое условие — остаётся чипом, даже если уже не сужает; не сужающее — пропадает.
        let only = vec![found[0].clone()];
        let q = Query::parse("договор has:attachment");
        assert_eq!(suggest(&q, &only, &now, &cats, &me), vec![Refine::Attachment]);
        assert!(suggest(&Query::parse("x"), &[], &now, &cats, &me).is_empty());
    }

    #[test]
    fn marks_cover_words_and_conditions() {
        let q = Query::parse("договор from:Смирнова is:unread");
        assert_eq!(q.marks()[0], "договор");
        assert!(q.marks().contains(&"смирнова".to_string()));
    }

    /// Размер (S5-29, «Очистить вложения…»): `larger:5M` — условие сервера `minSize` и проверка по кэшу.
    #[test]
    fn larger_than_size() {
        const MB: u64 = 1024 * 1024;
        assert_eq!(parse_size("5M"), Some(5 * MB));
        assert_eq!(parse_size("5мб"), Some(5 * MB));
        assert_eq!(parse_size("1,5G"), Some(3 * 512 * MB));
        assert_eq!(parse_size("500K"), Some(500 * 1024));
        assert_eq!(parse_size("700"), Some(700));
        assert_eq!(parse_size("много"), None);
        assert_eq!(parse_size("5X"), None);
        let q = Query::parse("has:attachment larger:5M before:1y");
        assert_eq!((q.larger, q.attachment, q.before), (Some(5 * MB), true, Some(When::Ago(365))));
        assert!(q.has_conditions());
        assert_eq!(Query::parse("больше:2мб").larger, Some(2 * MB));
        let env = q.env(&now(), |_| None, &[]);
        let f = q.server_filter(&env).unwrap();
        assert!(matches!(&f, Filter::And(v) if v.contains(&Filter::MinSize(5 * MB))), "{f:?}");
        let mut big = row("Анна", "Отчёт");
        big.has_attachment = true;
        big.size = 6 * MB;
        let env = Query::parse("has:attachment larger:5M").env(&now(), |_| None, &[]);
        let q = Query::parse("has:attachment larger:5M");
        assert!(q.matches(&env, &big, &Recipients::default()));
        big.size = 4 * MB;
        assert!(!q.matches(&env, &big, &Recipients::default()));
    }
}
