//! HTML -> IR по белому списку. Всё, чего нет в списке, либо прозрачно
//! (берутся только дети), либо выбрасывается вместе с поддеревом.

use html5ever::tendril::TendrilSink;
use html5ever::{ParseOpts, parse_document};
use markup5ever_rcdom::{Handle, NodeData, RcDom};

use crate::history::{self, Mark, MarkKind};
use crate::{Align, Block, Document, Inline, Marks, link, plain_text, style};

/// Элементы, поддерево которых выбрасывается целиком.
const DROPPED: &[&str] = &[
    "head", "script", "style", "iframe", "frame", "frameset", "object", "embed", "applet", "form", "input", "button", "select", "textarea", "option",
    "svg", "math", "link", "meta", "base", "title", "noscript", "template", "audio", "video", "canvas", "map", "dialog",
];

/// Элементы, задающие границу блока (содержимое становится абзацем).
const BLOCKISH: &[&str] = &[
    "p",
    "div",
    "center",
    "section",
    "article",
    "header",
    "footer",
    "main",
    "nav",
    "aside",
    "address",
    "figure",
    "figcaption",
    "dl",
    "dt",
    "dd",
    "caption",
    "body",
    "html",
];

/// Не больше этой глубины вложенности — защита стека от «бомб» из тегов.
const MAX_DEPTH: usize = 256;

/// Выше этой оценочной глубины html5ever работает за O(n²) — показываем текстом.
const MAX_ESTIMATED_DEPTH: usize = 2000;

pub fn from_html(html: &str) -> Document {
    if estimate_depth(html) > MAX_ESTIMATED_DEPTH {
        return crate::from_plain(&strip_tags(html));
    }
    let dom = parse_document(RcDom::default(), ParseOpts::default()).one(html);
    let mut b = Builder::default();
    let mut sink = Sink { top: true, ..Sink::default() };
    b.children(&dom.document, Ctx::default(), &mut sink);
    let mut blocks = sink.finish();
    let history = history::detect(&mut blocks, &b.quotes);
    Document { blocks, remote_images_blocked: b.remote_images, history }
}

#[derive(Clone, Copy, Default)]
struct Ctx {
    marks: Marks,
    align: Align,
    pre: bool,
    in_link: bool,
    in_table: bool,
    depth: usize,
}

#[derive(Default)]
struct Sink {
    blocks: Vec<Block>,
    para: Vec<Inline>,
    align: Align,
    /// Поток блоков верхнего уровня письма (не цитата, не ячейка): здесь отмечается история переписки.
    top: bool,
}

impl Sink {
    fn flush(&mut self) {
        trim_end(&mut self.para);
        while matches!(self.para.last(), Some(Inline::LineBreak)) {
            self.para.pop();
            trim_end(&mut self.para);
        }
        if !self.para.is_empty() {
            let content = std::mem::take(&mut self.para);
            self.blocks.push(Block::Paragraph { align: self.align, content });
        }
        self.align = Align::Start;
    }

    fn block(&mut self, b: Block) {
        self.flush();
        self.blocks.push(b);
    }

    fn inline(&mut self, i: Inline, align: Align) {
        if self.para.is_empty() {
            if matches!(i, Inline::LineBreak) {
                return;
            }
            self.align = align;
        }
        self.para.push(i);
    }

    fn text(&mut self, s: &str, ctx: Ctx) {
        let text = if ctx.pre {
            s.to_string()
        } else {
            let collapsed = collapse_ws(s);
            let at_start = match self.para.last() {
                None | Some(Inline::LineBreak) => true,
                Some(Inline::Text { text, .. }) => text.ends_with(' '),
                _ => false,
            };
            if at_start { collapsed.trim_start().to_string() } else { collapsed }
        };
        if text.is_empty() {
            return;
        }
        if let Some(Inline::Text { text: last, marks }) = self.para.last_mut()
            && *marks == ctx.marks
        {
            last.push_str(&text);
            return;
        }
        self.inline(Inline::Text { text, marks: ctx.marks }, ctx.align);
    }

    fn finish(mut self) -> Vec<Block> {
        self.flush();
        self.blocks
    }

