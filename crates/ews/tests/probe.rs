//! Разведка боевого Exchange (S7-06б), только чтение и только тестовая учётка: Autodiscover, EWS без входа и со
//! входом (схема, версия), IMAP4 (вход, возможности, хранит ли ключевые слова), SMTP submission (вход, без отправки).
//! Вывод — только коды, версии и признаки: ни адресов, ни узлов, ни имён.
//!
//! Окружение — как у `stand.rs`; IMAP и SMTP — только с `POSTMAN_PROBE_MAIL=1` и узлами `POSTMAN_EXCH_IMAP`,
//! `POSTMAN_EXCH_SMTP` (`узел`; SMTP — по умолчанию узел IMAP).
//! Запуск: cargo test -p postman-ews --test probe -- --ignored --nocapture

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)

use std::time::Duration;

use postman_ews::{Auth, Client, discover};

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn need(name: &str) -> String {
    env(name).unwrap_or_else(|| panic!("{name} не задан"))
}

fn host_of(url: &str) -> String {
    url.trim_start_matches("https://").split(['/', ':']).next().unwrap_or_default().to_owned()
}

/// Вид ошибки EWS без текста (в тексте бывают узлы и адреса).
fn kind(e: &postman_ews::Error) -> String {
    use postman_ews::Error as E;
    match e {
        E::Ews { code, .. } => code.clone(),
        E::Status(n) => format!("HTTP {n}"),
        E::Auth(_) => "вход отклонён".into(),
        E::AuthScheme => "нет Negotiate/NTLM".into(),
        E::Sspi { call, code } => format!("SSPI {call} 0x{code:08x}"),
        E::Tls(m) if m.contains("UnknownIssuer") => "TLS: неизвестный издатель".into(),
        E::Tls(m) if m.contains("NotValidForName") => "TLS: сертификат не на это имя".into(),
        E::Tls(m) if m.contains("Expired") => "TLS: сертификат истёк".into(),
        // Текст rustls — без слов с точкой (имена узлов).
        E::Tls(m) => format!("TLS: {}", m.split_whitespace().filter(|w| !w.contains('.')).collect::<Vec<_>>().join(" ")),
        E::Io(_) | E::Closed | E::Timeout => "нет связи".into(),
        E::Xml(_) => "не XML".into(),
        _ => "другое".into(),
    }
}

fn what(e: &postman_imap::Error) -> &'static str {
    match e {
        postman_imap::Error::Auth(_) => "вход отклонён",
        postman_imap::Error::Io(_) | postman_imap::Error::Timeout => "нет связи",
        postman_imap::Error::Tls(_) => "TLS",
        postman_imap::Error::Smtp(_) => "SMTP",
        _ => "протокол",
    }
}

