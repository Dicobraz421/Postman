//! «Переслать в…» и «Ответить в…» между каналами (ADR 0012 п. 11): письмо → чат, сообщение чата → письмом, чат →
//! другой чат. Ядро собирает исходящее из хранилища: тело — в IR со строкой «Переслано от Имя, дата» (текст строки —
//! у интерфейса, Fluent), вложения — по хэшу из хранилища содержимого, у источника заново не скачиваются; не загружены —
//! сначала загрузка у источника ([`Refused::NotLoaded`]).
//!
//! - **В чат** ([`Plan::Chat`]): выгрузить файлы (`Request::Upload`, байты — [`bytes`]) и отправить через очередь
//!   (`Request::Send`): окно «Отменить» и «позже» — как у любого сообщения.
//! - **В почту** ([`Plan::Mail`]): черновик `compose::Draft` с вложениями — в окно письма.
//! - **Внутри одной учётки** канала с `forward_native` — пересылка сервером ([`Plan::Native`], без выгрузки).
//! - **Пределы цели** (`max_text`, `max_attachment`) проверяются **до** отправки: не влезает — [`Refused`] с причиной.
//!
//! «Ответить в…» ([`reply_in`]) — цитата фрагмента источника и ссылка канала на него (`Links::outside`: `mid:`,
//! `https://t.me/c/…`), если она есть, иначе `postman://`. Чтение базы — I/O: из фона, не из потока интерфейса.

use postman_cache::{Cache, Hash};
use postman_compose::{Attachment as MailFile, Draft};
use postman_ir::{Align, Block, Document, Inline};

use super::mail::MailSource;
use super::{
    Action, Address, Attachment, Body, ChannelError, ChannelFeatures, ChannelStore, Entry, FileMeta, Links, Message, Outgoing, Quote, text_doc,
};

type Result<T> = std::result::Result<T, ChannelError>;

/// Откуда байты вложения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bytes {
    /// Часть хранилища содержимого по SHA-256 (вложение чата, часть письма).
    Part(Hash),
    /// Кэш блобов письма (скачанное вложение).
    Blob(String),
    /// Не загружено: сначала загрузить у источника.
    Missing,
}

/// Вложение источника и откуда его байты.
#[derive(Debug, Clone, PartialEq)]
pub struct File {
    pub attachment: Attachment,
    pub bytes: Bytes,
}

/// Сообщение-источник: адрес, само сообщение, имя автора, название беседы и вложения.
#[derive(Debug, Clone, PartialEq)]
pub struct Picked {
    pub at: Address,
    pub message: Message,
    pub author: String,
    pub title: String,
    pub files: Vec<File>,
}

/// Сообщение по адресу — из чата (база каналов) или почты (`mail`). Нет — `None`.
pub fn pick(cache: &Cache, mail: Option<&MailSource>, at: &Address) -> Result<Option<Picked>> {
    if at.channel.0 == super::MAIL {
        return match mail {
            Some(m) => pick_mail(cache, m, at),
            None => Ok(None),
        };
    }
    let (Some(store), Some(id)) = (ChannelStore::find(cache, &at.channel, &at.account)?, at.message.as_ref()) else { return Ok(None) };
    let Some(Entry::Message { message, loaded, .. }) = store.message(&at.conversation, id)? else { return Ok(None) };
    let author = store.participant(&message.author)?.map_or_else(|| message.author.0.clone(), |p| p.name);
    let title = store.conversation(&at.conversation)?.map(|c| c.conversation.title).unwrap_or_default();
    let files = message
        .attachments
        .iter()
        .map(|a| {
            let bytes = loaded.iter().find(|(s, _)| *s == a.source).map_or(Bytes::Missing, |(_, h)| Bytes::Part(*h));
            File { attachment: a.clone(), bytes }
        })
        .collect();
    Ok(Some(Picked { at: at.clone(), message, author, title, files }))
}

