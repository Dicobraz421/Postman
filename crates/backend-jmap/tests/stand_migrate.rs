//! Перенос между учётками (S8-13) против стенда Stalwart 1.0: 20 писем с меткой `[postman-test …]` из JMAP одной
//! учётки в JMAP другой (вторая учётная запись профиля) и обратно — шаги задания ядра (`migrate::Engine`), письмо
//! целиком, `Email/import`, удаление у источника по ответу; письма — те же (`LetterId`). Стенд разобран до выхода 1.0:
//! тест помечен `#[ignore]`, прогон — когда стенд поднят (`docs/STALWART.md`); за собой убирает.
//!
//! Запуск (пароли — только в окружении):
//!   POSTMAN_STAND_URL=… POSTMAN_STAND_USER1=… POSTMAN_STAND_PASSWORD1=… POSTMAN_STAND_USER2=… POSTMAN_STAND_PASSWORD2=…
//!   cargo test -p postman-backend-jmap --test stand_migrate -- --ignored --test-threads=1

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use postman_cache::Cache;
use postman_core::migrate::Engine;
use postman_core::model::{Auth, Features, FolderId, JobId, LetterId, MigrationMode, MigrationRequest, MigrationState};
use postman_core::store::CacheStore;
use postman_core::sync::{Account, Command, Event, Route, Status, Target, UnboundedSender};
use postman_jmap::{Client, Filter};

const WAIT: Duration = Duration::from_secs(60);
const LETTERS: u32 = 20;

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} не задан: стенд — docs/STALWART.md"))
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

/// Метка прогона: число мс — и в теме (`[postman-test <n>]`), и в `Message-ID`.
fn stamp() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()
}

fn login(n: u8) -> (String, String) {
    (env(&format!("POSTMAN_STAND_USER{n}")), env(&format!("POSTMAN_STAND_PASSWORD{n}")))
}

async fn client(n: u8) -> Client {
    let (user, password) = login(n);
    Client::connect(&env("POSTMAN_STAND_URL"), postman_jmap::Auth::Basic { user, password }).await.unwrap()
}

/// Письма с меткой в учётке `n`.
async fn tagged(n: u8, tag: &str) -> Vec<String> {
    client(n).await.query_ids(&Filter::Subject(tag.into()), 200).await.unwrap()
}

/// Письма с меткой — насовсем, в обеих учётках.
async fn cleanup(tag: &str) {
    for n in [1, 2] {
        let c = client(n).await;
        for id in c.query_ids(&Filter::Subject(tag.into()), 200).await.unwrap_or_default() {
            let _ = c.destroy_email(&id).await;
        }
    }
}

struct Live {
    commands: UnboundedSender<Command>,
    events: Receiver<Event>,
}

impl Live {
    fn start(n: u8, profile: Option<&str>, cache: &Cache) -> Live {
        let (user, password) = login(n);
        let (tx, events) = std::sync::mpsc::channel();
        let account = Account { url: env("POSTMAN_STAND_URL"), auth: Auth::Basic { user, password } };
        let commands = postman_backend_jmap::spawn_as(account, profile.map(str::to_owned), cache.clone(), None, tx, || {});
        let live = Live { commands, events };
        live.wait("Synced", |e| matches!(e, Event::Status(Status::Synced)));
        live
    }

    fn wait(&self, what: &str, pick: impl Fn(&Event) -> bool) -> Event {
        let start = Instant::now();
        loop {
            let left = WAIT.checked_sub(start.elapsed()).unwrap_or_else(|| panic!("не дождались: {what}"));
            match self.events.recv_timeout(left) {
                Ok(ev) if pick(&ev) => return ev,
                Ok(_) => {}
                Err(e) => panic!("не дождались: {what} ({e})"),
            }
        }
    }

    fn answers(&self, cmd: Command) -> Vec<Event> {
        let n = match &cmd {
            Command::DestroyLetters { letters, .. } | Command::ServerCopy { letters, .. } => letters.len(),
            _ => 1,
        };
        self.commands.send(cmd).unwrap();
        (0..n)
            .map(|_| {
                self.wait("ответ задания", |e| {
                    matches!(
                        e,
                        Event::Raw { .. }
                            | Event::Imported { .. }
                            | Event::ImportFailed { .. }
                            | Event::Destroyed { .. }
                            | Event::DestroyFailed { .. }
                    )
                })
            })
            .collect()
    }
}

