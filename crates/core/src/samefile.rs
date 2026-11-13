//! Одинаковые вложения по содержимому (MED-01; общее для «Медиа беседы» и «кто уже получал вложение» в беседе).
//!
//! Главный признак — SHA-256 раскодированных байтов ([`Hash::of`]): тот же ключ, под которым хранилище формата 6
//! держит части писем (ADR 0006, `postman_cache::content`, таблица `part`), — одинаковые байты под разными блобами
//! (пересланный файл) сводятся. Байтов нет (не скачаны) — тот же блоб; нет и его — то же имя (без регистра) и
//! размер. Разные известные хэши не сводятся никогда.

use std::collections::HashMap;

use postman_cache::{AttachmentRow, Hash};

/// По чему вложение признано тем же: для показа и отладки.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FileKey {
    /// SHA-256 раскодированных байтов.
    Content(Hash),
    /// Блоб на сервере (байтов нет).
    Blob(String),
    /// Имя в нижнем регистре и размер (нет ни байтов, ни блоба).
    NameSize(String, u64),
}

/// Имя для сравнения: без пробелов по краям, в нижнем регистре; пустое — не признак.
fn name_of(a: &AttachmentRow) -> Option<String> {
    let n = a.name.trim().to_lowercase();
    (!n.is_empty()).then_some(n)
}

/// Ключ вложения по лучшему из известных признаков; `hashes` — блоб → хэш байтов.
pub fn file_key(a: &AttachmentRow, hashes: &HashMap<String, Hash>) -> FileKey {
    match hashes.get(&a.blob_id) {
        Some(h) => FileKey::Content(*h),
        None if !a.blob_id.is_empty() => FileKey::Blob(a.blob_id.clone()),
        None => FileKey::NameSize(name_of(a).unwrap_or_default(), a.size),
    }
}

/// Одно ли это вложение: оба хэша известны — по ним; иначе тот же блоб или то же имя с размером.
pub fn same_file(a: &AttachmentRow, b: &AttachmentRow, hashes: &HashMap<String, Hash>) -> bool {
    match (hashes.get(&a.blob_id), hashes.get(&b.blob_id)) {
        (Some(x), Some(y)) => x == y,
        _ => (!a.blob_id.is_empty() && a.blob_id == b.blob_id) || (name_of(a).is_some() && name_of(a) == name_of(b) && a.size == b.size),
    }
}

/// Сводка вложений в группы одинаковых ([`same_file`] с транзитивностью по порядку добавления): номер группы на
/// вложение, с нуля, новые — подряд.
#[derive(Debug, Default)]
pub struct Groups {
    by_hash: HashMap<Hash, usize>,
    by_blob: HashMap<String, usize>,
    by_name: HashMap<(String, u64), usize>,
    /// Хэш группы, если он известен хоть у одного вложения в ней.
    hash: Vec<Option<Hash>>,
}

impl Groups {
    /// Группа вложения `a` (хэш байтов — `hash`, если байты есть) и новая ли она.
    pub fn add(&mut self, a: &AttachmentRow, hash: Option<Hash>) -> (usize, bool) {
        let found = self.find(a, hash);
        let g = found.unwrap_or(self.hash.len());
        if found.is_none() {
            self.hash.push(None);
        }
        if let Some(h) = hash {
            self.by_hash.insert(h, g);
            if let Some(slot) = self.hash.get_mut(g) {
                *slot = Some(h);
            }
        }
        if !a.blob_id.is_empty() {
            self.by_blob.entry(a.blob_id.clone()).or_insert(g);
        }
        if let Some(n) = name_of(a) {
            self.by_name.entry((n, a.size)).or_insert(g);
        }
        (g, found.is_none())
    }

    fn find(&self, a: &AttachmentRow, hash: Option<Hash>) -> Option<usize> {
        if let Some(g) = hash.and_then(|h| self.by_hash.get(&h)) {
            return Some(*g);
        }
        // Известный хэш, которого нет среди групп, к группе с другим хэшем не присоединяется.
        let fits = |g: &usize| hash.is_none() || self.hash.get(*g).is_some_and(Option::is_none);
        let blob = self.by_blob.get(&a.blob_id).filter(|g| fits(g));
        blob.or_else(|| name_of(a).and_then(|n| self.by_name.get(&(n, a.size))).filter(|g| fits(g))).copied()
    }

    /// Ключ группы `g`: хэш, если он известен, иначе — по первому вложению `first`.
    pub fn key(&self, g: usize, first: &AttachmentRow) -> FileKey {
        match self.hash.get(g).copied().flatten() {
            Some(h) => FileKey::Content(h),
            None => file_key(first, &HashMap::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn att(blob: &str, name: &str, size: u64) -> AttachmentRow {
        AttachmentRow { blob_id: blob.into(), name: name.into(), mime: "application/pdf".into(), size, ..Default::default() }
    }

    #[test]
    fn same_by_hash_blob_or_name_and_size() {
        let (a, b, c) = (att("b1", "ТЗ.pdf", 10), att("b2", "тз.PDF", 10), att("b3", "ТЗ.pdf", 11));
        let none = HashMap::new();
        assert!(same_file(&a, &b, &none), "имя без регистра и размер");
        assert!(!same_file(&a, &c, &none), "другой размер");
        assert!(same_file(&a, &att("b1", "другое.pdf", 99), &none), "тот же блоб");
        let x = Hash::of(b"x");
        let hashes = HashMap::from([("b1".to_string(), x), ("b2".to_string(), Hash::of(b"y")), ("b3".to_string(), x)]);
        assert!(!same_file(&a, &b, &hashes), "разные байты при тех же имени и размере");
        assert!(same_file(&a, &c, &hashes), "те же байты под другим блобом и размером");
        assert_eq!(file_key(&a, &hashes), FileKey::Content(x));
        assert_eq!(file_key(&a, &none), FileKey::Blob("b1".into()));
        assert_eq!(file_key(&att("", " Отчёт ", 5), &none), FileKey::NameSize("отчёт".into(), 5));
        assert!(!same_file(&att("", "", 5), &att("", "", 5), &none), "без имени и блоба — не признак");
    }

    #[test]
    fn groups_merge_and_never_join_different_hashes() {
        let mut g = Groups::default();
        let x = Hash::of(b"x");
        assert_eq!(g.add(&att("b1", "ТЗ.pdf", 10), Some(x)), (0, true));
        assert_eq!(g.add(&att("b2", "ТЗ.pdf", 10), Some(x)), (0, false), "те же байты");
        assert_eq!(g.add(&att("b3", "ТЗ.pdf", 10), None), (0, false), "байтов нет — по имени и размеру");
        assert_eq!(g.add(&att("b4", "ТЗ.pdf", 10), Some(Hash::of(b"y"))), (1, true), "другие байты");
        assert_eq!(g.add(&att("b3", "x.bin", 1), None), (0, false), "тот же блоб");
        assert_eq!(g.key(0, &att("b1", "ТЗ.pdf", 10)), FileKey::Content(x));
        assert_eq!(g.add(&att("", "Акт.pdf", 3), None), (2, true));
        assert_eq!(g.key(2, &att("", "Акт.pdf", 3)), FileKey::NameSize("акт.pdf".into(), 3));
    }
}
