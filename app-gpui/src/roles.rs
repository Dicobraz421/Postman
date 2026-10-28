//! Цвета и размеры экранов (S5-01, `docs/DESIGN.md` «Цвет»): роли pets-ui ([`Role`], [`Metric`]) и роли Postman
//! (`app.*`, [`Pm`]) поверх них; итог темы для отрисовки — [`Look`] (`look(cx)`).
//!
//! Единственный модуль интерфейса с литералами цвета (тест `no_color_literals` в `main.rs`). Цвета — только из таблицы
//! DESIGN «Токены цвета» (S5-31, решение владельца 2026-10-21 вечер): роли pets-ui, которые таблица задаёт, — слой
//! [`base`] поверх Fluent ([`CORE`]); роли, чьи оттенки почти совпадали с токеном таблицы (ΔE2000 ≤ 5), сведены к
//! нему. Роли вне таблицы сохранили прежние значения — список в тесте `colors_come_from_the_design_table`, его
//! расширяет владелец через таблицу. Цвета категорий, красный безопасности и янтарный сроков — в
//! `postman_core::palette`; здесь они только становятся ролями `app.*`, чтобы тема пользователя могла их
//! переопределить, а высокая контрастность — заменить системными.

use postman_core::prefs::Accent;
use std::rc::Rc;
use std::sync::LazyLock;

use gpui_kit::{App, Global, Hsla, Pixels, Window};
use pets_ui::theme::{AppRole, Color, Density, Metric, Role, Theme, ThemeConfig, ThemeSpec, Themes};
use postman_core::palette::{self, Reserved};
use postman_core::prefs::FreeColor;

/// Роль Postman: имя `app.…` и цвета светлой и тёмной темы ([`FIXED`]).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Pm {
    /// Подписи полей и кнопки панели письма, единые с «Отправить» (DESIGN «Новое письмо») — токен акцента-текста.
    LabelAccent,
    /// Угроза (DESIGN: бледно-красная подложка с тёмно-красным текстом); красный — только безопасность.
    ThreatFg,
    ThreatBg,
    /// Полоса и точка угрозы.
    ThreatDot,
    /// Срок впереди — янтарный текст без подложки («До чт»).
    DeadlineFg,
    /// Срок прошёл — плотная янтарная заливка («Просрочено 1 д») и текст на ней.
    DeadlineFill,
    DeadlineOn,
    /// Флаг письма.
    Flag,
    /// «Закрыть» заголовка окна при наведении — красный Windows, и значок на нём.
    CaptionClose,
    CaptionCloseOn,
    /// Событие календаря: подложка, «под вопросом», полоса, выбранное.
    EventBg,
    EventBgTentative,
    EventBorder,
    EventBorderSelected,
    /// Занятость (помощник планирования, макет «Новая встреча»): занят — заливка, текст на ней, линия (точка
    /// «занят», рамка «под вопросом», кружок «?» участника); под вопросом — заливка и текст; нет на месте — серый
    /// (S5-23): заливка, текст, точка; нет данных, нерабочее время. «Нет данных» и внешний — во встрече заливкой
    /// с прозрачностью.
    FbBusy,
    FbBusyFg,
    FbBusyLine,
    FbTentative,
    FbTentativeFg,
    FbOof,
    FbOofFg,
    FbOofLine,
    FbUnknown,
    FbOff,
    /// Занятость приглушённо (общие окна, S5-11); внешний участник. Свободное время — по выбору в настройках,
    /// [`Look::free`]; `FbFreeOn` — галочка на выбранном образце цвета.
    FbMute,
    FbMuteFg,
    FbFreeOn,
    FbExt,
    /// Пересечение с моим календарём (карточка приглашения, подробности события): текст и подложка.
    ConflictFg,
    ConflictBg,
    /// Лента дня приглашения (S5-24, макет «Приглашение в письме» Г1): пересечение — заливка и текст на ней; свои
    /// события — серые; приглашение — синее (принято — заливка, иначе — полупрозрачно с пунктиром).
    DayOwn,
    DayOwnFg,
    DayOverlap,
    DayOverlapOn,
    DayAccepted,
    DayAcceptedOn,
    /// Второстепенная (тонированная) кнопка — подложка, текст, наведение (S5-19, `crate::buttons`); подложка и текст —
    /// и у метки «Ждёт ответа» приглашения.
    Secondary,
    SecondaryFg,
    SecondaryHover,
    /// Метка «Принято»: зелёная подложка и текст (DESIGN, токены категорий — зелёный).
    OkBg,
    OkFg,
    /// Нерабочие часы в сетке недели.
    CalOff,
    /// «Сообщить о фишинге» — красная маска (DESIGN «Действия и состояния», #CD605A): красный — безопасность.
    Phish,
    /// Совпадение поиска (S5-10): янтарная подложка — янтарный только у сроков и совпадений (DESIGN «Цвет»).
    SearchHit,
    /// Статусы поручения (S6-10, макеты «Поручил я», «Поручено мне»): круги 20 px — принял и в работе синие, готово
    /// зелёное, отклонил тёмно-серый, ждёт — серый точечный; значок на заливке.
    AssignBlue,
    AssignGreen,
    AssignDeclined,
    AssignWait,
    AssignGlyph,
    /// Строка списка (письма, беседы, контакты, папки; T1-D): наведение и выделенная в фокусе — тон акцента поверх
    /// карточки ([`ROWS`]). Без фокуса — нейтральная заливка и полоса цвета подписей ([`Look::row_fill`]).
    RowHover,
    RowSelected,
}

impl Pm {
    pub(crate) const COUNT: usize = Pm::RowSelected as usize + 1;
}

const fn c(v: u32) -> Color {
    Color::rgb(v)
}

const fn a(v: u32) -> Color {
    Color::argb(v)
}

/// Роли pets-ui из таблицы DESIGN «Токены цвета»: (роль, светлая, тёмная). Слой [`base`] поверх Fluent — и для
/// компонентов pets-ui и gpui-kit; при высокой контрастности не действует (системные цвета). Наведение и нажатие
/// акцента — токен акцента с прозрачностью, как в pets-ui (90 % и 80 %).
pub(crate) const CORE: &[(Role, Color, Color)] = &[
    // Акцент: кнопка, ссылка, выбор; тёмная — текст #7CC4F0, заливка кнопки #2B6CA8.
    (Role::AccentFill, c(0x0067C0), c(0x2B6CA8)),
    (Role::AccentFillHover, a(0xE600_67C0), a(0xE62B_6CA8)),
    (Role::AccentFillPressed, a(0xCC00_67C0), a(0xCC2B_6CA8)),
    (Role::AccentText, c(0x0067C0), c(0x7CC4F0)),
    (Role::AccentOn, c(0xFFFFFF), c(0xFFFFFF)),
    // Синяя подложка: метки «Клиенты», тонированные и выбранные кнопки; строки списка — «выбранное» (fill.cursor).
    (Role::AccentSubtle, c(0xDFF0FF), c(0x23364F)),
    // Текст основной / вторичный / подписи.
    (Role::TextPrimary, c(0x242424), c(0xD2D2D2)),
    (Role::TextSecondary, c(0x616161), c(0xA6A6A6)),
    (Role::TextTertiary, c(0x8A8A8A), c(0x8A8A8A)),
    // Фон окна / карточка (и всплывающие — Fluent #F9F9F9 / #2C2C2C, ΔE ≤ 1,2) / наведение / нейтральная заливка
    // (чипы, полосы таблиц, выделенная строка без фокуса). Тёмное окно — #191919 (T1-D: список не сливается с окном).
    (Role::BgBase, c(0xF3F3F3), c(0x191919)),
    (Role::LayerPanel, c(0xFFFFFF), c(0x2B2B2B)),
    (Role::LayerOverlay, c(0xFFFFFF), c(0x2B2B2B)),
    (Role::FillSubtleHover, c(0xF5F5F5), c(0x323232)),
    (Role::FillCursor, c(0xEBEBEB), c(0x3A3A3A)),
    // Разделитель / рамка поля.
    (Role::StrokeDivider, c(0xE0E0E0), c(0x3D3D3D)),
    (Role::StrokeControl, c(0xD1D1D1), c(0x505050)),
    // Подложка ошибки — подложка угрозы (Fluent #FDE7E9 / #442726, ΔE 2,3).
    (Role::CriticalBg, c(0xFFE6E3), c(0x4C2B28)),
];

