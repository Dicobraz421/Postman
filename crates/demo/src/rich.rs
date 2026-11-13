//! Письма демо целиком, как в живом ящике (замер хранилища, ADR 0006, дополнение 2026-11-14): у каждого
//! [`EVERY`]-го письма основной учётки — содержимое RFC 5322 и разобранное тело, как после открытия. Только в ящиках
//! для замеров (от [`MIN_LETTERS`] писем): снимки и тесты на малых ящиках идут как прежде.
//!
//! Что в письме: цепочка `Received` и подпись DKIM, текст и HTML (у половины организаций — quoted-printable с
//! кириллицей, у другой — base64), у трёх писем из пяти — логотип организации в подписи (свой у каждой из
//! [`ORGS`], 4–40 КБ), у рассылок — значки соцсетей (одни и те же у всех), у каждого седьмого — вложение
//! (PDF, документ Office, таблица, фото; уже сжатые, у каждого седьмого такого — пересланное повторно). Байты
//! картинок и вложений — шум: как у настоящих PNG, JPEG и ZIP, они не сжимаются.

use std::sync::OnceLock;

use postman_cache::{BodyRow, Cache, Encoding, SplitPart};

use crate::{ME, ME_NAME};

/// Письмо `demo<i>` с содержимым — каждое такое.
pub const EVERY: usize = 10;
/// Письма целиком — только в ящике от стольких писем (бенч — 50 000).
pub const MIN_LETTERS: usize = 10_000;

/// Организация отправителя: имя, домен, размер логотипа.
pub struct Org {
    pub name: &'static str,
    pub domain: &'static str,
    pub logo: usize,
}

/// Организации отправителей (логотип подписи — у каждой свой).
pub const ORGS: &[Org] = &[
    Org { name: "Северсталь Трейд", domain: "seversteel.example", logo: 18_000 },
    Org { name: "Банк Восход", domain: "voskhod-bank.example", logo: 9_500 },
    Org { name: "ИТ-Интеграция", domain: "itint.example", logo: 32_000 },
    Org { name: "Логистик Плюс", domain: "logplus.example", logo: 6_200 },
    Org { name: "Медиагруппа Рассвет", domain: "rassvet.example", logo: 40_000 },
    Org { name: "Учебный центр Профи", domain: "profi-edu.example", logo: 12_400 },
    Org { name: "Канцторг", domain: "kanctorg.example", logo: 4_300 },
    Org { name: "Энергосбыт", domain: "energosbyt.example", logo: 21_000 },
    Org { name: "Юридическое бюро «Право»", domain: "pravo.example", logo: 7_700 },
    Org { name: "Типография «Оттиск»", domain: "ottisk.example", logo: 15_000 },
    Org { name: "Облако.Про", domain: "oblako.example", logo: 27_500 },
    Org { name: "Корпоративный портал", domain: "corp.example", logo: 11_000 },
    Org { name: "Служба качества", domain: "quality.corp.example", logo: 5_100 },
    Org { name: "Ассоциация ИБ", domain: "ib-assoc.example", logo: 36_000 },
    Org { name: "Турагентство «Маршрут»", domain: "marshrut.example", logo: 8_800 },
    Org { name: "Онлайн-магазин «Всё для офиса»", domain: "office-shop.example", logo: 24_000 },
    Org { name: "Кадровое агентство", domain: "hr-agency.example", logo: 6_900 },
    Org { name: "Вендор СЗИ", domain: "szi-vendor.example", logo: 19_500 },
    Org { name: "Конференция «Защита-2026»", domain: "zashita.example", logo: 30_000 },
    Org { name: "Сервисный центр", domain: "service.example", logo: 10_200 },
    Org { name: "Новости отрасли", domain: "news.example", logo: 14_600 },
    Org { name: "Клининг «Чисто»", domain: "chisto.example", logo: 4_900 },
    Org { name: "Страховая «Надёжность»", domain: "nadezhnost.example", logo: 22_000 },
    Org { name: "Партнёр по поставкам", domain: "supply.example", logo: 13_300 },
];

