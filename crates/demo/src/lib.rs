//! Синтетический ящик для замеров памяти/FPS без сервера.

pub mod smime;
pub mod store;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use postman_cache::{AccountRow, Addr, AttachmentRow, BodyRow, Cache, EmailRow, MailboxRow, Recipients, Rights, scoped};

/// Отправители демо-писем (имя, адрес); их же берёт заполнение ящика стенда (`postman-backend-jmap`, пример `fill_stand`).
pub const SENDERS: &[(&str, &str)] = &[
    ("Иванова Мария", "m.ivanova@corp.example"),
    ("Петров Алексей", "a.petrov@corp.example"),
    ("Служба поддержки", "support@vendor.example"),
    ("Сидоров Игорь", "i.sidorov@corp.example"),
    ("GitLab", "noreply@gitlab.corp.example"),
    ("Бухгалтерия", "buh@corp.example"),
];

/// Начало текста демо-писем (оно же краткое содержание в списке).
pub const PREVIEW: &str = "Коллеги, добрый день! Направляю материалы к обсуждению, прошу посмотреть до пятницы.";

/// Владелец демо-ящика: участник демо-приглашений и календаря.
pub const ME: &str = "me@corp.example";

/// Начало письма с упоминанием владельца (S5-03, фильтр «Упоминания»).
pub const MENTION: &str = "@Анна, посмотрите, пожалуйста, раздел рисков до отправки заказчику.";

/// Рассылка отдела: письма ей — не «Мне» (S5-03).
const TEAM: &str = "team@corp.example";

/// Темы демо-писем.
pub const SUBJECTS: &[&str] = &[
    "Отчёт за квартал",
    "Re: Совещание по проекту",
    "Счёт на оплату №",
    "Пропуск на парковку",
    "Pipeline failed",
    "Fwd: Договор — правки юристов",
    "Обновление регламента",
];

/// Каталог тестовых данных Postman (H-01): `%LOCALAPPDATA%\Pets\testdata\postman` — общий для приложений Pets,
/// не во временном каталоге; чистится `pets/scripts/stand/clean.ps1 -Testdata`.
pub fn testdata_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_DATA_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Pets")
        .join("testdata")
        .join("postman")
}

/// Свежий ящик для `--bench`: копия шаблона `bench-template-<n>.redb` (создаётся один раз и переиспользуется:
/// сид фиксирован) в `bench.redb`. Бенч помечает письма прочитанными; без свежей копии каждый следующий прогон
/// делал бы меньше работы. Рабочую копию удаляет бенч после прогона (`bench::remove_on_finish`).
pub fn bench_copy(dir: &Path, n: usize) -> postman_cache::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let template = dir.join(format!("bench-template-{n}.redb"));
    ensure(&template, n)?;
    let path = dir.join("bench.redb");
    std::fs::copy(&template, &path)?;
    Ok(path)
}

/// Вложенные папки основной учётки (T1-04е): (ключ, родитель, имя), `""` — корень дерева. До трёх уровней под
/// «Входящими» и под своей папкой «Проекты» (у неё нет роли).
const MAIN_NESTED: &[(&str, &str, &str)] = &[
    ("inbox-clients", "inbox", "Клиенты"),
    ("inbox-clients-north", "inbox-clients", "Северный филиал"),
    ("inbox-clients-north-contracts", "inbox-clients-north", "Договоры"),
    ("inbox-lists", "inbox", "Рассылки"),
    ("projects", "", "Проекты"),
    ("projects-office", "projects", "Офис на Лесной"),
    ("projects-office-estimate", "projects-office", "Смета"),
    ("projects-office-estimate-2026", "projects-office-estimate", "2026"),
    ("projects-passes", "projects", "Пропуска"),
];

/// Вложенные папки учётки Exchange (T1-04е): под «Входящими» и под своей папкой «Командировки».
const EXCHANGE_NESTED: &[(&str, &str, &str)] = &[
    ("inbox-buh", "inbox", "Бухгалтерия"),
    ("inbox-buh-reports", "inbox-buh", "Отчёты"),
    ("inbox-buh-reports-q3", "inbox-buh-reports", "III квартал"),
    ("trips", "", "Командировки"),
    ("trips-advance", "trips", "Авансовые отчёты"),
];

/// Строки вложенных папок учётки `scope`: ключи и родители — с префиксом учётки, роли нет; порядок — после
/// системных папок, в порядке таблицы.
fn nested(scope: Option<&str>, table: &[(&str, &str, &str)]) -> Vec<MailboxRow> {
    table
        .iter()
        .zip(100..)
        .map(|(&(id, parent, name), order)| MailboxRow {
            id: scoped(scope, id),
            parent_id: (!parent.is_empty()).then(|| scoped(scope, parent)),
            name: name.into(),
            role: None,
            sort_order: order,
            total: 0,
            unread: 0,
            rights: Rights::ALL,
        })
        .collect()
}

/// Папки основной учётки: системные и вложенные.
fn main_boxes() -> Vec<MailboxRow> {
    let mb = |id: &str, name: &str, order| MailboxRow {
        id: id.into(),
        parent_id: None,
        name: name.into(),
        role: Some(id.into()),
        sort_order: order,
        total: 0,
        unread: 0,
        rights: Rights::ALL,
    };
    let mut boxes = vec![
        mb("inbox", "Входящие", 1),
        mb("drafts", "Черновики", 2),
        mb("sent", "Отправленные", 3),
        mb("trash", "Удалённые", 4),
        mb("archive", "Архив", 5),
    ];
    boxes.extend(nested(None, MAIN_NESTED));
    boxes
}

pub fn ensure(path: &Path, n: usize) -> postman_cache::Result<()> {
    let cache = Cache::open(path)?;
    if cache.count("inbox").unwrap_or(0) + cache.count("archive").unwrap_or(0) >= n {
        // Шаблон старше беседы демо (S5-05) — дописать её.
        if cache.rows_by_ids(&[CHAIN[0].id.to_string()])?.is_empty() {
            chain(&cache, jiff::Timestamp::now().as_second())?;
        }
        // …и писем-поручений (S6-11).
        if cache.rows_by_ids(&[delegations::letter_id(1)])?.is_empty() {
            delegations::letters(&cache, jiff::Timestamp::now().as_second())?;
        }
        // …вложенных папок (T1-04е)…
        let boxes = cache.mailboxes()?;
        if !boxes.iter().any(|m| m.id == MAIN_NESTED[0].0) {
            cache.replace_mailboxes(None, &main_boxes())?;
        }
        // …и учёток «стандарты» и Exchange (S7-09а) с их вложенными папками.
        let exchange_nested = scoped(EXCHANGE.profile, EXCHANGE_NESTED[0].0);
        if !cache.accounts()?.iter().any(|a| Some(a.id.as_str()) == EXCHANGE.profile) || !boxes.iter().any(|m| m.id == exchange_nested) {
            extra_accounts(&cache, jiff::Timestamp::now().as_second())?;
        }
        // …и «На этом компьютере» с общим вложением (S8-14).
        store::fill(&cache, jiff::Timestamp::now().as_second())?;
        return Ok(());
    }
    cache.replace_mailboxes(None, &main_boxes())?;
    let mut rng = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let now = jiff::Timestamp::now().as_second();
    let mut at = now;
    let mut batch = Vec::with_capacity(5000);
    let mut to = Vec::with_capacity(5000);
    for i in 0..n {
        at -= (next() % 3600) as i64;
        let (mut name, mut addr) = SENDERS[next() as usize % SENDERS.len()];
        let mut subject = SUBJECTS[next() as usize % SUBJECTS.len()];
        // Подписанные образцы (S5-12): отправитель и тема — как в подписанном письме.
        let signed = smime::sample_for(i);
        if let Some(s) = signed {
            (name, addr, subject) = (s.from_name, s.from_addr, s.subject);
        }
        let mailbox = match next() % 10 {
            0..=6 => "inbox",
            7 | 8 => "archive",
            _ => "sent",
        };
        batch.push(EmailRow {
            id: format!("demo{i}"),
            thread_id: format!("t{}", i / 3),
            mailbox_ids: vec![mailbox.into()],
            received_at: at,
            subject: if signed.is_some() { subject.to_string() } else { format!("{subject} {}", i % 997) },
            from_name: name.into(),
            from_addr: addr.into(),
            preview: match signed {
                // Шифрованное: сервер текста не видит.
                Some(s) if s.encrypted => String::new(),
                Some(s) => smime::preview(s),
                None if i % 13 == 2 => MENTION.into(),
                None => PREVIEW.into(),
            },
            seen: i > 40 && next() % 5 != 0,
            flagged: next() % 50 == 0,
            has_attachment: next() % 7 == 0 || signed.is_some(),
            size: 4096 + next() % 100_000,
            keywords: match signed {
                Some(s) if s.encrypted => {
                    let mut k = demo_keywords(i);
                    k.push(postman_cache::ENCRYPTED.into());
                    k.sort();
                    k
                }
                _ => demo_keywords(i),
            },
        });
        to.push((format!("demo{i}"), demo_recipients(i)));
        if batch.len() == batch.capacity() {
            cache.upsert_emails(&batch)?;
            cache.set_recipients(&to)?;
            batch.clear();
            to.clear();
        }
    }
    cache.upsert_emails(&batch)?;
    cache.set_recipients(&to)?;
    shared(&cache, now)?;
    extra_accounts(&cache, now)?;
    chain(&cache, now)?;
    delegations::letters(&cache, now)?;
    store::fill(&cache, now)
}

