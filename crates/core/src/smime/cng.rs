//! Windows: свои сертификаты `CurrentUser\My` и снятие ключа содержимого средствами CNG (S5-12 Ф2, `docs/SMIME.md`
//! §4). Закрытый ключ не покидает поставщика (программный KSP, TPM, смарт-карта): RSA — `NCryptDecrypt` (PKCS#1 v1.5
//! или OAEP), ECDH — `NCryptSecretAgreement` + `BCRYPT_KDF_RAW_SECRET` (общий секрет — в обратном порядке байтов),
//! дальше KDF X9.63 и снятие обёртки AES — `postman_smime::envelope::ecdh_cek`.
//!
//! Получатель письма сравнивается со своими сертификатами побайтно: издатель — DER `CERT_INFO.Issuer` как есть,
//! серийный номер — `CERT_INFO.SerialNumber` (little-endian) против `INTEGER` письма (big-endian); SKI —
//! `CERT_KEY_IDENTIFIER_PROP_ID`. В crypt32 чужие байты не передаются.

use std::ptr;

use postman_smime::envelope::{Cek, Envelope, Rid, Wrap};
use windows_sys::Win32::Security::Cryptography::*;
use zeroize::Zeroizing;

use super::keys::{KeyError, Prompt, labels, supported};

const NTE_SILENT_CONTEXT: i32 = 0x8009_0022u32 as i32;
const NTE_DEVICE_NOT_READY: i32 = 0x8009_0030u32 as i32;
const NTE_NO_KEY: i32 = 0x8009_000Du32 as i32;
const SCARD_W_REMOVED_CARD: i32 = 0x8010_0069u32 as i32;
const SCARD_E_NO_SMARTCARD: i32 = 0x8010_000Cu32 as i32;
const SCARD_W_CANCELLED_BY_USER: i32 = 0x8010_006Eu32 as i32;
const ERROR_CANCELLED_HR: i32 = 0x8007_04C7u32 as i32;

/// Код CNG → ошибка интерфейса.
fn key_error(hr: i32, what: &str) -> KeyError {
    match hr {
        NTE_SILENT_CONTEXT => KeyError::NeedPin,
        NTE_DEVICE_NOT_READY | SCARD_W_REMOVED_CARD | SCARD_E_NO_SMARTCARD => KeyError::NoCard,
        SCARD_W_CANCELLED_BY_USER | ERROR_CANCELLED_HR => KeyError::Cancelled,
        _ => KeyError::Failed(format!("{what}: 0x{:08X}", hr as u32)),
    }
}

/// Хэндл CNG: освобождается в `Drop`.
struct Handle(usize);

impl Drop for Handle {
    fn drop(&mut self) {
        if self.0 != 0 {
            // SAFETY: хэндл получен от ncrypt в этом модуле и освобождается один раз.
            unsafe { NCryptFreeObject(self.0) };
        }
    }
}

