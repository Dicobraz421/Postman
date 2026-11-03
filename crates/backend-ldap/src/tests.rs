//! Тесты на записанных ответах LDAP (`docs/SMIME.md` Ф5): подменный сервер на 127.0.0.1 отвечает сообщениями BER
//! по RFC 4511 — как AD (rootDSE, записи с `userCertificate`, ссылки на другие разделы, предел записей).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use ldap3::asn1::{PL, StructureTag, TagClass};
use postman_core::smime::directory::{Directory, DirectoryError};

use super::*;

const BASE: &str = "DC=corp,DC=example";

fn tlv(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let len = body.len();
    if len < 0x80 {
        out.push(len as u8);
    } else {
        let bytes: Vec<u8> = len.to_be_bytes().into_iter().skip_while(|b| *b == 0).collect();
        out.push(0x80 | bytes.len() as u8);
        out.extend(bytes);
    }
    out.extend_from_slice(body);
    out
}

fn int(v: u32) -> Vec<u8> {
    let mut b: Vec<u8> = v.to_be_bytes().into_iter().skip_while(|b| *b == 0).collect();
    if b.first().is_none_or(|f| f & 0x80 != 0) {
        b.insert(0, 0);
    }
    tlv(0x02, &b)
}

fn octet(b: &[u8]) -> Vec<u8> {
    tlv(0x04, b)
}

fn message(id: u32, op: Vec<u8>) -> Vec<u8> {
    tlv(0x30, &[int(id), op].concat())
}

fn result(tag: u8, rc: u8) -> Vec<u8> {
    tlv(tag, &[tlv(0x0a, &[rc]), octet(b""), octet(b"")].concat())
}

/// `SearchResultEntry`: DN и атрибуты.
fn entry(dn: &str, attrs: &[(&str, &[&[u8]])]) -> Vec<u8> {
    let attrs: Vec<u8> = attrs
        .iter()
        .flat_map(|(name, vals)| tlv(0x30, &[octet(name.as_bytes()), tlv(0x31, &vals.iter().flat_map(|v| octet(v)).collect::<Vec<u8>>())].concat()))
        .collect();
    tlv(0x64, &[octet(dn.as_bytes()), tlv(0x30, &attrs)].concat())
}

/// Одна TLV из начала `b`: тег, содержимое, остаток.
fn take(b: &[u8]) -> (u8, &[u8], &[u8]) {
    let (len, head) = match b[1] {
        l if l < 0x80 => (usize::from(l), 2),
        l => {
            let n = usize::from(l & 0x7f);
            (b[2..2 + n].iter().fold(0usize, |a, x| a << 8 | usize::from(*x)), 2 + n)
        }
    };
    (b[0], &b[head..head + len], &b[head + len..])
}

fn read_message(s: &mut TcpStream) -> Option<Vec<u8>> {
    let mut head = [0u8; 2];
    s.read_exact(&mut head).ok()?;
    let mut msg = head.to_vec();
    let len = if head[1] < 0x80 {
        usize::from(head[1])
    } else {
        let mut n = vec![0u8; usize::from(head[1] & 0x7f)];
        s.read_exact(&mut n).ok()?;
        msg.extend(&n);
        n.iter().fold(0usize, |a, x| a << 8 | usize::from(*x))
    };
    let mut body = vec![0u8; len];
    s.read_exact(&mut body).ok()?;
    msg.extend(body);
    Some(msg)
}

/// Что сервер видел: операции (тег) и базы поиска.
#[derive(Default)]
struct Seen {
    ops: Vec<u8>,
    bases: Vec<String>,
}

/// Подменный сервер: на каждый поиск — ответ `answer(база)` (записи и код итога).
fn server(answer: impl Fn(&str) -> (Vec<Vec<u8>>, u8) + Send + 'static) -> (String, Arc<Mutex<Seen>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let log = seen.clone();
    std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        while let Some(msg) = read_message(&mut s) {
            let (_, body, _) = take(&msg);
            let (_, id, rest) = take(body);
            let id = id.iter().fold(0u32, |a, x| a << 8 | u32::from(*x));
            let (op, req, _) = take(rest);
            log.lock().unwrap().ops.push(op);
            let reply = match op {
                0x60 => message(id, result(0x61, 0)),
                0x63 => {
                    let (_, base, _) = take(req);
                    let base = String::from_utf8(base.to_vec()).unwrap();
                    let (entries, rc) = answer(&base);
                    log.lock().unwrap().bases.push(base);
                    let mut out: Vec<u8> = entries.into_iter().flat_map(|e| message(id, e)).collect();
                    out.extend(message(id, result(0x65, rc)));
                    out
                }
                _ => break, // выход (unbind)
            };
            s.write_all(&reply).unwrap();
        }
    });
    (addr, seen)
}

