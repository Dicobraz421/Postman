//! Письмо Outlook `.msg` (S8-20, MS-OXMSG): составной файл (`cfb`) → то же, что у `.eml` ([`letter`]). Только здесь, в
//! песочнице.
//!
//! Свойства — потоки `__substg1.0_<id><тип>` (строки UTF-16 `001F` и 8-битные `001E` в кодировке письма, двоичные
//! `0102`) и фиксированные значения из `__properties_version1.0`. Шапка: тема (`PR_SUBJECT`), от (`PR_SENDER_*`,
//! иначе `PR_SENT_REPRESENTING_*`), кому и копия — получатели `__recip_version1.0_#N` (иначе `PR_DISPLAY_TO`,
//! `PR_DISPLAY_CC`), дата (`PR_CLIENT_SUBMIT_TIME`, иначе `PR_MESSAGE_DELIVERY_TIME`). Тело: `PR_HTML`, иначе HTML
//! внутри `PR_RTF_COMPRESSED` (`\fromhtml`), иначе `PR_BODY`, иначе текст RTF; сжатый RTF — LZFu (MS-OXRTFCP) с
//! проверкой CRC, разбор — [`rtf`]. Вложения `__attach_version1.0_#N` — имя, MIME и размер; вложенные письма
//! (`__substg1.0_3701000D`) — не глубже [`letter::MAX_NESTING`]. Поток читается не больше [`MAX_STREAM`] байт.

use std::collections::HashMap;
use std::io::{self, Read};

use super::letter::{self, clean, cut, people};
use super::rtf::{self, Body, Codepage};
use super::{MAX_INPUT, MAX_LISTED, Preview};
use crate::mime::Addr;

/// Поток свойства (тело, HTML, сжатый RTF): дальше не читаем, показанное — `truncated`.
pub const MAX_STREAM: u64 = 16 * 1024 * 1024;
/// Строка шапки (тема, имя, адрес).
const MAX_FIELD: u64 = 64 * 1024;

type Cf<'a> = cfb::CompoundFile<io::Cursor<&'a [u8]>>;

const PT_STRING8: u16 = 0x001E;
const PT_UNICODE: u16 = 0x001F;
const PT_BINARY: u16 = 0x0102;
const PT_LONG: u16 = 0x0003;
const PT_SYSTIME: u16 = 0x0040;

const PR_SUBJECT: u16 = 0x0037;
const PR_CLIENT_SUBMIT_TIME: u16 = 0x0039;
const PR_SENT_REPRESENTING_NAME: u16 = 0x0042;
const PR_SENT_REPRESENTING_EMAIL: u16 = 0x0065;
const PR_RECIPIENT_TYPE: u16 = 0x0C15;
const PR_SENDER_NAME: u16 = 0x0C1A;
const PR_SENDER_EMAIL: u16 = 0x0C1F;
const PR_DISPLAY_CC: u16 = 0x0E03;
const PR_DISPLAY_TO: u16 = 0x0E04;
const PR_MESSAGE_DELIVERY_TIME: u16 = 0x0E06;
const PR_ATTACH_SIZE: u16 = 0x0E20;
const PR_BODY: u16 = 0x1000;
const PR_RTF_COMPRESSED: u16 = 0x1009;
const PR_HTML: u16 = 0x1013;
const PR_DISPLAY_NAME: u16 = 0x3001;
const PR_ADDRTYPE: u16 = 0x3002;
const PR_EMAIL_ADDRESS: u16 = 0x3003;
const PR_ATTACH_DATA: u16 = 0x3701;
const PR_ATTACH_FILENAME: u16 = 0x3704;
const PR_ATTACH_METHOD: u16 = 0x3705;
const PR_ATTACH_LONG_FILENAME: u16 = 0x3707;
const PR_ATTACH_MIME_TAG: u16 = 0x370E;
const PR_SMTP_ADDRESS: u16 = 0x39FE;
const PR_INTERNET_CPID: u16 = 0x3FDE;
const PR_MESSAGE_CODEPAGE: u16 = 0x3FFD;
const PR_SENDER_SMTP_ADDRESS: u16 = 0x5D01;
const PR_SENT_REPRESENTING_SMTP_ADDRESS: u16 = 0x5D02;

