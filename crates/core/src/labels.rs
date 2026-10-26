//! Метки и категории на ключевых словах JMAP (S5-02, `docs/DESIGN.md` «Строка письма и метки»).
//!
//! Ключевое слово JMAP — ASCII без пробелов и `( ) { ] % * " \`, в нижнем регистре (RFC 8621 §4.1.1), а имена
//! категорий — любые («Клиенты»). Поэтому на сервере у письма не имя, а id категории: ключевое слово
//! `pm.cat.<id>` (id — `[a-z0-9-]`), имя и цвет — в таблице категорий аккаунта. Без `$`: так помечают слова
//! с общим смыслом (`$seen`, `$flagged`, `$label1`), а это — слово Postman.
//!
//! Совместимость: `$seen`, `$flagged`, `$answered`, `$forwarded`, `$draft` — состояния письма, меток в строке у них
//! нет (флаг — значком). Метки Thunderbird `$label1…$label5` («Важное», «Работа», «Личное», «Сделать», «Позже») —
//! встроенные категории: видны у письма нейтральной меткой; в таблицу их можно взять с цветом (ключевое слово
//! категории — как есть, `$label1`).
//!
//! Таблица категорий — у аккаунта на сервере (решение владельца №4): файл JMAP FileNode, у общего ящика — свой
//! (`crate::categories`). Правки — методами [`Categories`] (настройки, S5-09) с меткой времени; две правки с разных
//! клиентов сводятся [`Categories::merge`]: у каждой категории побеждает более поздняя. Удалённая категория остаётся
//! в таблице скрытой: слово у писем не трогается, а другой клиент не вернёт её своей старой копией. Локальный файл
//! ([`LocalFile`], [`Book`]) — кэш и запас, когда сервер недоступен или FileNode у него нет.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::palette;

pub const SEEN: &str = "$seen";
pub const FLAGGED: &str = "$flagged";
pub const ANSWERED: &str = "$answered";
pub const FORWARDED: &str = "$forwarded";
pub const DRAFT: &str = "$draft";
/// Префикс ключевого слова категории Postman.
pub const CATEGORY_PREFIX: &str = "pm.cat.";
/// Метки Thunderbird по номеру: `$label1` — 1.
pub const THUNDERBIRD: [&str; 5] = ["$label1", "$label2", "$label3", "$label4", "$label5"];
/// Больше меток в строке письма не помещается (DESIGN: имя отправителя ≥ 60 px).
pub const ROW_LABELS: usize = 2;

/// Что значит ключевое слово письма.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keyword<'a> {
    Seen,
    Flagged,
    Answered,
    Forwarded,
    Draft,
    /// Категория Postman: id из `pm.cat.<id>`.
    Category(&'a str),
    /// Метка Thunderbird `$labelN`, N = 1…5.
    Thunderbird(u8),
    /// Чужое ключевое слово (`$junk`, `$mdnsent`, слово другого клиента) — не показываем, не трогаем.
    Other(&'a str),
}

/// Разбор ключевого слова (регистр не важен: в JMAP он всегда нижний, но IMAP-клиенты пишут как придётся).
pub fn classify(keyword: &str) -> Keyword<'_> {
    let lower = |s: &str| keyword.eq_ignore_ascii_case(s);
    if lower(SEEN) {
        Keyword::Seen
    } else if lower(FLAGGED) {
        Keyword::Flagged
    } else if lower(ANSWERED) {
        Keyword::Answered
    } else if lower(FORWARDED) {
        Keyword::Forwarded
    } else if lower(DRAFT) {
        Keyword::Draft
    } else if let Some(n) = THUNDERBIRD.iter().position(|t| lower(t)) {
        Keyword::Thunderbird(n as u8 + 1)
    } else if let Some(id) = keyword.get(..CATEGORY_PREFIX.len()).filter(|p| p.eq_ignore_ascii_case(CATEGORY_PREFIX)) {
        let id = &keyword[id.len()..];
        if valid_id(id) { Keyword::Category(id) } else { Keyword::Other(keyword) }
    } else {
        Keyword::Other(keyword)
    }
}

/// id категории: 1–64 знака `[a-z0-9-]`.
pub fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Ключевое слово категории с id `id`.
pub fn category_keyword(id: &str) -> String {
    format!("{CATEGORY_PREFIX}{id}")
}

/// Допустимое ключевое слово JMAP: 1–255 знаков `%x21-7E` без `( ) { ] % * " \`, без заглавных (RFC 8621 §4.1.1).
pub fn valid_keyword(k: &str) -> bool {
    (1..=255).contains(&k.len()) && k.bytes().all(|b| (0x21..=0x7e).contains(&b) && !b"(){]%*\"\\".contains(&b) && !b.is_ascii_uppercase())
}

/// Ключевое слово в виде, в котором его хранит JMAP (нижний регистр).
pub fn normalize(k: &str) -> String {
    k.to_ascii_lowercase()
}

/// Категория таблицы аккаунта.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Category {
    /// Ключевое слово на сервере: `pm.cat.<id>` или взятая в таблицу метка Thunderbird (`$label1`). Это и id
    /// категории при сведении правок.
    pub keyword: String,
    pub name: String,
    /// Цвет — имя цвета таблицы DESIGN (`blue`, [`palette::PALETTE`]); прежнее имя Fluent 2 рисуется ближайшим цветом
    /// таблицы ([`palette::LEGACY`]), неизвестное — нейтральным.
    pub color: String,
    /// Клавиша 1–9, которая ставит и снимает категорию на выбранном письме.
    #[serde(default)]
    pub key: Option<u8>,
    /// Когда правили (Unix, мс): из двух правок одной категории побеждает поздняя. 0 — не правили (по умолчанию).
    #[serde(default)]
    pub changed: i64,
    /// Удалена: не показывается, клавиша свободна; ключевое слово у писем остаётся.
    #[serde(default, skip_serializing_if = "is_false")]
    pub hidden: bool,
}

