//! Замер: индексация N писем и время запросов. `cargo run --release -p postman-search --example bench -- 50000 [--keep]`
//! Индекс — во временном каталоге `postman-search-bench-*`, удаляется после замера; `--keep` — оставить.
use std::time::Instant;

use postman_search::{Doc, SearchIndex};

const WORDS: &[&str] = &[
    "отчёт",
    "квартал",
    "совещание",
    "проект",
    "договор",
    "оплата",
    "счёт",
    "поставка",
    "сервер",
    "доступ",
    "пропуск",
    "парковка",
    "регламент",
    "бюджет",
    "закупка",
    "лицензия",
    "deploy",
    "pipeline",
    "release",
    "incident",
    "migration",
    "сотрудник",
    "отпуск",
    "командировка",
    "презентация",
    "заказчик",
    "требования",
    "аудит",
    "безопасность",
    "сертификат",
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let n: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(50_000);
    let tmp = tempfile::Builder::new().prefix("postman-search-bench-").tempdir()?;
    let dir = tmp.path().to_path_buf();
    let idx = SearchIndex::open(&dir)?;
    let mut rng = 0x2545F4914F6CDD1Du64;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng as usize
    };
    let t = Instant::now();
    let mut texts = Vec::with_capacity(n);
    for i in 0..n {
        let subj: Vec<&str> = (0..4).map(|_| WORDS[next() % WORDS.len()]).collect();
        let body: Vec<&str> = (0..40).map(|_| WORDS[next() % WORDS.len()]).collect();
        texts.push((i.to_string(), subj.join(" "), format!("Сотрудник {} user{}@corp.example", i % 300, i % 300), body.join(" ")));
    }
    for chunk in texts.chunks(5000) {
        let docs: Vec<Doc> = chunk.iter().map(|(id, s, p, b)| Doc { id, subject: s, people: p, body: b, attachments: "", received_at: 0 }).collect();
        idx.upsert(&docs)?;
    }
    idx.commit()?;
    let index_time = t.elapsed();
    let size: u64 = std::fs::read_dir(&dir)?.flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum();
    println!("docs={} index={:?} size={:.1}MB", idx.len(), index_time, size as f64 / 1e6);
    for q in ["отчеты", "кваратл", "jnxtn", "командир", "отчет квартал безопасность", "deploying", "сотрудник 17", "лиценз"]
    {
        let t = Instant::now();
        let mut hits = 0;
        for _ in 0..20 {
            hits = idx.search(q, 50)?.len();
        }
        println!("{q:28} hits={hits:3} {:?}/запрос", t.elapsed() / 20);
    }
    let rss = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    println!("{}", rss.lines().find(|l| l.starts_with("VmRSS")).unwrap_or(""));
    drop(idx);
    if std::env::args().any(|a| a == "--keep") {
        println!("индекс оставлен: {}", tmp.keep().display());
    }
    Ok(())
}
