//! Расшифровка S/MIME (S5-12 Ф2, 12.2а): свои зашифрованные образцы — RSA PKCS#1 v1.5 и OAEP, ECDH P-256/P-384,
//! AES-CBC, AES-GCM, 3DES; ключ снимает `soft` (как демо), ECDH — тест сам (общий секрет из своего ключа).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // тест: «не падать» — для кода приложения (S2-03)

use postman_smime::envelope::{self, Cipher, OpenError, Rid, Wrap};
use postman_smime::soft::{self, SoftError, SoftKey};
use postman_smime::testkit::{self, Head, Key, KeyAlg, NOW, Pki, Sign, Spec, To, issue};
use postman_smime::{Hash, Kind, Status};

const HTML: &str = "<p>Секретный договор</p>";

fn head() -> Head<'static> {
    Head {
        from: "Иванов <user1@itt.test>", to: "me@itt.test", subject: "Конфиденциально", date: "Wed, 21 Oct 2026 10:00:00 +0300"
    }
}

fn rsa_key(which: char) -> Vec<u8> {
    let Key::Rsa(k) = Key::rsa(which) else { unreachable!() };
    soft::pkcs1_der(&k).unwrap()
}

fn me() -> (Pki, SoftKey) {
    let pki = Pki::rsa("Root", "Смирнова", "me@itt.test");
    let key = SoftKey::new(&pki.user.der, &rsa_key('b')).unwrap();
    (pki, key)
}

fn letter(cms: &[u8], cipher: Cipher) -> Vec<u8> {
    testkit::encrypted_letter(&head(), cms, matches!(cipher, Cipher::AesGcm(_)))
}

fn cek(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 13 + 7) as u8).collect()
}

/// Каждый шифр и способ снятия ключа: получатели видны ядру, ключ снимается, тело — то же.
#[test]
fn rsa_recipients_and_ciphers() {
    let (pki, key) = me();
    let cases = [
        (KeyAlg::RsaPkcs1, Cipher::AesCbc(256), false),
        (KeyAlg::RsaPkcs1, Cipher::AesCbc(128), true),
        (KeyAlg::RsaOaep(Hash::Sha1), Cipher::AesCbc(192), false),
        (KeyAlg::RsaOaep(Hash::Sha256), Cipher::AesGcm(256), false),
        (KeyAlg::RsaOaep(Hash::Sha512), Cipher::AesGcm(128), true),
        (KeyAlg::RsaPkcs1, Cipher::Des3, false),
    ];
    for (alg, cipher, ski) in cases {
        let k = cek(cipher.key_len());
        let cms = testkit::enveloped(&testkit::html_part(HTML), &[To { cert: &pki.user, key: &pki.user_key, alg, ski }], cipher, &k, false);
        let m = letter(&cms, cipher);
        let env = envelope::envelope(&m).unwrap();
        assert_eq!(env.cipher, Ok(cipher), "{alg:?} {cipher:?}");
        assert_eq!(env.authenticated, matches!(cipher, Cipher::AesGcm(_)));
        let r = &env.recipients[0];
        assert!(matches!((&r.rid, ski), (Rid::Ski(_), true) | (Rid::IssuerSerial { .. }, false)), "{:?}", r.rid);
        match alg {
            KeyAlg::RsaPkcs1 => assert_eq!(r.wrap, Wrap::RsaPkcs1),
            KeyAlg::RsaOaep(h) => assert_eq!(r.wrap, Wrap::RsaOaep(h)),
            KeyAlg::Ecdh(..) | KeyAlg::GostTransport => unreachable!(),
        }
        let got = soft::unwrap(std::slice::from_ref(&key), &env).unwrap();
        assert_eq!(got.as_slice(), k.as_slice(), "{alg:?}");
        let opened = envelope::open(&m, &got, &pki.trust(), NOW).unwrap();
        assert_eq!(opened.cipher, cipher);
        assert_eq!(opened.html.as_deref(), Some(HTML));
        assert_eq!(opened.inner.kind, Kind::None, "внутри не подписано");
    }
}

