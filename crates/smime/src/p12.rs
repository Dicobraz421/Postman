//! Свой сертификат и ключ из файла PKCS#12 (`.p12`/`.pfx`, RFC 7292) — Linux (LIN-05, решение владельца 2026-11-12:
//! ключи сотрудников — в файлах; токенов и хранилищ нет). Файл — свой, выбранный пользователем, не из письма, но
//! разбор всё равно без паник и с пределами.
//!
//! Поддержано: MAC HMAC-SHA-1/SHA-2 (ключ по PKCS#12 KDF), шифрование PBES2 (PBKDF2 с HMAC-SHA-1/SHA-2, AES-CBC и
//! 3DES-CBC — так экспортируют OpenSSL 3 и Windows «AES256-SHA256»), старое `pbeWithSHAAnd3-KeyTripleDES-CBC` и
//! `pbeWithSHAAnd40BitRC2-CBC`/`128BitRC2` (OpenSSL 1.x, Windows «TripleDES-SHA1»); RC2 — свой, только расшифровка
//! (`rc2`). Ключи — RSA (PKCS#8); EC и ГОСТ — отказ [`P12Error::Unsupported`]. Закрытые операции — `rsa` 0.10
//! (постоянное время на `crypto-bigint`, RUSTSEC-2023-0071 закрыт в 0.10).

use der::{Decode, Encode};
use rsa::PrivateKey as RsaPrivateKey;
use rsa::pkcs8::{DecodePrivateKey, DecodePublicKey};
use x509_cert::Certificate;
use zeroize::Zeroizing;

use crate::Hash;
use crate::ber::{self, Tlv};

/// Больше — не файл ключа (защита от случайно выбранного большого файла).
pub const MAX_FILE: usize = 1 << 20;
/// Больше итераций KDF не считаем: файл с таким числом — повреждён или враждебен (зависание).
const MAX_ITERATIONS: u64 = 10_000_000;

/// Почему файл не открыт.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum P12Error {
    /// Неверный пароль (не сошёлся MAC или заполнение).
    #[error("wrong password")]
    BadPassword,
    /// Не PKCS#12 или повреждён.
    #[error("not a PKCS#12 file: {0}")]
    Malformed(&'static str),
    /// Алгоритм или вид ключа не поддержан (имя или OID).
    #[error("not supported: {0}")]
    Unsupported(String),
    /// В файле нет закрытого ключа к сертификату.
    #[error("no private key for a certificate")]
    NoKey,
}

/// Свой сертификат с закрытым ключом RSA и остальные сертификаты файла (промежуточные, корень).
pub struct Identity {
    /// Сертификат ключа (DER).
    pub cert: Vec<u8>,
    /// Остальные сертификаты файла (DER), без сертификата ключа.
    pub chain: Vec<Vec<u8>>,
    pub(crate) key: RsaPrivateKey,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity").field("cert", &self.cert.len()).field("chain", &self.chain.len()).finish_non_exhaustive()
    }
}

impl Identity {
    /// Ключ для снятия ключа содержимого письма.
    pub fn soft_key(&self) -> Option<crate::soft::SoftKey> {
        crate::soft::SoftKey::from_key(&self.cert, self.key.clone())
    }

    /// Подписант схемой `scheme` (RSA PKCS#1 v1.5).
    pub fn signer(&self, scheme: crate::Scheme) -> crate::soft::SoftSigner {
        crate::soft::SoftSigner { cert: self.cert.clone(), chain: self.chain.clone(), scheme, key: self.key.clone() }
    }
}

