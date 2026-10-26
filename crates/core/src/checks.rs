//! Проверки «Перед отправкой» (S5-06б, `docs/DESIGN.md` «Новое письмо»): чистые функции без UI, сети и состояния.
//! Интерфейс зовёт их при изменении текста ([`text`]) и адресатов ([`recipients`]); тексты замечаний — у него.
//!
//! - **DLP** — номер платёжной карты (13–19 цифр подряд или группами через пробел/дефис, первая 2–6, сумма Луна) или
//!   явный секрет: закрытый ключ PEM, токен известного вида (AWS, GitHub, GitLab, Slack, ключи `sk-`), пароль после
//!   «пароль:» / `password=`. Блокирует отправку ([`Check::blocks_send`]), пока текст не замаскирован ([`mask`]) или
//!   замечание не снято («Это не карта»).
//! - **Упомянуто вложение, файла нет** — «во вложении», «прилагаю», «attached»… при пустом списке вложений.
//! - **Внешние получатели** при метке «Конфиденциально»: домен не свой (свои — домены адресов отправителя).
//! - **Получатель сменил адрес** — у того же человека (имя из адресной книги) есть адрес новее того, что в письме.
//!
//! Цитата (`>`) — чужой текст: в ней не ищем; упоминание вложения ищем только в своём тексте — до цитаты или
//! пересланного сообщения.

use std::ops::Range;

use postman_compose::Address;

use crate::contacts::AddressBook;

/// Вид секрета.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Secret {
    /// Закрытый ключ PEM (`-----BEGIN … PRIVATE KEY-----`).
    PrivateKey,
    /// Токен доступа известного вида.
    Token,
    /// Пароль после «пароль:» / `password=`.
    Password,
}

/// Замечание перед отправкой.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    /// Похоже на номер карты: место в тексте (байты) и найденный текст.
    Card { range: Range<usize>, text: String },
    /// Похоже на секрет.
    Secret { range: Range<usize>, text: String, kind: Secret },
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
    /// Отправка остановлена, пока замечание не снято: только DLP.
    pub fn blocks_send(&self) -> bool {
        matches!(self, Check::Card { .. } | Check::Secret { .. } | Check::NoCertificate { .. })
    }

    /// Ключ замечания: «Игнорировать» и «Это не карта» снимают его, пока текст тот же.
    pub fn key(&self) -> String {
        match self {
            Check::Card { text, .. } => format!("card:{}", text.chars().filter(char::is_ascii_digit).collect::<String>()),
            Check::Secret { text, .. } => format!("secret:{text}"),
            Check::MissingAttachment { .. } => "attach".into(),
            Check::External { domains, .. } => format!("external:{}", domains.join(",")),
            Check::NoCertificate { addresses, .. } => format!("nocert:{}", addresses.join(",")),
            Check::Split { gost } => format!("split:{}", gost.join(",")),
            Check::Moved { from, to, .. } => format!("moved:{from}>{to}"),
        }
    }
}

/// Строки своего текста письма: смещение и строка; цитата (`>`) пропускается. `stop_at_forward` — закончить на
/// пересланном сообщении или строке перед цитатой.
fn own_lines(body: &str, stop_at_forward: bool) -> impl Iterator<Item = (usize, &str)> {
    let mut offset = 0;
    let mut stopped = false;
    body.split_inclusive('\n').filter_map(move |raw| {
        let start = offset;
        offset += raw.len();
        let line = raw.trim_end_matches(['\n', '\r']);
        if stopped {
            return None;
        }
        let t = line.trim_start();
        if t.starts_with('>') {
            if stop_at_forward {
                stopped = true;
            }
            return None;
        }
        if stop_at_forward && t.starts_with("-------- ") {
            stopped = true;
            return None;
        }
        Some((start, line))
    })
}

/// Замечания по тексту: DLP и «упомянуто вложение». `attachments` — сколько файлов в письме.
pub fn text(body: &str, attachments: usize) -> Vec<Check> {
    let mut out = Vec::new();
    for (start, line) in own_lines(body, false) {
        cards(line, start, &mut out);
        tokens(line, start, &mut out);
        passwords(line, start, &mut out);
    }
    private_keys(body, &mut out);
    out.sort_by_key(|c| match c {
        Check::Card { range, .. } | Check::Secret { range, .. } => range.start,
        _ => usize::MAX,
    });
    if attachments == 0
        && let Some(word) = own_lines(body, true).find_map(|(_, line)| attachment_word(line))
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

// --- карты ---

/// Байт «часть слова»: буква, цифра, `_`, не-ASCII (кириллица, «•» маски).
fn wordish(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Сумма Луна по цифрам (значения 0–9).
pub fn luhn(digits: &[u8]) -> bool {
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &d)| {
            let d = u32::from(d);
            if i % 2 == 1 {
                let x = d * 2;
                if x > 9 { x - 9 } else { x }
            } else {
                d
            }
        })
        .sum();
    !digits.is_empty() && sum.is_multiple_of(10)
}

