//! LTP (MS-PST 2.3): куча на узле (HN), B-дерево на куче (BTH), контексты свойств (PC) и таблиц (TC). Значения
//! больше 3580 байт — в подузлах (`NID_TYPE_LTP`), строки таблицы не помещаются в кучу — тоже.
//!
//! Данные узла больше блока — дерево XBLOCK, и каждый его блок, кроме последнего, — полный (8176 байт): так у Outlook,
//! scanpst иначе пишет «Middle page not full» и выбрасывает таблицу. Страница кучи после HNPAGEMAP и блок строк
//! таблицы после последней целой строки добиваются нулями («мёртвое место», читатели его не смотрят).
//!
//! Контексты письма, вложения и малых таблиц собираются в памяти ([`pc`], [`tc`]). Таблица содержимого папки —
//! потоком ([`TcStream`]): строки по возрастанию `LtpRowId` (NID писем), блоки кучи и строк уходят в файл по мере
//! заполнения; в памяти — первый блок кучи (в нём заголовки, они известны в конце), блок карты заполнения (до 128
//! следующих блоков) и по одному недописанному узлу на уровень BTH.

use std::io;

use super::ndb::{BLOCK_MAX, DataTree, Ndb, Sub};
use crate::mapi::{PT_BINARY, PT_BOOLEAN, PT_LONG, PT_SYSTIME, PT_UNICODE, Tag, Value};

/// Наибольшее размещение в куче; больше — в подузел.
pub(super) const ALLOC_MAX: usize = 3580;
/// Размещений в одном блоке кучи — не больше (11 бит индекса HID).
const ALLOCS_MAX: usize = 2047;
const CLIENT_TC: u8 = 0x7C;
const CLIENT_PC: u8 = 0xBC;
/// Клиент кучи HMP (`NID_HST_HMP`); в MS-PST — `bTypeReserved3`.
const CLIENT_HMP: u8 = 0x9C;
const TYPE_BTH: u8 = 0xB5;
pub(super) const NID_TYPE_LTP: u32 = 0x1F;

pub(super) const PR_LTP_ROW_ID: Tag = 0x67F2_0003;
pub(super) const PR_LTP_ROW_VER: Tag = 0x67F3_0003;
const PT_I8: u16 = 0x0014;

/// Свойство-объект (`PtypObject`, вложенное письмо): тег, NID подузла с объектом, размер объекта.
pub(super) type Object = (Tag, u32, u32);

/// Подузлы, которые набирает узел (значения, строки таблиц), счётчик их NID и объём их данных. Без NDB (`ndb` —
/// `None`) — черновик: номера и объём те же, данные не пишутся.
pub(super) struct Subs<'a> {
    pub list: Vec<Sub>,
    pub next: &'a mut u32,
    /// Байт данных в подузлах — для `PR_MESSAGE_SIZE`.
    pub bytes: u64,
}

impl<'a> Subs<'a> {
    pub fn new(next: &'a mut u32) -> Self {
        Self { list: Vec::new(), next, bytes: 0 }
    }

    /// Данные в подузел `NID_TYPE_LTP` → его NID (HNID значения).
    fn put(&mut self, ndb: Option<&mut Ndb>, bytes: &[u8]) -> io::Result<u32> {
        *self.next += 1;
        let nid = *self.next << 5 | NID_TYPE_LTP;
        self.bytes += bytes.len() as u64;
        if let Some(ndb) = ndb {
            let data = ndb.data(bytes)?;
            self.list.push(Sub { nid, data, sub: 0 });
        }
        Ok(nid)
    }
}

fn header_len(block: usize) -> usize {
    match block {
        0 => 12,
        b if b % 128 == 8 => 66,
        _ => 2,
    }
}

fn block_len(block: usize, allocs: &[Vec<u8>]) -> usize {
    let used: usize = allocs.iter().map(Vec::len).sum();
    (header_len(block) + used).next_multiple_of(2) + 4 + 2 * (allocs.len() + 1)
}

