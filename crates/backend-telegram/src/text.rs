//! Текст Telegram ↔ IR (ADR 0012 «Проверка договора», тело): текст и сущности (смещения в UTF-16) → IR без разбора
//! разметки и обратно. Жирный, курсив, подчёркнутый, зачёркнутый, код, скрытый — отметки; `pre` — `Block::Code`;
//! `blockquote` — `Block::Quote`; `text_url`, `url`, `email`, `@имя` — ссылки (проверка `postman_ir::check_link`);
//! `mention_name` — `Inline::Mention`. Свои эмодзи — их текстовая замена (она уже в тексте).

use grammers_tl_types as tl;
use postman_ir::{Align, Block, Document, Inline, Marks, check_link};

/// Сколько единиц UTF-16 в строке.
pub fn utf16_len(s: &str) -> i32 {
    i32::try_from(s.encode_utf16().count()).unwrap_or(i32::MAX)
}

/// Смещения UTF-16 → байты строки: смещение внутри суррогатной пары — к началу знака.
struct Map {
    bytes: Vec<usize>,
}

impl Map {
    fn new(text: &str) -> Self {
        let mut bytes = Vec::with_capacity(text.len() + 1);
        for (i, c) in text.char_indices() {
            for _ in 0..c.len_utf16() {
                bytes.push(i);
            }
        }
        bytes.push(text.len());
        Self { bytes }
    }

    fn len(&self) -> i32 {
        i32::try_from(self.bytes.len() - 1).unwrap_or(i32::MAX)
    }

    fn byte(&self, u: i32) -> usize {
        let u = usize::try_from(u.max(0)).unwrap_or(0).min(self.bytes.len() - 1);
        self.bytes[u]
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    Bold,
    Italic,
    Underline,
    Strike,
    Code,
    Spoiler,
}

#[derive(Clone, PartialEq, Eq)]
enum Linky {
    Url(String),
    /// Адрес — сам текст (`url`, `email`, `@имя`), с приставкой.
    Text(&'static str),
    Username,
    Mention(i64),
}

/// Сущность, которую понимает IR.
#[derive(Clone)]
enum Ent {
    Mark(Mark),
    Link(Linky),
    Pre,
    Quote,
}

fn ent(e: &tl::enums::MessageEntity) -> Option<(i32, i32, Ent)> {
    use tl::enums::MessageEntity as E;
    let (o, l, k) = match e {
        E::Bold(x) => (x.offset, x.length, Ent::Mark(Mark::Bold)),
        E::Italic(x) => (x.offset, x.length, Ent::Mark(Mark::Italic)),
        E::Underline(x) => (x.offset, x.length, Ent::Mark(Mark::Underline)),
        E::Strike(x) => (x.offset, x.length, Ent::Mark(Mark::Strike)),
        E::Code(x) => (x.offset, x.length, Ent::Mark(Mark::Code)),
        E::Spoiler(x) => (x.offset, x.length, Ent::Mark(Mark::Spoiler)),
        E::Pre(x) => (x.offset, x.length, Ent::Pre),
        E::Blockquote(x) => (x.offset, x.length, Ent::Quote),
        E::TextUrl(x) => (x.offset, x.length, Ent::Link(Linky::Url(x.url.clone()))),
        E::Url(x) => (x.offset, x.length, Ent::Link(Linky::Text("https://"))),
        E::Email(x) => (x.offset, x.length, Ent::Link(Linky::Text("mailto:"))),
        E::Mention(x) => (x.offset, x.length, Ent::Link(Linky::Username)),
        E::MentionName(x) => (x.offset, x.length, Ent::Link(Linky::Mention(x.user_id))),
        _ => return None,
    };
    (l > 0 && o >= 0).then_some((o, o.saturating_add(l), k))
}

/// Текст и сущности сообщения → IR.
pub fn to_ir(text: &str, entities: &[tl::enums::MessageEntity]) -> Document {
    let map = Map::new(text);
    let all: Vec<(i32, i32, Ent)> = entities.iter().filter_map(ent).map(|(s, e, k)| (s.min(map.len()), e.min(map.len()), k)).collect();
    let mut blocks_at: Vec<(i32, i32, bool)> =
        all.iter().filter_map(|(s, e, k)| matches!(k, Ent::Pre | Ent::Quote).then_some((*s, *e, matches!(k, Ent::Pre)))).collect();
    blocks_at.sort_by_key(|b| b.0);
    let inline: Vec<(i32, i32, Ent)> = all.into_iter().filter(|(_, _, k)| matches!(k, Ent::Mark(_) | Ent::Link(_))).collect();
    let cx = Cx { text, map: &map, inline: &inline };
    let mut out = Vec::new();
    let mut cur = 0;
    let mut after_block = false;
    for (s, e, pre) in blocks_at {
        if s < cur || e <= s {
            continue;
        }
        cx.paragraphs(cur, s, after_block, true, &mut out);
        if pre {
            out.push(Block::Code(text[map.byte(s)..map.byte(e)].trim_end_matches('\n').to_owned()));
        } else {
            let mut inner = Vec::new();
            cx.paragraphs(s, e, false, false, &mut inner);
            out.push(Block::Quote(inner));
        }
        cur = e;
        after_block = true;
    }
    cx.paragraphs(cur, map.len(), after_block, false, &mut out);
    Document { blocks: out, ..Document::default() }
}

struct Cx<'a> {
    text: &'a str,
    map: &'a Map,
    inline: &'a [(i32, i32, Ent)],
}

impl Cx<'_> {
    /// Строки отрезка — абзацы; перевод строки сразу после блока и сразу перед блоком — граница, не пустой абзац.
    fn paragraphs(&self, s: i32, e: i32, after_block: bool, before_block: bool, out: &mut Vec<Block>) {
        if e <= s {
            return;
        }
        let mut lines: Vec<(i32, i32)> = Vec::new();
        let mut start = s;
        let mut u = s;
        for c in self.text[self.map.byte(s)..self.map.byte(e)].chars() {
            let w = i32::try_from(c.len_utf16()).unwrap_or(1);
            if c == '\n' {
                lines.push((start, u));
                start = u + w;
            }
            u += w;
        }
        lines.push((start, e));
        if after_block && lines.first().is_some_and(|(a, b)| a == b) {
            lines.remove(0);
        }
        if before_block && lines.last().is_some_and(|(a, b)| a == b) {
            lines.pop();
        }
        for (a, b) in lines {
            out.push(Block::Paragraph { align: Align::Start, content: self.inlines(a, b) });
        }
    }