/// Значки соцсетей в подписи рассылок: одни и те же у всех организаций.
const ICONS: [usize; 4] = [1_300, 1_700, 2_100, 2_500];

const PHRASES: &[&str] = &[
    "Коллеги, добрый день!",
    "Направляем вам материалы по итогам встречи.",
    "Просим подтвердить участие до конца недели.",
    "В приложении — обновлённая версия документа с правками юристов.",
    "Сроки поставки переносятся на две недели из-за задержки на таможне.",
    "Счёт будет выставлен после подписания акта выполненных работ.",
    "Обратите внимание на изменения в регламенте доступа к серверной.",
    "Вебинар пройдёт в четверг в 11:00 по московскому времени, ссылка придёт за час до начала.",
    "По вашей заявке назначен инженер, он свяжется с вами в течение дня.",
    "Напоминаем, что отчётность за квартал нужно сдать до 20-го числа.",
    "Мы обновили условия обслуживания: подробности — в личном кабинете.",
    "Если письмо пришло вам по ошибке, сообщите отправителю и удалите его.",
    "Скидка действует до конца месяца для всех корпоративных клиентов.",
    "Прошу согласовать бюджет на следующий год в приложенной таблице.",
    "Протокол совещания и список поручений — ниже.",
    "Спасибо за быстрый ответ, всё получили.",
];

fn rng(seed: u64) -> impl FnMut() -> u64 {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    }
}

/// Шум: байты уже сжатого файла.
fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut next = rng(seed);
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        out.extend_from_slice(&next().to_le_bytes());
    }
    out.truncate(len);
    out
}

/// Картинка PNG `len` байт: подпись формата и шум.
fn png(len: usize, seed: u64) -> Vec<u8> {
    let mut b = noise(len, seed);
    b[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    b
}

/// Организация отправителя письма `i` (номер в [`ORGS`]).
pub fn org_of(i: usize) -> usize {
    ((i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 40) as usize % ORGS.len()
}

/// Логотип организации `n` — одни и те же байты во всех её письмах.
pub fn logo(n: usize) -> &'static [u8] {
    static LOGOS: OnceLock<Vec<Vec<u8>>> = OnceLock::new();
    &LOGOS.get_or_init(|| ORGS.iter().enumerate().map(|(i, o)| png(o.logo, 1000 + i as u64)).collect())[n % ORGS.len()]
}

fn icon(n: usize) -> &'static [u8] {
    static ICONS_BYTES: OnceLock<Vec<Vec<u8>>> = OnceLock::new();
    &ICONS_BYTES.get_or_init(|| ICONS.iter().enumerate().map(|(i, &len)| png(len, 2000 + i as u64)).collect())[n % ICONS.len()]
}

/// Вложение письма `i` (у каждого седьмого): тип, имя, байты. У каждого седьмого такого — то же, что семь писем назад
/// (пересланное).
fn attachment(i: usize) -> Option<(&'static str, String, Vec<u8>)> {
    if !i.is_multiple_of(7) {
        return None;
    }
    let src = if i.is_multiple_of(49) && i >= 7 { i - 7 } else { i };
    let mut next = rng(src as u64 + 77);
    let (mime, ext, len) = match next() % 4 {
        0 => ("application/pdf", "pdf", 60_000 + next() % 300_000),
        1 => ("application/vnd.openxmlformats-officedocument.wordprocessingml.document", "docx", 25_000 + next() % 120_000),
        2 => ("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", "xlsx", 15_000 + next() % 60_000),
        _ => ("image/jpeg", "jpg", 150_000 + next() % 900_000),
    };
    Some((mime, format!("Документ {src}.{ext}"), noise(len as usize, src as u64 + 99)))
}

/// Текст письма `i`: абзацы из фраз.
fn text(i: usize, org: &Org, next: &mut impl FnMut() -> u64) -> String {
    let paragraphs = 2 + next() % 6;
    let mut s = String::new();
    for _ in 0..paragraphs {
        for _ in 0..2 + next() % 4 {
            s.push_str(PHRASES[next() as usize % PHRASES.len()]);
            s.push(' ');
        }
        s.push_str("\r\n\r\n");
    }
    s.push_str(&format!("--\r\nС уважением,\r\nотдел по работе с клиентами\r\n{}\r\n+7 495 000-{:02}-{:02}\r\n", org.name, i % 100, i % 61));
    s
}

