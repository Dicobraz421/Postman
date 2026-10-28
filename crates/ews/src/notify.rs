//! Уведомления EWS об изменениях (T1-21а): дерево папок и письма — по событиям сервера, а не опросом всего ящика.
//!
//! - **Подписка** — `Subscribe` на папки и корень ящика (или на все папки — `SubscribeToAllFolders`): события писем
//!   и папок `NewMail`, `Created`, `Deleted`, `Modified`, `Moved`, `Copied`.
//! - **Streaming** (Exchange 2010 SP1+) — `GetStreamingEvents`: сервер держит ответ открытым до [`STREAM_MINUTES`]
//!   минут и пишет события кусками (SOAP-конверт на кусок), затем `ConnectionStatus` `Closed` — открыть заново по той
//!   же подписке. Соединение потока — своё, отдельно от запросов клиента ([`EventStream`]).
//! - **Pull** — запасной путь: `GetEvents` с водяным знаком (`Watermark`) раз в [`Notifier::pull_every`].
//! - **Обрыв** (`ErrorSubscriptionNotFound`, `ErrorExpiredSubscription`…) — [`Error::is_subscription_lost`]:
//!   переподписаться и сверить всё ([`Change::Resync`]).
//!
//! Итог для плагина — [`Change`]: письма папки, свойства папки, дерево целиком. [`Notifier`] собирает это в один
//! цикл: подписка, поток, переподписка, запасной pull.

use std::time::{Duration, Instant};

use crate::soap::{self, text_el};
use crate::xml::{self, El};
use crate::{Client, Error, FolderRef, Result};

/// Сколько минут сервер держит поток (`ConnectionTimeout`, 1–30).
pub const STREAM_MINUTES: u32 = 30;
/// Срок pull-подписки без `GetEvents`, минут (`Timeout`, 1–1440).
pub const PULL_TIMEOUT_MINUTES: u32 = 30;
/// Кусок потока (один конверт) больше — ошибка.
const MAX_FRAME: usize = 4 << 20;
/// Запросов `GetEvents` подряд, пока сервер говорит `MoreEvents`.
const MORE_EVENTS: usize = 10;

/// События, на которые подписываемся: письма и папки (`FreeBusyChangedEvent` — не нужен).
const EVENT_TYPES: [&str; 6] = ["NewMailEvent", "CreatedEvent", "DeletedEvent", "ModifiedEvent", "MovedEvent", "CopiedEvent"];

/// Коды ошибок «подписки больше нет»: переподписаться и сверить всё.
const LOST: [&str; 6] = [
    "ErrorSubscriptionNotFound",
    "ErrorExpiredSubscription",
    "ErrorInvalidSubscription",
    "ErrorMissedNotificationEvents",
    "ErrorInvalidWatermark",
    "ErrorReadEventsFailed",
];

impl Error {
    /// Подписка потеряна (истекла, сервер её забыл, пропущены события): переподписаться и сверить всё.
    pub fn is_subscription_lost(&self) -> bool {
        self.code().is_some_and(|c| LOST.contains(&c))
    }
}

/// Вид события.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    NewMail,
    Created,
    Deleted,
    Modified,
    Moved,
    Copied,
    /// Служебное «всё в порядке» pull-подписки (только водяной знак).
    Status,
}

impl EventKind {
    fn parse(name: &str) -> Option<EventKind> {
        Some(match name {
            "NewMailEvent" => EventKind::NewMail,
            "CreatedEvent" => EventKind::Created,
            "DeletedEvent" => EventKind::Deleted,
            "ModifiedEvent" => EventKind::Modified,
            "MovedEvent" => EventKind::Moved,
            "CopiedEvent" => EventKind::Copied,
            "StatusEvent" => EventKind::Status,
            _ => return None,
        })
    }
}

/// Событие сервера: письмо (`item`) или папка (`folder`), её родитель; у переноса и копии — прежний родитель.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub kind: EventKind,
    /// Водяной знак события (pull); у streaming его нет.
    pub watermark: Option<String>,
    /// Id письма (`ItemId`) — событие письма.
    pub item: Option<String>,
    /// Id папки (`FolderId`) — событие папки.
    pub folder: Option<String>,
    /// Папка письма или родитель папки (`ParentFolderId`).
    pub parent: Option<String>,
    /// Прежняя папка письма или прежний родитель папки (`OldParentFolderId`, у `Moved` и `Copied`).
    pub old_parent: Option<String>,
}