    fn inlines(&self, s: i32, e: i32) -> Vec<Inline> {
        let mut cuts = vec![s, e];
        for (a, b, _) in self.inline {
            for x in [*a, *b] {
                if x > s && x < e {
                    cuts.push(x);
                }
            }
        }
        cuts.sort_unstable();
        cuts.dedup();
        // Кусок: текст, отметки, ссылка (номер сущности).
        let mut atoms: Vec<(String, Marks, Option<usize>)> = Vec::new();
        for w in cuts.windows(2) {
            let (a, b) = (w[0], w[1]);
            if b <= a {
                continue;
            }
            let mut marks = Marks::default();
            let mut link = None;
            for (i, (x, y, k)) in self.inline.iter().enumerate() {
                if *x <= a && b <= *y {
                    match k {
                        Ent::Mark(Mark::Bold) => marks.bold = true,
                        Ent::Mark(Mark::Italic) => marks.italic = true,
                        Ent::Mark(Mark::Underline) => marks.underline = true,
                        Ent::Mark(Mark::Strike) => marks.strike = true,
                        Ent::Mark(Mark::Code) => marks.code = true,
                        Ent::Mark(Mark::Spoiler) => marks.spoiler = true,
                        Ent::Link(_) if link.is_none() => link = Some(i),
                        _ => {}
                    }
                }
            }
            atoms.push((self.text[self.map.byte(a)..self.map.byte(b)].to_owned(), marks, link));
        }
        let mut out = Vec::new();
        let mut i = 0;
        while i < atoms.len() {
            let Some(li) = atoms[i].2 else {
                let (t, m, _) = &atoms[i];
                out.push(Inline::Text { text: t.clone(), marks: *m });
                i += 1;
                continue;
            };
            let mut j = i;
            while j < atoms.len() && atoms[j].2 == Some(li) {
                j += 1;
            }
            let group = &atoms[i..j];
            let visible: String = group.iter().map(|a| a.0.as_str()).collect();
            let content: Vec<Inline> = group.iter().map(|(t, m, _)| Inline::Text { text: t.clone(), marks: *m }).collect();
            i = j;
            let href = match &self.inline[li].2 {
                Ent::Link(Linky::Mention(id)) => {
                    out.push(Inline::Mention { id: id.to_string(), text: visible });
                    continue;
                }
                Ent::Link(Linky::Url(u)) => u.clone(),
                Ent::Link(Linky::Text(_)) if visible.contains("://") || visible.starts_with("mailto:") => visible.clone(),
                Ent::Link(Linky::Text(prefix)) => format!("{prefix}{visible}"),
                Ent::Link(Linky::Username) => format!("https://t.me/{}", visible.trim_start_matches('@')),
                _ => {
                    out.extend(content);
                    continue;
                }
            };
            match check_link(&href, &visible) {
                Some(c) => out.push(Inline::Link { href: c.href, host: c.host, suspicious: c.suspicious, content }),
                None => out.extend(content),
            }
        }
        out
    }
}

