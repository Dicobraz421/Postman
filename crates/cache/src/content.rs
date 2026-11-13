//! Содержимое писем (ADR 0006 п. 2 и дополнение 2026-11-14): письмо целиком по SHA-256 исходных байтов, листовые
//! части — отдельно, раскодированными, одна запись на одинаковые байты. Скелет и части собираются ровно в исходные
//! байты: часть вынимается, только если её обратное кодирование совпало с исходным диапазоном.
//!
//! Что вынимается (формат 7): любая часть от [`MIN_PART`] и картинка (`image/*`) от [`MIN_IMAGE`] — логотипы подписей
//! и значки рассылок повторяются в тысячах писем. Скелет, разобранное тело (`body`) и сжимаемые части хранятся
//! сжатыми zstd ([`LEVEL`]); уже сжатое (картинки, архивы, документы Office, PDF, видео) не жмётся: решают тип MIME и
//! пробное сжатие начала части.
//!
//! Счётчики ссылок: `content_refs` — писем с этим содержимым, `part_refs` — вхождений части в разные записи
//! содержимого; рядом со счётчиком — размеры записи для сводки ([`Cache::store_stats`] не читает сами записи). Ноль
//! — запись удаляется в той же транзакции; [`Cache::check_refs`] пересчитывает их с нуля.

use std::collections::HashMap;
use std::io::{self, Read};
use std::ops::Range;

use redb::{ReadOnlyTable, ReadTransaction, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition, WriteTransaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::letter::{LETTER, LetterId, LetterRow, Letters};
use crate::{BODY, Cache, Error, Result, dec, enc};

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
/// закодированное тело, что в нём после раскодирования и её тип MIME (`type/subtype` строчными, пусто — не указан).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitPart {
    pub range: Range<usize>,
    pub encoding: Encoding,
    pub decoded: Vec<u8>,
    pub mime: String,
}

/// Любая часть от этого (раскодированной) — отдельно; меньше — в скелете. Замер 2026-11-14 (ADR 0006): от 1 до 64 КиБ
/// размер почти один и тот же — уникальный текст в скелете жмётся вместе с соседним; 4 КиБ — ради повторяющихся мелких
/// не-картинок (приглашения, визитки, приложенные политики).
pub const MIN_PART: usize = 4 * 1024;
/// Картинка (`image/*`) — отдельно от этого: логотипы и значки повторяются из письма в письмо.
pub const MIN_IMAGE: usize = 1024;
/// Часть больше этого хранится кусками такого размера.
pub const CHUNK: usize = 1024 * 1024;
/// Уровень zstd: скорость записи при синхронизации важнее последних процентов (замер 2026-11-14).
pub const LEVEL: i32 = 3;

/// Как резать и жать — для замера ADR 0006 (дополнение 2026-11-14) и тестов; приложение берёт [`Tuning::default`].
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tuning {
    pub min_part: usize,
    pub min_image: usize,
    /// Уровень zstd; `None` — без сжатия.
    pub level: Option<i32>,
}

impl Default for Tuning {
    fn default() -> Self {
        Self { min_part: MIN_PART, min_image: MIN_IMAGE, level: Some(LEVEL) }
    }
}

impl Tuning {
    /// Как формат 6: части от 64 КиБ, без сжатия.
    pub const FORMAT_6: Tuning = Tuning { min_part: 64 * 1024, min_image: 64 * 1024, level: None };
}

/// Набор таблиц содержимого: рабочий и временный на время переноса формата 6 в 7 (`migrate`).
#[derive(Clone, Copy)]
pub(crate) struct Tables {
    /// Хэш письма → [`ContentRow`].
    pub(crate) content: TableDefinition<'static, [u8; 32], &'static [u8]>,
    /// Хэш письма → [`ContentMeta`]: писем с этим содержимым и размеры.
    pub(crate) content_refs: TableDefinition<'static, [u8; 32], ContentMeta>,
    /// Хэш раскодированной части → [`PartRow`].
    pub(crate) part: TableDefinition<'static, [u8; 32], &'static [u8]>,
    /// Хэш части → [`PartMeta`]: вхождений в записи содержимого и размеры.
    pub(crate) part_refs: TableDefinition<'static, [u8; 32], PartMeta>,
    /// (хэш части, номер) → кусок до [`CHUNK`] байтов ([`Packed`]).
    pub(crate) chunks: TableDefinition<'static, ([u8; 32], u32), &'static [u8]>,
    /// Разобранные тела ([`crate::BodyRow`] в [`Packed`]).
    pub(crate) body: TableDefinition<'static, &'static str, &'static [u8]>,
}

/// Писем с содержимым; письмо без дедупликации (скелет и раскодированные части); скелет; скелет в записи.
pub(crate) type ContentMeta = (u32, u64, u64, u64);
/// Вхождений части; её длина; в записях (со всеми кусками).
pub(crate) type PartMeta = (u32, u64, u64);

pub(crate) const TABLES: Tables = Tables {
    content: TableDefinition::new("content"),
    content_refs: TableDefinition::new("content_refs"),
    part: TableDefinition::new("part"),
    part_refs: TableDefinition::new("part_refs"),
    chunks: TableDefinition::new("chunk"),
    body: BODY,
};