/// Что изменилось — для плагина.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Change {
    /// Письма папки (по `FolderId`): новое, изменено, удалено, перенесено в неё или из неё — синхронизировать её.
    Mail(String),
    /// Свойства папки: счётчики или имя (`ModifiedEvent` папки) — перечитать её одну.
    Folder(String),
    /// Дерево папок: папка создана, удалена, перенесена — перечитать дерево.
    Tree,
    /// Подписка потеряна и открыта заново: события между ними могли пропасть — сверить всё.
    Resync,
}

/// Изменения по событиям: без повторов, в порядке событий.
pub fn changes(events: &[Event]) -> Vec<Change> {
    let mut out: Vec<Change> = Vec::new();
    let mut push = |c: Change| {
        if !out.contains(&c) {
            out.push(c);
        }
    };
    for e in events {
        match (e.kind, &e.item, &e.folder) {
            (EventKind::Status, _, _) => {}
            (_, Some(_), _) => {
                for p in [&e.parent, &e.old_parent].into_iter().flatten() {
                    push(Change::Mail(p.clone()));
                }
            }
            (EventKind::Modified, None, Some(f)) => push(Change::Folder(f.clone())),
            (_, None, Some(_)) => push(Change::Tree),
            (_, None, None) => {}
        }
    }
    out
}

/// Подписка сервера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    pub id: String,
    /// Водяной знак pull-подписки: с него следующий `GetEvents`. У streaming — `None`.
    pub watermark: Option<String>,
    pub streaming: bool,
}

/// События одного `GetEvents` (pull): по порядку; водяной знак подписки уже сдвинут.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pull {
    pub events: Vec<Event>,
    pub watermark: Option<String>,
    /// Сервер отдал не всё — спросить ещё.
    pub more: bool,
}

pub(crate) mod req {
    //! Запросы подписки — отдельно, для тестов сборки.
    use super::*;

    /// `FolderIds` (папки и корень) и `EventTypes`; без папок — подписка на все папки ящика.
    fn scope(folders: &[FolderRef]) -> (&'static str, String) {
        let types: String = EVENT_TYPES.iter().map(|t| text_el("t:EventType", t)).collect();
        let types = format!("<t:EventTypes>{types}</t:EventTypes>");
        if folders.is_empty() {
            return ("true", types);
        }
        let ids: String = folders.iter().map(FolderRef::xml).collect();
        ("false", format!("<t:FolderIds>{ids}</t:FolderIds>{types}"))
    }

    pub(crate) fn subscribe_streaming(folders: &[FolderRef]) -> String {
        let (all, inner) = scope(folders);
        format!(r#"<m:Subscribe><m:StreamingSubscriptionRequest SubscribeToAllFolders="{all}">{inner}</m:StreamingSubscriptionRequest></m:Subscribe>"#)
    }

    pub(crate) fn subscribe_pull(folders: &[FolderRef], watermark: Option<&str>) -> String {
        let (all, inner) = scope(folders);
        let watermark = watermark.map(|w| text_el("t:Watermark", w)).unwrap_or_default();
        format!(
            r#"<m:Subscribe><m:PullSubscriptionRequest SubscribeToAllFolders="{all}">{inner}{watermark}<t:Timeout>{PULL_TIMEOUT_MINUTES}</t:Timeout></m:PullSubscriptionRequest></m:Subscribe>"#
        )
    }

    pub(crate) fn get_events(sub: &str, watermark: &str) -> String {
        format!("<m:GetEvents>{}{}</m:GetEvents>", text_el("m:SubscriptionId", sub), text_el("m:Watermark", watermark))
    }

    pub(crate) fn get_streaming_events(sub: &str, minutes: u32) -> String {
        format!(
            "<m:GetStreamingEvents><m:SubscriptionIds>{}</m:SubscriptionIds><m:ConnectionTimeout>{}</m:ConnectionTimeout></m:GetStreamingEvents>",
            text_el("t:SubscriptionId", sub),
            minutes.clamp(1, 30)
        )
    }

    pub(crate) fn unsubscribe(sub: &str) -> String {
        format!("<m:Unsubscribe>{}</m:Unsubscribe>", text_el("m:SubscriptionId", sub))
    }
}

pub(crate) mod parse {
    //! Разбор ответов подписки — для тестов на корпусе.
    use super::*;

