//! Ключевые слова Postman у письма: метка «Конфиденциально» (S5-06б) и сроки жизненного цикла (S5-04) — срок,
//! «Ждём ответа», «Напомнит», «Исчезнет». Ставят их композер (`Draft::keywords`) и действия с письмом; читают —
//! строка письма, быстрые фильтры (`crate::filters`) и планировщик (`crate::lifecycle`).
//!
//! Формат — ключевые слова JMAP (`crate::labels::valid_keyword`: ASCII без заглавных и `( ) { ] % * " \`), без `$`:
//! это слова Postman. У слова JMAP нет значения, поэтому срок — в самом слове, а рядом — общее слово без даты:
//! - `pm.conf` — метка «Конфиденциально». Шифрования нет: S/MIME — S5-12 (`docs/SMIME.md`).
//! - `pm.due` + `pm.due.<ГГГГ-ММ-ДД>[t<ЧЧММ>]` — срок («До чт», «Просрочено 1 д»); без времени — до конца дня.
//! - `pm.wait` + `pm.wait.<ГГГГ-ММ-ДД>` — ждём ответа до этой даты.
//! - `pm.remind` + `pm.remind.<ГГГГ-ММ-ДД>t<ЧЧММ>` — напомнить («Напомнит чт 09:00»): до срока письма нет во
//!   «Входящих», в срок оно возвращается непрочитанным. Общее слово без даты остаётся, пока письмо не прочитано, —
//!   «напомнило»: письмо наверху списка.
//! - `pm.expire` + `pm.expire.<ГГГГ-ММ-ДД>[t<ЧЧММ>]` — исчезнет: в срок уйдёт в «Архив» или «Удалённые».
//! - `pm.taken` + `pm.taken.<ГГГГ-ММ-ДД>t<ЧЧММ>z.<адрес>` — «Взять в работу» в общем ящике (S5-28): кто взял и
//!   когда (время — UTC, `z`: письмо видят коллеги в других поясах). Слова — на письме в ящике, их видят все с правом
//!   чтения; «Забрать себе» снимает прежние слова и ставит своё. Двое взяли разом — в работе у взявшего позже.
//!
//! Дата и время — местные (часовой пояс компьютера). `t` — строчная: заглавных в ключевых словах JMAP нет; время
//! без двоеточия. Общее слово нужно серверу: `hasKeyword` ищет слово целиком, по нему находит письма «Все папки»
//! (быстрые фильтры) и индекс кэша (планировщик). Слова других клиентов в регистре IMAP (`PM.Due.…`, `T`) тоже
//! разбираются. Проверено на Stalwart: `crates/core/tests/stand_lifecycle.rs`, `docs/STALWART.md`.

use jiff::ToSpan;
use jiff::civil::{Date, Time};

/// Метка «Конфиденциально».
pub const CONFIDENTIAL: &str = "pm.conf";
/// Общие слова сроков (без даты): по ним ищут сервер и кэш.
pub const DUE: &str = "pm.due";
pub const WAIT: &str = "pm.wait";
pub const REMIND: &str = "pm.remind";
pub const EXPIRE: &str = "pm.expire";
/// Префикс «Ждать ответа до даты».
pub const WAIT_PREFIX: &str = "pm.wait.";
/// Сроки «Ждать ответа · N дней» на выбор и срок по умолчанию.
pub const WAIT_DAYS: [u8; 5] = [1, 2, 3, 5, 7];
pub const WAIT_DEFAULT: u8 = 3;

/// Срок жизненного цикла письма.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum Kind {
    /// Срок: «До чт», «Просрочено 1 д».
    Due,
    /// Ждём ответа до даты.
    Wait,
    /// Напомнить в срок.
    Remind,
    /// Исчезнет в срок.
    Expire,
}

