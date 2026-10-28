//! Учётная запись Exchange через плагин Postman (`postman_backend_ews`, S7-06г) против боевого Exchange компании — только
//! тестовые учётки (S7 «Проверка на боевом Exchange»). Стенда Exchange нет: без окружения тесты пропускаются
//! (`#[ignore]`), CI их не запускает.
//!
//! Правила: только тестовые учётки; письма и встречи — только им (`test_recipient`); созданное — с меткой
//! `[postman-test <прогон>]` в теме, убирается в начале и в конце (`cleanup`, только ящики из окружения); справочник и
//! занятость — только чтение; запросы — последовательно (клиент EWS — не чаще 5 в секунду), ожидание — раз в 5 с, не
//! дольше минуты. Вывод — только счётчики и коды. Встречи (S7-07) — только между тестовыми учётками; справочник и
//! занятость (S7-08) — только чтение.
//!
//! Окружение — как у `postman-ews` (`crates/ews/tests/stand.rs`): POSTMAN_EWS_URL (или Autodiscover по адресу),
//! POSTMAN_EWS_USER, POSTMAN_EWS_PASSWORD, POSTMAN_EWS_USER2/PASSWORD2 (необязательно).
//! Запуск (воркер — `cargo build -p postman-worker` заранее):
//!   cargo test -p postman-backend-ews --test stand_exchange -- --ignored --test-threads=1

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use postman_cache::Cache;
use postman_core::backend::{MailPlugin, Spawn};
use postman_core::model::{Auth, Feature, Features};
use postman_core::schedule::Meeting;
use postman_core::sync::{Account, Command, Event, Status, UnboundedSender};
use postman_ews::{Client, DeleteMode, WellKnown};

const WAIT: Duration = Duration::from_secs(60);
/// Метка всего, что создают тесты: по ней — уборка.
const TAG: &str = "[postman-test";

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn need(name: &str) -> String {
    env(name).unwrap_or_else(|| panic!("{name} не задан: S7 «Проверка на боевом Exchange»"))
}

fn tag() -> String {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
    format!("{TAG} {ms}]")
}

fn now() -> i64 {
    jiff::Timestamp::now().as_second()
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

struct Account2 {
    user: String,
    password: String,
    email: String,
}

fn account(n: &str) -> Option<Account2> {
    let user = env(&format!("POSTMAN_EWS_USER{n}"))?;
    let password = need(&format!("POSTMAN_EWS_PASSWORD{n}"));
    let email = env(&format!("POSTMAN_EWS_EMAIL{n}")).unwrap_or_else(|| user.clone());
    // Страховка правил S7: только тестовые учётки.
    assert!(email.split('@').next().unwrap_or("").to_lowercase().contains("test"), "учётка не похожа на тестовую — стоп");
    Some(Account2 { user, password, email })
}

fn first() -> Account2 {
    account("").unwrap_or_else(|| panic!("POSTMAN_EWS_USER не задан"))
}

/// Только адреса тестовых учёток; любой другой — паника до отправки.
fn test_recipient(email: &str) -> String {
    let ok = [account(""), account("2")].into_iter().flatten().any(|a| a.email.eq_ignore_ascii_case(email));
    assert!(ok, "адресат не из тестовых учёток — отправка запрещена");
    email.to_owned()
}

async fn ews_url(a: &Account2) -> String {
    match env("POSTMAN_EWS_URL") {
        Some(u) => u,
        None => postman_ews::discover(&a.email, &postman_backend_ews::auth(&a.user, &a.password), None).await.unwrap().urls.remove(0),
    }
}

/// Созданное тестами (метка в теме) — насовсем, без рассылки, только в ящике тестовой учётки.
async fn cleanup(a: &Account2) {
    let mut c = Client::new(&ews_url(a).await, postman_backend_ews::auth(&a.user, &a.password)).unwrap();
    // Контакты — по «Хранить как» (тема у контакта EWS пустая).
    for folder in [WellKnown::Contacts, WellKnown::DeletedItems] {
        if let Ok(ids) = c.find_contacts(&postman_ews::FolderRef::WellKnown(folder), TAG, 20).await
            && !ids.is_empty()
            && let Err(e) = c.delete(&ids, DeleteMode::HardDelete, postman_ews::Notify::None).await
        {
            eprintln!("уборка контактов {folder:?}: {}", e.code().unwrap_or("ошибка"));
        }
    }
    for folder in [WellKnown::Calendar, WellKnown::Inbox, WellKnown::Drafts, WellKnown::SentItems, WellKnown::DeletedItems] {
        if let Ok(ids) = c.find_by_subject(folder, TAG, 20).await
            && !ids.is_empty()
            && let Err(e) = c.delete(&ids, DeleteMode::HardDelete, postman_ews::Notify::None).await
        {
            eprintln!("уборка {folder:?}: {}", e.code().unwrap_or("ошибка"));
        }
    }
}

fn worker_next_to_tests() {
    let exe = std::env::current_exe().unwrap();
    let deps = exe.parent().unwrap();
    let name = "postman-worker.exe";
    let built = deps.parent().unwrap().join(name);
    if !deps.join(name).exists() {
        std::fs::copy(&built, deps.join(name)).unwrap_or_else(|e| panic!("{}: {e} — сначала cargo build -p postman-worker", built.display()));
    }
}

struct Live {
    commands: UnboundedSender<Command>,
    events: Receiver<Event>,
    cache: Cache,
    features: Features,
    /// Адрес ящика, который синхронизация нашла у сервера (`Event::Me`).
    me: String,
}

impl Live {
    /// Учётка Exchange, как её заводит «Добавить аккаунт»: адрес — URL EWS, вход — логин и пароль.
    fn start(a: &Account2) -> Live {
        Live::start_with(a, &a.password)
    }

    /// Учётка с этим паролем; пустой — вход учётной записью Windows без пароля (T1-02а).
    fn start_with(a: &Account2, password: &str) -> Live {
        worker_next_to_tests();
        let url = rt().block_on(ews_url(a));
        let (tx, events) = std::sync::mpsc::channel();
        let cache = Cache::open_in_memory().unwrap();
        let account = Account { url, auth: Auth::Basic { user: a.user.clone(), password: password.to_owned() } };
        let commands = postman_backend_ews::EwsPlugin.spawn(Spawn {
            account,
            profile: None,
            cache: cache.clone(),
            index: None,
            events: tx,
            repaint: Box::new(|| {}),
            areas: Vec::new(),
        });
        let mut live = Live { commands, events, cache, features: Features::NONE, me: String::new() };
        let Event::Features(f) = live.wait("Features", |e| matches!(e, Event::Features(_))) else { unreachable!() };
        live.features = f;
        let Event::Me { address, .. } = live.wait("Me", |e| matches!(e, Event::Me { .. })) else { unreachable!() };
        live.me = address.email;
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
                Ok(Event::Error(e)) => panic!("синхронизация: {:?}", e.kind),
                Ok(Event::CalendarFailed(_)) => panic!("календарь Exchange не ответил"),
                Ok(ev) if pick(&ev) => return ev,
                Ok(_) => {}
                Err(e) => panic!("не дождались: {what} ({e})"),
            }
        }
    }

    fn inbox(&self) -> String {
        self.cache.mailboxes().unwrap().into_iter().find(|b| b.role.as_deref() == Some("inbox")).expect("«Входящие»").id
    }

    /// Неделя `[from, to)`, пока не выполнится условие: `Events` раз в 5 с, не дольше минуты.
    fn events_until(&self, from: i64, to: i64, what: &str, ok: impl Fn(&[postman_calendar::Event]) -> bool) -> Vec<postman_calendar::Event> {
        let start = Instant::now();
        loop {
            self.send(Command::Events { from, to });
            let Event::EventsLoaded { events, .. } = self.wait("EventsLoaded", |e| matches!(e, Event::EventsLoaded { .. })) else { unreachable!() };
            if ok(&events) {
                return events;
            }
            assert!(start.elapsed() < WAIT, "не дождались за минуту: {what}");
            std::thread::sleep(Duration::from_secs(5));
        }
    }

    /// Письмо с меткой во «Входящих»: сверка по `Reconnect` раз в 5 с, не дольше минуты.
    fn arrived(&self, tag: &str) -> postman_cache::EmailRow {
        let start = Instant::now();
        loop {
            let found = self.cache.page(&self.inbox(), 0, 200).unwrap().into_iter().find(|r| r.subject.contains(tag));
            if let Some(r) = found {
                return r;
            }
            assert!(start.elapsed() < WAIT, "письмо не пришло за минуту");
            self.send(Command::Reconnect);
            std::thread::sleep(Duration::from_secs(5));
            while self.events.try_recv().is_ok() {}
        }
    }
}

