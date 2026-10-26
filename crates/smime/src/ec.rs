//! Эллиптические кривые в форме Вейерштрасса `y² = x³ + ax + b` над простым полем — только проверка подписи
//! (открытые данные, постоянное время не нужно): ECDSA (P-256, P-384) и ГОСТ Р 34.10-2012 (наборы ТК 26 и
//! КриптоПро; наборы в форме скрученных кривых Эдвардса — 256-A и 512-C — заданы своей формой Вейерштрасса).
//!
//! Арифметика — `crypto-bigint` (`U512`, форма Монтгомери с модулем во время работы): поле до 512 бит. Точки — в
//! якобиевых координатах, `u₁·G + u₂·Q` — одним проходом (приём Шамира), одно обращение в конце.

use std::sync::OnceLock;

use const_oid::ObjectIdentifier as Oid;
use crypto_bigint::modular::{FixedMontyForm, FixedMontyParams};
use crypto_bigint::{NonZero, Odd, U512};

use crate::oid;

type Fe = FixedMontyForm<{ U512::LIMBS }>;
type Params = FixedMontyParams<{ U512::LIMBS }>;

/// Кривая с подгруппой простого порядка `q` и образующей `g`.
pub(crate) struct Curve {
    /// Набор параметров (OID); у одинаковых наборов КриптоПро и ТК 26 — свои записи с теми же числами.
    pub oid: Oid,
    pub name: &'static str,
    /// Длина координаты в байтах (32, 48, 64).
    pub bytes: usize,
    p: Params,
    a: Fe,
    b: Fe,
    q: U512,
    qm: Params,
    g: (U512, U512),
}

/// Точка в якобиевых координатах: `(X/Z², Y/Z³)`.
#[derive(Clone, Copy)]
struct Jac {
    x: Fe,
    y: Fe,
    z: Fe,
    inf: bool,
}

/// Беззнаковое целое из big-endian байтов (ведущие нули отбрасываются); длиннее 512 бит — `None`.
pub(crate) fn uint_be(bytes: &[u8]) -> Option<U512> {
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
    let bytes = &bytes[start..];
    if bytes.len() > 64 {
        return None;
    }
    let mut buf = [0u8; 64];
    buf[64 - bytes.len()..].copy_from_slice(bytes);
    Some(U512::from_be_slice(&buf))
}

/// То же из little-endian (ГОСТ: координаты ключа и значение хэша).
pub(crate) fn uint_le(bytes: &[u8]) -> Option<U512> {
    let mut be = bytes.to_vec();
    be.reverse();
    uint_be(&be)
}

/// Big-endian байты фиксированной длины `n` (младшие `n` байтов числа).
#[cfg(any(test, feature = "testkit"))]
pub(crate) fn to_be(x: &U512, n: usize) -> Vec<u8> {
    let all = x.to_be_bytes();
    let all: &[u8] = all.as_ref();
    all[all.len().saturating_sub(n)..].to_vec()
}

fn hex(s: &str) -> Option<Vec<u8>> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    let s = if s.len() % 2 == 1 { format!("0{s}") } else { s };
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

fn params(m: &U512) -> Option<Params> {
    Odd::new(*m).into_option().map(Params::new_vartime)
}

/// Строка таблицы: OID, имя, длина координаты, затем a, b, p, q, x, y (hex).
type Row = (Oid, &'static str, usize, [&'static str; 6]);

