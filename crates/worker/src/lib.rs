//! Изолированный воркер разбора писем (HANDOFF §3a).
//!
//! UI держит один процесс `postman-worker` и общается с ним сообщениями
//! «JSON-заголовок + бинарная часть» (каждая — `u32 LE длина + байты`)
//! по stdin/stdout. Бинарная часть — исходная картинка в запросе и RGBA в ответе. Воркер запирает себя до чтения
//! первого запроса: без сети, без файловой системы, без дочерних процессов,
//! на Windows — без win32k. Зависание или падение воркера стоит одного письма:
//! клиент убивает процесс и поднимает новый.
//!
//! Модули просмотра WASM (S8-24, [`wasm`]) — в отдельном процессе `postman-viewer` с той же песочницей и тем же
//! обменом ([`Client::viewer`]).

mod ics;
pub use ics::Cut;
pub mod mapi;
pub mod mime;
pub mod preview;
mod sandbox;
pub mod smime;
pub mod wasm;

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use postman_ir::Document;
use postman_smime::envelope::{Envelope, OpenError, Opened};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

pub use preview::{Entry, Listing, OfficeFormat, OfficeScan, Preview, PreviewKind, Section, SectionTitle};
/// Распределитель с пределом кучи для `#[global_allocator]` процессов песочницы на macOS (MAC-03): `RLIMIT_AS` там не
/// действует. Приложению и тестам не нужен.
#[cfg(target_os = "macos")]
#[doc(hidden)]
pub use sandbox::Limited;
pub use sandbox::{Level, Namespaces};

/// Код выхода воркера, которого остановил сторож времени: разбор одного запроса дольше [`TIME_LIMIT`].
pub const EXIT_TIME: i32 = 5;
/// Код выхода процесса песочницы сверх предела памяти (macOS: распределитель `Limited`; на Windows предел ставит Job
/// Object, на Linux — `RLIMIT_AS`: отказ в памяти там — аварийное завершение, `SIGABRT`).
pub const EXIT_MEMORY: i32 = 6;
/// Предел времени на один запрос внутри воркера (сторож в [`serve`]); клиент ждёт чуть дольше.
pub const TIME_LIMIT: Duration = Duration::from_secs(8);
/// Сколько клиент ждёт ответа на просмотр вложения: сторож воркера срабатывает раньше.
pub const PREVIEW_TIMEOUT: Duration = Duration::from_secs(10);

/// Сколько клиент ждёт разбора письма на части (до [`mime::MAX_SPLIT`]): сторож воркера срабатывает раньше.
pub const SPLIT_TIMEOUT: Duration = Duration::from_secs(10);
/// Сколько клиент ждёт разбора календаря (до [`MAX_ICS`]): сторож воркера срабатывает раньше.
pub const ICS_TIMEOUT: Duration = Duration::from_secs(10);

/// Больше этого не принимаем ни в одну сторону (кроме бинарной части `MimeSplit`).
const MAX_FRAME: usize = 64 * 1024 * 1024;
/// Бинарная часть запроса и ответа `MimeSplit` и `Mapi`: письмо целиком и его раскодированные части.
const MAX_SPLIT_FRAME: usize = mime::MAX_SPLIT;

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    /// Письмо целиком (RFC 5322) в бинарной части → тело и вложения (S7-04б, IMAP: MIME разбирает Postman).
    Mime,
    /// Байты вложения `index` письма из бинарной части → в бинарной части ответа.
    MimePart {
        index: u32,
    },
    /// Письмо целиком для хранилища (S8-07, ADR 0006 §2) в бинарной части → [`mime::Split`] (части и `Message-ID`),
    /// раскодированные части подряд — в бинарной части ответа.
    MimeSplit,
    /// Заголовки цепочки писем (S8-36, список IMAP): в бинарной части — строки `In-Reply-To` и `References` каждого
    /// письма как пришли ([`mime::encode_heads`]) → [`mime::Thread`] каждого, по порядку.
    Threads,
    Html(String),
    Text(String),
    /// Приглашение iCalendar (.ics) в бинарной части; ответ — METHOD и JSCalendar (JSON).
    Calendar,
    /// Карточки vCard (.vcf, одна или несколько) в бинарной части; ответ — JSContact (JSON) каждой (S7-05б, CardDAV).
    Contacts,
    /// Занятость по ссылке (S8-42): iCalendar (VFREEBUSY или события) в бинарной части → интервалы в `[from, to)`;
    /// `tz` — пояс событий на весь день и без пояса.
    FreeBusy {
        from: i64,
        to: i64,
        tz: String,
    },
    /// Календарь подписки (S8-44): iCalendar в бинарной части → имя и события `[from, to)` в поясе `tz`.
    Ics {
        from: i64,
        to: i64,
        tz: String,
    },
    /// Картинка (PNG/JPEG/GIF/WebP/BMP/ICO, у TIFF — первая страница) в бинарной части; ответ — RGBA не больше
    /// `max_side` по стороне.
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
    /// Отзыв по OCSP (Linux, LIN-05d): в бинарной части — сертификат, издатель, nonce запроса и ответ ответчика,
    /// полученный ядром (`postman_smime::wire::encode_ocsp`). Ответ — «не отозван / отозван» или почему не годен.
    SmimeOcsp {
        now: u64,
    },
    /// Письмо целиком (S8-29) в бинарной части → свойства MAPI для MSG и PST ([`mapi::mapi`]); `seen` — прочитано.
    /// Ответ — письмо MAPI байтами в бинарной части.
    Mapi {
        seen: bool,
    },
    /// Документ OOXML с макросами в бинарной части → копия без макросов там же (T2-29, «Открыть без макросов»).
    StripMacros,
    /// Модуль просмотра (только `postman-viewer`, S8-24): в бинарной части — модуль (`wasm` байт), за ним вход.
    /// Ответ — [`wasm::ViewerOutput`] или [`wasm::Refusal`]; пиксели картинки — в бинарной части.
    ViewModule {
        wasm: u32,
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
    match smime::inner_body(content) {
        (Some(html), _) => Some(postman_ir::from_html(&html)),
        (None, Some(text)) => Some(postman_ir::from_plain(&text)),
        (None, None) => None,
    }
}

fn smime_envelope(bin: &[u8]) -> Response {
    Response::Envelope(smime::envelope(bin))
}

/// Расшифровка в песочнице: тело → IR; вложение — байты (только в памяти, на диск — нигде).
fn smime_open(bin: &[u8], now: u64, part: Option<u32>) -> (Response, Vec<u8>) {
    let Some((trust, message, cek)) = postman_smime::wire::decode_open(bin) else {
        return (Response::Err("запрос расшифровки повреждён".into()), Vec::new());
    };
    if let Some(index) = part {
        return match smime::part(message, cek, index) {
            Ok(bytes) => (Response::Part, bytes.to_vec()),
            Err(e) => (Response::Opened { opened: Box::new(Err(e)), doc: None }, Vec::new()),
        };
    }
    match smime::open(message, cek, &trust, now) {
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
        return match smime::part_inner(inner, index) {
            Ok(bytes) => (Response::Part, bytes.to_vec()),
            Err(e) => (Response::Opened { opened: Box::new(Err(e)), doc: None }, Vec::new()),
        };
    }
    match smime::open_inner(inner, from, postman_smime::envelope::Cipher::Gost28147, &trust, now) {
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
    let report = smime::verify(message, &trust, now);
    let doc = report.content.as_deref().and_then(inner_document);
    let out = postman_smime::wire::encode_response(&report.chain, None);
    (Response::Smime { report: Box::new(report), doc }, out)
}

#[cfg(debug_assertions)]
#[doc(hidden)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Probe {
    ReadFile(String),
    Connect(String),
    Spawn(String),
    Hang,
    /// Занять столько байт кучи (нулями: страницы не трогаются) — предел памяти должен сработать раньше.
    Alloc(u64),
    /// Записать столько байт в унаследованный stderr; ответ — «stderr: записано».
    Stderr(u32),
    /// macOS: найти службу Mach у launchd.
    MachLookup(String),
    /// macOS: прочитать sysctl.
    Sysctl(String),
    /// Переменная окружения задана (пути профиля воркеру не передаются, SBX-02).
    Env(String),
    /// Дескриптор с этим номером открыт (унаследован от клиента; SBX-05). Только Unix.
    Fd(i32),
}

