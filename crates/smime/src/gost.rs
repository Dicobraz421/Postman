//! ГОСТ Р 34.10-2012 (подпись) и ГОСТ Р 34.11-2012 «Стрибог» (хэш) — только проверка подписи.
//!
//! **Не сертифицировано.** Это своя реализация на открытых крейтах (`streebog` RustCrypto, арифметика кривых —
//! `ec.rs` на `crypto-bigint`), не СКЗИ: она не проходила сертификацию ФСБ России и не заменяет КриптоПро CSP или
//! ViPNet там, где нужна сертифицированная криптография. Закрытых ключей и подписи здесь нет — только проверка
//! открытым ключом. Подключить внешнее СКЗИ (через CryptoAPI Windows) можно позже, решение владельца 2026-10-21.
//!
//! Кодирование — RFC 9215 и рекомендации ТК 26 (как у КриптоПро, OpenSSL gost-engine):
//! - открытый ключ — `OCTET STRING` из `x‖y`, каждая координата little-endian (64 или 128 байт) внутри
//!   `subjectPublicKey`;
//! - параметры ключа — `SEQUENCE { publicKeyParamSet OID, digestParamSet OID OPTIONAL }`;
//! - подпись — `s‖r`, каждое big-endian (RFC 4491 §2.2.2), в `BIT STRING` сертификата и `OCTET STRING` CMS;
//! - значение хэша «Стрибог» — little-endian число (байты результата в обратном порядке).
//!
//! Наборы параметров: 256 бит — ТК 26 A–D и КриптоПро A, B, C, XchA, XchB (A–D ТК 26 = КриптоПро и 256-A в форме
//! скрученной кривой Эдвардса, заданной формой Вейерштрасса), 512 бит — ТК 26 A–C, тестовые наборы стандарта.

use const_oid::ObjectIdentifier as Oid;
use der::Decode;
use der::asn1::OctetString;

use crate::ec::{self, Curve};
use crate::oid;

/// Длина ключа: 256 или 512 бит.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    S256,
    S512,
}

impl Size {
    pub(crate) fn bytes(self) -> usize {
        match self {
            Size::S256 => 32,
            Size::S512 => 64,
        }
    }

    pub(crate) fn of_key(alg: &Oid) -> Option<Size> {
        match *alg {
            oid::GOST_KEY_256 => Some(Size::S256),
            oid::GOST_KEY_512 => Some(Size::S512),
            _ => None,
        }
    }
}

/// `GostR3410-2012-PublicKeyParameters` (RFC 9215 §4.2).
#[derive(der::Sequence)]
struct KeyParams {
    param_set: Oid,
    digest: Option<Oid>,
}

/// Набор параметров ключа из `AlgorithmIdentifier.parameters` (DER `SEQUENCE`).
pub(crate) fn param_set(params_der: &[u8]) -> Option<Oid> {
    KeyParams::from_der(params_der).ok().map(|p| p.param_set)
}

/// Открытый ключ ГОСТ: кривая и точка.
pub(crate) struct Key {
    pub curve: &'static Curve,
    pub x: crypto_bigint::U512,
    pub y: crypto_bigint::U512,
}

/// Ключ из `subjectPublicKey` (внутри — DER `OCTET STRING` из `x‖y` little-endian) и набора параметров.
pub(crate) fn key(size: Size, param_set: &Oid, subject_public_key: &[u8]) -> Option<Key> {
    let curve = ec::curve(param_set).filter(|c| c.bytes == size.bytes())?;
    let point = OctetString::from_der(subject_public_key).ok()?;
    let point = point.as_bytes();
    let n = curve.bytes;
    if point.len() != 2 * n {
        return None;
    }
    let x = ec::uint_le(&point[..n])?;
    let y = ec::uint_le(&point[n..])?;
    curve.on_curve(&x, &y).then_some(Key { curve, x, y })
}

/// Проверка подписи `s‖r` (big-endian) над значением хэша `digest` (байты «Стрибога» как есть).
pub(crate) fn verify(key: &Key, digest: &[u8], signature: &[u8]) -> bool {
    let n = key.curve.bytes;
    if signature.len() != 2 * n {
        return false;
    }
    let (Some(s), Some(r)) = (ec::uint_be(&signature[..n]), ec::uint_be(&signature[n..])) else { return false };
    ec::gost_verify(key.curve, &key.x, &key.y, digest, &r, &s)
}

/// Все поддержанные наборы параметров данного размера (для таблицы алгоритмов цепочки).
pub(crate) fn param_sets(size: Size) -> impl Iterator<Item = Oid> {
    ec::curves().iter().filter(move |c| c.bytes == size.bytes() && c.name != "P-256" && c.name != "P-384").map(|c| c.oid)
}

/// Тестовый подписант (только тесты и `testkit`): `k` детерминирован, подпись — `s‖r`.
#[cfg(any(test, feature = "testkit"))]
pub(crate) fn sign(curve: &Curve, d: &crypto_bigint::U512, digest: &[u8], k: &crypto_bigint::U512) -> Option<Vec<u8>> {
    let e = ec::uint_le(digest)?;
    let mut e = curve.reduce(&e);
    if e.is_zero_vartime() {
        e = crypto_bigint::U512::ONE;
    }
    let k = curve.reduce(k);
    let r = curve.reduce(&curve.mul_g(&k)?.0);
    let s = curve.add_q(&curve.mul_q(&r, d), &curve.mul_q(&k, &e));
    if r.is_zero_vartime() || s.is_zero_vartime() {
        return None;
    }
    let mut out = ec::to_be(&s, curve.bytes);
    out.extend(ec::to_be(&r, curve.bytes));
    Some(out)
}
