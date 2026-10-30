//! Демо-ящик формата 6 (S8-14, ADR 0006): группа «На этом компьютере», одно вложение в письмах трёх учёток — одна
//! запись части, задания переноса в каждом состоянии для снимков.
//!
//! Путь выгрузки в демо подменный: сети нет, итог его работы (письма у цели, строка задания) пишется прямо в
//! хранилище. Движок ядра на таком ящике ничего не выгружает: команды путям уходят в никуда, задание стоит в
//! том состоянии, в каком его оставил снимок.

use std::sync::OnceLock;

use postman_cache::{Cache, EmailRow, Encoding, FolderId, JobId, JobRow, LetterRow, MigrationMode, MigrationState, Owner, SplitPart, scoped};

use crate::{EXCHANGE, ME, ME_NAME, PREVIEW, SENDERS, STANDARDS, SUBJECTS};

/// Локальная папка с архивом (`place = Local`).
pub const ARCHIVE: &str = "Архив 2024";
/// Писем в [`ARCHIVE`].
pub const ARCHIVE_LETTERS: usize = 1200;
/// Вторая локальная папка.
pub const PROJECTS: &str = "Проекты";
/// Писем в [`PROJECTS`].
pub const PROJECTS_LETTERS: usize = 24;

/// 2024-01-01T00:00:00Z: письма архива — в пределах 2024 года.
const YEAR_2024: i64 = 1_704_067_200;
const YEAR: i64 = 366 * 86_400;

/// Общее вложение: одно и то же в письмах трёх учёток — у основной в «Проектах», у «стандартов» и Exchange во
/// «Входящих».
pub const SHARED_PDF: &str = "Сводка за 2024.pdf";
/// Блоб общего вложения ([`crate::blob`]).
pub const SHARED_PDF_BLOB: &str = "shared-pdf";
/// Писем с общим вложением в каждой учётке.
pub const SHARED_PER_ACCOUNT: usize = 10;
/// Учётки (префикс ключей) и папка писем с общим вложением.
const SHARED_IN: [(Option<&str>, &str); 3] = [(None, "projects"), (STANDARDS.profile, "inbox"), (EXCHANGE.profile, "inbox")];

/// Ключ письма `i` с общим вложением в учётке `profile`.
pub fn shared_pdf_letter(profile: Option<&str>, i: usize) -> String {
    scoped(profile, &format!("share{i}"))
}

/// Папки «На этом компьютере» с письмами и письма с общим вложением; уже есть — ничего (шаблон старше S8-14 —
/// дописать).
pub(crate) fn fill(cache: &Cache, now: i64) -> postman_cache::Result<()> {
    if cache.folders()?.iter().any(|(_, f)| f.owner == Owner::Local && f.name == ARCHIVE) {
        return Ok(());
    }
    local(cache)?;
    shared_pdf(cache, now)
}

fn local(cache: &Cache) -> postman_cache::Result<()> {
    let archive = cache.create_folder(None, ARCHIVE)?;
    let rows = (0..ARCHIVE_LETTERS).map(|i| {
        let (name, addr) = SENDERS[i % SENDERS.len()];
        LetterRow {
            thread_id: format!("local-t{}", i / 2),
            received_at: YEAR_2024 + YEAR * i as i64 / ARCHIVE_LETTERS as i64,
            subject: format!("{} {}", SUBJECTS[i % SUBJECTS.len()], i % 997),
            from_name: name.into(),
            from_addr: addr.into(),
            preview: PREVIEW.into(),
            seen: true,
            flagged: i.is_multiple_of(97),
            keywords: crate::demo_keywords(i),
            ..LetterRow::default()
        }
    });
    put_local(cache, archive, &rows.collect::<Vec<_>>())?;
    let projects = cache.create_folder(None, PROJECTS)?;
    let rows = (0..PROJECTS_LETTERS).map(|i| {
        let (name, addr) = SENDERS[(i + 1) % SENDERS.len()];
        LetterRow {
            thread_id: format!("local-p{}", i / 3),
            received_at: YEAR_2024 + YEAR - (i as i64 + 1) * 5 * 86_400,
            subject: format!("{} {}", PROJECT_SUBJECTS[i % PROJECT_SUBJECTS.len()], i + 1),
            from_name: name.into(),
            from_addr: addr.into(),
            preview: PREVIEW.into(),
            seen: true,
            ..LetterRow::default()
        }
    });
    put_local(cache, projects, &rows.collect::<Vec<_>>())
}

