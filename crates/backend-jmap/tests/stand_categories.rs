//! Таблица категорий на сервере (S5-02б) против живого Stalwart: файл JMAP FileNode `.postman/categories.json`
//! user3 — запись, чтение второй сессией, одновременные правки двух клиентов (у категории побеждает поздняя),
//! push другому клиенту через поток синхронизации; своя таблица у общего ящика `shared-sales@itt.test` (user3 —
//! участник); снятие категории с писем. В конце таблицы возвращаются как были (не было — папка удаляется).
//!
//! Запуск: `POSTMAN_STAND_URL`, `POSTMAN_STAND_USER3`, `POSTMAN_STAND_PASSWORD3` (и `POSTMAN_INSECURE_TLS=1` напрямую),
//! `cargo test -p postman-backend-jmap --test stand_categories -- --ignored --test-threads 1` (тесты пишут один и тот же файл).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)

use std::time::{Duration, Instant};

use postman_cache::Cache;
use postman_core::categories::{FILE, FOLDER, Reply, Request};
use postman_core::labels::Categories;
use postman_core::sync::{Account, Command, Event, Status};
use postman_jmap::{Auth, CORE, Client, Filter, Invocation, MAIL, Written};
use serde_json::json;

const WAIT: Duration = Duration::from_secs(30);
const SHARED: &str = "shared-sales@itt.test";

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} не задан: стенд — docs/STALWART.md"))
}

fn auth() -> Auth {
    Auth::Basic { user: env("POSTMAN_STAND_USER3"), password: env("POSTMAN_STAND_PASSWORD3") }
}

/// Новая сессия user3 — как второй клиент.
async fn user3() -> Client {
    Client::connect(&env("POSTMAN_STAND_URL"), auth()).await.unwrap_or_else(|e| panic!("вход user3: {e}"))
}

fn own(c: &Client) -> String {
    c.mail_account().unwrap().to_owned()
}

fn shared(c: &Client) -> String {
    c.session().shared_accounts().into_iter().find(|a| a.name == SHARED).map(|a| a.id).expect("нет ящика shared-sales")
}

fn defaults(tag: &str) -> Categories {
    Categories::defaults(|id| format!("{tag} {id}"))
}

/// Что было на сервере до теста: содержимое файла таблицы (или `None` — файла не было) и была ли папка.
struct Before {
    folder: bool,
    file: Option<Vec<u8>>,
}

async fn before(c: &Client, acc: &str) -> Before {
    let (folder, _) = c.file_node(acc, None, FOLDER).await.unwrap();
    let Some(folder) = folder else {
        return Before { folder: false, file: None };
    };
    let (file, _) = c.file_node(acc, Some(&folder.id), FILE).await.unwrap();
    let file = match file.and_then(|f| f.blob_id) {
        Some(blob) => Some(c.download(acc, &blob, FILE, "application/json").await.unwrap()),
        None => None,
    };
    Before { folder: true, file }
}

/// Вернуть как было: папки не было — удалить; файл был — записать прежнее содержимое; не было — удалить файл.
async fn restore(c: &Client, acc: &str, was: Before) {
    let (folder, _) = c.file_node(acc, None, FOLDER).await.unwrap();
    let Some(folder) = folder else { return };
    if !was.folder {
        c.destroy_file_nodes(acc, &[folder.id]).await.unwrap();
        return;
    }
    let (file, _) = c.file_node(acc, Some(&folder.id), FILE).await.unwrap();
    match (was.file, file) {
        (Some(data), file) => {
            let blob = c.upload(acc, data, "application/json").await.unwrap();
            let id = file.map(|f| f.id);
            let w = c.write_file(acc, &folder.id, id.as_deref(), FILE, &blob, "application/json", None).await.unwrap();
            assert!(matches!(w, Written::Done { .. }), "{w:?}");
        }
        (None, Some(file)) => c.destroy_file_nodes(acc, &[file.id]).await.unwrap(),
        (None, None) => {}
    }
}

fn name(t: &Categories, keyword: &str) -> Option<String> {
    t.by_keyword(keyword).map(|(_, c)| c.name.clone())
}

