//! Календарь EWS: папки (`GetFolder`, `FindFolder`), изменения опросом (`SyncFolderItems`), окно с развёрнутыми
//! повторами (`FindItem` + `CalendarView`), встречи целиком (`GetItem`) → [`postman_calendar::Event`], создание,
//! правка и удаление с рассылкой (`CreateItem`/`UpdateItem`/`DeleteItem`), ответы на приглашения
//! (`AcceptItem`/`TentativelyAcceptItem`/`DeclineItem`).
//!
//! Времена — UTC (`2026-10-20T07:00:00Z`); пояс встречи для сервера — Windows id ([`crate::tz`]): по нему Exchange
//! разворачивает повторы и переводы часов.

use jiff::civil::Date;
use postman_calendar::{Event, EventStatus, FreeBusy, NewEvent, Participant, Partstat, Span};

use crate::soap::{self, id_el, text_el};
use crate::xml::{El, esc};
use crate::{BATCH, Client, Error, Result};

/// Объект сервера: `Id` и `ChangeKey` (версия; без неё правка и ответ — по последней).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ItemId {
    pub id: String,
    pub change_key: Option<String>,
}

impl ItemId {
    pub fn new(id: impl Into<String>) -> ItemId {
        ItemId { id: id.into(), change_key: None }
    }

    pub(crate) fn xml(&self, name: &str) -> String {
        id_el(name, &self.id, self.change_key.as_deref())
    }

    pub(crate) fn from_el(el: &El) -> Option<ItemId> {
        Some(ItemId { id: el.attr("Id")?.to_owned(), change_key: el.attr("ChangeKey").map(str::to_owned) })
    }
}

/// Папка сервера.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FolderId {
    pub id: String,
    pub change_key: Option<String>,
}

/// Стандартные папки ящика, которые нужны: календарь — для встреч; остальные — найти свои объекты по метке
/// (уборка тестов, приглашения, которые сервер положил во «Входящие»).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WellKnown {
    Calendar,
    Inbox,
    Drafts,
    SentItems,
    DeletedItems,
    /// Личные контакты (T1-25а).
    Contacts,
    /// Корень почтовых папок ящика (`msgfolderroot`, верхний уровень дерева): родитель при создании и переносе папки.
    Root,
}

impl WellKnown {
    pub(crate) fn id(self) -> &'static str {
        match self {
            WellKnown::Calendar => "calendar",
            WellKnown::Inbox => "inbox",
            WellKnown::Drafts => "drafts",
            WellKnown::SentItems => "sentitems",
            WellKnown::DeletedItems => "deleteditems",
            WellKnown::Contacts => "contacts",
            WellKnown::Root => "msgfolderroot",
        }
    }
}

/// Папка в запросе: стандартная или по id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderRef {
    WellKnown(WellKnown),
    Id(FolderId),
}

impl FolderRef {
    pub(crate) fn xml(&self) -> String {
        match self {
            FolderRef::WellKnown(w) => format!(r#"<t:DistinguishedFolderId Id="{}"/>"#, w.id()),
            FolderRef::Id(f) => id_el("t:FolderId", &f.id, f.change_key.as_deref()),
        }
    }
}

/// Папка календаря.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub id: FolderId,
    pub name: String,
    pub total: u32,
}

/// Состояние синхронизации папки (`SyncState`) — непрозрачная строка сервера; хранится в кэше как есть.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncState(pub String);

impl SyncState {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Изменения с прошлого состояния.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncChanges {
    pub state: SyncState,
    pub created: Vec<ItemId>,
    pub updated: Vec<ItemId>,
    /// Удалённые: только `Id` (версии у удалённого нет).
    pub deleted: Vec<String>,
    /// `false` — это не все изменения: запросить ещё с новым `state`.
    pub done: bool,
}

/// Вид объекта календаря (`CalendarItemType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ItemKind {
    #[default]
    Single,
    /// Экземпляр повторяющейся встречи (только из `CalendarView`).
    Occurrence,
    /// Изменённый экземпляр.
    Exception,
    /// Сама серия.
    RecurringMaster,
}

impl ItemKind {
    fn parse(s: &str) -> ItemKind {
        match s {
            "Occurrence" => ItemKind::Occurrence,
            "Exception" => ItemKind::Exception,
            "RecurringMaster" => ItemKind::RecurringMaster,
            _ => ItemKind::Single,
        }
    }
}

/// Строка окна календаря (`CalendarView`): что и когда; подробности — [`Client::get_items`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemRef {
    pub id: ItemId,
    pub span: Span,
    pub kind: ItemKind,
}

/// Изменённый экземпляр серии (`ModifiedOccurrences`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    pub id: ItemId,
    pub span: Span,
    /// Где экземпляр был по правилу серии (Unix-секунды).
    pub original_start: i64,
}

/// День недели в правиле повтора; `Day`, `Weekday`, `WeekendDay` — группы для «первый рабочий день месяца».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DayOfWeek {
    Sunday,
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Day,
    Weekday,
    WeekendDay,
}

const DAYS: [(DayOfWeek, &str, &[&str]); 10] = [
    (DayOfWeek::Sunday, "Sunday", &["SU"]),
    (DayOfWeek::Monday, "Monday", &["MO"]),
    (DayOfWeek::Tuesday, "Tuesday", &["TU"]),
    (DayOfWeek::Wednesday, "Wednesday", &["WE"]),
    (DayOfWeek::Thursday, "Thursday", &["TH"]),
    (DayOfWeek::Friday, "Friday", &["FR"]),
    (DayOfWeek::Saturday, "Saturday", &["SA"]),
    (DayOfWeek::Day, "Day", &["SU", "MO", "TU", "WE", "TH", "FR", "SA"]),
    (DayOfWeek::Weekday, "Weekday", &["MO", "TU", "WE", "TH", "FR"]),
    (DayOfWeek::WeekendDay, "WeekendDay", &["SA", "SU"]),
];

impl DayOfWeek {
    fn ews(self) -> &'static str {
        DAYS.iter().find(|d| d.0 == self).map_or("Monday", |d| d.1)
    }

    fn ical(self) -> &'static [&'static str] {
        DAYS.iter().find(|d| d.0 == self).map_or(&[], |d| d.2)
    }

    fn parse(s: &str) -> Option<DayOfWeek> {
        DAYS.iter().find(|d| d.1 == s).map(|d| d.0)
    }
}

/// Какая по счёту неделя месяца.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeekIndex {
    First,
    Second,
    Third,
    Fourth,
    Last,
}

impl WeekIndex {
    const ALL: [(WeekIndex, &'static str, i8); 5] = [
        (WeekIndex::First, "First", 1),
        (WeekIndex::Second, "Second", 2),
        (WeekIndex::Third, "Third", 3),
        (WeekIndex::Fourth, "Fourth", 4),
        (WeekIndex::Last, "Last", -1),
    ];

    fn ews(self) -> &'static str {
        Self::ALL.iter().find(|w| w.0 == self).map_or("First", |w| w.1)
    }

    fn pos(self) -> i8 {
        Self::ALL.iter().find(|w| w.0 == self).map_or(1, |w| w.2)
    }

    fn parse(s: &str) -> Option<WeekIndex> {
        Self::ALL.iter().find(|w| w.1 == s).map(|w| w.0)
    }
}

