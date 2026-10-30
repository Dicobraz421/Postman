//! Настройки пользователя (S5-09, `docs/DESIGN.md` «Настройки»): то, что меняется во вкладке «Настройки» и
//! хранится на этом компьютере. Таблица категорий — не здесь (на сервере, `categories`), правила и автоответ — тоже
//! (`settings`).
//!
//! - [`Prefs`] — общие для пользователя: тема, плотность, язык, быстрые фильтры, сроки и жизненный цикл, чтение,
//!   календарь ([`Calendar`], S5-20), свои клавиши. Файл [`FILE`] в каталоге данных (`%LOCALAPPDATA%\Postman`).
//! - [`AccountPrefs`] — у учётной записи: подписи и шаблоны. Файл рядом с её кэшем ([`AccountPrefs::beside`]).
//!   Хранение на сервере (FileNode, как таблица категорий) — [проверить].
//!
//! Без UI и без gpui: интерфейс читает файл до первого окна (микросекунды), пишет — в фоне ([`Writer`]).
//! Жизненный цикл писем (S5-04) берёт сроки отсюда: [`Lifecycle::wait_reply`], [`Lifecycle::archive_newsletters`],
//! [`Lifecycle::expire_codes`]; порядок и видимость быстрых фильтров — [`QuickFilters::shown`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Файл общих настроек в каталоге данных.
pub const FILE: &str = "settings.json";
/// Формат файлов: растёт при несовместимых изменениях; файл новее — не читается (настройки по умолчанию).
pub const VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("settings file: {0}")]
    Io(#[from] std::io::Error),
    #[error("settings file: {0}")]
    Json(#[from] serde_json::Error),
    #[error("settings file version {0} is newer than this Postman")]
    Version(u32),
}

// --- общие ---

/// Общие настройки пользователя.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub version: u32,
    pub general: General,
    pub quick: QuickFilters,
    pub lifecycle: Lifecycle,
    pub reading: Reading,
    /// «Настройки → Календарь» (S5-20).
    pub calendar: Calendar,
    /// Свои клавиши: id команды → сочетания в записи gpui (`ctrl-shift-r`); пустой список — без клавиши. Нет id —
    /// клавиши по умолчанию из реестра команд.
    pub keys: BTreeMap<String, Vec<String>>,
    /// Ревизия клавиш по умолчанию, к которой приведены `keys`: интерфейс переводит свои клавиши со старой ревизии
    /// (S5-16 сняла Alt, J/K, A/T/N и др.). Нет в файле — 0 (до S5-16).
    #[serde(default)]
    pub keys_rev: u32,
    /// Схема клавиш (S5-17, «Настройки → Клавиатура»): «Outlook» по умолчанию или «Одиночные клавиши».
    pub key_scheme: KeyScheme,
    /// KeyTips (S5-18): удержание Ctrl показывает клавиши над кнопками; выключатель — «Внешний вид» (S5-30).
    pub keytips: bool,
    /// «Внешний вид» (S5-30): размер шрифта, аватары, превью, анимации, задержка KeyTips.
    pub look: Look,
    /// «Безопасность» (S5-30).
    pub security: Security,
    /// Свои клавиши столбца «Одиночные клавиши» (как `keys`, но для слоя схемы); действуют, когда она выбрана.
    pub keys_single: BTreeMap<String, Vec<String>>,
    /// Ширины панелей главного окна (T1-05а): их тянут разделители.
    pub layout: Layout,
    /// «Просмотр вложений» (S8-26): модули просмотра.
    pub viewers: Viewers,
}

/// Схема клавиш.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyScheme {
    /// Как Outlook: Ctrl + буква — команды, без модификатора — навигация.
    #[default]
    Outlook,
    /// Как Gmail: ещё одиночные буквы (J/K, R, A, F, E…) вне полей ввода; клавиши Outlook действуют и в ней.
    Single,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            version: VERSION,
            general: General::default(),
            quick: QuickFilters::default(),
            lifecycle: Lifecycle::default(),
            reading: Reading::default(),
            calendar: Calendar::default(),
            keys: BTreeMap::new(),
            keys_rev: 0,
            key_scheme: KeyScheme::Outlook,
            look: Look::default(),
            security: Security::default(),
            keytips: true,
            keys_single: BTreeMap::new(),
            layout: Layout::default(),
            viewers: Viewers::default(),
        }
    }
}

/// Вид: тема, плотность, язык; запуск, уведомления, обновления, журнал (S5-30). `None` — по умолчанию («как в ОС»,
/// 24 px, язык ОС).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    /// Id темы pets-ui: `system`, `fluent-light`, `fluent-dark`, `high-contrast` или своей.
    pub theme: Option<String>,
    /// Высота строки, px: 20, 24, 28 или 32.
    pub density: Option<u16>,
    /// `ru`, `en`; `None` — язык Windows.
    pub language: Option<String>,
    /// «Сообщить о фишинге» (S5-08): письмо ещё и вложением на этот адрес (служба безопасности); `None` — только в
    /// «Нежелательные» с меткой.
    pub phishing_report: Option<String>,
    /// Запускать вместе с Windows (HKCU Run, окно свёрнутым).
    pub autostart: bool,
    /// Что открыть при запуске.
    pub start: StartView,
    /// Уведомления Windows о новых письмах.
    pub notify: Notify,
    /// Тихие часы: уведомления не всплывают, копятся в истории; часы — с `quiet_from` до `quiet_to`.
    pub quiet: bool,
    pub quiet_from: u8,
    pub quiet_to: u8,
    /// Звук «Новое письмо».
    pub sound: bool,
    /// Канал обновлений.
    pub channel: Channel,
    /// Уровень журнала (без `POSTMAN_LOG`; действует со следующего запуска).
    pub log: LogLevel,
}

impl Default for General {
    fn default() -> Self {
        Self {
            theme: None,
            density: None,
            language: None,
            phishing_report: None,
            autostart: false,
            start: StartView::Inbox,
            notify: Notify::All,
            quiet: true,
            quiet_from: 22,
            quiet_to: 8,
            sound: false,
            channel: Channel::Stable,
            log: LogLevel::Info,
        }
    }
}

/// Стартовый экран.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartView {
    #[default]
    Inbox,
    /// Вкладки, открытые при закрытии.
    LastTabs,
}

/// Уведомления о новых письмах.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Notify {
    #[default]
    All,
    /// Упоминания, ответы в моих беседах, письма со сроком.
    Important,
    Off,
}

/// Канал обновлений.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    #[default]
    Stable,
    Preview,
}

/// Уровень журнала.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
}

impl LogLevel {
    pub const ALL: [LogLevel; 4] = [LogLevel::Error, LogLevel::Warn, LogLevel::Info, LogLevel::Debug];

    /// Фильтр `tracing` (`POSTMAN_LOG`).
    pub fn filter(self) -> &'static str {
        match self {
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
        }
    }
}

/// Часы начала тихих часов и их конца (выбор в настройках).
pub const QUIET_FROM: [u8; 4] = [20, 21, 22, 23];
pub const QUIET_TO: [u8; 3] = [7, 8, 9];

