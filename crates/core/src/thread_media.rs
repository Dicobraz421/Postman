//! Медиа беседы (MED-01, `docs/decisions/2026-11-13-thread-media.md`): как «Общие медиа» в Telegram — в одной беседе
//! собраны файлы, картинки и видео, ссылки, участники и визитки, встречи; по ним есть поиск.
//!
//! [`collect`] и [`filter`] — чистые функции: на входе строки писем беседы, их тела и IR (разбор тела — в воркере), а
//! также сведения о вложениях ([`Facts`]: хэши байтов, размеры кадров, разобранные воркером приглашения и визитки).
//! Чтение хранилища и воркер — [`gather`], вне потока интерфейса (фоновая задача). Тела, которых ещё нет, —
//! [`fetch`]: команды синхронизации, как при открытии письма (`Command::Body`).
//!
//! Правила отбора:
//! - **Файлы** — все вложения, кроме картинок в тексте и подписей S/MIME (`smime.p7s`); одинаковые
//!   ([`crate::samefile`]) — одна строка со всеми письмами, где они были.
//! - **Медиа** — картинки и видео, вложением и в тексте, кроме мелких и подписей ([`is_tiny`], [`signatures`]).
//! - **Ссылки** — `http`/`https` из нового текста писем ([`Document::new_text`], без истории и блоков цитаты);
//!   `mailto:`, `cid:`, `javascript:` и прочие схемы отбрасываются; один адрес — одна строка.
//! - **Контакты** — «От», «Кому», «Копия» всех писем без повторов (адрес без регистра) и визитки vCard.
//! - **Встречи** — приглашения `.ics` и iMIP (`text/calendar`); одна встреча (UID) — одна строка, по новейшей версии.
//!
//! **Любой канал** (ADR 0012 п. 11, CH-02в): сообщения бесед каналов — тем же входом ([`chat_letters`]: сообщение —
//! «письмо» с адресом `postman://…` вместо ключа, вложения — по хэшам загруженных частей), правила отбора те же.
//! Связанные беседы (`channel::linked`) — вместе ([`load_group`]); строка показывает канал ([`Origin::channel`]).

use std::collections::{HashMap, HashSet};

use jiff::tz::TimeZone;
use postman_cache::{AttachmentRow, BodyRow, Cache, EmailRow, Hash};
use postman_calendar::{EventStatus, Invitation, Partstat, Span};
use postman_ir::{Block, Document, Inline};
use postman_worker::Client;

use crate::attach::{self, Viewer};
use crate::channel::mail::MailSource;
use crate::channel::{Address, AttachmentKind, Body, ChannelError, ChannelStore, Chats, ConversationSource, Line, Participant};
use crate::invite::Seen;
use crate::model::ContactCard;
use crate::samefile::{self, FileKey, Groups};
use crate::sync::Command;

/// Картинка меньше этого по обеим сторонам — пиксель отслеживания или значок, не медиа.
pub const MIN_SIDE: u32 = 32;
/// Байты картинки до этого размера [`gather`] показывает декодеру воркера, чтобы узнать кадр (пиксели отслеживания
/// и значки маленькие); крупнее — кадр из разметки письма или неизвестен.
pub const PROBE_BYTES: usize = 16 * 1024;
/// Сторона кадра при пробе: мельче [`MIN_SIDE`] картинка остаётся мельче и после уменьшения.
const PROBE_SIDE: u32 = 256;
/// Расширения видео (MIME `video/*` — тоже видео).
const VIDEO_EXT: &[&str] = &["mp4", "m4v", "mov", "webm", "avi", "mkv", "wmv", "3gp"];
/// Ключ ответа на приглашение в хранилище состояния (`invite_card::state_key` интерфейса).
const ANSWER_KEY: &str = "invite:";

/// Письмо беседы: строка, тело (`None` — ещё не скачано) и его IR (`None` — не разобрано).
#[derive(Debug, Clone)]
pub struct Letter {
    pub row: EmailRow,
    pub body: Option<BodyRow>,
    pub doc: Option<Document>,
}

/// Сведения о вложениях кроме строк тела, по блобу; чего нет — того не знаем.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    /// SHA-256 раскодированных байтов (байты в хранилище).
    pub hashes: HashMap<String, Hash>,
    /// Кадр картинки (ширина, высота) — от декодера воркера.
    pub frames: HashMap<String, (u32, u32)>,
    /// Приглашения, разобранные воркером.
    pub invites: HashMap<String, Invitation>,
    /// Карточки визиток vCard, разобранные воркером.
    pub cards: HashMap<String, Vec<ContactCard>>,
    /// UID встречи → мой ответ (запомненный при ответе).
    pub answers: HashMap<String, Partstat>,
}

/// Где встретилось: письмо (сообщение канала) и его автор.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    /// Ключ письма в кэше («Перейти к письму»); у сообщения канала — его адрес `postman://…`.
    pub letter: String,
    pub from_name: String,
    pub from_addr: String,
    /// Unix-время письма, секунды.
    pub at: i64,
    /// Канал: `mail` у письма, иначе канал сообщения (связанные беседы — вместе, строка показывает канал).
    pub channel: String,
}

impl Origin {
    fn of(row: &EmailRow) -> Self {
        let channel = Address::parse(&row.id).map_or_else(|| crate::channel::MAIL.to_owned(), |a| a.channel.0);
        Self { letter: row.id.clone(), from_name: row.from_name.clone(), from_addr: row.from_addr.clone(), at: row.received_at, channel }
    }
}

/// Одна копия вложения: в каком письме и под каким блобом и именем («Открыть», «Сохранить», «Переслать»).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    pub origin: Origin,
    pub blob_id: String,
    pub name: String,
}

/// Строка «Файлы»: одинаковые вложения — одна строка, копии — по времени (старые первыми).
#[derive(Debug, Clone, PartialEq)]
pub struct File {
    pub key: FileKey,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub copies: Vec<Occurrence>,
}

impl File {
    /// Сколько раз прислан.
    pub fn count(&self) -> usize {
        self.copies.len()
    }

    /// Первая копия: кто прислал и когда.
    pub fn first(&self) -> Option<&Occurrence> {
        self.copies.first()
    }

    fn last_at(&self) -> i64 {
        self.copies.last().map_or(i64::MIN, |c| c.origin.at)
    }
}

/// Строка «Медиа»: картинка или видео.
#[derive(Debug, Clone, PartialEq)]
pub struct Media {
    pub file: File,
    pub video: bool,
    /// Картинка в тексте письма (не вложением).
    pub inline: bool,
    /// Кадр, если известен.
    pub frame: Option<(u32, u32)>,
}

/// Чем ссылка помечена (дешёвые признаки по адресу и тексту; ссылка остаётся в списке).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    Plain,
    /// Отписка от рассылки.
    Unsubscribe,
    /// Через счётчик переходов (`utm_*`, `click.`/`track.`).
    Tracking,
}

/// Строка «Ссылки»: один адрес — одна строка.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    /// Нормализованный адрес (IR).
    pub url: String,
    /// Сервер без `www.`.
    pub domain: String,
    /// Текст ссылки (первый непустой).
    pub text: String,
    pub kind: LinkKind,
    /// Текст похож на адрес другого сервера (IR) — правила безопасных ссылок.
    pub suspicious: bool,
    /// Письма, где ссылка в новом тексте, по времени.
    pub seen: Vec<Origin>,
}

