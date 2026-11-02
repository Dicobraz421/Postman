//! NDB (MS-PST 2.2): узлы и блоки нового Unicode-файла — потоком. Блоки пишутся подряд с 0x4800 (данные, XBLOCK/
//! XXBLOCK, SLBLOCK/SIBLOCK; без шифрования, `NDB_CRYPT_NONE`); карты размещения (AMap, PMap, FMap, FPMap) — на своих
//! местах: служебные страницы области — при входе в неё, AMap — когда область заполнена (размещение только растёт,
//! назад не возвращается). Деревья узлов (NBT) и блоков (BBT) строятся в конце страницами по 512 байт из записей,
//! накопленных по возрастанию ключа в рабочих файлах рядом с томом; в памяти — только узлы папок и служебные (их
//! мало) и блоки, номер которых занят раньше записи (первые блоки кучи большой таблицы). Затем — пустой DList и
//! заголовок с CRC. Правки существующего файла и транзакций нет.

use std::collections::BTreeMap;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};

use super::Storage;

/// Данные блока без трейлера — не больше.
pub(super) const BLOCK_MAX: usize = 8176;
const TRAILER: usize = 16;
const PAGE: u64 = 512;
const DLIST: u64 = 0x4200;
const AMAP_FIRST: u64 = 0x4400;
/// Сколько байт описывает одна AMap: 496 байт карты, бит — 64 байта.
const AMAP_RANGE: u64 = 496 * 8 * 64;
/// FMap — с 128-й AMap (первые 128 описывает заголовок), дальше через 496.
const FMAP_FIRST: u64 = 128;
const FMAP_EVERY: u64 = 496;
/// FPMap — с 8192-й AMap (первые 2 ГиБ описывает заголовок), дальше через 496 · 8 PMap (8 ГиБ).
const FPMAP_FIRST: u64 = 8192;
const FPMAP_EVERY: u64 = 496 * 8 * 8;
/// Записей в XBLOCK и XXBLOCK; в SLBLOCK (по 24 байта) и SIBLOCK (по 16).
const XENTRIES: usize = (BLOCK_MAX - 8) / 8;
const SLENTRIES: usize = (BLOCK_MAX - 8) / 24;
const SIENTRIES: usize = (BLOCK_MAX - 8) / 16;

const PTYPE_BBT: u8 = 0x80;
const PTYPE_NBT: u8 = 0x81;
const PTYPE_FMAP: u8 = 0x82;
const PTYPE_PMAP: u8 = 0x83;
const PTYPE_AMAP: u8 = 0x84;
const PTYPE_FPMAP: u8 = 0x85;
const PTYPE_DLIST: u8 = 0x86;

/// Запись BBT в рабочем файле: BID, смещение, размер.
const BBT_REC: usize = 18;
/// Запись NBT в рабочем файле: NID, данные, поддерево, родитель.
const NBT_REC: usize = 24;

/// Узел в поддереве (SLENTRY): данные и своё поддерево (0 — нет).
#[derive(Debug, Clone, Copy)]
pub(super) struct Sub {
    pub nid: u32,
    pub data: u64,
    pub sub: u64,
}

#[derive(Clone, Copy)]
struct Node {
    nid: u32,
    data: u64,
    sub: u64,
    parent: u32,
}

/// Рабочий файл записей по возрастанию ключа: пишется подряд, читается с начала.
struct Spill {
    out: BufWriter<Box<dyn Storage>>,
    count: u64,
    last: u64,
}

impl Spill {
    fn new(file: Box<dyn Storage>) -> Self {
        Self { out: BufWriter::with_capacity(64 * 1024, file), count: 0, last: 0 }
    }

    fn push(&mut self, key: u64, rec: &[u8]) -> io::Result<()> {
        if self.count > 0 && key <= self.last {
            return Err(io::Error::other("PST: записи рабочего файла не по возрастанию"));
        }
        self.out.write_all(rec)?;
        self.count += 1;
        self.last = key;
        Ok(())
    }

