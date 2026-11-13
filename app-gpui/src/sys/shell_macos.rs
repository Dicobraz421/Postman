//! Связь с macOS для «Настройки → Общие» и окна (MAC-02): клиент `mailto:` по умолчанию (LaunchServices), журнал
//! в файл (`~/Library/Logs/Postman` — путь даёт `postman_core::paths::log_dir`), Finder («Открыть», «Показать в
//! Finder»), уведомления центра уведомлений. MAC-04: запуск при входе (`SMAppService`), политики (профиль
//! конфигурации), FileVault, щелчок по уведомлению. Вызовы ОС — `pets_shell` и `pets_gpui_macos`; здесь — что делает
//! Postman. MAC-05: вход без пароля — UPN и билет Kerberos (`klist`, `dsconfigad`).

use std::path::Path;

use pets_gpui_macos::toast;

use super::PolicyValue;

/// Подсказка «сделать клиентом `mailto:`» (PAR-04 №41): macOS спрашивает подтверждение.
pub const MAILTO_HINT: &str = "settings-mailto-hint-confirm";

/// Схема ссылок почты (объявлена в `Info.plist`, `scripts/macos/package.sh`).
const MAILTO: &str = "mailto";

/// Шифрование диска с данными (S8-35, PAR-04 №23): том, на котором лежат данные (`df` → точка монтирования,
/// `diskutil info` — строка «FileVault»), а не загрузочный: данные на внешнем томе без шифрования — «не зашифрован».
/// Нет строки (старая ФС) — для системного тома данных FileVault (`fdesetup status`), иначе `None`. `Some(true)` —
/// включён, `Some(false)` — выключен, `None` — не ответил. Звать в фоне.
pub fn bitlocker(path: &Path) -> (String, Option<bool>) {
    let mount = run("/bin/df", &["-P", &path.to_string_lossy()]).as_deref().and_then(df_mount).unwrap_or_else(|| "/".to_owned());
    let info = run("/usr/sbin/diskutil", &["info", &mount]);
    let drive = info.as_deref().and_then(volume_name).unwrap_or_else(|| mount.clone());
    if let Some(state) = info.as_deref().and_then(volume_filevault) {
        return (drive, Some(state));
    }
    if mount != "/" && mount != "/System/Volumes/Data" {
        tracing::warn!("disk encryption: diskutil did not report FileVault for the data volume");
        return (drive, None);
    }
    let out = std::process::Command::new("/usr/bin/fdesetup").arg("status").output();
    let state = match out {
        Ok(o) if o.status.success() => filevault(&String::from_utf8_lossy(&o.stdout)),
        Ok(o) => {
            tracing::warn!("fdesetup status: {}", o.status);
            None
        }
        Err(e) => {
            tracing::warn!("fdesetup: {e}");
            None
        }
    };
    (drive, state)
}

/// Точка монтирования из `df -P <путь>`: последний столбец второй строки.
fn df_mount(out: &str) -> Option<String> {
    let line = out.lines().nth(1)?;
    let at = line.find(" /")?;
    Some(line[at + 1..].trim().to_owned())
}

/// Строка `FileVault: Yes|No` из `diskutil info` (APFS).
fn volume_filevault(info: &str) -> Option<bool> {
    let v = info.lines().find_map(|l| l.trim().strip_prefix("FileVault:"))?.trim();
    if v.starts_with("Yes") {
        Some(true)
    } else if v.starts_with("No") {
        Some(false)
    } else {
        None
    }
}

