//! Перенос между учётками (S8-13) на подменном сервере JMAP: шаги задания ядра (`postman_core::migrate::Engine`) —
//! выгрузка (`uploadUrl` и `Email/import`), перенос сервером между аккаунтами сессии (`Email/copy`), письмо целиком
//! (блоб `Email.blobId`) и удаление у источника (`Email/set` `destroy`) — через плагин JMAP тем же договором команд и
//! событий. Сервер — HTTP без TLS в потоке теста: сессия с двумя аккаунтами (свой и общий ящик), письма — в памяти.
//! Адреса и письма — синтетические. Стенд Stalwart 1.0 — `stand_migrate.rs` (`#[ignore]`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // тест: «не падать» — для кода приложения (S2-03)

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use postman_cache::{Cache, FolderRow, LetterRow, Owner, Place};
use postman_core::migrate::Engine;
use postman_core::model::{Auth, Features, FolderId, JobId, LetterId, MigrationMode, MigrationRequest, MigrationState};
use postman_core::store::CacheStore;
use postman_core::sync::{Account, Command, Event, Route, Status, Target, UnboundedSender};
use serde_json::{Value, json};

/// Письмо подменного сервера.
#[derive(Clone)]
struct Mail {
    id: String,
    blob: String,
    mailbox: String,
    keywords: Vec<String>,
    message_id: String,
}

/// Подменный сервер JMAP: блобы, письма по аккаунтам и сбои по номеру `Email/import`.
#[derive(Default)]
struct Server {
    blobs: HashMap<String, Vec<u8>>,
    mail: HashMap<String, Vec<Mail>>,
    next: u64,
    imports: u64,
    /// На этом `Email/import` ящик полон.
    quota_at: u64,
    /// Этот `Email/import` выполнен, ответа нет: соединение закрыто.
    lost_at: u64,
    /// Вызванные методы по порядку.
    methods: Vec<String>,
}

type Shared = Arc<Mutex<Server>>;

impl Server {
    fn id(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{prefix}{}", self.next)
    }

    /// Письмо в аккаунте `account` (как будто пришло раньше): блоб с письмом целиком.
    fn put(&mut self, account: &str, raw: &[u8]) -> String {
        let (id, blob) = (self.id("E"), self.id("B"));
        self.blobs.insert(blob.clone(), raw.to_vec());
        let message_id = postman_core::store::message_id(raw).unwrap();
        let mail = Mail { id: id.clone(), blob, mailbox: format!("{account}-inbox"), keywords: Vec::new(), message_id };
        self.mail.entry(account.to_owned()).or_default().push(mail);
        id
    }

    fn of(&self, account: &str) -> Vec<Mail> {
        self.mail.get(account).cloned().unwrap_or_default()
    }