fn fits(block: usize, allocs: &[Vec<u8>], len: usize) -> bool {
    allocs.len() < ALLOCS_MAX && block_len(block, allocs) + len + 2 <= BLOCK_MAX
}

/// Блок кучи `i`: заголовок (HNHDR, HNPAGEHDR или HNBITMAPHDR с уровнями заполнения `fills`), размещения, карта
/// страницы (HNPAGEMAP); не последний блок (`full`) — нулями до 8176 байт.
fn render(i: usize, allocs: &[Vec<u8>], fills: &[u8], client: u8, root: u32, full: bool) -> Vec<u8> {
    let packed = |from: usize, n: usize| -> Vec<u8> {
        (0..n / 2).map(|j| fills.get(from + 2 * j).copied().unwrap_or(0) | fills.get(from + 2 * j + 1).copied().unwrap_or(0) << 4).collect()
    };
    let mut b = vec![0; 2];
    match i {
        0 => {
            b.extend([0xEC, client]);
            b.extend(root.to_le_bytes());
            b.extend(packed(0, 8));
        }
        _ if i % 128 == 8 => b.extend(packed(i, 128)),
        _ => {}
    }
    let mut offsets = vec![b.len() as u16];
    for a in allocs {
        b.extend(a);
        offsets.push(b.len() as u16);
    }
    if b.len() % 2 == 1 {
        b.push(0);
    }
    let map = b.len() as u16;
    b[0..2].copy_from_slice(&map.to_le_bytes());
    b.extend((allocs.len() as u16).to_le_bytes());
    b.extend(0u16.to_le_bytes());
    for o in offsets {
        b.extend(o.to_le_bytes());
    }
    if full {
        b.resize(BLOCK_MAX, 0);
    }
    b
}

/// Строки таблицы в блоки подузла: целыми строками, все блоки, кроме последнего, — по 8176 байт (MS-PST 2.3.4.4).
fn row_blocks(matrix: &[u8], size: usize) -> Vec<Vec<u8>> {
    let per = (BLOCK_MAX / size).max(1) * size;
    let mut blocks: Vec<Vec<u8>> = matrix.chunks(per).map(<[u8]>::to_vec).collect();
    let n = blocks.len();
    for b in blocks.iter_mut().take(n.saturating_sub(1)) {
        b.resize(BLOCK_MAX, 0);
    }
    blocks
}

/// Куча на узле в памяти: блоки размещений; HID — (номер блока, номер размещения с 1).
struct Heap {
    client: u8,
    root: u32,
    blocks: Vec<Vec<Vec<u8>>>,
}

impl Heap {
    fn new(client: u8) -> Self {
        Self { client, root: 0, blocks: vec![Vec::new()] }
    }

    fn alloc(&mut self, data: Vec<u8>) -> io::Result<u32> {
        if data.len() > ALLOC_MAX {
            return Err(io::Error::other("размещение кучи PST больше 3580 байт"));
        }
        let last = self.blocks.len() - 1;
        if !fits(last, &self.blocks[last], data.len()) {
            self.blocks.push(Vec::new());
        }
        let b = self.blocks.len() - 1;
        if b > 0xFFFF {
            return Err(io::Error::other("куча PST больше 65536 блоков"));
        }
        self.blocks[b].push(data);
        Ok((b as u32) << 16 | (self.blocks[b].len() as u32) << 5)
    }

    /// Заменить размещение (того же размера): заголовки BTH и TCINFO занимаются первыми, заполняются в конце.
    fn set(&mut self, hid: u32, data: Vec<u8>) {
        let slot = &mut self.blocks[(hid >> 16) as usize][((hid >> 5) & 0x7FF) as usize - 1];
        debug_assert_eq!(slot.len(), data.len());
        *slot = data;
    }

