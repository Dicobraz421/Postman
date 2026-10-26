//! Метки и категории на ключевых словах (S5-02) против живого Stalwart: user3 ставит и снимает категории у письма
//! патчем `keywords/<слово>` (одним `Email/set`), ищет по `hasKeyword`/`notKeyword`, считает «N из M» (`limit: 0`);
//! то же — через поток синхронизации (`SetKeywords`, `Query`, `Counts`); таблица категорий помещается в файл
//! JMAP FileNode (кандидат на хранение таблицы на сервере, `docs/specs/S5.md`).
//!
//! Письмо создаётся прямо в «Входящих» (`Email/set create`, без SMTP — ограничение входящих на получателя не
//! задевается, `docs/STALWART.md` п. 16) и в конце удаляется. Без стенда тесты пропускаются (`#[ignore]`).
//!
//! Запуск: `POSTMAN_STAND_URL`, `POSTMAN_STAND_USER3`, `POSTMAN_STAND_PASSWORD3` (и `POSTMAN_INSECURE_TLS=1` напрямую),
//! `cargo test -p postman-backend-jmap --test stand_labels -- --ignored`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)

use std::time::{Duration, Instant};

use postman_cache::Cache;
use postman_core::labels::{self, Keyword};
use postman_core::sync::{Account, Command, Event, Status};
use postman_jmap::{Auth, CORE, Client, Filter, Invocation, MAIL};
use serde_json::{Value, json};

/// Сколько ждать синхронизацию.
const WAIT: Duration = Duration::from_secs(30);
const FILENODE: &str = "urn:ietf:params:jmap:filenode";

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} не задан: стенд — docs/STALWART.md"))
}

fn auth() -> Auth {
    Auth::Basic { user: env("POSTMAN_STAND_USER3"), password: env("POSTMAN_STAND_PASSWORD3") }
}

async fn user3() -> Client {
    Client::connect(&env("POSTMAN_STAND_URL"), auth()).await.unwrap_or_else(|e| panic!("вход user3: {e}"))
}

fn stamp() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    format!("{}{:03}", t.as_secs(), t.subsec_millis())
}

async fn inbox(c: &Client) -> String {
    let (boxes, _) = c.mailboxes().await.unwrap();
    boxes.into_iter().find(|m| m.role.as_deref() == Some("inbox")).map(|m| m.id).expect("нет «Входящих»")
}

/// Письмо во «Входящих» user3 без доставки: сервер сам собирает MIME (`Email/set create`).
async fn create_letter(c: &Client, inbox: &str, subject: &str) -> String {
    let acc = c.mail_account().unwrap().to_owned();
    let now = jiff::Timestamp::now().strftime("%Y-%m-%dT%H:%M:%SZ").to_string();
    let email = json!({
        "mailboxIds": { inbox: true },
        "keywords": { "$seen": true },
        "receivedAt": now,
        "from": [{ "name": "Stand", "email": "stand@itt.test" }],
        "to": [{ "email": env("POSTMAN_STAND_USER3") }],
        "subject": subject,
        "bodyValues": { "t": { "value": "Метки и категории (S5-02)." } },
        "textBody": [{ "partId": "t", "type": "text/plain" }],
    });
    let calls = [Invocation("Email/set".into(), json!({ "accountId": acc, "create": { "n": email } }), "s".into())];
    let r = c.call(&[CORE, MAIL], &calls).await.unwrap();
    r[0].1["created"]["n"]["id"].as_str().unwrap_or_else(|| panic!("письмо не создано: {}", r[0].1)).to_owned()
}

async fn keywords_on_server(c: &Client, id: &str) -> Vec<String> {
    let (emails, _) = c.email_get(&[id.to_owned()]).await.unwrap();
    let mut out: Vec<String> = emails[0].keywords.iter().filter(|(_, on)| **on).map(|(k, _)| k.clone()).collect();
    out.sort();
    out
}

fn s(list: &[&str]) -> Vec<String> {
    list.iter().map(|x| x.to_string()).collect()
}

