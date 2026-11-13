//! Беседа демо для «Медиа беседы» (MED-01): в четырёх письмах есть всё, что собирает `postman_core::thread_media`, —
//! файл, пересланный дважды (другой блоб, те же байты), картинка подписи в письмах двух авторов, пиксель
//! отслеживания, фото, схема в тексте, видео, ссылки (в том числе только в цитате, `mailto:` и `javascript:`),
//! ссылки отписки и отслеживания, визитка vCard и приглашение. Письма — в «Архиве», месяц назад: на снимках
//! списков их не видно.

use std::sync::OnceLock;

use postman_cache::{Addr, AttachmentRow, BodyRow, Cache, EmailRow, Recipients};

use crate::{ME, ME_NAME, SENDERS, TEAM};

/// Ключ беседы (`EmailRow::thread_id`).
pub const THREAD: &str = "media";
/// Тема беседы.
pub const SUBJECT: &str = "Пилот «Маяк»: материалы";
/// Письма беседы по времени (старые первыми).
pub const LETTERS: [&str; 4] = ["media1", "media2", "media3", "media4"];
/// Файл, пересланный дважды: в `media1` и `media2`, блобы разные, байты одни.
pub const TWICE: &str = "ТЗ пилота «Маяк».pdf";
pub const TWICE_BLOBS: [&str; 2] = ["media-tz-1", "media-tz-2"];
/// Картинка подписи: одни байты в письмах Ивановой (`media1`, `media4`) и Петрова (`media3`).
pub const SIGN_BLOBS: [&str; 3] = ["media-sign-1", "media-sign-3", "media-sign-4"];
/// Пиксель отслеживания 1×1 в `media1`.
pub const PIXEL_BLOB: &str = "media-px";
/// Фото вложением, схема в тексте и видео — в `media3`.
pub const PHOTO_BLOB: &str = "media-photo";
pub const SCHEME_BLOB: &str = "media-scheme";
pub const VIDEO_BLOB: &str = "media-video";
/// Визитка в `media2`.
pub const CARD_BLOB: &str = "media-vcf";
pub const CARD_NAME: &str = "Сидоров Игорь.vcf";
/// Приглашение в `media4`.
pub const INVITE_BLOB: &str = "media-ics";
/// Ссылка в новом тексте `media1` и `media4` (в цитате `media2` — тоже, но цитата не считается).
pub const PLAN_URL: &str = "https://wiki.corp.example/mayak/plan";
/// Ссылки только в цитатах: в медиа беседы их нет.
pub const QUOTED_URLS: [&str; 2] = ["https://wiki.corp.example/mayak/v0", "https://legacy.corp.example/ticket/42"];
pub const SHEET_URL: &str = "https://docs.corp.example/sheets/mayak";
pub const UNSUBSCRIBE_URL: &str = "https://lists.corp.example/unsubscribe?list=mayak";
pub const TRACKING_URL: &str = "https://click.corp.example/t/abc?utm_source=mail";

/// Дней назад — первое письмо беседы.
const DAYS_AGO: i64 = 30;

const VCARD: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Сидоров Игорь\r\nN:Сидоров;Игорь;;;\r\nORG:Отдел эксплуатации\r\n\
TITLE:Администратор стенда\r\nEMAIL;TYPE=work:i.sidorov@corp.example\r\nTEL;TYPE=cell:+7 000 000-00-01\r\nEND:VCARD\r\n";

const VIDEO: &[u8] = b"\0\0\0\x18ftypmp42demo video";

const HTML_1: &str = r#"<p>Анна, добрый день!</p>
<p>Направляю ТЗ пилота «Маяк». Черновик плана — <a href="https://wiki.corp.example/mayak/plan">в вики</a>.</p>
<p>С уважением,<br>Мария Иванова<br><img src="cid:sign1@media" width=120 height=40 alt="Логотип отдела"></p>
<img src="cid:px@media" width=1 height=1 alt="">"#;

const HTML_2: &str = r#"<html><body><div class=WordSection1><p class=MsoNormal>Мария, спасибо!</p>
<p class=MsoNormal>Карточка администратора стенда — во вложении. Сводная таблица — <a href="https://docs.corp.example/sheets/mayak">по ссылке</a>,
вопросы — <a href="mailto:i.sidorov@corp.example">Игорю</a>.</p><p class=MsoNormal>ТЗ пересылаю с правками.</p>
<p class=MsoNormal>С уважением,<br>Анна Смирнова</p>
<div style='border:none;border-top:solid #E1E1E1 1.0pt;padding:3.0pt 0cm 0cm 0cm'><p class=MsoNormal><b>От:</b> Иванова Мария
&lt;m.ivanova@corp.example&gt;<br><b>Отправлено:</b> понедельник, 12 октября 2026 г. 10:15<br><b>Кому:</b> Смирнова Анна<br>
<b>Тема:</b> Пилот «Маяк»: материалы</p></div><p class=MsoNormal>Направляю ТЗ пилота «Маяк». Черновик плана —
<a href="https://wiki.corp.example/mayak/plan">в вики</a>, прежний — <a href="https://wiki.corp.example/mayak/v0">здесь</a>.</p></div></body></html>"#;

