//! Палитра категорий и меток (S5-02, S5-31): свои токены `docs/DESIGN.md` «Токены цвета» — решение владельца
//! 2026-10-21 (вечер, `docs/specs/S0-decisions.md`), заменило «общие цвета» Fluent 2.
//!
//! Пять цветов таблицы — синий, зелёный, розовый бренда, графит, серый; у каждого точка (и полоса в строке письма),
//! подложка и текст метки для светлой и тёмной темы. Бирюзы нет. Цветов пять — шестой категории цвета нет (решение
//! владельца 2026-10-22, новые цвета — через дизайн).
//!
//! Красный — только безопасность, янтарный — только сроки ([`Reserved`]): подложка и текст — из таблицы, точки —
//! прежние (вне таблицы). В одной таблице категорий цвета не близки (ΔE2000 ≥ [`MIN_DELTA_E`], [`clash`], [`check`]);
//! принятое исключение — [`EXEMPT`] (розовый бренда и красный безопасности).
//!
//! Цвета категорий, сохранённые прежними версиями (имена Fluent: `darkGreen`, `lightTeal`…), рисуются ближайшим цветом
//! таблицы ([`LEGACY`], [`resolve`]); категории по умолчанию — своим цветом по имени (`labels::Category::shown_color`).
//! При правке категории сохраняется новое имя.

use std::sync::OnceLock;

/// Цвет sRGB `0xRRGGBB`.
pub type Rgb = u32;

/// Наименьшая разница цветов в одном списке (ΔE2000).
pub const MIN_DELTA_E: f64 = 25.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Theme {
    Light,
    Dark,
}

impl Theme {
    pub const ALL: [Theme; 2] = [Theme::Light, Theme::Dark];
}

/// Цвет в OKLCH: светлота 0…1, насыщенность (≈0…0,4), оттенок в градусах.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Oklch {
    pub l: f64,
    pub c: f64,
    pub h: f64,
}

/// Цвета роли в теме.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Swatch {
    /// Точка у фильтра и полоса слева в строке письма.
    pub dot: Rgb,
    /// Текст метки.
    pub text: Rgb,
    /// Подложка метки.
    pub bg: Rgb,
}

/// Цвет категории: имя (так он хранится в таблице категорий на сервере) и цвета светлой и тёмной темы.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub key: &'static str,
    pub light: Swatch,
    pub dark: Swatch,
}

/// Точка, подложка, текст — в порядке таблицы DESIGN.
const fn sw(dot: Rgb, bg: Rgb, text: Rgb) -> Swatch {
    Swatch { dot, text, bg }
}

impl Color {
    pub fn swatch(&self, theme: Theme) -> Swatch {
        match theme {
            Theme::Light => self.light,
            Theme::Dark => self.dark,
        }
    }
}

pub const BLUE: &str = "blue";
pub const GREEN: &str = "green";
/// Розовый бренда.
pub const PINK: &str = "pink";
pub const GRAPHITE: &str = "graphite";
pub const GREY: &str = "grey";
/// Нейтральный: метки Thunderbird вне таблицы, служебные метки, неизвестный цвет.
pub const NEUTRAL: &str = GREY;

/// Цвета категорий — строка «Категории» таблицы DESIGN, по её порядку.
pub const PALETTE: &[Color] = &[
    Color { key: BLUE, light: sw(0x4687D8, 0xDFF0FF, 0x234E82), dark: sw(0x689BDB, 0x23364F, 0xBFDDFF) },
    Color { key: GREEN, light: sw(0x5F983A, 0xE5F4DD, 0x34591B), dark: sw(0x709349, 0x2A3B20, 0xBAD9AB) },
    Color { key: PINK, light: sw(0xE4107F, 0xFCE4EF, 0x8C0B4E), dark: sw(0xD2569A, 0x3A1A2A, 0xF0A8CA) },
    Color { key: GRAPHITE, light: sw(0x5A5A5A, 0xEBEBEB, 0x3D3D3D), dark: sw(0xCDCDCD, 0x3A3A3A, 0xE0E0E0) },
    Color { key: GREY, light: sw(0xB7B7B7, 0xF5F5F5, 0x616161), dark: sw(0x5C5C5C, 0x323232, 0xA6A6A6) },
];

