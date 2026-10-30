//! MSG (MS-OXMSG): письмо Outlook — составной файл MS-CFB (крейт `cfb`) из свойств MAPI ([`MapiMessage`], S8-29).
//!
//! Хранилище письма: поток свойств `__properties_version1.0` (заголовок 32 байта у верхнего письма, 24 — у вложенного,
//! 8 — у получателя и вложения; дальше записи по 16 байт), строки и двоичные — потоками `__substg1.0_<тег>`, получатели —
//! `__recip_version1.0_#XXXXXXXX`, вложения — `__attach_version1.0_#XXXXXXXX`, вложенное письмо — хранилище
//! `__substg1.0_3701000D` во вложении. У верхнего письма — пустое `__nameid_version1.0` (Outlook без него не открывает).
//! Строки — UTF-16LE (`PT_UNICODE`), пустые не пишутся (MS-OXMSG 2.4.2.2: хотя бы один символ).

use std::collections::HashSet;
use std::io::{self, Cursor, Read, Seek, Write};

use cfb::CompoundFile;

use crate::mapi::{
    MapiMessage, PR_ATTACH_DATA_OBJ, PR_ATTACH_LONG_FILENAME_W, PR_ATTACH_NUM, PR_DISPLAY_NAME_W, PR_MESSAGE_CLASS_W, PR_ROWID,
    PR_STORE_SUPPORT_MASK, PR_SUBJECT_W, STORE_UNICODE_OK, Tag, Value,
};

/// CLSID хранилища письма (корень и вложенное письмо), как у файлов Outlook.
const CLSID_MESSAGE: &str = "00020d0b-0000-0000-c000-000000000046";
/// Флаги записи свойства: `PROPATTR_READABLE | PROPATTR_WRITABLE`.
const PROPATTR_RW: u32 = 0x6;
/// Потоки пустой таблицы именованных свойств: GUID, записи, строки.
const NAMEID_STREAMS: [&str; 3] = ["00020102", "00030102", "00040102"];

/// Письмо → файл `.msg`. Файл собирается в памяти (`cfb` пишет с перемоткой) и отдаётся в `out` целиком.
pub fn write(out: &mut impl Write, msg: &MapiMessage) -> io::Result<()> {
    let mut cf = CompoundFile::create_with_version(cfb::Version::V3, Cursor::new(Vec::new()))?;
    message(&mut cf, "", msg, true)?;
    cf.create_storage("/__nameid_version1.0")?;
    for name in NAMEID_STREAMS {
        cf.create_stream(format!("/__nameid_version1.0/__substg1.0_{name}"))?;
    }
    cf.flush()?;
    out.write_all(cf.into_inner().get_ref())
}

/// Письмо в хранилище `path` (`""` — корень): свойства, получатели, вложения.
fn message<F: Read + Write + Seek>(cf: &mut CompoundFile<F>, path: &str, msg: &MapiMessage, top: bool) -> io::Result<()> {
    if !path.is_empty() {
        cf.create_storage(path)?;
    }
    let clsid = CLSID_MESSAGE.parse().map_err(|_| io::Error::other("CLSID"))?;
    cf.set_storage_clsid(if path.is_empty() { "/" } else { path }, clsid)?;
    let mut props = Props::default();
    props.add(cf, path, PR_MESSAGE_CLASS_W, &Value::Unicode(msg.class.clone()))?;
    props.add(cf, path, PR_STORE_SUPPORT_MASK, &Value::Long(STORE_UNICODE_OK))?;
    for (tag, v) in &msg.props {
        props.add(cf, path, *tag, v)?;
    }

    for (i, r) in msg.recipients.iter().enumerate() {
        let sub = format!("{path}/__recip_version1.0_#{i:08X}");
        cf.create_storage(&sub)?;
        let mut rp = Props::default();
        rp.add(cf, &sub, PR_ROWID, &Value::Long(i as i32))?;
        for (tag, v) in &r.props {
            rp.add(cf, &sub, *tag, v)?;
        }
        rp.finish(cf, &sub, &[0; 8])?;
    }

    for (i, a) in msg.attachments.iter().enumerate() {
        let sub = format!("{path}/__attach_version1.0_#{i:08X}");
        cf.create_storage(&sub)?;
        let mut ap = Props::default();
        ap.add(cf, &sub, PR_ATTACH_NUM, &Value::Long(i as i32))?;
        for (tag, v) in &a.props {
            ap.add(cf, &sub, *tag, v)?;
        }
        // Имя в списке вложений: имя файла, у вложенного письма без имени — его тема.
        let name = text(&a.props, PR_ATTACH_LONG_FILENAME_W).or_else(|| a.embedded.as_deref().and_then(|e| text(&e.props, PR_SUBJECT_W)));
        if let Some(name) = name {
            ap.add(cf, &sub, PR_DISPLAY_NAME_W, &Value::Unicode(name.to_owned()))?;
        }
        if let Some(embedded) = &a.embedded {
            // MS-OXMSG 2.4.2.2: у PtypObject размер 0xFFFFFFFF, Reserved — 1 для вложенного письма.
            ap.entry(PR_ATTACH_DATA_OBJ, 0xFFFF_FFFF, 1);
            message(cf, &format!("{sub}/__substg1.0_3701000D"), embedded, false)?;
        }
        ap.finish(cf, &sub, &[0; 8])?;
    }

    let (n, m) = (count(msg.recipients.len())?, count(msg.attachments.len())?);
    let mut header = vec![0; 8];
    for v in [n, m, n, m] {
        header.extend(v.to_le_bytes());
    }
    if top {
        header.extend([0; 8]);
    }
    props.finish(cf, path, &header)
}

