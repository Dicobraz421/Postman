//! Проверка подписи S/MIME в настоящей песочнице (S5-12 Ф1, 12.1б): цепочка к переданным якорям, назначение,
//! адрес, пределы размера; GOST — тоже.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)

use std::path::PathBuf;
use std::time::Duration;

use postman_smime::testkit::{self, Eku, Head, Key, NOW, Pki, Sign, Spec};
use postman_smime::{Kind, Status, oid};
use postman_worker::{Client, Request, SmimeFailure};

fn client() -> Client {
    Client::new(PathBuf::from(env!("CARGO_BIN_EXE_postman-worker")), Duration::from_secs(3))
}

fn head(from: &str) -> Head<'_> {
    Head { from, to: "user3@itt.test", subject: "Проверка", date: "Wed, 21 Oct 2026 10:00:00 +0300" }
}

const HTML: &str = "<p>Подписанное письмо.</p>";

#[test]
fn verdicts_through_the_sandbox() {
    let mut c = client();
    let pki = Pki::rsa("Postman Test Root", "Иванов", "user1@itt.test");
    let ok = testkit::signed_letter(&head("user1@itt.test"), HTML, &pki.user, &pki.user_key, &[&pki.root], &Sign::default());
    let (r, doc) = c.smime_verify(&ok, &pki.trust(), NOW).unwrap();
    assert_eq!(r.status, Status::Valid, "{r:?}");
    assert!(r.address_ok && r.corporate);
    assert_eq!(r.chain.len(), 2, "DER цепочки пришёл в бинарной части");
    assert_eq!(r.chain[0], pki.user.der);
    assert!(doc.is_none(), "отдельная подпись — тело показывает сервер");
    if std::env::var_os("POSTMAN_REQUIRE_FULL_SANDBOX").is_some() {
        assert_eq!(c.level(), postman_worker::Level::Full, "проверка — в полной песочнице (AppContainer): {:?}", c.fallback());
    }

    // CA не из якорей.
    let other = Pki::build(Key::p256("o"), Key::p256("p"), "Other EC", "y", "y@itt.test");
    assert!(matches!(c.smime_verify(&ok, &other.trust(), NOW).unwrap().0.status, Status::Untrusted(_)));

    // Просроченный; без EKU (верна); чужой EKU; чужой адрес.
    let issue = |spec: Spec| testkit::issue(&spec, &pki.user_key, Some((&pki.root, &pki.root_key)));
    let letter = |cert: &testkit::Issued, from: &str| testkit::signed_letter(&head(from), HTML, cert, &pki.user_key, &[&pki.root], &Sign::default());
    let expired = issue(Spec { not_after: NOW as i64 - 60, ..Spec::user("u", "user1@itt.test") });
    assert_eq!(c.smime_verify(&letter(&expired, "user1@itt.test"), &pki.trust(), NOW).unwrap().0.status, Status::Expired);
    let any = issue(Spec { eku: Eku::None, ..Spec::user("u", "user1@itt.test") });
    assert_eq!(c.smime_verify(&letter(&any, "user1@itt.test"), &pki.trust(), NOW).unwrap().0.status, Status::Valid);
    let server = issue(Spec { eku: Eku::Server, ..Spec::user("u", "user1@itt.test") });
    assert_eq!(c.smime_verify(&letter(&server, "user1@itt.test"), &pki.trust(), NOW).unwrap().0.status, Status::WrongUsage);
    let (r, _) = c.smime_verify(&letter(&pki.user, "boss@itt.test"), &pki.trust(), NOW).unwrap();
    assert!(r.status == Status::Valid && !r.address_ok);

    // Подпись внутри: тело — IR из воркера.
    let opaque =
        testkit::signed_letter(&head("user1@itt.test"), HTML, &pki.user, &pki.user_key, &[&pki.root], &Sign { opaque: true, ..Sign::default() });
    let (r, doc) = c.smime_verify(&opaque, &pki.trust(), NOW).unwrap();
    assert_eq!((r.kind, r.status), (Kind::Opaque, Status::Valid));
    assert_eq!(postman_ir::to_markdown(&doc.unwrap()), "Подписанное письмо.");

    // Мусор — итог, воркер жив.
    let (r, _) = c.smime_verify(b"\x00\x01garbage", &pki.trust(), NOW).unwrap();
    assert!(!r.status.signature_ok());
    assert!(c.render(&Request::Text("ok".into())).is_ok());
}

