//! HTTP/1.1 к серверу Exchange одним соединением TLS: NTLM (и Kerberos у IIS) привязан к TCP — всё рукопожатие
//! и следующие запросы идут по нему. Вход — по ответу 401: схема из `WWW-Authenticate`, токены — SSPI (`sys`) или
//! NTLM с паролем (`ntlm`),
//! с привязкой к каналу TLS. Соединение закрылось — новое и вход заново.
//!
//! Поток (`GetStreamingEvents`, T1-21а) — ответ, который сервер пишет кусками до получаса: [`Http::post_stream`]
//! отдаёт тело как есть и забирает соединение с собой, следующий запрос клиента откроет новое.

use std::time::Instant;

use http_body_util::{BodyExt as _, Full, Limited};
use hyper::body::{Bytes, Incoming};
use hyper::client::conn::http1::SendRequest;
use hyper::header;
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;

use crate::auth::{self, Scheme};
use crate::{Auth, CREDENTIALS_REJECTED, Error, MIN_INTERVAL, Result, TIMEOUT};

/// Ответ больше — ошибка (пачка в 20 встреч с описаниями — сотни КБ).
const MAX_BODY: usize = 16 << 20;
/// Шагов рукопожатия: NTLM — 2, Kerberos — 1–2.
const MAX_ROUNDS: usize = 4;

pub(crate) struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
    pub location: Option<String>,
    www_authenticate: Vec<String>,
}

struct Conn {
    host: String,
    port: u16,
    sender: SendRequest<Full<Bytes>>,
    /// `tls-server-end-point` сертификата сервера.
    binding: Option<Vec<u8>>,
}

pub(crate) struct Http {
    auth: Auth,
    conn: Option<Conn>,
    last: Option<Instant>,
    pub(crate) min_interval: std::time::Duration,
    /// Предел на запрос целиком (с соединением и входом).
    pub(crate) timeout: std::time::Duration,
    /// Схема последнего удачного входа (`Negotiate`, `NTLM`).
    pub(crate) signed_in: Option<&'static str>,
}

/// Только TLS 1.3 («Настройки → Безопасность», S5-30), как у JMAP и DAV; иначе — 1.2 и выше. Новые соединения.
static TLS13_ONLY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_tls13_only(on: bool) {
    TLS13_ONLY.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// `POSTMAN_INSECURE_TLS=1` — не проверять сертификат сервера (как у JMAP и IMAP). Только для стенда; у рабочего
/// сервера сертификат проверяется всегда.
fn insecure_tls() -> bool {
    std::env::var("POSTMAN_INSECURE_TLS").is_ok_and(|v| v == "1")
}

/// TLS — SChannel Windows (`native-tls`), как у Outlook: шифры и протоколы — политика Windows, сертификат проверяет
/// хранилище Windows. rustls здесь не годится: Exchange компании (IIS) принимает ECDHE только с
/// AES-CBC (`ECDHE-RSA-AES256-SHA384`), а GCM — только без ECDHE; rustls не умеет ни того ни другого, и SChannel
/// сервера рвёт соединение на ClientHello (TCP RST) — так на боевом Exchange (S7-06б, `docs/EXCHANGE.md`).
fn tls_connector() -> Result<tokio_native_tls::TlsConnector> {
    let mut b = native_tls::TlsConnector::builder();
    let tls13 = TLS13_ONLY.load(std::sync::atomic::Ordering::Relaxed);
    b.min_protocol_version(Some(if tls13 { native_tls::Protocol::Tlsv13 } else { native_tls::Protocol::Tlsv12 }));
    if insecure_tls() {
        tracing::warn!("POSTMAN_INSECURE_TLS: server certificate is NOT verified (test stand only)");
        b.danger_accept_invalid_certs(true);
    }
    let c = b.build().map_err(|e| Error::Tls(e.to_string()))?;
    Ok(tokio_native_tls::TlsConnector::from(c))
}

fn hyper_err(e: hyper::Error) -> Error {
    if e.is_closed() || e.is_canceled() || e.is_incomplete_message() { Error::Closed } else { Error::Http(e.to_string()) }
}

async fn connect(host: &str, port: u16, plain: bool) -> Result<Conn> {
    let tcp = TcpStream::connect((host, port)).await?;
    tcp.set_nodelay(true)?;
    if plain {
        // Только сборка с `testkit` (подменный сервер тестов): без TLS и без привязки к каналу.
        let (sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tcp)).await.map_err(hyper_err)?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        return Ok(Conn { host: host.to_owned(), port, sender, binding: None });
    }
    let tls = tls_connector()?.connect(host, tcp).await.map_err(|e| Error::Tls(e.to_string()))?;
    let binding = match tls.get_ref().peer_certificate() {
        Ok(Some(cert)) => cert.to_der().ok().map(|der| auth::channel_binding(&der)),
        _ => None,
    };
    let (sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tls)).await.map_err(hyper_err)?;
    // Соединение живёт, пока жив `sender`; закрылось — следующий запрос откроет новое.
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            tracing::debug!(error = %e, "ews connection closed");
        }
    });
    Ok(Conn { host: host.to_owned(), port, sender, binding })
}

