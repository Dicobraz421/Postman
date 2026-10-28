//! Клиент Postman против живого Stalwart (стенд, `docs/STALWART.md`): сессия, почта, push, приглашения,
//! занятость, контакты — тем же кодом, что и приложение. Без стенда тесты пропускаются (`#[ignore]`).
//!
//! Запуск (пароли — только в окружении, на диск не пишем):
//!   POSTMAN_STAND_URL=http://localhost:18080 POSTMAN_STAND_USER1=user1@itt.test POSTMAN_STAND_PASSWORD1=…
//!   POSTMAN_STAND_USER2=user2@itt.test POSTMAN_STAND_PASSWORD2=… cargo test -p postman-backend-jmap --test stand -- --ignored

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)

use std::future::Future;
use std::time::{Duration, Instant};

use postman_calendar::{Event, NewEvent, Partstat, participant_id};
use postman_compose::{Address, Draft};
use postman_jmap::{Auth, Client, Envelope};
use serde_json::Value;

/// Сколько ждать, пока сервер доставит письмо или приглашение.
const DELIVERY: Duration = Duration::from_secs(30);

struct User {
    email: String,
    client: Client,
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} не задан: стенд — docs/STALWART.md"))
}

async fn user(n: u8) -> User {
    let email = env(&format!("POSTMAN_STAND_USER{n}"));
    let password = env(&format!("POSTMAN_STAND_PASSWORD{n}"));
    let client = Client::connect(&env("POSTMAN_STAND_URL"), Auth::Basic { user: email.clone(), password })
        .await
        .unwrap_or_else(|e| panic!("вход {email}: {e}"));
    User { email, client }
}

/// Уникальная метка прогона: письма и события разных прогонов не путаются.
fn stamp() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    format!("{}-{}", t.as_secs(), t.subsec_nanos() % 1000)
}

/// Повторять `f`, пока не вернёт `Some`, не дольше `DELIVERY`.
async fn eventually<T, F, Fut>(what: &str, mut f: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let start = Instant::now();
    loop {
        if let Some(v) = f().await {
            return v;
        }
        assert!(start.elapsed() < DELIVERY, "не дождались за {DELIVERY:?}: {what}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Начало события через `days` дней, 14:00 по Москве (как в эталонной коллекции стенда).
fn start_in(days: i64) -> jiff::civil::DateTime {
    let today = jiff::Zoned::now().with_time_zone(jiff::tz::TimeZone::get("Europe/Moscow").unwrap()).date();
    today.checked_add(jiff::Span::new().days(days)).unwrap().at(14, 0, 0, 0)
}

async fn first_calendar(c: &Client) -> String {
    let mut cals = c.calendars().await.unwrap();
    cals.sort_by_key(|c| (!c.is_visible, c.sort_order));
    cals.first().expect("у пользователя нет календаря").id.clone()
}

#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn session_follows_connection_origin_and_has_capabilities() {
    let u = user(1).await;
    let s = u.client.session();
    let base = env("POSTMAN_STAND_URL");
    // Сервер объявляет свой внутренний адрес; клиент ходит туда же, куда подключился.
    for url in [&s.api_url, &s.download_url, &s.upload_url, &s.event_source_url] {
        assert!(url.starts_with(&base), "{url} не на {base}");
    }
    for cap in [postman_jmap::MAIL, postman_jmap::SUBMISSION, postman_jmap::CALENDARS, postman_jmap::CONTACTS] {
        assert!(u.client.has(cap), "нет {cap}");
    }
    assert!(u.client.has(postman_jmap::PRINCIPALS) && u.client.has(postman_jmap::PRINCIPALS_AVAILABILITY));
    assert!(u.client.has(postman_jmap::WEBSOCKET));
}

#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn mail_is_sent_received_searched_and_flagged() {
    let (a, b) = (user(1).await, user(2).await);
    let subject = format!("Postman stand {}", stamp());

    // Отправка как в приложении: письмо целиком → blob → «Черновики» → EmailSubmission → «Отправленные».
    let (boxes, _) = a.client.mailboxes().await.unwrap();
    let role = |r: &str| boxes.iter().find(|m| m.role.as_deref() == Some(r)).map(|m| m.id.clone()).unwrap();
    let identity = a.client.identities().await.unwrap().into_iter().find(|i| i.email == a.email).unwrap();
    let draft = Draft {
        from: Address::new("User 1", &a.email),
        to: vec![Address::new("User 2", &b.email)],
        subject: subject.clone(),
        markdown: "Привет со **стенда**.".into(),
        ..Default::default()
    };
    let now = jiff::Timestamp::now().as_second();
    let raw = postman_compose::build(&draft, now, &postman_compose::new_message_id(&draft.from, now)).unwrap();
    let envelope = Envelope { mail_from: a.email.clone(), rcpt_to: vec![b.email.clone()] };
    a.client.send(raw, &identity.id, &envelope, &role("drafts"), &role("sent")).await.unwrap();

    // Получатель находит его поиском во «Входящих».
    let (boxes, _) = b.client.mailboxes().await.unwrap();
    let inbox = boxes.iter().find(|m| m.role.as_deref() == Some("inbox")).unwrap().id.clone();
    let id = eventually("письмо у получателя", || async {
        let page = b.client.search(&subject, Some(&inbox), 5).await.ok()?;
        page.emails.into_iter().find(|e| e.subject.as_deref() == Some(subject.as_str())).map(|e| e.id)
    })
    .await;
    let body = b.client.email_body(&id).await.unwrap();
    assert!(body.text.is_some() || body.html.is_some(), "у письма нет тела");

    // Флаг «прочитано» и дельта изменений.
    let (_, state) = b.client.email_get(std::slice::from_ref(&id)).await.unwrap();
    b.client.set_keyword(&id, "$seen", true).await.unwrap();
    let changes = b.client.changes("Email", &state, 50).await.unwrap();
    assert!(changes.updated.contains(&id), "Email/changes не вернул изменённое письмо");
    b.client.destroy_email(&id).await.unwrap();
}

/// Удаление (S3-02): письмо — в «Удалённые» (`Email/set` mailboxIds), оттуда — насовсем (`destroy`).
/// Один пользователь пишет себе: прогон не трогает чужие ящики.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn mail_moves_to_trash_and_is_destroyed() {
    let u = user(2).await;
    let subject = format!("Postman trash {}", stamp());
    let (boxes, _) = u.client.mailboxes().await.unwrap();
    let role = |r: &str| boxes.iter().find(|m| m.role.as_deref() == Some(r)).map(|m| m.id.clone()).unwrap();
    let (inbox, trash) = (role("inbox"), role("trash"));
    let identity = u.client.identities().await.unwrap().into_iter().find(|i| i.email == u.email).unwrap();
    let draft = Draft {
        from: Address::new("User 2", &u.email),
        to: vec![Address::new("User 2", &u.email)],
        subject: subject.clone(),
        markdown: "В корзину.".into(),
        ..Default::default()
    };
    let now = jiff::Timestamp::now().as_second();
    let raw = postman_compose::build(&draft, now, &postman_compose::new_message_id(&draft.from, now)).unwrap();
    let envelope = Envelope { mail_from: u.email.clone(), rcpt_to: vec![u.email.clone()] };
    u.client.send(raw, &identity.id, &envelope, &role("drafts"), &role("sent")).await.unwrap();
    let id = eventually("письмо во «Входящих»", || async {
        let page = u.client.search(&subject, Some(&inbox), 5).await.ok()?;
        page.emails.into_iter().find(|e| e.subject.as_deref() == Some(subject.as_str())).map(|e| e.id)
    })
    .await;

    u.client.move_email(&id, &inbox, &trash).await.unwrap();
    let (emails, _) = u.client.email_get(std::slice::from_ref(&id)).await.unwrap();
    let boxes_of: Vec<&String> = emails[0].mailbox_ids.iter().filter(|(_, on)| **on).map(|(k, _)| k).collect();
    assert_eq!(boxes_of, [&trash], "письмо не перенесено в «Удалённые»");

    u.client.destroy_email(&id).await.unwrap();
    let (emails, _) = u.client.email_get(std::slice::from_ref(&id)).await.unwrap();
    assert!(emails.is_empty(), "письмо не удалено");
    // Копия в «Отправленных» — тоже прочь.
    let sent = u.client.search(&subject, Some(&role("sent")), 5).await.unwrap();
    for e in sent.emails {
        u.client.destroy_email(&e.id).await.unwrap();
    }
}