const P256_ROW: [&str; 6] = [
    "FFFFFFFF00000001000000000000000000000000FFFFFFFFFFFFFFFFFFFFFFFC",
    "5AC635D8AA3A93E7B3EBBD55769886BC651D06B0CC53B0F63BCE3C3E27D2604B",
    "FFFFFFFF00000001000000000000000000000000FFFFFFFFFFFFFFFFFFFFFFFF",
    "FFFFFFFF00000000FFFFFFFFFFFFFFFFBCE6FAADA7179E84F3B9CAC2FC632551",
    "6B17D1F2E12C4247F8BCE6E563A440F277037D812DEB33A0F4A13945D898C296",
    "4FE342E2FE1A7F9B8EE7EB4A7C0F9E162BCE33576B315ECECBB6406837BF51F5",
];
const P384_ROW: [&str; 6] = [
    "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFFFF0000000000000000FFFFFFFC",
    "B3312FA7E23EE7E4988E056BE3F82D19181D9C6EFE8141120314088F5013875AC656398D8A2ED19D2A85C8EDD3EC2AEF",
    "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFFFF0000000000000000FFFFFFFF",
    "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFC7634D81F4372DDF581A0DB248B0A77AECEC196ACCC52973",
    "AA87CA22BE8B05378EB1C71EF320AD746E1D3B628BA79B9859F741E082542A385502F25DBF55296C3A545E3872760AB7",
    "3617DE4A96262C6F5D9E98BF9292DC29F8F41DBD289A147CE9DA3113B5F0B8C00A60B1CE1D7E819D7A431D7C90EA0E5F",
];
// ГОСТ: RFC 7091 §7.1 (тестовый), RFC 4357 §11.4 (КриптоПро A, B, C = ТК 26 256-B, C, D), RFC 7836 (256-A, 512-C),
// ТК 26 / RFC 9215 прил. C (512-A, 512-B, тестовый 512).
const GOST_TEST_ROW: [&str; 6] = [
    "7",
    "5FBFF498AA938CE739B8E022FBAFEF40563F6E6A3472FC2A514C0CE9DAE23B7E",
    "8000000000000000000000000000000000000000000000000000000000000431",
    "8000000000000000000000000000000150FE8A1892976154C59CFC193ACCF5B3",
    "2",
    "08E2A8A0E65147D4BD6316030E16D19C85C97F0A9CA267122B96ABBCEA7E8FC8",
];
const CP_A_ROW: [&str; 6] = [
    "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFD94",
    "A6",
    "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFD97",
    "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF6C611070995AD10045841B09B761B893",
    "1",
    "8D91E471E0989CDA27DF505A453F2B7635294F2DDF23E3B122ACC99C9E9F1E14",
];
const CP_B_ROW: [&str; 6] = [
    "8000000000000000000000000000000000000000000000000000000000000C96",
    "3E1AF419A269A5F866A7D3C25C3DF80AE979259373FF2B182F49D4CE7E1BBC8B",
    "8000000000000000000000000000000000000000000000000000000000000C99",
    "800000000000000000000000000000015F700CFFF1A624E5E497161BCC8A198F",
    "1",
    "3FA8124359F96680B83D1C3EB2C070E5C545C9858D03ECFB744BF8D717717EFC",
];
const CP_C_ROW: [&str; 6] = [
    "9B9F605F5A858107AB1EC85E6B41C8AACF846E86789051D37998F7B9022D7598",
    "805A",
    "9B9F605F5A858107AB1EC85E6B41C8AACF846E86789051D37998F7B9022D759B",
    "9B9F605F5A858107AB1EC85E6B41C8AA582CA3511EDDFB74F02F3A6598980BB9",
    "0",
    "41ECE55743711A8C3CBF3783CD08C0EE4D4DC440D4641A8F366E550DFDB3BB67",
];
const TC26_256_A_ROW: [&str; 6] = [
    "C2173F1513981673AF4892C23035A27CE25E2013BF95AA33B22C656F277E7335",
    "295F9BAE7428ED9CCC20E7C359A9D41A22FCCD9108E17BF7BA9337A6F8AE9513",
    "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFD97",
    "400000000000000000000000000000000FD8CDDFC87B6635C115AF556C360C67",
    "91E38443A5E82C0D880923425712B2BB658B9196932E02C78B2582FE742DAA28",
    "32879423AB1A0375895786C4BB46E9565FDE0B5344766740AF268ADB32322E5C",
];
const TC26_512_TEST_ROW: [&str; 6] = [
    "7",
    "1CFF0806A31116DA29D8CFA54E57EB748BC5F377E49400FDD788B649ECA1AC4361834013B2AD7322480A89CA58E0CF74BC9E540C2ADD6897FAD0A3084F302ADC",
    "4531ACD1FE0023C7550D267B6B2FEE80922B14B2FFB90F04D4EB7C09B5D2D15DF1D852741AF4704A0458047E80E4546D35B8336FAC224DD81664BBF528BE6373",
    "4531ACD1FE0023C7550D267B6B2FEE80922B14B2FFB90F04D4EB7C09B5D2D15DA82F2D7ECB1DBAC719905C5EECC423F1D86E25EDBE23C595D644AAF187E6E6DF",
    "24D19CC64572EE30F396BF6EBBFD7A6C5213B3B3D7057CC825F91093A68CD762FD60611262CD838DC6B60AA7EEE804E28BC849977FAC33B4B530F1B120248A9A",
    "2BB312A43BD2CE6E0D020613C857ACDDCFBF061E91E5F2C3F32447C259F39B2C83AB156D77F1496BF7EB3351E1EE4E43DC1A18B91B24640B6DBB92CB1ADD371E",
];
const TC26_512_A_ROW: [&str; 6] = [
    "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFDC4",
    "E8C2505DEDFC86DDC1BD0B2B6667F1DA34B82574761CB0E879BD081CFD0B6265EE3CB090F30D27614CB4574010DA90DD862EF9D4EBEE4761503190785A71C760",
    "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFDC7",
    "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF27E69532F48D89116FF22B8D4E0560609B4B38ABFAD2B85DCACDB1411F10B275",
    "3",
    "7503CFE87A836AE3A61B8816E25450E6CE5E1C93ACF1ABC1778064FDCBEFA921DF1626BE4FD036E93D75E6A50E3A41E98028FE5FC235F5B889A589CB5215F2A4",
];
const TC26_512_B_ROW: [&str; 6] = [
    "8000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000006C",
    "687D1B459DC841457E3E06CF6F5E2517B97C7D614AF138BCBF85DC806C4B289F3E965D2DB1416D217F8B276FAD1AB69C50F78BEE1FA3106EFB8CCBC7C5140116",
    "8000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000006F",
    "800000000000000000000000000000000000000000000000000000000000000149A1EC142565A545ACFDB77BD9D40CFA8B996712101BEA0EC6346C54374F25BD",
    "2",
    "1A8F7EDA389B094C2C071E3647A8940F3C123B697578C213BE6DD9E6C8EC7335DCB228FD1EDF4A39152CBCAAF8C0398828041055F94CEEEC7E21340780FE41BD",
];
const TC26_512_C_ROW: [&str; 6] = [
    "DC9203E514A721875485A529D2C722FB187BC8980EB866644DE41C68E143064546E861C0E2C9EDD92ADE71F46FCF50FF2AD97F951FDA9F2A2EB6546F39689BD3",
    "B4C4EE28CEBC6C2C8AC12952CF37F16AC7EFB6A9F69F4B57FFDA2E4F0DE5ADE038CBC2FFF719D2C18DE0284B8BFEF3B52B8CC7A5F5BF0A3C8D2319A5312557E1",
    "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFDC7",
    "3FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFC98CDBA46506AB004C33A9FF5147502CC8EDA9E7A769A12694623CEF47F023ED",
    "E2E31EDFC23DE7BDEBE241CE593EF5DE2295B7A9CBAEF021D385F7074CEA043AA27272A7AE602BF2A7B9033DB9ED3610C6FB85487EAE97AAC5BC7928C1950148",
    "F5CE40D95B5EB899ABBCCFF5911CB8577939804D6527378B8C108C3D2090FF9BE18E2D33E3021ED2EF32D85822423B6304F726AA854BAE07D0396E9A9ADDC40F",
];

