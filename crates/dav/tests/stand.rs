//! Клиент CalDAV и CardDAV против стенда Stalwart (`docs/STALWART.md`): поиск, событие, занятость между двумя
//! пользователями, встреча с неявным планированием, карточка. Без стенда тесты пропускаются (`#[ignore]`).
//! Каждый тест убирает за собой: ресурсы, записи ACL, приглашения во входящих планирования; письма сервера о
//! встрече — по IMAP, если задан `POSTMAN_STAND_IMAP`.
//!
//! Запуск (пароли — только в окружении; самоподписанный сертификат стенда — `POSTMAN_INSECURE_TLS=1`):
//!   POSTMAN_STAND_DAV=https://<адрес стенда> POSTMAN_STAND_USER1=user1@itt.test POSTMAN_STAND_PASSWORD1=…
//!   POSTMAN_STAND_USER2=user2@itt.test POSTMAN_STAND_PASSWORD2=… [POSTMAN_STAND_IMAP=<адрес>:993]
//!   cargo test -p postman-dav --test stand -- --ignored --test-threads=1
//! Без `POSTMAN_STAND_DAV` — `POSTMAN_STAND_URL` (тот же HTTPS, что у JMAP). Печатаются только счётчики и коды.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)

use std::future::Future;
use std::time::{Duration, Instant};

use postman_calendar::Span;
use postman_dav::{Client, Config, Content, Discovery, Error, Precondition, Privilege, child_href, same_href};

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} не задан: стенд — docs/STALWART.md"))
}

fn config(n: u8) -> Config {
    let url = std::env::var("POSTMAN_STAND_DAV").unwrap_or_else(|_| env("POSTMAN_STAND_URL"));
    Config { url, user: env(&format!("POSTMAN_STAND_USER{n}")), password: env(&format!("POSTMAN_STAND_PASSWORD{n}")) }
}

fn client(n: u8) -> Client {
    Client::new(&config(n)).unwrap()
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

fn stamp() -> String {
    format!("{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis())
}

fn now() -> i64 {
    jiff::Timestamp::now().as_second()
}

fn utc(secs: i64) -> String {
    jiff::Timestamp::from_second(secs).unwrap().strftime("%Y%m%dT%H%M%SZ").to_string()
}

/// Окно в будущем, кратное часу: через `days` дней в 10:00 UTC, час.
fn slot(days: i64) -> Span {
    let day = (now() / 86_400 + days) * 86_400;
    Span::new(day + 10 * 3600, day + 11 * 3600)
}

fn event(uid: &str, summary: &str, at: Span, extra: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Postman//DAV stand//RU\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:{}\r\nDTSTART:{}\r\nDTEND:{}\r\nSUMMARY:{summary}\r\n{extra}END:VEVENT\r\nEND:VCALENDAR\r\n",
        utc(now()),
        utc(at.start),
        utc(at.end)
    )
}

/// Строки iCalendar без переносов (RFC 5545 §3.1).
fn unfold(ics: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in ics.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        match line.strip_prefix([' ', '\t']) {
            Some(rest) if !out.is_empty() => out.last_mut().unwrap().push_str(rest),
            _ => out.push(line.to_owned()),
        }
    }
    out
}

/// Строка `ATTENDEE` участника.
fn attendee_line(ics: &str, email: &str) -> Option<String> {
    let needle = format!("mailto:{}", email.to_ascii_lowercase());
    unfold(ics).into_iter().find(|l| l.to_ascii_uppercase().starts_with("ATTENDEE") && l.to_ascii_lowercase().ends_with(&needle))
}

async fn until<T, F: Future<Output = Option<T>>>(what: &str, mut f: impl FnMut() -> F) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f().await {
            return v;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "{what}: не дождались за 30 с");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn default_calendar(c: &Client, d: &Discovery) -> String {
    let home = d.calendar_home.first().expect("calendar-home-set");
    let cals = c.calendars(home).await.unwrap();
    cals.iter().find(|c| c.supports("VEVENT") && c.writable).map(|c| c.href.clone()).expect("календарь с VEVENT")
}