/// Время в слове: нет, можно или обязательно.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Clock {
    No,
    Optional,
    Required,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Due, Kind::Wait, Kind::Remind, Kind::Expire];

    /// Общее слово без даты.
    pub fn base(self) -> &'static str {
        match self {
            Kind::Due => DUE,
            Kind::Wait => WAIT,
            Kind::Remind => REMIND,
            Kind::Expire => EXPIRE,
        }
    }

    fn clock(self) -> Clock {
        match self {
            Kind::Wait => Clock::No,
            Kind::Due | Kind::Expire => Clock::Optional,
            Kind::Remind => Clock::Required,
        }
    }

    /// Слово со сроком: `pm.due.2026-10-24`, `pm.remind.2026-10-24t0900`. У «Ждём ответа» время отбрасывается; у
    /// «Напомнит» без времени — 09:00.
    pub fn keyword(self, when: When) -> String {
        let date = when.date.strftime("%Y-%m-%d");
        let time = match (self.clock(), when.time) {
            (Clock::No, _) | (Clock::Optional, None) => None,
            (_, Some(t)) => Some(t),
            (Clock::Required, None) => Time::new(MORNING, 0, 0, 0).ok(),
        };
        match time {
            Some(t) => format!("{}.{date}t{}", self.base(), t.strftime("%H%M")),
            None => format!("{}.{date}", self.base()),
        }
    }

    /// Срок из слова; другое слово (в том числе общее без даты) или неверная дата — `None`. Регистр не важен.
    pub fn parse(self, keyword: &str) -> Option<When> {
        let base = self.base();
        let head = keyword.get(..base.len() + 1)?;
        if !head[..base.len()].eq_ignore_ascii_case(base) || !head.ends_with('.') {
            return None;
        }
        let rest = &keyword[base.len() + 1..];
        let date: Date = rest.get(..10)?.parse().ok()?;
        let tail = &rest[10..];
        let time = match tail.len() {
            0 => None,
            5 if tail.starts_with(['t', 'T']) && tail[1..].bytes().all(|b| b.is_ascii_digit()) => {
                let (h, m) = (tail[1..3].parse::<i8>().ok()?, tail[3..5].parse::<i8>().ok()?);
                Some(Time::new(h, m, 0, 0).ok()?)
            }
            _ => return None,
        };
        match (self.clock(), time) {
            (Clock::No, Some(_)) | (Clock::Required, None) => None,
            _ => Some(When { date, time }),
        }
    }

    /// Слово относится к этому сроку: общее или со сроком (в том числе неразобранное `pm.due.что-то`).
    pub fn owns(self, keyword: &str) -> bool {
        let base = self.base();
        keyword.get(..base.len()).is_some_and(|h| h.eq_ignore_ascii_case(base))
            && (keyword.len() == base.len() || keyword.as_bytes().get(base.len()) == Some(&b'.'))
    }
}

/// Утро по умолчанию у напоминания без времени.
pub const MORNING: i8 = 9;

/// Срок: дата и, если есть, время (местные).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct When {
    pub date: Date,
    pub time: Option<Time>,
}

impl When {
    pub fn day(date: Date) -> When {
        When { date, time: None }
    }

    pub fn at(date: Date, time: Time) -> When {
        When { date, time: Some(time) }
    }
}

/// Срок письма с ключевыми словами `keywords`: самый ранний разобранный. Нет слова со сроком — `None`.
pub fn find(keywords: &[String], kind: Kind) -> Option<When> {
    keywords.iter().filter_map(|k| kind.parse(k)).min()
}

/// У письма есть этот срок: общее слово или слово со сроком.
pub fn has(keywords: &[String], kind: Kind) -> bool {
    keywords.iter().any(|k| kind.owns(k))
}

/// Что снять, чтобы срока не было: общее слово и все слова со сроком (как они записаны у письма).
pub fn clearing(keywords: &[String], kind: Kind) -> Vec<String> {
    let mut out: Vec<String> = keywords.iter().filter(|k| kind.owns(k)).cloned().collect();
    if !out.iter().any(|k| k.eq_ignore_ascii_case(kind.base())) {
        out.push(kind.base().to_owned());
    }
    out
}