fn pick_mail(cache: &Cache, mail: &MailSource, at: &Address) -> Result<Option<Picked>> {
    let Some((row, body)) = mail.find(at)? else { return Ok(None) };
    let message = mail.message_of(&row, body.as_ref());
    let parts = match crate::store::letter_of_key(cache, &row.id)? {
        Some(l) => cache.letter_parts(crate::store::cache_id(l))?,
        None => Vec::new(),
    };
    let rows: Vec<postman_cache::AttachmentRow> = body.map(|b| b.attachments.into_iter().filter(|a| !a.inline).collect()).unwrap_or_default();
    let mut files = Vec::with_capacity(message.attachments.len());
    for (a, r) in message.attachments.iter().zip(&rows) {
        let bytes = match crate::samefile::hash_of(r, &parts, |_| None) {
            Some(h) => Bytes::Part(h),
            None if cache.blob(&r.blob_id)?.is_some() => Bytes::Blob(r.blob_id.clone()),
            None => Bytes::Missing,
        };
        files.push(File { attachment: a.clone(), bytes });
    }
    let author = if row.from_name.trim().is_empty() { row.from_addr.clone() } else { row.from_name.clone() };
    let at = mail.resolve(at)?.unwrap_or_else(|| at.clone());
    Ok(Some(Picked { at, message, author, title: row.subject, files }))
}

/// Байты вложения из хранилища; не загружено — `None`.
pub fn bytes(cache: &Cache, b: &Bytes) -> Result<Option<Vec<u8>>> {
    Ok(match b {
        Bytes::Part(h) => cache.chan_part(*h)?,
        Bytes::Blob(id) => cache.blob(id)?,
        Bytes::Missing => None,
    })
}

/// Выгрузить в канал цели перед отправкой: что и откуда байты.
#[derive(Debug, Clone, PartialEq)]
pub struct Upload {
    pub meta: FileMeta,
    pub bytes: Bytes,
}

/// Что сделать, чтобы переслать или ответить.
#[derive(Debug, Clone)]
pub enum Plan {
    /// Пересылка сервером внутри учётки (`forward_native`): действие каналу.
    Native(Action),
    /// В чат: выгрузить файлы `uploads` по порядку (`Request::Upload`), их `Uploaded` — в `outgoing.attachments`, затем
    /// `Request::Send`.
    Chat { outgoing: Outgoing, uploads: Vec<Upload> },
    /// В почту: черновик в окно письма.
    Mail(Box<Draft>),
}

/// Почему не отправить: показывается строкой с причиной, отправки нет.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// Текст длиннее предела цели, знаков.
    TooLong { max: u32, len: usize },
    /// Вложение больше предела цели, байт.
    TooLarge { name: String, max: u64, size: u64 },
    /// Вложения не загружены у источника: сначала загрузить (`Request::Download`, у почты — `Command::Blob`).
    NotLoaded(Vec<(Address, Attachment)>),
    /// Нечего пересылать.
    Empty,
}

/// Строка «Переслано от …» перед телом источника: текст даёт интерфейс (имя, дата — по `Picked`).
pub type Header<'a> = &'a dyn Fn(&Picked) -> String;

fn paragraph(content: Vec<Inline>) -> Block {
    Block::Paragraph { align: Align::Start, content }
}

/// Тело источника в IR.
fn body_doc(m: &Message) -> Document {
    match &m.body {
        Body::Ir(d) => d.clone(),
        Body::Html { plain, .. } | Body::Plain(plain) => text_doc(plain),
    }
}

/// Пределы цели и загруженность вложений.
fn check(files: &[(&Picked, &File)], text: &Document, to: &Address, features: &ChannelFeatures) -> std::result::Result<(), Refused> {
    let missing: Vec<(Address, Attachment)> =
        files.iter().filter(|(_, f)| f.bytes == Bytes::Missing).map(|(p, f)| (p.at.clone(), f.attachment.clone())).collect();
    if !missing.is_empty() {
        return Err(Refused::NotLoaded(missing));
    }
    if to.channel.0 != super::MAIL
        && let Some(max) = features.max_text
    {
        let len = super::doc_text(text).chars().count();
        if len > max as usize {
            return Err(Refused::TooLong { max, len });
        }
    }
    if let Some(max) = features.max_attachment
        && let Some((_, f)) = files.iter().find(|(_, f)| f.attachment.size.unwrap_or(0) > max)
    {
        return Err(Refused::TooLarge { name: f.attachment.name.clone(), max, size: f.attachment.size.unwrap_or(0) });
    }
    Ok(())
}

