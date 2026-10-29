//! Перенос 100 000 писем между учётками (S8-04, ADR 0006 п. 3): одна транзакция — членство, индексы, счётчики,
//! `detached` у связей источника и строка задания на каждое письмо. Цель — не больше 5 с на стенде владельца
//! (замер ADR: 1,4–2,4 с). Файл на диске, у части писем 1–3 ключевых слова.
//!
//! Запуск (release): `cargo test --release -p postman-cache --features store6 --bench move -- --ignored --nocapture`.

#![cfg(feature = "store6")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // бенч: «не падать» — для кода приложения (S2-03)

use std::time::Instant;

use postman_cache::{Cache, FolderRow, LetterRow, MOVE_BATCH, Owner, Place, Scope};

const LETTERS: usize = 100_000;
const KEYWORDS: &[&str] = &["pm.cat.clients", "pm.cat.team", "$answered", "pm.cat.tenders"];

fn folder(c: &Cache, account: &str) -> postman_cache::FolderId {
    let row = FolderRow {
        owner: Owner::Account(account.into()),
        parent: None,
        name: "Входящие".into(),
        role: Some("inbox".into()),
        remote: Some("in".into()),
    };
    c.upsert_folder(&row).unwrap()
}

#[test]
#[ignore = "бенч: release, по запросу"]
fn move_100k_between_accounts() {
    const { assert!(LETTERS <= MOVE_BATCH, "одна пачка — одна транзакция") };
    let dir = tempfile::Builder::new().prefix("postman-bench-").tempdir().unwrap();
    let c = Cache::open(dir.path().join("move.redb")).unwrap();
    let (from, to) = (folder(&c, "a"), folder(&c, "b"));
    let rows: Vec<(String, LetterRow)> = (0..LETTERS)
        .map(|i| {
            // Метки у двух из трёх писем, по 1–3.
            let keywords = match i % 3 {
                0 => Vec::new(),
                n => KEYWORDS.iter().cycle().skip(i % 4).take(n + i % 2).map(|k| k.to_string()).collect(),
            };
            let row = LetterRow {
                thread_id: format!("t{}", i / 3),
                received_at: 1_700_000_000 + i as i64 * 37,
                subject: format!("Письмо {i}"),
                from_name: "Отправитель".into(),
                from_addr: "sender@corp.example".into(),
                preview: "Текст превью письма для строки списка".into(),
                size: 20_000,
                keywords,
                flagged: i % 17 == 0,
                folders: vec![from],
                ..LetterRow::default()
            };
            (format!("M{i:08}"), row)
        })
        .collect();
    let ids = c.upsert_remotes("a", &rows).unwrap();
    drop(rows);

    let started = Instant::now();
    let ticket = c.move_letters(&ids, to).unwrap();
    let took = started.elapsed();
    println!("move {LETTERS} letters between accounts: {took:?}");

    assert_eq!((ticket.moved, ticket.outbound), (LETTERS, LETTERS));
    assert_eq!((c.folder_count(from).unwrap(), c.folder_count(to).unwrap()), (0, LETTERS));
    assert_eq!(c.keyword_count_in("pm.cat.clients", &Scope::All(Owner::Account("a".into()))).unwrap(), 0);
    assert!(matches!(c.letter(ids[0]).unwrap().unwrap().place, Place::Outbound(_)));
    assert!(took.as_secs_f64() <= 5.0, "перенос {took:?} дольше 5 с");
}
