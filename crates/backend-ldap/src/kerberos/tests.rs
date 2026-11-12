//! Обмен SASL GSSAPI, StartTLS, привязка к каналу и мост — без сервера и без Kerberos: подменный контекст GSS
//! («обёртка» — метка и флаг шифрования перед данными) и подменный сервер на `tokio::io::duplex`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ldap3::asn1::{StructureTag, TagClass, parse_tag};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, duplex};

use super::*;

/// Подменный контекст: два шага (AP-REQ → AP-REP), «обёртка» — `W`, флаг шифрования, данные.
struct FakeGss {
    steps: usize,
    /// Сколько шагов до готовности (`usize::MAX` — никогда).
    rounds: usize,
}

impl FakeGss {
    fn new() -> FakeGss {
        FakeGss { steps: 0, rounds: 2 }
    }
}

fn fake_wrap(encrypt: bool, data: &[u8]) -> Vec<u8> {
    [&[b'W', u8::from(encrypt)][..], data].concat()
}

impl Gss for FakeGss {
    fn step(&mut self, input: Option<&[u8]>) -> Result<GssStep, String> {
        self.steps += 1;
        match (self.steps, input) {
            (1, None) => Ok(GssStep { token: b"ap-req".to_vec(), done: self.rounds == 1 }),
            (_, Some(b"ap-rep")) => Ok(GssStep { token: Vec::new(), done: self.steps >= self.rounds }),
            (_, Some(_)) if self.rounds == usize::MAX => Ok(GssStep { token: b"more".to_vec(), done: false }),
            _ => Err("unexpected GSS step".into()),
        }
    }

    fn wrap(&mut self, encrypt: bool, data: &[u8]) -> Result<Vec<u8>, String> {
        Ok(fake_wrap(encrypt, data))
    }

    fn unwrap(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        match data {
            [b'W', _, rest @ ..] => Ok(rest.to_vec()),
            _ => Err("bad token".into()),
        }
    }

    fn wrap_limit(&mut self, max_token: u32) -> Result<u32, String> {
        Ok(max_token.saturating_sub(2))
    }
}

/// Запрос клиента, как его видит сервер: номер, операция и её поля.
struct Request {
    id: u32,
    op: u64,
    fields: Vec<StructureTag>,
}

async fn recv(io: &mut DuplexStream) -> Request {
    let msg = read_message(io).await.unwrap();
    let (rest, tag) = parse_tag(&msg).unwrap();
    assert!(rest.is_empty());
    let mut parts = tag.expect_constructed().unwrap().into_iter();
    let id = uint(&parts.next().unwrap().expect_primitive().unwrap()).unwrap();
    let op = parts.next().unwrap();
    assert_eq!(op.class, TagClass::Application);
    let op_id = op.id;
    let fields = match op.payload {
        ldap3::asn1::PL::C(f) => f,
        ldap3::asn1::PL::P(_) => vec![op],
    };
    Request { id, op: op_id, fields }
}

/// Поля привязки SASL: механизм и токен.
fn sasl(req: Request) -> (u32, String, Option<Vec<u8>>) {
    assert_eq!(req.op, 0, "BindRequest");
    let mut f = req.fields.into_iter();
    assert_eq!(uint(&f.next().unwrap().expect_primitive().unwrap()), Some(3));
    assert_eq!(f.next().unwrap().expect_primitive().unwrap(), b"");
    let auth = f.next().unwrap();
    assert_eq!((auth.class, auth.id), (TagClass::Context, 3));
    let mut auth = auth.expect_constructed().unwrap().into_iter();
    let mech = String::from_utf8(auth.next().unwrap().expect_primitive().unwrap()).unwrap();
    (req.id, mech, auth.next().map(|c| c.expect_primitive().unwrap()))
}

fn enumerated(v: u32) -> Vec<u8> {
    let mut i = int(v);
    i[0] = 0x0A;
    i
}

fn response(id: u32, op: u8, rc: u32, text: &str, creds: Option<&[u8]>) -> Vec<u8> {
    let mut body = enumerated(rc);
    body.extend(tlv(0x04, b""));
    body.extend(tlv(0x04, text.as_bytes()));
    if let Some(c) = creds {
        body.extend(tlv(0x87, c));
    }
    message(id, &tlv(0x60 | op, &body))
}

