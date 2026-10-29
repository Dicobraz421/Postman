//! Изолированный воркер разбора писем (HANDOFF §3a).
//!
//! UI держит один процесс `postman-worker` и общается с ним сообщениями
//! «JSON-заголовок + бинарная часть» (каждая — `u32 LE длина + байты`)
//! по stdin/stdout. Бинарная часть — исходная картинка в запросе и RGBA в ответе. Воркер запирает себя до чтения
//! первого запроса: без сети, без файловой системы, без дочерних процессов,
//! на Windows — без win32k. Зависание или падение воркера стоит одного письма:
//! клиент убивает процесс и поднимает новый.

pub mod mime;
pub mod preview;
mod sandbox;

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use postman_ir::Document;
use postman_smime::envelope::{Envelope, OpenError, Opened};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

pub use preview::{Entry, Listing, OfficeFormat, OfficeScan, Preview, PreviewKind};
pub use sandbox::Level;

/// Код выхода воркера, которого остановил сторож времени: разбор одного запроса дольше [`TIME_LIMIT`].
pub const EXIT_TIME: i32 = 5;
/// Предел времени на один запрос внутри воркера (сторож в [`serve`]); клиент ждёт чуть дольше.
pub const TIME_LIMIT: Duration = Duration::from_secs(8);
/// Сколько клиент ждёт ответа на просмотр вложения: сторож воркера срабатывает раньше.
pub const PREVIEW_TIMEOUT: Duration = Duration::from_secs(10);

/// Сколько клиент ждёт разбора письма на части (до [`mime::MAX_SPLIT`]): сторож воркера срабатывает раньше.
pub const SPLIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Больше этого не принимаем ни в одну сторону (кроме бинарной части `MimeSplit`).
const MAX_FRAME: usize = 64 * 1024 * 1024;
/// Бинарная часть запроса и ответа `MimeSplit`: письмо целиком и его раскодированные части (не длиннее письма).
const MAX_SPLIT_FRAME: usize = mime::MAX_SPLIT;

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    /// Письмо целиком (RFC 5322) в бинарной части → тело и вложения (S7-04б, IMAP: MIME разбирает Postman).
    Mime,
    /// Байты вложения `index` письма из бинарной части → в бинарной части ответа.
    MimePart {
        index: u32,
    },
    /// Письмо целиком для хранилища (S8-07, ADR 0006 §2) в бинарной части → [`mime::Split`], раскодированные части
    /// подряд — в бинарной части ответа.
    MimeSplit,
    Html(String),
    Text(String),
    /// Приглашение iCalendar (.ics) в бинарной части; ответ — METHOD и JSCalendar (JSON).
    Calendar,
    /// Карточки vCard (.vcf, одна или несколько) в бинарной части; ответ — JSContact (JSON) каждой (S7-05б, CardDAV).
    Contacts,
    /// Картинка (PNG/JPEG/GIF/WebP) в бинарной части; ответ — RGBA не больше `max_side` по стороне.
    Image {
        max_side: u32,
    },
    /// Встроенный просмотр вложения (S5-07): файл в бинарной части; `page` — страница PDF с нуля; ответ —
    /// [`Preview`], пиксели картинки и страницы — RGBA в бинарной части; `width` — ширина страницы PDF (0 — по
    /// `max_side`).
    Preview {
        kind: PreviewKind,
        page: u32,
        max_side: u32,
        #[serde(default)]
        width: u32,
    },
    /// Проверка подписи S/MIME (S5-12 Ф1): в бинарной части — якоря доверия и письмо целиком
    /// (`postman_smime::wire`); `now` — Unix-время проверки. Ответ — [`postman_smime::Report`], тело письма с
    /// подписью внутри (`signed-data`) — IR, DER цепочки — в бинарной части.
    SmimeVerify {
        now: u64,
    },
    /// Получатели и шифр шифрованного письма S/MIME (S5-12 Ф2): письмо — в бинарной части. Ничего не
    /// расшифровывает; ответ — [`postman_smime::envelope::Envelope`] (поля для ядра, решение владельца №1).
    SmimeEnvelope,
    /// Расшифровка (Ф2): в бинарной части — ключ содержимого, якоря и письмо (`postman_smime::wire::encode_open`).
    /// `part` — `None`: тело (IR), вложения и подпись внутри, DER её цепочки — в бинарной части ответа; номер —
    /// байты этого вложения в бинарной части ответа.
    SmimeOpen {
        now: u64,
        part: Option<u32>,
    },
    /// Письмо, расшифрованное в ядре СКЗИ (ГОСТ): в бинарной части — якоря и внутреннее письмо
    /// (`postman_smime::wire::encode_request`); `from` — «От» внешнего письма. Ответ — как у `SmimeOpen`.
    SmimeInner {
        now: u64,
        from: String,
        part: Option<u32>,
    },
    /// Подготовить сертификаты получателей к шифрованию (S5-12 Ф4): в бинарной части — якоря и сертификаты
    /// (`postman_smime::wire::encode_prepare`). Ответ — на каждый сертификат значения ключа или причина отказа.
    SmimePrepare {
        now: u64,
    },
    /// Отзыв ГОСТ без СКЗИ (S5-12.1д): в бинарной части — сертификат, его издатель (из уже доверенной цепочки) и CRL,
    /// скачанный ядром (`postman_smime::wire::encode_crl`). Ответ — «не отозван / отозван» или почему не проверен.
    SmimeCrl {
        now: u64,
    },
    /// Проверки песочницы для тестов: попытка открыть файл / сокет / зависнуть.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    Probe(Probe),
}