    /// Ответы на вызовы запроса; `None` — ответа нет (обрыв).
    fn api(&mut self, request: &Value) -> Option<Value> {
        let mut out = Vec::new();
        let mut drop = false;
        for call in request["methodCalls"].as_array().unwrap() {
            let (name, args, tag) = (call[0].as_str().unwrap(), &call[1], call[2].clone());
            self.methods.push(name.to_owned());
            let acc = args["accountId"].as_str().unwrap_or("a").to_owned();
            // `Email/copy` с удалением источника — ещё неявный `Email/set` с тем же id вызова (RFC 8620 §5.4).
            let mut implicit = None;
            let answer = match name {
                "Mailbox/get" => json!({
                    "accountId": acc, "state": "m1", "notFound": [],
                    "list": [{ "id": format!("{acc}-inbox"), "name": "Inbox", "role": "inbox", "parentId": null }]
                }),
                "Email/query" => json!({ "accountId": acc, "queryState": "q1", "canCalculateChanges": false, "position": 0, "ids": [], "total": 0 }),
                "Email/changes" => {
                    let since = args["sinceState"].clone();
                    json!({ "accountId": acc, "oldState": since, "newState": since, "hasMoreChanges": false, "created": [], "updated": [], "destroyed": [] })
                }
                "Email/get" => {
                    let ids: Vec<&str> = args["ids"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
                    let list: Vec<Value> = self
                        .of(&acc)
                        .iter()
                        .filter(|m| ids.contains(&m.id.as_str()))
                        .map(|m| json!({ "id": m.id, "blobId": m.blob, "subject": "s" }))
                        .collect();
                    json!({ "accountId": acc, "state": "s1", "list": list, "notFound": [] })
                }
                "Email/import" => {
                    let mut created = serde_json::Map::new();
                    let mut failed = serde_json::Map::new();
                    for (k, e) in args["emails"].as_object().unwrap() {
                        self.imports += 1;
                        let blob = e["blobId"].as_str().unwrap().to_owned();
                        let message_id = postman_core::store::message_id(&self.blobs[&blob]).unwrap();
                        if self.imports == self.quota_at {
                            failed.insert(k.clone(), json!({ "type": "overQuota" }));
                            continue;
                        }
                        if let Some(m) = self.of(&acc).iter().find(|m| m.message_id == message_id) {
                            failed.insert(k.clone(), json!({ "type": "alreadyExists", "existingId": m.id }));
                            continue;
                        }
                        let id = self.id("E");
                        let mailbox = e["mailboxIds"].as_object().unwrap().keys().next().unwrap().clone();
                        let keywords = e["keywords"].as_object().unwrap().keys().cloned().collect();
                        self.mail.entry(acc.clone()).or_default().push(Mail { id: id.clone(), blob, mailbox, keywords, message_id });
                        created.insert(k.clone(), json!({ "id": id }));
                        drop |= self.imports == self.lost_at;
                    }
                    json!({ "accountId": acc, "created": created, "notCreated": failed })
                }
                "Email/copy" => {
                    let from = args["fromAccountId"].as_str().unwrap().to_owned();
                    let mut created = serde_json::Map::new();
                    let mut destroyed = Vec::new();
                    for (k, e) in args["create"].as_object().unwrap() {
                        let source = e["id"].as_str().unwrap();
                        let list = self.mail.entry(from.clone()).or_default();
                        let i = list.iter().position(|m| m.id == source).unwrap();
                        let mut m = list.remove(i);
                        destroyed.push(m.id.clone());
                        m.id = self.id("E");
                        m.mailbox = e["mailboxIds"].as_object().unwrap().keys().next().unwrap().clone();
                        created.insert(k.clone(), json!({ "id": m.id }));
                        self.mail.entry(acc.clone()).or_default().push(m);
                    }
                    assert_eq!(args["onSuccessDestroyOriginal"], json!(true), "перенос, а не копия");
                    implicit = Some(json!(["Email/set", { "accountId": from, "destroyed": destroyed }, tag.clone()]));
                    json!({ "fromAccountId": from, "accountId": acc, "created": created })
                }
                "Email/set" => {
                    let mut destroyed = Vec::new();
                    let mut failed = serde_json::Map::new();
                    for id in args["destroy"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                        let list = self.mail.entry(acc.clone()).or_default();
                        match list.iter().position(|m| m.id == id) {
                            Some(i) => {
                                list.remove(i);
                                destroyed.push(id.to_owned());
                            }
                            None => {
                                failed.insert(id.to_owned(), json!({ "type": "notFound" }));
                            }
                        }
                    }
                    json!({ "accountId": acc, "destroyed": destroyed, "notDestroyed": failed })
                }
                _ => {
                    out.push(json!(["error", { "type": "unknownMethod" }, tag]));
                    continue;
                }
            };
            out.push(json!([name, answer, tag]));
            out.extend(implicit);
        }
        (!drop).then(|| json!({ "methodResponses": out, "sessionState": "s1" }))
    }
}

const SESSION: &str = r#"{
  "capabilities": {
    "urn:ietf:params:jmap:core": { "maxSizeUpload": 50000000, "maxConcurrentUpload": 4, "maxObjectsInSet": 500, "maxCallsInRequest": 16 },
    "urn:ietf:params:jmap:mail": {}
  },
  "accounts": {
    "a": { "name": "user1@example.test", "isPersonal": true, "accountCapabilities": { "urn:ietf:params:jmap:mail": {} } },
    "b": { "name": "shared@example.test", "isPersonal": false, "accountCapabilities": { "urn:ietf:params:jmap:mail": {} } }
  },
  "primaryAccounts": { "urn:ietf:params:jmap:mail": "a" },
  "username": "user1@example.test",
  "apiUrl": "/api",
  "downloadUrl": "/download/{accountId}/{blobId}/{name}?type={type}",
  "uploadUrl": "/upload/{accountId}/",
  "eventSourceUrl": "/events?types={types}&closeafter={closeafter}&ping={ping}",
  "state": "s1"
}"#;

