//! Настройки ящика на живом Stalwart (S4-05, S4-07): автоответ, квота и правила — тем же кодом, что и приложение
//! (`postman_core::settings`). Меняются настройки только user2; user1 пишет ему письма. Без стенда тесты
//! пропускаются (`#[ignore]`); за собой убирают (автоответ выключен, правила и папки удалены, письма удалены).
//!
//! Запуск — как `stand.rs` (`docs/STALWART.md`):
//!   cargo test -p postman-backend-jmap --test stand_settings -- --ignored

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)

use std::cell::RefCell;
use std::future::Future;
use std::time::{Duration, Instant};

use postman_compose::{Address, Draft};
use postman_core::autoreply::{Audience, AutoReply, Text};
use postman_core::model::VacationResponse;
use postman_core::rules::{Action, Field, Rule};
use postman_core::settings::{AutoReplySave, Reply, Request, RulesState};
use postman_jmap::{Activate, Auth, Client, Envelope, Invocation, SaveError};
use serde_json::{Value, json};

/// Сколько ждать доставки письма и автоответа.
const DELIVERY: Duration = Duration::from_secs(60);

struct User {
    email: String,
    client: Client,
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} не задан: стенд — docs/STALWART.md"))
}

async fn user(n: u8) -> User {
    let email = env(&format!("POSTMAN_STAND_USER{n}"));
    let password = env(&format!("POSTMAN_STAND_PASSWORD{n}"));
    let client = Client::connect(&env("POSTMAN_STAND_URL"), Auth::Basic { user: email.clone(), password })
        .await
        .unwrap_or_else(|e| panic!("вход {email}: {e}"));
    User { email, client }
}

fn stamp() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    format!("{}-{}", t.as_secs(), t.subsec_nanos() % 1000)
}

/// Повторять `f`, пока не вернёт `Some`, не дольше `DELIVERY`; `None` — не дождались.
async fn eventually<T, F, Fut>(mut f: F) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let start = Instant::now();
    loop {
        if let Some(v) = f().await {
            return Some(v);
        }
        if start.elapsed() > DELIVERY {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(1000)).await;
    }
}

/// Запрос настроек как в приложении; ответы — списком.
async fn settings(client: &Client, request: Request) -> Vec<Reply> {
    let replies = RefCell::new(Vec::new());
    postman_backend_jmap::settings::handle(client, request, &|r| replies.borrow_mut().push(r)).await;
    replies.into_inner()
}

fn role(boxes: &[postman_jmap::Mailbox], r: &str) -> String {
    boxes.iter().find(|m| m.role.as_deref() == Some(r)).map(|m| m.id.clone()).unwrap()
}

/// Письмо `from` → `to` тем же путём, что и приложение (blob → «Черновики» → EmailSubmission).
async fn send(from: &User, to: &str, subject: &str, text: &str) -> String {
    let (boxes, _) = from.client.mailboxes().await.unwrap();
    let identity = from.client.identities().await.unwrap().into_iter().find(|i| i.email == from.email).unwrap();
    let draft = Draft {
        from: Address::new("User 1", &from.email),
        to: vec![Address::new("", to)],
        subject: subject.into(),
        markdown: text.into(),
        ..Default::default()
    };
    let now = jiff::Timestamp::now().as_second();
    let raw = postman_compose::build(&draft, now, &postman_compose::new_message_id(&draft.from, now)).unwrap();
    let envelope = Envelope { mail_from: from.email.clone(), rcpt_to: vec![to.into()] };
    from.client.send(raw, &identity.id, &envelope, &role(&boxes, "drafts"), &role(&boxes, "sent")).await.unwrap()
}

/// Письма с текстом `text` во всех папках пользователя: (id, тема, ящики).
async fn find(u: &User, text: &str) -> Vec<postman_jmap::EmailSummary> {
    u.client.search(text, None, 20).await.map(|p| p.emails).unwrap_or_default()
}

async fn destroy_all(u: &User, text: &str) {
    for e in find(u, text).await {
        let _ = u.client.destroy_email(&e.id).await;
    }
}