#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn push_delivers_state_change() {
    let (a, b) = (user(1).await, user(2).await);
    let mut events = b.client.event_source(5).await.unwrap();
    // Изменение в ящике получателя: письмо от user1.
    let (boxes, _) = a.client.mailboxes().await.unwrap();
    let role = |r: &str| boxes.iter().find(|m| m.role.as_deref() == Some(r)).map(|m| m.id.clone()).unwrap();
    let identity = a.client.identities().await.unwrap().into_iter().find(|i| i.email == a.email).unwrap();
    let draft = Draft {
        from: Address::new("", &a.email),
        to: vec![Address::new("", &b.email)],
        subject: format!("Postman push {}", stamp()),
        markdown: "push".into(),
        ..Default::default()
    };
    let now = jiff::Timestamp::now().as_second();
    let raw = postman_compose::build(&draft, now, &postman_compose::new_message_id(&draft.from, now)).unwrap();
    let envelope = Envelope { mail_from: a.email.clone(), rcpt_to: vec![b.email.clone()] };
    a.client.send(raw, &identity.id, &envelope, &role("drafts"), &role("sent")).await.unwrap();
    let got = tokio::time::timeout(DELIVERY, async {
        while let Some(ev) = events.next().await.unwrap() {
            if ev.event == "state" {
                return ev;
            }
        }
        panic!("сервер закрыл EventSource");
    })
    .await
    .expect("нет события state");
    assert!(got.data.contains("changed"), "{}", got.data);
}

/// Сценарий эталонной коллекции: user1 приглашает user2 → у user2 событие (своё id, ищем по uid) →
/// user2 принимает → у user1 статус accepted → user1 удаляет → у user2 отмена.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn invitation_round_trip() {
    let (a, b) = (user(1).await, user(2).await);
    let uid = format!("postman-stand-{}@itt.test", stamp());
    let cal = first_calendar(&a.client).await;
    let attendees = [("User 2", b.email.as_str(), false)];
    let ev = NewEvent {
        uid: &uid,
        calendar_id: &cal,
        title: "Postman: проверка приглашения",
        description: "",
        location: "Переговорная 1",
        start: start_in(8),
        time_zone: "Europe/Moscow",
        duration_secs: 3600,
        organizer: ("User 1", &a.email),
        attendees: &attendees,
    };
    let my_id = a.client.create_event(ev.to_jmap(), true).await.unwrap();

    let theirs = eventually("приглашение у участника", || async { b.client.event_by_uid(&uid).await.ok().flatten() }).await;
    // id — в своём аккаунте участника (может совпасть с id организатора по значению, но это другое событие).
    let their_id = theirs["id"].as_str().unwrap().to_string();
    let parsed = Event::from_jmap(&theirs, &jiff::tz::TimeZone::UTC).unwrap();
    assert_eq!(parsed.organizer().map(|o| o.email.as_str()), Some(a.email.as_str()));

    // Ответ — тем же путём, что «Принять» в приложении (`postman_backend_jmap::invite::via_server`).
    let pid = participant_id(&theirs, &b.email).expect("себя среди участников");
    b.client.reply(&their_id, &pid, postman_calendar::partstat_json(Partstat::Accepted)).await.unwrap();

    eventually("ответ у организатора", || async {
        let mine = a.client.event_by_uid(&uid).await.ok().flatten()?;
        let e = Event::from_jmap(&mine, &jiff::tz::TimeZone::UTC).ok()?;
        e.participants.iter().any(|p| p.email.eq_ignore_ascii_case(&b.email) && p.status == Partstat::Accepted).then_some(())
    })
    .await;

    a.client.destroy_event(&my_id, true).await.unwrap();
    eventually("отмена у участника", || async {
        match b.client.event_by_uid(&uid).await.ok()? {
            None => Some(()),
            Some(e) => (e["status"].as_str() == Some("cancelled")).then_some(()),
        }
    })
    .await;
    // Убрать отменённое событие у участника (без рассылки).
    if let Some(e) = b.client.event_by_uid(&uid).await.unwrap() {
        b.client.destroy_event(e["id"].as_str().unwrap(), false).await.unwrap();
    }
}