    fn into_blocks(self) -> Vec<Vec<u8>> {
        let fills: Vec<u8> = self.blocks.iter().enumerate().map(|(i, a)| fill_level(BLOCK_MAX - block_len(i, a))).collect();
        let last = self.blocks.len() - 1;
        self.blocks.iter().enumerate().map(|(i, a)| render(i, a, &fills, self.client, self.root, i < last)).collect()
    }
}

/// Куча таблицы потоком: закрытые блоки — в файл (номер блока занимается сразу, в дереве данных — по порядку), кроме
/// первого (заголовки — в конце) и блоков карты заполнения (ждут 127 следующих).
struct HeapStream {
    client: u8,
    index: usize,
    cur: Vec<Vec<u8>>,
    fills: Vec<u8>,
    /// Отложенные блоки (номер, BID, размещения, полный ли).
    block0: Option<(u64, Vec<Vec<u8>>, bool)>,
    bitmap: Option<(usize, u64, Vec<Vec<u8>>, bool)>,
    tree: DataTree,
}

impl HeapStream {
    fn new(client: u8) -> Self {
        Self { client, index: 0, cur: Vec::new(), fills: Vec::new(), block0: None, bitmap: None, tree: DataTree::default() }
    }

    fn alloc(&mut self, ndb: &mut Ndb, data: Vec<u8>) -> io::Result<u32> {
        if data.len() > ALLOC_MAX {
            return Err(io::Error::other("размещение кучи PST больше 3580 байт"));
        }
        if !fits(self.index, &self.cur, data.len()) {
            self.close(ndb, false)?;
        }
        if self.index > 0xFFFF {
            return Err(io::Error::other("куча PST больше 65536 блоков (слишком много писем в папке)"));
        }
        self.cur.push(data);
        Ok((self.index as u32) << 16 | (self.cur.len() as u32) << 5)
    }

    /// Заменить размещение первого блока (того же размера).
    fn set(&mut self, hid: u32, data: Vec<u8>) {
        let i = ((hid >> 5) & 0x7FF) as usize - 1;
        let allocs = match (&mut self.block0, hid >> 16) {
            (Some((_, a, _)), 0) => a,
            _ => &mut self.cur,
        };
        debug_assert_eq!(allocs[i].len(), data.len());
        allocs[i] = data;
    }

    /// Закрыть текущий блок; не последний — полный (8176 байт).
    fn close(&mut self, ndb: &mut Ndb, last: bool) -> io::Result<()> {
        let i = self.index;
        let allocs = std::mem::take(&mut self.cur);
        let used = block_len(i, &allocs);
        self.fills.push(fill_level(BLOCK_MAX - used));
        let len = if last { used } else { BLOCK_MAX };
        let bid = ndb.reserve(false);
        if i == 0 {
            self.block0 = Some((bid, allocs, !last));
        } else if i % 128 == 8 {
            self.bitmap = Some((i, bid, allocs, !last));
        } else {
            ndb.write_block(bid, &render(i, &allocs, &self.fills, self.client, 0, !last))?;
        }
        if let Some((b, ..)) = self.bitmap
            && i == b + 127
        {
            self.write_bitmap(ndb)?;
        }
        self.tree.push(ndb, bid, len as u64)?;
        self.index += 1;
        Ok(())
    }

    fn write_bitmap(&mut self, ndb: &mut Ndb) -> io::Result<()> {
        if let Some((b, bid, allocs, full)) = self.bitmap.take() {
            ndb.write_block(bid, &render(b, &allocs, &self.fills, self.client, 0, full))?;
        }
        Ok(())
    }

    /// Куча готова: корень `root` → BID данных узла.
    fn finish(mut self, ndb: &mut Ndb, root: u32) -> io::Result<u64> {
        if !self.cur.is_empty() || self.index == 0 {
            self.close(ndb, true)?;
        }
        self.write_bitmap(ndb)?;
        if let Some((bid, allocs, full)) = self.block0.take() {
            ndb.write_block(bid, &render(0, &allocs, &self.fills, self.client, root, full))?;
        }
        self.tree.finish(ndb)
    }
}

