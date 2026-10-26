//! Жизненный цикл писем (S5-04) против живого Stalwart: ключевые слова сроков (`postman_core::marks`) ставятся,
//! читаются и ищутся как есть; рассылки узнаются по сырым заголовкам `List-Id` / `List-Unsubscribe` в `Email/get`
//! (условие `header` в `Email/query` Stalwart не выполняет — `header_filter_finds_nothing`); `before` отсекает
//! свежие; «Напомнить» — перенос в «Архив» и возврат во «Входящие» непрочитанным одним `Email/set`. «Архива» у
//! стенда нет — папка с ролью `archive` создаётся (так делает и Postman, `sync::Command::Patch`).
//!
//! Письма создаются прямо во «Входящих» user1 (`Email/set create` и `Email/import` сырого MIME, без SMTP) и в конце
//! удаляются, как и созданная папка. Без стенда тесты пропускаются (`#[ignore]`).
//!
//! Запуск: `POSTMAN_STAND_URL`, `POSTMAN_STAND_USER1`, `POSTMAN_STAND_PASSWORD1` (и `POSTMAN_INSECURE_TLS=1`
//! напрямую), `cargo test -p postman-backend-jmap --test stand_lifecycle -- --ignored --nocapture --test-threads=1`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)

use jiff::civil::{Date, Time};
use postman_core::labels::valid_keyword;
use postman_core::lifecycle::{self, NewsItem, newsletter_sweep};
use postman_core::marks::{self, Kind, When};
use postman_core::sync;
use postman_jmap::{Auth, CORE, Client, EmailPatch, Filter, Invocation, MAIL};
use serde_json::{Value, json};

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} не задан: стенд — docs/STALWART.md"))
}

async fn user1() -> Client {
    let auth = Auth::Basic { user: env("POSTMAN_STAND_USER1"), password: env("POSTMAN_STAND_PASSWORD1") };
    Client::connect(&env("POSTMAN_STAND_URL"), auth).await.unwrap_or_else(|e| panic!("вход user1: {e}"))
}

fn stamp() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    format!("{}{:03}", t.as_secs(), t.subsec_millis())
}

async fn find_role(c: &Client, r: &str) -> Option<String> {
    let (boxes, _) = c.mailboxes().await.unwrap();
    boxes.into_iter().find(|m| m.role.as_deref() == Some(r)).map(|m| m.id)
}

async fn role(c: &Client, r: &str) -> String {
    find_role(c, r).await.unwrap_or_else(|| panic!("нет папки {r}"))
}

/// Письмо во «Входящих» без доставки (`Email/set create`); `extra` — поля сверх обычных.
async fn create(c: &Client, inbox: &str, subject: &str, received: &str, extra: Value) -> String {
    let acc = c.mail_account().unwrap().to_owned();
    let mut email = json!({
        "mailboxIds": { inbox: true },
        "keywords": { "$seen": true },
        "receivedAt": received,
        "from": [{ "name": "Stand", "email": "plain@itt.test" }],
        "to": [{ "email": env("POSTMAN_STAND_USER1") }],
        "subject": subject,
        "bodyValues": { "t": { "value": "Жизненный цикл (S5-04)." } },
        "textBody": [{ "partId": "t", "type": "text/plain" }],
    });
    if let (Some(e), Some(x)) = (email.as_object_mut(), extra.as_object()) {
        e.extend(x.clone());
    }
    let calls = [Invocation("Email/set".into(), json!({ "accountId": acc, "create": { "n": email } }), "s".into())];
    let r = c.call(&[CORE, MAIL], &calls).await.unwrap();
    r[0].1["created"]["n"]["id"].as_str().unwrap_or_else(|| panic!("письмо не создано: {}", r[0].1)).to_owned()
}

/// Письмо, как пришло бы по SMTP: сырой MIME с заголовками рассылки — `Email/import` во «Входящие», прочитанным.
async fn import(c: &Client, inbox: &str, from: &str, subject: &str, received: &str, headers: &str) -> String {
    let acc = c.mail_account().unwrap().to_owned();
    let raw = format!(
        "From: Stand news <{from}>\r\nTo: {}\r\nSubject: {subject}\r\nDate: Mon, 05 Jan 2026 08:00:00 +0000\r\n\
         Message-ID: <{}@itt.test>\r\n{headers}MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nNews.\r\n",
        env("POSTMAN_STAND_USER1"),
        stamp(),
    );
    let blob = c.upload(&acc, raw.into_bytes(), "message/rfc822").await.unwrap();
    let email = json!({ "blobId": blob, "mailboxIds": { inbox: true }, "keywords": { "$seen": true }, "receivedAt": received });
    let args = json!({ "accountId": acc, "emails": { "n": email } });
    let r = c.call(&[CORE, MAIL], &[Invocation("Email/import".into(), args, "i".into())]).await.unwrap();
    r[0].1["created"]["n"]["id"].as_str().unwrap_or_else(|| panic!("письмо не загружено: {}", r[0].1)).to_owned()
}

