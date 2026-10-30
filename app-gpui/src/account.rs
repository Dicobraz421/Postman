//! Учётная запись (S3-04): откуда она берётся при запуске, где лежит её кэш и что хранится на диске.
//!
//! На диске — только несекретная часть (`account.json`: адрес сервера и логин); пароль — в диспетчере учётных
//! данных Windows (`sys::credentials`), цель `Postman:<адрес>:<логин>`. Токен (`POSTMAN_TOKEN`) — только из
//! окружения, не сохраняется.

use std::path::{Path, PathBuf};

use postman_core::model::Auth;
use postman_core::sync::Account;
use serde::{Deserialize, Serialize};

use crate::sys;

const SETTINGS_FILE: &str = "account.json";

/// Несекретная часть учётной записи: её можно писать на диск.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Settings {
    pub(crate) url: String,
    pub(crate) user: String,
    /// Вход учётной записью Windows без пароля (T1-02б): пароль не спрашивается и не хранится.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) windows: bool,
}

impl Settings {
    pub(crate) fn of(account: &Account) -> Self {
        let windows = matches!(&account.auth, Auth::Basic { password, .. } if password.is_empty());
        Self { url: account.url.clone(), user: user_of(account).to_owned(), windows }
    }

    /// Учётная запись входа Windows: пустой пароль — путь EWS входит учётными данными текущего входа (T1-02а).
    pub(crate) fn windows_account(&self) -> Account {
        Account { url: self.url.clone(), auth: Auth::Basic { user: self.user.clone(), password: String::new() } }
    }

    pub(crate) fn load(dir: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(dir.join(SETTINGS_FILE)).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub(crate) fn save(&self, dir: &Path) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(dir.join(SETTINGS_FILE), text)
    }

    pub(crate) fn remove(dir: &Path) {
        let _ = std::fs::remove_file(dir.join(SETTINGS_FILE));
    }

    /// Имя пароля в диспетчере учётных данных.
    pub(crate) fn credential_target(&self) -> String {
        format!("Postman:{}:{}", self.url, self.user)
    }
}

/// Логин учётной записи; вход по токену — «token».
pub(crate) fn user_of(account: &Account) -> &str {
    match &account.auth {
        Auth::Basic { user, .. } => user,
        Auth::Bearer(_) => "token",
    }
}

/// Кэш учётной записи: свой файл на пару «сервер + логин» (не `demo.redb`). Индекс поиска — рядом,
/// `<кэш>.search`.
pub(crate) fn cache_path(dir: &Path, account: &Account) -> PathBuf {
    let user = user_of(account);
    let readable: String = user.chars().map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c.to_ascii_lowercase() } else { '_' }).collect();
    dir.join(format!("account-{readable}-{:08x}.redb", fnv(&format!("{}\n{user}", account.url))))
}

/// FNV-1a, 32 бита: устойчивое между запусками имя файла (std-хэш от версии к версии меняется).
fn fnv(s: &str) -> u32 {
    s.bytes().fold(0x811c_9dc5u32, |h, b| (h ^ u32::from(b)).wrapping_mul(0x0100_0193))
}

/// Что сохранено с прошлого входа.
pub(crate) enum Stored {
    Account(Account),
    /// Адрес и логин есть, пароля нет (удалён из диспетчера): строка входа с подставленными полями.
    Settings(Settings),
    None,
}

pub(crate) fn load_stored(dir: &Path) -> Stored {
    let Some(s) = Settings::load(dir) else { return Stored::None };
    // Вход Windows (T1-02б): пароля нет и не нужно.
    if s.windows {
        return Stored::Account(s.windows_account());
    }
    match sys::credentials::read(&s.credential_target()) {
        Ok(Some(password)) => Stored::Account(Account { url: s.url, auth: Auth::Basic { user: s.user, password } }),
        Ok(None) => Stored::Settings(s),
        Err(e) => {
            tracing::warn!("credential manager: {e}");
            Stored::Settings(s)
        }
    }
}