/// Случайный ключ нужной длины: снятие PKCS#1 не удалось — содержимое не расшифруется той же ошибкой, что и при
/// неверном ключе (RFC 3218).
fn random(len: usize) -> Cek {
    let mut out = Zeroizing::new(vec![0u8; len]);
    // SAFETY: буфер на `len` байтов; системный генератор (`BCRYPT_USE_SYSTEM_PREFERRED_RNG`).
    unsafe {
        windows_sys::Win32::Security::Cryptography::BCryptGenRandom(ptr::null_mut(), out.as_mut_ptr(), len as u32, BCRYPT_USE_SYSTEM_PREFERRED_RNG)
    };
    out
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// RSA: `NCryptDecrypt` с PKCS#1 v1.5 или OAEP (хэш и MGF1 одним алгоритмом, без метки).
fn rsa_decrypt(key: usize, wrap: &Wrap, data: &[u8], silent: bool, key_len: usize) -> Result<Cek, KeyError> {
    let silent_flag = if silent { NCRYPT_SILENT_FLAG } else { 0 };
    let alg_name = |h: postman_smime::Hash| match h {
        postman_smime::Hash::Sha1 => Some("SHA1"),
        postman_smime::Hash::Sha256 => Some("SHA256"),
        postman_smime::Hash::Sha384 => Some("SHA384"),
        postman_smime::Hash::Sha512 => Some("SHA512"),
        _ => None,
    };
    let (alg, flags) = match wrap {
        Wrap::RsaPkcs1 => (None, NCRYPT_PAD_PKCS1_FLAG),
        Wrap::RsaOaep(h) => (Some(wide(alg_name(*h).ok_or_else(|| KeyError::Unsupported(format!("RSA-OAEP {h:?}")))?)), NCRYPT_PAD_OAEP_FLAG),
        _ => return Err(KeyError::Unsupported("RSA".into())),
    };
    let oaep = alg.as_ref().map(|a| BCRYPT_OAEP_PADDING_INFO { pszAlgId: a.as_ptr(), pbLabel: ptr::null_mut(), cbLabel: 0 });
    let padding: *const core::ffi::c_void = oaep.as_ref().map_or(ptr::null(), |p| ptr::from_ref(p).cast());
    let mut out = Zeroizing::new(vec![0u8; data.len().max(512)]);
    let mut got = 0u32;
    // SAFETY: входной и выходной буферы с длинами; заполнение живёт до конца вызова; ключ — открытый хэндл.
    let hr =
        unsafe { NCryptDecrypt(key, data.as_ptr(), data.len() as u32, padding, out.as_mut_ptr(), out.len() as u32, &mut got, flags | silent_flag) };
    match hr {
        0 => {
            out.truncate(got as usize);
            Ok(out)
        }
        hr => match key_error(hr, "NCryptDecrypt") {
            // Заполнение PKCS#1 не снялось (KSP отвечает `NTE_BAD_DATA` или `NTE_INVALID_PARAMETER`): случайный ключ —
            // одна ошибка с неверным ключом содержимого (RFC 3218). PIN, карта, отмена — как есть.
            KeyError::Failed(_) if matches!(wrap, Wrap::RsaPkcs1) => Ok(random(key_len)),
            e => Err(e),
        },
    }
}

/// ECDH: открытый ключ отправителя — во временный ключ того же поставщика, общий секрет (`BCRYPT_KDF_RAW_SECRET`,
/// little-endian → big-endian), затем KDF и снятие обёртки.
fn ecdh(key: usize, wrap: &Wrap, data: &[u8], silent: bool) -> Result<Cek, KeyError> {
    let Wrap::Ecdh { ephemeral, .. } = wrap else { return Err(KeyError::Unsupported("ECDH".into())) };
    let (magic, size) = match ephemeral.len() {
        65 => (BCRYPT_ECDH_PUBLIC_P256_MAGIC, 32),
        97 => (BCRYPT_ECDH_PUBLIC_P384_MAGIC, 48),
        133 => (BCRYPT_ECDH_PUBLIC_P521_MAGIC, 66),
        _ => return Err(KeyError::Unsupported("ECDH curve".into())),
    };
    if ephemeral.first() != Some(&4) {
        return Err(KeyError::Unsupported("ECDH point".into()));
    }
    let mut blob = Vec::with_capacity(8 + size * 2);
    blob.extend_from_slice(&magic.to_le_bytes());
    blob.extend_from_slice(&(size as u32).to_le_bytes());
    blob.extend_from_slice(&ephemeral[1..]);
    let mut provider: usize = 0;
    let mut got = 0u32;
    // SAFETY: свойство — хэндл поставщика (`usize`), буфер своего размера.
    let hr = unsafe {
        NCryptGetProperty(key, NCRYPT_PROVIDER_HANDLE_PROPERTY, ptr::from_mut(&mut provider).cast(), size_of::<usize>() as u32, &mut got, 0)
    };
    if hr != 0 {
        return Err(key_error(hr, "NCryptGetProperty"));
    }
    let provider = Handle(provider);
    let mut public: usize = 0;
    // SAFETY: блоб `BCRYPT_ECCKEY_BLOB` + X + Y с длиной; ключ освобождается `Handle`.
    let hr = unsafe { NCryptImportKey(provider.0, 0, BCRYPT_ECCPUBLIC_BLOB, ptr::null(), &mut public, blob.as_ptr(), blob.len() as u32, 0) };
    if hr != 0 {
        return Err(key_error(hr, "NCryptImportKey"));
    }
    let public = Handle(public);
    let mut secret: usize = 0;
    let silent_flag = if silent { NCRYPT_SILENT_FLAG } else { 0 };
    // SAFETY: свой закрытый и временный открытый ключи одного поставщика.
    let hr = unsafe { NCryptSecretAgreement(key, public.0, &mut secret, silent_flag) };
    if hr != 0 {
        return Err(key_error(hr, "NCryptSecretAgreement"));
    }
    let secret = Handle(secret);
    let mut z = Zeroizing::new(vec![0u8; size]);
    let mut got = 0u32;
    // SAFETY: буфер на размер координаты; `TRUNCATE` (сырой секрет) без параметров.
    let hr = unsafe { NCryptDeriveKey(secret.0, BCRYPT_KDF_RAW_SECRET, ptr::null(), z.as_mut_ptr(), z.len() as u32, &mut got, 0) };
    if hr != 0 {
        return Err(key_error(hr, "NCryptDeriveKey"));
    }
    z.truncate(got as usize);
    z.reverse();
    postman_smime::envelope::ecdh_cek(&z, wrap, data).ok_or_else(|| KeyError::Failed("ECDH: key unwrap".into()))
}

/// Снять ключ содержимого ключом CNG `key` (тесты зовут с временным ключом, без хранилища).
pub(super) fn decrypt_with(key: usize, wrap: &Wrap, data: &[u8], silent: bool, key_len: usize) -> Result<Cek, KeyError> {
    match wrap {
        Wrap::RsaPkcs1 | Wrap::RsaOaep(_) => rsa_decrypt(key, wrap, data, silent, key_len),
        Wrap::Ecdh { .. } => ecdh(key, wrap, data, silent),
        Wrap::Gost(_) => Err(KeyError::Unsupported("ГОСТ — через СКЗИ".into())),
        Wrap::Unsupported(n) => Err(KeyError::Unsupported(n.clone())),
    }
}

/// Свой сертификат подходит получателю.
fn matches(ctx: *const CERT_CONTEXT, rid: &Rid) -> bool {
    // SAFETY: контекст сертификата хранилища действителен на время перебора.
    let info = unsafe { &*(*ctx).pCertInfo };
    match rid {
        Rid::IssuerSerial { issuer, serial } => {
            // SAFETY: блобы `CERT_INFO` с длинами.
            let own_issuer = unsafe { std::slice::from_raw_parts(info.Issuer.pbData, info.Issuer.cbData as usize) };
            let own_serial = unsafe { std::slice::from_raw_parts(info.SerialNumber.pbData, info.SerialNumber.cbData as usize) };
            own_issuer == issuer.as_slice() && postman_smime::envelope::serial_matches(serial, own_serial)
        }
        Rid::Ski(ski) => {
            let mut buf = [0u8; 64];
            let mut len = buf.len() as u32;
            // SAFETY: буфер своего размера; свойство — SKI сертификата (из расширения или посчитанный ОС).
            let ok = unsafe { CertGetCertificateContextProperty(ctx, CERT_KEY_IDENTIFIER_PROP_ID, buf.as_mut_ptr().cast(), &mut len) };
            ok != 0 && !ski.is_empty() && &buf[..len as usize] == ski.as_slice()
        }
    }
}

/// Закрытый ключ сертификата (CNG): владение (освобождается в `Drop`, если наше) и сам хэндл.
fn private_key(ctx: *const CERT_CONTEXT, prompt: Prompt) -> Result<(Handle, usize), KeyError> {
    let mut key: HCRYPTPROV_OR_NCRYPT_KEY_HANDLE = 0;
    let mut spec = 0u32;
    let mut free = 0i32;
    let hwnd;
    let (flags, param): (u32, *const core::ffi::c_void) = match prompt {
        Prompt::Silent => (CRYPT_ACQUIRE_ONLY_NCRYPT_KEY_FLAG | CRYPT_ACQUIRE_SILENT_FLAG, ptr::null()),
        Prompt::Window(h) => {
            hwnd = h;
            (CRYPT_ACQUIRE_ONLY_NCRYPT_KEY_FLAG | CRYPT_ACQUIRE_WINDOW_HANDLE_FLAG, ptr::from_ref(&hwnd).cast())
        }
    };
    // SAFETY: контекст из хранилища; параметр — указатель на HWND (живёт до конца вызова).
    let ok = unsafe { CryptAcquireCertificatePrivateKey(ctx, flags, param, &mut key, &mut spec, &mut free) };
    if ok == 0 {
        // SAFETY: код последней ошибки потока.
        let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        let hr = if err & 0x8000_0000 != 0 { err as i32 } else { ((err & 0xFFFF) | 0x8007_0000) as i32 };
        return Err(match hr {
            NTE_NO_KEY => KeyError::Failed("no private key".into()),
            hr => key_error(hr, "CryptAcquireCertificatePrivateKey"),
        });
    }
    if spec != CERT_NCRYPT_KEY_SPEC {
        return Err(KeyError::Unsupported("CryptoAPI key (not CNG)".into()));
    }
    // Без `CACHE_FLAG` ключ освобождает вызывающий (если `free`); иначе — crypt32.
    Ok((Handle(if free != 0 { key } else { 0 }), key))
}

/// Снять ключ содержимого своими сертификатами `CurrentUser\My`.
pub(crate) fn unwrap_cek(env: &Envelope, prompt: Prompt) -> Result<Cek, KeyError> {
    let key_len = env.cipher.as_ref().map(|c| c.key_len()).unwrap_or(32);
    let name = wide("MY");
    let flags = CERT_SYSTEM_STORE_CURRENT_USER | CERT_STORE_READONLY_FLAG | CERT_STORE_OPEN_EXISTING_FLAG;
    // SAFETY: имя хранилища — UTF-16 с нулём; хранилище закрывается ниже.
    let store = unsafe { CertOpenStore(CERT_STORE_PROV_SYSTEM_W, 0, 0, flags, name.as_ptr().cast()) };
    if store.is_null() {
        return Err(KeyError::NotForYou(labels(env)));
    }
    let mut result: Option<Result<Cek, KeyError>> = None;
    let mut ctx: *const CERT_CONTEXT = ptr::null();
    'certs: loop {
        // SAFETY: перебор; предыдущий контекст освобождает сама функция.
        ctx = unsafe { CertEnumCertificatesInStore(store, ctx) };
        if ctx.is_null() {
            break;
        }
        for r in env.recipients.iter().filter(|r| matches(ctx, &r.rid)) {
            if !supported(&r.wrap) {
                result.get_or_insert(Err(KeyError::Unsupported(match &r.wrap {
                    Wrap::Unsupported(n) => n.clone(),
                    _ => "?".into(),
                })));
                continue;
            }
            let attempt =
                private_key(ctx, prompt).and_then(|(_owned, k)| decrypt_with(k, &r.wrap, &r.encrypted_key, prompt == Prompt::Silent, key_len));
            match attempt {
                Ok(cek) => {
                    result = Some(Ok(cek));
                    // SAFETY: перебор прерван — текущий контекст освобождаем сами.
                    unsafe { CertFreeCertificateContext(ctx) };
                    break 'certs;
                }
                // Нужен PIN или карта — это и показать (важнее «не для вас»).
                Err(e @ (KeyError::NeedPin | KeyError::NoCard | KeyError::Cancelled)) => result = Some(Err(e)),
                Err(e) => {
                    tracing::warn!("S/MIME key: {e:?}");
                    result.get_or_insert(Err(e));
                }
            }
        }
    }
    // SAFETY: хранилище открыто выше.
    unsafe { CertCloseStore(store, 0) };
    result.unwrap_or_else(|| Err(KeyError::NotForYou(labels(env))))
}