    fn id(el: &El, name: &str) -> Option<String> {
        el.child(name).and_then(|i| i.attr("Id")).map(str::to_owned)
    }

    fn event(el: &El) -> Option<Event> {
        let kind = EventKind::parse(&el.name)?;
        Some(Event {
            kind,
            watermark: el.text_at(&["Watermark"]).map(str::to_owned),
            item: id(el, "ItemId"),
            folder: id(el, "FolderId"),
            parent: id(el, "ParentFolderId"),
            old_parent: id(el, "OldParentFolderId"),
        })
    }

    /// События `<Notification>` по порядку.
    pub(crate) fn notification(n: &El) -> Vec<Event> {
        n.children.iter().filter_map(event).collect()
    }

    pub(crate) fn subscribed(resp: &El, streaming: bool) -> Result<Subscription> {
        let msg = soap::single(resp)?;
        let id = msg.text_at(&["SubscriptionId"]).filter(|s| !s.is_empty()).ok_or_else(|| Error::Xml("Subscribe: no SubscriptionId".into()))?;
        Ok(Subscription { id: id.to_owned(), watermark: msg.text_at(&["Watermark"]).map(str::to_owned), streaming })
    }

    /// `GetEvents`: события и последний водяной знак (последнего события, иначе `PreviousWatermark`).
    pub(crate) fn events(resp: &El) -> Result<Pull> {
        let msg = soap::single(resp)?;
        let n = msg.child("Notification").ok_or_else(|| Error::Xml("GetEvents: no Notification".into()))?;
        let events = notification(n);
        let watermark = events.iter().rev().find_map(|e| e.watermark.clone()).or_else(|| n.text_at(&["PreviousWatermark"]).map(str::to_owned));
        Ok(Pull { events, watermark, more: n.bool_at(&["MoreEvents"]) })
    }

    /// Один конверт потока: события и закрыт ли поток (`ConnectionStatus` `Closed`). Ошибка подписки — ошибка.
    pub(crate) fn frame(doc: El) -> Result<(Vec<Event>, bool)> {
        let resp = soap::response(doc)?;
        let mut events = Vec::new();
        let mut closed = false;
        for msg in soap::messages(&resp) {
            soap::check(msg)?;
            closed |= msg.text_at(&["ConnectionStatus"]) == Some("Closed");
            for n in msg.child("Notifications").into_iter().flat_map(|n| n.all("Notification")) {
                events.extend(notification(n));
            }
        }
        Ok((events, closed))
    }

    /// Конец первого целого конверта в буфере потока: после `</Envelope>` (с любым префиксом). Сервер пишет
    /// конверты подряд, без разделителя; `<` внутри текста экранирован — ложного конца не бывает.
    pub(crate) fn envelope_end(buf: &[u8]) -> Option<usize> {
        const TAIL: &[u8] = b"Envelope>";
        let mut from = 0;
        while let Some(i) = buf[from..].windows(TAIL.len()).position(|w| w == TAIL).map(|i| i + from) {
            let mut j = i;
            while j > 0 && (buf[j - 1].is_ascii_alphanumeric() || buf[j - 1] == b':' || buf[j - 1] == b'_' || buf[j - 1] == b'-') {
                j -= 1;
            }
            let prefix = &buf[j..i];
            if j >= 2 && &buf[j - 2..j] == b"</" && (prefix.is_empty() || prefix.ends_with(b":")) {
                return Some(i + TAIL.len());
            }
            from = i + 1;
        }
        None
    }
}

/// Поток событий одной подписки (`GetStreamingEvents`): своё соединение, конверты по мере прихода.
pub struct EventStream {
    body: crate::http::Body,
    buf: Vec<u8>,
    deadline: Instant,
    closed: bool,
}

impl std::fmt::Debug for EventStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventStream").field("closed", &self.closed).finish()
    }
}