/// Сколько клиент ждёт проверки подписи (письмо до 62 МБ): сторож воркера срабатывает раньше.
pub const SMIME_TIMEOUT: Duration = Duration::from_secs(10);

/// Почему проверка подписи не состоялась.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmimeFailure {
    /// Письмо больше предела безопасной проверки (`postman_smime::MAX_MESSAGE`).
    TooLarge,
    /// Воркер не ответил или ответил ошибкой.
    Worker(String),
}

/// Тело `signed-data` → IR (в песочнице).
fn inner_document(content: &[u8]) -> Option<Document> {
    match postman_smime::inner_body(content) {
        (Some(html), _) => Some(postman_ir::from_html(&html)),
        (None, Some(text)) => Some(postman_ir::from_plain(&text)),
        (None, None) => None,
    }
}

fn smime_envelope(bin: &[u8]) -> Response {
    Response::Envelope(postman_smime::envelope::envelope(bin))
}

/// Расшифровка в песочнице: тело → IR; вложение — байты (только в памяти, на диск — нигде).
fn smime_open(bin: &[u8], now: u64, part: Option<u32>) -> (Response, Vec<u8>) {
    let Some((trust, message, cek)) = postman_smime::wire::decode_open(bin) else {
        return (Response::Err("запрос расшифровки повреждён".into()), Vec::new());
    };
    if let Some(index) = part {
        return match postman_smime::envelope::part(message, cek, index) {
            Ok(bytes) => (Response::Part, bytes.to_vec()),
            Err(e) => (Response::Opened { opened: Box::new(Err(e)), doc: None }, Vec::new()),
        };
    }
    match postman_smime::envelope::open(message, cek, &trust, now) {
        Ok(o) => {
            let doc = match (&o.html, &o.text) {
                (Some(html), _) => Some(postman_ir::from_html(html)),
                (None, Some(text)) => Some(postman_ir::from_plain(text)),
                (None, None) => None,
            };
            let out = postman_smime::wire::encode_response(&o.inner.chain, None);
            (Response::Opened { opened: Box::new(Ok(o)), doc }, out)
        }
        Err(e) => (Response::Opened { opened: Box::new(Err(e)), doc: None }, Vec::new()),
    }
}

/// Письмо, расшифрованное СКЗИ в ядре: разбор — как после своей расшифровки.
fn smime_inner(bin: &[u8], now: u64, from: &str, part: Option<u32>) -> (Response, Vec<u8>) {
    let Some((trust, inner)) = postman_smime::wire::decode_request(bin) else {
        return (Response::Err("запрос разбора повреждён".into()), Vec::new());
    };
    if let Some(index) = part {
        return match postman_smime::envelope::part_inner(inner, index) {
            Ok(bytes) => (Response::Part, bytes.to_vec()),
            Err(e) => (Response::Opened { opened: Box::new(Err(e)), doc: None }, Vec::new()),
        };
    }
    match postman_smime::envelope::open_inner(inner, from, postman_smime::envelope::Cipher::Gost28147, &trust, now) {
        Ok(o) => {
            let doc = match (&o.html, &o.text) {
                (Some(html), _) => Some(postman_ir::from_html(html)),
                (None, Some(text)) => Some(postman_ir::from_plain(text)),
                (None, None) => None,
            };
            let out = postman_smime::wire::encode_response(&o.inner.chain, None);
            (Response::Opened { opened: Box::new(Ok(o)), doc }, out)
        }
        Err(e) => (Response::Opened { opened: Box::new(Err(e)), doc: None }, Vec::new()),
    }
}

fn smime_verify(bin: &[u8], now: u64) -> (Response, Vec<u8>) {
    let Some((trust, message)) = postman_smime::wire::decode_request(bin) else {
        return (Response::Err("запрос проверки подписи повреждён".into()), Vec::new());
    };
    let report = postman_smime::verify(message, &trust, now);
    let doc = report.content.as_deref().and_then(inner_document);
    let out = postman_smime::wire::encode_response(&report.chain, None);
    (Response::Smime { report: Box::new(report), doc }, out)
}

#[cfg(debug_assertions)]
#[doc(hidden)]
#[derive(Debug, Serialize, Deserialize)]
pub enum Probe {
    ReadFile(String),
    Connect(String),
    Spawn(String),
    Hang,
}

#[cfg(debug_assertions)]
fn probe(p: Probe) -> Response {
    let r = match p {
        Probe::ReadFile(path) => std::fs::read(path).map(|_| ()),
        Probe::Connect(addr) => std::net::TcpStream::connect(addr).map(drop),
        Probe::Spawn(cmd) => std::process::Command::new(cmd).status().map(drop),
        Probe::Hang => loop {
            std::hint::spin_loop();
        },
    };
    match r {
        Ok(()) => Response::Err("ДОСТУП РАЗРЕШЁН".into()),
        Err(e) => Response::Err(format!("запрещено: {e}")),
    }
}

#[derive(Debug, Serialize, Deserialize)]
enum Response {
    Ok(Document),
    Mime(Box<mime::Mime>),
    /// Листовые части письма; раскодированные — в бинарной части.
    Split(mime::Split),
    /// RGBA8 в бинарной части.
    Image {
        width: u32,
        height: u32,
    },
    Calendar(serde_json::Value),
    Contacts(Vec<serde_json::Value>),
    Preview(Preview),
    /// Итог проверки подписи; DER цепочки — в бинарной части.
    Smime {
        report: Box<postman_smime::Report>,
        doc: Option<Document>,
    },
    /// Получатели и шифр (Ф2).
    Envelope(Result<postman_smime::envelope::Envelope, postman_smime::envelope::OpenError>),
    /// Итог расшифровки; DER цепочки подписи внутри — в бинарной части.
    Opened {
        opened: Box<Result<postman_smime::envelope::Opened, postman_smime::envelope::OpenError>>,
        doc: Option<Document>,
    },
    /// Байты вложения расшифрованного письма — в бинарной части.
    Part,
    /// Подготовленные получатели (Ф4) — по порядку сертификатов запроса.
    Prepared(Vec<Result<postman_smime::prepare::Prepared, String>>),
    /// Итог проверки по CRL (12.1д).
    Crl(Result<postman_smime::crl::Checked, postman_smime::crl::CrlError>),
    Err(String),
}