/// Участник беседы.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub name: String,
    pub email: String,
    /// В скольких письмах — автором или получателем.
    pub letters: usize,
    /// Сколько писем написал.
    pub sent: usize,
    /// Это я (`me` у [`collect`]): «Написать» и «Добавить в контакты» не нужны.
    pub me: bool,
}

/// Визитка vCard во вложении; одинаковые — одна строка.
#[derive(Debug, Clone, PartialEq)]
pub struct CardFile {
    pub file: File,
    /// Карточки, разобранные воркером; пусто — не разобрана (показывается имя файла).
    pub cards: Vec<ContactCard>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Contacts {
    pub people: Vec<Person>,
    pub cards: Vec<CardFile>,
}

/// Строка «Встречи».
#[derive(Debug, Clone, PartialEq)]
pub struct Meeting {
    /// `None` — приглашение не разобрано: показываются имя файла и письмо.
    pub uid: Option<String>,
    /// Тема встречи; не разобрано — имя файла.
    pub title: String,
    pub span: Option<Span>,
    pub all_day: bool,
    pub location: String,
    /// REQUEST, CANCEL, REPLY…
    pub method: Option<String>,
    pub cancelled: bool,
    /// Мой ответ, если он известен.
    pub status: Option<Partstat>,
    /// Файл новейшей версии: «Открыть приглашение».
    pub file: File,
}

/// Медиа беседы.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ThreadMedia {
    pub files: Vec<File>,
    pub media: Vec<Media>,
    pub links: Vec<Link>,
    pub contacts: Contacts,
    pub meetings: Vec<Meeting>,
    /// Писем с телом.
    pub loaded: usize,
    /// Писем в беседе.
    pub total: usize,
}

/// Счётчики вкладок.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub files: usize,
    pub media: usize,
    pub links: usize,
    pub contacts: usize,
    pub meetings: usize,
}

impl ThreadMedia {
    /// Все тела загружены: строки «загружено N из M» нет.
    pub fn complete(&self) -> bool {
        self.loaded >= self.total
    }