/// S5-11: метки события (`keywords`, `categories`) и номер версии (`sequence`) сервер хранит и отдаёт — по ним цвет
/// блока в неделе и состояние карточки приглашения («изменено», «устарело»). Справочник: какие типы Principal есть
/// (переговорные и ресурсы — строка «Переговорная» во вкладке встречи).
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn event_keywords_sequence_and_principal_kinds() {
    let a = user(1).await;
    let cal = first_calendar(&a.client).await;
    let uid = format!("postman-s511-{}@itt.test", stamp());
    let start = start_in(10);
    let ev = NewEvent {
        uid: &uid,
        calendar_id: &cal,
        title: "Postman: метки события",
        description: "",
        location: "",
        start,
        time_zone: "Europe/Moscow",
        duration_secs: 1800,
        organizer: ("User 1", &a.email),
        attendees: &[],
    };
    let mut v = ev.to_jmap();
    v["keywords"] = serde_json::json!({ "pm.cat.clients": true });
    v["categories"] = serde_json::json!({ "http://example.com/categories/team": true });
    let id = a.client.create_event(v, false).await.unwrap();
    let got = a.client.event_by_uid(&uid).await.unwrap().expect("событие по uid");
    let e = Event::from_jmap(&got, &jiff::tz::TimeZone::UTC).unwrap();
    eprintln!("keywords={:?} sequence={} raw.keywords={} raw.categories={}", e.keywords, e.sequence, got["keywords"], got["categories"]);
    assert!(e.keywords.iter().any(|k| k == "pm.cat.clients"), "keywords потеряны: {got}");
    a.client.destroy_event(&id, false).await.unwrap();

    // Номер версии: встреча с участником (user3), правка времени с рассылкой — у участника `sequence` больше.
    let c = user(3).await;
    let uid = format!("postman-s511-seq-{}@itt.test", stamp());
    let attendees = [("User 3", c.email.as_str(), false)];
    let ev = NewEvent { uid: &uid, attendees: &attendees, title: "Postman: версия встречи", ..ev };
    let id = a.client.create_event(ev.to_jmap(), true).await.unwrap();
    let first = eventually("приглашение у user3", || async { c.client.event_by_uid(&uid).await.ok().flatten() }).await;
    let mut patch = serde_json::Map::new();
    patch.insert("start".into(), Value::from(start.checked_add(jiff::Span::new().hours(1)).unwrap().to_string()));
    a.client.update_event(&id, patch, true).await.unwrap();
    let moved = eventually("перенос у user3", || async {
        let e = c.client.event_by_uid(&uid).await.ok().flatten()?;
        (e["start"] != first["start"]).then_some(e)
    })
    .await;
    let mine = a.client.event_by_uid(&uid).await.unwrap().expect("у организатора");
    eprintln!("sequence: organizer={} attendee before={} after={}", mine["sequence"], first["sequence"], moved["sequence"]);
    // Перенос с рассылкой — новая версия у участника (без `sequence` в ответе — 0).
    let seq = |e: &Value| e["sequence"].as_i64().unwrap_or(0);
    assert!(seq(&moved) > seq(&first), "sequence не вырос: {} → {}", first["sequence"], moved["sequence"]);
    let parsed = Event::from_jmap(&moved, &jiff::tz::TimeZone::UTC).unwrap();
    assert_eq!(parsed.sequence, seq(&moved));
    a.client.destroy_event(&id, true).await.unwrap();
    if let Some(e) = c.client.event_by_uid(&uid).await.unwrap() {
        c.client.destroy_event(e["id"].as_str().unwrap(), false).await.unwrap();
    }

    let people = a.client.directory(200).await.unwrap();
    let mut kinds: Vec<&str> = people.iter().map(|p| p.kind.as_str()).collect();
    kinds.sort_unstable();
    kinds.dedup();
    eprintln!("principal kinds: {kinds:?} ({} total)", people.len());
    assert!(kinds.contains(&"individual"));
}

/// «Помощник планирования»: занятость коллеги видна только после того, как он открыл календарь.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn free_busy_after_sharing() {
    let (a, c) = (user(1).await, user(3).await);
    let me = a.client.principal_by_email(&a.email).await.unwrap().expect("principal user1");
    let viewer = c.client.principal_by_email(&c.email).await.unwrap().expect("principal user3");
    let cal = first_calendar(&a.client).await;
    let uid = format!("postman-busy-{}@itt.test", stamp());
    let start = start_in(9);
    let ev = NewEvent {
        uid: &uid,
        calendar_id: &cal,
        title: "Postman: занятость",
        description: "",
        location: "",
        start,
        time_zone: "Europe/Moscow",
        duration_secs: 3600,
        organizer: ("User 1", &a.email),
        attendees: &[],
    };
    let id = a.client.create_event(ev.to_jmap(), false).await.unwrap();

    let day = |d: jiff::civil::DateTime| {
        d.to_zoned(jiff::tz::TimeZone::get("Europe/Moscow").unwrap()).unwrap().timestamp().strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
    };
    let (from, to) = (day(start.date().at(0, 0, 0, 0)), day(start.date().at(23, 0, 0, 0)));
    let busy_for = |client: Client| {
        let (pid, from, to) = (me.id.clone(), from.clone(), to.clone());
        async move { client.availability(&pid, &from, &to).await.unwrap() }
    };

    a.client.share_calendar(&cal, &viewer.id, false).await.unwrap();
    let busy = eventually("занятость после шаринга", || {
        let f = busy_for(c.client.clone());
        async move { Some(f.await).filter(|b| !b.is_empty()) }
    })
    .await;
    assert!(busy.iter().any(|p| p.utc_start.starts_with(&day(start)[..13])), "нет занятости 14:00 МСК: {busy:?}");
    a.client.destroy_event(&id, false).await.unwrap();
}

/// S5-22: календарь user2, открытый user1 (`shareWith`, `mayReadItems`), виден у user1 отдельным аккаунтом сессии
/// (после нового входа); его календари и события читаются (`Calendar/get`, `CalendarEvent/query` в этом аккаунте).
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn shared_calendar_is_visible() {
    let (a, b) = (user(1).await, user(2).await);
    let viewer = b.client.principal_by_email(&a.email).await.unwrap().expect("principal user1");
    let cal = first_calendar(&b.client).await;
    let uid = format!("postman-shared-{}@itt.test", stamp());
    let start = start_in(10);
    let ev = NewEvent {
        uid: &uid,
        calendar_id: &cal,
        title: "Postman: календарь коллеги",
        description: "",
        location: "",
        start,
        time_zone: "Europe/Moscow",
        duration_secs: 3600,
        organizer: ("User 2", &b.email),
        attendees: &[],
    };
    let id = b.client.create_event(ev.to_jmap(), false).await.unwrap();
    b.client.share_calendar(&cal, &viewer.id, true).await.unwrap();
    let day = |d: jiff::civil::DateTime| d.to_zoned(jiff::tz::TimeZone::UTC).unwrap().timestamp().strftime("%Y-%m-%dT%H:%M:%SZ").to_string();
    let (from, to) = (day(start.date().at(0, 0, 0, 0)), day(start.date().at(23, 0, 0, 0)));

    // Аккаунты сессии — на момент входа: входим заново.
    let found = eventually("календарь коллеги в сессии user1", || {
        let (from, to, uid) = (from.clone(), to.clone(), uid.clone());
        async move {
            let a = user(1).await;
            let shared = a.client.session().shared_calendar_accounts();
            eprintln!("shared calendar accounts: {shared:?}");
            for (acc, name) in shared {
                let cals = a.client.calendars_of(&acc).await.ok()?;
                let events = a.client.events_of(&acc, None, &from, &to).await.ok()?;
                if events.iter().any(|e| e["uid"].as_str() == Some(uid.as_str())) {
                    return Some((name, cals.len()));
                }
            }
            None
        }
    })
    .await;
    eprintln!("calendar of user2 at user1: account {:?}, calendars {}", found.0, found.1);

    b.client.unshare_calendar(&cal, &viewer.id).await.unwrap();
    b.client.destroy_event(&id, false).await.unwrap();
    drop(a);
}