/// Поставить срок `when`: (поставить, снять) — общее слово и новое слово со сроком; прежние слова со сроком снять.
pub fn setting(keywords: &[String], kind: Kind, when: When) -> (Vec<String>, Vec<String>) {
    let new = kind.keyword(when);
    let remove = keywords.iter().filter(|k| kind.owns(k) && !k.eq_ignore_ascii_case(kind.base()) && **k != new).cloned().collect();
    (vec![kind.base().to_owned(), new], remove)
}

/// `pm.wait.2026-10-24`.
pub fn wait_keyword(due: Date) -> String {
    Kind::Wait.keyword(When::day(due))
}

/// Срок из слова «Ждать ответа»; чужое слово или неверная дата — `None`.
pub fn parse_wait(keyword: &str) -> Option<Date> {
    Kind::Wait.parse(keyword).map(|w| w.date)
}

/// Срок через `days` дней от `today`.
pub fn wait_due(today: Date, days: u8) -> Option<Date> {
    today.checked_add(i64::from(days).days()).ok()
}

/// Слово управляется композером (метка, ожидание ответа): при сохранении черновика ставится заново.
pub fn is_compose_mark(keyword: &str) -> bool {
    keyword.eq_ignore_ascii_case(CONFIDENTIAL) || Kind::Wait.owns(keyword)
}

/// Общее слово «Взято в работу» (S5-28): по нему сервер находит взятые письма.
pub const TAKEN: &str = "pm.taken";
const TAKEN_PREFIX: &str = "pm.taken.";

/// Кто взял письмо общего ящика в работу и когда.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Taken {
    /// Адрес взявшего (нижний регистр).
    pub who: String,
    pub at: jiff::Timestamp,
}

impl Taken {
    /// Взял `me` (адрес сравнивается без регистра).
    pub fn by(&self, me: &str) -> bool {
        self.who.eq_ignore_ascii_case(me.trim())
    }
}

/// Слово «взял `who` в `at`»: `pm.taken.2026-10-22t1042z.p.koval@corp.example`. Адрес, который не годится в ключевое
/// слово JMAP (пробел, кавычка, скобка…), — `None`.
pub fn taken_keyword(who: &str, at: jiff::Timestamp) -> Option<String> {
    let who = who.trim().to_ascii_lowercase();
    let k = format!("{TAKEN_PREFIX}{}z.{who}", at.strftime("%Y-%m-%dt%H%M"));
    (!who.is_empty() && crate::labels::valid_keyword(&k)).then_some(k)
}

fn parse_taken(keyword: &str) -> Option<Taken> {
    let head = keyword.get(..TAKEN_PREFIX.len())?;
    if !head.eq_ignore_ascii_case(TAKEN_PREFIX) {
        return None;
    }
    let rest = &keyword[TAKEN_PREFIX.len()..];
    // `ГГГГ-ММ-ДДtЧЧММz.` — 17 знаков, дальше адрес.
    let (stamp, who) = (rest.get(..15)?, rest.get(17..)?);
    if !rest.get(15..17)?.eq_ignore_ascii_case("z.") || who.is_empty() || !who.contains('@') {
        return None;
    }
    let date: Date = stamp.get(..10)?.parse().ok()?;
    let clock = stamp.get(10..)?;
    if !clock.starts_with(['t', 'T']) || !clock[1..].bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let time = Time::new(clock[1..3].parse().ok()?, clock[3..5].parse().ok()?, 0, 0).ok()?;
    let at = date.to_datetime(time).to_zoned(jiff::tz::TimeZone::UTC).ok()?.timestamp();
    Some(Taken { who: who.to_ascii_lowercase(), at })
}

