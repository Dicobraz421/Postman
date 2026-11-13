//! Не Windows и не macOS: СКЗИ нет, цепочка в ОС не подтверждается; на других ОС, кроме Linux (`linux.rs`), нет и
//! якорей и ключей.

#[cfg(not(target_os = "linux"))]
use postman_smime::Trust;

#[cfg(not(target_os = "linux"))]
use super::Revocation;

#[cfg(not(target_os = "linux"))]
pub(super) fn load_trust(_corporate_extra: &[String]) -> Trust {
    Trust::default()
}

#[cfg(not(target_os = "linux"))]
pub(super) fn confirm(_chain: &[Vec<u8>], _exclusive_root: Option<&[u8]>) -> Revocation {
    Revocation::NotChecked
}

pub(super) fn gost_provider() -> Option<String> {
    None
}

/// Сети для CRL нет: отзыв ГОСТ — «не проверен».
#[cfg(not(target_os = "linux"))]
pub(super) fn fetch_crl(_url: &str, _timeout: std::time::Duration) -> Result<Vec<u8>, String> {
    Err("CRL download is not available on this OS".into())
}

/// СКЗИ нет вне Windows.
pub(super) fn gost_decrypt(
    _env: &postman_smime::envelope::Envelope,
    _prompt: super::keys::Prompt,
) -> Result<super::skzi::Plain, super::keys::KeyError> {
    Err(super::keys::KeyError::NeedSkzi)
}

/// Хранилища ключей нет: письмо — «не для вас».
#[cfg(not(target_os = "linux"))]
pub(super) fn unwrap_cek(
    env: &postman_smime::envelope::Envelope,
    _prompt: super::keys::Prompt,
) -> Result<postman_smime::envelope::Cek, super::keys::KeyError> {
    Err(super::keys::KeyError::NotForYou(super::keys::labels(env)))
}