    /// Свести всё содержимое к инлайнам (для заголовков, ссылок, ячеек).
    fn into_inlines(mut self) -> Vec<Inline> {
        self.flush();
        let mut out = Vec::new();
        for b in self.blocks {
            if !out.is_empty() {
                out.push(Inline::LineBreak);
            }
            flatten_block(b, &mut out);
        }
        out
    }
}

fn flatten_block(b: Block, out: &mut Vec<Inline>) {
    match b {
        Block::Paragraph { content, .. } | Block::Heading { content, .. } => out.extend(content),
        Block::Quote(bs) => {
            for (n, b) in bs.into_iter().enumerate() {
                if n > 0 {
                    out.push(Inline::LineBreak);
                }
                flatten_block(b, out);
            }
        }
        Block::List { items, .. } => {
            for (n, item) in items.into_iter().enumerate() {
                if n > 0 {
                    out.push(Inline::LineBreak);
                }
                for b in item {
                    flatten_block(b, out);
                }
            }
        }
        Block::Code(s) => out.push(Inline::Text { text: s, marks: Marks { code: true, ..Default::default() } }),
        Block::Rule => {}
        Block::Table(rows) => {
            for (n, row) in rows.into_iter().enumerate() {
                if n > 0 {
                    out.push(Inline::LineBreak);
                }
                for (m, cell) in row.into_iter().enumerate() {
                    if m > 0 {
                        out.push(Inline::text(" "));
                    }
                    out.extend(cell);
                }
            }
        }
    }
}

fn trim_end(para: &mut Vec<Inline>) {
    while let Some(Inline::Text { text, marks }) = para.last_mut() {
        if marks.code {
            break;
        }
        let t = text.trim_end().len();
        text.truncate(t);
        if text.is_empty() {
            para.pop();
        } else {
            break;
        }
    }
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut ws = false;
    for c in s.chars() {
        // NBSP сохраняем: им часто выравнивают текст вручную.
        if c.is_whitespace() && c != '\u{a0}' {
            ws = true;
        } else {
            if ws {
                out.push(' ');
                ws = false;
            }
            out.push(c);
        }
    }
    if ws {
        out.push(' ');
    }
    out
}

#[derive(Default)]
struct Builder {
    remote_images: usize,
    /// Цитаты, отмеченные разметкой клиентов (S5-05).
    quotes: Vec<Mark>,
}

impl Builder {
    fn children(&mut self, node: &Handle, ctx: Ctx, sink: &mut Sink) {
        if ctx.depth > MAX_DEPTH {
            return;
        }
        let ctx = Ctx { depth: ctx.depth + 1, ..ctx };
        for child in node.children.borrow().iter() {
            self.node(child, ctx, sink);
        }
    }

    fn node(&mut self, node: &Handle, ctx: Ctx, sink: &mut Sink) {
        match &node.data {
            NodeData::Text { contents } => sink.text(&contents.borrow(), ctx),
            NodeData::Element { name, .. } => {
                let tag: &str = &name.local;
                self.element(node, tag, ctx, sink);
            }
            NodeData::Document => self.children(node, ctx, sink),
            // Комментарии (включая <!--[if mso]-->), doctype, PI — выбрасываются.
            _ => {}
        }
    }

    fn element(&mut self, node: &Handle, tag: &str, ctx: Ctx, sink: &mut Sink) {
        if DROPPED.contains(&tag) {
            return;
        }
        let Some(kind) = sink.top.then(|| quote_mark(node, tag)).flatten() else {
            return self.element_body(node, tag, ctx, sink);
        };
        sink.flush();
        let start = sink.blocks.len();
        self.element_body(node, tag, ctx, sink);
        sink.flush();
        self.quotes.push(Mark { kind, start, end: sink.blocks.len() });
    }