#[derive(Serialize, Deserialize)]
pub(crate) struct ContentRow {
    /// Письмо без вынутых частей.
    pub(crate) skeleton: Packed,
    /// По возрастанию `at`.
    pub(crate) parts: Vec<PartRef>,
    /// Длина письма целиком.
    pub(crate) size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PartRef {
    /// Смещение в скелете, куда встаёт закодированная часть.
    pub(crate) at: u64,
    pub(crate) hash: Hash,
    pub(crate) coding: Coding,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct PartRow {
    pub(crate) data: PartData,
    /// Длина раскодированной части.
    pub(crate) len: u64,
}

#[derive(Serialize, Deserialize)]
pub(crate) enum PartData {
    Bytes(Packed),
    /// Столько кусков в `chunk`, каждый — [`Packed`].
    Chunks(u32),
}

/// Байты как есть или сжатые zstd (с длиной до сжатия).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Packed {
    Raw(#[serde(with = "bytes")] Vec<u8>),
    Zstd(u64, #[serde(with = "bytes")] Vec<u8>),
}

/// Сжатое больше этого не разжимается (повреждённая запись не должна просить гигабайты памяти).
const MAX_UNPACKED: u64 = 1 << 32;
/// Меньше этого не жмётся: заголовок кадра zstd съест выигрыш.
const MIN_PACK: usize = 64;
/// Пробное сжатие — по началу части такой длины.
const PROBE: usize = 32 * 1024;

impl Packed {
    /// `data` сжатыми уровнем `level`, если это экономит хотя бы шестнадцатую часть; иначе как есть.
    pub(crate) fn new(data: Vec<u8>, level: Option<i32>) -> Self {
        let Some(level) = level.filter(|_| data.len() >= MIN_PACK) else { return Packed::Raw(data) };
        match zstd::bulk::compress(&data, level) {
            Ok(z) if worth(z.len(), data.len()) => Packed::Zstd(data.len() as u64, z),
            _ => Packed::Raw(data),
        }
    }

    /// Длина до сжатия.
    pub(crate) fn len(&self) -> u64 {
        match self {
            Packed::Raw(b) => b.len() as u64,
            Packed::Zstd(n, _) => *n,
        }
    }

    /// Сколько занимает в записи.
    pub(crate) fn stored(&self) -> u64 {
        match self {
            Packed::Raw(b) | Packed::Zstd(_, b) => b.len() as u64,
        }
    }

    pub(crate) fn into_bytes(self) -> Result<Vec<u8>> {
        match self {
            Packed::Raw(b) => Ok(b),
            Packed::Zstd(n, z) => {
                if n > MAX_UNPACKED {
                    return Err(Error(format!("packed record of {n} bytes")));
                }
                let out = zstd::bulk::decompress(&z, n as usize)?;
                if out.len() as u64 != n {
                    return Err(Error(format!("packed record: {} bytes, expected {n}", out.len())));
                }
                Ok(out)
            }
        }
    }
}

/// Сжатое `packed` вместо `raw` — выигрыш не меньше шестнадцатой части.
fn worth(packed: usize, raw: usize) -> bool {
    packed + raw / 16 <= raw
}

/// Типы, которые уже сжаты (или зашифрованы): не жмутся без пробы. `Some(true)` — заведомо сжимаемые, `None` —
/// решает проба.
fn compressible_type(mime: &str) -> Option<bool> {
    let mime = mime.to_ascii_lowercase();
    let (top, sub) = mime.split_once('/').unwrap_or((mime.as_str(), ""));
    match top {
        "text" | "message" => return Some(true),
        "video" | "audio" | "font" => return Some(false),
        "image" => return Some(matches!(sub, "svg+xml" | "bmp" | "x-bmp" | "x-icon" | "vnd.microsoft.icon")),
        _ => {}
    }
    if sub.ends_with("+xml") || sub.ends_with("+json") || matches!(sub, "json" | "xml" | "javascript" | "x-javascript" | "rtf" | "ics") {
        return Some(true);
    }
    let packed = sub.contains("zip")
        || sub.starts_with("vnd.openxmlformats-officedocument.")
        || sub.starts_with("vnd.oasis.opendocument.")
        || sub.starts_with("vnd.ms-") && sub.contains("macroenabled")
        || sub.starts_with("pkcs7-")
        || sub.starts_with("x-pkcs7-")
        || matches!(
            sub,
            "pdf"
                | "gzip"
                | "x-gzip"
                | "x-7z-compressed"
                | "x-rar-compressed"
                | "vnd.rar"
                | "x-rar"
                | "x-bzip2"
                | "x-xz"
                | "zstd"
                | "x-zstd"
                | "java-archive"
                | "vnd.android.package-archive"
                | "vnd.apple.pkpass"
                | "octet-stream-encrypted"
                | "pgp-encrypted"
        );
    if packed { Some(false) } else { None }
}

/// Часть стоит жать: тип не из уже сжатых и (для неизвестных) пробное сжатие начала даёт выигрыш.
fn compressible(mime: &str, data: &[u8], level: i32) -> bool {
    compressible_type(mime).unwrap_or_else(|| {
        let probe = &data[..data.len().min(PROBE)];
        zstd::bulk::compress(probe, level).is_ok_and(|z| worth(z.len(), probe.len()))
    })
}

/// `Vec<u8>` строк содержимого — одним куском (`serialize_bytes`), а не последовательностью по байту: в postcard
/// это те же байты (длина и данные), но без разбора каждого байта отдельно — запись и чтение частей в десятки КиБ
/// были на трети времени записи содержимого.
pub(crate) mod bytes {
    use std::fmt;

    use serde::de::{SeqAccess, Visitor};
    use serde::{Deserializer, Serializer};

    pub(crate) fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(v)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        d.deserialize_byte_buf(Bytes)
    }

    struct Bytes;

    impl<'de> Visitor<'de> for Bytes {
        type Value = Vec<u8>;

        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("bytes")
        }

        fn visit_bytes<E>(self, v: &[u8]) -> Result<Vec<u8>, E> {
            Ok(v.to_vec())
        }

        fn visit_byte_buf<E>(self, v: Vec<u8>) -> Result<Vec<u8>, E> {
            Ok(v)
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<u8>, A::Error> {
            let mut out = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(1 << 20));
            while let Some(b) = seq.next_element()? {
                out.push(b);
            }
            Ok(out)
        }
    }
}

/// Как часть была закодирована в исходнике — всё, что нужно, чтобы получить те же байты.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Coding {
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
pub(crate) struct Encoder {
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
    pub(crate) fn new(coding: Coding) -> Self {
        Self { coding, col: 0, carry: [0; 3], n: 0, pending: Vec::new() }
    }