/// ECDH: общий секрет — из ключа получателя (как CNG `BCRYPT_KDF_RAW_SECRET`), дальше — KDF и снятие обёртки.
#[test]
fn ecdh_recipients() {
    for (key, bits) in [(Key::p256("me-ec"), 256u16), (Key::p384("me-ec384"), 128)] {
        let root_key = Key::rsa('a');
        let root = issue(&Spec::ca("Root"), &root_key, None);
        let user = issue(&Spec::user("Смирнова", "me@itt.test"), &key, Some((&root, &root_key)));
        let k = cek(32);
        let cms = testkit::enveloped(
            &testkit::html_part(HTML),
            &[To { cert: &user, key: &key, alg: KeyAlg::Ecdh(Hash::Sha256, bits), ski: false }],
            Cipher::AesCbc(256),
            &k,
            false,
        );
        let m = letter(&cms, Cipher::AesCbc(256));
        let env = envelope::envelope(&m).unwrap();
        let r = &env.recipients[0];
        let Wrap::Ecdh { kdf, wrap_bits, ephemeral, ukm } = &r.wrap else { panic!("{:?}", r.wrap) };
        assert_eq!((*kdf, *wrap_bits, ukm.as_deref()), (Hash::Sha256, bits, Some(&b"postman-ukm"[..])));
        let z = match &key {
            Key::P256(sk) => {
                let peer = p256::PublicKey::from_sec1_bytes(ephemeral).unwrap();
                p256::ecdh::diffie_hellman(*sk.as_nonzero_scalar(), peer.as_affine()).raw_secret_bytes().to_vec()
            }
            Key::P384(sk) => {
                let peer = p384::PublicKey::from_sec1_bytes(ephemeral).unwrap();
                p384::ecdh::diffie_hellman(*sk.as_nonzero_scalar(), peer.as_affine()).raw_secret_bytes().to_vec()
            }
            _ => unreachable!(),
        };
        let got = envelope::ecdh_cek(&z, &r.wrap, &r.encrypted_key).unwrap();
        assert_eq!(got.as_slice(), k.as_slice());
        let mut bad = z.clone();
        bad[0] ^= 1;
        assert!(envelope::ecdh_cek(&bad, &r.wrap, &r.encrypted_key).is_none(), "чужой секрет — обёртка не снимается");
        assert_eq!(envelope::open(&m, &got, &Default::default(), NOW).unwrap().html.as_deref(), Some(HTML));
    }
}

/// Неверный ключ: CBC и сбой снятия PKCS#1 — одна ошибка «повреждено»; тег GCM изменён — «повреждено» до разбора
/// MIME; письмо не мне — «не для вас».
#[test]
fn one_error_for_every_oracle() {
    let (pki, key) = me();
    let k = cek(32);
    let to = [To { cert: &pki.user, key: &pki.user_key, alg: KeyAlg::RsaPkcs1, ski: false }];
    let m = letter(&testkit::enveloped(&testkit::html_part(HTML), &to, Cipher::AesCbc(256), &k, false), Cipher::AesCbc(256));
    let mut wrong = k.clone();
    wrong[3] ^= 0x40;
    assert_eq!(envelope::open(&m, &wrong, &Default::default(), NOW), Err(OpenError::Damaged));
    // Ключ содержимого испорчен в письме: PKCS#1 не снимается — ключ из нулей, та же ошибка.
    let mut env = envelope::envelope(&m).unwrap();
    env.recipients[0].encrypted_key[10] ^= 1;
    let zero = soft::unwrap(std::slice::from_ref(&key), &env).unwrap();
    assert_eq!(envelope::open(&m, &zero, &Default::default(), NOW), Err(OpenError::Damaged));
    // Короткий ключ — та же ошибка.
    assert_eq!(envelope::open(&m, &k[..16], &Default::default(), NOW), Err(OpenError::Damaged));

    let gcm = letter(&testkit::enveloped(&testkit::html_part(HTML), &to, Cipher::AesGcm(256), &k, true), Cipher::AesGcm(256));
    assert_eq!(envelope::open(&gcm, &k, &Default::default(), NOW), Err(OpenError::Damaged));
    let cbc = letter(&testkit::enveloped(&testkit::html_part(HTML), &to, Cipher::AesCbc(256), &k, true), Cipher::AesCbc(256));
    assert_eq!(envelope::open(&cbc, &k, &Default::default(), NOW), Err(OpenError::Damaged));

    let other = Pki::rsa("Other", "Петров", "p@itt.test");
    let theirs = [To { cert: &other.user, key: &other.user_key, alg: KeyAlg::RsaPkcs1, ski: false }];
    let m = letter(&testkit::enveloped(&testkit::html_part(HTML), &theirs, Cipher::AesCbc(256), &k, false), Cipher::AesCbc(256));
    let env = envelope::envelope(&m).unwrap();
    // Тот же ключ RSA «b», но сертификат другого издателя — не наш получатель.
    assert_eq!(soft::unwrap(std::slice::from_ref(&key), &env), Err(SoftError::NotForYou));
}

