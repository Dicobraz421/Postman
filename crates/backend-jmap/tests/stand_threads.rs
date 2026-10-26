//! Беседы (S5-05) против живого Stalwart: user1 и user3 обмениваются цепочкой из трёх писем (ответы с In-Reply-To и
//! цитатой, как из окна письма Postman); у user3 синхронизация собирает одну беседу из трёх писем, включая его
//! собственное из «Отправленных» (`Command::Thread` → `Thread/get`), а «только новый текст» оставляет в каждом письме
//! лишь его новые строки. Без стенда тест пропускается (`#[ignore]`).
//!
//! Запуск: `POSTMAN_STAND_URL`, `POSTMAN_STAND_USER1`, `POSTMAN_STAND_PASSWORD1`, `POSTMAN_STAND_USER3`,
//! `POSTMAN_STAND_PASSWORD3` (и `POSTMAN_INSECURE_TLS=1` напрямую), `cargo test -p postman-backend-jmap --test stand_threads
//! -- --ignored`. Писем — три на прогон (ограничение входящих на получателя, `docs/STALWART.md`); в конце они удаляются.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)

use std::time::{Duration, Instant};

use postman_cache::{Cache, EmailRow};
use postman_compose::{Address, Draft, Mode, Original};
use postman_core::sync::{Account, Command, Event, Status};
use postman_core::thread::Conversation;
use postman_ir::{Block, Document};
use postman_jmap::{Auth, Client, EmailSummary, Envelope};

/// Сколько ждать доставку и синхронизацию.
const DELIVERY: Duration = Duration::from_secs(45);

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} не задан: стенд — docs/STALWART.md"))
}

fn auth(n: u8) -> Auth {
    Auth::Basic { user: env(&format!("POSTMAN_STAND_USER{n}")), password: env(&format!("POSTMAN_STAND_PASSWORD{n}")) }
}

async fn user(n: u8) -> (String, Client) {
    let c = Client::connect(&env("POSTMAN_STAND_URL"), auth(n)).await.unwrap_or_else(|e| panic!("вход user{n}: {e}"));
    (env(&format!("POSTMAN_STAND_USER{n}")), c)
}

fn stamp() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    format!("{}-{}", t.as_secs(), t.subsec_nanos() % 1000)
}

async fn role(c: &Client, r: &str) -> String {
    let (boxes, _) = c.mailboxes().await.unwrap();
    boxes.into_iter().find(|m| m.role.as_deref() == Some(r)).map(|m| m.id).unwrap_or_else(|| panic!("нет папки {r}"))
}

/// Отправить черновик из своего аккаунта.
async fn send(c: &Client, me: &str, draft: &Draft) {
    let identity = c.identities().await.unwrap().into_iter().find(|i| i.email == me).unwrap();
    let now = jiff::Timestamp::now().as_second();
    let raw = postman_compose::build(draft, now, &postman_compose::new_message_id(&draft.from, now)).unwrap();
    let envelope = Envelope { mail_from: me.into(), rcpt_to: draft.to.iter().map(|a| a.email.clone()).collect() };
    c.send(raw, &identity.id, &envelope, &role(c, "drafts").await, &role(c, "sent").await).await.unwrap();
}