/// «Внешний вид» (S5-30).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Look {
    /// Размер шрифта списка, писем и панелей, px (`FONT_SIZES`); `None` — по умолчанию ([`DEFAULT_FONT_SIZE`]).
    pub font_size: Option<u16>,
    /// Аватары и превью текста в строке письма (в компактной плотности — нет всегда).
    pub avatars: bool,
    pub preview: bool,
    /// Плавные переходы (выключены в Windows — выключены и здесь).
    pub animations: bool,
    /// Задержка KeyTips, мс (`KEYTIPS_DELAYS`).
    pub keytips_delay: u16,
    /// Акцентный цвет.
    pub accent: Accent,
}

impl Default for Look {
    fn default() -> Self {
        Self { font_size: None, avatars: true, preview: true, animations: true, keytips_delay: 400, accent: Accent::System }
    }
}

/// Размеры шрифта, px.
pub const FONT_SIZES: std::ops::RangeInclusive<u16> = 12..=18;
/// Размер шрифта по умолчанию, px.
pub const DEFAULT_FONT_SIZE: u16 = 14;
/// Задержки KeyTips, мс.
pub const KEYTIPS_DELAYS: [u16; 3] = [200, 400, 800];

impl Look {
    /// Размер шрифта, px: свой (в пределах) или по умолчанию.
    pub fn font_px(&self) -> u16 {
        self.font_size.filter(|s| FONT_SIZES.contains(s)).unwrap_or(DEFAULT_FONT_SIZE)
    }
}

/// «Безопасность» (S5-30). Внешние картинки не грузятся никогда (политика, не настройка); макросы не открываются
/// никогда (решение №7) — настройка только у сохранения. Выбор — только строже, чем по умолчанию (решение владельца
/// 2026-10-22); политика организации может закрепить любое из значений (`app-gpui/src/security.rs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Security {
    /// Разрешить «Сохранить…» файлов с макросами (с пометкой «из Интернета», решение владельца №7).
    pub macro_save: bool,
    /// Внешние картинки.
    pub images: Images,
    /// Только TLS 1.3 (иначе — 1.2 и выше).
    pub tls13: bool,
    /// Хранить пароль в Диспетчере учётных данных Windows (иначе — спрашивать при каждом запуске).
    pub store_password: bool,
    /// Подписывать все письма S/MIME (решение владельца №7; S5-12 Ф3): «Подписать» в новом письме включено.
    pub sign_all: bool,
}

impl Default for Security {
    fn default() -> Self {
        Self { macro_save: true, images: Images::Never, tls13: false, store_password: true, sign_all: true }
    }
}

/// Внешние картинки в письмах.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Images {
    #[default]
    Never,
    /// Спрашивать в письме.
    Ask,
}

/// Акцентный цвет («Внешний вид», S5-30; токены — DESIGN «Токены цвета»).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Accent {
    /// Синий Postman (#0067C0) — по умолчанию.
    #[default]
    System,
    Teal,
    Green,
    Violet,
    Graphite,
}

impl Accent {
    pub const ALL: [Accent; 5] = [Accent::System, Accent::Teal, Accent::Green, Accent::Violet, Accent::Graphite];

    pub fn id(self) -> &'static str {
        match self {
            Accent::System => "system",
            Accent::Teal => "teal",
            Accent::Green => "green",
            Accent::Violet => "violet",
            Accent::Graphite => "graphite",
        }
    }
}

/// Плотности, которые можно выбрать (высота строки, px).
/// Две, как в макете «Внешний вид» (решение владельца 2026-10-22): компактная и обычная.
pub const DENSITIES: [u16; 2] = [20, 24];
/// Языки интерфейса.
pub const LANGUAGES: [&str; 2] = ["ru", "en"];

impl Prefs {
    /// Прочитать файл `path`. Нет файла — по умолчанию; файл новее этой версии — ошибка (вызывающий берёт
    /// умолчание и не перезаписывает файл). Значения вне допустимых — к ближайшим допустимым ([`Prefs::normalized`]).
    pub fn load(path: &Path) -> Result<Prefs, Error> {
        let Some(text) = read(path)? else { return Ok(Prefs::default()) };
        let version = version_of(&text)?;
        if version > VERSION {
            return Err(Error::Version(version));
        }
        let p: Prefs = serde_json::from_str(&text)?;
        Ok(p.normalized())
    }

    /// Записать через временный файл: сбой посреди записи не портит прежние настройки.
    pub fn save(&self, path: &Path) -> Result<(), Error> {
        write(path, &serde_json::to_string_pretty(self)?)
    }

    /// Привести к допустимым значениям: неизвестные плотность и язык — по умолчанию, быстрые фильтры — все пять по
    /// одному разу, сроки — в пределах.
    pub fn normalized(mut self) -> Prefs {
        self.version = VERSION;
        // Прежние «удобная» 28 и «просторная» 32 — к ближайшей из двух (обычная 24).
        self.general.density = match self.general.density {
            Some(20) => Some(20),
            Some(24 | 28 | 32) => Some(24),
            _ => None,
        };
        if self.general.language.as_deref().is_some_and(|l| !LANGUAGES.contains(&l)) {
            self.general.language = None;
        }
        self.quick = std::mem::take(&mut self.quick).normalized();
        self.lifecycle = self.lifecycle.clamped();
        self.calendar = std::mem::take(&mut self.calendar).normalized();
        self.keys.retain(|id, _| !id.is_empty());
        self.keys_single.retain(|id, _| !id.is_empty());
        // Адрес для фишинга: пробелы по краям — прочь, пустой и без «@» — нет адреса.
        self.general.phishing_report = phishing_address(self.general.phishing_report.as_deref());
        if !QUIET_FROM.contains(&self.general.quiet_from) {
            self.general.quiet_from = General::default().quiet_from;
        }
        if !QUIET_TO.contains(&self.general.quiet_to) {
            self.general.quiet_to = General::default().quiet_to;
        }
        self.look.font_size = self.look.font_size.map(|s| s.clamp(*FONT_SIZES.start(), *FONT_SIZES.end()));
        if !KEYTIPS_DELAYS.contains(&self.look.keytips_delay) {
            self.look.keytips_delay = Look::default().keytips_delay;
        }
        self.viewers.disabled =
            std::mem::take(&mut self.viewers.disabled).into_iter().map(|id| id.trim().to_lowercase()).filter(|id| !id.is_empty()).collect();
        self
    }
}

/// Адрес «Сообщить о фишинге» (S5-08) из введённого текста: обрезанный, с «@» и без пробелов внутри; иначе `None`.
pub fn phishing_address(text: Option<&str>) -> Option<String> {
    let t = text?.trim();
    let ok = t.contains('@') && !t.starts_with('@') && !t.ends_with('@') && !t.chars().any(char::is_whitespace);
    ok.then(|| t.to_owned())
}

// --- быстрые фильтры ---

/// Быстрый фильтр (закладки списка, DESIGN «Быстрые фильтры»).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quick {
    /// Угрозы: подозрительные и фишинг.
    Threats,
    /// Срок: впереди или прошёл.
    Deadline,
    /// Ждём ответа.
    Waiting,
    /// Мне лично.
    Mine,
    /// Истекают: коды и временные письма.
    Expiring,
}

