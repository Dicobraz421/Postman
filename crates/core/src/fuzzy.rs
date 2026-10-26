//! Нечёткое совпадение палитры команд (S5-10, `docs/DESIGN.md` «Палитра команд и поиск»): совпадение строки
//! запроса с названием (точное, начало, начало слова, подстрока, буквы по порядку), набор в другой раскладке
//! («fhb» → «ари», «руддщ» → «hello»), порядок результатов (недавние — первыми при пустом запросе, точные — раньше
//! нечётких) и подсветка совпадений поиска в тексте ([`term_spans`]).
//!
//! Сравнение — без регистра, ё = е. Диапазоны — байтовые, по границам символов исходной строки.

use std::ops::Range;

/// Сила совпадения: от слабой к сильной (сравнение — по порядку вариантов).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// Буквы запроса по порядку, с пропусками («нвпс» → «Новое письмо»).
    Fuzzy,
    /// Запрос внутри слова.
    Substring,
    /// Каждое слово запроса — начало какого-то слова строки.
    WordPrefix,
    /// Строка начинается с запроса.
    Prefix,
    /// Строка и есть запрос.
    Exact,
}

/// Совпадение запроса со строкой.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub tier: Tier,
    /// Запрос набран в другой раскладке.
    pub switched: bool,
    /// Где совпало (байты строки, по возрастанию, без пересечений).
    pub spans: Vec<Range<usize>>,
}

impl Match {
    fn start(&self) -> usize {
        self.spans.first().map_or(0, |r| r.start)
    }
}

/// Символ для сравнения: нижний регистр, ё = е.
pub fn fold(c: char) -> char {
    let c = c.to_lowercase().next().unwrap_or(c);
    if c == 'ё' { 'е' } else { c }
}

/// Строка для сравнения ([`fold`] по символам).
pub fn folded(s: &str) -> String {
    s.chars().map(fold).collect()
}

/// Клавиши английской и русской раскладок по местам (нижний регистр и Shift-знаки).
const EN: &str = "qwertyuiop[]asdfghjkl;'zxcvbnm,.`{}:\"<>~";
const RU: &str = "йцукенгшщзхъфывапролджэячсмитьбюёхъжэбюё";

/// Тот же набор в другой раскладке: латиница по местам клавиш — в кириллицу, кириллица — в латиницу; прочее
/// (цифры, пробел) как есть. Ничего не поменялось — `None`.
pub fn other_layout(s: &str) -> Option<String> {
    let en: Vec<char> = EN.chars().collect();
    let ru: Vec<char> = RU.chars().collect();
    let mut changed = false;
    let out: String = s
        .chars()
        .map(fold)
        .map(|c| {
            let swapped = if let Some(i) = en.iter().position(|&e| e == c) {
                ru.get(i).copied()
            } else if let Some(i) = ru.iter().position(|&r| r == c) {
                en.get(i).copied()
            } else {
                None
            };
            match swapped {
                Some(x) => {
                    changed = true;
                    fold(x)
                }
                None => c,
            }
        })
        .collect();
    (changed && out != folded(s)).then_some(out)
}

/// Символы строки для сравнения с их байтовыми позициями в исходной строке.
fn chars(s: &str) -> Vec<(usize, char)> {
    s.char_indices().map(|(i, c)| (i, fold(c))).collect()
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric()
}

/// Байтовый диапазон символов `from..to` (номера символов) строки `t`.
fn bytes(t: &[(usize, char)], len: usize, from: usize, to: usize) -> Range<usize> {
    let start = t.get(from).map_or(len, |x| x.0);
    let end = t.get(to).map_or(len, |x| x.0);
    start..end
}

/// Где в `t` (с символа `from`) стоят символы `q` подряд.
fn find_run(t: &[(usize, char)], q: &[char], from: usize, word_start: bool) -> Option<usize> {
    if q.is_empty() || q.len() > t.len() {
        return None;
    }
    (from..=t.len() - q.len())
        .find(|&i| (!word_start || i == 0 || !is_word(t[i - 1].1)) && t[i..i + q.len()].iter().map(|x| x.1).eq(q.iter().copied()))
}

