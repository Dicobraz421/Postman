//! Проверка подписи S/MIME (S5-12 Ф1, 12.1а): свои образцы (`testkit`), образцы OpenSSL (независимая реализация) и
//! враждебный ввод.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)

use postman_smime::testkit::{self, Eku, Head, Key, NOW, Pki, Sign, Spec};
use postman_smime::{Anchor, Hash, Kind, Scheme, Status, Trust, oid};
use postman_worker::smime::verify;

const HTML: &str = "<p>Договор согласован, подписанная версия — во вложении.</p>";

fn head(from: &str) -> Head<'_> {
    Head { from, to: "user3@itt.test", subject: "Договор", date: "Wed, 21 Oct 2026 10:00:00 +0300" }
}

fn letter(pki: &Pki, opts: &Sign) -> Vec<u8> {
    testkit::signed_letter(&head("Иванов <user1@itt.test>"), HTML, &pki.user, &pki.user_key, &[&pki.root], opts)
}

fn rsa() -> Pki {
    Pki::rsa("Postman Test Root", "Иванов Иван", "user1@itt.test")
}

#[test]
fn valid_rsa_signature_is_trusted() {
    let pki = rsa();
    let r = verify(&letter(&pki, &Sign::default()), &pki.trust(), NOW);
    assert_eq!(r.status, Status::Valid, "{r:?}");
    assert_eq!(r.kind, Kind::Detached);
    assert_eq!(r.scheme, Some(Scheme::RsaPkcs1(Hash::Sha256)));
    assert!(r.address_ok && r.corporate && !r.weak);
    assert_eq!(r.from, "user1@itt.test");
    assert_eq!(r.signing_time, Some(NOW as i64 - 3600));
    assert_eq!(r.certs.len(), 2, "подписант и корень");
    assert_eq!(r.certs[0].subject, "Иванов Иван");
    assert_eq!(r.certs[0].emails, ["user1@itt.test"]);
    assert_eq!(r.certs[0].issuer, "Postman Test Root");
    assert_eq!(r.chain.len(), 2);
    assert_eq!(r.capabilities, [oid::AES256_GCM.to_string(), oid::AES256_CBC.to_string(), oid::AES128_CBC.to_string()]);
}

#[test]
fn changed_content_is_invalid() {
    let pki = rsa();
    let mut m = letter(&pki, &Sign::default());
    // Байт в base64 подписанной HTML-части (после заголовков части).
    let at = m.windows(8).position(|w| w == b"base64\r\n").unwrap() + 12;
    m[at] = if m[at] == b'A' { b'B' } else { b'A' };
    let r = verify(&m, &pki.trust(), NOW);
    assert_eq!(r.status, Status::Invalid, "{r:?}");
    assert!(r.chain.is_empty(), "недействительная подпись — без цепочки для Windows");
    assert_eq!(r.certs.len(), 1, "кто назвался подписантом — в карточке");
}

#[test]
fn signature_by_another_key_is_invalid() {
    let pki = rsa();
    // Сертификат пользователя, подпись — чужим ключом.
    let other = Key::rsa('c');
    let m = testkit::signed_letter(&head("user1@itt.test"), HTML, &pki.user, &other, &[&pki.root], &Sign::default());
    assert_eq!(verify(&m, &pki.trust(), NOW).status, Status::Invalid);
}

#[test]
fn sha1_is_weak_not_invalid() {
    let pki = rsa();
    let r = verify(&letter(&pki, &Sign { scheme: Some(Scheme::RsaPkcs1(Hash::Sha1)), ..Sign::default() }), &pki.trust(), NOW);
    assert_eq!(r.status, Status::Valid);
    assert!(r.weak);
}