/// HTML письма: вёрстка таблицами со встроенными стилями, как у почтовых конструкторов и Outlook.
fn html(body: &str, org: &Org, logo: bool, icons: bool) -> String {
    let mut s = String::from(
        "<!DOCTYPE html><html><head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=utf-8\">\
         <style>body{margin:0;padding:0;background:#f4f4f4;font-family:Segoe UI,Arial,sans-serif}\
         .wrap{width:100%;background:#f4f4f4}.card{max-width:640px;margin:0 auto;background:#ffffff}\
         p{margin:0 0 12px 0;font-size:14px;line-height:20px;color:#222222}\
         .foot{font-size:11px;line-height:16px;color:#888888}</style></head><body>\
         <table class=\"wrap\" role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\"><tr><td align=\"center\">\
         <table class=\"card\" role=\"presentation\" width=\"640\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\">",
    );
    if logo {
        s.push_str("<tr><td style=\"padding:24px 32px 8px 32px\"><img src=\"cid:logo@demo\" width=\"160\" height=\"48\" alt=\"\" style=\"display:block;border:0\"></td></tr>");
    }
    s.push_str("<tr><td style=\"padding:16px 32px 24px 32px\">");
    for para in body.split("\r\n\r\n").filter(|p| !p.trim().is_empty()) {
        s.push_str("<p style=\"margin:0 0 12px 0;font-size:14px;line-height:20px;color:#222222;font-family:Segoe UI,Arial,sans-serif\">");
        s.push_str(&para.replace("\r\n", "<br>"));
        s.push_str("</p>");
    }
    s.push_str("</td></tr>");
    if icons {
        s.push_str("<tr><td align=\"center\" style=\"padding:8px 32px\">");
        for n in 0..ICONS.len() {
            s.push_str(&format!(
                "<a href=\"https://{}/s/{n}\" style=\"text-decoration:none\"><img src=\"cid:icon{n}@demo\" width=\"24\" height=\"24\" alt=\"\" style=\"border:0;margin:0 6px\"></a>",
                org.domain
            ));
        }
        s.push_str("</td></tr>");
    }
    s.push_str(&format!(
        "<tr><td class=\"foot\" style=\"padding:16px 32px 32px 32px;font-size:11px;line-height:16px;color:#888888\">\
         Вы получили это письмо, потому что подписаны на новости {}. <a href=\"https://{}/unsubscribe\" style=\"color:#888888\">Отписаться</a>.\
         </td></tr></table></td></tr></table></body></html>",
        org.name, org.domain
    ));
    s
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// base64 строками по 76 знаков, каждая с `\r\n`.
fn base64(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len() * 4 / 3 + data.len() / 38 + 4);
    let mut col = 0;
    for c in data.chunks(3) {
        let n = u32::from(c[0]) << 16 | u32::from(c.get(1).copied().unwrap_or(0)) << 8 | u32::from(c.get(2).copied().unwrap_or(0));
        for k in 0..4 {
            s.push(if k <= c.len() { B64[(n >> (18 - 6 * k) & 63) as usize] as char } else { '=' });
            col += 1;
            if col == 76 {
                s.push_str("\r\n");
                col = 0;
            }
        }
    }
    if col > 0 {
        s.push_str("\r\n");
    }
    s
}

/// quoted-printable (RFC 2045 §6.7), как у почтовых программ: строки до 76 знаков с мягким переносом `=`, кроме
/// ASCII — `=XX`, переводы строки текста — как есть.
fn quoted_printable(text: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(text.len() * 2);
    for (n, line) in text.split("\r\n").enumerate() {
        if n > 0 {
            out.push_str("\r\n");
        }
        let mut col = 0;
        let bytes = line.as_bytes();
        for (i, &b) in bytes.iter().enumerate() {
            let last = i + 1 == bytes.len();
            let token: String = if (33..=126).contains(&b) && b != b'=' || (b == b' ' || b == b'\t') && !last {
                (b as char).to_string()
            } else {
                format!("={}{}", HEX[usize::from(b >> 4)] as char, HEX[usize::from(b & 15)] as char)
            };
            if col + token.len() > 75 {
                out.push_str("=\r\n");
                col = 0;
            }
            col += token.len();
            out.push_str(&token);
        }
    }
    out
}