    pub(crate) fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) {
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

    pub(crate) fn finish(mut self, out: &mut Vec<u8>) {
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
    open_all(tx, &TABLES)
}

pub(crate) fn open_all(tx: &WriteTransaction, t: &Tables) -> Result<()> {
    tx.open_table(t.content)?;
    tx.open_table(t.content_refs)?;
    tx.open_table(t.part)?;
    tx.open_table(t.part_refs)?;
    tx.open_table(t.chunks)?;
    tx.open_table(t.body)?;
    Ok(())
}

/// Часть вынимается: крупная или картинка.
fn wanted(p: &SplitPart, t: &Tuning) -> bool {
    let n = p.decoded.len();
    n >= t.min_part || n >= t.min_image && p.mime.get(..6).is_some_and(|s| s.eq_ignore_ascii_case("image/"))
}

/// Разрезать письмо: скелет и вынутые части (ссылка и часть). Части с неверным или пересекающимся диапазоном,
/// мелкие и не совпавшие при обратном кодировании остаются в скелете.
fn cut<'a>(raw: &[u8], split: &'a [SplitPart], t: &Tuning) -> (Vec<u8>, Vec<(PartRef, &'a SplitPart)>) {
    let mut order: Vec<&SplitPart> = split.iter().collect();
    order.sort_by_key(|p| p.range.start);
    let mut skeleton = Vec::with_capacity(raw.len());
    let mut parts = Vec::new();
    let mut pos = 0;
    for p in order {
        let Range { start, end } = p.range;
        if start < pos || end < start || end > raw.len() || !wanted(p, t) {
            continue;
        }
        let Some(coding) = Coding::matching(p.encoding, &p.decoded, &raw[start..end]) else { continue };
        skeleton.extend_from_slice(&raw[pos..start]);
        parts.push((PartRef { at: skeleton.len() as u64, hash: Hash::of(&p.decoded), coding }, p));
        pos = end;
    }
    skeleton.extend_from_slice(&raw[pos..]);
    (skeleton, parts)
}

/// Записать новую часть `p` (её ещё нет в `t.part`): одним значением или кусками, сжатой, если она сжимается.
/// Ответ — сколько байтов она занимает в записях.
fn put_part(tx: &WriteTransaction, t: &Tables, hash: Hash, p: &SplitPart, level: Option<i32>) -> Result<u64> {
    let bytes = &p.decoded;
    let level = level.filter(|&l| compressible(&p.mime, bytes, l));
    let mut stored = 0;
    let data = if bytes.len() > CHUNK {
        let mut chunks = tx.open_table(t.chunks)?;
        let mut n = 0u32;
        for c in bytes.chunks(CHUNK) {
            let packed = Packed::new(c.to_vec(), level);
            stored += packed.stored();
            chunks.insert((hash.0, n), enc(&packed)?.as_slice())?;
            n += 1;
        }
        PartData::Chunks(n)
    } else {
        let packed = Packed::new(bytes.clone(), level);
        stored += packed.stored();
        PartData::Bytes(packed)
    };
    tx.open_table(t.part)?.insert(hash.0, enc(&PartRow { data, len: bytes.len() as u64 })?.as_slice())?;
    Ok(stored)
}

/// Содержимое `hash` (байты `raw`) — ещё `letters` письмам: есть — счётчик растёт, нет — записать скелет и части
/// (новая часть — запись, известная — счётчик вхождений + 1).
pub(crate) fn acquire(tx: &WriteTransaction, t: &Tables, hash: Hash, raw: &[u8], split: &[SplitPart], letters: u32, tuning: &Tuning) -> Result<()> {
    let known = tx.open_table(t.content_refs)?.get(hash.0)?.map(|v| v.value());
    if let Some((n, logical, skel, stored)) = known {
        tx.open_table(t.content_refs)?.insert(hash.0, (n.saturating_add(letters), logical, skel, stored))?;
        return Ok(());
    }
    let (skeleton, cut_parts) = cut(raw, split, tuning);
    let mut refs = Vec::with_capacity(cut_parts.len());
    let mut logical = skeleton.len() as u64;
    for (r, p) in cut_parts {
        logical += p.decoded.len() as u64;
        let known = tx.open_table(t.part_refs)?.get(r.hash.0)?.map(|v| v.value());
        let meta = match known {
            Some((n, len, stored)) => (n.saturating_add(1), len, stored),
            None => (1, p.decoded.len() as u64, put_part(tx, t, r.hash, p, tuning.level)?),
        };
        tx.open_table(t.part_refs)?.insert(r.hash.0, meta)?;
        refs.push(r);
    }
    let skeleton = Packed::new(skeleton, tuning.level);
    tx.open_table(t.content_refs)?.insert(hash.0, (letters, logical, skeleton.len(), skeleton.stored()))?;
    let row = ContentRow { skeleton, parts: refs, size: raw.len() as u64 };
    tx.open_table(t.content)?.insert(hash.0, enc(&row)?.as_slice())?;
    Ok(())
}

/// Одним письмом с содержимым `hash` меньше: `refs − 1`; ноль — удалить содержимое и вхождения его частей.
pub(crate) fn release(tx: &WriteTransaction, hash: Hash) -> Result<()> {
    let t = &TABLES;
    let mut content_refs = tx.open_table(t.content_refs)?;
    let Some((n, logical, skel, stored)) = content_refs.get(hash.0)?.map(|v| v.value()) else { return Ok(()) };
    if n > 1 {
        content_refs.insert(hash.0, (n - 1, logical, skel, stored))?;
        return Ok(());
    }
    content_refs.remove(hash.0)?;
    let mut content = tx.open_table(t.content)?;
    let Some(row) = content.remove(hash.0)?.map(|v| dec::<ContentRow>(v.value())).transpose()? else { return Ok(()) };
    let mut part_refs = tx.open_table(t.part_refs)?;
    let mut part = tx.open_table(t.part)?;
    let mut chunks = tx.open_table(t.chunks)?;
    for p in &row.parts {
        let Some((n, len, stored)) = part_refs.get(p.hash.0)?.map(|v| v.value()) else { continue };
        if n > 1 {
            part_refs.insert(p.hash.0, (n - 1, len, stored))?;
            continue;
        }
        part_refs.remove(p.hash.0)?;
        if let Some(PartRow { data: PartData::Chunks(n), .. }) = part.remove(p.hash.0)?.map(|v| dec::<PartRow>(v.value())).transpose()? {
            for i in 0..n {
                chunks.remove((p.hash.0, i))?;
            }
        }
    }
    Ok(())
}

/// Читатель содержимого `hash` в транзакции `tx`; `None` — такого содержимого нет.
fn reader(tx: &ReadTransaction, hash: Hash) -> Result<Option<ContentReader>> {
    let Some(row) = tx.open_table(TABLES.content)?.get(hash.0)?.map(|v| dec::<ContentRow>(v.value())).transpose()? else {
        return Ok(None);
    };
    Ok(Some(ContentReader {
        skeleton: row.skeleton.into_bytes()?,
        parts: row.parts,
        part: tx.open_table(TABLES.part)?,
        chunks: tx.open_table(TABLES.chunks)?,
        pos: 0,
        next: 0,
        stream: None,
        buf: Vec::new(),
        off: 0,
    }))
}

pub(crate) fn hex(h: &Hash) -> String {
    h.0[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Чтение письма потоком: скелет кусками, части — кусками из базы, закодированными заново на лету.
pub struct ContentReader {
    skeleton: Vec<u8>,
    parts: Vec<PartRef>,
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
                            let bytes = dec::<Packed>(c.value())?.into_bytes()?;
                            s.encoder.feed(&bytes, &mut self.buf);
                        }
                    }
                    s.chunk += 1;
                } else if let Some(s) = self.stream.take() {
                    s.encoder.finish(&mut self.buf);
                    self.next += 1;
                }
                continue;
            }
            let end = self.parts.get(self.next).map_or(self.skeleton.len(), |p| p.at as usize).min(self.skeleton.len());
            if self.pos < end {
                let to = end.min(self.pos + SKELETON_PIECE);
                self.buf.extend_from_slice(&self.skeleton[self.pos..to]);
                self.pos = to;
                continue;
            }
            let Some(p) = self.parts.get(self.next) else { return Ok(()) };
            let Some(pr) = self.part.get(p.hash.0)?.map(|v| dec::<PartRow>(v.value())).transpose()? else {
                return Err(Error(format!("no part {}", hex(&p.hash))));
            };
            let (chunks, bytes) = match pr.data {
                PartData::Bytes(b) => (1, Some(b.into_bytes()?)),
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

/// Разобранное тело — значением строки `body`: postcard, сжатый zstd.
pub(crate) fn pack_body(b: &crate::BodyRow, level: Option<i32>) -> Result<Vec<u8>> {
    enc(&Packed::new(enc(b)?, level))
}

pub(crate) fn unpack_body(v: &[u8]) -> Result<crate::BodyRow> {
    dec(&dec::<Packed>(v)?.into_bytes()?)
}

/// Длина до сжатия и в записи у значения [`Packed`] — по его заголовку, без копии байтов.
fn packed_sizes(v: &[u8]) -> Result<(u64, u64)> {
    let (tag, rest) = postcard::take_from_bytes::<u32>(v)?;
    Ok(match tag {
        0 => {
            let (n, _) = postcard::take_from_bytes::<u64>(rest)?;
            (n, n)
        }
        _ => {
            let (len, rest) = postcard::take_from_bytes::<u64>(rest)?;
            let (n, _) = postcard::take_from_bytes::<u64>(rest)?;
            (len, n)
        }
    })
}

/// Сводка хранилища для «Диагностики» ([`Cache::store_stats`]): что занимали бы письма и тела без дедупликации и
/// сжатия, без повторов и как хранятся.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StoreStats {
    /// Строк писем.
    pub letters: u64,
    /// Уникальных писем целиком.
    pub contents: u64,
    /// Уникальных вынутых частей.
    pub parts: u64,
    /// Разобранных тел.
    pub bodies: u64,
    /// Байт без дедупликации и сжатия: каждое письмо — своим скелетом и своими частями, тела — как есть.
    pub logical: u64,
    /// Байт без повторов, без сжатия.
    pub unique: u64,
    /// Байт в записях.
    pub stored: u64,
}

impl StoreStats {
    /// Сэкономлено дедупликацией.
    pub fn dedup_saved(&self) -> u64 {
        self.logical.saturating_sub(self.unique)
    }