/// Кто держит письмо: из слов «взял» — позднее (при равном времени — меньший адрес, одинаково у всех). Нет слова —
/// никто, даже если общее слово осталось (другой клиент снял не всё).
pub fn taken(keywords: &[String]) -> Option<Taken> {
    keywords.iter().filter_map(|k| parse_taken(k)).max_by(|a, b| a.at.cmp(&b.at).then_with(|| b.who.cmp(&a.who)))
}

/// Слово относится к «Взять в работу»: общее или со временем и адресом (в том числе неразобранное).
pub fn is_taken_word(keyword: &str) -> bool {
    keyword.get(..TAKEN.len()).is_some_and(|h| h.eq_ignore_ascii_case(TAKEN))
        && (keyword.len() == TAKEN.len() || keyword.as_bytes().get(TAKEN.len()) == Some(&b'.'))
}

/// «Взять в работу» и «Забрать себе»: (поставить, снять) — общее слово и своё, прежние слова «взял» снять. Адрес не
/// годится в ключевое слово — `None`.
pub fn taking(keywords: &[String], me: &str, at: jiff::Timestamp) -> Option<(Vec<String>, Vec<String>)> {
    let new = taken_keyword(me, at)?;
    let remove = keywords.iter().filter(|k| is_taken_word(k) && !k.eq_ignore_ascii_case(TAKEN) && **k != new).cloned().collect();
    Some((vec![TAKEN.to_owned(), new], remove))
}