async fn reply(io: &mut DuplexStream, id: u32, rc: u32, creds: Option<&[u8]>) {
    io.write_all(&response(id, 1, rc, "", creds)).await.unwrap();
}

/// Сервер, как AD: AP-REQ → AP-REP, пустой ответ → предложение слоёв `mask` с пределом 0x1000, ответ клиента → `final_rc`.
/// Отдаёт ответ клиента о слое (развёрнутый).
async fn server(mut io: DuplexStream, mask: u8, final_rc: u32) -> Vec<u8> {
    let (id, mech, creds) = sasl(recv(&mut io).await);
    assert_eq!((mech.as_str(), creds.as_deref()), ("GSSAPI", Some(&b"ap-req"[..])));
    reply(&mut io, id, 14, Some(b"ap-rep")).await;
    let (id, _, creds) = sasl(recv(&mut io).await);
    assert_eq!(creds, None, "empty response after the context is ready");
    reply(&mut io, id, 14, Some(&fake_wrap(false, &[mask, 0x00, 0x10, 0x00]))).await;
    let (id, _, creds) = sasl(recv(&mut io).await);
    let answer = creds.unwrap();
    reply(&mut io, id, final_rc, None).await;
    answer
}

#[tokio::test]
async fn bind_over_plain_ldap_negotiates_privacy() {
    let (mut client, srv) = duplex(4096);
    let server = tokio::spawn(server(srv, 7, 0));
    let mut id = 0;
    let layer = bind(&mut client, &mut FakeGss::new(), false, &mut id).await.unwrap();
    assert_eq!(layer, Some(0x1000));
    assert_eq!(id, 3);
    // Шифрование и наш предел буфера; ответ о слое — без шифрования (RFC 4752).
    assert_eq!(server.await.unwrap(), fake_wrap(false, &[LAYER_PRIVACY, 0x9F, 0xFF, 0xB8]));
}

#[tokio::test]
async fn bind_over_tls_takes_no_layer() {
    let (mut client, srv) = duplex(4096);
    let server = tokio::spawn(server(srv, 7, 0));
    let mut id = 1;
    assert_eq!(bind(&mut client, &mut FakeGss::new(), true, &mut id).await.unwrap(), None);
    assert_eq!(id, 4, "message ids go on after StartTLS");
    assert_eq!(server.await.unwrap(), fake_wrap(false, &[LAYER_NONE, 0, 0, 0]));
}

#[tokio::test]
async fn bind_needs_privacy_without_tls() {
    let (mut client, mut srv) = duplex(4096);
    let server = tokio::spawn(async move {
        let (id, ..) = sasl(recv(&mut srv).await);
        reply(&mut srv, id, 14, Some(b"ap-rep")).await;
        let (id, ..) = sasl(recv(&mut srv).await);
        // Только «без слоя» и подпись — по `ldap://` мало.
        reply(&mut srv, id, 14, Some(&fake_wrap(false, &[LAYER_NONE | 2, 0, 0x10, 0]))).await;
    });
    let err = bind(&mut client, &mut FakeGss::new(), false, &mut 0).await.unwrap_err();
    assert!(err.to_string().contains("no suitable GSSAPI security layer"), "{err}");
    server.await.unwrap();
}

#[tokio::test]
async fn bind_reports_server_refusal() {
    let (mut client, mut srv) = duplex(4096);
    tokio::spawn(async move {
        let (id, ..) = sasl(recv(&mut srv).await);
        srv.write_all(&response(id, 1, 49, "80090308: LdapErr: DSID-0C09056D", None)).await.unwrap();
    });
    let err = bind(&mut client, &mut FakeGss::new(), false, &mut 0).await.unwrap_err();
    assert!(err.to_string().contains("rc=49 80090308"), "{err}");
}

#[tokio::test]
async fn bind_refuses_success_without_layer_negotiation() {
    let (mut client, mut srv) = duplex(4096);
    tokio::spawn(async move {
        let (id, ..) = sasl(recv(&mut srv).await);
        reply(&mut srv, id, 0, None).await;
    });
    let err = bind(&mut client, &mut FakeGss::new(), false, &mut 0).await.unwrap_err();
    assert!(err.to_string().contains("without security layer"), "{err}");
}

