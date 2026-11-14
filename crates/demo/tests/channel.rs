//! Демо-канал проходит проверки договора каналов (ADR 0012, `postman_core::channel::conformance`): со всеми
//! возможностями и без части — «нет у сервера — нет на экране».

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use postman_core::channel::conformance::{Harness, Script, run_all};
use postman_core::channel::{AccountId, Answer, ChannelFeatures, ChannelPlugin, Limit, Reactions, Secret};
use postman_demo::channel::{ACCOUNT, CODE, DemoChannel, DemoServer, PASSWORD, SESSION, all_features};

fn harness(server: DemoServer) -> Harness {
    let script: Rc<dyn Script> = Rc::new(server.clone());
    Harness {
        plugin: Arc::new(DemoChannel::new(server)),
        account: AccountId::new(ACCOUNT),
        secret: Secret::new(SESSION.to_vec()),
        sign_in: Some(vec![Answer::Text("+70000000000".into()), Answer::Text(CODE.into()), Answer::Secret(Secret::new(PASSWORD.to_vec()))]),
        script: Some(script),
        wait: Duration::from_secs(5),
    }
}

#[test]
fn demo_channel_passes_conformance() {
    let report = run_all(&harness(DemoServer::new()));
    assert!(report.ok(), "{report}");
    // Пропуск — только проверка отказов: всё умеющему каналу отказывать не в чем.
    assert_eq!(report.skipped, ["features_are_honest"], "всё умеющий канал проходит все проверки: {report}");
    assert!(report.passed.len() >= 24, "{report}");
}

/// Канал без реакций, правки, удаления у всех и «печатает»: отказы `Unsupported`, проверки этих умений пропущены.
#[test]
fn limited_demo_channel_is_honest() {
    let f =
        ChannelFeatures { edit: Limit::No, delete_for_all: Limit::No, reactions: Reactions::No, typing: false, server_mute: false, ..all_features() };
    let server = DemoServer::with_features(f.clone());
    let mut h = harness(server);
    h.script = None;
    assert_eq!(h.plugin.features(), f);
    let report = run_all(&h);
    assert!(report.ok(), "{report}");
    assert!(report.passed.contains(&"features_are_honest"), "{report}");
    for skipped in ["edit", "react"] {
        assert!(report.skipped.contains(&skipped), "{skipped}: {report}");
    }
}

/// Ссылка канала → адрес Postman; чужая — нет.
#[test]
fn demo_links() {
    let p = DemoChannel::default();
    let a = p.parse_link("https://chat.example.test/g-team/m7").unwrap();
    assert_eq!(a.to_string(), format!("postman://demo/{ACCOUNT}/g-team/m7"));
    assert!(p.parse_link("https://chat.example.test/nope/m1").is_none());
    assert!(p.parse_link("https://t.me/c/1/2").is_none());
    assert_eq!(p.link(&a).as_deref(), Some("https://chat.example.test/g-team/m7"), "наружу — ссылка канала");
}

/// Кросс-ссылки через плагин (CH-02в): ссылка канала в письме — обратная ссылка у сообщения; «Связать с контактом» —
/// ссылка на человека в карточку на сервере; связь — у того же человека в поиске `от:`.
#[test]
fn demo_links_and_people() {
    use postman_cache::Cache;
    use postman_core::channel::person::{self, Contact};
    use postman_core::channel::{Address, ChannelEvent, ChannelId, ChannelStore, ConversationId, Links, MessageId, Participant, ParticipantId, xref};
    use postman_core::model::{ContactCard, LetterId};
    use postman_core::sync::Command;

    let links = Links::new(vec![Arc::new(DemoChannel::new(DemoServer::new()))]);
    let cache = Cache::open_in_memory().unwrap();
    let msg = Address::message(&ChannelId::new("demo"), &AccountId::new(ACCOUNT), &ConversationId::new("g-team"), &MessageId::new("m7"));
    assert_eq!(links.outside(&msg), "https://chat.example.test/g-team/m7");
    xref::index_letter(&cache, &links, LetterId(1), "Обсуждали тут: https://chat.example.test/g-team/m7.").unwrap();
    assert_eq!(cache.xref_sources(&xref::target_key(&msg)).unwrap(), ["letter:1"]);

    let store = ChannelStore::open(cache.clone(), &ChannelId::new("demo"), &AccountId::new(ACCOUNT), "Я").unwrap();
    let anna = Participant { id: ParticipantId::new("u-anna"), name: "Анна".into(), handle: Some("anna".into()), ..Participant::default() };
    store.apply(vec![ChannelEvent::People(vec![anna.clone()])]).unwrap();
    let card = ContactCard { id: "card-anna".into(), name: "Анна Смирнова".into(), ..ContactCard::default() };
    let cmd = person::link(&cache, &links, &store.chan(), &anna, Contact::Personal(&card), 7).unwrap();
    let Some(Command::SaveContact { card: new, old: Some(_), ticket: 7 }) = cmd else { panic!("нет записи в карточку") };
    assert_eq!(new.services, ["https://chat.example.test/@anna"]);
    // На другом компьютере — та же связь по адресу в карточке, автоматически.
    let other = Cache::open_in_memory().unwrap();
    ChannelStore::open(other.clone(), &ChannelId::new("demo"), &AccountId::new(ACCOUNT), "Я")
        .unwrap()
        .apply(vec![ChannelEvent::People(vec![anna.clone()])])
        .unwrap();
    assert_eq!(person::auto(&other, &links, &[*new], &[]).unwrap(), 1);
    assert_eq!(person::get(&other, &store.chan(), &anna.id).unwrap().unwrap().how, person::How::Service);
    assert!(person::link(&cache, &links, &store.chan(), &anna, Contact::Directory("gal-anna"), 8).unwrap().is_none(), "справочник — только база");
}