impl Quick {
    /// Порядок по умолчанию — по срочности (DESIGN): Угрозы → Срок → Ждём → Мне → Истекают.
    pub const ALL: [Quick; 5] = [Quick::Threats, Quick::Deadline, Quick::Waiting, Quick::Mine, Quick::Expiring];

    /// Id для ключей текстов (`settings-quick-<id>`).
    pub fn id(self) -> &'static str {
        match self {
            Quick::Threats => "threats",
            Quick::Deadline => "deadline",
            Quick::Waiting => "waiting",
            Quick::Mine => "mine",
            Quick::Expiring => "expiring",
        }
    }
}

/// Быстрый фильтр в списке настроек: показан ли закладкой.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuickItem {
    pub filter: Quick,
    pub shown: bool,
}

/// Порядок и видимость быстрых фильтров.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct QuickFilters {
    pub items: Vec<QuickItem>,
    /// Закладки скрыты целиком («×» у закладок, S5-03); вернуть — Вид → Быстрые фильтры. Выбор `items` остаётся.
    pub hidden: bool,
}

impl Default for QuickFilters {
    fn default() -> Self {
        Self { items: Quick::ALL.iter().map(|&filter| QuickItem { filter, shown: true }).collect(), hidden: false }
    }
}

impl QuickFilters {
    /// Показанные закладками, по порядку (S5-03 строит по ним закладки списка).
    pub fn shown(&self) -> impl Iterator<Item = Quick> + '_ {
        self.items.iter().filter(|i| i.shown).map(|i| i.filter)
    }

    /// Закладки в списке: показанные, если полоса не скрыта целиком.
    pub fn tabs(&self) -> impl Iterator<Item = Quick> + '_ {
        self.shown().filter(|_| !self.hidden)
    }

    /// Каждый фильтр по одному разу; пропавшие (файл старой версии) — в конец, показанными.
    pub fn normalized(mut self) -> Self {
        let mut seen = Vec::new();
        self.items.retain(|i| {
            let fresh = !seen.contains(&i.filter);
            seen.push(i.filter);
            fresh
        });
        for q in Quick::ALL {
            if !seen.contains(&q) {
                self.items.push(QuickItem { filter: q, shown: true });
            }
        }
        self
    }

    /// Показать или скрыть закладку.
    pub fn toggle(&mut self, q: Quick) {
        if let Some(i) = self.items.iter_mut().find(|i| i.filter == q) {
            i.shown = !i.shown;
        }
    }

    /// Сдвинуть на `delta` позиций (−1 — выше); за край — не сдвигается. `true` — сдвинулся.
    pub fn shift(&mut self, q: Quick, delta: isize) -> bool {
        let Some(at) = self.items.iter().position(|i| i.filter == q) else { return false };
        let to = at as isize + delta;
        if to < 0 || to >= self.items.len() as isize {
            return false;
        }
        let item = self.items.remove(at);
        self.items.insert(to as usize, item);
        true
    }
}

// --- сроки и жизненный цикл (S5-04) ---

/// Сроки жизненного цикла (DESIGN «Быстрые фильтры, сроки и жизненный цикл»). Работают, пока Postman запущен
/// (решение владельца №2, `docs/specs/S5.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Lifecycle {
    /// «Ждём ответа»: нет ответа на отправленное — вернуть во «Входящие» через `wait_reply_days`.
    pub wait_reply: bool,
    pub wait_reply_days: u16,
    /// Рассылки — в архив через `archive_news_days`.
    pub archive_news: bool,
    pub archive_news_days: u16,
    /// Коды подтверждения — в «Удалённые» через `expire_codes_hours` («Исчезнет»).
    pub expire_codes: bool,
    pub expire_codes_hours: u16,
    /// Просроченный срок — наверх списка.
    pub overdue_first: bool,
    /// Куда уходят письма, когда наступает «Исчезнет» (свой срок и коды подтверждения).
    pub expire_to: ExpireTo,
}

/// Куда уходит письмо, у которого наступило «Исчезнет».
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpireTo {
    Archive,
    /// В «Удалённые» (по умолчанию: коды подтверждения — в корзину, макет настроек).
    #[default]
    Trash,
}

/// Пределы сроков: дни ожидания и архива, часы кодов.
pub const DAYS: std::ops::RangeInclusive<u16> = 1..=90;
pub const HOURS: std::ops::RangeInclusive<u16> = 1..=168;

impl Default for Lifecycle {
    fn default() -> Self {
        Self {
            wait_reply: true,
            wait_reply_days: 3,
            archive_news: false,
            archive_news_days: 7,
            expire_codes: true,
            expire_codes_hours: 24,
            overdue_first: true,
            expire_to: ExpireTo::Trash,
        }
    }
}

const DAY: u64 = 24 * 60 * 60;

impl Lifecycle {
    /// Сроки — в пределах [`DAYS`] и [`HOURS`].
    pub fn clamped(mut self) -> Self {
        let days = |d: u16| d.clamp(*DAYS.start(), *DAYS.end());
        self.wait_reply_days = days(self.wait_reply_days);
        self.archive_news_days = days(self.archive_news_days);
        self.expire_codes_hours = self.expire_codes_hours.clamp(*HOURS.start(), *HOURS.end());
        self
    }

    /// Через сколько вернуть письмо без ответа; `None` — правило выключено.
    pub fn wait_reply(&self) -> Option<Duration> {
        self.wait_reply.then(|| Duration::from_secs(u64::from(self.wait_reply_days) * DAY))
    }

    /// Через сколько рассылку — в архив; `None` — выключено.
    pub fn archive_newsletters(&self) -> Option<Duration> {
        self.archive_news.then(|| Duration::from_secs(u64::from(self.archive_news_days) * DAY))
    }

    /// Через сколько код подтверждения — в «Удалённые»; `None` — выключено.
    pub fn expire_codes(&self) -> Option<Duration> {
        self.expire_codes.then(|| Duration::from_secs(u64::from(self.expire_codes_hours) * 60 * 60))
    }
}

// --- просмотр вложений (S8-26) ---

/// «Просмотр вложений» (S8-26): модули просмотра WASM; встроенные форматы не выключаются.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Viewers {
    /// Выключенные модули (`postman_core::viewers::Module::id` — имя файла строчными): выбор просмотра их не видит.
    pub disabled: BTreeSet<String>,
}

// --- главное окно (T1-05а) ---

/// Ширины панелей главного окна: папки и список, px при масштабе 100 %; чтению — остаток. `None` — ширина макета
/// ([`Layout::FOLDERS`], [`Layout::LIST`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    pub folders: Option<u16>,
    pub list: Option<u16>,
}

impl Layout {
    /// Ширины макета.
    pub const FOLDERS: u16 = 236;
    pub const LIST: u16 = 452;
    /// Минимумы: уже панель не сжимается ни разделителем, ни окном.
    pub const MIN_FOLDERS: u16 = 160;
    pub const MIN_LIST: u16 = 280;
    pub const MIN_READING: u16 = 360;

