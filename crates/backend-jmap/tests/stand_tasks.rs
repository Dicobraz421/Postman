//! S6-01: где хранить задачи и как идёт поручение — против живого Stalwart v0.16.27 (`docs/STALWART.md` п. 37–42).
//!
//! Проверено: JMAP Tasks не объявлен, но задача — `CalendarEvent` с `"@type": "Task"` (JSCalendar 2.0,
//! draft-ietf-calext-jscalendarbis) в обычном календаре, и это тот же VTODO, что видит CalDAV; поля карточки задачи
//! из макетов (срок, начало, приоритет «!», оценка, прогресс, статус, повтор, напоминание, категории, заметки,
//! подзадача, ссылка на письмо, проект) сохраняются; свои поля — `X-POSTMAN-*` через `iCalendar.properties`.
//! Поручение — iTIP силами сервера (`sendSchedulingMessages`): копия у исполнителя и наблюдателя, ответы и прогресс
//! исполнителя доходят до поручившего. Запасной путь — FileNode (JSON). Напоминания: сервер шлёт письмо только по
//! `action: email` относительно начала, по сроку задачи — нет.
//!
//! Где сервер делает не так, как хотелось бы, тест проверяет текущее поведение и говорит об этом в комментарии
//! («сейчас:») — чтобы заметить, когда Stalwart это поменяет.
//!
//! Каждый тест убирает за собой (задачи и события по UID с префиксом `postman-s601-<тест>-`, письма с темой
//! «Postman s601 <тест>»), в начале — остатки упавшего прогона. Писем мало: входящие на адрес ограничены
//! (25 в час на пару «домен отправителя × получатель», п. 17), поручение — 10 писем на троих, своё письмо-поручение — 1, напоминания — 2.
//!
//! Запуск: `POSTMAN_STAND_URL`, `POSTMAN_STAND_USER1..3`, `POSTMAN_STAND_PASSWORD1..3` (и `POSTMAN_INSECURE_TLS=1`
//! напрямую), `cargo test -p postman-backend-jmap --test stand_tasks -- --ignored --nocapture --test-threads=1`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)

use std::future::Future;
use std::time::{Duration, Instant};

use postman_jmap::{Auth, CALENDARS, CORE, Client, Envelope, FILENODE, Invocation, MAIL, Written};
use serde_json::{Value, json};

/// Сколько ждать доставки iTIP: на стенде — доли секунды, запас на нагрузку.
const DELIVERY: Duration = Duration::from_secs(30);
const TASKS: &str = "urn:ietf:params:jmap:tasks";

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} не задан: стенд — docs/STALWART.md"))
}

struct User {
    email: String,
    jmap: Client,
    dav: Dav,
}

async fn user(n: u8) -> User {
    let email = env(&format!("POSTMAN_STAND_USER{n}"));
    let password = env(&format!("POSTMAN_STAND_PASSWORD{n}"));
    let auth = Auth::Basic { user: email.clone(), password: password.clone() };
    let jmap = Client::connect(&env("POSTMAN_STAND_URL"), auth).await.unwrap_or_else(|e| panic!("вход {email}: {e}"));
    let dav = Dav::new(&email, &password);
    User { email, jmap, dav }
}

fn stamp() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    format!("{}{:03}", t.as_secs(), t.subsec_millis())
}

/// Повторять `f`, пока не вернёт `Some`, не дольше `limit`; `None` — не дождались (иногда это и проверяется).
async fn eventually<T, F, Fut>(what: &str, limit: Duration, mut f: F) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let start = Instant::now();
    loop {
        if let Some(v) = f().await {
            println!("{what}: {:?}", start.elapsed());
            return Some(v);
        }
        if start.elapsed() > limit {
            println!("{what}: нет за {limit:?}");
            return None;
        }
        tokio::time::sleep(Duration::from_millis(700)).await;
    }
}

// ---------------------------------------------------------------------------------------------------------------
// CalDAV (RFC 4791, 6578): минимальный клиент для теста — PROPFIND, REPORT, PUT, GET, DELETE с Basic.

#[derive(Debug)]
struct Resp {
    status: u16,
    etag: Option<String>,
    body: String,
}

struct Dav {
    http: reqwest::Client,
    base: reqwest::Url,
    user: String,
    password: String,
}

impl Dav {
    fn new(user: &str, password: &str) -> Self {
        let mut b = reqwest::Client::builder();
        if postman_jmap::insecure_tls() {
            b = b.danger_accept_invalid_certs(true);
        }
        Self {
            http: b.build().unwrap(),
            base: reqwest::Url::parse(&env("POSTMAN_STAND_URL")).unwrap(),
            user: user.to_owned(),
            password: password.to_owned(),
        }
    }

    async fn send(&self, method: &str, path: &str, headers: &[(&str, &str)], body: Option<String>) -> Resp {
        let url = self.base.join(path).unwrap();
        let method = reqwest::Method::from_bytes(method.as_bytes()).unwrap();
        let mut rb = self.http.request(method.clone(), url).basic_auth(&self.user, Some(&self.password));
        for (k, v) in headers {
            rb = rb.header(*k, *v);
        }
        if let Some(b) = body {
            rb = rb.body(b);
        }
        let r = rb.send().await.unwrap_or_else(|e| panic!("{method} {path}: {e}"));
        let status = r.status().as_u16();
        let etag = r.headers().get("etag").and_then(|v| v.to_str().ok()).map(str::to_owned);
        Resp { status, etag, body: r.text().await.unwrap_or_default() }
    }

    async fn xml(&self, method: &str, path: &str, depth: &str, body: &str) -> Resp {
        let h = [("Depth", depth), ("Content-Type", "application/xml; charset=utf-8")];
        self.send(method, path, &h, Some(body.to_owned())).await
    }

    /// `PUT` .ics: `if_match` — заменить эту версию, иначе создать (`If-None-Match: *`).
    async fn put(&self, path: &str, ics: &str, if_match: Option<&str>) -> Resp {
        let cond = match if_match {
            Some(e) => ("If-Match", e),
            None => ("If-None-Match", "*"),
        };
        self.send("PUT", path, &[("Content-Type", "text/calendar; charset=utf-8"), cond], Some(ics.to_owned())).await
    }

    /// Календарь по умолчанию: `calendar-home-set` из `PROPFIND /dav/cal/`, в нём — коллекция-календарь.
    /// Возвращает адрес и ответ `PROPFIND` (с `supported-calendar-component-set`).
    async fn default_calendar(&self) -> (String, String) {
        let r = self.xml("PROPFIND", "/dav/cal/", "0", PROPFIND_HOME).await;
        assert_eq!(r.status, 207, "{}", r.body);
        let at = r.body.find("calendar-home-set>").expect("calendar-home-set");
        let home = between(&r.body[at..], "href>", "<").expect("calendar-home-set");
        let home = home.to_owned();
        let r = self.xml("PROPFIND", &home, "1", PROPFIND_CALENDARS).await;
        assert_eq!(r.status, 207, "{}", r.body);
        let cal = responses(&r.body)
            .into_iter()
            .find(|x| x.contains(":calendar/>") && x.contains("VTODO"))
            .or_else(|| responses(&r.body).into_iter().find(|x| x.contains(":calendar/>")))
            .expect("нет календаря");
        let href = between(cal, "href>", "<").unwrap().to_owned();
        (href, cal.to_owned())
    }

    /// `calendar-query` по компоненту и условию (`inner` внутри `comp-filter`): адреса и .ics.
    async fn query(&self, cal: &str, comp: &str, inner: &str) -> Vec<(String, String)> {
        let body = format!(
            r#"<?xml version="1.0"?><c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:prop><d:getetag/><c:calendar-data/></d:prop><c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="{comp}">{inner}</c:comp-filter></c:comp-filter></c:filter></c:calendar-query>"#
        );
        let r = self.xml("REPORT", cal, "1", &body).await;
        assert_eq!(r.status, 207, "REPORT {comp} {inner}: {}", r.body);
        responses(&r.body)
            .into_iter()
            .filter(|x| x.contains("200 OK"))
            .map(|x| {
                let href = between(x, "href>", "<").unwrap().to_owned();
                let ics = between(x, "<![CDATA[", "]]>").unwrap_or("").to_owned();
                (href, ics)
            })
            .collect()
    }

    /// VTODO или VEVENT по UID: адрес и .ics (без переносов строк, RFC 5545 §3.1).
    async fn by_uid(&self, cal: &str, comp: &str, uid: &str) -> Option<(String, String)> {
        let inner = format!(r#"<c:prop-filter name="UID"><c:text-match collation="i;octet">{uid}</c:text-match></c:prop-filter>"#);
        self.query(cal, comp, &inner).await.into_iter().next().map(|(h, ics)| (h, unfold(&ics)))
    }

    /// `sync-collection` (RFC 6578): изменённые адреса (`true` — есть, `false` — удалён) и новый токен.
    async fn sync(&self, cal: &str, token: &str) -> (Vec<(String, bool)>, String) {
        let body = format!(
            r#"<?xml version="1.0"?><d:sync-collection xmlns:d="DAV:"><d:sync-token>{token}</d:sync-token><d:sync-level>1</d:sync-level><d:prop><d:getetag/></d:prop></d:sync-collection>"#
        );
        let r = self.xml("REPORT", cal, "1", &body).await;
        assert_eq!(r.status, 207, "sync-collection: {}", r.body);
        let items = responses(&r.body).into_iter().map(|x| (between(x, "href>", "<").unwrap().to_owned(), !x.contains("404 Not Found"))).collect();
        let token = between(&r.body, "sync-token>", "<").expect("sync-token").to_owned();
        (items, token)
    }
}

const PROPFIND_HOME: &str = r#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:prop><d:current-user-principal/><c:calendar-home-set/></d:prop></d:propfind>"#;
const PROPFIND_CALENDARS: &str = r#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:prop><d:resourcetype/><d:displayname/><c:supported-calendar-component-set/><d:sync-token/></d:prop></d:propfind>"#;

/// Текст между `start` и следующим `end`.
fn between<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let i = s.find(start)? + start.len();
    let j = s[i..].find(end)? + i;
    Some(&s[i..j])
}

