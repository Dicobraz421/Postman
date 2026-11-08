//! Pets patch: быстрый путь `layout_line` (решение Урфина 2026-11-06, рецепт `dwrite-fast-path.md`).
//!
//! Старый путь на каждую строку создаёт `IDWriteTextFormat` и `IDWriteTextLayout`. Для строк,
//! где результат не зависит от соседних символов, глиф и ширина берутся из таблицы по
//! (шрифт, размер). Заполняет таблицу сам старый путь, по одному символу: так ловятся замены
//! шрифта, `locl` по локали и всё, что DirectWrite делает с символом без соседей. Зависимость
//! от соседей (лигатуры, контекстные замены, кернинг) — по таблицам шрифта, один раз на шрифт.
//! Всё остальное — старым путём, целой строкой.

use gpui::{FontId, Pixels};

/// Сколько таблиц (шрифт, размер) держим; при переполнении сбрасываем все. Размеров немного:
/// по одному на шрифт, и нечётные прогоны строки идут с размером `next_up`.
pub(super) const MAX_TABLES: usize = 32;

const RANGES: &[(u32, u32)] = &[
    (0x20, 0x7e),
    (0xa0, 0xac),
    // U+00AD — мягкий перенос.
    (0xae, 0xff),
    (0x400, 0x45f),
    (0x2010, 0x2027),
    (0x2116, 0x2116),
    (0x2190, 0x2193),
];

/// Символов быстрого пути: сумма длин [`RANGES`].
pub(super) const CHARS: usize = 95 + 13 + 82 + 96 + 24 + 1 + 4;

/// Индекс символа в таблицах; `None` — символ только старым путём.
#[inline]
pub(super) fn char_index(ch: char) -> Option<usize> {
    let c = ch as u32;
    // Самая частая ветка — первая: ASCII.
    if (0x20..=0x7e).contains(&c) {
        return Some((c - 0x20) as usize);
    }
    let mut base = 95;
    for &(start, end) in &RANGES[1..] {
        if c < start {
            return None;
        }
        if c <= end {
            return Some(base + (c - start) as usize);
        }
        base += (end - start + 1) as usize;
    }
    None
}

/// Все символы быстрого пути в порядке [`char_index`].
pub(super) fn all_chars() -> impl Iterator<Item = u32> {
    RANGES.iter().flat_map(|&(start, end)| start..=end)
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Slot {
    /// Ещё не спрашивали.
    Unknown,
    /// Не один глиф своего шрифта: такие строки — старым путём.
    Slow,
    Fast {
        glyph: u16,
        advance: f32,
    },
}

/// Таблица символов на (шрифт, размер).
#[derive(Debug)]
pub(super) struct SizeTable {
    font_id: FontId,
    size_bits: u32,
    pub slots: Box<[Slot; CHARS]>,
    /// Ascent и descent строки в этом шрифте; пусто — ещё не мерили.
    pub metrics: Option<(Pixels, Pixels)>,
}

/// Разбор шрифта: один раз на `FontId`.
#[derive(Debug)]
pub(super) struct FontTable {
    /// Глиф по `cmap`; 0 — символа в шрифте нет.
    pub cmap: Box<[u16; CHARS]>,
    /// Символ не участвует в правилах, зависящих от соседей.
    pub allowed: Box<[bool; CHARS]>,
}

#[derive(Debug, Default)]
pub(super) struct FastCache {
    /// По `FontId`: внешний `None` — не разобран, внутренний — шрифт целиком старым путём.
    pub fonts: Vec<Option<Option<FontTable>>>,
    pub tables: Vec<SizeTable>,
}

impl FastCache {
    /// Индекс таблицы; индекс живёт до следующего вызова с другим ключом.
    pub fn table(&mut self, font_id: FontId, size: f32) -> usize {
        let size_bits = size.to_bits();
        if let Some(ix) = self
            .tables
            .iter()
            .position(|t| t.font_id == font_id && t.size_bits == size_bits)
        {
            return ix;
        }
        if self.tables.len() >= MAX_TABLES {
            self.tables.clear();
        }
        self.tables.push(SizeTable {
            font_id,
            size_bits,
            slots: Box::new([Slot::Unknown; CHARS]),
            metrics: None,
        });
        self.tables.len() - 1
    }
}

/// Функции OpenType, которые DirectWrite включает сам. Необязательные (`ss01`, `zero`, …) сюда
/// не входят: шрифт с заданными `FontFeatures` идёт старым путём целиком.
const DEFAULT_FEATURES: &[&[u8; 4]] = &[
    b"ccmp", b"locl", b"rlig", b"rclt", b"calt", b"clig", b"liga", b"rvrn", b"kern", b"mark", b"mkmk", b"dist",
    b"abvm", b"blwm", b"abvs", b"blws", b"curs", b"pres", b"psts",
];

/// Скрипты символов быстрого пути. Символы `Common` идут со скриптом соседей либо `DFLT`.
const SCRIPTS: &[&[u8; 4]] = &[b"DFLT", b"dflt", b"latn", b"cyrl"];

#[derive(Clone, Copy)]
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn u16(self, at: usize) -> Option<u16> {
        let b = self.0.get(at..at + 2)?;
        Some(u16::from_be_bytes([b[0], b[1]]))
    }

    fn tag(self, at: usize) -> Option<&'a [u8]> {
        self.0.get(at..at + 4)
    }

    /// Подтаблица по 16-битному смещению в поле `at`.
    fn sub(self, at: usize) -> Option<Self> {
        Some(Self(self.0.get(usize::from(self.u16(at)?)..)?))
    }

    /// То же для 32-битного смещения.
    fn sub32(self, at: usize) -> Option<Self> {
        let hi = usize::from(self.u16(at)?);
        let lo = usize::from(self.u16(at + 2)?);
        Some(Self(self.0.get((hi << 16 | lo)..)?))
    }
}