    /// Ширины в окне шириной `width` (px при 100 %, место трёх панелей): не меньше минимумов и не больше того, что
    /// оставляет чтению его минимум. Папкам — первыми (список уступает), `None` остаётся `None`: ширину макета
    /// сжимает [`Layout::widths`]. Окно уже суммы минимумов — минимумы.
    pub fn clamp(self, width: u16) -> Layout {
        let folders_max = width.saturating_sub(Self::MIN_LIST + Self::MIN_READING).max(Self::MIN_FOLDERS);
        let folders = self.folders.map(|f| f.clamp(Self::MIN_FOLDERS, folders_max));
        let taken = folders.unwrap_or(Self::FOLDERS).min(folders_max);
        let list_max = width.saturating_sub(taken + Self::MIN_READING).max(Self::MIN_LIST);
        Layout { folders, list: self.list.map(|l| l.clamp(Self::MIN_LIST, list_max)) }
    }

    /// Ширины папок и списка в окне `width`: свои или макета, в пределах [`Layout::clamp`].
    pub fn widths(self, width: u16) -> (u16, u16) {
        let full = Layout { folders: Some(self.folders.unwrap_or(Self::FOLDERS)), list: Some(self.list.unwrap_or(Self::LIST)) }.clamp(width);
        (full.folders.unwrap_or(Self::FOLDERS), full.list.unwrap_or(Self::LIST))
    }
}

// --- чтение ---

/// Вид беседы (DESIGN «Беседы»).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Conversation {
    /// Лента без цитат в панели чтения (по умолчанию).
    #[default]
    Feed,
    /// Стопка в панели чтения: старые письма свёрнуты.
    Stack,
    /// В списке, как Outlook.
    Outlook,
}

impl Conversation {
    pub const ALL: [Conversation; 3] = [Conversation::Feed, Conversation::Stack, Conversation::Outlook];
}

/// Где панель чтения.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pane {
    #[default]
    Right,
    Bottom,
    /// Скрыта: письмо открывается вместо списка, Esc — назад.
    Hidden,
}

impl Pane {
    pub const ALL: [Pane; 3] = [Pane::Right, Pane::Bottom, Pane::Hidden];
}

/// Порядок писем в беседе (T1-16а): в ленте и стопке и под головой беседы в списке. Голова в списке — всегда самое
/// новое письмо.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadOrder {
    /// Сначала старые (по умолчанию).
    #[default]
    OldestFirst,
    /// Сначала новые.
    NewestFirst,
}

impl ThreadOrder {
    pub const ALL: [ThreadOrder; 2] = [ThreadOrder::OldestFirst, ThreadOrder::NewestFirst];
}

/// Чтение. Внешние картинки не грузятся никогда — это политика Postman, не настройка (`docs/HANDOFF.md` §3a).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Reading {
    pub conversation: Conversation,
    /// Вид беседы для папки (id папки → вид), поверх общего.
    pub folders: BTreeMap<String, Conversation>,
    pub pane: Pane,
    /// Предупреждать, если текст ссылки — один адрес, а ведёт она на другой.
    pub warn_link_mismatch: bool,
    /// Приглашение в письме (S5-24): подробности, кнопки столбиком и лента дня — колонкой 300 px справа, а не
    /// карточкой над текстом.
    pub invite_side: bool,
    /// Порядок писем в беседе (T1-16а).
    pub thread_order: ThreadOrder,
}

impl Default for Reading {
    fn default() -> Self {
        Self {
            conversation: Conversation::Feed,
            folders: BTreeMap::new(),
            pane: Pane::Right,
            warn_link_mismatch: true,
            invite_side: false,
            thread_order: ThreadOrder::OldestFirst,
        }
    }
}

impl Reading {
    /// Вид беседы в папке: свой у папки или общий.
    pub fn conversation_in(&self, mailbox: Option<&str>) -> Conversation {
        mailbox.and_then(|m| self.folders.get(m)).copied().unwrap_or(self.conversation)
    }

    /// Свой вид для папки; `None` — как везде.
    pub fn set_folder(&mut self, mailbox: &str, view: Option<Conversation>) {
        match view {
            Some(v) => {
                self.folders.insert(mailbox.to_owned(), v);
            }
            None => {
                self.folders.remove(mailbox);
            }
        }
    }
}

// --- календарь (S5-20) ---

/// С какого дня начинается неделя (неделя, месяц и мини-календарь — S5-21).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WeekStart {
    #[default]
    Monday,
    Sunday,
}

impl WeekStart {
    pub const ALL: [WeekStart; 2] = [WeekStart::Monday, WeekStart::Sunday];
}

/// Вид календаря при переходе в раздел (виды — S5-21).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalendarView {
    Day,
    #[default]
    WorkWeek,
    Week,
    Month,
}

impl CalendarView {
    pub const ALL: [CalendarView; 4] = [CalendarView::Day, CalendarView::WorkWeek, CalendarView::Week, CalendarView::Month];

    pub fn id(self) -> &'static str {
        match self {
            CalendarView::Day => "day",
            CalendarView::WorkWeek => "work-week",
            CalendarView::Week => "week",
            CalendarView::Month => "month",
        }
    }
}

/// Цвет свободного времени: общие свободные окна во встрече (DESIGN «Настройки → Календарь»). Только цвета бренда
/// (решение владельца 2026-10-21): розовый, синий и серо-голубой; зелёный, синий Fluent и бирюзовый отменены —
/// в файле прежних версий `green` и `teal` читаются розовым, `blue` — синим бренда. Цвета ролей —
/// `app.fb.free.<id>.*` в интерфейсе.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FreeColor {
    /// Розовый бренда #E4107F.
    #[default]
    #[serde(alias = "green", alias = "teal")]
    Brand,
    /// Синий бренда #3A9BD5.
    Blue,
    /// Серо-голубой бренда #7A8FA6.
    Slate,
}

impl FreeColor {
    pub const ALL: [FreeColor; 3] = [FreeColor::Brand, FreeColor::Blue, FreeColor::Slate];

    pub fn id(self) -> &'static str {
        match self {
            FreeColor::Brand => "brand",
            FreeColor::Blue => "blue",
            FreeColor::Slate => "slate",
        }
    }
}

/// Шаг сетки, мин.
pub const STEPS: [u16; 2] = [30, 15];
/// Длительность новой встречи, мин (выбор встречи — 30 мин … 2 ч).
pub const DURATIONS: [u16; 4] = [30, 60, 90, 120];
/// Напоминание, мин до начала; 0 — без напоминания.
pub const REMINDERS: [u16; 6] = [0, 5, 10, 15, 30, 60];
/// Минут в сутках: конец рабочего дня — не позже.
const DAY_MIN: u16 = 24 * 60;

/// Часовые пояса выбора (IANA): Россия с запада на восток, затем Лондон и UTC. Другой пояс в файле — тоже
/// действует, если jiff его знает.
pub const ZONES: [&str; 14] = [
    "Europe/Kaliningrad",
    "Europe/Moscow",
    "Europe/Samara",
    "Asia/Yekaterinburg",
    "Asia/Omsk",
    "Asia/Novosibirsk",
    "Asia/Krasnoyarsk",
    "Asia/Irkutsk",
    "Asia/Yakutsk",
    "Asia/Vladivostok",
    "Asia/Magadan",
    "Asia/Kamchatka",
    "Europe/London",
    "UTC",
];