/// Тема «Серая» (T1-D, решение владельца 2026-10-28): светлая без белого — окно и карточки в оттенках серого, как
/// Outlook. Её цвета — слой [`base`] поверх светлой ([`GRAY_CORE`]); сама тема для pets-ui — пустая светлая
/// ([`gray_theme`]), иначе слой Postman перекрыл бы её.
pub(crate) const GRAY: &str = "gray";

/// Роли «Серой» из таблицы DESIGN; остальное — как в светлой ([`CORE`]).
pub(crate) const GRAY_CORE: &[(Role, Color)] = &[
    (Role::BgBase, c(0xDADADA)),
    (Role::LayerPanel, c(0xEDEDED)),
    (Role::LayerOverlay, c(0xEDEDED)),
    (Role::FillSubtleHover, c(0xE5E5E5)),
    (Role::FillCursor, c(0xDEDEDE)),
    (Role::StrokeDivider, c(0xD5D5D5)),
    (Role::StrokeControl, c(0xC2C2C2)),
    (Role::TextPrimary, c(0x242424)),
    (Role::TextSecondary, c(0x575757)),
    (Role::TextTertiary, c(0x767676)),
];

/// Тема «Серая» для списка тем pets-ui: всегда светлая (системная тёмная её не переключает).
pub(crate) fn gray_theme() -> ThemeSpec {
    ThemeSpec {
        id: GRAY.to_owned(),
        name: Some(pets_i18n::tr!("settings-theme-gray")),
        appearance: Some(pets_ui::theme::Appearance::Light),
        ..ThemeSpec::default()
    }
}

/// Строка списка по акценту (DESIGN «Токены цвета», T1-D): [наведение, выделена] — светлая, тёмная, «Серая».
/// Акцент и [наведение, выделена] светлой, тёмной и «Серой».
type RowTones = (Accent, [u32; 2], [u32; 2], [u32; 2]);

pub(crate) const ROWS: &[RowTones] = &[
    (Accent::System, [0xF0F6FB, 0xD1E4F4], [0x2B333A, 0x2B3E50], [0xDFE5EA, 0xC2D5E5]),
    (Accent::Teal, [0xF0F7F6, 0xD1E6E5], [0x283333, 0x23403E], [0xDFE6E5, 0xC2D7D6]),
    (Accent::Green, [0xF3F7F2, 0xDCE7D7], [0x2D342A, 0x304129], [0xE2E6E1, 0xCDD8C8]),
    (Accent::Violet, [0xF6F4FB, 0xE5DFF2], [0x332F3B, 0x3E3552], [0xE5E3EA, 0xD6D0E3]),
    (Accent::Graphite, [0xF4F5F6, 0xDFE1E3], [0x303133, 0x383B3E], [0xE3E4E5, 0xD0D2D5]),
];

/// Слой таблицы DESIGN поверх Fluent ([`CORE`]; в «Серой» — ещё [`GRAY_CORE`]), строка списка по акценту ([`ROWS`])
/// и плотность — переопределения темы Postman.
pub(crate) fn base(density: Option<Density>, accent: Accent, gray: bool) -> ThemeSpec {
    let table = |dark: bool| {
        let mut t: std::collections::BTreeMap<String, Color> =
            CORE.iter().map(|&(r, l, d)| (r.name().to_owned(), if dark { d } else { l })).collect();
        if gray && !dark {
            t.extend(GRAY_CORE.iter().map(|&(r, v)| (r.name().to_owned(), v)));
            // Нерабочие часы календаря (T1-09, DESIGN «Токены цвета», #153) — темнее карточки #EDEDED.
            t.insert(pm_name(Pm::CalOff).to_owned(), c(0xE2E2E2));
        }
        if let Some(&(_, light, dark_t, gray_t)) = ROWS.iter().find(|(a, ..)| *a == accent) {
            let [hover, selected] = match (dark, gray) {
                (true, _) => dark_t,
                (false, true) => gray_t,
                (false, false) => light,
            };
            t.insert(pm_name(Pm::RowHover).to_owned(), c(hover));
            t.insert(pm_name(Pm::RowSelected).to_owned(), c(selected));
        }
        // Акцент «Внешнего вида» (S5-30): текст, заливка кнопки, тонированная подложка и текст на ней — из таблицы.
        if let Some(&(_, light, dark_t)) = ACCENTS.iter().find(|(a, _, _)| *a == accent) {
            let [text, fill, tint, tint_fg] = if dark { dark_t } else { light };
            // Наведение и нажатие — та же заливка с прозрачностью 90 % и 80 %, как у синего; наведение тонированной —
            // подложка с 8 % текста на ней.
            t.insert(Role::AccentText.name().to_owned(), c(text));
            t.insert(Role::AccentFill.name().to_owned(), c(fill));
            t.insert(Role::AccentFillHover.name().to_owned(), a((0xE6 << 24) | fill));
            t.insert(Role::AccentFillPressed.name().to_owned(), a((0xCC << 24) | fill));
            t.insert(Role::AccentSubtle.name().to_owned(), c(tint));
            t.insert(pm_name(Pm::Secondary).to_owned(), c(tint));
            t.insert(pm_name(Pm::SecondaryFg).to_owned(), c(tint_fg));
            t.insert(pm_name(Pm::SecondaryHover).to_owned(), c(mix(tint, tint_fg, 0.08)));
        }
        t.into_iter().collect()
    };
    ThemeSpec { density, light: table(false), dark: table(true), ..ThemeSpec::default() }
}

/// Акценты «Настройки → Внешний вид» (S5-30, DESIGN «Токены цвета», значения макета `SettingsAppearance`): светлая
/// и тёмная — [текст и ссылка, заливка кнопки, тонированная подложка, текст на ней]. Синий Postman — в [`CORE`].
pub(crate) const ACCENTS: &[(Accent, [u32; 4], [u32; 4])] = &[
    (Accent::Teal, [0x00756F, 0x00756F, 0xE1F3F1, 0x00524D], [0x4DC2B8, 0x11706A, 0x173B38, 0xA6E3DC]),
    (Accent::Green, [0x3A7A1E, 0x3A7A1E, 0xE8F3E1, 0x2A5A15], [0x86C266, 0x3D7425, 0x26371E, 0xBEE0AA]),
    (Accent::Violet, [0x6E4BB5, 0x6E4BB5, 0xEFEAF9, 0x4F3487], [0xB39DEB, 0x6A4CAE, 0x332A4A, 0xD7CBF5]),
    (Accent::Graphite, [0x4D5965, 0x4D5965, 0xECEEF0, 0x36404A], [0xAEB8C2, 0x56616C, 0x33383D, 0xD3DAE0]),
];

