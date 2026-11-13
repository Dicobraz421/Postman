//! Ключ содержимого шифрованного письма (S5-12 Ф2, `docs/SMIME.md` §4, §6.2): ядро получает от воркера
//! типизированные поля получателей ([`Envelope`]) и снимает ключ своим закрытым ключом — средствами ОС (CNG,
//! [`WindowsKeys`]); в демо и тестах — ключом в памяти ([`SoftKeys`]). Чужой DER здесь не разбирается: издатель и
//! серийный номер сравниваются побайтно с полями своих сертификатов.
//!
//! Ключ содержимого живёт только в памяти: на сеанс — [`Session`] (LRU, обнуление при вытеснении и выходе), чтобы
//! вложения расшифрованного письма открывались без второго PIN; на диск — ни ключ, ни текст (решение владельца №4).
//!
//! ГОСТ (решение владельца 2026-10-22): операции с закрытым ключом ГОСТ — только в сертифицированном СКЗИ (КриптоПро
//! CSP, ViPNet CSP); ключ ГОСТ в файле не хранится никогда. Нет СКЗИ — [`KeyError::NeedSkzi`]. Ключ содержимого ГОСТ
//! из СКЗИ не выходит: [`KeyStore::gost`] отдаёт уже расшифрованное внутреннее письмо (`skzi.rs`).

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock, PoisonError};

use postman_smime::envelope::{Cek, Envelope, Wrap};

/// Как спрашивать ключ: молча (открытие письма) или с окном PIN ОС поверх окна `hwnd` (пользователь нажал «Ввести»).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prompt {
    Silent,
    Window(isize),
}

/// Почему ключ не снят — строки области чтения (`docs/SMIME.md` §7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyError {
    /// Нет своего сертификата ни для одного получателя: подписи получателей (издатель, серийный номер).
    NotForYou(Vec<String>),
    /// Ключ на смарт-карте или защищён: нужен PIN («Ввести»).
    NeedPin,
    /// Смарт-карта не вставлена или не готова («Повторить»).
    NoCard,
    /// Пользователь отменил ввод PIN.
    Cancelled,
    /// Получатель наш, но алгоритм не поддерживается (имя).
    Unsupported(String),
    /// Письмо ГОСТ, а сертифицированного СКЗИ на компьютере нет.
    NeedSkzi,
    /// Другой сбой ОС (текст — для журнала).
    Failed(String),
}

/// Чем кончилась операция с закрытым ключом, кроме «сбоя»: одно правило на все ОС (ADR 0011, PAR-AUDIT №28) —
/// защищённый ключ или неверный PIN/пароль — «Ввести», нет карты — «Повторить», отмена — без ошибки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyFault {
    NeedPin,
    NoCard,
    Cancelled,
}

/// Коды CNG/CryptoAPI/смарт-карт Windows (HRESULT) → [`KeyFault`]; `None` — другой сбой. Таблица общая для `cng.rs`
/// и `sign_win.rs`; проверяется тестом на любой ОС.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn windows_fault(hr: i32) -> Option<KeyFault> {
    const NTE_SILENT_CONTEXT: u32 = 0x8009_0022;
    const NTE_INCORRECT_PASSWORD: u32 = 0x8009_0033;
    const NTE_DEVICE_NOT_READY: u32 = 0x8009_0030;
    const SCARD_W_WRONG_CHV: u32 = 0x8010_006B;
    const SCARD_W_REMOVED_CARD: u32 = 0x8010_0069;
    const SCARD_E_NO_SMARTCARD: u32 = 0x8010_000C;
    const SCARD_W_CANCELLED_BY_USER: u32 = 0x8010_006E;
    const ERROR_CANCELLED_HR: u32 = 0x8007_04C7;
    match hr as u32 {
        NTE_SILENT_CONTEXT | NTE_INCORRECT_PASSWORD | SCARD_W_WRONG_CHV => Some(KeyFault::NeedPin),
        NTE_DEVICE_NOT_READY | SCARD_W_REMOVED_CARD | SCARD_E_NO_SMARTCARD => Some(KeyFault::NoCard),
        SCARD_W_CANCELLED_BY_USER | ERROR_CANCELLED_HR => Some(KeyFault::Cancelled),
        _ => None,
    }
}

#[cfg_attr(not(windows), allow(dead_code))]
impl KeyFault {
    pub(crate) fn key_error(self) -> KeyError {
        match self {
            KeyFault::NeedPin => KeyError::NeedPin,
            KeyFault::NoCard => KeyError::NoCard,
            KeyFault::Cancelled => KeyError::Cancelled,
        }
    }