/// Сборка текста и сущностей из IR.
struct Out<'a> {
    text: String,
    at: i32,
    ents: Vec<tl::enums::MessageEntity>,
    user: &'a dyn Fn(&str) -> Option<tl::enums::InputUser>,
}

impl Out<'_> {
    fn push(&mut self, s: &str) {
        self.text.push_str(s);
        self.at += utf16_len(s);
    }

    fn mark(&mut self, start: i32, m: Mark) {
        let (offset, length) = (start, self.at - start);
        if length <= 0 {
            return;
        }
        use tl::types as t;
        self.ents.push(match m {
            Mark::Bold => t::MessageEntityBold { offset, length }.into(),
            Mark::Italic => t::MessageEntityItalic { offset, length }.into(),
            Mark::Underline => t::MessageEntityUnderline { offset, length }.into(),
            Mark::Strike => t::MessageEntityStrike { offset, length }.into(),
            Mark::Code => t::MessageEntityCode { offset, length }.into(),
            Mark::Spoiler => t::MessageEntitySpoiler { offset, length }.into(),
        });
    }

    fn inlines(&mut self, items: &[Inline], bold: bool) {
        for i in items {
            let start = self.at;
            match i {
                Inline::Text { text, marks } => {
                    self.push(text);
                    let ms = [
                        (marks.bold || bold, Mark::Bold),
                        (marks.italic, Mark::Italic),
                        (marks.underline, Mark::Underline),
                        (marks.strike, Mark::Strike),
                        (marks.code, Mark::Code),
                        (marks.spoiler, Mark::Spoiler),
                    ];
                    for (on, m) in ms {
                        if on {
                            self.mark(start, m);
                        }
                    }
                }
                Inline::Link { href, content, .. } => {
                    self.inlines(content, bold);
                    let visible = postman_ir::plain_text(content);
                    if self.at > start && visible.trim() != href.as_str() {
                        self.ents.push(tl::types::MessageEntityTextUrl { offset: start, length: self.at - start, url: href.clone() }.into());
                    }
                }
                Inline::Mention { id, text } => {
                    self.push(text);
                    if let Some(user_id) = (self.user)(id)
                        && self.at > start
                    {
                        self.ents.push(tl::types::InputMessageEntityMentionName { offset: start, length: self.at - start, user_id }.into());
                    }
                }
                Inline::LineBreak => self.push("\n"),
                Inline::Image { alt, .. } => self.push(alt),
                Inline::Remote { url, alt, .. } => self.push(if alt.is_empty() { url } else { alt }),
            }
        }
    }

    fn blocks(&mut self, blocks: &[Block]) {
        for (n, b) in blocks.iter().enumerate() {
            if n > 0 {
                self.push("\n");
            }
            match b {
                Block::Paragraph { content, .. } => self.inlines(content, false),
                Block::Heading { content, .. } => self.inlines(content, true),
                Block::Quote(inner) => {
                    let start = self.at;
                    self.blocks(inner);
                    if self.at > start {
                        self.ents.push(tl::types::MessageEntityBlockquote { collapsed: false, offset: start, length: self.at - start }.into());
                    }
                }
                Block::List { ordered, items } => {
                    for (k, item) in items.iter().enumerate() {
                        if k > 0 {
                            self.push("\n");
                        }
                        let bullet = if *ordered { format!("{}. ", k + 1) } else { "• ".to_owned() };
                        self.push(&bullet);
                        self.blocks(item);
                    }
                }
                Block::Code(code) => {
                    let start = self.at;
                    self.push(code);
                    if self.at > start {
                        self.ents.push(tl::types::MessageEntityPre { offset: start, length: self.at - start, language: String::new() }.into());
                    }
                }
                Block::Rule => self.push("———"),
                Block::Table(rows) => {
                    for (r, row) in rows.iter().enumerate() {
                        if r > 0 {
                            self.push("\n");
                        }
                        for (c, cell) in row.iter().enumerate() {
                            if c > 0 {
                                self.push("\t");
                            }
                            self.inlines(cell, false);
                        }
                    }
                }
                Block::Figure => {}
            }
        }
    }
}