/// Хранилище вложенного письма.
const EMBEDDED: &str = "__substg1.0_3701000D";
/// Вложение-письмо (`PR_ATTACH_METHOD`).
const ATTACH_EMBEDDED_MSG: u32 = 5;

/// Письмо `.msg` → шапка, тело IR и список вложений.
pub fn msg(bytes: &[u8]) -> Result<Preview, String> {
    let mut cf = cfb::CompoundFile::open(io::Cursor::new(bytes)).map_err(|e| format!("письмо .msg не разобрано: {e}"))?;
    let deepest = cf.walk().map(|e| e.path().components().filter(|c| c.as_os_str() == EMBEDDED).count()).max().unwrap_or(0);
    if deepest + 1 > letter::MAX_NESTING {
        return Err(letter::too_deep());
    }
    let mut root = Store::open(&mut cf, "", 32, 0);
    root.cp = root.long(PR_INTERNET_CPID).or_else(|| root.long(PR_MESSAGE_CODEPAGE)).unwrap_or(0);
    if !root.cf.is_stream("/__properties_version1.0") && !root.cf.walk().any(|e| e.name().starts_with("__substg1.0_")) {
        return Err("не письмо .msg".into());
    }

    let subject = root.string(PR_SUBJECT).unwrap_or_default();
    let from = root
        .address(PR_SENDER_NAME, PR_SENDER_SMTP_ADDRESS, PR_SENDER_EMAIL)
        .or_else(|| root.address(PR_SENT_REPRESENTING_NAME, PR_SENT_REPRESENTING_SMTP_ADDRESS, PR_SENT_REPRESENTING_EMAIL));
    let date = root.time(PR_CLIENT_SUBMIT_TIME).or_else(|| root.time(PR_MESSAGE_DELIVERY_TIME));
    let (blocks, mut truncated) = body(&mut root);

    let (mut recips, mut attaches): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    for e in root.cf.read_root_storage() {
        let name = e.name();
        if e.is_storage() && name.starts_with("__recip_version1.0_#") {
            recips.push(name.to_string());
        } else if e.is_storage() && name.starts_with("__attach_version1.0_#") {
            attaches.push(name.to_string());
        }
    }
    recips.sort();
    attaches.sort();
    let cp = root.cp;
    let (mut to, mut cc) = (Vec::new(), Vec::new());
    for dir in &recips {
        let mut r = Store::open(root.cf, &format!("/{dir}"), 8, cp);
        let Some(addr) = r.address(PR_DISPLAY_NAME, PR_SMTP_ADDRESS, PR_EMAIL_ADDRESS) else { continue };
        match r.long(PR_RECIPIENT_TYPE) {
            Some(2) => cc.push(addr),
            Some(3) => {}
            _ => to.push(addr),
        }
    }
    if recips.is_empty() {
        to = names(root.string(PR_DISPLAY_TO));
        cc = names(root.string(PR_DISPLAY_CC));
    }

    let mut attachments = Vec::new();
    for dir in &attaches {
        if attachments.len() >= MAX_LISTED {
            truncated = true;
            break;
        }
        let dir = format!("/{dir}");
        let mut a = Store::open(root.cf, &dir, 8, cp);
        let method = a.long(PR_ATTACH_METHOD).unwrap_or(1);
        let mut name = [PR_ATTACH_LONG_FILENAME, PR_ATTACH_FILENAME, PR_DISPLAY_NAME].into_iter().find_map(|id| a.string(id));
        let mut mime = a.string(PR_ATTACH_MIME_TAG);
        let data = format!("{dir}/__substg1.0_{PR_ATTACH_DATA:04X}{PT_BINARY:04X}");
        let mut size = a.cf.entry(&data).ok().filter(|e| e.is_stream()).map(|e| e.len());
        if method == ATTACH_EMBEDDED_MSG {
            let mut inner = Store::open(a.cf, &format!("{dir}/{EMBEDDED}"), 24, cp);
            name = name.or_else(|| inner.string(PR_SUBJECT));
            mime = mime.or_else(|| Some("application/vnd.ms-outlook".into()));
            size = None;
        }
        let size = size.or_else(|| a.long(PR_ATTACH_SIZE).map(u64::from)).unwrap_or(0);
        attachments.push((
            clean(&name.unwrap_or_default()),
            clean(&mime.unwrap_or_else(|| "application/octet-stream".into())).to_ascii_lowercase(),
            size,
        ));
    }

    Ok(Preview::Letter {
        from: people(from.into_iter().collect()),
        to: people(to),
        cc: people(cc),
        date,
        subject: clean(&subject),
        blocks,
        attachments,
        truncated,
    })
}

