//! IR письма → элементы gpui. Абзац — один StyledText с выделениями:
//! перенос строк, выравнивание и настоящий полужирный — силами текстового движка,
//! ссылки — кликабельные диапазоны того же текста. Картинки `cid:` (уже декодированные воркером) — отдельными
//! элементами между кусками текста абзаца; пока картинки нет — её подпись.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::Role;
use postman_ir::{Align, Block, Document, Inline, Marks};

use crate::roles::{Look, Pm, look};

/// Картинки письма по `cid:`, кадры для gpui.
pub type Images = HashMap<String, Arc<RenderImage>>;

pub fn blocks(bs: &[Block], key: &str, images: &Images, cx: &mut App) -> Vec<AnyElement> {
    blocks_marked(bs, key, images, &Hits::new(&[], 0, 0), cx)
}

/// Совпадения поиска при отрисовке письма (S5-10, S5-14): слова, номер текущего и счёт по порядку чтения — тот же
/// порядок, что видит пользователь (тема, затем текст сверху вниз). Число совпадений — [`Hits::seen`] после отрисовки.
pub struct Hits<'a> {
    words: &'a [String],
    current: usize,
    /// Сколько совпадений уже нарисовано: номер следующего.
    seen: Cell<usize>,
    /// Прокрутить к текущему (после перехода F3 / Shift+F3).
    reveal: Option<Reveal>,
}

/// Куда прокручивать: прокрутка области чтения и её view (перерисовать со сдвигом).
#[derive(Clone)]
pub struct Reveal {
    pub scroll: ScrollHandle,
    pub view: EntityId,
}

impl<'a> Hits<'a> {
    /// `start` — совпадений до текста (в теме письма).
    pub fn new(words: &'a [String], current: usize, start: usize) -> Self {
        Hits { words, current, seen: Cell::new(start), reveal: None }
    }

    pub fn with_reveal(self, reveal: Option<Reveal>) -> Self {
        Hits { reveal, ..self }
    }

    pub fn seen(&self) -> usize {
        self.seen.get()
    }

    /// Совпадения в тексте `text` следующими номерами; `Some(k)` — k-е из них текущее.
    fn take(&self, text: &str) -> (Vec<std::ops::Range<usize>>, Option<usize>) {
        if self.words.is_empty() {
            return (Vec::new(), None);
        }
        let spans = postman_core::fuzzy::term_spans(text, self.words);
        let first = self.seen.get();
        self.seen.set(first + spans.len());
        let current = self.current.checked_sub(first).filter(|&k| k < spans.len());
        (spans, current)
    }
}

/// Текст с совпадениями поиска поверх своих выделений `hl`: обычные — янтарной подложкой, текущее — плотной
/// (S5-14). Второе — начало текущего совпадения в тексте, если оно здесь.
pub fn hit_text(text: String, mut hl: Vec<(std::ops::Range<usize>, HighlightStyle)>, hits: &Hits, l: &Look) -> (StyledText, Option<usize>) {
    let (spans, current) = hits.take(&text);
    let (hit, now) = (crate::search::hit_style(l), crate::search::current_hit_style(l));
    let at = current.and_then(|k| spans.get(k)).map(|r| r.start);
    hl.extend(spans.into_iter().enumerate().map(|(k, r)| (r, if Some(k) == current { now } else { hit })));
    (StyledText::new(text).with_highlights(normalize(hl)), at)
}

