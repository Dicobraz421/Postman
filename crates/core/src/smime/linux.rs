//! S/MIME на Linux (LIN-05, решение владельца 2026-11-12): якоря — системное хранилище корней (`/etc/ssl/certs` и
//! аналоги дистрибутивов), свои ключи — файлы PKCS#12 с паролем (токенов PKCS#11 и хранилищ нет; ГОСТ — LIN-07).
//!
//! Файлы и пароли передаёт приложение ([`load_personal_files`]): пути — из настроек, пароли — из хранилища паролей
//! системы (`pets_shell::credentials`, Secret Service); на диск ядро их не пишет. Открытые ключи живут в памяти
//! процесса до следующей загрузки. Отзыв — своя проверка доверенной цепочки (`crl.rs`): сначала OCSP по AIA, затем CRL
//! по CDP, как CryptoAPI; ответы и списки разбирает воркер. Сеть — [`fetch_crl`] и [`post_ocsp`]: HTTP и HTTPS с
//! проверкой сертификата системы, прокси `http(s)_proxy`.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use postman_smime::p12::{Identity, P12Error};
use postman_smime::seal::SealError;
use postman_smime::sign::{SignError, own_certificate};
use postman_smime::{Anchor, Scheme, Trust};

pub(super) use super::other::{gost_decrypt, gost_provider};

/// Общие хранилища корней дистрибутивов (первое найденное): почтовые (Fedora/RHEL — отдельный набор для S/MIME),
/// затем общие TLS. `SSL_CERT_FILE` — вместо них.
const BUNDLES: &[&str] = &[
    "/etc/pki/ca-trust/extracted/pem/email-ca-bundle.pem",
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/pki/tls/certs/ca-bundle.crt",
    "/etc/ssl/ca-bundle.pem",
    "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
];

/// Корни, добавленные администратором (Debian/Ubuntu, Fedora/RHEL, Arch, openSUSE) — «корпоративные».
const ADMIN_DIRS: &[&str] =
    &["/usr/local/share/ca-certificates", "/etc/pki/ca-trust/source/anchors", "/etc/ca-certificates/trust-source/anchors", "/etc/pki/trust/anchors"];

/// Запрещённые администратором (Fedora/RHEL, Arch, openSUSE).
const DENY_DIRS: &[&str] = &["/etc/pki/ca-trust/source/blocklist", "/etc/ca-certificates/trust-source/blocklist", "/etc/pki/trust/blacklisted"];

/// Файл больше — не набор корней.
const MAX_BUNDLE: u64 = 8 << 20;

/// Сертификаты из PEM (`BEGIN CERTIFICATE`) или одного DER.
fn certificates(data: &[u8]) -> Vec<Vec<u8>> {
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";
    let Ok(text) = std::str::from_utf8(data) else {
        // Не текст — один сертификат DER.
        return if data.first() == Some(&0x30) { vec![data.to_vec()] } else { Vec::new() };
    };
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(BEGIN) {
        let body = &rest[start + BEGIN.len()..];
        let Some(end) = body.find(END) else { break };
        if let Some(der) = base64(&body[..end]) {
            out.push(der);
        }
        rest = &body[end + END.len()..];
    }
    out
}

/// Base64 (RFC 4648) с пробелами и переводами строк; ошибка — `None`.
fn base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.bytes().filter(|c| !c.is_ascii_whitespace()) {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn read_limited(path: &Path) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_BUNDLE {
        return None;
    }
    std::fs::read(path).ok()
}

/// Сертификаты всех файлов каталога (без подкаталогов).
fn dir_certificates(dir: &Path) -> Vec<Vec<u8>> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    files.sort();
    files.iter().filter_map(|p| read_limited(p)).flat_map(|d| certificates(&d)).collect()
}

fn sha256_hex(der: &[u8]) -> String {
    postman_smime::Hash::Sha256.digest(der).iter().map(|b| format!("{b:02x}")).collect()
}

