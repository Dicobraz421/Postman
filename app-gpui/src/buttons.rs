//! Кнопки (S5-19, DESIGN «Цвет»): один компонент на всё приложение. Цвет кнопки — её роль, а не смысл: главная —
//! заливка акцентом, остальные — тонированные (синяя подложка таблицы токенов, без рамки). Цвета статусов,
//! категорий и сроков — только на метках; кнопка, которая ставит статус, несёт значок цветом своего текста.
//!
//! Роли: заливка — `accent.*` pets-ui; тонированная — `app.button.secondary` (подложка, текст, наведение;
//! [`Pm::Secondary`]). Нажатие — то же наведение: своего токена у таблицы нет. Высокая контрастность — системные цвета
//! (выделение и текст на нём).

use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::{Metric, Role};

use crate::roles::{Look, Pm};

/// Высота кнопки, px.
const H: f32 = 32.;

fn shell(id: impl Into<ElementId>, l: &Look) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .h(px(H))
        .px(px(12.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(l.px(Metric::RadiusControl))
        .cursor_pointer()
}

/// Главная кнопка: заливка акцентом.
pub(crate) fn primary(id: impl Into<ElementId>, l: &Look) -> Stateful<Div> {
    let (fill, hover) = (l.accent(), l.accent_hover());
    shell(id, l).bg(fill).text_color(l.on_accent()).hover(move |d| d.bg(hover)).active(move |d| d.bg(hover))
}

/// Второстепенная кнопка: тонированная, без рамки.
pub(crate) fn secondary(id: impl Into<ElementId>, l: &Look) -> Stateful<Div> {
    let hover = l.pm(Pm::SecondaryHover);
    shell(id, l).bg(l.pm(Pm::Secondary)).text_color(l.pm(Pm::SecondaryFg)).hover(move |d| d.bg(hover)).active(move |d| d.bg(hover))
}

/// Кнопка с подписью: главная или второстепенная.
pub(crate) fn button(id: impl Into<ElementId>, label: String, primary: bool, l: &Look) -> Stateful<Div> {
    if primary { self::primary(id, l) } else { secondary(id, l) }.child(label)
}

/// То же без `Look` под рукой (построители без `cx`).
pub(crate) fn plain(id: impl Into<ElementId>, label: String, primary: bool) -> Stateful<Div> {
    button(id, label, primary, &crate::roles::now())
}

/// Цвет текста кнопки — для значка статуса на ней (одноцветный, как текст).
pub(crate) fn ink(primary: bool, l: &Look) -> Hsla {
    if primary { l.on_accent() } else { l.pm(Pm::SecondaryFg) }
}

/// Образцы для снимка `buttons`: главная и тонированная на каждой поверхности, где лежат кнопки.
pub(crate) fn gallery(l: &Look) -> Div {
    let row = |name: &'static str, under: Hsla| {
        div()
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(16.))
            .py(px(10.))
            .rounded(l.px(Metric::RadiusOverlay))
            .bg(under)
            .child(div().w(px(160.)).text_color(l.text2()).child(name))
            .child(button(("gallery-primary", name.len()), tr!("cal-accept"), true, l))
            .child(button(("gallery-secondary", name.len()), tr!("cal-tentative-answer"), false, l))
            .child(secondary(("gallery-hover", name.len()), l).bg(l.pm(Pm::SecondaryHover)).child(tr!("cal-decline")))
    };
    div()
        .absolute()
        .inset_0()
        .p(px(24.))
        .flex()
        .flex_col()
        .gap(px(8.))
        .bg(l.base())
        .text_color(l.text())
        .children(surfaces(l).into_iter().map(|(name, under)| row(name, under)))
}

/// Поверхности, на которых лежат кнопки: фон окна, карточка, всплывающее, форма правки (настройки), выделенная
/// строка, строка вопроса (письмо), полоса приглашения.
fn surfaces(l: &Look) -> Vec<(&'static str, Hsla)> {
    let panel = l.panel();
    let over = |c: Hsla| over_panel(c, panel);
    vec![
        ("bg.base", l.base()),
        ("layer.panel", panel),
        ("layer.overlay", l.c(Role::LayerOverlay)),
        ("fill.cursor.passive", over(l.fill_passive())),
        ("fill.cursor", over(l.selected())),
        ("caution.bg", over(l.warning_bg())),
    ]
}