/// ГОСТ через СКЗИ (CryptoAPI, `skzi.rs`): свой сертификат получателя, ключ — в провайдере СКЗИ; согласование
/// ключа, сессионный ключ из `SIMPLEBLOB`, `CryptDecrypt` содержимого. [проверить с КриптоПро CSP]
pub(crate) fn gost_decrypt(env: &Envelope, prompt: Prompt) -> Result<super::skzi::Plain, KeyError> {
    let content = env.gost.as_ref().ok_or_else(|| KeyError::Unsupported("ГОСТ: нет параметров содержимого".into()))?;
    let name = wide("MY");
    let flags = CERT_SYSTEM_STORE_CURRENT_USER | CERT_STORE_READONLY_FLAG | CERT_STORE_OPEN_EXISTING_FLAG;
    // SAFETY: имя хранилища — UTF-16 с нулём; хранилище закрывается ниже.
    let store = unsafe { CertOpenStore(CERT_STORE_PROV_SYSTEM_W, 0, 0, flags, name.as_ptr().cast()) };
    if store.is_null() {
        return Err(KeyError::NotForYou(labels(env)));
    }
    let mut result: Option<Result<super::skzi::Plain, KeyError>> = None;
    let mut ctx: *const CERT_CONTEXT = ptr::null();
    'certs: loop {
        // SAFETY: перебор; предыдущий контекст освобождает сама функция.
        ctx = unsafe { CertEnumCertificatesInStore(store, ctx) };
        if ctx.is_null() {
            break;
        }
        for r in env.recipients.iter().filter(|r| matches(ctx, &r.rid)) {
            let Wrap::Gost(t) = &r.wrap else { continue };
            match gost_with_cert(ctx, t, content, prompt) {
                Ok(plain) => {
                    result = Some(Ok(plain));
                    // SAFETY: перебор прерван — текущий контекст освобождаем сами.
                    unsafe { CertFreeCertificateContext(ctx) };
                    break 'certs;
                }
                Err(e @ (KeyError::NeedPin | KeyError::NoCard | KeyError::Cancelled)) => result = Some(Err(e)),
                Err(e) => {
                    tracing::warn!("GOST key: {e:?}");
                    result.get_or_insert(Err(e));
                }
            }
        }
    }
    // SAFETY: хранилище открыто выше.
    unsafe { CertCloseStore(store, 0) };
    result.unwrap_or_else(|| Err(KeyError::NotForYou(labels(env))))
}

