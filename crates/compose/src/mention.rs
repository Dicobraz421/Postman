//! @упоминания в тексте письма (решение владельца `docs/decisions/2026-11-14-messenger-comforts.md`): набранное `@…` у
//! курсора — запрос подсказки, выбранный человек — `**@Имя**` (выделен в письме полужирным). Признак «упомянули
//! меня» у пришедшего письма — `postman_core::filters::Me::mentioned`. Без I/O.

/// Знаков запроса не больше (дальше `@` — уже не упоминание).
const QUERY_MAX: usize = 40;

/// Запрос упоминания у курсора `cursor` (байт текста): начало `@` и набранное после него. `@` — в начале строки или
/// после пробела, скобки, кавычки (адрес `a@b` — не упоминание); запрос — буквы, цифры, `.`, `-`, `_`, не больше
/// двух слов, в одной строке.
pub fn query_at(text: &str, cursor: usize) -> Option<(usize, String)> {
    let mut cursor = cursor.min(text.len());
    while !text.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let before = &text[..cursor];
    let at = before.rfind('@')?;
    let lead = before[..at].chars().next_back();
    if lead.is_some_and(|c| !(c.is_whitespace() || "([{\"'«*_".contains(c))) {
        return None;
    }
    let q = &before[at + 1..];
    let ok = |c: char| c.is_alphanumeric() || matches!(c, ' ' | '.' | '-' | '_');
    if q.starts_with(' ') || q.contains("  ") || q.chars().count() > QUERY_MAX || !q.chars().all(ok) || q.split(' ').count() > 2 {
        return None;
    }
    Some((at, q.to_owned()))
}

/// Подходит ли человек под запрос: начало имени, любого слова имени или адреса (без учёта регистра). Пустой
/// запрос — подходят все.
pub fn matches(name: &str, email: &str, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    let name = name.trim().to_lowercase();
    name.starts_with(&q) || name.split_whitespace().any(|w| w.starts_with(&q)) || email.to_lowercase().starts_with(&q)
}

/// Как человек стоит в тексте: имя, без имени — адрес до `@`.
pub fn shown(name: &str, email: &str) -> String {
    let name = name.trim();
    if name.is_empty() { email.split('@').next().unwrap_or(email).to_owned() } else { name.to_owned() }
}

/// Вставить упоминание вместо набранного `@…` (`start..cursor`): `**@Имя** `; текст и курсор после него.
pub fn insert(text: &str, start: usize, cursor: usize, name: &str) -> (String, usize) {
    let (start, cursor) = (start.min(text.len()), cursor.min(text.len()).max(start.min(text.len())));
    let token = format!("**@{}** ", name.trim());
    let rest = text[cursor..].strip_prefix(' ').unwrap_or(&text[cursor..]);
    (format!("{}{token}{rest}", &text[..start]), start + token.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_at_cursor() {
        let t = "Коллеги, @Ан";
        assert_eq!(query_at(t, t.len()), Some((t.find('@').unwrap(), "Ан".into())));
        assert_eq!(query_at("@", 1), Some((0, String::new())));
        let two = "ok (@Анна См";
        assert_eq!(query_at(two, two.len()).map(|q| q.1), Some("Анна См".into()));
        let end = |t: &str| query_at(t, t.len());
        assert_eq!(end("пишите a@b"), None, "адрес — не упоминание");
        assert_eq!(end("@Анна Смирнова Иванова"), None, "три слова — уже текст");
        assert_eq!(end("@Анна,"), None);
        assert_eq!(end("@Анна\nдальше"), None);
    }

    #[test]
    fn match_and_insert() {
        assert!(matches("Анна Смирнова", "anna@corp.example", "смир"));
        assert!(matches("Анна Смирнова", "anna@corp.example", "Анна С"));
        assert!(matches("", "anna@corp.example", "ann"));
        assert!(!matches("Анна Смирнова", "anna@corp.example", "иван"));
        assert_eq!(shown("", "anna.s@corp.example"), "anna.s");
        let t = "Привет, @Ан и всем";
        let start = t.find('@').unwrap();
        let (out, at) = insert(t, start, start + "@Ан".len(), "Анна Смирнова");
        assert_eq!(out, "Привет, **@Анна Смирнова** и всем");
        assert_eq!(&out[..at], "Привет, **@Анна Смирнова** ");
    }
}
