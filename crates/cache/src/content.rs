//! Содержимое писем формата 6 (ADR 0006 п. 2): письмо целиком по SHA-256 исходных байтов, крупные листовые
//! части — отдельно, раскодированными, одна запись на одинаковые байты. Скелет и части собираются ровно в исходные
//! байты: часть вынимается, только если её обратное кодирование совпало с исходным диапазоном.
//!
//! Счётчики ссылок: `content.refs` — писем с этим содержимым, `part.refs` — вхождений части в содержимое этих писем.
//! Ноль — запись удаляется в той же транзакции; [`Cache::check_refs`] пересчитывает их с нуля.

use std::collections::HashMap;
use std::io::{self, Read};
use std::ops::Range;

use redb::{ReadOnlyTable, ReadableDatabase, ReadableTable, TableDefinition, WriteTransaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::letter::{LETTER, LetterId, LetterRow, Letters};
use crate::{Cache, Error, Result, dec, enc};

/// SHA-256 байтов.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Hash(pub [u8; 32]);

impl Hash {
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }
}

/// Кодирование листовой части (`Content-Transfer-Encoding`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Base64,
    QuotedPrintable,
}

/// Листовая часть письма от воркера (его `MimeSplit`, S8-07; сопоставление — у ядра): где в исходных байтах её
/// закодированное тело и что в нём после раскодирования.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitPart {
    pub range: Range<usize>,
    pub encoding: Encoding,
    pub decoded: Vec<u8>,
}

/// Части меньше этого остаются в скелете.
pub const MIN_PART: usize = 64 * 1024;
/// Часть больше этого хранится кусками такого размера.
pub const CHUNK: usize = 1024 * 1024;

/// Хэш письма → [`ContentRow`].
const CONTENT: TableDefinition<[u8; 32], &[u8]> = TableDefinition::new("content");
/// Хэш раскодированной части → [`PartRow`].
const PART: TableDefinition<[u8; 32], &[u8]> = TableDefinition::new("part");
/// (хэш части, номер) → кусок до [`CHUNK`] байтов.
const CHUNKS: TableDefinition<([u8; 32], u32), &[u8]> = TableDefinition::new("chunk");

#[derive(Serialize, Deserialize)]
struct ContentRow {
    /// Письмо без вынутых частей.
    skeleton: Vec<u8>,
    /// По возрастанию `at`.
    parts: Vec<PartRef>,
    refs: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PartRef {
    /// Смещение в скелете, куда встаёт закодированная часть.
    at: u64,
    hash: Hash,
    coding: Coding,
}

#[derive(Serialize, Deserialize)]
struct PartRow {
    data: PartData,
    refs: u32,
}

#[derive(Serialize, Deserialize)]
enum PartData {
    Bytes(Vec<u8>),
    /// Столько кусков в [`CHUNKS`].
    Chunks(u32),
}

/// Как часть была закодирована в исходнике — всё, что нужно, чтобы получить те же байты.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Coding {
    /// `width` — длина строки (0 — без переносов); `crlf` — перевод строки; `tail` — перевод строки в конце.
    Base64 { width: u32, crlf: bool, tail: bool },
    /// Строки до 76 знаков с мягкими переносами `=`; жёсткие переводы строки — как в тексте части.
    QuotedPrintable { crlf: bool, tail: bool },
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const HEX: &[u8; 16] = b"0123456789ABCDEF";
/// Длина строки quoted-printable (RFC 2045 §6.7) вместе с `=` мягкого переноса.
const QP_LINE: usize = 76;

impl Coding {
    /// Кодирование, которое из `decoded` даёт ровно `src`; нет такого — `None`.
    fn matching(encoding: Encoding, decoded: &[u8], src: &[u8]) -> Option<Self> {
        let first_nl = src.iter().position(|&b| b == b'\n');
        let crlf = first_nl.is_some_and(|i| i > 0 && src[i - 1] == b'\r');
        let coding = match encoding {
            Encoding::Base64 => {
                let width = first_nl.map_or(0, |i| i - usize::from(crlf));
                Coding::Base64 { width: u32::try_from(width).ok()?, crlf, tail: false }
            }
            Encoding::QuotedPrintable => Coding::QuotedPrintable { crlf, tail: false },
        };
        let mut out = Vec::with_capacity(src.len());
        let mut enc = Encoder::new(coding);
        enc.feed(decoded, &mut out);
        enc.finish(&mut out);
        if out == src {
            return Some(coding);
        }
        let nl: &[u8] = if crlf { b"\r\n" } else { b"\n" };
        let tailed = src.len() == out.len() + nl.len() && src.starts_with(&out) && src.ends_with(nl);
        tailed.then_some(match coding {
            Coding::Base64 { width, crlf, .. } => Coding::Base64 { width, crlf, tail: true },
            Coding::QuotedPrintable { crlf, .. } => Coding::QuotedPrintable { crlf, tail: true },
        })
    }

