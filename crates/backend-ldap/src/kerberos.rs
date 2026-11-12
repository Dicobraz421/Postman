//! Вход учётной записью на macOS (MAC-05) — SASL GSSAPI (RFC 4752) своими силами. `ldap3` на macOS собран без
//! `gssapi`: его `cross-krb5` тянет `libgssapi` с `iov`, а в GSS.framework нет `gss_wrap_iov*`; общего SASL-вызова у
//! `ldap3` нет.
//!
//! Как устроено: соединение с сервером открываем сами (TCP; TLS — `ldaps://` или StartTLS, с привязкой к каналу),
//! проводим обмен SASL GSSAPI (контекст — GSS.framework, `sys`), а `ldap3` получает локальную пару сокетов
//! (`ldapi:///`). Мост между ней и сервером — как есть по TLS или со слоем защиты Kerberos по `ldap://` (буферы SASL,
//! `gss_wrap` с шифрованием) — так же, как `ldap3` делает сам на Windows и Linux. Поиск и разбор ответа — общие.
//!
//! Обмен, StartTLS, привязка к каналу и мост от платформы не зависят и проверяются тестами на подменном контексте GSS
//! (`kerberos/tests.rs`). Ответы сервера — чужие данные: разбор без паник, размеры ограничены.

#[cfg(test)]
mod tests;

use std::sync::Mutex;

use ldap3::asn1::{StructureTag, TagClass, parse_tag};
use postman_core::smime::directory::DirectoryError;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Шаг установления контекста GSS-API: токен для сервера (может быть пустым) и готов ли контекст.
pub(crate) struct GssStep {
    pub(crate) token: Vec<u8>,
    pub(crate) done: bool,
}

/// Контекст клиента GSS-API (Kerberos): GSS.framework в `sys`, в тестах — подменный.
pub(crate) trait Gss: Send + 'static {
    /// Шаг обмена: `None` — первый, иначе токен сервера.
    fn step(&mut self, input: Option<&[u8]>) -> Result<GssStep, String>;
    /// `gss_wrap`: `encrypt` — с шифрованием (иначе только подпись).
    fn wrap(&mut self, encrypt: bool, data: &[u8]) -> Result<Vec<u8>, String>;
    /// `gss_unwrap`.
    fn unwrap(&mut self, data: &[u8]) -> Result<Vec<u8>, String>;
    /// `gss_wrap_size_limit` с шифрованием: наибольший открытый текст, чей токен не длиннее `max_token`.
    fn wrap_limit(&mut self, max_token: u32) -> Result<u32, String>;
}

/// Код LDAP «привязка SASL продолжается» (RFC 4511).
const SASL_BIND_IN_PROGRESS: u32 = 14;
/// Слои защиты SASL GSSAPI (RFC 4752, 3.3): без слоя (поверх TLS) и с шифрованием (`ldap://`).
pub(crate) const LAYER_NONE: u8 = 1;
pub(crate) const LAYER_PRIVACY: u8 = 4;
/// Наибольший буфер SASL, который принимаем от сервера, — как у OpenLDAP и `ldap3` (≈10 МиБ).
pub(crate) const RECV_MAX: u32 = 0x9F_FFB8;
/// Ответ на привязку и StartTLS — не больше (в нём только токен Kerberos).
const RESPONSE_MAX: usize = 1 << 20;
/// Шагов обмена — не больше: Kerberos укладывается в два-три.
const ROUNDS: usize = 8;
/// Открытый текст одного буфера SASL к серверу — не больше.
const CHUNK_MAX: usize = 64 * 1024;
/// Операции ответа LDAP (`[APPLICATION n]`).
const BIND_RESPONSE: u64 = 1;
const EXTENDED_RESPONSE: u64 = 24;
const STARTTLS_OID: &[u8] = b"1.3.6.1.4.1.1466.20037";

fn unavailable(e: impl std::fmt::Display) -> DirectoryError {
    DirectoryError::Unavailable(e.to_string())
}

