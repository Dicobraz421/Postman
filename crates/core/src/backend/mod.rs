//! Договор плагинов путей к серверам (ADR 0004, S7-02б): `core` функционален, протоколы — плагины.
//!
//! - **Почтовый путь** ([`MailPlugin`]) — фабрика синхронизации учётной записи: свой поток с tokio, который говорит
//!   договором команд и событий (`sync::Command`, `sync::Event`) на общем кэше. Так уже жили JMAP и IMAP; граница
//!   плагина — канал команд, поэтому `async` через `dyn` здесь не нужен.
//! - **Пути областей** ([`AreaPlugin`]) — календарь, задачи, занятость, контакты, справочник у учётной записи, где
//!   почтовый путь их не даёт (IMAP + CalDAV/CardDAV, позже IMAP + EWS). Адрес области — в адресе учётной записи
//!   (`?<ключ>=https://…`); почтовый путь подключает её в своём потоке и передаёт ей команды её областей
//!   ([`Areas`]). Методы — с упакованными футурами ([`BoxFuture`]): трейты остаются dyn-совместимыми.
//! - **Что умеет сервер** — `model::Features`: путь сообщает их событием `Event::Features`, области — через
//!   [`Areas::features`] (почтовый путь объединяет). Правило «Нет у сервера — нет на экране» — у интерфейса.
//! - **Перенос между учётками** (ADR 0006 п. 4) — функция ядра: задание ведёт ядро, почтовый путь выполняет шаги
//!   своей учётки — письмо целиком (`Command::FetchRaw`), выгрузку (`Command::Import`, если `Features::import`),
//!   перенос сервером между аккаунтами сессии (`Command::ServerCopy`, если `Features::server_copy`), квоту
//!   (`settings::Request::Quota`, если `Feature::Quota`). Не умеет — `Command::unsupported`.
//! - **Реестр** ([`Registry`]) собирает композиционный корень (`app-gpui`): какие плагины есть и в каком порядке.
//!   Новый протокол — новый крейт и строка в корне; `core` не меняется.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, mpsc};

use postman_cache::Cache;
use postman_calendar::{Invitation, Owner, Partstat};

use crate::model::{Error, Features};
use crate::sync::{Account, Command, Event, Index, SyncError, UnboundedSender};

/// Футура метода плагина: упакована, чтобы трейты были dyn-совместимы. Без `Send`: плагин работает в потоке
/// синхронизации своей учётной записи (рантайм tokio на один поток).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Всё для запуска синхронизации учётной записи.
pub struct Spawn {
    pub account: Account,
    /// Учётная запись профиля (S7-03б): `None` — основная, `@<n>` — дополнительная.
    pub profile: Option<String>,
    pub cache: Cache,
    pub index: Index,
    pub events: mpsc::Sender<Event>,
    pub repaint: Box<dyn Fn() + Send>,
    /// Пути областей реестра: почтовый путь подключает те, чьи адреса есть у учётной записи.
    pub areas: Vec<Arc<dyn AreaPlugin>>,
}

/// Почтовый путь к серверу (JMAP, IMAP + SMTP…): синхронизация учётной записи целиком.
pub trait MailPlugin: Send + Sync {
    /// Для журнала: `jmap`, `imap`.
    fn name(&self) -> &'static str;

    /// Адрес — точно этого пути (своя схема: `imaps://`). Такой адрес не проверяется по сети.
    fn claims(&self, _url: &str) -> bool {
        false
    }

    /// Вид сервера по адресу (S7-04в): отвечает ли по адресу этот путь; `Some` — адрес учётной записи для него.
    fn probe<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Option<String>>;

    /// Узел, где искать пути областей (календарь, контакты), если этот путь их не даёт; `None` — даёт сам.
    fn area_host(&self, _url: &str) -> Option<String> {
        None
    }

    /// Адрес области с ключом `key` уже есть в адресе учётной записи.
    fn has_area(&self, _url: &str, _key: &str) -> bool {
        false
    }

    /// Адрес учётной записи с адресом области `key`.
    fn with_area(&self, url: &str, _key: &str, _area: &str) -> String {
        url.to_owned()
    }

    /// Только TLS 1.3 для новых подключений (S5-30).
    fn set_tls13_only(&self, _on: bool) {}

    /// Запустить синхронизацию учётной записи; ответ — канал её команд.
    fn spawn(&self, s: Spawn) -> UnboundedSender<Command>;
}

/// Вход в область: логин и пароль учётной записи и её адрес (участник встреч, если сервер не дал свой).
#[derive(Clone)]
pub struct Login {
    pub user: String,
    pub password: String,
    pub email: String,
}

impl std::fmt::Debug for Login {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Login").field("user", &self.user).field("email", &self.email).finish_non_exhaustive()
    }
}