impl EventStream {
    /// Следующие события; `None` — сервер закрыл поток (истёк `ConnectionTimeout`): открыть заново по той же
    /// подписке. Ответ «всё в порядке» без событий — ждать дальше. Ошибка подписки — [`Error::is_subscription_lost`].
    pub async fn next(&mut self) -> Result<Option<Vec<Event>>> {
        loop {
            while let Some(end) = parse::envelope_end(&self.buf) {
                let frame: Vec<u8> = self.buf.drain(..end).collect();
                let (events, closed) = parse::frame(xml::parse(&frame)?)?;
                self.closed |= closed;
                if !events.is_empty() {
                    return Ok(Some(events));
                }
            }
            if self.closed {
                return Ok(None);
            }
            let left = self.deadline.saturating_duration_since(Instant::now());
            match tokio::time::timeout(left, self.body.chunk()).await.map_err(|_| Error::Timeout)?? {
                Some(chunk) => {
                    self.buf.extend_from_slice(&chunk);
                    if self.buf.len() > MAX_FRAME {
                        return Err(Error::TooLarge);
                    }
                }
                None => {
                    self.closed = true;
                    if self.buf.iter().any(|b| !b.is_ascii_whitespace()) {
                        // Хвост без конца конверта: сервер оборвал ответ.
                        return Err(Error::Closed);
                    }
                    return Ok(None);
                }
            }
        }
    }
}

impl Client {
    /// Streaming-подписка на папки (`folders` — с корнем ящика для событий дерева; пусто — все папки ящика).
    /// Сервер без streaming (до 2010 SP1) — ошибка: тогда [`Client::subscribe_pull`].
    pub async fn subscribe_streaming(&mut self, folders: &[FolderRef]) -> Result<Subscription> {
        parse::subscribed(&self.call("Subscribe", &req::subscribe_streaming(folders)).await?, true)
    }

    /// Pull-подписка (запасной путь): события — [`Client::get_events`]; `watermark` — продолжить с него.
    pub async fn subscribe_pull(&mut self, folders: &[FolderRef], watermark: Option<&str>) -> Result<Subscription> {
        parse::subscribed(&self.call("Subscribe", &req::subscribe_pull(folders, watermark)).await?, false)
    }

    /// События pull-подписки с её водяного знака, пока сервер говорит «ещё» (не больше 10 запросов); знак
    /// сдвигается.
    pub async fn get_events(&mut self, sub: &mut Subscription) -> Result<Vec<Event>> {
        let mut all = Vec::new();
        for _ in 0..MORE_EVENTS {
            let watermark = sub.watermark.clone().ok_or_else(|| Error::Xml("pull subscription without watermark".into()))?;
            let pull = parse::events(&self.call("GetEvents", &req::get_events(&sub.id, &watermark)).await?)?;
            if pull.watermark.is_some() {
                sub.watermark = pull.watermark;
            }
            all.extend(pull.events);
            if !pull.more {
                break;
            }
        }
        Ok(all)
    }

    /// Открыть поток streaming-подписки на `minutes` минут (до 30). Соединение уходит потоку; следующий запрос
    /// клиента откроет новое.
    pub async fn open_stream(&mut self, sub: &Subscription, minutes: u32) -> Result<EventStream> {
        let minutes = minutes.clamp(1, 30);
        let envelope = soap::envelope(&req::get_streaming_events(&sub.id, minutes));
        let (status, body) = self.http.post_stream(&self.url, envelope.into_bytes()).await?;
        if status != 200 && status != 500 {
            return Err(Error::Status(status));
        }
        tracing::debug!(status, minutes, "ews stream open");
        // Запас на последний кусок после `ConnectionTimeout`.
        let deadline = Instant::now() + Duration::from_secs(u64::from(minutes) * 60 + 90);
        Ok(EventStream { body, buf: Vec::new(), deadline, closed: false })
    }