async fn keywords_on_server(c: &Client, id: &str) -> Vec<String> {
    let (emails, _) = c.email_get(&[id.to_owned()]).await.unwrap();
    let mut out: Vec<String> = emails[0].keywords.iter().filter(|(_, on)| **on).map(|(k, _)| k.clone()).collect();
    out.sort();
    out
}

/// Остатки прошлого упавшего прогона: письма «Postman lc …» и папки «Archive <метка>».
async fn sweep_leftovers(c: &Client) {
    let left = c.search("Postman lc", None, 200).await.unwrap();
    for e in left.emails.iter().filter(|e| e.subject.as_deref().unwrap_or("").starts_with("Postman lc ")) {
        c.destroy_email(&e.id).await.unwrap();
    }
    let (boxes, _) = c.mailboxes().await.unwrap();
    let ours = |name: &str| name.strip_prefix("Archive ").is_some_and(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()));
    for m in boxes.iter().filter(|m| ours(&m.name)) {
        c.destroy_mailbox(&m.id).await.unwrap();
    }
}

/// Все слова сроков ставятся одним патчем, читаются тем же текстом, ищутся по `hasKeyword` — и по слову с датой, и
/// по общему слову (`pm.due`), которое ставится рядом: по нему находит письма «Все папки» и планировщик.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn lifecycle_keywords_round_trip() {
    let c = user1().await;
    let inbox = role(&c, "inbox").await;
    let run = stamp();
    let now = jiff::Timestamp::now().strftime("%Y-%m-%dT%H:%M:%SZ").to_string();
    let id = create(&c, &inbox, &format!("Postman lc keywords {run}"), &now, json!({})).await;
    let day: Date = "2026-10-29".parse().unwrap();
    let at = |h, m| When::at(day, Time::new(h, m, 0, 0).unwrap());
    let words: Vec<(Kind, When)> =
        vec![(Kind::Due, When::day(day)), (Kind::Wait, When::day(day)), (Kind::Remind, at(9, 0)), (Kind::Expire, at(23, 59))];
    let mut add = Vec::new();
    for (kind, when) in &words {
        let k = kind.keyword(*when);
        assert!(valid_keyword(&k), "{k}");
        add.push(k);
        add.push(kind.base().to_owned());
    }
    // Срок с часами — тоже.
    add.push(Kind::Due.keyword(at(15, 0)));
    let failed = c.set_keywords(std::slice::from_ref(&id), &add, &[]).await.unwrap();
    assert!(failed.is_empty(), "{failed:?}");
    let kws = keywords_on_server(&c, &id).await;
    println!("keywords on server: {kws:?}");
    for k in &add {
        assert!(kws.contains(k), "{k} не сохранилось как есть: {kws:?}");
    }
    // Разбор того, что вернул сервер, — те же сроки.
    assert_eq!(marks::find(&kws, Kind::Due).map(|w| w.date), Some(day));
    for (kind, when) in words.iter().filter(|(k, _)| *k != Kind::Due) {
        assert_eq!(marks::find(&kws, *kind), Some(*when), "{kind:?}");
    }
    for k in &add {
        let found = c.filtered(&Filter::HasKeyword(k.clone()), 10).await.unwrap();
        assert!(found.ids.contains(&id), "hasKeyword {k}");
    }

    // Заглавная `T` (как в ISO): RFC 8621 §4.1.1 — слова без учёта регистра, сервер хранит в нижнем; запишем, что делает.
    let upper = "pm.remind.2026-10-29T0900".to_owned();
    let failed = c.set_keywords(std::slice::from_ref(&id), std::slice::from_ref(&upper), &[]).await.unwrap();
    let kws = keywords_on_server(&c, &id).await;
    let reminds: Vec<&String> = kws.iter().filter(|k| k.starts_with("pm.remind.")).collect();
    println!("upper-case T: failed={failed:?}, stored={reminds:?}");
    // Двоеточие во времени (`09:00`) — тоже записать.
    let colon = "pm.remind.2026-10-29t09:00".to_owned();
    let failed = c.set_keywords(std::slice::from_ref(&id), std::slice::from_ref(&colon), &[]).await.unwrap();
    println!("colon: failed={failed:?}, stored={}", keywords_on_server(&c, &id).await.contains(&colon));

    // Снять всё одним патчем.
    let mut remove = add.clone();
    remove.extend([upper, colon]);
    let failed = c.set_keywords(std::slice::from_ref(&id), &[], &remove).await.unwrap();
    assert!(failed.is_empty(), "{failed:?}");
    let left = keywords_on_server(&c, &id).await;
    c.destroy_email(&id).await.unwrap();
    assert_eq!(left, ["$seen"], "{left:?}");
}