#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn contacts_are_read() {
    let a = user(1).await;
    let stamp = stamp();
    let acc = a.client.account_for(postman_jmap::CONTACTS).unwrap().to_string();
    let using = [postman_jmap::CORE, postman_jmap::CONTACTS];
    let call = |name: &str, args: Value| postman_jmap::Invocation(name.into(), args, "c".into());
    let books = a.client.call(&using, &[call("AddressBook/get", serde_json::json!({ "accountId": acc, "ids": null }))]).await.unwrap();
    let book = books[0].1["list"][0]["id"].as_str().expect("адресная книга").to_string();
    let card = serde_json::json!({
        "@type": "Card", "version": "1.0", "addressBookIds": { book: true },
        "name": { "components": [{ "kind": "given", "value": "Иван" }, { "kind": "surname", "value": format!("Стендов{stamp}") }] },
        "emails": { "e1": { "address": format!("ivan.{stamp}@example.com") } }
    });
    let created = a.client.call(&using, &[call("ContactCard/set", serde_json::json!({ "accountId": acc, "create": { "n": card } }))]).await.unwrap();
    let id = created[0].1["created"]["n"]["id"].as_str().expect("карточка создана").to_string();

    let cards = a.client.contact_cards().await.unwrap();
    let c = cards.iter().find(|c| c.id == id).expect("карточка в списке");
    // Stalwart сам заполняет `name.full` по компонентам в их порядке (`isOrdered`): имя, фамилия.
    assert_eq!(c.name, format!("Иван Стендов{stamp}"));
    assert_eq!(c.emails, [format!("ivan.{stamp}@example.com")]);
    a.client.call(&using, &[call("ContactCard/set", serde_json::json!({ "accountId": acc, "destroy": [id] }))]).await.unwrap();
}

/// Контакты (S4-02) методами клиента, как в приложении: user3 создаёт карточку с телефоном, организацией и
/// должностью → правит её патчем (новый адрес, без телефона, другое имя) → удаляет; справочник (Principals)
/// отдаёт сотрудников стенда с адресами и ФИО (`description`; `name` у Stalwart — логин).
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn contacts_are_edited_and_directory_is_listed() {
    use postman_jmap::ContactCard;

    let c = user(3).await;
    let stamp = stamp();
    let new = ContactCard {
        id: String::new(),
        name: format!("Стендов{stamp} Пётр"),
        emails: vec![format!("petr.{stamp}@example.com")],
        phones: vec!["+7 495 000-00-01".into()],
        organization: "ООО Стенд".into(),
        title: "Инженер".into(),
    };
    let id = c.client.create_contact(&new).await.unwrap();
    let find = |cards: Vec<ContactCard>| cards.into_iter().find(|x| x.id == id);
    let got = find(c.client.contact_cards().await.unwrap()).expect("карточка в списке");
    // Имя — как задано (`name.full` без компонентов сервер не переписывает).
    assert_eq!(got, ContactCard { id: id.clone(), ..new.clone() });

    let edited = ContactCard {
        name: format!("Стендов{stamp} Пётр Иванович"),
        emails: vec![format!("petr.{stamp}@example.com"), format!("p.{stamp}@home.example")],
        phones: Vec::new(),
        title: "Ведущий инженер".into(),
        ..got.clone()
    };
    c.client.update_contact(&got, &edited).await.unwrap();
    let got = find(c.client.contact_cards().await.unwrap()).expect("карточка после правки");
    assert_eq!(got, edited);

    c.client.destroy_contact(&id).await.unwrap();
    assert!(find(c.client.contact_cards().await.unwrap()).is_none(), "карточка не удалена");

    let people = c.client.directory(1000).await.unwrap();
    for n in 1..=3 {
        let email = env(&format!("POSTMAN_STAND_USER{n}"));
        let p = people
            .iter()
            .find(|p| p.email.as_deref().is_some_and(|e| e.eq_ignore_ascii_case(&email)))
            .unwrap_or_else(|| panic!("{email} нет в справочнике: {people:?}"));
        assert_eq!(p.kind, "individual", "{p:?}");
        assert!(!p.display_name().is_empty() && !p.display_name().eq_ignore_ascii_case(&email), "имя — не ФИО: {p:?}");
    }
}

/// Контакты (S4-02) через поток синхронизации, как раздел «Контакты»: после входа — `ContactsLoaded` и
/// `DirectoryLoaded`; `SaveContact` (новая, затем правка) и `DeleteContact` — итог и свежий список. user3.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn contacts_through_sync() {
    use postman_core::model::ContactCard;
    use postman_core::sync::{Account, Command, Event as SyncEvent};

    let email = env("POSTMAN_STAND_USER3");
    let account = Account {
        url: env("POSTMAN_STAND_URL"),
        auth: postman_core::model::Auth::Basic { user: email.clone(), password: env("POSTMAN_STAND_PASSWORD3") },
    };
    let cache = postman_cache::Cache::open_in_memory().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let commands = postman_backend_jmap::spawn(account, cache, None, tx, || {});
    let wait = |what: &str, pick: &dyn Fn(&SyncEvent) -> bool| -> SyncEvent {
        let start = Instant::now();
        loop {
            let left = DELIVERY.checked_sub(start.elapsed()).unwrap_or_else(|| panic!("не дождались: {what}"));
            match rx.recv_timeout(left) {
                Ok(SyncEvent::Error(e)) => panic!("синхронизация: {e}"),
                Ok(SyncEvent::ContactFailed { error, .. }) => panic!("контакты: {error}"),
                Ok(ev) if pick(&ev) => return ev,
                Ok(_) => {}
                Err(e) => panic!("не дождались: {what} ({e})"),
            }
        }
    };
    let SyncEvent::DirectoryLoaded(people) = wait("DirectoryLoaded", &|e| matches!(e, SyncEvent::DirectoryLoaded(_))) else { unreachable!() };
    assert!(people.iter().any(|p| p.email.as_deref().is_some_and(|e| e.eq_ignore_ascii_case(&email))), "{people:?}");

    let stamp = stamp();
    let card =
        ContactCard { name: format!("Синхронов{stamp} Иван"), emails: vec![format!("ivan.{stamp}@example.com")], ..Default::default() };
    // Список с карточкой `id` и `phones` телефонами; `None` — без неё.
    let listed = |e: &SyncEvent, id: &str, phones: Option<usize>| match (e, phones) {
        (SyncEvent::ContactsLoaded(cards), Some(n)) => cards.iter().any(|c| c.id == id && c.phones.len() == n),
        (SyncEvent::ContactsLoaded(cards), None) => cards.iter().all(|c| c.id != id),
        _ => false,
    };
    assert!(commands.send(Command::SaveContact { card: Box::new(card.clone()), old: None, ticket: 1 }).is_ok());
    let SyncEvent::ContactSaved { ticket: 1, id } = wait("ContactSaved", &|e| matches!(e, SyncEvent::ContactSaved { .. })) else {
        panic!("не тот тикет")
    };
    wait("новая карточка в списке", &|e| listed(e, &id, Some(0)));

    let old = ContactCard { id: id.clone(), ..card };
    let edited = ContactCard { phones: vec!["+7 495 000-00-02".into()], ..old.clone() };
    let cmd = Command::SaveContact { card: Box::new(edited), old: Some(Box::new(old)), ticket: 2 };
    assert!(commands.send(cmd).is_ok());
    wait("ContactSaved 2", &|e| matches!(e, SyncEvent::ContactSaved { ticket: 2, .. }));
    wait("правка в списке", &|e| listed(e, &id, Some(1)));

    assert!(commands.send(Command::DeleteContact { id: id.clone(), ticket: 3 }).is_ok());
    wait("ContactDeleted", &|e| matches!(e, SyncEvent::ContactDeleted { ticket: 3, .. }));
    wait("карточки нет в списке", &|e| listed(e, &id, None));
}