/// TLV BER: тег, длина (короткая или длинная форма), тело.
pub(crate) fn tlv(tag: u8, body: &[u8]) -> Vec<u8> {
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

/// INTEGER BER (неотрицательный).
fn int(v: u32) -> Vec<u8> {
    let mut b: Vec<u8> = v.to_be_bytes().into_iter().skip_while(|b| *b == 0).collect();
    if b.first().is_none_or(|f| f & 0x80 != 0) {
        b.insert(0, 0);
    }
    tlv(0x02, &b)
}

/// LDAPMessage: номер и операция.
fn message(id: u32, op: &[u8]) -> Vec<u8> {
    let mut m = int(id);
    m.extend_from_slice(op);
    tlv(0x30, &m)
}

/// BindRequest с SASL `GSSAPI` (RFC 4511, 4.2): версия 3, пустое имя, `creds` — токен (`None` — без поля).
pub(crate) fn bind_request(id: u32, creds: Option<&[u8]>) -> Vec<u8> {
    let mut sasl = tlv(0x04, b"GSSAPI");
    if let Some(c) = creds {
        sasl.extend(tlv(0x04, c));
    }
    let mut bind = int(3);
    bind.extend(tlv(0x04, b""));
    bind.extend(tlv(0xA3, &sasl));
    message(id, &tlv(0x60, &bind))
}

/// ExtendedRequest StartTLS (RFC 4511, 4.14).
pub(crate) fn starttls_request(id: u32) -> Vec<u8> {
    message(id, &tlv(0x77, &tlv(0x80, STARTTLS_OID)))
}

/// Ответ сервера на привязку или StartTLS.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Response {
    pub(crate) rc: u32,
    pub(crate) text: String,
    /// `serverSaslCreds` (`[7]` у BindResponse).
    pub(crate) creds: Option<Vec<u8>>,
}

/// Одно сообщение LDAP целиком (SEQUENCE BER) — не длиннее `RESPONSE_MAX`.
pub(crate) async fn read_message<S: AsyncRead + Unpin>(io: &mut S) -> Result<Vec<u8>, DirectoryError> {
    let mut head = [0u8; 2];
    io.read_exact(&mut head).await.map_err(unavailable)?;
    if head[0] != 0x30 {
        return Err(unavailable("bad LDAP message"));
    }
    let mut msg = head.to_vec();
    let len = if head[1] < 0x80 {
        usize::from(head[1])
    } else {
        let n = usize::from(head[1] & 0x7F);
        if n == 0 || n > 4 {
            return Err(unavailable("bad LDAP message length"));
        }
        let mut b = [0u8; 4];
        io.read_exact(&mut b[4 - n..]).await.map_err(unavailable)?;
        msg.extend_from_slice(&b[4 - n..]);
        usize::try_from(u32::from_be_bytes(b)).map_err(unavailable)?
    };
    if len > RESPONSE_MAX {
        return Err(unavailable("LDAP response too large"));
    }
    let start = msg.len();
    msg.resize(start + len, 0);
    io.read_exact(&mut msg[start..]).await.map_err(unavailable)?;
    Ok(msg)
}

/// Целое без знака из тела INTEGER/ENUMERATED BER.
fn uint(bytes: &[u8]) -> Option<u32> {
    let bytes = match bytes {
        [0, rest @ ..] => rest,
        [f, ..] if f & 0x80 != 0 => return None,
        b => b,
    };
    if bytes.len() > 4 {
        return None;
    }
    Some(bytes.iter().fold(0u32, |v, b| (v << 8) | u32::from(*b)))
}