    /// Сэкономлено сжатием.
    pub fn packed_saved(&self) -> u64 {
        self.unique.saturating_sub(self.stored)
    }
}

impl Cache {
    #[doc(hidden)]
    pub fn tuned(mut self, tuning: Tuning) -> Self {
        self.tuning = tuning;
        self
    }

    /// Письмо целиком `raw` — содержимым письма `letter`: хэш, скелет и части по `split` (диапазоны листовых частей
    /// и их раскодированные байты от воркера; пусто — шифрованное или неразобранное, скелет — всё письмо). Одинаковые
    /// байты хранятся один раз; прежнее содержимое письма — `refs − 1`. Одной транзакцией: содержимого без письма не
    /// бывает.
    pub fn put_content(&self, letter: LetterId, raw: &[u8], split: &[SplitPart]) -> Result<Hash> {
        let [hash] = self.put_contents(&[(letter, raw, split)])?[..] else { return Err(Error("put_content: no hash".into())) };
        Ok(hash)
    }

    /// [`Cache::put_content`] для многих писем одной транзакцией (одна фиксация на диск): заливка архива, демо-ящик.
    /// Хэши — в порядке `items`; нет хоть одного письма — ничего не записано.
    pub fn put_contents(&self, items: &[(LetterId, &[u8], &[SplitPart])]) -> Result<Vec<Hash>> {
        let mut hashes = Vec::with_capacity(items.len());
        let tx = self.db.begin_write()?;
        {
            let mut l = Letters::open(&tx)?;
            for &(letter, raw, split) in items {
                let hash = Hash::of(raw);
                let Some(old) = l.get(letter)? else { return Err(Error(format!("no letter {}", letter.0))) };
                if old.content != Some(hash) {
                    acquire(&tx, &TABLES, hash, raw, split, 1, &self.tuning)?;
                    if let Some(prev) = old.content {
                        release(&tx, prev)?;
                    }
                    l.put(letter, Some(&old), &LetterRow { content: Some(hash), ..old.clone() })?;
                }
                hashes.push(hash);
            }
            l.finish()?;
        }
        tx.commit()?;
        Ok(hashes)
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
        reader(&self.db.begin_read()?, hash)
    }

    /// Письмо `letter` целиком (выгрузка на сервер цели); `None` — письма нет или содержимое не загружено.
    pub fn letter_content(&self, letter: LetterId) -> Result<Option<Vec<u8>>> {
        let Some(mut r) = self.letter_content_reader(letter)? else { return Ok(None) };
        let mut out = Vec::new();
        r.read_to_end(&mut out)?;
        Ok(Some(out))
    }