/// Путь областей (CalDAV и CardDAV, позже EWS): календарь, задачи, занятость, контакты, справочник.
pub trait AreaPlugin: Send + Sync {
    /// Ключ адреса в адресе учётной записи: `dav` → `?dav=https://…`.
    fn key(&self) -> &'static str;

    /// Есть ли этот путь на узле `host` (вид сервера по адресу); `Some` — его адрес.
    fn probe<'a>(&'a self, host: &'a str) -> BoxFuture<'a, Option<String>>;

    /// Только TLS 1.3 для новых подключений (S5-30).
    fn set_tls13_only(&self, _on: bool) {}

    /// Войти и найти области. Ошибка — тостом календаря; почта работает без областей.
    fn connect<'a>(&'a self, url: &'a str, login: &'a Login) -> BoxFuture<'a, Result<Box<dyn Areas>, Error>>;
}

/// Что нужно областям от потока учётной записи.
pub struct AreaCtx<'a> {
    pub cache: &'a Cache,
    /// Учётная запись профиля: `None` — основная.
    pub profile: Option<&'a str>,
    /// Владелец событий дополнительной учётной записи в общем календаре (S7-03б); у основной — `None`.
    pub owner: Option<&'a Owner>,
    pub notify: &'a dyn Fn(Event),
}

/// Подключённые области учётной записи: выполняют команды своих областей тем же договором событий.
pub trait Areas {
    /// Что нашлось на сервере.
    fn features(&self) -> Features;

    /// Команда области (неделя, поиск в календаре, занятость, встреча, задачи, контакты); `Some` — не её, вернуть.
    fn handle<'a>(&'a mut self, cmd: Command, ctx: AreaCtx<'a>) -> BoxFuture<'a, Option<Command>>;

    /// Ответ на приглашение своей копией на сервере (ответ организатору шлёт сервер); `false` — копии нет, отвечать
    /// письмом.
    fn respond<'a>(&'a mut self, invite: &'a Invitation, status: Partstat) -> BoxFuture<'a, Result<bool, Error>>;

    /// Правки задач из кэша — на сервер (шаг поручения, S6-09).
    fn flush_tasks<'a>(&'a mut self, ctx: AreaCtx<'a>) -> BoxFuture<'a, Result<(), Error>>;

    /// Изменилось ли что-то с прошлого опроса (без push).
    fn changed(&mut self) -> BoxFuture<'_, Result<bool, Error>>;
}

/// Плагины путей к серверам, которые знает приложение. Порядок почтовых путей — порядок проверки адреса; первый —
/// путь по умолчанию (адрес без своей схемы).
#[derive(Default, Clone)]
pub struct Registry {
    mail: Vec<Arc<dyn MailPlugin>>,
    areas: Vec<Arc<dyn AreaPlugin>>,
}

impl Registry {
    #[must_use]
    pub fn with_mail(mut self, p: impl MailPlugin + 'static) -> Self {
        self.mail.push(Arc::new(p));
        self
    }

    #[must_use]
    pub fn with_area(mut self, p: impl AreaPlugin + 'static) -> Self {
        self.areas.push(Arc::new(p));
        self
    }

    /// Адрес со своей схемой какого-то пути (`imaps://…`): его не дополняют `https://`.
    pub fn claims(&self, url: &str) -> bool {
        self.mail.iter().any(|p| p.claims(url))
    }

    /// Путь учётной записи: чья схема, иначе первый (путь по умолчанию).
    fn mail_for(&self, url: &str) -> Option<&Arc<dyn MailPlugin>> {
        self.mail.iter().find(|p| p.claims(url)).or(self.mail.first())
    }

    /// Синхронизация учётной записи путём её адреса. Путей нет — ошибка в окне, команды уходят в никуда.
    pub fn spawn(
        &self,
        account: Account,
        profile: Option<String>,
        cache: Cache,
        index: Index,
        events: mpsc::Sender<Event>,
        repaint: impl Fn() + Send + 'static,
    ) -> UnboundedSender<Command> {
        let Some(p) = self.mail_for(&account.url) else {
            let _ = events.send(Event::Error(SyncError::internal("no server backend registered")));
            return tokio::sync::mpsc::unbounded_channel().0;
        };
        tracing::debug!("sync backend: {}", p.name());
        p.spawn(Spawn { account, profile, cache, index, events, repaint: Box::new(repaint), areas: self.areas.clone() })
    }

    /// Только TLS 1.3 («Настройки → Безопасность», S5-30); иначе — 1.2 и выше. Действует на новые подключения всех
    /// путей.
    pub fn set_tls13_only(&self, on: bool) {
        self.mail.iter().for_each(|p| p.set_tls13_only(on));
        self.areas.iter().for_each(|p| p.set_tls13_only(on));
    }