impl Category {
    /// Цвет для отрисовки и проверок — имя цвета таблицы. Прежнее имя Fluent у категории по умолчанию — её цвет по
    /// умолчанию (Тендеры — розовый, Пентест — графит, Сервисы — серый; решение 2026-10-22), у остальных — ближайший
    /// по ΔE ([`palette::resolve`]); неизвестное — нейтральный.
    pub fn shown_color(&self) -> &'static str {
        let legacy = palette::LEGACY.iter().any(|(old, _)| *old == self.color);
        let default = || DEFAULTS.iter().find(|(id, _, _)| category_keyword(id) == self.keyword).map(|d| d.1);
        match legacy.then(default).flatten() {
            Some(color) => color,
            None => palette::resolve(&self.color),
        }
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Ошибка в таблице категорий или в правке.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum Problem {
    #[error("{0}: not a JMAP keyword")]
    BadKeyword(String),
    #[error("{0}: used twice")]
    DuplicateKeyword(String),
    #[error("key {0}: used twice")]
    DuplicateKey(u8),
    #[error("key {0}: not 1-9")]
    BadKey(u8),
    #[error("{name}: colour too close ({clash:?}, ΔE {delta:.1})")]
    Colour { name: String, clash: palette::Clash, delta: f64 },
    #[error("name is empty or longer than {NAME_MAX} characters")]
    BadName,
    #[error("{0}: name used twice")]
    DuplicateName(String),
    #[error("{0}: no such category")]
    NotFound(String),
}

/// Имя категории — не длиннее (знаков).
pub const NAME_MAX: usize = 64;

/// Таблица категорий одного аккаунта; порядок — порядок меток в строке и фильтров.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Categories {
    pub list: Vec<Category>,
}

/// Категории по умолчанию (DESIGN.md «Цвет»): id, цвет, клавиша. Имена — у интерфейса (Fluent).
pub const DEFAULTS: &[(&str, &str, u8)] = &[
    ("clients", palette::BLUE, 1),
    ("team", palette::GREEN, 2),
    ("tenders", palette::PINK, 3),
    ("pentest", palette::GRAPHITE, 4),
    ("services", palette::NEUTRAL, 5),
];

impl Categories {
    /// Таблица по умолчанию; `name(id)` — имя категории на языке интерфейса.
    pub fn defaults(name: impl Fn(&str) -> String) -> Self {
        let list = DEFAULTS
            .iter()
            .map(|&(id, color, key)| Category {
                keyword: category_keyword(id),
                name: name(id),
                color: color.to_owned(),
                key: Some(key),
                changed: 0,
                hidden: false,
            })
            .collect();
        Self { list }
    }

    /// Видимые категории (без удалённых) в порядке таблицы.
    pub fn visible(&self) -> impl Iterator<Item = &Category> {
        self.list.iter().filter(|c| !c.hidden)
    }

    /// Место категории в таблице, в том числе удалённой.
    fn find(&self, keyword: &str) -> Option<usize> {
        self.list.iter().position(|c| c.keyword.eq_ignore_ascii_case(keyword))
    }

    fn find_visible(&self, keyword: &str) -> Result<usize, Problem> {
        self.find(keyword).filter(|&i| !self.list[i].hidden).ok_or_else(|| Problem::NotFound(keyword.to_owned()))
    }

    /// Видимая категория по ключевому слову и её место в таблице.
    pub fn by_keyword(&self, keyword: &str) -> Option<(usize, &Category)> {
        self.find(keyword).map(|i| (i, &self.list[i])).filter(|(_, c)| !c.hidden)
    }

    /// Категория события календаря (S5-11) по его меткам — JSCalendar `keywords` и `categories`: ключевое слово
    /// категории (`pm.cat.<id>`, `$labelN` из таблицы) или имя категории без учёта регистра (так их ставят Outlook и
    /// другие клиенты). Несколько — первая по порядку таблицы; удалённые не считаются.
    pub fn for_event(&self, keywords: &[String]) -> Option<&Category> {
        let name = |c: &Category, k: &str| c.name.trim().to_lowercase() == k.trim().to_lowercase();
        self.visible().find(|c| keywords.iter().any(|k| c.keyword.eq_ignore_ascii_case(k) || name(c, k)))
    }

    /// Видимая категория на клавише `key`.
    pub fn by_key(&self, key: u8) -> Option<&Category> {
        self.visible().find(|c| c.key == Some(key))
    }

    /// Ключевое слово для новой категории: `pm.cat.c<время в base36>` — две категории, добавленные на разных клиентах,
    /// не получат одно слово; свободное в таблице (и среди удалённых).
    pub fn new_keyword(&self, now: i64) -> String {
        let start = u64::try_from(now).unwrap_or(0);
        (start..).map(|n| category_keyword(&format!("c{}", base36(n)))).find(|k| self.find(k).is_none()).unwrap_or_default()
    }

    /// Таблица годится: ключевые слова JMAP без повторов, у видимых — клавиши 1–9 без повторов и цвета различимы
    /// (ΔE2000, [`palette::check`]).
    pub fn check(&self) -> Result<(), Problem> {
        let mut colors: Vec<&str> = Vec::new();
        for (i, c) in self.list.iter().enumerate() {
            if !valid_keyword(&c.keyword) {
                return Err(Problem::BadKeyword(c.keyword.clone()));
            }
            if self.list[..i].iter().any(|o| o.keyword.eq_ignore_ascii_case(&c.keyword)) {
                return Err(Problem::DuplicateKeyword(c.keyword.clone()));
            }
            if c.hidden {
                continue;
            }
            if let Some(k) = c.key {
                if !(1..=9).contains(&k) {
                    return Err(Problem::BadKey(k));
                }
                if self.list[..i].iter().any(|o| !o.hidden && o.key == Some(k)) {
                    return Err(Problem::DuplicateKey(k));
                }
            }
            if let Some((clash, delta)) = palette::clash(c.shown_color(), &colors) {
                return Err(Problem::Colour { name: c.name.clone(), clash, delta });
            }
            colors.push(c.shown_color());
        }
        Ok(())
    }