/// Якоря из файлов системы: `bundle` — общий набор, `admin` — добавленные администратором, `deny` — запрещённые.
fn trust_from(bundle: Vec<Vec<u8>>, admin: Vec<Vec<u8>>, deny: Vec<Vec<u8>>, corporate_extra: &[String]) -> Trust {
    let mut roots: Vec<Anchor> = Vec::new();
    let mut size = 0usize;
    for der in admin.iter().cloned().chain(bundle) {
        if deny.contains(&der) || roots.iter().any(|a| a.der == der) || size + der.len() > postman_smime::MAX_TRUST / 2 {
            continue;
        }
        size += der.len();
        let corporate = admin.contains(&der) || (!corporate_extra.is_empty() && corporate_extra.contains(&sha256_hex(&der)));
        roots.push(Anchor { der, corporate });
    }
    let mut disallowed: Vec<Vec<u8>> = Vec::new();
    for der in deny {
        if !disallowed.contains(&der) && size + der.len() <= postman_smime::MAX_TRUST {
            size += der.len();
            disallowed.push(der);
        }
    }
    // Промежуточные — из письма (воркер): отдельного хранилища CA нет.
    Trust { roots, intermediates: Vec::new(), disallowed }
}

pub(super) fn load_trust(corporate_extra: &[String]) -> Trust {
    let from_env = std::env::var_os("SSL_CERT_FILE").map(PathBuf::from);
    let bundle = from_env
        .into_iter()
        .chain(BUNDLES.iter().map(PathBuf::from))
        .find_map(|p| read_limited(&p).map(|d| certificates(&d)).filter(|c| !c.is_empty()))
        .unwrap_or_default();
    if bundle.is_empty() {
        tracing::warn!("smime: no system root certificates found");
    }
    let admin = ADMIN_DIRS.iter().flat_map(|d| dir_certificates(Path::new(d))).collect();
    let deny = DENY_DIRS.iter().flat_map(|d| dir_certificates(Path::new(d))).collect();
    trust_from(bundle, admin, deny, corporate_extra)
}

/// Подтверждения цепочки ОС нет: отзыв проверяет `crl.rs` (см. `revocation` в `mod.rs`).
pub(super) fn confirm(_chain: &[Vec<u8>], _exclusive_root: Option<&[u8]>) -> super::Revocation {
    super::Revocation::NotChecked
}

/// Перенаправлений CRL — не больше.
const MAX_REDIRECTS: usize = 3;

/// Адрес `http(s)://узел[:порт]/путь` → TLS, узел, порт, путь.
fn split_url(url: &str) -> Result<(bool, String, u16, String), String> {
    let lower = url.get(..8).map(str::to_ascii_lowercase).unwrap_or_default();
    let (tls, rest) = if lower.starts_with("http://") {
        (false, &url[7..])
    } else if lower == "https://" {
        (true, &url[8..])
    } else {
        return Err("only http and https".into());
    };
    let (authority, path) = rest.find('/').map_or((rest, "/"), |i| (&rest[..i], &rest[i..]));
    if authority.is_empty() || authority.contains('@') || path.bytes().any(|b| b.is_ascii_control() || b == b' ') {
        return Err("bad address".into());
    }
    let default = if tls { 443 } else { 80 };
    let (host, port) = match authority.rsplit_once(':').filter(|(h, _)| !h.ends_with(']') || h.starts_with('[')) {
        Some((h, p)) if !h.contains(':') || h.starts_with('[') => (h, p.parse::<u16>().map_err(|_| "bad port".to_string())?),
        _ => (authority, default),
    };
    Ok((tls, host.trim_start_matches('[').trim_end_matches(']').to_owned(), port, path.to_owned()))
}

/// Ответ HTTP: код, заголовки (имена — строчными), тело.
type Http = (u16, Vec<(String, String)>, Vec<u8>);