/// Ключ CryptoAPI: освобождается в `Drop`.
struct CapiKey(usize);

impl Drop for CapiKey {
    fn drop(&mut self) {
        if self.0 != 0 {
            // SAFETY: ключ получен от CryptoAPI в этом модуле.
            unsafe { CryptDestroyKey(self.0) };
        }
    }
}

fn last_error(what: &str) -> KeyError {
    // SAFETY: код последней ошибки потока.
    let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
    let hr = if err & 0x8000_0000 != 0 { err as i32 } else { ((err & 0xFFFF) | 0x8007_0000) as i32 };
    key_error(hr, what)
}

fn gost_with_cert(
    ctx: *const CERT_CONTEXT,
    t: &postman_smime::envelope::GostTransport,
    content: &postman_smime::envelope::GostContent,
    prompt: Prompt,
) -> Result<super::skzi::Plain, KeyError> {
    use super::skzi;
    let mut prov: HCRYPTPROV_OR_NCRYPT_KEY_HANDLE = 0;
    let mut spec = 0u32;
    let mut free = 0i32;
    let hwnd;
    let (flags, param): (u32, *const core::ffi::c_void) = match prompt {
        Prompt::Silent => (CRYPT_ACQUIRE_SILENT_FLAG, ptr::null()),
        Prompt::Window(h) => {
            hwnd = h;
            (CRYPT_ACQUIRE_WINDOW_HANDLE_FLAG, ptr::from_ref(&hwnd).cast())
        }
    };
    // SAFETY: контекст из хранилища; провайдер СКЗИ (CryptoAPI, не CNG — ключи ГОСТ КриптоПро в режиме CSP).
    if unsafe { CryptAcquireCertificatePrivateKey(ctx, flags, param, &mut prov, &mut spec, &mut free) } == 0 {
        return Err(last_error("CryptAcquireCertificatePrivateKey (GOST)"));
    }
    let release = Release(if free != 0 { prov } else { 0 });
    if spec == CERT_NCRYPT_KEY_SPEC {
        return Err(KeyError::Unsupported("ГОСТ через CNG (KSP СКЗИ) [проверить]".into()));
    }
    let mut user: usize = 0;
    // SAFETY: провайдер получен выше.
    if unsafe { CryptGetUserKey(prov, spec, &mut user) } == 0 {
        return Err(last_error("CryptGetUserKey"));
    }
    let user = CapiKey(user);
    let blob = skzi::public_key_blob(t).ok_or_else(|| KeyError::Unsupported("ГОСТ: эфемерный ключ".into()))?;
    let mut agree: usize = 0;
    // SAFETY: блоб с длиной; ключ согласования освобождается `CapiKey`.
    if unsafe { CryptImportKey(prov, blob.as_ptr(), blob.len() as u32, user.0, 0, &mut agree) } == 0 {
        return Err(last_error("CryptImportKey (VKO)"));
    }
    let agree = CapiKey(agree);
    let export = skzi::export_alg(t.key);
    // SAFETY: `KP_ALGID` — `ALG_ID` по указателю.
    if unsafe { CryptSetKeyParam(agree.0, KP_ALGID, ptr::from_ref(&export).cast(), 0) } == 0 {
        return Err(last_error("CryptSetKeyParam(KP_ALGID)"));
    }
    let simple = skzi::simple_blob(t).ok_or_else(|| KeyError::Unsupported("ГОСТ: ключ сеанса".into()))?;
    let mut session: usize = 0;
    // SAFETY: блоб с длиной; ключ сеанса освобождается `CapiKey`.
    if unsafe { CryptImportKey(prov, simple.as_ptr(), simple.len() as u32, agree.0, 0, &mut session) } == 0 {
        // Имитовставка ключа не сошлась — письмо повреждено или не для этого ключа.
        return Err(last_error("CryptImportKey (SIMPLEBLOB)"));
    }
    let session = CapiKey(session);
    let oid = skzi::cipher_oid(content);
    // SAFETY: набор параметров — строка с нулём; IV — 8 байтов.
    unsafe {
        CryptSetKeyParam(session.0, skzi::KP_CIPHEROID, oid.as_ptr(), 0);
        if CryptSetKeyParam(session.0, KP_IV, content.iv.as_ptr(), 0) == 0 {
            return Err(last_error("CryptSetKeyParam(KP_IV)"));
        }
    }
    let mut data = Zeroizing::new(content.data.clone());
    let mut len = data.len() as u32;
    // SAFETY: буфер на `len` байтов; CFB не меняет длину.
    if unsafe { CryptDecrypt(session.0, 0, 1, 0, data.as_mut_ptr(), &mut len) } == 0 {
        return Err(last_error("CryptDecrypt (GOST)"));
    }
    data.truncate(len as usize);
    drop(release);
    Ok(data)
}