/// Вход учётки Exchange целиком: почта, календарь, занятость, личные контакты и справочник; задач и правил — нет.
#[test]
#[ignore = "боевой Exchange: S7 «Проверка на боевом Exchange»"]
fn sign_in() {
    let live = Live::start(&first());
    for f in [Feature::Calendar, Feature::FreeBusy, Feature::Contacts, Feature::Directory] {
        assert!(live.features.has(f), "{f:?}");
    }
    for f in [Feature::Tasks, Feature::Rules, Feature::Autoreply] {
        assert!(!live.features.has(f), "{f:?} у Exchange не показывается");
    }
    let boxes = live.cache.mailboxes().unwrap();
    for role in ["inbox", "sent", "drafts", "trash"] {
        assert!(boxes.iter().any(|b| b.role.as_deref() == Some(role)), "папка {role}");
    }
    assert!(live.me.eq_ignore_ascii_case(&first().email), "адрес ящика — у сервера, совпал с учёткой");
    println!("папок: {}", boxes.len());
}

/// Вход текущим пользователем Windows без пароля (T1-02а): адрес ящика найден у сервера. Только если сам процесс
/// запущен от тестовой учётки (`runas`): вход Windows на машине владельца — его ящик, тесты его не трогают (S7).
#[test]
#[ignore = "боевой Exchange: S7 «Проверка на боевом Exchange»; процесс — от тестовой учётки"]
fn sign_in_windows() {
    let a = first();
    let me = env("USERNAME").unwrap_or_default().to_lowercase();
    let login = a.user.to_lowercase();
    let account = login.rsplit_once('\\').map_or(login.as_str(), |(_, u)| u);
    let account = account.split_once('@').map_or(account, |(u, _)| u);
    if me.is_empty() || me != account {
        println!("пропуск: процесс не от тестовой учётки — вход Windows дал бы чужой ящик");
        return;
    }
    let live = Live::start_with(&a, "");
    assert!(live.me.eq_ignore_ascii_case(&a.email), "адрес ящика — у сервера");
    assert!(live.cache.mailboxes().unwrap().iter().any(|b| b.role.as_deref() == Some("inbox")));
    println!("вход Windows: почта есть, адрес найден");
}