/// Ответ HTTP/1.x: код, заголовки, тело (с `Transfer-Encoding: chunked`). Чужие байты сети — без паник, с пределом.
fn parse_http(resp: &[u8]) -> Result<Http, String> {
    let end = resp.windows(4).position(|w| w == b"\r\n\r\n").ok_or("no HTTP header")?;
    let head = std::str::from_utf8(&resp[..end]).map_err(|_| "bad HTTP header")?;
    let mut lines = head.split("\r\n");
    let status = lines.next().and_then(|l| l.split(' ').nth(1)).and_then(|c| c.parse::<u16>().ok()).ok_or("bad HTTP status")?;
    let headers: Vec<(String, String)> =
        lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned())).collect();
    let body = &resp[end + 4..];
    let chunked = headers.iter().any(|(k, v)| k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked"));
    if !chunked {
        let len = headers.iter().find(|(k, _)| k == "content-length").and_then(|(_, v)| v.parse::<usize>().ok());
        let body = body.get(..len.unwrap_or(body.len())).ok_or("short HTTP body")?;
        return Ok((status, headers, body.to_vec()));
    }
    let mut out = Vec::new();
    let mut rest = body;
    loop {
        let line_end = rest.windows(2).position(|w| w == b"\r\n").ok_or("bad chunk")?;
        let size = std::str::from_utf8(&rest[..line_end])
            .ok()
            .and_then(|l| usize::from_str_radix(l.split(';').next()?.trim(), 16).ok())
            .ok_or("bad chunk")?;
        if size == 0 {
            break;
        }
        let data = rest.get(line_end + 2..line_end + 2 + size).ok_or("short chunk")?;
        out.extend_from_slice(data);
        rest = rest.get(line_end + 4 + size..).ok_or("short chunk")?;
    }
    Ok((status, headers, out))
}

/// Прокси HTTP из окружения (`http_proxy`/`https_proxy`), кроме узлов из `no_proxy`.
fn proxy_for(host: &str, tls: bool) -> Option<(String, u16)> {
    let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let no = get("no_proxy").or_else(|| get("NO_PROXY")).unwrap_or_default();
    let skip = no.split(',').map(str::trim).filter(|d| !d.is_empty()).any(|d| d == "*" || host.ends_with(d.trim_start_matches('.')));
    if skip {
        return None;
    }
    let p = if tls { get("https_proxy").or_else(|| get("HTTPS_PROXY")) } else { None }.or_else(|| get("http_proxy").or_else(|| get("HTTP_PROXY")))?;
    let with_scheme = if p.contains("://") { p } else { format!("http://{p}") };
    split_url(&with_scheme).ok().filter(|(t, ..)| !t).map(|(_, h, port, _)| (h, port))
}

fn tls_config() -> Result<std::sync::Arc<rustls::ClientConfig>, String> {
    use rustls_platform_verifier::BuilderVerifierExt as _;
    let provider = std::sync::Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider).with_safe_default_protocol_versions().map_err(|e| e.to_string())?;
    Ok(std::sync::Arc::new(builder.with_platform_verifier().map_err(|e| e.to_string())?.with_no_client_auth()))
}

/// Предел ответа: CRL — до `MAX_CRL`, плюс заголовки.
const MAX_HTTP: u64 = postman_smime::crl::MAX_CRL as u64 + 64 * 1024;