/// Подписать, затем зашифровать: два слоя, подпись внутри верна; вложение — по номеру.
#[test]
fn signed_inside_and_attachments() {
    let (pki, key) = me();
    let sender = Pki::rsa("Root", "Иванов", "user1@itt.test");
    let inner = testkit::mixed_part(HTML, ("Договор.pdf", "application/pdf", b"%PDF-1.7 test"));
    let signed = testkit::signed_part(&inner, &sender.user, &sender.user_key, &[&sender.root], &Sign::default());
    let k = cek(32);
    let to = [
        To { cert: &sender.user, key: &sender.user_key, alg: KeyAlg::RsaPkcs1, ski: false },
        To { cert: &pki.user, key: &pki.user_key, alg: KeyAlg::RsaOaep(Hash::Sha256), ski: false },
    ];
    let m = letter(&testkit::enveloped(&signed, &to, Cipher::AesGcm(256), &k, false), Cipher::AesGcm(256));
    let env = envelope::envelope(&m).unwrap();
    assert_eq!(env.recipients.len(), 2, "себе тоже");
    let got = soft::unwrap(std::slice::from_ref(&key), &env).unwrap();
    let opened = envelope::open(&m, &got, &sender.trust(), NOW).unwrap();
    assert_eq!(opened.inner.kind, Kind::Detached);
    assert_eq!(opened.inner.status, Status::Valid, "{:?}", opened.inner.status);
    assert!(opened.inner.address_ok, "«От» внешнего письма сверяется с сертификатом подписи внутри");
    assert_eq!(opened.html.as_deref(), Some(HTML));
    assert_eq!(opened.parts.len(), 1);
    assert_eq!((opened.parts[0].name.as_str(), opened.parts[0].mime.as_str()), ("Договор.pdf", "application/pdf"));
    assert_eq!(envelope::part(&m, &got, 0).unwrap().as_slice(), b"%PDF-1.7 test");
    assert!(envelope::part(&m, &got, 1).is_err());
}

/// Шифрование внутри шифрования — не открываем; письмо без шифрования — «не зашифровано».
#[test]
fn nested_and_plain() {
    let (pki, key) = me();
    let k = cek(32);
    let to = [To { cert: &pki.user, key: &pki.user_key, alg: KeyAlg::RsaPkcs1, ski: false }];
    let inner_cms = testkit::enveloped(&testkit::html_part(HTML), &to, Cipher::AesCbc(256), &k, false);
    let inner = format!(
        "Content-Type: application/pkcs7-mime; smime-type=enveloped-data\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n",
        testkit::base64(&inner_cms)
    );
    let m = letter(&testkit::enveloped(inner.as_bytes(), &to, Cipher::AesCbc(256), &k, false), Cipher::AesCbc(256));
    let got = soft::unwrap(std::slice::from_ref(&key), &envelope::envelope(&m).unwrap()).unwrap();
    assert_eq!(envelope::open(&m, &got, &Default::default(), NOW), Err(OpenError::Nested));
    let plain = b"From: a@x\r\nContent-Type: text/plain\r\n\r\nhi".to_vec();
    assert_eq!(envelope::envelope(&plain), Err(OpenError::NotEncrypted));
    // Подписанное письмо (Ф1) — не шифрованное, а `verify` по-прежнему видит «зашифровано» у шифрованного.
    assert_eq!(postman_smime::verify(&m, &pki.trust(), NOW).kind, Kind::Encrypted);
}