/// Календарь на сервере (S3-05) через поток синхронизации, как в приложении: user3 открывает user1 свою
/// занятость и занят в 14:00 → user1 командой `CreateMeeting` приглашает user3 на 16:00 → у user3 событие по uid →
/// `Events` отдаёт встречу в неделе user1 → `Availability` показывает занятость user3. Затем всё удаляется.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn calendar_through_sync() {
    use postman_backend_jmap::schedule::{self, Meeting};
    use postman_core::sync::{Account, Command, Event as SyncEvent};

    let (a, c) = (user(1).await, user(3).await);
    let tz = "Europe/Moscow";
    let busy_start = start_in(10);
    let meeting_start = busy_start.date().at(16, 0, 0, 0);
    let stamp = stamp();

    // user3: открыть занятость user1 и занять 14:00–15:00.
    schedule::share_free_busy(&c.client, &a.email).await.unwrap();
    let busy = Meeting {
        uid: format!("postman-s305-busy-{stamp}@itt.test"),
        title: "Postman: занят".into(),
        location: String::new(),
        description: String::new(),
        start: busy_start,
        time_zone: tz.into(),
        duration_secs: 3600,
        attendees: Vec::new(),
        optional: Vec::new(),
        from: None,
    };
    let identities3 = c.client.identities().await.unwrap();
    let busy_id = schedule::create(&c.client, &identities3, &busy).await.unwrap();

    // user1 — поток синхронизации приложения.
    let account = Account {
        url: env("POSTMAN_STAND_URL"),
        auth: postman_core::model::Auth::Basic { user: a.email.clone(), password: env("POSTMAN_STAND_PASSWORD1") },
    };
    let cache = postman_cache::Cache::open_in_memory().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let commands = postman_backend_jmap::spawn(account, cache, None, tx, || {});
    let wait = |what: &str, pick: &dyn Fn(&SyncEvent) -> bool| -> SyncEvent {
        let start = Instant::now();
        loop {
            let left = DELIVERY.checked_sub(start.elapsed()).unwrap_or_else(|| panic!("не дождались: {what}"));
            match rx.recv_timeout(left) {
                Ok(SyncEvent::Error(e)) => panic!("синхронизация: {e}"),
                Ok(ev) if pick(&ev) => return ev,
                Ok(_) => {}
                Err(e) => panic!("не дождались: {what} ({e})"),
            }
        }
    };
    let uid = format!("postman-s305-{stamp}@itt.test");
    let meeting = Meeting {
        uid: uid.clone(),
        title: "Postman: встреча S3-05".into(),
        location: "Переговорная 1".into(),
        description: String::new(),
        start: meeting_start,
        time_zone: tz.into(),
        duration_secs: 1800,
        attendees: vec![("User 3".into(), c.email.clone())],
        optional: Vec::new(),
        from: None,
    };
    assert!(commands.send(Command::CreateMeeting { meeting: Box::new(meeting), ticket: 1 }).is_ok());
    match wait("MeetingCreated", &|e| matches!(e, SyncEvent::MeetingCreated { .. } | SyncEvent::MeetingFailed { .. })) {
        SyncEvent::MeetingCreated { ticket, uid: got } => assert_eq!((ticket, got.as_str()), (1, uid.as_str())),
        SyncEvent::MeetingFailed { error, .. } => panic!("встреча не создана: {error}"),
        _ => unreachable!(),
    }
    eventually("приглашение у user3", || async { c.client.event_by_uid(&uid).await.ok().flatten() }).await;

    let zoned = |d: jiff::civil::DateTime| d.to_zoned(jiff::tz::TimeZone::get(tz).unwrap()).unwrap().timestamp().as_second();
    let (from, to) = (zoned(busy_start.date().at(0, 0, 0, 0)), zoned(busy_start.date().at(23, 59, 0, 0)));
    assert!(commands.send(Command::Events { from, to }).is_ok());
    let SyncEvent::EventsLoaded { events, .. } = wait("EventsLoaded", &|e| matches!(e, SyncEvent::EventsLoaded { .. })) else { unreachable!() };
    let mine = events.iter().find(|e| e.uid == uid).expect("встреча в неделе user1");
    assert_eq!((mine.span.start, mine.span.len()), (zoned(meeting_start), 1800));
    assert!(mine.participants.iter().any(|p| p.email.eq_ignore_ascii_case(&c.email)));

    assert!(commands.send(Command::Availability { ticket: 2, emails: vec![c.email.clone()], from, to, sender: None }).is_ok());
    let SyncEvent::Availability { me, people, error, .. } = wait("Availability", &|e| matches!(e, SyncEvent::Availability { .. })) else {
        unreachable!()
    };
    assert_eq!(error, None);
    assert!(me.iter().any(|b| b.span.start == zoned(meeting_start)), "моя занятость без встречи: {me:?}");
    let theirs = people[0].1.as_ref().expect("занятость user3 после шаринга");
    assert!(theirs.iter().any(|b| b.span.start == zoned(busy_start)), "нет занятости user3 в 14:00: {theirs:?}");
    drop(commands);

    // Уборка: встреча с отменой, копия у user3, занятость user3.
    let my = a.client.event_by_uid(&uid).await.unwrap().expect("встреча у user1");
    a.client.destroy_event(my["id"].as_str().unwrap(), true).await.unwrap();
    eventually("отмена у user3", || async {
        match c.client.event_by_uid(&uid).await.ok()? {
            None => Some(()),
            Some(e) => (e["status"].as_str() == Some("cancelled")).then_some(()),
        }
    })
    .await;
    if let Some(e) = c.client.event_by_uid(&uid).await.unwrap() {
        c.client.destroy_event(e["id"].as_str().unwrap(), false).await.unwrap();
    }
    c.client.destroy_event(&busy_id, false).await.unwrap();
}

type SyncEvents = std::sync::mpsc::Receiver<postman_core::sync::Event>;

