//! Windows: подпись исходящих своим сертификатом (S5-12 Ф3, 12.3б, `docs/SMIME.md` §4, §6.4).
//!
//! Сертификат — из `CurrentUser\My` по адресу «От» (SAN или `emailAddress`), годный для подписи почты, действующий,
//! с закрытым ключом; из нескольких — с самым поздним сроком. Ключ не покидает поставщика: RSA и ECDSA — CNG
//! `NCryptSignHash` (хэш считается здесь; ECDSA `r‖s` → DER), ГОСТ — только сертифицированное СКЗИ через CryptoAPI
//! (решение владельца 2026-10-22): `CryptCreateHash` + `HP_HASHVAL` + `CryptSignHash`; нет СКЗИ — «нужно СКЗИ».
//! Промежуточные сертификаты — цепочкой Windows (`CertGetCertificateChain` по своему сертификату), без корня.

use std::ptr;

use postman_smime::sign::{SignError, Signer, ecdsa_der, own_certificate};
use postman_smime::{Hash, Scheme};
use windows_sys::Win32::Security::Cryptography::*;

const NTE_SILENT_CONTEXT: i32 = 0x8009_0022u32 as i32;
const NTE_DEVICE_NOT_READY: i32 = 0x8009_0030u32 as i32;
const SCARD_W_REMOVED_CARD: i32 = 0x8010_0069u32 as i32;
const SCARD_E_NO_SMARTCARD: i32 = 0x8010_000Cu32 as i32;
const SCARD_W_CANCELLED_BY_USER: i32 = 0x8010_006Eu32 as i32;
const ERROR_CANCELLED_HR: i32 = 0x8007_04C7u32 as i32;
/// `ALG_ID` хэша «Стрибог» в CryptoAPI КриптоПро [проверить]; `HP_HASHVAL`.
const CALG_GR3411_2012_256: u32 = 0x8021;
const CALG_GR3411_2012_512: u32 = 0x8022;
const HP_HASHVAL: u32 = 2;

fn sign_error(hr: i32, what: &str) -> SignError {
    match hr {
        NTE_SILENT_CONTEXT => SignError::NeedPin,
        NTE_DEVICE_NOT_READY | SCARD_W_REMOVED_CARD | SCARD_E_NO_SMARTCARD => SignError::NoCard,
        SCARD_W_CANCELLED_BY_USER | ERROR_CANCELLED_HR => SignError::Cancelled,
        _ => SignError::Failed(format!("{what}: 0x{:08X}", hr as u32)),
    }
}

