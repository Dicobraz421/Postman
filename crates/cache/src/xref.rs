//! Связи между беседами, сообщениями и людьми (ADR 0012 п. 9, 11): ссылки и обратные ссылки (`xref`), связанные
//! беседы (`conv_link`), адрес письма по `Message-ID` (`by_message_id`), участник канала = контакт (`person_link`).
//! Таблицы добавочные — создаются при открытии, формат остаётся 7, миграции нет.
//!
//! Хранилище знает только строки ключей: что такое адрес цели и источника, как связан человек — знает ядро
//! (`postman_core::channel::xref`, `linked`, `person`).
//!
//! - `xref` — `(цель, источник)`; к нему `xref_from` — `(источник, цель)`: заменить ссылки источника целиком при
//!   новой версии тела и убрать их, когда источника нет.
//! - `conv_link` — `(беседа, беседа)` в обе стороны.
//! - `by_message_id` — `(Message-ID без <>, письмо)`: пишется вместе с телом ([`Cache::put_body`]); тела, записанные до
//!   таблицы, дописывает [`Cache::index_message_ids`] в фоне.
//! - `person_link` — `(канал, учётка, участник)` → значение ядра (контакт, как связано).

use redb::{ReadableDatabase, ReadableTable, TableDefinition, WriteTransaction};

use crate::letter::{LETTER, LetterId};
use crate::remote::REMOTE;
use crate::{BODY, Cache, MAX_ID, Result, unscoped};

/// (цель, источник): обратные ссылки — кто сослался на цель.
const XREF: TableDefinition<(&str, &str), ()> = TableDefinition::new("xref");
/// (источник, цель): ссылки источника — заменить и убрать целиком.
const XREF_FROM: TableDefinition<(&str, &str), ()> = TableDefinition::new("xref_from");
/// (беседа, беседа) в обе стороны: связанные беседы.
const CONV_LINK: TableDefinition<(&str, &str), ()> = TableDefinition::new("conv_link");
/// (`Message-ID` без `<>`, письмо): адрес почтового сообщения.
const BY_MESSAGE_ID: TableDefinition<(&str, u64), ()> = TableDefinition::new("by_message_id");
/// (канал, учётка, участник) → значение ядра: один человек во всех каналах.
const PERSON_LINK: TableDefinition<(&str, &str, &str), &[u8]> = TableDefinition::new("person_link");

/// Ключ `person_link`: канал, учётка, участник.
pub type PersonKey<'a> = (&'a str, &'a str, &'a str);
/// Строка `person_link`: (канал, учётка, участник) и значение ядра.
pub type PersonRow = ((String, String, String), Vec<u8>);

/// Ключ ссылки тела письма из содержимого (`postman_core::store::letter_ref`): `#<LetterId>`.
const LETTER_REF: char = '#';

pub(crate) fn init(tx: &WriteTransaction) -> Result<()> {
    tx.open_table(XREF)?;
    tx.open_table(XREF_FROM)?;
    tx.open_table(CONV_LINK)?;
    tx.open_table(BY_MESSAGE_ID)?;
    tx.open_table(PERSON_LINK)?;
    Ok(())
}

/// `Message-ID` для индекса: без `<>` и пробелов; пусто — `None`.
pub fn bare_message_id(id: &str) -> Option<&str> {
    let s = id.trim().trim_start_matches('<').trim_end_matches('>').trim();
    (!s.is_empty()).then_some(s)
}

/// Письмо ключа тела: ссылка `#<LetterId>` или ключ кэша `[<учётка>/]<id>` по действующей связи.
fn letter_of_body_key(tx: &WriteTransaction, key: &str) -> Result<Option<u64>> {
    if let Some(n) = key.strip_prefix(LETTER_REF).and_then(|n| n.parse::<u64>().ok()) {
        return Ok(tx.open_table(LETTER)?.get(n)?.map(|_| n));
    }
    let (account, id) = unscoped(key);
    Ok(tx.open_table(REMOTE)?.get((account.unwrap_or(""), id))?.map(|v| v.value()))
}

/// Тело письма `key` с `Message-ID` записано — в индекс этой же транзакцией. Письма ещё нет — индекс допишет
/// [`Cache::index_message_ids`].
pub(crate) fn index_body(tx: &WriteTransaction, key: &str, message_id: Option<&str>) -> Result<()> {
    let Some(mid) = message_id.and_then(bare_message_id) else { return Ok(()) };
    if let Some(letter) = letter_of_body_key(tx, key)? {
        tx.open_table(BY_MESSAGE_ID)?.insert((mid, letter), ())?;
    }
    Ok(())
}