/// Уровень заполнения блока кучи по свободному месту (MS-PST 2.3.1.1).
fn fill_level(free: usize) -> u8 {
    const STEPS: [usize; 15] = [3584, 2560, 2048, 1792, 1536, 1280, 1024, 768, 512, 256, 128, 64, 32, 16, 8];
    STEPS.iter().position(|&s| free >= s).unwrap_or(15) as u8
}

/// BTH из записей по возрастанию ключа — потоком: на уровень один недописанный узел (до 3580 байт).
struct Bth {
    key: usize,
    value: usize,
    levels: Vec<BthLevel>,
}

#[derive(Default)]
struct BthLevel {
    buf: Vec<u8>,
    count: usize,
    first: Vec<u8>,
    written: usize,
}

impl Bth {
    fn new(key: u8, value: u8) -> Self {
        Self { key: usize::from(key), value: usize::from(value), levels: vec![BthLevel::default()] }
    }

    fn per(&self, level: usize) -> usize {
        ALLOC_MAX / (self.key + if level == 0 { self.value } else { 4 })
    }

    fn push(&mut self, alloc: &mut dyn FnMut(Vec<u8>) -> io::Result<u32>, level: usize, key: &[u8], value: &[u8]) -> io::Result<()> {
        if self.levels.len() <= level {
            self.levels.push(BthLevel::default());
        }
        let per = self.per(level);
        let l = &mut self.levels[level];
        if l.count == 0 {
            l.first = key.to_vec();
        }
        l.buf.extend_from_slice(key);
        l.buf.extend_from_slice(value);
        l.count += 1;
        if l.count == per {
            self.flush(alloc, level)?;
        }
        Ok(())
    }

    fn flush(&mut self, alloc: &mut dyn FnMut(Vec<u8>) -> io::Result<u32>, level: usize) -> io::Result<()> {
        let l = &mut self.levels[level];
        let (buf, first) = (std::mem::take(&mut l.buf), std::mem::take(&mut l.first));
        l.count = 0;
        l.written += 1;
        let hid = alloc(buf)?;
        self.push(alloc, level + 1, &first, &hid.to_le_bytes())
    }

    /// Заголовок BTH (8 байт): тип, размеры ключа и значения, уровней индекса, корень (пустое дерево — 0).
    fn finish(mut self, alloc: &mut dyn FnMut(Vec<u8>) -> io::Result<u32>) -> io::Result<Vec<u8>> {
        let (root, levels) = if self.levels[0].count == 0 && self.levels[0].written == 0 {
            (0, 0)
        } else {
            let mut level = 0;
            loop {
                let top = level + 1 == self.levels.len();
                let l = &self.levels[level];
                if top && l.written == 0 && l.count == 1 && level > 0 {
                    let hid = u32::from_le_bytes([l.buf[self.key], l.buf[self.key + 1], l.buf[self.key + 2], l.buf[self.key + 3]]);
                    break (hid, level as u8 - 1);
                }
                if l.count > 0 {
                    self.flush(alloc, level)?;
                }
                level += 1;
            }
        };
        let mut h = vec![TYPE_BTH, self.key as u8, self.value as u8, levels];
        h.extend(root.to_le_bytes());
        Ok(h)
    }
}

/// BTH над упорядоченными записями в куче в памяти, заголовок — в заранее занятое размещение `header`.
fn bth(heap: &mut Heap, header: u32, key: u8, value: u8, records: &[(Vec<u8>, Vec<u8>)]) -> io::Result<()> {
    let mut b = Bth::new(key, value);
    let mut alloc = |d: Vec<u8>| heap.alloc(d);
    for (k, v) in records {
        b.push(&mut alloc, 0, k, v)?;
    }
    let h = b.finish(&mut alloc)?;
    heap.set(header, h);
    Ok(())
}