#[cfg(debug_assertions)]
fn probe(p: Probe) -> Response {
    if let Probe::Stderr(n) = p {
        let line = b"postman-worker: stderr probe
";
        let text: Vec<u8> = line.iter().copied().cycle().take(n as usize).collect();
        return Response::Err(match io::stderr().write_all(&text) {
            Ok(()) => "stderr: записано".into(),
            Err(e) => format!("stderr: {e}"),
        });
    }
    match attempt(p) {
        Ok(()) => Response::Err("ДОСТУП РАЗРЕШЁН".into()),
        Err(e) => Response::Err(format!("запрещено: {e}")),
    }
}

/// Проверка [`Probe`] в вызывающем процессе, без песочницы: тест убеждается, что сама проверка умеет удаваться и отказ в
/// воркере — от песочницы, а не от ошибки в проверке (службы Mach, sysctl). Только отладочная сборка.
#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn probe_here(p: Probe) -> Result<(), String> {
    attempt(p)
}

#[cfg(debug_assertions)]
fn attempt(p: Probe) -> Result<(), String> {
    match p {
        Probe::ReadFile(path) => std::fs::read(path).map(|_| ()).map_err(|e| e.to_string()),
        Probe::Connect(addr) => std::net::TcpStream::connect(addr).map(drop).map_err(|e| e.to_string()),
        Probe::Spawn(cmd) => std::process::Command::new(cmd).status().map(drop).map_err(|e| e.to_string()),
        Probe::Fd(fd) => sandbox::probe_fd(fd),
        Probe::Env(name) => std::env::var_os(&name).map(drop).ok_or_else(|| format!("{name} не задана")),
        Probe::Hang => loop {
            std::hint::spin_loop();
        },
        Probe::Alloc(n) => usize::try_from(n).map_err(|e| e.to_string()).map(|n| drop(std::hint::black_box(vec![0u8; n]))),
        Probe::Stderr(n) => io::stderr().write_all(&vec![b'.'; n as usize]).map_err(|e| e.to_string()),
        #[cfg(target_os = "macos")]
        Probe::MachLookup(name) => sandbox::probe::mach_lookup(&name),
        #[cfg(target_os = "macos")]
        Probe::Sysctl(name) => sandbox::probe::sysctl(&name),
        #[cfg(not(target_os = "macos"))]
        Probe::MachLookup(_) | Probe::Sysctl(_) => Err("только macOS".into()),
    }
}

#[derive(Debug, Serialize, Deserialize)]
enum Response {
    Ok(Document),
    Mime(Box<mime::Mime>),
    /// Листовые части письма; раскодированные — в бинарной части.
    Split(mime::Split),
    /// Заголовки цепочки писем по порядку запроса.
    Threads(Vec<mime::Thread>),
    /// RGBA8 в бинарной части.
    Image {
        width: u32,
        height: u32,
    },
    Calendar(serde_json::Value),
    Contacts(Vec<serde_json::Value>),
    /// Интервалы и что отсекли пределы: журнала в песочнице нет, запись делает приложение ([`Client::take_cut`]).
    FreeBusy(Vec<BusySpan>, Cut),
    Ics(IcsCalendar, Cut),
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
    /// Итог проверки ответа OCSP (LIN-05d).
    Ocsp(Result<postman_smime::crl::Checked, postman_smime::ocsp::OcspError>),
    /// Письмо MAPI — в бинарной части.
    Mapi,
    /// Ответ модуля просмотра; пиксели картинки — в бинарной части.
    View(Result<wasm::ViewerOutput, wasm::Refusal>),
    Err(String),
}

/// Декодированная картинка.
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

fn decode_image(bytes: &[u8], max_side: u32) -> Result<Rgba, String> {
    decode(bytes, max_side, 0).map(|d| d.img)
}

/// Декодированная картинка с исходным размером, форматом («PNG») и страницей: показанная (с нуля) и всего — у TIFF
/// страницы, как у PDF, у остальных — 0 и 1.
pub(crate) struct Decoded {
    pub img: Rgba,
    pub source: (u32, u32),
    pub format: &'static str,
    pub page: u32,
    pub pages: u32,
}

/// Страниц TIFF — не больше (цепочка IFD дальше не читается).
const MAX_TIFF_PAGES: usize = 1000;

/// Картинка не больше `max_side` по стороне; у TIFF — страница `page` (за последней — последняя). ICO — наибольший
/// кадр (при равной глубине цвета).
pub(crate) fn decode(bytes: &[u8], max_side: u32, page: u32) -> Result<Decoded, String> {
    use image::codecs::{
        bmp::BmpDecoder, gif::GifDecoder, ico::IcoDecoder, jpeg::JpegDecoder, png::PngDecoder, tiff::TiffDecoder, webp::WebPDecoder,
    };
    use image::{DynamicImage, ImageDecoder, ImageError, ImageFormat, ImageResult, Limits};

    // Как `ImageReader::decode`, но с декодером нужного формата напрямую: `ImageReader` выбирает декодер
    // во время работы и тянет в exe все форматы, включённые в `image` по рабочей области (gpui включает
    // exr, dds, hdr…), хотя всё, кроме этих семи, мы отвергаем: +1,2 МиБ к воркеру (docs/SIZE.md).
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
    // BigTIFF `image` по сигнатуре не узнаёт, а `tiff` читает.
    let big_tiff = bytes.starts_with(b"II+\0") || bytes.starts_with(b"MM\0+");
    let guessed = if big_tiff { Ok(ImageFormat::Tiff) } else { image::guess_format(bytes) };
    let (mut shown, mut pages) = (0, 1);
    let (decoded, format) = match guessed {
        Ok(ImageFormat::Png) => (PngDecoder::with_limits(r, limits.clone()).and_then(|d| load(d, limits)), "PNG"),
        Ok(ImageFormat::Jpeg) => (JpegDecoder::new(r).and_then(|d| load(d, limits)), "JPEG"),
        Ok(ImageFormat::Gif) => (GifDecoder::new(r).and_then(|d| load(d, limits)), "GIF"),
        Ok(ImageFormat::WebP) => (WebPDecoder::new(r).and_then(|d| load(d, limits)), "WebP"),
        Ok(ImageFormat::Bmp) => (BmpDecoder::new(r).and_then(|d| load(d, limits)), "BMP"),
        Ok(ImageFormat::Ico) => (IcoDecoder::new(r).and_then(|d| load(d, limits)), "ICO"),
        Ok(ImageFormat::Tiff) => {
            let (file, at, count) = tiff_page(bytes, page)?;
            (shown, pages) = (at, count);
            (TiffDecoder::new(io::Cursor::new(&file[..])).and_then(|d| load(d, limits)), "TIFF")
        }
        _ => return Err("формат картинки не поддерживается".into()),
    };
    let mut img = decoded.map_err(|e| match e {
        // Например, TIFF факса CCITT G3 (G4 `tiff` умеет).
        ImageError::Unsupported(_) => "формат картинки не поддерживается".to_string(),
        e => e.to_string(),
    })?;
    let source = (img.width(), img.height());
    let max_side = max_side.clamp(16, 4096);
    if img.width() > max_side || img.height() > max_side {
        img = img.thumbnail(max_side, max_side);
    }
    let rgba = img.into_rgba8();
    let img = Rgba { width: rgba.width(), height: rgba.height(), pixels: rgba.into_raw() };
    Ok(Decoded { img, source, format, page: shown, pages })
}