    /// Снять подписку. Уже нет — не ошибка.
    pub async fn unsubscribe(&mut self, sub: &Subscription) -> Result<()> {
        match soap::single(&self.call("Unsubscribe", &req::unsubscribe(&sub.id)).await?) {
            Err(e) if e.is_subscription_lost() => Ok(()),
            r => r.map(|_| ()),
        }
    }
}

/// Как идут события.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Streaming,
    Pull,
}

/// Цикл уведомлений одной учётки: подписка (streaming, иначе pull), поток с переоткрытием, переподписка после
/// обрыва. Свой [`Client`] — отдельно от клиента синхронизации: поток держит соединение до получаса.
pub struct Notifier {
    client: Client,
    folders: Vec<FolderRef>,
    sub: Option<Subscription>,
    stream: Option<EventStream>,
    mode: Option<Mode>,
    /// Поток уже открывался: сбой открытия потом — не повод уходить на pull.
    streamed: bool,
    /// Подписка была и потерялась: после новой — [`Change::Resync`].
    lost: bool,
    /// Pull: пауза между `GetEvents` без событий.
    pub pull_every: Duration,
}

impl std::fmt::Debug for Notifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Notifier").field("mode", &self.mode).field("subscribed", &self.sub.is_some()).finish()
    }
}

impl Notifier {
    /// `folders` — папки писем и корень ящика ([`crate::WellKnown::Root`]); пусто — все папки ящика.
    pub fn new(client: Client, folders: Vec<FolderRef>) -> Notifier {
        Notifier { client, folders, sub: None, stream: None, mode: None, streamed: false, lost: false, pull_every: Duration::from_secs(60) }
    }

    /// Как идут события сейчас (`None` — ещё не подписаны).
    pub fn mode(&self) -> Option<Mode> {
        self.mode
    }

    async fn subscribe(&mut self) -> Result<()> {
        if self.mode != Some(Mode::Pull) {
            match self.client.subscribe_streaming(&self.folders).await {
                Ok(s) => {
                    self.sub = Some(s);
                    self.mode = Some(Mode::Streaming);
                    return Ok(());
                }
                Err(e @ Error::Ews { .. }) => {
                    tracing::info!(code = e.code().unwrap_or(""), "ews: streaming subscription refused, using pull");
                }
                Err(e) => return Err(e),
            }
        }
        self.sub = Some(self.client.subscribe_pull(&self.folders, None).await?);
        self.mode = Some(Mode::Pull);
        Ok(())
    }

    fn drop_subscription(&mut self) {
        tracing::info!("ews: subscription lost, resubscribing");
        self.sub = None;
        self.stream = None;
        self.lost = true;
    }