/// Почта через синхронизацию Postman: письмо себе, во «Входящих», тело, «прочитано», удаление.
#[test]
#[ignore = "боевой Exchange: S7 «Проверка на боевом Exchange»"]
fn exchange_mail_round_trip() {
    let a = first();
    rt().block_on(cleanup(&a));
    let live = Live::start(&a);
    let tag = tag();
    let me = test_recipient(&a.email);
    let draft = postman_compose::Draft {
        from: postman_compose::Address::new("", &me),
        to: vec![postman_compose::Address::new("", &me)],
        subject: format!("{tag} plugin"),
        markdown: "Проверка почты Exchange через плагин.".into(),
        ..Default::default()
    };
    live.send(Command::Send { draft: Box::new(draft), replace: None, ticket: 1, release: postman_core::model::Release::Now });
    live.wait("Sent", |e| matches!(e, Event::Sent { ticket: 1, .. }));
    let row = live.arrived(&tag);
    assert!(!row.seen && row.size > 0);

    live.send(Command::Body(row.id.clone()));
    live.wait("BodyReady", |e| matches!(e, Event::BodyReady(k) if *k == row.id));
    let body = live.cache.body(&row.id).unwrap().expect("тело");
    assert!(body.text.unwrap_or_default().contains("Проверка почты Exchange"));

    live.send(Command::Keyword { id: row.id.clone(), keyword: "$seen", on: true });
    live.send(Command::Destroy(row.id.clone()));
    live.wait("удалено", |e| matches!(e, Event::MailboxesChanged));
    rt().block_on(cleanup(&a));
    println!("почта: отправлено, пришло, тело, прочитано, удалено");
}

/// Календарь, справочник и занятость через синхронизацию: своя встреча в неделе, своя учётка в GAL, занятость.
#[test]
#[ignore = "боевой Exchange: S7 «Проверка на боевом Exchange»"]
fn exchange_calendar_and_people() {
    let a = first();
    rt().block_on(cleanup(&a));
    let live = Live::start(&a);
    let title = format!("Встреча {}", tag());
    let start = jiff::Timestamp::from_second((now() / 3600 + 48) * 3600).unwrap();
    let local = start.to_zoned(jiff::tz::TimeZone::get("Europe/Moscow").unwrap()).datetime();
    let meeting = Meeting {
        uid: String::new(),
        title: title.clone(),
        location: String::new(),
        description: String::new(),
        start: local,
        time_zone: "Europe/Moscow".into(),
        duration_secs: 1800,
        attendees: vec![],
        optional: vec![],
        from: None,
    };
    live.send(Command::CreateMeeting { meeting: Box::new(meeting), ticket: 2 });
    live.wait("MeetingCreated", |e| matches!(e, Event::MeetingCreated { ticket: 2, .. }));
    live.send(Command::Events { from: start.as_second() - 86_400, to: start.as_second() + 86_400 });
    let Event::EventsLoaded { events, .. } = live.wait("EventsLoaded", |e| matches!(e, Event::EventsLoaded { .. })) else { unreachable!() };
    let mine = events.iter().find(|e| e.title == title).expect("встреча в неделе");
    assert_eq!(mine.span.start, start.as_second());

    live.send(Command::FindPeople { text: a.email.clone(), offset: 0 });
    let Event::PeopleFound { people, .. } = live.wait("PeopleFound", |e| matches!(e, Event::PeopleFound { .. })) else { unreachable!() };
    assert!(people.iter().any(|p| p.email.as_deref().is_some_and(|e| e.eq_ignore_ascii_case(&a.email))), "своя учётка в GAL");

    let other = account("2").map_or(a.email.clone(), |b| b.email);
    live.send(Command::Availability { ticket: 3, emails: vec![other], from: now(), to: now() + 7 * 86_400, sender: None });
    let Event::Availability { people, error, .. } = live.wait("Availability", |e| matches!(e, Event::Availability { ticket: 3, .. })) else {
        unreachable!()
    };
    assert_eq!(error, None);
    assert!(people[0].1.is_some(), "занятость тестовой учётки известна");
    rt().block_on(cleanup(&a));
    println!("календарь, справочник, занятость: ок");
}

/// Уборка по метке у всех учёток — и при падении теста (`Drop`).
struct Cleanup(Vec<Account2>);

impl Drop for Cleanup {
    fn drop(&mut self) {
        for a in &self.0 {
            rt().block_on(cleanup(a));
        }
    }
}

/// Области EWS учётки напрямую: перенос и отмена встречи — у плагина есть, команд интерфейса для них пока нет.
fn areas(a: &Account2) -> (tokio::runtime::Runtime, postman_backend_ews::EwsAreas) {
    let rt = rt();
    let url = rt.block_on(ews_url(a));
    let areas = rt.block_on(postman_backend_ews::EwsAreas::connect(&url, &a.user, &a.password, &a.email)).unwrap();
    (rt, areas)
}

/// Приглашение из письма, как его видит карточка письма: часть `text/calendar` (`Command::Blob`) → воркер → `Invitation`.
fn invite_in_mail(live: &Live, tag: &str) -> (postman_calendar::Invitation, Option<String>) {
    let row = live.arrived(tag);
    live.send(Command::Body(row.id.clone()));
    live.wait("BodyReady", |e| matches!(e, Event::BodyReady(k) if *k == row.id));
    let body = live.cache.body(&row.id).unwrap().expect("тело приглашения");
    // «Прочитано» у письма встречи (`MeetingRequest`) — та же правка, что у письма.
    live.send(Command::Keyword { id: row.id.clone(), keyword: "$seen", on: true });
    let part = body.attachments.iter().find(|a| a.mime.to_ascii_lowercase().starts_with("text/calendar")).expect("часть text/calendar в письме");
    live.send(Command::Blob { blob_id: part.blob_id.clone(), name: part.name.clone(), mime: part.mime.clone() });
    let Event::BlobReady { data, .. } = live.wait("BlobReady", |e| matches!(e, Event::BlobReady { blob_id, .. } if *blob_id == part.blob_id)) else {
        unreachable!()
    };
    let v = postman_worker::parse_calendar(&data).expect("приглашение разобрано");
    let inv = postman_calendar::Invitation::from_worker(&v, &jiff::tz::TimeZone::UTC).expect("приглашение");
    (inv, body.message_id)
}