const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// Правило повтора EWS (месяцы — 1…12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern {
    Daily { interval: u32 },
    Weekly { interval: u32, days: Vec<DayOfWeek>, first_day: Option<DayOfWeek> },
    AbsoluteMonthly { interval: u32, day: u8 },
    RelativeMonthly { interval: u32, days: DayOfWeek, index: WeekIndex },
    AbsoluteYearly { month: u8, day: u8 },
    RelativeYearly { month: u8, days: DayOfWeek, index: WeekIndex },
}

/// Конец серии; `start` — дата первого экземпляра (в поясе встречи).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Range {
    NoEnd { start: Date },
    EndDate { start: Date, end: Date },
    Numbered { start: Date, count: u32 },
}

/// Повтор серии (`Recurrence` у `RecurringMaster`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recurrence {
    pub pattern: Pattern,
    pub range: Range,
}

fn month_name(m: u8) -> &'static str {
    MONTHS.get(usize::from(m.clamp(1, 12)) - 1).copied().unwrap_or("January")
}

fn parse_date(s: &str) -> Option<Date> {
    // `2026-10-05`, `2026-10-05Z`, `2026-10-05+03:00` — дата без пояса.
    s.trim().get(..10)?.parse().ok()
}

impl Recurrence {
    /// Правило RFC 5545 (`FREQ=WEEKLY;INTERVAL=1;BYDAY=MO,WE;COUNT=3`) — нейтральная форма для модели Postman.
    /// `UNTIL` — дата конца серии включительно, в поясе встречи.
    pub fn rrule(&self) -> String {
        let days = |d: &[DayOfWeek]| d.iter().flat_map(|d| d.ical().iter().copied()).collect::<Vec<_>>().join(",");
        let mut r = match &self.pattern {
            Pattern::Daily { interval } => format!("FREQ=DAILY;INTERVAL={interval}"),
            Pattern::Weekly { interval, days: d, first_day } => {
                let mut s = format!("FREQ=WEEKLY;INTERVAL={interval};BYDAY={}", days(d));
                if let Some(f) = first_day.and_then(|f| f.ical().first().copied()) {
                    s.push_str(&format!(";WKST={f}"));
                }
                s
            }
            Pattern::AbsoluteMonthly { interval, day } => format!("FREQ=MONTHLY;INTERVAL={interval};BYMONTHDAY={day}"),
            Pattern::RelativeMonthly { interval, days: d, index } => {
                format!("FREQ=MONTHLY;INTERVAL={interval};BYDAY={};BYSETPOS={}", days(&[*d]), index.pos())
            }
            Pattern::AbsoluteYearly { month, day } => format!("FREQ=YEARLY;BYMONTH={month};BYMONTHDAY={day}"),
            Pattern::RelativeYearly { month, days: d, index } => {
                format!("FREQ=YEARLY;BYMONTH={month};BYDAY={};BYSETPOS={}", days(&[*d]), index.pos())
            }
        };
        match &self.range {
            Range::NoEnd { .. } => {}
            Range::EndDate { end, .. } => r.push_str(&format!(";UNTIL={}", end.strftime("%Y%m%d"))),
            Range::Numbered { count, .. } => r.push_str(&format!(";COUNT={count}")),
        }
        r
    }

    fn to_xml(&self) -> String {
        let interval = |n: &u32| format!("<t:Interval>{}</t:Interval>", n.max(&1));
        let days = |d: &[DayOfWeek]| format!("<t:DaysOfWeek>{}</t:DaysOfWeek>", d.iter().map(|d| d.ews()).collect::<Vec<_>>().join(" "));
        let index = |i: &WeekIndex| format!("<t:DayOfWeekIndex>{}</t:DayOfWeekIndex>", i.ews());
        let month = |m: &u8| format!("<t:Month>{}</t:Month>", month_name(*m));
        let pattern = match &self.pattern {
            Pattern::Daily { interval: n } => format!("<t:DailyRecurrence>{}</t:DailyRecurrence>", interval(n)),
            Pattern::Weekly { interval: n, days: d, first_day } => format!(
                "<t:WeeklyRecurrence>{}{}{}</t:WeeklyRecurrence>",
                interval(n),
                days(d),
                first_day.map(|f| format!("<t:FirstDayOfWeek>{}</t:FirstDayOfWeek>", f.ews())).unwrap_or_default()
            ),
            Pattern::AbsoluteMonthly { interval: n, day } => {
                format!("<t:AbsoluteMonthlyRecurrence>{}<t:DayOfMonth>{day}</t:DayOfMonth></t:AbsoluteMonthlyRecurrence>", interval(n))
            }
            Pattern::RelativeMonthly { interval: n, days: d, index: i } => {
                format!("<t:RelativeMonthlyRecurrence>{}{}{}</t:RelativeMonthlyRecurrence>", interval(n), days(&[*d]), index(i))
            }
            Pattern::AbsoluteYearly { month: m, day } => {
                format!("<t:AbsoluteYearlyRecurrence><t:DayOfMonth>{day}</t:DayOfMonth>{}</t:AbsoluteYearlyRecurrence>", month(m))
            }
            Pattern::RelativeYearly { month: m, days: d, index: i } => {
                format!("<t:RelativeYearlyRecurrence>{}{}{}</t:RelativeYearlyRecurrence>", days(&[*d]), index(i), month(m))
            }
        };
        let date = |d: &Date| d.strftime("%Y-%m-%d").to_string();
        let range = match &self.range {
            Range::NoEnd { start } => format!("<t:NoEndRecurrence><t:StartDate>{}</t:StartDate></t:NoEndRecurrence>", date(start)),
            Range::EndDate { start, end } => {
                format!("<t:EndDateRecurrence><t:StartDate>{}</t:StartDate><t:EndDate>{}</t:EndDate></t:EndDateRecurrence>", date(start), date(end))
            }
            Range::Numbered { start, count } => format!(
                "<t:NumberedRecurrence><t:StartDate>{}</t:StartDate><t:NumberOfOccurrences>{count}</t:NumberOfOccurrences></t:NumberedRecurrence>",
                date(start)
            ),
        };
        format!("<t:Recurrence>{pattern}{range}</t:Recurrence>")
    }

