//! Каналы бесед в композиционном корне (ADR 0012, CH-02б): потоки учёток каналов и их события — без интерфейса
//! каналов (он — CH-04). Учётки — из базы профиля (`chan_account`), канал — из реестра плагинов (`plugins.rs`);
//! демо-канал (`postman_demo::channel`) — только в `--demo`, без замера и снимков. Секреты сессий — хранилище ОС
//! (`sys::credentials`, запись `Postman channel <канал> <учётка>`), HTML сообщений — в IR воркером-песочницей.
//!
//! Запрос интерфейса — `sync::Command::Channel` ([`Mailer::send_channel`]); ответ потока — `sync::Event::Channel`
//! в общем канале событий ([`Mailer::on_channel`]).

use std::sync::Arc;

use gpui_kit::{AppContext as _, Context};
use postman_core::channel::{self, AccountId, ChanAccount, ChannelError, ChannelId, ChannelState, Request, Secret, Secrets, Start, Update};
use postman_core::sync::{Command, Event, Index, UnboundedSender};

use crate::mailer::Mailer;
use crate::sys;

/// Потоки учёток каналов этого окна.
#[derive(Default)]
pub(crate) struct Channels {
    lines: Vec<(ChanAccount, UnboundedSender<Request>)>,
    /// Последнее состояние каждой учётки (строка состояния, «Проблема → причина → исправление» — CH-04).
    pub(crate) states: Vec<(ChanAccount, ChannelState)>,
    /// Кто я в учётке канала (имя — строкой учётки в «Настройки → Аккаунты»).
    names: Vec<(ChanAccount, String)>,
    /// Последние личные контакты и справочник: участники каналов связываются с ними (`channel::person::auto`).
    cards: Arc<Vec<postman_core::model::ContactCard>>,
    directory: Arc<Vec<postman_core::model::Principal>>,
}

impl Channels {
    /// Запрос в поток учётки; `false` — потока нет (канал выключен, учётка вышла).
    fn send(&self, account: &ChanAccount, request: Request) -> bool {
        self.lines.iter().find(|(a, _)| a == account).is_some_and(|(_, tx)| tx.send(request).is_ok())
    }

    fn running(&self, account: &ChanAccount) -> bool {
        self.lines.iter().any(|(a, tx)| a == account && !tx.is_closed())
    }

    fn set_name(&mut self, account: &ChanAccount, name: String) {
        self.names.retain(|(a, _)| a != account);
        self.names.push((account.clone(), name));
    }
}

/// Секреты сессий каналов в хранилище ОС: байты — шестнадцатеричной строкой.
struct OsSecrets;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

impl Secrets for OsSecrets {
    fn get(&self, channel: &ChannelId, account: &AccountId) -> Option<Secret> {
        match sys::credentials::read(&Self::name(channel, account)) {
            Ok(Some(s)) => unhex(&s).map(Secret::new),
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(channel = %channel, "channel: session secret not read: {e}");
                None
            }
        }
    }

    fn put(&self, channel: &ChannelId, account: &AccountId, secret: &Secret) -> Result<(), ChannelError> {
        sys::credentials::write(&Self::name(channel, account), &account.0, &hex(secret.expose())).map_err(ChannelError::Storage)
    }

    fn remove(&self, channel: &ChannelId, account: &AccountId) -> Result<(), ChannelError> {
        sys::credentials::delete(&Self::name(channel, account)).map_err(ChannelError::Storage)
    }
}

/// HTML → IR — своим процессом воркера на поток учётки.
fn render() -> channel::Render {
    channel::worker_render(postman_worker::Client::new(postman_worker::Client::default_exe(), crate::migration::WORKER_TIMEOUT))
}

impl Mailer {
    /// Потоки учёток каналов из базы профиля, чей канал есть в реестре (живой ящик). Плагины каналов — ссылкам ядра
    /// (`channel::Links`: кросс-ссылки в письмах, задачах, сообщениях); тела, записанные до индекса `Message-ID`, —
    /// в него, в фоне.
    pub(crate) fn start_channels(&mut self, index: Index, cx: &mut Context<Self>) {
        channel::Links::of(crate::plugins::registry()).install();
        let cache = self.cache.clone();
        cx.background_spawn(async move {
            if let Err(e) = channel::mail::index_message_ids(&cache, &|| false) {
                tracing::warn!("channel: message ids not indexed: {e}");
            }
        })
        .detach();
        let stores = match channel::ChannelStore::all(&self.cache) {
            Ok(s) => s,
            Err(e) => return tracing::warn!("channel: accounts not read: {e}"),
        };
        let secrets: Arc<dyn Secrets> = Arc::new(OsSecrets);
        let prefs = crate::prefs::get(cx);
        for store in stores {
            // Канал выключен настройкой или политикой (ADR 0012 п. 11): поток не запускается, данные — в базе.
            if !crate::account_telegram::channel_on(&store.channel().0, &prefs) {
                continue;
            }
            let Some(plugin) = crate::plugins::registry().channel(&store.channel().0).cloned() else { continue };
            if let Ok(Some(me)) = store.me() {
                self.channels.set_name(&ChanAccount::new(store.channel().0.clone(), store.account().0.clone()), me.name);
            }
            let start = Start {
                cache: self.cache.clone(),
                secrets: secrets.clone(),
                account: store.account().clone(),
                name: String::new(),
                render: Some(render()),
                index: index.clone(),
            };
            self.spawn_channel(plugin, start, cx);
        }
        self.publish_channels(cx);
    }