/// Страница TIFF: страницы — цепочка IFD (смещение первой — в заголовке, следующей — в конце каждой). Страница
/// `page` (за последней — последняя) — копией файла, у которой первая IFD — нужная. Итог — файл, показанная
/// страница и всего страниц (не больше [`MAX_TIFF_PAGES`]; цикл в цепочке — её конец).
fn tiff_page(bytes: &[u8], page: u32) -> Result<(std::borrow::Cow<'_, [u8]>, u32, u32), String> {
    let le = match bytes.get(..2) {
        Some(b"II") => true,
        Some(b"MM") => false,
        _ => return Err("TIFF не разобран".into()),
    };
    let int = |at: usize, len: usize| -> Option<u64> {
        let b = bytes.get(at..at.checked_add(len)?)?;
        let fold = |n: u64, &x: &u8| (n << 8) | u64::from(x);
        Some(if le { b.iter().rev().fold(0, fold) } else { b.iter().fold(0, fold) })
    };
    // Классический TIFF: смещения по 4 байта, IFD — 2 байта счётчика и записи по 12; BigTIFF — 8, 8 и 20.
    let (first, wide, head, entry) = match int(2, 2) {
        Some(42) => (4, 4, 2, 12),
        Some(43) => (8, 8, 8, 20),
        _ => return Err("TIFF не разобран".into()),
    };
    let mut offsets: Vec<u64> = Vec::new();
    let mut next = int(first, wide);
    while let Some(at) = next.filter(|&o| o != 0 && !offsets.contains(&o)) {
        if offsets.len() >= MAX_TIFF_PAGES {
            break;
        }
        let Some(start) = usize::try_from(at).ok() else { break };
        let Some(count) = int(start, head).and_then(|n| usize::try_from(n).ok()) else { break };
        offsets.push(at);
        next = count.checked_mul(entry).and_then(|n| n.checked_add(start + head)).and_then(|end| int(end, wide));
    }
    let Some(last) = offsets.len().checked_sub(1) else { return Err("в TIFF нет страниц".into()) };
    let shown = (page as usize).min(last);
    if shown == 0 {
        return Ok((std::borrow::Cow::Borrowed(bytes), 0, offsets.len() as u32));
    }
    let mut file = bytes.to_vec();
    let at = offsets[shown];
    let value: Vec<u8> = if wide == 8 {
        if le { at.to_le_bytes().to_vec() } else { at.to_be_bytes().to_vec() }
    } else {
        let at = u32::try_from(at).map_err(|_| "TIFF не разобран".to_string())?;
        if le { at.to_le_bytes().to_vec() } else { at.to_be_bytes().to_vec() }
    };
    file[first..first + wide].copy_from_slice(&value);
    Ok((std::borrow::Cow::Owned(file), shown as u32, offsets.len() as u32))
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

/// Вид занятости интервала по ссылке (RFC 5545 `FBTYPE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BusyType {
    Busy,
    Tentative,
    Unavailable,
}

/// Интервал занятости (Unix-секунды).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusySpan {
    pub start: i64,
    pub end: i64,
    pub kind: BusyType,
}

/// Событие календаря по ссылке (S8-44), экземпляр повтора — отдельно.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IcsEvent {
    pub uid: String,
    pub title: String,
    pub location: String,
    pub description: String,
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    pub tentative: bool,
    /// `TRANSP:TRANSPARENT`: занятости не даёт.
    pub transparent: bool,
    pub recurring: bool,
}

/// Календарь по ссылке: имя (`X-WR-CALNAME`) и события периода.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IcsCalendar {
    pub name: Option<String>,
    pub events: Vec<IcsEvent>,
}

/// Файл занятости — не больше: календарь по чужой ссылке.
pub const MAX_FREE_BUSY: usize = 2 * 1024 * 1024;
/// Календарь подписки — не больше.
pub const MAX_ICS: usize = 8 * 1024 * 1024;
/// Интервалов занятости — не больше.
const MAX_BUSY: usize = 20_000;
/// Событий периода — не больше.
const MAX_ICS_EVENTS: usize = 5_000;

