//! Подпись исходящих (S5-12 Ф3, 12.3а): своя подпись проверяется своей проверкой (Ф1); подписанная часть — 7 бит,
//! заголовки письма снаружи; письмо с вложением и календарём (iMIP) подписывается целиком.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // тест: «не падать» — для кода приложения (S2-03)

use postman_smime::sign::{self, SignError};
use postman_smime::testkit::{self, Key, NOW, Pki, TestSigner};
use postman_smime::{Kind, Status, oid};

fn letter(body: &str) -> Vec<u8> {
    format!(
        "From: =?utf-8?B?0JjQstCw0L3QvtCy?= <user1@itt.test>
To: me@itt.test
Subject: Test
Date: Wed, 21 Oct 2026 10:00:00 +0300
Message-ID: <x@itt.test>
MIME-Version: 1.0
{body}"
    )
    .into_bytes()
}

const MIXED: &str = "Content-Type: multipart/mixed;
 boundary=\"b1\"

--b1
Content-Type: text/plain; charset=utf-8
Content-Transfer-Encoding: quoted-printable

=D0=9F=D1=80=D0=B8=D0=B2=D0=B5=D1=82
--b1
Content-Type: text/calendar; method=REQUEST; charset=utf-8
Content-Transfer-Encoding: base64

QkVHSU46VkNBTEVOREFS
--b1
Content-Type: application/pdf; name=a.pdf
Content-Transfer-Encoding: base64
Content-Disposition: attachment; filename=a.pdf

JVBERi0xLjQ=
--b1--
";

/// RSA, ECDSA P-256 и ГОСТ: подписано своим подписантом — своя проверка «верна», адрес совпал, 7 бит.
#[test]
fn own_signature_verifies() {
    for (user_key, name) in [(Key::rsa('b'), "rsa"), (Key::p256("sign-ec"), "ec"), (Key::gost("sign-gost", oid::TC26_256_A), "gost")] {
        let pki = Pki::build(Key::rsa('a'), user_key, "Postman Test Root", "Иванов", "user1@itt.test");
        let signer = TestSigner { cert: &pki.user, key: &pki.user_key, chain: Vec::new() };
        let signed = sign::sign_message(&letter(MIXED), &signer, NOW as i64 - 60).unwrap();
        assert!(signed.iter().all(|&b| b < 0x80), "{name}: 7 бит");
        let r = postman_worker::smime::verify(&signed, &pki.trust(), NOW);
        assert_eq!((r.kind, &r.status), (Kind::Detached, &Status::Valid), "{name}: {r:?}");
        assert!(r.address_ok, "{name}");
        assert_eq!(r.signing_time, Some(NOW as i64 - 60));
        assert!(!r.capabilities.is_empty());
        let text = String::from_utf8(signed).unwrap();
        assert!(text.starts_with("From: "), "заголовки письма — снаружи");
        assert!(text.contains("text/calendar") && text.contains("application/pdf"), "подписано целиком");
        assert_eq!(text.matches("MIME-Version").count(), 1);
    }
}

/// 8-битная часть не подписывается; изменённое после подписи письмо — «недействительна».
#[test]
fn eight_bit_and_tamper() {
    let pki = Pki::rsa("Postman Test Root", "Иванов", "user1@itt.test");
    let signer = TestSigner { cert: &pki.user, key: &pki.user_key, chain: Vec::new() };
    let eight = letter(
        "Content-Type: text/plain; charset=utf-8
Content-Transfer-Encoding: 8bit

Привет
",
    );
    assert_eq!(sign::sign_message(&eight, &signer, NOW as i64), Err(SignError::EightBit));
    let signed = sign::sign_message(
        &letter(
            "Content-Type: text/plain

Pay 100
",
        ),
        &signer,
        NOW as i64,
    )
    .unwrap();
    let text = String::from_utf8(signed).unwrap().replace("Pay 100", "Pay 900");
    assert_eq!(postman_worker::smime::verify(text.as_bytes(), &pki.trust(), NOW).status, Status::Invalid);
}

/// `r‖s` (CNG) → DER `ECDSA-Sig-Value`, с нулём знака и без ведущих нулей.
#[test]
fn ecdsa_der() {
    assert_eq!(sign::ecdsa_der(&[0x80, 0x01, 0x00, 0x7f]).unwrap(), vec![0x30, 0x08, 0x02, 0x03, 0x00, 0x80, 0x01, 0x02, 0x01, 0x7f]);
    assert!(sign::ecdsa_der(&[1, 2, 3]).is_none());
    let _ = testkit::NOW;
}
