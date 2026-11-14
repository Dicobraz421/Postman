//! Ответ на фрагменты (решение владельца `docs/decisions/2026-11-14-messenger-comforts.md`): черновик — список реплик
//! «цитата фрагмента → мой ответ» по порядку добавления. Порядок меняется, реплика убирается кнопкой, убранная
//! возвращается отменой. При отправке реплики — обычный Markdown письма: цитата `> ` с подписью «Имя, дата:» (в HTML —
//! `<blockquote>`, `build`), ответ — абзацем после неё. Черновик на сервере — уже собранный текст: как он устроен,
//! окно письма хранит в базе профиля (`postman_core::local::DraftLayout`), без записи — запасной разбор [`parse`].
//! Без I/O.

use std::ops::Range;

/// Фрагмент письма, на который отвечают, и ответ на него.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Fragment {
    /// Номер реплики в черновике: не меняется при перестановке и отмене удаления.
    pub id: u64,
    /// Письмо, из которого фрагмент (ключ письма в кэше).
    pub letter: String,
    /// Автор фрагмента и дата письма — подпись цитаты: «Анна Петрова, 17.10.2026 12:00».
    pub who: String,
    pub date: String,
    /// Выделенный текст как есть (простой текст, абзацы — пустой строкой).
    pub text: String,
    /// Мой ответ (Markdown, как текст письма).
    pub answer: String,
}

impl Fragment {
    pub fn new(letter: impl Into<String>, who: impl Into<String>, date: impl Into<String>, text: impl Into<String>) -> Self {
        Self { id: 0, letter: letter.into(), who: who.into(), date: date.into(), text: text.into(), answer: String::new() }
    }
}

/// Реплики черновика и убранные (для отмены, последняя — сверху).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Replies {
    items: Vec<Fragment>,
    removed: Vec<(usize, Fragment)>,
    next: u64,
}

impl Replies {
    pub fn items(&self) -> &[Fragment] {
        &self.items
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn get(&self, id: u64) -> Option<&Fragment> {
        self.items.iter().find(|f| f.id == id)
    }

    pub fn index(&self, id: u64) -> Option<usize> {
        self.items.iter().position(|f| f.id == id)
    }

    /// Новая реплика в конец; номер реплики.
    pub fn add(&mut self, mut f: Fragment) -> u64 {
        self.next += 1;
        f.id = self.next;
        self.items.push(f);
        self.next
    }

    /// Убрать реплику (кнопкой «×»): её можно вернуть [`Replies::undo`].
    pub fn remove(&mut self, id: u64) -> bool {
        let Some(i) = self.index(id) else { return false };
        let f = self.items.remove(i);
        self.removed.push((i, f));
        true
    }

    /// Есть что вернуть.
    pub fn can_undo(&self) -> bool {
        !self.removed.is_empty()
    }

    /// Вернуть последнюю убранную реплику на её место; её номер.
    pub fn undo(&mut self) -> Option<u64> {
        let (i, f) = self.removed.pop()?;
        let id = f.id;
        self.items.insert(i.min(self.items.len()), f);
        Some(id)
    }

    /// Сдвинуть реплику вверх (`-1`) или вниз (`+1`); у края — нет.
    pub fn shift(&mut self, id: u64, delta: isize) -> bool {
        let Some(i) = self.index(id) else { return false };
        let Some(j) = i.checked_add_signed(delta).filter(|&j| j < self.items.len()) else { return false };
        self.place(id, j)
    }

    /// Поставить реплику на место `to` (перетаскивание): остальные сдвигаются.
    pub fn place(&mut self, id: u64, to: usize) -> bool {
        let Some(i) = self.index(id) else { return false };
        let to = to.min(self.items.len().saturating_sub(1));
        if i == to {
            return false;
        }
        let f = self.items.remove(i);
        self.items.insert(to, f);
        true
    }

    /// Мой ответ на реплику. `true` — текст изменился.
    pub fn set_answer(&mut self, id: u64, answer: &str) -> bool {
        match self.items.iter_mut().find(|f| f.id == id) {
            Some(f) if f.answer != answer => {
                answer.clone_into(&mut f.answer);
                true
            }
            _ => false,
        }
    }

    /// Markdown реплик по порядку: цитата и ответ под ней.
    pub fn markdown(&self) -> String {
        let mut out = String::new();
        for f in &self.items {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(quote(f).trim_end_matches('\n'));
            let answer = f.answer.trim();
            if !answer.is_empty() {
                out.push_str("\n\n");
                out.push_str(f.answer.trim_end());
            }
        }
        out
    }
}

/// Цитата фрагмента: строка подписи «Имя, дата:» и текст, каждая строка с `> ` (пустая — `>`). Текст фрагмента —
/// простой: знаки разметки экранируются, как в цитате ответа (`postman_ir::quote`).
pub fn quote(f: &Fragment) -> String {
    let caption = match (f.who.trim(), f.date.trim()) {
        ("", "") => String::new(),
        (who, "") => format!("{}:", escape(who)),
        ("", date) => format!("{}:", escape(date)),
        (who, date) => format!("{}, {}:", escape(who), escape(date)),
    };
    let mut out = String::new();
    if !caption.is_empty() {
        out.push_str("> ");
        out.push_str(&caption);
        out.push('\n');
    }
    for line in f.text.trim_matches('\n').lines() {
        let line = line.trim_end();
        if line.is_empty() {
            out.push_str(">\n");
        } else {
            out.push_str("> ");
            out.push_str(&escape_line(line));
            out.push('\n');
        }
    }
    out
}

/// Текст письма с репликами: свой текст `head`, реплики, затем `tail` (подпись и полная цитата ответа).
pub fn assemble(head: &str, replies: &Replies, tail: &str) -> String {
    let mut out = head.trim_end_matches('\n').to_owned();
    let pairs = replies.markdown();
    for part in [pairs.as_str(), tail.trim_start_matches('\n')] {
        if part.trim().is_empty() {
            continue;
        }
        if !out.trim().is_empty() {
            out.push_str("\n\n");
        } else {
            out.clear();
        }
        out.push_str(part);
    }
    out
}

/// Черновик, разобранный на части: свой текст, реплики, хвост (подпись и полная цитата).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parsed {
    pub head: String,
    pub fragments: Vec<Fragment>,
    pub tail: String,
}

