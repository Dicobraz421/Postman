use crate::{Align, Block, Document, Inline};

/// text/plain -> IR: абзацы по пустым строкам, строки с `>` — цитаты.
pub fn from_plain(text: &str) -> Document {
    let lines: Vec<&str> = text.lines().collect();
    let mut blocks = blocks(&lines, 0);
    let history = crate::history::detect(&mut blocks, &[]);
    Document { blocks, remote_images_blocked: 0, history }
}

fn blocks(lines: &[&str], depth: usize) -> Vec<Block> {
    let mut out = Vec::new();
    let mut para: Vec<Inline> = Vec::new();
    let mut i = 0;
    let flush = |para: &mut Vec<Inline>, out: &mut Vec<Block>| {
        if !para.is_empty() {
            out.push(Block::Paragraph { align: Align::Start, content: std::mem::take(para) });
        }
    };
    while i < lines.len() {
        let line = lines[i].trim_end();
        if depth < 32 && line.starts_with('>') {
            flush(&mut para, &mut out);
            let start = i;
            while i < lines.len() && lines[i].starts_with('>') {
                i += 1;
            }
            let inner: Vec<&str> = lines[start..i].iter().map(|l| l[1..].strip_prefix(' ').unwrap_or(&l[1..])).collect();
            out.push(Block::Quote(blocks(&inner, depth + 1)));
            continue;
        }
        if line.is_empty() {
            flush(&mut para, &mut out);
        } else {
            if !para.is_empty() {
                para.push(Inline::LineBreak);
            }
            para.push(Inline::text(line));
        }
        i += 1;
    }
    flush(&mut para, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_and_paragraphs() {
        let d = from_plain("Привет\nмир\n\n> старое\n>> совсем старое\nответ");
        assert_eq!(d.blocks.len(), 3);
        assert!(matches!(&d.blocks[1], Block::Quote(inner) if matches!(inner[1], Block::Quote(_))));
    }
}