fn anonymous(server: &str, base_dn: Option<&str>) -> LdapDirectory {
    LdapDirectory::new(Settings { server: Some(server.into()), base_dn: base_dn.map(Into::into), login: Login::Anonymous, ..Settings::default() })
}

/// Как AD: база — из rootDSE, сертификаты из нескольких записей и обоих имён атрибута; ссылки на другие разделы и
/// лишние атрибуты пропускаются; предел записей — не ошибка.
#[test]
fn finds_certificates() {
    let (addr, seen) = server(|base| match base {
        "" => (vec![entry("", &[("defaultNamingContext", &[BASE.as_bytes()])])], 0),
        _ => (
            vec![
                entry(
                    "CN=Anna,OU=Staff,DC=corp,DC=example",
                    &[("mail", &[b"anna@corp.example"]), ("userCertificate", &[b"\x30\x82C1", b"\x30\x82C2"])],
                ),
                // Ссылка на другой раздел (`SearchResultReference`).
                tlv(0x73, &octet(b"ldap://ForestDnsZones.corp.example/DC=ForestDnsZones,DC=corp,DC=example")),
                entry("CN=Boris,OU=Staff,DC=corp,DC=example", &[("usercertificate;binary", &[b"\x30\x82C3"])]),
            ],
            4,
        ),
    });
    let got = anonymous(&addr, None).certificates(&["anna@corp.example".into(), "boris@corp.example".into()]).unwrap();
    assert_eq!(got, [b"\x30\x82C1".to_vec(), b"\x30\x82C2".to_vec(), b"\x30\x82C3".to_vec()]);
    assert_eq!(seen.lock().unwrap().bases, ["", BASE]);
}

/// База из настроек — rootDSE не спрашивают; входа без настроек нет (подменный сервер — без входа).
#[test]
fn base_from_settings() {
    let (addr, seen) = server(|_| (vec![entry("CN=A", &[("userCertificate", &[b"\x30\x82A"])])], 0));
    let got = anonymous(&format!("ldap://{addr}"), Some(BASE)).certificates(&["a@corp.example".into()]).unwrap();
    assert_eq!(got, [b"\x30\x82A".to_vec()]);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.bases, [BASE]);
    assert!(!seen.ops.contains(&0x60), "bind: {:?}", seen.ops);
}

/// Отказ поиска (нет такой базы) — «каталог недоступен».
#[test]
fn search_error_is_unavailable() {
    let (addr, _) = server(|_| (vec![], 32));
    let got = anonymous(&addr, Some("DC=gone")).certificates(&["a@corp.example".into()]);
    assert!(matches!(got, Err(DirectoryError::Unavailable(ref e)) if e.contains("rc=32")), "{got:?}");
}

fn closed_port() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().to_string()
}

/// Нет связи — «каталог недоступен», без ожидания тайм-аута.
#[test]
fn refused_is_unavailable() {
    let t = std::time::Instant::now();
    let got = anonymous(&closed_port(), Some(BASE)).certificates(&["a@corp.example".into()]);
    assert!(matches!(got, Err(DirectoryError::Unavailable(_))), "{got:?}");
    assert!(t.elapsed() < TIMEOUT);
}

/// Простой вход по открытому каналу не начинается: пароль ушёл бы как есть.
#[test]
fn simple_login_needs_tls() {
    let login = Login::Simple { user: "CN=A".into(), password: Zeroizing::new("secret".into()) };
    let dir = LdapDirectory::new(Settings { server: Some(closed_port()), base_dn: Some(BASE.into()), login, ..Settings::default() });
    let got = dir.certificates(&["a@corp.example".into()]);
    assert!(matches!(got, Err(DirectoryError::Unavailable(ref e)) if e.contains("ldaps")), "{got:?}");
}