/// Запасной разбор черновика без своей записи в базе (открыт на другом компьютере, запись потерялась): реплики —
/// цитаты `> ` с подписью «Имя, дата:» (как их пишет [`quote`]) над полной цитатой ответа или пересланным письмом;
/// ответ на реплику — текст до следующей; хвост — с подписи `signature` (если она ниже реплик) или с полной цитаты.
/// Части собираются ([`assemble`]) в тот же текст (пробелы не в счёт) — иначе `None`: разбор не меняет письмо.
pub fn parse(markdown: &str, signature: &str) -> Option<Parsed> {
    let text = markdown.replace("\r\n", "\n");
    let blocks = blocks(&text);
    // Полная цитата ответа (строка «… пишет:» и под ней `> `) или пересланное письмо — хвост, реплик ниже нет.
    let mut end = text.len();
    for b in &blocks {
        let lines = lines_at(&text, b.clone());
        if let Some((at, _)) = lines.iter().find(|(_, l)| l.starts_with("-------- ")) {
            end = *at;
            break;
        }
        if let Some(q) = lines.iter().position(|(_, l)| l.trim_start().starts_with('>'))
            && q > 0
        {
            end = lines[q - 1].0;
            break;
        }
    }
    let found: Vec<(Range<usize>, Fragment)> =
        blocks.iter().filter(|b| b.end <= end).filter_map(|b| Some((b.clone(), fragment_of(&text[b.clone()])?))).collect();
    let last = found.last()?.0.end;
    let sig = signature.trim();
    let tail_at = match text[..end].rfind(sig) {
        Some(at) if !sig.is_empty() && at >= last && (at == 0 || text[..at].ends_with('\n')) => at,
        _ => end,
    };
    let head = text[..found[0].0.start].trim_end().to_owned();
    let mut replies = Replies::default();
    let mut fragments = Vec::with_capacity(found.len());
    for (i, (range, f)) in found.iter().enumerate() {
        let next = found.get(i + 1).map_or(tail_at, |(r, _)| r.start).max(range.end);
        let mut f = f.clone();
        f.answer = text[range.end..next].trim_matches('\n').trim_end().to_owned();
        replies.add(f.clone());
        fragments.push(f);
    }
    let tail = text[tail_at..].trim_matches('\n').to_owned();
    let squeeze = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    (squeeze(&assemble(&head, &replies, &tail)) == squeeze(&text)).then_some(Parsed { head, fragments, tail })
}

/// Абзацы текста: байты строк подряд без пустых.
fn blocks(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let (mut start, mut off) = (None, 0);
    for line in text.split_inclusive('\n') {
        match (line.trim().is_empty(), start) {
            (false, None) => start = Some(off),
            (true, Some(s)) => {
                out.push(s..off);
                start = None;
            }
            _ => {}
        }
        off += line.len();
    }
    if let Some(s) = start {
        out.push(s..text.len());
    }
    out
}

/// Строки абзаца с их началом в тексте.
fn lines_at(text: &str, range: Range<usize>) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut off = range.start;
    for line in text[range].split_inclusive('\n') {
        out.push((off, line.trim_end_matches(['\n', '\r'])));
        off += line.len();
    }
    out
}