/// Запись и чтение второй сессией; правки двух клиентов от одной таблицы — последовательно и одновременно
/// (`ifInState`: второй получает `stateMismatch`, перечитывает и сводит); устаревшее состояние — `Stale`.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn table_is_shared_between_sessions_last_writer_wins() {
    let a = user3().await;
    let b = user3().await;
    let acc = own(&a);
    assert!(a.has_files(&acc), "у аккаунта нет FileNode");
    let was = before(&a, &acc).await;

    // Чистый лист: таблицы нет.
    if was.folder {
        let (folder, _) = a.file_node(&acc, None, FOLDER).await.unwrap();
        a.destroy_file_nodes(&acc, &[folder.unwrap().id]).await.unwrap();
    }
    assert_eq!(postman_backend_jmap::categories::load(&a, &acc).await.unwrap(), None, "таблицы нет — умолчание");

    // Первая запись создаёт папку и файл; вторая сессия читает то же.
    let base = defaults("Стенд");
    let saved = postman_backend_jmap::categories::save(&a, &acc, &base).await.unwrap();
    assert_eq!(saved, base);
    assert_eq!(postman_backend_jmap::categories::load(&b, &acc).await.unwrap().as_ref(), Some(&base), "вторая сессия читает таблицу");
    let (folder, _) = a.file_node(&acc, None, FOLDER).await.unwrap();
    let folder = folder.expect("папка .postman создана");
    assert_eq!((folder.parent_id.as_deref(), folder.blob_id.as_deref()), (None, None), "папка в корне");
    // Корень — `isTopLevel: true`: папка с тем же именем внутри `.postman` в ответ не попадает.
    let (nested, _) = a.create_folder(&acc, Some(&folder.id), FOLDER).await.unwrap();
    let (top, _) = a.file_node(&acc, None, FOLDER).await.unwrap();
    assert_eq!(top.map(|n| n.id), Some(folder.id.clone()), "в корне — только верхняя папка");
    a.destroy_file_nodes(&acc, &[nested]).await.unwrap();

    // Последовательно: A правит team позже, B (со старой копией) — tenders и team раньше.
    let mut ta = base.clone();
    ta.rename("pm.cat.team", "A team", 2_000).unwrap();
    let mut tb = base.clone();
    tb.rename("pm.cat.tenders", "B tenders", 3_000).unwrap();
    tb.rename("pm.cat.team", "B team", 1_000).unwrap();
    postman_backend_jmap::categories::save(&a, &acc, &ta).await.unwrap();
    let merged = postman_backend_jmap::categories::save(&b, &acc, &tb).await.unwrap();
    let on_server = postman_backend_jmap::categories::load(&a, &acc).await.unwrap().unwrap();
    assert_eq!(on_server, merged);
    assert_eq!(name(&on_server, "pm.cat.team").as_deref(), Some("A team"), "у team позже правил A");
    assert_eq!(name(&on_server, "pm.cat.tenders").as_deref(), Some("B tenders"));

    // Одновременно: обе записи читают одно состояние, одна из них получает stateMismatch и сводит заново.
    let mut ca = on_server.clone();
    let ka = ca.add("Стенд A", "seafoam", Some(6), 4_000).unwrap();
    let mut cb = on_server.clone();
    cb.delete("pm.cat.services", 4_100).unwrap();
    cb.rename("pm.cat.pentest", "B pentest", 4_200).unwrap();
    let (ra, rb) = tokio::join!(postman_backend_jmap::categories::save(&a, &acc, &ca), postman_backend_jmap::categories::save(&b, &acc, &cb));
    ra.unwrap();
    rb.unwrap();
    let last = postman_backend_jmap::categories::load(&a, &acc).await.unwrap().unwrap();
    assert_eq!(name(&last, &ka).as_deref(), Some("Стенд A"), "добавленная A на месте: {last:?}");
    assert_eq!(name(&last, "pm.cat.services"), None, "удалённая B скрыта");
    assert_eq!(name(&last, "pm.cat.pentest").as_deref(), Some("B pentest"));
    assert_eq!(name(&last, "pm.cat.team").as_deref(), Some("A team"));
    assert_eq!(last.by_key(6).map(|c| c.keyword.clone()), Some(ka.clone()));
    println!("concurrent: {} categories, {} visible", last.list.len(), last.visible().count());

    // Запись с устаревшим состоянием — Stale, файл не тронут.
    let (file, state) = a.file_node(&acc, Some(&folder.id), FILE).await.unwrap();
    let file = file.unwrap();
    let blob = a.upload(&acc, b"{}".to_vec(), "application/json").await.unwrap();
    let mut other = last.clone();
    other.rename("pm.cat.team", "bump", 5_000).unwrap();
    postman_backend_jmap::categories::save(&b, &acc, &other).await.unwrap();
    let w = a.write_file(&acc, &folder.id, Some(&file.id), FILE, &blob, "application/json", Some(&state)).await.unwrap();
    assert_eq!(w, Written::Stale);
    assert_eq!(name(&postman_backend_jmap::categories::load(&a, &acc).await.unwrap().unwrap(), "pm.cat.team").as_deref(), Some("bump"));
    // Создать файл, который уже есть, — alreadyExists с его id.
    let w = a.write_file(&acc, &folder.id, None, FILE, &blob, "application/json", None).await.unwrap();
    assert_eq!(w, Written::Exists(file.id.clone()));

    restore(&a, &acc, was).await;
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

