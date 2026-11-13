//! Воркер в настоящей песочнице: рендер работает, побег — нет.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)

use std::path::PathBuf;
use std::time::Duration;

use postman_worker::{Client, Level, Probe, Request};

fn client() -> Client {
    Client::new(PathBuf::from(env!("CARGO_BIN_EXE_postman-worker")), Duration::from_secs(3))
}

/// Побег не удался: либо ОС вернула ошибку, либо seccomp убил процесс.
fn denied(r: Result<postman_ir::Document, String>) {
    let e = r.expect_err("должно быть отказано");
    assert!(!e.contains("ДОСТУП РАЗРЕШЁН"), "{e}");
}

#[test]
fn renders_and_survives_escape_attempts() {
    let mut c = client();
    let doc = c.render(&Request::Html("<p><b>Привет</b><script>x</script></p>".into())).unwrap();
    assert_eq!(postman_ir::to_markdown(&doc), "**Привет**");
    // Файл пользователя: AppContainer и Landlock не должны давать его прочитать.
    // Каталог удаляется при выходе из теста, даже при падении (H-01).
    let secret_dir = tempfile::Builder::new().prefix("postman-secret-").tempdir().unwrap();
    let secret = secret_dir.path().join("secret.txt");
    std::fs::write(&secret, "секрет").unwrap();
    let secret_path = secret.to_string_lossy().into_owned();

    // На Windows полная изоляция (AppContainer) требует права на чтение exe для
    // ALL APPLICATION PACKAGES — так в Program Files и в CI (см. .github/workflows/ci.yml).
    // В остальных случаях воркер работает без AppContainer: файлы и сеть ему доступны,
    // проверяем только то, что даёт Job Object и mitigation policies.
    let full = c.level() == Level::Full;
    if cfg!(windows) && std::env::var_os("POSTMAN_REQUIRE_FULL_SANDBOX").is_none() {
        assert!(matches!(c.level(), Level::Full | Level::Partial));
    } else {
        assert_eq!(c.level(), Level::Full, "{:?}", c.fallback());
    }
    let shell = if cfg!(windows) { "cmd.exe" } else { "/bin/sh" };
    let mut probes = vec![Probe::Spawn(shell.into())];
    if full {
        probes.push(Probe::ReadFile(secret_path));
        probes.push(Probe::Connect("1.1.1.1:443".into()));
    } else {
        eprintln!("песочница частичная: проверки чтения файлов и сети пропущены");
    }
    for p in probes {
        denied(c.render(&Request::Probe(p)));
        // После убитого процесса клиент поднимает новый.
        let doc = c.render(&Request::Text("ещё жив".into())).unwrap();
        assert_eq!(postman_ir::to_markdown(&doc), "ещё жив");
    }
}

/// Граница истории переписки (S5-05) ставится в песочнице и приходит в ответе вместе с полным IR.
#[test]
fn history_is_marked_in_the_sandbox() {
    let mut c = client();
    let html = r#"<div>Да, подходит.</div><div class="gmail_quote"><div class="gmail_attr">20.10.2026 10:15, Иван &lt;i@example.ru&gt;:</div>
        <blockquote class="gmail_quote"><div>Встречаемся?</div></blockquote></div>"#;
    let doc = c.render(&Request::Html(html.into())).unwrap();
    assert_eq!(doc.history, Some(1));
    assert_eq!(postman_ir::to_markdown(&postman_ir::Document { blocks: doc.new_text().to_vec(), ..Default::default() }), "Да, подходит.");
    assert!(postman_ir::to_markdown(&doc).contains("Встречаемся?"));
}

#[test]
fn hang_is_killed_by_timeout() {
    let mut c = client();
    let e = c.render(&Request::Probe(Probe::Hang)).unwrap_err();
    assert!(e.contains("не ответил"), "{e}");
    assert!(c.render(&Request::Text("ok".into())).is_ok());
}

