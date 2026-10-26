//! Шифрование исходящих (S5-12 Ф4, `docs/SMIME.md` §6.4, решения владельца №8 и «ГОСТ-ключи»): EnvelopedData
//! AES-256-CBC (ключ получателя — RSA PKCS#1 v1.5 или ECDH P-256/384 + AES-256 Key Wrap) или AuthEnvelopedData
//! AES-256-GCM + RSA-OAEP-SHA256 — если **все** получатели объявили GCM в `SMIMECapabilities`. Получатели — только
//! «подготовленные» ([`crate::prepare`]: ключ значениями, разобран и проверен воркером); себя добавляет вызывающий.
//!
//! Ключ содержимого и случайные числа — от [`Ops`] (ядро: системный генератор Windows, ECDH — CNG); RSA — открытая
//! операция `rsa` здесь же. ГОСТ сюда не попадает: письмо получателям ГОСТ шифрует СКЗИ ([`gost_message`] только
//! собирает CMS из того, что вернуло СКЗИ).

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::envelope::{Cipher, GostTransport};
use crate::prepare::{Prepared, RecipientKey};
use crate::{Hash, oid};

/// Почему письмо не зашифровано.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
pub enum SealError {
    /// Получателей нет.
    #[error("no recipients")]
    NoRecipients,
    /// Получатель ГОСТ — шифрует только СКЗИ (письмо делится, решение владельца 2026-10-22).
    #[error("GOST recipient needs the SKZI")]
    Gost,
    #[error("{0}")]
    Failed(String),
}

/// Случайные числа и ECDH — средствами ОС (ядро) или тестов.
pub trait Ops {
    /// Криптографически стойкие случайные байты.
    fn random(&self, out: &mut [u8]);
    /// Эфемерный ключ на кривой `bits` (256/384) и общий секрет с точкой `peer` (`04‖X‖Y`): (своя точка, Z
    /// big-endian).
    fn ecdh(&self, bits: u16, peer: &[u8]) -> Result<(Vec<u8>, Zeroizing<Vec<u8>>), SealError>;
}

/// Получатель письма: подготовленный сертификат и объявил ли он AES-GCM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct To {
    pub prepared: Prepared,
    pub gcm: bool,
}

/// Шифр письма (решение №8): AES-256-GCM, если все объявили GCM и ни у кого нет ключа EC (RFC 5753 c GCM —
/// [проверить у Outlook]); иначе AES-256-CBC.
pub fn choose_cipher(to: &[To]) -> Cipher {
    if !to.is_empty() && to.iter().all(|t| t.gcm && matches!(t.prepared.key, RecipientKey::Rsa { .. })) {
        Cipher::AesGcm(256)
    } else {
        Cipher::AesCbc(256)
    }
}

/// Объявил ли адресат AES-GCM (`SMIMECapabilities` его подписанных писем, OID строками).
pub fn declares_gcm(capabilities: &[String]) -> bool {
    let gcm = [oid::AES256_GCM.to_string(), oid::AES128_GCM.to_string()];
    capabilities.iter().any(|c| gcm.contains(c))
}

fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = crate::ber::header(tag, content.len());
    out.extend_from_slice(content);
    out
}

fn seq(parts: &[Vec<u8>]) -> Vec<u8> {
    tlv(0x30, &parts.concat())
}

fn oid_der(o: const_oid::ObjectIdentifier) -> Vec<u8> {
    use der::Encode;
    o.to_der().unwrap_or_default()
}

fn alg(o: const_oid::ObjectIdentifier, params: Option<Vec<u8>>) -> Vec<u8> {
    let mut parts = vec![oid_der(o)];
    parts.extend(params);
    seq(&parts)
}

fn integer(bytes: &[u8]) -> Vec<u8> {
    let mut v: Vec<u8> = bytes.iter().copied().skip_while(|&b| b == 0).collect();
    if v.first().is_none_or(|&b| b >= 0x80) {
        v.insert(0, 0);
    }
    tlv(0x02, &v)
}

fn rid(p: &Prepared) -> Vec<u8> {
    seq(&[p.issuer.clone(), integer(&p.serial)])
}

/// Генератор для `rsa` поверх [`Ops::random`].
struct Rng<'a>(&'a dyn Ops);

impl rand_core::TryRng for Rng<'_> {
    type Error = std::convert::Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        let mut b = [0u8; 4];
        self.0.random(&mut b);
        Ok(u32::from_le_bytes(b))
    }

    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        let mut b = [0u8; 8];
        self.0.random(&mut b);
        Ok(u64::from_le_bytes(b))
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
        self.0.random(dst);
        Ok(())
    }
}

impl rand_core::TryCryptoRng for Rng<'_> {}

fn failed(e: impl std::fmt::Display) -> SealError {
    SealError::Failed(e.to_string())
}

