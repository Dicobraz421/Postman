//! Ссылки в простом тексте (T2-01): `http(s)://…`, `www.…`, `mailto:…` — как их находят Outlook и Thunderbird.
//! Текст письма text/plain, место и описание встречи. Exchange отдаёт HTML-ссылку в текстовом теле как
//! «текст<https://…>» — угловые скобки ссылку ограничивают. Каждая найденная ссылка проходит [`check_link`]: схемы,
//! кроме http(s) и mailto, остаются текстом.

use std::ops::Range;

use crate::{Inline, LinkCheck, check_link};

/// Найденные ссылки: диапазон видимого текста (байты) и проверенная ссылка.
pub fn find_links(text: &str) -> Vec<(Range<usize>, LinkCheck)> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some((start, prefix, www)) = next_start(text, at) {
        let end = start + link_len(&text[start..]);
        let visible = &text[start..end];
        let href = if www { format!("http://{visible}") } else { visible.to_owned() };
        match check_link(&href, visible) {
            Some(check) if visible.len() > prefix && (!www || visible[prefix..].contains('.')) => {
                out.push((start..end, check));
                at = end;
            }
            _ => at = start + 1,
        }
    }
    out
}

/// Текст → инлайны: найденные ссылки — [`Inline::Link`], остальное — [`Inline::Text`].
pub fn autolink(text: &str) -> Vec<Inline> {
    let mut out = Vec::new();
    let mut at = 0;
    for (range, check) in find_links(text) {
        if range.start > at {
            out.push(Inline::text(&text[at..range.start]));
        }
        out.push(Inline::Link { href: check.href, host: check.host, suspicious: false, content: vec![Inline::text(&text[range.clone()])] });
        at = range.end;
    }
    if at < text.len() || out.is_empty() {
        out.push(Inline::text(&text[at..]));
    }
    out
}

const PREFIXES: [(&str, bool); 4] = [("https://", false), ("http://", false), ("mailto:", false), ("www.", true)];

/// Начало следующей ссылки не раньше `from`: префикс без учёта регистра в начале слова — место, длина префикса и
/// `true` для «www.» без схемы.
fn next_start(text: &str, from: usize) -> Option<(usize, usize, bool)> {
    let bytes = text.as_bytes();
    (from..bytes.len()).filter(|&i| text.is_char_boundary(i)).find_map(|i| {
        let word_start = text[..i].chars().next_back().is_none_or(|c| !c.is_alphanumeric() && c != '.' && c != '/' && c != '@');
        if !word_start {
            return None;
        }
        PREFIXES
            .iter()
            .find(|(p, _)| bytes.len() >= i + p.len() && bytes[i..i + p.len()].eq_ignore_ascii_case(p.as_bytes()))
            .map(|(p, www)| (i, p.len(), *www))
    })
}

/// Длина ссылки с начала `s`: до пробела или `<>"`, без хвостовой пунктуации и непарных закрывающих скобок.
fn link_len(s: &str) -> usize {
    let mut end = s.find(|c: char| c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | '"' | '«' | '»')).unwrap_or(s.len());
    loop {
        let Some(last) = s[..end].chars().next_back() else { return 0 };
        let cut = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '“' | '”' | '…' => true,
            ')' => unbalanced(&s[..end], '(', ')'),
            ']' => unbalanced(&s[..end], '[', ']'),
            '}' => unbalanced(&s[..end], '{', '}'),
            _ => false,
        };
        if !cut {
            return end;
        }
        end -= last.len_utf8();
    }
}

fn unbalanced(s: &str, open: char, close: char) -> bool {
    s.matches(close).count() > s.matches(open).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn links(text: &str) -> Vec<String> {
        find_links(text).into_iter().map(|(r, c)| format!("{}={}", &text[r], c.href)).collect()
    }

    #[test]
    fn plain_urls_and_punctuation() {
        assert_eq!(
            links("Ссылка: https://telemost.yandex.ru/j/123, до встречи."),
            ["https://telemost.yandex.ru/j/123=https://telemost.yandex.ru/j/123"]
        );
        assert_eq!(
            links("(см. https://ru.wikipedia.org/wiki/Rust_(язык))."),
            ["https://ru.wikipedia.org/wiki/Rust_(язык)=https://ru.wikipedia.org/wiki/Rust_(%D1%8F%D0%B7%D1%8B%D0%BA)"]
        );
        assert_eq!(links("зайдите на www.example.com!"), ["www.example.com=http://www.example.com/"]);
        assert_eq!(links("HTTPS://Zoom.us/j/1"), ["HTTPS://Zoom.us/j/1=https://zoom.us/j/1"]);
    }

    #[test]
    fn exchange_angle_brackets() {
        let t = "Присоединиться к собранию Microsoft Teams<https://teams.microsoft.com/l/meetup-join/19%3ameeting_x%40thread.v2/0?context=%7b%22Tid%22%7d>";
        assert_eq!(find_links(t).len(), 1);
        assert!(links(t)[0].starts_with("https://teams.microsoft.com/l/meetup-join/19%3ameeting_x%40thread.v2/0?context=%7b%22Tid%22%7d="));
    }

    #[test]
    fn not_links() {
        assert!(links("javascript:alert(1) file:///c:/x xhttp://a.b mywww.site.ru").is_empty());
        assert!(links("https:// и www. и mailto:").is_empty());
        assert_eq!(links("mailto:a@b.ru").len(), 1);
    }

    #[test]
    fn inlines_keep_text() {
        let i = autolink("до https://a.ru после");
        assert_eq!(crate::plain_text(&i), "до https://a.ru после");
        assert!(matches!(&i[1], Inline::Link { href, .. } if href == "https://a.ru/"));
        assert_eq!(autolink(""), vec![Inline::text("")]);
    }
}