/// Ключевые слова демо-письма `i` (S5-02): категории по умолчанию (`postman_core::labels::DEFAULTS`) и метка
/// Thunderbird — список показывает полосы категорий и метки, у части писем — по две.
fn demo_keywords(i: usize) -> Vec<String> {
    let mut out: Vec<&str> = Vec::new();
    match i % 6 {
        0 => out.push("pm.cat.clients"),
        1 => out.push("pm.cat.team"),
        3 => out.push("pm.cat.tenders"),
        4 => out.push("pm.cat.pentest"),
        _ => {}
    }
    if i.is_multiple_of(8) {
        out.push("pm.cat.team");
    }
    if i % 10 == 7 {
        out.push("pm.cat.services");
    }
    if i % 17 == 5 {
        out.push("$label2");
    }
    // Быстрые фильтры (S5-03): угрозы и слова жизненного цикла (их поставит S5-04) — у немногих писем.
    if i % 97 == 13 {
        out.push(words::PHISHING);
    }
    if i % 23 == 4 {
        out.push(words::DUE);
    }
    if i % 29 == 6 {
        out.push(words::WAIT);
    }
    if i % 31 == 9 {
        out.push(words::EXPIRE);
    }
    let mut out: Vec<String> = out.into_iter().map(String::from).collect();
    out.sort();
    out.dedup();
    out
}

/// Получатели демо-письма `i`: чаще мне, иногда — рассылке отдела (а мне копия).
fn demo_recipients(i: usize) -> Recipients {
    if i % 3 == 1 { Recipients { to: vec![TEAM.into()], cc: vec![ME.into()] } } else { Recipients { to: vec![ME.into()], cc: Vec::new() } }
}

/// Ключевые слова фильтров (`postman_core::filters`): демо от ядра не зависит (слои), слова — те же.
mod words {
    pub const PHISHING: &str = "$phishing";
    pub const DUE: &str = "pm.due";
    pub const WAIT: &str = "pm.wait";
    pub const EXPIRE: &str = "pm.expire";
}

/// Письмо беседы демо (S5-05): цепочка ответов с цитатами разных клиентов, мои письма — в «Отправленных».
pub struct ChainMessage {
    pub id: &'static str,
    /// Часов назад.
    pub hours_ago: i64,
    pub mine: bool,
    pub subject: &'static str,
    html: Option<&'static str>,
    text: Option<&'static str>,
}

/// Беседа демо: письмо, ответ из Outlook (шапка «От: … Отправлено: …»), ответ из Gmail (`gmail_quote`), ответ
/// текстом с `>`. Снимок `thread` открывает третье.
pub const CHAIN: &[ChainMessage] = &[
    ChainMessage { id: "chain1", hours_ago: 50, mine: false, subject: "Пилот SIEM: сроки", html: Some(CHAIN_1), text: None },
    ChainMessage { id: "chain2", hours_ago: 48, mine: true, subject: "RE: Пилот SIEM: сроки", html: Some(CHAIN_2), text: None },
    ChainMessage { id: "chain3", hours_ago: 26, mine: false, subject: "Re: Пилот SIEM: сроки", html: Some(CHAIN_3), text: None },
    ChainMessage { id: "chain4", hours_ago: 2, mine: true, subject: "Re: Пилот SIEM: сроки", html: None, text: Some(CHAIN_4) },
];

/// Письмо беседы демо, открытое в снимке `thread`.
pub const CHAIN_OPEN: &str = "chain3";

/// Имя владельца демо-ящика (адрес — [`ME`]).
pub const ME_NAME: &str = "Смирнова Анна";

fn chain(cache: &Cache, now: i64) -> postman_cache::Result<()> {
    let (name, addr) = SENDERS[1];
    let rows: Vec<EmailRow> = CHAIN
        .iter()
        .map(|m| EmailRow {
            id: m.id.into(),
            thread_id: "chain".into(),
            mailbox_ids: vec![if m.mine { "sent" } else { "inbox" }.into()],
            received_at: now - m.hours_ago * 3600,
            subject: m.subject.into(),
            from_name: if m.mine { ME_NAME } else { name }.into(),
            from_addr: if m.mine { ME } else { addr }.into(),
            preview: String::new(),
            seen: true,
            flagged: false,
            has_attachment: false,
            size: 4096,
            keywords: Vec::new(),
        })
        .collect();
    cache.upsert_emails(&rows)?;
    let to = |m: &ChainMessage| if m.mine { addr } else { ME };
    cache.set_recipients(&CHAIN.iter().map(|m| (m.id.to_string(), Recipients { to: vec![to(m).into()], cc: Vec::new() })).collect::<Vec<_>>())
}

fn chain_body(id: &str) -> Option<BodyRow> {
    let m = CHAIN.iter().find(|m| m.id == id)?;
    let (name, addr) = SENDERS[1];
    let to = if m.mine { Addr { name: name.into(), email: addr.into() } } else { Addr { name: ME_NAME.into(), email: ME.into() } };
    Some(BodyRow {
        to: vec![to],
        message_id: Some(format!("{id}@corp.example")),
        html: m.html.map(Into::into),
        text: m.text.map(Into::into),
        ..Default::default()
    })
}

const CHAIN_1: &str = "<p>Анна, добрый день!</p><p>Предлагаю начать пилот SIEM 3 ноября. Нужны от вас:</p>
<ul><li>список источников событий;</li><li>контакт администратора.</li></ul><p>С уважением,<br>Алексей Петров</p>";

const CHAIN_2: &str = r#"<html><body><div class=WordSection1><p class=MsoNormal>Алексей, добрый день!</p>
<p class=MsoNormal>Источники пришлю завтра, администратор — Сидоров Игорь.</p><p class=MsoNormal>&nbsp;</p>
<p class=MsoNormal>С уважением,<br>Анна Смирнова</p>
<div style='border:none;border-top:solid #E1E1E1 1.0pt;padding:3.0pt 0cm 0cm 0cm'><p class=MsoNormal><b>От:</b> Петров Алексей
&lt;a.petrov@corp.example&gt;<br><b>Отправлено:</b> понедельник, 3 ноября 2026 г. 10:15<br><b>Кому:</b> Смирнова Анна<br>
<b>Тема:</b> Пилот SIEM: сроки</p></div><p class=MsoNormal>Анна, добрый день!</p>
<p class=MsoNormal>Предлагаю начать пилот SIEM 3 ноября.</p></div></body></html>"#;

const CHAIN_3: &str = r#"<div dir="ltr">Спасибо! Тогда стартуем 3-го, созвон в 10:00.</div><br>
<div class="gmail_quote"><div dir="ltr" class="gmail_attr">пн, 3 нояб. 2026 г. в 12:10, Смирнова Анна &lt;me@corp.example&gt;:<br></div>
<blockquote class="gmail_quote"><p>Алексей, добрый день!</p><p>Источники пришлю завтра, администратор — Сидоров Игорь.</p>
<p><b>От:</b> Петров Алексей<br><b>Отправлено:</b> понедельник, 3 ноября 2026 г. 10:15</p></blockquote></div>"#;

const CHAIN_4: &str = "Подтверждаю, до встречи.

Анна

04.11.2026 10:05, Петров Алексей пишет:
> Спасибо! Тогда стартуем 3-го, созвон в 10:00.
>
> пн, 3 нояб. 2026 г. в 12:10, Смирнова Анна <me@corp.example>:
>> Алексей, добрый день!
";

/// Вид сервера учётной записи демо (S7-09а): каким путём она ходит на сервер.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Stalwart по JMAP — основная учётка.
    Jmap,
    /// «Стандарты»: IMAP и SMTP, CalDAV и CardDAV.
    Standards,
    /// Exchange on-prem: почта по IMAP, календарь, занятость и справочник по EWS.
    Exchange,
}

/// Что дал сервер учётной записи демо — те же функции, что `postman_core::model::Feature` (демо от ядра не
/// зависит, слои): интерфейс переводит их в `Features` один к одному. `keywords` — сервер хранит ключевые слова
/// (категории, сроки, «Ждём»; у IMAP — `PERMANENTFLAGS \*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Has {
    pub calendar: bool,
    pub tasks: bool,
    pub contacts: bool,
    pub directory: bool,
    pub free_busy: bool,
    pub rules: bool,
    pub autoreply: bool,
    pub quota: bool,
    pub keywords: bool,
}

/// Учётная запись демо: префикс ключей в кэше (`None` — основная), адрес, вид и узел сервера, функции.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Account {
    pub profile: Option<&'static str>,
    pub email: &'static str,
    pub kind: Kind,
    pub host: &'static str,
    pub has: Has,
}

/// Основная учётная запись демо — Stalwart, всё есть.
pub const MAIN: Account = Account {
    profile: None,
    email: ME,
    kind: Kind::Jmap,
    host: "mail.corp.example",
    has: Has {
        calendar: true,
        tasks: true,
        contacts: true,
        directory: true,
        free_busy: true,
        rules: true,
        autoreply: true,
        quota: true,
        keywords: true,
    },
};

/// Учётка «стандарты» (S7-09а): IMAP + CalDAV/CardDAV — календарь со scheduling, задачи VTODO, контакты; справочника,
/// правил, автоответа и квоты нет (таблица S7 «Нет у сервера — нет на экране»).
pub const STANDARDS: Account = Account {
    profile: Some("@2"),
    email: "anna.smirnova@example.test",
    kind: Kind::Standards,
    host: "mail.example.test",
    has: Has {
        calendar: true,
        tasks: true,
        contacts: true,
        directory: false,
        free_busy: true,
        rules: false,
        autoreply: false,
        quota: false,
        keywords: true,
    },
};

