//! Плагин EWS (`postman_backend_ews`) против подменного сервера EWS: HTTP без TLS (сборка `postman-ews` с `testkit`),
//! ответы — корпус `crates/ews/tests/fixtures` (по примерам Microsoft, обезличен). Проверяет синхронизацию почты тем
//! же договором команд и событий без боевого Exchange: вход, роли папок, запасное дерево `FindFolder` (сервер падает на
//! `SyncFolderHierarchy`, как боевой 2013), первые письма папки, «прочитано», отправка из MIME, справочник, области;
//! календарь (S7-07, S7-08): ответ на приглашение по UID или письмом, новая встреча и UID от сервера, порядок
//! занятости и «нет данных», неделя без повторных `GetItem`, запасной `ResolveNames`.
//! Боевой Exchange — `stand_exchange.rs` (`#[ignore]`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // тест: «не падать» — для кода приложения (S2-03)

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use postman_cache::Cache;
use postman_core::backend::{MailPlugin, Spawn};
use postman_core::model::{Auth, Feature};
use postman_core::sync::{Account, Command, Event, Status, UnboundedSender};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../ews/tests/fixtures/");

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{FIXTURES}{name}")).unwrap()
}

/// `SyncFolderHierarchy` боевого Exchange 2013: внутренняя ошибка сервера.
const HIERARCHY_FAILS: &str = r#"<?xml version="1.0" encoding="utf-8"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><m:SyncFolderHierarchyResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"><m:ResponseMessages><m:SyncFolderHierarchyResponseMessage ResponseClass="Error"><m:MessageText>internal</m:MessageText><m:ResponseCode>ErrorInternalServerError</m:ResponseCode></m:SyncFolderHierarchyResponseMessage></m:ResponseMessages></m:SyncFolderHierarchyResponse></s:Body></s:Envelope>"#;

/// `FindItem` пустой папки. Готовым ответом, а не правкой текста корпуса: на CI файлы корпуса — с CRLF
/// (`core.autocrlf`), и замена многострочного куска молча не срабатывала.
const EMPTY_PAGE: &str = r#"<?xml version="1.0" encoding="utf-8"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><m:FindItemResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types"><m:ResponseMessages><m:FindItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:RootFolder IndexedPagingOffset="0" TotalItemsInView="0" IncludesLastItemInRange="true"><t:Items/></m:RootFolder></m:FindItemResponseMessage></m:ResponseMessages></m:FindItemResponse></s:Body></s:Envelope>"#;

/// Ответ подменного сервера по телу запроса.
fn answer(body: &str) -> String {
    let op = |tag: &str| body.contains(&format!("<m:{tag}"));
    if op("GetFolder") && body.contains(r#"Id="msgfolderroot""#) {
        fixture("get_folder_msgroot.xml")
    } else if op("GetFolder") && body.contains(r#"Id="inbox""#) {
        fixture("get_folder_mail_roles.xml")
    } else if op("GetFolder") {
        fixture("get_folder_calendar.xml")
    } else if op("SyncFolderHierarchy") {
        HIERARCHY_FAILS.to_owned()
    } else if op("FindFolder") {
        fixture("find_folder_mail.xml")
    } else if op("SyncFolderItems") {
        fixture("sync_mail_items.xml")
    } else if op("FindItem") && body.contains("CalendarView") {
        fixture("find_item_calendar_view.xml")
    } else if op("FindItem") && body.contains("AAMkADinbox") {
        fixture("find_item_mail_page.xml")
    } else if op("FindItem") {
        // Другие папки — пустые: в корпусе одни и те же письма, и строки одной папки иначе перезапишут другую.
        EMPTY_PAGE.to_owned()
    } else if op("GetItem") && body.contains("IncludeMimeContent") {
        fixture("get_item_message_mime.xml")
    } else if op("GetItem") && body.contains("calendar:") {
        fixture("get_item_meeting.xml")
    } else if op("GetItem") {
        fixture("get_item_headers.xml")
    } else if op("DeleteItem") {
        fixture("delete_item.xml")
    } else if op("UpdateItem") {
        fixture("update_item_read.xml")
    } else if op("CreateItem") && ["<t:AcceptItem", "<t:TentativelyAcceptItem", "<t:DeclineItem"].iter().any(|r| body.contains(r)) {
        fixture("accept_item.xml")
    } else if op("CreateItem") && body.contains("SendMeetingInvitations") {
        fixture("create_item.xml")
    } else if op("CreateItem") {
        fixture("create_item_send.xml")
    } else if op("FindPeople") {
        fixture("find_people.xml")
    } else if op("ResolveNames") {
        fixture("resolve_names.xml")
    } else if op("GetUserAvailabilityRequest") {
        fixture("get_user_availability.xml")
    } else {
        fixture("fault_schema.xml")
    }
}