/// Очередь отправки (ADR 0012 п. 11) на демо-канале через поток учётки: окно «Отменить» — «отправляется» в ленте,
/// «Отменить» снимает его; по сроку — сообщение сервера вместо «отправляется»; очередь пуста.
#[test]
fn outbox_undo_and_send() {
    use postman_core::channel::{ChannelId, ChannelStore, MemorySecrets, Outgoing, Request, Secrets, Start, Update, outbox, spawn, text_doc};
    use postman_core::model::Release;
    use postman_demo::channel::DIRECT;

    let cache = postman_cache::Cache::open_in_memory().unwrap();
    let (id, account) = (ChannelId::new(postman_demo::channel::ID), AccountId::new(ACCOUNT));
    let secrets = Arc::new(MemorySecrets::default());
    secrets.put(&id, &account, &Secret::new(SESSION.to_vec())).unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<Update>();
    let start = Start { cache: cache.clone(), secrets, account: account.clone(), name: "Я".into(), render: None, index: None };
    let requests = spawn(
        Arc::new(DemoChannel::new(DemoServer::new())),
        start,
        Box::new(move |u| {
            let _ = tx.send(u);
        }),
    );
    let wait = |what: &str, f: &dyn Fn(&Update) -> bool| -> Update {
        let until = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let left = until.saturating_duration_since(std::time::Instant::now());
            match rx.recv_timeout(left) {
                Ok(u) if f(&u) => return u,
                Ok(_) => {}
                Err(e) => panic!("{what}: {e}"),
            }
        }
    };
    wait("connected", &|u| matches!(u, Update::Connected { .. }));
    let store = ChannelStore::find(&cache, &id, &account).unwrap().unwrap();
    let conv = postman_core::channel::ConversationId::new(DIRECT);
    let out = |client_id| Outgoing {
        conversation: conv.clone(),
        client_id,
        body: text_doc("через очередь"),
        attachments: vec![],
        reply_to: None,
        quotes: vec![],
        mentions: vec![],
        at: None,
        silent: false,
    };
    let pending = |c| store.message(&conv, &outbox::pending_id(c)).unwrap().is_some();

    requests.send(Request::Send { outgoing: out(900), release: Release::After(30) }).unwrap();
    wait("queued", &|u| matches!(u, Update::Queued { client_id: 900, .. }));
    assert!(pending(900), "«отправляется» в ленте");
    assert_eq!(outbox::holding(), 1, "окно «Отменить» держит выход");
    requests.send(Request::Unsend { client_id: 900 }).unwrap();
    wait("unsent", &|u| matches!(u, Update::Unsent { client_id: 900, done: true }));
    assert!(!pending(900) && store.queued().unwrap().is_empty());

    requests.send(Request::Send { outgoing: out(901), release: Release::After(1) }).unwrap();
    let Update::Sent { id: sent, .. } = wait("sent", &|u| matches!(u, Update::Sent { client_id: 901, .. } | Update::SendFailed { .. })) else {
        panic!("not sent");
    };
    assert!(!pending(901) && store.message(&conv, &sent).unwrap().is_some(), "сообщение сервера вместо «отправляется»");
    assert!(store.queued().unwrap().is_empty());
    assert_eq!(outbox::holding(), 0);
    drop(requests);
}