/// «Настройки → Календарь» (DESIGN, макет «Настройки → Календарь»). Действуют сразу: рабочие дни и часы, пояс,
/// длительность новой встречи, «Учитывать необязательных», цвет свободного времени, удаление письма-приглашения
/// после ответа. Хранятся до своих задач: вид и первый день недели, шаг, второй пояс (S5-21), напоминание,
/// «Начинать на 5 мин позже» и «под вопросом» для новых приглашений, цвета и имена коллег (S5-22).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Calendar {
    /// Рабочие дни, Пн … Вс.
    pub work_days: [bool; 7],
    /// Рабочие часы: начало и конец, минуты от полуночи.
    pub work_start: u16,
    pub work_end: u16,
    pub week_start: WeekStart,
    pub view: CalendarView,
    /// Шаг сетки, мин ([`STEPS`]).
    pub step: u16,
    /// Свой часовой пояс (IANA); `None` — пояс Windows.
    pub zone: Option<String>,
    /// Вторая шкала часов слева от сетки.
    pub second_zone_on: bool,
    pub second_zone: String,
    /// Длительность новой встречи, мин ([`DURATIONS`]).
    pub duration: u16,
    /// Напоминание, мин ([`REMINDERS`]; 0 — нет).
    pub reminder: u16,
    /// Начинать встречи на 5 мин позже (перерыв между встречами подряд).
    pub start_late: bool,
    /// Необязательные участники закрывают общие окна.
    pub with_optional: bool,
    pub free_color: FreeColor,
    /// События коллег — цветом календаря владельца; имя владельца в событии (S5-22).
    pub owner_colors: bool,
    pub owner_names: bool,
    /// Цвет календаря владельца (S5-22): ключ — адрес владельца в нижнем регистре, `""` — мой; значение — цвет
    /// `palette::CALENDAR`. Назначенные цвета тоже хранятся — не меняются между запусками.
    pub calendar_colors: std::collections::BTreeMap<String, String>,
    /// Скрытые календари коллег (ключи владельцев).
    pub hidden_calendars: Vec<String>,
    /// Новые приглашения — в календарь «под вопросом».
    pub tentative_invites: bool,
    /// Письмо-приглашение — в «Удалённые» после ответа.
    pub delete_answered: bool,
}

impl Default for Calendar {
    fn default() -> Self {
        Self {
            work_days: [true, true, true, true, true, false, false],
            work_start: 9 * 60,
            work_end: 18 * 60,
            week_start: WeekStart::Monday,
            view: CalendarView::WorkWeek,
            step: 30,
            zone: None,
            second_zone_on: false,
            second_zone: "Asia/Yekaterinburg".into(),
            duration: 30,
            reminder: 15,
            start_late: true,
            with_optional: false,
            free_color: FreeColor::Brand,
            owner_colors: true,
            owner_names: true,
            calendar_colors: std::collections::BTreeMap::new(),
            hidden_calendars: Vec::new(),
            tentative_invites: true,
            delete_answered: true,
        }
    }
}

/// Пояс по имени IANA: известен ли jiff (база поясов в exe).
fn known_zone(name: &str) -> bool {
    jiff::tz::TimeZone::get(name).is_ok()
}

impl Calendar {
    /// К допустимым значениям: без рабочих дней — Пн–Пт, часы — начало раньше конца в пределах суток, шаг,
    /// длительность и напоминание — из списков, неизвестный пояс — пояс Windows.
    pub fn normalized(mut self) -> Self {
        let d = Calendar::default();
        if !self.work_days.contains(&true) {
            self.work_days = d.work_days;
        }
        if self.work_end > DAY_MIN || self.work_start >= self.work_end {
            (self.work_start, self.work_end) = (d.work_start, d.work_end);
        }
        if !STEPS.contains(&self.step) {
            self.step = d.step;
        }
        if !DURATIONS.contains(&self.duration) {
            self.duration = d.duration;
        }
        if !REMINDERS.contains(&self.reminder) {
            self.reminder = d.reminder;
        }
        self.zone = self.zone.take().map(|z| z.trim().to_owned()).filter(|z| known_zone(z));
        if !known_zone(&self.second_zone) {
            self.second_zone = d.second_zone;
        }
        self
    }

    /// Часовой пояс календаря и встреч: свой или Windows.
    pub fn zone(&self) -> jiff::tz::TimeZone {
        self.zone.as_deref().and_then(|z| jiff::tz::TimeZone::get(z).ok()).unwrap_or_else(jiff::tz::TimeZone::system)
    }

    /// Рабочее время для сетки и подбора окон (вместо `WorkHours::standard`).
    pub fn work_hours(&self, tz: jiff::tz::TimeZone) -> postman_calendar::WorkHours {
        use jiff::civil::{Time, Weekday};
        let at = |m: u16| {
            if m >= DAY_MIN { Time::MAX } else { Time::new((m / 60) as i8, (m % 60) as i8, 0, 0).unwrap_or(Time::midnight()) }
        };
        let days = (0..7).filter(|&i| self.work_days[i as usize]).filter_map(|i| Weekday::from_monday_zero_offset(i).ok()).collect();
        postman_calendar::WorkHours { tz, start: at(self.work_start), end: at(self.work_end), days }
    }

    /// Длительность новой встречи, с.
    pub fn duration_secs(&self) -> i64 {
        i64::from(self.duration) * 60
    }
}

// --- у учётной записи: подписи и шаблоны ---

/// Подпись. У адреса (Identity) может быть несколько.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature {
    /// Постоянный id (`s<время в мс>`), на него ссылаются умолчания.
    pub id: String,
    pub name: String,
    /// Адрес отправителя, к которому подпись относится; `None` — к любому.
    #[serde(default)]
    pub identity: Option<String>,
    pub text: String,
}

/// Шаблон письма: вставляется в новое письмо.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Template {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub body: String,
}

/// Какую подпись ставить адресу: в новое письмо и в ответ или пересылку (id подписи; `None` — без подписи).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Defaults {
    pub new: Option<String>,
    pub reply: Option<String>,
}

/// Настройки учётной записи.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountPrefs {
    pub version: u32,
    pub signatures: Vec<Signature>,
    pub templates: Vec<Template>,
    /// Адрес отправителя → подписи по умолчанию.
    pub defaults: BTreeMap<String, Defaults>,
}

impl Default for AccountPrefs {
    fn default() -> Self {
        Self { version: VERSION, signatures: Vec::new(), templates: Vec::new(), defaults: BTreeMap::new() }
    }
}

impl AccountPrefs {
    /// Файл настроек для кэша учётной записи `cache`: `<кэш>.settings.json`.
    pub fn beside(cache: &Path) -> PathBuf {
        let mut name = cache.file_name().unwrap_or_default().to_os_string();
        name.push(".settings.json");
        cache.with_file_name(name)
    }

    pub fn load(path: &Path) -> Result<AccountPrefs, Error> {
        let Some(text) = read(path)? else { return Ok(AccountPrefs::default()) };
        let version = version_of(&text)?;
        if version > VERSION {
            return Err(Error::Version(version));
        }
        Ok(serde_json::from_str::<AccountPrefs>(&text)?.normalized())
    }

    pub fn save(&self, path: &Path) -> Result<(), Error> {
        write(path, &serde_json::to_string_pretty(self)?)
    }