const TABLE: &[Row] = &[
    (oid::P256, "P-256", 32, P256_ROW),
    (oid::P384, "P-384", 48, P384_ROW),
    (oid::GOST_TEST_256, "test-256", 32, GOST_TEST_ROW),
    (oid::CRYPTOPRO_A, "CryptoPro-A", 32, CP_A_ROW),
    (oid::CRYPTOPRO_B, "CryptoPro-B", 32, CP_B_ROW),
    (oid::CRYPTOPRO_C, "CryptoPro-C", 32, CP_C_ROW),
    (oid::CRYPTOPRO_XCH_A, "CryptoPro-XchA", 32, CP_A_ROW),
    (oid::CRYPTOPRO_XCH_B, "CryptoPro-XchB", 32, CP_C_ROW),
    (oid::TC26_256_A, "TC26-256-A", 32, TC26_256_A_ROW),
    (oid::TC26_256_B, "TC26-256-B", 32, CP_A_ROW),
    (oid::TC26_256_C, "TC26-256-C", 32, CP_B_ROW),
    (oid::TC26_256_D, "TC26-256-D", 32, CP_C_ROW),
    (oid::TC26_512_TEST, "test-512", 64, TC26_512_TEST_ROW),
    (oid::TC26_512_A, "TC26-512-A", 64, TC26_512_A_ROW),
    (oid::TC26_512_B, "TC26-512-B", 64, TC26_512_B_ROW),
    (oid::TC26_512_C, "TC26-512-C", 64, TC26_512_C_ROW),
];

