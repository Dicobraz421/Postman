//! Вход HTTP без SSPI: разбор `WWW-Authenticate`, выбор схемы, заголовок `Authorization`, привязка к каналу TLS
//! (`tls-server-end-point`, RFC 5929) — её требует Extended Protection у Exchange 2019 CU14+ и SE.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use sha2::Digest as _;

/// Схема входа. Basic не поддерживается вовсе.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scheme {
    /// SPNEGO: Kerberos, если есть домен и SPN, иначе NTLM внутри.
    Negotiate,
    Ntlm,
}

impl Scheme {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Scheme::Negotiate => "Negotiate",
            Scheme::Ntlm => "NTLM",
        }
    }
}

/// Вызовы из всех заголовков `WWW-Authenticate`: схема и токен (если есть). Значения через запятую — тоже
/// (так их склеивают прокси).
pub(crate) fn challenges(values: &[String]) -> Vec<(String, Option<Vec<u8>>)> {
    values
        .iter()
        .flat_map(|v| v.split(','))
        .filter_map(|part| {
            let part = part.trim();
            if part.is_empty() {
                return None;
            }
            let (scheme, rest) = part.split_once(char::is_whitespace).unwrap_or((part, ""));
            let token = rest.trim();
            // Токен — base64 (`=` только в конце); параметры вида `realm="…"` (Basic, Digest) — не токен.
            let is_token = !token.is_empty() && token.trim_end_matches('=').bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/');
            let token = if is_token { B64.decode(token).ok() } else { None };
            Some((scheme.to_owned(), token))
        })
        .collect()
}

/// Схема из предложенных сервером: Negotiate, иначе NTLM.
pub(crate) fn choose(values: &[String]) -> Option<Scheme> {
    let offered = challenges(values);
    let has = |s: &str| offered.iter().any(|(n, _)| n.eq_ignore_ascii_case(s));
    if has("Negotiate") {
        Some(Scheme::Negotiate)
    } else if has("NTLM") {
        Some(Scheme::Ntlm)
    } else {
        None
    }
}

/// Токен сервера для этой схемы.
pub(crate) fn token(values: &[String], scheme: Scheme) -> Option<Vec<u8>> {
    challenges(values).into_iter().find(|(n, _)| n.eq_ignore_ascii_case(scheme.name())).and_then(|(_, t)| t)
}

pub(crate) fn header(scheme: Scheme, token: &[u8]) -> String {
    format!("{} {}", scheme.name(), B64.encode(token))
}

/// Данные приложения для привязки к каналу: `tls-server-end-point:` + хэш сертификата сервера. Хэш — алгоритм
/// подписи сертификата; MD5 и SHA-1 заменяются на SHA-256 (RFC 5929 §4.1).
pub(crate) fn channel_binding(cert_der: &[u8]) -> Vec<u8> {
    let mut out = b"tls-server-end-point:".to_vec();
    match signature_hash(cert_der) {
        Hash::Sha384 => out.extend_from_slice(&sha2::Sha384::digest(cert_der)),
        Hash::Sha512 => out.extend_from_slice(&sha2::Sha512::digest(cert_der)),
        Hash::Sha256 => out.extend_from_slice(&sha2::Sha256::digest(cert_der)),
    }
    out
}

/// Структура `SEC_CHANNEL_BINDINGS` с данными приложения сразу за ней (8 × u32, смещение данных — 32).
pub(crate) fn sec_channel_bindings(app: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; 32];
    out[24..28].copy_from_slice(&(app.len() as u32).to_le_bytes()); // cbApplicationDataLength
    out[28..32].copy_from_slice(&32u32.to_le_bytes()); // dwApplicationDataOffset
    out.extend_from_slice(app);
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hash {
    Sha256,
    Sha384,
    Sha512,
}

// OID в DER (без тега и длины).
const SHA384_RSA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0C];
const SHA512_RSA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0D];
const RSA_PSS: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0A];
const ECDSA_SHA384: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x03];
const ECDSA_SHA512: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x04];
const SHA384: &[u8] = &[0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x02];
const SHA512: &[u8] = &[0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x03];