    fn from_el(el: &El) -> Option<Recurrence> {
        let num = |e: &El, k: &str| e.text_at(&[k]).and_then(|t| t.parse::<u32>().ok());
        let day_list =
            |e: &El| -> Vec<DayOfWeek> { e.text_at(&["DaysOfWeek"]).unwrap_or("").split_whitespace().filter_map(DayOfWeek::parse).collect() };
        let one_day = |e: &El| day_list(e).first().copied();
        let index = |e: &El| e.text_at(&["DayOfWeekIndex"]).and_then(WeekIndex::parse);
        let month = |e: &El| e.text_at(&["Month"]).and_then(|m| MONTHS.iter().position(|x| *x == m)).map(|i| i as u8 + 1);
        let day_of_month = |e: &El| num(e, "DayOfMonth").map(|d| d as u8);
        let mut pattern = None;
        let mut range = None;
        for p in &el.children {
            match p.name.as_str() {
                "DailyRecurrence" => pattern = Some(Pattern::Daily { interval: num(p, "Interval")? }),
                "WeeklyRecurrence" => {
                    pattern = Some(Pattern::Weekly {
                        interval: num(p, "Interval")?,
                        days: day_list(p),
                        first_day: p.text_at(&["FirstDayOfWeek"]).and_then(DayOfWeek::parse),
                    })
                }
                "AbsoluteMonthlyRecurrence" => pattern = Some(Pattern::AbsoluteMonthly { interval: num(p, "Interval")?, day: day_of_month(p)? }),
                "RelativeMonthlyRecurrence" => {
                    pattern = Some(Pattern::RelativeMonthly { interval: num(p, "Interval")?, days: one_day(p)?, index: index(p)? })
                }
                "AbsoluteYearlyRecurrence" => pattern = Some(Pattern::AbsoluteYearly { month: month(p)?, day: day_of_month(p)? }),
                "RelativeYearlyRecurrence" => pattern = Some(Pattern::RelativeYearly { month: month(p)?, days: one_day(p)?, index: index(p)? }),
                "NoEndRecurrence" => range = Some(Range::NoEnd { start: parse_date(p.text_at(&["StartDate"])?)? }),
                "EndDateRecurrence" => {
                    range = Some(Range::EndDate { start: parse_date(p.text_at(&["StartDate"])?)?, end: parse_date(p.text_at(&["EndDate"])?)? })
                }
                "NumberedRecurrence" => {
                    range = Some(Range::Numbered { start: parse_date(p.text_at(&["StartDate"])?)?, count: num(p, "NumberOfOccurrences")? })
                }
                _ => {}
            }
        }
        Some(Recurrence { pattern: pattern?, range: range? })
    }
}

/// Встреча целиком: событие модели Postman и то, что у EWS сверх него.
#[derive(Debug, Clone, PartialEq)]
pub struct CalendarItem {
    pub id: ItemId,
    pub kind: ItemKind,
    /// `id` — `ItemId.Id`, `calendar_ids` — папка, `uid` — UID iCalendar (связь с приглашением из письма IMAP).
    pub event: Event,
    /// Мой ответ (`MyResponseType`); у организатора — `Accepted`.
    pub my_response: Partstat,
    pub is_organizer: bool,
    pub is_meeting: bool,
    /// Правило серии — у `RecurringMaster`.
    pub recurrence: Option<Recurrence>,
    /// Изменённые экземпляры серии.
    pub modified: Vec<Occurrence>,
    /// Начала удалённых экземпляров серии (Unix-секунды).
    pub deleted: Vec<i64>,
    /// У экземпляра и исключения — где он был по правилу серии.
    pub original_start: Option<i64>,
}

/// Время EWS → Unix-секунды: `…Z` или со сдвигом; без сдвига — UTC (так отвечает `GetUserAvailability`).
pub(crate) fn parse_time(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Ok(t) = s.parse::<jiff::Timestamp>() {
        return Some(t.as_second());
    }
    let dt: jiff::civil::DateTime = s.parse().ok()?;
    dt.to_zoned(jiff::tz::TimeZone::UTC).ok().map(|z| z.timestamp().as_second())
}

/// Unix-секунды → `2026-10-20T07:00:00Z`.
pub(crate) fn format_time(ts: i64) -> String {
    jiff::Timestamp::from_second(ts).unwrap_or(jiff::Timestamp::UNIX_EPOCH).to_string()
}

fn partstat(response: &str) -> Partstat {
    match response {
        "Accept" | "Organizer" => Partstat::Accepted,
        "Decline" => Partstat::Declined,
        "Tentative" => Partstat::Tentative,
        _ => Partstat::NeedsAction,
    }
}

fn mailbox(el: &El) -> (String, String) {
    (el.str_at(&["Mailbox", "Name"]), el.str_at(&["Mailbox", "EmailAddress"]))
}

fn span_of(el: &El) -> Option<Span> {
    Some(Span::new(parse_time(el.text_at(&["Start"])?)?, parse_time(el.text_at(&["End"])?)?))
}

/// `<t:CalendarItem>` ответа `GetItem` → встреча.
pub(crate) fn item(el: &El) -> Option<CalendarItem> {
    let id = ItemId::from_el(el.child("ItemId")?)?;
    let kind = ItemKind::parse(el.text_at(&["CalendarItemType"]).unwrap_or("Single"));
    let my = el.text_at(&["MyResponseType"]).unwrap_or("");
    let mut participants: Vec<Participant> = Vec::new();
    let organizer = el.child("Organizer").map(mailbox);
    if let Some((name, email)) = &organizer {
        participants.push(Participant {
            name: name.clone(),
            email: email.clone(),
            owner: true,
            attendee: true,
            optional: false,
            status: Partstat::Accepted,
        });
    }
    for (list, optional) in [("RequiredAttendees", false), ("OptionalAttendees", true)] {
        for a in el.child(list).into_iter().flat_map(|l| l.all("Attendee")) {
            let (name, email) = mailbox(a);
            // Организатор бывает и в списке участников — один раз.
            if organizer.as_ref().is_some_and(|(_, o)| !o.is_empty() && o.eq_ignore_ascii_case(&email)) {
                continue;
            }
            participants.push(Participant {
                name,
                email,
                owner: false,
                attendee: true,
                optional,
                status: partstat(a.text_at(&["ResponseType"]).unwrap_or("")),
            });
        }
    }
    let mut keywords: Vec<String> = el.child("Categories").into_iter().flat_map(|c| c.all("String")).map(|s| s.text.trim().to_owned()).collect();
    keywords.sort();
    keywords.dedup();
    let event = Event {
        id: id.id.clone(),
        uid: el.str_at(&["UID"]),
        calendar_ids: el.child("ParentFolderId").and_then(|p| p.attr("Id")).map(str::to_owned).into_iter().collect(),
        title: el.str_at(&["Subject"]),
        description: el.child("Body").map(|b| b.text.replace("\r\n", "\n").trim_end().to_owned()).unwrap_or_default(),
        location: el.str_at(&["Location"]),
        span: span_of(el)?,
        all_day: el.bool_at(&["IsAllDayEvent"]),
        status: if el.bool_at(&["IsCancelled"]) { EventStatus::Cancelled } else { EventStatus::Confirmed },
        free_busy: if el.text_at(&["LegacyFreeBusyStatus"]) == Some("Free") { FreeBusy::Free } else { FreeBusy::Busy },
        participants,
        recurring: kind != ItemKind::Single,
        keywords,
        sequence: el.text_at(&["AppointmentSequenceNumber"]).and_then(|s| s.parse().ok()).unwrap_or(0),
        owner: None,
    };
    let modified = el
        .child("ModifiedOccurrences")
        .into_iter()
        .flat_map(|m| m.all("Occurrence"))
        .filter_map(|o| {
            Some(Occurrence {
                id: ItemId::from_el(o.child("ItemId")?)?,
                span: span_of(o)?,
                original_start: parse_time(o.text_at(&["OriginalStart"])?)?,
            })
        })
        .collect();
    let deleted = el
        .child("DeletedOccurrences")
        .into_iter()
        .flat_map(|d| d.all("DeletedOccurrence"))
        .filter_map(|d| parse_time(d.text_at(&["Start"])?))
        .collect();
    Some(CalendarItem {
        id,
        kind,
        event,
        my_response: partstat(my),
        is_organizer: my == "Organizer",
        is_meeting: el.bool_at(&["IsMeeting"]),
        recurrence: el.child("Recurrence").and_then(Recurrence::from_el),
        modified,
        deleted,
        original_start: el.text_at(&["OriginalStart"]).and_then(parse_time),
    })
}