/// Части `<…:response>` ответа multistatus (префикс пространства имён у Stalwart — `D:`).
fn responses(xml: &str) -> Vec<&str> {
    xml.split(":response>").filter(|p| p.contains("href>")).collect()
}

/// Склеить перенесённые строки .ics.
fn unfold(ics: &str) -> String {
    ics.replace("\r\n ", "").replace("\n ", "").replace("\r\n", "\n")
}

fn vtodo(uid: &str, body: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Postman//S6-01//RU\r\nBEGIN:VTODO\r\nUID:{uid}\r\nDTSTAMP:20261021T090000Z\r\n{body}END:VTODO\r\nEND:VCALENDAR\r\n"
    )
}

// ---------------------------------------------------------------------------------------------------------------
// JMAP

fn cal_account(c: &Client) -> String {
    c.account_for(CALENDARS).unwrap().to_owned()
}

/// Вызовы одним запросом; ответы по порядку (ошибка метода — паника с текстом).
async fn call(c: &Client, using: &[&str], calls: Vec<(&str, Value)>) -> Vec<Value> {
    let inv: Vec<Invocation> = calls.into_iter().enumerate().map(|(i, (m, a))| Invocation(m.into(), a, format!("c{i}"))).collect();
    c.call(using, &inv).await.unwrap_or_else(|e| panic!("{e}")).into_iter().map(|r| r.1).collect()
}

/// Ошибка метода (`unsupportedFilter`, `unsupportedSort` …) или `None`, если вызов прошёл.
async fn method_error(c: &Client, method: &str, args: Value) -> Option<String> {
    match c.call(&[CORE, CALENDARS], &[Invocation(method.into(), args, "x".into())]).await {
        Ok(_) => None,
        Err(postman_jmap::Error::Method { kind, .. }) => Some(kind),
        Err(e) => panic!("{method}: {e}"),
    }
}

async fn default_calendar(c: &Client) -> String {
    let acc = cal_account(c);
    let r = call(c, &[CORE, CALENDARS], vec![("Calendar/get", json!({ "accountId": acc, "ids": null }))]).await;
    let list = r[0]["list"].as_array().unwrap();
    let cal = list.iter().find(|c| c["isDefault"] == json!(true)).or(list.first()).expect("нет календаря");
    cal["id"].as_str().unwrap().to_owned()
}

/// `CalendarEvent/set create` одной записи: id или `notCreated`.
async fn create(c: &Client, item: Value, send: bool) -> Result<String, Value> {
    let acc = cal_account(c);
    let args = json!({ "accountId": acc, "create": { "n": item }, "sendSchedulingMessages": send });
    let r = call(c, &[CORE, CALENDARS], vec![("CalendarEvent/set", args)]).await;
    match r[0]["created"]["n"]["id"].as_str() {
        Some(id) => Ok(id.to_owned()),
        None => Err(r[0]["notCreated"]["n"].clone()),
    }
}

async fn update(c: &Client, id: &str, patch: Value, send: bool) {
    let acc = cal_account(c);
    let args = json!({ "accountId": acc, "update": { id: patch }, "sendSchedulingMessages": send });
    let r = call(c, &[CORE, CALENDARS], vec![("CalendarEvent/set", args)]).await;
    assert!(r[0]["updated"].get(id).is_some(), "не обновлено: {}", r[0]);
}

async fn destroy(c: &Client, ids: &[String], send: bool) {
    let acc = cal_account(c);
    let args = json!({ "accountId": acc, "destroy": ids, "sendSchedulingMessages": send });
    let r = call(c, &[CORE, CALENDARS], vec![("CalendarEvent/set", args)]).await;
    assert!(r[0]["notDestroyed"].as_object().is_none_or(|m| m.is_empty()), "{}", r[0]);
}

async fn get(c: &Client, id: &str, props: Option<&[&str]>) -> Value {
    let acc = cal_account(c);
    let mut args = json!({ "accountId": acc, "ids": [id] });
    if let Some(p) = props {
        args["properties"] = json!(p);
    }
    let r = call(c, &[CORE, CALENDARS], vec![("CalendarEvent/get", args)]).await;
    r[0]["list"][0].clone()
}

/// Запись по UID (фильтр `uid` у Stalwart — точное совпадение) с нужными свойствами.
async fn by_uid(c: &Client, uid: &str, props: &[&str]) -> Option<Value> {
    let acc = cal_account(c);
    let r = call(
        c,
        &[CORE, CALENDARS],
        vec![
            ("CalendarEvent/query", json!({ "accountId": acc, "filter": { "uid": uid } })),
            (
                "CalendarEvent/get",
                json!({ "accountId": acc, "#ids": { "resultOf": "c0", "name": "CalendarEvent/query", "path": "/ids" }, "properties": props }),
            ),
        ],
    )
    .await;
    r[1]["list"].as_array().and_then(|l| l.first()).cloned()
}

async fn query_ids(c: &Client, filter: Value, extra: Value) -> Result<Vec<String>, String> {
    let acc = cal_account(c);
    let mut args = json!({ "accountId": acc, "filter": filter });
    if let (Some(a), Some(x)) = (args.as_object_mut(), extra.as_object()) {
        a.extend(x.clone());
    }
    if let Some(kind) = method_error(c, "CalendarEvent/query", args.clone()).await {
        return Err(kind);
    }
    let r = call(c, &[CORE, CALENDARS], vec![("CalendarEvent/query", args)]).await;
    Ok(r[0]["ids"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_owned()).collect())
}

async fn cal_state(c: &Client) -> String {
    let acc = cal_account(c);
    let r = call(c, &[CORE, CALENDARS], vec![("CalendarEvent/get", json!({ "accountId": acc, "ids": [] }))]).await;
    r[0]["state"].as_str().unwrap().to_owned()
}

async fn cal_changes(c: &Client, since: &str) -> Value {
    let acc = cal_account(c);
    let r = call(c, &[CORE, CALENDARS], vec![("CalendarEvent/changes", json!({ "accountId": acc, "sinceState": since }))]).await;
    r[0].clone()
}

fn ids_of(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect()).unwrap_or_default()
}

/// Письма с `needle` в теме (поиск сервера по слову, затем точная сверка), во всех папках: id и тема.
async fn mails(c: &Client, word: &str, needle: &str) -> Vec<(String, String)> {
    let acc = c.mail_account().unwrap().to_owned();
    let r = call(
        c,
        &[CORE, MAIL],
        vec![
            ("Email/query", json!({ "accountId": acc, "filter": { "subject": word }, "limit": 100 })),
            (
                "Email/get",
                json!({ "accountId": acc, "#ids": { "resultOf": "c0", "name": "Email/query", "path": "/ids" }, "properties": ["subject"] }),
            ),
        ],
    )
    .await;
    r[1]["list"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| {
            let s = e["subject"].as_str().unwrap_or("");
            s.contains(needle).then(|| (e["id"].as_str().unwrap().to_owned(), s.to_owned()))
        })
        .collect()
}

/// Подождать письмо с `needle` в теме.
async fn mail_arrives(u: &User, word: &str, needle: &str, limit: Duration) -> bool {
    eventually(&format!("письмо «{needle}» у {}", u.email), limit, || async {
        (!mails(&u.jmap, word, needle).await.is_empty()).then_some(())
    })
    .await
    .is_some()
}