/// Один запрос HTTP/1.1 (`GET` или `POST` с телом `body` типа `kind`) по `http`/`https` с прокси, до `deadline`.
fn request_once(url: &str, body: Option<(&str, &[u8])>, deadline: std::time::Instant) -> Result<Http, String> {
    use std::io::{Read, Write};
    use std::net::{TcpStream, ToSocketAddrs};
    let (tls, host, port, path) = split_url(url)?;
    let proxy = proxy_for(&host, tls);
    let (to_host, to_port) = proxy.clone().unwrap_or((host.clone(), port));
    let left = || deadline.saturating_duration_since(std::time::Instant::now()).max(std::time::Duration::from_millis(1));
    let addrs = (to_host.as_str(), to_port).to_socket_addrs().map_err(|e| format!("resolve: {}", e.kind()))?;
    let mut stream = addrs.into_iter().find_map(|a| TcpStream::connect_timeout(&a, left()).ok()).ok_or("connect failed")?;
    stream.set_read_timeout(Some(left())).map_err(|e| e.kind().to_string())?;
    stream.set_write_timeout(Some(left())).map_err(|e| e.kind().to_string())?;
    let authority = if port == if tls { 443 } else { 80 } { host.clone() } else { format!("{host}:{port}") };
    if tls && proxy.is_some() {
        // Туннель через прокси (RFC 9110 §9.3.6).
        stream
            .write_all(format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n\r\n").as_bytes())
            .map_err(|e| format!("proxy: {}", e.kind()))?;
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            if head.len() > 8192 || stream.read(&mut byte).map_err(|e| format!("proxy: {}", e.kind()))? == 0 {
                return Err("proxy: no answer".into());
            }
            head.push(byte[0]);
        }
        let (status, ..) = parse_http(&head)?;
        if status != 200 {
            return Err(format!("proxy: HTTP {status}"));
        }
    }
    let target = if proxy.is_some() && !tls { url.to_owned() } else { path };
    let mut req = match body {
        Some((kind, data)) => format!("POST {target} HTTP/1.1\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n", data.len()).into_bytes(),
        None => format!("GET {target} HTTP/1.1\r\n").into_bytes(),
    };
    req.extend_from_slice(format!("Host: {authority}\r\nAccept: */*\r\nConnection: close\r\nUser-Agent: Postman\r\n\r\n").as_bytes());
    if let Some((_, data)) = body {
        req.extend_from_slice(data);
    }
    let mut resp = Vec::new();
    if tls {
        let name = rustls::pki_types::ServerName::try_from(host).map_err(|_| "bad host name".to_string())?;
        let conn = rustls::ClientConnection::new(tls_config()?, name).map_err(|e| format!("TLS: {e}"))?;
        let mut s = rustls::StreamOwned::new(conn, stream);
        s.write_all(&req).map_err(|e| format!("send: {e}"))?;
        // Сервер может закрыть TCP без close_notify — то, что успели прочитать, разбирается как есть.
        match (&mut s).take(MAX_HTTP + 1).read_to_end(&mut resp) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && !resp.is_empty() => {}
            Err(e) => return Err(format!("receive: {e}")),
        }
    } else {
        stream.write_all(&req).map_err(|e| format!("send: {}", e.kind()))?;
        (&mut stream).take(MAX_HTTP + 1).read_to_end(&mut resp).map_err(|e| format!("receive: {}", e.kind()))?;
    }
    if resp.len() as u64 > MAX_HTTP {
        return Err("response too large".into());
    }
    parse_http(&resp)
}

/// GET или POST с перенаправлениями (`http`/`https`), не дольше `timeout`.
fn fetch(url: &str, body: Option<(&str, &[u8])>, timeout: std::time::Duration) -> Result<Vec<u8>, String> {
    let deadline = std::time::Instant::now() + timeout;
    let mut url = url.to_owned();
    for _ in 0..=MAX_REDIRECTS {
        let (status, headers, data) = request_once(&url, body, deadline)?;
        match status {
            200 => return Ok(data),
            301 | 302 | 303 | 307 | 308 => {
                url = headers.into_iter().find(|(k, _)| k == "location").map(|(_, v)| v).ok_or("redirect without Location")?;
            }
            s => return Err(format!("HTTP {s}")),
        }
    }
    Err("too many redirects".into())
}

/// Скачать CRL по адресу из сертификата доверенной цепочки (`http`, `https`), с перенаправлениями, не дольше
/// `timeout`. Байты не разбирает (CRL проверяет воркер).
pub(super) fn fetch_crl(url: &str, timeout: std::time::Duration) -> Result<Vec<u8>, String> {
    fetch(url, None, timeout)
}