/// Уборка и при падении теста: ресурсы (пользователь, путь) и права (пользователь, коллекция, принципал).
/// В `Drop` — своим рантаймом в отдельном потоке: внутри `block_on` теста второй рантайм не запустить.
#[derive(Default)]
struct Todo {
    resources: Vec<(u8, String)>,
    shares: Vec<(u8, String, String)>,
    /// Входящие планирования (пользователь, ящик) и строка, по которой искать приглашения теста.
    inboxes: Vec<(u8, String)>,
    marker: String,
}

struct Cleanup(Todo);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let (resources, shares, inboxes, marker) =
            (std::mem::take(&mut self.0.resources), std::mem::take(&mut self.0.shares), std::mem::take(&mut self.0.inboxes), self.0.marker.clone());
        let done = std::thread::spawn(move || {
            rt().block_on(async {
                let mut n = 0;
                for (user, href) in resources {
                    n += usize::from(client(user).delete(&href, Precondition::None).await.unwrap_or(false));
                }
                for (user, href, principal) in shares {
                    let _ = client(user).unshare(&href, &principal).await;
                }
                for (user, inbox) in inboxes {
                    n += clean_inbox(&client(user), &inbox, &marker).await;
                }
                n
            })
        })
        .join();
        if let Ok(n) = done
            && n > 0
        {
            println!("уборка: удалено ресурсов {n}");
        }
    }
}

/// Удалить из входящих планирования то, что относится к тесту.
async fn clean_inbox(c: &Client, inbox: &str, marker: &str) -> usize {
    if marker.is_empty() {
        return 0;
    }
    let Ok(items) = c.inbox(inbox).await else { return 0 };
    let mut n = 0;
    for r in items.iter().filter(|r| r.data.contains(marker)) {
        n += usize::from(c.delete(&r.href, Precondition::None).await.unwrap_or(false));
    }
    n
}

/// Письма сервера о встрече (приглашение, ответ, отмена) — по IMAP во «Входящих» обоих, если задан IMAP стенда.
async fn clean_letters(marker: &str) {
    let Ok(at) = std::env::var("POSTMAN_STAND_IMAP") else {
        println!("письма сервера не убраны: POSTMAN_STAND_IMAP не задан");
        return;
    };
    let (host, port) = at.rsplit_once(':').expect("host:port");
    let port: u16 = port.parse().expect("port");
    let security = if port == 993 { postman_imap::Security::Tls } else { postman_imap::Security::StartTls };
    tokio::time::sleep(Duration::from_secs(5)).await;
    for n in [1u8, 2] {
        let cfg = postman_imap::Config {
            host: host.into(),
            port,
            security,
            user: env(&format!("POSTMAN_STAND_USER{n}")),
            password: env(&format!("POSTMAN_STAND_PASSWORD{n}")),
        };
        let Ok(mut s) = postman_imap::Session::connect(&cfg).await else { continue };
        if s.select("INBOX").await.is_ok()
            && let Ok(uids) = s.search_text(marker).await
            && !uids.is_empty()
        {
            let _ = s.delete(&postman_imap::uid_set(&uids)).await;
            println!("user{n}: удалено писем сервера {}", uids.len());
        }
        let _ = s.logout().await;
    }
}

/// Поиск: принципал, домашние наборы, ящики планирования, календарь и адресная книга.
#[test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
fn dav_discovery() {
    rt().block_on(async {
        let me = env("POSTMAN_STAND_USER1");
        let c = client(1);
        let d = c.discover().await.unwrap();
        assert!(!d.principal.is_empty());
        assert_eq!(d.email().map(str::to_ascii_lowercase), Some(me.to_ascii_lowercase()));
        assert!(d.has_scheduling() && d.schedule_inbox.is_some());
        let cals = c.calendars(&d.calendar_home[0]).await.unwrap();
        assert!(cals.iter().any(|c| c.supports("VEVENT") && c.supports("VTODO") && c.sync_token.is_some() && c.ctag.is_some()));
        let books = c.address_books(&d.addressbook_home[0]).await.unwrap();
        assert!(!books.is_empty() && books[0].sync_token.is_some());
        println!("календарей {}, адресных книг {}", cals.len(), books.len());
    });
}