/// Цвета календарей (S5-22): цвета таблицы DESIGN по порядку; свой календарь — синий. Больше календарей, чем цветов,
/// — цвет повторяется, различает имя владельца.
pub const CALENDAR: &[&str] = &[BLUE, GREEN, PINK, GRAPHITE, GREY];

/// Цвета календарей владельцев `owners` (ключи, `""` — мой): сохранённые (`saved`, ручные и назначенные раньше)
/// остаются; новым — наименее занятый цвет [`CALENDAR`] (при равенстве — по порядку). Устойчиво между запусками,
/// если назначенное сохраняют.
pub fn assign_calendar_colors(owners: &[String], saved: &std::collections::BTreeMap<String, String>) -> std::collections::BTreeMap<String, String> {
    let mut out: std::collections::BTreeMap<String, String> =
        saved.iter().filter(|(_, c)| CALENDAR.contains(&c.as_str())).map(|(k, c)| (k.clone(), c.clone())).collect();
    let mut keys: Vec<&String> = owners.iter().collect();
    keys.sort();
    keys.dedup();
    // Мой — первым: по умолчанию синий.
    keys.sort_by_key(|k| !k.is_empty());
    for k in keys {
        if out.contains_key(k) {
            continue;
        }
        let used = |c: &str| out.values().filter(|v| v.as_str() == c).count();
        let pick = CALENDAR.iter().min_by_key(|c| used(c)).copied().unwrap_or(BLUE);
        out.insert(k.clone(), pick.to_owned());
    }
    out
}

/// Прежние цвета (общие цвета Fluent 2, S5-02) → ближайший цвет таблицы: наименьшая ΔE2000 точки `primary` Fluent
/// (одна на обе темы) к точке светлой темы. `blue` и `pink` — те же имена в новой палитре.
pub const LEGACY: &[(&str, &str)] = &[
    ("red", PINK),
    ("darkRed", GRAPHITE),
    ("cranberry", PINK),
    ("pumpkin", GRAPHITE),
    ("peach", GREY),
    ("marigold", GREY),
    ("gold", GREEN),
    ("brass", GRAPHITE),
    ("brown", GRAPHITE),
    ("forest", GREEN),
    ("seafoam", GREEN),
    ("darkGreen", GREEN),
    ("lightTeal", GREY),
    ("teal", GRAPHITE),
    ("steel", GRAPHITE),
    ("royalBlue", GRAPHITE),
    ("cornflower", BLUE),
    ("navy", BLUE),
    ("lavender", BLUE),
    ("purple", GRAPHITE),
    ("grape", PINK),
    ("berry", PINK),
    ("lilac", PINK),
    ("magenta", PINK),
    ("plum", PINK),
    ("beige", GRAPHITE),
    ("mink", GRAPHITE),
    ("platinum", GRAPHITE),
    ("anchor", GRAPHITE),
];

/// Занятые цвета: в категориях их и близких к ним не бывает.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reserved {
    /// Красный — только безопасность.
    Threat,
    /// Янтарный — только сроки и подсветка совпадений поиска.
    Deadline,
}

impl Reserved {
    pub const ALL: [Reserved; 2] = [Reserved::Threat, Reserved::Deadline];
}

/// Угроза: подложка и текст — таблица DESIGN; точка и полоса — вне таблицы (прежний красный Fluent).
const THREAT: (Swatch, Swatch) = (sw(0xD13438, 0xFFE6E3, 0x7A3430), sw(0xD13438, 0x4C2B28, 0xF9BDB7));
/// Срок: текст «срок впереди», подложка «просрочено» — таблица DESIGN; точка — вне таблицы (прежний янтарный Fluent).
const DEADLINE: (Swatch, Swatch) = (sw(0xEAA300, 0xF8DEA8, 0x664700), sw(0xEAA300, 0x6C5005, 0xE3CB99));