/// Имя тома из `diskutil info`: строка «Volume Name: Macintosh HD».
fn volume_name(info: &str) -> Option<String> {
    let name = info.lines().find_map(|l| l.trim().strip_prefix("Volume Name:"))?.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// Ответ `fdesetup status`: «FileVault is On.» (и «… Encryption in progress» — уже защищает новые данные),
/// «FileVault is Off.» (и расшифровка).
fn filevault(status: &str) -> Option<bool> {
    let first = status.lines().next()?.trim();
    if first.starts_with("FileVault is On") {
        Some(true)
    } else if first.starts_with("FileVault is Off") {
        Some(false)
    } else {
        None
    }
}

/// «Системные настройки → Конфиденциальность и безопасность → FileVault».
pub fn open_bitlocker() -> Result<(), String> {
    open("x-apple.systempreferences:com.apple.preference.security?FileVault")
}

/// UPN для входа без пароля (MAC-05): принципал билета Kerberos (`klist`: `anna@CORP.EXAMPLE` → `anna@corp.example`);
/// билета нет — домен AD (`dsconfigad -show`) и имя входа macOS. Звать в фоне.
pub fn upn() -> Option<String> {
    let klist = run("/usr/bin/klist", &[]);
    if let Some(upn) = klist.as_deref().and_then(kerberos_principal).map(|p| upn_of(&p)) {
        return Some(upn);
    }
    let domain = run("/usr/sbin/dsconfigad", &["-show"]).as_deref().and_then(ad_domain)?;
    let user = std::env::var("USER").ok().filter(|u| !u.is_empty())?;
    Some(format!("{user}@{}", domain.to_lowercase()))
}

/// Домен входа без пароля (PAR-06 №24): область действующего билета Kerberos (`klist --test`, принципал `klist`)
/// строчными; нет билета — `None`. Без кэша.
pub fn domain() -> Option<String> {
    if !in_domain() {
        return None;
    }
    let principal = run("/usr/bin/klist", &[]).as_deref().and_then(kerberos_principal)?;
    principal.rsplit_once('@').map(|(_, realm)| realm.to_lowercase())
}

/// Вход без пароля имеет смысл: есть действующий билет Kerberos (`klist --test` — вход учётной записью домена AD,
/// Kerberos SSO, `kinit`). Один раз за запуск: процесс — единицы миллисекунд.
pub fn in_domain() -> bool {
    // Без кэша (PAR-04 №24): билет появляется (`kinit`) и истекает во время работы — как на Windows и Linux.
    std::process::Command::new("/usr/bin/klist")
        .arg("--test")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Вывод команды при успехе.
fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(cmd).args(args).stderr(std::process::Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Принципал кэша Kerberos из вывода `klist` (Heimdal): строка `Principal: user@REALM`.
fn kerberos_principal(klist: &str) -> Option<String> {
    klist.lines().find_map(|l| l.trim().strip_prefix("Principal:")).map(str::trim).filter(|p| p.contains('@')).map(str::to_owned)
}

/// `user@REALM` → `user@realm`: область Kerberos AD — домен DNS заглавными.
fn upn_of(principal: &str) -> String {
    match principal.rsplit_once('@') {
        Some((user, realm)) => format!("{user}@{}", realm.to_lowercase()),
        None => principal.to_owned(),
    }
}

/// Домен AD из `dsconfigad -show`: строка `Active Directory Domain = corp.example`; Mac не в домене — пусто.
fn ad_domain(show: &str) -> Option<String> {
    show.lines()
        .find_map(|l| l.trim().strip_prefix("Active Directory Domain"))
        .and_then(|rest| rest.split_once('='))
        .map(|(_, d)| d.trim().to_owned())
        .filter(|d| !d.is_empty())
}

/// Открывать Postman при входе в систему: `SMAppService.mainApp` (объект входа «Postman»). Выключенный пользователем
/// в «Объектах входа» — открыть их: включить можно только там.
pub fn set_autostart(on: bool) -> Result<(), String> {
    use pets_shell::login_item::{self, State};
    login_item::set(on).map_err(|e| e.to_string())?;
    if on && login_item::state() == State::RequiresApproval {
        login_item::open_settings();
    }
    Ok(())
}

/// Postman открывается при входе.
pub fn autostart() -> bool {
    pets_shell::login_item::state() == pets_shell::login_item::State::On
}

/// Сделать Postman приложением по умолчанию для `mailto:`; macOS сама спросит подтверждение.
pub fn register_mailto() -> Result<(), String> {
    pets_shell::url_scheme::set_default(MAILTO).map_err(|e| e.to_string())
}

/// На macOS выбор делает `register_mailto` (с подтверждением системы): «Приложения по умолчанию» открывать не нужно.
pub fn show_default_apps() -> Result<(), String> {
    Ok(())
}

/// Ссылки `mailto:` открывает Postman.
pub fn mailto_is_default() -> bool {
    pets_shell::url_scheme::is_default(MAILTO)
}

/// Политика организации (как `Software\Policies\Pets\Postman` на Windows): принудительное значение профиля
/// конфигурации для пакета Postman (`CFPreferences`) с типом; свои `defaults write` не считаются. Дробное число
/// (`MinTls` = 1.3 `<real>`) — [`PolicyValue::Real`]: `pets_shell::managed::get` его теряет (POL-01).
pub fn policy(name: &str) -> Option<PolicyValue> {
    cf::forced(name)
}

/// Чтение принудительного значения `CFPreferences` любого типа. Перенести в `pets_shell::managed` при подъёме пина
/// pets (POL-01, решения владельца).
mod cf {
    use std::ffi::{c_char, c_void};

    use super::PolicyValue;

    type Ref = *const c_void;
    const UTF8: u32 = 0x0800_0100;
    const SINT64: isize = 4;
    const FLOAT64: isize = 6;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFPreferencesCurrentApplication: Ref;
        fn CFStringCreateWithBytes(alloc: Ref, bytes: *const u8, len: isize, encoding: u32, external: u8) -> Ref;
        fn CFPreferencesAppValueIsForced(key: Ref, app: Ref) -> u8;
        fn CFPreferencesCopyAppValue(key: Ref, app: Ref) -> Ref;
        fn CFGetTypeID(cf: Ref) -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFNumberGetTypeID() -> usize;
        fn CFBooleanGetTypeID() -> usize;
        fn CFBooleanGetValue(b: Ref) -> u8;
        fn CFNumberIsFloatType(n: Ref) -> u8;
        fn CFNumberGetValue(n: Ref, kind: isize, out: *mut c_void) -> u8;
        fn CFStringGetCString(s: Ref, buf: *mut c_char, size: isize, encoding: u32) -> u8;
        fn CFRelease(cf: Ref);
    }

    /// Освободить объект при выходе.
    struct Owned(Ref);

    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: объект получен по правилу Create/Copy и освобождается один раз.
            unsafe { CFRelease(self.0) };
        }
    }

    pub fn forced(name: &str) -> Option<PolicyValue> {
        let len = isize::try_from(name.len()).ok()?;
        // SAFETY: байты UTF-8 живут до конца вызова; результат — по правилу Create, освобождает `Owned`.
        let key = unsafe { CFStringCreateWithBytes(std::ptr::null(), name.as_ptr(), len, UTF8, 0) };
        if key.is_null() {
            return None;
        }
        let key = Owned(key);
        // SAFETY: неизменяемая константа CoreFoundation, живёт весь процесс; ключ — живая строка.
        let app = unsafe { kCFPreferencesCurrentApplication };
        // SAFETY: ключ и домен — живые объекты CoreFoundation.
        if unsafe { CFPreferencesAppValueIsForced(key.0, app) } == 0 {
            return None;
        }
        // SAFETY: то же; результат — по правилу Copy, освобождает `Owned`.
        let value = unsafe { CFPreferencesCopyAppValue(key.0, app) };
        if value.is_null() {
            return None;
        }
        let value = Owned(value);
        // SAFETY: `value` — живой объект; вызовы по его типу.
        unsafe {
            let id = CFGetTypeID(value.0);
            if id == CFStringGetTypeID() {
                let mut buf = [0 as c_char; 256];
                if CFStringGetCString(value.0, buf.as_mut_ptr(), 256, UTF8) == 0 {
                    return Some(PolicyValue::Other("too long string"));
                }
                let text = std::ffi::CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned();
                Some(PolicyValue::Text(text))
            } else if id == CFBooleanGetTypeID() {
                Some(PolicyValue::Bool(CFBooleanGetValue(value.0) != 0))
            } else if id == CFNumberGetTypeID() {
                if CFNumberIsFloatType(value.0) != 0 {
                    let mut n = 0f64;
                    (CFNumberGetValue(value.0, FLOAT64, (&raw mut n).cast()) != 0).then_some(PolicyValue::Real(n))
                } else {
                    let mut n = 0i64;
                    (CFNumberGetValue(value.0, SINT64, (&raw mut n).cast()) != 0).then_some(PolicyValue::Int(n))
                }
            } else {
                Some(PolicyValue::Other("unsupported plist type"))
            }
        }
    }
}