/// Тело ответа-потока: куски по мере прихода; соединение живёт, пока жив поток.
pub(crate) struct Body {
    body: Incoming,
    _conn: Conn,
}

impl Body {
    /// Следующий кусок; `None` — сервер закончил ответ.
    pub(crate) async fn chunk(&mut self) -> Result<Option<Bytes>> {
        loop {
            match self.body.frame().await {
                None => return Ok(None),
                Some(Err(e)) => return Err(hyper_err(e)),
                Some(Ok(frame)) => {
                    if let Ok(data) = frame.into_data() {
                        return Ok(Some(data));
                    }
                }
            }
        }
    }
}

fn www_authenticate(resp: &hyper::Response<Incoming>) -> Vec<String> {
    resp.headers().get_all(header::WWW_AUTHENTICATE).iter().filter_map(|v| v.to_str().ok().map(str::to_owned)).collect()
}

/// Ответ целиком (с пределом размера).
async fn collect(resp: hyper::Response<Incoming>) -> Result<Reply> {
    let status = resp.status().as_u16();
    let www_authenticate = www_authenticate(&resp);
    let location = resp.headers().get(header::LOCATION).and_then(|v| v.to_str().ok()).map(str::to_owned);
    let body = Limited::new(resp.into_body(), MAX_BODY)
        .collect()
        .await
        .map_err(|e| if e.is::<http_body_util::LengthLimitError>() { Error::TooLarge } else { Error::Http(e.to_string()) })?;
    Ok(Reply { status, body: body.to_bytes().to_vec(), location, www_authenticate })
}

/// Дочитать тело ответа 401 рукопожатия: иначе соединение не пойдёт под следующий запрос.
async fn drain(resp: hyper::Response<Incoming>) {
    let _ = Limited::new(resp.into_body(), MAX_BODY).collect().await;
}

impl Conn {
    async fn send(&mut self, url: &url::Url, body: &Bytes, authorization: Option<&str>) -> Result<Reply> {
        collect(self.request(url, body, authorization).await?).await
    }

    async fn request(&mut self, url: &url::Url, body: &Bytes, authorization: Option<&str>) -> Result<hyper::Response<Incoming>> {
        self.sender.ready().await.map_err(hyper_err)?;
        let host = if self.port == 443 { self.host.clone() } else { format!("{}:{}", self.host, self.port) };
        let path = match url.query() {
            Some(q) => format!("{}?{q}", url.path()),
            None => url.path().to_owned(),
        };
        let mut req = hyper::Request::post(path)
            .header(header::HOST, host)
            .header(header::CONTENT_TYPE, "text/xml; charset=utf-8")
            .header(header::USER_AGENT, concat!("Postman/", env!("CARGO_PKG_VERSION")));
        if let Some(a) = authorization {
            req = req.header(header::AUTHORIZATION, a);
        }
        let req = req.body(Full::new(body.clone())).map_err(|e| Error::Http(e.to_string()))?;
        self.sender.send_request(req).await.map_err(hyper_err)
    }
}

impl Http {
    pub(crate) fn new(auth: Auth) -> Http {
        Http { auth, conn: None, last: None, min_interval: MIN_INTERVAL, timeout: TIMEOUT, signed_in: None }
    }

    /// POST тела XML; ответ — любой статус (разбирает вызывающий). Вход — по 401.
    pub(crate) async fn post(&mut self, url: &url::Url, body: Vec<u8>) -> Result<Reply> {
        self.post_with(url, body, true).await
    }