    fn reader(self) -> io::Result<(BufReader<Box<dyn Storage>>, u64)> {
        let mut file = self.out.into_inner().map_err(io::IntoInnerError::into_error)?;
        file.seek(SeekFrom::Start(0))?;
        Ok((BufReader::with_capacity(64 * 1024, file), self.count))
    }

    fn empty() -> Self {
        Self::new(Box::new(io::Cursor::new(Vec::new())))
    }
}

pub(super) struct Ndb {
    out: BufWriter<Box<dyn Storage>>,
    /// Позиция `out`: всё до неё записано.
    pos: u64,
    /// Первый свободный байт за последним размещением.
    end: u64,
    next_bid: u64,
    next_page: u64,
    /// Текущая область AMap и её биты.
    region: u64,
    bits: [u8; 496],
    /// Свободно во всех закрытых областях.
    free: u64,
    /// FMap текущего окна (496 областей с `FMAP_FIRST`): самый длинный свободный участок области.
    fmap: [u8; 496],
    bbt: Spill,
    /// Блоки, номер которых занят раньше записи: в BBT — слиянием с рабочим файлом.
    late: BTreeMap<u64, (u64, u16)>,
    nodes: Vec<Node>,
    nbt: Spill,
    /// Узлы с NID не меньше — в рабочий файл (по возрастанию), меньше — в памяти.
    spill_from: u32,
    /// Блоков записано.
    pub(super) blocks: u64,
}

impl Ndb {
    /// Новый том `out`; рабочие файлы BBT и NBT — `bbt`, `nbt`. Узлы от `spill_from` (письма) идут по возрастанию NID.
    pub(super) fn new(out: Box<dyn Storage>, bbt: Box<dyn Storage>, nbt: Box<dyn Storage>, spill_from: u32) -> io::Result<Self> {
        let mut ndb = Self {
            out: BufWriter::with_capacity(256 * 1024, out),
            pos: 0,
            end: AMAP_FIRST,
            next_bid: 1,
            next_page: 1,
            region: 0,
            bits: [0; 496],
            free: 0,
            fmap: [0; 496],
            bbt: Spill::new(bbt),
            late: BTreeMap::new(),
            nodes: Vec::new(),
            nbt: Spill::new(nbt),
            spill_from,
            blocks: 0,
        };
        // Заголовок и DList — в конце, на их место пока нули.
        ndb.enter(0)?;
        ndb.end = ndb.pos;
        Ok(ndb)
    }

    /// Байт в томе сейчас (без деревьев и карт конца).
    pub(super) fn size(&self) -> u64 {
        self.end
    }

    /// Узлов в NBT.
    pub(super) fn node_count(&self) -> u64 {
        self.nodes.len() as u64 + self.nbt.count
    }

    /// Номер блока заранее: данные — позже ([`Ndb::write_block`]).
    pub(super) fn reserve(&mut self, internal: bool) -> u64 {
        let bid = self.next_bid << 2 | if internal { 2 } else { 0 };
        self.next_bid += 1;
        bid
    }

    /// Блок данных или служебный (`internal`: XBLOCK, SLBLOCK…) → BID.
    pub(super) fn block(&mut self, data: &[u8], internal: bool) -> io::Result<u64> {
        let bid = self.reserve(internal);
        self.write_block(bid, data)?;
        Ok(bid)
    }

