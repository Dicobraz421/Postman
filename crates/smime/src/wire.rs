//! Бинарная часть запроса и ответа воркера `SmimeVerify`, `SmimeOpen` (Ф2: впереди — ключ содержимого) и `SmimeCrl`:
//! якоря, промежуточные, запрещённые и письмо (у CRL — сертификат, издатель и список); в ответе — DER цепочки и
//! подписанное содержимое. Без JSON: DER в JSON-массивах чисел — вчетверо больше.
//!
//! Формат: `PMS1`, затем списки `u32 LE число` + элементы `u32 LE длина + байты`. Разбор строгий: лишние или
//! недостающие байты — ошибка.

use crate::{Anchor, Trust};

const MAGIC: &[u8; 4] = b"PMS1";

fn put(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

fn put_list<'a>(out: &mut Vec<u8>, items: impl ExactSizeIterator<Item = &'a [u8]>) {
    out.extend_from_slice(&(items.len() as u32).to_le_bytes());
    for i in items {
        put(out, i);
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, rest) = (self.0.get(..n)?, self.0.get(n..)?);
        self.0 = rest;
        Some(head)
    }

    fn u32(&mut self) -> Option<usize> {
        let b: [u8; 4] = self.take(4)?.try_into().ok()?;
        Some(u32::from_le_bytes(b) as usize)
    }

    fn bytes(&mut self) -> Option<&'a [u8]> {
        let n = self.u32()?;
        self.take(n)
    }

    fn list(&mut self) -> Option<Vec<&'a [u8]>> {
        let n = self.u32()?;
        // Число элементов не больше, чем байтов осталось: враждебное `n` не выделяет память.
        if n > self.0.len() / 4 {
            return None;
        }
        (0..n).map(|_| self.bytes()).collect()
    }
}

/// Запрос: доверие и письмо.
pub fn encode_request(trust: &Trust, message: &[u8]) -> Vec<u8> {
    let size = trust.roots.iter().map(|a| a.der.len() + 5).sum::<usize>()
        + trust.intermediates.iter().chain(&trust.disallowed).map(|d| d.len() + 4).sum::<usize>()
        + message.len()
        + 32;
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(trust.roots.len() as u32).to_le_bytes());
    for a in &trust.roots {
        out.push(u8::from(a.corporate));
        put(&mut out, &a.der);
    }
    put_list(&mut out, trust.intermediates.iter().map(Vec::as_slice));
    put_list(&mut out, trust.disallowed.iter().map(Vec::as_slice));
    put(&mut out, message);
    out
}

/// Разбор запроса (в воркере).
pub fn decode_request(bin: &[u8]) -> Option<(Trust, &[u8])> {
    let mut r = Reader(bin);
    if r.take(4)? != MAGIC {
        return None;
    }
    let n = r.u32()?;
    if n > r.0.len() / 5 {
        return None;
    }
    let mut roots = Vec::with_capacity(n);
    for _ in 0..n {
        let corporate = r.take(1)?[0] != 0;
        roots.push(Anchor { der: r.bytes()?.to_vec(), corporate });
    }
    let intermediates = r.list()?.into_iter().map(<[u8]>::to_vec).collect();
    let disallowed = r.list()?.into_iter().map(<[u8]>::to_vec).collect();
    let message = r.bytes()?;
    r.0.is_empty().then_some((Trust { roots, intermediates, disallowed }, message))
}

const OPEN: &[u8; 4] = b"PMO1";

/// Запрос расшифровки (Ф2): ключ содержимого, затем доверие (для подписи внутри) и письмо — как у проверки.
pub fn encode_open(trust: &Trust, message: &[u8], cek: &[u8]) -> zeroize::Zeroizing<Vec<u8>> {
    let mut out = zeroize::Zeroizing::new(OPEN.to_vec());
    put(&mut out, cek);
    out.extend_from_slice(&encode_request(trust, message));
    out
}

/// Разбор запроса расшифровки (в воркере): доверие, письмо, ключ содержимого.
pub fn decode_open(bin: &[u8]) -> Option<(Trust, &[u8], &[u8])> {
    let mut r = Reader(bin.strip_prefix(OPEN)?);
    let cek = r.bytes()?;
    let (trust, message) = decode_request(r.0)?;
    Some((trust, message, cek))
}