/// Пары «цвет — занятый цвет» ближе [`MIN_DELTA_E`], принятые владельцем, и их нижняя граница ΔE (в обеих темах):
/// розовый бренда и красный безопасности (≈ 19 и 23,5; DESIGN «Цвет», строка «Исключение», PR #60).
pub const EXEMPT: &[(&str, Reserved, f64)] = &[(PINK, Reserved::Threat, 18.0)];

/// Имя цвета таблицы для сохранённого имени: своё, прежнее ([`LEGACY`]) или `None`.
fn canonical(key: &str) -> Option<&'static str> {
    PALETTE.iter().find(|c| c.key == key).map(|c| c.key).or_else(|| LEGACY.iter().find(|(old, _)| *old == key).map(|(_, new)| *new))
}

/// Цвет по сохранённому имени (своему или прежнему).
pub fn color(key: &str) -> Option<&'static Color> {
    let key = canonical(key)?;
    PALETTE.iter().find(|c| c.key == key)
}

fn neutral() -> &'static Color {
    // NEUTRAL — из PALETTE (тест `defaults_are_in_the_palette`); запасной — последний цвет.
    color(NEUTRAL).unwrap_or(&PALETTE[PALETTE.len() - 1])
}

/// Имя цвета для отрисовки: прежний — ближайший из таблицы, неизвестный (таблица из новой версии, правка руками) —
/// нейтральный.
pub fn resolve(key: &str) -> &'static str {
    canonical(key).unwrap_or(NEUTRAL)
}

/// Цвета роли в теме; неизвестный цвет — нейтральный.
pub fn swatch(key: &str, theme: Theme) -> Swatch {
    color(key).unwrap_or_else(neutral).swatch(theme)
}

/// Цвета занятого цвета (угроза, срок) в теме.
pub fn reserved(r: Reserved, theme: Theme) -> Swatch {
    let (light, dark) = match r {
        Reserved::Threat => THREAT,
        Reserved::Deadline => DEADLINE,
    };
    match theme {
        Theme::Light => light,
        Theme::Dark => dark,
    }
}

/// С чем цвет слишком близок.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Clash {
    /// С цветом другой категории.
    Color(&'static str),
    /// С занятым цветом (или это он сам).
    Reserved(Reserved),
    /// Такого цвета в палитре нет.
    Unknown,
}

/// Наименьшая ΔE точек двух цветов категорий по темам, если она меньше [`MIN_DELTA_E`].
fn too_close(a: &Color, b: &Color) -> Option<f64> {
    Theme::ALL.iter().map(|&t| delta_e(a.swatch(t).dot, b.swatch(t).dot)).filter(|&d| d < MIN_DELTA_E).min_by(f64::total_cmp)
}

/// Нижняя граница ΔE цвета `key` к занятому `r`.
fn reserved_min(key: &str, r: Reserved) -> f64 {
    EXEMPT.iter().find(|e| e.0 == key && e.1 == r).map_or(MIN_DELTA_E, |e| e.2)
}

/// Ближайший занятый цвет, если он ближе допустимого в какой-либо теме.
fn near_reserved(c: &Color) -> Option<(Reserved, f64)> {
    Reserved::ALL
        .iter()
        .flat_map(|&r| Theme::ALL.map(|t| (r, delta_e(c.swatch(t).dot, reserved(r, t).dot))))
        .filter(|&(r, d)| d < reserved_min(c.key, r))
        .min_by(|a, b| a.1.total_cmp(&b.1))
}

/// Цвета, которые можно дать категории: далёкие от красного и янтарного в обеих темах (порядок таблицы).
pub fn usable() -> &'static [&'static str] {
    static U: OnceLock<Vec<&'static str>> = OnceLock::new();
    U.get_or_init(|| PALETTE.iter().filter(|c| near_reserved(c).is_none()).map(|c| c.key).collect())
}