/// Запомнить учётную запись после успешного входа: адрес и логин — в файл, пароль — в диспетчер (`store`) или
/// нигде: тогда прежний пароль этой записи из диспетчера удаляется, при запуске — строка входа с адресом и логином.
pub(crate) fn remember(dir: &Path, account: &Account, store: bool) -> Result<(), String> {
    let s = Settings::of(account);
    // Вход Windows (T1-02б): в диспетчер учётных данных ничего не пишется.
    if let Auth::Basic { password, .. } = &account.auth
        && !s.windows
    {
        if store {
            sys::credentials::write(&s.credential_target(), &s.user, password)?;
        } else {
            sys::credentials::delete(&s.credential_target())?;
        }
    }
    s.save(dir).map_err(|e| format!("{SETTINGS_FILE}: {e}"))
}

/// Пароль учётной записи лежит в диспетчере учётных данных (чтение — не в потоке интерфейса).
pub(crate) fn password_stored(s: &Settings) -> bool {
    matches!(sys::credentials::read(&s.credential_target()), Ok(Some(_)))
}

/// `--sign-out`: забыть сохранённую учётную запись (кэш остаётся).
pub(crate) fn forget(dir: &Path) {
    if let Some(s) = Settings::load(dir)
        && let Err(e) = sys::credentials::delete(&s.credential_target())
    {
        tracing::warn!("credential manager: {e}");
    }
    Settings::remove(dir);
}

/// Дополнительные учётные записи профиля (S7-03б): несекретная часть, как у основной; пароли — в диспетчере учётных
/// данных под теми же именами (`Postman:<адрес>:<логин>`).
const EXTRA_FILE: &str = "accounts.json";

/// Дополнительная учётная запись: префикс её ключей в кэше (`@2`, `@3`…) и адрес с логином.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Extra {
    pub(crate) profile: String,
    #[serde(flatten)]
    pub(crate) settings: Settings,
}