#[tokio::test]
async fn bind_stops_endless_exchange() {
    let (mut client, mut srv) = duplex(4096);
    tokio::spawn(async move {
        loop {
            let (id, ..) = sasl(recv(&mut srv).await);
            reply(&mut srv, id, 14, Some(b"again")).await;
        }
    });
    let mut gss = FakeGss { steps: 0, rounds: usize::MAX };
    let err = bind(&mut client, &mut gss, false, &mut 0).await.unwrap_err();
    assert!(err.to_string().contains("too many rounds"), "{err}");
}

#[tokio::test]
async fn bind_reports_final_failure() {
    let (mut client, srv) = duplex(4096);
    tokio::spawn(server(srv, 7, 49));
    let err = bind(&mut client, &mut FakeGss::new(), false, &mut 0).await.unwrap_err();
    assert!(err.to_string().contains("rc=49"), "{err}");
}

#[tokio::test]
async fn starttls_exchange() {
    for (rc, ok) in [(0, true), (2, false)] {
        let (mut client, mut srv) = duplex(4096);
        let server = tokio::spawn(async move {
            let req = recv(&mut srv).await;
            assert_eq!(req.op, 23, "ExtendedRequest");
            let name = req.fields.into_iter().next().unwrap();
            assert_eq!((name.class, name.id), (TagClass::Context, 0));
            assert_eq!(name.expect_primitive().unwrap(), STARTTLS_OID);
            srv.write_all(&response(req.id, 24, rc, "", None)).await.unwrap();
        });
        let mut id = 0;
        assert_eq!(starttls(&mut client, &mut id).await.is_ok(), ok);
        assert_eq!(id, 1);
        server.await.unwrap();
    }
}

#[test]
fn parse_response_rejects_foreign_messages() {
    // Уведомление об отключении (номер 0), чужой номер, чужая операция, мусор.
    let notice = response(0, 24, 52, "going down", None);
    assert!(parse_response(&notice, 1, BIND_RESPONSE).unwrap_err().to_string().contains("rc=52 going down"));
    assert!(parse_response(&response(2, 1, 0, "", None), 1, BIND_RESPONSE).is_err());
    assert!(parse_response(&response(1, 24, 0, "", None), 1, BIND_RESPONSE).is_err());
    assert!(parse_response(b"\x30\x03\x02\x01", 1, BIND_RESPONSE).is_err());
    let ok = parse_response(&response(1, 1, 14, "", Some(b"tok")), 1, BIND_RESPONSE).unwrap();
    assert_eq!(ok, Response { rc: 14, text: String::new(), creds: Some(b"tok".to_vec()) });
}

#[tokio::test]
async fn read_message_limits_size() {
    let (mut client, mut srv) = duplex(64);
    srv.write_all(&[0x30, 0x84, 0x7F, 0xFF, 0xFF, 0xFF]).await.unwrap();
    assert!(read_message(&mut client).await.unwrap_err().to_string().contains("too large"));
    srv.write_all(&[0x02, 0x01, 0x00]).await.unwrap();
    assert!(read_message(&mut client).await.is_err(), "not a SEQUENCE");
}

#[test]
fn bind_request_encoding() {
    // RFC 4511: LDAPMessage { 1, [APPLICATION 0] { 3, "", [3] { "GSSAPI", "tok" } } }.
    let expected = b"\x30\x19\x02\x01\x01\x60\x14\x02\x01\x03\x04\x00\xa3\x0d\x04\x06GSSAPI\x04\x03tok";
    assert_eq!(bind_request(1, Some(b"tok")), expected);
    assert_eq!(bind_request(2, None), b"\x30\x14\x02\x01\x02\x60\x0f\x02\x01\x03\x04\x00\xa3\x08\x04\x06GSSAPI");
    // Длинный токен — длинная форма длины.
    let long = bind_request(3, Some(&[0u8; 300]));
    assert_eq!(&long[..4], b"\x30\x82\x01\x48");
}