/// Убрать за тестом: задачи и события с UID `prefix…`, письма с `subject` в теме. Записи — JMAP без рассылки:
/// `DELETE` копии участника по CalDAV сервер считает отказом и шлёт организатору «Declined: …» (п. 40); что
/// осталось — CalDAV с `Schedule-Reply: F` (RFC 6638 §8.1).
async fn sweep(u: &User, prefix: &str, subject: &str) {
    let acc = cal_account(&u.jmap);
    let r = call(
        &u.jmap,
        &[CORE, CALENDARS],
        vec![
            ("CalendarEvent/query", json!({ "accountId": acc, "filter": { "text": subject } })),
            (
                "CalendarEvent/get",
                json!({ "accountId": acc, "#ids": { "resultOf": "c0", "name": "CalendarEvent/query", "path": "/ids" }, "properties": ["uid"] }),
            ),
        ],
    )
    .await;
    let ids: Vec<String> = r[1]["list"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["uid"].as_str().is_some_and(|x| x.starts_with(prefix)))
        .map(|e| e["id"].as_str().unwrap().to_owned())
        .collect();
    if !ids.is_empty() {
        destroy(&u.jmap, &ids, false).await;
    }
    let (cal, _) = u.dav.default_calendar().await;
    let inner = format!(r#"<c:prop-filter name="UID"><c:text-match collation="i;octet">{prefix}</c:text-match></c:prop-filter>"#);
    for comp in ["VTODO", "VEVENT"] {
        for (href, _) in u.dav.query(&cal, comp, &inner).await {
            let r = u.dav.send("DELETE", &href, &[("Schedule-Reply", "F")], None).await;
            assert!(matches!(r.status, 200 | 204 | 404), "DELETE {href}: {}", r.status);
        }
    }
    // Письма: два прохода — только что пришедшие (ответы iTIP) поиск сервера находит не сразу.
    let word = subject.split_whitespace().nth(1).unwrap_or(subject);
    for pass in 0..2 {
        if pass > 0 {
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
        for (id, _) in mails(&u.jmap, word, subject).await {
            u.jmap.destroy_email(&id).await.unwrap();
        }
    }
}

/// Отправить iTIP письмом (iMIP, RFC 6047): `text/plain` + `text/calendar; method=…` — как отправит Postman
/// (S6-09), если поручение идёт не силами сервера.
async fn send_imip(from: &User, to: &str, subject: &str, method: &str, ics: &str) {
    let raw = format!(
        "From: {from}\r\nTo: {to}\r\nSubject: {subject}\r\nMessage-ID: <s601-{}@itt.test>\r\nMIME-Version: 1.0\r\n\
         Content-Type: multipart/alternative; boundary=\"s601\"\r\n\r\n--s601\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
         {subject}\r\n--s601\r\nContent-Type: text/calendar; method={method}; charset=utf-8\r\n\r\n{ics}\r\n--s601--\r\n",
        stamp(),
        from = from.email,
    );
    let c = &from.jmap;
    let identity = c.identities().await.unwrap().into_iter().find(|i| i.email == from.email).expect("Identity").id;
    let (boxes, _) = c.mailboxes().await.unwrap();
    let role = |r: &str| boxes.iter().find(|m| m.role.as_deref() == Some(r)).map(|m| m.id.clone()).unwrap();
    let env = Envelope { mail_from: from.email.clone(), rcpt_to: vec![to.to_owned()] };
    c.send(raw.into_bytes(), &identity, &env, &role("drafts"), &role("sent")).await.unwrap();
}

fn imip(method: &str, body: &str) -> String {
    format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Postman//S6-01//RU\r\nMETHOD:{method}\r\nBEGIN:VTODO\r\n{body}END:VTODO\r\nEND:VCALENDAR")
}

// ---------------------------------------------------------------------------------------------------------------

/// 1. Где задачи могут жить: JMAP Tasks не объявлен; признака компонентов у JMAP `Calendar` нет; календарь по
/// умолчанию в CalDAV принимает VTODO (и VEVENT) — задачи и события в одном календаре.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn tasks_capability_and_vtodo_in_calendar() {
    let u = user(1).await;
    // Сейчас: `urn:ietf:params:jmap:tasks` (draft-ietf-jmap-tasks) не объявлен ни в сессии, ни у аккаунта.
    assert!(!u.jmap.has(TASKS), "JMAP Tasks появился — первый путь хранения, пересмотреть S6-02");
    let acc = cal_account(&u.jmap);
    assert!(!u.jmap.session().accounts[&acc].account_capabilities.contains_key(TASKS));

    let r = call(&u.jmap, &[CORE, CALENDARS], vec![("Calendar/get", json!({ "accountId": acc, "ids": null }))]).await;
    let cal = r[0]["list"].as_array().unwrap().iter().find(|c| c["isDefault"] == json!(true)).unwrap().clone();
    let mut keys: Vec<&String> = cal.as_object().unwrap().keys().collect();
    keys.sort();
    println!("Calendar properties: {keys:?}");
    // Сейчас: у JMAP Calendar нет свойства «какие компоненты» — задачи кладутся в любой календарь.
    assert!(keys.iter().all(|k| !k.to_lowercase().contains("component")), "{keys:?}");

    let (href, prop) = u.dav.default_calendar().await;
    println!("CalDAV calendar {href}");
    let comps = between(&prop, "supported-calendar-component-set>", "supported-calendar-component-set").unwrap_or("");
    assert!(comps.contains("\"VTODO\"") && comps.contains("\"VEVENT\""), "{prop}");
}

/// Задача со всеми полями карточки (макеты «Задачи», «Поручил я», «Поручено мне»).
fn full_task(uid: &str, cal: &str) -> Value {
    json!({
        "@type": "Task",
        "uid": uid,
        "calendarIds": { cal: true },
        "title": "Postman s601 jmap — отправить редакцию договора",
        "description": "Заметки **Markdown**\n- пункт",
        "start": "2027-03-01T09:00:00",
        "due": "2027-03-02T16:00:00",
        "timeZone": "Europe/Moscow",
        "estimatedDuration": "PT1H30M",
        "priority": 1,
        "percentComplete": 20,
        "progress": "in-process",
        "keywords": { "pm.cat.clients": true },
        "categories": { "https://postman.invalid/cat/clients": true },
        "recurrenceRule": { "frequency": "monthly", "byMonthDay": [20] },
        "alerts": { "a1": { "@type": "Alert", "trigger": { "@type": "OffsetTrigger", "offset": "-PT30M", "relativeTo": "end" }, "action": "display" } },
        "relatedTo": { "postman-s601-jmap-parent@itt.test": { "@type": "Relation", "relation": { "parent": true } } },
        "links": { "mail": { "@type": "Link", "href": "mid:s601-letter@itt.test", "rel": "describedby" } },
        "iCalendar": { "name": "vtodo", "properties": [
            ["x-postman-project", {}, "unknown", "vec"],
            ["x-postman-message-id", {}, "unknown", "<s601-letter@itt.test>"],
        ] },
    })
}

/// 2. JMAP: `CalendarEvent` с `@type: Task` — создать, прочитать (поля как есть), это VTODO в CalDAV, правка,
/// изменения у второго клиента (`CalendarEvent/changes`), удаление.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn jmap_task_fields_round_trip() {
    let u = user(1).await;
    let other = user(1).await; // второй клиент того же пользователя
    let prefix = "postman-s601-jmap-";
    sweep(&u, prefix, "Postman s601 jmap").await;
    let cal = default_calendar(&u.jmap).await;
    let uid = format!("{prefix}{}@itt.test", stamp());
    let since = cal_state(&other.jmap).await;

    // Сейчас (JSCalendar 2.0): `recurrenceRules` (RFC 8984, массив) — `invalidProperties`, повтор — `recurrenceRule`;
    // `progressUpdated` (RFC 8984 §5.2.5) — тоже; свойство с префиксом поставщика (`example.com:project`) — тоже.
    for (prop, value) in [
        ("recurrenceRules", json!([{ "@type": "RecurrenceRule", "frequency": "monthly" }])),
        ("progressUpdated", json!("2026-10-21T10:00:00Z")),
        ("example.com:project", json!("vec")),
    ] {
        let mut t = json!({ "@type": "Task", "uid": format!("{prefix}bad-{}@itt.test", stamp()), "calendarIds": { &cal: true },
            "title": "Postman s601 jmap bad", "due": "2027-03-02T16:00:00", "timeZone": "Europe/Moscow" });
        t[prop] = value;
        let err = create(&u.jmap, t, false).await.expect_err(prop);
        assert_eq!(err["type"], "invalidProperties", "{prop}: {err}");
        assert_eq!(err["properties"], json!([prop]), "{prop}: {err}");
    }

    let sent = full_task(&uid, &cal);
    let id = create(&u.jmap, sent.clone(), false).await.unwrap_or_else(|e| panic!("Task не создан: {e}"));
    let props: Vec<&str> = sent.as_object().unwrap().keys().map(String::as_str).chain(["id", "iCalendar"]).collect();
    let got = get(&u.jmap, &id, Some(&props)).await;
    println!("JMAP Task: {got}");
    for k in [
        "@type",
        "uid",
        "title",
        "description",
        "start",
        "due",
        "timeZone",
        "estimatedDuration",
        "priority",
        "percentComplete",
        "progress",
        "keywords",
        "categories",
        "recurrenceRule",
        "alerts",
        "links",
    ] {
        assert_eq!(got[k], sent[k], "{k}");
    }
    // `@type` у связи сервер не возвращает (значение по умолчанию), сама связь — как есть.
    assert_eq!(got["relatedTo"]["postman-s601-jmap-parent@itt.test"]["relation"], json!({ "parent": true }));
    let x = got["iCalendar"]["properties"].as_array().unwrap();
    assert!(x.contains(&json!(["x-postman-project", {}, "unknown", "vec"])), "{x:?}");
    assert!(x.contains(&json!(["x-postman-message-id", {}, "unknown", "<s601-letter@itt.test>"])), "{x:?}");

    // Та же запись в CalDAV — VTODO; свои поля — X-POSTMAN-*.
    let (dav_cal, _) = u.dav.default_calendar().await;
    let (_, ics) = u.dav.by_uid(&dav_cal, "VTODO", &uid).await.expect("JMAP Task не виден в CalDAV как VTODO");
    println!("CalDAV VTODO:\n{ics}");
    for line in [
        "DUE;TZID=Europe/Moscow:20270302T160000",
        "DTSTART;TZID=Europe/Moscow:20270301T090000",
        "PRIORITY:1",
        "PERCENT-COMPLETE:20",
        "STATUS:IN-PROCESS",
        "RRULE:FREQ=MONTHLY;BYMONTHDAY=20",
        "BEGIN:VALARM",
        "RELATED-TO;RELTYPE=PARENT:postman-s601-jmap-parent@itt.test",
        "X-POSTMAN-PROJECT:vec",
        "X-POSTMAN-MESSAGE-ID:<s601-letter@itt.test>",
    ] {
        assert!(ics.contains(line), "нет «{line}» в\n{ics}");
    }

    // Второй клиент видит создание.
    let ch = cal_changes(&other.jmap, &since).await;
    assert!(ids_of(&ch["created"]).contains(&id), "{ch}");
    let since = ch["newState"].as_str().unwrap().to_owned();

    // Выполнение: прогресс, процент, свои поля — патчем.
    update(
        &u.jmap,
        &id,
        json!({ "progress": "completed", "percentComplete": 100,
                "iCalendar": { "name": "vtodo", "properties": [["x-postman-project", {}, "unknown", "soc"]] } }),
        false,
    )
    .await;
    let got = get(&u.jmap, &id, Some(&["progress", "percentComplete", "iCalendar"])).await;
    assert_eq!(got["progress"], "completed");
    assert_eq!(got["percentComplete"], 100);
    // Сейчас: `iCalendar` в патче заменяется целиком — второе X-свойство ушло.
    assert_eq!(got["iCalendar"]["properties"], json!([["x-postman-project", {}, "unknown", "soc"]]), "{got}");
    let (_, ics) = u.dav.by_uid(&dav_cal, "VTODO", &uid).await.unwrap();
    assert!(ics.contains("STATUS:COMPLETED"), "{ics}");
    // Сейчас: дату выполнения (COMPLETED) сервер не ставит — ставит клиент.
    assert!(!ics.contains("\nCOMPLETED:"), "{ics}");
    let ch = cal_changes(&other.jmap, &since).await;
    assert!(ids_of(&ch["updated"]).contains(&id), "{ch}");
    let since = ch["newState"].as_str().unwrap().to_owned();

    destroy(&u.jmap, std::slice::from_ref(&id), false).await;
    let ch = cal_changes(&other.jmap, &since).await;
    assert!(ids_of(&ch["destroyed"]).contains(&id), "{ch}");
    assert!(u.dav.by_uid(&dav_cal, "VTODO", &uid).await.is_none());
    sweep(&u, prefix, "Postman s601 jmap").await;
}