mod oids {
    pub const DATA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x07, 0x01];
    pub const ENCRYPTED_DATA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x07, 0x06];
    // Мешки, 1.2.840.113549.1.12.10.1.x
    pub const KEY_BAG: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x0C, 0x0A, 0x01, 0x01];
    pub const SHROUDED_KEY_BAG: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x0C, 0x0A, 0x01, 0x02];
    pub const CERT_BAG: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x0C, 0x0A, 0x01, 0x03];
    pub const SAFE_CONTENTS_BAG: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x0C, 0x0A, 0x01, 0x06];
    /// x509Certificate, 1.2.840.113549.1.9.22.1.
    pub const X509: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x09, 0x16, 0x01];
    // PBE PKCS#12, 1.2.840.113549.1.12.1.x
    pub const PBE_SHA1_RC2_128: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x0C, 0x01, 0x05];
    pub const PBE_SHA1_RC2_40: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x0C, 0x01, 0x06];
    pub const PBE_SHA1_3DES: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x0C, 0x01, 0x03];
    pub const PBES2: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x05, 0x0D];
    pub const PBKDF2: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x05, 0x0C];
    // HMAC, 1.2.840.113549.2.x
    pub const HMAC_SHA1: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x02, 0x07];
    pub const HMAC_SHA256: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x02, 0x09];
    pub const HMAC_SHA384: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x02, 0x0A];
    pub const HMAC_SHA512: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x02, 0x0B];
    pub const DES_EDE3_CBC: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x03, 0x07];
    pub const AES128_CBC: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x01, 0x02];
    pub const AES192_CBC: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x01, 0x16];
    pub const AES256_CBC: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x01, 0x2A];
    // Хэши MAC
    pub const SHA1: &[u8] = &[0x2B, 0x0E, 0x03, 0x02, 0x1A];
    pub const SHA256: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01];
    pub const SHA384: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x02];
    pub const SHA512: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x03];
}

const SEQUENCE: u8 = 0x30;
const OCTETS: u8 = 0x04;
const OID: u8 = 0x06;
const INTEGER: u8 = 0x02;
const EXPLICIT0: u8 = 0xA0;
const IMPLICIT0_PRIM: u8 = 0x80;

const BAD: P12Error = P12Error::Malformed("structure");

fn one(input: &[u8]) -> Result<Tlv<'_>, P12Error> {
    ber::read(input).map(|(t, _)| t).ok_or(BAD)
}

fn kids<'a>(t: &Tlv<'a>, tag: u8) -> Result<Vec<Tlv<'a>>, P12Error> {
    if t.tag != tag {
        return Err(BAD);
    }
    t.children().ok_or(BAD)
}

fn oid_of<'a>(t: Option<&Tlv<'a>>) -> Result<&'a [u8], P12Error> {
    t.filter(|t| t.tag == OID).map(|t| t.value).ok_or(BAD)
}

fn uint(t: Option<&Tlv<'_>>) -> Result<u64, P12Error> {
    let t = t.filter(|t| t.tag == INTEGER).ok_or(BAD)?;
    if t.value.len() > 8 || t.value.first().is_some_and(|b| b & 0x80 != 0) {
        return Err(BAD);
    }
    Ok(t.value.iter().fold(0u64, |a, &b| (a << 8) | u64::from(b)))
}

fn iterations(t: Option<&Tlv<'_>>) -> Result<u64, P12Error> {
    let n = uint(t)?;
    if n == 0 || n > MAX_ITERATIONS {
        return Err(P12Error::Unsupported(format!("{n} iterations")));
    }
    Ok(n)
}

fn octets(t: Option<&Tlv<'_>>) -> Result<Vec<u8>, P12Error> {
    t.filter(|t| t.tag & !0x20 == OCTETS).and_then(Tlv::octets).ok_or(BAD)
}

fn unsupported(oid: &[u8]) -> P12Error {
    match der::asn1::ObjectIdentifier::from_bytes(oid) {
        Ok(o) => P12Error::Unsupported(o.to_string()),
        Err(_) => BAD,
    }
}

/// Блок хэша для HMAC и PKCS#12 KDF.
fn block(h: Hash) -> usize {
    match h {
        Hash::Sha384 | Hash::Sha512 => 128,
        _ => 64,
    }
}