/// Категории ставятся и снимаются патчем, другие ключевые слова не трогаются; поиск по `hasKeyword` и
/// `notKeyword`, счётчики с `limit: 0`.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn categories_are_set_queried_and_cleared() {
    let c = user3().await;
    let inbox = inbox(&c).await;
    let run = stamp();
    let mine = labels::category_keyword(&format!("stand-{run}"));
    let other = labels::category_keyword("clients");
    // `~` и `/` в ключевом слове — экранирование пути патча (JSON Pointer).
    let odd = format!("pm~stand/{run}");
    assert!(labels::valid_keyword(&mine) && labels::valid_keyword(&odd));
    let id = create_letter(&c, &inbox, &format!("Postman labels {run}")).await;

    let failed = c.set_keywords(std::slice::from_ref(&id), &s(&[&mine, &other, &odd, "$label2"]), &[]).await.unwrap();
    assert!(failed.is_empty(), "{failed:?}");
    let kws = keywords_on_server(&c, &id).await;
    let mut want = s(&["$label2", "$seen", &mine, &other, &odd]);
    want.sort();
    assert_eq!(kws, want, "сервер хранит слова как есть, $seen на месте");
    assert_eq!(labels::classify(&mine), Keyword::Category(&mine["pm.cat.".len()..]));

    let found = c.filtered(&Filter::And(vec![Filter::InMailbox(inbox.clone()), Filter::HasKeyword(mine.clone())]), 10).await.unwrap();
    assert_eq!((found.ids.as_slice(), found.total), (std::slice::from_ref(&id), 1));
    // «Все папки» — без inMailbox.
    let everywhere = c.filtered(&Filter::HasKeyword(mine.clone()), 10).await.unwrap();
    assert_eq!(everywhere.ids, std::slice::from_ref(&id));
    let odd_found = c.filtered(&Filter::HasKeyword(odd.clone()), 10).await.unwrap();
    assert_eq!(odd_found.ids, std::slice::from_ref(&id), "слово с ~ и / ищется");
    let total = c.count(&[Filter::InMailbox(inbox.clone())]).await.unwrap()[0];
    let in_inbox = |f: Filter| Filter::And(vec![Filter::InMailbox(inbox.clone()), f]);
    let counts = c.count(&[in_inbox(Filter::HasKeyword(mine.clone())), in_inbox(Filter::NotKeyword(mine.clone()))]).await.unwrap();
    assert_eq!(counts, [1, total - 1], "«N из M»: {total} во «Входящих»");
    println!("inbox {total}, with {mine}: {}", counts[0]);

    // Снять одну категорию — остальные слова остаются.
    let failed = c.set_keywords(std::slice::from_ref(&id), &[], &s(&[&mine, &odd])).await.unwrap();
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(keywords_on_server(&c, &id).await, s(&["$label2", "$seen", &other]));
    assert!(c.filtered(&Filter::HasKeyword(mine.clone()), 10).await.unwrap().ids.is_empty());
    // Несуществующее письмо — в ответе, а не ошибкой запроса.
    let failed = c.set_keywords(&s(&["zzzzzz"]), &s(&[&mine]), &[]).await.unwrap();
    assert_eq!(failed.len(), 1, "{failed:?}");
    println!("unknown id: {}", failed[0].1);

    c.destroy_email(&id).await.unwrap();
}

type Events = std::sync::mpsc::Receiver<Event>;

