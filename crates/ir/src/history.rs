//! История переписки в письме (S5-05): где кончается новый текст и начинаются цитаты прошлых писем.
//!
//! Работает на IR внутри воркера-песочницы: [`crate::from_html`] и [`crate::from_plain`] ставят
//! [`crate::Document::history`] — индекс первого блока истории; новый текст — блоки до него. Полный IR остаётся
//! (переключатель «показать цитаты»).
//!
//! Признаки истории:
//! - HTML-разметка клиентов (её отмечает разбор HTML, [`Mark`]): `blockquote type=cite` (Apple Mail, Thunderbird),
//!   `gmail_quote`, `moz-cite-prefix`, `yahoo_quoted`, `protonmail_quote` — область цитаты; `divRplyFwdMsg`,
//!   `appendonsend` (Outlook в браузере) — всё после;
//! - шапка Outlook «From/Sent/To/Subject» («От/Отправлено/Кому/Тема») и разделитель «-----Original Message-----»
//!   («Исходное сообщение») — всё после;
//! - цитата `>` (или `blockquote` без разметки) в конце письма — только со строкой «On … wrote:» / «… пишет:» перед ней.
//!
//! Точность важнее полноты: текст автора не теряется никогда. Цитата посреди письма (ответы между цитатами),
//! пересылка, просьба «см. ниже» в новом тексте, пустой новый текст — история не отделяется, письмо целиком.

use crate::{Block, Inline, plain_text};

/// Чем отмечен кусок письма в HTML.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MarkKind {
    /// Блоки `start..end` — цитата.
    Range,
    /// Всё с блока `start` до конца — история.
    From,
}

/// Отметка разбора HTML: блоки верхнего уровня `start..end`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Mark {
    pub(crate) kind: MarkKind,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

/// Место разреза: блок верхнего уровня и строка в абзаце (0 — блок целиком).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Cut {
    block: usize,
    line: usize,
}

/// Строка блока верхнего уровня: строки абзаца (по переносам), у остальных блоков — одна строка.
struct Line {
    at: Cut,
    text: String,
    rule: bool,
}

/// Отделить историю: разрезать абзац, если она начинается посреди него, и вернуть индекс её первого блока.
pub(crate) fn detect(blocks: &mut Vec<Block>, marks: &[Mark]) -> Option<usize> {
    let lines = flat(blocks);
    let cut = [html_cut(blocks, marks, &lines), header_cut(&lines), trailing_quote_cut(blocks, &lines)]
        .into_iter()
        .flatten()
        .filter(|c| !forwarded(&lines, *c))
        .min()?;
    let fresh = new_text(&lines, cut);
    if fresh.trim().is_empty() || points_below(&fresh) {
        return None;
    }
    Some(split(blocks, cut))
}

/// Строки абзаца: инлайны между переносами.
fn paragraph_lines(content: &[Inline]) -> Vec<&[Inline]> {
    content.split(|i| matches!(i, Inline::LineBreak)).collect()
}

fn flat(blocks: &[Block]) -> Vec<Line> {
    let mut out = Vec::new();
    for (b, block) in blocks.iter().enumerate() {
        match block {
            Block::Paragraph { content, .. } => {
                for (l, line) in paragraph_lines(content).into_iter().enumerate() {
                    out.push(Line { at: Cut { block: b, line: l }, text: plain_text(line), rule: false });
                }
            }
            Block::Heading { content, .. } => {
                out.push(Line { at: Cut { block: b, line: 0 }, text: plain_text(content), rule: false });
            }
            // Цитата, список, таблица, код — без текста: строкой шапки они не бывают.
            other => out.push(Line { at: Cut { block: b, line: 0 }, text: String::new(), rule: matches!(other, Block::Rule) }),
        }
    }
    out
}

fn index_of(lines: &[Line], cut: Cut) -> usize {
    lines.iter().position(|l| l.at == cut).unwrap_or(lines.len())
}

/// Новый текст до разреза (для проверок).
fn new_text(lines: &[Line], cut: Cut) -> String {
    lines.iter().take_while(|l| l.at < cut).map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n")
}

/// Разрезать абзац на строке `cut.line`; индекс первого блока истории.
fn split(blocks: &mut Vec<Block>, cut: Cut) -> usize {
    if cut.line == 0 {
        return cut.block;
    }
    let Some(Block::Paragraph { align, content }) = blocks.get_mut(cut.block) else { return cut.block };
    // Начало строки `line` — после `line`-го переноса.
    let mut breaks = 0;
    let Some(pos) = content.iter().position(|i| {
        breaks += usize::from(matches!(i, Inline::LineBreak));
        breaks == cut.line
    }) else {
        return cut.block;
    };
    let tail: Vec<Inline> = content.drain(pos..).skip(1).collect();
    let align = *align;
    blocks.insert(cut.block + 1, Block::Paragraph { align, content: tail });
    cut.block + 1
}