/// Совпадение запроса со строкой без смены раскладки.
fn find_plain(query: &str, text: &str) -> Option<Match> {
    let q: Vec<char> = folded(query.trim()).chars().collect();
    if q.is_empty() {
        return None;
    }
    let t = chars(text);
    let len = text.len();
    let tq: Vec<char> = t.iter().map(|x| x.1).collect();
    let whole = |tier| Some(Match { tier, switched: false, spans: vec![bytes(&t, len, 0, q.len())] });
    let trimmed: Vec<char> = folded(text.trim()).chars().collect();
    if trimmed == q {
        let lead = text.len() - text.trim_start().len();
        let first = t.iter().position(|x| x.0 >= lead).unwrap_or(0);
        return Some(Match { tier: Tier::Exact, switched: false, spans: vec![bytes(&t, len, first, first + q.len())] });
    }
    if tq.starts_with(&q) {
        return whole(Tier::Prefix);
    }
    // Слова запроса — начала слов строки (в любом порядке, каждое — своё место).
    let words: Vec<Vec<char>> = q.split(|c| c.is_whitespace()).filter(|w| !w.is_empty()).map(<[char]>::to_vec).collect();
    let mut spans = Vec::new();
    let mut taken: Vec<usize> = Vec::new();
    let all = words.iter().all(|w| {
        let mut from = 0;
        while let Some(i) = find_run(&t, w, from, true) {
            if !taken.contains(&i) {
                taken.push(i);
                spans.push(bytes(&t, len, i, i + w.len()));
                return true;
            }
            from = i + 1;
        }
        false
    });
    if all && !words.is_empty() {
        spans.sort_by_key(|r| r.start);
        return Some(Match { tier: Tier::WordPrefix, switched: false, spans: merge(spans) });
    }
    if let Some(i) = find_run(&t, &q, 0, false) {
        return Some(Match { tier: Tier::Substring, switched: false, spans: vec![bytes(&t, len, i, i + q.len())] });
    }
    fuzzy(&t, len, &q)
}

/// Буквы запроса по порядку (пробелы запроса не нужны): начала слов — предпочтительнее; слишком рваное — нет.
fn fuzzy(t: &[(usize, char)], len: usize, q: &[char]) -> Option<Match> {
    let q: Vec<char> = q.iter().copied().filter(|c| !c.is_whitespace()).collect();
    if q.len() < 2 {
        return None;
    }
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut at = 0;
    for &c in &q {
        // Сначала — начало слова впереди, иначе — ближайшая такая же буква.
        let next_start = (at..t.len()).find(|&i| t[i].1 == c && (i == 0 || !is_word(t[i - 1].1)));
        let continues = runs.last().is_some_and(|&(_, end)| end == at) && t.get(at).is_some_and(|x| x.1 == c);
        let i = if continues { Some(at) } else { next_start.or_else(|| (at..t.len()).find(|&i| t[i].1 == c)) }?;
        match runs.last_mut() {
            Some((_, end)) if *end == i => *end = i + 1,
            _ => runs.push((i, i + 1)),
        }
        at = i + 1;
    }
    // Начало — с начала слова, кусков посреди слов — не больше половины букв запроса: иначе это совпадение
    // отдельных букв, а не запроса.
    let word_start = |i: usize| i == 0 || !is_word(t[i - 1].1);
    let mid = runs.iter().filter(|(a, _)| !word_start(*a)).count();
    if !runs.first().is_some_and(|(a, _)| word_start(*a)) || mid > (q.len() / 2).max(1) {
        return None;
    }
    let spans = runs.into_iter().map(|(a, b)| bytes(t, len, a, b)).collect();
    Some(Match { tier: Tier::Fuzzy, switched: false, spans })
}