fn build(row: &Row) -> Option<Curve> {
    let (oid, name, bytes, [a, b, p, q, x, y]) = row;
    let num = |s: &str| hex(s).and_then(|b| uint_be(&b));
    let (p, q) = (num(p)?, num(q)?);
    let pp = params(&p)?;
    Some(Curve {
        oid: *oid,
        name,
        bytes: *bytes,
        a: Fe::new(&num(a)?, &pp),
        b: Fe::new(&num(b)?, &pp),
        p: pp,
        qm: params(&q)?,
        q,
        g: (num(x)?, num(y)?),
    })
}

/// Все кривые (собираются один раз).
pub(crate) fn curves() -> &'static [Curve] {
    static CURVES: OnceLock<Vec<Curve>> = OnceLock::new();
    CURVES.get_or_init(|| TABLE.iter().filter_map(build).collect())
}

/// Кривая по OID набора параметров.
pub(crate) fn curve(oid: &Oid) -> Option<&'static Curve> {
    curves().iter().find(|c| &c.oid == oid)
}

impl Curve {
    fn fe(&self, v: &U512) -> Option<Fe> {
        (v < self.p.modulus().as_ref()).then(|| Fe::new(v, &self.p))
    }

    fn small(&self, v: u64) -> Fe {
        Fe::new(&U512::from_u64(v), &self.p)
    }

    pub(crate) fn order(&self) -> &U512 {
        &self.q
    }

    /// Точка `(x, y)` лежит на кривой.
    pub(crate) fn on_curve(&self, x: &U512, y: &U512) -> bool {
        let (Some(x), Some(y)) = (self.fe(x), self.fe(y)) else { return false };
        let rhs = x.square().mul(&x).add(&self.a.mul(&x)).add(&self.b);
        y.square().retrieve() == rhs.retrieve()
    }

    fn jac(&self, x: &U512, y: &U512) -> Option<Jac> {
        Some(Jac { x: self.fe(x)?, y: self.fe(y)?, z: self.small(1), inf: false })
    }

    fn infinity(&self) -> Jac {
        Jac { x: self.small(1), y: self.small(1), z: self.small(0), inf: true }
    }

    fn is_zero(v: &Fe) -> bool {
        v.retrieve().is_zero_vartime()
    }

    fn dbl(&self, p: &Jac) -> Jac {
        if p.inf || Self::is_zero(&p.y) {
            return self.infinity();
        }
        let xx = p.x.square();
        let yy = p.y.square();
        let yyyy = yy.square();
        let zz = p.z.square();
        let s = p.x.mul(&yy).double().double();
        let m = xx.double().add(&xx).add(&self.a.mul(&zz.square()));
        let x3 = m.square().sub(&s.double());
        let y3 = m.mul(&s.sub(&x3)).sub(&yyyy.double().double().double());
        let z3 = p.y.mul(&p.z).double();
        Jac { x: x3, y: y3, z: z3, inf: false }
    }