/// RecipientInfo для одного получателя.
fn recipient_info(t: &To, cek: &[u8], oaep: bool, ops: &dyn Ops) -> Result<Vec<u8>, SealError> {
    match &t.prepared.key {
        RecipientKey::Rsa { n, e } => {
            use rsa::pkcs1::DecodeRsaPublicKey;
            let key = rsa::RsaPublicKey::from_pkcs1_der(&seq(&[integer(n), integer(e)])).map_err(failed)?;
            let mut rng = Rng(ops);
            let (alg_id, ek) = if oaep {
                let params = seq(&[tlv(0xA0, &alg(oid::SHA256, None)), tlv(0xA1, &alg(oid::MGF1, Some(alg(oid::SHA256, None))))]);
                (alg(oid::RSA_OAEP, Some(params)), key.encrypt(&mut rng, rsa::Oaep::<sha2::Sha256>::new(), cek).map_err(failed)?)
            } else {
                (alg(oid::RSA_ENCRYPTION, Some(vec![0x05, 0x00])), key.encrypt(&mut rng, rsa::Pkcs1v15Encrypt, cek).map_err(failed)?)
            };
            Ok(seq(&[tlv(0x02, &[0]), rid(&t.prepared), alg_id, tlv(0x04, &ek)]))
        }
        RecipientKey::Ec { bits, point } => {
            let (eph, z) = ops.ecdh(*bits, point)?;
            let kek = crate::envelope::ecdh_kek(&z, Hash::Sha256, 256, None).ok_or_else(|| failed("KDF"))?;
            let ek = crate::envelope::wrap_key(&kek, cek).ok_or_else(|| failed("key wrap"))?;
            let mut bit = vec![0u8];
            bit.extend_from_slice(&eph);
            let originator = tlv(0xA0, &tlv(0xA1, &[alg(oid::EC_PUBLIC_KEY, None), tlv(0x03, &bit)].concat()));
            let kea = alg(oid::ECDH_STD_SHA256, Some(alg(oid::AES256_WRAP, None)));
            let keys = seq(&[seq(&[rid(&t.prepared), tlv(0x04, &ek)])]);
            Ok(tlv(0xA1, &[tlv(0x02, &[3]), originator, kea, keys].concat()))
        }
        RecipientKey::Gost { .. } => Err(SealError::Gost),
    }
}

/// CMS (`ContentInfo`, DER) над MIME-сущностью `content` для получателей `to`.
pub fn envelope(content: &[u8], to: &[To], ops: &dyn Ops) -> Result<(Vec<u8>, Cipher), SealError> {
    use aes::cipher::block_padding::Pkcs7;
    use aes::cipher::{BlockEncryptMut, KeyIvInit};
    if to.is_empty() {
        return Err(SealError::NoRecipients);
    }
    let cipher = choose_cipher(to);
    let mut cek = Zeroizing::new(vec![0u8; 32]);
    ops.random(&mut cek);
    let gcm = matches!(cipher, Cipher::AesGcm(_));
    let infos = to.iter().map(|t| recipient_info(t, &cek, gcm, ops)).collect::<Result<Vec<_>, _>>()?.concat();
    let any_v2 = to.iter().any(|t| matches!(t.prepared.key, RecipientKey::Ec { .. }));
    let cms = if gcm {
        use aes_gcm::aead::AeadInPlace;
        use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
        let mut nonce = [0u8; 12];
        ops.random(&mut nonce);
        let mut buf = content.to_vec();
        let tag =
            Aes256Gcm::new_from_slice(&cek).map_err(failed)?.encrypt_in_place_detached(Nonce::from_slice(&nonce), &[], &mut buf).map_err(failed)?;
        let eci = seq(&[oid_der(oid::DATA), alg(oid::AES256_GCM, Some(seq(&[tlv(0x04, &nonce), tlv(0x02, &[16])]))), tlv(0x80, &buf)]);
        let body = seq(&[tlv(0x02, &[0]), tlv(0x31, &infos), eci, tlv(0x04, &tag)]);
        seq(&[oid_der(oid::AUTH_ENVELOPED_DATA), tlv(0xA0, &body)])
    } else {
        let mut iv = [0u8; 16];
        ops.random(&mut iv);
        let ct = cbc::Encryptor::<aes::Aes256>::new_from_slices(&cek, &iv).map_err(failed)?.encrypt_padded_vec_mut::<Pkcs7>(content);
        let eci = seq(&[oid_der(oid::DATA), alg(oid::AES256_CBC, Some(tlv(0x04, &iv))), tlv(0x80, &ct)]);
        let body = seq(&[tlv(0x02, &[if any_v2 { 2 } else { 0 }]), tlv(0x31, &infos), eci]);
        seq(&[oid_der(oid::ENVELOPED_DATA), tlv(0xA0, &body)])
    };
    Ok((cms, cipher))
}