/// Декодированная картинка.
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

fn decode_image(bytes: &[u8], max_side: u32) -> Result<Rgba, String> {
    decode(bytes, max_side).map(|(img, _, _)| img)
}

/// Картинка, её исходный размер и формат.
pub(crate) fn decode(bytes: &[u8], max_side: u32) -> Result<(Rgba, (u32, u32), &'static str), String> {
    use image::codecs::{gif::GifDecoder, jpeg::JpegDecoder, png::PngDecoder, webp::WebPDecoder};
    use image::{DynamicImage, ImageDecoder, ImageFormat, ImageResult, Limits};

    // Как `ImageReader::decode`, но с декодером нужного формата напрямую: `ImageReader` выбирает декодер
    // во время работы и тянет в exe все форматы, включённые в `image` по рабочей области (gpui включает
    // exr, tiff, dds, hdr…), хотя всё, кроме четырёх, мы отвергаем: +1,2 МиБ к воркеру (docs/SIZE.md).
    fn load(mut decoder: impl ImageDecoder, mut limits: Limits) -> ImageResult<DynamicImage> {
        limits.reserve(decoder.total_bytes())?;
        decoder.set_limits(limits)?;
        DynamicImage::from_decoder(decoder)
    }

    let mut limits = Limits::default();
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    limits.max_alloc = Some(256 * 1024 * 1024);
    let r = io::Cursor::new(bytes);
    let (decoded, format) = match image::guess_format(bytes) {
        Ok(ImageFormat::Png) => (PngDecoder::with_limits(r, limits.clone()).and_then(|d| load(d, limits)), "PNG"),
        Ok(ImageFormat::Jpeg) => (JpegDecoder::new(r).and_then(|d| load(d, limits)), "JPEG"),
        Ok(ImageFormat::Gif) => (GifDecoder::new(r).and_then(|d| load(d, limits)), "GIF"),
        Ok(ImageFormat::WebP) => (WebPDecoder::new(r).and_then(|d| load(d, limits)), "WebP"),
        _ => return Err("формат картинки не поддерживается".into()),
    };
    let mut img = decoded.map_err(|e| e.to_string())?;
    let source = (img.width(), img.height());
    let max_side = max_side.clamp(16, 4096);
    if img.width() > max_side || img.height() > max_side {
        img = img.thumbnail(max_side, max_side);
    }
    let rgba = img.into_rgba8();
    Ok((Rgba { width: rgba.width(), height: rgba.height(), pixels: rgba.into_raw() }, source, format))
}

/// iCalendar → `{ "method": "REQUEST", "data": <JSCalendar> }`. Чужие данные — только в песочнице.
/// Без песочницы — только для тестов и самого воркера.
pub fn parse_calendar(bytes: &[u8]) -> Result<serde_json::Value, String> {
    if bytes.len() > 2 * 1024 * 1024 {
        return Err("приглашение слишком большое".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "приглашение не в UTF-8".to_string())?;
    let method = text.lines().find_map(|l| l.trim().strip_prefix("METHOD:")).map(|m| m.trim().to_ascii_uppercase()).unwrap_or_default();
    let ical = match calcard::Parser::new(text).entry() {
        calcard::Entry::ICalendar(c) => c,
        _ => return Err("не iCalendar".into()),
    };
    let js = ical.into_jscalendar::<String, String>();
    let data = serde_json::to_value(&js.0).map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "method": method, "data": data }))
}

/// Карточек в одном .vcf — не больше: адресная книга приходит по карточке на ресурс.
const MAX_CARDS: usize = 1000;

/// vCard (одна или несколько карточек) → JSContact каждой (RFC 9555, как JMAP Contacts у Stalwart). Чужие данные —
/// только в песочнице; без песочницы — только для тестов и самого воркера.
pub fn parse_contacts(bytes: &[u8]) -> Result<Vec<serde_json::Value>, String> {
    if bytes.len() > 2 * 1024 * 1024 {
        return Err("карточка слишком большая".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "карточка не в UTF-8".to_string())?;
    let mut parser = calcard::Parser::new(text);
    let mut out = Vec::new();
    loop {
        match parser.entry() {
            calcard::Entry::VCard(card) => {
                if out.len() >= MAX_CARDS {
                    return Err("слишком много карточек".into());
                }
                let js = card.into_jscontact::<String, String>();
                out.push(serde_json::to_value(&js.0).map_err(|e| e.to_string())?);
            }
            calcard::Entry::Eof => break,
            calcard::Entry::ICalendar(_) => return Err("не vCard".into()),
            // Неразборчивая строка или компонент — карточка без неё; остальные — как есть.
            _ => {}
        }
    }
    if out.is_empty() {
        return Err("нет карточек".into());
    }
    Ok(out)
}

/// Предел бинарной части запроса `req` и ответа на него: письмо на разбор частей — больше общего [`MAX_FRAME`].
fn max_bin(req: &Request) -> usize {
    match req {
        Request::MimeSplit => MAX_SPLIT_FRAME,
        _ => MAX_FRAME,
    }
}

fn write_msg(w: &mut impl Write, json: &[u8], bin: &[u8], max_bin: usize) -> io::Result<()> {
    write_frame(w, json, MAX_FRAME)?;
    write_frame(w, bin, max_bin)
}

fn write_frame(w: &mut impl Write, payload: &[u8], max: usize) -> io::Result<()> {
    if payload.len() > max {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "кадр слишком большой"));
    }
    w.write_all(&(payload.len() as u32).to_le_bytes())?;
    w.write_all(payload)?;
    w.flush()
}

