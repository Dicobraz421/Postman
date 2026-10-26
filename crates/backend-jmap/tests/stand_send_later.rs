//! «Отправить позже» и отмена отправки (S5-06) против живого Stalwart: FUTURERELEASE (`HOLDFOR`/`HOLDUNTIL` в
//! `mailFrom` конверта), `undoStatus: pending` → `canceled`. user1 пишет user3: отложенная и отменённая отправка не
//! доставляется, письмо возвращается в «Черновики»; отложенная без отмены приходит не раньше срока; через поток
//! синхронизации — `Send` с `Release::At` → `Sent` со сроком → `Scheduled` → `Unsend` → `Unsent`.
//! Без стенда тесты пропускаются (`#[ignore]`); сервер без FUTURERELEASE — тесты пишут об этом и выходят.
//!
//! Запуск: `POSTMAN_STAND_URL`, `POSTMAN_STAND_USER1`, `POSTMAN_STAND_PASSWORD1`, `POSTMAN_STAND_USER3`,
//! `POSTMAN_STAND_PASSWORD3` (и `POSTMAN_INSECURE_TLS=1` напрямую), `cargo test -p postman-backend-jmap --test
//! stand_send_later -- --ignored --test-threads 1`. До user3 доходит одно письмо на прогон (ограничение входящих на
//! получателя, `docs/STALWART.md`); письма за собой тест удаляет.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)

use std::time::{Duration, Instant};

use postman_cache::Cache;
use postman_compose::{Address, Draft};
use postman_core::send_later;
use postman_core::sync::{Account, Command, Event, Release, Status};
use postman_jmap::{Auth, Client, Envelope, Submitted, UnsendError};

/// Запас на доставку после срока (очередь Stalwart выпускает письмо с точностью до секунд).
const DELIVERY: Duration = Duration::from_secs(45);

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} не задан: стенд — docs/STALWART.md"))
}

fn auth(n: u8) -> Auth {
    Auth::Basic { user: env(&format!("POSTMAN_STAND_USER{n}")), password: env(&format!("POSTMAN_STAND_PASSWORD{n}")) }
}

async fn user(n: u8) -> Client {
    Client::connect(&env("POSTMAN_STAND_URL"), auth(n)).await.unwrap_or_else(|e| panic!("вход user{n}: {e}"))
}

fn stamp() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    format!("{}-{}", t.as_secs(), t.subsec_nanos() % 1000)
}

async fn role(c: &Client, r: &str) -> String {
    let (boxes, _) = c.mailboxes().await.unwrap();
    boxes.into_iter().find(|m| m.role.as_deref() == Some(r)).map(|m| m.id).unwrap_or_else(|| panic!("нет папки {r}"))
}

/// Сервер умеет отложенную отправку; нет — сказать и пропустить тест.
fn supported(c: &Client) -> bool {
    let max = send_later::max_delay(c.max_delayed_send());
    if max == 0 {
        eprintln!("сервер не объявил maxDelayedSend: FUTURERELEASE нет, тест пропущен");
    }
    max > 0
}

fn draft(subject: &str) -> Draft {
    Draft {
        from: Address::new("User 1", env("POSTMAN_STAND_USER1")),
        to: vec![Address::new("User 3", env("POSTMAN_STAND_USER3"))],
        subject: subject.into(),
        markdown: "Отложенная отправка.".into(),
        ..Default::default()
    }
}

/// user1 → user3 с задержкой `release`.
async fn submit(c: &Client, subject: &str, release: Release) -> Submitted {
    let me = env("POSTMAN_STAND_USER1");
    let identity = c.identities().await.unwrap().into_iter().find(|i| i.email == me).unwrap();
    let d = draft(subject);
    let now = jiff::Timestamp::now().as_second();
    let raw = postman_compose::build(&d, now, &postman_compose::new_message_id(&d.from, now)).unwrap();
    let envelope = Envelope { mail_from: me, rcpt_to: vec![env("POSTMAN_STAND_USER3")] };
    c.submit(
        raw,
        &identity.id,
        &envelope,
        &role(c, "drafts").await,
        &role(c, "sent").await,
        postman_backend_jmap::convert::ToJmap::to_jmap(&release),
        &[],
    )
    .await
    .unwrap()
}