/// Враждебный CMS: обрезанный на каждом байте и с мусором — ошибка без паники; ГОСТ и RC2 — «не поддерживается».
#[test]
fn truncated_and_unsupported() {
    let (pki, _) = me();
    let k = cek(16);
    let to = [To { cert: &pki.user, key: &pki.user_key, alg: KeyAlg::RsaOaep(Hash::Sha256), ski: true }];
    let cms = testkit::enveloped(&testkit::html_part(HTML), &to, Cipher::AesGcm(128), &k, false);
    for cut in (0..cms.len()).step_by(3) {
        let m = letter(&cms[..cut], Cipher::AesGcm(128));
        let _ = envelope::envelope(&m);
        let _ = envelope::open(&m, &k, &Default::default(), NOW);
    }
    // RC2 вместо 3DES: OID той же длины (1.2.840.113549.3.7 → .3.2).
    let des = testkit::enveloped(&testkit::html_part(HTML), &to, Cipher::Des3, &cek(24), false);
    let oid = [0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x03, 0x07];
    let at = des.windows(oid.len()).position(|w| w == oid).unwrap();
    let mut rc2 = des.clone();
    rc2[at + 7] = 0x02;
    let env = envelope::envelope(&letter(&rc2, Cipher::Des3)).unwrap();
    assert_eq!(env.cipher, Err("RC2".into()));
}

/// ГОСТ (RFC 4490): получатель и параметры содержимого разобраны для СКЗИ; ключ в памяти не снимается — «нужно
/// СКЗИ» (решение владельца 2026-10-22), воркер содержимое ГОСТ не расшифровывает.
#[test]
fn gost_recipient_is_parsed_for_the_skzi() {
    use postman_smime::envelope::{GostKey, OpenError as E};
    use postman_smime::oid;
    let pki = Pki::build(Key::gost("root", oid::TC26_256_A), Key::gost("user", oid::TC26_256_A), "ГОСТ УЦ", "Сидоров", "me@itt.test");
    let cms = testkit::enveloped(
        &testkit::html_part(HTML),
        &[To { cert: &pki.user, key: &pki.user_key, alg: KeyAlg::GostTransport, ski: false }],
        Cipher::Gost28147,
        &[0; 32],
        false,
    );
    let m = letter(&cms, Cipher::Gost28147);
    let env = envelope::envelope(&m).unwrap();
    assert_eq!(env.cipher, Ok(Cipher::Gost28147));
    let Wrap::Gost(t) = &env.recipients[0].wrap else { panic!("{:?}", env.recipients[0].wrap) };
    assert_eq!(t.key, GostKey::G2012_256);
    assert_eq!(t.param_set, oid::GOST28147_PARAM_A.to_string());
    assert_eq!(t.public_param_set, oid::TC26_256_A.to_string());
    assert_eq!((t.ukm.len(), t.encrypted_key.len(), t.mac.len(), t.ephemeral.len()), (8, 32, 4, 64));
    let g = env.gost.as_ref().unwrap();
    assert_eq!((g.iv.len(), g.param_set.as_str()), (8, oid::GOST28147_PARAM_A.to_string().as_str()));
    assert_eq!(g.data, testkit::html_part(HTML), "шифр ГОСТ в тесте — заглушка: содержимое как есть");
    let Key::Gost { .. } = pki.user_key else { unreachable!() };
    assert!(matches!(envelope::open(&m, &[0; 32], &Default::default(), NOW), Err(E::Unsupported(_))));
    // Письмо, расшифрованное в ядре (СКЗИ): тело и вложения — тем же разбором.
    let o = envelope::open_inner(&g.data, "a@itt.test", Cipher::Gost28147, &Default::default(), NOW).unwrap();
    assert_eq!(o.html.as_deref(), Some(HTML));
}
