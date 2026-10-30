//! NDB (MS-PST 2.2): узлы и блоки нового Unicode-файла. Блоки пишутся подряд с 0x4800 (данные, XBLOCK/XXBLOCK,
//! SLBLOCK/SIBLOCK; без шифрования, `NDB_CRYPT_NONE`), деревья узлов (NBT) и блоков (BBT) строятся в конце страницами
//! по 512 байт, затем — карты размещения (AMap, PMap, FMap), пустой DList и заголовок с CRC. Файл собирается в памяти
//! и отдаётся целиком; правки существующего файла и транзакций нет.

use std::io::{self, Write};

/// Данные блока без трейлера — не больше.
pub(super) const BLOCK_MAX: usize = 8176;
const TRAILER: usize = 16;
const PAGE: u64 = 512;
const DLIST: u64 = 0x4200;
const AMAP_FIRST: u64 = 0x4400;
/// Сколько байт описывает одна AMap: 496 байт карты, бит — 64 байта.
const AMAP_RANGE: u64 = 496 * 8 * 64;
/// FMap — с 128-й AMap, дальше через 496; FPMap (с 8192-й, файлы больше 2 ГиБ) не пишем.
const FMAP_FIRST: u64 = 128;
const FMAP_EVERY: u64 = 496;
const AMAP_MAX: u64 = 8192;
/// Записей в XBLOCK и XXBLOCK; в SLBLOCK (по 24 байта) и SIBLOCK (по 16).
const XENTRIES: usize = (BLOCK_MAX - 8) / 8;
const SLENTRIES: usize = (BLOCK_MAX - 8) / 24;
const SIENTRIES: usize = (BLOCK_MAX - 8) / 16;

const PTYPE_BBT: u8 = 0x80;
const PTYPE_NBT: u8 = 0x81;
const PTYPE_FMAP: u8 = 0x82;
const PTYPE_PMAP: u8 = 0x83;
const PTYPE_AMAP: u8 = 0x84;
const PTYPE_DLIST: u8 = 0x86;

/// Узел в поддереве (SLENTRY): данные и своё поддерево (0 — нет).
#[derive(Debug, Clone, Copy)]
pub(super) struct Sub {
    pub nid: u32,
    pub data: u64,
    pub sub: u64,
}

struct Block {
    bid: u64,
    ib: u64,
    cb: u16,
}

struct Node {
    nid: u32,
    data: u64,
    sub: u64,
    parent: u32,
}

#[derive(Default)]
pub(super) struct Ndb {
    file: Vec<u8>,
    /// Первый свободный байт за последним размещением.
    end: u64,
    next_bid: u64,
    next_page: u64,
    blocks: Vec<Block>,
    nodes: Vec<Node>,
    /// Занятые участки (смещение, длина кратна 64) — для AMap.
    used: Vec<(u64, u64)>,
}

impl Ndb {
    pub(super) fn new() -> Self {
        Self { end: AMAP_FIRST, next_bid: 1, next_page: 1, ..Default::default() }
    }

    /// Блок данных или служебный (`internal`: XBLOCK, SLBLOCK…) → BID.
    fn block(&mut self, data: &[u8], internal: bool) -> io::Result<u64> {
        if data.len() > BLOCK_MAX {
            return Err(io::Error::other("блок PST больше 8176 байт"));
        }
        let bid = self.next_bid << 2 | if internal { 2 } else { 0 };
        self.next_bid += 1;
        let size = (data.len() + TRAILER).next_multiple_of(64) as u64;
        let ib = self.alloc(size, 64)?;
        let at = ib as usize;
        self.file[at..at + data.len()].copy_from_slice(data);
        let t = at + size as usize - TRAILER;
        self.file[t..t + 2].copy_from_slice(&(data.len() as u16).to_le_bytes());
        self.file[t + 2..t + 4].copy_from_slice(&sig(ib, bid).to_le_bytes());
        self.file[t + 4..t + 8].copy_from_slice(&crc(data).to_le_bytes());
        self.file[t + 8..t + 16].copy_from_slice(&bid.to_le_bytes());
        self.blocks.push(Block { bid, ib, cb: data.len() as u16 });
        Ok(bid)
    }

