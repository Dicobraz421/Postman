//! Таблица клавиш инструкции пользователя (`docs/USER-GUIDE.md`, S4-04) — из реестра команд, как `docs/PARITY.md`.
//! Обновить: `POSTMAN_UPDATE_DOCS=1 cargo test -p postman-gpui user_guide`. Устаревшая таблица PR не блокирует
//! (R-04, скилл pets-dev): её обновляет полоса docs-gen раз в день от main, вместе с `docs/PARITY.md`.

// Модуль только для тестов (`#[cfg(test)] mod keys_doc` в `main.rs`); метка ниже — граница для теста сырых строк.
#[cfg(test)]

const BEGIN: &str = "<!-- keys:begin -->";
const END: &str = "<!-- keys:end -->";

use crate::keymap::human;

/// Где действует: по контексту команды.
fn place(context: Option<&str>) -> &'static str {
    match context {
        Some(c) if c.starts_with("Palette") => "палитра команд",
        Some(c) if c.starts_with("Composer") => "письмо",
        Some(c) if c.contains("Meeting") => "вкладка встречи",
        Some(c) if c.contains("ContactsView") => "контакты",
        Some(c) if c.contains("CalendarView") => "календарь",
        Some(c) if c.contains("MailView") => "почта",
        Some(c) if c.contains("Mailer") => "главное окно",
        _ => "везде",
    }
}

/// Буквы команды в схеме «Одиночные клавиши» (S5-17).
fn single(c: &pets_commands::Command) -> Vec<String> {
    crate::commands::SINGLE.keys.iter().filter(|k| k.command == c.id).flat_map(|k| k.keys).map(|k| human(k)).collect()
}

fn table() -> String {
    let names = crate::commands::parity::ftl_names(crate::commands::parity::RU_FTL);
    let mut s = String::from("| Действие | Outlook | Одиночные клавиши | Где |\n|---|---|---|---|\n");
    for c in crate::commands::all().iter().filter(|c| !c.keys.is_empty() || !single(c).is_empty()) {
        let key = c.name_key();
        let name = names.iter().find(|(k, _)| *k == key).map_or(key.as_str(), |(_, v)| v);
        let keys: Vec<String> = c.keys.iter().map(|k| human(k)).collect();
        let outlook = if keys.is_empty() { "—".to_owned() } else { keys.join(", ") };
        s.push_str(&format!("| {name} | {outlook} | {} | {} |\n", single(c).join(", "), place(c.context)));
    }
    s
}

#[test]
fn user_guide_keys_are_current() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/USER-GUIDE.md");
    let old = std::fs::read_to_string(&path).unwrap().replace("\r\n", "\n");
    let (head, rest) = old.split_once(BEGIN).expect("метка keys:begin");
    let (_, tail) = rest.split_once(END).expect("метка keys:end");
    let doc = format!("{head}{BEGIN}\n{}{END}{tail}", table());
    if std::env::var_os("POSTMAN_UPDATE_DOCS").is_some() {
        std::fs::write(&path, &doc).unwrap();
    } else if old != doc {
        eprintln!("docs/USER-GUIDE.md устарел — обновит полоса docs-gen: POSTMAN_UPDATE_DOCS=1 cargo test -p postman-gpui user_guide");
    }
}
