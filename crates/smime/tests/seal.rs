//! Шифрование исходящих (S5-12 Ф4, 12.4а): круговые тесты со своей расшифровкой (Ф2) для каждого сочетания;
//! выбор шифра; подготовка получателя (срок, назначение, доверие); подписать, затем зашифровать.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // тест: «не падать» — для кода приложения (S2-03)

use postman_smime::envelope::{self, Cipher, Wrap};
use postman_smime::prepare::{self, Prepared, RecipientKey};
use postman_smime::seal::{self, To};
use postman_smime::soft::{self, SoftKey};
use postman_smime::testkit::{Issued, Key, NOW, Pki, Spec, TestOps, TestSigner, issue};
use postman_smime::{Kind, Status};

const RAW: &[u8] = b"From: user1@itt.test\r\nTo: me@itt.test\r\nSubject: Secret\r\nDate: Wed, 21 Oct 2026 10:00:00 +0300\r\nMessage-ID: <s@itt.test>\r\nMIME-Version: 1.0\r\nContent-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\nPHA+0KHQtdC60YDQtdGCPC9wPg==\r\n";
const HTML: &str = "<p>Секрет</p>";

fn prep(pki: &Pki, cert: &Issued) -> Prepared {
    prepare::prepare(&cert.der, &[], &pki.trust(), NOW).unwrap()
}

fn soft_key(pki: &Pki) -> SoftKey {
    let Key::Rsa(k) = &pki.user_key else { unreachable!() };
    SoftKey::new(&pki.user.der, &soft::pkcs1_der(k).unwrap()).unwrap()
}

/// RSA: CBC + PKCS#1 v1.5, GCM + OAEP, если все объявили GCM; своя расшифровка (Ф2) — тот же текст.
#[test]
fn rsa_round_trips() {
    let pki = Pki::rsa("Postman Test Root", "Смирнова", "me@itt.test");
    let p = prep(&pki, &pki.user);
    assert!(matches!(p.key, RecipientKey::Rsa { .. }));
    assert_eq!(p.emails, ["me@itt.test"]);
    for (gcm, want) in [(false, Cipher::AesCbc(256)), (true, Cipher::AesGcm(256))] {
        let to = [To { prepared: p.clone(), gcm }];
        assert_eq!(seal::choose_cipher(&to), want);
        let m = seal::seal_message(RAW, &to, &TestOps::default()).unwrap();
        let text = String::from_utf8(m.clone()).unwrap();
        assert!(text.starts_with("From: user1@itt.test") && text.contains("Subject: Secret"), "заголовки — открыто");
        assert!(text.contains("application/pkcs7-mime"));
        let env = envelope::envelope(&m).unwrap();
        assert_eq!(env.cipher, Ok(want));
        assert_eq!(env.recipients[0].wrap, if gcm { Wrap::RsaOaep(postman_smime::Hash::Sha256) } else { Wrap::RsaPkcs1 });
        let cek = soft::unwrap(&[soft_key(&pki)], &env).unwrap();
        assert_eq!(envelope::open(&m, &cek, &pki.trust(), NOW).unwrap().html.as_deref(), Some(HTML));
    }
    // Хоть один без GCM — CBC.
    let to = [To { prepared: p.clone(), gcm: true }, To { prepared: p, gcm: false }];
    assert_eq!(seal::choose_cipher(&to), Cipher::AesCbc(256));
}

/// ECDH P-256 и P-384: KDF X9.63 и AES-256 KW; получатель снимает обёртку своим ключом.
#[test]
fn ec_round_trips() {
    for key in [Key::p256("seal-ec"), Key::p384("seal-ec384")] {
        let root_key = Key::rsa('a');
        let root = issue(&Spec::ca("Root"), &root_key, None);
        let user = issue(&Spec::user("Смирнова", "me@itt.test"), &key, Some((&root, &root_key)));
        let trust = postman_smime::Trust { roots: vec![postman_smime::Anchor { der: root.der.clone(), corporate: false }], ..Default::default() };
        let p = prepare::prepare(&user.der, &[], &trust, NOW).unwrap();
        let to = [To { prepared: p, gcm: true }];
        assert_eq!(seal::choose_cipher(&to), Cipher::AesCbc(256), "EC — CBC");
        let m = seal::seal_message(RAW, &to, &TestOps::default()).unwrap();
        let env = envelope::envelope(&m).unwrap();
        let r = &env.recipients[0];
        let Wrap::Ecdh { ephemeral, .. } = &r.wrap else { panic!("{:?}", r.wrap) };
        let z = match &key {
            Key::P256(sk) => p256::ecdh::diffie_hellman(*sk.as_nonzero_scalar(), p256::PublicKey::from_sec1_bytes(ephemeral).unwrap().as_affine())
                .raw_secret_bytes()
                .to_vec(),
            Key::P384(sk) => p384::ecdh::diffie_hellman(*sk.as_nonzero_scalar(), p384::PublicKey::from_sec1_bytes(ephemeral).unwrap().as_affine())
                .raw_secret_bytes()
                .to_vec(),
            _ => unreachable!(),
        };
        let cek = envelope::ecdh_cek(&z, &r.wrap, &r.encrypted_key).unwrap();
        assert_eq!(envelope::open(&m, &cek, &trust, NOW).unwrap().html.as_deref(), Some(HTML));
    }
}

