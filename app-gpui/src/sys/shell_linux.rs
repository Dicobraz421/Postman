//! Связь с Linux для «Настройки → Общие» и окна (LIN-03, стандарты freedesktop): клиент `mailto:` по умолчанию и
//! автозапуск (`postman.desktop` пакета LIN-06, `xdg-settings`/`xdg-mime`, `~/.config/autostart` — `pets_shell`), журнал в файл
//! (`$XDG_STATE_HOME/Postman` — путь даёт `postman_core::paths::log_dir`), «Открыть» (`xdg-open`), «Показать в папке»
//! (`org.freedesktop.FileManager1`, иначе `xdg-open` каталога), уведомления (`org.freedesktop.Notifications`),
//! политики администратора (`/etc/postman/policies.json`), шифрование диска (LUKS, `findmnt` и `lsblk`), вход без
//! пароля — билет Kerberos MIT (`klist`).

use std::collections::HashMap;
use std::path::Path;

use zbus::zvariant::Value;

/// Подсказка «сделать клиентом `mailto:`» (PAR-04 №41): `xdg-settings` назначает сразу.
pub const MAILTO_HINT: &str = "settings-mailto-hint-now";

/// Схема ссылок почты (`MimeType=x-scheme-handler/mailto` в `scripts/linux/data/postman.desktop`, LIN-06).
const MAILTO: &str = "mailto";

/// Политики администратора (ADR 0011 п. 5): те же имена и типы, что `Software\Policies\Pets\Postman` на Windows и
/// профиль конфигурации на macOS. Файл — JSON-объект, как у политик Firefox и Chromium в `/etc`.
pub const POLICY_FILE: &str = "/etc/postman/policies.json";

/// Шифрование диска с данными (S8-35): LUKS — среди устройств под файловой системой каталога есть `crypt`
/// (`findmnt --target`, затем `lsblk --inverse`). Имя диска — устройство файловой системы. `None` — не ответил.
/// Звать в фоне.
pub fn bitlocker(path: &Path) -> (String, Option<bool>) {
    let source = run("findmnt", &["-n", "-o", "SOURCE", "--target", &path.to_string_lossy()]).map(|s| mount_source(&s));
    let Some(source) = source.filter(|s| s.starts_with("/dev/")) else {
        tracing::debug!("disk encryption: no block device under the data folder");
        return ("/".to_owned(), None);
    };
    let state = run("lsblk", &["-s", "-n", "-o", "TYPE", &source]).map(|types| types.lines().any(|t| t.trim() == "crypt"));
    if state.is_none() {
        tracing::warn!("disk encryption: lsblk did not answer");
    }
    (source, state)
}

/// Источник из `findmnt`: `/dev/mapper/luks-1[/@home]` (btrfs, подтом) → `/dev/mapper/luks-1`.
fn mount_source(s: &str) -> String {
    let s = s.lines().next().unwrap_or_default().trim();
    s.split_once('[').map_or(s, |(dev, _)| dev).to_owned()
}

/// Включить шифрование готового раздела нельзя (LUKS ставится при установке системы): общих настроек во freedesktop
/// нет, кнопки в «Безопасности» на Linux нет (`settings_security`).
pub fn open_bitlocker() -> Result<(), String> {
    Err("disk encryption settings are not available on Linux".into())
}

/// UPN для входа без пароля: принципал билета Kerberos (`klist` MIT: `Default principal: anna@CORP.EXAMPLE` →
/// `anna@corp.example`). Звать в фоне.
pub fn upn() -> Option<String> {
    let klist = run(KLIST, &[])?;
    kerberos_principal(&klist).map(|p| upn_of(&p))
}

/// Домен входа без пароля (PAR-06 №24): область действующего билета Kerberos (`klist -s`, принципал `klist`)
/// строчными; нет билета — `None`. Без кэша.
pub fn domain() -> Option<String> {
    if !in_domain() {
        return None;
    }
    let principal = kerberos_principal(&run(KLIST, &[])?)?;
    principal.rsplit_once('@').map(|(_, realm)| realm.to_lowercase())
}

/// Вход без пароля имеет смысл: есть действующий билет Kerberos (`klist -s`). Без кэша: билет появляется и
/// истекает во время работы (PAR-AUDIT №24); процесс — единицы миллисекунд.
pub fn in_domain() -> bool {
    let Ok(mut klist) = super::system_command(KLIST) else { return false };
    klist.arg("-s").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().is_ok_and(|s| s.success())
}

/// `klist` MIT — `/usr/bin/klist` ([`super::system_command`]), не из `PATH`.
const KLIST: &str = "klist";