fn last_error(what: &str) -> SignError {
    // SAFETY: код последней ошибки потока.
    let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
    let hr = if err & 0x8000_0000 != 0 { err as i32 } else { ((err & 0xFFFF) | 0x8007_0000) as i32 };
    sign_error(hr, what)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Подписант CNG: сертификат, промежуточные, ключ (хэндл — у вызывающего).
pub(super) struct CngSigner {
    pub(super) cert: Vec<u8>,
    pub(super) chain: Vec<Vec<u8>>,
    pub(super) scheme: Scheme,
    pub(super) key: usize,
    pub(super) silent: bool,
}

impl Signer for CngSigner {
    fn certificate(&self) -> &[u8] {
        &self.cert
    }

    fn chain(&self) -> &[Vec<u8>] {
        &self.chain
    }

    fn scheme(&self) -> Scheme {
        self.scheme
    }

    fn sign(&self, digest: &[u8]) -> Result<Vec<u8>, SignError> {
        let alg = match self.scheme.hash() {
            Hash::Sha1 => wide("SHA1"),
            Hash::Sha384 => wide("SHA384"),
            Hash::Sha512 => wide("SHA512"),
            _ => wide("SHA256"),
        };
        let pkcs1 = BCRYPT_PKCS1_PADDING_INFO { pszAlgId: alg.as_ptr() };
        let (padding, flags): (*const core::ffi::c_void, u32) = match self.scheme {
            Scheme::RsaPkcs1(_) => (ptr::from_ref(&pkcs1).cast(), NCRYPT_PAD_PKCS1_FLAG),
            Scheme::Ecdsa(_) => (ptr::null(), 0),
            _ => return Err(SignError::Failed("CNG: схема не поддерживается".into())),
        };
        let silent = if self.silent { NCRYPT_SILENT_FLAG } else { 0 };
        let mut out = vec![0u8; 1024];
        let mut got = 0u32;
        // SAFETY: хэш и буфер с длинами; заполнение живёт до конца вызова; ключ — открытый хэндл.
        let hr = unsafe {
            NCryptSignHash(self.key, padding, digest.as_ptr(), digest.len() as u32, out.as_mut_ptr(), out.len() as u32, &mut got, flags | silent)
        };
        if hr != 0 {
            return Err(sign_error(hr, "NCryptSignHash"));
        }
        out.truncate(got as usize);
        match self.scheme {
            Scheme::Ecdsa(_) => ecdsa_der(&out).ok_or_else(|| SignError::Failed("ECDSA".into())),
            _ => Ok(out),
        }
    }
}

/// Подписант ГОСТ через СКЗИ (CryptoAPI). [проверить с КриптоПро CSP]
struct SkziSigner {
    cert: Vec<u8>,
    chain: Vec<Vec<u8>>,
    scheme: Scheme,
    prov: usize,
    spec: u32,
}

impl Signer for SkziSigner {
    fn certificate(&self) -> &[u8] {
        &self.cert
    }

    fn chain(&self) -> &[Vec<u8>] {
        &self.chain
    }

    fn scheme(&self) -> Scheme {
        self.scheme
    }

    fn sign(&self, digest: &[u8]) -> Result<Vec<u8>, SignError> {
        let alg = if self.scheme == Scheme::Gost512 { CALG_GR3411_2012_512 } else { CALG_GR3411_2012_256 };
        let mut hash: usize = 0;
        // SAFETY: провайдер СКЗИ открыт; хэш уничтожается ниже.
        if unsafe { CryptCreateHash(self.prov, alg, 0, 0, &mut hash) } == 0 {
            return Err(last_error("CryptCreateHash (GOST)"));
        }
        let result = (|| {
            // SAFETY: значение хэша — `digest` (уже посчитан «Стрибогом» здесь).
            if unsafe { CryptSetHashParam(hash, HP_HASHVAL, digest.as_ptr(), 0) } == 0 {
                return Err(last_error("CryptSetHashParam"));
            }
            let mut len = 0u32;
            // SAFETY: первый вызов узнаёт длину подписи.
            if unsafe { CryptSignHashW(hash, self.spec, ptr::null(), 0, ptr::null_mut(), &mut len) } == 0 {
                return Err(last_error("CryptSignHash"));
            }
            let mut sig = vec![0u8; len as usize];
            // SAFETY: буфер на `len` байтов.
            if unsafe { CryptSignHashW(hash, self.spec, ptr::null(), 0, sig.as_mut_ptr(), &mut len) } == 0 {
                return Err(last_error("CryptSignHash"));
            }
            sig.truncate(len as usize);
            // CryptoAPI отдаёт подпись в обратном порядке байтов; в CMS — как в RFC 4491 [проверить].
            sig.reverse();
            Ok(sig)
        })();
        // SAFETY: хэш создан выше.
        unsafe { CryptDestroyHash(hash) };
        result
    }
}

/// Промежуточные сертификаты своего сертификата (цепочка Windows без проверки отзыва), без корня.
fn intermediates(ctx: *const CERT_CONTEXT) -> Vec<Vec<u8>> {
    let para = CERT_CHAIN_PARA { cbSize: size_of::<CERT_CHAIN_PARA>() as u32, ..Default::default() };
    let mut chain: *mut CERT_CHAIN_CONTEXT = ptr::null_mut();
    // SAFETY: свой сертификат из хранилища; контекст цепочки освобождается ниже.
    if unsafe { CertGetCertificateChain(ptr::null_mut(), ctx, ptr::null(), ptr::null_mut(), &para, 0, ptr::null(), &mut chain) } == 0
        || chain.is_null()
    {
        return Vec::new();
    }
    let mut out = Vec::new();
    // SAFETY: простая цепочка 0 и её элементы действительны до освобождения контекста.
    unsafe {
        if (*chain).cChain > 0 {
            let simple = *(*chain).rgpChain;
            let n = (*simple).cElement as usize;
            for i in 1..n {
                let el = *(*simple).rgpElement.add(i);
                let c = (*el).pCertContext;
                let info = &*(*c).pCertInfo;
                let issuer = std::slice::from_raw_parts(info.Issuer.pbData, info.Issuer.cbData as usize);
                let subject = std::slice::from_raw_parts(info.Subject.pbData, info.Subject.cbData as usize);
                if issuer == subject {
                    continue; // корень
                }
                out.push(std::slice::from_raw_parts((*c).pbCertEncoded, (*c).cbCertEncoded as usize).to_vec());
            }
        }
        CertFreeCertificateChain(chain);
    }
    out
}

/// Есть ли свой сертификат подписи для адреса `from` (годный, действующий, с ключом) — без доступа к ключу.
pub(super) fn can_sign(from: &str, now: i64) -> bool {
    let name = wide("MY");
    let flags = CERT_SYSTEM_STORE_CURRENT_USER | CERT_STORE_READONLY_FLAG | CERT_STORE_OPEN_EXISTING_FLAG;
    // SAFETY: имя хранилища — UTF-16 с нулём; хранилище закрывается ниже.
    let store = unsafe { CertOpenStore(CERT_STORE_PROV_SYSTEM_W, 0, 0, flags, name.as_ptr().cast()) };
    if store.is_null() {
        return false;
    }
    let mut found = false;
    let mut ctx: *const CERT_CONTEXT = ptr::null();
    loop {
        // SAFETY: перебор; предыдущий контекст освобождает сама функция.
        ctx = unsafe { CertEnumCertificatesInStore(store, ctx) };
        if ctx.is_null() {
            break;
        }
        // SAFETY: байты сертификата действительны до следующего шага перебора.
        let der = unsafe { std::slice::from_raw_parts((*ctx).pbCertEncoded, (*ctx).cbCertEncoded as usize) };
        let Some((info, can, Some(_))) = own_certificate(der) else { continue };
        let mut len = 0u32;
        // SAFETY: только размер свойства — есть ли закрытый ключ.
        let has_key = unsafe { CertGetCertificateContextProperty(ctx, CERT_KEY_PROV_INFO_PROP_ID, ptr::null_mut(), &mut len) } != 0;
        if can && has_key && info.not_before <= now && now <= info.not_after && info.emails.iter().any(|e| e.eq_ignore_ascii_case(from.trim())) {
            found = true;
            // SAFETY: перебор прерван — текущий контекст освобождаем сами.
            unsafe { CertFreeCertificateContext(ctx) };
            break;
        }
    }
    // SAFETY: хранилище открыто выше.
    unsafe { CertCloseStore(store, 0) };
    found
}

/// Подписать письмо `raw` сертификатом адреса `from` из `CurrentUser\My`; `hwnd` — окно для PIN ОС (`None` — молча).
pub(super) fn sign_message(raw: &[u8], from: &str, hwnd: Option<isize>, now: i64, gost_provider: Option<String>) -> Result<Vec<u8>, SignError> {
    let name = wide("MY");
    let flags = CERT_SYSTEM_STORE_CURRENT_USER | CERT_STORE_READONLY_FLAG | CERT_STORE_OPEN_EXISTING_FLAG;
    // SAFETY: имя хранилища — UTF-16 с нулём; хранилище закрывается ниже.
    let store = unsafe { CertOpenStore(CERT_STORE_PROV_SYSTEM_W, 0, 0, flags, name.as_ptr().cast()) };
    if store.is_null() {
        return Err(SignError::NoCertificate(from.into()));
    }
    // Лучший сертификат: годный, действующий, с ключом; самый поздний срок.
    let mut best: Option<(*const CERT_CONTEXT, i64, Scheme)> = None;
    let mut ctx: *const CERT_CONTEXT = ptr::null();
    loop {
        // SAFETY: перебор; предыдущий контекст освобождает сама функция (кроме выбранного — он продублирован).
        ctx = unsafe { CertEnumCertificatesInStore(store, ctx) };
        if ctx.is_null() {
            break;
        }
        // SAFETY: байты сертификата действительны до следующего шага перебора.
        let der = unsafe { std::slice::from_raw_parts((*ctx).pbCertEncoded, (*ctx).cbCertEncoded as usize) };
        let Some((info, can, Some(scheme))) = own_certificate(der) else { continue };
        let mine = info.emails.iter().any(|e| e.eq_ignore_ascii_case(from.trim()));
        let valid = info.not_before <= now && now <= info.not_after;
        let mut len = 0u32;
        // SAFETY: только размер свойства — есть ли закрытый ключ.
        let has_key = unsafe { CertGetCertificateContextProperty(ctx, CERT_KEY_PROV_INFO_PROP_ID, ptr::null_mut(), &mut len) } != 0;
        if can && mine && valid && has_key && best.as_ref().is_none_or(|b| info.not_after > b.1) {
            if let Some((old, ..)) = best.take() {
                // SAFETY: дубликат, сделанный ниже.
                unsafe { CertFreeCertificateContext(old) };
            }
            // SAFETY: дубликат переживает перебор; освобождается ниже.
            best = Some((unsafe { CertDuplicateCertificateContext(ctx) }, info.not_after, scheme));
        }
    }
    let result = match best {
        None => Err(SignError::NoCertificate(from.into())),
        Some((cert, _, scheme)) => {
            let r = sign_with(cert, scheme, raw, hwnd, now, gost_provider);
            // SAFETY: дубликат из перебора.
            unsafe { CertFreeCertificateContext(cert) };
            r
        }
    };
    // SAFETY: хранилище открыто выше.
    unsafe { CertCloseStore(store, 0) };
    result
}

fn sign_with(
    ctx: *const CERT_CONTEXT,
    scheme: Scheme,
    raw: &[u8],
    hwnd: Option<isize>,
    now: i64,
    gost_provider: Option<String>,
) -> Result<Vec<u8>, SignError> {
    let gost = matches!(scheme, Scheme::Gost256 | Scheme::Gost512);
    if gost && gost_provider.is_none() {
        return Err(SignError::NeedSkzi);
    }
    // SAFETY: байты своего сертификата.
    let cert = unsafe { std::slice::from_raw_parts((*ctx).pbCertEncoded, (*ctx).cbCertEncoded as usize) }.to_vec();
    let chain = intermediates(ctx);
    let mut key: HCRYPTPROV_OR_NCRYPT_KEY_HANDLE = 0;
    let mut spec = 0u32;
    let mut free = 0i32;
    let window;
    let (flags, param): (u32, *const core::ffi::c_void) = match hwnd {
        None => (CRYPT_ACQUIRE_ALLOW_NCRYPT_KEY_FLAG | CRYPT_ACQUIRE_SILENT_FLAG, ptr::null()),
        Some(h) => {
            window = h;
            (CRYPT_ACQUIRE_ALLOW_NCRYPT_KEY_FLAG | CRYPT_ACQUIRE_WINDOW_HANDLE_FLAG, ptr::from_ref(&window).cast())
        }
    };
    // SAFETY: свой сертификат; параметр — указатель на HWND, живёт до конца вызова.
    if unsafe { CryptAcquireCertificatePrivateKey(ctx, flags, param, &mut key, &mut spec, &mut free) } == 0 {
        return Err(last_error("CryptAcquireCertificatePrivateKey"));
    }
    let result = if spec == CERT_NCRYPT_KEY_SPEC {
        if gost {
            Err(SignError::Failed("ГОСТ через CNG (KSP СКЗИ) [проверить]".into()))
        } else {
            postman_smime::sign::sign_message(raw, &CngSigner { cert, chain, scheme, key, silent: hwnd.is_none() }, now)
        }
    } else if gost {
        postman_smime::sign::sign_message(raw, &SkziSigner { cert, chain, scheme, prov: key, spec }, now)
    } else {
        Err(SignError::Failed("ключ CryptoAPI (не CNG) — не поддерживается".into()))
    };
    if free != 0 {
        // SAFETY: ключ или провайдер получены выше с признаком освобождения.
        unsafe {
            if spec == CERT_NCRYPT_KEY_SPEC {
                NCryptFreeObject(key);
            } else {
                CryptReleaseContext(key, 0);
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use postman_smime::testkit::{Key, NOW, Pki};
    use postman_smime::{Kind, Status};

    struct Handle(usize);

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: хэндл получен в тесте.
            unsafe { NCryptFreeObject(self.0) };
        }
    }

    fn import(blob_type: windows_sys::core::PCWSTR, blob: &[u8]) -> (Handle, Handle) {
        let mut p: usize = 0;
        // SAFETY: имя поставщика — константа.
        assert_eq!(unsafe { NCryptOpenStorageProvider(&mut p, MS_KEY_STORAGE_PROVIDER, 0) }, 0);
        let p = Handle(p);
        let mut k: usize = 0;
        // SAFETY: временный ключ (без имени — не сохраняется).
        let hr = unsafe { NCryptImportKey(p.0, 0, blob_type, ptr::null(), &mut k, blob.as_ptr(), blob.len() as u32, NCRYPT_SILENT_FLAG) };
        assert_eq!(hr, 0, "NCryptImportKey 0x{:08X}", hr as u32);
        (p, Handle(k))
    }

    const LETTER: &[u8] = b"From: user1@itt.test\r\nTo: me@itt.test\r\nSubject: CNG\r\nDate: Wed, 21 Oct 2026 10:00:00 +0300\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=us-ascii\r\n\r\nSigned by CNG.\r\n";

    /// RSA PKCS#1 и ECDSA P-256 временными ключами CNG (хранилище не трогается): своя проверка — «верна».
    #[test]
    fn cng_signatures_verify() {
        let pki = Pki::rsa("Postman Test Root", "Иванов", "user1@itt.test");
        let (_p, key) = import(NCRYPT_PKCS8_PRIVATE_KEY_BLOB, &pki.user_key.rsa_pkcs8().unwrap());
        let s = CngSigner { cert: pki.user.der.clone(), chain: Vec::new(), scheme: Scheme::RsaPkcs1(Hash::Sha256), key: key.0, silent: true };
        let signed = postman_smime::sign::sign_message(LETTER, &s, NOW as i64).unwrap();
        let r = postman_worker::smime::verify(&signed, &pki.trust(), NOW);
        assert_eq!((r.kind, &r.status), (Kind::Detached, &Status::Valid), "{r:?}");

        let ec = Pki::build(Key::rsa('a'), Key::p256("cng-sign"), "Postman Test Root", "Иванов", "user1@itt.test");
        let (xy, d) = ec.user_key.ec_raw().unwrap();
        let mut blob = Vec::new();
        blob.extend_from_slice(&BCRYPT_ECDSA_PRIVATE_P256_MAGIC.to_le_bytes());
        blob.extend_from_slice(&32u32.to_le_bytes());
        blob.extend_from_slice(&xy);
        blob.extend_from_slice(&d);
        let (_p, key) = import(BCRYPT_ECCPRIVATE_BLOB, &blob);
        let s = CngSigner { cert: ec.user.der.clone(), chain: Vec::new(), scheme: Scheme::Ecdsa(Hash::Sha256), key: key.0, silent: true };
        let signed = postman_smime::sign::sign_message(LETTER, &s, NOW as i64).unwrap();
        let r = postman_worker::smime::verify(&signed, &ec.trust(), NOW);
        assert_eq!((r.kind, &r.status), (Kind::Detached, &Status::Valid), "{r:?}");
    }

    /// Адреса в `My` нет — «нет сертификата»; ГОСТ без СКЗИ — «нужно СКЗИ» (до доступа к ключу).
    #[test]
    fn no_certificate_and_no_skzi() {
        assert_eq!(
            sign_message(LETTER, "nobody-postman-test@itt.test", None, NOW as i64, None),
            Err(SignError::NoCertificate("nobody-postman-test@itt.test".into()))
        );
        assert_eq!(sign_error(NTE_SILENT_CONTEXT, "x"), SignError::NeedPin);
        assert_eq!(sign_error(SCARD_W_REMOVED_CARD, "x"), SignError::NoCard);
    }
}
