//! Канал Telegram проходит проверки договора каналов (ADR 0012, `postman_core::channel::conformance`) на подставном
//! сервере (`postman_backend_telegram::fake`): те же байты TL, без сети. Плюс то, чего общий набор не видит: вход по
//! QR, неверный код и пароль, догон от курсора после пропуска, просроченная ссылка на файл, папки и архив.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use postman_backend_telegram::fake::{self, FakeTelegram};
use postman_backend_telegram::{BAD_CODE, BAD_PASSWORD, BAD_PHONE, TelegramChannel};
use postman_cache::Cache;
use postman_core::channel::conformance::{Harness, Script, run_all};
use postman_core::channel::{
    AccountId, Answer, ChannelError, ChannelEvent, ChannelId, ChannelPlugin, ChannelSession, ConnectCtx, ConversationId, FlowCtx, LocalStore, Prompt,
    Secret,
};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

fn harness(server: FakeTelegram) -> Harness {
    let script: Rc<dyn Script> = Rc::new(server.clone());
    Harness {
        plugin: Arc::new(TelegramChannel::with_dial(Arc::new(server))),
        account: AccountId::new(fake::ACCOUNT),
        secret: fake::session(),
        sign_in: Some(vec![
            Answer::Text(fake::PHONE.into()),
            Answer::Text(fake::CODE.into()),
            Answer::Secret(Secret::new(fake::PASSWORD.as_bytes().to_vec())),
        ]),
        script: Some(script),
        wait: Duration::from_secs(5),
    }
}

#[test]
fn telegram_passes_conformance() {
    let report = run_all(&harness(FakeTelegram::new()));
    assert!(report.ok(), "{report}");
    // Telegram умеет всё, что проверяется отказом: пропуск — только «честность возможностей».
    assert_eq!(report.skipped, ["features_are_honest"], "{report}");
    assert!(report.passed.len() >= 24, "{report}");
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

fn store(cache: &Cache) -> Rc<dyn postman_core::channel::PluginStore> {
    Rc::new(LocalStore::new(cache.clone(), &ChannelId::new("telegram"), &AccountId::new(fake::ACCOUNT)))
}

/// Вход: неверный телефон, код, пароль — отказ с кодом, шаг тот же; затем вход; QR — с другого устройства.
#[test]
fn sign_in_errors_and_qr() {
    let server = FakeTelegram::new();
    let plugin = TelegramChannel::with_dial(Arc::new(server.clone()));
    let cache = Cache::open_in_memory().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt(), async {
        let mut flow = plugin.sign_in(FlowCtx { store: store(&cache) }).await.unwrap();
        assert_eq!(flow.step(None).await.unwrap(), Prompt::Phone);
        assert!(matches!(flow.step(Some(Answer::Text("+999 0".into()))).await, Err(ChannelError::Denied(c)) if c == BAD_PHONE));
        assert!(matches!(flow.step(Some(Answer::Text(fake::PHONE.into()))).await.unwrap(), Prompt::Code { length: Some(5), .. }));
        assert!(matches!(flow.step(Some(Answer::Text("00000".into()))).await, Err(ChannelError::Denied(c)) if c == BAD_CODE));
        assert!(matches!(flow.step(Some(Answer::Resend)).await.unwrap(), Prompt::Code { .. }));
        let p = flow.step(Some(Answer::Text(fake::CODE.into()))).await.unwrap();
        assert_eq!(p, Prompt::Password { hint: Some("подсказка".into()) });
        assert!(matches!(flow.step(Some(Answer::Text("не тот".into()))).await, Err(ChannelError::Denied(c)) if c == BAD_PASSWORD));
        let Prompt::Done { account, secret, name } = flow.step(Some(Answer::Text(fake::PASSWORD.into()))).await.unwrap() else { panic!() };
        assert_eq!((account.as_str(), name.as_str()), (fake::ACCOUNT, "Иван Тестов"));
        assert!(!format!("{secret:?}").contains("171"), "секрет без значения в журнале");
        drop(flow);

        // QR: ссылка tg://login, срок; не отсканирован — тот же QR; отсканирован — вход.
        let mut flow = plugin.sign_in(FlowCtx { store: store(&cache) }).await.unwrap();
        flow.step(None).await.unwrap();
        let Prompt::Qr { url, expires } = flow.step(Some(Answer::Confirm(true))).await.unwrap() else { panic!() };
        assert!(url.starts_with("tg://login?token=") && expires > 0, "{url}");
        server.scan_qr();
        let done = flow.step(Some(Answer::Confirm(true))).await.unwrap();
        assert!(matches!(done, Prompt::Done { ref account, .. } if account.as_str() == fake::ACCOUNT), "{done:?}");
    });
}

struct Conn {
    session: Box<dyn ChannelSession>,
    rx: UnboundedReceiver<ChannelEvent>,
}

async fn connect(plugin: &TelegramChannel, cache: &Cache) -> Conn {
    let (tx, rx) = unbounded_channel();
    let ctx = ConnectCtx { account: AccountId::new(fake::ACCOUNT), secret: fake::session(), incoming: tx, store: store(cache) };
    Conn { session: plugin.connect(ctx).await.unwrap(), rx }
}

async fn until(rx: &mut UnboundedReceiver<ChannelEvent>, mut f: impl FnMut(&ChannelEvent) -> bool) -> Vec<ChannelEvent> {
    let mut out = Vec::new();
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.expect("event in time").expect("events open");
        let stop = f(&ev);
        out.push(ev);
        if stop {
            return out;
        }
    }
}