    /// [`Http::post`] без повтора на новом соединении, когда сервер закрыл прежнее: для запросов, которые создают
    /// объект (`CreateItem`), — запрос мог дойти, а ответ потеряться; повтор — у вызывающего, с проверкой.
    pub(crate) async fn post_no_resend(&mut self, url: &url::Url, body: Vec<u8>) -> Result<Reply> {
        self.post_with(url, body, false).await
    }

    async fn post_with(&mut self, url: &url::Url, body: Vec<u8>, resend: bool) -> Result<Reply> {
        self.pace().await;
        let body = Bytes::from(body);
        tokio::time::timeout(self.timeout, async { collect(self.post_once(url, &body, resend).await?).await }).await.map_err(|_| Error::Timeout)?
    }

    /// POST, ответ на который — поток: статус и тело кусками ([`Body`]). Предел времени — до заголовков ответа;
    /// соединение уходит с потоком.
    pub(crate) async fn post_stream(&mut self, url: &url::Url, body: Vec<u8>) -> Result<(u16, Body)> {
        self.pace().await;
        let body = Bytes::from(body);
        let resp = tokio::time::timeout(self.timeout, self.post_once(url, &body, true)).await.map_err(|_| Error::Timeout)??;
        let conn = self.conn.take().ok_or(Error::Closed)?;
        Ok((resp.status().as_u16(), Body { body: resp.into_body(), _conn: conn }))
    }

    /// Не чаще `min_interval`: подождать остаток с прошлого запроса.
    async fn pace(&mut self) {
        if let Some(last) = self.last {
            let wait = self.min_interval.saturating_sub(last.elapsed());
            if !wait.is_zero() {
                tokio::time::sleep(wait).await;
            }
        }
        self.last = Some(Instant::now());
    }

    async fn conn(&mut self, url: &url::Url) -> Result<&mut Conn> {
        let host = url.host_str().ok_or_else(|| Error::Http("URL without host".into()))?;
        let port = url.port_or_known_default().unwrap_or(443);
        let stale = match &self.conn {
            Some(c) => c.host != host || c.port != port || c.sender.is_closed(),
            None => true,
        };
        if stale {
            self.conn = Some(connect(host, port, url.scheme() == "http").await?);
        }
        self.conn.as_mut().ok_or(Error::Closed)
    }

    /// POST без входа: ответ как есть (401 — тоже). Для [`crate::probe`].
    pub(crate) async fn post_anonymous(&mut self, url: &url::Url, body: Vec<u8>) -> Result<Reply> {
        let body = Bytes::from(body);
        tokio::time::timeout(self.timeout, async { self.conn(url).await?.send(url, &body, None).await }).await.map_err(|_| Error::Timeout)?
    }

    /// Сервер предлагает Negotiate или NTLM (по ответу 401).
    pub(crate) fn offers_sspi(reply: &Reply) -> bool {
        reply.status == 401 && auth::choose(&reply.www_authenticate, false).is_some()
    }

