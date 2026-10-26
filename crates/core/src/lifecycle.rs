//! Жизненный цикл писем (S5-04, `docs/DESIGN.md` «Строка письма и метки», «Настройки»): метки сроков в строке
//! письма, планировщик клиента и эвристики (коды подтверждения, рассылки). Ключевые слова — `crate::marks`.
//!
//! Решение владельца №2 (`docs/specs/S5.md`): на сервере планировщика нет, всё работает, пока Postman запущен.
//! Здесь — без UI и сети: [`gather`] читает из кэша письма со сроками (индекс ключевых слов, без обхода ящика) и
//! свежие «Входящие», [`plan`] решает, что с ними сделать сейчас: правки писем ([`Patch`] — кэшу сразу, серверу одним
//! `Email/set`), уведомления и ближайший срок, к которому интерфейс ставит один таймер (в простое — 0 % CPU).
//!
//! Что делает планировщик:
//! - **Напомнит**: в срок слово со сроком снимается, письмо — во «Входящие» (из «Архива», куда его убрало
//!   «Напомнить…») и непрочитанным; общее слово `pm.remind` остаётся, пока письмо не прочитают, — оно наверху списка
//!   ([`on_top`]); тост «Открыть».
//! - **Ждём ответа**: пришёл ответ в беседе (тот же `threadId`, отправитель — адресат письма) — ожидание снимается;
//!   срок прошёл без ответа (настройка «Ждём ответа») — письмо из «Отправленных» и во «Входящие», тост.
//! - **Исчезнет**: в срок письмо уходит в «Архив» или «Удалённые» (настройка), слова срока снимаются.
//! - **Коды подтверждения** (настройка): письмо с кодом ([`is_code`]) во «Входящих» получает «Исчезнет» через N часов
//!   после получения. Снятое вручную («Никогда») — слово [`KEEP`], правило его не ставит снова.
//! - **Рассылки** (настройка «В архив через N»): письма «Входящих» с `List-Id` или `List-Unsubscribe` (сырые
//!   заголовки — в `Email/get` сервера: в кэше их нет, условие `header` Stalwart не выполняет), прочитанные старше
//!   N дней — в «Архив», последняя от каждого отправителя остаётся ([`newsletter_sweep`],
//!   `sync::Command::Newsletters`).

use std::collections::HashSet;
use std::time::Duration;

use jiff::civil::{Date, Time, Weekday};
use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};
pub use postman_cache::Patch;
use postman_cache::{Cache, EmailRow, MailboxRow, Recipients, Scan, all_folders};

use crate::filters::Me;
use crate::marks::{self, Kind, When};
use crate::prefs::{ExpireTo, Lifecycle};

/// «Архив», которого у аккаунта ещё нет (у Stalwart по умолчанию его нет): в правке для сервера — папка с ролью
/// `archive`, синхронизация создаёт её (`sync::Command::Patch`); в кэше такой папки нет — правка кэша без неё
/// ([`Patch`] через [`for_cache`]).
pub const ARCHIVE: &str = "\u{1}archive";
/// «Не исчезнет»: «Исчезнет… → Никогда» у письма с кодом — правило кодов больше не ставит срок.
pub const KEEP: &str = "pm.keep";
/// Коды подтверждения ищутся среди писем «Входящих» за это время (секунды).
pub const CODE_WINDOW: i64 = 7 * DAY;
/// Писем одного срока за проход — не больше (остальные — следующим проходом).
pub const LIMIT: usize = 2000;
/// Рассылки проверяются раз в столько (запрос к серверу).
pub const NEWS_EVERY: Duration = Duration::from_secs(6 * 3600);
/// Рассылок за проверку — не больше (одна страница `Email/query`).
pub const NEWS_LIMIT: u64 = 500;

const DAY: i64 = 86_400;
const HOUR: i64 = 3600;

// --- моменты ---

/// Когда наступает срок в поясе `tz`: срок (`Due`) без времени — в конце дня (начало следующего), остальные без
/// времени — в начале дня даты.
pub fn moment(kind: Kind, when: When, tz: &TimeZone) -> Option<Timestamp> {
    let (date, time) = match (kind, when.time) {
        (_, Some(t)) => (when.date, t),
        (Kind::Due, None) => (when.date.tomorrow().ok()?, Time::midnight()),
        (_, None) => (when.date, Time::midnight()),
    };
    date.to_datetime(time).to_zoned(tz.clone()).ok().map(|z| z.timestamp())
}

/// Местные дата и время момента `ts` (минуты вверх: «через 24 ч» не раньше срока).
pub fn when_at(ts: i64, tz: &TimeZone) -> Option<When> {
    let up = ts.checked_add(59)? / 60 * 60;
    let z = Timestamp::from_second(up).ok()?.to_zoned(tz.clone());
    let t = Time::new(z.hour(), z.minute(), 0, 0).ok()?;
    Some(When::at(z.date(), t))
}

/// Дней от `from` до `to` (может быть меньше нуля).
fn days_between(from: Date, to: Date) -> i64 {
    from.until(to).map_or(0, |s| i64::from(s.get_days()))
}

// --- метки строки ---

/// День срока относительно сегодня: для текста «До 15:00», «До завтра», «До чт», «До 24.10».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Day {
    Today,
    Tomorrow,
    /// В ближайшие шесть дней.
    Weekday(Weekday),
    Date(Date),
}

/// Срок впереди: день и, если есть, время.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ahead {
    pub day: Day,
    pub time: Option<Time>,
}

impl Ahead {
    pub fn of(when: When, today: Date) -> Ahead {
        let day = match days_between(today, when.date) {
            0 => Day::Today,
            1 => Day::Tomorrow,
            2..=6 => Day::Weekday(when.date.weekday()),
            _ => Day::Date(when.date),
        };
        Ahead { day, time: when.time }
    }
}

/// Сколько: часы (меньше суток) или дни.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Span {
    Hours(i64),
    Days(i64),
}

/// Метка срока в строке письма и в шапке (DESIGN): срок впереди — янтарный текст с часами, прошёл — янтарная
/// заливка; остальные — нейтральные серые. Тексты — у интерфейса (`lifecycle.ftl`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Badge {
    /// «До чт», «До 15:00»; `None` — слово без даты («Срок»).
    Due(Option<Ahead>),
    /// «Просрочено 1 д».
    Overdue(Span),
    /// «Ждём ответа · N д»: дней до срока (0 — сегодня); `None` — без даты.
    Waiting(Option<i64>),
    /// Срок ожидания прошёл без ответа: дней.
    NoReply(i64),
    /// «Напомнит чт 09:00».
    Remind(Ahead),
    /// «Исчезнет через 3 д»; `None` — без даты или уже сейчас.
    Expires(Option<Span>),
}

impl Badge {
    /// Янтарная — только сроки (DESIGN «Цвет»).
    pub fn amber(&self) -> bool {
        matches!(self, Badge::Due(_) | Badge::Overdue(_))
    }