fn cards(line: &str, base: usize, out: &mut Vec<Check>) {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_digit() || (i > 0 && wordish(b[i - 1])) {
            i += 1;
            continue;
        }
        let start = i;
        let mut digits = Vec::new();
        let mut j = i;
        let mut end = i;
        while j < b.len() {
            if b[j].is_ascii_digit() {
                digits.push(b[j] - b'0');
                j += 1;
                end = j;
            } else if (b[j] == b' ' || b[j] == b'-') && b.get(j + 1).is_some_and(u8::is_ascii_digit) {
                j += 1;
            } else {
                break;
            }
        }
        let bounded = b.get(end).is_none_or(|&c| !wordish(c));
        let card = bounded && (13..=19).contains(&digits.len()) && (2..=6).contains(&digits[0]) && luhn(&digits);
        if card {
            out.push(Check::Card { range: base + start..base + end, text: line[start..end].to_owned() });
        }
        i = end.max(i + 1);
    }
}

// --- секреты ---

fn token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// Токен известного вида.
fn is_token(w: &str) -> bool {
    let rest_upper = |s: &str| s.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    (w.len() == 20 && w.starts_with("AKIA") && rest_upper(&w[4..]))
        || (w.len() >= 40 && ["ghp_", "gho_", "ghu_", "ghs_", "ghr_"].iter().any(|p| w.starts_with(p)))
        || (w.len() >= 40 && w.starts_with("github_pat_"))
        || (w.len() >= 26 && w.starts_with("glpat-"))
        || (w.len() >= 15 && ["xoxb-", "xoxp-", "xoxa-", "xoxr-", "xoxs-"].iter().any(|p| w.starts_with(p)))
        || (w.len() >= 32 && w.starts_with("sk-"))
        || (w.len() >= 24 && w.starts_with("sk_live_"))
}

fn tokens(line: &str, base: usize, out: &mut Vec<Check>) {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if !token_byte(b[i]) || (i > 0 && wordish(b[i - 1])) {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && token_byte(b[i]) {
            i += 1;
        }
        let bounded = b.get(i).is_none_or(|&c| c < 0x80);
        let w = &line[start..i];
        if bounded && is_token(w) {
            out.push(Check::Secret { range: base + start..base + i, text: w.to_owned(), kind: Secret::Token });
        }
    }
}

/// Пароль после «пароль:», `password=`, `passwd:`, `pwd:`.
fn passwords(line: &str, base: usize, out: &mut Vec<Check>) {
    const WORDS: [&str; 4] = ["password", "passwd", "pwd", "пароль"];
    let lower = line.to_lowercase();
    // Смещения строки и её нижнего регистра совпадают только при той же длине (ASCII, кириллица).
    if lower.len() != line.len() {
        return;
    }
    for w in WORDS {
        let mut from = 0;
        while let Some(pos) = lower[from..].find(w).map(|p| p + from) {
            from = pos + w.len();
            if pos > 0 && lower[..pos].chars().next_back().is_some_and(char::is_alphanumeric) {
                continue;
            }
            let after = &line[from..];
            let rest = after.trim_start();
            let Some(sep) = rest.chars().next().filter(|c| *c == ':' || *c == '=') else { continue };
            let value = rest[sep.len_utf8()..].trim_start();
            let len = value.find(char::is_whitespace).unwrap_or(value.len());
            if value[..len].chars().count() < 4 {
                continue;
            }
            let start = line.len() - value.len();
            out.push(Check::Secret { range: base + start..base + start + len, text: value[..len].to_owned(), kind: Secret::Password });
        }
    }
}

fn private_keys(body: &str, out: &mut Vec<Check>) {
    let mut from = 0;
    while let Some(start) = body[from..].find("-----BEGIN ").map(|p| p + from) {
        let head_end = body[start + 11..].find("-----").map(|p| p + start + 11);
        let Some(head_end) = head_end else { break };
        let label = &body[start + 11..head_end];
        from = head_end + 5;
        if !label.ends_with("PRIVATE KEY") {
            continue;
        }
        let end = body[from..].find("-----END ").and_then(|p| body[from + p + 9..].find("-----").map(|q| from + p + 9 + q + 5)).unwrap_or(body.len());
        out.push(Check::Secret { range: start..end, text: body[start..end].to_owned(), kind: Secret::PrivateKey });
        from = end;
    }
}