/// Разметка клиентов в HTML: история — хвост письма, весь покрытый отметками (и линиями `hr` между ними).
/// Текст автора после цитаты (ответ снизу, ответы между цитатами) — хвоста нет, кроме последней цитаты.
fn html_cut(blocks: &[Block], marks: &[Mark], lines: &[Line]) -> Option<Cut> {
    let n = blocks.len();
    let mut covered = vec![false; n];
    for m in marks {
        let end = match m.kind {
            MarkKind::Range => m.end.min(n),
            MarkKind::From => n,
        };
        for c in covered.iter_mut().take(end).skip(m.start) {
            *c = true;
        }
    }
    let mut s = n;
    while s > 0 && (covered[s - 1] || matches!(blocks[s - 1], Block::Rule)) {
        s -= 1;
    }
    if s == n || !covered[s..].iter().any(|c| *c) {
        return None;
    }
    let cut = Cut { block: s, line: 0 };
    Some(attribution_before(lines, cut).unwrap_or(cut))
}

/// Цитаты в конце письма без разметки (`>` в тексте, `blockquote`): только со строкой «… wrote:» перед ними —
/// иначе это может быть цитата, которую автор привёл сам («как сказано в регламенте:»).
fn trailing_quote_cut(blocks: &[Block], lines: &[Line]) -> Option<Cut> {
    let mut s = blocks.len();
    while s > 0 && matches!(blocks[s - 1], Block::Quote(_)) {
        s -= 1;
    }
    if s == blocks.len() {
        return None;
    }
    attribution_before(lines, Cut { block: s, line: 0 })
}

/// Строка «On … wrote:» (одна-три строки — длинную строку клиенты переносят) прямо перед разрезом.
fn attribution_before(lines: &[Line], cut: Cut) -> Option<Cut> {
    let i = index_of(lines, cut);
    for k in 1..=3.min(i) {
        let window = &lines[i - k..i];
        // Строка подписи цитаты — внутри одного абзаца.
        if window.iter().any(|l| l.at.block != window[0].at.block || l.rule) {
            break;
        }
        let joined = window.iter().map(|l| l.text.trim()).collect::<Vec<_>>().join(" ");
        if !is_attribution(&joined) {
            continue;
        }
        // Нашлась короткая — но, может быть, это хвост перенесённой строки («i@x> wrote:» под «On … Ivan <»).
        let start = i - k;
        if start > 0 && k < 3 && continues(window[0].text.trim(), &lines[start - 1]) && lines[start - 1].at.block == window[0].at.block {
            let longer = format!("{} {joined}", lines[start - 1].text.trim());
            if is_attribution(&longer) {
                return Some(lines[start - 1].at);
            }
        }
        return Some(window[0].at);
    }
    None
}

/// Строка — продолжение перенесённой предыдущей: начинается не с заглавной («i@x>», «<i@x>», «пишет:»),
/// а предыдущая не кончается точкой.
fn continues(line: &str, above: &Line) -> bool {
    let first = line.chars().next();
    let tail = first.is_some_and(|c| c.is_lowercase() || matches!(c, '<' | '"' | '('));
    tail && !above.rule && !above.text.trim_end().ends_with(['.', '!', '?'])
}

/// Шапка Outlook или разделитель «-----Original Message-----»: история — с неё (и с линии над ней).
fn header_cut(lines: &[Line]) -> Option<Cut> {
    for (i, line) in lines.iter().enumerate() {
        let t = line.text.trim();
        if !is_original_separator(t) && !(has_key(t, FROM) && header_follows(&lines[i + 1..])) {
            continue;
        }
        let mut start = i;
        // Линия над шапкой («____», `hr`) — тоже история.
        while start > 0 && (lines[start - 1].rule || is_line_separator(lines[start - 1].text.trim())) {
            start -= 1;
        }
        return Some(lines[start].at);
    }
    None
}

const FROM: &[&str] = &["from", "от", "от кого"];
const SENT: &[&str] = &["sent", "date", "отправлено", "дата"];
const TO: &[&str] = &["to", "subject", "cc", "кому", "тема", "копия"];

/// После «From:» в ближайших строках — и дата («Sent:»), и адресат или тема («To:», «Subject:»).
fn header_follows(rest: &[Line]) -> bool {
    let window = &rest[..rest.len().min(8)];
    let any = |keys: &[&str]| window.iter().any(|l| has_key(l.text.trim(), keys));
    any(SENT) && any(TO)
}