    /// Плотная заливка — срок прошёл.
    pub fn filled(&self) -> bool {
        matches!(self, Badge::Overdue(_))
    }
}

/// Метки сроков письма по порядку: срок, напоминание, ожидание, «Исчезнет».
pub fn badges(keywords: &[String], now: &Zoned) -> Vec<Badge> {
    let mut out = Vec::new();
    if !keywords.iter().any(|k| k.starts_with("pm.") || k.starts_with("PM.")) {
        return out;
    }
    let tz = now.time_zone();
    let (today, ts) = (now.date(), now.timestamp().as_second());
    match marks::find(keywords, Kind::Due) {
        Some(w) => {
            let m = moment(Kind::Due, w, tz).map_or(i64::MAX, |m| m.as_second());
            if m > ts {
                out.push(Badge::Due(Some(Ahead::of(w, today))));
            } else {
                let late = ts - m;
                let span = match w.time {
                    Some(_) if late < DAY => Span::Hours((late + HOUR - 1) / HOUR),
                    Some(_) => Span::Days(late / DAY),
                    None => Span::Days(days_between(w.date, today).max(1)),
                };
                out.push(Badge::Overdue(span));
            }
        }
        None if marks::has(keywords, Kind::Due) => out.push(Badge::Due(None)),
        None => {}
    }
    if let Some(w) = marks::find(keywords, Kind::Remind) {
        out.push(Badge::Remind(Ahead::of(w, today)));
    }
    match marks::find(keywords, Kind::Wait) {
        Some(w) => {
            let left = days_between(today, w.date);
            out.push(if left >= 0 { Badge::Waiting(Some(left)) } else { Badge::NoReply(-left) });
        }
        None if marks::has(keywords, Kind::Wait) => out.push(Badge::Waiting(None)),
        None => {}
    }
    if !keywords.iter().any(|k| k.eq_ignore_ascii_case(KEEP)) {
        match marks::find(keywords, Kind::Expire) {
            Some(w) => {
                let left = moment(Kind::Expire, w, tz).map_or(0, |m| m.as_second() - ts);
                let span = if left >= DAY {
                    Some(Span::Days(left / DAY))
                } else if left > 0 {
                    Some(Span::Hours((left + HOUR - 1) / HOUR))
                } else {
                    None
                };
                out.push(Badge::Expires(span));
            }
            None if marks::has(keywords, Kind::Expire) => out.push(Badge::Expires(None)),
            None => {}
        }
    }
    out
}

/// Срок письма прошёл.
pub fn overdue(keywords: &[String], now: &Zoned) -> bool {
    marks::find(keywords, Kind::Due).and_then(|w| moment(Kind::Due, w, now.time_zone())).is_some_and(|m| m <= now.timestamp())
}

/// Напоминание сработало, письмо ещё не прочитано: общее слово `pm.remind` без срока.
pub fn reminded(row: &EmailRow) -> bool {
    !row.seen && row.has_keyword(marks::REMIND) && marks::find(&row.keywords, Kind::Remind).is_none()
}

/// Письмо — наверху списка: просроченный срок (настройка «Просроченный срок — наверх») или сработавшее напоминание.
pub fn on_top(row: &EmailRow, now: &Zoned, overdue_first: bool) -> bool {
    reminded(row) || (overdue_first && overdue(&row.keywords, now))
}

// --- правки по действиям пользователя ---

/// Папки своего аккаунта по ролям (ключи кэша).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Roles {
    pub inbox: Option<String>,
    pub archive: Option<String>,
    pub trash: Option<String>,
    pub sent: Option<String>,
}

impl Roles {
    /// Папки аккаунта `account` (`None` — свой).
    pub fn of(mailboxes: &[MailboxRow], account: Option<&str>) -> Roles {
        let find = |role: &str| {
            mailboxes.iter().find(|m| m.role.as_deref() == Some(role) && postman_cache::account_of(&m.id) == account).map(|m| m.id.clone())
        };
        Roles { inbox: find("inbox"), archive: find("archive"), trash: find("trash"), sent: find("sent") }
    }

    /// «Архив»: свой или [`ARCHIVE`] — создать.
    pub fn archive_or_new(&self) -> String {
        self.archive.clone().unwrap_or_else(|| ARCHIVE.to_owned())
    }
}

/// Правка для кэша: без папки [`ARCHIVE`] (её ещё нет). Письмо без «Входящих» и без новой папки кэш не оставит без
/// папки ([`Patch::apply`]) — до ответа сервера оно на месте.
pub fn for_cache(p: &Patch) -> Patch {
    let mut out = p.clone();
    out.mailboxes.retain(|(m, _)| m != ARCHIVE);
    out
}

fn keywords(add: Vec<String>, remove: Vec<String>) -> Vec<(String, bool)> {
    remove.into_iter().map(|k| (k, false)).chain(add.into_iter().map(|k| (k, true))).collect()
}

/// Поставить срок `kind` письму (`None` — снять). «Исчезнет» снимает и [`KEEP`].
pub fn set(row: &EmailRow, kind: Kind, when: Option<When>) -> Patch {
    let kw = match when {
        Some(w) => {
            let (add, mut remove) = marks::setting(&row.keywords, kind, w);
            if kind == Kind::Expire && row.has_keyword(KEEP) {
                remove.push(KEEP.to_owned());
            }
            keywords(add, remove)
        }
        None => keywords(Vec::new(), marks::clearing(&row.keywords, kind)),
    };
    Patch { id: row.id.clone(), keywords: kw, mailboxes: Vec::new() }
}

/// «Исчезнет… → Никогда»: снять срок и поставить [`KEEP`] (коды подтверждения больше не помечаются).
pub fn keep(row: &EmailRow) -> Patch {
    Patch { id: row.id.clone(), keywords: keywords(vec![KEEP.to_owned()], marks::clearing(&row.keywords, Kind::Expire)), mailboxes: Vec::new() }
}

/// «Напомнить…»: срок напоминания, письмо из «Входящих» — в «Архив» (до срока его там нет; «Архива» нет — создаётся,
/// [`ARCHIVE`]).
pub fn snooze(row: &EmailRow, when: When, roles: &Roles) -> Patch {
    let mut p = set(row, Kind::Remind, Some(when));
    if let Some(inbox) = roles.inbox.as_ref().filter(|i| row.mailbox_ids.contains(i)) {
        p.mailboxes = vec![(inbox.clone(), false), (roles.archive_or_new(), true)];
    }
    p
}

// --- выбор срока ---

/// Быстрый выбор в панели «Срок…», «Напомнить…», «Ждать ответа…», «Исчезнет…».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    Today,
    Tomorrow,
    /// Ближайшая пятница после сегодня.
    Friday,
    /// Пятница следующей недели (как «На следующей неделе» флага Outlook).
    NextWeek,
    /// Через три часа, к целому часу (поздно вечером — завтра утром).
    LaterToday,
    /// Ближайший понедельник после сегодня, утром.
    Monday,
    /// Через столько дней.
    Days(u16),
}