/// Запуск из Finder или Dock (stderr — `/dev/null`): поток ошибок — в файл журнала, туда пишет `tracing`. Из терминала
/// — ничего. `true` — журнал пишется в файл.
pub fn log_to_file(path: &Path) -> bool {
    pets_shell::console::stderr_to_file(path).unwrap_or(false)
}

/// Уровень журнала по умолчанию (`POSTMAN_LOG`), если его не задали снаружи. Вызывать до потоков.
pub fn default_env(name: &str, value: &str) {
    if std::env::var_os(name).is_none() {
        // SAFETY: в начале `main`, до запуска потоков (Rust 2024: `set_var` — unsafe).
        unsafe { std::env::set_var(name, value) };
    }
}

/// Открыть путь или адрес приложением по умолчанию (`open`) — по действию пользователя.
pub fn open(target: &str) -> Result<(), String> {
    std::process::Command::new("/usr/bin/open").arg(target).spawn().map(|_| ()).map_err(|e| format!("open: {e}"))
}

/// Показать файл в Finder (`open -R`).
pub fn reveal(path: &Path) -> Result<(), String> {
    std::process::Command::new("/usr/bin/open").arg("-R").arg(path).spawn().map(|_| ()).map_err(|e| format!("open -R: {e}"))
}

/// Уведомление о новом письме: со звуком или молча («Настройки → Общие → Звук»).
pub fn notify_with(title: &str, body: &str, sound: bool) -> Result<(), String> {
    let sound = if sound { toast::Sound::Default } else { toast::Sound::Silent };
    toast::show_with(title, body, sound).map_err(|e| e.to_string())
}