/// Учётка Exchange (S7-09а; демо-учётка Exchange для снимков, не сделанная в S7-06г): почта, календарь, занятость,
/// справочник; без задач, контактов, правил, автоответа, квоты и категорий (ключевые слова не хранит).
pub const EXCHANGE: Account = Account {
    profile: Some("@3"),
    email: "a.smirnova@exchange.example.test",
    kind: Kind::Exchange,
    host: "exchange.example.test",
    has: Has {
        calendar: true,
        tasks: false,
        contacts: false,
        directory: true,
        free_busy: true,
        rules: false,
        autoreply: false,
        quota: false,
        keywords: false,
    },
};

/// Все учётные записи демо — в порядке дерева папок: основная, «стандарты», Exchange.
pub const ACCOUNTS: [Account; 3] = [MAIN, STANDARDS, EXCHANGE];

/// Учётная запись демо по префиксу ключей (`None` — основная).
pub fn account(profile: Option<&str>) -> Option<&'static Account> {
    ACCOUNTS.iter().find(|a| a.profile == profile)
}

/// Вторая учётная запись профиля демо (S7-03б) — она же «стандарты» ([`STANDARDS`]): префикс ключей и адрес — группа
/// в дереве папок, «От» письма и встречи, свой календарь в «Моих календарях».
pub const SECOND: (&str, &str) = ("@2", STANDARDS.email);

/// Папки дополнительной учётки: роль и имя у сервера — как их отдают такие серверы.
fn folders(kind: Kind) -> &'static [(&'static str, &'static str)] {
    match kind {
        Kind::Exchange => {
            &[("inbox", "Входящие"), ("drafts", "Черновики"), ("sent", "Отправленные"), ("trash", "Удалённые"), ("junk", "Нежелательная почта")]
        }
        _ => &[("inbox", "Входящие"), ("drafts", "Черновики"), ("sent", "Отправленные"), ("trash", "Корзина"), ("junk", "Спам")],
    }
}

/// Темы писем дополнительной учётки.
fn subjects(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Exchange => &["Согласование бюджета", "Re: Пропуск на объект", "Отчёт о командировке", "Обновление VPN"],
        _ => &["Договор с партнёром", "Re: График поставок", "Счёт № 3415", "Приглашение на выставку"],
    }
}

/// Папки и письма дополнительных учёток демо (`@2` «стандарты», `@3` Exchange) — каждая группой в дереве папок.
fn extra_accounts(cache: &Cache, now: i64) -> postman_cache::Result<()> {
    for (n, a) in ACCOUNTS.iter().enumerate() {
        let Some(profile) = a.profile else { continue };
        let scope = Some(profile);
        cache.replace_accounts_of(scope, &[AccountRow { id: profile.into(), name: a.email.into(), is_personal: true, read_only: false }])?;
        let mut boxes: Vec<MailboxRow> = folders(a.kind)
            .iter()
            .zip(1..)
            .map(|(&(role, name), order)| MailboxRow {
                id: scoped(scope, role),
                parent_id: None,
                name: name.into(),
                role: Some(role.into()),
                sort_order: order,
                total: 0,
                unread: 0,
                rights: Rights::ALL,
            })
            .collect();
        if a.kind == Kind::Exchange {
            boxes.extend(nested(scope, EXCHANGE_NESTED));
        }
        cache.replace_mailboxes(scope, &boxes)?;
        let subjects = subjects(a.kind);
        let rows: Vec<EmailRow> = (0..8)
            .map(|i| {
                let (name, addr) = SENDERS[(i + 3 * n) % SENDERS.len()];
                EmailRow {
                    id: scoped(scope, &format!("demo{}", 500 + i)),
                    thread_id: scoped(scope, &format!("t{i}")),
                    mailbox_ids: vec![scoped(scope, "inbox")],
                    received_at: now - (i as i64 + 1) * 7200 - 600 * n as i64,
                    subject: format!("{} {}", subjects[i % subjects.len()], 100 * n + i),
                    from_name: name.into(),
                    from_addr: addr.into(),
                    preview: PREVIEW.into(),
                    seen: i > 1,
                    flagged: false,
                    has_attachment: i % 3 == 0,
                    size: 8192,
                    // Нет ключевых слов у сервера — нет и категорий у его писем.
                    keywords: if a.has.keywords { demo_keywords(i) } else { Vec::new() },
                }
            })
            .collect();
        cache.upsert_emails(&rows)?;
    }
    Ok(())
}

/// Общие ящики демо (S4-06): ящик отдела, где можно всё, и ящик руководителя, открытый только для чтения.
pub const SHARED: &[(&str, &str, bool)] = &[("sales", "sales@corp.example", false), ("boss", "director@corp.example", true)];

fn shared(cache: &Cache, now: i64) -> postman_cache::Result<()> {
    let ro = Rights { read: true, add: false, remove: false, seen: false, keywords: false, submit: false };
    let acc = |id: &str, name: &str| AccountRow { id: id.into(), name: name.into(), is_personal: false, read_only: false };
    cache.replace_accounts(&SHARED.iter().map(|(id, name, _)| acc(id, name)).collect::<Vec<_>>())?;
    for (n, &(account, _, read_only)) in SHARED.iter().enumerate() {
        let scope = Some(account);
        let mb = |id: &str, name: &str, order| MailboxRow {
            id: scoped(scope, id),
            parent_id: None,
            name: name.into(),
            role: Some(id.into()),
            sort_order: order,
            total: 0,
            unread: 0,
            rights: if read_only { ro } else { Rights::ALL },
        };
        let boxes = if read_only {
            vec![mb("inbox", "Входящие", 1)]
        } else {
            vec![mb("inbox", "Входящие", 1), mb("drafts", "Черновики", 2), mb("sent", "Отправленные", 3), mb("trash", "Удалённые", 4)]
        };
        cache.replace_mailboxes(scope, &boxes)?;
        let rows: Vec<EmailRow> = (0..12)
            .map(|i| {
                let (name, addr) = SENDERS[(i + n) % SENDERS.len()];
                EmailRow {
                    id: scoped(scope, &format!("demo{}", i * 2 + n)),
                    thread_id: scoped(scope, &format!("t{i}")),
                    mailbox_ids: vec![scoped(scope, "inbox")],
                    received_at: now - (i as i64 + 1) * 5400,
                    subject: format!("{} {}", SHARED_SUBJECTS[(i + n) % SHARED_SUBJECTS.len()], 100 + i),
                    from_name: name.into(),
                    from_addr: addr.into(),
                    preview: PREVIEW.into(),
                    seen: i > 2,
                    flagged: false,
                    has_attachment: i % 4 == 0,
                    size: 8192,
                    keywords: taken_demo(n, i, now),
                }
            })
            .collect();
        cache.upsert_emails(&rows)?;
    }
    Ok(())
}

/// «Взять в работу» в ящике отдела демо (S5-28): кто взял письмо `i` и через сколько минут после прихода.
const TAKEN: &[(usize, &str, i64)] = &[(1, "a.petrov@corp.example", 11), (2, ME, 25), (4, "m.ivanova@corp.example", 40)];

/// Слова «взял» (формат — `postman_core::marks`, тест там же): только ящик отдела (`n == 0`).
fn taken_demo(n: usize, i: usize, now: i64) -> Vec<String> {
    let Some(&(_, who, after)) = TAKEN.iter().find(|t| n == 0 && t.0 == i) else { return Vec::new() };
    let at = now - (i as i64 + 1) * 5400 + after * 60;
    let Ok(at) = jiff::Timestamp::from_second(at) else { return Vec::new() };
    vec!["pm.taken".into(), format!("pm.taken.{}z.{who}", at.strftime("%Y-%m-%dt%H%M"))]
}

/// Слова «взял» письма `i` ящика отдела демо (тест формата в `postman_core::marks`).
pub fn taken_keywords(i: usize, now: i64) -> Vec<String> {
    taken_demo(0, i, now)
}

/// Темы писем общих ящиков демо.
const SHARED_SUBJECTS: &[&str] = &["Заявка клиента №", "Коммерческое предложение", "Re: Счёт на оплату №", "Вопрос по поставке"];