fn read_frame(r: &mut impl Read, max: usize) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > max {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "кадр слишком большой"));
    }
    let mut buf = vec![0; len];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

/// Точка входа процесса-воркера.
pub fn serve() -> ! {
    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    // Сторож времени — до запирания (потом новые потоки могут быть запрещены): запрос дольше `TIME_LIMIT`
    // завершает процесс; клиент видит код `EXIT_TIME` и поднимает новый воркер.
    let watchdog = watchdog();
    if let Err(e) = sandbox::lockdown() {
        // Без песочницы не работаем: лучше показать письмо текстом, чем разбирать HTML без защиты.
        let json = serde_json::to_vec(&Response::Err(format!("песочница: {e}"))).unwrap_or_default();
        let _ = write_msg(&mut stdout, &json, &[], MAX_FRAME);
        std::process::exit(3);
    }
    loop {
        let Ok(frame) = read_frame(&mut stdin, MAX_FRAME) else { std::process::exit(0) };
        if let Some(w) = &watchdog {
            let _ = w.send(true);
        }
        // Предел бинарной части — по запросу; не разобрался — общий.
        let req = serde_json::from_slice::<Request>(&frame);
        let max = req.as_ref().map_or(MAX_FRAME, max_bin);
        let Ok(bin) = read_frame(&mut stdin, max) else { std::process::exit(0) };
        let mut out_bin = Vec::new();
        let resp = match req {
            Ok(Request::Mime) => match mime::parse(&bin) {
                Ok(m) => Response::Mime(Box::new(m)),
                Err(e) => Response::Err(e),
            },
            Ok(Request::MimePart { index }) => match mime::part(&bin, index) {
                Ok(bytes) => {
                    out_bin = bytes;
                    Response::Part
                }
                Err(e) => Response::Err(e),
            },
            Ok(Request::MimeSplit) => match mime::split(&bin) {
                Ok((split, bytes)) => {
                    out_bin = bytes;
                    Response::Split(split)
                }
                Err(e) => Response::Err(e),
            },
            Ok(Request::Html(h)) => Response::Ok(postman_ir::from_html(&h)),
            Ok(Request::Text(t)) => Response::Ok(postman_ir::from_plain(&t)),
            Ok(Request::Calendar) => match parse_calendar(&bin) {
                Ok(v) => Response::Calendar(v),
                Err(e) => Response::Err(e),
            },
            Ok(Request::Contacts) => match parse_contacts(&bin) {
                Ok(v) => Response::Contacts(v),
                Err(e) => Response::Err(e),
            },
            Ok(Request::Image { max_side }) => match decode_image(&bin, max_side) {
                Ok(img) => {
                    out_bin = img.pixels;
                    Response::Image { width: img.width, height: img.height }
                }
                Err(e) => Response::Err(e),
            },
            Ok(Request::Preview { kind, page, max_side, width }) => {
                // Сбой разборщика на чужом файле — ошибка этого просмотра, а не смерть процесса.
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| preview::preview(kind, &bin, page, max_side, width)));
                match r {
                    Ok(Ok((p, pixels))) => {
                        out_bin = pixels;
                        Response::Preview(p)
                    }
                    Ok(Err(e)) => Response::Err(e),
                    Err(_) => Response::Err("файл не разобран: сбой разборщика".into()),
                }
            }
            Ok(Request::SmimeVerify { now }) => {
                let (resp, bin) = smime_verify(&bin, now);
                out_bin = bin;
                resp
            }
            Ok(Request::SmimeEnvelope) => smime_envelope(&bin),
            Ok(Request::SmimeOpen { now, part }) => {
                let (resp, bin) = smime_open(&bin, now, part);
                out_bin = bin;
                resp
            }
            Ok(Request::SmimeInner { now, from, part }) => {
                let (resp, bin) = smime_inner(&bin, now, &from, part);
                out_bin = bin;
                resp
            }
            Ok(Request::SmimePrepare { now }) => match postman_smime::wire::decode_prepare(&bin) {
                Some((trust, certs)) => Response::Prepared(
                    certs
                        .iter()
                        .enumerate()
                        .map(|(i, c)| {
                            let others: Vec<Vec<u8>> = certs.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, d)| d.clone()).collect();
                            postman_smime::prepare::prepare(c, &others, &trust, now)
                        })
                        .collect(),
                ),
                None => Response::Err("запрос подготовки повреждён".into()),
            },
            Ok(Request::SmimeCrl { now }) => match postman_smime::wire::decode_crl(&bin) {
                Some((cert, issuer, crl)) => Response::Crl(postman_smime::crl::check(cert, issuer, crl, now)),
                None => Response::Err("запрос проверки CRL повреждён".into()),
            },
            #[cfg(debug_assertions)]
            Ok(Request::Probe(p)) => probe(p),
            Err(e) => Response::Err(e.to_string()),
        };
        if let Some(w) = &watchdog {
            let _ = w.send(false);
        }
        let out = serde_json::to_vec(&resp).unwrap_or_default();
        if write_msg(&mut stdout, &out, &out_bin, max).is_err() {
            std::process::exit(0);
        }
    }
}