/// Участник новой или изменённой встречи.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attendee {
    pub name: String,
    pub email: String,
}

/// Новая встреча. Организатор — владелец ящика; приглашения рассылает сервер ([`Notify`]).
#[derive(Debug, Clone, PartialEq)]
pub struct NewItem {
    pub title: String,
    pub description: String,
    pub location: String,
    pub span: Span,
    /// Весь день: `span` — от полуночи до полуночи в поясе встречи.
    pub all_day: bool,
    /// Пояс IANA (`Europe/Moscow`); нет Windows id для него — времена в UTC без пояса.
    pub time_zone: Option<String>,
    pub free_busy: FreeBusy,
    pub required: Vec<Attendee>,
    pub optional: Vec<Attendee>,
    /// Серия: `range.start` — дата первого экземпляра (= начало `span` в поясе встречи).
    pub recurrence: Option<Recurrence>,
}

impl NewItem {
    /// Из новой встречи модели Postman (как её собирает вкладка встречи для JMAP): местное начало в поясе
    /// `time_zone` и длительность → интервал UTC. Организатор не нужен — им будет владелец ящика.
    pub fn from_event(e: &NewEvent<'_>) -> std::result::Result<NewItem, String> {
        let tz = jiff::tz::TimeZone::get(e.time_zone).map_err(|err| format!("часовой пояс {}: {err}", e.time_zone))?;
        let start = e.start.to_zoned(tz).map_err(|err| err.to_string())?.timestamp().as_second();
        let (mut required, mut optional) = (Vec::new(), Vec::new());
        for (name, email, opt) in e.attendees {
            let a = Attendee { name: (*name).to_owned(), email: (*email).to_owned() };
            if *opt { optional.push(a) } else { required.push(a) }
        }
        Ok(NewItem {
            title: e.title.to_owned(),
            description: e.description.to_owned(),
            location: e.location.to_owned(),
            span: Span::new(start, start + e.duration_secs),
            all_day: false,
            time_zone: Some(e.time_zone.to_owned()),
            free_busy: FreeBusy::Busy,
            required,
            optional,
            recurrence: None,
        })
    }

    /// `<t:CalendarItem>` для `CreateItem` (порядок элементов — как в схеме `CalendarItemType`).
    fn to_xml(&self) -> String {
        let mut x = String::from("<t:CalendarItem>");
        x.push_str(&text_el("t:Subject", &self.title));
        x.push_str(&format!(r#"<t:Body BodyType="Text">{}</t:Body>"#, esc(&self.description)));
        x.push_str(&times(self.span, self.all_day));
        x.push_str(&text_el("t:LegacyFreeBusyStatus", free_busy(self.free_busy)));
        if !self.location.is_empty() {
            x.push_str(&text_el("t:Location", &self.location));
        }
        if !self.required.is_empty() {
            x.push_str(&attendees("t:RequiredAttendees", &self.required));
        }
        if !self.optional.is_empty() {
            x.push_str(&attendees("t:OptionalAttendees", &self.optional));
        }
        if let Some(r) = &self.recurrence {
            x.push_str(&r.to_xml());
        }
        x.push_str(&zones(self.time_zone.as_deref()));
        x.push_str("</t:CalendarItem>");
        x
    }
}

fn times(span: Span, all_day: bool) -> String {
    format!("<t:Start>{}</t:Start><t:End>{}</t:End><t:IsAllDayEvent>{all_day}</t:IsAllDayEvent>", format_time(span.start), format_time(span.end))
}

fn free_busy(f: FreeBusy) -> &'static str {
    match f {
        FreeBusy::Busy => "Busy",
        FreeBusy::Free => "Free",
    }
}

fn attendees(list: &str, a: &[Attendee]) -> String {
    let items: String = a
        .iter()
        .map(|a| {
            let name = if a.name.is_empty() { String::new() } else { text_el("t:Name", &a.name) };
            format!("<t:Attendee><t:Mailbox>{name}{}</t:Mailbox></t:Attendee>", text_el("t:EmailAddress", &a.email))
        })
        .collect();
    format!("<{list}>{items}</{list}>")
}

fn zones(iana: Option<&str>) -> String {
    match iana.and_then(crate::tz::windows_zone) {
        Some(w) => format!(r#"<t:StartTimeZone Id="{w}"/><t:EndTimeZone Id="{w}"/>"#),
        None => String::new(),
    }
}

/// Правка встречи: `Some` — поле меняется, `None` — как было.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Changes {
    pub title: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    /// Новое время и «весь день».
    pub time: Option<(Span, bool)>,
    /// Пояс IANA к новому времени.
    pub time_zone: Option<String>,
    pub free_busy: Option<FreeBusy>,
    pub required: Option<Vec<Attendee>>,
    pub optional: Option<Vec<Attendee>>,
    /// `Some(None)` — больше не серия.
    pub recurrence: Option<Option<Recurrence>>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        *self == Changes::default()
    }