    /// Блок с номером из [`Ndb::reserve`].
    pub(super) fn write_block(&mut self, bid: u64, data: &[u8]) -> io::Result<()> {
        if data.len() > BLOCK_MAX {
            return Err(io::Error::other("блок PST больше 8176 байт"));
        }
        let size = (data.len() + TRAILER).next_multiple_of(64);
        let ib = self.alloc(size as u64, 64)?;
        let mut b = Vec::with_capacity(size);
        b.extend_from_slice(data);
        b.resize(size - TRAILER, 0);
        b.extend((data.len() as u16).to_le_bytes());
        b.extend(sig(ib, bid).to_le_bytes());
        b.extend(crc(data).to_le_bytes());
        b.extend(bid.to_le_bytes());
        self.put(ib, &b)?;
        self.blocks += 1;
        let cb = data.len() as u16;
        if self.bbt.count == 0 || bid > self.bbt.last {
            let mut rec = [0u8; BBT_REC];
            rec[0..8].copy_from_slice(&bid.to_le_bytes());
            rec[8..16].copy_from_slice(&ib.to_le_bytes());
            rec[16..18].copy_from_slice(&cb.to_le_bytes());
            self.bbt.push(bid, &rec)
        } else {
            self.late.insert(bid, (ib, cb));
            Ok(())
        }
    }

    /// Данные узла: до 8176 байт — блок, больше — дерево XBLOCK кусками по 8176.
    pub(super) fn data(&mut self, bytes: &[u8]) -> io::Result<u64> {
        if bytes.len() <= BLOCK_MAX {
            return self.block(bytes, false);
        }
        let mut tree = DataTree::default();
        for c in bytes.chunks(BLOCK_MAX) {
            let bid = self.block(c, false)?;
            tree.push(self, bid, c.len() as u64)?;
        }
        tree.finish(self)
    }

    /// Данные узла кусками заданного размера (блоки кучи, строки таблицы): один — блок, больше — XBLOCK.
    pub(super) fn chunks(&mut self, chunks: &[Vec<u8>]) -> io::Result<u64> {
        if let [one] = chunks {
            return self.block(one, false);
        }
        let mut tree = DataTree::default();
        for c in chunks {
            let bid = self.block(c, false)?;
            tree.push(self, bid, c.len() as u64)?;
        }
        tree.finish(self)
    }

