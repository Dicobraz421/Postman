//! Замер хранилища формата 7 (ADR 0006, дополнение 2026-11-14): демо-ящик на 50 000 писем (у каждого десятого —
//! письмо целиком и тело, `postman_demo::rich`) с разными настройками — размер файла (после уплотнения), сводка
//! [`postman_cache::StoreStats`], время сборки; отдельно — запись и чтение письма по одному (как при открытии).
//!
//! Запуск (release): `cargo test --release -p postman-demo --test store_size -- --ignored --nocapture`.
//! Файлы — в `%LOCALAPPDATA%\Pets\testdata\postman\store-size`, удаляются в конце.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // замер: «не падать» — для кода приложения (S2-03)

use std::path::Path;
use std::time::Instant;

use postman_cache::{Cache, FolderRow, LetterRow, Owner, Tuning};
use postman_demo::rich;

const LETTERS: usize = 50_000;
/// Писем в замере записи и чтения по одному.
const SINGLE: usize = 1_000;

fn variants() -> Vec<(&'static str, Tuning)> {
    let t = |min_part: usize, min_image: usize, level: Option<i32>| Tuning { min_part, min_image, level };
    let k = 1024;
    vec![
        ("формат 6 (части от 64 КиБ, без сжатия)", Tuning::FORMAT_6),
        ("п. 1: части от 4 КиБ, картинки от 1 КиБ", t(4 * k, k, None)),
        ("п. 2: zstd 3, части от 64 КиБ", t(64 * k, 64 * k, Some(3))),
        ("п. 1 + п. 2: zstd 3", t(4 * k, k, Some(3))),
        ("п. 1 + п. 2: zstd 1", t(4 * k, k, Some(1))),
        ("п. 1 + п. 2: zstd 6", t(4 * k, k, Some(6))),
        ("порог 1 КиБ, zstd 3", t(k, k, Some(3))),
        ("порог 2 КиБ, zstd 3", t(2 * k, k, Some(3))),
        ("порог 8 КиБ, zstd 3", t(8 * k, k, Some(3))),
        ("порог 16 КиБ, картинки от 1 КиБ, zstd 3", t(16 * k, k, Some(3))),
        ("порог 64 КиБ, картинки от 1 КиБ, zstd 3", t(64 * k, k, Some(3))),
    ]
}

fn mb(b: u64) -> f64 {
    b as f64 / 1_048_576.0
}

fn compacted(path: &Path) -> u64 {
    let mut db = redb::Database::create(path).unwrap();
    while db.compact().unwrap() {}
    drop(db);
    std::fs::metadata(path).unwrap().len()
}