#[test]
#[ignore = "боевой Exchange: S7 «Проверка на боевом Exchange»"]
fn probe() {
    let user = need("POSTMAN_EWS_USER");
    let password = need("POSTMAN_EWS_PASSWORD");
    let email = env("POSTMAN_EWS_EMAIL").unwrap_or_else(|| user.clone());
    // Страховка правил S7: только тестовые учётки.
    assert!(email.split('@').next().unwrap_or("").to_lowercase().contains("test"), "учётка не похожа на тестовую — стоп");
    let auth = Auth::password(user.clone(), password.clone());
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        // Autodiscover.
        let found = discover(&email, &auth, None).await;
        let url = match (&found, env("POSTMAN_EWS_URL")) {
            (_, Some(u)) => u,
            (Ok(f), None) => f.urls[0].clone(),
            (Err(e), None) => panic!("Autodiscover: {}", kind(e)),
        };
        match &found {
            Ok(f) => println!(
                "Autodiscover: {:?}, адресов EWS {}, имя ящика есть: {}, внутренний и внешний один узел: {}",
                f.source,
                f.urls.len(),
                !f.display_name.is_empty(),
                f.urls.iter().map(|u| host_of(u)).collect::<std::collections::BTreeSet<_>>().len() == 1
            ),
            Err(e) => println!("Autodiscover: ошибка {}", kind(e)),
        }

        let ews_host = host_of(&url);
        let dom = email.rsplit_once('@').map(|(_, d)| d.to_lowercase()).unwrap_or_default();
        let where_ = match ews_host.to_lowercase() {
            h if h == format!("autodiscover.{dom}") => "autodiscover.<домен>",
            h if h == format!("mail.{dom}") => "mail.<домен>",
            h if h.ends_with(&format!(".{dom}")) => "другой узел домена",
            _ => "вне домена",
        };
        println!("Узел EWS: {where_}");
        for port in [993u16, 143, 587, 465, 25] {
            let open = tokio::time::timeout(Duration::from_secs(4), tokio::net::TcpStream::connect((ews_host.as_str(), port))).await;
            println!("  узел EWS, порт {port}: {}", if matches!(open, Ok(Ok(_))) { "открыт" } else { "закрыт" });
        }

        // Каждый адрес Autodiscover отдельно — вид ответа.
        let domain = email.rsplit_once('@').map(|(_, d)| d.to_owned()).unwrap_or_default();
        for (label, host) in [("<домен>", domain.clone()), ("autodiscover.<домен>", format!("autodiscover.{domain}"))] {
            let at = format!("https://{host}/autodiscover/autodiscover.xml");
            match postman_ews::autodiscover::discover_at(&at, &email, &auth).await {
                Ok(f) => println!("  {label}: настройки, адресов EWS {}", f.urls.len()),
                Err(postman_ews::Error::Autodiscover(m)) => println!("  {label}: {m}"),
                Err(e) => println!("  {label}: {}", kind(&e)),
            }
        }

        // EWS без входа и со входом.
        println!("EWS без входа просит Negotiate/NTLM: {}", postman_ews::probe(&url).await);
        let mut c = Client::new(&url, auth.clone()).unwrap();
        match c.calendar_folder().await {
            Ok(cal) => {
                let v = c.server_version().unwrap_or_default();
                println!(
                    "EWS вход: {} ; Exchange {}.{}.{}.{} ; календарь: {} объектов",
                    c.signed_in_with().unwrap_or("без 401"),
                    v.major,
                    v.minor,
                    v.build,
                    v.revision,
                    cal.total
                );
            }
            Err(e) => println!("EWS вход: ошибка {}", kind(&e)),
        }

        // IMAP4 и SMTP — только по явной просьбе (`POSTMAN_PROBE_MAIL=1`) и с узлами из окружения: неудачные входы
        // блокируют учётку, а узел почты у Exchange компании может быть другой системой.
        if env("POSTMAN_PROBE_MAIL").as_deref() != Some("1") || env("POSTMAN_EXCH_IMAP").is_none() {
            return;
        }
        // IMAP4.
        let mail_host = need("POSTMAN_EXCH_IMAP");
        for (port, security) in [(993u16, postman_imap::Security::Tls), (143, postman_imap::Security::StartTls)] {
            let cfg = postman_imap::Config { host: mail_host.clone(), port, security, user: user.clone(), password: password.clone() };
            match tokio::time::timeout(Duration::from_secs(20), postman_imap::Session::connect(&cfg)).await {
                Ok(Ok(mut s)) => {
                    let caps =
                        ["IMAP4rev1", "IDLE", "MOVE", "UIDPLUS", "CONDSTORE", "QRESYNC", "SPECIAL-USE", "ID"].map(|k| format!("{k}={}", s.has(k)));
                    let folders = s.folders().await.map(|f| f.len()).unwrap_or(0);
                    let inbox = s.select("INBOX").await;
                    println!(
                        "IMAP {port}: вход есть; {}; папок {folders}; ключевые слова хранит: {}",
                        caps.join(" "),
                        inbox.as_ref().map_or("?".to_owned(), |i| i.keywords.to_string())
                    );
                    let _ = s.logout().await;
                }
                Ok(Err(e)) => println!("IMAP {port}: {}", what(&e)),
                Err(_) => println!("IMAP {port}: нет ответа за 20 с"),
            }
        }

        // SMTP submission: вход без отправки.
        let _ = tokio_rustls::rustls::crypto::aws_lc_rs::default_provider().install_default();
        let smtp_host = env("POSTMAN_EXCH_SMTP").unwrap_or_else(|| mail_host.clone());
        for (port, implicit) in [(587u16, false), (465, true)] {
            let builder = mail_send::SmtpClientBuilder::new(smtp_host.as_str(), port)
                .unwrap()
                .implicit_tls(implicit)
                .credentials((user.as_str(), password.as_str()))
                .timeout(Duration::from_secs(20));
            match builder.connect().await {
                Ok(client) => {
                    println!("SMTP {port}: вход есть (письмо не отправлялось)");
                    let _ = client.quit().await;
                }
                Err(mail_send::Error::AuthenticationFailed(_)) => println!("SMTP {port}: вход отклонён"),
                Err(mail_send::Error::Io(_) | mail_send::Error::Timeout) => println!("SMTP {port}: нет связи"),
                Err(e) => println!(
                    "SMTP {port}: {}",
                    match e {
                        mail_send::Error::MissingStartTls => "нет STARTTLS".to_owned(),
                        mail_send::Error::UnsupportedAuthMechanism => "нет подходящего AUTH".to_owned(),
                        mail_send::Error::Tls(_) => "TLS".to_owned(),
                        _ => "протокол".to_owned(),
                    }
                ),
            }
        }
    });
}