/// Дождаться во «Входящих» письма с темой. Поиск — по метке прогона: «RE:» в запросе Stalwart не находит.
async fn received(c: &Client, query: &str, subject: &str) -> EmailSummary {
    let inbox = role(c, "inbox").await;
    let start = Instant::now();
    loop {
        if let Ok(page) = c.search(query, Some(&inbox), 10).await
            && let Some(e) = page.emails.into_iter().find(|e| e.subject.as_deref() == Some(subject))
        {
            return e;
        }
        assert!(start.elapsed() < DELIVERY, "письмо «{subject}» не пришло за {DELIVERY:?}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Ответ на письмо `e`, как из окна письма: новый текст сверху, цитата «…, … пишет:» с `>` ниже.
async fn reply(c: &Client, me: &str, e: &EmailSummary, text: &str) -> Draft {
    let body = c.email_body(&e.id).await.unwrap();
    let doc = match (&body.html, &body.text) {
        (Some(h), _) => postman_ir::from_html(h),
        (None, Some(t)) => postman_ir::from_plain(t),
        _ => Document::default(),
    };
    let addr = |a: &postman_jmap::EmailAddress| Address::new(a.name.as_deref().unwrap_or(""), a.email.as_deref().unwrap_or(""));
    let at = jiff::Timestamp::from_second(postman_jmap::utc_to_unix(&e.received_at).unwrap()).unwrap();
    let orig = Original {
        message_id: body.message_id.clone(),
        references: body.references.clone(),
        from: e.from.iter().flatten().map(addr).collect(),
        to: body.to.iter().map(addr).collect(),
        subject: e.subject.clone().unwrap_or_default(),
        date: at.to_zoned(jiff::tz::TimeZone::UTC).strftime("%d.%m.%Y %H:%M").to_string(),
        ..Default::default()
    };
    let mut d = postman_compose::respond(&orig, &doc, Mode::Reply, &Address::new("", me), "");
    d.markdown = format!("{text}{}", d.markdown);
    d
}

fn wait_for<T>(rx: &std::sync::mpsc::Receiver<Event>, what: &str, pick: impl Fn(&Event) -> Option<T>) -> T {
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

/// Текст блоков без разметки.
fn text(blocks: &[Block]) -> String {
    postman_ir::to_markdown(&Document { blocks: blocks.to_vec(), ..Default::default() })
}

/// Удалить письма прогона у пользователя (все папки).
async fn clean(c: &Client, subject: &str) {
    if let Ok(page) = c.search(subject, None, 20).await {
        for e in page.emails.iter().filter(|e| e.subject.as_deref().is_some_and(|s| s.contains(subject))) {
            let _ = c.destroy_email(&e.id).await;
        }
    }
}

#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn reply_chain_is_one_thread_with_new_text_only() {
    let ((me1, c1), (me3, c3)) = (user(1).await, user(3).await);
    let subject = format!("S5-05 thread {}", stamp());
    let reply_subject = format!("RE: {subject}");

    // 1. user1 → user3.
    let first = Draft {
        from: Address::new("User 1", &me1),
        to: vec![Address::new("User 3", &me3)],
        subject: subject.clone(),
        markdown: "Первое письмо: начинаем пилот.\n\nСрок — пятница.".into(),
        ..Default::default()
    };
    send(&c1, &me1, &first).await;
    // 2. user3 отвечает с цитатой (In-Reply-To — Message-ID первого).
    let got1 = received(&c3, &subject, &subject).await;
    let second = reply(&c3, &me3, &got1, "Ответ user3: согласен.").await;
    assert!(second.in_reply_to.is_some(), "у первого письма нет Message-ID");
    // receivedAt — с точностью до секунды: письма цепочки — в разные секунды, порядок однозначен.
    tokio::time::sleep(Duration::from_secs(2)).await;
    send(&c3, &me3, &second).await;
    // 3. user1 отвечает на ответ.
    let got2 = received(&c1, &subject, &reply_subject).await;
    let third = reply(&c1, &me1, &got2, "Третье письмо user1: тогда до пятницы.").await;
    assert_eq!(third.subject, reply_subject);
    tokio::time::sleep(Duration::from_secs(2)).await;
    send(&c1, &me1, &third).await;
    received(&c3, &subject, &reply_subject).await;

    // Синхронизация user3 на кэше в памяти.
    let cache = Cache::open_in_memory().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let commands = postman_backend_jmap::spawn(
        Account { url: env("POSTMAN_STAND_URL"), auth: postman_backend_jmap::convert::FromJmap::from_jmap(auth(3)) },
        cache.clone(),
        None,
        tx,
        || {},
    );
    wait_for(&rx, "первая синхронизация", |e| matches!(e, Event::Status(Status::Synced)).then_some(()));
    let inbox = role(&c3, "inbox").await;
    let last: EmailRow =
        cache.page(&inbox, 0, 200).unwrap().into_iter().find(|r| r.subject == reply_subject).expect("третьего письма нет в кэше после синхронизации");

    // Беседа с сервера: три письма из «Входящих» и «Отправленных», тела — в кэше.
    commands.send(Command::Thread(last.thread_id.clone())).unwrap();
    let ids = wait_for(&rx, "беседа", |e| match e {
        Event::ThreadReady { thread, ids } if *thread == last.thread_id => Some(ids.clone()),
        _ => None,
    });
    assert_eq!(ids.len(), 3, "в беседе не три письма: {ids:?}");
    let rows = cache.thread(&last.thread_id).unwrap();
    assert_eq!(rows.iter().map(|r| &r.id).collect::<Vec<_>>(), ids.iter().collect::<Vec<_>>(), "порядок кэша и сервера");
    let sent = role(&c3, "sent").await;
    assert_eq!(rows.iter().filter(|r| r.mailbox_ids.contains(&sent)).count(), 1, "своё письмо из «Отправленных»");

    let conv = Conversation::assemble(&last.thread_id, rows, std::slice::from_ref(&me3), &jiff::tz::TimeZone::UTC);
    assert_eq!(conv.messages.iter().map(|m| m.mine).collect::<Vec<_>>(), [false, true, false]);
    let order: Vec<(&str, bool)> = conv.messages.iter().map(|m| (m.row.subject.as_str(), m.row.mailbox_ids.contains(&sent))).collect();
    assert_eq!(order, [(subject.as_str(), false), (reply_subject.as_str(), true), (reply_subject.as_str(), false)]);

    // Только новый текст: в каждом письме — его строки, без цитат прошлых.
    let docs: Vec<Document> = conv
        .messages
        .iter()
        .map(|m| {
            let b = cache.body(&m.row.id).unwrap().expect("тело письма беседы не в кэше");
            match (&b.html, &b.text) {
                (Some(h), _) => postman_ir::from_html(h),
                (_, Some(t)) => postman_ir::from_plain(t),
                _ => panic!("у письма нет тела"),
            }
        })
        .collect();
    let fresh: Vec<String> = docs.iter().map(|d| text(d.new_text())).collect();
    assert!(fresh[0].contains("Первое письмо") && fresh[0].contains("пятница"), "{}", fresh[0]);
    assert!(fresh[1].contains("Ответ user3") && !fresh[1].contains("Первое письмо") && !fresh[1].contains("пишет"), "{}", fresh[1]);
    assert!(fresh[2].contains("Третье письмо") && !fresh[2].contains("Ответ user3") && !fresh[2].contains("Первое письмо"), "{}", fresh[2]);
    // Полный IR — с историей (переключатель «показать цитаты»).
    assert!(text(&docs[2].blocks).contains("Ответ user3") && text(&docs[2].blocks).contains("Первое письмо"));

    drop(commands);
    clean(&c1, &subject).await;
    clean(&c3, &subject).await;
}