/// Событие: создать → sync-collection → multiget, calendar-query → правка с If-Match (старый ETag — 412) →
/// удалить → sync-collection (удаление). ETag и ctag — запасной путь синхронизации.
#[test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
fn caldav_event_round_trip() {
    rt().block_on(async {
        let c = client(1);
        let d = c.discover().await.unwrap();
        let cal = default_calendar(&c, &d).await;
        let before = c.sync(&cal, None).await.unwrap();
        assert!(before.full);
        let tokens = c.tokens(&cal).await.unwrap();

        let uid = format!("postman-dav-{}", stamp());
        let href = child_href(&cal, &uid, Content::Calendar);
        let mut guard = Cleanup(Todo { resources: vec![(1, href.clone())], ..Default::default() });
        let at = slot(40);
        let w = c.put(&href, Content::Calendar, &event(&uid, "Проверка CalDAV", at, ""), Precondition::Create).await.unwrap();
        assert!(w.created);
        let again = c.put(&href, Content::Calendar, &event(&uid, "Проверка CalDAV", at, ""), Precondition::Create).await;
        assert!(matches!(again, Err(Error::Precondition)), "{again:?}");

        let ch = c.sync(&cal, Some(&before.token)).await.unwrap();
        assert!(!ch.full && !ch.truncated, "{ch:?}");
        let item = ch.changed.iter().find(|i| same_href(&i.href, &href)).expect("новое событие в sync-collection");
        if let Some(e) = &w.etag {
            assert_eq!(&item.etag, e);
        }
        assert_ne!(c.tokens(&cal).await.unwrap(), tokens, "ctag и sync-token меняются");

        let got = c.calendar_multiget(&cal, &[item.href.clone(), child_href(&cal, "nope", Content::Calendar)]).await.unwrap();
        assert_eq!(got.missing.len(), 1);
        assert!(got.resources[0].data.contains(&uid) && got.resources[0].data.contains("SUMMARY:Проверка CalDAV"));
        let window = c.calendar_query(&cal, "VEVENT", Some(Span::new(at.start - 3600, at.end + 3600))).await.unwrap();
        assert!(window.iter().any(|r| r.data.contains(&uid)));
        let outside = c.calendar_query(&cal, "VEVENT", Some(Span::new(at.end + 86_400, at.end + 2 * 86_400))).await.unwrap();
        assert!(!outside.iter().any(|r| r.data.contains(&uid)));

        let old = item.etag.clone();
        let w2 = c.put(&href, Content::Calendar, &event(&uid, "Проверка CalDAV 2", at, "SEQUENCE:1\r\n"), Precondition::Etag(&old)).await.unwrap();
        assert!(!w2.created);
        let stale = c.put(&href, Content::Calendar, &event(&uid, "x", at, ""), Precondition::Etag(&old)).await;
        assert!(matches!(stale, Err(Error::Precondition)), "{stale:?}");
        let listed = c.etags(&cal).await.unwrap();
        let now_etag = listed.iter().find(|i| same_href(&i.href, &href)).map(|i| i.etag.clone()).expect("в списке ETag");
        assert_ne!(now_etag, old);
        let (changed, removed) = postman_dav::diff(&[postman_dav::Item { href: href.clone(), etag: old.clone() }], &listed);
        assert!(changed.iter().any(|i| same_href(&i.href, &href)) && removed.is_empty());
        let fetched = c.get(&href).await.unwrap();
        assert!(fetched.data.contains("Проверка CalDAV 2") && fetched.etag.as_deref() == Some(now_etag.as_str()));

        // Неизвестный токен: по RFC 6578 — 403 valid-sync-token; Stalwart — полный список (узнаём по `full`).
        match c.sync(&cal, Some("urn:postman:not-a-token")).await {
            Err(Error::SyncToken) => println!("неизвестный токен: valid-sync-token"),
            Ok(full) => {
                assert!(full.full, "неизвестный токен без признака полного списка");
                println!("неизвестный токен: полный список ({} ресурсов)", full.changed.len());
            }
            Err(e) => panic!("неизвестный токен: {e:?}"),
        }

        assert!(matches!(c.delete(&href, Precondition::Etag(&old)).await, Err(Error::Precondition)));
        assert!(c.delete(&href, Precondition::Etag(&now_etag)).await.unwrap());
        let gone = c.sync(&cal, Some(&ch.token)).await.unwrap();
        assert!(gone.removed.iter().any(|h| same_href(h, &href)), "{gone:?}");
        assert!(!c.delete(&href, Precondition::None).await.unwrap(), "второй раз — 404");
        assert!(matches!(c.get(&href).await, Err(Error::NotFound)));
        guard.0.resources.clear();
    });
}

