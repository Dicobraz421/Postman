//! Договор канала на Rust (ADR 0012 п. 3): трейты dyn-совместимы, футуры — [`BoxFuture`] без `Send` (поток учётки
//! однопоточный), реестр собирается из разнородных плагинов (`backend::Registry::with_channel`).

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Mutex;

use postman_cache::{Cache, LocalPut};

use super::{AccountId, ChannelError, ChannelFeatures, ChannelId, ConversationId, Cursor, MessageId, Page, Participant, Secret};
use super::{Action, Address, AttachmentSource, Done, FileMeta, Uploaded, encode_segment};
use crate::backend::BoxFuture;

/// Куда канал шлёт события.
pub type Incoming = tokio::sync::mpsc::UnboundedSender<super::ChannelEvent>;

/// Канал по адресу учётки и входу. Регистрируется в `backend::Registry::with_channel`.
pub trait ChannelPlugin: Send + Sync {
    /// `telegram`, `matrix`, `demo` — часть адреса `postman://` и ключа политики; `mail` занят адаптером почты.
    fn id(&self) -> &'static str;
    /// Что канал может уметь вообще — для «Настройки → Каналы» до входа. После входа — [`ChannelSession::features`].
    fn features(&self) -> ChannelFeatures;
    /// Ссылка канала (`https://t.me/c/…`, `https://matrix.to/#/…`) → адрес Postman: открыть у себя.
    fn parse_link(&self, _url: &str) -> Option<Address> {
        None
    }
    /// Обратно к [`ChannelPlugin::parse_link`]: адрес беседы или сообщения → ссылка канала наружу (в письмо, в другой
    /// канал, ADR 0012 п. 4). Нет — наружу уходит `postman://`.
    fn link(&self, _at: &Address) -> Option<String> {
        None
    }
    /// Ссылка на человека в канале для карточки контакта (`https://t.me/<имя>`, `matrix:u/<user>:<server>`; JSContact
    /// `onlineServices`, vCard `IMPP`, ADR 0012 п. 4). Нет — связь «участник = контакт» только в базе профиля.
    fn person_url(&self, _p: &Participant) -> Option<String> {
        None
    }
    /// Вход: шаги с вопросами пользователю; итог — [`Prompt::Done`] с секретом сессии (его хранит ядро).
    fn sign_in<'a>(&'a self, ctx: FlowCtx) -> BoxFuture<'a, Result<Box<dyn Flow + 'a>, ChannelError>>;
    /// Подключиться с сохранённой сессией; события — в `ctx.incoming`.
    fn connect<'a>(&'a self, ctx: ConnectCtx) -> BoxFuture<'a, Result<Box<dyn ChannelSession>, ChannelError>>;
}

/// Что ядро даёт каналу при подключении. Всё владеющее: сессия живёт дольше вызова `connect`.
pub struct ConnectCtx {
    pub account: AccountId,
    /// Байты сессии из хранилища секретов ОС (ключ авторизации Telegram, токен и устройство Matrix).
    pub secret: Secret,
    pub incoming: Incoming,
    /// Своё постоянное состояние канала — в таблице `local` профиля, ключи `chan/<канал>/<учётка>/…`.
    pub store: Rc<dyn PluginStore>,
}

pub struct FlowCtx {
    pub store: Rc<dyn PluginStore>,
}