/// Вывод системной утилиты (`/usr/bin/<cmd>`, PAR-04 №36) при успехе.
fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = super::system_command(cmd).ok()?.args(args).stderr(std::process::Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Принципал кэша из вывода `klist` (MIT): строка `Default principal: user@REALM`.
fn kerberos_principal(klist: &str) -> Option<String> {
    klist.lines().find_map(|l| l.trim().strip_prefix("Default principal:")).map(str::trim).filter(|p| p.contains('@')).map(str::to_owned)
}

/// `user@REALM` → `user@realm`.
fn upn_of(principal: &str) -> String {
    match principal.rsplit_once('@') {
        Some((user, realm)) => format!("{user}@{}", realm.to_lowercase()),
        None => principal.to_owned(),
    }
}

/// Открывать Postman при входе: `~/.config/autostart/postman.desktop`. Выключенный пользователем в настройках среды —
/// `set(true)` включает его снова.
///
/// Запуск при входе — сразу в трей, как на Windows (PAR-06, №40): в строку `Exec` записи добавляется `--minimized`.
pub fn set_autostart(on: bool) -> Result<(), String> {
    pets_shell::login_item::set(on).map_err(|e| e.to_string())?;
    if !on {
        return Ok(());
    }
    let id = pets_shell::url_scheme::bundle_id().unwrap_or_else(|| "postman.desktop".to_owned());
    let Some(path) = postman_core::paths::config_home().map(|c| c.join("autostart").join(id)) else { return Ok(()) };
    let text = std::fs::read_to_string(&path).map_err(|e| format!("autostart read: {e}"))?;
    let minimized = with_minimized(&text);
    if minimized != text {
        std::fs::write(&path, minimized).map_err(|e| format!("autostart write: {e}"))?;
    }
    Ok(())
}

/// Строка `Exec=` записи автозапуска с `--minimized` в конце (уже есть — без изменений).
fn with_minimized(text: &str) -> String {
    text.lines()
        .map(|l| match l.strip_prefix("Exec=") {
            Some(cmd) if !cmd.split_whitespace().any(|a| a == "--minimized") => format!("Exec={cmd} --minimized"),
            _ => l.to_owned(),
        })
        .map(|l| l + "\n")
        .collect()
}

/// Postman открывается при входе.
pub fn autostart() -> bool {
    pets_shell::login_item::state() == pets_shell::login_item::State::On
}

/// Сделать Postman приложением по умолчанию для `mailto:` (`xdg-settings`, запасной — `xdg-mime`). Нужен
/// установленный `postman.desktop` (пакет, LIN-06); без него — ошибка.
pub fn register_mailto() -> Result<(), String> {
    pets_shell::url_scheme::set_default(MAILTO).map_err(|e| e.to_string())
}

/// Выбор делает `register_mailto` сразу: окна «Приложения по умолчанию» открывать не нужно (как на macOS).
pub fn show_default_apps() -> Result<(), String> {
    Ok(())
}

/// Ссылки `mailto:` открывает Postman.
pub fn mailto_is_default() -> bool {
    pets_shell::url_scheme::is_default(MAILTO)
}

/// Политика организации из [`POLICY_FILE`]; нет файла или ключа — `None`.
pub fn policy(name: &str) -> Option<String> {
    static FILE: std::sync::OnceLock<HashMap<String, String>> = std::sync::OnceLock::new();
    FILE.get_or_init(|| read_policies(Path::new(POLICY_FILE))).get(name).cloned()
}

/// Файл политик: нет — пусто; не читается, не JSON-объект или его может править не только root — WARN и пусто
/// (политика только строже, но поддельный файл не должен ничего закреплять).
fn read_policies(path: &Path) -> HashMap<String, String> {
    use std::os::unix::fs::MetadataExt;
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return HashMap::new(),
        Err(e) => {
            tracing::warn!(path = %path.display(), "policy: file not read: {e}");
            return HashMap::new();
        }
    };
    if !meta.is_file() || meta.uid() != 0 || meta.mode() & 0o022 != 0 {
        tracing::warn!(path = %path.display(), "policy: file must be a regular file owned by root and not writable by others, ignored");
        return HashMap::new();
    }
    match std::fs::read_to_string(path) {
        Ok(text) => parse_policies(&text),
        Err(e) => {
            tracing::warn!(path = %path.display(), "policy: file not read: {e}");
            HashMap::new()
        }
    }
}

/// JSON-объект → значения строками, как из реестра (ADR 0011 п. 5): строка — как есть, целое и дробное — числом
/// (`1`, `1.3`), логическое — `1`/`0`. Массив, объект, `null` — WARN, ключ пропущен.
fn parse_policies(text: &str) -> HashMap<String, String> {
    let map = match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Object(map)) => map,
        Ok(_) => {
            tracing::warn!("policy: file is not a JSON object, ignored");
            return HashMap::new();
        }
        Err(e) => {
            tracing::warn!("policy: file is not valid JSON, ignored: {e}");
            return HashMap::new();
        }
    };
    map.into_iter()
        .filter_map(|(name, v)| {
            let value = match v {
                serde_json::Value::String(s) => s,
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => if b { "1" } else { "0" }.to_owned(),
                _ => {
                    tracing::warn!(policy = name, "policy: unsupported value type, not applied");
                    return None;
                }
            };
            Some((name, value))
        })
        .collect()
}