/// Поток-сторож: `true` — запрос начат, `false` — закончен. Не закончен за [`TIME_LIMIT`] — выход с [`EXIT_TIME`].
/// В простое поток спит в `recv` и процессор не тратит. Поток не запустился — работаем без сторожа (клиент всё
/// равно убьёт воркер по своему таймауту).
fn watchdog() -> Option<mpsc::Sender<bool>> {
    let (tx, rx) = mpsc::channel::<bool>();
    let spawned = std::thread::Builder::new().name("watchdog".into()).spawn(move || {
        while let Ok(started) = rx.recv() {
            if !started {
                continue;
            }
            match rx.recv_timeout(TIME_LIMIT) {
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => std::process::exit(EXIT_TIME),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });
    spawned.ok().map(|_| tx)
}

/// Запущенный процесс-воркер, как его видит клиент.
pub(crate) struct Process {
    pub stdin: Box<dyn Write + Send>,
    pub stdout: Box<dyn Read + Send>,
    pub kill: Box<dyn FnMut() + Send>,
    /// Код завершения, если процесс уже завершился (для текста ошибки).
    pub exit_code: Box<dyn FnMut() -> Option<u32> + Send>,
    pub level: Level,
    /// Почему изоляция не полная (Windows: отказ AppContainer) — для журнала и текста падения тестов.
    pub fallback: Option<String>,
}

struct Running {
    stdin: Box<dyn Write + Send>,
    responses: mpsc::Receiver<io::Result<(Vec<u8>, Vec<u8>)>>,
    /// Предел бинарной части ответа на текущий запрос: ставится до отправки, читатель берёт его после заголовка.
    max_bin: Arc<AtomicUsize>,
    kill: Box<dyn FnMut() + Send>,
    exit_code: Box<dyn FnMut() -> Option<u32> + Send>,
    /// Воркер ответил хотя бы раз — значит, в этой песочнице он работоспособен.
    answered: bool,
}

impl Drop for Running {
    fn drop(&mut self) {
        (self.kill)();
    }
}

/// Клиент воркера. Не потокобезопасен: держите в одном потоке.
pub struct Client {
    exe: PathBuf,
    timeout: Duration,
    running: Option<Running>,
    level: Level,
    /// Строгая песочница не дала воркеру работать — запускаем с ослабленной (Windows: без AppContainer).
    relaxed: bool,
    /// Почему уровень не `Full` ([`Client::fallback`]).
    fallback: Option<String>,
}

impl Client {
    pub fn new(exe: PathBuf, timeout: Duration) -> Self {
        Self { exe, timeout, running: None, level: Level::None, relaxed: false, fallback: None }
    }

    /// Рядом с основным исполняемым файлом.
    pub fn default_exe() -> PathBuf {
        let name = if cfg!(windows) { "postman-worker.exe" } else { "postman-worker" };
        std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(name))).unwrap_or_else(|| PathBuf::from(name))
    }

    /// Насколько изолирован текущий процесс (после первого запроса).
    pub fn level(&self) -> Level {
        self.level
    }

    /// Почему изоляция не полная: отказ AppContainer при запуске или воркер, не ответивший в полной изоляции.
    /// `None` — уровень полный (или причина неизвестна).
    pub fn fallback(&self) -> Option<&str> {
        self.fallback.as_deref()
    }

    pub fn render(&mut self, req: &Request) -> Result<Document, String> {
        match self.call(req, &[])?.0 {
            Response::Ok(doc) => Ok(doc),
            Response::Image { .. }
            | Response::Calendar(_)
            | Response::Contacts(_)
            | Response::Preview(_)
            | Response::Smime { .. }
            | Response::Envelope(_)
            | Response::Opened { .. }
            | Response::Part
            | Response::Mime(_)
            | Response::Split(_)
            | Response::Prepared(_)
            | Response::Crl(_) => Err("неожиданный ответ".into()),
            Response::Err(e) => Err(e),
        }
    }

    /// Декодировать картинку из письма в песочнице.
    pub fn decode_image(&mut self, bytes: &[u8], max_side: u32) -> Result<Rgba, String> {
        match self.call(&Request::Image { max_side }, bytes)? {
            (Response::Image { width, height }, pixels) if pixels.len() == width as usize * height as usize * 4 => Ok(Rgba { width, height, pixels }),
            (Response::Err(e), _) => Err(e),
            _ => Err("неожиданный ответ".into()),
        }
    }

    /// Встроенный просмотр вложения в песочнице (S5-07): итог и пиксели картинки или страницы PDF (`width` —
    /// ширина страницы в пикселях, 0 — по `max_side`).
    pub fn preview(&mut self, kind: PreviewKind, bytes: &[u8], page: u32, max_side: u32, width: u32) -> Result<(Preview, Option<Rgba>), String> {
        if bytes.len() > preview::MAX_INPUT {
            return Err("файл слишком большой для просмотра".into());
        }
        let timeout = self.timeout.max(PREVIEW_TIMEOUT);
        match self.call_for(&Request::Preview { kind, page, max_side, width }, bytes, timeout)? {
            (Response::Preview(p), pixels) => match &p {
                Preview::Image { width, height, .. } | Preview::Pdf { width, height, .. } => {
                    let img = preview::rgba(*width, *height, pixels)?;
                    Ok((p, Some(img)))
                }
                _ => Ok((p, None)),
            },
            (Response::Err(e), _) => Err(e),
            _ => Err("неожиданный ответ".into()),
        }
    }

    /// Проверить подпись S/MIME письма `raw` (RFC 5322 целиком) к якорям `trust` в песочнице (S5-12 Ф1). DER цепочки
    /// — в `Report::chain`; тело письма с подписью внутри — IR.
    pub fn smime_verify(
        &mut self,
        raw: &[u8],
        trust: &postman_smime::Trust,
        now: u64,
    ) -> Result<(postman_smime::Report, Option<Document>), SmimeFailure> {
        if raw.len() > postman_smime::MAX_MESSAGE {
            return Err(SmimeFailure::TooLarge);
        }
        let bin = postman_smime::wire::encode_request(trust, raw);
        if bin.len() > MAX_FRAME {
            return Err(SmimeFailure::TooLarge);
        }
        let timeout = self.timeout.max(SMIME_TIMEOUT);
        match self.call_for(&Request::SmimeVerify { now }, &bin, timeout).map_err(SmimeFailure::Worker)? {
            (Response::Smime { report, doc }, out) => {
                let mut report = *report;
                let (chain, _) = postman_smime::wire::decode_response(&out).ok_or_else(|| SmimeFailure::Worker("неожиданный ответ".into()))?;
                report.chain = chain;
                Ok((report, doc))
            }
            (Response::Err(e), _) => Err(SmimeFailure::Worker(e)),
            _ => Err(SmimeFailure::Worker("неожиданный ответ".into())),
        }
    }

    /// Получатели и шифр шифрованного письма `raw` (S5-12 Ф2) — для ядра: оно снимает ключ средствами ОС.
    pub fn smime_envelope(&mut self, raw: &[u8]) -> Result<Result<Envelope, OpenError>, SmimeFailure> {
        if raw.len() > postman_smime::MAX_MESSAGE {
            return Err(SmimeFailure::TooLarge);
        }
        let timeout = self.timeout.max(SMIME_TIMEOUT);
        match self.call_for(&Request::SmimeEnvelope, raw, timeout).map_err(SmimeFailure::Worker)? {
            (Response::Envelope(e), _) => Ok(e),
            (Response::Err(e), _) => Err(SmimeFailure::Worker(e)),
            _ => Err(SmimeFailure::Worker("неожиданный ответ".into())),
        }
    }

    /// Расшифровать письмо `raw` ключом `cek` (Ф2): тело (IR), вложения, подпись внутри (DER цепочки — в
    /// `Opened::inner.chain`).
    pub fn smime_open(
        &mut self,
        raw: &[u8],
        cek: &[u8],
        trust: &postman_smime::Trust,
        now: u64,
    ) -> Result<Result<(Opened, Option<Document>), OpenError>, SmimeFailure> {
        let bin = open_request(raw, cek, trust)?;
        let timeout = self.timeout.max(SMIME_TIMEOUT);
        match self.call_for(&Request::SmimeOpen { now, part: None }, &bin, timeout).map_err(SmimeFailure::Worker)? {
            (Response::Opened { opened, doc }, out) => match *opened {
                Ok(mut o) => {
                    let (chain, _) = postman_smime::wire::decode_response(&out).ok_or_else(|| SmimeFailure::Worker("неожиданный ответ".into()))?;
                    o.inner.chain = chain;
                    Ok(Ok((o, doc)))
                }
                Err(e) => Ok(Err(e)),
            },
            (Response::Err(e), _) => Err(SmimeFailure::Worker(e)),
            _ => Err(SmimeFailure::Worker("неожиданный ответ".into())),
        }
    }

    /// Письмо, расшифрованное СКЗИ в ядре (ГОСТ): тело, вложения, подпись внутри; `part` — байты вложения.
    pub fn smime_inner(
        &mut self,
        inner: &[u8],
        from: &str,
        trust: &postman_smime::Trust,
        now: u64,
        part: Option<u32>,
    ) -> Result<Result<Inner, OpenError>, SmimeFailure> {
        if inner.len() > postman_smime::MAX_MESSAGE {
            return Err(SmimeFailure::TooLarge);
        }
        let bin = Zeroizing::new(postman_smime::wire::encode_request(trust, inner));
        let timeout = self.timeout.max(SMIME_TIMEOUT);
        match self.call_for(&Request::SmimeInner { now, from: from.to_owned(), part }, &bin, timeout).map_err(SmimeFailure::Worker)? {
            (Response::Part, out) => Ok(Ok((None, None, Zeroizing::new(out)))),
            (Response::Opened { opened, doc }, out) => match *opened {
                Ok(mut o) => {
                    let (chain, _) = postman_smime::wire::decode_response(&out).ok_or_else(|| SmimeFailure::Worker("неожиданный ответ".into()))?;
                    o.inner.chain = chain;
                    Ok(Ok((Some(o), doc, Zeroizing::new(Vec::new()))))
                }
                Err(e) => Ok(Err(e)),
            },
            (Response::Err(e), _) => Err(SmimeFailure::Worker(e)),
            _ => Err(SmimeFailure::Worker("неожиданный ответ".into())),
        }
    }

    /// Подготовить сертификаты получателей (Ф4): проверка цепочки к якорям `trust`, назначения и срока — в
    /// песочнице; ответ — значения ключей по порядку `certs`.
    pub fn smime_prepare(
        &mut self,
        certs: &[Vec<u8>],
        trust: &postman_smime::Trust,
        now: u64,
    ) -> Result<Vec<Result<postman_smime::prepare::Prepared, String>>, SmimeFailure> {
        let bin = postman_smime::wire::encode_prepare(trust, certs);
        if bin.len() > MAX_FRAME {
            return Err(SmimeFailure::TooLarge);
        }
        let timeout = self.timeout.max(SMIME_TIMEOUT);
        match self.call_for(&Request::SmimePrepare { now }, &bin, timeout).map_err(SmimeFailure::Worker)? {
            (Response::Prepared(list), _) if list.len() == certs.len() => Ok(list),
            (Response::Err(e), _) => Err(SmimeFailure::Worker(e)),
            _ => Err(SmimeFailure::Worker("неожиданный ответ".into())),
        }
    }

    /// Отзыв ГОСТ без СКЗИ (12.1д): проверить по CRL `crl` (скачан ядром) сертификат `cert`, выпущенный `issuer`
    /// (оба — из цепочки, которую воркер уже довёл до якоря). Подпись CRL, серийный номер и срок — в песочнице;
    /// `Ok(Err(_))` — CRL не годится (итог — «не проверен»).
    pub fn smime_crl(
        &mut self,
        cert: &[u8],
        issuer: &[u8],
        crl: &[u8],
        now: u64,
    ) -> Result<Result<postman_smime::crl::Checked, postman_smime::crl::CrlError>, SmimeFailure> {
        if crl.len() > postman_smime::crl::MAX_CRL {
            return Err(SmimeFailure::TooLarge);
        }
        let bin = postman_smime::wire::encode_crl(cert, issuer, crl);
        if bin.len() > MAX_FRAME {
            return Err(SmimeFailure::TooLarge);
        }
        let timeout = self.timeout.max(SMIME_TIMEOUT);
        match self.call_for(&Request::SmimeCrl { now }, &bin, timeout).map_err(SmimeFailure::Worker)? {
            (Response::Crl(r), _) => Ok(r),
            (Response::Err(e), _) => Err(SmimeFailure::Worker(e)),
            _ => Err(SmimeFailure::Worker("неожиданный ответ".into())),
        }
    }

    /// Байты вложения `index` расшифрованного письма (только в памяти).
    pub fn smime_part(&mut self, raw: &[u8], cek: &[u8], index: u32) -> Result<Result<Zeroizing<Vec<u8>>, OpenError>, SmimeFailure> {
        let bin = open_request(raw, cek, &postman_smime::Trust::default())?;
        let timeout = self.timeout.max(SMIME_TIMEOUT);
        match self.call_for(&Request::SmimeOpen { now: 0, part: Some(index) }, &bin, timeout).map_err(SmimeFailure::Worker)? {
            (Response::Part, out) => Ok(Ok(Zeroizing::new(out))),
            (Response::Opened { opened, .. }, _) => match *opened {
                Err(e) => Ok(Err(e)),
                Ok(_) => Err(SmimeFailure::Worker("неожиданный ответ".into())),
            },
            (Response::Err(e), _) => Err(SmimeFailure::Worker(e)),
            _ => Err(SmimeFailure::Worker("неожиданный ответ".into())),
        }
    }

    /// Разобрать письмо целиком (IMAP, S7-04б) в песочнице: тело и вложения.
    pub fn mime(&mut self, raw: &[u8]) -> Result<mime::Mime, String> {
        match self.call(&Request::Mime, raw)?.0 {
            Response::Mime(m) => Ok(*m),
            Response::Err(e) => Err(e),
            _ => Err("неожиданный ответ".into()),
        }
    }

    /// Байты вложения письма (номер из [`Client::mime`]) — в песочнице.
    pub fn mime_part(&mut self, raw: &[u8], index: u32) -> Result<Vec<u8>, String> {
        match self.call(&Request::MimePart { index }, raw)? {
            (Response::Part, bytes) => Ok(bytes),
            (Response::Err(e), _) => Err(e),
            _ => Err("неожиданный ответ".into()),
        }
    }

    /// Письмо целиком для хранилища (S8-07) — в песочнице: диапазоны крупных листовых частей в `raw` и их
    /// раскодированные байты подряд, по порядку частей. Ответ сверен с письмом: диапазоны внутри него, по порядку.
    pub fn mime_split(&mut self, raw: &[u8]) -> Result<(mime::Split, Vec<u8>), String> {
        if raw.len() > mime::MAX_SPLIT {
            return Err("письмо слишком большое".into());
        }
        let timeout = self.timeout.max(SPLIT_TIMEOUT);
        match self.call_for(&Request::MimeSplit, raw, timeout)? {
            (Response::Split(split), bytes) if split.fits(raw.len(), bytes.len()) => Ok((split, bytes)),
            (Response::Err(e), _) => Err(e),
            _ => Err("неожиданный ответ".into()),
        }
    }

    /// Разобрать приглашение (.ics) в песочнице.
    pub fn parse_calendar(&mut self, ics: &[u8]) -> Result<serde_json::Value, String> {
        match self.call(&Request::Calendar, ics)?.0 {
            Response::Calendar(v) => Ok(v),
            Response::Err(e) => Err(e),
            _ => Err("неожиданный ответ".into()),
        }
    }

    /// Разобрать карточки vCard (CardDAV) в песочнице → JSContact каждой.
    pub fn parse_contacts(&mut self, vcf: &[u8]) -> Result<Vec<serde_json::Value>, String> {
        match self.call(&Request::Contacts, vcf)?.0 {
            Response::Contacts(v) => Ok(v),
            Response::Err(e) => Err(e),
            _ => Err("неожиданный ответ".into()),
        }
    }

    fn call(&mut self, req: &Request, bin: &[u8]) -> Result<(Response, Vec<u8>), String> {
        self.call_for(req, bin, self.timeout)
    }

    fn call_for(&mut self, req: &Request, bin: &[u8], timeout: Duration) -> Result<(Response, Vec<u8>), String> {
        let json = serde_json::to_vec(req).map_err(|e| e.to_string())?;
        let max = max_bin(req);
        let mut result = self.exchange(&json, bin, max, timeout);
        // Воркер не ответил ни разу в полной изоляции (например, AppContainer не может прочитать
        // свой exe в профиле пользователя) — одна попытка с ослабленной, уровень станет «частичная».
        if let Err(strict) = &result
            && !self.relaxed
            && self.level == Level::Full
            && self.running.as_ref().is_some_and(|r| !r.answered)
        {
            self.running = None;
            self.relaxed = true;
            self.fallback = Some(format!("в полной изоляции воркер не ответил: {strict}"));
            result = self.exchange(&json, bin, max, timeout).map_err(|e| format!("{e}; в полной изоляции: {strict}"));
        }
        if result.is_err() {
            // Таймаут, падение или ошибка песочницы: процесс больше не используем.
            self.running = None;
        }
        let (json, bin) = result?;
        Ok((serde_json::from_slice(&json).map_err(|e| e.to_string())?, bin))
    }

    fn exchange(&mut self, json: &[u8], bin: &[u8], max_bin: usize, timeout: Duration) -> Result<(Vec<u8>, Vec<u8>), String> {
        if self.running.is_none() {
            self.running = Some(self.start()?);
        }
        let Some(r) = self.running.as_mut() else { return Err("воркер не запущен".into()) };
        r.max_bin.store(max_bin, Ordering::SeqCst);
        write_msg(&mut r.stdin, json, bin, max_bin).map_err(|e| format!("воркер недоступен: {e}"))?;
        match r.responses.recv_timeout(timeout) {
            Ok(Ok(msg)) => {
                r.answered = true;
                Ok(msg)
            }
            Ok(Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => Err(match (r.exit_code)() {
                Some(code) => format!("воркер завершился, код {}", exit_name(code)),
                None => "воркер завершился".into(),
            }),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(format!("воркер не ответил за {timeout:?}")),
        }
    }

    fn start(&mut self) -> Result<Running, String> {
        let p = sandbox::spawn(&self.exe, self.relaxed).map_err(|e| format!("запуск {}: {e}", self.exe.display()))?;
        self.level = p.level;
        match (p.level, p.fallback) {
            (Level::Full, _) => self.fallback = None,
            (_, Some(reason)) => self.fallback = Some(reason),
            _ => {}
        }
        let (tx, rx) = mpsc::channel();
        let mut stdout = p.stdout;
        let max_bin = Arc::new(AtomicUsize::new(MAX_FRAME));
        let limit = Arc::clone(&max_bin);
        std::thread::Builder::new()
            .name("worker-reader".into())
            .spawn(move || {
                loop {
                    let r = read_frame(&mut stdout, MAX_FRAME).and_then(|json| Ok((json, read_frame(&mut stdout, limit.load(Ordering::SeqCst))?)));
                    let stop = r.is_err();
                    if tx.send(r).is_err() || stop {
                        break;
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Running { stdin: p.stdin, responses: rx, max_bin, kill: p.kill, exit_code: p.exit_code, answered: false })
    }
}

/// Разбор письма, расшифрованного СКЗИ: итог и тело (`part` — `None`) или байты вложения.
pub type Inner = (Option<Opened>, Option<Document>, Zeroizing<Vec<u8>>);

/// Запрос расшифровки: ключ, якоря, письмо; больше кадра — «слишком большое».
fn open_request(raw: &[u8], cek: &[u8], trust: &postman_smime::Trust) -> Result<Zeroizing<Vec<u8>>, SmimeFailure> {
    if raw.len() > postman_smime::MAX_MESSAGE {
        return Err(SmimeFailure::TooLarge);
    }
    let bin = postman_smime::wire::encode_open(trust, raw, cek);
    if bin.len() > MAX_FRAME {
        return Err(SmimeFailure::TooLarge);
    }
    Ok(bin)
}

/// Код завершения с расшифровкой частых NTSTATUS.
fn exit_name(code: u32) -> String {
    let name = match code {
        5 => " (превышено время разбора)",
        0xC000_0005 => " (нарушение доступа к памяти)",
        0xC000_0022 => " (нет доступа — вероятно, AppContainer не может прочитать файлы воркера)",
        0xC000_0135 => " (не найдена DLL)",
        0xC000_0139 => " (не найдена точка входа DLL)",
        0xC000_0142 => " (ошибка инициализации DLL)",
        0xC000_0409 => " (аварийное завершение / fail-fast)",
        0xC000_0602 => " (fail-fast)",
        0xC000_0374 => " (повреждение кучи)",
        0xC000_00FD => " (переполнение стека)",
        0xC000_0017 | 0xC000_012D => " (не хватило памяти)",
        _ => "",
    };
    if code > 0xFFFF { format!("0x{code:08X}{name}") } else { format!("{code}{name}") }
}

#[cfg(test)]
mod tests {
    #[test]
    fn vcard_becomes_jscontact() {
        let vcf = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:c1\r\nFN:Иван Петров\r\nEMAIL;PREF=1:ivan@example.test\r\nTEL:+7 900\r\nORG:Рога;Отдел\r\nTITLE:Директор\r\nEND:VCARD\r\nBEGIN:VCARD\r\nVERSION:3.0\r\nFN:Вторая\r\nN:;Вторая;;;\r\nEND:VCARD\r\n";
        let cards = super::parse_contacts(vcf.as_bytes()).unwrap();
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0]["name"]["full"], "Иван Петров");
        assert_eq!(cards[0]["uid"], "c1");
        assert_eq!(cards[0]["titles"]["k1"]["name"], "Директор");
        assert_eq!(cards[1]["name"]["full"], "Вторая");
        assert!(super::parse_contacts(b"BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n").is_err());
        assert!(super::parse_contacts(b"").is_err());
    }
}
