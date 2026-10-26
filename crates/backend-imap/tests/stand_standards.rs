//! Учётная запись «стандарты» через синхронизацию Postman (`postman_backend_imap` + `postman_backend_dav`, S7-05б) против
//! IMAP, submission, CalDAV и CardDAV стенда Stalwart: событие недели, встреча user1 → user2 с ответом и отменой,
//! занятость коллеги, задачи двумя клиентами, контакт. Без стенда тесты пропускаются (`#[ignore]`); за собой
//! убирают: события, задачи, карточки, права календаря, входящие планирования, письма сервера о встрече.
//!
//! Запуск (пароли и адрес — только в окружении; самоподписанный сертификат стенда — `POSTMAN_INSECURE_TLS=1`;
//! воркер — `cargo build -p postman-worker` заранее):
//!   POSTMAN_STAND_IMAP=<адрес>:993 POSTMAN_STAND_SMTP=<адрес>:465 POSTMAN_STAND_DAV=https://<адрес>
//!   POSTMAN_STAND_USER1=… POSTMAN_STAND_PASSWORD1=… POSTMAN_STAND_USER2=… POSTMAN_STAND_PASSWORD2=…
//!   cargo test -p postman-backend-imap --test stand_standards -- --ignored --test-threads=1

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use postman_backend_imap::{self as imap, ImapAccount};
use postman_cache::Cache;
use postman_calendar::{Invitation, Partstat, Task};
use postman_core::model::{ContactCard, Feature, Features};
use postman_core::schedule::Meeting;
use postman_core::sync::{Command, Event, Status, UnboundedSender};
use postman_core::tasks::{Reply, Request};

const WAIT: Duration = Duration::from_secs(40);
/// Сколько ждать, пока правка задачи у одного клиента дойдёт до другого (опрос `Refresh`): сервер отдаёт дельту
/// `sync-collection` не сразу.
const PROPAGATE: Duration = Duration::from_secs(90);
/// Пауза между опросами `Refresh`: ответы предыдущего опроса за это время дочитываются все.
const POLL_EVERY: Duration = Duration::from_secs(2);

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} не задан: стенд — docs/STALWART.md"))
}

/// Воркер-песочница — рядом с исполняемым файлом теста (`target/debug/deps`), как у приложения.
fn worker_next_to_tests() {
    let exe = std::env::current_exe().unwrap();
    let deps = exe.parent().unwrap();
    let name = "postman-worker.exe";
    let built = deps.parent().unwrap().join(name);
    if !deps.join(name).exists() {
        std::fs::copy(&built, deps.join(name)).unwrap_or_else(|e| panic!("{}: {e} — сначала cargo build -p postman-worker", built.display()));
    }
}