/// Можно ли дать категории цвет `key`, когда у других категорий таблицы цвета `others`: первое, к чему он ближе
/// допустимого в какой-либо теме, и разница; `None` — можно. Тот же цвет в `others` — разница 0.
pub fn clash<S: AsRef<str>>(key: &str, others: &[S]) -> Option<(Clash, f64)> {
    let Some(c) = color(key) else { return Some((Clash::Unknown, 0.0)) };
    if let Some((r, d)) = near_reserved(c) {
        return Some((Clash::Reserved(r), d));
    }
    others
        .iter()
        .filter_map(|o| color(o.as_ref()))
        .filter_map(|o| too_close(c, o).map(|d| (o, d)))
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(o, d)| (Clash::Color(o.key), d))
}

/// Весь список: любые два цвета и занятые цвета различимы. Ошибка — первый цвет, который ближе допустимого
/// к одному из предыдущих (или к занятому).
pub fn check<S: AsRef<str>>(keys: &[S]) -> Result<(), (String, Clash, f64)> {
    for (i, k) in keys.iter().enumerate() {
        if let Some((c, d)) = clash(k.as_ref(), &keys[..i]) {
            return Err((k.as_ref().to_owned(), c, d));
        }
    }
    Ok(())
}

// --- преобразования цвета ---