/// 3. Отбор на сервере: по сроку — да (`after`/`before` в JMAP, `time-range` в CalDAV); по статусу — только CalDAV
/// (`prop-filter STATUS`), в JMAP условия нет; сортировки по сроку в JMAP нет.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn task_query_by_due_and_status() {
    let u = user(1).await;
    let prefix = "postman-s601-query-";
    sweep(&u, prefix, "Postman s601 query").await;
    let cal = default_calendar(&u.jmap).await;
    let run = stamp();
    let mut ids = Vec::new();
    for (i, due, progress) in
        [(1, "2027-04-05T10:00:00", "needs-action"), (2, "2027-04-08T10:00:00", "completed"), (3, "2027-04-12T10:00:00", "in-process")]
    {
        let t = json!({ "@type": "Task", "uid": format!("{prefix}{i}-{run}@itt.test"), "calendarIds": { &cal: true },
            "title": format!("Postman s601 query {i} {run}"), "due": due, "timeZone": "Europe/Moscow", "progress": progress });
        ids.push(create(&u.jmap, t, false).await.unwrap());
    }
    let (a, b, c) = (&ids[0], &ids[1], &ids[2]);

    // Задача без начала: `utcStart` — срок, `utcEnd` — конец того же дня по поясу задачи.
    let t = get(&u.jmap, a, Some(&["utcStart", "utcEnd"])).await;
    println!("utcStart/utcEnd задачи без start: {t}");
    assert_eq!(t["utcStart"], "2027-04-05T07:00:00Z");

    let found = query_ids(&u.jmap, json!({ "after": "2027-04-04T00:00:00Z", "before": "2027-04-09T00:00:00Z" }), json!({})).await.unwrap();
    assert!(found.contains(a) && found.contains(b) && !found.contains(c), "{found:?}");
    let found = query_ids(&u.jmap, json!({ "after": "2027-04-07T00:00:00Z", "before": "2027-04-13T00:00:00Z" }), json!({})).await.unwrap();
    assert!(!found.contains(a) && found.contains(b) && found.contains(c), "{found:?}");
    let found = query_ids(&u.jmap, json!({ "title": format!("query 2 {run}") }), json!({})).await.unwrap();
    assert_eq!(found, vec![b.clone()]);
    // Сейчас: условий по статусу и типу записи нет, сортировки по сроку — тоже.
    for f in [json!({ "progress": "completed" }), json!({ "status": "completed" }), json!({ "type": "Task" })] {
        assert_eq!(query_ids(&u.jmap, f.clone(), json!({})).await, Err("unsupportedFilter".into()), "{f}");
    }
    let range = json!({ "after": "2027-04-04T00:00:00Z", "before": "2027-04-13T00:00:00Z" });
    assert_eq!(query_ids(&u.jmap, range.clone(), json!({ "sort": [{ "property": "due" }] })).await, Err("unsupportedSort".into()));
    let sorted = query_ids(&u.jmap, range, json!({ "sort": [{ "property": "start" }] })).await.unwrap();
    let pos = |id: &String| sorted.iter().position(|x| x == id).unwrap();
    assert!(pos(a) < pos(b) && pos(b) < pos(c), "sort start: {sorted:?}");

    // CalDAV: и срок, и статус — на сервере.
    let (dav_cal, _) = u.dav.default_calendar().await;
    let uids = |items: Vec<(String, String)>| -> Vec<String> {
        items
            .iter()
            .filter_map(|(_, ics)| between(ics, "UID:", "\n").map(|s| s.trim().to_owned()))
            .filter(|s| s.ends_with(&format!("{run}@itt.test")))
            .collect()
    };
    let uid = |i: u8| format!("{prefix}{i}-{run}@itt.test");
    let r = uids(u.dav.query(&dav_cal, "VTODO", r#"<c:time-range start="20270404T000000Z" end="20270409T000000Z"/>"#).await);
    assert!(r.contains(&uid(1)) && r.contains(&uid(2)) && !r.contains(&uid(3)), "{r:?}");
    let r = uids(u.dav.query(&dav_cal, "VTODO", r#"<c:prop-filter name="STATUS"><c:text-match>COMPLETED</c:text-match></c:prop-filter>"#).await);
    assert_eq!(r, vec![uid(2)]);
    let r = uids(
        u.dav
            .query(&dav_cal, "VTODO", r#"<c:prop-filter name="STATUS"><c:text-match negate-condition="yes">COMPLETED</c:text-match></c:prop-filter>"#)
            .await,
    );
    println!("STATUS не COMPLETED: {r:?}");
    assert!(r.contains(&uid(1)) && r.contains(&uid(3)) && !r.contains(&uid(2)), "{r:?}");

    destroy(&u.jmap, &ids, false).await;
    sweep(&u, prefix, "Postman s601 query").await;
}

/// 4. CalDAV: VTODO с полями карточки — `PUT` → `GET` как есть → вид в JMAP → правка с `If-Match` (старый ETag —
/// 412) → `sync-collection` видит создание, правку и удаление.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn caldav_vtodo_round_trip_and_sync() {
    let u = user(1).await;
    let prefix = "postman-s601-dav-";
    sweep(&u, prefix, "Postman s601 dav").await;
    let (cal, prop) = u.dav.default_calendar().await;
    let token = between(&prop, "sync-token>", "<").expect("sync-token у календаря").to_owned();
    let run = stamp();
    let uid = format!("{prefix}{run}@itt.test");
    let path = format!("{cal}s601-{run}.ics");
    let body = "SUMMARY:Postman s601 dav — ресурсный план\r\nDESCRIPTION:Заметки **Markdown**\\nвторая строка\r\n\
        DTSTART:20270301T060000Z\r\nDUE:20270302T130000Z\r\nPRIORITY:1\r\nPERCENT-COMPLETE:20\r\nSTATUS:IN-PROCESS\r\n\
        CATEGORIES:Клиенты,Команда\r\nRRULE:FREQ=MONTHLY;BYMONTHDAY=20\r\nRELATED-TO;RELTYPE=PARENT:postman-s601-dav-parent@itt.test\r\n\
        X-POSTMAN-PROJECT:vec\r\nX-POSTMAN-MESSAGE-ID:<s601-letter@itt.test>\r\nESTIMATED-DURATION:PT1H30M\r\n\
        ATTACH:mid:s601-letter@itt.test\r\nCOMMENT:Комментарий\r\n\
        BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER;RELATED=END:-PT30M\r\nDESCRIPTION:Напоминание\r\nEND:VALARM\r\n";
    let ics = vtodo(&uid, body);
    let r = u.dav.put(&path, &ics, None).await;
    assert_eq!(r.status, 201, "{}", r.body);
    let etag1 = r.etag.expect("ETag после PUT");

    // GET — ровно то, что положили.
    let r = u.dav.send("GET", &path, &[], None).await;
    assert_eq!(r.status, 200);
    assert_eq!(unfold(&r.body).trim_end(), unfold(&ics).trim_end());

    // Вид в JMAP: CATEGORIES → keywords, ATTACH → links (`rel: enclosure`), X-, COMMENT — в `iCalendar`.
    let j = by_uid(
        &u.jmap,
        &uid,
        &[
            "@type",
            "keywords",
            "links",
            "estimatedDuration",
            "alerts",
            "relatedTo",
            "recurrenceRule",
            "progress",
            "percentComplete",
            "priority",
            "due",
            "start",
            "timeZone",
            "iCalendar",
        ],
    )
    .await
    .expect("VTODO не виден в JMAP");
    println!("JMAP view of VTODO: {j}");
    assert_eq!(j["@type"], "Task");
    assert_eq!(j["keywords"], json!({ "Клиенты": true, "Команда": true }));
    assert_eq!(j["timeZone"], "Etc/UTC");
    assert_eq!(j["due"], "2027-03-02T13:00:00");
    assert_eq!(j["estimatedDuration"], "PT1H30M");
    assert_eq!(j["progress"], "in-process");
    assert_eq!(j["percentComplete"], 20);
    assert_eq!(j["recurrenceRule"], json!({ "frequency": "monthly", "byMonthDay": [20] }));
    let link = j["links"].as_object().unwrap().values().next().unwrap();
    assert_eq!((link["href"].as_str(), link["rel"].as_str()), (Some("mid:s601-letter@itt.test"), Some("enclosure")));
    let x = j["iCalendar"]["properties"].as_array().unwrap();
    for p in [json!(["x-postman-project", {}, "unknown", "vec"]), json!(["comment", {}, "unknown", "Комментарий"])] {
        assert!(x.contains(&p), "{p} нет в {x:?}");
    }

    // Выполнение с If-Match; повтор со старым ETag — 412.
    let done = vtodo(
        &uid,
        &body.replace("PERCENT-COMPLETE:20\r\nSTATUS:IN-PROCESS", "PERCENT-COMPLETE:100\r\nSTATUS:COMPLETED\r\nCOMPLETED:20261021T100000Z"),
    );
    let r = u.dav.put(&path, &done, Some(&etag1)).await;
    assert!(matches!(r.status, 200 | 204), "{} {}", r.status, r.body);
    let r = u.dav.put(&path, &ics, Some(&etag1)).await;
    assert_eq!(r.status, 412, "старый ETag: {}", r.body);
    let j = by_uid(&u.jmap, &uid, &["progress", "percentComplete", "iCalendar"]).await.unwrap();
    assert_eq!((j["progress"].as_str(), j["percentComplete"].as_u64()), (Some("completed"), Some(100)));
    // Сейчас: COMPLETED (дата выполнения) в JMAP — не `progressUpdated`, а сырое свойство в `iCalendar`.
    assert!(j["iCalendar"]["properties"].as_array().unwrap().contains(&json!(["completed", {}, "unknown", "20261021T100000Z"])), "{j}");

    let (items, token2) = u.dav.sync(&cal, &token).await;
    assert!(items.iter().any(|(h, live)| h == &path && *live), "{items:?}");
    let r = u.dav.send("DELETE", &path, &[], None).await;
    assert!(matches!(r.status, 200 | 204));
    let (items, _) = u.dav.sync(&cal, &token2).await;
    assert_eq!(items, vec![(path.clone(), false)], "удаление в sync-collection");
    sweep(&u, prefix, "Postman s601 dav").await;
}

/// 5. Запасной путь: список задач JSON-файлом в FileNode — запись, чтение второй сессией, замена с `ifInState`,
/// `FileNode/changes` у второго клиента, удаление.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn filenode_task_list_round_trip() {
    let u = user(1).await;
    let other = user(1).await;
    let acc = u.jmap.mail_account().unwrap().to_owned();
    assert!(u.jmap.has_files(&acc));
    let name = format!("postman-s601-tasks-{}.json", stamp());
    let (folder, _) = u.jmap.create_folder(&acc, None, &format!("postman-s601-{}", stamp())).await.unwrap();
    let (_, state0) = other.jmap.file_node(&acc, Some(&folder), &name).await.unwrap();

    let list = json!({ "tasks": [{ "uid": "t1", "title": "Ресурсный план", "due": "2027-03-02T16:00:00+03:00", "progress": "in-process" }] });
    let blob = u.jmap.upload(&acc, serde_json::to_vec(&list).unwrap(), "application/json").await.unwrap();
    let Written::Done { id, state } = u.jmap.write_file(&acc, &folder, None, &name, &blob, "application/json", None).await.unwrap() else {
        panic!("файл не создан");
    };
    let (node, _) = other.jmap.file_node(&acc, Some(&folder), &name).await.unwrap();
    let node = node.expect("второй клиент видит файл");
    let read = other.jmap.download(&acc, node.blob_id.as_deref().unwrap(), &name, "application/json").await.unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&read).unwrap(), list);

    let list2 = json!({ "tasks": [{ "uid": "t1", "title": "Ресурсный план", "progress": "completed" }] });
    let blob2 = u.jmap.upload(&acc, serde_json::to_vec(&list2).unwrap(), "application/json").await.unwrap();
    assert!(matches!(
        u.jmap.write_file(&acc, &folder, Some(&id), &name, &blob2, "application/json", Some(&state)).await.unwrap(),
        Written::Done { .. }
    ));
    // Тот же прежний state — уже `stateMismatch`: правку другого клиента не затрём.
    assert_eq!(u.jmap.write_file(&acc, &folder, Some(&id), &name, &blob, "application/json", Some(&state)).await.unwrap(), Written::Stale);

    let r = call(&other.jmap, &[CORE, FILENODE], vec![("FileNode/changes", json!({ "accountId": acc, "sinceState": state0 }))]).await;
    println!("FileNode/changes: {}", r[0]);
    let touched: Vec<String> = ids_of(&r[0]["created"]).into_iter().chain(ids_of(&r[0]["updated"])).collect();
    assert!(touched.contains(&id), "{}", r[0]);

    u.jmap.destroy_file_nodes(&acc, &[folder]).await.unwrap();
    let (node, _) = other.jmap.file_node(&acc, None, &name).await.unwrap();
    assert!(node.is_none());
}

