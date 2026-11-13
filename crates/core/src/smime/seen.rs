//! Промежуточные сертификаты из полученных подписанных писем (PAR-02b, решение оркестратора 2026-11-13): на macOS и
//! Linux — замена хранилища `CA` Windows. Письмо без промежуточных проверяется так же, как на Windows, если такие уже
//! приходили в другом письме с цепочкой до доверенного корня.
//!
//! Только для построения цепочки — не доверие: воркер берёт их кандидатами между подписантом и якорем, корни
//! и запреты — из системы. Ядро байты не разбирает: DER — из цепочки, которую воркер уже довёл до доверенного корня
//! (подписант и корень не берутся). Файл — в каталоге данных (`smime-intermediates.bin`, только владелец), формат —
//! длина (u32, big-endian) + DER подряд, новые — в конце; пределы — [`MAX_ITEMS`], [`MAX_ONE`], [`MAX_BYTES`]:
//! сверх — вытесняются самые старые. Без каталога данных — только память процесса.

use std::path::{Path, PathBuf};

/// Сколько сертификатов держать.
pub(crate) const MAX_ITEMS: usize = 256;
/// Один сертификат больше — не берётся.
pub(crate) const MAX_ONE: usize = 16 * 1024;
/// Всего байтов DER.
pub(crate) const MAX_BYTES: usize = 1 << 20;
/// Имя файла в каталоге данных.
const FILE: &str = "smime-intermediates.bin";

/// Файл кэша в каталоге данных.
pub(crate) fn path() -> PathBuf {
    crate::paths::data_dir().join(FILE)
}

/// Разбор своего файла: до первой нецелой записи, с пределами.
pub(crate) fn decode(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut rest = bytes;
    while rest.len() >= 4 {
        let n = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
        let Some(der) = rest.get(4..4 + n) else { break };
        if n > 0 && n <= MAX_ONE && !out.iter().any(|d| d == der) {
            out.push(der.to_vec());
        }
        rest = &rest[4 + n..];
    }
    trim(&mut out);
    out
}

pub(crate) fn encode(list: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::with_capacity(list.iter().map(|d| d.len() + 4).sum());
    for der in list {
        // `MAX_ONE` меньше `u32::MAX`: длина помещается.
        out.extend_from_slice(&(der.len() as u32).to_be_bytes());
        out.extend_from_slice(der);
    }
    out
}

/// Старые — вон, пока список не уложится в пределы.
fn trim(list: &mut Vec<Vec<u8>>) {
    let mut size: usize = list.iter().map(Vec::len).sum();
    while list.len() > MAX_ITEMS || size > MAX_BYTES {
        size -= list.remove(0).len();
    }
}

/// Добавить `ders` (уже знакомый — в конец, как свежий); `true` — появился новый.
pub(crate) fn add(list: &mut Vec<Vec<u8>>, ders: &[Vec<u8>]) -> bool {
    let mut new = false;
    for der in ders.iter().filter(|d| !d.is_empty() && d.len() <= MAX_ONE) {
        match list.iter().position(|d| d == der) {
            Some(i) => {
                let d = list.remove(i);
                list.push(d);
            }
            None => {
                list.push(der.clone());
                new = true;
            }
        }
    }
    trim(list);
    new
}

/// Промежуточные цепочки «подписант, …, корень» — без первого и последнего.
pub(crate) fn middle(chain: &[Vec<u8>]) -> &[Vec<u8>] {
    if chain.len() < 3 { &[] } else { &chain[1..chain.len() - 1] }
}

/// Прочитать файл; нет или не читается — пусто.
pub(crate) fn load(path: &Path) -> Vec<Vec<u8>> {
    let max = (MAX_BYTES + 4 * MAX_ITEMS) as u64;
    match std::fs::metadata(path) {
        Ok(m) if m.len() <= max => std::fs::read(path).map(|b| decode(&b)).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Записать атомарно (временный файл рядом и переименование), права — только владелец.
pub(crate) fn save(path: &Path, list: &[Vec<u8>]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(&tmp)?;
    f.write_all(&encode(list))?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn der(n: u8, len: usize) -> Vec<u8> {
        vec![n; len]
    }

    #[test]
    fn round_trip_and_limits() {
        let list = vec![der(1, 10), der(2, 20)];
        assert_eq!(decode(&encode(&list)), list);
        // Обрезанный хвост и пустые записи — мимо.
        let mut bytes = encode(&list);
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes.extend_from_slice(&[0, 0, 1, 0, 9]);
        assert_eq!(decode(&bytes), list);

        let mut l = Vec::new();
        assert!(add(&mut l, &[der(1, 10), der(2, 10)]));
        assert!(!add(&mut l, &[der(1, 10)]), "знакомый — не новый");
        assert_eq!(l.last(), Some(&der(1, 10)), "знакомый — в конец");
        assert!(!add(&mut l, &[der(3, MAX_ONE + 1)]), "слишком большой — не берётся");

        let mut many = Vec::new();
        for i in 0..MAX_ITEMS + 3 {
            add(&mut many, &[(i as u32).to_be_bytes().to_vec()]);
        }
        assert_eq!(many.len(), MAX_ITEMS);
        assert_eq!(many[0], 3u32.to_be_bytes().to_vec(), "вытеснены самые старые");

        let mut big = Vec::new();
        for i in 0..80u8 {
            add(&mut big, &[der(i, MAX_ONE)]);
        }
        assert!(big.iter().map(Vec::len).sum::<usize>() <= MAX_BYTES);
    }

    #[test]
    fn middle_of_chain() {
        let (a, b, c) = (der(1, 1), der(2, 1), der(3, 1));
        assert!(middle(&[a.clone(), c.clone()]).is_empty());
        assert_eq!(middle(&[a, b.clone(), c]), &[b]);
    }

    #[test]
    fn file_round_trip() {
        let dir = std::env::temp_dir().join(format!("postman-seen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(FILE);
        assert!(load(&p).is_empty());
        save(&p, &[der(7, 30)]).unwrap();
        assert_eq!(load(&p), vec![der(7, 30)]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