fn hmac(h: Hash, key: &[u8], data: &[u8]) -> Zeroizing<Vec<u8>> {
    let b = block(h);
    let mut k = Zeroizing::new(if key.len() > b { h.digest(key) } else { key.to_vec() });
    k.resize(b, 0);
    let mut inner = Zeroizing::new(k.iter().map(|x| x ^ 0x36).collect::<Vec<u8>>());
    inner.extend_from_slice(data);
    let mut outer = Zeroizing::new(k.iter().map(|x| x ^ 0x5C).collect::<Vec<u8>>());
    outer.extend_from_slice(&h.digest(&inner));
    Zeroizing::new(h.digest(&outer))
}

/// PBKDF2 (RFC 8018 §5.2).
fn pbkdf2(h: Hash, password: &[u8], salt: &[u8], rounds: u64, len: usize) -> Zeroizing<Vec<u8>> {
    let mut out = Zeroizing::new(Vec::with_capacity(len));
    let mut i = 1u32;
    while out.len() < len {
        let mut s = salt.to_vec();
        s.extend_from_slice(&i.to_be_bytes());
        let mut u = hmac(h, password, &s);
        let mut t = u.clone();
        for _ in 1..rounds {
            u = hmac(h, password, &u);
            t.iter_mut().zip(u.iter()).for_each(|(a, b)| *a ^= b);
        }
        out.extend_from_slice(&t);
        i += 1;
    }
    out.truncate(len);
    out
}

/// Пароль для PKCS#12 KDF: BMPString (UTF-16BE) с нулём на конце (RFC 7292 Прил. B.1).
fn bmp(password: &str) -> Zeroizing<Vec<u8>> {
    let mut out = Zeroizing::new(Vec::with_capacity(password.len() * 2 + 2));
    for u in password.encode_utf16().chain(std::iter::once(0)) {
        out.extend_from_slice(&u.to_be_bytes());
    }
    out
}

/// PKCS#12 KDF (RFC 7292 Прил. B.2): `id` 1 — ключ, 2 — IV, 3 — ключ MAC.
fn p12_kdf(h: Hash, id: u8, password: &[u8], salt: &[u8], rounds: u64, len: usize) -> Zeroizing<Vec<u8>> {
    let v = block(h);
    let fill = |src: &[u8]| -> Vec<u8> {
        if src.is_empty() {
            return Vec::new();
        }
        let n = v * src.len().div_ceil(v);
        src.iter().copied().cycle().take(n).collect()
    };
    let mut i = Zeroizing::new(fill(salt));
    i.extend_from_slice(&fill(password));
    let d = vec![id; v];
    let mut out = Zeroizing::new(Vec::with_capacity(len));
    while out.len() < len {
        let mut input = Zeroizing::new(d.clone());
        input.extend_from_slice(&i);
        let mut a = Zeroizing::new(h.digest(&input));
        for _ in 1..rounds {
            a = Zeroizing::new(h.digest(&a));
        }
        out.extend_from_slice(&a);
        if out.len() >= len {
            break;
        }
        let b: Vec<u8> = a.iter().copied().cycle().take(v).collect();
        for chunk in i.chunks_mut(v) {
            // chunk = chunk + b + 1 (по модулю 2^(8v)), байты — big-endian.
            let mut carry = 1u16;
            for (x, y) in chunk.iter_mut().rev().zip(b.iter().rev()) {
                let s = u16::from(*x) + u16::from(*y) + carry;
                *x = s as u8;
                carry = s >> 8;
            }
        }
    }
    out.truncate(len);
    out
}

/// Шифр CBC с ключом и IV.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cbc {
    Aes,
    Des3,
    /// RC2 с действующей длиной ключа в битах.
    Rc2(usize),
}