/// Подключённая учётка канала. Методы — `&self`: ядро держит несколько вызовов сразу (большая загрузка не задерживает
/// отправку).
pub trait ChannelSession {
    fn me(&self) -> Participant;
    fn features(&self) -> ChannelFeatures;
    /// Догнать сервер от курсора (`None` — первый вход): беседы и их последние сообщения — событиями, в конце —
    /// `ChannelEvent::Cursor`. Дальше события идут сами, пока жива связь.
    fn catch_up<'a>(&'a self, cursor: Option<Cursor>) -> BoxFuture<'a, Result<(), ChannelError>>;
    /// История беседы до сообщения `before` (`None` — с конца): прокрутка ленты вверх, «Медиа беседы», поиск.
    fn history<'a>(&'a self, conv: &'a ConversationId, before: Option<&'a MessageId>, limit: u32) -> BoxFuture<'a, Result<Page, ChannelError>>;
    /// Поиск на сервере — дополнение к своему индексу, когда беседа загружена не вся (`features.server_search`).
    fn search<'a>(&'a self, query: &'a str, conv: Option<&'a ConversationId>, limit: u32) -> BoxFuture<'a, Result<Page, ChannelError>>;
    /// Действие пользователя (отправка, правка, удаление, реакция, отметки…).
    fn act<'a>(&'a self, action: Action) -> BoxFuture<'a, Result<Done, ChannelError>>;
    /// Байты вложения — в `out` кусками (ядро считает SHA-256 и кладёт в хранилище содержимого).
    fn download<'a>(&'a self, src: &'a AttachmentSource, out: &'a mut dyn ByteSink) -> BoxFuture<'a, Result<(), ChannelError>>;
    /// Выгрузить файл перед отправкой; байты ядро читает из хранилища — с источника заново не скачиваются.
    fn upload<'a>(&'a self, meta: &'a FileMeta, src: &'a mut dyn ByteSource) -> BoxFuture<'a, Result<Uploaded, ChannelError>>;
    /// Особый поток вопросов канала после входа: проверка устройства, ключ резервной копии (Matrix).
    fn flow<'a>(&'a self, kind: FlowKind) -> BoxFuture<'a, Result<Box<dyn Flow + 'a>, ChannelError>>;
    fn sign_out<'a>(&'a self) -> BoxFuture<'a, Result<(), ChannelError>>;
}

/// Шаги входа и проверок: вопрос → ответ → следующий вопрос. Тексты вопросов — у интерфейса (Fluent).
pub trait Flow {
    /// `None` — начать; иначе ответ на прошлый вопрос.
    fn step<'a>(&'a mut self, answer: Option<Answer>) -> BoxFuture<'a, Result<Prompt, ChannelError>>;
}

/// Откуда код входа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeVia {
    App,
    Sms,
    Call,
    Email,
}

/// Вопрос пользователю.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    /// Адрес сервера (Matrix).
    Server,
    /// Номер телефона (Telegram).
    Phone,
    /// Имя и пароль (Matrix).
    Login,
    /// Код из приложения, SMS, звонка, почты.
    Code {
        via: CodeVia,
        length: Option<u8>,
    },
    /// Пароль двухэтапной проверки.
    Password {
        hint: Option<String>,
    },
    /// Вход с другого устройства.
    Qr {
        url: String,
        expires: i64,
    },
    /// SSO (Matrix): открыть в браузере.
    Browser {
        url: String,
    },
    /// Сверка устройств (SAS): символ и подпись.
    Compare {
        emoji: Vec<(String, String)>,
    },
    /// Ключ резервной копии ключей.
    RecoveryKey,
    Done {
        account: AccountId,
        name: String,
        secret: Secret,
    },
    Finished,
}

/// Ответ пользователя. Пароль и коды не сохраняются нигде.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Text(String),
    Secret(Secret),
    Confirm(bool),
    Resend,
}

/// Поток вопросов после входа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowKind {
    /// Проверка устройства (Matrix SAS).
    Verify,
    /// Ключ резервной копии ключей.
    Recovery,
}

/// Состояние канала в базе профиля: курсор Matrix, хранилище ключей сквозного шифрования, состояние обновлений
/// Telegram сверх курсора. Синхронно: redb в потоке учётки.
pub trait PluginStore {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, ChannelError>;
    fn put(&self, key: &str, value: &[u8]) -> Result<(), ChannelError>;
    fn remove_prefix(&self, prefix: &str) -> Result<(), ChannelError>;
}

pub trait ByteSink {
    fn write(&mut self, chunk: &[u8]) -> Result<(), ChannelError>;
}

pub trait ByteSource {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, ChannelError>;
    fn len(&self) -> u64;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// [`PluginStore`] над таблицей `local` профиля: ключи `chan/<канал>/<учётка>/<ключ канала>` (решение «всё в базе»).
pub struct LocalStore {
    cache: Cache,
    prefix: String,
}

impl LocalStore {
    pub fn new(cache: Cache, channel: &ChannelId, account: &AccountId) -> Self {
        Self { cache, prefix: Self::prefix(channel, account) }
    }