    fn element_body(&mut self, node: &Handle, tag: &str, mut ctx: Ctx, sink: &mut Sink) {
        if let Some(s) = attr(node, "style") {
            style::apply(&s, &mut ctx.marks, &mut ctx.align);
        }
        if let Some(a) = attr(node, "align").and_then(|a| style::parse_align(&a)) {
            ctx.align = a;
        }
        match tag {
            "br" => sink.inline(Inline::LineBreak, ctx.align),
            "hr" => sink.block(Block::Rule),
            "b" | "strong" => self.marked(node, ctx, sink, |m| m.bold = true),
            "i" | "em" | "cite" | "var" | "dfn" => self.marked(node, ctx, sink, |m| m.italic = true),
            "u" | "ins" => self.marked(node, ctx, sink, |m| m.underline = true),
            "s" | "strike" | "del" => self.marked(node, ctx, sink, |m| m.strike = true),
            "code" | "tt" | "kbd" | "samp" => self.marked(node, ctx, sink, |m| m.code = true),
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = tag.as_bytes()[1] - b'0';
                let content = self.sub(node, ctx).into_inlines();
                if !content.is_empty() {
                    sink.block(Block::Heading { level, content });
                }
            }
            "blockquote" => {
                let inner = self.sub(node, Ctx { align: Align::Start, ..ctx }).finish();
                if !inner.is_empty() {
                    sink.block(Block::Quote(inner));
                }
            }
            "ul" | "ol" => self.list(node, tag == "ol", ctx, sink),
            "pre" | "xmp" | "listing" | "plaintext" => {
                let mut s = String::new();
                raw_text(node, &mut s, 0);
                let s = s.strip_prefix('\n').unwrap_or(&s).trim_end().to_string();
                if !s.is_empty() {
                    sink.block(Block::Code(s));
                }
            }
            "table" => self.table(node, ctx, sink),
            "a" => self.link(node, ctx, sink),
            "img" => self.image(node, ctx, sink),
            t if BLOCKISH.contains(&t) || t == "li" || t == "tr" || t == "td" || t == "th" => {
                sink.flush();
                self.children(node, ctx, sink);
                sink.flush();
            }
            // span, font, small, big, sup, sub, label, неизвестные теги — прозрачны.
            _ => self.children(node, ctx, sink),
        }
    }

    fn marked(&mut self, node: &Handle, mut ctx: Ctx, sink: &mut Sink, f: impl FnOnce(&mut Marks)) {
        f(&mut ctx.marks);
        // style на самом элементе уже применён и должен перекрывать тег.
        if let Some(s) = attr(node, "style") {
            style::apply(&s, &mut ctx.marks, &mut ctx.align);
        }
        self.children(node, ctx, sink);
    }

    fn sub(&mut self, node: &Handle, ctx: Ctx) -> Sink {
        let mut s = Sink::default();
        self.children(node, ctx, &mut s);
        s
    }

    fn list(&mut self, node: &Handle, ordered: bool, ctx: Ctx, sink: &mut Sink) {
        let mut items = Vec::new();
        let mut loose = Sink::default();
        for child in node.children.borrow().iter() {
            if element_name(child).as_deref() == Some("li") {
                let prev = std::mem::take(&mut loose).finish();
                if !prev.is_empty() {
                    items.push(prev);
                }
                let item = self.sub(child, Ctx { align: Align::Start, ..ctx }).finish();
                if !item.is_empty() {
                    items.push(item);
                }
            } else {
                self.node(child, ctx, &mut loose);
            }
        }
        let rest = loose.finish();
        if !rest.is_empty() {
            items.push(rest);
        }
        if !items.is_empty() {
            sink.block(Block::List { ordered, items });
        }
    }

    fn table(&mut self, node: &Handle, ctx: Ctx, sink: &mut Sink) {
        let nested = ctx.in_table;
        let ctx = Ctx { in_table: true, ..ctx };
        let rows = rows_of(node);
        let cols = rows.iter().map(|r| cells_of(r).len()).max().unwrap_or(0);
        // Вложенная таблица — тоже вёрстка (ctx.in_table уже выставлен для детей).
        let layout = nested || cols <= 1 || has_descendant(node, "table") || is_presentation(node);
        if layout {
            // Вёрсточная таблица: линеаризуем ячейки в поток блоков.
            sink.flush();
            for row in &rows {
                for cell in cells_of(row) {
                    sink.flush();
                    self.children(&cell, ctx, sink);
                    sink.flush();
                }
            }
            return;
        }
        let mut grid = Vec::new();
        for row in &rows {
            let cells: Vec<Vec<Inline>> = cells_of(row)
                .iter()
                .map(|c| {
                    let mut cctx = ctx;
                    if element_name(c).as_deref() == Some("th") {
                        cctx.marks.bold = true;
                    }
                    self.sub(c, cctx).into_inlines()
                })
                .collect();
            if cells.iter().any(|c| !c.is_empty()) {
                grid.push(cells);
            }
        }
        if !grid.is_empty() {
            sink.block(Block::Table(grid));
        }
    }

    fn link(&mut self, node: &Handle, ctx: Ctx, sink: &mut Sink) {
        if ctx.in_link {
            return self.children(node, ctx, sink);
        }
        let content = self.sub(node, Ctx { in_link: true, ..ctx }).into_inlines();
        let checked = attr(node, "href").and_then(|h| link::check_link(&h, &plain_text(&content)));
        match checked {
            Some(l) if !content.is_empty() => sink.inline(Inline::Link { href: l.href, host: l.host, suspicious: l.suspicious, content }, ctx.align),
            // Запрещённая схема или пустая ссылка: остаётся только текст.
            _ => {
                for i in content {
                    sink.inline(i, ctx.align);
                }
            }
        }
    }

    fn image(&mut self, node: &Handle, ctx: Ctx, sink: &mut Sink) {
        let alt = attr(node, "alt").map(|a| collapse_ws(&a).trim().to_string()).unwrap_or_default();
        let src = attr(node, "src").unwrap_or_default();
        let src = src.trim();
        if let Some(cid) = src.get(..4).filter(|p| p.eq_ignore_ascii_case("cid:")).map(|_| &src[4..]) {
            let (width, height) = image_size(node);
            sink.inline(Inline::Image { cid: cid.to_string(), alt, width, height }, ctx.align);
            return;
        }
        if !src.is_empty() {
            self.remote_images += 1;
        }
        if !alt.is_empty() {
            sink.text(&format!("[{alt}]"), ctx);
        }
    }
}

