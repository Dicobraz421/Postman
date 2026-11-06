//! Выборка на сервере поверх настоящего кэша и движка переноса; сервер — список ключей в памяти.

use std::collections::BTreeSet;
use std::sync::Arc;

use postman_cache::{EmailRow, MailboxRow, Rights};

use super::*;
use crate::migrate::Engine;
use crate::model::Filter;
use crate::store::{CacheStore, tests::splitter};

const INBOX: &str = "in";

fn row(key: &str, n: usize) -> EmailRow {
    EmailRow {
        id: key.to_owned(),
        thread_id: format!("t{n}"),
        mailbox_ids: vec![INBOX.to_owned()],
        received_at: 1_000_000 + n as i64,
        subject: format!("s{n}"),
        from_name: String::new(),
        from_addr: "a@example.test".into(),
        preview: String::new(),
        seen: false,
        flagged: false,
        has_attachment: false,
        size: 100,
        keywords: Vec::new(),
    }
}

/// Сервер источника и поток ядра.
struct World {
    cache: Cache,
    engine: Engine<CacheStore>,
    sweeper: Sweeper,
    /// Ключи на сервере, новые первыми.
    server: Vec<String>,
    now: Instant,
    /// Страниц снимка и `StoreLetters`.
    listed: Vec<u64>,
    stored: Vec<usize>,
    patches: Vec<crate::lifecycle::Patch>,
    progress: Vec<Progress>,
    migration: Vec<(u64, u64, MigrationState)>,
}

impl World {
    /// `n` писем на сервере, первые `cached` — в кэше.
    fn new(n: usize, cached: usize) -> Self {
        let cache = Cache::open_in_memory().unwrap();
        let inbox = MailboxRow {
            id: INBOX.into(),
            parent_id: None,
            name: "Inbox".into(),
            role: Some("inbox".into()),
            sort_order: 0,
            total: n as u32,
            unread: 0,
            rights: Rights::ALL,
        };
        cache.replace_mailboxes(None, &[inbox]).unwrap();
        let server: Vec<String> = (0..n).map(|i| format!("m{i}")).collect();
        let rows: Vec<EmailRow> = server.iter().enumerate().take(cached).map(|(i, k)| row(k, i)).collect();
        cache.upsert_emails(&rows).unwrap();
        let mut w = World {
            engine: Engine::open(CacheStore::new(cache.clone(), splitter())).unwrap(),
            sweeper: Sweeper::open(cache.clone()).unwrap(),
            cache,
            server,
            now: Instant::now(),
            listed: Vec::new(),
            stored: Vec::new(),
            patches: Vec::new(),
            progress: Vec::new(),
            migration: Vec::new(),
        };
        w.online();
        w
    }

    fn online(&mut self) {
        self.sweeper.online(None);
        self.engine.online(None);
    }

    /// Перезапуск: всё из базы.
    fn restart(&mut self) {
        self.sweeper = Sweeper::open(self.cache.clone()).unwrap();
        let held = self.sweeper.held();
        self.engine = Engine::open_held(CacheStore::new(self.cache.clone(), splitter()), &held).unwrap();
        self.online();
    }

    fn selector(&self) -> Selector {
        Selector { mailbox: INBOX.into(), filter: None, except: Vec::new() }
    }

    fn local(&self) -> FolderId {
        FolderId(self.cache.create_folder(None, "Архив").unwrap().0)
    }

    fn in_folder(&self, to: FolderId) -> usize {
        self.cache.folder_count(postman_cache::FolderId(to.0)).unwrap()
    }

    /// Один круг потока ядра; `true` — были команды или события.
    fn turn(&mut self) -> bool {
        let mut o = self.sweeper.poll(&mut self.engine, self.now).unwrap();
        let e = self.engine.poll(self.now).unwrap();
        o.commands.extend(e.commands);
        o.events.extend(e.events);
        let busy = !o.commands.is_empty() || !o.events.is_empty();
        for e in o.events {
            match e {
                Event::Sweep(p) => self.progress.push(p),
                Event::Migration { done, total, state, .. } => self.migration.push((done, total, state)),
                _ => {}
            }
        }
        for (_, c) in o.commands {
            self.answer(c);
        }
        busy
    }