/// Запуск без терминала (из меню среды, stderr — `/dev/null`): поток ошибок — в файл журнала, туда пишет `tracing`.
/// stderr — терминал или systemd-journald — ничего. `true` — журнал пишется в файл.
pub fn log_to_file(path: &Path) -> bool {
    match pets_shell::console::stderr_to_file(path) {
        Ok(to_file) => to_file,
        Err(e) => {
            tracing::warn!("log: stderr not redirected to the log file: {e}");
            false
        }
    }
}

/// Уровень журнала по умолчанию (`POSTMAN_LOG`), если его не задали снаружи. Вызывать до потоков.
pub fn default_env(name: &str, value: &str) {
    if std::env::var_os(name).is_none() {
        // SAFETY: в начале `main`, до запуска потоков (Rust 2024: `set_var` — unsafe).
        unsafe { std::env::set_var(name, value) };
    }
}

/// Открыть путь или адрес приложением по умолчанию (`xdg-open`) — по действию пользователя.
pub fn open(target: &str) -> Result<(), String> {
    super::system_command("xdg-open")?.arg(target).spawn().map(|_| ()).map_err(|e| format!("xdg-open: {e}"))
}

/// Показать файл в файловом менеджере с выделением (`org.freedesktop.FileManager1.ShowItems`); службы нет —
/// открыть его каталог (`xdg-open`).
pub fn reveal(path: &Path) -> Result<(), String> {
    let uri = file_uri(path);
    let shown = zbus::block_on(async {
        let conn = zbus::Connection::session().await?;
        conn.call_method(
            Some("org.freedesktop.FileManager1"),
            "/org/freedesktop/FileManager1",
            Some("org.freedesktop.FileManager1"),
            "ShowItems",
            &(vec![uri.as_str()], ""),
        )
        .await
        .map(|_| ())
    });
    match shown {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::debug!("reveal: FileManager1 not available, opening the folder: {e}");
            open(&path.parent().unwrap_or(path).to_string_lossy())
        }
    }
}

/// `file://` с процентной записью всего, кроме безопасных символов пути.
fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            uri.push(char::from(b));
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri
}

/// Уведомление о новом письме: со звуком (`sound-name` `message-new-email` темы звуков freedesktop) или молча
/// (`suppress-sound`).
pub fn notify_with(title: &str, body: &str, sound: bool) -> Result<(), String> {
    let hints = hints(sound);
    zbus::block_on(async {
        let conn = zbus::Connection::session().await?;
        conn.call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &("Postman", 0u32, "postman", title, body, Vec::<&str>::new(), hints, -1i32),
        )
        .await
        .map(|_| ())
    })
    .map_err(|e| format!("notification: {e}"))
}

/// Уведомление со щелчком (PAR-06, как T3-13 на Windows и MAC-04): действие `default` — щелчок по уведомлению;
/// `on_click` — из своего потока, пока уведомление не закрыто. `sound`: `None` и `Some(true)` — со звуком,
/// `Some(false)` — молча. Служба без действий (`actions` нет в `GetCapabilities`) — уведомление без щелчка.
pub fn notify_click(title: &str, body: &str, sound: Option<bool>, on_click: impl Fn() + Send + 'static) -> Result<(), String> {
    let (title, body, label) = (title.to_owned(), body.to_owned(), pets_i18n::tr!("tray-open"));
    let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let spawned = std::thread::Builder::new().name("notification".into()).spawn(move || {
        zbus::block_on(async move {
            use futures::StreamExt as _;
            let shown: zbus::Result<_> = async {
                let conn = zbus::Connection::session().await?;
                let proxy =
                    zbus::Proxy::new(&conn, "org.freedesktop.Notifications", "/org/freedesktop/Notifications", "org.freedesktop.Notifications")
                        .await?;
                // Подписка — до показа: щелчок сразу после показа не теряется.
                let actions = proxy.receive_signal("ActionInvoked").await?;
                let closed = proxy.receive_signal("NotificationClosed").await?;
                let hints = hints(sound != Some(false));
                let reply = proxy
                    .call_method(
                        "Notify",
                        &("Postman", 0u32, "postman", title.as_str(), body.as_str(), vec!["default", label.as_str()], hints, -1i32),
                    )
                    .await?;
                let id: u32 = reply.body().deserialize()?;
                Ok((conn, id, actions, closed))
            }
            .await;
            let (_conn, id, actions, closed) = match shown {
                Ok(v) => {
                    let _ = tx.send(Ok(()));
                    v
                }
                Err(e) => {
                    let _ = tx.send(Err(format!("notification: {e}")));
                    return;
                }
            };
            enum Got {
                Click(u32),
                Closed(u32),
            }
            let clicks = actions.filter_map(|m| async move {
                let (nid, action): (u32, String) = m.body().deserialize().ok()?;
                (action == "default").then_some(Got::Click(nid))
            });
            let closes = closed.filter_map(|m| async move { m.body().deserialize::<(u32, u32)>().ok().map(|(nid, _)| Got::Closed(nid)) });
            let mut events = futures::stream::select(Box::pin(clicks), Box::pin(closes));
            while let Some(got) = events.next().await {
                match got {
                    Got::Click(nid) if nid == id => {
                        on_click();
                        return;
                    }
                    Got::Closed(nid) if nid == id => return,
                    _ => {}
                }
            }
        });
    });
    spawned.map_err(|e| format!("notification thread: {e}"))?;
    rx.recv().map_err(|e| format!("notification: {e}"))?
}