/// Задуманный размер картинки (T1-10б): атрибуты `width`/`height`, поверх них — `style` (CSS главнее
/// атрибутов-подсказок, как в браузере).
fn image_size(node: &Handle) -> (Option<u32>, Option<u32>) {
    let mut w = attr(node, "width").and_then(|v| style::attr_px(&v));
    let mut h = attr(node, "height").and_then(|v| style::attr_px(&v));
    if let Some(css) = attr(node, "style") {
        let (cw, ch) = style::image_size(&css);
        w = cw.or(w);
        h = ch.or(h);
    }
    (w, h)
}

/// Разметка цитаты почтовыми клиентами: класс, id или `type=cite`.
fn quote_mark(node: &Handle, tag: &str) -> Option<MarkKind> {
    if tag == "blockquote" && attr(node, "type").is_some_and(|t| t.trim().eq_ignore_ascii_case("cite")) {
        return Some(MarkKind::Range);
    }
    let class = attr(node, "class").unwrap_or_default();
    let has = |c: &str| class.split_ascii_whitespace().any(|x| x.eq_ignore_ascii_case(c));
    // Gmail, Thunderbird (строка «… wrote:» над цитатой), Yahoo, Proton.
    if ["gmail_quote", "gmail_quote_container", "moz-cite-prefix", "yahoo_quoted", "protonmail_quote"].iter().any(|c| has(c)) {
        return Some(MarkKind::Range);
    }
    // Outlook в браузере: шапка ответа и метка, после которой идёт исходное письмо.
    let id = attr(node, "id").unwrap_or_default();
    ["divRplyFwdMsg", "appendonsend"].iter().any(|i| id.eq_ignore_ascii_case(i)).then_some(MarkKind::From)
}

const VOID: &[&str] = &["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track", "wbr"];

/// Грубая оценка максимальной глубины вложенности по тегам, без разбора.
/// Незакрытые `<p>`/`<li>` её завышают — это безопасная сторона.
fn estimate_depth(html: &str) -> usize {
    let b = html.as_bytes();
    let (mut depth, mut max, mut i) = (0usize, 0usize, 0usize);
    while let Some(off) = b[i..].iter().position(|&c| c == b'<') {
        i += off + 1;
        match b.get(i) {
            Some(b'/') => depth = depth.saturating_sub(1),
            Some(c) if c.is_ascii_alphabetic() => {
                let end = b[i..].iter().position(|c| !c.is_ascii_alphanumeric()).map_or(b.len(), |e| i + e);
                let name = html[i..end].to_ascii_lowercase();
                let close = b[end..].iter().position(|&c| c == b'>').map(|e| end + e);
                let self_closing = close.is_some_and(|e| e > 0 && b[e - 1] == b'/');
                if !VOID.contains(&name.as_str()) && !self_closing {
                    depth += 1;
                    max = max.max(depth);
                }
            }
            _ => {}
        }
    }
    max
}