    /// Сервер отвечает.
    fn answer(&mut self, c: Command) {
        let now = self.now;
        match c {
            Command::ListLetters { sweep, selector, position, limit } => {
                assert_eq!(selector.mailbox, INBOX);
                self.listed.push(position);
                let from = usize::try_from(position).unwrap();
                let ids: Vec<String> = self.server.iter().skip(from).take(limit as usize).cloned().collect();
                let end = from + ids.len() >= self.server.len();
                self.sweeper.event(&Event::LettersListed { sweep, ids, total: self.server.len() as u64, end }, now).unwrap();
            }
            Command::StoreLetters { sweep, ids, .. } => {
                self.stored.push(ids.len());
                let (known, gone): (Vec<_>, Vec<_>) = ids.into_iter().partition(|k| self.server.contains(k));
                let rows: Vec<EmailRow> = known.iter().map(|k| row(k, k[1..].parse().unwrap())).collect();
                self.cache.upsert_emails(&rows).unwrap();
                self.sweeper.event(&Event::LettersStored { sweep, gone }, now).unwrap();
            }
            Command::FetchRaw { letter } => {
                self.engine.event(&Event::Raw { letter, data: Ok(Arc::new(b"Subject: x\r\n\r\nbody\r\n".to_vec())) }, now).unwrap();
            }
            Command::DestroyLetters { job, account, letters } => {
                for letter in letters {
                    let refs = self.cache.remotes(postman_cache::LetterId(letter.0)).unwrap();
                    let r = refs.iter().find(|r| r.account == account && r.detached).unwrap();
                    self.server.retain(|k| *k != r.id);
                    self.engine.event(&Event::Destroyed { job, letter }, now).unwrap();
                }
            }
            Command::Patch { patches, .. } => self.patches.extend(patches),
            Command::Destroy(key) => self.server.retain(|k| *k != key),
            other => panic!("неожиданная команда: {:?}", other.routes().len()),
        }
    }

    fn run(&mut self) {
        for _ in 0..20_000 {
            let ready = self.sweeper.wake_at(self.now).is_some_and(|t| t <= self.now);
            if !self.turn() && !ready {
                let later = [self.sweeper.wake_at(self.now), self.engine.wake_at()].into_iter().flatten().min();
                match later.filter(|t| *t > self.now) {
                    Some(t) => self.now = t,
                    None => return,
                }
            }
        }
        panic!("выборка не кончается");
    }

    /// Круги, пока условие не сбудется.
    fn until(&mut self, done: impl Fn(&World) -> bool) {
        for _ in 0..2000 {
            if done(self) {
                return;
            }
            self.turn();
        }
        panic!("условие не сбылось");
    }

    fn end(&self) -> &Progress {
        self.progress.last().unwrap()
    }
}

/// Ctrl+A в папке, где загружено 500 из 1250: перенос всех, а не только загруженных; задание одно, ход честный: всего —
/// вся выборка с первого события, готово растёт.
#[test]
fn moves_the_whole_selection_not_the_loaded_page() {
    let mut w = World::new(1250, 500);
    let to = w.local();
    w.sweeper.start(w.selector(), Action::Transfer { to }).unwrap();
    w.run();
    assert!(w.server.is_empty(), "у сервера не осталось: {}", w.server.len());
    assert_eq!(w.listed, [0, 500, 1000], "страницы снимка");
    assert_eq!(w.stored, [500, 250], "строки — только недостающие");
    assert_eq!(w.in_folder(to), 1250);
    assert!(w.cache.jobs().unwrap().is_empty() && w.cache.state(INDEX).unwrap().as_deref() == Some("[]"));
    assert!(w.migration.iter().all(|m| m.1 == 1250), "всего — вся выборка: {:?}", w.migration.iter().map(|m| m.1).collect::<BTreeSet<_>>());
    assert!(w.migration.windows(2).all(|p| p[0].0 <= p[1].0), "готово не убывает");
    assert_eq!(w.migration.last().map(|m| (m.0, m.2)), Some((1250, MigrationState::Done)));
    assert_eq!(w.end().state, State::Done);
}

/// Снятые вручную письма и письмо, пришедшее после снимка, остаются у сервера; новое письмо сверху посреди снимка не
/// даёт дубля.
#[test]
fn except_and_later_mail_stay() {
    let mut w = World::new(1200, 500);
    let to = w.local();
    let selector = Selector { except: vec!["m3".into(), "m700".into()], ..w.selector() };
    w.sweeper.start(selector, Action::Transfer { to }).unwrap();
    w.until(|w| w.listed.len() == 2);
    // Письмо пришло между страницами: вторая страница сдвинулась на одно.
    w.server.insert(0, "m9000".into());
    w.run();
    let mut left = w.server.clone();
    left.sort();
    assert_eq!(left, ["m3", "m700", "m9000"], "у сервера — снятые и новое письмо");
    assert_eq!(w.migration.last().map(|m| m.1), Some(1198));
}