/// Тела писем в демо-режиме: типичное письмо Outlook и «враждебное» —
/// чтобы видеть работу белого списка.
pub fn body(id: &str) -> BodyRow {
    // Письма общих ящиков демо — с префиксом аккаунта, тела — те же.
    let (account, id) = postman_cache::unscoped(id);
    if let Some(b) = chain_body(id) {
        return b;
    }
    // Общее вложение (S8-14): одна и та же сводка в письмах трёх учёток.
    if id.starts_with("share") {
        return BodyRow {
            to: vec![Addr { name: ME_NAME.into(), email: account.and_then(|a| self::account(Some(a))).map_or(ME, |a| a.email).into() }],
            message_id: Some(format!("{id}@corp.example")),
            html: Some(store::SHARED_HTML.into()),
            attachments: vec![AttachmentRow {
                blob_id: store::SHARED_PDF_BLOB.into(),
                name: store::SHARED_PDF.into(),
                mime: "application/pdf".into(),
                size: store::pdf().len() as u64,
                cid: None,
                inline: false,
            }],
            ..Default::default()
        };
    }
    let n: usize = id.trim_start_matches("demo").parse().unwrap_or(0);
    // Подписанное письмо (S5-12): тело — как его разобрал бы сервер, `smime.p7s` во вложениях, письмо целиком —
    // блоб `smime-<вид>`.
    if let Some(s) = smime::sample_for(n).filter(|_| account.is_none() && smime::sample_for(n).is_some_and(|s| s.encrypted)) {
        // Шифрованное (Ф2): тела нет, одно вложение `smime.p7m`, письмо целиком — блоб `smime-<вид>`.
        return BodyRow {
            to: vec![Addr { name: ME_NAME.into(), email: ME.into() }],
            message_id: Some(format!("{id}@corp.example")),
            attachments: vec![AttachmentRow {
                blob_id: format!("p7m-{}", s.kind),
                name: "smime.p7m".into(),
                mime: "application/pkcs7-mime".into(),
                size: smime::p7m_size(s.kind),
                cid: None,
                inline: false,
            }],
            blob_id: Some(format!("smime-{}", s.kind)),
            ..Default::default()
        };
    }
    if let Some(s) = smime::sample_for(n).filter(|_| account.is_none()) {
        return BodyRow {
            to: vec![Addr { name: ME_NAME.into(), email: ME.into() }],
            message_id: Some(format!("{id}@corp.example")),
            html: Some(s.html.into()),
            attachments: vec![AttachmentRow {
                blob_id: format!("p7s-{}", s.kind),
                name: "smime.p7s".into(),
                mime: "application/pkcs7-signature".into(),
                size: smime::P7S_SIZE,
                cid: None,
                inline: false,
            }],
            blob_id: Some(format!("smime-{}", s.kind)),
            ..Default::default()
        };
    }
    let att = |blob: &str, name: &str, mime: &str, size: u64, cid: Option<&str>| AttachmentRow {
        blob_id: blob.into(),
        name: name.into(),
        mime: mime.into(),
        size,
        cid: cid.map(Into::into),
        inline: cid.is_some(),
    };
    let (html, attachments) = if n % 100 == CHART_EVERY {
        // Большая картинка в тексте (S5-07б): кадр больше 1024 px по стороне.
        // Подпись таблицей с крупным логотипом, у которого в HTML задан размер (T1-10г).
        (
            CHART_HTML,
            vec![
                att("chart", "chart.png", "image/png", chart().len() as u64, Some("chart@corp")),
                att("sign-logo", "logo-big.png", "image/png", sign_logo().len() as u64, Some("sign@corp")),
            ],
        )
    } else if n % 100 == TASK_EVERY {
        // Поручение письмом (S6-10): VTODO REQUEST — карточка поручения над текстом.
        (TASK_HTML, vec![att("todo", "task.ics", "text/calendar", delegations::letter().len() as u64, None)])
    } else if n.is_multiple_of(10) {
        (INVITE_HTML, vec![att("ics", "invite.ics", "text/calendar", INVITE.len() as u64, None)])
    } else if n.is_multiple_of(2) {
        (
            OUTLOOK,
            vec![
                att("logo", "logo.png", "image/png", LOGO.len() as u64, Some("logo@corp")),
                // Встроенный просмотр (S5-07): PDF, архив с опасными файлами, книга с макросами, фото.
                att("pdf", "Отчёт за квартал.pdf", "application/pdf", REPORT.len() as u64, None),
                att("zip", "Снимки экрана.zip", "application/zip", SCREENS.len() as u64, None),
                att("xlsm", "Бюджет.xlsm", "application/vnd.ms-excel.sheet.macroEnabled.12", BUDGET.len() as u64, None),
                att("xlsx", "Итоги Q3.xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", 18_432, None),
                att("photo", "Фото стенда.jpg", "image/jpeg", PHOTO.len() as u64, None),
            ],
        )
    } else {
        (
            HOSTILE,
            vec![
                att("exe", "invoice.pdf.exe", "application/octet-stream", 73_728, None),
                att("rtlo", "счёт\u{202e}fdp.exe", "application/octet-stream", 73_728, None),
            ],
        )
    };
    BodyRow {
        to: vec![Addr { name: "Команда проекта".into(), email: "team@corp.example".into() }],
        message_id: Some(format!("{id}@corp.example")),
        html: Some(html.into()),
        attachments,
        ..Default::default()
    }
}

const LOGO: &[u8] = include_bytes!("../../worker/tests/fixtures/img.png");
/// Вложения для встроенного просмотра (S5-07): синтетические, из тестов воркера.
const REPORT: &[u8] = include_bytes!("../../worker/tests/fixtures/report.pdf");
const SCREENS: &[u8] = include_bytes!("../../worker/tests/fixtures/screens.zip");
const BUDGET: &[u8] = include_bytes!("../../worker/tests/fixtures/budget.xlsm");
const PHOTO: &[u8] = include_bytes!("../../worker/tests/fixtures/img.jpg");
/// Приглашение Outlook (iMIP REQUEST) — каждое десятое демо-письмо.
const INVITE: &[u8] = include_bytes!("../../worker/tests/fixtures/invite-outlook.ics");
/// Приглашение демо — на завтра (дата файла заменяется): в любой час запуска оно впереди и ждёт ответа, а не
/// «Встреча уже прошла». Длина файла та же — размер вложения в письме верен.
pub fn invite() -> Vec<u8> {
    let tomorrow = jiff::Zoned::now().date().tomorrow().unwrap_or(jiff::civil::date(2026, 10, 22));
    let day = format!("{:04}{:02}{:02}T", tomorrow.year(), tomorrow.month(), tomorrow.day());
    String::from_utf8_lossy(INVITE).replace(INVITE_DAY, &day).into_bytes()
}

/// День встречи в файле приглашения (`DTSTART`, `DTEND`).
const INVITE_DAY: &str = "20261021T";
const INVITE_HTML: &str = "<p>Иван Петров приглашает вас на встречу «Обсуждение пилота SIEM».</p>";

/// Письмо-поручение (S6-10) — `demo20`, `demo120`… (одно на сотню, вместо приглашения).
pub const TASK_EVERY: usize = 20;
const TASK_HTML: &str = "<p>Иванова Мария поручила вам задачу. Ответьте в Postman или ответным письмом.</p>";

/// Письмо с большой картинкой в тексте — `demo4`, `demo104`… (одно на сотню: бенч открытия писем их почти не видит).
const CHART_EVERY: usize = 4;
const CHART_HTML: &str = r#"<p>Коллеги, график загрузки стенда за неделю — ниже.</p>
<p><img src="cid:chart@corp" alt="График загрузки"></p>
<p>Пики — по вторникам, подробности — в отчёте.</p>
<table cellpadding=0 cellspacing=0 style='border-collapse:collapse'><tr>
<td style='padding:0 12px 0 0'><img src="cid:sign@corp" width=180 height=60 style='width:1.875in;height:.625in' alt="Логотип отдела"></td>
<td><p class=MsoNormal><b>Мария Иванова</b><br>Финансовый отдел<br>+7 000 000-00-00</p></td></tr></table>"#;
/// Сторона большой картинки: шире предела кадра в тексте письма (1600) — воркер её уменьшает.
pub const CHART_SIZE: (u32, u32) = (2400, 1500);

/// Кадр логотипа в подписи письма с графиком (T1-10г): крупнее колонки, в HTML задан размер 180×60 — показ в
/// своём размере выглядел бы огромным.
pub const SIGN_LOGO_SIZE: (u32, u32) = (1200, 400);

fn sign_logo() -> &'static [u8] {
    static LOGO: OnceLock<Vec<u8>> = OnceLock::new();
    LOGO.get_or_init(|| chart_png(SIGN_LOGO_SIZE.0, SIGN_LOGO_SIZE.1).unwrap_or_default())
}

/// Большая картинка (PNG): рамка и цветные углы — на снимке видно, что кадр целиком и не сдвинут. Собирается один
/// раз при первом запросе; ошибка кодировщика — пустой файл (просмотр покажет ошибку разбора).
fn chart() -> &'static [u8] {
    static CHART: OnceLock<Vec<u8>> = OnceLock::new();
    CHART.get_or_init(|| chart_png(CHART_SIZE.0, CHART_SIZE.1).unwrap_or_default())
}

fn chart_png(w: u32, h: u32) -> Option<Vec<u8>> {
    const BORDER: u32 = 24;
    const CORNER: u32 = 220;
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            let left = x < CORNER;
            let top = y < CORNER;
            let right = x >= w - CORNER;
            let bottom = y >= h - CORNER;
            let px: [u8; 3] = if x < BORDER || y < BORDER || x >= w - BORDER || y >= h - BORDER {
                [0x33, 0x33, 0x44]
            } else if left && top {
                [0xD1, 0x34, 0x38]
            } else if right && top {
                [0x10, 0x7C, 0x10]
            } else if left && bottom {
                [0x00, 0x78, 0xD4]
            } else if right && bottom {
                [0xFF, 0x8C, 0x00]
            } else if x.abs_diff(w / 2) < 8 || y.abs_diff(h / 2) < 8 {
                [0x33, 0x33, 0x44]
            } else if x % 150 < 3 || y % 150 < 3 {
                [0xB0, 0xC4, 0xDE]
            } else {
                // Сверху вниз — от голубого к белому.
                let t = (y * 255 / h) as u8;
                [0xC8u8.saturating_add(t / 5), 0xDCu8.saturating_add(t / 8), 0xF5]
            };
            rgb.extend_from_slice(&px);
        }
    }
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().ok()?;
    writer.write_image_data(&rgb).ok()?;
    writer.finish().ok()?;
    Some(out)
}

/// Содержимое демо-вложений.
pub fn blob(blob_id: &str) -> Option<Vec<u8>> {
    match blob_id {
        "logo" => Some(LOGO.to_vec()),
        "chart" => Some(chart().to_vec()),
        "sign-logo" => Some(sign_logo().to_vec()),
        "ics" => Some(invite()),
        "todo" => Some(delegations::letter()),
        "pdf" => Some(REPORT.to_vec()),
        "zip" => Some(SCREENS.to_vec()),
        "xlsm" => Some(BUDGET.to_vec()),
        "photo" => Some(PHOTO.to_vec()),
        "xlsx" | "exe" | "rtlo" => Some(b"demo".to_vec()),
        store::SHARED_PDF_BLOB => Some(store::pdf().to_vec()),
        other => other.strip_prefix("smime-").and_then(smime::raw).map(<[u8]>::to_vec),
    }
}