fn reply(s: &mut TcpStream, status: &str, body: &[u8]) {
    let head = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", body.len());
    s.write_all(head.as_bytes()).unwrap();
    s.write_all(body).unwrap();
}

fn serve(s: TcpStream, server: Shared) {
    let mut out = s.try_clone().unwrap();
    let mut r = BufReader::new(s);
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let path = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
        let mut len = 0;
        loop {
            let mut h = String::new();
            r.read_line(&mut h).unwrap();
            if h.trim().is_empty() {
                break;
            }
            if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                len = v.trim().parse().unwrap();
            }
        }
        let mut body = vec![0u8; len];
        r.read_exact(&mut body).unwrap();
        let mut srv = server.lock().unwrap();
        if path == "/.well-known/jmap" {
            reply(&mut out, "200 OK", SESSION.as_bytes());
        } else if path == "/api" {
            match srv.api(&serde_json::from_slice(&body).unwrap()) {
                Some(v) => reply(&mut out, "200 OK", v.to_string().as_bytes()),
                None => return,
            }
        } else if let Some(rest) = path.strip_prefix("/upload/") {
            let acc = rest.trim_end_matches('/').to_owned();
            let blob = srv.id("B");
            let size = body.len();
            srv.blobs.insert(blob.clone(), body);
            reply(&mut out, "200 OK", json!({ "accountId": acc, "blobId": blob, "type": "message/rfc822", "size": size }).to_string().as_bytes());
        } else if let Some(rest) = path.strip_prefix("/download/") {
            let blob = rest.split('/').nth(1).unwrap();
            match srv.blobs.get(blob) {
                Some(b) => reply(&mut out, "200 OK", b),
                None => reply(&mut out, "404 Not Found", b""),
            }
        } else {
            reply(&mut out, "404 Not Found", b"");
        }
    }
}

fn server() -> (String, Shared) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let shared: Shared = Arc::default();
    let s = shared.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let s = s.clone();
            std::thread::spawn(move || serve(conn, s));
        }
    });
    (url, shared)
}

struct Live {
    commands: UnboundedSender<Command>,
    events: Receiver<Event>,
    cache: Cache,
}

impl Live {
    fn wait(&self, what: &str, pick: impl Fn(&Event) -> bool) -> Event {
        let start = Instant::now();
        loop {
            let left = Duration::from_secs(20).checked_sub(start.elapsed()).unwrap_or_else(|| panic!("не дождались: {what}"));
            match self.events.recv_timeout(left) {
                Ok(ev) if pick(&ev) => return ev,
                Ok(_) => {}
                Err(e) => panic!("не дождались: {what} ({e})"),
            }
        }
    }
}

/// Синхронизация основной учётной записи на подменном сервере: первая загрузка прошла.
fn start(url: &str) -> Live {
    let (tx, events) = std::sync::mpsc::channel();
    let cache = Cache::open_in_memory().unwrap();
    let account = Account { url: url.to_owned(), auth: Auth::Basic { user: "user1@example.test".into(), password: "secret".into() } };
    let commands = postman_backend_jmap::spawn(account, cache.clone(), None, tx, || {});
    let live = Live { commands, events, cache };
    let Event::Features(f) = live.wait("Features", |e| matches!(e, Event::Features(_))) else { unreachable!() };
    assert!(f.import() && f.server_copy(), "выгрузка и копия между аккаунтами сессии");
    live.wait("Synced", |e| matches!(e, Event::Status(Status::Synced)));
    live
}

/// Ответы пути на команду задания: по письму.
fn answers(live: &Live, cmd: Command) -> Vec<Event> {
    let n = match &cmd {
        Command::DestroyLetters { letters, .. } | Command::ServerCopy { letters, .. } => letters.len(),
        _ => 1,
    };
    live.commands.send(cmd).unwrap();
    (0..n)
        .map(|_| {
            live.wait("ответ задания", |e| {
                matches!(
                    e,
                    Event::Raw { .. } | Event::Imported { .. } | Event::ImportFailed { .. } | Event::Destroyed { .. } | Event::DestroyFailed { .. }
                )
            })
        })
        .collect()
}

