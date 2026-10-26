//! Большой ящик на стенде (S4-03): заполнить учётную запись Stalwart `N` письмами — для замеров живого ящика.
//!
//!   POSTMAN_URL=https://<стенд> POSTMAN_USER=… POSTMAN_PASSWORD=… [POSTMAN_INSECURE_TLS=1]
//!   cargo run --release -p postman-backend-jmap --example fill_stand -- 20000 [--batch 250]
//!
//! Письма — из содержимого демо-ящика (`postman-demo`: отправители, темы, текст), текст и HTML (Markdown →
//! `postman_ir::markdown_to_html`), даты — в прошлое от сейчас. Пачка — один запрос `Email/set create` с телом
//! в `bodyValues`: сервер сам собирает MIME. `Email/import` здесь не годится: у Stalwart v0.16.27 квота загрузки
//! блобов — 1000 файлов или 50 МБ на учётную запись (`overQuota` после первой тысячи писем), а квота считает и
//! уже импортированные блобы, пока не истечёт их срок. 85 % — во «Входящие», остальное — в «Архив» (если папка
//! с ролью `archive` есть). Повторный запуск дополняет ящик до `N`. Пароль — только из окружения.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // инструмент разработчика, не код приложения

use std::time::Instant;

use postman_jmap::{Auth, Client, Invocation};
use serde_json::{Value, json};

const USING: &[&str] = &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"];

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
}

/// xorshift: одинаковое содержимое от запуска к запуску.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn pick<'a, T>(&mut self, v: &'a [T]) -> &'a T {
        &v[self.next() as usize % v.len()]
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let target: u64 = args.first().and_then(|s| s.parse().ok()).unwrap_or(20_000);
    let batch: usize = args.iter().position(|a| a == "--batch").and_then(|i| args.get(i + 1)).and_then(|s| s.parse().ok()).unwrap_or(250);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(fill(target, batch));
}

fn utc(ts: i64) -> String {
    jiff::Timestamp::from_second(ts).unwrap().strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

async fn fill(target: u64, batch: usize) {
    let user = env("POSTMAN_USER");
    let client = Client::connect(&env("POSTMAN_URL"), Auth::Basic { user: user.clone(), password: env("POSTMAN_PASSWORD") })
        .await
        .unwrap_or_else(|e| panic!("sign-in as {user}: {e}"));
    let acc = client.mail_account().unwrap().to_string();
    let (boxes, _) = client.mailboxes().await.unwrap();
    let role = |r: &str| boxes.iter().find(|m| m.role.as_deref() == Some(r));
    let inbox = role("inbox").expect("no inbox").id.clone();
    let archive = role("archive").map(|m| m.id.clone());
    let have: u64 = boxes.iter().map(|m| u64::from(m.total_emails)).sum();
    if have >= target {
        println!("fill: {user} already has {have} messages (target {target})");
        return;
    }
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ have);
    // Сначала свежие: каждое следующее письмо старше предыдущего на 0–60 минут; дозаполнение — старше уже загруженных.
    let mut at = jiff::Timestamp::now().as_second() - have as i64 * 1800;
    let started = Instant::now();
    let mut done = have;
    while done < target {
        let n = batch.min((target - done) as usize);
        let mut emails = serde_json::Map::new();
        for i in 0..n {
            at -= (rng.next() % 3600) as i64;
            let (name, addr) = *rng.pick(postman_demo::SENDERS);
            let subject = format!("{} {}", rng.pick(postman_demo::SUBJECTS), (done as usize + i) % 997);
            let text = format!("{}\n\n- пункт {}\n- срок — пятница\n\nС уважением,\n{name}", postman_demo::PREVIEW, rng.next() % 100);
            let html = postman_ir::markdown_to_html(&text);
            let mailbox = match (&archive, rng.next() % 100) {
                (Some(a), 0..15) => a.clone(),
                _ => inbox.clone(),
            };
            let mut keywords = serde_json::Map::new();
            if !rng.next().is_multiple_of(5) {
                keywords.insert("$seen".into(), json!(true));
            }
            if rng.next().is_multiple_of(50) {
                keywords.insert("$flagged".into(), json!(true));
            }
            emails.insert(
                format!("e{i}"),
                json!({
                    "mailboxIds": { mailbox: true },
                    "keywords": keywords,
                    "receivedAt": utc(at),
                    "sentAt": utc(at),
                    "messageId": [format!("fill-{}.{}@itt.test", done as usize + i, rng.next() % 1_000_000)],
                    "from": [{ "name": name, "email": addr }],
                    "to": [{ "email": user }],
                    "subject": subject,
                    "bodyValues": { "t": { "value": text }, "h": { "value": html } },
                    "textBody": [{ "partId": "t", "type": "text/plain" }],
                    "htmlBody": [{ "partId": "h", "type": "text/html" }],
                }),
            );
        }
        let calls = [Invocation("Email/set".into(), json!({ "accountId": acc, "create": emails }), "s".into())];
        let r = client.call(USING, &calls).await.unwrap_or_else(|e| panic!("batch at {done}: {e}"));
        let set = &r[0].1;
        if let Some(bad) = set.get("notCreated").and_then(Value::as_object).filter(|m| !m.is_empty()) {
            panic!("Email/set: {} not created, first: {}", bad.len(), bad.values().next().unwrap_or(&Value::Null));
        }
        let created = set.get("created").and_then(Value::as_object).map_or(0, |m| m.len());
        assert!(created > 0, "the server created nothing");
        done += created as u64;
        let secs = started.elapsed().as_secs_f32();
        println!("fill: {done}/{target} ({:.0} msg/s)", (done - have) as f32 / secs.max(0.001));
    }
    println!("fill: done, {} messages in {:.0} s", done - have, started.elapsed().as_secs_f32());
}