#[test]
fn unknown_digest_is_unsupported() {
    let pki = rsa();
    // OID SHA-256 (…04 02 01) в SignedData → SHA-224 (…04 02 04): хэш, которого нет в списке.
    let p7s = testkit::signed_data(&testkit::html_part(HTML), &pki.user, &pki.user_key, &[], &Sign::default());
    let mut patched = p7s.clone();
    let sha256 = [0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01];
    for i in 0..patched.len() - sha256.len() {
        if patched[i..i + sha256.len()] == sha256 {
            patched[i + sha256.len() - 1] = 0x04;
        }
    }
    let m = testkit::multipart_signed(&head("user1@itt.test"), &testkit::html_part(HTML), &patched, "sha-224");
    let r = verify(&m, &pki.trust(), NOW);
    assert_eq!(r.status, Status::Unsupported("2.16.840.1.101.3.4.2.4".into()), "{r:?}");
}

#[test]
fn opaque_and_detached() {
    let pki = rsa();
    let r = verify(&letter(&pki, &Sign { opaque: true, ..Sign::default() }), &pki.trust(), NOW);
    assert_eq!((r.kind, &r.status), (Kind::Opaque, &Status::Valid), "{r:?}");
    let inner = r.content.unwrap();
    assert!(inner.starts_with(b"Content-Type: text/html"));
    let r = verify(&letter(&pki, &Sign { by_ski: true, ..Sign::default() }), &pki.trust(), NOW);
    assert_eq!(r.status, Status::Valid, "sid — SubjectKeyIdentifier");
    let r = verify(&letter(&pki, &Sign { attrs: false, ..Sign::default() }), &pki.trust(), NOW);
    assert_eq!(r.status, Status::Valid, "без подписанных атрибутов");
    assert_eq!(r.signing_time, None);
}

#[test]
fn lf_only_letter_is_canonicalized() {
    let pki = rsa();
    let m = letter(&pki, &Sign::default());
    let lf: Vec<u8> = m.iter().copied().filter(|&b| b != b'\r').collect();
    assert_eq!(verify(&lf, &pki.trust(), NOW).status, Status::Valid);
}

#[test]
fn untrusted_expired_usage_address() {
    let pki = rsa();
    let m = letter(&pki, &Sign::default());
    // CA не из якорей.
    let other = Pki::build(Key::p256("other-root"), Key::p256("x"), "Other Root", "x", "x@itt.test");
    let r = verify(&m, &other.trust(), NOW);
    assert!(matches!(r.status, Status::Untrusted(_)), "{r:?}");
    assert!(!r.corporate);
    // Просроченный сертификат подписанта.
    let expired = testkit::issue(
        &Spec { not_after: NOW as i64 - 86_400, ..Spec::user("Иванов", "user1@itt.test") },
        &pki.user_key,
        Some((&pki.root, &pki.root_key)),
    );
    let m2 = testkit::signed_letter(&head("user1@itt.test"), HTML, &expired, &pki.user_key, &[&pki.root], &Sign::default());
    assert_eq!(verify(&m2, &pki.trust(), NOW).status, Status::Expired);
    // Только «Проверка подлинности сервера» и только шифрование.
    for spec in [Spec { eku: Eku::Server, ..Spec::user("u", "user1@itt.test") }, Spec { encrypt_only: true, ..Spec::user("u", "user1@itt.test") }] {
        let c = testkit::issue(&spec, &pki.user_key, Some((&pki.root, &pki.root_key)));
        let m = testkit::signed_letter(&head("user1@itt.test"), HTML, &c, &pki.user_key, &[&pki.root], &Sign::default());
        assert_eq!(verify(&m, &pki.trust(), NOW).status, Status::WrongUsage);
    }
    // Без EKU — любое назначение (RFC 5280).
    let any = testkit::issue(&Spec { eku: Eku::None, ..Spec::user("u", "user1@itt.test") }, &pki.user_key, Some((&pki.root, &pki.root_key)));
    let m3 = testkit::signed_letter(&head("user1@itt.test"), HTML, &any, &pki.user_key, &[&pki.root], &Sign::default());
    assert_eq!(verify(&m3, &pki.trust(), NOW).status, Status::Valid);
    // Чужой адрес: подпись верна и доверена, но «От» другой.
    let m4 = testkit::signed_letter(&head("boss@itt.test"), HTML, &pki.user, &pki.user_key, &[&pki.root], &Sign::default());
    let r = verify(&m4, &pki.trust(), NOW);
    assert_eq!(r.status, Status::Valid);
    assert!(!r.address_ok);
}