    /// Ждать изменений: без событий — без запросов (streaming) или `GetEvents` раз в [`Notifier::pull_every`].
    /// Сеть или сервер подвели — ошибка; следующий вызов продолжит (поток откроется заново, подписка — та же).
    pub async fn next(&mut self) -> Result<Vec<Change>> {
        loop {
            if self.sub.is_none() {
                self.subscribe().await?;
                if std::mem::take(&mut self.lost) {
                    return Ok(vec![Change::Resync]);
                }
            }
            let Some(sub) = self.sub.as_mut() else { continue };
            if sub.streaming {
                if self.stream.is_none() {
                    match self.client.open_stream(sub, STREAM_MINUTES).await {
                        Ok(s) => self.stream = Some(s),
                        Err(e) if e.is_subscription_lost() => {
                            self.drop_subscription();
                            continue;
                        }
                        Err(e @ (Error::Status(_) | Error::Http(_))) if !self.streamed => {
                            // Поток не проходит (прокси режет долгий ответ) — pull.
                            tracing::info!(error = %e, "ews: streaming unavailable, using pull");
                            self.mode = Some(Mode::Pull);
                            self.sub = None;
                            continue;
                        }
                        Err(e) => return Err(e),
                    }
                    self.streamed = true;
                }
                let Some(stream) = self.stream.as_mut() else { continue };
                match stream.next().await {
                    Ok(Some(events)) => {
                        let ch = changes(&events);
                        if !ch.is_empty() {
                            return Ok(ch);
                        }
                    }
                    Ok(None) => self.stream = None,
                    Err(e) if e.is_subscription_lost() => self.drop_subscription(),
                    Err(e) => {
                        self.stream = None;
                        return Err(e);
                    }
                }
            } else {
                match self.client.get_events(sub).await {
                    Ok(events) => {
                        let ch = changes(&events);
                        if !ch.is_empty() {
                            return Ok(ch);
                        }
                        tokio::time::sleep(self.pull_every).await;
                    }
                    Err(e) if e.is_subscription_lost() => self.drop_subscription(),
                    Err(e) => return Err(e),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FolderId, WellKnown};

    fn resp(b: &[u8]) -> El {
        soap::response(xml::parse(b).unwrap()).unwrap()
    }

    fn body(x: String) -> El {
        xml::parse(soap::envelope(&x).as_bytes()).unwrap()
    }

    #[test]
    fn subscribe_requests() {
        let inbox = FolderRef::Id(FolderId { id: "AAMkADinbox".into(), change_key: None });
        let folders = [FolderRef::WellKnown(WellKnown::Root), inbox];
        let s = body(req::subscribe_streaming(&folders));
        let r = s.at(&["Body", "Subscribe", "StreamingSubscriptionRequest"]).unwrap();
        assert_eq!(r.attr("SubscribeToAllFolders"), Some("false"));
        let order: Vec<&str> = r.children.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(order, ["FolderIds", "EventTypes"], "порядок схемы StreamingSubscriptionRequestType");
        assert_eq!(r.at(&["FolderIds", "DistinguishedFolderId"]).and_then(|d| d.attr("Id")), Some("msgfolderroot"), "корень — события дерева");
        assert_eq!(r.at(&["FolderIds", "FolderId"]).and_then(|d| d.attr("Id")), Some("AAMkADinbox"));
        let types: Vec<&str> = r.child("EventTypes").unwrap().all("EventType").map(|t| t.text.as_str()).collect();
        assert_eq!(types, ["NewMailEvent", "CreatedEvent", "DeletedEvent", "ModifiedEvent", "MovedEvent", "CopiedEvent"]);

        let all = body(req::subscribe_streaming(&[]));
        let r = all.at(&["Body", "Subscribe", "StreamingSubscriptionRequest"]).unwrap();
        assert_eq!(r.attr("SubscribeToAllFolders"), Some("true"));
        assert!(r.child("FolderIds").is_none(), "все папки — без FolderIds");

        let p = body(req::subscribe_pull(&folders, Some("AQAAAA==")));
        let r = p.at(&["Body", "Subscribe", "PullSubscriptionRequest"]).unwrap();
        let order: Vec<&str> = r.children.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(order, ["FolderIds", "EventTypes", "Watermark", "Timeout"], "порядок схемы PullSubscriptionRequestType");
        assert_eq!(r.text_at(&["Timeout"]), Some("30"));
        assert!(!req::subscribe_pull(&folders, None).contains("Watermark"));

        let g = body(req::get_events("sub1", "wm1"));
        let order: Vec<&str> = g.at(&["Body", "GetEvents"]).unwrap().children.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(order, ["SubscriptionId", "Watermark"]);
        let st = body(req::get_streaming_events("sub1", 45));
        let gs = st.at(&["Body", "GetStreamingEvents"]).unwrap();
        assert_eq!(gs.text_at(&["SubscriptionIds", "SubscriptionId"]), Some("sub1"));
        assert_eq!(gs.text_at(&["ConnectionTimeout"]), Some("30"), "не больше 30 минут");
        assert!(req::unsubscribe("sub1").contains("<m:SubscriptionId>sub1</m:SubscriptionId>"));
    }

    #[test]
    fn subscribed() {
        let s = parse::subscribed(&resp(include_bytes!("../tests/fixtures/subscribe_streaming.xml")), true).unwrap();
        assert_eq!((s.id.as_str(), s.watermark.as_deref(), s.streaming), ("JwBkYjVwcjA2bWIxMTQ=", None, true));
        let p = parse::subscribed(&resp(include_bytes!("../tests/fixtures/subscribe_pull.xml")), false).unwrap();
        assert_eq!((p.id.as_str(), p.watermark.as_deref()), ("KQBkYjVwcjA2bWIxMTQ=", Some("AQAAAPq2ZQ==")));
    }

    #[test]
    fn pull_events() {
        let p = parse::events(&resp(include_bytes!("../tests/fixtures/get_events.xml"))).unwrap();
        assert!(!p.more);
        assert_eq!(p.watermark.as_deref(), Some("AQAAAPq2ZE="), "знак последнего события");
        let kinds: Vec<EventKind> = p.events.iter().map(|e| e.kind).collect();
        assert_eq!(kinds, [EventKind::Status, EventKind::NewMail, EventKind::Moved, EventKind::Created, EventKind::Modified]);
        let moved = &p.events[2];
        assert_eq!((moved.item.as_deref(), moved.parent.as_deref(), moved.old_parent.as_deref()), (Some("AAMkADmsg2"), Some("AAMkADtrash"), Some("AAMkADinbox")));
        assert_eq!((p.events[3].folder.as_deref(), p.events[3].parent.as_deref()), (Some("AAMkADnew"), Some("AAMkADinbox")));
        assert_eq!(
            changes(&p.events),
            [Change::Mail("AAMkADinbox".into()), Change::Mail("AAMkADtrash".into()), Change::Tree, Change::Folder("AAMkADinbox".into())],
            "по папкам без повторов; создание папки — дерево; счётчики папки — сама папка"
        );
        let err = parse::events(&resp(include_bytes!("../tests/fixtures/get_events_expired.xml"))).unwrap_err();
        assert!(err.is_subscription_lost(), "{err}");
        assert!(!Error::Ews { code: "ErrorAccessDenied".into(), message: String::new() }.is_subscription_lost());
    }

    /// Поток: конверты подряд без разделителя, куски режут их где угодно (`ConnectionStatus`, события, `Closed`).
    #[test]
    fn streaming_frames() {
        let stream = include_bytes!("../tests/fixtures/get_streaming_events.xml");
        let mut buf: Vec<u8> = Vec::new();
        let mut frames = Vec::new();
        for chunk in stream.chunks(37) {
            buf.extend_from_slice(chunk);
            while let Some(end) = parse::envelope_end(&buf) {
                let frame: Vec<u8> = buf.drain(..end).collect();
                frames.push(parse::frame(xml::parse(&frame).unwrap()).unwrap());
            }
        }
        assert!(buf.iter().all(u8::is_ascii_whitespace), "хвоста нет");
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0], (Vec::new(), false), "OK без событий");
        let (events, closed) = &frames[1];
        assert!(!closed);
        assert_eq!(events.iter().map(|e| e.kind).collect::<Vec<_>>(), [EventKind::NewMail, EventKind::Deleted, EventKind::Moved]);
        assert_eq!(events[0].watermark, None, "у streaming водяного знака нет");
        assert_eq!(changes(events), [Change::Mail("AAMkADinbox".into()), Change::Tree]);
        assert_eq!(frames[2], (Vec::new(), true), "Closed — открыть заново");

        let lost = include_bytes!("../tests/fixtures/get_streaming_events_lost.xml");
        let end = parse::envelope_end(lost).unwrap();
        let err = parse::frame(xml::parse(&lost[..end]).unwrap()).unwrap_err();
        assert_eq!(err.code(), Some("ErrorSubscriptionNotFound"));
        assert!(err.is_subscription_lost());
    }

    #[test]
    fn envelope_end_with_any_prefix() {
        assert_eq!(parse::envelope_end(b"<Envelope><Body/></Envelope><Env"), Some(28));
        assert_eq!(parse::envelope_end(b"<s:Envelope></s:Envelope>"), Some(25));
        assert_eq!(parse::envelope_end(b"<x>MyEnvelope></x>"), None, "не закрывающий тег");
        assert_eq!(parse::envelope_end(b"</xEnvelope>"), None, "другое имя");
        assert_eq!(parse::envelope_end(b"<Envelope>"), None);
    }
}