/// Сколько дней «Исчезнет» на выбор.
pub const EXPIRE_DAYS: [u16; 4] = [1, 3, 7, 30];
/// Позже сегодня — через столько часов.
const LATER_HOURS: i64 = 3;
/// Позже сегодня — не позже этого часа (дальше — завтра утром).
const LATE_EVENING: i8 = 21;

/// Варианты панели для срока `kind`.
pub fn picks(kind: Kind) -> Vec<Pick> {
    match kind {
        Kind::Due => vec![Pick::Today, Pick::Tomorrow, Pick::Friday, Pick::NextWeek],
        Kind::Remind => vec![Pick::LaterToday, Pick::Tomorrow, Pick::Monday],
        Kind::Wait => marks::WAIT_DAYS.iter().map(|d| Pick::Days(u16::from(*d))).collect(),
        Kind::Expire => EXPIRE_DAYS.iter().map(|d| Pick::Days(*d)).collect(),
    }
}

/// День недели строго после `today`.
fn next(today: Date, wd: Weekday) -> Option<Date> {
    let ahead = (wd.to_monday_zero_offset() - today.weekday().to_monday_zero_offset()).rem_euclid(7);
    today.checked_add(jiff::Span::new().days(if ahead == 0 { 7 } else { i64::from(ahead) })).ok()
}

fn morning() -> Time {
    Time::new(marks::MORNING, 0, 0, 0).unwrap_or(Time::midnight())
}

impl Pick {
    /// Срок варианта для `kind` сейчас (`now`, местный пояс).
    pub fn when(self, kind: Kind, now: &Zoned) -> Option<When> {
        let today = now.date();
        let tz = now.time_zone();
        let day = |d: Date| match kind {
            Kind::Remind => When::at(d, morning()),
            _ => When::day(d),
        };
        Some(match self {
            Pick::Today => day(today),
            Pick::Tomorrow => day(today.tomorrow().ok()?),
            Pick::Friday => day(next(today, Weekday::Friday)?),
            Pick::NextWeek => day(next(today, Weekday::Monday)?.checked_add(jiff::Span::new().days(4)).ok()?),
            Pick::Monday => day(next(today, Weekday::Monday)?),
            Pick::LaterToday => {
                let ts = now.timestamp().as_second() + LATER_HOURS * HOUR;
                let up = (ts + HOUR - 1) / HOUR * HOUR;
                let w = when_at(up, tz)?;
                if w.date != today || w.time.is_some_and(|t| t.hour() > LATE_EVENING) { When::at(today.tomorrow().ok()?, morning()) } else { w }
            }
            Pick::Days(n) => match kind {
                Kind::Expire => when_at(now.timestamp().as_second() + i64::from(n) * DAY, tz)?,
                _ => day(today.checked_add(jiff::Span::new().days(i64::from(n))).ok()?),
            },
        })
    }
}

/// Почему введённый срок не годится.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bad {
    /// Не разобрать (`ДД.ММ.ГГГГ [ЧЧ:ММ]`).
    Format,
    /// Срок уже прошёл.
    Past,
}

/// Срок из строки «Выбрать дату…»: `ДД.ММ.ГГГГ`, `ДД.ММ`, `ГГГГ-ММ-ДД` и, через пробел, `ЧЧ:ММ`. У «Ждём ответа»
/// время отбрасывается; у «Напомнит» без времени — утро. Прошедший срок — [`Bad::Past`].
pub fn parse_when(text: &str, kind: Kind, now: &Zoned) -> Result<When, Bad> {
    let mut parts = text.split_whitespace();
    let date = parts.next().ok_or(Bad::Format)?;
    let time = parts.next();
    if parts.next().is_some() {
        return Err(Bad::Format);
    }
    let at = crate::send_later::parse(date, time.unwrap_or("0:00"), now).map_err(|_| Bad::Format)?;
    let t = Time::new(at.hour(), at.minute(), 0, 0).map_err(|_| Bad::Format)?;
    let when = match (kind, time) {
        (Kind::Wait, _) | (Kind::Due | Kind::Expire, None) => When::day(at.date()),
        (Kind::Remind, None) => When::at(at.date(), morning()),
        (_, Some(_)) => When::at(at.date(), t),
    };
    match moment(kind, when, now.time_zone()) {
        Some(m) if m > now.timestamp() => Ok(when),
        _ => Err(Bad::Past),
    }
}

// --- планировщик ---

/// Письмо со сроком и что нужно для «Ждём ответа»: адресаты и письма беседы.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub row: EmailRow,
    pub to: Recipients,
    pub thread: Vec<EmailRow>,
}

/// Что читает планировщик из кэша.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Input {
    /// Письма со словами сроков (свой аккаунт, все папки).
    pub items: Vec<Item>,
    /// «Входящие» за [`CODE_WINDOW`] — коды подтверждения.
    pub recent: Vec<EmailRow>,
}

/// Прочитать из кэша: письма со словами сроков — по индексу ключевых слов (слово и его уточнения, без обхода
/// ящика), для ожидающих ответа — адресаты и беседа; свежие «Входящие» — по порядку папки до [`CODE_WINDOW`].
pub fn gather(cache: &Cache, roles: &Roles, now: i64) -> postman_cache::Result<Input> {
    let all = all_folders(None);
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    for kind in Kind::ALL {
        for row in cache.keyword_prefix(kind.base(), &all, LIMIT)? {
            if !row.keywords.iter().any(|k| kind.owns(k)) || !seen.insert(row.id.clone()) {
                continue;
            }
            let waiting = marks::has(&row.keywords, Kind::Wait);
            let to = if waiting { cache.recipients(&row.id)? } else { Recipients::default() };
            let thread = if waiting && !row.thread_id.is_empty() { cache.thread(&row.thread_id)? } else { Vec::new() };
            items.push(Item { row, to, thread });
        }
    }
    let mut recent = Vec::new();
    if let Some(inbox) = &roles.inbox {
        cache.scan(&Scan::Mailbox(inbox.clone()), |row, _| {
            if row.received_at < now - CODE_WINDOW {
                return false;
            }
            recent.push(row);
            true
        })?;
    }
    Ok(Input { items, recent })
}

/// О чём сказать пользователю.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// Напоминание сработало: тост «Открыть».
    Reminded { id: String, subject: String },
    /// Ответа нет, срок прошёл: письмо во «Входящих».
    NoReply { id: String, subject: String },
    /// Ответ пришёл — ожидание снято.
    Replied { id: String, subject: String },
    /// «Исчезнет»: столько писем ушло.
    Expired(usize),
}

/// Решение планировщика на этот момент.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub patches: Vec<Patch>,
    pub notices: Vec<Notice>,
    /// Ближайший срок после сейчас (Unix-секунды): к нему — таймер. Срок уже прошёл (письмо только что помечено) —
    /// сразу.
    pub next: Option<i64>,
}

