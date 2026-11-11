//! Без связи (S5-08): сервер не отвечает — синхронизация не останавливается, а сообщает «нет связи» со сроком
//! повтора; правки писем копятся в очереди; «Повторить сейчас» — попытка сразу, пауза растёт. Сервер не нужен:
//! адрес, где никто не слушает (соединение отклоняется сразу).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use postman_core::link::Connection;
use postman_core::sync::{Account, Command, ErrorKind, Event, SendReceive};

const WAIT: Duration = Duration::from_secs(20);

fn next_connection(rx: &Receiver<Event>) -> (Connection, Vec<ErrorKind>) {
    let start = Instant::now();
    let mut errors = Vec::new();
    loop {
        let left = WAIT.checked_sub(start.elapsed()).unwrap_or_else(|| panic!("нет события связи"));
        match rx.recv_timeout(left) {
            Ok(Event::Connection(c)) => return (c, errors),
            Ok(Event::Error(e)) => errors.push(e.kind),
            Ok(_) => {}
            Err(e) => panic!("нет события связи: {e}"),
        }
    }
}

#[test]
fn unreachable_server_goes_offline_queues_and_retries_on_demand() {
    let account = Account { url: "http://127.0.0.1:9".into(), auth: postman_core::model::Auth::Basic { user: "u".into(), password: "p".into() } };
    let (tx, rx) = std::sync::mpsc::channel();
    let commands = postman_backend_jmap::spawn(account, postman_cache::Cache::open_in_memory().unwrap(), None, tx, || {});

    // Вход не удался по сети — без связи, повтор через 5 с (а не конец синхронизации).
    let (c, errors) = next_connection(&rx);
    assert_eq!(c, Connection::Offline { retry_in: 5, queued: 0 });
    assert_eq!(errors, [ErrorKind::Network]);

    // Правка письма без связи — в очередь.
    let flag = Command::Keyword { id: "m1".into(), keyword: "$flagged", on: true };
    assert!(commands.send(flag).is_ok());
    let (c, _) = next_connection(&rx);
    assert!(matches!(c, Connection::Offline { queued: 1, .. }), "{c:?}");

    // «Повторить сейчас» — попытка сразу; снова нет связи — пауза вдвое длиннее, очередь цела.
    let asked = Instant::now();
    assert!(commands.send(Command::Reconnect).is_ok());
    let (c, errors) = next_connection(&rx);
    assert_eq!(c, Connection::Offline { retry_in: 10, queued: 1 });
    assert_eq!(errors, [ErrorKind::Network]);
    assert!(asked.elapsed() < Duration::from_secs(4), "повтор не ждал паузы");

    // Интерфейс закрыт — синхронизация заканчивается.
    drop(commands);
    let start = Instant::now();
    loop {
        match rx.recv_timeout(WAIT) {
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(e) => panic!("синхронизация не закончилась: {e}"),
            Ok(_) => assert!(start.elapsed() < WAIT),
        }
    }
}

/// «Отправить и получить» без связи (T3-02): попытка входа сразу, ход — начало и конец с ошибкой.
#[test]
fn send_receive_offline_ends_with_an_error() {
    let account = Account { url: "http://127.0.0.1:9".into(), auth: postman_core::model::Auth::Basic { user: "u".into(), password: "p".into() } };
    let (tx, rx) = std::sync::mpsc::channel();
    let commands = postman_backend_jmap::spawn(account, postman_cache::Cache::open_in_memory().unwrap(), None, tx, || {});
    next_connection(&rx);
    assert!(commands.send(Command::SyncNow).is_ok());
    let mut progress = Vec::new();
    let start = Instant::now();
    while progress.len() < 2 {
        let left = WAIT.checked_sub(start.elapsed()).unwrap_or_else(|| panic!("нет хода: {progress:?}"));
        if let Event::SendReceive(p) = rx.recv_timeout(left).unwrap() {
            progress.push(p);
        }
    }
    assert_eq!(progress[0], SendReceive::Started);
    assert!(matches!(&progress[1], SendReceive::Finished { error: Some(_) }), "{progress:?}");
}