/// Перезапуск посреди действия: страница не теряется, задание то же, письма не удваиваются; перезапуск посреди снимка —
/// снимок заново.
#[test]
fn survives_a_restart_in_both_steps() {
    let mut w = World::new(1200, 0);
    let to = w.local();
    w.sweeper.start(w.selector(), Action::Transfer { to }).unwrap();
    w.until(|w| w.listed.len() == 2);
    w.restart();
    w.listed.clear();
    w.until(|w| w.in_folder(to) >= 500);
    assert_eq!(w.listed, [0, 500, 1000], "снимок снят заново");
    let job = w.sweeper.held().first().map(|h| h.0).unwrap();
    assert_eq!(Sweeper::expected(&w.cache, job), 700);
    w.restart();
    assert_eq!(w.engine.job_state(job), Some(MigrationState::Running), "то же задание");
    w.run();
    assert!(w.server.is_empty());
    assert_eq!(w.in_folder(to), 1200);
    assert!(w.cache.jobs().unwrap().is_empty() && w.cache.check_refs().unwrap().is_empty());
}

/// Задание, у которого пришедшие письма кончились, а страницы ещё не пришли, живёт и после перезапуска.
#[test]
fn job_without_letters_waits_for_the_next_page() {
    let mut w = World::new(700, 0);
    let to = w.local();
    w.sweeper.start(w.selector(), Action::Transfer { to }).unwrap();
    w.until(|w| w.sweeper.held().first().is_some_and(|h| h.2 <= 200));
    let job = w.sweeper.held()[0].0;
    w.restart();
    assert!(w.engine.job_state(job).is_some(), "задание ждёт следующую страницу");
    w.run();
    assert!(w.server.is_empty() && w.cache.jobs().unwrap().is_empty());
}

/// Отмена задания: пришедшие письма возвращаются, остальные и не уходили, выборка кончается.
#[test]
fn cancel_leaves_the_rest_at_the_source() {
    let mut w = World::new(1200, 500);
    let to = w.local();
    w.sweeper.start(w.selector(), Action::Transfer { to }).unwrap();
    w.until(|w| w.in_folder(to) > 0);
    let job = w.sweeper.held()[0].0;
    w.engine.request(MigrationRequest::Pause { job }).unwrap();
    w.turn();
    let frozen = w.stored.len();
    w.turn();
    assert_eq!(w.stored.len(), frozen, "пауза задания — и страницы стоят");
    w.engine.request(MigrationRequest::Cancel { job, keep_local: false }).unwrap();
    w.run();
    assert_eq!(w.end().state, State::Stopped);
    assert!(w.cache.jobs().unwrap().is_empty() && w.sweeper.held().is_empty());
    assert_eq!(w.server.len() + w.in_folder(to), 1200, "ни одно письмо не потеряно");
    assert_eq!(w.cache.state(&page_key(SweepId(1), 2)).unwrap(), None, "страницы снимка убраны");
}

/// Пока источник не вошёл на сервер, команд ему нет; вошёл — выборка пошла сама. Отказ сервера — повтор страницы после
/// паузы, не быстрее.
#[test]
fn waits_for_sign_in_and_retries_a_refused_page() {
    let mut w = World::new(10, 0);
    w.sweeper = Sweeper::open(w.cache.clone()).unwrap();
    let to = w.local();
    let id = w.sweeper.start(w.selector(), Action::Transfer { to }).unwrap();
    assert!(w.sweeper.poll(&mut w.engine, w.now).unwrap().commands.is_empty());
    assert_eq!(w.sweeper.wake_at(w.now), None, "будить незачем");
    w.sweeper.online(None);
    let out = w.sweeper.poll(&mut w.engine, w.now).unwrap();
    assert!(matches!(out.commands[..], [(_, Command::ListLetters { position: 0, .. })]));
    w.sweeper.event(&Event::ListFailed { sweep: id, error: "503".into() }, w.now).unwrap();
    assert!(w.sweeper.poll(&mut w.engine, w.now).unwrap().commands.is_empty());
    assert_eq!(w.sweeper.wake_at(w.now), Some(w.now + RETRY));
    w.now += RETRY;
    w.run();
    assert!(w.server.is_empty());
}

/// Чего путь не умеет — выборка кончается отказом, а не молчит и не подменяется папкой.
#[test]
fn unsupported_filter_fails_loudly() {
    let mut w = World::new(10, 0);
    let to = w.local();
    let selector = Selector { filter: Some(Filter::HasKeyword("x".into())), ..w.selector() };
    let id = w.sweeper.start(selector, Action::Transfer { to }).unwrap();
    let out = w.sweeper.poll(&mut w.engine, w.now).unwrap();
    assert!(matches!(&out.commands[..], [(_, Command::ListLetters { selector, .. })] if selector.filter.is_some()));
    w.sweeper.event(&Event::ListFailed { sweep: id, error: format!("protocol: {FOLDER_UNSUPPORTED}") }, w.now).unwrap();
    let out = w.sweeper.poll(&mut w.engine, w.now).unwrap();
    assert!(out.events.iter().any(|e| matches!(e, Event::Sweep(Progress { state: State::Failed(_), .. }))));
    assert_eq!(w.server.len(), 10);
}