    /// Метки письма с ключевыми словами `keywords` для строки списка: категории в порядке таблицы, затем метки
    /// Thunderbird, которых нет в таблице. Первая — категория строки (полоса слева). Удалённые категории не видны.
    pub fn labels<'a>(&'a self, keywords: &[String]) -> Vec<Label<'a>> {
        let mut cats: Vec<(usize, &Category)> = keywords.iter().filter_map(|k| self.by_keyword(k)).collect();
        cats.sort_by_key(|(i, _)| *i);
        cats.dedup_by_key(|(i, _)| *i);
        let mut out: Vec<Label<'a>> = cats.into_iter().map(|(_, c)| Label::Category(c)).collect();
        let mut tb: Vec<u8> = keywords
            .iter()
            .filter(|k| self.find(k).is_none())
            .filter_map(|k| match classify(k) {
                Keyword::Thunderbird(n) => Some(n),
                _ => None,
            })
            .collect();
        tb.sort_unstable();
        tb.dedup();
        out.extend(tb.into_iter().map(Label::Thunderbird));
        out
    }

    // --- правка (настройки, S5-09) ---

    /// Палитра для выбора цвета категории `keyword` (`None` — новой): цвета, которые можно дать категориям
    /// ([`palette::usable`]), и для каждого — с чем он близок в этой таблице (`None` — цвет свободен).
    pub fn colors(&self, keyword: Option<&str>) -> Vec<(&'static str, Option<(palette::Clash, f64)>)> {
        let except = keyword.and_then(|k| self.find(k));
        let used = self.used_colors(except);
        palette::usable().iter().map(|&key| (key, palette::clash(key, &used))).collect()
    }

    /// Цвета видимых категорий, кроме `except`.
    fn used_colors(&self, except: Option<usize>) -> Vec<&str> {
        self.list.iter().enumerate().filter(|&(i, c)| !c.hidden && Some(i) != except).map(|(_, c)| c.shown_color()).collect()
    }

    fn check_name(&self, name: &str, except: Option<usize>) -> Result<String, Problem> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > NAME_MAX {
            return Err(Problem::BadName);
        }
        let taken = self.list.iter().enumerate().any(|(i, c)| !c.hidden && Some(i) != except && c.name.trim().to_lowercase() == name.to_lowercase());
        if taken {
            return Err(Problem::DuplicateName(name.to_owned()));
        }
        Ok(name.to_owned())
    }

    /// Цвет годится — его имя в таблице (прежнее имя Fluent — новым, [`palette::resolve`]).
    fn check_color(&self, name: &str, color: &str, except: Option<usize>) -> Result<&'static str, Problem> {
        match palette::clash(color, &self.used_colors(except)) {
            Some((clash, delta)) => Err(Problem::Colour { name: name.to_owned(), clash, delta }),
            None => Ok(palette::resolve(color)),
        }
    }

    fn check_key(&self, key: Option<u8>, except: Option<usize>) -> Result<(), Problem> {
        let Some(k) = key else { return Ok(()) };
        if !(1..=9).contains(&k) {
            return Err(Problem::BadKey(k));
        }
        if self.list.iter().enumerate().any(|(i, c)| !c.hidden && Some(i) != except && c.key == Some(k)) {
            return Err(Problem::DuplicateKey(k));
        }
        Ok(())
    }

    /// Новая категория в конец таблицы; ответ — её ключевое слово. Цвет ближе ΔE 25 к цвету другой категории
    /// или к занятому — отказ ([`Problem::Colour`]), клавиша занята — [`Problem::DuplicateKey`].
    pub fn add(&mut self, name: &str, color: &str, key: Option<u8>, now: i64) -> Result<String, Problem> {
        let keyword = self.new_keyword(now);
        self.adopt(&keyword, name, color, key, now)?;
        Ok(keyword)
    }

    /// Взять в таблицу ключевое слово как есть (метка Thunderbird `$label2`) или вернуть удалённую категорию.
    pub fn adopt(&mut self, keyword: &str, name: &str, color: &str, key: Option<u8>, now: i64) -> Result<(), Problem> {
        let keyword = normalize(keyword);
        if !valid_keyword(&keyword) {
            return Err(Problem::BadKeyword(keyword));
        }
        let at = self.find(&keyword);
        if at.is_some_and(|i| !self.list[i].hidden) {
            return Err(Problem::DuplicateKeyword(keyword));
        }
        let name = self.check_name(name, at)?;
        let color = self.check_color(&name, color, at)?;
        self.check_key(key, at)?;
        let c = Category { keyword, name, color: color.to_owned(), key, changed: now, hidden: false };
        match at {
            Some(i) => self.list[i] = c,
            None => self.list.push(c),
        }
        Ok(())
    }

    pub fn rename(&mut self, keyword: &str, name: &str, now: i64) -> Result<(), Problem> {
        let i = self.find_visible(keyword)?;
        let name = self.check_name(name, Some(i))?;
        let c = &mut self.list[i];
        c.name = name;
        c.changed = now;
        Ok(())
    }

    /// Другой цвет: ближе ΔE 25 к цвету другой категории таблицы или к занятому — отказ.
    pub fn recolor(&mut self, keyword: &str, color: &str, now: i64) -> Result<(), Problem> {
        let i = self.find_visible(keyword)?;
        let color = self.check_color(&self.list[i].name, color, Some(i))?;
        let c = &mut self.list[i];
        c.color = color.to_owned();
        c.changed = now;
        Ok(())
    }

    /// Клавиша 1–9 или без клавиши (`None`).
    pub fn set_key(&mut self, keyword: &str, key: Option<u8>, now: i64) -> Result<(), Problem> {
        let i = self.find_visible(keyword)?;
        self.check_key(key, Some(i))?;
        let c = &mut self.list[i];
        c.key = key;
        c.changed = now;
        Ok(())
    }

    /// Удалить категорию: скрыть и освободить клавишу. Ключевое слово у писем остаётся — снять его с писем можно
    /// только явно (`categories::Request::Purge`).
    pub fn delete(&mut self, keyword: &str, now: i64) -> Result<(), Problem> {
        let i = self.find_visible(keyword)?;
        let c = &mut self.list[i];
        c.hidden = true;
        c.key = None;
        c.changed = now;
        Ok(())
    }

    /// Свести свою таблицу с чужой (с сервера): у каждой категории (по ключевому слову) побеждает поздняя правка,
    /// при равенстве — чужая. Порядок — чужой, свои новые — в конце. Клавиша, которая после сведения оказалась у двух
    /// видимых категорий, остаётся у правленной позже.
    pub fn merge(&self, theirs: &Categories) -> Categories {
        let mut list: Vec<Category> = theirs
            .list
            .iter()
            .map(|t| match self.find(&t.keyword).map(|i| &self.list[i]) {
                Some(mine) if mine.changed > t.changed => mine.clone(),
                _ => t.clone(),
            })
            .collect();
        list.extend(self.list.iter().filter(|m| theirs.find(&m.keyword).is_none()).cloned());
        for k in 1..=9u8 {
            let holders: Vec<usize> = (0..list.len()).filter(|&i| !list[i].hidden && list[i].key == Some(k)).collect();
            if let Some(&keep) = holders.iter().max_by(|&&a, &&b| list[a].changed.cmp(&list[b].changed).then(b.cmp(&a))) {
                for &i in holders.iter().filter(|&&i| i != keep) {
                    list[i].key = None;
                }
            }
        }
        Categories { list }
    }
}