fn parse_ical(bytes: &[u8], max: usize) -> Result<(calcard::icalendar::ICalendar, &str), String> {
    if bytes.len() > max {
        return Err("календарь слишком большой".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "календарь не в UTF-8".to_string())?;
    match calcard::Parser::new(text).entry() {
        calcard::Entry::ICalendar(c) => Ok((c, text)),
        _ => Err("не iCalendar".into()),
    }
}

/// События в `[from, to)`: повторы развёрнуты ([`ics::instances`]), без пояса — время пояса `tz`; отменённые не берутся.
fn events_of(ical: &calcard::icalendar::ICalendar, tz: &str, from: i64, to: i64) -> (Vec<IcsEvent>, Cut) {
    use calcard::common::timezone::Tz;
    use calcard::icalendar::{ICalendarProperty as Prop, ICalendarStatus, ICalendarTransparency, ICalendarValue};
    let zone: Tz = tz.parse().unwrap_or(Tz::UTC);
    let mut out = Vec::new();
    let (instances, mut cut) = ics::instances(ical, zone, from, to);
    for instance in instances {
        let Some(comp) = ical.components.get(instance.comp) else { continue };
        let value = |test: &dyn Fn(&ICalendarValue) -> bool| comp.entries.iter().any(|e| e.values.iter().any(test));
        if value(&|v| matches!(v, ICalendarValue::Status(ICalendarStatus::Cancelled))) {
            continue;
        }
        if out.len() >= MAX_ICS_EVENTS {
            cut.events_dropped = cut.events_dropped.saturating_add(1);
            continue;
        }
        let text = |name: Prop| {
            let entry = comp.entries.iter().find(|e| e.name == name);
            let values = entry.map(|e| e.values.as_slice()).unwrap_or_default();
            values.iter().find_map(|v| if let ICalendarValue::Text(t) = v { Some(t.clone()) } else { None }).unwrap_or_default()
        };
        out.push(IcsEvent {
            uid: text(Prop::Uid),
            title: text(Prop::Summary),
            location: text(Prop::Location),
            description: text(Prop::Description),
            start: instance.start,
            end: instance.end,
            all_day: instance.all_day,
            tentative: value(&|v| matches!(v, ICalendarValue::Status(ICalendarStatus::Tentative))),
            transparent: value(&|v| matches!(v, ICalendarValue::Transparency(ICalendarTransparency::Transparent))),
            recurring: comp.entries.iter().any(|e| matches!(e.name, Prop::Rrule | Prop::Rdate | Prop::RecurrenceId)),
        });
    }
    out.sort_by(|a, b| (a.start, &a.uid).cmp(&(b.start, &b.uid)));
    (out, cut)
}

/// iCalendar → занятость в `[from, to)`: `FREEBUSY` из VFREEBUSY и непрозрачные неотменённые события с повторами
/// (опубликованные календари Outlook и Google — события, а не VFREEBUSY); события на весь день и без пояса — в поясе `tz`.
/// Чужие данные — только в песочнице.
pub fn parse_free_busy(bytes: &[u8], from: i64, to: i64, tz: &str) -> Result<Vec<BusySpan>, String> {
    free_busy_of(bytes, from, to, tz).map(|got| got.0)
}

/// [`parse_free_busy`] и что отсекли пределы.
fn free_busy_of(bytes: &[u8], from: i64, to: i64, tz: &str) -> Result<(Vec<BusySpan>, Cut), String> {
    use calcard::icalendar::{
        ICalendarComponentType as Kind, ICalendarFreeBusyType as Fb, ICalendarParameterName as Param, ICalendarParameterValue as Value,
        ICalendarPeriod as Period, ICalendarProperty as Prop, ICalendarValue,
    };
    let (ical, _) = parse_ical(bytes, MAX_FREE_BUSY)?;
    let mut out = Vec::new();
    let mut dropped = 0u32;
    let mut push = |start: i64, end: i64, kind: BusyType| {
        if start < to && end > from && start < end {
            if out.len() < MAX_BUSY {
                out.push(BusySpan { start, end, kind });
            } else {
                dropped = dropped.saturating_add(1);
            }
        }
    };
    for comp in ical.components.iter().filter(|c| c.component_type == Kind::VFreebusy) {
        for entry in comp.entries.iter().filter(|e| e.name == Prop::Freebusy) {
            let fb = entry.params.iter().find(|p| p.name == Param::Fbtype).map(|p| &p.value);
            let kind = match fb {
                Some(Value::Fbtype(Fb::Free)) => continue,
                Some(Value::Fbtype(Fb::BusyTentative)) => BusyType::Tentative,
                Some(Value::Fbtype(Fb::BusyUnavailable)) => BusyType::Unavailable,
                _ => BusyType::Busy,
            };
            for value in &entry.values {
                let ICalendarValue::Period(period) = value else { continue };
                let (start, end) = match period {
                    Period::Range { start, end } => (start.to_timestamp(), end.to_timestamp()),
                    Period::Duration { start, duration } => {
                        let s = start.to_timestamp();
                        (s, s.map(|s| s.saturating_add(duration.as_seconds())))
                    }
                };
                if let (Some(s), Some(e)) = (start, end) {
                    push(s, e, kind);
                }
            }
        }
    }
    let (events, mut cut) = events_of(&ical, tz, from, to);
    for e in events.into_iter().filter(|e| !e.transparent) {
        push(e.start, e.end, if e.tentative { BusyType::Tentative } else { BusyType::Busy });
    }
    cut.busy_dropped = dropped;
    out.sort_by_key(|b| (b.start, b.end));
    Ok((out, cut))
}

/// iCalendar подписки (S8-44) → имя и события `[from, to)` в поясе `tz`. Чужие данные — только в песочнице.
pub fn parse_ics(bytes: &[u8], from: i64, to: i64, tz: &str) -> Result<IcsCalendar, String> {
    ics_of(bytes, from, to, tz).map(|got| got.0)
}

/// [`parse_ics`] и что отсекли пределы.
fn ics_of(bytes: &[u8], from: i64, to: i64, tz: &str) -> Result<(IcsCalendar, Cut), String> {
    let (ical, text) = parse_ical(bytes, MAX_ICS)?;
    let name = text.lines().find_map(|l| l.trim().strip_prefix("X-WR-CALNAME:")).map(|n| n.trim().to_owned()).filter(|n| !n.is_empty());
    let (events, cut) = events_of(&ical, tz, from, to);
    Ok((IcsCalendar { name, events }, cut))
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
        Request::MimeSplit | Request::Mapi { .. } => MAX_SPLIT_FRAME,
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

/// Удалить след песочницы на диске (Windows: профиль AppContainer; ADR 0011, PAR-05) — действие
/// `postman-worker --remove-appcontainer` при удалении Postman. Следа нет — успех.
pub fn remove_appcontainer() -> Result<(), String> {
    sandbox::remove_profile()
}

/// Точка входа процесса-воркера.
pub fn serve() -> ! {
    run(TIME_LIMIT, handle)
}

/// Обмен процесса песочницы (воркера и хоста модулей): сторож `limit` на запрос, запирание, затем запросы по одному.
pub(crate) fn run(limit: Duration, handle: impl Fn(Result<Request, serde_json::Error>, &[u8]) -> (Response, Vec<u8>)) -> ! {
    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    // Шаг, которому нужен однопоточный процесс (Linux: namespaces), — до сторожа.
    let isolated = sandbox::isolate();
    // Сторож времени — до запирания (потом новые потоки могут быть запрещены): запрос дольше `limit`
    // завершает процесс; клиент видит код `EXIT_TIME` и поднимает новый процесс.
    let watchdog = watchdog(limit);
    if let Err(e) = isolated.and_then(|()| sandbox::lockdown()) {
        // Без песочницы не работаем: лучше показать письмо текстом, чем разбирать HTML без защиты.
        let json = serde_json::to_vec(&Response::Err(format!("{SANDBOX_REFUSAL}{e}"))).unwrap_or_default();
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
        let (resp, out_bin) = handle(req, &bin);
        if let Some(w) = &watchdog {
            let _ = w.send(false);
        }
        let out = serde_json::to_vec(&resp).unwrap_or_default();
        if write_msg(&mut stdout, &out, &out_bin, max).is_err() {
            std::process::exit(0);
        }
    }
}

/// Запрос воркера → ответ и его бинарная часть.
fn handle(req: Result<Request, serde_json::Error>, bin: &[u8]) -> (Response, Vec<u8>) {
    let mut out_bin = Vec::new();
    let resp = match req {
        Ok(Request::Mime) => match mime::parse(bin) {
            Ok(m) => Response::Mime(Box::new(m)),
            Err(e) => Response::Err(e),
        },
        Ok(Request::MimePart { index }) => match mime::part(bin, index) {
            Ok(bytes) => {
                out_bin = bytes;
                Response::Part
            }
            Err(e) => Response::Err(e),
        },
        Ok(Request::MimeSplit) => match mime::split(bin) {
            Ok((split, bytes)) => {
                out_bin = bytes;
                Response::Split(split)
            }
            Err(e) => Response::Err(e),
        },
        Ok(Request::Threads) => match mime::threads(bin) {
            Ok(t) => Response::Threads(t),
            Err(e) => Response::Err(e),
        },
        Ok(Request::StripMacros) => match preview::strip_macros(bin) {
            Ok(bytes) => {
                out_bin = bytes;
                Response::Part
            }
            Err(e) => Response::Err(e),
        },
        Ok(Request::Mapi { seen }) => match mapi::mapi(bin, seen).map(|m| mapi::encode(&m)) {
            Ok(bytes) if bytes.len() <= MAX_SPLIT_FRAME => {
                out_bin = bytes;
                Response::Mapi
            }
            Ok(_) => Response::Err("письмо слишком большое".into()),
            Err(e) => Response::Err(e),
        },
        Ok(Request::Html(h)) => Response::Ok(postman_ir::from_html(&h)),
        Ok(Request::Text(t)) => Response::Ok(postman_ir::from_plain(&t)),
        Ok(Request::FreeBusy { from, to, tz }) => match free_busy_of(bin, from, to, &tz) {
            Ok((v, cut)) => Response::FreeBusy(v, cut),
            Err(e) => Response::Err(e),
        },
        Ok(Request::Ics { from, to, tz }) => match ics_of(bin, from, to, &tz) {
            Ok((v, cut)) => Response::Ics(v, cut),
            Err(e) => Response::Err(e),
        },
        Ok(Request::Calendar) => match parse_calendar(bin) {
            Ok(v) => Response::Calendar(v),
            Err(e) => Response::Err(e),
        },
        Ok(Request::Contacts) => match parse_contacts(bin) {
            Ok(v) => Response::Contacts(v),
            Err(e) => Response::Err(e),
        },
        Ok(Request::Image { max_side }) => match decode_image(bin, max_side) {
            Ok(img) => {
                out_bin = img.pixels;
                Response::Image { width: img.width, height: img.height }
            }
            Err(e) => Response::Err(e),
        },
        Ok(Request::Preview { kind, page, max_side, width }) => {
            // Сбой разборщика на чужом файле — ошибка этого просмотра, а не смерть процесса.
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| preview::preview(kind, bin, page, max_side, width)));
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
            let (resp, bin) = smime_verify(bin, now);
            out_bin = bin;
            resp
        }
        Ok(Request::SmimeEnvelope) => smime_envelope(bin),
        Ok(Request::SmimeOpen { now, part }) => {
            let (resp, bin) = smime_open(bin, now, part);
            out_bin = bin;
            resp
        }
        Ok(Request::SmimeInner { now, from, part }) => {
            let (resp, bin) = smime_inner(bin, now, &from, part);
            out_bin = bin;
            resp
        }
        Ok(Request::SmimePrepare { now }) => match postman_smime::wire::decode_prepare(bin) {
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
        Ok(Request::SmimeCrl { now }) => match postman_smime::wire::decode_crl(bin) {
            Some((cert, issuer, crl)) => Response::Crl(postman_smime::crl::check(cert, issuer, crl, now)),
            None => Response::Err("запрос проверки CRL повреждён".into()),
        },
        Ok(Request::SmimeOcsp { now }) => match postman_smime::wire::decode_ocsp(bin) {
            Some((cert, issuer, nonce, response)) => Response::Ocsp(postman_smime::ocsp::check(cert, issuer, nonce, response, now)),
            None => Response::Err("OCSP request is damaged".into()),
        },
        Ok(Request::ViewModule { .. }) => Response::Err("модули просмотра — только в postman-viewer".into()),
        #[cfg(debug_assertions)]
        Ok(Request::Probe(p)) => probe(p),
        Err(e) => Response::Err(e.to_string()),
    };
    (resp, out_bin)
}

/// Поток-сторож: `true` — запрос начат, `false` — закончен. Не закончен за `limit` — выход с [`EXIT_TIME`].
/// В простое поток спит в `recv` и процессор не тратит. Поток не запустился — работаем без сторожа (клиент всё
/// равно убьёт воркер по своему таймауту).
/// Тот же поток следит за клиентом (SBX-03, аудит №44): клиент умер (сменился родитель) — выход не позже
/// [`PARENT_POLL`], на всех ОС одинаково (на Windows процесс к тому же в Job Object с `KILL_ON_JOB_CLOSE`).
fn watchdog(limit: Duration) -> Option<mpsc::Sender<bool>> {
    let (tx, rx) = mpsc::channel::<bool>();
    let parent = sandbox::parent();
    let spawned = std::thread::Builder::new().name("watchdog".into()).spawn(move || {
        let mut started: Option<Instant> = None;
        loop {
            match rx.recv_timeout(PARENT_POLL) {
                Ok(true) => started = Some(Instant::now()),
                Ok(false) => started = None,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            if sandbox::parent() != parent {
                std::process::exit(0);
            }
            if started.is_some_and(|t| t.elapsed() >= limit) {
                std::process::exit(EXIT_TIME);
            }
        }
    });
    spawned.ok().map(|_| tx)
}

/// Как часто процесс песочницы проверяет, жив ли клиент.
const PARENT_POLL: Duration = Duration::from_millis(250);

/// Начало ответа процесса, отказавшего в запирании (до первого запроса, выход 3).
const SANDBOX_REFUSAL: &str = "песочница: ";

/// Приёмник строк журнала процесса песочницы ([`Client::journal`]).
pub type Journal = Arc<dyn Fn(&str) + Send + Sync>;

/// Наибольшая длина строки stderr процесса песочницы в журнале, байт; длиннее — обрезается с пометкой.
pub const STDERR_LINE: usize = 2048;
/// Наибольшее число строк stderr процесса песочницы в журнале за секунду; сверх — одна строка WARN за секунду.
pub const STDERR_RATE: u32 = 100;

/// Важность строки stderr процесса песочницы для журнала приложения (`pets-shell/LOGGING.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalLevel {
    /// Паника, отказ в памяти: разбор не состоялся.
    Error,
    /// Остальное: процесс песочницы пишет в stderr только о необычном (запасной путь, предел частоты строк).
    Warn,
}

impl JournalLevel {
    /// Важность строки по её началу: строки процесса — `WARN …`, паника std — `thread '…' panicked at`, отказ в памяти —
    /// `memory allocation of … failed`.
    pub fn of(line: &str) -> Self {
        if line.contains("panicked at") || line.starts_with("memory allocation of ") || line.contains("memory limit exceeded") {
            Self::Error
        } else {
            Self::Warn
        }
    }
}

/// Приёмник журнала по умолчанию для всех клиентов процесса ([`set_default_journal`]).
static DEFAULT_JOURNAL: std::sync::OnceLock<Journal> = std::sync::OnceLock::new();

/// Куда писать строки stderr процессов песочницы всем клиентам, у которых не задан свой [`Client::journal`]: приложение
/// ставит один раз при запуске (журнал `tracing`). Второй вызов не меняет приёмник; `false` — уже был задан.
pub fn set_default_journal(sink: impl Fn(JournalLevel, &str) + Send + Sync + 'static) -> bool {
    DEFAULT_JOURNAL.set(Arc::new(move |line: &str| sink(JournalLevel::of(line), line))).is_ok()
}

fn default_journal(line: &str) {
    if let Some(sink) = DEFAULT_JOURNAL.get() {
        return sink(line);
    }
    // Без приёмника приложения — его stderr (в файл его направляет приложение); строка уже ограничена по длине.
    let _ = writeln!(io::stderr().lock(), "worker: {line}");
}

/// Ограничитель строк журнала: длина — [`STDERR_LINE`], частота — [`STDERR_RATE`] в секунду; первая лишняя строка
/// за секунду заменяется одной строкой WARN, остальные до конца секунды отбрасываются.
struct StderrLog {
    journal: Journal,
    window: Instant,
    lines: u32,
}

impl StderrLog {
    fn line(&mut self, bytes: &[u8], cut: bool) {
        if self.window.elapsed() >= Duration::from_secs(1) {
            self.window = Instant::now();
            self.lines = 0;
        }
        self.lines = self.lines.saturating_add(1);
        if self.lines > STDERR_RATE {
            if self.lines == STDERR_RATE + 1 {
                (self.journal)(&format!("WARN stderr rate limit: over {STDERR_RATE} lines/s, dropping the rest of this second"));
            }
            return;
        }
        let text = String::from_utf8_lossy(bytes.strip_suffix(b"\r").unwrap_or(bytes));
        if cut { (self.journal)(&format!("{text} [line truncated]")) } else { (self.journal)(&text) }
    }
}

/// Фоновое чтение stderr процесса песочницы до его закрытия: канал не переполняется (процесс не встанет на записи),
/// UI не ждёт. Строки — в `journal` с ограничением [`StderrLog`].
/// Строка, которую std пишет в stderr перед аварийным завершением при отказе в памяти (все ОС): по ней клиент называет
/// исход «превышен предел памяти» одинаково везде (SBX-03, аудит №45).
const OOM_LINE: &[u8] = b"memory allocation of ";

fn drain_stderr(mut stderr: Box<dyn Read + Send>, journal: Journal, oom: Arc<AtomicBool>) -> io::Result<()> {
    std::thread::Builder::new()
        .name("worker-stderr".into())
        .spawn(move || {
            let mut log = StderrLog { journal, window: Instant::now(), lines: 0 };
            let mut buf = [0u8; 4096];
            let mut line = Vec::with_capacity(256);
            let mut cut = false;
            loop {
                let n = match stderr.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                };
                for part in buf[..n].split_inclusive(|&b| b == b'\n') {
                    let (body, end) = match part.strip_suffix(b"\n") {
                        Some(body) => (body, true),
                        None => (part, false),
                    };
                    let room = STDERR_LINE.saturating_sub(line.len());
                    line.extend_from_slice(&body[..body.len().min(room)]);
                    cut |= body.len() > room;
                    if end {
                        if line.starts_with(OOM_LINE) && line.ends_with(b" failed") {
                            oom.store(true, Ordering::SeqCst);
                        }
                        log.line(&line, cut);
                        line.clear();
                        cut = false;
                    }
                }
            }
            if !line.is_empty() {
                log.line(&line, cut);
            }
        })
        .map(drop)
}