fn wait_for<T>(rx: &Events, what: &str, pick: impl Fn(&Event) -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        let left = WAIT.checked_sub(start.elapsed()).unwrap_or_else(|| panic!("не дождались: {what}"));
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

/// Через поток синхронизации: кэш уже поменян интерфейсом, `SetKeywords` доводит метку до сервера; push
/// возвращает письмо в кэш с ней же; `Query` и `Counts` — по серверу.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn categories_through_sync() {
    let c = user3().await;
    let inbox = inbox(&c).await;
    let run = stamp();
    let mine = labels::category_keyword(&format!("sync-{run}"));
    let subject = format!("Postman labels sync {run}");
    let id = create_letter(&c, &inbox, &subject).await;

    let cache = Cache::open_in_memory().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let commands = postman_backend_jmap::spawn(
        Account { url: env("POSTMAN_STAND_URL"), auth: postman_backend_jmap::convert::FromJmap::from_jmap(auth()) },
        cache.clone(),
        None,
        tx,
        || {},
    );
    wait_for(&rx, "первая синхронизация", |e| matches!(e, Event::Status(Status::Synced)).then_some(()));
    // Свой аккаунт — ключи без префикса.
    let start = Instant::now();
    while cache.rows_by_ids(std::slice::from_ref(&id)).unwrap().is_empty() {
        assert!(start.elapsed() < WAIT, "письмо не пришло в кэш");
        let _ = rx.recv_timeout(Duration::from_millis(300));
    }

    // Как интерфейс: сначала кэш, затем команда.
    let row = cache.set_keywords(&id, std::slice::from_ref(&mine), &[]).unwrap().unwrap();
    assert!(row.has_keyword(&mine));
    assert_eq!(cache.keyword_count(&mine, &inbox).unwrap(), 1);
    assert!(commands.send(Command::SetKeywords { ids: vec![id.clone()], add: vec![mine.clone()], remove: vec![] }).is_ok());
    let start = Instant::now();
    while !keywords_on_server(&c, &id).await.contains(&mine) {
        assert!(start.elapsed() < WAIT, "метка не дошла до сервера");
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    assert!(
        commands
            .send(Command::Query { filter: postman_core::model::Filter::HasKeyword(mine.clone()), mailbox: Some(inbox.clone()), ticket: 7 })
            .is_ok()
    );
    let (ids, total) = wait_for(&rx, "письма по метке", |e| match e {
        Event::SearchResults { ticket: 7, ids, total } => Some((ids.clone(), *total)),
        _ => None,
    });
    assert_eq!((ids, total), (vec![id.clone()], 1));
    let filters = vec![postman_core::model::Filter::HasKeyword(mine.clone()), postman_core::model::Filter::NotKeyword(mine.clone())];
    assert!(commands.send(Command::Counts { filters, mailbox: None, ticket: 8 }).is_ok());
    let counts = wait_for(&rx, "счётчики", |e| match e {
        Event::Counts { ticket: 8, counts } => Some(counts.clone()),
        _ => None,
    });
    let counts = counts.expect("счётчики не посчитались");
    assert_eq!(counts[0], 1, "{counts:?}");

    // Снять — и в кэше, и на сервере; push возвращает письмо без метки.
    cache.set_keywords(&id, &[], std::slice::from_ref(&mine)).unwrap();
    assert!(commands.send(Command::SetKeywords { ids: vec![id.clone()], add: vec![], remove: vec![mine.clone()] }).is_ok());
    let start = Instant::now();
    while keywords_on_server(&c, &id).await.contains(&mine) {
        assert!(start.elapsed() < WAIT, "метка не снялась на сервере");
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    assert_eq!(cache.keyword_count(&mine, &inbox).unwrap(), 0);

    drop(commands);
    c.destroy_email(&id).await.unwrap();
}

/// Таблица категорий как файл JMAP FileNode в аккаунте user3: загрузка blob, `FileNode/set create`, чтение,
/// удаление. Кандидат на хранение таблицы на сервере (своя у общего ящика — FileNode есть и в его аккаунте).
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn category_table_fits_in_a_file_node() {
    let c = user3().await;
    assert!(c.has(FILENODE), "сервер не объявил {FILENODE}");
    let acc = c.mail_account().unwrap().to_owned();
    let table = labels::Book::own(labels::Categories::defaults(|id| format!("Категория {id}")));
    let data = serde_json::to_vec(&table).unwrap();
    let blob = c.upload(&acc, data.clone(), "application/json").await.unwrap();
    let name = format!("postman-categories-{}.json", stamp());
    let using = [CORE, FILENODE];
    let create = json!({ "accountId": acc, "create": { "n": { "name": name, "parentId": null, "blobId": blob, "type": "application/json" } } });
    let r = c.call(&using, &[Invocation("FileNode/set".into(), create, "s".into())]).await.unwrap();
    let node = r[0].1["created"]["n"]["id"].as_str().unwrap_or_else(|| panic!("узел не создан: {}", r[0].1)).to_owned();
    let get = json!({ "accountId": acc, "ids": [node] });
    let r = c.call(&using, &[Invocation("FileNode/get".into(), get, "g".into())]).await.unwrap();
    let got: &Value = &r[0].1["list"][0];
    assert_eq!(got["name"], json!(name));
    let stored = c.download(&acc, got["blobId"].as_str().unwrap(), &name, "application/json").await.unwrap();
    assert_eq!(serde_json::from_slice::<labels::Book>(&stored).unwrap(), table, "таблица читается обратно");
    let destroy = json!({ "accountId": acc, "destroy": [node] });
    let r = c.call(&using, &[Invocation("FileNode/set".into(), destroy, "d".into())]).await.unwrap();
    assert_eq!(r[0].1["destroyed"], json!([node]));
}