fn uploads(files: &[(&Picked, &File)]) -> Vec<Upload> {
    files
        .iter()
        .map(|(_, f)| Upload {
            meta: FileMeta { name: f.attachment.name.clone(), mime: f.attachment.mime.clone(), size: f.attachment.size.unwrap_or(0) },
            bytes: f.bytes.clone(),
        })
        .collect()
}

/// Вложения черновика письма: байты из хранилища.
fn mail_files(cache: &Cache, files: &[(&Picked, &File)]) -> Result<std::result::Result<Vec<MailFile>, Refused>> {
    let mut out = Vec::with_capacity(files.len());
    let mut missing = Vec::new();
    for (p, f) in files {
        match bytes(cache, &f.bytes)? {
            Some(data) => {
                out.push(MailFile { name: f.attachment.name.clone(), mime: f.attachment.mime.clone(), data, inline_cid: None, blob_id: None })
            }
            None => missing.push((p.at.clone(), f.attachment.clone())),
        }
    }
    Ok(if missing.is_empty() { Ok(out) } else { Err(Refused::NotLoaded(missing)) })
}

/// Тема пересылаемого письма: «Fwd: » и название беседы источника.
fn forward_subject(title: &str) -> String {
    if title.trim().is_empty() { String::new() } else { format!("Fwd: {}", title.trim()) }
}

/// Переслать сообщения `picked` (по порядку) в беседу `to` (адрес беседы цели; `features` — её канала). `client_id` —
/// номер отправки в чат.
pub fn forward(
    cache: &Cache,
    picked: &[Picked],
    to: &Address,
    features: &ChannelFeatures,
    header: Header,
    client_id: u64,
) -> Result<std::result::Result<Plan, Refused>> {
    let Some(first) = picked.first() else { return Ok(Err(Refused::Empty)) };
    let same_conv = picked.iter().all(|p| p.at.of_conversation() == first.at.of_conversation());
    if to.channel.0 != super::MAIL && features.forward_native && same_conv && first.at.chan() == to.chan() {
        let ids = picked.iter().filter_map(|p| p.at.message.clone()).collect();
        return Ok(Ok(Plan::Native(Action::Forward { from: first.at.conversation.clone(), ids, to: to.conversation.clone() })));
    }
    let mut doc = Document::default();
    for p in picked {
        doc.blocks.push(paragraph(vec![Inline::text(header(p))]));
        doc.blocks.push(Block::Quote(body_doc(&p.message).blocks));
    }
    let files: Vec<(&Picked, &File)> = picked.iter().flat_map(|p| p.files.iter().map(move |f| (p, f))).collect();
    if let Err(r) = check(&files, &doc, to, features) {
        return Ok(Err(r));
    }
    if to.channel.0 == super::MAIL {
        let attachments = match mail_files(cache, &files)? {
            Ok(a) => a,
            Err(r) => return Ok(Err(r)),
        };
        let draft = Draft { subject: forward_subject(&first.title), markdown: postman_ir::to_markdown(&doc), attachments, ..Draft::default() };
        return Ok(Ok(Plan::Mail(Box::new(draft))));
    }
    let outgoing = Outgoing {
        conversation: to.conversation.clone(),
        client_id,
        body: doc,
        attachments: Vec::new(),
        reply_to: None,
        quotes: Vec::new(),
        mentions: Vec::new(),
        at: None,
        silent: false,
    };
    Ok(Ok(Plan::Chat { outgoing, uploads: uploads(&files) }))
}