/// Рассылки: сырые `List-Id` / `List-Unsubscribe` в `Email/get` отличают их от обычных писем; в «Архив» — прочитанные
/// старше срока, кроме последней от отправителя. `before` отсекает свежие. «Напомнить»: в «Архив» и обратно
/// непрочитанным — одним патчем.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn newsletters_and_snooze_move() {
    let c = user1().await;
    sweep_leftovers(&c).await;
    let inbox = role(&c, "inbox").await;
    // У стенда «Архива» нет (`docs/STALWART.md`): Postman создаёт его сам — папку с ролью `archive`.
    let (archive, created) = match find_role(&c, "archive").await {
        Some(id) => (id, false),
        None => {
            let id = c.create_mailbox(&format!("Archive {}", stamp()), Some("archive")).await.unwrap();
            println!("archive created with role: {id}");
            (id, true)
        }
    };
    let archive_role = find_role(&c, "archive").await;
    let run = stamp();
    let news_from = format!("news-{run}@itt.test");
    let list = |tag: &str| format!("List-Id: Stand news <{tag}-{run}.itt.test>\r\nList-Unsubscribe: <mailto:u@itt.test>\r\n");
    let older = import(&c, &inbox, &news_from, &format!("Postman lc news 1 {run}"), "2026-01-05T08:00:00Z", &list("a")).await;
    let newer = import(&c, &inbox, &news_from, &format!("Postman lc news 2 {run}"), "2026-01-10T08:00:00Z", &list("a")).await;
    let unsub_from = format!("unsub-{run}@itt.test");
    let unsub =
        import(&c, &inbox, &unsub_from, &format!("Postman lc unsub {run}"), "2026-01-05T08:00:00Z", "List-Unsubscribe: <https://itt.test/u>\r\n")
            .await;
    let plain = create(&c, &inbox, &format!("Postman lc plain {run}"), "2026-01-05T08:00:00Z", json!({})).await;
    let now = jiff::Timestamp::now().strftime("%Y-%m-%dT%H:%M:%SZ").to_string();
    let fresh = import(&c, &inbox, &format!("fresh-{run}@itt.test"), &format!("Postman lc fresh {run}"), &now, &list("f")).await;
    let ours = [&older, &newer, &unsub, &plain, &fresh];

    // Сначала всё узнать, проверить — после уборки (упавший тест не оставляет писем и папок).
    let seen = Filter::And(vec![Filter::InMailbox(inbox.clone()), Filter::HasKeyword("$seen".into())]);
    let letters = c.list_letters(&seen, 500).await.unwrap();
    let flag = |id: &String| letters.iter().find(|l| &l.id == id).map(|l| l.list);
    let flags: Vec<Option<bool>> = ours.iter().map(|id| flag(id)).collect();
    println!("list flags (older, newer, unsub, plain, fresh): {flags:?}");
    let items: Vec<NewsItem> = letters
        .iter()
        .filter(|l| l.list)
        .map(|l| NewsItem { id: l.id.clone(), sender: l.from.clone(), received_at: l.received_at, seen: l.seen })
        .collect();
    let cutoff = jiff::Timestamp::from_second(1_769_904_000).unwrap(); // 2026-02-01
    let swept = newsletter_sweep(&items, cutoff.as_second());
    let before = c.filtered(&Filter::And(vec![Filter::InMailbox(inbox.clone()), Filter::Before("2026-02-01T00:00:00Z".into())]), 500).await.unwrap();

    // «Напомнить»: из «Входящих» в «Архив» и обратно непрочитанным — патч папок и ключевых слов одним `Email/set`.
    let remind = Kind::Remind.keyword(When::at("2026-10-29".parse().unwrap(), Time::new(9, 0, 0, 0).unwrap()));
    let hide = EmailPatch::default().mailbox(&inbox, false).mailbox(&archive, true).keyword(Kind::Remind.base(), true).keyword(&remind, true);
    let hidden = c.patch_emails(&[(plain.clone(), hide)]).await.unwrap();
    let (e, _) = c.email_get(std::slice::from_ref(&plain)).await.unwrap();
    let in_archive: Vec<String> = e[0].mailbox_ids.keys().cloned().collect();
    let back = EmailPatch::default().mailbox(&archive, false).mailbox(&inbox, true).keyword("$seen", false).keyword(&remind, false);
    let shown = c.patch_emails(&[(plain.clone(), back)]).await.unwrap();
    let (e, _) = c.email_get(std::slice::from_ref(&plain)).await.unwrap();
    let returned = e[0].clone();

    for id in ours {
        c.destroy_email(id).await.unwrap();
    }
    if created {
        c.destroy_mailbox(&archive).await.unwrap();
    }
    assert_eq!(archive_role.as_deref(), Some(archive.as_str()), "роль archive принята");
    assert_eq!(flags, [Some(true), Some(true), Some(true), Some(false), Some(true)]);
    assert!(swept.contains(&older), "старая рассылка — в архив");
    assert!(!swept.contains(&newer), "последняя от отправителя остаётся");
    assert!(!swept.contains(&unsub), "единственная от отправителя остаётся");
    assert!(!swept.contains(&fresh) && !swept.contains(&plain));
    assert!(before.ids.contains(&older) && !before.ids.contains(&fresh), "before");
    assert!(hidden.is_empty() && shown.is_empty(), "{hidden:?} {shown:?}");
    assert_eq!(in_archive, std::slice::from_ref(&archive));
    assert_eq!(returned.mailbox_ids.keys().collect::<Vec<_>>(), [&inbox]);
    assert!(!returned.has_keyword("$seen") && !returned.has_keyword(&remind) && returned.has_keyword(Kind::Remind.base()));
}