const HTML_3: &str = r#"<div dir="ltr">Коллеги, фото стенда и видео обхода — во вложении, схема — ниже.<br>
<img src="cid:scheme@media" width=600 height=375 alt="Схема стенда"><br>
<a href="javascript:alert(1)">Нажмите здесь</a><br>--<br>Алексей Петров<br>
<img src="cid:sign3@media" width=120 height=40 alt="Логотип"></div><br>
<div class="gmail_quote"><div dir="ltr" class="gmail_attr">пн, 12 окт. 2026 г. в 12:10, Смирнова Анна &lt;me@corp.example&gt;:<br></div>
<blockquote class="gmail_quote"><p>Мария, спасибо!</p><p>Прежний план — <a href="https://wiki.corp.example/mayak/v0">здесь</a>.</p></blockquote></div>"#;

const HTML_4: &str = r#"<p>Анна, приглашение на демонстрацию пилота — во вложении.</p>
<p>План — <a href="https://wiki.corp.example/mayak/plan">https://wiki.corp.example/mayak/plan</a>.</p>
<p><a href="https://click.corp.example/t/abc?utm_source=mail">Регистрация</a> ·
<a href="https://lists.corp.example/unsubscribe?list=mayak">Отписаться от рассылки проекта</a></p>
<p>С уважением,<br>Мария Иванова<br><img src="cid:sign4@media" width=120 height=40 alt="Логотип отдела"></p>
<blockquote><p>Старая заявка: <a href="https://legacy.corp.example/ticket/42">#42</a></p></blockquote>"#;

fn maria() -> Addr {
    Addr { name: SENDERS[0].0.into(), email: SENDERS[0].1.into() }
}

fn petrov() -> Addr {
    Addr { name: SENDERS[1].0.into(), email: SENDERS[1].1.into() }
}

fn me() -> Addr {
    Addr { name: ME_NAME.into(), email: ME.into() }
}

fn team() -> Addr {
    Addr { name: "Команда проекта".into(), email: TEAM.into() }
}

fn att(blob: &str, name: &str, mime: &str, size: usize, cid: Option<&str>) -> AttachmentRow {
    AttachmentRow { blob_id: blob.into(), name: name.into(), mime: mime.into(), size: size as u64, cid: cid.map(Into::into), inline: cid.is_some() }
}

fn sign(n: u8) -> AttachmentRow {
    att(&format!("media-sign-{n}"), "image001.png", "image/png", crate::LOGO.len(), Some(&format!("sign{n}@media")))
}

/// Строки писем беседы; `now` — Unix-время, секунды.
pub fn rows(now: i64) -> Vec<EmailRow> {
    let start = now - DAYS_AGO * 86_400;
    let from = [maria(), me(), petrov(), maria()];
    LETTERS
        .iter()
        .zip(from)
        .enumerate()
        .map(|(i, (id, from))| EmailRow {
            id: (*id).into(),
            thread_id: THREAD.into(),
            mailbox_ids: vec![if from.email == ME { "sent" } else { "archive" }.into()],
            received_at: start + i as i64 * 5 * 3600,
            subject: if i == 0 { SUBJECT.into() } else { format!("RE: {SUBJECT}") },
            from_name: from.name,
            from_addr: from.email,
            preview: String::new(),
            seen: true,
            flagged: false,
            has_attachment: true,
            size: 64 * 1024,
            keywords: Vec::new(),
        })
        .collect()
}