    async fn post_once(&mut self, url: &url::Url, body: &Bytes, resend: bool) -> Result<hyper::Response<Incoming>> {
        // Сервер закрыл соединение, пока оно ждало (простой, перезапуск IIS), — одна попытка на новом (`resend`).
        let first = match self.conn(url).await?.request(url, body, None).await {
            Err(Error::Closed) if resend => {
                self.conn = None;
                self.conn(url).await?.request(url, body, None).await?
            }
            r => r?,
        };
        if first.status().as_u16() != 401 {
            return Ok(first);
        }
        let offered = www_authenticate(&first);
        drain(first).await;
        let (auth, cbt) = remembered(&self.auth);
        let password = matches!(auth, Auth::Password { .. });
        let Some(scheme) = auth::choose(&offered, password) else {
            return self.basic(url, body, &auth, &offered).await;
        };
        let mut diag = Diag { offered: auth::challenges(&offered).into_iter().map(|(n, _)| n).collect(), scheme, binding: false, tries: Vec::new() };
        // Попытки входа с паролем при отказе сервера — не больше трёх (счётчик неверных входов AD): как введено и с
        // привязкой к каналу; без привязки (TLS перед Exchange завершает прокси — хэш сертификата не тот, а
        // Extended Protection в режиме «Allow» принимает вход без привязки); `ДОМЕН\имя` вместо адреса почты (домен
        // — из CHALLENGE сервера). Удачная — дальше у всех соединений этого входа сразу.
        let (mut auth, mut cbt) = (auth, cbt);
        loop {
            // 401 с `Connection: close` — рукопожатие на новом соединении.
            let conn = self.conn(url).await?;
            let with_cbt = cbt && conn.binding.is_some();
            diag.binding |= conn.binding.is_some();
            tracing::debug!(scheme = scheme.name(), binding = with_cbt, "ews sign-in");
            let mut rejected = Rejected::default();
            match handshake(conn, &auth, scheme, url, body, cbt, &mut rejected).await {
                Ok(reply) => {
                    remember(&self.auth, &auth, cbt);
                    self.signed_in = Some(scheme.name());
                    return Ok(reply);
                }
                Err(Error::Auth(e)) if e == CREDENTIALS_REJECTED => {
                    diag.tries.push(format!("{}{} → 401 after token {}", login_kind(&auth), if with_cbt { "+CBT" } else { "" }, rejected.round));
                    let tried_plain = diag.tries.iter().any(|t| !t.contains("+CBT"));
                    let next = match (&auth, rejected.alternative) {
                        (Auth::Password { .. }, _) if with_cbt && !tried_plain => Some((auth.clone(), false)),
                        (Auth::Password { password, .. }, Some(login)) if diag.tries.len() < 3 => {
                            Some((Auth::Password { user: login, password: password.clone() }, true))
                        }
                        _ => None,
                    };
                    match next {
                        Some(n) => (auth, cbt) = n,
                        None => {
                            tracing::debug!(diag = %diag, "ews sign-in rejected");
                            return Err(Error::Auth(format!("{CREDENTIALS_REJECTED} ({diag})")));
                        }
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Сервер не предлагает ни Negotiate, ни NTLM: запасной Basic — только по TLS и только с паролем; иначе
    /// [`Error::AuthScheme`].
    async fn basic(&mut self, url: &url::Url, body: &Bytes, auth: &Auth, offered: &[String]) -> Result<hyper::Response<Incoming>> {
        let Auth::Password { user, password } = auth else { return Err(Error::AuthScheme) };
        if url.scheme() != "https" || !auth::offers(offered, "Basic") {
            return Err(Error::AuthScheme);
        }
        let header = auth::basic(user, password);
        let reply = self.conn(url).await?.request(url, body, Some(&header)).await?;
        if reply.status().as_u16() == 401 {
            drain(reply).await;
            return Err(Error::Auth(CREDENTIALS_REJECTED.into()));
        }
        self.signed_in = Some("Basic");
        Ok(reply)
    }
}

/// Сведения об отказе во входе без секретов и имён — в текст ошибки («Вход не выполнен: …»), чтобы его можно было
/// прислать: что предложил сервер, чем входили, была ли привязка к каналу TLS, после какого токена пришёл 401.
struct Diag {
    offered: Vec<String>,
    scheme: Scheme,
    binding: bool,
    tries: Vec<String>,
}

impl std::fmt::Display for Diag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "offered: {}; used: {}; TLS channel binding: {}; tries: {}",
            self.offered.join(", "),
            self.scheme.name(),
            if self.binding { "available" } else { "none" },
            self.tries.join(", ")
        )
    }
}

/// Вид логина для [`Diag`]: сам логин — нет.
fn login_kind(auth: &Auth) -> &'static str {
    match auth {
        Auth::Windows => "Windows account",
        Auth::Password { user, .. } if user.contains('\\') => "DOMAIN\\user",
        Auth::Password { user, .. } if user.contains('@') => "UPN",
        Auth::Password { .. } => "user",
    }
}

/// Что принял сервер вместо введённого (`remembered`): введённый логин (без учёта регистра) → принятый и была ли
/// привязка к каналу. На время работы Postman: у каждого соединения учётки — сразу верный вход, без лишних отказов.
static ACCEPTED: std::sync::Mutex<Vec<(String, String, bool)>> = std::sync::Mutex::new(Vec::new());

/// Вход для рукопожатия и привязка к каналу (`true` — если есть у соединения).
fn remembered(auth: &Auth) -> (Auth, bool) {
    let Auth::Password { user, password } = auth else { return (auth.clone(), true) };
    let found = ACCEPTED.lock().ok().and_then(|m| m.iter().find(|(from, ..)| from.eq_ignore_ascii_case(user)).map(|(_, to, cbt)| (to.clone(), *cbt)));
    match found {
        Some((to, cbt)) => (Auth::Password { user: to, password: password.clone() }, cbt),
        None => (auth.clone(), true),
    }
}

fn remember(from: &Auth, to: &Auth, cbt: bool) {
    if let (Auth::Password { user: from, .. }, Auth::Password { user: to, .. }) = (from, to)
        && (from != to || !cbt)
        && let Ok(mut m) = ACCEPTED.lock()
    {
        m.retain(|(f, ..)| !f.eq_ignore_ascii_case(from));
        m.push((from.clone(), to.clone(), cbt));
    }
}

/// Отказ сервера в рукопожатии: после какого нашего токена (0 — первый) и логин на следующую попытку.
#[derive(Default)]
struct Rejected {
    round: usize,
    alternative: Option<String>,
}

/// Рукопожатие Negotiate или NTLM; `cbt` — с привязкой к каналу TLS (если она есть у соединения). Сервер отклонил вход —
/// [`CREDENTIALS_REJECTED`], подробности — в `rejected`.
async fn handshake(
    conn: &mut Conn,
    auth: &Auth,
    scheme: Scheme,
    url: &url::Url,
    body: &Bytes,
    cbt: bool,
    rejected: &mut Rejected,
) -> Result<hyper::Response<Incoming>> {
    let mut ctx = crate::sys::Context::new(scheme, auth, &format!("HTTP/{}", conn.host), conn.binding.as_deref().filter(|_| cbt))?;
    let mut token = ctx.step(None)?;
    for round in 0..MAX_ROUNDS {
        let reply = conn.request(url, body, Some(&auth::header(scheme, &token))).await.map_err(|e| match e {
            Error::Closed => Error::Auth("server closed the connection during the handshake".into()),
            e => e,
        })?;
        let server = auth::token(&www_authenticate(&reply), scheme);
        if reply.status().as_u16() != 401 {
            // Последний токен Kerberos (взаимная проверка): сервер уже проверен TLS — сбой только в журнал.
            if let (Some(t), false) = (server, ctx.done())
                && let Err(e) = ctx.step(Some(&t))
            {
                tracing::warn!(error = %e, "ews: final server token rejected");
            }
            return Ok(reply);
        }
        drain(reply).await;
        match server {
            Some(t) if !ctx.done() => token = ctx.step(Some(&t))?,
            _ => {
                *rejected = Rejected { round, alternative: ctx.alternative() };
                return Err(Error::Auth(CREDENTIALS_REJECTED.into()));
            }
        }
    }
    Err(Error::Auth("handshake did not finish".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Сведения об отказе — без логина и пароля: схемы, привязка, попытки по видам логина.
    #[test]
    fn diagnostics_without_secrets() {
        let d = Diag {
            offered: vec!["Negotiate".into(), "NTLM".into()],
            scheme: Scheme::Ntlm,
            binding: true,
            tries: vec![format!("{}+CBT → 401 after token 1", login_kind(&Auth::password("anna@corp.example", "pw")))],
        };
        let text = d.to_string();
        assert_eq!(text, "offered: Negotiate, NTLM; used: NTLM; TLS channel binding: available; tries: UPN+CBT → 401 after token 1");
        assert_eq!(login_kind(&Auth::password(r"CORP\anna", "pw")), r"DOMAIN\user");
        assert_eq!(login_kind(&Auth::Windows), "Windows account");
    }

    /// Принятый сервером логин и режим привязки — сразу у следующих соединений; принятое как введено — не хранится.
    #[test]
    fn accepted_login_is_reused() {
        let typed = Auth::password("boris@corp.example", "pw");
        assert!(matches!(remembered(&typed), (Auth::Password { ref user, .. }, true) if user == "boris@corp.example"));
        remember(&typed, &Auth::password(r"EXAMPLE\boris", "pw"), false);
        let (auth, cbt) = remembered(&Auth::password("Boris@corp.example", "pw"));
        assert!(matches!(auth, Auth::Password { ref user, ref password } if user == r"EXAMPLE\boris" && password.as_str() == "pw"));
        assert!(!cbt);
        let same = Auth::password("vera@corp.example", "pw");
        remember(&same, &same, true);
        assert!(ACCEPTED.lock().unwrap().iter().all(|(f, ..)| f != "vera@corp.example"));
    }
}
