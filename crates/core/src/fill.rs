//! Поля в фигурных скобках (T2-52, макет `SettingsSign`): `{имя}`, `{должность}`, `{телефон}` подписи — из профиля
//! учётной записи; свои поля шаблона (`{Компания}`, `{Дата}`) — значения спрашиваются при вставке или остаются метками.
//! Шаблон по команде: в тексте письма набрано `/кп` и пробел — шаблон с командой `кп` (или единственный, чьё
//! название так начинается). Чистый модуль.

use crate::prefs::{Profile, Template};

/// Поля профиля: ключи (русский и английский) и значение.
fn profile_keys(p: &Profile) -> [(&'static [&'static str], &str); 3] {
    [(&["имя", "name"], p.name.as_str()), (&["должность", "title"], p.title.as_str()), (&["телефон", "phone"], p.phone.as_str())]
}

/// Поле профиля (`имя`, `title`…).
pub fn is_profile(key: &str) -> bool {
    let k = key.trim().to_lowercase();
    profile_keys(&Profile::default()).iter().any(|(keys, _)| keys.contains(&k.as_str()))
}

/// Поля `{…}` в тексте по порядку, без повторов (без учёта регистра). Поле — буквы, цифры, пробел, `-`, `_`.
pub fn fields(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else { break };
        let key = &after[..close];
        let ok = !key.trim().is_empty() && key.chars().all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'));
        if ok && !out.iter().any(|k| k.to_lowercase() == key.to_lowercase()) {
            out.push(key.to_owned());
        }
        rest = if ok { &after[close + 1..] } else { after };
    }
    out
}

/// Свои поля шаблона — без полей профиля (их значения известны).
pub fn own_fields(text: &str) -> Vec<String> {
    fields(text).into_iter().filter(|k| !is_profile(k)).collect()
}

/// Подставить значения: `{ключ}` (без учёта регистра) → значение; пустое значение и неизвестное поле — метка остаётся.
pub fn fill(text: &str, values: &[(String, String)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let key = &after[..close];
        match values.iter().find(|(k, v)| k.to_lowercase() == key.to_lowercase() && !v.trim().is_empty()) {
            Some((_, v)) => out.push_str(v),
            None => {
                out.push('{');
                out.push_str(key);
                out.push('}');
            }
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Значения полей профиля под всеми их ключами.
pub fn profile_values(p: &Profile) -> Vec<(String, String)> {
    profile_keys(p).iter().flat_map(|(keys, v)| keys.iter().map(move |k| ((*k).to_owned(), (*v).to_owned()))).collect()
}

/// Подпись с полями профиля.
pub fn signature(text: &str, p: &Profile) -> String {
    fill(text, &profile_values(p))
}

/// Команда шаблона в конце текста: `/кп` и пробел или перевод строки; `/` — в начале или после пробела. Ответ — где
/// начинается `/` (байт) и набранное слово.
pub fn slash_at_end(text: &str) -> Option<(usize, String)> {
    let body = text.strip_suffix([' ', '\n'])?;
    let start = body.rfind('/')?;
    let word = &body[start + 1..];
    let before_ok = body[..start].chars().next_back().is_none_or(char::is_whitespace);
    (before_ok && !word.is_empty() && !word.contains(char::is_whitespace)).then(|| (start, word.to_owned()))
}

/// Шаблон по набранному слову: точная команда, иначе единственный шаблон, чьё название начинается с него (2+ знака).
pub fn by_command<'a>(templates: &'a [Template], typed: &str) -> Option<&'a Template> {
    let t = typed.trim_start_matches('/').to_lowercase();
    if t.is_empty() {
        return None;
    }
    if let Some(x) = templates.iter().find(|x| x.command.trim_start_matches('/').to_lowercase() == t) {
        return Some(x);
    }
    let mut by_name = templates.iter().filter(|x| t.chars().count() >= 2 && x.name.to_lowercase().starts_with(&t));
    match (by_name.next(), by_name.next()) {
        (Some(x), None) => Some(x),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tpl(name: &str, command: &str) -> Template {
        Template { id: name.into(), name: name.into(), command: command.into(), ..Default::default() }
    }

    #[test]
    fn fields_and_fill() {
        let text = "Для {Компания} по итогам {Дата}, {компания}. {имя} {} {a.b} {Дата";
        assert_eq!(fields(text), ["Компания", "Дата", "имя"]);
        assert_eq!(own_fields(text), ["Компания", "Дата"]);
        let v = vec![("компания".to_owned(), "Вектор".to_owned()), ("Дата".to_owned(), " ".to_owned())];
        assert_eq!(fill("Для {Компания} — {Дата}.", &v), "Для Вектор — {Дата}.", "пустое значение — метка остаётся");
        assert_eq!(fill("{x", &v), "{x");
        let p = Profile { name: "Антон Антропов".into(), title: "Технический директор".into(), phone: String::new() };
        assert_eq!(signature("{имя}, {Должность}\n{телефон} {title}", &p), "Антон Антропов, Технический директор\n{телефон} Технический директор");
    }

    #[test]
    fn slash_command() {
        assert_eq!(slash_at_end("Добрый день!\n/кп "), Some((23, "кп".into())));
        assert_eq!(slash_at_end("/кп\n"), Some((0, "кп".into())));
        assert_eq!(slash_at_end("и/или "), None, "слеш внутри слова");
        assert_eq!(slash_at_end("/кп"), None, "ещё набирают");
        assert_eq!(slash_at_end("/ "), None);
        let list = vec![tpl("Коммерческое предложение", "кп"), tpl("Приглашение на созвон", "созвон"), tpl("Приветствие", "")];
        assert_eq!(by_command(&list, "/КП").map(|t| t.id.as_str()), Some("Коммерческое предложение"));
        assert_eq!(by_command(&list, "ком").map(|t| t.id.as_str()), Some("Коммерческое предложение"));
        assert_eq!(by_command(&list, "при"), None, "два названия — неоднозначно");
        assert_eq!(by_command(&list, "к"), None, "одна буква — мало");
        assert_eq!(by_command(&list, "созвон").map(|t| t.id.as_str()), Some("Приглашение на созвон"));
    }
}
