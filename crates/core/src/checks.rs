//! Проверки «Перед отправкой» (S5-06б, `docs/DESIGN.md` «Новое письмо»): чистые функции без UI, сети и состояния.
//! DLP (что можно отправить) в клиенте нет — это средства защиты на сервере (решение владельца 2026-11-11).
//! Интерфейс зовёт их при изменении текста ([`text`]) и адресатов ([`recipients`]); тексты замечаний — у него.
//!
//! - **Упомянуто вложение, файла нет** — «во вложении», «прилагаю», «attached»… при пустом списке вложений.
//! - **Внешние получатели** при метке «Конфиденциально»: домен не свой (свои — домены адресов отправителя).
//! - **Получатель сменил адрес** — у того же человека (имя из адресной книги) есть адрес новее того, что в письме.
//!
//! Упоминание вложения ищем только в своём тексте — до цитаты (`>`) или пересланного сообщения.

use postman_compose::Address;

use crate::contacts::AddressBook;

/// Замечание перед отправкой.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    /// Упомянуто вложение (найденное слово), а файлов нет.
    MissingAttachment { word: String },
    /// Внешние получатели при метке «Конфиденциально»: сколько и какие домены.
    External { count: usize, domains: Vec<String> },
    /// У получателя `name` есть адрес новее: `from` — в письме, `to` — последний (Unix-секунды `last`).
    Moved { name: String, from: String, to: String, last: i64 },
    /// «Конфиденциально» (S5-12 Ф4): у адресатов (или у себя) нет сертификата шифрования — отправка держится
    /// (решение владельца №6): убрать их или снять метку.
    NoCertificate { names: Vec<String>, addresses: Vec<String> },
    /// «Конфиденциально»: есть адресаты с ГОСТ и с RSA/EC — уйдут два письма (решение владельца 2026-10-22).
    Split { gost: Vec<String> },
}

/// Семейство ключа сертификата шифрования (S5-12 Ф4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Family {
    /// RSA или EC.
    Rsa,
    Gost,
}

/// Замечания «Конфиденциально» по сертификатам: `known(адрес)` — семейства найденных сертификатов (`None` — ещё
/// ищем, замечания по адресу нет). Свой адрес `from` — нужен сертификат каждого семейства, что есть у адресатов.
pub fn certificates(list: &[Address], from: &str, known: &dyn Fn(&str) -> Option<Vec<Family>>) -> Vec<Check> {
    let mut missing: Vec<(String, String)> = Vec::new();
    let mut gost: Vec<String> = Vec::new();
    let mut rsa = false;
    for a in list {
        let name = if a.name.is_empty() { a.email.clone() } else { a.name.clone() };
        match known(&a.email) {
            None => {}
            Some(f) if f.contains(&Family::Rsa) => rsa = true,
            Some(f) if f.contains(&Family::Gost) => gost.push(name),
            Some(_) => missing.push((name, a.email.clone())),
        }
    }
    if let Some(own) = known(from) {
        let need_rsa = rsa || (gost.is_empty() && missing.is_empty());
        if (need_rsa && !own.contains(&Family::Rsa)) || (!gost.is_empty() && !own.contains(&Family::Gost)) {
            missing.push((from.to_owned(), from.to_owned()));
        }
    }
    let mut out = Vec::new();
    if !missing.is_empty() {
        let (names, addresses) = missing.into_iter().unzip();
        out.push(Check::NoCertificate { names, addresses });
    }
    if rsa && !gost.is_empty() {
        out.push(Check::Split { gost });
    }
    out
}

impl Check {
    /// Отправка остановлена, пока замечание не снято: только нет сертификата шифрования (решение владельца №6).
    pub fn blocks_send(&self) -> bool {
        matches!(self, Check::NoCertificate { .. })
    }

    /// Ключ замечания: «Игнорировать» снимает его, пока текст тот же.
    pub fn key(&self) -> String {
        match self {
            Check::MissingAttachment { .. } => "attach".into(),
            Check::External { domains, .. } => format!("external:{}", domains.join(",")),
            Check::NoCertificate { addresses, .. } => format!("nocert:{}", addresses.join(",")),
            Check::Split { gost } => format!("split:{}", gost.join(",")),
            Check::Moved { from, to, .. } => format!("moved:{from}>{to}"),
        }
    }
}

/// Строки своего текста письма: до цитаты (`>`) или пересланного сообщения.
fn own_lines(body: &str) -> impl Iterator<Item = &str> {
    body.lines().take_while(|line| {
        let t = line.trim_start();
        !t.starts_with('>') && !t.starts_with("-------- ")
    })
}