    /// `<t:Updates>`: по `SetItemField` на поле; пустое — `DeleteItemField`.
    fn to_xml(&self) -> String {
        let set = |uri: &str, value: String| {
            format!(r#"<t:SetItemField><t:FieldURI FieldURI="{uri}"/><t:CalendarItem>{value}</t:CalendarItem></t:SetItemField>"#)
        };
        let delete = |uri: &str| format!(r#"<t:DeleteItemField><t:FieldURI FieldURI="{uri}"/></t:DeleteItemField>"#);
        let mut x = String::from("<t:Updates>");
        if let Some(t) = &self.title {
            x.push_str(&set("item:Subject", text_el("t:Subject", t)));
        }
        if let Some(d) = &self.description {
            x.push_str(&set("item:Body", format!(r#"<t:Body BodyType="Text">{}</t:Body>"#, esc(d))));
        }
        if let Some((span, all_day)) = self.time {
            x.push_str(&set("calendar:Start", format!("<t:Start>{}</t:Start>", format_time(span.start))));
            x.push_str(&set("calendar:End", format!("<t:End>{}</t:End>", format_time(span.end))));
            x.push_str(&set("calendar:IsAllDayEvent", format!("<t:IsAllDayEvent>{all_day}</t:IsAllDayEvent>")));
        }
        if let Some(w) = self.time_zone.as_deref().and_then(crate::tz::windows_zone) {
            x.push_str(&set("calendar:StartTimeZone", format!(r#"<t:StartTimeZone Id="{w}"/>"#)));
            x.push_str(&set("calendar:EndTimeZone", format!(r#"<t:EndTimeZone Id="{w}"/>"#)));
        }
        if let Some(f) = self.free_busy {
            x.push_str(&set("calendar:LegacyFreeBusyStatus", text_el("t:LegacyFreeBusyStatus", free_busy(f))));
        }
        if let Some(l) = &self.location {
            x.push_str(&if l.is_empty() { delete("calendar:Location") } else { set("calendar:Location", text_el("t:Location", l)) });
        }
        for (list, uri, tag) in [
            (&self.required, "calendar:RequiredAttendees", "t:RequiredAttendees"),
            (&self.optional, "calendar:OptionalAttendees", "t:OptionalAttendees"),
        ] {
            if let Some(a) = list {
                x.push_str(&if a.is_empty() { delete(uri) } else { set(uri, attendees(tag, a)) });
            }
        }
        match &self.recurrence {
            Some(Some(r)) => x.push_str(&set("calendar:Recurrence", r.to_xml())),
            Some(None) => x.push_str(&delete("calendar:Recurrence")),
            None => {}
        }
        x.push_str("</t:Updates>");
        x
    }
}

/// Кому сервер рассылает приглашения, изменения и отмены. `…AndSaveCopy` — копия в «Отправленные».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notify {
    /// Никому (своя встреча без участников, уборка).
    None,
    All,
    /// При правке — только тем, кого она касается (добавленным, удалённым); иначе — как `All`.
    Changed,
    AllAndSaveCopy,
    ChangedAndSaveCopy,
}

impl Notify {
    /// `SendMeetingInvitations` (`CreateItem`) и `SendMeetingCancellations` (`DeleteItem`).
    fn invitations(self) -> &'static str {
        match self {
            Notify::None => "SendToNone",
            Notify::All | Notify::Changed => "SendOnlyToAll",
            Notify::AllAndSaveCopy | Notify::ChangedAndSaveCopy => "SendToAllAndSaveCopy",
        }
    }

    /// `SendMeetingInvitationsOrCancellations` (`UpdateItem`).
    fn update(self) -> &'static str {
        match self {
            Notify::None => "SendToNone",
            Notify::All => "SendOnlyToAll",
            Notify::Changed => "SendOnlyToChanged",
            Notify::AllAndSaveCopy => "SendToAllAndSaveCopy",
            Notify::ChangedAndSaveCopy => "SendToChangedAndSaveCopy",
        }
    }
}

/// Как удалять.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteMode {
    /// В «Удалённые» — как удаление в Outlook.
    MoveToDeletedItems,
    /// В «Восстанавливаемые» (из «Удалённых» не видно, восстановить можно).
    SoftDelete,
    /// Насовсем (уборка тестов).
    HardDelete,
}

impl DeleteMode {
    pub(crate) fn ews(self) -> &'static str {
        match self {
            DeleteMode::MoveToDeletedItems => "MoveToDeletedItems",
            DeleteMode::SoftDelete => "SoftDelete",
            DeleteMode::HardDelete => "HardDelete",
        }
    }
}

/// Ответ на приглашение.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    Accept,
    Tentative,
    Decline,
}

impl Reply {
    fn element(self) -> &'static str {
        match self {
            Reply::Accept => "t:AcceptItem",
            Reply::Tentative => "t:TentativelyAcceptItem",
            Reply::Decline => "t:DeclineItem",
        }
    }
}

/// Свойства встречи для `GetItem`: только нужное модели (без `MimeContent` — его пришлось бы разбирать в
/// воркере).
const PROPS: &[&str] = &[
    "item:ParentFolderId",
    "item:Subject",
    "item:Body",
    "item:Categories",
    "calendar:UID",
    "calendar:Start",
    "calendar:End",
    "calendar:OriginalStart",
    "calendar:IsAllDayEvent",
    "calendar:LegacyFreeBusyStatus",
    "calendar:Location",
    "calendar:IsMeeting",
    "calendar:IsCancelled",
    "calendar:CalendarItemType",
    "calendar:MyResponseType",
    "calendar:Organizer",
    "calendar:RequiredAttendees",
    "calendar:OptionalAttendees",
    "calendar:AppointmentSequenceNumber",
    "calendar:Recurrence",
    "calendar:ModifiedOccurrences",
    "calendar:DeletedOccurrences",
];

fn field_uris(props: &[&str]) -> String {
    props.iter().map(|p| format!(r#"<t:FieldURI FieldURI="{p}"/>"#)).collect()
}

/// Первый объект в `<m:Items>` сообщения ответа.
fn first_item_id(msg: &El) -> Option<ItemId> {
    msg.child("Items")?.children.first()?.child("ItemId").and_then(ItemId::from_el)
}

fn folder(el: &El) -> Option<Folder> {
    let id = el.child("FolderId")?;
    Some(Folder {
        id: FolderId { id: id.attr("Id")?.to_owned(), change_key: id.attr("ChangeKey").map(str::to_owned) },
        name: el.str_at(&["DisplayName"]),
        total: el.text_at(&["TotalCount"]).and_then(|t| t.parse().ok()).unwrap_or(0),
    })
}

pub(crate) mod parse {
    //! Разбор ответов — отдельно от запросов, для тестов на корпусе.
    use super::*;

    pub(crate) fn get_folder(resp: &El) -> Result<Folder> {
        let msg = soap::single(resp)?;
        msg.child("Folders").and_then(|f| f.children.first()).and_then(folder).ok_or_else(|| Error::Xml("GetFolder: no folder".into()))
    }

    pub(crate) fn find_folder(resp: &El) -> Result<Vec<Folder>> {
        let msg = soap::single(resp)?;
        Ok(msg
            .at(&["RootFolder", "Folders"])
            .into_iter()
            .flat_map(|f| f.children.iter())
            .filter(|f| f.name == "CalendarFolder" || f.text_at(&["FolderClass"]).is_some_and(|c| c.starts_with("IPF.Appointment")))
            .filter_map(folder)
            .collect())
    }

    pub(crate) fn sync(resp: &El) -> Result<SyncChanges> {
        let msg = soap::single(resp)?;
        let mut out =
            SyncChanges { state: SyncState(msg.str_at(&["SyncState"])), done: msg.bool_at(&["IncludesLastItemInRange"]), ..SyncChanges::default() };
        for c in msg.child("Changes").into_iter().flat_map(|c| c.children.iter()) {
            match c.name.as_str() {
                "Create" | "Update" => {
                    let Some(id) = c.children.first().and_then(|i| i.child("ItemId")).and_then(ItemId::from_el) else { continue };
                    if c.name == "Create" { out.created.push(id) } else { out.updated.push(id) }
                }
                "Delete" => out.deleted.extend(c.child("ItemId").and_then(|i| i.attr("Id")).map(str::to_owned)),
                _ => {} // ReadFlagChange — не о календаре
            }
        }
        Ok(out)
    }

    pub(crate) fn calendar_view(resp: &El) -> Result<(Vec<ItemRef>, bool)> {
        let msg = soap::single(resp)?;
        let root = msg.child("RootFolder").ok_or_else(|| Error::Xml("FindItem: no RootFolder".into()))?;
        let items = root
            .child("Items")
            .into_iter()
            .flat_map(|i| i.all("CalendarItem"))
            .filter_map(|c| {
                Some(ItemRef {
                    id: ItemId::from_el(c.child("ItemId")?)?,
                    span: span_of(c)?,
                    kind: ItemKind::parse(c.text_at(&["CalendarItemType"]).unwrap_or("Single")),
                })
            })
            .collect();
        Ok((items, root.attr("IncludesLastItemInRange") != Some("false")))
    }

    pub(crate) fn find_ids(resp: &El) -> Result<Vec<ItemId>> {
        let msg = soap::single(resp)?;
        Ok(msg
            .at(&["RootFolder", "Items"])
            .into_iter()
            .flat_map(|i| i.children.iter())
            .filter_map(|c| c.child("ItemId").and_then(ItemId::from_el))
            .collect())
    }

    /// Встречи `GetItem`; удалённые за это время (`ErrorItemNotFound`) — пропускаются.
    pub(crate) fn get_items(resp: &El) -> Result<Vec<CalendarItem>> {
        let mut out = Vec::new();
        for msg in soap::messages(resp) {
            if soap::code(msg) == "ErrorItemNotFound" {
                continue;
            }
            soap::check(msg)?;
            out.extend(msg.child("Items").into_iter().flat_map(|i| i.all("CalendarItem")).filter_map(item));
        }
        Ok(out)
    }

    pub(crate) fn created(resp: &El) -> Result<ItemId> {
        first_item_id(soap::single(resp)?).ok_or_else(|| Error::Xml("no ItemId in response".into()))
    }

    /// `DeleteItem`, ответы: уже удалённое — не ошибка.
    pub(crate) fn all_ok(resp: &El) -> Result<()> {
        for msg in soap::messages(resp) {
            if soap::code(msg) != "ErrorItemNotFound" {
                soap::check(msg)?;
            }
        }
        Ok(())
    }
}

impl Client {
    /// Основной календарь ящика.
    pub async fn calendar_folder(&mut self) -> Result<Folder> {
        let body = format!(
            r#"<m:GetFolder><m:FolderShape><t:BaseShape>Default</t:BaseShape></m:FolderShape><m:FolderIds>{}</m:FolderIds></m:GetFolder>"#,
            FolderRef::WellKnown(WellKnown::Calendar).xml()
        );
        parse::get_folder(&self.call("GetFolder", &body).await?)
    }

    /// Основной календарь и вложенные в него календари.
    pub async fn calendars(&mut self) -> Result<Vec<Folder>> {
        let mut out = vec![self.calendar_folder().await?];
        let body = format!(
            r#"<m:FindFolder Traversal="Deep"><m:FolderShape><t:BaseShape>Default</t:BaseShape><t:AdditionalProperties><t:FieldURI FieldURI="folder:FolderClass"/></t:AdditionalProperties></m:FolderShape><m:ParentFolderIds>{}</m:ParentFolderIds></m:FindFolder>"#,
            FolderRef::WellKnown(WellKnown::Calendar).xml()
        );
        out.extend(parse::find_folder(&self.call("FindFolder", &body).await?)?);
        Ok(out)
    }

    /// Изменения папки с `state` (`None` — с начала: все объекты как созданные). Не больше `max` (1…512) за раз;
    /// `done == false` — повторить с новым состоянием. Устаревшее состояние — [`Error::is_invalid_sync_state`].
    pub async fn sync(&mut self, folder: &FolderRef, state: Option<&SyncState>, max: u32) -> Result<SyncChanges> {
        let state = state.filter(|s| !s.0.is_empty()).map(|s| text_el("m:SyncState", &s.0)).unwrap_or_default();
        let body = format!(
            r#"<m:SyncFolderItems><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape></m:ItemShape><m:SyncFolderId>{}</m:SyncFolderId>{state}<m:MaxChangesReturned>{}</m:MaxChangesReturned><m:SyncScope>NormalItems</m:SyncScope></m:SyncFolderItems>"#,
            folder.xml(),
            max.clamp(1, 512)
        );
        let changes = parse::sync(&self.call("SyncFolderItems", &body).await?)?;
        tracing::debug!(
            created = changes.created.len(),
            updated = changes.updated.len(),
            deleted = changes.deleted.len(),
            done = changes.done,
            "ews sync"
        );
        Ok(changes)
    }

    /// Окно календаря с развёрнутыми сервером повторами (`CalendarView`): экземпляры, исключения, одиночные.
    /// `bool` — все ли попали (не больше `max`).
    pub async fn calendar_view(&mut self, folder: &FolderRef, window: Span, max: u32) -> Result<(Vec<ItemRef>, bool)> {
        let body = format!(
            r#"<m:FindItem Traversal="Shallow"><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape><t:AdditionalProperties>{}</t:AdditionalProperties></m:ItemShape><m:CalendarView MaxEntriesReturned="{}" StartDate="{}" EndDate="{}"/><m:ParentFolderIds>{}</m:ParentFolderIds></m:FindItem>"#,
            field_uris(&["calendar:Start", "calendar:End", "calendar:CalendarItemType"]),
            max.max(1),
            format_time(window.start),
            format_time(window.end),
            folder.xml()
        );
        parse::calendar_view(&self.call("FindItem", &body).await?)
    }

    /// Встречи целиком, пачками по [`BATCH`]. Удалённые за это время — пропускаются.
    pub async fn get_items(&mut self, ids: &[ItemId]) -> Result<Vec<CalendarItem>> {
        let mut out = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(BATCH) {
            let ids: String = chunk.iter().map(|i| i.xml("t:ItemId")).collect();
            let body = format!(
                r#"<m:GetItem><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape><t:BodyType>Text</t:BodyType><t:AdditionalProperties>{}</t:AdditionalProperties></m:ItemShape><m:ItemIds>{ids}</m:ItemIds></m:GetItem>"#,
                field_uris(PROPS)
            );
            out.extend(parse::get_items(&self.call("GetItem", &body).await?)?);
        }
        Ok(out)
    }

    /// Серия экземпляра или исключения (`RecurringMasterItemId`): ответ на приглашение серии и правка серии — по ней.
    /// Не серия или удалена — `None`.
    pub async fn master_of(&mut self, occurrence: &ItemId) -> Result<Option<CalendarItem>> {
        let id = match occurrence.change_key.as_deref() {
            Some(ck) => format!(r#"<t:RecurringMasterItemId OccurrenceId="{}" ChangeKey="{}"/>"#, esc(&occurrence.id), esc(ck)),
            None => format!(r#"<t:RecurringMasterItemId OccurrenceId="{}"/>"#, esc(&occurrence.id)),
        };
        let body = format!(
            r#"<m:GetItem><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape><t:BodyType>Text</t:BodyType><t:AdditionalProperties>{}</t:AdditionalProperties></m:ItemShape><m:ItemIds>{id}</m:ItemIds></m:GetItem>"#,
            field_uris(PROPS)
        );
        match parse::get_items(&self.call("GetItem", &body).await?) {
            Ok(items) => Ok(items.into_iter().find(|i| i.kind == ItemKind::RecurringMaster)),
            // Не экземпляр серии — у объекта нет серии.
            Err(e) if matches!(e.code(), Some("ErrorItemCorrupt" | "ErrorInvalidOperation" | "ErrorCalendarOccurrenceIsDeletedFromRecurrence")) => {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// Создать встречу (в `folder` или основном календаре); участникам — приглашения по `notify`.
    pub async fn create(&mut self, folder: Option<&FolderRef>, item: &NewItem, notify: Notify) -> Result<ItemId> {
        let folder = folder.map(|f| format!("<m:SavedItemFolderId>{}</m:SavedItemFolderId>", f.xml())).unwrap_or_default();
        let body =
            format!(r#"<m:CreateItem SendMeetingInvitations="{}">{folder}<m:Items>{}</m:Items></m:CreateItem>"#, notify.invitations(), item.to_xml());
        parse::created(&self.call("CreateItem", &body).await?)
    }

    /// Изменить встречу; участникам — изменения по `notify`. Ответ — новая версия (`ChangeKey`).
    pub async fn update(&mut self, id: &ItemId, changes: &Changes, notify: Notify) -> Result<ItemId> {
        if changes.is_empty() {
            return Ok(id.clone());
        }
        let body = format!(
            r#"<m:UpdateItem ConflictResolution="AutoResolve" SendMeetingInvitationsOrCancellations="{}"><m:ItemChanges><t:ItemChange>{}{}</t:ItemChange></m:ItemChanges></m:UpdateItem>"#,
            notify.update(),
            id.xml("t:ItemId"),
            changes.to_xml()
        );
        let resp = self.call("UpdateItem", &body).await?;
        let msg = soap::single(&resp)?;
        Ok(first_item_id(msg).unwrap_or_else(|| id.clone()))
    }

    /// Удалить объекты; у встреч, где я организатор, участникам — отмены по `notify`. Уже удалённое — не ошибка.
    pub async fn delete(&mut self, ids: &[ItemId], mode: DeleteMode, notify: Notify) -> Result<()> {
        for chunk in ids.chunks(BATCH) {
            let ids: String = chunk.iter().map(|i| i.xml("t:ItemId")).collect();
            let body = format!(
                r#"<m:DeleteItem DeleteType="{}" SendMeetingCancellations="{}"><m:ItemIds>{ids}</m:ItemIds></m:DeleteItem>"#,
                mode.ews(),
                notify.invitations()
            );
            parse::all_ok(&self.call("DeleteItem", &body).await?)?;
        }
        Ok(())
    }

    /// Ответить на приглашение (встреча в календаре или само приглашение): ответ уходит организатору;
    /// `save_copy` — копия в «Отправленные». «Отклонить» убирает встречу из календаря (так делает сервер).
    pub async fn respond(&mut self, id: &ItemId, reply: Reply, text: Option<&str>, save_copy: bool) -> Result<()> {
        let body_el = text.map(|t| format!(r#"<t:Body BodyType="Text">{}</t:Body>"#, esc(t))).unwrap_or_default();
        let el = reply.element();
        let body = format!(
            r#"<m:CreateItem MessageDisposition="{}"><m:Items><{el}>{body_el}{}</{el}></m:Items></m:CreateItem>"#,
            if save_copy { "SendAndSaveCopy" } else { "SendOnly" },
            id.xml("t:ReferenceItemId")
        );
        parse::all_ok(&self.call("CreateItem", &body).await?)
    }

    /// Свои объекты по строке в теме (метка `[postman-test …]`, тема приглашения) в стандартной папке — не больше
    /// `max`. Только id: содержимое писем здесь не читается.
    pub async fn find_by_subject(&mut self, folder: WellKnown, text: &str, max: u32) -> Result<Vec<ItemId>> {
        let body = format!(
            r#"<m:FindItem Traversal="Shallow"><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape></m:ItemShape><m:IndexedPageItemView MaxEntriesReturned="{}" Offset="0" BasePoint="Beginning"/><m:Restriction><t:Contains ContainmentMode="Substring" ContainmentComparison="IgnoreCase"><t:FieldURI FieldURI="item:Subject"/><t:Constant Value="{}"/></t:Contains></m:Restriction><m:ParentFolderIds>{}</m:ParentFolderIds></m:FindItem>"#,
            max.max(1),
            esc(text),
            FolderRef::WellKnown(folder).xml()
        );
        parse::find_ids(&self.call("FindItem", &body).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml::parse as xml;

    fn resp(fixture: &[u8]) -> El {
        soap::response(xml(fixture).unwrap()).unwrap()
    }

    fn ts(s: &str) -> i64 {
        parse_time(s).unwrap()
    }

    #[test]
    fn times() {
        assert_eq!(parse_time("2026-10-20T07:00:00Z"), Some(1_792_479_600));
        assert_eq!(parse_time("2026-10-20T10:00:00+03:00"), Some(1_792_479_600));
        assert_eq!(parse_time("2026-10-20T07:00:00"), Some(1_792_479_600));
        assert_eq!(format_time(1_792_479_600), "2026-10-20T07:00:00Z");
        assert_eq!(parse_time("вчера"), None);
    }

    #[test]
    fn folders() {
        let f = parse::get_folder(&resp(include_bytes!("../tests/fixtures/get_folder_calendar.xml"))).unwrap();
        assert_eq!((f.name.as_str(), f.total), ("Calendar", 12));
        assert!(f.id.id.starts_with("AAMkAD") && f.id.change_key.is_some());
        let sub = parse::find_folder(&resp(include_bytes!("../tests/fixtures/find_folder_calendars.xml"))).unwrap();
        assert_eq!(sub.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["Проекты", "Отпуска"]);
    }

    #[test]
    fn sync_changes() {
        let c = parse::sync(&resp(include_bytes!("../tests/fixtures/sync_folder_items.xml"))).unwrap();
        assert_eq!(c.state.as_str(), "H4sIAAAAAAAEAGNgYGcAAkY2BgYGJgYAAA==");
        assert!(!c.done);
        assert_eq!((c.created.len(), c.updated.len(), c.deleted.len()), (2, 1, 1));
        assert_eq!(c.created[0].change_key.as_deref(), Some("DwAAABYAAAA1"));
        assert_eq!(c.deleted[0], "AAMkADdel1");
    }

    #[test]
    fn invalid_sync_state() {
        let doc = xml(include_bytes!("../tests/fixtures/sync_invalid_state.xml")).unwrap();
        let err = soap::response(doc).and_then(|r| parse::sync(&r)).unwrap_err();
        assert!(err.is_invalid_sync_state(), "{err}");
    }

    #[test]
    fn meeting_with_attendees() {
        let items = parse::get_items(&resp(include_bytes!("../tests/fixtures/get_item_meeting.xml"))).unwrap();
        assert_eq!(items.len(), 1, "ErrorItemNotFound пропущен");
        let it = &items[0];
        assert_eq!(it.kind, ItemKind::Single);
        assert!(it.is_meeting && !it.is_organizer);
        assert_eq!(it.my_response, Partstat::Tentative);
        let e = &it.event;
        assert_eq!(e.title, "Планёрка & отчёт");
        assert_eq!(e.description, "Повестка:\n1. Итоги недели");
        assert_eq!(e.location, "Переговорная 3");
        assert_eq!(e.uid, "040000008200E00074C5B7101A82E00800000000A1B2C3D4");
        assert_eq!(e.calendar_ids, ["AAMkADcal"]);
        assert_eq!(e.span, Span::new(ts("2026-10-20T07:00:00Z"), ts("2026-10-20T08:00:00Z")));
        assert_eq!(e.sequence, 2);
        assert_eq!(e.keywords, ["Проект Альфа"]);
        assert_eq!(e.free_busy, FreeBusy::Busy);
        assert!(!e.recurring);
        let org = e.organizer().unwrap();
        assert_eq!((org.name.as_str(), org.email.as_str()), ("Анна Петрова", "anna@example.test"));
        let people: Vec<_> = e.participants.iter().filter(|p| !p.owner).map(|p| (p.email.as_str(), p.optional, p.status)).collect();
        assert_eq!(
            people,
            [
                ("user1@example.test", false, Partstat::Tentative),
                ("boris@example.test", false, Partstat::Accepted),
                ("vera@example.test", true, Partstat::NeedsAction)
            ]
        );
    }

    #[test]
    fn recurring_master() {
        let items = parse::get_items(&resp(include_bytes!("../tests/fixtures/get_item_master.xml"))).unwrap();
        let m = &items[0];
        assert_eq!(m.kind, ItemKind::RecurringMaster);
        assert!(m.is_organizer && m.event.recurring);
        let r = m.recurrence.as_ref().unwrap();
        assert_eq!(
            r.pattern,
            Pattern::Weekly { interval: 1, days: vec![DayOfWeek::Monday, DayOfWeek::Wednesday], first_day: Some(DayOfWeek::Monday) }
        );
        assert_eq!(r.range, Range::EndDate { start: "2026-10-05".parse().unwrap(), end: "2026-12-30".parse().unwrap() });
        assert_eq!(r.rrule(), "FREQ=WEEKLY;INTERVAL=1;BYDAY=MO,WE;WKST=MO;UNTIL=20261230");
        assert_eq!(m.modified.len(), 1);
        assert_eq!(m.modified[0].original_start, ts("2026-10-14T06:00:00Z"));
        assert_eq!(m.modified[0].span.start, ts("2026-10-14T09:00:00Z"));
        assert_eq!(m.deleted, [ts("2026-10-19T06:00:00Z")]);
    }

    #[test]
    fn calendar_view_occurrences() {
        let (items, all) = parse::calendar_view(&resp(include_bytes!("../tests/fixtures/find_item_calendar_view.xml"))).unwrap();
        assert!(all);
        assert_eq!(items.iter().map(|i| i.kind).collect::<Vec<_>>(), [ItemKind::Occurrence, ItemKind::Exception, ItemKind::Single]);
    }

    #[test]
    fn create_update_delete_respond() {
        let id = parse::created(&resp(include_bytes!("../tests/fixtures/create_item.xml"))).unwrap();
        assert_eq!(id.id, "AAMkADnew");
        parse::all_ok(&resp(include_bytes!("../tests/fixtures/delete_item.xml"))).unwrap();
        parse::all_ok(&resp(include_bytes!("../tests/fixtures/accept_item.xml"))).unwrap();
        let upd = resp(include_bytes!("../tests/fixtures/update_item.xml"));
        assert_eq!(first_item_id(soap::single(&upd).unwrap()).unwrap().change_key.as_deref(), Some("DwAAABYAAAA9"));
    }

    #[test]
    fn rrules() {
        let start: Date = "2026-11-02".parse().unwrap();
        let r = |pattern, range| Recurrence { pattern, range }.rrule();
        assert_eq!(r(Pattern::Daily { interval: 2 }, Range::Numbered { start, count: 5 }), "FREQ=DAILY;INTERVAL=2;COUNT=5");
        assert_eq!(
            r(Pattern::RelativeMonthly { interval: 1, days: DayOfWeek::Weekday, index: WeekIndex::Last }, Range::NoEnd { start }),
            "FREQ=MONTHLY;INTERVAL=1;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1"
        );
        assert_eq!(r(Pattern::AbsoluteYearly { month: 3, day: 8 }, Range::NoEnd { start }), "FREQ=YEARLY;BYMONTH=3;BYMONTHDAY=8");
    }

    #[test]
    fn new_item_request() {
        let item = NewItem {
            title: "Обзор <Q3>".into(),
            description: "Строка 1\nСтрока 2".into(),
            location: "Зал".into(),
            span: Span::new(ts("2026-11-02T07:00:00Z"), ts("2026-11-02T08:00:00Z")),
            all_day: false,
            time_zone: Some("Europe/Moscow".into()),
            free_busy: FreeBusy::Busy,
            required: vec![Attendee { name: "Борис".into(), email: "boris@example.test".into() }],
            optional: vec![],
            recurrence: Some(Recurrence {
                pattern: Pattern::Weekly { interval: 1, days: vec![DayOfWeek::Monday], first_day: None },
                range: Range::Numbered { start: "2026-11-02".parse().unwrap(), count: 3 },
            }),
        };
        let x = xml(soap::envelope(&item.to_xml()).as_bytes()).unwrap();
        let c = x.at(&["Body", "CalendarItem"]).unwrap();
        let order: Vec<&str> = c.children.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            order,
            [
                "Subject",
                "Body",
                "Start",
                "End",
                "IsAllDayEvent",
                "LegacyFreeBusyStatus",
                "Location",
                "RequiredAttendees",
                "Recurrence",
                "StartTimeZone",
                "EndTimeZone"
            ]
        );
        assert_eq!(c.text_at(&["Subject"]), Some("Обзор <Q3>"));
        assert_eq!(c.child("StartTimeZone").and_then(|z| z.attr("Id")), Some("Russian Standard Time"));
        assert_eq!(c.text_at(&["RequiredAttendees", "Attendee", "Mailbox", "EmailAddress"]), Some("boris@example.test"));
        let back = Recurrence::from_el(c.child("Recurrence").unwrap()).unwrap();
        assert_eq!(Some(back), item.recurrence);
    }

    #[test]
    fn new_item_from_event() {
        let ev = NewEvent {
            uid: "u",
            calendar_id: "c",
            title: "Созвон",
            description: "",
            location: "",
            start: "2026-11-02T10:00:00".parse().unwrap(),
            time_zone: "Europe/Moscow",
            duration_secs: 1800,
            organizer: ("Я", "user1@example.test"),
            attendees: &[("Борис", "boris@example.test", false), ("Вера", "vera@example.test", true)],
        };
        let it = NewItem::from_event(&ev).unwrap();
        assert_eq!(it.span, Span::new(ts("2026-11-02T07:00:00Z"), ts("2026-11-02T07:30:00Z")));
        assert_eq!((it.required.len(), it.optional.len()), (1, 1));
    }

    #[test]
    fn changes_request() {
        assert!(Changes::default().is_empty());
        let ch = Changes {
            title: Some("Новое".into()),
            location: Some(String::new()),
            time: Some((Span::new(0, 3600), false)),
            time_zone: Some("Europe/Moscow".into()),
            optional: Some(vec![]),
            recurrence: Some(None),
            ..Changes::default()
        };
        let x = xml(format!("<x>{}</x>", ch.to_xml()).as_bytes()).unwrap();
        let ops: Vec<String> = x
            .child("Updates")
            .unwrap()
            .children
            .iter()
            .map(|op| format!("{} {}", op.name, op.child("FieldURI").and_then(|f| f.attr("FieldURI")).unwrap_or("")))
            .collect();
        assert_eq!(
            ops,
            [
                "SetItemField item:Subject",
                "SetItemField calendar:Start",
                "SetItemField calendar:End",
                "SetItemField calendar:IsAllDayEvent",
                "SetItemField calendar:StartTimeZone",
                "SetItemField calendar:EndTimeZone",
                "DeleteItemField calendar:Location",
                "DeleteItemField calendar:OptionalAttendees",
                "DeleteItemField calendar:Recurrence",
            ]
        );
    }

    #[test]
    fn notify_attributes() {
        assert_eq!(Notify::Changed.invitations(), "SendOnlyToAll");
        assert_eq!(Notify::Changed.update(), "SendOnlyToChanged");
        assert_eq!(Notify::AllAndSaveCopy.update(), "SendToAllAndSaveCopy");
        assert_eq!(Reply::Tentative.element(), "t:TentativelyAcceptItem");
    }
}