/// 6. Поручение силами сервера (iTIP по VTODO): user1 поручает user2, user3 — наблюдатель (`informational` →
/// `ROLE=NON-PARTICIPANT`). Копия у исполнителя и наблюдателя → принять → в работе 50 % → готово → у поручившего;
/// поручивший меняет срок → у исполнителя `SEQUENCE` 1; «предложить срок» (COUNTER) и «отклонить с комментарием»
/// письмом (iMIP) → что делает сервер; «забрать» (удаление с рассылкой) → отмена.
/// Писем: user2 — 2, user3 — 3, user1 — 5.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn delegation_round_trip() {
    let (a, b, w) = (user(1).await, user(2).await, user(3).await);
    let prefix = "postman-s601-deleg-";
    let subject = "Postman s601 deleg";
    for x in [&a, &b, &w] {
        sweep(x, prefix, subject).await;
    }
    let run = stamp();
    let uid = format!("{prefix}{run}@itt.test");
    let title = format!("{subject} {run}");
    let cal = default_calendar(&a.jmap).await;
    let task = json!({
        "@type": "Task", "uid": uid, "calendarIds": { cal: true }, "title": title,
        "description": "Поручение: проверить отчёт", "due": "2027-03-04T18:00:00", "timeZone": "Europe/Moscow",
        "progress": "needs-action",
        "organizerCalendarAddress": format!("mailto:{}", a.email),
        "participants": {
            "o": { "calendarAddress": format!("mailto:{}", a.email), "name": "User 1", "roles": { "owner": true } },
            "p": { "calendarAddress": format!("mailto:{}", b.email), "name": "User 2", "roles": { "attendee": true },
                   "participationStatus": "needs-action", "expectReply": true },
            "w": { "calendarAddress": format!("mailto:{}", w.email), "name": "User 3", "roles": { "informational": true } },
        },
        "iCalendar": { "name": "vtodo", "properties": [["x-postman-project", {}, "unknown", "vec"]] },
    });
    let mine = create(&a.jmap, task, true).await.unwrap_or_else(|e| panic!("поручение не создано: {e}"));
    let (cal_a, _) = a.dav.default_calendar().await;
    let (_, ics) = a.dav.by_uid(&cal_a, "VTODO", &uid).await.unwrap();
    println!("у поручившего:\n{ics}");
    assert!(ics.contains("ROLE=NON-PARTICIPANT") && ics.contains(&format!("mailto:{}", w.email)), "{ics}");

    // Копия у исполнителя — в его календаре, сама (как у VEVENT, п. 4), и письмо «Invitation: …» с text/calendar.
    let props = ["participants", "progress", "percentComplete", "sequence", "due", "iCalendar", "organizerCalendarAddress"];
    let theirs = eventually("копия у исполнителя", DELIVERY, || by_uid(&b.jmap, &uid, &props)).await.expect("поручение не дошло");
    let their_id = by_uid(&b.jmap, &uid, &["id"]).await.unwrap()["id"].as_str().unwrap().to_owned();
    assert_eq!(theirs["organizerCalendarAddress"], format!("mailto:{}", a.email));
    assert_eq!(theirs["participants"]["p"]["participationStatus"], "needs-action");
    assert!(
        theirs["iCalendar"]["properties"].as_array().unwrap().contains(&json!(["x-postman-project", {}, "unknown", "vec"])),
        "X-POSTMAN-* доходят до исполнителя"
    );
    assert!(
        mail_arrives(&b, "s601", &format!("Invitation: {title}"), DELIVERY).await,
        "письмо не пришло: порог входящих на адрес (docs/STALWART.md п. 17)?"
    );
    // Сейчас: наблюдатель получает то же приглашение и копию — сервер не различает «только итоги» (решение 9).
    assert!(eventually("копия у наблюдателя", DELIVERY, || by_uid(&w.jmap, &uid, &["id"])).await.is_some());
    assert!(
        mail_arrives(&w, "s601", &format!("Invitation: {title}"), DELIVERY).await,
        "письмо не пришло: порог входящих на адрес (docs/STALWART.md п. 17)?"
    );

    let at_organizer = |want: fn(&Value) -> bool| {
        let c = a.jmap.clone();
        let mine = mine.clone();
        async move {
            let v = get(&c, &mine, Some(&["participants", "progress", "percentComplete", "due", "sequence"])).await;
            want(&v).then_some(v)
        }
    };

    // Принять.
    update(&b.jmap, &their_id, json!({ "participants/p/participationStatus": "accepted" }), true).await;
    let v = eventually("принято у поручившего", DELIVERY, || {
        at_organizer(|v| v["participants"]["p"]["participationStatus"] == "accepted")
    })
    .await;
    assert!(v.is_some(), "REPLY ACCEPTED не дошёл");

    // В работе, 50 %: прогресс участника и задачи — одним патчем.
    update(&b.jmap, &their_id, json!({ "participants/p/progress": "in-process", "progress": "in-process", "percentComplete": 50 }), true).await;
    let v = eventually("в работе у поручившего", DELIVERY, || {
        at_organizer(|v| v["percentComplete"] == 50 && v["participants"]["p"]["progress"] == "in-process")
    })
    .await;
    let v = v.expect("прогресс исполнителя не дошёл");
    assert_eq!(v["progress"], "in-process");
    let (_, ics) = a.dav.by_uid(&cal_a, "VTODO", &uid).await.unwrap();
    assert!(ics.contains("PARTSTAT=IN-PROCESS") && ics.contains("PERCENT-COMPLETE:50"), "{ics}");

    // Готово — на проверку.
    update(&b.jmap, &their_id, json!({ "participants/p/progress": "completed", "progress": "completed", "percentComplete": 100 }), true).await;
    let v = eventually("готово у поручившего", DELIVERY, || at_organizer(|v| v["participants"]["p"]["progress"] == "completed")).await;
    let v = v.expect("«готово» не дошло");
    assert_eq!((v["progress"].as_str(), v["percentComplete"].as_u64()), (Some("completed"), Some(100)));
    let (_, ics) = a.dav.by_uid(&cal_a, "VTODO", &uid).await.unwrap();
    println!("у поручившего после «готово»:\n{ics}");
    assert!(ics.contains("PARTSTAT=COMPLETED"), "{ics}");

    // Поручивший меняет срок → REQUEST с SEQUENCE 1 у исполнителя (и у наблюдателя).
    update(&a.jmap, &mine, json!({ "due": "2027-03-05T18:00:00" }), true).await;
    let t = eventually("новый срок у исполнителя", DELIVERY, || async {
        by_uid(&b.jmap, &uid, &["due", "sequence"]).await.filter(|v| v["due"] == "2027-03-05T18:00:00")
    })
    .await
    .expect("изменение срока не дошло");
    assert_eq!(t["sequence"], 1);
    assert!(
        mail_arrives(&b, "s601", &format!("Updated invitation: {title}"), DELIVERY).await,
        "письмо не пришло: порог входящих на адрес (docs/STALWART.md п. 17)?"
    );

    // «Предложить срок» письмом: COUNTER (RFC 5546 §3.4.7). Сейчас: письмо доходит, копию у поручившего сервер не
    // меняет — предложение разбирает и показывает клиент.
    let counter = imip(
        "COUNTER",
        &format!(
            "UID:{uid}\r\nDTSTAMP:20261021T193000Z\r\nSEQUENCE:1\r\nSUMMARY:{title}\r\nDUE;TZID=Europe/Moscow:20270309T180000\r\n\
             ORGANIZER:mailto:{}\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:{}\r\nCOMMENT:Жду данные\\, раньше не соберу\r\n",
            a.email, b.email
        ),
    );
    send_imip(&b, &a.email, &format!("Counter: {title}"), "COUNTER", &counter).await;
    assert!(
        mail_arrives(&a, "s601", &format!("Counter: {title}"), DELIVERY).await,
        "письмо не пришло: порог входящих на адрес (docs/STALWART.md п. 17)?"
    );
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(get(&a.jmap, &mine, Some(&["due"])).await["due"], "2027-03-05T18:00:00", "COUNTER применён сервером");

    // «Отклонить с комментарием» письмом: REPLY DECLINED + COMMENT. Сейчас: статус сервер применяет, COMMENT — нет.
    let decline = imip(
        "REPLY",
        &format!(
            "UID:{uid}\r\nDTSTAMP:20261021T193100Z\r\nSEQUENCE:1\r\nORGANIZER:mailto:{}\r\n\
             ATTENDEE;PARTSTAT=DECLINED:mailto:{}\r\nCOMMENT:Нет доступа к системе\r\n",
            a.email, b.email
        ),
    );
    send_imip(&b, &a.email, &format!("Declined: {title}"), "REPLY", &decline).await;
    let v = eventually("отказ у поручившего", DELIVERY, || {
        at_organizer(|v| v["participants"]["p"]["participationStatus"] == "declined")
    })
    .await;
    assert!(v.is_some(), "REPLY письмом не применён");
    let (_, ics) = a.dav.by_uid(&cal_a, "VTODO", &uid).await.unwrap();
    assert!(ics.contains("PARTSTAT=DECLINED") && !ics.contains("Нет доступа"), "{ics}");

    // «Забрать»: удаление с рассылкой → CANCEL. Сейчас: наблюдателю — да, отказавшемуся исполнителю — нет.
    destroy(&a.jmap, std::slice::from_ref(&mine), true).await;
    let cancelled = eventually("отмена у наблюдателя", DELIVERY, || async {
        by_uid(&w.jmap, &uid, &["progress"]).await.filter(|v| v["progress"] == "cancelled")
    })
    .await;
    assert!(cancelled.is_some(), "CANCEL не дошёл до наблюдателя");
    assert!(
        mail_arrives(&w, "s601", &format!("Cancelled: {title}"), DELIVERY).await,
        "письмо не пришло: порог входящих на адрес (docs/STALWART.md п. 17)?"
    );
    let t = by_uid(&b.jmap, &uid, &["progress"]).await.expect("копия исполнителя");
    assert_ne!(t["progress"], "cancelled", "CANCEL дошёл и до отказавшегося — обновить находку");
    assert!(mails(&b.jmap, "s601", &format!("Cancelled: {title}")).await.is_empty());

    // У поручившего — 5 писем: «Accepted», два «Reply» (в работе, готово), COUNTER и отказ; ответы сервер шлёт с
    // задержкой — дождаться всех, иначе уборка их не застанет.
    let got = eventually("все ответы у поручившего", DELIVERY, || async {
        let m = mails(&a.jmap, "s601", &title).await;
        (m.len() >= 5).then_some(m)
    })
    .await;
    println!("письма у поручившего: {got:?}");

    for x in [&a, &b, &w] {
        sweep(x, prefix, subject).await;
    }
}