/// Цвет точки акцента в настройках: заливка кнопки (светлая) или текст (тёмная), как в макете.
pub(crate) fn accent_swatch(accent: Accent, dark: bool) -> Hsla {
    match ACCENTS.iter().find(|(a, _, _)| *a == accent) {
        Some(&(_, light, d)) => Hsla::from(if dark { c(d[0]) } else { c(light[1]) }),
        None => {
            let pick = |role: Role| CORE.iter().find(|(r, _, _)| *r == role).map(|&(_, l, d)| if dark { d } else { l });
            Hsla::from(pick(if dark { Role::AccentText } else { Role::AccentFill }).unwrap_or(c(0)))
        }
    }
}

/// Смесь двух цветов `0xRRGGBB`: `t` — доля второго.
fn mix(a: u32, b: u32, t: f32) -> u32 {
    let ch = |v: u32, s: u32| ((v >> s) & 0xFF) as f32;
    let one = |s: u32| ((ch(a, s) * (1. - t) + ch(b, s) * t).round() as u32).min(255) << s;
    one(16) | one(8) | one(0)
}

/// Акцент «Внешнего вида» или тема — на лету: слой токенов заново под действующую тему (плотность — та же).
pub(crate) fn set_accent(accent: Accent, density: Option<Density>, cx: &mut App) {
    Themes::update(cx, |s| {
        let gray = s.effective() == GRAY;
        s.set_overrides(Some(base(density, accent, gray)))
    });
}

/// Цвета ролей Postman: (роль, имя, светлая, тёмная, при высокой контрастности — роль pets-ui). Роли из палитры
/// (угроза, срок) — в [`roles`], по `palette::reserved`.
const FIXED: &[(Pm, &str, Color, Color, Role)] = &[
    (Pm::LabelAccent, "app.label.accent", c(0x0067C0), c(0x7CC4F0), Role::AccentText),
    (Pm::DeadlineOn, "app.deadline.on", c(0x664700), c(0xF7E2B8), Role::CautionFg),
    (Pm::Flag, "app.flag", c(0xC50F1F), c(0xD47C76), Role::TextPrimary),
    (Pm::CaptionClose, "app.caption.close", c(0xC42B1C), c(0xC42B1C), Role::AccentFill),
    (Pm::CaptionCloseOn, "app.caption.close.on", c(0xFFFFFF), c(0xFFFFFF), Role::AccentOn),
    (Pm::EventBg, "app.event.bg", c(0xDFF0FF), c(0x23364F), Role::AccentSubtle),
    (Pm::EventBgTentative, "app.event.bg.tentative", c(0xEEF4FB), c(0x1E2A38), Role::LayerPanel),
    (Pm::EventBorder, "app.event.border", c(0x2F6DB5), c(0x689BDB), Role::AccentFill),
    (Pm::EventBorderSelected, "app.event.border.selected", c(0x1B4F8A), c(0xAFD1FC), Role::TextPrimary),
    (Pm::FbBusy, "app.fb.busy", c(0x4687D8), c(0x3A5F8F), Role::AccentFill),
    (Pm::FbBusyFg, "app.fb.busy.fg", c(0xFFFFFF), c(0xE6EEF8), Role::AccentOn),
    (Pm::FbBusyLine, "app.fb.busy.line", c(0x4687D8), c(0x689BDB), Role::AccentFill),
    (Pm::FbTentative, "app.fb.tentative", c(0xDFF0FF), c(0x23364F), Role::AccentSubtle),
    (Pm::FbTentativeFg, "app.fb.tentative.fg", c(0x234E82), c(0xBFDDFF), Role::TextPrimary),
    (Pm::FbOof, "app.fb.oof", c(0x6E6E6E), c(0x4F4F4F), Role::TextPrimary),
    (Pm::FbOofFg, "app.fb.oof.fg", c(0xFFFFFF), c(0xE6E6E6), Role::LayerPanel),
    (Pm::FbOofLine, "app.fb.oof.line", c(0x6E6E6E), c(0x8A8A8A), Role::TextPrimary),
    (Pm::FbUnknown, "app.fb.unknown", c(0xB7B7B7), c(0x727272), Role::TextDisabled),
    (Pm::FbOff, "app.fb.off", c(0xF0F0F0), c(0x2C2C2C), Role::BgBase),
    (Pm::FbMute, "app.fb.mute", c(0xE3E3E3), c(0x3A3A3A), Role::TextDisabled),
    (Pm::FbMuteFg, "app.fb.mute.fg", c(0x8A8A8A), c(0x8A8A8A), Role::TextSecondary),
    (Pm::FbFreeOn, "app.fb.free.on", c(0xFFFFFF), c(0xFFFFFF), Role::AccentOn),
    (Pm::FbExt, "app.fb.ext", c(0x177CAA), c(0x5FA8D3), Role::TextSecondary),
    (Pm::ConflictFg, "app.conflict.fg", c(0x5C2E91), c(0xDFC1EE), Role::TextPrimary),
    (Pm::ConflictBg, "app.conflict.bg", c(0xF3EEFA), c(0x3F2D47), Role::LayerPanel),
    // Лента дня приглашения — DESIGN «Токены цвета», «День: своё / пересечение / принятое».
    (Pm::DayOwn, "app.day.own", c(0xE3E3E3), c(0x444444), Role::FillCursor),
    (Pm::DayOwnFg, "app.day.own.fg", c(0x424242), c(0xD2D2D2), Role::TextPrimary),
    (Pm::DayOverlap, "app.day.overlap", c(0xA569BF), c(0xB681C2), Role::TextPrimary),
    (Pm::DayOverlapOn, "app.day.overlap.on", c(0xFFFFFF), c(0x1A1A1A), Role::LayerPanel),
    (Pm::DayAccepted, "app.day.accepted", c(0x0067C0), c(0x7CC4F0), Role::AccentFill),
    (Pm::DayAcceptedOn, "app.day.accepted.on", c(0xFFFFFF), c(0x1A1A1A), Role::AccentOn),
    // Тонированная (второстепенная) кнопка, S5-19 — DESIGN «Токены цвета», синяя подложка, текст на ней и наведение;
    // высокая контрастность — выделение ОС и текст на нём (`crate::buttons`).
    (Pm::Secondary, "app.button.secondary", c(0xDFF0FF), c(0x23364F), Role::AccentSubtle),
    (Pm::SecondaryFg, "app.button.secondary.fg", c(0x234E82), c(0xBFDDFF), Role::AccentOn),
    (Pm::SecondaryHover, "app.button.secondary.hover", c(0xD0E6FA), c(0x2C4466), Role::AccentFillHover),
    (Pm::OkBg, "app.ok.bg", c(0xE5F4DD), c(0x2A3B20), Role::LayerPanel),
    (Pm::OkFg, "app.ok.fg", c(0x34591B), c(0xBAD9AB), Role::SuccessFg),
    (Pm::CalOff, "app.cal.off", c(0xF6F6F6), c(0x232323), Role::LayerPanel),
    (Pm::Phish, "app.phish", c(0xCD605A), c(0xD47C76), Role::CriticalFg),
    (Pm::SearchHit, "app.search.hit", c(0xF9EDD5), c(0x413315), Role::AccentSubtle),
    // Статусы поручения — по макетам «Поручил я» и «Поручено мне».
    (Pm::AssignBlue, "app.assign.blue", c(0x4687D8), c(0x689BDB), Role::AccentFill),
    (Pm::AssignGreen, "app.assign.green", c(0x5F983A), c(0x7AA761), Role::SuccessFg),
    (Pm::AssignDeclined, "app.assign.declined", c(0x303030), c(0xCECECE), Role::TextPrimary),
    (Pm::AssignWait, "app.assign.wait", c(0xAAAAAA), c(0x6E6E6E), Role::TextSecondary),
    (Pm::AssignGlyph, "app.assign.glyph", c(0xFFFFFF), c(0x2B2B2B), Role::LayerPanel),
    // Строка списка, синий акцент (T1-D); акцент и «Серая» — слой [`base`] по [`ROWS`].
    (Pm::RowHover, "app.row.hover", c(0xF0F6FB), c(0x2B333A), Role::FillSubtleHover),
    (Pm::RowSelected, "app.row.selected", c(0xD1E4F4), c(0x2B3E50), Role::FillCursor),
];