const PROJECT_SUBJECTS: &[&str] = &["Офис на Лесной: смета", "Пилот SIEM: протокол", "Пропуска: список", "Переезд склада"];

/// Учётка, через которую заводятся локальные письма демо; её связи и строки заданий в ящике не остаются.
const STAGE: &str = "";
const STAGE_FOLDER: &str = "inbox";

/// Письма `rows` — в локальную папку `folder`, `place = Local`, без связей с сервером. Писем без сервера хранилище
/// само не заводит (их даёт перенос), поэтому — как после переноса, у которого источник уже удалил письма: письма
/// учётки → перенос в `folder` (задание) → источник удалил (связей нет) → задание отменено (`Local` в прежней
/// папке) → снова в `folder` (без связей — сразу `Local`). Шесть транзакций на все письма. Письмо только в базе —
/// с содержимым (взять его больше негде): оно — по транзакции на письмо.
fn put_local(cache: &Cache, folder: FolderId, rows: &[LetterRow]) -> postman_cache::Result<()> {
    let stage = cache.folder_of(STAGE, STAGE_FOLDER)?.ok_or_else(|| missing(format!("folder {STAGE_FOLDER} of account '{STAGE}'")))?;
    let mut keyed = Vec::with_capacity(rows.len());
    let mut raws = Vec::with_capacity(rows.len());
    for (i, r) in rows.iter().enumerate() {
        let key = format!("local-{}-{i}", folder.0);
        let raw = local_raw(&key, r);
        keyed.push((key, LetterRow { folders: vec![stage], size: raw.len() as u64, ..r.clone() }));
        raws.push(raw);
    }
    let ids = cache.upsert_remotes(STAGE, &keyed)?;
    let job = cache.move_letters(&ids, folder)?.job;
    cache.delete_remote(STAGE, &keyed.into_iter().map(|(k, _)| k).collect::<Vec<_>>())?;
    if let Some(job) = job {
        cache.restore(job, &ids)?;
        cache.drop_job(job)?;
    }
    cache.move_letters(&ids, folder)?;
    for (id, raw) in ids.into_iter().zip(raws) {
        cache.put_content(id, &raw, &[])?;
    }
    Ok(())
}

/// Письмо локальной папки целиком: текст без вложений, ключ `key` — в `Message-ID`.
fn local_raw(key: &str, r: &LetterRow) -> Vec<u8> {
    let date = jiff::Timestamp::from_second(r.received_at).unwrap_or(jiff::Timestamp::UNIX_EPOCH);
    [
        format!("From: =?utf-8?B?{}?= <{}>", base64(r.from_name.as_bytes(), 0), r.from_addr),
        format!("To: {ME}"),
        format!("Subject: =?utf-8?B?{}?=", base64(r.subject.as_bytes(), 0)),
        format!("Date: {}", date.strftime("%a, %d %b %Y %H:%M:%S +0000")),
        format!("Message-ID: <{key}@corp.example>"),
        "MIME-Version: 1.0".into(),
        "Content-Type: text/plain; charset=utf-8".into(),
        "Content-Transfer-Encoding: 8bit".into(),
        String::new(),
        r.preview.clone(),
        String::new(),
    ]
    .join("\r\n")
    .into_bytes()
}

fn missing(what: String) -> postman_cache::Error {
    std::io::Error::other(format!("demo mailbox has no {what}")).into()
}