/// S4-05: автоответ user2 включается, user1 получает ответ на своё письмо, автоответ выключается.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn vacation_reply_round_trip() {
    let (a, b) = (user(1).await, user(2).await);
    let stamp = stamp();
    let before = <VacationResponse as postman_backend_jmap::convert::FromJmap<_>>::from_jmap(b.client.vacation().await.unwrap());
    let reply_subject = format!("Postman vacation {stamp}");

    let result = async {
        let on = VacationResponse {
            is_enabled: true,
            from_date: None,
            to_date: None,
            subject: Some(reply_subject.clone()),
            text_body: Some("Нет на месте (тест стенда).".into()),
            html_body: None,
        };
        let saved = match settings(&b.client, Request::SetVacation(on.clone())).await.into_iter().next() {
            Some(Reply::VacationSaved(v)) => v,
            Some(Reply::Failed { error, .. }) => return Err(format!("автоответ не включён: {error}")),
            _ => return Err("нет ответа VacationSaved".into()),
        };
        if saved != (VacationResponse { html_body: saved.html_body.clone(), ..on }) {
            return Err(format!("сервер сохранил не то: {saved:?}"));
        }
        send(&a, &b.email, &format!("Postman probe {stamp}"), "Проверка автоответа.").await;
        let got = eventually(|| async {
            find(&a, &reply_subject).await.into_iter().find(|e| {
                e.subject.as_deref() == Some(reply_subject.as_str())
                    && e.from.iter().flatten().any(|f| f.email.as_deref().is_some_and(|m| m.eq_ignore_ascii_case(&b.email)))
            })
        })
        .await;
        got.map(|_| ()).ok_or_else(|| format!("автоответ не пришёл за {DELIVERY:?}"))
    }
    .await;

    // Уборка — и при сбое: автоответ как был (выключен), письма прогона удалены.
    let off = VacationResponse { is_enabled: false, ..before };
    let replies = settings(&b.client, Request::SetVacation(off)).await;
    let off_ok = replies.iter().any(|r| matches!(r, Reply::VacationSaved(v) if !v.is_enabled));
    destroy_all(&a, &stamp).await;
    destroy_all(&b, &stamp).await;
    result.unwrap();
    assert!(off_ok, "автоответ не выключился");
    assert!(!b.client.vacation().await.unwrap().is_enabled);
}

/// S5-29 [проверить]: свой скрипт `vacation` Postman — коллеге (свой домен) уходит текст «Внутри компании»,
/// настройки читаются обратно из скрипта, `VacationResponse/get` после записи скрипта работает, событие «Вне офиса»
/// ставится на период и убирается при выключении.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn autoreply_split_texts() {
    let (a, b) = (user(1).await, user(2).await);
    let stamp = stamp();
    let before = <VacationResponse as postman_backend_jmap::convert::FromJmap<_>>::from_jmap(b.client.vacation().await.unwrap());
    let internal = format!("Postman inside {stamp}");
    let external = format!("Postman outside {stamp}");
    let reply = AutoReply {
        internal: Text { subject: internal.clone(), body: "Коллеги, я в отпуске (**тест стенда**).".into() },
        external: Text { subject: external.clone(), body: "Спасибо за письмо (тест стенда).".into() },
        ..AutoReply::default()
    };
    let now = jiff::Timestamp::now();
    let period = |h: i64| now.checked_add(jiff::SignedDuration::from_hours(h)).unwrap().to_string();
    let save = |on: bool, reply: AutoReply| AutoReplySave {
        vacation: VacationResponse {
            is_enabled: on,
            from_date: Some(period(-1)),
            to_date: Some(period(48)),
            subject: None,
            text_body: None,
            html_body: None,
        },
        reply,
        audience: Audience { domains: Audience::domains_of(&[a.email.clone(), b.email.clone()]), contacts: vec![], mine: vec![b.email.clone()] },
        ooo_title: format!("Вне офиса {stamp}"),
        zone: "Europe/Moscow".into(),
    };

    let result = async {
        let replies = settings(&b.client, Request::SetAutoReply(Box::new(save(true, reply.clone())))).await;
        if let Some(Reply::Failed { error, .. }) = replies.first() {
            return Err(format!("автоответ Postman не записан: {error}"));
        }
        let saved = replies.iter().find_map(|r| match r {
            Reply::VacationSaved(v) => Some(v.clone()),
            _ => None,
        });
        let back = replies.iter().find_map(|r| match r {
            Reply::AutoReply(r) => Some(r.clone()),
            _ => None,
        });
        if !saved.as_ref().is_some_and(|v| v.is_enabled) {
            return Err(format!("VacationResponse/get после своего скрипта: {saved:?}"));
        }
        if back.flatten().as_ref() != Some(&reply) {
            return Err("настройки не прочитаны из скрипта vacation".into());
        }
        // «Вне офиса» — событие на период.
        let events = b.client.events(None, &period(-48), &period(96)).await.unwrap_or_default();
        if !events.iter().any(|e| e["title"].as_str() == Some(format!("Вне офиса {stamp}").as_str())) {
            return Err("событие «Вне офиса» не появилось".into());
        }
        send(&a, &b.email, &format!("Postman probe {stamp}"), "Проверка автоответа Postman.").await;
        let got =
            eventually(|| async { find(&a, &stamp).await.into_iter().find_map(|e| e.subject.filter(|s| s == &internal || s == &external)) }).await;
        match got {
            Some(s) if s == internal => Ok(()),
            Some(s) => Err(format!("коллеге ушёл не тот текст: {s}")),
            None => Err(format!("автоответ не пришёл за {DELIVERY:?}")),
        }
    }
    .await;

    // Уборка: выключить (событие «Вне офиса» уходит), затем автоответ как был.
    let off = settings(&b.client, Request::SetAutoReply(Box::new(save(false, AutoReply { out_of_office: false, ..reply })))).await;
    let ooo_left = b
        .client
        .events(None, &period(-48), &period(96))
        .await
        .unwrap_or_default()
        .iter()
        .any(|e| e["title"].as_str() == Some(format!("Вне офиса {stamp}").as_str()));
    let _ = settings(&b.client, Request::SetVacation(VacationResponse { is_enabled: false, ..before })).await;
    destroy_all(&a, &stamp).await;
    destroy_all(&b, &stamp).await;
    result.unwrap();
    assert!(off.iter().any(|r| matches!(r, Reply::VacationSaved(v) if !v.is_enabled)), "автоответ не выключился");
    assert!(!ooo_left, "событие «Вне офиса» осталось");
}

