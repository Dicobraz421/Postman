//! Время встречи в строке списка вместо превью (T2-18, сверка макетов T2-G): у письма-приглашения — «Чт, 23 октября ·
//! 11:00–12:00» со значком календаря, у отменённой — «Отменена · …».
//!
//! Откуда время: `.ics` разбирает воркер — когда письмо открыли (`ReadingPane::parse_invite`) или в фоне для строк
//! окна списка (`ReadingPane::want_meetings`: байты — из хранилища, демо или командой `Blob`). Итог — в хранилище
//! (`Cache::set_state`, ключ [`key`]), список читает его вместе со строками ([`read`]) и больше не спрашивает.

use std::collections::HashMap;

use jiff::tz::TimeZone;
use pets_i18n::tr;
use postman_cache::{Cache, EmailRow};
use postman_calendar::{Invitation, Span};

/// Встреча письма: время, весь день, отменена ли.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct When {
    pub(crate) span: Span,
    pub(crate) all_day: bool,
    pub(crate) cancelled: bool,
}

impl When {
    pub(crate) fn of(inv: &Invitation) -> Self {
        Self { span: inv.event.span, all_day: inv.event.all_day, cancelled: inv.method.eq_ignore_ascii_case("CANCEL") }
    }

    /// Текст строки списка.
    pub(crate) fn text(&self, tz: &TimeZone) -> String {
        let when = crate::invite_card::when_line(self.span, self.all_day, tz);
        if self.cancelled { tr!("list-meeting-cancelled", when = when) } else { when }
    }
}

/// Ключ в хранилище: встреча письма `id`.
pub(crate) fn key(id: &str) -> String {
    format!("invite-when:{id}")
}

/// Значение в хранилище: «начало конец весь-день отменена»; `-` — во вложении не встреча (поручение, ошибка разбора).
pub(crate) fn encode(when: Option<&When>) -> String {
    match when {
        Some(w) => format!("{} {} {} {}", w.span.start, w.span.end, u8::from(w.all_day), u8::from(w.cancelled)),
        None => "-".into(),
    }
}

/// `None` — значение не разобрать; `Some(None)` — не встреча.
pub(crate) fn decode(s: &str) -> Option<Option<When>> {
    if s == "-" {
        return Some(None);
    }
    let mut it = s.split(' ');
    let start = it.next()?.parse().ok()?;
    let end = it.next()?.parse().ok()?;
    let all_day = it.next()? == "1";
    let cancelled = it.next()? == "1";
    Some(Some(When { span: Span::new(start, end), all_day, cancelled }))
}

/// Встречи строк окна (из хранилища) и строки без записи (кроме шифрованных). По `has_attachment` не отбираем: у писем
/// встреч Exchange его нет, у IMAP — когда `text/calendar` в `multipart/alternative`.
pub(crate) fn read(cache: &Cache, rows: &[EmailRow]) -> (HashMap<String, When>, Vec<EmailRow>) {
    let mut known = HashMap::new();
    let mut unknown = Vec::new();
    for r in rows.iter().filter(|r| !crate::smime_enc::is_encrypted(r)) {
        match cache.state(&key(&r.id)).ok().flatten().as_deref().and_then(decode) {
            Some(Some(w)) => {
                known.insert(r.id.clone(), w);
            }
            Some(None) => {}
            None => unknown.push(r.clone()),
        }
    }
    (known, unknown)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn when_round_trip() {
        let w = When { span: Span::new(1_700_000_000, 1_700_003_600), all_day: false, cancelled: true };
        assert_eq!(decode(&encode(Some(&w))), Some(Some(w)));
        assert_eq!(decode(&encode(None)), Some(None));
        assert_eq!(decode("мусор"), None);
    }

    #[test]
    fn read_splits_known_and_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("c.redb")).unwrap();
        let row = |id: &str, att: bool| EmailRow {
            id: id.into(),
            thread_id: id.into(),
            mailbox_ids: vec!["inbox".into()],
            received_at: 0,
            subject: String::new(),
            from_name: String::new(),
            from_addr: String::new(),
            preview: String::new(),
            seen: true,
            flagged: false,
            has_attachment: att,
            size: 0,
            keywords: Vec::new(),
        };
        let w = When { span: Span::new(100, 200), all_day: false, cancelled: false };
        cache.set_state(&key("a"), &encode(Some(&w))).unwrap();
        cache.set_state(&key("b"), &encode(None)).unwrap();
        let rows = [row("a", true), row("b", true), row("c", true), row("d", false)];
        let (known, unknown) = read(&cache, &rows);
        assert_eq!(known.get("a"), Some(&w));
        assert_eq!(known.len(), 1, "«не встреча» не показывается");
        assert_eq!(unknown.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["c", "d"]);
    }
}