/// Тело: HTML, HTML внутри RTF, текст, текст RTF — что есть первым.
fn body(root: &mut Store<'_, '_>) -> (Vec<postman_ir::Block>, bool) {
    if let Some((html, cut_html)) = root.binary(PR_HTML).map(|(b, c)| (root.html(&b), c)).or_else(|| root.string_cut(PR_HTML)) {
        let (blocks, c) = cut(postman_ir::from_html(&html).blocks);
        return (blocks, c || cut_html);
    }
    let rtf = root.binary(PR_RTF_COMPRESSED).and_then(|(b, c)| lzfu(&b).ok().map(|r| (r, c))).and_then(|(r, c)| rtf::body(&r).ok().map(|b| (b, c)));
    if let Some((Body::Html(html, c), cut_rtf)) = &rtf {
        let (blocks, cut_blocks) = cut(postman_ir::from_html(html).blocks);
        return (blocks, *c || *cut_rtf || cut_blocks);
    }
    if let Some((text, cut_text)) = root.string_cut(PR_BODY) {
        let (blocks, c) = cut(postman_ir::from_plain(&text).blocks);
        return (blocks, c || cut_text);
    }
    match rtf {
        Some((Body::Blocks(blocks, c), cut_rtf)) => (blocks, c || cut_rtf),
        _ => (Vec::new(), false),
    }
}

/// «Иван Петров; Анна» → адреса без почты.
fn names(display: Option<String>) -> Vec<Addr> {
    display
        .unwrap_or_default()
        .split(';')
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(|n| Addr { name: n.to_string(), email: String::new() })
        .collect()
}

/// Объект `.msg` (письмо, получатель, вложение): каталог его потоков и фиксированные свойства.
struct Store<'c, 'a> {
    cf: &'c mut Cf<'a>,
    dir: String,
    /// Свойство → тип и 8 байт значения.
    fixed: HashMap<u16, (u16, [u8; 8])>,
    /// Кодировка 8-битных строк.
    cp: u32,
}

impl<'c, 'a> Store<'c, 'a> {
    /// `header` — длина заголовка потока свойств: 32 у письма, 24 у вложенного письма, 8 у получателя и вложения.
    fn open(cf: &'c mut Cf<'a>, dir: &str, header: usize, cp: u32) -> Self {
        let mut fixed = HashMap::new();
        if let Some((data, _)) = stream(cf, &format!("{dir}/__properties_version1.0"), 1 << 20) {
            for e in data.get(header..).unwrap_or_default().as_chunks::<16>().0 {
                let ty = u16::from_le_bytes([e[0], e[1]]);
                let id = u16::from_le_bytes([e[2], e[3]]);
                let mut value = [0u8; 8];
                value.copy_from_slice(&e[8..16]);
                fixed.insert(id, (ty, value));
            }
        }
        Store { cf, dir: dir.to_string(), fixed, cp }
    }

    fn path(&self, id: u16, ty: u16) -> String {
        format!("{}/__substg1.0_{id:04X}{ty:04X}", self.dir)
    }

    fn long(&self, id: u16) -> Option<u32> {
        match self.fixed.get(&id) {
            Some((PT_LONG, v)) => Some(u32::from_le_bytes([v[0], v[1], v[2], v[3]])),
            _ => None,
        }
    }