const OUTLOOK: &str = r#"<html><head><style>p.MsoNormal{margin:0}</style></head><body>
<div class=WordSection1><p class=MsoNormal><img src="cid:logo@corp" alt="Логотип"></p><p class=MsoNormal>Коллеги, добрый день!</p>
<p class=MsoNormal>Направляю <b>итоги квартала</b>. Обратите внимание на <span style='color:red;font-weight:bold'>раздел 3</span>
и <i>сроки</i>.</p>
<table border=1><tr><th>Показатель</th><th>План</th><th>Факт</th></tr>
<tr><td>Выручка</td><td>100</td><td>112</td></tr><tr><td>Расходы</td><td>80</td><td>77</td></tr></table>
<ul><li>Сверка до пятницы</li><li>Презентация — <a href="https://wiki.corp.example/q3">в вики</a></li></ul>
<p class=MsoNormal>&nbsp;</p>
<p class=MsoNormal>С уважением,<br>Мария Иванова<br>Финансовый отдел</p>
<div style='border:none;border-top:solid #E1E1E1 1.0pt'><p class=MsoNormal><b>From:</b> Петров Алексей<br><b>Sent:</b> Monday</p></div>
<blockquote><p>Мария, пришлите, пожалуйста, итоги.</p></blockquote></div></body></html>"#;

const HOSTILE: &str = r#"<html><body onload="steal()"><script>alert('xss')</script>
<p>Уважаемый клиент! Ваш аккаунт <span style="display:none">скрытый текст для антиспама</span> заблокирован.</p>
<p>Для разблокировки перейдите: <a href="https://evil.example/login">https://bank.example/secure</a></p>
<p><a href="javascript:steal()">Нажмите здесь</a></p>
<img src="https://tracker.example/pixel.gif?u=123" width=1 height=1>
<img src="https://cdn.example/banner.png" alt="Баннер банка">
<form action="https://evil.example"><input name=password type=password></form>
<iframe src="https://evil.example/frame"></iframe>
<p style="font-size:1px;color:#fff">мелкий белый текст</p></body></html>"#;

/// Демо-календарь на текущую рабочую неделю: мои события и занятость коллег.
pub mod calendar {
    use postman_calendar::{Busy, BusyKind, Event, EventStatus, FreeBusy, Owner, Participant, Partstat, Span};

    /// Понедельник текущей недели, 00:00 в системном часовом поясе (Unix-секунды).
    pub fn week_start() -> i64 {
        let now = jiff::Zoned::now();
        let monday = now.date().checked_sub(jiff::Span::new().days(i64::from(now.weekday().to_monday_zero_offset()))).unwrap_or(now.date());
        monday.to_zoned(now.time_zone().clone()).map(|z| z.timestamp().as_second()).unwrap_or(0)
    }

    fn at(day: i64, h: i64, m: i64) -> i64 {
        week_start() + day * 86_400 + h * 3600 + m * 60
    }

    /// Кто я во встрече демо: организатор, приглашённый (с моим ответом) — организатор тогда коллега.
    #[derive(Clone, Copy)]
    enum Me {
        Owner,
        Invited(Partstat),
    }

    #[allow(clippy::too_many_arguments)]
    fn event(id: &str, title: &str, day: i64, (h, m): (i64, i64), mins: i64, location: &str, tentative: bool, me: Me, category: &str) -> Event {
        let mine = |status| Participant { name: "Я".into(), email: crate::ME.into(), owner: false, attendee: true, optional: false, status };
        let participants = match me {
            Me::Owner => vec![Participant { owner: true, ..mine(Partstat::Accepted) }],
            Me::Invited(status) => vec![
                Participant {
                    name: "Петров Алексей".into(),
                    email: "a.petrov@corp.example".into(),
                    owner: true,
                    attendee: true,
                    optional: false,
                    status: Partstat::Accepted,
                },
                mine(status),
                Participant {
                    name: "Иванова Мария".into(),
                    email: "m.ivanova@corp.example".into(),
                    owner: false,
                    attendee: true,
                    optional: false,
                    status: Partstat::Accepted,
                },
                Participant {
                    name: "Сидоров Игорь".into(),
                    email: "i.sidorov@corp.example".into(),
                    owner: false,
                    attendee: true,
                    optional: true,
                    status: Partstat::NeedsAction,
                },
            ],
        };
        Event {
            id: id.into(),
            uid: format!("{id}@corp.example"),
            calendar_ids: vec!["work".into()],
            title: title.into(),
            description: String::new(),
            location: location.into(),
            span: Span::new(at(day, h, m), at(day, h, m) + mins * 60),
            all_day: false,
            status: if tentative { EventStatus::Tentative } else { EventStatus::Confirmed },
            free_busy: FreeBusy::Busy,
            participants,
            recurring: false,
            keywords: if category.is_empty() { Vec::new() } else { vec![format!("pm.cat.{category}")] },
            sequence: 0,
            owner: None,
        }
    }

    pub fn my_events() -> Vec<Event> {
        use Me::{Invited, Owner};
        use Partstat::{Accepted, Declined, NeedsAction, Tentative};
        let weekly = |id: &str, day| {
            let mut e = event(id, "Планёрка отдела", day, (9, 30), 30, "Переговорная 1", false, Invited(Accepted), "team");
            e.recurring = true;
            e
        };
        let mut v = vec![
            weekly("e1", 0),
            event("e2", "Созвон с заказчиком", 0, (11, 0), 90, "Teams", false, Owner, "clients"),
            event("e3", "Обед с партнёром", 0, (13, 0), 60, "Кафе", true, Owner, ""),
            event("e4", "Разбор инцидента", 0, (11, 30), 60, "Переговорная 2", false, Invited(Accepted), "pentest"),
            weekly("e5", 1),
            event("e6", "Пресейл: SIEM", 1, (14, 0), 120, "Переговорная 3", false, Invited(NeedsAction), "tenders"),
            event("e7", "1:1 с руководителем", 2, (10, 0), 60, "", false, Owner, ""),
            event("e8", "Демо продукта", 2, (15, 0), 60, "Переговорная 1", false, Invited(Accepted), "clients"),
            event("e9", "Ревью архитектуры", 2, (15, 30), 90, "Переговорная 2", true, Invited(Tentative), "services"),
            event("e13", "Утренний стендап", 2, (11, 30), 30, "Teams", false, Invited(Accepted), "team"),
            event("e14", "Вебинар вендора", 2, (12, 30), 60, "", false, Invited(Declined), ""),
            weekly("e10", 3),
            event("e11", "Обучение: Stalwart", 3, (12, 0), 180, "Учебный класс", false, Invited(Accepted), "team"),
        ];
        // Своя повторяющаяся встреча с участниками (S7-07б): «Изменить» и «Отменить встречу» в подробностях.
        let mut results = event("e12", "Итоги недели", 4, (16, 0), 60, "Переговорная 1", false, Owner, "");
        results.recurring = true;
        results.participants.extend([
            Participant {
                name: "Иванова Мария".into(),
                email: "m.ivanova@corp.example".into(),
                owner: false,
                attendee: true,
                optional: false,
                status: Accepted,
            },
            Participant {
                name: "Сидоров Игорь".into(),
                email: "i.sidorov@corp.example".into(),
                owner: false,
                attendee: true,
                optional: false,
                status: NeedsAction,
            },
        ]);
        v.push(results);
        // На весь день, два дня: четверг и пятница.
        let mut conference = event("e15", "Выездной семинар", 3, (0, 0), 2 * 24 * 60, "Конгресс-центр", false, Invited(Accepted), "clients");
        conference.all_day = true;
        v.push(conference);
        v.sort_by_key(|e| e.span.start);
        v
    }

    /// Календарь дополнительной учётной записи демо (S7-03б): владелец — она сама, как у календарей коллег; подпись —
    /// адрес учётки.
    fn of_account(a: &crate::Account) -> Owner {
        Owner { email: a.email.into(), name: a.email.into() }
    }

    /// Календари дополнительных учёток демо (S7-09а), где календарь есть, — в «Моих календарях», по порядку дерева.
    pub fn account_calendars() -> Vec<Owner> {
        crate::ACCOUNTS.iter().filter(|a| a.profile.is_some() && a.has.calendar).map(of_account).collect()
    }

    /// Событие календаря дополнительной учётки: моя встреча, календарь — её.
    fn account_event(a: &crate::Account, calendar: &str, (id, title): (&str, &str), day: i64, at: (i64, i64), mins: i64, place: &str) -> Event {
        let mut e = event(id, title, day, at, mins, place, false, Me::Owner, "");
        e.calendar_ids = vec![calendar.into()];
        e.owner = Some(of_account(a));
        e
    }

    /// События дополнительных учёток демо-недели (S7-09а): «стандарты» (CalDAV) и Exchange (EWS).
    pub fn account_events() -> Vec<Event> {
        let (s, x) = (&crate::STANDARDS, &crate::EXCHANGE);
        vec![
            account_event(s, "s", ("s1", "Созвон с партнёром"), 1, (16, 0), 60, "Teams"),
            account_event(s, "s", ("s2", "Выставка: стенд"), 4, (11, 0), 120, "Экспоцентр"),
            account_event(x, "x", ("x1", "Комитет по бюджету"), 0, (15, 0), 60, "Зал совета"),
            account_event(x, "x", ("x2", "Инструктаж по пропускам"), 3, (16, 30), 30, "Проходная"),
        ]
    }

    /// Коллеги, открывшие мне календарь (S5-22): события их календарей — [`colleague_events`].
    pub fn shared_calendars() -> Vec<Owner> {
        vec![
            Owner { email: "m.ivanova@corp.example".into(), name: "Иванова Мария".into() },
            Owner { email: "a.petrov@corp.example".into(), name: "Петров Алексей".into() },
        ]
    }

