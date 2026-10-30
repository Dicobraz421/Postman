//! LTP (MS-PST 2.3): куча на узле (HN), B-дерево на куче (BTH), контексты свойств (PC) и таблиц (TC). Значения
//! больше 3580 байт — в подузлах (`NID_TYPE_LTP`), строки таблицы не помещаются в кучу — тоже.

use std::io;

use super::ndb::{BLOCK_MAX, Ndb, Sub};
use crate::mapi::{PT_BINARY, PT_BOOLEAN, PT_LONG, PT_SYSTIME, PT_UNICODE, Tag, Value};

/// Наибольшее размещение в куче; больше — в подузел.
const ALLOC_MAX: usize = 3580;
/// Размещений в одном блоке кучи — не больше (11 бит индекса HID).
const ALLOCS_MAX: usize = 2047;
const CLIENT_TC: u8 = 0x7C;
const CLIENT_PC: u8 = 0xBC;
const TYPE_BTH: u8 = 0xB5;
const NID_TYPE_LTP: u32 = 0x1F;

pub(super) const PR_LTP_ROW_ID: Tag = 0x67F2_0003;
pub(super) const PR_LTP_ROW_VER: Tag = 0x67F3_0003;
const PT_I8: u16 = 0x0014;

/// Подузлы, которые набирает узел (значения, строки таблиц), и счётчик их NID.
pub(super) struct Subs<'a> {
    pub list: Vec<Sub>,
    pub next: &'a mut u32,
}

impl Subs<'_> {
    /// Данные в подузел `NID_TYPE_LTP` → его NID (HNID значения).
    fn put(&mut self, ndb: &mut Ndb, chunks: &[Vec<u8>]) -> io::Result<u32> {
        *self.next += 1;
        let nid = *self.next << 5 | NID_TYPE_LTP;
        let data = ndb.chunks(chunks)?;
        self.list.push(Sub { nid, data, sub: 0 });
        Ok(nid)
    }
}

/// Куча на узле: блоки размещений; HID — (номер блока, номер размещения с 1).
struct Heap {
    client: u8,
    root: u32,
    blocks: Vec<Vec<Vec<u8>>>,
}

impl Heap {
    fn new(client: u8) -> Self {
        Self { client, root: 0, blocks: vec![Vec::new()] }
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
        (Self::header_len(block) + used).next_multiple_of(2) + 4 + 2 * (allocs.len() + 1)
    }

