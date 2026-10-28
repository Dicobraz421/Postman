//! Не CSS-парсер: из `style=""` берутся только начертание, выравнивание и размер картинки.

use crate::{Align, Marks};

pub(crate) fn apply(style: &str, marks: &mut Marks, align: &mut Align) {
    for decl in style.split(';') {
        let Some((name, value)) = decl.split_once(':') else { continue };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim().trim_end_matches("!important").trim().to_ascii_lowercase();
        match name.as_str() {
            "font-weight" => {
                marks.bold = match value.as_str() {
                    "bold" | "bolder" => true,
                    "normal" | "lighter" => false,
                    v => v.parse::<u16>().map(|w| w >= 600).unwrap_or(marks.bold),
                }
            }
            "font-style" => match value.as_str() {
                "italic" | "oblique" => marks.italic = true,
                "normal" => marks.italic = false,
                _ => {}
            },
            "text-decoration" | "text-decoration-line" => {
                if value.contains("none") {
                    marks.underline = false;
                    marks.strike = false;
                }
                if value.contains("underline") {
                    marks.underline = true;
                }
                if value.contains("line-through") {
                    marks.strike = true;
                }
            }
            "text-align" => {
                if let Some(a) = parse_align(&value) {
                    *align = a;
                }
            }
            _ => {}
        }
    }
}

pub(crate) fn parse_align(v: &str) -> Option<Align> {
    Some(match v.trim().to_ascii_lowercase().as_str() {
        "left" | "start" => Align::Start,
        "center" | "middle" => Align::Center,
        "right" | "end" => Align::End,
        "justify" => Align::Justify,
        _ => return None,
    })
}

/// Наибольшая сторона картинки, CSS px: задуманный размер больше — не размер (T1-10б).
pub const MAX_IMAGE_SIDE: u32 = 10_000;

/// `width`/`height` картинки из `style=""`; чего нет или что не длина — `None`.
pub(crate) fn image_size(style: &str) -> (Option<u32>, Option<u32>) {
    let (mut w, mut h) = (None, None);
    for decl in style.split(';') {
        let Some((name, value)) = decl.split_once(':') else { continue };
        let value = value.trim().trim_end_matches("!important");
        match name.trim().to_ascii_lowercase().as_str() {
            "width" => w = css_px(value),
            "height" => h = css_px(value),
            _ => {}
        }
    }
    (w, h)
}

/// Атрибут `width`/`height`: число пикселей с необязательным хвостом (`120px`), как разбирает браузер; проценты —
/// не размер в px.
pub(crate) fn attr_px(v: &str) -> Option<u32> {
    let (n, rest) = number(v.trim());
    if rest.trim_start().starts_with('%') {
        return None;
    }
    px(n?)
}

/// Длина CSS в px: px, pt, pc, in, cm, mm (Outlook пишет размеры подписи в дюймах), число без единицы — px, как в
/// режиме совместимости. `auto`, `%`, `em` и прочее — `None`.
fn css_px(v: &str) -> Option<u32> {
    let (n, unit) = number(v.trim());
    let k = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "px" => 1.,
        "pt" => 96. / 72.,
        "pc" => 16.,
        "in" => 96.,
        "cm" => 96. / 2.54,
        "mm" => 96. / 25.4,
        _ => return None,
    };
    px(n? * k)
}

/// Неотрицательное число в начале строки (цифры и точка) и остаток.
fn number(v: &str) -> (Option<f32>, &str) {
    let end = v.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(v.len());
    (v[..end].parse().ok(), &v[end..])
}

/// Округлённые px в 1..=[`MAX_IMAGE_SIDE`].
fn px(v: f32) -> Option<u32> {
    let r = v.round();
    (r.is_finite() && (1. ..=MAX_IMAGE_SIDE as f32).contains(&r)).then_some(r as u32)
}