/// Цвет свободного времени (S5-20): только цвета бренда (решение владельца 2026-10-21) — розовый, синий,
/// серо-голубой. У каждого — рамка окна (и свечение, S5-23), подложка, подложка при наведении и текст; светлая и
/// тёмная тема. Тёмная — приглушённая (DESIGN «Цвет»): розовый — вариант B макета встречи, синий и серо-голубой —
/// в его духе. Роли `app.fb.free.<id>[.bg|.hover|.fg]`.
/// Выбор, имена ролей (рамка, подложка, наведение, текст), цвета светлой и тёмной темы.
type FreeRow = (FreeColor, [&'static str; 4], [u32; 4], [u32; 4]);

pub(crate) const FREE: [FreeRow; 3] = [
    (
        FreeColor::Brand,
        ["app.fb.free.brand", "app.fb.free.brand.bg", "app.fb.free.brand.hover", "app.fb.free.brand.fg"],
        [0xE4107F, 0xFCE4EF, 0xF8CDE1, 0x8C0B4E],
        [0xD2569A, 0x3A1A2A, 0x4A2135, 0xF0A8CA],
    ),
    (
        FreeColor::Blue,
        ["app.fb.free.blue", "app.fb.free.blue.bg", "app.fb.free.blue.hover", "app.fb.free.blue.fg"],
        [0x3A9BD5, 0xEDF6FC, 0xD9ECF8, 0x1F5F8A],
        [0x4F94C4, 0x1A2B3A, 0x213648, 0xA8D2F0],
    ),
    (
        FreeColor::Slate,
        ["app.fb.free.slate", "app.fb.free.slate.bg", "app.fb.free.slate.hover", "app.fb.free.slate.fg"],
        [0x7A8FA6, 0xF2F5F8, 0xE3E9EF, 0x4F6378],
        [0x8494A8, 0x262C34, 0x2F3741, 0xC2CCD8],
    ),
];

/// Цвета свободного времени в теме.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Free {
    /// Рамка окна, точка «свободен» у участника, образец в настройках.
    pub(crate) line: Hsla,
    pub(crate) bg: Hsla,
    /// Подложка окна под указателем.
    pub(crate) hover: Hsla,
    pub(crate) fg: Hsla,
}

/// Части цвета категории: точка и полоса, текст и подложка метки.
const CAT_PARTS: [&str; 3] = ["dot", "text", "bg"];

/// Имена ролей категорий `app.cat.<цвет>.<часть>` по порядку `palette::PALETTE` — строки живут весь процесс
/// (имя роли pets-ui — `&'static str`).
static CAT_NAMES: LazyLock<Vec<[String; 3]>> =
    LazyLock::new(|| palette::PALETTE.iter().map(|c| CAT_PARTS.map(|part| format!("app.cat.{}.{part}", c.key))).collect());

/// Все роли Postman для `Themes::init`: постоянные, угроза и срок из палитры, цвета категорий (таблица DESIGN).
pub(crate) static ROLES: LazyLock<Vec<AppRole>> = LazyLock::new(roles);

fn roles() -> Vec<AppRole> {
    let mut out: Vec<AppRole> = FIXED.iter().map(|&(_, name, light, dark, hc)| AppRole { name, light, dark, hc }).collect();
    let reserved = |r: Reserved| (palette::reserved(r, palette::Theme::Light), palette::reserved(r, palette::Theme::Dark));
    let (tl, td) = reserved(Reserved::Threat);
    let (dl, dd) = reserved(Reserved::Deadline);
    out.extend([
        AppRole { name: "app.threat.fg", light: c(tl.text), dark: c(td.text), hc: Role::CriticalFg },
        AppRole { name: "app.threat.bg", light: c(tl.bg), dark: c(td.bg), hc: Role::LayerPanel },
        AppRole { name: "app.threat.dot", light: c(tl.dot), dark: c(td.dot), hc: Role::CriticalFg },
        AppRole { name: "app.deadline.fg", light: c(dl.text), dark: c(dd.text), hc: Role::CautionFg },
        AppRole { name: "app.deadline.fill", light: c(dl.bg), dark: c(dd.bg), hc: Role::CautionBg },
    ]);
    for (_, names, light, dark) in FREE {
        let hc = [Role::AccentFill, Role::LayerPanel, Role::FillSubtleHover, Role::TextPrimary];
        for i in 0..4 {
            out.push(AppRole { name: names[i], light: c(light[i]), dark: c(dark[i]), hc: hc[i] });
        }
    }
    for (color, names) in palette::PALETTE.iter().zip(CAT_NAMES.iter()) {
        let (l, d) = (color.swatch(palette::Theme::Light), color.swatch(palette::Theme::Dark));
        let hc = [Role::TextPrimary, Role::TextPrimary, Role::LayerPanel];
        for (i, (light, dark)) in [(l.dot, d.dot), (l.text, d.text), (l.bg, d.bg)].into_iter().enumerate() {
            out.push(AppRole { name: names[i].as_str(), light: c(light), dark: c(dark), hc: hc[i] });
        }
    }
    out
}

/// Имя роли из палитры для [`Pm`] (постоянные — в [`FIXED`]).
fn pm_name(r: Pm) -> &'static str {
    match r {
        Pm::ThreatFg => "app.threat.fg",
        Pm::ThreatBg => "app.threat.bg",
        Pm::ThreatDot => "app.threat.dot",
        Pm::DeadlineFg => "app.deadline.fg",
        Pm::DeadlineFill => "app.deadline.fill",
        other => FIXED.iter().find(|f| f.0 == other).map_or("app.text", |f| f.1),
    }
}

const PMS: [Pm; Pm::COUNT] = [
    Pm::LabelAccent,
    Pm::ThreatFg,
    Pm::ThreatBg,
    Pm::ThreatDot,
    Pm::DeadlineFg,
    Pm::DeadlineFill,
    Pm::DeadlineOn,
    Pm::Flag,
    Pm::CaptionClose,
    Pm::CaptionCloseOn,
    Pm::EventBg,
    Pm::EventBgTentative,
    Pm::EventBorder,
    Pm::EventBorderSelected,
    Pm::FbBusy,
    Pm::FbBusyFg,
    Pm::FbBusyLine,
    Pm::FbTentative,
    Pm::FbTentativeFg,
    Pm::FbOof,
    Pm::FbOofFg,
    Pm::FbOofLine,
    Pm::FbUnknown,
    Pm::FbOff,
    Pm::FbMute,
    Pm::FbMuteFg,
    Pm::FbFreeOn,
    Pm::FbExt,
    Pm::ConflictFg,
    Pm::ConflictBg,
    Pm::DayOwn,
    Pm::DayOwnFg,
    Pm::DayOverlap,
    Pm::DayOverlapOn,
    Pm::DayAccepted,
    Pm::DayAcceptedOn,
    Pm::Secondary,
    Pm::SecondaryFg,
    Pm::SecondaryHover,
    Pm::OkBg,
    Pm::OkFg,
    Pm::CalOff,
    Pm::Phish,
    Pm::SearchHit,
    Pm::AssignBlue,
    Pm::AssignGreen,
    Pm::AssignDeclined,
    Pm::AssignWait,
    Pm::AssignGlyph,
    Pm::RowHover,
    Pm::RowSelected,
];