/// Запасной путь для «бомб»: только текст, теги и сущности (кроме базовых) выброшены.
fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&nbsp;", " ").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&amp;", "&")
}

fn attr(node: &Handle, name: &str) -> Option<String> {
    match &node.data {
        NodeData::Element { attrs, .. } => {
            attrs.borrow().iter().find(|a| a.name.ns.is_empty() && &*a.name.local == name).map(|a| a.value.to_string())
        }
        _ => None,
    }
}

fn element_name(node: &Handle) -> Option<String> {
    match &node.data {
        NodeData::Element { name, .. } => Some(name.local.to_string()),
        _ => None,
    }
}

fn is_presentation(node: &Handle) -> bool {
    attr(node, "role").is_some_and(|r| r.eq_ignore_ascii_case("presentation"))
}

fn has_descendant(node: &Handle, tag: &str) -> bool {
    node.children.borrow().iter().any(|c| element_name(c).as_deref() == Some(tag) || has_descendant(c, tag))
}

/// Строки таблицы (через thead/tbody/tfoot), не заходя во вложенные таблицы.
fn rows_of(table: &Handle) -> Vec<Handle> {
    let mut out = Vec::new();
    for c in table.children.borrow().iter() {
        match element_name(c).as_deref() {
            Some("tr") => out.push(c.clone()),
            Some("thead" | "tbody" | "tfoot") => out.extend(rows_of(c)),
            _ => {}
        }
    }
    out
}

fn cells_of(row: &Handle) -> Vec<Handle> {
    row.children.borrow().iter().filter(|c| matches!(element_name(c).as_deref(), Some("td" | "th"))).cloned().collect()
}