/// Письмо с темой во «Входящих» user3 (id сервера).
async fn in_inbox(c: &Client, subject: &str) -> Option<String> {
    let inbox = role(c, "inbox").await;
    let page = c.search(subject, Some(&inbox), 5).await.ok()?;
    page.emails.into_iter().find(|e| e.subject.as_deref() == Some(subject)).map(|e| e.id)
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn held_submission_is_canceled_and_not_delivered() {
    let (u1, u3) = (user(1).await, user(3).await);
    if !supported(&u1) {
        return;
    }
    let subject = format!("Postman send-later cancel {}", stamp());
    let sub = submit(&u1, &subject, Release::After(30)).await;
    assert!(sub.pending, "отложенная отправка — undoStatus pending");
    let now = jiff::Timestamp::now().as_second();
    let at = sub.send_at.expect("sendAt");
    assert!((now + 25..=now + 35).contains(&at), "sendAt — через 30 с: {at} против {now}");
    // Письмо сразу в «Отправленных» (onSuccessUpdateEmail — при создании отправки), отправка — среди ждущих.
    let (drafts, sent) = (role(&u1, "drafts").await, role(&u1, "sent").await);
    let pending = u1.pending_submissions().await.unwrap();
    assert!(pending.iter().any(|p| p.id == sub.submission_id && p.email_id == sub.email_id), "{pending:?}");

    u1.unsend(&sub.submission_id, &sub.email_id, &drafts, &sent).await.unwrap();
    assert!(u1.pending_submissions().await.unwrap().iter().all(|p| p.id != sub.submission_id));
    // Повторная отмена — «уже не в очереди».
    assert!(matches!(u1.unsend(&sub.submission_id, &sub.email_id, &drafts, &sent).await, Err(UnsendError::TooLate)));
    // Письмо — снова черновик.
    let (back, _) = u1.email_get(std::slice::from_ref(&sub.email_id)).await.unwrap();
    let e = back.first().expect("письмо на месте");
    assert!(e.mailbox_ids.contains_key(&drafts) && !e.mailbox_ids.contains_key(&sent), "{e:?}");
    assert_eq!(e.keywords.get("$draft"), Some(&true), "{e:?}");

    // Срок прошёл — у user3 ничего.
    tokio::time::sleep(Duration::from_secs(40)).await;
    assert_eq!(in_inbox(&u3, &subject).await, None, "отменённое письмо доставлено");
    u1.destroy_email(&sub.email_id).await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn held_submission_is_delivered_after_the_delay() {
    let (u1, u3) = (user(1).await, user(3).await);
    if !supported(&u1) {
        return;
    }
    let subject = format!("Postman send-later deliver {}", stamp());
    let at = jiff::Timestamp::now().as_second() + 20;
    let sub = submit(&u1, &subject, Release::At(at)).await;
    assert_eq!((sub.pending, sub.send_at), (true, Some(at)), "HOLDUNTIL — sendAt тот же");
    // До срока письма нет.
    tokio::time::sleep(Duration::from_secs(8)).await;
    assert_eq!(in_inbox(&u3, &subject).await, None, "письмо пришло раньше срока");
    let start = Instant::now();
    let got = loop {
        if let Some(id) = in_inbox(&u3, &subject).await {
            break id;
        }
        assert!(start.elapsed() < DELIVERY, "письмо не пришло за {DELIVERY:?} после ожидания");
        tokio::time::sleep(Duration::from_secs(2)).await;
    };
    // Срок — по часам сервера (на стенде они на 1–2 с впереди рабочей машины): `receivedAt` не раньше `sendAt`.
    let (got_at, _) = u3.email_get(std::slice::from_ref(&got)).await.unwrap();
    let received = got_at.first().and_then(|e| postman_jmap::utc_to_unix(&e.received_at)).unwrap();
    assert!(received >= at, "доставлено до срока: receivedAt {received}, sendAt {at}");
    // Ушло — отменить нельзя.
    let (drafts, sent) = (role(&u1, "drafts").await, role(&u1, "sent").await);
    assert!(matches!(u1.unsend(&sub.submission_id, &sub.email_id, &drafts, &sent).await, Err(UnsendError::TooLate)));
    u3.destroy_email(&got).await.unwrap();
    u1.destroy_email(&sub.email_id).await.unwrap();
}

type Events = std::sync::mpsc::Receiver<Event>;

fn wait_for<T>(rx: &Events, what: &str, pick: impl Fn(&Event) -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        let left = DELIVERY.checked_sub(start.elapsed()).unwrap_or_else(|| panic!("не дождались: {what}"));
        match rx.recv_timeout(left) {
            Ok(ev) => {
                if let Some(v) = pick(&ev) {
                    return v;
                }
                if let Event::Error(e) = ev {
                    panic!("синхронизация ({what}): {e}");
                }
            }
            Err(e) => panic!("не дождались: {what} ({e})"),
        }
    }
}

/// Тот же путь, что у окна письма: `Send` с `Release::At` → `Sent` со сроком → `Scheduled` с ним → `Unsend` →
/// `Unsent`, и в `Scheduled` его больше нет. Письмо не уходит (отменено за две минуты до срока).
#[tokio::test(flavor = "current_thread")]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn send_later_through_sync() {
    let u1 = user(1).await;
    if !supported(&u1) {
        return;
    }
    let cache = Cache::open_in_memory().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let commands = postman_backend_jmap::spawn(
        Account { url: env("POSTMAN_STAND_URL"), auth: postman_backend_jmap::convert::FromJmap::from_jmap(auth(1)) },
        cache,
        None,
        tx,
        || {},
    );
    wait_for(&rx, "синхронизация", |e| matches!(e, Event::Status(Status::Synced)).then_some(()));
    let max = wait_for(&rx, "отложенные", |e| match e {
        Event::Scheduled { max_delay, .. } => Some(*max_delay),
        _ => None,
    });
    assert_eq!(max, send_later::SERVER_HOLD_MAX, "maxDelayedSend стенда — 30 дней, очередь держит 7");

    let subject = format!("Postman send-later sync {}", stamp());
    let at = jiff::Timestamp::now().as_second() + 120;
    let release = Release::At(at);
    commands.send(Command::Send { draft: Box::new(draft(&subject)), replace: None, ticket: 5, release }).unwrap();
    let scheduled = wait_for(&rx, "отправлено", |e| match e {
        Event::Sent { ticket: 5, scheduled } => Some(scheduled.clone().expect("срок отправки")),
        Event::ComposeFailed { ticket: 5, error } => panic!("отправка: {error}"),
        _ => None,
    });
    assert_eq!(scheduled.send_at, at);
    let listed = wait_for(&rx, "список отложенных", |e| match e {
        Event::Scheduled { list, .. } => Some(list.clone()),
        _ => None,
    });
    assert!(listed.contains(&scheduled), "{listed:?}");

    commands.send(Command::Unsend { scheduled: scheduled.clone(), ticket: 6 }).unwrap();
    let email = wait_for(&rx, "отмена", |e| match e {
        Event::Unsent { ticket: 6, email } => Some(email.clone()),
        Event::UnsendFailed { ticket: 6, error, .. } => panic!("отмена: {error}"),
        _ => None,
    });
    assert_eq!(email, scheduled.email);
    let after = wait_for(&rx, "список после отмены", |e| match e {
        Event::Scheduled { list, .. } => Some(list.clone()),
        _ => None,
    });
    assert!(!after.contains(&scheduled), "{after:?}");
    drop(commands);
    u1.destroy_email(&scheduled.email).await.unwrap();
}