/// Через поток синхронизации: таблицы своего аккаунта и общего ящика — при входе (`Loaded`), запись командой
/// (`Saved`), правка другого клиента приходит push (`FileNode` в `StateChange`) → `Loaded` с ней. У общего ящика
/// таблица своя.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn tables_through_sync_and_shared_mailbox() {
    let other = user3().await;
    let acc = own(&other);
    let group = shared(&other);
    assert!(other.has_files(&group), "у общего ящика нет FileNode");
    let was_own = before(&other, &acc).await;
    let was_group = before(&other, &group).await;
    let mine = defaults("Свои");
    postman_backend_jmap::categories::save(&other, &acc, &mine).await.unwrap();
    // Таблица общего ящика — своя: другое имя у clients.
    let mut theirs = defaults("Отдел");
    theirs.rename("pm.cat.clients", "Клиенты отдела", 10).unwrap();
    let group_saved = postman_backend_jmap::categories::save(&other, &group, &theirs).await.unwrap();

    let cache = Cache::open_in_memory().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let commands = postman_backend_jmap::spawn(
        Account { url: env("POSTMAN_STAND_URL"), auth: postman_backend_jmap::convert::FromJmap::from_jmap(auth()) },
        cache,
        None,
        tx,
        || {},
    );
    // При входе — ответ на каждый аккаунт: свой, общий ящик и папка shared-ro, открытая user3 (у неё файлы
    // закрыты — `Unsupported`).
    let readonly = other.session().shared_accounts().into_iter().find(|a| a.name == "shared-ro@itt.test").map(|a| a.id);
    let mut replies = Vec::new();
    while replies.len() < 2 + usize::from(readonly.is_some()) {
        replies.push(wait_for(&rx, "таблицы при входе", |e| match e {
            Event::Categories(r) => Some(r.clone()),
            _ => None,
        }));
    }
    let table_of = |want: Option<String>| {
        replies.iter().find_map(|r| match r {
            Reply::Loaded { account, table } if *account == want => Some(table.clone()),
            _ => None,
        })
    };
    let own_table = table_of(None).unwrap_or_else(|| panic!("своей таблицы нет: {replies:?}"));
    let group_table = table_of(Some(group.clone())).unwrap_or_else(|| panic!("таблицы ящика нет: {replies:?}"));
    if let Some(ro) = readonly {
        assert!(replies.contains(&Reply::Unsupported { account: Some(ro) }), "{replies:?}");
    }
    assert_eq!(own_table.as_ref().and_then(|t| name(t, "pm.cat.clients")).as_deref(), Some("Свои clients"));
    assert_eq!(group_table.as_ref(), Some(&group_saved));
    wait_for(&rx, "первая синхронизация", |e| matches!(e, Event::Status(Status::Synced)).then_some(()));

    // Запись командой: ответ — таблица после сведения.
    let mut edit = own_table.unwrap();
    edit.rename("pm.cat.team", "Команда (sync)", 20).unwrap();
    let request = Request::Save { account: None, table: edit.clone() };
    assert!(commands.send(Command::Categories(request)).is_ok());
    let saved = wait_for(&rx, "Saved", |e| match e {
        Event::Categories(Reply::Saved { account: None, table }) => Some(table.clone()),
        Event::Categories(Reply::Failed { error, .. }) => panic!("запись: {error}"),
        _ => None,
    });
    assert_eq!(saved, edit);

    // Правка другого клиента — push → Loaded с ней.
    let mut remote = saved.clone();
    remote.rename("pm.cat.tenders", "Тендеры (другой клиент)", 30).unwrap();
    let start = Instant::now();
    postman_backend_jmap::categories::save(&other, &acc, &remote).await.unwrap();
    let pushed = wait_for(&rx, "push таблицы", |e| match e {
        Event::Categories(Reply::Loaded { account: None, table: Some(t) })
            if name(t, "pm.cat.tenders").as_deref() == Some("Тендеры (другой клиент)") =>
        {
            Some(t.clone())
        }
        _ => None,
    });
    println!("push of the table: {} ms", start.elapsed().as_millis());
    assert_eq!(pushed, remote);
    // Общий ящик — та же правка его таблицы через команду с его аккаунтом.
    let mut g = group_saved.clone();
    g.set_key("pm.cat.clients", Some(9), 40).unwrap();
    assert!(commands.send(Command::Categories(Request::Save { account: Some(group.clone()), table: g })).is_ok());
    let gs = wait_for(&rx, "Saved общего ящика", |e| match e {
        Event::Categories(Reply::Saved { account: Some(a), table }) if *a == group => Some(table.clone()),
        Event::Categories(Reply::Failed { error, .. }) => panic!("запись общего ящика: {error}"),
        _ => None,
    });
    assert_eq!(gs.by_key(9).map(|c| c.name.as_str()), Some("Клиенты отдела"));
    assert_eq!(postman_backend_jmap::categories::load(&other, &acc).await.unwrap().unwrap().by_key(9), None, "своя таблица не тронута");

    drop(commands);
    restore(&other, &acc, was_own).await;
    restore(&other, &group, was_group).await;
}

