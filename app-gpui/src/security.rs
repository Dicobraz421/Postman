//! «Настройки → Безопасность» (S5-30): что действует — выбор пользователя или политика организации. Выбор — только
//! строже прежнего (решение владельца 2026-10-22): картинки «никогда» или «спрашивать», TLS 1.2 или 1.3, пароль —
//! в Диспетчере учётных данных или не хранить; проверка отзыва — всегда. Политика — `Software\Policies\Pets\Postman`
//! (HKLM, затем HKCU): `ExternalImages` = `never` | `ask`, `MinTls` = `1.2` | `1.3`, `StorePasswords` = `0` | `1` —
//! закрепляет значение, в настройках оно недоступно. Linux (LIN-03) — файл `/etc/postman/policies.json` (JSON-объект,
//! те же имена; `sys::shell::POLICY_FILE`); там же `SandboxNamespaces` = `optional` | `required` ([`sandbox_namespaces`]).

use std::sync::OnceLock;

use postman_core::prefs::{Images, Prefs};

/// Закреплённое политикой.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Policy {
    pub(crate) images: Option<Images>,
    pub(crate) tls13: Option<bool>,
    pub(crate) store_password: Option<bool>,
}

impl Policy {
    /// Значения политики из строк реестра; непонятное — не закреплено.
    pub(crate) fn parse(images: Option<&str>, tls: Option<&str>, store: Option<&str>) -> Policy {
        Policy {
            images: match images.map(str::trim) {
                Some(v) if v.eq_ignore_ascii_case("never") => Some(Images::Never),
                Some(v) if v.eq_ignore_ascii_case("ask") => Some(Images::Ask),
                _ => None,
            },
            tls13: match tls.map(str::trim) {
                Some("1.3") => Some(true),
                Some("1.2") => Some(false),
                _ => None,
            },
            store_password: match store.map(str::trim) {
                Some("1") => Some(true),
                Some("0") => Some(false),
                _ => None,
            },
        }
    }
}

/// Политика — читается один раз (реестр, микросекунды).
pub(crate) fn policy() -> Policy {
    static POLICY: OnceLock<Policy> = OnceLock::new();
    *POLICY.get_or_init(|| {
        let read = crate::sys::shell::policy;
        Policy::parse(read("ExternalImages").as_deref(), read("MinTls").as_deref(), read("StorePasswords").as_deref())
    })
}

/// Что действует: политика сильнее выбора.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Effective {
    pub(crate) images: Images,
    pub(crate) tls13: bool,
    pub(crate) store_password: bool,
}

pub(crate) fn effective_with(p: &Prefs, policy: Policy) -> Effective {
    Effective {
        images: policy.images.unwrap_or(p.security.images),
        tls13: policy.tls13.unwrap_or(p.security.tls13),
        store_password: policy.store_password.unwrap_or(p.security.store_password),
    }
}

pub(crate) fn effective(p: &Prefs) -> Effective {
    effective_with(p, policy())
}

/// Минимальная версия TLS — во все пути к серверам (новые подключения).
pub(crate) fn apply(p: &Prefs) {
    crate::plugins::registry().set_tls13_only(effective(p).tls13);
}

/// Политика `SandboxNamespaces` (LIN-03, ADR 0010): `required` — воркер и хост модулей без user/net/ipc namespaces не
/// работают (письмо — текстом), `optional` или нет политики — по умолчанию. Нераспознанное — WARN и строже
/// (`required`, ADR 0011 п. 5). Действует на Linux; на других ОС namespaces нет.
#[cfg_attr(not(target_os = "linux"), allow(dead_code, reason = "namespaces are Linux only; the parser is tested on every OS"))]
pub(crate) fn sandbox_namespaces(value: Option<&str>) -> postman_worker::Namespaces {
    use postman_worker::Namespaces;
    match value.map(str::trim) {
        None => Namespaces::Optional,
        Some(v) if v.eq_ignore_ascii_case("optional") => Namespaces::Optional,
        Some(v) if v.eq_ignore_ascii_case("required") => Namespaces::Required,
        Some(v) => {
            tracing::warn!(policy = "SandboxNamespaces", value = v, "policy: unsupported value, using the stricter `required`");
            Namespaces::Required
        }
    }
}

/// Режим namespaces всех воркеров процесса — из политики (до первого воркера).
#[cfg(target_os = "linux")]
pub(crate) fn apply_sandbox_policy() {
    let mode = sandbox_namespaces(crate::sys::shell::policy("SandboxNamespaces").as_deref());
    if mode == postman_worker::Namespaces::Required {
        tracing::info!("policy: sandbox namespaces are required");
    }
    postman_worker::set_default_namespaces(mode);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// LIN-03: `SandboxNamespaces` — без политики и `optional` — по умолчанию; `required` — обязательны; непонятное —
    /// строже.
    #[test]
    fn sandbox_namespaces_policy() {
        use postman_worker::Namespaces;
        assert_eq!(sandbox_namespaces(None), Namespaces::Optional);
        assert_eq!(sandbox_namespaces(Some("Optional")), Namespaces::Optional);
        assert_eq!(sandbox_namespaces(Some(" required ")), Namespaces::Required);
        assert_eq!(sandbox_namespaces(Some("1")), Namespaces::Required);
    }

    /// Политика закрепляет значение; непонятное в реестре — не закреплено; без политики — выбор пользователя.
    #[test]
    fn policy_pins_and_choice_follows() {
        let pol = Policy::parse(Some("Ask"), Some("1.3"), Some("0"));
        assert_eq!(pol, Policy { images: Some(Images::Ask), tls13: Some(true), store_password: Some(false) });
        assert_eq!(Policy::parse(Some("always"), Some("1.0"), Some("yes")), Policy::default(), "слабее — не бывает");
        let mut p = Prefs::default();
        let e = effective_with(&p, Policy::default());
        assert_eq!(e, Effective { images: Images::Ask, tls13: false, store_password: true });
        p.security.tls13 = true;
        p.security.store_password = false;
        let e = effective_with(&p, Policy { tls13: Some(false), ..Policy::default() });
        assert!(!e.tls13, "политика сильнее выбора");
        assert!(!e.store_password);
    }
}