/// Письма с общим вложением: тело — своё у каждого (разные заголовки), вложение — одни и те же байты. Даты — дни
/// назад: старше писем дополнительных учёток, в основной учётке — в своей папке.
fn shared_pdf(cache: &Cache, now: i64) -> postman_cache::Result<()> {
    for (n, &(profile, folder)) in SHARED_IN.iter().enumerate() {
        let at = |i: usize| now - (3 + i as i64) * 86_400 - 3600 * n as i64;
        let rows: Vec<EmailRow> = (0..SHARED_PER_ACCOUNT)
            .map(|i| {
                let (name, addr) = shared_from(n, i);
                EmailRow {
                    id: shared_pdf_letter(profile, i),
                    thread_id: shared_pdf_letter(profile, i),
                    mailbox_ids: vec![scoped(profile, folder)],
                    received_at: at(i),
                    subject: shared_subject(n, i),
                    from_name: name.into(),
                    from_addr: addr.into(),
                    preview: SHARED_TEXT.into(),
                    seen: true,
                    flagged: false,
                    has_attachment: true,
                    size: shared_raw(n, i, at(i)).0.len() as u64,
                    keywords: Vec::new(),
                }
            })
            .collect();
        cache.upsert_emails(&rows)?;
        let account = profile.unwrap_or("");
        for i in 0..SHARED_PER_ACCOUNT {
            let key = format!("share{i}");
            let id = cache.letter_of(account, &key)?.ok_or_else(|| missing(format!("letter {key} of account '{account}'")))?;
            let (raw, part) = shared_raw(n, i, at(i));
            cache.put_content(id, &raw, &[part])?;
        }
    }
    Ok(())
}

const SHARED_TOPICS: &[&str] = &["финансы", "для руководства", "к совещанию", "правки", "итог"];

/// Отправитель письма `i` с общим вложением в учётке `n` из [`SHARED_IN`].
fn shared_from(n: usize, i: usize) -> (&'static str, &'static str) {
    SENDERS[(i + n) % SENDERS.len()]
}

fn shared_subject(n: usize, i: usize) -> String {
    format!("Сводка за 2024 год — {}", SHARED_TOPICS[(i + n) % SHARED_TOPICS.len()])
}
const SHARED_TEXT: &str = "Коллеги, сводка за 2024 год — во вложении. Замечания — до пятницы.";
pub(crate) const SHARED_HTML: &str = "<p>Коллеги, сводка за 2024 год — во вложении.</p><p>Замечания — до пятницы.</p>";

const BOUNDARY: &str = "demo-share";