/// Значение переменной длины → HNID: пустое — 0, до 3580 байт — в куче, больше — подузел.
fn hnid(heap: &mut Heap, ndb: Option<&mut Ndb>, subs: &mut Subs, bytes: Vec<u8>) -> io::Result<u32> {
    if bytes.is_empty() {
        Ok(0)
    } else if bytes.len() <= ALLOC_MAX {
        heap.alloc(bytes)
    } else {
        subs.put(ndb, &bytes)
    }
}

pub(super) fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// Байты значения переменной длины (строка — UTF-16LE).
fn var_bytes(v: &Value) -> Option<Vec<u8>> {
    match v {
        Value::Unicode(s) => Some(utf16(s)),
        Value::Binary(b) => Some(b.clone()),
        _ => None,
    }
}

/// Контекст свойств → блоки кучи узла. Тип значения должен совпадать с тегом, повтор свойства — первое; объекты
/// (`PtypObject`) — в куче структурой (NID подузла, размер). `ndb` — `None`: черновик (размер без записи).
pub(super) fn pc(mut ndb: Option<&mut Ndb>, subs: &mut Subs, props: &[(Tag, &Value)], objects: &[Object]) -> io::Result<Vec<Vec<u8>>> {
    let mut heap = Heap::new(CLIENT_PC);
    heap.root = heap.alloc(vec![0; 8])?;
    enum Item<'a> {
        Value(&'a Value),
        Object(u32, u32),
    }
    let mut sorted: Vec<(Tag, Item)> = props.iter().filter(|(t, v)| *t as u16 == v.prop_type()).map(|(t, v)| (*t, Item::Value(v))).collect();
    sorted.extend(objects.iter().map(|&(t, nid, size)| (t, Item::Object(nid, size))));
    sorted.sort_by_key(|(t, _)| *t >> 16);
    sorted.dedup_by_key(|(t, _)| *t >> 16);
    let mut records = Vec::with_capacity(sorted.len());
    for (tag, item) in sorted {
        let value = match item {
            Item::Value(Value::Long(n)) => *n as u32,
            Item::Value(Value::Boolean(b)) => u32::from(*b),
            Item::Value(Value::SysTime(t)) => heap.alloc(t.to_le_bytes().to_vec())?,
            Item::Value(Value::Unicode(s)) => hnid(&mut heap, ndb.as_deref_mut(), subs, utf16(s))?,
            Item::Value(Value::Binary(b)) => hnid(&mut heap, ndb.as_deref_mut(), subs, b.clone())?,
            Item::Object(nid, size) => heap.alloc([nid.to_le_bytes(), size.to_le_bytes()].concat())?,
        };
        let mut rec = (tag as u16).to_le_bytes().to_vec();
        rec.extend(value.to_le_bytes());
        records.push((((tag >> 16) as u16).to_le_bytes().to_vec(), rec));
    }
    let root = heap.root;
    bth(&mut heap, root, 2, 6, &records)?;
    Ok(heap.into_blocks())
}

/// HMP (`NID_HST_HMP`, не описан в MS-PST) — как в новом файле Outlook: корень кучи — HID пустого BTH с ключами по
/// 16 байт и значениями по 4. Без него scanpst пишет «HMP not found - creating».
pub(super) fn hmp() -> io::Result<Vec<Vec<u8>>> {
    let mut heap = Heap::new(CLIENT_HMP);
    let root = heap.alloc(vec![0; 4])?;
    let header = heap.alloc(vec![0; 8])?;
    bth(&mut heap, header, 16, 4, &[])?;
    heap.set(root, header.to_le_bytes().to_vec());
    heap.root = root;
    Ok(heap.into_blocks())
}

/// Строка таблицы: идентификатор (`LtpRowId`), версия (`LtpRowVer`), значения по тегам.
pub(super) struct Row {
    pub id: u32,
    pub ver: u32,
    pub values: Vec<(Tag, Value)>,
}