    /// XBLOCK (уровень 1) или XXBLOCK (2): btype 1, уровень, число записей, общий размер данных, BID.
    fn xblock(&mut self, level: u8, bids: &[u64], total: u64) -> io::Result<u64> {
        let total = u32::try_from(total).map_err(|_| io::Error::other("данные узла PST больше 4 ГиБ"))?;
        let mut b = vec![1, level];
        b.extend((bids.len() as u16).to_le_bytes());
        b.extend(total.to_le_bytes());
        for bid in bids {
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
    pub(super) fn node(&mut self, nid: u32, data: u64, sub: u64, parent: u32) -> io::Result<()> {
        if nid < self.spill_from {
            self.nodes.push(Node { nid, data, sub, parent });
            return Ok(());
        }
        let mut rec = [0u8; NBT_REC];
        rec[0..4].copy_from_slice(&nid.to_le_bytes());
        rec[4..12].copy_from_slice(&data.to_le_bytes());
        rec[12..20].copy_from_slice(&sub.to_le_bytes());
        rec[20..24].copy_from_slice(&parent.to_le_bytes());
        self.nbt.push(u64::from(nid), &rec)
    }

    /// Место под блок или страницу: выравнивание, в пределах одной AMap и мимо её служебных страниц.
    fn alloc(&mut self, size: u64, align: u64) -> io::Result<u64> {
        let mut ib = self.end.next_multiple_of(align);
        loop {
            let k = (ib - AMAP_FIRST) / AMAP_RANGE;
            if k != self.region {
                self.enter(k)?;
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
        let start = AMAP_FIRST + self.region * AMAP_RANGE;
        mark(&mut self.bits, ib - start, size);
        self.end = ib + size;
        Ok(ib)
    }

    /// Новая область `k` (следующая за текущей): закрыть прежнюю, записать служебные страницы новой — AMap (нули до
    /// конца области), PMap (устарела: все страницы «заняты»), место FMap, FPMap.
    fn enter(&mut self, k: u64) -> io::Result<()> {
        if k > 0 {
            self.close_region()?;
        }
        let start = AMAP_FIRST + k * AMAP_RANGE;
        self.pad(start)?;
        self.region = k;
        self.bits = [0; 496];
        mark(&mut self.bits, 0, reserved(k) * PAGE);
        self.put(start, &[0; PAGE as usize])?;
        let mut at = start + PAGE;
        if k.is_multiple_of(8) {
            self.put_page(at, &[0xFF; 496], PTYPE_PMAP, 0, at)?;
            at += PAGE;
        }
        if is_fmap(k) {
            self.put(at, &[0; PAGE as usize])?;
            at += PAGE;
        }
        if is_fpmap(k) {
            // FPMap устарела: везде «есть свободные». Где она в области без FMap, MS-PST не говорит: `outlook-pst`
            // (Microsoft) ищет её четвёртой страницей, по порядку служебных страниц она третья — пишем обе.
            for _ in 0..2 {
                self.put_page(at, &[0xFF; 496], PTYPE_FPMAP, 0, at)?;
                at += PAGE;
            }
        }
        Ok(())
    }

    /// Текущая область заполнена: её AMap — на место, свободное — в итог, самый длинный свободный участок — в FMap.
    fn close_region(&mut self) -> io::Result<()> {
        let k = self.region;
        let start = AMAP_FIRST + k * AMAP_RANGE;
        let bits = self.bits;
        self.free += bits.iter().map(|b| u64::from(b.count_zeros())).sum::<u64>() * 64;
        self.patch_page(start, &bits, PTYPE_AMAP, 0, start)?;
        if k >= FMAP_FIRST {
            let slot = (k - FMAP_FIRST) % FMAP_EVERY;
            self.fmap[slot as usize] = longest_free(&bits);
            if slot == FMAP_EVERY - 1 {
                self.write_fmap(k - slot)?;
            }
        }
        Ok(())
    }

    /// FMap окна, что начинается областью `first`: её третья страница.
    fn write_fmap(&mut self, first: u64) -> io::Result<()> {
        let at = AMAP_FIRST + first * AMAP_RANGE + 2 * PAGE;
        let f = self.fmap;
        self.patch_page(at, &f, PTYPE_FMAP, 0, at)?;
        self.fmap = [0; 496];
        Ok(())
    }

    /// Записать байты с `ib` (не раньше уже записанного); промежуток — нулями.
    fn put(&mut self, ib: u64, bytes: &[u8]) -> io::Result<()> {
        self.pad(ib)?;
        self.out.write_all(bytes)?;
        self.pos += bytes.len() as u64;
        Ok(())
    }

    fn pad(&mut self, to: u64) -> io::Result<()> {
        if to < self.pos {
            return Err(io::Error::other("PST: запись назад"));
        }
        const ZEROS: [u8; 4096] = [0; 4096];
        while self.pos < to {
            let n = (to - self.pos).min(ZEROS.len() as u64) as usize;
            self.out.write_all(&ZEROS[..n])?;
            self.pos += n as u64;
        }
        Ok(())
    }

    fn page_bytes(body: &[u8; 496], ptype: u8, wsig: u16, bid: u64) -> [u8; 512] {
        let mut p = [0u8; 512];
        p[..496].copy_from_slice(body);
        p[496] = ptype;
        p[497] = ptype;
        p[498..500].copy_from_slice(&wsig.to_le_bytes());
        p[500..504].copy_from_slice(&crc(body).to_le_bytes());
        p[504..512].copy_from_slice(&bid.to_le_bytes());
        p
    }

    fn put_page(&mut self, ib: u64, body: &[u8; 496], ptype: u8, wsig: u16, bid: u64) -> io::Result<()> {
        self.put(ib, &Self::page_bytes(body, ptype, wsig, bid))
    }

    /// Байты на уже записанное место (AMap, FMap, DList, заголовок) — и обратно в конец.
    fn patch(&mut self, ib: u64, bytes: &[u8]) -> io::Result<()> {
        self.out.seek(SeekFrom::Start(ib))?;
        self.out.write_all(bytes)?;
        self.out.seek(SeekFrom::Start(self.pos))?;
        Ok(())
    }

    fn patch_page(&mut self, ib: u64, body: &[u8; 496], ptype: u8, wsig: u16, bid: u64) -> io::Result<()> {
        self.patch(ib, &Self::page_bytes(body, ptype, wsig, bid))
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
        self.put_page(ib, &p, ptype, sig(ib, bid), bid)?;
        Ok((bid, ib))
    }

    /// Деревья, карты, DList и заголовок — том готов. `unique` — dwUnique, `nids` — последний занятый индекс NID по
    /// типам.
    pub(super) fn finish(mut self, unique: u32, nids: &[u32; 32]) -> io::Result<()> {
        // BBT: BREF, cb, cRef (2: запись BBT и тот, кто ссылается), выравнивание — слиянием рабочего файла и `late`.
        let (mut reader, mut left) = std::mem::replace(&mut self.bbt, Spill::empty()).reader()?;
        let mut late = std::mem::take(&mut self.late).into_iter().peekable();
        let mut bbt = Bulk::new(PTYPE_BBT, 24);
        let mut rec = [0u8; BBT_REC];
        let mut pending: Option<(u64, u64, u16)> = None;
        loop {
            if pending.is_none() && left > 0 {
                reader.read_exact(&mut rec)?;
                left -= 1;
                pending = Some((le64(&rec[0..8]), le64(&rec[8..16]), u16::from_le_bytes([rec[16], rec[17]])));
            }
            let from_late = match (pending, late.peek()) {
                (None, None) => break,
                (None, Some(_)) => true,
                (Some(_), None) => false,
                (Some((bid, ..)), Some((l, _))) => *l < bid,
            };
            let next = if from_late { late.next().map(|(bid, (ib, cb))| (bid, ib, cb)) } else { pending.take() };
            let Some((bid, ib, cb)) = next else { break };
            let mut e = [0u8; 24];
            e[0..8].copy_from_slice(&bid.to_le_bytes());
            e[8..16].copy_from_slice(&ib.to_le_bytes());
            e[16..18].copy_from_slice(&cb.to_le_bytes());
            e[18..20].copy_from_slice(&2u16.to_le_bytes());
            bbt.push(&mut self, 0, bid, &e)?;
        }
        drop(reader);
        let bbt_root = bbt.finish(&mut self)?;

        // NBT: nid, данные, поддерево, родитель — сначала узлы в памяти (NID меньше), затем рабочий файл.
        let mut nodes = std::mem::take(&mut self.nodes);
        nodes.sort_by_key(|n| n.nid);
        if nodes.windows(2).any(|w| w[0].nid == w[1].nid) {
            return Err(io::Error::other("PST: повтор NID"));
        }
        let mut nbt = Bulk::new(PTYPE_NBT, 32);
        let entry = |n: &Node| {
            let mut e = [0u8; 32];
            e[0..8].copy_from_slice(&u64::from(n.nid).to_le_bytes());
            e[8..16].copy_from_slice(&n.data.to_le_bytes());
            e[16..24].copy_from_slice(&n.sub.to_le_bytes());
            e[24..28].copy_from_slice(&n.parent.to_le_bytes());
            e
        };
        for n in &nodes {
            nbt.push(&mut self, 0, u64::from(n.nid), &entry(n))?;
        }
        let (mut reader, count) = std::mem::replace(&mut self.nbt, Spill::empty()).reader()?;
        let mut rec = [0u8; NBT_REC];
        for _ in 0..count {
            reader.read_exact(&mut rec)?;
            let n = Node { nid: le32(&rec[0..4]), data: le64(&rec[4..12]), sub: le64(&rec[12..20]), parent: le32(&rec[20..24]) };
            nbt.push(&mut self, 0, u64::from(n.nid), &entry(&n))?;
        }
        drop(reader);
        let nbt_root = nbt.finish(&mut self)?;

        // Файл — до конца последней области: её AMap описывает и свободный хвост.
        let last = self.region;
        let eof = AMAP_FIRST + (last + 1) * AMAP_RANGE;
        self.pad(eof)?;
        self.close_region()?;
        if last >= FMAP_FIRST && (last - FMAP_FIRST) % FMAP_EVERY != FMAP_EVERY - 1 {
            self.write_fmap(last - (last - FMAP_FIRST) % FMAP_EVERY)?;
        }

        // DList пуст (как в новом файле Outlook), его BID — следующий номер страницы.
        let next_page = self.next_page;
        self.patch_page(DLIST, &[0; 496], PTYPE_DLIST, sig(DLIST, next_page), next_page)?;

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
        let root = [eof, AMAP_FIRST + last * AMAP_RANGE, self.free, 0, nbt_root.0, nbt_root.1, bbt_root.0, bbt_root.1];
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
        self.patch(0, &h)?;
        self.out.flush()
    }
}

fn le64(b: &[u8]) -> u64 {
    b.iter().rev().fold(0, |v, &x| v << 8 | u64::from(x))
}

fn le32(b: &[u8]) -> u32 {
    b.iter().rev().fold(0, |v, &x| v << 8 | u32::from(x))
}

/// Дерево данных узла (XBLOCK, XXBLOCK) потоком: листья по одному, XBLOCK — по заполнении.
#[derive(Default)]
pub(super) struct DataTree {
    x: Vec<u64>,
    xlen: u64,
    xx: Vec<u64>,
    total: u64,
}

impl DataTree {
    pub(super) fn push(&mut self, ndb: &mut Ndb, bid: u64, len: u64) -> io::Result<()> {
        self.x.push(bid);
        self.xlen += len;
        self.total += len;
        if self.x.len() == XENTRIES {
            let b = ndb.xblock(1, &self.x, self.xlen)?;
            self.xx.push(b);
            self.x.clear();
            self.xlen = 0;
        }
        Ok(())
    }

    /// BID данных: лист, XBLOCK или XXBLOCK.
    pub(super) fn finish(mut self, ndb: &mut Ndb) -> io::Result<u64> {
        if self.xx.is_empty() {
            return match self.x[..] {
                [] => Err(io::Error::other("PST: пустые данные узла")),
                [one] => Ok(one),
                _ => ndb.xblock(1, &self.x, self.xlen),
            };
        }
        if !self.x.is_empty() {
            let b = ndb.xblock(1, &self.x, self.xlen)?;
            self.xx.push(b);
        }
        if self.xx.len() > XENTRIES {
            return Err(io::Error::other("данные узла PST больше 8 ГиБ"));
        }
        ndb.xblock(2, &self.xx, self.total)
    }
}

/// B-дерево страниц (NBT, BBT) из записей по возрастанию ключа — потоком: на уровень одна недописанная страница.
struct Bulk {
    ptype: u8,
    size: usize,
    levels: Vec<Level>,
}

#[derive(Default)]
struct Level {
    buf: Vec<u8>,
    count: usize,
    first: u64,
    /// Страниц уровня уже записано.
    written: u64,
}

impl Bulk {
    fn new(ptype: u8, size: usize) -> Self {
        Self { ptype, size, levels: vec![Level::default()] }
    }

    fn width(&self, level: usize) -> usize {
        if level == 0 { self.size } else { 24 }
    }

    fn push(&mut self, ndb: &mut Ndb, level: usize, key: u64, entry: &[u8]) -> io::Result<()> {
        if self.levels.len() <= level {
            self.levels.push(Level::default());
        }
        let per = 488 / self.width(level);
        let l = &mut self.levels[level];
        if l.count == 0 {
            l.first = key;
        }
        l.buf.extend_from_slice(entry);
        l.count += 1;
        if l.count == per {
            self.flush(ndb, level)?;
        }
        Ok(())
    }

    fn flush(&mut self, ndb: &mut Ndb, level: usize) -> io::Result<()> {
        let size = self.width(level);
        let l = &mut self.levels[level];
        let (buf, count, first) = (std::mem::take(&mut l.buf), l.count, l.first);
        l.count = 0;
        l.written += 1;
        let (bid, ib) = ndb.page(self.ptype, level as u8, &buf, count, size)?;
        let mut e = [0u8; 24];
        e[0..8].copy_from_slice(&first.to_le_bytes());
        e[8..16].copy_from_slice(&bid.to_le_bytes());
        e[16..24].copy_from_slice(&ib.to_le_bytes());
        self.push(ndb, level + 1, first, &e)
    }

    /// BREF корня: верхний уровень без записанных страниц с одной записью — она и есть корень.
    fn finish(mut self, ndb: &mut Ndb) -> io::Result<(u64, u64)> {
        let mut level = 0;
        loop {
            let top = level + 1 == self.levels.len();
            let l = &self.levels[level];
            if top && l.written == 0 && level > 0 && l.count == 1 {
                return Ok((le64(&l.buf[8..16]), le64(&l.buf[16..24])));
            }
            // Пустое дерево — одна пустая страница-лист.
            if l.count > 0 || (level == 0 && l.written == 0) {
                self.flush(ndb, level)?;
            }
            level += 1;
        }
    }
}

/// Служебных страниц в начале области `k`: AMap, PMap (каждая восьмая), FMap, FPMap (две, см. [`Ndb::enter`]).
fn reserved(k: u64) -> u64 {
    1 + u64::from(k.is_multiple_of(8)) + u64::from(is_fmap(k)) + 2 * u64::from(is_fpmap(k))
}

fn is_fmap(k: u64) -> bool {
    k >= FMAP_FIRST && (k - FMAP_FIRST).is_multiple_of(FMAP_EVERY)
}

fn is_fpmap(k: u64) -> bool {
    k >= FPMAP_FIRST && (k - FPMAP_FIRST).is_multiple_of(FPMAP_EVERY)
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

    fn mem() -> Box<dyn Storage> {
        Box::new(io::Cursor::new(Vec::new()))
    }

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
        let mut ndb = Ndb::new(mem(), mem(), mem(), u32::MAX).unwrap();
        // Первые две страницы — AMap и PMap.
        assert_eq!(ndb.alloc(64, 64).unwrap(), 0x4800);
        // Блок не пересекает границу области AMap: следующая начинается её страницей.
        ndb.pad(AMAP_FIRST + AMAP_RANGE - 64).unwrap();
        ndb.end = AMAP_FIRST + AMAP_RANGE - 64;
        assert_eq!(ndb.alloc(128, 64).unwrap(), AMAP_FIRST + AMAP_RANGE + PAGE);
        assert_eq!(reserved(0), 2);
        assert_eq!(reserved(1), 1);
        assert_eq!(reserved(128), 3);
        assert_eq!(reserved(624), 3);
        // FPMap — с 8192-й области (2 ГиБ), дальше через 8 ГиБ; FMap там не бывает.
        assert_eq!(reserved(8192), 4);
        assert_eq!(reserved(8192 + FPMAP_EVERY), 4);
        assert!((0..100).all(|n| !is_fmap(FPMAP_FIRST + n * FPMAP_EVERY)));
    }

    #[test]
    fn page_trees_any_size() {
        // B-дерево страниц потоком: 0, 1, ровно страница, два уровня, три уровня.
        for n in [0u64, 1, 15, 16, 300, 20 * 15 * 2 + 7] {
            let mut ndb = Ndb::new(mem(), mem(), mem(), u32::MAX).unwrap();
            let mut b = Bulk::new(PTYPE_NBT, 32);
            for k in 0..n {
                b.push(&mut ndb, 0, k + 1, &[0; 32]).unwrap();
            }
            let (bid, _) = b.finish(&mut ndb).unwrap();
            assert!(bid >= 1);
            let pages = ndb.next_page - 1;
            let leaves = n.div_ceil(15).max(1);
            let expect = leaves + if leaves > 1 { leaves.div_ceil(20) + u64::from(leaves > 20) } else { 0 };
            assert_eq!(pages, expect, "{n} записей");
        }
    }
}