/// S4-05: квота читается (у пользователя без ограничений — `None`), запрос без ошибки.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn quota_is_read() {
    let b = user(2).await;
    let replies = settings(&b.client, Request::Quota).await;
    match replies.as_slice() {
        [Reply::Quota(usage)] => {
            if let Some(u) = usage {
                assert!(u.limit > 0 && u.used <= u.limit.saturating_mul(2), "{u:?}");
            }
        }
        [Reply::Failed { error, .. }] => panic!("Quota/get: {error}"),
        _ => panic!("нет ответа Quota"),
    }
}

/// Папка у пользователя (`Mailbox/set`) → id.
async fn create_mailbox(u: &User, name: &str) -> String {
    let acc = u.client.mail_account().unwrap().to_string();
    let calls = [Invocation("Mailbox/set".into(), json!({ "accountId": acc, "create": { "y": { "name": name, "parentId": null } } }), "m".into())];
    let mut r = u.client.call(&[postman_jmap::CORE, postman_jmap::MAIL], &calls).await.unwrap();
    let v: Value = postman_jmap::take(&mut r, "m").unwrap();
    v["created"]["y"]["id"].as_str().unwrap_or_else(|| panic!("папка не создана: {v}")).to_string()
}

async fn destroy_mailbox(u: &User, id: &str) {
    let acc = u.client.mail_account().unwrap().to_string();
    let calls = [Invocation("Mailbox/set".into(), json!({ "accountId": acc, "destroy": [id], "onDestroyRemoveEmails": true }), "m".into())];
    let _ = u.client.call(&[postman_jmap::CORE, postman_jmap::MAIL], &calls).await;
}

fn saved_rules(replies: &[Reply]) -> Result<RulesState, String> {
    match replies.first() {
        Some(Reply::RulesSaved(s)) => Ok(s.clone()),
        Some(Reply::RulesInvalid(e)) => Err(format!("сервер не принял скрипт: {e}")),
        Some(Reply::RulesConflict { name, .. }) => Err(format!("чужой скрипт {name}")),
        Some(Reply::Failed { error, .. }) => Err(error.clone()),
        _ => Err("нет ответа RulesSaved".into()),
    }
}