/// Письмо `application/pkcs7-mime` из своих заголовков `outer` и CMS.
fn letter(outer: &[u8], cms: &[u8], authenticated: bool) -> Vec<u8> {
    let kind = if authenticated { "authEnveloped-data" } else { "enveloped-data" };
    let mut out = outer.to_vec();
    out.extend_from_slice(b"MIME-Version: 1.0\r\n");
    out.extend(
        format!(
            "Content-Type: application/pkcs7-mime; smime-type={kind}; name=\"smime.p7m\"\r\nContent-Transfer-Encoding: base64\r\nContent-Disposition: attachment; filename=\"smime.p7m\"\r\nContent-Description: S/MIME Encrypted Message\r\n\r\n{}\r\n",
            crate::sign::base64(cms)
        )
        .into_bytes(),
    );
    out
}

/// Зашифровать своё письмо `raw` (возможно, уже подписанное — RFC 8551 §3.7): заголовки письма остаются открытыми
/// (тема — решение №9), MIME-сущность — внутрь.
pub fn seal_message(raw: &[u8], to: &[To], ops: &dyn Ops) -> Result<Vec<u8>, SealError> {
    let (outer, entity) = crate::sign::split_entity(raw).ok_or_else(|| failed("no body"))?;
    let (cms, cipher) = envelope(&entity, to, ops)?;
    Ok(letter(&outer, &cms, matches!(cipher, Cipher::AesGcm(_))))
}

/// Что вернуло СКЗИ для письма ГОСТ: на каждого получателя — `GostR3410-KeyTransport` (RFC 4490), IV, набор
/// параметров шифра и шифрованное содержимое (ГОСТ 28147-89 CFB).
#[derive(Debug, Clone)]
pub struct GostSealed {
    pub recipients: Vec<(Prepared, GostTransport)>,
    pub iv: Vec<u8>,
    pub param_set: String,
    pub ciphertext: Vec<u8>,
}

fn oid_text(t: &str) -> Result<Vec<u8>, SealError> {
    crate::envelope::oid_der(t).ok_or_else(|| failed(format!("OID {t}")))
}

/// `GostR3410-KeyTransport` (RFC 4490 §5.2) из полей СКЗИ.
fn gost_transport(t: &GostTransport) -> Result<Vec<u8>, SealError> {
    let key_oid = match t.key {
        crate::envelope::GostKey::G2001 => oid::GOST2001_KEY,
        crate::envelope::GostKey::G2012_256 => oid::GOST_KEY_256,
        crate::envelope::GostKey::G2012_512 => oid::GOST_KEY_512,
    };
    let mut params = vec![oid_text(&t.public_param_set)?];
    if let Some(d) = &t.digest_param_set {
        params.push(oid_text(d)?);
    }
    let mut bits = vec![0u8];
    bits.extend(tlv(0x04, &t.ephemeral));
    let eph = tlv(0xA0, &[alg(key_oid, Some(seq(&params))), tlv(0x03, &bits)].concat());
    let sek = seq(&[tlv(0x04, &t.encrypted_key), tlv(0x04, &t.mac)]);
    let tp = [oid_text(&t.param_set)?, eph, tlv(0x04, &t.ukm)].concat();
    Ok(seq(&[sek, tlv(0xA0, &tp)]))
}

/// Письмо ГОСТ из того, что вернуло СКЗИ (шифрование — там). [проверить с КриптоПро CSP]
pub fn gost_message(raw: &[u8], sealed: &GostSealed) -> Result<Vec<u8>, SealError> {
    let (outer, _) = crate::sign::split_entity(raw).ok_or_else(|| failed("no body"))?;
    let mut infos = Vec::new();
    for (p, t) in &sealed.recipients {
        let key_oid = match t.key {
            crate::envelope::GostKey::G2001 => oid::GOST2001_KEY,
            crate::envelope::GostKey::G2012_256 => oid::GOST_KEY_256,
            crate::envelope::GostKey::G2012_512 => oid::GOST_KEY_512,
        };
        infos.extend(seq(&[tlv(0x02, &[0]), rid(p), alg(key_oid, None), tlv(0x04, &gost_transport(t)?)]));
    }
    let params = seq(&[tlv(0x04, &sealed.iv), oid_text(&sealed.param_set)?]);
    let eci = seq(&[oid_der(oid::DATA), alg(oid::GOST28147, Some(params)), tlv(0x80, &sealed.ciphertext)]);
    let body = seq(&[tlv(0x02, &[0]), tlv(0x31, &infos), eci]);
    let cms = seq(&[oid_der(oid::ENVELOPED_DATA), tlv(0xA0, &body)]);
    Ok(letter(&outer, &cms, false))
}

/// MIME-сущность письма (для СКЗИ: оно шифрует её само).
pub fn entity(raw: &[u8]) -> Option<Vec<u8>> {
    crate::sign::split_entity(raw).map(|(_, e)| e)
}