fn raw_text(node: &Handle, out: &mut String, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    for c in node.children.borrow().iter() {
        match &c.data {
            NodeData::Text { contents } => out.push_str(&contents.borrow()),
            NodeData::Element { name, .. } if &*name.local == "br" => out.push('\n'),
            NodeData::Element { name, .. } if DROPPED.contains(&&*name.local) => {}
            NodeData::Element { .. } => raw_text(c, out, depth + 1),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Inline {
        Inline::text(s)
    }

    fn para(content: Vec<Inline>) -> Block {
        Block::Paragraph { align: Align::Start, content }
    }

    #[test]
    fn drops_active_content() {
        let d = from_html(
            r#"<html><head><style>p{color:red}</style><script>alert(1)</script></head>
            <body onload="x()"><p onclick="y()">Привет</p><iframe src="https://e.x"></iframe>
            <form><input value="pwd"></form><!--[if mso]><p>mso</p><![endif]--><svg><text>s</text></svg></body></html>"#,
        );
        assert_eq!(d.blocks, vec![para(vec![text("Привет")])]);
    }

    #[test]
    fn marks_from_tags_and_style() {
        let d =
            from_html(r#"<p><b>жир</b> <span style="font-style: italic; color: red">курс</span> <span style="font-weight:400"><b>не</b></span></p>"#);
        let Block::Paragraph { content, .. } = &d.blocks[0] else { panic!() };
        assert!(matches!(&content[0], Inline::Text { marks, .. } if marks.bold));
        assert!(matches!(&content[2], Inline::Text { text, marks } if text == "курс" && marks.italic));
        // <b> внутри font-weight:400 — снова жирный.
        assert!(matches!(&content[4], Inline::Text { text, marks } if text == "не" && marks.bold));
    }

    #[test]
    fn links_and_images() {
        let d = from_html(
            r#"<p><a href="javascript:evil()">клик</a> <a href="https://evil.example">https://bank.ru</a>
            <img src="https://track.example/p.gif" width=1><img src="cid:logo@x" alt="Лого"></p>"#,
        );
        assert_eq!(d.remote_images_blocked, 1);
        let Block::Paragraph { content, .. } = &d.blocks[0] else { panic!() };
        assert_eq!(content[0], text("клик "));
        assert!(matches!(&content[1], Inline::Link { suspicious: true, .. }));
        assert!(matches!(&content[3], Inline::Image { cid, width: None, height: None, .. } if cid == "logo@x"));
    }

    /// T1-10б: размер из атрибутов и `style`; CSS главнее атрибутов; мусор, проценты и огромное — не размер.
    #[test]
    fn image_sizes() {
        let size = |html: &str| {
            let d = from_html(html);
            let Block::Paragraph { content, .. } = &d.blocks[0] else { panic!("{d:?}") };
            let Inline::Image { width, height, .. } = &content[0] else { panic!("{content:?}") };
            (*width, *height)
        };
        assert_eq!(size(r#"<p><img src="cid:a" width="120" height=40></p>"#), (Some(120), Some(40)));
        assert_eq!(size(r#"<p><img src="cid:a" width="120px"></p>"#), (Some(120), None));
        // Outlook: атрибуты в px и они же в дюймах в style.
        assert_eq!(size(r#"<p><img src="cid:a" width=200 height=80 style='width:1.25in;height:.4166in'></p>"#), (Some(120), Some(40)));
        assert_eq!(size(r#"<p><img src="cid:a" style="height: 30pt !important; WIDTH: 90PX"></p>"#), (Some(90), Some(40)));
        assert_eq!(size(r#"<p><img src="cid:a" width="50%" height="0"></p>"#), (None, None));
        assert_eq!(size(r#"<p><img src="cid:a" width="999999" height="-5" style="width:auto"></p>"#), (None, None));
        assert_eq!(size(r#"<p><img src="cid:a" width="x" height="NaN" style="width: 2em; height: 1e9px"></p>"#), (None, None));
        assert_eq!(size(r#"<p><img src="cid:a" width="12.6"></p>"#), (Some(13), None));
    }

    #[test]
    fn layout_tables_are_linearized() {
        let d = from_html("<table><tr><td><table><tr><td>a</td><td>b</td></tr></table></td></tr><tr><td>c</td></tr></table>");
        assert_eq!(d.blocks, vec![para(vec![text("a")]), para(vec![text("b")]), para(vec![text("c")])]);
        let d = from_html("<table><tr><th>Имя</th><th>Сумма</th></tr><tr><td>A</td><td>5</td></tr></table>");
        assert!(matches!(&d.blocks[0], Block::Table(rows) if rows.len() == 2 && rows[0].len() == 2));
    }

    #[test]
    fn structure() {
        let d = from_html("<h2>Тема</h2><blockquote><p>цитата</p></blockquote><ul><li>один</li><li>два</li></ul><pre>  x\n  y</pre><hr>");
        assert!(matches!(&d.blocks[0], Block::Heading { level: 2, .. }));
        assert!(matches!(&d.blocks[1], Block::Quote(b) if b.len() == 1));
        assert!(matches!(&d.blocks[2], Block::List { ordered: false, items } if items.len() == 2));
        assert_eq!(d.blocks[3], Block::Code("  x\n  y".into()));
        assert_eq!(d.blocks[4], Block::Rule);
    }

    #[test]
    fn whitespace_and_breaks() {
        let d = from_html("<div>  строка\n  один<br>строка   два<br><br></div><div><br></div>");
        assert_eq!(d.blocks, vec![para(vec![text("строка один"), Inline::LineBreak, text("строка два")])]);
    }

    #[test]
    fn deep_nesting_does_not_overflow() {
        let html = "<span>".repeat(50_000) + "x";
        let _ = from_html(&html);
    }

    #[test]
    fn nesting_bomb_falls_back_to_text() {
        let html = "<div>".repeat(100_000) + "<b>текст</b>";
        let t = std::time::Instant::now();
        let d = from_html(&html);
        assert!(t.elapsed().as_secs() < 2, "{:?}", t.elapsed());
        assert_eq!(d.blocks, vec![para(vec![text("текст")])]);
        assert_eq!(estimate_depth("<p>a<br><img src=x/><p>b</p></p>"), 2);
    }
}
