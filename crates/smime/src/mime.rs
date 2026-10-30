//! Где подпись в письме (RFC 8551 §3.5): `multipart/signed` с отдельной подписью или `application/pkcs7-mime`.
//!
//! Разбор MIME (заголовки, границы частей, Content-Transfer-Encoding) — в воркере (`postman_worker::smime`, S8-36):
//! сюда приходят готовые части байтами ([`Parts`]). Здесь — только толкование CMS и канонизация концов строк
//! подписанной части в CRLF (RFC 8551 §3.1.1: подпись считается над каноническим видом).

use std::borrow::Cow;

use crate::{oid, signed};

/// S/MIME в корне письма — как его нашёл разбор MIME в воркере: части байтами, без толкования CMS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parts {
    /// Не S/MIME: другой тип, OpenPGP/MIME, нет `Content-Type`.
    None,
    /// `multipart/signed` (протокол `pkcs7-signature`): первая часть как пришла — от строки-разделителя до CRLF перед
    /// следующей — и подпись (`smime.p7s`) после Content-Transfer-Encoding.
    Detached { content: Vec<u8>, signature: Vec<u8> },
    /// `application/pkcs7-mime`: тело после Content-Transfer-Encoding (`None` — не снялось); `enveloped` —
    /// `smime-type` называет шифрование (`enveloped-data`, `authEnveloped-data`).
    Pkcs7 { cms: Option<Vec<u8>>, enveloped: bool },
}

/// Корень письма: адрес «От» (в нижнем регистре, пусто — нет) и S/MIME-части; `Err` — MIME не разобран.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Letter {
    pub from: String,
    pub parts: Result<Parts, String>,
}

pub(crate) enum Found {
    None,
    /// Шифровано: DER CMS (`None` — тело не снялось).
    Encrypted {
        cms: Option<Vec<u8>>,
    },
    /// Подписанная часть (канонизированная) и DER подписи (`smime.p7s`).
    Detached {
        signed: Vec<u8>,
        signature: Vec<u8>,
    },
    /// DER `signed-data` (`smime.p7m`).
    Opaque {
        cms: Vec<u8>,
    },
}

/// Концы строк — CRLF (одиночный LF → CRLF).
pub(crate) fn canonical(bytes: &[u8]) -> Cow<'_, [u8]> {
    let bare = bytes.iter().enumerate().any(|(i, &b)| b == b'\n' && (i == 0 || bytes[i - 1] != b'\r'));
    if !bare {
        return Cow::Borrowed(bytes);
    }
    let mut out = Vec::with_capacity(bytes.len() + bytes.len() / 32);
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' && (i == 0 || bytes[i - 1] != b'\r') {
            out.push(b'\r');
        }
        out.push(b);
    }
    Cow::Owned(out)
}

/// Что за S/MIME в частях: вид `pkcs7-mime` — по типу содержимого CMS, если `smime-type` не назвал шифрование.
pub(crate) fn find(parts: &Parts) -> Result<Found, String> {
    match parts {
        Parts::None => Ok(Found::None),
        Parts::Detached { content, signature } => Ok(Found::Detached { signed: canonical(content).into_owned(), signature: signature.clone() }),
        Parts::Pkcs7 { cms, enveloped: true } => Ok(Found::Encrypted { cms: cms.clone() }),
        Parts::Pkcs7 { cms: None, enveloped: false } => Err("pkcs7-mime body".into()),
        Parts::Pkcs7 { cms: Some(cms), enveloped: false } => match signed::content_type(cms) {
            Some(oid::ENVELOPED_DATA) | Some(oid::AUTH_ENVELOPED_DATA) => Ok(Found::Encrypted { cms: Some(cms.clone()) }),
            Some(oid::SIGNED_DATA) => Ok(Found::Opaque { cms: cms.clone() }),
            Some(_) => Ok(Found::None),
            None => Err("pkcs7-mime is not CMS".into()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_line_ends() {
        assert_eq!(&*canonical(b"a\r\nb"), b"a\r\nb");
        assert!(matches!(canonical(b"a\r\nb"), Cow::Borrowed(_)));
        assert_eq!(&*canonical(b"a\nb\r\nc\n"), b"a\r\nb\r\nc\r\n");
        assert_eq!(&*canonical(b"\n"), b"\r\n");
    }

    #[test]
    fn parts_are_classified() {
        assert!(matches!(find(&Parts::None), Ok(Found::None)));
        let enc = Parts::Pkcs7 { cms: Some(b"AAAA".to_vec()), enveloped: true };
        assert!(matches!(find(&enc), Ok(Found::Encrypted { cms: Some(_) })));
        assert!(find(&Parts::Pkcs7 { cms: None, enveloped: false }).is_err());
        assert!(find(&Parts::Pkcs7 { cms: Some(b"junk".to_vec()), enveloped: false }).is_err(), "не CMS");
        let d = Parts::Detached { content: b"A\n\nx".to_vec(), signature: b"sig".to_vec() };
        assert!(matches!(find(&d), Ok(Found::Detached { signed, .. }) if signed == b"A\r\n\r\nx"));
    }
}