/// Листовая часть в письмо: письмо, тип, параметры типа и заголовки, кодировка, раскодированное, закодированное.
type Leaf<'a> = dyn FnMut(&mut String, &str, &str, Encoding, &[u8], String) + 'a;

/// Письмо демо `i` целиком: байты, листовые части (как их даст воркер) и разобранное тело.
pub struct Letter {
    pub raw: Vec<u8>,
    pub parts: Vec<SplitPart>,
    pub body: BodyRow,
}

/// Письмо `demo<i>` целиком от отправителя `from` с темой `subject`, пришло в `at`.
pub fn letter(i: usize, from: (&str, &str), subject: &str, at: i64) -> Letter {
    let mut next = rng(i as u64);
    let org_n = org_of(i);
    let org = &ORGS[org_n];
    let with_logo = next() % 5 < 3;
    let newsletter = next().is_multiple_of(3);
    let qp = org_n.is_multiple_of(2);
    let date = jiff::Timestamp::from_second(at).unwrap_or(jiff::Timestamp::UNIX_EPOCH);
    let date = date.strftime("%a, %d %b %Y %H:%M:%S +0000").to_string();
    let mut raw = String::new();
    for hop in 0..3 {
        raw.push_str(&format!(
            "Received: from mx{hop}.{} (mx{hop}.{} [10.{}.{}.{}])\r\n\tby mail.corp.example with ESMTPS id {:016x}\r\n\tfor <{ME}>; {date}\r\n",
            org.domain,
            org.domain,
            i % 250,
            hop * 17,
            i % 199,
            next()
        ));
    }
    let sig: Vec<u8> = (0..256).map(|_| next() as u8).collect();
    raw.push_str(&format!(
        "DKIM-Signature: v=1; a=rsa-sha256; c=relaxed/relaxed; d={}; s=mail;\r\n\th=from:to:subject:date:message-id; bh={}\r\n\tb={}\r\n",
        org.domain,
        base64(&sig[..32]).trim_end(),
        base64(&sig).replace("\r\n", "\r\n\t").trim_end()
    ));
    raw.push_str(&format!("From: =?utf-8?B?{}?= <{}>\r\n", base64(from.0.as_bytes()).trim_end(), from.1));
    raw.push_str(&format!("To: =?utf-8?B?{}?= <{ME}>\r\n", base64(ME_NAME.as_bytes()).trim_end()));
    raw.push_str(&format!("Subject: =?utf-8?B?{}?=\r\n", base64(subject.as_bytes()).replace("\r\n", "").trim_end()));
    raw.push_str(&format!("Date: {date}\r\nMessage-ID: <demo{i}.{:x}@{}>\r\nMIME-Version: 1.0\r\n", next(), org.domain));
    if newsletter {
        raw.push_str(&format!("List-Unsubscribe: <https://{}/unsubscribe?u={:x}>\r\nList-Id: <news.{}>\r\n", org.domain, next(), org.domain));
    }
    let plain = text(i, org, &mut next);
    let markup = html(&plain, org, with_logo, newsletter);
    let mut parts = Vec::new();
    let mut leaf = |raw: &mut String, mime: &str, extra: &str, encoding: Encoding, decoded: &[u8], body: String| {
        let cte = match encoding {
            Encoding::Base64 => "base64",
            Encoding::QuotedPrintable => "quoted-printable",
        };
        raw.push_str(&format!("Content-Type: {mime}{extra}\r\nContent-Transfer-Encoding: {cte}\r\n\r\n"));
        let start = raw.len();
        raw.push_str(&body);
        parts.push(SplitPart { range: start..raw.len(), encoding, decoded: decoded.to_vec(), mime: mime.to_owned() });
        raw.push_str("\r\n");
    };
    let text_leaf = |raw: &mut String, leaf: &mut Leaf, mime: &str, s: &str| {
        if qp {
            leaf(raw, mime, "; charset=utf-8", Encoding::QuotedPrintable, s.as_bytes(), quoted_printable(s));
        } else {
            leaf(raw, mime, "; charset=utf-8", Encoding::Base64, s.as_bytes(), base64(s.as_bytes()));
        }
    };
    let attached = attachment(i);
    let images = with_logo || newsletter;
    raw.push_str("Content-Type: multipart/mixed; boundary=\"m\"\r\n\r\n--m\r\n");
    if images {
        raw.push_str("Content-Type: multipart/related; boundary=\"r\"; type=\"multipart/alternative\"\r\n\r\n--r\r\n");
    }
    raw.push_str("Content-Type: multipart/alternative; boundary=\"a\"\r\n\r\n--a\r\n");
    text_leaf(&mut raw, &mut leaf, "text/plain", &plain);
    raw.push_str("--a\r\n");
    text_leaf(&mut raw, &mut leaf, "text/html", &markup);
    raw.push_str("--a--\r\n");
    if images {
        if with_logo {
            raw.push_str("--r\r\n");
            leaf(
                &mut raw,
                "image/png",
                "; name=\"logo.png\"\r\nContent-ID: <logo@demo>\r\nContent-Disposition: inline",
                Encoding::Base64,
                logo(org_n),
                base64(logo(org_n)),
            );
        }
        if newsletter {
            for n in 0..ICONS.len() {
                raw.push_str("--r\r\n");
                let extra = format!("; name=\"icon{n}.png\"\r\nContent-ID: <icon{n}@demo>\r\nContent-Disposition: inline");
                leaf(&mut raw, "image/png", &extra, Encoding::Base64, icon(n), base64(icon(n)));
            }
        }
        raw.push_str("--r--\r\n");
    }
    if let Some((mime, name, data)) = &attached {
        raw.push_str("--m\r\n");
        let extra = format!("; name=\"{}\"\r\nContent-Disposition: attachment", name.replace(' ', "_"));
        leaf(&mut raw, mime, &extra, Encoding::Base64, data, base64(data));
    }
    raw.push_str("--m--\r\n");
    let body = BodyRow {
        to: vec![postman_cache::Addr { name: ME_NAME.into(), email: ME.into() }],
        text: Some(plain),
        html: Some(markup),
        message_id: Some(format!("demo{i}@{}", org.domain)),
        ..BodyRow::default()
    };
    Letter { raw: raw.into_bytes(), parts, body }
}