    /// Умолчания ссылаются только на существующие подписи этого адреса (или любого).
    pub fn normalized(mut self) -> Self {
        self.version = VERSION;
        let sigs = self.signatures.clone();
        for (identity, d) in self.defaults.iter_mut() {
            let fits = |id: &Option<String>| {
                id.as_ref().is_none_or(|id| sigs.iter().any(|s| &s.id == id && s.identity.as_ref().is_none_or(|i| i == identity)))
            };
            if !fits(&d.new) {
                d.new = None;
            }
            if !fits(&d.reply) {
                d.reply = None;
            }
        }
        self.defaults.retain(|_, d| d.new.is_some() || d.reply.is_some());
        self
    }

    /// Подписи, которые можно поставить адресу `identity`: его и общие.
    pub fn signatures_for<'a>(&'a self, identity: &'a str) -> impl Iterator<Item = &'a Signature> + 'a {
        self.signatures.iter().filter(move |s| s.identity.as_deref().is_none_or(|i| i == identity))
    }

    /// Подпись письма от `identity`: в ответ (`reply`) или в новое (S5-06 вставляет её в композер).
    pub fn signature(&self, identity: &str, reply: bool) -> Option<&Signature> {
        let d = self.defaults.get(identity)?;
        let id = if reply { d.reply.as_ref() } else { d.new.as_ref() }?;
        self.signatures.iter().find(|s| &s.id == id)
    }

    /// Новый id для подписи или шаблона: `<буква><время в мс>`, свободный.
    pub fn new_id(&self, prefix: char, now_ms: i64) -> String {
        let taken = |id: &str| self.signatures.iter().any(|s| s.id == id) || self.templates.iter().any(|t| t.id == id);
        (now_ms.max(0)..).map(|n| format!("{prefix}{n}")).find(|id| !taken(id)).unwrap_or_default()
    }

    /// Удалить подпись и ссылки на неё в умолчаниях.
    pub fn remove_signature(&mut self, id: &str) {
        self.signatures.retain(|s| s.id != id);
        *self = std::mem::take(self).normalized();
    }
}

// --- файлы ---

fn read(path: &Path) -> Result<Option<String>, Error> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn version_of(text: &str) -> Result<u32, Error> {
    #[derive(Deserialize)]
    struct V {
        #[serde(default)]
        version: u32,
    }
    Ok(serde_json::from_str::<V>(text)?.version)
}

fn write(path: &Path, text: &str) -> Result<(), Error> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Запись в фоне по порядку: каждая правка получает номер ([`Writer::next`]), запись с номером меньше уже
/// записанного пропускается — поздний поток не затрёт новые настройки старыми.
#[derive(Clone, Default)]
pub struct Writer {
    written: Arc<Mutex<u64>>,
    issued: Arc<Mutex<u64>>,
}

impl Writer {
    /// Номер следующей записи (звать в потоке интерфейса, по порядку правок).
    pub fn next(&self) -> u64 {
        let mut n = self.issued.lock().unwrap_or_else(PoisonError::into_inner);
        *n += 1;
        *n
    }