/// Запущенный процесс-воркер, как его видит клиент.
pub(crate) struct Process {
    pub stdin: Box<dyn Write + Send>,
    pub stdout: Box<dyn Read + Send>,
    /// Журнал процесса — канал на всех ОС (SBX-01); клиент читает его в фоне ([`drain_stderr`]).
    pub stderr: Box<dyn Read + Send>,
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
    /// В stderr была строка std об отказе в памяти ([`OOM_LINE`]).
    oom: Arc<AtomicBool>,
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
    /// Почему уровень не `Full` ([`Client::fallback`]).
    fallback: Option<String>,
    /// Предел памяти процесса (Windows: Job Object).
    memory: usize,
    /// Что отсекли пределы в последнем календаре ([`Client::take_cut`]).
    cut: Option<Cut>,
    /// Куда идут строки stderr процесса ([`Client::journal`]).
    journal: Journal,
    /// Режим namespaces процесса песочницы (Linux).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    namespaces: Namespaces,
}

impl Client {
    pub fn new(exe: PathBuf, timeout: Duration) -> Self {
        Self {
            exe,
            timeout,
            running: None,
            level: Level::None,
            fallback: None,
            memory: sandbox::MEMORY_LIMIT,
            cut: None,
            journal: Arc::new(default_journal),
            namespaces: Namespaces::Optional,
        }
    }