/// Поток синхронизации user`n` на кэше в памяти: команды и события.
fn sync_of(n: u8, password: String) -> (postman_core::sync::UnboundedSender<postman_core::sync::Command>, SyncEvents) {
    use postman_core::sync::Account;
    let user = env(&format!("POSTMAN_STAND_USER{n}"));
    let account = Account { url: env("POSTMAN_STAND_URL"), auth: postman_core::model::Auth::Basic { user, password } };
    let (tx, rx) = std::sync::mpsc::channel();
    let commands = postman_backend_jmap::spawn(account, postman_cache::Cache::open_in_memory().unwrap(), None, tx, || {});
    (commands, rx)
}

/// Ждать событие синхронизации, на котором `pick` возвращает `Some`; ошибка синхронизации — провал.
fn wait_for<T>(rx: &SyncEvents, what: &str, pick: impl Fn(&postman_core::sync::Event) -> Option<T>) -> T {
    use postman_core::sync::Event as SyncEvent;
    let start = Instant::now();
    loop {
        let left = DELIVERY.checked_sub(start.elapsed()).unwrap_or_else(|| panic!("не дождались: {what}"));
        match rx.recv_timeout(left) {
            Ok(ev) => {
                if let Some(v) = pick(&ev) {
                    return v;
                }
                if let SyncEvent::Error(e) = ev {
                    panic!("синхронизация ({what}): {e}");
                }
            }
            Err(e) => panic!("не дождались: {what} ({e})"),
        }
    }
}

/// S4-03: неверный пароль — ошибка рода `Auth` (HTTP 401), синхронизация останавливается сама (канал событий
/// закрывается), без повторных попыток. Одна попытка входа: сервер блокирует адрес за перебор паролей.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn rejected_password_stops_sync_with_auth_error() {
    use postman_core::sync::{ErrorKind, Event as SyncEvent, Stage};
    let (_commands, rx) = sync_of(3, "not-the-password".into());
    let err = wait_for(&rx, "ошибка входа", |e| match e {
        SyncEvent::Error(e) => Some(e.clone()),
        _ => None,
    });
    assert_eq!((err.kind, err.stage), (ErrorKind::Auth, Stage::Connect), "{err}");
    // Поток закончился: больше событий нет, отправитель закрыт.
    loop {
        match rx.recv_timeout(DELIVERY) {
            Ok(SyncEvent::Status(_)) => {}
            Ok(SyncEvent::Error(e)) => panic!("вторая ошибка после отказа во входе: {e}"),
            Ok(_) => panic!("событие после отказа во входе"),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(e) => panic!("синхронизация не остановилась: {e}"),
        }
    }
}

/// S4-03: догрузка папки — `LoadMore` после первой страницы (500) отдаёт `LoadedMore` с числом пришедших писем:
/// следующая страница, если на сервере писем больше, иначе 0.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn load_more_through_sync() {
    use postman_core::sync::{Command, Event as SyncEvent, Status};
    let u = user(1).await;
    let (boxes, _) = u.client.mailboxes().await.unwrap();
    let inbox = boxes.iter().find(|m| m.role.as_deref() == Some("inbox")).unwrap();
    let (commands, rx) = sync_of(1, env("POSTMAN_STAND_PASSWORD1"));
    wait_for(&rx, "первая синхронизация", |e| matches!(e, SyncEvent::Status(Status::Synced)).then_some(()));
    assert!(commands.send(Command::LoadMore { mailbox: inbox.id.clone() }).is_ok());
    let (mailbox, added) = wait_for(&rx, "LoadedMore", |e| match e {
        SyncEvent::LoadedMore { mailbox, added } => Some((mailbox.clone(), *added)),
        _ => None,
    });
    let total = inbox.total_emails as usize;
    assert_eq!((mailbox.as_str(), added), (inbox.id.as_str(), total.saturating_sub(500).min(500)));
}

/// S7-06г: поиск людей справочника через синхронизацию — `FindPeople` → `Principal/query` с фильтром `text` → `PeopleFound`;
/// другого пользователя стенда находит по началу его адреса. Только чтение.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn find_people_through_sync() {
    use postman_core::sync::{Command, Event as SyncEvent, Status};
    let (commands, rx) = sync_of(1, env("POSTMAN_STAND_PASSWORD1"));
    wait_for(&rx, "первая синхронизация", |e| matches!(e, SyncEvent::Status(Status::Synced)).then_some(()));
    let other = env("POSTMAN_STAND_USER2");
    let query = other.split('@').next().unwrap().to_owned();
    assert!(commands.send(Command::FindPeople { text: query.clone(), offset: 0 }).is_ok());
    let people = wait_for(&rx, "PeopleFound", |e| match e {
        SyncEvent::PeopleFound { text, people, .. } if *text == query => Some(people.clone()),
        _ => None,
    });
    assert!(people.iter().any(|p| p.email.as_deref().is_some_and(|e| e.eq_ignore_ascii_case(&other))), "найдено: {}", people.len());
    // Короткая строка — без запроса и пусто.
    assert!(commands.send(Command::FindPeople { text: "us".into(), offset: 0 }).is_ok());
    let short = wait_for(&rx, "PeopleFound (коротко)", |e| match e {
        SyncEvent::PeopleFound { text, people, .. } if text == "us" => Some(people.len()),
        _ => None,
    });
    assert_eq!(short, 0);
}

/// S4-03: изменение календаря на сервере (событие создано другим клиентом) приходит push и после задержки на
/// пачку — одним `CalendarChanged`; письма при этом не перечитываются (`EmailsChanged` нет).
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn calendar_change_arrives_by_push() {
    use postman_backend_jmap::schedule::{self, Meeting};
    use postman_core::sync::{Event as SyncEvent, Status};
    let u = user(2).await;
    let (_commands, rx) = sync_of(2, env("POSTMAN_STAND_PASSWORD2"));
    wait_for(&rx, "первая синхронизация", |e| matches!(e, SyncEvent::Status(Status::Synced)).then_some(()));
    // Сверка после подключения push — дать ей пройти.
    tokio::time::sleep(Duration::from_secs(3)).await;
    while rx.try_recv().is_ok() {}
    let meeting = Meeting {
        uid: format!("postman-s403-{}@itt.test", stamp()),
        title: "Postman: push календаря".into(),
        location: String::new(),
        description: String::new(),
        start: start_in(12),
        time_zone: "Europe/Moscow".into(),
        duration_secs: 1800,
        attendees: Vec::new(),
        optional: Vec::new(),
        from: None,
    };
    let identities = u.client.identities().await.unwrap();
    let id = schedule::create(&u.client, &identities, &meeting).await.unwrap();
    let started = Instant::now();
    wait_for(&rx, "CalendarChanged", |e| matches!(e, SyncEvent::CalendarChanged).then_some(()));
    let elapsed = started.elapsed();
    let mut mail = false;
    while let Ok(ev) = rx.try_recv() {
        mail |= matches!(ev, SyncEvent::EmailsChanged);
    }
    u.client.destroy_event(&id, false).await.unwrap();
    assert!(!mail, "календарное изменение перечитало письма");
    println!("calendar push → CalendarChanged in {elapsed:?}");
}