/// Прочитано на всей выборке: правки страницами, кэш — сразу, письма за страницей — тоже.
#[test]
fn patch_covers_every_page() {
    let mut w = World::new(1100, 500);
    let action = Action::Patch { act: Act::Seen(true), archive_name: "Archive".into() };
    w.sweeper.start(w.selector(), action).unwrap();
    w.run();
    assert_eq!(w.patches.len(), 1100);
    let rows = w.cache.rows_by_ids(&w.server).unwrap();
    assert!(rows.len() == 1100 && rows.iter().all(|r| r.seen));
    assert_eq!((w.end().done, w.end().total, w.end().state.clone()), (1100, 1100, State::Done));
}

/// Срок на всей выборке: письма за страницей — тоже, дата с временем переживает запись выборки.
#[test]
fn mark_covers_every_page() {
    let mut w = World::new(700, 100);
    let when = When { date: jiff::civil::date(2026, 12, 1), time: Some(jiff::civil::time(9, 30, 0, 0)) };
    assert_eq!(parse_when(&when_text(when)), Some(when));
    let action = Action::Mark { kind: Kind::Due, when: Some(when_text(when)), archive_name: "Archive".into() };
    w.sweeper.start(w.selector(), action).unwrap();
    w.run();
    assert_eq!(w.patches.len(), 700);
    let rows = w.cache.rows_by_ids(&w.server).unwrap();
    assert!(rows.iter().all(|r| r.keywords.iter().any(|k| k.starts_with("pm.due"))));
}

/// Фишинг на всей выборке — обычная правка: слова у всех писем.
#[test]
fn phishing_covers_every_page() {
    let mut w = World::new(600, 100);
    let action = Action::Patch { act: Act::Phishing, archive_name: "Archive".into() };
    w.sweeper.start(w.selector(), action).unwrap();
    w.run();
    let rows = w.cache.rows_by_ids(&w.server).unwrap();
    assert!(rows.len() == 600 && rows.iter().all(|r| r.has_keyword(crate::filters::PHISHING)));
}

/// Удалить насовсем: кэш и сервер, всё.
#[test]
fn purge_covers_every_page() {
    let mut w = World::new(700, 500);
    w.sweeper.start(w.selector(), Action::Purge).unwrap();
    w.run();
    assert!(w.server.is_empty());
    assert_eq!(w.cache.count(INBOX).unwrap(), 0);
}

/// Экспорт: страницы приносят строки, задание экспорта получает все письма разом.
#[test]
fn export_gets_every_letter() {
    let mut w = World::new(700, 100);
    let action = Action::Export { format: Format::Eml, dir: "out".into() };
    w.sweeper.start(w.selector(), action).unwrap();
    let mut scope = None;
    for _ in 0..50 {
        let out = w.sweeper.poll(&mut w.engine, w.now).unwrap();
        for (route, c) in out.commands {
            match c {
                Command::Export { scope: s, .. } => {
                    assert_eq!(route, Route::Core);
                    scope = Some(s);
                }
                c => w.answer(c),
            }
        }
    }
    let Some(export::Scope::Letters(ids)) = scope else { panic!("экспорта нет") };
    assert_eq!(ids.len(), 700);
    assert_eq!(w.cache.count(INBOX).unwrap(), 700, "письма на месте");
}

/// Экспорт живёт до выхода: после перезапуска его нет.
#[test]
fn export_does_not_survive_a_restart() {
    let mut w = World::new(10, 0);
    w.sweeper.start(w.selector(), Action::Export { format: Format::Eml, dir: "out".into() }).unwrap();
    w.turn();
    w.restart();
    assert_eq!(w.sweeper.wake_at(w.now), None);
    assert!(w.cache.state(&page_key(SweepId(1), 0)).unwrap().is_none());
}

/// «Остановить» на снимке: письма не трогаем, задания нет.
#[test]
fn stop_while_listing() {
    let mut w = World::new(1200, 0);
    let to = w.local();
    let id = w.sweeper.start(w.selector(), Action::Transfer { to }).unwrap();
    w.until(|w| !w.listed.is_empty());
    w.sweeper.stop(id, &mut w.engine).unwrap();
    w.run();
    assert_eq!(w.end().state, State::Stopped);
    assert_eq!(w.server.len(), 1200);
    assert!(w.cache.jobs().unwrap().is_empty());
}
