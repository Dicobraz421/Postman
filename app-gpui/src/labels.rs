//! Метки и категории в интерфейсе (S5-02): таблица категорий по умолчанию, имена встроенных меток, полоса и метки
//! строки письма. Ключевые слова и таблицы — `postman_core::labels`, таблица на сервере — `postman_core::categories`,
//! цвета (общие цвета Fluent 2) — `postman_core::palette`.

use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use postman_cache::EmailRow;
use postman_core::labels::{Book, Categories, Label, ROW_LABELS};

use crate::roles::Look;

/// Таблица категорий по умолчанию (DESIGN.md) с именами на языке интерфейса — пока у учётной записи нет своей.
pub(crate) fn default_book() -> Book {
    Book::own(default_table())
}

/// Таблица по умолчанию одного аккаунта (на сервере файла ещё нет).
pub(crate) fn default_table() -> Categories {
    Categories::defaults(default_name)
}

fn default_name(id: &str) -> String {
    match id {
        "clients" => tr!("labels-default-clients"),
        "team" => tr!("labels-default-team"),
        "tenders" => tr!("labels-default-tenders"),
        "pentest" => tr!("labels-default-pentest"),
        "services" => tr!("labels-default-services"),
        other => other.to_owned(),
    }
}

/// Имя метки в строке: категория — из таблицы, метка Thunderbird — на языке интерфейса.
pub(crate) fn label_name(l: &Label) -> String {
    match l {
        Label::Category(c) => c.name.clone(),
        Label::Thunderbird(1) => tr!("labels-thunderbird-1"),
        Label::Thunderbird(2) => tr!("labels-thunderbird-2"),
        Label::Thunderbird(3) => tr!("labels-thunderbird-3"),
        Label::Thunderbird(4) => tr!("labels-thunderbird-4"),
        Label::Thunderbird(_) => tr!("labels-thunderbird-5"),
    }
}

/// Что нарисовать в строке письма: полоса слева — цвет первой категории; метки — не больше [`ROW_LABELS`],
/// категория первой (DESIGN.md «Строка письма и метки»).
#[derive(Debug, Default, PartialEq)]
pub(crate) struct RowLabels {
    /// Цвет категории для полосы (имя цвета Fluent); цвет в теме — роль (`Look::cat`).
    pub(crate) stripe: Option<&'static str>,
    /// Имя и цвет (имя цвета Fluent).
    pub(crate) chips: Vec<(String, &'static str)>,
    /// Метки сроков (S5-04, `lifecycle.rs`): ставит список — им нужно «сейчас».
    pub(crate) marks: Vec<postman_core::lifecycle::Badge>,
    /// «Назначено: …» (S5-28, `shared.rs`): письмо общего ящика взято в работу; ставит список.
    pub(crate) assigned: Option<String>,
    /// Значок переноса и подсказка (S8-16, `local::Mark`); ставит список.
    pub(crate) migration: Option<(char, String)>,
}

pub(crate) fn row_labels(book: &Book, row: &EmailRow) -> RowLabels {
    if row.keywords.is_empty() {
        return RowLabels::default();
    }
    let Some(table) = book.for_account(postman_cache::account_of(&row.id)) else { return RowLabels::default() };
    let labels = table.labels(&row.keywords);
    let stripe = labels.iter().find(|l| matches!(l, Label::Category(_))).map(|l| l.color());
    let chips = labels.iter().take(ROW_LABELS).map(|l| (label_name(l), l.color())).collect();
    RowLabels { stripe, chips, marks: Vec::new(), assigned: None, migration: None }
}

/// Метка в строке отправителя: подложка и текст цвета категории; длинное имя обрезается.
pub(crate) fn chip(name: String, color: &str, l: &Look) -> Div {
    let s = l.cat(color);
    div().flex_none().max_w(px(96.)).px_1p5().rounded_sm().text_size(px(11.)).line_height(px(16.)).bg(s.bg).text_color(s.text).truncate().child(name)
}

/// Опись части (ADR 0005, `parts.rs`).
pub(crate) const PART: crate::part::Part = crate::part::Part { name: "labels", texts: TEXTS, scenes: scenes::SCENES, ..crate::part::Part::NONE };

/// Тексты части (`i18n/<язык>/<часть>.ftl`).
const TEXTS: &[pets_i18n::Resource] = &[("ru", include_str!("../i18n/ru/labels.ftl")), ("en", include_str!("../i18n/en/labels.ftl"))];

/// Сцены `--screens` меток и категорий (ADR 0005).
pub(crate) mod scenes {
    use gpui_kit::*;