    /// FILETIME → Unix-секунды.
    fn time(&self, id: u16) -> Option<i64> {
        match self.fixed.get(&id) {
            Some((PT_SYSTIME, v)) => {
                let ft = u64::from_le_bytes(*v);
                (ft > 0).then(|| i64::try_from(ft / 10_000_000).unwrap_or(i64::MAX) - 11_644_473_600)
            }
            _ => None,
        }
    }

    fn binary(&mut self, id: u16) -> Option<(Vec<u8>, bool)> {
        let path = self.path(id, PT_BINARY);
        stream(self.cf, &path, MAX_STREAM)
    }

    /// Строка шапки: UTF-16 или 8-битная; пустая — нет.
    fn string(&mut self, id: u16) -> Option<String> {
        self.read_string(id, MAX_FIELD).map(|(s, _)| s).filter(|s| !s.trim().is_empty())
    }

    /// Длинная строка (тело) и признак «обрезана».
    fn string_cut(&mut self, id: u16) -> Option<(String, bool)> {
        self.read_string(id, MAX_STREAM).filter(|(s, _)| !s.trim().is_empty())
    }

    fn read_string(&mut self, id: u16, limit: u64) -> Option<(String, bool)> {
        let path = self.path(id, PT_UNICODE);
        if let Some((data, cut)) = stream(self.cf, &path, limit) {
            let units = data.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c));
            let s: String = char::decode_utf16(units).map(|c| c.unwrap_or('\u{FFFD}')).collect();
            return Some((s.trim_end_matches('\0').to_string(), cut));
        }
        let path = self.path(id, PT_STRING8);
        let (data, cut) = stream(self.cf, &path, limit)?;
        let data = data.strip_suffix(&[0]).unwrap_or(&data);
        Some((decode8(data, self.cp), cut))
    }

    /// HTML из байтов: UTF-8, если он корректен, иначе — кодировка письма.
    fn html(&self, b: &[u8]) -> String {
        match std::str::from_utf8(b) {
            Ok(s) => s.to_string(),
            Err(_) => decode8(b, self.cp),
        }
    }

    /// Адрес из имени и почты (SMTP, иначе — адрес, если это почта, а не путь Exchange).
    fn address(&mut self, name: u16, smtp: u16, email: u16) -> Option<Addr> {
        let addr = self.string(smtp).or_else(|| {
            let typed = self.string(PR_ADDRTYPE).is_none_or(|t| t.eq_ignore_ascii_case("SMTP"));
            self.string(email).filter(|e| typed && e.contains('@') && !e.starts_with('/'))
        });
        let name = self.string(name);
        if name.is_none() && addr.is_none() {
            return None;
        }
        Some(Addr { name: name.unwrap_or_default(), email: addr.unwrap_or_default() })
    }
}

/// Поток целиком, но не больше `limit` байт, и признак «обрезан».
fn stream(cf: &mut Cf<'_>, path: &str, limit: u64) -> Option<(Vec<u8>, bool)> {
    let s = cf.open_stream(path).ok()?;
    let len = s.len();
    let mut data = Vec::new();
    s.take(limit).read_to_end(&mut data).ok()?;
    Some((data, len > limit))
}

/// 8-битная строка: UTF-8 (65001), однобайтовая кодовая страница письма; без неё — UTF-8, если корректен, иначе 1251.
fn decode8(b: &[u8], cp: u32) -> String {
    let cp = match (cp, std::str::from_utf8(b)) {
        (65001, _) | (0, Ok(_)) => return String::from_utf8_lossy(b).into_owned(),
        (0, Err(_)) => 1251,
        (cp, _) => cp,
    };
    let table = Codepage::new(cp);
    b.iter().map(|&c| table.char(c)).collect()
}

// --- LZFu (MS-OXRTFCP) ---