/// «Ответить в…» беседу `to`: цитата `fragment` (нет — начало текста источника) и ссылка на источник, затем `text`
/// пользователя. В ту же беседу канала с `quote_fragment` — ответом сервера (`reply_to`, `quotes`), без ссылки.
pub fn reply_in(
    links: &Links,
    picked: &Picked,
    fragment: Option<&str>,
    text: &Document,
    to: &Address,
    features: &ChannelFeatures,
    client_id: u64,
) -> std::result::Result<Plan, Refused> {
    let quote = fragment.map(str::to_owned).unwrap_or_else(|| super::source::preview(&picked.message.body.plain()));
    let native = to.channel.0 != super::MAIL && picked.at.of_conversation() == to.of_conversation();
    let mut doc = Document::default();
    if !native {
        let link = links.outside(&picked.at);
        doc.blocks.push(Block::Quote(text_doc(&quote).blocks));
        let href =
            Inline::Link { href: link.clone(), host: None, suspicious: false, content: vec![Inline::text(format!("{} — {link}", picked.author))] };
        doc.blocks.push(paragraph(vec![href]));
    }
    doc.blocks.extend(text.blocks.iter().cloned());
    check(&[], &doc, to, features)?;
    if to.channel.0 == super::MAIL {
        let subject = if picked.title.trim().is_empty() { String::new() } else { format!("Re: {}", picked.title.trim()) };
        return Ok(Plan::Mail(Box::new(Draft { subject, markdown: postman_ir::to_markdown(&doc), ..Draft::default() })));
    }
    let (reply_to, quotes) = match (&picked.at.message, native) {
        (Some(id), true) if features.reply_to => {
            let quotes =
                if features.quote_fragment && fragment.is_some() { vec![Quote { text: quote, source: Some(id.clone()) }] } else { Vec::new() };
            (Some(id.clone()), quotes)
        }
        _ => (None, Vec::new()),
    };
    Ok(Plan::Chat {
        outgoing: Outgoing {
            conversation: to.conversation.clone(),
            client_id,
            body: doc,
            attachments: Vec::new(),
            reply_to,
            quotes,
            mentions: Vec::new(),
            at: None,
            silent: false,
        },
        uploads: Vec::new(),
    })
}