fn base36(mut n: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    loop {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
        if n == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// Метка в строке письма.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Label<'a> {
    Category(&'a Category),
    /// Метка Thunderbird вне таблицы: имя — у интерфейса, цвет — нейтральный.
    Thunderbird(u8),
}

impl Label<'_> {
    /// Цвет метки (имя Fluent; неизвестный — нейтральный).
    pub fn color(&self) -> &'static str {
        match self {
            Label::Category(c) => c.shown_color(),
            Label::Thunderbird(_) => palette::NEUTRAL,
        }
    }
}

/// Переключить ключевое слово у писем: если у всех оно есть — снять, иначе — поставить (как категория в Outlook
/// на нескольких выбранных письмах). Ответ — (поставить, снять).
pub fn toggle<'a>(rows: impl IntoIterator<Item = &'a [String]>, keyword: &str) -> (Vec<String>, Vec<String>) {
    let all = rows.into_iter().all(|kws| kws.iter().any(|k| k.eq_ignore_ascii_case(keyword)));
    let k = vec![normalize(keyword)];
    if all { (Vec::new(), k) } else { (k, Vec::new()) }
}

/// Таблицы категорий по аккаунтам — локальная копия (кэш) таблиц с сервера: ключ — id аккаунта на сервере, свой
/// аккаунт — [`OWN`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Book {
    /// Формат файла: растёт при несовместимых изменениях.
    pub version: u32,
    pub accounts: BTreeMap<String, Categories>,
    /// Аккаунты, где правка ещё не дошла до сервера (сервер недоступен): при следующем входе она уйдёт на сервер
    /// (сведением с серверной таблицей), а не будет затёрта ею.
    #[serde(default)]
    pub pending: BTreeSet<String>,
}

/// Ключ своего аккаунта в [`Book::accounts`].
pub const OWN: &str = "";
/// 2 — цвета по именам Fluent, метки времени и удалённые категории (S5-02б); файл версии 1 — не читается (кэш).
pub const BOOK_VERSION: u32 = 2;

impl Book {
    /// Только таблица своего аккаунта.
    pub fn own(table: Categories) -> Self {
        Self { version: BOOK_VERSION, accounts: BTreeMap::from([(OWN.to_owned(), table)]), pending: BTreeSet::new() }
    }

    /// Ключ аккаунта: `None` — свой.
    pub fn key(account: Option<&str>) -> String {
        account.unwrap_or(OWN).to_owned()
    }

    /// Таблица аккаунта (`None` — свой); у общего ящика её ещё нет (не загружена) — своя.
    pub fn for_account(&self, account: Option<&str>) -> Option<&Categories> {
        account.and_then(|a| self.accounts.get(a)).or_else(|| self.accounts.get(OWN))
    }

    /// Правка таблицы в настройках: таблица — новая, до ответа сервера аккаунт ждёт отправки (`pending`). Отправить —
    /// `categories::Request::Save`.
    pub fn edit(&mut self, account: Option<&str>, table: Categories) {
        let key = Book::key(account);
        self.pending.insert(key.clone());
        self.accounts.insert(key, table);
    }

    /// Таблица с сервера при входе или после правки на другом клиенте (`categories::Reply::Loaded`; `None` — файла
    /// на сервере нет, берётся `defaults`). Своя правка ещё не дошла до сервера — таблица не заменяется, а ответ —
    /// своя таблица, которую надо отправить (`Request::Save` сведёт её с серверной).
    pub fn loaded(&mut self, account: Option<&str>, table: Option<Categories>, defaults: impl FnOnce() -> Categories) -> Option<Categories> {
        let key = Book::key(account);
        if self.pending.contains(&key)
            && let Some(mine) = self.accounts.get(&key)
        {
            return Some(mine.clone());
        }
        self.accounts.insert(key, table.unwrap_or_else(defaults));
        None
    }