/// Задание ядра до остановки: команды — путям учётных записей по маршруту (основная, `@2`).
fn drive(engine: &mut Engine<CacheStore>, first: &Live, second: &Live) -> MigrationState {
    let mut now = Instant::now();
    let mut last = MigrationState::Running;
    for _ in 0..10_000 {
        let out = engine.poll(now).unwrap();
        for e in &out.events {
            if let Event::Migration { state, .. } = e {
                last = *state;
            }
        }
        if out.commands.is_empty() {
            match engine.wake_at() {
                Some(t) => now = now.max(t),
                None => return last,
            }
            continue;
        }
        for (route, cmd) in out.commands {
            let events = match route {
                Route::Sync(Target::Profile(None)) => first.answers(cmd),
                Route::Sync(Target::Profile(Some(p))) if p == "@2" => second.answers(cmd),
                other => panic!("маршрут {other:?}"),
            };
            for e in events {
                engine.event(&e, now).unwrap();
            }
        }
    }
    panic!("задание не остановилось");
}

/// Перенести письма в папку и вести задание до конца.
fn migrate(engine: &mut Engine<CacheStore>, letters: &[LetterId], to: postman_cache::FolderId, first: &Live, second: &Live) {
    let ticket = postman_core::store::move_letters(engine, letters, FolderId(to.0)).unwrap();
    engine.request(MigrationRequest::Answer { job: JobId(ticket.job.unwrap().0), mode: MigrationMode::Upload }).unwrap();
    assert_eq!(drive(engine, first, second), MigrationState::Done);
}

fn inbox(cache: &Cache, account: &str) -> postman_cache::FolderId {
    let key = cache
        .mailboxes()
        .unwrap()
        .into_iter()
        .find(|m| m.role.as_deref() == Some("inbox") && postman_cache::account_of(&m.id).unwrap_or("") == account)
        .unwrap()
        .id;
    cache.folder_of(account, postman_cache::unscoped(&key).1).unwrap().unwrap()
}

#[test]
#[ignore = "стенд Stalwart 1.0 (docs/STALWART.md)"]
fn twenty_letters_to_another_account_and_back() {
    let rt = rt();
    let stamp = stamp();
    let tag = format!("[postman-test {stamp}]");
    // Письма — в «Входящие» первой учётки выгрузкой (`Email/import`), без рассылки.
    rt.block_on(async {
        let c = client(1).await;
        let acc = c.mail_account().unwrap().to_owned();
        let (boxes, _) = c.mailboxes().await.unwrap();
        let inbox = boxes.iter().find(|m| m.role.as_deref() == Some("inbox")).unwrap().id.clone();
        for i in 1..=LETTERS {
            let raw = format!("Message-ID: <{i}.{stamp}@postman.test>\r\nSubject: {tag} {i}\r\nFrom: a@postman.test\r\n\r\nперенос {i}\r\n");
            let blob = c.upload(&acc, raw.into_bytes(), "message/rfc822").await.unwrap();
            c.import_email(&acc, &blob, std::slice::from_ref(&inbox), &["$seen".into()], None).await.unwrap();
        }
    });
    let cache = Cache::open_in_memory().unwrap();
    let first = Live::start(1, None, &cache);
    let second = Live::start(2, Some("@2"), &cache);
    first.commands.send(Command::Search { query: tag.clone(), mailbox: None, ticket: 1 }).unwrap();
    let Event::SearchResults { ids, .. } = first.wait("SearchResults", |e| matches!(e, Event::SearchResults { ticket: 1, .. })) else {
        unreachable!()
    };
    let letters: Vec<LetterId> = ids.iter().map(|k| postman_core::store::letter_of_key(&cache, k).unwrap().unwrap()).collect();
    assert_eq!(letters.len(), LETTERS as usize);

    let mut engine = Engine::open(CacheStore::new(cache.clone(), Box::new(|_| Ok(Vec::new())))).unwrap();
    engine.features(None, Features::NONE.with_import(true));
    engine.features(Some("@2"), Features::NONE.with_import(true));
    migrate(&mut engine, &letters, inbox(&cache, "@2"), &first, &second);
    assert_eq!((rt.block_on(tagged(1, &tag)).len(), rt.block_on(tagged(2, &tag)).len()), (0, LETTERS as usize), "перенесено");
    for l in &letters {
        let refs = cache.remotes(postman_cache::LetterId(l.0)).unwrap();
        assert!(refs.len() == 1 && refs[0].account == "@2" && !refs[0].detached, "{refs:?}");
    }

    migrate(&mut engine, &letters, inbox(&cache, ""), &first, &second);
    assert_eq!((rt.block_on(tagged(1, &tag)).len(), rt.block_on(tagged(2, &tag)).len()), (LETTERS as usize, 0), "и обратно");
    rt.block_on(cleanup(&tag));
    println!("перенос JMAP → JMAP другой учётки и обратно: {LETTERS} писем");
}