    /// События календарей коллег демо-недели: владелец — у события, организатор — он сам.
    pub fn colleague_events() -> Vec<Event> {
        let owners = shared_calendars();
        let of = |n: usize, id: &str, title: &str, day: i64, at: (i64, i64), mins: i64, place: &str| {
            let o = owners[n].clone();
            let mut e = event(id, title, day, at, mins, place, false, Me::Owner, "");
            e.participants = vec![Participant {
                name: o.name.clone(),
                email: o.email.clone(),
                owner: true,
                attendee: true,
                optional: false,
                status: Partstat::Accepted,
            }];
            e.calendar_ids = vec!["c".into()];
            e.owner = Some(o);
            e
        };
        vec![
            of(0, "c1", "Аудит доступа", 0, (14, 30), 60, "Переговорная 2"),
            of(0, "c2", "Встреча с юристами", 2, (9, 0), 60, ""),
            of(0, "c3", "Отпуск — подготовка", 4, (10, 0), 90, ""),
            of(1, "c4", "Звонок поставщику", 1, (10, 30), 60, "Teams"),
            of(1, "c5", "Совет директоров", 3, (16, 0), 90, "Зал совета"),
        ]
    }

    /// События соседних недель и выходных (S5-21: виды «Неделя» и «Месяц» не пустые вокруг текущей недели): в
    /// занятость помощника и ленту приглашения не попадают.
    pub fn other_events() -> Vec<Event> {
        use Me::{Invited, Owner};
        use Partstat::Accepted;
        let mut v = vec![
            event("o1", "Ретро квартала", -4, (15, 0), 60, "Переговорная 1", false, Invited(Accepted), "team"),
            event("o2", "Аудит доступа", -3, (10, 0), 120, "Переговорная 2", false, Owner, "pentest"),
            event("o3", "Велопрогулка", 5, (10, 0), 180, "Парк", false, Owner, ""),
            event("o4", "Тендер: подача заявки", 8, (12, 0), 60, "", false, Owner, "tenders"),
            event("o5", "Созвон с заказчиком", 9, (11, 0), 90, "Teams", false, Owner, "clients"),
            event("o6", "Обучение: SOC", 15, (10, 0), 240, "Учебный класс", false, Invited(Accepted), "team"),
            event("o7", "Демо продукта", -11, (14, 0), 60, "Переговорная 1", false, Invited(Accepted), "clients"),
        ];
        // Командировка: с субботы через выходные до вторника — полосой через границу недели.
        let mut trip = event("o8", "Командировка", 12, (0, 0), 4 * 24 * 60, "Екатеринбург", false, Owner, "services");
        trip.all_day = true;
        v.push(trip);
        v.sort_by_key(|e| e.span.start);
        v
    }

    /// Коллеги для «Помощника планирования»: (имя, адрес, занятость).
    pub fn colleagues() -> Vec<(&'static str, &'static str, Vec<Busy>)> {
        let b = |day, (h, m): (i64, i64), mins: i64, kind| Busy { span: Span::new(at(day, h, m), at(day, h, m) + mins * 60), kind };
        use BusyKind::{Busy as B, Tentative as T, Unavailable as U};
        vec![
            (
                "Иванова Мария",
                "m.ivanova@corp.example",
                vec![b(0, (10, 0), 120, B), b(1, (9, 0), 240, B), b(2, (14, 0), 60, T), b(3, (9, 0), 540, U)],
            ),
            (
                "Петров Алексей",
                "a.petrov@corp.example",
                vec![b(0, (9, 0), 60, B), b(0, (15, 0), 120, B), b(1, (13, 0), 120, B), b(2, (10, 0), 90, B), b(4, (11, 0), 60, T)],
            ),
            ("Сидоров Игорь", "i.sidorov@corp.example", vec![b(0, (12, 0), 60, B), b(1, (10, 0), 60, B), b(2, (9, 0), 180, B), b(4, (9, 0), 120, B)]),
            ("Переговорная 3", "room3@corp.example", vec![b(1, (14, 0), 120, B), b(2, (11, 0), 60, B), b(3, (15, 0), 60, B)]),
        ]
    }

    /// Моя занятость из событий — для сетки помощника (отклонённые не занимают время).
    pub fn my_busy() -> Vec<Busy> {
        let declined = |e: &Event| e.participants.iter().any(|p| p.email == crate::ME && p.status == Partstat::Declined);
        my_events()
            .iter()
            .filter(|e| !declined(e))
            .map(|e| Busy { span: e.span, kind: if e.status == EventStatus::Tentative { BusyKind::Tentative } else { BusyKind::Busy } })
            .collect()
    }
}

/// Демо-контакты (S4-02): личная адресная книга и справочник сотрудников; одни и те же от запуска к запуску.
/// Задачи демо (S6-05): названия вымышленные; сроки — в днях от сегодня (час — `None`: без времени).
pub mod tasks {
    pub struct Task {
        pub title: &'static str,
        /// Категория таблицы по умолчанию (`pm.cat.<id>`).
        pub category: Option<&'static str>,
        pub project: Option<&'static str>,
        pub due: Option<(i64, Option<i8>)>,
        pub important: bool,
    }

    const fn t(
        title: &'static str,
        category: Option<&'static str>,
        project: Option<&'static str>,
        due: Option<(i64, Option<i8>)>,
        important: bool,
    ) -> Task {
        Task { title, category, project, due, important }
    }

    /// Задача с подзадачами, заметкой и напоминанием (снимок `tasks`).
    pub const MAIN: &str = "Отправить редакцию договора v3";
    /// Просроченная (снимок `tasks-today`).
    pub const OVERDUE: &str = "Согласовать отчёт по аудиту v2";
    /// Проект снимка `tasks-project`.
    pub const PROJECT: &str = "Пилот «Маяк»";
    pub const NOTES: &str = "Учесть правки юриста по п. 7.2.\n- SLA — по согласованию\n- приложение 3 — перечень работ";
    pub const SUBTASKS: &[(&str, bool)] =
        &[("Учесть правки юриста", true), ("Согласовать SLA", true), ("Обновить приложение 3", false), ("Сравнить с редакцией v2", false)];
    /// Фраза строки «Добавить задачу» (снимок `task-quickadd`).
    pub const QUICK: &str = "Отправить КП «Северу» завтра 10:00 #Клиенты !";
    /// Повторяется каждый месяц.
    pub const REPEAT: &str = "Продлить лицензии";
    /// «Когда-нибудь».
    pub const SOMEDAY: &str = "Идея: шаблон отчёта для клиентов";
    /// Выполненные: название, категория, проект, сколько часов назад.
    pub const DONE: &[(&str, Option<&str>, Option<&str>, i64)] =
        &[("Выгрузить журнал событий за квартал", Some("services"), None, 1), ("Согласовать план обучения", Some("team"), Some("План IV кв."), 26)];

    pub const OPEN: &[Task] = &[
        t(MAIN, Some("clients"), Some("Договор «Север»"), Some((0, Some(16))), true),
        t(OVERDUE, Some("pentest"), Some("Аудит «Луч»"), Some((-1, None)), true),
        t("Проверить счета подрядчиков за месяц", Some("services"), None, Some((-3, None)), false),
        t("Ресурсный план группы на следующий месяц", Some("team"), Some("План IV кв."), Some((0, Some(17))), false),
        t("Проверить методику оценки рисков", Some("clients"), Some(PROJECT), Some((0, Some(18))), false),
        t("Согласовать отпуск сотрудника", Some("team"), None, Some((0, None)), false),
        t("Вопросы к созвону по тендеру", Some("tenders"), Some("Тендер № 7"), Some((0, None)), false),
        t("Ответ по разъяснениям тендера", Some("tenders"), Some("Тендер № 7"), Some((1, Some(12))), false),
        t("Напомнить про коммерческое предложение", Some("clients"), Some("Договор «Север»"), Some((2, None)), false),
        t("Комментарии к статусу проектов", Some("team"), None, Some((3, None)), false),
        t("Отчёт за месяц для клиента", Some("clients"), Some(PROJECT), Some((6, None)), false),
        t("Итоговая презентация пилота", Some("clients"), Some(PROJECT), Some((9, None)), true),
        t("Посмотреть доклад о защите агентов", None, None, None, false),
        t("Разобрать заметки с конференции", None, None, None, false),
        t(SOMEDAY, None, None, None, false),
        t(REPEAT, Some("services"), None, Some((0, None)), false),
    ];
}

/// Поручения демо (S6-10): я поручил и мне поручили, все статусы; лента шагов — как её ведёт Postman
/// (`postman_calendar::assign`). Коллеги — из справочника демо.
pub mod delegations {
    use postman_calendar::assign::{self, Step, StepKind};
    use postman_calendar::{Moment, Progress, Role, Task, TaskParticipant, TaskPartstat};

    /// Проект поручений демо: в нём видны все статусы (снимок `delegation-statuses`).
    pub const PROJECT: &str = "Отчёт для заказчика";
    /// Готово — на проверку (снимок `delegation-steps`).
    pub const REVIEW: &str = "Итоговый отчёт по обследованию";
    /// Поручено мне, ждёт ответа (снимок `delegation-in`).
    pub const INCOMING: &str = "Проверить перечень работ к договору";
    /// Поручить (снимок `delegate`): своя задача без поручения.
    pub const TO_ASSIGN: &str = "Вопросы к созвону по тендеру";
    /// Поручил коллега, я наблюдаю (снимок `delegations-out` со снятым «Только свои»).
    pub const WATCHED: &str = "Регламент реагирования на инциденты";
    /// Письмо-поручение (снимок `delegation-mail`).
    pub const LETTER: &str = "Подготовить вопросы к созвону с заказчиком";