/// Задание ядра до остановки: команды основной учётной записи — плагину JMAP, прочих — `other`; время задания — к его
/// сроку (повторы, пауза сервера) без ожидания. Ответ — состояния хода.
fn drive(engine: &mut Engine<CacheStore>, live: &Live, other: &mut dyn FnMut(Command) -> Vec<Event>) -> Vec<MigrationState> {
    let mut now = Instant::now();
    let mut states = Vec::new();
    for _ in 0..10_000 {
        let out = engine.poll(now).unwrap();
        states.extend(out.events.iter().filter_map(|e| match e {
            Event::Migration { state, .. } => Some(*state),
            _ => None,
        }));
        if out.commands.is_empty() {
            match engine.wake_at() {
                Some(t) => now = now.max(t),
                None => return states,
            }
            continue;
        }
        for (route, cmd) in out.commands {
            let events = if route == Route::Sync(Target::Profile(None)) { answers(live, cmd) } else { other(cmd) };
            for e in events {
                engine.event(&e, now).unwrap();
            }
        }
    }
    panic!("задание не остановилось");
}

fn engine_on(cache: &Cache, features: Features) -> Engine<CacheStore> {
    let mut e = Engine::open(CacheStore::new(cache.clone(), Box::new(|_| Ok(Vec::new())))).unwrap();
    e.features(None, features);
    e
}

fn raw(i: u32) -> Vec<u8> {
    format!("Message-ID: <m{i}@example.test>\r\nSubject: s{i}\r\nFrom: a@example.test\r\n\r\nтекст {i}\r\n").into_bytes()
}

/// Письма учётки `account` в её папке с id сервера `folder`: связи `ids`, содержимое — если `content`.
fn letters_in(cache: &Cache, account: &str, folder: &str, ids: &[String], content: bool) -> Vec<postman_cache::LetterId> {
    let f = match cache.folder_of(account, folder).unwrap() {
        Some(f) => f,
        None => cache
            .upsert_folder(&FolderRow {
                owner: Owner::Account(account.into()),
                parent: None,
                name: "Входящие".into(),
                role: Some("inbox".into()),
                remote: Some(folder.into()),
            })
            .unwrap(),
    };
    let rows: Vec<(String, LetterRow)> = ids
        .iter()
        .zip(1..)
        .map(|(id, i)| {
            let row = LetterRow {
                thread_id: format!("t{i}"),
                received_at: 1_790_000_000 + i64::from(i),
                seen: i % 2 == 0,
                folders: vec![f],
                ..Default::default()
            };
            (id.clone(), row)
        })
        .collect();
    let letters = cache.upsert_remotes(account, &rows).unwrap();
    if content {
        for (l, i) in letters.iter().zip(1..) {
            cache.put_content(*l, &raw(i), &[]).unwrap();
        }
    }
    letters
}

fn ids(letters: &[postman_cache::LetterId]) -> Vec<LetterId> {
    letters.iter().map(|l| LetterId(l.0)).collect()
}

/// 50 писем другой учётки (`@9`, её сервер — в тесте) — в «Входящие» JMAP: `Email/import` со всеми ключевыми словами,
/// ящик полон на 30-м — «нет места», «Продолжить» — до конца; ответ на 12-й потерян (обрыв) — повтор, сервер узнаёт
/// письмо (`alreadyExists`), дубля нет; у источника — удаление по ответу; письма связаны с JMAP тем же `LetterId`.
#[test]
fn fifty_letters_into_jmap_with_quota_and_a_lost_answer() {
    let (url, server) = server();
    {
        let mut s = server.lock().unwrap();
        (s.quota_at, s.lost_at) = (30, 12);
    }
    let live = start(&url);
    let inbox = live.cache.folder_of("", "a-inbox").unwrap().unwrap();
    let letters = letters_in(&live.cache, "@9", "in9", &(1..=50).map(|i| format!("s{i}")).collect::<Vec<_>>(), true);
    let mut engine = engine_on(&live.cache, Features::NONE.with_import(true));
    let ticket = postman_core::store::move_letters(&mut engine, &ids(&letters), FolderId(inbox.0)).unwrap();
    let job = JobId(ticket.job.unwrap().0);
    engine.request(MigrationRequest::Answer { job, mode: MigrationMode::Upload }).unwrap();

    let mut destroyed = Vec::new();
    let mut source = |cmd: Command| match cmd {
        Command::DestroyLetters { job, account, letters } => {
            assert_eq!(account, "@9");
            destroyed.extend(letters.iter().copied());
            letters.into_iter().map(|letter| Event::Destroyed { job, letter }).collect()
        }
        _ => panic!("источнику — только удаление"),
    };
    let states = drive(&mut engine, &live, &mut source);
    assert_eq!(states.last(), Some(&MigrationState::QuotaFull), "ящик полон — задание стоит");
    engine.request(MigrationRequest::Resume { job }).unwrap();
    let states = drive(&mut engine, &live, &mut source);
    assert_eq!(states.last(), Some(&MigrationState::Done));

    let mail = server.lock().unwrap().of("a");
    assert_eq!(server.lock().unwrap().imports, 52, "50 писем, отказ «нет места» и повтор после обрыва");
    let mids: HashSet<&String> = mail.iter().map(|m| &m.message_id).collect();
    assert_eq!((mail.len(), mids.len()), (50, 50), "без дублей");
    assert!(mail.iter().all(|m| m.mailbox == "a-inbox"));
    let seen = mail.iter().filter(|m| m.keywords.iter().any(|k| k == "$seen")).count();
    assert_eq!(seen, 25, "ключевые слова — как у письма");
    destroyed.sort();
    assert_eq!(destroyed, ids(&letters), "у источника — каждое, после подтверждения цели");
    let server_ids: HashSet<String> = mail.iter().map(|m| m.id.clone()).collect();
    for l in &letters {
        let refs = live.cache.remotes(*l).unwrap();
        assert!(refs.len() == 1 && !refs[0].detached && refs[0].account.is_empty() && server_ids.contains(&refs[0].id), "{refs:?}");
    }
    assert!(live.cache.jobs().unwrap().is_empty());
}

