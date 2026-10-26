//! Не Windows: хранилищ и СКЗИ нет — якорей нет, цепочка в ОС не подтверждается.

use postman_smime::Trust;

use super::Revocation;

pub(super) fn load_trust(_corporate_extra: &[String]) -> Trust {
    Trust::default()
}

pub(super) fn confirm(_chain: &[Vec<u8>], _exclusive_root: Option<&[u8]>) -> Revocation {
    Revocation::NotChecked
}

pub(super) fn gost_provider() -> Option<String> {
    None
}

/// СКЗИ нет вне Windows.
pub(super) fn gost_decrypt(
    _env: &postman_smime::envelope::Envelope,
    _prompt: super::keys::Prompt,
) -> Result<super::skzi::Plain, super::keys::KeyError> {
    Err(super::keys::KeyError::NeedSkzi)
}

/// Хранилища ключей нет: письмо — «не для вас».
pub(super) fn unwrap_cek(
    env: &postman_smime::envelope::Envelope,
    _prompt: super::keys::Prompt,
) -> Result<postman_smime::envelope::Cek, super::keys::KeyError> {
    Err(super::keys::KeyError::NotForYou(super::keys::labels(env)))
}
