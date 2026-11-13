//! Вложения беседы новым участникам (решение владельца 2026-11-13, `docs/decisions/2026-11-13-reply-all-attachments.md`):
//! в ответ добавили человека, которого не было ни в одном письме беседы, — он получает её вложения, прежние участники —
//! нет. Без I/O: участников и файлы беседы собирает интерфейс из кэша ([`collect`]), копии письма отправляет
//! синхронизация ([`Draft::outgoing`], [`Draft::attachments_letter`]).

use crate::{Address, Attachment, Draft};

/// Вложения беседы для черновика ответа.
#[derive(Debug, Clone, Default)]
pub struct Share {
    /// Адреса писем беседы («От», «Кому», «Копия»), в нижнем регистре.
    pub known: Vec<String>,
    /// Файлы беседы без повторов по содержимому. Байтов может не быть — их скачает синхронизация по `blob_id`.
    pub attachments: Vec<Attachment>,
    /// Текст отдельного письма «Вложения из беседы» (Markdown): у сервера, который доставляет всем из заголовков
    /// (EWS), новым участникам уходит оно.
    pub note: String,
    /// Отключено для этого письма (строка над текстом черновика).
    pub off: bool,
}

impl Share {
    /// Адрес есть в письмах беседы.
    pub fn knows(&self, email: &str) -> bool {
        self.known.iter().any(|k| k.eq_ignore_ascii_case(email))
    }
}

/// Файл письма беседы.
#[derive(Debug, Clone, Default)]
pub struct File {
    pub name: String,
    pub mime: String,
    pub blob_id: String,
    pub size: u64,
    /// SHA-256 байтов, если они есть в кэше.
    pub hash: Option<[u8; 32]>,
}

impl File {
    /// Тот же файл: по хэшу байтов, если он есть у обоих; иначе — тот же blob или тот же размер и имя.
    pub fn same(&self, other: &File) -> bool {
        match (self.hash, other.hash) {
            (Some(a), Some(b)) => a == b,
            _ => self.blob_id == other.blob_id || (self.size == other.size && self.name.eq_ignore_ascii_case(&other.name)),
        }
    }
}

/// Письмо беседы: кто в нём был («От», «Кому», «Копия») и его файлы.
#[derive(Debug, Clone, Default)]
pub struct Letter {
    pub people: Vec<String>,
    pub files: Vec<File>,
}