fn status_of(e: &postman_calendar::Event, who: &str) -> Option<postman_calendar::Partstat> {
    e.participants.iter().find(|p| p.email.eq_ignore_ascii_case(who)).map(|p| p.status)
}

/// Встреча user1 → user2 через плагин: приглашение рассылает сервер (UID — от сервера) → у user2 письмо-приглашение,
/// UID его `text/calendar` = UID встречи в календаре (карточка письма находит встречу) → «Принять» из карточки — ответ
/// сервером → у user1 user2 принял → перенос на час (у user2 — новое время) → отмена (у user2 встречи нет) → уборка.
#[test]
#[ignore = "боевой Exchange: S7 «Проверка на боевом Exchange»"]
fn exchange_meeting_cycle() {
    let (a, b) = (first(), account("2").expect("POSTMAN_EWS_USER2 — вторая тестовая учётка"));
    let _guard = Cleanup(vec![first(), account("2").unwrap()]);
    rt().block_on(cleanup(&a));
    rt().block_on(cleanup(&b));
    let (la, lb) = (Live::start(&a), Live::start(&b));
    let tag = tag();
    let title = format!("Встреча {tag}");
    let tz = "Europe/Moscow";
    let start = jiff::Timestamp::from_second((now() / 3600 + 72) * 3600).unwrap();
    let local = start.to_zoned(jiff::tz::TimeZone::get(tz).unwrap()).datetime();
    let (from, to) = (start.as_second() - 86_400, start.as_second() + 86_400);
    let meeting = Meeting {
        uid: String::new(),
        title: title.clone(),
        location: String::new(),
        description: "Проверка приглашений Postman".into(),
        start: local,
        time_zone: tz.into(),
        duration_secs: 1800,
        attendees: vec![(String::new(), test_recipient(&b.email))],
        optional: vec![],
        from: None,
    };
    la.send(Command::CreateMeeting { meeting: Box::new(meeting), ticket: 5 });
    let Event::MeetingCreated { uid, .. } = la.wait("MeetingCreated", |e| matches!(e, Event::MeetingCreated { ticket: 5, .. })) else {
        unreachable!()
    };
    assert!(!uid.is_empty(), "UID — от сервера");

    // У user2: письмо-приглашение и встреча в календаре с тем же UID.
    let (inv, message_id) = invite_in_mail(&lb, &tag);
    let same_uid = inv.event.uid.eq_ignore_ascii_case(&uid);
    let theirs = lb.events_until(from, to, "встреча у user2", |e| e.iter().any(|x| x.title == title));
    let in_calendar = theirs.iter().find(|x| x.title == title).unwrap();
    assert_eq!(status_of(in_calendar, &b.email), Some(postman_calendar::Partstat::NeedsAction), "у user2 — ждёт ответа");
    let smtp = in_calendar.participants.iter().all(|p| p.email.contains('@'));
    let organizer_uid = in_calendar.uid.eq_ignore_ascii_case(&uid);
    println!(
        "UID письма = UID организатора: {same_uid}; UID встречи у участника = UID организатора: {organizer_uid}; адреса участников SMTP: {smtp}"
    );
    assert!(same_uid, "UID приглашения в письме ≠ UID встречи: карточка письма не найдёт встречу");

    // «Принять» из карточки письма: ответ — сервером (по встрече с этим UID), не письмом.
    lb.send(Command::RespondInvite { invite: Box::new(inv), status: postman_calendar::Partstat::Accepted, in_reply_to: message_id });
    let Event::InviteAnswered { delivery, .. } = lb.wait("InviteAnswered", |e| match e {
        Event::InviteFailed { error, .. } => panic!("ответ не ушёл ({} символов ошибки)", error.len()),
        Event::InviteAnswered { .. } => true,
        _ => false,
    }) else {
        unreachable!()
    };
    assert_eq!(delivery, postman_core::invite::Delivery::Server);
    let peer = b.email.clone();
    let accepted =
        |e: &[postman_calendar::Event]| e.iter().any(|x| x.title == title && status_of(x, &peer) == Some(postman_calendar::Partstat::Accepted));
    let mine = la.events_until(from, to, "ответ у организатора", accepted);
    let event = mine.into_iter().find(|x| x.title == title).unwrap();

    // Перенос на час и отмена — областями плагина (изменение и отмена уходят участникам сервером).
    let (rt1, mut ar) = areas(&a);
    let moved = postman_calendar::Span::new(event.span.start + 3600, event.span.end + 3600);
    assert!(rt1.block_on(ar.reschedule(&event, &postman_backend_ews::areas::moved(moved, tz), false)).unwrap());
    lb.events_until(from, to + 3600, "перенос у user2", |e| e.iter().any(|x| x.title == title && x.span.start == moved.start));
    let week = la.events_until(from, to + 3600, "перенос у user1", |e| e.iter().any(|x| x.title == title && x.span.start == moved.start));
    let event = week.into_iter().find(|x| x.title == title).unwrap();
    assert!(rt1.block_on(ar.cancel(&event, false)).unwrap());
    lb.events_until(from, to + 3600, "отмена у user2", |e| !e.iter().any(|x| x.title == title));
    println!("встреча через плагин: приглашение, UID письма, ответ сервером, перенос, отмена");
}