/// Разбор ответа `op` на запрос `id`: код, текст, `serverSaslCreds`. Уведомление об отключении (номер 0) — ошибка.
pub(crate) fn parse_response(msg: &[u8], id: u32, op: u64) -> Result<Response, DirectoryError> {
    let bad = || unavailable("bad LDAP response");
    let (_, tag) = parse_tag(msg).map_err(|_| bad())?;
    let mut parts =
        tag.match_class(TagClass::Universal).and_then(|t| t.match_id(16)).and_then(StructureTag::expect_constructed).ok_or_else(bad)?.into_iter();
    let got = parts.next().and_then(StructureTag::expect_primitive).as_deref().and_then(uint).ok_or_else(bad)?;
    let body = parts.next().and_then(|t| t.match_class(TagClass::Application)).ok_or_else(bad)?;
    let (got_op, fields) = (body.id, body.expect_constructed().ok_or_else(bad)?);
    let mut fields = fields.into_iter();
    let rc = fields.next().and_then(StructureTag::expect_primitive).as_deref().and_then(uint).ok_or_else(bad)?;
    let _matched = fields.next();
    let text = fields.next().and_then(StructureTag::expect_primitive).map(|t| String::from_utf8_lossy(&t).into_owned()).unwrap_or_default();
    if got == 0 {
        return Err(unavailable(format!("server closed the connection: rc={rc} {text}")));
    }
    if got != id || got_op != op {
        return Err(bad());
    }
    let creds = fields.find(|t| t.class == TagClass::Context && t.id == 7).and_then(StructureTag::expect_primitive);
    Ok(Response { rc, text, creds })
}

/// Запрос и ответ: номер — следующий за `id`.
async fn call<S>(io: &mut S, id: &mut u32, request: impl FnOnce(u32) -> Vec<u8>, op: u64) -> Result<Response, DirectoryError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    *id += 1;
    io.write_all(&request(*id)).await.map_err(unavailable)?;
    io.flush().await.map_err(unavailable)?;
    let msg = read_message(io).await?;
    parse_response(&msg, *id, op)
}

/// StartTLS по открытому соединению; TLS поднимает вызывающий.
pub(crate) async fn starttls<S>(io: &mut S, id: &mut u32) -> Result<(), DirectoryError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let r = call(io, id, starttls_request, EXTENDED_RESPONSE).await?;
    if r.rc != 0 {
        return Err(unavailable(format!("StartTLS failed: rc={} {}", r.rc, r.text)));
    }
    Ok(())
}

fn failed(r: &Response) -> DirectoryError {
    if r.rc == 0 {
        unavailable("GSSAPI bind finished without security layer negotiation")
    } else {
        unavailable(format!("GSSAPI bind failed: rc={} {}", r.rc, r.text))
    }
}

/// Ответ клиента о слое защиты (RFC 4752, 3.3): слой и наибольший буфер, который примем (без слоя — 0).
pub(crate) fn layer_answer(layer: u8) -> [u8; 4] {
    let max = if layer == LAYER_NONE { 0 } else { RECV_MAX };
    let m = max.to_be_bytes();
    [layer, m[1], m[2], m[3]]
}

/// Привязка SASL GSSAPI (RFC 4752): обмен токенами до готовности контекста, затем выбор слоя защиты — без слоя
/// поверх TLS, иначе шифрование. Ответ — наибольший буфер сервера, если слой — шифрование.
pub(crate) async fn bind<S, G>(io: &mut S, gss: &mut G, tls: bool, id: &mut u32) -> Result<Option<u32>, DirectoryError>
where
    S: AsyncRead + AsyncWrite + Unpin,
    G: Gss,
{
    let mut step = gss.step(None).map_err(unavailable)?;
    let mut creds = Some(std::mem::take(&mut step.token));
    let mut rounds = 0;
    let offer = loop {
        rounds += 1;
        if rounds > ROUNDS {
            return Err(unavailable("GSSAPI bind: too many rounds"));
        }
        let r = call(io, id, |id| bind_request(id, creds.as_deref()), BIND_RESPONSE).await?;
        if r.rc != SASL_BIND_IN_PROGRESS {
            return Err(failed(&r));
        }
        let token = r.creds.unwrap_or_default();
        if step.done {
            break token;
        }
        step = gss.step(Some(&token)).map_err(unavailable)?;
        creds = (!step.token.is_empty()).then(|| std::mem::take(&mut step.token));
    };
    let offer = gss.unwrap(&offer).map_err(unavailable)?;
    let &[mask, a, b, c] = offer.as_slice() else {
        return Err(unavailable("bad GSSAPI security layer offer"));
    };
    let layer = if tls { LAYER_NONE } else { LAYER_PRIVACY };
    if mask & layer == 0 {
        return Err(unavailable(format!("no suitable GSSAPI security layer: needed {layer}, offered {mask}")));
    }
    let wrapped = gss.wrap(false, &layer_answer(layer)).map_err(unavailable)?;
    let r = call(io, id, |id| bind_request(id, Some(&wrapped)), BIND_RESPONSE).await?;
    if r.rc != 0 {
        return Err(failed(&r));
    }
    Ok((layer == LAYER_PRIVACY).then_some(u32::from_be_bytes([0, a, b, c])))
}