/// Подписать, затем зашифровать (себе и получателю): у получателя — два слоя, подпись верна; себе — тоже читается.
#[test]
fn sign_then_encrypt_to_self_and_recipient() {
    let me = Pki::rsa("Postman Test Root", "Иванов", "user1@itt.test");
    let them = Pki::build(Key::rsa('a'), Key::rsa('c'), "Postman Test Root (them)", "Петров", "me@itt.test");
    let signed = postman_smime::sign::sign_message(RAW, &TestSigner { cert: &me.user, key: &me.user_key, chain: Vec::new() }, NOW as i64).unwrap();
    let to = [To { prepared: prep(&them, &them.user), gcm: false }, To { prepared: prep(&me, &me.user), gcm: false }];
    let m = seal::seal_message(&signed, &to, &TestOps::default()).unwrap();
    for pki in [&them, &me] {
        let env = envelope::envelope(&m).unwrap();
        let cek = soft::unwrap(&[soft_key(pki)], &env).unwrap();
        let o = envelope::open(&m, &cek, &me.trust(), NOW).unwrap();
        assert_eq!((o.inner.kind, &o.inner.status), (Kind::Detached, &Status::Valid), "{:?}", o.inner);
        assert_eq!(o.html.as_deref(), Some(HTML));
    }
}

/// Подготовка: просроченный, не доверенный и CA вместо пользователя — не берутся; выбор — предпочтение, затем срок.
#[test]
fn prepare_and_choose() {
    let pki = Pki::rsa("Postman Test Root", "Смирнова", "me@itt.test");
    let expired = issue(&Spec { not_after: NOW as i64 - 10, ..Spec::user("u", "me@itt.test") }, &pki.user_key, Some((&pki.root, &pki.root_key)));
    assert!(prepare::prepare(&expired.der, &[], &pki.trust(), NOW).is_err());
    let other = Pki::rsa("Other Root", "x", "me@itt.test");
    assert!(prepare::prepare(&pki.user.der, &[], &other.trust(), NOW).is_err(), "корень не доверен");
    assert!(prepare::prepare(&pki.root.der, &[], &pki.trust(), NOW).is_err(), "CA — не получатель");
    let later = issue(
        &Spec { serial: 0x99, not_after: postman_smime::testkit::Y2046 + 86_400, ..Spec::user("u", "me@itt.test") },
        &pki.user_key,
        Some((&pki.root, &pki.root_key)),
    );
    let a = prep(&pki, &pki.user);
    let b = prep(&pki, &later);
    let list = [a.clone(), b.clone()];
    assert_eq!(prepare::choose(&list, None), Some(&b), "позже срок");
    assert_eq!(prepare::choose(&list, Some(&a.fingerprint)), Some(&a), "предпочтение важнее");
}

/// ГОСТ-получатель шифрует только СКЗИ: в обычное письмо не берётся (письмо делится).
#[test]
fn gost_recipient_is_not_sealed_here() {
    let pki = Pki::build(
        Key::gost("r", postman_smime::oid::TC26_256_A),
        Key::gost("u", postman_smime::oid::TC26_256_A),
        "ГОСТ УЦ",
        "Сидоров",
        "s@itt.test",
    );
    let p = prep(&pki, &pki.user);
    assert!(p.key.is_gost());
    assert_eq!(seal::seal_message(RAW, &[To { prepared: p, gcm: false }], &TestOps::default()), Err(seal::SealError::Gost));
    assert_eq!(seal::seal_message(RAW, &[], &TestOps::default()), Err(seal::SealError::NoRecipients));
}