    fn add(&self, p: &Jac, q: &Jac) -> Jac {
        if p.inf {
            return *q;
        }
        if q.inf {
            return *p;
        }
        let z1z1 = p.z.square();
        let z2z2 = q.z.square();
        let u1 = p.x.mul(&z2z2);
        let u2 = q.x.mul(&z1z1);
        let s1 = p.y.mul(&q.z).mul(&z2z2);
        let s2 = q.y.mul(&p.z).mul(&z1z1);
        if u1.retrieve() == u2.retrieve() {
            return if s1.retrieve() == s2.retrieve() { self.dbl(p) } else { self.infinity() };
        }
        let h = u2.sub(&u1);
        let r = s2.sub(&s1);
        let hh = h.square();
        let hhh = hh.mul(&h);
        let v = u1.mul(&hh);
        let x3 = r.square().sub(&hhh).sub(&v.double());
        let y3 = r.mul(&v.sub(&x3)).sub(&s1.mul(&hhh));
        let z3 = h.mul(&p.z).mul(&q.z);
        Jac { x: x3, y: y3, z: z3, inf: false }
    }

    /// Аффинная `x` точки `u1·G + u2·Q`; бесконечность или `Q` не на кривой — `None`.
    pub(crate) fn mul2_x(&self, u1: &U512, u2: &U512, qx: &U512, qy: &U512) -> Option<U512> {
        if !self.on_curve(qx, qy) {
            return None;
        }
        let g = self.jac(&self.g.0, &self.g.1)?;
        let q = self.jac(qx, qy)?;
        let gq = self.add(&g, &q);
        let bits = u1.bits_vartime().max(u2.bits_vartime());
        let mut acc = self.infinity();
        for i in (0..bits).rev() {
            acc = self.dbl(&acc);
            match (u1.bit_vartime(i), u2.bit_vartime(i)) {
                (true, true) => acc = self.add(&acc, &gq),
                (true, false) => acc = self.add(&acc, &g),
                (false, true) => acc = self.add(&acc, &q),
                (false, false) => {}
            }
        }
        self.affine_x(&acc)
    }

    fn affine_x(&self, p: &Jac) -> Option<U512> {
        if p.inf || Self::is_zero(&p.z) {
            return None;
        }
        let zi = p.z.invert_vartime().into_option()?;
        Some(p.x.mul(&zi.square()).retrieve())
    }

    /// `k·G` в аффинных координатах (тесты и тестовый подписант).
    #[cfg(any(test, feature = "testkit"))]
    pub(crate) fn mul_g(&self, k: &U512) -> Option<(U512, U512)> {
        let g = self.jac(&self.g.0, &self.g.1)?;
        let mut acc = self.infinity();
        for i in (0..k.bits_vartime()).rev() {
            acc = self.dbl(&acc);
            if k.bit_vartime(i) {
                acc = self.add(&acc, &g);
            }
        }
        if acc.inf || Self::is_zero(&acc.z) {
            return None;
        }
        let zi = acc.z.invert_vartime().into_option()?;
        let z2 = zi.square();
        Some((acc.x.mul(&z2).retrieve(), acc.y.mul(&z2).mul(&zi).retrieve()))
    }

    // --- арифметика по модулю порядка `q` ---

    pub(crate) fn reduce(&self, v: &U512) -> U512 {
        match NonZero::new(self.q).into_option() {
            Some(q) => v.rem_vartime(&q),
            None => U512::ZERO,
        }
    }

    fn sfe(&self, v: &U512) -> Fe {
        Fe::new(&self.reduce(v), &self.qm)
    }

    /// `a⁻¹ mod q`.
    pub(crate) fn inv_q(&self, a: &U512) -> Option<U512> {
        self.sfe(a).invert_vartime().into_option().map(|f| f.retrieve())
    }

    pub(crate) fn mul_q(&self, a: &U512, b: &U512) -> U512 {
        self.sfe(a).mul(&self.sfe(b)).retrieve()
    }

    #[cfg(any(test, feature = "testkit"))]
    pub(crate) fn add_q(&self, a: &U512, b: &U512) -> U512 {
        self.sfe(a).add(&self.sfe(b)).retrieve()
    }

