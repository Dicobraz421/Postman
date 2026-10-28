//! Клиент EWS против боевого Exchange компании — только тестовые учётки (S7 «Проверка на боевом Exchange»). Стенда
//! Exchange нет: без окружения тесты пропускаются (`#[ignore]`), CI их не запускает.
//!
//! Правила: трогаем только тестовые учётки; встречи — только между ними (`test_recipient`); всё созданное — с меткой
//! `[postman-test <прогон>]` в теме и убирается и при падении теста (`guarded`); справочник и занятость — только
//! чтение (до 10 результатов, до 3 адресов на неделю); запросы — последовательно, не чаще 5 в секунду (клиент), в
//! опросе — раз в 3 с, не дольше минуты. Вывод — только счётчики и коды: ни адресов, ни имён, ни id.
//!
//! Окружение (значения — только у владельца, вне репозитория):
//!   POSTMAN_EWS_URL       адрес EWS (`https://…/EWS/Exchange.asmx`); нет — Autodiscover по POSTMAN_EWS_EMAIL
//!   POSTMAN_EWS_EMAIL     адрес первой тестовой учётки (если вход POSTMAN_EWS_USER — не адрес)
//!   POSTMAN_EWS_USER      вход первой тестовой учётки: `user@domain` (UPN) или `DOMAIN\user`
//!   POSTMAN_EWS_PASSWORD  её пароль (NTLM или Kerberos через Negotiate)
//!   POSTMAN_EWS_USER2     вторая тестовая учётка (для встреч), POSTMAN_EWS_PASSWORD2, POSTMAN_EWS_EMAIL2
//!
//! Запуск: cargo test -p postman-ews --test stand -- --ignored --test-threads=1

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)

use std::future::Future;
use std::time::{Duration, Instant};

use futures::FutureExt as _;
use postman_calendar::{FreeBusy, Partstat, Span};
use postman_ews::{
    Attendee, Auth, Changes, Client, DayOfWeek, DeleteMode, FolderRef, ItemKind, MailRole, NewItem, Notify, Pattern, Range, Recurrence, Reply,
    Source, WellKnown, discover,
};