/// Свои ответы теста поверх корпуса: `None` — ответ по умолчанию ([`answer`]).
type Extra = fn(&str) -> Option<String>;

async fn serve(mut s: TcpStream, log: Arc<Mutex<Vec<String>>>, extra: Extra) {
    loop {
        let mut head = Vec::new();
        let mut b = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            if s.read(&mut b).await.unwrap_or(0) == 0 {
                return;
            }
            head.push(b[0]);
        }
        let head = String::from_utf8_lossy(&head).to_string();
        let len: usize =
            head.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap())).unwrap_or(0);
        let mut body = vec![0u8; len];
        s.read_exact(&mut body).await.unwrap();
        let body = String::from_utf8(body).unwrap();
        let reply = extra(&body).unwrap_or_else(|| answer(&body));
        log.lock().unwrap().push(body);
        let out = format!("HTTP/1.1 200 OK\r\nContent-Type: text/xml; charset=utf-8\r\nContent-Length: {}\r\n\r\n{reply}", reply.len());
        s.write_all(out.as_bytes()).await.unwrap();
    }
}

/// Сервер в своём потоке (синхронизация плагина — в своём).
fn server() -> (String, Arc<Mutex<Vec<String>>>) {
    server_with(|_| None)
}

fn server_with(extra: Extra) -> (String, Arc<Mutex<Vec<String>>>) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let (tx, rx) = std::sync::mpsc::channel();
    let l = log.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            while let Ok((s, _)) = listener.accept().await {
                tokio::spawn(serve(s, l.clone(), extra));
            }
        });
    });
    (format!("http://{}/EWS/Exchange.asmx", rx.recv().unwrap()), log)
}

struct Live {
    commands: UnboundedSender<Command>,
    events: Receiver<Event>,
    cache: Cache,
}

impl Live {
    fn wait(&self, what: &str, pick: impl Fn(&Event) -> bool) -> Event {
        let start = Instant::now();
        loop {
            let left = Duration::from_secs(20).checked_sub(start.elapsed()).unwrap_or_else(|| panic!("не дождались: {what}"));
            match self.events.recv_timeout(left) {
                Ok(Event::Error(e)) => panic!("синхронизация: {e}"),
                Ok(ev) if pick(&ev) => return ev,
                Ok(_) => {}
                Err(e) => panic!("не дождались: {what} ({e})"),
            }
        }
    }
}

fn start(url: &str) -> Live {
    let (tx, events) = std::sync::mpsc::channel();
    let cache = Cache::open_in_memory().unwrap();
    // Логин не адрес почты — без Autodiscover (только подменный сервер).
    let account = Account { url: url.to_owned(), auth: Auth::Basic { user: r"EXAMPLE\user1".into(), password: "secret".into() } };
    let commands = postman_backend_ews::EwsPlugin.spawn(Spawn {
        account,
        profile: None,
        cache: cache.clone(),
        index: None,
        events: tx,
        repaint: Box::new(|| {}),
        areas: Vec::new(),
    });
    Live { commands, events, cache }
}