/// Приглашение на серию (user1 → user2, 2 недели): ответ из карточки письма — всей серии (по серии, а не по экземпляру)
/// → у организатора user2 принял серию.
#[test]
#[ignore = "боевой Exchange: S7 «Проверка на боевом Exchange»"]
fn exchange_series_invite() {
    let (a, b) = (first(), account("2").expect("POSTMAN_EWS_USER2 — вторая тестовая учётка"));
    let _guard = Cleanup(vec![first(), account("2").unwrap()]);
    let lb = Live::start(&b);
    let tag = tag();
    let rt = rt();
    let mut c1 = Client::new(&rt.block_on(ews_url(&a)), postman_backend_ews::auth(&a.user, &a.password)).unwrap();
    let tz = jiff::tz::TimeZone::get("Europe/Moscow").unwrap();
    let day = jiff::Timestamp::now().to_zoned(tz.clone()).date().checked_add(jiff::ToSpan::days(8)).unwrap();
    let start = day.at(11, 0, 0, 0).to_zoned(tz).unwrap().timestamp().as_second();
    let item = postman_ews::NewItem {
        title: format!("Серия {tag}"),
        description: String::new(),
        location: String::new(),
        span: postman_calendar::Span::new(start, start + 900),
        all_day: false,
        time_zone: Some("Europe/Moscow".into()),
        free_busy: postman_calendar::FreeBusy::Busy,
        required: vec![postman_ews::Attendee { name: String::new(), email: test_recipient(&b.email) }],
        optional: vec![],
        recurrence: Some(postman_ews::Recurrence {
            pattern: postman_ews::Pattern::Daily { interval: 7 },
            range: postman_ews::Range::Numbered { start: day, count: 2 },
        }),
    };
    let id = rt.block_on(c1.create(None, &item, postman_ews::Notify::All)).unwrap();
    let (inv, message_id) = invite_in_mail(&lb, &tag);
    assert!(inv.event.recurring, "приглашение на серию");
    lb.send(Command::RespondInvite { invite: Box::new(inv), status: postman_calendar::Partstat::Accepted, in_reply_to: message_id });
    let answered = |e: &Event| matches!(e, Event::InviteAnswered { .. } | Event::InviteFailed { .. });
    let Event::InviteAnswered { delivery, .. } = lb.wait("InviteAnswered", answered) else { panic!("ответ на серию не ушёл") };
    assert_eq!(delivery, postman_core::invite::Delivery::Server);
    let started = Instant::now();
    loop {
        let master = rt.block_on(c1.get_items(std::slice::from_ref(&id))).unwrap().pop().unwrap();
        if status_of(&master.event, &b.email) == Some(postman_calendar::Partstat::Accepted) {
            break;
        }
        assert!(started.elapsed() < WAIT, "у серии организатора нет ответа user2 — ответили экземпляру?");
        std::thread::sleep(Duration::from_secs(5));
    }
    println!("серия: ответ из письма — всей серии");
}

/// Серия своей учётки (без участников): экземпляры — отдельными событиями недели с общим UID; перенос одного экземпляра
/// — исключение на новом месте; удаление экземпляра — его нет, остальные на месте.
#[test]
#[ignore = "боевой Exchange: S7 «Проверка на боевом Exchange»"]
fn exchange_recurring_in_week() {
    let a = first();
    let _guard = Cleanup(vec![first()]);
    let live = Live::start(&a);
    let tag = tag();
    let rt0 = rt();
    let mut c = Client::new(&rt0.block_on(ews_url(&a)), postman_backend_ews::auth(&a.user, &a.password)).unwrap();
    let tz = jiff::tz::TimeZone::get("Europe/Moscow").unwrap();
    let day = jiff::Timestamp::now().to_zoned(tz.clone()).date().checked_add(jiff::ToSpan::days(9)).unwrap();
    let start = day.at(9, 0, 0, 0).to_zoned(tz).unwrap().timestamp().as_second();
    let title = format!("Серия {tag}");
    let item = postman_ews::NewItem {
        title: title.clone(),
        description: String::new(),
        location: String::new(),
        span: postman_calendar::Span::new(start, start + 900),
        all_day: false,
        time_zone: Some("Europe/Moscow".into()),
        free_busy: postman_calendar::FreeBusy::Free,
        required: vec![],
        optional: vec![],
        recurrence: Some(postman_ews::Recurrence {
            pattern: postman_ews::Pattern::Daily { interval: 1 },
            range: postman_ews::Range::Numbered { start: day, count: 3 },
        }),
    };
    rt0.block_on(c.create(None, &item, postman_ews::Notify::None)).unwrap();
    let (from, to) = (start - 3600, start + 4 * 86_400);
    let ours = |e: &[postman_calendar::Event]| e.iter().filter(|x| x.title == title).cloned().collect::<Vec<_>>();
    let week = live.events_until(from, to, "три экземпляра", |e| ours(e).len() == 3);
    let series = ours(&week);
    assert!(series.iter().all(|x| x.recurring && x.uid == series[0].uid), "экземпляры — серия с общим UID");
    assert_eq!(series.iter().map(|x| x.span.start - start).collect::<Vec<_>>(), [0, 86_400, 2 * 86_400], "по дням, то же время");

    let (rt1, mut ar) = areas(&a);
    let second = series[1].clone();
    let later = postman_calendar::Span::new(second.span.start + 7200, second.span.end + 7200);
    assert!(rt1.block_on(ar.reschedule(&second, &postman_backend_ews::areas::moved(later, "Europe/Moscow"), false)).unwrap());
    let week =
        live.events_until(from, to, "исключение на новом месте", |e| ours(e).iter().any(|x| x.span.start == later.start));
    assert_eq!(ours(&week).len(), 3, "перенос экземпляра — не новая встреча");
    let third = ours(&week).into_iter().find(|x| x.span.start == start + 2 * 86_400).unwrap();
    assert!(rt1.block_on(ar.cancel(&third, false)).unwrap());
    let week = live.events_until(from, to, "экземпляр удалён", |e| ours(e).len() == 2);
    assert!(ours(&week).iter().any(|x| x.span.start == start), "первый экземпляр на месте");
    println!("серия в неделе: экземпляры, исключение, удалённый экземпляр");
}