fn cbc_decrypt(cipher: Cbc, key: &[u8], iv: &[u8], data: &[u8]) -> Result<Zeroizing<Vec<u8>>, P12Error> {
    use aes::cipher::block_padding::Pkcs7;
    use aes::cipher::{BlockModeDecrypt, KeyIvInit};
    let bad = |_| P12Error::BadPassword;
    let out = match (cipher, key.len()) {
        (Cbc::Aes, 16) => cbc::Decryptor::<aes::Aes128>::new_from_slices(key, iv).map_err(|_| BAD)?.decrypt_padded_vec::<Pkcs7>(data).map_err(bad)?,
        (Cbc::Aes, 24) => cbc::Decryptor::<aes::Aes192>::new_from_slices(key, iv).map_err(|_| BAD)?.decrypt_padded_vec::<Pkcs7>(data).map_err(bad)?,
        (Cbc::Aes, 32) => cbc::Decryptor::<aes::Aes256>::new_from_slices(key, iv).map_err(|_| BAD)?.decrypt_padded_vec::<Pkcs7>(data).map_err(bad)?,
        (Cbc::Des3, 24) => {
            cbc::Decryptor::<des::TdesEde3>::new_from_slices(key, iv).map_err(|_| BAD)?.decrypt_padded_vec::<Pkcs7>(data).map_err(bad)?
        }
        (Cbc::Rc2(bits), _) => return rc2::decrypt_cbc(key, bits, iv, data).ok_or(P12Error::BadPassword),
        _ => return Err(BAD),
    };
    Ok(Zeroizing::new(out))
}

/// Расшифровать `data` алгоритмом `alg` (AlgorithmIdentifier) паролем.
fn decrypt(alg: &Tlv<'_>, password: &str, data: &[u8]) -> Result<Zeroizing<Vec<u8>>, P12Error> {
    let a = kids(alg, SEQUENCE)?;
    let oid = oid_of(a.first())?;
    let legacy = match oid {
        oids::PBE_SHA1_3DES => Some((Cbc::Des3, 24)),
        oids::PBE_SHA1_RC2_40 => Some((Cbc::Rc2(40), 5)),
        oids::PBE_SHA1_RC2_128 => Some((Cbc::Rc2(128), 16)),
        _ => None,
    };
    if let Some((cipher, key_len)) = legacy {
        let p = kids(a.get(1).ok_or(BAD)?, SEQUENCE)?;
        let (salt, rounds) = (octets(p.first())?, iterations(p.get(1))?);
        let pw = bmp(password);
        let key = p12_kdf(Hash::Sha1, 1, &pw, &salt, rounds, key_len);
        let iv = p12_kdf(Hash::Sha1, 2, &pw, &salt, rounds, 8);
        return cbc_decrypt(cipher, &key, &iv, data);
    }
    if oid != oids::PBES2 {
        return Err(unsupported(oid));
    }
    let p = kids(a.get(1).ok_or(BAD)?, SEQUENCE)?;
    let kdf = kids(p.first().ok_or(BAD)?, SEQUENCE)?;
    let enc = kids(p.get(1).ok_or(BAD)?, SEQUENCE)?;
    let kdf_oid = oid_of(kdf.first())?;
    if kdf_oid != oids::PBKDF2 {
        return Err(unsupported(kdf_oid));
    }
    let k = kids(kdf.get(1).ok_or(BAD)?, SEQUENCE)?;
    let salt = k.first().filter(|t| t.tag == OCTETS).map(|t| t.value.to_vec()).ok_or(P12Error::Unsupported("PBKDF2 salt source".into()))?;
    let rounds = iterations(k.get(1))?;
    let mut rest = &k[2..];
    let mut key_len = None;
    if rest.first().is_some_and(|t| t.tag == INTEGER) {
        key_len = Some(uint(rest.first())?);
        rest = &rest[1..];
    }
    let prf = match rest.first() {
        None => Hash::Sha1,
        Some(t) => match oid_of(kids(t, SEQUENCE)?.first())? {
            oids::HMAC_SHA1 => Hash::Sha1,
            oids::HMAC_SHA256 => Hash::Sha256,
            oids::HMAC_SHA384 => Hash::Sha384,
            oids::HMAC_SHA512 => Hash::Sha512,
            other => return Err(unsupported(other)),
        },
    };
    let (cipher, len) = match oid_of(enc.first())? {
        oids::AES128_CBC => (Cbc::Aes, 16),
        oids::AES192_CBC => (Cbc::Aes, 24),
        oids::AES256_CBC => (Cbc::Aes, 32),
        oids::DES_EDE3_CBC => (Cbc::Des3, 24),
        other => return Err(unsupported(other)),
    };
    if key_len.is_some_and(|l| l != len as u64) {
        return Err(BAD);
    }
    let iv = enc.get(1).filter(|t| t.tag == OCTETS).map(|t| t.value).ok_or(BAD)?;
    let key = pbkdf2(prf, password.as_bytes(), &salt, rounds, len);
    cbc_decrypt(cipher, &key, iv, data)
}