/// Привязка к каналу TLS (RFC 5929 `tls-server-end-point`) — как у `ldap3` на Windows и Linux: хеш сертификата
/// сервера по алгоритму его подписи (MD5 и SHA-1 — SHA-256). Неизвестный алгоритм — без привязки.
pub(crate) fn channel_binding(cert: &[u8]) -> Option<Vec<u8>> {
    use sha2::Digest as _;
    // OID подписи (DER, без тега и длины): RSA с MD5, SHA-1, SHA-256/384/512; ECDSA с SHA-256/384/512.
    const RSA: [u8; 8] = [0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01];
    const ECDSA: [u8; 7] = [0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03];
    let (_, tag) = parse_tag(cert).ok()?;
    let alg = tag.expect_constructed()?.into_iter().nth(1)?.expect_constructed()?.into_iter().next()?.expect_primitive()?;
    let hash = match (alg.split_last()?, alg.len()) {
        ((4 | 5 | 11, head), 9) if head == RSA => sha2::Sha256::digest(cert).to_vec(),
        ((12, head), 9) if head == RSA => sha2::Sha384::digest(cert).to_vec(),
        ((13, head), 9) if head == RSA => sha2::Sha512::digest(cert).to_vec(),
        ((2, head), 8) if head == ECDSA => sha2::Sha256::digest(cert).to_vec(),
        ((3, head), 8) if head == ECDSA => sha2::Sha384::digest(cert).to_vec(),
        ((4, head), 8) if head == ECDSA => sha2::Sha512::digest(cert).to_vec(),
        _ => {
            tracing::warn!("no TLS channel binding: unknown certificate signature algorithm");
            return None;
        }
    };
    let mut out = b"tls-server-end-point:".to_vec();
    out.extend(hash);
    Some(out)
}

fn lock<G>(gss: &Mutex<G>) -> Result<std::sync::MutexGuard<'_, G>, String> {
    gss.lock().map_err(|_| "GSS-API context lock poisoned".to_owned())
}

/// Буфер SASL: длина (4 байта, BE) и токен.
pub(crate) fn frame(token: &[u8]) -> Result<Vec<u8>, String> {
    let len = u32::try_from(token.len()).map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(token.len() + 4);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(token);
    Ok(out)
}

/// К серверу: открытый текст `ldap3` — буферами SASL с шифрованием, не длиннее того, что сервер примет.
async fn seal<R, W, G>(from: &mut R, to: &mut W, gss: &Mutex<G>, send_max: u32) -> Result<(), String>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    G: Gss,
{
    // 0 — сервер не назвал предела: берём свой.
    let max = if send_max == 0 { RECV_MAX } else { send_max };
    let limit = lock(gss)?.wrap_limit(max)?;
    let chunk = usize::try_from(limit).map_err(|e| e.to_string())?.min(CHUNK_MAX);
    if chunk == 0 {
        return Err("GSS-API wrap size limit is zero".into());
    }
    let mut buf = vec![0u8; chunk];
    loop {
        let n = from.read(&mut buf).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return to.shutdown().await.map_err(|e| e.to_string());
        }
        let token = lock(gss)?.wrap(true, &buf[..n])?;
        to.write_all(&frame(&token)?).await.map_err(|e| e.to_string())?;
        to.flush().await.map_err(|e| e.to_string())?;
    }
}

