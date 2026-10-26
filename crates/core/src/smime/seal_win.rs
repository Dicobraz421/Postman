//! Windows: шифрование исходящих (S5-12 Ф4, `docs/SMIME.md` §6.4): случайные числа и ECDH — CNG (BCrypt), свои
//! сертификаты шифрования из `CurrentUser\My` (себе — всегда), сертификаты адресатов из `CurrentUser\AddressBook`
//! (их проверяет воркер), письмо ГОСТ — только СКЗИ через CryptoAPI (решение «ГОСТ-ключи»).

use std::ptr;

use postman_smime::envelope::{GostKey, GostTransport};
use postman_smime::prepare::RecipientKey;
use postman_smime::seal::{GostSealed, Ops, SealError, To};
use windows_sys::Win32::Security::Cryptography::*;
use zeroize::Zeroizing;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn failed(what: &str, st: i32) -> SealError {
    SealError::Failed(format!("{what}: 0x{:08X}", st as u32))
}

/// Генератор ОС и ECDH CNG.
pub(crate) struct WindowsOps;

struct Alg(BCRYPT_ALG_HANDLE);
impl Drop for Alg {
    fn drop(&mut self) {
        // SAFETY: провайдер открыт в этом модуле.
        unsafe { BCryptCloseAlgorithmProvider(self.0, 0) };
    }
}
struct Key(BCRYPT_KEY_HANDLE);
impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: ключ создан в этом модуле.
        unsafe { BCryptDestroyKey(self.0) };
    }
}
struct Secret(BCRYPT_SECRET_HANDLE);
impl Drop for Secret {
    fn drop(&mut self) {
        // SAFETY: секрет создан в этом модуле.
        unsafe { BCryptDestroySecret(self.0) };
    }
}