#[test]
fn hostile_input() {
    let mut c = client();
    let deep = "<div><span>".repeat(100_000) + "x";
    let r = c.render(&Request::Html(deep));
    assert!(r.is_ok(), "{:?}", r.err());
    let wide = "<p>a</p>".repeat(30_000);
    assert!(c.render(&Request::Html(wide)).is_ok());
}

#[test]
fn decodes_images_and_rejects_others() {
    let mut c = client();
    // PNG 1x1, красный пиксель.
    let png = [
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
        0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63, 0xF8, 0xCF,
        0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D, 0xB0, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    let img = c.decode_image(&png, 512).unwrap();
    assert_eq!((img.width, img.height), (1, 1));
    assert_eq!(&img.pixels[..3], &[255, 0, 0]);
    assert!(c.decode_image(b"<svg onload=alert(1)>", 512).is_err());
    assert!(c.decode_image(&png[..20], 512).is_err());
    // BMP 1×1, 24 бита (S8-21).
    let bmp = [
        0x42, 0x4D, 0x3A, 0, 0, 0, 0, 0, 0, 0, 0x36, 0, 0, 0, 0x28, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0x18, 0, 0, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xFF, 0,
    ];
    let img = c.decode_image(&bmp, 512).unwrap();
    assert_eq!((img.width, img.height, img.pixels.len()), (1, 1, 4));
    // Формат, который `image` узнаёт, но воркер не берёт (QOI 1×1): отказ до разбора.
    assert_eq!(c.decode_image(b"qoif\0\0\0\x01\0\0\0\x01\x04\0", 512).err().as_deref(), Some("формат картинки не поддерживается"));
    // Воркер жив после ошибок разбора.
    assert!(c.render(&Request::Text("ok".into())).is_ok());
}

#[test]
fn all_supported_formats_decode_under_seccomp() {
    let mut c = client();
    for ext in ["jpg", "gif", "webp", "png"] {
        let bytes = std::fs::read(format!("{}/tests/fixtures/img.{ext}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let img = c.decode_image(&bytes, 100).unwrap_or_else(|e| panic!("{ext}: {e}"));
        assert_eq!((img.width, img.height), (100, 67), "{ext}");
    }
}

/// Занятость по ссылке (S8-42) в песочнице: VFREEBUSY — виды `FBTYPE`, период длительностью, свободное и вне диапазона не
/// берутся; опубликованный календарь — события с повторами, поясом; прозрачное и отменённое занятости не дают.
#[test]
fn free_busy_round_trip() {
    use postman_worker::{BusySpan, BusyType};
    let mut c = client();
    // 2026-11-02 00:00 UTC — понедельник.
    let (from, to) = (1_793_577_600, 1_793_577_600 + 7 * 86_400);
    let vfb = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//T//T//EN\r\nBEGIN:VFREEBUSY\r\n\
        FREEBUSY;FBTYPE=BUSY:20261103T090000Z/20261103T100000Z,20261103T120000Z/PT30M\r\n\
        FREEBUSY;FBTYPE=BUSY-TENTATIVE:20261104T090000Z/20261104T100000Z\r\n\
        FREEBUSY;FBTYPE=BUSY-UNAVAILABLE:20261105T090000Z/20261105T100000Z\r\n\
        FREEBUSY;FBTYPE=FREE:20261106T090000Z/20261106T100000Z\r\n\
        FREEBUSY:20261201T090000Z/20261201T100000Z\r\n\
        END:VFREEBUSY\r\nEND:VCALENDAR\r\n";
    let got = c.free_busy(vfb.as_bytes(), from, to, "UTC").unwrap();
    let hour = |day: i64, h: i64| from + day * 86_400 + h * 3600;
    assert_eq!(
        got,
        vec![
            BusySpan { start: hour(1, 9), end: hour(1, 10), kind: BusyType::Busy },
            BusySpan { start: hour(1, 12), end: hour(1, 12) + 1800, kind: BusyType::Busy },
            BusySpan { start: hour(2, 9), end: hour(2, 10), kind: BusyType::Tentative },
            BusySpan { start: hour(3, 9), end: hour(3, 10), kind: BusyType::Unavailable },
        ]
    );
    let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//T//T//EN\r\n\
        BEGIN:VEVENT\r\nUID:a\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:20261005T090000Z\r\nDTEND:20261005T100000Z\r\nRRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\n\
        BEGIN:VEVENT\r\nUID:b\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:20261103T090000Z\r\nDTEND:20261103T100000Z\r\nTRANSP:TRANSPARENT\r\nEND:VEVENT\r\n\
        BEGIN:VEVENT\r\nUID:c\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:20261104T090000Z\r\nDTEND:20261104T100000Z\r\nSTATUS:CANCELLED\r\nEND:VEVENT\r\n\
        BEGIN:VEVENT\r\nUID:d\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:20261105T090000Z\r\nDTEND:20261105T100000Z\r\nSTATUS:TENTATIVE\r\nEND:VEVENT\r\n\
        END:VCALENDAR\r\n";
    let got = c.free_busy(ics.as_bytes(), from, to, "UTC").unwrap();
    assert_eq!(
        got,
        vec![
            BusySpan { start: hour(0, 9), end: hour(0, 10), kind: BusyType::Busy },
            BusySpan { start: hour(3, 9), end: hour(3, 10), kind: BusyType::Tentative },
        ]
    );
    assert!(c.free_busy(b"<html>not a calendar</html>", from, to, "UTC").is_err());
    assert!(c.free_busy(&vec![b'x'; postman_worker::MAX_FREE_BUSY + 1], from, to, "UTC").is_err());
}

/// Подписка (S8-44): имя календаря, повтор с поясом, событие на весь день без `DTEND`, отменённое не берётся.
#[test]
fn ics_round_trip() {
    let mut c = client();
    // 2026-11-02 00:00 UTC — понедельник.
    let (from, to) = (1_793_577_600, 1_793_577_600 + 7 * 86_400);
    let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//T//T//EN\r\nX-WR-CALNAME:Праздники\r\n\
        BEGIN:VEVENT\r\nUID:a\r\nDTSTAMP:20261001T000000Z\r\nDTSTART;TZID=Europe/Moscow:20261006T100000\r\nDTEND;TZID=Europe/Moscow:20261006T110000\r\n\
        RRULE:FREQ=WEEKLY\r\nSUMMARY:Планёрка\r\nLOCATION:Зал\r\nEND:VEVENT\r\n\
        BEGIN:VEVENT\r\nUID:b\r\nDTSTAMP:20261001T000000Z\r\nDTSTART;VALUE=DATE:20261104\r\nSUMMARY:День единства\r\nEND:VEVENT\r\n\
        BEGIN:VEVENT\r\nUID:c\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:20261105T090000Z\r\nDTEND:20261105T100000Z\r\nSTATUS:CANCELLED\r\nEND:VEVENT\r\n\
        END:VCALENDAR\r\n";
    let got = c.ics(ics.as_bytes(), from, to, "Europe/Moscow").unwrap();
    assert_eq!(got.name.as_deref(), Some("Праздники"));
    let list: Vec<(&str, i64, i64, bool, bool)> = got.events.iter().map(|e| (e.title.as_str(), e.start, e.end, e.all_day, e.recurring)).collect();
    let at = |day: i64, h: i64| from + day * 86_400 + h * 3600;
    assert_eq!(list, [("Планёрка", at(1, 7), at(1, 8), false, true), ("День единства", at(1, 21), at(2, 21), true, false)]);
    assert_eq!(got.events[0].location, "Зал");
    assert!(c.ics(b"not a calendar", from, to, "UTC").is_err());
}

/// Календарь предельного размера с дорогими правилами в песочнице: укладывается в память и сторожа, воркер жив.
/// Выбраны самые дорогие виды из `hostile_rules_are_bounded` (`crates/worker/src/ics.rs`).
#[test]
fn ics_at_the_size_limit() {
    let mut c = client();
    let (from, to) = (1_793_577_600, 1_793_577_600 + 7 * 86_400);
    for rule in ["FREQ=DAILY;COUNT=4000000000", "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR,SA,SU;BYSETPOS=1,2,3,4,5,6,7,8,9,10", "FREQ=SECONDLY"] {
        let mut ics = String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//T//T//EN\r\n");
        let mut i = 0;
        while ics.len() < postman_worker::MAX_ICS - 2048 {
            ics += &format!("BEGIN:VEVENT\r\nUID:h{i}\r\nDTSTART:19710101T000000Z\r\nDURATION:PT1H\r\nRRULE:{rule}\r\nEND:VEVENT\r\n");
            i += 1;
        }
        ics += "END:VCALENDAR\r\n";
        assert!(!c.ics(ics.as_bytes(), from, to, "Europe/Moscow").unwrap().events.is_empty(), "{rule}");
    }
    assert!(c.ics(b"not a calendar", from, to, "UTC").is_err());
}

/// Пределы доходят до клиента только в ответе: журнала в песочнице нет. Обычный файл ничего не оставляет, забранное не повторяется.
#[test]
fn cut_reaches_the_client() {
    let mut c = client();
    let (from, to) = (1_793_577_600, 1_793_577_600 + 7 * 86_400);
    let event = |uid: &str, rule: &str| {
        format!("BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTART:20261101T000000Z\r\nDURATION:PT1S\r\nRRULE:FREQ={rule}\r\nEND:VEVENT\r\n")
    };
    let calendar = |body: String| format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//T//T//EN\r\n{body}END:VCALENDAR\r\n").into_bytes();
    let hostile = calendar((0..8).map(|i| event(&format!("h{i}"), "SECONDLY")).collect());
    let ordinary = calendar(event("a", "WEEKLY"));
    c.ics(&ordinary, from, to, "UTC").unwrap();
    assert_eq!(c.take_cut(), None);
    c.ics(&hostile, from, to, "UTC").unwrap();
    let cut = c.take_cut().unwrap();
    assert_eq!(cut.series, 8);
    assert!(cut.series_skipped > 0, "{cut:?}");
    assert_eq!(c.take_cut(), None);
    c.free_busy(&hostile, from, to, "UTC").unwrap();
    assert!(c.take_cut().is_some());
}

#[test]
fn parses_invitation_under_seccomp() {
    let mut c = client();
    let ics = std::fs::read(format!("{}/tests/fixtures/invite-outlook.ics", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let v = c.parse_calendar(&ics).unwrap();
    println!("{}", serde_json::to_string_pretty(&v).unwrap());
    assert_eq!(v["method"], "REQUEST");
    assert!(c.parse_calendar(b"not a calendar").is_err());
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// Встроенный просмотр (S5-07): запрос и ответ через песочницу — картинка, страница PDF, оглавление, макросы,
/// текст; после ошибок воркер жив.
#[test]
fn preview_round_trip() {
    use postman_worker::{OfficeFormat, Preview, PreviewKind};
    let mut c = client();
    let (p, px) = c.preview(PreviewKind::Image, &fixture("img.png"), 0, 50, 0).unwrap();
    let Preview::Image { width, height, source, format, page, pages } = p else { panic!("{p:?}") };
    assert_eq!((source, format.as_str(), page, pages), ((300, 200), "PNG", 0, 1));
    assert!(width <= 50 && height <= 50);
    assert_eq!(px.unwrap().pixels.len(), (width * height * 4) as usize);

    let pdf = fixture("report.pdf");
    let (p, px) = c.preview(PreviewKind::Pdf, &pdf, 1, 800, 0).unwrap();
    let Preview::Pdf { width, height, page, pages, title } = p else { panic!("{p:?}") };
    assert_eq!((page, pages, title.as_deref()), (1, 3, Some("Quarterly report Q3")));
    assert_eq!(height, 800);
    assert!(width > 500 && width < 600, "{width}");
    let px = px.unwrap();
    // Белый фон и тёмный текст: страница отрисована, а не пустая.
    assert!(px.pixels.as_chunks::<4>().0.iter().any(|p| p[0] < 100));
    assert!(px.pixels.as_chunks::<4>().0.iter().filter(|p| p[0] > 250).count() > px.pixels.len() / 8);
    // Страница за концом — последняя.
    let (p, _) = c.preview(PreviewKind::Pdf, &pdf, 99, 400, 0).unwrap();
    assert!(matches!(p, Preview::Pdf { page: 2, pages: 3, .. }), "{p:?}");

    let (p, _) = c.preview(PreviewKind::Archive, &fixture("screens.zip"), 0, 0, 0).unwrap();
    let Preview::Archive(l) = p else { panic!("{p:?}") };
    assert_eq!(l.files, 8);
    assert!(l.entries.iter().any(|e| e.name == "tools/collect.bat" && e.modified.as_deref() == Some("2026-10-14 11:20")));

    let (p, _) = c.preview(PreviewKind::Office, &fixture("budget.xlsm"), 0, 0, 0).unwrap();
    let Preview::Office(s) = p else { panic!("{p:?}") };
    assert_eq!((s.format, s.macros), (OfficeFormat::Ooxml, true));
    assert_eq!(s.modules, ["Module1", "Module2", "ThisWorkbook"]);

    let (p, _) = c.preview(PreviewKind::Text, "Дата;Сумма\r\n21.10;100".as_bytes(), 0, 0, 0).unwrap();
    assert_eq!(p, Preview::Text { text: "Дата;Сумма\n21.10;100".into(), truncated: false });
    // Таблица разделами (S8-17).
    let (p, _) = c.preview(PreviewKind::Table, "Дата;Сумма\r\n21.10;100".as_bytes(), 0, 0, 0).unwrap();
    let Preview::Document { sections, truncated: false } = p else { panic!("{p:?}") };
    assert!(matches!(sections.as_slice(), [postman_worker::Section { title: postman_worker::SectionTitle::None, blocks }] if blocks.len() == 1));

    assert!(c.preview(PreviewKind::Pdf, b"%PDF-1.7 broken", 0, 800, 0).is_err());
    assert!(c.preview(PreviewKind::Archive, b"PK\x03\x04", 0, 0, 0).is_err());
    assert!(c.preview(PreviewKind::Image, &vec![0; 60 * 1024 * 1024], 0, 0, 0).is_err(), "больше предела — отказ до отправки");
    assert!(c.render(&Request::Text("ok".into())).is_ok());
}

/// Сторож времени в самом воркере: запрос дольше предела завершает процесс с понятным кодом, клиент
/// поднимает новый.
#[test]
fn watchdog_stops_a_long_request() {
    let mut c = Client::new(PathBuf::from(env!("CARGO_BIN_EXE_postman-worker")), Duration::from_secs(20));
    // Сначала обычный запрос: воркер отвечал, клиент не повторяет зависший запрос в ослабленной песочнице.
    assert!(c.render(&Request::Text("ok".into())).is_ok());
    let started = std::time::Instant::now();
    let e = c.render(&Request::Probe(Probe::Hang)).unwrap_err();
    assert!(e.contains("превышено время"), "{e}");
    assert!(started.elapsed() < Duration::from_secs(15));
    assert!(c.render(&Request::Text("ok".into())).is_ok());
}

/// Письмо для хранилища (S8-07) через песочницу: часть больше общего кадра в 64 МиБ — и в запросе, и в ответе.
/// quoted-printable печатного текста раскодируется почти один к одному: разбор в отладочной сборке успевает до сторожа.
#[test]
fn mime_split_round_trip() {
    let mut c = client();
    let line = "ABC".repeat(25);
    let lines = 64 * 1024 * 1024 / line.len() + 1000;
    let body = vec![line; lines].join("=\r\n");
    let raw = format!(
        "From: a@example.test\r\nTo: b@example.test\r\nMessage-ID: <big@example.test>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b\"\r\n\r\n\
         --b\r\nContent-Type: text/plain\r\n\r\nТело\r\n--b\r\nContent-Type: application/octet-stream\r\n\
         Content-Transfer-Encoding: quoted-printable\r\n\r\n{body}\r\n--b--\r\n"
    );
    let (split, bin) = c.mime_split(raw.as_bytes()).unwrap();
    assert_eq!((split.parts.len(), split.message_id.as_deref()), (1, Some("<big@example.test>")));
    let p = &split.parts[0];
    assert_eq!(&raw.as_bytes()[p.range.start as usize..p.range.end as usize], body.as_bytes());
    assert_eq!(bin.len(), lines * 75);
    assert!(bin.len() > 64 * 1024 * 1024 && bin.chunks(3).all(|c| c == b"ABC"));

    assert!(c.mime_split(b"").is_err());
    assert!(c.mime_split(&vec![b'a'; postman_worker::mime::MAX_SPLIT + 1]).is_err(), "больше предела — отказ до отправки");
    // Общий предел кадра для остальных запросов не вырос.
    assert!(c.mime(raw.as_bytes()).is_err());
    assert!(c.render(&Request::Text("ok".into())).is_ok());
}

/// Заголовки цепочки (S8-36) через песочницу: по письму на строки, по порядку; слишком длинные — без цепочки.
#[test]
fn threads_round_trip() {
    use postman_worker::mime::{MAX_THREAD_HEAD, Thread};
    let mut c = client();
    let heads = vec![
        b"In-Reply-To: <b@x.test>\r\nReferences: <a@x.test>\r\n <b@x.test>\r\n\r\n".to_vec(),
        b"\r\n".to_vec(),
        format!("References: <a@x.test>{}\r\n\r\n", " ".repeat(MAX_THREAD_HEAD)).into_bytes(),
    ];
    let t = c.threads(&heads).unwrap();
    assert_eq!(t.len(), 3);
    assert_eq!(
        (t[0].in_reply_to.as_deref(), t[0].references.as_slice()),
        (Some("<b@x.test>"), &["<a@x.test>".to_owned(), "<b@x.test>".to_owned()][..])
    );
    assert_eq!((&t[1], &t[2]), (&Thread::default(), &Thread::default()));
    assert_eq!(c.threads(&[]), Ok(Vec::new()));
}

/// Свойства MAPI (S8-29) через песочницу: письмо с вложением приходит тем же письмом MAPI.
#[test]
fn mapi_round_trip() {
    use postman_export::mapi::{PR_ATTACH_DATA_BIN, PR_SUBJECT_W, Value};
    let mut c = client();
    let raw = "From: a@example.test\r\nTo: b@example.test\r\nSubject: =?utf-8?B?0J7RgtGH0ZHRgg==?=\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b\"\r\n\r\n\
               --b\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nТело\r\n--b\r\nContent-Type: application/pdf; name=\"a.pdf\"\r\n\
               Content-Transfer-Encoding: base64\r\n\r\nJVBERg==\r\n--b--\r\n";
    let m = c.mapi(raw.as_bytes(), true).unwrap();
    assert_eq!(m, postman_worker::mapi::mapi(raw.as_bytes(), true).unwrap());
    assert!(m.props.contains(&(PR_SUBJECT_W, Value::Unicode("Отчёт".into()))));
    assert!(m.attachments[0].props.contains(&(PR_ATTACH_DATA_BIN, Value::Binary(b"%PDF".to_vec()))));
    assert!(c.mapi(b"", true).is_err());
}

/// Воркер, зависший или упавший на первом запросе, не перезапускается с ослабленной изоляцией (SBX-02): уровень
/// остаётся полным, причины ослабления нет, следующий запрос — в той же песочнице.
#[test]
fn failure_on_first_request_does_not_lower_isolation() {
    let mut c = Client::new(PathBuf::from(env!("CARGO_BIN_EXE_postman-worker")), Duration::from_secs(20));
    let e = c.render(&Request::Probe(Probe::Hang)).unwrap_err();
    assert!(e.contains("превышено время"), "{e}");
    assert!(!e.contains("в полной изоляции"), "{e}");
    assert!(c.render(&Request::Text("ok".into())).is_ok());
    if std::env::var_os("POSTMAN_REQUIRE_FULL_SANDBOX").is_some() || !cfg!(windows) {
        assert_eq!(c.level(), Level::Full, "{:?}", c.fallback());
        assert_eq!(c.fallback(), None);
    }
}

/// Пути профиля и имя пользователя воркеру не передаются ни на одной ОС (SBX-02).
#[test]
fn worker_gets_no_profile_paths() {
    let mut c = Client::new(PathBuf::from(env!("CARGO_BIN_EXE_postman-worker")), Duration::from_secs(5));
    for name in ["USERPROFILE", "APPDATA", "USERNAME", "HOME", "USER"] {
        let e = c.render(&Request::Probe(Probe::Env(name.into()))).unwrap_err();
        assert_eq!(e, format!("запрещено: {name} не задана"));
    }
}

/// stderr процесса песочницы — канал на всех ОС (SBX-01): строка доходит до журнала клиента, поток сверх
/// `STDERR_RATE` строк в секунду не останавливает процесс и даёт одну строку WARN.
#[test]
fn worker_stderr_reaches_the_journal() {
    use std::sync::{Arc, Mutex};
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&lines);
    let mut c = Client::new(PathBuf::from(env!("CARGO_BIN_EXE_postman-worker")), Duration::from_secs(20))
        .journal(move |l| sink.lock().unwrap().push(l.to_owned()));
    let line = "postman-worker: stderr probe\n".len() as u32;
    assert_eq!(c.render(&Request::Probe(Probe::Stderr(line * 3))).unwrap_err(), "stderr: записано");
    let wait = |pred: &dyn Fn(&[String]) -> bool| {
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while !pred(&lines.lock().unwrap()) && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        lines.lock().unwrap().clone()
    };
    let got = wait(&|l| l.len() >= 3);
    assert_eq!(&got[..3], ["postman-worker: stderr probe"; 3], "{got:?}");
    // 1 МиБ строк: канал не переполняется, процесс отвечает, лишнее отброшено одной строкой WARN.
    assert_eq!(c.render(&Request::Probe(Probe::Stderr(1 << 20))).unwrap_err(), "stderr: записано");
    assert!(c.render(&Request::Text("ещё жив".into())).is_ok());
    drop(c);
    let got = wait(&|l| l.iter().any(|s| s.starts_with("WARN stderr rate limit")));
    // Записано ~36 тыс. строк; в журнал — не больше `STDERR_RATE` в секунду (запись идёт секунды на медленном CI).
    assert!(got.len() < 36_000 / 10, "{}", got.len());
    assert!(got.iter().any(|s| s.starts_with("WARN stderr rate limit")), "{:?}", &got[got.len().saturating_sub(3)..]);
}

/// Предел памяти воркера — 1 ГиБ частной выделенной памяти на всех ОС, исход назван одинаково (SBX-03, аудит №17, №45).
#[test]
fn worker_memory_limit_has_one_name_everywhere() {
    let mut c = Client::new(PathBuf::from(env!("CARGO_BIN_EXE_postman-worker")), Duration::from_secs(20));
    assert_eq!(c.render(&Request::Probe(Probe::Alloc(256 << 20))).unwrap_err(), "ДОСТУП РАЗРЕШЁН", "под пределом");
    let e = c.render(&Request::Probe(Probe::Alloc(3 << 29))).unwrap_err();
    assert!(e.contains("превышен предел памяти"), "{e}");
    assert!(c.render(&Request::Text("ещё жив".into())).is_ok());
}