    /// Данные узла кусками заданного размера (страницы кучи, строки таблицы): один — блок, больше — XBLOCK.
    pub(super) fn chunks(&mut self, chunks: &[Vec<u8>]) -> io::Result<u64> {
        match chunks {
            [one] => self.block(one, false),
            _ => self.tree(&chunks.iter().map(Vec::as_slice).collect::<Vec<_>>()),
        }
    }

    fn tree(&mut self, chunks: &[&[u8]]) -> io::Result<u64> {
        let mut leaves = Vec::with_capacity(chunks.len());
        for c in chunks {
            leaves.push((self.block(c, false)?, c.len() as u64));
        }
        if leaves.len() <= XENTRIES {
            return self.xblock(1, &leaves);
        }
        if leaves.len() > XENTRIES * XENTRIES {
            return Err(io::Error::other("данные узла PST больше 8 ГиБ"));
        }
        let mut xs = Vec::new();
        for part in leaves.chunks(XENTRIES) {
            xs.push((self.xblock(1, part)?, part.iter().map(|(_, n)| n).sum()));
        }
        self.xblock(2, &xs)
    }

    /// XBLOCK (уровень 1) или XXBLOCK (2): btype 1, уровень, число записей, общий размер данных, BID.
    fn xblock(&mut self, level: u8, entries: &[(u64, u64)]) -> io::Result<u64> {
        let total: u64 = entries.iter().map(|(_, n)| n).sum();
        let total = u32::try_from(total).map_err(|_| io::Error::other("данные узла PST больше 4 ГиБ"))?;
        let mut b = vec![1, level];
        b.extend((entries.len() as u16).to_le_bytes());
        b.extend(total.to_le_bytes());
        for (bid, _) in entries {
            b.extend(bid.to_le_bytes());
        }
        self.block(&b, true)
    }

    /// Поддерево узла: SLBLOCK, больше 340 записей — SIBLOCK над ними. Записи упорядочиваются по NID.
    pub(super) fn subnodes(&mut self, mut subs: Vec<Sub>) -> io::Result<u64> {
        subs.sort_by_key(|s| s.nid);
        if subs.len() <= SLENTRIES {
            return self.slblock(&subs);
        }
        if subs.len() > SLENTRIES * SIENTRIES {
            return Err(io::Error::other("слишком много подузлов PST"));
        }
        let mut b = vec![2, 1];
        b.extend((subs.len().div_ceil(SLENTRIES) as u16).to_le_bytes());
        b.extend([0; 4]);
        for part in subs.chunks(SLENTRIES) {
            let bid = self.slblock(part)?;
            b.extend(u64::from(part[0].nid).to_le_bytes());
            b.extend(bid.to_le_bytes());
        }
        self.block(&b, true)
    }

    fn slblock(&mut self, subs: &[Sub]) -> io::Result<u64> {
        let mut b = vec![2, 0];
        b.extend((subs.len() as u16).to_le_bytes());
        b.extend([0; 4]);
        for s in subs {
            b.extend(u64::from(s.nid).to_le_bytes());
            b.extend(s.data.to_le_bytes());
            b.extend(s.sub.to_le_bytes());
        }
        self.block(&b, true)
    }

    /// Узел в NBT: данные (0 — пустой узел), поддерево (0 — нет), родитель (0 — нет).
    pub(super) fn node(&mut self, nid: u32, data: u64, sub: u64, parent: u32) {
        self.nodes.push(Node { nid, data, sub, parent });
    }