#[test]
fn disallowed_and_missing_certificate() {
    let pki = rsa();
    let m = letter(&pki, &Sign::default());
    let mut trust = pki.trust();
    trust.disallowed.push(pki.user.der.clone());
    assert_eq!(verify(&m, &trust, NOW).status, Status::Distrusted);
    let m = letter(&pki, &Sign { omit_signer: true, ..Sign::default() });
    assert_eq!(verify(&m, &pki.trust(), NOW).status, Status::NoCertificate);
}

#[test]
fn intermediate_from_the_store() {
    let root_key = Key::rsa('a');
    let ca_key = Key::p384("intermediate");
    let user_key = Key::p256("user");
    let root = testkit::issue(&Spec::ca("Root"), &root_key, None);
    let ca = testkit::issue(&Spec { serial: 2, ..Spec::ca("Issuing CA") }, &ca_key, Some((&root, &root_key)));
    let user = testkit::issue(&Spec::user("Петров", "user2@itt.test"), &user_key, Some((&ca, &ca_key)));
    let m = testkit::signed_letter(&head("user2@itt.test"), HTML, &user, &user_key, &[], &Sign::default());
    let trust = Trust { roots: vec![Anchor { der: root.der.clone(), corporate: false }], ..Default::default() };
    assert!(matches!(verify(&m, &trust, NOW).status, Status::Untrusted(_)), "промежуточного нет ни в письме, ни в хранилище");
    let trust = Trust { intermediates: vec![ca.der.clone()], ..trust };
    let r = verify(&m, &trust, NOW);
    assert_eq!(r.status, Status::Valid, "{r:?}");
    assert_eq!(r.certs.iter().map(|c| c.subject.as_str()).collect::<Vec<_>>(), ["Петров", "Issuing CA", "Root"]);
    assert_eq!(r.scheme, Some(Scheme::Ecdsa(Hash::Sha256)));
}

#[test]
fn ecdsa_p256_and_p384() {
    for (root_key, user_key, scheme) in [
        (Key::p256("r1"), Key::p256("u1"), Scheme::Ecdsa(Hash::Sha256)),
        (Key::p384("r2"), Key::p384("u2"), Scheme::Ecdsa(Hash::Sha384)),
        (Key::p256("r3"), Key::p384("u3"), Scheme::Ecdsa(Hash::Sha512)),
    ] {
        let pki = Pki::build(root_key, user_key, "EC Root", "EC User", "user1@itt.test");
        let r = verify(&letter(&pki, &Sign { scheme: Some(scheme), ..Sign::default() }), &pki.trust(), NOW);
        assert_eq!((r.status, r.scheme), (Status::Valid, Some(scheme)));
    }
}