impl Ops for WindowsOps {
    fn random(&self, out: &mut [u8]) {
        // SAFETY: буфер своей длины; системный генератор.
        unsafe { BCryptGenRandom(ptr::null_mut(), out.as_mut_ptr(), out.len() as u32, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    }

    fn ecdh(&self, bits: u16, peer: &[u8]) -> Result<(Vec<u8>, Zeroizing<Vec<u8>>), SealError> {
        let (alg_id, magic, size) = match bits {
            384 => (BCRYPT_ECDH_P384_ALGORITHM, BCRYPT_ECDH_PUBLIC_P384_MAGIC, 48usize),
            _ => (BCRYPT_ECDH_P256_ALGORITHM, BCRYPT_ECDH_PUBLIC_P256_MAGIC, 32usize),
        };
        if peer.len() != 1 + 2 * size || peer[0] != 4 {
            return Err(SealError::Failed("EC point".into()));
        }
        let mut h: BCRYPT_ALG_HANDLE = ptr::null_mut();
        // SAFETY: имя алгоритма — константа; провайдер закрывается `Alg`.
        let st = unsafe { BCryptOpenAlgorithmProvider(&mut h, alg_id, ptr::null(), 0) };
        if st != 0 {
            return Err(failed("BCryptOpenAlgorithmProvider", st));
        }
        let alg = Alg(h);
        let mut k: BCRYPT_KEY_HANDLE = ptr::null_mut();
        // SAFETY: эфемерная пара на кривой провайдера.
        let st = unsafe { BCryptGenerateKeyPair(alg.0, &mut k, u32::from(bits), 0) };
        if st != 0 {
            return Err(failed("BCryptGenerateKeyPair", st));
        }
        let eph = Key(k);
        // SAFETY: пара создана выше.
        let st = unsafe { BCryptFinalizeKeyPair(eph.0, 0) };
        if st != 0 {
            return Err(failed("BCryptFinalizeKeyPair", st));
        }
        let mut blob = vec![0u8; 8 + 2 * size];
        let mut got = 0u32;
        // SAFETY: буфер на заголовок и точку.
        let st = unsafe { BCryptExportKey(eph.0, ptr::null_mut(), BCRYPT_ECCPUBLIC_BLOB, blob.as_mut_ptr(), blob.len() as u32, &mut got, 0) };
        if st != 0 {
            return Err(failed("BCryptExportKey", st));
        }
        let mut own = vec![4u8];
        own.extend_from_slice(&blob[8..8 + 2 * size]);
        let mut pb = Vec::with_capacity(8 + 2 * size);
        pb.extend_from_slice(&magic.to_le_bytes());
        pb.extend_from_slice(&(size as u32).to_le_bytes());
        pb.extend_from_slice(&peer[1..]);
        let mut p: BCRYPT_KEY_HANDLE = ptr::null_mut();
        // SAFETY: блоб открытого ключа получателя с длиной.
        let st = unsafe { BCryptImportKeyPair(alg.0, ptr::null_mut(), BCRYPT_ECCPUBLIC_BLOB, &mut p, pb.as_ptr(), pb.len() as u32, 0) };
        if st != 0 {
            return Err(failed("BCryptImportKeyPair", st));
        }
        let peer_key = Key(p);
        let mut s: BCRYPT_SECRET_HANDLE = ptr::null_mut();
        // SAFETY: эфемерный закрытый и открытый ключ получателя одной кривой.
        let st = unsafe { BCryptSecretAgreement(eph.0, peer_key.0, &mut s, 0) };
        if st != 0 {
            return Err(failed("BCryptSecretAgreement", st));
        }
        let secret = Secret(s);
        let mut z = Zeroizing::new(vec![0u8; size]);
        let mut got = 0u32;
        // SAFETY: буфер на координату; `TRUNCATE` без параметров.
        let st = unsafe { BCryptDeriveKey(secret.0, BCRYPT_KDF_RAW_SECRET, ptr::null(), z.as_mut_ptr(), z.len() as u32, &mut got, 0) };
        if st != 0 {
            return Err(failed("BCryptDeriveKey", st));
        }
        z.truncate(got as usize);
        // Сырой секрет CNG — little-endian.
        z.reverse();
        Ok((own, z))
    }
}

/// Сертификаты системного хранилища пользователя `name` (DER).
fn store_certs(name: &str) -> Vec<Vec<u8>> {
    let n = wide(name);
    let flags = CERT_SYSTEM_STORE_CURRENT_USER | CERT_STORE_READONLY_FLAG | CERT_STORE_OPEN_EXISTING_FLAG;
    // SAFETY: имя хранилища — UTF-16 с нулём; хранилище закрывается ниже.
    let store = unsafe { CertOpenStore(CERT_STORE_PROV_SYSTEM_W, 0, 0, flags, n.as_ptr().cast()) };
    if store.is_null() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut ctx: *const CERT_CONTEXT = ptr::null();
    loop {
        // SAFETY: перебор; предыдущий контекст освобождает сама функция.
        ctx = unsafe { CertEnumCertificatesInStore(store, ctx) };
        if ctx.is_null() {
            break;
        }
        // SAFETY: байты копируются сразу.
        out.push(unsafe { std::slice::from_raw_parts((*ctx).pbCertEncoded, (*ctx).cbCertEncoded as usize) }.to_vec());
    }
    // SAFETY: хранилище открыто выше.
    unsafe { CertCloseStore(store, 0) };
    out
}

/// Свои сертификаты для адреса `from` (`My`, с закрытым ключом) — кандидаты «себе»; проверяет воркер.
pub(crate) fn own_certs(from: &str) -> Vec<Vec<u8>> {
    store_certs("MY")
        .into_iter()
        .filter(|der| {
            postman_smime::sign::own_certificate(der).is_some_and(|(info, _, _)| info.emails.iter().any(|e| e.eq_ignore_ascii_case(from.trim())))
        })
        .collect()
}

/// «Другие пользователи» (`AddressBook`) — все: адреса узнает воркер.
pub(crate) fn address_book() -> Vec<Vec<u8>> {
    store_certs("AddressBook")
}

// --- ГОСТ через СКЗИ ---

/// `ALG_ID` КриптоПро [проверить]: эфемерный ключ согласования 2012-256 и 2012-512, тип провайдера 2012.
const CALG_DH_GR3410_12_256_EPHEM: u32 = 0xAA46;
const CALG_DH_GR3410_12_512_EPHEM: u32 = 0xAA42;
const PROV_GOST_2012_256: u32 = 80;
const PROV_GOST_2012_512: u32 = 81;

struct Prov(usize);
impl Drop for Prov {
    fn drop(&mut self) {
        // SAFETY: контекст получен в этом модуле.
        unsafe { CryptReleaseContext(self.0, 0) };
    }
}
struct CKey(usize);
impl Drop for CKey {
    fn drop(&mut self) {
        // SAFETY: ключ CryptoAPI создан в этом модуле.
        unsafe { CryptDestroyKey(self.0) };
    }
}

fn last(what: &str) -> SealError {
    // SAFETY: код последней ошибки потока.
    let e = unsafe { windows_sys::Win32::Foundation::GetLastError() };
    SealError::Failed(format!("{what}: {e:#x}"))
}

fn export(key: usize, with: usize, kind: u32) -> Result<Vec<u8>, SealError> {
    let mut len = 0u32;
    // SAFETY: первый вызов — длина, второй — данные.
    if unsafe { CryptExportKey(key, with, kind, 0, ptr::null_mut(), &mut len) } == 0 {
        return Err(last("CryptExportKey"));
    }
    let mut buf = vec![0u8; len as usize];
    // SAFETY: буфер на `len` байтов.
    if unsafe { CryptExportKey(key, with, kind, 0, buf.as_mut_ptr(), &mut len) } == 0 {
        return Err(last("CryptExportKey"));
    }
    buf.truncate(len as usize);
    Ok(buf)
}

/// Письмо получателям ГОСТ — шифрует СКЗИ (CryptoAPI): ключ сеанса ГОСТ 28147-89, на каждого получателя — эфемерный
/// ключ VKO и экспорт ключа сеанса (`SIMPLEBLOB`), содержимое — `CryptEncrypt`. [проверить с КриптоПро CSP]
pub(crate) fn seal_gost(raw: &[u8], to: &[To]) -> Result<Vec<u8>, SealError> {
    let wide512 = to.iter().any(|t| matches!(t.prepared.key, RecipientKey::Gost { key: GostKey::G2012_512, .. }));
    let mut p = 0usize;
    let prov_type = if wide512 { PROV_GOST_2012_512 } else { PROV_GOST_2012_256 };
    // SAFETY: контекст без контейнера (только эфемерные ключи).
    if unsafe { CryptAcquireContextW(&mut p, ptr::null(), ptr::null(), prov_type, CRYPT_VERIFYCONTEXT) } == 0 {
        return Err(last("CryptAcquireContext (GOST)"));
    }
    let prov = Prov(p);
    let mut s = 0usize;
    // SAFETY: ключ сеанса ГОСТ 28147-89, экспортируемый (только в обёртке).
    if unsafe { CryptGenKey(prov.0, super::skzi::CALG_G28147, CRYPT_EXPORTABLE, &mut s) } == 0 {
        return Err(last("CryptGenKey (session)"));
    }
    let session = CKey(s);
    let mut recipients = Vec::new();
    for t in to {
        let RecipientKey::Gost { key, point, public_param_set, digest_param_set } = &t.prepared.key else {
            return Err(SealError::Failed("not a GOST recipient".into()));
        };
        let eph_alg = if *key == GostKey::G2012_512 { CALG_DH_GR3410_12_512_EPHEM } else { CALG_DH_GR3410_12_256_EPHEM };
        let mut e = 0usize;
        // SAFETY: эфемерный ключ согласования.
        if unsafe { CryptGenKey(prov.0, eph_alg, CRYPT_EXPORTABLE, &mut e) } == 0 {
            return Err(last("CryptGenKey (ephemeral)"));
        }
        let eph = CKey(e);
        let peer = GostTransport {
            key: *key,
            param_set: String::new(),
            ephemeral: point.clone(),
            public_param_set: public_param_set.clone(),
            digest_param_set: digest_param_set.clone(),
            ukm: Vec::new(),
            encrypted_key: Vec::new(),
            mac: Vec::new(),
        };
        let blob = super::skzi::public_key_blob(&peer).ok_or_else(|| SealError::Failed("GOST public key".into()))?;
        let mut a = 0usize;
        // SAFETY: открытый ключ получателя на своём эфемерном — ключ согласования.
        if unsafe { CryptImportKey(prov.0, blob.as_ptr(), blob.len() as u32, eph.0, 0, &mut a) } == 0 {
            return Err(last("CryptImportKey (VKO)"));
        }
        let agree = CKey(a);
        let export_alg = super::skzi::export_alg(*key);
        // SAFETY: `ALG_ID` по указателю.
        if unsafe { CryptSetKeyParam(agree.0, KP_ALGID, ptr::from_ref(&export_alg).cast(), 0) } == 0 {
            return Err(last("CryptSetKeyParam(KP_ALGID)"));
        }
        let simple = export(session.0, agree.0, SIMPLEBLOB)?;
        let own_pub = export(eph.0, 0, PUBLICKEYBLOB)?;
        let n = point.len();
        // SIMPLEBLOB: заголовок 8, магия 4, алгоритм 4, синхропосылка 8, ключ 32, имитовставка 4, DER набора.
        if simple.len() < 60 || own_pub.len() < n {
            return Err(SealError::Failed("GOST blobs".into()));
        }
        let param_set = postman_smime::envelope::oid_text_of(&simple[60..]).unwrap_or_else(|| "1.2.643.2.2.31.1".into());
        recipients.push((
            t.prepared.clone(),
            GostTransport {
                key: *key,
                param_set,
                ephemeral: own_pub[own_pub.len() - n..].to_vec(),
                public_param_set: public_param_set.clone(),
                digest_param_set: digest_param_set.clone(),
                ukm: simple[16..24].to_vec(),
                encrypted_key: simple[24..56].to_vec(),
                mac: simple[56..60].to_vec(),
            },
        ));
    }
    let mut iv = [0u8; 8];
    WindowsOps.random(&mut iv);
    // SAFETY: IV — 8 байтов.
    if unsafe { CryptSetKeyParam(session.0, KP_IV, iv.as_ptr(), 0) } == 0 {
        return Err(last("CryptSetKeyParam(KP_IV)"));
    }
    let entity = postman_smime::seal::entity(raw).ok_or_else(|| SealError::Failed("no body".into()))?;
    let mut data = entity.clone();
    let mut len = data.len() as u32;
    // SAFETY: CFB не меняет длину; буфер своего размера.
    if unsafe { CryptEncrypt(session.0, 0, 1, 0, data.as_mut_ptr(), &mut len, data.len() as u32) } == 0 {
        return Err(last("CryptEncrypt (GOST)"));
    }
    data.truncate(len as usize);
    let param_set = recipients.first().map(|(_, t)| t.param_set.clone()).unwrap_or_default();
    postman_smime::seal::gost_message(raw, &GostSealed { recipients, iv: iv.to_vec(), param_set, ciphertext: data })
}

#[cfg(test)]
mod tests {
    use super::*;
    use postman_smime::envelope::{self, Wrap};
    use postman_smime::prepare;
    use postman_smime::testkit::{Key, NOW, Spec, issue};

    /// ECDH через CNG (эфемерный ключ BCrypt): получатель снимает обёртку своим ключом — тот же текст.
    #[test]
    fn cng_ecdh_round_trip() {
        let key = Key::p256("seal-cng");
        let root_key = Key::rsa('a');
        let root = issue(&Spec::ca("Root"), &root_key, None);
        let user = issue(&Spec::user("Смирнова", "me@itt.test"), &key, Some((&root, &root_key)));
        let trust = postman_smime::Trust { roots: vec![postman_smime::Anchor { der: root.der.clone(), corporate: false }], ..Default::default() };
        let p = prepare::prepare(&user.der, &[], &trust, NOW).unwrap();
        let raw = b"From: a@itt.test\r\nTo: me@itt.test\r\nSubject: s\r\nContent-Type: text/plain\r\n\r\nCNG ECDH\r\n";
        let m = postman_smime::seal::seal_message(raw, &[To { prepared: p, gcm: false }], &WindowsOps).unwrap();
        let env = envelope::envelope(&m).unwrap();
        let r = &env.recipients[0];
        let Wrap::Ecdh { ephemeral, .. } = &r.wrap else { panic!("{:?}", r.wrap) };
        let z = postman_smime::testkit::ecdh_with(&key, ephemeral).unwrap();
        let cek = envelope::ecdh_cek(&z, &r.wrap, &r.encrypted_key).unwrap();
        let o = envelope::open(&m, &cek, &trust, NOW).unwrap();
        assert_eq!(o.text.as_deref().map(str::trim), Some("CNG ECDH"));
    }
}