    pub(crate) fn neg_q(&self, a: &U512) -> U512 {
        self.sfe(&U512::ZERO).sub(&self.sfe(a)).retrieve()
    }

    /// `0 < v < q`.
    pub(crate) fn in_range(&self, v: &U512) -> bool {
        !v.is_zero_vartime() && v < &self.q
    }
}

/// Проверка ECDSA (SEC 1 §4.1.4): `e` — хэш, усечённый до длины порядка; `r`, `s` — из `Ecdsa-Sig-Value`.
pub(crate) fn ecdsa_verify(c: &Curve, qx: &U512, qy: &U512, hash: &[u8], r: &U512, s: &U512) -> bool {
    if !c.in_range(r) || !c.in_range(s) {
        return false;
    }
    let qbits = c.order().bits_vartime() as usize;
    let take = hash.len().min(qbits.div_ceil(8));
    let Some(mut e) = uint_be(&hash[..take]) else { return false };
    if take * 8 > qbits {
        e = e.shr_vartime((take * 8 - qbits) as u32);
    }
    let Some(w) = c.inv_q(s) else { return false };
    let u1 = c.mul_q(&e, &w);
    let u2 = c.mul_q(r, &w);
    c.mul2_x(&u1, &u2, qx, qy).is_some_and(|x| &c.reduce(&x) == r)
}