/// Проверить MAC файла (`MacData`) над содержимым `authSafe`.
fn check_mac(mac: &Tlv<'_>, password: &str, content: &[u8]) -> Result<(), P12Error> {
    let m = kids(mac, SEQUENCE)?;
    let digest_info = kids(m.first().ok_or(BAD)?, SEQUENCE)?;
    let alg = kids(digest_info.first().ok_or(BAD)?, SEQUENCE)?;
    let h = match oid_of(alg.first())? {
        oids::SHA1 => Hash::Sha1,
        oids::SHA256 => Hash::Sha256,
        oids::SHA384 => Hash::Sha384,
        oids::SHA512 => Hash::Sha512,
        other => return Err(unsupported(other)),
    };
    let expected = digest_info.get(1).filter(|t| t.tag == OCTETS).map(|t| t.value).ok_or(BAD)?;
    let salt = m.get(1).filter(|t| t.tag == OCTETS).map(|t| t.value).ok_or(BAD)?;
    let rounds = if m.get(2).is_some() { iterations(m.get(2))? } else { 1 };
    let key = p12_kdf(h, 3, &bmp(password), salt, rounds, h.output_len());
    let got = hmac(h, &key, content);
    // Сравнение без раннего выхода.
    let same = got.len() == expected.len() && got.iter().zip(expected).fold(0u8, |a, (x, y)| a | (x ^ y)) == 0;
    if same { Ok(()) } else { Err(P12Error::BadPassword) }
}

#[derive(Default)]
struct Found {
    certs: Vec<Vec<u8>>,
    keys: Vec<RsaPrivateKey>,
    unsupported_key: Option<P12Error>,
}

fn private_key(pkcs8: &[u8], found: &mut Found) {
    match RsaPrivateKey::from_pkcs8_der(pkcs8) {
        Ok(k) => found.keys.push(k),
        Err(_) => {
            // Не RSA (или повреждён): назвать алгоритм ключа, если он читается.
            let alg = one(pkcs8)
                .ok()
                .and_then(|t| t.children())
                .and_then(|c| c.get(1).and_then(|a| a.children()).and_then(|a| a.first().map(|o| o.value.to_vec())));
            found.unsupported_key.get_or_insert(alg.map_or(P12Error::Malformed("private key"), |o| unsupported(&o)));
        }
    }
}

/// Разобрать `SafeContents` (глубина — у вложенных `safeContentsBag`).
fn safe_contents(der: &[u8], password: &str, found: &mut Found, depth: u8) -> Result<(), P12Error> {
    if depth > 4 {
        return Err(BAD);
    }
    for bag in kids(&one(der)?, SEQUENCE)? {
        let b = kids(&bag, SEQUENCE)?;
        let id = oid_of(b.first())?;
        let value = b.get(1).filter(|t| t.tag == EXPLICIT0).and_then(|t| t.children()).and_then(|c| c.into_iter().next()).ok_or(BAD)?;
        match id {
            oids::KEY_BAG => private_key(value.raw, found),
            oids::SHROUDED_KEY_BAG => {
                let e = kids(&value, SEQUENCE)?;
                let data = octets(e.get(1))?;
                let plain = decrypt(e.first().ok_or(BAD)?, password, &data)?;
                private_key(&plain, found);
            }
            oids::CERT_BAG => {
                let c = kids(&value, SEQUENCE)?;
                if oid_of(c.first())? == oids::X509 {
                    let v = c.get(1).filter(|t| t.tag == EXPLICIT0).and_then(|t| t.children()).ok_or(BAD)?;
                    found.certs.push(octets(v.first())?);
                }
            }
            oids::SAFE_CONTENTS_BAG => safe_contents(value.raw, password, found, depth + 1)?,
            // CRL, секреты и прочее — не нужны.
            _ => {}
        }
    }
    Ok(())
}