/// Справочник через `FindPeople` (только чтение, до 20): своя учётка находится по началу адреса; печать — только
/// счётчики заполненных полей (должность, отдел, телефон).
#[test]
#[ignore = "боевой Exchange: S7 «Проверка на боевом Exchange»"]
fn exchange_directory_fields() {
    let a = first();
    let live = Live::start(&a);
    let local: String = a.email.split('@').next().unwrap_or("").chars().take(6).collect();
    live.send(Command::FindPeople { text: local, offset: 0 });
    let Event::PeopleFound { people, .. } = live.wait("PeopleFound", |e| matches!(e, Event::PeopleFound { .. })) else { unreachable!() };
    let me = people.iter().any(|p| p.email.as_deref().is_some_and(|e| e.eq_ignore_ascii_case(&a.email)));
    let count = |f: fn(&postman_core::model::Principal) -> bool| people.iter().filter(|p| f(p)).count();
    println!(
        "справочник: найдено {}, своя учётка {me}, с должностью {}, с отделом {}, с телефоном {}",
        people.len(),
        count(|p| !p.title.is_empty()),
        count(|p| !p.department.is_empty()),
        count(|p| !p.phone.is_empty())
    );
    assert!(me, "своя учётка в GAL");
    assert!(people.len() <= postman_backend_ews::areas::PEOPLE_LIMIT);
}

/// Ждать итог правки или отмены встречи с тикетом `ticket` (`MeetingFailed` — провал с кодом).
fn meeting_done(live: &Live, what: &str, ticket: u64) {
    live.wait(what, |e| match e {
        Event::MeetingFailed { ticket: t, error } if *t == ticket => panic!("{what}: {error}"),
        Event::MeetingUpdated { ticket: t, .. } | Event::MeetingCancelled { ticket: t, .. } => *t == ticket,
        _ => false,
    });
}