fn stamp() -> String {
    format!("{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis())
}

fn now() -> i64 {
    jiff::Timestamp::now().as_second()
}

/// Начало дня через `days` дней, 10:00 UTC, — окно встречи; неделя вокруг — окно запросов.
fn slot(days: i64) -> i64 {
    (now() / 86_400 + days) * 86_400 + 10 * 3600
}

fn dav(n: u8) -> postman_dav::Client {
    postman_dav::Client::new(&postman_dav::Config {
        url: env("POSTMAN_STAND_DAV"),
        user: env(&format!("POSTMAN_STAND_USER{n}")),
        password: env(&format!("POSTMAN_STAND_PASSWORD{n}")),
    })
    .unwrap()
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

struct Live {
    commands: UnboundedSender<Command>,
    events: Receiver<Event>,
    me: String,
    features: Features,
}

impl Live {
    fn start(n: u8) -> Live {
        worker_next_to_tests();
        let me = env(&format!("POSTMAN_STAND_USER{n}"));
        let url = format!("imaps://{}?smtp={}&dav={}", env("POSTMAN_STAND_IMAP"), env("POSTMAN_STAND_SMTP"), env("POSTMAN_STAND_DAV"));
        let account = ImapAccount::from_url(&url, &me, &env(&format!("POSTMAN_STAND_PASSWORD{n}"))).unwrap();
        let (tx, events) = std::sync::mpsc::channel();
        let commands =
            imap::spawn(account, None, Cache::open_in_memory().unwrap(), None, tx, || {}, vec![std::sync::Arc::new(postman_backend_dav::DavPlugin)]);
        let mut live = Live { commands, events, me, features: Features::NONE };
        let f = live.wait("Features", |e| matches!(e, Event::Features(_)));
        let Event::Features(f) = f else { unreachable!() };
        live.features = f;
        live.wait("Synced", |e| matches!(e, Event::Status(Status::Synced)));
        live
    }

    fn send(&self, c: Command) {
        self.commands.send(c).unwrap();
    }

    fn wait(&self, what: &str, pick: impl Fn(&Event) -> bool) -> Event {
        let start = Instant::now();
        loop {
            let left = WAIT.checked_sub(start.elapsed()).unwrap_or_else(|| panic!("не дождались: {what}"));
            match self.events.recv_timeout(left) {
                Ok(Event::Error(e)) => panic!("синхронизация: {e}"),
                Ok(Event::CalendarFailed(e)) => panic!("календарь: {e}"),
                Ok(ev) if pick(&ev) => return ev,
                Ok(_) => {}
                Err(e) => panic!("не дождались: {what} ({e})"),
            }
        }
    }

    /// События окна; повторять, пока `ok` не согласится (сервер рассылает и применяет ответы не мгновенно).
    fn events_until(&self, from: i64, to: i64, what: &str, ok: impl Fn(&[postman_calendar::Event]) -> bool) -> Vec<postman_calendar::Event> {
        let start = Instant::now();
        loop {
            self.send(Command::Events { from, to });
            let Event::EventsLoaded { events, .. } = self.wait("EventsLoaded", |e| matches!(e, Event::EventsLoaded { .. })) else { unreachable!() };
            if ok(&events) {
                return events;
            }
            assert!(start.elapsed() < WAIT, "{what}: не дождались");
            std::thread::sleep(Duration::from_millis(700));
        }
    }

    /// Задача `uid` у этого клиента — по ответам задач: `Some(задача)` — есть, `None` — нет. Опрос `Refresh` с
    /// паузой [`POLL_EVERY`], пока `done` по последнему известному состоянию не истинно или не вышел срок
    /// [`PROPAGATE`]. Ответы читаются все, а не по одному на опрос: дельта приходит любым из них.
    fn poll_task(&self, uid: &str, what: &str, done: impl Fn(Option<&Task>) -> bool) {
        let start = Instant::now();
        let mut known: Option<Option<Task>> = None;
        loop {
            self.send(Command::Tasks(Request::Refresh));
            let round = Instant::now();
            while let Some(left) = POLL_EVERY.checked_sub(round.elapsed()) {
                match self.events.recv_timeout(left) {
                    Ok(Event::Error(e)) => panic!("синхронизация: {e}"),
                    Ok(Event::Tasks(Reply::Failed { error, .. })) => panic!("задачи: {error}"),
                    Ok(Event::Tasks(Reply::Loaded(v))) => known = Some(v.into_iter().find(|t| t.uid == uid)),
                    Ok(Event::Tasks(Reply::Changed(v))) => {
                        if let Some(t) = v.into_iter().find(|t| t.uid == uid) {
                            known = Some(Some(t));
                        }
                    }
                    Ok(Event::Tasks(Reply::Removed(v))) if v.iter().any(|u| u == uid) => known = Some(None),
                    Ok(_) => {}
                    Err(_) => break,
                }
                if known.as_ref().is_some_and(|k| done(k.as_ref())) {
                    return;
                }
            }
            assert!(start.elapsed() < PROPAGATE, "не дошло до второго клиента за {PROPAGATE:?}: {what}");
        }
    }

    fn tasks(&self, r: Request, what: &str, pick: impl Fn(&Reply) -> bool) -> Reply {
        self.send(Command::Tasks(r));
        let Event::Tasks(reply) = self.wait(what, |e| match e {
            Event::Tasks(Reply::Failed { error, .. }) => panic!("задачи: {error}"),
            Event::Tasks(r) => pick(r),
            _ => false,
        }) else {
            unreachable!()
        };
        reply
    }
}

/// Удалить у пользователя `n` ресурсы календаря с UID `marker` (и во входящих планирования) — уборка.
fn clean_calendar(n: u8, marker: &str) {
    rt().block_on(async {
        let c = dav(n);
        let d = c.discover().await.unwrap();
        let mut places: Vec<String> = Vec::new();
        for home in &d.calendar_home {
            places.extend(c.calendars(home).await.unwrap().into_iter().map(|c| c.href));
        }
        for cal in &places {
            for r in c.calendar_query(cal, "VEVENT", None).await.unwrap().into_iter().chain(c.calendar_query(cal, "VTODO", None).await.unwrap()) {
                if r.data.contains(marker) {
                    let _ = c.delete(&r.href, postman_dav::Precondition::None).await;
                }
            }
        }
        if let Some(inbox) = d.schedule_inbox {
            for r in c.inbox(&inbox).await.unwrap_or_default() {
                if r.data.contains(marker) {
                    let _ = c.delete(&r.href, postman_dav::Precondition::None).await;
                }
            }
        }
    });
}

/// Письма сервера о встрече у обоих — по IMAP (тема содержит `marker`).
fn clean_letters(marker: &str) {
    rt().block_on(async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let at = env("POSTMAN_STAND_IMAP");
        let (host, port) = at.rsplit_once(':').unwrap();
        for n in [1u8, 2] {
            let cfg = postman_imap::Config {
                host: host.into(),
                port: port.parse().unwrap(),
                security: postman_imap::Security::Tls,
                user: env(&format!("POSTMAN_STAND_USER{n}")),
                password: env(&format!("POSTMAN_STAND_PASSWORD{n}")),
            };
            let Ok(mut s) = postman_imap::Session::connect(&cfg).await else { continue };
            if s.select("INBOX").await.is_ok()
                && let Ok(uids) = s.search_text(marker).await
                && !uids.is_empty()
            {
                let _ = s.delete(&postman_imap::uid_set(&uids)).await;
            }
            let _ = s.logout().await;
        }
    });
}

fn meeting(uid: &str, title: &str, start: i64, attendees: Vec<(String, String)>) -> Meeting {
    let local = jiff::Timestamp::from_second(start).unwrap().to_zoned(jiff::tz::TimeZone::UTC).datetime();
    Meeting {
        uid: uid.into(),
        title: title.into(),
        location: "Переговорная".into(),
        description: "Проверка CalDAV".into(),
        start: local,
        time_zone: "Etc/UTC".into(),
        duration_secs: 3600,
        attendees,
        optional: Vec::new(),
        from: None,
    }
}

/// Событие через синхронизацию: `Features` по CalDAV/CardDAV, `CreateMeeting` без участников → неделя (повторы —
/// сервером) → поиск по теме → убрано.
#[test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
fn caldav_event_round_trip() {
    let live = Live::start(1);
    for f in [Feature::Calendar, Feature::Tasks, Feature::Contacts, Feature::FreeBusy] {
        assert!(live.features.has(f), "{f:?}: {:?}", live.features);
    }
    assert!(!live.features.has(Feature::Directory) && !live.features.has(Feature::Rules));
    let uid = format!("postman-std-ev-{}", stamp());
    let at = slot(43);
    live.send(Command::CreateMeeting { meeting: Box::new(meeting(&uid, &format!("Событие {uid}"), at, Vec::new())), ticket: 1 });
    live.wait("MeetingCreated", |e| match e {
        Event::MeetingFailed { error, .. } => panic!("встреча: {error}"),
        Event::MeetingCreated { ticket: 1, .. } => true,
        _ => false,
    });
    let week = live.events_until(at - 3 * 86_400, at + 3 * 86_400, "событие в неделе", |e| e.iter().any(|x| x.uid == uid));
    let e = week.iter().find(|x| x.uid == uid).unwrap();
    assert_eq!((e.span.start, e.span.end, e.location.as_str()), (at, at + 3600, "Переговорная"));
    live.send(Command::SearchEvents { text: uid.clone(), from: at - 86_400, to: at + 86_400 });
    let Event::EventsFound { events, error, .. } = live.wait("EventsFound", |e| matches!(e, Event::EventsFound { .. })) else { unreachable!() };
    assert!(error.is_none() && events.iter().any(|x| x.uid == uid));
    clean_calendar(1, &uid);
    live.events_until(at - 86_400, at + 86_400, "событие убрано", |e| !e.iter().any(|x| x.uid == uid));
}

/// Встреча user1 → user2: приглашение рассылает сервер → у user2 в неделе со статусом «нужен ответ» → «принять»
/// своей копией (`Delivery::Server`) → у user1 user2 принял → убрано (отмена, копии, входящие, письма).
#[test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
fn caldav_meeting_cycle() {
    let (a, b) = (Live::start(1), Live::start(2));
    let uid = format!("postman-std-meet-{}", stamp());
    let at = slot(44);
    let (from, to) = (at - 86_400, at + 86_400);
    a.send(Command::CreateMeeting {
        meeting: Box::new(meeting(&uid, &format!("Встреча {uid}"), at, vec![("User 2".into(), b.me.clone())])),
        ticket: 2,
    });
    a.wait("MeetingCreated", |e| matches!(e, Event::MeetingCreated { ticket: 2, .. } | Event::MeetingFailed { .. }));
    // Не дошло — первым делом ограничение входящих Stalwart (25 в час на «домен отправителя × получатель», правило
    // «Sender address to recipient throttle», `docs/STALWART.md` п. 17): приглашение теряется молча, у отправителя успех.
    let what = "приглашение у user2 (ограничение входящих Stalwart? docs/STALWART.md п. 17; журнал сервера — smtp.rate-limit-exceeded)";
    let theirs = b.events_until(from, to, what, |e| e.iter().any(|x| x.uid == uid));
    let ev = theirs.into_iter().find(|x| x.uid == uid).unwrap();
    let mine = |e: &postman_calendar::Event, who: &str| e.participants.iter().find(|p| p.email.eq_ignore_ascii_case(who)).map(|p| p.status);
    assert_eq!(mine(&ev, &b.me), Some(Partstat::NeedsAction));
    let inv = Invitation { method: "REQUEST".into(), event: ev, sequence: 0, raw: serde_json::Value::Null };
    b.send(Command::RespondInvite { invite: Box::new(inv), status: Partstat::Accepted, in_reply_to: None });
    let Event::InviteAnswered { delivery, .. } = b.wait("InviteAnswered", |e| match e {
        Event::InviteFailed { error, .. } => panic!("ответ: {error}"),
        Event::InviteAnswered { .. } => true,
        _ => false,
    }) else {
        unreachable!()
    };
    assert_eq!(delivery, postman_core::invite::Delivery::Server);
    let peer = b.me.clone();
    a.events_until(from, to, "ответ у организатора", |e| {
        e.iter().any(|x| x.uid == uid && mine(x, &peer) == Some(Partstat::Accepted))
    });
    clean_calendar(1, &uid);
    clean_calendar(2, &uid);
    clean_letters(&uid);
}

/// Занятость: событие user2 и календарь user2 открыт user1 (`read` и `read-free-busy`, `docs/STALWART.md` п. 46) →
/// `Availability` user1 → занятость user2 на событии; `ShareFreeBusy` (только `read-free-busy`) → право на месте.
#[test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
fn caldav_free_busy() {
    let a = Live::start(1);
    let peer = env("POSTMAN_STAND_USER2");
    let uid = format!("postman-std-fb-{}", stamp());
    let at = slot(45);
    let (c2, principal1, cal2) = rt().block_on(async {
        let c2 = dav(2);
        let d2 = c2.discover().await.unwrap();
        let cal2 = c2.calendars(&d2.calendar_home[0]).await.unwrap().into_iter().find(|c| c.supports("VEVENT")).unwrap().href;
        let ics = format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Postman//stand//RU\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20261023T000000Z\r\nDTSTART:{}\r\nDTEND:{}\r\nSUMMARY:Занят\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
            jiff::Timestamp::from_second(at).unwrap().strftime("%Y%m%dT%H%M%SZ"),
            jiff::Timestamp::from_second(at + 3600).unwrap().strftime("%Y%m%dT%H%M%SZ")
        );
        let href = postman_dav::child_href(&cal2, &uid, postman_dav::Content::Calendar);
        c2.put(&href, postman_dav::Content::Calendar, &ics, postman_dav::Precondition::Create).await.unwrap();
        let p1 = dav(1).discover().await.unwrap().principal;
        c2.share(&cal2, &p1, &[postman_dav::Privilege::read(), postman_dav::Privilege::read_free_busy()]).await.unwrap();
        (c2, p1, cal2)
    });
    a.send(Command::Availability {
        ticket: 3,
        emails: vec![peer.clone(), format!("nobody-{uid}@itt.test")],
        from: at - 3600,
        to: at + 7200,
        sender: None,
    });
    let Event::Availability { people, error, .. } = a.wait("Availability", |e| matches!(e, Event::Availability { ticket: 3, .. })) else {
        unreachable!()
    };
    let unshare = || rt().block_on(async { c2.unshare(&cal2, &principal1).await.unwrap() });
    unshare();
    clean_calendar(2, &uid);
    assert!(error.is_none(), "{error:?}");
    let busy = people.iter().find(|(e, _)| e.eq_ignore_ascii_case(&peer)).and_then(|(_, b)| b.clone()).expect("занятость user2");
    assert!(busy.iter().any(|b| b.span.start == at && b.span.end == at + 3600), "{busy:?}");
    assert!(people.iter().any(|(e, b)| e.starts_with("nobody-") && b.is_none()));

    // «Открыть мою занятость» коллеге — только `read-free-busy` моих календарей.
    a.send(Command::ShareFreeBusy { email: peer.clone() });
    a.wait("FreeBusyShared", |e| matches!(e, Event::FreeBusyShared { .. }));
    rt().block_on(async {
        let c1 = dav(1);
        let d1 = c1.discover().await.unwrap();
        let p2 = dav(2).discover().await.unwrap().principal;
        let cal1 = c1.calendars(&d1.calendar_home[0]).await.unwrap().into_iter().find(|c| c.supports("VEVENT")).unwrap().href;
        let acl = c1.acl(&cal1).await.unwrap();
        let ace =
            acl.iter().find(|a| matches!(&a.who, postman_dav::acl::Who::Href(h) if postman_dav::same_href(h, &p2))).expect("запись ACL для user2");
        assert!(ace.grant.contains(&postman_dav::Privilege::read_free_busy()) && !ace.grant.contains(&postman_dav::Privilege::read()));
        assert!(c1.unshare(&cal1, &p2).await.unwrap());
    });
}