/// Кто уже получал файл: участники писем беседы, где он был (в нижнем регистре, без повторов).
pub fn received(letters: &[Letter], file: &File) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for l in letters.iter().filter(|l| l.files.iter().any(|f| f.same(file))) {
        for p in &l.people {
            let p = p.to_lowercase();
            if !p.is_empty() && !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// Вложения беседы для черновика: участники всех писем и их файлы без повторов по содержимому. Файл, который
/// черновик уже несёт (`carried` — по `blob_id`), не повторяется. Файлов нет — `None`: поведение как без правила.
pub fn collect(letters: &[Letter], carried: &[Attachment], note: String) -> Option<Share> {
    let mut known: Vec<String> = Vec::new();
    for p in letters.iter().flat_map(|l| &l.people) {
        let p = p.to_lowercase();
        if !p.is_empty() && !known.contains(&p) {
            known.push(p);
        }
    }
    let mut files: Vec<&File> = Vec::new();
    for f in letters.iter().flat_map(|l| &l.files) {
        let in_draft = carried.iter().any(|a| a.blob_id.as_deref() == Some(f.blob_id.as_str()));
        if !in_draft && !files.iter().any(|g| g.same(f)) {
            files.push(f);
        }
    }
    if files.is_empty() {
        return None;
    }
    let attachments = files
        .into_iter()
        .map(|f| Attachment { name: f.name.clone(), mime: f.mime.clone(), data: Vec::new(), inline_cid: None, blob_id: Some(f.blob_id.clone()) })
        .collect();
    Some(Share { known, attachments, note, off: false })
}

/// Одна копия письма на отправку: письмо, адреса конверта и класть ли её в «Отправленные».
#[derive(Debug, Clone)]
pub struct Outgoing {
    pub draft: Draft,
    /// Адреса конверта (SMTP RCPT TO, JMAP `envelope.rcptTo`).
    pub rcpt: Vec<String>,
    /// Копия в «Отправленные» — та, что видели прежние участники; копия новым участникам — нет (без дублей в списке).
    pub keep: bool,
}

impl Draft {
    /// Новые участники: адресаты черновика, которых нет ни в одном письме беседы. Правило отключено, вложений
    /// беседы нет или это не ответ — никого.
    pub fn newcomers(&self) -> Vec<&Address> {
        let Some(share) = self.share.as_ref().filter(|s| !s.off && !s.attachments.is_empty()) else { return Vec::new() };
        let mut out: Vec<&Address> = Vec::new();
        for a in self.recipients() {
            let me = a.email.eq_ignore_ascii_case(&self.from.email);
            if !a.email.is_empty() && !me && !share.knows(&a.email) && !out.iter().any(|b| b.same(a)) {
                out.push(a);
            }
        }
        out
    }

    /// Копии письма на отправку. Новых участников нет — одна, как написана, всем. Есть — две с одинаковыми
    /// заголовками (кроме Message-ID, его ставит сборка): без вложений беседы — прежним участникам (её — в
    /// «Отправленные»), с ними — новым. Прежних среди адресатов нет — одна, с вложениями, всем.
    pub fn outgoing(&self) -> Vec<Outgoing> {
        let mut plain = self.clone();
        plain.share = None;
        let new: Vec<String> = self.newcomers().into_iter().map(|a| a.email.clone()).collect();
        let mut all: Vec<String> = Vec::new();
        for a in self.recipients() {
            if !all.iter().any(|e| e.eq_ignore_ascii_case(&a.email)) {
                all.push(a.email.clone());
            }
        }
        let Some(share) = self.share.as_ref().filter(|_| !new.is_empty()) else {
            return vec![Outgoing { draft: plain, rcpt: all, keep: true }];
        };
        let mut with = plain.clone();
        with.attachments.extend(share.attachments.iter().cloned());
        let known: Vec<String> = all.into_iter().filter(|e| !new.iter().any(|n| n.eq_ignore_ascii_case(e))).collect();
        if known.is_empty() {
            return vec![Outgoing { draft: with, rcpt: new, keep: true }];
        }
        vec![Outgoing { draft: plain, rcpt: known, keep: true }, Outgoing { draft: with, rcpt: new, keep: false }]
    }

    /// Запасной путь сервера, который доставляет всем из заголовков (EWS): основное письмо уходит всем как обычно,
    /// новым участникам — отдельное «Вложения из беседы»: та же тема и место в беседе, короткий текст
    /// ([`Share::note`]), вложения. `None` — отдельного письма не нужно (новых нет или прежних среди адресатов нет:
    /// тогда вложения — в основном письме, [`Draft::outgoing`]).
    pub fn attachments_letter(&self) -> Option<Draft> {
        let out = self.outgoing();
        let [_, extra] = out.as_slice() else { return None };
        let share = self.share.as_ref()?;
        let new = self.newcomers();
        let in_headers = |a: &&Address| self.to.iter().chain(&self.cc).any(|b| b.same(a));
        let to: Vec<Address> = new.iter().copied().filter(in_headers).cloned().collect();
        let bcc: Vec<Address> = new.iter().copied().filter(|a| !in_headers(a)).cloned().collect();
        let mut d = extra.draft.clone();
        d.attachments = share.attachments.clone();
        d.markdown = share.note.clone();
        d.to = to;
        d.cc = Vec::new();
        d.bcc = bcc;
        d.calendar = None;
        Some(d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(blob: &str, name: &str, size: u64, hash: Option<u8>) -> File {
        File { name: name.into(), mime: "application/pdf".into(), blob_id: blob.into(), size, hash: hash.map(|h| [h; 32]) }
    }

    fn letter(people: &[&str], files: Vec<File>) -> Letter {
        Letter { people: people.iter().map(|p| p.to_string()).collect(), files }
    }

    /// Беседа: Иван прислал отчёт Петру и мне; Пётр ответил всем с тем же отчётом (другой blob — те же байты) и
    /// сметой; Анна была только в копии последнего письма, без вложений.
    fn thread() -> Vec<Letter> {
        vec![
            letter(&["ivan@corp.example", "me@corp.example", "Petr@corp.example"], vec![file("B1", "Отчёт.pdf", 10, Some(1))]),
            letter(
                &["petr@corp.example", "ivan@corp.example", "me@corp.example"],
                vec![file("B7", "Отчёт (копия).pdf", 10, Some(1)), file("B2", "Смета.xlsx", 20, Some(2))],
            ),
            letter(&["ivan@corp.example", "me@corp.example", "anna@corp.example"], Vec::new()),
        ]
    }

    fn me() -> Address {
        Address::new("Я", "me@corp.example")
    }

    fn reply(share: Option<Share>) -> Draft {
        Draft {
            from: me(),
            to: vec![Address::new("Иван", "ivan@corp.example")],
            cc: vec![Address::new("Анна", "anna@corp.example"), Address::new("Ольга", "olga@corp.example")],
            bcc: vec![Address::new("", "boss@corp.example")],
            subject: "RE: Отчёт".into(),
            markdown: "Добавляю Ольгу.".into(),
            in_reply_to: Some("m3@corp.example".into()),
            references: vec!["m1@corp.example".into(), "m3@corp.example".into()],
            share,
            ..Default::default()
        }
    }

    #[test]
    fn same_file_by_content_then_by_blob_size_and_name() {
        // Байты есть у обоих — решает хэш, имя и blob не важны.
        assert!(file("B1", "a.pdf", 10, Some(1)).same(&file("B9", "b.pdf", 99, Some(1))));
        assert!(!file("B1", "a.pdf", 10, Some(1)).same(&file("B1", "a.pdf", 10, Some(2))));
        // Байтов нет хотя бы у одного — тот же blob или тот же размер и имя.
        assert!(file("B1", "a.pdf", 10, None).same(&file("B1", "x.pdf", 5, Some(3))));
        assert!(file("B1", "A.pdf", 10, None).same(&file("B2", "a.pdf", 10, Some(3))));
        assert!(!file("B1", "a.pdf", 10, None).same(&file("B2", "a.pdf", 11, None)));
    }

    #[test]
    fn who_already_received_and_files_without_repeats() {
        let t = thread();
        // Отчёт — в первых двух письмах (по содержимому): его получали Иван, Пётр и я, Анна — нет.
        assert_eq!(received(&t, &file("Bx", "x", 0, Some(1))), ["ivan@corp.example", "me@corp.example", "petr@corp.example"]);
        assert!(received(&t, &file("Bx", "x", 0, Some(9))).is_empty());
        let s = collect(&t, &[], "note".into()).unwrap();
        assert_eq!(s.known, ["ivan@corp.example", "me@corp.example", "petr@corp.example", "anna@corp.example"]);
        let names: Vec<_> = s.attachments.iter().map(|a| (a.name.as_str(), a.blob_id.as_deref(), a.data.len())).collect();
        assert_eq!(names, [("Отчёт.pdf", Some("B1"), 0), ("Смета.xlsx", Some("B2"), 0)], "отчёт — один раз");
        // Файл, который черновик уже несёт, не повторяется; файлов не осталось — правила нет.
        let carried =
            Attachment { name: "Смета.xlsx".into(), mime: String::new(), data: Vec::new(), inline_cid: None, blob_id: Some("B2".into()) };
        assert_eq!(collect(&t, std::slice::from_ref(&carried), String::new()).unwrap().attachments.len(), 1);
        assert!(collect(&t[2..], &[], String::new()).is_none(), "в беседе нет файлов");
    }

    #[test]
    fn newcomers_are_recipients_absent_from_the_whole_thread() {
        let share = collect(&thread(), &[], "note".into());
        let d = reply(share.clone());
        // Анна была только в письме без вложений — всё равно прежняя; Ольга и скрытый адресат — новые.
        let new: Vec<_> = d.newcomers().iter().map(|a| a.email.as_str()).collect();
        assert_eq!(new, ["olga@corp.example", "boss@corp.example"]);
        // «Ответить» с добавленным вручную — то же правило; никого не добавили — никого.
        let mut r = reply(share.clone());
        r.cc.clear();
        r.bcc.clear();
        assert!(r.newcomers().is_empty());
        r.to.push(Address::new("", "OLGA@corp.example"));
        assert_eq!(r.newcomers().len(), 1);
        // Отключено в черновике или это не ответ — никого.
        let mut off = share.unwrap();
        off.off = true;
        assert!(reply(Some(off)).newcomers().is_empty());
        assert!(reply(None).newcomers().is_empty());
    }

    #[test]
    fn two_copies_same_headers_split_envelopes() {
        let d = reply(collect(&thread(), &[], "note".into()));
        let out = d.outgoing();
        assert_eq!(out.len(), 2);
        let (old, new) = (&out[0], &out[1]);
        assert_eq!(old.rcpt, ["ivan@corp.example", "anna@corp.example"]);
        assert_eq!(new.rcpt, ["olga@corp.example", "boss@corp.example"]);
        assert!(old.keep && !new.keep, "в «Отправленные» — копия прежних участников");
        assert!(old.draft.attachments.is_empty() && new.draft.attachments.len() == 2);
        for c in &out {
            assert_eq!((&c.draft.to, &c.draft.cc, &c.draft.bcc), (&d.to, &d.cc, &d.bcc));
            assert_eq!((&c.draft.subject, &c.draft.in_reply_to, &c.draft.references), (&d.subject, &d.in_reply_to, &d.references));
            assert!(c.draft.share.is_none());
        }
        // Обе копии собираются в письма с одинаковыми заголовками, кроме Message-ID.
        let a = crate::build(&old.draft, 0, "a@corp.example").unwrap();
        let b = crate::build(&new.draft, 0, "b@corp.example").unwrap();
        let (a, b) = (mail_parser::MessageParser::default().parse(&a).unwrap(), mail_parser::MessageParser::default().parse(&b).unwrap());
        assert_eq!((a.subject(), a.in_reply_to().as_text()), (b.subject(), b.in_reply_to().as_text()));
        assert_ne!(a.message_id(), b.message_id());
        assert_eq!((a.attachment_count(), b.attachment_count()), (0, 2));

        // Новых нет — одно письмо всем, как написано.
        let mut plain = d.clone();
        plain.cc.truncate(1);
        plain.bcc.clear();
        let one = plain.outgoing();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].rcpt, ["ivan@corp.example", "anna@corp.example"]);
        assert!(one[0].keep && one[0].draft.attachments.is_empty());
        // Прежних среди адресатов нет — одно письмо с вложениями всем.
        let mut fresh = d.clone();
        fresh.to = vec![Address::new("", "olga@corp.example")];
        fresh.cc.clear();
        fresh.bcc.clear();
        let one = fresh.outgoing();
        assert_eq!((one.len(), one[0].draft.attachments.len(), one[0].keep), (1, 2, true));
        assert!(fresh.attachments_letter().is_none());
    }

    /// EWS: основное письмо — всем из заголовков; новым — отдельное «Вложения из беседы» с той же темой и местом в
    /// беседе, текстом-пояснением и вложениями.
    #[test]
    fn attachments_letter_for_servers_that_deliver_by_headers() {
        let d = reply(collect(&thread(), &[], "Вложения из беседы".into()));
        let extra = d.attachments_letter().unwrap();
        assert_eq!(extra.to, [Address::new("Ольга", "olga@corp.example")]);
        assert!(extra.cc.is_empty());
        assert_eq!(extra.bcc, [Address::new("", "boss@corp.example")], "скрытый — скрытым");
        assert_eq!((&extra.subject, &extra.in_reply_to, &extra.references), (&d.subject, &d.in_reply_to, &d.references));
        assert_eq!(extra.markdown, "Вложения из беседы");
        assert_eq!(extra.attachments.len(), 2);
        assert!(extra.share.is_none());
        let mut off = d.clone();
        off.share.as_mut().unwrap().off = true;
        assert!(off.attachments_letter().is_none());
    }
}