/// Один элемент DER: тег, содержимое, остаток.
fn tlv(b: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = b.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (first as usize, rest)
    } else {
        let n = (first & 0x7F) as usize;
        if n == 0 || n > 4 || rest.len() < n {
            return None;
        }
        (rest[..n].iter().fold(0usize, |acc, b| acc << 8 | *b as usize), &rest[n..])
    };
    (rest.len() >= len).then(|| (tag, &rest[..len], &rest[len..]))
}

/// Алгоритм хэша подписи сертификата: `Certificate ::= SEQUENCE { tbsCertificate, signatureAlgorithm, … }`.
fn signature_hash(cert: &[u8]) -> Hash {
    let alg = (|| {
        let (_, body, _) = tlv(cert)?;
        let (_, _, rest) = tlv(body)?; // tbsCertificate
        let (tag, alg, _) = tlv(rest)?;
        (tag == 0x30).then_some(alg)
    })();
    let Some(alg) = alg else { return Hash::Sha256 };
    let Some((0x06, oid, params)) = tlv(alg) else { return Hash::Sha256 };
    match oid {
        o if o == SHA384_RSA || o == ECDSA_SHA384 => Hash::Sha384,
        o if o == SHA512_RSA || o == ECDSA_SHA512 => Hash::Sha512,
        // RSASSA-PSS: хэш — в параметрах.
        o if o == RSA_PSS && params.windows(SHA384.len()).any(|w| w == SHA384) => Hash::Sha384,
        o if o == RSA_PSS && params.windows(SHA512.len()).any(|w| w == SHA512) => Hash::Sha512,
        _ => Hash::Sha256,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_headers() {
        let v = vec!["Negotiate".to_owned(), "NTLM".to_owned(), "Basic realm=\"mail\"".to_owned()];
        assert_eq!(choose(&v), Some(Scheme::Negotiate));
        assert_eq!(choose(&["NTLM, Basic realm=\"x\"".to_owned()]), Some(Scheme::Ntlm));
        assert_eq!(choose(&["Basic realm=\"x\"".to_owned()]), None);
        let with_token = vec![format!("Negotiate {}", B64.encode(b"srv-token=="))];
        assert_eq!(token(&with_token, Scheme::Negotiate).as_deref(), Some(&b"srv-token=="[..]));
        assert_eq!(token(&with_token, Scheme::Ntlm), None);
        assert_eq!(header(Scheme::Ntlm, b"abc"), "NTLM YWJj");
    }

    fn cert_with(oid: &[u8], params: &[u8]) -> Vec<u8> {
        // SEQUENCE { SEQUENCE {} (tbs), SEQUENCE { OID, params }, BIT STRING {} }
        let mut alg = vec![0x06, oid.len() as u8];
        alg.extend_from_slice(oid);
        alg.extend_from_slice(params);
        let mut body = vec![0x30, 0x00, 0x30, alg.len() as u8];
        body.extend_from_slice(&alg);
        body.extend_from_slice(&[0x03, 0x01, 0x00]);
        let mut out = vec![0x30, body.len() as u8];
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn hash_by_signature_algorithm() {
        assert_eq!(signature_hash(&cert_with(SHA384_RSA, &[0x05, 0x00])), Hash::Sha384);
        assert_eq!(signature_hash(&cert_with(ECDSA_SHA512, &[])), Hash::Sha512);
        assert_eq!(signature_hash(&cert_with(&[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0B], &[0x05, 0x00])), Hash::Sha256);
        let mut pss = vec![0x30, SHA384.len() as u8 + 2, 0xA0, SHA384.len() as u8];
        pss.extend_from_slice(SHA384);
        assert_eq!(signature_hash(&cert_with(RSA_PSS, &pss)), Hash::Sha384);
        assert_eq!(signature_hash(b"junk"), Hash::Sha256);
        let cb = channel_binding(&cert_with(SHA384_RSA, &[]));
        assert_eq!(cb.len(), "tls-server-end-point:".len() + 48);
    }

    #[test]
    fn bindings_layout() {
        let b = sec_channel_bindings(b"abc");
        assert_eq!(b.len(), 35);
        assert_eq!(&b[24..32], &[3, 0, 0, 0, 32, 0, 0, 0]);
        assert_eq!(&b[32..], b"abc");
    }
}