/// Тело письма беседы; `None` — письмо не из неё.
pub fn body(id: &str) -> Option<BodyRow> {
    let tz = crate::REPORT.len();
    let (to, cc, html, attachments) = match id {
        "media1" => (
            vec![me()],
            vec![petrov(), team()],
            HTML_1,
            vec![
                att(TWICE_BLOBS[0], TWICE, "application/pdf", tz, None),
                sign(1),
                att(PIXEL_BLOB, "image002.png", "image/png", pixel().len(), Some("px@media")),
            ],
        ),
        "media2" => (
            vec![maria()],
            vec![petrov()],
            HTML_2,
            vec![att(TWICE_BLOBS[1], TWICE, "application/pdf", tz, None), att(CARD_BLOB, CARD_NAME, "text/vcard", VCARD.len(), None)],
        ),
        "media3" => (
            vec![me(), maria()],
            Vec::new(),
            HTML_3,
            vec![
                att(PHOTO_BLOB, "Фото стенда.jpg", "image/jpeg", crate::PHOTO.len(), None),
                att(VIDEO_BLOB, "Обход стенда.mp4", "video/mp4", VIDEO.len(), None),
                att(SCHEME_BLOB, "scheme.png", "image/png", scheme().len(), Some("scheme@media")),
                sign(3),
            ],
        ),
        "media4" => {
            (vec![me()], Vec::new(), HTML_4, vec![att(INVITE_BLOB, "Демонстрация пилота.ics", "text/calendar", crate::invite().len(), None), sign(4)])
        }
        _ => return None,
    };
    Some(BodyRow { to, cc, message_id: Some(format!("{id}@corp.example")), html: Some(html.into()), attachments, ..Default::default() })
}

/// Байты вложений беседы; `None` — блоб не из неё.
pub fn blob(blob_id: &str) -> Option<Vec<u8>> {
    Some(match blob_id {
        b if TWICE_BLOBS.contains(&b) => crate::REPORT.to_vec(),
        b if SIGN_BLOBS.contains(&b) => crate::LOGO.to_vec(),
        PIXEL_BLOB => pixel().to_vec(),
        PHOTO_BLOB => crate::PHOTO.to_vec(),
        SCHEME_BLOB => scheme().to_vec(),
        VIDEO_BLOB => VIDEO.to_vec(),
        CARD_BLOB => VCARD.as_bytes().to_vec(),
        INVITE_BLOB => crate::invite(),
        _ => return None,
    })
}

/// Записать беседу в ящик: строки и получатели. Тела и вложения, как у остальных писем демо, отдаёт путь демо по
/// запросу ([`crate::body`], [`crate::blob`]): медиа беседы видят «загружено 0 из 4» и просят тела.
pub(crate) fn fill(cache: &Cache, now: i64) -> postman_cache::Result<()> {
    cache.upsert_emails(&rows(now))?;
    let to: Vec<(String, Recipients)> = LETTERS
        .iter()
        .filter_map(|id| body(id).map(|b| (id.to_string(), b)))
        .map(|(id, b)| (id, Recipients { to: b.to.iter().map(|a| a.email.clone()).collect(), cc: b.cc.iter().map(|a| a.email.clone()).collect() }))
        .collect();
    cache.set_recipients(&to)
}

/// Пиксель отслеживания: PNG 1×1.
fn pixel() -> &'static [u8] {
    static PIXEL: OnceLock<Vec<u8>> = OnceLock::new();
    PIXEL.get_or_init(|| png_of(1, 1).unwrap_or_default())
}

/// Схема стенда в тексте письма: PNG 600×375.
fn scheme() -> &'static [u8] {
    static SCHEME: OnceLock<Vec<u8>> = OnceLock::new();
    SCHEME.get_or_init(|| crate::chart_png(600, 375).unwrap_or_default())
}

fn png_of(w: u32, h: u32) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().ok()?;
    writer.write_image_data(&vec![0xFF; (w * h * 3) as usize]).ok()?;
    writer.finish().ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_thread_is_complete() {
        let rows = rows(2_000_000_000);
        assert_eq!(rows.len(), LETTERS.len());
        assert!(rows.windows(2).all(|w| w[0].received_at < w[1].received_at));
        for id in LETTERS {
            let b = body(id).unwrap();
            assert!(b.attachments.iter().all(|a| blob(&a.blob_id).is_some_and(|bytes| bytes.len() as u64 == a.size)), "{id}");
        }
        assert_eq!(blob(TWICE_BLOBS[0]), blob(TWICE_BLOBS[1]));
        assert!(!pixel().is_empty() && !scheme().is_empty());
    }

    #[test]
    fn media_thread_in_demo_box() {
        let cache = Cache::open_in_memory().unwrap();
        fill(&cache, 2_000_000_000).unwrap();
        let ids: Vec<String> = cache.thread(THREAD).unwrap().into_iter().map(|r| r.id).collect();
        assert_eq!(ids, LETTERS);
        // Тела — по запросу, как у остальных писем демо.
        assert!(cache.body("media2").unwrap().is_none());
        assert_eq!(crate::body("media2").attachments.len(), 2);
        assert_eq!(crate::blob(CARD_BLOB), Some(VCARD.as_bytes().to_vec()));
    }
}