    /// Демо-канал (`--demo`): сервер в памяти, сессия — в памяти; беседы — в базу демо-ящика.
    pub(crate) fn start_demo_channel(&mut self, cx: &mut Context<Self>) {
        use postman_demo::channel as demo;
        let (id, account) = (ChannelId::new(demo::ID), AccountId::new(demo::ACCOUNT));
        let secrets = Arc::new(channel::MemorySecrets::default());
        if let Err(e) = secrets.put(&id, &account, &Secret::new(demo::SESSION.to_vec())) {
            return tracing::warn!("channel: demo session not stored: {e}");
        }
        let plugin: Arc<dyn channel::ChannelPlugin> = Arc::new(demo::DemoChannel::new(demo::DemoServer::new()));
        channel::Links::new(vec![plugin.clone()]).install();
        let start = Start { cache: self.cache.clone(), secrets, account, name: postman_demo::ME_NAME.into(), render: Some(render()), index: None };
        self.spawn_channel(plugin, start, cx);
    }

    fn spawn_channel(&mut self, plugin: Arc<dyn channel::ChannelPlugin>, start: Start, cx: &mut Context<Self>) {
        let account = ChanAccount::new(plugin.id(), start.account.0.clone());
        if self.channels.running(&account) {
            return;
        }
        let (events, rx) = std::sync::mpsc::channel::<Event>();
        let (wake_tx, mut wake_rx) = futures::channel::mpsc::unbounded::<()>();
        let repaint = Box::new(move || {
            let _ = wake_tx.unbounded_send(());
        });
        tracing::info!(channel = %account.channel, "channel: start");
        let tx = postman_core::sync::spawn_channel(plugin, start, events, repaint);
        self.channels.lines.retain(|(a, _)| *a != account);
        self.channels.lines.push((account, tx));
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            // Поток учётки закончился — отправитель пробуждений закрыт, задача завершается.
            while wake_rx.next().await.is_some() {
                while wake_rx.try_recv().is_ok() {}
                let batch: Vec<Event> = rx.try_iter().collect();
                if batch.is_empty() {
                    continue;
                }
                let alive = this.update(cx, |m, cx| {
                    for e in batch {
                        if let Event::Channel(account, update) = e {
                            m.on_channel(account, update, cx);
                        }
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Запрос интерфейса учётке канала (`Command::Channel`); `false` — не команда канала или потока нет.
    #[allow(dead_code)] // интерфейс каналов — CH-04
    pub(crate) fn send_channel(&self, command: Command) -> bool {
        let Command::Channel { account, request } = command else { return false };
        self.channels.send(&account, *request)
    }

    /// События почты для связей каналов (CH-02в), в фоне: контакты и справочник — участники каналов связываются с ними
    /// (только в базе); встречи (неделя и поиск календаря) — их ссылки в `xref` («Где на это сослались»).
    pub(crate) fn on_links_sync(&mut self, ev: &Event, cx: &mut Context<Self>) {
        let cache = self.cache.clone();
        match ev {
            Event::ContactsLoaded(cards) => self.channels.cards = Arc::new(cards.clone()),
            Event::DirectoryLoaded(people) => self.channels.directory = Arc::new(people.clone()),
            // Встречи недели и найденные поиском календаря (на год назад и вперёд): кэша календаря нет — ссылки
            // встреч, которые не приходили, не видны.
            Event::EventsLoaded { events, .. } | Event::EventsFound { events, .. } => {
                let texts: Vec<(String, String)> = events
                    .iter()
                    .map(|e| {
                        (
                            e.uid.clone(),
                            format!(
                                "{}
{}
{}",
                                e.title, e.location, e.description
                            ),
                        )
                    })
                    .collect();
                cx.background_spawn(async move {
                    let links = channel::Links::installed();
                    for (uid, text) in texts {
                        if let Err(e) = channel::xref::index_event(&cache, &links, &uid, &text) {
                            tracing::warn!("channel: meeting links not stored: {e}");
                        }
                    }
                })
                .detach();
                return;
            }
            _ => return,
        }
        let (cards, directory) = (self.channels.cards.clone(), self.channels.directory.clone());
        cx.background_spawn(async move {
            match channel::person::auto(&cache, &channel::Links::installed(), &cards, &directory) {
                Ok(n) => tracing::debug!(linked = n, "channel: people linked to contacts"),
                Err(e) => tracing::warn!("channel: people not linked to contacts: {e}"),
            }
        })
        .detach();
    }

    /// Ответ потока учётки канала. Интерфейса каналов ещё нет (CH-04): состояние — в память окна и журнал.
    pub(crate) fn on_channel(&mut self, account: ChanAccount, update: Update, cx: &mut Context<Self>) {
        match update {
            Update::State(state) => {
                if let ChannelState::Problem(e) = &state {
                    tracing::warn!(channel = %account.channel, "channel: {e}");
                }
                match self.channels.states.iter_mut().find(|(a, _)| *a == account) {
                    Some((_, s)) => *s = state,
                    None => self.channels.states.push((account, state)),
                }
                cx.notify();
            }
            Update::SignedOut => {
                self.channels.lines.retain(|(a, _)| *a != account);
                self.channels.states.retain(|(a, _)| *a != account);
                self.channels.names.retain(|(a, _)| *a != account);
            }
            Update::Connected { me, .. } => self.channels.set_name(&account, me.name),
            Update::Changed { conversations, gone } => {
                tracing::debug!(channel = %account.channel, changed = conversations.len(), gone = gone.len(), "channel: conversations changed");
                return;
            }
            _ => return,
        }
        self.publish_channels(cx);
    }

    /// Учётки каналов — настройкам (`account_telegram::Lines`).
    fn publish_channels(&self, cx: &mut Context<Self>) {
        let lines = self
            .channels
            .lines
            .iter()
            .filter(|(_, tx)| !tx.is_closed())
            .map(|(a, _)| crate::account_telegram::Line {
                account: a.clone(),
                name: self.channels.names.iter().find(|(x, _)| x == a).map(|(_, n)| n.clone()).unwrap_or_default(),
                state: self.channels.states.iter().find(|(x, _)| x == a).map(|(_, s)| s.clone()),
            })
            .collect();
        cx.set_global(crate::account_telegram::Lines(lines));
    }

    /// «Настройки → Аккаунты → Каналы бесед» (CH-03): вход удался, «Выйти», включатель.
    pub(crate) fn on_telegram(&mut self, ev: crate::account_telegram::PanelEvent, cx: &mut Context<Self>) {
        use crate::account_telegram::{PanelEvent, TELEGRAM};
        match ev {
            PanelEvent::Toggled(on) => {
                crate::prefs::update(cx, |p| p.channels.telegram = on);
                if on {
                    let index = self.live.index.clone();
                    self.start_channels(index, cx);
                } else {
                    // Запросы закрыты — потоки учёток канала кончаются (исходящее в окне «Отменить» уходит).
                    self.channels.lines.retain(|(a, _)| a.channel.0 != TELEGRAM);
                    self.channels.states.retain(|(a, _)| a.channel.0 != TELEGRAM);
                    tracing::info!(channel = TELEGRAM, "channel: switched off");
                }
                self.publish_channels(cx);
            }
            PanelEvent::SignOut(account) => {
                if !self.channels.send(&account, Request::SignOut) {
                    tracing::warn!(channel = %account.channel, "channel: sign-out, account not running");
                }
            }
            PanelEvent::Signed { account, name, secret } => {
                let Some(plugin) = crate::plugins::registry().channel(TELEGRAM).cloned() else { return };
                let id = ChannelId::new(TELEGRAM);
                let index = self.live.index.clone();
                let cache = self.cache.clone();
                self.channels.set_name(&ChanAccount::new(TELEGRAM, account.0.clone()), name.clone());
                // Секрет — в хранилище ОС, строка учётки — в базу профиля: в фоне; затем поток учётки.
                let task = cx.background_spawn({
                    let (account, name) = (account.clone(), name.clone());
                    async move {
                        OsSecrets.put(&id, &account, &secret)?;
                        channel::ChannelStore::open(cache, &id, &account, &name).map(|_| ())
                    }
                });
                cx.spawn(async move |this, cx| {
                    let stored = task.await;
                    let _ = this.update(cx, |m, cx| match stored {
                        Ok(()) => {
                            tracing::info!(channel = TELEGRAM, "channel: account added");
                            let start = Start { cache: m.cache.clone(), secrets: Arc::new(OsSecrets), account, name, render: Some(render()), index };
                            m.spawn_channel(plugin, start, cx);
                            m.publish_channels(cx);
                        }
                        Err(e) => tracing::warn!(channel = TELEGRAM, "channel: account not stored: {e}"),
                    });
                })
                .detach();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn secret_hex_round_trip() {
        let b = vec![0u8, 1, 0xab, 0xff];
        assert_eq!(super::unhex(&super::hex(&b)), Some(b));
        assert_eq!(super::unhex("abc"), None);
        assert_eq!(super::unhex("zz"), None);
    }
}