    pub(crate) fn sign_error(self) -> postman_smime::sign::SignError {
        use postman_smime::sign::SignError;
        match self {
            KeyFault::NeedPin => SignError::NeedPin,
            KeyFault::NoCard => SignError::NoCard,
            KeyFault::Cancelled => SignError::Cancelled,
        }
    }
}

/// Снимает ключ содержимого.
pub trait KeyStore: Send + Sync {
    fn cek(&self, env: &Envelope, prompt: Prompt) -> Result<Cek, KeyError>;

    /// ГОСТ 28147-89: внутреннее письмо, расшифрованное СКЗИ (ключ из СКЗИ не выходит). Без СКЗИ — «нужно СКЗИ».
    fn gost(&self, _env: &Envelope, _prompt: Prompt) -> Result<super::skzi::Plain, KeyError> {
        Err(KeyError::NeedSkzi)
    }
}

/// Ключи пользователя в хранилище Windows `CurrentUser\My` (CNG: программный KSP, TPM, смарт-карта).
pub struct WindowsKeys;

impl KeyStore for WindowsKeys {
    fn cek(&self, env: &Envelope, prompt: Prompt) -> Result<Cek, KeyError> {
        if let Err(name) = &env.cipher {
            return Err(KeyError::Unsupported(name.clone()));
        }
        super::sys::unwrap_cek(env, prompt)
    }

    fn gost(&self, env: &Envelope, prompt: Prompt) -> Result<super::skzi::Plain, KeyError> {
        use super::GostProvider;
        gost_with(super::SystemGost.provider(), env, prompt)
    }
}

/// ГОСТ: СКЗИ есть (`provider`) — расшифровка его средствами; нет — «нужно СКЗИ» (без попытки своими силами).
pub fn gost_with(provider: Option<String>, env: &Envelope, prompt: Prompt) -> Result<super::skzi::Plain, KeyError> {
    let Some(name) = provider else { return Err(KeyError::NeedSkzi) };
    tracing::debug!(provider = %name, "smime: GOST decryption through the SKZI");
    super::sys::gost_decrypt(env, prompt)
}

/// Ключи в памяти (демо-ящик, тесты): сертификат и ключ RSA.
pub struct SoftKeys(pub Vec<postman_smime::soft::SoftKey>);

impl KeyStore for SoftKeys {
    fn cek(&self, env: &Envelope, _prompt: Prompt) -> Result<Cek, KeyError> {
        if let Err(name) = &env.cipher {
            return Err(KeyError::Unsupported(name.clone()));
        }
        postman_smime::soft::unwrap(&self.0, env).map_err(|e| match e {
            postman_smime::soft::SoftError::NotForYou => KeyError::NotForYou(labels(env)),
            postman_smime::soft::SoftError::Unsupported(n) => KeyError::Unsupported(n),
        })
    }
}

/// Подписи получателей (для «не для вас»), без неподдерживаемых пустых.
pub fn labels(env: &Envelope) -> Vec<String> {
    env.recipients.iter().map(|r| r.label.clone()).filter(|l| !l.is_empty()).collect()
}

/// Получатель, ключ которого можно снять средствами ОС (RSA или ECDH).
pub fn supported(w: &Wrap) -> bool {
    matches!(w, Wrap::RsaPkcs1 | Wrap::RsaOaep(_) | Wrap::Ecdh { .. })
}

/// Сколько ключей держать на сеанс.
const SESSION_MAX: usize = 64;

/// Ключи содержимого открытых за сеанс писем (по id письма): вложения — без второго PIN. Вытеснение и выход —
/// обнуление (`Zeroizing`).
#[derive(Default)]
pub struct Session {
    keys: VecDeque<(String, Cek)>,
}

impl Session {
    pub fn get(&mut self, id: &str) -> Option<Cek> {
        let i = self.keys.iter().position(|(k, _)| k == id)?;
        let item = self.keys.remove(i)?;
        let cek = item.1.clone();
        self.keys.push_back(item);
        Some(cek)
    }

    pub fn put(&mut self, id: &str, cek: Cek) {
        self.keys.retain(|(k, _)| k != id);
        if self.keys.len() >= SESSION_MAX {
            self.keys.pop_front();
        }
        self.keys.push_back((id.to_owned(), cek));
    }

