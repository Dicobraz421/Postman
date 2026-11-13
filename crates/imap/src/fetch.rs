//! Заголовки писем порциями: одна `UID FETCH` на сотни писем — тяжёлая команда. Яндекс на ней отвечает
//! `NO [UNAVAILABLE] FETCH Backend error` или не успевает за [`crate::TIMEOUT`]; одно письмо, ответ на которое
//! `imap-proto` не разбирает, роняет всю команду. Тогда папка оставалась пустой при каждой сверке. Здесь — план
//! порций: по [`CHUNK`] писем, порция с отказом — пополам (и следующие — не больше половины), одно письмо с
//! отказом — без `BODYSTRUCTURE`, затем только флаги, дата и размер; не вышло и так — письмо пропускается. Отказов
//! больше [`MAX_FAILURES`] — сервер не отвечает вовсе, сверка папки прекращается.

use std::collections::VecDeque;

use crate::Error;

/// Писем в первой порции.
pub const CHUNK: usize = 100;
/// Отказов на одну выборку — не больше.
pub const MAX_FAILURES: usize = 32;

/// Что просить у сервера о письме.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fields {
    /// Всё для списка: флаги, дата, размер, ENVELOPE, BODYSTRUCTURE (значок вложения), строки цепочки.
    Full,
    /// Без BODYSTRUCTURE: её сервер строит разбором письма — самая тяжёлая и самая капризная часть ответа.
    NoStructure,
    /// Только флаги, дата и размер: письмо в списке есть, тема и отправитель — пустые.
    Bare,
}

impl Fields {
    /// Список полей `UID FETCH` (`modseq` — CONDSTORE включён).
    pub fn query(self, modseq: bool) -> String {
        let m = if modseq { " MODSEQ" } else { "" };
        match self {
            Fields::Full => {
                format!("(UID FLAGS INTERNALDATE RFC822.SIZE ENVELOPE BODYSTRUCTURE{m} BODY.PEEK[HEADER.FIELDS (IN-REPLY-TO REFERENCES)])")
            }
            Fields::NoStructure => format!("(UID FLAGS INTERNALDATE RFC822.SIZE ENVELOPE{m} BODY.PEEK[HEADER.FIELDS (IN-REPLY-TO REFERENCES)])"),
            Fields::Bare => format!("(UID FLAGS INTERNALDATE RFC822.SIZE{m})"),
        }
    }

    fn lighter(self) -> Option<Fields> {
        match self {
            Fields::Full => Some(Fields::NoStructure),
            Fields::NoStructure => Some(Fields::Bare),
            Fields::Bare => None,
        }
    }
}

/// План порций одной выборки.
#[derive(Debug)]
pub struct Batches {
    rest: VecDeque<u32>,
    retry: VecDeque<(Vec<u32>, Fields)>,
    size: usize,
    failures: usize,
    skipped: Vec<u32>,
}

impl Batches {
    pub fn new(uids: &[u32]) -> Batches {
        Batches { rest: uids.iter().copied().collect(), retry: VecDeque::new(), size: CHUNK, failures: 0, skipped: Vec::new() }
    }

    /// Следующая порция: сначала — разделённые после отказа, затем — новые.
    pub fn next_batch(&mut self) -> Option<(Vec<u32>, Fields)> {
        if let Some(b) = self.retry.pop_front() {
            return Some(b);
        }
        let n = self.size.min(self.rest.len());
        (n > 0).then(|| (self.rest.drain(..n).collect(), Fields::Full))
    }

    /// Порция не получена. `false` — отказов слишком много, выборку прекратить.
    pub fn failed(&mut self, uids: Vec<u32>, fields: Fields) -> bool {
        self.failures += 1;
        if self.failures > MAX_FAILURES {
            return false;
        }
        if uids.len() > 1 {
            let half = uids.len().div_ceil(2);
            self.size = self.size.min(half);
            let (a, b) = uids.split_at(half);
            self.retry.push_front((b.to_vec(), fields));
            self.retry.push_front((a.to_vec(), fields));
        } else {
            match fields.lighter() {
                Some(l) => self.retry.push_front((uids, l)),
                None => self.skipped.extend(uids),
            }
        }
        true
    }

    /// Письма, которых сервер не отдал и в самом простом виде.
    pub fn skipped(&self) -> &[u32] {
        &self.skipped
    }
}

/// Отказ порции, после которого стоит попросить меньше: `NO`/`BAD` сервера, неразобранный ответ, тайм-аут, обрыв
/// (Яндекс закрывает соединение посреди тяжёлой выборки). Вход и TLS — нет: меньшая порция не поможет.
pub fn retry_smaller(e: &Error) -> bool {
    matches!(e, Error::Imap(async_imap::error::Error::No(_) | async_imap::error::Error::Bad(_)) | Error::Protocol(_) | Error::Timeout | Error::Io(_))
}

/// После отказа соединение не годится: ответ прерван посередине (тайм-аут, обрыв) или в буфере неразобранные байты.
pub fn drops_session(e: &Error) -> bool {
    matches!(e, Error::Protocol(_) | Error::Timeout | Error::Io(_))
}

/// Ответ, который `imap-proto` не разобрал (`async-imap` отдаёт его как `io::Error` с байтами ответа в тексте), — без
/// этих байтов: в них чужое письмо, ему не место в журнале и тосте.
pub fn unparsed(e: Error) -> Error {
    match e {
        Error::Io(io) if io.kind() == std::io::ErrorKind::Other && io.to_string().contains("during parsing") => {
            Error::Protocol("unparsable FETCH response".into())
        }
        e => e,
    }
}

