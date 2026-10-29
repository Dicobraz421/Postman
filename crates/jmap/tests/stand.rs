//! Выгрузка и перенос писем (S8-11) против стенда Stalwart (`docs/STALWART.md`) [проверить на 1.0]: пределы сессии,
//! `Email/import` в любую папку с ключевыми словами и датой получения, повтор (`alreadyExists`), `Email/copy` в общий
//! ящик с удалением источника. Без стенда тесты пропускаются (`#[ignore]`).
//!
//! Запуск: `POSTMAN_STAND_URL`, `POSTMAN_STAND_USER1`, `POSTMAN_STAND_PASSWORD1` (для `copy_to_shared_mailbox` —
//! `POSTMAN_STAND_USER3`, `POSTMAN_STAND_PASSWORD3`, участник ящика группы; `POSTMAN_INSECURE_TLS=1` напрямую),
//! `cargo test -p postman-jmap --test stand -- --ignored --test-threads=1`. Всё созданное удаляется.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)

use postman_jmap::{Auth, Client};

/// Ящик группы, где user3 — участник (`docs/STALWART.md`, «Общие ящики»).
const GROUP: &str = "shared-sales@itt.test";

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} не задан: стенд — docs/STALWART.md"))
}

async fn client(n: &str) -> Client {
    let auth = Auth::Basic { user: env(&format!("POSTMAN_STAND_USER{n}")), password: env(&format!("POSTMAN_STAND_PASSWORD{n}")) };
    Client::connect(&env("POSTMAN_STAND_URL"), auth).await.unwrap_or_else(|e| panic!("вход user{n}: {e}"))
}

fn stamp() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    format!("{}-{}", t.as_secs(), t.subsec_nanos())
}

fn letter(subject: &str) -> Vec<u8> {
    format!(
        "From: <sender@itt.test>\r\nTo: <user@itt.test>\r\nSubject: {subject}\r\nMessage-ID: <{subject}@postman.test>\r\nDate: Sat, 17 Oct 2026 12:30:00 +0000\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nВыгрузка S8-11.\r\n"
    )
    .into_bytes()
}

async fn inbox(c: &Client) -> String {
    let (boxes, _) = c.mailboxes().await.unwrap();
    boxes.into_iter().find(|m| m.role.as_deref() == Some("inbox")).map(|m| m.id).expect("«Входящие»")
}

/// Пределы сессии, выгрузка с ключевыми словами и датой получения, повтор той же выгрузки.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn import_into_inbox() {
    let c = client("1").await;
    let limits = c.session().limits();
    assert!(limits.max_size_upload > 0 && limits.max_objects_in_set > 0 && limits.max_calls_in_request > 0, "{limits:?}");
    let acc = c.mail_account().unwrap().to_owned();
    let inbox = inbox(&c).await;
    let subject = format!("postman-import-{}", stamp());
    let raw = letter(&subject);
    let blob = c.upload(&acc, raw.clone(), "message/rfc822").await.unwrap();
    let keywords = ["$seen".to_string(), "pm.cat.clients".into()];
    let received = 1_792_240_200; // 2026-10-17 12:30:00 UTC
    let first = c.import_email(&acc, &blob, std::slice::from_ref(&inbox), &keywords, Some(received)).await.unwrap();
    assert!(!first.existed);
    let (got, _) = c.email_get(std::slice::from_ref(&first.id)).await.unwrap();
    let e = got.first().expect("письмо выгружено");
    assert_eq!(postman_jmap::utc_to_unix(&e.received_at), Some(received), "receivedAt — дата письма");
    assert!(e.has_keyword("$seen") && e.has_keyword("pm.cat.clients"), "{:?}", e.keywords);
    assert!(!e.has_keyword("$draft"), "не черновик");

    // Повтор: Stalwart узнаёт письмо (`alreadyExists`) или создаёт второе — [проверить на 1.0].
    let again = c.import_email(&acc, &blob, std::slice::from_ref(&inbox), &keywords, Some(received)).await.unwrap();
    println!("пределы {limits:?}; повтор выгрузки: alreadyExists = {}", again.existed);
    if again.existed {
        assert_eq!(again.id, first.id);
    } else {
        c.destroy_email(&again.id).await.unwrap();
    }
    c.destroy_email(&first.id).await.unwrap();
}

/// `Email/copy` из своего ящика в ящик группы с удалением источника (перенос без выгрузки); уборка.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md, «Общие ящики»"]
async fn copy_to_shared_mailbox() {
    let c = client("3").await;
    let own = c.mail_account().unwrap().to_owned();
    let group = c.session().shared_accounts().into_iter().find(|a| a.name == GROUP).expect("ящик группы в сессии");
    let shared = c.in_account(&group.id);
    let (own_inbox, group_inbox) = (inbox(&c).await, inbox(&shared).await);
    let subject = format!("postman-copy-{}", stamp());
    let blob = c.upload(&own, letter(&subject), "message/rfc822").await.unwrap();
    let src = c.import_email(&own, &blob, std::slice::from_ref(&own_inbox), &[], None).await.unwrap();

    let copied = c.copy_emails(&own, &group.id, std::slice::from_ref(&src.id), &group_inbox, true).await.unwrap();
    assert_eq!(copied.len(), 1);
    let new_id = copied[0].copy.as_ref().unwrap().id.clone();
    assert!(!copied[0].original_left, "источник удалён");
    let (left, _) = c.email_get(std::slice::from_ref(&src.id)).await.unwrap();
    assert!(left.is_empty(), "в своём ящике письма нет");
    let (there, _) = shared.email_get(std::slice::from_ref(&new_id)).await.unwrap();
    assert_eq!(there.len(), 1, "письмо в ящике группы");
    shared.destroy_email(&new_id).await.unwrap();
}