/// Открыть файл PKCS#12 паролем: свои сертификаты с ключами (у каждого ключа RSA — его сертификат по открытому
/// ключу), лучший — по позднему сроку, первым. Ключа нет — [`P12Error::NoKey`].
pub fn parse(pfx: &[u8], password: &str) -> Result<Vec<Identity>, P12Error> {
    if pfx.len() > MAX_FILE {
        return Err(P12Error::Malformed("too large"));
    }
    let top = kids(&one(pfx).map_err(|_| P12Error::Malformed("not DER"))?, SEQUENCE)?;
    if uint(top.first())? != 3 {
        return Err(P12Error::Malformed("version"));
    }
    let auth = kids(top.get(1).ok_or(BAD)?, SEQUENCE)?;
    if oid_of(auth.first())? != oids::DATA {
        return Err(P12Error::Unsupported("public-key integrity mode".into()));
    }
    let content = auth.get(1).filter(|t| t.tag == EXPLICIT0).and_then(|t| t.children()).ok_or(BAD)?;
    let content = octets(content.first())?;
    if let Some(mac) = top.get(2) {
        check_mac(mac, password, &content)?;
    }
    let mut found = Found::default();
    for ci in kids(&one(&content)?, SEQUENCE)? {
        let c = kids(&ci, SEQUENCE)?;
        let inner = c.get(1).filter(|t| t.tag == EXPLICIT0).and_then(|t| t.children()).and_then(|v| v.into_iter().next()).ok_or(BAD)?;
        match oid_of(c.first())? {
            oids::DATA => safe_contents(&octets(Some(&inner))?, password, &mut found, 0)?,
            oids::ENCRYPTED_DATA => {
                let ed = kids(&inner, SEQUENCE)?;
                let eci = kids(ed.get(1).ok_or(BAD)?, SEQUENCE)?;
                let data = eci.get(2).filter(|t| t.tag & !0x20 == IMPLICIT0_PRIM).and_then(Tlv::octets).ok_or(BAD)?;
                let plain = decrypt(eci.get(1).ok_or(BAD)?, password, &data)?;
                safe_contents(&plain, password, &mut found, 0)?;
            }
            other => return Err(unsupported(other)),
        }
    }
    let mut out: Vec<(Identity, der::DateTime)> = Vec::new();
    for key in found.keys {
        let public = key.to_public_key();
        let matching = found.certs.iter().find(|der| {
            Certificate::from_der(der)
                .ok()
                .and_then(|c| c.tbs_certificate().subject_public_key_info().to_der().ok())
                .and_then(|spki| rsa::PublicKey::from_public_key_der(&spki).ok())
                .is_some_and(|p| p == public)
        });
        let Some(cert) = matching.cloned() else { continue };
        let Ok(c) = Certificate::from_der(&cert) else { continue };
        let until = c.tbs_certificate().validity().not_after.to_date_time();
        let chain = found.certs.iter().filter(|d| **d != cert).cloned().collect();
        out.push((Identity { cert, chain, key }, until));
    }
    if out.is_empty() {
        return Err(found.unsupported_key.unwrap_or(P12Error::NoKey));
    }
    out.sort_by_key(|(_, until)| std::cmp::Reverse(*until));
    Ok(out.into_iter().map(|(i, _)| i).collect())
}