impl Plan {
    fn wake(&mut self, at: i64) {
        self.next = Some(self.next.map_or(at, |n| n.min(at)));
    }
}

/// Пришёл ответ на письмо `row` (ждём ответа): в беседе есть письмо позже него от его адресата (To, Cc); адресаты не
/// известны — от кого угодно, кроме меня.
pub fn replied(row: &EmailRow, to: &Recipients, thread: &[EmailRow], me: &Me) -> bool {
    let addressee = |from: &str| {
        let from = from.trim();
        if to.to.is_empty() && to.cc.is_empty() {
            !from.is_empty() && !from.eq_ignore_ascii_case(&row.from_addr) && !me.addresses().iter().any(|a| a.eq_ignore_ascii_case(from))
        } else {
            to.to.iter().chain(&to.cc).any(|a| a.eq_ignore_ascii_case(from))
        }
    };
    thread.iter().any(|r| r.id != row.id && r.received_at >= row.received_at && addressee(&r.from_addr))
}

/// Что сделать сейчас (см. описание модуля). `rules` — настройки «Сроки и жизненный цикл».
pub fn plan(input: &Input, roles: &Roles, rules: &Lifecycle, me: &Me, now: &Zoned) -> Plan {
    let tz = now.time_zone();
    let ts = now.timestamp().as_second();
    let at = |kind: Kind, w: When| moment(kind, w, tz).map_or(i64::MAX, |m| m.as_second());
    let mut out = Plan::default();
    let mut expired = 0;
    for item in &input.items {
        let row = &item.row;
        let mut p = Patch { id: row.id.clone(), ..Patch::default() };
        // Напомнит.
        match marks::find(&row.keywords, Kind::Remind) {
            Some(w) if at(Kind::Remind, w) <= ts => {
                p.keywords.extend(
                    row.keywords.iter().filter(|k| Kind::Remind.owns(k) && !k.eq_ignore_ascii_case(marks::REMIND)).map(|k| (k.clone(), false)),
                );
                p.keywords.push((marks::REMIND.to_owned(), true));
                p.keywords.push((postman_cache::SEEN.to_owned(), false));
                if let Some(inbox) = &roles.inbox {
                    if !row.mailbox_ids.contains(inbox) {
                        p.mailboxes.push((inbox.clone(), true));
                    }
                    if let Some(archive) = roles.archive.as_ref().filter(|a| row.mailbox_ids.contains(a)) {
                        p.mailboxes.push((archive.clone(), false));
                    }
                }
                out.notices.push(Notice::Reminded { id: row.id.clone(), subject: row.subject.clone() });
            }
            Some(w) => out.wake(at(Kind::Remind, w)),
            // Напомнило и прочитано — наверху больше не держать.
            None if row.seen && row.has_keyword(marks::REMIND) => p.keywords.push((marks::REMIND.to_owned(), false)),
            None => {}
        }
        // Срок: только смена метки «До …» на «Просрочено».
        if let Some(w) = marks::find(&row.keywords, Kind::Due) {
            let m = at(Kind::Due, w);
            if m > ts {
                out.wake(m);
            }
        }
        // Ждём ответа.
        if marks::has(&row.keywords, Kind::Wait) {
            if replied(row, &item.to, &item.thread, me) {
                p.keywords.extend(marks::clearing(&row.keywords, Kind::Wait).into_iter().map(|k| (k, false)));
                // Вернули во «Входящие» без ответа — теперь не нужно там.
                if let (Some(inbox), Some(sent)) = (&roles.inbox, &roles.sent)
                    && row.mailbox_ids.contains(inbox)
                    && row.mailbox_ids.contains(sent)
                {
                    p.mailboxes.push((inbox.clone(), false));
                }
                out.notices.push(Notice::Replied { id: row.id.clone(), subject: row.subject.clone() });
            } else if let Some(w) = marks::find(&row.keywords, Kind::Wait) {
                let m = at(Kind::Wait, w);
                if m > ts {
                    out.wake(m);
                } else if rules.wait_reply
                    && let (Some(inbox), Some(sent)) = (&roles.inbox, &roles.sent)
                    && !row.mailbox_ids.is_empty()
                    && row.mailbox_ids.iter().all(|b| b == sent)
                {
                    // Только из «Отправленных»: убранное потом из «Входящих» (в «Архив», «Удалённые») не возвращается.
                    p.mailboxes.push((inbox.clone(), true));
                    out.notices.push(Notice::NoReply { id: row.id.clone(), subject: row.subject.clone() });
                }
            }
        }
        // Исчезнет.
        if !row.has_keyword(KEEP)
            && let Some(w) = marks::find(&row.keywords, Kind::Expire)
        {
            let m = at(Kind::Expire, w);
            let target = match rules.expire_to {
                ExpireTo::Trash => roles.trash.clone(),
                ExpireTo::Archive => Some(roles.archive_or_new()),
            };
            if m > ts {
                out.wake(m);
            } else if let Some(target) = target {
                p.keywords.extend(marks::clearing(&row.keywords, Kind::Expire).into_iter().map(|k| (k, false)));
                p.mailboxes.retain(|(b, _)| *b != target);
                p.mailboxes.extend(row.mailbox_ids.iter().filter(|b| **b != target).map(|b| (b.clone(), false)));
                p.mailboxes.push((target, true));
                expired += 1;
            }
        }
        if !p.is_empty() {
            out.patches.push(p);
        }
    }
    if expired > 0 {
        out.notices.push(Notice::Expired(expired));
    }
    // Коды подтверждения: «Исчезнет» через N часов после получения.
    if let Some(after) = rules.expire_codes() {
        let after = i64::try_from(after.as_secs()).unwrap_or(i64::MAX);
        for row in &input.recent {
            if marks::has(&row.keywords, Kind::Expire) || row.has_keyword(KEEP) || !is_code(&row.subject, &row.preview) {
                continue;
            }
            let Some(w) = when_at(row.received_at.saturating_add(after), tz) else { continue };
            let (add, remove) = marks::setting(&row.keywords, Kind::Expire, w);
            out.patches.push(Patch { id: row.id.clone(), keywords: keywords(add, remove), mailboxes: Vec::new() });
            out.wake(at(Kind::Expire, w));
        }
    }
    out
}

// --- эвристики ---

/// Слова, рядом с которыми ищется код (нижний регистр); `true` — только целым словом.
const CODE_WORDS: &[(&str, bool)] = &[
    ("код", false),
    ("code", false),
    ("парол", false),
    ("passcode", false),
    ("password", false),
    ("verification", false),
    ("verify", false),
    ("подтвержд", false),
    ("одноразов", false),
    ("one-time", false),
    ("otp", true),
    ("pin", true),
    ("2fa", true),
];
/// Код ищется в стольких знаках от слова (до и после).
const CODE_REACH: usize = 48;