#[test]
fn exchange_mail_through_the_plugin() {
    let (url, log) = server();
    let live = start(&url);
    let Event::Features(f) = live.wait("Features", |e| matches!(e, Event::Features(_))) else { unreachable!() };
    assert!(f.has(Feature::Calendar) && f.has(Feature::FreeBusy) && f.has(Feature::Directory));
    assert!(!f.has(Feature::Tasks) && !f.has(Feature::Contacts) && !f.has(Feature::Rules));
    live.wait("Synced", |e| matches!(e, Event::Status(Status::Synced)));

    // Дерево — запасным путём (FindFolder), роли — по стандартным папкам; календарь и контакты — не почта.
    let boxes = live.cache.mailboxes().unwrap();
    let names: Vec<(&str, Option<&str>)> = boxes.iter().map(|b| (b.name.as_str(), b.role.as_deref())).collect();
    assert!(names.contains(&("Inbox", Some("inbox"))) && names.contains(&("Sent Items", Some("sent"))), "{names:?}");
    assert!(!names.iter().any(|(n, _)| *n == "Calendar"));
    let project = boxes.iter().find(|b| b.name == "Проект").unwrap();
    let inbox = boxes.iter().find(|b| b.role.as_deref() == Some("inbox")).unwrap();
    assert_eq!(project.parent_id.as_deref(), Some(inbox.id.as_str()));
    assert!(log.lock().unwrap().iter().any(|b| b.contains("<m:SyncFolderHierarchy")), "сначала — SyncFolderHierarchy");

    // Первые письма «Входящих»: заголовки списка.
    let rows = live.cache.page(&inbox.id, 0, 50).unwrap();
    let mut subjects: Vec<&str> = rows.iter().map(|r| r.subject.as_str()).collect();
    subjects.sort_unstable();
    assert_eq!(subjects, ["Re: Планы", "Отчёт за неделю"]);
    let report = rows.iter().find(|r| r.subject == "Отчёт за неделю").unwrap();
    assert!(!report.seen && report.flagged && report.has_attachment);

    // «Прочитано» — UpdateItem.
    let n = log.lock().unwrap().len();
    live.commands.send(Command::Keyword { id: report.id.clone(), keyword: "$seen", on: true }).unwrap();
    let start = Instant::now();
    while !log.lock().unwrap()[n..].iter().any(|b| b.contains("<m:UpdateItem")) {
        assert!(start.elapsed() < Duration::from_secs(10), "UpdateItem не ушёл");
        std::thread::sleep(Duration::from_millis(50));
    }
    let upd = log.lock().unwrap().iter().rev().find(|b| b.contains("<m:UpdateItem")).cloned().unwrap();
    assert!(upd.contains(r#"FieldURI="message:IsRead""#) && upd.contains("<t:IsRead>true</t:IsRead>") && upd.contains("AAMkADmsg1"));

    // Справочник — FindPeople по GAL: должность и отдел (телефонов FindPeople у Exchange 2013 не отдаёт).
    live.commands.send(Command::FindPeople { text: "Петр".into() }).unwrap();
    let Event::PeopleFound { people, .. } = live.wait("PeopleFound", |e| matches!(e, Event::PeopleFound { .. })) else { unreachable!() };
    assert_eq!(people.len(), 3);
    let anna = &people[0];
    assert_eq!((anna.title.as_str(), anna.department.as_str(), anna.phone.as_str()), ("Ведущий инженер", "Отдел разработки", ""));
    assert_eq!(people[2].kind, "group");
    assert!(log.lock().unwrap().iter().any(|b| b.contains("<m:FindPeople")) && !log.lock().unwrap().iter().any(|b| b.contains("<m:ResolveNames")));

    // Отправка из MIME — CreateItem SendAndSaveCopy.
    let draft = postman_compose::Draft {
        from: postman_compose::Address::new("", "user1@example.test"),
        to: vec![postman_compose::Address::new("Борис", "boris@example.test")],
        subject: "Проверка".into(),
        markdown: "Тело".into(),
        ..Default::default()
    };
    live.commands.send(Command::Send { draft: Box::new(draft), replace: None, ticket: 7, release: postman_core::model::Release::Now }).unwrap();
    live.wait("Sent", |e| matches!(e, Event::Sent { ticket: 7, .. }));
    let create = log.lock().unwrap().iter().rev().find(|b| b.contains("<m:CreateItem")).cloned().unwrap();
    assert!(create.contains(r#"MessageDisposition="SendAndSaveCopy""#) && create.contains("<t:MimeContent>"));

    // Неделя календаря — областями EWS.
    live.commands.send(Command::Events { from: 1_792_368_000, to: 1_792_972_800 }).unwrap();
    let Event::EventsLoaded { events, .. } = live.wait("EventsLoaded", |e| matches!(e, Event::EventsLoaded { .. })) else { unreachable!() };
    assert_eq!(events.iter().map(|e| e.title.as_str()).collect::<Vec<_>>(), ["Планёрка & отчёт"]);
}

/// Запросы теста с операцией `op` (после `from`-го).
fn requests(log: &Arc<Mutex<Vec<String>>>, from: usize, op: &str) -> Vec<String> {
    log.lock().unwrap()[from..].iter().filter(|b| b.contains(&format!("<m:{op}"))).cloned().collect()
}

/// Учётка поднялась: области подключены, первая сверка прошла.
fn ready(url: &str) -> Live {
    let live = start(url);
    live.wait("Synced", |e| matches!(e, Event::Status(Status::Synced)));
    live
}

/// UID встречи корпуса (`get_item_meeting.xml`).
const MEETING_UID: &str = "040000008200E00074C5B7101A82E00800000000A1B2C3D4";
/// UID, который сервер задал новой встрече.
const NEW_UID: &str = "040000008200E00074C5B7101A82E008000000005E2F0000";

fn ts(s: &str) -> i64 {
    s.parse::<jiff::Timestamp>().unwrap().as_second()
}

/// Приглашение из письма (как его даёт воркер): организатор, я, UID и время встречи.
fn invite(uid: &str) -> postman_calendar::Invitation {
    let person = |name: &str, email: &str, owner: bool| postman_calendar::Participant {
        name: name.into(),
        email: email.into(),
        owner,
        attendee: true,
        optional: false,
        status: postman_calendar::Partstat::NeedsAction,
    };
    let event = postman_calendar::Event {
        id: String::new(),
        uid: uid.into(),
        calendar_ids: Vec::new(),
        title: "Планёрка".into(),
        description: String::new(),
        location: String::new(),
        span: postman_calendar::Span::new(ts("2026-10-20T07:00:00Z"), ts("2026-10-20T08:00:00Z")),
        all_day: false,
        status: postman_calendar::EventStatus::Confirmed,
        free_busy: postman_calendar::FreeBusy::Busy,
        participants: vec![person("Анна Петрова", "anna@example.test", true), person("", "user1@example.test", false)],
        recurring: false,
        keywords: Vec::new(),
        sequence: 0,
        owner: None,
    };
    postman_calendar::Invitation { method: "REQUEST".into(), event, sequence: 0, raw: serde_json::json!({ "uid": uid }) }
}

/// Ответ на приглашение: встреча с UID приглашения в календаре Exchange — `AcceptItem` по ней (ответ шлёт сервер);
/// встречи нет — письмо iMIP организатору (`CreateItem` из MIME).
#[test]
fn invite_reply_by_uid_or_by_mail() {
    let (url, log) = server();
    let live = ready(&url);
    let accepted = postman_calendar::Partstat::Accepted;
    let answered = |e: &Event| matches!(e, Event::InviteAnswered { .. } | Event::InviteFailed { .. });

    let n = log.lock().unwrap().len();
    live.commands.send(Command::RespondInvite { invite: Box::new(invite(MEETING_UID)), status: accepted, in_reply_to: None }).unwrap();
    let Event::InviteAnswered { delivery, uid, .. } = live.wait("InviteAnswered", answered) else { panic!("ответ не ушёл") };
    assert_eq!((delivery, uid.as_str()), (postman_core::invite::Delivery::Server, MEETING_UID));
    let sent = requests(&log, n, "CreateItem");
    assert_eq!(sent.len(), 1, "один ответ");
    assert!(
        sent[0].contains("<t:AcceptItem>") && sent[0].contains(r#"<t:ReferenceItemId Id="AAMkADmeet" ChangeKey="DwAAABYAAAA7"/>"#),
        "{}",
        sent[0]
    );
    assert!(!sent[0].contains("<t:MimeContent>"), "не письмом");

    let n = log.lock().unwrap().len();
    let other = invite("not-in-calendar@example.test");
    live.commands.send(Command::RespondInvite { invite: Box::new(other), status: accepted, in_reply_to: Some("<inv@example.test>".into()) }).unwrap();
    let Event::InviteAnswered { delivery, .. } = live.wait("InviteAnswered", answered) else { panic!("ответ письмом не ушёл") };
    assert_eq!(delivery, postman_core::invite::Delivery::Mail { imported: false });
    let sent = requests(&log, n, "CreateItem");
    assert_eq!(sent.len(), 1);
    assert!(sent[0].contains("<t:MimeContent>") && sent[0].contains(r#"MessageDisposition="SendAndSaveCopy""#));
    assert!(!sent[0].contains("AcceptItem"));
}

/// `GetItem` новой встречи: UID, который задал сервер.
fn new_meeting(body: &str) -> Option<String> {
    (body.contains("<m:GetItem") && body.contains(r#"Id="AAMkADnew""#))
        .then(|| fixture("get_item_meeting.xml").replace(r#"Id="AAMkADmeet""#, r#"Id="AAMkADnew""#).replace(MEETING_UID, NEW_UID))
}

/// Новая встреча вкладки «Встреча»: `CreateItem` с рассылкой сервером (участники, пояс Windows, время UTC, без своего
/// UID) → `MeetingCreated` с UID, который задал сервер.
#[test]
fn new_meeting_request_and_server_uid() {
    let (url, log) = server_with(new_meeting);
    let live = ready(&url);
    let meeting = postman_core::schedule::Meeting {
        uid: "client-uid".into(),
        title: "Обзор <Q4>".into(),
        location: "Зал".into(),
        description: "Повестка".into(),
        start: "2026-11-02T10:00:00".parse().unwrap(),
        time_zone: "Europe/Moscow".into(),
        duration_secs: 3600,
        attendees: vec![("Борис".into(), "boris@example.test".into())],
        optional: vec![("Вера".into(), "vera@example.test".into())],
        from: None,
    };
    let n = log.lock().unwrap().len();
    live.commands.send(Command::CreateMeeting { meeting: Box::new(meeting), ticket: 3 }).unwrap();
    let created = |e: &Event| matches!(e, Event::MeetingCreated { ticket: 3, .. } | Event::MeetingFailed { .. });
    let Event::MeetingCreated { uid, .. } = live.wait("MeetingCreated", created) else { panic!("встреча не создана") };
    assert_eq!(uid, NEW_UID, "UID — от сервера, не свой");
    live.wait("CalendarChanged", |e| matches!(e, Event::CalendarChanged));

    let create = requests(&log, n, "CreateItem");
    assert_eq!(create.len(), 1);
    let c = &create[0];
    for part in [
        r#"<m:CreateItem SendMeetingInvitations="SendToAllAndSaveCopy">"#,
        "<t:Subject>Обзор &lt;Q4&gt;</t:Subject>",
        "<t:Start>2026-11-02T07:00:00Z</t:Start><t:End>2026-11-02T08:00:00Z</t:End>",
        "<t:RequiredAttendees><t:Attendee><t:Mailbox><t:Name>Борис</t:Name><t:EmailAddress>boris@example.test</t:EmailAddress>",
        "<t:OptionalAttendees><t:Attendee><t:Mailbox><t:Name>Вера</t:Name><t:EmailAddress>vera@example.test</t:EmailAddress>",
        r#"<t:StartTimeZone Id="Russian Standard Time"/><t:EndTimeZone Id="Russian Standard Time"/>"#,
    ] {
        assert!(c.contains(part), "нет «{part}» в {c}");
    }
    assert!(!c.contains("<t:UID>") && !c.contains("client-uid"), "UID задаёт сервер");
    assert!(requests(&log, n, "GetItem").iter().any(|b| b.contains(r#"Id="AAMkADnew""#)), "UID — из GetItem новой встречи");
}

/// Занятость: моя — первым адресом запроса, затем участники в порядке вкладки встречи; адрес, которого сервер не нашёл,
/// — «нет данных» (`None`), а не «свободен».
#[test]
fn free_busy_in_order_unknown_is_no_data() {
    let (url, log) = server();
    let live = ready(&url);
    let n = log.lock().unwrap().len();
    let emails = vec!["vera@example.test".to_owned(), "nobody@example.test".to_owned()];
    let (from, to) = (ts("2026-10-19T00:00:00Z"), ts("2026-10-26T00:00:00Z"));
    live.commands.send(Command::Availability { ticket: 9, emails, from, to, sender: None }).unwrap();
    let Event::Availability { me, people, error, .. } = live.wait("Availability", |e| matches!(e, Event::Availability { ticket: 9, .. })) else {
        unreachable!()
    };
    assert_eq!(error, None);
    assert_eq!(me.len(), 2, "моя занятость — первый ответ (без «свободен»)");
    assert_eq!(people.iter().map(|(e, _)| e.as_str()).collect::<Vec<_>>(), ["vera@example.test", "nobody@example.test"]);
    let vera = people[0].1.as_ref().expect("занятость известна");
    assert_eq!(vera[0].kind, postman_calendar::BusyKind::Unavailable, "OOF — «вне офиса»");
    assert_eq!(people[1].1, None, "не найден — нет данных");

    let req = requests(&log, n, "GetUserAvailabilityRequest").pop().unwrap();
    let order: Vec<&str> = req.split("<t:Address>").skip(1).filter_map(|s| s.split("</t:Address>").next()).collect();
    assert_eq!(order, [r"EXAMPLE\user1", "vera@example.test", "nobody@example.test"], "я — первым, затем по порядку");
}

/// `GetItem` календаря: все три встречи окна корпуса (экземпляр серии, исключение, одиночная) с теми же версиями.
fn whole_week(body: &str) -> Option<String> {
    if !(body.contains("<m:GetItem") && body.contains("calendar:UID")) {
        return None;
    }
    let one = fixture("get_item_meeting.xml");
    let start = one.find("<t:CalendarItem>").unwrap();
    let end = one.find("</t:CalendarItem>").unwrap() + "</t:CalendarItem>".len();
    let item = |id: &str, kind: &str| {
        one[start..end]
            .replace(r#"Id="AAMkADmeet" ChangeKey="DwAAABYAAAA7""#, id)
            .replace("<t:CalendarItemType>Single</t:CalendarItemType>", &format!("<t:CalendarItemType>{kind}</t:CalendarItemType>"))
    };
    let items = [
        item(r#"Id="AAMkADocc1" ChangeKey="DwAAABYAAAA8""#, "Occurrence"),
        item(r#"Id="AAMkADexc1" ChangeKey="DwAAABYAAAA9""#, "Exception"),
        item(r#"Id="AAMkADmeet" ChangeKey="DwAAABYAAAA7""#, "Single"),
    ]
    .concat();
    let head = &one[..one.find("<m:ResponseMessages>").unwrap()];
    Some(format!(
        r#"{head}<m:ResponseMessages><m:GetItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Items>{items}</m:Items></m:GetItemResponseMessage></m:ResponseMessages></m:GetItemResponse></s:Body></s:Envelope>"#
    ))
}

/// Неделя второй раз: встречи той же версии (`ChangeKey` окна совпал) заново не запрашиваются; экземпляры серии —
/// отдельными событиями с общим UID.
#[test]
fn unchanged_meeting_is_not_fetched_again() {
    let (url, log) = server_with(whole_week);
    let live = ready(&url);
    let (from, to) = (ts("2026-10-12T00:00:00Z"), ts("2026-10-26T00:00:00Z"));
    let week = |live: &Live| {
        live.commands.send(Command::Events { from, to }).unwrap();
        let Event::EventsLoaded { events, .. } = live.wait("EventsLoaded", |e| matches!(e, Event::EventsLoaded { .. })) else { unreachable!() };
        events
    };
    let n = log.lock().unwrap().len();
    let first = week(&live);
    assert_eq!(first.len(), 3);
    assert!(first.iter().all(|e| e.uid == MEETING_UID));
    assert_eq!(first.iter().filter(|e| e.recurring).count(), 2, "экземпляр и исключение — серия");
    let details = |from: usize| requests(&log, from, "GetItem").into_iter().filter(|b| b.contains("calendar:UID")).count();
    assert_eq!(details(n), 1, "подробности — одним запросом");

    let n = log.lock().unwrap().len();
    let again = week(&live);
    assert_eq!(again.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), first.iter().map(|e| e.id.as_str()).collect::<Vec<_>>());
    assert_eq!(requests(&log, n, "FindItem").len(), 1, "окно — снова");
    assert_eq!(details(n), 0, "те же версии — без GetItem");
}

/// Сервер без `FindPeople` (отказ схемы): справочник — `ResolveNames`, и дальше `FindPeople` не спрашивается.
#[test]
fn people_fall_back_to_resolve_names() {
    let (url, log) = server_with(|b| b.contains("<m:FindPeople").then(|| fixture("fault_schema.xml")));
    let live = ready(&url);
    for text in ["Петр", "Анна"] {
        live.commands.send(Command::FindPeople { text: text.into() }).unwrap();
        let Event::PeopleFound { people, .. } = live.wait("PeopleFound", |e| matches!(e, Event::PeopleFound { .. })) else { unreachable!() };
        assert_eq!(people.len(), 3);
        assert_eq!((people[0].department.as_str(), people[0].phone.as_str()), ("Отдел разработки", "+7 495 000-00-01"), "у ResolveNames — и телефон");
    }
    assert_eq!(requests(&log, 0, "FindPeople").len(), 1, "отказал — больше не спрашиваем");
    assert_eq!(requests(&log, 0, "ResolveNames").len(), 2);
}

/// Правка и отмена своей встречи (S7-07б): `UpdateItem` по встрече недели — тема, время (UTC), пояс, участники,
/// рассылка всем с копией; `DeleteItem` — отмена участникам, встреча — в «Удалённые».
#[test]
fn meeting_edit_and_cancel_requests() {
    let (url, log) = server();
    let live = ready(&url);
    live.commands.send(Command::Events { from: ts("2026-10-12T00:00:00Z"), to: ts("2026-10-26T00:00:00Z") }).unwrap();
    let Event::EventsLoaded { events, .. } = live.wait("EventsLoaded", |e| matches!(e, Event::EventsLoaded { .. })) else { unreachable!() };
    let ev = events.into_iter().find(|e| e.id == "AAMkADmeet").expect("встреча корпуса");
    let meeting = postman_core::schedule::Meeting {
        uid: ev.uid.clone(),
        title: "Планёрка, позже".into(),
        location: String::new(),
        description: String::new(),
        start: "2026-10-20T11:00:00".parse().unwrap(),
        time_zone: "Europe/Moscow".into(),
        duration_secs: 3600,
        attendees: vec![("Борис".into(), "boris@example.test".into())],
        optional: vec![],
        from: None,
    };
    let n = log.lock().unwrap().len();
    live.commands.send(Command::UpdateMeeting { event: Box::new(ev.clone()), meeting: Box::new(meeting), series: false, ticket: 21 }).unwrap();
    let done =
        |e: &Event| matches!(e, Event::MeetingUpdated { ticket: 21, .. } | Event::MeetingCancelled { ticket: 22, .. } | Event::MeetingFailed { .. });
    let Event::MeetingUpdated { uid, .. } = live.wait("MeetingUpdated", done) else { panic!("правка не ушла") };
    assert_eq!(uid, MEETING_UID);
    let upd = requests(&log, n, "UpdateItem").pop().expect("UpdateItem");
    for part in [
        r#"SendMeetingInvitationsOrCancellations="SendToAllAndSaveCopy""#,
        r#"<t:ItemId Id="AAMkADmeet" ChangeKey="DwAAABYAAAA7"/>"#,
        "<t:Subject>Планёрка, позже</t:Subject>",
        "<t:Start>2026-10-20T08:00:00Z</t:Start>",
        "<t:End>2026-10-20T09:00:00Z</t:End>",
        r#"<t:StartTimeZone Id="Russian Standard Time"/>"#,
        "<t:EmailAddress>boris@example.test</t:EmailAddress>",
        r#"<t:DeleteItemField><t:FieldURI FieldURI="calendar:Location"/>"#,
    ] {
        assert!(upd.contains(part), "нет «{part}» в {upd}");
    }

    let n = log.lock().unwrap().len();
    live.commands.send(Command::CancelMeeting { event: Box::new(ev), series: false, from: None, ticket: 22 }).unwrap();
    let Event::MeetingCancelled { .. } = live.wait("MeetingCancelled", done) else { panic!("отмена не ушла") };
    let del = requests(&log, n, "DeleteItem").pop().expect("DeleteItem");
    assert!(
        del.contains(r#"DeleteType="MoveToDeletedItems" SendMeetingCancellations="SendToAllAndSaveCopy""#) && del.contains("AAMkADmeet"),
        "{del}"
    );
}