struct Column {
    tag: Tag,
    ib: u16,
    cb: u8,
    bit: u8,
}

fn width(tag: Tag) -> u8 {
    match tag as u16 {
        // Время, целое 8 байт, дробное, деньги, время OLE.
        PT_SYSTIME | PT_I8 | 0x0005 | 0x0006 | 0x0007 => 8,
        0x0002 => 2,
        PT_BOOLEAN => 1,
        _ => 4,
    }
}

/// Раскладка строки (MS-PST 2.3.4.4): `LtpRowId`, `LtpRowVer`, затем 8- и 4-байтовые столбцы, 2-байтовые, 1-байтовые,
/// карта существования (CEB). Биты CEB — в порядке столбцов. Возвращает столбцы и rgib.
fn layout(tags: &[Tag]) -> (Vec<Column>, [u16; 4]) {
    let all: Vec<Tag> = [PR_LTP_ROW_ID, PR_LTP_ROW_VER].into_iter().chain(tags.iter().copied()).collect();
    let mut cols: Vec<Column> = all.iter().enumerate().map(|(i, &tag)| Column { tag, ib: 0, cb: width(tag), bit: i as u8 }).collect();
    let mut ib = 0u16;
    let mut ends = [0u16; 3];
    for (group, sizes) in [&[8u8, 4][..], &[2], &[1]].iter().enumerate() {
        for c in cols.iter_mut().filter(|c| sizes.contains(&c.cb)) {
            c.ib = ib;
            ib += u16::from(c.cb);
        }
        ends[group] = ib;
    }
    let bm = ends[2] + all.len().div_ceil(8) as u16;
    cols.sort_by_key(|c| c.tag);
    (cols, [ends[0], ends[1], ends[2], bm])
}

/// Строка в байтах: значения переменной длины — через `var` (HNID).
fn row_bytes(
    cols: &[Column],
    rgib: [u16; 4],
    id: u32,
    ver: u32,
    values: &[(Tag, &Value)],
    var: &mut dyn FnMut(Vec<u8>) -> io::Result<u32>,
) -> io::Result<Vec<u8>> {
    let mut r = vec![0u8; usize::from(rgib[3])];
    r[0..4].copy_from_slice(&id.to_le_bytes());
    r[4..8].copy_from_slice(&ver.to_le_bytes());
    let ceb = usize::from(rgib[2]);
    for c in cols {
        let at = usize::from(c.ib);
        let cell: Option<Vec<u8>> = match c.tag {
            PR_LTP_ROW_ID | PR_LTP_ROW_VER => Some(r[at..at + 4].to_vec()),
            _ => match values.iter().find(|(t, _)| *t == c.tag).map(|(_, v)| *v) {
                Some(Value::Long(n)) if c.tag as u16 == PT_LONG => Some(n.to_le_bytes().to_vec()),
                Some(Value::Boolean(b)) if c.tag as u16 == PT_BOOLEAN => Some(vec![u8::from(*b)]),
                Some(Value::SysTime(t)) if c.tag as u16 == PT_SYSTIME => Some(t.to_le_bytes().to_vec()),
                Some(v @ Value::Unicode(_)) if c.tag as u16 == PT_UNICODE => Some(var_cell(v, var)?),
                Some(v @ Value::Binary(_)) if c.tag as u16 == PT_BINARY => Some(var_cell(v, var)?),
                _ => None,
            },
        };
        if let Some(cell) = cell {
            r[at..at + cell.len()].copy_from_slice(&cell);
            r[ceb + usize::from(c.bit / 8)] |= 0x80 >> (c.bit % 8);
        }
    }
    Ok(r)
}