/// OpenLDAP: в rootDSE только `namingContexts` — база — первый раздел данных, `cn=config` пропускается; сертификат —
/// `userCertificate;binary` (RFC 4523).
#[test]
fn openldap_base_from_naming_contexts() {
    let (addr, seen) = server(|base| match base {
        "" => (vec![entry("", &[("namingContexts", &[b"cn=config", b"dc=example,dc=org"])])], 0),
        _ => (vec![entry("uid=anna,ou=people,dc=example,dc=org", &[("userCertificate;binary", &[b"\x30\x82O"])])], 0),
    });
    let got = anonymous(&addr, None).certificates(&["anna@example.org".into()]).unwrap();
    assert_eq!(got, [b"\x30\x82O".to_vec()]);
    assert_eq!(seen.lock().unwrap().bases, ["", "dc=example,dc=org"]);
}

/// Выбор базы из rootDSE: `defaultNamingContext` (AD, 389 DS — ALD Pro, FreeIPA) важнее списка; служебные разделы
/// 389 DS пропускаются; нет подходящего — нет базы.
#[test]
fn base_choice() {
    let root = |attrs: &[(&str, &[&[u8]])]| ldap3::asn1::parse_tag(&entry("", attrs)).map(|(_, t)| t).unwrap();
    let r389 =
        root(&[("namingContexts", &[b"o=netscaperoot", b"cn=changelog", b"dc=ald,dc=example"]), ("defaultnamingcontext", &[b"dc=ald,dc=example"])]);
    assert_eq!(choose_base(r389).as_deref(), Some("dc=ald,dc=example"));
    let r389_list = root(&[("namingContexts", &[b"o=NetscapeRoot", b"cn=changelog", b"dc=ald,dc=example"])]);
    assert_eq!(choose_base(r389_list).as_deref(), Some("dc=ald,dc=example"));
    assert_eq!(choose_base(root(&[("namingContexts", &[b"cn=config"])])), None);
    assert_eq!(choose_base(root(&[])), None);
}

#[test]
fn targets() {
    let t = |s: &str| Target::parse(s).map(|t| (t.url, t.host, t.tls));
    assert_eq!(t("dc1.corp.example"), Ok(("ldap://dc1.corp.example".into(), "dc1.corp.example".into(), false)));
    assert_eq!(t(" LDAPS://dc1.corp.example:636/ "), Ok(("ldaps://dc1.corp.example:636".into(), "dc1.corp.example".into(), true)));
    assert_eq!(t("[::1]:389"), Ok(("ldap://[::1]:389".into(), "::1".into(), false)));
    for bad in ["", "http://dc1", "ldap://", "ldap://user@dc1", "dc 1"] {
        assert!(Target::parse(bad).is_err(), "{bad}");
    }
}

/// Спецсимволы адреса экранируются (RFC 4515): фильтр не меняет смысл.
#[test]
fn filter_escapes() {
    assert_eq!(
        filter(&["a*(b)@x.test".into(), "c@x.test".into()]),
        r"(|(mail=a\2a\28b\29@x.test)(proxyAddresses=smtp:a\2a\28b\29@x.test)(mail=c@x.test)(proxyAddresses=smtp:c@x.test))"
    );
}

/// Чужая запись странной формы — пусто, без паники.
#[test]
fn malformed_entries_are_skipped() {
    let tag = |id, payload| StructureTag { class: TagClass::Application, id, payload };
    let prim = |b: &[u8]| StructureTag { class: TagClass::Universal, id: 4, payload: PL::P(b.to_vec()) };
    assert!(values(tag(4, PL::P(vec![1])), &CERT_ATTRS).is_empty());
    assert!(values(tag(5, PL::C(vec![])), &CERT_ATTRS).is_empty());
    assert!(values(tag(4, PL::C(vec![prim(b"CN=A")])), &CERT_ATTRS).is_empty());
    let odd_attr = StructureTag { class: TagClass::Universal, id: 16, payload: PL::C(vec![prim(b"userCertificate")]) };
    let attrs = StructureTag { class: TagClass::Universal, id: 16, payload: PL::C(vec![prim(b"x"), odd_attr]) };
    assert!(values(tag(4, PL::C(vec![prim(b"CN=A"), attrs])), &CERT_ATTRS).is_empty());
}
