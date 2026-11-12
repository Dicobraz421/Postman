//! ГОСТ через СКЗИ (S5-12 Ф2, решение владельца 2026-10-22): операции с закрытым ключом ГОСТ — **только** в
//! сертифицированном СКЗИ (КриптоПро CSP, ViPNet CSP) через CryptoAPI; ключ ГОСТ в файле не хранится никогда. Нет
//! СКЗИ — «нужно СКЗИ» (`KeyError::NeedSkzi`).
//!
//! Расшифровка (RFC 4490, ГОСТ 28147-89 с экспортом ключа КриптоПро): эфемерный ключ отправителя импортируется
//! `PUBLICKEYBLOB` на своём ключе (`CryptGetUserKey`) — ключ согласования (VKO); у него `KP_ALGID` = экспорт КриптоПро
//! (`CALG_PRO12_EXPORT` / `CALG_PRO_EXPORT`); им импортируется `SIMPLEBLOB` (синхропосылка, зашифрованный ключ,
//! имитовставка) — сессионный ключ; содержимое расшифровывает `CryptDecrypt` с IV и набором параметров из письма.
//! Ключ содержимого из СКЗИ не выходит, поэтому содержимое расшифровывается здесь, а разбирает его воркер
//! (`SmimeInner`). Все поля — от воркера значениями; блобы собираются здесь, чужой DER не разбирается.
//!
//! Константы и форматы блобов — по документации КриптоПро CSP (`WinCryptEx.h`, «Структуры ключевых блобов»)
//! [проверить с КриптоПро CSP]: СКЗИ на стенде нет, путь не проверен на живом провайдере.

use zeroize::Zeroizing;

/// Расшифрованное СКЗИ внутреннее письмо (только в памяти).
pub type Plain = Zeroizing<Vec<u8>>;

/// Блобы КриптоПро собираются только для CryptoAPI (Windows); на других ОС СКЗИ нет (`KeyError::NeedSkzi`), разметку
/// блобов проверяют тесты на любой ОС.
#[cfg(windows)]
pub(crate) use blob::{CALG_G28147, KP_CIPHEROID, cipher_oid, export_alg, public_key_blob, simple_blob};

#[cfg(any(windows, test))]
mod blob {
    #[cfg(windows)]
    use postman_smime::envelope::GostContent;
    use postman_smime::envelope::{GostKey, GostTransport};

    /// `BLOBHEADER.bVersion` блобов КриптоПро.
    const BLOB_VERSION: u8 = 0x20;
    /// `ALG_ID` КриптоПро [проверить]: ГОСТ 28147-89, экспорт ключа КриптоПро (2001 и 2012), открытый ключ ГОСТ Р 34.10.
    pub(crate) const CALG_G28147: u32 = 0x661E;
    pub(crate) const CALG_PRO_EXPORT: u32 = 0x661F;
    pub(crate) const CALG_PRO12_EXPORT: u32 = 0x6621;
    pub(crate) const CALG_GR3410EL: u32 = 0x2E23;
    pub(crate) const CALG_GR3410_12_256: u32 = 0x2E49;
    pub(crate) const CALG_GR3410_12_512: u32 = 0x2E3D;
    /// Магия `CRYPT_PUBKEYPARAM` («MAG1») и `CRYPT_SIMPLEBLOB_HEADER` (ГОСТ 28147-89) [проверить].
    const GR3410_1_MAGIC: u32 = 0x3147_414D;
    const G28147_MAGIC: u32 = 0x374A_51FD;
    /// `KP_CIPHEROID` КриптоПро: набор параметров шифра (OID строкой с нулём) [проверить].
    #[cfg(windows)]
    pub(crate) const KP_CIPHEROID: u32 = 104;

    fn header(kind: u8, alg: u32) -> Vec<u8> {
        let mut out = vec![kind, BLOB_VERSION, 0, 0];
        out.extend_from_slice(&alg.to_le_bytes());
        out
    }

    fn oid_der(text: &str) -> Option<Vec<u8>> {
        postman_smime::envelope::oid_der(text)
    }

    fn seq(parts: &[Vec<u8>]) -> Vec<u8> {
        let body = parts.concat();
        let mut out = vec![0x30];
        if body.len() < 0x80 {
            out.push(body.len() as u8);
        } else {
            out.extend([0x81, body.len() as u8]);
        }
        out.extend(body);
        out
    }