/// Совпадение запроса со строкой, с учётом набора в другой раскладке: из двух — сильнейшее, при равной силе —
/// в своей раскладке.
pub fn find(query: &str, text: &str) -> Option<Match> {
    let plain = find_plain(query, text);
    let other = other_layout(query).and_then(|q| find_plain(&q, text)).map(|m| Match { switched: true, ..m });
    match (plain, other) {
        (Some(a), Some(b)) => Some(if b.tier > a.tier { b } else { a }),
        (a, b) => a.or(b),
    }
}

fn merge(mut spans: Vec<Range<usize>>) -> Vec<Range<usize>> {
    spans.sort_by_key(|r| (r.start, r.end));
    let mut out: Vec<Range<usize>> = Vec::new();
    for r in spans {
        match out.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => out.push(r),
        }
    }
    out
}

// --- порядок палитры ---

/// Строка палитры для ранжирования: ключ (для недавних), название и подпись (совпадение в подписи — слабее).
#[derive(Clone, Copy, Debug)]
pub struct Candidate<'a> {
    pub key: &'a str,
    pub text: &'a str,
    pub extra: &'a str,
}

/// Строка палитры в итоговом порядке: номер во входном списке и совпадение (при пустом запросе — нет).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ranked {
    pub index: usize,
    pub hit: Option<Match>,
    /// Совпала подпись, а не название.
    pub in_extra: bool,
}