/// Цвета категории в теме.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Cat {
    pub(crate) dot: Hsla,
    pub(crate) text: Hsla,
    pub(crate) bg: Hsla,
}

/// Итоговая тема для отрисовки: роли pets-ui, роли Postman и категории — по индексу, без поиска по имени.
/// Пересчитывается при смене темы ([`init`]); в `render` — `let l = look(cx);` (дёшево: `Rc`).
#[derive(Clone, Debug)]
pub(crate) struct Look {
    theme: Theme,
    pm: [Hsla; Pm::COUNT],
    cats: Vec<Cat>,
    free: [Free; 3],
}

impl Look {
    pub(crate) fn new(theme: &Theme) -> Look {
        let pm = PMS.map(|r| theme.app(pm_name(r)));
        let cats = CAT_NAMES.iter().map(|n| Cat { dot: theme.app(&n[0]), text: theme.app(&n[1]), bg: theme.app(&n[2]) }).collect();
        let free = FREE.map(|(_, n, _, _)| Free { line: theme.app(n[0]), bg: theme.app(n[1]), hover: theme.app(n[2]), fg: theme.app(n[3]) });
        Look { theme: theme.clone(), pm, cats, free }
    }

    /// Цвета свободного времени по выбору «Настройки → Календарь».
    pub(crate) fn free(&self, color: FreeColor) -> Free {
        let i = FREE.iter().position(|f| f.0 == color).unwrap_or(0);
        self.free[i]
    }

    pub(crate) fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Цвет роли pets-ui.
    pub(crate) fn c(&self, r: Role) -> Hsla {
        self.theme.c(r)
    }

    /// Цвет роли Postman.
    pub(crate) fn pm(&self, r: Pm) -> Hsla {
        self.pm[r as usize]
    }

    /// Цвета категории по имени цвета (прежнее имя Fluent — ближайший цвет таблицы); неизвестный — нейтральный.
    pub(crate) fn cat(&self, key: &str) -> Cat {
        let index = |k: &str| palette::PALETTE.iter().position(|c| c.key == k);
        let i = index(palette::resolve(key)).or_else(|| index(palette::NEUTRAL));
        i.and_then(|i| self.cats.get(i)).copied().unwrap_or(Cat { dot: self.c(Role::TextTertiary), text: self.text2(), bg: self.c(Role::FillCursor) })
    }

    pub(crate) fn px(&self, m: Metric) -> Pixels {
        self.theme.px(m)
    }

    pub(crate) fn m(&self, m: Metric) -> f32 {
        self.theme.m(m)
    }

    pub(crate) fn dark(&self) -> bool {
        self.theme.is_dark()
    }

    pub(crate) fn density(&self) -> Density {
        self.theme.density()
    }

    /// Компактная плотность: строка письма без аватара и превью (DESIGN «Главное окно»).
    pub(crate) fn compact(&self) -> bool {
        self.density() == Density::Compact
    }

    // --- частые роли ---

    pub(crate) fn text(&self) -> Hsla {
        self.c(Role::TextPrimary)
    }

    pub(crate) fn text2(&self) -> Hsla {
        self.c(Role::TextSecondary)
    }

    pub(crate) fn text3(&self) -> Hsla {
        self.c(Role::TextTertiary)
    }

    /// Фон каркаса окна.
    pub(crate) fn base(&self) -> Hsla {
        self.c(Role::BgBase)
    }

    /// Карточка (список, область чтения, панели).
    pub(crate) fn panel(&self) -> Hsla {
        self.c(Role::LayerPanel)
    }

    /// Разделители и рамки карточек.
    pub(crate) fn border(&self) -> Hsla {
        self.c(Role::StrokeDivider)
    }

    /// Рамка контрола (кнопка, поле).
    pub(crate) fn stroke(&self) -> Hsla {
        self.c(Role::StrokeControl)
    }

    pub(crate) fn hover(&self) -> Hsla {
        self.c(Role::FillSubtleHover)
    }

    pub(crate) fn pressed(&self) -> Hsla {
        self.c(Role::FillSubtlePressed)
    }

    /// Синяя подложка: выбранная кнопка-переключатель, тонированные кнопки (DESIGN «Токены цвета»).
    pub(crate) fn picked(&self) -> Hsla {
        self.c(Role::AccentSubtle)
    }

    /// Нейтральное «выбранное» (элементы gpui-kit); строки списков — [`Look::row_fill`].
    pub(crate) fn selected(&self) -> Hsla {
        self.c(Role::FillCursor)
    }

    /// Тема «Серая»: панель папок — карточкой (DESIGN «Токены цвета»).
    pub(crate) fn gray(&self) -> bool {
        self.theme.id() == GRAY
    }

    /// Строка списка (письма, беседы, контакты, папки; T1-D) под указателем — тон акцента 6 % (тёмная 12 %).
    pub(crate) fn row_hover(&self) -> Hsla {
        self.pm(Pm::RowHover)
    }

    /// Выделенная строка: в фокусе — тон акцента 18 % (тёмная 30 %), без фокуса — нейтральная заливка.
    pub(crate) fn row_fill(&self, focused: bool) -> Hsla {
        if focused { self.pm(Pm::RowSelected) } else { self.c(Role::FillCursor) }
    }

    /// Полоса слева у выделенной строки: в фокусе — акцент, без фокуса — цвет подписей.
    pub(crate) fn row_strip(&self, focused: bool) -> Hsla {
        if focused { self.accent() } else { self.text3() }
    }

    pub(crate) fn accent(&self) -> Hsla {
        self.c(Role::AccentFill)
    }

    pub(crate) fn accent_hover(&self) -> Hsla {
        self.c(Role::AccentFillHover)
    }

    pub(crate) fn accent_text(&self) -> Hsla {
        self.c(Role::AccentText)
    }

    /// Текст на акцентной заливке.
    pub(crate) fn on_accent(&self) -> Hsla {
        self.c(Role::AccentOn)
    }

    pub(crate) fn error(&self) -> Hsla {
        self.c(Role::CriticalFg)
    }

    pub(crate) fn error_bg(&self) -> Hsla {
        self.c(Role::CriticalBg)
    }

    pub(crate) fn warning_bg(&self) -> Hsla {
        self.c(Role::CautionBg)
    }

    pub(crate) fn warning(&self) -> Hsla {
        self.c(Role::CautionFg)
    }

    pub(crate) fn success(&self) -> Hsla {
        self.c(Role::SuccessFg)
    }

    /// Нейтральная заливка (код, полосы таблиц, подложки чипов).
    pub(crate) fn fill(&self) -> Hsla {
        self.c(Role::FillCursor)
    }

    pub(crate) fn fill_passive(&self) -> Hsla {
        self.c(Role::FillCursorPassive)
    }