fn wait_for<T>(rx: &std::sync::mpsc::Receiver<sync::Event>, what: &str, pick: impl Fn(&sync::Event) -> Option<T>) -> T {
    let start = std::time::Instant::now();
    loop {
        let left = std::time::Duration::from_secs(45).checked_sub(start.elapsed()).unwrap_or_else(|| panic!("не дождались: {what}"));
        match rx.recv_timeout(left) {
            Ok(ev) => {
                if let Some(v) = pick(&ev) {
                    return v;
                }
                if let sync::Event::Error(e) = ev {
                    panic!("синхронизация ({what}): {e}");
                }
            }
            Err(e) => panic!("не дождались: {what} ({e})"),
        }
    }
}

/// Через поток синхронизации, как интерфейс: «Напомнить» (`Command::Patch` с «Архивом», которого может не быть —
/// [`lifecycle::ARCHIVE`]) и обратно; рассылки (`Command::Newsletters`) — старая уходит в «Архив», последняя от
/// отправителя остаётся.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn patch_and_newsletters_through_sync() {
    let c = user1().await;
    sweep_leftovers(&c).await;
    let had_archive = find_role(&c, "archive").await;
    let inbox = role(&c, "inbox").await;
    let run = stamp();
    let letter = create(&c, &inbox, &format!("Postman lc sync {run}"), "2026-01-05T08:00:00Z", json!({})).await;
    let from = format!("sync-news-{run}@itt.test");
    let list = format!("List-Unsubscribe: <mailto:u-{run}@itt.test>\r\n");
    let older = import(&c, &inbox, &from, &format!("Postman lc sync news 1 {run}"), "2026-01-05T08:00:00Z", &list).await;
    let newer = import(&c, &inbox, &from, &format!("Postman lc sync news 2 {run}"), "2026-01-06T08:00:00Z", &list).await;

    let cache = postman_cache::Cache::open_in_memory().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let auth = Auth::Basic { user: env("POSTMAN_STAND_USER1"), password: env("POSTMAN_STAND_PASSWORD1") };
    let commands = postman_backend_jmap::spawn(
        sync::Account { url: env("POSTMAN_STAND_URL"), auth: postman_backend_jmap::convert::FromJmap::from_jmap(auth) },
        cache.clone(),
        None,
        tx,
        || {},
    );
    wait_for(&rx, "первая синхронизация", |e| matches!(e, sync::Event::Status(sync::Status::Synced)).then_some(()));
    let row = cache.rows_by_ids(std::slice::from_ref(&letter)).unwrap().pop().expect("письмо в кэше");
    let when = When::at("2026-12-01".parse().unwrap(), Time::new(9, 0, 0, 0).unwrap());
    let roles = lifecycle::Roles { inbox: Some(inbox.clone()), ..lifecycle::Roles::default() };
    let snooze = lifecycle::snooze(&row, when, &roles);
    assert!(snooze.mailboxes.contains(&(lifecycle::ARCHIVE.to_owned(), true)));
    let name = format!("Archive {}", stamp());
    commands.send(sync::Command::Patch { patches: vec![snooze], archive_name: name }).unwrap();
    let start = std::time::Instant::now();
    let archive = loop {
        let (e, _) = c.email_get(std::slice::from_ref(&letter)).await.unwrap();
        if !e[0].mailbox_ids.contains_key(&inbox) {
            break e[0].mailbox_ids.keys().next().cloned().unwrap();
        }
        assert!(start.elapsed().as_secs() < 30, "письмо не ушло из «Входящих»");
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    };
    let kws = keywords_on_server(&c, &letter).await;
    assert_eq!(find_role(&c, "archive").await.as_deref(), Some(archive.as_str()), "в «Архиве»");
    assert!(kws.contains(&"pm.remind".to_owned()) && kws.contains(&"pm.remind.2026-12-01t0900".to_owned()), "{kws:?}");

    let cutoff = jiff::Timestamp::from_second(1_769_904_000).unwrap().as_second(); // 2026-02-01
    commands.send(sync::Command::Newsletters { cutoff, archive_name: "Archive".into() }).unwrap();
    let archived = wait_for(&rx, "рассылки", |e| match e {
        sync::Event::Newsletters { archived } => Some(*archived),
        _ => None,
    });
    let (e, _) = c.email_get(&[older.clone(), newer.clone()]).await.unwrap();
    let place = |id: &str| e.iter().find(|x| x.id == id).map(|x| x.mailbox_ids.contains_key(&archive));
    let (older_archived, newer_archived) = (place(&older), place(&newer));
    drop(commands);
    for id in [&letter, &older, &newer] {
        c.destroy_email(id).await.unwrap();
    }
    if had_archive.is_none() {
        c.destroy_mailbox(&archive).await.unwrap();
    }
    println!("newsletters archived: {archived}");
    assert!(archived >= 1);
    assert_eq!((older_archived, newer_archived), (Some(true), Some(false)), "старая — в «Архиве», последняя — нет");
}