/// Дополнительные учётные записи по порядку добавления; файла нет или он испорчен — нет и их.
pub(crate) fn load_extras(dir: &Path) -> Vec<Extra> {
    std::fs::read_to_string(dir.join(EXTRA_FILE)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save_extras(dir: &Path, list: &[Extra]) -> Result<(), String> {
    let text = serde_json::to_string_pretty(list).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(EXTRA_FILE), text).map_err(|e| format!("{EXTRA_FILE}: {e}"))
}

/// Префикс следующей учётной записи: `@2`, затем на единицу больше самого большого (удалённый номер не занимается
/// снова — его строки в кэше могли остаться у другой копии профиля).
fn next_profile(list: &[Extra]) -> String {
    let max = list.iter().filter_map(|e| e.profile.strip_prefix(postman_cache::PROFILE)?.parse::<u32>().ok()).max().unwrap_or(1);
    format!("{}{}", postman_cache::PROFILE, max + 1)
}

/// Учётная запись для синхронизации: пароль — из диспетчера. Нет пароля — `None` (её покажут «Настройки →
/// Аккаунты», синхронизации нет).
pub(crate) fn extra_account(e: &Extra) -> Option<Account> {
    if e.settings.windows {
        return Some(e.settings.windows_account());
    }
    match sys::credentials::read(&e.settings.credential_target()) {
        Ok(Some(password)) => Some(Account { url: e.settings.url.clone(), auth: Auth::Basic { user: e.settings.user.clone(), password } }),
        Ok(None) => None,
        Err(err) => {
            tracing::warn!("credential manager: {err}");
            None
        }
    }
}

/// Добавить учётную запись к профилю после успешного входа: адрес и логин — в файл, пароль — в диспетчер. Та же
/// учётная запись ещё раз — прежняя запись (новым паролем).
pub(crate) fn add_extra(dir: &Path, account: &Account, store: bool) -> Result<Extra, String> {
    let settings = Settings::of(account);
    if let Auth::Basic { password, .. } = &account.auth
        && !settings.windows
    {
        if store {
            sys::credentials::write(&settings.credential_target(), &settings.user, password)?;
        } else {
            sys::credentials::delete(&settings.credential_target())?;
        }
    }
    let mut list = load_extras(dir);
    if let Some(e) = list.iter().find(|e| e.settings == settings) {
        return Ok(e.clone());
    }
    let extra = Extra { profile: next_profile(&list), settings };
    list.push(extra.clone());
    save_extras(dir, &list)?;
    Ok(extra)
}

/// Убрать дополнительную учётную запись: из файла и пароль из диспетчера (её строки в кэше убирает вызывающий).
pub(crate) fn remove_extra(dir: &Path, profile: &str) -> Result<(), String> {
    let mut list = load_extras(dir);
    if let Some(e) = list.iter().find(|e| e.profile == profile)
        && let Err(err) = sys::credentials::delete(&e.settings.credential_target())
    {
        tracing::warn!("credential manager: {err}");
    }
    list.retain(|e| e.profile != profile);
    save_extras(dir, &list)
}

/// С чем открыть окно.
pub(crate) enum Start {
    /// Демо-ящик: `--demo`, `--bench`, `--screens` без `--live`.
    Demo,
    Live(Account),
    /// Нет учётной записи: строка входа (поля — с прошлого входа, если есть).
    SignIn(Option<Settings>),
}

/// Флаги запуска, от которых зависит выбор.
#[derive(Clone, Copy, Default)]
pub(crate) struct Flags {
    pub(crate) demo: bool,
    pub(crate) bench: bool,
    pub(crate) screens: bool,
    pub(crate) live: bool,
}

/// Выбор при запуске: замер живого ящика — из окружения; демо по флагам; иначе окружение (`POSTMAN_URL`…), затем сохранённый вход, иначе — строка
/// входа. `--screens --live` — только из окружения (агенты снимают живой ящик стенда), без него — строка входа.
pub(crate) fn choose(flags: Flags, env: Option<Account>, stored: impl FnOnce() -> Stored) -> Start {
    // Замер живого ящика (`--bench --live`, S4-03) — только из окружения, как снимки.
    if flags.bench && flags.live {
        return env.map_or(Start::SignIn(None), Start::Live);
    }
    if flags.bench || flags.demo || (flags.screens && !flags.live) {
        return Start::Demo;
    }
    if let Some(a) = env {
        return Start::Live(a);
    }
    // Снимки без учётной записи в окружении — строка входа, без сохранённого входа (повторяемо).
    if flags.screens {
        return Start::SignIn(None);
    }
    match stored() {
        Stored::Account(a) => Start::Live(a),
        Stored::Settings(s) => Start::SignIn(Some(s)),
        Stored::None => Start::SignIn(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn basic(url: &str, user: &str, password: &str) -> Account {
        Account { url: url.into(), auth: Auth::Basic { user: user.into(), password: password.into() } }
    }

    #[test]
    fn settings_round_trip_without_secrets() {
        let dir = tempfile::Builder::new().prefix("postman-").tempdir().unwrap();
        let account = basic("http://localhost:18080", "user1@itt.test", "not-a-real-secret");
        let s = Settings::of(&account);
        s.save(dir.path()).unwrap();
        assert_eq!(Settings::load(dir.path()), Some(s.clone()));
        let text = std::fs::read_to_string(dir.path().join(SETTINGS_FILE)).unwrap();
        assert!(!text.contains("not-a-real-secret"), "password on disk: {text}");
        assert_eq!(s.credential_target(), "Postman:http://localhost:18080:user1@itt.test");
        Settings::remove(dir.path());
        assert_eq!(Settings::load(dir.path()), None);
    }

    #[test]
    fn extras_get_their_own_prefix() {
        let s = |u: &str| Settings { url: "http://a".into(), user: u.into(), windows: false };
        assert_eq!(next_profile(&[]), "@2");
        let list = vec![Extra { profile: "@2".into(), settings: s("a") }, Extra { profile: "@5".into(), settings: s("b") }];
        assert_eq!(next_profile(&list), "@6");
        let dir = tempfile::Builder::new().prefix("postman-").tempdir().unwrap();
        save_extras(dir.path(), &list).unwrap();
        assert_eq!(load_extras(dir.path()), list);
        let text = std::fs::read_to_string(dir.path().join(EXTRA_FILE)).unwrap();
        assert!(text.contains("\"profile\": \"@2\"") && text.contains("\"user\": \"a\""), "{text}");
        std::fs::write(dir.path().join(EXTRA_FILE), "[ broken").unwrap();
        assert!(load_extras(dir.path()).is_empty());
    }

    #[test]
    fn broken_settings_file_is_ignored() {
        let dir = tempfile::Builder::new().prefix("postman-").tempdir().unwrap();
        std::fs::write(dir.path().join(SETTINGS_FILE), "{ not json").unwrap();
        assert_eq!(Settings::load(dir.path()), None);
    }

    #[test]
    fn cache_is_per_account() {
        let dir = Path::new("data");
        let a = cache_path(dir, &basic("http://a", "user1@itt.test", "x"));
        assert_eq!(a, cache_path(dir, &basic("http://a", "user1@itt.test", "other password")));
        assert_ne!(a, cache_path(dir, &basic("http://b", "user1@itt.test", "x")));
        assert_ne!(a, cache_path(dir, &basic("http://a", "user2@itt.test", "x")));
        let name = a.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("account-user1@itt.test-".replace('@', "_").as_str()), "{name}");
        assert!(name.ends_with(".redb") && name != "demo.redb");
        let token = Account { url: "http://a".into(), auth: Auth::Bearer("t".into()) };
        assert!(cache_path(dir, &token).to_string_lossy().contains("account-token-"));
    }

    #[test]
    fn start_mode() {
        let env = || Some(basic("http://a", "u", "p"));
        let none = || Stored::None;
        let f = |demo, bench, screens, live| Flags { demo, bench, screens, live };
        assert!(matches!(choose(f(true, false, false, false), env(), none), Start::Demo));
        assert!(matches!(choose(f(false, true, false, false), env(), none), Start::Demo));
        assert!(matches!(choose(f(false, true, false, true), env(), || unreachable!()), Start::Live(_)));
        assert!(matches!(choose(f(false, true, false, true), None, || unreachable!()), Start::SignIn(None)));
        assert!(matches!(choose(f(false, false, true, false), env(), none), Start::Demo));
        assert!(matches!(choose(f(false, false, true, true), env(), none), Start::Live(_)));
        assert!(matches!(choose(f(false, false, true, true), None, || unreachable!()), Start::SignIn(None)));
        assert!(matches!(choose(f(false, false, false, false), env(), || unreachable!()), Start::Live(_)));
        assert!(matches!(choose(f(false, false, false, false), None, none), Start::SignIn(None)));
        let partial = || Stored::Settings(Settings { url: "http://a".into(), user: "u".into(), windows: false });
        assert!(matches!(choose(f(false, false, false, false), None, partial), Start::SignIn(Some(_))));
        let saved = || Stored::Account(basic("http://a", "u", "p"));
        assert!(matches!(choose(f(false, false, false, false), None, saved), Start::Live(_)));
    }
}

#[cfg(test)]
mod windows_tests {
    use postman_core::model::Auth;
    use postman_core::sync::Account;

    use super::Settings;

    /// Вход Windows (T1-02б): пустой пароль — признак в настройках; при запуске — учётная запись без пароля, без
    /// диспетчера; файл прежних настроек (без признака) читается как вход с паролем.
    #[test]
    fn windows_sign_in_keeps_no_password() {
        let a = Account {
            url: "https://mail.corp.test/EWS/Exchange.asmx".into(),
            auth: Auth::Basic { user: "a@corp.test".into(), password: String::new() },
        };
        let s = Settings::of(&a);
        assert!(s.windows);
        assert!(matches!(s.windows_account().auth, Auth::Basic { password, .. } if password.is_empty()));
        let dir = tempfile::Builder::new().prefix("postman-win-").tempdir().unwrap();
        super::remember(dir.path(), &a, true).unwrap();
        assert!(
            matches!(super::load_stored(dir.path()), super::Stored::Account(Account { auth: Auth::Basic { password, .. }, .. }) if password.is_empty())
        );
        let old: Settings = serde_json::from_str(r#"{"url": "u", "user": "x"}"#).unwrap();
        assert!(!old.windows);
        assert!(!serde_json::to_string(&old).unwrap().contains("windows"), "признак — только у входа Windows");
    }
}