/// Поиск в календаре (S5-21): `CalendarEvent/query` с `filter.text` находит событие по слову темы и по месту, чужое
/// слово — не находит. Событие — своё, без участников; затем удаляется. Находки — `docs/STALWART.md`.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn stand_calendar_search() {
    use postman_backend_jmap::schedule::{self, Meeting};

    let a = user(1).await;
    let stamp = stamp().replace('-', "");
    let word = format!("поисксобытия{stamp}");
    let place = format!("Переговорная{stamp}");
    let start = start_in(12);
    let meeting = Meeting {
        uid: format!("postman-s521-search-{stamp}@itt.test"),
        title: format!("Postman: {word}"),
        location: place.clone(),
        description: String::new(),
        start,
        time_zone: "Europe/Moscow".into(),
        duration_secs: 1800,
        attendees: Vec::new(),
        optional: Vec::new(),
        from: None,
    };
    let identities = a.client.identities().await.unwrap();
    let id = schedule::create(&a.client, &identities, &meeting).await.unwrap();
    let tz = jiff::tz::TimeZone::get("Europe/Moscow").unwrap();
    let at = start.to_zoned(tz).unwrap().timestamp().as_second();
    let (from, to) = (at - 30 * 86_400, at + 30 * 86_400);
    let by_title = eventually("поиск по слову темы", || async {
        let found = schedule::search(&a.client, &word, from, to).await.unwrap();
        (!found.is_empty()).then_some(found)
    })
    .await;
    assert!(by_title.iter().any(|e| e.uid == meeting.uid), "по теме: {by_title:?}");
    let by_place = schedule::search(&a.client, &place, from, to).await.unwrap();
    eprintln!("поиск по месту: {} событий", by_place.len());
    let none = schedule::search(&a.client, &format!("нетакогослова{stamp}"), from, to).await.unwrap();
    assert!(none.iter().all(|e| e.uid != meeting.uid), "чужое слово нашло событие");
    a.client.destroy_event(&id, false).await.unwrap();
}

/// Правка и отмена своей встречи через синхронизацию (S7-07б): user1 приглашает user3 → `UpdateMeeting` (на час позже,
/// новая тема) → у user3 новое время и тема → `CancelMeeting` → у user3 отмена. Серия (каждый день, 3 раза) с user3:
/// второй экземпляр — на час позже (`series: false`, исключение), третий — отменён (`EXDATE`), затем вся серия отменена.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn meeting_edit_and_cancel_through_sync() {
    use postman_backend_jmap::schedule::Meeting;
    use postman_calendar::Event as CalEvent;
    use postman_core::sync::{Command, Event as SyncEvent};

    let (a, c) = (user(1).await, user(3).await);
    let tz = "Europe/Moscow";
    let zone = jiff::tz::TimeZone::get(tz).unwrap();
    let zoned = |d: jiff::civil::DateTime| d.to_zoned(zone.clone()).unwrap().timestamp().as_second();
    let local = |ts: i64| jiff::Timestamp::from_second(ts).unwrap().to_zoned(zone.clone()).datetime();
    let stamp = stamp();
    let (commands, rx) = sync_of(1, env("POSTMAN_STAND_PASSWORD1"));
    let start = start_in(12).date().at(11, 0, 0, 0);
    let (from, to) = (zoned(start.date().at(0, 0, 0, 0)), zoned(start.date().at(0, 0, 0, 0)) + 4 * 86_400);
    let week = |what: &str| -> Vec<CalEvent> {
        assert!(commands.send(Command::Events { from, to }).is_ok());
        wait_for(&rx, what, |e| match e {
            SyncEvent::EventsLoaded { events, .. } => Some(events.clone()),
            _ => None,
        })
    };
    let done = |what: &str, ticket: u64| {
        wait_for(&rx, what, |e| match e {
            SyncEvent::MeetingUpdated { ticket: t, .. } | SyncEvent::MeetingCancelled { ticket: t, .. } if *t == ticket => Some(()),
            SyncEvent::MeetingFailed { ticket: t, error } if *t == ticket => panic!("{what}: {error}"),
            _ => None,
        })
    };
    let theirs = |uid: String| {
        let c = &c.client;
        async move { c.event_by_uid(&uid).await.ok().flatten().and_then(|v| CalEvent::from_jmap(&v, &jiff::tz::TimeZone::UTC).ok()) }
    };
    // Забытое прошлыми прогонами (упавший тест) — убрать у обоих: события с UID `postman-s707b-` в окне.
    for u in [&a, &c] {
        let utc = |t: i64| jiff::Timestamp::from_second(t).unwrap().strftime("%Y-%m-%dT%H:%M:%SZ").to_string();
        let mut uids: Vec<String> = u
            .client
            .events(None, &utc(from), &utc(to))
            .await
            .unwrap()
            .iter()
            .filter_map(|v| v["uid"].as_str().map(str::to_owned))
            .filter(|x| x.starts_with("postman-s707b-"))
            .collect();
        uids.dedup();
        for x in uids {
            if let Some(e) = u.client.event_by_uid(&x).await.unwrap() {
                u.client.destroy_event(e["id"].as_str().unwrap(), false).await.unwrap();
            }
        }
    }
    let meeting = |uid: &str, title: &str, at: jiff::civil::DateTime| Meeting {
        uid: uid.into(),
        title: title.into(),
        location: "Переговорная 2".into(),
        description: String::new(),
        start: at,
        time_zone: tz.into(),
        duration_secs: 1800,
        attendees: vec![("User 3".into(), c.email.clone())],
        optional: Vec::new(),
        from: None,
    };

    // Одиночная встреча: создать, перенести на час, отменить.
    let uid = format!("postman-s707b-{stamp}@itt.test");
    assert!(commands.send(Command::CreateMeeting { meeting: Box::new(meeting(&uid, "Postman: правка", start)), ticket: 1 }).is_ok());
    wait_for(&rx, "MeetingCreated", |e| matches!(e, SyncEvent::MeetingCreated { ticket: 1, .. }).then_some(()));
    eventually("приглашение у user3", || theirs(uid.clone())).await;
    let ev = week("неделя").into_iter().find(|e| e.uid == uid).expect("встреча в неделе");
    let moved = meeting(&uid, "Postman: правка, позже", local(ev.span.start + 3600));
    assert!(commands.send(Command::UpdateMeeting { event: Box::new(ev.clone()), meeting: Box::new(moved), series: false, ticket: 2 }).is_ok());
    done("MeetingUpdated", 2);
    eventually("перенос у user3", || {
        let f = theirs(uid.clone());
        let want = ev.span.start + 3600;
        async move { f.await.filter(|e| e.span.start == want && e.title == "Postman: правка, позже") }
    })
    .await;
    let ev = week("неделя после правки").into_iter().find(|e| e.uid == uid).expect("встреча после правки");
    assert_eq!(ev.span.start, zoned(start) + 3600);
    assert!(commands.send(Command::CancelMeeting { event: Box::new(ev), series: false, from: None, ticket: 3 }).is_ok());
    done("MeetingCancelled", 3);
    eventually("отмена у user3", || {
        let f = theirs(uid.clone());
        async move {
            match f.await {
                None => Some(()),
                Some(e) => (e.status == postman_calendar::EventStatus::Cancelled).then_some(()),
            }
        }
    })
    .await;
    assert!(!week("неделя после отмены").iter().any(|e| e.uid == uid));

    // Серия: экземпляр позже, экземпляр отменён, затем вся серия.
    let suid = format!("postman-s707b-series-{stamp}@itt.test");
    let identities = a.client.identities().await.unwrap();
    let me = identities.first().expect("Identity user1");
    let mut series = meeting(&suid, "Postman: серия", start).to_jmap(&first_calendar(&a.client).await, (&me.name, &me.email));
    // Stalwart — JSCalendar 2.0: одно правило `recurrenceRule` (`recurrenceRules` — `invalidProperties`).
    series["recurrenceRule"] = serde_json::json!({ "frequency": "daily", "count": 3 });
    a.client.create_event(series, true).await.unwrap();
    let ours = |w: Vec<CalEvent>| w.into_iter().filter(|e| e.uid == suid).collect::<Vec<_>>();
    let all = ours(week("серия"));
    assert_eq!(all.len(), 3, "три экземпляра: {all:?}");
    let second = all[1].clone();
    let later = meeting(&suid, "Postman: серия", local(second.span.start + 3600));
    assert!(commands.send(Command::UpdateMeeting { event: Box::new(second.clone()), meeting: Box::new(later), series: false, ticket: 4 }).is_ok());
    done("MeetingUpdated экземпляра", 4);
    let all = ours(week("серия после правки экземпляра"));
    assert_eq!(all.len(), 3);
    assert!(all.iter().any(|e| e.span.start == second.span.start + 3600), "экземпляр не перенесён: {all:?}");
    assert!(all.iter().any(|e| e.span.start == zoned(start)), "первый — на месте");
    let third = all.iter().max_by_key(|e| e.span.start).unwrap().clone();
    assert!(commands.send(Command::CancelMeeting { event: Box::new(third), series: false, from: None, ticket: 5 }).is_ok());
    done("MeetingCancelled экземпляра", 5);
    let all = ours(week("серия без экземпляра"));
    assert_eq!(all.len(), 2, "отменённый экземпляр остался: {all:?}");
    assert!(commands.send(Command::CancelMeeting { event: Box::new(all[0].clone()), series: true, from: None, ticket: 6 }).is_ok());
    done("MeetingCancelled серии", 6);
    assert!(ours(week("серия отменена")).is_empty());
    drop(commands);

    // Уборка: копии у user3.
    for u in [&uid, &suid] {
        if let Some(e) = c.client.event_by_uid(u).await.unwrap() {
            c.client.destroy_event(e["id"].as_str().unwrap(), false).await.unwrap();
        }
    }
}