/// Провайдер CryptoAPI: освобождается в `Drop`, если он наш.
struct Release(usize);

impl Drop for Release {
    fn drop(&mut self) {
        if self.0 != 0 {
            // SAFETY: провайдер получен `CryptAcquireCertificatePrivateKey` с признаком освобождения.
            unsafe { CryptReleaseContext(self.0, 0) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use postman_smime::Hash;
    use postman_smime::envelope::{self, Cipher};
    use postman_smime::testkit::{self, Head, Key, KeyAlg, Pki, Spec, To, issue};

    fn open_ksp() -> Handle {
        let mut p: usize = 0;
        // SAFETY: имя поставщика — константа.
        assert_eq!(unsafe { NCryptOpenStorageProvider(&mut p, MS_KEY_STORAGE_PROVIDER, 0) }, 0);
        Handle(p)
    }

    /// Временный ключ CNG (без имени — не сохраняется, хранилище сертификатов не трогается).
    fn import(provider: &Handle, blob_type: windows_sys::core::PCWSTR, blob: &[u8]) -> Handle {
        let mut k: usize = 0;
        // SAFETY: блоб с длиной.
        let hr = unsafe { NCryptImportKey(provider.0, 0, blob_type, ptr::null(), &mut k, blob.as_ptr(), blob.len() as u32, NCRYPT_SILENT_FLAG) };
        assert_eq!(hr, 0, "NCryptImportKey 0x{:08X}", hr as u32);
        Handle(k)
    }

    fn letter(to: &[To<'_>], cipher: Cipher, cek: &[u8]) -> Vec<u8> {
        let cms = testkit::enveloped(&testkit::html_part("<p>CNG</p>"), to, cipher, cek, false);
        testkit::encrypted_letter(
            &Head { from: "a@itt.test", to: "me@itt.test", subject: "s", date: "Wed, 21 Oct 2026 10:00:00 +0300" },
            &cms,
            matches!(cipher, Cipher::AesGcm(_)),
        )
    }

    /// RSA PKCS#1 v1.5 и OAEP (SHA-1, SHA-256) — `NCryptDecrypt` временным ключом из PKCS#8; испорченный PKCS#1 —
    /// случайный ключ (не ошибка).
    #[test]
    fn rsa_through_cng() {
        let pki = Pki::rsa("Root", "Смирнова", "me@itt.test");
        let Key::Rsa(k) = &pki.user_key else { unreachable!() };
        let pkcs8 = postman_smime::soft::pkcs8_der(k).unwrap();
        let ksp = open_ksp();
        let key = import(&ksp, NCRYPT_PKCS8_PRIVATE_KEY_BLOB, &pkcs8);
        let cek = [0x42u8; 32];
        for alg in [KeyAlg::RsaPkcs1, KeyAlg::RsaOaep(Hash::Sha1), KeyAlg::RsaOaep(Hash::Sha256)] {
            let m = letter(&[To { cert: &pki.user, key: &pki.user_key, alg, ski: false }], Cipher::AesCbc(256), &cek);
            let env = envelope::envelope(&m).unwrap();
            let r = &env.recipients[0];
            let got = decrypt_with(key.0, &r.wrap, &r.encrypted_key, true, 32).unwrap_or_else(|e| panic!("{alg:?}: {e:?}"));
            assert_eq!(got.as_slice(), &cek, "{alg:?}");
        }
        let m = letter(&[To { cert: &pki.user, key: &pki.user_key, alg: KeyAlg::RsaPkcs1, ski: false }], Cipher::AesCbc(256), &cek);
        let mut env = envelope::envelope(&m).unwrap();
        env.recipients[0].encrypted_key[7] ^= 0x10;
        let r = &env.recipients[0];
        let got = decrypt_with(key.0, &r.wrap, &r.encrypted_key, true, 32).unwrap();
        assert_eq!(got.len(), 32);
        assert_ne!(got.as_slice(), &cek);
        assert_eq!(envelope::open(&m, &got, &Default::default(), testkit::NOW), Err(envelope::OpenError::Damaged));
    }

    /// ECDH P-256: временный ключ ECDH из `BCRYPT_ECCPRIVATE_BLOB`, общий секрет CNG (разворот байтов) → тот же CEK.
    #[test]
    fn ecdh_through_cng() {
        let key = Key::p256("cng-ecdh");
        let root_key = Key::rsa('a');
        let root = issue(&Spec::ca("Root"), &root_key, None);
        let user = issue(&Spec::user("Смирнова", "me@itt.test"), &key, Some((&root, &root_key)));
        let Key::P256(sk) = &key else { unreachable!() };
        let point = sk.verifying_key().to_sec1_point(false);
        let mut blob = Vec::new();
        blob.extend_from_slice(&BCRYPT_ECDH_PRIVATE_P256_MAGIC.to_le_bytes());
        blob.extend_from_slice(&32u32.to_le_bytes());
        blob.extend_from_slice(&point.as_bytes()[1..]);
        blob.extend_from_slice(&sk.to_bytes());
        let ksp = open_ksp();
        let cng = import(&ksp, BCRYPT_ECCPRIVATE_BLOB, &blob);
        let cek = [0x17u8; 32];
        let m = letter(&[To { cert: &user, key: &key, alg: KeyAlg::Ecdh(Hash::Sha256, 256), ski: false }], Cipher::AesGcm(256), &cek);
        let env = envelope::envelope(&m).unwrap();
        let r = &env.recipients[0];
        let got = decrypt_with(cng.0, &r.wrap, &r.encrypted_key, true, 32).unwrap();
        assert_eq!(got.as_slice(), &cek);
    }

    /// Хранилище `My` без нашего сертификата: «не для вас» с подписью получателя (ничего не импортируется).
    #[test]
    fn not_in_my_store() {
        let pki = Pki::rsa("Postman Test Absent Root", "Никто", "nobody@itt.test");
        let m = letter(&[To { cert: &pki.user, key: &pki.user_key, alg: KeyAlg::RsaPkcs1, ski: false }], Cipher::AesCbc(256), &[1; 32]);
        let env = envelope::envelope(&m).unwrap();
        match unwrap_cek(&env, Prompt::Silent) {
            Err(KeyError::NotForYou(l)) => assert!(l[0].contains("Postman Test Absent Root"), "{l:?}"),
            other => panic!("{other:?}"),
        }
    }

    /// Полный путь (12.2б): сертификат и ключ импортированы в `CurrentUser\My` (программный KSP, имя
    /// `Postman test <время>`) → письмо на него снимается из хранилища → уборка: ни сертификата, ни ключа.
    /// Меняет хранилище пользователя — только вручную: `POSTMAN_TEST_MY_STORE=1 cargo test -p postman-core
    /// my_store_round_trip -- --ignored` [проверить].
    #[test]
    #[ignore = "импортирует тестовый сертификат в хранилище My (с уборкой)"]
    fn my_store_round_trip() {
        if std::env::var_os("POSTMAN_TEST_MY_STORE").is_none() {
            return;
        }
        let pki = Pki::rsa("Postman Test Root", "Postman test", "me@itt.test");
        let Key::Rsa(k) = &pki.user_key else { unreachable!() };
        let pkcs8 = postman_smime::soft::pkcs8_der(k).unwrap();
        let name = wide(&format!("Postman test {}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let ksp = open_ksp();
        let mut buffer =
            BCryptBuffer { cbBuffer: (name.len() * 2) as u32, BufferType: NCRYPTBUFFER_PKCS_KEY_NAME, pvBuffer: name.as_ptr() as *mut _ };
        let desc = BCryptBufferDesc { ulVersion: 0, cBuffers: 1, pBuffers: &mut buffer };
        let mut key: usize = 0;
        // SAFETY: блоб PKCS#8 и имя ключа; ключ сохраняется в KSP и удаляется в конце.
        let hr = unsafe {
            NCryptImportKey(
                ksp.0,
                0,
                NCRYPT_PKCS8_PRIVATE_KEY_BLOB,
                &desc,
                &mut key,
                pkcs8.as_ptr(),
                pkcs8.len() as u32,
                NCRYPT_OVERWRITE_KEY_FLAG | NCRYPT_SILENT_FLAG,
            )
        };
        assert_eq!(hr, 0, "NCryptImportKey 0x{:08X}", hr as u32);
        let my = wide("MY");
        // SAFETY: хранилище пользователя на запись; закрывается в конце.
        let store = unsafe { CertOpenStore(CERT_STORE_PROV_SYSTEM_W, 0, 0, CERT_SYSTEM_STORE_CURRENT_USER, my.as_ptr().cast()) };
        assert!(!store.is_null());
        let mut ctx: *mut CERT_CONTEXT = ptr::null_mut();
        // SAFETY: DER сертификата с длиной; контекст удаляется в конце.
        let ok = unsafe {
            CertAddEncodedCertificateToStore(
                store,
                X509_ASN_ENCODING | PKCS_7_ASN_ENCODING,
                pki.user.der.as_ptr(),
                pki.user.der.len() as u32,
                CERT_STORE_ADD_REPLACE_EXISTING,
                &mut ctx,
            )
        };
        assert!(ok != 0);
        let mut provider = wide("Microsoft Software Key Storage Provider");
        let mut container = name.clone();
        let info = CRYPT_KEY_PROV_INFO {
            pwszContainerName: container.as_mut_ptr(),
            pwszProvName: provider.as_mut_ptr(),
            dwProvType: 0,
            dwFlags: 0,
            cProvParam: 0,
            rgProvParam: ptr::null_mut(),
            dwKeySpec: 0,
        };
        // SAFETY: свойство — структура с указателями на живые строки.
        unsafe { CertSetCertificateContextProperty(ctx, CERT_KEY_PROV_INFO_PROP_ID, 0, ptr::from_ref(&info).cast()) };
        let m = letter(&[To { cert: &pki.user, key: &pki.user_key, alg: KeyAlg::RsaOaep(Hash::Sha256), ski: false }], Cipher::AesGcm(256), &[5; 32]);
        let got = unwrap_cek(&envelope::envelope(&m).unwrap(), Prompt::Silent);
        // Уборка — до проверок.
        // SAFETY: контекст и ключ созданы выше.
        unsafe {
            CertDeleteCertificateFromStore(ctx);
            NCryptDeleteKey(key, 0);
            CertCloseStore(store, 0);
        }
        assert_eq!(got.unwrap().as_slice(), &[5; 32]);
    }

    /// Коды CNG → строки интерфейса.
    #[test]
    fn error_codes() {
        assert_eq!(key_error(NTE_SILENT_CONTEXT, "x"), KeyError::NeedPin);
        assert_eq!(key_error(SCARD_W_REMOVED_CARD, "x"), KeyError::NoCard);
        assert_eq!(key_error(SCARD_W_CANCELLED_BY_USER, "x"), KeyError::Cancelled);
        assert!(matches!(key_error(0x8009_0016u32 as i32, "x"), KeyError::Failed(_)));
    }
}