/// 7. Напоминания: `alerts` хранятся (тесты 2, 4); письмо сервера («Notification: …» от `calendar-notification@`)
/// приходит только для `action: email` относительно начала — у события и у задачи с `start`. Для срока задачи
/// (`relativeTo: end`) и для `display` — ничего: напоминания по сроку — только клиентом (решение 7).
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn alerts_server_mails_only_start_relative_email() {
    let u = user(1).await;
    let prefix = "postman-s601-alert-";
    let subject = "Postman s601 alert";
    sweep(&u, prefix, subject).await;
    let cal = default_calendar(&u.jmap).await;
    let run = stamp();
    // Всё срабатывает через минуту: начало / срок через 3 мин, напоминание за 2 мин.
    let at = (jiff::Timestamp::now() + jiff::Span::new().minutes(3)).strftime("%Y-%m-%dT%H:%M:%S").to_string();
    let later = (jiff::Timestamp::now() + jiff::Span::new().minutes(10)).strftime("%Y-%m-%dT%H:%M:%S").to_string();
    let alert = |action: &str, rel: &str| json!({ "a": { "@type": "Alert", "action": action, "trigger": { "@type": "OffsetTrigger", "offset": "-PT2M", "relativeTo": rel } } });
    let items = [
        ("event-email", json!({ "@type": "Event", "start": at, "duration": "PT30M", "alerts": alert("email", "start") }), true),
        ("task-start-email", json!({ "@type": "Task", "start": at, "due": later, "alerts": alert("email", "start") }), true),
        ("task-due-email", json!({ "@type": "Task", "due": at, "alerts": alert("email", "end") }), false),
        ("task-due-display", json!({ "@type": "Task", "due": at, "alerts": alert("display", "end") }), false),
    ];
    let mut ids = Vec::new();
    for (name, extra, _) in &items {
        let mut v = json!({ "uid": format!("{prefix}{name}-{run}@itt.test"), "calendarIds": { &cal: true },
            "title": format!("{subject} {name} {run}"), "timeZone": "Etc/UTC" });
        v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        ids.push(create(&u.jmap, v, false).await.unwrap());
    }
    // Ждём 2 письма (~1 мин), затем ещё минуту — что других нет.
    let deadline = Duration::from_secs(150);
    let got = eventually("письма напоминаний", deadline, || async {
        let m = mails(&u.jmap, "s601", &run).await;
        (m.len() >= 2).then_some(m)
    })
    .await
    .unwrap_or_default();
    tokio::time::sleep(Duration::from_secs(60)).await;
    let all = mails(&u.jmap, "s601", &run).await;
    println!("письма сервера: {all:?}");
    assert!(got.len() >= 2, "сервер не прислал напоминания по action: email: {all:?}");
    for (name, _, mailed) in &items {
        let has = all.iter().any(|(_, s)| s.starts_with("Notification:") && s.contains(&format!("{name} {run}")));
        assert_eq!(has, *mailed, "{name}: письмо {}", if *mailed { "не пришло" } else { "пришло — обновить находку" });
    }
    // Напоминания остались в записях как были.
    for id in &ids {
        assert!(get(&u.jmap, id, Some(&["alerts"])).await["alerts"]["a"].is_object());
    }
    destroy(&u.jmap, &ids, false).await;
    sweep(&u, prefix, subject).await;
}

/// 6б. Поручение своим письмом (iMIP от клиента, без `sendSchedulingMessages`): REQUEST с VTODO, тема и текст —
/// Postman. Сервер исполнителя сам кладёт VTODO в его календарь (как iTIP сервера), письмо — во «Входящих» как
/// отправили. Значит, письмо-поручение можно делать своим (HTML для людей без Postman, решение 3). Письмо: user2 — 1.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn imip_request_by_mail_lands_in_calendar() {
    let (a, b) = (user(1).await, user(2).await);
    let prefix = "postman-s601-imip-";
    let subject = "Postman s601 imip";
    sweep(&a, prefix, subject).await;
    sweep(&b, prefix, subject).await;
    let run = stamp();
    let uid = format!("{prefix}{run}@itt.test");
    let title = format!("{subject} {run}");
    let request = imip(
        "REQUEST",
        &format!(
            "UID:{uid}\r\nDTSTAMP:20261021T200000Z\r\nSEQUENCE:0\r\nSUMMARY:{title}\r\nDUE;VALUE=DATE:20270310\r\n\
             STATUS:NEEDS-ACTION\r\nORGANIZER:mailto:{}\r\nATTENDEE;RSVP=TRUE;PARTSTAT=NEEDS-ACTION:mailto:{}\r\n\
             X-POSTMAN-PROJECT:vec\r\n",
            a.email, b.email
        ),
    );
    send_imip(&a, &b.email, &format!("Поручение: {title}"), "REQUEST", &request).await;
    let t = eventually("VTODO из письма у исполнителя", DELIVERY, || {
        by_uid(&b.jmap, &uid, &["@type", "due", "participants", "iCalendar"])
    })
    .await
    .expect("сервер не положил VTODO из iMIP в календарь (или письмо не дошло: порог входящих, п. 17)");
    println!("копия из iMIP: {t}");
    assert_eq!(t["@type"], "Task");
    assert_eq!(t["due"], "2027-03-10T00:00:00");
    assert!(t["iCalendar"]["properties"].as_array().unwrap().contains(&json!(["x-postman-project", {}, "unknown", "vec"])));
    assert!(
        mail_arrives(&b, "s601", &format!("Поручение: {title}"), DELIVERY).await,
        "письмо не пришло: порог входящих на адрес (docs/STALWART.md п. 17)?"
    );
    sweep(&a, prefix, subject).await;
    sweep(&b, prefix, subject).await;
}