    /// Прозрачный — рамка «нет рамки» той же толщины.
    pub(crate) fn none(&self) -> Hsla {
        gpui_kit::transparent_black()
    }
}

impl Default for Look {
    /// Светлая тема с таблицей DESIGN — до [`init`] (тесты).
    fn default() -> Self {
        static LIGHT: LazyLock<Theme> = LazyLock::new(|| {
            pets_ui::theme::resolve(
                &ThemeSpec::default(),
                Some(&base(None, Accent::System, false)),
                &pets_ui::theme::OsLook::default(),
                pets_ui::theme::FontSizes::default(),
                &ROLES,
            )
        });
        Look::new(&LIGHT)
    }
}

struct Current(Rc<Look>);

impl Global for Current {}

/// Текущая тема для отрисовки; до [`init`] (тесты) — Fluent светлая.
pub(crate) fn look(cx: &App) -> Rc<Look> {
    cx.try_global::<Current>().map_or_else(|| Rc::new(Look::default()), |c| c.0.clone())
}

thread_local! {
    /// Та же тема для мелких построителей без `cx` (кнопки, чипы): поток интерфейса один, тема ставится в
    /// [`refresh`] до перерисовки окон.
    static NOW: std::cell::RefCell<Option<Rc<Look>>> = const { std::cell::RefCell::new(None) };
}

/// Текущая тема без `cx` — для построителей элементов, которым её неоткуда передать; в `render` — [`look`].
pub(crate) fn now() -> Rc<Look> {
    NOW.with(|n| n.borrow().clone()).unwrap_or_else(|| Rc::new(Look::default()))
}

/// `--theme light|dark|gray|system`: тема снимков и запуска; по умолчанию — «как в ОС».
pub(crate) fn choice(arg: Option<&str>) -> &'static str {
    match arg {
        Some("light") => pets_ui::theme::FLUENT_LIGHT,
        Some("dark") => pets_ui::theme::FLUENT_DARK,
        Some("gray") => GRAY,
        _ => pets_ui::theme::SYSTEM,
    }
}

/// `--density compact|normal|comfortable`.
pub(crate) fn density(arg: Option<&str>) -> Option<Density> {
    match arg {
        Some("compact") => Some(Density::Compact),
        Some("normal") => Some(Density::Normal),
        Some("comfortable") => Some(Density::Comfortable),
        Some("spacious") => Some(Density::Spacious),
        _ => None,
    }
}

/// Темы при запуске (до первого окна): «как в ОС» или `--theme`, плотность `--density`; смена темы ОС, акцента
/// или файла темы — на лету: [`Look`] пересчитывается, компоненты gpui-kit (поля ввода) — в тот же вид.
pub(crate) fn init(choice: &str, density: Option<Density>, accent: Accent, cx: &mut App) {
    Themes::init(
        ThemeConfig {
            app: "Postman",
            choice: choice.to_owned(),
            overrides: Some(base(density, accent, choice == GRAY)),
            // «Серая» — своя тема Postman в списке тем (T1-D).
            user_themes: vec![gray_theme()],
            app_roles: ROLES.as_slice(),
            ..ThemeConfig::default()
        },
        cx,
    );
    // Политика могла выбрать тему за пользователя: слой — под действующую.
    set_accent(accent, density, cx);
    refresh(cx);
    cx.observe_global::<Themes>(refresh).detach();
}

/// Окно следит за видом ОС (светлая ↔ тёмная) сразу, не дожидаясь потока слежения pets-ui.
pub(crate) fn observe_window(window: &Window) {
    Themes::observe_window(window).detach();
}

fn refresh(cx: &mut App) {
    let next = Look::new(Theme::global(cx));
    let changed = cx.try_global::<Current>().is_none_or(|c| c.0.theme() != next.theme());
    if !changed {
        return;
    }
    sync_components(&next, cx);
    let next = Rc::new(next);
    NOW.with(|n| *n.borrow_mut() = Some(next.clone()));
    cx.set_global(Current(next));
}

/// Поля ввода и полосы прокрутки gpui-kit — в цвета темы: свой вид (светлая/тёмная) и роли pets-ui.
fn sync_components(l: &Look, cx: &mut App) {
    use gpui_kit::component::{Theme as Kit, ThemeMode};
    let mode = if l.dark() { ThemeMode::Dark } else { ThemeMode::Light };
    if Kit::global(cx).mode != mode {
        Kit::change(mode, None, cx);
    }
    Kit::update(cx, |k| {
        k.background = l.c(Role::FillControlInput);
        k.foreground = l.text();
        k.muted = l.fill();
        k.muted_foreground = l.text3();
        k.border = l.stroke();
        k.input = l.c(Role::StrokeControl);
        k.ring = l.accent();
        k.caret = l.text();
        k.selection = l.picked();
        k.accent = l.hover();
        k.accent_foreground = l.text();
        k.primary = l.accent();
        k.primary_foreground = l.on_accent();
        k.popover = l.c(Role::LayerOverlay);
        k.popover_foreground = l.text();
        k.link = l.accent_text();
    });
}

#[cfg(test)]
mod tests {
    use super::{GRAY, Look, PMS, Pm, ROLES, base, gray_theme, pm_name};
    use pets_ui::theme::{FontSizes, OsLook, ThemeSpec, resolve};
    use postman_core::prefs::Accent;

    /// Акцент «Внешнего вида» (S5-30): заливка, текст и тонированная подложка — токены акцента; синий — по умолчанию.
    #[test]
    fn accent_tokens_apply() {
        let hex = |c: gpui_kit::Hsla| {
            let r = gpui_kit::Rgba::from(c);
            format!("#{:02X}{:02X}{:02X}", (r.r * 255.).round() as u8, (r.g * 255.).round() as u8, (r.b * 255.).round() as u8)
        };
        let look = |dark: bool, accent: Accent| {
            let os = OsLook { dark, ..OsLook::default() };
            Look::new(&resolve(&ThemeSpec::default(), Some(&base(None, accent, false)), &os, FontSizes::default(), &ROLES))
        };
        let l = look(false, Accent::Teal);
        assert_eq!(
            [hex(l.accent()), hex(l.accent_text()), hex(l.picked()), hex(l.pm(Pm::SecondaryFg))],
            ["#00756F", "#00756F", "#E1F3F1", "#00524D"]
        );
        let d = look(true, Accent::Violet);
        assert_eq!([hex(d.accent()), hex(d.accent_text()), hex(d.pm(Pm::Secondary))], ["#6A4CAE", "#B39DEB", "#332A4A"]);
        assert_eq!(hex(look(false, Accent::System).accent()), "#0067C0");
    }

    #[test]
    fn every_role_is_declared_once() {
        for (i, r) in PMS.iter().enumerate() {
            assert_eq!(*r as usize, i, "{r:?}: порядок PMS");
            assert!(ROLES.iter().any(|a| a.name == pm_name(*r)), "{r:?}: роль {} не объявлена", pm_name(*r));
        }
        let mut names: Vec<&str> = ROLES.iter().map(|a| a.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before, "имена ролей повторяются");
        assert!(names.iter().all(|n| n.starts_with("app.")));
    }