/// Отправить запрос OCSP ответчику (RFC 6960 Прил. A, POST). Ответ не разбирает (проверяет воркер).
pub(super) fn post_ocsp(url: &str, request: &[u8], timeout: std::time::Duration) -> Result<Vec<u8>, String> {
    let resp = fetch(url, Some(("application/ocsp-request", request)), timeout)?;
    if resp.len() > postman_smime::ocsp::MAX_RESPONSE {
        return Err("OCSP response too large".into());
    }
    Ok(resp)
}

/// Случайный nonce запроса OCSP (16 байт, `/dev/urandom`).
pub(super) fn ocsp_nonce() -> Option<Vec<u8>> {
    use std::io::Read;
    let mut n = vec![0u8; 16];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut n)).ok().map(|_| n)
}

/// Открытые файлы PKCS#12: сертификаты с ключами в памяти процесса.
static PERSONAL: Mutex<Vec<Identity>> = Mutex::new(Vec::new());

/// Почему файл своего сертификата не открыт — для строки в настройках.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PersonalFileError {
    #[error("cannot read the file: {0}")]
    Read(String),
    #[error(transparent)]
    Pkcs12(#[from] P12Error),
}

fn open_file(path: &Path, password: &str) -> Result<Vec<Identity>, PersonalFileError> {
    let meta = std::fs::metadata(path).map_err(|e| PersonalFileError::Read(e.kind().to_string()))?;
    if meta.len() > postman_smime::p12::MAX_FILE as u64 {
        return Err(P12Error::Malformed("too large").into());
    }
    let data = zeroize::Zeroizing::new(std::fs::read(path).map_err(|e| PersonalFileError::Read(e.kind().to_string()))?);
    Ok(postman_smime::p12::parse(&data, password)?)
}

/// Проверить файл PKCS#12 паролем (настройки: «Добавить сертификат из файла»), ничего не запоминая: свои сертификаты
/// файла для списка.
pub fn check_personal_file(path: &Path, password: &str) -> Result<Vec<super::Personal>, PersonalFileError> {
    Ok(super::personal_from(open_file(path, password)?.into_iter().map(|i| i.cert).collect()))
}

/// Открыть свои файлы PKCS#12 (путь и пароль; пароль — из хранилища паролей, не с диска) и держать ключи в памяти
/// вместо прежних. Итог — по файлу: число сертификатов с ключом или ошибка (файл пропускается, остальные работают).
pub fn load_personal_files(files: &[(PathBuf, zeroize::Zeroizing<String>)]) -> Vec<Result<usize, PersonalFileError>> {
    let mut all = Vec::new();
    let mut out = Vec::new();
    for (path, password) in files {
        match open_file(path, password) {
            Ok(ids) => {
                out.push(Ok(ids.len()));
                all.extend(ids);
            }
            Err(e) => {
                tracing::warn!("smime: personal certificate file not opened: {e}");
                out.push(Err(e));
            }
        }
    }
    *PERSONAL.lock().unwrap_or_else(PoisonError::into_inner) = all;
    out
}

/// Забыть ключи (выход, смена настроек).
pub fn clear_personal_files() {
    PERSONAL.lock().unwrap_or_else(PoisonError::into_inner).clear();
}

fn with_personal<T>(f: impl FnOnce(&[Identity]) -> T) -> T {
    f(&PERSONAL.lock().unwrap_or_else(PoisonError::into_inner))
}

/// Свои сертификаты с ключом (для списка в настройках).
pub(super) fn personal() -> Vec<Vec<u8>> {
    with_personal(|ids| ids.iter().map(|i| i.cert.clone()).collect())
}