    /// Записать (в фоновом потоке): `save` выполняется, только если номер `seq` новее записанного. `Ok(false)` —
    /// пропущено.
    pub fn write(&self, seq: u64, save: impl FnOnce() -> Result<(), Error>) -> Result<bool, Error> {
        let mut written = self.written.lock().unwrap_or_else(PoisonError::into_inner);
        if seq <= *written {
            return Ok(false);
        }
        save()?;
        *written = seq;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> tempfile::TempDir {
        tempfile::Builder::new().prefix("postman-").tempdir().unwrap()
    }

    /// Настройки туда и обратно: всё, что поменяли, читается тем же.
    #[test]
    fn prefs_round_trip() {
        let d = dir();
        let path = d.path().join(FILE);
        assert_eq!(Prefs::load(&path).unwrap(), Prefs::default(), "нет файла — по умолчанию");
        let mut p = Prefs {
            general: General {
                theme: Some("fluent-dark".into()),
                density: Some(20),
                language: Some("en".into()),
                phishing_report: Some("soc@corp.example".into()),
                ..General::default()
            },
            ..Prefs::default()
        };
        p.quick.toggle(Quick::Expiring);
        assert!(p.quick.shift(Quick::Mine, -3));
        p.lifecycle.archive_news = true;
        p.lifecycle.archive_news_days = 14;
        p.reading.pane = Pane::Bottom;
        p.reading.set_folder("news", Some(Conversation::Outlook));
        p.reading.warn_link_mismatch = false;
        p.reading.invite_side = true;
        p.reading.thread_order = ThreadOrder::NewestFirst;
        p.keys.insert("mail.reply".into(), vec!["ctrl-shift-y".into()]);
        p.keys.insert("tab.close".into(), vec![]);
        p.key_scheme = KeyScheme::Single;
        p.keytips = false;
        p.general.autostart = true;
        p.general.notify = Notify::Important;
        p.general.quiet_from = 21;
        p.general.log = LogLevel::Debug;
        p.look.font_size = Some(16);
        p.look.avatars = false;
        p.look.keytips_delay = 800;
        p.security.macro_save = false;
        p.security.images = Images::Ask;
        p.security.tls13 = true;
        p.security.store_password = false;
        p.look.accent = Accent::Teal;
        p.keys_single.insert("mail.archive".into(), vec!["y".into()]);
        p.save(&path).unwrap();
        let back = Prefs::load(&path).unwrap();
        assert_eq!(back, p);
        assert_eq!(back.quick.shown().collect::<Vec<_>>(), [Quick::Mine, Quick::Threats, Quick::Deadline, Quick::Waiting]);
        assert_eq!(back.reading.conversation_in(Some("news")), Conversation::Outlook);
        assert_eq!(back.reading.conversation_in(Some("inbox")), Conversation::Feed);
        assert_eq!(back.lifecycle.archive_newsletters(), Some(Duration::from_secs(14 * DAY)));
        // Временного файла не остаётся.
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
    }

    /// S5-30: значения вне выбора — к умолчанию; размер шрифта — в пределах 12…18, без своего — 13.
    #[test]
    fn appearance_and_general_are_clamped() {
        let mut p = Prefs::default();
        assert_eq!(p.look.font_px(), DEFAULT_FONT_SIZE);
        assert!(p.security.macro_save, "решение №7: «Сохранить…» можно");
        p.look.font_size = Some(40);
        p.look.keytips_delay = 123;
        p.general.quiet_from = 3;
        p.general.quiet_to = 12;
        let p = p.normalized();
        assert_eq!(p.look.font_size, Some(18));
        assert_eq!(Prefs::default().look.font_px(), 14, "по умолчанию — 14, как в макете");
        assert_eq!(p.look.font_px(), 18);
        assert_eq!(p.look.keytips_delay, 400);
        assert_eq!((p.general.quiet_from, p.general.quiet_to), (22, 8));
        // Плотность: прежние 28 и 32 — обычная 24, 20 — компактная.
        for (was, now) in [(20, 20), (24, 24), (28, 24), (32, 24)] {
            let mut p = Prefs::default();
            p.general.density = Some(was);
            assert_eq!(p.normalized().general.density, Some(now), "{was}");
        }
        let old: Prefs = serde_json::from_str(r#"{"version":1,"general":{"language":"ru"}}"#).unwrap();
        assert_eq!(old.security, Security::default(), "по умолчанию — не слабее прежнего");
        assert_eq!((old.security.images, old.security.tls13, old.security.store_password), (Images::Never, false, true));
        assert_eq!(old.general.language.as_deref(), Some("ru"));
        assert!(old.general.quiet && old.look.avatars && old.keytips, "прежний файл — умолчания новых полей");
        assert_eq!(LogLevel::Debug.filter(), "debug");
    }

    /// Ширины панелей (T1-05а): старый файл — макет; на узком окне — минимумы, на широком — свои; круг записи.
    #[test]
    fn layout_widths_are_clamped_and_kept() {
        let old: Prefs = serde_json::from_str(r#"{"version":1,"reading":{"pane":"bottom"}}"#).unwrap();
        assert_eq!(old.layout, Layout::default(), "старый файл — ширины макета");
        assert_eq!((old.reading.pane, old.reading.thread_order), (Pane::Bottom, ThreadOrder::OldestFirst), "беседа — как раньше");
        assert_eq!(old.layout.widths(1600), (Layout::FOLDERS, Layout::LIST));
        let wide = Layout { folders: Some(300), list: Some(600) };
        assert_eq!(wide.clamp(1600), wide, "на широком окне — свои");
        // 1000 px: папкам — до 1000 − 280 − 360 = 360, списку — что оставит чтению 360.
        assert_eq!(wide.clamp(1000), Layout { folders: Some(300), list: Some(340) });
        assert_eq!(Layout { folders: Some(900), list: Some(900) }.clamp(1000), Layout { folders: Some(360), list: Some(280) });
        // Меньше минимума — минимум; окно уже суммы минимумов — минимумы.
        assert_eq!(Layout { folders: Some(10), list: Some(10) }.clamp(1600), Layout { folders: Some(160), list: Some(280) });
        assert_eq!(wide.clamp(600), Layout { folders: Some(160), list: Some(280) });
        assert_eq!(Layout::default().widths(600), (160, 280));
        // Без своей ширины папок список считается от ширины макета (236).
        assert_eq!(Layout { folders: None, list: Some(900) }.clamp(1000), Layout { folders: None, list: Some(404) });
        let d = dir();
        let path = d.path().join(FILE);
        let p = Prefs { layout: Layout { folders: Some(200), list: None }, ..Prefs::default() };
        p.save(&path).unwrap();
        assert_eq!(Prefs::load(&path).unwrap().layout, p.layout);
    }

    #[test]
    fn disabled_viewers_round_trip() {
        let old: Prefs = serde_json::from_str(r#"{"version":1}"#).unwrap();
        assert!(old.viewers.disabled.is_empty(), "старый файл — все модули включены");
        let d = dir();
        let path = d.path().join(FILE);
        let mut p = Prefs::default();
        p.viewers.disabled = BTreeSet::from(["FB2 ".into(), "".into(), "djvu".into()]);
        p.save(&path).unwrap();
        assert_eq!(Prefs::load(&path).unwrap().viewers.disabled, BTreeSet::from(["fb2".into(), "djvu".into()]));
    }

    /// Адрес «Сообщить о фишинге» (S5-08): обрезается; не адрес — нет пересылки.
    #[test]
    fn phishing_address_is_checked() {
        assert_eq!(phishing_address(Some("  soc@corp.example ")), Some("soc@corp.example".into()));
        for bad in ["", "   ", "soc", "@corp", "soc@", "so c@corp.example"] {
            assert_eq!(phishing_address(Some(bad)), None, "{bad:?}");
        }
        assert_eq!(phishing_address(None), None);
        let p = Prefs { general: General { phishing_report: Some("nope".into()), ..General::default() }, ..Prefs::default() };
        assert_eq!(p.normalized().general.phishing_report, None);
    }

    /// Вид беседы (S5-05б): у папки свой — он, иначе общий; поиск (нет папки) — общий; «По умолчанию» снимает свой.
    #[test]
    fn conversation_view_resolves_per_folder() {
        let mut r = Reading { conversation: Conversation::Stack, ..Reading::default() };
        assert_eq!(r.conversation_in(Some("inbox")), Conversation::Stack);
        assert_eq!(r.conversation_in(None), Conversation::Stack);
        r.set_folder("inbox", Some(Conversation::Outlook));
        r.set_folder("sent", Some(Conversation::Feed));
        assert_eq!(r.conversation_in(Some("inbox")), Conversation::Outlook);
        assert_eq!(r.conversation_in(Some("sent")), Conversation::Feed);
        assert_eq!(r.conversation_in(Some("archive")), Conversation::Stack);
        assert_eq!(r.conversation_in(None), Conversation::Stack);
        // Общий сменился — папка со своим видом его не замечает, без своего — следует за ним.
        r.conversation = Conversation::Feed;
        assert_eq!((r.conversation_in(Some("inbox")), r.conversation_in(Some("archive"))), (Conversation::Outlook, Conversation::Feed));
        r.set_folder("inbox", None);
        assert_eq!(r.conversation_in(Some("inbox")), Conversation::Feed);
        assert!(!r.folders.contains_key("inbox"));
    }

    /// Чужие и испорченные значения — к допустимым; файл новее версии — ошибка, а не тихое затирание.
    #[test]
    fn bad_values_are_normalized_and_newer_file_is_refused() {
        let d = dir();
        let path = d.path().join(FILE);
        let text = r#"{"version":1,"general":{"density":23,"language":"de"},
            "quick":{"items":[{"filter":"mine","shown":false},{"filter":"mine","shown":true}]},
            "lifecycle":{"wait_reply_days":0,"expire_codes_hours":1000},"future_field":1}"#;
        std::fs::write(&path, text).unwrap();
        let p = Prefs::load(&path).unwrap();
        assert_eq!((p.general.density, p.general.language.as_deref()), (None, None));
        assert_eq!(p.quick.items.len(), 5);
        assert_eq!(p.quick.items[0], QuickItem { filter: Quick::Mine, shown: false });
        assert_eq!((p.lifecycle.wait_reply_days, p.lifecycle.expire_codes_hours), (1, 168));
        std::fs::write(&path, r#"{"version":99}"#).unwrap();
        assert!(matches!(Prefs::load(&path), Err(Error::Version(99))));
        std::fs::write(&path, "{ not json").unwrap();
        assert!(Prefs::load(&path).is_err());
    }

    /// «Настройки → Календарь» (S5-20): по умолчанию — по макету, правка читается той же, неверное — к допустимому.
    #[test]
    fn calendar_prefs_default_round_trip_and_bad_values() {
        let c = Calendar::default();
        assert_eq!(c.free_color, FreeColor::Brand, "розовый бренда по умолчанию");
        assert_eq!((c.work_start, c.work_end, c.duration, c.step, c.reminder), (540, 1080, 30, 30, 15));
        assert_eq!(c.work_days, [true, true, true, true, true, false, false]);
        assert!(c.start_late && !c.with_optional && c.tentative_invites && c.delete_answered && c.zone.is_none());
        assert_eq!(c.view, CalendarView::WorkWeek);
        let h = c.work_hours(jiff::tz::TimeZone::UTC);
        assert_eq!((h.start, h.end), (jiff::civil::time(9, 0, 0, 0), jiff::civil::time(18, 0, 0, 0)));
        assert_eq!(h.days.len(), 5);
        assert_eq!(c.duration_secs(), 1800);

        let d = dir();
        let path = d.path().join(FILE);
        let p = Prefs {
            calendar: Calendar {
                work_days: [true, true, true, true, false, false, true],
                work_start: 8 * 60,
                work_end: 17 * 60,
                week_start: WeekStart::Sunday,
                view: CalendarView::Month,
                step: 15,
                zone: Some("Asia/Novosibirsk".into()),
                second_zone_on: true,
                second_zone: "Europe/London".into(),
                duration: 60,
                reminder: 0,
                start_late: false,
                with_optional: true,
                free_color: FreeColor::Slate,
                owner_colors: false,
                owner_names: false,
                calendar_colors: std::collections::BTreeMap::from([("a@x".to_string(), "pink".to_string())]),
                hidden_calendars: vec!["b@x".into()],
                tentative_invites: false,
                delete_answered: false,
            },
            ..Prefs::default()
        };
        p.save(&path).unwrap();
        let back = Prefs::load(&path).unwrap();
        assert_eq!(back, p);
        assert_eq!(back.calendar.zone().iana_name(), Some("Asia/Novosibirsk"));
        let h = back.calendar.work_hours(back.calendar.zone());
        assert_eq!(
            h.days,
            [
                jiff::civil::Weekday::Monday,
                jiff::civil::Weekday::Tuesday,
                jiff::civil::Weekday::Wednesday,
                jiff::civil::Weekday::Thursday,
                jiff::civil::Weekday::Sunday
            ]
        );

        // Старый файл без раздела — по умолчанию; неверное — к допустимому, остальное не теряется.
        std::fs::write(&path, r#"{"version":1,"reading":{"pane":"bottom"}}"#).unwrap();
        let old = Prefs::load(&path).unwrap();
        assert_eq!((old.calendar, old.reading.pane), (Calendar::default(), Pane::Bottom));
        let text = r#"{"version":1,"calendar":{"work_days":[false,false,false,false,false,false,false],
            "work_start":1200,"work_end":600,"step":20,"duration":45,"reminder":7,"zone":"Mars/Olympus",
            "second_zone":"","free_color":"green","with_optional":true}}"#;
        std::fs::write(&path, text).unwrap();
        let c = Prefs::load(&path).unwrap().calendar;
        let def = Calendar::default();
        assert_eq!(c.work_days, def.work_days, "без рабочих дней — Пн–Пт");
        assert_eq!((c.work_start, c.work_end), (540, 1080), "начало после конца — по умолчанию");
        assert_eq!((c.step, c.duration, c.reminder), (30, 30, 15));
        assert_eq!(c.zone, None, "неизвестный пояс — пояс Windows");
        assert_eq!(c.second_zone, def.second_zone);
        assert_eq!((c.free_color, c.with_optional), (FreeColor::Brand, true), "отменённый зелёный — розовым");
        // Отменённые цвета прежних версий: бирюзовый — розовым, синий — синим бренда.
        for (old, now) in [("teal", FreeColor::Brand), ("blue", FreeColor::Blue), ("slate", FreeColor::Slate)] {
            let text = format!(r#"{{"version":1,"calendar":{{"free_color":"{old}"}}}}"#);
            std::fs::write(&path, text).unwrap();
            assert_eq!(Prefs::load(&path).unwrap().calendar.free_color, now, "{old}");
        }
        // Конец суток — допустим, позже — нет.
        let edge = Calendar { work_start: 22 * 60, work_end: 24 * 60, ..Calendar::default() }.normalized();
        assert_eq!((edge.work_start, edge.work_end), (1320, 1440));
        assert_eq!(edge.work_hours(jiff::tz::TimeZone::UTC).end, jiff::civil::Time::MAX);
        let over = Calendar { work_end: 24 * 60 + 1, ..Calendar::default() }.normalized();
        assert_eq!(over.work_end, 1080);
    }

    #[test]
    fn quick_filters_shift_within_bounds() {
        let mut q = QuickFilters::default();
        assert!(!q.shift(Quick::Threats, -1), "первый выше не сдвигается");
        assert!(!q.shift(Quick::Expiring, 1));
        assert!(q.shift(Quick::Threats, 1));
        assert_eq!(q.items[1].filter, Quick::Threats);
        q.toggle(Quick::Threats);
        assert!(!q.shown().any(|f| f == Quick::Threats));
    }

    #[test]
    fn lifecycle_api_for_s5_04() {
        let l = Lifecycle::default();
        assert_eq!(l.wait_reply(), Some(Duration::from_secs(3 * DAY)));
        assert_eq!(l.archive_newsletters(), None, "рассылки в архив — выключено по умолчанию");
        assert_eq!(l.expire_codes(), Some(Duration::from_secs(24 * 60 * 60)));
    }

    /// Подписи и шаблоны учётной записи — в файле рядом с её кэшем; умолчания без подписи отпадают.
    #[test]
    fn account_prefs_round_trip_and_defaults() {
        let d = dir();
        let path = AccountPrefs::beside(&d.path().join("account-u-1234.redb"));
        assert!(path.to_string_lossy().ends_with("account-u-1234.redb.settings.json"));
        let mut a = AccountPrefs::default();
        let main = a.new_id('s', 10);
        a.signatures.push(Signature { id: main.clone(), name: "Main".into(), identity: None, text: "--\nMe".into() });
        let short = a.new_id('s', 10);
        assert_ne!(main, short);
        a.signatures.push(Signature { id: short.clone(), name: "Short".into(), identity: Some("me@corp.example".into()), text: "Me".into() });
        a.templates.push(Template { id: a.new_id('t', 10), name: "Offer".into(), subject: "S".into(), body: "B".into() });
        a.defaults.insert("me@corp.example".into(), Defaults { new: Some(main.clone()), reply: Some(short.clone()) });
        // Подпись другого адреса умолчанием не станет.
        a.defaults.insert("other@corp.example".into(), Defaults { new: Some(short.clone()), reply: None });
        a = a.normalized();
        assert!(!a.defaults.contains_key("other@corp.example"));
        a.save(&path).unwrap();
        let back = AccountPrefs::load(&path).unwrap();
        assert_eq!(back, a);
        assert_eq!(back.signature("me@corp.example", true).map(|s| s.name.as_str()), Some("Short"));
        assert_eq!(back.signatures_for("other@corp.example").count(), 1);
        let mut a = back;
        a.remove_signature(&short);
        assert_eq!(a.signature("me@corp.example", true), None);
        assert_eq!(a.signature("me@corp.example", false).map(|s| s.name.as_str()), Some("Main"));
    }

    /// Поздняя запись не затирает более новую.
    #[test]
    fn writer_skips_stale_writes() {
        let w = Writer::default();
        let (a, b) = (w.next(), w.next());
        let mut out = Vec::new();
        let mut push = |n: u64| {
            out.push(n);
            Ok(())
        };
        assert!(w.write(b, || push(b)).unwrap());
        assert!(!w.write(a, || push(a)).unwrap());
        assert_eq!(out, [b]);
    }
}