/// Замечания по тексту: «упомянуто вложение». `attachments` — сколько файлов в письме.
pub fn text(body: &str, attachments: usize) -> Vec<Check> {
    let mut out = Vec::new();
    if attachments == 0
        && let Some(word) = own_lines(body).find_map(attachment_word)
    {
        out.push(Check::MissingAttachment { word });
    }
    out
}

/// Замечания по адресатам: внешние при «Конфиденциально», сменившие адрес. `own` — свои домены.
pub fn recipients(list: &[Address], confidential: bool, own: &[String], book: &AddressBook) -> Vec<Check> {
    let mut out = Vec::new();
    if confidential {
        let external: Vec<&Address> = list.iter().filter(|a| is_external(&a.email, own)).collect();
        if !external.is_empty() {
            let mut domains: Vec<String> = external.iter().filter_map(|a| domain(&a.email)).collect();
            domains.sort();
            domains.dedup();
            out.push(Check::External { count: external.len(), domains });
        }
    }
    out.extend(moved(list, book));
    out
}

// --- упоминание вложения ---

/// Начала слов, означающих вложение (RU/EN), в нижнем регистре.
const ATTACH_WORDS: [&str; 13] = [
    "вложени",
    "вложен",
    "вложил",
    "вложу",
    "прилага",
    "приложил",
    "приложен",
    "прикрепл",
    "прикрепил",
    "attach",
    "enclosed",
    "enclosing",
    "enclose",
];

fn attachment_word(line: &str) -> Option<String> {
    let lower = line.to_lowercase();
    let chars: Vec<char> = lower.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let at_start = chars[i].is_alphabetic() && (i == 0 || !chars[i - 1].is_alphabetic());
        if at_start {
            let end = (i..chars.len()).find(|&j| !chars[j].is_alphabetic()).unwrap_or(chars.len());
            let word: String = chars[i..end].iter().collect();
            if ATTACH_WORDS.iter().any(|w| word.starts_with(w)) {
                return Some(word);
            }
            i = end;
        } else {
            i += 1;
        }
    }
    None
}

// --- адресаты ---

/// Домен адреса в нижнем регистре.
pub fn domain(email: &str) -> Option<String> {
    let (_, d) = email.trim().rsplit_once('@')?;
    (!d.is_empty()).then(|| d.to_lowercase())
}

/// Адрес вне своих доменов (свой — домен или его поддомен). Своих доменов не знаем — не внешний.
pub fn is_external(email: &str, own: &[String]) -> bool {
    let Some(d) = domain(email) else { return false };
    !own.is_empty()
        && !own.iter().any(|o| {
            let o = o.to_lowercase();
            d == o || d.ends_with(&format!(".{o}"))
        })
}

/// Имя для сравнения: нижний регистр, «ё» как «е», слова по порядку (Иванов Иван = Иван Иванов).
fn name_key(name: &str) -> String {
    let lower = name.to_lowercase().replace('ё', "е");
    let mut words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    words.sort_unstable();
    words.join(" ")
}