    pub const PETROV: (&str, &str) = ("Петров Алексей", "a.petrov@corp.example");
    pub const IVANOVA: (&str, &str) = ("Иванова Мария", "m.ivanova@corp.example");
    pub const VOLKOV: (&str, &str) = ("Волков Сергей", "s.volkov@corp.example");
    pub const KUZNETSOVA: (&str, &str) = ("Кузнецова Елена", "e.kuznetsova@corp.example");

    const HOUR: i64 = 3600;

    fn person(key: &str, who: (&str, &str), role: Role, status: TaskPartstat) -> TaskParticipant {
        TaskParticipant { key: key.into(), name: who.0.into(), email: who.1.into(), role, status }
    }

    fn me() -> (&'static str, &'static str) {
        (super::ME_NAME, super::ME)
    }

    fn day(today: jiff::civil::Date, n: i64, hour: i8) -> jiff::civil::DateTime {
        today.checked_add(jiff::Span::new().days(n)).unwrap_or(today).at(hour, 0, 0, 0)
    }

    /// Поручение: кто поручил, кому, ответ исполнителя, срок (дней от сегодня, час), шаги (часов назад, кто, что,
    /// комментарий).
    #[allow(clippy::too_many_arguments)]
    fn task(
        n: usize,
        title: &str,
        owner: (&str, &str),
        worker: (&str, &str),
        status: TaskPartstat,
        due: (i64, i8),
        today: jiff::civil::Date,
        now: i64,
        steps: &[(i64, (&str, &str), StepKind, &str)],
    ) -> Task {
        let mut t = Task::new(&format!("demo-deleg-{n}@postman"), "demo", title);
        t.project = Some(PROJECT.into());
        t.due = Some(Moment::new(day(today, due.0, due.1), None));
        t.participants = vec![person("owner", owner, Role::Owner, TaskPartstat::NeedsAction), person("assignee", worker, Role::Assignee, status)];
        if matches!(status, TaskPartstat::InProcess | TaskPartstat::Completed) {
            t.progress = Progress::InProcess;
            t.percent = Some(if status == TaskPartstat::Completed { 100 } else { 40 });
        }
        for (ago, who, kind, comment) in steps {
            assign::push_step(&mut t, Step { at: now - ago * HOUR, who: who.1.into(), kind: *kind, comment: (*comment).into(), due: None });
        }
        t
    }

    /// Все поручения демо; `today` и `now` — в поясе пользователя.
    pub fn all(today: jiff::civil::Date, now: i64) -> Vec<Task> {
        use StepKind as K;
        use TaskPartstat as P;
        let me = me();
        let proposed = day(today, 6, 18);
        let mut out = vec![
            task(
                1,
                REVIEW,
                me,
                PETROV,
                P::Completed,
                (1, 18),
                today,
                now,
                &[
                    (72, me, K::Assigned, "Нужен к пятнице, с выводами для руководства"),
                    (71, PETROV, K::Accepted, ""),
                    (48, PETROV, K::Started, ""),
                    (1, PETROV, K::Finished, "Отчёт готов: добавил раздел о сегментации сети. Посмотри выводы — заказчик прочтёт их первыми."),
                ],
            ),
            task(
                2,
                "Ресурсный план группы на ноябрь",
                me,
                IVANOVA,
                P::InProcess,
                (2, 12),
                today,
                now,
                &[(30, me, K::Assigned, ""), (29, IVANOVA, K::Accepted, ""), (5, IVANOVA, K::Started, "")],
            ),
            task(3, "Расчёт коммерческого предложения", me, VOLKOV, P::NeedsAction, (3, 18), today, now, &[(20, me, K::Assigned, "")]),
            task(4, "Требования к мониторингу для ТЗ", me, KUZNETSOVA, P::NeedsAction, (1, 12), today, now, &[(26, me, K::Assigned, "")]),
            task(
                5,
                "Заказать пропуска для аудиторов",
                me,
                VOLKOV,
                P::Declined,
                (1, 10),
                today,
                now,
                &[(22, me, K::Assigned, ""), (4, VOLKOV, K::Declined, "Пропуска оформляет хозяйственный отдел, у нас нет доступа к их системе.")],
            ),
            task(
                6,
                INCOMING,
                IVANOVA,
                me,
                P::NeedsAction,
                (1, 15),
                today,
                now,
                &[(1, IVANOVA, K::Assigned, "Посмотри, пожалуйста, до созвона — особенно п. 3.")],
            ),
            task(
                7,
                "Комментарии к статусу проектов",
                PETROV,
                me,
                P::Accepted,
                (3, 15),
                today,
                now,
                &[(20, PETROV, K::Assigned, ""), (18, me, K::Accepted, "")],
            ),
            watched(task(
                8,
                WATCHED,
                PETROV,
                VOLKOV,
                P::InProcess,
                (6, 18),
                today,
                now,
                &[(30, PETROV, K::Assigned, ""), (28, VOLKOV, K::Accepted, ""), (3, VOLKOV, K::Started, "")],
            )),
            letter_task(today),
        ];
        // Предложенный срок — через неделю, с комментарием.
        if let Some(t) = out.iter_mut().find(|t| t.uid == "demo-deleg-4@postman") {
            assign::set_proposed(t, Some(&Moment::new(proposed, None)));
            assign::push_step(
                t,
                Step {
                    at: now - 2 * HOUR,
                    who: KUZNETSOVA.1.into(),
                    kind: K::Proposed,
                    comment: "Жду ответа заказчика на разъяснения — раньше понедельника не соберу.".into(),
                    due: Some(proposed.strftime("%Y-%m-%dT%H:%M:%S").to_string()),
                },
            );
        }
        out
    }

    /// Я — наблюдатель (`ROLE=NON-PARTICIPANT`): видно в «Я поручил», если «Только свои» снят.
    fn watched(mut t: Task) -> Task {
        t.participants.push(person("observer1", me(), Role::Observer, TaskPartstat::NeedsAction));
        t
    }

    /// Мои поручения (я поручивший) — номера задач демо.
    const MINE: [usize; 5] = [1, 2, 3, 4, 5];

    /// Ключ письма-поручения `n` в кэше демо.
    pub fn letter_id(n: usize) -> String {
        format!("deleg-mail-{n}")
    }

    /// Письма моих поручений в «Отправленных» (как их оставляет отправка, S6-11): со словом поручения
    /// (`assign::keyword`) — быстрый фильтр «Ждём» находит письма невыполненных.
    pub fn letters(cache: &postman_cache::Cache, now: i64) -> postman_cache::Result<()> {
        let today = jiff::Timestamp::from_second(now).map(|t| t.to_zoned(jiff::tz::TimeZone::system()).date()).unwrap_or_default();
        let list = all(today, now);
        let mine: Vec<(usize, &Task)> =
            MINE.iter().filter_map(|&n| list.iter().find(|t| t.uid == format!("demo-deleg-{n}@postman")).map(|t| (n, t))).collect();
        let rows: Vec<postman_cache::EmailRow> = mine
            .iter()
            .map(|&(n, t)| postman_cache::EmailRow {
                id: letter_id(n),
                thread_id: letter_id(n),
                mailbox_ids: vec!["sent".into()],
                received_at: assign::steps(t).first().map_or(now, |s| s.at),
                subject: format!("Поручение: {}", t.title),
                from_name: super::ME_NAME.into(),
                from_addr: super::ME.into(),
                preview: t.notes.clone(),
                seen: true,
                flagged: false,
                has_attachment: false,
                size: 6144,
                keywords: vec![assign::keyword(&t.uid)],
            })
            .collect();
        cache.upsert_emails(&rows)?;
        let to: Vec<(String, postman_cache::Recipients)> = mine
            .iter()
            .map(|&(n, t)| {
                let to = assign::assignee(t).map(|p| vec![p.email.clone()]).unwrap_or_default();
                (letter_id(n), postman_cache::Recipients { to, cc: Vec::new() })
            })
            .collect();
        cache.set_recipients(&to)
    }

    /// Задача письма-поручения: сервер уже положил её в мой календарь (как VTODO из iMIP, S6-01) — срок завтра.
    fn letter_task(today: jiff::civil::Date) -> Task {
        let mut t = Task::new("demo-deleg-mail@postman", "demo", LETTER);
        t.notes = "Сроки поставки, требования к мониторингу, лицензии и SLA.".into();
        t.due = Some(Moment::new(day(today, 1, 12), None));
        t.participants = vec![
            person("owner", IVANOVA, Role::Owner, TaskPartstat::NeedsAction),
            person("assignee", me(), Role::Assignee, TaskPartstat::NeedsAction),
        ];
        t
    }

    /// Письмо-поручение демо: REQUEST от коллеги мне, срок — завтра (в любой день запуска оно впереди).
    pub fn letter() -> Vec<u8> {
        let now = jiff::Zoned::now();
        let t = letter_task(now.date());
        let ts = now.timestamp().as_second();
        assign::request(&t, "Нужно до созвона в среду.", false, ts - 2 * HOUR, &jiff::tz::TimeZone::system()).into_bytes()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn every_status_is_shown() {
            let list = all(jiff::civil::date(2026, 10, 21), 1_792_000_000);
            let mut seen: Vec<&str> = list.iter().filter_map(assign::status).map(|s| s.id()).collect();
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(seen, ["accepted", "declined", "done", "in-progress", "proposed", "waiting"]);
            assert_eq!(list.iter().filter(|t| t.is_assigned_to(crate::ME)).count(), 3);
            let ics = String::from_utf8(letter()).unwrap();
            assert!(ics.contains("METHOD:REQUEST") && ics.contains(crate::ME));
        }
    }
}