/// Строка начинается с поля шапки: `From:`, `От:` (без учёта регистра).
fn has_key(line: &str, keys: &[&str]) -> bool {
    let lower = line.to_lowercase();
    keys.iter().any(|k| lower.strip_prefix(k).is_some_and(|rest| rest.trim_start().starts_with(':')))
}

/// Текст между дефисами: `-----Original Message-----` → `original message`.
fn dashed(line: &str) -> Option<String> {
    let inner = line.trim_matches(|c: char| c == '-' || c.is_whitespace());
    (line.starts_with("---") && inner.len() < line.len()).then(|| inner.to_lowercase())
}

fn is_original_separator(line: &str) -> bool {
    dashed(line).is_some_and(|s| ["original message", "исходное сообщение", "оригинальное сообщение"].contains(&s.as_str()))
}

/// Линия из подчёркиваний или дефисов (Outlook ставит её над шапкой в тексте).
fn is_line_separator(line: &str) -> bool {
    line.chars().count() >= 8 && (line.chars().all(|c| c == '_') || line.chars().all(|c| c == '-'))
}

/// История начинается с пересылки — это содержимое письма, а не цитата.
fn forwarded(lines: &[Line], cut: Cut) -> bool {
    // Строка над разрезом — тоже: «-------- Пересланное сообщение --------» над шапкой «От: … Дата: …».
    let i = index_of(lines, cut).saturating_sub(1);
    lines[i..].iter().map(|l| l.text.trim()).filter(|t| !t.is_empty()).take(4).any(|t| {
        let lower = t.to_lowercase();
        let marker = dashed(t).unwrap_or_else(|| lower.trim_end_matches(':').to_string());
        ["forwarded message", "пересылаемое сообщение", "пересланное сообщение", "begin forwarded message", "начало переадресованного сообщения"]
            .contains(&marker.as_str())
    })
}

/// Строка подписи цитаты: «On Mon, 20 Oct 2026 at 10:15, Ivan <i@x> wrote:», «20.10.2026 10:15, Иван пишет:»,
/// «пн, 20 окт. 2026 г. в 10:15, Иван <i@x>:». С датой (время) или адресом — «Here is what they wrote:» не она.
fn is_attribution(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() || t.chars().count() > 300 {
        return false;
    }
    let lower = t.to_lowercase();
    let ends = ["wrote:", "writes:", "пишет:", "написал:", "написала:", "написал(а):", "написал(-а):"].iter().any(|s| lower.ends_with(s))
        || (lower.ends_with(">:") && lower.contains('@'));
    ends && (lower.contains('@') || has_time(&lower))
}

/// Время вида `10:15`.
fn has_time(s: &str) -> bool {
    let c: Vec<char> = s.chars().collect();
    c.windows(4).any(|w| w[0].is_ascii_digit() && w[1] == ':' && w[2].is_ascii_digit() && w[3].is_ascii_digit())
}

/// Автор просит читать ниже («ответы ниже», «see my comments inline»): его текст — внутри цитаты.
fn points_below(fresh: &str) -> bool {
    let lower = fresh.to_lowercase();
    [
        "см. ниже",
        "смотри ниже",
        "смотрите ниже",
        "ответы ниже",
        "ответ ниже",
        "комментарии ниже",
        "ответил ниже",
        "ответила ниже",
        "ниже по тексту",
        "по тексту ниже",
        "в тексте ниже",
        "красным",
        "see below",
        "see my comments",
        "comments inline",
        "answers inline",
        "replies inline",
        "inline below",
        "in red",
    ]
    .iter()
    .any(|p| lower.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribution_lines() {
        assert!(is_attribution("On Mon, Oct 20, 2026 at 10:15 AM Ivan Petrov <i.petrov@example.com> wrote:"));
        assert!(is_attribution("20.10.2026 10:15, Иван Петров пишет:"));
        assert!(is_attribution("пн, 20 окт. 2026 г. в 10:15, Иван Петров <i.petrov@example.ru>:"));
        assert!(is_attribution("20 окт. 2026 г., в 10:15, Иван Петров <i@example.ru> написал(а):"));
        assert!(!is_attribution("Here is what the customer wrote:"));
        assert!(!is_attribution("In 2020 John wrote:"));
        assert!(!is_attribution("Встреча в 10:15, адрес:"));
    }

    #[test]
    fn header_keys() {
        assert!(has_key("From: Ivan", FROM) && has_key("ОТ : Иван", FROM) && has_key("От кого: Иван", FROM));
        assert!(!has_key("Fromage: cheese", FROM) && !has_key("Отчёт: готов", FROM));
        assert!(is_original_separator("-----Original Message-----") && is_original_separator("----- Исходное сообщение -----"));
        assert!(!is_original_separator("---") && is_line_separator("________________________________"));
    }
}