/// Начало словаря сжатого RTF (MS-OXRTFCP 3.1.3.1); запись — с его конца.
const PREBUF: &[u8; 207] = b"{\\rtf1\\ansi\\mac\\deff0\\deftab720{\\fonttbl;}{\\f0\\fnil \\froman \\fswiss \\fmodern \\fscript \\fdecor MS Sans SerifSymbolArialTimes New RomanCourier{\\colortbl\\red0\\green0\\blue0\r\n\\par \\pard\\plain\\f0\\fs20\\b\\i\\u\\tab\\tx";
const COMPRESSED: u32 = 0x7546_5A4C;
const UNCOMPRESSED: u32 = 0x414C_454D;

/// `PR_RTF_COMPRESSED` → RTF: заголовок (размеры, тип, CRC), затем серии «байт флагов + 8 знаков» — литерал или
/// ссылка в словарь 4 КиБ (12 бит смещения, 4 бита длины − 2); ссылка на позицию записи — конец. Больше
/// [`MAX_INPUT`] не распаковываем.
pub fn lzfu(data: &[u8]) -> Result<Vec<u8>, String> {
    let word = |at: usize| data.get(at..at + 4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]));
    let (Some(comp_size), Some(raw_size), Some(kind), Some(crc)) = (word(0), word(4), word(8), word(12)) else {
        return Err("сжатый RTF повреждён".into());
    };
    let end = usize::try_from(comp_size).unwrap_or(usize::MAX).saturating_add(4).min(data.len());
    let contents = data.get(16..end).unwrap_or_default();
    let raw_size = usize::try_from(raw_size).unwrap_or(usize::MAX).min(MAX_INPUT);
    match kind {
        UNCOMPRESSED => return Ok(contents[..contents.len().min(raw_size)].to_vec()),
        COMPRESSED => {}
        _ => return Err("сжатый RTF: неизвестный тип".into()),
    }
    if crc32(contents) != crc {
        return Err("сжатый RTF повреждён".into());
    }
    let mut dict = [0u8; 4096];
    dict[..PREBUF.len()].copy_from_slice(PREBUF);
    let mut w = PREBUF.len();
    let mut out = Vec::with_capacity(raw_size.min(1 << 20));
    let mut i = 0;
    'runs: while let Some(&control) = contents.get(i) {
        i += 1;
        for bit in 0..8 {
            if control & (1 << bit) == 0 {
                let Some(&b) = contents.get(i) else { break 'runs };
                i += 1;
                out.push(b);
                dict[w] = b;
                w = (w + 1) % dict.len();
            } else {
                let Some(r) = contents.get(i..i + 2) else { break 'runs };
                i += 2;
                let r = u16::from_be_bytes([r[0], r[1]]);
                let offset = usize::from(r >> 4);
                if offset == w {
                    break 'runs;
                }
                for k in 0..usize::from(r & 0xF) + 2 {
                    let b = dict[(offset + k) % dict.len()];
                    out.push(b);
                    dict[w] = b;
                    w = (w + 1) % dict.len();
                }
            }
            if out.len() >= MAX_INPUT {
                break 'runs;
            }
        }
    }
    Ok(out)
}