/// Элемент текста с текущим совпадением: обёртка с невидимым холстом поверх — он после раскладки текста знает, где
/// совпадение, и сдвигает прокрутку области чтения так, чтобы оно было видно (на трети высоты). Без перехода
/// (`reveal` нет) — только обёртка: раскладка та же, текст не прыгает, когда совпадение становится текущим.
pub fn reveal_at(el: AnyElement, layout: TextLayout, at: usize, hits: &Hits) -> AnyElement {
    let reveal = hits.reveal.clone();
    div()
        .relative()
        .child(el)
        .children(reveal.map(|r| {
            // Холст — после текста в той же обёртке: раскладка и prepaint текста к этому моменту сделаны.
            canvas(
                move |_, window, _| {
                    let Some(pos) = layout.position_for_index(at) else { return };
                    let (view, offset, max) = (r.scroll.bounds(), r.scroll.offset(), r.scroll.max_offset());
                    if pos.y >= view.top() && pos.y + layout.line_height() <= view.bottom() {
                        return;
                    }
                    let y = (offset.y + view.top() + view.size.height / 3. - pos.y).min(px(0.)).max(-max.y);
                    if (y - offset.y).abs() < px(1.) {
                        return;
                    }
                    r.scroll.set_offset(point(offset.x, y));
                    // Область чтения закэширована: новый сдвиг — со следующего кадра, с её перерисовкой.
                    let view = r.view;
                    window.on_next_frame(move |_, cx| cx.notify(view));
                    window.request_animation_frame();
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full()
        }))
        .into_any_element()
}

/// Блоки с подсветкой совпадений поиска (S5-10): слова `hits` (`postman_core::fuzzy::term_spans`) — янтарной
/// подложкой поверх своих выделений, текущее — плотной.
pub fn blocks_marked(bs: &[Block], key: &str, images: &Images, hits: &Hits, cx: &mut App) -> Vec<AnyElement> {
    bs.iter().enumerate().map(|(n, b)| block(b, &format!("{key}.{n}"), images, hits, cx)).collect()
}

/// Все `cid:` картинок документа, по порядку.
pub fn cids(doc: &Document) -> Vec<String> {
    fn inl(items: &[Inline], out: &mut Vec<String>) {
        for i in items {
            match i {
                Inline::Image { cid, .. } => out.push(cid.clone()),
                Inline::Link { content, .. } => inl(content, out),
                _ => {}
            }
        }
    }
    fn blk(bs: &[Block], out: &mut Vec<String>) {
        for b in bs {
            match b {
                Block::Paragraph { content, .. } | Block::Heading { content, .. } => inl(content, out),
                Block::Quote(inner) => blk(inner, out),
                Block::List { items, .. } => items.iter().for_each(|i| blk(i, out)),
                Block::Table(rows) => rows.iter().flatten().for_each(|c| inl(c, out)),
                Block::Code(_) | Block::Rule => {}
            }
        }
    }
    let mut out = Vec::new();
    blk(&doc.blocks, &mut out);
    out
}

fn block(b: &Block, key: &str, images: &Images, hits: &Hits, cx: &mut App) -> AnyElement {
    let l = &*look(cx);
    match b {
        Block::Paragraph { align, content } => {
            let d = div().child(paragraph(content, key, images, hits, l));
            match align {
                Align::Center => d.text_center(),
                Align::End => d.text_right(),
                _ => d,
            }
            .into_any_element()
        }
        Block::Heading { level, content } => {
            let size = match level {
                1 => 22.,
                2 => 19.,
                3 => 17.,
                _ => 15.,
            };
            div().text_size(px(size)).font_weight(FontWeight::SEMIBOLD).child(paragraph(content, key, images, hits, l)).into_any_element()
        }
        Block::Quote(inner) => div()
            .flex()
            .flex_col()
            .gap_2()
            .pl_3()
            .border_l_2()
            .border_color(l.c(Role::StrokeStrong))
            .text_color(l.text2())
            .children(blocks_marked(inner, key, images, hits, cx))
            .into_any_element(),
        Block::List { ordered, items } => div()
            .flex()
            .flex_col()
            .gap_1()
            .children(items.iter().enumerate().map(|(n, item)| {
                let marker = if *ordered { format!("{}.", n + 1) } else { "•".into() };
                div().flex().gap_2().child(div().w(px(16.)).child(marker)).child(div().flex().flex_col().flex_1().children(blocks_marked(
                    item,
                    &format!("{key}.{n}"),
                    images,
                    hits,
                    cx,
                )))
            }))
            .into_any_element(),
        Block::Code(code) => div().p_2().rounded_md().bg(l.fill()).font_family("Cascadia Mono").child(code.clone()).into_any_element(),
        Block::Rule => div().h(px(1.)).bg(l.border()).into_any_element(),
        Block::Table(rows) => div()
            .flex()
            .flex_col()
            .children(rows.iter().enumerate().map(|(r, row)| {
                div().flex().when(r % 2 == 1, |d| d.bg(l.fill_passive())).children(
                    row.iter()
                        .enumerate()
                        .map(|(c, cell)| div().min_w(px(90.)).px_2().py_1().child(paragraph(cell, &format!("{key}.{r}.{c}"), images, hits, l))),
                )
            }))
            .into_any_element(),
    }
}

/// Инлайны → (текст, выделения, ссылки).
fn flatten(
    items: &[Inline],
    text: &mut String,
    hl: &mut Vec<(std::ops::Range<usize>, HighlightStyle)>,
    links: &mut Vec<(std::ops::Range<usize>, String, bool)>,
    l: &Look,
) {
    for i in items {
        match i {
            Inline::Text { text: t, marks } => {
                let start = text.len();
                text.push_str(t);
                if let Some(s) = style(*marks, l) {
                    hl.push((start..text.len(), s));
                }
            }
            Inline::LineBreak => text.push('\n'),
            Inline::Image { alt, .. } => {
                let start = text.len();
                text.push_str(&if alt.is_empty() { tr!("reading-image") } else { tr!("reading-image-alt", alt = alt.as_str()) });
                hl.push((start..text.len(), HighlightStyle { color: Some(l.text2()), ..Default::default() }));
            }
            Inline::Link { href, suspicious, content, .. } => {
                if *suspicious {
                    let s = text.len();
                    text.push('⚠');
                    hl.push((s..text.len(), HighlightStyle { color: Some(l.pm(Pm::ThreatFg)), ..Default::default() }));
                }
                let start = text.len();
                flatten(content, text, hl, links, l);
                let range = start..text.len();
                hl.push((
                    range.clone(),
                    HighlightStyle {
                        color: Some(l.accent_text()),
                        underline: Some(UnderlineStyle { thickness: px(1.), color: None, wavy: false }),
                        ..Default::default()
                    },
                ));
                links.push((range, href.clone(), *suspicious));
            }
        }
    }
}

fn style(m: Marks, l: &Look) -> Option<HighlightStyle> {
    if m == Marks::default() {
        return None;
    }
    Some(HighlightStyle {
        font_weight: m.bold.then_some(FontWeight::BOLD),
        font_style: m.italic.then_some(FontStyle::Italic),
        underline: m.underline.then_some(UnderlineStyle { thickness: px(1.), color: None, wavy: false }),
        strikethrough: m.strike.then_some(StrikethroughStyle { thickness: px(1.), color: None }),
        background_color: m.code.then(|| l.fill()),
        ..Default::default()
    })
}

/// Куски абзаца: текст и показанные картинки верхнего уровня (в ссылке картинка остаётся подписью).
#[derive(Debug, PartialEq)]
enum Piece<'a> {
    Text(&'a [Inline]),
    Image(&'a str),
}

fn pieces<'a>(items: &'a [Inline], shown: impl Fn(&str) -> bool) -> Vec<Piece<'a>> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, item) in items.iter().enumerate() {
        if let Inline::Image { cid, .. } = item
            && shown(cid)
        {
            out.push(Piece::Text(&items[start..i]));
            out.push(Piece::Image(cid));
            start = i + 1;
        }
    }
    out.push(Piece::Text(&items[start..]));
    // Пустые куски и одни переводы строк вокруг картинки не дают пустых строк.
    out.retain(|p| match p {
        Piece::Text(t) => t.iter().any(|i| match i {
            Inline::Text { text, .. } => !text.trim().is_empty(),
            Inline::LineBreak => false,
            _ => true,
        }),
        Piece::Image(_) => true,
    });
    out
}