    /// Вид сервера по адресу (S7-04в): адрес со своей схемой — его путь; иначе почтовые пути по порядку (JMAP по
    /// `/.well-known/jmap`, IMAP на порту 993 того же узла…); никто не ответил — адрес как есть (ошибку покажет
    /// вход). Путь без своих областей — ещё пути областей на узле учётной записи (CalDAV и CardDAV, RFC 6764) —
    /// в адрес. SRV-записи (RFC 6186) — нет: без своего DNS-клиента.
    pub async fn detect(&self, url: &str) -> String {
        let found = match self.mail.iter().find(|p| p.claims(url)) {
            Some(p) => Some((p, url.to_owned())),
            None => {
                let mut found = None;
                for p in &self.mail {
                    if let Some(u) = p.probe(url).await {
                        found = Some((p, u));
                        break;
                    }
                }
                found
            }
        };
        let Some((p, mut url)) = found else { return url.to_owned() };
        if let Some(host) = p.area_host(&url) {
            for a in &self.areas {
                if p.has_area(&url, a.key()) {
                    continue;
                }
                if let Some(area) = a.probe(&host).await {
                    url = p.with_area(&url, a.key(), &area);
                }
            }
        }
        url
    }

    /// [`Registry::detect`] из потока без рантайма tokio (интерфейс — в фоне).
    pub fn detect_blocking(&self, url: &str) -> String {
        match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(rt) => rt.block_on(self.detect(url)),
            Err(_) => url.to_owned(),
        }
    }
}

/// `POSTMAN_INSECURE_TLS=1` — сертификат сервера не проверяется (только стенд, `docs/STALWART.md`). Плагины читают
/// ту же переменную.
pub fn insecure_tls() -> bool {
    std::env::var("POSTMAN_INSECURE_TLS").is_ok_and(|v| v == "1")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Путь для теста: своя схема `<имя>://`, по сети отвечает на адрес `<имя>-host`; у пути `x` областей нет —
    /// их ищут на узле адреса.
    struct Mail(&'static str);

    impl MailPlugin for Mail {
        fn name(&self) -> &'static str {
            self.0
        }
        fn claims(&self, url: &str) -> bool {
            url.starts_with(&format!("{}://", self.0))
        }
        fn probe<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Option<String>> {
            Box::pin(async move { (url == format!("{}-host", self.0)).then(|| format!("{}://{url}", self.0)) })
        }
        fn area_host(&self, url: &str) -> Option<String> {
            (self.0 == "x").then(|| url.trim_start_matches("x://").split('?').next().unwrap_or_default().to_owned())
        }
        fn has_area(&self, url: &str, key: &str) -> bool {
            url.contains(&format!("{key}="))
        }
        fn with_area(&self, url: &str, key: &str, area: &str) -> String {
            format!("{url}?{key}={area}")
        }
        fn spawn(&self, _s: Spawn) -> UnboundedSender<Command> {
            tokio::sync::mpsc::unbounded_channel().0
        }
    }

    struct Area;

    impl AreaPlugin for Area {
        fn key(&self) -> &'static str {
            "a"
        }
        fn probe<'a>(&'a self, host: &'a str) -> BoxFuture<'a, Option<String>> {
            Box::pin(async move { (host == "x-host").then(|| "https://x-host".to_owned()) })
        }
        fn connect<'a>(&'a self, _url: &'a str, _login: &'a Login) -> BoxFuture<'a, Result<Box<dyn Areas>, Error>> {
            Box::pin(async { Err(Error::Protocol("no areas in tests".into())) })
        }
    }

    #[test]
    fn detect_asks_plugins_in_order() {
        let r = Registry::default().with_mail(Mail("j")).with_mail(Mail("x")).with_area(Area);
        assert!(r.claims("x://h") && !r.claims("https://h"));
        assert_eq!(r.detect_blocking("j-host"), "j://j-host", "первый ответивший путь");
        assert_eq!(r.detect_blocking("x-host"), "x://x-host?a=https://x-host", "области — на узле учётной записи");
        assert_eq!(r.detect_blocking("x://x-host?a=https://y"), "x://x-host?a=https://y", "адрес области уже есть");
        assert_eq!(r.detect_blocking("x://other"), "x://other", "на узле областей нет");
        assert_eq!(r.detect_blocking("nobody"), "nobody");
        assert_eq!(r.mail_for("x://h").map(|p| p.name()), Some("x"));
        assert_eq!(r.mail_for("https://h").map(|p| p.name()), Some("j"), "путь по умолчанию — первый");
        assert!(Registry::default().mail_for("https://h").is_none());
    }

    #[test]
    fn no_backend_is_an_error_not_a_panic() {
        let (tx, rx) = mpsc::channel();
        let account = Account { url: "https://h".into(), auth: crate::model::Auth::Bearer("t".into()) };
        let _commands = Registry::default().spawn(account, None, Cache::open_in_memory().unwrap(), None, tx, || {});
        assert!(matches!(rx.try_recv(), Ok(Event::Error(e)) if e.kind == crate::model::ErrorKind::Internal));
    }
}