#[test]
fn gost_chain_in_the_sandbox() {
    let mut c = client();
    let pki = Pki::build(Key::gost("root", oid::TC26_256_A), Key::gost("user", oid::TC26_512_C), "ГОСТ Корень", "Сидоров", "user2@itt.test");
    let m = testkit::signed_letter(&head("user2@itt.test"), HTML, &pki.user, &pki.user_key, &[&pki.root], &Sign::default());
    let (r, _) = c.smime_verify(&m, &pki.trust(), NOW).unwrap();
    assert_eq!(r.status, Status::Valid, "{r:?}");
    assert!(r.is_gost());
}

/// Письмо 60 МБ — в пределах (проверено), 70 МБ — «слишком большое» до отправки в воркер.
#[test]
fn size_limits() {
    let mut c = client();
    let pki = Pki::build(Key::p256("root"), Key::p256("user"), "Root", "User", "user1@itt.test");
    let big = "x".repeat(44 * 1024 * 1024);
    let html = format!("<p>{big}</p>");
    // base64 — ×4/3: ~59 МБ части.
    let m = testkit::signed_letter(&head("user1@itt.test"), &html, &pki.user, &pki.user_key, &[&pki.root], &Sign::default());
    assert!(m.len() > 58 * 1024 * 1024 && m.len() < postman_smime::MAX_MESSAGE, "{}", m.len());
    let (r, _) = c.smime_verify(&m, &pki.trust(), NOW).unwrap();
    assert_eq!(r.status, Status::Valid);
    let huge = vec![b'a'; 70 * 1024 * 1024];
    assert_eq!(c.smime_verify(&huge, &pki.trust(), NOW).unwrap_err(), SmimeFailure::TooLarge);
}

/// Расшифровка в песочнице (S5-12 Ф2, 12.2в): два прохода (получатели → ключ ядра → тело), зашифрованное и
/// подписанное — два слоя, подпись верна; вложение по номеру; неверный ключ — «повреждено», воркер жив.
#[test]
fn decryption_through_the_sandbox() {
    use postman_smime::envelope::{Cipher, OpenError, Wrap};
    use postman_smime::soft::{self, SoftKey};
    use postman_smime::testkit::{KeyAlg, To};
    let mut c = client();
    let me = Pki::rsa("Postman Test Root", "Смирнова", "user3@itt.test");
    let sender = Pki::rsa("Postman Test Root", "Иванов", "user1@itt.test");
    let inner = testkit::mixed_part(HTML, ("Смета.xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", b"PK\x03\x04 test"));
    let signed = testkit::signed_part(&inner, &sender.user, &sender.user_key, &[&sender.root], &Sign::default());
    let cek: Vec<u8> = (0..32).map(|i| i as u8 ^ 0x5a).collect();
    let to = [To { cert: &me.user, key: &me.user_key, alg: KeyAlg::RsaOaep(postman_smime::Hash::Sha256), ski: false }];
    let cms = testkit::enveloped(&signed, &to, Cipher::AesGcm(256), &cek, false);
    let m = testkit::encrypted_letter(&head("user1@itt.test"), &cms, true);

    let env = c.smime_envelope(&m).unwrap().unwrap();
    assert_eq!(env.cipher, Ok(Cipher::AesGcm(256)));
    assert_eq!(env.recipients[0].wrap, Wrap::RsaOaep(postman_smime::Hash::Sha256));
    let Key::Rsa(k) = &me.user_key else { unreachable!() };
    let key = SoftKey::new(&me.user.der, &soft::pkcs1_der(k).unwrap()).unwrap();
    let got = soft::unwrap(std::slice::from_ref(&key), &env).unwrap();
    let (opened, doc) = c.smime_open(&m, &got, &sender.trust(), NOW).unwrap().unwrap();
    assert_eq!((opened.inner.kind, &opened.inner.status), (Kind::Detached, &Status::Valid));
    assert_eq!(opened.inner.chain.first(), Some(&sender.user.der), "цепочка подписи внутри — в бинарной части");
    assert_eq!(postman_ir::to_markdown(&doc.unwrap()), "Подписанное письмо.");
    assert_eq!(opened.parts.len(), 1);
    assert_eq!(c.smime_part(&m, &got, 0).unwrap().unwrap().as_slice(), b"PK\x03\x04 test");

    let mut wrong = got.to_vec();
    wrong[0] ^= 1;
    assert_eq!(c.smime_open(&m, &wrong, &sender.trust(), NOW).unwrap().unwrap_err(), OpenError::Damaged);
    assert_eq!(c.smime_envelope(b"From: a@x\r\n\r\nhi").unwrap(), Err(OpenError::NotEncrypted));
    assert!(c.render(&Request::Text("ok".into())).is_ok());
}