/// От сервера: буферы SASL — расшифровать и отдать `ldap3`.
async fn open<R, W, G>(from: &mut R, to: &mut W, gss: &Mutex<G>) -> Result<(), String>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    G: Gss,
{
    loop {
        let mut len = [0u8; 4];
        match from.read_exact(&mut len).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return to.shutdown().await.map_err(|e| e.to_string()),
            Err(e) => return Err(e.to_string()),
        }
        let len = u32::from_be_bytes(len);
        if len == 0 || len > RECV_MAX {
            return Err(format!("bad SASL buffer length {len}"));
        }
        let mut token = vec![0u8; usize::try_from(len).map_err(|e| e.to_string())?];
        from.read_exact(&mut token).await.map_err(|e| e.to_string())?;
        let plain = lock(gss)?.unwrap(&token)?;
        to.write_all(&plain).await.map_err(|e| e.to_string())?;
        to.flush().await.map_err(|e| e.to_string())?;
    }
}

/// Мост `ldap3` (`local`) ↔ сервер (`remote`): как есть или со слоем шифрования (`layer` — контекст и наибольший
/// буфер сервера). Кончается, когда закрыта любая сторона; ошибка — в журнал, соединение закрывается.
pub(crate) async fn relay<L, R, G>(mut local: L, mut remote: R, layer: Option<(G, u32)>)
where
    L: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    R: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    G: Gss,
{
    let Some((gss, send_max)) = layer else {
        if let Err(e) = tokio::io::copy_bidirectional(&mut local, &mut remote).await {
            tracing::debug!("ldap relay closed: {e}");
        }
        return;
    };
    let gss = std::sync::Arc::new(Mutex::new(gss));
    let (mut local_rx, mut local_tx) = tokio::io::split(local);
    let (mut remote_rx, mut remote_tx) = tokio::io::split(remote);
    let up = gss.clone();
    let sealing = tokio::spawn(async move {
        if let Err(e) = seal(&mut local_rx, &mut remote_tx, &up, send_max).await {
            tracing::warn!("ldap relay to server: {e}");
        }
    });
    if let Err(e) = open(&mut remote_rx, &mut local_tx, &gss).await {
        tracing::warn!("ldap relay from server: {e}");
    }
    sealing.abort();
}

/// Поток к серверу: TCP или TLS поверх него.
#[cfg(target_os = "macos")]
trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
#[cfg(target_os = "macos")]
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// Подключиться к `target`, войти Kerberos (GSS.framework) и отдать `ldap3` готовое соединение.
#[cfg(target_os = "macos")]
pub(crate) async fn connect(settings: &crate::Settings, target: &crate::Target) -> Result<ldap3::Ldap, DirectoryError> {
    use ldap3::{LdapConnAsync, LdapConnSettings, StdStream};

    let mut tcp = tokio::net::TcpStream::connect(target.socket_addr()).await.map_err(unavailable)?;
    let mut id = 0;
    let tls = target.tls || settings.starttls;
    let (mut remote, binding): (Box<dyn Io>, Option<Vec<u8>>) = if tls {
        if !target.tls {
            starttls(&mut tcp, &mut id).await?;
        }
        let name = rustls::pki_types::ServerName::try_from(target.host.clone()).map_err(unavailable)?;
        let stream = tokio_rustls::TlsConnector::from(crate::tls_config()?).connect(name, tcp).await.map_err(unavailable)?;
        let binding = stream.get_ref().1.peer_certificates().and_then(|c| c.first()).and_then(|c| channel_binding(c));
        (Box::new(stream), binding)
    } else {
        (Box::new(tcp), None)
    };
    let mut gss = crate::sys::Kerberos::new(&target.host, binding.as_deref()).map_err(unavailable)?;
    let layer = bind(&mut remote, &mut gss, tls, &mut id).await?;
    let (ours, theirs) = std::os::unix::net::UnixStream::pair().map_err(unavailable)?;
    ours.set_nonblocking(true).map_err(unavailable)?;
    let ours = tokio::net::UnixStream::from_std(ours).map_err(unavailable)?;
    tokio::spawn(relay(ours, remote, layer.map(|max| (gss, max))));
    let conn = LdapConnSettings::new().set_std_stream(StdStream::Unix(theirs));
    let (conn, ldap) = LdapConnAsync::with_settings(conn, "ldapi:///").await.map_err(unavailable)?;
    ldap3::drive!(conn);
    Ok(ldap)
}
