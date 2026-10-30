//! S/MIME против живого Stalwart (S5-12 Ф2): шифрованное письмо (тестовый CA и ключ — `testkit`, не хранилище Windows)
//! от user1 к user3 обычной отправкой → у user3 корень — `application/pkcs7-mime` (заголовок `Content-Type` в
//! `Email/get`, локальная метка строки), блоб письма скачан и расшифрован ключом user3 (в памяти, `soft`), подпись
//! внутри верна. Без стенда тест пропускается (`#[ignore]`); запуск — как `stand.rs` (`docs/STALWART.md`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)

use std::time::{Duration, Instant};

use postman_jmap::{Auth, Client, Envelope};
use postman_smime::envelope::Cipher;
use postman_smime::soft::{self, SoftKey};
use postman_smime::testkit::{self, Head, Key, KeyAlg, Pki, Sign, To};
use postman_smime::{Hash, Kind, Status};

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
async fn encrypted_mail_survives_the_server() {
    let (from, sender) = user(1).await;
    let (to, receiver) = user(3).await;
    let pki_to = Pki::rsa("Postman Test Root", "User 3", &to);
    let pki_from = Pki::build(Key::rsa('a'), Key::rsa('c'), "Postman Test Root (sender)", "User 1", &from);
    let subject = format!("Postman encrypted {}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs());
    // Подписать, затем зашифровать (себе и получателю), как Outlook: AES-256-CBC + RSA PKCS#1 v1.5.
    let inner =
        testkit::signed_part(&testkit::html_part("<p>Секрет стенда</p>"), &pki_from.user, &pki_from.user_key, &[&pki_from.root], &Sign::default());
    let cek = [0x33u8; 32];
    let to_list = [
        To { cert: &pki_from.user, key: &pki_from.user_key, alg: KeyAlg::RsaPkcs1, ski: false },
        To { cert: &pki_to.user, key: &pki_to.user_key, alg: KeyAlg::RsaOaep(Hash::Sha256), ski: false },
    ];
    let cms = testkit::enveloped(&inner, &to_list, Cipher::AesCbc(256), &cek, false);
    let date = jiff::Zoned::now().strftime("%a, %d %b %Y %H:%M:%S %z").to_string();
    let raw = testkit::encrypted_letter(&Head { from: &from, to: &to, subject: &subject, date: &date }, &cms, false);

    let identity = sender.identities().await.unwrap().into_iter().find(|i| i.email == from).unwrap();
    let envelope = Envelope { mail_from: from.clone(), rcpt_to: vec![to.clone()] };
    sender.send(raw.clone(), &identity.id, &envelope, &role(&sender, "drafts").await, &role(&sender, "sent").await).await.unwrap();

    let inbox = role(&receiver, "inbox").await;
    let start = Instant::now();
    let id = loop {
        if let Some(id) = find(&receiver, &inbox, &subject).await {
            break id;
        }
        assert!(start.elapsed() < DELIVERY, "шифрованное письмо не пришло");
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    // Метаданные: корень — pkcs7-mime (по нему строка списка получает замок).
    let (emails, _) = receiver.email_get(std::slice::from_ref(&id)).await.unwrap();
    let ct = emails[0].content_type.clone().unwrap_or_default().to_ascii_lowercase();
    assert!(ct.contains("application/pkcs7-mime") && ct.contains("enveloped-data"), "Content-Type: {ct}");
    // Блоб письма — расшифровывается ключом получателя, подпись внутри верна.
    let blob = receiver.email_blobs(std::slice::from_ref(&id)).await.unwrap().remove(0);
    let account = receiver.session().primary_accounts.get(postman_jmap::MAIL).cloned().unwrap();
    let got = receiver.download(&account, &blob.blob_id, "message.eml", "message/rfc822").await.unwrap();
    let env_ = postman_worker::smime::envelope(&got).unwrap();
    assert_eq!(env_.recipients.len(), 2);
    let Key::Rsa(k) = &pki_to.user_key else { unreachable!() };
    let key = SoftKey::new(&pki_to.user.der, &soft::pkcs1_der(k).unwrap()).unwrap();
    let cek_got = soft::unwrap(std::slice::from_ref(&key), &env_).unwrap();
    assert_eq!(cek_got.as_slice(), &cek);
    let opened = postman_worker::smime::open(&got, &cek_got, &pki_from.trust(), jiff::Timestamp::now().as_second() as u64).unwrap();
    assert_eq!((opened.inner.kind, &opened.inner.status), (Kind::Detached, &Status::Valid), "{:?}", opened.inner);
    assert_eq!(opened.html.as_deref(), Some("<p>Секрет стенда</p>"));

    receiver.destroy_email(&id).await.unwrap();
    if let Some(copy) = find(&sender, &role(&sender, "sent").await, &subject).await {
        sender.destroy_email(&copy).await.unwrap();
    }
}