#[cfg(test)]
mod tests {
    use super::{Batches, CHUNK, Fields, MAX_FAILURES, drops_session, retry_smaller, unparsed};
    use crate::Error;

    /// Полученные письма с полями, пропущенные, размеры порций по порядку.
    type Outcome = (Vec<(u32, Fields)>, Vec<u32>, Vec<usize>);

    /// Выборка поддельным сервером: `fails(порция, поля)` — отказ; `None` — выборка прекращена.
    fn run(uids: &[u32], fails: impl Fn(&[u32], Fields) -> bool) -> Option<Outcome> {
        let mut b = Batches::new(uids);
        let (mut got, mut sizes) = (Vec::new(), Vec::new());
        while let Some((set, fields)) = b.next_batch() {
            sizes.push(set.len());
            if fails(&set, fields) {
                if !b.failed(set, fields) {
                    return None;
                }
            } else {
                got.extend(set.iter().map(|&u| (u, fields)));
            }
        }
        Some((got, b.skipped().to_vec(), sizes))
    }

    #[test]
    fn healthy_server_gets_chunks() {
        let uids: Vec<u32> = (1..=250).collect();
        let (got, skipped, sizes) = run(&uids, |_, _| false).unwrap();
        assert_eq!(sizes, [CHUNK, CHUNK, 50]);
        assert_eq!(got.len(), 250);
        assert!(got.iter().all(|(_, f)| *f == Fields::Full) && skipped.is_empty());
        assert_eq!(run(&[], |_, _| false).unwrap().0, []);
    }

    /// Яндекс: «Backend error» на порции больше 30 писем — порции мельчают, письма все и целиком.
    #[test]
    fn big_batches_shrink() {
        let uids: Vec<u32> = (1..=500).collect();
        let (got, skipped, sizes) = run(&uids, |set, _| set.len() > 30).unwrap();
        assert_eq!(got.iter().map(|(u, _)| *u).collect::<Vec<_>>(), uids, "все письма, по порядку");
        assert!(got.iter().all(|(_, f)| *f == Fields::Full) && skipped.is_empty());
        assert_eq!(sizes.iter().filter(|&&s| s > 30).count(), 3, "отказы — только 100 и две половины по 50: {sizes:?}");
    }

    /// Одно письмо не разбирается с BODYSTRUCTURE — остальные целиком, оно — без неё; другое не отдаётся вовсе —
    /// пропущено.
    #[test]
    fn one_bad_letter_does_not_empty_the_folder() {
        let uids: Vec<u32> = (1..=500).collect();
        let (got, skipped, _) = run(&uids, |set, f| (set.contains(&137) && f == Fields::Full) || set.contains(&400)).unwrap();
        assert_eq!(got.len(), 499);
        assert!(got.contains(&(137, Fields::NoStructure)));
        assert!(got.iter().filter(|(u, _)| *u != 137).all(|(_, f)| *f == Fields::Full));
        assert_eq!(skipped, [400]);
    }

    /// Сервер отказывает во всём — выборка прекращается, а не перебирает письма по одному.
    #[test]
    fn dead_server_gives_up() {
        let uids: Vec<u32> = (1..=500).collect();
        let mut calls = 0;
        let mut b = Batches::new(&uids);
        while let Some((set, f)) = b.next_batch() {
            calls += 1;
            if !b.failed(set, f) {
                break;
            }
        }
        assert_eq!(calls, MAX_FAILURES + 1);
    }

    /// Записанные отказы: `NO [UNAVAILABLE]` Яндекса, неразобранный ответ, обрыв — порцию меньше; вход — нет.
    #[test]
    fn which_errors_shrink() {
        let no = Error::Imap(async_imap::error::Error::No("[UNAVAILABLE] FETCH Backend error. sc=abc".into()));
        assert!(retry_smaller(&no) && !drops_session(&no));
        assert!(retry_smaller(&Error::Timeout) && drops_session(&Error::Timeout));
        let io = std::io::Error::other("Error(Tag) during parsing of \"* 7 FETCH (UID 9 ENVELOPE (\\\"секрет\\\"…\"");
        let parsed = unparsed(Error::from(async_imap::error::Error::Io(io)));
        assert!(matches!(&parsed, Error::Protocol(m) if !m.contains("секрет")), "{parsed}");
        assert!(retry_smaller(&parsed) && drops_session(&parsed));
        assert!(!retry_smaller(&Error::Auth("x".into())));
        // Яндекс закрыл соединение посреди тяжёлой выборки: порция меньше, соединение заново.
        let reset = unparsed(Error::from(async_imap::error::Error::Io(std::io::Error::from(std::io::ErrorKind::ConnectionReset))));
        assert!(matches!(reset, Error::Io(_)) && retry_smaller(&reset) && drops_session(&reset));
        assert!(!retry_smaller(&Error::Tls("x".into())));
    }

    #[test]
    fn queries() {
        assert_eq!(
            Fields::Full.query(true),
            "(UID FLAGS INTERNALDATE RFC822.SIZE ENVELOPE BODYSTRUCTURE MODSEQ BODY.PEEK[HEADER.FIELDS (IN-REPLY-TO REFERENCES)])"
        );
        assert_eq!(
            Fields::NoStructure.query(false),
            "(UID FLAGS INTERNALDATE RFC822.SIZE ENVELOPE BODY.PEEK[HEADER.FIELDS (IN-REPLY-TO REFERENCES)])"
        );
        assert_eq!(Fields::Bare.query(false), "(UID FLAGS INTERNALDATE RFC822.SIZE)");
    }
}