    fn nl(self) -> &'static [u8] {
        match self {
            Coding::Base64 { crlf: true, .. } | Coding::QuotedPrintable { crlf: true, .. } => b"\r\n",
            _ => b"\n",
        }
    }
}

/// Кодировщик потоком: часть подаётся кусками ([`CHUNK`]), выход тот же, что при подаче целиком.
struct Encoder {
    coding: Coding,
    /// Знаков в текущей строке.
    col: usize,
    /// base64: байты неполной тройки.
    carry: [u8; 3],
    n: usize,
    /// quoted-printable: пробелы и `\r`, судьба которых зависит от следующего байта.
    pending: Vec<u8>,
}

impl Encoder {
    fn new(coding: Coding) -> Self {
        Self { coding, col: 0, carry: [0; 3], n: 0, pending: Vec::new() }
    }

    fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) {
        match self.coding {
            Coding::Base64 { .. } => {
                for &b in input {
                    self.carry[self.n] = b;
                    self.n += 1;
                    if self.n == 3 {
                        self.quad(out);
                    }
                }
            }
            Coding::QuotedPrintable { .. } => {
                for &b in input {
                    self.qp_byte(b, out);
                }
            }
        }
    }

    fn finish(mut self, out: &mut Vec<u8>) {
        let tail = match self.coding {
            Coding::Base64 { tail, .. } => {
                if self.n > 0 {
                    self.quad(out);
                }
                tail
            }
            Coding::QuotedPrintable { tail, .. } => {
                // Конец данных — конец строки: пробелы перед ним кодируются; `\r` без `\n` — тоже.
                let cr = self.pending.last() == Some(&b'\r');
                let pending = std::mem::take(&mut self.pending);
                for (i, &b) in pending.iter().enumerate() {
                    if cr && i + 1 < pending.len() {
                        self.qp_token(&[b], out);
                    } else {
                        self.qp_escaped(b, out);
                    }
                }
                tail
            }
        };
        if tail {
            out.extend_from_slice(self.coding.nl());
        }
    }

    /// Тройка (или неполный остаток с `=`) → четыре знака base64, с переносом строки по ширине.
    fn quad(&mut self, out: &mut Vec<u8>) {
        let [a, b, c] = self.carry;
        let n = self.n;
        let (b, c) = (if n > 1 { b } else { 0 }, if n > 2 { c } else { 0 });
        let chars = [
            B64[usize::from(a >> 2)],
            B64[usize::from((a & 0x03) << 4 | b >> 4)],
            if n > 1 { B64[usize::from((b & 0x0f) << 2 | c >> 6)] } else { b'=' },
            if n > 2 { B64[usize::from(c & 0x3f)] } else { b'=' },
        ];
        let Coding::Base64 { width, .. } = self.coding else { return };
        let width = width as usize;
        for ch in chars {
            if width > 0 && self.col == width {
                out.extend_from_slice(self.coding.nl());
                self.col = 0;
            }
            out.push(ch);
            self.col += 1;
        }
        self.n = 0;
    }

    fn qp_byte(&mut self, b: u8, out: &mut Vec<u8>) {
        match b {
            b'\n' => {
                let cr = self.pending.last() == Some(&b'\r');
                if cr {
                    self.pending.pop();
                }
                // Пробелы в конце строки кодируются: иначе их срежет транспорт.
                for b in std::mem::take(&mut self.pending) {
                    self.qp_escaped(b, out);
                }
                out.extend_from_slice(if cr { b"\r\n" } else { b"\n" });
                self.col = 0;
            }
            b' ' | b'\t' | b'\r' => {
                if self.pending.last() == Some(&b'\r') {
                    self.qp_flush(out);
                }
                self.pending.push(b);
            }
            _ => {
                self.qp_flush(out);
                if (33..=126).contains(&b) && b != b'=' {
                    self.qp_token(&[b], out);
                } else {
                    self.qp_escaped(b, out);
                }
            }
        }
    }

    /// Отложенное — не в конце строки: пробелы как есть, `\r` — `=0D`.
    fn qp_flush(&mut self, out: &mut Vec<u8>) {
        for b in std::mem::take(&mut self.pending) {
            if b == b'\r' {
                self.qp_escaped(b, out);
            } else {
                self.qp_token(&[b], out);
            }
        }
    }

    fn qp_escaped(&mut self, b: u8, out: &mut Vec<u8>) {
        self.qp_token(&[b'=', HEX[usize::from(b >> 4)], HEX[usize::from(b & 0x0f)]], out);
    }

    /// Знак или `=XX`; не помещается в строку вместе с `=` мягкого переноса — перенос.
    fn qp_token(&mut self, token: &[u8], out: &mut Vec<u8>) {
        if self.col + token.len() > QP_LINE - 1 {
            out.push(b'=');
            out.extend_from_slice(self.coding.nl());
            self.col = 0;
        }
        out.extend_from_slice(token);
        self.col += token.len();
    }
}