/// Замаскировать найденное: у карты остаются первые и последние 4 цифры, у токена — первые 4 знака, у пароля —
/// ничего, у ключа — строки BEGIN и END. Текст изменился с тех пор (места нет) — без изменений.
pub fn mask(body: &str, check: &Check) -> String {
    let (range, text) = match check {
        Check::Card { range, text } | Check::Secret { range, text, .. } => (range, text),
        _ => return body.to_owned(),
    };
    if body.get(range.clone()) != Some(text.as_str()) {
        return body.to_owned();
    }
    let masked: String = match check {
        Check::Card { .. } => {
            let n = text.chars().filter(char::is_ascii_digit).count();
            let mut k = 0;
            text.chars()
                .map(|c| {
                    if !c.is_ascii_digit() {
                        return c;
                    }
                    k += 1;
                    if k <= 4 || k > n.saturating_sub(4) { c } else { '•' }
                })
                .collect()
        }
        Check::Secret { kind: Secret::PrivateKey, .. } => {
            let first = text.find('\n').unwrap_or(text.len());
            let last = text.rfind('\n').filter(|&l| l > first).unwrap_or(text.len());
            let middle: String = text[first..last].chars().map(|c| if c.is_whitespace() { c } else { '•' }).collect();
            format!("{}{middle}{}", &text[..first], &text[last..])
        }
        Check::Secret { kind, .. } => {
            let keep = if *kind == Secret::Password { 0 } else { 4 };
            text.chars().enumerate().map(|(i, c)| if i < keep || c.is_whitespace() { c } else { '•' }).collect()
        }
        _ => return body.to_owned(),
    };
    format!("{}{masked}{}", &body[..range.start], &body[range.end..])
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
                Check::Card { .. } => "card",
                Check::Secret { kind: Secret::PrivateKey, .. } => "key",
                Check::Secret { kind: Secret::Token, .. } => "token",
                Check::Secret { kind: Secret::Password, .. } => "password",
                Check::MissingAttachment { .. } => "attach",
                Check::External { .. } => "external",
                Check::Moved { .. } => "moved",
                Check::NoCertificate { .. } => "nocert",
                Check::Split { .. } => "split",
            })
            .collect()
    }

    #[test]
    fn luhn_sum() {
        let d = |s: &str| s.bytes().map(|b| b - b'0').collect::<Vec<u8>>();
        assert!(luhn(&d("4111111111111111")));
        assert!(luhn(&d("5500005555555559")));
        assert!(!luhn(&d("4111111111111112")));
        assert!(!luhn(&[]));
    }

    #[test]
    fn card_numbers_are_found_and_masked() {
        let body = "Реквизиты: карта 4111 1111 1111 1111, срок 12/28.\nТелефон +7 916 123-45-67, счёт 40702810900000001234.";
        let checks = text(body, 1);
        assert_eq!(kinds(&checks), ["card"]);
        let Check::Card { range, text: t } = &checks[0] else { unreachable!() };
        assert_eq!(&body[range.clone()], "4111 1111 1111 1111");
        assert_eq!(t, "4111 1111 1111 1111");
        let masked = mask(body, &checks[0]);
        assert!(masked.contains("4111 •••• •••• 1111"), "{masked}");
        assert!(super::text(&masked, 1).is_empty(), "замаскированное — не карта");
        // Слитно, дефисами; неверная сумма, первая цифра не 2–6, часть слова — не карта.
        assert_eq!(kinds(&text("5500-0055-5555-5559", 1)), ["card"]);
        assert_eq!(kinds(&text("4111111111111111", 1)), ["card"]);
        assert!(text("4111 1111 1111 1112", 1).is_empty());
        assert!(text("1111 1111 1111 1117", 1).is_empty(), "первая цифра 1");
        assert!(text("id4111111111111111", 1).is_empty());
        assert!(text("4276 38•• •••• 1295", 1).is_empty(), "уже замаскировано");
        // Цитата — чужой текст.
        assert!(text("> 4111 1111 1111 1111", 1).is_empty());
        // Текст изменился — маска ничего не портит.
        assert_eq!(mask("другой текст", &checks[0]), "другой текст");
        assert!(checks[0].blocks_send());
        assert_eq!(checks[0].key(), "card:4111111111111111");
    }

    #[test]
    fn secrets_are_found_and_masked() {
        let body = "Доступ: пароль: Qwerty123!\nkey AKIAIOSFODNN7EXAMPLE и ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n";
        let checks = text(body, 1);
        assert_eq!(kinds(&checks), ["password", "token", "token"]);
        let once = mask(body, &checks[0]);
        assert!(once.contains("пароль: ••••••••••"), "{once}");
        let Check::Secret { text: t, .. } = &checks[1] else { unreachable!() };
        assert_eq!(t, "AKIAIOSFODNN7EXAMPLE");
        assert!(mask(body, &checks[1]).contains("AKIA••••••••••••••••"));
        assert!(text("password = abc", 1).is_empty(), "короче 4 знаков");
        assert!(text("Сменил пароль вчера", 1).is_empty(), "без двоеточия — не пароль");
        let pem = "Ключ:\n-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA\nabcd\n-----END RSA PRIVATE KEY-----\nвсё";
        let checks = text(pem, 1);
        assert_eq!(kinds(&checks), ["key"]);
        let masked = mask(pem, &checks[0]);
        assert!(masked.contains("-----BEGIN RSA PRIVATE KEY-----\n••••••••••••••••\n••••\n-----END RSA PRIVATE KEY-----"), "{masked}");
        assert!(text("-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----", 1).is_empty(), "сертификат — не секрет");
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