/// Правка и отмена своей встречи командами интерфейса (S7-07б, `UpdateMeeting`, `CancelMeeting`) user1 → user2:
/// одиночная — на час позже и новая тема (у user2 — новое время), затем отмена (у user2 встречи нет); серия с user2
/// (каждый день, 3 раза) — второй экземпляр на час позже, третий отменён, затем вся серия. Карточка справочника —
/// `PersonDetails` по своему адресу (`ResolveNames`).
#[test]
#[ignore = "боевой Exchange: S7 «Проверка на боевом Exchange»"]
fn exchange_meeting_edit_through_commands() {
    let (a, b) = (first(), account("2").expect("POSTMAN_EWS_USER2 — вторая тестовая учётка"));
    let _guard = Cleanup(vec![first(), account("2").unwrap()]);
    rt().block_on(cleanup(&a));
    rt().block_on(cleanup(&b));
    let (la, lb) = (Live::start(&a), Live::start(&b));
    let tag = tag();
    let tz = "Europe/Moscow";
    let zone = jiff::tz::TimeZone::get(tz).unwrap();
    let local = |ts: i64| jiff::Timestamp::from_second(ts).unwrap().to_zoned(zone.clone()).datetime();
    let start = (now() / 3600 + 96) * 3600;
    let (from, to) = (start - 86_400, start + 4 * 86_400);
    let peer = test_recipient(&b.email);
    let meeting = |title: &str, at: i64| Meeting {
        uid: String::new(),
        title: title.into(),
        location: String::new(),
        description: "Проверка правки Postman".into(),
        start: local(at),
        time_zone: tz.into(),
        duration_secs: 1800,
        attendees: vec![(String::new(), peer.clone())],
        optional: vec![],
        from: None,
    };

    // Одиночная: правка и отмена.
    let title = format!("Правка {tag}");
    la.send(Command::CreateMeeting { meeting: Box::new(meeting(&title, start)), ticket: 11 });
    la.wait("MeetingCreated", |e| matches!(e, Event::MeetingCreated { ticket: 11, .. }));
    let ev = la.events_until(from, to, "встреча у user1", |e| e.iter().any(|x| x.title == title)).into_iter().find(|x| x.title == title).unwrap();
    let later = format!("Позже {tag}");
    la.send(Command::UpdateMeeting { event: Box::new(ev), meeting: Box::new(meeting(&later, start + 3600)), series: false, ticket: 12 });
    meeting_done(&la, "MeetingUpdated", 12);
    lb.events_until(from, to, "правка у user2", |e| e.iter().any(|x| x.title == later && x.span.start == start + 3600));
    let ev = la.events_until(from, to, "правка у user1", |e| e.iter().any(|x| x.title == later)).into_iter().find(|x| x.title == later).unwrap();
    la.send(Command::CancelMeeting { event: Box::new(ev), series: false, from: None, ticket: 13 });
    meeting_done(&la, "MeetingCancelled", 13);
    lb.events_until(from, to, "отмена у user2", |e| !e.iter().any(|x| x.title == later));

    // Серия с участником: экземпляр позже, экземпляр отменён, вся серия отменена.
    let stitle = format!("Серия {tag}");
    let rt0 = rt();
    let mut c1 = Client::new(&rt0.block_on(ews_url(&a)), postman_backend_ews::auth(&a.user, &a.password)).unwrap();
    let day = jiff::Timestamp::from_second(start).unwrap().to_zoned(zone.clone()).date();
    let item = postman_ews::NewItem {
        title: stitle.clone(),
        description: String::new(),
        location: String::new(),
        span: postman_calendar::Span::new(start, start + 1800),
        all_day: false,
        time_zone: Some(tz.into()),
        free_busy: postman_calendar::FreeBusy::Busy,
        required: vec![postman_ews::Attendee { name: String::new(), email: peer.clone() }],
        optional: vec![],
        recurrence: Some(postman_ews::Recurrence {
            pattern: postman_ews::Pattern::Daily { interval: 1 },
            range: postman_ews::Range::Numbered { start: day, count: 3 },
        }),
    };
    rt0.block_on(c1.create(None, &item, postman_ews::Notify::All)).unwrap();
    let ours = |e: &[postman_calendar::Event]| {
        let mut v: Vec<_> = e.iter().filter(|x| x.title == stitle).cloned().collect();
        v.sort_by_key(|x| x.span.start);
        v
    };
    let all = ours(&la.events_until(from, to, "три экземпляра", |e| ours(e).len() == 3));
    let second = all[1].clone();
    let mut moved = meeting(&stitle, second.span.start + 3600);
    moved.description = String::new();
    la.send(Command::UpdateMeeting { event: Box::new(second.clone()), meeting: Box::new(moved), series: false, ticket: 14 });
    meeting_done(&la, "MeetingUpdated экземпляра", 14);
    let all = ours(&la.events_until(from, to, "исключение у user1", |e| ours(e).iter().any(|x| x.span.start == second.span.start + 3600)));
    assert_eq!(all.len(), 3, "перенос экземпляра — не новая встреча");
    lb.events_until(from, to, "исключение у user2", |e| e.iter().any(|x| x.title == stitle && x.span.start == second.span.start + 3600));
    la.send(Command::CancelMeeting { event: Box::new(all[2].clone()), series: false, from: None, ticket: 15 });
    meeting_done(&la, "MeetingCancelled экземпляра", 15);
    let all = ours(&la.events_until(from, to, "без третьего", |e| ours(e).len() == 2));
    la.send(Command::CancelMeeting { event: Box::new(all[0].clone()), series: true, from: None, ticket: 16 });
    meeting_done(&la, "MeetingCancelled серии", 16);
    la.events_until(from, to, "серии нет у user1", |e| ours(e).is_empty());
    lb.events_until(from, to, "серии нет у user2", |e| ours(e).is_empty());

    // Карточка справочника: подробности по адресу.
    la.send(Command::PersonDetails { email: a.email.clone() });
    let Event::PersonDetails { person, .. } = la.wait("PersonDetails", |e| matches!(e, Event::PersonDetails { .. })) else { unreachable!() };
    let p = person.expect("своя учётка по ResolveNames");
    println!("правка командами: одиночная и серия; карточка: телефон {}, должность {}", !p.phone.is_empty(), !p.title.is_empty());
}

/// Письмо, удалённое другим клиентом (T1-20): второй клиент EWS переносит одно в «Удалённые» (как Outlook) и удаляет
/// другое насовсем → после опроса обоих нет во «Входящих» кэша; тело исчезнувшего — без ошибки.
#[test]
#[ignore = "боевой Exchange: S7 «Проверка на боевом Exchange»"]
fn exchange_deleted_elsewhere() {
    let a = first();
    rt().block_on(cleanup(&a));
    let live = Live::start(&a);
    let tag = tag();
    let me = test_recipient(&a.email);
    for (n, what) in [(1, "soft"), (2, "hard")] {
        let draft = postman_compose::Draft {
            from: postman_compose::Address::new("", &me),
            to: vec![postman_compose::Address::new("", &me)],
            subject: format!("{tag} {what}"),
            markdown: "Удалить другим клиентом.".into(),
            ..Default::default()
        };
        live.send(Command::Send { draft: Box::new(draft), replace: None, ticket: n, release: postman_core::model::Release::Now });
        live.wait("Sent", |e| matches!(e, Event::Sent { ticket, .. } if *ticket == n));
    }
    let soft = live.arrived(&format!("{tag} soft"));
    let hard = live.arrived(&format!("{tag} hard"));
    while live.events.try_recv().is_ok() {}

    // Второй клиент — `crates/ews` напрямую.
    rt().block_on(async {
        let mut c = Client::new(&ews_url(&a).await, postman_backend_ews::auth(&a.user, &a.password)).unwrap();
        for (row, mode) in [(&soft, DeleteMode::MoveToDeletedItems), (&hard, DeleteMode::HardDelete)] {
            let id = postman_backend_ews::sync::parse_key(&row.id).expect("ключ EWS");
            c.delete_mail(std::slice::from_ref(&id), mode).await.unwrap();
        }
    });
    // До опроса: открыть перенесённое — оно уходит из кэша, ошибки нет (`wait` падает на `Event::Error`).
    live.send(Command::Body(soft.id.clone()));
    live.wait("MailboxesChanged", |e| matches!(e, Event::MailboxesChanged));
    assert!(live.cache.rows_by_ids(std::slice::from_ref(&soft.id)).unwrap().is_empty(), "открытое исчезнувшее — из кэша");
    let start = Instant::now();
    loop {
        let here: Vec<String> =
            live.cache.page(&live.inbox(), 0, 500).unwrap().into_iter().filter(|r| r.subject.contains(&tag)).map(|r| r.subject).collect();
        if here.is_empty() {
            break;
        }
        assert!(start.elapsed() < WAIT, "удалённое другим клиентом осталось во «Входящих»: {}", here.len());
        live.send(Command::Reconnect);
        std::thread::sleep(Duration::from_secs(5));
        while live.events.try_recv().is_ok() {}
    }
    println!("удалено другим клиентом: ушло из кэша за {} с", start.elapsed().as_secs());
    rt().block_on(cleanup(&a));
}

