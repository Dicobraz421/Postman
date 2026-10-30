//! Перенос между учётками (S8-13) против стенда Stalwart 1.0: 20 писем с меткой `[postman-test …]` из JMAP одной
//! учётки в IMAP другой (вторая учётная запись профиля) и обратно — шаги задания ядра (`migrate::Engine`): письмо
//! целиком (`UID FETCH BODY.PEEK[]` и блоб JMAP), выгрузка (`APPEND` с флагами и `INTERNALDATE`, `Email/import`),
//! удаление у источника по ответу; письма — те же (`LetterId`). Письма заводятся тем же заданием из третьей учётки
//! (её сервер — в тесте). Стенд разобран до выхода 1.0: тест помечен `#[ignore]`, прогон — когда стенд поднят
//! (`docs/STALWART.md`); за собой убирает.
//!
//! Запуск (пароли и адреса — только в окружении; самоподписанный сертификат стенда — `POSTMAN_INSECURE_TLS=1`):
//!   POSTMAN_STAND_URL=… POSTMAN_STAND_IMAP=<адрес>:993 POSTMAN_STAND_SMTP=<адрес>:465 POSTMAN_STAND_USER1=…
//!   POSTMAN_STAND_PASSWORD1=… POSTMAN_STAND_USER2=… POSTMAN_STAND_PASSWORD2=…
//!   cargo test -p postman-backend-imap --test stand_migrate -- --ignored --test-threads=1

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use postman_backend_imap::{self as imap, ImapAccount};
use postman_cache::{Cache, FolderRow, LetterRow, Owner};
use postman_core::migrate::Engine;
use postman_core::model::{Auth, Features, FolderId, JobId, LetterId, MigrationMode, MigrationRequest, MigrationState};
use postman_core::store::CacheStore;
use postman_core::sync::{Account, Command, Event, Route, Status, Target, UnboundedSender};

const WAIT: Duration = Duration::from_secs(60);
const LETTERS: u32 = 20;

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} не задан: стенд — docs/STALWART.md"))
}

struct Live {
    commands: UnboundedSender<Command>,
    events: Receiver<Event>,
}

impl Live {
    fn ready(commands: UnboundedSender<Command>, events: Receiver<Event>) -> Live {
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

/// Задание ядра до остановки: команды — JMAP (основная), IMAP (`@2`); третьей учётке (`@9`, источник писем) —
/// только удаление, отвечает тест.
fn drive(engine: &mut Engine<CacheStore>, jmap: &Live, imap: &Live) -> MigrationState {
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
            let events = match (route, cmd) {
                (Route::Sync(Target::Profile(None)), cmd) => jmap.answers(cmd),
                (Route::Sync(Target::Profile(Some(p))), cmd) if p == "@2" => imap.answers(cmd),
                (_, Command::DestroyLetters { job, letters, .. }) => letters.into_iter().map(|letter| Event::Destroyed { job, letter }).collect(),
                (route, _) => panic!("маршрут {route:?}"),
            };
            for e in events {
                engine.event(&e, now).unwrap();
            }
        }
    }
    panic!("задание не остановилось");
}

fn migrate(engine: &mut Engine<CacheStore>, letters: &[LetterId], to: postman_cache::FolderId, jmap: &Live, imap: &Live) {
    let ticket = postman_core::store::move_letters(engine, letters, FolderId(to.0)).unwrap();
    engine.request(MigrationRequest::Answer { job: JobId(ticket.job.unwrap().0), mode: MigrationMode::Upload }).unwrap();
    assert_eq!(drive(engine, jmap, imap), MigrationState::Done);
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

/// Учётка, где письма лежат до теста: `n` писем с меткой и содержимым (её сервер — тест).
fn seed(cache: &Cache, stamp: u128) -> Vec<LetterId> {
    let folder = cache
        .upsert_folder(&FolderRow {
            owner: Owner::Account("@9".into()),
            parent: None,
            name: "Входящие".into(),
            role: None,
            remote: Some("in9".into()),
        })
        .unwrap();
    let rows: Vec<(String, LetterRow)> = (1..=LETTERS)
        .map(|i| {
            (format!("s{i}"), LetterRow { received_at: 1_790_000_000 + i64::from(i), seen: i % 2 == 0, folders: vec![folder], ..Default::default() })
        })
        .collect();
    let ids = cache.upsert_remotes("@9", &rows).unwrap();
    for (id, i) in ids.iter().zip(1..) {
        let raw =
            format!("Message-ID: <{i}.{stamp}@postman.test>\r\nSubject: [postman-test {stamp}] {i}\r\nFrom: a@postman.test\r\n\r\nперенос {i}\r\n");
        cache.put_content(*id, raw.as_bytes(), &[]).unwrap();
    }
    ids.into_iter().map(|l| LetterId(l.0)).collect()
}

#[test]
#[ignore = "стенд Stalwart 1.0 (docs/STALWART.md)"]
fn twenty_letters_jmap_to_imap_and_back() {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
    let cache = Cache::open_in_memory().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let main =
        Account { url: env("POSTMAN_STAND_URL"), auth: Auth::Basic { user: env("POSTMAN_STAND_USER1"), password: env("POSTMAN_STAND_PASSWORD1") } };
    let jmap = Live::ready(postman_backend_jmap::spawn(main, cache.clone(), None, tx, || {}), rx);
    let url = format!("imaps://{}?smtp={}", env("POSTMAN_STAND_IMAP"), env("POSTMAN_STAND_SMTP"));
    let account = ImapAccount::from_url(&url, &env("POSTMAN_STAND_USER2"), &env("POSTMAN_STAND_PASSWORD2")).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let imap = Live::ready(imap::spawn(account, Some("@2".into()), cache.clone(), None, tx, || {}, Vec::new()), rx);

    let mut engine = Engine::open(CacheStore::new(cache.clone(), Box::new(|_| Ok(Vec::new())))).unwrap();
    engine.features(None, Features::NONE.with_import(true));
    engine.features(Some("@2"), Features::NONE.with_import(true));
    let letters = seed(&cache, stamp);
    migrate(&mut engine, &letters, inbox(&cache, ""), &jmap, &imap);
    migrate(&mut engine, &letters, inbox(&cache, "@2"), &jmap, &imap);
    for l in &letters {
        let refs = cache.remotes(postman_cache::LetterId(l.0)).unwrap();
        assert!(refs.len() == 1 && refs[0].account == "@2" && !refs[0].detached, "в IMAP: {refs:?}");
        assert!(cache.letter(postman_cache::LetterId(l.0)).unwrap().unwrap().folders == [inbox(&cache, "@2")]);
    }
    migrate(&mut engine, &letters, inbox(&cache, ""), &jmap, &imap);
    let keys: Vec<String> = letters.iter().map(|l| postman_core::store::key_of(&cache, *l).unwrap().expect("связь с JMAP")).collect();
    assert!(keys.iter().all(|k| postman_cache::account_of(k).is_none()), "обратно — в JMAP основной учётки");

    // Уборка: письма с меткой — насовсем у JMAP (у IMAP их удалило задание).
    for k in keys {
        jmap.commands.send(Command::Destroy(k)).unwrap();
    }
    jmap.commands.send(Command::Search { query: format!("postman-test {stamp}"), mailbox: None, ticket: 9 }).unwrap();
    let Event::SearchResults { ids, .. } = jmap.wait("SearchResults", |e| matches!(e, Event::SearchResults { ticket: 9, .. })) else {
        unreachable!()
    };
    assert!(ids.is_empty(), "убрано");
    println!("перенос JMAP → IMAP другой учётки и обратно: {LETTERS} писем");
}