/// S6-02: хранилище задач Postman (`postman_core::tasks`, `JmapStore`) — два клиента user1 со своими кэшами:
/// создать → у второго по дельте (`CalendarEvent/changes`) → выполнить вторым → у первого → удалить → у второго нет.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn task_round_trip() {
    use postman_backend_jmap::tasks::JmapStore;
    use postman_core::tasks::{self, Reply, Request};
    let (a, b) = (user(1).await, user(1).await);
    let prefix = "postman-s601-store-";
    let subject = "Postman s601 store";
    sweep(&a, prefix, subject).await;
    let (ca, cb) = (postman_cache::Cache::open_in_memory().unwrap(), postman_cache::Cache::open_in_memory().unwrap());
    let (sa, sb) = (JmapStore::new(&a.jmap), JmapStore::new(&b.jmap));
    let quiet = |_: Reply| {};
    let list = |c: &postman_cache::Cache| -> Vec<postman_calendar::Task> {
        c.tasks().unwrap().iter().filter_map(tasks::from_row).filter(|t| t.uid.starts_with(prefix)).collect()
    };
    tasks::handle(&sa, &ca, Request::Load, &quiet).await.unwrap();
    tasks::handle(&sb, &cb, Request::Load, &quiet).await.unwrap();

    let uid = format!("{prefix}{}@itt.test", stamp());
    let mut t = postman_calendar::Task::new(&uid, "", &format!("{subject} {}", stamp()));
    t.project = Some("IV кв.".into());
    t.important = true;
    t.due = Some(postman_calendar::Moment::new("2027-03-02T16:00:00".parse().unwrap(), Some("Europe/Moscow")));
    tasks::handle(&sa, &ca, Request::Save(Box::new(t)), &quiet).await.unwrap();
    let mine = list(&ca);
    assert_eq!(mine.len(), 1);
    assert!(!mine[0].id.is_empty() && !mine[0].calendar_ids.is_empty(), "{:?}", mine[0]);
    assert!(ca.dirty_tasks().unwrap().is_empty());

    tasks::handle(&sb, &cb, Request::Refresh, &quiet).await.unwrap();
    let theirs = list(&cb);
    assert_eq!(theirs.len(), 1, "второй клиент не увидел задачу по дельте");
    assert_eq!((theirs[0].project.as_deref(), theirs[0].important), (Some("IV кв."), true));
    let mut done = theirs[0].clone();
    done.set_done(true, jiff::Timestamp::now().as_second());
    tasks::handle(&sb, &cb, Request::Save(Box::new(done)), &quiet).await.unwrap();

    tasks::handle(&sa, &ca, Request::Refresh, &quiet).await.unwrap();
    let mine = list(&ca);
    assert_eq!(mine[0].progress, postman_calendar::Progress::Completed);
    assert!(mine[0].completed_at.is_some(), "дата выполнения дошла через iCalendar");
    assert_eq!(mine[0].project.as_deref(), Some("IV кв."), "свои поля не потеряны патчем");

    tasks::handle(&sa, &ca, Request::Delete(uid.clone()), &quiet).await.unwrap();
    tasks::handle(&sb, &cb, Request::Refresh, &quiet).await.unwrap();
    assert!(list(&cb).is_empty(), "удаление не дошло до второго клиента");
    assert!(by_uid(&a.jmap, &uid, &["id"]).await.is_none());
    sweep(&a, prefix, subject).await;
}

/// Письмо стенда как строка кэша (только то, что нужно связи письма и задачи).
async fn letter_row(c: &Client, id: &str) -> Option<postman_cache::EmailRow> {
    let (list, _) = c.email_get(&[id.to_owned()]).await.unwrap();
    let e = list.into_iter().next()?;
    let mut keywords: Vec<String> = e.keywords.iter().filter(|(_, on)| **on).map(|(k, _)| k.to_lowercase()).collect();
    keywords.sort();
    keywords.retain(|k| k != "$seen" && k != "$flagged");
    Some(postman_cache::EmailRow {
        id: e.id,
        thread_id: e.thread_id,
        mailbox_ids: e.mailbox_ids.into_keys().collect(),
        received_at: 0,
        subject: e.subject.unwrap_or_default(),
        from_name: String::new(),
        from_addr: String::new(),
        preview: e.preview,
        seen: true,
        flagged: false,
        has_attachment: e.has_attachment,
        size: e.size,
        keywords,
    })
}

/// S6-03: письмо со сроком у user1 — задача «Из почты» → заметка → задача в хранилище со ссылкой → выполнить →
/// оба слова срока сняты на сервере (видит второй клиент) → письмо удалено → у задачи второго клиента ссылки нет.
/// Письмо создаётся прямо во «Входящих» (`Email/set create`, без SMTP).
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn mail_due_is_task() {
    use postman_backend_jmap::tasks::JmapStore;
    use postman_core::mailtask;
    use postman_core::tasks::{self, Reply, Request};
    let (a, b) = (user(1).await, user(1).await);
    let prefix = "postman-s601-maildue-";
    let subject = "Postman s601 maildue";
    sweep(&a, prefix, subject).await;
    let tz = jiff::tz::TimeZone::system();
    let run = stamp();
    let (boxes, _) = a.jmap.mailboxes().await.unwrap();
    let inbox = boxes.iter().find(|m| m.role.as_deref() == Some("inbox")).unwrap().id.clone();
    let acc = a.jmap.mail_account().unwrap().to_owned();
    let email = json!({
        "mailboxIds": { &inbox: true },
        "keywords": { "$seen": true, "pm.due": true, "pm.due.2027-03-02t1500": true },
        "from": [{ "email": "plain@itt.test" }],
        "subject": format!("{subject} {run}"),
        "bodyValues": { "t": { "value": "Письмо со сроком (S6-03)." } },
        "textBody": [{ "partId": "t", "type": "text/plain" }],
    });
    let r = call(&a.jmap, &[CORE, MAIL], vec![("Email/set", json!({ "accountId": acc, "create": { "n": email } }))]).await;
    let email_id = r[0]["created"]["n"]["id"].as_str().unwrap_or_else(|| panic!("письмо не создано: {}", r[0])).to_owned();

    // Письмо — задача «Из почты» со сроком письма.
    let row = letter_row(&a.jmap, &email_id).await.unwrap();
    let letter = mailtask::as_task(&row, None).expect("письмо со сроком — задача");
    assert_eq!(mailtask::due_when(&letter, &tz), mailtask::letter_due(&row));
    // Правка заметки — задача в хранилище со ссылкой.
    let mut t = mailtask::promote(&row, None, &format!("{prefix}{run}@itt.test"));
    t.notes = "Заметка к письму".into();
    let (ca, cb) = (postman_cache::Cache::open_in_memory().unwrap(), postman_cache::Cache::open_in_memory().unwrap());
    let (sa, sb) = (JmapStore::new(&a.jmap), JmapStore::new(&b.jmap));
    let quiet = |_: Reply| {};
    tasks::handle(&sa, &ca, Request::Save(Box::new(t.clone())), &quiet).await.unwrap();

    // Выполнить: задача — выполнена, у письма — оба слова срока сняты одним Email/set.
    let mut done = tasks::from_row(&ca.task(&t.uid).unwrap().unwrap()).unwrap();
    done.set_done(true, jiff::Timestamp::now().as_second());
    let patch = mailtask::letter_patch(&done, &row, &tz).expect("выполнение снимает срок");
    let add: Vec<String> = patch.keywords.iter().filter(|(_, on)| *on).map(|(k, _)| k.clone()).collect();
    let remove: Vec<String> = patch.keywords.iter().filter(|(_, on)| !*on).map(|(k, _)| k.clone()).collect();
    assert!(a.jmap.set_keywords(std::slice::from_ref(&email_id), &add, &remove).await.unwrap().is_empty());
    tasks::handle(&sa, &ca, Request::Save(Box::new(done)), &quiet).await.unwrap();
    let seen = letter_row(&b.jmap, &email_id).await.unwrap();
    assert!(!seen.keywords.iter().any(|k| k.starts_with("pm.due")), "у второго клиента срок не снят: {:?}", seen.keywords);

    // Второй клиент: задача выполнена; письмо удалено — сверка снимает ссылку, не падая.
    tasks::handle(&sb, &cb, Request::Load, &quiet).await.unwrap();
    let theirs = tasks::from_row(&cb.task(&t.uid).unwrap().expect("задача у второго клиента")).unwrap();
    assert_eq!(theirs.progress, postman_calendar::Progress::Completed);
    let mut reopened = theirs.clone();
    reopened.set_done(false, 0);
    a.jmap.destroy_email(&email_id).await.unwrap();
    let lookup = |_: &str| None;
    let fixed = mailtask::reconcile(&[reopened], lookup, 0, &tz);
    assert_eq!(fixed.len(), 1);
    assert_eq!(fixed[0].mail, None, "письмо удалено — задача без ссылки");
    sweep(&a, prefix, subject).await;
}