/// Занятость: событие у user2 → календарь user2 открыт user1 (ACL: read и read-free-busy) → POST VFREEBUSY в
/// outbox user1 → занятость user2 в окне; неизвестный адрес — 3.x без занятости → права и событие убраны.
#[test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
fn caldav_free_busy() {
    rt().block_on(async {
        let (c1, c2) = (client(1), client(2));
        let (d1, d2) = (c1.discover().await.unwrap(), c2.discover().await.unwrap());
        let (me, peer) = (d1.email().unwrap().to_owned(), d2.email().unwrap().to_owned());
        let cal2 = default_calendar(&c2, &d2).await;
        let outbox = d1.schedule_outbox.clone().expect("schedule-outbox-URL");
        let at = slot(41);
        let range = Span::new(at.start - 3 * 3600, at.end + 3 * 3600);
        let domain = me.rsplit_once('@').map(|(_, d)| d).unwrap();
        let nobody = format!("nobody-{}@{domain}", stamp());

        let before = c1.free_busy(&outbox, &me, &[&peer], range, &format!("postman-fb-{}", stamp())).await.unwrap();
        println!("до открытия: ответов {}, периодов {:?}", before.len(), before[0].busy.as_ref().map(Vec::len));

        let uid = format!("postman-dav-fb-{}", stamp());
        let href = child_href(&cal2, &uid, Content::Calendar);
        let mut guard =
            Cleanup(Todo { resources: vec![(2, href.clone())], shares: vec![(2, cal2.clone(), d1.principal.clone())], ..Default::default() });
        c2.put(&href, Content::Calendar, &event(&uid, "Занят", at, ""), Precondition::Create).await.unwrap();
        c2.share(&cal2, &d1.principal, &[Privilege::read(), Privilege::read_free_busy()]).await.unwrap();
        assert!(c2.acl(&cal2).await.unwrap().iter().any(|a| matches!(&a.who, postman_dav::acl::Who::Href(h) if same_href(h, &d1.principal))));

        let replies = c1.free_busy(&outbox, &me, &[&peer, &nobody], range, &format!("postman-fb-{}", stamp())).await.unwrap();
        let p = replies.iter().find(|r| r.email.eq_ignore_ascii_case(&peer)).expect("ответ по user2");
        assert!(p.ok(), "{}", p.status);
        let busy = p.busy.as_ref().expect("занятость user2");
        assert!(busy.iter().any(|b| b.span == at), "событие user2 — занятость в окне: {busy:?}");
        let n = replies.iter().find(|r| r.email.eq_ignore_ascii_case(&nobody)).expect("ответ по неизвестному");
        assert!(!n.ok() && n.busy.is_none(), "{}", n.status);
        println!("после открытия: периодов {}, неизвестный — {}", busy.len(), n.status.split(';').next().unwrap_or(""));

        assert!(c2.unshare(&cal2, &d1.principal).await.unwrap());
        assert!(c2.delete(&href, Precondition::None).await.unwrap());
        guard.0.resources.clear();
        guard.0.shares.clear();
    });
}