    /// [`Cache::letter_content`] потоком — строка письма и содержимое из одной транзакции чтения.
    pub fn letter_content_reader(&self, letter: LetterId) -> Result<Option<ContentReader>> {
        let tx = self.db.begin_read()?;
        let row: Option<LetterRow> = tx.open_table(LETTER)?.get(letter.0)?.map(|v| dec(v.value())).transpose()?;
        match row.and_then(|r| r.content) {
            Some(hash) => reader(&tx, hash),
            None => Ok(None),
        }
    }

    /// Части, вынутые из содержимого письма `letter` (листовые от [`MIN_PART`] и картинки от [`MIN_IMAGE`],
    /// раскодированные): SHA-256 и размер, по порядку в письме. Хэш уже посчитан при записи — по нему одинаковые
    /// вложения разных писем сравниваются без чтения байтов (вложения беседы новым участникам). Содержимого нет — пусто.
    pub fn letter_parts(&self, letter: LetterId) -> Result<Vec<(Hash, u64)>> {
        let tx = self.db.begin_read()?;
        let row: Option<LetterRow> = tx.open_table(LETTER)?.get(letter.0)?.map(|v| dec(v.value())).transpose()?;
        let Some(hash) = row.and_then(|r| r.content) else { return Ok(Vec::new()) };
        let Some(content) = tx.open_table(TABLES.content)?.get(hash.0)?.map(|v| dec::<ContentRow>(v.value())).transpose()? else {
            return Ok(Vec::new());
        };
        let meta = tx.open_table(TABLES.part_refs)?;
        let mut out = Vec::with_capacity(content.parts.len());
        for p in &content.parts {
            let Some((_, len, _)) = meta.get(p.hash.0)?.map(|v| v.value()) else { continue };
            out.push((p.hash, len));
        }
        Ok(out)
    }

    /// Сводка хранилища ([`StoreStats`]) — обходом счётчиков и тел (сами письма и части не читаются): звать в фоне.
    pub fn store_stats(&self) -> Result<StoreStats> {
        let tx = self.db.begin_read()?;
        let t = &TABLES;
        let mut s = StoreStats { letters: tx.open_table(LETTER)?.len()?, ..StoreStats::default() };
        for r in tx.open_table(t.part_refs)?.iter()? {
            let (_, len, stored) = r?.1.value();
            s.parts += 1;
            s.unique += len;
            s.stored += stored;
        }
        for r in tx.open_table(t.content_refs)?.iter()? {
            let (letters, logical, skel, stored) = r?.1.value();
            s.contents += 1;
            s.logical += u64::from(letters) * logical;
            s.unique += skel;
            s.stored += stored;
        }
        for r in tx.open_table(t.body)?.iter()? {
            let (len, stored) = packed_sizes(r?.1.value())?;
            s.bodies += 1;
            s.logical += len;
            s.unique += len;
            s.stored += stored;
        }
        Ok(s)
    }