    use crate::screens::{self, Scene};
    use crate::{Mailer, Section};

    pub(crate) const SCENES: &[Scene] = &[
        // Свои «Входящие» с полосами категорий и метками (S5-02): первому письму без меток клавишами 1 и 3 (как
        // `mail.categorize`) поставлены «Клиенты» и «Тендеры», оно открыто.
        Scene::new("labels", labels, labels_ready),
    ];

    fn labels(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.set_section(Section::Mail, cx);
        let inbox = crate::live::inbox(&m.folders.read(cx).mailboxes);
        let Some(id) = inbox else { return Err("no inbox".into()) };
        m.open_mailbox(id, cx);
        let list = m.list.read(cx);
        let (offset, rows) = &list.rows;
        let plain = rows.iter().position(|r| super::row_labels(&list.labels, r).chips.is_empty());
        let Some(i) = plain.map(|p| p + offset) else {
            return Err("no message without labels in the first rows of the demo inbox".into());
        };
        m.list.update(cx, |l, cx| {
            l.select(i, cx);
            l.toggle_category(1, cx);
            l.toggle_category(3, cx);
        });
        Ok(())
    }

    fn labels_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        let list = m.list.read(cx);
        let row = list.selected.and_then(|i| list.row(i));
        let labeled = row.is_some_and(|r| super::row_labels(&list.labels, r).chips.len() == 2);
        let id = row.map(|r| r.id.as_str());
        labeled
            && list.mailbox == crate::live::inbox(&m.folders.read(cx).mailboxes)
            && screens::reading(m, cx).is_some_and(|r| r.doc.is_some() && Some(r.row.id.as_str()) == id)
    }
}

#[cfg(test)]
mod tests {
    use super::{RowLabels, row_labels};
    use postman_cache::EmailRow;
    use postman_core::labels::{Book, Categories};
    use postman_core::palette;

    fn row(id: &str, keywords: &[&str]) -> EmailRow {
        EmailRow {
            id: id.into(),
            thread_id: String::new(),
            mailbox_ids: Vec::new(),
            received_at: 0,
            subject: String::new(),
            from_name: String::new(),
            from_addr: String::new(),
            preview: String::new(),
            seen: true,
            flagged: false,
            has_attachment: false,
            size: 0,
            keywords: keywords.iter().map(|k| k.to_string()).collect(),
        }
    }

    #[test]
    fn row_shows_two_labels_category_first() {
        let book = Book::own(Categories::defaults(|id| id.to_owned()));
        let got = row_labels(&book, &row("a", &["$label4", "pm.cat.team", "pm.cat.clients", "pm.cat.services"]));
        assert_eq!(got.stripe, Some(palette::BLUE));
        assert_eq!(got.chips, [("clients".to_string(), palette::BLUE), ("team".to_string(), palette::GREEN)]);
        // Только метка Thunderbird — нейтральная, без полосы.
        let tb = row_labels(&book, &row("a", &["$label4"]));
        assert_eq!((tb.stripe, tb.chips.len(), tb.chips[0].1), (None, 1, palette::NEUTRAL));
        assert_eq!(row_labels(&book, &row("a", &["$answered"])), RowLabels::default());
        // Общий ящик без своей таблицы — таблица своего аккаунта.
        assert_eq!(row_labels(&book, &row("i/a", &["pm.cat.team"])).chips.len(), 1);
    }
}