/// Записи потока свойств и уже записанные теги (повтор тега — пропуск, первый побеждает).
#[derive(Default)]
struct Props {
    entries: Vec<u8>,
    seen: HashSet<Tag>,
}

impl Props {
    /// Свойство: число, логическое и время — в записи, строка и двоичное — своим потоком.
    fn add<F: Read + Write + Seek>(&mut self, cf: &mut CompoundFile<F>, path: &str, tag: Tag, v: &Value) -> io::Result<()> {
        if tag as u16 != v.prop_type() || self.seen.contains(&tag) {
            return Ok(());
        }
        let mut fixed = [0; 8];
        match v {
            Value::Long(n) => fixed[..4].copy_from_slice(&n.to_le_bytes()),
            Value::Boolean(b) => fixed[0] = u8::from(*b),
            Value::SysTime(t) => fixed = t.to_le_bytes(),
            Value::Unicode(s) => {
                if s.is_empty() {
                    return Ok(());
                }
                let bytes: Vec<u8> = s.encode_utf16().flat_map(u16::to_le_bytes).collect();
                // Размер строки — с завершающим нулём, в потоке нуля нет.
                let size = u32::try_from(bytes.len() + 2).map_err(|_| too_big())?;
                stream(cf, path, tag, &bytes)?;
                self.entry(tag, size, 0);
                return Ok(());
            }
            Value::Binary(b) => {
                let size = u32::try_from(b.len()).map_err(|_| too_big())?;
                stream(cf, path, tag, b)?;
                self.entry(tag, size, 0);
                return Ok(());
            }
        }
        self.seen.insert(tag);
        self.entries.extend(tag.to_le_bytes());
        self.entries.extend(PROPATTR_RW.to_le_bytes());
        self.entries.extend(fixed);
        Ok(())
    }

    /// Запись свойства переменной длины: размер и Reserved.
    fn entry(&mut self, tag: Tag, size: u32, reserved: u32) {
        if self.seen.insert(tag) {
            for v in [tag, PROPATTR_RW, size, reserved] {
                self.entries.extend(v.to_le_bytes());
            }
        }
    }

    fn finish<F: Read + Write + Seek>(self, cf: &mut CompoundFile<F>, path: &str, header: &[u8]) -> io::Result<()> {
        let mut s = cf.create_stream(format!("{path}/__properties_version1.0"))?;
        s.write_all(header)?;
        s.write_all(&self.entries)
    }
}

fn stream<F: Read + Write + Seek>(cf: &mut CompoundFile<F>, path: &str, tag: Tag, data: &[u8]) -> io::Result<()> {
    cf.create_stream(format!("{path}/__substg1.0_{tag:08X}"))?.write_all(data)
}

fn text(props: &[(Tag, Value)], tag: Tag) -> Option<&str> {
    props.iter().find_map(|(t, v)| match v {
        Value::Unicode(s) if *t == tag && !s.is_empty() => Some(s.as_str()),
        _ => None,
    })
}