/// Сертификат-заготовка: SEQUENCE { tbs, AlgorithmIdentifier { OID }, BIT STRING }.
fn cert(oid: &[u8]) -> Vec<u8> {
    let mut body = tlv(0x30, &tlv(0x02, b"\x01"));
    body.extend(tlv(0x30, &[tlv(0x06, oid), tlv(0x05, b"")].concat()));
    body.extend(tlv(0x03, b"\x00sig"));
    tlv(0x30, &body)
}

#[test]
fn channel_binding_follows_signature_hash() {
    use sha2::Digest as _;
    let sha256_rsa = cert(b"\x2a\x86\x48\x86\xf7\x0d\x01\x01\x0b");
    let expected = [&b"tls-server-end-point:"[..], &sha2::Sha256::digest(&sha256_rsa)].concat();
    assert_eq!(channel_binding(&sha256_rsa), Some(expected));
    // SHA-1 — SHA-256 (RFC 5929, 4.1); ECDSA с SHA-384 — SHA-384.
    let sha1_rsa = cert(b"\x2a\x86\x48\x86\xf7\x0d\x01\x01\x05");
    assert_eq!(channel_binding(&sha1_rsa).unwrap()[21..], sha2::Sha256::digest(&sha1_rsa)[..]);
    let ecdsa384 = cert(b"\x2a\x86\x48\xce\x3d\x04\x03\x03");
    assert_eq!(channel_binding(&ecdsa384).unwrap()[21..], sha2::Sha384::digest(&ecdsa384)[..]);
    // RSASSA-PSS (хеш в параметрах), Ed25519, мусор — без привязки.
    assert_eq!(channel_binding(&cert(b"\x2a\x86\x48\x86\xf7\x0d\x01\x01\x0a")), None);
    assert_eq!(channel_binding(&cert(b"\x2b\x65\x70")), None);
    assert_eq!(channel_binding(b"\x30\x02\x05\x00"), None);
}

#[tokio::test]
async fn relay_with_privacy_layer() {
    let (ldap, local) = duplex(4096);
    let (remote, server) = duplex(4096);
    let relay = tokio::spawn(relay(local, remote, Some((FakeGss::new(), 8))));
    let (mut ldap, mut server) = (ldap, server);
    // К серверу: буферы SASL не длиннее его предела (8 байт токена — 6 байт текста).
    ldap.write_all(b"search-request").await.unwrap();
    let mut got = Vec::new();
    while got.len() < 14 {
        let mut len = [0u8; 4];
        server.read_exact(&mut len).await.unwrap();
        let mut token = vec![0u8; u32::from_be_bytes(len) as usize];
        assert!(token.len() <= 8);
        server.read_exact(&mut token).await.unwrap();
        assert_eq!(&token[..2], b"W\x01", "encrypted");
        got.extend_from_slice(&token[2..]);
    }
    assert_eq!(got, b"search-request");
    // От сервера: развёрнутый текст.
    server.write_all(&frame(&fake_wrap(true, b"entry")).unwrap()).await.unwrap();
    let mut buf = [0u8; 5];
    ldap.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"entry");
    // Слишком длинный буфер — мост закрывается.
    server.write_all(&(RECV_MAX + 1).to_be_bytes()).await.unwrap();
    relay.await.unwrap();
    assert_eq!(ldap.read(&mut buf).await.unwrap(), 0);
}

#[tokio::test]
async fn relay_without_layer_passes_bytes() {
    let (mut ldap, local) = duplex(4096);
    let (remote, mut server) = duplex(4096);
    tokio::spawn(relay(local, remote, None::<(FakeGss, u32)>));
    ldap.write_all(b"plain").await.unwrap();
    let mut buf = [0u8; 5];
    server.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"plain");
    server.write_all(b"reply").await.unwrap();
    ldap.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"reply");
}

#[test]
fn socket_address_defaults_port() {
    let addr = |s: &str| crate::Target::parse(s).unwrap().socket_addr();
    assert_eq!(addr("dc1.corp.example"), "dc1.corp.example:389");
    assert_eq!(addr("ldaps://dc1.corp.example"), "dc1.corp.example:636");
    assert_eq!(addr("ldap://dc1.corp.example:3268"), "dc1.corp.example:3268");
    assert_eq!(addr("ldap://[2001:db8::1]"), "[2001:db8::1]:389");
    assert_eq!(addr("ldaps://[2001:db8::1]:3269"), "[2001:db8::1]:3269");
}