/// Подготовка получателей в песочнице (S5-12 Ф4): доверенный — ключ значениями, чужой корень — отказ.
#[test]
fn prepare_through_the_sandbox() {
    use postman_smime::prepare::RecipientKey;
    let mut c = client();
    let pki = Pki::rsa("Postman Test Root", "Петров", "p@itt.test");
    let other = Pki::rsa("Other Root", "x", "x@itt.test");
    let r = c.smime_prepare(&[pki.user.der.clone(), other.user.der.clone()], &pki.trust(), NOW).unwrap();
    assert!(matches!(&r[0], Ok(p) if matches!(p.key, RecipientKey::Rsa { .. }) && p.emails == ["p@itt.test"]), "{r:?}");
    assert!(r[1].is_err());
    assert!(c.render(&Request::Text("ok".into())).is_ok());
}

/// Отзыв ГОСТ без СКЗИ (12.1д): CRL тестового УЦ ГОСТ проверяется в песочнице.
#[test]
fn gost_crl_through_the_sandbox() {
    use postman_smime::crl::{CrlError, Verdict};
    let mut c = client();
    let root_key = Key::gost("crl-root", oid::TC26_256_A);
    let root = testkit::issue(&Spec::ca("ГОСТ УЦ"), &root_key, None);
    let user_key = Key::gost("crl-user", oid::TC26_512_C);
    let user = |serial: u64| testkit::issue(&Spec { serial, ..Spec::user("Сидоров", "user2@itt.test") }, &user_key, Some((&root, &root_key)));
    let (revoked, good) = (user(0x51), user(0x52));
    let now = NOW as i64;
    let crl = testkit::crl(&root, &root_key, &[0x51], now - 3600, now + 86_400);
    assert_eq!(c.smime_crl(&revoked.der, &root.der, &crl, NOW).unwrap().unwrap().verdict, Verdict::Revoked { at: now - 3600 });
    assert_eq!(c.smime_crl(&good.der, &root.der, &crl, NOW).unwrap().unwrap().verdict, Verdict::NotRevoked);
    let forged = testkit::crl(&root, &Key::gost("forger", oid::TC26_256_A), &[], now - 3600, now + 86_400);
    assert_eq!(c.smime_crl(&revoked.der, &root.der, &forged, NOW).unwrap(), Err(CrlError::BadSignature));
    assert!(matches!(c.smime_crl(&revoked.der, &root.der, &crl[..crl.len() / 2], NOW).unwrap(), Err(CrlError::Malformed(_))));
    assert!(matches!(c.smime_crl(&revoked.der, &root.der, &vec![0u8; postman_smime::crl::MAX_CRL + 1], NOW), Err(SmimeFailure::TooLarge)));
    assert!(c.render(&Request::Text("ok".into())).is_ok(), "воркер жив");
}