/// Проверка ГОСТ Р 34.10-2012 (алгоритм II): `e` — хэш как little-endian число по модулю `q` (0 → 1),
/// `z₁ = s·e⁻¹`, `z₂ = −r·e⁻¹`, `R = x(z₁·P + z₂·Q) mod q`.
pub(crate) fn gost_verify(c: &Curve, qx: &U512, qy: &U512, hash_le: &[u8], r: &U512, s: &U512) -> bool {
    if !c.in_range(r) || !c.in_range(s) {
        return false;
    }
    let Some(e) = uint_le(hash_le) else { return false };
    let mut e = c.reduce(&e);
    if e.is_zero_vartime() {
        e = U512::ONE;
    }
    let Some(v) = c.inv_q(&e) else { return false };
    let z1 = c.mul_q(s, &v);
    let z2 = c.mul_q(&c.neg_q(r), &v);
    c.mul2_x(&z1, &z2, qx, qy).is_some_and(|x| &c.reduce(&x) == r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(s: &str) -> U512 {
        uint_be(&hex(s).unwrap()).unwrap()
    }

    /// Константы таблицы переписаны верно: образующая на кривой, `q·G` — бесконечность, `(q−1)·G = −G`.
    #[test]
    fn every_curve_is_consistent() {
        assert_eq!(curves().len(), TABLE.len(), "все строки таблицы разобраны");
        for c in curves() {
            assert!(c.on_curve(&c.g.0, &c.g.1), "{}: G не на кривой", c.name);
            assert!(c.mul_g(&c.q).is_none(), "{}: q·G ≠ O", c.name);
            let (x, y) = c.mul_g(&c.q.wrapping_sub(&U512::ONE)).unwrap();
            assert_eq!(x, c.g.0, "{}", c.name);
            assert_eq!(c.fe(&y).unwrap().add(&c.fe(&c.g.1).unwrap()).retrieve(), U512::ZERO, "{}", c.name);
            assert_eq!(c.bytes * 8, c.p.modulus().as_ref().bits_vartime().next_multiple_of(8) as usize, "{}", c.name);
        }
    }

    /// ГОСТ Р 34.10-2012, приложение A.1 (RFC 7091 §7): открытый ключ из закрытого и проверка подписи.
    #[test]
    fn gost_256_standard_example() {
        let c = curve(&oid::GOST_TEST_256).unwrap();
        let d = h("7A929ADE789BB9BE10ED359DD39A72C11B60961F49397EEE1D19CE9891EC3B28");
        let (qx, qy) = c.mul_g(&d).unwrap();
        assert_eq!(qx, h("7F2B49E270DB6D90D8595BEC458B50C58585BA1D4E9B788F6689DBD8E56FD80B"));
        assert_eq!(qy, h("26F1B489D6701DD185C8413A977B3CBBAF64D1C593D26627DFFB101A87FF77DA"));
        let e = h("2DFBC1B372D89A1188C09C52E0EEC61FCE52032AB1022E8E67ECE6672B043EE5");
        let r = h("41AA28D2F1AB148280CD9ED56FEDA41974053554A42767B83AD043FD39DC0493");
        let s = h("01456C64BA4642A1653C235A98A60249BCD6D3F746B631DF928014F6C5BF9C40");
        // В стандарте `e` — число; хэш — его little-endian байты.
        let mut e_le = to_be(&e, 32);
        e_le.reverse();
        assert!(gost_verify(c, &qx, &qy, &e_le, &r, &s));
        assert!(!gost_verify(c, &qx, &qy, &e_le, &r, &s.wrapping_add(&U512::ONE)));
        e_le[0] ^= 1;
        assert!(!gost_verify(c, &qx, &qy, &e_le, &r, &s));
    }

    /// ГОСТ Р 34.10-2012, приложение A.2 (512 бит): ключ, подпись (`r`, `s` при заданном `k`) и проверка.
    #[test]
    fn gost_512_standard_example() {
        let c = curve(&oid::TC26_512_TEST).unwrap();
        let d = h("0BA6048AADAE241BA40936D47756D7C93091A0E8514669700EE7508E508B102072E8123B2200A0563322DAD2827E2714A2636B7BFD18AADFC62967821FA18DD4");
        let (qx, qy) = c.mul_g(&d).unwrap();
        assert_eq!(
            qx,
            h("115DC5BC96760C7B48598D8AB9E740D4C4A85A65BE33C1815B5C320C854621DD5A515856D13314AF69BC5B924C8B4DDFF75C45415C1D9DD9DD33612CD530EFE1")
        );
        assert_eq!(
            qy,
            h("37C7C90CD40B0F5621DC3AC1B751CFA0E2634FA0503B3D52639F5D7FB72AFD61EA199441D943FFE7F0C70A2759A3CDB84C114E1F9339FDF27F35ECA93677BEEC")
        );
        let e = h("3754F3CFACC9E0615C4F4A7C4D8DAB531B09B6F9C170C533A71D147035B0C5917184EE536593F4414339976C647C5D5A407ADEDB1D560C4FC6777D2972075B8C");
        let k = h("0359E7F4B1410FEACC570456C6801496946312120B39D019D455986E364F365886748ED7A44B3E794434006011842286212273A6D14CF70EA3AF71BB1AE679F1");
        let r = c.reduce(&c.mul_g(&k).unwrap().0);
        let s = c.add_q(&c.mul_q(&r, &d), &c.mul_q(&k, &e));
        assert_eq!(
            r,
            h("2F86FA60A081091A23DD795E1E3C689EE512A3C82EE0DCC2643C78EEA8FCACD35492558486B20F1C9EC197C90699850260C93BCBCD9C5C3317E19344E173AE36")
        );
        assert_eq!(
            s,
            h("1081B394696FFE8E6585E7A9362D26B6325F56778AADBC081C0BFBE933D52FF5823CE288E8C4F362526080DF7F70CE406A6EEB1F56919CB92A9853BDE73E5B4A")
        );
        let mut e_le = to_be(&e, 64);
        e_le.reverse();
        assert!(gost_verify(c, &qx, &qy, &e_le, &r, &s));
        assert!(!gost_verify(c, &qy, &qx, &e_le, &r, &s), "точка не на кривой — отказ");
    }

    #[test]
    fn integers_from_bytes() {
        assert_eq!(uint_be(&[0, 0, 1, 2]), Some(U512::from_u64(0x102)));
        assert_eq!(uint_le(&[2, 1, 0]), Some(U512::from_u64(0x102)));
        assert!(uint_be(&[1; 65]).is_none());
        assert_eq!(uint_be(&[0; 70]), Some(U512::ZERO));
        assert_eq!(to_be(&U512::from_u64(0x102), 3), vec![0, 1, 2]);
        assert_eq!(hex("abc"), Some(vec![0x0a, 0xbc]));
        assert_eq!(hex("zz"), None);
    }
}
