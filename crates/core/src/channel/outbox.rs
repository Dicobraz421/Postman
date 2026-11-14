//! Очередь отправки каналов (`chan_outbox`, ADR 0012 п. 11): «Отправить» — строка очереди и сообщение «отправляется»
//! в ленте; окно «Отменить» (CMF-03, задержка `crate::send_later::undo_release`) держит ядро — у чатов сервер его не
//! держит; «Отправить позже» — сервер при `scheduled_send` (Telegram `schedule_date`), иначе ядро до срока, пока
//! Postman запущен. Закрыли Postman в окне — исходящее уходит сразу при выходе ([`leave`], как IMAP и Exchange); был
//! закрыт до срока — уходит при следующем запуске. Без связи — ждёт в очереди; повтор — с тем же `client_id`, без
//! дублей. Поток учётки — `super::run`, хранение — `ChannelStore::queue`.

use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};

use super::{Attachment, Body, ChannelError, ChannelFeatures, Message, MessageId, Outgoing, ParticipantId};
use crate::model::Release;

/// Исходящее в очереди: что отправить, когда (Unix, мс) и шаг.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Queued {
    pub outgoing: Outgoing,
    pub due: i64,
    pub step: Step,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Step {
    /// Ждёт срока (окно «Отменить», «позже») или связи.
    Held,
    /// Сервер отказал без повтора («Проблема → причина → исправление»): ждёт «Отменить» или повтора пользователем.
    Failed(ChannelError),
}

/// Префикс id сообщения «отправляется»: у сервера таких id нет.
const PENDING: &str = "~out";

/// id сообщения «отправляется» исходящего `client_id`.
pub fn pending_id(client_id: u64) -> MessageId {
    MessageId(format!("{PENDING}{client_id}"))
}

/// Сообщение ленты — исходящее, ещё не принятое сервером (показ «отправляется»).
pub fn is_pending(id: &MessageId) -> bool {
    id.0.starts_with(PENDING)
}

/// Исходящее как сообщение ленты: моё, тело — IR, вложения — выгруженные.
pub fn message(o: &Outgoing, id: MessageId, author: ParticipantId, sent: i64) -> Message {
    Message {
        id,
        author,
        sent,
        body: Body::Ir(o.body.clone()),
        attachments: o
            .attachments
            .iter()
            .map(|u| Attachment {
                name: u.name.clone(),
                mime: u.mime.clone(),
                size: Some(u.size),
                kind: u.kind.clone(),
                source: u.source.clone(),
                thumb: None,
            })
            .collect(),
        reply_to: o.reply_to.clone(),
        quotes: o.quotes.clone(),
        mentions: o.mentions.clone(),
        mine: true,
        ..Message::default()
    }
}

/// Когда отправить и что сказать каналу по выбору пользователя `release` в момент `now` (Unix, мс):
/// `Now` — сразу; `After(с)` — окно «Отменить» держит ядро; `At(с)` — сервер при `scheduled_send` (уходит сразу с
/// `at`), иначе ядро держит до срока.
pub fn plan(mut o: Outgoing, release: Release, features: &ChannelFeatures, now: i64) -> Queued {
    let due = match release {
        Release::Now => now,
        Release::After(secs) => now.saturating_add(i64::from(secs) * 1000),
        Release::At(at) if features.scheduled_send => {
            o.at = Some(at);
            now
        }
        Release::At(at) => {
            o.at = None;
            at.saturating_mul(1000).max(now)
        }
    };
    Queued { outgoing: o, due, step: Step::Held }
}

/// Сколько исходящих держат потоки каналов сейчас (окно «Отменить», «позже», нет связи).
static HOLDING: AtomicUsize = AtomicUsize::new(0);
static LEAVING: AtomicBool = AtomicBool::new(false);
static LEAVE: LazyLock<tokio::sync::Notify> = LazyLock::new(tokio::sync::Notify::new);

/// Исходящих в окне «Отменить» и до срока у всех учёток каналов: выход их дожидается ([`leave`]).
pub fn holding() -> usize {
    HOLDING.load(Ordering::Relaxed)
}

/// Выход: исходящие в окне уходят сейчас, «позже» — остаются в очереди до следующего запуска.
pub fn leave() {
    LEAVING.store(true, Ordering::Relaxed);
    LEAVE.notify_waiters();
}

pub(super) fn leaving() -> bool {
    LEAVING.load(Ordering::Relaxed)
}

/// Ждать выхода ([`leave`]).
pub(super) async fn left() {
    let wait = LEAVE.notified();
    if leaving() {
        return;
    }
    wait.await;
}

/// Счётчик [`holding`] этого потока: изменения — в общий, при конце потока — вычитается.
#[derive(Default)]
pub(super) struct Held(usize);

impl Held {
    pub(super) fn set(&mut self, n: usize) {
        if n > self.0 {
            HOLDING.fetch_add(n - self.0, Ordering::Relaxed);
        } else {
            HOLDING.fetch_sub(self.0 - n, Ordering::Relaxed);
        }
        self.0 = n;
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.set(0);
    }
}

/// Окно «Отменить» в выходе: исходящее со сроком не позже `now + window` (мс) уходит сейчас; «позже» — ждёт.
pub fn goes_on_leave(q: &Queued, now: i64, window_ms: i64) -> bool {
    q.step == Step::Held && q.due <= now.saturating_add(window_ms)
}

/// Самое длинное окно «Отменить» (CMF-03): до него — «уходит при выходе», дальше — «позже».
pub const UNDO_MAX_MS: i64 = 60_000;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::ConversationId;

    fn out() -> Outgoing {
        Outgoing {
            conversation: ConversationId::new("c"),
            client_id: 7,
            body: crate::channel::text_doc("привет"),
            attachments: vec![],
            reply_to: None,
            quotes: vec![],
            mentions: vec![],
            at: None,
            silent: false,
        }
    }

    /// Окно «Отменить» держит ядро; «позже» — сервер при `scheduled_send`, иначе ядро до срока; при выходе уходит
    /// окно, «позже» — нет.
    #[test]
    fn release_plans() {
        let now = 1_000_000;
        let no = ChannelFeatures::default();
        let server = ChannelFeatures { scheduled_send: true, ..ChannelFeatures::default() };
        assert_eq!(plan(out(), Release::Now, &no, now).due, now);
        let undo = plan(out(), Release::After(10), &no, now);
        assert_eq!(undo.due, now + 10_000);
        let later = plan(out(), Release::At(5_000), &no, now);
        assert_eq!((later.due, later.outgoing.at), (5_000_000, None));
        let by_server = plan(out(), Release::At(5_000), &server, now);
        assert_eq!((by_server.due, by_server.outgoing.at), (now, Some(5_000)));
        assert!(goes_on_leave(&undo, now, UNDO_MAX_MS) && !goes_on_leave(&later, now, UNDO_MAX_MS));
        assert!(is_pending(&pending_id(7)) && !is_pending(&MessageId::new("7")));
        let m = message(&out(), pending_id(7), ParticipantId::new("me"), now);
        assert!(m.mine && m.body.plain() == "привет");
    }
}