/// Таблицы содержимого — при открытии кэша.
pub(crate) fn init(tx: &WriteTransaction) -> Result<()> {
    tx.open_table(CONTENT)?;
    tx.open_table(PART)?;
    tx.open_table(CHUNKS)?;
    Ok(())
}

/// Разрезать письмо: скелет и вынутые части (ссылка и раскодированные байты). Части с неверным или
/// пересекающимся диапазоном, мелкие и не совпавшие при обратном кодировании остаются в скелете.
fn cut<'a>(raw: &[u8], split: &'a [SplitPart]) -> (Vec<u8>, Vec<(PartRef, &'a [u8])>) {
    let mut order: Vec<&SplitPart> = split.iter().collect();
    order.sort_by_key(|p| p.range.start);
    let mut skeleton = Vec::with_capacity(raw.len());
    let mut parts = Vec::new();
    let mut pos = 0;
    for p in order {
        let Range { start, end } = p.range;
        if start < pos || end < start || end > raw.len() || p.decoded.len() < MIN_PART {
            continue;
        }
        let Some(coding) = Coding::matching(p.encoding, &p.decoded, &raw[start..end]) else { continue };
        skeleton.extend_from_slice(&raw[pos..start]);
        parts.push((PartRef { at: skeleton.len() as u64, hash: Hash::of(&p.decoded), coding }, p.decoded.as_slice()));
        pos = end;
    }
    skeleton.extend_from_slice(&raw[pos..]);
    (skeleton, parts)
}

/// Содержимое `hash` (байты `raw`) — ещё одному письму: есть — `refs + 1` у него и его частей, нет — записать.
fn acquire(tx: &WriteTransaction, hash: Hash, raw: &[u8], split: &[SplitPart]) -> Result<()> {
    let mut content = tx.open_table(CONTENT)?;
    let mut part = tx.open_table(PART)?;
    let mut chunks = tx.open_table(CHUNKS)?;
    let old: Option<ContentRow> = content.get(hash.0)?.map(|v| dec(v.value())).transpose()?;
    let row = match old {
        Some(mut row) => {
            for p in &row.parts {
                let Some(mut pr) = part.get(p.hash.0)?.map(|v| dec::<PartRow>(v.value())).transpose()? else {
                    return Err(Error(format!("content {}: no part {}", hex(&hash), hex(&p.hash))));
                };
                pr.refs = pr.refs.saturating_add(1);
                part.insert(p.hash.0, enc(&pr)?.as_slice())?;
            }
            row.refs = row.refs.saturating_add(1);
            row
        }
        None => {
            let (skeleton, cut_parts) = cut(raw, split);
            let mut refs = Vec::with_capacity(cut_parts.len());
            for (p, bytes) in cut_parts {
                let known: Option<PartRow> = part.get(p.hash.0)?.map(|v| dec(v.value())).transpose()?;
                let pr = match known {
                    Some(mut pr) => {
                        pr.refs = pr.refs.saturating_add(1);
                        pr
                    }
                    None if bytes.len() > CHUNK => {
                        let mut n = 0u32;
                        for c in bytes.chunks(CHUNK) {
                            chunks.insert((p.hash.0, n), c)?;
                            n += 1;
                        }
                        PartRow { data: PartData::Chunks(n), refs: 1 }
                    }
                    None => PartRow { data: PartData::Bytes(bytes.to_vec()), refs: 1 },
                };
                part.insert(p.hash.0, enc(&pr)?.as_slice())?;
                refs.push(p);
            }
            ContentRow { skeleton, parts: refs, refs: 1 }
        }
    };
    content.insert(hash.0, enc(&row)?.as_slice())?;
    Ok(())
}

/// Одним письмом с содержимым `hash` меньше: `refs − 1` у него и его частей, ноль — удалить.
pub(crate) fn release(tx: &WriteTransaction, hash: Hash) -> Result<()> {
    let mut content = tx.open_table(CONTENT)?;
    let mut part = tx.open_table(PART)?;
    let mut chunks = tx.open_table(CHUNKS)?;
    let Some(mut row) = content.get(hash.0)?.map(|v| dec::<ContentRow>(v.value())).transpose()? else { return Ok(()) };
    for p in &row.parts {
        let Some(mut pr) = part.get(p.hash.0)?.map(|v| dec::<PartRow>(v.value())).transpose()? else { continue };
        pr.refs = pr.refs.saturating_sub(1);
        if pr.refs > 0 {
            part.insert(p.hash.0, enc(&pr)?.as_slice())?;
            continue;
        }
        part.remove(p.hash.0)?;
        if let PartData::Chunks(n) = pr.data {
            for i in 0..n {
                chunks.remove((p.hash.0, i))?;
            }
        }
    }
    row.refs = row.refs.saturating_sub(1);
    if row.refs == 0 {
        content.remove(hash.0)?;
    } else {
        content.insert(hash.0, enc(&row)?.as_slice())?;
    }
    Ok(())
}

fn hex(h: &Hash) -> String {
    h.0[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Чтение письма потоком: скелет кусками, части — кусками из базы, закодированными заново на лету.
pub struct ContentReader {
    row: ContentRow,
    part: ReadOnlyTable<[u8; 32], &'static [u8]>,
    chunks: ReadOnlyTable<([u8; 32], u32), &'static [u8]>,
    /// Прочитано скелета.
    pos: usize,
    /// Следующая часть.
    next: usize,
    stream: Option<PartStream>,
    buf: Vec<u8>,
    off: usize,
}

struct PartStream {
    encoder: Encoder,
    hash: Hash,
    /// Следующий кусок и сколько их (у части одним значением — один, `bytes`).
    chunk: u32,
    chunks: u32,
    bytes: Option<Vec<u8>>,
}

/// Скелет отдаётся кусками не больше этого.
const SKELETON_PIECE: usize = 64 * 1024;

impl ContentReader {
    /// Следующая порция в `buf`; пусто — конец.
    fn fill(&mut self) -> Result<()> {
        self.buf.clear();
        self.off = 0;
        while self.buf.is_empty() {
            if let Some(s) = &mut self.stream {
                if s.chunk < s.chunks {
                    match s.bytes.take() {
                        Some(bytes) => s.encoder.feed(&bytes, &mut self.buf),
                        None => {
                            let Some(c) = self.chunks.get((s.hash.0, s.chunk))? else {
                                return Err(Error(format!("part {}: no chunk {}", hex(&s.hash), s.chunk)));
                            };
                            s.encoder.feed(c.value(), &mut self.buf);
                        }
                    }
                    s.chunk += 1;
                } else if let Some(s) = self.stream.take() {
                    s.encoder.finish(&mut self.buf);
                    self.next += 1;
                }
                continue;
            }
            let end = self.row.parts.get(self.next).map_or(self.row.skeleton.len(), |p| p.at as usize).min(self.row.skeleton.len());
            if self.pos < end {
                let to = end.min(self.pos + SKELETON_PIECE);
                self.buf.extend_from_slice(&self.row.skeleton[self.pos..to]);
                self.pos = to;
                continue;
            }
            let Some(p) = self.row.parts.get(self.next) else { return Ok(()) };
            let Some(pr) = self.part.get(p.hash.0)?.map(|v| dec::<PartRow>(v.value())).transpose()? else {
                return Err(Error(format!("no part {}", hex(&p.hash))));
            };
            let (chunks, bytes) = match pr.data {
                PartData::Bytes(b) => (1, Some(b)),
                PartData::Chunks(n) => (n, None),
            };
            self.stream = Some(PartStream { encoder: Encoder::new(p.coding), hash: p.hash, chunk: 0, chunks, bytes });
        }
        Ok(())
    }
}

impl Read for ContentReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.off == self.buf.len() {
            self.fill().map_err(|e| io::Error::other(e.to_string()))?;
        }
        let n = out.len().min(self.buf.len() - self.off);
        out[..n].copy_from_slice(&self.buf[self.off..self.off + n]);
        self.off += n;
        Ok(n)
    }
}

impl Cache {
    /// Письмо целиком `raw` — содержимым письма `letter`: хэш, скелет и части по `split` (диапазоны листовых частей
    /// и их раскодированные байты от воркера; пусто — шифрованное или мелкое, скелет — всё письмо). Одинаковые байты
    /// хранятся один раз; прежнее содержимое письма — `refs − 1`. Одной транзакцией: содержимого без письма не бывает.
    pub fn put_content(&self, letter: LetterId, raw: &[u8], split: &[SplitPart]) -> Result<Hash> {
        let hash = Hash::of(raw);
        let tx = self.db.begin_write()?;
        {
            let mut l = Letters::open(&tx)?;
            let Some(old) = l.get(letter)? else { return Err(Error(format!("no letter {}", letter.0))) };
            if old.content != Some(hash) {
                acquire(&tx, hash, raw, split)?;
                if let Some(prev) = old.content {
                    release(&tx, prev)?;
                }
                l.put(letter, Some(&old), &LetterRow { content: Some(hash), ..old.clone() })?;
            }
            l.finish()?;
        }
        tx.commit()?;
        Ok(hash)
    }

    /// Письмо целиком, ровно исходные байты; `None` — такого содержимого нет.
    pub fn content(&self, hash: Hash) -> Result<Option<Vec<u8>>> {
        let Some(mut r) = self.content_reader(hash)? else { return Ok(None) };
        let mut out = Vec::new();
        r.read_to_end(&mut out)?;
        Ok(Some(out))
    }

    /// Письмо целиком потоком (экспорт, выгрузка на сервер): в памяти — скелет и кусок части.
    pub fn content_reader(&self, hash: Hash) -> Result<Option<ContentReader>> {
        let tx = self.db.begin_read()?;
        let Some(row) = tx.open_table(CONTENT)?.get(hash.0)?.map(|v| dec::<ContentRow>(v.value())).transpose()? else {
            return Ok(None);
        };
        Ok(Some(ContentReader {
            row,
            part: tx.open_table(PART)?,
            chunks: tx.open_table(CHUNKS)?,
            pos: 0,
            next: 0,
            stream: None,
            buf: Vec::new(),
            off: 0,
        }))
    }

    /// Пересчёт ссылок на содержимое и части с нуля по письмам и сверка с `refs` (и кусками частей) — для тестов и
    /// диагностики. Ответ — расхождения по-английски (для журнала); пусто — сходится.
    pub fn check_refs(&self) -> Result<Vec<String>> {
        let tx = self.db.begin_read()?;
        let mut want_content: HashMap<Hash, u32> = HashMap::new();
        for r in tx.open_table(LETTER)?.iter()? {
            if let Some(h) = dec::<LetterRow>(r?.1.value())?.content {
                *want_content.entry(h).or_default() += 1;
            }
        }
        let content = tx.open_table(CONTENT)?;
        let mut problems = Vec::new();
        let mut want_part: HashMap<Hash, u32> = HashMap::new();
        for r in content.iter()? {
            let (k, v) = r?;
            let (h, row) = (Hash(k.value()), dec::<ContentRow>(v.value())?);
            let want = want_content.remove(&h).unwrap_or(0);
            if row.refs != want {
                problems.push(format!("content {}: refs {}, letters {want}", hex(&h), row.refs));
            }
            for p in &row.parts {
                *want_part.entry(p.hash).or_default() += want;
            }
        }
        for h in want_content.keys() {
            problems.push(format!("content {}: referenced by letters, missing", hex(h)));
        }
        let chunks = tx.open_table(CHUNKS)?;
        let mut chunked: HashMap<Hash, u32> = HashMap::new();
        for r in tx.open_table(PART)?.iter()? {
            let (k, v) = r?;
            let (h, row) = (Hash(k.value()), dec::<PartRow>(v.value())?);
            let want = want_part.remove(&h).unwrap_or(0);
            if row.refs != want {
                problems.push(format!("part {}: refs {}, references {want}", hex(&h), row.refs));
            }
            if let PartData::Chunks(n) = row.data {
                chunked.insert(h, n);
                for i in 0..n {
                    if chunks.get((h.0, i))?.is_none() {
                        problems.push(format!("part {}: no chunk {i}", hex(&h)));
                    }
                }
            }
        }
        for (h, n) in want_part {
            if n > 0 {
                problems.push(format!("part {}: referenced, missing", hex(&h)));
            }
        }
        for r in chunks.iter()? {
            let (h, i) = r?.0.value();
            if chunked.get(&Hash(h)).is_none_or(|n| i >= *n) {
                problems.push(format!("chunk {} {i}: no part", hex(&Hash(h))));
            }
        }
        Ok(problems)
    }
}

#[cfg(test)]
mod tests {
    use redb::ReadableTableMetadata;

    use super::*;
    use crate::{FolderId, FolderRow, Owner};

    /// base64 независимо от [`Encoder`]: строки по `width` (0 — одной строкой), перевод строки `nl` после каждой.
    fn b64(data: &[u8], width: usize, nl: &str) -> String {
        let mut s = String::new();
        for c in data.chunks(3) {
            let n = u32::from(c[0]) << 16 | u32::from(*c.get(1).unwrap_or(&0)) << 8 | u32::from(*c.get(2).unwrap_or(&0));
            for i in 0..4 {
                s.push(if i <= c.len() { B64[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
            }
        }
        if width == 0 {
            return s + nl;
        }
        s.as_bytes().chunks(width).map(|l| format!("{}{nl}", std::str::from_utf8(l).unwrap())).collect()
    }

    fn noise(len: usize, seed: u64) -> Vec<u8> {
        let mut x = seed | 1;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    /// Письмо с одним вложением base64 (`body` — уже закодированное) и часть для него.
    fn letter_with(subject: &str, data: &[u8], body: &str, nl: &str) -> (Vec<u8>, SplitPart) {
        let head = [
            format!("Subject: {subject}"),
            "MIME-Version: 1.0".into(),
            "Content-Type: multipart/mixed; boundary=\"b1\"".into(),
            String::new(),
            "--b1".into(),
            "Content-Type: text/plain".into(),
            String::new(),
            "Привет".into(),
            "--b1".into(),
            "Content-Type: application/octet-stream".into(),
            "Content-Transfer-Encoding: base64".into(),
            String::new(),
            String::new(),
        ]
        .join(nl);
        let raw = format!("{head}{body}--b1--{nl}").into_bytes();
        let part = SplitPart { range: head.len()..head.len() + body.len(), encoding: Encoding::Base64, decoded: data.to_vec() };
        (raw, part)
    }

    fn setup() -> (Cache, FolderId, FolderId) {
        let c = Cache::open_in_memory().unwrap();
        let folder = |a: &str| {
            c.upsert_folder(&FolderRow { owner: Owner::Account(a.into()), parent: None, name: "in".into(), role: None, remote: Some("in".into()) })
                .unwrap()
        };
        let (a, b) = (folder("a"), folder("b"));
        (c, a, b)
    }

    fn new_letter(c: &Cache, account: &str, id: &str, f: FolderId) -> LetterId {
        c.upsert_remote(account, id, &LetterRow { folders: vec![f], ..LetterRow::default() }).unwrap()
    }

    fn rows(c: &Cache) -> (usize, usize, usize) {
        let tx = c.db.begin_read().unwrap();
        let n = |t: TableDefinition<[u8; 32], &[u8]>| tx.open_table(t).unwrap().len().unwrap() as usize;
        (n(CONTENT), n(PART), tx.open_table(CHUNKS).unwrap().len().unwrap() as usize)
    }

    fn part_refs(c: &Cache, data: &[u8]) -> u32 {
        let tx = c.db.begin_read().unwrap();
        tx.open_table(PART).unwrap().get(Hash::of(data).0).unwrap().map_or(0, |v| dec::<PartRow>(v.value()).unwrap().refs)
    }

    fn assert_refs(c: &Cache) {
        assert_eq!(c.check_refs().unwrap(), Vec::<String>::new());
    }

    /// Одно вложение в двух учётках и в другом письме — одна запись части; сборка — исходные байты; удаление
    /// последнего письма убирает содержимое и части.
    #[test]
    fn one_attachment_is_stored_once() {
        let (c, a, b) = setup();
        let data = noise(200 * 1024, 1);
        let (raw, part) = letter_with("x", &data, &b64(&data, 76, "\r\n"), "\r\n");
        let (x, y) = (new_letter(&c, "a", "x", a), new_letter(&c, "b", "x", b));
        let h = c.put_content(x, &raw, std::slice::from_ref(&part)).unwrap();
        assert_eq!(c.put_content(y, &raw, std::slice::from_ref(&part)).unwrap(), h);
        assert_eq!(h, Hash::of(&raw));
        assert_eq!(rows(&c), (1, 1, 0));
        assert_eq!(part_refs(&c, &data), 2);
        // Пересланное: другие заголовки, то же вложение.
        let (raw2, part2) = letter_with("Fwd: x", &data, &b64(&data, 76, "\r\n"), "\r\n");
        let z = new_letter(&c, "a", "z", a);
        let h2 = c.put_content(z, &raw2, &[part2]).unwrap();
        assert_eq!((rows(&c), part_refs(&c, &data)), ((2, 1, 0), 3));
        assert_eq!(c.content(h).unwrap().unwrap(), raw);
        assert_eq!(c.content(h2).unwrap().unwrap(), raw2);
        // Скелет без вложения.
        let tx = c.db.begin_read().unwrap();
        let row: ContentRow = dec(tx.open_table(CONTENT).unwrap().get(h.0).unwrap().unwrap().value()).unwrap();
        assert!(row.skeleton.len() < 1024 && row.refs == 2);
        drop(tx);
        assert_refs(&c);

        c.delete_letters(&[x, z]).unwrap();
        assert_eq!((rows(&c), part_refs(&c, &data)), ((1, 1, 0), 1));
        assert_refs(&c);
        c.delete_remote("b", &["x".into()]).unwrap();
        assert_eq!(rows(&c), (0, 0, 0));
        assert_eq!(c.content(h).unwrap(), None);
        assert_refs(&c);
    }

    /// Точность байтов: base64 без переносов, с LF, со своей шириной, с хвостовыми пробелами (не совпало — остаётся в
    /// скелете), quoted-printable; часть больше 1 МиБ — кусками, чтение потоком малыми порциями.
    #[test]
    fn assembly_gives_the_original_bytes() {
        let (c, a, _) = setup();
        let data = noise(100 * 1024, 2);
        let trailing: String = b64(&data, 76, "\r\n").replace("\r\n", "  \r\n");
        let cases = [
            ("no wrap", b64(&data, 0, "\r\n"), "\r\n", true),
            ("lf", b64(&data, 76, "\n"), "\n", true),
            ("width 64", b64(&data, 64, "\r\n"), "\r\n", true),
            ("trailing spaces", trailing, "\r\n", false),
        ];
        for (i, (name, body, nl, cut)) in cases.into_iter().enumerate() {
            let (raw, part) = letter_with(name, &data, &body, nl);
            let l = new_letter(&c, "a", &format!("m{i}"), a);
            let h = c.put_content(l, &raw, &[part]).unwrap();
            assert_eq!(c.content(h).unwrap().unwrap(), raw, "{name}");
            assert_eq!(part_refs(&c, &data) > 0, cut, "{name}");
            c.delete_letters(&[l]).unwrap();
        }

        // quoted-printable: текст с пробелами в конце строк, `=`, не-ASCII, длинными строками.
        let mut text = Vec::new();
        for i in 0..3000 {
            text.extend_from_slice(format!("Строка {i} = цена;\tтаб и пробел в конце ").as_bytes());
            if i % 7 == 0 {
                text.extend_from_slice("очень длинная строка ".repeat(9).as_bytes());
            }
            text.extend_from_slice(b"\r\n");
        }
        let mut qp = Vec::new();
        let mut e = Encoder::new(Coding::QuotedPrintable { crlf: true, tail: false });
        e.feed(&text, &mut qp);
        e.finish(&mut qp);
        assert!(qp.split(|&b| b == b'\n').all(|l| l.len() <= QP_LINE + 1));
        assert!(qp.windows(4).any(|w| w == b"=20\r"), "пробел в конце строки закодирован");
        let mut raw = b"Subject: qp\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n".to_vec();
        let start = raw.len();
        raw.extend_from_slice(&qp);
        raw.extend_from_slice(b"\r\n");
        let part = SplitPart { range: start..raw.len(), encoding: Encoding::QuotedPrintable, decoded: text.clone() };
        let l = new_letter(&c, "a", "qp", a);
        let h = c.put_content(l, &raw, &[part]).unwrap();
        assert_eq!(c.content(h).unwrap().unwrap(), raw);
        assert_eq!(part_refs(&c, &text), 1);

        // Больше 1 МиБ — кусками; поток малыми порциями.
        let big = noise(CHUNK * 2 + 12_345, 3);
        let (raw, part) = letter_with("big", &big, &b64(&big, 76, "\r\n"), "\r\n");
        let l = new_letter(&c, "a", "big", a);
        let h = c.put_content(l, &raw, &[part]).unwrap();
        assert_eq!(rows(&c).2, 3);
        let mut r = c.content_reader(h).unwrap().unwrap();
        let mut out = Vec::new();
        let mut buf = [0u8; 1000];
        loop {
            let n = r.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        assert!(out == raw);
        // Неверные диапазоны и мелкие части — в скелете, байты те же.
        let small = noise(1000, 4);
        let (raw, mut part) = letter_with("small", &small, &b64(&small, 76, "\r\n"), "\r\n");
        let bad = SplitPart { range: part.range.start..raw.len() + 5, ..part.clone() };
        part.decoded = small;
        let l = new_letter(&c, "a", "small", a);
        let h = c.put_content(l, &raw, &[part, bad]).unwrap();
        assert_eq!(c.content(h).unwrap().unwrap(), raw);
        assert_refs(&c);
    }

    /// `multipart/signed`: подписанная часть с вложением и подпись — после сборки те же байты (подпись сходится так
    /// же, как до).
    #[test]
    fn signed_letter_is_byte_exact() {
        let (c, a, _) = setup();
        let data = noise(300 * 1024, 5);
        let sig = noise(2000, 6);
        let body = b64(&data, 76, "\r\n");
        let signed = format!(
            "Content-Type: multipart/mixed; boundary=\"in\"\r\n\r\n--in\r\nContent-Type: text/plain\r\n\r\nhi\r\n--in\r\n\
             Content-Type: application/pdf\r\nContent-Transfer-Encoding: base64\r\n\r\n{body}--in--\r\n"
        );
        let head = "Subject: signed\r\nContent-Type: multipart/signed; protocol=\"application/pkcs7-signature\"; \
                    micalg=sha-256; boundary=\"out\"\r\n\r\n--out\r\n";
        let tail = format!(
            "\r\n--out\r\nContent-Type: application/pkcs7-signature\r\nContent-Transfer-Encoding: base64\r\n\r\n{}--out--\r\n",
            b64(&sig, 76, "\r\n")
        );
        let raw = format!("{head}{signed}{tail}").into_bytes();
        let start = head.len() + signed.find(&body).unwrap();
        let part = SplitPart { range: start..start + body.len(), encoding: Encoding::Base64, decoded: data.clone() };
        let l = new_letter(&c, "a", "s", a);
        let h = c.put_content(l, &raw, &[part]).unwrap();
        assert_eq!(part_refs(&c, &data), 1);
        let back = c.content(h).unwrap().unwrap();
        assert!(back == raw);
        assert_eq!(Hash::of(&back), h);
    }

    /// Поток кусками любой длины даёт то же, что кодирование целиком.
    #[test]
    fn encoder_is_chunk_agnostic() {
        let text = "a b \r\n\t=x\r\r\n é  \n\n end  ".repeat(50).into_bytes();
        let data = noise(5000, 7);
        for (coding, input) in [
            (Coding::QuotedPrintable { crlf: false, tail: true }, &text),
            (Coding::Base64 { width: 72, crlf: true, tail: false }, &data),
            (Coding::Base64 { width: 0, crlf: false, tail: true }, &data),
        ] {
            let mut whole = Vec::new();
            let mut e = Encoder::new(coding);
            e.feed(input, &mut whole);
            e.finish(&mut whole);
            for size in [1, 2, 3, 7, 1000] {
                let mut out = Vec::new();
                let mut e = Encoder::new(coding);
                for c in input.chunks(size) {
                    e.feed(c, &mut out);
                }
                e.finish(&mut out);
                assert!(out == whole, "{coding:?} кусками по {size}");
            }
        }
        assert_eq!(b64(b"hello", 0, ""), {
            let mut out = Vec::new();
            let mut e = Encoder::new(Coding::Base64 { width: 0, crlf: false, tail: false });
            e.feed(b"hello", &mut out);
            e.finish(&mut out);
            String::from_utf8(out).unwrap()
        });
    }

    /// 1 000 случайных записей содержимого и удалений писем: счётчики сходятся с пересчётом.
    #[test]
    fn refs_survive_random_writes() {
        let (c, a, b) = setup();
        let attachments: Vec<Vec<u8>> = (0..3).map(|i| noise(MIN_PART + i * 999, 10 + i as u64)).collect();
        let letters: Vec<(Vec<u8>, SplitPart)> = (0..6)
            .map(|i| {
                let data = &attachments[i % 3];
                letter_with(&format!("l{}", i / 2), data, &b64(data, 76, "\r\n"), "\r\n")
            })
            .collect();
        let mut rng = 0xD1B5_4A32_D192_ED03u64;
        let mut next = move |n: u64| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng % n
        };
        for step in 0..1000 {
            let (account, folder) = if next(2) == 0 { ("a", a) } else { ("b", b) };
            let id = format!("m{}", next(12));
            match next(4) {
                0 => {
                    let letter = c.letter_of(account, &id).unwrap();
                    c.delete_letters(&letter.into_iter().collect::<Vec<_>>()).unwrap();
                }
                1 => {
                    c.delete_remote(account, &[id]).unwrap();
                }
                _ => {
                    let letter = new_letter(&c, account, &id, folder);
                    let (raw, part) = &letters[next(6) as usize];
                    let split = if next(5) == 0 { Vec::new() } else { vec![part.clone()] };
                    c.put_content(letter, raw, &split).unwrap();
                }
            }
            if step % 100 == 0 {
                assert_refs(&c);
            }
        }
        assert_refs(&c);
        let all: Vec<LetterId> = {
            let tx = c.db.begin_read().unwrap();
            tx.open_table(LETTER).unwrap().iter().unwrap().map(|r| LetterId(r.unwrap().0.value())).collect()
        };
        c.delete_letters(&all).unwrap();
        assert_eq!(rows(&c), (0, 0, 0));
    }
}