    /// Таблица записана на сервер (`categories::Reply::Saved`): так она теперь выглядит там (после сведения).
    pub fn saved(&mut self, account: Option<&str>, table: Categories) {
        let key = Book::key(account);
        self.pending.remove(&key);
        self.accounts.insert(key, table);
    }
}

/// Ошибка хранилища таблиц.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("labels file: {0}")]
    Io(#[from] std::io::Error),
    #[error("labels file: {0}")]
    Json(#[from] serde_json::Error),
    #[error("labels file version {0} is newer than this Postman")]
    Version(u32),
}

/// Где лежит локальная копия таблиц.
pub trait Store: Send + Sync {
    /// `None` — копии ещё нет (первый запуск) или она старого формата: интерфейс берёт [`Categories::defaults`],
    /// пока не ответит сервер.
    fn load(&self) -> Result<Option<Book>, StoreError>;
    fn save(&self, book: &Book) -> Result<(), StoreError>;
}

/// Таблицы в файле JSON рядом с кэшем учётной записи (кэш пересоздаётся при смене формата — таблица в нём бы
/// пропала, поэтому отдельно).
pub struct LocalFile(pub PathBuf);

impl LocalFile {
    /// Файл таблиц для кэша `cache`: `<кэш>.labels.json`.
    pub fn beside(cache: &Path) -> Self {
        let mut name = cache.file_name().unwrap_or_default().to_os_string();
        name.push(".labels.json");
        Self(cache.with_file_name(name))
    }
}

#[derive(Deserialize)]
struct Version {
    #[serde(default)]
    version: u32,
}

impl Store for LocalFile {
    fn load(&self) -> Result<Option<Book>, StoreError> {
        let text = match std::fs::read_to_string(&self.0) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let Version { version } = serde_json::from_str(&text)?;
        if version > BOOK_VERSION {
            return Err(StoreError::Version(version));
        }
        // Старый формат — только копия: таблицу пришлёт сервер.
        if version < BOOK_VERSION {
            return Ok(None);
        }
        Ok(Some(serde_json::from_str(&text)?))
    }

    /// Запись через временный файл: сбой посреди записи не портит прежнюю таблицу.
    fn save(&self, book: &Book) -> Result<(), StoreError> {
        let text = serde_json::to_string_pretty(book)?;
        let mut tmp = self.0.clone().into_os_string();
        tmp.push(".tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &self.0)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kws(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn defaults() -> Categories {
        Categories::defaults(|id| format!("[{id}]"))
    }

    /// Ключевое слово категории — ASCII JMAP, туда и обратно; чужие и системные слова узнаются.
    #[test]
    fn keywords_round_trip() {
        for id in ["clients", "c12", "a-b"] {
            let k = category_keyword(id);
            assert!(valid_keyword(&k), "{k}");
            assert_eq!(classify(&k), Keyword::Category(id));
        }
        assert_eq!(classify("PM.Cat.clients"), Keyword::Category("clients"));
        assert_eq!(classify("pm.cat.Клиенты"), Keyword::Other("pm.cat.Клиенты"));
        assert_eq!(classify("pm.cat."), Keyword::Other("pm.cat."));
        assert_eq!(classify("$Flagged"), Keyword::Flagged);
        assert_eq!(classify("$seen"), Keyword::Seen);
        assert_eq!(classify("$answered"), Keyword::Answered);
        assert_eq!(classify("$forwarded"), Keyword::Forwarded);
        assert_eq!(classify("$draft"), Keyword::Draft);
        assert_eq!(classify("$label3"), Keyword::Thunderbird(3));
        assert_eq!(classify("$Label5"), Keyword::Thunderbird(5));
        assert_eq!(classify("$label6"), Keyword::Other("$label6"));
        assert_eq!(classify("$junk"), Keyword::Other("$junk"));
        assert!(!valid_keyword("Клиенты") && !valid_keyword("a b") && !valid_keyword("") && !valid_keyword("Pm.cat.x"));
        assert!(!valid_keyword("a(b") && !valid_keyword("a]") && !valid_keyword(&"x".repeat(256)));
        assert!(!valid_id("Clients") && !valid_id("") && !valid_id("a.b") && !valid_id(&"a".repeat(65)));
        assert_eq!(normalize("$Label1"), "$label1");
    }

    #[test]
    fn default_table_is_valid() {
        let t = defaults();
        assert_eq!(t.check(), Ok(()));
        assert_eq!(t.list.len(), 5);
        assert_eq!(t.by_key(1).map(|c| c.keyword.as_str()), Some("pm.cat.clients"));
        assert_eq!(t.by_key(5).map(|c| c.color.as_str()), Some(palette::NEUTRAL));
        assert_eq!(t.by_key(6), None);
        assert_eq!(t.list[1].name, "[team]");
        assert_eq!(t.new_keyword(0), "pm.cat.c0");
        let k = t.new_keyword(1_761_000_000_000);
        assert!(valid_keyword(&k) && matches!(classify(&k), Keyword::Category(_)), "{k}");
    }

    #[test]
    fn bad_tables_are_rejected() {
        let mut t = defaults();
        t.list[1].key = Some(1);
        assert_eq!(t.check(), Err(Problem::DuplicateKey(1)));
        let mut t = defaults();
        t.list[0].key = Some(10);
        assert_eq!(t.check(), Err(Problem::BadKey(10)));
        let mut t = defaults();
        t.list[2].keyword = "PM.CAT.clients".into();
        assert!(matches!(t.check(), Err(Problem::BadKeyword(_))));
        t.list[2].keyword = "pm.cat.clients".into();
        assert!(matches!(t.check(), Err(Problem::DuplicateKeyword(_))));
        // Шестая категория прежним цветом `royalBlue` — это графит, как у «Пентеста».
        let mut t = defaults();
        let keyword = t.new_keyword(1);
        let c = Category { keyword, name: "x".into(), color: "royalBlue".into(), key: Some(6), changed: 1, hidden: false };
        t.list.push(c);
        assert!(matches!(t.check(), Err(Problem::Colour { .. })));
        // Удалённая не мешает ни клавишей, ни цветом.
        t.list[5].hidden = true;
        t.list[5].key = Some(1);
        assert_eq!(t.check(), Ok(()));
    }

    /// Метки строки: категории в порядке таблицы (первая — полоса), затем метки Thunderbird вне таблицы.
    #[test]
    fn row_labels() {
        let t = defaults();
        let row = kws(&["$seen", "pm.cat.services", "$label2", "pm.cat.clients", "$junk", "pm.cat.unknown"]);
        let labels = t.labels(&row);
        let shown: Vec<String> = labels
            .iter()
            .map(|l| match l {
                Label::Category(c) => c.keyword.clone(),
                Label::Thunderbird(n) => format!("tb{n}"),
            })
            .collect();
        assert_eq!(shown, ["pm.cat.clients", "pm.cat.services", "tb2"]);
        assert_eq!(labels[0].color(), palette::BLUE);
        assert_eq!(labels[2].color(), palette::NEUTRAL);
        assert!(t.labels(&kws(&["$seen", "$flagged"])).is_empty());
        // Метка Thunderbird, взятая в таблицу, — категория с цветом.
        let mut t = t;
        let work = Category { keyword: "$label2".into(), name: "Work".into(), color: "navy".into(), key: None, changed: 1, hidden: false };
        t.list.insert(0, work);
        assert_eq!(t.labels(&row)[0], Label::Category(&t.list[0]));
        assert_eq!(t.labels(&row).len(), 3);
        // Удалённая категория и удалённая метка Thunderbird не видны.
        t.delete("pm.cat.clients", 2).unwrap();
        t.delete("$label2", 2).unwrap();
        let shown: Vec<&str> = t
            .labels(&row)
            .iter()
            .map(|l| match l {
                Label::Category(c) => c.keyword.as_str(),
                Label::Thunderbird(_) => "tb",
            })
            .collect();
        assert_eq!(shown, ["pm.cat.services"]);
        // Неизвестный цвет (таблица из новой версии) рисуется нейтральным.
        t.list[1].color = "chartreuse".into();
        assert_eq!(Label::Category(&t.list[1]).color(), palette::NEUTRAL);
    }

    /// Прежние цвета Fluent: категории по умолчанию — своим цветом по имени, остальные — ближайшим по ΔE.
    #[test]
    fn legacy_colors_of_defaults_follow_their_names() {
        let mut t = defaults();
        let set = |t: &mut Categories, kw: &str, color: &str| {
            let i = t.find(kw).unwrap();
            t.list[i].color = color.into();
        };
        set(&mut t, "pm.cat.tenders", "grape");
        set(&mut t, "pm.cat.pentest", "lightTeal");
        set(&mut t, "pm.cat.services", "mink");
        set(&mut t, "pm.cat.team", "darkGreen");
        let shown = |t: &Categories, kw: &str| t.by_keyword(kw).map(|(_, c)| c.shown_color());
        assert_eq!(shown(&t, "pm.cat.tenders"), Some(palette::PINK));
        assert_eq!(shown(&t, "pm.cat.pentest"), Some(palette::GRAPHITE), "по ΔE был бы серый");
        assert_eq!(shown(&t, "pm.cat.services"), Some(palette::GREY), "по ΔE был бы графит");
        assert_eq!(shown(&t, "pm.cat.team"), Some(palette::GREEN));
        assert_eq!(t.check(), Ok(()));
        // Своя категория с прежним цветом — ближайший по ΔE.
        let k = t.new_keyword(1);
        t.list.push(Category { keyword: k.clone(), name: "x".into(), color: "lightTeal".into(), key: None, changed: 1, hidden: false });
        assert_eq!(shown(&t, &k), Some(palette::GREY));
        // Неизвестный цвет у категории по умолчанию — нейтральный, не её цвет.
        set(&mut t, "pm.cat.tenders", "chartreuse");
        assert_eq!(shown(&t, "pm.cat.tenders"), Some(palette::NEUTRAL));
    }

    /// Правка в настройках: добавить, переименовать, перекрасить с запретом близких цветов, клавиша, удалить.
    #[test]
    fn editing_api() {
        let mut t = defaults();
        // Свободных цветов нет: синий занят «Клиентами», прежнее имя Fluent — тот же синий, неизвестный — отказ.
        assert!(matches!(t.add("Partners", palette::BLUE, None, 10), Err(Problem::Colour { clash: palette::Clash::Color(palette::BLUE), .. })));
        assert!(matches!(t.add("Partners", "cornflower", None, 10), Err(Problem::Colour { clash: palette::Clash::Color(palette::BLUE), .. })));
        assert!(matches!(t.add("Partners", "chartreuse", None, 10), Err(Problem::Colour { clash: palette::Clash::Unknown, .. })));
        // Серый освобождается удалением «Сервисов».
        assert_eq!(t.delete("pm.cat.services", 9), Ok(()));
        assert_eq!(t.add(" [team] ", palette::GREY, None, 10), Err(Problem::DuplicateName("[team]".into())));
        assert_eq!(t.add("Partners", palette::GREY, Some(1), 10), Err(Problem::DuplicateKey(1)));
        assert_eq!(t.add("Partners", palette::GREY, Some(0), 10), Err(Problem::BadKey(0)));
        assert_eq!(t.add("  ", palette::GREY, None, 10), Err(Problem::BadName));
        assert_eq!(t.add(&"я".repeat(NAME_MAX + 1), palette::GREY, None, 10), Err(Problem::BadName));
        assert_eq!(t.list.len(), 5, "отказ ничего не добавляет");
        // Палитра для новой — только цвета таблицы: синий занят, серый свободен.
        let colors = t.colors(None);
        assert_eq!(colors.len(), palette::PALETTE.len());
        let state = |k: &str| colors.iter().find(|(c, _)| *c == k).map(|(_, s)| *s);
        assert!(matches!(state(palette::BLUE), Some(Some((palette::Clash::Color(palette::BLUE), _)))));
        assert_eq!(state(palette::GREY), Some(None));
        assert_eq!(state("red"), None, "красного в выборе нет");

        // Прежнее имя (`lightTeal` — серый) сохраняется именем таблицы.
        let k = t.add(" Partners ", "lightTeal", Some(6), 10).unwrap();
        assert_eq!(t.check(), Ok(()));
        let c = t.by_keyword(&k).unwrap().1;
        assert_eq!((c.name.as_str(), c.color.as_str(), c.key, c.changed), ("Partners", palette::GREY, Some(6), 10));
        // Свой цвет — не помеха себе.
        assert!(t.colors(Some(&k)).iter().any(|(c, s)| *c == palette::GREY && s.is_none()));
        assert_eq!(t.rename(&k, "Партнёры", 11), Ok(()));
        assert_eq!(t.rename(&k, "[clients]", 11), Err(Problem::DuplicateName("[clients]".into())));
        assert!(matches!(t.recolor(&k, palette::BLUE, 12), Err(Problem::Colour { .. })));
        assert_eq!(t.recolor(&k, palette::GREY, 12), Ok(()));
        assert_eq!(t.set_key(&k, Some(4), 13), Err(Problem::DuplicateKey(4)));
        assert_eq!(t.set_key(&k, Some(7), 13), Ok(()));
        assert_eq!(t.by_key(7).map(|c| c.name.as_str()), Some("Партнёры"));
        assert_eq!(t.set_key(&k, None, 14), Ok(()));
        assert_eq!(t.by_key(7), None);
        assert_eq!(t.rename("pm.cat.nope", "x", 1), Err(Problem::NotFound("pm.cat.nope".into())));

        // Удалить: скрыта, клавиша свободна, слово и имя можно взять снова.
        assert_eq!(t.delete("pm.cat.clients", 20), Ok(()));
        assert!(t.by_keyword("pm.cat.clients").is_none() && t.by_key(1).is_none());
        assert_eq!(t.list.len(), 6, "удалённая остаётся в таблице");
        assert_eq!(t.rename("pm.cat.clients", "x", 21), Err(Problem::NotFound("pm.cat.clients".into())));
        assert_eq!(t.delete("pm.cat.clients", 21), Err(Problem::NotFound("pm.cat.clients".into())));
        // Синий освободился.
        assert!(t.colors(None).iter().any(|(c, s)| *c == palette::BLUE && s.is_none()));
        assert_eq!(t.set_key("pm.cat.team", Some(1), 22), Ok(()));
        // Вернуть удалённую — тем же словом.
        assert_eq!(t.adopt("pm.cat.clients", "[clients]", palette::BLUE, Some(9), 23), Ok(()));
        assert_eq!(t.by_key(9).map(|c| c.keyword.as_str()), Some("pm.cat.clients"));
        assert_eq!(t.list.len(), 6);
        assert_eq!(t.adopt("pm.cat.clients", "y", "steel", None, 24), Err(Problem::DuplicateKeyword("pm.cat.clients".into())));
        // Метка Thunderbird в таблицу — со своим словом; свободных цветов нет — сначала освободить.
        assert!(matches!(t.adopt("$label3", "Personal", "plum", None, 25), Err(Problem::Colour { .. })));
        assert_eq!(t.delete(&k, 25), Ok(()));
        assert_eq!(t.adopt("$Label3", "Personal", palette::GREY, None, 25), Ok(()));
        assert_eq!(t.by_keyword("$label3").map(|(_, c)| c.keyword.as_str()), Some("$label3"));
        assert_eq!(t.check(), Ok(()));
    }

    /// Две правки с разных клиентов: у категории побеждает поздняя (при равенстве — серверная), новые с обеих
    /// сторон остаются, удаление поздней правкой — тоже; клавиша на двух — у правленной позже.
    #[test]
    fn merge_is_last_writer_wins_per_category() {
        let base = defaults();
        let mut mine = base.clone();
        let mut theirs = base.clone();
        mine.rename("pm.cat.team", "Mine", 100).unwrap();
        theirs.rename("pm.cat.team", "Theirs", 200).unwrap();
        theirs.rename("pm.cat.tenders", "Theirs tenders", 50).unwrap();
        mine.rename("pm.cat.tenders", "Mine tenders", 60).unwrap();
        theirs.delete("pm.cat.services", 70).unwrap();
        // Цветов в таблице пять: серый для новой освобождается удалением «Сервисов» и у себя.
        mine.delete("pm.cat.services", 65).unwrap();
        let a = mine.add("A", palette::GREY, Some(6), 80).unwrap();
        let b = theirs.add("B", palette::GREY, Some(6), 90).unwrap();
        assert_ne!(a, b);
        let merged = mine.merge(&theirs);
        let name = |t: &Categories, k: &str| t.by_keyword(k).map(|(_, c)| c.name.clone());
        assert_eq!(name(&merged, "pm.cat.team").as_deref(), Some("Theirs"));
        assert_eq!(name(&merged, "pm.cat.tenders").as_deref(), Some("Mine tenders"));
        assert_eq!(name(&merged, "pm.cat.services"), None, "удалена позже — удалена");
        assert_eq!(name(&merged, &a).as_deref(), Some("A"));
        assert_eq!(name(&merged, &b).as_deref(), Some("B"));
        // Порядок — серверный, своя новая — в конце.
        let order: Vec<&str> = merged.list.iter().map(|c| c.keyword.as_str()).collect();
        assert_eq!(order.last(), Some(&a.as_str()));
        assert_eq!(merged.by_key(6).map(|c| c.keyword.clone()), Some(b.clone()), "клавиша 6 — у правленной позже");
        assert_eq!(merged.by_keyword(&a).and_then(|(_, c)| c.key), None);
        // Сведение в обе стороны даёт одни и те же категории.
        let back = theirs.merge(&mine);
        for c in &merged.list {
            assert_eq!(back.list.iter().find(|o| o.keyword == c.keyword), Some(c), "{}", c.keyword);
        }
        // Равное время — серверная.
        let mut x = base.clone();
        let mut y = base.clone();
        x.rename("pm.cat.team", "X", 5).unwrap();
        y.rename("pm.cat.team", "Y", 5).unwrap();
        assert_eq!(name(&x.merge(&y), "pm.cat.team").as_deref(), Some("Y"));
        // Свежее удаление на этом клиенте не воскрешает старая серверная копия.
        let mut z = base.clone();
        z.delete("pm.cat.pentest", 300).unwrap();
        assert_eq!(name(&z.merge(&base), "pm.cat.pentest"), None);
        assert_eq!(base.merge(&base), base);
    }

    #[test]
    fn book_falls_back_to_own_table() {
        let mut book = Book::own(defaults());
        assert_eq!(book.for_account(Some("i")).map(|t| t.list.len()), Some(5));
        book.accounts.insert("i".into(), Categories::default());
        assert_eq!(book.for_account(Some("i")).map(|t| t.list.len()), Some(0));
        assert_eq!(book.for_account(None).map(|t| t.list.len()), Some(5));
        assert_eq!(Book::default().for_account(None), None);
    }

    /// Ответы сервера: таблица заменяет копию; несохранённая правка — не заменяется, а уходит на сервер.
    #[test]
    fn book_follows_the_server_unless_an_edit_is_pending() {
        let mut book = Book::own(defaults());
        let mut server = defaults();
        server.rename("pm.cat.team", "Server", 5).unwrap();
        assert_eq!(book.loaded(None, Some(server.clone()), defaults), None);
        assert_eq!(book.for_account(None), Some(&server));
        // Общий ящик без файла — по умолчанию, не своя таблица.
        assert_eq!(book.loaded(Some("i"), None, defaults), None);
        assert_eq!(book.accounts.get("i"), Some(&defaults()));
        let mut edited = server.clone();
        edited.rename("pm.cat.team", "Edited", 6).unwrap();
        book.edit(None, edited.clone());
        assert!(book.pending.contains(OWN));
        assert_eq!(book.loaded(None, Some(server.clone()), defaults), Some(edited.clone()));
        assert_eq!(book.for_account(None), Some(&edited), "правка не затёрта");
        book.saved(None, edited.merge(&server));
        assert!(book.pending.is_empty());
        assert_eq!(book.for_account(None).and_then(|t| t.by_key(2)).map(|c| c.name.as_str()), Some("Edited"));
    }

    #[test]
    fn local_file_round_trip() {
        let dir = tempfile::Builder::new().prefix("postman-labels-").tempdir().unwrap();
        let store = LocalFile::beside(&dir.path().join("account-x.redb"));
        assert!(store.0.ends_with("account-x.redb.labels.json"));
        assert!(store.load().unwrap().is_none());
        let mut book = Book::own(defaults());
        book.edit(Some("i"), defaults());
        store.save(&book).unwrap();
        assert_eq!(store.load().unwrap(), Some(book));
        std::fs::write(&store.0, r#"{"version": 99, "accounts": {}}"#).unwrap();
        assert!(matches!(store.load(), Err(StoreError::Version(99))));
        // Файл S5-02а (номера цветов) — только копия: не читается, таблицу пришлёт сервер.
        let v1 = r#"{"version": 1, "accounts": {"": {"list": [{"keyword": "pm.cat.team", "name": "T", "color": 2}]}}}"#;
        std::fs::write(&store.0, v1).unwrap();
        assert!(store.load().unwrap().is_none());
        std::fs::write(&store.0, "not json").unwrap();
        assert!(matches!(store.load(), Err(StoreError::Json(_))));
    }

    /// Цвет события календаря — по ключевому слову категории или по её имени; первая по таблице, удалённые — нет.
    #[test]
    fn event_category_by_keyword_or_name() {
        let mut t = Categories::defaults(|id| match id {
            "clients" => "Клиенты".to_owned(),
            other => other.to_owned(),
        });
        let k = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let color = |t: &Categories, list: &[&str]| t.for_event(&k(list)).map(|c| c.color.clone());
        assert_eq!(color(&t, &["pm.cat.clients"]), Some(palette::BLUE.to_owned()));
        assert_eq!(color(&t, &["  КЛИЕНТЫ "]), Some(palette::BLUE.to_owned()), "имя категории, как в Outlook");
        // Две категории — первая по таблице (clients раньше team).
        assert_eq!(t.for_event(&k(&["pm.cat.team", "PM.CAT.CLIENTS"])).map(|c| c.keyword.as_str()), Some("pm.cat.clients"));
        assert_eq!(color(&t, &["чужое", "$junk"]), None);
        assert_eq!(color(&t, &[]), None);
        t.delete("pm.cat.clients", 1).unwrap();
        assert_eq!(t.for_event(&k(&["pm.cat.clients", "pm.cat.team"])).map(|c| c.keyword.as_str()), Some("pm.cat.team"));
    }
}