/// IR → текст и сущности Telegram. `user` — ключ человека для упоминания (`InputUser`); нет — упоминание текстом.
pub fn from_ir(doc: &Document, user: &dyn Fn(&str) -> Option<tl::enums::InputUser>) -> (String, Vec<tl::enums::MessageEntity>) {
    let mut out = Out { text: String::new(), at: 0, ents: Vec::new(), user };
    out.blocks(&doc.blocks);
    let mut ents = merge(out.ents);
    ents.sort_by_key(|e| (e.offset(), -e.length()));
    (out.text, ents)
}

/// Соседние отметки одного вида — одна сущность (жирный текст из двух кусков).
fn merge(ents: Vec<tl::enums::MessageEntity>) -> Vec<tl::enums::MessageEntity> {
    use tl::enums::MessageEntity as E;
    let mut out: Vec<E> = Vec::with_capacity(ents.len());
    for e in ents {
        let joined = out.iter_mut().any(|p| {
            let same = std::mem::discriminant(p) == std::mem::discriminant(&e)
                && matches!(e, E::Bold(_) | E::Italic(_) | E::Underline(_) | E::Strike(_) | E::Code(_) | E::Spoiler(_));
            if same && p.offset() + p.length() == e.offset() {
                set_length(p, p.length() + e.length());
                true
            } else {
                false
            }
        });
        if !joined {
            out.push(e);
        }
    }
    out
}