/// Из общего ящика (аккаунт `b` той же сессии) в свой — сервером (`Email/copy` с удалением источника), без выгрузки
/// и без отдельного удаления.
#[test]
fn shared_box_to_own_by_email_copy() {
    let (url, server) = server();
    let source: Vec<String> = {
        let mut s = server.lock().unwrap();
        (1..=5).map(|i| s.put("b", &raw(i))).collect()
    };
    let live = start(&url);
    let letters = letters_in(&live.cache, "b", "b-inbox", &source, true);
    let inbox = live.cache.folder_of("", "a-inbox").unwrap().unwrap();
    let mut engine = engine_on(&live.cache, Features::NONE.with_import(true).with_server_copy(true));
    let ticket = postman_core::store::move_letters(&mut engine, &ids(&letters), FolderId(inbox.0)).unwrap();
    engine.request(MigrationRequest::Answer { job: JobId(ticket.job.unwrap().0), mode: MigrationMode::Upload }).unwrap();
    let states = drive(&mut engine, &live, &mut |_| panic!("всё — в одной сессии"));
    assert_eq!(states.last(), Some(&MigrationState::Done));

    let s = server.lock().unwrap();
    assert!(s.of("b").is_empty() && s.of("a").len() == 5, "перенесено сервером");
    assert!(s.methods.iter().any(|m| m == "Email/copy") && !s.methods.iter().any(|m| m == "Email/import" || m == "Email/set"));
    for l in &letters {
        let refs = live.cache.remotes(*l).unwrap();
        assert!(refs.len() == 1 && refs[0].account.is_empty() && !refs[0].detached, "{refs:?}");
    }
}

/// Письма JMAP — в «На этом компьютере»: письмо целиком — блоб `Email.blobId`, затем `Email/set` `destroy`; письмо
/// только в базе, когда сервер ответил.
#[test]
fn jmap_letters_to_this_computer() {
    let (url, server) = server();
    let source: Vec<String> = {
        let mut s = server.lock().unwrap();
        (1..=2).map(|i| s.put("a", &raw(i))).collect()
    };
    let live = start(&url);
    let letters = letters_in(&live.cache, "", "a-inbox", &source, false);
    let local = live.cache.create_folder(None, "Архив 2025").unwrap();
    let mut engine = engine_on(&live.cache, Features::NONE.with_import(true));
    postman_core::store::move_letters(&mut engine, &ids(&letters), FolderId(local.0)).unwrap();
    let states = drive(&mut engine, &live, &mut |_| panic!("всё — у JMAP"));
    assert_eq!(states.last(), Some(&MigrationState::Done));

    assert!(server.lock().unwrap().of("a").is_empty(), "удалено у источника");
    for (l, i) in letters.iter().zip(1..) {
        let row = live.cache.letter(*l).unwrap().unwrap();
        assert_eq!((row.place, row.folders.as_slice()), (Place::Local, &[local][..]));
        assert_eq!(live.cache.letter_content(*l).unwrap(), Some(raw(i)), "письмо целиком — с сервера");
        assert!(live.cache.remotes(*l).unwrap().is_empty());
    }
}
