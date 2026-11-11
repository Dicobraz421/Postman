//! «Настройки → Безопасность» (S5-30): что действует — выбор пользователя или политика организации. Выбор — только
//! строже прежнего (решение владельца 2026-10-22): картинки «никогда» или «спрашивать», TLS 1.2 или 1.3, пароль —
//! в Диспетчере учётных данных или не хранить; проверка отзыва — всегда. Политика — `Software\Policies\Pets\Postman`
//! (HKLM, затем HKCU): `ExternalImages` = `never` | `ask`, `MinTls` = `1.2` | `1.3`, `StorePasswords` = `0` | `1` —
//! закрепляет значение, в настройках оно недоступно.

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

#[cfg(test)]
mod tests {
    use super::*;

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