    fn alloc(&mut self, data: Vec<u8>) -> io::Result<u32> {
        if data.len() > ALLOC_MAX {
            return Err(io::Error::other("размещение кучи PST больше 3580 байт"));
        }
        let last = self.blocks.len() - 1;
        let fits = self.blocks[last].len() < ALLOCS_MAX && Self::block_len(last, &self.blocks[last]) + data.len() + 2 <= BLOCK_MAX;
        if !fits {
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

    /// Блоки кучи: заголовок (HNHDR, HNPAGEHDR или HNBITMAPHDR), размещения, карта страницы (HNPAGEMAP).
    fn into_blocks(self) -> Vec<Vec<u8>> {
        let fill: Vec<u8> = self.blocks.iter().enumerate().map(|(i, a)| fill_level(BLOCK_MAX - Self::block_len(i, a))).collect();
        let packed = |from: usize, n: usize| -> Vec<u8> {
            (0..n / 2).map(|j| fill.get(from + 2 * j).copied().unwrap_or(0) | fill.get(from + 2 * j + 1).copied().unwrap_or(0) << 4).collect()
        };
        let mut out = Vec::with_capacity(self.blocks.len());
        for (i, allocs) in self.blocks.iter().enumerate() {
            let mut b = vec![0; 2];
            match i {
                0 => {
                    b.extend([0xEC, self.client]);
                    b.extend(self.root.to_le_bytes());
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
            out.push(b);
        }
        out
    }
}

/// Уровень заполнения блока кучи по свободному месту (MS-PST 2.3.1.1).
fn fill_level(free: usize) -> u8 {
    const STEPS: [usize; 15] = [3584, 2560, 2048, 1792, 1536, 1280, 1024, 768, 512, 256, 128, 64, 32, 16, 8];
    STEPS.iter().position(|&s| free >= s).unwrap_or(15) as u8
}

/// BTH над упорядоченными записями (ключ, значение одного размера) в заранее занятый заголовок `header` (8 байт).
/// Листья и уровни индекса — по 3580 байт; пустое дерево — корень 0.
fn bth(heap: &mut Heap, header: u32, key: u8, value: u8, records: &[(Vec<u8>, Vec<u8>)]) -> io::Result<()> {
    let mut level: Vec<(Vec<u8>, u32)> = Vec::new();
    for part in records.chunks(ALLOC_MAX / usize::from(key + value)) {
        let bytes: Vec<u8> = part.iter().flat_map(|(k, v)| k.iter().chain(v).copied()).collect();
        level.push((part[0].0.clone(), heap.alloc(bytes)?));
    }
    let mut levels = 0u8;
    while level.len() > 1 {
        let mut up = Vec::new();
        for part in level.chunks(ALLOC_MAX / usize::from(key + 4)) {
            let bytes: Vec<u8> = part.iter().flat_map(|(k, hid)| k.iter().copied().chain(hid.to_le_bytes())).collect();
            up.push((part[0].0.clone(), heap.alloc(bytes)?));
        }
        level = up;
        levels += 1;
    }
    let root = level.first().map_or(0, |(_, hid)| *hid);
    let mut h = vec![TYPE_BTH, key, value, levels];
    h.extend(root.to_le_bytes());
    heap.set(header, h);
    Ok(())
}

/// Значение переменной длины → HNID: пустое — 0, до 3580 байт — в куче, больше — подузел.
fn hnid(heap: &mut Heap, ndb: &mut Ndb, subs: &mut Subs, bytes: Vec<u8>) -> io::Result<u32> {
    if bytes.is_empty() {
        Ok(0)
    } else if bytes.len() <= ALLOC_MAX {
        heap.alloc(bytes)
    } else {
        let chunks: Vec<Vec<u8>> = bytes.chunks(BLOCK_MAX).map(<[u8]>::to_vec).collect();
        subs.put(ndb, &chunks)
    }
}

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// Контекст свойств → блоки кучи узла. Тип значения должен совпадать с тегом, повтор свойства — первое.
pub(super) fn pc(ndb: &mut Ndb, subs: &mut Subs, props: &[(Tag, Value)]) -> io::Result<Vec<Vec<u8>>> {
    let mut heap = Heap::new(CLIENT_PC);
    heap.root = heap.alloc(vec![0; 8])?;
    let mut sorted: Vec<&(Tag, Value)> = props.iter().filter(|(t, v)| *t as u16 == v.prop_type()).collect();
    sorted.sort_by_key(|(t, _)| t >> 16);
    sorted.dedup_by_key(|(t, _)| t >> 16);
    let mut records = Vec::with_capacity(sorted.len());
    for (tag, v) in sorted {
        let value = match v {
            Value::Long(n) => *n as u32,
            Value::Boolean(b) => u32::from(*b),
            Value::SysTime(t) => heap.alloc(t.to_le_bytes().to_vec())?,
            Value::Unicode(s) => hnid(&mut heap, ndb, subs, utf16(s))?,
            Value::Binary(b) => hnid(&mut heap, ndb, subs, b.clone())?,
        };
        let mut rec = (*tag as u16).to_le_bytes().to_vec();
        rec.extend(value.to_le_bytes());
        records.push((((tag >> 16) as u16).to_le_bytes().to_vec(), rec));
    }
    let root = heap.root;
    bth(&mut heap, root, 2, 6, &records)?;
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

/// Контекст таблицы → блоки кучи узла. Столбцы — `LtpRowId`, `LtpRowVer` и `tags` (порядок задаёт раскладку);
/// значения строк других типов или вне столбцов пропускаются.
pub(super) fn tc(ndb: &mut Ndb, subs: &mut Subs, tags: &[Tag], rows: &[Row]) -> io::Result<Vec<Vec<u8>>> {
    let (cols, rgib) = layout(tags);
    let mut heap = Heap::new(CLIENT_TC);
    let index = heap.alloc(vec![0; 8])?;
    let info = heap.alloc(vec![0; 22 + 8 * cols.len()])?;

    let size = usize::from(rgib[3]);
    let mut matrix = Vec::with_capacity(rows.len() * size);
    for row in rows {
        let mut r = vec![0u8; size];
        r[0..4].copy_from_slice(&row.id.to_le_bytes());
        r[4..8].copy_from_slice(&row.ver.to_le_bytes());
        let ceb = usize::from(rgib[2]);
        for c in &cols {
            let at = usize::from(c.ib);
            let cell: Option<Vec<u8>> = match c.tag {
                PR_LTP_ROW_ID | PR_LTP_ROW_VER => Some(r[at..at + 4].to_vec()),
                _ => match row.values.iter().find(|(t, _)| *t == c.tag).map(|(_, v)| v) {
                    Some(Value::Long(n)) if c.tag as u16 == PT_LONG => Some(n.to_le_bytes().to_vec()),
                    Some(Value::Boolean(b)) if c.tag as u16 == PT_BOOLEAN => Some(vec![u8::from(*b)]),
                    Some(Value::SysTime(t)) if c.tag as u16 == PT_SYSTIME => Some(t.to_le_bytes().to_vec()),
                    Some(Value::Unicode(s)) if c.tag as u16 == PT_UNICODE => Some(hnid(&mut heap, ndb, subs, utf16(s))?.to_le_bytes().to_vec()),
                    Some(Value::Binary(b)) if c.tag as u16 == PT_BINARY => Some(hnid(&mut heap, ndb, subs, b.clone())?.to_le_bytes().to_vec()),
                    _ => None,
                },
            };
            if let Some(cell) = cell {
                r[at..at + cell.len()].copy_from_slice(&cell);
                r[ceb + usize::from(c.bit / 8)] |= 0x80 >> (c.bit % 8);
            }
        }
        matrix.extend(r);
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
        let per = (BLOCK_MAX / size).max(1) * size;
        let chunks: Vec<Vec<u8>> = matrix.chunks(per).map(<[u8]>::to_vec).collect();
        subs.put(ndb, &chunks)?
    };

    let mut t = vec![CLIENT_TC, cols.len() as u8];
    for v in rgib {
        t.extend(v.to_le_bytes());
    }
    t.extend(index.to_le_bytes());
    t.extend(rows_hnid.to_le_bytes());
    t.extend(0u32.to_le_bytes()); // hidIndex устарел
    for c in &cols {
        t.extend(c.tag.to_le_bytes());
        t.extend(c.ib.to_le_bytes());
        t.extend([c.cb, c.bit]);
    }
    heap.set(info, t);
    heap.root = info;
    Ok(heap.into_blocks())
}