    /// Цвет свободного времени (S5-20), все три выбора в обеих темах: текст на подложке и на подложке наведения
    /// ≥ 4,5:1, рамка на панели (белая / #2B2B2B) ≥ 3:1 (не текст, WCAG 1.4.11), рамка далека от красного
    /// безопасности и янтарного сроков (ΔE2000 ≥ 25). Исключение — розовый бренда (#E4107F, тёмная — #D2569A): до
    /// красного ΔE ≈ 19 и 23,5 — решение владельца, DESIGN «Цвет» (строка «Исключение»: только свободное время, не
    /// рядом с красным безопасности); держим ≥ 18.
    #[test]
    fn free_time_colors_contrast_and_distance() {
        use postman_core::palette::{Reserved, Theme as T, delta_e, reserved};
        use postman_core::prefs::FreeColor;
        fn lum(v: u32) -> f64 {
            let ch = |s: u32| {
                let c = f64::from((v >> s) & 0xFF) / 255.0;
                if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
            };
            0.2126 * ch(16) + 0.7152 * ch(8) + 0.0722 * ch(0)
        }
        fn contrast(a: u32, b: u32) -> f64 {
            let (x, y) = (lum(a), lum(b));
            (x.max(y) + 0.05) / (x.min(y) + 0.05)
        }
        assert_eq!(super::FREE.map(|f| f.0), FreeColor::ALL, "по выбору, по порядку");
        for (color, names, light, dark) in super::FREE {
            for (theme, [line, bg, hover, fg], panel) in [(T::Light, light, 0xFFFFFF), (T::Dark, dark, 0x2B2B2B)] {
                assert!(names[0].ends_with(color.id()), "{color:?}: {}", names[0]);
                for under in [bg, hover] {
                    let tc = contrast(fg, under);
                    assert!(tc >= 4.5, "{color:?} {theme:?}: текст {tc:.2}");
                }
                let lc = contrast(line, panel);
                assert!(lc >= 3.0, "{color:?} {theme:?}: рамка {lc:.2}");
                for r in Reserved::ALL {
                    let d = delta_e(line, reserved(r, theme).dot);
                    let min = if (color, r) == (FreeColor::Brand, Reserved::Threat) { 18.0 } else { 25.0 };
                    assert!(d >= min, "{color:?} {theme:?} ~ {r:?}: ΔE {d:.1}");
                }
            }
        }
        // Look отдаёт роли выбранного цвета; по умолчанию — розовый бренда.
        let look = theme(&OsLook::default());
        let hex = |c| pets_ui::theme::Color::from_hsla(c).hex();
        assert_eq!(hex(look.free(FreeColor::default()).line), "#E4107F");
        assert_eq!(hex(look.free(FreeColor::Blue).line), "#3A9BD5");
        assert_eq!(hex(look.free(FreeColor::Slate).fg), "#4F6378");
    }

    /// Тема Postman, как после [`super::init`]: Fluent и слой таблицы DESIGN.
    fn theme(os: &OsLook) -> Look {
        Look::new(&resolve(&ThemeSpec::default(), Some(&base(None, Accent::System, false)), os, FontSizes::default(), &ROLES))
    }

    /// T1-D: строка списка — тон акцента (наведение, выделена в фокусе), без фокуса — нейтральная заливка и полоса
    /// цвета подписей; тёмное окно #191919; «Серая» — окно и карточки в оттенках серого, всегда светлая.
    #[test]
    fn selection_surfaces_and_gray_theme() {
        let hex = |c| pets_ui::theme::Color::from_hsla(c).hex();
        let look = |spec: &ThemeSpec, dark: bool, accent: Accent| {
            let os = OsLook { dark, ..OsLook::default() };
            Look::new(&resolve(spec, Some(&base(None, accent, spec.id == GRAY)), &os, FontSizes::default(), &ROLES))
        };
        let plain = ThemeSpec::default();
        let (l, d) = (look(&plain, false, Accent::System), look(&plain, true, Accent::System));
        assert_eq!([hex(l.row_hover()), hex(l.row_fill(true)), hex(l.row_fill(false))], ["#F0F6FB", "#D1E4F4", "#EBEBEB"]);
        assert_eq!([hex(d.row_hover()), hex(d.row_fill(true)), hex(d.row_fill(false))], ["#2B333A", "#2B3E50", "#3A3A3A"]);
        assert_eq!([hex(l.row_strip(true)), hex(l.row_strip(false))], ["#0067C0", "#8A8A8A"]);
        assert_eq!([hex(d.base()), hex(d.panel())], ["#191919", "#2B2B2B"]);
        let teal = look(&plain, true, Accent::Teal);
        assert_eq!([hex(teal.row_hover()), hex(teal.row_fill(true))], ["#283333", "#23403E"]);
        let gray = gray_theme();
        // Системная тёмная «Серую» не переключает.
        let g = look(&gray, true, Accent::System);
        assert!(g.gray() && !g.dark() && !l.gray());
        assert_eq!([hex(g.base()), hex(g.panel()), hex(g.hover()), hex(g.fill())], ["#DADADA", "#EDEDED", "#E5E5E5", "#DEDEDE"]);
        assert_eq!([hex(g.border()), hex(g.stroke())], ["#D5D5D5", "#C2C2C2"]);
        assert_eq!([hex(g.text()), hex(g.text2()), hex(g.text3())], ["#242424", "#575757", "#767676"]);
        assert_eq!([hex(g.row_hover()), hex(g.row_fill(true))], ["#DFE5EA", "#C2D5E5"]);
        assert_eq!(hex(g.accent()), "#0067C0", "акцент — как в светлой");
        let gv = look(&gray, false, Accent::Violet);
        assert_eq!([hex(gv.row_hover()), hex(gv.row_fill(true))], ["#E5E3EA", "#D6D0E3"]);
    }

    /// Тёмная и светлая тема — токены таблицы DESIGN; категории — тёмные варианты палитры.
    #[test]
    fn themes_use_design_tokens() {
        let (l, d) = (theme(&OsLook::default()), theme(&OsLook { dark: true, ..OsLook::default() }));
        assert!(d.dark() && !l.dark());
        let hex = |c| pets_ui::theme::Color::from_hsla(c).hex();
        assert_eq!([hex(d.text()), hex(d.text2()), hex(d.text3())], ["#D2D2D2", "#A6A6A6", "#8A8A8A"]);
        assert_eq!([hex(l.text()), hex(l.text2()), hex(l.text3())], ["#242424", "#616161", "#8A8A8A"]);
        assert_eq!([hex(d.accent()), hex(d.accent_text()), hex(d.pm(Pm::LabelAccent))], ["#2B6CA8", "#7CC4F0", "#7CC4F0"]);
        assert_eq!([hex(l.accent()), hex(l.accent_text()), hex(l.picked())], ["#0067C0", "#0067C0", "#DFF0FF"]);
        assert_eq!([hex(d.base()), hex(d.panel()), hex(d.hover()), hex(d.fill())], ["#191919", "#2B2B2B", "#323232", "#3A3A3A"]);
        assert_eq!([hex(l.border()), hex(l.stroke()), hex(d.border()), hex(d.stroke())], ["#E0E0E0", "#D1D1D1", "#3D3D3D", "#505050"]);
        assert_eq!([hex(d.pm(Pm::ThreatBg)), hex(d.pm(Pm::ThreatFg))], ["#4C2B28", "#F9BDB7"]);
        assert_eq!([hex(l.pm(Pm::DeadlineFg)), hex(l.pm(Pm::DeadlineFill)), hex(l.pm(Pm::DeadlineOn))], ["#664700", "#F8DEA8", "#664700"]);
        let blue = postman_core::palette::BLUE;
        let dark_bg = postman_core::palette::swatch(blue, postman_core::palette::Theme::Dark).bg;
        assert_eq!(hex(d.cat(blue).bg), format!("#{dark_bg:06X}"));
        assert_ne!(l.cat(blue).bg, d.cat(blue).bg);
        // Розовый бренда — у «Тендеров»; прежнее имя Fluent — ближайший цвет таблицы; неизвестный — нейтральный.
        assert_eq!(hex(l.cat(postman_core::palette::PINK).dot), "#E4107F");
        assert_eq!(l.cat("lightTeal"), l.cat(postman_core::palette::GREY));
        assert_eq!(l.cat("no-such-color"), l.cat(postman_core::palette::NEUTRAL));
        // До `init` (тесты экранов) — та же светлая тема.
        assert_eq!(Look::default().pm, l.pm);
        assert_eq!(hex(Look::default().text()), "#242424");
    }