/// Получателей и вложений — не больше 2048 (MS-OXMSG 2.2.1, 2.2.2).
fn count(n: usize) -> io::Result<u32> {
    if n > 2048 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "больше 2048 получателей или вложений"));
    }
    Ok(n as u32)
}

fn too_big() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "свойство больше 4 ГиБ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapi::{ATTACH_BY_VALUE, Attachment, IPM_NOTE, PR_ATTACH_DATA_BIN, PR_ATTACH_METHOD, PR_SUBJECT_PREFIX_W};

    fn read(file: &[u8], path: &str) -> Vec<u8> {
        let mut cf = CompoundFile::open(Cursor::new(file)).unwrap();
        let mut out = Vec::new();
        cf.open_stream(path).unwrap().read_to_end(&mut out).unwrap();
        out
    }

    #[test]
    fn property_stream_layout() {
        let inner = MapiMessage { class: IPM_NOTE.into(), props: vec![(PR_SUBJECT_W, Value::Unicode("в".into()))], ..Default::default() };
        let msg = MapiMessage {
            class: IPM_NOTE.into(),
            props: vec![
                (PR_SUBJECT_W, Value::Unicode("Тема".into())),
                (PR_SUBJECT_PREFIX_W, Value::Unicode(String::new())),
                (PR_SUBJECT_W, Value::Unicode("повтор".into())),
            ],
            attachments: vec![
                Attachment {
                    props: vec![(PR_ATTACH_METHOD, Value::Long(ATTACH_BY_VALUE)), (PR_ATTACH_DATA_BIN, Value::Binary(vec![1, 2, 3]))],
                    embedded: None,
                },
                Attachment { props: vec![], embedded: Some(Box::new(inner)) },
            ],
            ..Default::default()
        };
        let mut file = Vec::new();
        write(&mut file, &msg).unwrap();

        let top = read(&file, "/__properties_version1.0");
        // Заголовок: следующий получатель 0, следующее вложение 2, получателей 0, вложений 2.
        assert_eq!(&top[8..24], &[0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0]);
        assert_eq!((top.len() - 32) % 16, 0);
        let entries: Vec<&[u8]> = top[32..].chunks(16).collect();
        let subject = entries.iter().find(|e| e[..4] == PR_SUBJECT_W.to_le_bytes()).unwrap();
        // «Тема»: 4 символа UTF-16 и завершающий ноль.
        assert_eq!(&subject[8..12], &10u32.to_le_bytes());
        assert_eq!(read(&file, "/__substg1.0_0037001F"), "Тема".encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>());
        assert_eq!(entries.iter().filter(|e| e[..4] == PR_SUBJECT_W.to_le_bytes()).count(), 1);
        assert!(!entries.iter().any(|e| e[..4] == PR_SUBJECT_PREFIX_W.to_le_bytes()));
        let mask = entries.iter().find(|e| e[..4] == PR_STORE_SUPPORT_MASK.to_le_bytes()).unwrap();
        assert_eq!(&mask[8..12], &STORE_UNICODE_OK.to_le_bytes());

        let att = read(&file, "/__attach_version1.0_#00000001/__properties_version1.0");
        let obj = att[8..].chunks(16).find(|e| e[..4] == PR_ATTACH_DATA_OBJ.to_le_bytes()).unwrap();
        assert_eq!(&obj[8..16], &[0xFF, 0xFF, 0xFF, 0xFF, 1, 0, 0, 0]);
        let embedded = read(&file, "/__attach_version1.0_#00000001/__substg1.0_3701000D/__properties_version1.0");
        assert_eq!((embedded.len() - 24) % 16, 0);
        assert_eq!(read(&file, "/__attach_version1.0_#00000000/__substg1.0_37010102"), [1, 2, 3]);

        let cf = CompoundFile::open(Cursor::new(&file)).unwrap();
        assert!(cf.is_storage("/__nameid_version1.0"));
        assert!(!cf.is_storage("/__attach_version1.0_#00000001/__substg1.0_3701000D/__nameid_version1.0"));
        assert_eq!(cf.root_entry().clsid().to_string(), CLSID_MESSAGE);
    }
}