/// Порядок строк палитры. Пустой запрос — недавние (`recent`, новые первыми) впереди, остальные — как пришли.
/// Иначе — только совпавшие: сильнее совпадение — выше; при равной силе — название раньше подписи, своя раскладка
/// раньше другой, недавние раньше прочих, совпадение ближе к началу и короткое название — раньше.
pub fn rank(query: &str, items: &[Candidate<'_>], recent: &[String]) -> Vec<Ranked> {
    let recency = |key: &str| recent.iter().position(|r| r == key).unwrap_or(usize::MAX);
    if query.trim().is_empty() {
        let mut out: Vec<Ranked> = (0..items.len()).map(|index| Ranked { index, hit: None, in_extra: false }).collect();
        out.sort_by_key(|r| (recency(items[r.index].key), r.index));
        return out;
    }
    let mut out: Vec<Ranked> = items
        .iter()
        .enumerate()
        .filter_map(|(index, c)| match find(query, c.text) {
            Some(m) => Some(Ranked { index, hit: Some(m), in_extra: false }),
            None => find(query, c.extra).map(|m| Ranked { index, hit: Some(m), in_extra: true }),
        })
        .collect();
    out.sort_by_key(|r| {
        let m = r.hit.as_ref();
        (
            std::cmp::Reverse(m.map(|m| m.tier)),
            r.in_extra,
            m.is_some_and(|m| m.switched),
            recency(items[r.index].key),
            m.map_or(0, Match::start),
            items[r.index].text.chars().count(),
            r.index,
        )
    });
    out
}

// --- подсветка совпадений поиска ---

/// Окончания, которые словоформа может сменить: «договоры» — «договора», «reports» — «report».
const RU_ENDINGS: &str = "аеиоуыэюяйьъ";

/// Основа слова запроса: начало, с которого слово текста считается совпадением (≈ стемминг индекса).
fn stem(term: &[char]) -> Vec<char> {
    let mut s = term.to_vec();
    let cyrillic = s.iter().any(|c| ('а'..='я').contains(c));
    if cyrillic {
        for _ in 0..2 {
            if s.len() > 4 && s.last().is_some_and(|c| RU_ENDINGS.contains(*c)) {
                s.pop();
            }
        }
    } else {
        for end in ["ing", "es", "ed", "s"] {
            let e: Vec<char> = end.chars().collect();
            if s.len() >= e.len() + 4 && s.ends_with(&e) {
                s.truncate(s.len() - e.len());
                break;
            }
        }
    }
    s
}

/// Слова для подсветки из слов запроса: каждое, ещё и в другой раскладке; без повторов, в сравнимом виде.
pub fn highlight_terms<S: AsRef<str>>(words: &[S]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in words {
        for part in w.as_ref().split(|c: char| !is_word(c)).filter(|p| !p.is_empty()) {
            for v in std::iter::once(folded(part)).chain(other_layout(part)) {
                if !out.contains(&v) {
                    out.push(v);
                }
            }
        }
    }
    out
}

/// Где в тексте совпадения поиска: слово текста, которое начинается с основы слова запроса, — целиком
/// («договор» → «договора», «догов» → «договор»). Диапазоны — байты `text`, по возрастанию, без пересечений.
pub fn term_spans<S: AsRef<str>>(text: &str, terms: &[S]) -> Vec<Range<usize>> {
    let stems: Vec<Vec<char>> = terms.iter().map(|t| stem(&folded(t.as_ref()).chars().collect::<Vec<_>>())).filter(|s| !s.is_empty()).collect();
    if stems.is_empty() || text.is_empty() {
        return Vec::new();
    }
    let t = chars(text);
    let len = text.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < t.len() {
        if !is_word(t[i].1) {
            i += 1;
            continue;
        }
        let end = (i..t.len()).find(|&j| !is_word(t[j].1)).unwrap_or(t.len());
        let word: Vec<char> = t[i..end].iter().map(|x| x.1).collect();
        if stems.iter().any(|s| word.starts_with(s)) {
            out.push(bytes(&t, len, i, end));
        }
        i = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at<'a>(text: &'a str, spans: &[Range<usize>]) -> Vec<&'a str> {
        spans.iter().map(|r| &text[r.clone()]).collect()
    }

    #[test]
    fn tiers_from_exact_to_fuzzy() {
        assert_eq!(find("архив", "Архив").map(|m| m.tier), Some(Tier::Exact));
        assert_eq!(find("арх", "Архивировать").map(|m| m.tier), Some(Tier::Prefix));
        assert_eq!(find("арх", "Показать папку Архив").map(|m| m.tier), Some(Tier::WordPrefix));
        assert_eq!(find("нов пис", "Создать новое письмо").map(|m| m.tier), Some(Tier::WordPrefix));
        assert_eq!(find("хив", "Архив").map(|m| m.tier), Some(Tier::Substring));
        assert_eq!(find("нвпс", "Новое письмо").map(|m| m.tier), Some(Tier::Fuzzy));
        assert_eq!(find("ёлка", "Елка").map(|m| m.tier), Some(Tier::Exact));
        assert!(find("zzz", "Архив").is_none());
        // Буквы вразброс посреди слов — не совпадение; начало не с начала слова — тоже.
        assert!(find("олтеи", "Показать папку, Отправленные и Черновики").is_none());
        assert!(find("рхв", "Архив").is_none());
    }

    #[test]
    fn spans_point_at_the_match() {
        let text = "Показать папку Архив";
        let m = find("арх", text).unwrap();
        assert_eq!(at(text, &m.spans), ["Арх"]);
        let m = find("нвпс", "Новое письмо").unwrap();
        assert_eq!(at("Новое письмо", &m.spans), ["Н", "в", "п", "с"]);
        let m = find("пап арх", text).unwrap();
        assert_eq!(at(text, &m.spans), ["пап", "Арх"]);
    }

    #[test]
    fn wrong_layout_still_matches() {
        assert_eq!(other_layout("fhb").as_deref(), Some("ари"));
        assert_eq!(other_layout("руддщ").as_deref(), Some("hello"));
        assert_eq!(other_layout("Ghbdtn").as_deref(), Some("привет"));
        assert_eq!(other_layout("123"), None);
        // «fh[» набрано вместо «арх»: совпадение есть, помечено как другая раскладка.
        let m = find("fh[", "Архив").unwrap();
        assert_eq!((m.tier, m.switched), (Tier::Prefix, true));
        let m = find("ыутв", "Send").unwrap();
        assert_eq!((m.tier, m.switched), (Tier::Exact, true));
        // В своей раскладке совпадение сильнее — оно и остаётся.
        assert!(!find("арх", "Архив").unwrap().switched);
    }

    fn cands<'a>(texts: &'a [(&'a str, &'a str)]) -> Vec<Candidate<'a>> {
        texts.iter().map(|(key, text)| Candidate { key, text, extra: "" }).collect()
    }

    #[test]
    fn empty_query_puts_recent_first() {
        let items = cands(&[("a", "Ответить"), ("b", "Переслать"), ("c", "Удалить"), ("d", "Флаг")]);
        let recent = vec!["c".to_string(), "a".to_string()];
        let order: Vec<usize> = rank("", &items, &recent).into_iter().map(|r| r.index).collect();
        assert_eq!(order, [2, 0, 1, 3]);
    }

    #[test]
    fn exact_before_prefix_before_fuzzy() {
        let items = cands(&[
            ("fuzzy", "Архив проекта новый"),
            ("prefix", "Архивировать"),
            ("word", "Перейти в Архив проекта"),
            ("exact", "Архив"),
            ("none", "Удалить"),
        ]);
        let order: Vec<&str> = rank("архив", &items, &[]).into_iter().map(|r| items[r.index].key).collect();
        // «Архив проекта новый» начинается с запроса, как «Архивировать», — короче выше.
        assert_eq!(order, ["exact", "prefix", "fuzzy", "word"]);
        // Равная сила: недавняя — выше.
        let items = cands(&[("x", "Срок…"), ("y", "Сроки")]);
        let order: Vec<&str> = rank("срок", &items, &["y".into()]).into_iter().map(|r| items[r.index].key).collect();
        assert_eq!(order, ["y", "x"]);
        // Совпала только подпись — ниже совпавшего названия.
        let items = [Candidate { key: "sub", text: "Входящие", extra: "архив" }, Candidate { key: "main", text: "Архивировать", extra: "" }];
        let r = rank("арх", &items, &[]);
        assert_eq!(r.iter().map(|r| items[r.index].key).collect::<Vec<_>>(), ["main", "sub"]);
        assert!(r[1].in_extra);
        // Другая раскладка — после своей при равной силе.
        let items = cands(&[("ru", "fhb"), ("en", "ари")]);
        let order: Vec<&str> = rank("fhb", &items, &[]).into_iter().map(|r| items[r.index].key).collect();
        assert_eq!(order, ["ru", "en"]);
    }

    #[test]
    fn highlight_whole_word_forms() {
        let text = "Re: Договор ИБ — правки по договору, договорённость; ДОГОВОРЫ.";
        let spans = term_spans(text, &["договоры"]);
        assert_eq!(at(text, &spans), ["Договор", "договору", "договорённость", "ДОГОВОРЫ"]);
        // Начало слова (поиск по мере набора) — слово целиком; внутри слова — нет.
        assert_eq!(at("Переговоры о договоре", &term_spans("Переговоры о договоре", &["догов"])), ["договоре"]);
        // ё = е, английские окончания.
        assert_eq!(at("Отчёт и отчеты", &term_spans("Отчёт и отчеты", &["отчет"])), ["Отчёт", "отчеты"]);
        assert_eq!(at("Reports: report", &term_spans("Reports: report", &["reports"])), ["Reports", "report"]);
        assert!(term_spans("пусто", &[] as &[&str]).is_empty());
        assert!(term_spans("", &["x"]).is_empty());
    }

    #[test]
    fn highlight_terms_include_the_other_layout() {
        let terms = highlight_terms(&["ljujdjh", "ИБ"]);
        assert_eq!(terms, ["ljujdjh", "договор", "иб", "b,"]);
        let text = "Договор ИБ";
        assert_eq!(at(text, &term_spans(text, &terms)), ["Договор", "ИБ"]);
    }
}