/// RC2 (RFC 2268) — только расшифровка старых файлов PKCS#12 (сертификаты в OpenSSL 1.x шифровались RC2-40).
mod rc2 {
    /// PITABLE, RFC 2268 §2.
    const PI: [u8; 256] = [
        0xd9, 0x78, 0xf9, 0xc4, 0x19, 0xdd, 0xb5, 0xed, 0x28, 0xe9, 0xfd, 0x79, 0x4a, 0xa0, 0xd8, 0x9d, 0xc6, 0x7e, 0x37, 0x83, 0x2b, 0x76, 0x53,
        0x8e, 0x62, 0x4c, 0x64, 0x88, 0x44, 0x8b, 0xfb, 0xa2, 0x17, 0x9a, 0x59, 0xf5, 0x87, 0xb3, 0x4f, 0x13, 0x61, 0x45, 0x6d, 0x8d, 0x09, 0x81,
        0x7d, 0x32, 0xbd, 0x8f, 0x40, 0xeb, 0x86, 0xb7, 0x7b, 0x0b, 0xf0, 0x95, 0x21, 0x22, 0x5c, 0x6b, 0x4e, 0x82, 0x54, 0xd6, 0x65, 0x93, 0xce,
        0x60, 0xb2, 0x1c, 0x73, 0x56, 0xc0, 0x14, 0xa7, 0x8c, 0xf1, 0xdc, 0x12, 0x75, 0xca, 0x1f, 0x3b, 0xbe, 0xe4, 0xd1, 0x42, 0x3d, 0xd4, 0x30,
        0xa3, 0x3c, 0xb6, 0x26, 0x6f, 0xbf, 0x0e, 0xda, 0x46, 0x69, 0x07, 0x57, 0x27, 0xf2, 0x1d, 0x9b, 0xbc, 0x94, 0x43, 0x03, 0xf8, 0x11, 0xc7,
        0xf6, 0x90, 0xef, 0x3e, 0xe7, 0x06, 0xc3, 0xd5, 0x2f, 0xc8, 0x66, 0x1e, 0xd7, 0x08, 0xe8, 0xea, 0xde, 0x80, 0x52, 0xee, 0xf7, 0x84, 0xaa,
        0x72, 0xac, 0x35, 0x4d, 0x6a, 0x2a, 0x96, 0x1a, 0xd2, 0x71, 0x5a, 0x15, 0x49, 0x74, 0x4b, 0x9f, 0xd0, 0x5e, 0x04, 0x18, 0xa4, 0xec, 0xc2,
        0xe0, 0x41, 0x6e, 0x0f, 0x51, 0xcb, 0xcc, 0x24, 0x91, 0xaf, 0x50, 0xa1, 0xf4, 0x70, 0x39, 0x99, 0x7c, 0x3a, 0x85, 0x23, 0xb8, 0xb4, 0x7a,
        0xfc, 0x02, 0x36, 0x5b, 0x25, 0x55, 0x97, 0x31, 0x2d, 0x5d, 0xfa, 0x98, 0xe3, 0x8a, 0x92, 0xae, 0x05, 0xdf, 0x29, 0x10, 0x67, 0x6c, 0xba,
        0xc9, 0xd3, 0x00, 0xe6, 0xcf, 0xe1, 0x9e, 0xa8, 0x2c, 0x63, 0x16, 0x01, 0x3f, 0x58, 0xe2, 0x89, 0xa9, 0x0d, 0x38, 0x34, 0x1b, 0xab, 0x33,
        0xff, 0xb0, 0xbb, 0x48, 0x0c, 0x5f, 0xb9, 0xb1, 0xcd, 0x2e, 0xc5, 0xf3, 0xdb, 0x47, 0xe5, 0xa5, 0x9c, 0x77, 0x0a, 0xa6, 0x20, 0x68, 0xfe,
        0x7f, 0xc1, 0xad,
    ];