/// S6-08: «Время в календаре» — блок под задачу: событие «занят» в своём календаре со ссылкой на задачу
/// (`relatedTo` с `parent`), у задачи — `X-POSTMAN-BLOCK`; «Убрать» — событие удалено.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn task_block_in_calendar() {
    use postman_backend_jmap::tasks::JmapStore;
    use postman_core::tasks::{self, Reply, Request};
    let a = user(1).await;
    let prefix = "postman-s601-block-";
    let subject = "Postman s601 block";
    sweep(&a, prefix, subject).await;
    let cache = postman_cache::Cache::open_in_memory().unwrap();
    let store = JmapStore::new(&a.jmap);
    let quiet = |_: Reply| {};
    let uid = format!("{prefix}{}@itt.test", stamp());
    let t = postman_calendar::Task::new(&uid, "", &format!("{subject} {}", stamp()));
    tasks::handle(&store, &cache, Request::Save(Box::new(t)), &quiet).await.unwrap();
    let start = (jiff::Timestamp::now() + jiff::Span::new().hours(48)).as_second() / 3600 * 3600;
    tasks::handle(&store, &cache, Request::Block { uid: uid.clone(), start, end: start + 1800 }, &quiet).await.unwrap();
    let task = tasks::from_row(&cache.task(&uid).unwrap().unwrap()).unwrap();
    let block = task.block.clone().expect("у задачи блок");
    assert_eq!((block.start, block.end), (start, start + 1800));
    let ev = get(&a.jmap, &block.event_id, Some(&["@type", "title", "duration", "freeBusyStatus", "relatedTo", "keywords", "utcStart"])).await;
    println!("событие блока: {ev}");
    assert_eq!(ev["@type"], "Event");
    assert_eq!(ev["duration"], "PT30M");
    assert_eq!(ev["relatedTo"][&uid]["relation"]["parent"], true, "ссылка на задачу");
    assert_eq!(ev["utcStart"].as_str().and_then(postman_jmap::utc_to_unix), Some(start));
    // Задача на сервере — с X-POSTMAN-BLOCK (второй клиент увидит блок).
    let server = by_uid(&a.jmap, &uid, &["iCalendar"]).await.unwrap();
    assert!(server["iCalendar"]["properties"].as_array().unwrap().iter().any(|p| p[0] == "x-postman-block"), "{server}");
    tasks::handle(&store, &cache, Request::Unblock { uid: uid.clone() }, &quiet).await.unwrap();
    assert!(get(&a.jmap, &block.event_id, Some(&["id"])).await.is_null(), "событие блока удалено");
    sweep(&a, prefix, subject).await;
}

/// S6-09: поручение путём Postman (`postman_core::assign`): своя запись поручившего без рассылки и свои письма iMIP
/// (REQUEST, REPLY с COMMENT). user1 поручает user2, user3 — наблюдатель: принять → в работе → готово → принять
/// результат. Проверяется: копия у исполнителя из письма, PARTSTAT из писем REPLY у копии поручившего, созданной
/// без рассылки [проверить — S6-01 этого не проверял], итог у наблюдателя. Комментарии сервер не хранит —
/// их учитывает `assign::receive` (здесь — по тексту письма, как его разберёт воркер).
/// Писем: user2 — 2, user3 — 3, user1 — 3.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn postman_delegation_cycle() {
    use postman_backend_jmap::tasks::JmapStore;
    use postman_calendar::assign::{self as model, Action, Incoming, Status};
    use postman_core::assign::{self, Input, Received};
    use postman_core::tasks::{self, Reply, Request};
    let (a, b, w) = (user(1).await, user(2).await, user(3).await);
    let prefix = "postman-s609-deleg-";
    let subject = "Postman s609 deleg";
    for x in [&a, &b, &w] {
        sweep(x, prefix, subject).await;
    }
    let tz = jiff::tz::TimeZone::get("Europe/Moscow").unwrap();
    let run = stamp();
    let uid = format!("{prefix}{run}@itt.test");
    let title = format!("{subject} {run}");
    let now = || jiff::Timestamp::now().as_second();
    let parse = |ics: &str| Incoming::from_worker(&postman_worker::parse_calendar(ics.as_bytes()).unwrap()).unwrap();
    let addr = |u: &User, n: &str| postman_compose::Address::new(n, &u.email);
    let quiet = |_: Reply| {};
    let (ca, cb) = (postman_cache::Cache::open_in_memory().unwrap(), postman_cache::Cache::open_in_memory().unwrap());
    let (sa, sb) = (JmapStore::new(&a.jmap), JmapStore::new(&b.jmap));

    // Поручить: своя запись (без рассылки) и письмо REQUEST исполнителю и наблюдателю.
    let due = postman_calendar::Moment::new("2027-03-04T18:00:00".parse().unwrap(), Some("Europe/Moscow"));
    let plan = assign::assign(
        &postman_calendar::Task::new(&uid, "", &title),
        &addr(&a, "User 1"),
        &[addr(&w, "User 3")],
        &Input { comment: "Нужен к четвергу".into(), due: Some(due), to: Some(addr(&b, "User 2")) },
        now(),
        &tz,
    )
    .unwrap();
    tasks::handle(&sa, &ca, Request::Save(Box::new(plan.task.clone())), &quiet).await.unwrap();
    let out = &plan.outgoing[0];
    for to in &out.to {
        send_imip(&a, &to.email, &format!("Поручение: {title}"), &out.method, &out.ics).await;
    }

    // У исполнителя — копия из письма (сервер кладёт VTODO сам); шаг «поручено» с комментарием — из письма.
    let raw = eventually("копия у исполнителя", DELIVERY, || by_uid(&b.jmap, &uid, postman_calendar::TASK_PROPERTIES))
        .await
        .expect("поручение не дошло (порог входящих, п. 17?)");
    tasks::handle(&sb, &cb, Request::Load, &quiet).await.unwrap();
    let mut mine = tasks::from_row(&cb.task(&uid).unwrap().expect("копия в кэше исполнителя")).unwrap();
    assert!(mine.is_assigned_to(&b.email), "{raw}");
    if let Received::Applied(t) = assign::receive(&mine, &parse(&out.ics), now()) {
        mine = *t;
    }
    assert!(eventually("копия у наблюдателя", DELIVERY, || by_uid(&w.jmap, &uid, &["id"])).await.is_some());

    // Шаги исполнителя: своя копия — правкой без рассылки, ответ — своим письмом REPLY.
    let mut boss = tasks::from_row(&ca.task(&uid).unwrap().unwrap()).unwrap();
    for (action, comment, want) in
        [(Action::Accept, "", Status::Accepted), (Action::Start, "Начал", Status::InProgress), (Action::Finish, "Отчёт готов", Status::Done)]
    {
        let p = assign::act(&mine, &b.email, action, &Input { comment: comment.into(), ..Input::default() }, now(), &tz).unwrap();
        tasks::handle(&sb, &cb, Request::Save(Box::new(p.task.clone())), &quiet).await.unwrap();
        mine = tasks::from_row(&cb.task(&uid).unwrap().unwrap()).unwrap();
        for o in &p.outgoing {
            for to in &o.to {
                send_imip(&b, &to.email, &format!("Ответ: {title}"), &o.method, &o.ics).await;
            }
            // Сервер поручившего применяет REPLY к записи, созданной без рассылки? Сейчас — печатаем, что вышло.
            let want_ps = model::assignee(&p.task).unwrap().status;
            let applied = eventually("ответ у поручившего на сервере", DELIVERY, || async {
                let v = by_uid(&a.jmap, &uid, postman_calendar::TASK_PROPERTIES).await?;
                let t = postman_calendar::Task::from_jmap(&v).ok()?;
                (model::assignee(&t)?.status == want_ps).then_some(())
            })
            .await;
            let v = by_uid(&a.jmap, &uid, &["progress", "percentComplete"]).await.unwrap_or_default();
            println!("{action:?}: сервер применил REPLY к копии поручившего — {}; ход {} {}", applied.is_some(), v["progress"], v["percentComplete"]);
            if let Received::Applied(t) = assign::receive(&boss, &parse(&o.ics), now()) {
                boss = *t;
            }
        }
        assert_eq!(model::status(&boss), Some(want), "{action:?}");
    }
    assert_eq!(model::steps(&boss).last().map(|s| s.comment.as_str()), Some("Отчёт готов"), "комментарий — из письма");
    tasks::handle(&sa, &ca, Request::Save(Box::new(boss.clone())), &quiet).await.unwrap();

    // Принять результат: REQUEST исполнителю и наблюдателю; у исполнителя — закрыто.
    let res = assign::act(&boss, &a.email, Action::AcceptResult, &Input::default(), now(), &tz).unwrap();
    tasks::handle(&sa, &ca, Request::Save(Box::new(res.task.clone())), &quiet).await.unwrap();
    for to in &res.outgoing[0].to {
        send_imip(&a, &to.email, &format!("Принято: {title}"), "REQUEST", &res.outgoing[0].ics).await;
    }
    assert!(mail_arrives(&w, "s609", &format!("Принято: {title}"), DELIVERY).await, "итог не пришёл наблюдателю (порог входящих, п. 17?)");
    let closed = eventually("итог у исполнителя", DELIVERY, || async {
        let v = by_uid(&b.jmap, &uid, postman_calendar::TASK_PROPERTIES).await?;
        let t = postman_calendar::Task::from_jmap(&v).ok()?;
        (t.progress == postman_calendar::Progress::Completed).then_some(t)
    })
    .await;
    let t = closed.expect("итог не применён к копии исполнителя");
    println!("копия исполнителя после итога: X-POSTMAN-RESULT — {:?}", model::prop(&t, model::X_RESULT));
    assert_eq!(model::status(&t), Some(Status::Closed), "закрыто у исполнителя и без разбора письма");
    if let Received::Applied(t) = assign::receive(&mine, &parse(&res.outgoing[0].ics), now()) {
        assert_eq!(model::status(&t), Some(Status::Closed));
    }
    for x in [&a, &b, &w] {
        sweep(x, prefix, subject).await;
    }
}