/// Свои сертификаты для адреса `from` — кандидаты «себе»; проверяет воркер.
pub(super) fn own_certs(from: &str) -> Vec<Vec<u8>> {
    personal()
        .into_iter()
        .filter(|der| own_certificate(der).is_some_and(|(info, _, _)| info.emails.iter().any(|e| e.eq_ignore_ascii_case(from.trim()))))
        .collect()
}

/// Лучший свой сертификат для подписи `from`: годный, действующий, RSA или ECDSA, самый поздний срок.
fn signing(ids: &[Identity], from: &str, now: i64) -> Option<(usize, Scheme)> {
    let mut best: Option<(usize, Scheme, i64)> = None;
    for (n, id) in ids.iter().enumerate() {
        let Some((info, can, Some(scheme))) = own_certificate(&id.cert) else { continue };
        let mine = info.emails.iter().any(|e| e.eq_ignore_ascii_case(from.trim()));
        let valid = info.not_before <= now && now <= info.not_after;
        if can && mine && valid && matches!(scheme, Scheme::RsaPkcs1(_) | Scheme::Ecdsa(_)) && best.is_none_or(|b| info.not_after > b.2) {
            best = Some((n, scheme, info.not_after));
        }
    }
    best.map(|(n, s, _)| (n, s))
}

pub(super) fn can_sign(from: &str, now: i64) -> bool {
    with_personal(|ids| signing(ids, from, now).is_some())
}

pub(super) fn sign_message(raw: &[u8], from: &str, now: i64) -> Result<Vec<u8>, SignError> {
    with_personal(|ids| {
        let (n, scheme) = signing(ids, from, now).ok_or_else(|| SignError::NoCertificate(from.into()))?;
        let signer = ids.get(n).map(|id| id.signer(scheme)).ok_or_else(|| SignError::NoCertificate(from.into()))?;
        postman_smime::sign::sign_message(raw, &signer, now)
    })
}

/// Снять ключ содержимого своими ключами из файлов.
pub(super) fn unwrap_cek(
    env: &postman_smime::envelope::Envelope,
    _prompt: super::keys::Prompt,
) -> Result<postman_smime::envelope::Cek, super::keys::KeyError> {
    use super::keys::KeyError;
    use postman_smime::soft::SoftError;
    let keys: Vec<_> = with_personal(|ids| ids.iter().filter_map(Identity::soft_key).collect());
    match postman_smime::soft::unwrap(&keys, env) {
        Ok(cek) => Ok(cek),
        Err(SoftError::NotForYou) => Err(KeyError::NotForYou(super::keys::labels(env))),
        Err(SoftError::Unsupported(name)) => Err(KeyError::Unsupported(name)),
    }
}

/// Случайные числа — `/dev/urandom`; эфемерный ECDH (получатели с ключом EC P-256/P-384) — `p256`/`p384` с ключом
/// из тех же случайных чисел.
pub(super) struct LinuxOps {
    random: std::fs::File,
}

impl LinuxOps {
    pub(super) fn new() -> Result<LinuxOps, SealError> {
        let random = std::fs::File::open("/dev/urandom").map_err(|e| SealError::Failed(format!("/dev/urandom: {}", e.kind())))?;
        Ok(LinuxOps { random })
    }
}

impl postman_smime::seal::Ops for LinuxOps {
    fn random(&self, out: &mut [u8]) -> Result<(), SealError> {
        use std::io::Read;
        (&self.random).read_exact(out).map_err(|e| {
            out.fill(0);
            tracing::error!("smime: system random source failed: {}", e.kind());
            SealError::Failed("no system random source".into())
        })
    }

    fn ecdh(&self, bits: u16, peer: &[u8]) -> Result<(Vec<u8>, zeroize::Zeroizing<Vec<u8>>), SealError> {
        let mut seed = zeroize::Zeroizing::new(vec![0u8; if bits == 384 { 48 } else { 32 }]);
        // Байты вне порядка кривой — вероятность ~2^-32 и меньше; несколько попыток, затем отказ.
        for _ in 0..4 {
            self.random(&mut seed)?;
            if let Some(r) = postman_smime::soft::ecdh_ephemeral(bits, &seed, peer) {
                return Ok(r);
            }
        }
        Err(SealError::Failed("ECDH".into()))
    }
}

