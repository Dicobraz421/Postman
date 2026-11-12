use super::*;
use crate::testkit::{self, Eku, Issued, Key, NOW, OcspReply, OcspStatus, Spec};

const URL: &str = "http://ocsp.corp.test/";
const NONCE: &[u8] = b"0123456789abcdef";

struct Pki {
    root: Issued,
    key: Key,
    user: Issued,
}

fn pki() -> Pki {
    let key = Key::rsa('a');
    let root = testkit::issue(&Spec::ca("Corp CA"), &key, None);
    let user = testkit::issue(&Spec { ocsp_url: Some(URL.into()), ..Spec::user("User", "user@corp.test") }, &Key::rsa('b'), Some((&root, &key)));
    Pki { root, key, user }
}

fn reply<'a>(p: &'a Pki, status: OcspStatus) -> OcspReply<'a> {
    let now = NOW as i64;
    OcspReply {
        cert: &p.user,
        issuer: &p.root,
        signer: (&p.root, &p.key),
        status,
        this_update: now - 60,
        next_update: Some(now + 3600),
        nonce: Some(NONCE),
        by_key: false,
    }
}

fn run(p: &Pki, r: &OcspReply<'_>) -> Result<Checked, OcspError> {
    check(&p.user.der, &p.root.der, NONCE, &testkit::ocsp_response(r), NOW)
}

#[test]
fn responders_and_request() {
    let p = pki();
    assert_eq!(responders(&p.user.der), vec![URL.to_string()]);
    assert!(responders(&p.root.der).is_empty());
    let req = request(&p.user.der, &p.root.der, NONCE).unwrap();
    assert_eq!(req[0], 0x30);
    assert!(req.windows(NONCE.len()).any(|w| w == NONCE));
    assert!(request(&p.user.der, &p.root.der, &[]).is_none());
}

#[test]
fn good_and_revoked() {
    let p = pki();
    let now = NOW as i64;
    assert_eq!(run(&p, &reply(&p, OcspStatus::Good)).unwrap().verdict, Verdict::NotRevoked);
    assert_eq!(run(&p, &reply(&p, OcspStatus::Revoked(now - 100))).unwrap().verdict, Verdict::Revoked { at: now - 100 });
    assert_eq!(run(&p, &OcspReply { by_key: true, ..reply(&p, OcspStatus::Good) }).unwrap().next_update, now + 3600);
    assert_eq!(run(&p, &reply(&p, OcspStatus::Unknown)), Err(OcspError::Unknown));
}

#[test]
fn nonce_time_and_certificate() {
    let p = pki();
    let now = NOW as i64;
    assert_eq!(run(&p, &OcspReply { nonce: Some(b"other"), ..reply(&p, OcspStatus::Good) }), Err(OcspError::Nonce));
    // Ответчик без nonce (заранее подписанные ответы) — годен.
    assert!(run(&p, &OcspReply { nonce: None, ..reply(&p, OcspStatus::Good) }).is_ok());
    assert_eq!(run(&p, &OcspReply { next_update: Some(now - 1), ..reply(&p, OcspStatus::Good) }), Err(OcspError::Expired));
    assert_eq!(run(&p, &OcspReply { this_update: now + 3600, ..reply(&p, OcspStatus::Good) }), Err(OcspError::NotYetValid));
    assert_eq!(run(&p, &OcspReply { this_update: now - 7200, next_update: None, ..reply(&p, OcspStatus::Good) }), Err(OcspError::Expired));
    // Ответ о другом сертификате того же издателя.
    let other = testkit::issue(&Spec { serial: 0x2002, ..Spec::user("Other", "o@corp.test") }, &Key::rsa('c'), Some((&p.root, &p.key)));
    let bytes = testkit::ocsp_response(&OcspReply { cert: &other, ..reply(&p, OcspStatus::Good) });
    assert_eq!(check(&p.user.der, &p.root.der, NONCE, &bytes, NOW), Err(OcspError::WrongCert));
}

#[test]
fn signer_must_be_issuer_or_its_delegate() {
    let p = pki();
    // Чужой ключ под именем издателя — подпись не сходится.
    let forged = Key::rsa('d');
    assert_eq!(run(&p, &OcspReply { signer: (&p.root, &forged), ..reply(&p, OcspStatus::Good) }), Err(OcspError::BadSignature));
    // Делегат с EKU OCSPSigning, выпущенный издателем, — годен; без EKU или выпущенный другим — нет.
    let dkey = Key::rsa('e');
    let delegate =
        testkit::issue(&Spec { eku: Eku::OcspSigning, serial: 0x3003, ..Spec::user("OCSP", "ocsp@corp.test") }, &dkey, Some((&p.root, &p.key)));
    assert!(run(&p, &OcspReply { signer: (&delegate, &dkey), ..reply(&p, OcspStatus::Good) }).is_ok());
    let plain = testkit::issue(&Spec { serial: 0x3004, ..Spec::user("Plain", "p@corp.test") }, &dkey, Some((&p.root, &p.key)));
    assert_eq!(run(&p, &OcspReply { signer: (&plain, &dkey), ..reply(&p, OcspStatus::Good) }), Err(OcspError::Unauthorized));
    let other_ca = testkit::issue(&Spec::ca("Other CA"), &forged, None);
    let foreign = testkit::issue(&Spec { eku: Eku::OcspSigning, ..Spec::user("OCSP", "x@corp.test") }, &dkey, Some((&other_ca, &forged)));
    assert_eq!(run(&p, &OcspReply { signer: (&foreign, &dkey), ..reply(&p, OcspStatus::Good) }), Err(OcspError::Unauthorized));
}

#[test]
fn damaged_responses_do_not_panic() {
    let p = pki();
    let good = testkit::ocsp_response(&reply(&p, OcspStatus::Good));
    for cut in 0..good.len() {
        assert!(check(&p.user.der, &p.root.der, NONCE, &good[..cut], NOW).is_err());
    }
    let mut bad = good.clone();
    for i in 0..bad.len() {
        bad[i] ^= 0xA5;
        let _ = check(&p.user.der, &p.root.der, NONCE, &bad, NOW);
        bad[i] ^= 0xA5;
    }
    // Ответчик отказал (`tryLater`).
    assert_eq!(check(&p.user.der, &p.root.der, NONCE, &[0x30, 0x03, 0x0A, 0x01, 0x03], NOW), Err(OcspError::Status(3)));
}