/// Находка для `docs/STALWART-GAPS.md`: условие `header` (RFC 8621 §4.4.1) в `Email/query` не находит ни одного
/// письма — ни по имени (`List-Unsubscribe`, `Subject`), ни по имени и значению; `header:List-Id:asText` — `null`,
/// хотя сырой `header:List-Id` есть. Тест печатает, что делает сервер, и проверяет только сырой заголовок.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn header_filter_finds_nothing() {
    let c = user1().await;
    let inbox = role(&c, "inbox").await;
    let run = stamp();
    let headers = format!("List-Id: Probe <probe-{run}.itt.test>\r\nList-Unsubscribe: <mailto:u@itt.test>\r\nX-Probe: yes\r\n");
    let id = import(&c, &inbox, "probe@itt.test", &format!("Postman lc probe {run}"), "2026-01-05T08:00:00Z", &headers).await;
    let acc = c.mail_account().unwrap().to_owned();
    // Полнотекстовый индекс Stalwart строится в фоне: подождать.
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    let mut hits = Vec::new();
    for f in [
        json!({ "header": ["List-Unsubscribe"] }),
        json!({ "header": ["List-Unsubscribe", "mailto"] }),
        json!({ "header": ["List-Id"] }),
        json!({ "header": ["X-Probe", "yes"] }),
        json!({ "header": ["Subject"] }),
        json!({ "header": ["Subject", "probe"] }),
    ] {
        let args = json!({ "accountId": acc, "filter": { "operator": "AND", "conditions": [{ "inMailbox": inbox }, f] }, "limit": 50 });
        let r = c.call(&[CORE, MAIL], &[Invocation("Email/query".into(), args, "q".into())]).await.unwrap();
        let hit = r[0].1["ids"].as_array().is_some_and(|a| a.iter().any(|x| x.as_str() == Some(id.as_str())));
        println!("filter {f}: hit={hit}");
        hits.push(hit);
    }
    let props = json!(["header:List-Id", "header:List-Id:asText", "header:X-Probe:asText"]);
    let get = json!({ "accountId": acc, "ids": [&id], "properties": props });
    let r = c.call(&[CORE, MAIL], &[Invocation("Email/get".into(), get, "g".into())]).await.unwrap();
    let got = r[0].1["list"][0].clone();
    println!("get: {got}");
    c.destroy_email(&id).await.unwrap();
    assert!(got["header:List-Id"].as_str().is_some_and(|v| v.contains("probe")), "сырой заголовок есть");
    println!("header filter hits: {} of {}", hits.iter().filter(|h| **h).count(), hits.len());
}