/// Письмо с одноразовым кодом: рядом со словом «код», «code», «пароль», «verification»… (в теме или начале текста)
/// стоит число из 4–8 цифр (можно «123 456», «123-456»); четыре цифры, похожие на год (1900–2099), — не код.
pub fn is_code(subject: &str, preview: &str) -> bool {
    let text: Vec<char> = format!("{subject} \u{2029} {preview}").to_lowercase().chars().collect();
    let words: Vec<(Vec<char>, bool)> = CODE_WORDS.iter().map(|(w, whole)| (w.chars().collect(), *whole)).collect();
    for i in 0..text.len() {
        if i > 0 && text[i - 1].is_alphanumeric() {
            continue;
        }
        for (w, whole) in &words {
            let end = i + w.len();
            if end > text.len() || text[i..end] != w[..] || (*whole && text.get(end).is_some_and(|c| c.is_alphanumeric())) {
                continue;
            }
            let from = i.saturating_sub(CODE_REACH);
            let to = (end + CODE_REACH).min(text.len());
            if has_code_number(&text[from..to]) {
                return true;
            }
        }
    }
    false
}

/// Есть число-код: 4–8 цифр подряд (или две группы по 3–4 через пробел или дефис), не часть слова, не год.
fn has_code_number(s: &[char]) -> bool {
    let mut i = 0;
    while i < s.len() {
        if !s[i].is_ascii_digit() || (i > 0 && (s[i - 1].is_alphanumeric() && !s[i - 1].is_ascii_digit())) {
            i += 1;
            continue;
        }
        let run = |at: usize| s[at..].iter().take_while(|c| c.is_ascii_digit()).count();
        let first = run(i);
        let mut end = i + first;
        let mut digits = first;
        // «123 456», «123-456».
        if (3..=4).contains(&first)
            && s.get(end).is_some_and(|c| *c == ' ' || *c == '-')
            && let Some(second) = s.get(end + 1).filter(|c| c.is_ascii_digit()).map(|_| run(end + 1))
            && (3..=4).contains(&second)
        {
            digits += second;
            end += 1 + second;
        }
        let bounded = s.get(end).is_none_or(|c| !c.is_alphanumeric());
        let year = digits == 4 && {
            let n: String = s[i..end].iter().collect();
            n.parse::<u32>().is_ok_and(|y| (1900..2100).contains(&y))
        };
        if bounded && (4..=8).contains(&digits) && !year {
            return true;
        }
        i = end.max(i + 1);
    }
    false
}

/// Рассылка во «Входящих»: письмо с `List-Id` или `List-Unsubscribe` (RFC 2919, RFC 2369). Заголовки читает сервер
/// (у JMAP — `postman_jmap::Client::list_letters` в плагине `postman-backend-jmap`): в кэше их нет, а условие `header` в `Email/query` Stalwart не выполняет.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewsItem {
    pub id: String,
    /// Адрес отправителя.
    pub sender: String,
    pub received_at: i64,
    pub seen: bool,
}