    /// Префикс ключей учётки: части — в процент-кодировке, `/` в id учётки не смешает учётки.
    pub fn prefix(channel: &ChannelId, account: &AccountId) -> String {
        format!("chan/{}/{}/", encode_segment(channel.as_str()), encode_segment(account.as_str()))
    }
}

impl PluginStore for LocalStore {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, ChannelError> {
        Ok(self.cache.local(&format!("{}{key}", self.prefix))?.map(|d| d.value))
    }

    fn put(&self, key: &str, value: &[u8]) -> Result<(), ChannelError> {
        Ok(self.cache.put_local(&LocalPut { key: format!("{}{key}", self.prefix), value: value.to_vec(), parts: Vec::new() })?)
    }

    fn remove_prefix(&self, prefix: &str) -> Result<(), ChannelError> {
        self.cache.remove_local_prefix(&format!("{}{prefix}", self.prefix))?;
        Ok(())
    }
}

/// Хранилище секретов ОС для ядра (ADR 0012 п. 11): даёт композиционный корень (`pets_shell::credentials`), запись —
/// [`Secrets::name`]. Канал секретов не видит, кроме своего `ConnectCtx::secret`.
pub trait Secrets: Send + Sync {
    fn get(&self, channel: &ChannelId, account: &AccountId) -> Option<Secret>;
    fn put(&self, channel: &ChannelId, account: &AccountId, secret: &Secret) -> Result<(), ChannelError>;
    fn remove(&self, channel: &ChannelId, account: &AccountId) -> Result<(), ChannelError>;

    /// Имя записи в хранилище ОС.
    fn name(channel: &ChannelId, account: &AccountId) -> String
    where
        Self: Sized,
    {
        format!("Postman channel {channel} {account}")
    }
}

/// Секреты в памяти: нет хранилища ОС — вход каждый раз (ADR 0011 п. 4); тесты.
#[derive(Default)]
pub struct MemorySecrets(Mutex<HashMap<(String, String), Secret>>);

impl Secrets for MemorySecrets {
    fn get(&self, channel: &ChannelId, account: &AccountId) -> Option<Secret> {
        self.0.lock().ok()?.get(&(channel.0.clone(), account.0.clone())).cloned()
    }

    fn put(&self, channel: &ChannelId, account: &AccountId, secret: &Secret) -> Result<(), ChannelError> {
        let mut m = self.0.lock().map_err(|_| ChannelError::Storage("secrets lock".into()))?;
        m.insert((channel.0.clone(), account.0.clone()), secret.clone());
        Ok(())
    }

    fn remove(&self, channel: &ChannelId, account: &AccountId) -> Result<(), ChannelError> {
        let mut m = self.0.lock().map_err(|_| ChannelError::Storage("secrets lock".into()))?;
        m.remove(&(channel.0.clone(), account.0.clone()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Свои данные канала — под префиксом учётки; удаление по префиксу не задевает соседей.
    #[test]
    fn local_store_is_scoped() {
        let cache = Cache::open_in_memory().unwrap();
        let a = LocalStore::new(cache.clone(), &ChannelId::new("demo"), &AccountId::new("a/b@example.test"));
        let b = LocalStore::new(cache.clone(), &ChannelId::new("demo"), &AccountId::new("a"));
        a.put("state/pts", b"1").unwrap();
        a.put("keys", b"k").unwrap();
        b.put("state/pts", b"2").unwrap();
        assert_eq!(a.get("state/pts").unwrap().as_deref(), Some(&b"1"[..]));
        assert!(cache.local("chan/demo/a%2Fb@example.test/keys").unwrap().is_some());
        a.remove_prefix("state/").unwrap();
        assert_eq!(a.get("state/pts").unwrap(), None);
        assert!(a.get("keys").unwrap().is_some());
        assert_eq!(b.get("state/pts").unwrap().as_deref(), Some(&b"2"[..]));
        assert_eq!(MemorySecrets::name(&ChannelId::new("demo"), &AccountId::new("a")), "Postman channel demo a");
    }
}