/// Удаление категории «и с писем» (явный выбор): слово снимается со всех писем аккаунта, другие слова остаются.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn purge_removes_the_keyword_from_messages() {
    let c = user3().await;
    let acc = own(&c);
    let (boxes, _) = c.mailboxes().await.unwrap();
    let inbox = boxes.into_iter().find(|m| m.role.as_deref() == Some("inbox")).map(|m| m.id).unwrap();
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
    let kw = format!("pm.cat.purge-{t}");
    let email = |n: u32| {
        json!({
            "mailboxIds": { &inbox: true },
            "keywords": { "$seen": true, &kw: true },
            "from": [{ "email": "stand@itt.test" }],
            "subject": format!("Postman purge {t} {n}"),
            "bodyValues": { "t": { "value": "S5-02б" } },
            "textBody": [{ "partId": "t", "type": "text/plain" }],
        })
    };
    let create = json!({ "accountId": acc, "create": { "a": email(1), "b": email(2) } });
    let r = c.call(&[CORE, MAIL], &[Invocation("Email/set".into(), create, "s".into())]).await.unwrap();
    let ids: Vec<String> = ["a", "b"].iter().map(|k| r[0].1["created"][k]["id"].as_str().unwrap().to_owned()).collect();

    let replies = std::sync::Mutex::new(Vec::new());
    let request = Request::Purge { account: None, keyword: kw.clone(), ticket: 1 };
    postman_backend_jmap::categories::handle(&c, &acc, None, request, &|r| replies.lock().unwrap().push(r)).await;
    let replies = replies.into_inner().unwrap();
    assert_eq!(replies, [Reply::Purged { account: None, keyword: kw.clone(), ticket: 1, count: 2 }]);
    assert!(c.filtered(&Filter::HasKeyword(kw.clone()), 10).await.unwrap().ids.is_empty());
    let (emails, _) = c.email_get(&ids).await.unwrap();
    assert!(emails.iter().all(|e| e.keywords.get("$seen") == Some(&true)), "другие слова на месте");
    for id in &ids {
        c.destroy_email(id).await.unwrap();
    }
}