/// Какие рассылки — в «Архив»: прочитанные, полученные раньше `cutoff`, кроме последней от каждого отправителя.
pub fn newsletter_sweep(items: &[NewsItem], cutoff: i64) -> Vec<String> {
    let mut latest: std::collections::HashMap<String, (i64, &str)> = std::collections::HashMap::new();
    for n in items {
        let key = n.sender.trim().to_lowercase();
        let e = latest.entry(key).or_insert((n.received_at, &n.id));
        if (n.received_at, n.id.as_str()) > (e.0, e.1) {
            *e = (n.received_at, &n.id);
        }
    }
    let keep: HashSet<&str> = latest.values().map(|(_, id)| *id).collect();
    items.iter().filter(|n| n.seen && n.received_at < cutoff && !keep.contains(n.id.as_str())).map(|n| n.id.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tz() -> TimeZone {
        TimeZone::UTC
    }

    fn zoned(s: &str) -> Zoned {
        s.parse().unwrap()
    }

    fn date(s: &str) -> Date {
        s.parse().unwrap()
    }

    fn time(h: i8, m: i8) -> Time {
        Time::new(h, m, 0, 0).unwrap()
    }

    fn row(id: &str, boxes: &[&str], keywords: &[String]) -> EmailRow {
        EmailRow {
            id: id.into(),
            thread_id: format!("t-{id}"),
            mailbox_ids: boxes.iter().map(|s| s.to_string()).collect(),
            received_at: 0,
            subject: format!("s-{id}"),
            from_name: String::new(),
            from_addr: "me@corp.example".into(),
            preview: String::new(),
            seen: true,
            flagged: false,
            has_attachment: false,
            size: 1,
            keywords: keywords.to_vec(),
        }
    }

    fn words(kind: Kind, w: When) -> Vec<String> {
        vec![kind.base().to_owned(), kind.keyword(w)]
    }

    fn roles() -> Roles {
        Roles { inbox: Some("in".into()), archive: Some("arch".into()), trash: Some("trash".into()), sent: Some("sent".into()) }
    }

    fn me() -> Me {
        Me::new(["me@corp.example"], ["Me"])
    }

    /// Четверг, 22 октября 2026, 15:00 UTC.
    fn now() -> Zoned {
        zoned("2026-10-22T15:00:00+00:00[UTC]")
    }

    #[test]
    fn moments_of_dates_and_times() {
        let d = date("2026-10-22");
        let ts = |s: &str| zoned(s).timestamp();
        assert_eq!(moment(Kind::Due, When::day(d), &tz()), Some(ts("2026-10-23T00:00:00+00:00[UTC]")), "срок — до конца дня");
        assert_eq!(moment(Kind::Wait, When::day(d), &tz()), Some(ts("2026-10-22T00:00:00+00:00[UTC]")));
        assert_eq!(moment(Kind::Remind, When::at(d, time(9, 0)), &tz()), Some(ts("2026-10-22T09:00:00+00:00[UTC]")));
        // Минуты — вверх.
        let at = ts("2026-10-22T09:00:30+00:00[UTC]").as_second();
        assert_eq!(when_at(at, &tz()), Some(When::at(d, time(9, 1))));
        assert_eq!(when_at(at - 30, &tz()), Some(When::at(d, time(9, 0))));
    }

    /// Метки строки: «До 15:00» сегодня, «До чт», «Просрочено 1 д», ожидание, напоминание, «Исчезнет через».
    #[test]
    fn badges_by_now() {
        let now = now();
        let b = |kw: Vec<String>| badges(&kw, &now);
        let today = date("2026-10-22");
        assert_eq!(b(words(Kind::Due, When::at(today, time(18, 0)))), [Badge::Due(Some(Ahead { day: Day::Today, time: Some(time(18, 0)) }))]);
        assert_eq!(b(words(Kind::Due, When::day(today))), [Badge::Due(Some(Ahead { day: Day::Today, time: None }))], "до конца дня");
        assert_eq!(b(words(Kind::Due, When::day(date("2026-10-23")))), [Badge::Due(Some(Ahead { day: Day::Tomorrow, time: None }))]);
        assert_eq!(
            b(words(Kind::Due, When::day(date("2026-10-29")))),
            [Badge::Due(Some(Ahead { day: Day::Date(date("2026-10-29")), time: None }))],
            "через неделю — дата"
        );
        assert_eq!(b(words(Kind::Due, When::day(date("2026-10-27")))), [Badge::Due(Some(Ahead { day: Day::Weekday(Weekday::Tuesday), time: None }))]);
        // Прошёл.
        assert_eq!(b(words(Kind::Due, When::day(date("2026-10-21")))), [Badge::Overdue(Span::Days(1))]);
        assert_eq!(b(words(Kind::Due, When::at(today, time(12, 30)))), [Badge::Overdue(Span::Hours(3))]);
        assert_eq!(b(words(Kind::Due, When::at(date("2026-10-19"), time(9, 0)))), [Badge::Overdue(Span::Days(3))]);
        assert!(Badge::Overdue(Span::Days(1)).filled() && Badge::Due(None).amber() && !Badge::Waiting(None).amber());
        // Ожидание: дней до срока; прошёл — «нет ответа».
        assert_eq!(b(words(Kind::Wait, When::day(date("2026-10-25")))), [Badge::Waiting(Some(3))]);
        assert_eq!(b(words(Kind::Wait, When::day(date("2026-10-20")))), [Badge::NoReply(2)]);
        assert_eq!(b(vec!["pm.wait".into()]), [Badge::Waiting(None)], "общее слово без даты");
        // Напомнит и исчезнет.
        assert_eq!(
            b(words(Kind::Remind, When::at(date("2026-10-23"), time(9, 0)))),
            [Badge::Remind(Ahead { day: Day::Tomorrow, time: Some(time(9, 0)) })]
        );
        assert_eq!(b(words(Kind::Expire, When::at(date("2026-10-25"), time(16, 0)))), [Badge::Expires(Some(Span::Days(3)))]);
        assert_eq!(b(words(Kind::Expire, When::at(today, time(20, 0)))), [Badge::Expires(Some(Span::Hours(5)))]);
        let mut kept = words(Kind::Expire, When::day(today));
        kept.push(KEEP.into());
        assert!(b(kept).is_empty(), "«Никогда»");
        // Порядок: срок, напоминание, ожидание, «Исчезнет»; без слов Postman — пусто.
        let mut all = words(Kind::Expire, When::at(today, time(20, 0)));
        all.extend(words(Kind::Wait, When::day(today)));
        all.extend(words(Kind::Due, When::day(today)));
        let got = b(all);
        assert!(matches!(got.as_slice(), [Badge::Due(_), Badge::Waiting(Some(0)), Badge::Expires(_)]), "{got:?}");
        assert!(b(vec!["pm.cat.team".into(), "$label1".into()]).is_empty());
    }

    /// Варианты панели: сегодня, завтра, пятница, следующая неделя; «позже сегодня», понедельник; дни.
    #[test]
    fn quick_picks() {
        let now = now(); // чт 22.10, 15:00
        let w = |p: Pick, k: Kind| p.when(k, &now).unwrap();
        assert_eq!(w(Pick::Today, Kind::Due), When::day(date("2026-10-22")));
        assert_eq!(w(Pick::Tomorrow, Kind::Due), When::day(date("2026-10-23")));
        assert_eq!(w(Pick::Friday, Kind::Due), When::day(date("2026-10-23")));
        assert_eq!(w(Pick::NextWeek, Kind::Due), When::day(date("2026-10-30")));
        assert_eq!(w(Pick::LaterToday, Kind::Remind), When::at(date("2026-10-22"), time(18, 0)));
        assert_eq!(w(Pick::Tomorrow, Kind::Remind), When::at(date("2026-10-23"), time(9, 0)));
        assert_eq!(w(Pick::Monday, Kind::Remind), When::at(date("2026-10-26"), time(9, 0)));
        assert_eq!(w(Pick::Days(3), Kind::Wait), When::day(date("2026-10-25")));
        assert_eq!(w(Pick::Days(3), Kind::Expire), When::at(date("2026-10-25"), time(15, 0)));
        // В пятницу «в пятницу» — следующая; поздно вечером «позже сегодня» — завтра утром.
        let friday = zoned("2026-10-23T20:30:00+00:00[UTC]");
        assert_eq!(Pick::Friday.when(Kind::Due, &friday), Some(When::day(date("2026-10-30"))));
        assert_eq!(Pick::LaterToday.when(Kind::Remind, &friday), Some(When::at(date("2026-10-24"), time(9, 0))));
        assert_eq!(picks(Kind::Wait).len(), marks::WAIT_DAYS.len());
        assert!(picks(Kind::Due).contains(&Pick::NextWeek) && picks(Kind::Remind).contains(&Pick::LaterToday));
    }

    #[test]
    fn typed_dates() {
        let now = now();
        assert_eq!(parse_when("24.10.2026", Kind::Due, &now), Ok(When::day(date("2026-10-24"))));
        assert_eq!(parse_when("24.10 15:30", Kind::Due, &now), Ok(When::at(date("2026-10-24"), time(15, 30))));
        assert_eq!(parse_when("2026-10-24", Kind::Remind, &now), Ok(When::at(date("2026-10-24"), time(9, 0))));
        assert_eq!(parse_when("24.10.2026 18:00", Kind::Wait, &now), Ok(When::day(date("2026-10-24"))));
        assert_eq!(parse_when("22.10.2026", Kind::Due, &now), Ok(When::day(date("2026-10-22"))), "до конца дня");
        assert_eq!(parse_when("22.10.2026 09:00", Kind::Remind, &now), Err(Bad::Past));
        assert_eq!(parse_when("20.10.2026", Kind::Expire, &now), Err(Bad::Past));
        for bad in ["", "завтра", "32.10.2026", "24.10.2026 25:00", "24.10.2026 15:00 x"] {
            assert_eq!(parse_when(bad, Kind::Due, &now), Err(Bad::Format), "{bad}");
        }
    }

    #[test]
    fn on_top_is_overdue_or_reminded() {
        let now = now();
        let mut r = row("a", &["in"], &words(Kind::Due, When::day(date("2026-10-21"))));
        assert!(on_top(&r, &now, true) && !on_top(&r, &now, false));
        r.keywords = vec!["pm.remind".into()];
        r.seen = false;
        assert!(on_top(&r, &now, false), "напомнило, не прочитано");
        r.seen = true;
        assert!(!on_top(&r, &now, false));
        r.keywords = words(Kind::Remind, When::at(date("2026-10-23"), time(9, 0)));
        r.seen = false;
        assert!(!on_top(&r, &now, false), "ещё не напомнило");
    }

    #[test]
    fn user_actions_become_patches() {
        let r = row("a", &["in"], &["pm.cat.team".to_string(), "pm.due".into(), "pm.due.2026-10-20".into()]);
        let w = When::day(date("2026-10-24"));
        let p = set(&r, Kind::Due, Some(w));
        assert_eq!(p.keywords, [("pm.due.2026-10-20".into(), false), ("pm.due".into(), true), ("pm.due.2026-10-24".into(), true)]);
        let p = set(&r, Kind::Due, None);
        assert_eq!(p.keywords, [("pm.due".into(), false), ("pm.due.2026-10-20".into(), false)]);
        // «Напомнить» из «Входящих» — в «Архив»; из другой папки — на месте.
        let at = When::at(date("2026-10-23"), time(9, 0));
        let p = snooze(&r, at, &roles());
        assert_eq!(p.mailboxes, [("in".into(), false), ("arch".into(), true)]);
        assert!(p.keywords.contains(&("pm.remind.2026-10-23t0900".into(), true)));
        assert!(snooze(&row("b", &["proj"], &[]), at, &roles()).mailboxes.is_empty());
        // «Архива» нет — создать; кэшу — без него (письмо на месте до ответа сервера).
        let none = Roles { archive: None, ..roles() };
        let p = snooze(&r, at, &none);
        assert_eq!(p.mailboxes, [("in".into(), false), (ARCHIVE.into(), true)]);
        assert_eq!(for_cache(&p).mailboxes, [("in".into(), false)]);
        let mut cached = r.clone();
        for_cache(&p).apply(&mut cached);
        assert_eq!(cached.mailbox_ids, ["in"]);
        // «Никогда» и снова срок.
        let p = keep(&row("c", &["in"], &words(Kind::Expire, w)));
        assert!(p.keywords.contains(&(KEEP.into(), true)) && p.keywords.contains(&("pm.expire.2026-10-24".into(), false)));
        let p = set(&row("c", &["in"], &[KEEP.into()]), Kind::Expire, Some(w));
        assert!(p.keywords.contains(&(KEEP.into(), false)));
    }

    fn item(r: EmailRow) -> Item {
        Item { row: r, to: Recipients::default(), thread: Vec::new() }
    }

    /// Напоминание в срок: во «Входящие» из «Архива», непрочитанным, тост; будущее — ближайший срок таймера.
    #[test]
    fn reminders_fire_and_set_the_next_wake() {
        let now = now();
        let due = row("a", &["arch"], &words(Kind::Remind, When::at(date("2026-10-22"), time(9, 0))));
        let later = row("b", &["arch"], &words(Kind::Remind, When::at(date("2026-10-23"), time(9, 0))));
        let soon = row("c", &["in"], &words(Kind::Remind, When::at(date("2026-10-22"), time(16, 30))));
        let input = Input { items: vec![item(due), item(later), item(soon)], recent: vec![] };
        let p = plan(&input, &roles(), &Lifecycle::default(), &me(), &now);
        assert_eq!(p.patches.len(), 1);
        let patch = &p.patches[0];
        assert_eq!(patch.id, "a");
        assert_eq!(patch.mailboxes, [("in".into(), true), ("arch".into(), false)]);
        assert_eq!(patch.keywords, [("pm.remind.2026-10-22t0900".into(), false), ("pm.remind".into(), true), ("$seen".into(), false)]);
        assert_eq!(p.notices, [Notice::Reminded { id: "a".into(), subject: "s-a".into() }]);
        assert_eq!(p.next, Some(zoned("2026-10-22T16:30:00+00:00[UTC]").timestamp().as_second()), "ближайший — 16:30");
        // Применили — «напомнило»: наверху; прочитали — общее слово снимается следующим проходом.
        let mut r = input.items[0].row.clone();
        patch.apply(&mut r);
        assert!(reminded(&r) && r.mailbox_ids == ["in"]);
        r.seen = true;
        let p = plan(&Input { items: vec![item(r)], recent: vec![] }, &roles(), &Lifecycle::default(), &me(), &now);
        assert_eq!(p.patches[0].keywords, [("pm.remind".into(), false)]);
        assert_eq!(p.next, None, "сроков нет — таймера нет");
    }

    /// «Ждём ответа»: ответ адресата в беседе снимает ожидание; без ответа срок прошёл — во «Входящие» (один раз).
    #[test]
    fn waiting_is_cleared_by_a_reply() {
        let now = now();
        let mut sent = row("a", &["sent"], &words(Kind::Wait, When::day(date("2026-10-21"))));
        sent.received_at = 100;
        let to = Recipients { to: vec!["anna@vendor.example".into()], cc: vec![] };
        let mut reply = row("b", &["in"], &[]);
        reply.thread_id = sent.thread_id.clone();
        reply.received_at = 200;
        reply.from_addr = "Anna@Vendor.example".into();
        let mut other = reply.clone();
        other.id = "c".into();
        other.from_addr = "bob@vendor.example".into();
        assert!(replied(&sent, &to, &[sent.clone(), reply.clone()], &me()));
        assert!(!replied(&sent, &to, &[sent.clone(), other.clone()], &me()), "не адресат");
        let mut early = reply.clone();
        early.received_at = 50;
        assert!(!replied(&sent, &to, &[early], &me()), "раньше письма");
        // Адресаты неизвестны — кто угодно, кроме меня.
        assert!(replied(&sent, &Recipients::default(), &[other.clone()], &me()));
        let mut mine = other.clone();
        mine.from_addr = "me@corp.example".into();
        assert!(!replied(&sent, &Recipients::default(), &[mine], &me()));

        let waiting = |thread: Vec<EmailRow>, boxes: &[&str]| {
            let mut r = sent.clone();
            r.mailbox_ids = boxes.iter().map(|s| s.to_string()).collect();
            Input { items: vec![Item { row: r, to: to.clone(), thread }], recent: vec![] }
        };
        // Ответ пришёл: снять слова; письмо, возвращённое во «Входящие», — оттуда убрать.
        let p = plan(&waiting(vec![reply.clone()], &["sent", "in"]), &roles(), &Lifecycle::default(), &me(), &now);
        assert_eq!(p.patches[0].keywords, [("pm.wait".into(), false), ("pm.wait.2026-10-21".into(), false)]);
        assert_eq!(p.patches[0].mailboxes, [("in".into(), false)]);
        assert!(matches!(p.notices.as_slice(), [Notice::Replied { .. }]));
        // Нет ответа, срок прошёл: из «Отправленных» — во «Входящие».
        let p = plan(&waiting(vec![], &["sent"]), &roles(), &Lifecycle::default(), &me(), &now);
        assert_eq!(p.patches[0].mailboxes, [("in".into(), true)]);
        assert!(matches!(p.notices.as_slice(), [Notice::NoReply { .. }]));
        // Уже убрали из «Входящих» в «Архив» — не возвращать; правило выключено — тоже.
        assert!(plan(&waiting(vec![], &["sent", "arch"]), &roles(), &Lifecycle::default(), &me(), &now).patches.is_empty());
        let off = Lifecycle { wait_reply: false, ..Lifecycle::default() };
        assert!(plan(&waiting(vec![], &["sent"]), &roles(), &off, &me(), &now).patches.is_empty());
        // Срок впереди — проснуться к нему.
        let mut ahead = sent.clone();
        ahead.keywords = words(Kind::Wait, When::day(date("2026-10-24")));
        let p = plan(&Input { items: vec![item(ahead)], recent: vec![] }, &roles(), &Lifecycle::default(), &me(), &now);
        assert_eq!(p.next, Some(zoned("2026-10-24T00:00:00+00:00[UTC]").timestamp().as_second()));
    }

    /// «Исчезнет»: в срок — в «Удалённые» или «Архив» из всех папок, слова снимаются.
    #[test]
    fn expiry_moves_the_letter() {
        let now = now();
        let gone = row("a", &["in", "proj"], &words(Kind::Expire, When::at(date("2026-10-22"), time(14, 0))));
        let p = plan(&Input { items: vec![item(gone.clone())], recent: vec![] }, &roles(), &Lifecycle::default(), &me(), &now);
        assert_eq!(p.patches[0].mailboxes, [("in".into(), false), ("proj".into(), false), ("trash".into(), true)]);
        assert_eq!(p.patches[0].keywords, [("pm.expire".into(), false), ("pm.expire.2026-10-22t1400".into(), false)]);
        assert_eq!(p.notices, [Notice::Expired(1)]);
        let to_archive = Lifecycle { expire_to: ExpireTo::Archive, ..Lifecycle::default() };
        let p = plan(&Input { items: vec![item(gone.clone())], recent: vec![] }, &roles(), &to_archive, &me(), &now);
        assert_eq!(p.patches[0].mailboxes.last(), Some(&("arch".into(), true)));
        // Нет «Удалённых» — ничего не делать; нет «Архива» — создать.
        let p = plan(&Input { items: vec![item(gone.clone())], recent: vec![] }, &Roles::default(), &Lifecycle::default(), &me(), &now);
        assert!(p.patches.is_empty());
        let p = plan(&Input { items: vec![item(gone)], recent: vec![] }, &Roles::default(), &to_archive, &me(), &now);
        assert_eq!(p.patches[0].mailboxes.last(), Some(&(ARCHIVE.into(), true)));
    }

    /// Коды подтверждения: «Исчезнет» через N часов после получения; уже со сроком или «Никогда» — не трогать.
    #[test]
    fn codes_get_an_expiry() {
        let now = now();
        let mut code = row("a", &["in"], &[]);
        code.subject = "Код подтверждения".into();
        code.preview = "Ваш код: 482 913. Никому его не сообщайте.".into();
        code.received_at = zoned("2026-10-22T10:00:00+00:00[UTC]").timestamp().as_second();
        let mut plain = code.clone();
        plain.id = "b".into();
        plain.subject = "Отчёт".into();
        plain.preview = "Пентест: исходный код v2 за 2026 год".into();
        let mut kept = code.clone();
        kept.id = "c".into();
        kept.keywords = vec![KEEP.into()];
        let input = Input { items: vec![], recent: vec![code, plain, kept] };
        let p = plan(&input, &roles(), &Lifecycle::default(), &me(), &now);
        assert_eq!(p.patches.len(), 1);
        assert_eq!(p.patches[0].keywords, [("pm.expire".into(), true), ("pm.expire.2026-10-23t1000".into(), true)]);
        assert_eq!(p.next, Some(zoned("2026-10-23T10:00:00+00:00[UTC]").timestamp().as_second()));
        let off = Lifecycle { expire_codes: false, ..Lifecycle::default() };
        assert!(plan(&input, &roles(), &off, &me(), &now).patches.is_empty());
    }

    #[test]
    fn code_heuristic() {
        for (s, p) in [
            ("Код подтверждения", "Ваш код: 482913"),
            ("Your verification code", "is 123 456"),
            ("", "G-582910 is your Google verification code."),
            ("Вход в портал", "Одноразовый пароль 7781-334, действует 10 минут"),
            ("PIN", "4821"),
            ("Код для входа: 5521", ""),
        ] {
            assert!(is_code(s, p), "{s} / {p}");
        }
        for (s, p) in [
            ("Пентест «Альфа»: исходный код v2", "Посмотри до пятницы"),
            ("Отчёт за 2026", "код проекта в задаче"),
            ("Code review 2026", ""),
            ("Счёт 48291", "оплатите до 25.10"),
            ("Ping", "server 12345 down"),
            ("Пароль изменён", "Если это были не вы, напишите в поддержку"),
            ("Код", "заказ 123456789"),
        ] {
            assert!(!is_code(s, p), "{s} / {p}");
        }
    }

    #[test]
    fn newsletters_keep_the_latest_per_sender() {
        let n = |id: &str, sender: &str, at: i64, seen: bool| NewsItem { id: id.into(), sender: sender.into(), received_at: at, seen };
        let items = [
            n("a1", "news@a.example", 10, true),
            n("a2", "news@a.example", 20, true),
            n("a3", "NEWS@a.example", 30, true),
            n("b1", "digest@b.example", 15, true),
            n("c1", "c@c.example", 5, false),
            n("c2", "c@c.example", 6, true),
        ];
        let mut got = newsletter_sweep(&items, 25);
        got.sort();
        // a3 — последняя от a (остаётся), a1 и a2 — старше срока; b1 — единственная; c1 не прочитана.
        assert_eq!(got, ["a1", "a2"]);
        assert_eq!(newsletter_sweep(&items, 100), ["a1", "a2"].map(String::from).into_iter().collect::<Vec<_>>());
    }

    /// Из кэша: письма со словами сроков по индексу (и слова без общего — от прежних версий), беседа ожидающих,
    /// свежие «Входящие».
    #[test]
    fn gather_reads_the_keyword_index() {
        let cache = Cache::open_in_memory().unwrap();
        let now = 1_000_000;
        let mut wait = row("w", &["sent"], &["pm.wait.2026-10-25".to_string()]);
        wait.received_at = now - 10;
        let mut reply = row("r", &["in"], &[]);
        reply.thread_id = wait.thread_id.clone();
        reply.received_at = now - 5;
        let mut due = row("d", &["in"], &words(Kind::Due, When::day(date("2026-10-25"))));
        due.received_at = now - 2 * CODE_WINDOW;
        let mut other = row("o", &["in"], &["pm.duet".to_string(), "pm.cat.team".into()]);
        other.received_at = now - 1;
        cache.upsert_emails(&[wait, reply, due, other]).unwrap();
        cache.set_recipients(&[("w".into(), Recipients { to: vec!["anna@x".into()], cc: vec![] })]).unwrap();
        let input = gather(&cache, &roles(), now).unwrap();
        let mut ids: Vec<&str> = input.items.iter().map(|i| i.row.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, ["d", "w"], "pm.duet — не срок");
        let w = input.items.iter().find(|i| i.row.id == "w").unwrap();
        assert_eq!((w.to.to.len(), w.thread.len()), (1, 2));
        let recent: Vec<&str> = input.recent.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(recent, ["o", "r"], "старше окна кодов — нет");
    }
}