/// «Вернуть в очередь»: что снять — общее слово и все слова «взял».
pub fn releasing(keywords: &[String]) -> Vec<String> {
    let mut out: Vec<String> = keywords.iter().filter(|k| is_taken_word(k)).cloned().collect();
    if !out.iter().any(|k| k.eq_ignore_ascii_case(TAKEN)) {
        out.push(TAKEN.to_owned());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::labels::valid_keyword;

    fn date(s: &str) -> Date {
        s.parse().unwrap()
    }

    fn kws(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn wait_keyword_round_trips_and_is_a_valid_jmap_keyword() {
        let today = date("2026-10-21");
        let due = wait_due(today, 3).unwrap();
        let k = wait_keyword(due);
        assert_eq!(k, "pm.wait.2026-10-24");
        assert!(valid_keyword(&k) && valid_keyword(CONFIDENTIAL));
        assert_eq!(parse_wait(&k), Some(due));
        assert_eq!(parse_wait("PM.WAIT.2026-10-24"), Some(due), "регистр IMAP-клиентов");
        for bad in ["pm.wait.", "pm.wait.2026-13-01", "pm.wait.2026-10-24x", "pm.cat.team", "pm.wait", "pm.wait.2026-10-24t0900"] {
            assert_eq!(parse_wait(bad), None, "{bad}");
        }
        // Конец месяца и года.
        assert_eq!(wait_due(date("2026-12-30"), 5).map(wait_keyword).as_deref(), Some("pm.wait.2027-01-04"));
        assert!(is_compose_mark("pm.conf") && is_compose_mark(&k) && is_compose_mark(WAIT) && !is_compose_mark("pm.cat.team"));
        assert!(!is_compose_mark("pm.waiting"));
    }

    /// Все слова сроков — допустимые ключевые слова JMAP, туда и обратно; время — только где можно.
    #[test]
    fn lifecycle_keywords_round_trip() {
        let d = date("2026-10-29");
        let t = Time::new(9, 5, 0, 0).unwrap();
        let cases = [
            (Kind::Due, When::day(d), "pm.due.2026-10-29"),
            (Kind::Due, When::at(d, t), "pm.due.2026-10-29t0905"),
            (Kind::Wait, When::day(d), "pm.wait.2026-10-29"),
            (Kind::Remind, When::at(d, t), "pm.remind.2026-10-29t0905"),
            (Kind::Expire, When::day(d), "pm.expire.2026-10-29"),
            (Kind::Expire, When::at(d, Time::new(23, 59, 0, 0).unwrap()), "pm.expire.2026-10-29t2359"),
        ];
        for (kind, when, text) in cases {
            let k = kind.keyword(when);
            assert_eq!(k, text);
            assert!(valid_keyword(&k) && valid_keyword(kind.base()), "{k}");
            assert_eq!(kind.parse(&k), Some(when), "{k}");
            assert!(kind.owns(&k) && kind.owns(kind.base()));
        }
        // Время у «Ждём» отбрасывается; напоминание без времени — утром.
        assert_eq!(Kind::Wait.keyword(When::at(d, t)), "pm.wait.2026-10-29");
        assert_eq!(Kind::Remind.keyword(When::day(d)), "pm.remind.2026-10-29t0900");
        // Чужой регистр (IMAP) и заглавная `T`.
        assert_eq!(Kind::Remind.parse("PM.Remind.2026-10-29T0905"), Some(When::at(d, t)));
        for (kind, bad) in [
            (Kind::Remind, "pm.remind.2026-10-29"),
            (Kind::Remind, "pm.remind"),
            (Kind::Due, "pm.due.2026-10-29t2500"),
            (Kind::Due, "pm.due.2026-10-29t09:00"),
            (Kind::Due, "pm.due.20261029"),
            (Kind::Due, "pm.duex.2026-10-29"),
            (Kind::Expire, "pm.due.2026-10-29"),
        ] {
            assert_eq!(kind.parse(bad), None, "{bad}");
        }
        assert!(!Kind::Due.owns("pm.duet") && Kind::Due.owns("pm.due.20261023"));
    }

    /// Совпадает с условиями быстрых фильтров (`filters`): слово со сроком — уточнение общего.
    #[test]
    fn dated_words_pass_the_quick_filters() {
        use crate::filters::{Facts, Me};
        let mut row = postman_cache::EmailRow {
            id: "a".into(),
            thread_id: String::new(),
            mailbox_ids: vec!["in".into()],
            received_at: 0,
            subject: String::new(),
            from_name: String::new(),
            from_addr: String::new(),
            preview: String::new(),
            seen: true,
            flagged: false,
            has_attachment: false,
            size: 0,
            keywords: Vec::new(),
        };
        let d = When::day(date("2026-10-29"));
        row.keywords = vec![Kind::Due.keyword(d), Kind::Wait.keyword(d), Kind::Expire.keyword(d)];
        let f = Facts::of(&Me::default(), &row, &Default::default());
        assert!(f.due && f.waiting && f.expiring);
        assert_eq!((crate::filters::DUE, crate::filters::WAIT, crate::filters::EXPIRE), (DUE, WAIT, EXPIRE));
    }

    #[test]
    fn find_set_and_clear() {
        let row = kws(&["pm.cat.team", "pm.due", "pm.due.2026-10-30", "pm.due.2026-10-29t1500", "PM.WAIT.2026-11-01"]);
        assert_eq!(find(&row, Kind::Due), Some(When::at(date("2026-10-29"), Time::new(15, 0, 0, 0).unwrap())), "ранний");
        assert_eq!(find(&row, Kind::Wait), Some(When::day(date("2026-11-01"))));
        assert_eq!(find(&row, Kind::Remind), None);
        assert!(has(&row, Kind::Wait) && !has(&row, Kind::Expire));
        assert!(has(&kws(&["pm.expire"]), Kind::Expire), "общее слово без даты (другой клиент)");
        let (add, remove) = setting(&row, Kind::Due, When::day(date("2026-11-02")));
        assert_eq!(add, ["pm.due", "pm.due.2026-11-02"]);
        assert_eq!(remove, ["pm.due.2026-10-30", "pm.due.2026-10-29t1500"]);
        // Тот же срок — не снимается.
        let (_, remove) = setting(&row, Kind::Due, When::day(date("2026-10-30")));
        assert_eq!(remove, ["pm.due.2026-10-29t1500"]);
        assert_eq!(clearing(&row, Kind::Due), ["pm.due", "pm.due.2026-10-30", "pm.due.2026-10-29t1500"]);
        assert_eq!(clearing(&row, Kind::Wait), ["PM.WAIT.2026-11-01", "pm.wait"]);
        assert_eq!(clearing(&[], Kind::Remind), ["pm.remind"]);
    }

    /// «Взять в работу» (S5-28): формат слова, туда и обратно; позднее — главнее; забрать и вернуть.
    #[test]
    fn taken_words_round_trip() {
        let at: jiff::Timestamp = "2026-10-22T10:42:00Z".parse().unwrap();
        let k = taken_keyword("P.Koval@Corp.Example", at).unwrap();
        assert_eq!(k, "pm.taken.2026-10-22t1042z.p.koval@corp.example");
        assert!(valid_keyword(&k) && valid_keyword(TAKEN));
        let t = taken(&kws(&["pm.taken", &k])).unwrap();
        assert_eq!((t.who.as_str(), t.at), ("p.koval@corp.example", at));
        assert!(t.by("p.koval@corp.example ") && t.by("P.KOVAL@corp.example") && !t.by("me@corp.example"));
        // Регистр IMAP-клиентов; неверные слова — не «взял».
        assert_eq!(taken(&kws(&["PM.Taken.2026-10-22T1042Z.p.koval@corp.example"])).map(|t| t.at), Some(at));
        for bad in [
            "pm.taken",
            "pm.taken.",
            "pm.taken.2026-10-22t1042z.",
            "pm.taken.2026-10-22t1042z.koval",
            "pm.taken.2026-10-22t1042.p@corp.example",
            "pm.taken.2026-10-22t2542z.p@corp.example",
            "pm.taken.2026-10-22z.p@corp.example",
            "pm.takenx.2026-10-22t1042z.p@corp.example",
        ] {
            assert_eq!(taken(&kws(&[bad])), None, "{bad}");
        }
        assert_eq!(taken_keyword("a b@corp.example", at), None, "пробел в слове JMAP нельзя");
        assert_eq!(taken_keyword(" ", at), None);
        // Двое разом: позднее, при равном времени — меньший адрес.
        let later: jiff::Timestamp = "2026-10-22T10:45:00Z".parse().unwrap();
        let both = kws(&["pm.taken", &k, &taken_keyword("a.kotova@corp.example", later).unwrap()]);
        assert_eq!(taken(&both).map(|t| t.who), Some("a.kotova@corp.example".into()));
        let tie = kws(&[&k, &taken_keyword("a.kotova@corp.example", at).unwrap()]);
        assert_eq!(taken(&tie).map(|t| t.who), Some("a.kotova@corp.example".into()));
        // Забрать себе: общее и своё — поставить, чужие — снять; своё прежнее с тем же временем — не снимать.
        let now: jiff::Timestamp = "2026-10-22T14:05:00Z".parse().unwrap();
        let (add, remove) = taking(&both, "me@corp.example", now).unwrap();
        assert_eq!(add, ["pm.taken", "pm.taken.2026-10-22t1405z.me@corp.example"]);
        assert_eq!(remove, both[1..].to_vec());
        assert_eq!(releasing(&both), both);
        assert_eq!(releasing(&kws(&["pm.cat.team", &k])), [k.clone(), TAKEN.to_owned()]);
        assert!(is_taken_word("pm.taken") && is_taken_word(&k) && !is_taken_word("pm.takens") && !is_taken_word("pm.due"));
        // Демо-ящик пишет те же слова.
        let demo = postman_demo::taken_keywords(1, 1_792_240_200);
        assert_eq!(taken(&demo).map(|t| t.who), Some("a.petrov@corp.example".into()));
        assert!(demo.iter().all(|k| valid_keyword(k)));
    }
}
