//! Связь экрана с сервером (шов фазы 2, `docs/specs/S3.md`): живой ящик — поток синхронизации
//! `postman_core::sync` (подключает S3-04), демо — без сервера.
//!
//! Части экрана не держат свой канал к серверу: команды уходят через [`Backend::send`], события синхронизации
//! `Mailer` раздаёт всем частям (`on_sync`) — каждая берёт свои (тело письма, блоб, итог отправки…).
//!
//! Несколько учётных записей (S7-03б): у каждой — свой поток синхронизации; команда идёт туда, чьи её ключи
//! (`Command::routes`), письмо — в учётную запись адреса «От», поиск «Все папки» и неделя календаря — во все.

use postman_core::sync::{Command, Target, UnboundedSender};

/// Откуда экран берёт письма и куда отдаёт действия.
pub(crate) enum Backend {
    /// Демо-ящик (`--demo`, бенч, снимки): сервера нет, действия с сервером — тост «нужен сервер».
    Demo,
    /// Учётная запись на сервере: команды — в поток синхронизации; `extra` — дополнительные учётные записи профиля
    /// (`@<n>` и их поток), `senders` — адрес «От» → учётная запись.
    Live { commands: UnboundedSender<Command>, extra: Vec<(String, UnboundedSender<Command>)>, senders: Vec<(String, Option<String>)> },
}

impl Backend {
    pub(crate) fn is_live(&self) -> bool {
        matches!(self, Backend::Live { .. })
    }

    /// Команда синхронизации. `false` — сервера нет (демо) или поток синхронизации учётной записи остановился:
    /// вызывающий говорит об этом тостом и не теряет данные пользователя (черновик остаётся в окне).
    pub(crate) fn send(&self, command: Command) -> bool {
        let Backend::Live { commands, extra, senders } = self else { return false };
        let line = |profile: Option<&str>| match profile {
            None => Some(commands),
            Some(p) => extra.iter().find(|(q, _)| q == p).map(|(_, tx)| tx),
        };
        let mut ok = true;
        for (target, command) in command.routes() {
            ok &= match target {
                Target::Profile(p) => line(p.as_deref()).is_some_and(|tx| tx.send(command).is_ok()),
                Target::Sender(email) => {
                    let profile = senders.iter().find(|(e, _)| e.eq_ignore_ascii_case(&email)).and_then(|(_, p)| p.clone());
                    line(profile.as_deref()).is_some_and(|tx| tx.send(command).is_ok())
                }
                Target::All => {
                    for (_, tx) in extra {
                        if let Some(copy) = command.for_each_account() {
                            // Дополнительная учётная запись остановилась — основная всё равно ответит.
                            let _ = tx.send(copy);
                        }
                    }
                    commands.send(command).is_ok()
                }
            };
        }
        ok
    }

    /// Команда в синхронизацию учётной записи профиля `profile` (`None` — основной) как есть: маршрут уже решило ядро
    /// (`Command::routes_by`, задания переноса). `false` — сервера нет или учётная запись остановилась.
    pub(crate) fn send_to(&self, profile: Option<&str>, command: Command) -> bool {
        let Backend::Live { commands, extra, .. } = self else { return false };
        match profile {
            None => commands.send(command).is_ok(),
            Some(p) => extra.iter().find(|(q, _)| q == p).is_some_and(|(_, tx)| tx.send(command).is_ok()),
        }
    }

    /// Дополнительная учётная запись профиля начала синхронизацию.
    pub(crate) fn add_extra(&mut self, profile: String, tx: UnboundedSender<Command>) {
        if let Backend::Live { extra, .. } = self {
            extra.retain(|(p, _)| *p != profile);
            extra.push((profile, tx));
        }
    }

    /// Дополнительная учётная запись удалена: её поток заканчивается, когда канал закрыт.
    pub(crate) fn remove_extra(&mut self, profile: &str) {
        if let Backend::Live { extra, .. } = self {
            extra.retain(|(p, _)| p != profile);
        }
    }

    /// Адреса «От» всех учётных записей: куда уходит письмо.
    pub(crate) fn set_senders(&mut self, list: Vec<(String, Option<String>)>) {
        if let Backend::Live { senders, .. } = self {
            *senders = list;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Backend;
    use postman_compose::{Address, Draft};
    use postman_core::sync::{Command, Release};
    use tokio::sync::mpsc::unbounded_channel;

    #[test]
    fn commands_go_to_their_account() {
        let (main, mut main_rx) = unbounded_channel();
        let (second, mut second_rx) = unbounded_channel();
        let backend = Backend::Live {
            commands: main,
            extra: vec![("@2".into(), second)],
            senders: vec![("me@a.test".into(), None), ("me@b.test".into(), Some("@2".into()))],
        };
        assert!(backend.send(Command::Body("@2/e1".into())));
        assert!(matches!(second_rx.try_recv(), Ok(Command::Body(k)) if k == "@2/e1"));
        assert!(main_rx.try_recv().is_err());
        // Поиск «Все папки» — во все учётные записи.
        assert!(backend.send(Command::Search { query: "q".into(), mailbox: None, ticket: 3 }));
        assert!(matches!(main_rx.try_recv(), Ok(Command::Search { ticket: 3, .. })));
        assert!(matches!(second_rx.try_recv(), Ok(Command::Search { ticket: 3, .. })));
        // Письмо — в учётную запись адреса «От».
        let draft = Draft { from: Address::new("", "ME@b.test"), ..Default::default() };
        assert!(backend.send(Command::Send { draft: Box::new(draft), replace: None, ticket: 1, release: Release::Now }));
        assert!(matches!(second_rx.try_recv(), Ok(Command::Send { .. })));
        assert!(main_rx.try_recv().is_err());
        // Учётной записи нет (удалена) — команда не ушла.
        assert!(!backend.send(Command::Body("@9/e".into())));
        assert!(!Backend::Demo.send(Command::Contacts));
    }
}