/// Письма основной учётки с содержимым есть (шаблон новее замера хранилища).
pub(crate) fn present(cache: &Cache) -> postman_cache::Result<bool> {
    Ok(match cache.letter_of("", "demo0")? {
        Some(l) => cache.letter(l)?.is_some_and(|r| r.content.is_some()),
        None => true,
    })
}

/// Содержимое и тела писем `demo<i>` (`i` кратно [`EVERY`], меньше `n`) — пачками по одной транзакции.
pub(crate) fn fill(cache: &Cache, n: usize) -> postman_cache::Result<()> {
    if n < MIN_LETTERS {
        return Ok(());
    }
    const BATCH: usize = 200;
    let ids: Vec<usize> = (0..n).step_by(EVERY).collect();
    for chunk in ids.chunks(BATCH) {
        let mut letters = Vec::with_capacity(chunk.len());
        for &i in chunk {
            let key = format!("demo{i}");
            let Some(l) = cache.letter_of("", &key)? else { continue };
            let Some(row) = cache.letter(l)? else { continue };
            letters.push((l, key, letter(i, (&row.from_name, &row.from_addr), &row.subject, row.received_at)));
        }
        let items: Vec<_> = letters.iter().map(|(l, _, x)| (*l, x.raw.as_slice(), x.parts.as_slice())).collect();
        cache.put_contents(&items)?;
        for (_, key, x) in &letters {
            cache.put_body(key, &x.body)?;
        }
    }
    Ok(())
}