fn var_cell(v: &Value, var: &mut dyn FnMut(Vec<u8>) -> io::Result<u32>) -> io::Result<Vec<u8>> {
    let bytes = var_bytes(v).unwrap_or_default();
    let h = if bytes.is_empty() { 0 } else { var(bytes)? };
    Ok(h.to_le_bytes().to_vec())
}

/// TCINFO: столбцы, rgib, индекс строк, строки.
fn tcinfo(cols: &[Column], rgib: [u16; 4], index: u32, rows: u32) -> Vec<u8> {
    let mut t = vec![CLIENT_TC, cols.len() as u8];
    for v in rgib {
        t.extend(v.to_le_bytes());
    }
    t.extend(index.to_le_bytes());
    t.extend(rows.to_le_bytes());
    t.extend(0u32.to_le_bytes()); // hidIndex устарел
    for c in cols {
        t.extend(c.tag.to_le_bytes());
        t.extend(c.ib.to_le_bytes());
        t.extend([c.cb, c.bit]);
    }
    t
}

/// Контекст таблицы в памяти → блоки кучи узла. Столбцы — `LtpRowId`, `LtpRowVer` и `tags` (порядок задаёт
/// раскладку); значения строк других типов или вне столбцов пропускаются.
pub(super) fn tc(ndb: &mut Ndb, subs: &mut Subs, tags: &[Tag], rows: &[Row]) -> io::Result<Vec<Vec<u8>>> {
    let (cols, rgib) = layout(tags);
    let mut heap = Heap::new(CLIENT_TC);
    let index = heap.alloc(vec![0; 8])?;
    let info = heap.alloc(vec![0; 22 + 8 * cols.len()])?;

    let size = usize::from(rgib[3]);
    let mut matrix = Vec::with_capacity(rows.len() * size);
    for row in rows {
        let values: Vec<(Tag, &Value)> = row.values.iter().map(|(t, v)| (*t, v)).collect();
        let mut var = |b: Vec<u8>| hnid(&mut heap, Some(&mut *ndb), subs, b);
        matrix.extend(row_bytes(&cols, rgib, row.id, row.ver, &values, &mut var)?);
    }

    let mut order: Vec<(u32, u32)> = rows.iter().enumerate().map(|(i, r)| (r.id, i as u32)).collect();
    order.sort();
    let ids: Vec<(Vec<u8>, Vec<u8>)> = order.iter().map(|(id, i)| (id.to_le_bytes().to_vec(), i.to_le_bytes().to_vec())).collect();
    bth(&mut heap, index, 4, 4, &ids)?;

    // Строки: в куче, если помещаются одним размещением, иначе — подузлом, целыми строками в каждом блоке.
    let rows_hnid = if matrix.is_empty() {
        0
    } else if matrix.len() <= ALLOC_MAX {
        heap.alloc(matrix)?
    } else {
        *subs.next += 1;
        let nid = *subs.next << 5 | NID_TYPE_LTP;
        let chunks = row_blocks(&matrix, size);
        let data = ndb.chunks(&chunks)?;
        subs.bytes += chunks.iter().map(|c| c.len() as u64).sum::<u64>();
        subs.list.push(Sub { nid, data, sub: 0 });
        nid
    };

    heap.set(info, tcinfo(&cols, rgib, index, rows_hnid));
    heap.root = info;
    Ok(heap.into_blocks())
}

/// Таблица потоком (содержимое папки): строки — по возрастанию `LtpRowId`.
pub(super) struct TcStream {
    cols: Vec<Column>,
    rgib: [u16; 4],
    heap: HeapStream,
    index: u32,
    info: u32,
    bth: Bth,
    count: u32,
    last: Option<u32>,
    /// Строки, ещё не ушедшие в блок (или все, пока помещаются в кучу).
    matrix: Vec<u8>,
    /// Строки — подузлом: блоки по целым строкам.
    rows: Option<DataTree>,
    subs: Vec<Sub>,
}