/// CRC-32 сжатого RTF: многочлен 0xEDB88320, начальное значение 0, без финального XOR.
fn crc32(data: &[u8]) -> u32 {
    let mut c = 0u32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use postman_ir::{Align, Block, Inline, Marks};

    use super::*;
    use crate::preview::{PreviewKind, preview};

    /// Пример сжатия из MS-OXRTFCP (3.1.1.1).
    const SAMPLE: &[u8] = &[
        0x2d, 0x00, 0x00, 0x00, 0x2b, 0x00, 0x00, 0x00, 0x4c, 0x5a, 0x46, 0x75, 0xf1, 0xc5, 0xc7, 0xa7, 0x03, 0x00, 0x0a, 0x00, 0x72, 0x63, 0x70,
        0x67, 0x31, 0x32, 0x35, 0x42, 0x32, 0x0a, 0xf3, 0x20, 0x68, 0x65, 0x6c, 0x09, 0x00, 0x20, 0x62, 0x77, 0x05, 0xb0, 0x6c, 0x64, 0x7d, 0x0a,
        0x80, 0x0f, 0xa0,
    ];

    #[test]
    fn lzfu_sample_from_the_spec() {
        assert_eq!(lzfu(SAMPLE).unwrap(), b"{\\rtf1\\ansi\\ansicpg1252\\pard hello world}\r\n");
        let mut bad = SAMPLE.to_vec();
        bad[20] ^= 1;
        assert!(lzfu(&bad).is_err(), "CRC");
        assert!(lzfu(&SAMPLE[..10]).is_err());
        let mut raw = vec![0u8; 16];
        raw[0..4].copy_from_slice(&20u32.to_le_bytes());
        raw[4..8].copy_from_slice(&8u32.to_le_bytes());
        raw[8..12].copy_from_slice(&UNCOMPRESSED.to_le_bytes());
        raw.extend_from_slice(b"{\\rtf1 x}");
        assert_eq!(lzfu(&raw).unwrap(), b"{\\rtf1 x");
    }

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    /// Поток свойств: заголовок `header` байт и фиксированные свойства (тег, 8 байт значения).
    fn props(header: usize, list: &[(u32, u64)]) -> Vec<u8> {
        let mut out = vec![0u8; header];
        for (tag, value) in list {
            out.extend(tag.to_le_bytes());
            out.extend(6u32.to_le_bytes());
            out.extend(value.to_le_bytes());
        }
        out
    }

    struct Msg(cfb::CompoundFile<io::Cursor<Vec<u8>>>);

    impl Msg {
        fn new() -> Self {
            Msg(cfb::CompoundFile::create(io::Cursor::new(Vec::new())).unwrap())
        }

        fn put(&mut self, path: &str, data: &[u8]) -> &mut Self {
            self.0.create_stream(path).unwrap().write_all(data).unwrap();
            self
        }

        fn text(&mut self, dir: &str, id: u16, s: &str) -> &mut Self {
            self.put(&format!("{dir}/__substg1.0_{id:04X}001F"), &utf16(s))
        }

        fn dir(&mut self, path: &str) -> &mut Self {
            self.0.create_storage(path).unwrap();
            self
        }

        fn bytes(mut self) -> Vec<u8> {
            self.0.flush().unwrap();
            self.0.into_inner().into_inner()
        }
    }

    /// 2025-10-21 11:01 UTC в FILETIME.
    const FILETIME: u64 = (1_761_044_460 + 11_644_473_600) * 10_000_000;

    #[test]
    fn msg_with_html_recipients_and_attachments() {
        let mut m = Msg::new();
        m.put("/__properties_version1.0", &props(32, &[(0x0039_0040, FILETIME), (0x3FDE_0003, 65001)]))
            .text("", PR_SUBJECT, "Отчёт\u{202e}")
            .text("", PR_SENDER_NAME, "Анна")
            .text("", PR_SENDER_EMAIL, "/O=EXCHANGELABS/OU=ANNA")
            .text("", PR_SENDER_SMTP_ADDRESS, "anna@example.test")
            .text("", PR_DISPLAY_TO, "Пётр; Иван")
            .text("", PR_BODY, "Привет, мир")
            .put("/__substg1.0_10130102", "<p>Привет, <b>мир</b> <a href=\"javascript:x\">x</a></p>".as_bytes());
        for (n, (kind, name, smtp)) in [(1u64, "Пётр", "petr@example.test"), (2, "Команда", "team@example.test")].into_iter().enumerate() {
            let dir = format!("/__recip_version1.0_#{n:08X}");
            m.dir(&dir).put(&format!("{dir}/__properties_version1.0"), &props(8, &[(0x0C15_0003, kind)])).text(&dir, PR_DISPLAY_NAME, name).text(
                &dir,
                PR_SMTP_ADDRESS,
                smtp,
            );
        }
        let a = "/__attach_version1.0_#00000000";
        m.dir(a)
            .put(&format!("{a}/__properties_version1.0"), &props(8, &[(0x3705_0003, 1)]))
            .text(a, PR_ATTACH_LONG_FILENAME, "смета.xlsx")
            .text(a, PR_ATTACH_MIME_TAG, "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet")
            .put(&format!("{a}/__substg1.0_37010102"), &[7u8; 1234]);
        let e = "/__attach_version1.0_#00000001";
        m.dir(e).put(&format!("{e}/__properties_version1.0"), &props(8, &[(0x3705_0003, 5), (0x0E20_0003, 4321)])).dir(&format!("{e}/{EMBEDDED}"));
        m.text(&format!("{e}/{EMBEDDED}"), PR_SUBJECT, "Пересланное");
        let (pv, _) = preview(PreviewKind::Letter, &m.bytes(), 0, 0, 0).unwrap();
        let Preview::Letter { from, to, cc, date, subject, blocks, attachments, truncated } = pv else { panic!("{pv:?}") };
        assert_eq!(from, [Addr { name: "Анна".into(), email: "anna@example.test".into() }]);
        assert_eq!(to, [Addr { name: "Пётр".into(), email: "petr@example.test".into() }]);
        assert_eq!(cc, [Addr { name: "Команда".into(), email: "team@example.test".into() }]);
        assert_eq!(date, Some(1_761_044_460));
        assert_eq!(subject, "Отчёт");
        let bold = Marks { bold: true, ..Marks::default() };
        assert_eq!(
            blocks,
            [Block::Paragraph {
                align: Align::Start,
                content: vec![Inline::text("Привет, "), Inline::Text { text: "мир".into(), marks: bold }, Inline::text(" "), Inline::text("x")]
            }]
        );
        assert_eq!(
            attachments,
            [
                ("смета.xlsx".into(), "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".into(), 1234),
                ("Пересланное".into(), "application/vnd.ms-outlook".into(), 4321),
            ]
        );
        assert!(!truncated);
    }

    /// Только сжатый RTF (образец MS-OXRTFCP) — текст разбором RTF; получатели — из `PR_DISPLAY_TO`, 8-битная тема.
    #[test]
    fn msg_with_compressed_rtf_only() {
        let mut m = Msg::new();
        m.put("/__properties_version1.0", &props(32, &[(0x3FFD_0003, 1251)]))
            .put("/__substg1.0_0037001E", &[0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2, 0])
            .text("", PR_DISPLAY_TO, "Пётр; Иван")
            .put("/__substg1.0_10090102", SAMPLE);
        let Preview::Letter { to, subject, blocks, attachments, date, .. } = msg(&m.bytes()).unwrap() else { panic!() };
        assert_eq!(subject, "Привет");
        assert_eq!(to.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["Пётр", "Иван"]);
        assert_eq!(blocks, [Block::Paragraph { align: Align::Start, content: vec![Inline::text("hello world")] }]);
        assert!(attachments.is_empty() && date.is_none());
    }

    /// Письмо в письме в письме в письме — предел; битый `.msg` — ошибка без паники.
    #[test]
    fn nested_and_broken_msg() {
        let nested = |levels: usize| {
            let mut m = Msg::new();
            m.text("", PR_SUBJECT, "1");
            let mut dir = String::new();
            for _ in 1..levels {
                let a = format!("{dir}/__attach_version1.0_#00000000");
                dir = format!("{a}/{EMBEDDED}");
                m.dir(&a).dir(&dir).text(&dir, PR_SUBJECT, "n");
            }
            m.bytes()
        };
        assert!(msg(&nested(letter::MAX_NESTING)).is_ok());
        assert_eq!(msg(&nested(letter::MAX_NESTING + 1)).unwrap_err(), letter::too_deep());

        let good = nested(2);
        let mut garbage = OLE_HEADER.to_vec();
        garbage.extend([0xAB; 600]);
        for bad in [&garbage[..], &good[..good.len() / 2], &good[..520]] {
            assert!(preview(PreviewKind::Letter, bad, 0, 0, 0).is_err());
        }
        // Составной файл без свойств письма (документ Word) — не письмо.
        let mut doc = Msg::new();
        doc.put("/WordDocument", b"x");
        assert!(msg(&doc.bytes()).is_err());
    }

    const OLE_HEADER: &[u8] = &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
}
