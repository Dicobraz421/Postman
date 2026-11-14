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
}