/// Уведомление со щелчком (MAC-04, как T3-13 на Windows): `on_click` — из потока центра уведомлений, пока Postman
/// запущен. `sound`: `None` и `Some(true)` — звук уведомлений, `Some(false)` — молча.
pub fn notify_click(title: &str, body: &str, sound: Option<bool>, on_click: impl Fn() + Send + 'static) -> Result<(), String> {
    let sound = if sound == Some(false) { toast::Sound::Silent } else { toast::Sound::Default };
    toast::show_clickable(title, body, sound, on_click).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    /// Ответы `fdesetup status` (macOS 13 — 26).
    #[test]
    fn filevault_status() {
        assert_eq!(
            super::filevault(
                "FileVault is On.
"
            ),
            Some(true)
        );
        assert_eq!(
            super::filevault(
                "FileVault is On.
Encryption in progress: Percent completed = 12
"
            ),
            Some(true)
        );
        assert_eq!(
            super::filevault(
                "FileVault is Off.
"
            ),
            Some(false)
        );
        assert_eq!(
            super::filevault(
                "FileVault is Off.
Decryption in progress: Percent completed = 40
"
            ),
            Some(false)
        );
        assert_eq!(super::filevault(""), None);
        assert_eq!(
            super::volume_name(
                "   Device Node:  /dev/disk3s1s1
   Volume Name:              Macintosh HD
"
            )
            .as_deref(),
            Some("Macintosh HD")
        );
        assert_eq!(super::volume_name(""), None);
    }

    /// PAR-04 №23: том данных из `df -P` и его FileVault из `diskutil info`.
    #[test]
    fn data_volume_filevault() {
        let df = "Filesystem 512-blocks Used Available Capacity Mounted on\n/dev/disk4s1 1000 10 990 2% /Volumes/My Data\n";
        assert_eq!(super::df_mount(df).as_deref(), Some("/Volumes/My Data"));
        assert_eq!(super::volume_filevault("   FileVault:                 Yes (Unlocked)\n"), Some(true));
        assert_eq!(super::volume_filevault("   FileVault:                 No\n"), Some(false));
        assert_eq!(super::volume_filevault("   Volume Name: X\n"), None);
    }

    /// `klist` (Heimdal) и `dsconfigad -show` → UPN и домен AD (MAC-05).
    #[test]
    fn kerberos_and_ad_domain() {
        let klist = "Credentials cache: API:6E3B3A1D-0000-0000-0000-000000000000
        Principal: anna@CORP.EXAMPLE

  Issued                Expires               Principal
Nov 12 09:00:00 2026  Nov 12 19:00:00 2026  krbtgt/CORP.EXAMPLE@CORP.EXAMPLE
";
        let principal = super::kerberos_principal(klist);
        assert_eq!(principal.as_deref(), Some("anna@CORP.EXAMPLE"));
        assert_eq!(super::upn_of("anna@CORP.EXAMPLE"), "anna@corp.example");
        assert_eq!(super::kerberos_principal("klist: krb5_cc_get_principal: No credentials cache file found"), None);
        let show = "Active Directory Forest          = corp.example
Active Directory Domain          = corp.example
Computer Account                 = mac-anna$
";
        assert_eq!(super::ad_domain(show).as_deref(), Some("corp.example"));
        assert_eq!(super::ad_domain(""), None);
    }
}