/// Задачи (VTODO) двумя клиентами: создать у первого → у второго после загрузки → выполнить вторым → у первого
/// «выполнена» по дельте `sync-collection` → удалить → у второго нет. Задача с исполнителем user2 хранится с
/// `SCHEDULE-AGENT=CLIENT`: сервер user2 ничего не шлёт.
#[test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
fn caldav_task_round_trip() {
    let (a, b) = (Live::start(1), Live::start(1));
    let uid = format!("postman-std-task-{}", stamp());
    a.tasks(Request::Load, "Loaded", |r| matches!(r, Reply::Loaded(_)));
    let mut t = Task::new(&uid, "", &format!("Задача {uid}"));
    t.notes = "Проверка VTODO".into();
    t.participants.push(postman_calendar::TaskParticipant {
        key: "a1".into(),
        name: "User 2".into(),
        email: env("POSTMAN_STAND_USER2"),
        role: postman_calendar::Role::Assignee,
        status: postman_calendar::TaskPartstat::NeedsAction,
    });
    let me = a.me.clone();
    t.participants.push(postman_calendar::TaskParticipant {
        key: "o".into(),
        name: "User 1".into(),
        email: me,
        role: postman_calendar::Role::Owner,
        status: postman_calendar::TaskPartstat::Accepted,
    });
    a.send(Command::Tasks(Request::Save(Box::new(t))));
    // Записана на сервере — у строки появился id (путь).
    a.wait("задача на сервере", |e| matches!(e, Event::Tasks(Reply::Changed(v)) if v.iter().any(|x| x.uid == uid && !x.id.is_empty())));

    let Reply::Loaded(seen) = b.tasks(Request::Load, "Loaded у второго", |r| matches!(r, Reply::Loaded(v) if v.iter().any(|x| x.uid == uid)))
    else {
        unreachable!()
    };
    let mut theirs = seen.into_iter().find(|x| x.uid == uid).unwrap();
    assert_eq!(theirs.notes, "Проверка VTODO");
    theirs.set_done(true, now());
    b.send(Command::Tasks(Request::Save(Box::new(theirs))));
    b.wait(
        "выполнена на сервере",
        |e| matches!(e, Event::Tasks(Reply::Changed(v)) if v.iter().any(|x| x.uid == uid && x.progress.is_closed() && !x.id.is_empty())),
    );

    a.poll_task(&uid, "выполнение — у первого клиента", |t| t.is_some_and(|t| t.progress.is_closed()));

    // Исполнителю сервер ничего не прислал: письма поручения шлёт Postman (`SCHEDULE-AGENT=CLIENT`).
    let leaked = rt().block_on(async {
        let c2 = dav(2);
        let d2 = c2.discover().await.unwrap();
        let mut n = 0;
        for cal in c2.calendars(&d2.calendar_home[0]).await.unwrap() {
            n += c2.calendar_query(&cal.href, "VTODO", None).await.unwrap().iter().filter(|r| r.data.contains(&uid)).count();
        }
        n
    });

    a.send(Command::Tasks(Request::Delete(uid.clone())));
    a.wait("Removed", |e| matches!(e, Event::Tasks(Reply::Removed(v)) if v.contains(&uid)));
    b.poll_task(&uid, "удаление — у второго клиента", |t| t.is_none());
    clean_calendar(1, &uid);
    clean_calendar(2, &uid);
    assert_eq!(leaked, 0, "сервер разослал задачу с SCHEDULE-AGENT=CLIENT");
}