    /// Цвета вне таблицы DESIGN «Токены цвета»: (цвет, где и почему). Таблицу расширяет владелец; новый цвет — сначала
    /// в таблицу (DESIGN), затем в код — иначе тест [`colors_come_from_the_design_table`] не пройдёт.
    const OUTSIDE: &[(&str, &str)] = &[
        ("#C50F1F", "app.flag, светлая: флаг письма"),
        ("#D47C76", "app.flag и app.phish, тёмная"),
        ("#CD605A", "app.phish: маска «Сообщить о фишинге» (DESIGN «Действия и состояния»)"),
        ("#C42B1C", "app.caption.close: «Закрыть» окна при наведении — красный Windows"),
        ("#EEF4FB", "app.event.bg.tentative, светлая"),
        ("#1E2A38", "app.event.bg.tentative, тёмная"),
        ("#2F6DB5", "app.event.border, светлая (ΔE 10 к синему категорий)"),
        ("#1B4F8A", "app.event.border.selected, светлая"),
        ("#AFD1FC", "app.event.border.selected, тёмная"),
        ("#3A5F8F", "app.fb.busy, тёмная: «занят» во встрече (макет, S5-23)"),
        ("#E6EEF8", "app.fb.busy.fg, тёмная: текст на «занят»"),
        ("#4F4F4F", "app.fb.oof, тёмная"),
        ("#E6E6E6", "app.fb.oof.fg, тёмная"),
        ("#727272", "app.fb.unknown, тёмная: «нет данных» (S5-23)"),
        ("#F0F0F0", "app.fb.off, светлая"),
        ("#2C2C2C", "app.fb.off, тёмная"),
        ("#424242", "app.day.own.fg, светлая: текст своего события в ленте дня (S5-24)"),
        ("#1A1A1A", "app.day.overlap.on и app.day.accepted.on, тёмная: текст на заливке ленты дня (S5-24)"),
        ("#5C2E91", "app.conflict.fg, светлая: пересечение — фиолетовое (DESIGN «Календарь»)"),
        ("#DFC1EE", "app.conflict.fg, тёмная"),
        ("#F3EEFA", "app.conflict.bg, светлая"),
        ("#3F2D47", "app.conflict.bg, тёмная"),
        ("#F9EDD5", "app.search.hit, светлая: совпадение поиска"),
        ("#413315", "app.search.hit, тёмная"),
        ("#F8CDE1", "app.fb.free.brand.hover, светлая"),
        ("#4A2135", "app.fb.free.brand.hover, тёмная (DESIGN «Цвет», вариант B)"),
        ("#3A9BD5", "app.fb.free.blue: синий бренда (DESIGN «Цвет»)"),
        ("#EDF6FC", "app.fb.free.blue.bg, светлая"),
        ("#D9ECF8", "app.fb.free.blue.hover, светлая"),
        ("#1F5F8A", "app.fb.free.blue.fg, светлая"),
        ("#4F94C4", "app.fb.free.blue, тёмная"),
        ("#1A2B3A", "app.fb.free.blue.bg, тёмная"),
        ("#213648", "app.fb.free.blue.hover, тёмная"),
        ("#A8D2F0", "app.fb.free.blue.fg, тёмная"),
        ("#7A8FA6", "app.fb.free.slate: серо-голубой бренда (DESIGN «Цвет»)"),
        ("#F2F5F8", "app.fb.free.slate.bg, светлая"),
        ("#E3E9EF", "app.fb.free.slate.hover, светлая"),
        ("#4F6378", "app.fb.free.slate.fg, светлая"),
        ("#8494A8", "app.fb.free.slate, тёмная"),
        ("#262C34", "app.fb.free.slate.bg, тёмная"),
        ("#2F3741", "app.fb.free.slate.hover, тёмная"),
        ("#C2CCD8", "app.fb.free.slate.fg, тёмная"),
        ("#D13438", "точка и полоса угрозы (palette::Reserved::Threat) — в таблице только подложка и текст"),
        ("#EAA300", "точка срока (palette::Reserved::Deadline) для проверки ΔE — в таблице только текст и подложка"),
    ];

    /// Цвета `0xRRGGBB` и `0xAARRGGBB` в коде (до тестов): цвет — последние шесть цифр.
    fn literals(source: &str) -> Vec<String> {
        let code = source.split("#[cfg(test)]").next().unwrap_or(source);
        let mut out = Vec::new();
        for (i, _) in code.match_indices("0x") {
            let digits: String = code[i + 2..].chars().take_while(|c| c.is_ascii_hexdigit() || *c == '_').filter(|c| *c != '_').collect();
            if matches!(digits.len(), 6 | 8) {
                out.push(format!("#{}", digits[digits.len() - 6..].to_ascii_uppercase()));
            }
        }
        out
    }

    /// Единственный источник цвета (S5-31): каждый цвет в `roles.rs` и `postman_core::palette` — из таблицы DESIGN
    /// «Токены цвета» или из списка [`OUTSIDE`]; список не устаревает (каждый цвет в нём — в коде и не в таблице).
    #[test]
    fn colors_come_from_the_design_table() {
        let design = include_str!("../../docs/DESIGN.md");
        let start = design.find("## Токены цвета").expect("DESIGN: раздел «Токены цвета»");
        let table = &design[start..];
        let table = &table[..table[3..].find("\n## ").map_or(table.len(), |e| e + 3)];
        let tokens: Vec<String> = table
            .match_indices('#')
            .filter_map(|(i, _)| table.get(i + 1..i + 7))
            .filter(|h| h.chars().all(|c| c.is_ascii_hexdigit()))
            .map(|h| format!("#{}", h.to_ascii_uppercase()))
            .collect();
        assert!(tokens.len() > 40, "таблица DESIGN не разобрана: {tokens:?}");
        let mut used = Vec::new();
        let mut stray = Vec::new();
        for (file, source) in [("roles.rs", include_str!("roles.rs")), ("palette.rs", include_str!("../../crates/core/src/palette.rs"))] {
            for hex in literals(source) {
                if !tokens.contains(&hex) && !OUTSIDE.iter().any(|(c, _)| *c == hex) {
                    stray.push(format!("{file}: {hex}"));
                }
                used.push(hex);
            }
        }
        assert!(stray.is_empty(), "цвет не из таблицы DESIGN «Токены цвета» и не в OUTSIDE:\n{}", stray.join("\n"));
        for (hex, what) in OUTSIDE {
            assert!(!tokens.contains(&(*hex).to_owned()), "{hex} ({what}) уже в таблице — убрать из OUTSIDE");
            assert!(used.iter().any(|u| u == hex), "{hex} ({what}) в коде больше нет — убрать из OUTSIDE");
        }
    }
}