pub(super) fn seal(raw: &[u8], to: &[postman_smime::seal::To]) -> Result<Vec<u8>, SealError> {
    postman_smime::seal::seal_message(raw, to, &LinuxOps::new()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AES: &[u8] = include_bytes!("../../../smime/src/p12/aes.p12");

    #[test]
    fn splits_crl_addresses() {
        assert_eq!(
            split_url("http://pki.corp.example/CertEnroll/ca.crl").unwrap(),
            (false, "pki.corp.example".into(), 80, "/CertEnroll/ca.crl".into())
        );
        assert_eq!(split_url("HTTP://h:8080").unwrap(), (false, "h".into(), 8080, "/".into()));
        assert_eq!(split_url("http://[::1]:81/x").unwrap(), (false, "::1".into(), 81, "/x".into()));
        assert_eq!(split_url("https://h/x").unwrap(), (true, "h".into(), 443, "/x".into()));
        assert!(split_url("ldap://h/x").is_err());
        assert!(split_url("http://u@h/x").is_err());
        assert!(split_url("http://h/a b").is_err());
    }

    #[test]
    fn parses_http_responses() {
        let (s, _, b) = parse_http(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabcdef").unwrap();
        assert_eq!((s, b.as_slice()), (200, b"abc".as_slice()));
        let (_, _, b) = parse_http(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;x\r\nde\r\n0\r\n\r\n").unwrap();
        assert_eq!(b, b"abcde");
        let (s, h, _) = parse_http(b"HTTP/1.1 302 Found\r\nLocation: http://x/y\r\n\r\n").unwrap();
        assert_eq!((s, h[0].1.as_str()), (302, "http://x/y"));
        for bad in [
            &b"garbage"[..],
            b"HTTP/1.1 200\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n",
            b"HTTP/1.1 200\r\nTransfer-Encoding: chunked\r\n\r\nff\r\nab",
        ] {
            assert!(parse_http(bad).is_err());
        }
    }

    /// Загрузка с локального сервера: перенаправление, затем CRL.
    #[test]
    fn fetches_crl_over_http() {
        use std::io::{Read, Write};
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            for reply in [
                format!("HTTP/1.1 301 Moved\r\nLocation: http://127.0.0.1:{port}/b.crl\r\nContent-Length: 0\r\n\r\n"),
                "HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nCRL!".into(),
            ] {
                let (mut c, _) = server.accept().unwrap();
                let mut buf = [0u8; 1024];
                let _ = c.read(&mut buf).unwrap();
                c.write_all(reply.as_bytes()).unwrap();
            }
        });
        // SAFETY (окружение): прокси не должен перехватить локальный адрес; nextest — отдельный процесс на тест.
        unsafe {
            std::env::set_var("no_proxy", "127.0.0.1");
        }
        assert_eq!(fetch_crl(&format!("http://127.0.0.1:{port}/a.crl"), std::time::Duration::from_secs(5)).unwrap(), b"CRL!");
        t.join().unwrap();
    }

    /// OCSP: POST с телом и типом `application/ocsp-request`, ответ — как есть.
    #[test]
    fn posts_ocsp_requests() {
        use std::io::{Read, Write};
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut c, _) = server.accept().unwrap();
            let mut got = Vec::new();
            let mut buf = [0u8; 1024];
            while !got.ends_with(b"REQ!") {
                let n = c.read(&mut buf).unwrap();
                assert!(n > 0);
                got.extend_from_slice(&buf[..n]);
            }
            c.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/ocsp-response\r\nContent-Length: 5\r\n\r\nRESP!").unwrap();
            String::from_utf8_lossy(&got).into_owned()
        });
        // SAFETY (окружение): как в `fetches_crl_over_http`.
        unsafe {
            std::env::set_var("no_proxy", "127.0.0.1");
        }
        assert_eq!(post_ocsp(&format!("http://127.0.0.1:{port}/"), b"REQ!", std::time::Duration::from_secs(5)).unwrap(), b"RESP!");
        let head = t.join().unwrap();
        assert!(head.starts_with("POST / HTTP/1.1\r\n"), "{head}");
        assert!(head.contains("Content-Type: application/ocsp-request\r\n") && head.contains("Content-Length: 4\r\n"), "{head}");
        assert_eq!(ocsp_nonce().map(|n| n.len()), Some(16));
    }

    /// `https`: начинается рукопожатие TLS; сервер без TLS — ошибка, не паника и не открытый текст.
    #[test]
    fn https_uses_tls() {
        use std::io::{Read, Write};
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut c, _) = server.accept().unwrap();
            let mut first = [0u8; 1];
            c.read_exact(&mut first).unwrap();
            c.write_all(b"HTTP/1.1 200 OK\r\n\r\nplain").unwrap();
            first[0]
        });
        // SAFETY (окружение): как в `fetches_crl_over_http`.
        unsafe {
            std::env::set_var("no_proxy", "127.0.0.1");
        }
        assert!(fetch_crl(&format!("https://127.0.0.1:{port}/a.crl"), std::time::Duration::from_secs(5)).is_err());
        assert_eq!(t.join().unwrap(), 0x16, "запись TLS handshake");
    }

    #[test]
    fn reads_pem_bundles_and_der() {
        let pem = b"junk\n-----BEGIN CERTIFICATE-----\nMIIB\nAAA=\n-----END CERTIFICATE-----\n-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----\n";
        assert_eq!(certificates(pem), vec![vec![0x30, 0x82, 0x01, 0x00, 0x00], vec![1, 2, 3]]);
        assert_eq!(certificates(&[0x30, 0x82, 0xFF]), vec![vec![0x30, 0x82, 0xFF]]);
        assert!(certificates(b"-----BEGIN CERTIFICATE-----\n#$%\n-----END CERTIFICATE-----").is_empty());
    }

    #[test]
    fn admin_roots_are_corporate_and_denied_are_out() {
        let (a, b, c) = (vec![0x30, 1], vec![0x30, 2], vec![0x30, 3]);
        let t = trust_from(vec![a.clone(), b.clone(), c.clone()], vec![b.clone()], vec![c.clone()], &[sha256_hex(&a)]);
        assert_eq!(t.roots, vec![Anchor { der: b, corporate: true }, Anchor { der: a, corporate: true }]);
        assert_eq!(t.disallowed, vec![c]);
    }

    /// Файл PKCS#12 → свой сертификат в памяти: можно подписать письмо адреса из сертификата, кандидаты «себе» — он.
    #[test]
    fn personal_file_round_trip() {
        let path = std::env::temp_dir().join(format!("postman-p12-{}.p12", std::process::id()));
        std::fs::write(&path, AES).unwrap();
        assert!(matches!(check_personal_file(&path, "x"), Err(PersonalFileError::Pkcs12(P12Error::BadPassword))));
        assert_eq!(check_personal_file(&path, "тест-пароль").unwrap().len(), 1);
        let loaded = load_personal_files(&[(path.clone(), zeroize::Zeroizing::new("тест-пароль".into()))]);
        let _ = std::fs::remove_file(&path);
        assert_eq!(loaded, vec![Ok(1)]);
        let now = 1_900_000_000;
        assert!(can_sign("User@Example.test", now));
        assert!(!can_sign("other@example.test", now));
        assert_eq!(own_certs("user@example.test").len(), 1);
        let raw = b"Content-Type: text/plain\r\n\r\nhello\r\n";
        let signed = sign_message(raw, "user@example.test", now).unwrap();
        assert!(signed.len() > raw.len());
        clear_personal_files();
        assert!(!can_sign("user@example.test", now));
    }
}
