//! Шифрование Postman против живого Stalwart (S5-12 Ф4): письмо «Конфиденциально» user1 → user3 уходит потоком
//! синхронизации (`Command::Send`, шифрует ядро: RSA — открытая операция, случайные числа — Windows) → у user3 на
//! сервере `application/pkcs7-mime`, расшифровывается ключом user3; копия в «Отправленных» user1 — тоже
//! шифрованная и читается ключом user1. Ключи — тестовые в памяти (`testkit`, `soft`), хранилище Windows не трогается.
//! Без стенда тест пропускается (`#[ignore]`); запуск — как `stand.rs` (`docs/STALWART.md`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)

use std::time::{Duration, Instant};

use postman_cache::Cache;
use postman_compose::{Address, Draft};
use postman_core::sync::{self, Account, Command, Event, Status};
use postman_jmap::{Auth, Client};
use postman_smime::envelope;
use postman_smime::seal::To;
use postman_smime::soft::{self, SoftKey};
use postman_smime::testkit::{Key, NOW, Pki};

const DELIVERY: Duration = Duration::from_secs(30);

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} не задан: стенд — docs/STALWART.md"))
}

async fn user(n: u8) -> (String, Client) {
    let email = env(&format!("POSTMAN_STAND_USER{n}"));
    let password = env(&format!("POSTMAN_STAND_PASSWORD{n}"));
    let c = Client::connect(&env("POSTMAN_STAND_URL"), Auth::Basic { user: email.clone(), password })
        .await
        .unwrap_or_else(|e| panic!("вход {email}: {e}"));
    (email, c)
}

async fn role(c: &Client, r: &str) -> String {
    let (boxes, _) = c.mailboxes().await.unwrap();
    boxes.into_iter().find(|m| m.role.as_deref() == Some(r)).map(|m| m.id).unwrap_or_else(|| panic!("нет папки {r}"))
}

async fn find(c: &Client, mailbox: &str, subject: &str) -> Option<String> {
    let page = c.search(subject, Some(mailbox), 5).await.ok()?;
    page.emails.into_iter().find(|e| e.subject.as_deref() == Some(subject)).map(|e| e.id)
}

async fn wait_find(c: &Client, mailbox: &str, subject: &str, what: &str) -> String {
    let start = Instant::now();
    loop {
        if let Some(id) = find(c, mailbox, subject).await {
            return id;
        }
        assert!(start.elapsed() < DELIVERY, "{what}: письма нет");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Блоб письма целиком и его `Content-Type`.
async fn fetch(c: &Client, id: &str) -> (Vec<u8>, String) {
    let (emails, _) = c.email_get(&[id.to_owned()]).await.unwrap();
    let ct = emails[0].content_type.clone().unwrap_or_default().to_ascii_lowercase();
    let blob = c.email_blobs(&[id.to_owned()]).await.unwrap().remove(0);
    let account = c.session().primary_accounts.get(postman_jmap::MAIL).cloned().unwrap();
    (c.download(&account, &blob.blob_id, "message.eml", "message/rfc822").await.unwrap(), ct)
}

fn opens_with(raw: &[u8], pki: &Pki) -> String {
    let Key::Rsa(k) = &pki.user_key else { unreachable!() };
    let key = SoftKey::new(&pki.user.der, &soft::pkcs1_der(k).unwrap()).unwrap();
    let cek = soft::unwrap(&[key], &envelope::envelope(raw).unwrap()).unwrap();
    envelope::open(raw, &cek, &Default::default(), NOW).unwrap().text.unwrap_or_default()
}

#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn encrypted_round_trip() {
    let (from, sender) = user(1).await;
    let (to, receiver) = user(3).await;
    let pki_from = Pki::build(Key::rsa('a'), Key::rsa('c'), "Postman Test Root (1)", "User 1", &from);
    let pki_to = Pki::rsa("Postman Test Root (3)", "User 3", &to);
    let prepared = |p: &Pki| postman_smime::prepare::prepare(&p.user.der, &[], &p.trust(), NOW).unwrap();
    let subject = format!("Postman confidential {}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs());
    let draft = Draft {
        from: Address::new("User 1", &from),
        to: vec![Address::new("User 3", &to)],
        subject: subject.clone(),
        markdown: "Конфиденциально: **секрет стенда**.".into(),
        keywords: vec!["pm.conf".into()],
        encrypt_to: vec![To { prepared: prepared(&pki_to), gcm: true }, To { prepared: prepared(&pki_from), gcm: true }],
        ..Default::default()
    };

    let (tx, rx) = std::sync::mpsc::channel();
    let account = Account {
        url: env("POSTMAN_STAND_URL"),
        auth: postman_core::model::Auth::Basic { user: from.clone(), password: env("POSTMAN_STAND_PASSWORD1") },
    };
    let commands = postman_backend_jmap::spawn(account, Cache::open_in_memory().unwrap(), None, tx, || {});
    let wait = |what: &str, pick: &dyn Fn(&Event) -> bool| -> Event {
        let start = Instant::now();
        loop {
            let ev = rx.recv_timeout(DELIVERY.saturating_sub(start.elapsed())).unwrap_or_else(|e| panic!("не дождались: {what} ({e})"));
            if pick(&ev) {
                return ev;
            }
        }
    };
    wait("первая синхронизация", &|e| matches!(e, Event::Status(Status::Synced)));
    assert!(commands.send(Command::Send { draft: Box::new(draft), replace: None, ticket: 9, release: sync::Release::Now }).is_ok());
    match wait("отправлено", &|e| matches!(e, Event::Sent { ticket: 9, .. } | Event::ComposeFailed { ticket: 9, .. })) {
        Event::Sent { .. } => {}
        Event::ComposeFailed { error, .. } => panic!("не отправлено: {error}"),
        _ => unreachable!(),
    }

    // У получателя — шифрованное, открывается его ключом; GCM (оба объявили) — authEnveloped-data.
    let id = wait_find(&receiver, &role(&receiver, "inbox").await, &subject, "у user3").await;
    let (raw, ct) = fetch(&receiver, &id).await;
    assert!(ct.contains("application/pkcs7-mime") && ct.contains("authenveloped-data"), "Content-Type: {ct}");
    assert!(opens_with(&raw, &pki_to).contains("секрет стенда"));
    // Копия в «Отправленных» — шифрованная, открывается своим ключом.
    let copy = wait_find(&sender, &role(&sender, "sent").await, &subject, "в «Отправленных» user1").await;
    let (raw, ct) = fetch(&sender, &copy).await;
    assert!(ct.contains("application/pkcs7-mime"), "Content-Type копии: {ct}");
    assert!(opens_with(&raw, &pki_from).contains("секрет стенда"));

    receiver.destroy_email(&id).await.unwrap();
    sender.destroy_email(&copy).await.unwrap();
}
