//! Подпись Postman против живого Stalwart (S5-12 Ф3): письмо, собранное `postman_compose::build` и подписанное
//! `postman_smime::sign` (тестовый ключ — `testkit`, не хранилище Windows), уходит user1 → user3 обычной отправкой →
//! у user3 блоб письма скачан, подпись верна (байты подписанной части дошли без перекодирования). Без стенда тест
//! пропускается (`#[ignore]`); запуск — как `stand.rs` (`docs/STALWART.md`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)

use std::time::{Duration, Instant};

use postman_compose::{Address, Attachment, Draft};
use postman_jmap::{Auth, Client, Envelope};
use postman_smime::testkit::{Pki, TestSigner};
use postman_smime::{Kind, Status};

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

#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn signed_by_postman_is_verified() {
    let (from, sender) = user(1).await;
    let (to, receiver) = user(3).await;
    let pki = Pki::rsa("Postman Test Root", "User 1", &from);
    let subject = format!("Postman signed {}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs());
    let draft = Draft {
        from: Address::new("User 1", &from),
        to: vec![Address::new("User 3", &to)],
        subject: subject.clone(),
        markdown: "Подписанное письмо Postman — **кириллица**, длинная строка ".repeat(4),
        attachments: vec![Attachment {
            name: "отчёт.txt".into(),
            mime: "text/plain".into(),
            data: "строка\n".repeat(50).into_bytes(),
            inline_cid: None,
            blob_id: None,
        }],
        sign: true,
        ..Default::default()
    };
    let now = jiff::Timestamp::now().as_second();
    let raw = postman_compose::build(&draft, now, &postman_compose::new_message_id(&draft.from, now)).unwrap();
    let signed = postman_smime::sign::sign_message(&raw, &TestSigner { cert: &pki.user, key: &pki.user_key, chain: Vec::new() }, now).unwrap();

    let identity = sender.identities().await.unwrap().into_iter().find(|i| i.email == from).unwrap();
    let envelope = Envelope { mail_from: from.clone(), rcpt_to: vec![to.clone()] };
    sender.send(signed, &identity.id, &envelope, &role(&sender, "drafts").await, &role(&sender, "sent").await).await.unwrap();

    let inbox = role(&receiver, "inbox").await;
    let start = Instant::now();
    let id = loop {
        if let Some(id) = find(&receiver, &inbox, &subject).await {
            break id;
        }
        assert!(start.elapsed() < DELIVERY, "подписанное письмо не пришло");
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    let blob = receiver.email_blobs(std::slice::from_ref(&id)).await.unwrap().remove(0);
    let account = receiver.session().primary_accounts.get(postman_jmap::MAIL).cloned().unwrap();
    let got = receiver.download(&account, &blob.blob_id, "message.eml", "message/rfc822").await.unwrap();
    let r = postman_worker::smime::verify(&got, &pki.trust(), u64::try_from(jiff::Timestamp::now().as_second()).unwrap());
    assert_eq!((r.kind, &r.status), (Kind::Detached, &Status::Valid), "{r:?}");
    assert!(r.address_ok);

    receiver.destroy_email(&id).await.unwrap();
    if let Some(copy) = find(&sender, &role(&sender, "sent").await, &subject).await {
        sender.destroy_email(&copy).await.unwrap();
    }
}