    /// Клиент хоста модулей просмотра `postman-viewer` (S8-24): та же песочница, память — [`wasm::JOB_MEMORY`];
    /// процесс — по первому запросу, после выхода (сторож, падение) — новый на следующем.
    pub fn viewer(exe: PathBuf) -> Self {
        Self { memory: wasm::JOB_MEMORY, ..Self::new(exe, wasm::VIEW_TIMEOUT) }
    }

    /// Режим namespaces (Linux, ADR 0010): `Required` — политика администратора `SandboxNamespaces`. Применяется к
    /// следующему запуску процесса.
    pub fn namespaces(mut self, mode: Namespaces) -> Self {
        self.namespaces = mode;
        self
    }

    /// Куда писать строки stderr процесса песочницы: по умолчанию — в stderr приложения (его журнал) с префиксом
    /// `worker: `. Строки уже ограничены [`STDERR_LINE`] и [`STDERR_RATE`].
    pub fn journal(mut self, sink: impl Fn(&str) + Send + Sync + 'static) -> Self {
        self.journal = Arc::new(sink);
        self
    }

    /// `postman-viewer` рядом с основным исполняемым файлом.
    pub fn default_viewer_exe() -> PathBuf {
        let name = if cfg!(windows) { "postman-viewer.exe" } else { "postman-viewer" };
        std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(name))).unwrap_or_else(|| PathBuf::from(name))
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

    /// «Проверить снова» (T2-57): следующий запрос запустит процесс заново и снова попробует полную изоляцию
    /// (права на папку программы могли выдать без перезапуска Postman).
    pub fn restart(&mut self) {
        self.running = None;
        self.fallback = None;
        self.level = Level::None;
    }

    /// Почему изоляция не полная (только отладочная диагностика `POSTMAN_SANDBOX_DROP` на Windows).
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
            | Response::FreeBusy(..)
            | Response::Ics(..)
            | Response::Preview(_)
            | Response::Smime { .. }
            | Response::Envelope(_)
            | Response::Opened { .. }
            | Response::Part
            | Response::Mime(_)
            | Response::Split(_)
            | Response::Threads(_)
            | Response::Prepared(_)
            | Response::Crl(_)
            | Response::Ocsp(_)
            | Response::Mapi
            | Response::View(_) => Err("неожиданный ответ".into()),
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

    /// Отзыв по OCSP (LIN-05d): проверить ответ `response` ответчика (получен ядром) о сертификате `cert`, выпущенном
    /// `issuer` (оба — из доверенной цепочки), на запрос с `nonce`. Подпись ответчика, `CertID`, nonce и срок — в
    /// песочнице; `Ok(Err(_))` — ответ не годится (ядро переходит к CRL).
    pub fn smime_ocsp(
        &mut self,
        cert: &[u8],
        issuer: &[u8],
        nonce: &[u8],
        response: &[u8],
        now: u64,
    ) -> Result<Result<postman_smime::crl::Checked, postman_smime::ocsp::OcspError>, SmimeFailure> {
        if response.len() > postman_smime::ocsp::MAX_RESPONSE {
            return Err(SmimeFailure::TooLarge);
        }
        let bin = postman_smime::wire::encode_ocsp(cert, issuer, nonce, response);
        if bin.len() > MAX_FRAME {
            return Err(SmimeFailure::TooLarge);
        }
        let timeout = self.timeout.max(SMIME_TIMEOUT);
        match self.call_for(&Request::SmimeOcsp { now }, &bin, timeout).map_err(SmimeFailure::Worker)? {
            (Response::Ocsp(r), _) => Ok(r),
            (Response::Err(e), _) => Err(SmimeFailure::Worker(e)),
            _ => Err(SmimeFailure::Worker("unexpected response".into())),
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
    /// раскодированные байты подряд, по порядку частей, и `Message-ID` письма. Ответ сверен с письмом: диапазоны внутри
    /// него, по порядку; `Message-ID` — годный ([`mime::is_message_id`]).
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

    /// Заголовки цепочки писем (S8-36, список IMAP) — в песочнице: `heads` — строки `In-Reply-To` и `References`
    /// каждого письма как пришли (длиннее [`mime::MAX_THREAD_HEAD`] — без цепочки). Ответ — по письму на строки, по
    /// порядку, идентификаторы сверены ([`mime::is_message_id`]).
    pub fn threads(&mut self, heads: &[Vec<u8>]) -> Result<Vec<mime::Thread>, String> {
        if heads.len() > mime::MAX_THREADS {
            return Err("слишком много писем".into());
        }
        let heads: Vec<Vec<u8>> = heads.iter().map(|h| if h.len() > mime::MAX_THREAD_HEAD { Vec::new() } else { h.clone() }).collect();
        match self.call(&Request::Threads, &mime::encode_heads(&heads))? {
            (Response::Threads(t), _) if t.len() == heads.len() && t.iter().all(mime::Thread::fits) => Ok(t),
            (Response::Err(e), _) => Err(e),
            _ => Err("неожиданный ответ".into()),
        }
    }

    /// Показать файл `input` модулем просмотра `module` (клиент [`Client::viewer`]): ответ модуля, проверенный хостом,
    /// или почему не показан; `Err` — хост не ответил.
    pub fn view_module(&mut self, module: &[u8], input: &[u8]) -> Result<Result<wasm::ViewerOutput, wasm::Refusal>, String> {
        if module.len() > wasm::MAX_MODULE || input.len() > wasm::MAX_INPUT {
            return Ok(Err(wasm::Refusal::TooLarge));
        }
        let wasm = u32::try_from(module.len()).map_err(|e| e.to_string())?;
        let bin = [module, input].concat();
        match self.call(&Request::ViewModule { wasm }, &bin)? {
            (Response::View(Ok(wasm::ViewerOutput::Image { width, height, .. })), rgba) => {
                Ok(wasm::checked(wasm::ViewerOutput::Image { width, height, rgba }))
            }
            (Response::View(r), _) => Ok(r),
            (Response::Err(e), _) => Err(e),
            _ => Err("неожиданный ответ".into()),
        }
    }

    /// Письмо целиком → свойства MAPI для MSG и PST (S8-29) — в песочнице; `seen` — прочитано.
    pub fn mapi(&mut self, raw: &[u8], seen: bool) -> Result<postman_export::mapi::MapiMessage, String> {
        if raw.len() > mime::MAX_SPLIT {
            return Err("письмо слишком большое".into());
        }
        let timeout = self.timeout.max(SPLIT_TIMEOUT);
        match self.call_for(&Request::Mapi { seen }, raw, timeout)? {
            (Response::Mapi, bytes) => mapi::decode(&bytes).ok_or_else(|| "неожиданный ответ".into()),
            (Response::Err(e), _) => Err(e),
            _ => Err("неожиданный ответ".into()),
        }
    }

    /// Копия документа без макросов (T2-29) в песочнице.
    pub fn strip_macros(&mut self, doc: &[u8]) -> Result<Vec<u8>, String> {
        match self.call(&Request::StripMacros, doc)? {
            (Response::Part, bytes) => Ok(bytes),
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

    /// Занятость из iCalendar по ссылке (S8-42) в песочнице.
    pub fn free_busy(&mut self, ics: &[u8], from: i64, to: i64, tz: &str) -> Result<Vec<BusySpan>, String> {
        match self.call_for(&Request::FreeBusy { from, to, tz: tz.to_owned() }, ics, self.timeout.max(ICS_TIMEOUT))?.0 {
            Response::FreeBusy(v, cut) => {
                self.cut = (!cut.is_empty()).then_some(cut);
                Ok(v)
            }
            Response::Err(e) => Err(e),
            _ => Err("неожиданный ответ".into()),
        }
    }

    /// Что отсекли пределы в последнем календаре: журнал ведёт приложение, у воркера его нет. Только числа.
    pub fn take_cut(&mut self) -> Option<Cut> {
        self.cut.take()
    }

    /// Календарь подписки (S8-44) в песочнице.
    pub fn ics(&mut self, ics: &[u8], from: i64, to: i64, tz: &str) -> Result<IcsCalendar, String> {
        match self.call_for(&Request::Ics { from, to, tz: tz.to_owned() }, ics, self.timeout.max(ICS_TIMEOUT))?.0 {
            Response::Ics(v, cut) => {
                self.cut = (!cut.is_empty()).then_some(cut);
                Ok(v)
            }
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
        // Повтора с ослабленной изоляцией нет (SBX-02): падение или зависание на чужих данных не понижает песочницу.
        let result = self.exchange(&json, bin, max, timeout);
        if result.is_err() {
            // Таймаут, падение или ошибка песочницы: процесс больше не используем.
            self.running = None;
        }
        let (json, bin) = result?;
        let resp: Response = serde_json::from_slice(&json).map_err(|e| e.to_string())?;
        // Уровень — по первому ответу (SBX-03, аудит №16): процесс отказал в запирании («песочница: …», выход 3) — изоляции
        // нет, а не «полная».
        if let Response::Err(e) = &resp
            && e.starts_with(SANDBOX_REFUSAL)
        {
            self.running = None;
            self.level = Level::None;
            self.fallback = Some(e.clone());
        }
        Ok((resp, bin))
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
            Ok(Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                let code = (r.exit_code)();
                // Отказ в памяти — одно имя исхода на всех ОС (Windows — fail-fast, Linux — SIGABRT, macOS — код 6).
                if r.oom.load(Ordering::SeqCst) || code == Some(EXIT_MEMORY as u32) {
                    return Err(format!("воркер завершился: превышен предел памяти{}", code.map(|c| format!(" (код {c})")).unwrap_or_default()));
                }
                Err(match code {
                    Some(code) => format!("воркер завершился, код {}", exit_name(code)),
                    None => "воркер завершился".into(),
                })
            }
            Err(mpsc::RecvTimeoutError::Timeout) => Err(format!("воркер не ответил за {timeout:?}")),
        }
    }

    fn start(&mut self) -> Result<Running, String> {
        #[cfg(target_os = "linux")]
        let p = sandbox::spawn(&self.exe, self.memory, self.namespaces);
        #[cfg(not(target_os = "linux"))]
        let p = sandbox::spawn(&self.exe, self.memory);
        let p = p.map_err(|e| format!("запуск {}: {e}", self.exe.display()))?;
        self.level = p.level;
        match (p.level, p.fallback) {
            (Level::Full, _) => self.fallback = None,
            (_, Some(reason)) => self.fallback = Some(reason),
            _ => {}
        }
        let oom = Arc::new(AtomicBool::new(false));
        drain_stderr(p.stderr, Arc::clone(&self.journal), Arc::clone(&oom)).map_err(|e| e.to_string())?;
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
        Ok(Running { stdin: p.stdin, responses: rx, max_bin, kill: p.kill, exit_code: p.exit_code, answered: false, oom })
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

/// Код завершения с расшифровкой своих кодов, частых NTSTATUS и сигналов (Unix: 128 + номер).
fn exit_name(code: u32) -> String {
    let name = match code {
        // `EXIT_TIME`, `EXIT_MEMORY`.
        5 => " (превышено время разбора)",
        6 => " (превышен предел памяти)",
        134 if cfg!(unix) => " (аварийное завершение, SIGABRT)",
        137 if cfg!(unix) => " (убит, SIGKILL)",
        138 | 139 if cfg!(unix) => " (нарушение доступа к памяти)",
        153 if cfg!(target_os = "linux") => " (запись в файл запрещена, SIGXFSZ)",
        159 if cfg!(target_os = "linux") => " (запрещённый системный вызов, seccomp)",
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
        // Личная группа (T2-47): `KIND:group` и `MEMBER` — JSContact `kind` и `members`.
        let group = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:g1\r\nFN:Руководители\r\nKIND:group\r\nMEMBER:mailto:a@example.test\r\nMEMBER:urn:uuid:f81d4fae\r\nEND:VCARD\r\n";
        let g = &super::parse_contacts(group.as_bytes()).unwrap()[0];
        assert_eq!(g["kind"], "group", "{g}");
        assert_eq!(g["members"]["mailto:a@example.test"], true, "{g}");
        assert_eq!(g["members"]["urn:uuid:f81d4fae"], true, "{g}");
        assert!(super::parse_contacts(b"BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n").is_err());
        assert!(super::parse_contacts(b"").is_err());
    }

    fn bmp(pixels: &[u8], width: u32, color: image::ExtendedColorType) -> Vec<u8> {
        let mut out = Vec::new();
        let height = pixels.len() as u32 / width / u32::from(color.bits_per_pixel() / 8);
        image::codecs::bmp::BmpEncoder::new(&mut out).encode(pixels, width, height, color).unwrap();
        out
    }

    #[test]
    fn bmp_24_and_32_bits() {
        let d = super::decode(&bmp(&[255, 0, 0, 0, 0, 255], 2, image::ExtendedColorType::Rgb8), 512, 0).unwrap();
        assert_eq!((d.source, d.format, d.page, d.pages), ((2, 1), "BMP", 0, 1));
        assert_eq!(d.img.pixels, [255, 0, 0, 255, 0, 0, 255, 255]);
        let d = super::decode(&bmp(&[0, 255, 0, 128, 1, 2, 3, 255], 2, image::ExtendedColorType::Rgba8), 512, 0).unwrap();
        assert_eq!(d.img.pixels, [0, 255, 0, 128, 1, 2, 3, 255]);
    }

    /// Из трёх кадров ICO — наибольший.
    #[test]
    fn ico_largest_frame() {
        use image::codecs::ico::{IcoEncoder, IcoFrame};
        let frame = |side: u32, gray: u8| {
            let px: Vec<u8> = (0..side * side).flat_map(|_| [gray, gray, gray, 255]).collect();
            IcoFrame::as_png(&px, side, side, image::ExtendedColorType::Rgba8).unwrap()
        };
        let mut ico = Vec::new();
        IcoEncoder::new(&mut ico).encode_images(&[frame(16, 10), frame(48, 20), frame(32, 30)]).unwrap();
        let d = super::decode(&ico, 512, 0).unwrap();
        assert_eq!((d.source, d.format), ((48, 48), "ICO"));
        assert_eq!(&d.img.pixels[..4], &[20, 20, 20, 255]);
        // Уменьшается до `max_side`, как остальные.
        let d = super::decode(&ico, 16, 0).unwrap();
        assert_eq!((d.img.width, d.img.height), (16, 16));
    }

    /// TIFF из страниц (ширина, высота, серый): 8 бит без сжатия (или `compression`), полоса — перед IFD страницы.
    fn tiff(pages: &[(u32, u32, u8)], compression: u16) -> Vec<u8> {
        let mut out = b"II*\0\0\0\0\0".to_vec();
        let mut link = 4usize;
        for &(w, h, gray) in pages {
            let data = out.len() as u32;
            out.extend(std::iter::repeat_n(gray, (w * h) as usize));
            if out.len() % 2 == 1 {
                out.push(0);
            }
            let ifd = out.len() as u32;
            out[link..link + 4].copy_from_slice(&ifd.to_le_bytes());
            let tags: [(u16, u16, u32); 9] = [
                (256, 4, w),
                (257, 4, h),
                (258, 3, 8),
                (259, 3, u32::from(compression)),
                (262, 3, 1),
                (273, 4, data),
                (277, 3, 1),
                (278, 4, h),
                (279, 4, w * h),
            ];
            out.extend((tags.len() as u16).to_le_bytes());
            for (tag, ty, v) in tags {
                out.extend(tag.to_le_bytes());
                out.extend(ty.to_le_bytes());
                out.extend(1u32.to_le_bytes());
                if ty == 3 {
                    out.extend((v as u16).to_le_bytes());
                    out.extend([0, 0]);
                } else {
                    out.extend(v.to_le_bytes());
                }
            }
            link = out.len();
            out.extend(0u32.to_le_bytes());
        }
        out
    }

    #[test]
    fn tiff_pages_like_pdf() {
        let mut t = tiff(&[(4, 3, 10), (5, 2, 200), (6, 1, 99)], 1);
        let d = super::decode(&t, 512, 0).unwrap();
        assert_eq!((d.source, d.format, d.page, d.pages), ((4, 3), "TIFF", 0, 3));
        assert_eq!(&d.img.pixels[..4], &[10, 10, 10, 255]);
        let d = super::decode(&t, 512, 1).unwrap();
        assert_eq!((d.source, d.page, d.pages), ((5, 2), 1, 3));
        assert_eq!(&d.img.pixels[..4], &[200, 200, 200, 255]);
        // Страница за концом — последняя.
        let d = super::decode(&t, 512, 99).unwrap();
        assert_eq!((d.source, d.page, d.pages), ((6, 1), 2, 3));
        // Цикл в цепочке страниц — её конец.
        let first = t[4..8].to_vec();
        let len = t.len();
        t[len - 4..].copy_from_slice(&first);
        assert_eq!(super::decode(&t, 512, 99).map(|d| (d.page, d.pages)), Ok((2, 3)));
        // Факс CCITT G3 декодер `tiff` не умеет.
        let g3 = tiff(&[(4, 3, 10)], 3);
        assert_eq!(super::decode(&g3, 512, 0).err().as_deref(), Some("формат картинки не поддерживается"));
    }

    #[test]
    fn huge_and_broken_images_are_errors() {
        // BMP 30 000 × 30 000 без данных: отказ по размеру, до выделения памяти.
        let mut b = bmp(&[0, 0, 0], 1, image::ExtendedColorType::Rgb8);
        b[18..22].copy_from_slice(&30_000u32.to_le_bytes());
        b[22..26].copy_from_slice(&30_000u32.to_le_bytes());
        assert_eq!(super::decode(&b, 512, 0).err().as_deref(), Some("Memory limit exceeded"));
        // TIFF 30 000 × 30 000: у страницы 1 × 1 IFD — с 10-го байта; ширина, высота, строк в полосе и её длина —
        // значения записей 0, 1, 7 и 8.
        let mut t = tiff(&[(1, 1, 0)], 1);
        for (entry, v) in [(0, 30_000u32), (1, 30_000), (7, 30_000), (8, 900_000_000)] {
            let at = 10 + 2 + 12 * entry + 8;
            t[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }
        assert_eq!(super::decode(&t, 512, 0).err().as_deref(), Some("Memory limit exceeded"));
        for broken in [
            &b"BM"[..],
            b"BM\0\0garbage",
            b"II*\0",
            b"II*\0\xff\xff\xff\x7f",
            b"MM\0*\0\0\0\x08\0\x01",
            b"II+\0\x08\0\0\0",
            &[0, 0, 1, 0, 3, 0, 1, 2],
        ] {
            assert!(super::decode(broken, 512, 0).is_err(), "{broken:?}");
        }
        let t = tiff(&[(4, 3, 10)], 1);
        assert!(super::decode(&t[..t.len() - 20], 512, 0).is_err());
    }
}