fn paragraph(items: &[Inline], key: &str, images: &Images, hits: &Hits, l: &Look) -> AnyElement {
    let parts = pieces(items, |cid| images.contains_key(cid));
    if !parts.iter().any(|p| matches!(p, Piece::Image(_))) {
        return text(items, key, hits, l);
    }
    div()
        .flex()
        .flex_col()
        .gap_1()
        .children(parts.into_iter().enumerate().map(|(n, p)| match p {
            Piece::Text(t) => text(t, &format!("{key}.{n}"), hits, l),
            Piece::Image(cid) => match images.get(cid) {
                Some(image) => picture(image),
                None => div().into_any_element(),
            },
        }))
        .into_any_element()
}

/// Картинка в своём размере, не шире области чтения; высота — по пропорции (родитель — flex-колонка абзаца).
fn picture(image: &Arc<RenderImage>) -> AnyElement {
    let size = image.size(0);
    crate::preview::picture(image, px(size.width.0 as f32)).into_any_element()
}

fn text(items: &[Inline], key: &str, hits: &Hits, l: &Look) -> AnyElement {
    let (mut text, mut hl, mut links) = (String::new(), Vec::new(), Vec::new());
    flatten(items, &mut text, &mut hl, &mut links, l);
    let (styled, at) = hit_text(text, hl, hits, l);
    let layout = styled.layout().clone();
    let el = if links.is_empty() {
        styled.into_any_element()
    } else {
        let ranges: Vec<_> = links.iter().map(|(r, _, _)| r.clone()).collect();
        let hrefs: Vec<String> = links.into_iter().map(|(_, h, _)| h).collect();
        InteractiveText::new(SharedString::from(key.to_string()), styled)
            .on_click(ranges, move |ix, _window, cx| {
                if let Some(h) = hrefs.get(ix) {
                    cx.open_url(h);
                }
            })
            .into_any_element()
    };
    match at {
        Some(at) => reveal_at(el, layout, at, hits),
        None => el,
    }
}