impl TcStream {
    pub(super) fn new(ndb: &mut Ndb, tags: &[Tag]) -> io::Result<Self> {
        let (cols, rgib) = layout(tags);
        let mut heap = HeapStream::new(CLIENT_TC);
        let index = heap.alloc(ndb, vec![0; 8])?;
        let info = heap.alloc(ndb, vec![0; 22 + 8 * cols.len()])?;
        Ok(Self { cols, rgib, heap, index, info, bth: Bth::new(4, 4), count: 0, last: None, matrix: Vec::new(), rows: None, subs: Vec::new() })
    }

    /// Строка: `id` больше прежних; значения вне столбцов пропускаются. Длинные значения — подузлами (`ltp` — счётчик).
    pub(super) fn add(&mut self, ndb: &mut Ndb, ltp: &mut u32, id: u32, ver: u32, values: &[(Tag, &Value)]) -> io::Result<()> {
        if self.last.is_some_and(|l| id <= l) {
            return Err(io::Error::other("PST: строки таблицы не по возрастанию"));
        }
        self.last = Some(id);
        let (heap, subs) = (&mut self.heap, &mut self.subs);
        let mut var = |bytes: Vec<u8>| -> io::Result<u32> {
            if bytes.len() <= ALLOC_MAX {
                heap.alloc(ndb, bytes)
            } else {
                *ltp += 1;
                let nid = *ltp << 5 | NID_TYPE_LTP;
                let data = ndb.data(&bytes)?;
                subs.push(Sub { nid, data, sub: 0 });
                Ok(nid)
            }
        };
        let row = row_bytes(&self.cols, self.rgib, id, ver, values, &mut var)?;
        let n = self.count;
        let heap = &mut self.heap;
        self.bth.push(&mut |d| heap.alloc(ndb, d), 0, &id.to_le_bytes(), &n.to_le_bytes())?;
        self.count += 1;
        self.matrix.extend(row);
        if self.rows.is_none() && self.matrix.len() > ALLOC_MAX {
            self.rows = Some(DataTree::default());
        }
        let size = usize::from(self.rgib[3]);
        let per = (BLOCK_MAX / size).max(1) * size;
        if let Some(tree) = self.rows.as_mut() {
            // Блок строк — полный: после целых строк нули до 8176 байт (последний блок — в `finish`, без них).
            while self.matrix.len() >= per {
                let mut block = self.matrix[..per].to_vec();
                block.resize(BLOCK_MAX, 0);
                let bid = ndb.block(&block, false)?;
                tree.push(ndb, bid, BLOCK_MAX as u64)?;
                self.matrix.drain(..per);
            }
        }
        Ok(())
    }

    /// Таблица готова → (данные узла, поддерево: длинные значения и строки; 0 — нет).
    pub(super) fn finish(mut self, ndb: &mut Ndb, ltp: &mut u32) -> io::Result<(u64, u64)> {
        let heap = &mut self.heap;
        let header = self.bth.finish(&mut |d| heap.alloc(ndb, d))?;
        heap.set(self.index, header);
        let rows_hnid = match self.rows.take() {
            None if self.matrix.is_empty() => 0,
            None => self.heap.alloc(ndb, std::mem::take(&mut self.matrix))?,
            Some(mut tree) => {
                if !self.matrix.is_empty() {
                    let bid = ndb.block(&self.matrix, false)?;
                    tree.push(ndb, bid, self.matrix.len() as u64)?;
                }
                *ltp += 1;
                let nid = *ltp << 5 | NID_TYPE_LTP;
                let data = tree.finish(ndb)?;
                self.subs.push(Sub { nid, data, sub: 0 });
                nid
            }
        };
        self.heap.set(self.info, tcinfo(&self.cols, self.rgib, self.index, rows_hnid));
        let data = self.heap.finish(ndb, self.info)?;
        let sub = if self.subs.is_empty() { 0 } else { ndb.subnodes(self.subs)? };
        Ok((data, sub))
    }
}