/// Встреча с неявным планированием (RFC 6638): user1 кладёт событие с ORGANIZER и ATTENDEE user2 → сервер кладёт
/// копию в календарь user2 (со Schedule-Tag) → user2 принимает правкой своей копии с If-Schedule-Tag-Match →
/// у организатора PARTSTAT=ACCEPTED → организатор удаляет → у user2 копия отменена или удалена.
#[test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
fn caldav_meeting_cycle() {
    rt().block_on(async {
        let (c1, c2) = (client(1), client(2));
        let (d1, d2) = (c1.discover().await.unwrap(), c2.discover().await.unwrap());
        let (me, peer) = (d1.email().unwrap().to_owned(), d2.email().unwrap().to_owned());
        let (cal1, cal2) = (default_calendar(&c1, &d1).await, default_calendar(&c2, &d2).await);
        let uid = format!("postman-dav-meet-{}", stamp());
        let href = child_href(&cal1, &uid, Content::Calendar);
        let mut guard = Cleanup(Todo {
            resources: vec![(1, href.clone())],
            inboxes: [(1u8, &d1), (2, &d2)].into_iter().filter_map(|(n, d)| Some((n, d.schedule_inbox.clone()?))).collect(),
            marker: uid.clone(),
            ..Default::default()
        });
        let at = slot(42);
        let people = format!(
            "SEQUENCE:0\r\nORGANIZER;CN=Postman 1:mailto:{me}\r\nATTENDEE;CN=Postman 1;ROLE=CHAIR;PARTSTAT=ACCEPTED:mailto:{me}\r\nATTENDEE;CN=Postman 2;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:{peer}\r\n"
        );
        let w = c1.put(&href, Content::Calendar, &event(&uid, &format!("Встреча {uid}"), at, &people), Precondition::Create).await.unwrap();
        println!("организатор: Schedule-Tag в ответе PUT — {}", w.schedule_tag.is_some());

        let window = Span::new(at.start - 3600, at.end + 3600);
        let copy = until("копия у участника", || async {
            c2.calendar_query(&cal2, "VEVENT", Some(window)).await.unwrap().into_iter().find(|r| r.data.contains(&uid))
        })
        .await;
        guard.0.resources.push((2, copy.href.clone()));
        let line = attendee_line(&copy.data, &peer).expect("участник в копии");
        assert!(line.contains("PARTSTAT=NEEDS-ACTION"), "{line}");
        println!("участник: Schedule-Tag в calendar-query — {}", copy.schedule_tag.is_some());
        let tag = match copy.schedule_tag.clone() {
            Some(t) => t,
            None => c2.get(&copy.href).await.unwrap().schedule_tag.expect("Schedule-Tag у копии участника"),
        };

        // Ответ — правка своей копии: PARTSTAT своей строки ATTENDEE.
        let accepted: String = unfold(&copy.data)
            .into_iter()
            .map(|l| if attendee_line(&l, &peer).is_some() { l.replace("PARTSTAT=NEEDS-ACTION", "PARTSTAT=ACCEPTED") } else { l })
            .filter(|l| !l.is_empty())
            .map(|l| l + "\r\n")
            .collect();
        let reply = c2.put(&copy.href, Content::Calendar, &accepted, Precondition::ScheduleTag(&tag)).await.unwrap();
        let after = c2.get(&copy.href).await.unwrap().schedule_tag;
        println!(
            "участник: Schedule-Tag в ответе PUT — {}, после ответа тот же — {}",
            reply.schedule_tag.is_some(),
            after.as_deref() == Some(tag.as_str())
        );

        let organizer_sees = until("ответ у организатора", || async {
            let own = c1.get(&href).await.unwrap();
            attendee_line(&own.data, &peer).filter(|l| l.contains("PARTSTAT=ACCEPTED"))
        })
        .await;
        assert!(organizer_sees.contains("PARTSTAT=ACCEPTED"));
        // Своя копия организатора после ответа: старый ETag не годится — правка только по свежему.
        let stale = c1.put(&href, Content::Calendar, &event(&uid, "x", at, &people), Precondition::Etag(w.etag.as_deref().unwrap_or("\"0\""))).await;
        assert!(matches!(stale, Err(Error::Precondition)), "{stale:?}");

        assert!(c1.delete(&href, Precondition::None).await.unwrap());
        let cancelled = until("отмена у участника", || async {
            match c2.get(&copy.href).await {
                Err(Error::NotFound) => Some("удалена".to_owned()),
                Ok(r) if r.data.contains("STATUS:CANCELLED") => Some("STATUS:CANCELLED".to_owned()),
                _ => None,
            }
        })
        .await;
        println!("у участника после отмены: {cancelled}");

        // Что сервер положил во входящие планирования: у участника — REQUEST и CANCEL, у организатора — REPLY.
        let methods = |items: Vec<postman_dav::Resource>| -> Vec<String> {
            let mut m: Vec<String> = items
                .iter()
                .filter(|r| r.data.contains(&uid))
                .filter_map(|r| unfold(&r.data).into_iter().find_map(|l| l.strip_prefix("METHOD:").map(str::to_owned)))
                .collect();
            m.sort();
            m
        };
        let inbox2 = d2.schedule_inbox.clone().expect("schedule-inbox-URL");
        let at_peer = until("CANCEL во входящих участника", || async {
            Some(methods(c2.inbox(&inbox2).await.unwrap())).filter(|m| m.iter().any(|x| x == "CANCEL"))
        })
        .await;
        assert!(at_peer.iter().any(|m| m == "REQUEST"), "{at_peer:?}");
        let at_organizer = methods(c1.inbox(d1.schedule_inbox.as_deref().expect("schedule-inbox-URL")).await.unwrap());
        println!("входящие планирования: участник {at_peer:?}, организатор {at_organizer:?}");

        // Копия, приглашения во входящих планирования и письма сервера — убрать.
        let _ = c2.delete(&copy.href, Precondition::None).await;
        guard.0.resources.clear();
        let mut inboxes = 0;
        for (n, inbox) in &guard.0.inboxes {
            inboxes += clean_inbox(&client(*n), inbox, &uid).await;
        }
        println!("входящие планирования: удалено {inboxes}");
        guard.0.inboxes.clear();
        clean_letters(&uid).await;
    });
}