fn set_length(e: &mut tl::enums::MessageEntity, n: i32) {
    use tl::enums::MessageEntity as E;
    match e {
        E::Bold(x) => x.length = n,
        E::Italic(x) => x.length = n,
        E::Underline(x) => x.length = n,
        E::Strike(x) => x.length = n,
        E::Code(x) => x.length = n,
        E::Spoiler(x) => x.length = n,
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tl::types as t;

    fn none(_: &str) -> Option<tl::enums::InputUser> {
        None
    }

    /// Сущности в UTF-16: эмодзи перед жирным, ссылка, упоминание, скрытый текст, код и цитата блоками.
    #[test]
    fn entities_to_ir() {
        let text = "😀 жирный и ссылка\n@anna Анна тайна\ncode\nцитата";
        let u = |s: &str| text.split_once(s).map(|(a, _)| utf16_len(a)).unwrap();
        let ents: Vec<tl::enums::MessageEntity> = vec![
            t::MessageEntityBold { offset: u("жирный"), length: 6 }.into(),
            t::MessageEntityTextUrl { offset: u("ссылка"), length: 6, url: "https://example.test/x".into() }.into(),
            t::MessageEntityMention { offset: u("@anna"), length: 5 }.into(),
            t::MessageEntityMentionName { offset: u("Анна"), length: 4, user_id: 42 }.into(),
            t::MessageEntitySpoiler { offset: u("тайна"), length: 5 }.into(),
            t::MessageEntityPre { offset: u("code"), length: 5, language: String::new() }.into(),
            t::MessageEntityBlockquote { collapsed: false, offset: u("цитата"), length: 6 }.into(),
        ];
        let d = to_ir(text, &ents);
        assert_eq!(d.blocks.len(), 4, "{d:#?}");
        let Block::Paragraph { content, .. } = &d.blocks[0] else { panic!() };
        assert!(content.iter().any(|i| matches!(i, Inline::Text { text, marks } if text == "жирный" && marks.bold)));
        assert!(content.iter().any(|i| matches!(i, Inline::Link { href, .. } if href == "https://example.test/x")));
        let Block::Paragraph { content, .. } = &d.blocks[1] else { panic!() };
        assert!(content.iter().any(|i| matches!(i, Inline::Link { href, .. } if href == "https://t.me/anna")));
        assert!(content.iter().any(|i| matches!(i, Inline::Mention { id, text } if id == "42" && text == "Анна")));
        assert!(content.iter().any(|i| matches!(i, Inline::Text { text, marks } if text == "тайна" && marks.spoiler)));
        assert_eq!(d.blocks[2], Block::Code("code".into()));
        assert!(
            matches!(&d.blocks[3], Block::Quote(q) if postman_core::channel::doc_text(&Document { blocks: q.clone(), ..Document::default() }) == "цитата")
        );
        // Обратно — тот же текст и те же виды сущностей.
        let (back, e2) = from_ir(&d, &|id| (id == "42").then_some(tl::enums::InputUser::UserSelf));
        assert_eq!(back, text);
        assert!(e2.iter().any(|e| matches!(e, tl::enums::MessageEntity::Bold(b) if b.offset == u("жирный") && b.length == 6)));
        assert!(e2.iter().any(|e| matches!(e, tl::enums::MessageEntity::InputMessageEntityMentionName(m) if m.offset == u("Анна"))));
        assert!(e2.iter().any(|e| matches!(e, tl::enums::MessageEntity::Pre(_))));
        assert!(e2.iter().any(|e| matches!(e, tl::enums::MessageEntity::Blockquote(_))));
        assert!(e2.iter().any(|e| matches!(e, tl::enums::MessageEntity::Spoiler(_))));
    }

    /// Сущности за концом текста и неверные ссылки не роняют разбор; простой текст — абзацы.
    #[test]
    fn broken_entities_and_plain() {
        let ents: Vec<tl::enums::MessageEntity> = vec![
            t::MessageEntityBold { offset: 3, length: 100 }.into(),
            t::MessageEntityTextUrl { offset: 0, length: 2, url: "javascript:alert(1)".into() }.into(),
            t::MessageEntityItalic { offset: -1, length: 2 }.into(),
        ];
        let d = to_ir("ab\ncd", &ents);
        assert_eq!(postman_core::channel::doc_text(&d), "ab\ncd");
        assert!(!format!("{d:?}").contains("javascript"));
        let (t, e) = from_ir(&postman_core::channel::text_doc("раз\nдва"), &none);
        assert_eq!((t.as_str(), e.len()), ("раз\nдва", 0));
    }

    /// Соседние куски жирного — одна сущность; список и заголовок — текстом.
    #[test]
    fn merge_and_lists() {
        let b = Marks { bold: true, ..Marks::default() };
        let d = Document {
            blocks: vec![
                Block::Heading { level: 1, content: vec![Inline::text("Итог")] },
                Block::Paragraph {
                    align: Align::Start,
                    content: vec![Inline::Text { text: "а".into(), marks: b }, Inline::Text { text: "б".into(), marks: b }],
                },
                Block::List { ordered: true, items: vec![vec![Block::Paragraph { align: Align::Start, content: vec![Inline::text("x")] }]] },
            ],
            ..Document::default()
        };
        let (t, e) = from_ir(&d, &none);
        assert_eq!(t, "Итог\nаб\n1. x");
        let bolds: Vec<_> = e.iter().filter(|e| matches!(e, tl::enums::MessageEntity::Bold(_))).collect();
        assert_eq!(bolds.len(), 2, "{e:?}");
        assert!(bolds.iter().any(|b| b.offset() == 5 && b.length() == 2));
    }
}
