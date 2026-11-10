//! Файл `keymap.toml` (T2-55, макет `SettingsKeys`): «Экспорт» и «Импорт» своих сочетаний — схема и сочетания обеих
//! схем поверх реестра команд. Свой узкий формат TOML (без зависимостей):
//!
//! ```toml
//! scheme = "outlook"
//! [outlook]
//! "mail.toggle_flag" = ["ctrl-g"]
//! [single]
//! "mail.archive" = ["y"]
//! ```
//!
//! Разбор — только этого вида; прочее — ошибка с номером строки. Чистый модуль.

use std::collections::BTreeMap;

use crate::prefs::KeyScheme;

/// Свои сочетания: схема и клавиши команд в каждой схеме.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Keymap {
    pub scheme: KeyScheme,
    pub outlook: BTreeMap<String, Vec<String>>,
    pub single: BTreeMap<String, Vec<String>>,
}

/// Ошибка разбора: номер строки (с 1).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("keymap.toml, line {0}")]
pub struct Bad(pub usize);

fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        if matches!(c, '"' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

fn scheme_id(s: KeyScheme) -> &'static str {
    match s {
        KeyScheme::Outlook => "outlook",
        KeyScheme::Single => "single",
    }
}

/// Текст файла.
pub fn write(k: &Keymap) -> String {
    let mut out = String::from("# Postman keymap\n");
    out.push_str(&format!("scheme = {}\n", quote(scheme_id(k.scheme))));
    for (name, map) in [("outlook", &k.outlook), ("single", &k.single)] {
        out.push_str(&format!("\n[{name}]\n"));
        for (id, keys) in map {
            let list: Vec<String> = keys.iter().map(|x| quote(x)).collect();
            out.push_str(&format!("{} = [{}]\n", quote(id), list.join(", ")));
        }
    }
    out
}

/// Строка в кавычках в начале `s` → (значение, остаток).
fn unquote(s: &str) -> Option<(String, &str)> {
    let mut chars = s.strip_prefix('"')?.char_indices();
    let mut out = String::new();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => out.push(chars.next()?.1),
            '"' => return Some((out, &s[i + 2..])),
            c => out.push(c),
        }
    }
    None
}

fn parse_list(s: &str) -> Option<Vec<String>> {
    let mut rest = s.trim().strip_prefix('[')?.trim_start();
    let mut out = Vec::new();
    loop {
        if let Some(r) = rest.strip_prefix(']') {
            return r.trim().is_empty().then_some(out);
        }
        let (v, r) = unquote(rest)?;
        out.push(v);
        rest = r.trim_start();
        rest = rest.strip_prefix(',').map_or(rest, str::trim_start);
    }
}

/// Разбор файла.
pub fn read(text: &str) -> Result<Keymap, Bad> {
    let mut k = Keymap::default();
    let mut section: Option<&str> = None;
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        let bad = Bad(n + 1);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
            section = match name.trim() {
                "outlook" => Some("outlook"),
                "single" => Some("single"),
                _ => return Err(bad),
            };
            continue;
        }
        let (key, value) = if line.starts_with('"') {
            let (key, rest) = unquote(line).ok_or(bad.clone())?;
            (key, rest.trim_start().strip_prefix('=').ok_or(bad.clone())?.trim())
        } else {
            let (key, value) = line.split_once('=').ok_or(bad.clone())?;
            (key.trim().to_owned(), value.trim())
        };
        match section {
            None if key == "scheme" => {
                let (v, rest) = unquote(value).ok_or(bad.clone())?;
                if !rest.trim().is_empty() {
                    return Err(bad);
                }
                k.scheme = match v.as_str() {
                    "outlook" => KeyScheme::Outlook,
                    "single" => KeyScheme::Single,
                    _ => return Err(bad),
                };
            }
            None => return Err(bad),
            Some(s) => {
                let keys = parse_list(value).ok_or(bad)?;
                let map = if s == "outlook" { &mut k.outlook } else { &mut k.single };
                map.insert(key, keys);
            }
        }
    }
    Ok(k)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keymap_round_trips() {
        let mut k = Keymap { scheme: KeyScheme::Single, ..Default::default() };
        k.outlook.insert("mail.toggle_flag".into(), vec!["ctrl-g".into(), "ctrl-shift-g".into()]);
        k.outlook.insert("mail.archive".into(), vec![]);
        k.single.insert("mail.\"odd\"".into(), vec!["y".into()]);
        let text = write(&k);
        assert!(text.contains("scheme = \"single\"\n") && text.contains("\"mail.toggle_flag\" = [\"ctrl-g\", \"ctrl-shift-g\"]\n"));
        assert_eq!(read(&text), Ok(k));
        // Комментарии, пробелы и ключ без кавычек — тоже.
        let k2 = read("# мой\n scheme = \"outlook\" \n[outlook]\nmail.reply = [ \"ctrl-r\" , ]\n").unwrap();
        assert_eq!(k2.outlook["mail.reply"], ["ctrl-r"]);
        // Чужое — ошибка с номером строки.
        assert_eq!(read("scheme = \"emacs\""), Err(Bad(1)));
        assert_eq!(read("[outlook]\n\"a\" = \"ctrl-a\""), Err(Bad(2)));
        assert_eq!(read("[vim]"), Err(Bad(1)));
        assert_eq!(read("x = 1"), Err(Bad(1)));
    }
}