/// Карточка: создать → sync-collection → addressbook-multiget → правка с If-Match (старый ETag — 412) → удалить
/// → sync-collection (удаление).
#[test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
fn carddav_round_trip() {
    rt().block_on(async {
        let c = client(1);
        let d = c.discover().await.unwrap();
        let book = c.address_books(&d.addressbook_home[0]).await.unwrap().into_iter().find(|b| b.writable).expect("адресная книга").href;
        let before = c.sync(&book, None).await.unwrap();
        let uid = format!("postman-dav-card-{}", stamp());
        let href = child_href(&book, &uid, Content::VCard);
        let mut guard = Cleanup(Todo { resources: vec![(1, href.clone())], ..Default::default() });
        let card = |name: &str| format!("BEGIN:VCARD\r\nVERSION:4.0\r\nUID:{uid}\r\nFN:{name}\r\nEMAIL:{uid}@example.test\r\nEND:VCARD\r\n");
        assert!(c.put(&href, Content::VCard, &card("Иван Проверкин"), Precondition::Create).await.unwrap().created);

        let ch = c.sync(&book, Some(&before.token)).await.unwrap();
        let item = ch.changed.iter().find(|i| same_href(&i.href, &href)).expect("новая карточка в sync-collection").clone();
        let got = c.addressbook_multiget(&book, std::slice::from_ref(&item.href)).await.unwrap();
        assert!(got.resources[0].data.contains("FN:Иван Проверкин"));

        c.put(&href, Content::VCard, &card("Иван Проверкин-Второй"), Precondition::Etag(&item.etag)).await.unwrap();
        let stale = c.put(&href, Content::VCard, &card("x"), Precondition::Etag(&item.etag)).await;
        assert!(matches!(stale, Err(Error::Precondition)), "{stale:?}");
        let ch2 = c.sync(&book, Some(&ch.token)).await.unwrap();
        let item2 = ch2.changed.iter().find(|i| same_href(&i.href, &href)).expect("правка в sync-collection");
        assert_ne!(item2.etag, item.etag);
        assert!(c.get(&href).await.unwrap().data.contains("Проверкин-Второй"));

        assert!(c.delete(&href, Precondition::Etag(&item2.etag)).await.unwrap());
        let gone = c.sync(&book, Some(&ch2.token)).await.unwrap();
        assert!(gone.removed.iter().any(|h| same_href(h, &href)), "{gone:?}");
        guard.0.resources.clear();
    });
}

/// Неверный пароль — ошибка входа, а не «нет CalDAV».
#[test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
fn dav_wrong_password() {
    rt().block_on(async {
        let c = Client::new(&Config { password: "not-the-password".into(), ..config(1) }).unwrap();
        let r = c.discover().await;
        assert!(matches!(r, Err(Error::Auth)), "{r:?}");
    });
}
