//! Сборка vCard 4.0 (RFC 6350) для CardDAV (S7-05б): новая карточка из полей «Контактов» и правка своей карточки на
//! своём сервере построчно — поля Postman заменяются, остальное (адреса, фото, заметки других клиентов) остаётся.
//! Разбор чужих .vcf — только в воркере.

/// Поля карточки, которые показывает и правит Postman («Контакты», S4-02).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Card<'a> {
    pub uid: &'a str,
    pub name: &'a str,
    pub emails: &'a [String],
    pub phones: &'a [String],
    pub organization: &'a str,
    /// Должность.
    pub title: &'a str,
}

/// Свойства, которыми владеет Postman: при правке они пишутся заново.
const OWN: &[&str] = &["FN", "N", "EMAIL", "TEL", "ORG", "TITLE"];

/// Экранирование TEXT (RFC 6350 §3.4); `component` — ещё и `;` (части N и ORG).
fn text(s: &str) -> String {
    s.trim().replace('\\', "\\\\").replace(',', "\\,").replace(';', "\\;").replace("\r\n", "\\n").replace('\n', "\\n")
}

/// Перенос строк длиннее 75 октетов (RFC 6350 §3.2), не разрывая UTF-8.
fn fold(line: &str) -> String {
    let mut out = String::new();
    let mut len = 0;
    for c in line.chars() {
        let w = c.len_utf8();
        if len + w > 75 {
            out.push_str("\r\n ");
            len = 1;
        }
        out.push(c);
        len += w;
    }
    out
}

fn own_lines(c: &Card<'_>) -> Vec<String> {
    let name = c.name.trim();
    let mut l = vec![format!("FN:{}", text(if name.is_empty() { c.emails.first().map_or("", |e| e.as_str()) } else { name }))];
    // N обязателен в vCard 3.0, в 4.0 — нет; имя целиком — одним полем (как JMAP `name.full`, `docs/STALWART.md` п. 8).
    l.push(format!("N:;{};;;", text(name)));
    for (i, e) in c.emails.iter().map(|e| e.trim()).filter(|e| !e.is_empty()).enumerate() {
        l.push(if i == 0 { format!("EMAIL;PREF=1:{}", text(e)) } else { format!("EMAIL:{}", text(e)) });
    }
    for p in c.phones.iter().map(|p| p.trim()).filter(|p| !p.is_empty()) {
        l.push(format!("TEL:{}", text(p)));
    }
    if !c.organization.trim().is_empty() {
        l.push(format!("ORG:{}", text(c.organization)));
    }
    if !c.title.trim().is_empty() {
        l.push(format!("TITLE:{}", text(c.title)));
    }
    l
}

fn join(lines: &[String]) -> String {
    lines.iter().map(|x| fold(x)).collect::<Vec<_>>().join("\r\n") + "\r\n"
}

/// Новая карточка.
pub fn build(c: &Card<'_>) -> String {
    let mut l = vec!["BEGIN:VCARD".to_string(), "VERSION:4.0".into(), "PRODID:-//Postman//Postman//RU".into(), format!("UID:{}", text(c.uid))];
    l.extend(own_lines(c));
    l.push("END:VCARD".into());
    join(&l)
}

/// Своя карточка `original` с полями `c`: строки FN, N, EMAIL, TEL, ORG, TITLE — заново (в конце карточки), прочие
/// свойства и их порядок — как были. Первой карточки в тексте нет — новая.
pub fn patch(original: &str, c: &Card<'_>) -> String {
    let mut lines: Vec<String> = Vec::new();
    for raw in original.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(rest) = line.strip_prefix([' ', '\t'])
            && let Some(last) = lines.last_mut()
        {
            last.push_str(rest);
            continue;
        }
        if !line.is_empty() {
            lines.push(line.to_owned());
        }
    }
    let Some(end) = lines.iter().position(|l| l.eq_ignore_ascii_case("END:VCARD")) else { return build(c) };
    let name = |l: &str| -> String {
        let head = l.split([':', ';']).next().unwrap_or_default();
        // Группа (`item1.EMAIL`) — имя после точки.
        head.rsplit('.').next().unwrap_or(head).to_ascii_uppercase()
    };
    let mut out: Vec<String> = lines[..end].iter().filter(|l| !OWN.contains(&name(l).as_str())).cloned().collect();
    out.extend(own_lines(c));
    out.extend(lines[end..].iter().cloned());
    join(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card<'a>(emails: &'a [String], phones: &'a [String]) -> Card<'a> {
        Card {
            uid: "c1", name: "Иван Петров", emails, phones, organization: "Рога и копыта", title: "Директор, отдел"
        }
    }

    #[test]
    fn new_card_has_postman_fields() {
        let emails = vec!["ivan@example.test".to_string(), " ".into(), "i@example.test".into()];
        let v = build(&card(&emails, &["+7 900".into()]));
        assert!(v.starts_with("BEGIN:VCARD\r\nVERSION:4.0\r\n") && v.ends_with("END:VCARD\r\n"));
        assert!(v.contains("UID:c1\r\nFN:Иван Петров\r\nN:;Иван Петров;;;\r\n"));
        assert!(v.contains("EMAIL;PREF=1:ivan@example.test\r\nEMAIL:i@example.test\r\nTEL:+7 900\r\n"));
        assert!(v.contains("ORG:Рога и копыта\r\nTITLE:Директор\\, отдел\r\n"));
        let nameless = build(&Card { name: "", ..card(&emails, &[]) });
        assert!(nameless.contains("FN:ivan@example.test\r\n"));
    }

    #[test]
    fn edit_keeps_other_properties() {
        let original = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:c1\r\nFN:Старое\r\nN:Петров;Иван;;;\r\nitem1.EMAIL;TYPE=work:old@x\r\nADR:;;ул. Ленина\\, 1;;;;\r\nNOTE:длинная заметка, которая не помещается в одну строку и поэтому свёрнута сер\r\n verом\r\nTEL:1\r\nEND:VCARD\r\n";
        let emails = vec!["new@x".to_string()];
        let v = patch(original, &card(&emails, &[]));
        assert!(v.contains("VERSION:3.0\r\nUID:c1\r\nADR:;;ул. Ленина\\, 1;;;;\r\n"), "{v}");
        assert!(!v.contains("old@x") && !v.contains("Старое") && !v.contains("TEL:1"));
        assert!(v.replace("\r\n ", "").contains("свёрнута серverом"), "{v}");
        assert!(v.contains("FN:Иван Петров\r\n") && v.contains("EMAIL;PREF=1:new@x\r\n") && v.ends_with("END:VCARD\r\n"));
        assert_eq!(patch("garbage", &card(&emails, &[])), build(&card(&emails, &[])));
    }
}