pub mod contacts {
    /// Личный контакт.
    pub struct Card {
        pub name: &'static str,
        pub emails: &'static [&'static str],
        pub phones: &'static [&'static str],
        pub organization: &'static str,
        pub title: &'static str,
    }

    const fn card(
        name: &'static str,
        emails: &'static [&'static str],
        phones: &'static [&'static str],
        organization: &'static str,
        title: &'static str,
    ) -> Card {
        Card { name, emails, phones, organization, title }
    }

    const MINE: &[Card] = &[
        card(
            "Белова Ольга",
            &["o.belova@partner.example", "olga.belova@mail.example"],
            &["+7 495 123-45-67", "+7 916 555-01-02"],
            "ООО «Партнёр»",
            "Руководитель отдела закупок",
        ),
        card("Гусев Дмитрий", &["d.gusev@integrator.example"], &["+7 903 222-33-44"], "АО «Интегратор»", "Архитектор решений"),
        card("Курьерская служба", &["orders@courier.example"], &["+7 495 777-00-11"], "", ""),
        card("Лебедева Анна", &["anna.lebedeva@law.example"], &[], "Юридическая фирма «Лебедева и партнёры»", "Партнёр"),
        card("Морозов Кирилл", &["k.morozov@bank.example"], &["+7 495 600-60-60"], "Банк", "Персональный менеджер"),
        card("Никитина Светлана", &["svetlana@design.example"], &["+7 921 000-12-34"], "Студия дизайна", "Арт-директор"),
        card("Служба поддержки", &["support@vendor.example"], &["8 800 100-00-00"], "Вендор", ""),
        card("Тарасов Олег", &["oleg.tarasov@mail.example"], &["+7 905 111-22-33"], "", ""),
    ];

    /// Человек справочника демо: имя, адрес, тип Principal (`individual`, `location` — переговорная, `resource`), должность,
    /// отдел и телефон (S7-08: справочник Exchange их даёт).
    pub struct Person {
        pub name: &'static str,
        pub email: &'static str,
        pub kind: &'static str,
        pub title: &'static str,
        pub department: &'static str,
        pub phone: &'static str,
    }

    const fn person(name: &'static str, email: &'static str, kind: &'static str) -> Person {
        Person { name, email, kind, title: "", department: "", phone: "" }
    }

    const fn staff(name: &'static str, email: &'static str, title: &'static str, department: &'static str, phone: &'static str) -> Person {
        Person { name, email, kind: "individual", title, department, phone }
    }

    /// Справочник.
    const DIRECTORY: &[Person] = &[
        person("Бухгалтерия", "buh@corp.example", "individual"),
        staff("Волков Сергей", "s.volkov@corp.example", "Системный администратор", "Отдел ИТ", "+7 495 100-20-31"),
        staff("Иванова Мария", "m.ivanova@corp.example", "Руководитель проектов", "Проектный офис", "+7 495 100-20-12"),
        staff("Кузнецова Елена", "e.kuznetsova@corp.example", "Главный бухгалтер", "Бухгалтерия", "+7 495 100-20-05"),
        person("Переговорная 1", "room1@corp.example", "location"),
        person("Переговорная 2", "room2@corp.example", "location"),
        person("Переговорная 3", "room3@corp.example", "location"),
        staff("Петров Алексей", "a.petrov@corp.example", "Ведущий инженер", "Отдел разработки", "+7 495 100-20-44"),
        staff("Петров Иван", "ivan.petrov@corp.example", "Инженер", "Отдел разработки", ""),
        person("Проектор в учебном классе", "projector@corp.example", "resource"),
        staff("Сидоров Игорь", "i.sidorov@corp.example", "Аналитик", "Отдел продаж", "+7 495 100-20-57"),
        staff("Смирнова Анна", "anna.smirnova@corp.example", "Юрист", "Юридический отдел", "+7 495 100-20-68"),
        staff("Фёдорова Наталья", "n.fedorova@corp.example", "Менеджер по персоналу", "Отдел кадров", ""),
        staff("Щукин Алексей", "a.shchukin@corp.example", "Директор по развитию", "Дирекция", "+7 495 100-20-01"),
    ];

    pub fn mine() -> &'static [Card] {
        MINE
    }

    pub fn directory() -> &'static [Person] {
        DIRECTORY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chart_letter_has_a_large_inline_png() {
        let b = body("demo104");
        assert!(b.html.as_deref().is_some_and(|h| h.contains("cid:chart@corp")));
        assert_eq!(b.attachments.len(), 2);
        for (att, blob_id, size) in [(&b.attachments[0], "chart", CHART_SIZE), (&b.attachments[1], "sign-logo", SIGN_LOGO_SIZE)] {
            let data = blob(blob_id).unwrap();
            assert_eq!(att.size, data.len() as u64);
            let info = png::Decoder::new(std::io::Cursor::new(data)).read_info().unwrap();
            assert_eq!((info.info().width, info.info().height), size);
        }
    }

    /// T1-10г: подпись таблицей, логотип со своим размером в HTML меньше кадра.
    #[test]
    fn chart_letter_signs_with_a_sized_logo() {
        let html = body("demo4").html.unwrap_or_default();
        assert!(html.contains(r#"<img src="cid:sign@corp" width=180 height=60"#), "{html}");
        assert!(html.contains("<table"));
    }

    /// Три учётки демо (S7-09а): основная JMAP, «стандарты», Exchange — у каждой свои функции, адреса вымышленные.
    #[test]
    fn three_accounts_with_their_features() {
        assert_eq!(ACCOUNTS.map(|a| a.kind), [Kind::Jmap, Kind::Standards, Kind::Exchange]);
        assert_eq!(account(Some(SECOND.0)).map(|a| a.email), Some(SECOND.1));
        assert!(ACCOUNTS[1..].iter().all(|a| a.email.ends_with(".test") && a.host.ends_with("example.test")));
        // Функции разные: у каждой дополнительной учётки чего-то нет, и наборы не совпадают.
        assert!(ACCOUNTS[1..].iter().all(|a| a.has != MAIN.has) && STANDARDS.has != EXCHANGE.has);
        let owners: Vec<String> = calendar::account_calendars().into_iter().map(|o| o.email).collect();
        assert_eq!(owners, [STANDARDS.email, EXCHANGE.email]);
        assert!(calendar::account_events().iter().all(|e| e.owner.as_ref().is_some_and(|o| owners.contains(&o.email))));
    }

    /// Демо-ящик: группы учёток с их папками и письмами; у Exchange — без категорий.
    #[test]
    fn mailbox_has_extra_accounts() {
        let dir = std::env::temp_dir().join(format!("postman-demo-accounts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("demo.redb");
        ensure(&path, 50).unwrap();
        let cache = Cache::open(&path).unwrap();
        let ids: Vec<String> = cache.accounts().unwrap().into_iter().map(|a| a.id).collect();
        assert!(ids.iter().any(|id| id == "@2") && ids.iter().any(|id| id == "@3"));
        let boxes = cache.mailboxes().unwrap();
        assert!(boxes.iter().any(|b| b.id == scoped(Some("@3"), "junk") && b.name == "Нежелательная почта"));
        let mail = cache.rows_by_ids(&[scoped(Some("@3"), "demo500"), scoped(Some("@2"), "demo500")]).unwrap();
        assert_eq!(mail.len(), 2);
        assert!(mail.iter().any(|m| m.id.starts_with("@3") && m.keywords.is_empty()));
        drop(cache);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Вложенные папки (T1-04е): до глубины 3 под «Входящими» и под своей папкой основной учётки и у Exchange;
    /// у каждого родителя есть строка той же учётки.
    #[test]
    fn nested_folders_up_to_depth_three() {
        let dir = std::env::temp_dir().join(format!("postman-demo-nested-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("demo.redb");
        ensure(&path, 50).unwrap();
        let boxes = Cache::open(&path).unwrap().mailboxes().unwrap();
        let by_id = |id: &str| boxes.iter().find(|b| b.id == id);
        // Глубина и корень цепочки родителей; у каждого родителя есть строка.
        let chain = |b: &MailboxRow| {
            let (mut depth, mut top) = (0, b.id.clone());
            let mut parent = b.parent_id.clone();
            while let Some(p) = parent {
                let row = by_id(&p).unwrap_or_else(|| panic!("нет родителя {p} у {}", b.id));
                assert!(depth < 8, "цикл родителей у {}", b.id);
                (depth, top, parent) = (depth + 1, row.id.clone(), row.parent_id.clone());
            }
            (depth, top)
        };
        let deepest = |top: &str| boxes.iter().map(chain).filter(|(_, t)| t == top).map(|(d, _)| d).max().unwrap();
        assert_eq!(deepest("inbox"), 3);
        assert_eq!(deepest("projects"), 3);
        assert_eq!(deepest(&scoped(EXCHANGE.profile, "inbox")), 3);
        assert!(boxes.iter().all(|b| chain(b).0 <= 3));
        assert!(by_id("projects").is_some_and(|b| b.role.is_none() && b.parent_id.is_none()));
        // Родитель — той же учётки.
        let profile = |id: &str| id.split_once('/').map(|(p, _)| p.to_owned());
        assert!(boxes.iter().filter_map(|b| b.parent_id.as_deref().map(|p| (b, p))).all(|(b, p)| profile(&b.id) == profile(p)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Приглашение демо — на завтра, тот же размер файла; фиксированной даты в нём нет.
    #[test]
    fn demo_invite_is_tomorrow() {
        let ics = String::from_utf8(invite()).unwrap();
        assert_eq!(ics.len(), INVITE.len());
        assert!(!ics.contains(INVITE_DAY));
        let t = jiff::Zoned::now().date().tomorrow().unwrap();
        assert!(ics.contains(&format!("DTSTART;TZID=Russian Standard Time:{:04}{:02}{:02}T110000", t.year(), t.month(), t.day())));
    }
}