/// Письмо `i` учётки `n` из [`SHARED_IN`] (пришло в `at`) с общим вложением целиком и часть вложения для
/// [`Cache::put_content`]: разметку письма демо строит само, разбирать нечего.
fn shared_raw(n: usize, i: usize, at: i64) -> (Vec<u8>, SplitPart) {
    let profile = SHARED_IN[n].0;
    let (name, addr) = shared_from(n, i);
    let to = crate::account(profile).map_or(ME, |a| a.email);
    let date = jiff::Timestamp::from_second(at).unwrap_or(jiff::Timestamp::UNIX_EPOCH);
    let head = [
        format!("From: {name} <{addr}>"),
        format!("To: {ME_NAME} <{to}>"),
        format!("Subject: =?utf-8?B?{}?=", base64(shared_subject(n, i).as_bytes(), 0)),
        format!("Date: {}", date.strftime("%a, %d %b %Y %H:%M:%S +0000")),
        format!("Message-ID: <share{i}.{}@corp.example>", profile.map_or("main", |p| p.trim_start_matches('@'))),
        "MIME-Version: 1.0".into(),
        format!("Content-Type: multipart/mixed; boundary=\"{BOUNDARY}\""),
        String::new(),
        format!("--{BOUNDARY}"),
        "Content-Type: text/plain; charset=utf-8".into(),
        "Content-Transfer-Encoding: 8bit".into(),
        String::new(),
        SHARED_TEXT.into(),
        format!("--{BOUNDARY}"),
        "Content-Type: application/pdf".into(),
        format!("Content-Disposition: attachment; filename*=utf-8''{}", percent(SHARED_PDF)),
        "Content-Transfer-Encoding: base64".into(),
        String::new(),
        String::new(),
    ]
    .join("\r\n");
    let body = base64(pdf(), 76);
    let raw = format!("{head}{body}--{BOUNDARY}--\r\n").into_bytes();
    let part = SplitPart { range: head.len()..head.len() + body.len(), encoding: Encoding::Base64, decoded: pdf().to_vec() };
    (raw, part)
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// base64 строками по `width` знаков, каждая с `\r\n`; `width = 0` — одной строкой без перевода.
fn base64(data: &[u8], width: usize) -> String {
    let mut s = String::with_capacity(data.len() * 4 / 3 + 4);
    for c in data.chunks(3) {
        let n = u32::from(c[0]) << 16 | u32::from(c.get(1).copied().unwrap_or(0)) << 8 | u32::from(c.get(2).copied().unwrap_or(0));
        for k in 0..4 {
            s.push(if k <= c.len() { B64[(n >> (18 - 6 * k) & 63) as usize] as char } else { '=' });
        }
    }
    if width == 0 {
        return s;
    }
    s.as_bytes().chunks(width).flat_map(|l| l.iter().map(|&b| b as char).chain("\r\n".chars())).collect()
}

/// Имя файла для `filename*` (RFC 2231): байты UTF-8 вне букв и цифр ASCII — `%XX`.
fn percent(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b == b'.' || b == b'-' { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// Страниц в общем вложении: PDF больше [`postman_cache::MIN_PART`] — хранится частью.
const PDF_PAGES: usize = 30;

/// Общее вложение (PDF): сводная таблица на [`PDF_PAGES`] страниц. Собирается один раз при первом запросе.
pub fn pdf() -> &'static [u8] {
    static PDF: OnceLock<Vec<u8>> = OnceLock::new();
    PDF.get_or_init(make_pdf)
}

fn make_pdf() -> Vec<u8> {
    let mut objects: Vec<Vec<u8>> = Vec::new();
    let kids: Vec<String> = (0..PDF_PAGES).map(|p| format!("{} 0 R", 4 + 2 * p)).collect();
    objects.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
    objects.push(format!("<< /Type /Pages /Kids [{}] /Count {PDF_PAGES} >>", kids.join(" ")).into_bytes());
    objects.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec());
    for p in 0..PDF_PAGES {
        objects.push(
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>", 5 + 2 * p)
                .into_bytes(),
        );
        let mut text = format!("BT /F1 16 Tf 56 790 Td (Summary 2024, page {} of {PDF_PAGES}) Tj ET\n", p + 1);
        for line in 0..60 {
            let amount = (p * 60 + line) * 7919 % 100_000;
            text.push_str(&format!(
                "BT /F1 9 Tf 56 {} Td (Item {:02}-{line:02}   cost centre {:03}   {amount:>6}.00) Tj ET\n",
                760 - 12 * line,
                p + 1,
                line * 7 % 113
            ));
        }
        objects.push(format!("<< /Length {} >>\nstream\n{text}endstream", text.len()).into_bytes());
    }
    let mut out = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (n, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", n + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for off in offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
    out
}

/// Задание переноса для снимка (ADR 0006 п. 5): в каком состоянии его застаёт сцена.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobShot {
    /// Вопрос над списком папки: «Перенести на сервер» или «Оставить локальным архивом».
    Asking,
    /// Идёт: [`JOB_DONE`] из [`JOB_TOTAL`].
    Running,
    Paused,
    /// На сервере цели нет места.
    QuotaFull,
    /// Отменено: письма ещё в задании — движок вернёт их в папку источника и покажет итог.
    Cancelled,
}

impl JobShot {
    pub const ALL: [JobShot; 5] = [JobShot::Asking, JobShot::Running, JobShot::Paused, JobShot::QuotaFull, JobShot::Cancelled];

    pub fn state(self) -> MigrationState {
        match self {
            JobShot::Asking => MigrationState::Asking,
            JobShot::Running => MigrationState::Running,
            JobShot::Paused => MigrationState::Paused,
            JobShot::QuotaFull => MigrationState::QuotaFull,
            JobShot::Cancelled => MigrationState::Cancelled,
        }
    }
}

/// Писем в задании переноса демо.
pub const JOB_TOTAL: usize = 12_480;
/// Уже у цели (кроме [`JobShot::Asking`]).
pub const JOB_DONE: usize = 3_120;
/// Размер письма задания: перенесённые [`JOB_DONE`] — около 1,2 ГБ.
pub const JOB_LETTER_SIZE: u64 = 412_977;
/// Откуда: «Входящие» Exchange.
pub const JOB_SOURCE: (Option<&str>, &str) = (EXCHANGE.profile, "inbox");
/// Куда: «Архив» основной учётки (Stalwart; у неё есть квота — «нет места» возможно).
pub const JOB_TARGET: (Option<&str>, &str) = (None, "archive");

/// Задание переноса в состоянии `shot` — на свежей копии демо-ящика (ключи писем задания постоянные). Письма
/// источника — в его папке и сразу перенесены в папку цели (`Outbound`, связь с источником `detached`), уже
/// выгруженные — у цели (`Synced`); строка задания — режим, состояние, всего писем, байт выгружено.
pub fn migration(cache: &Cache, shot: JobShot) -> postman_cache::Result<JobId> {
    let folder = |(profile, remote): (Option<&str>, &str)| {
        let account = profile.unwrap_or("");
        cache.folder_of(account, remote)?.ok_or_else(|| missing(format!("folder {remote} of account '{account}'")))
    };
    let (source, target) = (folder(JOB_SOURCE)?, folder(JOB_TARGET)?);
    let done = if shot == JobShot::Asking { 0 } else { JOB_DONE };
    let row = |i: usize, folder: FolderId| {
        let (name, addr) = SENDERS[i % SENDERS.len()];
        LetterRow {
            thread_id: format!("move-t{}", i / 2),
            received_at: YEAR_2024 + YEAR * i as i64 / JOB_TOTAL as i64,
            subject: format!("{} {}", SUBJECTS[i % SUBJECTS.len()], i % 997),
            from_name: name.into(),
            from_addr: addr.into(),
            preview: PREVIEW.into(),
            seen: true,
            has_attachment: i.is_multiple_of(5),
            size: JOB_LETTER_SIZE,
            folders: vec![folder],
            ..LetterRow::default()
        }
    };
    let moved: Vec<(String, LetterRow)> = (0..done).map(|i| (format!("move{i}"), row(i, target))).collect();
    cache.upsert_remotes(JOB_TARGET.0.unwrap_or(""), &moved)?;
    let waiting: Vec<(String, LetterRow)> = (done..JOB_TOTAL).map(|i| (format!("move{i}"), row(i, source))).collect();
    let ids = cache.upsert_remotes(JOB_SOURCE.0.unwrap_or(""), &waiting)?;
    let job = cache.move_letters(&ids, target)?.job.ok_or_else(|| missing("migration job".into()))?;
    let mode = if shot == JobShot::Asking { MigrationMode::Ask } else { MigrationMode::Upload };
    cache.save_job(&JobRow { id: job, mode, state: shot.state(), total: JOB_TOTAL as u64, bytes: done as u64 * JOB_LETTER_SIZE })?;
    Ok(job)
}

#[cfg(test)]
mod tests {
    use postman_cache::{MIN_PART, Place, Scope};

    use super::*;

    fn dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("postman-demo-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn local_folder(cache: &Cache, name: &str) -> FolderId {
        cache.folders().unwrap().into_iter().find(|(_, f)| f.owner == Owner::Local && f.name == name).unwrap().0
    }

    /// «На этом компьютере»: «Архив 2024» и «Проекты» — письма только в базе, без связей; счётчики сходятся;
    /// повторный `ensure` ничего не дописывает.
    #[test]
    fn local_folders_and_counts() {
        let dir = dir("local");
        let path = dir.join("demo.redb");
        crate::ensure(&path, 50).unwrap();
        crate::ensure(&path, 50).unwrap();
        let cache = Cache::open(&path).unwrap();
        let locals: Vec<String> = cache.folders().unwrap().into_iter().filter(|(_, f)| f.owner == Owner::Local).map(|(_, f)| f.name).collect();
        assert_eq!(locals, [ARCHIVE, PROJECTS]);
        let (archive, projects) = (local_folder(&cache, ARCHIVE), local_folder(&cache, PROJECTS));
        assert_eq!((cache.folder_count(archive).unwrap(), cache.folder_count(projects).unwrap()), (ARCHIVE_LETTERS, PROJECTS_LETTERS));
        let letters = cache.folder_page(archive, 0, ARCHIVE_LETTERS + 1).unwrap();
        assert!(letters.iter().all(|(id, r)| r.place == Place::Local && r.folders == [archive] && cache.remotes(*id).unwrap().is_empty()));
        let (id, row) = &letters[0];
        let raw = cache.letter_content(*id).unwrap().expect("письмо только в базе — с содержимым");
        assert_eq!(row.size, raw.len() as u64);
        let (first, last) = (letters.last().unwrap().1.received_at, letters[0].1.received_at);
        assert!(first >= YEAR_2024 && last < YEAR_2024 + YEAR, "письма архива — 2024 года");
        assert!(cache.keyword_count_in("pm.cat.team", &Scope::All(Owner::Local)).unwrap() > 0);
        // Временных связей и заданий не осталось; основная учётка письма архива не видит.
        assert!(cache.jobs().unwrap().is_empty());
        assert!(cache.letter_of(STAGE, &format!("local-{}-0", archive.0)).unwrap().is_none());
        assert_eq!(cache.check_index().unwrap(), Vec::<String>::new());
        assert_eq!(cache.check_refs().unwrap(), Vec::<String>::new());
        drop(cache);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Письма с общим вложением в файле: с делением на части (как пишет демо) или без него — размер файла.
    fn shared_file(dir: &std::path::Path, split: bool) -> u64 {
        let path = dir.join(format!("shared-{split}.redb"));
        let cache = Cache::open(&path).unwrap();
        cache.replace_mailboxes(None, &crate::main_boxes()).unwrap();
        crate::extra_accounts(&cache, 0).unwrap();
        if split {
            shared_pdf(&cache, 1_792_000_000).unwrap();
        } else {
            for (n, (profile, folder)) in SHARED_IN.into_iter().enumerate() {
                let account = profile.unwrap_or("");
                let f = cache.folder_of(account, folder).unwrap().unwrap();
                for i in 0..SHARED_PER_ACCOUNT {
                    let id = cache.upsert_remote(account, &format!("share{i}"), &LetterRow { folders: vec![f], ..LetterRow::default() }).unwrap();
                    cache.put_content(id, &shared_raw(n, i, 1_792_000_000).0, &[]).unwrap();
                }
            }
        }
        drop(cache);
        std::fs::metadata(&path).unwrap().len()
    }

    /// Общее вложение — в 30 письмах трёх учёток: содержимое у каждого своё (письмо целиком — ровно те байты, что
    /// собрало демо), часть — одна: без деления на части те же письма занимают в файле в разы больше.
    #[test]
    fn one_part_for_thirty_letters() {
        assert!(pdf().len() >= MIN_PART && pdf().starts_with(b"%PDF-") && pdf().ends_with(b"%%EOF\n"));
        let dir = dir("shared");
        let path = dir.join("demo.redb");
        crate::ensure(&path, 50).unwrap();
        let cache = Cache::open(&path).unwrap();
        let mut hashes = Vec::new();
        for (n, (profile, _)) in SHARED_IN.into_iter().enumerate() {
            for i in 0..SHARED_PER_ACCOUNT {
                let key = shared_pdf_letter(profile, i);
                let (account, remote) = postman_cache::unscoped(&key);
                let id = cache.letter_of(account.unwrap_or(""), remote).unwrap().unwrap();
                let row = cache.letter(id).unwrap().unwrap();
                let (raw, part) = shared_raw(n, i, row.received_at);
                assert_eq!((row.subject, row.from_addr.as_str()), (shared_subject(n, i), shared_from(n, i).1));
                assert_eq!(cache.letter_content(id).unwrap().unwrap(), raw, "{key}");
                assert_eq!((part.decoded.as_slice(), row.size, row.has_attachment), (pdf(), raw.len() as u64, true));
                hashes.push(row.content.unwrap());
                let b = crate::body(&key);
                assert_eq!((b.attachments[0].blob_id.as_str(), b.attachments[0].size), (SHARED_PDF_BLOB, pdf().len() as u64));
            }
        }
        hashes.sort();
        hashes.dedup();
        assert_eq!(hashes.len(), 30, "письма разные");
        assert_eq!(crate::blob(SHARED_PDF_BLOB).as_deref(), Some(pdf()));
        assert_eq!(cache.check_refs().unwrap(), Vec::<String>::new());
        drop(cache);

        let (split, whole) = (shared_file(&dir, true), shared_file(&dir, false));
        let pdf = pdf().len() as u64;
        assert!(whole - split > 20 * pdf, "одна часть: {split} байт против {whole} без частей (PDF {pdf})");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Демо-ящик с папками основной учётки и Exchange — в памяти.
    fn boxes() -> Cache {
        let cache = Cache::open_in_memory().unwrap();
        cache.replace_mailboxes(None, &crate::main_boxes()).unwrap();
        crate::extra_accounts(&cache, 0).unwrap();
        cache
    }

    /// Задание переноса в каждом состоянии: строка задания, письма в задании и у цели.
    #[test]
    fn jobs_by_state() {
        for shot in JobShot::ALL {
            let cache = boxes();
            let job = migration(&cache, shot).unwrap();
            let row = cache.job(job).unwrap().unwrap();
            let done = if shot == JobShot::Asking { 0 } else { JOB_DONE };
            assert_eq!((row.state, row.total, row.bytes), (shot.state(), JOB_TOTAL as u64, done as u64 * JOB_LETTER_SIZE), "{shot:?}");
            assert_eq!(row.mode, if shot == JobShot::Asking { MigrationMode::Ask } else { MigrationMode::Upload });
            let outbound = cache.outbound(job).unwrap();
            assert_eq!(outbound.len(), JOB_TOTAL - done, "{shot:?}");
            let target = cache.folder_of("", "archive").unwrap().unwrap();
            let source = cache.folder_of("@3", "inbox").unwrap().unwrap();
            assert!(outbound.iter().all(|(_, o)| o.to == target && o.from == [source]));
            assert_eq!(cache.folder_count(target).unwrap(), JOB_TOTAL);
            assert_eq!(cache.letter(outbound[0].0).unwrap().unwrap().place, Place::Outbound(job));
            assert_eq!(cache.check_index().unwrap(), Vec::<String>::new());
        }
        // «Идёт»: 3 120 из 12 480, около 1,2 ГБ.
        assert_eq!((JOB_DONE, JOB_TOTAL), (3_120, 12_480));
        let gb = JOB_DONE as f64 * JOB_LETTER_SIZE as f64 / f64::from(1u32 << 30);
        assert!((gb - 1.2).abs() < 0.01, "{gb}");
    }
}