/// Строки с первой частью `first`: `(first, "")..` пока совпадает.
fn seconds<T: ReadableTable<(&'static str, &'static str), ()>>(t: &T, first: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for r in t.range((first, "")..=(first, MAX_ID))? {
        out.push(r?.0.value().1.to_owned());
    }
    Ok(out)
}

/// Ключи с первой частью, начинающейся с `prefix`.
fn with_prefix<T: ReadableTable<(&'static str, &'static str), ()>>(t: &T, prefix: &str) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for r in t.range((prefix, "")..)? {
        let (k, _) = r?;
        let (a, b) = k.value();
        if !a.starts_with(prefix) {
            break;
        }
        out.push((a.to_owned(), b.to_owned()));
    }
    Ok(out)
}

fn drop_sources(tx: &WriteTransaction, sources: &[String]) -> Result<()> {
    let mut from = tx.open_table(XREF_FROM)?;
    let mut to = tx.open_table(XREF)?;
    for s in sources {
        for t in seconds(&from, s)? {
            from.remove((s.as_str(), t.as_str()))?;
            to.remove((t.as_str(), s.as_str()))?;
        }
    }
    Ok(())
}

impl Cache {
    // --- xref ---

    /// Ссылки источника `source` — ровно `targets` (прежние уходят) одной транзакцией.
    pub fn xref_set(&self, source: &str, targets: &[String]) -> Result<()> {
        let tx = self.db.begin_write()?;
        drop_sources(&tx, &[source.to_owned()])?;
        {
            let mut from = tx.open_table(XREF_FROM)?;
            let mut to = tx.open_table(XREF)?;
            for t in targets.iter().filter(|t| !t.is_empty() && t.as_str() != source) {
                from.insert((source, t.as_str()), ())?;
                to.insert((t.as_str(), source), ())?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Источников больше нет (сообщение удалено, письмо ушло): их ссылки — из таблиц.
    pub fn xref_drop(&self, sources: &[String]) -> Result<()> {
        if sources.is_empty() {
            return Ok(());
        }
        let tx = self.db.begin_write()?;
        drop_sources(&tx, sources)?;
        tx.commit()?;
        Ok(())
    }

    /// Ссылки всех источников с началом `prefix` («Выйти», беседа ушла). Ответ — сколько источников.
    pub fn xref_drop_prefix(&self, prefix: &str) -> Result<usize> {
        let tx = self.db.begin_write()?;
        let sources: Vec<String> = {
            let mut s: Vec<String> = with_prefix(&tx.open_table(XREF_FROM)?, prefix)?.into_iter().map(|(s, _)| s).collect();
            s.dedup();
            s
        };
        drop_sources(&tx, &sources)?;
        tx.commit()?;
        Ok(sources.len())
    }

    /// Кто сослался на цель `target`.
    pub fn xref_sources(&self, target: &str) -> Result<Vec<String>> {
        let tx = self.db.begin_read()?;
        seconds(&tx.open_table(XREF)?, target)
    }

    /// На что ссылается источник `source`.
    pub fn xref_targets(&self, source: &str) -> Result<Vec<String>> {
        let tx = self.db.begin_read()?;
        seconds(&tx.open_table(XREF_FROM)?, source)
    }

    // --- conv_link ---

    /// Связать беседы `a` и `b` (в обе стороны). `false` — уже связаны или это одна беседа.
    pub fn conv_link(&self, a: &str, b: &str) -> Result<bool> {
        if a == b {
            return Ok(false);
        }
        let tx = self.db.begin_write()?;
        let new = {
            let mut t = tx.open_table(CONV_LINK)?;
            let new = t.insert((a, b), ())?.is_none();
            t.insert((b, a), ())?;
            new
        };
        tx.commit()?;
        Ok(new)
    }

    /// Развязать беседы. `false` — связи не было.
    pub fn conv_unlink(&self, a: &str, b: &str) -> Result<bool> {
        let tx = self.db.begin_write()?;
        let was = {
            let mut t = tx.open_table(CONV_LINK)?;
            let was = t.remove((a, b))?.is_some();
            t.remove((b, a))?;
            was
        };
        tx.commit()?;
        Ok(was)
    }

    /// Беседы, прямо связанные с `a`.
    pub fn conv_links(&self, a: &str) -> Result<Vec<String>> {
        let tx = self.db.begin_read()?;
        seconds(&tx.open_table(CONV_LINK)?, a)
    }

    /// Связи всех бесед с началом `prefix` («Выйти», беседа ушла). Ответ — сколько связей снято.
    pub fn conv_unlink_prefix(&self, prefix: &str) -> Result<usize> {
        let tx = self.db.begin_write()?;
        let n = {
            let mut t = tx.open_table(CONV_LINK)?;
            let pairs = with_prefix(&t, prefix)?;
            for (a, b) in &pairs {
                t.remove((a.as_str(), b.as_str()))?;
                t.remove((b.as_str(), a.as_str()))?;
            }
            pairs.len()
        };
        tx.commit()?;
        Ok(n)
    }

    // --- by_message_id ---

    /// Письма с этим `Message-ID` (с `<>` или без); строки писем, которых больше нет, пропускаются.
    pub fn letters_by_message_id(&self, message_id: &str) -> Result<Vec<LetterId>> {
        let Some(mid) = bare_message_id(message_id) else { return Ok(Vec::new()) };
        let tx = self.db.begin_read()?;
        let (t, letters) = (tx.open_table(BY_MESSAGE_ID)?, tx.open_table(LETTER)?);
        let mut out = Vec::new();
        for r in t.range((mid, 0)..=(mid, u64::MAX))? {
            let n = r?.0.value().1;
            if letters.get(n)?.is_some() {
                out.push(LetterId(n));
            }
        }
        Ok(out)
    }

    /// Дописать в индекс `Message-ID` тела, записанные до него: до `limit` тел после ключа `after` (`None` — с начала).
    /// Ответ — последний пройденный ключ; `None` — тела кончились.
    pub fn index_message_ids(&self, after: Option<&str>, limit: usize) -> Result<Option<String>> {
        let keys: Vec<(String, Option<String>)> = {
            let tx = self.db.begin_read()?;
            let t = tx.open_table(BODY)?;
            let range = match after {
                Some(a) => t.range(a..)?,
                None => t.iter()?,
            };
            let mut out = Vec::new();
            for r in range {
                let (k, v) = r?;
                if after == Some(k.value()) {
                    continue;
                }
                let mid = crate::content::unpack_body(v.value()).ok().and_then(|b| b.message_id);
                out.push((k.value().to_owned(), mid));
                if out.len() >= limit {
                    break;
                }
            }
            out
        };
        let Some(last) = keys.last().map(|(k, _)| k.clone()) else { return Ok(None) };
        let tx = self.db.begin_write()?;
        for (k, mid) in &keys {
            index_body(&tx, k, mid.as_deref())?;
        }
        tx.commit()?;
        Ok(Some(last))
    }

    // --- person_link ---

    /// Связь участника канала с контактом: значение ядра; `None` — убрать.
    pub fn person_link_set(&self, rows: &[(PersonKey, Option<&[u8]>)]) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut t = tx.open_table(PERSON_LINK)?;
            for (key, value) in rows {
                match value {
                    Some(v) => {
                        t.insert(*key, *v)?;
                    }
                    None => {
                        t.remove(*key)?;
                    }
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn person_link(&self, channel: &str, account: &str, participant: &str) -> Result<Option<Vec<u8>>> {
        let tx = self.db.begin_read()?;
        Ok(tx.open_table(PERSON_LINK)?.get((channel, account, participant))?.map(|v| v.value().to_vec()))
    }

    /// Все связи: (канал, учётка, участник) и значение ядра.
    pub fn person_links(&self) -> Result<Vec<PersonRow>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(PERSON_LINK)?;
        let mut out = Vec::new();
        for r in t.iter()? {
            let (k, v) = r?;
            let (c, a, p) = k.value();
            out.push(((c.to_owned(), a.to_owned(), p.to_owned()), v.value().to_vec()));
        }
        Ok(out)
    }

    /// Связи людей учётки канала — «Выйти». Ответ — сколько снято.
    pub fn person_unlink_account(&self, channel: &str, account: &str) -> Result<usize> {
        let tx = self.db.begin_write()?;
        let n = {
            let mut t = tx.open_table(PERSON_LINK)?;
            let keys: Vec<String> = {
                let mut out = Vec::new();
                for r in t.range((channel, account, "")..=(channel, account, MAX_ID))? {
                    out.push(r?.0.value().2.to_owned());
                }
                out
            };
            for p in &keys {
                t.remove((channel, account, p.as_str()))?;
            }
            keys.len()
        };
        tx.commit()?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BodyRow, EmailRow};

    fn row(id: &str) -> EmailRow {
        EmailRow {
            id: id.into(),
            thread_id: "t".into(),
            mailbox_ids: vec!["in".into()],
            received_at: 1,
            subject: String::new(),
            from_name: String::new(),
            from_addr: "anna@example.test".into(),
            preview: String::new(),
            seen: true,
            flagged: false,
            has_attachment: false,
            size: 0,
            keywords: Vec::new(),
        }
    }

    /// Ссылки источника заменяются целиком; обратные — по цели; источник и префикс уходят со всеми ссылками.
    #[test]
    fn xref_replaces_and_drops() {
        let c = Cache::open_in_memory().unwrap();
        c.xref_set("postman://demo/me/g/1", &["mid:a@x".into(), "postman://demo/me/g/2".into()]).unwrap();
        c.xref_set("letter:7", &["mid:a@x".into(), "letter:7".into()]).unwrap();
        assert_eq!(c.xref_sources("mid:a@x").unwrap(), ["letter:7", "postman://demo/me/g/1"]);
        assert!(c.xref_targets("letter:7").unwrap() == ["mid:a@x"], "на себя — не ссылка");
        c.xref_set("postman://demo/me/g/1", &["postman://demo/me/g/3".into()]).unwrap();
        assert_eq!(c.xref_sources("mid:a@x").unwrap(), ["letter:7"]);
        assert!(c.xref_sources("postman://demo/me/g/2").unwrap().is_empty());
        assert_eq!(c.xref_drop_prefix("postman://demo/me/").unwrap(), 1);
        assert!(c.xref_sources("postman://demo/me/g/3").unwrap().is_empty());
        c.xref_drop(&["letter:7".into()]).unwrap();
        assert!(c.xref_sources("mid:a@x").unwrap().is_empty());
    }

    /// Связь бесед — в обе стороны, повтор и сама с собой — нет; префикс снимает обе стороны.
    #[test]
    fn conv_links_both_ways() {
        let c = Cache::open_in_memory().unwrap();
        assert!(c.conv_link("postman://mail/me/t1", "postman://demo/me/g").unwrap());
        assert!(!c.conv_link("postman://demo/me/g", "postman://mail/me/t1").unwrap());
        assert!(!c.conv_link("a", "a").unwrap());
        assert_eq!(c.conv_links("postman://demo/me/g").unwrap(), ["postman://mail/me/t1"]);
        assert_eq!(c.conv_unlink_prefix("postman://demo/").unwrap(), 1);
        assert!(c.conv_links("postman://mail/me/t1").unwrap().is_empty());
        assert!(!c.conv_unlink("a", "b").unwrap());
    }

    /// `Message-ID` — вместе с телом (ключ кэша и ссылка `#n`); тела до таблицы — дописываются; ушедшее письмо — нет.
    #[test]
    fn message_ids_follow_bodies() {
        let c = Cache::open_in_memory().unwrap();
        c.upsert_emails(&[row("e1"), row("e2")]).unwrap();
        c.put_body("e1", &BodyRow { message_id: Some("<m1@example.test>".into()), ..BodyRow::default() }).unwrap();
        let l1 = c.letters_by_message_id("m1@example.test").unwrap();
        assert_eq!(l1.len(), 1);
        assert_eq!(c.letters_by_message_id("<m1@example.test>").unwrap(), l1);
        // Тело без индекса (записано до таблицы): индекс стёрт — проход дописывает.
        {
            let tx = c.db.begin_write().unwrap();
            tx.open_table(BY_MESSAGE_ID).unwrap().retain(|_, _| false).unwrap();
            tx.commit().unwrap();
        }
        assert!(c.letters_by_message_id("m1@example.test").unwrap().is_empty());
        let mut after = None;
        while let Some(k) = c.index_message_ids(after.as_deref(), 1).unwrap() {
            after = Some(k);
        }
        assert_eq!(c.letters_by_message_id("m1@example.test").unwrap(), l1);
        c.put_body(&format!("#{}", l1[0].0), &BodyRow { message_id: Some("m9@example.test".into()), ..BodyRow::default() }).unwrap();
        assert_eq!(c.letters_by_message_id("m9@example.test").unwrap(), l1);
        c.delete_emails(&["e1".into()]).unwrap();
        let gone = c.letter(l1[0]).unwrap().is_none();
        assert_eq!(c.letters_by_message_id("m1@example.test").unwrap().is_empty(), gone, "письма нет — нет и в ответе");
    }

    /// Связи людей: запись, снятие, по учётке.
    #[test]
    fn person_links_by_account() {
        let c = Cache::open_in_memory().unwrap();
        c.person_link_set(&[(("demo", "me", "u-anna"), Some(b"a".as_slice())), (("demo", "me", "u-boris"), Some(b"b".as_slice()))]).unwrap();
        c.person_link_set(&[(("telegram", "1", "42"), Some(b"a".as_slice()))]).unwrap();
        assert_eq!(c.person_link("demo", "me", "u-anna").unwrap().as_deref(), Some(b"a".as_slice()));
        assert_eq!(c.person_links().unwrap().len(), 3);
        c.person_link_set(&[(("demo", "me", "u-boris"), None)]).unwrap();
        assert_eq!(c.person_unlink_account("demo", "me").unwrap(), 1);
        assert_eq!(c.person_links().unwrap().len(), 1);
    }
}