struct Account {
    email: String,
    auth: Auth,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn need(name: &str) -> String {
    env(name).unwrap_or_else(|| panic!("{name} не задан: S7 «Проверка на боевом Exchange»"))
}

fn account(n: &str) -> Option<Account> {
    let user = env(&format!("POSTMAN_EWS_USER{n}"))?;
    let password = need(&format!("POSTMAN_EWS_PASSWORD{n}"));
    let email = env(&format!("POSTMAN_EWS_EMAIL{n}")).unwrap_or_else(|| {
        assert!(user.contains('@'), "POSTMAN_EWS_USER{n} — не адрес: задайте POSTMAN_EWS_EMAIL{n}");
        user.clone()
    });
    Some(Account { email, auth: Auth::password(user, password) })
}

fn first() -> Account {
    account("").unwrap_or_else(|| panic!("POSTMAN_EWS_USER не задан: S7 «Проверка на боевом Exchange»"))
}

/// Адрес участника встречи — только тестовые учётки из окружения; любой другой — паника до отправки.
fn test_recipient(email: &str) -> String {
    let allowed: Vec<String> = [account(""), account("2")].into_iter().flatten().map(|a| a.email.to_lowercase()).collect();
    assert!(allowed.contains(&email.to_lowercase()), "участник не из тестовых учёток — отправка запрещена");
    email.to_owned()
}

async fn client(a: &Account) -> Client {
    let url = match env("POSTMAN_EWS_URL") {
        Some(u) => u,
        None => discover(&a.email, &a.auth, None).await.unwrap().urls.remove(0),
    };
    Client::new(&url, a.auth.clone()).unwrap()
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

/// Метка прогона в теме: по ней убирается всё созданное.
fn tag() -> String {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
    format!("[postman-test {ms}]")
}

fn now() -> i64 {
    jiff::Timestamp::now().as_second()
}

/// Всё с меткой — насовсем, без рассылки: календарь, «Входящие» (приглашения, ответы, отмены), «Отправленные»,
/// «Удалённые».
async fn cleanup(c: &mut Client, tag: &str) {
    for folder in [WellKnown::Calendar, WellKnown::Inbox, WellKnown::Drafts, WellKnown::SentItems, WellKnown::DeletedItems] {
        match c.find_by_subject(folder, tag, 20).await {
            Ok(ids) if !ids.is_empty() => {
                if let Err(e) = c.delete(&ids, DeleteMode::HardDelete, Notify::None).await {
                    eprintln!("уборка: {:?} — {}", folder, e.code().unwrap_or("ошибка"));
                }
            }
            Ok(_) => {}
            Err(e) => eprintln!("уборка: {:?} — {}", folder, e.code().unwrap_or("ошибка")),
        }
    }
}

/// Тело теста, затем уборка по метке у всех учёток — и при падении; падение пробрасывается после уборки.
async fn guarded<F: Future<Output = ()>>(tag: &str, accounts: &[&Account], body: F) {
    let result = std::panic::AssertUnwindSafe(body).catch_unwind().await;
    for a in accounts {
        cleanup(&mut client(a).await, tag).await;
    }
    if let Err(p) = result {
        std::panic::resume_unwind(p);
    }
}

/// Ждать условия (тело — `async`-блок с `Option`): раз в 3 с, не дольше минуты (не больше 20 запросов).
macro_rules! poll {
    ($what:expr, $($body:tt)*) => {{
        let start = Instant::now();
        loop {
            let got: Option<_> = async { $($body)* }.await;
            if let Some(v) = got {
                break v;
            }
            assert!(start.elapsed() < Duration::from_secs(60), "{}: не дождались за минуту", $what);
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    }};
}

/// Все изменения папки до конца: состояние «сейчас».
async fn sync_to_end(c: &mut Client, state: Option<postman_ews::SyncState>) -> (postman_ews::SyncState, postman_ews::SyncChanges) {
    let mut state = state;
    let mut all = postman_ews::SyncChanges::default();
    for _ in 0..25 {
        let ch = c.sync(&FolderRef::WellKnown(WellKnown::Calendar), state.as_ref(), 20).await.unwrap();
        all.created.extend(ch.created);
        all.updated.extend(ch.updated);
        all.deleted.extend(ch.deleted);
        state = Some(ch.state.clone());
        if ch.done {
            return (ch.state, all);
        }
    }
    panic!("календарь тестовой учётки больше 500 объектов — это не тестовая учётка?");
}

/// Вход и версия: календарь читается.
#[test]
#[ignore = "боевой Exchange: S7 «Проверка на боевом Exchange»"]
fn sign_in() {
    rt().block_on(async {
        let mut c = client(&first()).await;
        let cal = c.calendar_folder().await.unwrap();
        assert!(!cal.id.id.is_empty());
        let v = c.server_version().expect("ServerVersionInfo");
        assert!(v.major >= 15, "Exchange 2016+ (15.1), а не {}.{}", v.major, v.minor);
        println!("Exchange {}.{}.{}; календарей: {}", v.major, v.minor, v.build, c.calendars().await.unwrap().len());
    });
}

/// Autodiscover POX находит адрес EWS.
#[test]
#[ignore = "боевой Exchange: S7 «Проверка на боевом Exchange»"]
fn autodiscover() {
    rt().block_on(async {
        let a = first();
        let found = discover(&a.email, &a.auth, None).await.unwrap();
        println!("Autodiscover: источник {:?}, адресов {}", found.source, found.urls.len());
        assert_eq!(found.source, Source::Autodiscover);
        let mut c = Client::new(&found.urls[0], a.auth.clone()).unwrap();
        c.calendar_folder().await.unwrap();
    });
}

/// Неверный пароль — ошибка входа (одна попытка: не блокировать учётку). Нужен POSTMAN_EWS_URL.
#[test]
#[ignore = "боевой Exchange: S7 «Проверка на боевом Exchange»"]
fn wrong_password() {
    rt().block_on(async {
        let a = first();
        let Auth::Password { user, .. } = &a.auth else { unreachable!() };
        let bad = Auth::password(user.clone(), "not-the-password");
        let err = Client::new(&need("POSTMAN_EWS_URL"), bad).unwrap().calendar_folder().await.unwrap_err();
        assert!(matches!(err, postman_ews::Error::Auth(_)), "{err}");
    });
}

/// Своя встреча без участников: создание, синхронизация, чтение, правка, окно, удаление.
#[test]
#[ignore = "боевой Exchange: S7 «Проверка на боевом Exchange»"]
fn ews_event_round_trip() {
    rt().block_on(async {
        let a = first();
        let tag = tag();
        guarded(&tag, &[&a], async {
            let mut c = client(&a).await;
            let (state, _) = sync_to_end(&mut c, None).await;
            let start = (now() / 3600 + 48) * 3600;
            let item = NewItem {
                title: format!("Проверка {tag}"),
                description: "Строка 1\nСтрока 2".into(),
                location: "Нигде".into(),
                span: Span::new(start, start + 1800),
                all_day: false,
                time_zone: Some("Europe/Moscow".into()),
                free_busy: FreeBusy::Free,
                required: vec![],
                optional: vec![],
                recurrence: None,
            };
            let id = c.create(None, &item, Notify::None).await.unwrap();

            let (state, ch) = sync_to_end(&mut c, Some(state)).await;
            assert!(ch.created.iter().any(|i| i.id == id.id), "SyncFolderItems не увидел новую встречу");
            let got = c.get_items(std::slice::from_ref(&id)).await.unwrap().pop().expect("встреча");
            assert_eq!(got.kind, ItemKind::Single);
            assert_eq!((got.event.title.as_str(), got.event.location.as_str()), (item.title.as_str(), "Нигде"));
            assert_eq!(got.event.span, item.span);
            assert_eq!(got.event.free_busy, FreeBusy::Free);
            assert!(got.event.description.contains("Строка 2"));
            assert!(!got.event.uid.is_empty(), "UID iCalendar");

            let changes = Changes { location: Some("Зал".into()), ..Changes::default() };
            let id = c.update(&got.id, &changes, Notify::None).await.unwrap();
            let (state, ch) = sync_to_end(&mut c, Some(state)).await;
            assert!(ch.updated.iter().any(|i| i.id == id.id), "SyncFolderItems не увидел правку");
            assert_eq!(c.get_items(std::slice::from_ref(&id)).await.unwrap()[0].event.location, "Зал");

            let (view, _) = c.calendar_view(&FolderRef::WellKnown(WellKnown::Calendar), Span::new(start - 60, start + 3600), 20).await.unwrap();
            assert!(view.iter().any(|v| v.id.id == id.id));

            c.delete(std::slice::from_ref(&id), DeleteMode::HardDelete, Notify::None).await.unwrap();
            let (_, ch) = sync_to_end(&mut c, Some(state)).await;
            assert!(ch.deleted.contains(&id.id), "SyncFolderItems не увидел удаление");
            println!("встреча: создана, изменена, удалена");
        })
        .await;
    });
}

/// Серия: правило и экземпляры, развёрнутые сервером.
#[test]
#[ignore = "боевой Exchange: S7 «Проверка на боевом Exchange»"]
fn ews_recurring_series() {
    rt().block_on(async {
        let a = first();
        let tag = tag();
        guarded(&tag, &[&a], async {
            let mut c = client(&a).await;
            // Ближайший понедельник через неделю, 10:00 по Москве.
            let tz = jiff::tz::TimeZone::get("Europe/Moscow").unwrap();
            let mut day = jiff::Timestamp::now().to_zoned(tz.clone()).date().checked_add(jiff::ToSpan::days(7)).unwrap();
            while day.weekday() != jiff::civil::Weekday::Monday {
                day = day.tomorrow().unwrap();
            }
            let start = day.at(10, 0, 0, 0).to_zoned(tz).unwrap().timestamp().as_second();
            let item = NewItem {
                title: format!("Серия {tag}"),
                description: String::new(),
                location: String::new(),
                span: Span::new(start, start + 900),
                all_day: false,
                time_zone: Some("Europe/Moscow".into()),
                free_busy: FreeBusy::Free,
                required: vec![],
                optional: vec![],
                recurrence: Some(Recurrence {
                    pattern: Pattern::Weekly { interval: 1, days: vec![DayOfWeek::Monday], first_day: None },
                    range: Range::Numbered { start: day, count: 3 },
                }),
            };
            let id = c.create(None, &item, Notify::None).await.unwrap();
            let master = c.get_items(std::slice::from_ref(&id)).await.unwrap().pop().unwrap();
            assert_eq!(master.kind, ItemKind::RecurringMaster);
            // Первый день недели сервер ставит сам (боевой 2013 — воскресенье, `WKST=SU`).
            let rule = master.recurrence.as_ref().map(|r| r.rrule()).unwrap_or_default();
            assert!(rule.starts_with("FREQ=WEEKLY;INTERVAL=1;BYDAY=MO") && rule.ends_with(";COUNT=3"), "{rule}");
            let window = Span::new(start - 60, start + 21 * 86_400);
            let (view, _) = c.calendar_view(&FolderRef::WellKnown(WellKnown::Calendar), window, 20).await.unwrap();
            let occ: Vec<_> = view.iter().filter(|v| v.kind == ItemKind::Occurrence).collect();
            assert!(occ.len() >= 3, "экземпляров серии: {}", occ.len());
            assert!(occ.iter().any(|o| o.span.start == start + 7 * 86_400), "второй экземпляр — через неделю, то же местное время");
            c.delete(std::slice::from_ref(&id), DeleteMode::HardDelete, Notify::None).await.unwrap();
            println!("серия: правило и {} экземпляра", occ.len());
        })
        .await;
    });
}

/// Встреча user1 → user2: приглашение, ответ, перенос, отмена; уборка у обоих.
#[test]
#[ignore = "боевой Exchange: S7 «Проверка на боевом Exchange»; нужна POSTMAN_EWS_USER2"]
fn ews_meeting_cycle() {
    rt().block_on(async {
        let (a, b) = (first(), account("2").expect("POSTMAN_EWS_USER2"));
        let tag = tag();
        guarded(&tag, &[&a, &b], async {
            let (mut c1, mut c2) = (client(&a).await, client(&b).await);
            let start = (now() / 3600 + 72) * 3600;
            let item = NewItem {
                title: format!("Встреча {tag}"),
                description: "Проверка приглашений".into(),
                location: String::new(),
                span: Span::new(start, start + 1800),
                all_day: false,
                time_zone: Some("Europe/Moscow".into()),
                free_busy: FreeBusy::Busy,
                required: vec![Attendee { name: String::new(), email: test_recipient(&b.email) }],
                optional: vec![],
                recurrence: None,
            };
            let id = c1.create(None, &item, Notify::All).await.unwrap();

            // У второй учётки встреча появляется в календаре (сервер кладёт её при доставке приглашения).
            let invited =
                poll!("приглашение у user2", c2.find_by_subject(WellKnown::Calendar, &tag, 5).await.ok().and_then(|v| v.into_iter().next()));
            let got = c2.get_items(std::slice::from_ref(&invited)).await.unwrap().pop().unwrap();
            assert!(got.is_meeting && !got.is_organizer);
            assert_eq!(got.my_response, Partstat::NeedsAction);
            c2.respond(&got.id, Reply::Accept, None, false).await.unwrap();

            // Ответ дошёл до организатора.
            poll!("ответ у организатора",
                let it = c1.get_items(std::slice::from_ref(&id)).await.ok()?.pop()?;
                it.event
                    .participants
                    .iter()
                    .any(|p| p.email.eq_ignore_ascii_case(&b.email) && p.status == Partstat::Accepted)
                    .then_some(())
            );

            // Перенос на час: изменение — всем участникам.
            let current = c1.get_items(std::slice::from_ref(&id)).await.unwrap().pop().unwrap().id;
            let moved = Changes { time: Some((Span::new(start + 3600, start + 5400), false)), ..Changes::default() };
            let id = c1.update(&current, &moved, Notify::All).await.unwrap();
            poll!("перенос у user2",
                let ids = c2.find_by_subject(WellKnown::Calendar, &tag, 5).await.ok()?;
                let it = c2.get_items(&ids).await.ok()?.pop()?;
                (it.event.span.start == start + 3600).then_some(())
            );

            // Отмена: у второй учётки встреча помечена отменённой.
            c1.delete(std::slice::from_ref(&id), DeleteMode::HardDelete, Notify::All).await.unwrap();
            poll!("отмена у user2",
                let ids = c2.find_by_subject(WellKnown::Calendar, &tag, 5).await.ok()?;
                let it = c2.get_items(&ids).await.ok()?.pop()?;
                (it.event.status == postman_calendar::EventStatus::Cancelled).then_some(())
            );
            println!("встреча: приглашение, ответ, перенос, отмена");
        })
        .await;
    });
}

/// Занятость тестовых учёток на неделю (только чтение, до 3 адресов).
#[test]
#[ignore = "боевой Exchange: S7 «Проверка на боевом Exchange»"]
fn ews_availability() {
    rt().block_on(async {
        let a = first();
        let mut emails = vec![a.email.clone()];
        emails.extend(account("2").map(|b| b.email));
        let refs: Vec<&str> = emails.iter().map(String::as_str).collect();
        let start = now();
        let got = client(&a).await.availability(&refs, Span::new(start, start + 7 * 86_400)).await.unwrap();
        assert_eq!(got.len(), refs.len());
        for x in &got {
            assert_eq!(x.error, None, "занятость тестовой учётки");
        }
        println!("занятость: {} адресов, интервалов {}", got.len(), got.iter().map(|x| x.busy.len()).sum::<usize>());
    });
}

/// Справочник: своя учётка находится по адресу (только чтение, до 10 результатов).
#[test]
#[ignore = "боевой Exchange: S7 «Проверка на боевом Exchange»"]
fn ews_resolve_names() {
    rt().block_on(async {
        let a = first();
        let people = client(&a).await.resolve_names(&a.email, 10).await.unwrap();
        assert!(people.iter().any(|p| p.email.eq_ignore_ascii_case(&a.email)), "своя учётка в GAL");
        assert!(people.len() <= 10);
        println!("справочник: найдено {}", people.len());
    });
}

/// Справочник по `FindPeople` (GAL, Exchange 2013+; только чтение, до 10): своя учётка по началу адреса; печать —
/// только счётчики, у кого есть должность, отдел, телефон.
#[test]
#[ignore = "боевой Exchange: S7 «Проверка на боевом Exchange»"]
fn ews_find_people() {
    rt().block_on(async {
        let a = first();
        let start: String = a.email.split('@').next().unwrap_or("").chars().take(6).collect();
        let people = client(&a).await.find_people(&start, 10).await.unwrap_or_else(|e| panic!("FindPeople: {}", e.code().unwrap_or("ошибка")));
        let count = |f: fn(&postman_ews::Person) -> bool| people.iter().filter(|p| f(p)).count();
        println!(
            "FindPeople: найдено {}, своя учётка {}, с должностью {}, с отделом {}, с телефоном {}",
            people.len(),
            people.iter().any(|p| p.email.eq_ignore_ascii_case(&a.email)),
            count(|p| !p.title.is_empty()),
            count(|p| !p.department.is_empty()),
            count(|p| !p.phone.is_empty())
        );
        assert!(people.iter().any(|p| p.email.eq_ignore_ascii_case(&a.email)), "своя учётка в GAL");
        assert!(people.len() <= 10);
    });
}

/// Письмо (RFC 5322) себе с меткой в теме.
fn letter(me: &str, subject: &str) -> Vec<u8> {
    let id = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    format!(
        "From: <{me}>
To: <{me}>
Subject: {subject}
Message-ID: <{id}@postman.test>
Content-Type: text/plain; charset=utf-8

Проверка почты Exchange по EWS.
"
    )
    .into_bytes()
}

/// Дерево папок и роли: вход, отправленные и удалённые на месте.
#[test]
#[ignore = "боевой Exchange: S7 «Проверка на боевом Exchange»"]
fn ews_mail_folders() {
    rt().block_on(async {
        let mut c = client(&first()).await;
        let roles = c.mail_roles().await.unwrap();
        assert!(roles.iter().any(|(r, _)| *r == MailRole::Inbox), "роль «Входящие»");
        let mut state = None;
        let mut all = Vec::new();
        let mut via = "SyncFolderHierarchy";
        for _ in 0..25 {
            match c.sync_folders(state.as_ref(), &roles).await {
                Ok(ch) => {
                    all.extend(ch.created);
                    all.extend(ch.updated);
                    let done = ch.done;
                    state = Some(ch.state);
                    if done {
                        break;
                    }
                }
                // Боевой Exchange 2013 CU23 отвечает на SyncFolderHierarchy ErrorInternalServerError (баг сервера,
                // docs/EXCHANGE.md); роли из GetFolder при этом работают.
                Err(e) if e.code() == Some("ErrorInternalServerError") => {
                    // Запасной путь — FindFolder (Deep).
                    via = "FindFolder";
                    all = c.find_folders(&roles).await.unwrap();
                    break;
                }
                Err(e) => panic!("sync_folders: {}", e.code().unwrap_or("ошибка")),
            }
        }
        assert!(all.iter().any(|f| f.role == Some(MailRole::Inbox)), "«Входящие» в дереве");
        let (page, _) = c.mail_page(&FolderRef::WellKnown(WellKnown::Inbox), 0, 5).await.unwrap();
        assert!(page.len() <= 5);
        println!("дерево — {via}: папок {}, вложенных {}; ролей {}", all.len(), all.iter().filter(|f| f.parent.is_some()).count(), roles.len());
    });
}

/// Письмо себе: отправка, появление во «Входящих», заголовок, тело (MIME), прочитано, флаг, перенос, удаление.
#[test]
#[ignore = "боевой Exchange: S7 «Проверка на боевом Exchange»"]
fn ews_mail_self_round_trip() {
    rt().block_on(async {
        let a = first();
        let tag = tag();
        guarded(&tag, &[&a], async {
            let mut c = client(&a).await;
            let me = test_recipient(&a.email);
            let subject = format!("{tag} self");
            c.send_mail(&letter(&me, &subject), true).await.unwrap();

            let id = poll!("письмо во «Входящих»", c.find_by_subject(WellKnown::Inbox, &tag, 5).await.ok().and_then(|v| v.into_iter().next()));
            let h = c.mail_headers(std::slice::from_ref(&id)).await.unwrap().pop().expect("заголовок");
            assert!(h.subject.contains(&tag) && h.from_addr.eq_ignore_ascii_case(&me) && h.size > 0);

            let mail = c.get_mail(&id).await.unwrap().expect("письмо");
            assert!(!mail.mime.is_empty());
            let text = String::from_utf8_lossy(&mail.mime);
            assert!(text.contains(&subject), "MIME содержит тему");

            let id = c.set_read(&id, true).await.unwrap();
            let h = c.mail_headers(std::slice::from_ref(&id)).await.unwrap().pop().unwrap();
            assert!(h.is_read, "прочитано");
            let id = c.set_flag(&id, true).await.unwrap();
            assert!(!id.id.is_empty(), "флаг поставлен");

            let moved = c.move_mail(std::slice::from_ref(&id), &FolderRef::WellKnown(WellKnown::DeletedItems)).await.unwrap();
            assert_eq!(moved.len(), 1);
            c.delete_mail(&moved, DeleteMode::HardDelete).await.unwrap();
            println!("письмо себе: отправлено, прочитано, флаг, перенос, удалено");
        })
        .await;
    });
}

/// Письмо между тестовыми учётками: user1 → user2, у user2 во «Входящих», тело читается.
#[test]
#[ignore = "боевой Exchange: S7 «Проверка на боевом Exchange»; нужна POSTMAN_EWS_USER2"]
fn ews_mail_between_accounts() {
    rt().block_on(async {
        let (a, b) = (first(), account("2").expect("POSTMAN_EWS_USER2"));
        let tag = tag();
        guarded(&tag, &[&a, &b], async {
            let (mut c1, mut c2) = (client(&a).await, client(&b).await);
            let to = test_recipient(&b.email);
            let subject = format!("{tag} cross");
            let mime = format!(
                "From: <{}>
To: <{to}>
Subject: {subject}
Content-Type: text/plain; charset=utf-8

Между тестовыми учётками.
",
                a.email
            );
            c1.send_mail(mime.as_bytes(), true).await.unwrap();

            let id = poll!("письмо у user2", c2.find_by_subject(WellKnown::Inbox, &tag, 5).await.ok().and_then(|v| v.into_iter().next()));
            let mail = c2.get_mail(&id).await.unwrap().expect("письмо");
            let text = String::from_utf8_lossy(&mail.mime);
            assert!(text.contains(&subject) && text.contains("Subject:"), "MIME доставлено целиком");
            println!("письмо user1 → user2: доставлено и прочитано");
        })
        .await;
    });
}

/// Черновик: сохранение в «Черновики», поиск по теме, удаление.
#[test]
#[ignore = "боевой Exchange: S7 «Проверка на боевом Exchange»"]
fn ews_mail_draft() {
    rt().block_on(async {
        let a = first();
        let tag = tag();
        guarded(&tag, &[&a], async {
            let mut c = client(&a).await;
            let subject = format!("{tag} draft");
            let id = c.save_draft(&letter(&a.email, &subject), None).await.unwrap();
            assert!(!id.id.is_empty());
            let found = poll!("черновик в «Черновиках»", c.find_by_subject(WellKnown::Drafts, &tag, 5).await.ok().and_then(|v| v.into_iter().next()));
            c.delete_mail(std::slice::from_ref(&found), DeleteMode::HardDelete).await.unwrap();
            println!("черновик: сохранён, найден, удалён");
        })
        .await;
    });
}

/// Папка: создать в корне ящика → переименовать → перенести во «Входящие» → удалить насовсем (T1-04а). Имя — с
/// меткой прогона; папка удаляется и при падении теста.
#[test]
#[ignore = "боевой Exchange: S7 «Проверка на боевом Exchange»"]
fn ews_mail_folder_ops() {
    rt().block_on(async {
        let mut c = client(&first()).await;
        let tag = tag();
        let roles = c.mail_roles().await.unwrap();
        let inbox = roles.iter().find(|(r, _)| *r == MailRole::Inbox).map(|(_, id)| id.clone()).expect("«Входящие»");
        let id = c.create_folder(&FolderRef::WellKnown(WellKnown::Root), &format!("{tag} folder")).await.unwrap();
        let result = std::panic::AssertUnwindSafe(async {
            let again = c.create_folder(&FolderRef::WellKnown(WellKnown::Root), &format!("{tag} folder")).await.unwrap_err();
            assert_eq!(again.code(), Some("ErrorFolderExists"), "то же имя");
            let id = c.rename_folder(&id, &format!("{tag} renamed")).await.unwrap();
            let id = c.move_folder(&id, &FolderRef::Id(inbox.clone())).await.unwrap();
            let tree = c.find_folders(&roles).await.unwrap();
            let f = tree.iter().find(|f| f.name == format!("{tag} renamed")).expect("папка в дереве под новым именем");
            assert_eq!(f.parent.as_deref(), Some(inbox.id.as_str()), "родитель — «Входящие»");
            id
        })
        .catch_unwind()
        .await;
        let last = result.as_ref().map(|id| id.clone()).unwrap_or(id);
        c.delete_folder(&last, DeleteMode::HardDelete).await.unwrap_or_else(|e| eprintln!("уборка папки: {}", e.code().unwrap_or("ошибка")));
        if let Err(p) = result {
            std::panic::resume_unwind(p);
        }
        println!("папка: создана, имя занято, переименована, перенесена, удалена");
    });
}