/// gpui требует непересекающиеся выделения по возрастанию: ссылка поверх жирного и т.п. сливаем.
fn normalize(mut hl: Vec<(std::ops::Range<usize>, HighlightStyle)>) -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
    let mut bounds: Vec<usize> = hl.iter().flat_map(|(r, _)| [r.start, r.end]).collect();
    bounds.sort_unstable();
    bounds.dedup();
    let mut out = Vec::new();
    for w in bounds.windows(2) {
        let (a, b) = (w[0], w[1]);
        let mut merged: Option<HighlightStyle> = None;
        for (r, s) in &hl {
            if r.start <= a && b <= r.end {
                merged = Some(match merged {
                    None => *s,
                    Some(m) => m.highlight(*s),
                });
            }
        }
        if let Some(s) = merged {
            out.push((a..b, s));
        }
    }
    hl.clear();
    out
}

#[cfg(test)]
mod tests {
    // Не `use super::*`: вместе с gpui пришёл бы атрибут `gpui::test`, он перекрывает `#[test]`.
    use super::{Piece, cids, pieces};
    use postman_ir::{Align, Block, Document, Inline};

    fn image(cid: &str) -> Inline {
        Inline::Image { cid: cid.into(), alt: String::new() }
    }

    #[test]
    fn pictures_split_the_paragraph() {
        let items = [image("logo"), Inline::LineBreak, Inline::text("Коллеги"), image("missing"), image("chart")];
        let p = pieces(&items, |c| c != "missing");
        assert_eq!(p.len(), 3, "{p:?}");
        assert_eq!(p[0], Piece::Image("logo"));
        // Текст и подпись картинки, которой нет, — одним куском.
        assert_eq!(p[1], Piece::Text(&items[1..4]));
        assert_eq!(p[2], Piece::Image("chart"));
        assert_eq!(pieces(&items[1..3], |_| false), [Piece::Text(&items[1..3])]);
    }

    #[test]
    fn cids_in_reading_order() {
        let doc = Document {
            blocks: vec![
                Block::Paragraph { align: Align::Start, content: vec![image("a")] },
                Block::Quote(vec![Block::Paragraph {
                    align: Align::Start,
                    content: vec![Inline::Link { href: "x".into(), host: None, suspicious: false, content: vec![image("b")] }],
                }]),
                Block::Table(vec![vec![vec![image("c")]]]),
            ],
            remote_images_blocked: 0,
            history: None,
        };
        assert_eq!(cids(&doc), ["a", "b", "c"]);
    }
}
