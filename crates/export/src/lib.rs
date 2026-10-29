//! Выгрузка писем в файлы для других клиентов (S8): EML, mbox, Maildir, MSG, PST. Принимает байты писем и
//! отдаёт байты файлов; без I/O, интерфейса не знает.
//!
//! EML, mbox и Maildir — обрамление байтов письма без разбора: пишут в `Write`, пути файлов дают `Tree` и
//! `maildir::Maildir`; создание каталогов и файлов — у вызывающего.

pub mod eml;
pub mod maildir;
pub mod mbox;
pub mod tree;

pub use tree::{Layout, Tree, safe_name};

/// Флаги письма: Maildir хранит их в имени файла; EML и mbox — нет (`X-Mozilla-Status` не пишем).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Flags {
    pub seen: bool,
    pub answered: bool,
    pub flagged: bool,
    pub draft: bool,
}

/// Письмо для выгрузки: байты как есть и данные из строки письма в базе (разбирать байты не нужно).
#[derive(Debug, Clone, Copy)]
pub struct Letter<'a> {
    /// Письмо целиком (RFC 5322), переводы строк — как пришли.
    pub raw: &'a [u8],
    /// Время письма, секунды Unix (UTC).
    pub date: i64,
    /// Сдвиг местного времени от UTC на дату письма, секунды — для имён EML.
    pub offset: i32,
    /// Адрес отправителя (`user@host`) — для строки `From ` в mbox.
    pub from: &'a str,
    pub subject: &'a str,
    pub flags: Flags,
    /// Путь папки в учётке от корня: `["Входящие", "Проекты"]`; путь на диске — `Tree::folder`.
    pub folder: &'a [String],
}

/// Дата и время по секундам Unix, без часовых поясов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Civil {
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    /// 0 — воскресенье.
    weekday: u32,
}

impl Civil {
    fn from_unix(secs: i64) -> Self {
        let days = secs.div_euclid(86_400);
        let rest = secs.rem_euclid(86_400) as u32;
        // Дни от 1970-01-01 → год, месяц, день (H. Hinnant, `civil_from_days`).
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        Self {
            year: yoe + era * 400 + i64::from(month <= 2),
            month,
            day,
            hour: rest / 3600,
            minute: rest / 60 % 60,
            second: rest % 60,
            // 1970-01-01 — четверг.
            weekday: (days + 4).rem_euclid(7) as u32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        let c = Civil::from_unix(0);
        assert_eq!((c.year, c.month, c.day, c.weekday), (1970, 1, 1, 4));
        // 2024-02-29 23:59:59 UTC, четверг.
        let c = Civil::from_unix(1_709_251_199);
        assert_eq!((c.year, c.month, c.day, c.hour, c.minute, c.second, c.weekday), (2024, 2, 29, 23, 59, 59, 4));
        // 1969-12-31 23:00:00 UTC, среда.
        let c = Civil::from_unix(-3600);
        assert_eq!((c.year, c.month, c.day, c.hour, c.weekday), (1969, 12, 31, 23, 3));
    }
}