    /// Место под блок или страницу: выравнивание, в пределах одной AMap и мимо её служебных страниц.
    fn alloc(&mut self, size: u64, align: u64) -> io::Result<u64> {
        let mut ib = self.end.next_multiple_of(align);
        loop {
            let k = (ib - AMAP_FIRST) / AMAP_RANGE;
            if k >= AMAP_MAX {
                return Err(io::Error::other("PST больше 2 ГиБ"));
            }
            let start = AMAP_FIRST + k * AMAP_RANGE;
            let data = start + reserved(k) * PAGE;
            if ib < data {
                ib = data;
            }
            if ib + size > start + AMAP_RANGE {
                ib = start + AMAP_RANGE;
                continue;
            }
            break;
        }
        self.end = ib + size;
        if self.file.len() < self.end as usize {
            self.file.resize(self.end as usize, 0);
        }
        self.used.push((ib, size));
        Ok(ib)
    }

    /// Страница дерева (BTPAGE): записи, их число, предел, размер записи, уровень; трейлер с подписью и CRC.
    fn page(&mut self, ptype: u8, level: u8, entries: &[u8], count: usize, size: usize) -> io::Result<(u64, u64)> {
        let ib = self.alloc(PAGE, PAGE)?;
        let bid = self.next_page;
        self.next_page += 1;
        let mut p = [0u8; 496];
        p[..entries.len()].copy_from_slice(entries);
        p[488] = count as u8;
        p[489] = (488 / size) as u8;
        p[490] = size as u8;
        p[491] = level;
        self.put_page(ib, &p, ptype, sig(ib, bid), bid);
        Ok((bid, ib))
    }

    fn put_page(&mut self, ib: u64, body: &[u8; 496], ptype: u8, wsig: u16, bid: u64) {
        let at = ib as usize;
        self.file[at..at + 496].copy_from_slice(body);
        self.file[at + 496] = ptype;
        self.file[at + 497] = ptype;
        self.file[at + 498..at + 500].copy_from_slice(&wsig.to_le_bytes());
        self.file[at + 500..at + 504].copy_from_slice(&crc(body).to_le_bytes());
        self.file[at + 504..at + 512].copy_from_slice(&bid.to_le_bytes());
    }

    /// B-дерево из упорядоченных записей (ключ, байты записи) → BREF корня.
    fn btree(&mut self, ptype: u8, entries: &[(u64, Vec<u8>)], size: usize) -> io::Result<(u64, u64)> {
        let per = 488 / size;
        let mut level: Vec<(u64, u64, u64)> = Vec::new();
        let mut parts: Vec<&[(u64, Vec<u8>)]> = entries.chunks(per).collect();
        if parts.is_empty() {
            parts.push(&[]);
        }
        for part in parts {
            let bytes: Vec<u8> = part.iter().flat_map(|(_, e)| e.iter().copied()).collect();
            let (bid, ib) = self.page(ptype, 0, &bytes, part.len(), size)?;
            level.push((part.first().map_or(0, |(k, _)| *k), bid, ib));
        }
        let mut depth = 1;
        while level.len() > 1 {
            let mut up = Vec::new();
            for part in level.chunks(488 / 24) {
                let bytes: Vec<u8> = part.iter().flat_map(|(k, bid, ib)| [k, bid, ib].into_iter().flat_map(|v| v.to_le_bytes())).collect();
                let (bid, ib) = self.page(ptype, depth, &bytes, part.len(), 24)?;
                up.push((part[0].0, bid, ib));
            }
            level = up;
            depth += 1;
        }
        Ok((level[0].1, level[0].2))
    }