    pub fn clear(&mut self) {
        self.keys.clear();
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

/// Ключи сеанса процесса.
pub fn session() -> &'static Mutex<Session> {
    static S: OnceLock<Mutex<Session>> = OnceLock::new();
    S.get_or_init(Mutex::default)
}

/// Ключ письма `id` на сеанс.
pub fn remembered(id: &str) -> Option<Cek> {
    session().lock().unwrap_or_else(PoisonError::into_inner).get(id)
}

pub fn remember(id: &str, cek: Cek) {
    session().lock().unwrap_or_else(PoisonError::into_inner).put(id, cek);
}

/// Выход из учётной записи и смена ящика: ключи сеанса — забыть.
pub fn forget_all() {
    session().lock().unwrap_or_else(PoisonError::into_inner).clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use postman_smime::envelope::Cipher;
    use postman_smime::soft::SoftKey;
    use postman_smime::testkit::{self, Head, Key, KeyAlg, Pki, To};

    /// Неверный PIN/пароль — «Ввести», как на macOS (`errSecAuthFailed`) и Linux (пароль PKCS#12).
    #[test]
    fn windows_codes() {
        let table: &[(u32, Option<KeyFault>)] = &[
            (0x8009_0022, Some(KeyFault::NeedPin)),
            (0x8009_0033, Some(KeyFault::NeedPin)),
            (0x8010_006B, Some(KeyFault::NeedPin)),
            (0x8009_0030, Some(KeyFault::NoCard)),
            (0x8010_0069, Some(KeyFault::NoCard)),
            (0x8010_000C, Some(KeyFault::NoCard)),
            (0x8010_006E, Some(KeyFault::Cancelled)),
            (0x8007_04C7, Some(KeyFault::Cancelled)),
            (0x8009_000D, None),
            (0, None),
        ];
        for &(hr, want) in table {
            assert_eq!(windows_fault(hr as i32), want, "0x{hr:08X}");
        }
        assert_eq!(KeyFault::NeedPin.key_error(), KeyError::NeedPin);
        assert!(matches!(KeyFault::NoCard.sign_error(), postman_smime::sign::SignError::NoCard));
    }

    fn soft(pki: &Pki) -> SoftKeys {
        let Key::Rsa(k) = &pki.user_key else { unreachable!() };
        SoftKeys(vec![SoftKey::new(&pki.user.der, &postman_smime::soft::pkcs1_der(k).unwrap()).unwrap()])
    }

    fn letter(to: &Pki) -> Vec<u8> {
        let cms = testkit::enveloped(
            &testkit::html_part("<p>x</p>"),
            &[To { cert: &to.user, key: &to.user_key, alg: KeyAlg::RsaPkcs1, ski: false }],
            Cipher::AesCbc(256),
            &[9; 32],
            false,
        );
        testkit::encrypted_letter(&Head { from: "a@itt.test", to: "me@itt.test", subject: "s", date: "Wed, 21 Oct 2026 10:00:00 +0300" }, &cms, false)
    }

    /// Свой ключ снимает CEK; чужое письмо — «не для вас» с издателем и серийным номером получателя.
    #[test]
    fn soft_keys_and_not_for_you() {
        let me = Pki::rsa("Корпоративный УЦ", "Смирнова", "me@itt.test");
        let env = postman_worker::smime::envelope(&letter(&me)).unwrap();
        assert_eq!(soft(&me).cek(&env, Prompt::Silent).unwrap().as_slice(), &[9; 32]);
        let other = Pki::build(Key::rsa('a'), Key::rsa('c'), "Чужой УЦ", "Петров", "p@itt.test");
        let env = postman_worker::smime::envelope(&letter(&other)).unwrap();
        match soft(&me).cek(&env, Prompt::Silent) {
            Err(KeyError::NotForYou(l)) => assert!(l[0].starts_with("Чужой УЦ, № "), "{l:?}"),
            other => panic!("{other:?}"),
        }
    }

    /// Сеанс: последние [`SESSION_MAX`], повторное — наверх.
    /// ГОСТ без СКЗИ — «нужно СКЗИ»; ключи в памяти ГОСТ не снимают никогда.
    #[test]
    fn gost_needs_skzi() {
        let env = Envelope { recipients: vec![], cipher: Ok(Cipher::Gost28147), authenticated: false, gost: None };
        assert_eq!(gost_with(None, &env, Prompt::Silent).unwrap_err(), KeyError::NeedSkzi);
        let me = Pki::rsa("УЦ", "Смирнова", "me@itt.test");
        assert_eq!(soft(&me).gost(&env, Prompt::Silent).unwrap_err(), KeyError::NeedSkzi);
    }

    #[test]
    fn session_lru() {
        let mut s = Session::default();
        for i in 0..SESSION_MAX + 5 {
            s.put(&format!("m{i}"), zeroize::Zeroizing::new(vec![i as u8]));
        }
        assert_eq!(s.len(), SESSION_MAX);
        assert!(s.get("m0").is_none());
        assert_eq!(s.get("m5").unwrap().as_slice(), &[5]);
        s.put("new", zeroize::Zeroizing::new(vec![1]));
        assert!(s.get("m5").is_some(), "m5 поднят наверх — не вытеснен");
        assert!(s.get("m6").is_none());
        s.clear();
        assert!(s.is_empty());
    }
}