/// Беседа-цель: может ли пользователь в неё писать (роль), по адресу — у чатов; почта — всегда.
pub fn can_write(cache: &Cache, to: &Address) -> Result<bool> {
    if to.channel.0 == super::MAIL {
        return Ok(true);
    }
    let Some(store) = ChannelStore::find(cache, &to.channel, &to.account)? else { return Ok(false) };
    Ok(store.conversation(&to.conversation)?.is_some_and(|c| c.conversation.my_role.can_write()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{AccountId, AttachmentSource, ChannelEvent, ChannelId, Conversation, ConversationId, MessageId, ParticipantId, Role};

    fn setup() -> (Cache, ChannelStore, Address) {
        let cache = Cache::open_in_memory().unwrap();
        let store = ChannelStore::open(cache.clone(), &ChannelId::new("demo"), &AccountId::new("me@example.test"), "Я").unwrap();
        let file = Attachment {
            name: "plan.pdf".into(),
            mime: "application/pdf".into(),
            size: Some(5),
            source: AttachmentSource(b"src-1".to_vec()),
            ..Attachment::default()
        };
        let m = Message {
            id: MessageId::new("1"),
            author: ParticipantId::new("u-anna"),
            sent: 1_000,
            body: Body::Plain("План на неделю".into()),
            attachments: vec![file.clone()],
            ..Message::default()
        };
        store
            .apply(vec![
                ChannelEvent::People(vec![crate::channel::Participant {
                    id: ParticipantId::new("u-anna"),
                    name: "Анна".into(),
                    ..Default::default()
                }]),
                ChannelEvent::Conversations(vec![
                    Conversation { id: ConversationId::new("g"), title: "Команда".into(), ..Conversation::default() },
                    Conversation {
                        id: ConversationId::new("news"), title: "Новости".into(), my_role: Role::ReadOnly, ..Conversation::default()
                    },
                ]),
                ChannelEvent::Messages { conversation: ConversationId::new("g"), messages: vec![m], live: false },
            ])
            .unwrap();
        let at = Address::message(store.channel(), store.account(), &ConversationId::new("g"), &MessageId::new("1"));
        (cache, store, at)
    }

    fn header(p: &Picked) -> String {
        format!("Переслано от {}", p.author)
    }

    /// Чат → почта: вложение не загружено — сначала загрузка; загружено — черновик с байтами из хранилища без
    /// повторной загрузки; чат → чат: выгрузка по хэшу и очередь; пределы цели — до отправки; своя учётка с
    /// `forward_native` — сервером.
    #[test]
    fn forward_between_channels() {
        let (cache, store, at) = setup();
        let mail_to = Address::conversation(&ChannelId::new("mail"), &AccountId::new("me@example.test"), &ConversationId::new("new"));
        let p = pick(&cache, None, &at).unwrap().unwrap();
        assert_eq!((p.author.as_str(), p.title.as_str()), ("Анна", "Команда"));
        let got = forward(&cache, std::slice::from_ref(&p), &mail_to, &ChannelFeatures::default(), &header, 1).unwrap();
        assert!(matches!(got, Err(Refused::NotLoaded(ref v)) if v.len() == 1));

        let mut d = store.begin_download();
        crate::channel::ByteSink::write(&mut d, b"%PDF-").unwrap();
        let h = store
            .finish_download(d, &at.conversation, at.message.as_ref().unwrap(), &AttachmentSource(b"src-1".to_vec()), "application/pdf")
            .unwrap()
            .unwrap();
        let p = pick(&cache, None, &at).unwrap().unwrap();
        assert_eq!(p.files[0].bytes, Bytes::Part(h));
        let Ok(Plan::Mail(draft)) = forward(&cache, std::slice::from_ref(&p), &mail_to, &ChannelFeatures::default(), &header, 1).unwrap() else {
            panic!()
        };
        assert_eq!(draft.subject, "Fwd: Команда");
        assert!(draft.markdown.contains("Переслано от Анна") && draft.markdown.contains("План на неделю"), "{}", draft.markdown);
        assert_eq!(draft.attachments[0].data, b"%PDF-");

        let other = Address::conversation(&ChannelId::new("telegram"), &AccountId::new("1"), &ConversationId::new("c"));
        let Ok(Plan::Chat { outgoing, uploads }) =
            forward(&cache, std::slice::from_ref(&p), &other, &ChannelFeatures::default(), &header, 7).unwrap()
        else {
            panic!()
        };
        assert_eq!((outgoing.client_id, outgoing.conversation.0.as_str()), (7, "c"));
        assert_eq!(bytes(&cache, &uploads[0].bytes).unwrap().as_deref(), Some(b"%PDF-".as_slice()));
        let small = ChannelFeatures { max_attachment: Some(4), ..ChannelFeatures::default() };
        assert!(matches!(forward(&cache, std::slice::from_ref(&p), &other, &small, &header, 7).unwrap(), Err(Refused::TooLarge { .. })));
        let short = ChannelFeatures { max_text: Some(5), ..ChannelFeatures::default() };
        assert!(matches!(forward(&cache, std::slice::from_ref(&p), &other, &short, &header, 7).unwrap(), Err(Refused::TooLong { max: 5, .. })));
        let native = ChannelFeatures { forward_native: true, ..ChannelFeatures::default() };
        let same = Address::conversation(store.channel(), store.account(), &ConversationId::new("news"));
        assert!(matches!(forward(&cache, &[p], &same, &native, &header, 7).unwrap(), Ok(Plan::Native(Action::Forward { .. }))));
        assert!(!can_write(&cache, &same).unwrap() && can_write(&cache, &at.of_conversation()).unwrap());
    }

    /// «Ответить в…»: в другую беседу — цитата и ссылка канала на источник; в ту же беседу — ответом сервера.
    #[test]
    fn reply_in_other_conversation() {
        let (cache, _store, at) = setup();
        let p = pick(&cache, None, &at).unwrap().unwrap();
        let links = Links::default();
        let text = text_doc("Согласен");
        let mail_to = Address::conversation(&ChannelId::new("mail"), &AccountId::new("me@example.test"), &ConversationId::new("t"));
        let Ok(Plan::Mail(d)) = reply_in(&links, &p, Some("на неделю"), &text, &mail_to, &ChannelFeatures::default(), 1) else { panic!() };
        assert!(d.markdown.contains("> на неделю") && d.markdown.contains(&at.to_string()) && d.markdown.contains("Согласен"), "{}", d.markdown);
        let f = ChannelFeatures { reply_to: true, quote_fragment: true, ..ChannelFeatures::default() };
        let Ok(Plan::Chat { outgoing, .. }) = reply_in(&links, &p, Some("на неделю"), &text, &at.of_conversation(), &f, 2) else { panic!() };
        assert_eq!(outgoing.reply_to, at.message);
        assert_eq!(outgoing.quotes[0].text, "на неделю");
        assert_eq!(crate::channel::doc_text(&outgoing.body), "Согласен");
    }

    /// Письмо → чат: вложение письма — из кэша блобов, без него — сначала загрузка у почты.
    #[test]
    fn letter_to_chat() {
        use crate::channel::mail::MailAccount;
        use postman_cache::{AttachmentRow, BodyRow, EmailRow};
        let cache = Cache::open_in_memory().unwrap();
        let row = EmailRow {
            id: "e1".into(),
            thread_id: "t1".into(),
            mailbox_ids: vec!["in".into()],
            received_at: 100,
            subject: "Отчёт".into(),
            from_name: "Борис".into(),
            from_addr: "boris@example.test".into(),
            preview: String::new(),
            seen: true,
            flagged: false,
            has_attachment: true,
            size: 0,
            keywords: Vec::new(),
        };
        cache.upsert_emails(&[row]).unwrap();
        let file = AttachmentRow { blob_id: "b1".into(), name: "a.txt".into(), mime: "text/plain".into(), size: 3, cid: None, inline: false };
        cache
            .put_body(
                "e1",
                &BodyRow {
                    message_id: Some("<m1@example.test>".into()),
                    text: Some("Отчёт готов".into()),
                    attachments: vec![file],
                    ..BodyRow::default()
                },
            )
            .unwrap();
        let mail =
            MailSource::new(cache.clone(), vec![MailAccount::new(None, "me@example.test")], crate::filters::Me::new(["me@example.test"], ["Я"]));
        let at = Address::message(
            &ChannelId::new("mail"),
            &AccountId::new("me@example.test"),
            &ConversationId::new("t1"),
            &MessageId::new("m1@example.test"),
        );
        let to = Address::conversation(&ChannelId::new("demo"), &AccountId::new("me@example.test"), &ConversationId::new("g"));
        let p = pick(&cache, Some(&mail), &at).unwrap().unwrap();
        assert_eq!((p.author.as_str(), p.files[0].bytes.clone()), ("Борис", Bytes::Missing));
        assert!(matches!(
            forward(&cache, std::slice::from_ref(&p), &to, &ChannelFeatures::default(), &header, 1).unwrap(),
            Err(Refused::NotLoaded(_))
        ));
        cache.put_blob("b1", b"abc").unwrap();
        let p = pick(&cache, Some(&mail), &at).unwrap().unwrap();
        let Ok(Plan::Chat { uploads, outgoing }) = forward(&cache, &[p], &to, &ChannelFeatures::default(), &header, 1).unwrap() else { panic!() };
        assert_eq!(bytes(&cache, &uploads[0].bytes).unwrap().as_deref(), Some(b"abc".as_slice()));
        assert!(crate::channel::doc_text(&outgoing.body).contains("Отчёт готов"));
    }
}