/// Папки с меткой прогона — насовсем (вложенные раньше родителей); только у стендового user1.
async fn remove_test_folders(c: &Client) {
    let (boxes, _) = c.mailboxes().await.unwrap();
    let mut tagged: Vec<&postman_jmap::Mailbox> = boxes.iter().filter(|m| m.name.starts_with("[postman-test")).collect();
    // Сначала те, у кого нет вложенных среди помеченных.
    while !tagged.is_empty() {
        let leaf = tagged.iter().position(|m| !tagged.iter().any(|o| o.parent_id.as_deref() == Some(m.id.as_str()))).unwrap_or(0);
        let m = tagged.remove(leaf);
        if let Err(e) = c.destroy_mailbox(&m.id).await {
            eprintln!("уборка папки: {e}");
        }
    }
}

/// T1-04в: папки через синхронизацию — создать две, переименовать, перенести одну в другую; дерево на сервере и в
/// кэше; перенос в свою подпапку — отказ ядра. Уборка — папки с меткой `[postman-test …]`.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn folders_through_sync() {
    use postman_core::sync::{Account, Command, Event as SyncEvent, Status};
    let u = user(1).await;
    remove_test_folders(&u.client).await;
    let tag = format!("[postman-test {}]", stamp());
    let account = Account {
        url: env("POSTMAN_STAND_URL"),
        auth: postman_core::model::Auth::Basic { user: u.email.clone(), password: env("POSTMAN_STAND_PASSWORD1") },
    };
    let cache = postman_cache::Cache::open_in_memory().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let commands = postman_backend_jmap::spawn(account, cache.clone(), None, tx, || {});
    wait_for(&rx, "первая синхронизация", |e| matches!(e, SyncEvent::Status(Status::Synced)).then_some(()));
    let key = |name: &str| cache.mailboxes().unwrap().into_iter().find(|m| m.name == name).map(|m| m.id);
    // Дерево в кэше — после `MailboxesChanged` команды (до неё бывают и другие, от push).
    let until = |what: &str, ok: &dyn Fn() -> bool| {
        let start = Instant::now();
        while !ok() {
            assert!(start.elapsed() < DELIVERY, "не дождались: {what}");
            wait_for(&rx, "MailboxesChanged", |e| matches!(e, SyncEvent::MailboxesChanged).then_some(()));
        }
    };

    for name in ["a", "b"] {
        commands.send(Command::CreateFolder { account: None, parent: None, name: format!("{tag} {name}") }).unwrap();
        until(name, &|| key(&format!("{tag} {name}")).is_some());
    }
    let (a, b) = (key(&format!("{tag} a")).unwrap(), key(&format!("{tag} b")).unwrap());
    commands.send(Command::RenameFolder { mailbox: a.clone(), name: format!("{tag} a2") }).unwrap();
    until("a2", &|| key(&format!("{tag} a2")).is_some());
    assert_eq!(key(&format!("{tag} a2")).as_deref(), Some(a.as_str()), "ключ тот же, имя новое");
    commands.send(Command::MoveFolder { mailbox: a.clone(), parent: Some(b.clone()) }).unwrap();
    until("a2 в b", &|| cache.mailboxes().unwrap().into_iter().any(|m| m.id == a && m.parent_id.as_deref() == Some(b.as_str())));
    let (boxes, _) = u.client.mailboxes().await.unwrap();
    let server = boxes.iter().find(|m| m.name == format!("{tag} a2")).expect("a2 на сервере");
    assert_eq!(server.parent_id.as_deref(), Some(b.as_str()), "a2 — в b (сервер)");

    commands.send(Command::MoveFolder { mailbox: b.clone(), parent: Some(a.clone()) }).unwrap();
    let refused = wait_for(&rx, "FolderFailed", |e| match e {
        SyncEvent::FolderFailed { error, .. } => Some(error.clone()),
        _ => None,
    });
    assert_eq!(refused, postman_core::sync::FOLDER_INTO_ITSELF);
    remove_test_folders(&u.client).await;
}