/// Получатели, у которых в адресной книге есть адрес новее: по совпадению имени из двух слов и больше.
fn moved(list: &[Address], book: &AddressBook) -> Vec<Check> {
    let mut out = Vec::new();
    for r in list {
        let known = book.get(&r.email);
        let name = if r.name.trim().is_empty() { known.map_or("", |e| e.name.as_str()) } else { r.name.as_str() };
        let key = name_key(name);
        if key.split(' ').count() < 2 {
            continue;
        }
        let since = known.map_or(0, |e| e.last);
        let newer = book
            .entries()
            .filter(|e| e.last > since && !e.email.eq_ignore_ascii_case(&r.email) && name_key(&e.name) == key)
            .filter(|e| !list.iter().any(|x| x.email.eq_ignore_ascii_case(&e.email)))
            .max_by_key(|e| e.last);
        if let Some(e) = newer {
            out.push(Check::Moved { name: name.trim().to_owned(), from: r.email.clone(), to: e.email.clone(), last: e.last });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contacts::Source;

    fn kinds(checks: &[Check]) -> Vec<&'static str> {
        checks
            .iter()
            .map(|c| match c {
                Check::MissingAttachment { .. } => "attach",
                Check::External { .. } => "external",
                Check::Moved { .. } => "moved",
                Check::NoCertificate { .. } => "nocert",
                Check::Split { .. } => "split",
            })
            .collect()
    }

    #[test]
    fn mentioned_attachment_without_files() {
        assert_eq!(text("Отчёт во вложении.", 0), [Check::MissingAttachment { word: "вложении".into() }]);
        assert_eq!(kinds(&text("Прилагаю расчёт стоимости", 0)), ["attach"]);
        assert_eq!(kinds(&text("Please see the attached file", 0)), ["attach"]);
        assert!(text("Отчёт во вложении.", 1).is_empty(), "файл есть");
        assert!(text("Привет!\n\n> во вложении договор", 0).is_empty(), "в цитате");
        let forward = "Смотри ниже.\n\n-------- Пересланное сообщение --------  \nДоговор во вложении";
        assert!(text(forward, 0).is_empty(), "в пересланном");
        assert!(text("Разложил по папкам", 0).is_empty(), "не начало слова");
    }

    #[test]
    fn external_recipients_only_with_the_confidential_mark() {
        let book = AddressBook::default();
        let own = vec!["corp.example".to_string()];
        let list = [
            Address::new("", "anna@corp.example"),
            Address::new("", "petr@msk.corp.example"),
            Address::new("", "s@partner.example"),
            Address::new("", "i@partner.example"),
            Address::new("", "x@other.example"),
        ];
        assert!(recipients(&list, false, &own, &book).is_empty());
        let got = recipients(&list, true, &own, &book);
        assert_eq!(got, [Check::External { count: 3, domains: vec!["other.example".into(), "partner.example".into()] }]);
        assert!(!got[0].blocks_send());
        assert!(!is_external("a@b.example", &[]), "своих доменов не знаем");
    }

    #[test]
    fn recipient_with_a_newer_address() {
        let mut book = AddressBook::default();
        book.touch("Ирина Смирнова", "i.smirnova@partner.example", Source::Recent, 1, 1_000);
        book.touch("Смирнова Ирина", "irina.s@partner.example", Source::Recent, 1, 2_000);
        book.touch("Ирина", "irina@other.example", Source::Recent, 1, 3_000);
        let list = [Address::new("", "i.smirnova@partner.example")];
        assert_eq!(
            recipients(&list, false, &[], &book),
            [Check::Moved {
                name: "Ирина Смирнова".into(),
                from: "i.smirnova@partner.example".into(),
                to: "irina.s@partner.example".into(),
                last: 2_000
            }]
        );
        // Новый адрес — уже последний; или оба в письме; или имя из одного слова — замечания нет.
        assert!(recipients(&[Address::new("", "irina.s@partner.example")], false, &[], &book).is_empty());
        let both = [Address::new("", "i.smirnova@partner.example"), Address::new("", "irina.s@partner.example")];
        assert!(recipients(&both, false, &[], &book).is_empty());
        assert!(recipients(&[Address::new("Ирина", "irina@new.example")], false, &[], &book).is_empty());
    }

    /// «Конфиденциально» (S5-12 Ф4): нет сертификата — держит отправку; ГОСТ и RSA — два письма; себе — нужное
    /// семейство; ещё ищем — молчим.
    #[test]
    fn certificate_checks() {
        let a = |e: &str| Address::new("", e);
        let known = |e: &str| -> Option<Vec<Family>> {
            match e {
                "rsa@x" => Some(vec![Family::Rsa]),
                "gost@x" => Some(vec![Family::Gost]),
                "both@x" | "me@x" => Some(vec![Family::Rsa, Family::Gost]),
                "me-rsa@x" => Some(vec![Family::Rsa]),
                "pending@x" => None,
                _ => Some(vec![]),
            }
        };
        assert!(certificates(&[a("rsa@x"), a("both@x"), a("pending@x")], "me@x", &known).is_empty());
        let c = certificates(&[a("rsa@x"), a("none@x")], "me@x", &known);
        assert!(matches!(&c[..], [Check::NoCertificate { addresses, .. }] if addresses == &["none@x".to_string()]));
        assert!(c[0].blocks_send());
        let c = certificates(&[a("rsa@x"), a("gost@x")], "me@x", &known);
        assert!(matches!(&c[..], [Check::Split { gost }] if gost == &["gost@x".to_string()]));
        assert!(!c[0].blocks_send());
        let c = certificates(&[a("gost@x")], "me-rsa@x", &known);
        assert!(matches!(&c[..], [Check::NoCertificate { addresses, .. }] if addresses == &["me-rsa@x".to_string()]), "себе ГОСТ нет");
    }
}