/// Догон: беседы с ролями, счётчиками, закреплением; папка «Работа» и архив; упоминание меня и ответ с цитатой; от
/// курсора — пропущенное пришло; живое сообщение — `live`, счётчик растёт.
#[test]
fn catch_up_folders_and_live() {
    let server = FakeTelegram::new();
    let plugin = TelegramChannel::with_dial(Arc::new(server.clone()));
    let cache = Cache::open_in_memory().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt(), async {
        let mut c = connect(&plugin, &cache).await;
        c.session.catch_up(None).await.unwrap();
        let evs = until(&mut c.rx, |e| matches!(e, ChannelEvent::Cursor(_))).await;
        let convs: Vec<_> =
            evs.iter().filter_map(|e| if let ChannelEvent::Conversations(cs) = e { Some(cs.clone()) } else { None }).flatten().collect();
        let team = convs.iter().find(|c| c.id.0 == format!("c{}", fake::TEAM)).unwrap();
        assert!(team.pinned && team.unread.unwrap().mentions == 1);
        assert!(team.folders.iter().any(|f| f.0 == "f2"), "папка «Работа»: {:?}", team.folders);
        let news = convs.iter().find(|c| c.id.0 == format!("c{}", fake::NEWS)).unwrap();
        assert_eq!(news.kind, postman_core::channel::ConversationKind::Broadcast);
        assert!(!news.my_role.can_write());
        assert!(news.folders.iter().any(|f| f.0 == "archive"));
        let anna = convs.iter().find(|c| c.id.0 == format!("u{}", fake::ANNA)).unwrap();
        assert!(anna.folders.iter().any(|f| f.0 == "f2"), "контакт — в папке по правилу");
        let folders = evs.iter().find_map(|e| if let ChannelEvent::Folders(f) = e { Some(f.clone()) } else { None }).unwrap();
        assert!(folders.iter().any(|f| f.name == "Работа") && folders.iter().any(|f| f.id.0 == "archive"));
        let msgs: Vec<_> = evs
            .iter()
            .filter_map(|e| {
                if let ChannelEvent::Messages { conversation, messages, .. } = e { Some((conversation.clone(), messages.clone())) } else { None }
            })
            .collect();
        let team_msgs = &msgs.iter().find(|(c, _)| *c == team.id).unwrap().1;
        assert!(team_msgs.iter().any(|m| m.mentions.iter().any(|p| p.0 == fake::ACCOUNT)), "упоминание меня");
        assert!(team_msgs.iter().any(|m| m.reply_to.is_some() && m.quotes.first().is_some_and(|q| q.text == "посмотри задачу")));
        let Some(ChannelEvent::Cursor(cursor)) = evs.last().cloned() else { panic!() };

        // Живое сообщение: `live`, непрочитанное +1.
        server.anna_writes("Ты тут?");
        let evs = until(&mut c.rx, |e| matches!(e, ChannelEvent::Conversations(_))).await;
        assert!(evs.iter().any(|e| matches!(e, ChannelEvent::Messages { live: true, messages, .. } if messages[0].body.plain() == "Ты тут?")));
        let ChannelEvent::Conversations(cs) = evs.last().unwrap() else { panic!() };
        assert_eq!(cs[0].unread.unwrap().unread, anna.unread.unwrap().unread + 1);
        drop(c);

        // Без связи написали ещё — новое подключение от курсора догоняет пропущенное.
        server.anna_writes("Пока тебя не было");
        let mut again = connect(&plugin, &cache).await;
        again.session.catch_up(Some(cursor)).await.unwrap();
        let evs = until(
            &mut again.rx,
            |e| matches!(e, ChannelEvent::Messages { live: true, messages, .. } if messages[0].body.plain() == "Пока тебя не было"),
        )
        .await;
        assert!(!evs.is_empty());
    });
}

struct Sink(Vec<u8>);

impl postman_core::channel::ByteSink for Sink {
    fn write(&mut self, chunk: &[u8]) -> Result<(), ChannelError> {
        self.0.extend_from_slice(chunk);
        Ok(())
    }
}

/// Ссылка на файл устарела — канал перечитывает сообщение и качает по свежей.
#[test]
fn expired_file_reference_is_refreshed() {
    let server = FakeTelegram::new();
    let plugin = TelegramChannel::with_dial(Arc::new(server.clone()));
    let cache = Cache::open_in_memory().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt(), async {
        let mut c = connect(&plugin, &cache).await;
        c.session.catch_up(None).await.unwrap();
        let evs = until(&mut c.rx, |e| matches!(e, ChannelEvent::Cursor(_))).await;
        let att = evs
            .iter()
            .find_map(|e| match e {
                ChannelEvent::Messages { messages, .. } => messages.iter().find_map(|m| m.attachments.first().cloned()),
                _ => None,
            })
            .unwrap();
        assert_eq!(att.name, "отчёт.pdf");
        server.expire_file_refs();
        let mut sink = Sink(Vec::new());
        c.session.download(&att.source, &mut sink).await.unwrap();
        assert_eq!(sink.0.len() as u64, att.size.unwrap());
        // Неизвестная беседа — не найдена, без запроса к серверу.
        let r = c.session.history(&ConversationId::new("u42"), None, 5).await;
        assert!(matches!(r, Err(ChannelError::NotFound)), "{r:?}");
    });
}