fn linear_to_srgb(c: f64) -> f64 {
    if c <= 0.003_130_8 { 12.92 * c } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

fn srgb_to_linear(c: f64) -> f64 {
    if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// OKLCH → линейный sRGB (Björn Ottosson, 2020).
fn oklch_to_linear(o: Oklch) -> [f64; 3] {
    let (a, b) = (o.c * o.h.to_radians().cos(), o.c * o.h.to_radians().sin());
    let l = (o.l + 0.396_337_777_4 * a + 0.215_803_757_3 * b).powi(3);
    let m = (o.l - 0.105_561_345_8 * a - 0.063_854_172_8 * b).powi(3);
    let s = (o.l - 0.089_484_177_5 * a - 1.291_485_548_0 * b).powi(3);
    [
        4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s,
        -1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s,
        -0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701_0 * s,
    ]
}

fn in_gamut(rgb: [f64; 3]) -> bool {
    rgb.iter().all(|c| (-1e-6..=1.0 + 1e-6).contains(c))
}

/// OKLCH → sRGB. Вне охвата sRGB — насыщенность уменьшается (L и оттенок те же), пока цвет не войдёт.
pub fn oklch_to_rgb(o: Oklch) -> Rgb {
    let mut rgb = oklch_to_linear(o);
    if !in_gamut(rgb) {
        let (mut lo, mut hi) = (0.0, o.c);
        for _ in 0..32 {
            let mid = (lo + hi) / 2.0;
            if in_gamut(oklch_to_linear(Oklch { c: mid, ..o })) { lo = mid } else { hi = mid }
        }
        rgb = oklch_to_linear(Oklch { c: lo, ..o });
    }
    let byte = |c: f64| (linear_to_srgb(c.clamp(0.0, 1.0)) * 255.0).round() as u32;
    (byte(rgb[0]) << 16) | (byte(rgb[1]) << 8) | byte(rgb[2])
}

/// sRGB → OKLCH (для проверок и настроек).
pub fn rgb_to_oklch(rgb: Rgb) -> Oklch {
    let [r, g, b] = linear(rgb);
    let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
    let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
    let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
    let ll = 0.210_454_255_3 * l + 0.793_617_785_0 * m - 0.004_072_046_8 * s;
    let a = 1.977_998_495_1 * l - 2.428_592_205_0 * m + 0.450_593_709_9 * s;
    let bb = 0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766_0 * s;
    Oklch { l: ll, c: a.hypot(bb), h: bb.atan2(a).to_degrees().rem_euclid(360.0) }
}

fn linear(rgb: Rgb) -> [f64; 3] {
    [16, 8, 0].map(|shift| srgb_to_linear(f64::from((rgb >> shift) & 0xff) / 255.0))
}

/// CIELAB (D65).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lab {
    pub l: f64,
    pub a: f64,
    pub b: f64,
}

pub fn rgb_to_lab(rgb: Rgb) -> Lab {
    let [r, g, b] = linear(rgb);
    let x = (0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b) / 0.950_47;
    let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175_0 * b;
    let z = (0.019_333_9 * r + 0.119_192_0 * g + 0.950_304_1 * b) / 1.088_83;
    let f = |t: f64| if t > 216.0 / 24389.0 { t.cbrt() } else { (24389.0 / 27.0 * t + 16.0) / 116.0 };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    Lab { l: 116.0 * fy - 16.0, a: 500.0 * (fx - fy), b: 200.0 * (fy - fz) }
}

/// Разница цветов CIEDE2000 (Sharma, Wu, Dalal, 2005).
pub fn delta_e2000(p: Lab, q: Lab) -> f64 {
    let c_bar = (p.a.hypot(p.b) + q.a.hypot(q.b)) / 2.0;
    let pow7 = |x: f64| x.powi(7);
    let g = 0.5 * (1.0 - (pow7(c_bar) / (pow7(c_bar) + pow7(25.0))).sqrt());
    let (a1, a2) = ((1.0 + g) * p.a, (1.0 + g) * q.a);
    let (c1, c2) = (a1.hypot(p.b), a2.hypot(q.b));
    let angle = |b: f64, a: f64| if a == 0.0 && b == 0.0 { 0.0 } else { b.atan2(a).to_degrees().rem_euclid(360.0) };
    let (h1, h2) = (angle(p.b, a1), angle(q.b, a2));
    let dl = q.l - p.l;
    let dc = c2 - c1;
    let dh = if c1 * c2 == 0.0 {
        0.0
    } else if (h2 - h1).abs() <= 180.0 {
        h2 - h1
    } else if h2 <= h1 {
        h2 - h1 + 360.0
    } else {
        h2 - h1 - 360.0
    };
    let dh_big = 2.0 * (c1 * c2).sqrt() * (dh / 2.0).to_radians().sin();
    let l_bar = (p.l + q.l) / 2.0;
    let c_bar2 = (c1 + c2) / 2.0;
    let h_bar = if c1 * c2 == 0.0 {
        h1 + h2
    } else if (h1 - h2).abs() <= 180.0 {
        (h1 + h2) / 2.0
    } else if h1 + h2 < 360.0 {
        (h1 + h2 + 360.0) / 2.0
    } else {
        (h1 + h2 - 360.0) / 2.0
    };
    let cos = |deg: f64| deg.to_radians().cos();
    let t = 1.0 - 0.17 * cos(h_bar - 30.0) + 0.24 * cos(2.0 * h_bar) + 0.32 * cos(3.0 * h_bar + 6.0) - 0.20 * cos(4.0 * h_bar - 63.0);
    let d_theta = 30.0 * (-((h_bar - 275.0) / 25.0).powi(2)).exp();
    let rc = 2.0 * (pow7(c_bar2) / (pow7(c_bar2) + pow7(25.0))).sqrt();
    let sl = 1.0 + 0.015 * (l_bar - 50.0).powi(2) / (20.0 + (l_bar - 50.0).powi(2)).sqrt();
    let sc = 1.0 + 0.045 * c_bar2;
    let sh = 1.0 + 0.015 * c_bar2 * t;
    let rt = -(2.0 * d_theta).to_radians().sin() * rc;
    ((dl / sl).powi(2) + (dc / sc).powi(2) + (dh_big / sh).powi(2) + rt * (dc / sc) * (dh_big / sh)).sqrt()
}

/// ΔE2000 между двумя цветами sRGB.
pub fn delta_e(a: Rgb, b: Rgb) -> f64 {
    delta_e2000(rgb_to_lab(a), rgb_to_lab(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Контрольные пары из статьи Sharma, Wu, Dalal (таблица 1).
    #[test]
    fn ciede2000_matches_the_reference_data() {
        let lab = |l, a, b| Lab { l, a, b };
        let cases = [
            (lab(50.0, 2.6772, -79.7751), lab(50.0, 0.0, -82.7485), 2.0425),
            (lab(50.0, -1.3802, -84.2814), lab(50.0, 0.0, -82.7485), 1.0000),
            (lab(50.0, 2.5, 0.0), lab(73.0, 25.0, -18.0), 27.1492),
            (lab(50.0, 2.5, 0.0), lab(50.0, 0.0, -2.5), 4.3065),
            (lab(60.2574, -34.0099, 36.2677), lab(60.4626, -34.1751, 39.4387), 1.2644),
            (lab(22.7233, 20.0904, -46.6940), lab(23.0331, 14.9730, -42.5619), 2.0373),
            (lab(2.0776, 0.0795, -1.1350), lab(0.9033, -0.0636, -0.5514), 0.9082),
        ];
        for (p, q, want) in cases {
            let got = delta_e2000(p, q);
            assert!((got - want).abs() < 1e-4, "{p:?} {q:?}: {got} != {want}");
            assert!((delta_e2000(q, p) - want).abs() < 1e-4, "симметрия");
        }
    }

    #[test]
    fn oklch_round_trips_through_srgb() {
        for rgb in [0x0078D4, 0x0B6A0B, 0x881798, 0x00B7C3, 0x5D5A58, 0x000000, 0xFFFFFF] {
            assert_eq!(oklch_to_rgb(rgb_to_oklch(rgb)), rgb, "{rgb:06X}");
        }
        // Белый и чёрный — L 1 и 0 без насыщенности.
        let w = rgb_to_oklch(0xFFFFFF);
        assert!((w.l - 1.0).abs() < 1e-3 && w.c < 1e-3);
    }

    /// Цвета календарей: мой — синий, новые — наименее занятые по порядку, 6-й повторяет; ручной и назначенный раньше
    /// сохраняются; порядок прихода коллег не меняет назначение.
    #[test]
    fn calendar_colors_are_stable() {
        use std::collections::BTreeMap;
        let owners: Vec<String> = ["", "a@x", "b@x", "c@x", "d@x", "e@x"].map(String::from).to_vec();
        let got = assign_calendar_colors(&owners, &BTreeMap::new());
        assert_eq!(got[""], BLUE);
        assert_eq!([&got["a@x"], &got["b@x"], &got["c@x"], &got["d@x"]], [GREEN, PINK, GRAPHITE, GREY]);
        assert_eq!(got["e@x"], BLUE, "цветов пять — шестой повторяет");
        let mut reversed = owners.clone();
        reversed.reverse();
        assert_eq!(assign_calendar_colors(&reversed, &BTreeMap::new()), got, "порядок прихода не важен");
        // Сохранённое остаётся, новый коллега — свободный цвет; ручной цвет — как выбран.
        let saved = BTreeMap::from([("".to_string(), PINK.to_string()), ("b@x".to_string(), PINK.to_string())]);
        let next = assign_calendar_colors(&["".into(), "b@x".into(), "z@x".into()], &saved);
        assert_eq!((next[""].as_str(), next["b@x"].as_str(), next["z@x"].as_str()), (PINK, PINK, BLUE));
        // Неизвестный сохранённый цвет — назначается заново.
        let odd = BTreeMap::from([("a@x".to_string(), "teal".to_string())]);
        assert!(CALENDAR.contains(&assign_calendar_colors(&["a@x".into()], &odd)["a@x"].as_str()));
    }

    /// Пять цветов таблицы DESIGN, имена без повторов; прежние имена Fluent — к ближайшему цвету таблицы.
    #[test]
    fn palette_is_the_design_table() {
        let keys: Vec<&str> = PALETTE.iter().map(|c| c.key).collect();
        assert_eq!(keys, [BLUE, GREEN, PINK, GRAPHITE, GREY]);
        let blue = swatch(BLUE, Theme::Light);
        assert_eq!(blue, Swatch { dot: 0x4687D8, text: 0x234E82, bg: 0xDFF0FF });
        let dark = swatch(BLUE, Theme::Dark);
        assert_eq!(dark, Swatch { dot: 0x689BDB, text: 0xBFDDFF, bg: 0x23364F });
        // Неизвестный цвет — нейтральный; прежний — ближайший из таблицы; бирюзы нет.
        assert_eq!(swatch("chartreuse", Theme::Dark), swatch(NEUTRAL, Theme::Dark));
        assert_eq!(resolve("chartreuse"), NEUTRAL);
        assert_eq!(resolve("teal"), GRAPHITE);
        assert_eq!(resolve("lightTeal"), GREY);
        assert_eq!(resolve("darkGreen"), GREEN);
        assert_eq!(resolve("grape"), PINK);
        assert_eq!(resolve("blue"), BLUE);
        assert!(PALETTE.iter().all(|c| !c.key.contains("teal")));
        assert_eq!(reserved(Reserved::Threat, Theme::Light).bg, 0xFFE6E3);
        assert_eq!(reserved(Reserved::Deadline, Theme::Dark).text, 0xE3CB99);
    }

    /// Прежние имена: каждое — ближайший по ΔE2000 цвет таблицы к точке Fluent `primary` (светлая тема); имена
    /// не совпадают с новыми.
    #[test]
    fn legacy_colors_map_to_the_nearest() {
        // `primary` общих цветов Fluent 2 (`@fluentui/tokens` 9.x) — только для проверки соответствия.
        const FLUENT: &[(&str, Rgb)] = &[
            ("red", 0xD13438),
            ("darkRed", 0x750B1C),
            ("cranberry", 0xC50F1F),
            ("pumpkin", 0xCA5010),
            ("peach", 0xFF8C00),
            ("marigold", 0xEAA300),
            ("gold", 0xC19C00),
            ("brass", 0x986F0B),
            ("brown", 0x8E562E),
            ("forest", 0x498205),
            ("seafoam", 0x00CC6A),
            ("darkGreen", 0x0B6A0B),
            ("lightTeal", 0x00B7C3),
            ("teal", 0x038387),
            ("steel", 0x005B70),
            ("royalBlue", 0x004E8C),
            ("cornflower", 0x4F6BED),
            ("navy", 0x0027B4),
            ("lavender", 0x7160E8),
            ("purple", 0x5C2E91),
            ("grape", 0x881798),
            ("berry", 0xC239B3),
            ("lilac", 0xB146C2),
            ("magenta", 0xBF0077),
            ("plum", 0x77004D),
            ("beige", 0x7A7574),
            ("mink", 0x5D5A58),
            ("platinum", 0x69797E),
            ("anchor", 0x394146),
        ];
        assert_eq!(LEGACY.len(), FLUENT.len());
        for (&(old, new), &(name, primary)) in LEGACY.iter().zip(FLUENT) {
            assert_eq!(old, name);
            assert!(PALETTE.iter().all(|c| c.key != old), "{old}: имя занято новой палитрой");
            let nearest = PALETTE.iter().min_by(|a, b| delta_e(primary, a.light.dot).total_cmp(&delta_e(primary, b.light.dot))).map(|c| c.key);
            assert_eq!(Some(new), nearest, "{old}");
        }
    }

    #[test]
    fn defaults_are_in_the_palette() {
        for key in [BLUE, GREEN, PINK, GRAPHITE, NEUTRAL] {
            assert!(color(key).is_some() && usable().contains(&key), "{key}");
        }
    }

    /// Выбор цвета категории: все пять цветов таблицы далеко от красного и янтарного в обеих темах; исключение —
    /// розовый бренда и красный безопасности ([`EXEMPT`], не ближе 18).
    #[test]
    fn usable_colors_are_far_from_reserved() {
        let usable = usable();
        assert_eq!(usable, [BLUE, GREEN, PINK, GRAPHITE, GREY]);
        for key in usable {
            for r in Reserved::ALL {
                for theme in Theme::ALL {
                    let d = delta_e(swatch(key, theme).dot, reserved(r, theme).dot);
                    let min = if (*key, r) == (PINK, Reserved::Threat) { 18.0 } else { MIN_DELTA_E };
                    assert!(d >= min, "{key} — {r:?} {theme:?}: ΔE {d:.1}");
                }
            }
        }
        // Розовый к красному ближе 25 — исключение действует, без него было бы отказано.
        let pink = delta_e(swatch(PINK, Theme::Light).dot, reserved(Reserved::Threat, Theme::Light).dot);
        assert!(pink < MIN_DELTA_E, "исключение больше не нужно: ΔE {pink:.1}");
        assert_eq!(clash("chartreuse", &[] as &[&str]), Some((Clash::Unknown, 0.0)));
    }

    /// Категории по умолчанию (DESIGN: синий, зелёный, розовый бренда, графит, серый) различимы между собой и с
    /// красным и янтарным в обеих темах (ΔE2000 ≥ 25, S5-01); исключение — только [`EXEMPT`].
    #[test]
    fn default_categories_are_far_apart() {
        let defaults = [BLUE, GREEN, PINK, GRAPHITE, NEUTRAL];
        assert_eq!(check(&defaults), Ok(()));
        for theme in Theme::ALL {
            let mut dots: Vec<(&str, Rgb)> = defaults.iter().map(|&s| (s, swatch(s, theme).dot)).collect();
            dots.push(("threat", reserved(Reserved::Threat, theme).dot));
            dots.push(("deadline", reserved(Reserved::Deadline, theme).dot));
            for (i, &(a, x)) in dots.iter().enumerate() {
                for &(b, y) in &dots[i + 1..] {
                    let d = delta_e(x, y);
                    let min = if (a, b) == (PINK, "threat") { 18.0 } else { MIN_DELTA_E };
                    assert!(d >= min, "{theme:?}: {a} {b} ΔE {d:.1}");
                }
            }
        }
    }

    /// Запрет близких цветов: повтор цвета — отказ; в таблице пять цветов, к пяти категориям по умолчанию шестую
    /// не добавить (вопрос владельцу, S5-31).
    #[test]
    fn close_colors_are_rejected() {
        let used = [BLUE, GREEN, PINK, GRAPHITE, NEUTRAL];
        assert!(matches!(clash(BLUE, &[BLUE]), Some((Clash::Color(BLUE), d)) if d == 0.0));
        // Прежнее имя — тот же цвет, что новое.
        assert!(matches!(clash("cornflower", &[BLUE]), Some((Clash::Color(BLUE), _))));
        assert!(check(&[BLUE, BLUE]).is_err());
        assert_eq!(clash(GREEN, &[BLUE]), None);
        assert!(usable().iter().all(|k| clash(k, &used).is_some()), "шестой цвет нашёлся — обновить тест");
    }

    /// Текст метки читается на её подложке (контраст WCAG ≥ 4,5) в обеих темах — у всех цветов палитры.
    #[test]
    fn chip_text_is_readable() {
        let lum = |rgb: Rgb| {
            let [r, g, b] = linear(rgb);
            0.2126 * r + 0.7152 * g + 0.0722 * b
        };
        for c in PALETTE {
            for theme in Theme::ALL {
                let s = c.swatch(theme);
                let (a, b) = (lum(s.text), lum(s.bg));
                let ratio = (a.max(b) + 0.05) / (a.min(b) + 0.05);
                assert!(ratio >= 4.5, "{} {theme:?}: {ratio:.2}", c.key);
            }
        }
    }
}