    /// Деревья, карты, DList и заголовок → файл. `unique` — dwUnique, `nids` — последний занятый индекс NID по типам.
    pub(super) fn finish(mut self, out: &mut impl Write, unique: u32, nids: &[u32; 32]) -> io::Result<()> {
        // BBT: BREF, cb, cRef (2: запись BBT и тот, кто ссылается), выравнивание. NBT: nid, данные, поддерево, родитель.
        self.blocks.sort_by_key(|b| b.bid);
        let bbt: Vec<(u64, Vec<u8>)> = self
            .blocks
            .iter()
            .map(|b| {
                let mut e = Vec::with_capacity(24);
                e.extend(b.bid.to_le_bytes());
                e.extend(b.ib.to_le_bytes());
                e.extend(b.cb.to_le_bytes());
                e.extend(2u16.to_le_bytes());
                e.extend([0; 4]);
                (b.bid, e)
            })
            .collect();
        self.nodes.sort_by_key(|n| n.nid);
        let nbt: Vec<(u64, Vec<u8>)> = self
            .nodes
            .iter()
            .map(|n| {
                let mut e = Vec::with_capacity(32);
                e.extend(u64::from(n.nid).to_le_bytes());
                e.extend(n.data.to_le_bytes());
                e.extend(n.sub.to_le_bytes());
                e.extend(n.parent.to_le_bytes());
                e.extend([0; 4]);
                (u64::from(n.nid), e)
            })
            .collect();
        let bbt_root = self.btree(PTYPE_BBT, &bbt, 24)?;
        let nbt_root = self.btree(PTYPE_NBT, &nbt, 32)?;

        // Файл — до конца последней AMap; карты описывают и свободный хвост.
        let amaps = (self.end - AMAP_FIRST).div_ceil(AMAP_RANGE);
        let eof = AMAP_FIRST + amaps * AMAP_RANGE;
        self.file.resize(eof as usize, 0);
        let mut maps = vec![[0u8; 496]; amaps as usize];
        for k in 0..amaps {
            mark(&mut maps[k as usize], 0, reserved(k) * PAGE);
        }
        for &(ib, size) in &self.used {
            let k = (ib - AMAP_FIRST) / AMAP_RANGE;
            mark(&mut maps[k as usize], ib - AMAP_FIRST - k * AMAP_RANGE, size);
        }
        let mut free = 0;
        for (k, bits) in maps.iter().enumerate() {
            let ib = AMAP_FIRST + k as u64 * AMAP_RANGE;
            free += bits.iter().map(|b| u64::from(b.count_zeros())).sum::<u64>() * 64;
            self.put_page(ib, bits, PTYPE_AMAP, 0, ib);
            if k % 8 == 0 {
                // PMap устарела: все страницы «заняты».
                self.put_page(ib + PAGE, &[0xFF; 496], PTYPE_PMAP, 0, ib + PAGE);
            }
            if is_fmap(k as u64) {
                // FMap: на каждую из следующих 496 AMap — самый длинный свободный участок (в битах, до 255).
                let mut f = [0u8; 496];
                for (j, slot) in f.iter_mut().enumerate() {
                    if let Some(m) = maps.get(k + j) {
                        *slot = longest_free(m);
                    }
                }
                self.put_page(ib + 2 * PAGE, &f, PTYPE_FMAP, 0, ib + 2 * PAGE);
            }
        }

        // DList пуст (как в новом файле Outlook), его BID — следующий номер страницы.
        let next_page = self.next_page;
        self.put_page(DLIST, &[0; 496], PTYPE_DLIST, sig(DLIST, next_page), next_page);

        let mut h = [0u8; 564];
        h[0..4].copy_from_slice(b"!BDN");
        h[8..10].copy_from_slice(b"SM");
        h[10..12].copy_from_slice(&23u16.to_le_bytes()); // wVer: Unicode
        h[12..14].copy_from_slice(&19u16.to_le_bytes()); // wVerClient
        h[14] = 1; // bPlatformCreate
        h[15] = 1; // bPlatformAccess
        h[32..40].copy_from_slice(&next_page.to_le_bytes());
        h[40..44].copy_from_slice(&unique.to_le_bytes());
        for (i, nid) in nids.iter().enumerate() {
            h[44 + 4 * i..48 + 4 * i].copy_from_slice(&nid.to_le_bytes());
        }
        // ROOT: ibFileEof, ibAMapLast, cbAMapFree, cbPMapFree, BREF NBT, BREF BBT, fAMapValid = VALID_AMAP2.
        let root = [eof, AMAP_FIRST + (amaps - 1) * AMAP_RANGE, free, 0, nbt_root.0, nbt_root.1, bbt_root.0, bbt_root.1];
        for (i, v) in root.iter().enumerate() {
            h[184 + 8 * i..192 + 8 * i].copy_from_slice(&v.to_le_bytes());
        }
        h[248] = 2;
        // rgbFM, rgbFP устарели: 0xFF.
        h[256..512].fill(0xFF);
        h[512] = 0x80; // bSentinel
        h[513] = 0; // bCryptMethod: NDB_CRYPT_NONE
        h[516..524].copy_from_slice(&(self.next_bid << 2).to_le_bytes());
        let partial = crc(&h[8..8 + 471]);
        h[4..8].copy_from_slice(&partial.to_le_bytes());
        let full = crc(&h[8..524]);
        h[524..528].copy_from_slice(&full.to_le_bytes());
        self.file[..564].copy_from_slice(&h);
        out.write_all(&self.file)
    }
}