/// Запрос подготовки получателей (Ф4): якоря и сертификаты (DER) списком.
pub fn encode_prepare(trust: &Trust, certs: &[Vec<u8>]) -> Vec<u8> {
    let mut list = Vec::new();
    put_list(&mut list, certs.iter().map(Vec::as_slice));
    encode_request(trust, &list)
}

/// Разбор запроса подготовки (в воркере).
pub fn decode_prepare(bin: &[u8]) -> Option<(Trust, Vec<Vec<u8>>)> {
    let (trust, list) = decode_request(bin)?;
    let mut r = Reader(list);
    let certs = r.list()?.into_iter().map(<[u8]>::to_vec).collect();
    r.0.is_empty().then_some((trust, certs))
}

const CRL: &[u8; 4] = b"PMC1";

/// Запрос проверки CRL (S5-12.1д): сертификат, его издатель (оба — из цепочки, которую воркер уже довёл до якоря)
/// и CRL, скачанный ядром.
pub fn encode_crl(cert: &[u8], issuer: &[u8], crl: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(cert.len() + issuer.len() + crl.len() + 16);
    out.extend_from_slice(CRL);
    for part in [cert, issuer, crl] {
        put(&mut out, part);
    }
    out
}

/// Разбор запроса проверки CRL (в воркере): сертификат, издатель, CRL.
pub fn decode_crl(bin: &[u8]) -> Option<(&[u8], &[u8], &[u8])> {
    let mut r = Reader(bin.strip_prefix(CRL)?);
    let (cert, issuer, crl) = (r.bytes()?, r.bytes()?, r.bytes()?);
    r.0.is_empty().then_some((cert, issuer, crl))
}

/// Ответ: DER цепочки и содержимое `signed-data` (пусто — нет).
pub fn encode_response(chain: &[Vec<u8>], content: Option<&[u8]>) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    put_list(&mut out, chain.iter().map(Vec::as_slice));
    out.push(u8::from(content.is_some()));
    put(&mut out, content.unwrap_or_default());
    out
}

/// Ответ воркера: DER цепочки и содержимое `signed-data`.
pub type Response = (Vec<Vec<u8>>, Option<Vec<u8>>);

/// Разбор ответа (в ядре): цепочка и содержимое.
pub fn decode_response(bin: &[u8]) -> Option<Response> {
    let mut r = Reader(bin);
    if r.take(4)? != MAGIC {
        return None;
    }
    let chain = r.list()?.into_iter().map(<[u8]>::to_vec).collect();
    let has = r.take(1)?[0] != 0;
    let content = r.bytes()?;
    r.0.is_empty().then(|| (chain, has.then(|| content.to_vec())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_strictness() {
        let trust = Trust {
            roots: vec![Anchor { der: vec![1, 2, 3], corporate: true }, Anchor { der: vec![4], corporate: false }],
            intermediates: vec![vec![5, 6]],
            disallowed: vec![],
        };
        let bin = encode_request(&trust, b"letter");
        let (t, m) = decode_request(&bin).unwrap();
        assert_eq!((t, m), (trust.clone(), &b"letter"[..]));
        for cut in 0..bin.len() {
            assert!(decode_request(&bin[..cut]).is_none(), "обрезан на {cut}");
        }
        let mut long = bin.clone();
        long.push(0);
        assert!(decode_request(&long).is_none());
        let mut huge = MAGIC.to_vec();
        huge.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode_request(&huge).is_none());

        let resp = encode_response(&[vec![9; 3], vec![8]], Some(b"inner"));
        assert_eq!(decode_response(&resp), Some((vec![vec![9; 3], vec![8]], Some(b"inner".to_vec()))));
        assert_eq!(decode_response(&encode_response(&[], None)), Some((vec![], None)));
        assert!(decode_response(&resp[..resp.len() - 1]).is_none());

        let open = encode_open(&trust, b"letter", &[7; 32]);
        let (t, m, k) = decode_open(&open).unwrap();
        assert_eq!((t, m, k), (trust.clone(), &b"letter"[..], &[7u8; 32][..]));
        for cut in 0..open.len() {
            assert!(decode_open(&open[..cut]).is_none(), "обрезан на {cut}");
        }

        let crl = encode_crl(b"cert", b"ca", b"list");
        assert_eq!(decode_crl(&crl), Some((&b"cert"[..], &b"ca"[..], &b"list"[..])));
        for cut in 0..crl.len() {
            assert!(decode_crl(&crl[..cut]).is_none(), "обрезан на {cut}");
        }
        assert!(decode_crl(&[crl.as_slice(), &[0]].concat()).is_none());
    }
}