    /// Расширение ключа (§2): 64 слова.
    fn expand(key: &[u8], bits: usize) -> Option<zeroize::Zeroizing<[u16; 64]>> {
        let t = key.len();
        if t == 0 || t > 128 || bits == 0 || bits > 1024 {
            return None;
        }
        let mut l = zeroize::Zeroizing::new([0u8; 128]);
        l[..t].copy_from_slice(key);
        for i in t..128 {
            l[i] = PI[usize::from(l[i - 1].wrapping_add(l[i - t]))];
        }
        let t8 = bits.div_ceil(8);
        let tm = 0xFFu8 >> (8 * t8 - bits);
        l[128 - t8] = PI[usize::from(l[128 - t8] & tm)];
        for i in (0..128 - t8).rev() {
            l[i] = PI[usize::from(l[i + 1] ^ l[i + t8])];
        }
        let mut k = zeroize::Zeroizing::new([0u16; 64]);
        for (i, w) in k.iter_mut().enumerate() {
            *w = u16::from_le_bytes([l[2 * i], l[2 * i + 1]]);
        }
        Some(k)
    }

    fn decrypt_block(k: &[u16; 64], block: &mut [u8]) {
        let mut r = [0u16; 4];
        for (i, w) in r.iter_mut().enumerate() {
            *w = u16::from_le_bytes([block[2 * i], block[2 * i + 1]]);
        }
        const S: [u32; 4] = [1, 2, 3, 5];
        let mut j = 63usize;
        let mix = |r: &mut [u16; 4], j: &mut usize| {
            for i in (0..4).rev() {
                r[i] = r[i].rotate_right(S[i]);
                r[i] = r[i].wrapping_sub(k[*j]).wrapping_sub(r[(i + 3) % 4] & r[(i + 2) % 4]).wrapping_sub(!r[(i + 3) % 4] & r[(i + 1) % 4]);
                *j = j.wrapping_sub(1);
            }
        };
        let mash = |r: &mut [u16; 4]| {
            for i in (0..4).rev() {
                r[i] = r[i].wrapping_sub(k[usize::from(r[(i + 3) % 4] & 63)]);
            }
        };
        for round in 0..16 {
            mix(&mut r, &mut j);
            if round == 4 || round == 10 {
                mash(&mut r);
            }
        }
        for (i, w) in r.iter().enumerate() {
            block[2 * i..2 * i + 2].copy_from_slice(&w.to_le_bytes());
        }
    }

    /// RC2-CBC с заполнением PKCS#7; неверное заполнение — `None`.
    pub(super) fn decrypt_cbc(key: &[u8], bits: usize, iv: &[u8], data: &[u8]) -> Option<zeroize::Zeroizing<Vec<u8>>> {
        let k = expand(key, bits)?;
        if iv.len() != 8 || data.is_empty() || !data.len().is_multiple_of(8) {
            return None;
        }
        let mut out = zeroize::Zeroizing::new(data.to_vec());
        let mut prev: [u8; 8] = iv.try_into().ok()?;
        for chunk in out.chunks_mut(8) {
            let cur: [u8; 8] = (&*chunk).try_into().ok()?;
            decrypt_block(&k, chunk);
            chunk.iter_mut().zip(prev).for_each(|(a, b)| *a ^= b);
            prev = cur;
        }
        let pad = usize::from(*out.last()?);
        if pad == 0 || pad > 8 || !out[out.len() - pad..].iter().all(|&b| usize::from(b) == pad) {
            return None;
        }
        let n = out.len() - pad;
        out.truncate(n);
        Some(out)
    }

    #[cfg(test)]
    pub(super) fn ecb_block(key: &[u8], bits: usize, block: &mut [u8; 8]) -> bool {
        let Some(k) = expand(key, bits) else { return false };
        decrypt_block(&k, block);
        true
    }
}

#[cfg(test)]
#[path = "p12/tests.rs"]
mod tests;