/// Подсказки уведомления: файл `.desktop`, звук или тишина.
fn hints(sound: bool) -> HashMap<&'static str, Value<'static>> {
    let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
    hints.insert("desktop-entry", Value::from("postman"));
    if sound {
        hints.insert("sound-name", Value::from("message-new-email"));
    } else {
        hints.insert("suppress-sound", Value::from(true));
    }
    hints
}

#[cfg(test)]
mod tests {
    /// PAR-06: автозапуск — с `--minimized`, один раз.
    #[test]
    fn autostart_minimized() {
        let t = "[Desktop Entry]\nExec=\"/opt/postman/postman\"\nTerminal=false\n";
        let once = super::with_minimized(t);
        assert!(once.contains("Exec=\"/opt/postman/postman\" --minimized\n"));
        assert_eq!(super::with_minimized(&once), once);
    }

    /// `klist` (MIT) → UPN; нет кэша — `None`.
    #[test]
    fn kerberos_principal_mit() {
        let klist = "Ticket cache: KCM:1000\nDefault principal: anna@CORP.EXAMPLE\n\nValid starting     Expires\n";
        assert_eq!(super::kerberos_principal(klist).as_deref(), Some("anna@CORP.EXAMPLE"));
        assert_eq!(super::upn_of("anna@CORP.EXAMPLE"), "anna@corp.example");
        assert_eq!(super::kerberos_principal("klist: No credentials cache found (filename: /tmp/krb5cc_1000)"), None);
    }

    /// Источник `findmnt`: подтом btrfs отрезается.
    #[test]
    fn mount_source() {
        assert_eq!(super::mount_source("/dev/mapper/luks-1[/@home]\n"), "/dev/mapper/luks-1");
        assert_eq!(super::mount_source("/dev/nvme0n1p2\n"), "/dev/nvme0n1p2");
    }

    /// ADR 0011 п. 5: строка, целое, дробное, логическое — строкой, как из реестра; массив — пропущен; не объект и не
    /// JSON — пусто.
    #[test]
    fn policies_json() {
        let p = super::parse_policies(
            r#"{"ExternalImages":"never","StorePasswords":0,"MinTls":1.3,"AllowUpdate":false,"SandboxNamespaces":"required","X":[1]}"#,
        );
        assert_eq!(p.get("ExternalImages").map(String::as_str), Some("never"));
        assert_eq!(p.get("StorePasswords").map(String::as_str), Some("0"));
        assert_eq!(p.get("MinTls").map(String::as_str), Some("1.3"));
        assert_eq!(p.get("AllowUpdate").map(String::as_str), Some("0"));
        assert_eq!(p.get("SandboxNamespaces").map(String::as_str), Some("required"));
        assert!(!p.contains_key("X"));
        assert!(super::parse_policies("[1]").is_empty());
        assert!(super::parse_policies("{").is_empty());
    }

    /// Файл политик, который может править не root, не читается; нет файла — пусто.
    #[test]
    fn policy_file_must_be_root_owned() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policies.json");
        std::fs::write(&path, r#"{"StorePasswords":0}"#).unwrap();
        let owned_by_root = std::fs::metadata(&path).map(|m| std::os::unix::fs::MetadataExt::uid(&m) == 0).unwrap();
        assert_eq!(super::read_policies(&path).is_empty(), !owned_by_root);
        assert!(super::read_policies(&dir.path().join("none.json")).is_empty());
    }

    #[test]
    fn file_uri_is_escaped() {
        assert_eq!(super::file_uri(std::path::Path::new("/home/a/Отчёт 1.pdf")), "file:///home/a/%D0%9E%D1%82%D1%87%D1%91%D1%82%201.pdf");
    }
}