/// Абзац — цитата фрагмента: все строки `>`, первая — подпись «Имя, дата:» (с цифрами даты).
fn fragment_of(block: &str) -> Option<Fragment> {
    let mut lines = block.lines().map(str::trim_end);
    let caption = lines.next()?.strip_prefix("> ")?.strip_suffix(':')?;
    if !caption.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut text = Vec::new();
    for line in lines {
        let body = line.strip_prefix("> ").or_else(|| line.strip_prefix('>'))?;
        text.push(unescape(body));
    }
    if text.iter().all(|l| l.trim().is_empty()) {
        return None;
    }
    let caption = unescape(caption);
    let (who, date) = match caption.rsplit_once(", ") {
        Some((who, date)) if date.chars().any(|c| c.is_ascii_digit()) => (who.to_owned(), date.to_owned()),
        _ => (String::new(), caption),
    };
    Some(Fragment::new("", who, date, text.join("\n")))
}

/// Обратное [`escape_line`]: `\` перед знаком препинания убирается.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\'
            && let Some(&n) = chars.peek()
            && n.is_ascii_punctuation()
        {
            out.push(n);
            chars.next();
            continue;
        }
        out.push(c);
    }
    out
}

/// Знаки разметки внутри строки — как в цитате ответа.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '*' | '_' | '`' | '[' | ']' | '<' | '~') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Строка простого текста в цитате: внутри — [`escape`], в начале — то, что Markdown прочёл бы заголовком, списком,
/// вложенной цитатой или чертой.
fn escape_line(line: &str) -> String {
    let body = escape(line);
    let start = body.trim_start();
    let lead = &body[..body.len() - start.len()];
    let digits = start.chars().take_while(char::is_ascii_digit).count();
    let after_digits = &start[digits..];
    let block = start.starts_with('#')
        || start.starts_with('>')
        || start.starts_with("- ")
        || start.starts_with("+ ")
        || start == "-"
        || start.starts_with("---")
        || start.starts_with("===")
        || (digits > 0 && (after_digits.starts_with(". ") || after_digits.starts_with(") ")));
    if !block {
        return body;
    }
    if digits > 0 {
        return format!("{lead}{}\\{}", &start[..digits], after_digits);
    }
    format!("{lead}\\{start}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frag(text: &str) -> Fragment {
        Fragment::new("m1", "Анна Петрова", "17.10.2026 12:00", text)
    }

    #[test]
    fn order_remove_undo() {
        let mut r = Replies::default();
        let a = r.add(frag("а"));
        let b = r.add(frag("б"));
        let c = r.add(frag("в"));
        let texts = |r: &Replies| r.items().iter().map(|f| f.text.clone()).collect::<Vec<_>>().join("");
        assert_eq!(texts(&r), "абв");
        assert!(r.shift(c, -1));
        assert_eq!(texts(&r), "авб");
        assert!(!r.shift(a, -1), "выше первой — некуда");
        assert!(r.place(a, 2));
        assert_eq!(texts(&r), "вба");
        assert!(r.remove(b));
        assert_eq!(texts(&r), "ва");
        assert!(r.remove(c));
        assert_eq!(texts(&r), "а");
        // Отмена — по одной, последняя убранная первой, на своё место.
        assert_eq!(r.undo(), Some(c));
        assert_eq!(texts(&r), "ва");
        assert_eq!(r.undo(), Some(b));
        assert_eq!(texts(&r), "вба");
        assert_eq!(r.undo(), None);
        assert!(!r.can_undo());
        assert!(r.set_answer(a, "ок") && !r.set_answer(a, "ок"));
    }

    #[test]
    fn quote_escapes_plain_text() {
        let q = quote(&frag("Цена *100* руб.\n\n# не заголовок\n1. не список\n> не цитата"));
        assert_eq!(q, "> Анна Петрова, 17.10.2026 12:00:\n> Цена \\*100\\* руб.\n>\n> \\# не заголовок\n> 1\\. не список\n> \\> не цитата\n");
    }

    #[test]
    fn assemble_keeps_head_pairs_tail() {
        let mut r = Replies::default();
        let a = r.add(frag("первый"));
        r.add(Fragment::new("m2", "Иван", "18.10.2026 09:00", "второй"));
        r.set_answer(a, "Согласен.");
        let md = assemble("Добрый день!\n\n", &r, "С уважением,\nЯ");
        assert_eq!(
            md,
            "Добрый день!\n\n> Анна Петрова, 17.10.2026 12:00:\n> первый\n\nСогласен.\n\n> Иван, 18.10.2026 09:00:\n> второй\n\nС уважением,\nЯ"
        );
        // Без своего текста — сразу реплики.
        assert!(assemble("\n\n", &r, "").starts_with("> Анна"));
        assert_eq!(assemble("Текст", &Replies::default(), ""), "Текст");
    }

    /// Запасной разбор черновика: собранный текст (свой текст, реплики, подпись, полная цитата) — снова на части; та
    /// же сборка даёт тот же текст.
    #[test]
    fn parse_restores_replies_from_the_letter() {
        let mut r = Replies::default();
        let a = r.add(frag("Сроки — *до* пятницы?\n\n1. пункт"));
        r.add(Fragment::new("m2", "Петров, Иван", "18.10.2026 09:00", "второй"));
        r.set_answer(a, "Да.\n\nИ ещё строка.");
        let tail = "С уважением,\nЯ\n\n17.10.2026 12:00, Анна пишет:\n> Сроки — до пятницы?\n>\n> Бюджет";
        let md = assemble("Коллеги,", &r, tail);
        let p = parse(&md, "С уважением,\nЯ").unwrap();
        assert_eq!(p.head, "Коллеги,");
        assert_eq!(p.tail, tail);
        let got: Vec<_> = p.fragments.iter().map(|f| (f.who.as_str(), f.date.as_str(), f.text.as_str(), f.answer.as_str())).collect();
        assert_eq!(
            got,
            [
                ("Анна Петрова", "17.10.2026 12:00", "Сроки — *до* пятницы?\n\n1. пункт", "Да.\n\nИ ещё строка."),
                ("Петров, Иван", "18.10.2026 09:00", "второй", ""),
            ]
        );
        // Подписи нет среди частей — хвост с полной цитаты, подпись — в ответе на последнюю реплику (текст тот же).
        let p = parse(&md, "").unwrap();
        assert!(p.tail.starts_with("17.10.2026 12:00, Анна пишет:"), "{}", p.tail);
        assert_eq!(p.fragments[1].answer, "С уважением,\nЯ");
        // Без полной цитаты («Не включать исходное») — хвост с подписи.
        let md = assemble("", &r, "С уважением,\nЯ");
        let p = parse(&md.replace('\n', "\r\n"), "С уважением,\nЯ").unwrap();
        assert_eq!((p.head.as_str(), p.tail.as_str(), p.fragments.len()), ("", "С уважением,\nЯ", 2));
    }

    /// Обычный ответ и цитата без подписи «Имя, дата:» — не реплики.
    #[test]
    fn parse_leaves_plain_replies_alone() {
        assert_eq!(parse("Привет\n\nС уважением\n\n17.10.2026, Анна пишет:\n> текст", "С уважением"), None);
        assert_eq!(parse("Смотри:\n\n> просто цитата\n> ещё\n\nОк", ""), None);
        assert_eq!(parse("> Анна:\n> без даты\n\nОк", ""), None);
    }

    /// Письмо из нескольких фрагментов — обычное письмо: в HTML у каждого фрагмента свой `<blockquote>` с подписью
    /// «Имя, дата:», ответ — абзацем после него; в text/plain — строки `> `.
    #[test]
    fn letter_from_fragments_html_and_plain() {
        use mail_parser::MessageParser;
        let mut r = Replies::default();
        let a = r.add(frag("Сроки — до пятницы?"));
        let b = r.add(Fragment::new("m2", "Иван", "18.10.2026 09:00", "Бюджет <утверждён>"));
        r.set_answer(a, "Да, до пятницы.");
        r.set_answer(b, "Хорошо.");
        let d = crate::Draft {
            from: crate::Address::new("Я", "me@corp.example"),
            to: vec![crate::Address::new("", "anna@corp.example")],
            subject: "RE: План".into(),
            markdown: assemble("Коллеги, по пунктам:", &r, "С уважением,\nЯ"),
            ..Default::default()
        };
        let raw = crate::build(&d, 0, "f@corp.example").unwrap();
        let msg = MessageParser::default().parse(&raw).unwrap();
        let html = msg.body_html(0).unwrap();
        assert_eq!(html.matches("<blockquote>").count(), 2, "{html}");
        let first = html.find("Анна Петрова, 17.10.2026 12:00:").unwrap();
        let answer = html.find("<p>Да, до пятницы.</p>").unwrap();
        let second = html.find("Иван, 18.10.2026 09:00:").unwrap();
        assert!(first < answer && answer < second, "{html}");
        assert!(html.contains("Бюджет &lt;утверждён&gt;"), "{html}");
        let text = msg.body_text(0).unwrap().replace("\r\n", "\n");
        assert!(text.starts_with("Коллеги, по пунктам:\n\n> Анна Петрова, 17.10.2026 12:00:\n> Сроки — до пятницы?\n\nДа, до пятницы."), "{text}");
        assert!(text.contains("> Иван, 18.10.2026 09:00:\n> Бюджет \\<утверждён>\n\nХорошо.\n\nС уважением,\nЯ"), "{text}");
    }
}