    /// Пересчёт ссылок на содержимое и части с нуля по письмам и сверка со счётчиками (и кусками частей) — для
    /// тестов, переноса формата и диагностики. Ответ — расхождения по-английски (для журнала); пусто — сходится.
    pub fn check_refs(&self) -> Result<Vec<String>> {
        let tx = self.db.begin_read()?;
        let t = &TABLES;
        let mut want_content: HashMap<Hash, u32> = HashMap::new();
        for r in tx.open_table(LETTER)?.iter()? {
            if let Some(h) = dec::<LetterRow>(r?.1.value())?.content {
                *want_content.entry(h).or_default() += 1;
            }
        }
        let mut problems = Vec::new();
        let mut content_refs: HashMap<Hash, u32> = HashMap::new();
        for r in tx.open_table(t.content_refs)?.iter()? {
            let (k, v) = r?;
            content_refs.insert(Hash(k.value()), v.value().0);
        }
        let mut want_part: HashMap<Hash, u32> = HashMap::new();
        for r in tx.open_table(t.content)?.iter()? {
            let (k, v) = r?;
            let (h, row) = (Hash(k.value()), dec::<ContentRow>(v.value())?);
            let want = want_content.remove(&h).unwrap_or(0);
            let have = content_refs.remove(&h).unwrap_or(0);
            if have != want {
                problems.push(format!("content {}: refs {have}, letters {want}", hex(&h)));
            }
            for p in &row.parts {
                *want_part.entry(p.hash).or_default() += 1;
            }
        }
        for h in want_content.keys() {
            problems.push(format!("content {}: referenced by letters, missing", hex(h)));
        }
        for h in content_refs.keys() {
            problems.push(format!("content {}: refs without content", hex(h)));
        }
        let mut part_refs: HashMap<Hash, u32> = HashMap::new();
        for r in tx.open_table(t.part_refs)?.iter()? {
            let (k, v) = r?;
            part_refs.insert(Hash(k.value()), v.value().0);
        }
        let chunks = tx.open_table(t.chunks)?;
        let mut chunked: HashMap<Hash, u32> = HashMap::new();
        for r in tx.open_table(t.part)?.iter()? {
            let (k, v) = r?;
            let (h, row) = (Hash(k.value()), dec::<PartRow>(v.value())?);
            let want = want_part.remove(&h).unwrap_or(0);
            let have = part_refs.remove(&h).unwrap_or(0);
            if have != want {
                problems.push(format!("part {}: refs {have}, references {want}", hex(&h)));
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
        for h in want_part.keys() {
            problems.push(format!("part {}: referenced, missing", hex(h)));
        }
        for h in part_refs.keys() {
            problems.push(format!("part {}: refs without part", hex(h)));
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
pub(crate) mod tests {
    use redb::ReadableTableMetadata;

    use super::*;
    use crate::{BodyRow, FolderId, FolderRow, Owner};

    /// base64 независимо от [`Encoder`]: строки по `width` (0 — одной строкой), перевод строки `nl` после каждой.
    pub(crate) fn b64(data: &[u8], width: usize, nl: &str) -> String {
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

    pub(crate) fn noise(len: usize, seed: u64) -> Vec<u8> {
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

    /// Сжимаемый текст: строки с номерами.
    pub(crate) fn text(len: usize, seed: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity(len + 64);
        let mut i = seed;
        while out.len() < len {
            out.extend_from_slice(format!("Line {i}: quarterly report, department total {}.\r\n", i % 17).as_bytes());
            i += 1;
        }
        out.truncate(len);
        out
    }

    /// Письмо с вложениями base64 (`(тип, данные)`) и их части, как от воркера; `subject` делает письма разными.
    pub(crate) fn letter_of(subject: &str, attachments: &[(&str, &[u8])], nl: &str) -> (Vec<u8>, Vec<SplitPart>) {
        let mut raw = [format!("Subject: {subject}"), "MIME-Version: 1.0".into(), "Content-Type: multipart/mixed; boundary=\"b1\"".into()]
            .join(nl)
            .into_bytes();
        raw.extend_from_slice(format!("{nl}{nl}--b1{nl}Content-Type: text/plain{nl}{nl}Привет, {subject}{nl}").as_bytes());
        let mut parts = Vec::new();
        for (mime, data) in attachments {
            raw.extend_from_slice(format!("--b1{nl}Content-Type: {mime}{nl}Content-Transfer-Encoding: base64{nl}{nl}").as_bytes());
            let body = b64(data, 76, nl);
            let start = raw.len();
            raw.extend_from_slice(body.as_bytes());
            parts.push(SplitPart { range: start..raw.len(), encoding: Encoding::Base64, decoded: data.to_vec(), mime: mime.to_string() });
        }
        raw.extend_from_slice(format!("--b1--{nl}").as_bytes());
        (raw, parts)
    }

    /// Письмо с одним вложением `application/octet-stream` (`body` — уже закодированное) и часть для него.
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
        let part = SplitPart {
            range: head.len()..head.len() + body.len(),
            encoding: Encoding::Base64,
            decoded: data.to_vec(),
            mime: "application/octet-stream".into(),
        };
        (raw, part)
    }

    pub(crate) fn setup() -> (Cache, FolderId, FolderId) {
        let c = Cache::open_in_memory().unwrap();
        let folder = |a: &str| {
            c.upsert_folder(&FolderRow { owner: Owner::Account(a.into()), parent: None, name: "in".into(), role: None, remote: Some("in".into()) })
                .unwrap()
        };
        let (a, b) = (folder("a"), folder("b"));
        (c, a, b)
    }

    pub(crate) fn new_letter(c: &Cache, account: &str, id: &str, f: FolderId) -> LetterId {
        c.upsert_remote(account, id, &LetterRow { folders: vec![f], ..LetterRow::default() }).unwrap()
    }

    /// Строк в `content`, `part`, `chunk`.
    fn rows(c: &Cache) -> (usize, usize, usize) {
        let tx = c.db.begin_read().unwrap();
        let n = |t: TableDefinition<[u8; 32], &[u8]>| tx.open_table(t).unwrap().len().unwrap() as usize;
        (n(TABLES.content), n(TABLES.part), tx.open_table(TABLES.chunks).unwrap().len().unwrap() as usize)
    }

    pub(crate) fn part_refs(c: &Cache, data: &[u8]) -> u32 {
        let tx = c.db.begin_read().unwrap();
        tx.open_table(TABLES.part_refs).unwrap().get(Hash::of(data).0).unwrap().map_or(0, |v| v.value().0)
    }

    fn content_refs(c: &Cache, h: Hash) -> u32 {
        let tx = c.db.begin_read().unwrap();
        tx.open_table(TABLES.content_refs).unwrap().get(h.0).unwrap().map_or(0, |v| v.value().0)
    }

    fn part_row(c: &Cache, data: &[u8]) -> Option<PartRow> {
        let tx = c.db.begin_read().unwrap();
        tx.open_table(TABLES.part).unwrap().get(Hash::of(data).0).unwrap().map(|v| dec(v.value()).unwrap())
    }

    fn content_row(c: &Cache, h: Hash) -> ContentRow {
        let tx = c.db.begin_read().unwrap();
        dec(tx.open_table(TABLES.content).unwrap().get(h.0).unwrap().unwrap().value()).unwrap()
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
        assert_eq!((content_refs(&c, h), part_refs(&c, &data)), (2, 1));
        // Пересланное: другие заголовки, то же вложение.
        let (raw2, part2) = letter_with("Fwd: x", &data, &b64(&data, 76, "\r\n"), "\r\n");
        let z = new_letter(&c, "a", "z", a);
        let h2 = c.put_content(z, &raw2, &[part2]).unwrap();
        assert_eq!((rows(&c), part_refs(&c, &data)), ((2, 1, 0), 2));
        assert_eq!(c.content(h).unwrap().unwrap(), raw);
        assert_eq!(c.content(h2).unwrap().unwrap(), raw2);
        // Скелет без вложения; случайные байты не сжаты.
        let row = content_row(&c, h);
        assert!(row.skeleton.len() < 1024 && row.size == raw.len() as u64);
        assert!(matches!(part_row(&c, &data).unwrap().data, PartData::Bytes(Packed::Raw(_))));
        assert_refs(&c);

        c.delete_letters(&[x, z]).unwrap();
        assert_eq!((rows(&c), part_refs(&c, &data), content_refs(&c, h)), ((1, 1, 0), 1, 1));
        assert_refs(&c);
        c.delete_remote("b", &["x".into()]).unwrap();
        assert_eq!(rows(&c), (0, 0, 0));
        assert_eq!(c.content(h).unwrap(), None);
        assert_refs(&c);
    }

    /// Хэши вынутых частей письма — те, что посчитаны при записи: и одним значением, и кусками, и мелкие картинки.
    #[test]
    fn letter_parts_report_stored_hashes_and_sizes() {
        let (c, a, _) = setup();
        let small = noise(200 * 1024, 7);
        let big = noise(CHUNK + 300 * 1024, 8);
        let logo = noise(2000, 9);
        let (x, y, z) = (new_letter(&c, "a", "x", a), new_letter(&c, "a", "y", a), new_letter(&c, "a", "z", a));
        let (raw, part) = letter_with("x", &small, &b64(&small, 76, "\r\n"), "\r\n");
        c.put_content(x, &raw, std::slice::from_ref(&part)).unwrap();
        let (raw, parts) = letter_of("y", &[("application/pdf", &big), ("image/png", &logo)], "\r\n");
        c.put_content(y, &raw, &parts).unwrap();
        assert_eq!(c.letter_parts(x).unwrap(), [(Hash::of(&small), small.len() as u64)]);
        assert_eq!(c.letter_parts(y).unwrap(), [(Hash::of(&big), big.len() as u64), (Hash::of(&logo), logo.len() as u64)]);
        assert!(c.letter_parts(z).unwrap().is_empty(), "содержимого нет");
    }

    /// Логотип подписи в 1000 разных писем — одна запись части со счётчиком 1000; удаление писем убавляет счётчик,
    /// последнее — удаляет запись. Мелкая не-картинка остаётся в скелете, картинка от [`MIN_IMAGE`] — вынимается.
    #[test]
    fn one_logo_in_a_thousand_letters() {
        let (c, a, _) = setup();
        let logo = noise(8 * 1024, 11);
        let icon = noise(MIN_IMAGE + 100, 12);
        let note = noise(MIN_PART - 1, 13);
        let tiny = noise(MIN_IMAGE - 1, 14);
        let mut items = Vec::new();
        for i in 0..1000 {
            let (raw, parts) =
                letter_of(&format!("рассылка {i}"), &[("image/png", &logo), ("IMAGE/GIF", &icon), ("text/csv", &note), ("image/png", &tiny)], "\r\n");
            items.push((new_letter(&c, "a", &format!("m{i}"), a), raw, parts));
        }
        let batch: Vec<_> = items.iter().map(|(l, raw, parts)| (*l, raw.as_slice(), parts.as_slice())).collect();
        let hashes = c.put_contents(&batch).unwrap();
        assert_eq!(rows(&c), (1000, 2, 0), "логотип и значок — по одной записи");
        assert_eq!((part_refs(&c, &logo), part_refs(&c, &icon), part_refs(&c, &note), part_refs(&c, &tiny)), (1000, 1000, 0, 0));
        for (h, (_, raw, _)) in hashes.iter().zip(&items).step_by(97) {
            assert!(c.content(*h).unwrap().unwrap() == *raw);
        }
        assert_refs(&c);
        let st = c.store_stats().unwrap();
        assert_eq!((st.letters, st.contents, st.parts), (1000, 1000, 2));
        assert!(st.dedup_saved() >= 999 * (logo.len() + icon.len()) as u64, "{st:?}");

        let (gone, kept) = items.split_at(400);
        c.delete_letters(&gone.iter().map(|i| i.0).collect::<Vec<_>>()).unwrap();
        assert_eq!((rows(&c), part_refs(&c, &logo)), ((600, 2, 0), 600));
        assert_refs(&c);
        c.delete_letters(&kept.iter().map(|i| i.0).collect::<Vec<_>>()).unwrap();
        assert_eq!(rows(&c), (0, 0, 0));
        assert!(part_row(&c, &logo).is_none());
        assert_refs(&c);
    }

    /// Пакет одной транзакцией: общее вложение в двух письмах пакета — одна часть с двумя ссылками; нет одного из
    /// писем — не записано ничего.
    #[test]
    fn contents_in_one_transaction() {
        let (c, a, b) = setup();
        let data = noise(200 * 1024, 3);
        let (raw, part) = letter_with("x", &data, &b64(&data, 76, "\r\n"), "\r\n");
        let (raw2, part2) = letter_with("Fwd: x", &data, &b64(&data, 76, "\r\n"), "\r\n");
        let (x, y) = (new_letter(&c, "a", "x", a), new_letter(&c, "b", "y", b));
        let parts = [part];
        let hs = c.put_contents(&[(x, &raw, &parts), (y, &raw2, std::slice::from_ref(&part2))]).unwrap();
        assert_eq!(hs, vec![Hash::of(&raw), Hash::of(&raw2)]);
        assert_eq!((rows(&c), part_refs(&c, &data)), ((2, 1, 0), 2));
        assert_eq!(c.letter_content(y).unwrap().unwrap(), raw2);
        assert_refs(&c);

        let z = new_letter(&c, "a", "z", a);
        assert!(c.put_contents(&[(z, &raw, &parts), (LetterId(u64::MAX), &raw, &parts)]).is_err());
        assert_eq!(c.letter_content(z).unwrap(), None);
        assert_eq!((rows(&c), part_refs(&c, &data)), ((2, 1, 0), 2));
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
        let part = SplitPart { range: start..raw.len(), encoding: Encoding::QuotedPrintable, decoded: text.clone(), mime: "text/plain".into() };
        let l = new_letter(&c, "a", "qp", a);
        let h = c.put_content(l, &raw, &[part]).unwrap();
        assert_eq!(c.content(h).unwrap().unwrap(), raw);
        assert_eq!(part_refs(&c, &text), 1);
        assert!(matches!(part_row(&c, &text).unwrap().data, PartData::Bytes(Packed::Zstd(..))), "текст сжат");

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

    /// Сжатие: текст, тело и скелет — zstd, байты те же; уже сжатые типы — как есть даже при сжимаемых байтах;
    /// неизвестный тип — по пробе; крупная сжимаемая часть — сжатыми кусками.
    #[test]
    fn compressed_parts_are_byte_exact_and_packed_types_are_not() {
        let (c, a, _) = setup();
        let words = text(40 * 1024, 1);
        let files: [(&str, Vec<u8>, bool); 8] = [
            ("text/html", text(30 * 1024, 2), true),
            ("application/vnd.openxmlformats-officedocument.wordprocessingml.document", text(30 * 1024, 3), false),
            ("application/zip", text(30 * 1024, 4), false),
            ("application/pdf", text(30 * 1024, 5), false),
            ("image/jpeg", text(30 * 1024, 6), false),
            ("video/mp4", text(30 * 1024, 7), false),
            ("application/octet-stream", text(30 * 1024, 8), true),
            ("application/octet-stream", noise(30 * 1024, 9), false),
        ];
        let attachments: Vec<(&str, &[u8])> = files.iter().map(|(m, d, _)| (*m, d.as_slice())).collect();
        let (mut raw, mut parts) = letter_of("сжатие", &attachments, "\r\n");
        // Текст письма в скелете — крупный и сжимаемый (после всех частей).
        let at = raw.len() - b"--b1--\r\n".len();
        let tail = raw.split_off(at);
        raw.extend_from_slice(b"--b1\r\nContent-Type: text/plain\r\n\r\n");
        raw.extend_from_slice(&words);
        raw.extend_from_slice(b"\r\n");
        raw.extend_from_slice(&tail);
        parts.retain(|p| p.range.end <= at);
        let l = new_letter(&c, "a", "z", a);
        let h = c.put_content(l, &raw, &parts).unwrap();
        assert!(c.content(h).unwrap().unwrap() == raw);
        for (mime, data, packed) in &files {
            let row = part_row(&c, data).unwrap();
            assert_eq!(matches!(row.data, PartData::Bytes(Packed::Zstd(..))), *packed, "{mime}");
            assert_eq!(row.len, data.len() as u64);
        }
        let row = content_row(&c, h);
        assert!(matches!(row.skeleton, Packed::Zstd(..)) && row.skeleton.stored() * 3 < row.skeleton.len(), "скелет сжат");

        // Кусками по 1 МиБ, каждый сжат; поток — те же байты.
        let big = text(CHUNK * 2 + 777, 10);
        let (raw, parts) = letter_of("большой", &[("text/plain", &big)], "\n");
        let l = new_letter(&c, "a", "big", a);
        let h = c.put_content(l, &raw, &parts).unwrap();
        assert!(c.content(h).unwrap().unwrap() == raw);
        {
            let tx = c.db.begin_read().unwrap();
            let chunks = tx.open_table(TABLES.chunks).unwrap();
            for i in 0..3 {
                let v = chunks.get((Hash::of(&big).0, i)).unwrap().unwrap();
                assert!(matches!(dec::<Packed>(v.value()).unwrap(), Packed::Zstd(..)), "кусок {i}");
            }
        }

        // Тело — сжатым, читается то же.
        let body = BodyRow { html: Some(String::from_utf8(text(50 * 1024, 11)).unwrap()), ..BodyRow::default() };
        c.put_body("k", &body).unwrap();
        assert_eq!(c.body("k").unwrap(), Some(body.clone()));
        {
            let tx = c.db.begin_read().unwrap();
            let v = tx.open_table(BODY).unwrap().get("k").unwrap().unwrap();
            assert!(matches!(dec::<Packed>(v.value()).unwrap(), Packed::Zstd(..)));
        }
        assert_refs(&c);

        // Без сжатия ([`Tuning::FORMAT_6`]): всё как есть, байты те же.
        let (c6, f, _) = setup();
        let c6 = c6.tuned(Tuning::FORMAT_6);
        let l = new_letter(&c6, "a", "z", f);
        let h = c6.put_content(l, &raw, &parts).unwrap();
        assert!(c6.content(h).unwrap().unwrap() == raw);
        assert!(matches!(content_row(&c6, h).skeleton, Packed::Raw(_)));
        c6.put_body("k", &body).unwrap();
        assert_eq!(c6.body("k").unwrap(), Some(body));
    }

    /// Сводка: без повторов меньше, чем «как есть», сжатое — меньше, чем без повторов; тела учтены.
    #[test]
    fn stats_count_dedup_and_packing() {
        let (c, a, b) = setup();
        let logo = noise(10 * 1024, 21);
        let doc = text(100 * 1024, 22);
        let mut skeletons = 0u64;
        for i in 0..10 {
            let (raw, parts) = letter_of(&format!("письмо {i}"), &[("image/png", &logo), ("text/plain", &doc)], "\r\n");
            let l = new_letter(&c, "a", &format!("m{i}"), a);
            let h = c.put_content(l, &raw, &parts).unwrap();
            let l = new_letter(&c, "b", &format!("m{i}"), b);
            c.put_content(l, &raw, &parts).unwrap();
            skeletons += content_row(&c, h).skeleton.len();
        }
        let body = BodyRow { text: Some(String::from_utf8(text(20 * 1024, 23)).unwrap()), ..BodyRow::default() };
        c.put_body("x", &body).unwrap();
        let body_len = enc(&body).unwrap().len() as u64;
        let st = c.store_stats().unwrap();
        assert_eq!((st.letters, st.contents, st.parts, st.bodies), (20, 10, 2, 1));
        let parts = (logo.len() + doc.len()) as u64;
        assert_eq!(st.unique, skeletons + parts + body_len);
        // Без дедупликации: каждое из 20 писем — свой скелет и обе части.
        assert_eq!(st.logical, 2 * skeletons + 20 * parts + body_len);
        assert_eq!(st.dedup_saved(), skeletons + 19 * parts);
        assert!(st.packed_saved() > doc.len() as u64 / 2, "{st:?}");
        assert!(st.stored < st.unique);
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
        let part = SplitPart { range: start..start + body.len(), encoding: Encoding::Base64, decoded: data.clone(), mime: "application/pdf".into() };
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

    /// Повреждённая сжатая запись — ошибка, не паника и не гигабайт памяти.
    #[test]
    fn broken_packed_record_is_an_error() {
        assert!(Packed::Zstd(10, vec![1, 2, 3]).into_bytes().is_err());
        assert!(Packed::Zstd(u64::MAX, vec![1, 2, 3]).into_bytes().is_err());
        let Packed::Zstd(n, b) = Packed::new(text(10_000, 1), Some(LEVEL)) else { panic!("не сжато") };
        assert!(Packed::Zstd(n + 1, b.clone()).into_bytes().is_err());
        assert_eq!(Packed::Zstd(n, b).into_bytes().unwrap(), text(10_000, 1));
        assert_eq!(Packed::new(vec![7; 10], Some(LEVEL)), Packed::Raw(vec![7; 10]), "мелкое не жмётся");
    }

    /// 1 000 случайных записей содержимого и удалений писем: счётчики сходятся с пересчётом.
    #[test]
    fn refs_survive_random_writes() {
        let (c, a, b) = setup();
        let attachments: Vec<Vec<u8>> = (0..3).map(|i| noise(MIN_PART + i * 999, 10 + i as u64)).collect();
        let logo = noise(3000, 30);
        let letters: Vec<(Vec<u8>, Vec<SplitPart>)> = (0..6)
            .map(|i| letter_of(&format!("l{}", i / 2), &[("application/octet-stream", &attachments[i % 3]), ("image/png", &logo)], "\r\n"))
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
                    let (raw, parts) = &letters[next(6) as usize];
                    let split = if next(5) == 0 { Vec::new() } else { parts.clone() };
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
