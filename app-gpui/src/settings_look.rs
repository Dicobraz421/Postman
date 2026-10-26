//! «Настройки → Внешний вид» (S5-30, макет `SettingsAppearance`): тема, акцент, плотность, размер шрифта, аватары и
//! превью, анимации, KeyTips (выключатель и задержка). Применяется сразу ко всем окнам.
//!
//! Акцент — синий Postman или бирюзовый, зелёный, фиолетовый, графит (токены DESIGN, значения макета; решение
//! владельца 2026-10-22). Плотность — две, как в макете; шрифт по умолчанию — 14. Анимации — запоминаются (плавных
//! переходов пока нет).

use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::{Metric, Role, Themes};
use postman_core::prefs::{Accent, FONT_SIZES, KEYTIPS_DELAYS};

use crate::chrome::icon;
use crate::roles::Look;
use crate::settings_general::stored_row;
use crate::settings_tab::{SettingsView, group, row, seg, seg_group, toggle};

impl SettingsView {
    pub(crate) fn look(&mut self, l: &Look, cx: &mut Context<Self>) -> Div {
        let p = crate::prefs::get(cx);
        // Тема и плотность — как были в «Общих» (S5-09).
        let (choice, locked, ids, titles) = match Themes::try_get(cx) {
            Some(t) => {
                let s = t.state();
                let ids: Vec<String> = s.ids().map(str::to_owned).collect();
                let titles = ids.iter().map(|id| pets_ui::theme::title(id, s).to_string()).collect::<Vec<_>>();
                (s.effective().to_owned(), s.locked(), ids, titles)
            }
            None => (pets_ui::theme::SYSTEM.to_owned(), false, vec![pets_ui::theme::SYSTEM.to_owned()], vec![String::new()]),
        };
        let themes: Vec<Stateful<Div>> = ids
            .iter()
            .zip(titles)
            .enumerate()
            .map(|(i, (id, title))| {
                let id = id.clone();
                seg(("settings-theme", i), title, choice == id, !locked, l).when(!locked, |d| {
                    d.on_click(cx.listener(move |this, _, _, cx| {
                        this.theme_error = crate::prefs::set_theme(&id, cx).err().map(|e| e.to_string());
                        cx.notify();
                    }))
                })
            })
            .collect();
        let theme_hint = if locked { Some(tr!("pets-ui-theme-locked")) } else { self.theme_error.clone().or(Some(tr!("settings-theme-hint"))) };
        let row_px = l.density().row();
        // Две плотности, как в макете: прежние «удобная» и «просторная» — обычная (`Prefs::normalized`).
        let densities: Vec<Stateful<Div>> = [(20u16, "settings-density-compact"), (24, "settings-density-normal")]
            .into_iter()
            .map(|(px_row, key)| {
                seg(("settings-density", px_row as usize), tr!(key, px = px_row), row_px == px_row, true, l).on_click(cx.listener(
                    move |_, _, _, cx| {
                        crate::prefs::set_density(px_row, cx);
                        cx.notify();
                    },
                ))
            })
            .collect();
        // Размер шрифта: − 13 px +.
        let size = p.look.font_px();
        let step = |id: &'static str, g: char, to: u16, enabled: bool| {
            div()
                .id(id)
                .size(px(28.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(l.px(Metric::RadiusControl))
                .border_1()
                .border_color(l.stroke())
                .when(enabled, |d| d.cursor_pointer().hover(|d| d.bg(l.hover())))
                .when(!enabled, |d| d.text_color(l.c(Role::TextDisabled)))
                .child(icon(g, 10.))
                .when(enabled, |d| {
                    d.on_click(cx.listener(move |_, _, _, cx| {
                        crate::prefs::update(cx, |p| p.look.font_size = Some(to));
                        cx.notify();
                    }))
                })
        };
        let font = div()
            .flex()
            .items_center()
            .gap_2()
            .child(step("settings-font-dec", '\u{E738}', size.saturating_sub(1), size > *FONT_SIZES.start()))
            .child(div().w(px(48.)).text_center().child(tr!("settings-font-px", px = size)))
            .child(step("settings-font-inc", '\u{E710}', size + 1, size < *FONT_SIZES.end()));
        let compact = l.compact();
        let flag = |id: &'static str, on: bool, set: fn(&mut postman_core::prefs::Look)| {
            toggle(id, on && !compact, l).when(!compact, |d| {
                d.on_click(cx.listener(move |_, _, _, cx| {
                    crate::prefs::update(cx, |p| set(&mut p.look));
                    cx.notify();
                }))
            })
        };
        let delays =
            KEYTIPS_DELAYS.map(|ms| {
                seg(("settings-keytips-delay", ms as usize), tr!("settings-keytips-delay-value", ms = ms), p.look.keytips_delay == ms, p.keytips, l)
                    .when(p.keytips, |d| {
                        d.on_click(cx.listener(move |_, _, _, cx| {
                            crate::prefs::update(cx, |p| p.look.keytips_delay = ms);
                            cx.notify();
                        }))
                    })
            });
        // Акцент: точки цвета с подписью; выбранный — в кольце акцента.
        let accents = div().flex().items_center().gap_3().children(Accent::ALL.map(|a| {
            let on = p.look.accent == a;
            let dot = crate::roles::accent_swatch(a, l.dark());
            div()
                .id(("settings-accent", a as usize))
                .flex()
                .flex_col()
                .items_center()
                .gap_1()
                .cursor_pointer()
                .child(div().size(px(24.)).rounded_full().bg(dot).when(on, |d| d.border_2().border_color(l.panel()).shadow_sm()))
                .child(
                    div()
                        .text_size(px(11.))
                        .when(on, |d| d.font_weight(FontWeight::SEMIBOLD).text_color(l.text()))
                        .when(!on, |d| d.text_color(l.text2()))
                        .child(tr!(&format!("settings-accent-{}", a.id()))),
                )
                .on_click(cx.listener(move |_, _, _, cx| {
                    crate::prefs::set_accent(a, cx);
                    cx.notify();
                }))
        }));
        div()
            .flex()
            .flex_col()
            .child(group(tr!("settings-look"), l))
            .child(row(tr!("settings-theme"), theme_hint, seg_group(themes, l), l))
            .child(row(tr!("settings-accent"), Some(tr!("settings-accent-hint")), accents, l))
            .child(row(tr!("settings-density"), Some(tr!("settings-density-hint")), seg_group(densities, l), l))
            .child(group(tr!("settings-text-group"), l))
            .child(row(tr!("settings-font"), Some(tr!("settings-font-hint")), font, l))
            .child(group(tr!("settings-layout-group"), l))
            .child(row(
                tr!("settings-avatars"),
                Some(tr!(if compact { "settings-compact-off" } else { "settings-avatars-hint" })),
                flag("settings-avatars", p.look.avatars, |l| l.avatars = !l.avatars),
                l,
            ))
            .child(row(
                tr!("settings-preview"),
                Some(tr!(if compact { "settings-compact-off" } else { "settings-preview-hint" })),
                flag("settings-preview", p.look.preview, |l| l.preview = !l.preview),
                l,
            ))
            .child(stored_row(
                tr!("settings-animations"),
                Some(tr!("settings-animations-hint")),
                toggle("settings-animations", p.look.animations, l).on_click(cx.listener(|_, _, _, cx| {
                    crate::prefs::update(cx, |p| p.look.animations = !p.look.animations);
                    cx.notify();
                })),
                l,
            ))
            .child(group(tr!("settings-keytips-group"), l))
            .child(row(
                tr!("settings-keytips"),
                Some(tr!("settings-keytips-hint")),
                toggle("settings-keytips", p.keytips, l).on_click(cx.listener(|_, _, _, cx| {
                    crate::prefs::update(cx, |p| p.keytips = !p.keytips);
                    cx.notify();
                })),
                l,
            ))
            .child(row(tr!("settings-keytips-delay"), Some(tr!("settings-keytips-delay-hint")), seg_group(delays, l), l))
    }
}