/// Контакт через синхронизацию: создать → в списке и подсказках → правка (телефон, должность) → удалить.
#[test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
fn carddav_round_trip() {
    let live = Live::start(1);
    let name = format!("Проверкин {}", stamp());
    let card = ContactCard {
        name: name.clone(),
        emails: vec![format!("{}@example.test", stamp())],
        phones: vec!["+7 900 000-00-00".into()],
        ..Default::default()
    };
    live.send(Command::SaveContact { card: Box::new(card), old: None, ticket: 5 });
    let Event::ContactSaved { id, .. } = live.wait("ContactSaved", |e| match e {
        Event::ContactFailed { error, .. } => panic!("контакт: {error}"),
        Event::ContactSaved { ticket: 5, .. } => true,
        _ => false,
    }) else {
        unreachable!()
    };
    let Event::ContactsLoaded(cards) = live.wait("ContactsLoaded", |e| matches!(e, Event::ContactsLoaded(_))) else { unreachable!() };
    let saved = cards.into_iter().find(|c| c.id == id).expect("карточка в списке");
    assert_eq!((saved.name.as_str(), saved.phones.len()), (name.as_str(), 1));

    let edited = ContactCard { phones: Vec::new(), title: "Директор".into(), organization: "Ромашка".into(), ..saved.clone() };
    live.send(Command::SaveContact { card: Box::new(edited), old: Some(Box::new(saved)), ticket: 6 });
    live.wait("ContactSaved", |e| matches!(e, Event::ContactSaved { ticket: 6, .. }));
    let Event::ContactsLoaded(cards) = live.wait("ContactsLoaded", |e| matches!(e, Event::ContactsLoaded(_))) else { unreachable!() };
    let after = cards.into_iter().find(|c| c.id == id).expect("карточка после правки");
    assert_eq!((after.title.as_str(), after.organization.as_str(), after.phones.len()), ("Директор", "Ромашка", 0));

    live.send(Command::DeleteContact { id: id.clone(), ticket: 7 });
    live.wait("ContactDeleted", |e| matches!(e, Event::ContactDeleted { ticket: 7, .. }));
    let Event::ContactsLoaded(cards) = live.wait("ContactsLoaded", |e| matches!(e, Event::ContactsLoaded(_))) else { unreachable!() };
    assert!(!cards.iter().any(|c| c.id == id));
}

