//! Исходящие письма: черновик ответа/пересылки из IR оригинала и сборка
//! RFC 5322 (multipart/alternative: Markdown как text/plain + HTML из
//! фиксированного набора тегов). Без I/O.

pub mod display;
pub mod mailto;
pub mod vcard;

pub use mailto::{Mailto, parse_mailto};

use std::borrow::Cow;

use mail_builder::MessageBuilder;
use mail_builder::headers::address::Address as MbAddress;
use mail_builder::headers::content_type::ContentType;
use mail_builder::mime::MimePart;
use postman_ir::Document;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Address {
    pub name: String,
    pub email: String,
}

impl Address {
    pub fn new(name: impl Into<String>, email: impl Into<String>) -> Self {
        Self { name: name.into(), email: email.into() }
    }

    fn same(&self, other: &Address) -> bool {
        self.email.eq_ignore_ascii_case(&other.email)
    }

    /// `Имя <адрес>` или просто адрес.
    pub fn display(&self) -> String {
        if self.name.is_empty() { self.email.clone() } else { format!("{} <{}>", self.name, self.email) }
    }
}

#[derive(Debug, Clone)]
pub struct Attachment {
    pub name: String,
    pub mime: String,
    pub data: Vec<u8>,
    /// Content-ID для картинок, вставленных в текст (`![](cid:…)`).
    pub inline_cid: Option<String>,
    /// Вложение оригинала при пересылке: данные скачиваются с сервера перед отправкой.
    pub blob_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Draft {
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    /// В заголовки не попадает — только в конверт отправки.
    pub bcc: Vec<Address>,
    pub subject: String,
    /// Текст письма в Markdown (вместе с цитатой).
    pub markdown: String,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub attachments: Vec<Attachment>,
    /// Часть `text/calendar` рядом с текстом (iMIP: ответ на приглашение).
    pub calendar: Option<CalendarPart>,
    /// Ключевые слова письма на сервере сверх `$draft`/`$seen` (S5-06б): категория, «Конфиденциально», «Ждать
    /// ответа» (`postman_core::marks`). В письмо (RFC 5322) не попадают.
    pub keywords: Vec<String>,
    /// Подписать S/MIME при отправке (S5-12 Ф3): сертификатом адреса «От» — RSA/EC через CNG, ГОСТ — через СКЗИ.
    /// Черновики не подписываются.
    pub sign: bool,
    /// Зашифровать S/MIME (S5-12 Ф4, «Конфиденциально»): подготовленные получатели, себя — тоже. Пусто — открыто.
    /// Черновик «Конфиденциально» — только себе (решение владельца №5).
    pub encrypt_to: Vec<postman_smime::seal::To>,
}

/// Объект iCalendar в письме (RFC 6047): метод и текст VCALENDAR.
#[derive(Debug, Clone)]
pub struct CalendarPart {
    pub method: String,
    pub ics: String,
}

impl Draft {
    /// Все получатели для SMTP-конверта.
    pub fn recipients(&self) -> impl Iterator<Item = &Address> {
        self.to.iter().chain(&self.cc).chain(&self.bcc)
    }
}

/// Метаданные оригинала, нужные для ответа и пересылки.
#[derive(Debug, Clone, Default)]
pub struct Original {
    pub message_id: Option<String>,
    pub references: Vec<String>,
    pub from: Vec<Address>,
    pub reply_to: Vec<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub subject: String,
    /// Уже отформатированная дата для строки цитаты.
    pub date: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Reply,
    ReplyAll,
    Forward,
}

/// Черновик ответа или пересылки. Курсор — в начале `markdown` (ответ сверху, как в Outlook);
/// подпись вставляется перед цитатой.
pub fn respond(orig: &Original, body: &Document, mode: Mode, me: &Address, signature: &str) -> Draft {
    let mut d = Draft { from: me.clone(), ..Default::default() };
    let mut top = String::from("\n\n");
    if !signature.trim().is_empty() {
        top.push_str(signature.trim_end());
        top.push_str("\n\n");
    }
    match mode {
        Mode::Reply | Mode::ReplyAll => {
            d.subject = prefixed(&orig.subject, "RE: ");
            let reply_to = if orig.reply_to.is_empty() { &orig.from } else { &orig.reply_to };
            push_unique(&mut d.to, reply_to.iter().filter(|a| !a.same(me)), &[]);
            if mode == Mode::ReplyAll {
                let others: Vec<_> = orig.to.iter().chain(&orig.cc).filter(|a| !a.same(me)).collect();
                push_unique(&mut d.cc, others.into_iter(), &d.to.clone());
            }
            // Ответ самому себе (в «Отправленных»): адресат — исходные получатели.
            if d.to.is_empty() {
                push_unique(&mut d.to, orig.to.iter(), &[]);
            }
            d.in_reply_to = orig.message_id.clone();
            d.references = orig.references.clone();
            if let Some(id) = &orig.message_id
                && !d.references.contains(id)
            {
                d.references.push(id.clone());
            }
            let who = orig.from.first().map(Address::display).unwrap_or_default();
            top.push_str(&postman_ir::quote(body, &format!("{}, {} пишет:", escape_md(&orig.date), escape_md(&who))));
        }
        Mode::Forward => {
            d.subject = prefixed(&orig.subject, "FW: ");
            let list = |v: &[Address]| v.iter().map(Address::display).collect::<Vec<_>>().join(", ");
            top.push_str("-------- Пересланное сообщение --------  \n");
            top.push_str(&format!("**От:** {}  \n", escape_md(&list(&orig.from))));
            top.push_str(&format!("**Дата:** {}  \n", escape_md(&orig.date)));
            top.push_str(&format!("**Кому:** {}  \n", escape_md(&list(&orig.to))));
            if !orig.cc.is_empty() {
                top.push_str(&format!("**Копия:** {}  \n", escape_md(&list(&orig.cc))));
            }
            top.push_str(&format!("**Тема:** {}\n\n", escape_md(&orig.subject)));
            top.push_str(&postman_ir::to_markdown(body));
            top.push('\n');
        }
    }
    d.markdown = top;
    d
}

/// Разбор поля «Кому»: `Имя <a@b>, c@d; "Фамилия, Имя" <e@f>`.
/// Возвращает адреса и нераспознанные фрагменты (чтобы подсветить их в поле).
pub fn parse_addresses(input: &str) -> (Vec<Address>, Vec<String>) {
    let mut parts = Vec::new();
    let (mut cur, mut quoted, mut angle) = (String::new(), false, false);
    for c in input.chars() {
        match c {
            '"' if !angle => quoted = !quoted,
            '<' if !quoted => angle = true,
            '>' if !quoted => angle = false,
            ',' | ';' | '\n' if !quoted && !angle => {
                parts.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    parts.push(cur);
    let (mut ok, mut bad) = (Vec::new(), Vec::new());
    for p in parts.iter().map(|p| p.trim()).filter(|p| !p.is_empty()) {
        let (name, email) = match (p.rfind('<'), p.rfind('>')) {
            (Some(l), Some(r)) if l < r => (p[..l].trim().trim_matches('"').trim(), p[l + 1..r].trim()),
            _ => ("", p.trim_matches(|c| c == '<' || c == '>')),
        };
        let valid =
            email.split_once('@').is_some_and(|(l, d)| !l.is_empty() && d.contains('.') && !d.ends_with('.')) && !email.contains(char::is_whitespace);
        if valid {
            ok.push(Address::new(name, email));
        } else {
            bad.push(p.to_string());
        }
    }
    (ok, bad)
}

/// Как в Outlook: префикс не дублируется, если тема уже начинается с RE:/FW:/Fwd:/Отв:/Пересл:.
fn prefixed(subject: &str, prefix: &str) -> String {
    let s = subject.trim();
    let lower = s.to_lowercase();
    let wanted = prefix.trim_end().trim_end_matches(':').to_lowercase();
    let same: &[&str] = if wanted == "re" { &["re:", "отв:", "ответ:"] } else { &["fw:", "fwd:", "пересл:"] };
    if same.iter().any(|p| lower.starts_with(p)) { s.to_string() } else { format!("{prefix}{s}") }
}

fn push_unique<'a>(out: &mut Vec<Address>, src: impl Iterator<Item = &'a Address>, exclude: &[Address]) {
    for a in src {
        if a.email.is_empty() || out.iter().chain(exclude).any(|b| b.same(a)) {
            continue;
        }
        out.push(a.clone());
    }
}

fn escape_md(s: &str) -> String {
    s.replace('\\', "\\\\").replace('*', "\\*").replace('_', "\\_").replace('<', "\\<").replace('[', "\\[")
}

/// Убрать переводы строк из значений заголовков (защита от инъекции заголовков).
fn header_safe(s: &str) -> Cow<'_, str> {
    if s.contains(['\r', '\n']) { Cow::Owned(s.replace(['\r', '\n'], " ")) } else { Cow::Borrowed(s) }
}

fn mb_list(list: &[Address]) -> MbAddress<'_> {
    MbAddress::new_list(
        list.iter()
            .map(|a| {
                let name = (!a.name.is_empty()).then(|| header_safe(&a.name));
                MbAddress::new_address(name, header_safe(&a.email))
            })
            .collect(),
    )
}

/// Собрать письмо. `date` — Unix-время; `message_id` — без угловых скобок.
pub fn build(d: &Draft, date: i64, message_id: &str) -> std::io::Result<Vec<u8>> {
    let html = format!("<!DOCTYPE html><html><head><meta charset=\"utf-8\"></head><body>{}</body></html>", postman_ir::markdown_to_html(&d.markdown));
    let mut m = MessageBuilder::new()
        .from(mb_list(std::slice::from_ref(&d.from)))
        .subject(header_safe(&d.subject))
        .date(date)
        .message_id(message_id)
        .text_body(d.markdown.as_str())
        .html_body(html.as_str());
    if !d.to.is_empty() {
        m = m.to(mb_list(&d.to));
    }
    if !d.cc.is_empty() {
        m = m.cc(mb_list(&d.cc));
    }
    if let Some(id) = &d.in_reply_to {
        m = m.in_reply_to(id.as_str());
    }
    if !d.references.is_empty() {
        m = m.references(d.references.as_slice());
    }
    if let Some(cal) = &d.calendar {
        // multipart/alternative [text, html, text/calendar; method=…] — так его понимают Outlook и Gmail.
        let alt = MimePart::new(
            "multipart/alternative",
            vec![
                MimePart::new("text/plain", d.markdown.as_str()),
                MimePart::new("text/html", html.as_str()),
                MimePart::new(
                    ContentType::new("text/calendar").attribute("method", cal.method.as_str()).attribute("charset", "utf-8"),
                    cal.ics.as_str(),
                ),
            ],
        );
        let body = if d.attachments.is_empty() {
            alt
        } else {
            let mut parts = vec![alt];
            parts.extend(d.attachments.iter().map(|a| match &a.inline_cid {
                Some(cid) => MimePart::new(a.mime.as_str(), a.data.as_slice()).inline().cid(cid.as_str()),
                None => MimePart::new(a.mime.as_str(), a.data.as_slice()).attachment(header_safe(&a.name)),
            }));
            MimePart::new("multipart/mixed", parts)
        };
        return m.body(body).write_to_vec();
    }
    for a in &d.attachments {
        let name = header_safe(&a.name).into_owned();
        m = match &a.inline_cid {
            Some(cid) => m.inline(a.mime.clone(), cid.clone(), a.data.as_slice()),
            None => m.attachment(a.mime.clone(), name, a.data.as_slice()),
        };
    }
    m.write_to_vec()
}

/// Message-ID в домене отправителя: `<время.случайное@домен>`.
pub fn new_message_id(from: &Address, now: i64) -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_i64(now);
    let domain = from.email.rsplit_once('@').map(|(_, d)| d).filter(|d| !d.is_empty()).unwrap_or("localhost");
    format!("{now:x}.{:016x}@{domain}", h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mail_parser::{MessageParser, MimeHeaders};

    fn me() -> Address {
        Address::new("Я", "me@corp.example")
    }

    fn original() -> Original {
        Original {
            message_id: Some("orig@corp.example".into()),
            references: vec!["root@corp.example".into()],
            from: vec![Address::new("Иван", "ivan@corp.example")],
            reply_to: vec![],
            to: vec![me(), Address::new("Пётр", "petr@corp.example")],
            cc: vec![Address::new("", "IVAN@corp.example"), Address::new("Анна", "anna@corp.example")],
            subject: "RE: Отчёт".into(),
            date: "17.10.2026 12:00".into(),
        }
    }

    #[test]
    fn reply_all_recipients() {
        let body = postman_ir::from_plain("текст");
        let d = respond(&original(), &body, Mode::ReplyAll, &me(), "С уважением,\nЯ");
        assert_eq!(d.to, vec![Address::new("Иван", "ivan@corp.example")]);
        let cc: Vec<_> = d.cc.iter().map(|a| a.email.as_str()).collect();
        assert_eq!(cc, ["petr@corp.example", "anna@corp.example"]);
        assert_eq!(d.subject, "RE: Отчёт");
        assert_eq!(d.references, ["root@corp.example", "orig@corp.example"]);
        assert!(d.markdown.starts_with("\n\nС уважением,\nЯ\n\n17.10.2026 12:00, Иван \\<ivan@corp.example> пишет:\n> текст"), "{}", d.markdown);
    }

    #[test]
    fn forward_subject_and_header() {
        let mut o = original();
        o.subject = "Отчёт".into();
        let d = respond(&o, &postman_ir::from_plain("x"), Mode::Forward, &me(), "");
        assert_eq!(d.subject, "FW: Отчёт");
        assert!(d.to.is_empty() && d.in_reply_to.is_none());
        assert!(d.markdown.contains("**От:** Иван \\<ivan@corp.example>"));
    }

    #[test]
    fn builds_parsable_multipart() {
        let mut d = respond(&original(), &postman_ir::from_plain("q"), Mode::Reply, &me(), "");
        d.markdown = format!("Привет, **коллеги**!\n\n<script>x</script>{}", d.markdown);
        d.subject = "Тема\r\nBcc: evil@x".into();
        d.bcc = vec![Address::new("", "secret@corp.example")];
        d.attachments.push(Attachment { name: "a.txt".into(), mime: "text/plain".into(), data: b"data".to_vec(), inline_cid: None, blob_id: None });
        let raw = build(&d, 1_792_240_200, "id1@corp.example").unwrap();
        let text = String::from_utf8_lossy(&raw);
        assert!(!text.contains("secret@corp.example"), "Bcc не должен попадать в заголовки");
        let msg = MessageParser::default().parse(&raw).unwrap();
        assert_eq!(msg.subject(), Some("Тема  Bcc: evil@x"));
        assert!(msg.header("Bcc").is_none());
        assert_eq!(msg.in_reply_to().as_text(), Some("orig@corp.example"));
        assert_eq!(msg.attachment_count(), 1);
        let html = msg.body_html(0).unwrap();
        assert!(html.contains("<strong>коллеги</strong>"));
        assert!(!html.contains("<script>"));
        assert!(msg.body_text(0).unwrap().contains("**коллеги**"));
    }

    #[test]
    fn address_field() {
        let (ok, bad) = parse_addresses(r#""Иванов, Иван" <ivan@corp.example>; petr@corp.example, мусор,  Анна <anna@x.ru>"#);
        assert_eq!(
            ok,
            vec![Address::new("Иванов, Иван", "ivan@corp.example"), Address::new("", "petr@corp.example"), Address::new("Анна", "anna@x.ru"),]
        );
        assert_eq!(bad, ["мусор"]);
    }

    #[test]
    fn message_id_domain() {
        let id = new_message_id(&me(), 5);
        assert!(id.starts_with("5.") && id.ends_with("@corp.example"));
    }

    #[test]
    fn imip_reply_part() {
        let d = Draft {
            from: me(),
            to: vec![Address::new("Иван", "ivan@partner.example")],
            subject: "Принято: Встреча".into(),
            markdown: "Принято".into(),
            calendar: Some(CalendarPart { method: "REPLY".into(), ics: "BEGIN:VCALENDAR\r\nMETHOD:REPLY\r\nEND:VCALENDAR\r\n".into() }),
            ..Default::default()
        };
        let raw = build(&d, 0, "x@corp.example").unwrap();
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("multipart/alternative"), "{text}");
        assert!(text.to_ascii_lowercase().contains("text/calendar; method=\"reply\"") || text.contains("method=REPLY"), "{text}");
        let msg = MessageParser::default().parse(&raw).unwrap();
        let cal = msg.parts.iter().find(|p| p.is_content_type("text", "calendar")).unwrap();
        assert!(cal.text_contents().unwrap().contains("METHOD:REPLY"));
        assert_eq!(msg.body_text(0).as_deref(), Some("Принято"));
    }
}