/// S4-07: правило «тема содержит X → папка Y» срабатывает на новом письме; вместе с ним работает автоответ
/// (Stalwart держит активным один скрипт — автоответ подключён к правилам). Уборка: правила, папка, письма.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn rule_files_by_subject_with_vacation() {
    let (a, b) = (user(1).await, user(2).await);
    let stamp = stamp();
    let folder = format!("Postman rules {stamp}");
    let folder_id = create_mailbox(&b, &folder).await;
    let marker = format!("rule-{stamp}");
    let reply_subject = format!("Postman vacation {stamp}");
    let vacation_before = <VacationResponse as postman_backend_jmap::convert::FromJmap<_>>::from_jmap(b.client.vacation().await.unwrap());

    let result = async {
        let rule = Rule {
            enabled: true,
            field: Field::Subject,
            value: marker.clone(),
            action: Action::Move { mailbox_id: folder_id.clone(), name: folder.clone() },
        };
        // Прошлые прогоны (и чужой скрипт с нашим именем) — заменить: настройки user2 — тестовые.
        let state = saved_rules(&settings(&b.client, Request::SaveRules { rules: vec![rule.clone()], replace: true }).await)?;
        if !(state.active && state.rules == vec![rule.clone()]) {
            return Err(format!("правила на сервере не те: {state:?}"));
        }
        // Автоответ при активных правилах: включён (через include), правила остались активными.
        let on = VacationResponse {
            is_enabled: true,
            subject: Some(reply_subject.clone()),
            text_body: Some("Нет на месте (правила и автоответ).".into()),
            ..Default::default()
        };
        let replies = settings(&b.client, Request::SetVacation(on)).await;
        match replies.as_slice() {
            [Reply::VacationSaved(v), Reply::Rules(s)] if v.is_enabled && s.active => {}
            [Reply::Failed { error, .. }] => return Err(format!("автоответ: {error}")),
            _ => return Err("автоответ при правилах: не VacationSaved(вкл) + Rules(активны)".into()),
        }
        send(&a, &b.email, &format!("Postman {marker}"), "Проверка правила.").await;
        let filed = eventually(|| async {
            let page = b.client.search(&marker, Some(&folder_id), 5).await.ok()?;
            page.emails.into_iter().find(|e| e.mailbox_ids.get(&folder_id) == Some(&true))
        })
        .await;
        if filed.is_none() {
            return Err(format!("письмо не попало в папку «{folder}» за {DELIVERY:?}"));
        }
        let replied =
            eventually(|| async { find(&a, &reply_subject).await.into_iter().find(|e| e.subject.as_deref() == Some(reply_subject.as_str())) }).await;
        replied.map(|_| ()).ok_or_else(|| "автоответ при правилах не пришёл".to_string())
    }
    .await;

    // Уборка — и при сбое.
    let off = VacationResponse { is_enabled: false, ..vacation_before };
    settings(&b.client, Request::SetVacation(off)).await;
    let cleared = saved_rules(&settings(&b.client, Request::SaveRules { rules: vec![], replace: true }).await);
    destroy_all(&a, &stamp).await;
    destroy_all(&b, &stamp).await;
    destroy_mailbox(&b, &folder_id).await;
    result.unwrap();
    let cleared = cleared.unwrap();
    assert!(cleared.rules.is_empty() && !cleared.active, "правила не убраны: {cleared:?}");
    let scripts = b.client.sieve_scripts().await.unwrap();
    assert!(!scripts.iter().any(|s| s.name.as_deref() == Some("postman")), "скрипт postman остался: {scripts:?}");
    assert!(!b.client.vacation().await.unwrap().is_enabled);
}

/// S5-26: правило «тема содержит X → категория» ставит новому письму ключевое слово категории (`addflag`,
/// `imap4flags`); письмо остаётся во «Входящих». Уборка: правила, письма.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn sieve_rule_sets_category() {
    let (a, b) = (user(1).await, user(2).await);
    let stamp = stamp();
    let marker = format!("category-{stamp}");
    let keyword = postman_core::labels::category_keyword("tenders");
    let result = async {
        let rule = Rule { enabled: true, field: Field::Subject, value: marker.clone(), action: Action::Category(keyword.clone()) };
        let state = saved_rules(&settings(&b.client, Request::SaveRules { rules: vec![rule.clone()], replace: true }).await)?;
        if !(state.active && state.rules == vec![rule]) {
            return Err(format!("правила на сервере не те: {state:?}"));
        }
        send(&a, &b.email, &format!("Postman {marker}"), "Проверка категории.").await;
        let got = eventually(|| async { find(&b, &marker).await.into_iter().find(|e| e.keywords.get(&keyword) == Some(&true)) }).await;
        got.map(|_| ()).ok_or_else(|| format!("письмо не получило слово {keyword} за {DELIVERY:?}"))
    }
    .await;
    let cleared = saved_rules(&settings(&b.client, Request::SaveRules { rules: vec![], replace: true }).await);
    destroy_all(&a, &stamp).await;
    destroy_all(&b, &stamp).await;
    result.unwrap();
    assert!(cleared.unwrap().rules.is_empty(), "правила не убраны");
}

/// S4-07: скрипт с ошибкой сервер не принимает (`SieveScript/validate`), на сервере ничего не меняется.
#[tokio::test]
#[ignore = "нужен стенд Stalwart: docs/STALWART.md"]
async fn invalid_script_is_rejected_by_validate() {
    let b = user(2).await;
    let before = b.client.sieve_scripts().await.unwrap();
    // Наш генератор пишет только верный Sieve; проверка сервера — тем же вызовом, что и при сохранении правил.
    let saved = b.client.save_sieve(None, "postman-invalid", "if true {\n", Activate::Keep).await;
    match saved {
        Err(SaveError::Invalid(e)) => assert!(e.contains("line"), "сообщение без места ошибки: {e}"),
        Ok(id) => {
            let _ = b.client.destroy_sieve(&id).await;
            panic!("сервер принял неверный скрипт");
        }
        Err(e) => panic!("{e:?}"),
    }
    assert_eq!(b.client.sieve_scripts().await.unwrap(), before);
}