    /// `PUBLICKEYBLOB` эфемерного ключа: заголовок, `CRYPT_PUBKEYPARAM`, DER `GostR3410-PublicKeyParameters`, точка
    /// (X‖Y, little-endian).
    pub(crate) fn public_key_blob(t: &GostTransport) -> Option<Vec<u8>> {
        let (alg, bits) = match t.key {
            GostKey::G2001 => (CALG_GR3410EL, 512u32),
            GostKey::G2012_256 => (CALG_GR3410_12_256, 512),
            GostKey::G2012_512 => (CALG_GR3410_12_512, 1024),
        };
        if t.ephemeral.len() != bits as usize / 8 {
            return None;
        }
        let mut out = header(postman_smime_blob::PUBLICKEYBLOB, alg);
        out.extend_from_slice(&GR3410_1_MAGIC.to_le_bytes());
        out.extend_from_slice(&bits.to_le_bytes());
        let mut params = vec![oid_der(&t.public_param_set)?];
        if let Some(d) = &t.digest_param_set {
            params.push(oid_der(d)?);
        }
        out.extend(seq(&params));
        out.extend_from_slice(&t.ephemeral);
        Some(out)
    }

    /// `SIMPLEBLOB` сессионного ключа: заголовок, `CRYPT_SIMPLEBLOB_HEADER`, синхропосылка, зашифрованный ключ,
    /// имитовставка, DER набора параметров.
    pub(crate) fn simple_blob(t: &GostTransport) -> Option<Vec<u8>> {
        if t.ukm.len() != 8 || t.encrypted_key.len() != 32 || t.mac.len() != 4 {
            return None;
        }
        let mut out = header(postman_smime_blob::SIMPLEBLOB, CALG_G28147);
        out.extend_from_slice(&G28147_MAGIC.to_le_bytes());
        out.extend_from_slice(&CALG_G28147.to_le_bytes());
        out.extend_from_slice(&t.ukm);
        out.extend_from_slice(&t.encrypted_key);
        out.extend_from_slice(&t.mac);
        out.extend(seq(&[oid_der(&t.param_set)?]));
        Some(out)
    }

    /// Экспорт ключа КриптоПро по виду ключа.
    pub(crate) fn export_alg(k: GostKey) -> u32 {
        match k {
            GostKey::G2001 => CALG_PRO_EXPORT,
            _ => CALG_PRO12_EXPORT,
        }
    }

    /// Набор параметров шифра — строкой с нулём (`KP_CIPHEROID`).
    #[cfg(windows)]
    pub(crate) fn cipher_oid(c: &GostContent) -> Vec<u8> {
        let mut v = c.param_set.as_bytes().to_vec();
        v.push(0);
        v
    }

    /// Типы блобов CryptoAPI (те же числа, что в `wincrypt.h`; без Windows — для тестов разметки).
    mod postman_smime_blob {
        pub(super) const PUBLICKEYBLOB: u8 = 6;
        pub(super) const SIMPLEBLOB: u8 = 1;
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn transport() -> GostTransport {
            GostTransport {
                key: GostKey::G2012_256,
                param_set: "1.2.643.2.2.31.1".into(),
                ephemeral: vec![7; 64],
                public_param_set: "1.2.643.7.1.2.1.1.1".into(),
                digest_param_set: None,
                ukm: vec![1; 8],
                encrypted_key: vec![2; 32],
                mac: vec![3; 4],
            }
        }

        /// Разметка блобов: заголовок, магия, длины, хвост с точкой и набором параметров.
        #[test]
        fn blob_layout() {
            let t = transport();
            let p = public_key_blob(&t).unwrap();
            assert_eq!(&p[..8], &[6, 0x20, 0, 0, 0x49, 0x2E, 0, 0]);
            assert_eq!(&p[8..12], &GR3410_1_MAGIC.to_le_bytes());
            assert_eq!(&p[12..16], &512u32.to_le_bytes());
            assert_eq!(p[16], 0x30, "DER параметров ключа");
            assert_eq!(&p[p.len() - 64..], &[7; 64][..]);
            let s = simple_blob(&t).unwrap();
            assert_eq!(&s[..8], &[1, 0x20, 0, 0, 0x1E, 0x66, 0, 0]);
            assert_eq!(&s[8..12], &G28147_MAGIC.to_le_bytes());
            assert_eq!(&s[16..24], &[1; 8]);
            assert_eq!(&s[24..56], &[2; 32][..]);
            assert_eq!(&s[56..60], &[3; 4]);
            assert_eq!(s[60], 0x30);
            assert_eq!(export_alg(GostKey::G2001), CALG_PRO_EXPORT);
            let mut bad = t.clone();
            bad.ephemeral.pop();
            assert!(public_key_blob(&bad).is_none());
        }
    }
}