    pub fn counts(&self) -> Counts {
        Counts {
            files: self.files.len(),
            media: self.media.len(),
            links: self.links.len(),
            contacts: self.contacts.people.len() + self.contacts.cards.len(),
            meetings: self.meetings.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.counts() == Counts::default()
    }
}

/// MIME без параметров, в нижнем регистре.
fn mime_of(a: &AttachmentRow) -> String {
    a.mime.split(';').next().unwrap_or_default().trim().to_ascii_lowercase()
}

fn extension(name: &str) -> String {
    name.rsplit_once('.').map(|(_, e)| e.trim().to_ascii_lowercase()).unwrap_or_default()
}

fn is_image(a: &AttachmentRow) -> bool {
    mime_of(a).starts_with("image/") || attach::viewer(&a.name, &a.mime) == Viewer::Image
}

fn is_video(a: &AttachmentRow) -> bool {
    mime_of(a).starts_with("video/") || VIDEO_EXT.contains(&extension(&a.name).as_str())
}

fn is_calendar(a: &AttachmentRow) -> bool {
    matches!(mime_of(a).as_str(), "text/calendar" | "application/ics") || extension(&a.name) == "ics"
}

fn is_vcard(a: &AttachmentRow) -> bool {
    matches!(mime_of(a).as_str(), "text/vcard" | "text/x-vcard" | "text/directory") || matches!(extension(&a.name).as_str(), "vcf" | "vcard")
}

/// Подпись S/MIME — служебная часть письма, не файл.
fn is_smime_signature(a: &AttachmentRow) -> bool {
    matches!(mime_of(a).as_str(), "application/pkcs7-signature" | "application/x-pkcs7-signature")
}

/// Вложение — в «Файлы»: всё, кроме картинок в тексте и подписей S/MIME.
fn is_file(a: &AttachmentRow) -> bool {
    !(a.inline && is_image(a)) && !is_smime_signature(a)
}

/// Мельче [`MIN_SIDE`] по обеим сторонам: пиксель отслеживания, значок.
pub fn is_tiny(frame: (u32, u32)) -> bool {
    frame.0 < MIN_SIDE && frame.1 < MIN_SIDE
}

/// Письма беседы с телами, по времени.
fn loaded(letters: &[Letter]) -> Vec<(&EmailRow, &BodyRow, Option<&Document>)> {
    let mut v: Vec<_> = letters.iter().filter_map(|l| l.body.as_ref().map(|b| (&l.row, b, l.doc.as_ref()))).collect();
    v.sort_by(|a, b| (a.0.received_at, &a.0.id).cmp(&(b.0.received_at, &b.0.id)));
    v
}

/// Сводка вложений `pick` в строки: одинаковые ([`crate::samefile`]) — одной строкой; новые — последними по времени.
fn files_of(letters: &[(&EmailRow, &BodyRow, Option<&Document>)], facts: &Facts, pick: impl Fn(&AttachmentRow) -> bool) -> Vec<File> {
    let mut groups = Groups::default();
    let mut out: Vec<File> = Vec::new();
    for (row, body, _) in letters {
        for a in body.attachments.iter().filter(|a| pick(a)) {
            let hash = facts.hashes.get(&a.blob_id).copied();
            let (g, new) = groups.add(a, hash);
            if new {
                out.push(File { key: FileKey::Blob(String::new()), name: a.name.clone(), mime: a.mime.clone(), size: a.size, copies: Vec::new() });
            }
            if let Some(f) = out.get_mut(g) {
                f.copies.push(Occurrence { origin: Origin::of(row), blob_id: a.blob_id.clone(), name: a.name.clone() });
            }
        }
    }
    for (g, f) in out.iter_mut().enumerate() {
        let first = AttachmentRow {
            blob_id: f.first().map(|c| c.blob_id.clone()).unwrap_or_default(),
            name: f.name.clone(),
            size: f.size,
            ..Default::default()
        };
        f.key = groups.key(g, &first);
    }
    out.sort_by_key(|f| std::cmp::Reverse(f.last_at()));
    out
}

/// Кадры картинок в тексте по `cid:` из разметки письма (`width`/`height` у `<img>`).
fn frames_in(doc: &Document) -> HashMap<String, (u32, u32)> {
    fn inl(items: &[Inline], out: &mut HashMap<String, (u32, u32)>) {
        for i in items {
            match i {
                Inline::Image { cid, width: Some(w), height: Some(h), .. } => {
                    out.insert(cid.trim_matches(['<', '>']).to_ascii_lowercase(), (*w, *h));
                }
                Inline::Link { content, .. } => inl(content, out),
                _ => {}
            }
        }
    }
    let mut out = HashMap::new();
    walk(&doc.blocks, true, &mut |items| inl(items, &mut out));
    out
}

/// Инлайны блоков по порядку чтения; `quotes` — заходить ли в блоки цитаты.
fn walk(blocks: &[Block], quotes: bool, f: &mut impl FnMut(&[Inline])) {
    for b in blocks {
        match b {
            Block::Paragraph { content, .. } | Block::Heading { content, .. } => f(content),
            Block::Quote(inner) if quotes => walk(inner, quotes, f),
            Block::List { items, .. } => items.iter().for_each(|i| walk(i, quotes, f)),
            Block::Table(rows) => rows.iter().flatten().for_each(|c| f(c)),
            Block::Quote(_) | Block::Code(_) | Block::Rule | Block::Figure => {}
        }
    }
}

/// Кадр картинки: от декодера ([`Facts::frames`]) или из разметки письма по `cid:`.
fn frame_of(a: &AttachmentRow, facts: &Facts, marked: &HashMap<String, (u32, u32)>) -> Option<(u32, u32)> {
    let cid = || a.cid.as_deref().map(|c| c.trim_matches(['<', '>']).to_ascii_lowercase());
    facts.frames.get(&a.blob_id).copied().or_else(|| cid().and_then(|c| marked.get(&c).copied()))
}

/// Картинки подписи: одна и та же картинка (по [`crate::samefile`]) в тексте нескольких писем, если эти письма —
/// разных авторов или все письма одного автора в беседе (у него их больше одного). Возвращает блобы таких копий.
///
/// Картинка, которую один автор вставил в текст лишь части своих писем, остаётся медиа: это скорее присланная
/// картинка, чем подпись. Картинка подписи, приложенная файлом (не в тексте), тоже остаётся.
pub fn signatures(letters: &[(&EmailRow, &BodyRow, Option<&Document>)], facts: &Facts) -> HashSet<String> {
    let mut by_author: HashMap<String, usize> = HashMap::new();
    for (row, ..) in letters {
        *by_author.entry(row.from_addr.to_lowercase()).or_default() += 1;
    }
    let mut groups = Groups::default();
    // Группа → (письма, авторы, блобы копий).
    let mut seen: Vec<(HashSet<&str>, HashSet<String>, Vec<String>)> = Vec::new();
    for (row, body, _) in letters {
        for a in body.attachments.iter().filter(|a| a.inline && is_image(a)) {
            let (g, new) = groups.add(a, facts.hashes.get(&a.blob_id).copied());
            if new {
                seen.push(Default::default());
            }
            if let Some((ls, authors, blobs)) = seen.get_mut(g) {
                ls.insert(row.id.as_str());
                authors.insert(row.from_addr.to_lowercase());
                blobs.push(a.blob_id.clone());
            }
        }
    }
    seen.into_iter()
        .filter(|(ls, authors, _)| {
            ls.len() >= 2 && (authors.len() >= 2 || authors.iter().next().is_some_and(|au| by_author.get(au).copied() == Some(ls.len())))
        })
        .flat_map(|(.., blobs)| blobs)
        .collect()
}

fn media_of(letters: &[(&EmailRow, &BodyRow, Option<&Document>)], facts: &Facts) -> Vec<Media> {
    let sign = signatures(letters, facts);
    let marked: HashMap<String, (u32, u32)> = letters.iter().filter_map(|(.., d)| *d).flat_map(frames_in).collect();
    let mut frames: HashMap<&str, (u32, u32)> = HashMap::new();
    let mut inline: HashSet<&str> = HashSet::new();
    let mut tiny: HashSet<&str> = HashSet::new();
    for (_, body, _) in letters {
        for a in &body.attachments {
            if let Some(f) = frame_of(a, facts, &marked) {
                frames.insert(&a.blob_id, f);
                if is_tiny(f) {
                    tiny.insert(&a.blob_id);
                }
            }
            if a.inline {
                inline.insert(&a.blob_id);
            }
        }
    }
    let pick = |a: &AttachmentRow| (is_image(a) || is_video(a)) && !tiny.contains(a.blob_id.as_str()) && !(a.inline && sign.contains(&a.blob_id));
    files_of(letters, facts, pick)
        .into_iter()
        .map(|file| {
            let blobs = || file.copies.iter().map(|c| c.blob_id.as_str());
            let frame = blobs().find_map(|b| frames.get(b).copied());
            let is_inline = blobs().all(|b| inline.contains(b));
            let video = is_video(&AttachmentRow { name: file.name.clone(), mime: file.mime.clone(), ..Default::default() });
            Media { video, inline: is_inline, frame, file }
        })
        .collect()
}

/// Адрес, годный для списка ссылок: только `http`/`https` с сервером (`mailto:`, `cid:`, `javascript:`, `data:`,
/// `file:` и прочие — нет). Ответ — нормализованный адрес и сервер.
fn web_link(href: &str) -> Option<(String, String)> {
    let check = postman_ir::check_link(href, "")?;
    let host = check.host.filter(|h| !h.is_empty())?;
    let lower = check.href.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://")).then_some((check.href, host))
}

/// Дешёвые признаки отписки и счётчика переходов по адресу и тексту.
pub fn link_kind(url: &str, text: &str) -> LinkKind {
    let (u, t) = (url.to_lowercase(), text.to_lowercase());
    const UNSUB: &[&str] = &["unsubscribe", "optout", "opt-out", "/unsub", "отписат", "отписк", "отказаться от рассылки"];
    if UNSUB.iter().any(|w| u.contains(w) || t.contains(w)) {
        return LinkKind::Unsubscribe;
    }
    let after_scheme = u.split_once("://").map_or(u.as_str(), |(_, r)| r);
    let host = after_scheme.split(['/', '?', '#']).next().unwrap_or_default();
    let first = host.split('.').next().unwrap_or_default();
    let query = u.split_once('?').map_or("", |(_, q)| q);
    let tracking = matches!(first, "click" | "clicks" | "track" | "tracking" | "trk" | "links")
        || after_scheme.contains("/click")
        || after_scheme.contains("/track")
        || query.split('&').any(|p| p.starts_with("utm_"));
    if tracking { LinkKind::Tracking } else { LinkKind::Plain }
}

fn links_of(letters: &[(&EmailRow, &BodyRow, Option<&Document>)]) -> Vec<Link> {
    let mut out: Vec<Link> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for (row, _, doc) in letters {
        let Some(doc) = doc else { continue };
        let mut here: HashSet<String> = HashSet::new();
        walk(doc.new_text(), false, &mut |items| {
            for i in items {
                let Inline::Link { href, suspicious, content, .. } = i else { continue };
                let Some((url, host)) = web_link(href) else { continue };
                let text = postman_ir::plain_text(content).trim().to_string();
                let n = *index.entry(url.clone()).or_insert_with(|| {
                    let domain = host.strip_prefix("www.").unwrap_or(&host).to_string();
                    out.push(Link {
                        kind: link_kind(&url, &text),
                        url: url.clone(),
                        domain,
                        text: String::new(),
                        suspicious: false,
                        seen: Vec::new(),
                    });
                    out.len() - 1
                });
                if let Some(l) = out.get_mut(n) {
                    if l.text.is_empty() {
                        l.text = text;
                    }
                    l.suspicious |= *suspicious;
                    if here.insert(url) {
                        l.seen.push(Origin::of(row));
                    }
                }
            }
        });
    }
    out.sort_by_key(|l| std::cmp::Reverse(l.seen.last().map_or(i64::MIN, |o| o.at)));
    out
}

fn people_of(letters: &[Letter], me: Option<&str>) -> Vec<Person> {
    let me = me.map(str::to_lowercase);
    let mut out: Vec<Person> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for l in letters {
        let mut here: HashSet<String> = HashSet::new();
        let from = std::iter::once((l.row.from_name.as_str(), l.row.from_addr.as_str(), true));
        let rcpt = l.body.iter().flat_map(|b| b.to.iter().chain(&b.cc)).map(|a| (a.name.as_str(), a.email.as_str(), false));
        for (name, email, sender) in from.chain(rcpt) {
            let key = email.trim().to_lowercase();
            if key.is_empty() {
                continue;
            }
            let n = *index.entry(key.clone()).or_insert_with(|| {
                out.push(Person {
                    name: String::new(),
                    email: email.trim().to_string(),
                    letters: 0,
                    sent: 0,
                    me: me.as_deref() == Some(key.as_str()),
                });
                out.len() - 1
            });
            if let Some(p) = out.get_mut(n) {
                if p.name.is_empty() {
                    p.name = name.trim().to_string();
                }
                if here.insert(key) {
                    p.letters += 1;
                }
                if sender {
                    p.sent += 1;
                }
            }
        }
    }
    out.sort_by(|a, b| b.letters.cmp(&a.letters).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    out
}

fn meetings_of(letters: &[(&EmailRow, &BodyRow, Option<&Document>)], facts: &Facts, me: Option<&str>) -> Vec<Meeting> {
    let files = files_of(letters, facts, is_calendar);
    let mut out: Vec<Meeting> = Vec::new();
    let mut by_uid: HashMap<String, usize> = HashMap::new();
    for file in files {
        // Новейшая разобранная версия среди копий.
        let parsed = file.copies.iter().rev().find_map(|c| facts.invites.get(&c.blob_id));
        let Some(inv) = parsed else {
            out.push(Meeting {
                uid: None,
                title: file.name.clone(),
                span: None,
                all_day: false,
                location: String::new(),
                method: None,
                cancelled: false,
                status: None,
                file,
            });
            continue;
        };
        let uid = inv.event.uid.clone();
        let seq = inv.sequence.max(inv.event.sequence);
        let status = facts.answers.get(&uid).copied().or_else(|| {
            let me = me?.to_lowercase();
            inv.event.participants.iter().find(|p| p.email.to_lowercase() == me).map(|p| p.status).filter(|s| *s != Partstat::NeedsAction)
        });
        let meeting = Meeting {
            uid: Some(uid.clone()),
            title: if inv.event.title.trim().is_empty() { file.name.clone() } else { inv.event.title.clone() },
            span: Some(inv.event.span),
            all_day: inv.event.all_day,
            location: inv.event.location.clone(),
            method: Some(inv.method.to_ascii_uppercase()),
            cancelled: inv.method.eq_ignore_ascii_case("CANCEL") || inv.event.status == EventStatus::Cancelled,
            status,
            file,
        };
        if let Some(&n) = by_uid.get(&uid) {
            if let Some(was) = out.get_mut(n) {
                let was_seq =
                    was.file.copies.iter().rev().find_map(|c| facts.invites.get(&c.blob_id)).map_or(i64::MIN, |i| i.sequence.max(i.event.sequence));
                let mut copies = std::mem::take(&mut was.file.copies);
                copies.extend(meeting.file.copies.iter().cloned());
                copies.sort_by_key(|c| c.origin.at);
                if (seq, meeting.file.last_at()) >= (was_seq, was.file.last_at()) {
                    *was = meeting;
                }
                was.file.copies = copies;
            }
        } else {
            by_uid.insert(uid, out.len());
            out.push(meeting);
        }
    }
    out.sort_by_key(|m| std::cmp::Reverse(m.file.last_at()));
    out
}

/// Медиа беседы из писем `letters` (любой порядок) и сведений `facts`; `me` — мой адрес (мой ответ на приглашение,
/// отметка «я» в участниках). Письма без тела дают только участника «От» и считаются незагруженными.
pub fn collect(letters: &[Letter], facts: &Facts, me: Option<&str>) -> ThreadMedia {
    let with_body = loaded(letters);
    let mut by_time: Vec<Letter> = letters.to_vec();
    by_time.sort_by(|a, b| (a.row.received_at, &a.row.id).cmp(&(b.row.received_at, &b.row.id)));
    let cards = files_of(&with_body, facts, is_vcard)
        .into_iter()
        .map(|file| {
            let cards = file.copies.iter().find_map(|c| facts.cards.get(&c.blob_id)).cloned().unwrap_or_default();
            CardFile { file, cards }
        })
        .collect();
    ThreadMedia {
        files: files_of(&with_body, facts, is_file),
        media: media_of(&with_body, facts),
        links: links_of(&with_body),
        contacts: Contacts { people: people_of(&by_time, me), cards },
        meetings: meetings_of(&with_body, facts, me),
        loaded: with_body.len(),
        total: letters.len(),
    }
}

/// Все ли слова запроса есть в тексте (уже в нижнем регистре).
fn hit(words: &[String], hay: &str) -> bool {
    words.iter().all(|w| hay.contains(w.as_str()))
}

fn origin_text(o: &Origin) -> String {
    format!("{} {}", o.from_name, o.from_addr)
}

fn file_text(f: &File) -> String {
    let mut s = format!("{} {}", f.name, f.mime);
    for c in &f.copies {
        s.push(' ');
        s.push_str(&c.name);
        s.push(' ');
        s.push_str(&origin_text(&c.origin));
    }
    s.to_lowercase()
}

/// Поиск внутри медиа беседы: каждое слово запроса (без регистра) — в имени файла, теме встречи, адресе или домене,
/// тексте ссылки, имени или адресе участника или отправителя. Пустой запрос — всё.
pub fn filter(m: &ThreadMedia, query: &str) -> ThreadMedia {
    let words: Vec<String> = query.to_lowercase().split_whitespace().map(String::from).collect();
    if words.is_empty() {
        return m.clone();
    }
    let link_text = |l: &Link| {
        let mut s = format!("{} {} {}", l.url, l.domain, l.text);
        l.seen.iter().for_each(|o| {
            s.push(' ');
            s.push_str(&origin_text(o));
        });
        s.to_lowercase()
    };
    let card_text = |c: &CardFile| {
        let mut s = file_text(&c.file);
        for card in &c.cards {
            s.push(' ');
            s.push_str(&format!("{} {} {}", card.name, card.emails.join(" "), card.organization).to_lowercase());
        }
        s
    };
    ThreadMedia {
        files: m.files.iter().filter(|f| hit(&words, &file_text(f))).cloned().collect(),
        media: m.media.iter().filter(|x| hit(&words, &file_text(&x.file))).cloned().collect(),
        links: m.links.iter().filter(|l| hit(&words, &link_text(l))).cloned().collect(),
        contacts: Contacts {
            people: m.contacts.people.iter().filter(|p| hit(&words, &format!("{} {}", p.name, p.email).to_lowercase())).cloned().collect(),
            cards: m.contacts.cards.iter().filter(|c| hit(&words, &card_text(c))).cloned().collect(),
        },
        meetings: m
            .meetings
            .iter()
            .filter(|x| hit(&words, &format!("{} {} {}", x.title.to_lowercase(), x.location.to_lowercase(), file_text(&x.file))))
            .cloned()
            .collect(),
        loaded: m.loaded,
        total: m.total,
    }
}

/// Письма без тела, по времени.
pub fn missing(letters: &[Letter]) -> Vec<String> {
    let mut v: Vec<&Letter> = letters.iter().filter(|l| l.body.is_none()).collect();
    v.sort_by_key(|l| l.row.received_at);
    v.into_iter().map(|l| l.row.id.clone()).collect()
}

/// Докачка недостающих тел в фоне — как при открытии письма: `Command::Body` на каждое письмо сервера без тела
/// (ответ — `Event::BodyReady`, после всех — собрать медиа заново). Письма «На этом компьютере» (`store::letter_ref`)
/// с сервера не качаются: их тело собирается из содержимого.
pub fn fetch(letters: &[Letter]) -> Vec<Command> {
    missing(letters).into_iter().filter(|id| crate::store::parse_ref(id).is_none() && Address::parse(id).is_none()).map(Command::Body).collect()
}

/// Письма беседы и сведения о вложениях из хранилища и воркера: тела, их IR, хэши вложений (частей хранилища и байтов
/// из кэша блобов, [`samefile::hash_of`]), кадры маленьких картинок, приглашения и визитки, мои ответы на приглашения.
/// I/O и воркер — вызывать вне потока интерфейса. Сбой воркера на письме — в IR только заметка об ошибке (ссылок нет), на вложении — нет сведений о нём.
pub fn gather(cache: &Cache, client: &mut Client, thread_id: &str) -> postman_cache::Result<(Vec<Letter>, Facts)> {
    let mut letters = Vec::new();
    let mut facts = Facts::default();
    for row in cache.thread(thread_id)? {
        let body = cache.body(&row.id)?;
        let doc = body.as_ref().filter(|b| b.html.is_some() || b.text.is_some()).map(|b| crate::render::render(client, b));
        // Хэши крупных частей письма уже посчитаны хранилищем (ADR 0006): по ним — без чтения байтов.
        let parts = match body.as_ref().filter(|b| !b.attachments.is_empty()) {
            Some(_) => {
                let (scope, id) = postman_cache::unscoped(&row.id);
                cache.letter_of(scope.unwrap_or(""), id)?.map(|l| cache.letter_parts(l)).transpose()?.unwrap_or_default()
            }
            None => Vec::new(),
        };
        for a in body.iter().flat_map(|b| &b.attachments) {
            if facts.hashes.contains_key(&a.blob_id) {
                continue;
            }
            let bytes = cache.blob(&a.blob_id)?;
            if let Some(h) = samefile::hash_of(a, &parts, |_| bytes.clone()) {
                facts.hashes.insert(a.blob_id.clone(), h);
            }
            let Some(bytes) = bytes else { continue };
            probe(client, a, &bytes, &mut facts);
        }
        letters.push(Letter { row, body, doc });
    }
    answers(cache, &mut facts)?;
    Ok((letters, facts))
}

/// Сведения о вложении по его байтам — воркером: кадр маленькой картинки, приглашение, визитка.
fn probe(client: &mut Client, a: &AttachmentRow, bytes: &[u8], facts: &mut Facts) {
    if is_image(a) && bytes.len() <= PROBE_BYTES && !facts.frames.contains_key(&a.blob_id) {
        match client.decode_image(bytes, PROBE_SIDE) {
            Ok(img) => {
                facts.frames.insert(a.blob_id.clone(), (img.width, img.height));
            }
            Err(e) => tracing::debug!("thread media: image frame unknown: {e}"),
        }
    }
    if is_calendar(a) {
        match client.parse_calendar(bytes).and_then(|v| Invitation::from_worker(&v, &TimeZone::system())) {
            Ok(inv) => {
                facts.invites.insert(a.blob_id.clone(), inv);
            }
            Err(e) => tracing::debug!("thread media: invitation not parsed: {e}"),
        }
    }
    if is_vcard(a) {
        match client.parse_contacts(bytes) {
            Ok(cards) => {
                facts.cards.insert(a.blob_id.clone(), cards.iter().map(ContactCard::from_jscontact).collect());
            }
            Err(e) => tracing::debug!("thread media: vCard not parsed: {e}"),
        }
    }
}

/// Мои ответы на приглашения из хранилища состояния.
fn answers(cache: &Cache, facts: &mut Facts) -> postman_cache::Result<()> {
    let uids: HashSet<String> = facts.invites.values().map(|i| i.event.uid.clone()).collect();
    for uid in uids {
        if let Some(seen) = cache.state(&format!("{ANSWER_KEY}{uid}"))?.as_deref().and_then(Seen::decode) {
            facts.answers.insert(uid, seen.status);
        }
    }
    Ok(())
}

/// Сообщения беседы канала `at` (строки ленты, люди канала) — входом медиа беседы: сообщение — «письмо» с адресом
/// `postman://…` вместо ключа (`Origin::letter`), автор — имя и почта (нет почты — id в канале), тело — IR,
/// вложения — с хэшами загруженных частей ([`Facts::hashes`]) и кадрами из модели. «Сообщение удалено» — мимо.
pub fn chat_letters(at: &Address, title: &str, lines: &[Line], people: &[Participant]) -> (Vec<Letter>, Facts) {
    let mut facts = Facts::default();
    let chan = at.chan();
    let mut letters = Vec::with_capacity(lines.len());
    for line in lines {
        let Line::Message { message: m, loaded } = line else { continue };
        let address = Address { message: Some(m.id.clone()), ..at.of_conversation() };
        let id = address.to_string();
        let who = people.iter().find(|p| p.id == m.author);
        let from_name = who.map_or_else(|| m.author.0.clone(), |p| p.name.clone());
        let from_addr = who.and_then(|p| p.emails.first().cloned()).unwrap_or_else(|| crate::channel::index::author_key(&chan, &m.author));
        let mut attachments = Vec::with_capacity(m.attachments.len());
        for (i, a) in m.attachments.iter().enumerate() {
            let blob_id = format!("{id}#{i}");
            if let Some((_, h)) = loaded.iter().find(|(s, _)| *s == a.source) {
                facts.hashes.insert(blob_id.clone(), *h);
            }
            match a.kind {
                AttachmentKind::Image { w, h } | AttachmentKind::Video { w, h, .. } if w > 0 && h > 0 => {
                    facts.frames.insert(blob_id.clone(), (w, h));
                }
                _ => {}
            }
            attachments.push(AttachmentRow {
                blob_id,
                name: a.name.clone(),
                mime: a.mime.clone(),
                size: a.size.unwrap_or(0),
                cid: None,
                inline: false,
            });
        }
        let doc = match &m.body {
            Body::Ir(d) => d.clone(),
            Body::Html { plain, .. } | Body::Plain(plain) => postman_ir::from_plain(plain),
        };
        let row = EmailRow {
            id,
            thread_id: at.of_conversation().to_string(),
            mailbox_ids: Vec::new(),
            received_at: m.sent.div_euclid(1000),
            subject: title.to_owned(),
            from_name,
            from_addr,
            preview: String::new(),
            seen: true,
            flagged: false,
            has_attachment: !attachments.is_empty(),
            size: 0,
            keywords: Vec::new(),
        };
        let body = BodyRow { text: Some(m.body.plain()), attachments, ..BodyRow::default() };
        letters.push(Letter { row, body: Some(body), doc: Some(doc) });
    }
    (letters, facts)
}

/// Медиа беседы `at` вместе со связанными (`channel::linked::group`): почтовые беседы — [`gather`] (`mail` — ящики
/// профиля; нет — почта мимо), беседы каналов — [`chat_letters`] по тому, что есть в базе (приглашения и визитки
/// загруженных файлов — воркером). Команды докачки тел — только у писем.
pub fn load_group(
    cache: &Cache,
    client: &mut Client,
    mail: Option<&MailSource>,
    at: &Address,
    me: Option<&str>,
) -> Result<(ThreadMedia, Vec<Command>), ChannelError> {
    let mut letters = Vec::new();
    let mut facts = Facts::default();
    let chats = Chats::new(cache.clone());
    for conv in crate::channel::linked::group(cache, at)? {
        if conv.channel.0 == crate::channel::MAIL {
            let Some(thread) = mail.and_then(|m| m.thread_key(&conv)) else { continue };
            let (l, f) = gather(cache, client, &thread)?;
            letters.extend(l);
            merge(&mut facts, f);
            continue;
        }
        let Some(store) = ChannelStore::find(cache, &conv.channel, &conv.account)? else { continue };
        let Some(summary) = chats.conversation(&conv)? else { continue };
        let lines = chats.timeline(&conv, None, usize::MAX)?;
        let (l, f) = chat_letters(&conv, &summary.conversation.title, &lines, &store.participants()?);
        for a in l.iter().flat_map(|l| l.body.iter().flat_map(|b| &b.attachments)).filter(|a| is_calendar(a) || is_vcard(a)) {
            if let Some(bytes) = f.hashes.get(&a.blob_id).map(|h| cache.chan_part(*h)).transpose()?.flatten() {
                probe(client, a, &bytes, &mut facts);
            }
        }
        letters.extend(l);
        merge(&mut facts, f);
    }
    answers(cache, &mut facts)?;
    Ok((collect(&letters, &facts, me), fetch(&letters)))
}

fn merge(into: &mut Facts, from: Facts) {
    into.hashes.extend(from.hashes);
    into.frames.extend(from.frames);
    into.invites.extend(from.invites);
    into.cards.extend(from.cards);
    into.answers.extend(from.answers);
}

/// Медиа беседы целиком: [`gather`], [`collect`] и команды докачки [`fetch`] (пусто — все тела есть).
pub fn load(cache: &Cache, client: &mut Client, thread_id: &str, me: Option<&str>) -> postman_cache::Result<(ThreadMedia, Vec<Command>)> {
    let (letters, facts) = gather(cache, client, thread_id)?;
    Ok((collect(&letters, &facts, me), fetch(&letters)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use postman_demo::media as demo;

    const NOW: i64 = 2_000_000_000;

    /// Беседа демо: тела — от демо, IR — тем же разбором, что в воркере (в тесте — напрямую).
    fn letters(with_body: &[&str]) -> Vec<Letter> {
        demo::rows(NOW)
            .into_iter()
            .map(|row| {
                let body = with_body.contains(&row.id.as_str()).then(|| postman_demo::body(&row.id));
                let doc = body.as_ref().and_then(|b| b.html.as_deref()).map(postman_ir::from_html);
                Letter { row, body, doc }
            })
            .collect()
    }

    /// Строка письма для тестов: у `EmailRow` нет `Default`.
    fn email(id: &str, from: &str, at: i64) -> EmailRow {
        EmailRow {
            id: id.into(),
            thread_id: String::new(),
            mailbox_ids: Vec::new(),
            received_at: at,
            subject: String::new(),
            from_name: String::new(),
            from_addr: from.into(),
            preview: String::new(),
            seen: true,
            flagged: false,
            has_attachment: false,
            size: 0,
            keywords: Vec::new(),
        }
    }

    fn all() -> Vec<Letter> {
        letters(&demo::LETTERS)
    }

    /// Хэши байтов всех вложений беседы (байты — в хранилище).
    fn hashed() -> Facts {
        let mut f = Facts::default();
        for id in demo::LETTERS {
            for a in postman_demo::body(id).attachments {
                if let Some(bytes) = postman_demo::blob(&a.blob_id) {
                    f.hashes.insert(a.blob_id, Hash::of(&bytes));
                }
            }
        }
        f
    }

    fn invitation(uid: &str, seq: i64, title: &str) -> Invitation {
        let v = serde_json::json!({ "method": "request", "data": {
            "@type": "Event", "uid": uid, "title": title, "sequence": seq,
            "start": "2026-12-01T10:00:00", "timeZone": "Europe/Moscow", "duration": "PT1H",
            "participants": {
                "o": { "name": "Иванова Мария", "calendarAddress": "mailto:m.ivanova@corp.example", "roles": { "owner": true } },
                "m": { "calendarAddress": "mailto:me@corp.example", "roles": { "attendee": true }, "participationStatus": "needs-action" }
            }
        }});
        Invitation::from_worker(&v, &TimeZone::UTC).unwrap()
    }

    fn names(files: &[File]) -> Vec<&str> {
        let mut v: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
        v.sort();
        v
    }

    #[test]
    fn same_file_sent_twice_is_one_row() {
        for facts in [hashed(), Facts::default()] {
            let m = collect(&all(), &facts, Some(postman_demo::ME));
            assert_eq!(
                names(&m.files),
                ["Демонстрация пилота.ics", "Обход стенда.mp4", "Сидоров Игорь.vcf", "ТЗ пилота «Маяк».pdf", "Фото стенда.jpg"]
            );
            let tz = m.files.iter().find(|f| f.name == demo::TWICE).unwrap();
            assert_eq!(tz.count(), 2);
            assert_eq!(tz.copies.iter().map(|c| c.blob_id.as_str()).collect::<Vec<_>>(), demo::TWICE_BLOBS);
            assert_eq!(tz.first().unwrap().origin.from_addr, "m.ivanova@corp.example");
            assert_eq!(tz.copies[1].origin.letter, "media2");
        }
        let tz = collect(&all(), &hashed(), None).files.into_iter().find(|f| f.name == demo::TWICE).unwrap();
        assert!(matches!(tz.key, FileKey::Content(_)), "по содержимому");
        // Те же имя и размер, но другие байты — разные строки.
        let mut facts = hashed();
        facts.hashes.insert(demo::TWICE_BLOBS[1].into(), Hash::of(b"other"));
        let m = collect(&all(), &facts, None);
        assert_eq!(m.files.iter().filter(|f| f.name == demo::TWICE).count(), 2);
    }

    #[test]
    fn no_signatures_or_pixels_in_media() {
        for facts in [hashed(), Facts::default()] {
            let m = collect(&all(), &facts, None);
            let mut got: Vec<(&str, bool, bool)> = m.media.iter().map(|x| (x.file.name.as_str(), x.video, x.inline)).collect();
            got.sort();
            assert_eq!(got, [("scheme.png", false, true), ("Обход стенда.mp4", true, false), ("Фото стенда.jpg", false, false)]);
            assert_eq!(m.media.iter().find(|x| x.inline).and_then(|x| x.frame), Some((600, 375)), "кадр из разметки");
        }
        // Пиксель без размера в разметке — по кадру от декодера.
        let mut l = all();
        if let Some(d) = l[0].doc.as_mut() {
            *d = postman_ir::from_html("<p>Без размеров</p>");
        }
        let mut facts = hashed();
        assert!(collect(&l, &facts, None).media.iter().any(|x| x.file.copies[0].blob_id == demo::PIXEL_BLOB), "кадр неизвестен — медиа");
        facts.frames.insert(demo::PIXEL_BLOB.into(), (1, 1));
        assert!(!collect(&l, &facts, None).media.iter().any(|x| x.file.copies[0].blob_id == demo::PIXEL_BLOB));
        assert!(is_tiny((31, 31)) && !is_tiny((32, 1)) && !is_tiny((1, 40)));
    }

    /// Подпись: в письмах разных авторов или во всех письмах одного автора; у одного автора в части писем — медиа.
    #[test]
    fn signature_rule() {
        let row = |id: &str, from: &str, at: i64| email(id, from, at);
        let logo = |blob: &str| AttachmentRow {
            blob_id: blob.into(),
            name: "image001.png".into(),
            mime: "image/png".into(),
            size: 500,
            cid: Some(format!("{blob}@x")),
            inline: true,
        };
        let with = |a: Option<AttachmentRow>| BodyRow { attachments: a.into_iter().collect(), ..Default::default() };
        let (r1, r2, r3) = (row("a1", "a@x", 1), row("a2", "a@x", 2), row("a3", "a@x", 3));
        let (b1, b2, none) = (with(Some(logo("l1"))), with(Some(logo("l2"))), with(None));
        let facts = Facts::default();
        // Во всех двух письмах автора — подпись.
        assert_eq!(signatures(&[(&r1, &b1, None), (&r2, &b2, None)], &facts).len(), 2);
        // В двух из трёх — медиа.
        assert!(signatures(&[(&r1, &b1, None), (&r2, &b2, None), (&r3, &none, None)], &facts).is_empty());
        // Одно письмо — не понять, медиа.
        assert!(signatures(&[(&r1, &b1, None)], &facts).is_empty());
        // Разные авторы — подпись, даже если у них есть письма без неё.
        let rb = row("b1", "b@x", 4);
        assert_eq!(signatures(&[(&r1, &b1, None), (&r3, &none, None), (&rb, &b2, None)], &facts).len(), 2);
        // Разные байты под тем же именем — разные картинки.
        let facts = Facts { hashes: HashMap::from([("l1".into(), Hash::of(b"1")), ("l2".into(), Hash::of(b"2"))]), ..Default::default() };
        assert!(signatures(&[(&r1, &b1, None), (&rb, &b2, None)], &facts).is_empty());
    }

    #[test]
    fn links_without_quotes_and_unsafe_schemes() {
        let m = collect(&all(), &Facts::default(), None);
        let mut urls: Vec<&str> = m.links.iter().map(|l| l.url.as_str()).collect();
        urls.sort();
        assert_eq!(urls, [demo::TRACKING_URL, demo::SHEET_URL, demo::UNSUBSCRIBE_URL, demo::PLAN_URL]);
        for q in demo::QUOTED_URLS {
            assert!(!urls.contains(&q), "{q} — только в цитате");
        }
        let plan = m.links.iter().find(|l| l.url == demo::PLAN_URL).unwrap();
        assert_eq!(plan.seen.iter().map(|o| o.letter.as_str()).collect::<Vec<_>>(), ["media1", "media4"], "цитата media2 не считается");
        assert_eq!((plan.domain.as_str(), plan.text.as_str(), plan.kind), ("wiki.corp.example", "в вики", LinkKind::Plain));
        let kind = |u: &str| m.links.iter().find(|l| l.url == u).map(|l| l.kind);
        assert_eq!(kind(demo::UNSUBSCRIBE_URL), Some(LinkKind::Unsubscribe));
        assert_eq!(kind(demo::TRACKING_URL), Some(LinkKind::Tracking));
        // Небезопасные схемы и `cid:` — даже если попали в IR.
        let link = |href: &str| Inline::Link { href: href.into(), host: None, suspicious: false, content: vec![Inline::text("x")] };
        let doc = Document {
            blocks: vec![Block::Paragraph {
                align: Default::default(),
                content: ["javascript:alert(1)", "mailto:a@b.example", "cid:img@x", "data:text/html,hi", "file:///c:/x", "https://www.ok.example/a"]
                    .into_iter()
                    .map(link)
                    .collect(),
            }],
            ..Default::default()
        };
        let one = vec![Letter { row: email("x", "", 0), body: Some(BodyRow::default()), doc: Some(doc) }];
        let links = collect(&one, &Facts::default(), None).links;
        assert_eq!(links.iter().map(|l| (l.url.as_str(), l.domain.as_str())).collect::<Vec<_>>(), [("https://www.ok.example/a", "ok.example")]);
        assert_eq!(link_kind("https://ex.example/news?utm_source=x", ""), LinkKind::Tracking);
        assert_eq!(link_kind("https://ex.example/a", "Отписаться"), LinkKind::Unsubscribe);
    }

    #[test]
    fn participants_without_repeats() {
        let m = collect(&all(), &Facts::default(), Some("ME@corp.example"));
        let people: Vec<(&str, usize, usize, bool)> = m.contacts.people.iter().map(|p| (p.email.as_str(), p.letters, p.sent, p.me)).collect();
        assert_eq!(
            people,
            [
                ("m.ivanova@corp.example", 4, 2, false),
                ("me@corp.example", 4, 1, true),
                ("a.petrov@corp.example", 3, 1, false),
                ("team@corp.example", 1, 0, false)
            ]
        );
        assert_eq!(m.contacts.people[0].name, "Иванова Мария");
        assert_eq!(m.contacts.cards.len(), 1);
        assert!(m.contacts.cards[0].cards.is_empty(), "не разобрана — только имя файла");
        let mut facts = Facts::default();
        let card = ContactCard { name: "Сидоров Игорь".into(), emails: vec!["i.sidorov@corp.example".into()], ..Default::default() };
        facts.cards.insert(demo::CARD_BLOB.into(), vec![card.clone()]);
        assert_eq!(collect(&all(), &facts, None).contacts.cards[0].cards, [card]);
        assert_eq!(collect(&all(), &facts, None).counts().contacts, 5);
    }

    #[test]
    fn meetings() {
        let m = collect(&all(), &Facts::default(), None);
        assert_eq!(m.meetings.len(), 1);
        assert_eq!((m.meetings[0].uid.as_deref(), m.meetings[0].title.as_str()), (None, "Демонстрация пилота.ics"));
        let mut facts = Facts::default();
        facts.invites.insert(demo::INVITE_BLOB.into(), invitation("demo-uid", 0, "Демонстрация пилота"));
        let m = collect(&all(), &facts, Some(postman_demo::ME));
        let x = &m.meetings[0];
        assert_eq!(
            (x.uid.as_deref(), x.title.as_str(), x.method.as_deref(), x.status),
            (Some("demo-uid"), "Демонстрация пилота", Some("REQUEST"), None)
        );
        assert!(x.span.is_some_and(|s| s.end - s.start == 3600) && !x.cancelled);
        facts.answers.insert("demo-uid".into(), Partstat::Accepted);
        assert_eq!(collect(&all(), &facts, None).meetings[0].status, Some(Partstat::Accepted));
        // Две версии одной встречи в разных письмах — одна строка, по новейшей.
        let mut l = all();
        if let Some(b) = l[1].body.as_mut() {
            b.attachments.push(AttachmentRow {
                blob_id: "v0".into(),
                name: "old.ics".into(),
                mime: "text/calendar".into(),
                size: 1,
                ..Default::default()
            });
        }
        facts.invites.insert("v0".into(), invitation("demo-uid", 0, "Черновик"));
        facts.invites.insert(demo::INVITE_BLOB.into(), invitation("demo-uid", 2, "Демонстрация пилота"));
        let ms = collect(&l, &facts, None).meetings;
        assert_eq!(ms.len(), 1);
        assert_eq!((ms[0].title.as_str(), ms[0].file.count()), ("Демонстрация пилота", 2));
    }

    #[test]
    fn search_inside() {
        let mut facts = hashed();
        let card = ContactCard { name: "Сидоров Игорь".into(), emails: vec!["i.sidorov@corp.example".into()], ..Default::default() };
        facts.cards.insert(demo::CARD_BLOB.into(), vec![card]);
        let m = collect(&all(), &facts, None);
        assert_eq!(filter(&m, "  "), m);
        let f = filter(&m, "МАЯК pdf");
        assert_eq!(names(&f.files), [demo::TWICE]);
        assert!(f.links.is_empty() && f.media.is_empty());
        let f = filter(&m, "docs.corp");
        assert_eq!(f.links.iter().map(|l| l.url.as_str()).collect::<Vec<_>>(), [demo::SHEET_URL]);
        assert_eq!(filter(&m, "регистрация").links.len(), 1, "по тексту ссылки");
        let f = filter(&m, "a.petrov");
        assert_eq!(names(&f.files), ["Обход стенда.mp4", "Фото стенда.jpg"], "по адресу отправителя");
        assert_eq!(f.contacts.people.len(), 1);
        assert_eq!(f.media.len(), 3);
        assert_eq!(filter(&m, "i.sidorov").contacts.cards.len(), 1, "по адресу в визитке");
        assert_eq!(filter(&m, "демонстрация").meetings.len(), 1);
        assert_eq!((filter(&m, "нет такого").is_empty(), filter(&m, "x").total), (true, 4));
    }

    #[test]
    fn partly_loaded_thread() {
        let mut l = letters(&["media1", "media3"]);
        let m = collect(&l, &hashed(), None);
        assert_eq!((m.loaded, m.total, m.complete()), (2, 4, false));
        assert_eq!(m.files.iter().find(|f| f.name == demo::TWICE).map(File::count), Some(1));
        assert!(m.meetings.is_empty() && m.contacts.cards.is_empty());
        // Автор письма без тела — уже участник.
        assert!(m.contacts.people.iter().any(|p| p.email == postman_demo::ME));
        assert_eq!(missing(&l), ["media2", "media4"]);
        // Письмо «На этом компьютере» с сервера не качается.
        l[3].row.id = crate::store::letter_ref(crate::model::LetterId(7));
        let ids: Vec<String> = fetch(&l).into_iter().filter_map(|c| if let Command::Body(id) = c { Some(id) } else { None }).collect();
        assert_eq!(ids, ["media2"]);
        assert!(collect(&all(), &hashed(), None).complete() && fetch(&all()).is_empty());
    }

    /// Сбор из хранилища: хэши — по байтам из кэша блобов; воркера нет — без разбора, без падения.
    #[test]
    fn gather_from_cache_without_worker() {
        let cache = Cache::open_in_memory().unwrap();
        cache.upsert_emails(&demo::rows(NOW)).unwrap();
        for id in ["media1", "media2"] {
            let b = postman_demo::body(id);
            for a in &b.attachments {
                cache.put_blob(&a.blob_id, &postman_demo::blob(&a.blob_id).unwrap()).unwrap();
            }
            cache.put_body(id, &b).unwrap();
        }
        let mut client = Client::new(std::env::temp_dir().join("postman-no-such-worker.exe"), std::time::Duration::from_secs(1));
        let (m, cmds) = load(&cache, &mut client, demo::THREAD, None).unwrap();
        assert_eq!((m.loaded, m.total), (2, 4));
        let tz = m.files.iter().find(|f| f.name == demo::TWICE).unwrap();
        assert!(tz.count() == 2 && matches!(tz.key, FileKey::Content(_)));
        assert_eq!(cmds.len(), 2);
        assert!(m.links.is_empty(), "без воркера ссылок нет");
        assert!(m.contacts.cards.iter().all(|c| c.cards.is_empty()));
    }
}

#[cfg(test)]
mod channel_tests {
    use super::*;
    use crate::channel::mail::MailAccount;
    use crate::channel::{
        AccountId, Attachment, AttachmentSource, ByteSink, ChannelEvent, ChannelId, Conversation, ConversationId, Message, MessageId, ParticipantId,
    };
    use crate::filters::Me;

    /// Медиа беседы чата и связанной почтовой беседы вместе: файлы и картинки по хэшам загруженных частей, ссылки из
    /// текста сообщений, люди обоих каналов; строка показывает канал; докачка — только тел писем.
    #[test]
    fn chat_and_linked_mail_media() {
        let cache = Cache::open_in_memory().unwrap();
        let store = ChannelStore::open(cache.clone(), &ChannelId::new("demo"), &AccountId::new("me@example.test"), "Я").unwrap();
        let g = ConversationId::new("g");
        let file = |name: &str, mime: &str, src: &[u8], kind| Attachment {
            name: name.into(),
            mime: mime.into(),
            size: Some(5),
            kind,
            source: AttachmentSource(src.to_vec()),
            thumb: None,
        };
        let m = |id: &str, sent: i64, text: &str, a: Vec<Attachment>| Message {
            id: MessageId::new(id),
            author: ParticipantId::new("u-anna"),
            sent,
            body: crate::channel::Body::Plain(text.into()),
            attachments: a,
            ..Message::default()
        };
        store
            .apply(vec![
                ChannelEvent::People(vec![Participant { id: ParticipantId::new("u-anna"), name: "Анна".into(), ..Participant::default() }]),
                ChannelEvent::Conversations(vec![Conversation { id: g.clone(), title: "Команда".into(), ..Conversation::default() }]),
                ChannelEvent::Messages {
                    conversation: g.clone(),
                    messages: vec![
                        m(
                            "1",
                            1_000_000,
                            "Смотри https://example.test/doc",
                            vec![file("photo.jpg", "image/jpeg", b"p1", AttachmentKind::Image { w: 800, h: 600 })],
                        ),
                        m("2", 2_000_000, "План", vec![file("plan.pdf", "application/pdf", b"p2", AttachmentKind::File)]),
                    ],
                    live: false,
                },
            ])
            .unwrap();
        let mut d = store.begin_download();
        d.write(b"\xff\xd8jpg").unwrap();
        store.finish_download(d, &g, &MessageId::new("1"), &AttachmentSource(b"p1".to_vec()), "image/jpeg").unwrap();
        cache
            .upsert_emails(&[EmailRow {
                thread_id: "t1".into(),
                mailbox_ids: vec!["in".into()],
                subject: "Команда".into(),
                ..email("e1", "boris@example.test", 500)
            }])
            .unwrap();
        let chat = Address::conversation(store.channel(), store.account(), &g);
        let mail_at = Address::conversation(&ChannelId::new("mail"), &AccountId::new("me@example.test"), &ConversationId::new("t1"));
        crate::channel::linked::link(&cache, &chat, &mail_at).unwrap();
        let mail = MailSource::new(cache.clone(), vec![MailAccount::new(None, "me@example.test")], Me::new(["me@example.test"], ["Я"]));
        let mut client = Client::new("no-worker.exe".into(), std::time::Duration::from_secs(1));
        let (media, commands) = load_group(&cache, &mut client, Some(&mail), &chat, Some("me@example.test")).unwrap();
        assert_eq!((media.total, media.loaded), (3, 2), "письмо без тела — не загружено");
        assert!(matches!(&commands[..], [Command::Body(id)] if id == "e1"));
        assert_eq!(media.media.len(), 1);
        assert_eq!(media.media[0].frame, Some((800, 600)));
        assert!(matches!(media.media[0].file.key, samefile::FileKey::Content(_)), "картинка — по хэшу загруженной части");
        assert_eq!(media.files.len(), 2);
        assert_eq!(media.links[0].url, "https://example.test/doc");
        assert_eq!(media.links[0].seen[0].channel, "demo");
        assert!(media.links[0].seen[0].letter.starts_with("postman://demo/"));
        let people: Vec<&str> = media.contacts.people.iter().map(|p| p.name.as_str()).collect();
        assert!(people.contains(&"Анна"), "{people:?}");
        assert!(media.contacts.people.iter().any(|p| p.email == "boris@example.test"));
    }

    fn email(id: &str, from: &str, at: i64) -> EmailRow {
        EmailRow {
            id: id.into(),
            thread_id: String::new(),
            mailbox_ids: Vec::new(),
            received_at: at,
            subject: String::new(),
            from_name: String::new(),
            from_addr: from.into(),
            preview: String::new(),
            seen: true,
            flagged: false,
            has_attachment: false,
            size: 0,
            keywords: Vec::new(),
        }
    }
}