/// Служебных страниц в начале AMap `k`: сама AMap, PMap (каждая восьмая), FMap.
fn reserved(k: u64) -> u64 {
    1 + u64::from(k.is_multiple_of(8)) + u64::from(is_fmap(k))
}

fn is_fmap(k: u64) -> bool {
    k >= FMAP_FIRST && (k - FMAP_FIRST).is_multiple_of(FMAP_EVERY)
}

/// Отметить в AMap участок от `offset` (от начала её области) длиной `size` (кратно 64).
fn mark(bits: &mut [u8; 496], offset: u64, size: u64) {
    for slot in offset / 64..(offset + size) / 64 {
        bits[(slot / 8) as usize] |= 0x80 >> (slot % 8);
    }
}

fn longest_free(bits: &[u8; 496]) -> u8 {
    let (mut best, mut run) = (0u32, 0u32);
    for slot in 0..496 * 8 {
        if bits[slot / 8] & (0x80 >> (slot % 8)) == 0 {
            run += 1;
            best = best.max(run);
        } else {
            run = 0;
        }
    }
    best.min(255) as u8
}

/// Подпись блока и страницы (MS-PST 5.5): младшие 32 бита смещения и BID.
fn sig(ib: u64, bid: u64) -> u16 {
    let v = (ib ^ bid) as u32;
    ((v >> 16) ^ (v & 0xFFFF)) as u16
}

const CRC_TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

/// CRC MS-PST (5.3): CRC-32 без начального и конечного обращения.
pub(super) fn crc(data: &[u8]) -> u32 {
    data.iter().fold(0, |c, &b| CRC_TABLE[((c ^ u32::from(b)) & 0xFF) as usize] ^ (c >> 8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_and_sig() {
        // CRC-32 с нулевым началом и без обращения результата (обычный CRC-32 этой строки — CBF43926).
        assert_eq!(crc(b""), 0);
        assert_eq!(crc(b"123456789"), 0x2DFD_2D88);
        // Как у страниц пустого файла Outlook: DList 0x4200 с BID 0x17F, BBT 0x8400 с BID 0x17B.
        assert_eq!(sig(0x4200, 0x17F), 0x437F);
        assert_eq!(sig(0x8400, 0x17B), 0x857B);
    }

    #[test]
    fn allocation_skips_map_pages() {
        let mut ndb = Ndb::new();
        // Первые две страницы — AMap и PMap.
        assert_eq!(ndb.alloc(64, 64).unwrap(), 0x4800);
        // Блок не пересекает границу области AMap: следующая начинается её страницей.
        ndb.end = AMAP_FIRST + AMAP_RANGE - 64;
        assert_eq!(ndb.alloc(128, 64).unwrap(), AMAP_FIRST + AMAP_RANGE + PAGE);
        assert_eq!(reserved(0), 2);
        assert_eq!(reserved(1), 1);
        assert_eq!(reserved(128), 3);
        assert_eq!(reserved(624), 3);
    }
}