/// Правка и отмена своей встречи (S7-07б) user1 → user2: `UpdateMeeting` (на час позже) — у user2 новое время;
/// `CancelMeeting` — у user2 встречи нет (или отменена). Серия (каждый день, 3 раза, PUT напрямую): второй экземпляр —
/// на час позже (исключение), третий — отменён (`EXDATE`), затем вся серия (DELETE). Уборка — календари и письма.
#[test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
fn caldav_meeting_edit_and_cancel() {
    // Забытое упавшими прогонами — убрать сначала.
    clean_calendar(1, "postman-std-edit-");
    clean_calendar(2, "postman-std-edit-");
    let (a, b) = (Live::start(1), Live::start(2));
    let uid = format!("postman-std-edit-{}", stamp());
    let at = slot(46);
    let (from, to) = (at - 86_400, at + 4 * 86_400);
    let done = |what: &str, ticket: u64| {
        a.wait(what, |e| match e {
            Event::MeetingFailed { ticket: t, error } if *t == ticket => panic!("{what}: {error}"),
            Event::MeetingUpdated { ticket: t, .. } | Event::MeetingCancelled { ticket: t, .. } => *t == ticket,
            _ => false,
        });
    };
    let peer = vec![("User 2".to_string(), b.me.clone())];
    a.send(Command::CreateMeeting { meeting: Box::new(meeting(&uid, &format!("Правка {uid}"), at, peer.clone())), ticket: 1 });
    a.wait("MeetingCreated", |e| matches!(e, Event::MeetingCreated { ticket: 1, .. }));
    b.events_until(from, to, "приглашение у user2 (п. 17 STALWART.md?)", |e| e.iter().any(|x| x.uid == uid));
    let mine = a.events_until(from, to, "встреча у user1", |e| e.iter().any(|x| x.uid == uid));
    let ev = mine.into_iter().find(|x| x.uid == uid).unwrap();
    a.send(Command::UpdateMeeting {
        event: Box::new(ev.clone()),
        meeting: Box::new(meeting(&uid, &format!("Позже {uid}"), at + 3600, peer.clone())),
        series: false,
        ticket: 2,
    });
    done("MeetingUpdated", 2);
    b.events_until(from, to, "перенос у user2", |e| e.iter().any(|x| x.uid == uid && x.span.start == at + 3600));
    let ev = a.events_until(from, to, "перенос у user1", |e| e.iter().any(|x| x.uid == uid && x.span.start == at + 3600));
    let ev = ev.into_iter().find(|x| x.uid == uid).unwrap();
    a.send(Command::CancelMeeting { event: Box::new(ev), series: false, from: None, ticket: 3 });
    done("MeetingCancelled", 3);
    // Отменённая копия у участника остаётся со `STATUS:CANCELLED` (`docs/STALWART.md`) — неделя её не показывает.
    b.events_until(from, to, "отмена у user2", |e| !e.iter().any(|x| x.uid == uid));

    // Серия user1 (без участников — проверка правки экземпляра и EXDATE, рассылка — выше).
    let suid = format!("{uid}-series");
    rt().block_on(async {
        let c = dav(1);
        let d = c.discover().await.unwrap();
        let cal = c.calendars(&d.calendar_home[0]).await.unwrap().into_iter().find(|c| c.supports("VEVENT")).unwrap().href;
        let ics = format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Postman//stand//RU\r\nBEGIN:VEVENT\r\nUID:{suid}\r\nDTSTAMP:20261023T000000Z\r\nDTSTART:{}\r\nDTEND:{}\r\nRRULE:FREQ=DAILY;COUNT=3\r\nSUMMARY:Серия {suid}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
            jiff::Timestamp::from_second(at).unwrap().strftime("%Y%m%dT%H%M%SZ"),
            jiff::Timestamp::from_second(at + 1800).unwrap().strftime("%Y%m%dT%H%M%SZ")
        );
        let href = postman_dav::child_href(&cal, &suid, postman_dav::Content::Calendar);
        c.put(&href, postman_dav::Content::Calendar, &ics, postman_dav::Precondition::Create).await.unwrap();
    });
    let ours = |e: &[postman_calendar::Event]| e.iter().filter(|x| x.uid == suid).cloned().collect::<Vec<_>>();
    let all = ours(&a.events_until(from, to, "три экземпляра", |e| ours(e).len() == 3));
    let second = all.iter().find(|x| x.span.start == at + 86_400).unwrap().clone();
    let mut later = meeting(&suid, &format!("Серия {suid}"), at + 86_400 + 3600, Vec::new());
    later.duration_secs = 1800;
    a.send(Command::UpdateMeeting { event: Box::new(second), meeting: Box::new(later), series: false, ticket: 4 });
    done("MeetingUpdated экземпляра", 4);
    // Stalwart v0.16.27: пока у серии нет `EXDATE`, `expand` отдаёт и исключение, и прежний экземпляр (ресурс —
    // верный, RFC 5545 §3.8.4.4; `docs/STALWART-GAPS.md` №49) — прежний здесь терпим, но печатаем.
    let all = ours(&a.events_until(from, to, "исключение", |e| ours(e).iter().any(|x| x.span.start == at + 86_400 + 3600)));
    let gap = all.iter().any(|x| x.span.start == at + 86_400);
    println!("исключение экземпляра: прежний экземпляр в expand — {}", if gap { "остался (№49)" } else { "заменён" });
    // Исключение — ещё раз (его `RECURRENCE-ID` в `expand` у Stalwart — новое начало, №49): ещё на полчаса позже.
    let moved = all.iter().find(|x| x.span.start == at + 86_400 + 3600).unwrap().clone();
    let mut again = meeting(&suid, &format!("Серия {suid}"), at + 86_400 + 5400, Vec::new());
    again.duration_secs = 1800;
    a.send(Command::UpdateMeeting { event: Box::new(moved), meeting: Box::new(again), series: false, ticket: 7 });
    done("MeetingUpdated исключения", 7);
    let twice = |e: &[postman_calendar::Event]| {
        let o = ours(e);
        o.iter().any(|x| x.span.start == at + 86_400 + 5400) && !o.iter().any(|x| x.span.start == at + 86_400 + 3600)
    };
    let all = ours(&a.events_until(from, to, "исключение ещё раз — то же исключение", twice));
    let third = all.iter().find(|x| x.span.start == at + 2 * 86_400).unwrap().clone();
    a.send(Command::CancelMeeting { event: Box::new(third), series: false, from: None, ticket: 5 });
    done("MeetingCancelled экземпляра", 5);
    let gone = |e: &[postman_calendar::Event]| {
        let o = ours(e);
        !o.iter().any(|x| x.span.start == at + 2 * 86_400) && o.iter().any(|x| x.span.start == at + 86_400 + 5400)
    };
    let all = ours(&a.events_until(from, to, "без третьего", gone));
    assert!(all.iter().any(|x| x.span.start == at), "первый — на месте");
    a.send(Command::CancelMeeting { event: Box::new(all[0].clone()), series: true, from: None, ticket: 6 });
    done("MeetingCancelled серии", 6);
    a.events_until(from, to, "серии нет", |e| ours(e).is_empty());
    clean_calendar(1, &uid);
    clean_calendar(2, &uid);
    clean_letters("postman-std-edit-");
}