/// ГОСТ Р 34.10-2012: 256 бит (ТК 26 A — скрученная кривая Эдвардса, B = КриптоПро A, D) и 512 бит (A, C), подпись
/// тестовым подписантом, цепочка — ГОСТ-корень.
#[test]
fn gost_letters_verify() {
    for set in [oid::TC26_256_A, oid::TC26_256_B, oid::CRYPTOPRO_A, oid::TC26_256_D, oid::TC26_512_A, oid::TC26_512_B, oid::TC26_512_C] {
        let pki = Pki::build(Key::gost("root", set), Key::gost("user", set), "ГОСТ Корень", "Сидоров", "user1@itt.test");
        let r = verify(&letter(&pki, &Sign::default()), &pki.trust(), NOW);
        assert_eq!(r.status, Status::Valid, "{set}: {r:?}");
        assert!(r.is_gost());
        assert!(matches!(r.certs[0].key, postman_smime::KeyKind::Gost { .. }));
        // Подпись «с хэшем» (id-tc26-signwithdigest) вместо OID ключа — тоже.
        let r = verify(&letter(&pki, &Sign { key_oid: false, ..Sign::default() }), &pki.trust(), NOW);
        assert_eq!(r.status, Status::Valid, "{set}");
        let mut m = letter(&pki, &Sign::default());
        let at = m.windows(8).position(|w| w == b"base64\r\n").unwrap() + 12;
        m[at] ^= 0x01;
        assert_eq!(verify(&m, &pki.trust(), NOW).status, Status::Invalid, "{set}");
    }
    // Корень RSA, пользователь ГОСТ — смешанная цепочка.
    let pki = Pki::build(Key::rsa('a'), Key::gost("u", oid::TC26_256_A), "Root", "Mixed", "user1@itt.test");
    assert_eq!(verify(&letter(&pki, &Sign::default()), &pki.trust(), NOW).status, Status::Valid);
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/fixtures/openssl/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn openssl_trust() -> Trust {
    let ca = testkit::pem_blocks(std::str::from_utf8(&fixture("ca.crt")).unwrap()).remove(0);
    Trust { roots: vec![Anchor { der: ca, corporate: false }], ..Default::default() }
}

/// Письма, подписанные OpenSSL (`tests/fixtures/openssl/gen.sh`): RSA PKCS#1 SHA-256/SHA-1, RSA-PSS, ECDSA P-256
/// с SHA-384, подпись внутри (SHA-512), sid по SKI и CRLF.
#[test]
fn openssl_letters_verify() {
    let trust = openssl_trust();
    let cases = [
        ("rsa-sha256.eml", Scheme::RsaPkcs1(Hash::Sha256), Kind::Detached, false),
        ("rsa-pss.eml", Scheme::RsaPss { hash: Hash::Sha256, salt: 32 }, Kind::Detached, false),
        ("rsa-sha1.eml", Scheme::RsaPkcs1(Hash::Sha1), Kind::Detached, true),
        ("ec-p256.eml", Scheme::Ecdsa(Hash::Sha384), Kind::Detached, false),
        ("rsa-opaque.eml", Scheme::RsaPkcs1(Hash::Sha512), Kind::Opaque, false),
        ("rsa-ski.eml", Scheme::RsaPkcs1(Hash::Sha256), Kind::Detached, false),
    ];
    for (name, scheme, kind, weak) in cases {
        let r = verify(&fixture(name), &trust, NOW);
        assert_eq!(r.status, Status::Valid, "{name}: {r:?}");
        assert_eq!((r.scheme, r.kind, r.weak, r.address_ok), (Some(scheme), kind, weak, true), "{name}");
    }
}

/// DER подписи, обрезанный на каждом байте, и мусор — итог без паники.
#[test]
fn truncated_and_random_input() {
    let pki = rsa();
    let p7s = testkit::signed_data(&testkit::html_part(HTML), &pki.user, &pki.user_key, &[&pki.root], &Sign::default());
    let inner = testkit::html_part(HTML);
    for cut in 0..p7s.len() {
        let m = testkit::multipart_signed(&head("user1@itt.test"), &inner, &p7s[..cut], "sha-256");
        let r = verify(&m, &pki.trust(), NOW);
        assert!(!matches!(r.status, Status::Valid), "обрезано на {cut}: {r:?}");
    }
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    for n in [0usize, 1, 2, 10, 100, 1000, 5000] {
        let junk: Vec<u8> = (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        let m = testkit::multipart_signed(&head("user1@itt.test"), &inner, &junk, "sha-256");
        assert!(!matches!(verify(&m, &pki.trust(), NOW).status, Status::Valid));
        assert!(!matches!(verify(&junk, &pki.trust(), NOW).status, Status::Valid));
    }
    assert_eq!(verify(b"From: a@b\r\n\r\nplain", &pki.trust(), NOW).status, Status::NotSigned);
}