#[test]
#[ignore = "замер: release, по запросу"]
fn store_size_by_tuning() {
    let dir = postman_demo::testdata_dir().join("store-size");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    println!("| Настройка | Файл, МБ | После уплотнения, МБ | Письма и тела как есть, МБ | Без повторов, МБ | В записях, МБ | Частей | Сборка, с |");
    println!("|---|---|---|---|---|---|---|---|");
    for (n, (name, tuning)) in variants().into_iter().enumerate() {
        let path = dir.join(format!("v{n}.redb"));
        let started = Instant::now();
        let st = {
            let c = Cache::open(&path).unwrap().tuned(tuning);
            postman_demo::build(&c, LETTERS).unwrap();
            c.store_stats().unwrap()
        };
        let built = started.elapsed().as_secs_f64();
        let raw = std::fs::metadata(&path).unwrap().len();
        let small = compacted(&path);
        println!(
            "| {name} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {} | {built:.1} |",
            mb(raw),
            mb(small),
            mb(st.logical),
            mb(st.unique),
            mb(st.stored),
            st.parts
        );
        std::fs::remove_file(&path).unwrap();
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Настройки для замера записи и чтения.
fn timed() -> Vec<(&'static str, Tuning)> {
    let t = |min_part: usize, min_image: usize, level: Option<i32>| Tuning { min_part, min_image, level };
    let k = 1024;
    vec![
        ("формат 6", Tuning::FORMAT_6),
        ("п. 1 (4 КиБ)", t(4 * k, k, None)),
        ("п. 2 (zstd 3)", t(64 * k, 64 * k, Some(3))),
        ("п. 1 + п. 2, zstd 1", t(4 * k, k, Some(1))),
        ("п. 1 + п. 2, zstd 3", t(4 * k, k, Some(3))),
        ("п. 1 (16 КиБ) + п. 2, zstd 3", t(16 * k, k, Some(3))),
    ]
}

/// Одна проба: письма по одному (транзакция с фиксацией на письмо, как при открытии), чтение, тела; затем все
/// другие письма одной транзакцией (одна фиксация — почти только работа процессора). Миллисекунды на письмо.
fn probe(path: &Path, tuning: Tuning, letters: &[rich::Letter], others: &[rich::Letter]) -> [f64; 5] {
    let _ = std::fs::remove_file(path);
    let c = Cache::open(path).unwrap().tuned(tuning);
    let f = c
        .upsert_folder(&FolderRow { owner: Owner::Account(String::new()), parent: None, name: "in".into(), role: None, remote: Some("in".into()) })
        .unwrap();
    let rows: Vec<(String, LetterRow)> = (0..2 * SINGLE).map(|i| (format!("m{i}"), LetterRow { folders: vec![f], ..LetterRow::default() })).collect();
    let ids = c.upsert_remotes("", &rows).unwrap();
    let per = |t: Instant| t.elapsed().as_secs_f64() * 1000.0 / SINGLE as f64;
    let t = Instant::now();
    for (id, l) in ids.iter().zip(letters) {
        c.put_content(*id, &l.raw, &l.parts).unwrap();
    }
    let write = per(t);
    let t = Instant::now();
    for (id, l) in ids.iter().zip(letters) {
        assert!(c.letter_content(*id).unwrap().unwrap().len() == l.raw.len());
    }
    let read = per(t);
    let t = Instant::now();
    for (i, l) in letters.iter().enumerate() {
        c.put_body(&format!("m{i}"), &l.body).unwrap();
    }
    let put_body = per(t);
    let t = Instant::now();
    for i in 0..SINGLE {
        assert!(c.body(&format!("m{i}")).unwrap().is_some());
    }
    let get_body = per(t);
    let items: Vec<_> = ids[SINGLE..].iter().zip(others).map(|(id, l)| (*id, l.raw.as_slice(), l.parts.as_slice())).collect();
    let t = Instant::now();
    c.put_contents(&items).unwrap();
    let batch = per(t);
    drop(c);
    std::fs::remove_file(path).unwrap();
    [write, read, put_body, get_body, batch]
}

#[test]
#[ignore = "замер: release, по запросу"]
fn write_and_read_one_by_one() {
    const ROUNDS: usize = 3;
    let dir = postman_demo::testdata_dir().join("store-size");
    std::fs::create_dir_all(&dir).unwrap();
    // Письма с вложениями: те же, что в демо, только подряд (у каждого седьмого — вложение).
    let letters: Vec<rich::Letter> =
        (0..SINGLE).map(|n| rich::letter(n * rich::EVERY, ("Иванова Мария", "m.ivanova@corp.example"), "Отчёт за квартал", 1_760_000_000)).collect();
    let others: Vec<rich::Letter> = (0..SINGLE)
        .map(|n| rich::letter(n * rich::EVERY + 1, ("Петров Алексей", "a.petrov@corp.example"), "Счёт на оплату", 1_760_000_000))
        .collect();
    let bytes: usize = letters.iter().map(|l| l.raw.len()).sum();
    println!("{SINGLE} писем, в среднем {:.1} КБ; лучшее из {ROUNDS} проб, пробы по кругу", bytes as f64 / SINGLE as f64 / 1024.0);
    let variants = timed();
    let mut best = vec![[f64::MAX; 5]; variants.len()];
    for _ in 0..ROUNDS {
        for (n, (_, tuning)) in variants.iter().enumerate() {
            let r = probe(&dir.join(format!("one{n}.redb")), *tuning, &letters, &others);
            for (b, v) in best[n].iter_mut().zip(r) {
                *b = b.min(v);
            }
        }
    }
    println!("| Настройка | Запись письма, мс | Чтение письма, мс | Запись тела, мс | Чтение тела, мс | Запись без фиксации, мс |");
    println!("|---|---|---|---|---|---|");
    for ((name, _), [w, r, pb, gb, b]) in variants.iter().zip(best) {
        println!("| {name} | {w:.2} | {r:.2} | {pb:.2} | {gb:.3} | {b:.2} |");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Шаблон бенча (`bench-template-50000-f<формат>.redb`) с настройкой `POSTMAN_STORE_TUNING` (`6` — как формат 6, иначе
/// своя): `--bench` до и после на одном exe. Запуск: `POSTMAN_STORE_TUNING=6 cargo test --release -p postman-demo
/// --test store_size -- --ignored bench_template`.
#[test]
#[ignore = "замер: release, по запросу"]
fn bench_template() {
    let tuning = if std::env::var("POSTMAN_STORE_TUNING").as_deref() == Ok("6") { Tuning::FORMAT_6 } else { Tuning::default() };
    let path = postman_demo::testdata_dir().join(format!("bench-template-{LETTERS}-f{}.redb", postman_cache::FORMAT_VERSION));
    let _ = std::fs::remove_file(&path);
    postman_demo::build(&Cache::open(&path).unwrap().tuned(tuning), LETTERS).unwrap();
    println!("{}: {:.1} МБ", path.display(), mb(compacted(&path)));
}