/// Изменения по событиям (T1-21б): письмо себе появляется во «Входящих» кэша без «снова на связи» и раньше опроса
/// (`POLL`, 2 мин) — по подписке.
#[test]
#[ignore = "боевой Exchange: S7 «Проверка на боевом Exchange»"]
fn exchange_events_bring_mail() {
    let a = first();
    rt().block_on(cleanup(&a));
    let live = Live::start(&a);
    // Подписка открывается рядом с первой сверкой.
    std::thread::sleep(Duration::from_secs(3));
    let tag = tag();
    let me = test_recipient(&a.email);
    let draft = postman_compose::Draft {
        from: postman_compose::Address::new("", &me),
        to: vec![postman_compose::Address::new("", &me)],
        subject: format!("{tag} events"),
        markdown: "Письмо по событию сервера.".into(),
        ..Default::default()
    };
    live.send(Command::Send { draft: Box::new(draft), replace: None, ticket: 1, release: postman_core::model::Release::Now });
    live.wait("Sent", |e| matches!(e, Event::Sent { ticket: 1, .. }));
    let start = Instant::now();
    loop {
        if live.cache.page(&live.inbox(), 0, 200).unwrap().iter().any(|r| r.subject.contains(&tag)) {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(90), "письмо не пришло событием за 90 с (опрос — 120 с)");
        let _ = live.events.recv_timeout(Duration::from_secs(1));
    }
    println!("событие: письмо в кэше через {} с", start.elapsed().as_secs());
    rt().block_on(cleanup(&a));
}

/// «Мои контакты» через синхронизацию (T1-25в): создать → в списке → изменить → удалить; справочник — поиском с
/// подсказкой (страница пустая, `search_only`), GAL целиком не читается. Имя — с меткой, уборка — по ней.
#[test]
#[ignore = "боевой Exchange: S7 «Проверка на боевом Exchange»"]
fn exchange_contacts_through_sync() {
    use postman_core::model::ContactCard;
    let a = first();
    rt().block_on(cleanup(&a));
    let live = Live::start(&a);
    let tag = tag();
    let loaded = |live: &Live| {
        let Event::ContactsLoaded(cards) = live.wait("ContactsLoaded", |e| matches!(e, Event::ContactsLoaded(_))) else { unreachable!() };
        cards
    };
    let card = ContactCard {
        name: format!("{tag} contact"),
        emails: vec!["postman-test@example.test".into()],
        phones: vec!["+7 000 000-00-01".into()],
        organization: "Postman".into(),
        title: "Тестовый".into(),
        ..Default::default()
    };
    live.send(Command::SaveContact { card: Box::new(card.clone()), old: None, ticket: 1 });
    let Event::ContactSaved { id, .. } =
        live.wait("ContactSaved", |e| matches!(e, Event::ContactSaved { ticket: 1, .. } | Event::ContactFailed { .. }))
    else {
        panic!("контакт не создан")
    };
    let mine = loaded(&live).into_iter().find(|c| c.id == id).expect("новый — в списке");
    assert_eq!((mine.name.as_str(), mine.emails.as_slice(), mine.title.as_str()), (card.name.as_str(), card.emails.as_slice(), "Тестовый"));

    let edited = ContactCard { title: "Изменённый".into(), phones: Vec::new(), ..mine.clone() };
    live.send(Command::SaveContact { card: Box::new(edited), old: Some(Box::new(mine)), ticket: 2 });
    live.wait("ContactSaved", |e| matches!(e, Event::ContactSaved { ticket: 2, .. }));
    let after = loaded(&live).into_iter().find(|c| c.id == id).expect("после правки");
    assert_eq!((after.title.as_str(), after.phones.len()), ("Изменённый", 0));

    live.send(Command::DeleteContact { id: id.clone(), ticket: 3 });
    live.wait("ContactDeleted", |e| matches!(e, Event::ContactDeleted { ticket: 3, .. }));
    assert!(!loaded(&live).iter().any(|c| c.id == id), "удалён");

    live.send(Command::DirectoryPage { account: None, offset: 0, max: 20 });
    let Event::DirectoryPage { people, search_only, .. } = live.wait("DirectoryPage", |e| matches!(e, Event::DirectoryPage { .. })) else {
        unreachable!()
    };
    assert!(search_only && people.len() <= 20, "справочник — поиском");
    rt().block_on(cleanup(&a));
    println!("контакт: создан, изменён, удалён; справочник — поиском");
}