/// Полупрозрачная поверхность на карточке — как её видно.
fn over_panel(c: Hsla, panel: Hsla) -> Hsla {
    pets_ui::theme::over(c, panel)
}

/// Опись части (ADR 0005, `parts.rs`).
pub(crate) const PART: crate::part::Part = crate::part::Part { name: "buttons", scenes: scenes::SCENES, reset: Some(scenes::reset), ..crate::part::Part::NONE };

/// Сцены `--screens` кнопок (ADR 0005).
pub(crate) mod scenes {
    use gpui_kit::*;

    use crate::screens::Scene;
    use crate::{Mailer, Section};

    pub(crate) const SCENES: &[Scene] = &[
        // Кнопки (S5-19): главная, тонированная и тонированная под указателем на каждой поверхности (`buttons::gallery`).
        Scene::new("buttons", buttons, buttons_ready),
    ];

    /// Образцы кнопок — только в своей сцене (S5-19).
    pub(crate) fn reset(s: &Scene, m: &mut Mailer, _: &mut Window, _: &mut Context<Mailer>) {
        if let Some(d) = &mut m.screens {
            d.gallery = s.name == "buttons";
        }
    }

    fn buttons(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.set_section(Section::Mail, cx);
        Ok(())
    }

    fn buttons_ready(_: &Scene, m: &Mailer, _: &App) -> bool {
        m.screens.as_ref().is_some_and(|d| d.gallery)
    }
}

#[cfg(test)]
mod tests {
    use super::{over_panel, surfaces};
    use crate::roles::{Look, Pm, ROLES, base};
    use pets_ui::theme::{Color, FontSizes, HcColors, OsLook, ThemeSpec, contrast, resolve};

    fn theme(os: &OsLook) -> Look {
        Look::new(&resolve(&ThemeSpec::default(), Some(&base(None, postman_core::prefs::Accent::System)), os, FontSizes::default(), &ROLES))
    }

    fn hex(c: gpui_kit::Hsla) -> String {
        Color::from_hsla(c).hex()
    }

    /// Тонированная кнопка — токены таблицы DESIGN; в обеих темах её подложка (и наведение) отличается от любой
    /// поверхности, на которой она лежит; текст на подложке и на наведении, текст главной — контраст ≥ 4,5:1.
    #[test]
    fn buttons_stand_out_and_read_well() {
        let light = theme(&OsLook::default());
        let dark = theme(&OsLook { dark: true, ..OsLook::default() });
        assert_eq!([hex(light.pm(Pm::Secondary)), hex(light.pm(Pm::SecondaryFg)), hex(light.pm(Pm::SecondaryHover))], ["#DFF0FF", "#234E82", "#D0E6FA"]);
        assert_eq!([hex(dark.pm(Pm::Secondary)), hex(dark.pm(Pm::SecondaryFg)), hex(dark.pm(Pm::SecondaryHover))], ["#23364F", "#BFDDFF", "#2C4466"]);
        for l in [&light, &dark] {
            let panel = l.panel();
            for fill in [l.pm(Pm::Secondary), l.pm(Pm::SecondaryHover), l.accent()] {
                for (name, under) in surfaces(l) {
                    assert_ne!(hex(over_panel(fill, panel)), hex(under), "dark={}: кнопка цвета поверхности {name}", l.dark());
                }
            }
            for under in [l.pm(Pm::Secondary), l.pm(Pm::SecondaryHover)] {
                let c = contrast(l.pm(Pm::SecondaryFg), under);
                assert!(c >= 4.5, "dark={}: текст тонированной {c:.2}", l.dark());
            }
            let c = contrast(l.on_accent(), l.accent());
            assert!(c >= 4.5, "dark={}: текст главной {c:.2}", l.dark());
        }
    }

    /// Высокая контрастность: тонированная — системное выделение и текст на нём, наведение — тоже выделение.
    #[test]
    fn high_contrast_uses_system_colors() {
        let hc = HcColors::default();
        let l = theme(&OsLook { high_contrast: true, hc: Some(hc), ..OsLook::default() });
        assert_eq!(hex(l.pm(Pm::Secondary)), hc.highlight.hex());
        assert_eq!(hex(l.pm(Pm::SecondaryFg)), hc.highlight_text.hex());
        assert_eq!(hex(l.pm(Pm::SecondaryHover)), hc.highlight.hex());
    }
}