/// Глифы покрытия в порядке индексов покрытия.
fn coverage(c: Reader<'_>) -> Option<Vec<u16>> {
    let mut out = Vec::new();
    match c.u16(0)? {
        1 => {
            for i in 0..usize::from(c.u16(2)?) {
                out.push(c.u16(4 + 2 * i)?);
            }
        }
        2 => {
            for i in 0..usize::from(c.u16(2)?) {
                let (start, end) = (c.u16(4 + 6 * i)?, c.u16(6 + 6 * i)?);
                out.extend(start..=end);
            }
        }
        _ => return None,
    }
    Some(out)
}

/// Какие глифы символов быстрого пути могут начать правило, зависящее от соседей.
struct Scan<'a> {
    /// Глифы символов быстрого пути.
    line: &'a [bool],
    hit: &'a mut [bool],
}

impl Scan<'_> {
    fn mark(&mut self, cov: Reader<'_>) -> Option<()> {
        for g in coverage(cov)? {
            if self.line[usize::from(g)] {
                self.hit[usize::from(g)] = true;
            }
        }
        Some(())
    }

    fn reachable(&self, cov: Reader<'_>) -> Option<bool> {
        Some(coverage(cov)?.iter().any(|&g| self.line[usize::from(g)]))
    }

    /// Формат 3: правило возможно, только если каждая позиция покрыта глифом быстрого пути.
    /// `i` + комбинируемый знак в строке без таких знаков не сработает.
    fn context3(&mut self, sub: Reader<'_>, covs: &[usize], first: usize) -> Option<()> {
        for &at in covs {
            if !self.reachable(sub.sub(at)?)? {
                return Some(());
            }
        }
        self.mark(sub.sub(first)?)
    }

    fn ligature(&mut self, sub: Reader<'_>) -> Option<()> {
        let sets = usize::from(sub.u16(4)?);
        for (i, g) in coverage(sub.sub(2)?)?.into_iter().enumerate() {
            if i >= sets || !self.line[usize::from(g)] {
                continue;
            }
            let set = sub.sub(6 + 2 * i)?;
            for j in 0..usize::from(set.u16(0)?) {
                let lig = set.sub(2 + 2 * j)?;
                let mut all = true;
                for k in 0..usize::from(lig.u16(2)?).saturating_sub(1) {
                    all &= self.line[usize::from(lig.u16(4 + 2 * k)?)];
                }
                if all {
                    self.hit[usize::from(g)] = true;
                }
            }
        }
        Some(())
    }

    fn context(&mut self, sub: Reader<'_>) -> Option<()> {
        if sub.u16(0)? != 3 {
            return self.mark(sub.sub(2)?);
        }
        let n = usize::from(sub.u16(2)?);
        if n == 0 {
            return Some(());
        }
        let covs: Vec<usize> = (0..n).map(|i| 6 + 2 * i).collect();
        self.context3(sub, &covs, 6)
    }

    fn chain(&mut self, sub: Reader<'_>) -> Option<()> {
        if sub.u16(0)? != 3 {
            return self.mark(sub.sub(2)?);
        }
        let back = usize::from(sub.u16(2)?);
        let input_at = 4 + 2 * back;
        let input = usize::from(sub.u16(input_at)?);
        let ahead_at = input_at + 2 + 2 * input;
        let ahead = usize::from(sub.u16(ahead_at)?);
        if input == 0 {
            return Some(());
        }
        let mut covs: Vec<usize> = (0..back).map(|i| 4 + 2 * i).collect();
        covs.extend((0..input).map(|i| input_at + 2 + 2 * i));
        covs.extend((0..ahead).map(|i| ahead_at + 2 + 2 * i));
        self.context3(sub, &covs, input_at + 2)
    }

    fn subtable(&mut self, gpos: bool, mut ty: u16, mut sub: Reader<'_>) -> Option<()> {
        if ty == if gpos { 9 } else { 7 } {
            ty = sub.u16(2)?;
            sub = sub.sub32(4)?;
        }
        match (gpos, ty) {
            // Без соседей: результат виден при заполнении таблицы по одному символу.
            (false, 1..=3) | (true, 1) => Some(()),
            (false, 4) => self.ligature(sub),
            (false, 5) | (true, 7) => self.context(sub),
            (false, 6) | (true, 8) => self.chain(sub),
            (false, 8) | (true, 2..=6) => self.mark(sub.sub(2)?),
            _ => None,
        }
    }

    fn layout_table(&mut self, table: &[u8], gpos: bool) -> Option<()> {
        let t = Reader(table);
        let (scripts, features, lookups) = (t.sub(4)?, t.sub(6)?, t.sub(8)?);
        let mut wanted = vec![false; usize::from(lookups.u16(0)?)];
        let mut lang_sys = |ls: Reader<'_>| -> Option<()> {
            let required = ls.u16(2)?;
            let n = usize::from(ls.u16(4)?);
            let list = (0..n).map(|i| ls.u16(6 + 2 * i));
            for ix in list.chain(std::iter::once(Some(required))) {
                let ix = ix?;
                if ix == 0xffff {
                    continue;
                }
                let at = 2 + 6 * usize::from(ix);
                let tag = features.tag(at)?;
                if ix != required && !DEFAULT_FEATURES.iter().any(|t| t[..] == *tag) {
                    continue;
                }
                let feature = features.sub(at + 4)?;
                for k in 0..usize::from(feature.u16(2)?) {
                    *wanted.get_mut(usize::from(feature.u16(4 + 2 * k)?))? = true;
                }
            }
            Some(())
        };
        for i in 0..usize::from(scripts.u16(0)?) {
            let at = 2 + 6 * i;
            let tag = scripts.tag(at)?;
            if !SCRIPTS.iter().any(|t| t[..] == *tag) {
                continue;
            }
            let script = scripts.sub(at + 4)?;
            if script.u16(0)? != 0 {
                lang_sys(script.sub(0)?)?;
            }
            for j in 0..usize::from(script.u16(2)?) {
                lang_sys(script.sub(4 + 6 * j + 4)?)?;
            }
        }
        for (i, on) in wanted.iter().enumerate() {
            if !*on {
                continue;
            }
            let lookup = lookups.sub(2 + 2 * i)?;
            let ty = lookup.u16(0)?;
            for k in 0..usize::from(lookup.u16(4)?) {
                self.subtable(gpos, ty, lookup.sub(6 + 2 * k)?)?;
            }
        }
        Some(())
    }

    /// Старая таблица `kern`: версия 0, формат 0 (левые глифы пар).
    fn kern(&mut self, table: &[u8]) -> Option<()> {
        let t = Reader(table);
        if t.u16(0)? != 0 {
            return None;
        }
        let mut at = 4;
        for _ in 0..t.u16(2)? {
            if t.u16(at + 4)? >> 8 != 0 {
                return None;
            }
            for i in 0..usize::from(t.u16(at + 6)?) {
                let left = usize::from(t.u16(at + 14 + 6 * i)?);
                if self.line[left] {
                    self.hit[left] = true;
                }
            }
            at += usize::from(t.u16(at + 2)?);
        }
        Some(())
    }
}

/// Отметить в `hit` глифы из `line`, с которых может начаться правило, зависящее от соседей.
/// `None` — таблица не разобрана, шрифт идёт старым путём.
pub(super) fn scan_tables(
    gsub: Option<&[u8]>,
    gpos: Option<&[u8]>,
    kern: Option<&[u8]>,
    line: &[bool],
    hit: &mut [bool],
) -> Option<()> {
    let mut scan = Scan { line, hit };
    if let Some(t) = gsub {
        scan.layout_table(t, false)?;
    }
    if let Some(t) = gpos {
        scan.layout_table(t, true)?;
    }
    if let Some(t) = kern {
        scan.kern(t)?;
    }
    Some(())
}

#[cfg(test)]
#[path = "fast_tests.rs"]
mod tests;
